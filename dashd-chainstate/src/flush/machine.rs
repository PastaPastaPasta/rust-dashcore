//! W's flush (CH2 §4.2): a step machine, one store-call group per [`FlushMachine::step`], so the
//! seam can stop between any two (ST3a §5.6 item 2) and a driver runs it on W's thread.
//!
//! b1's steps: begin and space (1), the files barrier (2), the presumed-effect barrier (3), B1 (4),
//! durable (5), the split flush's sub-batches (8), its pre-Bk barrier (9) and Bk (10), and the
//! release (11) with the engine's post-Bk hook. b2's: step 7's prune selection, its request that V
//! clear the selection's blocks and its apply, the second files barrier and the clears in Bk or
//! Bp (10), and step 12's unlink. b3 adds step 3b's rewind gate, step 6's wait for the
//! subscribers, and the prune locks' bound at step 7.

use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use dash_chain::{BlockRef, IndexWrite};
use dash_consensus::primitives::BlockHash;
use dashd_store::flatfile::{DirtyFiles, FlatFileError, FlatFs, MIN_DISK_SPACE};
use dashd_store::{Cf, Commit, ErrorKind, KvBatch, KvEngine, StoreError, ValueCodec};

use super::clears::{ClearsAnswer, ClearsWait, Polled, PruneClears};
use super::hub::{HubWait, SubscriberHub};
use super::observer::{BenchTimer, FlushObserver, LogCat, ObsMark, SecondsTimer, log};
use super::packet::{FreezePacket, FrozenIndex, StateDelta};
use super::prune::{PruneDecision, final_selection, prune_lock_bound};
use crate::coins::layer::{DirtyRef, LayerPos, ShardedLayer};
use crate::coins::{CoinLayerOps, CoinsTable, FrozenLayer, splitmix64};
use crate::planted::planted;
use crate::store::{
    CHAINSTATE_SCHEMA, Failure, Fatal, GlobalFailStop, GuardedEngine, GuardedFiles, Meta,
};
use crate::tuning::EngineTuning;

// Core: validation.cpp:3083-3088
/// The datadir check's bytes per coins cache entry: about 48 bytes a coin, written twice (log and
/// table), with a safety factor of 2.
pub const DISK_BYTES_PER_ENTRY: u64 = 48 * 2 * 2;

/// W's fixed inputs.
#[derive(Clone, Debug)]
pub struct FlushConfig {
    /// S: a coin sub-batch closes once it holds more than this (`tuning::sub_batch`, §2.5).
    pub sub_batch: u64,
    /// `blocks/`: the block index's space check. It must exist, as Core's `GetBlocksDirPath`
    /// creates it at startup.
    pub blocks_dir: PathBuf,
    /// The network's datadir: the coins' space check.
    pub data_dir: PathBuf,
    /// Regtest: an ALWAYS flush ends with `flush_memtables` and `sync_wal` before it is reported,
    /// so a copy of the running datadir is stable (A1 §2.9's live-copy rule).
    pub live_copy: bool,
}

/// Core's `fPeriodicWrite` without a coins flush, and a full flush with no best block: the files
/// barrier, then one Durable batch of the block index (§4.2's last row).
#[derive(Debug)]
pub struct IndexOnlyWrite {
    pub flush_id: u64,
    /// Every dirty block-index record at the decision, in B1's batch.
    pub index: FrozenIndex,
    pub decided_at: Instant,
    /// The ALWAYS shortcut (§4.2): the live-copy rule applies.
    pub always: bool,
    /// An RPC copy point's ALWAYS (§2.6): as [`FreezePacket::copy_point`].
    pub copy_point: bool,
}

// Core: txdb.cpp:159
/// What `-dbcrashratio`'s exit logs.
pub(crate) const CRASH_MESSAGE: &str = "Simulating a crash. Goodbye.";

/// `-dbcrashratio`'s roll (§4.6): after each partial coin sub-batch, Core's `_Exit(0)` with
/// probability 1/N.
#[derive(Clone, Debug)]
pub struct CrashRoll {
    ratio: u32,
    state: u64,
}

impl CrashRoll {
    /// Seeded per process, as Core's `FastRandomContext` is.
    pub fn new(ratio: u32) -> Self {
        Self::seeded(ratio, RandomState::new().hash_one(ratio))
    }

    /// A fixed seed: the seam's (§4.2 concurrency rule 6).
    pub fn seeded(ratio: u32, seed: u64) -> Self {
        Self {
            ratio,
            state: seed,
        }
    }

    // Core: txdb.cpp:154-160
    /// Core's `rng.randrange(crash_simulate) == 0`.
    pub fn roll(&mut self) -> bool {
        self.ratio != 0 && splitmix64(&mut self.state).is_multiple_of(u64::from(self.ratio))
    }
}

/// What a step reaches: W's stores and V's hub, and W's own state across flushes.
pub struct FlushEnv<'a, E: KvEngine, F: FlatFs, T: EngineTuning> {
    pub engine: &'a GuardedEngine<E>,
    pub files: &'a GuardedFiles<F>,
    /// The filesystem under `files`: the space checks.
    pub fs: &'a F,
    pub hub: &'a SubscriberHub,
    pub obs: &'a dyn FlushObserver,
    pub tuning: &'a T,
    pub cfg: &'a FlushConfig,
    /// The presumed-effect seq that W's last pre-B1 barrier covered (§4.1). Recorded only once the
    /// barrier has returned.
    pub covered_seq: u64,
    pub crash: CrashRoll,
}

/// A step of §4.2's table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlushStep {
    /// Begin, and step 1.
    Space,
    Files,
    PresumedBarrier,
    B1,
    Durable,
    /// Step 7: the selection `D ∩ eligible`, and its request that V clear the files' blocks.
    PruneSelect,
    /// Step 7 once V has answered: the files are pruned, and the automatic check re-armed if the
    /// selection leaves a target D had met unmet (T40).
    PruneApply,
    SubBatch(u32),
    PreBkBarrier,
    /// Step 10's second files barrier (d2): the pruned files' reset infos.
    PruneBarrier,
    Bk,
    /// Step 10 of a one-batch flush that pruned: the clears, d2 and `meta.pruned_files`.
    Bp,
    /// The engine's post-Bk hook, and the live-copy barrier of an ALWAYS flush.
    AfterBk,
    Release,
    /// Step 12: the pruned files' unlink.
    Unlink,
}

/// What a step did.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// The step ran.
    Progress(FlushStep),
    /// Step 7 ran and asks V to clear the selection's blocks: the driver hands V the request.
    PruneClears(PruneClears),
    /// Step 7's apply found V's answer not yet come: nothing ran, and the step runs again once it
    /// has ([`FlushMachine::park`]).
    Waiting,
    /// Step 11: W no longer holds F. The driver sends V `FlushDone` with the report.
    Released(FlushReport),
    /// Nothing is left.
    Done,
    /// `-dbcrashratio`'s roll came up after a partial sub-batch: the driver's `_Exit(0)`, the
    /// seam's crash point. Nothing more may be written.
    Crash,
    /// A store call was refused: another failure had tripped the latch, and its caller reports
    /// it. The machine is done, as when the driver finds the latch tripped before a step.
    Refused,
}

