//! [`IsProcessor`] (PLAN-LK1b §6 S4): `ProcessInstantSendLock`'s branches and call order
//! (`instantsend/net_instantsend.cpp:359-423`), each seam a recording double over one log.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use dash_clock::MockClock;
use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_evo::locks::isdlock_hash;
use dash_kv::MemoryEngine;
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use dashd_mempool::is::IsMempool;
use dashd_mempool::testkit::FakeIsMempool;

use super::*;
use crate::is::chain::GateSource;
use crate::is::db::{IsDb, SCHEMA};
use crate::is::listener::input_lock_request_id;
use crate::is::manager::MinedBlock;
use crate::is::mempool::tests::CheckedPool;
use crate::is::{IsGate, LockedTx};

type Log = Arc<Mutex<Vec<String>>>;

fn note(log: &Log, s: String) {
    log.lock().unwrap().push(s);
}

struct Txs(Option<(Arc<Tx>, Option<BlockHash>)>);

impl TxLookup for Txs {
    fn get_transaction(&self, _: &TxHash) -> Option<(Arc<Tx>, Option<BlockHash>)> {
        self.0.clone()
    }
}

/// V: heights by block, and a resolve that records what it sees of the lock.
struct Chain {
    log: Log,
    heights: BTreeMap<BlockHash, u32>,
    db: Arc<IsDb<MemoryEngine>>,
    manager: Arc<IsManager>,
    resolving: Arc<ResolvingLocks>,
}

impl IsChain for Chain {
    fn block_height(&self, hash: &BlockHash) -> Option<u32> {
        self.heights.get(hash).copied()
    }

    fn resolve_block_conflicts(&self, lock: &IsLockHash, _: &TxHash) {
        let stored = self.db.known(lock).unwrap();
        let has_tx = self.manager.has_tx_for_lock(lock);
        let held = self.resolving.get(lock).is_some();
        note(&self.log, format!("resolve stored={stored} has_tx={has_tx} held={held}"));
    }
}

struct ChainLocks(Option<(i32, BlockHash)>);

impl ChainLockReads for ChainLocks {
    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        self.0 == Some((height, *hash))
    }
}

struct RecSigs(Log);

impl RecoveredSigs for RecSigs {
    fn truncate(&self, _: LlmqType, id: &SignRequestId) -> Result<()> {
        note(&self.0, format!("truncate {}", id[0]));
        Ok(())
    }
}

struct Peers(Log);

impl IsPeers for Peers {
    fn misbehaving(&self, node: NodeId, score: i32) {
        note(&self.0, format!("misbehaving {node} {score}"));
    }

    fn erase_object_request(&self, _: NodeId, _: &IsLockHash) {}

    fn forget_object_request(&self, _: &IsLockHash) {}

    fn relay_inv_filtered_tx(&self, _: &IsLockHash, tx: &Arc<Tx>) {
        note(&self.0, format!("relay tx {}", tx.txid()));
    }

    fn relay_inv_filtered_txid(&self, _: &IsLockHash, txid: &TxHash) {
        note(&self.0, format!("relay txid {txid}"));
    }

    fn ask_peers_for_transaction(&self, txid: &TxHash) {
        note(&self.0, format!("ask {txid}"));
    }
}

struct Signer(Log);

impl IsSignerSeam for Signer {
    fn process_txs(&self, _: Vec<Arc<Tx>>, _: bool) {}

    fn retry(&self, _: Vec<Arc<Tx>>) {}

    fn clear_inputs(&self, ids: Vec<[u8; 32]>) {
        note(&self.0, format!("clear_inputs {}", ids.len()));
    }

    fn clear_lock(&self, _: Arc<LockedTx>) {
        note(&self.0, "clear_lock".into());
    }
}

struct Notify(Log);

