//! [`IsListener`] over the IS store (PLAN-LK1b §6 S1d): the tip cache, the mined marks across a
//! reorg, ChainLock and depth confirmation with the archive's pruning, the gates, the attempt
//! gate's wait, and the one-report fault path.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use dash_consensus::primitives::{
    Block, BlockHeader, Header, OutPoint, Transaction, TxHash, TxIn, TxType,
};
use dash_consensus::{BlockId, Tx};
use dash_evo::locks::ChainLockSig;
use dash_evo::locks::{isdlock_hash, islock_request_id};
use dash_kv::{ErrorKind, KvRead, MemoryEngine, StoreError};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{Cont, ValidationInterface};

use super::{
    BlockTree, IsFaults, IsListener, IsListenerParts, IsListenerSigner, IsSignerQueue,
    input_lock_request_id,
};
use crate::is::db::core_model::Model;
use crate::is::db::{IS, IsDb, SCHEMA};
use crate::is::manager::{IsManager, MinedBlock};
use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
use crate::is::seams::{
    ChainLockReads, IsManagerStore, IsOverlay, IsSignerSeam, IsStore, RecoveredSigs, SignRequestId,
    SyncView,
};
use crate::is::{IsGate, IsLockHash, IsTipCache, LockedTx, lock_inputs};

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

/// Blocks by id: each one's height and parent. A line's block at height `h` is `BlockId(h)`; a
/// fork's ids start at 1000.
#[derive(Default)]
struct Tree(Mutex<BTreeMap<u32, (u32, Option<u32>)>>);

impl Tree {
    fn line(tip: u32) -> Arc<Tree> {
        let tree = Tree::default();
        for h in 0..=tip {
            tree.add(h, h, h.checked_sub(1));
        }
        Arc::new(tree)
    }

    fn add(&self, id: u32, height: u32, parent: Option<u32>) {
        self.0.lock().unwrap().insert(id, (height, parent));
    }
}

impl BlockTree for Tree {
    fn height(&self, id: BlockId) -> Option<u32> {
        self.0.lock().unwrap().get(&id.0).map(|(h, _)| *h)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        let tree = self.0.lock().unwrap();
        let mut at = id.0;
        loop {
            let (h, parent) = *tree.get(&at)?;
            if h == height {
                return Some(BlockId(at));
            }
            if h < height {
                return None;
            }
            at = parent?;
        }
    }
}

