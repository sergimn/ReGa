//! The AWS provider against a fake account and a fake Terraform.

use std::sync::Arc;

use async_trait::async_trait;

use super::api::BucketInfo;
use super::fake::{Account, FakeAws, FakeTerraform, FixedIp, ACCOUNT, MY_IP};
use super::record::{self, InstanceState, MachineRecord};
use super::*;
use crate::setup::{ConnectionDetails, MachineAccess, MachineSecrets, MachineSetup};

struct Fixture {
    aws: FakeAws,
    terraform: Arc<FakeTerraform>,
    provider: AwsProvider,
    _dir: tempfile::TempDir,
}

fn fixture(account: Account) -> Fixture {
    let aws = FakeAws::new(account);
    let terraform = Arc::new(FakeTerraform::new(aws.clone()));
    let dir = tempfile::tempdir().unwrap();
    let provider = AwsProvider::new(
        Arc::new(aws.clone()),
        ACCOUNT.into(),
        terraform.clone(),
        dir.path().join("aws").join(ACCOUNT),
    )
    .with_ip_lookup(Arc::new(FixedIp(aws.clone())))
    .with_password_wait(Duration::from_millis(200), Duration::from_millis(1));
    Fixture {
        aws,
        terraform,
        provider,
        _dir: dir,
    }
}

fn request(type_name: &str, spot: bool) -> LaunchRequest {
    LaunchRequest {
        name: "Gaming PC".into(),
        region_id: "eu-central-1".into(),
        type_name: type_name.into(),
        disk_gib: 128,
        spot,
        wireguard: true,
        cpu_first: false,
    }
}

fn state_bucket_name() -> String {
    state_bucket(ACCOUNT)
}

impl Fixture {
    fn record(&self, machine_id: &str) -> MachineRecord {
        let body = self
            .aws
            .with(|a| {
                a.object(&state_bucket_name(), &record::record_key(machine_id))
                    .cloned()
            })
            .expect("record saved");
        MachineRecord::from_json(&body).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        self.aws.with(|a| a.calls.clone())
    }

    fn position(&self, call: &str) -> usize {
        self.calls()
            .iter()
            .position(|c| c == call)
            .unwrap_or_else(|| panic!("no call {call:?} in {:?}", self.calls()))
    }
}

#[tokio::test]
async fn first_run_creates_private_buckets() {
    let f = fixture(Account::typical());
    let location = f.provider.ensure_storage().await.unwrap();
    assert_eq!(location.state_bucket, "rega-tfstate-123456789012");
    assert_eq!(location.library_bucket, "rega-games-123456789012");
    assert_eq!(location.region_id, HOME_REGION);
    let calls = f.calls();
    assert!(calls.contains(&"create_bucket rega-tfstate-123456789012 eu-north-1".to_string()));
    assert!(calls.contains(&"create_bucket rega-games-123456789012 eu-north-1".to_string()));
    // Only the state bucket keeps versions.
    assert!(calls.contains(&"enable_versioning rega-tfstate-123456789012".to_string()));
    assert!(!calls.contains(&"enable_versioning rega-games-123456789012".to_string()));
    assert!(calls.contains(&"enable_encryption rega-games-123456789012".to_string()));
    assert!(calls.contains(&"tag_bucket rega-games-123456789012".to_string()));
    // Old versions of the state bucket's objects expire.
    assert!(calls.contains(&"expire_old_versions rega-tfstate-123456789012".to_string()));
    assert!(!calls.contains(&"expire_old_versions rega-games-123456789012".to_string()));

    // Done once per session.
    let before = f.calls().len();
    f.provider.ensure_storage().await.unwrap();
    assert_eq!(f.calls().len(), before);
}

#[tokio::test]
async fn existing_buckets_are_left_alone() {
    let mut account = Account::typical();
    let ready = BucketInfo {
        versioned: true,
        encrypted: true,
        public_access_blocked: true,
        tagged: true,
        lifecycle: true,
    };
    account.buckets.insert(state_bucket(ACCOUNT), ready);
    account.buckets.insert(library_bucket(ACCOUNT), ready);
    let f = fixture(account);
    f.provider.ensure_storage().await.unwrap();
    assert!(f.calls().iter().all(|c| c.starts_with("bucket ")));
}

#[tokio::test]
async fn a_public_bucket_is_refused_not_changed() {
    let mut account = Account::typical();
    account.buckets.insert(
        state_bucket(ACCOUNT),
        BucketInfo {
            versioned: true,
            encrypted: true,
            public_access_blocked: false,
            tagged: true,
            lifecycle: true,
        },
    );
    let f = fixture(account);
    let err = f.provider.ensure_storage().await.unwrap_err();
    assert!(err.to_string().contains("Block Public Access"), "{err}");
}

#[tokio::test]
async fn a_new_bucket_is_read_once_s3_shows_it() {
    let mut account = Account::typical();
    account.slow_new_buckets = 1;
    let f = fixture(account);
    f.provider.ensure_storage().await.unwrap();
}

#[tokio::test]
async fn regions_offering_nothing_are_hidden_and_failing_ones_kept() {
    let mut account = Account::typical();
    account.regions.push("eu-south-2".into());
    account.offered.insert("eu-south-2".into(), vec![]);
    account.regions.push("ap-east-9".into());
    account.broken_region = Some("ap-east-9".into());
    let f = fixture(account);
    let regions = f.provider.regions().await.unwrap();
    let ids: Vec<&str> = regions.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["ap-east-9", "eu-central-1", "eu-north-1"]);
    assert_eq!(regions[1].display_name, "Europe (Frankfurt)");
    // Unknown regions show their id.
    assert_eq!(regions[0].display_name, "ap-east-9");
    assert_eq!(
        regions[1].latency_probe_host,
        "ec2.eu-central-1.amazonaws.com"
    );
}

#[tokio::test]
async fn quota_is_what_is_left_after_running_machines() {
    let mut account = Account::typical();
    account.in_use.insert(("eu-central-1".into(), false), 4);
    let f = fixture(account);
    assert_eq!(
        f.provider.gpu_quota_vcpus("eu-central-1", false).await,
        Ok(4)
    );
    assert_eq!(
        f.provider.gpu_quota_vcpus("eu-central-1", true).await,
        Ok(4)
    );
    // No quota: running machines aren't even counted.
    assert_eq!(f.provider.gpu_quota_vcpus("eu-north-1", false).await, Ok(0));
    assert!(!f
        .calls()
        .contains(&"gpu_vcpus_in_use eu-north-1 false".to_string()));
}

#[tokio::test]
async fn prices_combine_the_price_list_and_spot() {
    let mut account = Account::typical();
    account
        .offered
        .insert("eu-north-1".into(), vec!["t3.medium".into()]);
    let f = fixture(account);
    let gpu = f
        .provider
        .price("eu-central-1", "g4dn.xlarge")
        .await
        .unwrap();
    assert_eq!(
        gpu,
        Some(Price {
            on_demand_per_hour: 0.84,
            spot_per_hour: Some(0.30),
        })
    );
    let cpu = f
        .provider
        .price("eu-central-1", "t3.medium")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cpu.spot_per_hour, None);
    // Not offered: no price, and the price list isn't asked.
    assert_eq!(
        f.provider.price("eu-north-1", "g4dn.xlarge").await,
        Ok(None)
    );
    assert!(!f
        .calls()
        .contains(&"on_demand_price eu-north-1 g4dn.xlarge".to_string()));
    // Offered but not in the price list (t3.large here): not sold.
    assert_eq!(f.provider.price("eu-central-1", "t3.large").await, Ok(None));
    assert!(matches!(
        f.provider.price("eu-central-1", "x1.huge").await,
        Err(ProviderError::UnknownMachineType(_))
    ));
}

#[tokio::test]
async fn prices_are_cached() {
    let f = fixture(Account::typical());
    f.provider
        .price("eu-central-1", "g4dn.xlarge")
        .await
        .unwrap();
    f.provider
        .price("eu-central-1", "g4dn.xlarge")
        .await
        .unwrap();
    let asked = f
        .calls()
        .iter()
        .filter(|c| c.starts_with("on_demand_price"))
        .count();
    assert_eq!(asked, 1);
}

