//! Region picker: combines measured latency with prices and quotas.
//!
//! The UI shows at most three choices (Recommended, Lowest latency and
//! Cheapest) and can expand to the full list.

use std::time::Duration;

use futures::stream::{self, StreamExt};

use crate::model::Region;
use crate::provider::{CloudProvider, ProviderError, ProviderResult};

/// A region is "close enough" for Recommended when its RTT is within this
/// margin of the closest region.
pub const RECOMMENDED_RTT_MARGIN: Duration = Duration::from_millis(10);

/// One-off costs (copying the game library) are spread over this many hours
/// of play when comparing regions by price.
pub const ONE_OFF_AMORTIZATION_HOURS: f64 = 20.0;

#[derive(Debug, Clone, PartialEq)]
pub struct RegionCandidate {
    pub region: Region,
    /// `None` until the background measurement for this region finishes.
    pub rtt: Option<Duration>,
    pub price_per_hour: f64,
    /// One-off cost of copying the game library here, in USD.
    pub one_off_cost: f64,
}

impl RegionCandidate {
    /// Hourly price with one-off costs folded in, used for comparisons.
    pub fn effective_per_hour(&self) -> f64 {
        self.price_per_hour + self.one_off_cost / ONE_OFF_AMORTIZATION_HOURS
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct RegionChoices {
    pub recommended: Option<RegionCandidate>,
    pub lowest_latency: Option<RegionCandidate>,
    pub cheapest: Option<RegionCandidate>,
    /// Every launchable region, closest first (unmeasured ones last).
    pub all: Vec<RegionCandidate>,
}

fn cheapest_of<'a>(it: impl Iterator<Item = &'a RegionCandidate>) -> Option<RegionCandidate> {
    it.min_by(|a, b| a.effective_per_hour().total_cmp(&b.effective_per_hour()))
        .cloned()
}

/// Picks Recommended, Lowest latency and Cheapest from launchable candidates.
pub fn choose(mut candidates: Vec<RegionCandidate>) -> RegionChoices {
    candidates.sort_by(|a, b| match (a.rtt, b.rtt) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.region.id.cmp(&b.region.id),
    });

    let lowest_latency = candidates.iter().find(|c| c.rtt.is_some()).cloned();
    let cheapest = cheapest_of(candidates.iter());
    let recommended = match lowest_latency.as_ref().and_then(|c| c.rtt) {
        Some(min_rtt) => cheapest_of(
            candidates
                .iter()
                .filter(|c| c.rtt.is_some_and(|r| r <= min_rtt + RECOMMENDED_RTT_MARGIN)),
        ),
        // Nothing measured yet: price is all we have.
        None => cheapest.clone(),
    };

    RegionChoices {
        recommended,
        lowest_latency,
        cheapest,
        all: candidates,
    }
}

/// GPU vCPUs of quota a machine of `type_name` needs. CPU types count as the
/// smallest GPU type, because the CPU phase ends with a resize to GPU.
fn gpu_vcpus_needed(provider: &dyn CloudProvider, type_name: &str) -> ProviderResult<u32> {
    let types = provider.machine_types();
    let t = types
        .iter()
        .find(|t| t.type_name == type_name)
        .ok_or_else(|| ProviderError::UnknownMachineType(type_name.into()))?;
    if t.is_gpu() {
        return Ok(t.spec.vcpus);
    }
    Ok(types
        .iter()
        .filter(|t| t.is_gpu())
        .map(|t| t.spec.vcpus)
        .min()
        .unwrap_or(0))
}

/// At most this many regions are queried at once.
const MAX_CONCURRENT_REGIONS: usize = 8;

/// Hourly price and one-off cost in a region, or `None` when the machine
/// can't be launched there.
type RegionOffer = Option<(f64, f64)>;

/// Launchable regions for one machine type, plus the regions that couldn't
/// be checked (their queries failed), so the UI can say so.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Candidates {
    pub found: Vec<RegionCandidate>,
    pub unreachable: Vec<Region>,
}

