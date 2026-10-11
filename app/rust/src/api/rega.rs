//! Everything the Flutter UI can call. Types here are flat views of the core
//! model, shaped for display.

use std::fmt::Display;

use futures::future::try_join_all;
use rega_core::region::{self, RegionCandidate};
use rega_core::{
    CloudProvider, LaunchRequest, Machine, MachineState, MachineType, Orphan, Region, ResourceKind,
    DEFAULT_DISK_GIB,
};

use crate::registry;

/// An error the UI shows as is: a plain message, never a backtrace.
#[derive(Debug)]
#[flutter_rust_bridge::frb(dart_code = "
  @override
  String toString() => message;
")]
pub struct RegaError {
    pub message: String,
}

impl<E: Display> From<E> for RegaError {
    fn from(e: E) -> Self {
        Self {
            message: e.to_string(),
        }
    }
}

pub type RegaResult<T> = Result<T, RegaError>;

pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    /// True while the provider shows sample data instead of a real account.
    pub demo: bool,
    /// Why it shows sample data, such as missing credentials.
    pub notice: Option<String>,
}

pub struct MachineOffer {
    pub type_name: String,
    pub vcpus: u32,
    pub ram_gib: f32,
    pub gpu_model: Option<String>,
    pub vram_gib: Option<u32>,
    pub disk_gib: u32,
    /// `None` when the type isn't sold this way (spot or on-demand) here.
    pub price_per_hour: Option<f64>,
}

impl MachineOffer {
    #[flutter_rust_bridge::frb(ignore)]
    pub fn is_gpu(&self) -> bool {
        self.gpu_model.is_some()
    }
}

pub struct RegionOption {
    pub region_id: String,
    pub display_name: String,
    pub rtt_ms: Option<u32>,
    pub price_per_hour: f64,
    /// One-off cost of copying the game library to this region, in USD.
    pub one_off_cost: f64,
}

pub struct RegionChoices {
    pub recommended: Option<RegionOption>,
    pub lowest_latency: Option<RegionOption>,
    pub cheapest: Option<RegionOption>,
    pub all: Vec<RegionOption>,
    /// Names of regions that couldn't be checked (their queries failed).
    pub unreachable: Vec<String>,
}

pub enum MachineStatus {
    Pending,
    Running,
    Stopping,
    Stopped,
    Terminated,
}

pub struct MachineSummary {
    pub id: String,
    pub name: String,
    pub region_id: String,
    /// Human name of the region, such as `Europe (Stockholm)`.
    pub region_name: String,
    pub offer: MachineOffer,
    pub spot: bool,
    pub status: MachineStatus,
    /// A change to it is running, on this device or another one; its
    /// actions wait until that ends.
    pub busy: bool,
    /// The type "Switch to GPU" or "Switch to CPU" moves to; `None` for spot
    /// machines, which can't change type.
    pub switch_type: Option<String>,
}

pub enum OrphanKind {
    Instance,
    Volume,
    SecurityGroup,
    KeyPair,
    Alarm,
}

/// A leftover resource tagged for ReGa that no machine accounts for.
pub struct OrphanSummary {
    pub kind: OrphanKind,
    pub id: String,
    pub region_id: String,
    pub region_name: String,
    pub machine_id: Option<String>,
    pub name: Option<String>,
}

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// Signs in to every provider and returns them for the side menu. A
/// provider without working credentials comes back as demo data, with the
/// reason. Calling it again reconnects.
pub async fn connect_providers() -> Vec<ProviderInfo> {
    registry::connect().await.iter().map(|e| info(e)).collect()
}

fn info(e: &registry::Entry) -> ProviderInfo {
    ProviderInfo {
        id: e.provider().id().into(),
        name: e.provider().display_name().into(),
        demo: e.demo_reason().is_some(),
        notice: e.demo_reason().map(String::from),
    }
}

