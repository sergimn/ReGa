//! Provider-agnostic core of the ReGa desktop app.
//!
//! Every cloud provider implements [`provider::CloudProvider`]. The UI never
//! talks to a cloud SDK directly: it goes through the bridge crate, which
//! holds a list of providers and calls into this crate. Machines are made
//! with Terraform ([`terraform`]); what runs on them afterwards plugs in
//! through [`setup::MachineSetup`], which [`ansible`] implements.
//!
//! A cloud is signed in to with the credentials the user enters in the app
//! and nothing else ([`credentials`], [`provider::Connector`]).

pub mod ansible;
pub mod aws;
pub mod credentials;
pub mod latency;
pub mod mock;
pub mod model;
pub mod provider;
pub mod region;
pub mod setup;
pub mod stream;
pub mod terraform;

pub use model::*;
pub use provider::{CloudProvider, Connector, ProviderError, ProviderResult};