/// What a flush reports to V at its release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlushReport {
    pub flush_id: u64,
    /// The decision's time: V's `FlushPolicy::written`.
    pub decided_at: Instant,
    /// A full flush's coins; `None` for an index-only write.
    pub coins: Option<CoinsWritten>,
    /// The engine's write epoch at the release: a copy point's snapshot must be at it (E-b1.8).
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoinsWritten {
    /// F's generation: V's `CoinsCache::release`.
    pub generation: u64,
    pub new: BlockRef,
    /// Written as B1 plus sub-batches and Bk under the marker, rather than in B1 alone.
    pub split: bool,
    /// The partial sub-batches written between B1 and Bk.
    pub sub_batches: u32,
    /// F's DIRTY entries: Core's "changed transaction outputs".
    pub changed: u64,
    /// F's entries.
    pub count: u64,
}

/// A flush that stopped (§4.6): what the driver hands Core's `AbortNode`. The latch has tripped,
/// so every later write in every store is refused and every waiter wakes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{log}")]
pub struct Abort {
    pub log: String,
    /// The message shown to the user; `None` is `AbortNode`'s default.
    pub user: Option<String>,
}

/// A step's failure, before the machine reports it.
struct Halt {
    abort: Abort,
    kind: ErrorKind,
    /// The store call that failed, if one did.
    op: Option<&'static str>,
    /// The guard refused the call, or it failed after another failure latched: not this flush's
    /// failure to report.
    refused: bool,
}

/// `HandleError`'s second line.
// Core: dbwrapper.cpp:267
const DEBUG_LEVELDB: &str = "You can use -debug=leveldb to get more complete diagnostic messages";

impl Halt {
    /// A failed store call. Unless the guard refused it (another failure's, already reported),
    /// it logs `HandleError`'s two lines at once, as Core's `CDBWrapper` does where the call
    /// fails, before any timer's line the failure unwinds through and the flush's `AbortNode`
    /// (BL-485). The first is the engine's text (CH2 D10), as the shutdown flush's failed WAL sync
    /// logs it.
    // Core: dbwrapper.cpp:261-268; validation.cpp:3121-3123
    fn system(obs: &dyn FlushObserver, op: &'static str, e: &StoreError) -> Self {
        let refused = e.kind() == Some(ErrorKind::ReadOnly);
        if !refused {
            log(obs, LogCat::Always, e.to_string());
            log(obs, LogCat::Always, DEBUG_LEVELDB.into());
        }
        Self {
            abort: Abort {
                log: format!("System error while flushing: {e}"),
                user: None,
            },
            kind: e.kind().unwrap_or(ErrorKind::Other),
            op: Some(op),
            refused,
        }
    }

    /// A block-file failure at `op`: its message starts with Core's `AbortNode` text (ST2).
    fn files_at(e: &FlatFileError, op: &'static str) -> Self {
        Self {
            abort: Abort {
                log: e.to_string(),
                user: None,
            },
            kind: e.kind(),
            op: Some(op),
            refused: matches!(e, FlatFileError::Failed),
        }
    }

    /// V dropped step 7's request unanswered: this flush's failure, or a refusal if one has
    /// latched already (V stopped for it).
    fn clears_dropped() -> Self {
        Self {
            abort: Abort {
                log: "The block index dropped the prune clears unanswered".into(),
                user: None,
            },
            kind: ErrorKind::Other,
            op: None,
            refused: false,
        }
    }
}

/// A full flush's state.
struct Full<D> {
    /// W's reference to F, dropped at the release.
    coins: Option<Arc<FrozenLayer>>,
    new: BlockRef,
    old: Option<BlockRef>,
    generation: u64,
    /// Dropped once B1 is written.
    delta: Option<D>,
    presumed_effects_seq: u64,
    tip_seq: u64,
    split: bool,
    /// Where the next sub-batch starts.
    cursor: LayerPos,
    sub_batches: u32,
    changed: u64,
    count: u64,
    /// Core's "write coins cache to disk" timer, from the first coin write to the last.
    timer: Option<BenchTimer>,
    /// The sub-batches' reference buffer between steps ([`refs_buffer`], CH2c D46).
    refs: Vec<usize>,
}

impl<D> Full<D> {
    fn layer(&self) -> &ShardedLayer {
        self.coins.as_deref().expect("F is held until the release").coins()
    }
}

/// Step 7's state (§4.2): the decision, then the selection and V's cleared records.
struct Prune {
    decision: PruneDecision,
    /// Core's `nLastBlockWeCanPrune` at the selection.
    bound: u32,
    /// The prune locks' `last_prune`, when it is below the chain's height.
    lock: Option<u32>,
    /// `D ∩ eligible(bound)`, in file order; empty until step 7, and after V's decline.
    selection: Vec<u32>,
    /// V's answer, awaited between step 7's request and its apply.
    wait: Option<ClearsWait>,
    /// V's cleared records, for Bk or Bp.
    cleared: Vec<IndexWrite>,
    /// The second files barrier's infos (d2), for Bk or Bp.
    d2: Option<DirtyFiles>,
}

/// W's flush of one freeze, or one index-only write.
pub struct FlushMachine<D: StateDelta> {
    flush_id: u64,
    decided_at: Instant,
    full: Option<Full<D>>,
    /// A freeze that prunes (`fFlushForPrune`: D ≠ ∅).
    prune: Option<Prune>,
    /// The dirty block-index records in B1's batch; taken by B1.
    index: Option<FrozenIndex>,
    always: bool,
    copy_point: bool,
    /// Step 12 runs before the release, as for `pruneblockchain`: `PruneAndFlush`'s caller
    /// returns once its files are gone (b4 D57).
    unlink_first: bool,
    /// The files barrier's infos, for B1.
    d1: Option<DirtyFiles>,
    /// Step 3b ran: the gate was opened, or had no target.
    gated: bool,
    /// A subscriber wait the next step runs behind (H7): step 3b's gate, or step 6's acks.
    awaiting: Option<Awaiting>,
    /// `None` once done, stopped or crashed.
    next: Option<FlushStep>,
}

/// W's waits on the subscribers (§4.2 steps 3b and 6).
enum Awaiting {
    Gate(HubWait),
    Acks(HubWait),
}

impl<D: StateDelta> FlushMachine<D> {
    pub fn new(packet: FreezePacket<D>) -> Self {
        let FreezePacket {
            flush_id,
            coins,
            old,
            delta,
            index,
            presumed_effects_seq,
            tip_seq,
            mode,
            prune,
            decided_at,
            copy_point,
            ..
        } = packet;
        let (changed, count) = (coins.coins().dirty_len() as u64, coins.coins().len() as u64);
        Self {
            flush_id,
            decided_at,
            full: Some(Full {
                new: coins.tip(),
                generation: coins.generation(),
                coins: Some(coins),
                old,
                delta: Some(delta),
                presumed_effects_seq,
                tip_seq,
                split: false,
                cursor: LayerPos::default(),
                sub_batches: 0,
                changed,
                count,
                timer: None,
                refs: Vec::new(),
            }),
            prune: prune.filter(|d| !d.files.is_empty()).map(|decision| Prune {
                decision,
                bound: 0,
                lock: None,
                selection: Vec::new(),
                wait: None,
                cleared: Vec::new(),
                d2: None,
            }),
            index: Some(index),
            always: mode == super::FlushStateMode::Always,
            copy_point,
            unlink_first: false,
            d1: None,
            gated: false,
            awaiting: None,
            next: Some(FlushStep::Space),
        }
    }

