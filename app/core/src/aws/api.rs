//! The AWS calls the provider makes, behind a trait so the provider's logic
//! can be tested without an account. [`super::sdk::SdkApi`] is the real one.

use async_trait::async_trait;

use crate::model::{MachineState, Orphan};
use crate::provider::ProviderResult;

/// An EC2 instance tagged `Project=ReGa`.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub id: String,
    pub region_id: String,
    /// The `ReGaMachine` tag.
    pub machine_id: Option<String>,
    pub type_name: String,
    pub state: MachineState,
    pub spot: bool,
    pub public_ip: Option<String>,
    /// When it last started, in Unix seconds (EC2's launch time, which every
    /// start moves).
    pub launched: Option<u64>,
}

/// What the provider checks on its buckets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BucketInfo {
    pub versioned: bool,
    pub encrypted: bool,
    /// All four Block Public Access settings are on.
    pub public_access_blocked: bool,
    /// Carries `Project=ReGa`.
    pub tagged: bool,
    /// Has lifecycle rules (ReGa's: old object versions expire).
    pub lifecycle: bool,
}

#[async_trait]
pub trait AwsApi: Send + Sync {
    /// Regions enabled for the account.
    async fn regions(&self) -> ProviderResult<Vec<String>>;

    /// Which of `types` are sold in `region`.
    async fn offered_types(&self, region: &str, types: &[String]) -> ProviderResult<Vec<String>>;

    /// The availability zones of `region` that sell `type_name`.
    async fn offered_zones(&self, region: &str, type_name: &str) -> ProviderResult<Vec<String>>;

    /// The applied G and VT vCPU quota, spot or on-demand.
    async fn gpu_quota(&self, region: &str, spot: bool) -> ProviderResult<u32>;

    /// vCPUs of pending or running G and VT instances, ReGa's or not.
    async fn gpu_vcpus_in_use(&self, region: &str, spot: bool) -> ProviderResult<u32>;

    /// Windows on-demand price per hour, `None` when the type isn't sold.
    async fn on_demand_price(&self, region: &str, type_name: &str) -> ProviderResult<Option<f64>>;

    /// Current Windows spot price per hour in each availability zone.
    async fn spot_prices(
        &self,
        region: &str,
        type_name: &str,
    ) -> ProviderResult<Vec<(String, f64)>>;

    /// Instances tagged `Project=ReGa` that aren't terminated.
    async fn tagged_instances(&self, region: &str) -> ProviderResult<Vec<Instance>>;

    /// The instance's Windows password, encrypted with its key pair; `None`
    /// until Windows has made one.
    async fn password_data(
        &self,
        region: &str,
        instance_id: &str,
    ) -> ProviderResult<Option<String>>;

    /// Every resource tagged `Project=ReGa` that the module can create:
    /// live instances, volumes, security groups, key pairs and alarms.
    async fn tagged_resources(&self, region: &str) -> ProviderResult<Vec<Orphan>>;

    /// Deletes one resource. Instances are terminated and waited for.
    async fn delete_resource(&self, resource: &Orphan) -> ProviderResult<()>;

    /// `None` when the bucket doesn't exist.
    async fn bucket(&self, name: &str) -> ProviderResult<Option<BucketInfo>>;

    async fn create_bucket(&self, name: &str, region: &str) -> ProviderResult<()>;

    async fn enable_versioning(&self, name: &str) -> ProviderResult<()>;

    /// Default encryption with S3-managed keys.
    async fn enable_encryption(&self, name: &str) -> ProviderResult<()>;

    /// Adds `Project=ReGa`, keeping other tags.
    async fn tag_bucket(&self, name: &str) -> ProviderResult<()>;

    /// Lifecycle rules that expire old object versions (a destroyed
    /// machine's state and key) and abandoned uploads.
    async fn expire_old_versions(&self, name: &str) -> ProviderResult<()>;

    /// Keys and sizes under `prefix`. A missing bucket lists nothing.
    async fn list_objects(&self, bucket: &str, prefix: &str) -> ProviderResult<Vec<(String, u64)>>;

    /// `None` when the object (or bucket) doesn't exist.
    async fn get_object(&self, bucket: &str, key: &str) -> ProviderResult<Option<Vec<u8>>>;

    async fn put_object(&self, bucket: &str, key: &str, body: Vec<u8>) -> ProviderResult<()>;

    /// Succeeds when the object is already gone.
    async fn delete_object(&self, bucket: &str, key: &str) -> ProviderResult<()>;

    /// The current credentials as environment variables, for Terraform.
    async fn credential_env(&self) -> ProviderResult<Vec<(String, String)>>;
}
