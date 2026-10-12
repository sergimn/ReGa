//! Read-only checks against a real AWS account. Ignored by default (CI has
//! no credentials). The app itself never reads credentials from the
//! environment; these tests take an access key from two variables of their
//! own, so nothing the computer already has is used by accident:
//!
//! ```sh
//! REGA_TEST_AWS_ACCESS_KEY_ID=... REGA_TEST_AWS_SECRET_ACCESS_KEY=... \
//!   cargo test -p rega_core --test aws_live -- --ignored --nocapture
//! ```
//!
//! They list and price things. The only change they make is getting the
//! buckets ready, as the app does the first time it lists machines.

use rega_core::aws::sdk::AccessKey;
use rega_core::aws::{AwsProvider, ConnectOptions};
use rega_core::{region, CloudProvider};

async fn connect() -> AwsProvider {
    let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
    let key = AccessKey {
        id: var("REGA_TEST_AWS_ACCESS_KEY_ID"),
        secret: var("REGA_TEST_AWS_SECRET_ACCESS_KEY"),
    };
    let aws = AwsProvider::connect(ConnectOptions::from_env(), &key)
        .await
        .expect("signing in with the test access key");
    println!("signed in as {}", aws.signed_in_as().unwrap_or_default());
    aws
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs AWS credentials"]
async fn reads_regions_quotas_and_prices() {
    let aws = connect().await;
    println!("account {}", aws.account_id());
    let regions = aws.regions().await.unwrap();
    assert!(regions.iter().any(|r| r.id == "eu-north-1"));
    println!("{} regions", regions.len());

    for r in ["eu-central-1", "eu-north-1"] {
        let on_demand = aws.gpu_quota_vcpus(r, false).await.unwrap();
        let spot = aws.gpu_quota_vcpus(r, true).await.unwrap();
        let price = aws.price(r, "g4dn.xlarge").await.unwrap();
        let cpu = aws.price(r, "t3.medium").await.unwrap();
        println!(
            "{r}: GPU quota {on_demand} on-demand / {spot} spot, g4dn {price:?}, t3.medium {cpu:?}"
        );
        let price = price.expect("g4dn.xlarge is sold here");
        assert!(price.on_demand_per_hour > 0.1);
        assert!(cpu.unwrap().spot_per_hour.is_none());
    }

    let candidates = region::candidates(&aws, "g4dn.xlarge", false, |_| None)
        .await
        .unwrap();
    println!(
        "launchable: {:?}, unreachable: {:?}",
        candidates
            .found
            .iter()
            .map(|c| (&c.region.id, c.price_per_hour))
            .collect::<Vec<_>>(),
        candidates.unreachable
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs AWS credentials"]
async fn lists_machines_and_orphans() {
    let aws = connect().await;
    println!("machines: {:?}", aws.list_machines().await.unwrap());
    println!("orphans: {:?}", aws.orphans().await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs AWS credentials"]
async fn storage_is_ready() {
    let aws = connect().await;
    println!("{:?}", aws.ensure_storage().await.unwrap());
}
