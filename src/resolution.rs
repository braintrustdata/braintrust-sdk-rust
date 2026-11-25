//! Bounded successful resolution shared by compatible authentication contexts.
use indexmap::IndexMap;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

pub(crate) struct Cache<K, V> {
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
        if let Some(value) = entries.get(&key) {
            return value.clone();
        }
        if entries.len() >= self.capacity {
            entries.shift_remove_index(0);
        }
        let value = Arc::new(OnceCell::new());
        entries.insert(key, value.clone());
        value
    }
}
/// The organization a request acts for. When neither its ID nor its name is
/// known, the credential stands in, so unrelated tenants never share a cached
/// resolution or a rate-limit pause.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
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
pub(crate) struct ProjectKey {
    pub app_url: String,
    pub org: OrgIdentity,
    pub project: String,
}
/// Ingestion limits are a property of the data-plane endpoint.
#[derive(Hash, PartialEq, Eq)]
pub(crate) struct CapabilityKey {
    pub api_url: String,
}
