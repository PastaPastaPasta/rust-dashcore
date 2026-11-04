//! Test support for other crates' tests and this crate's integration tests (`test-util`). Not a
//! stable API.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, DefaultHasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::pow::block_proof;
use dash_consensus::primitives::Header;
use dash_num::CompactTarget;
use dash_primitives::{BlockHash, BlockHeader};

use crate::chain::MutableGroup;
use crate::error::{FailStop, StoreError};
pub use crate::fault::{PausePoint, in_residency, in_slot_section};
use crate::feed::{
    ChainEvent, ControlMsg, FeedConfig, FeedReader, FeedShared, FeedSlot, Push, SendControl,
};
use crate::mem_store::MemHeaderStore;
use crate::notify::Notify;
use crate::status::Status;
use crate::step::{
    ActiveDiff, ChainMeta, Durability, HeaderStepSink as _, MetaDelta, StepRow, StepTail,
};
use crate::store::{HeaderRecord, HeaderStore, RecordKind};
use crate::{
    BlockId, ChainFeeds, FeedId, FeedSource, Handoff, Needs, Next, SubscriberFeed, Work, walk,
};

/// One slot with its reader, driven directly: the producer half of a feed without a `TipCell`.
pub struct SlotHarness {
    slot: Arc<FeedSlot>,
    shared: Arc<FeedShared>,
}

/// The harness's latch: a slot alone reads nothing, so it never trips.
struct NoReads;

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        unreachable!("the slot harness reads nothing ({what})")
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

/// `SlotHarness::push`'s result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    Pushed,
    Full,
    Closed,
}

impl SlotHarness {
    /// A slot reserved at `cfg.capacity` and its reader.
    pub fn new(cfg: FeedConfig) -> (SlotHarness, FeedReader) {
        let shared = Arc::new(FeedShared::new(cfg, Arc::new(NoReads)));
        let (slot, bufs) = FeedSlot::new(cfg.capacity);
        let reader = FeedReader::new(slot.clone(), shared.clone(), bufs);
        (
            SlotHarness {
                slot,
                shared,
            },
            reader,
        )
    }

    /// Another feed on the same chain-wide shared state (wakes, config), as a second
    /// `register` makes: what this allocates is one feed's share (§6.1).
    pub fn sibling(&self) -> (SlotHarness, FeedReader) {
        let (slot, bufs) = FeedSlot::new(self.shared.cfg.capacity);
        let reader = FeedReader::new(slot.clone(), self.shared.clone(), bufs);
        (
            SlotHarness {
                slot,
                shared: self.shared.clone(),
            },
            reader,
        )
    }

    /// The producer's `try_push`, then the subscriber's wake after the section.
    pub fn push(&self, e: ChainEvent) -> Pushed {
        match self.slot.try_push(e) {
            Push::Pushed(w) => {
                if let Some(w) = w {
                    w.notify();
                }
                Pushed::Pushed
            }
            Push::Full => Pushed::Full,
            Push::Closed => Pushed::Closed,
        }
    }

    pub fn send_control(&self, c: ControlMsg) -> SendControl {
        self.slot.send_control(c)
    }

    pub fn control_outstanding(&self) -> u8 {
        self.slot.control_outstanding()
    }

    pub fn set_producer_wake(&self, w: Notify) {
        self.shared.set_producer_wake(w);
    }

    pub fn set_close_hook(&self, w: Notify) {
        self.shared.set_close_hook(w);
    }

    /// `deregister` steps 2–3.
    pub fn close(&self) {
        self.slot.close(&self.shared);
    }

    /// The pause point inside `next()`'s slot section.
    pub fn intake_pause(&self) -> &PausePoint {
        &self.slot.faults.intake
    }

