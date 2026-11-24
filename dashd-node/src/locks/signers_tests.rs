use std::sync::mpsc;
use std::time::Instant;

use dash_clock::MockClock;
use dash_consensus::primitives::{OutPoint, Transaction, TxIn, TxType};
use dash_evo::locks::ChainLockSig;
use dash_kv::MemoryEngine;
use dash_num::Hash256;
use dash_types::Numeric as _;
use dashd_chainstate::chain::{NoFlush, NullConnector, RecordingAbort, genesis_block};
use dashd_locks::chainlock::ChainLockHandler;
use dashd_locks::is::db::{IsDb, SCHEMA};
use dashd_locks::signing_fake::FakeSignShares;
use dashd_masternode::sync::NodeSyncNotifier;
use dashd_mempool::testkit::FakeIsMempool;

use super::*;
use crate::chain::Chain;
use crate::chain::tests::{SharedFiles, branch, fresh, imported, regtest, startup_over};
use crate::locks::{NodeClChain, NodeClVerify};

const SPORK_KEY: &str = "-sporkkey=cP4EKFyJsHT39LDqgdcB43Y3YXjNyjb5Fuas1GQSeAtjnZWmZEQK";

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

/// A regtest V over block files the test keeps, with `n` blocks connected on genesis.
struct Rig {
    params: Arc<ChainSetup>,
    clock: Arc<dyn Clock>,
    abort: Arc<RecordingAbort>,
    files: SharedFiles,
    chain: Chain,
    blocks: Vec<Arc<Block>>,
}

impl Rig {
    fn new(n: u32) -> Rig {
        let params = regtest();
        let genesis = genesis_block(&params);
        let clock: Arc<dyn Clock> =
            Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + 3600));
        let abort = Arc::new(RecordingAbort::default());
        let files = SharedFiles::default();
        let mut chain =
            Chain::start(fresh(&params, &clock, &abort, Box::new(files.clone()))).expect("V");
        startup_over(&mut chain, Box::new(NullConnector), Box::new(NoFlush));
        imported(&mut chain);
        let blocks = branch(&params, &genesis, 0, n, 0);
        for block in &blocks {
            let r = chain.handle().process_block(Arc::clone(block), true, None, true).recv();
            assert!(r.expect("V answers").accepted);
        }
        Rig {
            params,
            clock,
            abort,
            files,
            chain,
            blocks,
        }
    }

    fn hash(&self, height: usize) -> BlockHash {
        self.blocks[height - 1].hash()
    }

    /// `UpdateBestChainlock` for the block at `height`, the signature taken as valid.
    fn lock(&self, height: i32) {
        let sig = Arc::new(ChainLockSig {
            height,
            block_hash: self.hash(usize::try_from(height).unwrap()),
            sig: ChainLockSig::null().sig,
        });
        let hash = Hash256::from_lendian([9; 32]);
        self.chain.handle().record_chainlock(sig, hash, None).recv().expect("V answers");
        let floor = self.chain.handle().view().snap.cl_floor.map(|f| i64::from(f.height));
        assert_eq!(floor, Some(i64::from(height)), "V's best lock");
    }

    /// A spork manager with `values` signed.
    fn sporks(&self, values: &[(SporkId, i64)]) -> Arc<SporkManager> {
        let args = crate::init::test_args(&[SPORK_KEY]);
        let sporks = crate::sporks::spork_manager(&args, &self.params, Arc::clone(&self.clock))
            .expect("sporks");
        for &(id, value) in values {
            sporks.update_spork(id, value).expect("signed");
        }
        sporks
    }

    fn sync(&self) -> Arc<MasternodeSync> {
        Arc::new(MasternodeSync::new(Arc::clone(&self.clock), Box::new(Quiet)))
    }

    fn reads(&self, sporks: Arc<SporkManager>) -> NodeLockReads {
        NodeLockReads {
            view: self.chain.handle().view_handle(),
            loading: Arc::clone(self.chain.handle().loading()),
            sporks,
            sync: self.sync(),
            files: Some(Arc::new(self.files.clone())),
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.chain.stop();
    }
}

fn tx(tag: u8) -> TxRef {
    let input = TxIn::new(OutPoint::new(TxHash::from([tag; 32]), 0), Default::default(), 0);
    Arc::new(Tx::new(Transaction::new(1, TxType::new(0), vec![input], vec![], 0, vec![])))
}

