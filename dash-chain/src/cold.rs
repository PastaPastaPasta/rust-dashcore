//! The cold records cache (SB5 §5.2): CLOCK (second chance) over the `HeaderRecord`s SPV reads
//! from the store below its window. A miss reads the store and inserts under the cache's one
//! mutex (Core `snapshotCacheCs`, `llmq/snapshot.cpp:307-315`), so a read is atomic with respect
//! to `invalidate`: after the invalidation that follows a commit, no pre-commit record survives
//! (loom L6). The mutex is never taken in a step's tip critical section (SB5-A10); a hold's
//! planning reads may take it (§4.8). A miss holds it across the store read, so the writer never
//! calls `invalidate` or `clear` while holding anything a `HeaderStore` read takes: it
//! invalidates after its commit returns.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher, Hasher};
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_primitives::BlockHash;

use crate::error::StoreError;
use crate::residency::Residency;
use crate::shared::Shared;
use crate::store::{HeaderRecord, HeaderStore};
use crate::sync::{AtomicU32, Mutex, Ordering};

/// The cache: `cap` records at most; its tables grow with the records read, up to `cap`.
pub(crate) struct ColdCache {
    clock: Mutex<Clock>,
    #[cfg(test)]
    pub(crate) faults: ColdFaults,
}

struct Clock {
    /// id → its ring position. Ids are the index's own, so a fixed hasher is safe, and it keeps
    /// the probe sequences, and the PF0 counts, the same in every process.
    map: HashMap<BlockId, usize, BuildHasherDefault<IdHasher>>,
    /// hash → id for the cached records, so a read by hash can hit (D43): SB4 resolves every
    /// cold record's `prev` by hash. Hashes are miners' choice, but at most `cap` are held, so
    /// a fixed hasher's worst probe is O(cap), as the CLOCK sweep is.
    by_hash: HashMap<BlockHash, BlockId, BuildHasherDefault<DefaultHasher>>,
    /// The ring; `None` is a position freed by `invalidate`.
    ring: Vec<Option<Frame>>,
    /// Freed ring positions, reused before the hand sweeps.
    free: Vec<usize>,
    hand: usize,
    cap: usize,
}

/// The id map's hasher: a multiplicative hash of the id, rotated as rustc-hash 2's is (D63b).
/// hashbrown indexes by the low bits and tags by the top seven of the low `usize` bits. The
/// product's low bits depend only on the id's low bits, so unrotated, ids at a power-of-two stride
/// would share a few buckets; the rotation puts the product's high bits, which mix every id bit,
/// at the bottom. On 64-bit targets the tag is the product's bits 31 to 37; on 32-bit ones it
/// includes bits 0 to 5, so strided ids share tags more (extra key compares, not more probes).
/// Ids are the index's own, so nothing adversarial reaches it, and it is the same in every
/// process. Small enough that the map's probes inline into `Clock`'s paths wherever the crate's
/// codegen units fall: std's SipHash went out of line when BL-206 added a module, +6 to 8% on
/// PF0's cold rows with no change here (T19, DEC-274).
#[derive(Clone, Copy, Default)]
struct IdHasher(u64);

impl Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.0 = self.0.rotate_left(32) ^ u64::from(n);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(26)
    }
}

struct Frame {
    rec: Arc<HeaderRecord>,
    /// Set on a hit; the hand clears it once before evicting (second chance).
    referenced: bool,
}

/// Planted faults for loom L6's negative control and the directed tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct ColdFaults {
    /// L6: a miss releases the mutex around its store read.
    pub(crate) unlock_around_read: bool,
}

impl ColdCache {
    /// # Panics
    ///
    /// If `cap` is 0.
    pub(crate) fn new(cap: u32) -> ColdCache {
        assert!(cap > 0, "a cold cache holds at least one record");
        // Nothing reserved: a chain that reads nothing cold holds nothing for its cache (PF0).
        ColdCache {
            clock: Mutex::new(Clock {
                map: HashMap::default(),
                by_hash: HashMap::default(),
                ring: Vec::new(),
                free: Vec::new(),
                hand: 0,
                cap: cap as usize,
            }),
            #[cfg(test)]
            faults: ColdFaults::default(),
        }
    }

