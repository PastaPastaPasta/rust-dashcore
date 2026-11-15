//! V's side over the inline and thread drivers (CH2 §4.2; E-b1.2, E-b1.4, E-b1.7, E-b1.8, D14,
//! DEC-166): the freeze after the step and its check, `ChainStateFlushed` after B1, the waits, the
//! ALWAYS shortcut, the copy point, kept causes, failures, the crash roll, and V's answer to W's
//! `PruneClears`.

use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use dash_consensus::BlockId;
use dash_consensus::primitives::TxOut;
use dashd_store::flatfile::{
    BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs, FlatRead, OsFs,
};
use dashd_store::{KvRead as _, MemoryEngine};

use super::*;
use crate::coins::{CoinSrc, CoinsBudget, OutPointKey, Ratio, Salt};
use crate::flush::test_files::{KEEP, blk_on_disk, create_on_disk, equal_files};
#[cfg(feature = "test-support")]
use crate::flush::{CauseCounts, DATABASE_FLUSH_INTERVAL, FlushTally};
use crate::flush::{
    CrashRoll, DATABASE_WRITE_INTERVAL, FilesPruneHost, FlushCauses, FlushConfig, FlushEnv,
    FlushObserver, FlushSignals, FlushStep, InlineDriver, LogCat, NoPrune, ObsMark,
    RecordingObserver, RecordingSignals, SubscriberHub, ThreadDriver, WThread,
};
use crate::store::held::{HeldSink, W_THREAD, fail_on_w, until};
use crate::store::{
    AbortMark, CHAINSTATE_SCHEMA, GlobalFailStop, GuardedFiles, Meta, ProbeEngine, RecordingSink,
};
use crate::tuning::MemoryTuning;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

fn block(height: u32) -> BlockRef {
    let mut hash = [0xc2; 32];
    hash[..4].copy_from_slice(&height.to_le_bytes());
    BlockRef {
        id: BlockId(height),
        hash: BlockHash::from(hash),
        height,
    }
}

fn key(i: u32) -> OutPointKey {
    let mut k = [0u8; 36];
    k[..4].copy_from_slice(&i.to_le_bytes());
    k[4] = 0x5b;
    OutPointKey(k)
}

/// The node's stores, W's inputs and V's ports.
struct Stores<F: FlatFs, E: KvEngine = MemoryEngine> {
    _dir: tempfile::TempDir,
    db: Arc<GuardedEngine<ProbeEngine<E>>>,
    files: Arc<GuardedFiles<F>>,
    fs: Arc<F>,
    hub: Arc<SubscriberHub>,
    obs: Arc<RecordingObserver>,
    tuning: Arc<MemoryTuning>,
    cfg: FlushConfig,
    sink: Arc<RecordingSink>,
    /// The engine's way to `sink`, where a test holds W's report (BL-554).
    held: Arc<HeldSink>,
    signals: Arc<RecordingSignals>,
}

impl Stores<OsFs> {
    fn new(sub_batch: u64) -> Self {
        Self::over(OsFs::new(), sub_batch)
    }
}

impl<F: FlatFs> Stores<F> {
    fn over(fs: F, sub_batch: u64) -> Self {
        Self::over_files(fs, sub_batch, BlockFilesState::default())
    }

    /// Stores whose block files start at `state`, each file on disk.
    fn over_files(fs: F, sub_batch: u64, state: BlockFilesState) -> Self {
        Self::build(fs, sub_batch, state, |_| ProbeEngine::new(&CHAINSTATE_SCHEMA))
    }
}

/// The chainstate on RocksDB, in the stores' directory.
#[cfg(feature = "rocksdb")]
impl Stores<OsFs, dashd_store::RocksEngine> {
    fn rocks(sub_batch: u64) -> Self {
        Self::build(OsFs::new(), sub_batch, BlockFilesState::default(), |dir| {
            let path = dir.join("chainstate");
            let config = Default::default();
            ProbeEngine::over(
                dashd_store::RocksEngine::open(&path, &CHAINSTATE_SCHEMA, &config).unwrap().0,
            )
        })
    }
}

impl<F: FlatFs, E: KvEngine> Stores<F, E> {
    /// Stores over `engine`'s chainstate, made in the stores' directory, and block files that
    /// start at `state`.
    fn build(
        fs: F,
        sub_batch: u64,
        state: BlockFilesState,
        engine: impl FnOnce(&Path) -> ProbeEngine<E>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let blocks = dir.path().join("blocks");
        create_on_disk(&blocks, &state);
        let sink = Arc::new(RecordingSink::default());
        let held = Arc::new(HeldSink::new(sink.clone()));
        let db =
            Arc::new(GuardedEngine::new(engine(dir.path()), GlobalFailStop::new(), held.clone()));
        let fs = Arc::new(fs);
        let files = BlockFiles::with_fs(fs.clone(), BlockFilesConfig::new(&blocks, MAGIC), state);
        Self {
            files: Arc::new(GuardedFiles::new(files, db.latch().clone())),
            db,
            fs,
            hub: Arc::default(),
            obs: Arc::default(),
            tuning: Arc::new(MemoryTuning),
            cfg: FlushConfig {
                sub_batch,
                data_dir: dir.path().to_path_buf(),
                blocks_dir: blocks,
                live_copy: false,
            },
            _dir: dir,
            sink,
            held,
            signals: Arc::default(),
        }
    }

    fn env(&self, crash: CrashRoll) -> FlushEnv<'_, ProbeEngine<E>, F, MemoryTuning> {
        self.env_with(&*self.obs, crash)
    }

    fn env_with<'a>(
        &'a self,
        obs: &'a dyn FlushObserver,
        crash: CrashRoll,
    ) -> FlushEnv<'a, ProbeEngine<E>, F, MemoryTuning> {
        FlushEnv {
            engine: &self.db,
            files: &self.files,
            fs: &self.fs,
            hub: &self.hub,
            obs,
            tuning: &self.tuning,
            cfg: &self.cfg,
            covered_seq: 0,
            crash,
        }
    }

    fn inline(
        &self,
        crash: CrashRoll,
        synchronous: bool,
    ) -> Ctl<InlineDriver<'_, ProbeEngine<E>, F, MemoryTuning, ()>> {
        self.control(InlineDriver::new(self.env(crash)), synchronous)
    }

    fn control<R: FlushDriver>(&self, driver: R, synchronous: bool) -> Ctl<R> {
        FlushControl::new(
            driver,
            FlushPolicy::new(synchronous),
            self.signals.clone(),
            Arc::default(),
            self.obs.clone(),
        )
    }

    fn v(&self) -> V<E> {
        self.v_with(64 << 20)
    }

    /// V with a coins share of `coins_share` bytes.
    fn v_with(&self, coins_share: u64) -> V<E> {
        self.v_budget(CoinsBudget::new(coins_share, 0, Ratio::HALF, 0))
    }

    fn v_budget(&self, budget: CoinsBudget) -> V<E> {
        V {
            coins: CoinsCache::new(self.db.clone(), budget, Salt::fixed(5), None),
            tip: block(0),
            seq: 0,
            dirty: Vec::new(),
            scripted: false,
            now: Instant::now(),
            evo: CountingEvo::default(),
        }
    }

    fn meta(&self) -> Meta {
        Meta::read(&*self.db).unwrap()
    }

    /// Appends a block to the block files, so W's files barrier has work.
    fn save_block(&self, height: u32) {
        let mut raw = [0; 81];
        raw[..4].copy_from_slice(&height.to_le_bytes());
        let bytes = dash_consensus::decode_block(&mut &raw[..]).unwrap().core_bytes();
        self.files.mutate(|f| f.save_block(&bytes, height, 1_700_000_000, None)).unwrap();
    }
}

type Ctl<R> = FlushControl<R>;

/// V: its cache, its tip and `seq`, the block index's dirty records, and its evo window.
struct V<E: KvEngine = MemoryEngine, W = CountingEvo> {
    coins: CoinsCache<ProbeEngine<E>>,
    tip: BlockRef,
    seq: u64,
    dirty: Vec<IndexWrite>,
    scripted: bool,
    now: Instant,
    evo: W,
}

/// V's evo window: the bytes a test sets, with its takes and releases counted. A take freezes the
/// active bytes until the release, as `EvoLayer` does (CH7 D22, D26).
#[derive(Debug, Default)]
struct CountingEvo {
    active: u64,
    frozen: Option<u64>,
    takes: u32,
    releases: u32,
}

impl EvoWindow<()> for CountingEvo {
    fn take(&mut self) {
        assert!(self.frozen.is_none(), "one window in flight");
        self.frozen = Some(std::mem::take(&mut self.active));
        self.takes += 1;
    }

    fn bytes(&self) -> u64 {
        self.active + self.frozen.unwrap_or(0)
    }

    fn best_block(&self) -> Option<BlockHash> {
        None
    }

    fn release(&mut self) {
        assert!(self.frozen.take().is_some(), "a release of a taken window");
        self.releases += 1;
    }
}

impl<E: KvEngine, W> V<E, W> {
    /// V with the evo window `evo`.
    fn with_evo<W2>(self, evo: W2) -> V<E, W2> {
        let Self {
            coins,
            tip,
            seq,
            dirty,
            scripted,
            now,
            evo: _,
        } = self;
        V {
            coins,
            tip,
            seq,
            dirty,
            scripted,
            now,
            evo,
        }
    }

    fn with_cx<D: StateDelta, T>(
        &mut self,
        f: impl FnOnce(&mut FlushCx<'_, ProbeEngine<E>, D>) -> T,
    ) -> T
    where
        W: EvoWindow<D>,
    {
        let mut cx = FlushCx {
            now: self.now,
            coins: &mut self.coins,
            mempool_usage: 0,
            tip: self.tip,
            last_seq: self.seq,
            index: &mut self.dirty,
            evo: &mut self.evo,
            prune: &mut NoPrune,
            manual_prune_height: None,
            reindex: false,
            is_ibd: false,
            left_ibd: false,
            scripted: self.scripted,
        };
        f(&mut cx)
    }

    /// A connector's commit of block `height`: `n` coins from `from`, the best block, and a dirty
    /// index record. V's tip moves only at [`step`](Self::step).
    fn connect(&mut self, from: u32, n: u32, height: u32) {
        for i in from..from + n {
            let out = TxOut::from_duffs(1_000 + i64::from(i), vec![0x51; 25].into());
            let src = CoinSrc::Output {
                out: &out,
                height,
                coinbase: false,
            };
            self.coins.add(&key(i), src, false).unwrap();
        }
        self.coins.set_best_block(Some(block(height)));
        self.dirty.push(IndexWrite {
            hash: block(height).hash,
            bytes: vec![height as u8; 40],
        });
    }

    /// The tip step: V's tip becomes `tip`, at the next `seq`.
    fn step(&mut self, tip: BlockRef) -> u64 {
        self.tip = tip;
        self.seq += 1;
        self.seq
    }

    /// Core's IF_NEEDED site around a step to `tip`: `at_site` before, `freeze_after_step` after.
    fn site_and_step<R: FlushDriver>(
        &mut self,
        ctl: &mut Ctl<R>,
        tip: BlockRef,
    ) -> Result<(), Fatal>
    where
        W: EvoWindow<R::Delta>,
    {
        let pending = self.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx))?;
        let seq = self.step(tip);
        self.with_cx(|cx| pending.freeze_after_step(ctl, cx, tip, seq))
    }
}

/// Runs W until it has nothing left.
fn run_w<F: FlatFs, E: KvEngine, D: StateDelta>(
    ctl: &mut Ctl<InlineDriver<'_, ProbeEngine<E>, F, MemoryTuning, D>>,
) -> Vec<FlushStep> {
    std::iter::from_fn(|| ctl.driver().step()).collect()
}

/// E-b1.2 and DEC-166: the decision at the site captures the pre-step tip; the freeze takes the
/// post-step pair; `ChainStateFlushed` is handed over only when V sees the release, after B1 is
/// durable, and names the block the freeze wrote: at Core's `ConnectTip` site, the connected block,
/// not the pre-step tip.
#[test]
fn the_freeze_follows_the_step_and_signals_after_b1() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 20, 1);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(
        pending.action(),
        FlushAction::Freeze { causes, wait_done: false, .. } if *causes == FlushCauses::SCRIPTED
    ));
    assert!(!ctl.in_flight(), "at_site never freezes");
    let seq = v.step(block(1));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(1), seq)).unwrap();
    assert!(ctl.in_flight());
    assert!(v.dirty.is_empty(), "the freeze took the index records");

    // W runs to Durable: `T_ack` moves, the signal waits for V to see the release.
    assert!(std::iter::from_fn(|| ctl.driver().step()).any(|s| s == FlushStep::Durable));
    assert_eq!(s.hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((block(1), 1)));
    assert!(s.signals.tips().is_empty());
    run_w(&mut ctl);
    assert!(s.signals.tips().is_empty());
    assert_eq!((v.evo.takes, v.evo.releases), (1, 0), "the evo window is held until V's release");
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert_eq!((v.evo.takes, v.evo.releases), (1, 1));
    // Core v24.0.0: validation.cpp:3105, :3125, flushed at :3468 before `SetTip` at :3484
    assert_eq!(s.signals.tips(), [block(1)]);
    assert!(!ctl.in_flight());
    assert_eq!((v.coins.durable(), s.meta().tip), (Some(block(1)), Some(block(1).hash)));
}

/// A disconnect of d freezes at `(parent(d), seq)`, and its signal names the parent, the block
/// the freeze wrote, as Core's does at `DisconnectTip`.
#[test]
fn a_disconnect_freezes_at_the_parent_and_signals_the_parent() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    for h in 1..=2 {
        v.connect(h * 10, 10, h);
        v.site_and_step(&mut ctl, block(h)).unwrap();
    }
    assert!(!ctl.in_flight(), "nothing was due");
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(s.meta().tip, Some(block(2).hash));

    // Disconnect 2: its coins go, the best block is 1, and V's tip moves to 1 at the next seq.
    for i in 20..30 {
        assert!(v.coins.spend(&key(i)).unwrap());
    }
    v.coins.set_best_block(Some(block(1)));
    v.scripted = true;
    v.site_and_step(&mut ctl, block(1)).unwrap();
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    // Core v24.0.0: validation.cpp:3105, :3125, flushed at :3315 before `SetTip` at :3327
    assert_eq!(s.signals.tips(), [block(2), block(1)]);
    assert_eq!(s.hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((block(1), 3)));
    assert_eq!(s.meta().tip, Some(block(1).hash));
}

/// CH1-node §7.1's check: a packet whose `new` is not the coins' best block and V's tip is never
/// handed to W; V reports `AbortNode` and trips the latch.
#[test]
fn a_mislabelled_freeze_aborts_the_node() {
    for wrong in [false, true] {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut v = s.v();
        v.scripted = true;
        v.connect(0, 5, 1);
        let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
        // Either the tip step is skipped, or the pair names another block.
        let tip = if wrong {
            block(7)
        } else {
            block(1)
        };
        let seq = if wrong {
            v.step(tip)
        } else {
            v.seq + 1
        };
        let err = v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, tip, seq)).unwrap_err();
        assert_eq!(err.0.site, "chainstate flush");
        assert!(!ctl.in_flight());
        assert!(v.coins.frozen().is_none(), "nothing was frozen");
        assert!(matches!(
            &s.sink.marks()[..],
            [AbortMark::AbortNode {
                user: None,
                ..
            }]
        ));
        assert!(s.db.latch().tripped().is_some());
    }
}

/// CH3b D240's route (i): a pending `FlushDue` with no job in flight is a NONE freeze with `DMAX`
/// at V's tip, so §7.1's check fails at a bell as at a site, and V's own `AbortNode` trips the
/// latch before anything is frozen or handed to W. Core has no counterpart to the pair check.
#[test]
fn a_flush_due_freeze_at_a_mislabelled_pair_aborts_the_node() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    ctl.set_hub(s.hub.clone());
    let mut v = s.v();
    assert!(!ctl.in_flight() && !v.scripted, "no job and no other cause");
    s.hub.post_flush_due();
    v.coins.set_best_block(Some(block(1)));
    // Core v24.0.0: validation.cpp:3088, :3102, the coins flush writes the best block it reads
    let err = v.with_cx(|cx| ctl.answer_due(cx)).unwrap_err();
    assert_eq!(err.0.site, "chainstate flush");
    assert!(err.0.message.starts_with("a freeze at "), "{}", err.0.message);
    assert!(!ctl.in_flight());
    assert!(v.coins.frozen().is_none(), "nothing was frozen");
    assert_eq!(v.evo.takes, 0);
    assert!(matches!(
        &s.sink.marks()[..],
        [AbortMark::AbortNode {
            user: None,
            ..
        }]
    ));
    assert!(s.db.latch().tripped().is_some());
}

/// CH7 D35: an evo-critical freeze waits for its release, as Core's evo-critical flush is
/// synchronous, so the next block's check sees only the new window and flushes nothing.
#[test]
fn an_evo_critical_freeze_waits_for_its_release() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.evo.active = EVO_CRITICAL_BYTES;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    assert!(!ctl.in_flight(), "the freeze waited for its release");
    assert_eq!((v.evo.takes, v.evo.releases, v.evo.bytes()), (1, 1, 0));
    assert_eq!(s.meta().tip, Some(block(1).hash));

    v.evo.active = 1 << 10;
    v.connect(10, 10, 2);
    v.site_and_step(&mut ctl, block(2)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!((v.evo.takes, v.evo.releases), (1, 1), "no second flush");
}

/// ALWAYS (§4.2): wait for the job in flight, then freeze the rest and wait, so the database is at
/// V's tip on return.
#[test]
fn always_waits_for_the_flight_then_flushes_the_rest() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.scripted = false;
    v.connect(10, 10, 2);
    v.site_and_step(&mut ctl, block(2)).unwrap();
    assert!(ctl.in_flight());
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(s.meta().tip, Some(block(2).hash));
    assert_eq!(v.coins.durable(), Some(block(2)));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
}

/// The ALWAYS shortcut (§4.2, E-b1.4): with nothing dirty at the durable tip, an index-only write,
/// waited for, then `ChainStateFlushed`.
#[test]
fn the_always_shortcut_writes_the_index_then_signals() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    let flushes = s.obs.marks().iter().filter(|m| matches!(m, ObsMark::FlushBegin { .. })).count();
    assert_eq!((v.evo.takes, v.evo.releases), (1, 1));

    v.dirty.push(IndexWrite {
        hash: block(1).hash,
        bytes: vec![9; 12],
    });
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert!(!ctl.in_flight());
    let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    assert_eq!(
        s.db.inner().unprobed().get(cf, block(1).hash.as_bytes()).unwrap(),
        Some(vec![9; 12])
    );
    let marks = s.obs.marks();
    assert_eq!(marks.iter().filter(|m| matches!(m, ObsMark::FlushBegin { .. })).count(), flushes);
    assert_eq!((v.evo.takes, v.evo.releases), (1, 1), "an index-only write takes no evo window");
    assert_eq!(s.signals.tips(), [block(1), block(1)]);
}

/// Core signals only after a full flush with a best block (`validation.cpp:3089-3125` at v24.0.0):
/// on a fresh datadir, before the first connect, an ALWAYS flush (at startup or shutdown) writes
/// the block index alone and sends nothing.
#[test]
fn an_always_flush_with_no_best_block_sends_nothing() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    assert_eq!(v.coins.best_block(), None);
    v.dirty.push(IndexWrite {
        hash: block(0).hash,
        bytes: vec![9; 12],
    });
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert!(!ctl.in_flight());
    let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    assert_eq!(
        s.db.inner().unprobed().get(cf, block(0).hash.as_bytes()).unwrap(),
        Some(vec![9; 12])
    );
    assert_eq!(s.meta().tip, None);
    assert!(s.signals.tips().is_empty());
}

