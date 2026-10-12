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
        cpu_first: false,
    }
}

#[tokio::test]
async fn cpu_then_gpu_lifecycle() {
    let p = MockProvider::demo_aws();
    let m = p.launch(&request("t3.medium", false)).await.unwrap();
    assert_eq!(m.state, MachineState::Running);

    // A running machine switches and keeps running.
    let m = p.resize(&m.id, "g4dn.xlarge").await.unwrap();
    assert!(m.machine_type.is_gpu());
    assert_eq!(m.state, MachineState::Running);

    p.stop(&m.id).await.unwrap();
    let m = p.resize(&m.id, "t3.medium").await.unwrap();
    assert!(!m.machine_type.is_gpu());
    assert_eq!(m.state, MachineState::Stopped);
    p.start(&m.id).await.unwrap();

    p.destroy(&m.id, false).await.unwrap();
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
        .unwrap()
        .found;
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
        .unwrap()
        .found;
    let spot = region::candidates(&p, "g4dn.xlarge", true, |_| None)
        .await
        .unwrap()
        .found;
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
    assert!(c.unreachable.is_empty());
    let choices = region::choose(c.found);
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
        .unwrap()
        .found;
    assert_eq!(on_demand.len(), 2);
    let spot = region::candidates(&p, "g4dn.xlarge", true, |_| None)
        .await
        .unwrap()
        .found;
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
        .unwrap()
        .found;
    assert_eq!(gpu.len(), 1);
    // A CPU machine is resized to GPU later, so it needs the same quota.
    let cpu = region::candidates(&p, "t3.medium", false, |_| None)
        .await
        .unwrap()
        .found;
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

/// The demo provider, except that every query about `broken` fails.
struct FlakyRegion(MockProvider);

#[async_trait::async_trait]
impl CloudProvider for FlakyRegion {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn display_name(&self) -> &str {
        self.0.display_name()
    }
    async fn ensure_storage(&self) -> rega_core::ProviderResult<rega_core::StorageLocation> {
        self.0.ensure_storage().await
    }
    async fn regions(&self) -> rega_core::ProviderResult<Vec<rega_core::Region>> {
        let mut regions = self.0.regions().await?;
        regions.push(aws::region("broken", "Broken"));
        Ok(regions)
    }
    async fn gpu_quota_vcpus(&self, region_id: &str, spot: bool) -> rega_core::ProviderResult<u32> {
        self.0.gpu_quota_vcpus(region_id, spot).await
    }
    fn machine_types(&self) -> Vec<rega_core::MachineType> {
        self.0.machine_types()
    }
    async fn price(
        &self,
        region_id: &str,
        type_name: &str,
    ) -> rega_core::ProviderResult<Option<Price>> {
        self.0.price(region_id, type_name).await
    }
    async fn library_copy_cost(&self, region_id: &str) -> rega_core::ProviderResult<f64> {
        self.0.library_copy_cost(region_id).await
    }
    async fn list_machines(&self) -> rega_core::ProviderResult<Vec<rega_core::Machine>> {
        self.0.list_machines().await
    }
    async fn launch(
        &self,
        request: &LaunchRequest,
    ) -> rega_core::ProviderResult<rega_core::Machine> {
        self.0.launch(request).await
    }
    async fn resize(
        &self,
        machine_id: &str,
        type_name: &str,
    ) -> rega_core::ProviderResult<rega_core::Machine> {
        self.0.resize(machine_id, type_name).await
    }
    async fn start(&self, machine_id: &str) -> rega_core::ProviderResult<rega_core::Machine> {
        self.0.start(machine_id).await
    }
    async fn stop(&self, machine_id: &str) -> rega_core::ProviderResult<rega_core::Machine> {
        self.0.stop(machine_id).await
    }
    async fn finish_stop(&self, machine_id: &str) -> rega_core::ProviderResult<rega_core::Machine> {
        self.0.finish_stop(machine_id).await
    }
    async fn destroy(
        &self,
        machine_id: &str,
        even_if_unsaved: bool,
    ) -> rega_core::ProviderResult<()> {
        self.0.destroy(machine_id, even_if_unsaved).await
    }
    async fn orphans(&self) -> rega_core::ProviderResult<Vec<rega_core::Orphan>> {
        self.0.orphans().await
    }
    async fn delete_orphans(&self, orphans: &[rega_core::Orphan]) -> rega_core::ProviderResult<()> {
        self.0.delete_orphans(orphans).await
    }
}

#[tokio::test]
async fn one_failing_region_is_skipped() {
    let p = FlakyRegion(MockProvider::demo_aws());
    let c = region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    // The mock rejects the unknown "broken" region; the other four remain,
    // and "broken" is reported as unreachable rather than silently dropped.
    assert_eq!(c.found.len(), 4);
    assert_eq!(c.unreachable.len(), 1);
    assert_eq!(c.unreachable[0].id, "broken");
}

#[tokio::test]
async fn every_region_failing_is_an_error() {
    let p = FlakyRegion(MockProvider::new("aws", "AWS", aws::catalog(), vec![]));
    assert!(region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .is_err());
}

#[tokio::test]
async fn resize_reports_spot_before_quota() {
    let p = MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![region_with("r", (0, 4), &[("g4dn.xlarge", GPU_BOTH)])],
    );
    let m = p
        .launch(&LaunchRequest {
            region_id: "r".into(),
            ..request("g4dn.xlarge", true)
        })
        .await
        .unwrap();
    p.stop(&m.id).await.unwrap();
    assert_eq!(
        p.resize(&m.id, "t3.medium").await,
        Err(ProviderError::SpotResize)
    );
}

