//! The mempool half (PLAN-LK1b §6 S5) on MP1a-0's testkit double: the accept-time answers, the
//! listener's `TransactionAddedToMempool`/`TransactionRemovedFromMempool`, re-pin R18's outpoint
//! index scenario, and the lock order around every `IsMempool` mutator.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_clock::MockClock;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_consensus::{BlockId, Tx, TxRef};
use dash_evo::locks::isdlock_hash;
use dash_kv::{MemoryEngine, StoreError};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{RemovalReason, ValidationInterface};
use dashd_mempool::api::MempoolRead;
use dashd_mempool::is::{IsAcceptHooks, IsMempool};
use dashd_mempool::testkit::FakeIsMempool;

use super::*;
use crate::is::chain::GateSource;
use crate::is::db::{IsDb, SCHEMA};
use crate::is::listener::{BlockTree, IsListener, IsListenerParts};
use crate::is::manager::MinedBlock;
use crate::is::process::ResolvingLocks;
use crate::is::seams::{
    ChainLockReads, IsManagerStore, IsOverlay, IsSignerSeam, IsStore, RecoveredSigs, SignRequestId,
    SyncView,
};
use crate::is::{IsGate, IsTipCache, lock_inputs};

/// [`FakeIsMempool`] whose mutators first check that no LK1b lock is held (MP1a §3 contract 2).
pub(in crate::is) struct CheckedPool {
    pub inner: Arc<FakeIsMempool>,
    pub manager: Arc<IsManager>,
    pub db: Arc<IsDb<MemoryEngine>>,
    pub resolving: Option<Arc<ResolvingLocks>>,
    pub mutators: AtomicUsize,
}

impl CheckedPool {
    pub fn new(
        inner: Arc<FakeIsMempool>,
        manager: Arc<IsManager>,
        db: Arc<IsDb<MemoryEngine>>,
        resolving: Option<Arc<ResolvingLocks>>,
    ) -> Self {
        CheckedPool {
            inner,
            manager,
            db,
            resolving,
            mutators: AtomicUsize::new(0),
        }
    }

    fn check(&self) {
        assert!(!self.manager.any_lock_held(), "a manager mutex is held across the pool call");
        assert!(!self.db.any_lock_held(), "a store mutex is held across the pool call");
        if let Some(resolving) = &self.resolving {
            assert!(!resolving.0.is_locked(), "ResolvingLocks is held across the pool call");
        }
        self.mutators.fetch_add(1, Ordering::SeqCst);
    }
}

impl IsMempool for CheckedPool {
    fn get(&self, txid: &TxHash) -> Option<TxRef> {
        self.inner.get(txid)
    }

    fn exists(&self, txid: &TxHash) -> bool {
        self.inner.exists(txid)
    }

    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef> {
        self.inner.spender_of(outpoint)
    }

    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash> {
        self.inner.asset_unlock_txids_by_index(index)
    }

    fn pending_asset_unlock_amount(&self) -> i64 {
        self.inner.pending_asset_unlock_amount()
    }

    fn remove_conflicts_for_lock(&self, txid: &TxHash, inputs: &[OutPoint]) -> Vec<TxRef> {
        self.check();
        self.inner.remove_conflicts_for_lock(txid, inputs)
    }

    fn add_transactions_updated(&self, n: u32) {
        self.check();
        self.inner.add_transactions_updated(n);
    }

    fn with_read(&self, f: &mut dyn FnMut(&dyn MempoolRead)) {
        self.inner.with_read(f);
    }
}

