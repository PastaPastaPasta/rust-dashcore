//! The state the writer shares with readers (SB4 §13.1): the arena, the hash index, the store
//! and the tip cell. The writer appends; readers resolve published ids lock-free, take the
//! index's read lock per lookup and load the published snapshot lock-free.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dash_clock::NodeTime;
use dash_consensus::BlockId;
use dash_params::ChainSetup;
use dash_primitives::{BlockHash, BlockHeader};

use crate::Work;
use crate::anchor::floor_below;
use crate::arena::Arena;
use crate::chain::MutableGroup;
use crate::cold::ColdCache;
use crate::config::{ChainConfig, ChainMode};
use crate::entry::MTP_UNKNOWN;
use crate::error::{FailStop, StoreError};
use crate::index::HashIndex;
use crate::reader::{EntryView, ReadBackend};
use crate::residency::Residency;
use crate::status::Status;
use crate::status_journal::JournalSlot;
use crate::store::{HeaderRecord, HeaderStore, RecordKind};
use crate::sync::{AtomicU32, Ordering};
use crate::tip::TipCell;
use crate::walk::Link;

pub(crate) struct Shared<S: HeaderStore, R: Residency> {
    pub(crate) arena: Arena<R>,
    pub(crate) index: HashIndex,
    pub(crate) store: S,
    pub(crate) readers: ReaderState,
    /// The latch a failed cold read trips (SB0 §4.4).
    pub(crate) fail: Arc<dyn FailStop>,
    /// The lowest height held: 0 from genesis, anchor − 24 when anchored (§8.4).
    pub(crate) floor: u32,
    /// The lowest accepted height: 0 from genesis, the anchor when anchored. The SPV locator's
    /// terminus and `FindForkInGlobalIndex`'s fallback (§10.1 n6, D41).
    pub(crate) anchor: u32,
    /// SB5's cold cache (SB5 §5.2): `cold_cache` records in SPV, which evicts; none for the node.
    pub(crate) cold: Option<Box<ColdCache>>,
    /// The writer's `next_id`: every issued id is below it. The cold cache refuses a record at
    /// or above it as corruption (DEC-187 amendment 3), and `open` restores it from the store.
    issued: AtomicU32,
}

// `arena` stays within a signed byte of the `Arc<Shared>` pointer, past its two counts: beyond
// it, x86-64 has no 8-bit displacement for the field, and every link of an ancestor walk spends
// an extra instruction forming the address (PF0 `chain-ancestor-random`). The cold cache is
// boxed for this. `Shared` is `repr(Rust)`, so this checks the benches' `NoStore` layouts only.
#[cfg(not(loom))]
const _: () = {
    use std::mem::{offset_of, size_of};

    use crate::residency::{Evicting, Permanent};
    use crate::store::NoStore;
    const COUNTS: usize = 2 * size_of::<usize>();
    const DISP8_REACH: usize = 128;
    assert!(
        offset_of!(Shared<NoStore, Permanent>, arena) + COUNTS + size_of::<Arena<Permanent>>()
            <= DISP8_REACH
    );
    assert!(
        offset_of!(Shared<NoStore, Evicting>, arena) + COUNTS + size_of::<Arena<Evicting>>()
            <= DISP8_REACH
    );
};

/// SPV's by-id resolution (SB0 §4.4, DEC-187 amendment 3): `resident`, else `cold`, the
/// committed record. An id below `issued` with neither is `missing`'s corruption; one at or
/// above it the chain never held. The watermark is read before `resident`: the writer sets an
/// entry before it issues the id and evicts it only after its record commits, so an id issued
/// by then is resident, or its record reads. Read after, it would reject an id issued between
/// a resident miss and an uncommitted store read (loom L14).
pub(crate) fn resolve_by_id<T>(
    issued: &AtomicU32,
    id: BlockId,
    resident: impl FnOnce() -> Option<T>,
    cold: impl FnOnce() -> Result<Option<T>, StoreError>,
    missing: impl FnOnce() -> StoreError,
) -> Result<Option<T>, StoreError> {
    let held = id.0 < issued.load(Ordering::Acquire);
    if let Some(t) = resident() {
        return Ok(Some(t));
    }
    match cold()? {
        None if held => Err(missing()),
        t => Ok(t),
    }
}

