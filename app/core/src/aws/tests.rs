//! The AWS provider against a fake account and a fake Terraform.

use std::sync::Arc;

use async_trait::async_trait;

use super::api::BucketInfo;
use super::fake::{Account, FakeAws, FakeTerraform, FixedIp, ACCOUNT, MY_IP};
use super::keys::MachineKey;
use super::record::{self, InstanceState, MachineRecord};
use super::*;
use crate::setup::{LibrarySync, MachineAccess, MachineSetup};

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
    .with_ip_lookup(Arc::new(FixedIp));
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
        },
    );
    let f = fixture(account);
    let err = f.provider.ensure_storage().await.unwrap_err();
    assert!(err.to_string().contains("Block Public Access"), "{err}");
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
    // The zone Terraform picked is pinned for later runs.
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
    f.provider.destroy(&machines[0].id).await.unwrap();
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
    assert!(matches!(
        f.provider.resize(&m.id, "g4dn.xlarge").await,
        Err(ProviderError::InvalidState(_))
    ));

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

    f.provider.destroy(&m.id).await.unwrap();
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
    f.provider.destroy(&m.id).await.unwrap();
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

#[derive(Default)]
struct Recorder {
    started: std::sync::Mutex<Vec<MachineAccess>>,
}

#[async_trait]
impl MachineSetup for Recorder {
    async fn after_start(&self, machine: &MachineAccess) -> ProviderResult<()> {
        self.started.lock().unwrap().push(machine.clone());
        Ok(())
    }

    async fn before_stop(&self, _machine: &MachineAccess) -> ProviderResult<LibrarySync> {
        Ok(LibrarySync::Synced)
    }
}

#[tokio::test]
async fn setup_hook_gets_the_password_and_can_drop_the_library_disk() {
    let mut f = fixture(Account::typical());
    let recorder = Arc::new(Recorder::default());
    f.provider = f.provider.with_setup(recorder.clone());

    let m = f
        .provider
        .launch(&request("t3.medium", false))
        .await
        .unwrap();
    // Windows has no password yet on the first boot in this fake.
    let first = recorder.started.lock().unwrap()[0].clone();
    assert_eq!(first.admin_password, None);
    assert_eq!(first.public_ip.as_deref(), Some("198.51.100.1"));

    // Encrypt a password for the machine's key, as EC2 does.
    let pem = String::from_utf8(
        f.aws
            .with(|a| {
                a.object(&state_bucket_name(), &record::key_key(&m.id))
                    .cloned()
            })
            .unwrap(),
    )
    .unwrap();
    let data = encrypt_password(&MachineKey::from_pem(&pem).unwrap(), "Pa55word!");
    f.aws.with(|a| a.password_data.insert("i-1".into(), data));

    f.provider.stop(&m.id).await.unwrap();
    assert!(!f.record(&m.id).vars.library_volume_enabled);
    f.provider.start(&m.id).await.unwrap();
    let second = recorder.started.lock().unwrap()[1].clone();
    assert_eq!(second.admin_password.as_deref(), Some("Pa55word!"));
    assert!(f.record(&m.id).vars.library_volume_enabled);
}

fn encrypt_password(key: &MachineKey, password: &str) -> String {
    use aws_lc_rs::rsa::{Pkcs1PublicEncryptingKey, PrivateDecryptingKey};
    use base64::Engine;
    let pem = key.to_pem();
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .unwrap();
    let public =
        Pkcs1PublicEncryptingKey::new(PrivateDecryptingKey::from_pkcs8(&der).unwrap().public_key())
            .unwrap();
    let mut out = vec![0; public.ciphertext_size()];
    let cipher = public.encrypt(password.as_bytes(), &mut out).unwrap();
    base64::engine::general_purpose::STANDARD.encode(cipher)
}
