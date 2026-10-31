//! V's side of the flush (CH2 §4.2 "V's side"; E-b1.2, E-b1.4): the decision at Core's call
//! sites, the freeze after the tip step, the release, Core's `ChainStateFlushed`, and V's waits.
//!
//! V has at most one job in flight: a freeze, or an index-only write. Each wait is a loop over W's
//! events, registered with the latch (E-b1.7), that answers W's `PruneClears` (step 7); b3 adds
//! `FlushDue`.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

use dash_chain::{BlockIndexRecord, BlockRef, IndexWrite};
use dash_consensus::primitives::BlockHash;
use dashd_store::{ErrorKind, KvEngine};

use super::driver::{DriverEvent, FlushDriver};
use super::hub::SubscriberHub;
use super::machine::{FlushMachine, FlushReport, IndexOnlyWrite};
use super::observer::{BenchTimer, FlushObserver, LogCat, ObsMark, log};
use super::packet::{EffectLedger, FreezePacket, FrozenIndex, StateDelta};
use super::policy::{
    EVO_CRITICAL_BYTES, FlushAction, FlushCauses, FlushPolicy, FlushStateMode, PolicyCtx,
};
use super::prune::{PruneDryRun, PruneHost, TimedPrune};
use super::signals::FlushSignals;
use crate::coins::{CoinLayerOps, CoinsBoundary, CoinsCache, CoinsCacheSizeState, FrozenLayer};
use crate::planted::planted;
use crate::store::{Failure, Fatal, GlobalFailStop, GuardedEngine, GuardedSnapshot};

/// V's evo window (CH7a's; [`NoEvo`] until then).
pub trait EvoWindow<D: StateDelta> {
    /// Takes the window's delta at a freeze.
    fn take(&mut self) -> D;

    /// `EVO_CRITICAL`'s measure: the window's memory (Core's `m_evoDb.GetMemoryUsage()`).
    fn bytes(&self) -> u64;

    /// The evo state's best block, once CH7a tracks one: a freeze checks that it is `new`.
    fn best_block(&self) -> Option<BlockHash>;

    /// W has released the full flush in flight: B1, which wrote the taken window, is durable.
    fn release(&mut self);
}

/// No evo state: the window before CH7a.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvo;

impl EvoWindow<()> for NoEvo {
    fn take(&mut self) {}

    fn bytes(&self) -> u64 {
        0
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }

    fn release(&mut self) {}
}

/// V's block index, as the flush sees it: `HeaderChain` (CH1-node §7.2).
pub trait FlushIndex {
    /// Takes the dirty records (`HeaderChain::take_dirty_into`), handing each to `put` (hash,
    /// encoded record) in order: a freeze or an index-only write puts them straight into B1's
    /// batch (CH2c D40).
    fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8]));

    /// What [`take_dirty_into`](Self::take_dirty_into) would hand over now, at most: its records
    /// and their keys' and values' bytes ([`stored_records`] for a node's). B1's batch is sized
    /// from it (BL-511); `None`, unknown, gives B1 R.
    fn dirty_bound(&mut self) -> Option<(usize, usize)> {
        None
    }

    /// [`take_dirty_into`](Self::take_dirty_into)'s records, collected.
    fn take_dirty(&mut self) -> Vec<IndexWrite> {
        let mut records = Vec::new();
        self.take_dirty_into(&mut |hash, bytes| {
            records.push(IndexWrite {
                hash: *hash,
                bytes: bytes.to_vec(),
            })
        });
        records
    }

    /// Step 7's clears (CH2 §4.2; Core's `PruneOneBlockFile` on the index): every entry whose
    /// blocks are in `files` loses its data and undo, and every unlinked pair of an entry in
    /// `files` is erased, file 0 including the entries with no block data (Core's `nFile` 0); the
    /// cleared records, in id order, for Bk or Bp. The entries stay dirty for the next B1.
    ///
    /// The caller first checks the selection against V's working tip, and declines it instead if
    /// V's tip has fallen behind it ([`PruneRule::allows`], DEC-211): this is index-only.
    ///
    /// # Errors
    ///
    /// The message Core's `AbortNode` gets: V's index could not clear. The flush stops, and
    /// nothing is pruned.
    ///
    /// [`PruneRule::allows`]: super::PruneRule::allows
    fn prune_clear(&mut self, files: &[u32]) -> Result<Vec<IndexWrite>, String>;

    /// The tip height DEC-211's check of a `PruneClears` reads, given V's `working` tip's: the
    /// working height itself, unless the index knows a lower published one (CH1-node D46,
    /// `FlushHost::prune_check_height`). `None` declines the request.
    fn clears_check_height(&mut self, working: u32) -> Option<u32> {
        Some(working)
    }

    /// SB5's writer rule at V's waits for W (CH1-node D45, D47 item 2): V's pending fanout, so a
    /// subscriber W waits on at step 6 gets the events toward `new`.
    fn pump_fanout(&mut self) {}
}