#[tokio::test]
async fn copying_the_library_costs_per_gb_outside_home() {
    let mut account = Account::typical();
    account.objects.insert(
        (library_bucket(ACCOUNT), "steamapps/game.bin".into()),
        vec![0; 1000],
    );
    let f = fixture(account);
    assert_eq!(f.provider.library_copy_cost(HOME_REGION).await, Ok(0.0));
    let cost = f.provider.library_copy_cost("eu-central-1").await.unwrap();
    assert!((cost - 1000.0 / 1e9 * LIBRARY_COPY_USD_PER_GB).abs() < 1e-12);
}

#[tokio::test]
async fn launch_saves_the_record_before_creating_anything() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", false))
        .await
        .unwrap();
    assert!(m.id.starts_with("gaming-pc-"), "{}", m.id);
    assert_eq!(m.name, "Gaming PC");
    assert_eq!(m.state, MachineState::Running);
    assert!(m.machine_type.is_gpu());

    let record_saved = f.position(&format!("put_object {}", record::record_key(&m.id)));
    let key_saved = f.position(&format!("put_object {}", record::key_key(&m.id)));
    let applied = f.position(&format!("terraform apply {}", m.id));
    assert!(key_saved < applied && record_saved < applied);

    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_type, "g4dn.xlarge");
    assert_eq!(record.vars.root_volume_size, 128);
    assert!(record.vars.public_key.starts_with("ssh-rsa "));
    assert!(record.vars.client_cidrs.is_empty());
    // The module's zone (the first one selling the GPU type) is pinned
    // before the first apply, so a run that dies can't move the machine.
    assert_eq!(
        record.vars.availability_zone.as_deref(),
        Some("eu-central-1a")
    );

    let listed = f.provider.list_machines().await.unwrap();
    assert_eq!(listed, vec![m]);
}

#[tokio::test]
async fn spot_launches_in_the_cheapest_zone() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", true))
        .await
        .unwrap();
    assert!(m.spot);
    let record = f.record(&m.id);
    assert_eq!(
        record.vars.availability_zone.as_deref(),
        Some("eu-central-1a")
    );
    assert!(record.vars.spot);
}

#[tokio::test]
async fn launch_checks_before_touching_anything() {
    let f = fixture(Account::typical());
    assert_eq!(
        f.provider.launch(&request("t3.medium", true)).await,
        Err(ProviderError::SpotNeedsGpu)
    );
    let no_quota = LaunchRequest {
        region_id: "eu-north-1".into(),
        ..request("g4dn.xlarge", false)
    };
    let err = f.provider.launch(&no_quota).await.unwrap_err();
    assert!(err.to_string().contains("GPU quota"), "{err}");
    let too_big = LaunchRequest {
        disk_gib: 512,
        ..request("t3.medium", false)
    };
    assert!(matches!(
        f.provider.launch(&too_big).await,
        Err(ProviderError::InvalidState(_))
    ));
    assert!(f.calls().iter().all(|c| !c.starts_with("put_object")));
}

#[tokio::test]
async fn without_wireguard_sunshine_opens_to_this_device() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&LaunchRequest {
            wireguard: false,
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap();
    let record = f.record(&m.id);
    assert!(!record.vars.wireguard_enabled);
    assert_eq!(record.vars.client_cidrs, vec![format!("{MY_IP}/32")]);
}

#[tokio::test]
async fn a_failed_launch_leaves_a_machine_to_destroy() {
    let f = fixture(Account::typical());
    *f.terraform.fail_apply.lock().unwrap() = Some("Blocked: account".into());
    let err = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Blocked"));
    let machines = f.provider.list_machines().await.unwrap();
    assert_eq!(machines.len(), 1);
    assert_eq!(machines[0].state, MachineState::Terminated);
    assert!(f.provider.orphans().await.unwrap().is_empty());

    *f.terraform.fail_apply.lock().unwrap() = None;
    f.provider.destroy(&machines[0].id, false).await.unwrap();
    assert!(f.provider.list_machines().await.unwrap().is_empty());
}

#[tokio::test]
async fn cpu_then_gpu_lifecycle() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();

    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert_eq!(stopped.state, MachineState::Stopped);
    // Nothing synced D:, so it stays.
    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_state, InstanceState::Stopped);
    assert!(record.vars.library_volume_enabled);

    let resized = f.provider.resize(&m.id, "g4dn.xlarge").await.unwrap();
    assert!(resized.machine_type.is_gpu());
    assert_eq!(resized.state, MachineState::Stopped);

    let started = f.provider.start(&m.id).await.unwrap();
    assert_eq!(started.state, MachineState::Running);
    assert_eq!(started.machine_type.type_name, "g4dn.xlarge");

    f.provider.destroy(&m.id, false).await.unwrap();
    assert!(f.provider.list_machines().await.unwrap().is_empty());
    let left = f
        .aws
        .with(|a| a.objects.keys().filter(|(_, k)| k.contains(&m.id)).count());
    assert_eq!(left, 0);
    assert!(!f.provider.machine_dir(&m.id).exists());
}

#[tokio::test]
async fn starting_needs_free_quota() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", false))
        .await
        .unwrap();
    f.provider.stop(&m.id).await.unwrap();
    // Someone else's GPU machines take the whole quota.
    f.aws
        .with(|a| a.in_use.insert(("eu-central-1".into(), false), 8));
    let err = f.provider.start(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("GPU quota"), "{err}");
}

#[tokio::test]
async fn spot_machines_are_destroyed_not_stopped_or_resized() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", true))
        .await
        .unwrap();
    assert!(matches!(
        f.provider.stop(&m.id).await,
        Err(ProviderError::InvalidState(_))
    ));
    assert_eq!(
        f.provider.resize(&m.id, "t3.medium").await,
        Err(ProviderError::SpotResize)
    );
    // Terminating first keeps Terraform from cancelling the spot request,
    // which the IAM policy denies.
    f.provider.destroy(&m.id, false).await.unwrap();
    let terminated = f.position("delete Instance i-1");
    let destroyed = f.position(&format!("terraform destroy {}", m.id));
    assert!(terminated < destroyed);
}

#[tokio::test]
async fn unknown_machines_are_reported() {
    let f = fixture(Account::typical());
    assert_eq!(
        f.provider.start("nope").await,
        Err(ProviderError::UnknownMachine("nope".into()))
    );
}

#[tokio::test]
async fn orphans_are_tagged_resources_without_a_machine() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let stray = |kind, id: &str, machine: Option<&str>| Orphan {
        kind,
        id: id.into(),
        region_id: "eu-north-1".into(),
        machine_id: machine.map(String::from),
        name: None,
    };
    f.aws.with(|a| {
        a.resources
            .push(stray(ResourceKind::Volume, "vol-lost", Some("gone-123456")));
        a.resources
            .push(stray(ResourceKind::KeyPair, "rega-x", None));
        a.instances.push(api::Instance {
            id: "i-lost".into(),
            region_id: "eu-north-1".into(),
            machine_id: Some("gone-123456".into()),
            type_name: "t3.micro".into(),
            state: MachineState::Running,
            spot: false,
            public_ip: None,
            launched: None,
        });
    });
    let orphans = f.provider.orphans().await.unwrap();
    let ids: Vec<&str> = orphans.iter().map(|o| o.id.as_str()).collect();
    // Instances first; the launched machine's resources aren't orphans.
    assert_eq!(ids, ["i-lost", "vol-lost", "rega-x"]);
    assert!(orphans
        .iter()
        .all(|o| o.machine_id.as_deref() != Some(m.id.as_str())));

    // Something that stopped being an orphan is skipped.
    let mut asked = orphans.clone();
    asked.push(stray(ResourceKind::SecurityGroup, "sg-1", Some(&m.id)));
    f.provider.delete_orphans(&asked).await.unwrap();
    assert!(f.provider.orphans().await.unwrap().is_empty());
    assert!(!f.calls().contains(&"delete SecurityGroup sg-1".to_string()));
    let instance = f.position("delete Instance i-lost");
    let volume = f.position("delete Volume vol-lost");
    assert!(instance < volume);
}