    pub fn index_only(w: IndexOnlyWrite) -> Self {
        Self {
            flush_id: w.flush_id,
            decided_at: w.decided_at,
            full: None,
            prune: None,
            index: Some(w.index),
            always: w.always,
            copy_point: w.copy_point,
            unlink_first: false,
            d1: None,
            gated: false,
            awaiting: None,
            next: Some(FlushStep::Space),
        }
    }

    pub fn flush_id(&self) -> u64 {
        self.flush_id
    }

    /// Runs step 12's unlink before the release (b4 D57).
    pub(crate) fn unlink_before_release(mut self) -> Self {
        self.unlink_first = true;
        self
    }

    /// The step the next [`step`](Self::step) runs; `None` once done, stopped or crashed.
    pub fn next_step(&self) -> Option<FlushStep> {
        self.next
    }

    /// Parks W until V answers step 7's request or drops it, or until a subscriber W waits on
    /// acks, rewinds or closes, registered with the latch: the driver's wait after
    /// [`Step::Waiting`].
    ///
    /// # Errors
    ///
    /// [`Fatal`] once the latch has tripped with no answer.
    pub fn park(&self, latch: &GlobalFailStop) -> Result<(), Fatal> {
        if let Some(Awaiting::Gate(w) | Awaiting::Acks(w)) = &self.awaiting {
            return w.park(latch);
        }
        match self.prune.as_ref().and_then(|p| p.wait.as_ref()) {
            Some(wait) => wait.park(latch),
            None => Ok(()),
        }
    }

    /// W waits on subscribers: step 3b's gate or step 6's acks (the seam's pump runs them).
    pub fn awaits_subscribers(&self) -> bool {
        self.awaiting.is_some()
    }

