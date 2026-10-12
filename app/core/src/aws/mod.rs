//! AWS implementation of [`CloudProvider`].
//!
//! Prices, quotas and regions come from the AWS APIs; machines are created,
//! changed and destroyed by running the Terraform module in
//! `infra/terraform/aws`, one state per machine in the account's state
//! bucket. The S3 bucket is the source of truth, so every device signed in to
//! the account sees the same machines; each device keeps a local working copy
//! (module, variables, key and last state) under the app's data folder.

pub mod api;
mod cache;
pub mod keys;
mod pricing;
pub mod record;
pub mod sdk;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::{join_all, try_join_all};

use crate::ansible::{self, AnsibleSetup};
use crate::model::{
    roomy_cpu_type, Gpu, LaunchRequest, Machine, MachineSpec, MachineState, MachineType, Orphan,
    Price, Region, ResourceKind, StorageLocation,
};
use crate::provider::{CloudProvider, ProviderError, ProviderResult};
use crate::setup::{MachineAccess, MachineSecrets, MachineSetup, NoSetup};
use crate::stream::{self, StreamTarget, WireGuardPeer};
use crate::terraform::{self, io_error, Backend, LockInfo, RunMarker, Terraform};
use api::{AwsApi, BucketInfo, Instance};
use cache::TtlCache;
use keys::MachineKey;
use record::{FinalSync, InstanceState, MachineRecord, MachineVars};

/// Region holding the state and library buckets (also the development region).
pub const HOME_REGION: &str = "eu-north-1";

/// Smallest and largest system disk the module and the IAM policy accept.
pub const MIN_DISK_GIB: u32 = 30;
pub const MAX_DISK_GIB: u32 = 256;

/// What copying the game library out of [`HOME_REGION`] costs, per GB.
pub const LIBRARY_COPY_USD_PER_GB: f64 = 0.02;

/// How long an action waits for an earlier Terraform run on the same machine
/// to end, such as a launch that kept going after the app was closed.
const EARLIER_RUN_WAIT: Duration = Duration::from_secs(30 * 60);

/// A state lock this old was left by a run that died: no run of the module
/// takes this long.
const STALE_LOCK_SECS: u64 = 2 * 60 * 60;

/// How long Windows may take to make its Administrator password after the
/// first boot (usually a few minutes), and how often to look.
const PASSWORD_WAIT: Duration = Duration::from_secs(30 * 60);
const PASSWORD_POLL: Duration = Duration::from_secs(15);

/// A machine whose record says running but whose instance is stopped only
/// stopped itself if nobody changed the record for this long: a device that
/// just asked for a start saves the record a moment before Terraform takes
/// the state lock, and the idle stop can't come sooner after a start.
const SETTLED_SECS: u64 = 10 * 60;

/// How often a device running the setup (or the upload before a stop) on a
/// machine tells other devices so, and how long they take its word for it.
const SETUP_HEARTBEAT: Duration = Duration::from_secs(60);
const SETUP_STALE_SECS: u64 = 5 * 60;

/// Nobody has changed the record for [`SETTLED_SECS`].
fn settled(record: &MachineRecord) -> bool {
    terraform::unix_now().saturating_sub(record.saved_at) > SETTLED_SECS
}

/// Machine types the IAM policy allows the app to use.
pub fn catalog() -> Vec<MachineType> {
    let cpu = |name: &str, vcpus, ram_gib| MachineType {
        type_name: name.into(),
        spec: MachineSpec {
            vcpus,
            ram_gib,
            gpu: None,
        },
    };
    vec![
        MachineType {
            type_name: "g4dn.xlarge".into(),
            spec: MachineSpec {
                vcpus: 4,
                ram_gib: 16.0,
                gpu: Some(Gpu {
                    model: "NVIDIA T4".into(),
                    vram_gib: 16,
                }),
            },
        },
        cpu("t3.micro", 2, 1.0),
        cpu("t3.medium", 2, 4.0),
        cpu("t3.large", 2, 8.0),
    ]
}

pub fn state_bucket(account_id: &str) -> String {
    format!("rega-tfstate-{account_id}")
}

pub fn library_bucket(account_id: &str) -> String {
    format!("rega-games-{account_id}")
}

pub fn region(id: &str, display_name: &str) -> Region {
    Region {
        id: id.into(),
        display_name: display_name.into(),
        latency_probe_host: format!("ec2.{id}.amazonaws.com"),
    }
}

/// The console name of a region, or its id when this app version doesn't
/// know it yet.
pub fn region_name(id: &str) -> String {
    let name = match id {
        "af-south-1" => "Africa (Cape Town)",
        "ap-east-1" => "Asia Pacific (Hong Kong)",
        "ap-east-2" => "Asia Pacific (Taipei)",
        "ap-northeast-1" => "Asia Pacific (Tokyo)",
        "ap-northeast-2" => "Asia Pacific (Seoul)",
        "ap-northeast-3" => "Asia Pacific (Osaka)",
        "ap-south-1" => "Asia Pacific (Mumbai)",
        "ap-south-2" => "Asia Pacific (Hyderabad)",
        "ap-southeast-1" => "Asia Pacific (Singapore)",
        "ap-southeast-2" => "Asia Pacific (Sydney)",
        "ap-southeast-3" => "Asia Pacific (Jakarta)",
        "ap-southeast-4" => "Asia Pacific (Melbourne)",
        "ap-southeast-5" => "Asia Pacific (Malaysia)",
        "ap-southeast-6" => "Asia Pacific (New Zealand)",
        "ap-southeast-7" => "Asia Pacific (Thailand)",
        "ca-central-1" => "Canada (Central)",
        "ca-west-1" => "Canada West (Calgary)",
        "eu-central-1" => "Europe (Frankfurt)",
        "eu-central-2" => "Europe (Zurich)",
        "eu-north-1" => "Europe (Stockholm)",
        "eu-south-1" => "Europe (Milan)",
        "eu-south-2" => "Europe (Spain)",
        "eu-west-1" => "Europe (Ireland)",
        "eu-west-2" => "Europe (London)",
        "eu-west-3" => "Europe (Paris)",
        "il-central-1" => "Israel (Tel Aviv)",
        "me-central-1" => "Middle East (UAE)",
        "me-south-1" => "Middle East (Bahrain)",
        "mx-central-1" => "Mexico (Central)",
        "sa-east-1" => "South America (São Paulo)",
        "us-east-1" => "US East (N. Virginia)",
        "us-east-2" => "US East (Ohio)",
        "us-west-1" => "US West (N. California)",
        "us-west-2" => "US West (Oregon)",
        _ => id,
    };
    name.into()
}

/// Finds this device's public IP, for the firewall rules that open
/// Sunshine (WireGuard off) or WinRM to it.
#[async_trait]
pub trait IpLookup: Send + Sync {
    async fn public_ip(&self) -> ProviderResult<String>;
}

/// Asks `checkip.amazonaws.com`.
pub struct CheckIp;

#[async_trait]
impl IpLookup for CheckIp {
    async fn public_ip(&self) -> ProviderResult<String> {
        let fail =
            |e: reqwest::Error| ProviderError::Cloud(format!("couldn't find your public IP: {e}"));
        let body = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(fail)?
            .get("https://checkip.amazonaws.com")
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(fail)?
            .text()
            .await
            .map_err(fail)?;
        let ip = body.trim();
        ip.parse::<std::net::Ipv4Addr>()
            .map(|_| ip.to_string())
            .map_err(|_| ProviderError::Cloud(format!("unexpected public IP answer {ip:?}")))
    }
}

