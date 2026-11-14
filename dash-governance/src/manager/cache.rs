//! Core's bounded insertion caches.

use std::collections::BTreeMap;

/// Core's governance cache capacity (`governance/governance.h:179`).
pub const MAX_CACHE_SIZE: usize = 1_000_000;

/// A map retaining the most recently inserted distinct keys.
// Core: cachemap.h:45-153
#[derive(Clone, Debug)]
pub struct CacheMap<K: Ord + Clone, V: Clone> {
    max_size: usize,
    items: Vec<(K, V)>,
    index: BTreeMap<K, usize>,
}

impl<K: Ord + Clone, V: Clone> CacheMap<K, V> {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            items: Vec::new(),
            index: BTreeMap::new(),
        }
    }
    pub fn max_size(&self) -> usize {
        self.max_size
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn clear(&mut self) {
        self.items.clear();
        self.index.clear();
    }
    pub fn set_max_size(&mut self, max_size: usize) {
        self.max_size = max_size;
    }
    pub fn contains_key(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.index.get(key).map(|&i| &self.items[i].1)
    }
    pub fn items(&self) -> impl Iterator<Item = (&K, &V)> {
        self.items.iter().map(|(k, v)| (k, v))
    }

    pub fn insert(&mut self, key: K, value: V) -> bool {
        if self.index.contains_key(&key) {
            return false;
        }
        if self.max_size != 0 && self.items.len() == self.max_size {
            let (old, _) = self.items.pop().expect("non-empty at capacity");
            self.index.remove(&old);
        }
        self.items.insert(0, (key.clone(), value));
        for index in self.index.values_mut() {
            *index += 1;
        }
        self.index.insert(key, 0);
        true
    }

    pub fn erase(&mut self, key: &K) {
        let Some(index) = self.index.remove(key) else {
            return;
        };
        self.items.remove(index);
        for value in self.index.values_mut() {
            if *value > index {
                *value -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CacheMap, CacheMultiMap};

    #[test]
    fn map_evicts_oldest_in_core_order() {
        let mut map = CacheMap::new(2);
        assert!(map.insert(1, "one"));
        assert!(map.insert(2, "two"));
        assert!(map.insert(3, "three"));
        assert!(!map.contains_key(&1));
        assert_eq!(map.items().map(|(k, _)| *k).collect::<Vec<_>>(), vec![3, 2]);
        assert!(!map.insert(2, "again"));
    }

    #[test]
    fn multimap_evicts_items_and_removes_empty_keys() {
        let mut map = CacheMultiMap::new(2);
        assert!(map.insert(1, 10));
        assert!(map.insert(1, 11));
        assert!(map.insert(2, 20));
        assert_eq!(map.get_all(&1).into_iter().copied().collect::<Vec<_>>(), vec![11]);
        assert!(map.contains_key(&2));
        assert!(map.contains_key(&1));
        map.erase_value(&1, &11);
        assert!(!map.contains_key(&1));
    }
}

/// A bounded multimap retaining insertion order while indexing values per key.
// Core: cachemultimap.h:37-190
#[derive(Clone, Debug)]
pub struct CacheMultiMap<K: Ord + Clone, V: Ord + Clone> {
    max_size: usize,
    items: Vec<(K, V)>,
    index: BTreeMap<K, BTreeMap<V, usize>>,
}

impl<K: Ord + Clone, V: Ord + Clone> CacheMultiMap<K, V> {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            items: Vec::new(),
            index: BTreeMap::new(),
        }
    }
    pub fn max_size(&self) -> usize {
        self.max_size
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn clear(&mut self) {
        self.items.clear();
        self.index.clear();
    }
    pub fn contains_key(&self, key: &K) -> bool {
        self.index.contains_key(key)
    }
    pub fn get(&self, key: &K) -> Option<&V> {
        self.index.get(key).and_then(|m| m.values().next()).map(|&i| &self.items[i].1)
    }
    pub fn get_all(&self, key: &K) -> Vec<&V> {
        self.index
            .get(key)
            .into_iter()
            .flat_map(|m| m.values())
            .map(|&i| &self.items[i].1)
            .collect()
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.index.keys()
    }

    pub fn insert(&mut self, key: K, value: V) -> bool {
        if self.index.get(&key).is_some_and(|m| m.contains_key(&value)) {
            return false;
        }
        if self.max_size != 0 && self.items.len() == self.max_size {
            let (old_key, old_value) = self.items.pop().expect("non-empty at capacity");
            if let Some(values) = self.index.get_mut(&old_key) {
                values.remove(&old_value);
                if values.is_empty() {
                    self.index.remove(&old_key);
                }
            }
        }
        self.items.insert(0, (key.clone(), value.clone()));
        for values in self.index.values_mut() {
            for index in values.values_mut() {
                *index += 1;
            }
        }
        self.index.entry(key).or_default().insert(value, 0);
        true
    }

    pub fn erase(&mut self, key: &K) {
        let values = self.get_all(key).into_iter().cloned().collect::<Vec<_>>();
        for value in values {
            self.erase_value(key, &value);
        }
    }
    pub fn erase_value(&mut self, key: &K, value: &V) {
        let Some(index) = self.index.get_mut(key).and_then(|m| m.remove(value)) else {
            return;
        };
        self.items.remove(index);
        for values in self.index.values_mut() {
            for i in values.values_mut() {
                if *i > index {
                    *i -= 1;
                }
            }
        }
        if self.index.get(key).is_some_and(BTreeMap::is_empty) {
            self.index.remove(key);
        }
    }
}