    /// Runs the next step.
    ///
    /// # Errors
    ///
    /// [`Abort`] when a store call fails or the disk is too full, and that failure tripped the
    /// latch (a refused call is [`Step::Refused`]); the machine is done.
    pub fn step<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &mut self,
        env: &mut FlushEnv<'_, E, F, T>,
    ) -> Result<Step, Abort> {
        let Some(at) = self.next.take() else {
            return Ok(Step::Done);
        };
        let ran = self.run(at, env);
        // D35: registrations wait from step 3b through the prune's apply.
        if matches!(ran, Ok(Step::Released(_) | Step::Refused | Step::Done) | Err(_)) {
            env.hub.end_flush(self.flush_id);
        }
        match ran {
            Ok(step) => Ok(step),
            Err(halt) if halt.refused => Ok(Step::Refused),
            Err(halt) => {
                // A store failure latched in the guard; a space check's latches here, unless
                // another failure came first, and then that one's caller reports.
                let latched = env.engine.latch().trip(Failure {
                    kind: halt.kind,
                    site: "chainstate flush",
                    message: halt.abort.log.clone(),
                });
                if !latched && halt.op.is_none() {
                    return Ok(Step::Refused);
                }
                if let Some(op) = halt.op {
                    env.obs.mark(ObsMark::StoreFailed {
                        op,
                    });
                }
                env.obs.mark(ObsMark::Abort {
                    message: halt.abort.log.clone(),
                });
                Err(halt.abort)
            }
        }
    }

    fn run<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &mut self,
        at: FlushStep,
        env: &mut FlushEnv<'_, E, F, T>,
    ) -> Result<Step, Halt> {
        let engine = env.engine;
        let obs = env.obs;
        let s = env.cfg.sub_batch;
        let capacity = batch_capacity(env.tuning, s);
        // H7: the step runs once every subscriber W waits on has answered, or is closing.
        let answered = match &self.awaiting {
            None => true,
            Some(Awaiting::Gate(_)) => env.hub.gate_done(self.flush_id),
            Some(Awaiting::Acks(_)) => env.hub.acks_done(self.flush_id),
        };
        if !answered {
            self.next = Some(at);
            return Ok(Step::Waiting);
        }
        self.awaiting = None;
        match at {
            FlushStep::Space => {
                if let Some(f) = &self.full {
                    obs.mark(ObsMark::FlushBegin {
                        flush: self.flush_id,
                        old: f.old.map(|b| b.hash),
                        new: f.new.hash,
                    });
                }
                check_space(env.fs, &env.cfg.blocks_dir, 0)?;
                if let Some(f) = &self.full {
                    check_space(env.fs, &env.cfg.data_dir, DISK_BYTES_PER_ENTRY * f.count)?;
                }
                // T13 and T19: step 7 before the files barrier and B1.
                let early = self.prune.is_some() && planted!(engine.planted(), T13 | T19);
                self.next = Some(if early {
                    FlushStep::PruneSelect
                } else {
                    FlushStep::Files
                });
            }
            FlushStep::Files => {
                let timer = BenchTimer::start(obs, "write block and undo data to disk".into());
                let d1 = match planted_barrier(engine, env.files, self.full.is_some()) {
                    Some(d1) => d1?,
                    None => files_barrier(env.files, "block files barrier")?,
                };
                timer.done(obs);
                self.d1 = Some(d1);
                let presumed = self.full.as_ref().map_or(0, |f| f.presumed_effects_seq);
                self.next =
                    Some(if presumed > env.covered_seq && !planted!(engine.planted(), T29) {
                        FlushStep::PresumedBarrier
                    } else {
                        FlushStep::B1
                    });
            }
            FlushStep::PresumedBarrier => {
                let seq = self.full.as_ref().expect("a full flush").presumed_effects_seq;
                engine.sync_wal().map_err(|e| Halt::system(obs, "sync_wal", &e))?;
                env.covered_seq = seq;
                self.next = Some(FlushStep::B1);
            }
            FlushStep::B1 => {
                if !self.gated
                    && let Some(f) = &self.full
                {
                    // Step 3b (§4.4.1 rule 4): a backward B1 waits until no mandatory locator
                    // strictly descends from `new`. X19: B1 without the gate.
                    self.gated = true;
                    if !planted!(engine.planted(), X19)
                        && !env.hub.open_rewind_gate(self.flush_id, f.new).is_empty()
                    {
                        self.awaiting = Some(Awaiting::Gate(env.hub.waiter()));
                        self.next = Some(at);
                        return Ok(Step::Waiting);
                    }
                    if planted!(engine.planted(), T10) {
                        // T10: step 5 before B1 has returned; W waits for the acks first.
                        if !env.hub.publish_durable(self.flush_id, f.new, f.tip_seq).is_empty() {
                            self.awaiting = Some(Awaiting::Acks(env.hub.waiter()));
                            self.next = Some(at);
                            return Ok(Step::Waiting);
                        }
                    }
                }
                self.b1(env)?;
            }
            FlushStep::Durable => {
                if let Some(f) = &self.full {
                    // Step 5 captures; step 6 waits for the captured set's acks (§4.4.1 rule 1).
                    // T10 published before B1.
                    if !planted!(engine.planted(), T10)
                        && !env.hub.publish_durable(self.flush_id, f.new, f.tip_seq).is_empty()
                    {
                        self.awaiting = Some(Awaiting::Acks(env.hub.waiter()));
                    }
                    if planted!(engine.planted(), X14) {
                        // X14: the next effect, written after the freeze, counted as covered.
                        env.covered_seq = env.covered_seq.max(f.presumed_effects_seq + 1);
                    }
                }
                obs.mark(ObsMark::Durable {
                    flush: self.flush_id,
                });
                let early = planted!(engine.planted(), T13 | T19);
                self.next = Some(if self.prune.is_some() && !early {
                    FlushStep::PruneSelect
                } else {
                    self.coins_next(env.cfg)
                });
            }
            FlushStep::PruneSelect => {
                let new = self.full.as_ref().expect("a full flush").new;
                let p = self.prune.as_mut().expect("a prune");
                let d = &p.decision;
                // The actual locks, once the captured set has committed (DEC-64).
                let (last_prune, _) = prune_lock_bound(d.tip_height, env.hub.actual_locks(&new));
                p.bound = d.rule().bound_at(d.tip_height, last_prune);
                p.lock = (last_prune < d.tip_height).then_some(last_prune);
                let bound = p.bound;
                // Never the selector again: a subset of D (T39).
                p.selection = rerun_selector(engine, env.files, d, d.tip_height)
                    .unwrap_or_else(|| env.files.read(|f| final_selection(f, &d.files, bound)));
                if p.selection.is_empty() {
                    self.apply_selection(env)?;
                    return Ok(Step::Progress(at));
                }
                // DEC-211: V clears only if its tip still allows every selected file.
                let highest = env.files.read(|f| {
                    p.selection.iter().filter_map(|&n| f.file_info(n)).map(|i| i.height_last).max()
                });
                let (req, wait) = PruneClears::new(
                    self.flush_id,
                    p.selection.clone(),
                    highest.unwrap_or(0),
                    d.rule(),
                );
                p.wait = Some(wait);
                self.next = Some(FlushStep::PruneApply);
                env.hub.awaits_v(self.flush_id, true);
                return Ok(Step::PruneClears(req));
            }
            FlushStep::PruneApply => {
                let p = self.prune.as_mut().expect("a prune");
                let polled = p.wait.as_ref().expect("step 7 asked V").poll();
                if !matches!(polled, Polled::Pending) {
                    env.hub.awaits_v(self.flush_id, false);
                }
                match polled {
                    Polled::Pending => {
                        self.next = Some(FlushStep::PruneApply);
                        return Ok(Step::Waiting);
                    }
                    Polled::Dropped => return Err(Halt::clears_dropped()),
                    Polled::Ready(ClearsAnswer::Cleared(cleared)) => p.cleared = cleared,
                    Polled::Ready(ClearsAnswer::Declined) if !planted!(engine.planted(), X33) => {
                        self.declined(env)?;
                        return Ok(Step::Progress(at));
                    }
                    // X33: the selection pruned with nothing cleared.
                    Polled::Ready(ClearsAnswer::Declined) => {}
                }
                self.apply_selection(env)?;
            }
            FlushStep::SubBatch(i) => {
                let f = self.full.as_mut().expect("a full flush");
                let mut b = engine.batch_with_capacity(capacity);
                let mut refs = std::mem::take(&mut f.refs);
                let (end, full) = put_dirty(
                    &mut b,
                    f.layer(),
                    f.cursor,
                    Some((s, &mut engine.batch())),
                    &mut refs,
                );
                f.refs = refs;
                if !full {
                    // The last chunk goes into Bk. T18 and T26 skip the barrier.
                    let barrier = f.sub_batches > 0 && !planted!(engine.planted(), T18 | T26);
                    self.next = Some(if barrier {
                        FlushStep::PreBkBarrier
                    } else {
                        self.before_bk()
                    });
                    return Ok(Step::Progress(at));
                }
                if f.timer.is_none() {
                    f.timer = Some(coins_timer(obs, f.count, f.layer().allocated_bytes()));
                }
                log_batch(obs, "partial", &b);
                engine.write(b, Commit::Buffered).map_err(|e| Halt::system(obs, "write", &e))?;
                f.cursor = end;
                f.sub_batches += 1;
                if env.crash.roll() {
                    log(obs, LogCat::Always, CRASH_MESSAGE.into());
                    return Ok(Step::Crash);
                }
                self.next = Some(FlushStep::SubBatch(i + 1));
            }
            FlushStep::PreBkBarrier => {
                engine.sync_wal().map_err(|e| Halt::system(obs, "sync_wal", &e))?;
                self.next = Some(self.before_bk());
            }
            FlushStep::PruneBarrier => {
                let d2 = files_barrier(env.files, "prune files barrier")?;
                self.prune.as_mut().expect("a prune").d2 = Some(d2);
                self.next = Some(match &self.full {
                    Some(f) if f.split => FlushStep::Bk,
                    _ => FlushStep::Bp,
                });
            }
            FlushStep::Bk => {
                let mut b = engine.batch_with_capacity(capacity);
                if self.pruned() {
                    self.put_clears(&mut b);
                }
                let f = self.full.as_mut().expect("a full flush");
                let mut refs = std::mem::take(&mut f.refs);
                put_dirty(&mut b, f.layer(), f.cursor, None, &mut refs);
                f.refs = refs;
                Meta::put_tip(&mut b, &f.new.hash);
                Meta::erase_head_blocks(&mut b);
                if planted!(engine.planted(), T3 | T4) {
                    // T3 and T4: the evo state in Bk.
                    f.delta.take().expect("kept for Bk").write_b1(&mut b);
                    Meta::put_evo_tip(&mut b, &f.new.hash);
                }
                if planted!(engine.planted(), T11 | T18 | T34) {
                    // T11, T18 and T34: Bk Buffered.
                    engine
                        .write(b, Commit::Buffered)
                        .map_err(|e| Halt::system(obs, "write", &e))?;
                } else {
                    let written = write_final(engine, obs, b, f);
                    if let Some(p) = &self.prune
                        && written.is_err()
                    {
                        unlink_after_failure(engine, env.files, obs, &p.selection);
                    }
                    written?;
                }
                if planted!(engine.planted(), T24) {
                    // T24: the evo state after Bk, as Core commits EvoDB after the coins.
                    let mut b = engine.batch_with_capacity(capacity);
                    f.delta.take().expect("kept past Bk").write_b1(&mut b);
                    Meta::put_evo_tip(&mut b, &f.new.hash);
                    engine
                        .write(b, Commit::Buffered)
                        .map_err(|e| Halt::system(obs, "write", &e))?;
                }
                self.next = Some(self.after_writes(env.cfg));
            }
            FlushStep::Bp => {
                let mut b = engine.batch();
                self.put_clears(&mut b);
                engine.write(b, Commit::Durable).map_err(|e| Halt::system(obs, "write", &e))?;
                self.next = Some(self.after_writes(env.cfg));
            }
            FlushStep::AfterBk => {
                if self.full.is_some() {
                    env.tuning.after_bk(engine).map_err(|e| Halt::system(obs, "after_bk", &e))?;
                }
                if self.always && env.cfg.live_copy {
                    engine
                        .flush_memtables()
                        .map_err(|e| Halt::system(obs, "flush_memtables", &e))?;
                    engine.sync_wal().map_err(|e| Halt::system(obs, "sync_wal", &e))?;
                }
                self.next = self.tail_after(env.cfg, at);
            }
            FlushStep::Release => {
                // Before `FlushDone`: V asserts it holds the last reference (§2.5).
                let coins = self.full.as_mut().map(|f| {
                    f.coins = None;
                    CoinsWritten {
                        generation: f.generation,
                        new: f.new,
                        split: f.split,
                        sub_batches: f.sub_batches,
                        changed: f.changed,
                        count: f.count,
                    }
                });
                let report = FlushReport {
                    flush_id: self.flush_id,
                    decided_at: self.decided_at,
                    coins,
                    epoch: engine.write_epoch(),
                };
                self.next = self.tail_after(env.cfg, at);
                return Ok(Step::Released(report));
            }
            FlushStep::Unlink => {
                // Core: validation.cpp:3069-3074, after the index write that cleared the files'
                // blocks: here Bk or Bp.
                unlink(env.files, obs, &self.prune.as_ref().expect("a prune").selection)?;
                self.next = self.tail_after(env.cfg, at);
            }
        }
        Ok(Step::Progress(at))
    }

    /// Step 7's apply (§4.2): prunes the selection's files and re-arms an automatic check that the
    /// selection leaves short of a target D had met (T40); then the selection's mark, and the
    /// coins' next step.
    fn apply_selection<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &mut self,
        env: &FlushEnv<'_, E, F, T>,
    ) -> Result<(), Halt> {
        let p = self.prune.as_ref().expect("a prune");
        let automatic = p.decision.manual.is_none();
        let (target_unmet, rearmed) = env
            .files
            .mutate(|f| {
                for &file in &p.selection {
                    f.prune_one_block_file(file);
                }
                let unmet = automatic && !f.prune_target_met(p.decision.target);
                // T40: no re-arm.
                let rearm = unmet && p.decision.met_target && !planted!(env.engine.planted(), T40);
                if rearm {
                    f.rearm_check_for_pruning();
                }
                Ok((unmet, rearm))
            })
            .map_err(|e| Halt::files_at(&e, "prune"))?;
        env.obs.mark(ObsMark::PruneSelected {
            flush: self.flush_id,
            files: p.selection.clone(),
            bound: p.bound,
            rearmed,
            target_unmet,
            lock: p.lock,
        });
        if !p.selection.is_empty() && planted!(env.engine.planted(), T5) {
            // T5: Core's unlink, before the coins; the tail's unlink then finds nothing.
            unlink(env.files, env.obs, &p.selection)?;
        }
        self.next = Some(self.after_selection(env));
        Ok(())
    }

    /// V declined step 7's selection (DEC-211): its tip has fallen behind it, as an
    /// `invalidateblock` or a reorg inside the flush can make it, where Core, selecting at its
    /// chain's tip, would not prune these files. This flush prunes nothing (no clears, no second
    /// files barrier, no Bp, no unlink), and the automatic check is armed again, so the next
    /// flush selects at V's tip.
    fn declined<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &mut self,
        env: &FlushEnv<'_, E, F, T>,
    ) -> Result<(), Halt> {
        let p = self.prune.as_mut().expect("a prune");
        env.files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .map_err(|e| Halt::files_at(&e, "prune"))?;
        env.obs.mark(ObsMark::PruneDeclined {
            flush: self.flush_id,
            files: std::mem::take(&mut p.selection),
        });
        self.next = Some(self.after_selection(env));
        Ok(())
    }

    /// The step after step 7: the coins' next, or the files barrier for T13 and T19, which
    /// select before it.
    fn after_selection<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &self,
        env: &FlushEnv<'_, E, F, T>,
    ) -> FlushStep {
        if planted!(env.engine.planted(), T13 | T19) {
            FlushStep::Files
        } else {
            self.coins_next(env.cfg)
        }
    }

    /// Step 7 selected files.
    fn pruned(&self) -> bool {
        self.prune.as_ref().is_some_and(|p| !p.selection.is_empty())
    }

    /// Step 10's prune half, in Bk or Bp: V's cleared records, d2's infos and `last_file`, and
    /// `meta.pruned_files`.
    fn put_clears(&self, b: &mut impl KvBatch) {
        let p = self.prune.as_ref().expect("a prune");
        put_records(b, &p.cleared);
        put_files(b, p.d2.as_ref().expect("the second files barrier ran"));
        Meta::put_pruned_files(b, true);
    }

    /// After step 5, or step 7: the split flush's sub-batches; else the second files barrier if
    /// step 7 selected files, then Bp; else the tail.
    fn coins_next(&self, cfg: &FlushConfig) -> FlushStep {
        match &self.full {
            Some(f) if f.split => FlushStep::SubBatch(0),
            _ if self.pruned() => FlushStep::PruneBarrier,
            _ => self.after_writes(cfg),
        }
    }

    /// The step before a split flush's Bk.
    fn before_bk(&self) -> FlushStep {
        if self.pruned() {
            FlushStep::PruneBarrier
        } else {
            FlushStep::Bk
        }
    }

    /// Step 4: the index, the files' infos and the evo state, and either all of F's coins at
    /// `meta.tip = new` (one-batch) or the marker (split).
    fn b1<E: KvEngine, F: FlatFs, T: EngineTuning>(
        &mut self,
        env: &mut FlushEnv<'_, E, F, T>,
    ) -> Result<(), Halt> {
        let (engine, obs, s) = (env.engine, env.obs, env.cfg.sub_batch);
        let capacity = batch_capacity(env.tuning, s);
        let timer = BenchTimer::start(obs, "write block index to disk".into());
        let mut d1 = self.d1.take().expect("the files barrier ran");
        if planted!(engine.planted(), T14) {
            // T14: B1 drops the dirty `block_files` records.
            d1.infos.clear();
        }
        // `copy` is V's records alone: T13 and T19 never combine with T3, T4, T24, T20 or T21,
        // since an engine plants one bug at most.
        let (mut b, mut bound, copy) =
            self.index.take().expect("B1 runs once").into_parts::<E::Batch>();
        let pruned = self.prune.as_ref().filter(|p| !p.selection.is_empty());
        let mut pruned_in_b1 = false;
        if let Some(p) = pruned {
            if planted!(engine.planted(), T13) {
                // T13: the clears and `meta.pruned_files` in B1.
                put_records(&mut b, &p.cleared);
                pruned_in_b1 = true;
                bound = None;
            } else if planted!(engine.planted(), T19) {
                // T19: B1 omits what the prune modified, for Bk or Bp to write.
                b = engine.batch_with_capacity(capacity);
                bound = None;
                let kept = copy.iter().filter(|w| !p.cleared.iter().any(|c| c.hash == w.hash));
                put_records(&mut b, kept);
                d1.infos.retain(|(file, _)| !p.selection.contains(file));
            }
        }
        // After the records V put at the freeze: the rest of Core's `WriteBatchSync`, and its bytes.
        let files = |b: &mut E::Batch| {
            let before = b.size_bytes();
            put_files(b, &d1);
            if pruned_in_b1 {
                Meta::put_pruned_files(b, true);
            }
            b.size_bytes() - before
        };
        let Some(f) = &mut self.full else {
            let infos = files(&mut b);
            assert_fits(&b, bound, infos);
            engine.write(b, Commit::Durable).map_err(|e| Halt::system(obs, "write", &e))?;
            timer.done(obs);
            self.next = Some(FlushStep::Durable);
            return Ok(());
        };
        let infos = files(&mut b);
        f.delta.as_ref().expect("B1 runs once").write_b1(&mut b);
        Meta::put_evo_tip(&mut b, &f.new.hash);
        // Core: txdb.cpp:152, one batch iff it never passes S, so no partial batch is written.
        // Here S is passed by B1's records plus the coins, not Core's marker plus the coins, so
        // a flush of many index records can split where Core's does not (ST3b-D1). Whether it
        // does is known before any coin is put: B1 holds the records once.
        let mut refs = std::mem::take(&mut f.refs);
        let taken = take_entries(
            f.layer(),
            LayerPos::default(),
            b.size_bytes(),
            Some((s, &mut engine.batch())),
            refs_buffer(&mut refs, f.layer()),
        );
        let split = taken.passed;
        // T3, T4 and T24 write a split flush's evo state later.
        let evo_later = split && planted!(engine.planted(), T3 | T4 | T24);
        if split {
            // Under the marker B1 holds no coins.
            f.refs = keep_refs(taken.refs);
            f.split = true;
            if evo_later {
                b = engine.batch_with_capacity(capacity);
                bound = None;
                put_records(&mut b, &copy);
                files(&mut b);
            }
            if !planted!(engine.planted(), T3) {
                // T20 and T21 name a block V has not connected.
                let new = unapplied(engine, &copy, f.new.height).unwrap_or(f.new.hash);
                if planted!(engine.planted(), T20) {
                    Meta::put_evo_tip(&mut b, &new);
                }
                Meta::put_head_blocks(&mut b, &new, f.old.as_ref().map(|o| &o.hash));
                Meta::erase_tip(&mut b);
            }
            let commit = if planted!(engine.planted(), T17) {
                Commit::Buffered
            } else {
                Commit::Durable
            };
            assert_fits(&b, bound, infos);
            evo_timed::<D, _>(obs, || {
                engine.write(b, commit).map_err(|e| Halt::system(obs, "write", &e))
            })?;
        } else {
            f.refs = keep_refs(put_taken(&mut b, f.layer(), taken));
            Meta::put_tip(&mut b, &f.new.hash);
            Meta::erase_head_blocks(&mut b);
            assert_fits(&b, bound, infos);
            evo_timed::<D, _>(obs, || write_final(engine, obs, b, f))?;
        }
        if !evo_later {
            f.delta = None;
        }
        if planted!(engine.planted(), T1) {
            // T1: the files barrier after B1.
            let d1 = files_barrier(env.files, "block files barrier")?;
            let mut b = engine.batch_with_capacity(capacity);
            put_files(&mut b, &d1);
            engine.write(b, Commit::Durable).map_err(|e| Halt::system(obs, "write", &e))?;
        }
        timer.done(obs);
        self.next = Some(FlushStep::Durable);
        Ok(())
    }

    /// Whether step 11 includes the engine's hook or the live-copy barrier.
    fn needs_after_bk(&self, cfg: &FlushConfig) -> bool {
        self.full.is_some() || (self.always && cfg.live_copy)
    }

    /// Whether they run before the release: V takes a copy point's snapshot once it sees the
    /// release, so the hook has run by then (E-b1.8), and an ALWAYS flush under the live-copy rule
    /// is reported only after its barrier.
    fn after_bk_first(&self, cfg: &FlushConfig) -> bool {
        self.needs_after_bk(cfg) && (self.copy_point || (self.always && cfg.live_copy))
    }

    /// The steps after the last durable write, in order: step 11's release, with the engine's
    /// hook after it, or before it for a copy point and a live-copy ALWAYS (E-b1.8); and step
    /// 12's unlink if step 7 selected files, after the release, or before it for
    /// `pruneblockchain` and `PruneAndFlush`, which return once V sees the release (§3.3: after
    /// step 12; b4 D57), and for a copy point and a live-copy ALWAYS, after whose release W does
    /// nothing more.
    fn tail(&self, cfg: &FlushConfig) -> impl Iterator<Item = FlushStep> {
        let manual = self.prune.as_ref().is_some_and(|p| p.decision.manual.is_some());
        let hook_first = self.after_bk_first(cfg);
        let unlink = self.pruned().then_some(FlushStep::Unlink);
        let (early, late) = if manual || self.unlink_first || hook_first {
            (unlink, None)
        } else {
            (None, unlink)
        };
        let (first, second) = if hook_first {
            (FlushStep::AfterBk, Some(FlushStep::Release))
        } else {
            (FlushStep::Release, self.needs_after_bk(cfg).then_some(FlushStep::AfterBk))
        };
        [early, Some(first), second, late].into_iter().flatten()
    }

    /// The step after the last durable write.
    fn after_writes(&self, cfg: &FlushConfig) -> FlushStep {
        self.tail(cfg).next().expect("the release")
    }

    /// The step after `done` in the [`tail`](Self::tail).
    fn tail_after(&self, cfg: &FlushConfig, done: FlushStep) -> Option<FlushStep> {
        let mut tail = self.tail(cfg);
        tail.find(|&s| s == done).expect("a step of the tail");
        tail.next()
    }
}