/// Where the app keeps its per-device files.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// Per-machine working copies go in `<data_dir>/aws/<account>/`.
    pub data_dir: PathBuf,
    /// Terraform provider plugins are cached in `<cache_dir>/terraform-plugins`.
    pub cache_dir: PathBuf,
}

impl ConnectOptions {
    /// `$XDG_DATA_HOME/rega` and `$XDG_CACHE_HOME/rega`, with the usual
    /// fallbacks under `$HOME`. Flatpak points both inside the sandbox.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let xdg = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
                .join("rega")
        };
        Self {
            data_dir: xdg("XDG_DATA_HOME", ".local/share"),
            cache_dir: xdg("XDG_CACHE_HOME", ".cache"),
        }
    }
}

struct Caches {
    regions: TtlCache<(), Vec<String>>,
    offered: TtlCache<String, Vec<String>>,
    quotas: TtlCache<(String, bool), u32>,
    on_demand: TtlCache<(String, String), Option<f64>>,
    spot: TtlCache<(String, String), Vec<(String, f64)>>,
    library_bytes: TtlCache<(), u64>,
}

impl Caches {
    fn new() -> Self {
        let mins = |m: u64| Duration::from_secs(60 * m);
        Self {
            regions: TtlCache::new(mins(24 * 60)),
            offered: TtlCache::new(mins(24 * 60)),
            quotas: TtlCache::new(mins(10)),
            on_demand: TtlCache::new(mins(24 * 60)),
            spot: TtlCache::new(mins(5)),
            library_bytes: TtlCache::new(mins(10)),
        }
    }
}

/// What the state bucket says about the machines, read in one go.
struct Snapshot {
    /// Readable records; one this app can't read is left out.
    records: Vec<MachineRecord>,
    /// The resource ids each machine's Terraform state names.
    owned: HashMap<String, HashSet<String>>,
    /// Machines with a change running, here or on another device.
    busy: HashSet<String>,
}

pub struct AwsProvider {
    api: Arc<dyn AwsApi>,
    account_id: String,
    terraform: Arc<dyn Terraform>,
    setup: Arc<dyn MachineSetup>,
    ip: Arc<dyn IpLookup>,
    /// One folder per machine: the Terraform working copy and cached files.
    machines_dir: PathBuf,
    caches: Caches,
    storage: tokio::sync::Mutex<Option<StorageLocation>>,
    /// Terraform runs one at a time: they share the plugin cache, and the
    /// account's GPU quota only fits one machine anyway.
    terraform_lock: tokio::sync::Mutex<()>,
    /// One action per machine at a time, so a stop can't land in the middle
    /// of a launch.
    actions: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Held by a launch, start or resize from its check that no other
    /// machine is up until Terraform is done, so two can't both pass it.
    booting: tokio::sync::Mutex<()>,
    /// How long to wait for a new machine's Windows password, and how often
    /// to look.
    password_wait: Duration,
    password_poll: Duration,
}

impl AwsProvider {
    pub fn new(
        api: Arc<dyn AwsApi>,
        account_id: String,
        terraform: Arc<dyn Terraform>,
        machines_dir: PathBuf,
    ) -> Self {
        Self {
            api,
            account_id,
            terraform,
            setup: Arc::new(NoSetup),
            ip: Arc::new(CheckIp),
            machines_dir,
            caches: Caches::new(),
            storage: tokio::sync::Mutex::new(None),
            terraform_lock: tokio::sync::Mutex::new(()),
            actions: Mutex::new(HashMap::new()),
            booting: tokio::sync::Mutex::new(()),
            password_wait: PASSWORD_WAIT,
            password_poll: PASSWORD_POLL,
        }
    }

    /// Signs in with the default AWS credential chain (environment, then
    /// `~/.aws` profiles). [`ProviderError::MissingCredentials`] when there
    /// are none. Machines are set up with Ansible ([`AnsibleSetup`]).
    pub async fn connect(options: ConnectOptions) -> ProviderResult<Self> {
        let (api, account_id) = sdk::SdkApi::connect().await?;
        let terraform = terraform::TerraformCli::find(options.cache_dir.join("terraform-plugins"));
        let machines_dir = options.data_dir.join("aws").join(&account_id);
        Ok(
            Self::new(Arc::new(api), account_id, Arc::new(terraform), machines_dir)
                .with_setup(Arc::new(AnsibleSetup::find())),
        )
    }

    /// Runs `setup` after starts and before stops (Ansible).
    pub fn with_setup(mut self, setup: Arc<dyn MachineSetup>) -> Self {
        self.setup = setup;
        self
    }

    pub fn with_ip_lookup(mut self, ip: Arc<dyn IpLookup>) -> Self {
        self.ip = ip;
        self
    }

    #[cfg(test)]
    fn with_password_wait(mut self, wait: Duration, poll: Duration) -> Self {
        self.password_wait = wait;
        self.password_poll = poll;
        self
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    fn state_bucket(&self) -> String {
        state_bucket(&self.account_id)
    }

    fn machine_type(&self, type_name: &str) -> ProviderResult<MachineType> {
        catalog()
            .into_iter()
            .find(|t| t.type_name == type_name)
            .ok_or_else(|| ProviderError::UnknownMachineType(type_name.into()))
    }

    async fn enabled_regions(&self) -> ProviderResult<Vec<String>> {
        self.caches.regions.get_or_try((), self.api.regions()).await
    }

    async fn offered(&self, region_id: &str) -> ProviderResult<Vec<String>> {
        let types: Vec<String> = catalog().into_iter().map(|t| t.type_name).collect();
        self.caches
            .offered
            .get_or_try(
                region_id.to_string(),
                self.api.offered_types(region_id, &types),
            )
            .await
    }

    async fn check_gpu_quota(
        &self,
        region_id: &str,
        machine_type: &MachineType,
        spot: bool,
    ) -> ProviderResult<()> {
        if machine_type.is_gpu()
            && self.gpu_quota_vcpus(region_id, spot).await? < machine_type.spec.vcpus
        {
            return Err(ProviderError::Cloud(format!(
                "not enough {} GPU quota in {} for a {}",
                if spot { "spot" } else { "on-demand" },
                region_name(region_id),
                machine_type.type_name
            )));
        }
        Ok(())
    }

    /// ReGa runs one machine at a time: refuses while another one is up, or
    /// is being started here or on another device. Hold [`Self::boot`]
    /// from this check until the machine is up.
    async fn check_none_running(&self, except: Option<&str>) -> ProviderResult<()> {
        for m in self.list_machines().await? {
            if Some(m.id.as_str()) == except {
                continue;
            }
            let up = matches!(
                m.state,
                MachineState::Pending | MachineState::Running | MachineState::Stopping
            );
            if up || m.starting {
                return Err(ProviderError::InvalidState(format!(
                    "{} is {}. ReGa runs one machine at a time, so {}.",
                    m.name,
                    if up { "still running" } else { "starting" },
                    if up {
                        "stop or destroy it first"
                    } else {
                        "wait for it to finish"
                    }
                )));
            }
        }
        Ok(())
    }

    /// Claims the right to bring a machine up; fails while another launch,
    /// start or resize in this app has it.
    fn boot(&self) -> ProviderResult<tokio::sync::MutexGuard<'_, ()>> {
        self.booting.try_lock().map_err(|_| {
            ProviderError::InvalidState(
                "another machine is starting. ReGa runs one machine at a time, so wait for it \
                 to finish."
                    .into(),
            )
        })
    }

