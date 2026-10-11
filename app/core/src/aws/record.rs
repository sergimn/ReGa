//! What the app keeps per machine next to its Terraform state, so any device
//! signed in to the account can drive the machine.
//!
//! Layout of the state bucket:
//!
//! ```text
//! machines/<id>.tfstate   Terraform state (S3 backend, with a .tflock while locked)
//! machines/<id>.json      MachineRecord: the name and the Terraform variables
//! machines/<id>.key.pem   the machine's RSA key, for its Windows password
//! machines/<id>.secrets.json  passwords the setup gives the machine
//! machines/<id>.setup     while a device sets the machine up: when it last
//!                         said it still is (Unix seconds)
//! ```

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{ProviderError, ProviderResult};

pub const PREFIX: &str = "machines/";
const RECORD_VERSION: u32 = 1;

pub fn state_key(machine_id: &str) -> String {
    format!("{PREFIX}{machine_id}.tfstate")
}

pub fn record_key(machine_id: &str) -> String {
    format!("{PREFIX}{machine_id}.json")
}

pub fn key_key(machine_id: &str) -> String {
    format!("{PREFIX}{machine_id}.key.pem")
}

/// The passwords the setup gives the machine ([`crate::setup::MachineSecrets`]).
pub fn secrets_key(machine_id: &str) -> String {
    format!("{PREFIX}{machine_id}.secrets.json")
}

/// Kept fresh while a device runs the setup on the machine, so other
/// devices see it as busy although no Terraform run holds its lock.
pub fn setup_key(machine_id: &str) -> String {
    format!("{PREFIX}{machine_id}.setup")
}

/// Where the machine says whether its last upload of the game library
/// worked, in the library bucket (`roles/library_agent` in
/// `infra/ansible`).
pub fn final_sync_key(machine_id: &str) -> String {
    format!("rega/machines/{machine_id}/final-sync.json")
}

/// Terraform's lock on the state (S3 backend with `use_lockfile`).
pub fn lock_key(machine_id: &str) -> String {
    format!("{}.tflock", state_key(machine_id))
}

/// Every object the bucket may hold for a machine, for clean-up.
pub fn all_keys(machine_id: &str) -> Vec<String> {
    vec![
        state_key(machine_id),
        lock_key(machine_id),
        record_key(machine_id),
        key_key(machine_id),
        secrets_key(machine_id),
        setup_key(machine_id),
    ]
}

