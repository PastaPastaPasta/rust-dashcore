//! Fallible InstantSend storage, effect and recovered-signature seams (LK1b-s S1.0b).
//!
//! The database implementation owns persistent rows and cache effects. Chain integration (S1b),
//! verification (S1c), listeners (S1d), and the effect-ledger adapter (S1f) consume these
//! interfaces without depending on that implementation's layout.

use std::collections::BTreeMap;
#[cfg(feature = "test-support")]
use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::Mutex;

use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, TxHash};
#[cfg(feature = "test-support")]
use dash_evo::locks::isdlock_hash;
use dash_kv::Result;
use dash_p2p_core::IsdLock;
use dash_params::LlmqType;
use dash_primitives::BlockHash;

use super::db::ChainedRemoval;
use super::{IsLockHash, LockedTx};

pub use super::db::IsOverlay;

/// An identifier for a staged chained removal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StageId(pub u64);

/// The fallible InstantSend store used by chain validation and verification.
pub trait IsStore: Send + Sync {
    /// The first lock in Core's `GetLockInputs(tx)` order whose txid differs from `tx.txid()`.
    /// Callers apply [`super::IsGate`] before querying, as Core's `GetConflictingLock` does.
    /// Core: `instantsend/instantsend.cpp:428-445`.
    fn conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>>;

    /// The attempt-overlay owner read. The S1a-2 overlay is opaque at this slice.
    fn conflicting_lock_in(
        &self,
        _overlay: &IsOverlay,
        tx: &Tx,
    ) -> Result<Option<(IsLockHash, LockedTx)>> {
        self.conflicting_lock(tx)
    }

    /// Look up a lock by its serialized hash, optionally consulting the recency cache.
    /// Core: `instantsend/db.cpp:265-284`.
    fn by_hash(&self, hash: &IsLockHash, use_cache: bool) -> Result<Option<LockedTx>>;

    /// Look up a lock by its transaction id.
    /// Core: `instantsend/db.cpp:286-303`.
    fn by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>>;

    /// Look up a lock through its input index.
    /// Core: `instantsend/db.cpp:305-316`.
    fn by_input(&self, input: &OutPoint) -> Result<Option<LockedTx>>;

    /// Whether a hash is known through a lock row, cache entry or archive marker.
    /// Core: `instantsend/db.cpp:235-240`.
    fn known(&self, hash: &IsLockHash) -> Result<bool>;

    /// Write a newly received lock and its indexes.
    /// Core: `instantsend/db.cpp:53-69`.
    fn write_new(&self, hash: &IsLockHash, lock: &LockedTx) -> Result<()>;

    /// Mark a lock mined at `height`.
    /// Core: `instantsend/db.cpp:91-103`.
    fn write_mined(&self, hash: &IsLockHash, height: i32) -> Result<()>;

    /// Add mined marks for transactions in a connected block.
    /// Core: `instantsend/db.cpp:200-216`.
    fn block_connected(&self, transactions: &[Tx], height: i32) -> Result<()>;

    /// Remove mined marks for transactions in a disconnected block.
    /// Core: `instantsend/db.cpp:218-233`.
    fn block_disconnected(&self, transactions: &[Tx], height: i32) -> Result<()>;

    /// Remove confirmed locks through `height`, returning retained lock objects.
    /// Core: `instantsend/db.cpp:118-164`.
    fn remove_confirmed(&self, height: i32) -> Result<BTreeMap<IsLockHash, LockedTx>>;

    /// Prune archived lock markers through `height`.
    /// Core: `instantsend/db.cpp:166-198`.
    fn remove_archived(&self, height: i32) -> Result<()>;

    /// Stage Core's chained-removal sequence for an attempt.
    fn stage_remove_chained(
        &self,
        overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)>;

    /// Commit a staged removal.
    fn commit_staged(&self, overlay: &mut IsOverlay, stage: StageId) -> Result<()>;

    /// Discard all staged removals in the overlay.
    fn discard_staged(&self, overlay: &mut IsOverlay);
}

