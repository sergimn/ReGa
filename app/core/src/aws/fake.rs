//! An in-memory AWS account and a Terraform that acts on it, for testing the
//! provider's logic without credentials.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::api::{AwsApi, BucketInfo, Instance};
use super::keys::MachineKey;
use super::record::{key_key, state_key, InstanceState, MachineVars};
use super::{state_bucket, IpLookup};
use crate::model::{MachineState, Orphan, ResourceKind};
use crate::provider::{ProviderError, ProviderResult};
use crate::terraform::{Terraform, VARS_FILE};

pub const ACCOUNT: &str = "123456789012";
pub const MY_IP: &str = "203.0.113.7";

#[derive(Default)]
pub struct Account {
    pub regions: Vec<String>,
    pub offered: HashMap<String, Vec<String>>,
    /// Zones selling the GPU type, per region.
    pub zones: HashMap<String, Vec<String>>,
    pub quotas: HashMap<(String, bool), u32>,
    pub in_use: HashMap<(String, bool), u32>,
    pub on_demand: HashMap<(String, String), f64>,
    pub spot: HashMap<(String, String), Vec<(String, f64)>>,
    pub instances: Vec<Instance>,
    /// Tagged resources other than instances.
    pub resources: Vec<Orphan>,
    pub buckets: HashMap<String, BucketInfo>,
    /// New buckets come out like this (AWS blocks public access by default).
    pub new_bucket: BucketInfo,
    pub objects: BTreeMap<(String, String), Vec<u8>>,
    pub password_data: HashMap<String, String>,
    /// Every call, in order, such as `put_object machines/x.json`.
    pub calls: Vec<String>,
    /// Calls about this region fail.
    pub broken_region: Option<String>,
    /// Deleting this resource fails.
    pub undeletable: Option<String>,
    /// A new bucket stays invisible for this many reads.
    pub slow_new_buckets: u32,
    /// Windows makes this Administrator password once its instance has
    /// been asked for its password this many times (0: right away).
    pub windows_password: Option<(String, u32)>,
    /// What the IP lookup answers, when not [`MY_IP`].
    pub my_ip: Option<String>,
    /// Unix seconds; moves on with every start, which sets an instance's
    /// launch time.
    pub clock: u64,
    next_instance: u32,
}

impl Account {
    /// Two regions: Frankfurt with GPU quota and Stockholm without.
    pub fn typical() -> Self {
        let all: Vec<String> = super::catalog().into_iter().map(|t| t.type_name).collect();
        let mut a = Self {
            regions: vec!["eu-central-1".into(), "eu-north-1".into()],
            new_bucket: BucketInfo {
                public_access_blocked: true,
                ..BucketInfo::default()
            },
            // 2026-10-11T10:00:00Z
            clock: 1_791_712_800,
            ..Self::default()
        };
        for r in ["eu-central-1", "eu-north-1"] {
            a.offered.insert(r.into(), all.clone());
            a.zones
                .insert(r.into(), vec![format!("{r}b"), format!("{r}a")]);
            a.on_demand.insert((r.into(), "g4dn.xlarge".into()), 0.84);
            a.on_demand.insert((r.into(), "t3.medium".into()), 0.06);
            a.spot.insert(
                (r.into(), "g4dn.xlarge".into()),
                vec![(format!("{r}b"), 0.31), (format!("{r}a"), 0.30)],
            );
        }
        a.quotas.insert(("eu-central-1".into(), false), 8);
        a.quotas.insert(("eu-central-1".into(), true), 4);
        a
    }

    pub fn object(&self, bucket: &str, key: &str) -> Option<&Vec<u8>> {
        self.objects.get(&(bucket.to_string(), key.to_string()))
    }

