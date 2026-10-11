//! Provider-agnostic core of the ReGa desktop app.
//!
//! Every cloud provider implements [`provider::CloudProvider`]. The UI never
//! talks to a cloud SDK directly: it goes through the bridge crate, which
//! holds a list of providers and calls into this crate. Machines are made
//! with Terraform ([`terraform`]); what runs on them afterwards (Ansible)
//! plugs in through [`setup::MachineSetup`].

pub mod aws;
pub mod latency;
pub mod mock;
pub mod model;
pub mod provider;
pub mod region;
pub mod setup;
pub mod terraform;

pub use model::*;
pub use provider::{CloudProvider, ProviderError, ProviderResult};