/// The machine id named by a state-bucket object, if it is one of ours.
pub fn machine_id_of(key: &str) -> Option<&str> {
    let name = key.strip_prefix(PREFIX)?;
    [
        ".tfstate.tflock",
        ".tfstate",
        ".secrets.json",
        ".json",
        ".key.pem",
        ".setup",
    ]
    .iter()
    .find_map(|ext| name.strip_suffix(ext))
    .filter(|id| is_machine_id(id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    Running,
    Stopped,
}

/// The module's input variables (`infra/terraform/aws/variables.tf`). Only
/// what the app sets; the rest keep their defaults.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineVars {
    pub region: String,
    pub machine_id: String,
    pub instance_type: String,
    pub spot: bool,
    pub instance_state: InstanceState,
    /// Pinned from the first apply's output so the machine never moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<String>,
    pub public_key: String,
    pub root_volume_size: u32,
    pub library_volume_enabled: bool,
    pub wireguard_enabled: bool,
    pub client_cidrs: Vec<String>,
    pub admin_cidrs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub version: u32,
    /// The name the user gave the machine.
    pub name: String,
    pub vars: MachineVars,
    /// When a device last saved it, in Unix seconds; 0 when unknown.
    #[serde(default)]
    pub saved_at: u64,
    /// Whether a setup has ever started on the machine. Until one has,
    /// nothing can have put games on its library disk: the agent, its
    /// restore and Steam all come with the setup. Records from before this
    /// was kept count as started.
    #[serde(default = "started_before_records_said")]
    pub setup_started: bool,
}

fn started_before_records_said() -> bool {
    true
}

impl MachineRecord {
    pub fn new(name: String, vars: MachineVars) -> Self {
        Self {
            version: RECORD_VERSION,
            name,
            vars,
            saved_at: 0,
            setup_started: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.vars.machine_id
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("records always serialize")
    }

    pub fn from_json(json: &[u8]) -> ProviderResult<Self> {
        let record: Self = serde_json::from_slice(json)
            .map_err(|e| ProviderError::Cloud(format!("unreadable machine record: {e}")))?;
        if record.version > RECORD_VERSION {
            return Err(ProviderError::Cloud(format!(
                "machine {} was saved by a newer ReGa; update the app",
                record.id()
            )));
        }
        Ok(record)
    }

    /// `terraform.tfvars.json` for this machine.
    pub fn tfvars_json(&self) -> String {
        serde_json::to_string_pretty(&self.vars).expect("variables always serialize")
    }
}

/// What the machine's agent writes at [`final_sync_key`] after its last
/// upload of the game library before a stop.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FinalSync {
    pub machine_id: String,
    #[serde(default)]
    pub instance_id: String,
    pub synced: bool,
    /// RFC 3339, UTC.
    pub finished_utc: String,
}

impl FinalSync {
    pub fn from_json(json: &[u8]) -> Option<Self> {
        // Windows PowerShell writes a byte order mark when asked for UTF-8.
        let json = json.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(json);
        serde_json::from_slice(json).ok()
    }

    /// Whether it vouches for `instance_id` of `machine_id`, last started at
    /// `started` (Unix seconds): the upload worked and finished after that
    /// start, so no game installed since is missing from it.
    pub fn vouches_for(&self, machine_id: &str, instance_id: &str, started: u64) -> bool {
        use aws_sdk_s3::primitives::{DateTime, DateTimeFormat};
        let finished = DateTime::from_str(&self.finished_utc, DateTimeFormat::DateTime)
            .map(|t| t.as_secs_f64())
            .unwrap_or(f64::MIN);
        self.synced
            && self.machine_id == machine_id
            && (self.instance_id.is_empty() || self.instance_id == instance_id)
            && finished > started as f64
    }
}

/// Every resource id a Terraform state names: each managed resource's `id`
/// (for key pairs and alarms that is their name) and any volume it holds,
/// such as an instance's root volume, which has no resource of its own.
pub fn state_resource_ids(state: &[u8]) -> ProviderResult<HashSet<String>> {
    #[derive(Deserialize)]
    struct State {
        #[serde(default)]
        resources: Vec<Resource>,
    }
    #[derive(Deserialize)]
    struct Resource {
        mode: String,
        #[serde(default)]
        instances: Vec<ResourceInstance>,
    }
    #[derive(Deserialize)]
    struct ResourceInstance {
        #[serde(default)]
        attributes: Value,
    }
    fn volume_ids(value: &Value, ids: &mut HashSet<String>) {
        match value {
            Value::Object(fields) => {
                for (name, value) in fields {
                    if let (true, Some(id)) = (name.ends_with("volume_id"), value.as_str()) {
                        ids.insert(id.to_string());
                    }
                    volume_ids(value, ids);
                }
            }
            Value::Array(items) => items.iter().for_each(|v| volume_ids(v, ids)),
            _ => {}
        }
    }

    let state: State = serde_json::from_slice(state)
        .map_err(|e| ProviderError::Cloud(format!("unreadable Terraform state: {e}")))?;
    let mut ids = HashSet::new();
    for resource in state.resources.iter().filter(|r| r.mode == "managed") {
        for instance in &resource.instances {
            if let Some(id) = instance.attributes["id"].as_str() {
                ids.insert(id.to_string());
            }
            volume_ids(&instance.attributes, &mut ids);
        }
    }
    Ok(ids)
}

/// Matches the module's `machine_id` validation: 1-32 lowercase letters,
/// digits or dashes, not starting with a dash.
pub fn is_machine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A new id from the machine's name plus a random suffix, such as
/// `gaming-pc-3f9a1c`.
pub fn new_machine_id(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 20 {
            break;
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug = if slug.is_empty() { "machine" } else { slug };
    let mut random = [0u8; 3];
    aws_lc_rs::rand::fill(&mut random).expect("the system has randomness");
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    format!("{slug}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn vars() -> MachineVars {
        MachineVars {
            region: "eu-central-1".into(),
            machine_id: "gaming-abc123".into(),
            instance_type: "g4dn.xlarge".into(),
            spot: false,
            instance_state: InstanceState::Running,
            availability_zone: None,
            public_key: "ssh-rsa AAAA".into(),
            root_volume_size: 128,
            library_volume_enabled: true,
            wireguard_enabled: true,
            client_cidrs: vec![],
            admin_cidrs: vec![],
        }
    }

    #[test]
    fn ids_fit_the_module_validation() {
        for name in [
            "Gaming PC",
            "  ",
            "Ñandú 3000!",
            "a very long machine name indeed",
        ] {
            let id = new_machine_id(name);
            assert!(is_machine_id(&id), "{id}");
        }
        assert!(new_machine_id("Gaming PC").starts_with("gaming-pc-"));
        assert!(new_machine_id("").starts_with("machine-"));
        assert_ne!(new_machine_id("x"), new_machine_id("x"));
        assert!(!is_machine_id("-a"));
        assert!(!is_machine_id("A"));
        assert!(!is_machine_id(&"a".repeat(33)));
    }

    #[test]
    fn bucket_keys_map_back_to_their_machine() {
        for key in all_keys("gaming-abc123") {
            assert_eq!(machine_id_of(&key), Some("gaming-abc123"), "{key}");
        }
        assert_eq!(
            secrets_key("gaming-abc123"),
            "machines/gaming-abc123.secrets.json"
        );
        assert_eq!(machine_id_of("machines/x.txt"), None);
        assert_eq!(machine_id_of("other/x.json"), None);
    }

    #[test]
    fn state_names_its_resources_and_their_volumes() {
        let state = br#"{
            "version": 4,
            "resources": [
                {"mode": "data", "type": "aws_vpc", "name": "default",
                 "instances": [{"attributes": {"id": "vpc-1"}}]},
                {"mode": "managed", "type": "aws_instance", "name": "machine",
                 "instances": [{"attributes": {"id": "i-1",
                     "root_block_device": [{"volume_id": "vol-root", "tags": {"Name": "x"}}]}}]},
                {"mode": "managed", "type": "aws_key_pair", "name": "machine",
                 "instances": [{"attributes": {"id": "rega-m", "key_pair_id": "key-1"}}]},
                {"mode": "managed", "type": "aws_volume_attachment", "name": "library",
                 "instances": [{"attributes": {"id": "vai-1", "volume_id": "vol-lib"}}]}
            ]
        }"#;
        let ids = state_resource_ids(state).unwrap();
        for id in ["i-1", "vol-root", "rega-m", "vai-1", "vol-lib"] {
            assert!(ids.contains(id), "{id}");
        }
        assert!(!ids.contains("vpc-1"));
        assert!(state_resource_ids(b"{}").unwrap().is_empty());
        assert!(state_resource_ids(b"{").is_err());
    }

    #[test]
    fn record_round_trips_and_rejects_newer_versions() {
        let record = MachineRecord::new("Gaming".into(), vars());
        let back = MachineRecord::from_json(record.to_json().as_bytes()).unwrap();
        assert_eq!(back, record);

        let mut newer = record.clone();
        newer.version = RECORD_VERSION + 1;
        assert!(MachineRecord::from_json(newer.to_json().as_bytes()).is_err());
    }

    #[test]
    fn older_records_without_a_save_time_still_read() {
        let mut json: serde_json::Value =
            serde_json::from_str(&MachineRecord::new("G".into(), vars()).to_json()).unwrap();
        json.as_object_mut().unwrap().remove("saved_at");
        json.as_object_mut().unwrap().remove("setup_started");
        let record = MachineRecord::from_json(json.to_string().as_bytes()).unwrap();
        assert_eq!(record.saved_at, 0);
        // It may have been played on, so its library disk counts.
        assert!(record.setup_started);
        assert!(!MachineRecord::new("G".into(), vars()).setup_started);
    }

    #[test]
    fn a_final_sync_vouches_only_for_a_good_upload_after_the_last_start() {
        // 2026-10-11T10:00:00Z
        let started = 1_791_712_800;
        // As Windows PowerShell writes it: a byte order mark and seven
        // fractional digits.
        let record = |synced: bool, machine: &str, instance: &str, finished: &str| {
            let json = format!(
                "\u{feff}{{\"machine_id\": \"{machine}\", \"instance_id\": \"{instance}\", \
                 \"boot_utc\": \"2026-10-11T10:01:00.0000000Z\", \
                 \"finished_utc\": \"{finished}\", \"synced\": {synced}, \
                 \"skipped_apps\": [], \"failed_apps\": []}}"
            );
            FinalSync::from_json(json.as_bytes()).unwrap()
        };
        let good = record(true, "m", "i-1", "2026-10-11T10:30:00.1234567Z");
        assert!(good.vouches_for("m", "i-1", started));
        // From before the last start: games installed since aren't in it.
        assert!(!good.vouches_for("m", "i-1", started + 3600));
        assert!(!good.vouches_for("other", "i-1", started));
        assert!(!good.vouches_for("m", "i-2", started));
        assert!(!record(false, "m", "i-1", "2026-10-11T10:30:00Z").vouches_for("m", "i-1", started));
        assert!(!record(true, "m", "i-1", "yesterday").vouches_for("m", "i-1", started));
        // Off EC2 the agent can't name the instance.
        assert!(record(true, "m", "", "2026-10-11T10:30:00Z").vouches_for("m", "i-1", started));
        assert_eq!(FinalSync::from_json(b"{"), None);
    }

    #[test]
    fn tfvars_use_the_module_names() {
        let json: serde_json::Value =
            serde_json::from_str(&MachineRecord::new("G".into(), vars()).tfvars_json()).unwrap();
        assert_eq!(json["instance_state"], "running");
        assert_eq!(json["root_volume_size"], 128);
        // Unset zone is left out, so Terraform picks one.
        assert!(json.get("availability_zone").is_none());
    }

    #[test]
    fn every_variable_is_declared_by_the_module() {
        let declared = include_str!("../../../../infra/terraform/aws/variables.tf");
        let json: serde_json::Value = serde_json::from_str(
            &MachineRecord::new(
                "G".into(),
                MachineVars {
                    availability_zone: Some("z".into()),
                    ..vars()
                },
            )
            .tfvars_json(),
        )
        .unwrap();
        for name in json.as_object().unwrap().keys() {
            assert!(
                declared.contains(&format!("variable \"{name}\"")),
                "{name} isn't a module variable"
            );
        }
    }
}