/// E-b1.8, O-R6-m2: a copy point's snapshot is at V's tip with no marker, one-batch or split, and
/// later flushes do not show through it.
#[test]
fn a_copy_point_snapshots_the_flushed_tip() {
    for sub_batch in [1 << 20, 2 << 10] {
        let s = Stores::new(sub_batch);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut v = s.v();
        v.connect(0, 200, 1);
        v.step(block(1));
        let snapshot = v.with_cx(|cx| ctl.copy_point(cx, &s.db)).unwrap();
        let at = Meta::read(&snapshot).unwrap();
        assert_eq!((at.tip, at.head_blocks), (Some(block(1).hash), None));
        v.connect(200, 10, 2);
        v.step(block(2));
        v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
        assert_eq!(s.meta().tip, Some(block(2).hash));
        assert_eq!(Meta::read(&snapshot).unwrap().tip, Some(block(1).hash));
    }
}

/// A cause that meets a job in flight is kept, and V's next poll after the release freezes it at
/// once, at V's tip and last seq.
#[test]
fn a_kept_cause_freezes_at_the_next_poll() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.connect(10, 10, 2);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(pending.action(), FlushAction::Pending { .. }));
    let seq = v.step(block(2));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(2), seq)).unwrap();
    run_w(&mut ctl);
    v.scripted = false;
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(ctl.in_flight(), "the kept cause froze at once");
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert_eq!(s.hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((block(2), 2)));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(!ctl.in_flight(), "and only once");
}

/// CH3a a10 Q2: `settle` reaches a fixed point. The job in flight is released, then the cause
/// kept for it (the pending second job) is frozen and released too, before it returns; a second
/// settle finds nothing to do.
#[cfg(feature = "test-support")]
#[test]
fn settle_releases_the_job_in_flight_then_the_kept_cause() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.connect(10, 10, 2);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(pending.action(), FlushAction::Pending { .. }));
    let seq = v.step(block(2));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(2), seq)).unwrap();
    assert!(ctl.in_flight() && ctl.pending() == FlushCauses::SCRIPTED, "a second job is kept");
    v.scripted = false;
    assert_eq!(v.with_cx(|cx| ctl.settle(cx)).unwrap(), 2, "both jobs released");
    assert!(!ctl.in_flight());
    assert!(ctl.pending().is_empty());
    assert_eq!(s.meta().tip, Some(block(2).hash));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
    assert_eq!(v.with_cx(|cx| ctl.settle(cx)).unwrap(), 0, "settled");
    assert_eq!(s.signals.tips(), [block(1), block(2)], "and no flush of its own");
}

/// CH3a a10-2: a tally on `ctl`, as a test's node supplies one.
#[cfg(feature = "test-support")]
fn tally<R: FlushDriver>(ctl: &mut Ctl<R>) -> Arc<FlushTally> {
    let tally = Arc::new(FlushTally::default());
    ctl.set_tally(Arc::clone(&tally));
    tally
}

/// Counts by cause, for a tally's expected half.
#[cfg(feature = "test-support")]
fn by_cause(list: &[(&str, u64)]) -> std::collections::BTreeMap<String, u64> {
    list.iter().map(|(cause, n)| ((*cause).to_owned(), *n)).collect()
}

/// M-5: ALWAYS at the durable tip with nothing dirty takes the shortcut, an index-only write, and
/// is booked as Core counts it: a full flush, `ALWAYS` (Core flushes the clean cache at its best
/// block and sets `full_flush_completed`).
// Core: validation.cpp:3138-3194 at the pin
#[cfg(feature = "test-support")]
#[test]
fn the_always_shortcut_is_booked_full() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    let tally = tally(&mut ctl);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(s.signals.tips(), [block(1), block(1)], "the shortcut");
    let want = CauseCounts {
        full: by_cause(&[("ALWAYS", 1)]),
        index_only: by_cause(&[]),
    };
    assert_eq!((tally.since_open(), tally.delta()), (want.clone(), want));
}

/// M-5: ALWAYS over dirty coins freezes, and is booked full, `ALWAYS`, once released.
#[cfg(feature = "test-support")]
#[test]
fn a_dirty_always_is_booked_full() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(v.evo.takes, 1, "a freeze");
    assert_eq!(tally.since_open().full, by_cause(&[("ALWAYS", 1)]));
    assert!(tally.since_open().index_only.is_empty());
}

/// O2b-4 §2.2 item 3: a job's stamp is read with its frozen data, so a job frozen in one open
/// and released after the next load keeps the freeze's generation and books in the release's.
#[cfg(feature = "test-support")]
#[test]
fn a_job_is_stamped_at_its_freeze_and_booked_at_its_release() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    tally.opened();
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    assert!(ctl.in_flight(), "a job not waited for");
    tally.opened();
    v.scripted = false;
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    let journal = tally.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].stamp.generation, 1, "the freeze's open");
    assert_eq!(journal[0].booked_in_generation, 2, "the release's");
}

/// W-B5's seam: a held release is not taken at the service points that follow, only at V's next
/// wait for W, here a settle's; the job stays in flight until then, and its release then books
/// and signals as an unheld one does.
#[cfg(feature = "test-support")]
#[test]
fn a_held_release_waits_for_the_next_wait() {
    let run = |hold: bool| {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut v = s.v();
        if hold {
            ctl.hold_next_release();
        }
        v.scripted = true;
        v.connect(0, 10, 1);
        v.site_and_step(&mut ctl, block(1)).unwrap();
        v.scripted = false;
        run_w(&mut ctl);
        v.with_cx(|cx| ctl.poll(cx)).unwrap();
        v.with_cx(|cx| ctl.service(cx)).unwrap();
        assert_eq!(ctl.in_flight(), hold, "held past the service points");
        assert_eq!(tally.journal().len(), usize::from(!hold), "booked at the release");
        v.with_cx(|cx| ctl.settle(cx)).unwrap();
        assert!(!ctl.in_flight());
        assert_eq!(tally.journal().len(), 1, "booked once");
        s.signals.tips()
    };
    assert_eq!(run(true), run(false), "the held release signals as the unheld one");
}

/// OD-L3-118's W schedules. A decision that meets a job in flight keeps its cause; under `Drain`
/// V first takes the job's release, so the decision meets none and freezes. Under `Hold` every
/// release, not only the next, is held past the service points until V waits for W.
#[cfg(feature = "test-support")]
#[test]
fn the_w_schedules_fix_when_v_takes_releases() {
    let kept = |schedule| {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        ctl.set_w_schedule(schedule);
        let mut v = s.v();
        v.scripted = true;
        v.connect(0, 10, 1);
        v.site_and_step(&mut ctl, block(1)).unwrap();
        v.connect(10, 10, 2);
        let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
        matches!(pending.action(), FlushAction::Pending { .. })
    };
    assert!(kept(None), "the release is not yet taken: the cause is kept");
    assert!(!kept(Some(WSchedule::Drain)), "the release is taken first: no job in flight");

    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    ctl.set_w_schedule(Some(WSchedule::Hold));
    let mut v = s.v();
    for (n, height) in [(1, 1), (2, 2)] {
        v.scripted = true;
        v.connect(10 * (height - 1), 10, height);
        v.site_and_step(&mut ctl, block(height)).unwrap();
        v.scripted = false;
        run_w(&mut ctl);
        v.with_cx(|cx| ctl.poll(cx)).unwrap();
        v.with_cx(|cx| ctl.service(cx)).unwrap();
        assert!(ctl.in_flight(), "release {n} held past the service points");
        assert_eq!(tally.journal().len(), n - 1, "release {n} not yet booked");
        v.with_cx(|cx| ctl.settle(cx)).unwrap();
        assert!(!ctl.in_flight());
        assert_eq!(tally.journal().len(), n, "release {n} booked at the settle");
    }
}

/// W-B4's seam: the next decision that would write nothing writes the block index alone under
/// the forced cause, with no `ChainStateFlushed`; one decision only.
#[cfg(feature = "test-support")]
#[test]
fn a_forced_index_only_write_takes_the_next_empty_decision() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    let tally = tally(&mut ctl);
    let signals = s.signals.tips().len();
    ctl.force_index_only(FlushCauses::PERIODIC_FLUSH);
    v.with_cx(|cx| ctl.flush(FlushStateMode::None, cx)).unwrap();
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    v.with_cx(|cx| ctl.flush(FlushStateMode::None, cx)).unwrap();
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    let want = CauseCounts {
        full: by_cause(&[]),
        index_only: by_cause(&[("PERIODIC_FLUSH", 1)]),
    };
    assert_eq!(tally.delta(), want, "one write, index-only, under the forced cause");
    assert_eq!(s.signals.tips().len(), signals, "no ChainStateFlushed");
}

/// A forced decision dropped unrun takes its mark with it: the next job, an unrelated ALWAYS
/// flush, is not booked forced (OD-L3-142 B).
#[cfg(feature = "test-support")]
#[test]
fn a_dropped_forced_decision_marks_no_later_job() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    let tally = tally(&mut ctl);
    ctl.force_index_only(FlushCauses::PERIODIC_FLUSH);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::None, cx)).unwrap();
    assert!(matches!(pending.action(), FlushAction::IndexOnly { .. }), "the forced decision");
    drop(pending);
    v.connect(10, 10, 2);
    v.step(block(2));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    let journal = tally.journal();
    assert_eq!(journal.len(), 1, "{journal:?}");
    assert!(journal[0].full, "the ALWAYS flush writes the coins: {journal:?}");
    assert!(!journal[0].forced, "an unrelated job is not forced: {journal:?}");
}

/// M-5: with no best block, Core's full flush writes the block index alone and leaves
/// `full_flush_completed` unset: booked index-only, by its causes, `ALWAYS`. The periodic write
/// alone is index-only `PERIODIC_WRITE`.
// Core: validation.cpp:3138-3194 at the pin
#[cfg(feature = "test-support")]
#[test]
fn no_best_block_and_the_periodic_write_are_booked_index_only() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    let mut v = s.v();
    assert_eq!(v.coins.best_block(), None);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    v.now += DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    let want = CauseCounts {
        full: by_cause(&[]),
        index_only: by_cause(&[("ALWAYS", 1), ("PERIODIC_WRITE", 1)]),
    };
    assert_eq!(tally.since_open(), want);
}

/// M-5: a job is booked when W releases it, so one W never released, failed or crashed, books
/// nothing.
#[cfg(feature = "test-support")]
#[test]
fn a_failed_job_books_nothing() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    s.db.inner().crash_at_write(1);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(tally.since_open(), CauseCounts::default());
}

/// M-5: causes kept for the job in flight freeze after its release, unwaited; that job is booked
/// at its own release, its causes joined in `FlushCauses`' bit order (Core's `v:3184-3189`).
#[cfg(feature = "test-support")]
#[test]
fn combined_causes_are_booked_in_order_at_a_later_release() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    // A share no layer nears: PERIODIC finds no LARGE cache.
    let mut v = s.v_with(1 << 34);
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.now += DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
    assert_eq!(ctl.pending(), FlushCauses::SCRIPTED | FlushCauses::PERIODIC_FLUSH);
    v.scripted = false;
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(ctl.in_flight(), "the kept causes froze");
    assert_eq!(tally.since_open().full, by_cause(&[("SCRIPTED", 1)]), "booked at its release");
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(!ctl.in_flight());
    let want = by_cause(&[("PERIODIC_FLUSH|SCRIPTED", 1), ("SCRIPTED", 1)]);
    assert_eq!(tally.since_open().full, want);
}

/// M-5 (Opus r1 n-1): one PERIODIC decision with two of Core's causes, a large cache past
/// `DATABASE_FLUSH_INTERVAL`, is one full flush booked under both, joined in bit order. The
/// first PERIODIC, over the same cache, starts the timers (`v:3039-3045` at v24.0.0) with
/// `CACHE_LARGE` alone.
// Core: validation.cpp:3180-3194 at the pin
#[cfg(feature = "test-support")]
#[test]
fn one_periodic_decision_books_two_core_causes_joined() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    // A share under the cache's own allocations: PERIODIC finds it LARGE (CRITICAL, Σ > B).
    let mut v = s.v_with(1);
    v.connect(0, 10, 1);
    v.step(block(1));
    let mut periodic = |v: &mut V| {
        v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
        run_w(&mut ctl);
        v.with_cx(|cx| ctl.poll(cx)).unwrap();
        assert!(!ctl.in_flight());
    };
    periodic(&mut v);
    assert_eq!(tally.since_open().full, by_cause(&[("CACHE_LARGE", 1)]));
    v.now += DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
    periodic(&mut v);
    let want = CauseCounts {
        full: by_cause(&[("CACHE_LARGE", 1), ("CACHE_LARGE|PERIODIC_FLUSH", 1)]),
        index_only: by_cause(&[]),
    };
    assert_eq!(tally.since_open(), want);
}

/// A FLUSH op's label replaces the causes of the jobs it submits, and goes with the job: one
/// released after the op returned is booked under it too.
// Core: src/test/oracle/oracle_chain.cpp:497-505, :1009-1027 at the pin
#[cfg(feature = "test-support")]
#[test]
fn a_label_goes_with_its_job_to_the_release() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    ctl.set_label(Some("mirror".into()));
    v.site_and_step(&mut ctl, block(1)).unwrap();
    ctl.set_label(None);
    assert!(ctl.in_flight());
    assert_eq!(tally.since_open(), CauseCounts::default(), "not before its release");
    v.scripted = false;
    v.with_cx(|cx| ctl.settle(cx)).unwrap();
    assert_eq!(tally.since_open().full, by_cause(&[("mirror", 1)]));
    v.connect(10, 10, 2);
    v.step(block(2));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(tally.since_open().full, by_cause(&[("ALWAYS", 1), ("mirror", 1)]), "unlabelled");
}

/// D14: under `-dbcrashratio` the wait for every freeze is inside `freeze_after_step`.
#[test]
fn dbcrashratio_waits_inside_freeze_after_step() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), true);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 10, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(s.meta().tip, Some(block(1).hash));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1)]);
}

/// D14, O-R6-M1's scenario (§4.6). `feature_dbcrash` submits one block at a time, and when a
/// submission fails because the node crashed it waits for that block after the restart
/// (`feature_dbcrash.py:76-103, 147-151`): recovery must land on it, and its index entry must be
/// durable. With the hook, the roll comes up inside the submission of the block being flushed,
/// after B1 wrote the marker and the index through it. Without it, V submits the next block while
/// W rolls the crash on the first flush, so the failed submission is a later block's, which has
/// no durable index entry.
#[test]
fn dbcrashratio_needs_the_synchronous_freeze() {
    for synchronous in [false, true] {
        let s = Stores::new(2 << 10);
        let mut ctl = s.inline(CrashRoll::seeded(1, 3), synchronous);
        let mut v = s.v();
        v.scripted = true;
        let failed = (1..=3)
            .find(|&h| {
                v.connect(h * 1000, 300, h);
                let failed = v.site_and_step(&mut ctl, block(h)).is_err();
                // W runs beside V: on to its next B1, or until it stops.
                while ctl.driver().step().is_some_and(|step| step != FlushStep::Durable) {}
                failed
            })
            .map(block)
            .expect("the roll comes up");
        let lands = s.meta().head_blocks.map(|(new, _)| new) == Some(failed.hash);
        let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let indexed = s.db.inner().unprobed().get(cf, failed.hash.as_bytes()).unwrap().is_some();
        assert_eq!((lands, indexed), (synchronous, synchronous), "synchronous: {synchronous}");
    }
}

/// Core sets `m_last_write` at each block-index write (`validation.cpp:3041, 3086` at v24.0.0): V
/// restarts the timer at the decision that writes, so PERIODIC writes the index an hour after the
/// last write, not at every call an hour after the first.
#[test]
fn the_periodic_write_timer_restarts_at_each_write() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    let (start, second) = (v.now, Duration::from_secs(1));
    for (after, writes) in [
        (Duration::ZERO, false),
        (DATABASE_WRITE_INTERVAL + second, true),
        (DATABASE_WRITE_INTERVAL + 2 * second, false),
        (2 * DATABASE_WRITE_INTERVAL + second, false),
        (2 * DATABASE_WRITE_INTERVAL + 2 * second, true),
    ] {
        v.now = start + after;
        v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
        assert_eq!(ctl.in_flight(), writes, "{after:?} after the first call");
        run_w(&mut ctl);
        v.with_cx(|cx| ctl.poll(cx)).unwrap();
        assert!(!ctl.in_flight());
    }
}

/// BL-818 (CH3b D243): a node-own job's release moves no timer. Core makes no write at a SCRIPTED
/// flush, so its hourly write is due an hour after the first call, not after the job.
#[test]
fn a_node_own_release_moves_no_timer() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    let start = v.now;
    v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
    assert!(!ctl.in_flight(), "no write right after startup");
    v.now = start + Duration::from_secs(59 * 60);
    v.scripted = true;
    v.with_cx(|cx| ctl.flush(FlushStateMode::None, cx)).unwrap();
    v.scripted = false;
    assert!(ctl.in_flight(), "the node-own job");
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(!ctl.in_flight(), "released");
    v.now = start + DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
    assert!(ctl.in_flight(), "Core's write is due an hour after its first call");
}

/// The ALWAYS shortcut needs the tip unmoved (§4.2). A block whose outputs are all unspendable and
/// which spends nothing leaves active clean; ALWAYS at it still writes its best block, as Core's
/// full flush does.
#[test]
fn always_after_a_clean_block_flushes_its_tip() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    v.connect(10, 0, 2);
    v.step(block(2));
    assert_eq!(v.coins.active().dirty_len(), 0);
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!((s.meta().tip, v.coins.durable()), (Some(block(2).hash), Some(block(2))));
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
}

/// Core logs "Cache size (%s) exceeds total space (%s)" at each check that finds CRITICAL, and
/// only there (`validation.cpp:2971-2972` at v24.0.0); V logs it with native Σ and B (CH2 §3.3,
/// §3.6). NONE freezes nothing, so V decides at every boundary on the way up: with f = 19/20, LARGE
/// is not hidden by THRESHOLD (f·B or a shard's trigger).
#[test]
fn a_critical_check_logs_cores_line() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v_budget(CoinsBudget::new(256 << 10, 0, Ratio::new(19, 20), 0));
    let mut seen = Vec::new();
    let line = (0..)
        .find_map(|i| {
            let boundary = v.coins.boundary_state(0, false);
            if !seen.contains(&boundary) {
                seen.push(boundary);
            }
            let (usage, total) = (v.coins.usage().total(), v.coins.budget().total(0));
            v.with_cx(|cx| ctl.flush(FlushStateMode::None, cx)).unwrap();
            if boundary == CoinsBoundary::Critical {
                assert!(usage > total);
                return Some(format!("Cache size ({usage}) exceeds total space ({total})"));
            }
            assert!(s.obs.lines(LogCat::Always).is_empty(), "{boundary:?}");
            v.connect(i, 1, 1);
            None
        })
        .unwrap();
    seen.sort_by_key(|b| *b as u8);
    assert_eq!(seen, {
        use CoinsBoundary::{Critical, Large, Ok, Threshold};
        [Ok, Threshold, Large, Critical]
    });
    assert_eq!(s.obs.lines(LogCat::Always), std::slice::from_ref(&line));
    // IF_NEEDED's check at the same Σ logs it again, as each of Core's does.
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(
        pending.action(),
        FlushAction::Freeze { causes, .. } if causes.contains(FlushCauses::CACHE_CRITICAL)
    ));
    let seq = v.step(block(1));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(1), seq)).unwrap();
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)).unwrap();
    assert_eq!(s.obs.lines(LogCat::Always), [line.clone(), line], "below B, nothing more");
}

/// §4.6: a failed write reaches the abort sink once, from the driver, with Core's text; V's wait
/// returns the latched failure, signals nothing, and every later call stops at once.
#[test]
fn a_failed_flush_is_reported_once_and_v_stops() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    s.db.inner().crash_at_write(1);
    let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(err.0.site, "chainstate write");
    let marks = s.sink.marks();
    assert!(
        matches!(&marks[..], [AbortMark::AbortNode { log, user: None }]
            if log.starts_with("System error while flushing: ")),
        "{marks:?}"
    );
    assert!(s.signals.tips().is_empty());
    assert_eq!(v.with_cx(|cx| ctl.flush(FlushStateMode::Periodic, cx)), Err(err));
    assert_eq!(s.sink.marks().len(), 1);
}