/// Records with no block data: the tests' index, where a prune clears nothing.
impl FlushIndex for Vec<IndexWrite> {
    fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
        for w in std::mem::take(self) {
            put(&w.hash, &w.bytes);
        }
    }

    fn dirty_bound(&mut self) -> Option<(usize, usize)> {
        Some((self.len(), self.iter().map(|w| 32 + w.bytes.len()).sum()))
    }

    fn prune_clear(&mut self, _: &[u32]) -> Result<Vec<IndexWrite>, String> {
        Ok(Vec::new())
    }
}

/// `n` stored block-index records at most, for [`FlushIndex::dirty_bound`]: each a 32-byte hash
/// and at most [`BlockIndexRecord::MAX_LEN`] bytes.
pub(crate) fn stored_records(n: usize) -> Option<(usize, usize)> {
    Some((n, n.saturating_mul(32 + BlockIndexRecord::MAX_LEN)))
}

/// RocksDB's `WriteBatch` header (`write_batch.cc`).
const BATCH_HEADER: usize = 12;
/// A batch record's bytes beyond its key and value, at most: RocksDB's type, column family id and
/// two length varints, or the memory engine's 8.
const RECORD_OVERHEAD: usize = 16;
/// A coin's key bytes beyond `dirty_bytes_of`'s 36: its txid and a `VARINT(vout)` of up to 5.
const COIN_KEY_EXTRA: usize = 1;

/// What B1 puts that no counter bounds at the freeze: `meta`'s rows and the block files' infos,
/// which W takes at its barrier. Room for some 1,100 infos where a flush's blocks touch a few;
/// more (an import's first flush has every file it read) grow the batch, and B1's guard leaves
/// the infos out.
const B1_SLACK: usize = 64 << 10;

/// What B1 can come to (CH2c D40, BL-511), from V's counters at the freeze, its infos within
/// [`B1_SLACK`]: the index records, `meta`'s rows, the evo state and, in a one-batch flush, F's
/// coins (`coins`: its dirty entries and their bytes, `dirty_bytes_of` each). An evo row's record
/// overhead is at most its counted bytes (a tombstone counts 9 at least, a put 32), so twice
/// them covers the rows. No term walks a set. `None` when the index cannot bound its records.
fn b1_bound(
    records: Option<(usize, usize)>,
    coins: Option<(usize, u64)>,
    evo_bytes: u64,
) -> Option<usize> {
    let (n, record_bytes) = records?;
    let (dirty, coin_bytes) = coins.unwrap_or_default();
    let per = |count: usize, overhead: usize| (count as u64).saturating_mul(overhead as u64);
    let u = [
        BATCH_HEADER as u64,
        per(n, RECORD_OVERHEAD),
        record_bytes as u64,
        per(dirty, RECORD_OVERHEAD + COIN_KEY_EXTRA),
        coin_bytes,
        evo_bytes.saturating_mul(2),
        B1_SLACK as u64,
    ]
    .into_iter()
    .fold(0u64, u64::saturating_add);
    Some(usize::try_from(u).unwrap_or(usize::MAX))
}

/// V's dirty block-index records, put into B1's batch (CH2c D40), whose capacity is the smaller of
/// R and [`b1_bound`]; B1's guard checks the bound in debug builds.
fn frozen_index<E: KvEngine>(
    coins: &CoinsCache<E>,
    index: &mut dyn FlushIndex,
    frozen: Option<&FrozenLayer>,
    evo_bytes: u64,
) -> FrozenIndex {
    let r = coins.batch_capacity();
    let f = frozen.map(|f| (f.coins().dirty_len(), f.coins().dirty_bytes()));
    let bound = b1_bound(index.dirty_bound(), f, evo_bytes);
    let capacity = bound.map_or(r, |u| u.min(r));
    FrozenIndex::take(coins.db().batch_with_capacity(capacity), index, bound)
}

/// What V lends its flush side at a call.
pub struct FlushCx<'a, E: KvEngine, D: StateDelta> {
    /// The call's time: the decision's timers, and `last_write` once the job is released.
    pub now: Instant,
    pub coins: &'a mut CoinsCache<E>,
    pub mempool_usage: u64,
    /// V's private tip now: at an IF_NEEDED site, the tip before the step.
    pub tip: BlockRef,
    /// The last `seq` V assigned: a freeze outside the IF_NEEDED sites takes it (E-b1.2).
    pub last_seq: u64,
    /// V's block index: a freeze's dirty records, and step 7's clears.
    pub index: &'a mut dyn FlushIndex,
    pub evo: &'a mut dyn EvoWindow<D>,
    pub prune: &'a mut dyn PruneHost,
    /// `pruneblockchain`'s height.
    pub manual_prune_height: Option<u32>,
    /// Core's `fReindex`.
    pub reindex: bool,
    pub is_ibd: bool,
    /// The node has left initial block download.
    pub left_ibd: bool,
    /// A test asks for a flush.
    pub scripted: bool,
}