    fn check(&self, region: &str) -> ProviderResult<()> {
        if self.broken_region.as_deref() == Some(region) {
            return Err(ProviderError::Cloud(format!("{region} is down")));
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct FakeAws(pub Arc<Mutex<Account>>);

impl FakeAws {
    pub fn new(account: Account) -> Self {
        Self(Arc::new(Mutex::new(account)))
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Account) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }

    fn call(&self, what: String) {
        self.with(|a| a.calls.push(what));
    }
}

#[async_trait]
impl AwsApi for FakeAws {
    async fn regions(&self) -> ProviderResult<Vec<String>> {
        self.call("regions".into());
        Ok(self.with(|a| a.regions.clone()))
    }

    async fn offered_types(&self, region: &str, types: &[String]) -> ProviderResult<Vec<String>> {
        self.call(format!("offered_types {region}"));
        self.with(|a| {
            a.check(region)?;
            Ok(a.offered
                .get(region)
                .into_iter()
                .flatten()
                .filter(|t| types.contains(t))
                .cloned()
                .collect())
        })
    }

    async fn offered_zones(&self, region: &str, _type_name: &str) -> ProviderResult<Vec<String>> {
        self.with(|a| {
            a.check(region)?;
            let mut zones = a.zones.get(region).cloned().unwrap_or_default();
            zones.sort();
            Ok(zones)
        })
    }

    async fn gpu_quota(&self, region: &str, spot: bool) -> ProviderResult<u32> {
        self.call(format!("gpu_quota {region} {spot}"));
        self.with(|a| {
            a.check(region)?;
            Ok(*a.quotas.get(&(region.into(), spot)).unwrap_or(&0))
        })
    }

    async fn gpu_vcpus_in_use(&self, region: &str, spot: bool) -> ProviderResult<u32> {
        self.call(format!("gpu_vcpus_in_use {region} {spot}"));
        self.with(|a| {
            let running: u32 = a
                .instances
                .iter()
                .filter(|i| i.region_id == region && i.spot == spot && i.type_name.starts_with('g'))
                .filter(|i| matches!(i.state, MachineState::Pending | MachineState::Running))
                .map(|_| 4)
                .sum();
            Ok(running + a.in_use.get(&(region.into(), spot)).unwrap_or(&0))
        })
    }

    async fn on_demand_price(&self, region: &str, type_name: &str) -> ProviderResult<Option<f64>> {
        self.call(format!("on_demand_price {region} {type_name}"));
        self.with(|a| {
            a.check(region)?;
            Ok(a.on_demand.get(&(region.into(), type_name.into())).copied())
        })
    }

    async fn spot_prices(
        &self,
        region: &str,
        type_name: &str,
    ) -> ProviderResult<Vec<(String, f64)>> {
        self.call(format!("spot_prices {region} {type_name}"));
        self.with(|a| {
            Ok(a.spot
                .get(&(region.into(), type_name.into()))
                .cloned()
                .unwrap_or_default())
        })
    }

    async fn tagged_instances(&self, region: &str) -> ProviderResult<Vec<Instance>> {
        self.with(|a| {
            a.check(region)?;
            Ok(a.instances
                .iter()
                .filter(|i| i.region_id == region && i.state != MachineState::Terminated)
                .cloned()
                .collect())
        })
    }

    async fn password_data(
        &self,
        _region: &str,
        instance_id: &str,
    ) -> ProviderResult<Option<String>> {
        self.call(format!("password_data {instance_id}"));
        Ok(self.with(|a| {
            let asked = a
                .calls
                .iter()
                .filter(|c| *c == &format!("password_data {instance_id}"))
                .count() as u32;
            if let (None, Some((password, after))) =
                (a.password_data.get(instance_id), a.windows_password.clone())
            {
                if asked > after {
                    let machine = a
                        .instances
                        .iter()
                        .find(|i| i.id == instance_id)
                        .and_then(|i| i.machine_id.clone())
                        .unwrap();
                    let pem = a
                        .object(&state_bucket(ACCOUNT), &key_key(&machine))
                        .cloned()
                        .unwrap();
                    let key = MachineKey::from_pem(&String::from_utf8(pem).unwrap()).unwrap();
                    a.password_data
                        .insert(instance_id.into(), encrypt_password(&key, &password));
                }
            }
            a.password_data.get(instance_id).cloned()
        }))
    }

    async fn tagged_resources(&self, region: &str) -> ProviderResult<Vec<Orphan>> {
        let instances = self.tagged_instances(region).await?;
        self.with(|a| {
            let mut out: Vec<Orphan> = instances
                .into_iter()
                .map(|i| Orphan {
                    kind: ResourceKind::Instance,
                    id: i.id,
                    region_id: i.region_id,
                    machine_id: i.machine_id,
                    name: None,
                })
                .collect();
            out.extend(
                a.resources
                    .iter()
                    .filter(|r| r.region_id == region)
                    .cloned(),
            );
            Ok(out)
        })
    }

    async fn delete_resource(&self, resource: &Orphan) -> ProviderResult<()> {
        self.call(format!("delete {:?} {}", resource.kind, resource.id));
        self.with(|a| {
            if a.undeletable.as_deref() == Some(resource.id.as_str()) {
                return Err(ProviderError::Cloud("DependencyViolation".into()));
            }
            match resource.kind {
                ResourceKind::Instance => {
                    for i in a.instances.iter_mut().filter(|i| i.id == resource.id) {
                        i.state = MachineState::Terminated;
                    }
                }
                _ => a.resources.retain(|r| r != resource),
            }
            Ok(())
        })
    }

    async fn bucket(&self, name: &str) -> ProviderResult<Option<BucketInfo>> {
        self.call(format!("bucket {name}"));
        Ok(self.with(|a| {
            let info = a.buckets.get(name).copied();
            if info.is_some()
                && a.slow_new_buckets > 0
                && a.calls.iter().any(|c| c.starts_with("create_bucket"))
            {
                a.slow_new_buckets -= 1;
                return None;
            }
            info
        }))
    }

    async fn create_bucket(&self, name: &str, region: &str) -> ProviderResult<()> {
        self.call(format!("create_bucket {name} {region}"));
        self.with(|a| {
            let info = a.new_bucket;
            a.buckets.entry(name.into()).or_insert(info);
        });
        Ok(())
    }

    async fn enable_versioning(&self, name: &str) -> ProviderResult<()> {
        self.call(format!("enable_versioning {name}"));
        self.with(|a| a.buckets.get_mut(name).unwrap().versioned = true);
        Ok(())
    }

    async fn enable_encryption(&self, name: &str) -> ProviderResult<()> {
        self.call(format!("enable_encryption {name}"));
        self.with(|a| a.buckets.get_mut(name).unwrap().encrypted = true);
        Ok(())
    }

    async fn tag_bucket(&self, name: &str) -> ProviderResult<()> {
        self.call(format!("tag_bucket {name}"));
        self.with(|a| a.buckets.get_mut(name).unwrap().tagged = true);
        Ok(())
    }

    async fn expire_old_versions(&self, name: &str) -> ProviderResult<()> {
        self.call(format!("expire_old_versions {name}"));
        self.with(|a| a.buckets.get_mut(name).unwrap().lifecycle = true);
        Ok(())
    }

    async fn list_objects(&self, bucket: &str, prefix: &str) -> ProviderResult<Vec<(String, u64)>> {
        Ok(self.with(|a| {
            a.objects
                .iter()
                .filter(|((b, k), _)| b == bucket && k.starts_with(prefix))
                .map(|((_, k), v)| (k.clone(), v.len() as u64))
                .collect()
        }))
    }

    async fn get_object(&self, bucket: &str, key: &str) -> ProviderResult<Option<Vec<u8>>> {
        Ok(self.with(|a| a.object(bucket, key).cloned()))
    }

    async fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>) -> ProviderResult<()> {
        self.call(format!("put_object {key}"));
        self.with(|a| a.objects.insert((bucket.into(), key.into()), body));
        Ok(())
    }

    async fn delete_object(&self, bucket: &str, key: &str) -> ProviderResult<()> {
        self.call(format!("delete_object {key}"));
        self.with(|a| a.objects.remove(&(bucket.to_string(), key.to_string())));
        Ok(())
    }

    async fn credential_env(&self) -> ProviderResult<Vec<(String, String)>> {
        Ok(vec![("AWS_ACCESS_KEY_ID".into(), "AKIAFAKE".into())])
    }
}

/// Applies the variables it finds to the fake account, like the module would,
/// and keeps a state in the bucket naming what it made.
pub struct FakeTerraform {
    pub aws: FakeAws,
    /// `apply` fails with this message when set.
    pub fail_apply: Mutex<Option<String>>,
    /// Per machine: the instance, root volume and security group in its state.
    owned: Mutex<HashMap<String, (String, String, String)>>,
}

impl FakeTerraform {
    pub fn new(aws: FakeAws) -> Self {
        Self {
            aws,
            fail_apply: Mutex::new(None),
            owned: Mutex::new(HashMap::new()),
        }
    }