/// §4.6: the crash roll stops W after a partial batch, before Bk; V stops with it, and nothing is
/// reported or signalled.
#[test]
fn a_crash_roll_stops_w_and_v() {
    let s = Stores::new(2 << 10);
    let mut ctl = s.inline(CrashRoll::seeded(1, 3), false);
    let mut v = s.v();
    v.connect(0, 300, 1);
    v.step(block(1));
    let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(err.0.site, "dbcrashratio");
    assert_eq!(ctl.driver().step(), None);
    assert!(s.sink.marks().is_empty() && s.signals.tips().is_empty());
    let meta = s.meta();
    assert_eq!((meta.tip, meta.head_blocks), (None, Some((block(1).hash, None))));
    // The crash latched: every later call stops at once, and nothing more is written.
    assert_eq!(s.db.latch().tripped(), Some(&err.0));
    assert_eq!(v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)), Err(err));
    assert!(ctl.in_flight(), "W never released");
}

/// An observer that records the thread of each `FlushBegin`.
#[derive(Default)]
struct Threads(Mutex<Vec<Option<String>>>);

impl FlushObserver for Threads {
    fn mark(&self, m: ObsMark) {
        if matches!(m, ObsMark::FlushBegin { .. }) {
            self.0.lock().unwrap().push(std::thread::current().name().map(Into::into));
        }
    }
}

fn w_thread<F: FlatFs + 'static>(
    s: &Stores<F>,
    obs: Arc<dyn FlushObserver>,
    crash: CrashRoll,
    on_crash: Box<dyn FnOnce() + Send>,
) -> ThreadDriver<()> {
    ThreadDriver::spawn(WThread {
        engine: s.db.clone(),
        files: s.files.clone(),
        fs: s.fs.clone(),
        hub: s.hub.clone(),
        obs,
        tuning: s.tuning.clone(),
        cfg: s.cfg.clone(),
        covered_seq: 0,
        crash,
        on_crash,
        after_release: None,
    })
    .unwrap()
}

/// Production's driver: W's steps run on its own thread, and V's waits end at its release.
#[test]
fn the_thread_driver_flushes_on_w() {
    let s = Stores::new(2 << 10);
    let threads = Arc::new(Threads::default());
    let driver = w_thread(&s, threads.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 300, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.scripted = false;
    v.connect(300, 10, 2);
    v.step(block(2));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(s.meta().tip, Some(block(2).hash));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
    let names = threads.0.lock().unwrap().clone();
    assert_eq!(names, vec![Some("dashd-flush".to_string()); 2]);
    drop(ctl);
}

/// A file system whose `sync_data` blocks until the test opens its gate.
struct GateFs {
    os: OsFs,
    entered: AtomicBool,
    open: Mutex<bool>,
    cv: Condvar,
}

impl GateFs {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }
}

impl FlatFs for GateFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        self.entered.store(true, Ordering::SeqCst);
        drop(self.cv.wait_while(self.open.lock().unwrap(), |open| !*open).unwrap());
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        self.os.available_space(dir)
    }
}

/// E-b1.7: V waiting for W wakes when another store trips the latch, though W is still inside
/// its files barrier; W then stops at its next step without a second `AbortNode`.
#[test]
fn a_trip_elsewhere_wakes_v_waiting_for_w() {
    let gate = GateFs {
        os: OsFs::new(),
        entered: AtomicBool::new(false),
        open: Mutex::new(true),
        cv: Condvar::new(),
    };
    let s = Stores::over(gate, 1 << 20);
    s.save_block(1);
    *s.fs.open.lock().unwrap() = false;
    s.fs.entered.store(false, Ordering::SeqCst);
    let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    let failure = Failure {
        kind: ErrorKind::Io,
        site: "elsewhere write",
        message: "another store failed".into(),
    };
    let (returned, watched) = std::sync::mpsc::channel::<()>();
    let (err, gate_closed, watchdog) = std::thread::scope(|scope| {
        scope.spawn(|| {
            while !s.fs.entered.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            std::thread::sleep(Duration::from_millis(20));
            s.db.latch().trip(failure.clone());
        });
        // A lost wake would leave V parked: the watchdog then opens the gate, so the test fails
        // rather than hangs.
        let fs = &s.fs;
        let watchdog = scope.spawn(move || {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test thread; LO-9 governs node threads (CH1-node §8.3)"
            )]
            let fired = watched.recv_timeout(Duration::from_secs(120)).is_err();
            if fired {
                fs.open();
            }
            fired
        });
        let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
        let gate_closed = !*s.fs.open.lock().unwrap();
        returned.send(()).unwrap();
        (err, gate_closed, watchdog.join().unwrap())
    });
    assert!(!watchdog && gate_closed, "V returned while W was still inside the barrier");
    assert_eq!(err, Fatal(failure));
    assert!(ctl.in_flight(), "W never released");
    s.fs.open();
    drop(ctl);
    assert!(s.sink.marks().is_empty(), "the failure was reported where it happened");
    assert_eq!(s.meta().tip, None);
}

/// §4.6: the crash roll runs ND1's exit on W; when it returns (tests), W stops and V with it.
#[test]
fn a_crash_roll_on_w_runs_the_exit() {
    let s = Stores::new(2 << 10);
    let exited = Arc::new(Mutex::new(None));
    let (on_w, latch) = (exited.clone(), s.db.latch().clone());
    let exit = Box::new(move || {
        let w = std::thread::current().name().map(String::from);
        *on_w.lock().unwrap() = Some((w, latch.tripped().is_none()));
    });
    let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(1, 3), exit);
    let mut ctl = s.control(driver, false);
    let mut v = s.v();
    v.connect(0, 300, 1);
    v.step(block(1));
    let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(err.0.site, "dbcrashratio");
    let exited = exited.lock().unwrap().take();
    assert_eq!(exited, Some((Some("dashd-flush".into()), true)), "the exit ran first, on W");
    drop(ctl);
    assert_eq!(s.meta().head_blocks, Some((block(1).hash, None)));
}

/// An observer that trips the latch, as another store's failure would, when the flush logs `line`.
/// The trip is made on a thread of its own, as that store's would be: on the flush's thread it
/// would be the flush's own trip, deferred to its report (BL-554).
struct TripOn {
    line: &'static str,
    latch: GlobalFailStop,
    failure: Failure,
    marks: RecordingObserver,
}

impl FlushObserver for TripOn {
    fn mark(&self, m: ObsMark) {
        if matches!(&m, ObsMark::Log { line, .. } if line == self.line) {
            std::thread::scope(|elsewhere| {
                elsewhere.spawn(|| self.latch.trip(self.failure.clone()));
            });
        }
        self.marks.mark(m);
    }
}

/// Review M1: a failure another store latches between W's check and its store call refuses the
/// call; W stops without marking a failure of its own or a second `AbortNode`.
#[test]
fn a_refused_store_call_is_not_reported_again() {
    let s = Stores::new(1 << 20);
    let failure = Failure {
        kind: ErrorKind::Io,
        site: "elsewhere write",
        message: "another store failed".into(),
    };
    let obs = TripOn {
        line: "FlushStateToDisk: write block index to disk started",
        latch: s.db.latch().clone(),
        failure: failure.clone(),
        marks: RecordingObserver::default(),
    };
    let mut ctl = s.control(InlineDriver::new(s.env_with(&obs, CrashRoll::seeded(0, 0))), false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(err, Fatal(failure));
    assert!(s.sink.marks().is_empty(), "{:?}", s.sink.marks());
    let marks = obs.marks.marks();
    assert!(marks.iter().any(|m| matches!(m, ObsMark::FlushBegin { .. })));
    assert!(
        !marks.iter().any(|m| matches!(m, ObsMark::StoreFailed { .. } | ObsMark::Abort { .. })),
        "{marks:?}"
    );
    assert_eq!(s.meta().tip, None);
}

/// An observer that panics on W's thread at a flush's begin.
struct PanicOnW;

impl FlushObserver for PanicOnW {
    fn mark(&self, m: ObsMark) {
        if matches!(m, ObsMark::FlushBegin { .. }) {
            panic!("a panic on W");
        }
    }
}

/// Review M2: a panic on W is a failure like any other. It trips the latch and is reported, so V's
/// wait returns, and dropping the driver re-raises it.
#[test]
fn a_panic_on_w_stops_v_and_is_reported() {
    let s = Stores::new(1 << 20);
    let driver = w_thread(&s, Arc::new(PanicOnW), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
    assert_eq!(err.0.message, "the chainstate flush thread panicked");
    assert_eq!(err.0.site, crate::store::sites::W_PANIC_SITE);
    assert_eq!(v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)), Err(err));
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(ctl)));
    assert!(dropped.is_err(), "the driver re-raises W's panic");
    // V returns once the latch trips (E-b1.7); W reports after tripping it, so the report is
    // checked once W's thread has ended.
    assert!(matches!(
        &s.sink.marks()[..],
        [AbortMark::AbortNode { log, user: None }] if log == "the chainstate flush thread panicked"
    ));
}

/// BL-554's checkpoint, on a thread of its own while `reader` waits: W has entered its held
/// report and `reader` is parked on it; then the release. Whether both were seen.
fn released_after_checkpoint(
    s: &Stores<OsFs>,
    release: crate::store::held::Release<'_>,
    reader: std::thread::ThreadId,
) -> bool {
    let parked = s.held.parked(s.db.latch(), reader).is_some();
    release.release();
    parked
}

/// BL-554 (Sol r1 M-5, CH3b D222): V's panic stop that finds W's failure latched, its report
/// still pending, returns only after that report, so V releases nothing ahead of it.
#[test]
fn a_panic_stop_that_finds_ws_failure_returns_after_ws_report() {
    let s = Stores::new(1 << 20);
    let ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let v = std::thread::current().id();
    let (first, heard, parked) = std::thread::scope(|scope| {
        let release = s.held.arm();
        let s = &s;
        let failure = Failure {
            kind: ErrorKind::Io,
            site: "chainstate write",
            message: "W's write failed".into(),
        };
        let w = fail_on_w(s.db.latch(), s.held.clone(), failure, "W's report".into());
        let checker = scope.spawn(move || released_after_checkpoint(s, release, v));
        let first = ctl.trip_latch(s.db.latch(), "the chain actor panicked");
        let heard = s.sink.marks();
        let parked = checker.join().unwrap();
        #[expect(clippy::disallowed_methods, reason = "a test thread; LO-9 governs node threads")]
        let reported = w.join();
        reported.unwrap();
        (first, heard, parked)
    });
    assert!(parked, "the panic stop waits for W's report");
    assert!(first, "W's failure latched first");
    let report = AbortMark::AbortNode {
        log: "W's report".into(),
        user: None,
    };
    assert_eq!(heard, [report], "W's report comes before the stop returns");
}

/// An observer that panics on W's thread at a store failure's mark, after the guard has latched it.
struct PanicAfterTrip;

impl FlushObserver for PanicAfterTrip {
    fn mark(&self, m: ObsMark) {
        if matches!(m, ObsMark::StoreFailed { .. }) {
            panic!("a panic on W after its trip");
        }
    }
}

/// V's ALWAYS flush on `driver`, W's report held until V waits for it: V's error, and whether
/// the checkpoint was seen.
fn always_flush_with_ws_report_held<R: FlushDriver<Delta = ()>>(
    s: &Stores<OsFs>,
    ctl: &mut Ctl<R>,
) -> (Fatal, bool) {
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    let me = std::thread::current().id();
    std::thread::scope(|scope| {
        let release = s.held.arm();
        let checker = scope.spawn(move || released_after_checkpoint(s, release, me));
        let err = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err();
        (err, checker.join().unwrap())
    })
}

/// BL-554 (Sol r2 M-1, CH3b D222): W panicking after its guard latched a failed write, before
/// the driver's report, reports the latched failure's text before V sees it; W's end then
/// reports nothing more.
#[test]
fn a_panic_on_w_after_its_trip_reports_the_latched_failure_first() {
    let s = Stores::new(1 << 20);
    let driver = w_thread(&s, Arc::new(PanicAfterTrip), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    s.db.inner().crash_at_write(1);
    let (err, parked) = always_flush_with_ws_report_held(&s, &mut ctl);
    assert!(parked, "V waits for W's report");
    let latched = s.db.latch().tripped().unwrap().clone();
    assert_eq!(err, Fatal(latched.clone()));
    assert_eq!(latched.site, "chainstate write");
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(ctl)));
    assert!(dropped.is_err(), "the driver re-raises W's panic");
    let report = AbortMark::AbortNode {
        log: crate::flush::abort_text(latched),
        user: None,
    };
    assert_eq!(s.sink.marks(), [report], "one report");
}

/// BL-554 (Sol r2 M-1, CH3b D222): W panicking before any trip: its end latches the panic and
/// reports it before V sees it.
#[test]
fn a_panic_on_w_before_any_trip_is_reported_before_v_sees_it() {
    let s = Stores::new(1 << 20);
    let driver = w_thread(&s, Arc::new(PanicOnW), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let (err, parked) = always_flush_with_ws_report_held(&s, &mut ctl);
    assert!(parked, "V waits for W's report");
    let panicked = "the chainstate flush thread panicked";
    assert_eq!(err.0.message, panicked);
    assert_eq!(err.0.site, crate::store::sites::W_PANIC_SITE);
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(ctl)));
    assert!(dropped.is_err(), "the driver re-raises W's panic");
    let report = AbortMark::AbortNode {
        log: panicked.into(),
        user: None,
    };
    assert_eq!(s.sink.marks(), [report], "one report");
}

/// BL-554 (Sol r2 M-1, r3, CH3b D222): the inline driver's step panicking after its guard
/// latched a failed write reports the latched failure's text before another thread sees it (the
/// inline owner, V and W at once, never waits on itself); then the panic goes on.
#[test]
fn an_inline_panic_after_its_trip_is_reported_before_another_reader_sees_it() {
    let s = Stores::new(1 << 20);
    s.db.inner().crash_at_write(1);
    let (parked, heard, unwound) = std::thread::scope(|scope| {
        let release = s.held.arm();
        let s = &s;
        let inline = std::thread::Builder::new()
            .name(W_THREAD.into())
            .spawn_scoped(scope, move || {
                let obs = PanicAfterTrip;
                let env = s.env_with(&obs, CrashRoll::seeded(0, 0));
                let mut ctl = s.control(InlineDriver::new(env), false);
                let mut v = s.v();
                v.connect(0, 10, 1);
                v.step(block(1));
                let _ = v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx));
            })
            .unwrap();
        let entered = s.held.wait_entered().is_some();
        let reader = scope.spawn(move || {
            s.db.latch().settle();
            s.sink.marks()
        });
        let id = reader.thread().id();
        let parked = entered && until(|| s.db.latch().pending_waiters().contains(&id));
        release.release();
        let heard = reader.join().unwrap();
        (parked, heard, inline.join().is_err())
    });
    assert!(parked, "the reader waits for the inline step's report");
    assert!(unwound, "the panic goes on after the report");
    let latched = s.db.latch().tripped().unwrap().clone();
    let report = AbortMark::AbortNode {
        log: crate::flush::abort_text(latched),
        user: None,
    };
    assert_eq!(heard, [report], "the report comes before the reader returns");
}

/// An observer that trips the latch on the thread that logs `line`: W's own trip, inside its
/// step, which no store call of the step reports.
struct TripHere {
    line: &'static str,
    latch: GlobalFailStop,
    failure: Failure,
}

impl FlushObserver for TripHere {
    fn mark(&self, m: ObsMark) {
        if matches!(&m, ObsMark::Log { line, .. } if line == self.line) {
            self.latch.trip(self.failure.clone());
        }
    }
}

/// BL-554 (Sol r3 m-1, CH3b D222): a step that wins a trip and returns without a report (no step
/// does) is reported by the driver with the latched failure's text before V sees it, and only
/// then fails W's debug assertion; W's end reports nothing more.
#[cfg(debug_assertions)]
#[test]
fn a_step_that_wins_a_trip_and_returns_is_reported_then_flagged() {
    let s = Stores::new(1 << 20);
    let failure = Failure {
        kind: ErrorKind::Io,
        site: "elsewhere write",
        message: "a trip on W's thread".into(),
    };
    let obs = TripHere {
        line: "FlushStateToDisk: write block index to disk started",
        latch: s.db.latch().clone(),
        failure: failure.clone(),
    };
    let driver = w_thread(&s, Arc::new(obs), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let (err, parked) = always_flush_with_ws_report_held(&s, &mut ctl);
    assert!(parked, "V waits for W's report");
    assert_eq!(err, Fatal(failure.clone()));
    let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(ctl)));
    assert!(dropped.is_err(), "W's debug assertion fails after the report");
    let report = AbortMark::AbortNode {
        log: failure.message,
        user: None,
    };
    assert_eq!(s.sink.marks(), [report], "one report");
}

/// The OS's files with W held at its space check until `open`, then its syncs failing once
/// `fail_sync` is set (BL-554's cycle).
#[derive(Default)]
struct CycleFs {
    os: OsFs,
    /// (W held at the space check, open).
    gate: Mutex<(bool, bool)>,
    cv: Condvar,
    fail_sync: AtomicBool,
}

impl CycleFs {
    fn wait_held(&self) -> bool {
        let gate = self.gate.lock().unwrap();
        let (gate, _) =
            self.cv.wait_timeout_while(gate, Duration::from_secs(10), |g| !g.0).unwrap();
        gate.0
    }

    fn open(&self) {
        self.gate.lock().unwrap().1 = true;
        self.cv.notify_all();
    }
}