impl IsNotify for Notify {
    fn notify_transaction_lock(&self, tx: &Arc<Tx>, _: &Arc<LockedTx>) {
        note(&self.0, format!("notify {}", tx.txid()));
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

fn block() -> BlockHash {
    BlockHash::from([9; 32])
}

struct Rig {
    log: Log,
    db: Arc<IsDb<MemoryEngine>>,
    manager: Arc<IsManager>,
    mempool: Arc<FakeIsMempool>,
    checked: Arc<CheckedPool>,
    processor: IsProcessor,
}

fn rig(found: Option<(Arc<Tx>, Option<BlockHash>)>, chainlock: Option<(i32, BlockHash)>) -> Rig {
    let log: Log = Arc::default();
    let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
    let gate: GateSource = Arc::new(|| IsGate::new(true, false, false));
    let manager = Arc::new(IsManager::new(db.clone(), gate, Arc::new(MockClock::new())));
    let resolving = Arc::new(ResolvingLocks::default());
    let mempool = Arc::new(FakeIsMempool::default());
    let checked = Arc::new(CheckedPool::new(
        Arc::clone(&mempool),
        Arc::clone(&manager),
        Arc::clone(&db),
        Some(Arc::clone(&resolving)),
    ));
    let processor = IsProcessor::new(IsProcessorParts {
        manager: Arc::clone(&manager),
        txs: Arc::new(Txs(found)),
        chain: Arc::new(Chain {
            log: Arc::clone(&log),
            heights: BTreeMap::from([(block(), 7)]),
            db: Arc::clone(&db),
            manager: Arc::clone(&manager),
            resolving: Arc::clone(&resolving),
        }),
        chainlocks: Arc::new(ChainLocks(chainlock)),
        recsigs: Arc::new(RecSigs(Arc::clone(&log))),
        peers: Arc::new(Peers(Arc::clone(&log))),
        signer: Some(Arc::new(Signer(Arc::clone(&log)))),
        mempool: Some(Arc::clone(&checked) as Arc<dyn IsMempool>),
        notify: Arc::new(Notify(Arc::clone(&log))),
        resolving,
        is_llmq: LlmqType::LlmqTest,
    });
    Rig {
        log,
        db,
        manager,
        mempool,
        checked,
        processor,
    }
}

impl Rig {
    fn process(&self, islock: &Arc<LockedTx>) {
        self.processor.process_instant_send_lock(3, &isdlock_hash(islock), islock, false).unwrap();
    }

    fn events(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

/// The truncations of `inputs`' request ids, in id order (`GetIdsFromLockable`'s set).
fn truncations(inputs: &[OutPoint]) -> Vec<String> {
    let mut ids: Vec<_> = inputs.iter().map(input_lock_request_id).collect();
    ids.sort();
    ids.into_iter().map(|id| format!("truncate {}", id[0])).collect()
}

/// Without the transaction the lock is parked, its block conflicts resolved while V reads it
/// from the parked queue, then relayed by txid with the transaction asked for.
// Core: instantsend/net_instantsend.cpp:364-367, :395-397, :400-404, :417-422
#[test]
fn a_lock_without_its_transaction_is_parked_resolved_and_asked_for() {
    let tx = spend(&[coin(1), coin(2)], 0);
    let islock = lock_of(&tx);
    let r = rig(None, None);
    r.process(&islock);
    let mut want = vec!["clear_lock".to_owned(), "clear_inputs 2".to_owned()];
    want.extend(truncations(islock.inputs()));
    want.extend([
        "resolve stored=false has_tx=false held=true".to_owned(),
        format!("relay txid {}", tx.txid()),
        format!("ask {}", tx.txid()),
    ]);
    assert_eq!(r.events(), want);
    assert!(r.manager.is_waiting_for_tx(&tx.txid()));
    assert!(!r.db.known(&isdlock_hash(&islock)).unwrap());
    assert_eq!(r.processor.parts.resolving.get(&isdlock_hash(&islock)), None);
}

/// A mempool transaction's lock is written unmined; after the resolution the pool's conflicting
/// spend goes, the lock is announced, the pool's counter bumps and the lock is relayed with its
/// transaction.
// Core: instantsend/net_instantsend.cpp:386-394, :400-415, :417-420; :475-505
#[test]
fn a_lock_with_its_mempool_transaction_is_written_and_announced() {
    let tx = spend(&[coin(1)], 0);
    let conflict = spend(&[coin(1)], 1);
    let islock = lock_of(&tx);
    let r = rig(Some((Arc::clone(&tx), None)), None);
    r.mempool.insert(Arc::clone(&conflict));
    r.manager.add_non_locked_tx(&conflict, None);
    r.process(&islock);
    let mut want = vec!["clear_lock".to_owned(), "clear_inputs 1".to_owned()];
    want.extend(truncations(islock.inputs()));
    want.extend([
        "resolve stored=true has_tx=true held=true".to_owned(),
        "clear_inputs 1".to_owned(),
        format!("notify {}", tx.txid()),
        format!("relay tx {}", tx.txid()),
    ]);
    assert_eq!(r.events(), want);
    let hash = isdlock_hash(&islock);
    assert_eq!(r.db.by_hash(&hash, false).unwrap().as_ref(), Some(&*islock));
    assert!(r.mempool.get(&conflict.txid()).is_none());
    assert_eq!(r.mempool.transactions_updated(), 1);
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 0);
}

/// A transaction from the txindex in a known block writes the lock mined at its height; a
/// ChainLock on another block does not drop it.
// Core: instantsend/net_instantsend.cpp:384-394; instantsend/instantsend.cpp:107-114
#[test]
fn a_mined_transaction_s_lock_is_written_mined() {
    let tx = spend(&[coin(1)], 0);
    let islock = lock_of(&tx);
    let r = rig(Some((Arc::clone(&tx), Some(block()))), Some((7, BlockHash::from([8; 32]))));
    r.process(&islock);
    let hash = isdlock_hash(&islock);
    assert!(r.db.known(&hash).unwrap());
    assert_eq!(r.db.remove_confirmed(7).unwrap().into_keys().collect::<Vec<_>>(), vec![hash]);
}

/// A transaction mined in a ChainLocked block drops the lock: nothing is written, parked,
/// resolved or relayed.
// Core: instantsend/net_instantsend.cpp:387-393
#[test]
fn a_lock_for_a_chainlocked_transaction_is_dropped() {
    let tx = spend(&[coin(1)], 0);
    let islock = lock_of(&tx);
    let r = rig(Some((Arc::clone(&tx), Some(block()))), Some((7, block())));
    r.manager.add_non_locked_tx(&tx, None);
    r.process(&islock);
    assert_eq!(r.events(), ["clear_lock"]);
    assert!(!r.db.known(&isdlock_hash(&islock)).unwrap());
    assert!(!r.manager.is_waiting_for_tx(&tx.txid()));
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 2, "the transaction and its parent");
}

/// A lock whose inputs are not its found transaction's lock inputs is dropped with no state
/// change, as is a lock the store already knows.
// Core: instantsend/net_instantsend.cpp:367, :371-382
#[test]
fn a_lock_input_mismatch_or_a_known_lock_is_dropped_unchanged() {
    let tx = spend(&[coin(1)], 0);
    let other = lock_of(&spend(&[coin(2)], 0));
    let islock = Arc::new(LockedTx::new(
        1,
        other.inputs().to_vec(),
        tx.txid(),
        Default::default(),
        [0; 96].into(),
    ));
    let r = rig(Some((Arc::clone(&tx), None)), None);
    r.manager.add_non_locked_tx(&tx, None);
    r.process(&islock);
    assert_eq!(r.events(), ["clear_lock"]);
    assert!(!r.db.known(&isdlock_hash(&islock)).unwrap());
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 2, "the transaction and its parent");

    let known = lock_of(&tx);
    r.db.write_new(&isdlock_hash(&known), &known).unwrap();
    r.log.lock().unwrap().clear();
    r.process(&known);
    assert_eq!(r.events(), ["clear_lock"]);
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 2);
}

