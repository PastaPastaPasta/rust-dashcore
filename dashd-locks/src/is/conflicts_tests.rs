//! [`IsBlockConflicts`] over the manager and the IS store (PLAN-LK1b §6 S4): V's half of
//! `ResolveBlockConflicts` asked as V asks it (`instantsend/net_instantsend.cpp:584-643`).

use std::sync::{Arc, Mutex};

use dash_clock::MockClock;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_consensus::{BlockId, Tx};
use dash_evo::locks::isdlock_hash;
use dash_kv::{ErrorKind, MemoryEngine, StoreError};
use dash_num::Hash256;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::IsConflicts;

use super::*;
use crate::is::chain::GateSource;
use crate::is::db::{IsDb, SCHEMA};
use crate::is::manager::MinedBlock;
use crate::is::seams::test_support::InMemoryIsStore;
use crate::is::{IsGate, LockedTx, lock_inputs};

type Log = Arc<Mutex<Vec<String>>>;

struct Signer(Log);

impl IsSignerSeam for Signer {
    fn process_txs(&self, _: Vec<Arc<Tx>>, _: bool) {}

    fn retry(&self, _: Vec<Arc<Tx>>) {}

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        self.0.lock().unwrap().push(format!("clear_inputs {}", ids.len()));
    }

    fn clear_lock(&self, _: Arc<LockedTx>) {}
}

struct Faults(Log);

impl IsFaults for Faults {
    fn failed(&self, site: &'static str, error: &StoreError) {
        self.0.lock().unwrap().push(format!("{site}: {error}"));
    }
}