    /// Claims `machine_id` for one action; fails while another one runs.
    fn begin(&self, machine_id: &str) -> ProviderResult<tokio::sync::OwnedMutexGuard<()>> {
        let lock = self
            .actions
            .lock()
            .unwrap()
            .entry(machine_id.into())
            .or_default()
            .clone();
        lock.try_lock_owned().map_err(|_| {
            ProviderError::InvalidState(
                "another change to this machine is still running; try again when it's done".into(),
            )
        })
    }

    /// Whether this app is in the middle of an action on `machine_id`.
    fn acting_on(&self, machine_id: &str) -> bool {
        self.actions
            .lock()
            .unwrap()
            .get(machine_id)
            .is_some_and(|m| m.try_lock().is_err())
    }

    fn machine_dir(&self, machine_id: &str) -> PathBuf {
        self.machines_dir.join(machine_id)
    }

    /// Reads every record, state and lock in the state bucket, making the
    /// buckets on first use.
    async fn snapshot(&self) -> ProviderResult<Snapshot> {
        self.ensure_storage().await?;
        let bucket = self.state_bucket();
        let objects = self.api.list_objects(&bucket, record::PREFIX).await?;
        // The machines with an object at `key_of(id)`.
        let ids_with = |key_of: fn(&str) -> String| -> Vec<String> {
            objects
                .iter()
                .filter_map(|(key, _)| record::machine_id_of(key).filter(|id| key_of(id) == *key))
                .map(String::from)
                .collect()
        };
        let (record_ids, state_ids, lock_ids, setup_ids) = (
            ids_with(record::record_key),
            ids_with(record::state_key),
            ids_with(record::lock_key),
            ids_with(record::setup_key),
        );

        let get = |key: String| {
            let bucket = &bucket;
            async move { self.api.get_object(bucket, &key).await }
        };
        let (bodies, states, locks, setups) = futures::try_join!(
            try_join_all(record_ids.iter().map(|id| get(record::record_key(id)))),
            try_join_all(state_ids.iter().map(|id| get(record::state_key(id)))),
            try_join_all(lock_ids.iter().map(|id| get(record::lock_key(id)))),
            try_join_all(setup_ids.iter().map(|id| get(record::setup_key(id)))),
        )?;
        // A record this app can't read (garbled, or saved by a newer
        // version) is left out rather than hiding every other machine. Its
        // resources stay owned by its state, so none show as leftovers.
        let records = bodies
            .into_iter()
            .flatten()
            .filter_map(|body| MachineRecord::from_json(&body).ok())
            .collect();
        let mut owned = HashMap::new();
        for (id, state) in state_ids.into_iter().zip(states) {
            if let Some(state) = state {
                owned.insert(id, record::state_resource_ids(&state)?);
            }
        }
        let mut busy = HashSet::new();
        for id in &record_ids {
            let marker = terraform::run_marker(&self.machine_dir(id)).await;
            if self.acting_on(id) || marker.as_ref().is_some_and(RunMarker::is_running) {
                busy.insert(id.clone());
            }
        }
        for (id, lock) in lock_ids.into_iter().zip(locks) {
            let Some(lock) = lock.as_deref().and_then(LockInfo::parse) else {
                continue;
            };
            let dead_run = terraform::run_marker(&self.machine_dir(&id))
                .await
                .filter(|m| !m.is_running());
            if !lock_is_stale(&lock, dead_run.as_ref()) {
                busy.insert(id);
            }
        }
        // A device running the setup refreshes its marker; one that quit
        // halfway leaves a marker that soon goes stale.
        for (id, marker) in setup_ids.into_iter().zip(setups) {
            let fresh = marker
                .as_deref()
                .and_then(|m| std::str::from_utf8(m).ok())
                .and_then(|m| m.trim().parse::<u64>().ok())
                .is_some_and(|at| terraform::unix_now().saturating_sub(at) < SETUP_STALE_SECS);
            if fresh {
                busy.insert(id);
            }
        }
        Ok(Snapshot {
            records,
            owned,
            busy,
        })
    }

    async fn record(&self, machine_id: &str) -> ProviderResult<MachineRecord> {
        let body = self
            .api
            .get_object(&self.state_bucket(), &record::record_key(machine_id))
            .await?
            .ok_or_else(|| ProviderError::UnknownMachine(machine_id.into()))?;
        let record = MachineRecord::from_json(&body)?;
        if record.id() != machine_id {
            return Err(ProviderError::Cloud(format!(
                "record of machine {machine_id} names {}",
                record.id()
            )));
        }
        Ok(record)
    }

    /// The resource ids the machine's Terraform state names, `None` when it
    /// has no state yet.
    async fn owned_ids(&self, machine_id: &str) -> ProviderResult<Option<HashSet<String>>> {
        self.api
            .get_object(&self.state_bucket(), &record::state_key(machine_id))
            .await?
            .map(|state| record::state_resource_ids(&state))
            .transpose()
    }

    async fn save_record(&self, record: &mut MachineRecord) -> ProviderResult<()> {
        record.saved_at = terraform::unix_now();
        let json = record.to_json();
        self.api
            .put_object(
                &self.state_bucket(),
                &record::record_key(record.id()),
                json.clone().into_bytes(),
            )
            .await?;
        self.write_local(record.id(), "record.json", &json).await
    }