impl FlatFs for CycleFs {
    fn link_count(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.link_count(path)
    }
    fn len(&self, path: &Path) -> io::Result<Option<u64>> {
        self.os.len(path)
    }
    fn create(&self, path: &Path) -> io::Result<()> {
        self.os.create(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FlatRead + '_>> {
        self.os.open_read(path)
    }
    fn read_exact_at(&self, path: &Path, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.os.read_exact_at(path, offset, buf)
    }
    fn write_all_at(&self, path: &Path, offset: u64, data: &[u8]) -> io::Result<()> {
        self.os.write_all_at(path, offset, data)
    }
    fn allocate(&self, path: &Path, offset: u64, len: u64) {
        self.os.allocate(path, offset, len);
    }
    fn truncate(&self, path: &Path, len: u64) -> io::Result<()> {
        self.os.truncate(path, len)
    }
    fn sync_data(&self, path: &Path) -> io::Result<()> {
        if self.fail_sync.load(Ordering::SeqCst) {
            return Err(io::Error::other("injected sync failure"));
        }
        self.os.sync_data(path)
    }
    fn sync_dir(&self, dir: &Path) -> io::Result<()> {
        self.os.sync_dir(dir)
    }
    fn copy(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.copy(from, to)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.os.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<bool> {
        self.os.remove(path)
    }
    fn create_dir_all(&self, dir: &Path) -> io::Result<()> {
        self.os.create_dir_all(dir)
    }
    fn available_space(&self, dir: &Path) -> io::Result<u64> {
        if std::thread::current().name() == Some("dashd-flush") {
            let mut gate = self.gate.lock().unwrap();
            gate.0 = true;
            self.cv.notify_all();
            drop(self.cv.wait_timeout_while(gate, Duration::from_secs(60), |g| !g.1).unwrap());
        }
        self.os.available_space(dir)
    }
}

/// The scripted feeds with a registration that takes the tip lock, as SB5's waits out another
/// subscriber's handoff (Sol r2 M-2): it says when it is about to.
struct TipRegister {
    feeds: crate::subscriber::ScriptedFeeds,
    registering: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl crate::subscriber::FeedTable for TipRegister {
    fn register(
        &self,
        id: crate::subscriber::SubscriberId,
        needs: dash_chain::Needs,
    ) -> Result<(), dash_chain::Refused> {
        if let Some(tx) = self.registering.lock().unwrap().take() {
            _ = tx.send(());
        }
        let _tip = self.feeds.hold_tip();
        self.feeds.register(id, needs)
    }
    fn deregister(&self, id: crate::subscriber::SubscriberId) {
        self.feeds.deregister(id);
    }
    fn on_release(&self, w: dash_chain::Notify) {
        self.feeds.on_release(w);
    }
    fn send_control(
        &self,
        id: crate::subscriber::SubscriberId,
        c: dash_chain::ControlMsg,
    ) -> dash_chain::SendControl {
        self.feeds.send_control(id, c)
    }
    fn is_closing(&self, id: crate::subscriber::SubscriberId) -> bool {
        self.feeds.is_closing(id)
    }
    fn synced(&self) -> Vec<crate::subscriber::SubscriberId> {
        self.feeds.synced()
    }
    fn set_close_hook(&self, w: dash_chain::Notify) {
        self.feeds.set_close_hook(w);
    }
    fn snapshot(&self) -> Box<dyn dash_chain::ChainSnapshot + '_> {
        self.feeds.snapshot()
    }
    fn subscribe_at(
        &self,
        id: crate::subscriber::SubscriberId,
        p: &BlockRef,
        finish: &mut dyn FnMut() -> Result<(), crate::subscriber::SubscriberError>,
    ) -> Result<dash_chain::Handoff, dash_chain::SubscribeError<crate::subscriber::SubscriberError>>
    {
        self.feeds.subscribe_at(id, p, finish)
    }
}

/// The child's flag for [`ws_report_breaks_the_tip_window_pending_cycle`].
const CYCLE_CHILD: &str = "BL554_CYCLE_CHILD";

/// BL-554 (Sol r2 M-2, CH3b D222): on one latch, a subscriber S in its handoff under the tip
/// lock, a registration R holding the hub's window and waiting for the tip lock, and W failing
/// at its Files step all finish. S's guarded WAL sync waits for W's report; W reports without
/// the window, so S returns its refusal and releases the tip, R registers and releases the
/// window, and W closes it. A report behind the window would close the cycle S → W → R → S,
/// which no thread can break: the scenario runs in a child process (this test, `--exact`, with
/// [`CYCLE_CHILD`] set) that the parent kills at its watchdog.
#[test]
fn ws_report_breaks_the_tip_window_pending_cycle() {
    if std::env::var_os(CYCLE_CHILD).is_some() {
        tip_window_pending_cycle();
        return;
    }
    let name = "flush::control::tests::ws_report_breaks_the_tip_window_pending_cycle";
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--test-threads=1"])
        .env(CYCLE_CHILD, "1")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let status = status.expect("the cycle's threads hung: the watchdog killed the child");
    assert!(status.success(), "the child failed: {status}");
}

/// [`ws_report_breaks_the_tip_window_pending_cycle`]'s scenario, in the child.
fn tip_window_pending_cycle() {
    let tree = crate::subscriber::TreeIndex::new();
    let g = tree.genesis();
    let main = std::iter::once(g).chain(tree.branch(g, 3)).collect::<Vec<_>>();
    let tip = *main.last().unwrap();
    let (registering, r_in_register) = std::sync::mpsc::channel();
    let table = Arc::new(TipRegister {
        feeds: crate::subscriber::ScriptedFeeds::new(4, main),
        registering: Mutex::new(None),
    });
    let mut s = Stores::over(CycleFs::default(), 1 << 20);
    s.hub = Arc::new(SubscriberHub::with_feeds(table.clone(), Arc::new(tree)));
    s.save_block(1);
    let latch = s.db.latch().clone();
    let reg = |name| crate::flush::Registration {
        name,
        needs: dash_chain::Needs::default(),
        mandatory: false,
        start_height: 1,
        durable: None,
    };
    let sub = s.hub.register(reg("s"), &latch).unwrap().unwrap();
    let watchdog = Duration::from_secs(30);
    let (v_err, synced, registered) = std::thread::scope(|scope| {
        let s = &s;
        let v = scope.spawn(move || {
            let driver = w_thread(s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            let mut v = s.v();
            v.connect(0, 10, 1);
            v.step(block(1));
            v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap_err()
        });
        assert!(s.fs.wait_held(), "V's job is at W's space check");
        let (in_handoff, s_in_handoff) = std::sync::mpsc::channel();
        let latch = &latch;
        let sub = scope.spawn(move || {
            let mut synced = None;
            let table = s.hub.table().unwrap();
            let _ = table.subscribe_at(sub.id, &tip, &mut || {
                in_handoff.send(()).unwrap();
                let start = Instant::now();
                while latch.tripped_now().is_none() && start.elapsed() < watchdog {
                    std::thread::sleep(Duration::from_millis(1));
                }
                synced = Some(s.db.sync_wal());
                Ok(())
            });
            synced
        });
        #[expect(clippy::disallowed_methods, reason = "a test thread; LO-9 governs node threads")]
        let in_handoff = s_in_handoff.recv_timeout(watchdog);
        in_handoff.expect("S is in its handoff");
        // Armed only now, past S's own registration: the event below is R's.
        *table.registering.lock().unwrap() = Some(registering);
        let r = scope.spawn(move || s.hub.register(reg("r"), latch).map(|r| r.is_ok()));
        #[expect(clippy::disallowed_methods, reason = "a test thread; LO-9 governs node threads")]
        let in_register = r_in_register.recv_timeout(watchdog);
        in_register.expect("R holds the window, at the tip lock");
        s.fs.fail_sync.store(true, Ordering::SeqCst);
        s.fs.open();
        let v_err = v.join().unwrap();
        (v_err, sub.join().unwrap(), r.join().unwrap())
    });
    assert_eq!(v_err.0.site, "block files write");
    let refused = synced.expect("S synced after W's failure").unwrap_err();
    assert_eq!(refused.kind(), Some(ErrorKind::ReadOnly), "S's sync is refused: {refused}");
    // R registers once the tip is free, or meets the failure while the window is open (r3 n-1).
    let _ = registered;
    assert_eq!(s.sink.marks().len(), 1, "{:?}", s.sink.marks());
}

/// E-b1.2's core case: CACHE_CRITICAL at an IF_NEEDED site with a job in flight waits inside
/// `at_site` for the release, freezes nothing before the step, and freezes the post-step pair.
#[test]
fn critical_at_a_site_waits_for_the_flight_before_the_step() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v_with(128 << 10);
    v.scripted = true;
    v.connect(0, 5, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.scripted = false;
    assert!(ctl.in_flight());
    v.connect(10, 3_000, 2);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(
        pending.action(),
        FlushAction::Freeze { causes, .. } if causes.contains(FlushCauses::CACHE_CRITICAL)
    ));
    assert!(!ctl.in_flight() && v.coins.frozen().is_none(), "the wait, and no freeze yet");
    assert_eq!(s.meta().tip, Some(block(1).hash));
    let seq = v.step(block(2));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(2), seq)).unwrap();
    // The frozen pair is above B, so the freeze was waited for (D32); W has nothing left.
    run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert_eq!(s.meta().tip, Some(block(2).hash));
    assert_eq!(s.hub.t_ack().map(|t| (t.tip, t.tip_seq)), Some((block(2), 2)));
    // Core v24.0.0: validation.cpp:3105, :3125 (the locator of the block the flush wrote)
    assert_eq!(s.signals.tips(), [block(1), block(2)]);
}

/// DEC-407: native Σ ≤ B(t) at every block boundary. Active grew past B with F in flight, so the
/// CRITICAL freeze after the wait still leaves F and the new active above B(t); V waits for that
/// release too, and the boundary is back under B(t) with nothing in flight.
#[test]
fn a_freeze_that_leaves_sigma_above_b_waits_for_its_release() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v_with(128 << 10);
    v.scripted = true;
    v.connect(0, 5, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.scripted = false;
    assert!(ctl.in_flight());
    v.connect(10, 3_000, 2);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    let seq = v.step(block(2));
    v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(2), seq)).unwrap();
    assert!(!ctl.in_flight() && v.coins.frozen().is_none(), "V waited for the second release");
    assert_eq!(s.meta().tip, Some(block(2).hash));
    let b = v.coins.budget().total(0);
    assert!(v.coins.usage().total() <= b, "Σ {} above B {b}", v.coins.usage().total());
}

/// D32's check counts R, the batch reserve a flight holds. The presize leaves R room, but not below
/// the tables' minimum: the same small freeze stays in flight with no reserve and is waited for
/// when R takes the frozen pair above B(t).
#[test]
fn the_reserve_counts_toward_the_wait_after_a_freeze() {
    for (reserve, waited) in [(0, false), (64 << 10, true)] {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut v = s.v_budget(CoinsBudget::new(128 << 10, 0, Ratio::HALF, reserve));
        v.scripted = true;
        v.connect(0, 5, 1);
        v.site_and_step(&mut ctl, block(1)).unwrap();
        let b = v.coins.budget().total(0);
        assert_eq!(ctl.in_flight(), !waited, "R = {reserve}");
        assert!(v.coins.usage().total() <= b, "R = {reserve}: {:?}", v.coins.usage());
    }
}

/// A port that records, with each signal, whether a block-index record was durable by then.
struct CheckingSignals {
    db: Arc<GuardedEngine<ProbeEngine>>,
    record: BlockHash,
    seen: Mutex<Vec<(BlockRef, bool)>>,
}

impl FlushSignals for CheckingSignals {
    fn chain_state_flushed(&self, tip: BlockRef) {
        let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let written = self.db.inner().unprobed().get(cf, self.record.as_bytes()).unwrap();
        self.seen.lock().unwrap().push((tip, written.is_some()));
    }
}

/// E-b1.4: the ALWAYS shortcut signals only after its index-only write has returned.
#[test]
fn the_always_shortcut_signals_after_its_write() {
    let s = Stores::new(1 << 20);
    let record = block(9).hash;
    let signals = Arc::new(CheckingSignals {
        db: s.db.clone(),
        record,
        seen: Mutex::default(),
    });
    let driver = InlineDriver::new(s.env(CrashRoll::seeded(0, 0)));
    let mut ctl = FlushControl::new(
        driver,
        FlushPolicy::new(false),
        signals.clone(),
        Arc::default(),
        s.obs.clone(),
    );
    let mut v = s.v();
    v.connect(0, 10, 1);
    v.step(block(1));
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    v.dirty.push(IndexWrite {
        hash: record,
        bytes: vec![9; 12],
    });
    v.with_cx(|cx| ctl.flush(FlushStateMode::Always, cx)).unwrap();
    assert_eq!(*signals.seen.lock().unwrap(), [(block(1), false), (block(1), true)]);
}

/// V's index in the prune tests: the files W asked it to clear, and a record for each, or the
/// failure it answers with.
#[derive(Default)]
struct ClearingIndex {
    asked: Vec<Vec<u32>>,
    fail: Option<String>,
    /// The height the clears' check reads, if not V's working tip's.
    check_height: Option<Option<u32>>,
    /// The automatic selection's IBD latches.
    latches: u32,
    /// V unwinds from `prune_clear`, before it records the ask.
    unwind_at_clear: bool,
}

impl FlushIndex for ClearingIndex {
    fn take_dirty_into(&mut self, _: &mut dyn FnMut(&BlockHash, &[u8])) {}

    fn clears_check_height(&mut self, working: u32) -> Option<u32> {
        self.check_height.unwrap_or(Some(working))
    }

    fn latch_ibd(&mut self) {
        self.latches += 1;
    }

    fn prune_clear(&mut self, files: &[u32]) -> Result<Vec<IndexWrite>, String> {
        assert!(!self.unwind_at_clear, "V unwinds inside its wait");
        self.asked.push(files.to_vec());
        if let Some(log) = &self.fail {
            return Err(log.clone());
        }
        Ok(files
            .iter()
            .map(|&f| IndexWrite {
                hash: block(1_000 + f).hash,
                bytes: vec![f as u8; 7],
            })
            .collect())
    }
}

/// The title of the first bench timer, which must be step 0's selector, and its one start: Core
/// times its selector before the flush writes anything.
fn prune_timer<F: FlatFs>(s: &Stores<F>) -> String {
    let bench = s.obs.lines(LogCat::Bench);
    let title = bench[0].strip_prefix("FlushStateToDisk: ").unwrap().strip_suffix(" started");
    let title = title.expect("a timer's start").to_owned();
    let starts = bench.iter().filter(|l| **l == bench[0]).count();
    assert!(bench[1].starts_with(&format!("FlushStateToDisk: {title} completed (")));
    assert_eq!(starts, 1, "{bench:?}");
    title
}

/// Core's NONE flush with `nManualPruneHeight` (`pruneblockchain`), or with none (an automatic
/// prune when the check is armed).
fn flush_pruning<R: FlushDriver<Delta = ()>, F: FlatFs>(
    ctl: &mut Ctl<R>,
    v: &mut V,
    host: &mut FilesPruneHost<F>,
    index: &mut ClearingIndex,
    manual: Option<u32>,
) -> Result<(), Fatal> {
    with_prune_cx(v, host, index, manual, |cx| ctl.flush(FlushStateMode::None, cx))
}

fn with_prune_cx<T>(
    v: &mut V,
    host: &mut dyn PruneHost,
    index: &mut ClearingIndex,
    manual: Option<u32>,
    f: impl FnOnce(&mut FlushCx<'_, ProbeEngine, ()>) -> T,
) -> T {
    let mut cx = FlushCx {
        now: v.now,
        coins: &mut v.coins,
        mempool_usage: 0,
        tip: v.tip,
        last_seq: v.seq,
        index,
        evo: &mut v.evo,
        prune: host,
        manual_prune_height: manual,
        reindex: false,
        is_ibd: false,
        left_ibd: false,
        scripted: false,
    };
    f(&mut cx)
}

/// `pruneblockchain` over each driver (CH2 §3.3, §4.2 step 7): V answers W's `PruneClears` inside
/// its wait, W writes the cleared records, and the call returns once the files are unlinked.
#[test]
fn v_answers_the_clears_in_its_wait_and_a_manual_prune_returns_unlinked() {
    let state = three_files();
    let tip = block(19 + KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, state.clone());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 0,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        };
        let mut index = ClearingIndex::default();
        if threaded {
            let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, Some(15)).unwrap();
            assert!(!ctl.in_flight());
        } else {
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, Some(15)).unwrap();
            assert!(!ctl.in_flight());
            assert_eq!(run_w(&mut ctl), [FlushStep::AfterBk], "the unlink came before the release");
        }
        assert_eq!(index.asked, [vec![0]], "threaded {threaded}");
        assert_eq!(prune_timer(&s), "find files to prune (manual)");
        let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let record = s.db.inner().unprobed().get(cf, block(1_000).hash.as_bytes()).unwrap();
        assert_eq!(record, Some(vec![0; 7]));
        let meta = s.meta();
        assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), true));
        let on_disk = |n: u32| blk_on_disk(&s.cfg.blocks_dir, n);
        assert_eq!((on_disk(0), on_disk(1)), (false, true), "threaded {threaded}");
    }
}

/// An automatic prune is waited for inside its call (CH3b D244), over each driver: V answers W's
/// `PruneClears` in its wait, W writes the clears, unlinks and releases, and nothing is in flight
/// when the call returns.
#[test]
fn v_answers_an_automatic_prunes_clears_inside_its_call() {
    let state = three_files();
    let tip = block(19 + KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, state.clone());
        let (mut v, mut host) = armed_prune_at(&s, tip);
        let mut index = ClearingIndex::default();
        if threaded {
            let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            assert!(!ctl.in_flight());
        } else {
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            assert!(!ctl.in_flight());
            assert_eq!(run_w(&mut ctl), [FlushStep::AfterBk], "the unlink came before the release");
        }
        assert_eq!(index.asked, [vec![0, 1]], "threaded {threaded}");
        assert_eq!(prune_timer(&s), "find files to prune");
        let meta = s.meta();
        assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), true));
        let on_disk = |n: u32| blk_on_disk(&s.cfg.blocks_dir, n);
        assert_eq!((on_disk(0), on_disk(1), on_disk(2)), (false, false, true), "{threaded}");
        assert_eq!(s.sink.marks(), []);
    }
}

/// V's call with no tip (b4 D55): Core's `m_chain.Height() == -1`.
fn without_tip<T>(
    v: &mut V,
    prune: &mut dyn PruneHost,
    f: impl FnOnce(&mut NoTipCx<'_, ProbeEngine, ()>) -> T,
) -> T {
    let mut cx = NoTipCx {
        now: v.now,
        coins: &mut v.coins,
        mempool_usage: 0,
        index: &mut v.dirty,
        evo: &mut v.evo,
        prune,
        reindex: false,
    };
    f(&mut cx)
}

/// b4 D55 (Opus r1 m-2): an ALWAYS flush with no tip is Core's at `Height() == -1`
/// (`v:3058-3079` at v24.0.0): the block files flushed and the block index written, no coins, no
/// `ChainStateFlushed`, and the call returns after the release.
#[test]
fn an_always_flush_with_no_tip_writes_the_files_and_the_index() {
    let s = Stores::new(1 << 20);
    s.save_block(0);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.dirty.push(IndexWrite {
        hash: block(0).hash,
        bytes: vec![7; 12],
    });
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Always, cx))
        .unwrap();
    assert!(!ctl.in_flight(), "ALWAYS waits for its release");
    let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    let written = s.db.inner().unprobed().get(cf, block(0).hash.as_bytes()).unwrap();
    assert_eq!(written, Some(vec![7; 12]));
    let bench = s.obs.lines(LogCat::Bench);
    let started = |title: &str| bench.contains(&format!("FlushStateToDisk: {title} started"));
    assert!(started("write block and undo data to disk"), "{bench:?}");
    assert!(started("write block index to disk"), "{bench:?}");
    assert_eq!(s.meta().tip, None);
    assert!(s.signals.tips().is_empty());
}

/// b4 D55: the no-tip ALWAYS write is an ALWAYS job, so with a live copy W syncs the engine
/// after its B-K writes (E-b1.8): a failed barrier there fails the call.
#[test]
fn an_always_flush_with_no_tip_syncs_a_live_copy() {
    let mut s = Stores::new(1 << 20);
    s.cfg.live_copy = true;
    s.db.inner().fail_barrier_at(1);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    let flushed =
        without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Always, cx));
    assert!(flushed.is_err(), "the engine's sync failed");
}

/// b4 D55: with no tip, PERIODIC writes the index once Core's write timer is due, and V's stop
/// waits for that write's release.
#[test]
fn a_periodic_write_with_no_tip_is_written_and_stop_waits_for_it() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    let start = v.now;
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Periodic, cx))
        .unwrap();
    assert!(!ctl.in_flight(), "no write right after startup");
    v.now = start + DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
    v.dirty.push(IndexWrite {
        hash: block(0).hash,
        bytes: vec![8; 12],
    });
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Periodic, cx))
        .unwrap();
    assert!(ctl.in_flight(), "PERIODIC does not wait");
    without_tip(&mut v, &mut NoPrune, |cx| ctl.stop_without_tip(cx)).unwrap();
    assert!(!ctl.in_flight());
    let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
    let written = s.db.inner().unprobed().get(cf, block(0).hash.as_bytes()).unwrap();
    assert_eq!(written, Some(vec![8; 12]));
    assert!(s.signals.tips().is_empty());
}

/// M-5: an ALWAYS with no tip (Core's `Height() == -1`) has no best block, so it is booked as
/// Core counts it: index-only, `ALWAYS`.
// Core: validation.cpp:3180-3194 at the pin
#[cfg(feature = "test-support")]
#[test]
fn an_always_flush_with_no_tip_is_booked_index_only() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    let mut v = s.v();
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Always, cx))
        .unwrap();
    let want = CauseCounts {
        full: by_cause(&[]),
        index_only: by_cause(&[("ALWAYS", 1)]),
    };
    assert_eq!(tally.since_open(), want);
}

/// b4 D55 (Opus r1 m-2): with no tip an armed automatic prune check is taken under its timer,
/// as Core's `FindFilesToPrune` returns at `chain_tip_height < 0` and `validation.cpp:3028` (at
/// v24.0.0) clears the flag; nothing is selected or written.
#[test]
fn an_armed_prune_check_with_no_tip_is_cleared_under_its_timer() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut v = s.v();
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    without_tip(&mut v, &mut host, |cx| ctl.flush_without_tip(FlushStateMode::IfNeeded, cx))
        .unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(prune_timer(&s), "find files to prune");
    assert_eq!(s.obs.lines(LogCat::Bench).len(), 2, "the selector's timer alone");
    assert!(!host.check_due(), "the check was cleared");
    assert!((0..3).all(|n| blk_on_disk(&s.cfg.blocks_dir, n)));
}

