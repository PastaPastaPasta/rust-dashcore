//! `getrawmempool`'s and `getmempoolinfo`'s reads of the node's pool: one pool read lock, off
//! V, as Core's `LOCK(pool.cs)` (PLAN-MP1a §3, a pool.cs-only site). `sendrawtransaction`'s
//! `BroadcastTransaction` over the pool's admission path and the peer manager's relay, and
//! `testmempoolaccept`'s and `prioritisetransaction`'s pool calls.

use std::sync::Arc;

use dash_consensus::{Tx, TxRef, TxValidationError};
use dash_primitives::TxHash;
use dashd_chainstate::chain::Blocking;
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};
use dashd_mempool::pool::{PoolEntry, PoolState};
use dashd_mempool::types::MempoolAcceptResultType;
use dashd_rpc::mempool::{
    AcceptedTx, MempoolEntryView, MempoolInfo, MempoolView, NoVerdict, Processed, TxBroadcast,
};

use super::{ChainSlot, NetSlot};
use crate::mempool::NodeMempool;

impl ChainSlot {
    /// `f` under one read lock of the node's pool, with its load-tried flag. The slot's own lock
    /// is released first. Before step 7 attaches the pool (and after shutdown detaches it) `f`
    /// reads a fresh, empty pool, not loaded, as Core's pool is before `LoadMempool`.
    // Core: txmempool.h:337, txmempool.cpp:2166-2170 at v24.0.0
    fn read_pool<R>(&self, f: impl FnOnce(bool, &PoolState) -> R) -> R {
        let mempool = self.mempool.lock().unwrap_or_else(|p| p.into_inner()).clone();
        match mempool {
            Some(mempool) => {
                let _order = LockOrderGuard::enter(LockRank::Mempool);
                let state = mempool.pool().read();
                f(mempool.load_tried(), &state)
            }
            None => f(false, &PoolState::default()),
        }
    }
}

/// `entryToJSON`'s reads of `e`.
// Core: rpc/mempool.cpp:268-313 at v24.0.0
fn entry_view(pool: &PoolState, e: &PoolEntry) -> MempoolEntryView {
    let txid = e.txid();
    let (meta, ancestors, descendants) = (e.metadata(), e.ancestors(), e.descendants());
    MempoolEntryView {
        txid,
        vsize: e.vsize(),
        time: meta.time,
        height: meta.height,
        descendant_count: descendants.count,
        descendant_size: descendants.size,
        ancestor_count: ancestors.count,
        ancestor_size: ancestors.size,
        fee: meta.fee,
        modified_fee: e.modified_fee(),
        ancestor_fees: ancestors.modified_fees,
        descendant_fees: descendants.modified_fees,
        depends: e
            .tx()
            .inputs()
            .iter()
            .map(|txin| txin.prevout().txid())
            .filter(|parent| pool.exists(parent))
            .collect(),
        spent_by: e.children().iter().copied().collect(),
        unbroadcast: pool.is_unbroadcast_tx(&txid),
    }
}

// Core: rpc/mempool.cpp:315-352, :657-677 at v24.0.0
impl MempoolView for ChainSlot {
    fn txids_and_sequence(&self) -> (Vec<TxHash>, u64) {
        self.read_pool(|_, pool| (pool.entries().keys().copied().collect(), pool.sequence()))
    }

    fn entries(&self) -> Vec<MempoolEntryView> {
        self.read_pool(|_, pool| pool.entries().values().map(|e| entry_view(pool, e)).collect())
    }

    fn info(&self) -> MempoolInfo {
        self.read_pool(|loaded, pool| {
            let options = pool.options();
            MempoolInfo {
                loaded,
                size: pool.size() as u64,
                bytes: pool.total_vsize(),
                usage: pool.dynamic_memory_usage() as u64,
                total_fee: pool.total_fee(),
                max_mempool: options.max_size_bytes,
                min_fee_per_k: pool.get_min_fee(),
                min_relay_fee_per_k: options.min_relay_feerate,
                unbroadcast_count: pool.get_unbroadcast_txs().len() as u64,
                pending_asset_unlocks: pool.pending_asset_unlock_amount(),
            }
        })
    }
}

/// `mempool->get(txid)` is held, and `tx` is not a version-2 asset unlock re-signed from it.
// Core: v24.0.2 src/node/transaction.cpp:57-62
fn holds_instance(tx: &Tx, held: Option<&Tx>) -> bool {
    held.is_some_and(|held| {
        !(tx.is_asset_unlock_with_stable_txid() && held.instance_hash() != tx.instance_hash())
    })
}

/// `BroadcastTransaction`'s node: the pool [`ChainSlot`] holds from step 7, V's signals queue
/// the pool raises its events on, and the peer manager [`NetSlot`] holds from step 12.
pub(super) struct NodeBroadcast {
    pub(super) chain: Arc<ChainSlot>,
    pub(super) net: Arc<NetSlot>,
}

