use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::MockClock;
use dash_consensus::primitives::canonical::{
    ASSET_UNLOCK_PAYLOAD_SIZE, ASSET_UNLOCK_QUORUM_INFO_SIZE, ASSET_UNLOCK_STABLE_TXID_VERSION,
};
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_consensus::{BlockId, Tx};
use dash_evo::locks::isdlock_hash;
use dash_kv::MemoryEngine;
use dash_primitives::BlockHash;

use super::*;

use crate::is::IsGate;
use crate::is::db::{IsDb, SCHEMA};

struct World {
    db: Arc<IsDb<MemoryEngine>>,
    enabled: Arc<AtomicBool>,
    man: IsManager,
}

fn world() -> World {
    let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
    let enabled = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&enabled);
    let gate: GateSource = Arc::new(move || IsGate::new(flag.load(Ordering::SeqCst), false, false));
    let man = IsManager::new(db.clone(), gate, Arc::new(MockClock::new()));
    World {
        db,
        enabled,
        man,
    }
}

fn txid(tag: u32) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    TxHash::from(bytes)
}

fn lock(locked: TxHash, inputs: Vec<OutPoint>) -> Arc<LockedTx> {
    Arc::new(LockedTx::new(1, inputs, locked, Default::default(), [0; 96].into()))
}

/// A lock for `tx` over its lock inputs, with its hash.
fn lock_for(tx: &Tx) -> (IsLockHash, Arc<LockedTx>) {
    let lock = lock(tx.txid(), lock_inputs(tx));
    (isdlock_hash(&lock), lock)
}

/// A distinct lock per `tag`, for the queue tests.
fn tagged(tag: u32) -> (IsLockHash, Arc<LockedTx>) {
    let lock = lock(txid(tag), vec![OutPoint::new(txid(tag), 0)]);
    (isdlock_hash(&lock), lock)
}