/// One entry, resident or cold, as the writer and the fallible readers see it (§3.7, §8.4). A
/// cold entry's status is its committed record's; its `mtp` is [`MTP_UNKNOWN`] (records carry
/// none, §8.3).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Node {
    pub(crate) hash: BlockHash,
    pub(crate) header: BlockHeader,
    pub(crate) height: u32,
    pub(crate) prev: Option<BlockId>,
    pub(crate) skip: Option<BlockId>,
    pub(crate) chain_work: Work,
    pub(crate) time_max: u32,
    pub(crate) mtp: i64,
    pub(crate) context: bool,
    pub(crate) status: Status,
    pub(crate) resident: bool,
}

impl Node {
    pub(crate) fn link(&self) -> Link {
        Link {
            height: self.height,
            prev: self.prev,
            skip: self.skip,
        }
    }
}

/// What SB5's cold cache drops after a step commits (§9.1 stage 5, SB5-A5): at most
/// `cold_cache` ids, or everything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColdInvalidation {
    Ids(Vec<BlockId>),
    Clear,
}

/// What readers read besides entries: the tip cell, the IBD latch and its parameters.
pub(crate) struct ReaderState {
    /// Set by every constructor's first publication.
    pub(crate) tip: OnceLock<TipCell>,
    /// Core's `m_cached_finished_ibd` (§9.4): one per chain.
    pub(crate) finished_ibd: AtomicBool,
    /// Core's `MinimumChainWork()`.
    pub(crate) min_chain_work: Work,
    /// Core's `m_options.max_tip_age`.
    pub(crate) max_tip_age: Duration,
    /// The node's status journals, where a reader finds a snapshot's (D63, D65, D68).
    pub(crate) journals: JournalSlot,
}

impl ReaderState {
    /// The tip cell: every constructor has published by the time a reader exists.
    pub(crate) fn tip(&self) -> &TipCell {
        self.tip.get().expect("every constructor publishes the tip")
    }

    /// Core `IsInitialBlockDownload` on a tip's `(work, time)`, `None` for no tip, latched
    /// (R4, §9.4): readers pass a snapshot's tip, the writer its private tip (CH1-A8). The age
    /// check is Core's `Tip()->Time() < NodeClock::now() - max_tip_age`, on `now` as precise as
    /// the caller's clock: a tip exactly `max_tip_age` old is too old once `now` is past the
    /// second.
    // Core: validation.cpp:1970-1993 Chainstate::IsInitialBlockDownload
    pub(crate) fn is_ibd(&self, tip: Option<(Work, u32)>, now: NodeTime, loading: bool) -> bool {
        if self.finished_ibd.load(Ordering::Acquire) {
            return false;
        }
        if loading {
            return true;
        }
        let Some((work, time)) = tip else {
            return true;
        };
        if work < self.min_chain_work {
            return true;
        }
        // time + max_age < now, exactly: over whole seconds, below `now`'s second, or at it
        // when `now` is past it.
        let oldest = i128::from(time) + i128::from(self.max_tip_age.as_secs());
        let past = i128::from(now.subsec_nanos() > 0);
        if oldest < i128::from(now.floor_secs()) + past {
            return true;
        }
        if !self.finished_ibd.swap(true, Ordering::AcqRel) {
            tracing::info!("Leaving InitialBlockDownload (latching to false)");
        }
        false
    }
}