/// What V lends its flush side at a call with no tip (CH1-node D47 item 6, b4 D55): before
/// genesis is connected, at genesis's ConnectTip and at the devnet step's AcceptBlock, Core's
/// `m_chain.Height() == -1`.
pub struct NoTipCx<'a, E: KvEngine, D: StateDelta> {
    pub now: Instant,
    pub coins: &'a mut CoinsCache<E>,
    pub mempool_usage: u64,
    /// V's block index: an index-only write's dirty records, and V's fanout at its waits.
    pub index: &'a mut dyn FlushIndex,
    pub evo: &'a mut dyn EvoWindow<D>,
    pub prune: &'a mut dyn PruneHost,
    /// Core's `fReindex`.
    pub reindex: bool,
}

/// The job in flight.
#[derive(Debug)]
struct InFlight {
    flush_id: u64,
    /// Core's `ChainStateFlushed` tip, handed over at the release (E-b1.4, DEC-166).
    signal: Option<BlockRef>,
}

/// V's flush side: the policy, the driver that runs W, and the job in flight. The latch and the
/// abort sink are the coins' engine's, W's.
pub struct FlushControl<R: FlushDriver> {
    policy: FlushPolicy,
    driver: R,
    signals: Arc<dyn FlushSignals>,
    ledger: Arc<EffectLedger>,
    /// V's log lines: the decision's.
    obs: Arc<dyn FlushObserver>,
    last_flush_id: u64,
    in_flight: Option<InFlight>,
    /// The engine's write epoch at the last release: a copy point's snapshot is at it.
    released_epoch: u64,
    /// The subscribers' hub, W's too: step 0's locks and `FlushDue` (CH2b b3).
    hub: Option<Arc<SubscriberHub>>,
    /// Inside [`prune_and_flush`](Self::prune_and_flush): its freeze unlinks before the release.
    unlink_first: bool,
}

/// A decision made at Core's call site, carried across the tip step (E-b1.2). It holds no freeze:
/// [`freeze_after_step`](Self::freeze_after_step) runs it.
#[must_use = "a decided freeze runs in freeze_after_step"]
#[derive(Debug)]
pub struct PendingFreeze {
    action: FlushAction,
    mode: FlushStateMode,
    /// V's tip at the site: `ChainStateFlushed`'s locator tip, Core's `m_chain` before `SetTip`
    /// (E-b1.4).
    locator_tip: BlockRef,
    decided_at: Instant,
}

impl PendingFreeze {
    pub fn action(&self) -> &FlushAction {
        &self.action
    }

    /// Runs the decision right after the tip step, under the same guard. A freeze takes `new =
    /// tip`, V's private tip after the step, and `tip_seq`, that step's `seq`: for a disconnect of
    /// d, `(parent(d), seq)`. Before W gets the packet V checks that `tip` is the coins' best block
    /// (and evo's, once tracked) and V's tip; a mismatch is `AbortNode`. A freeze that is waited
    /// for (ALWAYS, manual prune, every freeze under `-dbcrashratio`, D14, and one that leaves
    /// native Σ above B(t), CH2c D32) is waited for here.
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped, or W has crashed; the failure has been reported.
    pub fn freeze_after_step<E: KvEngine, R: FlushDriver>(
        self,
        ctl: &mut FlushControl<R>,
        cx: &mut FlushCx<'_, E, R::Delta>,
        tip: BlockRef,
        tip_seq: u64,
    ) -> Result<(), Fatal> {
        ctl.execute(self, cx, tip, tip_seq, false)
    }
}

impl<R: FlushDriver> FlushControl<R> {
    /// `ledger` is the one V's presumed effects are recorded in. `signals` must not block: V
    /// hands it the signal from inside its waits. `obs` takes V's log lines.
    pub fn new(
        driver: R,
        policy: FlushPolicy,
        signals: Arc<dyn FlushSignals>,
        ledger: Arc<EffectLedger>,
        obs: Arc<dyn FlushObserver>,
    ) -> Self {
        Self {
            policy,
            driver,
            signals,
            ledger,
            obs,
            last_flush_id: 0,
            in_flight: None,
            released_epoch: 0,
            hub: None,
            unlink_first: false,
        }
    }

    /// The hub W publishes to: step 0 reads its predicted locks, and V answers its `FlushDue`.
    pub fn set_hub(&mut self, hub: Arc<SubscriberHub>) {
        self.hub = Some(hub);
    }

