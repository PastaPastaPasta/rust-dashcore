//! Read access within a single mempool lock hold, and the handle the node's consumers hold.

use std::cmp::Ordering;

use dash_consensus::{Amount, Tx, TxRef};
use dash_dml::MnList;
use dash_primitives::{OutPoint, TxHash};
use dashd_chainstate::chain::{ActorReply, WaitError};

use crate::types::MempoolAcceptResult;

/// A borrowed pool view, already read-locked by [`IsMempool::with_read`](crate::is::IsMempool::with_read).
/// Callers must not re-enter the pool, wait for V, or hold its lock across an await.
pub trait MempoolRead {
    /// Returns a shared transaction (`src/txmempool.cpp`, `get`, Dash Core v24.0.0).
    fn get(&self, txid: &TxHash) -> Option<TxRef>;

    fn exists(&self, txid: &TxHash) -> bool {
        self.get(txid).is_some()
    }

    /// Returns the `mapNextTx` spender (`src/txmempool.cpp`, `GetConflictTx`, Dash Core v24.0.0).
    fn spender_of(&self, outpoint: &OutPoint) -> Option<TxRef>;

    /// Returns withdrawals claiming an index (`src/txmempool.cpp`, `GetAssetUnlockTxidsByIndex`, v24.0.0).
    fn asset_unlock_txids_by_index(&self, index: u64) -> Vec<TxHash>;

    /// Returns CAmount capped at i64::MAX (Core v24.0.0 `src/txmempool.h`, `GetPendingAssetUnlockAmount`).
    /// Internal bookkeeping retains the exact total separately, using 256 bits or a proven bound.
    fn pending_asset_unlock_amount(&self) -> i64;

    /// Core v24.0.0 `GetServiceUpdatesBeforeKeyChange` (R6).
    fn service_updates_before_key_change(
        &self,
        key_change: &TxHash,
        tip_list: &MnList,
    ) -> Vec<TxHash>;

    /// Core v24.0.0 `IsUnorderableServiceUpdate` (R7).
    fn is_unorderable_service_update(
        &self,
        tx: &Tx,
        ancestors: &[TxHash],
        tip_list: &MnList,
    ) -> bool;

    /// Core v24.0.0 `GetKeyChangeTarget` (R8).
    fn key_change_target(&self, tx: &Tx, tip_list: &MnList) -> Option<dash_dml::ProTxHash>;
}

/// An entry's transaction and admission time, from Dash Core v24.0.0 `src/txmempool.h`,
/// `TxMempoolInfo` (`tx`, `m_time`).
#[derive(Clone)]
pub struct TxMempoolInfo {
    /// The entry's current transaction instance.
    pub tx: TxRef,
    /// The entry's admission time in seconds (`CTxMemPoolEntry::GetTime`).
    pub time_s: i64,
}

impl std::fmt::Debug for TxMempoolInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxMempoolInfo")
            .field("txid", &self.tx.txid())
            .field("time_s", &self.time_s)
            .finish()
    }
}

/// The relay reads of a pool view, already read-locked by [`MempoolHandle::with_relay_read`].
/// Hash arguments and results are stable txids (`GetHash`) unless named `instance_hash`. Txid
/// order is Core's `uint256` order, which is [`TxHash`]'s `Ord` (byte-wise on the stored bytes).
/// The same rules as [`MempoolRead`] apply: no re-entry, no wait on V, no await under the lock.
pub trait RelayRead: MempoolRead {
    /// The entry's transaction and admission time, or `None` if `txid` is absent.
    /// `time_s` is the original admission time, kept when an asset unlock's instance is replaced
    /// in place (`ReplaceAssetUnlockInstance`, `src/txmempool.cpp:860-875`).
    /// Core v24.0.0 `src/txmempool.cpp:1572-1579`, `info`.
    fn info(&self, txid: &TxHash) -> Option<TxMempoolInfo>;

