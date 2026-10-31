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
    BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs, FlatRead, MIN_BLOCKS_TO_KEEP, OsFs,
};
use dashd_store::{KvRead as _, MemoryEngine};

use super::*;
use crate::coins::{CoinSrc, CoinsBudget, OutPointKey, Ratio, Salt};
use crate::flush::test_files::{blk_on_disk, create_on_disk, equal_files};
use crate::flush::{
    CrashRoll, DATABASE_WRITE_INTERVAL, FilesPruneHost, FlushCauses, FlushConfig, FlushEnv,
    FlushObserver, FlushSignals, FlushStep, InlineDriver, LogCat, NoPrune, ObsMark,
    RecordingObserver, RecordingSignals, SubscriberHub, ThreadDriver, WThread,
};
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
        let db =
            Arc::new(GuardedEngine::new(engine(dir.path()), GlobalFailStop::new(), sink.clone()));
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
            let out = TxOut {
                value: 1_000 + i64::from(i),
                script_pubkey: vec![0x51; 25].into(),
            };
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
/// durable, and names the pre-step tip.
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
    assert_eq!(s.signals.tips(), [block(0)]);
    assert!(!ctl.in_flight());
    assert_eq!((v.coins.durable(), s.meta().tip), (Some(block(1)), Some(block(1).hash)));
}

