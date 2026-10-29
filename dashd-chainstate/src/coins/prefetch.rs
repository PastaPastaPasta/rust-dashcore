//! [`PrefetchMap`]: coins read ahead of V from a database snapshot (the consumer side of CH4's S2;
//! CH2 §2.4.3), behind A1 §3.4's generation fence.
//!
//! 1. A batch carries the `released_gen` read **before** its snapshot was taken, and the
//!    database's write epoch read after that and before the snapshot (DEC-142, O5). The other order stamps
//!    a snapshot from before a release with the generation after it, and the batch would bring
//!    back a coin F spent. A batch of the current generation must therefore have been read at or
//!    after the release's epoch, which
//!    [`admit_prefetch`](super::CoinsCache::admit_prefetch) asserts in debug builds.
//!    CH4's producer (S2) keeps this order, or V stamps the generation when it dispatches the
//!    request.
//! 2. It is admitted only if that is still the current generation, and an entry only while the map
//!    stays within its headroom (B/16). Anything else is dropped, at the cost of a database read
//!    later.
//! 3. Absence is never cached: a batch holds positive results only.
//! 4. A release and every disconnect clear the map.
//!
//! The map answers below active and F, so an entry is only ever read for a key neither holds; such
//! a key's database value has not changed since the last release.

use std::cell::Cell;

use hashbrown::HashTable;

use super::hasher::{Salt, SaltedHasher};
use super::key::OutPointKey;
use super::packed::PackedCoin;
use crate::alloc_tag;

/// A producer's results: the coins it found, read from one snapshot.
pub struct PrefetchBatch {
    /// [`released_gen`](super::CoinsCache::released_gen), read before the snapshot was taken.
    pub fetch_gen: u64,
    /// [`write_epoch`](crate::store::GuardedEngine::write_epoch), read after `fetch_gen` and
    /// before the snapshot was taken.
    pub epoch: u64,
    pub entries: Vec<(OutPointKey, PackedCoin)>,
}

type Entry = (OutPointKey, PackedCoin);

/// hashbrown's control bytes past the last bucket, at most (as in the layers).
const TRAILING_CTRL: u64 = 16;

pub(crate) struct PrefetchMap {
    table: HashTable<Entry>,
    hasher: SaltedHasher,
    /// The long coins' bytes.
    long: u64,
    /// Lookups since the map was made: `have_in_cache` must make none (CH2 §2.4.5).
    lookups: Cell<u64>,
}

impl PrefetchMap {
    pub(crate) fn new(salt: Salt) -> Self {
        Self {
            table: HashTable::new(),
            hasher: SaltedHasher::new(salt),
            long: 0,
            lookups: Cell::new(0),
        }
    }

    pub(crate) fn get(&self, k: &OutPointKey) -> Option<&PackedCoin> {
        self.lookups.set(self.lookups.get() + 1);
        self.table.find(self.hasher.hash(k), |(e, _)| e == k).map(|(_, c)| c)
    }

    /// Removes `k`'s coin and returns it.
    pub(crate) fn take(&mut self, k: &OutPointKey) -> Option<PackedCoin> {
        self.lookups.set(self.lookups.get() + 1);
        let (entry, _) = self.table.find_entry(self.hasher.hash(k), |(e, _)| e == k).ok()?.remove();
        self.long -= entry.1.heap_bytes() as u64;
        Some(entry.1)
    }

    /// Admits `entries` while the map's bytes stay within `cap`, copying each coin under the coins
    /// tag (so the producer's allocations never count as the cache's); returns how many it admitted.
    pub(crate) fn admit(&mut self, entries: Vec<Entry>, cap: u64) -> usize {
        let _tag = alloc_tag::coins();
        let mut admitted = 0;
        for (k, coin) in entries {
            let h = self.hasher.hash(&k);
            if self.table.find(h, |(e, _)| *e == k).is_some() {
                continue;
            }
            if self.bytes_after_insert(coin.heap_bytes() as u64) > cap {
                break;
            }
            let coin = PackedCoin::from_trusted(coin.as_bytes());
            self.long += coin.heap_bytes() as u64;
            let hasher = &self.hasher;
            self.table.insert_unique(h, (k, coin), |(e, _)| hasher.hash(e));
            admitted += 1;
        }
        debug_assert!(self.allocated_bytes() <= cap || admitted == 0);
        admitted
    }

    /// The map's bytes once one more entry with `heap` long bytes is in: a full table doubles.
    fn bytes_after_insert(&self, heap: u64) -> u64 {
        let table = if self.table.len() < self.table.capacity() {
            self.table.allocation_size() as u64
        } else {
            let buckets = (2 * self.table.num_buckets()).max(4) as u64;
            buckets * (size_of::<Entry>() as u64 + 1) + TRAILING_CTRL
        };
        table + self.long + heap
    }

    /// Drops every entry and the table.
    pub(crate) fn clear(&mut self) {
        self.table = HashTable::new();
        self.long = 0;
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.table.len()
    }

    pub(crate) fn lookups(&self) -> u64 {
        self.lookups.get()
    }

    pub(crate) fn allocated_bytes(&self) -> u64 {
        self.table.allocation_size() as u64 + self.long
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u32) -> OutPointKey {
        let mut k = [0; 36];
        k[..4].copy_from_slice(&i.to_le_bytes());
        OutPointKey(k)
    }

    fn coin(len: usize) -> PackedCoin {
        let script = vec![0x51; len];
        let out = dash_consensus::primitives::TxOut {
            value: 5,
            script_pubkey: script.into(),
        };
        PackedCoin::from_output(&out, 1, false).unwrap()
    }

    /// The headroom holds: the map never passes its cap, the doubling estimate is never below the
    /// table hashbrown allocates, and duplicates are skipped.
    #[test]
    fn admission_stops_at_the_cap() {
        let mut map = PrefetchMap::new(Salt::fixed(3));
        let entries: Vec<_> = (0..1000)
            .map(|i| {
                (
                    key(i),
                    coin(if i % 7 == 0 {
                        60
                    } else {
                        3
                    }),
                )
            })
            .collect();
        let cap = 20_000;
        let n = map.admit(entries.clone(), cap);
        assert!(n > 0 && n < 1000, "{n}");
        assert!(map.allocated_bytes() <= cap);
        assert_eq!(map.len(), n);
        assert_eq!(map.admit(entries[..n].to_vec(), u64::MAX), 0);
        let mut grown = PrefetchMap::new(Salt::fixed(3));
        for (k, c) in entries {
            let want = grown.bytes_after_insert(c.heap_bytes() as u64);
            grown.admit(vec![(k, c)], u64::MAX);
            assert!(grown.allocated_bytes() <= want);
        }
        assert_eq!(grown.get(&key(14)), Some(&coin(60)));
        assert_eq!(grown.take(&key(14)), Some(coin(60)));
        assert_eq!(grown.get(&key(14)), None);
        assert_eq!(grown.lookups(), 3);
        grown.clear();
        assert_eq!((grown.len(), grown.allocated_bytes()), (0, 0));
    }
}
