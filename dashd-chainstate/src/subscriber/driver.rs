//! `SubscriberDriver` (CH2 §2.9, §4.4.1): one subscriber's progress p, its durable locator L and
//! the journal (L, p], on the subscriber's own engine.
//!
//! - **Syncing** ([`sync_step`](SubscriberDriver::sync_step)): catch-up from a coherent snapshot,
//!   with a capped cadence commit, then F10's handoff at V's tip (Core's `ThreadSync`,
//!   `index/base.cpp:153-209`).
//! - **Synced** ([`step`](SubscriberDriver::step)): one feed event or control per call, under
//!   §4.4.1's rules: rule 1's sequence-number case test before every event, compression and
//!   parking at the limit (rule 3), the rewind gate (rule 4), and the capped eager commit after a
//!   disconnect above L (rule 6). A handler never blocks on another component.
//!
//! Commits (§2.9 item 5) are private: (a) at X on p's path at or above L; (b) the eager commit at
//! `LCA(pprev, commit_cap)`; (c) the rewind commit at F below L, deferred to the next connect, a
//! `RewindRequest` or a `FlushCommit` (n5). Every one is capped at the hub's `commit_cap` (R11a).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use dash_chain::{
    BlockRef, ChainEvent, ChainSnapshot, ControlMsg, Handoff, IndexAccess, Next, Notify, Status,
    SubscribeError, SubscriberFeed, TipStep,
};
use dash_consensus::primitives::BlockHash;
use dashd_store::{Cf, Commit, KvBatch, KvEngine, KvRead, StoreError};

use super::data::{
    BlockRows, BlockSource, ConnectedBlock, DisconnectedBlock, DurableBelow, KeyValue, Row,
    RowClass, SubscriberData, height_key,
};
use super::{FeedTable, SubscriberError, SubscriberId};
use crate::flush::{
    CHAIN_STATE_FLUSHED, CommitTarget, FlushCommit, Flushed, Registration, RewindRequest,
    SubscriberHandle, SubscriberHub,
};
use crate::planted::planted;
use crate::store::{GuardedEngine, GuardedRead};
use crate::tuning::EngineTuning;

/// Core's `m_synced` (`index/base.cpp:90`, `:169`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubscriberClass {
    Synced,
    Syncing,
}

/// Why a locator was written (ST3a's `LocatorKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocatorKind {
    CommitStep,
    Checkpoint,
    Cadence,
    Rewind,
    EagerRewind,
}

/// A Synced step's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubStep {
    Progress,
    /// At its limit with nothing to compress: it posted `FlushDue(DMax)` and waits for a
    /// `FlushCommit`, a `RewindRequest` or a new event (rule 3).
    Parked,
    /// Nothing queued; its waker is set.
    Idle,
}

/// A Syncing step's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncProgress {
    Progress,
    /// F10's handoff succeeded: the driver is Synced.
    Live,
    /// At its limit, and its capped commit did not raise L: it posted `FlushDue` and waits for
    /// the next `T_ack` (its watch on the hub).
    Blocked,
}

/// The driver's tuning (§2.5.1, §2.9 item 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DriverConfig {
    /// `EngineTuning::journal_budget`.
    pub journal_budget: u64,
    /// `EngineTuning::unbounded_delete_span`: the blind delete's span with no `d_max`.
    pub unbounded_delete_span: u32,
    /// A block-count bound on the journal, beside its bytes.
    pub d_max: Option<u32>,
    /// A Syncing subscriber's capped commit every `cadence` blocks (Core's
    /// `SYNC_LOCATOR_WRITE_INTERVAL` is a timer; the seam counts blocks).
    pub cadence: u32,
}

impl DriverConfig {
    pub fn from_tuning(t: &impl EngineTuning, d_max: Option<u32>, cadence: u32) -> Self {
        Self {
            journal_budget: t.journal_budget(),
            unbounded_delete_span: t.unbounded_delete_span(),
            d_max,
            cadence: cadence.max(1),
        }
    }

    /// `D_range` (§2.9 item 5c).
    fn delete_span(&self) -> u32 {
        self.d_max.unwrap_or(self.unbounded_delete_span)
    }
}

/// One journal entry: a block of (L, p] (§2.9 item 2).
struct Entry<S> {
    block: BlockRef,
    keyed: Vec<Row>,
    /// Its height rows, archived if it is disconnected.
    archive_src: Vec<Row>,
    state: S,
    bytes: u64,
}

enum Connect {
    Done,
    /// The entry was not admitted (§2.9 item 2): it was discarded, and is recomputed later.
    AtLimit,
}

/// The target a Synced event is applied toward while its tip event is ahead (rule 1's case B).
#[derive(Clone, Copy)]
enum Toward {
    /// An outstanding `FlushCommit`'s `new`: connects off its leg are dropped.
    Commit(BlockRef),
    /// A flushed target's tip (D38): nothing is dropped, as Core's one queue drops nothing
    /// (D46).
    Flushed(BlockRef),
}

impl Toward {
    fn tip(self) -> BlockRef {
        match self {
            Self::Commit(b) | Self::Flushed(b) => b,
        }
    }
}

/// What a Synced event did.
enum Applied {
    Done,
    AtLimit(BlockRef),
}

pub struct SubscriberDriver<D: SubscriberData> {
    data: D,
    engine: Arc<GuardedEngine<D::E>>,
    hub: Arc<SubscriberHub>,
    table: Arc<dyn FeedTable>,
    handle: SubscriberHandle,
    idx: Arc<dyn IndexAccess>,
    blocks: Arc<dyn BlockSource>,
    cfg: DriverConfig,
    /// L and the state at L.
    durable: Option<BlockRef>,
    durable_state: D::State,
    /// p and the state at p.
    progress: Option<BlockRef>,
    progress_state: D::State,
    journal: VecDeque<Entry<D::State>>,
    journal_bytes: u64,
    /// O-R2-M2: the state with the process's highest resource marks.
    latest: D::State,
    /// MPIX-11's `best_block()`.
    best: Option<BlockRef>,
    class: SubscriberClass,
    feed: Option<Box<dyn SubscriberFeed>>,
    waker: Option<Notify>,
    /// Rule 1: the cut, the last `seq` processed, dropped or ignored, and the compressed `seq`s
    /// ahead of it.
    seq_synced: u64,
    last_seq: u64,
    skipped: BTreeSet<u64>,
    /// Rule 3's `FlushDue` was posted and not yet answered by a commit or a flush's control: a
    /// flush that writes nothing here still answers it, so the next park posts again (D50).
    posted: bool,
    /// The thread this driver last told the hub it steps on (D39).
    stepper: Option<std::thread::ThreadId>,
    /// Core's `ChainStateFlushed` not yet serviced: the last flushed target heard of with no
    /// commit outstanding, or `T_ack` at the handoff (D37, D38). Rule 1's case A/B, unacked.
    flushed: Option<CommitTarget>,
    cadence_from: u32,
    /// The largest entry computed and discarded (`engine-memory`'s report).
    largest_discarded: u64,
    /// Told of each locator write once it has returned (the CRASH model's `Mark::Locator`).
    on_locator: Option<LocatorHook>,
}

