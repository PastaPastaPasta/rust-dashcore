//! BL-683: a fault reading a block's `mnhf` row, which versionbits read through
//! `CMNHFManager::GetForBlock` (DEC-1028), on V's paths over the node's connector and `NodeEvo`:
//! the tip's connect, a proposal, the RPC report's stage and walk, the O8 recorder's walk, H4's
//! `ProcessBlock` and H9's `UndoBlock`. Core's `GetForBlock` throws out of its caller
//! (`mnhftx.cpp:285-321`) before versionbits caches anything (`versionbits.cpp:38`); V stops
//! with D5's `Fatal`, never a panic, and the fault is reported once: an engine error by the
//! guarded engine as it trips the latch, a row that does not decode (BL-274's bit out of range
//! included) the same way through its corrupt-row report.
//!
//! The chains are dashd's recorded regtest runs (`test-vectors/statedigest/regtest-v24`), V20 at
//! 100, so every block from 100 stores an `mnhf` row. `feature_asset_locks` mines V24's signal at
//! 188: V24 starts at 250, locks in at 500 and is active from 750. The rig's evo layer is flushed
//! before a restart, so the restarted node, whose MNHF manager is cold, reads each row from the
//! engine; V's clock is two days past the run, so V is in initial block download throughout and
//! `UpdateTip`'s warning walk reads no row. A fault is armed from inside the phase it targets
//! ([`Gate`]): the EHF read of one block, the record of one connect, the connect or disconnect
//! of one block. `VerifyBestBlock`'s evo read comes before versionbits' (`connect/serial.rs`), so
//! a fault armed before the connect would meet that read. Each window logs the `mnhf` reads it
//! saw, so a test checks the row and the one injected failure, and runs a clean control through
//! the same wrapper.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use dash_chain::{ChainReader, FailStop};
use dash_consensus::deployments::{EhfSignals, ThresholdState};
use dash_consensus::header::HeaderParams;
use dash_consensus::{Block, BlockId};
use dash_dml::mnhf::GetForBlockError;
use dash_params::ChainSetup;
use dash_primitives::BlockHash;
use dashd_store::flatfile::{BlockFiles as StoreFiles, BlockFilesConfig, BlockFilesState, OsFs};
use dashd_store::{Commit, ErrorKind, KvBatch as _, KvRead as _};

use super::connect::Latched;
use super::ehf::{PERIOD, core_state};
use super::evo_chain::linked;
use super::evo_chain_cl::{run_blocks, run_params_with};
use super::support::{
    Mined, Rig, RigConfig, assert_logged_before, install_accept_log, levels_on, lines_on,
};
use crate::chain::reply::{WaitError, panic_message};
use crate::chain::{
    AbortPath, AbortSink, BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome,
    DisconnectResult, EhfRows, EvoReads, Fatal, LocalShutdown, ProposalChecks, RecordingAbort,
    ScratchParent, ScratchView, VContext, VExit, ViewTarget,
};
use crate::checkqueue::CheckQueue;
use crate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use crate::connect::{ChainstateCell, ConnectorParts, CoreConnector, DigestSink, NodeHelper};
use crate::evo::RocksEvoStore;
use crate::evo::cl::AttemptClVerifier;
use crate::evo::codec::EvoCf;
use crate::evo::effects::{
    EffectAppliers, FacadeMineable, MemPlatformBans, MnListSignal, StoreSnapshots,
    UnreachableIsLock,
};
use crate::evo::ev1::DmlChecks;
use crate::evo::node::{
    FileBlocks, NodeEvo, NodeEvoParts, SharedEvoLayer, node_codec, node_net_info,
};
use crate::evo::testkit;
use crate::flush::EffectLedger;
use crate::llmq::facade::MembersFacade;
use crate::llmq::rows::RuntimeRows;
use crate::sigcache::{CacheSizes, ScriptCaches};
use crate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles, ProbeEngine,
    RowRead,
};

/// dashd's `feature_asset_locks.py` run: V20 at 100, V24's MNHF signal at 188, 978 blocks.
const ASSET_LOCKS: &str = "feature_asset_locks";
/// dashd's `feature_masternode_shares.py` run: V20 at 100, V24 and EVO_SHARES without EHF
/// (`-vbparams` useEHF 0), so no deployment reads an `mnhf` row.
const MASTERNODE_SHARES: &str = "feature_masternode_shares";
/// V24's signal in [`ASSET_LOCKS`].
const SIGNAL: u32 = 188;

/// A recorded run: its params, its blocks (element `i` at height `i + 1`) and V's clock.
struct Run {
    p: Arc<ChainSetup>,
    blocks: Vec<Mined>,
    after_genesis: i64,
}

impl Run {
    /// `test`'s run, V's clock two days past its last block.
    fn of(test: &str) -> Run {
        Run::with(test, &[])
    }

    /// [`Run::of`], its params with `extra` chain arguments.
    fn with(test: &str, extra: &[&str]) -> Run {
        let p = run_params_with(test, extra);
        let raw = run_blocks(test);
        let time = |b: &Block| i64::from(b.header().raw().time());
        let after_genesis = time(raw.last().expect("blocks")) - time(&raw[0]) + 2 * 86_400;
        let genesis = Mined::genesis(&p);
        assert_eq!(genesis.hash(), raw[0].hash(), "{test}: the run's genesis");
        let blocks = linked(&genesis, raw[1..].to_vec());
        Run {
            p,
            blocks,
            after_genesis,
        }
    }

    /// The block at `h`.
    fn at(&self, h: u32) -> &Mined {
        &self.blocks[h as usize - 1]
    }
}

/// The phase a window opens in: one block's EHF read through the connector's rows, the record
/// of one block's connect, or the connector's connect or disconnect of one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gate {
    Read(BlockId),
    #[cfg(feature = "statedigest")]
    Record(BlockHash),
    Connect(BlockHash),
    Disconnect(BlockId),
}

/// The arm a test sets: the phase that opens the window, the row whose reads the window logs,
/// and the failure the row's next read meets, if any.
#[derive(Clone, Copy, Debug)]
struct Arm {
    gate: Gate,
    row: BlockHash,
    fail: Option<ErrorKind>,
}

/// One opened window: the `mnhf` reads inside it, and whether the armed failure was still armed
/// as it closed (no read met it).
#[derive(Clone, Debug)]
struct Window {
    row: BlockHash,
    reads: Vec<RowRead>,
    left_armed: bool,
}

