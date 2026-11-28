//! MP1a-9: the node's live pool on the production startup and shutdown path. V runs its tip steps
//! through it, its admissions reach V only once V's evo state is attached, the flush reads its
//! usage as V's per-call snapshot, `LoadMempool`'s slot sets its load-tried flag, and shutdown
//! joins its admission worker.

use std::sync::mpsc;
use std::time::Duration;

use dash_consensus::TxRef;
use dashd_chainstate::chain::{ChainHandle, ValidationInterface, WaitError};
use dashd_chainstate::coins::{CoinSrc, OutPointKey};
use dashd_chainstate::flush::{FlushCauses, FlushStateMode};
use dashd_mempool::Mempool;
use dashd_mempool::types::{MempoolAcceptResult, MempoolAcceptResultType};

use super::*;

pub(super) const COIN: i64 = 100_000_000;
pub(super) const PAID: i64 = 30 * COIN;
pub(super) const FEE: i64 = 10_000;

/// A NodeSut whose V runs through the node's live pool, as rdashd's, on regtest with
/// `-acceptnonstdtxn` (the spends below are bare `OP_TRUE`s).
pub(super) fn live(dir: &Path, argv: &[&str]) -> (NodeSut, Arc<Mutex<Vec<String>>>) {
    let (ui, lines) = logged_ui();
    let mut full = vec!["-regtest", "-printtoconsole=0", "-acceptnonstdtxn"];
    full.extend_from_slice(argv);
    let harness = Harness {
        live_mempool: true,
        ..Harness::default()
    };
    let sut = NodeSut::with_harness(dir, &full, MOCKTIME, ui, harness)
        .unwrap_or_else(|refused| panic!("{argv:?} refused: {refused:?}"));
    (sut, lines)
}

pub(super) fn pool(sut: &NodeSut) -> Arc<Mempool> {
    Arc::clone(sut.steps.mempool().expect("step 7 built it").pool())
}

/// Blocks 1..=`n` on genesis, old enough to keep V in initial block download, each coinbase
/// paying [`PAID`] to `OP_TRUE`; the coinbase of height `h` spends at height `h + 100`.
pub(super) fn mature(sut: &NodeSut, n: u32) -> Vec<Arc<dash_consensus::Block>> {
    let p = sut.params.chain.clone();
    let mut tip = dashd_chainstate::chain::genesis_block(&p);
    let t0 = tip.header().time();
    let mut blocks = Vec::new();
    for h in 1..=n {
        tip = mine(&p, tip.header(), h, t0 + h, &[(PAID, &[0x51])], vec![]);
        submit(sut, &tip);
        blocks.push(Arc::clone(&tip));
    }
    assert_eq!(sut.chain().unwrap().tip_watch().get(), Some(tip.hash()), "connected");
    blocks
}

/// Spends `block`'s coinbase to `script`, paying [`FEE`].
pub(super) fn spend_to(block: &dash_consensus::Block, script: &[u8]) -> TxRef {
    let input = TxIn::new(OutPoint::new(block.txs()[0].txid(), 0), Vec::new().into(), u32::MAX);
    tx(vec![input], &[(PAID - FEE, script)])
}

/// Spends `block`'s coinbase to a 40-byte `OP_TRUE` script, above `tx-size-small`.
fn spend(block: &dash_consensus::Block) -> TxRef {
    spend_to(block, &[0x51; 40])
}

pub(super) fn assert_valid(result: &MempoolAcceptResult) {
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid, "{result:?}");
    assert_eq!(result.base_fee, Some(FEE), "{result:?}");
}

/// The node's pool on a live V: a normal start sets the load-tried flag (`SetLoadTried` at
/// `LoadMempool`'s slot); an admission is a V turn; a clean restart joins the worker with no
/// error and comes back with a new, empty pool, its flag set again.
// Core: init.cpp:2035-2037, :460; node/blockstorage.cpp:964; validation.cpp:4995-5000 at v24.0.0
#[test]
fn the_live_pool_starts_admits_and_stops_with_the_node() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, lines) = live(dir.path(), &[]);
    loaded(&sut);
    imported(&mut sut);
    assert!(sut.steps.mempool().unwrap().load_tried(), "a normal start reached LoadMempool");
    let blocks = mature(&sut, 101);
    let a = spend(&blocks[0]);
    let handle = sut.steps.mempool().unwrap().handle();
    assert_valid(&handle.accept(a.clone(), false).expect("V replies"));
    assert!(pool(&sut).read().exists(&a.txid()));

    assert_eq!(sut.restart(Teardown::Clean), 2);
    assert_eq!(sut.shutdown.exit_status(), 0);
    assert!(!lines.lock().unwrap().iter().any(|l| l.contains("admission worker")));
    loaded(&sut);
    imported(&mut sut);
    let mempool = sut.steps.mempool().expect("a new pool");
    assert!(mempool.load_tried());
    assert_eq!(mempool.pool().size(), 0, "no mempool.dat yet (MP1b)");
    assert_valid(&mempool.handle().accept(a, false).expect("V replies"));
}

