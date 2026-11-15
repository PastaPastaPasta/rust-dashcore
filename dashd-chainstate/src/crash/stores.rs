//! What both SUTs share: the stores and their adapters (W's side of an op), the front of
//! recovery, and the engine-level reads of the checks.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use dash_chain::BlockRef;
use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::{
    Abort, FatalCause, FaultyEngine, FaultyFlatFiles, InvariantId, Mark, Peek, Trace, TraceEntry,
    Violation, World,
};
use dashd_store::flatfile::{BlockFileInfo, BlockFilesConfig, FlatFileError, FlatFilePos};
use dashd_store::{Cf, DbSchema, KvBatch, KvRead, StoreError, ValueCodec as _};

use super::chain::{Arena, CORRUPT_READ, STATUS, VERDICT};
use super::reference::ChainRef;
use super::subs::{SUB_ABORT_PREFIX, SUB_STORE, Subs};
use super::synth::pow;
use super::world::DB_READ_FAILED;
use super::world::{WorldObserver, WorldSink, hash};
use super::{
    BLOCK_READ_FAILED, BLOCK_WRITE_FAILED, BLOCKS_DIR, CHAIN_STORE, ChainConfig, DISCONNECT_FAILED,
    INVALID_TIP, MAGIC, MAX_CRASH_AFTER, PATH_ACCEPT, PATH_CONNECT, PATH_DISCONNECT, PATH_FLUSH,
    PATH_LOAD, PATH_LOAD_INDEX, UNDO_WRITE_FAILED,
};
use crate::coins::{CoinsBudget, CoinsCache, DB_KEY_MAX, OutPointKey, Ratio, Salt};
use crate::flush::{
    CRASH_MESSAGE, CrashRoll, DriverEvent, EffectLedger, EvoWindow, FlushConfig, FlushControl,
    FlushDriver, FlushEnv, FlushMachine, FlushPolicy, FlushSignals, FlushStep, InlineDriver,
    StateDelta, SubscriberHub,
};
use crate::recovery::{ChainstateOpen, InitError, Opened, RecoveredChainstate, RecoveryConfig};
use crate::store::sites::BLOCK_FILES_READ_SITE;
use crate::store::{
    CHAINSTATE_SCHEMA, Failure, Fatal, GlobalFailStop, GuardedEngine, GuardedFiles, TIP,
};
use crate::tuning::MemoryTuning;

pub(super) type Db = GuardedEngine<FaultyEngine>;
pub(super) type Files = GuardedFiles<FaultyFlatFiles>;
pub(super) type Ctl<'a> = FlushControl<SignalDriver<'a>>;

const SIGNAL_MISMATCH: &str = "BL-807 ";