/// One staged conflict removal, as the attempt's effect ledger observes it (PLAN-LK1b §5.3
/// item 2): pushed after a successful stage, before the effect is emitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IsObservation {
    /// The removed root lock's serialized hash.
    pub lock_hash: IsLockHash,
    /// The block transaction that overrides the lock (`fx`'s `is_rm` names it).
    pub overriding_txid: TxHash,
    /// The root lock's transaction.
    pub locked_txid: TxHash,
    /// The height the removed locks are archived at: the cached tip.
    pub archive_height: i32,
    /// Whether the root lock was stored when the removal read it.
    pub root_stored: bool,
    /// Every removed hash in Core's order, repeats included.
    pub removed: Vec<IsLockHash>,
    /// The overlay stage holding the removal's batch.
    pub stage: StageId,
}

/// The result handed to the StateDigest effect writer after an attempt's ledger is taken.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LedgerTake {
    /// All removal observations in Core effect order.
    pub observations: Vec<IsObservation>,
    /// Hashes confirmed by the ledger, in confirmation order.
    pub confirmed: Vec<IsLockHash>,
    /// The observation index at which confirmation failed, if any.
    pub failed_at: Option<usize>,
}

/// The effect ledger used by S1f's `FxSource` adapter.
pub trait IsFxLedger: Send {
    /// Add an InstantSend removal observation.
    fn push(&mut self, observation: IsObservation);

    /// Confirm the next pending observation against the store.
    fn confirm_next(
        &mut self,
        hash: &IsLockHash,
        store: &dyn IsStore,
    ) -> std::result::Result<(), dashd_chainstate::chain::Fatal>;

    /// The attempt-owned overlay used by staged store operations.
    fn overlay(&mut self) -> &mut IsOverlay;

    /// Take and reset observations and confirmation state; the attempt overlay remains owned by
    /// the ledger and is neither committed nor discarded by this method.
    fn take(&mut self) -> LedgerTake;

    /// Clear observations and confirmation state; callers explicitly commit or discard the
    /// overlay through [`IsStore`] before clearing the attempt.
    fn clear(&mut self);
}

/// The store reads the manager (S2) needs beyond [`IsStore`]: Core's
/// `GetInstantSendLockHashByTxid` and `GetInstantSendLockCount`.
pub trait IsManagerStore: IsStore {
    /// The lock hash indexed for `txid` (cache first), or the null hash when none is.
    /// Core: `instantsend/db.cpp:286-297`.
    fn hash_by_txid(&self, txid: &TxHash) -> Result<IsLockHash>;

    /// The number of stored lock rows, read without the caches.
    /// Core: `instantsend/db.cpp:242-263`.
    fn count(&self) -> Result<usize>;
}

/// The LK1c signer's queue, as NetInstantSend drives it (PLAN-LK1c §3.1, LQ4b 2026-11-14).
///
/// Each method enqueues one job on the signer's FIFO worker and returns at once: it never
/// blocks, never fails and takes no lock a waiting thread holds, so any thread may call it; jobs
/// run in call order.
pub trait IsSignerSeam: Send + Sync {
    /// ni:455-456 (mempool add: one tx, retroactive=false) and ni:555-556 (BlockConnected: every
    /// tx of the block that passed Core's gates there, in block order, retroactive=true).
    fn process_txs(&self, txs: Vec<Arc<Tx>>, retroactive: bool);
    /// ni:435-437.
    fn retry(&self, txs: Vec<Arc<Tx>>);
    /// ni:470, :649 and ni:365.
    fn clear_inputs(&self, ids: Vec<[u8; 32]>);
    fn clear_lock(&self, lock: Arc<IsdLock>);
}

/// No signer: Core's `m_signer == nullptr` on a node that is not a masternode.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSigner;

impl IsSignerSeam for NoSigner {
    fn process_txs(&self, _txs: Vec<Arc<Tx>>, _retroactive: bool) {}
    fn retry(&self, _txs: Vec<Arc<Tx>>) {}
    fn clear_inputs(&self, _ids: Vec<[u8; 32]>) {}
    fn clear_lock(&self, _lock: Arc<IsdLock>) {}
}

/// Core's `CMasternodeSync::IsBlockchainSynced()`, which NetInstantSend reads before tracking
/// mempool and block transactions. Core: `instantsend/net_instantsend.cpp:449`, `:544`.
pub trait SyncView: Send + Sync {
    fn is_blockchain_synced(&self) -> bool;
}