    /// Every entry, in `GetSortedDepthAndScore` order: ascending ancestor count (the entry
    /// included), then descending score, then descending txid. Score compares
    /// `(double)fee_a * size_b` with `(double)fee_b * size_a` on the original fee, never the
    /// prioritisation delta (`CompareTxMemPoolEntryByScore`, `src/txmempool.h:131-143`).
    /// Core v24.0.0 `src/txmempool.cpp:1515-1561`, `infoAll`.
    fn info_all(&self) -> Vec<TxMempoolInfo>;

    /// `Less` when `a` is considered sooner than `b`: an absent `a` before a present `b`; then
    /// [`RelayRead::info_all`]'s order. Two absent txids are `Equal`, as are an entry and itself.
    /// Core v24.0.0 `src/txmempool.cpp:1495-1513`, `CompareDepthAndScore`, and
    /// `src/txmempool.h:131-143`.
    fn compare_depth_and_score(&self, a: &TxHash, b: &TxHash) -> Ordering;

    /// The direct in-pool parents of `txid` whose admission time is strictly greater than
    /// `since_s`, in `GetMemPoolParentsConst` order (ascending txid); empty if `txid` is absent.
    /// Core v24.0.0 `src/net_processing.cpp:2996-3010`, `ProcessGetData`.
    fn parents_entered_after(&self, txid: &TxHash, since_s: i64) -> Vec<TxHash>;

    /// The stable txid of the pool's version-2+ asset unlock whose current instance hash is
    /// `instance_hash`; `None` for any other transaction or an absent one.
    /// Core v24.0.0 `src/txmempool.cpp:852-858`, `GetAssetUnlockByInstanceHash`.
    fn asset_unlock_by_instance_hash(&self, instance_hash: &TxHash) -> Option<TxHash>;

    /// The unbroadcast txids in ascending order.
    /// Core v24.0.0 `src/txmempool.h:785-790`, `GetUnbroadcastTxs`.
    fn unbroadcast(&self) -> Vec<TxHash>;

    /// The number of entries (Core v24.0.0 `src/txmempool.h:702-706`, `size`).
    fn size(&self) -> usize;

    /// The pool's dynamic memory usage in bytes.
    /// Core v24.0.0 `src/txmempool.cpp:1984-1988`, `DynamicMemoryUsage`.
    fn dynamic_memory_usage(&self) -> usize;

    /// Calls `f(instance_hash, tx)` over `vTxHashes` until it returns `false`: insertion order
    /// with swap-remove on removal (`src/txmempool.cpp:475-476, 726-733`), an asset unlock v2
    /// rekeyed in place when its instance is replaced (`:871`). The order decides which of two
    /// short-ID-equal transactions a compact block reconstruction takes.
    /// Core v24.0.0 `src/blockencodings.cpp:112-139`, `InitData`.
    fn for_each_in_vtxhashes_order(&self, f: &mut dyn FnMut(&TxHash, &TxRef) -> bool);
}

/// The node's mempool as its consumers hold it: P2P attach, RPC and the wallet take an
/// `Arc<dyn MempoolHandle>`.
///
/// Admissions run Core's `ChainstateManager::ProcessTransaction` (`src/validation.cpp:4920-4932`,
/// Dash Core v24.0.0) one at a time in submission order, and publish their pool mutations and
/// TransactionAdded/Removed events before the reply resolves. Missing inputs are
/// [`MempoolAcceptResultType::Invalid`](crate::types::MempoolAcceptResultType::Invalid) with
/// [`TxValidationResult::MissingInputs`](dash_consensus::TxValidationResult::MissingInputs);
/// callers derive the parents from all inputs. A reply refuses a task marked no-wait (LO-9)
/// with [`WaitError::WaitOnVFromNoWaitTask`]. A pool with no admission worker replies
/// [`WaitError::Disconnected`] without reaching V: one not attached yet (the node attaches it
/// once V's evo state is), or one whose worker shutdown stopped with V.
///
/// The other methods take only the pool lock, once, never V: call them without holding a lock
/// the pool may wait for, and never from inside [`MempoolHandle::with_relay_read`].
pub trait MempoolHandle: Send + Sync {
    /// Submits a relayed transaction: `ProcessTransaction(tx, test_accept = false,
    /// bypass_limits = false)`, as `PeerManagerImpl` calls it for `TX` and orphans
    /// (`src/net_processing.cpp:4959, 3400`). `from` is
    /// the source peer's id, `None` for none (an orphan retried from a block); it never changes
    /// the result.
    fn accept_async(&self, tx: TxRef, from: Option<u64>) -> ActorReply<MempoolAcceptResult>;

