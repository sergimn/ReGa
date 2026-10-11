//! AWS implementation of [`CloudProvider`].
//!
//! The catalog (machine types and their specs) and bucket naming are real.
//! Calls that need the AWS SDK return [`ProviderError::NotImplemented`] until
//! they are wired up.

use async_trait::async_trait;

use crate::model::{
    Gpu, LaunchRequest, Machine, MachineSpec, MachineType, Price, Region, StorageLocation,
};
use crate::provider::{CloudProvider, ProviderError, ProviderResult};

/// Region holding the state and library buckets (also the development region).
pub const HOME_REGION: &str = "eu-north-1";

/// Machine types the IAM policy allows the app to use.
pub fn catalog() -> Vec<MachineType> {
    let cpu = |name: &str, vcpus, ram_gib| MachineType {
        type_name: name.into(),
        spec: MachineSpec {
            vcpus,
            ram_gib,
            gpu: None,
        },
    };
    vec![
        MachineType {
            type_name: "g4dn.xlarge".into(),
            spec: MachineSpec {
                vcpus: 4,
                ram_gib: 16.0,
                gpu: Some(Gpu {
                    model: "NVIDIA T4".into(),
                    vram_gib: 16,
                }),
            },
        },
        cpu("t3.micro", 2, 1.0),
        cpu("t3.medium", 2, 4.0),
        cpu("t3.large", 2, 8.0),
    ]
}

pub fn state_bucket(account_id: &str) -> String {
    format!("rega-tfstate-{account_id}")
}

pub fn library_bucket(account_id: &str) -> String {
    format!("rega-games-{account_id}")
}

pub fn region(id: &str, display_name: &str) -> Region {
    Region {
        id: id.into(),
        display_name: display_name.into(),
        latency_probe_host: format!("ec2.{id}.amazonaws.com"),
    }
}

pub struct AwsProvider {
    account_id: Option<String>,
}

impl AwsProvider {
    /// `account_id` comes from STS once credentials are configured.
    pub fn new(account_id: Option<String>) -> Self {
        Self { account_id }
    }

    fn account_id(&self) -> ProviderResult<&str> {
        self.account_id
            .as_deref()
            .ok_or_else(|| ProviderError::MissingCredentials("AWS".into()))
    }
}

#[async_trait]
impl CloudProvider for AwsProvider {
    fn id(&self) -> &str {
        "aws"
    }

    fn display_name(&self) -> &str {
        "AWS"
    }

    async fn ensure_storage(&self) -> ProviderResult<StorageLocation> {
        let account = self.account_id()?;
        let _location = StorageLocation {
            state_bucket: state_bucket(account),
            library_bucket: library_bucket(account),
            region_id: HOME_REGION.into(),
        };
        // Create both buckets if missing (state versioned and encrypted).
        // Block Public Access is only checked, never set.
        Err(ProviderError::NotImplemented("AWS bucket setup"))
    }

    async fn regions(&self) -> ProviderResult<Vec<Region>> {
        Err(ProviderError::NotImplemented("AWS region discovery"))
    }

    async fn gpu_quota_vcpus(&self, _region_id: &str, _spot: bool) -> ProviderResult<u32> {
        Err(ProviderError::NotImplemented("AWS service quotas"))
    }

    fn machine_types(&self) -> Vec<MachineType> {
        catalog()
    }

    async fn price(&self, _region_id: &str, _type_name: &str) -> ProviderResult<Option<Price>> {
        Err(ProviderError::NotImplemented("AWS pricing"))
    }

    async fn library_copy_cost(&self, region_id: &str) -> ProviderResult<f64> {
        if region_id == HOME_REGION {
            return Ok(0.0);
        }
        Err(ProviderError::NotImplemented("AWS library size"))
    }

    async fn list_machines(&self) -> ProviderResult<Vec<Machine>> {
        Err(ProviderError::NotImplemented("AWS machine listing"))
    }

    async fn launch(&self, _request: &LaunchRequest) -> ProviderResult<Machine> {
        Err(ProviderError::NotImplemented("AWS launch"))
    }

    async fn resize(&self, _machine_id: &str, _type_name: &str) -> ProviderResult<Machine> {
        Err(ProviderError::NotImplemented("AWS resize"))
    }

    async fn start(&self, _machine_id: &str) -> ProviderResult<Machine> {
        Err(ProviderError::NotImplemented("AWS start"))
    }

    async fn stop(&self, _machine_id: &str) -> ProviderResult<Machine> {
        Err(ProviderError::NotImplemented("AWS stop"))
    }

    async fn destroy(&self, _machine_id: &str) -> ProviderResult<()> {
        Err(ProviderError::NotImplemented("AWS destroy"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_one_gpu_type_and_cpu_types() {
        let types = catalog();
        let gpus: Vec<_> = types.iter().filter(|t| t.is_gpu()).collect();
        assert_eq!(gpus.len(), 1);
        assert_eq!(gpus[0].type_name, "g4dn.xlarge");
        assert!(types.iter().any(|t| t.type_name == "t3.medium"));
    }

    #[test]
    fn bucket_names_follow_the_account() {
        assert_eq!(state_bucket("123"), "rega-tfstate-123");
        assert_eq!(library_bucket("123"), "rega-games-123");
    }

    #[test]
    fn region_probes_the_ec2_endpoint() {
        assert_eq!(
            region("eu-north-1", "Stockholm").latency_probe_host,
            "ec2.eu-north-1.amazonaws.com"
        );
    }

    #[tokio::test]
    async fn storage_needs_credentials() {
        let aws = AwsProvider::new(None);
        assert_eq!(
            aws.ensure_storage().await,
            Err(ProviderError::MissingCredentials("AWS".into()))
        );
    }

    #[tokio::test]
    async fn home_region_has_no_copy_cost() {
        let aws = AwsProvider::new(None);
        assert_eq!(aws.library_copy_cost(HOME_REGION).await, Ok(0.0));
    }
}