/// BL-818 (CH3b D243, D244): with no tip, the prune half reads the host's armed check, so a call
/// in flight takes it too; its peek selects nothing, and its take takes the host's check and
/// returns false.
#[test]
fn a_peek_takes_no_check() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut prune = NoTipPrune {
        host: &mut host,
        obs: &*s.obs,
    };
    assert!(prune.check_armed(), "the host's armed check");
    assert!(!prune.would_select(KEEP + 30, 30, false), "no tip: selects nothing");
    assert!(!prune.check_due(), "selects nothing");
    assert!(!host.check_armed(), "the host's check is taken");
}

/// b4 D57: `PruneAndFlush` with no tip arms the check, and its NONE flush takes it under the
/// selector's timer, selecting nothing (`v:3152-3159` at v24.0.0, with `Height() == -1`).
#[test]
fn prune_and_flush_with_no_tip_arms_and_clears_the_check() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut v = s.v();
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    };
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    without_tip(&mut v, &mut host, |cx| ctl.prune_and_flush_without_tip(cx)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(prune_timer(&s), "find files to prune");
    assert!(!host.check_due(), "the check was cleared");
    assert!((0..3).all(|n| blk_on_disk(&s.cfg.blocks_dir, n)));
}

/// Once a failure has latched, V clears nothing for W: W writes nothing more, so the clears would
/// leave V's index short of blocks still on disk. The request reaches V's `handle` inside the
/// prune's call, as every prune's does (CH3b D244): V returns the failure and drops the request.
#[test]
fn v_clears_nothing_once_a_failure_has_latched() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + KEEP);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    ctl.in_flight = Some(InFlight {
        flush_id: 1,
        signal: Some(tip),
        #[cfg(feature = "test-support")]
        booked: Booked {
            full: true,
            causes: FlushCauses::PRUNE,
            label: None,
            stamp: Stamp::default(),
            forced: false,
            kept: true,
        },
    });
    let rule = crate::flush::PruneRule::Automatic {
        after: 0,
        keep: KEEP,
    };
    let (req, wait) = crate::flush::clears::PruneClears::new(1, vec![0], 9, rule);
    s.db.latch().trip(Failure {
        kind: ErrorKind::Other,
        site: "test",
        message: "a failure elsewhere".into(),
    });
    let mut v = s.v();
    v.tip = tip;
    let mut index = ClearingIndex::default();
    let handled = with_prune_cx(&mut v, &mut NoPrune, &mut index, None, |cx| {
        ctl.handle(DriverEvent::PruneClears(req), cx)
    });
    assert!(handled.is_err(), "V returns the failure");
    assert!(index.asked.is_empty());
    assert!(matches!(wait.poll(), crate::flush::clears::Polled::Dropped), "the request is dropped");
}

/// A freeze decided before a failure latched is refused once it has: V takes nothing from its
/// cache, its index or its evo window, and submits nothing to W.
#[test]
fn a_freeze_after_a_latched_failure_takes_nothing() {
    let s = Stores::new(1 << 20);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 20, 1);
    let pending = v.with_cx(|cx| ctl.at_site(FlushStateMode::IfNeeded, cx)).unwrap();
    assert!(matches!(pending.action(), FlushAction::Freeze { .. }));
    let failure = Failure {
        kind: ErrorKind::Other,
        site: "test",
        message: "a failure elsewhere".into(),
    };
    assert!(s.db.latch().trip(failure.clone()));
    let seq = v.step(block(1));
    let err = v.with_cx(|cx| pending.freeze_after_step(&mut ctl, cx, block(1), seq)).unwrap_err();
    assert_eq!(err, Fatal(failure));
    assert!(!s.db.latch().any_site(|site| site == FLUSH_SITE), "refused before any abort");
    assert!(!ctl.in_flight());
    assert!(v.coins.frozen().is_none(), "nothing was frozen");
    assert_eq!(v.dirty.len(), 1, "the index records stay with V");
    assert_eq!(v.evo.takes, 0);
    assert_eq!(run_w(&mut ctl), []);
}

/// V's index fails to clear: V stops the node with its own message, W writes nothing of the
/// prune and stops without a second report, and no file is removed.
#[test]
fn a_failed_clear_on_v_stops_the_flush_with_vs_message() {
    let tip = block(19 + KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 0,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        };
        let mut index = ClearingIndex {
            fail: Some("the index could not clear".into()),
            ..ClearingIndex::default()
        };
        if threaded {
            let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            assert!(flush_pruning(&mut ctl, &mut v, &mut host, &mut index, Some(15)).is_err());
            drop(ctl);
        } else {
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            assert!(flush_pruning(&mut ctl, &mut v, &mut host, &mut index, Some(15)).is_err());
            assert_eq!(run_w(&mut ctl), [], "W is refused");
        }
        assert_eq!(index.asked, [vec![0]], "threaded {threaded}");
        let abort = AbortMark::AbortNode {
            log: "the index could not clear".into(),
            user: None,
        };
        assert_eq!(s.sink.marks(), [abort], "threaded {threaded}");
        let meta = s.meta();
        assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), false));
        assert!(blk_on_disk(&s.cfg.blocks_dir, 0));
    }
}

/// Three files of ten blocks each, the last one current.
fn three_files() -> BlockFilesState {
    equal_files(3, 1 << 20, 1 << 10)
}

/// V unwinds inside a prune's call while W waits for its `PruneClears` answer, and then drops the
/// thread driver: the request is dropped, W's flush stops with nothing pruned, and the drop
/// returns.
#[test]
#[expect(clippy::disallowed_methods, reason = "a plain test thread that drops V's side")]
fn dropping_the_thread_driver_ends_ws_wait_for_the_clears() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + KEEP);
    let mut v = s.v();
    v.connect(0, 10, tip.height);
    v.step(tip);
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut index = ClearingIndex {
        unwind_at_clear: true,
        ..ClearingIndex::default()
    };
    let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None)
    }));
    assert!(unwound.is_err(), "V unwinds inside the prune's call");
    let (done, dropped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(ctl);
        done.send(()).unwrap();
    });
    dropped.recv_timeout(Duration::from_secs(120)).expect("the drop returns");
    assert!(index.asked.is_empty(), "V never answered");
    let abort = AbortMark::AbortNode {
        log: "The block index dropped the prune clears unanswered".into(),
        user: None,
    };
    assert_eq!(s.sink.marks(), [abort]);
    let meta = s.meta();
    assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), false));
    assert!(blk_on_disk(&s.cfg.blocks_dir, 0));
}