type LocatorHook = Box<dyn Fn(LocatorKind, &BlockRef) + Send>;

pub(crate) fn store_err<E: KvEngine>(engine: &GuardedEngine<E>, e: &StoreError) -> SubscriberError {
    match engine.latch().check() {
        Err(f) => SubscriberError::Fatal(f),
        Ok(()) => SubscriberError::Data(e.to_string()),
    }
}

// Core: primitives/block.h CBlockLocator (`nVersion`, then `vHave`). This writes `vHave = [L]`;
// v24's `BaseIndex::Init` resolves a locator by walking every entry (`FindForkInGlobalIndex`,
// index/base.cpp:80-85, validation.cpp:182-197), and a one-entry one is L or genesis (D17).
fn encode_locator(x: &BlockRef) -> Vec<u8> {
    let mut v = Vec::with_capacity(37);
    v.extend_from_slice(&0i32.to_le_bytes());
    v.push(1);
    v.extend_from_slice(x.hash.as_bytes());
    v
}

/// The locator's block, as `encode_locator` wrote it.
pub fn decode_locator(v: &[u8]) -> Option<BlockHash> {
    let h: [u8; 32] = v.get(5..37)?.try_into().ok()?;
    (v.get(4) == Some(&1)).then(|| BlockHash::from(h))
}

pub(super) fn event_seq(e: &ChainEvent) -> u64 {
    match e {
        TipStep::Connected {
            seq,
            ..
        }
        | TipStep::Disconnected {
            seq,
            ..
        } => *seq,
    }
}

/// Core's refusal to start an index whose catch-up needs pruned blocks, after its name
/// (`index/base.cpp:120-122`).
pub const PRUNE_VIOLATION: &str = " best block of the index goes beyond pruned data. Please disable \
     the index or reindex (which will download the whole blockchain again)";

/// [`SubscriberDriver::open`]'s refusal on a thread that steps a registered subscriber (D39),
/// after its name.
pub const STEPPING_THREAD: &str =
    ": open on a thread that steps a subscriber; a flush or a hold may wait on it";

/// Core's prune check in `BaseIndex::Init` (`index/base.cpp:90-119`), for a subscriber behind V's
/// tip: the block data from the tip down to its durable L, or to their fork when L is off V's
/// chain, or with no locator to genesis (`GetFirstStoredBlock`; the catch-up starts there). An
/// index that reports no status (SPV's, the tests') is not checked.
fn prune_violation(
    snap: &dyn ChainSnapshot,
    idx: &dyn IndexAccess,
    durable: Option<BlockRef>,
) -> bool {
    // No chain yet: Core's `Init` runs after genesis is loaded.
    if snap.at_height(0).is_none() {
        return false;
    }
    let tip = snap.tip();
    if durable == Some(tip) {
        return false;
    }
    let have = |h: u32| {
        snap.at_height(h).is_some_and(|b| idx.status(b.id).is_none_or(|s| s.has(Status::HAVE_DATA)))
    };
    match durable {
        // `GetFirstStoredBlock(tip) != genesis`: every block below the tip has data.
        None => !(0..tip.height).all(have),
        Some(l) => {
            let test = if snap.at_height(l.height) == Some(l) {
                Some(l.height)
            } else {
                idx.lca(l.id, tip.id).and_then(|f| idx.height(f))
            };
            // Back from the tip while it has data, until the block to test (itself included).
            !test.is_some_and(|t| (t..=tip.height).all(have))
        }
    }
}

/// A driver gone by any path, a failure's included, leaves the hub, and its prune lock goes
/// with it (D33). The caller of a failed step trips the latch before it drops the driver, so no
/// step 7 runs without the lock.
impl<D: SubscriberData> Drop for SubscriberDriver<D> {
    fn drop(&mut self) {
        self.feed = None;
        self.hub.deregister(self.handle.id);
    }
}

