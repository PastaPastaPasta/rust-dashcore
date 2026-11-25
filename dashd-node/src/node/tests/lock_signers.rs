//! DEC-1887 item 2: a masternode's ChainLock and InstantSend signers, built at step 7c, running
//! from step 10a and stopped at shutdown; a node without a masternode key builds none.

use dash_clock::{Clock, MockClock};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{NoFlush, NullConnector, RecordingAbort, ValidationInterface};
use dashd_indexes::{IndexStatus as _, Txid};
use dashd_locks::chainlock::first_seen::TxFirstSeen;
use dashd_locks::chainlock::reads::{ChainEntry, ChainReads as _};
use dashd_locks::signing::TxSource as _;

use super::*;
use crate::chain::Chain;
use crate::chain::tests::{SharedFiles, fresh, imported, regtest, startup_over};
use crate::instantsend::IsParts;
use crate::locks::signers::{NodeLockReads, NodeTxSource, cleanup_first_seen};

/// A node on `dir` through init, its `loadblk` joined.
fn started(dir: &Path, argv: &[&str]) -> Node {
    let (mut node, _, stderr) = start(dir, argv);
    let started = node.app_init_early() && node.app_init_main();
    let errors = stderr.lock().unwrap().clone();
    assert!(started, "{errors:?}");
    assert!(node.steps.join_import(), "loadblk ends");
    node
}

fn is_signer_set(node: &Node) -> bool {
    node.steps.instantsend().expect("loaded").signer.is_set()
}

/// The reads both signers make, over `view`.
fn reads(node: &Node, view: dashd_chainstate::chain::ChainViewHandle) -> NodeLockReads {
    NodeLockReads {
        view,
        loading: Arc::default(),
        sporks: Arc::clone(node.steps.sporks().unwrap()),
        sync: node.steps.is_sync.clone().expect("mn_sync"),
        files: None,
    }
}

/// Step 7c swaps NetInstantSend's listener on V's signals for one driving the IS signer: the
/// listener it built is on V's signals, and rebuilding it takes the one it replaces off.
fn assert_is_listener_swapped(node: &mut Node) {
    let (signals, built) = node.steps.is_listener().expect("NetInstantSend's listener").clone();
    assert!(signals.is_registered(&built), "step 7c's listener hears V");
    let signers = node.lock_signers.as_ref().expect("built");
    node.steps.attach_is_signer(|| signers.listener_signer());
    let (_, rebuilt) = node.steps.is_listener().expect("rebuilt").clone();
    assert!(!Arc::ptr_eq(&built, &rebuilt));
    assert!(!signals.is_registered(&built), "the replaced listener is off");
    assert!(signals.is_registered(&rebuilt));
}