impl Window {
    /// The window read the row once, and nothing else of the `mnhf` family; the armed failure
    /// answered that read when `failed`, and none was left.
    fn assert_read_once(&self, failed: bool) {
        self.assert_reads(&[(self.row, failed)]);
    }

    /// The window's `mnhf` reads were `want`'s rows, in order, and nothing else; the armed
    /// failure answered each read marked `true`, and none was left.
    fn assert_reads(&self, want: &[(BlockHash, bool)]) {
        let want: Vec<RowRead> = want
            .iter()
            .map(|(row, failed)| RowRead {
                key: EvoCf::Mnhf.key(row).as_bytes().to_vec(),
                failed: *failed,
            })
            .collect();
        assert_eq!(self.reads, want, "the window's mnhf reads");
        assert!(!self.left_armed, "a failure left armed");
    }
}

/// The test's switch, shared by the wrappers and the test: one arm at a time, each opening one
/// window at most, on the engine's probe.
struct Watch {
    db: Arc<GuardedEngine<ProbeEngine>>,
    armed: Mutex<Option<Arm>>,
    windows: Mutex<Vec<Window>>,
    /// Set by a test: the next connect runs inside a startup verify region on V's thread, whose
    /// record and outcome go to [`Watch::in_region`].
    region: AtomicBool,
    in_region: Mutex<Option<InRegion>>,
}

/// What a connect inside a startup verify region left: its outcome, the region's fault, and
/// whether a hook's `try` caught a read.
#[derive(Debug)]
struct InRegion {
    out: Result<ConnectOutcome, Fatal>,
    fault: Option<String>,
    caught: bool,
}

impl Watch {
    fn arm(&self, gate: Gate, row: BlockHash, fail: Option<ErrorKind>) {
        *self.armed.lock().expect("watch") = Some(Arm {
            gate,
            row,
            fail,
        });
    }

    /// `f`, inside a window if the arm is for `gate`: the arm is taken, so no later call opens
    /// another, and the row's failure, if any, is disarmed as the window closes, an unwind
    /// through it included.
    fn window<T>(&self, gate: Gate, f: impl FnOnce() -> T) -> T {
        let arm = self.armed.lock().expect("watch").take_if(|a| a.gate == gate);
        let Some(arm) = arm else {
            return f();
        };
        let probe = self.db.inner();
        probe.watch_rows(EvoCf::Mnhf.cf());
        if let Some(kind) = arm.fail {
            probe.fail_row_once(EvoCf::Mnhf.cf(), EvoCf::Mnhf.key(&arm.row).as_bytes(), kind);
        }
        let _open = Open {
            watch: self,
            row: arm.row,
        };
        f()
    }

    /// The one window opened since the last call; no arm is left unopened.
    fn take_one(&self) -> Window {
        assert!(self.armed.lock().expect("watch").is_none(), "an arm no phase opened");
        let mut windows = std::mem::take(&mut *self.windows.lock().expect("watch"));
        assert_eq!(windows.len(), 1, "{windows:?}");
        windows.pop().expect("one window")
    }
}

/// An open window, closed as it drops.
struct Open<'a> {
    watch: &'a Watch,
    row: BlockHash,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        let probe = self.watch.db.inner();
        let left_armed = probe.disarm_row();
        let reads = probe.take_row_reads();
        let mut windows = self.watch.windows.lock().unwrap_or_else(PoisonError::into_inner);
        windows.push(Window {
            row: self.row,
            reads,
            left_armed,
        });
    }
}

/// The connector's rows, read inside the [`Watch`]'s window for the armed block.
struct Watched {
    inner: Arc<dyn EhfRows>,
    watch: Arc<Watch>,
}

impl EhfRows for Watched {
    fn get_for_block(
        &self,
        reader: &ChainReader,
        block: BlockId,
    ) -> Result<EhfSignals, GetForBlockError> {
        self.watch.window(Gate::Read(block), || self.inner.get_for_block(reader, block))
    }
}

/// The node's connector whose rows are [`Watched`] and whose connects and disconnects open the
/// [`Watch`]'s windows.
struct Forward {
    inner: Box<dyn BlockConnector>,
    rows: Option<Arc<dyn EhfRows>>,
    watch: Arc<Watch>,
}

impl BlockConnector for Forward {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        let inner = &mut self.inner;
        let region =
            self.watch.region.swap(false, Ordering::SeqCst).then(|| self.watch.db.startup_verify());
        let out = self.watch.window(Gate::Connect(block.hash()), || {
            inner.connect_block(cx, v, target, block, checked, id, st)
        });
        if let Some(region) = region {
            *self.watch.in_region.lock().expect("watch") = Some(InRegion {
                out: out.clone(),
                fault: region.fault(),
                caught: region.caught(),
            });
        }
        out
    }

    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        let inner = &mut self.inner;
        self.watch.window(Gate::Disconnect(id), || inner.disconnect_block(cx, v, target, block, id))
    }

    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        self.inner.test_block_validity(cx, v, checks, block, prev, check_pow, check_merkle)
    }

    fn published_bls(&self) -> Option<dash_consensus::BlsCtx> {
        self.inner.published_bls()
    }

    fn evo_reads(&self) -> &dyn EvoReads {
        self.inner.evo_reads()
    }

    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        self.rows.clone()
    }

    fn verify_scratch(
        &mut self,
        parent: ScratchParent,
    ) -> Result<Option<Box<dyn ScratchView>>, Fatal> {
        self.inner.verify_scratch(parent)
    }

    fn take_scratch_refusal(&mut self) -> Option<String> {
        self.inner.take_scratch_refusal()
    }

    fn take_undo_fault(&mut self) -> Option<Fatal> {
        self.inner.take_undo_fault()
    }

    fn open_scheme_guard(&mut self, holder: crate::chain::SchemeGuardHolder) {
        self.inner.open_scheme_guard(holder);
    }

    fn commit_scheme_guard(&mut self) {
        self.inner.commit_scheme_guard();
    }

    fn exit_scheme_guard(&mut self, log: bool) {
        self.inner.exit_scheme_guard(log);
    }
}

