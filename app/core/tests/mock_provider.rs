//! Exercises the provider interface and region picker through the mock.

use std::collections::HashMap;

use rega_core::aws;
use rega_core::mock::{MockProvider, MockRegion};
use rega_core::region;
use rega_core::{CloudProvider, LaunchRequest, MachineState, Price, ProviderError};

fn request(type_name: &str, spot: bool) -> LaunchRequest {
    LaunchRequest {
        name: "test".into(),
        region_id: "eu-north-1".into(),
        type_name: type_name.into(),
        disk_gib: 128,
        spot,
        wireguard: true,
    }
}

#[tokio::test]
async fn cpu_then_gpu_lifecycle() {
    let p = MockProvider::demo_aws();
    let m = p.launch(&request("t3.medium", false)).await.unwrap();
    assert_eq!(m.state, MachineState::Running);

    let err = p.resize(&m.id, "g4dn.xlarge").await.unwrap_err();
    assert!(matches!(err, ProviderError::InvalidState(_)));

    p.stop(&m.id).await.unwrap();
    let m = p.resize(&m.id, "g4dn.xlarge").await.unwrap();
    assert!(m.machine_type.is_gpu());
    p.start(&m.id).await.unwrap();

    p.destroy(&m.id).await.unwrap();
    assert!(p.list_machines().await.unwrap().is_empty());
}

#[tokio::test]
async fn spot_is_gpu_only_and_never_resized() {
    let p = MockProvider::demo_aws();
    assert_eq!(
        p.launch(&request("t3.medium", true)).await,
        Err(ProviderError::SpotNeedsGpu)
    );
    let m = p.launch(&request("g4dn.xlarge", true)).await.unwrap();
    p.stop(&m.id).await.unwrap();
    assert_eq!(
        p.resize(&m.id, "t3.medium").await,
        Err(ProviderError::SpotResize)
    );
}

#[tokio::test]
async fn regions_without_gpu_quota_are_not_offered() {
    let p = MockProvider::demo_aws();
    let c = region::candidates(&p, "g4dn.xlarge", false, |id| p.simulated_rtt(id))
        .await
        .unwrap();
    assert!(!c.is_empty());
    assert!(c.iter().all(|c| c.region.id != "eu-south-1"));
}

#[tokio::test]
async fn spot_uses_the_spot_quota() {
    let p = MockProvider::demo_aws();
    let ids = |c: Vec<region::RegionCandidate>| -> Vec<String> {
        c.into_iter().map(|c| c.region.id).collect()
    };
    let on_demand = region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    let spot = region::candidates(&p, "g4dn.xlarge", true, |_| None)
        .await
        .unwrap();
    // Ireland has on-demand GPU quota but no spot quota.
    assert!(ids(on_demand).contains(&"eu-west-1".to_string()));
    assert!(!ids(spot).contains(&"eu-west-1".to_string()));
}

#[tokio::test]
async fn demo_choices_pick_three_regions() {
    let p = MockProvider::demo_aws();
    let c = region::candidates(&p, "g4dn.xlarge", true, |id| p.simulated_rtt(id))
        .await
        .unwrap();
    let choices = region::choose(c);
    assert_eq!(choices.lowest_latency.unwrap().region.id, "eu-central-1");
    // Stockholm is within 10 ms of Frankfurt and has no library copy cost,
    // which also makes it the cheapest once that cost is counted.
    assert_eq!(choices.recommended.unwrap().region.id, "eu-north-1");
    assert_eq!(choices.cheapest.unwrap().region.id, "eu-north-1");
    assert_eq!(choices.all.len(), 3);
}

fn region_with(id: &str, quotas: (u32, u32), prices: &[(&str, Price)]) -> MockRegion {
    MockRegion {
        region: aws::region(id, id),
        gpu_quota_on_demand: quotas.0,
        gpu_quota_spot: quotas.1,
        prices: prices
            .iter()
            .map(|(t, p)| (t.to_string(), *p))
            .collect::<HashMap<_, _>>(),
        library_copy_cost: 0.0,
        simulated_rtt: None,
    }
}

const GPU_ON_DEMAND_ONLY: Price = Price {
    on_demand_per_hour: 0.5,
    spot_per_hour: None,
};
const GPU_BOTH: Price = Price {
    on_demand_per_hour: 0.5,
    spot_per_hour: Some(0.2),
};
const CPU: Price = Price {
    on_demand_per_hour: 0.04,
    spot_per_hour: None,
};

#[tokio::test]
async fn regions_that_dont_sell_the_type_are_skipped_not_fatal() {
    let p = MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![
            region_with("has-gpu", (8, 8), &[("g4dn.xlarge", GPU_BOTH)]),
            region_with("cpu-only", (8, 8), &[("t3.medium", CPU)]),
            region_with("no-spot", (8, 8), &[("g4dn.xlarge", GPU_ON_DEMAND_ONLY)]),
        ],
    );
    let on_demand = region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    assert_eq!(on_demand.len(), 2);
    let spot = region::candidates(&p, "g4dn.xlarge", true, |_| None)
        .await
        .unwrap();
    assert_eq!(spot.len(), 1);
    assert_eq!(spot[0].region.id, "has-gpu");
}

#[tokio::test]
async fn quota_must_cover_the_gpu_types_vcpus() {
    let prices = [("g4dn.xlarge", GPU_BOTH), ("t3.medium", CPU)];
    let p = MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![
            region_with("enough", (4, 4), &prices),
            region_with("too-little", (2, 2), &prices),
        ],
    );
    let gpu = region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    assert_eq!(gpu.len(), 1);
    // A CPU machine is resized to GPU later, so it needs the same quota.
    let cpu = region::candidates(&p, "t3.medium", false, |_| None)
        .await
        .unwrap();
    assert_eq!(cpu.len(), 1);
    assert_eq!(cpu[0].region.id, "enough");

    let m = p
        .launch(&LaunchRequest {
            region_id: "too-little".into(),
            ..request("t3.medium", false)
        })
        .await
        .unwrap();
    p.stop(&m.id).await.unwrap();
    assert!(matches!(
        p.resize(&m.id, "g4dn.xlarge").await,
        Err(ProviderError::Cloud(_))
    ));
}

#[tokio::test]
async fn unknown_type_is_an_error() {
    let p = MockProvider::demo_aws();
    assert_eq!(
        region::candidates(&p, "x1.huge", false, |_| None).await,
        Err(ProviderError::UnknownMachineType("x1.huge".into()))
    );
}