#[tokio::test]
async fn failures_next_to_a_quota_less_region_are_still_reported() {
    // Milan answers (quota 0) and "broken" fails: no candidates, but the
    // failure shows up instead of looking like "no region offers this".
    let p = FlakyRegion(MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![region_with(
            "eu-south-1",
            (0, 0),
            &[("g4dn.xlarge", GPU_BOTH)],
        )],
    ));
    let c = region::candidates(&p, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    assert!(c.found.is_empty());
    assert_eq!(c.unreachable.len(), 1);
}

#[tokio::test]
async fn running_gpu_machines_use_up_quota() {
    let p = MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![region_with("r", (4, 4), &[("g4dn.xlarge", GPU_BOTH)])],
    );
    let first = p
        .launch(&LaunchRequest {
            region_id: "r".into(),
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap();
    let offered = || async {
        region::candidates(&p, "g4dn.xlarge", false, |_| None)
            .await
            .unwrap()
            .found
            .len()
    };
    assert_eq!(offered().await, 0);
    assert!(p
        .launch(&LaunchRequest {
            region_id: "r".into(),
            ..request("g4dn.xlarge", false)
        })
        .await
        .is_err());

    // Stopping frees the quota again.
    p.stop(&first.id).await.unwrap();
    assert_eq!(offered().await, 1);
}

#[tokio::test]
async fn start_is_refused_when_another_machine_took_the_quota() {
    let p = MockProvider::new(
        "aws",
        "AWS",
        aws::catalog(),
        vec![region_with("r", (4, 4), &[("g4dn.xlarge", GPU_BOTH)])],
    );
    let gpu = || LaunchRequest {
        region_id: "r".into(),
        ..request("g4dn.xlarge", false)
    };
    let a = p.launch(&gpu()).await.unwrap();
    p.stop(&a.id).await.unwrap();
    p.launch(&gpu()).await.unwrap();
    assert!(matches!(p.start(&a.id).await, Err(ProviderError::Cloud(_))));
}

#[tokio::test]
async fn deleting_orphans_removes_only_those_named() {
    let orphan = |id: &str| rega_core::Orphan {
        kind: rega_core::ResourceKind::Volume,
        id: id.into(),
        region_id: "eu-north-1".into(),
        machine_id: None,
        name: None,
    };
    let p = MockProvider::demo_aws().with_orphans(vec![orphan("vol-1"), orphan("vol-2")]);
    p.delete_orphans(&[orphan("vol-1")]).await.unwrap();
    assert_eq!(p.orphans().await.unwrap(), vec![orphan("vol-2")]);
}
