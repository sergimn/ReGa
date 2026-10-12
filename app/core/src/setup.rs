//! What runs on a machine once the cloud has it running, and before it stops.
//!
//! This is where the Ansible playbook in `infra/ansible` plugs in
//! ([`crate::ansible::AnsibleSetup`]): a provider calls
//! [`MachineSetup::after_start`] after every launch, start and switch, and
//! [`MachineSetup::before_stop`] before it stops or destroys a running
//! machine. Without it ([`NoSetup`]) nothing runs on the machine and stopping
//! keeps the game library disk, since nothing copied it to object storage.

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::provider::{ProviderError, ProviderResult};

/// Passwords the setup gives the machine, kept with the machine's record so
/// every device that drives it uses the same ones, and what a device needs
/// to connect to it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineSecrets {
    /// Login of Sunshine's web UI and API, which pairs Moonlight.
    pub sunshine_password: String,
    /// The Windows account that logs on at boot and runs Steam.
    pub desktop_password: String,
    /// This end of the WireGuard tunnel, base64. One per machine, shared by
    /// every device: a machine streams to one device at a time anyway.
    /// Secrets saved before the app had one lack it, and the provider adds
    /// it.
    #[serde(default)]
    pub wireguard_private_key: String,
    /// The machine's end of the tunnel, learnt from the setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_server_public_key: Option<String>,
    /// SHA-256 of Sunshine's self-signed certificate (lowercase hex),
    /// learnt from the setup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sunshine_cert_sha256: Option<String>,
}

impl MachineSecrets {
    pub fn generate() -> ProviderResult<Self> {
        Ok(Self {
            sunshine_password: random_password()?,
            // Windows wants three kinds of character; the tail guarantees it.
            desktop_password: format!("{}-Rg7", random_password()?),
            wireguard_private_key: new_wireguard_key()?,
            wireguard_server_public_key: None,
            sunshine_cert_sha256: None,
        })
    }

    /// Adds a WireGuard key to secrets saved without one. Whether it did.
    pub fn add_missing_key(&mut self) -> ProviderResult<bool> {
        if !self.wireguard_private_key.is_empty() {
            return Ok(false);
        }
        self.wireguard_private_key = new_wireguard_key()?;
        Ok(true)
    }

    /// Takes in what a setup learnt. Whether anything changed.
    pub fn learn(&mut self, details: &ConnectionDetails) -> bool {
        let mut changed = false;
        for (field, learnt) in [
            (
                &mut self.wireguard_server_public_key,
                &details.wireguard_server_public_key,
            ),
            (
                &mut self.sunshine_cert_sha256,
                &details.sunshine_cert_sha256,
            ),
        ] {
            if learnt.is_some() && field != learnt {
                field.clone_from(learnt);
                changed = true;
            }
        }
        changed
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("secrets always serialize")
    }

    pub fn from_json(json: &[u8]) -> ProviderResult<Self> {
        serde_json::from_slice(json)
            .map_err(|e| ProviderError::Cloud(format!("unreadable machine passwords: {e}")))
    }
}

impl std::fmt::Debug for MachineSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MachineSecrets(…)")
    }
}

fn new_wireguard_key() -> ProviderResult<String> {
    crate::stream::tunnel::generate_private_key().map_err(|e| ProviderError::Cloud(e.to_string()))
}

/// What a setup learnt about the machine that a device needs to connect to
/// it, besides the passwords.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionDetails {
    pub wireguard_server_public_key: Option<String>,
    pub sunshine_cert_sha256: Option<String>,
}

/// 24 letters and digits.
fn random_password() -> ProviderResult<String> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::new();
    while out.len() < 24 {
        let mut bytes = [0u8; 32];
        aws_lc_rs::rand::fill(&mut bytes)
            .map_err(|_| ProviderError::Cloud("no randomness for a password".into()))?;
        // 248 is the largest multiple of 62 that fits a byte, so dropping
        // the bytes above it keeps every character equally likely.
        out.extend(
            bytes
                .iter()
                .filter(|b| **b < 248)
                .map(|b| CHARS[usize::from(*b) % CHARS.len()] as char),
        );
    }
    out.truncate(24);
    Ok(out)
}

/// How to reach a running machine.
#[derive(Clone, PartialEq)]
pub struct MachineAccess {
    pub machine_id: String,
    pub region_id: String,
    pub instance_id: String,
    pub type_name: String,
    /// Changes on every start; `None` while stopped.
    pub public_ip: Option<String>,
    /// The Windows Administrator password, once the machine has made one
    /// (a few minutes after the first boot).
    pub admin_password: Option<String>,
    /// Whether Moonlight reaches Sunshine through the WireGuard tunnel.
    pub wireguard: bool,
    /// The machine's folder on this device, for the setup's own files.
    pub dir: PathBuf,
    /// Where the game library lives in object storage.
    pub library_bucket: String,
    pub library_region: String,
    pub secrets: MachineSecrets,
}

impl std::fmt::Debug for MachineAccess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineAccess")
            .field("machine_id", &self.machine_id)
            .field("region_id", &self.region_id)
            .field("instance_id", &self.instance_id)
            .field("type_name", &self.type_name)
            .field("public_ip", &self.public_ip)
            .field("admin_password", &self.admin_password.as_ref().map(|_| "…"))
            .field("wireguard", &self.wireguard)
            .field("dir", &self.dir)
            .field("library_bucket", &self.library_bucket)
            .field("library_region", &self.library_region)
            .field("secrets", &self.secrets)
            .finish()
    }
}