/// Returns this epoch's signal violations without adding store events.
/// Recovery's FA8 comes only from the post-open hook, so it never decides a DEC-196 refusal.
pub(super) fn signal_violations(trace: &Trace, epoch: u32) -> Vec<Violation> {
    trace
        .entries
        .iter()
        .filter_map(|entry| {
            if let TraceEntry::Mark {
                epoch: at_epoch,
                after_events,
                mark: Mark::Log {
                    line,
                },
            } = entry
            {
                if *at_epoch == epoch {
                    line.strip_prefix(SIGNAL_MISMATCH).map(|detail| {
                        Violation::new(InvariantId::FA8, detail).at(*at_epoch, *after_events)
                    })
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect()
}

struct SignalRequirement {
    required: bool,
    /// None for the clean shortcut. Its index-only write carries no BlockRef, so its signal is
    /// checked by hash against the durable best block.
    new: Option<BlockRef>,
    stored: Peek<Option<Vec<u8>>>,
    event: u64,
}

#[derive(Default)]
struct SignalState {
    released: Option<SignalRequirement>,
    leading: Vec<BlockRef>,
}

struct ReleaseSignals {
    state: Mutex<SignalState>,
    world: World,
}

impl ReleaseSignals {
    fn assert_signalled(&self) {
        if let Some(release) = self.state.lock().expect("signals").released.take()
            && release.required
        {
            self.mismatch(
                "release",
                release.event,
                "full flush released without ChainStateFlushed".into(),
            );
        }
    }

    fn mismatch(&self, at: &str, event: u64, detail: String) {
        self.world.mark(Mark::Log {
            line: format!("{SIGNAL_MISMATCH}{at} after event {event}: {detail}"),
        });
    }

    fn expected(&self, release: &SignalRequirement, tip: BlockRef) {
        if !release.required {
            self.mismatch(
                "release",
                release.event,
                "ordinary index-only write must not signal ChainStateFlushed".into(),
            );
        } else if let Some(new) = release.new {
            if tip != new {
                self.mismatch(
                    "release",
                    release.event,
                    format!(
                        "signalled {tip:?}, new {new:?}: ChainStateFlushed must name exactly new"
                    ),
                );
            }
        } else if release.stored != Peek::Known(Some(tip.hash.as_bytes().to_vec())) {
            self.mismatch("release", release.event, format!("signalled {tip:?}, expected {:?}: ChainStateFlushed must name the durable best block", release.stored));
        }
    }

    fn observe(&self, tip: BlockRef) {
        let mut state = self.state.lock().expect("signals");
        if let Some(release) = state.released.take() {
            self.expected(&release, tip);
        } else {
            self.mismatch(
                "signal",
                self.world.events(),
                format!("signalled {tip:?}: ChainStateFlushed before its flush's release"),
            );
            state.leading.push(tip);
        }
        // Core v24.0.0: validation.cpp:3105, :3123-3126
        let durable = self.world.durable_view().get(CHAIN_STORE, DbSchema::META, TIP);
        if durable != Peek::Known(Some(tip.hash.as_bytes().to_vec())) {
            self.mismatch("signal", self.world.events(), format!("signalled {tip:?}, durable {durable:?}: ChainStateFlushed must match durable meta.tip"));
        }
    }
}

impl FlushSignals for ReleaseSignals {
    fn chain_state_flushed(&self, tip: BlockRef) {
        #[cfg(test)]
        let Some(tip) = tests::signal(self, tip) else {
            return;
        };
        self.observe(tip);
    }
}

pub(super) struct SignalDriver<'a> {
    inner: InlineDriver<'a, FaultyEngine, FaultyFlatFiles, MemoryTuning, StubDelta>,
    signals: Arc<ReleaseSignals>,
    pending: Option<SignalRequirement>,
}

impl SignalDriver<'_> {
    fn released(&mut self, event: &DriverEvent) {
        if let DriverEvent::Released(_) = event {
            self.signals.assert_signalled();
            let mut release = self.pending.take().expect("a submitted machine");
            release.event = self.signals.world.events();
            let mut state = self.signals.state.lock().expect("signals");
            if state.leading.is_empty() {
                state.released = Some(release);
            } else {
                for tip in state.leading.drain(..) {
                    self.signals.expected(&release, tip);
                }
            }
            drop(state);
            #[cfg(test)]
            tests::released(&self.signals);
        }
    }

    pub(super) fn step(&mut self) -> Option<FlushStep> {
        self.inner.step()
    }
}

impl FlushDriver for SignalDriver<'_> {
    type Delta = StubDelta;

    fn submit(&mut self, job: FlushMachine<StubDelta>) {
        let (required, new) = job.signal_requirement();
        let stored = self.signals.world.durable_view().get(CHAIN_STORE, DbSchema::META, TIP);
        let required = required && (new.is_some() || stored != Peek::Known(None));
        self.pending = Some(SignalRequirement {
            required,
            new,
            stored,
            event: self.signals.world.events(),
        });
        #[cfg(test)]
        tests::submitted(&self.signals, new);
        self.inner.submit(job);
    }

    fn poll(&mut self) -> Option<DriverEvent> {
        let event = self.inner.poll()?;
        self.released(&event);
        Some(event)
    }

    fn wait(&mut self) -> Result<DriverEvent, Fatal> {
        let event = self.inner.wait()?;
        self.released(&event);
        Ok(event)
    }
}

// Core: init.cpp:2027 at v24.0.0
pub(super) const OPEN_FAILED: &str = "Error opening block database";
// Core: node/chainstate.cpp:183-185 (a false `LoadBlockIndex`; CH1-node's `LoadingBlockDb`)
pub(super) const LOAD_FAILED: &str = crate::chain::LOAD_BLOCK_DB_FAILED;
// Core: validation.cpp:3127-3128 at v24.0.0
pub(super) const FLUSH_FAILED: &str = "System error while flushing: ";
// Core: node/chainstate.cpp:252
pub(super) const UNABLE_TO_REPLAY: &str =
    "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.";
// Core: node/blockstorage.cpp:603, :584 at v24.0.0 (FlushBlockFile, FlushUndoFile; ST2's block-file
// texts)
const FLUSH_BLOCK_FILE: &str = "Flushing block file";
const FLUSH_UNDO_FILE: &str = "Flushing undo file";
// Core: node/blockstorage.cpp:718, :742 at v24.0.0 (FindBlockPos, FindUndoPos: `CheckDiskSpace`)
const DISK_SPACE_LOW: &str = "Disk space is too low!";

/// Core's message for the epoch's first fatal error `cause` (DEC-162 (4), DEC-165 (3)), at
/// v24.0.0-rc.3: the innermost Core path decides, except that a chainstate engine error anywhere
/// inside `LoadChainstate` throws out of it, recovery's replay included.
///
/// | path | first fatal error | Core's message |
/// |---|---|---|
/// | `LoadChainstate` | chainstate engine | "Error opening block database" (`dbwrapper.cpp:261`/`:268` throws, `init.cpp:2022`–`2029` catches) |
/// | `LoadChainstate` | block or undo read (the replay) | "Unable to replay blocks…" (`node/chainstate.cpp:252`) |
/// | `LoadBlockIndexDB` (in `LoadChainstate`) | blk-file open (the load's check, BL-127) | "Error loading block database" (`node/blockstorage.cpp:386-389`, `node/chainstate.cpp:183-185`) |
/// | `LoadChainstate` | none: the load's check finds a blk file missing, lost to a failed directory sync (DEC-212) | "Error loading block database", by [`sanctioned`]; recovery makes an unwarranted one unsanctioned |
/// | `FlushStateToDisk` | chainstate engine | "System error while flushing: …" (`validation.cpp:3127-3128` at v24.0.0) |
/// | `FlushStateToDisk` | block files | "Flushing block (undo) file to disk failed…" (`node/blockstorage.cpp`) |
/// | `AcceptBlock` | block write | "Failed to write block" (`node/blockstorage.cpp:869`); at a rollover, the old files' flush's "Flushing block (undo) file to disk failed…"; "Disk space is too low!" (`:715`) |
/// | `ConnectTip` | undo write | "Failed to write undo data" (`node/blockstorage.cpp:787` at v24.0.0); a lagging rev file's "Flushing undo file to disk failed…"; "Disk space is too low!" (`:742` at v24.0.0) |
/// | `AcceptBlock` | block-file read (V's link-count rule, D-c6-6) | "Failed to write block" |
/// | `ConnectTip` | block read | "Failed to read block" (`validation.cpp:3412`) |
/// | `ConnectTip` | block-file read (V's link-count rule, D-c6-6) | "Failed to write undo data" |
/// | `DisconnectTip` | undo read | "Failed to disconnect block…" (`validation.cpp:2300`, `:3282`, `:3621`) |
/// | `ConnectTip`, `DisconnectTip` | chainstate read | "Error reading from database, shutting down." (`init.cpp:2089`) |
/// | `ConnectTip` | chainstate write | "System error while flushing: …" (the model writes an effect at once: a failed evo snapshot write stops the node with one report, CH3b D221; EV3 D21: store faults are not consensus rejects) |
/// | any | subscriber store | "toyindex: …" (D29: the index's own failure; Core's index thread dies on `dbwrapper_error`, `dbwrapper.cpp:261-268`), or the path's chainstate messages when V's next write is refused first; V's stop names it as a chainstate write's, "System error while flushing: …", and reports nothing more (the harness's guard latches at `chainstate write`, CH3b D221; the node has no subscriber store) |
///
/// A failure only the system's `StoreFailed` reports (no store) allows any of its path's
/// messages. No fatal error (R0's refusals, which reach here with an empty cause) allows only
/// [`sanctioned`]'s refusals.
pub(super) fn sanctioned_after(cfg: &ChainConfig, message: &str, cause: &FatalCause) -> bool {
    // D29: the subscriber's store is under the node's latch. Its failure aborts with its own text
    // (Core's index thread dies on `dbwrapper_error`), or V's next write on the path is refused
    // first and V aborts as on a chainstate failure.
    let sub = cause.store.as_deref() == Some(SUB_STORE);
    if sub && message.starts_with(SUB_ABORT_PREFIX) {
        return true;
    }
    let (chainstate, fs, any) = match cause.store.as_deref() {
        Some(CHAIN_STORE | SUB_STORE) => (true, false, false),
        Some(_) => (false, cause.fs(), false),
        None => (true, true, true),
    };
    let read = cause.read;
    if cause.path() == Some(PATH_LOAD_INDEX) {
        return fs && read && message == LOAD_FAILED;
    }
    if cause.within(PATH_LOAD) {
        return (chainstate && message == OPEN_FAILED)
            || (fs && read && message == UNABLE_TO_REPLAY);
    }
    let flush_failed = message.starts_with(FLUSH_FAILED);
    let (flush_block, flush_undo) =
        (message.starts_with(FLUSH_BLOCK_FILE), message.starts_with(FLUSH_UNDO_FILE));
    match cause.path() {
        // No store and no path: no fatal error (R0's refusals). A `StoreFailed` of the SUT's own
        // with no event behind it, outside every path, would look the same; neither SUT reports
        // one. With only `crash_after` set, recovery never rolls, so accepting its exit there
        // costs nothing.
        None if any => sanctioned(cfg, message),
        None => false,
        Some(PATH_FLUSH) => (chainstate && flush_failed) || (fs && (flush_block || flush_undo)),
        // A read on a write path is V's link-count rule's (D-c6-6): Core's write fails there.
        Some(PATH_ACCEPT) => {
            let written = [BLOCK_WRITE_FAILED, DISK_SPACE_LOW].contains(&message);
            (fs && !read && (written || flush_block || flush_undo))
                || (fs && read && message == BLOCK_WRITE_FAILED)
        }
        Some(PATH_CONNECT) => {
            let written = [UNDO_WRITE_FAILED, DISK_SPACE_LOW].contains(&message);
            (fs && !read && (written || flush_undo))
                || (fs && read && [BLOCK_READ_FAILED, UNDO_WRITE_FAILED].contains(&message))
                || (chainstate && read && message == DB_READ_FAILED)
                || (chainstate && !read && flush_failed)
        }
        Some(PATH_DISCONNECT) => {
            (fs && read && message == DISCONNECT_FAILED)
                || (chainstate && read && message == DB_READ_FAILED)
        }
        Some(_) => false,
    }
}

/// The refusals with no fatal error before them: `-dbcrashratio`'s exit (`txdb.cpp:159` at
/// v24.0.0), only when it is configured, DEC-196's, which `ChainSut::open` turns into an
/// unsanctioned abort when E does not warrant it or recovery broke the stores, and the load's on a
/// missing blk file (BL-127), which recovery turns into one unless a failed directory sync lost the
/// file and E's records pass R1 (`blk_files_present`).
pub(super) fn sanctioned(cfg: &ChainConfig, message: &str) -> bool {
    ((cfg.crash_after.is_some() || cfg.crash_replay.is_some()) && message == CRASH_MESSAGE)
        || [INVALID_TIP, LOAD_FAILED].contains(&message)
}

/// Whether a block-file read failed with an I/O error: the bytes are unavailable, and the failed
/// call is fatal (DEC-148). A short or missing record is data that is not there.
pub(super) fn io_failure(e: &FlatFileError) -> bool {
    matches!(e, FlatFileError::Io { source, .. }
        if !matches!(source.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::NotFound))
}

/// The first op that refused: its number (from 1) and message.
pub(super) type Refusal = (usize, String);

/// FA7 (CH2 §6.4, rev2's O-R2-m3) on a forward run: from the first injected read fault, or the
/// connect that reads `K`'s corrupt coin, to the end, V gives no verdict (applied or not) and logs
/// no status change; and the op that met it (`met`) returned the fatal error, with the latch
/// tripped (`fatal`). The harness has no FA7, so these are R8's: the forward half of its
/// fail-stop.
pub(super) fn fa7(
    trace: &Trace,
    refused: Option<&Refusal>,
    met: Option<usize>,
    fatal: bool,
) -> Vec<Violation> {
    let fa7 = |what: String| Violation::new(InvariantId::R8, format!("FA7: {what}"));
    let mut from = None;
    let mut out = Vec::new();
    for e in &trace.entries {
        match (e, &from) {
            (
                TraceEntry::ReadFailed {
                    detail,
                    ..
                },
                None,
            ) => from = Some(detail.clone()),
            (
                TraceEntry::Mark {
                    mark: Mark::Log {
                        line,
                    },
                    ..
                },
                None,
            ) if line.starts_with(CORRUPT_READ) => from = Some(line.clone()),
            (
                TraceEntry::Mark {
                    mark,
                    ..
                },
                Some(at),
            ) => {
                let seen = match mark {
                    Mark::Connected {
                        block,
                        ..
                    } => Some(format!("connected {block:?}")),
                    Mark::Log {
                        line,
                    } if line.starts_with(VERDICT) || line.starts_with(STATUS) => {
                        Some(line.clone())
                    }
                    _ => None,
                };
                out.extend(seen.map(|seen| fa7(format!("`{seen}` after `{at}`"))));
            }
            _ => {}
        }
    }
    if let Some(at) = from
        && !(fatal && refused.is_some_and(|(n, _)| Some(*n) == met))
    {
        out.push(fa7(format!("the op that met `{at}` did not return the fatal error")));
    }
    out
}

/// A fault-free forward run's preconditions (§4.5) on how it ended, and V's active chain. With
/// `-dbcrashratio` in W the run ends at the roll, in the workload's last op (the chains are not
/// compared: the roll stops that op part way); otherwise every op returned, nothing stopped, and
/// V's active chain is the reference's.
pub(super) fn forward_preconditions(
    cfg: &ChainConfig,
    refused: Option<&Refusal>,
    active: &[BlockHash],
    refm: &ChainRef,
    trace: &Trace,
) -> Result<(), String> {
    if cfg.crash_after.is_some() {
        return match refused {
            Some((n, m)) if m == CRASH_MESSAGE && *n == refm.ops() => Ok(()),
            Some((n, m)) if m == CRASH_MESSAGE => Err(format!(
                "-dbcrashratio's roll came up in op {n} of {}: the ops after it never ran",
                refm.ops()
            )),
            r => Err(format!("-dbcrashratio's roll did not end the forward run: {r:?}")),
        };
    }
    if let Some((n, m)) = refused {
        return Err(format!("op {n} refused: {m}"));
    }
    if let Some(m) = trace.marks().find(|m| matches!(m, Mark::Abort { .. })) {
        return Err(format!("the forward run stopped: {m:?}"));
    }
    let want = refm.active();
    match active.iter().zip(want).position(|(a, b)| a != b) {
        None if active.len() == want.len() => Ok(()),
        at => Err(format!(
            "V's active chain ({} blocks) is not the reference's ({}): they part at height {}",
            active.len(),
            want.len(),
            at.unwrap_or(active.len().min(want.len()))
        )),
    }
}

/// Whether what an op ran inside W left V's own paths balanced: V's abort while W's flush is in
/// flight (`F d`) leaves its `AcceptBlock` or `ConnectTip` open, so the flush's is not left either.
pub(super) trait Balanced {
    fn balanced(&self) -> bool;
}

impl Balanced for () {
    fn balanced(&self) -> bool {
        true
    }
}

impl Balanced for Result<(), Abort> {
    fn balanced(&self) -> bool {
        self.is_ok()
    }
}

/// The stub evo state's key in `mn_diff`.
pub(super) const K_STATE: &[u8] = b"state";

/// The stub evo delta (CH2 §6.4): the state at the freeze's tip, which B1 writes to `mn_diff`; with
/// none (the coins profile), B1 writes nothing for the evo state.
pub(super) struct StubDelta {
    state: Option<[u8; 32]>,
}

impl StateDelta for StubDelta {
    fn write_b1<B: KvBatch>(&self, batch: &mut B) {
        if let Some(state) = &self.state {
            batch.put(cf("mn_diff"), K_STATE, state);
        }
    }

    fn bytes(&self) -> u64 {
        self.state.map_or(0, |s| (K_STATE.len() + s.len()) as u64)
    }
}

/// V's evo window over the stub state; the default has none.
#[derive(Default)]
pub(super) struct StubEvo {
    pub state: Option<[u8; 32]>,
    pub tip: Option<BlockHash>,
}

impl EvoWindow<StubDelta> for StubEvo {
    fn take(&mut self) -> StubDelta {
        StubDelta {
            state: self.state,
        }
    }

    fn bytes(&self) -> u64 {
        0
    }

    fn best_block(&self) -> Option<BlockHash> {
        self.tip
    }

    fn release(&mut self) {}
}

pub(super) fn cf(name: &str) -> Cf {
    CHAINSTATE_SCHEMA.cf(name).expect("a chainstate column family")
}

/// ST2's configuration: `-fastprune`, and prune mode as `cfg` sets it.
pub(super) fn files_cfg(cfg: &ChainConfig) -> BlockFilesConfig {
    let mut c = BlockFilesConfig::new(BLOCKS_DIR, MAGIC);
    c.fastprune = true;
    c.prune_mode = cfg.prune.is_some();
    c
}

/// A cache over `db` at `tip`, with the guard's planted bug armed. The coins' budget is large, so
/// no coin boundary ever makes a flush due.
pub(super) fn cache_at(db: &Arc<Db>, tip: Option<BlockRef>) -> CoinsCache<FaultyEngine> {
    let budget = CoinsBudget::new(64 << 20, 0, Ratio::HALF, 0);
    let mut cache = CoinsCache::new(db.clone(), budget, Salt::fixed(3), tip);
    cache.set_best_block(tip);
    #[cfg(feature = "planted-bugs")]
    if let Some(bug) = db.planted() {
        cache.plant(bug);
    }
    cache
}

/// `-dbcrashratio`'s roll for `after`: one that first comes up at roll `k` (0-based), or never.
///
/// # Panics
///
/// If `k` is above [`MAX_CRASH_AFTER`] (a configuration that parses never is).
pub(super) fn crash_roll(after: Option<u32>) -> CrashRoll {
    let Some(k) = after else {
        return CrashRoll::seeded(0, 0);
    };
    assert!(k <= MAX_CRASH_AFTER, "-dbcrashratio's roll at {k}: at most {MAX_CRASH_AFTER}");
    (0..)
        .map(|seed| CrashRoll::seeded(u64::from(k) + 1, seed))
        .find(|roll| {
            let mut roll = roll.clone();
            (0..=k).position(|_| roll.roll()) == Some(k as usize)
        })
        .expect("a seed")
}

/// The chainstate engine as the node holds it, and its adapters.
pub(super) struct Engines {
    /// The engine itself, for the checks (no latch, no abort sink).
    raw: FaultyEngine,
    obs: Arc<WorldObserver>,
    /// The engine under a latch whose abort sink is the observer.
    db: Arc<Db>,
    fs: Arc<FaultyFlatFiles>,
}

impl Engines {
    /// The engines on `world`, with `cfg`'s planted bug armed in the guard.
    pub(super) fn new(world: &World, cfg: &ChainConfig) -> Self {
        let raw = (*world.engine(CHAIN_STORE, &CHAINSTATE_SCHEMA)).clone();
        let obs = Arc::new(WorldObserver::new(world.clone()));
        let sink = Arc::new(WorldSink(obs.clone()));
        #[allow(unused_mut)]
        let mut db = GuardedEngine::new(raw.clone(), GlobalFailStop::new(), sink);
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = cfg.planted {
            db.plant(bug);
        }
        #[cfg(not(feature = "planted-bugs"))]
        let _ = cfg;
        Self {
            raw,
            obs,
            db: Arc::new(db),
            fs: world.fs(),
        }
    }

    pub(super) fn db(&self) -> &Arc<Db> {
        &self.db
    }

    pub(super) fn obs(&self) -> &WorldObserver {
        &self.obs
    }

    pub(super) fn obs_arc(&self) -> Arc<WorldObserver> {
        Arc::clone(&self.obs)
    }
}

/// Recovery's init error as Core's stop. Recovery has marked the stop of a failed store call
/// itself; a refusal (a replay with data missing, an inconsistent evo state) is unmarked, and R0
/// judges it.
fn init_abort(e: InitError) -> Abort {
    Abort::new(e.to_string())
}

/// Opens the chainstate under `LoadChainstate`'s path, which [`recover`] ends.
pub(super) fn open<'a>(
    world: &World,
    e: &'a Engines,
    cfg: &ChainConfig,
) -> Result<Opened<'a, FaultyEngine, FaultyFlatFiles>, Abort> {
    world.mark(Mark::PathBegin {
        path: PATH_LOAD,
    });
    ChainstateOpen::open(&e.db, e.fs.clone(), files_cfg(cfg), &*e.obs).map_err(init_abort)
}