/// BL-511: B1 never outgrows what V sized it to (`bound`), its files' `infos` bytes aside. A
/// debug assertion: a release build checks nothing, and a short bound only regrows the batch.
fn assert_fits(b: &impl KvBatch, bound: Option<usize>, infos: usize) {
    debug_assert!(
        bound.is_none_or(|u| b.size_bytes() <= u.saturating_add(infos)),
        "B1 outgrew its bound"
    );
}

/// A files barrier, d1 or step 10's d2: syncs the block and undo files and takes their dirty
/// infos.
fn files_barrier<F: FlatFs>(files: &GuardedFiles<F>, op: &'static str) -> Result<DirtyFiles, Halt> {
    files.mutate(|files| files.sync_and_take_dirty_infos()).map_err(|e| Halt::files_at(&e, op))
}

/// Step 12: the pruned files' unlink, in a best-effort region. A failed removal or directory sync
/// is logged and left; only a refusal after an earlier failure stops here.
fn unlink<F: FlatFs>(
    files: &GuardedFiles<F>,
    obs: &dyn FlushObserver,
    selection: &[u32],
) -> Result<(), Halt> {
    let timer = BenchTimer::start(obs, "unlink pruned files".into());
    obs.mark(ObsMark::BestEffortBegin);
    let unlinked = files.mutate(|f| f.unlink_pruned_files(selection));
    obs.mark(ObsMark::BestEffortEnd);
    unlinked.map_err(|e| Halt::files_at(&e, "unlink"))?;
    timer.done(obs);
    Ok(())
}