/// dashd-node's header-chain fail-stop (`ReadFailStop`): a read failure goes to the node's
/// abort. The rig's default panics, which aborts the process when V's tip hold drops unwinding.
struct ReadFailStop {
    abort: Arc<dyn AbortSink>,
    tripped: AtomicBool,
}

impl FailStop for ReadFailStop {
    fn read_failed(&self, what: &str) {
        self.tripped.store(true, Ordering::SeqCst);
        self.abort.read_failed(what);
    }

    fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::SeqCst)
    }
}

/// The node's stores, which outlive each V: the engine (a probe behind the node's latch), the
/// coins, the block files and the evo store with its layer, and the node's abort path.
struct Disk {
    _dir: tempfile::TempDir,
    p: Arc<ChainSetup>,
    after_genesis: i64,
    latch: GlobalFailStop,
    recorder: Arc<RecordingAbort>,
    sink: Arc<dyn AbortSink>,
    shutdown: Arc<LocalShutdown>,
    db: Arc<GuardedEngine<ProbeEngine>>,
    coins: ChainstateCell<ProbeEngine>,
    files: Arc<GuardedFiles<OsFs>>,
    store: Arc<RocksEvoStore<ProbeEngine>>,
    layer: SharedEvoLayer,
    watch: Arc<Watch>,
}

impl Disk {
    /// Fresh stores for `run`.
    fn new(run: &Run) -> Disk {
        use std::os::unix::fs::DirBuilderExt as _;
        let p = &run.p;
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::DirBuilder::new().mode(0o700).create(dir.path().join("blocks")).expect("dir");
        let latch = GlobalFailStop::new();
        let recorder = Arc::new(RecordingAbort::default());
        let shutdown = Arc::new(LocalShutdown::default());
        let sink: Arc<dyn AbortSink> = Arc::new(super::support::AbortThenShutdown {
            sink: recorder.clone(),
            shutdown: shutdown.clone(),
        });
        let db = Arc::new(GuardedEngine::new(
            ProbeEngine::new(&CHAINSTATE_SCHEMA),
            latch.clone(),
            sink.clone(),
        ));
        let budget = CoinsBudget::new(8 << 20, 0, Ratio::HALF, 0);
        let coins = Arc::new(Mutex::new(CoinsCache::new(db.clone(), budget, Salt::fixed(7), None)));
        let files = Arc::new(GuardedFiles::new(
            StoreFiles::with_fs(
                Arc::new(OsFs::new()),
                BlockFilesConfig::new(dir.path().join("blocks"), p.message_start),
                BlockFilesState::default(),
            ),
            latch.clone(),
        ));
        let net_info = node_net_info(p, false).expect("regtest's NetInfo rules");
        let genesis = Mined::genesis(p).hash();
        let store = Arc::new(RocksEvoStore::new(db.clone(), node_codec(net_info), genesis));
        let layer = SharedEvoLayer::new(store.resume_layer().expect("a fresh meta row"));
        let watch = Arc::new(Watch {
            db: db.clone(),
            armed: Mutex::new(None),
            windows: Mutex::new(Vec::new()),
            region: AtomicBool::new(false),
            in_region: Mutex::new(None),
        });
        Disk {
            _dir: dir,
            p: p.clone(),
            after_genesis: run.after_genesis,
            latch,
            recorder,
            sink,
            shutdown,
            db,
            coins,
            files,
            store,
            layer,
            watch,
        }
    }

    fn path(&self) -> AbortPath {
        AbortPath::new(self.sink.clone(), self.latch.clone())
    }

    fn block_files(&self) -> NodeBlockFiles<OsFs> {
        let pow = *HeaderParams::from_chain_params(&self.p).expect("regtest").pow();
        NodeBlockFiles::new(self.files.clone(), pow, Arc::new(self.path()))
    }

    /// The node's `NodeEvo` on these stores, as dashd-node's `node_evo` builds it at an open
    /// whose next block is at `next`: a new MNHF manager, cold, and its members facade.
    fn node_evo(&self, next: u32) -> (NodeEvo<ProbeEngine, DmlChecks>, Arc<MembersFacade>) {
        let p = &self.p;
        let rows =
            Arc::new(RuntimeRows::new(self.store.clone(), Arc::new(EffectLedger::default())));
        let facade = Arc::new(
            MembersFacade::new(p, Arc::clone(&rows), self.latch.clone()).expect("LLMQ parameters"),
        );
        let mn_list = MnListSignal::new(Default::default());
        let appliers = EffectAppliers {
            snapshot: Box::new(StoreSnapshots {
                rows,
            }),
            is_lock: Box::new(UnreachableIsLock),
            mineable: Box::new(FacadeMineable(Arc::clone(&facade))),
            platform_ban: Box::new(MemPlatformBans::default()),
            notify: Box::new(mn_list.applier()),
        };
        let evo = NodeEvo::new(NodeEvoParts {
            store: self.store.clone(),
            layer: self.layer.clone(),
            appliers,
            params: p.clone(),
            net_info: node_net_info(p, false).expect("regtest's NetInfo rules"),
            ev1: DmlChecks,
            cl: Box::new(AttemptClVerifier::default()),
            blocks: Box::new(FileBlocks(self.block_files())),
            scheme: dash_consensus::ctx::bls_ctx_for_block(p, next),
            mn_list,
            bls_checks: Arc::new(dash_evo::llmq::SerialBlsChecks),
        })
        .expect("regtest's payment parameters");
        (evo, facade)
    }

    /// V's seams on these stores for an open whose next block is at `next`: the node's
    /// connector, [`Forward`]ed, its records to `digest`, and a flush side over the node's latch
    /// (V's stop reports as production's does).
    fn config(&self, next: u32, digest: Option<Box<dyn DigestSink>>) -> RigConfig {
        let (hooks, facade) = self.node_evo(next);
        let parts = ConnectorParts {
            coins: self.coins.clone(),
            files: self.files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches: Arc::new(ScriptCaches::new(CacheSizes::default()).expect("caches")),
            helper: Box::new(NodeHelper),
            abort: self.path(),
            assume_valid: None,
            digest,
            xcheck: true,
        };
        let inner =
            CoreConnector::new(&self.p, parts, hooks, Arc::clone(&facade)).expect("connector");
        let rows = inner.ehf_rows().map(|inner| {
            Arc::new(Watched {
                inner,
                watch: self.watch.clone(),
            }) as Arc<dyn EhfRows>
        });
        let connector = Forward {
            inner: Box::new(inner),
            rows,
            watch: self.watch.clone(),
        };
        RigConfig {
            params: Some(self.p.clone()),
            after_genesis: Some(self.after_genesis),
            no_check_block_index: true,
            files: Some(Box::new(self.block_files())),
            connector: Some(Box::new(connector)),
            facade: Some(facade),
            abort: Some(self.recorder.clone()),
            shutdown: Some(self.shutdown.clone()),
            flush: Some(Box::new(Latched(self.latch.clone()))),
            fail: Some(Arc::new(ReadFailStop {
                abort: self.sink.clone(),
                tripped: AtomicBool::new(false),
            })),
            ..RigConfig::default()
        }
    }