#[derive(Default)]
struct Faults(Mutex<Vec<&'static str>>);

impl IsFaults for Faults {
    fn failed(&self, site: &'static str, _: &StoreError) {
        self.0.lock().unwrap().push(site);
    }
}

struct Line;

impl BlockTree for Line {
    fn height(&self, id: BlockId) -> Option<u32> {
        Some(id.0)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }
}

struct NoRecSigs;

impl RecoveredSigs for NoRecSigs {
    fn truncate(&self, _: LlmqType, _: &SignRequestId) -> dash_kv::Result<()> {
        Ok(())
    }
}

struct Synced(AtomicBool);

impl SyncView for Synced {
    fn is_blockchain_synced(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

struct NoChainLocks;

impl ChainLockReads for NoChainLocks {
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        false
    }
}

/// The signer seam's `ClearInputsFromQueue` calls, by id count.
#[derive(Default)]
struct Signer(Mutex<Vec<usize>>);

impl IsSignerSeam for Signer {
    fn process_txs(&self, _: Vec<Arc<Tx>>, _: bool) {}

    fn retry(&self, _: Vec<Arc<Tx>>) {}

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        self.0.lock().unwrap().push(ids.len());
    }

    fn clear_lock(&self, _: Arc<LockedTx>) {}
}

struct Rig {
    engine: Arc<MemoryEngine>,
    db: Arc<IsDb<MemoryEngine>>,
    spork2: Arc<AtomicBool>,
    manager: Arc<IsManager>,
    view: IsAcceptView,
    faults: Arc<Faults>,
    pool: Arc<FakeIsMempool>,
    checked: Arc<CheckedPool>,
    signer: Arc<Signer>,
    synced: Arc<Synced>,
    tip: IsTipCache,
    listener: IsListener,
}

/// The listener over `store` and `manager`, reporting to `faults`, with `signer` as its seam.
fn listener_on(
    store: Arc<dyn IsManagerStore>,
    manager: &Arc<IsManager>,
    gate: GateSource,
    tip: &IsTipCache,
    faults: &Arc<Faults>,
    synced: &Arc<Synced>,
    signer: &Arc<Signer>,
) -> IsListener {
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    IsListener::new(
        &params,
        IsListenerParts {
            store,
            tip: tip.clone(),
            gate,
            tree: Arc::new(Line),
            chainlocks_enabled: Arc::new(|| true),
            recsigs: Arc::new(NoRecSigs),
            faults: Arc::clone(faults) as _,
            manager: Arc::clone(manager),
            sync: Arc::clone(synced) as _,
            chainlocks: Arc::new(NoChainLocks),
        },
    )
    .with_seam(Arc::clone(signer) as _)
}

fn rig_on(engine: Arc<MemoryEngine>) -> Rig {
    let db = Arc::new(IsDb::open(Arc::clone(&engine)).unwrap());
    let spork2 = Arc::new(AtomicBool::new(true));
    let gate: GateSource = {
        let spork2 = Arc::clone(&spork2);
        Arc::new(move || IsGate::new(spork2.load(Ordering::SeqCst), false, false))
    };
    let manager =
        Arc::new(IsManager::new(db.clone(), Arc::clone(&gate), Arc::new(MockClock::new())));
    let faults = Arc::new(Faults::default());
    let pool = Arc::new(FakeIsMempool::default());
    let checked =
        Arc::new(CheckedPool::new(Arc::clone(&pool), Arc::clone(&manager), db.clone(), None));
    let signer = Arc::new(Signer::default());
    let synced = Arc::new(Synced(AtomicBool::new(true)));
    let tip = IsTipCache::new();
    let listener = listener_on(db.clone(), &manager, gate, &tip, &faults, &synced, &signer)
        .with_mempool(Arc::clone(&checked) as _);
    let view = IsAcceptView {
        manager: Arc::clone(&manager),
        faults: Arc::clone(&faults) as _,
    };
    Rig {
        engine,
        db,
        spork2,
        manager,
        view,
        faults,
        pool,
        checked,
        signer,
        synced,
        tip,
        listener,
    }
}

fn rig() -> Rig {
    rig_on(Arc::new(MemoryEngine::new(&SCHEMA)))
}

fn coin(tag: u8) -> OutPoint {
    OutPoint::new(TxHash::from([tag; 32]), 0)
}

fn spend(inputs: &[OutPoint], tag: u32) -> TxRef {
    let ins = inputs.iter().map(|o| TxIn::new(*o, Default::default(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(1, TxType::new(0), ins, vec![], tag, vec![])))
}

/// A version-3 asset unlock of withdrawal `index`; `tag` varies the instance's txid.
fn asset_unlock(index: u64, tag: u8) -> TxRef {
    let mut payload = vec![0; dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[1..9].copy_from_slice(&index.to_le_bytes());
    payload[20] = tag;
    Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
}

fn lock_of(tx: &Tx) -> Arc<LockedTx> {
    Arc::new(LockedTx::new(1, lock_inputs(tx), tx.txid(), Default::default(), [0; 96].into()))
}

/// `tx`'s lock, written to the store.
fn written(r: &Rig, tx: &Tx) -> (IsLockHash, Arc<LockedTx>) {
    let lock = lock_of(tx);
    let hash = isdlock_hash(&lock);
    r.db.write_new(&hash, &lock).unwrap();
    (hash, lock)
}

/// `tx`'s lock, verified and parked waiting for `tx`.
fn parked(r: &Rig, tx: &Tx) -> (IsLockHash, Arc<LockedTx>) {
    let lock = lock_of(tx);
    let hash = isdlock_hash(&lock);
    r.manager.add_pending_is_lock(hash, Arc::clone(&lock), 3, false);
    (hash, lock)
}

fn tracked(r: &Rig) -> usize {
    r.manager.counts().unwrap().unprotected_tx
}

/// ATMP's `tx-txlock-conflict` read: a stored lock on one of the candidate's inputs for another
/// txid, none for the locked transaction itself, none with InstantSend off.
// Core: validation.cpp:902-911; evo/chainhelper.cpp:79-85; instantsend/instantsend.cpp:428-445
#[test]
fn a_respend_of_a_locked_input_is_a_txlock_conflict() {
    let r = rig();
    let locked = spend(&[coin(1), coin(2)], 0);
    let (hash, _) = written(&r, &locked);
    let respend = spend(&[coin(3), coin(2)], 1);
    let want = Some(dashd_mempool::types::IsConflict {
        lock_hash: dash_num::Hash256::from(hash),
        txid: locked.txid(),
    });
    assert!(r.view.is_enabled());
    assert_eq!(r.view.conflicting_lock(&respend), want);
    assert_eq!(r.view.conflicting_lock(&locked), None);
    assert_eq!(r.view.conflicting_lock(&spend(&[coin(4)], 2)), None);
    assert!(r.view.is_locked(&locked.txid()));
    assert!(!r.view.is_locked(&respend.txid()));
    r.spork2.store(false, Ordering::SeqCst);
    assert!(!r.view.is_enabled());
    assert_eq!(r.view.conflicting_lock(&respend), None);
    assert!(!r.view.is_locked(&locked.txid()));
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// `keep_cache` (`instantsend/instantsend.cpp:518-524`).
// Core: instantsend/instantsend.cpp:518-524; instantsend/db.cpp:71-89, :149; validation.cpp:902
#[test]
fn a_removed_confirmed_lock_is_still_found() {
    let r = rig();
    let locked = spend(&[coin(1)], 0);
    let respend = spend(&[coin(1)], 1);
    let (hash, _) = written(&r, &locked);
    assert!(r.view.conflicting_lock(&respend).is_some(), "the caches now hold the lock");
    r.db.write_mined(&hash, 10).unwrap();
    // No bypass-cache read here: it would insert a negative entry (`db.cpp:281-282`).
    assert_eq!(r.db.remove_confirmed(10).unwrap().into_keys().collect::<Vec<_>>(), [hash]);
    assert_eq!(r.view.conflicting_lock(&respend).map(|c| c.txid), Some(locked.txid()));
    assert!(r.view.is_locked(&locked.txid()));

    let restarted = rig_on(Arc::clone(&r.engine));
    assert_eq!(restarted.view.conflicting_lock(&respend), None);
}

/// `IsWaitingForTx` is true while a verified lock is parked for the txid; the transaction's
/// arrival moves the lock back to the received queue.
// Core: validation.cpp:913-916; instantsend/instantsend.cpp:160-175, :409-426
#[test]
fn a_parked_lock_waits_for_its_transaction_until_it_arrives() {
    let r = rig();
    let tx = spend(&[coin(1)], 0);
    assert!(!r.view.is_waiting_for_tx(&tx.txid()));
    let (hash, _) = parked(&r, &tx);
    assert!(r.view.is_waiting_for_tx(&tx.txid()));
    r.spork2.store(false, Ordering::SeqCst);
    assert!(!r.view.is_waiting_for_tx(&tx.txid()));
    r.spork2.store(true, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert!(!r.view.is_waiting_for_tx(&tx.txid()));
    assert!(r.manager.has_tx_for_lock(&hash));
    let fetched: Vec<_> =
        r.manager.fetch_pending_locks().pending_is.iter().map(|e| e.islock_hash).collect();
    assert_eq!(fetched, [hash]);
    assert_eq!(tracked(&r), 0, "an attached transaction is not tracked");
}

/// The transaction of a parked lock arrives: its conflicting spends leave the pool with their
/// descendants, and only the spends (the roots) leave the tracking and the signer's queue.
// Core: instantsend/net_instantsend.cpp:452-462, :465-473, :479-505
#[test]
fn an_attached_lock_removes_its_pool_conflicts_recursively() {
    let r = rig();
    let tx = spend(&[coin(1), coin(2)], 0);
    let conflict = spend(&[coin(2)], 1);
    let child = spend(&[OutPoint::new(conflict.txid(), 0)], 2);
    let unrelated = spend(&[coin(5)], 3);
    for t in [&conflict, &child, &unrelated] {
        r.pool.insert(Arc::clone(t));
        r.listener.transaction_added_to_mempool(t, 0, 0);
    }
    assert_eq!(tracked(&r), 3 + 2, "three transactions and two outside parents");
    parked(&r, &tx);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert!(r.pool.exists(&unrelated.txid()));
    assert!(!r.pool.exists(&conflict.txid()));
    assert!(!r.pool.exists(&child.txid()));
    assert_eq!(*r.signer.0.lock().unwrap(), [1], "ClearInputsFromQueue for the root only");
    assert_eq!(tracked(&r), 2 + 1, "the child, the unrelated spend and its parent");
    assert_eq!(r.checked.mutators.load(Ordering::SeqCst), 1);
}

/// An asset unlock's lock removes the pool's other instances of its withdrawal, which mapNextTx
/// cannot reveal. They are not conflict roots, so their tracking goes only with their own
/// removal notification.
// Core: instantsend/net_instantsend.cpp:496-504; instantsend/instantsend.cpp:131-141
#[test]
fn an_asset_unlock_s_lock_removes_the_other_instances_of_its_withdrawal() {
    let r = rig();
    let unlock = asset_unlock(7, 1);
    let other = asset_unlock(7, 2);
    let another_index = asset_unlock(8, 3);
    assert_ne!(unlock.txid(), other.txid());
    for (t, index) in [(&unlock, 7), (&other, 7), (&another_index, 8)] {
        r.pool.insert(Arc::clone(t));
        assert!(r.pool.set_asset_unlock(&t.txid(), index, 1000));
    }
    r.listener.transaction_added_to_mempool(&other, 0, 0);
    parked(&r, &unlock);
    r.listener.transaction_added_to_mempool(&unlock, 0, 0);
    assert!(r.pool.exists(&unlock.txid()));
    assert!(!r.pool.exists(&other.txid()));
    assert!(r.pool.exists(&another_index.txid()));
    assert!(r.signer.0.lock().unwrap().is_empty());
    let before = tracked(&r);
    r.listener.transaction_removed_from_mempool(&other, RemovalReason::Conflict, 0);
    assert!(tracked(&r) < before, "the removed instance's tracking is released");
}

/// Eviction releases an unlocked transaction's tracking; a mined one stays tracked (so a later
/// conflicting lock still finds its block) unless it is a platform transfer; a transaction
/// without lock inputs changes nothing.
// Core: instantsend/instantsend.cpp:131-141, :209-255; net_instantsend.cpp:530-534
#[test]
fn transaction_is_removed_releases_unmined_tracking() {
    let r = rig();
    for reason in [RemovalReason::Expiry, RemovalReason::SizeLimit] {
        let tx = spend(&[coin(1)], 0);
        r.listener.transaction_added_to_mempool(&tx, 0, 0);
        assert!(Arc::strong_count(&tx) > 1);
        r.listener.transaction_removed_from_mempool(&tx, reason, 0);
        assert_eq!(Arc::strong_count(&tx), 1);
        assert_eq!(tracked(&r), 0);
    }
    let mined = MinedBlock {
        id: BlockId(5),
        height: 5,
        hash: BlockHash::from([5; 32]),
    };
    let unlock = asset_unlock(9, 0);
    let ordinary = spend(&[coin(2)], 0);
    r.manager.add_non_locked_tx(&unlock, Some(mined));
    r.manager.add_non_locked_tx(&ordinary, Some(mined));
    r.listener.transaction_removed_from_mempool(&unlock, RemovalReason::Conflict, 0);
    r.listener.transaction_removed_from_mempool(&ordinary, RemovalReason::Manual, 0);
    assert_eq!(Arc::strong_count(&unlock), 1, "a platform transfer is released when mined");
    assert_eq!(Arc::strong_count(&ordinary), 2, "a mined ordinary transaction stays tracked");
    let no_lock_inputs = spend(&[], 0);
    r.listener.transaction_removed_from_mempool(&no_lock_inputs, RemovalReason::Expiry, 0);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// A locked transaction leaving the pool removes its lock and every lock chained on it, each
/// archived at the cached tip.
// Core: instantsend/instantsend.cpp:143-147, :342-353; instantsend/db.cpp:348-388
#[test]
fn a_locked_transaction_s_removal_removes_its_chained_locks() {
    let r = rig();
    r.tip.set(50);
    let tx = spend(&[coin(1)], 0);
    let child = spend(&[OutPoint::new(tx.txid(), 0)], 1);
    let (hash, _) = written(&r, &tx);
    let (child_hash, _) = written(&r, &child);
    r.listener.transaction_removed_from_mempool(&tx, RemovalReason::Expiry, 0);
    for h in [hash, child_hash] {
        assert_eq!(r.db.by_hash(&h, false).unwrap(), None);
        assert!(r.db.known(&h).unwrap(), "archived");
    }
    assert_eq!(r.view.conflicting_lock(&spend(&[coin(1)], 2)), None);
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// The removal runs on the signals thread, off V: while a chain attempt holds a stage it waits
/// on the gate, then lands after the commit, as Core's waits on `cs_db`.
// Core: instantsend/db.cpp:348-388; PLAN-LK1b §5.3 "Attempt gate"
#[test]
fn a_locked_transaction_s_removal_waits_behind_an_attempt_s_stage() {
    let r = rig();
    let (staged_hash, staged) = written(&r, &spend(&[coin(7)], 0));
    let tx = spend(&[coin(1)], 0);
    let (hash, _) = written(&r, &tx);
    let mut overlay = IsOverlay::default();
    let (_, stage) = r.db.stage_remove_chained(&mut overlay, &staged_hash, &staged, 10).unwrap();
    std::thread::scope(|scope| {
        let signals = scope.spawn(|| {
            r.listener.transaction_removed_from_mempool(&tx, RemovalReason::Expiry, 0);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while r.db.gate_waiters() == 0 && !signals.is_finished() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the gate");
            std::thread::yield_now();
        }
        assert!(!signals.is_finished(), "the removal waits for the attempt");
        r.db.commit_staged(&mut overlay, stage).unwrap();
        signals.join().unwrap();
    });
    assert_eq!(r.db.by_hash(&hash, false).unwrap(), None);
    assert!(r.db.known(&hash).unwrap(), "archived");
    assert!(r.faults.0.lock().unwrap().is_empty());
}

/// `IsStore::remove_chained` called off V while an attempt holds a stage waits on the gate and
/// then writes; a staging path would refuse the second thread instead.
// Core: instantsend/db.cpp:348-388 (LOCK(cs_db)); PLAN-LK1b §5.3 "Attempt gate"
#[test]
fn the_store_s_direct_removal_waits_off_v_behind_an_attempt_s_stage() {
    let r = rig();
    let (staged_hash, staged) = written(&r, &spend(&[coin(7)], 0));
    let tx = spend(&[coin(1)], 0);
    let (hash, lock) = written(&r, &tx);
    let mut overlay = IsOverlay::default();
    let (_, stage) = r.db.stage_remove_chained(&mut overlay, &staged_hash, &staged, 10).unwrap();
    let store: Arc<dyn IsStore> = r.db.clone();
    std::thread::scope(|scope| {
        let off_v = scope.spawn(|| store.remove_chained(&hash, &lock, 10));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while r.db.gate_waiters() == 0 && !off_v.is_finished() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the gate");
            std::thread::yield_now();
        }
        assert!(!off_v.is_finished(), "off-V removal must wait, not fail");
        r.db.commit_staged(&mut overlay, stage).unwrap();
        let removal = off_v.join().unwrap().unwrap();
        assert_eq!(removal.hashes, [hash]);
    });
    assert_eq!(r.db.by_hash(&hash, false).unwrap(), None);
    assert!(r.db.known(&hash).unwrap(), "archived");
}

/// A store failure while removing the lock is reported once, and nothing is committed.
// Core: instantsend/instantsend.cpp:143-147 (a thrown dbwrapper_error ends the callback)
#[cfg(feature = "test-support")]
#[test]
fn a_failed_lock_removal_is_reported_once() {
    use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
    let tx = spend(&[coin(1)], 0);
    let mut store = InMemoryIsStore::default();
    store.insert(LockedTx::clone(&lock_of(&tx)));
    let error = StoreError::engine("test", dash_kv::ErrorKind::Io, "injected");
    store.scripted.get_mut().unwrap().stage_remove_chained.push_back(Err(error));
    let store = Arc::new(store);
    let gate: GateSource = Arc::new(|| IsGate::new(true, false, false));
    let manager =
        Arc::new(IsManager::new(store.clone(), Arc::clone(&gate), Arc::new(MockClock::new())));
    let faults = Arc::new(Faults::default());
    let synced = Arc::new(Synced(AtomicBool::new(true)));
    let signer = Arc::new(Signer::default());
    let listener =
        listener_on(store.clone(), &manager, gate, &IsTipCache::new(), &faults, &synced, &signer);
    listener.transaction_removed_from_mempool(&tx, RemovalReason::Expiry, 0);
    assert_eq!(*faults.0.lock().unwrap(), ["InstantSend TransactionRemovedFromMempool"]);
    assert!(!store.calls().iter().any(|c| matches!(c, IsStoreCall::CommitStaged(_))));
}

/// Re-pin R18 (MN4-08, Core #7839): eviction releases an unlocked transaction's tracking;
/// a peer rejection keeps a mined one; and an unmined spend of the same input leaving the pool
/// does not drop the mined transaction's outpoint entry, so a conflicting lock still finds its
/// block until confirmation untracks it.
// Core: src/test/evo_islock_tests.cpp:541-602; instantsend/instantsend.cpp:131-141, :239-254
#[test]
fn r18_removal_keeps_the_mined_owner_of_a_shared_outpoint() {
    let r = rig();
    for reason in [RemovalReason::Expiry, RemovalReason::SizeLimit] {
        let tx = spend(&[coin(1)], u32::from(reason == RemovalReason::Expiry));
        r.listener.transaction_added_to_mempool(&tx, 0, 0);
        r.listener.transaction_removed_from_mempool(&tx, reason, 0);
        assert_eq!(Arc::strong_count(&tx), 1);
    }
    let block = MinedBlock {
        id: BlockId(100),
        height: 100,
        hash: BlockHash::from([7; 32]),
    };
    let mined = spend(&[coin(9)], 0);
    r.manager.add_non_locked_tx(&mined, Some(block));
    r.listener.transaction_removed_from_mempool(&mined, RemovalReason::Manual, 0);
    assert_eq!(Arc::strong_count(&mined), 2);
    let respend = spend(&[coin(9)], 1);
    r.listener.transaction_added_to_mempool(&respend, 0, 0);
    r.listener.transaction_removed_from_mempool(&respend, RemovalReason::Expiry, 0);
    assert_eq!(Arc::strong_count(&respend), 1);
    let conflict =
        LockedTx::new(1, vec![coin(9)], TxHash::from([1; 32]), Default::default(), [0; 96].into());
    let found = r.manager.retrieve_is_conflicts(&[3; 32], &conflict);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].0, block);
    assert_eq!(found[0].1.iter().map(|t| t.txid()).collect::<Vec<_>>(), [mined.txid()]);
    drop(found);
    r.manager.remove_non_locked_tx(&mined.txid(), false, false);
    assert_eq!(Arc::strong_count(&mined), 1);
}

/// Both hooks read the listener's sync source, the one BlockConnected reads (S4 review r2 M2).
// Core: instantsend/net_instantsend.cpp:449, :544 (one m_mn_sync)
#[test]
fn the_mempool_hook_reads_the_listener_s_sync_state() {
    let r = rig();
    let tx = spend(&[coin(1)], 0);
    r.synced.0.store(false, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert_eq!(tracked(&r), 0);
    r.synced.0.store(true, Ordering::SeqCst);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert_eq!(tracked(&r), 2, "the transaction and its parent");
}

/// Hooks ATMP calls under the pool lock answer without calling a pool mutator (they hold no pool
/// handle), and the listener calls the pool's mutator with every manager and store mutex free.
// MP1a PLAN §3 contracts 2-3; Core: validation.cpp:902-916 under m_pool.cs;
// instantsend/net_instantsend.cpp:479-505 (pool.cs, then ClearConflicting)
#[test]
fn no_lk1b_lock_is_held_across_a_pool_mutator() {
    let r = rig();
    let tx = spend(&[coin(1)], 0);
    let conflict = spend(&[coin(1)], 1);
    r.pool.insert(Arc::clone(&conflict));
    r.listener.transaction_added_to_mempool(&conflict, 0, 0);
    let _ = r.view.conflicting_lock(&tx);
    let _ = r.view.is_waiting_for_tx(&tx.txid());
    let _ = r.view.is_locked(&tx.txid());
    assert_eq!(r.checked.mutators.load(Ordering::SeqCst), 0, "the hooks call no pool mutator");
    parked(&r, &tx);
    r.listener.transaction_added_to_mempool(&tx, 0, 0);
    assert_eq!(r.checked.mutators.load(Ordering::SeqCst), 1);
    assert!(!r.pool.exists(&conflict.txid()));
}
