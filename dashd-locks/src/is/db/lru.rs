use std::collections::HashMap;
use std::hash::Hash;

const MAX_SIZE: usize = 10_000;

pub(super) struct Lru<K, V> {
    entries: HashMap<K, (V, u64)>,
    counter: u64,
}

impl<K, V> Default for Lru<K, V> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            counter: 0,
        }
    }
}

impl<K: Eq + Hash, V: Clone> Lru<K, V> {
    pub(super) fn insert(&mut self, key: K, value: V) {
        self.entries.insert(key, (value, self.counter));
        self.counter += 1;
        if self.entries.len() > MAX_SIZE * 2 {
            let mut accesses: Vec<_> = self.entries.values().map(|(_, time)| *time).collect();
            let cutoff = accesses.len() - MAX_SIZE;
            accesses.select_nth_unstable(cutoff);
            let oldest_kept = accesses[cutoff];
            self.entries.retain(|_, (_, time)| *time >= oldest_kept);
        }
    }

    pub(super) fn get(&mut self, key: &K) -> Option<V> {
        let (value, time) = self.entries.get_mut(key)?;
        *time = self.counter;
        self.counter += 1;
        Some(value.clone())
    }

    pub(super) fn erase(&mut self, key: &K) {
        self.entries.remove(key);
    }

    /// Erases `key` only while it maps to `value`, leaving every recency unchanged.
    pub(super) fn erase_if_maps_to(&mut self, key: &K, value: &V)
    where
        V: PartialEq,
    {
        if self.entries.get(key).is_some_and(|(v, _)| v == value) {
            self.entries.remove(key);
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn entries(&self) -> &HashMap<K, (V, u64)> {
        &self.entries
    }
}