fn spend(inputs: &[OutPoint], lock_time: u32) -> Arc<Tx> {
    let ins = inputs.iter().map(|o| TxIn::new(*o, Default::default(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(1, TxType::new(0), ins, vec![], lock_time, vec![])))
}

fn coin(tag: u8) -> OutPoint {
    OutPoint::new(TxHash::from([tag; 32]), 0)
}

fn lock_of(tx: &Tx) -> Arc<LockedTx> {
    Arc::new(LockedTx::new(1, lock_inputs(tx), tx.txid(), Default::default(), [0; 96].into()))
}

fn mined(height: u32) -> MinedBlock {
    MinedBlock {
        id: BlockId(height),
        height,
        hash: BlockHash::from([height as u8; 32]),
    }
}

struct Rig {
    log: Log,
    manager: Arc<IsManager>,
    resolving: Arc<ResolvingLocks>,
    tip: IsTipCache,
    is: IsBlockConflicts,
}

fn rig(store: Arc<dyn IsStore>, manager_store: Arc<dyn crate::is::seams::IsManagerStore>) -> Rig {
    let log: Log = Arc::default();
    let gate: GateSource = Arc::new(|| IsGate::new(true, false, false));
    let manager = Arc::new(IsManager::new(manager_store, gate, Arc::new(MockClock::new())));
    let resolving = Arc::new(ResolvingLocks::default());
    let tip = IsTipCache::new();
    let is = IsBlockConflicts::new(IsConflictsParts {
        store,
        manager: Arc::clone(&manager),
        tip: tip.clone(),
        signer: Some(Arc::new(Signer(Arc::clone(&log)))),
        resolving: Arc::clone(&resolving),
        faults: Arc::new(Faults(Arc::clone(&log))),
    });
    Rig {
        log,
        manager,
        resolving,
        tip,
        is,
    }
}

fn db_rig() -> (Arc<IsDb<MemoryEngine>>, Rig) {
    let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
    let r = rig(db.clone(), db.clone());
    (db, r)
}

impl Rig {
    fn resolve(&self, islock: &Arc<LockedTx>) -> Hash256 {
        let hash = isdlock_hash(islock);
        self.resolving.0.lock().insert(hash, Arc::clone(islock));
        Hash256::from(hash)
    }
}

/// Only a tracked, mined transaction with its object, spending one of the lock's inputs under
/// another txid, conflicts; each block is named once, in the lock's input order.
// Core: instantsend/instantsend.cpp:304-334
#[test]
fn retrieve_names_each_block_of_a_mined_conflicting_transaction_once() {
    let (_, mut r) = db_rig();
    let tx = spend(&[coin(1), coin(2), coin(3), coin(4)], 0);
    let islock = lock_of(&tx);
    let in_b2 = spend(&[coin(2)], 1);
    let in_b1 = spend(&[coin(1), coin(3)], 2);
    let unmined = spend(&[coin(4)], 3);
    r.manager.add_non_locked_tx(&in_b1, Some(mined(1)));
    r.manager.add_non_locked_tx(&in_b2, Some(mined(2)));
    r.manager.add_non_locked_tx(&unmined, None);
    r.manager.add_non_locked_tx(&tx, Some(mined(5)));
    let lock = r.resolve(&islock);
    assert_eq!(r.is.retrieve_conflicts(&lock), vec![mined(1).hash, mined(2).hash]);

    assert!(r.is.has_tx_for_lock(&lock));
    r.manager.add_pending_is_lock(*lock.as_bytes(), Arc::clone(&islock), 1, false);
    assert!(!r.is.has_tx_for_lock(&lock));

    // `ClearConflicting(p.second)` for one block: its transactions leave the graph, the signer
    // forgets their inputs, and the other block's stay.
    r.is.clear_conflicting(&lock, &mined(1).hash);
    assert_eq!(*r.log.lock().unwrap(), ["clear_inputs 2"]);
    assert_eq!(r.is.retrieve_conflicts(&lock), vec![mined(2).hash]);
}

/// A lock V is not resolving has no conflicts and no removal.
#[test]
fn a_lock_not_being_processed_has_no_conflicts() {
    let (db, mut r) = db_rig();
    let tx = spend(&[coin(1)], 0);
    r.manager.add_non_locked_tx(&spend(&[coin(1)], 1), Some(mined(1)));
    let islock = lock_of(&tx);
    db.write_new(&isdlock_hash(&islock), &islock).unwrap();
    let lock = Hash256::from(isdlock_hash(&islock));
    assert!(r.is.retrieve_conflicts(&lock).is_empty());
    r.is.remove_conflicting_lock(&lock);
    assert!(db.by_hash(&isdlock_hash(&islock), false).unwrap().is_some());
}

/// A ChainLocked conflict removes the lock and the lock built on its transaction, both archived
/// at the cached tip, committed at once.
// Core: instantsend/net_instantsend.cpp:602-606; instantsend/instantsend.cpp:342-353;
// instantsend/db.cpp:348-388
#[test]
fn removing_a_conflicting_lock_removes_its_chain_at_the_tip() {
    let (db, mut r) = db_rig();
    let tx = spend(&[coin(1)], 0);
    let child_tx = spend(&[OutPoint::new(tx.txid(), 0)], 0);
    let (root, child) = (lock_of(&tx), lock_of(&child_tx));
    let (root_hash, child_hash) = (isdlock_hash(&root), isdlock_hash(&child));
    db.write_new(&root_hash, &root).unwrap();
    db.write_new(&child_hash, &child).unwrap();
    r.tip.set(40);
    let lock = r.resolve(&root);
    r.is.remove_conflicting_lock(&lock);
    for hash in [root_hash, child_hash] {
        assert_eq!(db.by_hash(&hash, false).unwrap(), None);
        assert!(db.known(&hash).unwrap(), "archived");
    }
    db.remove_archived(39).unwrap();
    assert!(db.known(&root_hash).unwrap());
    db.remove_archived(40).unwrap();
    assert!(!db.known(&root_hash).unwrap());
    assert!(r.log.lock().unwrap().is_empty());
}

/// A failed removal is reported once through the fault path, and nothing is committed.
#[test]
fn a_removal_store_failure_is_reported_once() {
    let store = Arc::new(InMemoryIsStore::default());
    let error = StoreError::engine("test", ErrorKind::Io, "scripted");
    store.scripted.lock().unwrap().stage_remove_chained.push_back(Err(error));
    let mut r = rig(store.clone(), store.clone());
    let lock = r.resolve(&lock_of(&spend(&[coin(1)], 0)));
    r.is.remove_conflicting_lock(&lock);
    assert_eq!(r.log.lock().unwrap().len(), 1);
    assert!(r.log.lock().unwrap()[0].starts_with("InstantSend RemoveConflictingLock: "));
    assert!(
        !store.calls().iter().any(|c| matches!(c, crate::is::seams::IsStoreCall::CommitStaged(_)))
    );
}
