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
#[derive(Hash, PartialEq, Eq)]
pub(crate) struct RouteKey {
    pub transport: u64,
    pub app_url: String,
    pub api_url: String,
    pub token: String,
    pub org_id: String,
    pub org_name: Option<String>,
}
#[derive(Hash, PartialEq, Eq)]
pub(crate) struct ProjectKey {
    pub route: RouteKey,
    pub project: String,
}