    /// Writes a private file into the machine's local folder.
    async fn write_local(
        &self,
        machine_id: &str,
        name: &str,
        contents: &str,
    ) -> ProviderResult<()> {
        use tokio::io::AsyncWriteExt;
        let dir = self.machine_dir(machine_id);
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| io_error("create the machine folder", e))?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(dir.join(name))
            .await
            .map_err(|e| io_error("save a machine file", e))?;
        file.write_all(contents.as_bytes())
            .await
            .map_err(|e| io_error("save a machine file", e))
    }

    /// The machine's key: the local copy, else the one in the state bucket.
    async fn key(&self, machine_id: &str) -> ProviderResult<Option<MachineKey>> {
        let local = self.machine_dir(machine_id).join("key.pem");
        if let Ok(pem) = tokio::fs::read_to_string(&local).await {
            return MachineKey::from_pem(&pem).map(Some);
        }
        let Some(pem) = self
            .api
            .get_object(&self.state_bucket(), &record::key_key(machine_id))
            .await?
        else {
            return Ok(None);
        };
        let pem = String::from_utf8(pem)
            .map_err(|_| ProviderError::Cloud(format!("machine {machine_id}: unreadable key")))?;
        let key = MachineKey::from_pem(&pem)?;
        self.write_local(machine_id, "key.pem", &pem).await?;
        Ok(Some(key))
    }

    /// The passwords the setup gives the machine and its WireGuard key,
    /// made the first time.
    async fn secrets(&self, machine_id: &str) -> ProviderResult<MachineSecrets> {
        let key = record::secrets_key(machine_id);
        if let Some(json) = self.api.get_object(&self.state_bucket(), &key).await? {
            let mut secrets = MachineSecrets::from_json(&json)?;
            // Saved by an app from before the WireGuard client.
            if secrets.add_missing_key()? {
                self.save_secrets(machine_id, &secrets).await?;
            }
            return Ok(secrets);
        }
        let secrets = MachineSecrets::generate()?;
        self.save_secrets(machine_id, &secrets).await?;
        Ok(secrets)
    }

    async fn save_secrets(&self, machine_id: &str, secrets: &MachineSecrets) -> ProviderResult<()> {
        self.api
            .put_object(
                &self.state_bucket(),
                &record::secrets_key(machine_id),
                secrets.to_json().into_bytes(),
            )
            .await
    }

    /// Keeps what the setup learnt about the machine (its WireGuard key and
    /// Sunshine's certificate) with its secrets, so every device can connect,
    /// not only the one that ran the setup. Saves only on a change.
    async fn learn_connection(&self, access: &MachineAccess) -> ProviderResult<()> {
        let Some(details) = self.setup.connection(access).await? else {
            return Ok(());
        };
        let mut secrets = self.secrets(&access.machine_id).await?;
        if secrets.learn(&details) {
            self.save_secrets(&access.machine_id, &secrets).await?;
        }
        Ok(())
    }

    async fn terraform_env(&self) -> ProviderResult<Vec<(String, String)>> {
        self.api.credential_env().await
    }

    async fn backend(&self, machine_id: &str) -> ProviderResult<Backend> {
        let storage = self.ensure_storage().await?;
        Ok(Backend {
            bucket: storage.state_bucket,
            key: record::state_key(machine_id),
            region: storage.region_id,
        })
    }

    /// Brings the machine to what its record asks for.
    async fn apply(&self, record: &MachineRecord) -> ProviderResult<()> {
        self.run_terraform(record, terraform::apply_args()).await
    }

    async fn run_terraform(
        &self,
        record: &MachineRecord,
        command: Vec<String>,
    ) -> ProviderResult<()> {
        let backend = self.backend(record.id()).await?;
        let _one_at_a_time = self.terraform_lock.lock().await;
        let dir = self.machine_dir(record.id());
        // A run that outlived the app may still be finishing; one that was
        // killed may have left its lock on the state.
        let dead_run = terraform::wait_for_earlier_run(&dir, EARLIER_RUN_WAIT).await?;
        self.clear_stale_lock(record.id(), dead_run.as_ref())
            .await?;
        terraform::prepare_workdir(&dir, &record.tfvars_json()).await?;
        let env = self.terraform_env().await?;
        self.terraform
            .run(&dir, &terraform::init_args(&backend), &env)
            .await?;
        self.terraform
            .run(&dir, &command, &env)
            .await
            .map_err(explain)?;
        // The local copy of the state is a cache; the bucket stays the
        // source of truth, so failing to refresh it doesn't fail the run.
        if let Ok(state) = self
            .terraform
            .run(&dir, &terraform::state_pull_args(), &env)
            .await
        {
            let _ = self.write_local(record.id(), "state.json", &state).await;
        }
        Ok(())
    }

    /// Removes the machine's state lock when the run that took it is dead.
    async fn clear_stale_lock(
        &self,
        machine_id: &str,
        dead_run: Option<&RunMarker>,
    ) -> ProviderResult<()> {
        let bucket = self.state_bucket();
        let key = record::lock_key(machine_id);
        let lock = self.api.get_object(&bucket, &key).await?;
        if lock
            .as_deref()
            .and_then(LockInfo::parse)
            .is_some_and(|lock| lock_is_stale(&lock, dead_run))
        {
            self.api.delete_object(&bucket, &key).await?;
        }
        Ok(())
    }

    /// The machine's instance, as its Terraform state names it.
    async fn instance_of(&self, record: &MachineRecord) -> ProviderResult<Option<Instance>> {
        let (instances, owned) = futures::try_join!(
            self.api.tagged_instances(&record.vars.region),
            self.owned_ids(record.id()),
        )?;
        Ok(pick_instance(
            &instances,
            record.id(),
            owned.as_ref(),
            false,
        ))
    }

    fn machine_from(
        &self,
        record: &MachineRecord,
        instance: Option<&Instance>,
        busy: bool,
    ) -> Machine {
        let type_name = instance.map_or(&record.vars.instance_type, |i| &i.type_name);
        let machine_type = self
            .machine_type(type_name)
            .unwrap_or_else(|_| MachineType {
                type_name: type_name.clone(),
                spec: MachineSpec {
                    vcpus: 0,
                    ram_gib: 0.0,
                    gpu: None,
                },
            });
        let state = match (instance, busy) {
            (Some(i), _) => i.state,
            // Being created.
            (None, true) => MachineState::Pending,
            // Terminated (a reclaimed spot machine) or never created (a
            // failed launch); destroy cleans up either.
            (None, false) => MachineState::Terminated,
        };
        let stopped_itself = !busy
            && !record.vars.spot
            && state == MachineState::Stopped
            && record.vars.instance_state == InstanceState::Running
            && settled(record);
        let library_kept = self.setup.enabled()
            && state == MachineState::Stopped
            && record.vars.instance_state == InstanceState::Stopped
            && record.vars.library_volume_enabled;
        Machine {
            id: record.id().into(),
            name: record.name.clone(),
            region_id: record.vars.region.clone(),
            machine_type,
            disk_gib: record.vars.root_volume_size,
            spot: record.vars.spot,
            state,
            busy,
            starting: busy && record.vars.instance_state == InstanceState::Running,
            stopped_itself,
            library_kept,
        }
    }

    async fn access(
        &self,
        record: &MachineRecord,
        instance: &Instance,
    ) -> ProviderResult<MachineAccess> {
        let password = match (
            self.api
                .password_data(&instance.region_id, &instance.id)
                .await?,
            self.key(record.id()).await?,
        ) {
            (Some(data), Some(key)) => Some(key.decrypt_password(&data)?),
            _ => None,
        };
        let storage = self.ensure_storage().await?;
        Ok(MachineAccess {
            machine_id: record.id().into(),
            region_id: instance.region_id.clone(),
            instance_id: instance.id.clone(),
            type_name: instance.type_name.clone(),
            public_ip: instance.public_ip.clone(),
            admin_password: password,
            wireguard: record.vars.wireguard_enabled,
            dir: self.machine_dir(record.id()),
            library_bucket: storage.library_bucket,
            library_region: storage.region_id,
            secrets: self.secrets(record.id()).await?,
        })
    }

    /// Like [`Self::access`], once Windows has made its password: a new
    /// machine takes a few minutes after its first boot.
    async fn access_with_password(
        &self,
        record: &MachineRecord,
        instance: &Instance,
    ) -> ProviderResult<MachineAccess> {
        let deadline = std::time::Instant::now() + self.password_wait;
        loop {
            let access = self.access(record, instance).await?;
            if access.admin_password.is_some() {
                return Ok(access);
            }
            if std::time::Instant::now() >= deadline {
                return Err(ProviderError::Cloud(format!(
                    "Windows on {} hasn't made its Administrator password after {} minutes",
                    record.name,
                    self.password_wait.as_secs() / 60
                )));
            }
            tokio::time::sleep(self.password_poll).await;
        }
    }

    /// This device's IP as /32 firewall rules: for Sunshine when WireGuard
    /// is off (`client_cidrs`), and for WinRM when the setup runs
    /// (`admin_cidrs`).
    async fn opened_to_me(&self, wireguard: bool) -> ProviderResult<(Vec<String>, Vec<String>)> {
        let setup = self.setup.enabled();
        if wireguard && !setup {
            return Ok((vec![], vec![]));
        }
        let me = format!("{}/32", self.ip.public_ip().await?);
        let client = if wireguard { vec![] } else { vec![me.clone()] };
        let admin = if setup { vec![me] } else { vec![] };
        Ok((client, admin))
    }

    /// Points the machine's firewall rules at this device, whose IP may have
    /// changed since the last start, or which may not be the device that
    /// started it. Applies only when they change.
    async fn open_to_me(&self, record: &mut MachineRecord) -> ProviderResult<()> {
        let (client, admin) = self.opened_to_me(record.vars.wireguard_enabled).await?;
        if record.vars.client_cidrs == client && record.vars.admin_cidrs == admin {
            return Ok(());
        }
        record.vars.client_cidrs = client;
        record.vars.admin_cidrs = admin;
        self.save_record(record).await?;
        self.apply(record).await
    }

    /// The zone a new machine goes in: for spot the one with the cheapest
    /// price right now, otherwise the module's choice (the first zone that
    /// sells the GPU type, so a later switch to GPU fits). Pinned before the
    /// first apply, so a run that dies halfway can't move the machine.
    async fn launch_zone(&self, request: &LaunchRequest) -> ProviderResult<String> {
        let gpu_type = catalog()
            .into_iter()
            .find(MachineType::is_gpu)
            .expect("the catalog has a GPU type")
            .type_name;
        let zone = if request.spot {
            self.spot_prices(&request.region_id, &request.type_name)
                .await?
                .into_iter()
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(zone, _)| zone)
        } else {
            self.api
                .offered_zones(&request.region_id, &gpu_type)
                .await?
                .into_iter()
                .min()
        };
        zone.ok_or_else(|| {
            ProviderError::Cloud(format!(
                "no zone in {} sells {gpu_type}",
                region_name(&request.region_id)
            ))
        })
    }

    async fn spot_prices(
        &self,
        region_id: &str,
        type_name: &str,
    ) -> ProviderResult<Vec<(String, f64)>> {
        self.caches
            .spot
            .get_or_try(
                (region_id.into(), type_name.into()),
                self.api.spot_prices(region_id, type_name),
            )
            .await
    }

    /// Runs after a launch, start or switch: the machine as it is now, set
    /// up (Ansible) once it's running. With `wait_for_library`, also waits
    /// for the game library restore to be over.
    async fn after_start(
        &self,
        record: &mut MachineRecord,
        wait_for_library: bool,
    ) -> ProviderResult<Machine> {
        let instance = self.instance_of(record).await?;
        if let Some(i) = instance
            .as_ref()
            .filter(|i| self.setup.enabled() && i.state == MachineState::Running)
        {
            let id = record.id().to_string();
            self.with_setup_marker(&id, async {
                let access = self.access_with_password(record, i).await?;
                // From here on the setup can put games on D:.
                if !record.setup_started {
                    record.setup_started = true;
                    self.save_record(record).await?;
                }
                self.setup.after_start(&access).await?;
                self.learn_connection(&access).await?;
                if wait_for_library {
                    self.setup.wait_for_library(&access).await?;
                }
                Ok(())
            })
            .await?;
        }
        Ok(self.machine_from(record, instance.as_ref(), false))
    }

    /// Runs `work` on the machine (the setup, or the upload before a stop)
    /// with a marker in the state bucket that tells other devices it's
    /// busy, since no Terraform lock says so meanwhile.
    async fn with_setup_marker<T>(
        &self,
        machine_id: &str,
        work: impl std::future::Future<Output = ProviderResult<T>>,
    ) -> ProviderResult<T> {
        let bucket = self.state_bucket();
        let key = record::setup_key(machine_id);
        let beat = |api: Arc<dyn AwsApi>, bucket: String, key: String| async move {
            // Best effort: a missed beat only lets the marker go stale.
            let now = terraform::unix_now().to_string();
            let _ = api.put_object(&bucket, &key, now.into_bytes()).await;
        };
        beat(self.api.clone(), bucket.clone(), key.clone()).await;
        let heartbeat = tokio::spawn({
            let (api, bucket, key) = (self.api.clone(), bucket.clone(), key.clone());
            async move {
                loop {
                    tokio::time::sleep(SETUP_HEARTBEAT).await;
                    beat(api.clone(), bucket.clone(), key.clone()).await;
                }
            }
        });
        let result = work.await;
        heartbeat.abort();
        let _ = heartbeat.await;
        let _ = self.api.delete_object(&bucket, &key).await;
        result
    }

    /// Stops the machine as [`CloudProvider::stop`] says.
    async fn stop_now(
        &self,
        mut record: MachineRecord,
        instance: Option<Instance>,
    ) -> ProviderResult<Machine> {
        // A failed upload doesn't block the stop; it only keeps D:.
        let saved = self.save_library(&mut record, instance.as_ref()).await;
        record.vars.instance_state = InstanceState::Stopped;
        // D: only goes away once its games are safe in S3 (one no setup has
        // started on has none). A machine that stopped itself saved them, or
        // not, before it did. When this upload failed, an older record
        // mustn't vouch for it: the upload may not have got as far as
        // deleting it.
        let empty = self.setup.enabled() && !record.setup_started;
        if empty || (saved.is_ok() && self.library_saved(&record, instance.as_ref()).await) {
            record.vars.library_volume_enabled = false;
        }
        self.save_record(&mut record).await?;
        self.apply(&record).await?;
        let instance = self.instance_of(&record).await?;
        Ok(self.machine_from(&record, instance.as_ref(), false))
    }

    /// Before a stop or a destroy: has a running machine upload its game
    /// library, recording the outcome for [`Self::library_saved`]. The
    /// error says why it couldn't.
    async fn save_library(
        &self,
        record: &mut MachineRecord,
        instance: Option<&Instance>,
    ) -> ProviderResult<()> {
        let Some(i) = instance.filter(|i| self.setup.enabled() && i.state == MachineState::Running)
        else {
            return Ok(());
        };
        if !record.setup_started {
            return Ok(());
        }
        // It runs with its library disk, whatever a stop that didn't get
        // through left in the record, so opening WinRM must keep both.
        record.vars.instance_state = InstanceState::Running;
        record.vars.library_volume_enabled = true;
        // An upload recorded earlier this boot, by a stop whose Terraform
        // run then failed, mustn't vouch for games installed since.
        self.api
            .delete_object(
                &library_bucket(&self.account_id),
                &record::final_sync_key(record.id()),
            )
            .await?;
        let id = record.id().to_string();
        self.with_setup_marker(&id, async {
            self.open_to_me(record).await?;
            let access = self.access(record, i).await?;
            self.setup.before_stop(&access).await
        })
        .await
    }

    /// Before a destroy: refuses when the machine's library disk may hold
    /// games the game library doesn't. `saved` is how the upload just now
    /// went.
    async fn check_games_saved(
        &self,
        record: &MachineRecord,
        instance: Option<&Instance>,
        saved: ProviderResult<()>,
    ) -> ProviderResult<()> {
        // D: outlives its instance (AWS taking back a spot machine), until
        // Terraform deletes it. Before any setup it holds nothing.
        let has_disk = record.vars.library_volume_enabled && record.setup_started;
        if !self.setup.enabled()
            || !has_disk
            || (saved.is_ok() && self.library_saved(record, instance).await)
        {
            return Ok(());
        }
        let name = &record.name;
        let why = match (saved, instance) {
            (Err(e), _) => format!("{name}'s games couldn't be saved to the game library ({e})."),
            // EC2 lists no ended instances, so there's no start to check
            // its last upload against.
            (Ok(()), None) => format!(
                "ReGa can't tell whether {name} saved its games to the game library before \
                 its instance ended."
            ),
            (Ok(()), Some(_)) => {
                format!("{name} hasn't saved its games to the game library since it last started.")
            }
        };
        Err(ProviderError::GamesNotSaved(format!(
            "{why} Destroying it loses any installed since its last upload."
        )))
    }

    /// Whether the machine's game library disk is safe to delete: the
    /// machine recorded an upload that worked and ended after its last
    /// start, whether the app asked for it or the idle stop did.
    async fn library_saved(&self, record: &MachineRecord, instance: Option<&Instance>) -> bool {
        let Some((instance, started)) = instance.and_then(|i| i.launched.map(|l| (i, l))) else {
            return false;
        };
        let key = record::final_sync_key(record.id());
        match self
            .api
            .get_object(&library_bucket(&self.account_id), &key)
            .await
        {
            Ok(Some(json)) => FinalSync::from_json(&json)
                .is_some_and(|f| f.vouches_for(record.id(), &instance.id, started)),
            _ => false,
        }
    }

    /// The CPU type a launch of `request` sets the machine up on first, if
    /// it does: for an on-demand GPU type, when asked and when it's sold in
    /// the region.
    async fn setup_type(
        &self,
        request: &LaunchRequest,
        machine_type: &MachineType,
    ) -> ProviderResult<Option<String>> {
        if !request.cpu_first || request.spot || !machine_type.is_gpu() || !self.setup.enabled() {
            return Ok(None);
        }
        let catalog = catalog();
        let Some(cpu) = roomy_cpu_type(&catalog) else {
            return Ok(None);
        };
        let sold = self.price(&request.region_id, &cpu.type_name).await?;
        Ok(sold.map(|_| cpu.type_name.clone()))
    }

    /// Makes a new bucket's settings readable: S3 can take a moment to show
    /// a bucket it just created.
    async fn new_bucket(&self, bucket: &str) -> ProviderResult<BucketInfo> {
        for attempt in 0..5 {
            if let Some(info) = self.api.bucket(bucket).await? {
                return Ok(info);
            }
            tokio::time::sleep(Duration::from_secs(1 << attempt)).await;
        }
        Err(ProviderError::Cloud(format!(
            "bucket {bucket} was created but can't be read yet; try again in a minute"
        )))
    }
}

