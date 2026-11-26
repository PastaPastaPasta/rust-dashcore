//! [`PoolSink`] under a real V (`spawn`): block connects, `InvalidateBlock` and ChainLock
//! enforcement drive the pool through V's holds, and V's signals thread publishes what they
//! staged.

mod support;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use dash_clock::MockClock;
use dash_consensus::primitives::{Block, BlockHash, OutPoint, Tx, TxHash};
use dash_consensus::validation::TxValidationResult;
use dash_consensus::{BlockId, TxRef};
use dash_dml::MnList;
use dash_num::Hash256;
use dash_params::ChainSetup;
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    ChainCommand, ChainHandle, ChainLockSig, Cont, DisconnectPool, Fatal, FlushHost, FlushSite,
    FlushStateMode, HeldMempool, MempoolCompletion, MempoolSink, MempoolTurn, NodeJoinHandle,
    RecordingAbort, RemovalReason, ReorgCx, SignalsThread, SiteDecision, Staged, TipFacts, VExit,
    ValidationInterface, ValidationSignals, WaitError,
};
use dashd_chainstate::connect::SpecialTxRules;
use dashd_mempool::PoolSink;
use dashd_mempool::accept::special::SpecialTxChain as _;
use dashd_mempool::options::MemPoolOptions;
use dashd_mempool::pool::{EntryMetadata, Mempool, PoolState};
use dashd_mempool::types::LockPoints;
use support::{MATURITY, Matured, NO_DIP3, V, child, params, params_with, spend, unlock};

const NON_FINAL: u32 = 0xffff_fffe;

/// What the node's listener saw, in order: removals, connects, disconnects and tip updates.
#[derive(Default)]
struct Events(Mutex<Vec<String>>);

impl Events {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("events"))
    }
}

impl ValidationInterface for Events {
    fn transaction_added_to_mempool(&self, tx: &TxRef, _: i64, _: u64) {
        self.0.lock().expect("events").push(format!("add {}", tx.txid()));
    }
    fn transaction_removed_from_mempool(&self, tx: &TxRef, reason: RemovalReason, _: u64) {
        self.0.lock().expect("events").push(format!("remove {reason:?} {}", tx.txid()));
    }
    fn block_connected(&self, block: &Arc<Block>, _: BlockId) -> Cont {
        self.0.lock().expect("events").push(format!("block {}", block.hash()));
        Cont::Done
    }
    fn block_disconnected(&self, block: &Arc<Block>, _: BlockId) {
        self.0.lock().expect("events").push(format!("disconnect {}", block.hash()));
    }
}

fn removed(reason: &str, tx: &Tx) -> String {
    format!("remove {reason} {}", tx.txid())
}

fn added(tx: &Tx) -> String {
    format!("add {}", tx.txid())
}

fn sorted(mut events: Vec<String>) -> Vec<String> {
    events.sort();
    events
}

/// A [`PoolSink`] whose next reorg drain, when `armed`, reads [`faulting_outpoint`] through V's
/// coins as a read fault: the `Fatal` a failed `CoinsTip` read gives the filter (`ReorgCx::access_coin`).
struct FaultAtDrain {
    inner: PoolSink,
    armed: Arc<AtomicBool>,
}

const INJECTED: &str = "injected coins read fault";

fn faulting_outpoint() -> OutPoint {
    OutPoint::new(TxHash::from([6; 32]), 0)
}

/// V's reorg context, with [`faulting_outpoint`]'s read failing.
struct FaultingCx<'a> {
    inner: &'a mut dyn ReorgCx,
}

impl TipFacts for FaultingCx<'_> {
    fn tip(&self) -> Option<BlockId> {
        self.inner.tip()
    }
    fn ancestry(&self) -> &dyn dash_consensus::ChainAncestry {
        self.inner.ancestry()
    }
    fn tip_height(&self) -> Option<u32> {
        self.inner.tip_height()
    }
    fn tip_mtp(&self) -> i64 {
        self.inner.tip_mtp()
    }
}

impl ReorgCx for FaultingCx<'_> {
    fn chain_contains(&self, block: BlockId) -> bool {
        self.inner.chain_contains(block)
    }
    fn access_coin(
        &mut self,
        outpoint: &OutPoint,
    ) -> Result<Option<dash_consensus::coins::Coin>, Fatal> {
        if *outpoint == faulting_outpoint() {
            return Err(Fatal::new(INJECTED));
        }
        self.inner.access_coin(outpoint)
    }
    fn uncache(&mut self, outpoints: &[OutPoint]) -> Result<(), Fatal> {
        self.inner.uncache(outpoints)
    }
    fn with_turn(&mut self, f: &mut dyn FnMut(&mut MempoolTurn<'_>)) -> bool {
        self.inner.with_turn(f)
    }
}