fn rfc3339(secs: u64) -> String {
    use aws_sdk_s3::primitives::{DateTime, DateTimeFormat};
    DateTime::from_secs(secs as i64)
        .fmt(DateTimeFormat::DateTime)
        .unwrap()
}

fn lock(who: &str, created: u64) -> Vec<u8> {
    format!(
        r#"{{"ID": "1f0c", "Operation": "OperationTypeApply", "Who": "{who}",
            "Version": "1.16.5", "Created": "{}", "Path": "x"}}"#,
        rfc3339(created)
    )
    .into_bytes()
}

impl Fixture {
    fn put(&self, key: String, body: Vec<u8>) {
        self.aws
            .with(|a| a.objects.insert((state_bucket_name(), key), body));
    }

    fn has(&self, key: &str) -> bool {
        self.aws
            .with(|a| a.object(&state_bucket_name(), key).is_some())
    }

    fn stray_instance(&self, id: &str, machine_id: &str) {
        self.aws.with(|a| {
            a.instances.push(api::Instance {
                id: id.into(),
                region_id: "eu-central-1".into(),
                machine_id: Some(machine_id.into()),
                type_name: "t3.medium".into(),
                state: MachineState::Running,
                spot: false,
                public_ip: None,
                launched: None,
            })
        });
    }
}

#[tokio::test]
async fn an_instance_its_state_lost_is_a_leftover() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // Say a killed apply made a second instance and never wrote it down.
    f.stray_instance("i-extra", &m.id);
    let orphans = f.provider.orphans().await.unwrap();
    let ids: Vec<&str> = orphans.iter().map(|o| o.id.as_str()).collect();
    assert_eq!(ids, ["i-extra"]);
    // The machine is still the instance its state names.
    let listed = f.provider.list_machines().await.unwrap();
    assert_eq!(listed, vec![m]);
}

#[tokio::test]
async fn a_machine_being_changed_is_busy_and_keeps_its_resources() {
    let f = fixture(Account::typical());
    *f.terraform.fail_apply.lock().unwrap() = Some("Blocked".into());
    f.provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    let id = f.provider.list_machines().await.unwrap()[0].id.clone();

    // Another device is launching it: its lock is fresh, and the instance
    // it is creating isn't in the state yet.
    f.put(
        record::lock_key(&id),
        lock("sergi@laptop", terraform::unix_now()),
    );
    f.stray_instance("i-new", &id);
    let m = &f.provider.list_machines().await.unwrap()[0];
    assert!(m.busy);
    assert_eq!(m.state, MachineState::Running);
    assert!(f.provider.orphans().await.unwrap().is_empty());

    // A lock this old was left by a run that died.
    f.put(
        record::lock_key(&id),
        lock("sergi@laptop", terraform::unix_now() - 3 * 3600),
    );
    let m = &f.provider.list_machines().await.unwrap()[0];
    assert!(!m.busy);
    assert_eq!(m.state, MachineState::Terminated);
    let orphans = f.provider.orphans().await.unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, "i-new");
}

#[tokio::test]
async fn busy_with_nothing_created_yet_reads_as_pending() {
    let f = fixture(Account::typical());
    *f.terraform.fail_apply.lock().unwrap() = Some("Blocked".into());
    f.provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    let id = f.provider.list_machines().await.unwrap()[0].id.clone();
    let _acting = f.provider.begin(&id).unwrap();
    let m = &f.provider.list_machines().await.unwrap()[0];
    assert!(m.busy);
    assert_eq!(m.state, MachineState::Pending);
}

#[tokio::test]
async fn one_change_per_machine_at_a_time() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let acting = f.provider.begin(&m.id).unwrap();
    let err = f.provider.stop(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("still running"), "{err}");
    assert!(matches!(
        f.provider.destroy(&m.id, false).await,
        Err(ProviderError::InvalidState(_))
    ));
    drop(acting);
    f.provider.stop(&m.id).await.unwrap();
}

#[tokio::test]
async fn one_machine_runs_at_a_time() {
    let f = fixture(Account::typical());
    let first = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let err = f
        .provider
        .launch(&request("g4dn.xlarge", false))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("one machine at a time"), "{err}");

    f.provider.stop(&first.id).await.unwrap();
    let second = f
        .provider
        .launch(&LaunchRequest {
            name: "Den".into(),
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap();
    let err = f.provider.start(&first.id).await.unwrap_err();
    assert!(err.to_string().contains(&second.name), "{err}");
}

#[tokio::test]
async fn a_machine_starting_anywhere_blocks_starting_another() {
    let f = fixture(Account::typical());
    let mut ids = vec![];
    for name in ["Den", "Gaming"] {
        let m = f
            .provider
            .launch(&LaunchRequest {
                name: name.into(),
                ..request("t3.medium", false)
            })
            .await
            .unwrap();
        f.provider.stop(&m.id).await.unwrap();
        ids.push(m.id);
    }
    let (den, gaming) = (&ids[0], &ids[1]);

    // Another device is stopping Den: that doesn't block a start.
    f.put(
        record::lock_key(den),
        lock("sergi@laptop", terraform::unix_now()),
    );
    let listed = f.provider.list_machines().await.unwrap();
    assert!(listed
        .iter()
        .any(|m| m.name == "Den" && m.busy && !m.starting));
    f.provider.start(gaming).await.unwrap();
    f.provider.stop(gaming).await.unwrap();

    // Now it is starting Den, whose instance still reads stopped.
    let mut record = f.record(den);
    record.vars.instance_state = InstanceState::Running;
    f.put(record::record_key(den), record.to_json().into_bytes());
    let listed = f.provider.list_machines().await.unwrap();
    assert!(listed.iter().any(|m| m.name == "Den" && m.starting));
    let err = f.provider.start(gaming).await.unwrap_err();
    assert!(err.to_string().contains("Den is starting"), "{err}");
    let err = f.provider.resize(gaming, "g4dn.xlarge").await.unwrap_err();
    assert!(err.to_string().contains("Den is starting"), "{err}");
    f.aws.with(|a| {
        a.objects
            .remove(&(state_bucket_name(), record::lock_key(den)))
    });

    // A start in this app holds the others off until it is done.
    let booting = f.provider.boot().unwrap();
    let err = f.provider.start(gaming).await.unwrap_err();
    assert!(
        err.to_string().contains("another machine is starting"),
        "{err}"
    );
    assert!(matches!(
        f.provider.launch(&request("t3.medium", false)).await,
        Err(ProviderError::InvalidState(_))
    ));
    drop(booting);
    f.provider.start(gaming).await.unwrap();
}

#[tokio::test]
async fn listing_makes_the_buckets() {
    let f = fixture(Account::typical());
    assert!(f.provider.list_machines().await.unwrap().is_empty());
    assert!(f
        .calls()
        .contains(&"create_bucket rega-tfstate-123456789012 eu-north-1".to_string()));
}

#[tokio::test]
async fn a_lock_left_by_a_killed_run_here_is_cleared() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The app was killed mid-run: its marker names a process that's gone,
    // and the lock it took is still in the bucket.
    let started = terraform::unix_now() - 30;
    let marker = format!(r#"{{"pid": 4294967295, "start_ticks": 1, "started": {started}}}"#);
    std::fs::write(f.provider.machine_dir(&m.id).join("terraform.pid"), marker).unwrap();
    let host = terraform::host_name().unwrap();
    f.put(
        record::lock_key(&m.id),
        lock(&format!("me@{host}"), started + 2),
    );
    assert!(!f.provider.list_machines().await.unwrap()[0].busy);

    f.provider.stop(&m.id).await.unwrap();
    let cleared = f.position(&format!("delete_object {}", record::lock_key(&m.id)));
    let applied = f
        .calls()
        .iter()
        .rposition(|c| c == &format!("terraform apply {}", m.id))
        .unwrap();
    assert!(cleared < applied);
    assert!(!f.has(&record::lock_key(&m.id)));
}

#[tokio::test]
async fn a_run_from_another_flatpak_sandbox_keeps_its_lock() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The app quit mid-run and was opened again: the run goes on in the old
    // sandbox, whose processes this one can't see.
    let started = terraform::unix_now() - 30;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let marker = serde_json::json!({
        "pid": 2,
        "start_ticks": 1,
        "started": started,
        "pid_namespace": "pid:[1]",
        "boot_id": boot.trim(),
    });
    std::fs::write(
        f.provider.machine_dir(&m.id).join("terraform.pid"),
        marker.to_string(),
    )
    .unwrap();
    let host = terraform::host_name().unwrap();
    f.put(
        record::lock_key(&m.id),
        lock(&format!("me@{host}"), started + 2),
    );
    assert!(f.provider.list_machines().await.unwrap()[0].busy);

    f.provider.stop(&m.id).await.unwrap();
    assert!(!f
        .calls()
        .contains(&format!("delete_object {}", record::lock_key(&m.id))));
    assert!(f.has(&record::lock_key(&m.id)));
}