/// The machine's instance among `instances`: the one its Terraform state
/// names. While a run is creating it (`busy`) and the state names none yet,
/// whichever live instance carries its tag.
fn pick_instance(
    instances: &[Instance],
    machine_id: &str,
    owned: Option<&HashSet<String>>,
    busy: bool,
) -> Option<Instance> {
    let mut mine = instances
        .iter()
        .filter(|i| i.machine_id.as_deref() == Some(machine_id));
    let named = mine
        .clone()
        .find(|i| owned.is_some_and(|ids| ids.contains(&i.id)));
    let names_one = owned.is_some_and(|ids| ids.iter().any(|id| id.starts_with("i-")));
    match named {
        Some(i) => Some(i.clone()),
        None if busy && !names_one => mine.find(|i| i.state != MachineState::Terminated).cloned(),
        None => None,
    }
}

/// Whether a state lock was left by a Terraform run that died: one this
/// computer started and that is gone (`dead_run`), or one older than any
/// run takes.
fn lock_is_stale(lock: &LockInfo, dead_run: Option<&RunMarker>) -> bool {
    stale_at(
        lock,
        dead_run,
        terraform::host_name().as_deref(),
        terraform::unix_now(),
    )
}

fn stale_at(lock: &LockInfo, dead_run: Option<&RunMarker>, host: Option<&str>, now: u64) -> bool {
    let Some(created) = unix_secs(&lock.created) else {
        return false;
    };
    if now.saturating_sub(created) > STALE_LOCK_SECS {
        return true;
    }
    // Terraform takes the lock right after it starts, or up to its lock
    // timeout later while another device holds it.
    dead_run.zip(host).is_some_and(|(run, host)| {
        lock.host() == Some(host)
            && (run.started.saturating_sub(60)..=run.started + 10 * 60).contains(&created)
    })
}