    /// `rig`'s V stopped cleanly at `tip`, the evo layer flushed (B1 and W's release), then
    /// `edit` run on the stopped stores, and V started again on them: a new `NodeEvo`, whose
    /// MNHF manager is cold, and a V whose caches are empty.
    fn restart(
        &self,
        mut rig: Rig,
        tip: &Mined,
        digest: Option<Box<dyn DigestSink>>,
        edit: impl FnOnce(&Disk),
    ) -> Rig {
        let at = rig.id(tip);
        let exit = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        drop(rig);
        testkit::flush(&self.store, &mut self.layer.lock());
        edit(self);
        let mut cfg = self.config(tip.height + 1, digest);
        let files = cfg.files.take().expect("the node's block files");
        let rig = Rig::restart_with(exit, files, at, cfg).expect("V starts");
        self.assert_no_abort();
        rig
    }

    /// Writes `bytes` as `block`'s `mnhf` row, as the evo flush would.
    fn put_row(&self, block: &BlockHash, bytes: &[u8]) {
        let mut b = self.db.batch();
        b.put(EvoCf::Mnhf.cf(), EvoCf::Mnhf.key(block).as_bytes(), bytes);
        self.db.write(b, Commit::Durable).expect("a row written");
    }

    /// No `mnhf` row of `m` is stored: a read of it misses.
    fn assert_no_row(&self, m: &Mined) {
        let row = self.db.get(EvoCf::Mnhf.cf(), EvoCf::Mnhf.key(&m.hash()).as_bytes());
        assert_eq!(row.expect("a read"), None, "{}: a stored mnhf row", m.height);
    }

    /// The node's `AbortNode` reports, in order.
    fn reports(&self) -> Vec<String> {
        self.recorder.messages()
    }

    fn assert_no_abort(&self) {
        let reports = self.reports();
        assert!(reports.is_empty(), "{reports:?}");
    }

    fn tripped(&self) -> bool {
        self.latch.tripped_now().is_some()
    }
}

/// `blocks` connected on `rig`, each accepted.
fn connect_all(rig: &Rig, blocks: &[Mined]) {
    for m in blocks {
        let r = rig.v.process_block(m.block.clone(), true, None, true).recv().expect("V replies");
        assert!(r.accepted && r.state.is_valid(), "{}: {r:?}", m.height);
    }
}

/// How V ended: its exit, or its thread's panic message.
fn ended(rig: &mut Rig) -> Result<VExit, String> {
    rig.v.shutdown();
    let join = rig.join.take().expect("running");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || _ = tx.send(join.join().expect("an unmarked joiner")));
    let joined = rx.recv_timeout(std::time::Duration::from_secs(30)).expect("V's thread ends");
    joined.map_err(|p| panic_message(&*p))
}

/// What a faulted V left: its exit (no panic), the reports, and the latch.
struct Stopped {
    exit: VExit,
    reports: Vec<String>,
    tripped: bool,
}

impl Stopped {
    /// `rig`'s V, which must have stopped without a panic.
    fn of(disk: &Disk, rig: &mut Rig) -> Stopped {
        let exit = ended(rig).unwrap_or_else(|p| panic!("V panicked: {p}"));
        Stopped {
            exit,
            reports: disk.reports(),
            tripped: disk.tripped(),
        }
    }

    /// V stopped on an `AbortNode`'s `Fatal`, not a panic's, whose text starts with `context`.
    fn assert_fatal(&self, context: &str) {
        let fatal = self.exit.fatal.as_ref().expect("V stopped on a Fatal");
        assert!(fatal.0.starts_with(context), "{}", fatal.0);
        assert_eq!(fatal.cause(), crate::chain::AbortCause::AbortNode, "{}", fatal.0);
    }

    /// V stopped on `GetForBlock`'s `Fatal`, which holds `detail`.
    fn assert_get_for_block(&self, detail: &str) {
        self.assert_fatal("GetForBlock: ");
        let fatal = self.exit.fatal.as_ref().expect("V stopped on a Fatal");
        assert!(fatal.0.contains(detail), "{} lacks {detail}", fatal.0);
    }

    /// One report, holding `detail`, and the latch tripped.
    fn assert_one_report(&self, detail: &str) {
        assert_eq!(self.reports.len(), 1, "{:?}", self.reports);
        assert!(self.reports[0].contains(detail), "{:?} lacks {detail}", self.reports);
        assert!(self.tripped, "the latch trips");
    }

    /// Both of V's caches empty, as a fresh V's whose first read threw.
    fn assert_caches_empty(&self) {
        let entries = &self.exit.deployment_cache_entries;
        assert_eq!(self.exit.deployment_caches, (true, true), "{entries:?}");
    }

    /// The block `m` not marked failed: a read fault is no verdict.
    fn assert_not_failed(&self, m: &Mined) {
        let reader = self.exit.chain.reader();
        if let Some(id) = reader.lookup(&m.hash()) {
            let status = reader.entry(id).expect("indexed").status;
            assert!(!status.has(dash_chain::Status::FAILED_MASK), "{}: {status:?}", m.height);
        }
    }
}

/// `run` connected to `n` on fresh stores and V restarted cold at `n`,
/// `edit` run on the stopped stores.
fn restarted(run: &Run, n: u32, edit: impl FnOnce(&Disk)) -> (Disk, Rig) {
    let disk = Disk::new(run);
    let rig = Rig::with(disk.config(1, None));
    assert_eq!(rig.genesis.hash(), Mined::genesis(&run.p).hash(), "the run's genesis");
    connect_all(&rig, &run.blocks[..n as usize]);
    disk.assert_no_abort();
    let rig = disk.restart(rig, run.at(n), None, edit);
    (disk, rig)
}