impl MempoolSink for FaultAtDrain {
    fn begin_hold(&mut self) -> Option<Box<dyn HeldMempool>> {
        let inner = self.inner.begin_hold()?;
        Some(Box::new(FaultHold {
            inner,
            armed: self.armed.clone(),
        }))
    }
    fn dynamic_memory_usage(&self) -> u64 {
        self.inner.dynamic_memory_usage()
    }
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        self.inner.new_disconnect_pool()
    }
    fn has_mempool(&self) -> bool {
        self.inner.has_mempool()
    }
}

struct FaultHold {
    inner: Box<dyn HeldMempool>,
    armed: Arc<AtomicBool>,
}

impl HeldMempool for FaultHold {
    fn dynamic_memory_usage(&self) -> u64 {
        self.inner.dynamic_memory_usage()
    }
    fn remove_for_block_at(
        &mut self,
        vtx: &[TxRef],
        height: u32,
        tip_list: &Arc<MnList>,
        staged: &mut Staged,
    ) {
        self.inner.remove_for_block_at(vtx, height, tip_list, staged);
    }
    fn remove_expired_asset_unlock(&mut self, height: u32, staged: &mut Staged) {
        self.inner.remove_expired_asset_unlock(height, staged);
    }
    fn remove_recursive(&mut self, tx: &TxRef, reason: RemovalReason, staged: &mut Staged) {
        self.inner.remove_recursive(tx, reason, staged);
    }
    fn add_transactions_updated(&mut self, n: u32, staged: &mut Staged) {
        self.inner.add_transactions_updated(n, staged);
    }
    fn check(&mut self, next_height: u32, staged: &mut Staged) {
        self.inner.check(next_height, staged);
    }
    fn maybe_update_for_reorg_at(
        &mut self,
        pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<MnList>>,
        cx: &mut dyn ReorgCx,
        staged: &mut Staged,
    ) -> Result<(), Fatal> {
        if self.armed.swap(false, Ordering::SeqCst) {
            let mut faulting = FaultingCx {
                inner: cx,
            };
            return self.inner.maybe_update_for_reorg_at(pool, tip_list, &mut faulting, staged);
        }
        self.inner.maybe_update_for_reorg_at(pool, tip_list, cx, staged)
    }
}

/// One PERIODIC `FlushStateToDisk` call: the pool usage V lent it and whether the pool was
/// write-held (a V hold) as it ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Periodic {
    usage: u64,
    in_hold: bool,
}

/// A [`FlushSite`] that decides nothing and records each PERIODIC call.
struct RecordPeriodic {
    pool: Arc<Mempool>,
    seen: Arc<Mutex<Vec<Periodic>>>,
}

impl FlushSite for RecordPeriodic {
    fn at_site(
        &mut self,
        mode: FlushStateMode,
        host: &mut FlushHost<'_>,
    ) -> Result<SiteDecision, Fatal> {
        if mode == FlushStateMode::Periodic {
            let call = Periodic {
                usage: host.mempool_usage(),
                in_hold: self.pool.state_lock().try_read().is_none(),
            };
            self.seen.lock().expect("periodic").push(call);
        }
        Ok(SiteDecision::nothing())
    }

    fn after_step(&mut self, _: SiteDecision, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }

    fn service(&mut self, _: &mut FlushHost<'_>) -> Result<(), Fatal> {
        Ok(())
    }
}

/// One V over block files on disk, coins in memory, `NoEvo`, and the real pool behind
/// [`FaultAtDrain`].
struct Rig {
    abort: Arc<RecordingAbort>,
    clock: Arc<MockClock>,
    v: ChainHandle,
    join: NodeJoinHandle<VExit>,
    pool: Arc<Mempool>,
    armed: Arc<AtomicBool>,
    events: Arc<Events>,
    periodic: Arc<Mutex<Vec<Periodic>>>,
    signals: ValidationSignals,
    thread: SignalsThread,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn start(chainlocks: bool) -> Rig {
        Rig::start_with(chainlocks, params())
    }