/// Every report, in order.
#[derive(Default)]
struct Faults(Mutex<Vec<(&'static str, Option<ErrorKind>)>>);

impl IsFaults for Faults {
    fn failed(&self, site: &'static str, error: &StoreError) {
        self.0.lock().unwrap().push((site, error.kind()));
    }
}

/// Every truncation tried, in order; while the switch is set, the next one fails.
#[derive(Default)]
struct RecSigs(Mutex<Vec<(LlmqType, SignRequestId)>>, AtomicBool);

impl RecoveredSigs for RecSigs {
    fn truncate(&self, llmq_type: LlmqType, id: &SignRequestId) -> dash_kv::Result<()> {
        self.0.lock().unwrap().push((llmq_type, *id));
        if self.1.swap(false, Ordering::SeqCst) {
            return Err(StoreError::engine("test", ErrorKind::Io, "injected"));
        }
        Ok(())
    }
}

/// `IsBlockchainSynced`, off unless a case turns it on.
#[derive(Default)]
struct Synced(AtomicBool);

impl SyncView for Synced {
    fn is_blockchain_synced(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// `HasChainLock`: every block while the switch is set.
#[derive(Default)]
struct ChainLocked(AtomicBool);

impl ChainLockReads for ChainLocked {
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The signer's queue: each call, in order.
#[derive(Default)]
struct Signer(Mutex<Vec<String>>);

impl IsSignerSeam for Signer {
    fn process_txs(&self, txs: Vec<Arc<Tx>>, retroactive: bool) {
        let txids: Vec<_> = txs.iter().map(|tx| tx.txid()).collect();
        self.0.lock().unwrap().push(format!("process {txids:?} {retroactive}"));
    }

    fn retry(&self, _: Vec<Arc<Tx>>) {}

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        self.0.lock().unwrap().push(format!("clear_inputs {}", ids.len()));
    }

    fn clear_lock(&self, _: Arc<LockedTx>) {}
}

/// A listener over `store`, with the switches the tests turn.
struct Rig {
    listener: IsListener,
    tip: IsTipCache,
    spork2: Arc<AtomicBool>,
    chainlocks: Arc<AtomicBool>,
    faults: Arc<Faults>,
    recsigs: Arc<RecSigs>,
    manager: Arc<IsManager>,
    synced: Arc<Synced>,
    chainlocked: Arc<ChainLocked>,
    signer: Arc<Signer>,
}

fn rig(store: Arc<dyn IsManagerStore>, tree: Arc<Tree>) -> Rig {
    rig_on(&regtest(), store, tree)
}

fn rig_on(params: &ChainSetup, store: Arc<dyn IsManagerStore>, tree: Arc<Tree>) -> Rig {
    let tip = IsTipCache::new();
    let spork2 = Arc::new(AtomicBool::new(true));
    let chainlocks = Arc::new(AtomicBool::new(true));
    let faults = Arc::new(Faults::default());
    let recsigs = Arc::new(RecSigs::default());
    let gate = {
        let spork2 = Arc::clone(&spork2);
        Arc::new(move || IsGate::new(spork2.load(Ordering::SeqCst), false, false))
    };
    let chainlocks_enabled = {
        let chainlocks = Arc::clone(&chainlocks);
        Arc::new(move || chainlocks.load(Ordering::SeqCst))
    };
    let manager = Arc::new(IsManager::new(
        Arc::clone(&store),
        Arc::clone(&gate) as _,
        Arc::new(dash_clock::MockClock::new()),
    ));
    let (synced, chainlocked, signer) = (
        Arc::new(Synced::default()),
        Arc::new(ChainLocked::default()),
        Arc::new(Signer::default()),
    );
    let listener = IsListener::new(
        params,
        IsListenerParts {
            store,
            tip: tip.clone(),
            gate,
            tree: tree as _,
            chainlocks_enabled,
            recsigs: Arc::clone(&recsigs) as _,
            faults: Arc::clone(&faults) as _,
            manager: Arc::clone(&manager),
            sync: Arc::clone(&synced) as _,
            chainlocks: Arc::clone(&chainlocked) as _,
        },
    )
    .with_seam(Arc::clone(&signer) as _);
    Rig {
        listener,
        tip,
        spork2,
        chainlocks,
        faults,
        recsigs,
        manager,
        synced,
        chainlocked,
        signer,
    }
}

fn memory() -> (Arc<MemoryEngine>, Arc<IsDb<MemoryEngine>>) {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let db = Arc::new(IsDb::open(Arc::clone(&engine)).unwrap());
    (engine, db)
}

fn spending(inputs: &[OutPoint], tag: u32) -> Tx {
    let inputs = inputs.iter().map(|i| TxIn::new(*i, Default::default(), u32::MAX)).collect();
    Tx::new(Transaction::new(1, TxType::new(0), inputs, vec![], tag, vec![]))
}

fn block(txs: &[&Tx]) -> Arc<Block> {
    let txs = txs.iter().map(|tx| Arc::new(Tx::new(tx.raw().clone()))).collect();
    Arc::new(Block::new(Header::new(BlockHeader::default()), txs))
}

/// A lock on `tx`'s inputs, written.
fn locked(db: &IsDb<MemoryEngine>, tx: &Tx) -> (IsLockHash, LockedTx) {
    let object = LockedTx::new(1, lock_inputs(tx), tx.txid(), Default::default(), [0; 96].into());
    let hash = isdlock_hash(&object);
    db.write_new(&hash, &object).unwrap();
    (hash, object)
}

fn txid(tag: u32) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    TxHash::from(bytes)
}

fn input(tag: u32, index: u32) -> OutPoint {
    OutPoint::new(txid(tag), index)
}

/// The heights of `hash`'s mined marks: Core's `is_m` rows, keyed by the inverted big-endian
/// height (`instantsend/db.cpp:66-72`).
fn mined_at(engine: &MemoryEngine, hash: &IsLockHash) -> Vec<i32> {
    let key =
        |h: i32| [&b"\x04is_m"[..], &u32::MAX.wrapping_sub(h as u32).to_be_bytes(), hash].concat();
    (0..400).filter(|h| engine.get(IS, &key(*h)).unwrap().is_some()).collect()
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        thread::yield_now();
    }
}

fn cl() -> Arc<ChainLockSig> {
    Arc::new(ChainLockSig::null())
}

#[test]
fn the_tip_cache_follows_cores_cache_tip_height() {
    let (_, db) = memory();
    let r = rig(db, Tree::line(20));
    r.listener.initialize_current_block_tip(None, true);
    assert_eq!(r.tip.get(), 0, "a null tip reads as 0");
    r.listener.initialize_current_block_tip(Some(BlockId(7)), true);
    assert_eq!(r.tip.get(), 7);
    r.listener.synchronous_updated_block_tip(BlockId(9), None, false);
    assert_eq!(r.tip.get(), 9);
    assert!(matches!(r.listener.block_connected(&block(&[]), BlockId(10)), Cont::Done));
    assert_eq!(r.tip.get(), 10);
    r.listener.block_disconnected(&block(&[]), BlockId(10));
    assert_eq!(r.tip.get(), 9, "the disconnected block's parent");
    r.spork2.store(false, Ordering::SeqCst);
    assert!(matches!(r.listener.block_connected(&block(&[]), BlockId(10)), Cont::Done));
    assert_eq!(r.tip.get(), 9, "BlockConnected caches nothing while InstantSend is off");
    r.listener.block_disconnected(&block(&[]), BlockId(9));
    assert_eq!(r.tip.get(), 8, "BlockDisconnected is not gated");
}

#[test]
fn a_reorg_across_a_mined_lock_moves_its_mark() {
    let (engine, db) = memory();
    let tree = Tree::line(10);
    // A fork from 8: ids 1009, 1010, 1011 at heights 9, 10, 11.
    tree.add(1009, 9, Some(8));
    tree.add(1010, 10, Some(1009));
    tree.add(1011, 11, Some(1010));
    let r = rig(db.clone(), tree);
    let tx = spending(&[input(5, 0)], 1);
    let (hash, _) = locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(10)), Cont::Done));
    assert_eq!(mined_at(&engine, &hash), [10]);
    r.listener.block_disconnected(&block(&[]), BlockId(10));
    r.listener.block_disconnected(&block(&[&tx]), BlockId(9));
    assert_eq!(mined_at(&engine, &hash), [10], "a block without it leaves the mark");
    r.listener.block_disconnected(&block(&[&tx]), BlockId(10));
    assert!(mined_at(&engine, &hash).is_empty(), "its block's disconnect clears it");
    for id in [1009, 1010] {
        assert!(matches!(r.listener.block_connected(&block(&[]), BlockId(id)), Cont::Done));
    }
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(1011)), Cont::Done));
    assert_eq!(mined_at(&engine, &hash), [11]);
    assert_eq!(r.tip.get(), 11);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

