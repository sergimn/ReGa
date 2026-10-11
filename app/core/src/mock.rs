//! In-memory provider for tests and for the app's demo mode (no cloud account
//! needed). It follows the same rules as a real provider: spot is GPU only,
//! spot machines can't be resized, and only stopped machines change type.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;

use crate::aws;
use crate::model::{
    LaunchRequest, Machine, MachineState, MachineType, Orphan, Price, Region, StorageLocation,
};
use crate::provider::{CloudProvider, ProviderError, ProviderResult};

#[derive(Debug, Clone)]
pub struct MockRegion {
    pub region: Region,
    /// GPU vCPU quotas for on-demand and spot.
    pub gpu_quota_on_demand: u32,
    pub gpu_quota_spot: u32,
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
    orphans: Mutex<Vec<Orphan>>,
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
            orphans: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }

    /// Pretends these resources were left behind in the account.
    pub fn with_orphans(self, orphans: Vec<Orphan>) -> Self {
        *self.orphans.lock().unwrap() = orphans;
        self
    }

    /// AWS-shaped sample data. Prices are illustrative, not live.
    pub fn demo_aws() -> Self {
        let region =
            |id: &str, name: &str, quotas: (u32, u32), gpu: f64, spot: f64, copy, rtt: u64| {
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
                    gpu_quota_on_demand: quotas.0,
                    gpu_quota_spot: quotas.1,
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
                region(
                    "eu-north-1",
                    "Europe (Stockholm)",
                    (4, 4),
                    0.56,
                    0.21,
                    0.0,
                    38,
                ),
                region(
                    "eu-central-1",
                    "Europe (Frankfurt)",
                    (4, 4),
                    0.66,
                    0.25,
                    4.0,
                    31,
                ),
                // On-demand quota only, so it drops out when Spot is on.
                region("eu-west-1", "Europe (Ireland)", (4, 0), 0.59, 0.19, 4.0, 45),
                region(
                    "us-east-1",
                    "US East (N. Virginia)",
                    (8, 8),
                    0.53,
                    0.18,
                    4.0,
                    110,
                ),
                region("eu-south-1", "Europe (Milan)", (0, 0), 0.62, 0.24, 4.0, 28),
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

    async fn check_gpu_quota(
        &self,
        region_id: &str,
        machine_type: &MachineType,
        spot: bool,
    ) -> ProviderResult<()> {
        if machine_type.is_gpu()
            && self.gpu_quota_vcpus(region_id, spot).await? < machine_type.spec.vcpus
        {
            return Err(ProviderError::Cloud(format!("no GPU quota in {region_id}")));
        }
        Ok(())
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

    async fn gpu_quota_vcpus(&self, region_id: &str, spot: bool) -> ProviderResult<u32> {
        let r = self.region(region_id)?;
        let quota = if spot {
            r.gpu_quota_spot
        } else {
            r.gpu_quota_on_demand
        };
        // Like AWS, running GPU machines use up quota; stopped ones don't.
        let in_use: u32 = self
            .machines
            .lock()
            .unwrap()
            .iter()
            .filter(|m| m.region_id == region_id && m.spot == spot && m.machine_type.is_gpu())
            .filter(|m| matches!(m.state, MachineState::Pending | MachineState::Running))
            .map(|m| m.machine_type.spec.vcpus)
            .sum();
        Ok(quota.saturating_sub(in_use))
    }

    fn machine_types(&self) -> Vec<MachineType> {
        self.types.clone()
    }

    async fn price(&self, region_id: &str, type_name: &str) -> ProviderResult<Option<Price>> {
        Ok(self.region(region_id)?.prices.get(type_name).copied())
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
        let price = self.price(&request.region_id, &request.type_name).await?;
        if price.and_then(|p| p.per_hour(request.spot)).is_none() {
            return Err(ProviderError::Cloud(format!(
                "{} isn't sold that way in {}",
                request.type_name, request.region_id
            )));
        }
        self.check_gpu_quota(&request.region_id, &machine_type, request.spot)
            .await?;
        let mut next_id = self.next_id.lock().unwrap();
        let machine = Machine {
            id: format!("mock-{}", *next_id),
            name: request.name.clone(),
            region_id: request.region_id.clone(),
            machine_type,
            disk_gib: request.disk_gib,
            spot: request.spot,
            state: MachineState::Running,
            busy: false,
            starting: false,
            stopped_itself: false,
            library_kept: false,
        };
        *next_id += 1;
        self.machines.lock().unwrap().push(machine.clone());
        Ok(machine)
    }

    async fn resize(&self, machine_id: &str, type_name: &str) -> ProviderResult<Machine> {
        let machine_type = self.machine_type(type_name)?;
        let current = self
            .machines
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.id == machine_id)
            .cloned()
            .ok_or_else(|| ProviderError::UnknownMachine(machine_id.into()))?;
        if current.spot {
            return Err(ProviderError::SpotResize);
        }
        if !matches!(current.state, MachineState::Stopped | MachineState::Running) {
            return Err(ProviderError::InvalidState(
                "wait until the machine is running or stopped".into(),
            ));
        }
        self.check_gpu_quota(&current.region_id, &machine_type, false)
            .await?;
        self.update(machine_id, |m| {
            m.machine_type = machine_type;
            Ok(())
        })
    }

    async fn start(&self, machine_id: &str) -> ProviderResult<Machine> {
        let current = self
            .machines
            .lock()
            .unwrap()
            .iter()
            .find(|m| m.id == machine_id)
            .cloned()
            .ok_or_else(|| ProviderError::UnknownMachine(machine_id.into()))?;
        if current.state == MachineState::Stopped {
            self.check_gpu_quota(&current.region_id, &current.machine_type, current.spot)
                .await?;
        }
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

    /// Demo machines never stop by themselves.
    async fn finish_stop(&self, machine_id: &str) -> ProviderResult<Machine> {
        self.update(machine_id, |_| Ok(()))
    }

    async fn destroy(&self, machine_id: &str, _even_if_unsaved: bool) -> ProviderResult<()> {
        let mut machines = self.machines.lock().unwrap();
        let before = machines.len();
        machines.retain(|m| m.id != machine_id);
        if machines.len() == before {
            return Err(ProviderError::UnknownMachine(machine_id.into()));
        }
        Ok(())
    }

    async fn orphans(&self) -> ProviderResult<Vec<Orphan>> {
        Ok(self.orphans.lock().unwrap().clone())
    }

    async fn delete_orphans(&self, orphans: &[Orphan]) -> ProviderResult<()> {
        self.orphans
            .lock()
            .unwrap()
            .retain(|o| !orphans.contains(o));
        Ok(())
    }
}
