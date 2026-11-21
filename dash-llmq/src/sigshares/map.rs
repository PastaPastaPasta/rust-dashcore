//! Signature-share and counted-bucket maps.

use std::collections::{BTreeMap, btree_map::Entry};

/// `(signHash, quorumMember)` (`l/signing_shares.h:43-44`).
pub type SigShareKey = ([u8; 32], u16);

/// A two-level map whose structural operations maintain an O(1) entry count.
pub struct CountedBucketMap<T> {
    data: BTreeMap<[u8; 32], BTreeMap<u16, T>>,
    num_entries: usize,
}

impl<T> Default for CountedBucketMap<T> {
    // Core: v24.0.0 l/signing_shares.h:184-185 CountedBucketMap
    fn default() -> Self {
        Self {
            data: BTreeMap::new(),
            num_entries: 0,
        }
    }
}

impl<T> CountedBucketMap<T> {
    // Core: v24.0.0 l/signing_shares.h:188-189 CountedBucketMap::Buckets
    pub fn buckets(&self) -> &BTreeMap<[u8; 32], BTreeMap<u16, T>> {
        &self.data
    }

    // Core: v24.0.0 l/signing_shares.h:190 CountedBucketMap::Size
    pub fn size(&self) -> usize {
        self.num_entries
    }

    // Core: v24.0.0 l/signing_shares.h:192-199 CountedBucketMap::Emplace
    pub fn emplace(&mut self, key: &SigShareKey, value: T) -> bool {
        if let Entry::Vacant(entry) = self.data.entry(key.0).or_default().entry(key.1) {
            entry.insert(value);
            self.num_entries += 1;
            true
        } else {
            false
        }
    }

    // Core: v24.0.0 l/signing_shares.h:201-211 CountedBucketMap::Erase
    pub fn erase(&mut self, key: &SigShareKey) {
        if let Entry::Occupied(mut bucket) = self.data.entry(key.0) {
            if bucket.get_mut().remove(&key.1).is_some() {
                self.num_entries -= 1;
            }
            if bucket.get().is_empty() {
                bucket.remove();
            }
        }
    }

    // Core: v24.0.0 l/signing_shares.h:213-221 CountedBucketMap::EraseBucket
    pub fn erase_bucket(&mut self, sign_hash: &[u8; 32]) {
        if let Some(bucket) = self.data.remove(sign_hash) {
            self.num_entries -= bucket.len();
        }
    }

    // Core: v24.0.0 l/signing_shares.h:223-244 CountedBucketMap::EraseIf
    pub fn erase_if(&mut self, mut predicate: impl FnMut(&SigShareKey, &mut T) -> bool) {
        self.data.retain(|hash, bucket| {
            bucket.retain(|member, value| {
                let erase = predicate(&(*hash, *member), value);
                if erase {
                    self.num_entries -= 1;
                }
                !erase
            });
            !bucket.is_empty()
        });
    }

    // Core: v24.0.0 l/signing_shares.h:246-250 CountedBucketMap::Clear
    pub fn clear(&mut self) {
        self.data.clear();
        self.num_entries = 0;
    }
}

/// Shares or per-share metadata, grouped by signing session.
pub struct SigShareMap<T> {
    internal: CountedBucketMap<T>,
}

impl<T> Default for SigShareMap<T> {
    // Core: v24.0.0 l/signing_shares.h:257 SigShareMap
    fn default() -> Self {
        Self {
            internal: CountedBucketMap::default(),
        }
    }
}

impl<T> SigShareMap<T> {
    // Core: v24.0.0 l/signing_shares.h:260-263 SigShareMap::Add
    pub fn add(&mut self, key: &SigShareKey, value: T) -> bool {
        self.internal.emplace(key, value)
    }

    // Core: v24.0.0 l/signing_shares.h:265-268 SigShareMap::Erase
    pub fn erase(&mut self, key: &SigShareKey) {
        self.internal.erase(key);
    }

    // Core: v24.0.0 l/signing_shares.h:270-273 SigShareMap::Clear
    pub fn clear(&mut self) {
        self.internal.clear();
    }

    // Core: v24.0.0 l/signing_shares.h:275-283 SigShareMap::Has
    pub fn has(&self, key: &SigShareKey) -> bool {
        self.get(key).is_some()
    }

    // Core: v24.0.0 l/signing_shares.h:285-299 SigShareMap::Get
    pub fn get(&self, key: &SigShareKey) -> Option<&T> {
        self.internal.data.get(&key.0)?.get(&key.1)
    }

    // Core: v24.0.0 l/signing_shares.h:285-299 SigShareMap::Get
    pub fn get_mut(&mut self, key: &SigShareKey) -> Option<&mut T> {
        self.internal.data.get_mut(&key.0)?.get_mut(&key.1)
    }

    // Core: v24.0.0 l/signing_shares.h:301-309 SigShareMap::GetOrAdd
    pub fn get_or_add(&mut self, key: &SigShareKey) -> &mut T
    where
        T: Default,
    {
        self.internal.data.entry(key.0).or_default().entry(key.1).or_insert_with(|| {
            let value = T::default();
            self.internal.num_entries += 1;
            value
        })
    }

    // Core: v24.0.0 l/signing_shares.h:311-318 SigShareMap::GetFirst
    pub fn get_first(&self) -> Option<&T> {
        self.internal.data.first_key_value()?.1.first_key_value().map(|(_, value)| value)
    }