/// Recovery over `index` with S = `sub_batch` and `-dbcrashratio`'s `crash`.
pub(super) fn recover(
    world: &World,
    opened: Opened<'_, FaultyEngine, FaultyFlatFiles>,
    index: &Arena,
    sub_batch: u64,
    crash: CrashRoll,
) -> Result<RecoveredChainstate<FaultyFlatFiles>, Abort> {
    let cfg = RecoveryConfig {
        pow: pow(),
        sub_batch,
        crash,
    };
    let rec = opened.recover(index, &SubscriberHub::new(), cfg).map_err(init_abort)?;
    world.mark(Mark::PathEnd);
    Ok(rec)
}

/// The `block_files` records of `db` that decode, by file number.
pub(super) fn file_infos(db: &impl KvRead) -> Result<BTreeMap<u32, BlockFileInfo>, StoreError> {
    let mut infos = BTreeMap::new();
    for r in db.iter_prefix(cf("block_files"), b"") {
        let (k, v) = r?;
        if let (Ok(n), Ok(info)) =
            (<[u8; 4]>::try_from(k.as_slice()), BlockFileInfo::decode_value(&v))
        {
            infos.insert(u32::from_be_bytes(n), info);
        }
    }
    Ok(infos)
}

/// What V shares with W during an op: the stores and their adapters.
pub(super) struct Stores {
    pub cfg: ChainConfig,
    pub world: World,
    pub db: Arc<Db>,
    /// The engine itself, for the checks (no latch, no abort sink).
    pub raw: FaultyEngine,
    fs: Arc<FaultyFlatFiles>,
    pub files: Arc<Files>,
    pub obs: Arc<WorldObserver>,
    hub: Arc<SubscriberHub>,
    /// b3's test subscriber (`index=`), on W's hub.
    pub sub: Option<Subs>,
    tuning: MemoryTuning,
    flush: FlushConfig,
    pub ledger: Arc<EffectLedger>,
    /// The time V's policy calls start from.
    pub t0: Instant,
    /// W's `covered_seq`, carried from one op's driver to the next.
    covered_seq: Cell<u64>,
    /// Inside [`with_ctl`](Self::with_ctl).
    in_ctl: Cell<bool>,
}

