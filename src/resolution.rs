//! Bounded successful resolution shared by compatible authentication contexts.
use indexmap::IndexMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::OnceCell;

/// A bounded map of single-flight resolutions, evicting least recently used.
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
    pub fn entry(&self, key: K) -> Arc<OnceCell<V>> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(index) = entries.get_index_of(&key) {
            let last = entries.len() - 1;
            entries.move_index(index, last);
            return entries[last].clone();
        }
        if entries.len() >= self.capacity {
            // Unresolved entries, such as ones whose resolution failed, go
            // first, so they cannot crowd out working ones.
            let victim = entries
                .values()
                .position(|cell| !cell.initialized())
                .unwrap_or(0);
            entries.shift_remove_index(victim);
        }
        let value = Arc::new(OnceCell::new());
        entries.insert(key, value.clone());
        value
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
    /// A non-secret label for scheduler scopes.
    pub fn scope_key(&self) -> String {
        match self {
            Self::Id(id) => id.clone(),
            Self::Name(name) => format!("name:{name}"),
            Self::Credential(token) => format!("credential:{:016x}", credential_digest(token)),
        }
    }
}
/// Stable within a process. Collisions only merge pauses, never cached values.
pub(crate) fn credential_digest(token: &str) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault};
    BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default().hash_one(token)
}
/// Project IDs belong to an organization, not to whichever token resolved them,
/// so rotating credentials reuse earlier registrations.
#[derive(Hash, PartialEq, Eq)]
struct ProjectKey {
    app_url: String,
    org: OrgIdentity,
    project: String,
}
pub(crate) fn project(app_url: &str, org: OrgIdentity, project: &str) -> Arc<OnceCell<String>> {
    static PROJECTS: OnceLock<Cache<ProjectKey, String>> = OnceLock::new();
    PROJECTS
        .get_or_init(|| Cache::new(crate::service::config().max_cached_resolutions))
        .entry(ProjectKey {
            app_url: app_url.to_owned(),
            org,
            project: project.to_owned(),
        })
}

pub(crate) fn capabilities(endpoint: &str) -> Arc<OnceCell<Option<usize>>> {
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
}
