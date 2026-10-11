//! Exercises the provider interface and region picker through the mock.

use rega_core::mock::MockProvider;
use rega_core::region;
use rega_core::{CloudProvider, LaunchRequest, MachineState, ProviderError};

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
async fn demo_choices_pick_three_regions() {
    let p = MockProvider::demo_aws();
    let c = region::candidates(&p, "g4dn.xlarge", true, |id| p.simulated_rtt(id))
        .await
        .unwrap();
    let choices = region::choose(c);
    assert_eq!(choices.lowest_latency.unwrap().region.id, "eu-central-1");
    // Stockholm is within 10 ms of Frankfurt and has no library copy cost.
    assert_eq!(choices.recommended.unwrap().region.id, "eu-north-1");
    assert_eq!(choices.all.len(), 4);
}