    fn start_with(chainlocks: bool, p: Arc<ChainSetup>) -> Rig {
        let abort = Arc::new(RecordingAbort::default());
        let signals = ValidationSignals::default();
        let events = Arc::new(Events::default());
        signals.register(events.clone());
        let thread = signals.start(abort.clone()).expect("the signals thread");
        // The spends here are bare `OP_TRUE`s, which the re-add's ATMP takes as `-acceptnonstdtxn`.
        let options = MemPoolOptions {
            require_standard: false,
            ..MemPoolOptions::default()
        };
        let pool = Arc::new(Mempool::with_signals(options, signals.clone()));
        let armed = Arc::new(AtomicBool::new(false));
        let sink = FaultAtDrain {
            inner: PoolSink::new(pool.clone(), p.clone()),
            armed: armed.clone(),
        };
        let periodic = Arc::new(Mutex::new(Vec::new()));
        let flush = RecordPeriodic {
            pool: pool.clone(),
            seen: periodic.clone(),
        };
        let V {
            abort,
            clock,
            v,
            join,
            dir,
        } = V::start_with_flush(chainlocks, &p, Box::new(sink), Box::new(flush), &signals, &abort);
        pool.set_clock(clock.clone());
        Rig {
            abort,
            clock,
            v,
            join,
            pool,
            armed,
            events,
            periodic,
            signals,
            thread,
            _dir: dir,
        }
    }

    /// `block` processed: `false` once V has stopped.
    fn submit(&self, block: &Arc<Block>) -> bool {
        self.clock.set_mocktime(i64::from(block.header().raw().time()));
        let Ok(result) = self.v.process_block(block.clone(), true, None, true).recv() else {
            return false;
        };
        assert!(result.accepted, "{} accepted: {result:?}", block.hash());
        true
    }

    /// Each block processed and connected as the tip.
    fn connect(&self, blocks: &[Arc<Block>]) {
        for block in blocks {
            assert!(self.submit(block), "V stopped at {}", block.hash());
            let reports = self.abort.messages();
            assert_eq!(self.tip_hash(), block.hash(), "{} is the tip: {reports:?}", block.hash());
        }
    }

    fn tip_hash(&self) -> BlockHash {
        self.v.view().snap.tip.tip_hash
    }

    fn tip(&self) -> BlockId {
        self.v.view().snap.tip.tip
    }

    /// The published DMN tip (D16).
    fn dmn_tip(&self) -> Option<BlockId> {
        self.v.view_handle().view().dmn_tip
    }

    /// What the listener saw since the last call, every earlier signal run.
    fn seen(&self) -> Vec<String> {
        self.signals.sync(&mut dashd_chainstate::chain::Blocking).expect("signals");
        self.events.take()
    }

    fn add(&self, tx: &TxRef, lock_points: LockPoints, spends_coinbase: bool) {
        let mut state = self.pool.write();
        let time = state.now();
        add_to(&mut state, tx, lock_points, spends_coinbase, time);
        self.pool.publish_usage(&state);
    }

    fn resident(&self, tx: &Tx) -> bool {
        self.pool.read().exists(&tx.txid())
    }

    fn lock_points(&self, tx: &Tx) -> LockPoints {
        self.pool.read().get_iter(&tx.txid()).expect("resident").metadata().lock_points
    }

    fn ancestors(&self, tx: &Tx) -> u64 {
        self.pool.read().get_iter(&tx.txid()).expect("resident").ancestors().count
    }

    /// The PERIODIC flushes since the last call.
    fn periodic(&self) -> Vec<Periodic> {
        std::mem::take(&mut *self.periodic.lock().expect("periodic"))
    }

    /// V stopped: its fatal, if any, and the abort reports.
    fn stop(self) -> (Option<Fatal>, Vec<String>) {
        self.v.shutdown();
        let exit = self.join.join().expect("an unmarked joiner").expect("V does not panic");
        let _ = self.thread.stop();
        (exit.fatal, self.abort.messages())
    }
}

/// `tx` admitted at `time` (V's clock), so `LimitMempoolSize`'s `Expire` keeps it.
fn add_to(
    state: &mut PoolState,
    tx: &TxRef,
    lock_points: LockPoints,
    spends_coinbase: bool,
    time: i64,
) {
    let metadata = EntryMetadata {
        fee: 1_000,
        time,
        lock_points,
        spends_coinbase,
        ..EntryMetadata::default()
    };
    let entry = state.new_entry(tx.clone(), metadata);
    state.add_unchecked(entry, &MnList::default_pre_dip3());
}