#[test]
fn a_chainlock_removes_confirmed_locks_truncates_their_sigs_and_prunes_the_archive() {
    let (engine, db) = memory();
    let r = rig(db.clone(), Tree::line(300));
    let tx_a = spending(&[input(5, 0), input(5, 1)], 1);
    let tx_b = spending(&[input(6, 0)], 2);
    let tx_c = spending(&[input(7, 0)], 3);
    let (ha, a) = locked(&db, &tx_a);
    let (hb, _) = locked(&db, &tx_b);
    let (hc, _) = locked(&db, &tx_c);
    assert!(matches!(r.listener.block_connected(&block(&[&tx_a]), BlockId(10)), Cont::Done));
    assert!(matches!(r.listener.block_connected(&block(&[&tx_b]), BlockId(150)), Cont::Done));
    assert!(matches!(r.listener.block_connected(&block(&[&tx_c]), BlockId(200)), Cont::Done));

    r.listener.notify_chain_lock(BlockId(100), &cl());
    assert!(db.by_hash(&ha, false).unwrap().is_none(), "A mined at 10 is confirmed");
    assert!(db.known(&ha).unwrap(), "and archived");
    assert!(db.by_hash(&hb, false).unwrap().is_some() && db.by_hash(&hc, false).unwrap().is_some());
    let is = regtest().consensus.llmq_type_dip0024_instant_send;
    let mut inputs: Vec<_> = a.inputs().iter().map(input_lock_request_id).collect();
    inputs.sort();
    let want: Vec<_> =
        std::iter::once(islock_request_id(a.inputs())).chain(inputs).map(|id| (is, id)).collect();
    assert_eq!(*r.recsigs.0.lock().unwrap(), want, "the lock's id, then each input's once");
    assert_eq!(*r.signer.0.lock().unwrap(), ["clear_inputs 2"]);

    // A confirmed lock is archived at its mined height, 10 (`instantsend/db.cpp:141-154`), and
    // pruned once a confirmation reaches 100 above it.
    r.listener.notify_chain_lock(BlockId(109), &cl());
    assert!(db.known(&ha).unwrap(), "109 - 100 is below A's archive height");
    r.listener.notify_chain_lock(BlockId(110), &cl());
    assert!(!db.known(&ha).unwrap(), "A's archive is pruned at 110 - 100");
    assert!(db.by_hash(&hb, false).unwrap().is_some() && db.by_hash(&hc, false).unwrap().is_some());
    r.listener.notify_chain_lock(BlockId(201), &cl());
    assert!(db.by_hash(&hb, false).unwrap().is_none() && db.by_hash(&hc, false).unwrap().is_none());
    assert!(db.known(&hb).unwrap() && db.known(&hc).unwrap(), "archived at 150 and 200");
    assert!(r.faults.0.lock().unwrap().is_empty());
    assert!(mined_at(&engine, &hb).is_empty(), "B's mark goes with it");
}

/// Core confirms before it prunes: `RemoveConfirmedInstantSendLocks(h)`, then
/// `RemoveArchivedInstantSendLocks(h - 100)`. A lock mined at 10 and first confirmed at 110 is
/// archived at 10 and pruned by that same confirmation, so it is no longer known; pruning first
/// would find no archive at 10 and leave it known until a later confirmation.
// Core: instantsend/instantsend.cpp:518-523
#[test]
fn a_first_confirmation_100_above_the_mined_height_archives_and_prunes_the_lock() {
    let (engine, db) = memory();
    let r = rig(db.clone(), Tree::line(120));
    let tx = spending(&[input(5, 0)], 1);
    let (hash, _) = locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(10)), Cont::Done));
    assert_eq!(mined_at(&engine, &hash), [10]);

    r.listener.notify_chain_lock(BlockId(110), &cl());
    assert!(db.by_hash(&hash, false).unwrap().is_none(), "confirmed");
    assert!(!db.known(&hash).unwrap(), "archived at 10 and pruned at 110 - 100");
    assert!(mined_at(&engine, &hash).is_empty());
    assert!(r.faults.0.lock().unwrap().is_empty());
}

#[test]
fn a_reorg_below_the_best_confirmed_height_removes_nothing_again() {
    let (_, db) = memory();
    let tree = Tree::line(20);
    tree.add(1009, 9, Some(8));
    let r = rig(db.clone(), tree);
    let tx = spending(&[input(5, 0)], 1);
    r.listener.notify_chain_lock(BlockId(10), &cl());
    for id in (9..=20).rev() {
        r.listener.block_disconnected(&block(&[]), BlockId(id));
    }
    let (hash, _) = locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(1009)), Cont::Done));
    r.listener.notify_chain_lock(BlockId(1009), &cl());
    assert!(db.by_hash(&hash, false).unwrap().is_some(), "9 is at or below the best, 10");
    assert!(r.recsigs.0.lock().unwrap().is_empty());
}

#[test]
fn without_chainlocks_the_block_keep_lock_deep_confirms() {
    let (_, db) = memory();
    let r = rig(db.clone(), Tree::line(40));
    let keep = u32::try_from(regtest().consensus.instant_send_keep_lock).unwrap();
    let tx = spending(&[input(5, 0)], 1);
    let (hash, _) = locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(20)), Cont::Done));

    // ChainLocks on and DIP0008 active at the new tip's parent: the locks wait for a ChainLock.
    r.listener.updated_block_tip(BlockId(20 + keep), None, false);
    assert!(db.by_hash(&hash, false).unwrap().is_some());

    r.chainlocks.store(false, Ordering::SeqCst);
    r.listener.updated_block_tip(BlockId(19 + keep), None, false);
    assert!(db.by_hash(&hash, false).unwrap().is_some(), "19 confirms nothing mined at 20");
    r.listener.updated_block_tip(BlockId(20 + keep), None, false);
    assert!(db.by_hash(&hash, false).unwrap().is_none(), "keep-lock deep");
    assert!(db.known(&hash).unwrap());
    // Below the keep-lock depth there is no ancestor: nothing to do.
    r.listener.updated_block_tip(BlockId(keep - 1), None, false);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