/// DEC-1867: the pool's admissions reach V only once V's evo state is attached. Just before
/// the attach (the coins and evo attached, the worker not), an admission replies
/// `Disconnected` and V is sent nothing; once the load has attached it, the same admission is
/// admitted.
// Core: init.cpp:2035-2037, validation.cpp:4920-4932 at v24.0.0
#[test]
fn an_admission_before_the_attach_never_reaches_v() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    let blocks = mature(&sut, 101);
    let a = spend(&blocks[0]);
    let (send, seen) = mpsc::channel();
    let early = a.clone();
    sut.next_before_pool_attach = Some(Box::new(move |pool, chain| {
        let v = chain.handle();
        let sent = v.commands_sent();
        let reply = pool.handle().accept(early, false);
        send.send((reply, v.commands_sent() - sent)).expect("the test");
    }));
    assert_eq!(sut.restart(Teardown::Clean), 2);
    loaded(&sut);
    assert_eq!(seen.try_recv(), Ok((Err(WaitError::Disconnected), 0)), "not sent to V");
    imported(&mut sut);
    let mempool = sut.steps.mempool().unwrap();
    assert_valid(&mempool.handle().accept(a, false).expect("V replies"));
}

/// The pool's entries survive a block connect and a disconnect with a flush between, with no
/// deadlock: the connect removes the mined entry (BLOCK) and keeps the other, the ALWAYS flush
/// samples the pool outside any hold, and the disconnect's hold leaves the kept entry and puts
/// the mined one back (`MaybeUpdateMempoolForReorg(disconnectpool, true)`, MP1b-R).
// Core: validation.cpp:2987-3133, :3290-3348, :3799-3811, :3976, txmempool.cpp:1321-1353 at v24.0.0
#[test]
fn a_mempool_with_txs_survives_a_connect_and_a_disconnect_with_a_flush_between() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, _) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 102);
    let (a, b) = (spend(&blocks[0]), spend(&blocks[1]));
    let handle = sut.steps.mempool().unwrap().handle();
    assert_valid(&handle.accept(a.clone(), false).unwrap());
    assert_valid(&handle.accept(b.clone(), false).unwrap());
    let p = sut.params.chain.clone();
    let tip = blocks.last().unwrap();
    let block =
        mine(&p, tip.header(), 103, tip.header().time() + 1, &[(0, &[0x51])], vec![a.clone()]);
    submit(&sut, &block);
    let v = sut.chain().unwrap().clone();
    assert_eq!(v.tip_watch().get(), Some(block.hash()));
    let held = |pool: &Mempool| (pool.read().exists(&a.txid()), pool.read().exists(&b.txid()));
    assert_eq!(held(&pool(&sut)), (false, true), "the mined entry left, the other stayed");
    assert_eq!(v.flush(FlushStateMode::Always).recv(), Ok(()));
    assert_eq!(v.invalidate_block(block.hash()).recv(), Ok(Ok(())));
    assert_eq!(v.tip_watch().get(), Some(tip.hash()));
    assert_eq!(held(&pool(&sut)), (true, true), "re-added, and the kept entry survived");
    assert!(sut.settle().is_some());
    assert!(!sut.shutdown.aborted());
    assert_eq!(sut.down(), None);
}