/// Core's `GetTransaction(nullptr, &mempool, txid, ..)` as `ProcessInstantSendLock` calls it:
/// the mempool first, then the txindex; a read failure is a miss. The block hash is `None` for a
/// mempool hit. Core: `instantsend/net_instantsend.cpp:369-371`, `node/transaction.cpp:126-156`.
pub trait TxLookup: Send + Sync {
    fn get_transaction(&self, txid: &TxHash) -> Option<(Arc<Tx>, Option<BlockHash>)>;
}

/// The recovered-signature cleanup seam owned by the LLMQ manager in a later slice.
pub trait RecoveredSigs: Send + Sync {
    /// Truncate recovered signatures for a signing request id.
    fn truncate(&self, llmq_type: LlmqType, request_id: &SignRequestId) -> Result<()>;
}

/// Core's `TruncateRecoveredSig` request id (`uint256`), not a quorum hash.
pub type SignRequestId = [u8; 32];

/// No-op recovered-signature store used until the LLMQ manager adapter lands.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRecSigs;

impl RecoveredSigs for NoRecSigs {
    fn truncate(&self, _llmq_type: LlmqType, _request_id: &SignRequestId) -> Result<()> {
        Ok(())
    }
}

/// Scripted answers for [`InMemoryIsStore`]. A queued error is returned like a storage error.
#[cfg(feature = "test-support")]
#[derive(Debug, Default)]
pub struct ScriptedIsStore {
    pub conflicting_lock: VecDeque<Result<Option<(IsLockHash, LockedTx)>>>,
    pub conflicting_lock_in: VecDeque<Result<Option<(IsLockHash, LockedTx)>>>,
    pub by_hash: VecDeque<Result<Option<LockedTx>>>,
    pub by_txid: VecDeque<Result<Option<LockedTx>>>,
    pub by_input: VecDeque<Result<Option<LockedTx>>>,
    pub known: VecDeque<Result<bool>>,
    pub write_new: VecDeque<Result<()>>,
    pub write_mined: VecDeque<Result<()>>,
    pub block_connected: VecDeque<Result<()>>,
    pub block_disconnected: VecDeque<Result<()>>,
    pub remove_confirmed: VecDeque<Result<BTreeMap<IsLockHash, LockedTx>>>,
    pub remove_archived: VecDeque<Result<()>>,
    pub stage_remove_chained: VecDeque<Result<(ChainedRemoval, StageId)>>,
    pub commit_staged: VecDeque<Result<()>>,
    pub next_stage_id: u64,
}

/// A call recorded by [`InMemoryIsStore`].
#[cfg(feature = "test-support")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IsStoreCall {
    ConflictingLock(TxHash),
    ConflictingLockIn(TxHash),
    ByHash {
        hash: IsLockHash,
        use_cache: bool,
    },
    ByTxid(TxHash),
    ByInput(OutPoint),
    Known(IsLockHash),
    WriteNew(IsLockHash),
    WriteMined(IsLockHash, i32),
    BlockConnected(i32, Vec<TxHash>),
    BlockDisconnected(i32, Vec<TxHash>),
    RemoveConfirmed(i32),
    RemoveArchived(i32),
    StageRemoveChained {
        hash: IsLockHash,
        root_txid: TxHash,
        archive_h: i32,
    },
    CommitStaged(StageId),
    DiscardStaged,
}

/// Scripted in-memory store for tests of the parallel LK1b slices.
#[cfg(feature = "test-support")]
#[derive(Debug, Default)]
pub struct InMemoryIsStore {
    /// Locks indexed by their Core wire hash.
    pub locks: BTreeMap<IsLockHash, LockedTx>,
    /// Conflict hash returned for a queried transaction id.
    pub conflicts: BTreeMap<TxHash, IsLockHash>,
    /// Known archive hashes without a retrievable lock object.
    pub archived: BTreeSet<IsLockHash>,
    /// Queued answers and injected failures. Scripted writes record calls and results but do not
    /// mutate [`InMemoryIsStore::locks`].
    pub scripted: Mutex<ScriptedIsStore>,
    /// Calls made by consumers, in order.
    pub call_log: Mutex<Vec<IsStoreCall>>,
}

