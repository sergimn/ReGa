//! In-memory provider for tests and for the app's demo mode (no cloud account
//! needed). It follows the same rules as a real provider: spot is GPU only,
//! spot machines can't be resized, and only stopped machines change type.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::aws;
use crate::model::{
    LaunchRequest, Machine, MachineState, MachineType, Price, Region, StorageLocation,
};
use crate::provider::{CloudProvider, ProviderError, ProviderResult};

#[derive(Debug, Clone)]
pub struct MockRegion {
    pub region: Region,
    pub gpu_quota: u32,
    pub prices: HashMap<String, Price>,
    pub library_copy_cost: f64,
    /// Latency the demo pretends to have measured.
    pub simulated_rtt: Option<Duration>,
}

pub struct MockProvider {
    id: String,
    display_name: String,
    types: Vec<MachineType>,
    regions: Vec<MockRegion>,
    machines: Mutex<Vec<Machine>>,
    next_id: Mutex<u32>,
}

impl MockProvider {
    pub fn new(
        id: &str,
        display_name: &str,
        types: Vec<MachineType>,
        regions: Vec<MockRegion>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            types,
            regions,
            machines: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }

    /// AWS-shaped sample data. Prices are illustrative, not live.
    pub fn demo_aws() -> Self {
        let region = |id: &str, name: &str, quota, gpu: f64, spot: f64, copy, rtt: u64| {
            let mut prices = HashMap::new();
            prices.insert(
                "g4dn.xlarge".to_string(),
                Price {
                    on_demand_per_hour: gpu,
                    spot_per_hour: Some(spot),
                },
            );
            for (t, p) in [
                ("t3.micro", 0.011),
                ("t3.medium", 0.045),
                ("t3.large", 0.09),
            ] {
                prices.insert(
                    t.to_string(),
                    Price {
                        on_demand_per_hour: p,
                        spot_per_hour: None,
                    },
                );
            }
            MockRegion {
                region: aws::region(id, name),
                gpu_quota: quota,
                prices,
                library_copy_cost: copy,
                simulated_rtt: Some(Duration::from_millis(rtt)),
            }
        };
        Self::new(
            "aws",
            "AWS",
            aws::catalog(),
            vec![
                region("eu-north-1", "Europe (Stockholm)", 4, 0.56, 0.21, 0.0, 38),
                region("eu-central-1", "Europe (Frankfurt)", 4, 0.66, 0.25, 4.0, 31),
                region("eu-west-1", "Europe (Ireland)", 4, 0.59, 0.19, 4.0, 45),
                region(
                    "us-east-1",
                    "US East (N. Virginia)",
                    4,
                    0.53,
                    0.18,
                    4.0,
                    110,
                ),
                region("eu-south-1", "Europe (Milan)", 0, 0.62, 0.24, 4.0, 28),
            ],
        )
    }

    pub fn simulated_rtt(&self, region_id: &str) -> Option<Duration> {
        self.region(region_id).ok()?.simulated_rtt
    }

    fn region(&self, region_id: &str) -> ProviderResult<&MockRegion> {
        self.regions
            .iter()
            .find(|r| r.region.id == region_id)
            .ok_or_else(|| ProviderError::Cloud(format!("unknown region {region_id}")))
    }

    fn machine_type(&self, type_name: &str) -> ProviderResult<MachineType> {
        self.types
            .iter()
            .find(|t| t.type_name == type_name)
            .cloned()
            .ok_or_else(|| ProviderError::UnknownMachineType(type_name.into()))
    }

    fn update(
        &self,
        machine_id: &str,
        f: impl FnOnce(&mut Machine) -> ProviderResult<()>,
    ) -> ProviderResult<Machine> {
        let mut machines = self.machines.lock().unwrap();
        let machine = machines
            .iter_mut()
            .find(|m| m.id == machine_id)
            .ok_or_else(|| ProviderError::UnknownMachine(machine_id.into()))?;
        f(machine)?;
        Ok(machine.clone())
    }
}

#[async_trait]
impl CloudProvider for MockProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    async fn ensure_storage(&self) -> ProviderResult<StorageLocation> {
        Ok(StorageLocation {
            state_bucket: format!("rega-tfstate-{}", self.id),
            library_bucket: format!("rega-games-{}", self.id),
            region_id: self.regions[0].region.id.clone(),
        })
    }

    async fn regions(&self) -> ProviderResult<Vec<Region>> {
        Ok(self.regions.iter().map(|r| r.region.clone()).collect())
    }

    async fn gpu_quota(&self, region_id: &str) -> ProviderResult<u32> {
        Ok(self.region(region_id)?.gpu_quota)
    }

    fn machine_types(&self) -> Vec<MachineType> {
        self.types.clone()
    }

    async fn price(&self, region_id: &str, type_name: &str) -> ProviderResult<Price> {
        self.region(region_id)?
            .prices
            .get(type_name)
            .copied()
            .ok_or_else(|| ProviderError::UnknownMachineType(type_name.into()))
    }

    async fn library_copy_cost(&self, region_id: &str) -> ProviderResult<f64> {
        Ok(self.region(region_id)?.library_copy_cost)
    }

    async fn list_machines(&self) -> ProviderResult<Vec<Machine>> {
        Ok(self.machines.lock().unwrap().clone())
    }

    async fn launch(&self, request: &LaunchRequest) -> ProviderResult<Machine> {
        let machine_type = self.machine_type(&request.type_name)?;
        if request.spot && !machine_type.is_gpu() {
            return Err(ProviderError::SpotNeedsGpu);
        }
        if machine_type.is_gpu() && self.gpu_quota(&request.region_id).await? == 0 {
            return Err(ProviderError::Cloud(format!(
                "no GPU quota in {}",
                request.region_id
            )));
        }
        let mut next_id = self.next_id.lock().unwrap();
        let machine = Machine {
            id: format!("mock-{}", *next_id),
            name: request.name.clone(),
            region_id: request.region_id.clone(),
            machine_type,
            disk_gib: request.disk_gib,
            spot: request.spot,
            state: MachineState::Running,
        };
        *next_id += 1;
        self.machines.lock().unwrap().push(machine.clone());
        Ok(machine)
    }

    async fn resize(&self, machine_id: &str, type_name: &str) -> ProviderResult<Machine> {
        let machine_type = self.machine_type(type_name)?;
        self.update(machine_id, |m| {
            if m.spot {
                return Err(ProviderError::SpotResize);
            }
            if m.state != MachineState::Stopped {
                return Err(ProviderError::InvalidState(
                    "stop the machine before changing its type".into(),
                ));
            }
            m.machine_type = machine_type;
            Ok(())
        })
    }

    async fn start(&self, machine_id: &str) -> ProviderResult<Machine> {
        self.update(machine_id, |m| {
            m.state = MachineState::Running;
            Ok(())
        })
    }

    async fn stop(&self, machine_id: &str) -> ProviderResult<Machine> {
        self.update(machine_id, |m| {
            m.state = MachineState::Stopped;
            Ok(())
        })
    }

    async fn destroy(&self, machine_id: &str) -> ProviderResult<()> {
        let mut machines = self.machines.lock().unwrap();
        let before = machines.len();
        machines.retain(|m| m.id != machine_id);
        if machines.len() == before {
            return Err(ProviderError::UnknownMachine(machine_id.into()));
        }
        Ok(())
    }
}