/// b3 adapter items 7 and 8 (D45): `stop` waits out the job in flight, and `trip` latches the
/// store with no report of its own, saying whether another failure latched first.
#[test]
fn stop_waits_out_the_flight_and_trip_reports_nothing() {
    let s = Stores::new(2 << 10);
    let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    let mut v = s.v();
    v.scripted = true;
    v.connect(0, 300, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    v.with_cx(|cx| ctl.stop(cx)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(s.meta().tip, Some(block(1).hash));
    assert!(!ctl.trip(&s.db, "AbortNode on V"));
    assert!(ctl.trip(&s.db, "again"));
    assert!(s.sink.marks().is_empty(), "V's sink reports it");
    assert_eq!(v.with_cx(|cx| ctl.stop(cx)).unwrap_err().0.message, "AbortNode on V");
}

/// One scripted full flush of `n` coins at `S` = `sub_batch`, with V's reserve `r` (the node's
/// R): B1's batch's capacity, and whether the flush split.
fn b1_capacity(sub_batch: u64, r: u64, n: u32) -> (usize, bool) {
    let s = Stores::new(sub_batch);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v_budget(CoinsBudget::new(1 << 30, 0, Ratio::HALF, r));
    v.scripted = true;
    v.connect(0, n, 1);
    v.site_and_step(&mut ctl, block(1)).unwrap();
    let steps = run_w(&mut ctl);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert_eq!(s.meta().tip, Some(block(1).hash), "the flush completed");
    let split = steps.iter().any(|s| matches!(s, FlushStep::SubBatch(_) | FlushStep::Bk));
    (s.db.inner().capacities()[0], split)
}

/// BL-511: B1's batch holds what B1 can come to, from V's counters, while that is under R, and
/// R once it is not. `b1`'s guard checks that B1 never outgrows a bound: many small coins in one
/// batch is the tightest case, every coin's batch overhead counted.
#[test]
fn b1_is_sized_from_the_dirty_set_capped_at_r() {
    const S: u64 = 4 << 20;
    const R: u64 = S + (64 << 10);
    let (cap, split) = b1_capacity(S, R, 20);
    assert!(!split && (B1_SLACK..2 * B1_SLACK).contains(&cap), "a few coins: {cap}");
    let (cap, split) = b1_capacity(S, R, 40_000);
    assert!(!split && cap > 40_000 * 66 && cap < R as usize, "one batch near S: {cap}");
    // A bound past R gives R: this flush's coins pass S, so it splits.
    let (cap, split) = b1_capacity(64 << 10, (64 << 10) * 2, 4_000);
    assert!(split);
    assert_eq!(cap, (64 << 10) * 2);
}

/// The bound's terms, saturating; an index that cannot say what it holds gives none. The values
/// are written out: RocksDB's 12-byte batch header, at most 16 bytes of a record's overhead, a
/// coin key's 37th byte and the 64 KiB slack.
#[test]
fn b1_bound_adds_every_part() {
    assert_eq!(b1_bound(Some((0, 0)), None, 0), Some(65_548));
    assert_eq!(b1_bound(Some((2, 300)), None, 0), Some(65_880));
    assert_eq!(b1_bound(Some((0, 0)), Some((3, 200)), 0), Some(65_799));
    assert_eq!(b1_bound(Some((0, 0)), None, 1_000), Some(67_548));
    assert_eq!(b1_bound(None, None, 0), None, "an unknown index");
    let most = b1_bound(Some((usize::MAX, usize::MAX)), None, u64::MAX);
    assert_eq!(most, Some(usize::MAX), "no overflow");
    assert_eq!(stored_records(3), Some((3, 426)));
}

/// BL-511 r1 m-1: a one-batch flush of real evo rows on RocksDB, from the freeze's bound to W's
/// B1. The evo window is an [`EvoLayer`] holding blocks' credit-data rows (32-byte keys) and the
/// tombstones of the commitments each erases, a 34-byte `llmq_mined` and a 9-byte
/// `llmq_mined_h` key apiece: the rows whose record overhead is largest beside what they count,
/// enough of them that the bound needs twice their bytes. B1 also puts the blocks' index
/// records, then with and without coins. B1's guard checks the batch against the bound in debug
/// builds; the rows are durable after.
#[cfg(feature = "rocksdb")]
#[test]
fn b1_holds_real_evo_rows_within_its_bound_on_rocksdb() {
    use crate::evo::codec::EvoCf;
    use crate::evo::{EvoLayer, EvoWindowDelta, testkit};
    use dash_dml::creditpool::CreditBlockData;
    use dash_dml::store::MinedErase;
    use dash_params::LlmqType;
    use dash_primitives::QuorumHash;

    const BLOCKS: u32 = 100;
    const ERASED: i16 = 250;
    const R: u64 = 32 << 20;
    for coins in [0, 20] {
        let s = Stores::rocks(R);
        let mut ctl = s.control(InlineDriver::new(s.env(CrashRoll::seeded(0, 0))), false);
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, R);
        let mut v = s.v_budget(budget).with_evo(EvoLayer::new());
        v.scripted = true;
        for height in 1..=BLOCKS {
            let at = block(height);
            let erased_mined = (0..ERASED)
                .map(|i| {
                    let mut q = [0; 32];
                    q[..4].copy_from_slice(&height.to_le_bytes());
                    q[4..6].copy_from_slice(&i.to_le_bytes());
                    MinedErase {
                        llmq_type: LlmqType::Llmq60_75,
                        quorum_hash: QuorumHash::from(q),
                        mined_height: height,
                        rotation_index: Some(i),
                    }
                })
                .collect();
            let delta = dash_dml::EvoDelta {
                block: at.hash,
                height,
                dml: None,
                mined_commitments: Vec::new(),
                credit_data: Some(CreditBlockData {
                    balance: i64::from(height),
                    unlocked: 0,
                }),
                credit_pool: None,
                mnhf_signals: None,
                erased_mined,
                evo_tip: Some(at.hash),
            };
            v.evo.commit(testkit::codec().rows(&delta).unwrap(), &*s.db).unwrap();
            v.connect(height * coins, coins, height);
        }
        let evo = EvoWindow::<EvoWindowDelta>::bytes(&v.evo);
        assert!(evo > 2 * B1_SLACK as u64, "{evo}");
        v.site_and_step(&mut ctl, block(BLOCKS)).unwrap();
        let steps = run_w(&mut ctl);
        v.with_cx(|cx| ctl.poll(cx)).unwrap();
        assert_eq!(s.meta().tip, Some(block(BLOCKS).hash), "the flush completed");
        assert!(!steps.iter().any(|s| matches!(s, FlushStep::SubBatch(_) | FlushStep::Bk)));
        let cap = s.db.inner().capacities()[0];
        assert!(cap < R as usize, "B1 is sized from its bound: {cap}");
        for height in [1, BLOCKS] {
            let key = EvoCf::CreditData.key(&block(height).hash);
            assert!(s.db.get(EvoCf::CreditData.cf(), key.as_bytes()).unwrap().is_some());
        }
    }
}

/// CH3b D242 (BL-813): each job's take as Core would make it, on a node `HeaderChain`. Each row
/// dirties the index's entry e, runs a job to its release, then makes an undirtied change to e
/// (Core's `MarkConflictingBlock` mark, D15): a take Core does not make leaves it dirty, as Core's
/// still-dirty entry carries it to its next write; Core's take leaves it clean.
#[cfg(feature = "test-support")]
mod index_dirt {
    use dash_chain::{BlockIndexRecord, ChainConfig, HeaderChain, NoStore, Services, Status};
    use dash_chain::{FailStop, StaticSporks};
    use dash_clock::MockClock;
    use dash_params::{ChainParamsBuilder, Network, Release};

    use super::*;
    use crate::flush::DATABASE_FLUSH_INTERVAL;

    const E: BlockId = BlockId(0);

    struct NoFail;

    impl FailStop for NoFail {
        fn read_failed(&self, what: &str) {
            panic!("the latch tripped: {what}");
        }

        fn is_tripped(&self) -> bool {
            false
        }
    }

    /// V's block index as `FlushHost` hands it to the flush: a node `HeaderChain` (genesis is
    /// e), its takes through the node's index view; its prune clears return nothing.
    struct ChainIndex(HeaderChain);

    impl ChainIndex {
        fn new() -> Self {
            let p = Arc::new(
                ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest"),
            );
            let svc = Services {
                clock: Arc::new(MockClock::at(0)),
                sporks: Arc::new(StaticSporks::for_network(&p, None)),
                fail: Arc::new(NoFail),
            };
            let cfg = ChainConfig::node(&p);
            Self(HeaderChain::new_genesis(p, cfg, svc, NoStore).expect("genesis"))
        }

        fn ix(&mut self) -> dash_chain::NodeIndex<'_> {
            self.0.node_index().expect("the node's")
        }

        /// e dirtied, as a write at one of Core's sites dirties it.
        fn dirty_e(&mut self) {
            self.ix().mark_dirty(E);
        }

        /// The undirtied mark on e (D15).
        fn mark_e(&mut self) {
            let conflict = |s: Status| Status(s.bits() | Status::CONFLICT_CHAINLOCK);
            self.ix().modify(E, |g| g.status = conflict(g.status));
        }

        /// Another undirtied change to e.
        fn change_e(&mut self, n_tx: u32) {
            self.ix().modify(E, |g| g.n_tx = n_tx);
        }

        fn dirty_len(&mut self) -> usize {
            self.ix().dirty_len()
        }

        fn hash(&self) -> BlockHash {
            self.0.reader().entry(E).expect("genesis").hash
        }
    }

    impl FlushIndex for ChainIndex {
        fn take_dirty_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
            self.ix().take_dirty_into(put);
        }

        fn take_dirty_keeping_into(&mut self, put: &mut dyn FnMut(&BlockHash, &[u8])) {
            self.ix().take_dirty_keeping_into(put);
        }

        fn prune_clear(&mut self, _: &[u32]) -> Result<Vec<IndexWrite>, String> {
            Ok(Vec::new())
        }
    }

    fn jobs(t: &FlushTally) -> u64 {
        let c = t.since_open();
        c.full.values().chain(c.index_only.values()).sum()
    }

    /// What a row's call reads beside V.
    #[derive(Clone, Copy, Default)]
    struct Knobs {
        left_ibd: bool,
        manual: Option<u32>,
    }

    fn with_chain<T>(
        v: &mut V,
        index: &mut ChainIndex,
        prune: &mut dyn PruneHost,
        knobs: Knobs,
        f: impl FnOnce(&mut FlushCx<'_, ProbeEngine, ()>) -> T,
    ) -> T {
        let mut cx = FlushCx {
            now: v.now,
            coins: &mut v.coins,
            mempool_usage: 0,
            tip: v.tip,
            last_seq: v.seq,
            index,
            evo: &mut v.evo,
            prune,
            manual_prune_height: knobs.manual,
            reindex: false,
            is_ibd: !knobs.left_ibd,
            left_ibd: knobs.left_ibd,
            scripted: v.scripted,
        };
        f(&mut cx)
    }

    fn without_tip_chain<T>(
        v: &mut V,
        index: &mut ChainIndex,
        f: impl FnOnce(&mut NoTipCx<'_, ProbeEngine, ()>) -> T,
    ) -> T {
        let mut cx = NoTipCx {
            now: v.now,
            coins: &mut v.coins,
            mempool_usage: 0,
            index,
            evo: &mut v.evo,
            prune: &mut NoPrune,
            reindex: false,
        };
        f(&mut cx)
    }

    type Inline<'s, F> = Ctl<InlineDriver<'s, ProbeEngine, F, MemoryTuning, ()>>;

    /// Every job released: W run dry, then V's poll, which may freeze again.
    fn release<F: FlatFs>(
        ctl: &mut Inline<'_, F>,
        v: &mut V,
        index: &mut ChainIndex,
        prune: &mut dyn PruneHost,
    ) {
        while ctl.in_flight() {
            run_w(ctl);
            with_chain(v, index, prune, Knobs::default(), |cx| ctl.poll(cx)).unwrap();
        }
    }

    /// `mode`'s flush on V, run to its release.
    fn flush_to_release<F: FlatFs>(
        ctl: &mut Inline<'_, F>,
        v: &mut V,
        index: &mut ChainIndex,
        mode: FlushStateMode,
        knobs: Knobs,
    ) {
        with_chain(v, index, &mut NoPrune, knobs, |cx| ctl.flush(mode, cx)).unwrap();
        release(ctl, v, index, &mut NoPrune);
    }

    /// e's stored record.
    fn stored<F: FlatFs>(s: &Stores<F>, index: &ChainIndex) -> BlockIndexRecord {
        let cf = crate::store::CHAINSTATE_SCHEMA.cf("block_index").unwrap();
        let bytes = s.db.inner().unprobed().get(cf, index.hash().as_bytes()).unwrap();
        BlockIndexRecord::decode(&bytes.expect("e is stored")).expect("a record")
    }

    /// After a row's job: the mark on e, dirty if the job kept Core's dirt. After a keeping job,
    /// an ALWAYS (Core's take) writes the mark and ends the kept dirt.
    fn mark_after<F: FlatFs>(
        s: &Stores<F>,
        ctl: &mut Inline<'_, F>,
        v: &mut V,
        index: &mut ChainIndex,
        kept: bool,
        row: &str,
    ) {
        assert!(!ctl.in_flight(), "{row}: released");
        index.mark_e();
        assert_eq!(index.dirty_len(), usize::from(kept), "{row}: the mark's dirt");
        if kept {
            flush_to_release(ctl, v, index, FlushStateMode::Always, Knobs::default());
            let status = stored(s, index).status;
            assert!(status.has(Status::CONFLICT_CHAINLOCK), "{row}: the ALWAYS wrote the mark");
            index.change_e(9);
            assert_eq!(index.dirty_len(), 0, "{row}: Core's take ended the kept dirt");
        }
    }

    /// V at a block whose coins put the cache at `target`, where a check at IF_NEEDED or PERIODIC
    /// finds it.
    fn v_at<F: FlatFs>(s: &Stores<F>, target: CoinsBoundary) -> V {
        let mut v = s.v_budget(CoinsBudget::new(256 << 10, 0, Ratio::new(19, 20), 0));
        for i in 0.. {
            if v.coins.boundary_state(0, false) == target {
                break;
            }
            assert!(i < 100_000, "{target:?} reached");
            v.connect(i, 1, 1);
        }
        v.step(block(1));
        v
    }

    /// V at block 1 with a cache no check finds large.
    fn v_ok<F: FlatFs>(s: &Stores<F>) -> V {
        let mut v = s.v_with(1 << 34);
        v.connect(0, 10, 1);
        v.step(block(1));
        v
    }

    /// The keeping rows: node-own causes (THRESHOLD, DMAX, LEFT_IBD, SCRIPTED), the
    /// native-measure ones (CACHE_LARGE, CACHE_CRITICAL, EVO_CRITICAL; DEC-1451 Q2), and an
    /// index-only write with a node-own cause (no best block).
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_keeping_rows() {
        type Setup = fn(&Stores<OsFs>, &mut Inline<'_, OsFs>) -> (V, FlushStateMode, Knobs);
        let rows: [(&str, Setup); 8] = [
            ("THRESHOLD", |s, _| {
                (v_at(s, CoinsBoundary::Threshold), FlushStateMode::IfNeeded, Knobs::default())
            }),
            ("DMAX", |s, ctl| {
                ctl.set_hub(s.hub.clone());
                s.hub.post_flush_due();
                (v_ok(s), FlushStateMode::None, Knobs::default())
            }),
            ("LEFT_IBD", |s, _| {
                (
                    v_ok(s),
                    FlushStateMode::None,
                    Knobs {
                        left_ibd: true,
                        ..Knobs::default()
                    },
                )
            }),
            ("CACHE_LARGE", |s, _| {
                (v_at(s, CoinsBoundary::Large), FlushStateMode::Periodic, Knobs::default())
            }),
            ("CACHE_CRITICAL", |s, _| {
                (v_at(s, CoinsBoundary::Critical), FlushStateMode::IfNeeded, Knobs::default())
            }),
            ("EVO_CRITICAL", |s, _| {
                let mut v = v_ok(s);
                v.evo.active = EVO_CRITICAL_BYTES;
                (v, FlushStateMode::IfNeeded, Knobs::default())
            }),
            ("SCRIPTED", |s, _| {
                let mut v = v_ok(s);
                v.scripted = true;
                (v, FlushStateMode::None, Knobs::default())
            }),
            ("IndexOnly LEFT_IBD (no best block)", |s, _| {
                (
                    s.v(),
                    FlushStateMode::None,
                    Knobs {
                        left_ibd: true,
                        ..Knobs::default()
                    },
                )
            }),
        ];
        for (row, setup) in rows {
            let s = Stores::new(1 << 20);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let tally = tally(&mut ctl);
            let (mut v, mode, knobs) = setup(&s, &mut ctl);
            let mut index = ChainIndex::new();
            index.dirty_e();
            flush_to_release(&mut ctl, &mut v, &mut index, mode, knobs);
            v.scripted = false;
            assert_eq!(jobs(&tally), 1, "{row}: one job");
            assert_eq!(index.dirty_len(), 0, "{row}: the job took e");
            mark_after(&s, &mut ctl, &mut v, &mut index, true, row);
        }
    }

    /// Core's rows (DEC-1500): ALWAYS, its shortcut, ALWAYS beside a node-own cause, and with no
    /// tip, ALWAYS. Beside a node-own cause the job is booked as ALWAYS, since 4c's tally names a
    /// job with a Core cause by Core's causes alone (O2b-4 §2.3), and its journal entry keeps both.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_core_rows() {
        type Run = fn(&Stores<OsFs>, &mut Inline<'_, OsFs>, &mut ChainIndex);
        let rows: [(&str, Run); 4] = [
            ("ALWAYS", |s, ctl, index| {
                let mut v = v_ok(s);
                flush_to_release(ctl, &mut v, index, FlushStateMode::Always, Knobs::default());
                mark_after(s, ctl, &mut v, index, false, "ALWAYS");
            }),
            ("the ALWAYS shortcut", |s, ctl, index| {
                let mut v = v_ok(s);
                flush_to_release(ctl, &mut v, index, FlushStateMode::Always, Knobs::default());
                index.dirty_e();
                let signals = s.signals.tips().len();
                flush_to_release(ctl, &mut v, index, FlushStateMode::Always, Knobs::default());
                assert_eq!(v.evo.takes, 1, "the shortcut takes no evo window");
                assert_eq!(s.signals.tips().len(), signals + 1, "and signals");
                mark_after(s, ctl, &mut v, index, false, "the ALWAYS shortcut");
            }),
            ("LEFT_IBD|ALWAYS", |s, ctl, index| {
                let mut v = v_ok(s);
                let tally = tally(ctl);
                let left = Knobs {
                    left_ibd: true,
                    ..Knobs::default()
                };
                flush_to_release(ctl, &mut v, index, FlushStateMode::Always, left);
                assert_eq!(tally.since_open().full, by_cause(&[("ALWAYS", 1)]));
                let journal = tally.journal();
                assert_eq!(journal.len(), 1, "LEFT_IBD|ALWAYS: one booking");
                assert!(journal[0].full, "LEFT_IBD|ALWAYS: a full flush");
                assert_eq!(journal[0].causes, FlushCauses::ALWAYS | FlushCauses::LEFT_IBD);
                assert!(!journal[0].kept, "LEFT_IBD|ALWAYS: Core's take");
                mark_after(s, ctl, &mut v, index, false, "LEFT_IBD|ALWAYS");
            }),
            ("no-tip ALWAYS", |s, ctl, index| {
                let mut v = s.v();
                without_tip_chain(&mut v, index, |cx| {
                    ctl.flush_without_tip(FlushStateMode::Always, cx)
                })
                .unwrap();
                mark_after(s, ctl, &mut v, index, false, "no-tip ALWAYS");
            }),
        ];
        for (row, run) in rows {
            let s = Stores::new(1 << 20);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let mut index = ChainIndex::new();
            index.dirty_e();
            run(&s, &mut ctl, &mut index);
            assert!(!ctl.in_flight(), "{row}");
        }
    }

    /// DEC-1500: Core's periodic write and PERIODIC_FLUSH keep, as V's timers can differ from
    /// Core's: the periodic write alone, PERIODIC_FLUSH, and with no tip the periodic write. The
    /// mark after each is dirty, and the ALWAYS after it stores the mark.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_periodic_rows() {
        let hour = DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
        let day = DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
        for (row, by) in [("the periodic write alone", hour), ("PERIODIC_FLUSH", day)] {
            let s = Stores::new(1 << 20);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let mut index = ChainIndex::new();
            let mut v = v_ok(&s);
            flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::None, Knobs::default());
            v.now += by;
            index.dirty_e();
            flush_to_release(
                &mut ctl,
                &mut v,
                &mut index,
                FlushStateMode::Periodic,
                Knobs::default(),
            );
            assert_eq!(v.evo.takes, u32::from(by == day), "{row}: a freeze for PERIODIC_FLUSH");
            assert_eq!(index.dirty_len(), 0, "{row}: the job took e");
            mark_after(&s, &mut ctl, &mut v, &mut index, true, row);
        }

        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut index = ChainIndex::new();
        index.dirty_e();
        let mut v = s.v();
        let periodic = |ctl: &mut Inline<'_, OsFs>, v: &mut V, index: &mut ChainIndex| {
            without_tip_chain(v, index, |cx| ctl.flush_without_tip(FlushStateMode::Periodic, cx))
                .unwrap();
        };
        periodic(&mut ctl, &mut v, &mut index);
        assert!(!ctl.in_flight(), "no write right after startup");
        v.now += hour;
        periodic(&mut ctl, &mut v, &mut index);
        assert!(ctl.in_flight());
        without_tip_chain(&mut v, &mut index, |cx| ctl.stop_without_tip(cx)).unwrap();
        index.mark_e();
        assert_eq!(index.dirty_len(), 1, "no tip: kept");
        without_tip_chain(&mut v, &mut index, |cx| {
            ctl.flush_without_tip(FlushStateMode::Always, cx)
        })
        .unwrap();
        assert!(stored(&s, &index).status.has(Status::CONFLICT_CHAINLOCK), "no tip: stored");
        index.change_e(9);
        assert_eq!(index.dirty_len(), 0, "no tip: the ALWAYS ended it");
    }

    /// An automatic prune keeps Core's dirt (DEC-1517): its job, waited for inside its call (CH3b
    /// D244).
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_prune_row() {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let tip = block(19 + KEEP);
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 1 << 20,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        };
        s.files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .unwrap();
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut index = ChainIndex::new();
        index.dirty_e();
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::None, cx)
        })
        .unwrap();
        assert!(!ctl.in_flight(), "an automatic prune is waited for");
        assert!(s.meta().pruned_files);
        mark_after(&s, &mut ctl, &mut v, &mut index, true, "PRUNE");
    }

    /// A manual prune keeps Core's dirt too (DEC-1517): after a decline or a deferred
    /// selection V's files can lag Core's, so its selection can be of files stock Core already
    /// pruned, where stock Core's manual run selects nothing and writes nothing.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_manual_prune_row() {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let tip = block(19 + KEEP);
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 0,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        };
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut index = ChainIndex::new();
        index.dirty_e();
        let knobs = Knobs {
            manual: Some(15),
            ..Knobs::default()
        };
        with_chain(&mut v, &mut index, &mut host, knobs, |cx| ctl.flush(FlushStateMode::None, cx))
            .unwrap();
        release(&mut ctl, &mut v, &mut index, &mut host);
        assert!(s.meta().pruned_files, "pruned");
        assert_eq!(index.dirty_len(), 0, "the prune took e");
        mark_after(&s, &mut ctl, &mut v, &mut index, true, "manual PRUNE");
    }

    /// DEC-1517, CH3b D244: an automatic prune check that Core's call meets, with a job in flight
    /// or not, is decided at that call, and its take keeps Core's dirt: e, dirtied before the call,
    /// is taken by the prune's freeze and is still Core's dirt, so its mark reaches the disk at the
    /// ALWAYS. The rows assert V's side only: V keeps whatever stock Core selected, so they need no
    /// model of Core's files.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_in_flight_prune_rows() {
        for (row, in_flight) in [("met in flight", true), ("met with nothing in flight", false)] {
            let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
            let tip = block(19 + KEEP);
            let mut v = s.v();
            v.connect(0, 10, tip.height);
            v.step(tip);
            let mut host = files_host(&s);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let mut index = ChainIndex::new();
            if in_flight {
                v.scripted = true;
                with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
                    ctl.flush(FlushStateMode::None, cx)
                })
                .unwrap();
                v.scripted = false;
                assert!(ctl.in_flight(), "{row}: a job in flight");
            }
            arm(&s);
            index.dirty_e();
            with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
                ctl.flush(FlushStateMode::None, cx)
            })
            .unwrap();
            assert!(!ctl.in_flight(), "{row}: the prune's job, waited for");
            assert!(s.meta().pruned_files, "{row}: pruned");
            assert_eq!(index.dirty_len(), 0, "{row}: the prune took e");
            mark_after(&s, &mut ctl, &mut v, &mut index, true, row);
        }
    }

    /// m-1 (M10): with no tip, a native cause's index-only write keeps Core's dirt; the no-tip
    /// ALWAYS after it writes the mark and ends it.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_no_tip_keeping_row() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut v = s.v_budget(CoinsBudget::new(256 << 10, 0, Ratio::new(19, 20), 0));
        let mut i = 0;
        while v.coins.boundary_state(0, false) != CoinsBoundary::Critical {
            let out = TxOut::from_duffs(1_000, vec![0x51; 25].into());
            let src = CoinSrc::Output {
                out: &out,
                height: 0,
                coinbase: false,
            };
            v.coins.add(&key(i), src, false).unwrap();
            i += 1;
        }
        assert_eq!(v.coins.best_block(), None);
        let mut index = ChainIndex::new();
        index.dirty_e();
        let tally = tally(&mut ctl);
        without_tip_chain(&mut v, &mut index, |cx| {
            ctl.flush_without_tip(FlushStateMode::IfNeeded, cx)
        })
        .unwrap();
        without_tip_chain(&mut v, &mut index, |cx| ctl.stop_without_tip(cx)).unwrap();
        let want = by_cause(&[("CACHE_CRITICAL", 1)]);
        assert_eq!(tally.since_open().index_only, want, "an index-only write");
        assert_eq!(index.dirty_len(), 0);
        index.mark_e();
        assert_eq!(index.dirty_len(), 1, "kept");
        without_tip_chain(&mut v, &mut index, |cx| {
            ctl.flush_without_tip(FlushStateMode::Always, cx)
        })
        .unwrap();
        assert!(stored(&s, &index).status.has(Status::CONFLICT_CHAINLOCK));
        index.change_e(9);
        assert_eq!(index.dirty_len(), 0, "the ALWAYS ended it");
    }

    /// B-1 (i) and (ii), DEC-1500: a take that writes as Core's periodic write (`validation.cpp:3053`)
    /// keeps beside its own causes: a DMAX freeze, and with no tip an index-only write for
    /// CACHE_LARGE. Then the mark is dirty, and the ALWAYS after it stores it.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_b1_due_rows() {
        let hour = DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
        for no_tip in [false, true] {
            let s = Stores::new(1 << 20);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let tally = tally(&mut ctl);
            let mut index = ChainIndex::new();
            let mut v = if no_tip {
                let mut v = s.v_budget(CoinsBudget::new(256 << 10, 0, Ratio::new(19, 20), 0));
                let mut i = 0;
                while v.coins.boundary_state(0, false) != CoinsBoundary::Large {
                    let out = TxOut::from_duffs(1_000, vec![0x51; 25].into());
                    let src = CoinSrc::Output {
                        out: &out,
                        height: 0,
                        coinbase: false,
                    };
                    v.coins.add(&key(i), src, false).unwrap();
                    i += 1;
                }
                without_tip_chain(&mut v, &mut index, |cx| {
                    ctl.flush_without_tip(FlushStateMode::None, cx)
                })
                .unwrap();
                v
            } else {
                let mut v = v_ok(&s);
                flush_to_release(
                    &mut ctl,
                    &mut v,
                    &mut index,
                    FlushStateMode::None,
                    Knobs::default(),
                );
                v
            };
            assert!(!ctl.in_flight(), "the first call starts the timers");
            index.take_dirty();
            index.dirty_e();
            v.now += hour;
            if no_tip {
                without_tip_chain(&mut v, &mut index, |cx| {
                    ctl.flush_without_tip(FlushStateMode::Periodic, cx)
                })
                .unwrap();
                without_tip_chain(&mut v, &mut index, |cx| ctl.stop_without_tip(cx)).unwrap();
                let want = by_cause(&[("CACHE_LARGE", 1)]);
                assert_eq!(tally.since_open().index_only, want, "(ii)");
            } else {
                ctl.set_hub(s.hub.clone());
                s.hub.post_flush_due();
                flush_to_release(
                    &mut ctl,
                    &mut v,
                    &mut index,
                    FlushStateMode::Periodic,
                    Knobs::default(),
                );
                assert_eq!(tally.since_open().full, by_cause(&[("DMAX", 1)]), "(i)");
            }
            index.mark_e();
            assert_eq!(index.dirty_len(), 1, "no_tip {no_tip}: kept beside Core's write");
            if no_tip {
                without_tip_chain(&mut v, &mut index, |cx| {
                    ctl.flush_without_tip(FlushStateMode::Always, cx)
                })
                .unwrap();
            } else {
                flush_to_release(
                    &mut ctl,
                    &mut v,
                    &mut index,
                    FlushStateMode::Always,
                    Knobs::default(),
                );
            }
            let status = stored(&s, &index).status;
            assert!(status.has(Status::CONFLICT_CHAINLOCK), "no_tip {no_tip}: stored");
        }
    }

    /// B-1 (iii), D8: a periodic write that comes due while a keeping job is in flight is taken
    /// after the release, at V's poll, as an index-only write. It keeps (DEC-1500): Core wrote at
    /// the earlier call, so a take after it may clean what Core holds. The mark is dirty, and the
    /// ALWAYS after it stores it.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_b1_in_flight_row() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut v = v_ok(&s);
        let mut index = ChainIndex::new();
        index.dirty_e();
        v.scripted = true;
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::None, cx)
        })
        .unwrap();
        v.scripted = false;
        assert!(ctl.in_flight(), "a keeping job in flight");
        v.now += DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        release(&mut ctl, &mut v, &mut index, &mut NoPrune);
        let booked = tally.since_open();
        index.mark_e();
        assert_eq!(index.dirty_len(), 1, "the carried write kept");
        flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::Always, Knobs::default());
        assert!(stored(&s, &index).status.has(Status::CONFLICT_CHAINLOCK), "stored");
        let want = CauseCounts {
            full: by_cause(&[("SCRIPTED", 1)]),
            index_only: by_cause(&[("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(booked, want, "the poll after the release wrote the index alone");
    }

    /// B-3: a keeping job J0 frozen at t0 is in flight when Core's write comes due; the next
    /// PERIODIC call takes J0's release and writes the carried write once. Nothing is carried past
    /// it: a later poll submits no job and takes nothing.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_b3_row() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut v = v_ok(&s);
        let mut index = ChainIndex::new();
        index.dirty_e();
        v.scripted = true;
        let t0 = v.now;
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::None, cx)
        })
        .unwrap();
        v.scripted = false;
        v.now = t0 + DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        run_w(&mut ctl);
        v.now += Duration::from_secs(1);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        assert!(ctl.in_flight(), "one follow-up job");
        release(&mut ctl, &mut v, &mut index, &mut NoPrune);
        let want = CauseCounts {
            full: by_cause(&[("SCRIPTED", 1)]),
            index_only: by_cause(&[("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want);
        index.dirty_e();
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| ctl.poll(cx)).unwrap();
        assert!(!ctl.in_flight(), "no job");
        assert_eq!(tally.since_open(), want, "nothing booked");
        assert_eq!(index.dirty_len(), 1, "e not taken");
        // Core wrote at the in-flight call, t0 + 1 h + 1 s, not at the carried write's decision a
        // second later: the next write is due an hour after the first.
        v.now = t0 + 2 * DATABASE_WRITE_INTERVAL + Duration::from_millis(1_500);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        assert!(ctl.in_flight(), "due an hour after Core's write");
    }

    /// B-1: a PERIODIC_FLUSH that comes due while a periodic write is in flight is Core's write
    /// at that call, so the freeze after the release, which takes it as a held cause, does not
    /// move the write time: the next periodic write is due an hour after the in-flight call.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_held_periodic_flush_row() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut v = v_ok(&s);
        let mut index = ChainIndex::new();
        let start = v.now;
        flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::None, Knobs::default());
        v.now = start + DATABASE_FLUSH_INTERVAL - Duration::from_secs(600);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        assert!(ctl.in_flight(), "the periodic write in flight");
        let t1 = start + DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
        for at in [t1, t1 + Duration::from_secs(1)] {
            v.now = at;
            with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
                ctl.flush(FlushStateMode::Periodic, cx)
            })
            .unwrap();
        }
        v.now += Duration::from_secs(1);
        release(&mut ctl, &mut v, &mut index, &mut NoPrune);
        let want = CauseCounts {
            full: by_cause(&[("PERIODIC_FLUSH", 1)]),
            index_only: by_cause(&[("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want, "the held PERIODIC_FLUSH froze after the release");
        v.now = t1 + DATABASE_WRITE_INTERVAL + Duration::from_millis(500);
        with_chain(&mut v, &mut index, &mut NoPrune, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        assert!(ctl.in_flight(), "due an hour after Core's write at the in-flight call");
        release(&mut ctl, &mut v, &mut index, &mut NoPrune);
        // BL-818: Core's flush at the in-flight call set `m_last_flush` there too.
        v.now = t1 + DATABASE_FLUSH_INTERVAL + Duration::from_millis(500);
        flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::Periodic, Knobs::default());
        let full = tally.since_open().full;
        assert_eq!(full, by_cause(&[("PERIODIC_FLUSH", 2)]), "due a day after the in-flight call");
    }

    /// B-1 (CH3b D242): a job that is Core's write, held in flight across more PERIODIC calls,
    /// is the one write Core makes: Core set `m_last_write` in its call (`validation.cpp:3086` at
    /// v24.0.0), so the later calls are not due and carry nothing. e, dirty again while the job is
    /// in flight, stays dirty after the release, so its mark is stored. The index alone, a
    /// PERIODIC_FLUSH freeze and a DMAX freeze with the write due, then with no tip.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_b1_held_rows() {
        let hour = DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
        let day = DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
        let rows = [
            ("the index alone", hour, false, ("PERIODIC_WRITE", false)),
            ("PERIODIC_FLUSH", day, false, ("PERIODIC_FLUSH", true)),
            ("DMAX with the write due", hour, true, ("DMAX", true)),
        ];
        for (row, by, dmax, (cause, full)) in rows {
            let s = Stores::new(1 << 20);
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let tally = tally(&mut ctl);
            let mut index = ChainIndex::new();
            let mut v = v_ok(&s);
            flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::None, Knobs::default());
            index.dirty_e();
            v.now += by;
            if dmax {
                ctl.set_hub(s.hub.clone());
                s.hub.post_flush_due();
            }
            let periodic = |ctl: &mut Inline<'_, OsFs>, v: &mut V, index: &mut ChainIndex| {
                with_chain(v, index, &mut NoPrune, Knobs::default(), |cx| {
                    ctl.flush(FlushStateMode::Periodic, cx)
                })
                .unwrap();
            };
            periodic(&mut ctl, &mut v, &mut index);
            assert!(ctl.in_flight(), "{row}: held");
            index.dirty_e();
            for _ in 0..2 {
                v.now += Duration::from_secs(1);
                periodic(&mut ctl, &mut v, &mut index);
            }
            release(&mut ctl, &mut v, &mut index, &mut NoPrune);
            assert!(!ctl.in_flight(), "{row}: no second job");
            let counts = tally.since_open();
            let want = by_cause(&[(cause, 1)]);
            let (got, none) = if full {
                (counts.full, counts.index_only)
            } else {
                (counts.index_only, counts.full)
            };
            assert_eq!((got, none), (want, by_cause(&[])), "{row}: one write");
            index.mark_e();
            assert_eq!(index.dirty_len(), 1, "{row}: e dirty since the job's take");
            flush_to_release(
                &mut ctl,
                &mut v,
                &mut index,
                FlushStateMode::Always,
                Knobs::default(),
            );
            assert!(stored(&s, &index).status.has(Status::CONFLICT_CHAINLOCK), "{row}: stored");
        }

        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut index = ChainIndex::new();
        let mut v = s.v();
        let no_tip = |ctl: &mut Inline<'_, OsFs>, v: &mut V, index: &mut ChainIndex, mode| {
            without_tip_chain(v, index, |cx| ctl.flush_without_tip(mode, cx)).unwrap();
        };
        no_tip(&mut ctl, &mut v, &mut index, FlushStateMode::Periodic);
        index.dirty_e();
        v.now += hour;
        no_tip(&mut ctl, &mut v, &mut index, FlushStateMode::Periodic);
        assert!(ctl.in_flight(), "no tip: held");
        index.dirty_e();
        for _ in 0..2 {
            v.now += Duration::from_secs(1);
            no_tip(&mut ctl, &mut v, &mut index, FlushStateMode::Periodic);
        }
        without_tip_chain(&mut v, &mut index, |cx| ctl.stop_without_tip(cx)).unwrap();
        no_tip(&mut ctl, &mut v, &mut index, FlushStateMode::None);
        assert!(!ctl.in_flight(), "no tip: no second job");
        let want = by_cause(&[("PERIODIC_WRITE", 1)]);
        assert_eq!(tally.since_open().index_only, want, "no tip: one write");
        index.mark_e();
        assert_eq!(index.dirty_len(), 1, "no tip: e dirty since the job's take");
        no_tip(&mut ctl, &mut v, &mut index, FlushStateMode::Always);
        assert!(stored(&s, &index).status.has(Status::CONFLICT_CHAINLOCK), "no tip: stored");
    }

    /// The automatic prune host over `s`'s three files, its check armed by `arm`.
    fn files_host(s: &Stores<OsFs>) -> FilesPruneHost<OsFs> {
        FilesPruneHost {
            files: s.files.clone(),
            target: 1 << 20,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        }
    }

    /// An allocation's re-arm of the automatic check, as Core's `bs:721`.
    fn arm(s: &Stores<OsFs>) {
        s.files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .unwrap();
    }

    /// BL-819 (CH3b D244), P3's first trace through `FlushControl`: a node-own job in flight from
    /// 58 minutes, and Core's NONE call at 59 minutes meeting the armed check. The call waits for
    /// the job's release and makes the prune's job, waited for, so both are booked when it returns.
    /// Core pruned and wrote at 59 minutes, so its next write is due an hour after that call.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_waited_prune_row() {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let tip = block(19 + KEEP);
        // A coins share no PERIODIC check finds large.
        let mut v = s.v_with(1 << 34);
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = files_host(&s);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let mut index = ChainIndex::new();
        let min = Duration::from_secs(60);
        let t0 = v.now;
        let call = |ctl: &mut Inline<'_, OsFs>,
                    v: &mut V,
                    index: &mut ChainIndex,
                    host: &mut FilesPruneHost<OsFs>,
                    mode: FlushStateMode| {
            with_chain(v, index, host, Knobs::default(), |cx| ctl.flush(mode, cx)).unwrap();
        };
        call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::Periodic);
        assert!(!ctl.in_flight(), "no write right after startup");
        v.now = t0 + 58 * min;
        v.scripted = true;
        call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::None);
        v.scripted = false;
        assert!(ctl.in_flight(), "the node-own job");
        arm(&s);
        v.now = t0 + 59 * min;
        call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::None);
        assert!(!ctl.in_flight(), "the prune's job, made and waited for in the call");
        assert!(s.meta().pruned_files, "pruned");
        let want = CauseCounts {
            full: by_cause(&[("PRUNE", 1), ("SCRIPTED", 1)]),
            index_only: by_cause(&[]),
        };
        assert_eq!(tally.since_open(), want, "both jobs, in the call");
        v.now = t0 + 119 * min;
        call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::Periodic);
        assert!(!ctl.in_flight(), "not due an hour after the call, to the second");
        v.now = t0 + 119 * min + Duration::from_secs(1);
        call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::Periodic);
        assert!(ctl.in_flight(), "Core's write is due an hour after the call that met the check");
    }

    /// BL-819 (CH3b D244), P3's PERIODIC-call trace through `FlushControl`. A node-own job is
    /// in flight from two minutes before Core's day is out, and Core's PERIODIC call a second past
    /// it meets the armed check with PERIODIC_FLUSH due: Core's one flush there had both. When the
    /// peek selects, the call waits and makes one job with both; when it selects nothing, the
    /// check is taken there and the held PERIODIC_FLUSH is frozen at the poll after the release.
    #[test]
    fn each_job_takes_the_index_dirt_as_core_would_periodic_call_prune_row() {
        for selects in [true, false] {
            let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
            let tip = block(19 + KEEP);
            // A coins share no PERIODIC check finds large.
            let mut v = s.v_with(1 << 34);
            v.connect(0, 10, tip.height);
            v.step(tip);
            let mut host = files_host(&s);
            if !selects {
                host.target = 1 << 40;
            }
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            let tally = tally(&mut ctl);
            let mut index = ChainIndex::new();
            let t0 = v.now;
            let call = |ctl: &mut Inline<'_, OsFs>,
                        v: &mut V,
                        index: &mut ChainIndex,
                        host: &mut FilesPruneHost<OsFs>,
                        mode: FlushStateMode| {
                with_chain(v, index, host, Knobs::default(), |cx| ctl.flush(mode, cx)).unwrap();
            };
            call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::Periodic);
            v.now = t0 + DATABASE_FLUSH_INTERVAL - Duration::from_secs(120);
            v.scripted = true;
            call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::None);
            v.scripted = false;
            assert!(ctl.in_flight(), "the node-own job");
            arm(&s);
            v.now = t0 + DATABASE_FLUSH_INTERVAL + Duration::from_secs(1);
            call(&mut ctl, &mut v, &mut index, &mut host, FlushStateMode::Periodic);
            assert_eq!(ctl.in_flight(), !selects, "selects {selects}");
            assert!(!host.check_armed(), "selects {selects}: the check is taken at the call");
            if !selects {
                run_w(&mut ctl);
                v.now = t0 + DATABASE_FLUSH_INTERVAL + Duration::from_secs(60);
                with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| ctl.poll(cx))
                    .unwrap();
                assert!(ctl.in_flight(), "the held PERIODIC_FLUSH");
                release(&mut ctl, &mut v, &mut index, &mut host);
            }
            assert_eq!(s.meta().pruned_files, selects);
            let job = if selects {
                "PERIODIC_FLUSH|PRUNE"
            } else {
                "PERIODIC_FLUSH"
            };
            let want = CauseCounts {
                full: by_cause(&[(job, 1), ("SCRIPTED", 1)]),
                index_only: by_cause(&[]),
            };
            assert_eq!(tally.since_open(), want, "selects {selects}: {job}");
        }
    }

    /// V at the prune rows' tip over three files, Core's first call made, and a node-own job in
    /// flight; the prune host over the files and V's hub.
    fn a_node_own_job_in_flight(
        s: &Stores<OsFs>,
        ctl: &mut Inline<'_, OsFs>,
    ) -> (V, ChainIndex, FilesPruneHost<OsFs>) {
        let tip = block(19 + KEEP);
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = files_host(s);
        let mut index = ChainIndex::new();
        ctl.set_hub(s.hub.clone());
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::Periodic, cx)
        })
        .unwrap();
        v.scripted = true;
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::None, cx)
        })
        .unwrap();
        v.scripted = false;
        assert!(ctl.in_flight(), "the node-own job");
        (v, index, host)
    }

    /// BL-818 (CH3b D243, DEC-1566 Q4): a `FlushDue` kept at a Core call that met no armed check
    /// freezes at V's poll after the release, alone: the check armed since is left for Core's
    /// next call.
    #[test]
    fn a_poll_leaves_an_unmet_check_armed() {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let (mut v, mut index, mut host) = a_node_own_job_in_flight(&s, &mut ctl);
        s.hub.post_flush_due();
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| {
            ctl.flush(FlushStateMode::None, cx)
        })
        .unwrap();
        assert!(ctl.pending().contains(FlushCauses::DMAX), "kept for the job in flight");
        arm(&s);
        run_w(&mut ctl);
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| ctl.poll(cx)).unwrap();
        assert!(ctl.in_flight(), "the poll's freeze");
        release(&mut ctl, &mut v, &mut index, &mut host);
        let want = CauseCounts {
            full: by_cause(&[("DMAX", 1), ("SCRIPTED", 1)]),
            index_only: by_cause(&[]),
        };
        assert_eq!(tally.since_open(), want, "DMAX alone");
        assert!(host.check_armed(), "the check is left for Core's next call");
        assert!(!s.meta().pruned_files);
    }

    /// BL-818 (CH3b D243, DEC-1566 Q4): `answer_due`'s own flush for a `FlushDue` freezes DMAX
    /// alone, and leaves an armed check no Core call has met for Core's next call.
    #[test]
    fn answer_due_leaves_an_unmet_check_armed() {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let (mut v, mut index, mut host) = a_node_own_job_in_flight(&s, &mut ctl);
        release(&mut ctl, &mut v, &mut index, &mut host);
        arm(&s);
        s.hub.post_flush_due();
        with_chain(&mut v, &mut index, &mut host, Knobs::default(), |cx| ctl.answer_due(cx))
            .unwrap();
        assert!(ctl.in_flight(), "answer_due's own freeze");
        release(&mut ctl, &mut v, &mut index, &mut host);
        let want = CauseCounts {
            full: by_cause(&[("DMAX", 1), ("SCRIPTED", 1)]),
            index_only: by_cause(&[]),
        };
        assert_eq!(tally.since_open(), want, "DMAX alone");
        assert!(host.check_armed(), "the check is left for Core's next call");
        assert!(!s.meta().pruned_files);
    }

    /// V at block 1, Core's first call, and an index write half an hour before Core's day is out.
    fn a_day_but_half_an_hour(s: &Stores<OsFs>, ctl: &mut Inline<'_, OsFs>) -> (V, ChainIndex) {
        let mut index = ChainIndex::new();
        let mut v = v_ok(s);
        let start = v.now;
        flush_to_release(ctl, &mut v, &mut index, FlushStateMode::Periodic, Knobs::default());
        v.now = start + DATABASE_FLUSH_INTERVAL - DATABASE_WRITE_INTERVAL / 2;
        flush_to_release(ctl, &mut v, &mut index, FlushStateMode::Periodic, Knobs::default());
        v.now = start + DATABASE_FLUSH_INTERVAL - Duration::from_secs(1);
        v.scripted = true;
        flush_to_release(ctl, &mut v, &mut index, FlushStateMode::None, Knobs::default());
        v.scripted = false;
        v.now = start + DATABASE_FLUSH_INTERVAL;
        (v, index)
    }

    /// BL-818 (CH3b D243): a node-own freeze a second before Core's day is out does not put off
    /// Core's PERIODIC_FLUSH.
    #[test]
    fn a_node_own_freeze_does_not_put_off_the_daily_flush() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let (mut v, mut index) = a_day_but_half_an_hour(&s, &mut ctl);
        v.now += Duration::from_secs(1);
        flush_to_release(&mut ctl, &mut v, &mut index, FlushStateMode::Periodic, Knobs::default());
        let want = CauseCounts {
            full: by_cause(&[("PERIODIC_FLUSH", 1), ("SCRIPTED", 1)]),
            index_only: by_cause(&[("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want);
    }

    /// BL-818 (CH3b D243): with no tip, Core's overdue PERIODIC_FLUSH writes the index alone and
    /// leaves `m_last_flush` (`validation.cpp:3089` at v24.0.0), so each PERIODIC call writes the
    /// index again; a node-own freeze at a tip before it does not hide it.
    #[test]
    fn an_overdue_flush_with_no_tip_writes_the_index_at_each_periodic_call() {
        let s = Stores::new(1 << 20);
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let tally = tally(&mut ctl);
        let (mut v, mut index) = a_day_but_half_an_hour(&s, &mut ctl);
        for _ in 0..3 {
            v.now += Duration::from_secs(1);
            without_tip_chain(&mut v, &mut index, |cx| {
                ctl.flush_without_tip(FlushStateMode::Periodic, cx)
            })
            .unwrap();
            assert!(ctl.in_flight(), "an index-only write");
            without_tip_chain(&mut v, &mut index, |cx| ctl.stop_without_tip(cx)).unwrap();
        }
        let want = CauseCounts {
            full: by_cause(&[("SCRIPTED", 1)]),
            index_only: by_cause(&[("PERIODIC_FLUSH", 3), ("PERIODIC_WRITE", 1)]),
        };
        assert_eq!(tally.since_open(), want);
    }
}

/// BL-819: three files of ten blocks, file 0 to 9 and file 1 to 19, with V at `tip` holding ten
/// coins, its automatic check armed, and a host over the files with a 1 MiB target.
fn armed_prune_at(s: &Stores<OsFs>, tip: BlockRef) -> (V, FilesPruneHost<OsFs>) {
    let mut v = s.v();
    v.connect(0, 10, tip.height);
    v.step(tip);
    let host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    (v, host)
}

/// The prune marks of `s`'s observer: each `PruneSelected`'s files and each `PruneDeclined`'s.
fn prune_marks<F: FlatFs>(s: &Stores<F>) -> (Vec<Vec<u32>>, Vec<Vec<u32>>) {
    let (mut picked, mut declined) = (Vec::new(), Vec::new());
    for m in s.obs.marks() {
        match m {
            ObsMark::PruneSelected {
                files,
                ..
            } => picked.push(files),
            ObsMark::PruneDeclined {
                files,
                ..
            } => declined.push(files),
            _ => {}
        }
    }
    (picked, declined)
}

/// The `PruneDecision` marks of `s`'s observer: each one's files.
fn decisions<F: FlatFs>(s: &Stores<F>) -> Vec<Vec<u32>> {
    s.obs
        .marks()
        .into_iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                files,
                ..
            } => Some(files),
            _ => None,
        })
        .collect()
}