#[test]
fn before_dip0008_the_depth_rule_applies_with_chainlocks_on() {
    // Regtest's DIP0008 is at 1, under the keep depth: move it to 30.
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::TestActivationHeight, "dip0008@30")
        .build()
        .expect("regtest with DIP0008 at 30");
    let dip0008 = params.consensus.dip0008_height.to_u32();
    assert_eq!(dip0008, 30);
    let keep = params.consensus.instant_send_keep_lock;
    let store = Arc::new(InMemoryIsStore::default());
    let r = rig_on(&params, Arc::clone(&store) as _, Tree::line(dip0008 + 1));
    // Core: DIP0008 is active when the new tip's parent is at or above DIP0008Height.
    r.listener.updated_block_tip(BlockId(dip0008), None, false);
    let h = i32::try_from(dip0008).unwrap() - keep;
    let calls = [IsStoreCall::RemoveConfirmed(h), IsStoreCall::RemoveArchived(h - 100)];
    assert_eq!(store.calls(), calls, "the parent is below DIP0008Height");
    r.listener.updated_block_tip(BlockId(dip0008 + 1), None, false);
    assert_eq!(store.calls(), calls, "active from the next block on");
}

#[test]
fn with_instantsend_off_neither_confirmation_nor_marks_touch_the_store() {
    let store = Arc::new(InMemoryIsStore::default());
    let r = rig(Arc::clone(&store) as _, Tree::line(40));
    r.spork2.store(false, Ordering::SeqCst);
    r.chainlocks.store(false, Ordering::SeqCst);
    let tx = spending(&[input(5, 0)], 1);
    r.listener.notify_chain_lock(BlockId(30), &cl());
    r.listener.updated_block_tip(BlockId(30), None, false);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(31)), Cont::Done));
    assert!(store.calls().is_empty(), "{:?}", store.calls());
    // BlockDisconnected is Core's one ungated callback.
    r.listener.block_disconnected(&block(&[&tx]), BlockId(31));
    assert_eq!(store.calls(), [IsStoreCall::BlockDisconnected(31, vec![tx.txid()])]);
}

#[test]
fn only_lock_input_transactions_reach_the_store() {
    let store = Arc::new(InMemoryIsStore::default());
    let r = rig(Arc::clone(&store) as _, Tree::line(10));
    let spend = spending(&[input(5, 0)], 1);
    let coinbase = spending(&[OutPoint::new(TxHash::from([0; 32]), u32::MAX)], 2);
    assert!(matches!(
        r.listener.block_connected(&block(&[&coinbase, &spend]), BlockId(5)),
        Cont::Done
    ));
    assert_eq!(store.calls(), [IsStoreCall::BlockConnected(5, vec![spend.txid()])]);
}

#[test]
fn each_failed_store_call_reports_once_and_ends_its_notification() {
    let store = Arc::new(InMemoryIsStore::default());
    let fault = || StoreError::engine("test", ErrorKind::Io, "injected");
    {
        let mut s = store.scripted.lock().unwrap();
        s.block_connected.push_back(Err(fault()));
        s.block_disconnected.push_back(Err(fault()));
        s.remove_confirmed.push_back(Err(fault()));
        s.remove_confirmed.push_back(Ok(BTreeMap::new()));
        s.remove_archived.push_back(Err(fault()));
    }
    let r = rig(Arc::clone(&store) as _, Tree::line(200));
    let tx = spending(&[input(5, 0)], 1);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(5)), Cont::Done));
    r.listener.block_disconnected(&block(&[&tx]), BlockId(5));
    r.listener.notify_chain_lock(BlockId(150), &cl());
    r.listener.notify_chain_lock(BlockId(151), &cl());
    assert_eq!(
        store.calls(),
        [
            IsStoreCall::BlockConnected(5, vec![tx.txid()]),
            IsStoreCall::BlockDisconnected(5, vec![tx.txid()]),
            IsStoreCall::RemoveConfirmed(150),
            IsStoreCall::RemoveConfirmed(151),
            IsStoreCall::RemoveArchived(51),
        ],
        "a failed removal skips the archive's pruning"
    );
    assert_eq!(
        *r.faults.0.lock().unwrap(),
        [
            (super::CONNECTED, Some(ErrorKind::Io)),
            (super::DISCONNECTED, Some(ErrorKind::Io)),
            (super::CONFIRMED, Some(ErrorKind::Io)),
            (super::CONFIRMED, Some(ErrorKind::Io)),
        ]
    );
}

#[test]
fn a_failed_truncation_reports_once_and_ends_the_confirmation() {
    let (_, db) = memory();
    let r = rig(db.clone(), Tree::line(20));
    let tx_a = spending(&[input(8, 0), input(8, 1)], 1);
    let tx_b = spending(&[input(9, 0)], 2);
    let (ha, _) = locked(&db, &tx_a);
    let (hb, _) = locked(&db, &tx_b);
    assert!(matches!(r.listener.block_connected(&block(&[&tx_a, &tx_b]), BlockId(10)), Cont::Done));
    r.recsigs.1.store(true, Ordering::SeqCst);
    r.listener.notify_chain_lock(BlockId(10), &cl());
    assert_eq!(r.recsigs.0.lock().unwrap().len(), 1, "nothing after the failed truncation");
    assert_eq!(*r.faults.0.lock().unwrap(), [(super::CONFIRMED, Some(ErrorKind::Io))]);
    assert!(db.by_hash(&ha, false).unwrap().is_none() && db.by_hash(&hb, false).unwrap().is_none());
}