/// `-bytespersigop=30` reaches the entry's vsize: 80 legacy sigops make
/// `max(size, 80 * 30)`, where the default 20 makes `80 * 20`.
// Core: policy/policy.cpp:278-286, init.cpp:1366 at v24.0.0
#[test]
fn bytespersigop_reaches_the_entry_vsize() {
    for (arg, per_sigop) in [(None, 20), (Some("-bytespersigop=30"), 30)] {
        let dir = tempfile::tempdir().unwrap();
        let argv: Vec<&str> = arg.into_iter().collect();
        let (mut sut, _) = live(dir.path(), &argv);
        imported(&mut sut);
        let blocks = mature(&sut, 101);
        let a = spend_to(&blocks[0], &[0xac; 80]);
        assert!(a.size() < 80 * 20, "the sigops dominate");
        let mempool = sut.steps.mempool().unwrap();
        assert_valid(&mempool.handle().accept(a.clone(), false).unwrap());
        let info = mempool.pool().info(&a.txid()).expect("resident");
        assert_eq!(info.vsize, 80 * per_sigop, "{arg:?}");
    }
}

/// A shutdown during the import leaves the flag unset: under `-reindex` with `-stopatheight`
/// V requests the shutdown in the import's ABC, so `SetLoadTried(!ShutdownRequested())` records
/// `false` and the load ends interrupted; the same reindex without it sets the flag.
// Core: validation.cpp:4995-5000, node/blockstorage.cpp:950-964 at v24.0.0
#[test]
fn a_shutdown_during_the_import_leaves_the_flag_unset() {
    for stop in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut sut, _) = live(dir.path(), &[]);
        mature(&sut, 12);
        sut.args.force_set_arg("-reindex", "1");
        if stop {
            sut.args.force_set_arg("-stopatheight", "6");
        }
        let (send, built) = mpsc::channel();
        sut.next_before_pool_attach = Some(Box::new(move |pool, _| {
            send.send(Arc::clone(pool)).expect("the test");
        }));
        sut.restart(Teardown::Clean);
        let mempool = built.try_recv().expect("the load built a pool");
        assert_eq!(sut.down().is_some(), stop, "{:?}", sut.down());
        assert_eq!(mempool.load_tried(), !stop, "stop {stop}");
    }
}

/// A worker panic is never only a `Disconnected` reply: shutdown joins the worker once V has
/// stopped, logs its error and exits with status 1.
#[test]
fn a_worker_panic_fails_the_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, lines) = live(dir.path(), &[]);
    imported(&mut sut);
    let blocks = mature(&sut, 101);
    let mempool = Arc::clone(sut.steps.mempool().unwrap());
    mempool.pool().panic_admission_worker();
    let a = spend(&blocks[0]);
    assert_eq!(mempool.handle().accept(a, false), Err(WaitError::Disconnected));
    sut.steps.shutdown();
    assert_eq!(sut.shutdown.exit_status(), crate::shutdown::EXIT_FAILURE);
    let line = "Error: the mempool admission worker panicked: injected mempool admission worker \
                panic\n";
    assert!(lines.lock().unwrap().iter().any(|l| l.ends_with(line)), "{:?}", lines.lock().unwrap());
}

#[derive(Default)]
struct Flushed(Mutex<u32>);

impl ValidationInterface for Flushed {
    fn chain_state_flushed(&self, _: &[BlockHash]) {
        *self.0.lock().unwrap() += 1;
    }
}

/// Runs `f` in a V query turn, between commands.
fn on_v<T: Send + 'static>(v: &ChainHandle, f: impl FnOnce() -> T + Send + 'static) -> T {
    v.query(move |_| f()).recv().expect("V serves")
}