fn spending(inputs: &[OutPoint], lock_time: u32) -> Arc<Tx> {
    let ins = inputs.iter().map(|o| TxIn::new(*o, Default::default(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(1, TxType::new(0), ins, vec![], lock_time, vec![])))
}

/// A version-2 asset unlock of withdrawal `index`; `quorum` fills the quorum info, which a
/// re-signed instance changes without changing the txid.
fn asset_unlock(index: u64, quorum: u8) -> Arc<Tx> {
    let mut payload = vec![0; ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[0] = ASSET_UNLOCK_STABLE_TXID_VERSION;
    payload[1..9].copy_from_slice(&index.to_le_bytes());
    payload[ASSET_UNLOCK_PAYLOAD_SIZE - ASSET_UNLOCK_QUORUM_INFO_SIZE..].fill(quorum);
    Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
}

fn mined(height: u32) -> MinedBlock {
    MinedBlock {
        id: BlockId(height),
        height,
        hash: BlockHash::from([height as u8; 32]),
    }
}

impl IsManager {
    fn pending_hashes(&self) -> (Vec<IsLockHash>, Vec<IsLockHash>) {
        let p = self.pending.lock();
        (p.incoming.hashes().collect(), p.no_tx.keys().copied().collect())
    }

    fn incoming_peer(&self, hash: &IsLockHash) -> Option<NodeId> {
        self.pending.lock().incoming.get(hash).map(|p| p.node_id)
    }

    fn tracked(&self, txid: &TxHash) -> Option<(Option<MinedBlock>, Option<TxHash>, Vec<TxHash>)> {
        let nl = self.non_locked.lock();
        nl.txs.get(txid).map(|i| {
            (
                i.mined,
                i.tx.as_ref().map(|t| t.instance_hash()),
                i.children.iter().copied().collect(),
            )
        })
    }

    fn outpoint_owner(&self, outpoint: &OutPoint) -> Option<TxHash> {
        self.non_locked.lock().by_outpoint.get(outpoint).copied()
    }

    fn retry_set(&self) -> Vec<TxHash> {
        self.pending_retry.lock().iter().copied().collect()
    }
}

// Core: instantsend/instantsend.h:57-63; instantsend/instantsend.cpp:47-55 (the received queue
// drops at 1024 and never replaces an entry).
#[test]
fn received_queue_caps_at_1024_and_keeps_the_first_entry() {
    let w = world();
    for tag in 0..MAX_PENDING_INSTANTSEND_LOCKS as u32 {
        let (hash, lock) = tagged(tag);
        w.man.enqueue_instant_send_lock(1, hash, lock, false);
    }
    let (first, first_lock) = tagged(0);
    w.man.enqueue_instant_send_lock(2, first, first_lock, false);
    assert_eq!(w.man.incoming_peer(&first), Some(1));
    let (over, over_lock) = tagged(5000);
    w.man.enqueue_instant_send_lock(1, over, over_lock, false);
    let (incoming, _) = w.man.pending_hashes();
    assert_eq!(incoming.len(), MAX_PENDING_INSTANTSEND_LOCKS);
    assert!(!incoming.contains(&over));
}

// Core: instantsend/instantsend.cpp:116-129 (the no-tx queue drops at 1024, try_emplace).
#[test]
fn no_tx_queue_caps_at_1024_and_keeps_the_first_entry() {
    let w = world();
    for tag in 0..MAX_PENDING_INSTANTSEND_LOCKS as u32 {
        let (hash, lock) = tagged(tag);
        w.man.add_pending_is_lock(hash, lock, 1, false);
    }
    let (over, over_lock) = tagged(5000);
    w.man.add_pending_is_lock(over, over_lock, 1, false);
    let (incoming, no_tx) = w.man.pending_hashes();
    assert!(incoming.is_empty());
    assert_eq!(no_tx.len(), MAX_PENDING_INSTANTSEND_LOCKS);
    assert!(!no_tx.contains(&over));
    assert_eq!(w.man.counts().unwrap().awaiting_tx, MAX_PENDING_INSTANTSEND_LOCKS);
}

// Core: instantsend/instantsend.cpp:170 and :298-301 (AttachISLockToTx and TryEmplacePendingLock
// insert into the received queue without its cap).
#[test]
fn attach_and_try_emplace_bypass_the_received_cap() {
    let w = world();
    for tag in 0..MAX_PENDING_INSTANTSEND_LOCKS as u32 {
        let (hash, lock) = tagged(tag);
        w.man.enqueue_instant_send_lock(1, hash, lock, false);
    }
    let tx = spending(&[OutPoint::new(txid(9000), 0)], 0);
    let (hash, parked) = lock_for(&tx);
    w.man.add_pending_is_lock(hash, parked, 3, false);
    assert!(w.man.attach_is_lock_to_tx(&tx).is_some());
    let (extra, extra_lock) = tagged(9001);
    w.man.try_emplace_pending_lock(extra, 4, extra_lock, false).unwrap();
    let (incoming, _) = w.man.pending_hashes();
    assert_eq!(incoming.len(), MAX_PENDING_INSTANTSEND_LOCKS + 2);
    assert_eq!(w.man.incoming_peer(&hash), Some(3));
    assert_eq!(w.man.incoming_peer(&extra), Some(4));
}

// Core: instantsend/instantsend.cpp:294-302 (TryEmplacePendingLock skips a known lock and an
// already received one).
#[test]
fn try_emplace_skips_known_and_received_locks() {
    let w = world();
    let (stored, stored_lock) = tagged(1);
    w.db.write_new(&stored, &stored_lock).unwrap();
    w.man.try_emplace_pending_lock(stored, 1, stored_lock, false).unwrap();
    let (queued, queued_lock) = tagged(2);
    w.man.enqueue_instant_send_lock(1, queued, Arc::clone(&queued_lock), false);
    w.man.try_emplace_pending_lock(queued, 2, queued_lock, false).unwrap();
    assert_eq!(w.man.pending_hashes().0, vec![queued]);
    assert_eq!(w.man.incoming_peer(&queued), Some(1));
}

// Core: instantsend/instantsend.cpp:58-85 (at most 32 per fetch; m_pending_work only when a 33rd
// entry was left).
#[test]
fn fetch_takes_at_most_32_and_reports_more_work() {
    let w = world();
    assert_eq!(w.man.fetch_pending_locks(), PendingState::default());
    for tag in 0..33 {
        let (hash, lock) = tagged(tag);
        w.man.enqueue_instant_send_lock(i64::from(tag), hash, lock, false);
    }
    let first = w.man.fetch_pending_locks();
    assert_eq!(first.pending_is.len(), MAX_FETCHED_PENDING_LOCKS);
    assert!(first.pending_work);
    for entry in &first.pending_is {
        assert_eq!(entry.islock_hash, isdlock_hash(&entry.islock));
        assert_eq!(entry.islock.txid(), txid(entry.node_id as u32));
    }
    let second = w.man.fetch_pending_locks();
    assert_eq!(second.pending_is.len(), 1);
    assert!(!second.pending_work);
    assert!(first.pending_is.iter().all(|e| e.islock_hash != second.pending_is[0].islock_hash));
    assert_eq!(w.man.pending_hashes().0, Vec::<IsLockHash>::new());

    for tag in 100..132 {
        let (hash, lock) = tagged(tag);
        w.man.enqueue_instant_send_lock(1, hash, lock, false);
    }
    let exact = w.man.fetch_pending_locks();
    assert_eq!(exact.pending_is.len(), 32);
    assert!(!exact.pending_work);
}

// Core: instantsend/instantsend.cpp:58-85, instantsend/instantsend.h:66, saltedhasher.h:47-52,
// saltedhasher.cpp:8-10 (`pendingInstantSendLocks` is a salted unordered map: which 32 locks one
// fetch takes is the salt's, not the lock hashes', so a peer cannot grind its locks to the front).
#[test]
fn fetch_order_follows_the_salt() {
    let fetch = |salt| {
        let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
        let gate: GateSource = Arc::new(|| IsGate::new(true, false, false));
        let man = IsManager::with_salt(db, gate, Arc::new(MockClock::new()), salt);
        for tag in 0..40 {
            let (hash, lock) = tagged(tag);
            man.enqueue_instant_send_lock(1, hash, lock, false);
        }
        let first: Vec<IsLockHash> =
            man.fetch_pending_locks().pending_is.iter().map(|e| e.islock_hash).collect();
        assert_eq!(first.len(), MAX_FETCHED_PENDING_LOCKS);
        first
    };
    let a = fetch(FetchSalt(1, 2));
    assert_eq!(fetch(FetchSalt(1, 2)), a);
    let b = fetch(FetchSalt(3, 4));
    let set = |v: &[IsLockHash]| v.iter().copied().collect::<std::collections::BTreeSet<_>>();
    assert_ne!(set(&a), set(&b));
    let mut by_hash: Vec<IsLockHash> = (0..40).map(|tag| tagged(tag).0).collect();
    by_hash.sort();
    assert!(set(&a) != set(&by_hash[..32]) || set(&b) != set(&by_hash[..32]));
    assert_ne!(FetchSalt::random(), FetchSalt::random());
}

// Core: instantsend/instantsend.cpp:160-175, :336-340, :409-426 (a parked lock moves back to the
// received queue once its transaction arrives, without replacing a received entry).
#[test]
fn attach_moves_a_parked_lock_back_to_the_received_queue() {
    let w = world();
    let tx = spending(&[OutPoint::new(txid(7), 1)], 0);
    let (hash, parked) = lock_for(&tx);
    assert!(w.man.attach_is_lock_to_tx(&tx).is_none());
    w.man.add_pending_is_lock(hash, Arc::clone(&parked), 5, false);
    assert!(!w.man.has_tx_for_lock(&hash));
    assert!(w.man.is_waiting_for_tx(&tx.txid()));

    w.man.enqueue_instant_send_lock(6, hash, Arc::clone(&parked), false);
    assert_eq!(w.man.attach_is_lock_to_tx(&tx), Some(parked));
    assert_eq!(w.man.incoming_peer(&hash), Some(6));
    assert!(w.man.has_tx_for_lock(&hash));
    assert!(!w.man.is_waiting_for_tx(&tx.txid()));
    assert_eq!(w.man.pending_hashes(), (vec![hash], vec![]));
    assert!(w.man.attach_is_lock_to_tx(&tx).is_none());

    w.enabled.store(false, Ordering::SeqCst);
    let (other, other_lock) = tagged(3);
    w.man.add_pending_is_lock(other, other_lock, 1, false);
    assert!(!w.man.is_waiting_for_tx(&txid(3)));
}

// Core: instantsend/instantsend.cpp:177-207 (AddNonLockedTx attaches a parked lock for the tx).
#[test]
fn add_non_locked_tx_attaches_a_parked_lock() {
    let w = world();
    let tx = spending(&[OutPoint::new(txid(7), 0)], 0);
    let (hash, parked) = lock_for(&tx);
    w.man.add_pending_is_lock(hash, parked, 2, false);
    w.man.add_non_locked_tx(&tx, None);
    assert_eq!(w.man.pending_hashes(), (vec![hash], vec![]));
    assert!(w.man.tracked(&tx.txid()).is_some());
}

// Core: instantsend/instantsend.cpp:177-207 and :209-255 (the graph: parent placeholders with
// children, the first spender owns an outpoint, removal erases only its own outpoint entries and
// drops an emptied placeholder).
#[test]
fn non_locked_graph_links_parents_children_and_outpoints() {
    let w = world();
    let parent = spending(&[OutPoint::new(txid(1), 0)], 0);
    let out = OutPoint::new(parent.txid(), 0);
    let child = spending(&[out], 1);
    let double = spending(&[out], 2);
    w.man.add_non_locked_tx(&child, None);
    assert_eq!(w.man.tracked(&parent.txid()), Some((None, None, vec![child.txid()])));
    assert_eq!(w.man.outpoint_owner(&out), Some(child.txid()));

    w.man.add_non_locked_tx(&double, Some(mined(9)));
    let mut kids = vec![child.txid(), double.txid()];
    kids.sort();
    assert_eq!(w.man.tracked(&parent.txid()), Some((None, None, kids)));
    assert_eq!(w.man.outpoint_owner(&out), Some(child.txid()));
    assert_eq!(w.man.counts().unwrap().unprotected_tx, 3);

    w.man.remove_non_locked_tx(&double.txid(), false, false);
    assert_eq!(w.man.outpoint_owner(&out), Some(child.txid()));
    assert_eq!(w.man.tracked(&parent.txid()), Some((None, None, vec![child.txid()])));
    w.man.remove_non_locked_tx(&child.txid(), false, false);
    assert_eq!(w.man.outpoint_owner(&out), None);
    assert_eq!(w.man.tracked(&parent.txid()), None);
    assert_eq!(w.man.counts().unwrap().unprotected_tx, 0);
}

// Core: instantsend/instantsend.cpp:181-196 (an existing entry, a placeholder parent included,
// takes the new block and a new instance, but never registers its inputs).
#[test]
fn existing_entry_updates_block_and_instance_only() {
    let w = world();
    let grand = OutPoint::new(txid(1), 0);
    let parent = spending(&[grand], 0);
    let child = spending(&[OutPoint::new(parent.txid(), 0)], 1);
    w.man.add_non_locked_tx(&child, None);
    w.man.add_non_locked_tx(&parent, Some(mined(4)));
    assert_eq!(
        w.man.tracked(&parent.txid()),
        Some((Some(mined(4)), Some(parent.instance_hash()), vec![child.txid()]))
    );
    assert_eq!(w.man.outpoint_owner(&grand), None);
    assert_eq!(w.man.tracked(&txid(1)), None);

    w.man.add_non_locked_tx(&parent, None);
    assert_eq!(w.man.tracked(&parent.txid()).unwrap().0, None);

    let first = asset_unlock(7, 1);
    let resigned = asset_unlock(7, 2);
    assert_eq!(first.txid(), resigned.txid());
    assert_ne!(first.instance_hash(), resigned.instance_hash());
    w.man.add_non_locked_tx(&first, None);
    w.man.add_non_locked_tx(&resigned, None);
    assert_eq!(w.man.tracked(&first.txid()).unwrap().1, Some(resigned.instance_hash()));
}

// Core: instantsend/instantsend.cpp:209-232 (keepMined keeps a mined entry; retryChildren queues
// the children; removal always unqueues the tx itself).
#[test]
fn remove_non_locked_keeps_mined_and_queues_children() {
    let w = world();
    let parent = spending(&[OutPoint::new(txid(1), 0)], 0);
    let child = spending(&[OutPoint::new(parent.txid(), 0)], 1);
    w.man.add_non_locked_tx(&parent, Some(mined(3)));
    w.man.add_non_locked_tx(&child, None);
    w.man.pending_retry.lock().insert(parent.txid());

    w.man.remove_non_locked_tx(&parent.txid(), true, true);
    assert!(w.man.tracked(&parent.txid()).is_some());
    assert_eq!(w.man.retry_set(), vec![parent.txid()]);

    w.man.remove_non_locked_tx(&parent.txid(), true, false);
    assert!(w.man.tracked(&parent.txid()).is_none());
    assert_eq!(w.man.retry_set(), vec![child.txid()]);
    assert_eq!(
        w.man.prepare_tx_to_retry().iter().map(|t| t.txid()).collect::<Vec<_>>(),
        vec![child.txid()]
    );

    w.man.remove_non_locked_tx(&txid(99), true, false);
    assert_eq!(w.man.retry_set(), vec![child.txid()]);
}

// Core: instantsend/instantsend.cpp:257-292 (ordinary retries stay queued; an asset unlock is
// handed out once per trigger; RetryUnminedAssetUnlocks re-queues only unmined tracked unlocks;
// an id without a tracked tx object stays queued and is not handed out).
#[test]
fn retry_sets_hand_out_asset_unlocks_once_per_trigger() {
    let w = world();
    let parent = spending(&[OutPoint::new(txid(1), 0)], 0);
    let child = spending(&[OutPoint::new(parent.txid(), 0)], 1);
    let unlock = asset_unlock(3, 1);
    let mined_unlock = asset_unlock(4, 1);
    w.man.add_non_locked_tx(&child, None);
    w.man.add_non_locked_tx(&unlock, None);
    w.man.add_non_locked_tx(&mined_unlock, Some(mined(2)));
    assert!(w.man.prepare_tx_to_retry().is_empty());

    w.man.retry_unmined_asset_unlocks();
    assert_eq!(w.man.retry_set(), vec![unlock.txid()]);
    {
        let mut retry = w.man.pending_retry.lock();
        retry.insert(child.txid());
        retry.insert(parent.txid());
    }
    let mut want = vec![child.txid(), unlock.txid()];
    want.sort();
    let mut got: Vec<_> = w.man.prepare_tx_to_retry().iter().map(|t| t.txid()).collect();
    got.sort();
    assert_eq!(got, want);
    let mut left = vec![child.txid(), parent.txid()];
    left.sort();
    assert_eq!(w.man.retry_set(), left);
    assert_eq!(
        w.man.prepare_tx_to_retry().iter().map(|t| t.txid()).collect::<Vec<_>>(),
        vec![child.txid()]
    );

    w.man.retry_unmined_asset_unlocks();
    assert!(w.man.prepare_tx_to_retry().iter().any(|t| t.txid() == unlock.txid()));
    assert!(!w.man.retry_set().contains(&unlock.txid()));
}

// Core: instantsend/instantsend.cpp:355-364 (AlreadyHave: true while disabled; else either
// pending map or the store, archives included).
#[test]
fn already_have_reads_both_queues_and_the_store() {
    let w = world();
    let (received, received_lock) = tagged(1);
    let (parked, parked_lock) = tagged(2);
    let (stored, stored_lock) = tagged(3);
    let (archived, archived_lock) = tagged(4);
    let (unknown, _) = tagged(5);
    w.man.enqueue_instant_send_lock(1, received, received_lock, false);
    w.man.add_pending_is_lock(parked, parked_lock, 1, false);
    w.db.write_new(&stored, &stored_lock).unwrap();
    w.db.write_new(&archived, &archived_lock).unwrap();
    w.db.write_mined(&archived, 10).unwrap();
    w.db.remove_confirmed(10).unwrap();
    w.db.by_hash(&archived, false).unwrap();
    for hash in [received, parked, stored, archived] {
        assert!(w.man.already_have(&hash).unwrap());
    }
    assert!(!w.man.already_have(&unknown).unwrap());
    w.enabled.store(false, Ordering::SeqCst);
    assert!(w.man.already_have(&unknown).unwrap());
}

// Core: instantsend/instantsend.cpp:366-398 (GetInstantSendLockByHash: the store, then the
// received queue, then the no-tx queue; the store wins; nothing while disabled).
#[test]
fn lock_by_hash_reads_store_then_received_then_no_tx() {
    let w = world();
    let (received, received_lock) = tagged(1);
    let (parked, parked_lock) = tagged(2);
    let (stored, stored_lock) = tagged(3);
    w.man.enqueue_instant_send_lock(1, received, Arc::clone(&received_lock), false);
    w.man.add_pending_is_lock(parked, Arc::clone(&parked_lock), 1, false);
    w.db.write_new(&stored, &stored_lock).unwrap();
    w.man.add_pending_is_lock(stored, Arc::clone(&stored_lock), 1, false);
    let get = |hash| w.man.get_instant_send_lock_by_hash(&hash).unwrap();
    assert_eq!(get(received), Some(LockedTx::clone(&received_lock)));
    assert_eq!(get(parked), Some(LockedTx::clone(&parked_lock)));
    assert_eq!(get(stored), Some(LockedTx::clone(&stored_lock)));
    assert_eq!(get(tagged(9).0), None);
    assert_eq!(
        w.man.get_instant_send_lock_by_txid(&txid(3)).unwrap(),
        Some(LockedTx::clone(&stored_lock))
    );
    assert_eq!(w.man.get_instant_send_lock_by_txid(&txid(1)).unwrap(), None);

    w.enabled.store(false, Ordering::SeqCst);
    assert_eq!(get(received), None);
    assert_eq!(get(stored), None);
    assert_eq!(w.man.get_instant_send_lock_by_txid(&txid(3)).unwrap(), None);
}

// Core: instantsend/instantsend.cpp:400-407, :447-466 (IsLocked through the txid index and
// Known; the counts read the store rows and both queues).
#[test]
fn is_locked_and_counts_read_the_store() {
    let w = world();
    let (stored, stored_lock) = tagged(3);
    w.db.write_new(&stored, &stored_lock).unwrap();
    let (received, received_lock) = tagged(4);
    w.man.enqueue_instant_send_lock(1, received, received_lock, false);
    assert!(w.man.is_locked(&txid(3)).unwrap());
    assert!(!w.man.is_locked(&txid(4)).unwrap());
    w.man.add_non_locked_tx(&spending(&[OutPoint::new(txid(8), 0)], 0), None);
    assert_eq!(w.man.instant_send_lock_count().unwrap(), 1);
    assert_eq!(
        w.man.counts().unwrap(),
        Counts {
            verified: 1,
            unverified: 1,
            awaiting_tx: 0,
            unprotected_tx: 2,
        }
    );
    w.enabled.store(false, Ordering::SeqCst);
    assert!(!w.man.is_locked(&txid(3)).unwrap());
}

// Core: instantsend/instantsend.cpp:87-105 (PreVerifyIsLock: false for a known hash or a txid
// that already has a lock; a stored lock on an input is only logged).
#[test]
fn pre_verify_rejects_known_and_same_txid_locks_only() {
    let w = world();
    let shared = OutPoint::new(txid(50), 0);
    let stored_lock = lock(txid(1), vec![shared]);
    let stored = isdlock_hash(&stored_lock);
    w.db.write_new(&stored, &stored_lock).unwrap();
    assert!(!w.man.pre_verify_is_lock(&stored, &stored_lock, 1).unwrap());

    let same_tx = lock(txid(1), vec![OutPoint::new(txid(51), 0)]);
    assert!(!w.man.pre_verify_is_lock(&isdlock_hash(&same_tx), &same_tx, 1).unwrap());

    let conflicting = lock(txid(2), vec![shared]);
    assert!(w.man.pre_verify_is_lock(&isdlock_hash(&conflicting), &conflicting, 1).unwrap());
}

// Core: instantsend/instantsend.cpp:355-389 (a store failure surfaces; Core's dbwrapper throws).
#[cfg(feature = "test-support")]
#[test]
fn store_failures_surface() {
    use crate::is::seams::test_support::InMemoryIsStore;
    let store = Arc::new(InMemoryIsStore::default());
    let error = || dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "scripted");
    store.scripted.lock().unwrap().known.push_back(Err(error()));
    store.scripted.lock().unwrap().by_hash.push_back(Err(error()));
    let man = IsManager::new(
        store,
        Arc::new(|| IsGate::new(true, false, false)),
        Arc::new(MockClock::new()),
    );
    assert!(man.already_have(&[1; 32]).is_err());
    assert!(man.get_instant_send_lock_by_hash(&[1; 32]).is_err());
}

/// The read scheme a lock was queued with travels with it through the received queue, the no-tx
/// queue and back (Core's lazy signature keeps `bufLegacyScheme`, `bls/bls.h:411-414`,
/// `:481-484`).
#[test]
fn the_read_scheme_travels_with_the_lock() {
    let w = world();
    let tx = spending(&[OutPoint::new(txid(9), 0)], 0);
    let (hash, parked) = lock_for(&tx);
    w.man.add_pending_is_lock(hash, parked, 3, true);
    assert!(w.man.attach_is_lock_to_tx(&tx).is_some());
    let (other, other_lock) = tagged(10);
    w.man.enqueue_instant_send_lock(4, other, other_lock, false);
    let fetched = w.man.fetch_pending_locks().pending_is;
    let mut reads: Vec<_> = fetched.iter().map(|e| (e.islock_hash, e.legacy)).collect();
    reads.sort_by_key(|(h, _)| *h);
    let mut expected = vec![(hash, true), (other, false)];
    expected.sort_by_key(|(h, _)| *h);
    assert_eq!(reads, expected);
}