impl<S: HeaderStore, R: Residency> Shared<S, R> {
    pub(crate) fn new(
        params: &ChainSetup,
        cfg: &ChainConfig,
        store: S,
        fail: Arc<dyn FailStop>,
        anchor: u32,
    ) -> Shared<S, R> {
        let readers = ReaderState {
            tip: OnceLock::new(),
            finished_ibd: AtomicBool::new(false),
            min_chain_work: cfg.min_chain_work.unwrap_or(params.consensus.minimum_chain_work),
            max_tip_age: cfg.max_tip_age,
            journals: JournalSlot::default(),
        };
        Shared {
            arena: Arena::new(),
            index: HashIndex::default(),
            store,
            readers,
            fail,
            floor: floor_below(anchor),
            anchor,
            cold: (cfg.mode == ChainMode::Spv).then(|| Box::new(ColdCache::new(cfg.cold_cache))),
            issued: AtomicU32::new(0),
        }
    }

    /// The writer issued every id below `next_id`. A reader that reads a record sees its id
    /// issued through the store: the writer issues an id before the step that writes its record
    /// commits, and a read returns only committed records (§3.7).
    pub(crate) fn issue(&self, next_id: u32) {
        self.issued.store(next_id, Ordering::Release);
    }

    pub(crate) fn link(&self, id: BlockId) -> Option<Link> {
        self.arena.imm(id, |e| Link {
            height: e.height,
            prev: e.prev,
            skip: e.skip,
        })
    }

    #[inline]
    fn resident_node(&self, id: BlockId) -> Option<Node> {
        self.arena.with(id, |e, entry| Node {
            hash: e.hash,
            header: e.header,
            height: e.height,
            prev: e.prev,
            skip: e.skip,
            chain_work: e.chain_work,
            time_max: e.time_max,
            mtp: e.mtp,
            context: e.context,
            status: entry.group.read().status,
            resident: true,
        })
    }

    /// The resident entry `id`, else its committed record (SB0 §4.4's resolution). `Ok(None)`:
    /// an id the chain never held. In SPV, an issued id with neither is corruption (DEC-187
    /// amendment 3); see [`resolve_by_id`].
    pub(crate) fn try_node(&self, id: BlockId) -> Result<Option<Node>, StoreError> {
        if !R::EVICTS {
            // Matched and rewrapped: `Ok(self.resident_node(id))` returns the `Node` through
            // a stack copy, about 40 instructions per tip step (PF0).
            if let Some(node) = self.resident_node(id) {
                return Ok(Some(node));
            }
            return Ok(None);
        }
        let resident = || self.resident_node(id);
        let cold = || match self.cold_by_id(id)? {
            Some(rec) if rec.id != id => {
                Err(self.corrupt(format!("entry {} read as record {}", id.0, rec.id.0)))
            }
            rec => rec.map(|rec| self.cold_node(rec)).transpose(),
        };
        resolve_by_id(&self.issued, id, resident, cold, || self.missing_record(id))
    }

    /// [`Shared::try_node`] for an id the chain issued: one with no record is corruption.
    #[inline]
    pub(crate) fn node(&self, id: BlockId) -> Result<Node, StoreError> {
        self.try_node(id)?.ok_or_else(|| self.missing_record(id))
    }

    /// An issued id with no record: corruption (the latch trips).
    fn missing_record(&self, id: BlockId) -> StoreError {
        self.corrupt(format!("entry {} has no record", id.0))
    }

    /// A stored link that reaches `id` off the height it names (a skip or `prev` pointer that
    /// does not step down as Core's `BuildSkip` built it): corruption (the latch trips).
    pub(crate) fn off_height(&self, id: BlockId) -> StoreError {
        self.corrupt(format!("entry {}: a stored link reaches it off its height", id.0))
    }

    /// Core `LookupBlockIndex` over the resident index, then the store.
    pub(crate) fn try_lookup(&self, h: &BlockHash) -> Result<Option<BlockId>, StoreError> {
        match self.index.lookup(&self.arena, h) {
            Some(id) => Ok(Some(id)),
            None if R::EVICTS => Ok(self.cold_hashed(h)?.map(|(id, _)| id)),
            None => Ok(None),
        }
    }

    /// `id`'s walk pointers, resident or cold.
    pub(crate) fn try_link(&self, id: BlockId) -> Result<Option<Link>, StoreError> {
        match self.link(id) {
            Some(link) => Ok(Some(link)),
            // `try_node` looks again after its watermark read: this miss alone proves nothing.
            None => Ok(self.try_node(id)?.map(|n| n.link())),
        }
    }