/// The lock's transaction leaves the non-locked graph with its children queued for a retry,
/// before the block conflicts are resolved.
// Core: instantsend/net_instantsend.cpp:399-400
#[test]
fn processing_untracks_the_transaction_and_retries_its_children() {
    let tx = spend(&[coin(1)], 0);
    let child = spend(&[OutPoint::new(tx.txid(), 0)], 0);
    let r = rig(None, None);
    let mined = MinedBlock {
        id: dash_consensus::BlockId(1),
        height: 1,
        hash: block(),
    };
    r.manager.add_non_locked_tx(&tx, Some(mined));
    r.manager.add_non_locked_tx(&child, None);
    r.process(&lock_of(&tx));
    assert_eq!(r.manager.counts().unwrap().unprotected_tx, 1);
    let retried: Vec<_> = r.manager.prepare_tx_to_retry().iter().map(|tx| tx.txid()).collect();
    assert_eq!(retried, [child.txid()]);
}

/// A found transaction's lock removes the pool's conflicting spend with its descendants, and
/// bumps the pool's counter once; a parked lock calls no pool mutator. Every mutator runs with no
/// manager, store or resolving-lock mutex held.
// Core: instantsend/net_instantsend.cpp:407-414, :479-505
#[test]
fn a_lock_removes_recursive_pool_conflicts_and_bumps_the_counter() {
    let tx = spend(&[coin(1), coin(2)], 0);
    let conflict = spend(&[coin(2)], 1);
    let child = spend(&[OutPoint::new(conflict.txid(), 0)], 0);
    let grandchild = spend(&[OutPoint::new(child.txid(), 0)], 0);
    let r = rig(Some((Arc::clone(&tx), None)), None);
    for t in [&conflict, &child, &grandchild] {
        r.mempool.insert(Arc::clone(t));
    }
    r.process(&lock_of(&tx));
    for t in [&conflict, &child, &grandchild] {
        assert!(r.mempool.get(&t.txid()).is_none());
    }
    assert_eq!(r.mempool.transactions_updated(), 1);
    assert_eq!(r.checked.mutators.load(std::sync::atomic::Ordering::SeqCst), 2);

    let parked = spend(&[coin(3)], 0);
    let r = rig(None, None);
    r.mempool.insert(spend(&[coin(3)], 1));
    r.process(&lock_of(&parked));
    assert_eq!(r.mempool.transactions_updated(), 0);
    assert_eq!(r.checked.mutators.load(std::sync::atomic::Ordering::SeqCst), 0);
}

/// An asset unlock's lock removes the pool's other instance of its withdrawal.
// Core: instantsend/net_instantsend.cpp:496-501
#[test]
fn an_asset_unlock_s_lock_removes_its_pool_twin() {
    let unlock = |tag: u8| {
        let mut payload = vec![0; dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE];
        payload[1..9].copy_from_slice(&7u64.to_le_bytes());
        payload[20] = tag;
        Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
    };
    let (mine, twin) = (unlock(1), unlock(2));
    let r = rig(Some((Arc::clone(&mine), None)), None);
    for t in [&mine, &twin] {
        r.mempool.insert(Arc::clone(t));
        assert!(r.mempool.set_asset_unlock(&t.txid(), 7, 1));
    }
    r.process(&lock_of(&mine));
    assert!(r.mempool.get(&mine.txid()).is_some());
    assert!(r.mempool.get(&twin.txid()).is_none());
}
