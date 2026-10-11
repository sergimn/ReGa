//! Plain data types shared by every provider and by the UI.

/// A GPU attached to a machine type.
#[derive(Debug, Clone, PartialEq)]
pub struct Gpu {
    pub model: String,
    pub vram_gib: u32,
}

/// What a machine type offers, in the terms the UI leads with.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineSpec {
    pub vcpus: u32,
    pub ram_gib: f32,
    pub gpu: Option<Gpu>,
}

/// A machine type a provider can launch, such as AWS `g4dn.xlarge`.
#[derive(Debug, Clone, PartialEq)]
pub struct MachineType {
    /// The provider's own name for the type. The UI shows it small.
    pub type_name: String,
    pub spec: MachineSpec,
}

impl MachineType {
    pub fn is_gpu(&self) -> bool {
        self.spec.gpu.is_some()
    }
}

/// The smallest CPU type among `types` that runs Windows comfortably (4 GB
/// of RAM or more): where a GPU machine goes to download games cheaply, and
/// where a new one is set up first.
pub fn roomy_cpu_type(types: &[MachineType]) -> Option<&MachineType> {
    types
        .iter()
        .filter(|t| !t.is_gpu() && t.spec.ram_gib >= 4.0)
        .min_by(|a, b| a.spec.ram_gib.total_cmp(&b.spec.ram_gib))
}

/// Hourly price of a machine type in one region, in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub on_demand_per_hour: f64,
    /// `None` when the provider has no spot offer for this type and region.
    pub spot_per_hour: Option<f64>,
}

impl Price {
    /// The price that applies for the chosen purchase option.
    pub fn per_hour(&self, spot: bool) -> Option<f64> {
        if spot {
            self.spot_per_hour
        } else {
            Some(self.on_demand_per_hour)
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// Provider region id, such as `eu-north-1`.
    pub id: String,
    /// Human name, such as `Europe (Stockholm)`.
    pub display_name: String,
    /// Host used to measure round-trip time, such as `ec2.eu-north-1.amazonaws.com`.
    pub latency_probe_host: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineState {
    Pending,
    Running,
    Stopping,
    Stopped,
    Terminated,
}

/// A machine that exists in a provider account (tagged `Project=ReGa`).
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub region_id: String,
    pub machine_type: MachineType,
    pub disk_gib: u32,
    pub spot: bool,
    pub state: MachineState,
    /// A change to it is running, on this device or another one. The UI
    /// leaves it alone until that ends.
    pub busy: bool,
    /// That change is bringing it up, though its instance may still read
    /// stopped. ReGa brings up one machine at a time, so no other starts.
    pub starting: bool,
    /// It stopped on its own (the idle stop, or the provider's backstop)
    /// and the app hasn't finished the stop yet: [`crate::CloudProvider::stop`]
    /// drops its game library disk if the machine saved its games first.
    pub stopped_itself: bool,
}

/// What the user asked for when launching a machine.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRequest {
    pub name: String,
    pub region_id: String,
    pub type_name: String,
    pub disk_gib: u32,
    pub spot: bool,
    pub wireguard: bool,
    /// For an on-demand GPU type: set the machine up and restore the games
    /// on a cheap CPU type first, then switch it to the GPU type.
    pub cpu_first: bool,
}

/// Where a provider keeps Terraform state and the game library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageLocation {
    pub state_bucket: String,
    pub library_bucket: String,
    pub region_id: String,
}

/// Kinds of tagged resources the app looks after, in the order they are
/// deleted: an instance holds its volumes and security group until it's gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
    Instance,
    Volume,
    SecurityGroup,
    KeyPair,
    Alarm,
}

/// A resource tagged `Project=ReGa` that no machine accounts for, such as
/// the leftovers of a failed launch or of a machine whose state was lost.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Orphan {
    pub kind: ResourceKind,
    /// The provider's id, such as `i-0abc` or a key pair name.
    pub id: String,
    pub region_id: String,
    /// The machine it was made for (the `ReGaMachine` tag), when tagged.
    pub machine_id: Option<String>,
    /// Its `Name` tag, for display.
    pub name: Option<String>,
}

/// System disk (C:) size offered by default, in GiB. The game library lives
/// on a separate disk that is restored from object storage at each start.
pub const DEFAULT_DISK_GIB: u32 = 128;