    /// §4.4.1 rule 5 at V's service points and idle loop (b3 adapter item 10): [`poll`](Self::poll)
    /// (W's events, then the freeze of a cause kept for the job that was in flight, a `FlushDue`
    /// a site took then among them, D24), then a pending `FlushDue` with no job in flight is one
    /// NONE freeze at `(V's tip, the last seq)` with `DMAX`, empty if nothing changed.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn answer_due<E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        self.poll(cx)?;
        if self.in_flight.is_none() && self.hub.as_ref().is_some_and(|h| h.flush_due()) {
            self.flush(FlushStateMode::None, cx)?;
        }
        Ok(())
    }

    /// V's backpressure wait (§4.4.1 rule 5, F3): until `retry` reports the pending fanout
    /// emitted, W's events and a pending `FlushDue` are answered at each turn, and V parks between
    /// turns (`park` returns on a wake: a pop, a bell, or its timeout). A Synced subscriber parked
    /// at its limit frees its queue only after the flush its `FlushDue` asks for, so a wait that
    /// does not answer it never ends (X15).
    ///
    /// # Errors
    ///
    /// As [`answer_due`](Self::answer_due).
    pub fn backpressure_wait<E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
        mut retry: impl FnMut() -> bool,
        mut park: impl FnMut(),
    ) -> Result<(), Fatal> {
        while !retry() {
            if planted!(cx.coins.db().planted(), X15) {
                self.service(cx)?;
            } else {
                self.answer_due(cx)?;
            }
            if retry() {
                break;
            }
            park();
        }
        Ok(())
    }

    // Core: validation.cpp:3141-3148 PruneAndFlush
    /// `PruneAndFlush` (b4 D57): the job in flight's release (Core's flushes run one at a time),
    /// the automatic check armed, then a NONE flush whose freeze unlinks before its release, and
    /// that release: Core's `FlushStateToDisk` unlinks before it returns (`v:3069-3074`).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn prune_and_flush<E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        self.wait_release(cx)?;
        cx.prune.arm();
        self.unlink_first = true;
        let flushed = self.flush(FlushStateMode::None, cx);
        self.unlink_first = false;
        flushed?;
        self.stop(cx)
    }

    /// [`prune_and_flush`](Self::prune_and_flush) with no tip: the check armed, then cleared by
    /// the flush at `Height() == -1`, which selects nothing (D55).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn prune_and_flush_without_tip<E: KvEngine>(
        &mut self,
        cx: &mut NoTipCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        self.wait_release_without_tip(cx)?;
        cx.prune.arm();
        self.flush_without_tip(FlushStateMode::None, cx)?;
        self.stop_without_tip(cx)
    }

    /// V's orderly stop (b3 adapter item 7, D45): while a job is in flight, its release,
    /// answering `PruneClears` inline, so a prune W selected after `Shutdown` is applied, as Core's
    /// shutdown flush prunes synchronously.
    ///
    /// # Errors
    ///
    /// [`Fatal`] if the latch trips during the wait.
    pub fn stop<E: KvEngine>(&mut self, cx: &mut FlushCx<'_, E, R::Delta>) -> Result<(), Fatal> {
        self.wait_release(cx)?;
        cx.coins.db().latch().check()
    }

    /// V's `AbortNode` stop (b3 adapter item 8, D45): trips the store latch without a report of
    /// its own, since V's sink reports it; W keeps reporting its own failures. True if the latch
    /// had already tripped.
    pub fn trip<E: KvEngine>(&self, engine: &GuardedEngine<E>, message: &str) -> bool {
        self.trip_latch(engine.latch(), message)
    }

    /// [`trip`](Self::trip) on the engine's `latch`, for a caller that cannot reach the engine
    /// (the node's adapter while V unwinds, b4 D59).
    pub fn trip_latch(&self, latch: &GlobalFailStop, message: &str) -> bool {
        !latch.trip(Failure {
            kind: ErrorKind::Other,
            site: "chainstate flush",
            message: message.to_owned(),
        })
    }

    /// The driver: the seam steps W through it.
    pub fn driver(&mut self) -> &mut R {
        &mut self.driver
    }

    /// A job is in flight.
    pub fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    // Core: validation.cpp:2966-3047 Chainstate::FlushStateToDisk (the decision)
    /// The decision at Core's call site in `mode` (E-b1.2), with any wait it makes there for the
    /// job in flight (CACHE_CRITICAL, EVO_CRITICAL, ALWAYS, manual prune). It never freezes: the
    /// caller takes the tip step, then runs [`PendingFreeze::freeze_after_step`]. W's releases are
    /// handled first.
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped, or W has crashed; the failure has been reported.
    pub fn at_site<E: KvEngine>(
        &mut self,
        mode: FlushStateMode,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<PendingFreeze, Fatal> {
        loop {
            self.service(cx)?;
            match self.decide(mode, cx) {
                // The waiting causes are kept, so the decision after the release freezes.
                FlushAction::WaitThenFreeze {
                    ..
                } => self.wait_release(cx)?,
                action => {
                    return Ok(PendingFreeze {
                        action,
                        mode,
                        locator_tip: cx.tip,
                        decided_at: cx.now,
                    });
                }
            }
        }
    }

    /// A call site with no tip step (PERIODIC, NONE, ALWAYS, `Flush`): the decision and its
    /// freeze at once, at `(V's tip, the last seq)` (E-b1.2).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn flush<E: KvEngine>(
        &mut self,
        mode: FlushStateMode,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        let pending = self.at_site(mode, cx)?;
        let (tip, seq) = (cx.tip, cx.last_seq);
        self.execute(pending, cx, tip, seq, false)
    }

    // Core: rpc/blockchain.cpp:3165-3180 (the cursor under cs_main right after the flush)
    /// An RPC copy point (§2.6): an ALWAYS flush, then a snapshot of the database at V's tip, with
    /// no marker. W runs the engine's hook before it releases and is then idle, and V takes the
    /// snapshot when it sees the release, before it submits anything else, so the snapshot holds
    /// that flush and nothing after it, and pins none of the memtables Bk filled (E-b1.8).
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn copy_point<'e, E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
        engine: &'e GuardedEngine<E>,
    ) -> Result<GuardedSnapshot<'e, E>, Fatal> {
        let released = self.copy_point_flush(cx)?;
        assert!(std::ptr::eq(engine, cx.coins.db()), "the coins' engine");
        let snapshot = engine.snapshot();
        // W wrote nothing after the release, and V submitted nothing since.
        assert_eq!(snapshot.epoch(), released, "a copy point's snapshot is at its flush");
        Ok(snapshot)
    }

    /// [`copy_point`](Self::copy_point)'s flush, for a snapshot another thread takes before V
    /// submits anything else (`gettxoutsetinfo`'s scan): the epoch W released at, which that
    /// snapshot's must equal.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn copy_point_flush<E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<u64, Fatal> {
        let pending = self.at_site(FlushStateMode::Always, cx)?;
        let (tip, seq) = (cx.tip, cx.last_seq);
        self.execute(pending, cx, tip, seq, true)?;
        debug_assert!(self.in_flight.is_none(), "ALWAYS waits for its release");
        Ok(self.released_epoch)
    }

    /// V's poll at a block boundary and in its idle loop: W's releases, then, if a cause was kept
    /// for the job that was in flight, its freeze at once (§4.2 "Release").
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn poll<E: KvEngine>(&mut self, cx: &mut FlushCx<'_, E, R::Delta>) -> Result<(), Fatal> {
        self.service(cx)?;
        if self.in_flight.is_none() && !self.policy.pending().is_empty() {
            self.flush(FlushStateMode::None, cx)?;
        }
        Ok(())
    }

    fn decide<E: KvEngine>(
        &mut self,
        mode: FlushStateMode,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> FlushAction {
        let coins = &*cx.coins;
        // F alone counts (§2.5): an index-only write holds no coins.
        let check = coins.boundary(cx.mempool_usage, false);
        if check.state == CoinsBoundary::Critical {
            // Core: validation.cpp:2960-2961, at each check that finds CRITICAL, with the native Σ
            // and B it compared (CH2 §3.3, BL-90(b); its timing is D1's).
            let line =
                format!("Cache size ({}) exceeds total space ({})", check.sigma, check.total);
            log(&*self.obs, LogCat::Always, line);
        }
        let mut prune = TimedPrune {
            host: &mut *cx.prune,
            obs: &*self.obs,
            ran: Cell::default(),
        };
        let (hub, tip) = (self.hub.as_deref(), cx.tip);
        // T32: the pre-commit (actual) locks.
        let t32 = planted!(coins.db().planted(), T32);
        let locks = move || match hub {
            Some(h) if t32 => h.actual_locks(&tip),
            Some(h) => h.predicted_locks(&tip),
            None => Vec::new(),
        };
        let mut ctx = PolicyCtx {
            now: cx.now,
            boundary: check.state,
            evo_bytes: cx.evo.bytes(),
            in_flight: self.in_flight.is_some(),
            has_best_block: coins.best_block().is_some(),
            // §4.2's shortcut: nothing to write, V's tip is `meta.tip`, and no Synced locator
            // lags it (D27).
            clean_at_durable: coins.active().dirty_len() == 0
                && coins.best_block() == coins.durable()
                && coins.durable() == Some(cx.tip)
                && hub.is_none_or(|h| h.synced_at(&tip)),
            prune: &mut prune,
            chain_height: cx.tip.height,
            manual_prune_height: cx.manual_prune_height,
            reindex: cx.reindex,
            is_ibd: cx.is_ibd,
            left_ibd: cx.left_ibd,
            // Taken right before the freeze it causes (b3 adapter item 10).
            dmax_due: hub.is_some_and(SubscriberHub::take_flush_due),
            prune_locks: &locks,
            scripted: cx.scripted,
        };
        let action = self.policy.decide(mode, &mut ctx);
        // Core: validation.cpp:3019-3020 (`fFlushForPrune`); the mark is CH2 §4.2 step 0 item 5.
        if let Some((run, manual)) = prune.ran.take() {
            let causes = match &action {
                FlushAction::Freeze {
                    causes,
                    ..
                } => *causes,
                _ => FlushCauses::empty(),
            };
            self.obs.mark(ObsMark::PruneDecision {
                flush_for_prune: !run.files.is_empty(),
                files: run.files,
                met_target: run.met_target,
                manual,
                tip: cx.tip.hash,
                causes,
            });
        }
        action
    }

    fn execute<E: KvEngine>(
        &mut self,
        pending: PendingFreeze,
        cx: &mut FlushCx<'_, E, R::Delta>,
        tip: BlockRef,
        tip_seq: u64,
        copy_point: bool,
    ) -> Result<(), Fatal> {
        cx.coins.db().latch().check()?;
        let PendingFreeze {
            action,
            mode,
            locator_tip,
            decided_at,
        } = pending;
        match action {
            FlushAction::Nothing
            | FlushAction::Pending {
                ..
            } => {}
            FlushAction::IndexOnly {
                signal,
            } => {
                let write = IndexOnlyWrite {
                    flush_id: self.next_flush_id(),
                    index: frozen_index(cx.coins, &mut *cx.index, None, 0),
                    decided_at,
                    always: mode == FlushStateMode::Always,
                    copy_point,
                };
                self.submit(FlushMachine::index_only(write), signal.then_some(locator_tip));
                if mode == FlushStateMode::Always {
                    self.wait_release(cx)?;
                }
            }
            FlushAction::Freeze {
                causes,
                prune,
                wait_done,
            } => {
                self.check_pair(cx, tip, tip_seq)?;
                let old = cx.coins.durable();
                // Frozen first: R counts in native Σ once a flush is in flight, and B1's batch,
                // with the records, holds at most R from here.
                let coins = cx.coins.freeze(cx.mempool_usage);
                let delta = cx.evo.take();
                let index = frozen_index(cx.coins, &mut *cx.index, Some(&coins), delta.bytes());
                let packet = FreezePacket {
                    flush_id: self.next_flush_id(),
                    old,
                    coins,
                    delta,
                    index,
                    presumed_effects_seq: self.ledger.current(),
                    tip_seq,
                    causes,
                    mode,
                    prune,
                    decided_at,
                    copy_point,
                };
                let mut job = FlushMachine::new(packet);
                if self.unlink_first {
                    job = job.unlink_before_release();
                }
                // Core signals after every full flush with a best block (`full_flush_completed`).
                self.submit(job, Some(locator_tip));
                // Native Σ ≤ B(t) at every block boundary (DEC-99, DEC-407): a layer that doubled
                // with F in flight, frozen beside a new active at its floor, can still exceed
                // B(t). V then waits for this release too, as Core's critical flush is synchronous
                // (CH2c D32). The frozen evo index counts in the evo bytes until its release
                // (CH7 D18), so a critical evo window waits too: Core's evo-critical flush is
                // synchronous, and without the wait the next block's check would stall V on the
                // same bytes, then flush again (CH7 D35).
                if wait_done
                    || cx.coins.size_state(cx.mempool_usage) == CoinsCacheSizeState::Critical
                    || cx.evo.bytes() >= EVO_CRITICAL_BYTES
                {
                    self.wait_release(cx)?;
                }
            }
            FlushAction::WaitThenFreeze {
                ..
            } => unreachable!("at_site waits for the release"),
        }
        Ok(())
    }

    /// CH1-node §7.1's check (its D3; CH2 E-b1.2): the packet's `new` is the coins' best block,
    /// evo's once tracked, and V's tip. Writing a mislabelled packet would corrupt `meta.tip` or
    /// the marker. Core's flush writes the best block it reads, so it checks nothing more, and
    /// neither does V: not the step's `seq`, nor its distance from the site's tip (DEC-95/99/102).
    fn check_pair<E: KvEngine>(
        &self,
        cx: &FlushCx<'_, E, R::Delta>,
        tip: BlockRef,
        tip_seq: u64,
    ) -> Result<(), Fatal> {
        let best = cx.coins.best_block();
        let evo = cx.evo.best_block();
        if best == Some(tip) && evo.is_none_or(|h| h == tip.hash) && tip == cx.tip {
            return Ok(());
        }
        Err(abort(
            cx.coins.db(),
            format!(
                "a freeze at {} (seq {tip_seq}) that V's tip {} (seq {}), the coins' best block \
                 {best:?} or evo's {evo:?} does not match",
                tip.hash, cx.tip.hash, cx.last_seq
            ),
        ))
    }

    fn next_flush_id(&mut self) -> u64 {
        self.last_flush_id += 1;
        self.last_flush_id
    }

    fn submit(&mut self, job: FlushMachine<R::Delta>, signal: Option<BlockRef>) {
        assert!(self.in_flight.is_none(), "one job in flight");
        if let Some(hub) = &self.hub {
            hub.submitted_here();
        }
        self.in_flight = Some(InFlight {
            flush_id: job.flush_id(),
            signal,
        });
        self.driver.submit(job);
    }

    /// W's ready events, as V's waits take them (b3 adapter item 5): its `PruneClears` answered
    /// and its releases taken. It never freezes, so V may call it at any wait, in a hold too.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn service<E: KvEngine>(&mut self, cx: &mut FlushCx<'_, E, R::Delta>) -> Result<(), Fatal> {
        while let Some(event) = self.driver.poll() {
            self.handle(event, cx)?;
        }
        cx.coins.db().latch().check()
    }

    /// W's ready events with no tip at V (CH1-node D47 item 6): before genesis is connected,
    /// at genesis's ConnectTip and at the devnet step's AcceptBlock. Its releases are taken, and
    /// every `PruneClears` is declined, as Core's `FindFilesToPrune` returns early at
    /// `Height() == -1`. A job in flight there is an index-only write ([`flush_without_tip`]),
    /// which asks for no clears, or a freeze made at a tip since rewound to none: a guard.
    ///
    /// [`flush_without_tip`]: Self::flush_without_tip
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn service_without_tip<E: KvEngine>(
        &mut self,
        cx: &mut NoTipCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        while let Some(event) = self.driver.poll() {
            self.handle_without_tip(event, cx.coins, &mut *cx.evo)?;
        }
        cx.coins.db().latch().check()
    }

    // Core: validation.cpp:2969-3120 at `m_chain.Height() == -1`
    /// Core's `FlushStateToDisk(mode)` with no tip at V (b4 D55): the decision and its write at
    /// once. Core's prune half runs: an armed automatic check is cleared under the selector's
    /// timer, the selector returning before it selects (`FindFilesToPrune`'s
    /// `chain_tip_height < 0`). With no best block, a full flush or a periodic write writes the
    /// block files and the index alone (`v:3053-3074`), as an index-only write with no
    /// `ChainStateFlushed`; ALWAYS waits for its release. A subscriber's `FlushDue` is left for a
    /// tip, and there is no manual prune: `pruneblockchain` is served once init has connected
    /// genesis.
    ///
    /// # Errors
    ///
    /// As [`at_site`](Self::at_site).
    pub fn flush_without_tip<E: KvEngine>(
        &mut self,
        mode: FlushStateMode,
        cx: &mut NoTipCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        let action = loop {
            self.service_without_tip(cx)?;
            let mut prune = NoTipPrune {
                host: &mut *cx.prune,
                obs: &*self.obs,
            };
            let mut ctx = PolicyCtx {
                now: cx.now,
                boundary: cx.coins.boundary(cx.mempool_usage, false).state,
                evo_bytes: cx.evo.bytes(),
                in_flight: self.in_flight.is_some(),
                // A no-tip site's coins hold no best block but at genesis's ConnectTip, whose
                // IF_NEEDED has no cause to flush them (one block puts no cache under pressure).
                has_best_block: false,
                clean_at_durable: false,
                prune: &mut prune,
                chain_height: 0,
                manual_prune_height: None,
                reindex: cx.reindex,
                is_ibd: true,
                left_ibd: false,
                dmax_due: false,
                prune_locks: &Vec::new,
                scripted: false,
            };
            match self.policy.decide(mode, &mut ctx) {
                FlushAction::WaitThenFreeze {
                    ..
                } => self.wait_release_without_tip(cx)?,
                action => break action,
            }
        };
        match action {
            FlushAction::Nothing
            | FlushAction::Pending {
                ..
            } => Ok(()),
            FlushAction::IndexOnly {
                signal,
            } => {
                debug_assert!(!signal, "no ChainStateFlushed without a best block");
                cx.coins.db().latch().check()?;
                let write = IndexOnlyWrite {
                    flush_id: self.next_flush_id(),
                    index: frozen_index(cx.coins, &mut *cx.index, None, 0),
                    decided_at: cx.now,
                    always: mode == FlushStateMode::Always,
                    copy_point: false,
                };
                self.submit(FlushMachine::index_only(write), None);
                if mode == FlushStateMode::Always {
                    self.wait_release_without_tip(cx)?;
                }
                Ok(())
            }
            FlushAction::Freeze {
                ..
            }
            | FlushAction::WaitThenFreeze {
                ..
            } => unreachable!("no freeze without a best block"),
        }
    }

    /// [`stop`](Self::stop) with no tip at V: the release of the job in flight, its clears
    /// declined.
    ///
    /// # Errors
    ///
    /// As [`stop`](Self::stop).
    pub fn stop_without_tip<E: KvEngine>(
        &mut self,
        cx: &mut NoTipCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        self.wait_release_without_tip(cx)?;
        cx.coins.db().latch().check()
    }

    fn wait_release_without_tip<E: KvEngine>(
        &mut self,
        cx: &mut NoTipCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        while self.in_flight.is_some() {
            let event = self.driver.wait_pumped(&mut || cx.index.pump_fanout())?;
            self.handle_without_tip(event, cx.coins, &mut *cx.evo)?;
        }
        Ok(())
    }

    fn handle_without_tip<E: KvEngine>(
        &mut self,
        event: DriverEvent,
        coins: &mut CoinsCache<E>,
        evo: &mut dyn EvoWindow<R::Delta>,
    ) -> Result<(), Fatal> {
        match event {
            DriverEvent::Released(report) => self.released(report, coins, evo),
            DriverEvent::PruneClears(req) => {
                coins.db().latch().check()?;
                req.decline();
            }
            DriverEvent::Failed | DriverEvent::Crashed => return Err(fatal(coins.db().latch())),
        }
        Ok(())
    }

    /// Waits for the job in flight's release, handling W's events. Each turn runs V's fanout
    /// before the driver parks V (D54).
    fn wait_release<E: KvEngine>(
        &mut self,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        while self.in_flight.is_some() {
            let event = self.driver.wait_pumped(&mut || cx.index.pump_fanout())?;
            self.handle(event, cx)?;
        }
        Ok(())
    }

    /// W's release of the job in flight: the policy's last write, the coins' frozen generation,
    /// and Core's `ChainStateFlushed`.
    fn released<E: KvEngine>(
        &mut self,
        report: FlushReport,
        coins: &mut CoinsCache<E>,
        evo: &mut dyn EvoWindow<R::Delta>,
    ) {
        let job = self.in_flight.take().expect("a release of the job in flight");
        assert_eq!(job.flush_id, report.flush_id, "the release of another job");
        self.policy.written(report.decided_at);
        self.released_epoch = report.epoch;
        if let Some(frozen) = report.coins {
            coins.release(frozen.generation);
            evo.release();
        }
        // DEC-166: B1 has returned Durable, so the locator never leads the database.
        if let Some(tip) = job.signal {
            self.signals.chain_state_flushed(tip);
        }
    }

    fn handle<E: KvEngine>(
        &mut self,
        event: DriverEvent,
        cx: &mut FlushCx<'_, E, R::Delta>,
    ) -> Result<(), Fatal> {
        match event {
            DriverEvent::Released(report) => {
                self.released(report, cx.coins, &mut *cx.evo);
                Ok(())
            }
            // Step 7: index-only, so answered at every wait (CH1-node §7.1, H3). A request that
            // is not answered is dropped, and W's flush stops: after a latched failure W writes
            // nothing more, and V's failure is reported first.
            DriverEvent::PruneClears(req) => {
                debug_assert_eq!(
                    self.in_flight.as_ref().map(|j| j.flush_id),
                    Some(req.flush_id),
                    "the clears of the job in flight"
                );
                cx.coins.db().latch().check()?;
                // DEC-211: at V's own tip now, the working one, which a rewind inside the flush
                // may have lowered, or the published one if it is lower (CH1-node D46).
                let height = cx.index.clears_check_height(cx.tip.height);
                if !height.is_some_and(|h| req.rule.allows(req.highest, h)) {
                    req.decline();
                    return Ok(());
                }
                match cx.index.prune_clear(&req.files) {
                    Ok(cleared) => {
                        req.reply(cleared);
                        Ok(())
                    }
                    Err(log) => Err(abort(cx.coins.db(), log)),
                }
            }
            // A failure, or the crash, has latched.
            DriverEvent::Failed | DriverEvent::Crashed => Err(fatal(cx.coins.db().latch())),
        }
    }
}