impl<D: SubscriberData> SubscriberDriver<D> {
    /// §2.9 item 7: registers with the hub at the durable locator, reads L and the state once
    /// before the first write, blind-deletes `(L, L + D_range]`, sets the high-water floor, and
    /// starts Syncing (catch-up L → LCA(L, G) → G is [`sync_step`](Self::sync_step)'s).
    ///
    /// It waits out SB5's `HoldActive` and a flush's registration window (D33, D35), so it is
    /// refused on a thread that steps a registered subscriber of the hub (D39): a hold may wait
    /// on that one's queue, and a flush on its ack. The hub refuses it on W's thread, and on V's
    /// while the flush waits on V's clears (`WouldDeadlock`, D43).
    ///
    /// # Errors
    ///
    /// A store failure, an undecodable locator or state, Core's prune refusal
    /// ([`PRUNE_VIOLATION`]), the hub's refusal, or [`STEPPING_THREAD`].
    pub fn open(
        data: D,
        engine: Arc<GuardedEngine<D::E>>,
        hub: Arc<SubscriberHub>,
        blocks: Arc<dyn BlockSource>,
        cfg: DriverConfig,
    ) -> Result<Self, SubscriberError> {
        if hub.steps_on_this_thread() {
            return Err(SubscriberError::Data(format!("{}{STEPPING_THREAD}", data.name())));
        }
        let table = Arc::clone(hub.table().expect("a hub over a feed table"));
        let idx = Arc::clone(hub.index().expect("a hub over a feed table"));
        let schema = data.schema();
        engine.sync_wal().map_err(|e| store_err(&engine, &e))?;
        let (lcf, lkey) = schema.locator;
        let locator = engine.read(lcf, lkey).map_err(|e| store_err(&engine, e.store_error()))?;
        let durable = match locator {
            None => None,
            Some(v) => {
                let hash = decode_locator(&v).ok_or_else(|| {
                    SubscriberError::Data(format!("{}: bad locator", data.name()))
                })?;
                // A locator the index does not know is refused (D17): Dash v24's `Init` falls back
                // to `FindForkInGlobalIndex`'s genesis (index/base.cpp:83, validation.cpp:182-197),
                // leaving rows and state above it; R11a keeps L known.
                let id = idx.lookup(&hash).ok_or_else(|| {
                    SubscriberError::Data(format!(
                        "{}: best block of the index not found. Please rebuild the index.",
                        data.name()
                    ))
                })?;
                Some(BlockRef {
                    id,
                    hash,
                    height: idx.height(id).expect("an indexed block"),
                })
            }
        };
        let mut rows = Vec::with_capacity(schema.state_keys.len());
        for &(cf, key) in schema.state_keys {
            let v = engine.read(cf, key).map_err(|e| store_err(&engine, e.store_error()))?;
            rows.push((cf, key.to_vec(), v));
        }
        let at_l = DurableBelow {
            engine: &engine,
            max_height: durable.map_or(0, |l| l.height),
            schema,
        };
        let state = if durable.is_some() || rows.iter().any(|r| r.2.is_some()) {
            data.decode_state(&rows, &at_l)?
        } else {
            data.initial_state()
        };
        let state = data.with_file_floor(&state, data.file_len()?);
        let reg = Registration {
            name: data.name(),
            needs: data.needs(),
            mandatory: data.mandatory(),
            start_height: data.start_height(),
            durable,
        };
        let handle = hub
            .register(reg, engine.latch())?
            .map_err(|e| SubscriberError::Data(format!("{}: register: {e:?}", data.name())))?;
        // After the registration, whose prune lock keeps the data the check finds (D33).
        // NoPruneChk: S11c's control.
        if !planted!(engine.planted(), NoPruneChk)
            && prune_violation(&*table.snapshot(), &*idx, durable)
        {
            hub.deregister(handle.id);
            return Err(SubscriberError::Data(format!("{}{PRUNE_VIOLATION}", data.name())));
        }
        let mut me = Self {
            handle,
            data,
            engine,
            hub,
            table,
            idx,
            blocks,
            cfg,
            durable,
            durable_state: state.clone(),
            progress: durable,
            progress_state: state.clone(),
            journal: VecDeque::new(),
            journal_bytes: 0,
            latest: state,
            best: durable,
            class: SubscriberClass::Syncing,
            feed: None,
            waker: None,
            seq_synced: 0,
            last_seq: 0,
            skipped: BTreeSet::new(),
            posted: false,
            stepper: None,
            flushed: None,
            cadence_from: durable.map_or(0, |l| l.height),
            largest_discarded: 0,
            on_locator: None,
        };
        // Step 2: the blind rewind of (L, L + D_range]; with no locator, genesis's rows too.
        let lo = durable.map_or(0, |l| l.height);
        let mut ranges = me.data.height_ranges(lo, lo.saturating_add(me.cfg.delete_span()));
        if durable.is_none() {
            for &cf in me.data.schema().height_cfs {
                ranges.push((cf, height_key(0, &[]), height_key(1, &[])));
            }
        }
        if !ranges.is_empty() {
            let mut b = me.engine.batch();
            for (cf, start, end) in &ranges {
                b.delete_range(*cf, start, end);
            }
            me.write(b, Commit::Buffered)?;
        }
        // R11a at the open (D45): a mandatory L off the durable tip's path, as a backward flush
        // the subscriber was not registered for leaves it, is rewound durably to the fork before
        // the open returns. Core's `Init` only sets its in-memory best block to
        // `FindForkInGlobalIndex(locator)` (index/base.cpp:80-85); R11a needs it durable. An
        // optional one keeps Core's lazy rewind (its catch-up). The rewind reads `T_ack`, so the
        // node opens its subscribers after recovery's step 10 (`SubscriberHub::recovered`,
        // b4 m-D); with no durable tip there is none, and nothing to rewind to.
        if me.data.mandatory()
            && !planted!(me.planted(), NoOpenRwd)
            && let (Some(l), Some(g)) = (durable, me.hub.t_ack())
            && !me.ancestor_or_eq(l, g.tip)
        {
            let fork = me.lca(Some(l), g.tip).expect("two indexed blocks share genesis");
            me.progress = Some(fork);
            me.set_best(Some(fork));
            me.cadence_from = fork.height;
            me.rewind_commit()?;
        }
        Ok(me)
    }

    pub fn id(&self) -> SubscriberId {
        self.handle.id
    }

    pub fn class(&self) -> SubscriberClass {
        self.class
    }

    /// L.
    pub fn durable(&self) -> Option<BlockRef> {
        self.durable
    }

    /// p.
    pub fn progress(&self) -> Option<BlockRef> {
        self.progress
    }

    /// MPIX-11: never ahead of the rows a query can read (F9).
    pub fn best_block(&self) -> Option<BlockRef> {
        self.best
    }

    pub fn journal_bytes(&self) -> u64 {
        self.journal_bytes
    }

    pub fn journal_len(&self) -> usize {
        self.journal.len()
    }

    pub fn largest_discarded(&self) -> u64 {
        self.largest_discarded
    }

    pub fn data(&self) -> &D {
        &self.data
    }

    /// Tells `f` of each locator write once it has returned.
    pub fn set_on_locator(&mut self, f: impl Fn(LocatorKind, &BlockRef) + Send + 'static) {
        self.on_locator = Some(Box::new(f));
    }

    /// A Syncing subscriber's cadence timer firing (Core's `SYNC_LOCATOR_WRITE_INTERVAL`): its
    /// capped commit now.
    ///
    /// # Errors
    ///
    /// As [`sync_step`](Self::sync_step).
    pub fn tick(&mut self) -> Result<(), SubscriberError> {
        if self.class == SubscriberClass::Syncing {
            self.cadence_from = self.progress.map_or(0, |p| p.height);
            self.capped_commit(LocatorKind::Cadence)?;
        }
        Ok(())
    }

    /// The events thread's wake: set on the feed once live, and on the hub's watch list.
    pub fn set_waker(&mut self, w: Notify) {
        self.hub.watch(self.handle.id, w.clone());
        if let Some(f) = &mut self.feed {
            f.set_waker(w.clone());
        }
        self.waker = Some(w);
    }