#[test]
fn a_mark_queued_behind_an_attempts_stage_lands_after_its_commit() {
    let (engine, db) = memory();
    let r = Arc::new(rig(db.clone(), Tree::line(10)));
    let (root_hash, root) = locked(&db, &spending(&[input(74, 0)], 1));
    let tx = spending(&[input(75, 0)], 2);
    let (hash, _) = locked(&db, &tx);
    let mut overlay = IsOverlay::default();
    let (_, stage) = db.stage_remove_chained(&mut overlay, &root_hash, &root, 10).unwrap();
    let signals = {
        let r = Arc::clone(&r);
        let block = block(&[&tx]);
        thread::spawn(move || r.listener.block_connected(&block, BlockId(9)))
    };
    wait_until("the mark to park on the gate", || db.gate_waiters() == 1 || signals.is_finished());
    assert!(!signals.is_finished(), "the mark waits for the attempt");
    assert!(mined_at(&engine, &hash).is_empty());
    db.commit_staged(&mut overlay, stage).unwrap();
    assert!(matches!(signals.join().unwrap(), Cont::Done));
    assert_eq!(mined_at(&engine, &hash), [9]);
    assert!(db.by_hash(&root_hash, false).unwrap().is_none(), "the stage's removal landed");
    assert!(r.faults.0.lock().unwrap().is_empty());
}

#[test]
fn the_input_lock_request_id_is_cores_serialize_hash() {
    let mut txid = [0; 32];
    for (i, b) in txid.iter_mut().enumerate() {
        *b = i as u8;
    }
    let id = input_lock_request_id(&OutPoint::new(TxHash::from(txid), 7));
    // sha256d(0x06 "inlock" || txid || 7 as LE u32), computed independently.
    let want = "1d3b9b76f0dfd26e9019c668f889aa9665b5ba25395bc0fb2d46c613ace28156";
    let got: String = id.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, want);
}

/// One notification, as the listener and Core's store model receive it.
#[derive(Clone, Copy, Debug)]
enum Note {
    /// `BlockConnected` at a height, with the case's transactions mined in it.
    Connected(u32),
    Disconnected(u32),
    ChainLock(u32),
    /// `UpdatedBlockTip` with ChainLocks off: the block `nInstantSendKeepLock` deep confirms.
    Depth(u32),
}

/// The listener over an IsDb beside Core's model of that store, both given each note.
struct Replay {
    engine: Arc<MemoryEngine>,
    db: Arc<IsDb<MemoryEngine>>,
    r: Rig,
    model: Model,
    txs: Vec<Tx>,
}

impl Replay {
    fn new() -> Self {
        let (engine, db) = memory();
        let r = rig(db.clone(), Tree::line(400));
        let mut model = Model::default();
        let txs: Vec<_> = (0..3).map(|i| spending(&[input(40 + i, 0)], i + 1)).collect();
        for tx in &txs {
            let (hash, lock) = locked(&db, tx);
            model.write(hash, &lock);
        }
        Replay {
            engine,
            db,
            r,
            model,
            txs,
        }
    }

    fn rows(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.engine.iter_prefix(IS, &[]).map(Result::unwrap).collect()
    }

    /// Core's `HandleFullyConfirmedBlock` on the model: the confirmation, then the prune.
    fn model_confirms(&mut self, height: u32) {
        let height = i32::try_from(height).unwrap();
        self.model.confirm(height);
        self.model.prune(height - 100);
    }

    fn deliver(&mut self, note: Note) {
        let refs: Vec<&Tx> = self.txs.iter().collect();
        let txs: Vec<Tx> = self.txs.iter().map(|tx| Tx::new(tx.raw().clone())).collect();
        match note {
            Note::Connected(h) => {
                assert!(matches!(
                    self.r.listener.block_connected(&block(&refs), BlockId(h)),
                    Cont::Done
                ));
                self.model.block(&txs, i32::try_from(h).unwrap(), true);
            }
            Note::Disconnected(h) => {
                self.r.listener.block_disconnected(&block(&refs), BlockId(h));
                self.model.block(&txs, i32::try_from(h).unwrap(), false);
            }
            Note::ChainLock(h) => {
                self.r.listener.notify_chain_lock(BlockId(h), &cl());
                self.model_confirms(h);
            }
            Note::Depth(h) => {
                let keep = u32::try_from(regtest().consensus.instant_send_keep_lock).unwrap();
                self.r.chainlocks.store(false, Ordering::SeqCst);
                self.r.listener.updated_block_tip(BlockId(h + keep), None, false);
                self.r.chainlocks.store(true, Ordering::SeqCst);
                self.model_confirms(h);
            }
        }
    }

    /// A restart: the store reopened with empty caches and `best_confirmed_height = 0`, and a
    /// new listener over it.
    fn restart(&mut self) {
        self.db = Arc::new(IsDb::open(Arc::clone(&self.engine)).unwrap());
        self.r = rig(self.db.clone(), Tree::line(400));
        self.model.reopen();
    }

    fn check(&self, at: &str) {
        assert_eq!(self.rows(), self.model.rows, "{at}: rows");
        assert_eq!(self.db.cache_snapshot(), self.model.snapshot(), "{at}: caches");
        assert!(self.r.faults.0.lock().unwrap().is_empty(), "{at}: no report");
    }
}

/// PLAN-LK1b §3.1's re-delivery table through the listener: every operation the listener drives
/// (a block's mined marks on connect and disconnect, a ChainLock's or the depth rule's
/// confirmation with its archive prune and recovered-signature truncation, the prune alone, and a
/// confirmation again after a restart) is delivered twice over IsDb and given to Core's model
/// twice. After the second delivery the rows are the first's, rows and caches are the model's,
/// a repeated confirmation truncates no recovered signature, and nothing is reported.
/// (`remove_chained` and `write_new` are the connector's and the lock intake's, S1a-tested.)
#[test]
fn each_section_3_1_row_delivered_twice_through_the_listener_follows_cores_model() {
    use Note::*;
    // (row, the notes before it, the note delivered twice, whether a restart comes between).
    let rows: [(&str, &[Note], Note, bool); 7] = [
        ("block_connected", &[], Connected(10), false),
        ("block_disconnected", &[Connected(10)], Disconnected(10), false),
        ("remove_confirmed (ChainLock)", &[Connected(10)], ChainLock(20), false),
        // Mined at the depth rule's height: a confirmation one block short leaves the lock.
        ("remove_confirmed (depth)", &[Connected(20)], Depth(20), false),
        ("remove_archived", &[Connected(10), ChainLock(20)], ChainLock(110), false),
        ("remove_confirmed after a restart", &[Connected(10)], ChainLock(20), true),
        ("a confirmation that finds nothing", &[], ChainLock(20), false),
    ];
    for (row, before, twice, restart) in rows {
        let mut replay = Replay::new();
        for note in before {
            replay.deliver(*note);
        }
        replay.check(&format!("{row}: before"));
        replay.deliver(twice);
        replay.check(&format!("{row}: once"));
        let once = replay.rows();
        if restart {
            replay.restart();
        }
        let truncated = replay.r.recsigs.0.lock().unwrap().len();
        replay.deliver(twice);
        replay.check(&format!("{row}: twice"));
        assert_eq!(replay.rows(), once, "{row}: the second delivery leaves the rows");
        let again = replay.r.recsigs.0.lock().unwrap().len();
        assert_eq!(again, truncated, "{row}: the second delivery truncates nothing");
    }
}

