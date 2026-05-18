use lru::LruCache;
use std::hash::Hash;
use std::num::NonZeroUsize;

/// Size-bounded LRU map. On overflow the least-recently-used entry is evicted.
/// When `limit` is `None` the map is unbounded.
///
/// Mirrors Python's `LRUDict` from `peg/engine/lru.py`.
pub struct LruDict<K: Hash + Eq, V> {
    inner: Option<LruCache<K, V>>,
    unbounded: Option<std::collections::HashMap<K, V>>,
    pub limit: Option<usize>,
    pub evictions: usize,
}

impl<K: Hash + Eq + Clone, V> LruDict<K, V> {
    pub fn new(limit: Option<usize>) -> Self {
        match limit.and_then(NonZeroUsize::new) {
            Some(cap) => Self {
                inner: Some(LruCache::new(cap)),
                unbounded: None,
                limit,
                evictions: 0,
            },
            None => Self {
                inner: None,
                unbounded: Some(std::collections::HashMap::new()),
                limit,
                evictions: 0,
            },
        }
    }

    /// Insert or update. Returns the previous value if the key existed.
    /// Promotes the key to MRU position on update.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if let Some(cache) = &mut self.inner {
            let had = cache.contains(&key);
            let prev = cache.push(key, value);
            if prev.is_some() && !had {
                self.evictions += 1;
            }
            prev.map(|(_, v)| v)
        } else {
            self.unbounded.as_mut().unwrap().insert(key, value)
        }
    }

    /// Look up without promoting (read-only peek).
    pub fn get(&self, key: &K) -> Option<&V> {
        if let Some(cache) = &self.inner {
            cache.peek(key)
        } else {
            self.unbounded.as_ref().unwrap().get(key)
        }
    }

    /// Look up and promote the key to MRU position.
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        if let Some(cache) = &mut self.inner {
            cache.get_mut(key)
        } else {
            self.unbounded.as_mut().unwrap().get_mut(key)
        }
    }

    pub fn contains_key(&self, key: &K) -> bool {
        if let Some(cache) = &self.inner {
            cache.contains(key)
        } else {
            self.unbounded.as_ref().unwrap().contains_key(key)
        }
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        if let Some(cache) = &mut self.inner {
            cache.pop(key)
        } else {
            self.unbounded.as_mut().unwrap().remove(key)
        }
    }

    pub fn len(&self) -> usize {
        if let Some(cache) = &self.inner {
            cache.len()
        } else {
            self.unbounded.as_ref().unwrap().len()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        if let Some(cache) = &mut self.inner {
            cache.clear();
        } else {
            self.unbounded.as_mut().unwrap().clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unbounded_stores_all() {
        let mut d: LruDict<i32, &str> = LruDict::new(None);
        d.insert(1, "a");
        d.insert(2, "b");
        assert_eq!(d.get(&1), Some(&"a"));
        assert_eq!(d.get(&2), Some(&"b"));
        assert_eq!(d.len(), 2);
        assert_eq!(d.evictions, 0);
    }

    #[test]
    fn bounded_evicts_lru() {
        let mut d: LruDict<i32, i32> = LruDict::new(Some(2));
        d.insert(1, 1);
        d.insert(2, 2);
        // Access key 1 to make it MRU
        d.get_mut(&1);
        // Insert key 3 — should evict key 2 (LRU)
        d.insert(3, 3);
        assert_eq!(d.len(), 2);
        assert!(d.contains_key(&1));
        assert!(!d.contains_key(&2));
        assert!(d.contains_key(&3));
        assert!(d.evictions > 0);
    }

    #[test]
    fn update_existing_key_promotes() {
        let mut d: LruDict<i32, i32> = LruDict::new(Some(2));
        d.insert(1, 10);
        d.insert(2, 20);
        // Update key 1 — it becomes MRU, so key 2 is now LRU
        d.insert(1, 11);
        // Insert key 3 — should evict key 2
        d.insert(3, 30);
        assert!(d.contains_key(&1));
        assert!(!d.contains_key(&2));
        assert!(d.contains_key(&3));
    }

    #[test]
    fn remove_works() {
        let mut d: LruDict<&str, i32> = LruDict::new(None);
        d.insert("x", 1);
        assert_eq!(d.remove(&"x"), Some(1));
        assert!(d.is_empty());
    }

    #[test]
    fn zero_limit_is_unbounded() {
        // NonZeroUsize::new(0) is None, so limit=0 falls through to unbounded
        let mut d: LruDict<i32, i32> = LruDict::new(Some(0));
        d.insert(1, 1);
        d.insert(2, 2);
        assert_eq!(d.len(), 2);
    }
}