    /// THE read path for every index lookup (O-R2-M1): keyed rows merge the journal, newest
    /// entry first, over the DB; the other classes are in the DB when `best_block()` reports
    /// their block.
    ///
    /// # Errors
    ///
    /// A store failure.
    pub fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError> {
        if self.data.schema().class(cf) == RowClass::Keyed {
            for e in self.journal.iter().rev() {
                if let Some(r) = e.keyed.iter().rev().find(|r| r.cf() == cf && r.key() == key) {
                    return Ok(match r {
                        Row::Put(_, _, v) => Some(v.clone()),
                        Row::Delete(..) => None,
                    });
                }
            }
        }
        self.engine.read(cf, key).map_err(|e| store_err(&self.engine, e.store_error()))
    }

    /// [`read`](Self::read) over a prefix, in key order.
    ///
    /// # Errors
    ///
    /// A store failure.
    pub fn iter_prefix(&self, cf: Cf, prefix: &[u8]) -> Result<Vec<KeyValue>, SubscriberError> {
        let mut rows: BTreeMap<Vec<u8>, Option<Vec<u8>>> = BTreeMap::new();
        for r in KvRead::iter_prefix(&*self.engine, cf, prefix) {
            let (k, v) = r.map_err(|e| store_err(&self.engine, &e))?;
            rows.insert(k, Some(v));
        }
        if self.data.schema().class(cf) == RowClass::Keyed {
            for e in &self.journal {
                for r in e.keyed.iter().filter(|r| r.cf() == cf && r.key().starts_with(prefix)) {
                    let v = match r {
                        Row::Put(_, _, v) => Some(v.clone()),
                        Row::Delete(..) => None,
                    };
                    rows.insert(r.key().to_vec(), v);
                }
            }
        }
        Ok(rows.into_iter().filter_map(|(k, v)| Some((k, v?))).collect())
    }

    // -------------------------------------------------------------------------------------
    // Syncing.

    /// One catch-up step (Core's `ThreadSync`): a rewind if the gate targets it, a disconnect off
    /// V's chain, the next block's connect with its capped cadence commit, or at V's tip F10's
    /// handoff with the final capped commit inside it.
    ///
    /// # Errors
    ///
    /// Any subscriber error; the caller trips the latch.
    pub fn sync_step(&mut self) -> Result<SyncProgress, SubscriberError> {
        debug_assert_eq!(self.class, SubscriberClass::Syncing);
        self.engine.latch().check()?;
        self.mark_stepper();
        if let Some(r) = self.hub.outstanding(self.handle.id).rewind {
            // Rule 4: a Syncing target rewinds at once.
            if self.l_strictly_above(&r.to) {
                let fork = self.lca(self.progress, r.to);
                if self.progress.map(|p| p.height) > fork.map(|f| f.height) {
                    self.disconnect_tip()?;
                    return Ok(SyncProgress::Progress);
                }
                self.rewind_if_due()?;
            }
            self.report_rewound(r);
            return Ok(SyncProgress::Progress);
        }
        let table = Arc::clone(&self.table);
        let snap = table.snapshot();
        let tip = snap.tip();
        if let Some(p) = self.progress {
            if snap.at_height(p.height) != Some(p) {
                // Core: index/base.cpp:172-176, rewind off a stale branch.
                self.disconnect_tip()?;
                return Ok(SyncProgress::Progress);
            }
            if p == tip {
                drop(snap);
                return self.handoff(p);
            }
        }
        let h = self.progress.map_or(0, |p| p.height + 1);
        let next = snap.at_height(h).expect("below V's tip");
        drop(snap);
        self.rewind_if_due()?;
        match self.connect_block(next)? {
            Connect::Done => {
                // A rewind off a stale branch may leave the progress below the last commit.
                if next.height.saturating_sub(self.cadence_from) >= self.cfg.cadence {
                    self.cadence_from = next.height;
                    self.capped_commit(LocatorKind::Cadence)?;
                }
                Ok(SyncProgress::Progress)
            }
            Connect::AtLimit => {
                let before = self.durable;
                self.capped_commit(LocatorKind::Cadence)?;
                if self.durable == before {
                    self.hub.post_flush_due();
                    return Ok(SyncProgress::Blocked);
                }
                Ok(SyncProgress::Progress)
            }
        }
    }

    /// F10: at V's tip p, the final capped commit and the cut, atomically under the tip lock.
    fn handoff(&mut self, p: BlockRef) -> Result<SyncProgress, SubscriberError> {
        let table = Arc::clone(&self.table);
        let id = self.handle.id;
        let r = table.subscribe_at(id, &p, &mut || self.capped_commit(LocatorKind::Cadence));
        match r {
            Ok(Handoff::Live {
                mut feed,
                seq_synced,
            }) => {
                if let Some(w) = &self.waker {
                    feed.set_waker(w.clone());
                }
                self.feed = Some(feed);
                self.class = SubscriberClass::Synced;
                self.seq_synced = seq_synced;
                self.last_seq = seq_synced - 1;
                self.skipped.clear();
                // Step 5's synced read may have missed this handoff (D37): `T_ack`, read after
                // the cut, is serviced as a missed `ChainStateFlushed`.
                self.flushed = self.hub.t_ack();
                Ok(SyncProgress::Live)
            }
            Ok(Handoff::Moved) => Ok(SyncProgress::Progress),
            Err(SubscribeError::HoldActive) => {
                // SB5 §4.3: `on_release`, then retry on the wake.
                if let Some(w) = &self.waker {
                    table.on_release(w.clone());
                }
                Ok(SyncProgress::Progress)
            }
            Err(SubscribeError::Finish(e)) => Err(e),
            Err(e) => Err(SubscriberError::Data(format!("subscribe_at: {e:?}"))),
        }
    }

    // -------------------------------------------------------------------------------------
    // Synced.