/// Machine types sold in `region_id` the chosen way, priced. With `spot` on
/// that means GPU types only.
pub async fn machine_offers(
    provider_id: String,
    region_id: String,
    spot: bool,
) -> RegaResult<Vec<MachineOffer>> {
    let entry = registry::entry(&provider_id)?;
    let provider = entry.provider();
    let types = provider.machine_types();
    let prices = try_join_all(
        types
            .iter()
            .map(|t| provider.price(&region_id, &t.type_name)),
    )
    .await?;
    Ok(types
        .iter()
        .zip(prices)
        .filter(|(t, _)| !spot || t.is_gpu())
        .filter_map(|(t, p)| Some(offer(t, Some(p?.per_hour(spot)?))))
        .collect())
}

/// Measures latency to every region and caches it. Resolves when done, with
/// `true` if any region was measured (so the UI knows to refresh).
pub async fn probe_latency(provider_id: String) -> RegaResult<bool> {
    let entry = registry::entry(&provider_id)?;
    let regions = entry.provider().regions().await?;
    Ok(entry.probe(regions).await)
}

/// Recommended, Lowest latency and Cheapest regions for `type_name`, or for
/// the provider's first GPU type when `type_name` is `None`.
pub async fn region_choices(
    provider_id: String,
    type_name: Option<String>,
    spot: bool,
) -> RegaResult<RegionChoices> {
    let entry = registry::entry(&provider_id)?;
    let provider = entry.provider();
    let type_name = match type_name {
        Some(t) => t,
        None => provider
            .machine_types()
            .into_iter()
            .find(|t| t.is_gpu())
            .map(|t| t.type_name)
            .ok_or_else(|| format!("{provider_id} offers no GPU type"))?,
    };
    let candidates = region::candidates(provider, &type_name, spot, |id| entry.rtt(id)).await?;
    Ok(choices_view(candidates))
}

fn choices_view(candidates: region::Candidates) -> RegionChoices {
    let choices = region::choose(candidates.found);
    RegionChoices {
        recommended: choices.recommended.map(option),
        lowest_latency: choices.lowest_latency.map(option),
        cheapest: choices.cheapest.map(option),
        all: choices.all.into_iter().map(option).collect(),
        unreachable: candidates
            .unreachable
            .into_iter()
            .map(|r| r.display_name)
            .collect(),
    }
}

pub async fn list_machines(provider_id: String) -> RegaResult<Vec<MachineSummary>> {
    let entry = registry::entry(&provider_id)?;
    let provider = entry.provider();
    let (machines, regions) = futures::join!(provider.list_machines(), provider.regions());
    // Region names are only for display; ids do when they can't be read.
    let (machines, regions) = (machines?, regions.unwrap_or_default());
    let types = provider.machine_types();
    Ok(machines
        .into_iter()
        .map(|m| summary(m, &types, |id| region_name(&regions, id)))
        .collect())
}

fn region_name(regions: &[Region], id: &str) -> String {
    regions
        .iter()
        .find(|r| r.id == id)
        .map_or_else(|| id.to_string(), |r| r.display_name.clone())
}

/// One machine as the UI shows it, after an action changed it. The action
/// already happened, so a region list that can't be read only costs the
/// region's display name.
async fn view(provider: &dyn CloudProvider, m: Machine) -> RegaResult<MachineSummary> {
    let regions = provider.regions().await.unwrap_or_default();
    Ok(summary(m, &provider.machine_types(), |id| {
        region_name(&regions, id)
    }))
}

/// Creates a machine and starts it. Takes several minutes: Windows has to
/// boot and make its Administrator password first.
pub async fn launch_machine(
    provider_id: String,
    name: String,
    region_id: String,
    type_name: String,
    spot: bool,
    wireguard: bool,
) -> RegaResult<MachineSummary> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("give the machine a name".into());
    }
    let entry = registry::entry(&provider_id)?;
    let provider = entry.provider();
    let machine = provider
        .launch(&LaunchRequest {
            name,
            region_id,
            type_name,
            disk_gib: DEFAULT_DISK_GIB,
            spot,
            wireguard,
        })
        .await?;
    view(provider, machine).await
}