#[tokio::test]
async fn another_devices_live_lock_is_left_alone() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.put(
        record::lock_key(&m.id),
        lock("sergi@laptop", terraform::unix_now()),
    );
    f.provider.stop(&m.id).await.unwrap();
    assert!(f.has(&record::lock_key(&m.id)));
}

#[test]
fn stale_locks() {
    let now = 1_800_000_000;
    let created = |secs| LockInfo {
        id: "1".into(),
        who: "me@pc".into(),
        created: rfc3339(secs),
    };
    let dead = RunMarker {
        pid: 1,
        start_ticks: 1,
        started: now - 100,
        pid_namespace: None,
        boot_id: None,
    };
    // Fresh and nobody known to be dead: held.
    assert!(!stale_at(&created(now - 90), None, Some("pc"), now));
    // Taken by the dead run on this computer.
    assert!(stale_at(&created(now - 90), Some(&dead), Some("pc"), now));
    // Same time, but another computer took it.
    assert!(!stale_at(
        &created(now - 90),
        Some(&dead),
        Some("other"),
        now
    ));
    // Taken well after the dead run started: someone else's.
    assert!(!stale_at(
        &created(now - 100 + 3600),
        Some(&dead),
        Some("pc"),
        now + 3600
    ));
    // Older than any run.
    assert!(stale_at(&created(now - 3 * 3600), None, None, now));
    // Unreadable time: never broken.
    let garbled = LockInfo {
        created: "yesterday".into(),
        ..created(now)
    };
    assert!(!stale_at(&garbled, Some(&dead), Some("pc"), now));
}

#[test]
fn service_linked_role_errors_say_what_to_do() {
    let alarm = explain(ProviderError::Cloud(
        "creating CloudWatch Metric Alarm: AccessDenied: not authorized to perform \
         iam:CreateServiceLinkedRole"
            .into(),
    ));
    assert!(
        alarm.to_string().contains("events.amazonaws.com"),
        "{alarm}"
    );
    let spot = explain(ProviderError::Cloud(
        "AuthFailure.ServiceLinkedRoleCreationNotPermitted: spot instances".into(),
    ));
    assert!(spot.to_string().contains("spot.amazonaws.com"), "{spot}");
    assert_eq!(
        explain(ProviderError::Cloud("Blocked".into())),
        ProviderError::Cloud("Blocked".into())
    );
}

#[tokio::test]
async fn deleting_leftovers_keeps_going_past_one_that_fails() {
    let mut account = Account::typical();
    let stray = |kind, id: &str| Orphan {
        kind,
        id: id.into(),
        region_id: "eu-north-1".into(),
        machine_id: None,
        name: None,
    };
    account.resources = vec![
        stray(ResourceKind::SecurityGroup, "sg-busy"),
        stray(ResourceKind::KeyPair, "rega-x"),
    ];
    account.undeletable = Some("sg-busy".into());
    let f = fixture(account);
    let orphans = f.provider.orphans().await.unwrap();
    let err = f.provider.delete_orphans(&orphans).await.unwrap_err();
    assert!(err.to_string().contains("sg-busy"), "{err}");
    let left: Vec<String> = f
        .provider
        .orphans()
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(left, ["sg-busy"]);
}

#[tokio::test]
async fn an_unreadable_record_hides_only_itself() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.put(record::record_key("broken-000000"), b"{".to_vec());
    assert_eq!(f.provider.list_machines().await.unwrap(), vec![m]);
}

#[tokio::test]
async fn without_a_setup_hook_no_password_is_fetched() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.provider.stop(&m.id).await.unwrap();
    // NoSetup needs no access, so nothing can fail on fetching it.
    assert!(!f.calls().iter().any(|c| c.starts_with("password_data")));
}

/// A setup that records what it was asked to do, and plays the machine's
/// agent: before a stop it writes the final-sync record it is given.
struct Recorder {
    aws: FakeAws,
    /// Such as `after_start t3.medium`.
    events: std::sync::Mutex<Vec<String>>,
    accesses: std::sync::Mutex<Vec<MachineAccess>>,
    /// What the agent records before a stop: whether the upload worked,
    /// and when it finished, in seconds after the instance's last start.
    final_sync: std::sync::Mutex<Option<(bool, i64)>>,
    /// `before_stop` fails (say, WinRM timed out) without recording.
    fail_before_stop: bool,
    /// The setup fails.
    fail_after_start: bool,
    /// The tool the setup runs isn't installed.
    not_ready: bool,
    /// The game library restore failed.
    fail_wait_for_library: bool,
    /// Another device, which lists the machines while the setup runs.
    other_device: std::sync::Mutex<Option<Arc<AwsProvider>>>,
    /// What it saw then: whether the machine was busy, and starting.
    seen_from_elsewhere: std::sync::Mutex<Vec<(bool, bool)>>,
    /// What the setup learns about the machine's connection.
    connection: std::sync::Mutex<Option<ConnectionDetails>>,
}

impl Recorder {
    /// Records a good upload a minute after each start.
    fn new(aws: &FakeAws) -> Self {
        Self {
            aws: aws.clone(),
            events: Default::default(),
            accesses: Default::default(),
            final_sync: std::sync::Mutex::new(Some((true, 60))),
            fail_before_stop: false,
            fail_after_start: false,
            not_ready: false,
            fail_wait_for_library: false,
            other_device: Default::default(),
            seen_from_elsewhere: Default::default(),
            connection: std::sync::Mutex::new(Some(learnt())),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn note(&self, what: &str, machine: &MachineAccess) {
        self.events
            .lock()
            .unwrap()
            .push(format!("{what} {}", machine.type_name));
        self.accesses.lock().unwrap().push(machine.clone());
    }
}

#[async_trait]
impl MachineSetup for Recorder {
    fn ready(&self) -> ProviderResult<()> {
        if self.not_ready {
            return Err(ProviderError::Cloud("Ansible isn't installed".into()));
        }
        Ok(())
    }

    async fn after_start(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.note("after_start", machine);
        let other = self.other_device.lock().unwrap().clone();
        if let Some(other) = other {
            let listed = other.list_machines().await?;
            let m = listed.iter().find(|m| m.id == machine.machine_id).unwrap();
            self.seen_from_elsewhere
                .lock()
                .unwrap()
                .push((m.busy, m.starting));
        }
        if self.fail_after_start {
            return Err(ProviderError::Cloud(
                "Install Steam: download failed".into(),
            ));
        }
        Ok(())
    }

    async fn connection(
        &self,
        _machine: &MachineAccess,
    ) -> ProviderResult<Option<ConnectionDetails>> {
        Ok(self.connection.lock().unwrap().clone())
    }

    async fn wait_for_library(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.note("wait_for_library", machine);
        if self.fail_wait_for_library {
            return Err(ProviderError::Cloud(
                "restoring the game library failed: it doesn't fit".into(),
            ));
        }
        Ok(())
    }

    async fn before_stop(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.note("before_stop", machine);
        self.aws.call(format!("before_stop {}", machine.machine_id));
        if self.fail_before_stop {
            return Err(ProviderError::Cloud("WinRM timed out".into()));
        }
        if let Some((synced, after)) = *self.final_sync.lock().unwrap() {
            write_final_sync(&self.aws, &machine.machine_id, synced, after);
        }
        Ok(())
    }
}

/// What the agent writes to the library bucket after its last upload.
fn write_final_sync(aws: &FakeAws, machine_id: &str, synced: bool, after_start: i64) {
    aws.with(|a| {
        let instance = a
            .instances
            .iter()
            .find(|i| i.machine_id.as_deref() == Some(machine_id))
            .unwrap();
        let finished = instance.launched.unwrap() as i64 + after_start;
        let json = format!(
            r#"{{"machine_id": "{machine_id}", "instance_id": "{}", "synced": {synced},
                "boot_utc": "{}", "finished_utc": "{}", "skipped_apps": [], "failed_apps": []}}"#,
            instance.id,
            rfc3339(instance.launched.unwrap()),
            rfc3339(finished as u64),
        );
        a.objects.insert(
            (library_bucket(ACCOUNT), record::final_sync_key(machine_id)),
            json.into_bytes(),
        );
    });
}