/// T25: the unlink after a failed Bk. The failure tripped the latch, which refuses every later
/// unlink through the guard, so the bug's unlink goes past it.
#[cfg(feature = "planted-bugs")]
fn unlink_after_failure<E: KvEngine, F: FlatFs>(
    engine: &GuardedEngine<E>,
    files: &GuardedFiles<F>,
    obs: &dyn FlushObserver,
    selection: &[u32],
) {
    if engine.planted() == Some(crate::Planted::T25) {
        obs.mark(ObsMark::BestEffortBegin);
        let _ = files.unguarded(|f| f.unlink_pruned_files(selection));
        obs.mark(ObsMark::BestEffortEnd);
    }
}

#[cfg(not(feature = "planted-bugs"))]
fn unlink_after_failure<E: KvEngine, F: FlatFs>(
    _: &GuardedEngine<E>,
    _: &GuardedFiles<F>,
    _: &dyn FlushObserver,
    _: &[u32],
) {
}

/// T39: an automatic check's selector run again at step 7 against `last_prune`, with W's
/// inputs (no `nPruneAfterHeight`, not in IBD); `None` for the real selection.
#[cfg(feature = "planted-bugs")]
fn rerun_selector<E: KvEngine, F: FlatFs>(
    engine: &GuardedEngine<E>,
    files: &GuardedFiles<F>,
    d: &PruneDecision,
    last_prune: u32,
) -> Option<Vec<u32>> {
    if engine.planted() != Some(crate::Planted::T39) || d.manual.is_some() {
        return None;
    }
    let last_prune = i32::try_from(last_prune).unwrap_or(i32::MAX);
    let plan =
        files.read(|f| f.plan_files_to_prune(d.target, 0, Some(d.tip_height), last_prune, false));
    Some(plan.files)
}

