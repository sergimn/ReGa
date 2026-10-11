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
