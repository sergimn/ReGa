//! The one interface every cloud provider implements.

use async_trait::async_trait;

use crate::model::{LaunchRequest, Machine, MachineType, Orphan, Price, Region, StorageLocation};

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ProviderError {
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("no credentials configured for {0}")]
    MissingCredentials(String),
    #[error("unknown machine type {0}")]
    UnknownMachineType(String),
    #[error("unknown machine {0}")]
    UnknownMachine(String),
    #[error("spot machines can't change type")]
    SpotResize,
    #[error("spot is only offered for GPU types")]
    SpotNeedsGpu,
    #[error("{0}")]
    InvalidState(String),
    /// Destroying the machine would lose games its last upload didn't
    /// save; [`CloudProvider::destroy`] with `even_if_unsaved` goes ahead.
    #[error("{0}")]
    GamesNotSaved(String),
    #[error("{0}")]
    Cloud(String),
}

pub type ProviderResult<T> = Result<T, ProviderError>;

/// A cloud provider: AWS today, Azure and GCP later.
///
/// Each provider keeps its own Terraform state and game library in its own
/// object storage, and only ever touches resources tagged `Project=ReGa`.
#[async_trait]
pub trait CloudProvider: Send + Sync {
    /// Stable id, such as `aws`.
    fn id(&self) -> &str;

    /// Name shown in the side menu, such as `AWS`.
    fn display_name(&self) -> &str;

    /// Creates the state and library buckets if they don't exist yet.
    async fn ensure_storage(&self) -> ProviderResult<StorageLocation>;

    /// Every region that offers at least one of [`Self::machine_types`].
    async fn regions(&self) -> ProviderResult<Vec<Region>>;

    /// GPU vCPUs the account can still start in `region_id`, for spot or
    /// on-demand (clouds keep separate quotas for each): the quota minus what
    /// is already running there, ReGa's or not. A region is only offered
    /// when this covers the GPU type's vCPUs.
    async fn gpu_quota_vcpus(&self, region_id: &str, spot: bool) -> ProviderResult<u32>;

    /// The machine types the app offers, CPU and GPU.
    fn machine_types(&self) -> Vec<MachineType>;

    /// `None` when `type_name` isn't sold in `region_id`.
    async fn price(&self, region_id: &str, type_name: &str) -> ProviderResult<Option<Price>>;

    /// One-off cost in USD of copying the game library to `region_id`.
    async fn library_copy_cost(&self, region_id: &str) -> ProviderResult<f64>;

    /// Machines tagged `Project=ReGa`, in every region.
    async fn list_machines(&self) -> ProviderResult<Vec<Machine>>;

    /// Creates a machine, starts it and sets it up. Takes a while: Windows
    /// boots, the setup installs everything and restores the game library.
    async fn launch(&self, request: &LaunchRequest) -> ProviderResult<Machine>;

    /// Changes an on-demand machine's type (the CPU and GPU switch). A
    /// stopped machine stays stopped; a running one restarts as the new
    /// type, keeping its game library disk, and is set up again.
    async fn resize(&self, machine_id: &str, type_name: &str) -> ProviderResult<Machine>;

    async fn start(&self, machine_id: &str) -> ProviderResult<Machine>;

    /// Saves the game library and stops the machine. The library disk goes
    /// only once the machine has saved it.
    async fn stop(&self, machine_id: &str) -> ProviderResult<Machine>;

    /// Finishes the stop of a machine that stopped itself
    /// ([`Machine::stopped_itself`]), dropping its library disk if it saved
    /// its games first. Does nothing if it no longer counts as stopped by
    /// itself, say because another device has started it since.
    async fn finish_stop(&self, machine_id: &str) -> ProviderResult<Machine>;

    /// Saves the game library of a running machine, then deletes the
    /// machine and its disks. Refuses with [`ProviderError::GamesNotSaved`]
    /// when its library disk may hold games the game library doesn't.
    /// `even_if_unsaved` deletes it without trying to save the games.
    async fn destroy(&self, machine_id: &str, even_if_unsaved: bool) -> ProviderResult<()>;

    /// Resources tagged `Project=ReGa`, in every region, that belong to no
    /// machine in the state store.
    async fn orphans(&self) -> ProviderResult<Vec<Orphan>>;

    /// Deletes those of `orphans` that are still orphans, instances first.
    async fn delete_orphans(&self, orphans: &[Orphan]) -> ProviderResult<()>;
}