    /// Read-through by id: a hit is O(1); a miss reads `store.by_id` and inserts, under the one
    /// mutex. A store error inserts nothing, nor does a record at or above `issued`, the
    /// writer's watermark, read after the store (DEC-187 amendment 3): it is corruption.
    pub(crate) fn get_or_load<S: HeaderStore + ?Sized>(
        &self,
        id: BlockId,
        store: &S,
        issued: &AtomicU32,
    ) -> Result<Option<Arc<HeaderRecord>>, StoreError> {
        let mut c = self.clock.lock();
        if let Some(rec) = c.hit(id) {
            return Ok(Some(rec));
        }
        #[cfg(test)]
        if self.faults.unlock_around_read {
            drop(c);
            let rec = store.by_id(id)?;
            return self.clock.lock().admit(rec, issued);
        }
        c.admit(store.by_id(id)?, issued)
    }

    /// Read-through by hash, as [`ColdCache::get_or_load`] by id; a miss's record is inserted
    /// by its id (§5.2, D43).
    pub(crate) fn load_by_hash<S: HeaderStore + ?Sized>(
        &self,
        h: &BlockHash,
        store: &S,
        issued: &AtomicU32,
    ) -> Result<Option<Arc<HeaderRecord>>, StoreError> {
        let mut c = self.clock.lock();
        let hit = c.by_hash.get(h).copied().and_then(|id| c.hit(id));
        if let Some(rec) = hit.filter(|r| r.hash == *h) {
            return Ok(Some(rec));
        }
        #[cfg(test)]
        if self.faults.unlock_around_read {
            drop(c);
            let rec = store.by_hash(h)?;
            return self.clock.lock().admit(rec, issued);
        }
        c.admit(store.by_hash(h)?, issued)
    }

    /// After a commit (SB4's stage 5, SB5-A5): drops these ids. The caller passes at most `cap`
    /// ids, else calls [`ColdCache::clear`].
    pub(crate) fn invalidate(&self, ids: &[BlockId]) {
        let mut c = self.clock.lock();
        debug_assert!(ids.len() <= c.cap, "more than `cap` ids: the caller clears instead");
        for &id in ids {
            c.remove(id);
        }
    }

    /// Drops every record: O(cap) under the mutex, always correct after a commit (DEC-157 (3)).
    pub(crate) fn clear(&self) {
        self.clock.lock().clear();
    }

    /// The cached record, without touching its reference bit (tests).
    #[cfg(test)]
    pub(crate) fn peek(&self, id: BlockId) -> Option<Arc<HeaderRecord>> {
        let c = self.clock.lock();
        c.map.get(&id).map(|&at| c.frame(at).rec.clone())
    }

    /// The number of cached records (tests).
    #[cfg(all(test, not(loom)))]
    pub(crate) fn len(&self) -> usize {
        let c = self.clock.lock();
        assert_eq!(c.by_hash.len(), c.map.len(), "one hash per cached record");
        c.map.len()
    }
}

/// The store's `walk_down` past the cache, for snapshot ranges (§5.3, D14): a range of n
/// records would otherwise evict up to n cached ones.
pub(crate) trait ColdWalk: Send + Sync {
    /// [`HeaderStore::walk_down`] for `n` records the caller knows are stored: `from`, then
    /// each one's parent. Fewer, or unlinked ones, are corruption; it and a failed read trip the
    /// latch.
    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError>;
}

impl<S: HeaderStore, R: Residency> ColdWalk for Shared<S, R> {
    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        let recs =
            self.store.walk_down(from, n).inspect_err(|e| self.fail.read_failed(&e.to_string()))?;
        let linked = recs.len() == n as usize
            && recs.first().is_some_and(|r| r.id == from)
            && recs
                .windows(2)
                .all(|w| w[0].header.prev_hash == w[1].hash && w[0].height == w[1].height + 1);
        match linked {
            true => Ok(recs),
            false => {
                let got = recs.len();
                Err(self
                    .corrupt(format!("walk_down({}, {n}): {got} records, not {n} linked", from.0)))
            }
        }
    }
}