    /// A cold record as a [`Node`]: its `prev` resolved by hash, except at the floor, where the
    /// lowest held entry has none (D-C4-1).
    fn cold_node(&self, rec: HeaderRecord) -> Result<Node, StoreError> {
        let prev = match rec.height > self.floor {
            true => Some(self.cold_parent(&rec)?),
            false => None,
        };
        Ok(Node {
            hash: rec.hash,
            header: rec.header,
            height: rec.height,
            prev,
            skip: rec.skip,
            chain_work: rec.chain_work,
            time_max: rec.time_max,
            mtp: MTP_UNKNOWN,
            context: rec.kind == RecordKind::Context,
            status: rec.status,
            resident: false,
        })
    }

    /// A cold record's parent by hash, one height below it: a record whose parent is missing
    /// or at another height is corruption, so no walk down cold `prev`s can climb or loop
    /// (Sol r1 m1). The lookup reads the parent's height anyway (resident or its record).
    fn cold_parent(&self, rec: &HeaderRecord) -> Result<BlockId, StoreError> {
        let h = &rec.header.prev_hash;
        let resident = self.index.lookup(&self.arena, h).and_then(|id| {
            // An entry evicted since the lookup is read from its record.
            self.arena.imm(id, |e| (id, e.height))
        });
        let parent = match resident {
            Some(p) => Some(p),
            None => self.cold_hashed(h)?,
        };
        match parent {
            Some((id, height)) if height.checked_add(1) == Some(rec.height) => Ok(id),
            Some((id, height)) => Err(self.corrupt(format!(
                "record {:?} at height {}: parent {id:?} at height {height}",
                rec.id, rec.height
            ))),
            None => Err(self.corrupt(format!("record {:?}: parent not found", rec.id))),
        }
    }

    /// A cold read by id, through SB5's cold cache (SB5 §5.2); a failure trips the latch.
    fn cold_by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        let rec = match &self.cold {
            Some(cache) => cache
                .get_or_load(id, &self.store, &self.issued)
                .map(|r| r.map(Arc::unwrap_or_clone)),
            None => self.store.by_id(id),
        };
        rec.inspect_err(|e| self.fail.read_failed(&e.to_string()))
    }

    /// A cold read by hash, inserted into the cache by its id (see [`Shared::cold_by_id`]): the
    /// record's id and height, refusing a record stored under another hash (the by-hash twin of
    /// [`Shared::try_node`]'s id check). Only these leave the cache: no record is cloned (PF0).
    fn cold_hashed(&self, h: &BlockHash) -> Result<Option<(BlockId, u32)>, StoreError> {
        let key = |rec: &HeaderRecord| (rec.id, rec.height, rec.hash);
        let rec = match &self.cold {
            Some(cache) => {
                cache.load_by_hash(h, &self.store, &self.issued).map(|r| r.as_deref().map(key))
            }
            None => self.store.by_hash(h).map(|r| r.as_ref().map(key)),
        };
        match rec.inspect_err(|e| self.fail.read_failed(&e.to_string()))? {
            Some((id, _, hash)) if hash != *h => {
                Err(self.corrupt(format!("hash {h:?} read as record {} with hash {hash:?}", id.0)))
            }
            rec => Ok(rec.map(|(id, height, _)| (id, height))),
        }
    }

    /// A page of the store's committed children of `prev` from `from` up (§3.7's SPV
    /// children); a failure trips the latch, as does a page out of id order.
    pub(crate) fn cold_children(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        let n = self
            .store
            .children_from(prev, from, out)
            .inspect_err(|e| self.fail.read_failed(&e.to_string()))?;
        self.checked_page("children", from, out, n, |&id| id)
    }

    /// A page of the store's leaves from `from` up (§3.7, §6.2's cold leaves); a failure trips
    /// the latch, as does a page out of id order.
    pub(crate) fn cold_leaves(
        &self,
        from: BlockId,
        out: &mut [(BlockId, Work)],
    ) -> Result<usize, StoreError> {
        let n = self
            .store
            .leaves_from(from, out)
            .inspect_err(|e| self.fail.read_failed(&e.to_string()))?;
        self.checked_page("leaves", from, out, n, |&(id, _)| id)
    }

    /// `n`, when `page[..n]` holds ids ascending from `from` up, the order the chain merges
    /// and resumes in; otherwise the store is corrupt.
    fn checked_page<T>(
        &self,
        what: &str,
        from: BlockId,
        page: &[T],
        n: usize,
        id: impl Fn(&T) -> BlockId,
    ) -> Result<usize, StoreError> {
        let ascending = |p: &[T]| {
            p.first().is_none_or(|f| id(f) >= from) && p.is_sorted_by(|a, b| id(a) < id(b))
        };
        match page.get(..n).is_some_and(ascending) {
            true => Ok(n),
            false => Err(self.corrupt(format!("a page of {what} from {} out of id order", from.0))),
        }
    }

    /// A store that contradicts itself (a missing parent or issued id) is corruption: the latch
    /// trips and the client stops (SB0 §4.4); the error is [`StoreError::Corrupt`].
    pub(crate) fn corrupt(&self, what: String) -> StoreError {
        let e = StoreError::Corrupt(what);
        self.fail.read_failed(&e.to_string());
        e
    }

    /// §9.1 stage 5, after the tip lock is released (SB5-A5, A10): SB5's cold cache drops the
    /// step's records, or all of them for [`ColdInvalidation::Clear`].
    pub(crate) fn invalidate_cold(&self, inv: ColdInvalidation) {
        let Some(cache) = &self.cold else {
            return;
        };
        match inv {
            ColdInvalidation::Ids(ids) => cache.invalidate(&ids),
            ColdInvalidation::Clear => cache.clear(),
        }
    }
}