pub async fn start_machine(provider_id: String, machine_id: String) -> RegaResult<MachineSummary> {
    let entry = registry::entry(&provider_id)?;
    let machine = entry.provider().start(&machine_id).await?;
    view(entry.provider(), machine).await
}

pub async fn stop_machine(provider_id: String, machine_id: String) -> RegaResult<MachineSummary> {
    let entry = registry::entry(&provider_id)?;
    let machine = entry.provider().stop(&machine_id).await?;
    view(entry.provider(), machine).await
}

/// Changes a stopped machine's type (Switch to GPU / Switch to CPU).
pub async fn resize_machine(
    provider_id: String,
    machine_id: String,
    type_name: String,
) -> RegaResult<MachineSummary> {
    let entry = registry::entry(&provider_id)?;
    let machine = entry.provider().resize(&machine_id, &type_name).await?;
    view(entry.provider(), machine).await
}

/// Deletes the machine and its disks. The game library in object storage
/// stays.
pub async fn destroy_machine(provider_id: String, machine_id: String) -> RegaResult<()> {
    let entry = registry::entry(&provider_id)?;
    Ok(entry.provider().destroy(&machine_id).await?)
}

/// Leftover resources in the account that belong to no machine.
pub async fn list_orphans(provider_id: String) -> RegaResult<Vec<OrphanSummary>> {
    let entry = registry::entry(&provider_id)?;
    let provider = entry.provider();
    let (orphans, regions) = futures::join!(provider.orphans(), provider.regions());
    let (orphans, regions) = (orphans?, regions.unwrap_or_default());
    Ok(orphans
        .into_iter()
        .map(|o| orphan_view(o, &regions))
        .collect())
}

/// Deletes the given leftovers, skipping any that a machine has claimed
/// since they were listed.
pub async fn delete_orphans(provider_id: String, orphans: Vec<OrphanSummary>) -> RegaResult<()> {
    let entry = registry::entry(&provider_id)?;
    let orphans: Vec<Orphan> = orphans.into_iter().map(orphan_from).collect();
    Ok(entry.provider().delete_orphans(&orphans).await?)
}

fn orphan_view(o: Orphan, regions: &[Region]) -> OrphanSummary {
    OrphanSummary {
        kind: match o.kind {
            ResourceKind::Instance => OrphanKind::Instance,
            ResourceKind::Volume => OrphanKind::Volume,
            ResourceKind::SecurityGroup => OrphanKind::SecurityGroup,
            ResourceKind::KeyPair => OrphanKind::KeyPair,
            ResourceKind::Alarm => OrphanKind::Alarm,
        },
        region_name: region_name(regions, &o.region_id),
        id: o.id,
        region_id: o.region_id,
        machine_id: o.machine_id,
        name: o.name,
    }
}

fn orphan_from(o: OrphanSummary) -> Orphan {
    Orphan {
        kind: match o.kind {
            OrphanKind::Instance => ResourceKind::Instance,
            OrphanKind::Volume => ResourceKind::Volume,
            OrphanKind::SecurityGroup => ResourceKind::SecurityGroup,
            OrphanKind::KeyPair => ResourceKind::KeyPair,
            OrphanKind::Alarm => ResourceKind::Alarm,
        },
        id: o.id,
        region_id: o.region_id,
        machine_id: o.machine_id,
        name: o.name,
    }
}

fn offer(t: &MachineType, price_per_hour: Option<f64>) -> MachineOffer {
    MachineOffer {
        type_name: t.type_name.clone(),
        vcpus: t.spec.vcpus,
        ram_gib: t.spec.ram_gib,
        gpu_model: t.spec.gpu.as_ref().map(|g| g.model.clone()),
        vram_gib: t.spec.gpu.as_ref().map(|g| g.vram_gib),
        disk_gib: DEFAULT_DISK_GIB,
        price_per_hour,
    }
}

