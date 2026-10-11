//! Small time-limited cache for answers that change slowly (prices, quotas).

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct TtlCache<K, V> {
    ttl: Duration,
    entries: Mutex<HashMap<K, (Instant, V)>>,
}

impl<K: Eq + Hash, V: Clone> TtlCache<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn get(&self, key: &K) -> Option<V> {
        let entries = self.entries.lock().unwrap();
        let (at, value) = entries.get(key)?;
        (at.elapsed() < self.ttl).then(|| value.clone())
    }

    pub fn insert(&self, key: K, value: V) {
        self.entries
            .lock()
            .unwrap()
            .insert(key, (Instant::now(), value));
    }

    /// Returns the cached value, or computes, stores and returns it. Errors
    /// aren't cached.
    pub async fn get_or_try<E, F>(&self, key: K, compute: F) -> Result<V, E>
    where
        F: std::future::Future<Output = Result<V, E>>,
    {
        if let Some(v) = self.get(&key) {
            return Ok(v);
        }
        let v = compute.await?;
        self.insert(key, v.clone());
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caches_successes_until_they_expire() {
        let cache = TtlCache::new(Duration::from_secs(60));
        assert_eq!(cache.get_or_try("k", async { Ok::<_, ()>(1) }).await, Ok(1));
        assert_eq!(cache.get_or_try("k", async { Ok::<_, ()>(2) }).await, Ok(1));

        let expired = TtlCache::new(Duration::ZERO);
        expired.insert("k", 1);
        assert_eq!(expired.get(&"k"), None);
    }

    #[tokio::test]
    async fn errors_are_not_cached() {
        let cache = TtlCache::new(Duration::from_secs(60));
        assert_eq!(
            cache.get_or_try("k", async { Err::<u32, _>("no") }).await,
            Err("no")
        );
        assert_eq!(
            cache.get_or_try("k", async { Ok::<_, &str>(3) }).await,
            Ok(3)
        );
    }
}