#[cfg(not(feature = "planted-bugs"))]
fn rerun_selector<E: KvEngine, F: FlatFs>(
    _: &GuardedEngine<E>,
    _: &GuardedFiles<F>,
    _: &PruneDecision,
    _: u32,
) -> Option<Vec<u32>> {
    None
}

/// The files barrier as T1, T2, T6 and T9 plant it, or `None` for the real one.
#[cfg(feature = "planted-bugs")]
fn planted_barrier<E: KvEngine, F: FlatFs>(
    engine: &GuardedEngine<E>,
    files: &GuardedFiles<F>,
    full: bool,
) -> Option<Result<DirtyFiles, Halt>> {
    use crate::Planted::{T1, T2, T6, T9};
    use dashd_store::flatfile::BlockFiles;
    let every = |f: &BlockFiles<F>| DirtyFiles {
        infos: f.infos().iter().enumerate().map(|(n, i)| (n as u32, *i)).collect(),
        last_file: f.last_file(),
    };
    let r = match (engine.planted()?, full) {
        // T1 runs it after B1; T2 writes the index-only batch without it.
        (T1, true) | (T2, false) => Ok(DirtyFiles {
            infos: Vec::new(),
            last_file: files.read(BlockFiles::last_file),
        }),
        // T6: Core's `FlushBlockFile`, the current pair only; every file's infos.
        (T6, _) => files.mutate(|f| {
            f.flush_block_file(false, false)?;
            Ok(every(f))
        }),
        // T9: a failed barrier ignored, unlatched; every file's infos.
        (T9, _) => files.mutate(|f| Ok(f.sync_and_take_dirty_infos().unwrap_or_else(|_| every(f)))),
        _ => return None,
    };
    Some(r.map_err(|e| Halt::files_at(&e, "block files barrier")))
}

#[cfg(not(feature = "planted-bugs"))]
fn planted_barrier<E: KvEngine, F: FlatFs>(
    _: &GuardedEngine<E>,
    _: &GuardedFiles<F>,
    _: bool,
) -> Option<Result<DirtyFiles, Halt>> {
    None
}

/// T20 and T21: the highest block above `height` the dirty index holds with data, accepted but
/// not in F, for the marker (and with T20 the evo state) to name.
#[cfg(feature = "planted-bugs")]
fn unapplied<E: KvEngine>(
    engine: &GuardedEngine<E>,
    index: &[IndexWrite],
    height: u32,
) -> Option<BlockHash> {
    use crate::Planted::{T20, T21};
    use dash_chain::{BlockIndexRecord, Status};
    if !matches!(engine.planted(), Some(T20 | T21)) {
        return None;
    }
    index
        .iter()
        .filter_map(|w| Some((BlockIndexRecord::decode(&w.bytes).ok()?, w.hash)))
        .filter(|(r, _)| r.height > height && r.status.has(Status::HAVE_DATA))
        .max_by_key(|(r, _)| r.height)
        .map(|(_, hash)| hash)
}

#[cfg(not(feature = "planted-bugs"))]
fn unapplied<E: KvEngine>(_: &GuardedEngine<E>, _: &[IndexWrite], _: u32) -> Option<BlockHash> {
    None
}

// Core: validation.cpp:3050-3052, 3088-3090 CheckDiskSpace
fn check_space<F: FlatFs>(fs: &F, dir: &Path, additional: u64) -> Result<(), Halt> {
    let free = fs.available_space(dir).map_err(|e| Halt {
        abort: Abort {
            log: format!("System error while flushing: {}: {e}", dir.display()),
            user: None,
        },
        kind: ErrorKind::Io,
        op: None,
        refused: false,
    })?;
    if free >= MIN_DISK_SPACE.saturating_add(additional) {
        return Ok(());
    }
    Err(Halt {
        abort: Abort {
            log: "Disk space is too low!".into(),
            user: Some("Disk space is too low!".into()),
        },
        kind: ErrorKind::NoSpace,
        op: None,
        refused: false,
    })
}

/// The block index's column family: a record's key is its hash (Core's `WriteBatchSync`).
pub(super) fn block_index_cf() -> Cf {
    CHAINSTATE_SCHEMA.cf("block_index").expect("the chainstate schema has it")
}

/// Block-index records, in order.
fn put_records<'a>(b: &mut impl KvBatch, index: impl IntoIterator<Item = &'a IndexWrite>) {
    let cf = block_index_cf();
    for w in index {
        b.put(cf, w.hash.as_bytes(), &w.bytes);
    }
}

/// The files' infos and last file, after the records (Core's `WriteBatchSync`).
fn put_files(b: &mut impl KvBatch, d1: &DirtyFiles) {
    let block_files = CHAINSTATE_SCHEMA.cf("block_files").expect("the chainstate schema has it");
    let mut value = Vec::new();
    for (file, info) in &d1.infos {
        value.clear();
        info.encode_value(&mut value);
        b.put(block_files, &file.to_be_bytes(), &value);
    }
    Meta::put_last_file(b, d1.last_file);
}