#[async_trait]
pub trait MachineSetup: Send + Sync {
    /// Whether the hooks run at all. When they don't, the provider doesn't
    /// wait for the machine's password or open WinRM for them.
    fn enabled(&self) -> bool {
        true
    }

    /// Whether the hooks can run on this device. Checked before a machine
    /// comes up, so a missing tool fails before the machine costs money.
    fn ready(&self) -> ProviderResult<()> {
        Ok(())
    }

    /// Sets the machine up. Runs after every launch, start and switch, once
    /// the machine is running and has made its Windows password; the
    /// provider has opened WinRM (5986) to this device by then.
    async fn after_start(&self, machine: &MachineAccess) -> ProviderResult<()>;

    /// What the last [`Self::after_start`] learnt that a device needs to
    /// connect to the machine; `None` when it learns nothing.
    async fn connection(
        &self,
        _machine: &MachineAccess,
    ) -> ProviderResult<Option<ConnectionDetails>> {
        Ok(None)
    }

    /// Waits until the game library restore that runs at boot is over, so
    /// the games are on the machine's disk.
    async fn wait_for_library(&self, machine: &MachineAccess) -> ProviderResult<()>;

    /// Uploads the game library before the provider stops or destroys a
    /// running machine. The machine records the outcome in the library
    /// bucket, and the provider reads that record to decide whether the
    /// library disk can go, so an error here only means the disk stays.
    async fn before_stop(&self, machine: &MachineAccess) -> ProviderResult<()>;
}

/// Runs nothing.
pub struct NoSetup;

#[async_trait]
impl MachineSetup for NoSetup {
    fn enabled(&self) -> bool {
        false
    }

    async fn after_start(&self, _machine: &MachineAccess) -> ProviderResult<()> {
        Ok(())
    }

    async fn wait_for_library(&self, _machine: &MachineAccess) -> ProviderResult<()> {
        Ok(())
    }

    async fn before_stop(&self, _machine: &MachineAccess) -> ProviderResult<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_passwords() {
        let secrets = MachineSecrets {
            sunshine_password: "sunshine-secret".into(),
            desktop_password: "desktop-secret".into(),
            wireguard_private_key: "wireguard-secret".into(),
            wireguard_server_public_key: None,
            sunshine_cert_sha256: None,
        };
        let access = MachineAccess {
            machine_id: "m".into(),
            region_id: "r".into(),
            instance_id: "i".into(),
            type_name: "t".into(),
            public_ip: None,
            admin_password: Some("hunter2".into()),
            wireguard: true,
            dir: PathBuf::from("/tmp/m"),
            library_bucket: "b".into(),
            library_region: "eu-north-1".into(),
            secrets,
        };
        let shown = format!("{access:?}");
        for secret in [
            "hunter2",
            "sunshine-secret",
            "desktop-secret",
            "wireguard-secret",
        ] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }

    #[test]
    fn generated_passwords_are_long_and_windows_accepts_them() {
        let a = MachineSecrets::generate().unwrap();
        let b = MachineSecrets::generate().unwrap();
        assert_ne!(a, b);
        assert_eq!(a.sunshine_password.len(), 24);
        assert!(a
            .sunshine_password
            .bytes()
            .all(|c| c.is_ascii_alphanumeric()));
        let d = &a.desktop_password;
        assert!(d.bytes().any(|c| c.is_ascii_uppercase()));
        assert!(d.bytes().any(|c| c.is_ascii_lowercase()));
        assert!(d.bytes().any(|c| c.is_ascii_digit()));
        assert_eq!(
            MachineSecrets::from_json(a.to_json().as_bytes()).unwrap(),
            a
        );
        assert!(MachineSecrets::from_json(b"{").is_err());
        // Each machine gets its own WireGuard key, a real one.
        assert_ne!(a.wireguard_private_key, b.wireguard_private_key);
        assert!(crate::stream::tunnel::public_key(&a.wireguard_private_key).is_ok());
    }

    #[test]
    fn secrets_from_before_wireguard_keys_get_one() {
        let mut old =
            MachineSecrets::from_json(br#"{"sunshine_password": "s", "desktop_password": "d"}"#)
                .unwrap();
        assert_eq!(old.wireguard_private_key, "");
        assert!(old.add_missing_key().unwrap());
        let key = old.wireguard_private_key.clone();
        assert!(crate::stream::tunnel::public_key(&key).is_ok());
        assert!(!old.add_missing_key().unwrap());
        assert_eq!(old.wireguard_private_key, key);
        // Nothing learnt yet, so nothing saved for it.
        assert!(!old.to_json().contains("sunshine_cert_sha256"));
    }

    #[test]
    fn learning_keeps_what_it_isnt_told() {
        let mut secrets = MachineSecrets::generate().unwrap();
        let details = ConnectionDetails {
            wireguard_server_public_key: Some("server".into()),
            sunshine_cert_sha256: Some("ab".repeat(32)),
        };
        assert!(secrets.learn(&details));
        assert!(!secrets.learn(&details));
        // A setup that learnt nothing new about the certificate.
        let partial = ConnectionDetails {
            wireguard_server_public_key: Some("server2".into()),
            sunshine_cert_sha256: None,
        };
        assert!(secrets.learn(&partial));
        assert_eq!(
            secrets.wireguard_server_public_key.as_deref(),
            Some("server2")
        );
        assert_eq!(secrets.sunshine_cert_sha256, Some("ab".repeat(32)));
        let round = MachineSecrets::from_json(secrets.to_json().as_bytes()).unwrap();
        assert_eq!(round, secrets);
    }
}
