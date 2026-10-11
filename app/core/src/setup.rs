//! What runs on a machine once the cloud has it running, and before it stops.
//!
//! This is where the Ansible playbook in `infra/ansible` plugs in: a provider
//! calls [`MachineSetup::after_start`] after every launch and start, and
//! [`MachineSetup::before_stop`] before every stop. Until the playbook is
//! wired up the app uses [`NoSetup`], which does nothing and keeps the game
//! library disk on stop so no downloaded game is lost.

use async_trait::async_trait;

use crate::provider::ProviderResult;

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
            .finish()
    }
}

/// Whether the game library disk (D:) was copied to object storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibrarySync {
    /// D: is safe in object storage, so the provider drops it on stop.
    Synced,
    /// D: was not copied, so the provider keeps it while stopped.
    NotSynced,
}

#[async_trait]
pub trait MachineSetup: Send + Sync {
    /// Whether the hooks run at all. When they don't, the provider skips
    /// fetching the machine's password for them.
    fn enabled(&self) -> bool {
        true
    }

    /// Runs after a launch or a start, once the machine is running. WinRM
    /// (5986) is closed unless the provider was asked to open it, so the
    /// Ansible run needs the provider's admin access first.
    async fn after_start(&self, machine: &MachineAccess) -> ProviderResult<()>;

    /// Runs before an on-demand machine stops. An error doesn't block the
    /// stop: the provider keeps D:, as for [`LibrarySync::NotSynced`].
    async fn before_stop(&self, machine: &MachineAccess) -> ProviderResult<LibrarySync>;
}

/// Runs nothing. Stopping keeps D:, because nothing synced it.
pub struct NoSetup;

#[async_trait]
impl MachineSetup for NoSetup {
    fn enabled(&self) -> bool {
        false
    }

    async fn after_start(&self, _machine: &MachineAccess) -> ProviderResult<()> {
        Ok(())
    }

    async fn before_stop(&self, _machine: &MachineAccess) -> ProviderResult<LibrarySync> {
        Ok(LibrarySync::NotSynced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_password() {
        let access = MachineAccess {
            machine_id: "m".into(),
            region_id: "r".into(),
            instance_id: "i".into(),
            type_name: "t".into(),
            public_ip: None,
            admin_password: Some("hunter2".into()),
            wireguard: true,
        };
        assert!(!format!("{access:?}").contains("hunter2"));
    }
}