/// A disconnect of d freezes at `(parent(d), seq)`, and its signal names d, as Core's does at
/// `DisconnectTip` (E-b1.4's exception).
#[test]
fn a_disconnect_freezes_at_the_parent_and_signals_the_disconnected_block() {
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
    assert_eq!(s.signals.tips(), [block(2), block(2)]);
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
    assert_eq!(s.signals.tips(), [block(0), block(2)]);
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

/// Core signals only after a full flush with a best block (`validation.cpp:3078-3114`): on a fresh
/// datadir, before the first connect, an ALWAYS flush (at startup or shutdown) writes the block
/// index alone and sends nothing.
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
    assert_eq!(s.signals.tips(), [block(0), block(2)]);
    v.with_cx(|cx| ctl.poll(cx)).unwrap();
    assert!(!ctl.in_flight(), "and only once");
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
    assert_eq!(s.signals.tips(), [block(0)]);
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

/// Core sets `m_last_write` at each block-index write (`validation.cpp:3030, 3075`): V restarts
/// the timer when W releases a write, so PERIODIC writes the index an hour after the last write,
/// not at every call an hour after the first.
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
/// only there (`validation.cpp:2960-2961`); V logs it with native Σ and B (CH2 §3.3, §3.6). NONE
/// freezes nothing, so V decides at every boundary on the way up: with f = 19/20, LARGE is not
/// hidden by THRESHOLD (f·B or a shard's trigger).
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
    assert_eq!(s.signals.tips(), [block(0), block(2)]);
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
struct TripOn {
    line: &'static str,
    latch: GlobalFailStop,
    failure: Failure,
    marks: RecordingObserver,
}

impl FlushObserver for TripOn {
    fn mark(&self, m: ObsMark) {
        if matches!(&m, ObsMark::Log { line, .. } if line == self.line) {
            self.latch.trip(self.failure.clone());
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
    assert_eq!(s.signals.tips(), [block(0), block(1)]);
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
}

impl FlushIndex for ClearingIndex {
    fn take_dirty_into(&mut self, _: &mut dyn FnMut(&BlockHash, &[u8])) {}

    fn prune_clear(&mut self, files: &[u32]) -> Result<Vec<IndexWrite>, String> {
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

/// V's poll with the prune tests' index.
fn poll_pruning<R: FlushDriver<Delta = ()>, F: FlatFs>(
    ctl: &mut Ctl<R>,
    v: &mut V,
    host: &mut FilesPruneHost<F>,
    index: &mut ClearingIndex,
) -> Result<(), Fatal> {
    with_prune_cx(v, host, index, None, |cx| ctl.poll(cx))
}

fn with_prune_cx<F: FlatFs, T>(
    v: &mut V,
    host: &mut FilesPruneHost<F>,
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
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, state.clone());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 0,
            prune_after_height: 0,
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

/// An automatic prune does not wait for its release: V answers W's `PruneClears` at its poll,
/// over each driver, and W then writes the clears, releases and unlinks.
#[test]
fn v_answers_an_automatic_prunes_clears_at_its_poll() {
    let state = three_files();
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, state.clone());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 1 << 20,
            prune_after_height: 0,
        };
        s.files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .unwrap();
        let mut index = ClearingIndex::default();
        if threaded {
            let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            let deadline = Instant::now() + Duration::from_secs(120);
            while ctl.in_flight() {
                assert!(Instant::now() < deadline, "W releases");
                poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
                std::thread::yield_now();
            }
            // W's thread ends after the unlink.
            drop(ctl);
        } else {
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            assert!(ctl.in_flight());
            let w = run_w(&mut ctl);
            assert_eq!(w.last(), Some(&FlushStep::PruneSelect), "W waits for V");
            assert!(index.asked.is_empty());
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            use FlushStep::*;
            let w = run_w(&mut ctl);
            assert_eq!(w, [PruneApply, PruneBarrier, Bp, Release, AfterBk, Unlink]);
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            assert!(!ctl.in_flight());
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

/// DEC-211, over each driver: a rewind on V between W's selection and V's answer drops V's tip
/// below what the selection needs, so V declines at its own tip, the working one. W prunes
/// nothing (no clears, no Bp, no unlink) and re-arms the automatic check, so the next flush
/// selects at V's tip: file 0 alone.
#[test]
fn v_declines_clears_its_rewound_tip_no_longer_allows() {
    let state = three_files();
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    // File 1 holds blocks up to 19: past this tip's bound of 12.
    let rewound = block(12 + MIN_BLOCKS_TO_KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, state.clone());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 1 << 20,
            prune_after_height: 0,
        };
        s.files
            .mutate(|f| {
                f.rearm_check_for_pruning();
                Ok(())
            })
            .unwrap();
        let mut index = ClearingIndex::default();
        let on_disk = |n: u32| blk_on_disk(&s.cfg.blocks_dir, n);
        // V's rewind: its coins and its tip, as disconnects leave them.
        let rewind = |v: &mut V| {
            v.coins.set_best_block(Some(rewound));
            v.step(rewound);
        };
        if threaded {
            let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
            let mut ctl = s.control(driver, false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            rewind(&mut v);
            let deadline = Instant::now() + Duration::from_secs(120);
            while ctl.in_flight() {
                assert!(Instant::now() < deadline, "W releases");
                poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
                std::thread::yield_now();
            }
            assert!(index.asked.is_empty());
            assert!(!s.meta().pruned_files);
            assert!((0..3).all(on_disk));
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            // Each flush gets its own deadline, so load can't fail the second (Sol r2 3).
            let deadline = Instant::now() + Duration::from_secs(120);
            while ctl.in_flight() {
                assert!(Instant::now() < deadline, "W releases the second flush");
                poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
                std::thread::yield_now();
            }
            drop(ctl);
        } else {
            let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            assert_eq!(run_w(&mut ctl).last(), Some(&FlushStep::PruneSelect));
            rewind(&mut v);
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            use FlushStep::*;
            assert_eq!(run_w(&mut ctl), [PruneApply, Release, AfterBk]);
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            assert!(!ctl.in_flight());
            assert!(index.asked.is_empty());
            let meta = s.meta();
            assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), false));
            assert!((0..3).all(on_disk));
            flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
            assert_eq!(run_w(&mut ctl).last(), Some(&PruneSelect), "the check was re-armed");
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            assert_eq!(run_w(&mut ctl), [PruneApply, PruneBarrier, Bp, Release, AfterBk, Unlink]);
            poll_pruning(&mut ctl, &mut v, &mut host, &mut index).unwrap();
            assert!(!ctl.in_flight());
        }
        assert_eq!(index.asked, [vec![0]], "threaded {threaded}");
        let meta = s.meta();
        assert_eq!((meta.tip, meta.pruned_files), (Some(rewound.hash), true));
        assert_eq!((on_disk(0), on_disk(1), on_disk(2)), (false, true, true), "{threaded}");
        assert_eq!(s.sink.marks(), []);
    }
}

/// CH2b b4 D55 (CH1-node D47 item 6): with no tip at V, its service declines W's `PruneClears`
/// without asking the index, and takes the release. W prunes nothing.
#[test]
fn v_without_a_tip_declines_the_clears_and_takes_the_release() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    let mut v = s.v();
    v.connect(0, 10, tip.height);
    v.step(tip);
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut index = ClearingIndex::default();
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert_eq!(run_w(&mut ctl).last(), Some(&FlushStep::PruneSelect));
    without_tip(&mut v, &mut NoPrune, |cx| ctl.service_without_tip(cx)).unwrap();
    use FlushStep::*;
    assert_eq!(run_w(&mut ctl), [PruneApply, Release, AfterBk]);
    assert_eq!((v.evo.takes, v.evo.releases), (1, 0));
    without_tip(&mut v, &mut NoPrune, |cx| ctl.service_without_tip(cx)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!((v.evo.takes, v.evo.releases), (1, 1), "the no-tip service releases the window");
    assert!(index.asked.is_empty());
    let meta = s.meta();
    assert_eq!((meta.tip, meta.pruned_files), (Some(tip.hash), false));
    assert!((0..3).all(|n| blk_on_disk(&s.cfg.blocks_dir, n)));
    assert_eq!(s.sink.marks(), []);
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
/// (`v:3053-3074`): the block files flushed and the block index written, no coins, no
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

/// b4 D55 (Opus r1 m-2): with no tip an armed automatic prune check is taken under its timer,
/// as Core's `FindFilesToPrune` returns at `chain_tip_height < 0` and `:3017` clears the flag;
/// nothing is selected or written.
#[test]
fn an_armed_prune_check_with_no_tip_is_cleared_under_its_timer() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut v = s.v();
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
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

/// b4 D57: `PruneAndFlush` with no tip arms the check, and its NONE flush takes it under the
/// selector's timer, selecting nothing (`v:3141-3148` at `Height() == -1`).
#[test]
fn prune_and_flush_with_no_tip_arms_and_clears_the_check() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let mut v = s.v();
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
    };
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    without_tip(&mut v, &mut host, |cx| ctl.prune_and_flush_without_tip(cx)).unwrap();
    assert!(!ctl.in_flight());
    assert_eq!(prune_timer(&s), "find files to prune");
    assert!(!host.check_due(), "the check was cleared");
    assert!((0..3).all(|n| blk_on_disk(&s.cfg.blocks_dir, n)));
}

