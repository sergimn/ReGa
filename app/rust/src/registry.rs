//! The providers the app knows about. [`connect`] signs in to each cloud at
//! startup; a cloud without credentials shows sample data instead.

use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;

use futures::channel::oneshot;
use rega_core::aws::{AwsProvider, ConnectOptions};
use rega_core::latency::RttCache;
use rega_core::mock::MockProvider;
use rega_core::{CloudProvider, ProviderError, Region};

/// Setting this to anything makes the app use sample data even when AWS
/// credentials exist (for screenshots and the integration test).
pub const DEMO_ENV: &str = "REGA_DEMO";

pub enum Entry {
    /// Sample data, with the reason the real account isn't used.
    Demo {
        provider: MockProvider,
        reason: String,
    },
    Live {
        provider: Box<dyn CloudProvider>,
        rtt: RttCache,
    },
}

impl Entry {
    pub fn demo(reason: impl Into<String>) -> Self {
        Entry::Demo {
            provider: MockProvider::demo_aws(),
            reason: reason.into(),
        }
    }

    pub fn provider(&self) -> &dyn CloudProvider {
        match self {
            Entry::Demo { provider, .. } => provider,
            Entry::Live { provider, .. } => provider.as_ref(),
        }
    }

    /// Why sample data is shown, or `None` for a real account.
    pub fn demo_reason(&self) -> Option<&str> {
        match self {
            Entry::Demo { reason, .. } => Some(reason),
            Entry::Live { .. } => None,
        }
    }

    pub fn rtt(&self, region_id: &str) -> Option<Duration> {
        match self {
            Entry::Demo { provider, .. } => provider.simulated_rtt(region_id),
            Entry::Live { rtt, .. } => rtt.get(region_id),
        }
    }

    /// Measures latency to `regions` off the async executor and resolves
    /// when every probe is done, with `true` if any region was measured.
    /// Demo data never changes, so it's `false`.
    pub async fn probe(&self, regions: Vec<Region>) -> bool {
        let Entry::Live { rtt, .. } = self else {
            return false;
        };
        let (done, finished) = oneshot::channel();
        let handles = rtt.measure_in_background(regions);
        thread::spawn(move || {
            // Join every probe (no short-circuit) so this resolves only
            // once all of them are done.
            let results: Vec<bool> = handles
                .into_iter()
                .map(|h| h.join().unwrap_or(false))
                .collect();
            let measured = results.contains(&true);
            let _ = done.send(measured);
        });
        finished.await.unwrap_or(false)
    }
}

static ENTRIES: RwLock<Vec<Arc<Entry>>> = RwLock::new(Vec::new());

/// The AWS entry: the real account when credentials work, else demo data
/// with the reason.
async fn connect_aws() -> Entry {
    if std::env::var_os(DEMO_ENV).is_some() {
        return Entry::demo(format!("{DEMO_ENV} is set"));
    }
    match AwsProvider::connect(ConnectOptions::from_env()).await {
        Ok(aws) => Entry::Live {
            provider: Box::new(aws),
            rtt: RttCache::default(),
        },
        Err(ProviderError::MissingCredentials(_)) => Entry::demo(
            "No AWS credentials found. Add an access key to ~/.aws/credentials \
             (or set AWS_PROFILE), then reconnect.",
        ),
        Err(e) => Entry::demo(format!("Couldn't sign in to AWS: {e}")),
    }
}

/// Signs in to every provider, replacing what was there.
pub async fn connect() -> Vec<Arc<Entry>> {
    set(vec![connect_aws().await]);
    entries()
}

pub fn set(entries: Vec<Entry>) {
    *ENTRIES.write().unwrap() = entries.into_iter().map(Arc::new).collect();
}

pub fn entries() -> Vec<Arc<Entry>> {
    ENTRIES.read().unwrap().clone()
}

pub fn entry(provider_id: &str) -> Result<Arc<Entry>, String> {
    entries()
        .into_iter()
        .find(|e| e.provider().id() == provider_id)
        .ok_or_else(|| format!("unknown provider {provider_id}"))
}