/// While SPORK_19 is off, no block is ChainLocked or conflicting, for either signer, though V
/// holds a best lock; with it at 0, the lock covers its block and its ancestors, conflicts with
/// another block at its height, and signing is enabled. The best height is never gated.
// Core: v24.0.2 src/chainlock/chainlock.cpp:33-35, :49-96
#[test]
fn chainlock_reads_answer_nothing_while_chainlocks_are_disabled() {
    let rig = Rig::new(3);
    rig.lock(2);
    let (b1, b2, b3, other) = (rig.hash(1), rig.hash(2), rig.hash(3), BlockHash::from([7; 32]));

    let off = rig.reads(rig.sporks(&[]));
    assert!(!off.enabled() && !off.signing_enabled());
    assert_eq!(off.best_height(), 2);
    assert!(!ClReads::has_chainlock(&off, 2, &b2));
    assert!(!off.has_conflicting_chainlock(2, &other));
    assert!(!IsReads::has_chain_lock(&off, 1, &b1));

    let on = rig.reads(rig.sporks(&[(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 0)]));
    assert!(on.enabled() && on.signing_enabled());
    assert!(ClReads::has_chainlock(&on, 2, &b2) && ClReads::has_chainlock(&on, 1, &b1));
    assert!(!ClReads::has_chainlock(&on, 3, &b3), "above the lock");
    assert!(on.has_conflicting_chainlock(2, &other) && !on.has_conflicting_chainlock(2, &b2));
    assert!(IsReads::has_chain_lock(&on, 1, &b1));

    let active = rig.reads(rig.sporks(&[(SporkId::SPORK_19_CHAINLOCKS_ENABLED, 1)]));
    assert!(active.enabled() && !active.signing_enabled(), "IsSigningEnabled is value 0");
}

/// The tree reads are V's published chain: the tip with its parent, lookups, ancestors, block
/// heights and the block files; an unknown block reads as none. Mempool signing needs SPORK_2's
/// value at 0.
// Core: v24.0.2 src/instantsend/signing.cpp:120-123; src/node/blockstorage.cpp:837-850
#[test]
fn the_tree_reads_follow_the_published_chain() {
    let rig = Rig::new(3);
    let reads = rig.reads(rig.sporks(&[]));
    let tip = ChainReads::tip(&reads).expect("a tip");
    assert_eq!((tip.hash, tip.height, tip.prev), (rig.hash(3), 3, Some(rig.hash(2))));
    assert_eq!(ChainReads::lookup(&reads, &rig.hash(1)).map(|e| e.height), Some(1));
    assert_eq!(ChainReads::ancestor_hash(&reads, &rig.hash(3), 1), Some(rig.hash(1)));
    assert_eq!(IsReads::block_height(&reads, &rig.hash(2)), Some(2));
    assert_eq!(reads.read_block(&rig.hash(2)).map(|b| b.hash()), Some(rig.hash(2)));
    let unknown = BlockHash::from([7; 32]);
    assert!(ChainReads::lookup(&reads, &unknown).is_none() && reads.read_block(&unknown).is_none());
    assert!(!reads.mempool_signing_enabled(), "SPORK_2 is off by default");

    let spork2 = rig.reads(rig.sporks(&[(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0)]));
    assert!(spork2.mempool_signing_enabled());
    let spork2 = rig.reads(rig.sporks(&[(SporkId::SPORK_2_INSTANTSEND_ENABLED, 1)]));
    assert!(!spork2.mempool_signing_enabled());
}

/// A mempool transaction is `Some(None)` with its body, an unknown one `None`; without a txindex
/// nothing is mined, and the first-seen cleanup is never held back.
// Core: v24.0.2 src/node/transaction.cpp:126-156 GetTransaction
#[test]
fn the_tx_source_reads_the_mempool_and_misses_the_unknown() {
    let pool = Arc::new(FakeIsMempool::default());
    let pooled = tx(1);
    pool.insert(Arc::clone(&pooled));
    let txs = NodeTxSource {
        mempool: Some(pool as _),
        txindex: None,
    };
    assert_eq!(TxSource::get_transaction(&txs, &pooled.txid()).unwrap(), Some(None));
    let (body, block) = txs.get_tx(&pooled.txid()).unwrap().expect("pooled");
    assert_eq!((body.txid(), block), (pooled.txid(), None));
    let unknown = tx(2).txid();
    assert_eq!(TxSource::get_transaction(&txs, &unknown).unwrap(), None);
    assert!(txs.get_tx(&unknown).unwrap().is_none());
    assert!(txs.indexed(None));
    txs.wait_synced();
}