impl NodeBroadcast {
    fn pool(&self) -> Option<Arc<NodeMempool>> {
        self.chain.mempool.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

// Core: v24.0.2 src/node/transaction.cpp:33-125
impl TxBroadcast for NodeBroadcast {
    fn has_mempool(&self) -> bool {
        self.pool().is_some()
    }

    fn has_peerman(&self) -> bool {
        self.net.shared().is_some()
    }

    fn in_mempool(&self, tx: &Tx) -> bool {
        let Some(pool) = self.pool() else {
            return false;
        };
        holds_instance(tx, pool.handle().read(|r| r.get(&tx.txid())).as_deref())
    }

    /// The pool's admission path, MP1a-11's: its reply comes once the admission's events are on
    /// V's signals queue. The slot holds the pool only while its admission worker runs (attached
    /// after the load that attaches it, detached at shutdown before it stops), so a dropped reply
    /// is V's `Fatal` or `AbortNode`.
    fn process_transaction(
        &self,
        tx: &TxRef,
        test_accept: bool,
        bypass_limits: bool,
    ) -> Result<Processed, NoVerdict> {
        let handle = self.pool().ok_or(NoVerdict::NoMempool)?.handle();
        let result = if test_accept {
            handle.test_accept(Arc::clone(tx), bypass_limits)
        } else {
            handle.accept(Arc::clone(tx), bypass_limits)
        }
        .map_err(|_| NoVerdict::Aborted)?;
        Ok(match result.result_type {
            MempoolAcceptResultType::Valid => Processed::Valid(AcceptedTx {
                vsize: result.vsize,
                base_fee: result.base_fee,
                effective_feerate: result.effective_feerate,
                effective_includes: result.txids_fee_calculations,
            }),
            MempoolAcceptResultType::Invalid => Processed::Invalid(
                TxValidationError::new(result.validation_result, result.reason)
                    .with_debug(result.debug_message),
            ),
            MempoolAcceptResultType::MempoolEntry => Processed::MempoolEntry,
        })
    }

    // Core: v24.0.2 src/txmempool.cpp:1878-1903
    fn prioritise(&self, txid: &TxHash, fee_delta: i64) -> bool {
        let Some(pool) = self.pool() else {
            return false;
        };
        pool.handle().prioritise(txid, fee_delta);
        true
    }

    fn add_unbroadcast(&self, txid: &TxHash) {
        if let Some(pool) = self.pool() {
            pool.handle().add_unbroadcast(txid);
        }
    }

    /// `CallFunctionInValidationInterfaceQueue` and its wait: a barrier on V's signals queue,
    /// behind the admission's `TransactionAddedToMempool`. A stopped signals thread has nothing
    /// left to run.
    // Core: v24.0.2 src/node/transaction.cpp:99-118
    fn sync_with_validation_queue(&self) {
        if let Some(pool) = self.pool() {
            let _ = pool.pool().signals().sync(&mut Blocking);
        }
    }

    fn relay_transaction(&self, txid: &TxHash) {
        if let Some(shared) = self.net.shared() {
            shared.relay_transaction(txid);
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::primitives::{OutPoint, Transaction, TxIn, TxOut, TxType};

    use super::*;

    fn tx(version: i16, tx_type: TxType, inputs: Vec<TxIn>, payload: Vec<u8>) -> Tx {
        let outputs = vec![TxOut::from_duffs(10_000, vec![0x51].into())];
        Tx::new(Transaction::new(version, tx_type, inputs, outputs, 0, payload))
    }

    /// An asset unlock of payload `version` (2 has a stable txid), signed with `sig`.
    fn unlock(version: u8, sig: u8) -> Tx {
        let mut payload = vec![version];
        payload.extend(7u64.to_le_bytes());
        payload.extend(1_000u32.to_le_bytes());
        payload.extend(150u32.to_le_bytes());
        payload.extend([3; 32]);
        payload.extend([sig; 96]);
        tx(3, TxType::AssetUnlock, vec![], payload)
    }

    /// A pooled txid is held, unless the pooled entry is another instance of a version-2 asset
    /// unlock: that resend reaches the pool, which refreshes the entry in place.
    // Core: v24.0.2 src/node/transaction.cpp:57-69
    #[test]
    fn a_resigned_stable_unlock_is_not_held() {
        let spend = tx(
            2,
            TxType::Spend,
            vec![TxIn::new(OutPoint::new(TxHash::from([7; 32]), 0), vec![].into(), u32::MAX)],
            vec![],
        );
        assert!(holds_instance(&spend, Some(&spend)));
        assert!(!holds_instance(&spend, None));

        let (held, resigned) = (unlock(2, 1), unlock(2, 2));
        assert!(held.is_asset_unlock_with_stable_txid());
        assert_eq!(held.txid(), resigned.txid());
        assert_ne!(held.instance_hash(), resigned.instance_hash());
        assert!(holds_instance(&held, Some(&held)), "the same instance is only relayed");
        assert!(!holds_instance(&resigned, Some(&held)), "another instance reaches the pool");

        let (v1, v1_resigned) = (unlock(1, 1), unlock(1, 2));
        assert!(!v1.is_asset_unlock_with_stable_txid());
        assert!(holds_instance(&v1, Some(&v1_resigned)), "a version-1 unlock has no exception");
    }
}