/// Lock points without relative locks, `maxInputBlock` at `block`.
fn open_at(block: BlockId) -> LockPoints {
    LockPoints {
        height: -1,
        time: -1,
        max_input_block: Some(block),
    }
}

/// `ConnectTip` (`validation.cpp:3477-3481` at v24.0.0) with the pool: `removeForBlock` takes the
/// mined entry silently and, for the block's spend of its second output, the in-pool spender's
/// tree as CONFLICT, `removeExpiredAssetUnlock(nHeight)`
/// takes the unlock whose expiry is below the block's height as EXPIRY, all before the block's
/// `BlockConnected` (queued by ABC after the step, `:3611-3614`); the mined entry's child stays.
/// D16 (CH7 §11.2): the DMN tip follows each ordinary connect.
#[test]
fn connect_removes_mined_conflicts_and_expired_unlocks_before_block_connected() {
    let m = Matured::new();
    let rig = Rig::start(false);
    rig.connect(&m.chain[1..]);
    assert_eq!(rig.dmn_tip(), Some(rig.tip()));
    let open = open_at(rig.tip());
    let mined = spend(&[(m.coin_at(1), u32::MAX)], 0, 20_000);
    let mined_child = child(&mined, 5_000);
    let second = OutPoint::new(mined.txid(), 1);
    let rival = spend(&[(second, u32::MAX)], 0, 8_000);
    let rival_child = child(&rival, 2_000);
    let expiring = unlock(7, 52);
    let staying = unlock(8, 53);
    for tx in [&mined, &mined_child, &rival, &rival_child, &expiring, &staying] {
        rig.add(tx, open, false);
    }
    let block_rival = spend(&[(second, u32::MAX)], 0, 9_000);
    rig.seen();

    let block = m.on(m.tip(), MATURITY + 1, 0, &[mined.clone(), block_rival]);
    rig.connect(std::slice::from_ref(&block));
    let seen = rig.seen();
    assert_eq!(
        sorted(seen[..2].to_vec()),
        sorted(vec![removed("Conflict", &rival), removed("Conflict", &rival_child)])
    );
    assert_eq!(
        seen[2..],
        [removed("Expiry", &expiring), format!("block {}", block.hash())],
        "{seen:?}"
    );
    assert!(!rig.resident(&mined) && !rig.resident(&rival) && !rig.resident(&expiring));
    assert!(rig.resident(&mined_child) && rig.resident(&staying));
    assert_eq!(rig.dmn_tip(), Some(rig.tip()));
    assert_eq!(rig.pool.usage_telemetry(), rig.pool.read().dynamic_memory_usage() as u64);

    let (fatal, reports) = rig.stop();
    assert!(fatal.is_none(), "{fatal:?}");
    assert!(reports.is_empty(), "{reports:?}");
}