    // Core: v24.0.0 l/signing_shares.h:320-323 SigShareMap::Size
    pub fn size(&self) -> usize {
        self.internal.size()
    }

    // Core: v24.0.0 l/signing_shares.h:325-333 SigShareMap::CountForSignHash
    pub fn count_for_sign_hash(&self, sign_hash: &[u8; 32]) -> usize {
        self.internal.data.get(sign_hash).map_or(0, BTreeMap::len)
    }

    // Core: v24.0.0 l/signing_shares.h:335-338 SigShareMap::Empty
    pub fn is_empty(&self) -> bool {
        self.internal.size() == 0
    }

    // Core: v24.0.0 l/signing_shares.h:340-348 SigShareMap::GetAllForSignHash
    pub fn get_all_for_sign_hash(&self, sign_hash: &[u8; 32]) -> Option<&BTreeMap<u16, T>> {
        self.internal.data.get(sign_hash)
    }

    // Core: v24.0.0 l/signing_shares.h:350-353 SigShareMap::EraseAllForSignHash
    pub fn erase_all_for_sign_hash(&mut self, sign_hash: &[u8; 32]) {
        self.internal.erase_bucket(sign_hash);
    }

    // Core: v24.0.0 l/signing_shares.h:355-359 SigShareMap::EraseIf
    pub fn erase_if(&mut self, predicate: impl FnMut(&SigShareKey, &mut T) -> bool) {
        self.internal.erase_if(predicate);
    }

    // Core: v24.0.0 l/signing_shares.h:361-372 SigShareMap::ForEach
    pub fn for_each(&mut self, mut f: impl FnMut(&SigShareKey, &mut T)) {
        for (hash, bucket) in &mut self.internal.data {
            for (member, value) in bucket {
                f(&(*hash, *member), value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn map_matches_core_operations() {
        let mut map = SigShareMap::<i64>::default();
        let a = ([1; 32], 2);
        let b = ([1; 32], 3);
        let c = ([2; 32], 2);
        assert!(map.is_empty());
        assert_eq!(map.get_first(), None);
        assert!(!map.has(&a));
        assert!(map.add(&a, 7));
        assert!(!map.add(&a, 99));
        assert_eq!(map.get(&a), Some(&7));
        assert_eq!(map.get_or_add(&a), &7);
        *map.get_or_add(&b) += 8;
        assert!(map.add(&c, 9));
        assert_eq!(map.size(), 3);
        assert_eq!(map.count_for_sign_hash(&a.0), 2);
        assert_eq!(map.count_for_sign_hash(&[3; 32]), 0);
        assert_eq!(map.get_all_for_sign_hash(&a.0).unwrap().len(), 2);
        assert!(map.get_all_for_sign_hash(&[3; 32]).is_none());
        assert!([7, 8, 9].contains(map.get_first().unwrap()));
        *map.get_mut(&a).unwrap() = 10;
        map.for_each(|key, value| *value += i64::from(key.1));
        assert_eq!(map.get(&a), Some(&12));
        map.erase_if(|key, value| key.0 == a.0 && *value == 11);
        assert_eq!(map.size(), 2);
        map.erase(&b);
        map.erase(&([9; 32], 0));
        map.erase(&a);
        assert_eq!(map.size(), 1);
        assert!(map.get_all_for_sign_hash(&a.0).is_none());
        map.erase_all_for_sign_hash(&a.0);
        map.erase_all_for_sign_hash(&c.0);
        assert!(map.is_empty());
        map.add(&c, 4);
        map.erase_if(|_, value| {
            *value += 1;
            false
        });
        assert_eq!(map.get(&c), Some(&5));
        assert_eq!(map.size(), 1);
        map.clear();
        assert_eq!(map.size(), 0);
        assert_eq!(map.get_first(), None);
    }

    proptest! {
        #[test]
        fn counted_map_matches_flat_reference(ops in prop::collection::vec((0u8..6, 0u8..5, 0u16..8, any::<u32>()), 0..200)) {
            let mut map = CountedBucketMap::default();
            let mut reference = BTreeMap::new();
            for (op, hash, member, value) in ops {
                let key = ([hash; 32], member);
                match op {
                    0 => {
                        let expected = if let Entry::Vacant(e) = reference.entry(key) { e.insert(value); true } else { false };
                        prop_assert_eq!(map.emplace(&key, value), expected);
                    }
                    1 => { map.erase(&key); reference.remove(&key); }
                    2 => {
                        map.erase_bucket(&key.0);
                        reference.retain(|k, _| k.0 != key.0);
                    }
                    3 => {
                        map.erase_if(|k, v| k.1 == member || *v == value);
                        reference.retain(|k, v| k.1 != member && *v != value);
                    }
                    4 => { map.clear(); reference.clear(); }
                    _ => { map.emplace(&key, value); reference.entry(key).or_insert(value); }
                }
                prop_assert_eq!(map.size(), reference.len());
                prop_assert!(map.buckets().values().all(|b| !b.is_empty()));
                let flat: BTreeMap<_, _> = map.buckets().iter().flat_map(|(h, b)| b.iter().map(|(m, v)| ((*h, *m), *v))).collect();
                prop_assert_eq!(&flat, &reference);
            }
        }
    }
}