/// Bytes no `mnhf` row decodes from: a map of 2 entries with only one's bytes.
const GARBAGE: [u8; 3] = [2, 13, 1];

/// The tip of the tip-path tests, past V20 and before V24's signal.
const TIP: u32 = 110;

/// T1: the first EHF read of a tip connect on a fresh V, its I/O failing. One report, the
/// engine's; the latch tripped; V stopped on `GetForBlock`'s `Fatal` with no panic and both of
/// its caches empty. The control, through the same window, connects with no report.
#[test]
fn an_io_fault_on_the_tips_first_ehf_read_stops_v_once() {
    let run = Run::of(ASSET_LOCKS);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    for fail in [None, Some(ErrorKind::Io)] {
        let (disk, mut rig) = restarted(&run, TIP, |_| {});
        disk.watch.arm(Gate::Read(rig.id(tip)), tip.hash(), fail);
        if fail.is_none() {
            connect_all(&rig, std::slice::from_ref(next));
            disk.watch.take_one().assert_read_once(false);
            disk.assert_no_abort();
            continue;
        }
        rig.submit_stopping(next, "the faulted connect");
        disk.watch.take_one().assert_read_once(true);
        let stopped = Stopped::of(&disk, &mut rig);
        stopped.assert_get_for_block("injected row read failure");
        stopped.assert_one_report("injected row read failure");
        stopped.assert_caches_empty();
        stopped.assert_not_failed(next);
    }
}

/// A tip connect whose parent's row does not decode (`bytes`): one report, the engine's corrupt
/// row with `detail`; the latch tripped; no panic; both caches empty; the block not failed.
fn tip_decode_fault(bytes: &[u8], detail: &str) {
    let run = Run::of(ASSET_LOCKS);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    let (disk, mut rig) = restarted(&run, TIP, |d| d.put_row(&tip.hash(), bytes));
    disk.watch.arm(Gate::Read(rig.id(tip)), tip.hash(), None);
    rig.submit_stopping(next, "the faulted connect");
    disk.watch.take_one().assert_read_once(false);
    let stopped = Stopped::of(&disk, &mut rig);
    stopped.assert_get_for_block(detail);
    stopped.assert_one_report(detail);
    stopped.assert_caches_empty();
    stopped.assert_not_failed(next);
}

/// T2: the tip's first EHF read meets a row that does not decode.
#[test]
fn a_corrupt_row_on_the_tips_first_ehf_read_stops_v_once() {
    tip_decode_fault(&GARBAGE, "corrupt record mnhf/");
}

/// T3, BL-274: a stored signal bit at or past `VERSIONBITS_NUM_BITS` (29) is a corrupt row, as
/// the in-block signal's `bad-mnhf-nbit-out-of-bounds` domain says.
#[test]
fn a_stored_bit_out_of_range_on_the_tips_read_stops_v_once() {
    // Core's `std::map<uint8_t, int>`: one entry, bit 29 at 105.
    let corrupt = [&[1, 29][..], &105_i32.to_le_bytes()].concat();
    tip_decode_fault(&corrupt, "bit 29 out of order or range");
}

/// A proposal on the tip of a fresh V, the EHF read of the tip's row failing as `fail` says or
/// the row not decoding (`bytes`), or, with neither, the control: V stops on `GetForBlock`'s
/// `Fatal` with one report and the latch tripped, the RPC seeing V gone (`Aborted`); the control
/// answers valid with no report.
fn proposal(fail: Option<ErrorKind>, bytes: Option<&[u8]>, detail: &str) {
    let run = Run::of(ASSET_LOCKS);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    let (disk, mut rig) = restarted(&run, TIP, |d| {
        if let Some(bytes) = bytes {
            d.put_row(&tip.hash(), bytes);
        }
    });
    disk.watch.arm(Gate::Read(rig.id(tip)), tip.hash(), fail);
    let answer = rig.v.test_block_validity(next.block.clone(), true, true).recv();
    disk.watch.take_one().assert_read_once(fail.is_some());
    if fail.is_none() && bytes.is_none() {
        assert!(matches!(answer, Ok(Ok(ref s)) if s.is_valid()), "{answer:?}");
        disk.assert_no_abort();
        return;
    }
    assert_eq!(answer.map(|_| ()), Err(WaitError::Disconnected), "the proposal's answer");
    let stopped = Stopped::of(&disk, &mut rig);
    stopped.assert_get_for_block(detail);
    stopped.assert_one_report(detail);
    stopped.assert_caches_empty();
}

/// T4: a proposal's first EHF read fails with an I/O error.
#[test]
fn an_io_fault_on_a_proposals_ehf_read_stops_v_once() {
    proposal(Some(ErrorKind::Io), None, "injected row read failure");
}

/// T5: a proposal's first EHF read meets a row that does not decode.
#[test]
fn a_corrupt_row_on_a_proposals_ehf_read_stops_v_once() {
    proposal(None, Some(&GARBAGE), "corrupt record mnhf/");
}

/// The proposal's control: the same window on a clean row answers, with no report.
#[test]
fn a_proposals_clean_ehf_read_answers() {
    proposal(None, None, "");
}

/// H4's `ProcessBlock` is the first reader of the parent's row where no EHF deployment reads it
/// first: dashd's `feature_masternode_shares` run, V24 and EVO_SHARES without EHF. A row that
/// does not decode is one report, the engine's, and the latch tripped (OD-CP-105 N1); V stops on
/// `ProcessBlock`'s `Fatal`. The control connects through the same window with no report. The
/// control's window reads the parent's row and then the block's own, which no store holds yet
/// (a miss); the faulted connect's stops at the parent's.
#[test]
fn a_corrupt_parent_row_read_first_by_process_block_is_reported_once() {
    let run = Run::of(MASTERNODE_SHARES);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    for bytes in [None, Some(&GARBAGE[..])] {
        let (disk, mut rig) = restarted(&run, TIP, |d| {
            if let Some(bytes) = bytes {
                d.put_row(&tip.hash(), bytes);
            }
        });
        disk.assert_no_row(next);
        disk.watch.arm(Gate::Connect(next.hash()), tip.hash(), None);
        if bytes.is_none() {
            connect_all(&rig, std::slice::from_ref(next));
            disk.watch.take_one().assert_reads(&[(tip.hash(), false), (next.hash(), false)]);
            disk.assert_no_abort();
            continue;
        }
        rig.submit_stopping(next, "the faulted connect");
        disk.watch.take_one().assert_read_once(false);
        let stopped = Stopped::of(&disk, &mut rig);
        stopped.assert_fatal("CMNHFManager::ProcessBlock: ");
        stopped.assert_one_report("corrupt record mnhf/");
        stopped.assert_not_failed(next);
    }
}