/// `InvalidateBlock`'s disconnect-only adding drain (`validation.cpp:3976`, `:425-525` at
/// v24.0.0): D16 moves the DMN tip to the parent first; the disconnected block's transaction goes
/// back through `AcceptToMemoryPool` (`:446-458`) after the block's `BlockDisconnected`, and
/// `UpdateTransactionsFromBlock` links its in-pool descendants to it (`txmempool.cpp:108-161`);
/// then `removeForReorg` removes the entry no longer final at the new tip and the spend of a
/// coinbase now immature, and refreshes the lock points whose `maxInputBlock` was the
/// disconnected block; the rest stays, unchanged.
#[test]
fn invalidate_drain_readds_and_links_then_filters_and_refreshes_lock_points() {
    let m = Matured::new();
    let rig = Rig::start(false);
    let genesis = rig.tip();
    rig.connect(&m.chain[1..]);
    let parent = rig.tip();
    let mined = spend(&[(m.coin_at(1), u32::MAX)], 0, 20_000);
    rig.add(&mined, open_at(genesis), false);
    let block = m.on(m.tip(), MATURITY + 1, 0, std::slice::from_ref(&mined));
    rig.connect(std::slice::from_ref(&block));
    let disconnected = rig.tip();
    let open = open_at(genesis);
    let descendant = child(&mined, 5_000);
    let grandchild = child(&descendant, 1_000);
    let non_final = spend(&[(OutPoint::new(TxHash::from([5; 32]), 0), NON_FINAL)], MATURITY + 1, 1);
    let immature = spend(&[(m.coin_at(2), u32::MAX)], 0, 20_000);
    let stale = spend(&[(m.coin_at(3), u32::MAX)], 0, 20_000);
    let kept = spend(&[(m.coin_at(4), u32::MAX)], 0, 20_000);
    for tx in [&descendant, &grandchild, &non_final, &kept] {
        rig.add(tx, open, false);
    }
    rig.add(&immature, open, true);
    rig.add(&stale, open_at(disconnected), false);
    assert_eq!(rig.ancestors(&descendant), 1);
    rig.seen();

    assert_eq!(rig.v.invalidate_block(block.hash()).recv(), Ok(Ok(())));
    assert_eq!(rig.tip(), parent);
    assert_eq!(rig.dmn_tip(), Some(parent), "D16: the adding drain moved the DMN tip");
    let seen = rig.seen();
    assert_eq!(seen[..2], [format!("disconnect {}", block.hash()), added(&mined)], "{seen:?}");
    assert_eq!(
        sorted(seen[2..].to_vec()),
        sorted(vec![removed("Reorg", &non_final), removed("Reorg", &immature)]),
        "{seen:?}"
    );
    for tx in [&mined, &descendant, &grandchild, &stale, &kept] {
        assert!(rig.resident(tx), "{} resident", tx.txid());
    }
    assert_eq!((rig.ancestors(&descendant), rig.ancestors(&grandchild)), (2, 3));
    assert_eq!(rig.lock_points(&stale), open_at(genesis), "refreshed off the stale block");
    assert_eq!(rig.lock_points(&kept), open);
    rig.pool.read().check();

    let (fatal, reports) = rig.stop();
    assert!(fatal.is_none(), "{fatal:?}");
    assert!(reports.is_empty(), "{reports:?}");
}

/// ChainLock enforcement (`EnforceBlock` → `MarkConflictingBlock`, `validation.cpp:4089-4140` at
/// v24.0.0): the block conflicting with the ChainLock is disconnected and its transactions go
/// back through `AcceptToMemoryPool` (`MaybeUpdateMempoolForReorg(disconnectpool, true)`,
/// `:4137`), the coinbase excepted; a re-added parent takes in its in-pool child. When the locked
/// block then connects, its spend of the coin a re-added transaction spent removes that one as
/// CONFLICT (`removeForBlock`, `txmempool.cpp:1321-1353`) before its `BlockConnected`.
#[test]
fn chainlock_enforcement_readds_the_marked_blocks_transactions() {
    let m = Matured::new();
    let rig = Rig::start(true);
    rig.connect(&m.chain[1..]);
    let base = m.on(m.tip(), MATURITY + 1, 0, &[]);
    rig.connect(std::slice::from_ref(&base));
    let kept = spend(&[(m.coin_at(1), u32::MAX)], 0, 20_000);
    let conflicted = spend(&[(m.coin_at(2), u32::MAX)], 0, 20_000);
    let marked = m.on(&base, MATURITY + 2, 0, &[kept.clone(), conflicted.clone()]);
    rig.connect(std::slice::from_ref(&marked));
    let kept_child = child(&kept, 5_000);
    rig.add(&kept_child, open_at(rig.tip()), false);
    let rival = spend(&[(m.coin_at(2), u32::MAX)], 0, 21_000);
    let locked = m.on(&base, MATURITY + 2, 1, std::slice::from_ref(&rival));
    assert!(rig.submit(&locked));
    assert_eq!(rig.tip_hash(), marked.hash(), "the rival block is stored, not connected");
    rig.v.cl_intake().recv().expect("V replies");
    rig.seen();

    let sig = Arc::new(ChainLockSig {
        height: (MATURITY + 2) as i32,
        block_hash: locked.hash(),
        sig: ChainLockSig::null().sig,
    });
    let _ = rig.v.record_chainlock(sig, Hash256::from_lendian([0x5b; 32]), None).recv();
    let deadline = Instant::now() + Duration::from_secs(120);
    while rig.tip_hash() != locked.hash() {
        assert!(Instant::now() < deadline, "the enforcement never reached the locked block");
        std::thread::sleep(Duration::from_millis(5));
    }
    rig.v.cl_intake().recv().expect("V ran the enforcement's scope");
    let seen = rig.seen();
    let at = |event: &str| {
        seen.iter().position(|e| e == event).unwrap_or_else(|| panic!("{event}: {seen:?}"))
    };
    assert!(at(&format!("disconnect {}", marked.hash())) < at(&added(&kept)), "{seen:?}");
    assert!(at(&added(&kept)) < at(&added(&conflicted)), "re-added in block order: {seen:?}");
    assert!(at(&added(&conflicted)) < at(&removed("Conflict", &conflicted)), "{seen:?}");
    assert!(at(&removed("Conflict", &conflicted)) < at(&format!("block {}", locked.hash())));
    assert!(rig.resident(&kept) && rig.resident(&kept_child));
    assert!(!rig.resident(&conflicted) && !rig.resident(&rival));
    assert!(!rig.resident(&marked.txs()[0]), "the coinbase is not re-added");
    assert_eq!(rig.ancestors(&kept_child), 2);
    rig.pool.read().check();

    let (fatal, reports) = rig.stop();
    assert!(fatal.is_none(), "{fatal:?}");
    assert!(reports.is_empty(), "{reports:?}");
}