/// An account whose new machines make their Windows password right away.
fn ready_account() -> Account {
    let mut account = Account::typical();
    account.windows_password = Some(("Pa55word!".into(), 0));
    account
}

fn with_setup(account: Account) -> (Fixture, Arc<Recorder>) {
    let mut f = fixture(account);
    let recorder = Arc::new(Recorder::new(&f.aws));
    f.provider = f.provider.with_setup(recorder.clone());
    (f, recorder)
}

impl Fixture {
    fn applies(&self, machine_id: &str) -> usize {
        let apply = format!("terraform apply {machine_id}");
        self.calls().iter().filter(|c| **c == apply).count()
    }

    /// Makes the record look saved long ago, as after a session of play.
    fn age_record(&self, machine_id: &str) {
        let mut record = self.record(machine_id);
        record.saved_at = 0;
        self.put(
            record::record_key(machine_id),
            record.to_json().into_bytes(),
        );
    }

    /// What the idle stop (or the backstop alarm) does: Windows shuts down
    /// and EC2 stops the instance, behind Terraform's back.
    fn stop_behind_terraforms_back(&self, machine_id: &str) {
        self.aws.with(|a| {
            for i in a
                .instances
                .iter_mut()
                .filter(|i| i.machine_id.as_deref() == Some(machine_id))
            {
                i.state = MachineState::Stopped;
            }
        });
    }
}

#[tokio::test]
async fn setup_runs_once_windows_has_made_its_password() {
    let mut account = Account::typical();
    account.windows_password = Some(("Pa55word!".into(), 2));
    let (f, setup) = with_setup(account);
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    assert_eq!(setup.events(), ["after_start t3.medium"]);
    let access = setup.accesses.lock().unwrap()[0].clone();
    assert_eq!(access.admin_password.as_deref(), Some("Pa55word!"));
    assert_eq!(access.public_ip.as_deref(), Some("198.51.100.1"));
    assert_eq!(access.dir, f.provider.machine_dir(&m.id));
    assert_eq!(access.library_bucket, library_bucket(ACCOUNT));
    assert_eq!(access.library_region, HOME_REGION);
    assert!(access.wireguard);
    // WinRM is open to this device only.
    assert_eq!(f.record(&m.id).vars.admin_cidrs, [format!("{MY_IP}/32")]);

    // The passwords live with the machine, so every device uses the same.
    let saved = f
        .aws
        .with(|a| {
            a.object(&state_bucket_name(), &record::secrets_key(&m.id))
                .cloned()
        })
        .unwrap();
    // Plus what the setup learnt, which the setup was handed nothing of.
    let saved = MachineSecrets::from_json(&saved).unwrap();
    assert_eq!(
        saved,
        MachineSecrets {
            wireguard_server_public_key: learnt().wireguard_server_public_key,
            sunshine_cert_sha256: learnt().sunshine_cert_sha256,
            ..access.secrets.clone()
        }
    );
    f.provider.stop(&m.id).await.unwrap();
    f.provider.start(&m.id).await.unwrap();
    let again = setup.accesses.lock().unwrap().last().unwrap().clone();
    assert_eq!(again.secrets, saved);
    assert_eq!(
        setup.events(),
        [
            "after_start t3.medium",
            "before_stop t3.medium",
            "after_start t3.medium"
        ]
    );
}

#[tokio::test]
async fn a_machine_without_a_password_fails_its_setup() {
    let (f, setup) = with_setup(Account::typical());
    let err = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("hasn't made its Administrator password"),
        "{err}"
    );
    assert!(setup.events().is_empty());
    // The machine is there, running, to stop or destroy.
    let listed = f.provider.list_machines().await.unwrap();
    assert_eq!(listed[0].state, MachineState::Running);
}

#[tokio::test]
async fn without_a_setup_winrm_stays_closed() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    assert!(f.record(&m.id).vars.admin_cidrs.is_empty());
    // NoSetup needs no access, so nothing can fail on fetching it.
    f.provider.stop(&m.id).await.unwrap();
    assert!(!f.calls().iter().any(|c| c.starts_with("password_data")));
}

#[tokio::test]
async fn stop_drops_the_library_disk_only_once_the_machine_saved_it() {
    // What the agent records before the stop, and whether D: goes.
    for (recorded, dropped) in [
        (Some((true, 60)), true),
        (Some((false, 60)), false),
        // Written before the last start: games installed since aren't in it.
        (Some((true, -60)), false),
        (None, false),
    ] {
        let (f, setup) = with_setup(ready_account());
        *setup.final_sync.lock().unwrap() = recorded;
        let m = f
            .provider
            .launch(&request("t3.medium", false))
            .await
            .unwrap();
        let stopped = f.provider.stop(&m.id).await.unwrap();
        assert_eq!(stopped.state, MachineState::Stopped);
        let record = f.record(&m.id);
        assert_eq!(record.vars.instance_state, InstanceState::Stopped);
        assert_eq!(!record.vars.library_volume_enabled, dropped, "{recorded:?}");
        assert_eq!(setup.events().last().unwrap(), "before_stop t3.medium");
    }
}

#[tokio::test]
async fn a_failing_upload_doesnt_block_a_stop() {
    let mut f = fixture(ready_account());
    let setup = Arc::new(Recorder {
        final_sync: Default::default(),
        fail_before_stop: true,
        ..Recorder::new(&f.aws)
    });
    f.provider = f.provider.with_setup(setup.clone());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert_eq!(stopped.state, MachineState::Stopped);
    // Nothing saved D:, so it stays.
    assert!(f.record(&m.id).vars.library_volume_enabled);
}

#[tokio::test]
async fn a_setup_that_cant_run_stops_a_launch_before_it_costs_anything() {
    let (f, _) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.provider.stop(&m.id).await.unwrap();

    let mut f = f;
    f.provider = f.provider.with_setup(Arc::new(Recorder {
        not_ready: true,
        ..Recorder::new(&f.aws)
    }));
    let err = f
        .provider
        .launch(&request("g4dn.xlarge", false))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("isn't installed"), "{err}");
    let err = f.provider.start(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("isn't installed"), "{err}");
    assert_eq!(f.provider.list_machines().await.unwrap().len(), 1);
    assert_eq!(f.applies(&m.id), 2);
    // Switching a stopped machine runs nothing on it, so it still works.
    f.provider.resize(&m.id, "g4dn.xlarge").await.unwrap();
}

#[tokio::test]
async fn a_machine_that_stopped_itself_is_finished_by_stop() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The idle stop: the agent saves the games, then Windows shuts down.
    write_final_sync(&f.aws, &m.id, true, 900);
    f.stop_behind_terraforms_back(&m.id);
    // Just after a save, a stopped instance may be another device starting
    // it, so it isn't taken for an idle stop yet.
    let listed = f.provider.list_machines().await.unwrap();
    assert_eq!(listed[0].state, MachineState::Stopped);
    assert!(!listed[0].stopped_itself);

    f.age_record(&m.id);
    let listed = f.provider.list_machines().await.unwrap();
    assert!(listed[0].stopped_itself);
    let events = setup.events().len();
    let done = f.provider.finish_stop(&m.id).await.unwrap();
    assert!(!done.stopped_itself);
    // Nothing to upload from a stopped machine: its record decides.
    assert_eq!(setup.events().len(), events);
    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_state, InstanceState::Stopped);
    assert!(!record.vars.library_volume_enabled);
    assert!(!f.provider.list_machines().await.unwrap()[0].stopped_itself);
}