/// Once a failure has latched, V clears nothing for W: W writes nothing more, so the clears would
/// leave V's index short of blocks still on disk.
#[test]
fn v_clears_nothing_once_a_failure_has_latched() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    let mut v = s.v();
    v.connect(0, 10, tip.height);
    v.step(tip);
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut index = ClearingIndex::default();
    let mut ctl = s.inline(CrashRoll::seeded(0, 0), false);
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert_eq!(run_w(&mut ctl).last(), Some(&FlushStep::PruneSelect));
    s.db.latch().trip(Failure {
        kind: ErrorKind::Other,
        site: "test",
        message: "a failure elsewhere".into(),
    });
    assert!(poll_pruning(&mut ctl, &mut v, &mut host, &mut index).is_err());
    assert!(index.asked.is_empty());
    assert_eq!(run_w(&mut ctl), [], "W is refused");
    assert!(!s.meta().pruned_files);
    assert!(blk_on_disk(&s.cfg.blocks_dir, 0));
}

/// V's index fails to clear: V stops the node with its own message, W writes nothing of the
/// prune and stops without a second report, and no file is removed.
#[test]
fn a_failed_clear_on_v_stops_the_flush_with_vs_message() {
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    for threaded in [false, true] {
        let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
        let mut v = s.v();
        v.connect(0, 10, tip.height);
        v.step(tip);
        let mut host = FilesPruneHost {
            files: s.files.clone(),
            target: 0,
            prune_after_height: 0,
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

/// V drops the thread driver while W waits for its `PruneClears` answer, or is about to ask: the
/// request is dropped, W's flush stops with nothing pruned, and the drop returns.
#[test]
#[expect(clippy::disallowed_methods, reason = "a plain test thread that drops V's side")]
fn dropping_the_thread_driver_ends_ws_wait_for_the_clears() {
    let s = Stores::over_files(OsFs::new(), 1 << 20, three_files());
    let tip = block(19 + MIN_BLOCKS_TO_KEEP);
    let mut v = s.v();
    v.connect(0, 10, tip.height);
    v.step(tip);
    let mut host = FilesPruneHost {
        files: s.files.clone(),
        target: 1 << 20,
        prune_after_height: 0,
    };
    s.files
        .mutate(|f| {
            f.rearm_check_for_pruning();
            Ok(())
        })
        .unwrap();
    let mut index = ClearingIndex::default();
    let driver = w_thread(&s, s.obs.clone(), CrashRoll::seeded(0, 0), Box::new(|| {}));
    let mut ctl = s.control(driver, false);
    // An automatic prune: the freeze does not wait for its release.
    flush_pruning(&mut ctl, &mut v, &mut host, &mut index, None).unwrap();
    assert!(ctl.in_flight());
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
    use dash_primitives::{LlmqType, QuorumHash};

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