/// BL-787 (OD-CP-178): inside startup's verify region, a read that fails in H4's
/// `CMNHFManager::ProcessBlock` is caught by its own `try`, the innermost (`evo/mnhftx.cpp:257-260`
/// at v24.0.0; a read's `dbwrapper_error` is no `EvoDbInconsistencyError`): its line, then
/// `failed-proc-mnhf-inblock`, which `ProcessSpecialTxsInBlock` passes on without its own
/// FAILURE (`specialtxman.cpp:1033-1036` at v24.0.0), and `ConnectBlock`'s line
/// (`validation.cpp:2677` at v24.0.0). The region records the read as caught, not as its fault, and
/// nothing reports it. The walk's G1 keeps such a block out of startup's attempts (CH5 D2), so the
/// region is opened around the tip's connect here, on V's thread.
#[test]
fn an_io_fault_in_process_blocks_read_inside_the_startup_region_is_caught_by_its_try() {
    install_accept_log();
    let run = Run::of(MASTERNODE_SHARES);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    let (disk, mut rig) = restarted(&run, TIP, |_| {});
    let v = rig.v_thread();
    disk.watch.arm(Gate::Connect(next.hash()), tip.hash(), Some(ErrorKind::Io));
    disk.watch.region.store(true, Ordering::SeqCst);
    let replied = rig.v.process_block(next.block.clone(), true, None, true).recv();
    disk.watch.take_one().assert_read_once(true);
    let in_region = disk.watch.in_region.lock().expect("watch").take().expect("the connect ran");
    let reason = "failed-proc-mnhf-inblock";
    let Ok(ConnectOutcome::Invalid(err)) = &in_region.out else {
        panic!("the catch's verdict, not {in_region:?}");
    };
    assert_eq!(err.reject_reason(), reason);
    assert_eq!((in_region.fault.as_deref(), in_region.caught), (None, true), "caught");
    let read = disk.latch.tripped().expect("the read latched").clone();
    assert_eq!(read.site, crate::store::sites::READ_SITE);
    assert!(disk.reports().is_empty(), "nothing reported the read: {:?}", disk.reports());
    let failed = format!("CMNHFManager::ProcessBlock -- failed: {}", read.message);
    let connect = format!(
        "ConnectBlock(DASH): ProcessSpecialTxsInBlock for block {} failed with {reason}",
        next.hash()
    );
    assert_eq!(levels_on(v, &failed), [tracing::Level::INFO]);
    assert_eq!(levels_on(v, &connect), [tracing::Level::ERROR]);
    assert_logged_before(&lines_on(v), &failed, &connect);
    let outer = "CSpecialTxProcessor::ProcessSpecialTxsInBlock -- FAILURE!";
    assert!(!lines_on(v).iter().any(|l| l.starts_with(outer)), "H4's own catch caught nothing");
    drop(replied);
    ended(&mut rig).unwrap_or_else(|p| panic!("V panicked: {p}"));
}

/// BL-708: `LLMQ_TEST_V17`'s `IsQuorumTypeEnabled` reads TESTDUMMY, which with `-vbparams`
/// useEHF reads the parent's row through `GetForBlock`. On dashd's `feature_masternode_shares` run
/// (V24 and EVO_SHARES without EHF) the quorum block processor's `GetEnabledQuorumParams` is the
/// first reader of that row. A read that fails (`fail`) or a row that does not decode (`bytes`)
/// stops V on D5's `Fatal`, with no panic, one report, the latch tripped, no TESTDUMMY state
/// cached, and the block not failed. With neither, the control connects with no report.
fn test_v17_fault(fail: Option<ErrorKind>, bytes: Option<&[u8]>, detail: &str) {
    let run = Run::with(MASTERNODE_SHARES, &["-vbparams=testdummy:0:999999999999:0:144:108:0:0:1"]);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    let (disk, mut rig) = restarted(&run, TIP, |d| {
        if let Some(bytes) = bytes {
            d.put_row(&tip.hash(), bytes);
        }
    });
    disk.watch.arm(Gate::Connect(next.hash()), tip.hash(), fail);
    if fail.is_none() && bytes.is_none() {
        connect_all(&rig, std::slice::from_ref(next));
        disk.watch.take_one().assert_reads(&[(tip.hash(), false), (next.hash(), false)]);
        disk.assert_no_abort();
        return;
    }
    rig.submit_stopping(next, "the faulted connect");
    disk.watch.take_one().assert_read_once(fail.is_some());
    let stopped = Stopped::of(&disk, &mut rig);
    let fatal = &stopped.exit.fatal.as_ref().expect("V stopped on a Fatal").0;
    assert!(fatal.contains("GetForBlock: "), "{fatal}");
    stopped.assert_fatal("quorum block processor: ");
    stopped.assert_one_report(detail);
    let (versionbits, _) = &stopped.exit.deployment_cache_entries;
    assert!(versionbits.contains("data: [{}, "), "a TESTDUMMY state cached: {versionbits}");
    stopped.assert_not_failed(next);
}

#[test]
fn test_v17_clean_parent_row_connects() {
    test_v17_fault(None, None, "");
}

#[test]
fn an_io_fault_on_test_v17s_ehf_read_stops_v_once() {
    test_v17_fault(Some(ErrorKind::Io), None, "injected row read failure");
}

#[test]
fn a_corrupt_row_on_test_v17s_ehf_read_stops_v_once() {
    test_v17_fault(None, Some(&GARBAGE), "corrupt record mnhf/");
}