impl<S: HeaderStore, R: Residency> ReadBackend for Shared<S, R> {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.index.lookup(&self.arena, h)
    }

    fn entry(&self, id: BlockId) -> Option<EntryView> {
        self.arena.with(id, |imm, e| {
            let g = e.group.read();
            EntryView {
                id,
                hash: imm.hash,
                header: imm.header,
                prev: imm.prev,
                height: imm.height,
                chain_work: imm.chain_work,
                mtp: imm.mtp,
                time_max: imm.time_max,
                status: g.status,
                n_tx: g.n_tx,
                file: g.file,
                data_pos: g.data_pos,
                undo_pos: g.undo_pos,
            }
        })
    }

    fn link(&self, id: BlockId) -> Option<Link> {
        Shared::link(self, id)
    }

    fn group(&self, id: BlockId) -> Option<MutableGroup> {
        self.arena.group(id)
    }

    fn readers(&self) -> &ReaderState {
        &self.readers
    }

    fn floor(&self) -> u32 {
        self.floor
    }

    fn anchor(&self) -> u32 {
        self.anchor
    }

    fn try_lookup(&self, h: &BlockHash) -> Result<Option<BlockId>, StoreError> {
        Shared::try_lookup(self, h)
    }

    fn try_entry(&self, id: BlockId) -> Result<Option<EntryView>, StoreError> {
        if let Some(view) = self.entry(id) {
            return Ok(Some(view));
        }
        // As in `try_link`: `try_node` looks again after its watermark read.
        Ok(self.try_node(id)?.map(|n| EntryView {
            id,
            hash: n.hash,
            header: n.header,
            prev: n.prev,
            height: n.height,
            chain_work: n.chain_work,
            mtp: n.mtp,
            time_max: n.time_max,
            status: n.status,
            n_tx: 0,
            file: None,
            data_pos: 0,
            undo_pos: 0,
        }))
    }

    fn try_link(&self, id: BlockId) -> Result<Option<Link>, StoreError> {
        Shared::try_link(self, id)
    }

    fn missing(&self, id: BlockId) -> StoreError {
        self.missing_record(id)
    }

    fn off_height(&self, id: BlockId) -> StoreError {
        Shared::off_height(self, id)
    }
}