    /// The planted negative control: the intake reallocates inside its section.
    pub fn plant_alloc_in_intake(&self, on: bool) {
        self.slot.faults.alloc_in_intake.store(on, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Appends `headers` to an SPV `store` as the header steps would write them, in one step: each
/// an accepted record at `VALID_TREE` with no mark, and the tip moved to the most work (the lower
/// id on a tie). Nothing is hashed: each pair's hash stands for its header's X11, so a bench can
/// build a long chain from synthetic hashes below the target instead of mining it. Each parent
/// is in `store` or earlier in `headers`. The store has had a committed step, and its tip is its
/// best header: the tip moves to the most work, where a real step would move it to the fork
/// target.
///
/// # Panics
///
/// On a store with no chain, a parent that is not held, or a held record off its height.
pub fn append_spv_records(
    store: &mut MemHeaderStore,
    headers: &[(BlockHeader, BlockHash)],
) -> Result<(), StoreError> {
    let meta = store.chain_meta()?.expect("a chain");
    let floor = store.by_id(BlockId(0))?.expect("the lowest record").height;
    let status = MutableGroup::default().status.raise_validity(Status::VALID_TREE).0;
    let first = meta.next_id.0;
    let mut new: Vec<(HeaderRecord, BlockId)> = Vec::with_capacity(headers.len());
    // A fixed hasher, so that a bench's setup repeats across processes.
    let mut ids: Ids = HashMap::with_capacity_and_hasher(headers.len(), Default::default());
    for (i, &(header, hash)) in headers.iter().enumerate() {
        let id = BlockId(first + i as u32);
        let prev = match ids.get(&header.prev_hash) {
            Some(&p) => p,
            None => store.by_hash(&header.prev_hash)?.expect("a held parent").id,
        };
        let p = record(store, &new, first, prev)?;
        let height = p.height + 1;
        let skip = match walk::skip_height(height) >= floor {
            true => {
                let link = |id| link(store, &new, &ids, first, id);
                walk::build_skip(Some(prev), height, link, walk::resident_off_height)?
            }
            false => None,
        };
        let rec = HeaderRecord {
            id,
            hash,
            height,
            header,
            skip,
            status,
            chain_work: p.chain_work + block_proof(CompactTarget::new(header.bits)),
            time_max: p.time_max.max(header.time),
            kind: RecordKind::Accepted,
        };
        new.push((rec, prev));
        ids.insert(hash, id);
    }
    let old = record(store, &new, first, meta.tip)?;
    let best = record(store, &new, first, meta.best_header)?;
    let best = new.iter().map(|(r, _)| r).fold(best, |b, r| match r.chain_work > b.chain_work {
        true => r.clone(),
        false => b,
    });
    let active = match best.id == old.id {
        true => None,
        false => {
            let lca = |id| link(store, &new, &ids, first, id);
            let fork = walk::last_common_ancestor(old.id, best.id, lca, walk::resident_off_height)?
                .expect("one chain");
            Some(ActiveDiff {
                from_height: record(store, &new, first, fork)?.height + 1,
                new_tip: best.id,
                new_height: best.height,
                old_height: old.height,
            })
        }
    };
    let next_id = BlockId(first + new.len() as u32);
    let rows: Vec<StepRow> =
        new.into_iter().flat_map(|(r, prev)| r.step_rows(Some(prev))).collect();
    store.begin_step();
    store.rows(&rows)?;
    let tail = StepTail {
        active,
        cl: None,
        meta: MetaDelta {
            next_id,
            tip: best.id,
            best_header: best.id,
        },
    };
    store.commit_step(&tail, Durability::Buffered)
}

type Ids = HashMap<BlockHash, BlockId, BuildHasherDefault<DefaultHasher>>;

/// `id`'s record: from `new` at and above `first`, else from `store`.
fn record(
    store: &MemHeaderStore,
    new: &[(HeaderRecord, BlockId)],
    first: u32,
    id: BlockId,
) -> Result<HeaderRecord, StoreError> {
    match id.0.checked_sub(first) {
        Some(i) => Ok(new[i as usize].0.clone()),
        None => Ok(store.by_id(id)?.expect("a held record")),
    }
}

/// `id`'s skip walk links; a stored record's parent is found by hash, none below the floor.
fn link(
    store: &MemHeaderStore,
    new: &[(HeaderRecord, BlockId)],
    ids: &Ids,
    first: u32,
    id: BlockId,
) -> Result<walk::Link, StoreError> {
    let r = record(store, new, first, id)?;
    let prev = match ids.get(&r.header.prev_hash) {
        Some(&p) => Some(p),
        None => store.by_hash(&r.header.prev_hash)?.map(|p| p.id),
    };
    Ok(walk::Link {
        height: r.height,
        prev,
        skip: r.skip,
    })
}

/// What [`SpvRig::step`] is doing, for a test that counts the header writer's allocations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RigMark {
    /// Stage 3 hands the store a batch of this many rows.
    Rows(usize),
    /// The store's own work starts (`begin_step`, a stage-3 batch, the commit): its tables stand
    /// in for SQLite's pages, outside the step budget (SB5 §6.2).
    StoreIn,
    /// The store's work ends.
    StoreOut,
    /// Stage 4 is blocked on a target that lacks events: the test drains its readers.
    Wait,
}

/// Regtest's `nBits`, its proof-of-work limit: about two tries per header.
const REGTEST_BITS: u32 = 0x207f_ffff;
/// The rig's anchor height: post-DGW on regtest (DGW starts at mainnet's 34,140).
pub const RIG_ANCHOR: u32 = 40_000;
/// The rig's anchor pack is mined from here, 150 s apart; the clock is at 1,700,000,000.
pub const RIG_T0: u32 = 1_690_000_000;
/// The anchor pack's headers, the anchor last.
const RIG_PACK_LEN: u32 = 25;
/// One arena slot's size. §6.3's residency rows estimate 200 B (SB4 §3.8); A-F12 measures.
pub const ARENA_ENTRY_BYTES: usize = size_of::<crate::entry::Entry>();
/// One bucket of SPV's status overlay: an entry (`BlockId`, SB4's `Mark`) and a control byte.
/// §6.2's M(m) is this per bucket of the old and new tables; the step budget's marks account
/// sizes the live table from its own type.
pub const OVERLAY_BUCKET_BYTES: usize = size_of::<(BlockId, crate::chain::Mark)>() + 1;
/// One arena chunk's allocations (§6.3): its 1,024 entries and its `Arc`, whose `Chunk` holds the
/// entries' box and the evicted bits.
pub const ARENA_CHUNK_BYTES: usize = crate::arena::CHUNK as usize * ARENA_ENTRY_BYTES
    + 2 * size_of::<usize>()
    + size_of::<crate::arena::Chunk>();
/// One `Evicting` second-level table: a chunk slot per 1,024 ids, for 2^20 ids (§6.3's fixed
/// tables, beside the first level's, which the arena allocates at construction).
pub const ARENA_TABLE_BYTES: usize =
    crate::arena::L2 as usize * size_of::<arc_swap::ArcSwapOption<crate::arena::Chunk>>();

/// A hashbrown table of `entry`-byte entries at `buckets` (a power of two, at least 4): the
/// entries, then a control byte per bucket and per group slot, 16 on SSE2.
pub const fn table_bytes(buckets: usize, entry: usize) -> usize {
    buckets * (entry + 1) + 16
}

/// The hash index's table at `buckets`.
pub const fn index_table_bytes(buckets: usize) -> usize {
    table_bytes(buckets, size_of::<BlockId>())
}

/// A hashbrown table of `entry`-byte entries that has held at most `n` at once, with its
/// rehash transient (DEC-187 amendment 2 (c)). An insert into a full table of b buckets (7b/8
/// entries, b − 1 below 8) rehashes it in place when the live entries fit half of that, and
/// otherwise allocates 2b and frees b after moving them. So the table never passes the smallest
/// b whose half capacity holds n, and while it grows into b the old table of b/2 is live beside
/// it.
pub const fn grown_table_bytes(n: usize, entry: usize) -> usize {
    let mut b = 4;
    loop {
        let capacity = if b < 8 {
            b - 1
        } else {
            b / 8 * 7
        };
        if n <= capacity / 2 {
            return table_bytes(b, entry) + table_bytes(b / 2, entry);
        }
        b *= 2;
    }
}

/// §6.3's hash-index term for at most `n` live entries, with its rehash transient.
pub const fn index_bytes(n: usize) -> usize {
    grown_table_bytes(n, size_of::<BlockId>())
}

/// A leaf of std's B-tree over SPV's resident set, `(height, id)` keys: 11 keys, the parent
/// pointer, its index and the length. The layout is std's; `exact_resident_nodes` checks it.
pub const RESIDENT_LEAF_BYTES: usize =
    (size_of::<usize>() + 4 + 11 * size_of::<(u32, BlockId)>()).next_multiple_of(8);
/// An internal node: a leaf and its 12 edges.
pub const RESIDENT_INTERNAL_BYTES: usize = RESIDENT_LEAF_BYTES + 12 * size_of::<usize>();

/// §6.3's resident-set term for at most `n` entries. std's B-tree keeps every node but the root
/// at 5 keys or more, so there are at most n/5 + 1 leaves, and every internal node but the root
/// has 6 children or more, so at most leaves/5 + 1 internal nodes (a geometric sum).
pub const fn resident_bytes(n: usize) -> usize {
    let leaves = n / 5 + 1;
    leaves * RESIDENT_LEAF_BYTES + (leaves / 5 + 1) * RESIDENT_INTERNAL_BYTES
}

/// §6.3's `ActiveChain` term for `heights` resident heights: at most heights/1,024 + 2 leaves,
/// and per level to the deepest at most the level below's nodes / 64 + 2 inner nodes.
pub const fn active_bytes(heights: usize) -> usize {
    let mut nodes = heights / crate::active::LEAF as usize + 2;
    let mut bytes = nodes * crate::active::LEAF_NODE_BYTES;
    let mut level = 0;
    while level < crate::active::MAX_DEPTH {
        nodes = nodes / 64 + 2;
        bytes += nodes * crate::active::INNER_NODE_BYTES;
        level += 1;
    }
    bytes
}

/// An active chain at the deepest heights, `u32::MAX − 2,048` on, holding two, with a retained
/// view of it: its next push copies the whole root-to-leaf path out of the view, which is §6.2's
/// path term for one path.
pub struct DeepActive {
    chain: crate::active::ActiveChain,
    _view: crate::active::ActiveChain,
}

impl DeepActive {
    pub fn new() -> DeepActive {
        let floor = u32::MAX - 2 * crate::active::LEAF;
        let anchor = floor + crate::anchor::CONTEXT_ENTRIES;
        let mut chain = crate::active::ActiveChain::starting_at(anchor);
        chain.push(BlockId(0));
        chain.push(BlockId(1));
        DeepActive {
            _view: chain.clone(),
            chain,
        }
    }

    /// Pushes one id.
    pub fn push(&mut self) {
        self.chain.push(BlockId(2));
    }
}

impl Default for DeepActive {
    fn default() -> DeepActive {
        DeepActive::new()
    }
}

/// A regtest SPV chain anchored at [`RIG_ANCHOR`] over a `MemHeaderStore`, its pack committed,
/// and SB4 §9.1's header step with SPV's segmented stage 4: for integration tests that run
/// the real step under a counting allocator (A-F12).
pub struct SpvRig {
    pub chain: crate::HeaderChain<MemHeaderStore, crate::Evicting>,
    pub store: MemHeaderStore,
    pub params: Arc<dash_params::ChainSetup>,
    cfg: crate::ChainConfig,
    svc: crate::Services,
    /// The anchor pack's last header (the anchor).
    pub anchor: Header,
}

impl SpvRig {
    /// §6.2's fixed term: `measure` runs one release's publication, with the snapshot and the
    /// status facts both new and the old ones held.
    pub fn republish(&self, measure: impl FnOnce(&mut dyn FnMut())) {
        self.chain.republish(measure);
    }

    /// Window `w`, `feeds`, a cold cache of `cold_cache` records, and SPORK_19 on or off.
    pub fn new(w: u32, feeds: FeedConfig, cold_cache: u32, chainlocks: bool) -> SpvRig {
        use dash_params::{ChainParamsBuilder, Network, Release};
        let params = Arc::new(
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
                .build()
                .expect("Core's parameters"),
        );
        let svc = crate::Services {
            clock: Arc::new(dash_clock::MockClock::at(1_700_000_000)),
            sporks: Arc::new(crate::StaticSporks::for_network(&params, Some(chainlocks))),
            fail: Arc::new(RigLatch::default()),
        };
        let mut cfg = crate::ChainConfig::spv(&params, w);
        cfg.feeds = feeds;
        cfg.cold_cache = cold_cache;
        let pack_headers = mine_regtest(&params, BlockHash::from([7; 32]), RIG_T0, RIG_PACK_LEN);
        let anchor = pack_headers[RIG_PACK_LEN as usize - 1];
        let pack = crate::AnchorPack {
            height: RIG_ANCHOR,
            hash: anchor.hash(),
            chain_work: block_proof(CompactTarget::new(REGTEST_BITS))
                .wrapping_mul(Work::from_u64(1 << 20)),
            headers: pack_headers
                .iter()
                .map(|h| *h.raw())
                .collect::<Vec<_>>()
                .try_into()
                .expect("the pack's length"),
        };
        let store = MemHeaderStore::new();
        let chain = crate::HeaderChain::new_anchored(
            params.clone(),
            cfg.clone(),
            svc.clone(),
            store.clone(),
            &pack,
        )
        .expect("a valid pack");
        store
            .reset(ChainMeta {
                schema: crate::CHAIN_SCHEMA,
                anchor: (pack.hash, pack.height),
                anchor_state: crate::AnchorState::Candidate,
                next_id: BlockId(0),
                tip: BlockId(0),
                best_header: BlockId(0),
                sync_seq: 0,
                cl: crate::ClPersisted {
                    best: None,
                    floor: None,
                    enabled_observed: false,
                },
            })
            .expect("reset");
        let mut rig = SpvRig {
            chain,
            store,
            params,
            cfg,
            svc,
            anchor,
        };
        rig.step(&[], &[], &mut |_| {}).expect("the pack's step");
        rig
    }

    /// The chain the store's last commit holds, reopened (SB5 §8).
    pub fn reopen(
        &self,
    ) -> Result<crate::HeaderChain<MemHeaderStore, crate::Evicting>, crate::OpenError> {
        crate::HeaderChain::open(
            self.params.clone(),
            self.cfg.clone(),
            self.svc.clone(),
            self.store.clone(),
        )
    }

    /// `n` regtest headers mined on `prev`, the first at `time`, 150 s apart.
    pub fn mine(&self, prev: BlockHash, time: u32, n: u32) -> Vec<Header> {
        mine_regtest(&self.params, prev, time, n)
    }

    /// `n` headers on the anchor, the first `skew` s after the slot that follows the pack.
    pub fn mine_on_anchor(&self, skew: u32, n: u32) -> Vec<Header> {
        self.mine(self.anchor.hash(), RIG_T0 + 150 * RIG_PACK_LEN + skew, n)
    }

    /// `headers` in steps of `step_headers`, no locks, nothing heard.
    pub fn extend(&mut self, headers: &[Header]) {
        for (i, c) in headers.chunks(self.cfg.feeds.step_headers as usize).enumerate() {
            self.step(c, &[], &mut |_| {}).unwrap_or_else(|e| panic!("step {i}: {e}"));
        }
    }

    /// The batch [`SpvRig::step_batch`] takes: `headers` and their hashes, as the host decodes
    /// them before the step (§6.2 leaves it outside the step budget).
    pub fn batch(headers: &[Header]) -> crate::HeaderBatch {
        crate::HeaderBatch {
            hashes: headers.iter().map(|h| h.hash()).collect(),
            headers: headers.to_vec(),
        }
    }

    /// [`SpvRig::step_batch`] on `headers`' batch, built inside the step.
    pub fn step(
        &mut self,
        headers: &[Header],
        locks: &[crate::ChainLockRef],
        marks: &mut dyn FnMut(RigMark),
    ) -> Result<crate::TipSummary, crate::ChainError> {
        let batch = (!headers.is_empty()).then(|| SpvRig::batch(headers));
        self.step_batch(batch, locks, marks)
    }

    /// One header step: `accept` of `batch`, the `locks` against one `VirtualTip`,
    /// `fork_target`, stage 3 committed Buffered, `activate_segmented`, `records_committed`.
    /// `marks` hears each [`RigMark`], `Wait` once per blocked wait.
    pub fn step_batch(
        &mut self,
        batch: Option<crate::HeaderBatch>,
        locks: &[crate::ChainLockRef],
        marks: &mut dyn FnMut(RigMark),
    ) -> Result<crate::TipSummary, crate::ChainError> {
        let c = &mut self.chain;
        if let Some(batch) = batch {
            c.accept(batch, None)?;
        }
        let mut vt = c.virtual_tip();
        for &cl in locks {
            c.apply_chainlock(cl, &mut vt).map_err(|e| match e {
                crate::ApplyError::Chain(e) => e,
                crate::ApplyError::Host(e) => e,
            })?;
        }
        // Stage 1 builds no plan: stage 3 reads the move's shape (`tip_target`), and each
        // sub-step of stage 4 plans its own.
        let target = c.fork_target(&vt)?;
        let e = c.reader().try_entry(target.id)?.expect("the target is held");
        let to = crate::BlockRef {
            id: e.id,
            hash: e.hash,
            height: e.height,
        };
        let moved = c.private_tip().expect("a tip").0.id != target.id;
        in_store(marks, || self.store.begin_step());
        let mut sink = MarkedSink {
            store: &mut self.store,
            marks,
        };
        let tail = c.write_header_step(moved.then_some(to), &mut sink)?;
        let MarkedSink {
            store,
            marks,
        } = sink;
        in_store(marks, || store.commit_step(&tail, Durability::Buffered))?;
        let summary = c.activate_segmented(to, &mut || marks(RigMark::Wait))?;
        c.records_committed();
        Ok(summary)
    }

    /// Drains every reader, then retries the writer's pending fanout, until nothing is pending:
    /// what a writer runs after each step before the next.
    pub fn settle(&mut self, subs: &mut [Box<dyn SubscriberFeed>]) {
        loop {
            for f in subs.iter_mut() {
                drain(&mut **f);
            }
            if self.chain.pending_is_empty() {
                return;
            }
            self.chain.retry_pending();
        }
    }
}

/// A live subscriber registered at the published tip.
pub fn subscribe(feeds: &ChainFeeds) -> (FeedId, Box<dyn SubscriberFeed>) {
    let id = feeds.register(Needs::default()).expect("registers");
    let tip = FeedSource::<()>::snapshot(feeds).tip();
    match feeds.subscribe_at(id, &tip, &mut || Ok::<(), ()>(())) {
        Ok(Handoff::Live {
            feed,
            ..
        }) => (id, feed),
        other => panic!("subscribe_at: {:?}", other.err()),
    }
}

/// Takes every event and control `f` holds now; the events' count.
pub fn drain(f: &mut dyn SubscriberFeed) -> usize {
    let mut n = 0;
    loop {
        match f.next() {
            Next::Event(_) => {
                f.pop().expect("selected");
                n += 1;
            }
            Next::Control(_) => {}
            Next::Empty => return n,
        }
    }
}

/// `f` between [`RigMark::StoreIn`] and [`RigMark::StoreOut`].
fn in_store<T>(marks: &mut dyn FnMut(RigMark), f: impl FnOnce() -> T) -> T {
    marks(RigMark::StoreIn);
    let out = f();
    marks(RigMark::StoreOut);
    out
}

/// The store's sink: [`RigMark::Rows`], then the batch inside [`in_store`].
struct MarkedSink<'a> {
    store: &'a mut MemHeaderStore,
    marks: &'a mut dyn FnMut(RigMark),
}

impl crate::HeaderStepSink for MarkedSink<'_> {
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), StoreError> {
        (self.marks)(RigMark::Rows(batch.len()));
        in_store(&mut *self.marks, || self.store.rows(batch))
    }
}

/// The rig's fail-stop latch.
#[derive(Default)]
struct RigLatch(AtomicBool);

impl FailStop for RigLatch {
    fn read_failed(&self, _what: &str) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn is_tripped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// `n` regtest headers mined on `prev`, the first at `time`, 150 s apart.
fn mine_regtest(
    p: &dash_params::ChainSetup,
    mut prev: BlockHash,
    time: u32,
    n: u32,
) -> Vec<Header> {
    use dash_consensus::header::HeaderParams;
    use dash_consensus::pow::check_proof_of_work;
    let pow = *HeaderParams::from_chain_params(p).expect("regtest").pow();
    let target = CompactTarget::new(REGTEST_BITS);
    (0..n)
        .map(|i| {
            let h = (0..)
                .map(|nonce| {
                    Header::new(BlockHeader {
                        version: 0x2000_0000,
                        prev_hash: prev,
                        time: time + 150 * i,
                        bits: REGTEST_BITS,
                        nonce,
                        ..BlockHeader::default()
                    })
                })
                .find(|h| check_proof_of_work(h.hash().as_bytes(), target, &pow))
                .expect("a nonce");
            prev = h.hash();
            h
        })
        .collect()
}