/// T6, the control: on a fresh V the stored rows are read, the period end 749's from the engine
/// once inside its window, and V24 starts at 250, locks in at 500 and is active from 750, as
/// Core's states at each block (the run's golden `dep`). Nothing is reported.
#[test]
fn clean_stored_rows_carry_v24_across_three_period_ends() {
    let run = Run::of(ASSET_LOCKS);
    let n = 3 * PERIOD + 2;
    let (disk, rig) = restarted(&run, n, |_| {});
    let blocks = &run.blocks[..n as usize];
    let end = run.at(3 * PERIOD - 1);
    disk.watch.arm(Gate::Read(rig.id(end)), end.hash(), None);
    let ids = rig.ids(blocks);
    let states = rig.v.query(move |q| {
        use dash_consensus::ctx::DeploymentView as _;
        let v24 = dash_params::DeploymentPos::V24;
        let answer = |id| q.deployments.versionbits(Some(id), v24).expect("V24 on regtest").state;
        ids.into_iter().map(answer).collect::<Vec<_>>()
    });
    let states = states.recv().expect("V serves");
    let want: Vec<_> = blocks.iter().map(|m| core_state(Some(SIGNAL), m.height)).collect();
    assert_eq!(states, want);
    let first = |s| want.iter().position(|w| *w == s).map(|i| blocks[i].height + 1);
    assert_eq!(
        [ThresholdState::Started, ThresholdState::LockedIn, ThresholdState::Active].map(first),
        [Some(PERIOD), Some(2 * PERIOD), Some(3 * PERIOD)]
    );
    disk.watch.take_one().assert_read_once(false);
    disk.assert_no_abort();
}

/// T7a, a characterisation of today's behaviour (it holds before BL-683's fix too): the RPC
/// report's stage of a block with no row whose data cannot be read is Core's
/// `failed-getehfforblock-read`, and V goes on serving with nothing reported.
#[test]
fn the_reports_stage_without_block_data_is_a_read_error() {
    let run = Run::of(ASSET_LOCKS);
    let (tip, next) = (run.at(TIP), run.at(TIP + 1));
    let (disk, rig) = restarted(&run, TIP, |_| {});
    rig.headers(std::slice::from_ref(next));
    let id = rig.id(next);
    let stage = rig.v.query(move |q| q.ehf_signals_stage(id).map_err(|e| e.to_string()));
    assert_eq!(stage.recv(), Ok(Err("failed-getehfforblock-read".to_owned())));
    let tip_id = rig.id(tip);
    let served = rig.v.query(move |q| q.ehf_signals_stage(tip_id).is_ok());
    assert_eq!(served.recv(), Ok(true), "V serves");
    disk.assert_no_abort();
    assert!(!disk.tripped());
}

/// A tip in V24's `LOCKED_IN` period: `StateSinceHeight`'s walk from it reads the older period
/// end's row (249), which no connect reads, and finds `STARTED` there.
const WALK_TIP: u32 = 2 * PERIOD + 10;

/// T7b: the RPC report at [`WALK_TIP`], its stage read answering, and the older period end's
/// row, which only the report's walk reads, faulting as `fail` says or not decoding (`bytes`).
/// The report answers an error naming the cause (the injected I/O failure, or the corrupt
/// record with the row's block): no panic, one report, the latch tripped. The window shows the
/// failing read was that row's alone. With neither, the control: the same read answers, the
/// whole report with V24's entry, no report, the latch open and V serving.
fn report_walk_fault(fail: Option<ErrorKind>, bytes: Option<&[u8]>, detail: &str) {
    let run = Run::of(ASSET_LOCKS);
    let end = run.at(PERIOD - 1);
    let (disk, mut rig) = restarted(&run, WALK_TIP, |d| {
        if let Some(bytes) = bytes {
            d.put_row(&end.hash(), bytes);
        }
    });
    let tip = rig.id(run.at(WALK_TIP));
    disk.watch.arm(Gate::Read(rig.id(end)), end.hash(), fail);
    let report = rig.v.query(move |q| {
        let stage = q.ehf_signals_stage(tip).map_err(|e| e.to_string());
        let report = q.deployments.deployment_info(tip);
        let names = report.map(|r| r.iter().map(|d| d.name()).collect::<Vec<_>>());
        (stage.is_ok(), names.map_err(|e| e.to_string()))
    });
    let (staged, names) = report.recv().expect("the report answers, with no panic");
    assert!(staged, "the stage's pre-read answers");
    disk.watch.take_one().assert_read_once(fail.is_some());
    if fail.is_none() && bytes.is_none() {
        let names = names.unwrap_or_else(|e| panic!("the whole report: {e}"));
        assert!(names.contains(&dash_params::DeploymentPos::V24.name()), "{names:?}");
        disk.assert_no_abort();
        assert!(!disk.tripped(), "the latch stays open");
        let serving = rig.v.query(move |q| q.ehf_signals_stage(tip).is_ok()).recv();
        assert_eq!(serving, Ok(true), "V serves");
        return;
    }
    let err = names.expect_err("an error, not a report");
    assert!(err.contains(detail), "the cause: {err}");
    if bytes.is_some() {
        let row = format!("corrupt record mnhf/{}", end.hash());
        assert!(err.contains(&row), "the targeted row: {err}");
    }
    let stopped = Stopped::of(&disk, &mut rig);
    stopped.assert_one_report(detail);
}

#[test]
fn a_clean_older_period_end_in_the_reports_walk_answers() {
    report_walk_fault(None, None, "");
}

#[test]
fn a_corrupt_older_period_end_in_the_reports_walk_is_an_error() {
    report_walk_fault(None, Some(&GARBAGE), "corrupt record mnhf/");
}

#[test]
fn an_io_fault_on_an_older_period_end_in_the_reports_walk_is_an_error() {
    report_walk_fault(Some(ErrorKind::Io), None, "injected row read failure");
}