/// The starts of the selector's timer, "find files to prune".
fn selector_timers<F: FlatFs>(s: &Stores<F>) -> usize {
    let start = "FlushStateToDisk: find files to prune started";
    s.obs.lines(LogCat::Bench).iter().filter(|l| *l == start).count()
}

/// A non-prune job, SCRIPTED, frozen at V's tip and left in flight: the inline driver runs W only
/// when V waits.
fn scripted_job_in_flight<R: FlushDriver<Delta = ()>>(ctl: &mut Ctl<R>, v: &mut V) {
    v.scripted = true;
    v.with_cx(|cx| ctl.flush(FlushStateMode::None, cx)).unwrap();
    v.scripted = false;
    assert!(ctl.in_flight(), "J0 is in flight");
}

/// BL-819 FF3 (CH3b D244): an automatic prune at Core's NONE call is waited for, so V answers its
/// clears inside the call and nothing is in flight after it. A later rewind of V's tip and a
/// service point find nothing to answer.
#[test]
fn an_automatic_prune_is_cleared_inside_its_call() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let (mut v, mut host) = armed_prune_at(&s, block(19 + KEEP));
    let mut index = ClearingIndex::default();
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert!(!ctl.in_flight(), "the prune's job was waited for");
    assert_eq!(index.asked, [vec![0, 1]]);
    assert_eq!(prune_marks(&s), (vec![vec![0, 1]], vec![]));
    let rewound = block(12 + KEEP);
    v.coins.set_best_block(Some(rewound));
    v.step(rewound);
    let before = s.obs.marks().len();
    with_prune_cx(&mut v, &mut host, &mut index, None, |cx| ctl.service(cx)).unwrap();
    assert_eq!(s.obs.marks().len(), before, "no new mark");
    assert!((0..2).all(|n| !blk_on_disk(&s.cfg.blocks_dir, n)));
}

/// BL-819 FF5, the control half (CH3b D244): Core's NONE call meets a non-prune job in flight and
/// an armed check that selects nothing. The check is taken at the call under the selector's
/// timer, with one decision mark with no files, and the job stays in flight.
#[test]
fn an_armed_check_that_selects_nothing_in_flight_is_taken_there_in_control() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let (mut v, mut host) = armed_prune_at(&s, block(19 + KEEP));
    host.target = 1 << 40;
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    scripted_job_in_flight(&mut ctl, &mut v);
    let mut index = ClearingIndex::default();
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert!(ctl.in_flight(), "J0 is not waited for");
    assert_eq!(selector_timers(&s), 1, "one selector timer");
    assert_eq!(decisions(&s), [Vec::<u32>::new()], "one decision mark, no files");
    assert!(!s.files.read(|f| f.check_for_pruning()), "the check is taken");
}

/// A host whose automatic check is the files', and whose peek and dry run are failed assertions.
struct NoRuns(FilesPruneHost<OsFs>);

impl PruneHost for NoRuns {
    fn check_due(&mut self) -> bool {
        self.0.check_due()
    }

    fn check_armed(&self) -> bool {
        self.0.check_armed()
    }

    fn would_select(&self, _: u32, _: u32, _: bool) -> bool {
        panic!("no peek without a tip")
    }

    fn dry_run(&self, _: u32, _: u32, _: Option<u32>, _: bool) -> PruneDryRun {
        panic!("no dry run without a tip")
    }
}