/// Builds candidates for `type_name` from a provider, skipping regions where
/// the free GPU quota doesn't cover the machine, or where the type isn't
/// sold the chosen way (spot or on-demand). Regions are queried a few at a
/// time. Regions whose queries fail are listed in `unreachable`; only when
/// every region fails is it an error.
///
/// `rtt_of` looks up the cached measurement for a region id.
pub async fn candidates(
    provider: &dyn CloudProvider,
    type_name: &str,
    spot: bool,
    rtt_of: impl Fn(&str) -> Option<Duration>,
) -> ProviderResult<Candidates> {
    let needed = gpu_vcpus_needed(provider, type_name)?;
    let regions = provider.regions().await?;
    let total = regions.len();
    let results: Vec<(Region, ProviderResult<RegionOffer>)> = stream::iter(regions)
        .map(|region| async move {
            let result = async {
                let (quota, price, one_off_cost) = futures::try_join!(
                    provider.gpu_quota_vcpus(&region.id, spot),
                    provider.price(&region.id, type_name),
                    provider.library_copy_cost(&region.id),
                )?;
                let price_per_hour = price.and_then(|p| p.per_hour(spot));
                Ok(match price_per_hour {
                    Some(price_per_hour) if quota >= needed && quota > 0 => {
                        Some((price_per_hour, one_off_cost))
                    }
                    _ => None,
                })
            }
            .await;
            (region, result)
        })
        .buffer_unordered(MAX_CONCURRENT_REGIONS)
        .collect()
        .await;

    let mut out = Candidates::default();
    let mut first_error = None;
    for (region, result) in results {
        match result {
            Ok(Some((price_per_hour, one_off_cost))) => out.found.push(RegionCandidate {
                rtt: rtt_of(&region.id),
                region,
                price_per_hour,
                one_off_cost,
            }),
            Ok(None) => {}
            Err(e) => {
                first_error.get_or_insert(e);
                out.unreachable.push(region);
            }
        }
    }
    if let Some(e) = first_error.filter(|_| out.unreachable.len() == total) {
        return Err(e);
    }
    out.unreachable.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, rtt_ms: Option<u64>, price: f64, one_off: f64) -> RegionCandidate {
        RegionCandidate {
            region: Region {
                id: id.into(),
                display_name: id.into(),
                latency_probe_host: format!("{id}.example"),
            },
            rtt: rtt_ms.map(Duration::from_millis),
            price_per_hour: price,
            one_off_cost: one_off,
        }
    }

    #[test]
    fn empty_input_gives_no_choices() {
        assert_eq!(choose(vec![]), RegionChoices::default());
    }

    #[test]
    fn recommended_is_cheapest_within_margin_of_closest() {
        let choices = choose(vec![
            cand("near", Some(20), 0.70, 0.0),
            cand("near-cheap", Some(29), 0.60, 0.0),
            cand("far-cheapest", Some(80), 0.40, 0.0),
        ]);
        assert_eq!(choices.lowest_latency.unwrap().region.id, "near");
        assert_eq!(choices.recommended.unwrap().region.id, "near-cheap");
        assert_eq!(choices.cheapest.unwrap().region.id, "far-cheapest");
    }

    #[test]
    fn margin_is_inclusive_and_strict_beyond() {
        let choices = choose(vec![
            cand("a", Some(20), 0.70, 0.0),
            cand("b", Some(31), 0.50, 0.0),
        ]);
        assert_eq!(choices.recommended.unwrap().region.id, "a");

        let choices = choose(vec![
            cand("a", Some(20), 0.70, 0.0),
            cand("b", Some(30), 0.50, 0.0),
        ]);
        assert_eq!(choices.recommended.unwrap().region.id, "b");
    }

    #[test]
    fn one_off_library_copy_is_folded_into_price() {
        // b is cheaper per hour, but copying the library there costs $10,
        // which adds $0.50/h over 20 hours.
        let choices = choose(vec![
            cand("a", Some(20), 0.70, 0.0),
            cand("b", Some(25), 0.30, 10.0),
        ]);
        assert_eq!(choices.cheapest.unwrap().region.id, "a");
    }

    #[test]
    fn unmeasured_regions_fall_back_to_price_and_sort_last() {
        let choices = choose(vec![
            cand("z-unmeasured", None, 0.40, 0.0),
            cand("a-unmeasured", None, 0.50, 0.0),
        ]);
        assert_eq!(choices.lowest_latency, None);
        assert_eq!(choices.recommended.unwrap().region.id, "z-unmeasured");

        let choices = choose(vec![
            cand("unmeasured", None, 0.10, 0.0),
            cand("measured", Some(40), 0.50, 0.0),
        ]);
        assert_eq!(choices.all[0].region.id, "measured");
        assert_eq!(choices.recommended.unwrap().region.id, "measured");
        assert_eq!(choices.cheapest.unwrap().region.id, "unmeasured");
    }
}