fn option(c: RegionCandidate) -> RegionOption {
    RegionOption {
        rtt_ms: c.rtt.map(|d| d.as_millis().min(u32::MAX as u128) as u32),
        region_id: c.region.id,
        display_name: c.region.display_name,
        price_per_hour: c.price_per_hour,
        one_off_cost: c.one_off_cost,
    }
}

/// Where "Switch to GPU/CPU" goes: a CPU machine to the first GPU type, a
/// GPU machine to the smallest CPU type that runs Windows comfortably.
fn switch_type(m: &Machine, types: &[MachineType]) -> Option<String> {
    if m.spot {
        return None;
    }
    let target = if m.machine_type.is_gpu() {
        types
            .iter()
            .filter(|t| !t.is_gpu() && t.spec.ram_gib >= 4.0)
            .min_by(|a, b| a.spec.ram_gib.total_cmp(&b.spec.ram_gib))
    } else {
        types.iter().find(|t| t.is_gpu())
    };
    target.map(|t| t.type_name.clone())
}

fn summary(
    m: Machine,
    types: &[MachineType],
    region_name: impl Fn(&str) -> String,
) -> MachineSummary {
    let mut o = offer(&m.machine_type, None);
    o.disk_gib = m.disk_gib;
    MachineSummary {
        switch_type: switch_type(&m, types),
        offer: o,
        status: match m.state {
            MachineState::Pending => MachineStatus::Pending,
            MachineState::Running => MachineStatus::Running,
            MachineState::Stopping => MachineStatus::Stopping,
            MachineState::Stopped => MachineStatus::Stopped,
            MachineState::Terminated => MachineStatus::Terminated,
        },
        region_name: region_name(&m.region_id),
        busy: m.busy,
        id: m.id,
        name: m.name,
        region_id: m.region_id,
        spot: m.spot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    /// Every test runs on the same demo provider.
    fn demo() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| registry::set(vec![registry::Entry::demo("tests")]));
    }

    #[test]
    fn demo_entries_say_why() {
        let p = info(&registry::Entry::demo("No AWS credentials found."));
        assert_eq!(p.id, "aws");
        assert_eq!(p.name, "AWS");
        assert!(p.demo);
        assert_eq!(p.notice.as_deref(), Some("No AWS credentials found."));
    }

    #[tokio::test]
    async fn spot_offers_only_gpu_types() {
        demo();
        let all = machine_offers("aws".into(), "eu-north-1".into(), false)
            .await
            .unwrap();
        assert!(all.iter().any(|o| !o.is_gpu()));
        assert!(all.iter().all(|o| o.price_per_hour.is_some()));

        let spot = machine_offers("aws".into(), "eu-north-1".into(), true)
            .await
            .unwrap();
        assert!(!spot.is_empty());
        assert!(spot
            .iter()
            .all(|o| o.is_gpu() && o.price_per_hour.is_some()));
    }

    #[tokio::test]
    async fn region_choices_are_filled() {
        demo();
        let c = region_choices("aws".into(), None, false).await.unwrap();
        assert!(c.recommended.is_some());
        assert!(c.unreachable.is_empty());
        let named = region_choices("aws".into(), Some("g4dn.xlarge".into()), false)
            .await
            .unwrap();
        assert_eq!(named.all.len(), c.all.len());
        assert!(c.all.iter().all(|r| r.rtt_ms.is_some()));
    }

    #[test]
    fn unreachable_regions_are_shown_by_name() {
        let view = choices_view(region::Candidates {
            found: vec![],
            unreachable: vec![rega_core::aws::region(
                "ap-northeast-1",
                "Asia Pacific (Tokyo)",
            )],
        });
        assert!(view.all.is_empty());
        assert_eq!(view.unreachable, vec!["Asia Pacific (Tokyo)".to_string()]);
    }

    fn machine(type_index: usize, spot: bool) -> Machine {
        Machine {
            id: "i-1".into(),
            name: "Gaming".into(),
            region_id: "us-east-1".into(),
            machine_type: rega_core::aws::catalog().remove(type_index),
            disk_gib: 128,
            spot,
            state: MachineState::Running,
            busy: false,
        }
    }

    #[test]
    fn machines_carry_their_region_name() {
        let names = |id: &str| format!("name of {id}");
        let s = summary(machine(0, false), &[], names);
        assert_eq!(s.region_name, "name of us-east-1");
        assert!(matches!(s.status, MachineStatus::Running));
    }

    #[test]
    fn switching_goes_between_gpu_and_a_roomy_cpu_type() {
        let types = rega_core::aws::catalog();
        let gpu = machine(0, false);
        assert_eq!(switch_type(&gpu, &types).as_deref(), Some("t3.medium"));
        let cpu = machine(1, false);
        assert_eq!(switch_type(&cpu, &types).as_deref(), Some("g4dn.xlarge"));
        assert_eq!(switch_type(&machine(0, true), &types), None);
    }

    #[tokio::test]
    async fn machine_actions_go_through_the_provider() {
        demo();
        let m = launch_machine(
            "aws".into(),
            " Bridge test ".into(),
            "eu-north-1".into(),
            "t3.medium".into(),
            false,
            true,
        )
        .await
        .unwrap();
        assert_eq!(m.name, "Bridge test");
        assert_eq!(m.region_name, "Europe (Stockholm)");
        assert_eq!(m.switch_type.as_deref(), Some("g4dn.xlarge"));
        let m = stop_machine("aws".into(), m.id).await.unwrap();
        assert!(matches!(m.status, MachineStatus::Stopped));
        let m = resize_machine("aws".into(), m.id, "g4dn.xlarge".into())
            .await
            .unwrap();
        assert_eq!(m.offer.gpu_model.as_deref(), Some("NVIDIA T4"));
        let m = start_machine("aws".into(), m.id).await.unwrap();
        assert!(matches!(m.status, MachineStatus::Running));
        destroy_machine("aws".into(), m.id.clone()).await.unwrap();
        let left = list_machines("aws".into()).await.unwrap();
        assert!(left.iter().all(|x| x.id != m.id));
    }

    #[tokio::test]
    async fn a_machine_needs_a_name() {
        demo();
        let err = launch_machine(
            "aws".into(),
            "  ".into(),
            "eu-north-1".into(),
            "t3.medium".into(),
            false,
            true,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(err.message, "give the machine a name");
    }

    #[test]
    fn orphans_round_trip_through_the_view() {
        let regions = [rega_core::aws::region("eu-north-1", "Europe (Stockholm)")];
        for kind in [
            ResourceKind::Instance,
            ResourceKind::Volume,
            ResourceKind::SecurityGroup,
            ResourceKind::KeyPair,
            ResourceKind::Alarm,
        ] {
            let o = Orphan {
                kind,
                id: "x-1".into(),
                region_id: "eu-north-1".into(),
                machine_id: Some("m".into()),
                name: Some("rega-m".into()),
            };
            let view = orphan_view(o.clone(), &regions);
            assert_eq!(view.region_name, "Europe (Stockholm)");
            assert_eq!(orphan_from(view), o);
        }
    }

    #[tokio::test]
    async fn demo_has_no_orphans() {
        demo();
        assert!(list_orphans("aws".into()).await.unwrap().is_empty());
        delete_orphans("aws".into(), vec![]).await.unwrap();
    }

    #[tokio::test]
    async fn errors_are_plain_messages() {
        demo();
        let err = list_machines("nope".into()).await.err().unwrap();
        assert_eq!(err.message, "unknown provider nope");
    }

    #[tokio::test]
    async fn demo_latency_never_changes() {
        demo();
        assert!(!probe_latency("aws".into()).await.unwrap());
    }
}