/// The signers over `rig`, subscribing to `signing` and `signals`.
fn signers(rig: &Rig, signing: &Arc<FakeSignShares>, signals: &ValidationSignals) -> LockSigners {
    let sporks = rig.sporks(&[]);
    let store = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
    let v = rig.chain.handle();
    let is = IsParts::new(store as _, Arc::clone(&sporks), Arc::clone(v.loading()), {
        Arc::clone(&rig.clock)
    });
    let abort: Arc<dyn AbortSink> = Arc::clone(&rig.abort) as _;
    let handler = ChainLockHandler::new(
        NodeClChain::new(v.clone(), Arc::clone(&abort)),
        NodeClVerify::new(Arc::clone(&rig.params), v.clone()),
        Arc::clone(&rig.clock),
        || false,
    );
    LockSigners::new(LockSignerParts {
        params: &rig.params,
        view: v.view_handle(),
        loading: Arc::clone(v.loading()),
        signals: signals.clone(),
        sporks,
        sync: rig.sync(),
        is: &is,
        signing: Arc::clone(signing) as _,
        chainlocks: SharedHandler::new(handler),
        mempool: None,
        txindex: None,
        files: None,
        clock: Arc::clone(&rig.clock),
        abort,
    })
    .expect("the threads start")
}

/// The recovered-signature listeners are the ChainLock then the IS signer; `subscribe` puts two
/// hooks on the signals and `unsubscribe` takes them off; `stop` joins `cls-schdlr` and the IS
/// queue, so a job posted after it is dropped unrun. A second `stop` (the drop's) does nothing.
// Core: v24.0.2 src/active/context.cpp:55-73; src/chainlock/signing.cpp:55-59
#[test]
fn lock_signers_subscribe_on_start_and_stop_joins_their_threads() {
    let rig = Rig::new(1);
    let signing = FakeSignShares::new();
    let signals = ValidationSignals::default();
    let signers = signers(&rig, &signing, &signals);
    assert!(!signals.has_listeners(), "built idle");
    let held = (Arc::strong_count(&signers.cl), Arc::strong_count(&signers.is));
    let listeners = signers.recovered_sig_listeners();
    assert_eq!((Arc::strong_count(&signers.cl), Arc::strong_count(&signers.is)), held, "weak");
    drop(listeners);

    signers.start();
    assert!(!signals.has_listeners(), "start leaves the hooks to subscribe");
    signers.subscribe();
    signers.subscribe();
    assert!(signals.has_listeners());
    let (ran, heard) = mpsc::channel();
    let queue = signers.listener_signer().queue;
    queue.post(Box::new(move || ran.send(()).unwrap()));
    heard.recv_timeout(Duration::from_secs(60)).expect("the queue runs a posted job");
    let end = Instant::now() + Duration::from_secs(60);
    while !signers.scheduler.are_threads_servicing_queue() {
        assert!(Instant::now() < end, "cls-schdlr never serviced its queue");
        std::thread::sleep(Duration::from_millis(5));
    }

    signers.unsubscribe();
    assert!(!signals.has_listeners());
    signers.subscribe();
    signers.stop();
    assert!(!signals.has_listeners(), "stop unsubscribes what is left");
    assert!(!signers.scheduler.are_threads_servicing_queue(), "cls-schdlr joined");
    assert!(signers.queue.joined(), "is-signer joined");
    let late = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&late);
    queue.post(Box::new(move || flag.store(true, Ordering::SeqCst)));
    assert_eq!(Arc::strong_count(&late), 1, "dropped at the post");
    assert!(!late.load(Ordering::SeqCst));
    drop(signers);
    assert_eq!((signing.listener_count(), signals.has_listeners()), (0, false));
}

/// The store the node hands the signers' lock view answers through the txid's lock hash, so a
/// lock removed as confirmed is still locked while its txid cache and archive marker stand.
// Core: v24.0.2 src/instantsend/db.cpp:148-154, :286-297; src/instantsend/instantsend.cpp:400-407
#[test]
fn the_node_store_keeps_a_confirmed_lock_locked_for_the_signers() {
    let rig = Rig::new(1);
    let store = crate::instantsend::engine_opener(Arc::new(MemoryEngine::new(&SCHEMA)))(false)
        .expect("the store");
    let sporks = rig.sporks(&[(SporkId::SPORK_2_INSTANTSEND_ENABLED, 0)]);
    let is = IsParts::new(store, sporks, Arc::default(), Arc::clone(&rig.clock));
    let input = OutPoint::new(TxHash::from([2; 32]), 0);
    let txid = TxHash::from([1; 32]);
    let lock = LockedTx::new(1, vec![input], txid, Default::default(), [0; 96].into());
    let hash = dash_evo::locks::isdlock_hash(&lock);
    is.store.write_new(&hash, &lock).unwrap();
    is.store.write_mined(&hash, 5).unwrap();
    assert_eq!(is.store.remove_confirmed(5).unwrap().len(), 1);
    let view = StoreLockView {
        store: Arc::clone(&is.store),
        gate: Arc::clone(&is.gate),
    };
    assert!(view.is_locked(&txid).unwrap());
}
