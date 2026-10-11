//! The providers the app knows about, built once at startup.

use std::sync::OnceLock;
use std::time::Duration;

use rega_core::latency::RttCache;
use rega_core::mock::MockProvider;
use rega_core::{CloudProvider, Region};

// `Live` is used once the AWS provider talks to a real account.
#[allow(dead_code)]
pub enum Entry {
    /// Sample data, used until real credentials are configured.
    Demo(MockProvider),
    Live {
        provider: Box<dyn CloudProvider>,
        rtt: RttCache,
    },
}

impl Entry {
    pub fn provider(&self) -> &dyn CloudProvider {
        match self {
            Entry::Demo(p) => p,
            Entry::Live { provider, .. } => provider.as_ref(),
        }
    }

    pub fn is_demo(&self) -> bool {
        matches!(self, Entry::Demo(_))
    }

    pub fn rtt(&self, region_id: &str) -> Option<Duration> {
        match self {
            Entry::Demo(p) => p.simulated_rtt(region_id),
            Entry::Live { rtt, .. } => rtt.get(region_id),
        }
    }

    /// Starts measuring latency to `regions` in the background (live only).
    pub fn probe(&self, regions: Vec<Region>) {
        if let Entry::Live { rtt, .. } = self {
            rtt.measure_in_background(regions);
        }
    }
}

static ENTRIES: OnceLock<Vec<Entry>> = OnceLock::new();

pub fn entries() -> &'static [Entry] {
    // AWS calls are not wired up yet, so AWS runs on demo data for now.
    ENTRIES.get_or_init(|| vec![Entry::Demo(MockProvider::demo_aws())])
}

pub fn entry(provider_id: &str) -> anyhow::Result<&'static Entry> {
    entries()
        .iter()
        .find(|e| e.provider().id() == provider_id)
        .ok_or_else(|| anyhow::anyhow!("unknown provider {provider_id}"))
}
