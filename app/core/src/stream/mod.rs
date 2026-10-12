//! Streaming from a running machine: the WireGuard tunnel to it, pairing
//! Moonlight with its Sunshine, and starting Moonlight. Nothing here knows
//! about a cloud; a provider says where the machine is
//! ([`crate::CloudProvider::stream_target`]).

#[cfg(test)]
mod fake;
mod ipv4;
pub mod moonlight;
pub mod sunshine;
pub mod tunnel;

use std::net::{Ipv4Addr, SocketAddr};

/// The machine's address inside the tunnel (the playbook's
/// `rega_wireguard_server_address`).
pub const SERVER_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 213, 37, 1);
/// This device's address inside the tunnel (`rega_wireguard_client_address`).
pub const CLIENT_ADDRESS: Ipv4Addr = Ipv4Addr::new(10, 213, 37, 2);
/// The tunnel's MTU (`rega_wireguard_mtu`): 1400 fits paths down to 1460
/// bytes, such as PPPoE and DS-Lite.
pub const TUNNEL_MTU: u16 = 1400;
/// Sunshine's web UI and API login (`rega_sunshine_username`).
pub const SUNSHINE_USER: &str = "rega";

/// What a device needs to stream from a running machine.
#[derive(Clone, PartialEq, Eq)]
pub struct StreamTarget {
    pub machine_id: String,
    pub public_ip: String,
    pub sunshine_user: String,
    pub sunshine_password: String,
    /// Lowercase hex; `None` until a setup has read it.
    pub sunshine_cert_sha256: Option<String>,
    /// `None` when the machine has WireGuard off: Moonlight then goes to
    /// the public IP.
    pub wireguard: Option<WireGuardPeer>,
}

impl std::fmt::Debug for StreamTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamTarget")
            .field("machine_id", &self.machine_id)
            .field("public_ip", &self.public_ip)
            .field("sunshine_user", &self.sunshine_user)
            .field("sunshine_password", &"…")
            .field("sunshine_cert_sha256", &self.sunshine_cert_sha256)
            .field("wireguard", &self.wireguard)
            .finish()
    }
}

/// The machine's end of the WireGuard tunnel, and this device's key for it.
#[derive(Clone, PartialEq, Eq)]
pub struct WireGuardPeer {
    /// The machine's public IP and WireGuard port.
    pub endpoint: SocketAddr,
    pub server_public_key: String,
    pub client_private_key: String,
    /// This device's address inside the tunnel, 10.213.37.2.
    pub client_address: Ipv4Addr,
    /// The machine's address inside the tunnel, 10.213.37.1.
    pub server_address: Ipv4Addr,
    pub mtu: u16,
}

impl std::fmt::Debug for WireGuardPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WireGuardPeer")
            .field("endpoint", &self.endpoint)
            .field("server_public_key", &self.server_public_key)
            .field("client_private_key", &"…")
            .field("client_address", &self.client_address)
            .field("server_address", &self.server_address)
            .field("mtu", &self.mtu)
            .finish()
    }
}