    /// One Synced step: rule 1's check of the hub's mirror, then one control or event.
    ///
    /// # Errors
    ///
    /// Any subscriber error, `FeedOrder` included; the caller trips the latch.
    pub fn step(&mut self) -> Result<SubStep, SubscriberError> {
        debug_assert_eq!(self.class, SubscriberClass::Synced);
        self.engine.latch().check()?;
        self.mark_stepper();
        let out = self.hub.outstanding(self.handle.id);
        if let Some(r) = out.rewind {
            // Rule 4: a Synced target drains its disconnects; the rewind commit lowers L.
            if !self.l_strictly_above(&r.to) {
                self.report_rewound(r);
                return Ok(SubStep::Progress);
            }
            if self.rewind_due() {
                self.rewind_commit()?;
                return Ok(SubStep::Progress);
            }
        }
        let toward = match (out.commit, self.flushed) {
            (Some(c), _) if self.case_b(c.tip_seq) => Some(Toward::Commit(c.new)),
            (Some(c), _) => {
                if self.case_a(c)? {
                    return Ok(SubStep::Progress);
                }
                None
            }
            (None, Some(t)) if self.case_b(t.tip_seq) => Some(Toward::Flushed(t.tip)),
            (None, Some(t)) => {
                self.flushed = None;
                self.on_flushed(t.tip)?;
                return Ok(SubStep::Progress);
            }
            (None, None) => None,
        };
        let feed = self.feed.as_mut().expect("Synced");
        let event = match feed.next() {
            Next::Control(m) => {
                self.on_control(m)?;
                return Ok(SubStep::Progress);
            }
            Next::Event(e) => *e,
            Next::Empty => {
                if let Some(w) = &self.waker {
                    feed.set_waker(w.clone());
                }
                return Ok(match feed.next() {
                    Next::Empty => SubStep::Idle,
                    Next::Control(m) => {
                        self.on_control(m)?;
                        SubStep::Progress
                    }
                    Next::Event(_) => SubStep::Progress,
                });
            }
        };
        match self.apply(event, toward)? {
            Applied::Done => {
                self.feed.as_mut().expect("Synced").pop();
                Ok(SubStep::Progress)
            }
            Applied::AtLimit(b) => self.at_limit(b),
        }
    }

    /// Rule 3 at the limit with no outstanding commit: compress, else park with `FlushDue`.
    fn at_limit(&mut self, b: BlockRef) -> Result<SubStep, SubscriberError> {
        let feed = self.feed.as_mut().expect("Synced");
        if let Some((connected, disconnected)) = feed.remove_round_trip(&b.hash) {
            self.last_seq = connected;
            self.skipped.insert(disconnected);
            return Ok(SubStep::Progress);
        }
        if let Some(w) = &self.waker {
            feed.set_waker(w.clone());
        }
        if !self.posted {
            self.posted = true;
            self.hub.post_flush_due();
        }
        Ok(SubStep::Parked)
    }

    /// A Synced subscriber that reaches its `D_max` bound posts `FlushDue` at once (ST3a's toy,
    /// `sub_connect`), so V's next call site flushes before a block meets the limit.
    fn due_at_dmax(&mut self, b: BlockRef) {
        let l_height = self.durable.map_or(-1, |l| i64::from(l.height));
        let at_bound =
            self.cfg.d_max.is_some_and(|d| i64::from(b.height) >= l_height + i64::from(d));
        if at_bound && !self.posted {
            self.posted = true;
            self.hub.post_flush_due();
        }
    }

    /// Rule 1's case test, by sequence number: the tip event is still ahead (queued, or pending
    /// emission).
    fn case_b(&self, tip_seq: u64) -> bool {
        tip_seq >= self.seq_synced && tip_seq > self.last_seq && !self.skipped.contains(&tip_seq)
    }

    /// Case A: commit at `X = LCA(p, new)` and ack, unless X is below L and a disconnect is
    /// queued (its rewind commit lowers L first). True if the commit was serviced.
    fn case_a(&mut self, c: FlushCommit) -> Result<bool, SubscriberError> {
        self.rewind_if_due()?;
        let x = self.lca(self.progress, c.new);
        let l_height = self.durable.map(|l| l.height);
        if x.map(|x| x.height) >= l_height {
            if let Some(x) = x {
                self.commit_at(x, LocatorKind::CommitStep)?;
            }
        } else if self.disconnect_queued()? {
            return Ok(false);
        }
        self.posted = false;
        if let Some(l) = self.durable {
            self.hub.ack(self.handle.id, c.flush, l);
        } else {
            self.hub.ack(self.handle.id, c.flush, c.new);
        }
        Ok(true)
    }

    /// The head of the data queue is a disconnect (controls first are handled on the way).
    fn disconnect_queued(&mut self) -> Result<bool, SubscriberError> {
        loop {
            let feed = self.feed.as_mut().expect("Synced");
            match feed.next() {
                Next::Control(m) => self.on_control(m)?,
                Next::Event(e) => return Ok(matches!(e, TipStep::Disconnected { .. })),
                Next::Empty => return Ok(false),
            }
        }
    }

    /// A control is a notification: rule 1 re-reads the mirror. `ChainStateFlushed` with no
    /// commit outstanding is Core's `BaseIndex::ChainStateFlushed` (`index/base.cpp:332-369`),
    /// which runs after the data Core queued before it (`validationinterface.cpp:250-255`): its
    /// target waits in `flushed` until the tip step at its `tip_seq` is consumed (D38).
    fn on_control(&mut self, m: ControlMsg) -> Result<(), SubscriberError> {
        if m.kind() != CHAIN_STATE_FLUSHED {
            return Ok(());
        }
        self.posted = false;
        let Ok(Flushed {
            new,
            tip_seq,
            ..
        }) = m.downcast::<Flushed>()
        else {
            return Ok(());
        };
        // Never back to an older target: the handoff's `T_ack` may be newer than a queued
        // control's (D37).
        if self.hub.outstanding(self.handle.id).commit.is_none()
            && self.flushed.is_none_or(|t| tip_seq >= t.tip_seq)
        {
            self.flushed = Some(CommitTarget {
                tip: new,
                tip_seq,
            });
        }
        Ok(())
    }

    /// The flush callback's commit, capped at the flushed tip (D33): at `new` when it is on p's
    /// path, else nothing, as Core's `ChainStateFlushed` skips a locator off the index's best
    /// chain with its warning (`index/base.cpp:357-363`, D46, b4 n-H). Nothing is acked.
    fn on_flushed(&mut self, new: BlockRef) -> Result<(), SubscriberError> {
        self.rewind_if_due()?;
        match self.progress {
            Some(p) if self.ancestor_or_eq(new, p) => {
                self.commit_at(new, LocatorKind::CommitStep)?
            }
            // Core: index/base.cpp:357-361
            Some(p) => tracing::info!(
                "ChainStateFlushed: WARNING: Locator contains block (hash={}) not on known best \
                 chain (tip={}); not writing index locator",
                new.hash,
                p.hash
            ),
            // No best block yet: nothing to commit, nothing to compare.
            None => {}
        }
        Ok(())
    }