#[tokio::test]
async fn a_backstop_stop_without_a_saved_library_keeps_the_disk() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The CloudWatch alarm stops it; the agent never ran its final upload.
    f.stop_behind_terraforms_back(&m.id);
    f.age_record(&m.id);
    assert!(f.provider.list_machines().await.unwrap()[0].stopped_itself);
    let done = f.provider.finish_stop(&m.id).await.unwrap();
    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_state, InstanceState::Stopped);
    assert!(record.vars.library_volume_enabled);
    // It says so, since the disk now costs money for nothing it knows of.
    assert!(done.library_kept);
}

#[tokio::test]
async fn finishing_a_stop_leaves_a_machine_someone_is_starting_alone() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    write_final_sync(&f.aws, &m.id, true, 900);
    f.stop_behind_terraforms_back(&m.id);
    // Its record was saved moments ago, as when another device has just
    // started it: not a machine that stopped itself, so it's left alone.
    let applies = f.applies(&m.id);
    let done = f.provider.finish_stop(&m.id).await.unwrap();
    assert!(!done.stopped_itself);
    assert_eq!(f.applies(&m.id), applies);
    assert_eq!(f.record(&m.id).vars.instance_state, InstanceState::Running);

    // Nor while another device runs its setup on it.
    f.age_record(&m.id);
    f.put(
        record::setup_key(&m.id),
        terraform::unix_now().to_string().into_bytes(),
    );
    let done = f.provider.finish_stop(&m.id).await.unwrap();
    assert!(done.busy);
    assert_eq!(f.applies(&m.id), applies);

    // A marker left by a device that quit halfway goes stale.
    f.put(
        record::setup_key(&m.id),
        (terraform::unix_now() - SETUP_STALE_SECS - 1)
            .to_string()
            .into_bytes(),
    );
    let done = f.provider.finish_stop(&m.id).await.unwrap();
    assert!(!done.busy);
    assert_eq!(f.applies(&m.id), applies + 1);
    assert!(!f.record(&m.id).vars.library_volume_enabled);
}

#[tokio::test]
async fn other_devices_see_a_machine_being_set_up_as_busy() {
    let (f, setup) = with_setup(ready_account());
    let other_dir = tempfile::tempdir().unwrap();
    let other = AwsProvider::new(
        Arc::new(f.aws.clone()),
        ACCOUNT.into(),
        Arc::new(FakeTerraform::new(f.aws.clone())),
        other_dir.path().to_path_buf(),
    )
    .with_ip_lookup(Arc::new(FixedIp(f.aws.clone())));
    *setup.other_device.lock().unwrap() = Some(Arc::new(other));
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // Busy, and starting, so the other device starts nothing else.
    assert_eq!(*setup.seen_from_elsewhere.lock().unwrap(), [(true, true)]);
    let marker = f.aws.with(|a| {
        a.object(&state_bucket_name(), &record::setup_key(&m.id))
            .is_some()
    });
    assert!(!marker, "the marker goes once the setup is done");
    assert!(!f.provider.list_machines().await.unwrap()[0].busy);
}

#[tokio::test]
async fn a_stop_that_didnt_get_through_cant_drop_the_disk_on_a_switch() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The stop saves the games and its record, then Terraform fails, so
    // the machine keeps running with a record that says stopped, no D:.
    *f.terraform.fail_apply.lock().unwrap() = Some("state lock timed out".into());
    assert!(f.provider.stop(&m.id).await.is_err());
    *f.terraform.fail_apply.lock().unwrap() = None;
    let stale = f.record(&m.id);
    assert_eq!(stale.vars.instance_state, InstanceState::Stopped);
    assert!(!stale.vars.library_volume_enabled);

    f.provider.resize(&m.id, "t3.large").await.unwrap();
    let applied = f.terraform.applied.lock().unwrap().last().cloned().unwrap();
    assert_eq!(applied.instance_type, "t3.large");
    assert_eq!(applied.instance_state, InstanceState::Running);
    assert!(applied.library_volume_enabled);
}

#[tokio::test]
async fn a_failed_upload_after_an_earlier_one_this_boot_keeps_the_disk() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // A stop uploads the games, then its Terraform run fails.
    *f.terraform.fail_apply.lock().unwrap() = Some("state lock timed out".into());
    assert!(f.provider.stop(&m.id).await.is_err());
    *f.terraform.fail_apply.lock().unwrap() = None;

    // The user plays on and installs more, then stops it from another
    // network, and this time the upload records nothing.
    f.aws.with(|a| a.my_ip = Some("203.0.113.9".into()));
    *setup.final_sync.lock().unwrap() = None;
    let stopped = f.provider.stop(&m.id).await.unwrap();
    let applied = f.terraform.applied.lock().unwrap().clone();
    // Opening WinRM to the new IP kept it running, with D:.
    let opened = applied
        .iter()
        .find(|v| v.admin_cidrs == ["203.0.113.9/32"])
        .unwrap();
    assert_eq!(opened.instance_state, InstanceState::Running);
    assert!(opened.library_volume_enabled);
    // The earlier upload doesn't vouch for the games installed since.
    let last = applied.last().unwrap();
    assert_eq!(last.instance_state, InstanceState::Stopped);
    assert!(last.library_volume_enabled);
    assert!(stopped.library_kept);
}

#[tokio::test]
async fn a_failed_upload_cant_lean_on_a_record_it_couldnt_clear() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // An upload earlier this boot, by a stop whose Terraform run failed.
    write_final_sync(&f.aws, &m.id, true, 60);
    // Now S3 won't delete that record, so this upload never starts.
    f.aws
        .with(|a| a.undeletable = Some(record::final_sync_key(&m.id)));
    let err = f.provider.destroy(&m.id, false).await.unwrap_err();
    assert!(matches!(err, ProviderError::GamesNotSaved(_)), "{err}");
    assert!(err.to_string().contains("can't be deleted"), "{err}");
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert!(stopped.library_kept);
    assert!(f.record(&m.id).vars.library_volume_enabled);
}

#[tokio::test]
async fn destroy_asks_before_deleting_the_disk_of_a_machine_aws_took_back() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", true))
        .await
        .unwrap();
    // AWS reclaims the spot instance mid-game; D: outlives it.
    f.aws.with(|a| {
        for i in a.instances.iter_mut() {
            i.state = MachineState::Terminated;
        }
    });
    let destroy = format!("terraform destroy {}", m.id);
    let err = f.provider.destroy(&m.id, false).await.unwrap_err();
    assert!(matches!(err, ProviderError::GamesNotSaved(_)), "{err}");
    assert!(
        err.to_string().contains("before its instance ended"),
        "{err}"
    );
    assert!(!f.calls().contains(&destroy));
    f.provider.destroy(&m.id, true).await.unwrap();
    assert!(f.calls().contains(&destroy));
}

#[tokio::test]
async fn a_machine_no_setup_started_on_has_no_games_to_save() {
    // Windows never makes its password, so the setup never starts.
    let (f, setup) = with_setup(Account::typical());
    f.provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    let m = f.provider.list_machines().await.unwrap().remove(0);
    assert!(!f.record(&m.id).setup_started);

    // Nothing to upload: the stop drops D:, and destroy doesn't ask.
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert!(!stopped.library_kept);
    assert!(!f.record(&m.id).vars.library_volume_enabled);
    f.provider.start(&m.id).await.unwrap_err();
    assert!(f.record(&m.id).vars.library_volume_enabled);
    f.provider.destroy(&m.id, false).await.unwrap();
    assert!(!setup.events().iter().any(|e| e.starts_with("before_stop")));
}