// Core: txdb.cpp:136-150 (the loop of CCoinsViewDB::BatchWrite)
/// Puts the DIRTY entries from `at` into `b`, a tombstone as an erase, until `b` holds more than
/// the bound's bytes; the position after the last entry put, and whether the bound was passed.
///
/// The entries are put in key order (CH2c D37): RocksDB inserts a batch into the memtable's skip
/// list in batch order, and keys in order reuse the insert's splice, where the layer's hash order
/// made the shutdown flush spend most of W's time in the skip list. Which entries a batch holds,
/// and so where it ends, is as in layer order: with a bound, each entry is first put alone into
/// `probe` for its encoded size (batch sizes add up).
fn put_dirty<B: KvBatch>(
    b: &mut B,
    layer: &ShardedLayer,
    at: LayerPos,
    bound: Option<(u64, &mut B)>,
    kept: &mut Vec<usize>,
) -> (LayerPos, bool) {
    let taken = take_entries(layer, at, b.size_bytes(), bound, refs_buffer(kept, layer));
    let (end, passed) = (taken.end, taken.passed);
    *kept = keep_refs(put_taken(b, layer, taken));
    (end, passed)
}

/// The sub-batches' buffer of references into `layer`, from the one kept between steps (CH2c D46):
/// allocated once per flush at exactly the layer's DIRTY count, the most any batch takes, so no
/// sub-batch grows it. Only the pages the largest sub-batch writes become resident.
fn refs_buffer<'a>(kept: &mut Vec<usize>, layer: &'a ShardedLayer) -> Vec<DirtyRef<'a>> {
    let mut v = std::mem::take(kept);
    if v.capacity() < layer.dirty_len() {
        v = Vec::with_capacity(layer.dirty_len());
    }
    recycle(v)
}

/// Empties `refs` for [`refs_buffer`] to reuse.
fn keep_refs(refs: Vec<DirtyRef<'_>>) -> Vec<usize> {
    recycle(refs)
}

/// `v`'s allocation, emptied, as a `Vec<U>`: the standard library collects a mapped `IntoIter` in
/// place when the two types have one layout (pinned by `recycling_a_buffer_keeps_its_allocation`).
fn recycle<T, U>(mut v: Vec<T>) -> Vec<U> {
    const { assert!(size_of::<T>() == size_of::<U>() && align_of::<T>() == align_of::<U>()) };
    v.clear();
    v.into_iter().map(|_| -> U { unreachable!("emptied") }).collect()
}

/// The DIRTY entries a batch of `size` bytes takes from `at`, as [`put_dirty`] puts them.
struct Taken<'a> {
    refs: Vec<DirtyRef<'a>>,
    /// The position after the last entry taken.
    end: LayerPos,
    /// With a bound, the batch's size once they are put.
    size: Option<usize>,
    /// Whether the bound was passed.
    passed: bool,
}

/// [`put_dirty`]'s entries for a batch of `size` bytes, without putting them.
fn take_entries<'a, B: KvBatch>(
    layer: &'a ShardedLayer,
    at: LayerPos,
    mut size: usize,
    bound: Option<(u64, &mut B)>,
    mut refs: Vec<DirtyRef<'a>>,
) -> Taken<'a> {
    let table = CoinsTable::default();
    let (mut end, mut passed) = (at, false);
    debug_assert!(refs.is_empty());
    let mut bound = bound.map(|(s, probe)| {
        probe.clear();
        let empty = probe.size_bytes();
        (s, probe, empty)
    });
    for (next, r) in layer.iter_dirty_refs_from(at) {
        refs.push(r);
        end = next;
        if let Some((s, probe, empty)) = &mut bound {
            put_entry(&table, layer, &mut **probe, r);
            size += probe.size_bytes() - *empty;
            probe.clear();
            if size as u64 > *s {
                passed = true;
                break;
            }
        }
    }
    Taken {
        size: bound.is_some().then_some(size),
        refs,
        end,
        passed,
    }
}

/// Puts `taken`'s entries into `b` in key order (CH2c D37); returns their emptied buffer.
fn put_taken<'a, B: KvBatch>(
    b: &mut B,
    layer: &ShardedLayer,
    taken: Taken<'a>,
) -> Vec<DirtyRef<'a>> {
    let mut refs = taken.refs;
    let table = CoinsTable::default();
    refs.sort_unstable_by(|x, y| x.key().cmp(y.key()));
    for r in refs.drain(..) {
        put_entry(&table, layer, b, r);
    }
    debug_assert!(taken.size.is_none_or(|s| b.size_bytes() == s), "batch sizes add up");
    refs
}

/// One entry: a tombstone as an erase.
fn put_entry<B: KvBatch>(table: &CoinsTable, layer: &ShardedLayer, b: &mut B, r: DirtyRef<'_>) {
    match layer.dirty_entry(r) {
        (flags, _) if flags.is_spent() => table.erase(b, r.key()),
        (_, coin) => table.put(b, r.key(), coin),
    }
}

// Core: txdb.cpp:165-170
/// Writes the coins' final batch Durable, with Core's logs.
fn write_final<E: KvEngine, D>(
    engine: &GuardedEngine<E>,
    obs: &dyn FlushObserver,
    b: E::Batch,
    f: &mut Full<D>,
) -> Result<(), Halt> {
    let mem = f.layer().allocated_bytes();
    let timer = f.timer.take().unwrap_or_else(|| coins_timer(obs, f.count, mem));
    log_batch(obs, "final", &b);
    engine.write(b, Commit::Durable).map_err(|e| Halt::system(obs, "write", &e))?;
    log(
        obs,
        LogCat::CoinDb,
        format!(
            "Committed {} changed transaction outputs (out of {}) to coin database...",
            f.changed, f.count
        ),
    );
    timer.done(obs);
    Ok(())
}

// Core: validation.cpp:3164-3169 (LOG_TIME_SECONDS, unconditional); logging/timer.h:38-47
/// `write`, B1's durable write, which carries the evo rows, inside Core's "write evodb cache to
/// disk" pair when the window has evo state: before the coin batches, where Core prints it after
/// the coins flush (CH7 D20). The completed line is logged whether or not the write succeeds, as
/// Core's `Timer` logs it while a failed write's exception unwinds, before the flush's abort
/// (CH7 D42).
fn evo_timed<D: StateDelta, T>(obs: &dyn FlushObserver, write: impl FnOnce() -> T) -> T {
    let timer = D::EVO.then(|| SecondsTimer::start(obs, "write evodb cache to disk"));
    let written = write();
    if let Some(t) = timer {
        t.done(obs);
    }
    written
}

// Core: validation.cpp:3080-3081
fn coins_timer(obs: &dyn FlushObserver, count: u64, mem: u64) -> BenchTimer {
    // Core divides `size_t`s, then formats the integer with `%.2f`, which tinyformat prints as an
    // integer.
    let kb = mem / 1000;
    BenchTimer::start(obs, format!("write coins cache to disk ({count} coins, {kb}kB)"))
}

// Core: txdb.cpp:153, 170
/// "Writing partial batch of %.2f MiB" (`kind` "partial") or "Writing final batch …".
pub(crate) fn log_batch(obs: &dyn FlushObserver, kind: &str, b: &impl KvBatch) {
    let mib = b.size_bytes() as f64 / 1_048_576.0;
    log(obs, LogCat::CoinDb, format!("Writing {kind} batch of {mib:.2} MiB"));
}

/// A coin batch's capacity: R, the tuning's reserve for a batch of S (E-b1.8).
fn batch_capacity<T: EngineTuning>(tuning: &T, s: u64) -> usize {
    usize::try_from(tuning.batch_reserve(s)).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests;