impl Stores {
    pub(super) fn new(
        world: &World,
        cfg: &ChainConfig,
        Engines {
            raw,
            obs,
            db,
            fs,
        }: Engines,
        files: Arc<Files>,
        sub: Option<Subs>,
    ) -> Self {
        Self {
            hub: sub.as_ref().map_or_else(Arc::default, |s| Arc::clone(&s.hub)),
            sub,
            cfg: cfg.clone(),
            world: world.clone(),
            raw,
            files,
            fs,
            obs,
            tuning: MemoryTuning,
            flush: FlushConfig {
                sub_batch: cfg.sub_batch.unwrap_or(u64::MAX),
                blocks_dir: BLOCKS_DIR.into(),
                data_dir: dashd_store::faulty::DATADIR.into(),
                live_copy: false,
            },
            ledger: Arc::new(EffectLedger::default()),
            t0: Instant::now(),
            covered_seq: Cell::new(0),
            in_ctl: Cell::new(false),
            db,
        }
    }

    pub(super) fn in_ctl(&self) -> bool {
        self.in_ctl.get()
    }

    /// Core's stop with `message`: marked once per epoch, after `StoreFailed { op }` for a store
    /// call's failure.
    pub(super) fn stop(&self, op: Option<&'static str>, message: &str) -> Abort {
        self.obs.abort(op, message);
        Abort::new(message)
    }