// Core: validation.cpp:2989-3017 and node/blockstorage.cpp:191-193 at `m_chain.Height() == -1`
/// The prune half with no tip: an armed automatic check is taken under the selector's timer,
/// and selects nothing. The policy then makes no dry run.
struct NoTipPrune<'a> {
    host: &'a mut dyn PruneHost,
    obs: &'a dyn FlushObserver,
}

impl PruneHost for NoTipPrune<'_> {
    fn check_due(&mut self) -> bool {
        if self.host.check_due() {
            BenchTimer::start(self.obs, "find files to prune".into()).done(self.obs);
        }
        false
    }

    fn arm(&mut self) {
        self.host.arm();
    }

    fn dry_run(&self, _: u32, _: u32, _: Option<u32>, _: bool) -> PruneDryRun {
        unreachable!("no dry run without a tip")
    }
}

/// [`FlushControl::prune_and_flush_without_tip`]'s prune half, for a caller with no flush to
/// run (CRASH's restart): the check armed, then taken under the selector's timer.
#[cfg(feature = "crash")]
pub(crate) fn arm_and_take_without_tip(host: &mut dyn PruneHost, obs: &dyn FlushObserver) {
    let mut prune = NoTipPrune {
        host,
        obs,
    };
    prune.arm();
    prune.check_due();
}

/// Core's `AbortNode` from V: trips the latch, so nothing more is written, and reports, unless
/// another failure latched first (its caller reports).
fn abort<E: KvEngine>(engine: &GuardedEngine<E>, log: String) -> Fatal {
    let latched = engine.latch().trip(Failure {
        kind: ErrorKind::Other,
        site: "chainstate flush",
        message: log.clone(),
    });
    if latched {
        engine.abort_sink().abort_node(&log, None);
    }
    fatal(engine.latch())
}

fn fatal(latch: &GlobalFailStop) -> Fatal {
    Fatal(latch.tripped().expect("the latch has tripped").clone())
}

#[cfg(test)]
mod tests;
