//! [`PoolsOnly`] (test support): a mempool of Core's disconnect pools alone, so that a node's
//! reorgs fill and evict them as a node with a mempool's do (CH3b b2a, DEC-500), and a log of the
//! calls V makes on them with what each pool gave back (CH3 §4.3, §7.6, b3).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use dash_consensus::TxRef;
use dash_dml::MnList;
use dash_primitives::TxHash;

use super::DisconnectedBlockTransactions;
use super::memusage::recursive_dynamic_usage;
use crate::chain::{DisconnectPool, MempoolSink, RemovalReason};

/// [`NullMempool`](crate::chain::NullMempool) but for its disconnect pools: each one is Core's
/// [`DisconnectedBlockTransactions`] at its 20,000,000-byte budget. What a pool evicts reaches
/// [`remove_recursive`](crate::chain::HeldMempool::remove_recursive), the only transactions the tip steps
/// pass it (`DisconnectTip`, `validation.cpp:3412-3418` at the pin), and is counted there in the
/// pool's own bytes. `MaybeUpdateMempoolForReorg` drains a pool with nothing to re-add it to.
///
/// Clones share the count and the log: a test keeps one while V owns another.
#[derive(Clone, Debug, Default)]
pub struct PoolsOnly {
    evicted: Arc<AtomicUsize>,
    calls: Arc<Mutex<Vec<PoolCall>>>,
}

/// A call V made on a [`PoolsOnly`]'s pools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PoolCall {
    /// `new_disconnect_pool`: Core's `DisconnectedBlockTransactions disconnectpool` comes into
    /// scope.
    Open,
    /// `remove_recursive(tx, reason)`: Core's `removeRecursive`, with a transaction a pool
    /// evicted.
    Remove(TxHash, RemovalReason),
    /// `maybe_update_for_reorg_at(pool, tip_list)`: Core's
    /// `MaybeUpdateMempoolForReorg(disconnectpool, fAddToMempool)`, `fAddToMempool` being whether
    /// a list came, with the txids the pool's `take()` gave it, front first.
    Update(bool, Vec<TxHash>),
}

impl PoolsOnly {
    /// The evicted transactions' `RecursiveDynamicUsage`, summed over every pool so far.
    pub fn evicted_bytes(&self) -> usize {
        self.evicted.load(Ordering::Relaxed)
    }

    /// The calls since the last take, in order.
    pub fn take_calls(&self) -> Vec<PoolCall> {
        std::mem::take(&mut self.calls.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn log(&self, call: PoolCall) {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call);
    }
}

impl MempoolSink for PoolsOnly {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        self.log(PoolCall::Open);
        Box::new(DisconnectedBlockTransactions::default())
    }

    fn begin_hold(&mut self) -> Option<Box<dyn crate::chain::HeldMempool>> {
        Some(Box::new(self.clone()))
    }
}

impl crate::chain::HeldMempool for PoolsOnly {
    fn dynamic_memory_usage(&self) -> u64 {
        0
    }

    fn maybe_update_for_reorg_at(
        &mut self,
        mut pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<MnList>>,
        _cx: &mut dyn crate::chain::ReorgCx,
        _staged: &mut crate::chain::Staged,
    ) -> Result<(), crate::chain::Fatal> {
        let taken = pool.take().iter().map(|tx| tx.txid()).collect();
        self.log(PoolCall::Update(tip_list.is_some(), taken));
        Ok(())
    }

    fn remove_for_block_at(
        &mut self,
        _: &[TxRef],
        _: u32,
        _: &Arc<MnList>,
        _staged: &mut crate::chain::Staged,
    ) {
    }

    fn remove_expired_asset_unlock(&mut self, _: u32, _staged: &mut crate::chain::Staged) {}

    fn remove_recursive(
        &mut self,
        tx: &TxRef,
        reason: RemovalReason,
        _staged: &mut crate::chain::Staged,
    ) {
        self.log(PoolCall::Remove(tx.txid(), reason));
        self.evicted.fetch_add(recursive_dynamic_usage(tx), Ordering::Relaxed);
    }

    fn add_transactions_updated(&mut self, _: u32, _staged: &mut crate::chain::Staged) {}

    fn check(&mut self, _: u32, _staged: &mut crate::chain::Staged) {}
}