fn unix_secs(rfc3339: &str) -> Option<u64> {
    use aws_sdk_s3::primitives::{DateTime, DateTimeFormat};
    DateTime::from_str(rfc3339, DateTimeFormat::DateTime)
        .ok()
        .and_then(|t| u64::try_from(t.secs()).ok())
}

/// Adds what to do about errors only an AWS admin can fix: a service-linked
/// role the `rega-claude` policy can't create.
fn explain(err: ProviderError) -> ProviderError {
    let ProviderError::Cloud(message) = err else {
        return err;
    };
    let lower = message.to_lowercase();
    if ![
        "servicelinkedrole",
        "service-linked role",
        "service linked role",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        return ProviderError::Cloud(message);
    }
    let service = if lower.contains("alarm") || lower.contains("cloudwatch") {
        "events.amazonaws.com"
    } else if lower.contains("spot") {
        "spot.amazonaws.com"
    } else {
        "events.amazonaws.com"
    };
    ProviderError::Cloud(format!(
        "{message}. An AWS admin has to create a service-linked role once for this account: \
         aws iam create-service-linked-role --aws-service-name {service}"
    ))
}

#[async_trait]
impl CloudProvider for AwsProvider {
    fn id(&self) -> &str {
        "aws"
    }

    fn display_name(&self) -> &str {
        "AWS"
    }

    async fn ensure_storage(&self) -> ProviderResult<StorageLocation> {
        let mut done = self.storage.lock().await;
        if let Some(location) = done.as_ref() {
            return Ok(location.clone());
        }
        let location = StorageLocation {
            state_bucket: state_bucket(&self.account_id),
            library_bucket: library_bucket(&self.account_id),
            region_id: HOME_REGION.into(),
        };
        for (bucket, is_state) in [
            (&location.state_bucket, true),
            (&location.library_bucket, false),
        ] {
            let info = match self.api.bucket(bucket).await? {
                Some(info) => info,
                None => {
                    self.api.create_bucket(bucket, HOME_REGION).await?;
                    self.new_bucket(bucket).await?
                }
            };
            if is_state && !info.versioned {
                self.api.enable_versioning(bucket).await?;
            }
            // Versions of a destroyed machine's state and key would
            // otherwise stay forever.
            if is_state && !info.lifecycle {
                self.api.expire_old_versions(bucket).await?;
            }
            if !info.encrypted {
                self.api.enable_encryption(bucket).await?;
            }
            if !info.tagged {
                self.api.tag_bucket(bucket).await?;
            }
            // New buckets block public access by default. ReGa only checks
            // it: the IAM policy denies changing it, on purpose.
            if !info.public_access_blocked {
                return Err(ProviderError::Cloud(format!(
                    "bucket {bucket} doesn't block public access. Turn on all four Block \
                     Public Access settings for it in the S3 console, then try again."
                )));
            }
        }
        *done = Some(location.clone());
        Ok(location)
    }

    async fn regions(&self) -> ProviderResult<Vec<Region>> {
        let ids = self.enabled_regions().await?;
        let offered = join_all(ids.iter().map(|id| self.offered(id))).await;
        let mut regions: Vec<Region> = ids
            .iter()
            .zip(offered)
            // A region whose offerings can't be read stays listed, so the
            // region picker reports it as unreachable instead of hiding it.
            .filter(|(_, offered)| offered.as_ref().map_or(true, |o| !o.is_empty()))
            .map(|(id, _)| region(id, &region_name(id)))
            .collect();
        regions.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(regions)
    }

    async fn gpu_quota_vcpus(&self, region_id: &str, spot: bool) -> ProviderResult<u32> {
        let quota = self
            .caches
            .quotas
            .get_or_try(
                (region_id.into(), spot),
                self.api.gpu_quota(region_id, spot),
            )
            .await?;
        if quota == 0 {
            return Ok(0);
        }
        let in_use = self.api.gpu_vcpus_in_use(region_id, spot).await?;
        Ok(quota.saturating_sub(in_use))
    }

