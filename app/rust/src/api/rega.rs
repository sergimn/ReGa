//! Everything the Flutter UI can call. Types here are flat views of the core
//! model, shaped for display.

use std::fmt::Display;

use futures::future::try_join_all;
use rega_core::region::{self, RegionCandidate};
use rega_core::{Machine, MachineState, MachineType, DEFAULT_DISK_GIB};

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
    pub offer: MachineOffer,
    pub spot: bool,
    pub status: MachineStatus,
}

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// Providers for the side menu.
#[flutter_rust_bridge::frb(sync)]
pub fn list_providers() -> Vec<ProviderInfo> {
    registry::entries()
        .iter()
        .map(|e| ProviderInfo {
            id: e.provider().id().into(),
            name: e.provider().display_name().into(),
            demo: e.is_demo(),
        })
        .collect()
}

/// Machine types sold in `region_id` the chosen way, priced. With `spot` on
/// that means GPU types only.
pub async fn machine_offers(
    provider_id: String,
    region_id: String,
    spot: bool,
) -> RegaResult<Vec<MachineOffer>> {
    let provider = registry::entry(&provider_id)?.provider();
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
    let choices = region::choose(candidates.found);
    Ok(RegionChoices {
        recommended: choices.recommended.map(option),
        lowest_latency: choices.lowest_latency.map(option),
        cheapest: choices.cheapest.map(option),
        all: choices.all.into_iter().map(option).collect(),
        unreachable: candidates
            .unreachable
            .into_iter()
            .map(|r| r.display_name)
            .collect(),
    })
}

pub async fn list_machines(provider_id: String) -> RegaResult<Vec<MachineSummary>> {
    let provider = registry::entry(&provider_id)?.provider();
    let machines = provider.list_machines().await?;
    Ok(machines.into_iter().map(summary).collect())
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

fn summary(m: Machine) -> MachineSummary {
    let mut o = offer(&m.machine_type, None);
    o.disk_gib = m.disk_gib;
    MachineSummary {
        offer: o,
        status: match m.state {
            MachineState::Pending => MachineStatus::Pending,
            MachineState::Running => MachineStatus::Running,
            MachineState::Stopping => MachineStatus::Stopping,
            MachineState::Stopped => MachineStatus::Stopped,
            MachineState::Terminated => MachineStatus::Terminated,
        },
        id: m.id,
        name: m.name,
        region_id: m.region_id,
        spot: m.spot,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aws_is_listed() {
        let providers = list_providers();
        assert_eq!(providers[0].id, "aws");
        assert_eq!(providers[0].name, "AWS");
    }

    #[tokio::test]
    async fn spot_offers_only_gpu_types() {
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
        let c = region_choices("aws".into(), None, false).await.unwrap();
        assert!(c.recommended.is_some());
        assert!(c.unreachable.is_empty());
        let named = region_choices("aws".into(), Some("g4dn.xlarge".into()), false)
            .await
            .unwrap();
        assert_eq!(named.all.len(), c.all.len());
        assert!(c.all.iter().all(|r| r.rtt_ms.is_some()));
    }

    #[tokio::test]
    async fn errors_are_plain_messages() {
        let err = list_machines("nope".into()).await.err().unwrap();
        assert_eq!(err.message, "unknown provider nope");
    }

    #[tokio::test]
    async fn demo_latency_never_changes() {
        assert!(!probe_latency("aws".into()).await.unwrap());
    }
}