/// Carried from MP1a-4 (m3): a coins read fault in the reorg filter, whether the drain is
/// `InvalidateBlock`'s or the ChainLock enforcement's (`EnforceBestChainLock`,
/// `llmq/chainlocks.cpp`; `validation.cpp:425-525` at v24.0.0), stops V without a verdict:
/// `removeForReorg` applies nothing (no lock points refreshed, no filtered entry removed; the
/// faulting entry is the last the filter visits) and the guard is released. V discards the hold's
/// staged signals, so no removal is published; what the drain applied before the filter stays,
/// with its events queued to the pool's queue (MP1a-8 review m1). `InvalidateBlock` drains after
/// each disconnect, so its first drain, of the transactionless tip, faults; the enforcement
/// drains once, after both disconnects, so the first block's transaction is re-added first.
#[test]
fn drain_coins_fault_applies_nothing_of_the_filter_and_releases_the_guard() {
    for enforce in [false, true] {
        let m = Matured::new();
        let rig = Rig::start(enforce);
        let genesis = rig.tip();
        rig.connect(&m.chain[1..]);
        let base = m.tip().clone();
        let mined = spend(&[(m.coin_at(1), u32::MAX)], 0, 20_000);
        let a1 = m.on(&base, MATURITY + 1, 0, std::slice::from_ref(&mined));
        let main = [a1.clone(), m.on(&a1, MATURITY + 2, 0, &[])];
        rig.connect(&main[..1]);
        let first = rig.tip();
        rig.connect(&main[1..]);
        let side = m.on(&base, MATURITY + 1, 1, &[]);
        if enforce {
            assert!(rig.submit(&side));
            rig.v.cl_intake().recv().expect("V replies");
        }

        let stale = spend(&[(m.coin_at(3), u32::MAX)], 0, 20_000);
        let stale_points = open_at(first);
        let non_final = spend(&[(OutPoint::new(TxHash::from([5; 32]), 0), NON_FINAL)], 101, 1);
        let last = stale.txid().max(non_final.txid());
        let faulting = (1..)
            .map(|value| spend(&[(faulting_outpoint(), u32::MAX)], 0, value))
            .find(|tx| tx.txid() > last)
            .expect("a txid after every other entry");
        rig.add(&stale, stale_points, false);
        rig.add(&non_final, open_at(genesis), false);
        rig.add(&faulting, open_at(genesis), true);
        rig.seen();

        rig.armed.store(true, Ordering::SeqCst);
        if enforce {
            let sig = Arc::new(ChainLockSig {
                height: (MATURITY + 1) as i32,
                block_hash: side.hash(),
                sig: ChainLockSig::null().sig,
            });
            let _ = rig.v.record_chainlock(sig, Hash256::from_lendian([0x5a; 32]), None).recv();
        } else {
            let reply = rig.v.invalidate_block(main[0].hash()).recv();
            assert_eq!(reply, Err(WaitError::Disconnected));
        }
        let events = rig.events.clone();
        let pool = rig.pool.clone();
        let (fatal, reports) = rig.stop();
        assert!(format!("{fatal:?}").contains(INJECTED), "enforce={enforce}: {fatal:?}");
        assert_eq!(reports.len(), 1, "one report, enforce={enforce}: {reports:?}");
        assert!(reports[0].contains(INJECTED), "enforce={enforce}: {reports:?}");
        let published = events.take();
        assert!(
            published.iter().all(|e| !e.starts_with("remove")),
            "no removal published, enforce={enforce}: {published:?}"
        );
        assert_eq!(
            published.contains(&added(&mined)),
            enforce,
            "the re-add's event queued, enforce={enforce}: {published:?}"
        );
        assert!(pool.state_lock().try_write().is_some(), "released, enforce={enforce}");
        let state = pool.read();
        assert_eq!(state.exists(&mined.txid()), enforce, "the re-add stays, enforce={enforce}");
        state.check();
        assert!(state.exists(&non_final.txid()), "no filtered removal applied, enforce={enforce}");
        assert_eq!(
            state.get_iter(&stale.txid()).expect("resident").metadata().lock_points,
            stale_points,
            "no lock points applied, enforce={enforce}"
        );
    }
}