#[cfg(feature = "test-support")]
impl InMemoryIsStore {
    /// Insert a lock using Core's wire hash helper.
    pub fn insert(&mut self, lock: LockedTx) -> IsLockHash {
        let hash = isdlock_hash(&lock);
        self.locks.insert(hash, lock);
        hash
    }

    /// Return a snapshot of the recorded calls.
    pub fn calls(&self) -> Vec<IsStoreCall> {
        self.call_log.lock().expect("call log mutex poisoned").clone()
    }

    fn record(&self, call: IsStoreCall) {
        self.call_log.lock().expect("call log mutex poisoned").push(call);
    }

    fn scripted<T>(
        queue: &mut VecDeque<Result<T>>,
        fallback: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        queue.pop_front().unwrap_or_else(fallback)
    }
}

#[cfg(feature = "test-support")]
impl IsStore for InMemoryIsStore {
    fn conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>> {
        self.record(IsStoreCall::ConflictingLock(tx.txid()));
        let mut scripted = self.scripted.lock().expect("script mutex poisoned");
        Self::scripted(&mut scripted.conflicting_lock, || {
            let Some(hash) = self.conflicts.get(&tx.txid()) else {
                return Ok(None);
            };
            Ok(self.locks.get(hash).cloned().map(|lock| (*hash, lock)))
        })
    }

    fn conflicting_lock_in(
        &self,
        _overlay: &IsOverlay,
        tx: &Tx,
    ) -> Result<Option<(IsLockHash, LockedTx)>> {
        self.record(IsStoreCall::ConflictingLockIn(tx.txid()));
        let mut scripted = self.scripted.lock().expect("script mutex poisoned");
        Self::scripted(&mut scripted.conflicting_lock_in, || {
            let Some(hash) = self.conflicts.get(&tx.txid()) else {
                return Ok(None);
            };
            Ok(self.locks.get(hash).cloned().map(|lock| (*hash, lock)))
        })
    }

    fn by_hash(&self, hash: &IsLockHash, use_cache: bool) -> Result<Option<LockedTx>> {
        self.record(IsStoreCall::ByHash {
            hash: *hash,
            use_cache,
        });
        Self::scripted(&mut self.scripted.lock().expect("script mutex poisoned").by_hash, || {
            Ok(self.locks.get(hash).cloned())
        })
    }

    fn by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>> {
        self.record(IsStoreCall::ByTxid(*txid));
        Self::scripted(&mut self.scripted.lock().expect("script mutex poisoned").by_txid, || {
            Ok(self.locks.values().find(|lock| lock.txid() == *txid).cloned())
        })
    }

    fn by_input(&self, input: &OutPoint) -> Result<Option<LockedTx>> {
        self.record(IsStoreCall::ByInput(*input));
        Self::scripted(&mut self.scripted.lock().expect("script mutex poisoned").by_input, || {
            let hash = self
                .locks
                .iter()
                .find_map(|(hash, lock)| lock.inputs().contains(input).then_some(hash));
            Ok(hash.and_then(|hash| self.locks.get(hash).cloned()))
        })
    }

    fn known(&self, hash: &IsLockHash) -> Result<bool> {
        self.record(IsStoreCall::Known(*hash));
        Self::scripted(&mut self.scripted.lock().expect("script mutex poisoned").known, || {
            Ok(self.locks.contains_key(hash) || self.archived.contains(hash))
        })
    }

    fn write_new(&self, hash: &IsLockHash, _lock: &LockedTx) -> Result<()> {
        self.record(IsStoreCall::WriteNew(*hash));
        Self::scripted(&mut self.scripted.lock().expect("script mutex poisoned").write_new, || {
            Ok(())
        })
    }