/// The blocks a lock over `input` (for another txid) finds conflicting transactions mined in.
fn conflicts_on(r: &Rig, input: OutPoint) -> Vec<(BlockId, Vec<TxHash>)> {
    let probe = LockedTx::new(1, vec![input], txid(999), Default::default(), [0; 96].into());
    r.manager
        .retrieve_is_conflicts(&isdlock_hash(&probe), &probe)
        .into_iter()
        .map(|(mined, txs)| (mined.id, txs.iter().map(|tx| tx.txid()).collect()))
        .collect()
}

fn retry_txids(manager: &IsManager) -> Vec<TxHash> {
    manager.prepare_tx_to_retry().iter().map(|tx| tx.txid()).collect()
}

/// While the blockchain is synced, an unlocked transaction with lock inputs is tracked as mined
/// in the block and handed to the signer after the loop; a locked one is untracked with its
/// children retried; one without lock inputs is skipped. Not synced, nothing is tracked.
// Core: instantsend/net_instantsend.cpp:544-568
#[test]
fn block_connected_tracks_unlocked_transactions_while_synced() {
    let (_, db) = memory();
    let r = rig(db.clone(), Tree::line(20));
    let free = spending(&[input(5, 0)], 1);
    let held = spending(&[input(6, 0)], 2);
    let no_inputs = spending(&[], 3);
    locked(&db, &held);
    let child = Arc::new(spending(&[OutPoint::new(held.txid(), 0)], 4));
    r.manager.add_non_locked_tx(&Arc::new(Tx::new(held.raw().clone())), None);
    r.manager.add_non_locked_tx(&child, None);
    let b = block(&[&no_inputs, &free, &held]);
    assert!(matches!(r.listener.block_connected(&b, BlockId(10)), Cont::Done));
    assert!(conflicts_on(&r, input(5, 0)).is_empty(), "not synced");

    r.synced.0.store(true, Ordering::SeqCst);
    assert!(matches!(r.listener.block_connected(&b, BlockId(10)), Cont::Done));
    assert_eq!(conflicts_on(&r, input(5, 0)), [(BlockId(10), vec![free.txid()])]);
    assert!(conflicts_on(&r, input(6, 0)).is_empty());
    assert_eq!(retry_txids(&r.manager), [child.txid()]);
    assert_eq!(*r.signer.0.lock().unwrap(), [format!("process {:?} true", [free.txid()])]);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// A ChainLocked block's transactions are untracked, not tracked, and none goes to the signer.
// Core: instantsend/net_instantsend.cpp:545, :548-562
#[test]
fn a_chainlocked_block_untracks_its_transactions() {
    let (_, db) = memory();
    let r = rig(db, Tree::line(20));
    r.synced.0.store(true, Ordering::SeqCst);
    r.chainlocked.0.store(true, Ordering::SeqCst);
    let free = spending(&[input(5, 0)], 1);
    let mined = MinedBlock {
        id: BlockId(3),
        height: 3,
        hash: BlockHash::from([3; 32]),
    };
    r.manager.add_non_locked_tx(&Arc::new(Tx::new(free.raw().clone())), Some(mined));
    assert!(matches!(r.listener.block_connected(&block(&[&free]), BlockId(10)), Cont::Done));
    assert!(conflicts_on(&r, input(5, 0)).is_empty());
    assert!(r.signer.0.lock().unwrap().is_empty());
}

/// A confirmation untracks every transaction mined in the confirmed block's chain (its ancestor
/// at the mined height is the mined block), retrying their children; a fork's and an unmined
/// transaction stay.
// Core: instantsend/instantsend.cpp:525-541
#[test]
fn a_confirmation_untracks_transactions_mined_in_its_chain() {
    let (_, db) = memory();
    let tree = Tree::line(300);
    tree.add(1005, 5, Some(4));
    let r = rig(db, tree);
    let at = |id: u32, height: u32| MinedBlock {
        id: BlockId(id),
        height,
        hash: BlockHash::from([id as u8; 32]),
    };
    let on_line = Arc::new(spending(&[input(5, 0)], 1));
    let on_fork = Arc::new(spending(&[input(6, 0)], 2));
    let unmined = Arc::new(spending(&[input(7, 0)], 3));
    let child = Arc::new(spending(&[OutPoint::new(on_line.txid(), 0)], 4));
    r.manager.add_non_locked_tx(&on_line, Some(at(10, 10)));
    r.manager.add_non_locked_tx(&on_fork, Some(at(1005, 5)));
    r.manager.add_non_locked_tx(&unmined, None);
    r.manager.add_non_locked_tx(&child, None);
    r.listener.notify_chain_lock(BlockId(100), &cl());
    assert!(conflicts_on(&r, input(5, 0)).is_empty());
    assert_eq!(conflicts_on(&r, input(6, 0)), [(BlockId(1005), vec![on_fork.txid()])]);
    assert_eq!(retry_txids(&r.manager), [child.txid()]);
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 5, "fork, unmined, child, parents");
}