    /// V's fatal block-file read (`DisconnectTip`'s undo, activation's block), with Core's text
    /// for it: an I/O failure trips the latch, as recovery's read does, so V fail-stops (FA7); a
    /// short or missing record is not a read failure.
    pub(super) fn read_failed(&self, e: &FlatFileError, message: &str) -> Abort {
        let io = io_failure(e);
        if io {
            self.db.latch().trip(Failure {
                kind: e.kind(),
                site: BLOCK_FILES_READ_SITE,
                message: e.to_string(),
            });
        }
        self.stop(io.then_some("read"), message)
    }

    /// A failure of `op` that is reported unless `latched`: another failure tripped the latch
    /// first, and has been.
    pub(super) fn failed(&self, latched: bool, op: &'static str, message: &str) -> Abort {
        if latched {
            Abort::new(message)
        } else {
            self.stop(Some(op), message)
        }
    }

    /// A block-file write that failed, with Core's text for the step that failed: the old file's
    /// flush at a rollover, `CheckDiskSpace`, and otherwise the write (`write`), which a failed
    /// open, allocation or link-count read on the way is in Core.
    pub(super) fn files_failed(&self, e: &FlatFileError, op: &'static str, write: &str) -> Abort {
        let message = match e {
            FlatFileError::Io {
                context,
                ..
            } if context.starts_with(FLUSH_BLOCK_FILE) || context.starts_with(FLUSH_UNDO_FILE) => {
                context
            }
            FlatFileError::NoSpace => DISK_SPACE_LOW,
            _ => write,
        };
        self.failed(matches!(e, FlatFileError::Failed), op, message)
    }