#[tokio::test]
async fn a_setup_that_failed_part_way_still_saves_the_games() {
    let mut f = fixture(ready_account());
    let setup = Arc::new(Recorder {
        fail_after_start: true,
        final_sync: Default::default(),
        ..Recorder::new(&f.aws)
    });
    f.provider = f.provider.with_setup(setup.clone());
    let err = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Install Steam"), "{err}");
    let m = f.provider.list_machines().await.unwrap().remove(0);
    // Every device knows the setup may have put games on D:.
    assert!(f.record(&m.id).setup_started);

    // It may have restored games and been played on: the upload runs, and
    // without a good one D: stays.
    let err = f.provider.destroy(&m.id, false).await.unwrap_err();
    assert!(matches!(err, ProviderError::GamesNotSaved(_)), "{err}");
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert!(stopped.library_kept);
    let uploads = setup
        .events()
        .iter()
        .filter(|e| e.starts_with("before_stop"))
        .count();
    assert_eq!(uploads, 2);
}

#[tokio::test]
async fn destroy_keeps_a_machine_whose_games_it_couldnt_save() {
    let mut f = fixture(ready_account());
    let setup = Arc::new(Recorder {
        final_sync: Default::default(),
        fail_before_stop: true,
        ..Recorder::new(&f.aws)
    });
    f.provider = f.provider.with_setup(setup.clone());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let destroy = format!("terraform destroy {}", m.id);
    let err = f.provider.destroy(&m.id, false).await.unwrap_err();
    assert!(matches!(err, ProviderError::GamesNotSaved(_)), "{err}");
    assert!(err.to_string().contains("WinRM timed out"), "{err}");
    assert!(!f.calls().contains(&destroy));
    let uploads = |f: &Fixture| {
        f.calls()
            .iter()
            .filter(|c| c.starts_with("before_stop"))
            .count()
    };
    assert_eq!(uploads(&f), 1);

    // Stopped, it keeps D: and the games on it.
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert!(stopped.library_kept);
    let err = f.provider.destroy(&m.id, false).await.unwrap_err();
    assert!(matches!(err, ProviderError::GamesNotSaved(_)), "{err}");
    assert!(!f.calls().contains(&destroy));

    // Unless the user says to go ahead.
    f.provider.destroy(&m.id, true).await.unwrap();
    assert!(f.calls().contains(&destroy));
    assert!(f.provider.list_machines().await.unwrap().is_empty());
}

#[tokio::test]
async fn destroying_anyway_doesnt_try_the_upload_again() {
    let mut f = fixture(ready_account());
    let setup = Arc::new(Recorder {
        final_sync: Default::default(),
        fail_before_stop: true,
        ..Recorder::new(&f.aws)
    });
    f.provider = f.provider.with_setup(setup.clone());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.provider.destroy(&m.id, false).await.unwrap_err();
    let before = f.calls();
    f.provider.destroy(&m.id, true).await.unwrap();
    let after = f.calls();
    assert!(
        !after[before.len()..]
            .iter()
            .any(|c| c.starts_with("before_stop")),
        "{after:?}"
    );
    assert!(f.provider.list_machines().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_stopped_machine_whose_games_were_saved_destroys_without_asking() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let stopped = f.provider.stop(&m.id).await.unwrap();
    assert!(!stopped.library_kept);
    f.provider.destroy(&m.id, false).await.unwrap();
}

#[tokio::test]
async fn switching_a_machine_that_stopped_itself_drops_its_saved_disk() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    write_final_sync(&f.aws, &m.id, true, 900);
    f.stop_behind_terraforms_back(&m.id);
    f.age_record(&m.id);
    f.provider.resize(&m.id, "t3.large").await.unwrap();
    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_state, InstanceState::Stopped);
    assert!(!record.vars.library_volume_enabled);
}

#[tokio::test]
async fn a_failed_restore_stops_a_cpu_first_launch_on_the_cpu_type() {
    let mut f = fixture(ready_account());
    let setup = Arc::new(Recorder {
        fail_wait_for_library: true,
        ..Recorder::new(&f.aws)
    });
    f.provider = f.provider.with_setup(setup.clone());
    let err = f
        .provider
        .launch(&LaunchRequest {
            cpu_first: true,
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("doesn't fit"), "{err}");
    // Still the cheap type: no GPU hours for a library that isn't there.
    let listed = f.provider.list_machines().await.unwrap();
    assert_eq!(listed[0].machine_type.type_name, "t3.medium");
    assert_eq!(listed[0].state, MachineState::Running);
}

#[tokio::test]
async fn spot_machines_never_count_as_stopped_by_themselves() {
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", true))
        .await
        .unwrap();
    f.stop_behind_terraforms_back(&m.id);
    f.age_record(&m.id);
    assert!(!f.provider.list_machines().await.unwrap()[0].stopped_itself);
}

#[tokio::test]
async fn cpu_first_sets_up_on_a_cpu_type_then_switches_to_the_gpu() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&LaunchRequest {
            cpu_first: true,
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap();
    assert_eq!(m.machine_type.type_name, "g4dn.xlarge");
    assert_eq!(m.state, MachineState::Running);
    assert_eq!(
        setup.events(),
        [
            "after_start t3.medium",
            "wait_for_library t3.medium",
            "after_start g4dn.xlarge"
        ]
    );
    // One machine, switched in place with D: kept.
    let record = f.record(&m.id);
    assert_eq!(record.vars.instance_type, "g4dn.xlarge");
    assert!(record.vars.library_volume_enabled);
    assert_eq!(f.applies(&m.id), 2);
    assert_eq!(f.aws.with(|a| a.instances.len()), 1);
}

#[tokio::test]
async fn cpu_first_is_only_for_on_demand_gpu_machines_with_a_setup() {
    // Spot machines can't change type.
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&LaunchRequest {
            cpu_first: true,
            ..request("g4dn.xlarge", true)
        })
        .await
        .unwrap();
    assert_eq!(setup.events(), ["after_start g4dn.xlarge"]);
    assert_eq!(f.applies(&m.id), 1);

    // Without a setup there is nothing to do on the CPU type.
    let f = fixture(Account::typical());
    let m = f
        .provider
        .launch(&LaunchRequest {
            cpu_first: true,
            ..request("g4dn.xlarge", false)
        })
        .await
        .unwrap();
    assert_eq!(f.record(&m.id).vars.instance_type, "g4dn.xlarge");
    assert_eq!(f.applies(&m.id), 1);
}

#[tokio::test]
async fn switching_a_running_machine_restarts_it_set_up() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let switched = f.provider.resize(&m.id, "g4dn.xlarge").await.unwrap();
    assert_eq!(switched.state, MachineState::Running);
    assert!(switched.machine_type.is_gpu());
    assert_eq!(
        setup.events(),
        ["after_start t3.medium", "after_start g4dn.xlarge"]
    );
    assert!(f.record(&m.id).vars.library_volume_enabled);

    // A stopped one stays stopped, with nothing to set up.
    f.provider.stop(&m.id).await.unwrap();
    let back = f.provider.resize(&m.id, "t3.medium").await.unwrap();
    assert_eq!(back.state, MachineState::Stopped);
    assert_eq!(setup.events().last().unwrap(), "before_stop g4dn.xlarge");
}

#[tokio::test]
async fn stop_opens_winrm_to_where_this_device_is_now() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The laptop moved to another network while the machine ran.
    f.aws.with(|a| a.my_ip = Some("192.0.2.9".into()));
    let before = f.applies(&m.id);
    f.provider.stop(&m.id).await.unwrap();
    // One apply to let WinRM in from the new IP before the upload, one to
    // stop.
    assert_eq!(f.applies(&m.id), before + 2);
    assert_eq!(f.record(&m.id).vars.admin_cidrs, ["192.0.2.9/32"]);
    assert_eq!(setup.events().last().unwrap(), "before_stop t3.medium");

    // From where it started, nothing extra.
    f.provider.start(&m.id).await.unwrap();
    let before = f.applies(&m.id);
    f.provider.stop(&m.id).await.unwrap();
    assert_eq!(f.applies(&m.id), before + 1);
}