impl Clock {
    #[cfg(test)]
    fn frame(&self, at: usize) -> &Frame {
        self.ring[at].as_ref().expect("a mapped position holds a frame")
    }

    fn frame_mut(&mut self, at: usize) -> &mut Frame {
        self.ring[at].as_mut().expect("a mapped position holds a frame")
    }

    fn hit(&mut self, id: BlockId) -> Option<Arc<HeaderRecord>> {
        let &at = self.map.get(&id)?;
        let f = self.frame_mut(at);
        f.referenced = true;
        Some(f.rec.clone())
    }

    /// Caches a found record and returns it shared, unless its id is at or above `issued`. The
    /// watermark is read after the store: every record the store returns was committed after
    /// its id was issued.
    #[inline]
    fn admit(
        &mut self,
        rec: Option<HeaderRecord>,
        issued: &AtomicU32,
    ) -> Result<Option<Arc<HeaderRecord>>, StoreError> {
        // A store miss admits nothing: SPV's common read (a new header's hash) stays inline (PF0).
        match rec {
            Some(rec) => self.admit_found(rec, issued).map(Some),
            None => Ok(None),
        }
    }

    fn admit_found(
        &mut self,
        rec: HeaderRecord,
        issued: &AtomicU32,
    ) -> Result<Arc<HeaderRecord>, StoreError> {
        let issued = issued.load(Ordering::Acquire);
        if rec.id.0 >= issued {
            return Err(StoreError::Corrupt(format!(
                "a record names entry {}, never issued (next {issued})",
                rec.id.0
            )));
        }
        let rec = Arc::new(rec);
        self.insert(rec.clone());
        Ok(rec)
    }

    /// Inserts or replaces `rec`. A full ring evicts at the hand: each referenced frame it
    /// passes loses its bit first, so the worst case clears all `cap` bits (§5.2, R1-m3).
    fn insert(&mut self, rec: Arc<HeaderRecord>) {
        let id = rec.id;
        if let Some(&at) = self.map.get(&id) {
            let frame = self.frame_mut(at);
            // `by_hash` keeps the old entry: an id's record never changes its hash.
            debug_assert_eq!(frame.rec.hash, rec.hash, "a replaced record keeps its hash");
            frame.rec = rec;
            return;
        }
        let frame = Frame {
            rec,
            referenced: false,
        };
        let at = match self.free.pop() {
            Some(at) => at,
            None if self.ring.len() < self.cap => {
                self.ring.push(None);
                self.ring.len() - 1
            }
            None => self.victim(),
        };
        self.by_hash.insert(frame.rec.hash, id);
        self.ring[at] = Some(frame);
        self.map.insert(id, at);
    }

    /// The hand's next unreferenced frame, removed. The ring is full, so every position holds a
    /// frame and the sweep ends within `cap + 1` steps.
    fn victim(&mut self) -> usize {
        loop {
            let at = self.hand;
            self.hand = (self.hand + 1) % self.ring.len();
            let f = self.ring[at].as_mut().expect("a full ring has no holes");
            if std::mem::take(&mut f.referenced) {
                continue;
            }
            self.map.remove(&f.rec.id);
            self.by_hash.remove(&f.rec.hash);
            return at;
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.by_hash.clear();
        self.ring.clear();
        self.free.clear();
        self.hand = 0;
    }

    fn remove(&mut self, id: BlockId) {
        if let Some(at) = self.map.remove(&id) {
            let f = self.ring[at].take().expect("a mapped position holds a frame");
            self.by_hash.remove(&f.rec.hash);
            self.free.push(at);
        }
    }
}

#[cfg(all(test, loom))]
mod loom_tests;
#[cfg(test)]
mod test_store;
#[cfg(all(test, not(loom)))]
mod tests;