    /// A store write that failed.
    pub(super) fn write_failed(&self, e: &StoreError) -> Abort {
        let latched = e.kind() == Some(dashd_store::ErrorKind::ReadOnly);
        self.failed(latched, "write", &format!("{FLUSH_FAILED}{e}"))
    }

    pub(super) fn enter(&self, path: &'static str) {
        self.world.mark(Mark::PathBegin {
            path,
        });
    }

    pub(super) fn leave(&self) {
        self.world.mark(Mark::PathEnd);
    }

    /// A block's bytes from the block files.
    pub(super) fn read_block(&self, pos: FlatFilePos) -> Result<Vec<u8>, FlatFileError> {
        let mut out = Vec::new();
        self.files.read(|f| f.reader()).read_block(pos, &mut out)?;
        Ok(out)
    }

    /// A block's undo bytes from the block files.
    pub(super) fn read_undo(
        &self,
        pos: FlatFilePos,
        parent: &BlockHash,
    ) -> Result<Vec<u8>, FlatFileError> {
        let mut out = Vec::new();
        self.files.read(|f| f.reader()).read_undo(pos, parent.as_bytes(), &mut out)?;
        Ok(out)
    }

    /// Runs `f` on W for one op, rebuilt for each op so that each of W's store calls is a world
    /// event; `covered_seq` carries over to the next op. W's failure is the op's `Abort`: it has
    /// reported (or `-dbcrashratio` came up).
    pub(super) fn with_ctl<R: Balanced>(
        &self,
        f: impl FnOnce(&mut Ctl<'_>) -> Result<R, Fatal>,
    ) -> Result<R, Abort> {
        let env = FlushEnv {
            engine: &self.db,
            files: &self.files,
            fs: &self.fs,
            hub: &self.hub,
            obs: &*self.obs,
            tuning: &self.tuning,
            cfg: &self.flush,
            covered_seq: self.covered_seq.get(),
            // The forward run's flushes only.
            crash: crash_roll(self.cfg.crash_after.filter(|_| self.world.epoch() == 0)),
        };
        let signals = Arc::new(ReleaseSignals {
            state: Mutex::default(),
            world: self.world.clone(),
        });
        let mut ctl = FlushControl::new(
            SignalDriver {
                inner: InlineDriver::new(env),
                signals: signals.clone(),
                pending: None,
            },
            FlushPolicy::new(false),
            signals.clone(),
            self.ledger.clone(),
            self.obs.clone(),
        );
        // W's hub, and the subscriber's steps while W waits on it (steps 3b and 6); its failure
        // trips the latch, which stops W.
        if let Some(sub) = &self.sub {
            ctl.set_hub(Arc::clone(&self.hub));
            ctl.driver().inner.set_pump(move || {
                let mut blocked = false;
                sub.step(&mut blocked).unwrap_or_else(|e| {
                    sub.fail(&e);
                    true
                })
            });
        }
        self.enter(PATH_FLUSH);
        self.in_ctl.set(true);
        let r = f(&mut ctl);
        signals.assert_signalled();
        self.in_ctl.set(false);
        if let Some(sub) = &self.sub {
            sub.mark_synced();
        }
        if r.as_ref().is_ok_and(Balanced::balanced) {
            self.leave();
        }
        self.covered_seq.set(ctl.driver().inner.env.covered_seq);
        r.map_err(|f| Abort::new(self.obs.aborted().unwrap_or_else(|| f.0.message.clone())))
    }

    /// The coins table, as the reference models it.
    pub(super) fn coins(&self) -> BTreeMap<OutPointKey, Vec<u8>> {
        self.raw
            .iter_prefix(cf("coins"), b"")
            .map(|r| {
                let (k, v) = r.expect("a check's read");
                (OutPointKey::from_db_key(&k).expect("a coin key"), v)
            })
            .collect()
    }

    /// R7a's digest of the recovered state: the tip, the coins, and the evo state (read last).
    pub(super) fn projection(
        &self,
        tip: Option<&BlockHash>,
        evo: impl FnOnce() -> Option<Vec<u8>>,
    ) -> [u8; 32] {
        let mut buf = tip.map_or([0; 32], hash).to_vec();
        for (k, coin) in self.coins() {
            let mut key = [0; DB_KEY_MAX];
            let n = k.db_key(&mut key);
            buf.extend_from_slice(&key[..n]);
            buf.extend_from_slice(&coin);
        }
        buf.extend_from_slice(&evo().unwrap_or_default());
        dash_consensus::hash::sha256d(&buf)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use dash_chain::BlockRef;
    use dashd_store::faulty::{
        Abort, CrashKind, EventSel, FatalCause, Fault, FaultKind, FaultOutcome, InvariantId, Mark,
        OpKind, RefExtractor, Repro, Scenario, StoreSel, Sut, Trace, Violation, World,
        forward_trace, replay_trace,
    };

    use super::ReleaseSignals;
    use crate::crash::{ChainConfig, ChainOp, ChainRef, ChainSut, ops};

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SignalFault {
        DropShortcut,
        Lead,
        RecoveryHeight,
        SpuriousIndex,
    }

    thread_local! {
        static FAULT: Cell<Option<SignalFault>> = const { Cell::new(None) };
        static LAST_TIP: Cell<Option<BlockRef>> = const { Cell::new(None) };
    }

    fn with_fault<T>(fault: SignalFault, f: impl FnOnce() -> T) -> T {
        struct Reset(Option<SignalFault>);
        impl Drop for Reset {
            fn drop(&mut self) {
                FAULT.set(self.0);
            }
        }
        let _reset = Reset(FAULT.replace(Some(fault)));
        f()
    }

    pub(super) fn signal(signals: &ReleaseSignals, mut tip: BlockRef) -> Option<BlockRef> {
        LAST_TIP.set(Some(tip));
        match FAULT.get() {
            Some(SignalFault::DropShortcut)
                if signals
                    .state
                    .lock()
                    .expect("signals")
                    .released
                    .as_ref()
                    .is_some_and(|r| r.new.is_none()) =>
            {
                None
            }
            Some(SignalFault::Lead) => None,
            Some(SignalFault::RecoveryHeight) if signals.world.epoch() > 0 => {
                tip.height += 1;
                Some(tip)
            }
            _ => Some(tip),
        }
    }

    pub(super) fn submitted(signals: &ReleaseSignals, tip: Option<BlockRef>) {
        if FAULT.get() == Some(SignalFault::Lead)
            && let Some(tip) = tip
        {
            signals.observe(tip);
        }
    }

    pub(super) fn released(signals: &ReleaseSignals) {
        if FAULT.get() == Some(SignalFault::SpuriousIndex)
            && signals.state.lock().expect("signals").released.as_ref().is_some_and(|r| !r.required)
            && let Some(tip) = LAST_TIP.get()
        {
            signals.observe(tip);
        }
    }

    #[test]
    fn ordinary_index_only_write_rejects_a_signal() {
        let sc = Scenario::<ChainSut>::new("index-signal", ChainConfig::default(), ops("c1;f;i"));
        let (_, violations) = with_fault(SignalFault::SpuriousIndex, || forward_trace(&sc));
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].invariant, InvariantId::FA8);
        assert!(
            violations[0]
                .detail
                .ends_with("ordinary index-only write must not signal ChainStateFlushed")
        );
    }

