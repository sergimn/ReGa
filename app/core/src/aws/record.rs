//! What the app keeps per machine next to its Terraform state, so any device
//! signed in to the account can drive the machine.
//!
//! Layout of the state bucket:
//!
//! ```text
//! machines/<id>.tfstate   Terraform state (S3 backend, with a .tflock while locked)
//! machines/<id>.json      MachineRecord: the name and the Terraform variables
//! machines/<id>.key.pem   the machine's RSA key, for its Windows password
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
    ]
}

/// The machine id named by a state-bucket object, if it is one of ours.
pub fn machine_id_of(key: &str) -> Option<&str> {
    let name = key.strip_prefix(PREFIX)?;
    [".tfstate.tflock", ".tfstate", ".json", ".key.pem"]
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
}

impl MachineRecord {
    pub fn new(name: String, vars: MachineVars) -> Self {
        Self {
            version: RECORD_VERSION,
            name,
            vars,
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