#[tokio::test]
async fn destroying_a_running_machine_saves_its_games_first() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("g4dn.xlarge", true))
        .await
        .unwrap();
    f.provider.destroy(&m.id, false).await.unwrap();
    assert_eq!(setup.events().last().unwrap(), "before_stop g4dn.xlarge");
    // Before the spot instance is terminated.
    assert!(
        f.position(&format!("before_stop {}", m.id)) < f.position("delete Instance i-1"),
        "{:?}",
        f.calls()
    );
    // Its record of the last upload goes with it.
    let left = f.aws.with(|a| {
        a.object(&library_bucket(ACCOUNT), &record::final_sync_key(&m.id))
            .is_some()
    });
    assert!(!left);
}

/// The server key and certificate fingerprint a setup reads off the machine.
fn learnt() -> ConnectionDetails {
    ConnectionDetails {
        wireguard_server_public_key: Some("v++jn5TbyumgcnDNP7I1lcyU/BnB04Gsu/qedNNanzA=".into()),
        sunshine_cert_sha256: Some("ab".repeat(32)),
    }
}

impl Fixture {
    fn secrets(&self, machine_id: &str) -> MachineSecrets {
        let body = self
            .aws
            .with(|a| {
                a.object(&state_bucket_name(), &record::secrets_key(machine_id))
                    .cloned()
            })
            .expect("secrets saved");
        MachineSecrets::from_json(&body).unwrap()
    }

    fn secrets_saves(&self, machine_id: &str) -> usize {
        let put = format!("put_object {}", record::secrets_key(machine_id));
        self.calls().iter().filter(|c| **c == put).count()
    }
}

#[tokio::test]
async fn play_gets_the_tunnel_of_a_set_up_machine() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // The setup's findings are kept with the secrets, for every device.
    let secrets = f.secrets(&m.id);
    assert_eq!(
        secrets.wireguard_server_public_key,
        learnt().wireguard_server_public_key
    );
    assert_eq!(secrets.sunshine_cert_sha256, learnt().sunshine_cert_sha256);
    // The setup was given this machine's own client key.
    let access = setup.accesses.lock().unwrap()[0].clone();
    assert_eq!(
        access.secrets.wireguard_private_key,
        secrets.wireguard_private_key
    );

    let target = f.provider.stream_target(&m.id).await.unwrap();
    assert_eq!(target.machine_id, m.id);
    assert_eq!(target.public_ip, "198.51.100.1");
    assert_eq!(target.sunshine_user, "rega");
    assert_eq!(target.sunshine_password, secrets.sunshine_password);
    assert_eq!(target.sunshine_cert_sha256, secrets.sunshine_cert_sha256);
    let peer = target.wireguard.clone().unwrap();
    assert_eq!(peer.endpoint, "198.51.100.1:51820".parse().unwrap());
    assert_eq!(
        peer.server_public_key,
        learnt().wireguard_server_public_key.unwrap()
    );
    assert_eq!(peer.client_private_key, secrets.wireguard_private_key);
    assert_eq!(peer.client_address.to_string(), "10.213.37.2");
    assert_eq!(peer.server_address.to_string(), "10.213.37.1");
    assert_eq!(peer.mtu, 1400);
    // Nothing to secret in what it prints.
    let shown = format!("{target:?}");
    assert!(!shown.contains(&secrets.sunshine_password), "{shown}");
    assert!(!shown.contains(&secrets.wireguard_private_key), "{shown}");

    // Another device, which never ran the setup, gets the same.
    let other_dir = tempfile::tempdir().unwrap();
    let other = AwsProvider::new(
        Arc::new(f.aws.clone()),
        ACCOUNT.into(),
        Arc::new(FakeTerraform::new(f.aws.clone())),
        other_dir.path().to_path_buf(),
    )
    .with_ip_lookup(Arc::new(FixedIp(f.aws.clone())));
    assert_eq!(other.stream_target(&m.id).await.unwrap(), target);

    // With WireGuard on, Play changes nothing in AWS.
    let applies = f.applies(&m.id);
    f.provider.stream_target(&m.id).await.unwrap();
    assert_eq!(f.applies(&m.id), applies);

    f.provider.stop(&m.id).await.unwrap();
    let err = f.provider.stream_target(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("start it first"), "{err}");
}

#[tokio::test]
async fn the_setups_findings_are_saved_only_when_they_change() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let saves = f.secrets_saves(&m.id);
    f.provider.stop(&m.id).await.unwrap();
    f.provider.start(&m.id).await.unwrap();
    assert_eq!(f.secrets_saves(&m.id), saves);
    // Sunshine made a new certificate, say after its files were reset.
    *setup.connection.lock().unwrap() = Some(ConnectionDetails {
        sunshine_cert_sha256: Some("cd".repeat(32)),
        ..learnt()
    });
    f.provider.stop(&m.id).await.unwrap();
    f.provider.start(&m.id).await.unwrap();
    assert_eq!(f.secrets_saves(&m.id), saves + 1);
    assert_eq!(f.secrets(&m.id).sunshine_cert_sha256, Some("cd".repeat(32)));
}

#[tokio::test]
async fn play_needs_a_machine_the_setup_has_seen() {
    let (f, setup) = with_setup(ready_account());
    *setup.connection.lock().unwrap() = None;
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    let err = f.provider.stream_target(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("set up for streaming"), "{err}");
}

#[tokio::test]
async fn play_waits_for_a_busy_machine() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // Another device is setting it up.
    f.put(
        record::setup_key(&m.id),
        terraform::unix_now().to_string().into_bytes(),
    );
    let err = f.provider.stream_target(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("busy"), "{err}");
}

#[tokio::test]
async fn without_wireguard_play_follows_this_devices_ip() {
    let (f, _setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&LaunchRequest {
            wireguard: false,
            ..request("t3.medium", false)
        })
        .await
        .unwrap();
    let applies = f.applies(&m.id);
    let target = f.provider.stream_target(&m.id).await.unwrap();
    assert_eq!(target.wireguard, None);
    assert_eq!(target.sunshine_cert_sha256, Some("ab".repeat(32)));
    // Same IP: nothing to apply.
    assert_eq!(f.applies(&m.id), applies);

    // This device moved to another network.
    f.aws.with(|a| a.my_ip = Some("192.0.2.99".into()));
    f.provider.stream_target(&m.id).await.unwrap();
    assert_eq!(f.applies(&m.id), applies + 1);
    let vars = f.record(&m.id).vars;
    assert_eq!(vars.client_cidrs, ["192.0.2.99/32"]);
    assert_eq!(vars.admin_cidrs, ["192.0.2.99/32"]);
}

#[tokio::test]
async fn without_wireguard_play_needs_the_certificate() {
    let (f, setup) = with_setup(ready_account());
    *setup.connection.lock().unwrap() = Some(ConnectionDetails {
        sunshine_cert_sha256: None,
        ..learnt()
    });
    let m = f
        .provider
        .launch(&LaunchRequest {
            wireguard: false,
            ..request("t3.medium", false)
        })
        .await
        .unwrap();
    let err = f.provider.stream_target(&m.id).await.unwrap_err();
    assert!(err.to_string().contains("set up for streaming"), "{err}");
}

#[tokio::test]
async fn secrets_from_before_the_tunnel_get_a_key_once() {
    let (f, setup) = with_setup(ready_account());
    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    f.provider.stop(&m.id).await.unwrap();
    // As an app without the WireGuard client saved them.
    let old = r#"{"sunshine_password": "sun", "desktop_password": "desk-Rg7"}"#;
    f.put(record::secrets_key(&m.id), old.as_bytes().to_vec());
    f.provider.start(&m.id).await.unwrap();
    let secrets = f.secrets(&m.id);
    assert_eq!(secrets.sunshine_password, "sun");
    assert!(crate::stream::tunnel::public_key(&secrets.wireguard_private_key).is_ok());
    let used = setup.accesses.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        used.secrets.wireguard_private_key,
        secrets.wireguard_private_key
    );
    // And keeps it.
    f.provider.stop(&m.id).await.unwrap();
    f.provider.start(&m.id).await.unwrap();
    assert_eq!(
        f.secrets(&m.id).wireguard_private_key,
        secrets.wireguard_private_key
    );
}