    /// One feed event (§2.9.1, §4.4.1). With `toward` a commit's, connects off its leg are
    /// dropped; with either target, a connect at the limit checkpoints at p while p is on its
    /// leg, and with a flushed target's, one at the limit with p off it is dropped (D49).
    fn apply(&mut self, e: ChainEvent, toward: Option<Toward>) -> Result<Applied, SubscriberError> {
        let seq = event_seq(&e);
        match e {
            TipStep::Connected {
                block,
                ..
            } => {
                let parent = self.parent(block);
                if parent != self.progress {
                    if let Some(parent) = parent
                        && self.strict_ancestor_of_p(parent)
                    {
                        return Err(self.feed_order("a connect onto a strict ancestor", block));
                    }
                    self.last_seq = seq;
                    return Ok(Applied::Done);
                }
                if let Some(Toward::Commit(new)) = toward
                    && !self.ancestor_or_eq(block, new)
                {
                    // Case B: off the leg toward `new`.
                    self.last_seq = seq;
                    return Ok(Applied::Done);
                }
                self.rewind_if_due()?;
                let p_on_leg = |me: &Self, t: Toward| {
                    me.progress.is_some_and(|p| me.ancestor_or_eq(p, t.tip()))
                };
                match self.connect_block(block)? {
                    Connect::Done => self.due_at_dmax(block),
                    Connect::AtLimit
                        if toward.is_some_and(|t| p_on_leg(self, t))
                            && !planted!(self.planted(), T37) =>
                    {
                        // Rule 1's case B at the limit: a checkpoint at p, an ancestor of `new`,
                        // then the block, on the leg or (a flushed target's) off it (D49).
                        let p = self.progress.expect("a parent");
                        self.commit_at(p, LocatorKind::Checkpoint)?;
                        if let Connect::AtLimit = self.connect_block(block)? {
                            return Ok(Applied::AtLimit(block));
                        }
                    }
                    Connect::AtLimit
                        if let Some(t @ Toward::Flushed(_)) = toward
                            && !p_on_leg(self, t) =>
                    {
                        // D49: p off a flushed target's leg, at the limit: no checkpoint is in
                        // the cap, and parking could hold V's fanout behind a stale leg for
                        // good, so the stale block is dropped, as a commit's case B drops it.
                        self.last_seq = seq;
                        return Ok(Applied::Done);
                    }
                    Connect::AtLimit => return Ok(Applied::AtLimit(block)),
                }
                self.last_seq = seq;
                Ok(Applied::Done)
            }
            TipStep::Disconnected {
                block,
                ..
            } => {
                if Some(block) != self.progress {
                    if self.strict_ancestor_of_p(block) {
                        return Err(self.feed_order("a disconnect of a strict ancestor", block));
                    }
                    self.last_seq = seq;
                    return Ok(Applied::Done);
                }
                self.disconnect_tip()?;
                self.last_seq = seq;
                Ok(Applied::Done)
            }
        }
    }

    // -------------------------------------------------------------------------------------
    // Per-block processing.

    /// Connects `b` onto p (§2.9 items 1–2): its entry, admitted by bytes and `d_max`, then its
    /// height and inexact rows in one Buffered batch, then `best_block() = b`.
    fn connect_block(&mut self, b: BlockRef) -> Result<Connect, SubscriberError> {
        debug_assert!(!self.rewind_due(), "the rewind commit comes first");
        let needs = self.data.needs();
        let data = if needs.block {
            Some(self.blocks.block(&b)?)
        } else {
            None
        };
        let undo = if needs.undo {
            Some(self.blocks.undo(&b)?)
        } else {
            None
        };
        let cb = ConnectedBlock {
            block: b,
            data: data.as_deref(),
            undo: undo.as_deref(),
        };
        let (
            BlockRows {
                rows,
            },
            state,
        ) = self.data.connect(&cb, &self.progress_state)?;
        let schema = self.data.schema();
        let journaled = !schema.inexact_only();
        let (mut now, mut keyed, mut archive_src) = (Vec::new(), Vec::new(), Vec::new());
        for r in rows {
            match schema.class(r.cf()) {
                RowClass::Height => {
                    archive_src.push(r.clone());
                    now.push(r);
                }
                RowClass::Inexact => now.push(r),
                RowClass::Keyed => keyed.push(r),
                RowClass::Archive | RowClass::State => {
                    return Err(SubscriberError::Data(format!(
                        "{}: connect wrote an archive or state row",
                        self.data.name()
                    )));
                }
            }
        }
        if journaled {
            let bytes = keyed.iter().chain(&archive_src).map(Row::bytes).sum::<u64>()
                + ((keyed.capacity() + archive_src.capacity()) * size_of::<Row>()) as u64
                + self.data.state_bytes(&state);
            let l_height = self.durable.map_or(-1, |l| i64::from(l.height));
            let over_dmax =
                self.cfg.d_max.is_some_and(|d| i64::from(b.height) > l_height + i64::from(d));
            let over_bytes = self.journal_bytes + bytes > self.cfg.journal_budget;
            // An oversized entry goes alone into an empty journal (§2.9 item 2).
            if over_dmax || (over_bytes && !self.journal.is_empty()) {
                self.largest_discarded = self.largest_discarded.max(bytes);
                return Ok(Connect::AtLimit);
            }
            self.write_rows(&now)?;
            self.journal.push_back(Entry {
                block: b,
                keyed,
                archive_src,
                state: state.clone(),
                bytes,
            });
            self.journal_bytes += bytes;
        } else {
            self.write_rows(&now)?;
        }
        self.progress = Some(b);
        self.latest = state.clone();
        self.progress_state = state;
        self.set_best(Some(b));
        Ok(Connect::Done)
    }