/// A regtest operator key makes step 7c build both signers and hand the IS one to NetInstantSend,
/// swapping its listener on V's signals; step 10a subscribes them to recovered signatures. The
/// tx source they read finds a mined coinbase in its block once the txindex has indexed it, and holds the first-seen cleanup back
/// until the index's tip is the active tip. Shutdown stops them: no subscription, and
/// `cls-schdlr` and the IS queue joined.
// Core: v24.0.2 src/init.cpp:2165-2170, :2201, :335; src/active/context.cpp:47-73
#[test]
fn a_masternode_runs_its_lock_signers_from_step_10a_until_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
    let mut node = started(dir.path(), &[&key, "-dnsseed=0", "-fixedseeds=0"]);
    let (subscribed, scheduler, queue) = node.lock_signers.as_ref().expect("built").probe();
    assert!(subscribed, "the ChainLock signer's hooks hear V");
    let signing = node.signing.as_ref().expect("made at step 7c");
    assert_eq!(signing.active_listeners(), Some(3), "the ChainLock and IS signers, then shareman");
    assert!(scheduler.are_threads_servicing_queue() && !queue.joined());
    assert!(is_signer_set(&node));
    assert_is_listener_swapped(&mut node);

    let mined = generate(&node, dir.path(), 2);
    let txindex = node.txindex().expect("a masternode keeps the txindex");
    let end = Instant::now() + Duration::from_secs(120);
    while txindex.summary().best_block_height != 2 {
        assert!(Instant::now() < end, "the txindex never indexed the tip");
        std::thread::sleep(Duration::from_millis(5));
    }
    let txs = NodeTxSource {
        mempool: node.mempool().map(|pool| pool.handle()),
        txindex: Some(Arc::clone(txindex)),
    };
    let table = command_table(&node, dir.path());
    let block = crate::chain::tests::rpc(&table, "getblock", &format!(r#"["{}", 1]"#, mined[1]));
    let block: serde_json::Value = serde_json::from_str(&block.expect("getblock")).unwrap();
    let coinbase: Txid = block["tx"][0].as_str().expect("a coinbase").parse().unwrap();
    let tip_hash: BlockHash = mined[1].parse().unwrap();
    assert_eq!(txs.get_transaction(&coinbase).unwrap(), Some(Some(tip_hash)));
    assert_eq!(txs.get_transaction(&Txid::from([7; 32])).unwrap(), None);
    let reads = reads(&node, node.steps.chain().unwrap().handle().view_handle());
    let tip = reads.tip().expect("a tip");
    assert_eq!(tip.hash, tip_hash);
    assert!(txs.indexed(Some(&tip)));
    let stale = ChainEntry {
        hash: BlockHash::from([7; 32]),
        ..tip
    };
    assert!(!txs.indexed(Some(&stale)) && !txs.indexed(None), "the cleanup waits");
    assert_first_seen_cleanup_waits_for_the_txindex(&node, &txs, &reads);

    node.shutdown();
    assert!(node.lock_signers.is_none());
    assert!(!scheduler.are_threads_servicing_queue(), "cls-schdlr joined");
    assert!(queue.joined(), "is-signer joined");
}

/// The scheduled first-seen cleanup erases a vanished transaction only over a tip the txindex
/// has indexed: over another chain's tip it does nothing.
// Core: v24.0.2 src/chainlock/handler.cpp:339-348, :363-383
fn assert_first_seen_cleanup_waits_for_the_txindex(
    node: &Node,
    txs: &NodeTxSource,
    reads: &NodeLockReads,
) {
    let params = regtest();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_000_000));
    let abort = Arc::new(RecordingAbort::default());
    let files = Box::new(SharedFiles::default());
    let mut other = Chain::start(fresh(&params, &clock, &abort, files)).expect("V");
    startup_over(&mut other, Box::new(NullConnector), Box::new(NoFlush));
    imported(&mut other);
    let first_seen = TxFirstSeen::new(clock, Arc::new(|| true), Arc::new(|e: &str| panic!("{e}")));
    let gone = Txid::from([8; 32]);
    first_seen.update([gone], 0);
    assert!(first_seen.is_tx_safe_for_mining(&gone));
    cleanup_first_seen(&first_seen, txs, &self::reads(node, other.handle().view_handle()));
    assert!(first_seen.is_tx_safe_for_mining(&gone), "skipped over an unindexed tip");
    cleanup_first_seen(&first_seen, txs, reads);
    assert!(!first_seen.is_tx_safe_for_mining(&gone), "erased as vanished");
    let _ = other.stop();
}

/// A listener on V's signals holding the IS parts, as the lock processor's future holders will.
struct HoldsIsParts {
    _parts: IsParts,
}

impl ValidationInterface for HoldsIsParts {}

/// Shutdown releases the IS store's last handle, so RocksDB syncs its WAL as it closes, even
/// with the IS parts held by a listener on V's signals: the stopped signers no longer hold the
/// mempool, whose signals hold that listener.
// Core: v24.0.2 src/init.cpp:335, :463
#[test]
fn a_masternode_releases_its_is_store_at_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
    let mut node = started(dir.path(), &[&key, "-dnsseed=0", "-fixedseeds=0"]);
    assert!(node.lock_signers.is_some() && is_signer_set(&node));
    let is = node.steps.instantsend().expect("loaded").clone();
    let store = Arc::downgrade(&is.store);
    node.steps.chain().unwrap().signals().register(Arc::new(HoldsIsParts {
        _parts: is,
    }));
    node.shutdown();
    assert!(store.upgrade().is_none(), "the IS store's last handle dropped");
}

/// Without an operator key there is no signer: NetInstantSend's `m_signer` stays null.
// Core: v24.0.2 src/init.cpp:2159, :2201
#[test]
fn a_node_without_a_masternode_key_builds_no_lock_signers() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[]);
    assert!(node.lock_signers.is_none());
    assert!(!is_signer_set(&node));
    node.shutdown();
}