type Job = Box<dyn FnOnce() + Send>;

/// An unbounded FIFO whose `post` only appends; `run` drains it on the calling thread. Each post
/// also records the signer store's calls so far.
struct Queue {
    jobs: Mutex<Vec<Job>>,
    store: Arc<InMemoryIsStore>,
    calls_at_post: Mutex<Vec<Vec<IsStoreCall>>>,
}

impl IsSignerQueue for Queue {
    fn post(&self, job: Job) {
        self.calls_at_post.lock().unwrap().push(self.store.calls());
        self.jobs.lock().unwrap().push(job);
    }
}

impl Queue {
    fn posted(&self) -> usize {
        self.calls_at_post.lock().unwrap().len()
    }

    fn take(&self) -> Vec<Job> {
        std::mem::take(&mut *self.jobs.lock().unwrap())
    }

    fn run(&self) {
        self.take().into_iter().for_each(|job| job());
    }
}

mod signer_rig {
    pub(super) use crate::is::signer::tests::{Rig, TIP, child, txid, vote};
}

/// A listener over `store` with the signer of `s` and its queue, the blockchain synced.
fn with_signer(store: Arc<dyn IsManagerStore>, s: &signer_rig::Rig) -> (Rig, Arc<Queue>) {
    let queue = Arc::new(Queue {
        jobs: Mutex::default(),
        store: s.store.clone(),
        calls_at_post: Mutex::default(),
    });
    let mut r = rig(store, Tree::line(300));
    r.synced.0.store(true, Ordering::SeqCst);
    r.listener = r.listener.with_signer(IsListenerSigner {
        signer: Arc::clone(&s.signer),
        queue: Arc::clone(&queue) as _,
    });
    (r, queue)
}

/// `child(parent)` with `parent` mined deep for the signer.
fn lockable(s: &signer_rig::Rig, parent: u32) -> Tx {
    let parent = signer_rig::txid(parent);
    s.mine(parent, signer_rig::TIP - 10);
    signer_rig::child(&[parent])
}

/// A version-3 asset unlock: one lock input, no `vin`.
fn asset_unlock() -> Tx {
    let mut payload = vec![0; dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[1..9].copy_from_slice(&7u64.to_le_bytes());
    Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload))
}

// Core: v24.0.2 src/instantsend/net_instantsend.cpp:544-569 BlockConnected
#[test]
fn a_connected_block_signs_its_unlocked_txs_retroactively_before_the_store_write() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    let unlocked = lockable(&s, 1);
    let locked = lockable(&s, 2);
    s.view.lock(locked.txid());
    let unlock = asset_unlock();
    let coinbase = spending(&[], 9);
    // `IsLocked` of each tx with lock inputs, in block order: unlocked, locked, the unlock.
    s.store.scripted.lock().unwrap().known.extend([Ok(false), Ok(true), Ok(false)]);
    assert!(matches!(
        r.listener.block_connected(&block(&[&coinbase, &unlocked, &locked, &unlock]), BlockId(5)),
        Cont::Done
    ));
    let written = |calls: &[IsStoreCall]| {
        calls.iter().any(|c| matches!(c, IsStoreCall::BlockConnected(5, _)))
    };
    assert_eq!(queue.posted(), 1);
    assert!(!written(&queue.calls_at_post.lock().unwrap()[0]), "posted before the store write");
    assert!(written(&s.store.calls()));
    queue.run();
    assert_eq!(s.requests(), [signer_rig::vote(&s, &unlocked, true)]);
}

// Core: v24.0.2 src/instantsend/net_instantsend.cpp:545, :551 BlockConnected
#[test]
fn a_chainlocked_block_is_not_signed_retroactively() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    let tx = lockable(&s, 1);
    let b = block(&[&tx]);
    r.chainlocked.0.store(true, Ordering::SeqCst);
    assert!(matches!(r.listener.block_connected(&b, BlockId(5)), Cont::Done));
    assert_eq!(queue.posted(), 0);
    assert_eq!(
        s.store.calls(),
        [
            IsStoreCall::HashByTxid(tx.txid()),
            IsStoreCall::Known([0; 32]),
            IsStoreCall::BlockConnected(5, vec![tx.txid()])
        ]
    );
}

// Core: v24.0.2 src/instantsend/net_instantsend.cpp:544 BlockConnected
#[test]
fn an_unsynced_node_signs_nothing_retroactively() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    r.synced.0.store(false, Ordering::SeqCst);
    let tx = lockable(&s, 1);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(5)), Cont::Done));
    assert_eq!(queue.posted(), 0);
    assert_eq!(s.store.calls(), [IsStoreCall::BlockConnected(5, vec![tx.txid()])]);
}

/// A parked lock attaches to its arriving transaction, which is then not signed.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:452-462 TransactionAddedToMempool
#[test]
fn a_mempool_tx_without_an_attached_lock_is_signed_and_one_with_it_is_not() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    let unlocked = Arc::new(lockable(&s, 1));
    let attached = Arc::new(lockable(&s, 2));
    let lock = LockedTx::new(
        1,
        lock_inputs(&attached),
        attached.txid(),
        Default::default(),
        [0; 96].into(),
    );
    r.manager.add_pending_is_lock(isdlock_hash(&lock), Arc::new(lock), 3, false);
    r.listener.transaction_added_to_mempool(&unlocked, 0, 0);
    r.listener.transaction_added_to_mempool(&attached, 0, 0);
    assert_eq!(queue.posted(), 1);
    queue.run();
    assert_eq!(s.requests(), [signer_rig::vote(&s, &unlocked, false)]);
}