/// BL-819 FF7 (CH3b D244, BL-818 open item n6): with no tip, Core's call clears an armed check
/// whatever is in flight (`FindFilesToPrune` returns at `Height() == -1`, `v:3028` at v24.0.0).
/// An index-only job J0 in flight: the check is taken under the selector's timer, with no peek,
/// no dry run and no wait for J0.
#[test]
fn a_no_tip_call_in_flight_takes_the_check() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut v = s.v();
    let start = v.now;
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Periodic, cx))
        .unwrap();
    v.now = start + DATABASE_WRITE_INTERVAL + Duration::from_secs(1);
    v.dirty.push(IndexWrite {
        hash: block(0).hash,
        bytes: vec![8; 12],
    });
    without_tip(&mut v, &mut NoPrune, |cx| ctl.flush_without_tip(FlushStateMode::Periodic, cx))
        .unwrap();
    assert!(ctl.in_flight(), "J0, the periodic index write, is in flight");
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut host = NoRuns(FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
        min_blocks_to_keep: KEEP,
    });
    without_tip(&mut v, &mut host, |cx| ctl.flush_without_tip(FlushStateMode::None, cx)).unwrap();
    assert!(ctl.in_flight(), "J0 is still in flight: no wait");
    assert!(!run_w(&mut ctl).is_empty(), "W had not run J0");
    assert!(!s.files.read(|f| f.check_for_pruning()), "the check is taken");
    assert_eq!(selector_timers(&s), 1, "under the selector's timer");
}

/// BL-819 FF8 (CH3b D244, DEC-1604 Q1 (a), the control half): Core's IF_NEEDED call at a
/// `DisconnectTip` site decides at T, the tip before the step, where file 0's last block is at
/// the bound T − keep; the freeze follows the step to T − 1. V answers the clears at the tip the
/// job froze at, so file 0 is cleared, as Core's selector at T selects it.
#[test]
fn a_prune_at_a_disconnect_site_is_cleared_at_the_tip_core_selects_at() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let t = block(9 + KEEP);
    let (mut v, mut host) = armed_prune_at(&s, t);
    let mut index = ClearingIndex::default();
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let pending = with_prune_cx(&mut v, &mut host, &mut index, None, |cx| {
        ctl.at_site(FlushStateMode::IfNeeded, cx)
    })
    .unwrap();
    assert_eq!(decisions(&s), [vec![0]], "decided at T: file 0 alone");
    let parent = block(t.height - 1);
    v.coins.set_best_block(Some(parent));
    let seq = v.step(parent);
    with_prune_cx(&mut v, &mut host, &mut index, None, |cx| {
        pending.freeze_after_step(&mut ctl, cx, parent, seq)
    })
    .unwrap();
    while ctl.in_flight() {
        run_w(&mut ctl);
        with_prune_cx(&mut v, &mut host, &mut index, None, |cx| ctl.service(cx)).unwrap();
    }
    assert_eq!(prune_marks(&s), (vec![vec![0]], vec![]), "cleared, not declined");
    assert_eq!(index.asked, [vec![0]]);
    assert!(!blk_on_disk(&s.cfg.blocks_dir, 0) && blk_on_disk(&s.cfg.blocks_dir, 1));
}

/// A host over the files whose peek answers `peek` and then runs `after_peek`, once: the seam
/// where a test moves the subscribers' locks between the peek and the logged run.
struct PeekSeam {
    host: FilesPruneHost<OsFs>,
    peek: Option<bool>,
    after_peek: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
}

impl PeekSeam {
    fn new(host: FilesPruneHost<OsFs>) -> Self {
        Self {
            host,
            peek: None,
            after_peek: std::cell::RefCell::default(),
        }
    }

    fn then(self, f: impl FnOnce() + 'static) -> Self {
        *self.after_peek.borrow_mut() = Some(Box::new(f));
        self
    }
}

impl PruneHost for PeekSeam {
    fn check_due(&mut self) -> bool {
        self.host.check_due()
    }

    fn check_armed(&self) -> bool {
        self.host.check_armed()
    }

    fn would_select(&self, tip: u32, bound: u32, is_ibd: bool) -> bool {
        let answer = self.peek.unwrap_or_else(|| self.host.would_select(tip, bound, is_ibd));
        if let Some(f) = self.after_peek.borrow_mut().take() {
            f();
        }
        answer
    }

    fn dry_run(&self, tip: u32, bound: u32, manual: Option<u32>, is_ibd: bool) -> PruneDryRun {
        self.host.dry_run(tip, bound, manual, is_ibd)
    }

    fn arm(&mut self) {
        self.host.arm();
    }
}

/// [`flush_pruning`] in `mode` with any prune host.
fn call_pruning<R: FlushDriver<Delta = ()>>(
    ctl: &mut Ctl<R>,
    v: &mut V,
    host: &mut dyn PruneHost,
    index: &mut ClearingIndex,
    mode: FlushStateMode,
) -> Result<(), Fatal> {
    with_prune_cx(v, host, index, None, |cx| ctl.flush(mode, cx))
}

/// The rows of the call's lock snapshot (Sol B-1): a main chain of 600 blocks over a tree index,
/// a hub with feeds over it, which V and W both read, and three files whose last blocks are 50,
/// 150 and 600, the last current. V is at block 600 with its automatic check armed.
struct LockRig {
    s: Stores<OsFs>,
    main: Vec<BlockRef>,
    feeds: crate::subscriber::ScriptedFeeds,
    latch: GlobalFailStop,
}

impl LockRig {
    fn new() -> Self {
        use crate::subscriber::{ScriptedFeeds, TreeIndex};
        let info = |first: u32, last: u32| dashd_store::flatfile::BlockFileInfo {
            blocks: last - first + 1,
            size: 1 << 20,
            undo_size: 1 << 10,
            height_first: first,
            height_last: last,
            time_first: 0,
            time_last: 0,
        };
        let state = BlockFilesState {
            infos: vec![info(0, 50), info(51, 150), info(151, 600)],
            last_file: 2,
        };
        let mut s = Stores::over_files(OsFs::new(), 1 << 20, state);
        let tree = TreeIndex::new();
        let g = tree.genesis();
        let main = std::iter::once(g).chain(tree.branch(g, 600)).collect::<Vec<_>>();
        let feeds = ScriptedFeeds::new(4, main.clone());
        s.hub = Arc::new(SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(tree)));
        Self {
            s,
            main,
            feeds,
            latch: GlobalFailStop::new(),
        }
    }

    /// A mandatory subscriber durable at `durable`, Synced at the tip if `live`.
    fn sub(
        &self,
        name: &'static str,
        durable: u32,
        live: bool,
    ) -> (crate::flush::hub::SubscriberHandle, Option<Box<dyn dash_chain::SubscriberFeed>>) {
        let r = crate::flush::Registration {
            name,
            needs: dash_chain::Needs::default(),
            mandatory: true,
            start_height: 1,
            durable: Some(self.main[durable as usize]),
        };
        let h = self.s.hub.register(r, &self.latch).expect("not latched").expect("registers");
        let feed = live.then(|| {
            let tip = self.feeds.tip().expect("a tip");
            match self.s.hub.table().expect("feeds").subscribe_at(h.id, &tip, &mut || Ok(())) {
                Ok(dash_chain::Handoff::Live {
                    feed,
                    ..
                }) => feed,
                _ => panic!("subscribe_at"),
            }
        });
        (h, feed)
    }

    /// V at block 600, the check armed, and the host over the files with a 1-byte target.
    fn v(&self) -> (V, FilesPruneHost<OsFs>) {
        let tip = self.main[600];
        let mut v = self.s.v();
        v.connect(0, 10, 600);
        v.coins.set_best_block(Some(tip));
        v.step(tip);
        let host = FilesPruneHost {
            files: self.s.files.clone(),
            target: 1,
            prune_after_height: 0,
            min_blocks_to_keep: KEEP,
        };
        self.s
            .files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .unwrap();
        (v, host)
    }
}

/// The `PruneDecision` marks' files and the `PruneSelected` marks' files, in order.
fn decided_and_selected<F: FlatFs>(s: &Stores<F>) -> (Vec<Vec<u32>>, Vec<Vec<u32>>) {
    (decisions(s), prune_marks(s).0)
}

/// BL-819 L1 (Sol B-1, CH3b D244): the peek and the logged run read one lock snapshot. A
/// mandatory Syncing subscriber durable at 100 bounds the call at 89, so file 0 (to 50) is
/// eligible and file 1 (to 150) is not. After the peek its locator moves to 300; the logged run
/// after the wait still selects file 0 alone, with one limited-pruning line, one timer line and
/// one decision mark.
#[test]
fn a_locator_that_advances_after_the_peek_does_not_widen_the_selection() {
    let rig = LockRig::new();
    let (sub, _) = rig.sub("sub", 100, false);
    let (mut v, host) = rig.v();
    let mut ctl = rig.s.inline(CrashRoll::seeded(0, 0), false);
    ctl.set_hub(rig.s.hub.clone());
    scripted_job_in_flight(&mut ctl, &mut v);
    let (hub, to) = (rig.s.hub.clone(), rig.main[300]);
    let mut host = PeekSeam::new(host).then(move || hub.set_durable(sub.id, to));
    let mut index = ClearingIndex::default();
    call_pruning(&mut ctl, &mut v, &mut host, &mut index, FlushStateMode::None).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(rig.s.hub.durable_locator(sub.id), Some(rig.main[300]), "the seam ran");
    assert_eq!(decided_and_selected(&rig.s), (vec![vec![0]], vec![vec![0]]));
    let limited = rig.s.obs.lines(LogCat::Prune);
    assert_eq!(limited, ["sub limited pruning to height 89"], "one limited-pruning line");
    assert_eq!(selector_timers(&rig.s), 1, "one timer line");
}

/// BL-819 L2 (Sol B-1, CH3b D244): a lock that would change after the peek doesn't change the
/// selection. A Synced subscriber durable at 100 is counted at the tip at the call; its feed
/// closes after the peek, which would lower its predicted lock to 100: D is still the peek's,
/// files 0 and 1, and step 7's actual locks then keep file 0 alone. A Syncing one durable at 100
/// deregisters after the peek, which would lift its lock: D is still file 0 alone, and so is
/// step 7's selection.
#[test]
fn a_close_or_deregistration_after_the_peek_does_not_change_the_selection() {
    for synced in [true, false] {
        let rig = LockRig::new();
        let (sub, feed) = rig.sub("sub", 100, synced);
        let (mut v, host) = rig.v();
        let mut ctl = rig.s.inline(CrashRoll::seeded(0, 0), false);
        ctl.set_hub(rig.s.hub.clone());
        scripted_job_in_flight(&mut ctl, &mut v);
        let hub = rig.s.hub.clone();
        let mut host = PeekSeam::new(host).then(move || match feed {
            Some(feed) => drop(feed),
            None => hub.deregister(sub.id),
        });
        let mut index = ClearingIndex::default();
        call_pruning(&mut ctl, &mut v, &mut host, &mut index, FlushStateMode::None).unwrap();
        assert!(!ctl.in_flight());
        let (d, picked) = decided_and_selected(&rig.s);
        if synced {
            assert_eq!(d, [vec![0, 1]], "the call's snapshot: Synced at the tip");
            assert_eq!(picked, [vec![0]], "step 7's actual lock keeps file 0 alone");
        } else {
            assert_eq!(d, [vec![0]], "the call's snapshot: the lock at 100");
            assert_eq!(picked, [vec![0]], "step 7, with no lock left, within D");
        }
    }
}

/// BL-819 L3 (Sol B-1, CH3b D244): a lock that allows nothing at the call (a Syncing subscriber
/// durable at 40, bound 29, below file 0's last block) gives an empty peek, so the check is taken
/// at the call with the logged run on the same snapshot, though the locator advances to 300 right
/// after the peek: no job, no wait, a decision mark with no files, and J0 still in flight.
#[test]
fn an_empty_peek_is_followed_by_an_empty_logged_run() {
    let rig = LockRig::new();
    let (sub, _) = rig.sub("sub", 40, false);
    let (mut v, host) = rig.v();
    let mut ctl = rig.s.inline(CrashRoll::seeded(0, 0), false);
    ctl.set_hub(rig.s.hub.clone());
    scripted_job_in_flight(&mut ctl, &mut v);
    let (hub, to) = (rig.s.hub.clone(), rig.main[300]);
    let mut host = PeekSeam::new(host).then(move || hub.set_durable(sub.id, to));
    let mut index = ClearingIndex::default();
    call_pruning(&mut ctl, &mut v, &mut host, &mut index, FlushStateMode::None).unwrap();
    assert!(ctl.in_flight(), "J0 still in flight: no wait");
    assert!(!rig.s.files.read(|f| f.check_for_pruning()), "the check is taken");
    assert_eq!(decided_and_selected(&rig.s), (vec![vec![]], vec![]), "an empty logged run");
    assert!(index.asked.is_empty());
}

/// BL-819 (CH3b D244, rule 2's fallback, P27): a peek that says empty and a logged run that
/// selects. The call waits and selects after the release; the call makes one decision mark, with
/// the freeze's causes and the files, and one IBD latch.
#[test]
fn a_fallback_run_waits_and_marks_once() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let (mut v, host) = armed_prune_at(&s, block(19 + KEEP));
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    scripted_job_in_flight(&mut ctl, &mut v);
    let mut host = PeekSeam::new(host);
    host.peek = Some(false);
    let mut index = ClearingIndex::default();
    call_pruning(&mut ctl, &mut v, &mut host, &mut index, FlushStateMode::None).unwrap();
    assert!(!ctl.in_flight());
    let marks: Vec<_> = s
        .obs
        .marks()
        .into_iter()
        .filter_map(|m| match m {
            ObsMark::PruneDecision {
                files,
                causes,
                ..
            } => Some((files, causes)),
            _ => None,
        })
        .collect();
    assert_eq!(marks, [(vec![0, 1], FlushCauses::PRUNE)], "exactly one PruneDecision mark");
    assert_eq!(index.latches, 1, "one latch");
    assert_eq!(index.asked, [vec![0, 1]]);
}

/// BL-819 (CH3b D244, P28): an ALWAYS call in flight with an armed check that selects nothing.
/// The check is taken at the call with one decision mark with no files and one IBD latch, where
/// Core latches (`validation.cpp:3027` at v24.0.0); the call then waits for ALWAYS and freezes it
/// without PRUNE, and makes no second mark.
#[test]
#[cfg(feature = "test-support")]
fn an_empty_peek_at_an_always_call_keeps_its_mark() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let (mut v, mut host) = armed_prune_at(&s, block(19 + KEEP));
    host.target = 1 << 40;
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let tally = tally(&mut ctl);
    scripted_job_in_flight(&mut ctl, &mut v);
    let mut index = ClearingIndex::default();
    call_pruning(&mut ctl, &mut v, &mut host, &mut index, FlushStateMode::Always).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(decisions(&s), [Vec::<u32>::new()], "exactly one mark, no files");
    assert_eq!(index.latches, 1, "one latch");
    let want = CauseCounts {
        full: by_cause(&[("ALWAYS", 1), ("SCRIPTED", 1)]),
        index_only: by_cause(&[]),
    };
    assert_eq!(tally.since_open(), want, "ALWAYS without PRUNE");
}

/// BL-819 (CH3b D244, P19): a Core call in flight whose peek selects marks nothing and latches
/// nothing before the wait ends: the call's one mark, latch and timer line come from the decision
/// after J0's release.
#[test]
fn a_peek_marks_nothing_and_latches_nothing() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let (mut v, mut host) = armed_prune_at(&s, block(19 + KEEP));
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    scripted_job_in_flight(&mut ctl, &mut v);
    let mut index = ClearingIndex::default();
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert!(!ctl.in_flight());
    let marks = s.obs.marks();
    let at = |want: &dyn Fn(&ObsMark) -> bool| marks.iter().position(want).expect("a mark");
    let j0_durable = at(&|m| {
        matches!(
            m,
            ObsMark::Durable {
                flush: 1
            }
        )
    });
    let decision = at(&|m| matches!(m, ObsMark::PruneDecision { .. }));
    assert!(j0_durable < decision, "no mark before the wait ends");
    assert_eq!(decisions(&s), [vec![0, 1]], "exactly one mark");
    assert_eq!(index.latches, 1, "one latch");
    assert_eq!(selector_timers(&s), 1, "one timer line");
}

/// The answer W got to a `PruneClears` V handled.
fn answer(wait: &crate::flush::clears::ClearsWait) -> Option<crate::flush::ClearsAnswer> {
    match wait.poll() {
        crate::flush::clears::Polled::Ready(a) => Some(a),
        _ => None,
    }
}

/// BL-819 (CH3b D244, DEC-1604 Q1): V answers a prune's clears only at the tip the job froze at,
/// by hash and height; any other answer declines, as a guard. Height: the index's check reads
/// another height. No tip: it reads none. Hash: V's working tip is a sibling of the frozen tip at
/// its height, through `handle` directly. And with no tip at V, the no-tip service declines. Each
/// arm: the request reached V, the answer was `Declined`, `prune_clear` was not called, and the
/// latch is clear.
#[test]
fn a_clears_request_at_another_tip_is_declined() {
    for (arm, check) in [("height", Some(Some(19 + KEEP - 1))), ("no tip", Some(None))] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let (mut v, mut host) = armed_prune_at(&s, block(19 + KEEP));
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        let mut index = ClearingIndex {
            check_height: check,
            ..ClearingIndex::default()
        };
        flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
        assert!(!ctl.in_flight(), "{arm}");
        assert_eq!(prune_marks(&s), (vec![], vec![vec![0, 1]]), "{arm}: declined");
        assert!(index.asked.is_empty(), "{arm}: no prune_clear");
        assert!(s.db.latch().tripped().is_none(), "{arm}: the latch is clear");
    }

    let tip = block(19 + KEEP);
    let mut sibling = tip;
    sibling.hash = BlockHash::from([0x5a; 32]);
    for arm in ["hash", "no-tip service"] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
        ctl.in_flight = Some(InFlight {
            flush_id: 1,
            signal: Some(tip),
            #[cfg(feature = "test-support")]
            booked: Booked {
                full: true,
                causes: FlushCauses::PRUNE,
                label: None,
                stamp: Stamp::default(),
                forced: false,
                kept: true,
            },
        });
        let rule = crate::flush::PruneRule::Automatic {
            after: 0,
            keep: KEEP,
        };
        let (req, wait) = crate::flush::clears::PruneClears::new(1, vec![0], 9, rule);
        let mut v = s.v();
        v.tip = sibling;
        let mut index = ClearingIndex::default();
        if arm == "hash" {
            let mut host = NoPrune;
            with_prune_cx(&mut v, &mut host, &mut index, None, |cx| {
                ctl.handle(DriverEvent::PruneClears(req), cx)
            })
            .unwrap();
        } else {
            without_tip(&mut v, &mut NoPrune, |cx| {
                ctl.handle_without_tip(DriverEvent::PruneClears(req), cx.coins, &mut *cx.evo)
            })
            .unwrap();
        }
        assert_eq!(answer(&wait), Some(crate::flush::ClearsAnswer::Declined), "{arm}");
        assert!(index.asked.is_empty(), "{arm}: no prune_clear");
        assert!(s.db.latch().tripped().is_none(), "{arm}: the latch is clear");
    }
}

/// BL-819 (CH3b D244, DEC-1604 Q1): the index's check returns V's working height, the frozen
/// tip's, as `FlushHost` now does whatever the published tip is (a lower published tip is
/// `the_clears_height_inside_a_forward_hold_is_the_working_tips`'s), and the real `FlushControl`
/// clears inside the call. FF3 has the hook's default; this row pins the explicit height.
#[test]
fn a_prune_with_published_below_working_is_cleared() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + KEEP);
    let (mut v, mut host) = armed_prune_at(&s, tip);
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    let mut index = ClearingIndex {
        check_height: Some(Some(tip.height)),
        ..ClearingIndex::default()
    };
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(prune_marks(&s), (vec![vec![0, 1]], vec![]), "cleared");
    assert_eq!(index.asked, [vec![0, 1]]);
}