/// T10: H9's `CMNHFManager::UndoBlock` reads the row of the block whose signals it undoes, the
/// run's V24 signal at 188; a cold MNHF manager reads it from the engine. V disconnects the
/// block (`invalidateblock`), the read faulting as `fail` says or the row not decoding
/// (`bytes`), or, with neither, the control: one report, the latch tripped, and V stopped on a
/// `Fatal` naming `CMNHFManager::UndoBlock`. The control disconnects with no report.
fn undo(fail: Option<ErrorKind>, bytes: Option<&[u8]>, detail: &str) {
    let run = Run::of(ASSET_LOCKS);
    let s = run.at(SIGNAL);
    let (disk, mut rig) = restarted(&run, SIGNAL, |d| {
        if let Some(bytes) = bytes {
            d.put_row(&s.hash(), bytes);
        }
    });
    disk.watch.arm(Gate::Disconnect(rig.id(s)), s.hash(), fail);
    let answer = rig.v.invalidate_block(s.hash()).recv();
    disk.watch.take_one().assert_read_once(fail.is_some());
    if fail.is_none() && bytes.is_none() {
        assert_eq!(answer, Ok(Ok(())), "the disconnect");
        disk.assert_no_abort();
        return;
    }
    assert_eq!(answer.map(|_| ()), Err(WaitError::Disconnected), "the disconnect");
    let stopped = Stopped::of(&disk, &mut rig);
    stopped.assert_fatal("CMNHFManager::UndoBlock: ");
    stopped.assert_one_report(detail);
}

#[test]
fn a_clean_undo_of_a_signalling_block_reports_nothing() {
    undo(None, None, "");
}

#[test]
fn a_corrupt_row_in_the_undo_of_a_signalling_block_is_reported_once() {
    undo(None, Some(&GARBAGE), "corrupt record mnhf/");
}

#[test]
fn an_io_fault_in_the_undo_of_a_signalling_block_is_reported_once() {
    undo(Some(ErrorKind::Io), None, "injected row read failure");
}

/// T9, with the O8 recorder (feature `statedigest`).
#[cfg(feature = "statedigest")]
mod record {
    use dash_statedigest::{Chain, FileKind, Header, Writer};

    use super::*;
    use crate::connect::{DigestAttempt, DigestLog};

    /// One state-hash log across V's restarts (its coin stats must see every block), each record of
    /// the armed connect inside the [`Watch`]'s window.
    #[derive(Clone)]
    struct SharedLog {
        log: Arc<Mutex<DigestLog<Vec<u8>>>>,
        watch: Arc<Watch>,
    }

    impl SharedLog {
        fn new(watch: Arc<Watch>) -> SharedLog {
            let header = Header {
                file: FileKind::Digest,
                src: Writer::Rdashd,
                ver: b"test".to_vec(),
                chain: Chain::Regtest,
                run: b"1-1".to_vec(),
            };
            let log = DigestLog::new(Vec::new(), header).expect("the H is written");
            SharedLog {
                log: Arc::new(Mutex::new(log)),
                watch,
            }
        }

        fn sink(&self) -> Box<dyn DigestSink> {
            Box::new(self.clone())
        }
    }

    impl DigestSink for SharedLog {
        fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
            let mut log = self.log.lock().expect("log");
            let DigestAttempt::Connect(c) = attempt else {
                return log.record(cx, attempt);
            };
            self.watch.window(Gate::Record(c.at.hash), || log.record(cx, attempt))
        }
    }

    /// T9: the O8 recorder's record of the connect of [`WALK_TIP`]'s child, whose `since` walk is
    /// the only reader of the older period end's row (249): its read faulting as `fail` says or the
    /// row not decoding (`bytes`), or, with neither, the control. The ordinary connect's reads
    /// answer. One report, the latch tripped by the guarded read itself (its site, kind and row)
    /// and still that failure after the next write, no panic; the attempt's outcome, `Connected`,
    /// is committed, and the latch refuses V's next write: the block after is not stored, with no
    /// second report. The control records with no report. The
    /// record's window reads that row and no other `mnhf` row.
    fn record_fault(fail: Option<ErrorKind>, bytes: Option<&[u8]>, detail: &str) {
        let run = Run::of(ASSET_LOCKS);
        let end = run.at(PERIOD - 1);
        let next = run.at(WALK_TIP + 1);
        let disk = Disk::new(&run);
        let log = SharedLog::new(disk.watch.clone());
        let rig = Rig::with(disk.config(1, Some(log.sink())));
        connect_all(&rig, &run.blocks[..WALK_TIP as usize]);
        disk.assert_no_abort();
        let mut rig = disk.restart(rig, run.at(WALK_TIP), Some(log.sink()), |d| {
            if let Some(bytes) = bytes {
                d.put_row(&end.hash(), bytes);
            }
        });
        disk.watch.arm(Gate::Record(next.hash()), end.hash(), fail);
        if fail.is_none() && bytes.is_none() {
            connect_all(&rig, std::slice::from_ref(next));
            disk.watch.take_one().assert_read_once(false);
            disk.assert_no_abort();
            return;
        }
        let _ = rig.v.process_block(next.block.clone(), true, None, true).recv();
        disk.watch.take_one().assert_read_once(fail.is_some());
        assert_eq!(rig.v.view().snap.tip.tip, rig.id(next), "the attempt's Connected committed");
        let winner = || disk.latch.tripped().map(|f| (f.kind, f.site, f.message.clone()));
        let (kind, row) = match fail {
            Some(kind) => (kind, detail.to_owned()),
            None => (ErrorKind::Corruption, format!("corrupt record mnhf/{}", end.hash())),
        };
        let first = winner().expect("the latch tripped");
        assert_eq!(
            (first.0, first.1),
            (kind, "chainstate read"),
            "the guarded read won: {first:?}"
        );
        assert!(first.2.contains(&row), "the targeted row's failure: {first:?}");
        let after = run.at(WALK_TIP + 2);
        let answer = rig.v.process_block(after.block.clone(), true, None, true).recv();
        let refused = answer.expect("V answers");
        assert!(!refused.accepted, "the latch refuses the next write: {refused:?}");
        assert!(format!("{:?}", refused.state).contains("Failed to find position"), "{refused:?}");
        assert_eq!(winner(), Some(first), "the read's failure stays the winner");
        let stopped = Stopped::of(&disk, &mut rig);
        stopped.assert_one_report(detail);
        let committed = stopped.exit.chain.private_tip().map(|(t, _)| t.hash);
        assert_eq!(committed, Some(next.hash()), "nothing past the record's block");
    }

    #[test]
    fn a_records_clean_walk_reports_nothing() {
        record_fault(None, None, "");
    }

    #[test]
    fn an_io_fault_in_a_records_walk_is_reported_once() {
        record_fault(Some(ErrorKind::Io), None, "injected row read failure");
    }

    #[test]
    fn a_corrupt_row_in_a_records_walk_is_reported_once() {
        record_fault(None, Some(&GARBAGE), "corrupt record mnhf/");
    }
}
