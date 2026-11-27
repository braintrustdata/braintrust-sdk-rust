//! Bounded successful resolution shared by compatible authentication contexts.
use indexmap::IndexMap;
use std::hash::Hash;
use std::ops::Deref;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::OnceCell;

/// A single-flight cache with least-recently-used eviction of idle entries.
struct Cache<K, V> {
    entries: Mutex<IndexMap<K, Arc<OnceCell<V>>>>,
    capacity: usize,
}
impl<K: Hash + Eq, V> Cache<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(IndexMap::new()),
            capacity,
        }
    }
    pub fn entry(&self, key: K) -> CacheEntry<'_, K, V> {
        let mut entries = self.entries.lock().unwrap();
        let cell = if let Some(index) = entries.get_index_of(&key) {
            let last = entries.len() - 1;
            entries.move_index(index, last);
            entries[last].clone()
        } else {
            let cell = Arc::new(OnceCell::new());
            entries.insert(key, cell.clone());
            self.trim(&mut entries);
            cell
        };
        CacheEntry {
            cache: self,
            cell: Some(cell),
        }
    }

    fn trim(&self, entries: &mut IndexMap<K, Arc<OnceCell<V>>>) {
        while entries.len() > self.capacity {
            // Keep cells in use and prefer evicting failed lookups over cached results.
            let victim = entries
                .values()
                .enumerate()
                .filter(|(_, cell)| Arc::strong_count(cell) == 1)
                .min_by_key(|(_, cell)| cell.initialized());
            let Some((index, _)) = victim else {
                break;
            };
            entries.shift_remove_index(index);
        }
    }
}

struct CacheEntry<'a, K: Hash + Eq, V> {
    cache: &'a Cache<K, V>,
    cell: Option<Arc<OnceCell<V>>>,
}

impl<K: Hash + Eq, V> Deref for CacheEntry<'_, K, V> {
    type Target = OnceCell<V>;

    fn deref(&self) -> &Self::Target {
        // The cell is present until the handle is dropped.
        self.cell.as_deref().expect("live cache entry")
    }
}

impl<K: Hash + Eq, V> Drop for CacheEntry<'_, K, V> {
    fn drop(&mut self) {
        drop(self.cell.take());
        self.cache.trim(&mut self.cache.entries.lock().unwrap());
    }
}

/// The organization a request acts for. When neither its ID nor its name is
/// known, the credential stands in, so unrelated tenants never share a cached
/// resolution or a rate-limit pause.
#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) enum OrgIdentity {
    Id(String),
    Name(String),
    Credential(String),
}
impl OrgIdentity {
    pub fn new(org_id: &str, org_name: Option<&str>, token: &str) -> Self {
        if !org_id.is_empty() {
            Self::Id(org_id.to_owned())
        } else if let Some(name) = org_name.filter(|name| !name.is_empty()) {
            Self::Name(name.to_owned())
        } else {
            Self::Credential(token.to_owned())
        }
    }
}
/// Project IDs belong to an organization, not to whichever token resolved them,
/// so rotating credentials reuse earlier registrations.
#[derive(Hash, PartialEq, Eq)]
struct ProjectKey {
    app_url: String,
    org: OrgIdentity,
    project: String,
}
pub(crate) fn project(
    app_url: &str,
    org: OrgIdentity,
    project: &str,
) -> impl Deref<Target = OnceCell<String>> + use<> {
    static PROJECTS: OnceLock<Cache<ProjectKey, String>> = OnceLock::new();
    PROJECTS
        .get_or_init(|| Cache::new(crate::service::config().max_cached_resolutions))
        .entry(ProjectKey {
            app_url: app_url.to_owned(),
            org,
            project: project.to_owned(),
        })
}

pub(crate) fn capabilities(endpoint: &str) -> impl Deref<Target = OnceCell<Option<usize>>> + use<> {
    static CAPABILITIES: OnceLock<Cache<String, Option<usize>>> = OnceLock::new();
    CAPABILITIES
        .get_or_init(|| Cache::new(crate::service::config().max_cached_resolutions))
        .entry(endpoint.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(cache: &Cache<&'static str, u32>) -> Vec<&'static str> {
        cache.entries.lock().unwrap().keys().copied().collect()
    }
    #[test]
    fn the_cache_keeps_recently_used_and_resolved_entries() {
        let cache = Cache::new(2);
        cache.entry("a").set(1).unwrap();
        cache.entry("b").set(2).unwrap();
        // Using `a` makes `b` the least recently used.
        assert_eq!(cache.entry("a").get(), Some(&1));
        cache.entry("c").set(3).unwrap();
        assert_eq!(keys(&cache), ["a", "c"]);
        // An entry whose resolution failed stays unresolved...
        cache.entry("failed");
        assert_eq!(keys(&cache), ["c", "failed"]);
        // ...and is evicted before resolved entries.
        cache.entry("d").set(4).unwrap();
        assert_eq!(keys(&cache), ["c", "d"]);
    }

    #[tokio::test]
    async fn a_burst_joins_running_lookups_and_keeps_the_idle_cache_bounded() {
        let cache = Cache::new(1);
        {
            let a = cache.entry("a");
            let (release, gate) = tokio::sync::oneshot::channel();
            let first = a.get_or_init(|| async { gate.await.unwrap() });
            tokio::pin!(first);
            assert!(futures::poll!(&mut first).is_pending());

            let b = cache.entry("b");
            let again = cache.entry("a");
            let joined = again.get_or_init(|| async { panic!("started a duplicate lookup") });
            tokio::pin!(joined);
            assert!(futures::poll!(&mut joined).is_pending());
            release.send(1).unwrap();
            assert_eq!(*first.await, 1);
            assert_eq!(*joined.await, 1);
            b.set(2).unwrap();
        }
        assert_eq!(keys(&cache).len(), 1);
    }
}