    fn machine_types(&self) -> Vec<MachineType> {
        catalog()
    }

    async fn price(&self, region_id: &str, type_name: &str) -> ProviderResult<Option<Price>> {
        let machine_type = self.machine_type(type_name)?;
        if !self
            .offered(region_id)
            .await?
            .iter()
            .any(|t| t == type_name)
        {
            return Ok(None);
        }
        let on_demand = self
            .caches
            .on_demand
            .get_or_try(
                (region_id.into(), type_name.into()),
                self.api.on_demand_price(region_id, type_name),
            )
            .await?;
        let Some(on_demand_per_hour) = on_demand else {
            return Ok(None);
        };
        // Spot only makes sense for GPU types: CPU machines must be resizable.
        let spot_per_hour = if machine_type.is_gpu() {
            self.spot_prices(region_id, type_name)
                .await?
                .into_iter()
                .map(|(_, p)| p)
                .min_by(f64::total_cmp)
        } else {
            None
        };
        Ok(Some(Price {
            on_demand_per_hour,
            spot_per_hour,
        }))
    }

    async fn library_copy_cost(&self, region_id: &str) -> ProviderResult<f64> {
        if region_id == HOME_REGION {
            return Ok(0.0);
        }
        let bytes = self
            .caches
            .library_bytes
            .get_or_try((), async {
                let objects = self
                    .api
                    .list_objects(&library_bucket(&self.account_id), "")
                    .await?;
                Ok::<u64, ProviderError>(objects.iter().map(|(_, size)| size).sum())
            })
            .await?;
        Ok(bytes as f64 / 1e9 * LIBRARY_COPY_USD_PER_GB)
    }