    fn write_mined(&self, hash: &IsLockHash, height: i32) -> Result<()> {
        self.record(IsStoreCall::WriteMined(*hash, height));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").write_mined,
            || Ok(()),
        )
    }

    fn block_connected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        self.record(IsStoreCall::BlockConnected(
            height,
            transactions.iter().map(Tx::txid).collect(),
        ));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").block_connected,
            || Ok(()),
        )
    }

    fn block_disconnected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        self.record(IsStoreCall::BlockDisconnected(
            height,
            transactions.iter().map(Tx::txid).collect(),
        ));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").block_disconnected,
            || Ok(()),
        )
    }

    fn remove_confirmed(&self, height: i32) -> Result<BTreeMap<IsLockHash, LockedTx>> {
        self.record(IsStoreCall::RemoveConfirmed(height));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").remove_confirmed,
            || Ok(BTreeMap::new()),
        )
    }

    fn remove_archived(&self, height: i32) -> Result<()> {
        self.record(IsStoreCall::RemoveArchived(height));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").remove_archived,
            || Ok(()),
        )
    }

    fn stage_remove_chained(
        &self,
        _overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)> {
        self.record(IsStoreCall::StageRemoveChained {
            hash: *hash,
            root_txid: root.txid(),
            archive_h,
        });
        let mut scripted = self.scripted.lock().expect("script mutex poisoned");
        if let Some(result) = scripted.stage_remove_chained.pop_front() {
            result
        } else {
            let stage = StageId(scripted.next_stage_id);
            scripted.next_stage_id += 1;
            Ok((
                ChainedRemoval {
                    hashes: vec![*hash],
                    root_present: self.locks.contains_key(hash),
                },
                stage,
            ))
        }
    }

    fn commit_staged(&self, _overlay: &mut IsOverlay, stage: StageId) -> Result<()> {
        self.record(IsStoreCall::CommitStaged(stage));
        Self::scripted(
            &mut self.scripted.lock().expect("script mutex poisoned").commit_staged,
            || Ok(()),
        )
    }

    fn discard_staged(&self, _overlay: &mut IsOverlay) {
        self.record(IsStoreCall::DiscardStaged);
    }
}

#[cfg(feature = "test-support")]
impl IsManagerStore for InMemoryIsStore {
    fn hash_by_txid(&self, txid: &TxHash) -> Result<IsLockHash> {
        Ok(self.locks.iter().find(|(_, lock)| lock.txid() == *txid).map_or([0; 32], |(h, _)| *h))
    }

    fn count(&self) -> Result<usize> {
        Ok(self.locks.len())
    }
}

/// Test-support exports kept behind the crate feature so consumer slices can share one double.
#[cfg(feature = "test-support")]
pub mod test_support {
    pub use super::{InMemoryIsStore, IsStoreCall, ScriptedIsStore};
}

#[cfg(feature = "test-support")]
#[cfg(test)]
mod tests {
    use super::*;
    use dash_consensus::primitives::{OutPoint, Transaction, TxIn, TxType};

    #[test]
    fn scripted_store_records_calls_and_errors() {
        let tx = Tx::new(Transaction::new(1, TxType::new(0), vec![], vec![], 0, vec![]));
        let store = InMemoryIsStore::default();
        store.scripted.lock().unwrap().known.push_back(Err(dash_kv::StoreError::engine(
            "test",
            dash_kv::ErrorKind::Io,
            "scripted",
        )));
        assert!(store.known(&[1; 32]).is_err());
        assert!(store.conflicting_lock(&tx).unwrap().is_none());
        assert_eq!(store.calls().len(), 2);
    }

    #[test]
    fn store_lookup_conflict_and_archive_answers_match_core_shape() {
        let tx = Tx::new(Transaction::new(1, TxType::new(0), vec![], vec![], 0, vec![]));
        let lock_txid = TxHash::from([1; 32]);
        let mut store = InMemoryIsStore::default();
        let lock = LockedTx::new(
            1,
            vec![OutPoint::new(TxHash::from([2; 32]), 0)],
            lock_txid,
            Default::default(),
            [0; 96].into(),
        );
        let hash = store.insert(lock.clone());
        assert_eq!(store.by_hash(&hash, true).unwrap(), Some(lock.clone()));
        assert_eq!(store.by_txid(&lock_txid).unwrap(), Some(lock.clone()));
        assert!(store.known(&hash).unwrap());
        store.conflicts.insert(tx.txid(), hash);
        assert_eq!(store.conflicting_lock(&tx).unwrap(), Some((hash, lock)));

        store.locks.remove(&hash);
        store.archived.insert(hash);
        assert!(store.by_hash(&hash, false).unwrap().is_none());
        assert!(store.known(&hash).unwrap());

        let other = Tx::new(Transaction::new(
            1,
            TxType::new(0),
            vec![TxIn::new(OutPoint::new(lock_txid, 0), Default::default(), u32::MAX)],
            vec![],
            0,
            vec![],
        ));
        assert!(store.conflicting_lock(&other).unwrap().is_none());
    }