    fn save_state(&self, machine_id: &str) {
        let owned = self.owned.lock().unwrap().get(machine_id).cloned();
        let resources = match owned {
            Some((instance, volume, group)) => format!(
                r#"[{{"mode": "managed", "type": "aws_instance", "instances": [{{"attributes":
                    {{"id": "{instance}", "root_block_device": [{{"volume_id": "{volume}"}}]}}}}]}},
                   {{"mode": "managed", "type": "aws_security_group", "instances": [{{"attributes":
                    {{"id": "{group}"}}}}]}}]"#
            ),
            None => "[]".into(),
        };
        self.aws.with(|a| {
            a.objects.insert(
                (state_bucket(ACCOUNT), state_key(machine_id)),
                format!(r#"{{"version": 4, "resources": {resources}}}"#).into_bytes(),
            )
        });
    }
}

fn read_vars(dir: &Path) -> MachineVars {
    serde_json::from_str(&std::fs::read_to_string(dir.join(VARS_FILE)).unwrap()).unwrap()
}

#[async_trait]
impl Terraform for FakeTerraform {
    async fn run(
        &self,
        dir: &Path,
        args: &[String],
        env: &[(String, String)],
    ) -> ProviderResult<String> {
        assert!(env.iter().any(|(k, _)| k == "AWS_ACCESS_KEY_ID"));
        let vars = read_vars(dir);
        let id = vars.machine_id.clone();
        let verb = args.first().cloned().unwrap_or_default();
        self.aws.call(format!("terraform {verb} {id}"));
        match verb.as_str() {
            "apply" => {
                if let Some(e) = self.fail_apply.lock().unwrap().clone() {
                    return Err(ProviderError::Cloud(e));
                }
                let owned = self.owned.lock().unwrap().get(&id).cloned();
                let made = self.aws.with(|a| {
                    let state = match vars.instance_state {
                        InstanceState::Running => MachineState::Running,
                        InstanceState::Stopped => MachineState::Stopped,
                    };
                    a.clock += 600;
                    let now = a.clock;
                    let live = owned.as_ref().and_then(|(instance, _, _)| {
                        a.instances
                            .iter_mut()
                            .find(|i| &i.id == instance && i.state != MachineState::Terminated)
                    });
                    if let Some(i) = live {
                        // A resize restarts it, as does a start.
                        let restarted =
                            i.type_name != vars.instance_type || i.state != MachineState::Running;
                        if state == MachineState::Running && restarted {
                            i.launched = Some(now);
                        }
                        i.type_name = vars.instance_type.clone();
                        i.state = state;
                        return owned;
                    }
                    a.next_instance += 1;
                    let n = a.next_instance;
                    a.instances.push(Instance {
                        id: format!("i-{n}"),
                        region_id: vars.region.clone(),
                        machine_id: Some(id.clone()),
                        type_name: vars.instance_type.clone(),
                        state,
                        spot: vars.spot,
                        public_ip: Some("198.51.100.1".into()),
                        launched: Some(now),
                    });
                    for (kind, rid) in [
                        (ResourceKind::Volume, format!("vol-{n}")),
                        (ResourceKind::SecurityGroup, format!("sg-{n}")),
                    ] {
                        a.resources.push(Orphan {
                            kind,
                            id: rid,
                            region_id: vars.region.clone(),
                            machine_id: Some(id.clone()),
                            name: None,
                        });
                    }
                    Some((format!("i-{n}"), format!("vol-{n}"), format!("sg-{n}")))
                });
                self.owned.lock().unwrap().insert(id.clone(), made.unwrap());
                self.save_state(&id);
                Ok(String::new())
            }
            "destroy" => {
                let owned = self.owned.lock().unwrap().get(&id).cloned();
                if let Some((instance, volume, group)) = owned {
                    self.aws.with(|a| {
                        if a.instances.iter().any(|i| {
                            i.id == instance && i.spot && i.state != MachineState::Terminated
                        }) {
                            // What the real AWS provider would do: cancel the
                            // spot request, which the IAM policy denies.
                            return Err(ProviderError::Cloud(
                                "CancelSpotInstanceRequests denied".into(),
                            ));
                        }
                        for i in a.instances.iter_mut().filter(|i| i.id == instance) {
                            i.state = MachineState::Terminated;
                        }
                        a.resources.retain(|r| r.id != volume && r.id != group);
                        Ok(())
                    })?;
                }
                self.owned.lock().unwrap().remove(&id);
                self.save_state(&id);
                Ok(String::new())
            }
            _ => Ok("{}".into()),
        }
    }
}

/// Answers [`MY_IP`], or the account's `my_ip` when set.
pub struct FixedIp(pub FakeAws);

#[async_trait]
impl IpLookup for FixedIp {
    async fn public_ip(&self) -> ProviderResult<String> {
        Ok(self
            .0
            .with(|a| a.my_ip.clone())
            .unwrap_or_else(|| MY_IP.into()))
    }
}

/// `password` encrypted with `key`, as EC2 hands out a Windows password.
pub fn encrypt_password(key: &MachineKey, password: &str) -> String {
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
