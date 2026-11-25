//! Bounded interning of live token values with shared read access.
use indexmap::IndexMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

const CAPACITY: usize = 256;

pub(super) struct TokenMap {
    entries: OnceLock<RwLock<IndexMap<String, Weak<str>>>>,
}

impl TokenMap {
    pub const fn new() -> Self {
        Self {
            entries: OnceLock::new(),
        }
    }

    pub fn get_or_insert(&self, token: String) -> Arc<str> {
        let entries = self.entries.get_or_init(Default::default);
        if let Some(value) = entries.read().unwrap().get(&token).and_then(Weak::upgrade) {
            return value;
        }

        let mut entries = entries.write().unwrap();
        // Another writer may have inserted the token after the read lock was released.
        if let Some(value) = entries.get(&token).and_then(Weak::upgrade) {
            return value;
        }
        entries.retain(|_, value| value.strong_count() > 0);
        if entries.len() >= CAPACITY {
            entries.shift_remove_index(0);
        }
        let value: Arc<str> = Arc::from(token.as_str());
        entries.insert(token, Arc::downgrade(&value));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn racing_insertions_reuse_the_same_live_value() {
        let tokens = TokenMap::new();
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|threads| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    threads.spawn(|| {
                        start.wait();
                        tokens.get_or_insert("token".into())
                    })
                })
                .collect();
            let values: Vec<_> = handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect();
            for value in &values {
                assert!(Arc::ptr_eq(&values[0], value));
            }
            assert!(Arc::ptr_eq(
                &values[0],
                &tokens.get_or_insert("token".into())
            ));
        });
    }

    #[test]
    fn expired_values_are_reclaimed_and_retained_entries_remain_bounded() {
        let tokens = TokenMap::new();
        let expired = Arc::downgrade(&tokens.get_or_insert("expired".into()));
        assert!(expired.upgrade().is_none());
        let values: Vec<_> = (0..=CAPACITY)
            .map(|index| tokens.get_or_insert(index.to_string()))
            .collect();
        let entries = tokens.entries.get().unwrap().read().unwrap();
        assert_eq!(entries.len(), CAPACITY);
        assert!(!entries.contains_key("expired"));
        assert!(!entries.contains_key("0"));
        assert_eq!(&*values[0], "0");
    }
}