    #[test]
    fn scripted_store_scripts_staged_errors_and_records_arguments() {
        let tx = Tx::new(Transaction::new(1, TxType::new(0), vec![], vec![], 0, vec![]));
        let root = LockedTx::new(
            1,
            vec![OutPoint::new(TxHash::from([8; 32]), 0)],
            TxHash::from([7; 32]),
            Default::default(),
            [0; 96].into(),
        );
        let hash = [6; 32];
        let store = InMemoryIsStore::default();
        let error = || dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "scripted");
        store.scripted.lock().unwrap().conflicting_lock_in.push_back(Err(error()));
        store.scripted.lock().unwrap().remove_confirmed.push_back(Err(error()));
        store.scripted.lock().unwrap().stage_remove_chained.push_back(Err(error()));
        store.scripted.lock().unwrap().commit_staged.push_back(Err(error()));

        assert!(store.conflicting_lock_in(&IsOverlay::default(), &tx).is_err());
        assert!(store.remove_confirmed(12).is_err());
        assert!(store.stage_remove_chained(&mut IsOverlay::default(), &hash, &root, 13).is_err());
        assert!(store.commit_staged(&mut IsOverlay::default(), StageId(9)).is_err());
        assert_eq!(
            store.calls(),
            vec![
                IsStoreCall::ConflictingLockIn(tx.txid()),
                IsStoreCall::RemoveConfirmed(12),
                IsStoreCall::StageRemoveChained {
                    hash,
                    root_txid: root.txid(),
                    archive_h: 13,
                },
                IsStoreCall::CommitStaged(StageId(9)),
            ]
        );
    }

    #[test]
    fn scripted_conflicting_lock_records_the_queried_txid() {
        let spend = |index| {
            Tx::new(Transaction::new(
                1,
                TxType::new(0),
                vec![TxIn::new(OutPoint::new(TxHash::from([5; 32]), index), Default::default(), 0)],
                vec![],
                0,
                vec![],
            ))
        };
        let (tx, other) = (spend(0), spend(1));
        assert_ne!(tx.txid(), other.txid());
        let store = InMemoryIsStore::default();
        assert!(store.conflicting_lock(&tx).unwrap().is_none());
        assert!(store.conflicting_lock(&other).unwrap().is_none());
        assert_eq!(
            store.calls(),
            vec![
                IsStoreCall::ConflictingLock(tx.txid()),
                IsStoreCall::ConflictingLock(other.txid())
            ]
        );
    }

    #[test]
    fn scripted_stage_ids_increment_and_writes_leave_locks_unchanged() {
        let root = LockedTx::new(
            1,
            vec![OutPoint::new(TxHash::from([8; 32]), 0)],
            TxHash::from([7; 32]),
            Default::default(),
            [0; 96].into(),
        );
        let hash = [6; 32];
        let mut store = InMemoryIsStore::default();
        let stored_hash = store.insert(root.clone());
        let before = store.locks.clone();
        let mut overlay = IsOverlay::default();
        assert_eq!(
            store.stage_remove_chained(&mut overlay, &hash, &root, 13).unwrap().1,
            StageId(0)
        );
        assert_eq!(
            store.stage_remove_chained(&mut overlay, &hash, &root, 13).unwrap().1,
            StageId(1)
        );
        store.write_new(&stored_hash, &root).unwrap();
        assert_eq!(store.locks, before);
    }

    #[test]
    fn scripted_store_records_transaction_slices_in_order() {
        let tx = |lock_time| {
            Tx::new(Transaction::new(1, TxType::new(0), vec![], vec![], lock_time, vec![]))
        };
        let (tx_a, tx_b) = (tx(1), tx(2));
        let store = InMemoryIsStore::default();
        store.block_connected(&[tx(1), tx(2)], 3).unwrap();
        store.block_disconnected(std::slice::from_ref(&tx_b), 3).unwrap();
        assert_eq!(
            store.calls(),
            vec![
                IsStoreCall::BlockConnected(3, vec![tx_a.txid(), tx_b.txid()]),
                IsStoreCall::BlockDisconnected(3, vec![tx_b.txid()]),
            ]
        );
    }
}