    async fn list_machines(&self) -> ProviderResult<Vec<Machine>> {
        let Snapshot {
            mut records,
            owned,
            busy,
        } = self.snapshot().await?;
        records.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id().cmp(b.id())));
        let regions: BTreeSet<&str> = records.iter().map(|r| r.vars.region.as_str()).collect();
        let instances: Vec<Instance> =
            try_join_all(regions.iter().map(|r| self.api.tagged_instances(r)))
                .await?
                .into_iter()
                .flatten()
                .collect();
        Ok(records
            .iter()
            .map(|r| {
                let busy = busy.contains(r.id());
                let instance = pick_instance(&instances, r.id(), owned.get(r.id()), busy);
                self.machine_from(r, instance.as_ref(), busy)
            })
            .collect())
    }

    async fn launch(&self, request: &LaunchRequest) -> ProviderResult<Machine> {
        let machine_type = self.machine_type(&request.type_name)?;
        if request.spot && !machine_type.is_gpu() {
            return Err(ProviderError::SpotNeedsGpu);
        }
        if !(MIN_DISK_GIB..=MAX_DISK_GIB).contains(&request.disk_gib) {
            return Err(ProviderError::InvalidState(format!(
                "the system disk must be {MIN_DISK_GIB} to {MAX_DISK_GIB} GB"
            )));
        }
        let price = self.price(&request.region_id, &request.type_name).await?;
        if price.and_then(|p| p.per_hour(request.spot)).is_none() {
            return Err(ProviderError::Cloud(format!(
                "{} isn't sold {} in {}",
                request.type_name,
                if request.spot { "as spot" } else { "on demand" },
                region_name(&request.region_id)
            )));
        }
        self.check_gpu_quota(&request.region_id, &machine_type, request.spot)
            .await?;
        self.setup.ready()?;
        let setup_type = self.setup_type(request, &machine_type).await?;
        let _booting = self.boot()?;
        self.check_none_running(None).await?;

        let machine_id = record::new_machine_id(&request.name);
        let _acting = self.begin(&machine_id)?;
        let key = MachineKey::generate()?;
        let (client_cidrs, admin_cidrs) = self.opened_to_me(request.wireguard).await?;
        let mut record = MachineRecord::new(
            request.name.clone(),
            MachineVars {
                region: request.region_id.clone(),
                machine_id: machine_id.clone(),
                instance_type: setup_type
                    .clone()
                    .unwrap_or_else(|| request.type_name.clone()),
                spot: request.spot,
                instance_state: InstanceState::Running,
                availability_zone: Some(self.launch_zone(request).await?),
                public_key: key.openssh_public(&format!("rega-{machine_id}")),
                root_volume_size: request.disk_gib,
                library_volume_enabled: true,
                wireguard_enabled: request.wireguard,
                client_cidrs,
                admin_cidrs,
            },
        );

        // Save the key and the record before anything exists in EC2, so a
        // failed or interrupted launch leaves a machine to destroy, not
        // orphans.
        self.ensure_storage().await?;
        let pem = key.to_pem();
        self.api
            .put_object(
                &self.state_bucket(),
                &record::key_key(&machine_id),
                pem.clone().into_bytes(),
            )
            .await?;
        self.write_local(&machine_id, "key.pem", &pem).await?;
        self.secrets(&machine_id).await?;
        self.save_record(&mut record).await?;

        self.apply(&record).await?;
        let machine = self.after_start(&mut record, setup_type.is_some()).await?;
        if setup_type.is_none() {
            return Ok(machine);
        }
        // Set up, with the games on D:: now the GPU. The zone sells it, and
        // D: stays through the restart.
        record.vars.instance_type = request.type_name.clone();
        self.save_record(&mut record).await?;
        self.apply(&record).await?;
        self.after_start(&mut record, false).await
    }

    async fn resize(&self, machine_id: &str, type_name: &str) -> ProviderResult<Machine> {
        let machine_type = self.machine_type(type_name)?;
        let _acting = self.begin(machine_id)?;
        let mut record = self.record(machine_id).await?;
        if record.vars.spot {
            return Err(ProviderError::SpotResize);
        }
        let instance = self.instance_of(&record).await?;
        let running = match instance.as_ref().map(|i| i.state) {
            Some(MachineState::Running) => true,
            Some(MachineState::Stopped) => false,
            _ => {
                return Err(ProviderError::InvalidState(
                    "wait until the machine is running or stopped".into(),
                ))
            }
        };
        // AWS boots the machine after a resize (for a stopped one, Terraform
        // stops it again), so the new type needs quota, and no other machine
        // up.
        self.check_gpu_quota(&record.vars.region, &machine_type, false)
            .await?;
        if running {
            self.setup.ready()?;
        }
        let _booting = self.boot()?;
        self.check_none_running(Some(machine_id)).await?;
        record.vars.instance_type = type_name.into();
        if running {
            // Restarts as the new type with D: kept, so it's set up again
            // (the GPU part on a GPU type). The record may still hold what a
            // stop that didn't get through asked for.
            let (client, admin) = self.opened_to_me(record.vars.wireguard_enabled).await?;
            record.vars.client_cidrs = client;
            record.vars.admin_cidrs = admin;
            record.vars.instance_state = InstanceState::Running;
            record.vars.library_volume_enabled = true;
            self.save_record(&mut record).await?;
            self.apply(&record).await?;
            return self.after_start(&mut record, false).await;
        }
        // One that stopped itself loses D: as a stop would, once its games
        // are saved.
        if record.vars.instance_state == InstanceState::Running
            && settled(&record)
            && self.library_saved(&record, instance.as_ref()).await
        {
            record.vars.library_volume_enabled = false;
        }
        record.vars.instance_state = InstanceState::Stopped;
        self.save_record(&mut record).await?;
        self.apply(&record).await?;
        let instance = self.instance_of(&record).await?;
        Ok(self.machine_from(&record, instance.as_ref(), false))
    }

    async fn start(&self, machine_id: &str) -> ProviderResult<Machine> {
        let _acting = self.begin(machine_id)?;
        let mut record = self.record(machine_id).await?;
        let instance = self.instance_of(&record).await?;
        let up = instance
            .as_ref()
            .is_some_and(|i| matches!(i.state, MachineState::Pending | MachineState::Running));
        if !up {
            let machine_type = self.machine_type(&record.vars.instance_type)?;
            self.check_gpu_quota(&record.vars.region, &machine_type, record.vars.spot)
                .await?;
        }
        self.setup.ready()?;
        let _booting = self.boot()?;
        self.check_none_running(Some(machine_id)).await?;
        let (client, admin) = self.opened_to_me(record.vars.wireguard_enabled).await?;
        record.vars.instance_state = InstanceState::Running;
        record.vars.library_volume_enabled = true;
        record.vars.client_cidrs = client;
        record.vars.admin_cidrs = admin;
        self.save_record(&mut record).await?;
        self.apply(&record).await?;
        self.after_start(&mut record, false).await
    }

    async fn stop(&self, machine_id: &str) -> ProviderResult<Machine> {
        let _acting = self.begin(machine_id)?;
        let record = self.record(machine_id).await?;
        if record.vars.spot {
            return Err(ProviderError::InvalidState(
                "spot machines can't be stopped; destroy it instead".into(),
            ));
        }
        let instance = self.instance_of(&record).await?;
        self.stop_now(record, instance).await
    }

    async fn finish_stop(&self, machine_id: &str) -> ProviderResult<Machine> {
        // Before taking this device's own claim, which counts as busy.
        let busy = self.snapshot().await?.busy.contains(machine_id);
        let _acting = self.begin(machine_id)?;
        let record = self.record(machine_id).await?;
        let instance = self.instance_of(&record).await?;
        let machine = self.machine_from(&record, instance.as_ref(), busy);
        if !machine.stopped_itself {
            return Ok(machine);
        }
        self.stop_now(record, instance).await
    }

    async fn destroy(&self, machine_id: &str, even_if_unsaved: bool) -> ProviderResult<()> {
        let _acting = self.begin(machine_id)?;
        let mut record = self.record(machine_id).await?;
        let instance = self.instance_of(&record).await?;
        // Games installed since the last upload would go with D:. Going
        // ahead anyway means the upload already failed; it isn't retried.
        if !even_if_unsaved {
            let saved = self.save_library(&mut record, instance.as_ref()).await;
            self.check_games_saved(&record, instance.as_ref(), saved)
                .await?;
        }
        // The AWS provider cancels the spot request before terminating, and
        // the IAM policy doesn't allow that; terminating first leaves
        // Terraform nothing to cancel.
        if record.vars.spot {
            if let Some(i) = instance.filter(|i| i.state != MachineState::Terminated) {
                self.api
                    .delete_resource(&Orphan {
                        kind: ResourceKind::Instance,
                        id: i.id,
                        region_id: i.region_id,
                        machine_id: Some(machine_id.into()),
                        name: None,
                    })
                    .await?;
            }
        }
        self.run_terraform(&record, terraform::destroy_args())
            .await?;
        let bucket = self.state_bucket();
        for key in record::all_keys(machine_id) {
            self.api.delete_object(&bucket, &key).await?;
        }
        self.api
            .delete_object(
                &library_bucket(&self.account_id),
                &record::final_sync_key(machine_id),
            )
            .await?;
        match tokio::fs::remove_dir_all(self.machine_dir(machine_id)).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(io_error("remove the machine folder", e))
            }
            _ => Ok(()),
        }
    }

    async fn stream_target(&self, machine_id: &str) -> ProviderResult<StreamTarget> {
        // Before taking this device's own claim, which counts as busy.
        let busy = self.snapshot().await?.busy.contains(machine_id);
        let _acting = self.begin(machine_id)?;
        let mut record = self.record(machine_id).await?;
        let instance = self.instance_of(&record).await?;
        let name = &record.name;
        let Some(public_ip) = instance
            .filter(|i| i.state == MachineState::Running)
            .and_then(|i| i.public_ip)
        else {
            return Err(ProviderError::InvalidState(format!(
                "{name} isn't running; start it first"
            )));
        };
        if busy {
            return Err(ProviderError::InvalidState(format!(
                "{name} is busy with a change; wait for it to finish"
            )));
        }
        let secrets = self.secrets(machine_id).await?;
        let wireguard = record.vars.wireguard_enabled;
        let set_up_first = || {
            ProviderError::InvalidState(format!(
                "{name} hasn't been set up for streaming yet. Start it again to set it up"
            ))
        };
        let wireguard = if wireguard {
            let server_public_key = secrets
                .wireguard_server_public_key
                .clone()
                .ok_or_else(set_up_first)?;
            let ip: std::net::IpAddr = public_ip.parse().map_err(|_| {
                ProviderError::Cloud(format!("{name} has an odd public IP: {public_ip}"))
            })?;
            Some(WireGuardPeer {
                endpoint: (ip, ansible::WIREGUARD_PORT).into(),
                server_public_key,
                client_private_key: secrets.wireguard_private_key.clone(),
                client_address: stream::CLIENT_ADDRESS,
                server_address: stream::SERVER_ADDRESS,
                mtu: stream::TUNNEL_MTU,
            })
        } else {
            // The password crosses the internet, so only to the certificate
            // the setup saw.
            if secrets.sunshine_cert_sha256.is_none() {
                return Err(set_up_first());
            }
            self.open_to_me(&mut record).await?;
            None
        };
        Ok(StreamTarget {
            machine_id: machine_id.into(),
            public_ip,
            sunshine_user: stream::SUNSHINE_USER.into(),
            sunshine_password: secrets.sunshine_password,
            sunshine_cert_sha256: secrets.sunshine_cert_sha256,
            wireguard,
        })
    }

    async fn orphans(&self) -> ProviderResult<Vec<Orphan>> {
        let snapshot = self.snapshot().await?;
        let owned: HashSet<&String> = snapshot.owned.values().flatten().collect();
        let regions = self.enabled_regions().await?;
        let mut orphans: Vec<Orphan> =
            try_join_all(regions.iter().map(|r| self.api.tagged_resources(r)))
                .await?
                .into_iter()
                .flatten()
                // Owned by a state, or by a machine whose run may still be
                // creating it.
                .filter(|o| !owned.contains(&o.id))
                .filter(|o| {
                    o.machine_id
                        .as_ref()
                        .is_none_or(|m| !snapshot.busy.contains(m))
                })
                .collect();
        orphans.sort_by(|a, b| (a.kind, &a.region_id, &a.id).cmp(&(b.kind, &b.region_id, &b.id)));
        Ok(orphans)
    }

    async fn delete_orphans(&self, orphans: &[Orphan]) -> ProviderResult<()> {
        // Only what is still an orphan: a machine may have claimed it since.
        let current: HashSet<Orphan> = self.orphans().await?.into_iter().collect();
        let mut doomed: Vec<&Orphan> = orphans.iter().filter(|o| current.contains(*o)).collect();
        doomed.sort_by_key(|o| o.kind);
        // One that can't go yet doesn't keep the others.
        let mut failures = Vec::new();
        for orphan in doomed {
            if let Err(e) = self.api.delete_resource(orphan).await {
                failures.push(format!("{}: {e}", orphan.id));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(ProviderError::Cloud(format!(
                "couldn't delete {}",
                failures.join("; ")
            )))
        }
    }
}

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