    fn write_rows(&self, rows: &[Row]) -> Result<(), SubscriberError> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut batch = self.engine.batch();
        rows.iter().for_each(|r| r.put_into(&mut batch));
        self.write(batch, Commit::Buffered)
    }

    /// The disconnect of p (§2.9 item 4): above L, its entry is popped, its archive rows written
    /// after `sync_files`, and the capped eager commit follows; at or below L, the rewind commit
    /// is due. `best_block()` moves to `pprev` either way (MPIX-11).
    fn disconnect_tip(&mut self) -> Result<(), SubscriberError> {
        let p = self.progress.expect("a processed block");
        let parent = self.parent(p);
        self.set_best(parent);
        let above_l = self.durable.is_none_or(|l| p.height > l.height);
        self.progress = parent;
        // D51: the cadence point follows p down, as the open's rewind (D45) and `tick` reset it,
        // so a catch-up that re-climbs after a stale-branch rewind commits on cadence again.
        self.cadence_from = self.cadence_from.min(parent.map_or(0, |b| b.height));
        if !above_l {
            // Archives and state come from the rewind commit.
            return Ok(());
        }
        if let Some(e) = self.journal.pop_back() {
            debug_assert_eq!(e.block, p);
            self.journal_bytes -= e.bytes;
            let prev = self.journal.back().map_or(&self.durable_state, |e| &e.state);
            self.progress_state = self.data.with_high_water(prev, &self.latest);
            let archive = self.data.archive(&e.archive_src);
            if !archive.is_empty() {
                self.data.sync_files()?;
                let mut b = self.engine.batch();
                archive.iter().for_each(|r| r.put_into(&mut b));
                self.write(b, Commit::Buffered)?;
            }
        } else {
            self.progress_state = self.data.with_high_water(&self.durable_state, &self.latest);
        }
        // Rule 6 (§2.9 item 5b): Core's BlockDisconnected → Rewind → Commit
        // (index/base.cpp:308-330), capped.
        if let Some(cap) = self.hub.commit_cap()
            && let Some(x) = self.lca(parent, cap)
            && Some(x.height) > self.durable.map(|l| l.height)
        {
            self.commit_at(x, LocatorKind::EagerRewind)?;
        }
        Ok(())
    }

    // -------------------------------------------------------------------------------------
    // Commits.

    /// p is a strict ancestor of L: a disconnect went below the locator.
    fn rewind_due(&self) -> bool {
        match (self.durable, self.progress) {
            (Some(l), Some(p)) => p.height < l.height,
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn rewind_if_due(&mut self) -> Result<(), SubscriberError> {
        if self.rewind_due() {
            self.rewind_commit()?;
        }
        Ok(())
    }

    /// (a) A commit at X on p's path with h(L) ≤ h(X) (§2.9 item 5a), capped by the hub
    /// atomically with step 3b's read (R11a; D33); a no-op at L.
    fn commit_at(&mut self, x: BlockRef, kind: LocatorKind) -> Result<(), SubscriberError> {
        if self.at_or_below_l(x) {
            return Ok(());
        }
        let Some(x) = self.hub.begin_commit(self.handle.id, x) else {
            return Ok(());
        };
        if self.at_or_below_l(x) {
            self.hub.end_commit(self.handle.id);
            return Ok(());
        }
        // A failed write stays pending: it may have become durable, and the guarded write's
        // failure has tripped the latch, so no B1 follows (D29 (a)).
        self.write_commit(x, kind)
    }

    fn at_or_below_l(&self, x: BlockRef) -> bool {
        Some(x) == self.durable || Some(x.height) < self.durable.map(|l| l.height)
    }

    /// The commit at X itself: the journal's prefix to X, the state at X and the locator, in
    /// one Durable batch.
    fn write_commit(&mut self, x: BlockRef, kind: LocatorKind) -> Result<(), SubscriberError> {
        debug_assert!(!self.rewind_due());
        let n = self.journal.iter().position(|e| e.block == x).map(|i| i + 1);
        let state_x = match n {
            Some(n) => self.journal[n - 1].state.clone(),
            None if Some(x) == self.progress => self.progress_state.clone(),
            None => {
                return Err(SubscriberError::Data(format!(
                    "{}: a commit at {} off p's journal",
                    self.data.name(),
                    x.hash
                )));
            }
        };
        let checkpoint = kind == LocatorKind::Checkpoint;
        if self.data.schema().has_files {
            self.data.sync_files()?;
        }
        // T22: the locator without its barrier.
        if !planted!(self.planted(), T22) {
            self.engine.sync_wal().map_err(|e| store_err(&self.engine, &e))?;
        }
        let mut b = self.engine.batch();
        // T41: the checkpoint drops the staged keyed rows.
        if !(checkpoint && planted!(self.planted(), T41)) {
            for e in self.journal.iter().take(n.unwrap_or(0)) {
                e.keyed.iter().for_each(|r| r.put_into(&mut b));
            }
        }
        let state_x = self.data.with_high_water(&state_x, &self.latest);
        self.data.encode_state(&state_x).iter().for_each(|r| r.put_into(&mut b));
        let (lcf, lkey) = self.data.schema().locator;
        b.put(lcf, lkey, &encode_locator(&x));
        // T38: a Buffered checkpoint.
        let commit = if checkpoint && planted!(self.planted(), T38) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.write(b, commit)?;
        for _ in 0..n.unwrap_or(0) {
            let e = self.journal.pop_front().expect("counted");
            self.journal_bytes -= e.bytes;
        }
        self.durable = Some(x);
        self.durable_state = state_x;
        self.posted = false;
        self.hub.set_durable(self.handle.id, x);
        self.located(kind, x);
        Ok(())
    }

    /// (c) The rewind commit at F = p below L (§2.9 item 5c): `stage_disconnected` top-down over
    /// (F, L], their archives, then one Durable batch with the blind deletes, the keyed undo, the
    /// archives, the state at F and the locator F.
    fn rewind_commit(&mut self) -> Result<(), SubscriberError> {
        let l = self.durable.expect("a locator");
        let f = self.progress.expect("genesis is never disconnected");
        let schema = self.data.schema();
        let below = DurableBelow {
            engine: &self.engine,
            max_height: l.height,
            schema,
        };
        let (mut undo, mut archives) = (Vec::new(), Vec::new());
        let mut state = self.durable_state.clone();
        if !schema.inexact_only() {
            let needs = self.data.needs();
            for h in (f.height + 1..=l.height).rev() {
                let id = self.idx.ancestor(l.id, h).expect("L's path is indexed");
                let b = BlockRef {
                    id,
                    hash: self.idx.hash(id).expect("indexed"),
                    height: h,
                };
                let data = if needs.block {
                    Some(self.blocks.block(&b)?)
                } else {
                    None
                };
                let u = if needs.undo {
                    Some(self.blocks.undo(&b)?)
                } else {
                    None
                };
                let db = DisconnectedBlock {
                    block: b,
                    data: data.as_deref(),
                    undo: u.as_deref(),
                };
                let (rows, at_parent) = self.data.stage_disconnected(&db, &state, &below)?;
                undo.extend(rows);
                state = at_parent;
                let mut height_rows = Vec::new();
                for &cf in schema.height_cfs {
                    for (k, v) in below.height_rows(cf, h)? {
                        height_rows.push(Row::Put(cf, k, v));
                    }
                }
                archives.extend(self.data.archive(&height_rows));
            }
            self.data.sync_files()?;
        }
        self.engine.sync_wal().map_err(|e| store_err(&self.engine, &e))?;
        let mut b = self.engine.batch();
        let hi = l.height.saturating_add(self.cfg.delete_span());
        for (cf, start, end) in self.data.height_ranges(f.height, hi) {
            b.delete_range(cf, &start, &end);
        }
        undo.iter().chain(&archives).for_each(|r| r.put_into(&mut b));
        let state = self.data.with_high_water(&state, &self.latest);
        self.data.encode_state(&state).iter().for_each(|r| r.put_into(&mut b));
        let (lcf, lkey) = schema.locator;
        b.put(lcf, lkey, &encode_locator(&f));
        let commit = if planted!(self.planted(), T36) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        self.write(b, commit)?;
        self.journal.clear();
        self.journal_bytes = 0;
        self.durable = Some(f);
        self.durable_state = state.clone();
        self.progress_state = state;
        self.posted = false;
        self.hub.set_durable(self.handle.id, f);
        self.located(LocatorKind::Rewind, f);
        Ok(())
    }

    /// A capped commit at `LCA(p, commit_cap)` (Core's `Commit` from `ThreadSync`, capped; T33:
    /// uncapped, at p).
    fn capped_commit(&mut self, kind: LocatorKind) -> Result<(), SubscriberError> {
        self.rewind_if_due()?;
        if planted!(self.planted(), T33) {
            if let Some(p) = self.progress
                && !self.at_or_below_l(p)
            {
                self.write_commit(p, kind)?;
            }
            return Ok(());
        }
        if let Some(x) = self.hub.commit_cap().and_then(|cap| self.lca(self.progress, cap)) {
            self.commit_at(x, kind)?;
        }
        Ok(())
    }

    /// Shutdown (§4.2's order, step 3): the final capped commit, then deregistration.
    ///
    /// # Errors
    ///
    /// The commit's failure; the subscriber deregisters either way (on drop).
    pub fn stop(mut self) -> Result<(), SubscriberError> {
        self.finish()
    }

    /// [`stop`](Self::stop)'s final capped commit, with the registration (and its prune lock)
    /// kept until the driver drops: a disable records itself durably in between (D36).
    ///
    /// # Errors
    ///
    /// The commit's failure.
    pub fn finish(&mut self) -> Result<(), SubscriberError> {
        self.capped_commit(LocatorKind::Cadence)
    }

    fn mark_stepper(&mut self) {
        let me = std::thread::current().id();
        if self.stepper != Some(me) {
            self.stepper = Some(me);
            self.hub.stepping_here(self.handle.id);
        }
    }

    fn located(&self, kind: LocatorKind, at: BlockRef) {
        if let Some(f) = &self.on_locator {
            f(kind, &at);
        }
    }

    fn report_rewound(&self, r: RewindRequest) {
        if let Some(l) = self.durable
            && !self.l_strictly_above(&r.to)
        {
            self.hub.rewound(self.handle.id, r.flush, l);
        } else if self.durable.is_none() {
            self.hub.rewound(self.handle.id, r.flush, r.to);
        }
    }

    // -------------------------------------------------------------------------------------
    // Ancestry (F8: `IndexAccess` from subscriber threads).

    fn parent(&self, b: BlockRef) -> Option<BlockRef> {
        let h = b.height.checked_sub(1)?;
        let id = self.idx.ancestor(b.id, h)?;
        Some(BlockRef {
            id,
            hash: self.idx.hash(id)?,
            height: h,
        })
    }

    fn lca(&self, a: Option<BlockRef>, b: BlockRef) -> Option<BlockRef> {
        let a = a?;
        let id = self.idx.lca(a.id, b.id)?;
        Some(BlockRef {
            id,
            hash: self.idx.hash(id)?,
            height: self.idx.height(id)?,
        })
    }

    /// `a` is an ancestor of `b`, or `b`.
    fn ancestor_or_eq(&self, a: BlockRef, b: BlockRef) -> bool {
        a.height <= b.height && self.idx.ancestor(b.id, a.height) == Some(a.id)
    }

    fn strict_ancestor_of_p(&self, a: BlockRef) -> bool {
        self.progress.is_some_and(|p| a.height < p.height && self.ancestor_or_eq(a, p))
    }

    /// L strictly descends from `to` (the gate's target test).
    fn l_strictly_above(&self, to: &BlockRef) -> bool {
        self.durable.is_some_and(|l| l.height > to.height && self.ancestor_or_eq(*to, l))
    }

    fn feed_order(&self, what: &str, b: BlockRef) -> SubscriberError {
        SubscriberError::FeedOrder(format!(
            "{}: {what} ({} at {}), p = {:?}",
            self.data.name(),
            b.hash,
            b.height,
            self.progress.map(|p| p.hash)
        ))
    }

    fn set_best(&mut self, b: Option<BlockRef>) {
        self.best = b;
        self.handle.set_best_height(b.map_or(0, |b| b.height));
    }

    fn write(&self, b: <D::E as KvEngine>::Batch, commit: Commit) -> Result<(), SubscriberError> {
        self.engine.write(b, commit).map_err(|e| store_err(&self.engine, &e))
    }

    #[cfg(feature = "planted-bugs")]
    fn planted(&self) -> Option<crate::Planted> {
        self.engine.planted()
    }
}

impl<D: SubscriberData> std::fmt::Debug for SubscriberDriver<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriberDriver")
            .field("name", &self.data.name())
            .field("class", &self.class)
            .field("durable", &self.durable)
            .field("progress", &self.progress)
            .field("journal", &self.journal.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