    #[test]
    fn clean_always_shortcut_requires_a_signal() {
        let sc = Scenario::<ChainSut>::new("shortcut", ChainConfig::default(), ops("c1;f;f;i"));
        let (clean, violations) = forward_trace(&sc);
        assert!(violations.is_empty(), "{violations:?}");
        let (trace, violations) = with_fault(SignalFault::DropShortcut, || forward_trace(&sc));
        assert_eq!(trace.events().count(), clean.events().count());
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].invariant, InvariantId::FA8);
        assert!(violations[0].detail.ends_with("full flush released without ChainStateFlushed"));
    }

    #[test]
    fn leading_signal_is_non_fatal_and_paired_with_its_release() {
        let sc = Scenario::<ChainSut>::new("leading", ChainConfig::default(), ops("c1;f;c1;f"));
        let (clean, violations) = forward_trace(&sc);
        assert!(violations.is_empty(), "{violations:?}");
        let (trace, violations) = with_fault(SignalFault::Lead, || forward_trace(&sc));
        assert_eq!(trace.events().count(), clean.events().count());
        assert!(!violations.is_empty());
        assert!(violations.iter().all(|v| v.invariant == InvariantId::FA8), "{violations:?}");
        assert!(
            violations
                .iter()
                .any(|v| v.detail.ends_with("ChainStateFlushed before its flush's release"))
        );
        assert!(
            violations
                .iter()
                .any(|v| v.detail.ends_with("ChainStateFlushed must match durable meta.tip"))
        );
        assert!(
            !violations.iter().any(|v| v.detail.contains("released without ChainStateFlushed")),
            "{violations:?}"
        );
    }

    struct RestartPrune(ChainSut);

    impl Sut for RestartPrune {
        type Config = ChainConfig;
        type Op = ChainOp;
        type Ref = ChainRef;
        const NAME: &'static str = "restart-prune";

        fn open(world: &World, cfg: &ChainConfig) -> Result<Self, Abort> {
            let cfg = if world.epoch() > 0 {
                ChainConfig {
                    prune: Some(1),
                    ..cfg.clone()
                }
            } else {
                cfg.clone()
            };
            ChainSut::open(world, &cfg).map(Self)
        }

        fn apply(&mut self, op: &ChainOp) -> Result<(), Abort> {
            self.0.apply(op)
        }

        fn check(
            &self,
            world: &World,
            refm: &ChainRef,
            trace: &Trace,
        ) -> Result<(), Vec<Violation>> {
            self.0.check(world, refm, trace)
        }

        fn forward_check(&self, world: &World, refm: &ChainRef, trace: &Trace) -> Vec<Violation> {
            self.0.forward_check(world, refm, trace)
        }

        fn recovery_observations(
            world: &World,
            cfg: &ChainConfig,
            trace: &Trace,
        ) -> Vec<Violation> {
            ChainSut::recovery_observations(world, cfg, trace)
        }

        fn store_check(world: &World, cfg: &ChainConfig) -> Vec<Violation> {
            ChainSut::store_check(world, cfg)
        }

        fn sanctioned_after(cfg: &ChainConfig, message: &str, cause: &FatalCause) -> bool {
            ChainSut::sanctioned_after(cfg, message, cause)
        }

        fn extractor(cfg: &ChainConfig) -> Option<Arc<dyn RefExtractor>> {
            ChainSut::extractor(cfg)
        }
    }

    #[test]
    fn recovery_signal_survives_a_later_sanctioned_abort() {
        let cfg = ChainConfig {
            prune: Some(0),
            prune_after: Some(0),
            sub_batch: Some(50_000),
            ..ChainConfig::default()
        };
        let sc = Scenario::<RestartPrune>::new("recovery-signal", cfg, ops("c410;f;a2;i"));
        let (forward, before) = forward_trace(&sc);
        assert!(before.is_empty(), "{before:?}");
        let mut repro = Repro::new::<RestartPrune>(&sc, None);
        repro.crash_at = forward.events().last().map(|(id, ..)| id.index);
        repro.kind = CrashKind::ProcessKill;
        repro.recovery_fault = Some(Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::Read,
                n: 4,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        });
        let (violations, trace) = with_fault(SignalFault::RecoveryHeight, || {
            replay_trace::<RestartPrune>(&repro).expect("replays")
        });
        let signals = trace
            .marks_with_epoch()
            .filter(|(epoch, mark)| {
                *epoch == 1 && matches!(mark, Mark::Log { line } if line.starts_with("BL-807 "))
            })
            .count();
        assert_eq!(signals, 1);
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].invariant, InvariantId::FA8);
        assert!(violations[0].detail.ends_with("ChainStateFlushed must name exactly new"));
        assert!(trace.marks().any(
            |mark| matches!(mark, Mark::Abort { message } if message.contains("Failed to read block"))
        ));
    }

    #[test]
    fn recovery_signal_does_not_decide_a_warranted_refusal() {
        let cfg = ChainConfig {
            prune: Some(0),
            prune_after: Some(0),
            sub_batch: Some(50_000),
            ..ChainConfig::default()
        };
        let sc = Scenario::<RestartPrune>::new("refusal-signal", cfg, ops("c410;f;x408;i"));
        let (forward, before) = forward_trace(&sc);
        assert!(before.is_empty(), "{before:?}");
        let mut repro = Repro::new::<RestartPrune>(&sc, None);
        repro.crash_at = forward.events().last().map(|(id, ..)| id.index);
        repro.kind = CrashKind::ProcessKill;
        let (clean, _) = replay_trace::<RestartPrune>(&repro).expect("replays");
        assert!(clean.is_empty(), "{clean:?}");
        let (violations, _) = with_fault(SignalFault::RecoveryHeight, || {
            replay_trace::<RestartPrune>(&repro).expect("replays")
        });
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert_eq!(violations[0].invariant, InvariantId::FA8);
        assert!(violations[0].detail.ends_with("ChainStateFlushed must name exactly new"));
    }
}
