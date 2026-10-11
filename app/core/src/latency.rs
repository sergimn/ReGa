//! Round-trip time to a region, measured with TCP handshakes to the region's
//! API endpoint. No machine is launched for this.

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::Region;

pub const PROBE_PORT: u16 = 443;
pub const PROBE_SAMPLES: usize = 5;
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Median of the samples, or `None` when there are none.
pub fn median(mut samples: Vec<Duration>) -> Option<Duration> {
    if samples.is_empty() {
        return None;
    }
    samples.sort();
    let mid = samples.len() / 2;
    Some(if samples.len().is_multiple_of(2) {
        (samples[mid - 1] + samples[mid]) / 2
    } else {
        samples[mid]
    })
}

/// Times `samples` TCP handshakes to `host:port` and returns the median.
/// Failed handshakes are dropped; `None` means none succeeded.
///
/// This blocks, so callers run it off the UI thread.
pub fn measure_rtt(host: &str, port: u16, samples: usize, timeout: Duration) -> Option<Duration> {
    let addr = (host, port).to_socket_addrs().ok()?.next()?;
    let times = (0..samples)
        .filter_map(|_| {
            let start = Instant::now();
            TcpStream::connect_timeout(&addr, timeout)
                .ok()
                .map(|_| start.elapsed())
        })
        .collect();
    median(times)
}

/// Latest RTT per region id, filled in by background probes.
#[derive(Debug, Clone, Default)]
pub struct RttCache {
    inner: Arc<Mutex<HashMap<String, Duration>>>,
}

impl RttCache {
    pub fn get(&self, region_id: &str) -> Option<Duration> {
        self.inner.lock().unwrap().get(region_id).copied()
    }

    pub fn insert(&self, region_id: &str, rtt: Duration) {
        self.inner.lock().unwrap().insert(region_id.into(), rtt);
    }

    /// Probes every region on its own thread and records results as they
    /// arrive. Returns the handles so callers (and tests) can wait.
    pub fn measure_in_background(&self, regions: Vec<Region>) -> Vec<thread::JoinHandle<()>> {
        regions
            .into_iter()
            .map(|region| {
                let cache = self.clone();
                thread::spawn(move || {
                    let host = region.latency_probe_host.as_str();
                    if let Some(rtt) = measure_rtt(host, PROBE_PORT, PROBE_SAMPLES, PROBE_TIMEOUT) {
                        cache.insert(&region.id, rtt);
                    }
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    #[test]
    fn median_of_odd_and_even_counts() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![ms(30), ms(10), ms(20)]), Some(ms(20)));
        assert_eq!(median(vec![ms(40), ms(10), ms(20), ms(30)]), Some(ms(25)));
    }

    #[test]
    fn measures_a_local_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let rtt = measure_rtt("127.0.0.1", port, 3, PROBE_TIMEOUT);
        assert!(rtt.is_some_and(|d| d < Duration::from_secs(1)));
    }

    #[test]
    fn cache_stores_and_returns_measurements() {
        let cache = RttCache::default();
        assert_eq!(cache.get("eu-north-1"), None);
        cache.insert("eu-north-1", ms(38));
        assert_eq!(cache.clone().get("eu-north-1"), Some(ms(38)));
    }

    #[test]
    fn background_probe_skips_unresolvable_hosts() {
        let cache = RttCache::default();
        let region = Region {
            id: "nowhere".into(),
            display_name: "Nowhere".into(),
            latency_probe_host: "host.invalid".into(),
        };
        for h in cache.measure_in_background(vec![region]) {
            h.join().unwrap();
        }
        assert_eq!(cache.get("nowhere"), None);
    }

    #[test]
    fn unreachable_host_gives_none() {
        // Port 0 can never accept a connection.
        assert_eq!(measure_rtt("127.0.0.1", 0, 2, ms(200)), None);
    }
}