/// `CheckSpecialTx` (`evo/specialtxman.cpp:217-229`, `:276-281` at v24.0.0) through a real class-M
/// turn: below DIP0003 a special transaction is TX_BAD_SPECIAL `bad-tx-type-dip3-inactive` before
/// any evo read, so a node without evo state still rejects it as Core does; past DIP0003 a check
/// without evo state is a fault, never a verdict.
#[test]
fn mempool_turn_checks_special_tx_against_the_dip3_gate_before_evo_state() {
    for (dip3, inactive) in [(NO_DIP3, true), (1, false)] {
        let rig = Rig::start_with(false, params_with(dip3));
        let tip = rig.tip();
        let tx = unlock(1, 0);
        let (send, recv) = mpsc::channel();
        rig.v
            .send(ChainCommand::Mempool(Box::new(move |turn| {
                let rules = SpecialTxRules {
                    v24: true,
                    ..SpecialTxRules::default()
                };
                let checked = turn.check_special_tx(&tx, tip, rules);
                Ok(MempoolCompletion {
                    uncache: vec![],
                    reply: Box::new(move || send.send(checked).expect("the test waits")),
                })
            })))
            .expect("V runs");
        let checked = recv.recv().expect("the turn replies");
        if inactive {
            let Ok(Err(e)) = checked else {
                panic!("dip3={dip3}: {checked:?}")
            };
            assert_eq!(e.result(), TxValidationResult::BadSpecial);
            assert_eq!(e.reject_reason(), "bad-tx-type-dip3-inactive");
        } else {
            assert_eq!(checked, Err(Fatal::new("CheckSpecialTx without evo state")));
        }
        let (fatal, reports) = rig.stop();
        assert!(fatal.is_none(), "{fatal:?}");
        assert!(reports.is_empty(), "{reports:?}");
    }
}

/// `AcceptToMemoryPool`'s `FlushStateToDisk(PERIODIC)` (`validation.cpp:1757-1759` at v24.0.2)
/// in `InvalidateBlock`'s adding drain: one call per re-added transaction, the coinbase excepted,
/// made inside V's hold without a deadlock, each lent the held pool's usage after that
/// transaction's admission.
#[test]
fn the_readd_flushes_periodic_per_atmp_call_inside_the_hold() {
    let m = Matured::new();
    let rig = Rig::start(false);
    rig.connect(&m.chain[1..]);
    let first = spend(&[(m.coin_at(1), u32::MAX)], 0, 20_000);
    let second = spend(&[(m.coin_at(2), u32::MAX)], 0, 20_000);
    let base = m.on(m.tip(), MATURITY + 1, 0, &[]);
    let block = m.on(&base, MATURITY + 2, 0, &[first.clone(), second.clone()]);
    rig.connect(&[base, block.clone()]);
    assert!(rig.periodic().iter().all(|call| !call.in_hold), "no drain, no held flush");
    rig.seen();

    assert_eq!(rig.v.invalidate_block(block.hash()).recv(), Ok(Ok(())));
    let held: Vec<_> = rig.periodic().into_iter().filter(|call| call.in_hold).collect();
    assert!(rig.resident(&first) && rig.resident(&second));
    let usage = rig.pool.read().dynamic_memory_usage() as u64;
    assert_eq!(held.len(), 2, "{held:?}");
    assert!(0 < held[0].usage && held[0].usage < usage, "{held:?}");
    assert_eq!(held[1].usage, usage);
    let seen = rig.seen();
    assert_eq!(seen[1..], [added(&first), added(&second)], "{seen:?}");

    let (fatal, reports) = rig.stop();
    assert!(fatal.is_none(), "{fatal:?}");
    assert!(reports.is_empty(), "{reports:?}");
}