/// §3's barrier against the production adapter (`PoolSink`'s sampling, `MempoolFlushInputs`,
/// `NodeFlush`). An off-V removal batch, the two roots of one InstantSend lock under one pool
/// write guard (as `remove_conflicts_for_lock` holds it), pauses after its first root: the
/// ordinary PERIODIC flush's usage sampling waits for it before it takes the coins cell. The
/// coins are then sized between the old and the completed totals, CRITICAL at the old usage and
/// not at the completed one; the flush decides on the completed batch (THRESHOLD, no
/// CACHE_LARGE, no "exceeds total space" line) and its `ChainStateFlushed` is heard. A held
/// flush (the disconnect's hold) then completes without relocking the pool.
// Core: validation.cpp:2937-2972, :3047-3051, txmempool.cpp:1984-1988,
// instantsend/net_instantsend.cpp:474-505 at v24.0.0
#[test]
fn the_flush_samples_the_completed_off_v_batch_before_coins() {
    let dir = tempfile::tempdir().unwrap();
    let (mut sut, lines) = live(dir.path(), &["-maxmempool=5"]);
    imported(&mut sut);
    let blocks = mature(&sut, 102);
    let (a, b) = (spend(&blocks[0]), spend(&blocks[1]));
    let mempool = Arc::clone(sut.steps.mempool().unwrap());
    let handle = mempool.handle();
    assert_valid(&handle.accept(a.clone(), false).unwrap());
    assert_valid(&handle.accept(b.clone(), false).unwrap());
    let v = sut.chain().unwrap().clone();
    let cell = Arc::clone(sut.steps.coins_cell().expect("the coins attached"));
    // Native Σ past the 5 MB the empty pool would leave, in V's turn.
    let seed = Arc::clone(&cell);
    on_v(&v, move || {
        let mut coins = seed.lock().unwrap();
        let out = dash_primitives::TxOut::from_duffs(1, vec![0x51; 1000].into());
        for n in 0..6_000u32 {
            let mut hash = [0xee; 32];
            hash[..4].copy_from_slice(&n.to_le_bytes());
            let key = OutPointKey::new(&OutPoint::new(TxHash::from(hash), 0));
            let src = CoinSrc::Output {
                out: &out,
                height: 0,
                coinbase: false,
            };
            coins.add(&key, src, false).expect("seed");
        }
    });
    assert!(sut.settle().is_some());
    let flushed = Arc::new(Flushed::default());
    assert!(sut.register_listener(Arc::clone(&flushed) as _));
    sut.begin_op();

    let lock_txid = TxHash::from([0x15; 32]);
    let roots = [a.inputs()[0].prevout(), b.inputs()[0].prevout()];
    let (paused, batch_paused) = mpsc::channel();
    let (resume, finish) = mpsc::channel::<()>();
    let batch_pool = Arc::clone(mempool.pool());
    let batch_cell = Arc::clone(&cell);
    let batch = std::thread::spawn(move || {
        let _rank = dashd_chainstate::chain::mempool_turn::LockOrderGuard::enter(
            dashd_chainstate::chain::mempool_turn::LockRank::Mempool,
        );
        let mut state = batch_pool.write();
        let old = state.dynamic_memory_usage() as u64;
        assert_eq!(state.remove_conflicts_for_lock(&lock_txid, &roots[..1]).len(), 1);
        paused.send(()).expect("the test");
        finish.recv().expect("the test");
        assert_eq!(state.remove_conflicts_for_lock(&lock_txid, &roots[1..]).len(), 1);
        let new = state.dynamic_memory_usage() as u64;
        assert!(new < old);
        // Pool, then coins (§3's order): the window, while V's sampler waits on this guard.
        let mut coins = batch_cell.lock().unwrap();
        let sigma = coins.usage().total();
        let max_mempool = 5_000_000;
        assert!(sigma > max_mempool - new, "{sigma}");
        coins.resize(sigma - max_mempool + new, false);
        assert_eq!(
            coins.boundary_state(old, false),
            dashd_chainstate::coins::CoinsBoundary::Critical
        );
        assert_ne!(
            coins.boundary_state(new, false),
            dashd_chainstate::coins::CoinsBoundary::Critical
        );
        drop(coins);
        batch_pool.publish_usage(&state);
        (old, new)
    });
    batch_paused.recv().expect("the first root removed");
    let result = v.flush(FlushStateMode::Periodic);
    std::thread::sleep(Duration::from_millis(50));
    assert!(cell.try_lock().is_ok(), "V's sampling waits before the coins cell");
    assert_eq!(result.recv_timeout(Duration::from_millis(20)), None, "waits for the batch");
    resume.send(()).expect("the batch");
    let (old, new) = batch.join().expect("the batch");
    assert_eq!(result.recv(), Ok(()), "the flush completed");
    assert_eq!(mempool.pool().read().dynamic_memory_usage() as u64, new);
    assert!(sut.settle().is_some());
    let journal = sut.tally().journal();
    assert_eq!(journal.len(), 1, "{journal:?}");
    let causes = journal[0].causes;
    assert!(causes.contains(FlushCauses::THRESHOLD), "{journal:?}");
    assert!(!causes.intersects(FlushCauses::CACHE_LARGE | FlushCauses::CACHE_CRITICAL));
    assert!(!lines.lock().unwrap().iter().any(|l| l.contains("exceeds total space")), "{old}");
    assert_eq!(*flushed.0.lock().unwrap(), 1, "ChainStateFlushed");

    let tip = blocks.last().unwrap().hash();
    assert_eq!(v.invalidate_block(tip).recv(), Ok(Ok(())), "the held flush does not relock");
    assert!(!sut.shutdown.aborted());
}