    /// Submits a local transaction and blocks for the result: `ProcessTransaction(tx,
    /// test_accept = false, bypass_limits)`, as `BroadcastTransaction` calls it
    /// (`src/node/transaction.cpp:83`).
    ///
    /// # Errors
    ///
    /// The reply's [`WaitError`].
    fn accept(&self, tx: TxRef, bypass_limits: bool) -> Result<MempoolAcceptResult, WaitError>;

    /// Checks a transaction without admitting it and blocks for the result:
    /// `ProcessTransaction(tx, test_accept = true, bypass_limits)`, as `testmempoolaccept`
    /// (`src/rpc/mempool.cpp:178`) and `BroadcastTransaction`'s fee check
    /// (`src/node/transaction.cpp:75`) call it. The pool is unchanged and no event is raised.
    ///
    /// # Errors
    ///
    /// The reply's [`WaitError`].
    fn test_accept(&self, tx: TxRef, bypass_limits: bool)
    -> Result<MempoolAcceptResult, WaitError>;

    /// Adds `fee_delta` to `txid`'s delta, saturating, whether or not `txid` is in the pool, and
    /// updates the modified fees of a present entry's ancestors and descendants.
    /// Core v24.0.0 `src/txmempool.cpp:1878-1903`, `PrioritiseTransaction`.
    ///
    /// Never call it from V (a V turn, a hold, or anything V waits on): it waits behind the
    /// queued admissions, and they wait on V.
    fn prioritise(&self, txid: &TxHash, fee_delta: Amount);

    /// [`MempoolHandle::prioritise`], posted: queued behind earlier admissions and ahead of later
    /// ones, it returns at once and applies in its turn, or at once without an admission worker.
    /// It never waits on V, so a caller that must not wait (LO-9) may post it. A delta posted
    /// before a transaction's [`MempoolHandle::accept_async`] applies before that admission, as
    /// `ValidateDSTX`'s `PrioritiseTransaction` precedes `ProcessTransaction`.
    /// Core v24.0.0 `src/net_processing.cpp:3716`, `:4959`.
    fn post_prioritise(&self, txid: &TxHash, fee_delta: Amount);

    /// Inserts `txid` into the unbroadcast set if it is in the pool; otherwise does nothing.
    /// Core v24.0.0 `src/txmempool.h:773-780`, `AddUnbroadcastTx`.
    fn add_unbroadcast(&self, txid: &TxHash);

    /// Removes `txid` from the unbroadcast set; harmless if absent. `unchecked` marks a removal
    /// before relay was confirmed, which only changes Core's log line.
    /// Core v24.0.0 `src/txmempool.cpp:1990-1997`, `RemoveUnbroadcastTx`.
    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool);

    /// Runs `f` exactly once under one pool read lock, so the reads it makes are coherent with
    /// each other; an entry may disappear between two calls. Do not re-enter the pool, wait for
    /// V, or hold the lock across an await.
    fn with_relay_read(&self, f: &mut dyn FnMut(&dyn RelayRead));
}

impl dyn MempoolHandle + '_ {
    /// [`MempoolHandle::with_relay_read`] returning `f`'s value.
    ///
    /// # Panics
    ///
    /// If the implementation breaks `with_relay_read`'s run-once contract.
    pub fn read<R>(&self, f: impl FnOnce(&dyn RelayRead) -> R) -> R {
        let mut f = Some(f);
        let mut out = None;
        self.with_relay_read(&mut |read| {
            if let Some(f) = f.take() {
                out = Some(f(read));
            }
        });
        out.expect("with_relay_read runs its closure once")
    }
}