/// The hook reads the listener's sync state, the one BlockConnected reads, not the signer's
/// (S4 review r2 M2); the signer's own `ProcessTx` gate still applies when the job runs.
// Core: v24.0.0 src/instantsend/net_instantsend.cpp:449-451, :544; signing.cpp:362-364
#[test]
fn the_mempool_hook_needs_instantsend_a_synced_chain_and_lock_inputs() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    let tx = Arc::new(lockable(&s, 1));
    r.spork2.store(false, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    r.spork2.store(true, Ordering::SeqCst);
    r.synced.0.store(false, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    r.synced.0.store(true, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&Arc::new(spending(&[], 9)), 0, 0);
    assert_eq!(queue.posted(), 0);
    s.reads.synced.store(false, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert_eq!(queue.posted(), 1, "the signer's sync flag does not gate the hook");
    queue.run();
    assert!(s.requests().is_empty(), "ProcessTx checks its own sync state");
}

// Core calls ProcessTx on the signals thread (net_instantsend.cpp:453-459, :544-569); here the
// hooks post it, so no sign call runs on a thread with the no-wait mark.
#[test]
fn the_hooks_post_process_tx_off_the_marked_thread() {
    let s = signer_rig::Rig::regtest();
    let (r, queue) = with_signer(s.store.clone(), &s);
    let mined = lockable(&s, 1);
    let pooled = Arc::new(lockable(&s, 2));
    {
        let _mark = dashd_chainstate::chain::NoWaitGuard::set();
        assert!(matches!(r.listener.block_connected(&block(&[&mined]), BlockId(5)), Cont::Done));
        r.listener.transaction_added_to_mempool(&pooled, 0, 0);
    }
    assert!(s.requests().is_empty());
    let jobs = queue.take();
    thread::spawn(move || jobs.into_iter().for_each(|job| job())).join().unwrap();
    assert_eq!(
        s.requests(),
        [signer_rig::vote(&s, &mined, true), signer_rig::vote(&s, &pooled, false)]
    );
}

// Core: v24.0.2 src/instantsend/net_instantsend.cpp:645-654 TruncateRecoveredSigsForInputs
#[test]
fn a_fully_confirmed_lock_clears_its_input_ids() {
    let s = signer_rig::Rig::regtest();
    let (_engine, db) = memory();
    let (r, queue) = with_signer(db.clone(), &s);
    let tx = lockable(&s, 1);
    r.listener.transaction_added_to_mempool(&Arc::new(Tx::new(tx.raw().clone())), 0, 0);
    queue.run();
    assert_eq!(s.input_ids().len(), 1);
    locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(10)), Cont::Done));
    r.listener.notify_chain_lock(BlockId(100), &cl());
    queue.run();
    assert!(s.input_ids().is_empty());
}

/// A confirmed lock's input ids leave the signer's queue through one `ClearInputsFromQueue`,
/// posted behind the signer's earlier jobs, and each recovered signature is truncated once: the
/// lock's, then its inputs'.
// Core: v24.0.2 src/instantsend/net_instantsend.cpp:645-654, :669-673
#[test]
fn a_confirmed_lock_clears_the_signer_s_input_queue_exactly_once() {
    let s = signer_rig::Rig::regtest();
    let (_engine, db) = memory();
    let (r, queue) = with_signer(db.clone(), &s);
    let parents = [signer_rig::txid(1), signer_rig::txid(2)];
    for parent in parents {
        s.mine(parent, signer_rig::TIP - 10);
    }
    let tx = signer_rig::child(&parents);
    r.listener.transaction_added_to_mempool(&Arc::new(Tx::new(tx.raw().clone())), 0, 0);
    queue.run();
    assert_eq!(s.input_ids().len(), 2);
    let (_, lock) = locked(&db, &tx);
    assert!(matches!(r.listener.block_connected(&block(&[&tx]), BlockId(10)), Cont::Done));
    assert_eq!(queue.posted(), 1, "the lock is not signed again");

    r.listener.notify_chain_lock(BlockId(100), &cl());
    assert_eq!(queue.posted(), 2, "one job for the confirmed lock");
    assert_eq!(s.input_ids().len(), 2, "queued, not run inline");
    queue.run();
    assert!(s.input_ids().is_empty());
    let is = regtest().consensus.llmq_type_dip0024_instant_send;
    let mut inputs: Vec<_> = lock.inputs().iter().map(input_lock_request_id).collect();
    inputs.sort();
    let want: Vec<_> = std::iter::once(islock_request_id(lock.inputs()))
        .chain(inputs)
        .map(|id| (is, id))
        .collect();
    assert_eq!(*r.recsigs.0.lock().unwrap(), want);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// `BlockConnected` interleaves per transaction as Core does: each unlocked transaction's
/// `ProcessTx` is posted before it is tracked, and after the previous one is.
// Core: v24.0.2 src/instantsend/net_instantsend.cpp:548-562
#[test]
fn block_connected_posts_and_tracks_each_transaction_in_turn() {
    struct Counting {
        manager: Arc<IsManager>,
        tracked_at_post: Mutex<Vec<usize>>,
    }
    impl IsSignerQueue for Counting {
        fn post(&self, _: Job) {
            let tracked = self.manager.counts().unwrap().unprotected_tx;
            self.tracked_at_post.lock().unwrap().push(tracked);
        }
    }
    let s = signer_rig::Rig::regtest();
    let (_engine, db) = memory();
    let mut r = rig(db, Tree::line(300));
    r.synced.0.store(true, Ordering::SeqCst);
    let queue = Arc::new(Counting {
        manager: Arc::clone(&r.manager),
        tracked_at_post: Mutex::default(),
    });
    r.listener = r.listener.with_signer(IsListenerSigner {
        signer: Arc::clone(&s.signer),
        queue: Arc::clone(&queue) as _,
    });
    let (first, second) = (lockable(&s, 1), lockable(&s, 2));
    assert!(matches!(
        r.listener.block_connected(&block(&[&first, &second]), BlockId(10)),
        Cont::Done
    ));
    assert_eq!(*queue.tracked_at_post.lock().unwrap(), [0, 2], "the first and its parent");
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 4);
}
