//! [`MempoolSink`] and [`HeldMempool`] on the real pool: V's calls from block connect, disconnect
//! and reorg, each inside a hold that owns the pool's write guard (`m_mempool->cs` held across
//! ABC's inner loop). Every mutator moves its removals into the hold's stream as they occur.

use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;

use dash_consensus::TxRef;
use dash_dml::MnList;
use dash_params::ChainSetup;
use dashd_chainstate::chain::{
    DisconnectPool, Fatal, HeldMempool, MempoolSink, RemovalReason, ReorgCx, Staged,
};
use dashd_chainstate::reorg::DisconnectedBlockTransactions;
use parking_lot::RawRwLock;
use parking_lot::lock_api::ArcRwLockWriteGuard;

use crate::accept::AcceptChain;
use crate::events::{self, Collect, pool_reason};
use crate::pool::{Mempool, PoolState};
use crate::reorg::{Readded, finish_reorg, readd_disconnected};

/// The chainstate's mempool: `pool` with the chain parameters its reorg filter reads.
pub struct PoolSink {
    pool: Arc<Mempool>,
    params: Arc<ChainSetup>,
}

impl PoolSink {
    pub fn new(pool: Arc<Mempool>, params: Arc<ChainSetup>) -> PoolSink {
        PoolSink {
            pool,
            params,
        }
    }
}

impl MempoolSink for PoolSink {
    fn begin_hold(&mut self) -> Option<Box<dyn HeldMempool>> {
        Some(Box::new(HeldPool {
            guard: self.pool.state_lock().write_arc(),
            pool: self.pool.clone(),
            params: self.params.clone(),
        }))
    }

    /// `DynamicMemoryUsage()` under a short read guard, which V takes before the coins cell and
    /// releases before entering it (§3).
    // Core: txmempool.cpp:1984-1988 at v24.0.0
    fn dynamic_memory_usage(&self) -> u64 {
        self.pool.read().dynamic_memory_usage() as u64
    }

    // Core: validation.cpp:3319, kernel/disconnected_transactions.h:18 at v24.0.0
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(DisconnectedBlockTransactions::default())
    }
}

/// A hold's pool: the write guard, released when V drops the hold after its splice.
struct HeldPool {
    guard: ArcRwLockWriteGuard<RawRwLock, PoolState>,
    pool: Arc<Mempool>,
    params: Arc<ChainSetup>,
}

impl Drop for HeldPool {
    fn drop(&mut self) {
        self.pool.publish_usage(&self.guard);
    }
}

impl HeldMempool for HeldPool {
    fn dynamic_memory_usage(&self) -> u64 {
        self.guard.dynamic_memory_usage() as u64
    }

    /// `removeForBlock` with the list at V's tip (CH7 §11.2). The fee estimator's
    /// `processBlock` (MP2) and the rolling-fee fields (MP1b) are not here, so `height` is
    /// unread.
    // Core: txmempool.cpp:1321-1353 at v24.0.0
    fn remove_for_block_at(
        &mut self,
        vtx: &[TxRef],
        _height: u32,
        tip_list: &Arc<MnList>,
        staged: &mut Staged,
    ) {
        self.guard.remove_for_block(vtx, tip_list);
        events::stage(&mut self.guard, staged);
    }

    // Core: txmempool.cpp:1355-1369 at v24.0.0
    fn remove_expired_asset_unlock(&mut self, height: u32, staged: &mut Staged) {
        self.guard.remove_expired_asset_unlock(height);
        events::stage(&mut self.guard, staged);
    }

    // Core: txmempool.cpp:907-935 at v24.0.0
    fn remove_recursive(&mut self, tx: &TxRef, reason: RemovalReason, staged: &mut Staged) {
        self.guard.remove_recursive(tx, pool_reason(reason));
        events::stage(&mut self.guard, staged);
    }

    // Core: txmempool.cpp:422-425 at v24.0.0
    fn add_transactions_updated(&mut self, n: u32, _: &mut Staged) {
        self.guard.add_transactions_updated(n);
    }

    /// `-checkmempool`'s sampled `check`: one call in `m_check_ratio`. The coins half
    /// (`CheckTxInputs` over `CoinsTip()` at `next_height`) needs a coins view the seam does not
    /// pass, so only the pool's own invariants run here.
    // Core: txmempool.cpp:1371-1380 at v24.0.0
    fn check(&mut self, _next_height: u32, _: &mut Staged) {
        if !sampled(self.guard.options().check_ratio) {
            return;
        }
        tracing::debug!(
            target: "mempool",
            "Checking mempool with {} transactions and {} inputs",
            self.guard.size(),
            self.guard.next_tx().len()
        );
        self.guard.check();
    }

    /// `MaybeUpdateMempoolForReorg`. With `tip_list` (`fAddToMempool`), each disconnected
    /// transaction goes back through `AcceptToMemoryPool` in `cx`'s admission turn at the new tip,
    /// whose list is `tip_list` (D16, `UpdatedMNListTip`), with the IS accept hooks in force;
    /// without it, each is removed with its descendants. Then `UpdateTransactionsFromBlock`,
    /// `removeForReorg` with `filter_final_and_mature` over `cx`, and `LimitMempoolSize`
    /// ([`crate::reorg`]). Every event reaches `staged` in the order it occurred.
    ///
    /// # Errors
    ///
    /// A re-add or filter fault, or an adding drain whose `cx` lends no turn. The pool is left
    /// consistent: what was re-added or removed before the fault stays, its descendants' state
    /// fixed, and `removeForReorg` applies nothing. V discards `staged` and stops, so those events
    /// are queued straight to the pool's queue rather than dropped. Nothing may dump or persist
    /// the pool after this fault.
    // Core: validation.cpp:425-525 at v24.0.0
    fn maybe_update_for_reorg_at(
        &mut self,
        mut pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<MnList>>,
        cx: &mut dyn ReorgCx,
        staged: &mut Staged,
    ) -> Result<(), Fatal> {
        let queued = pool.take();
        let mut events = Collect::default();
        let readded = match tip_list {
            None => readd_disconnected(&mut self.guard, &queued, None, &mut events),
            Some(tip_list) => {
                let mut readded = None;
                cx.with_turn(&mut |turn| {
                    debug_assert!(Arc::ptr_eq(turn.mn_list(), tip_list), "the turn reads tip_list");
                    let now = turn.clock().node_now().secs();
                    let add: (&mut dyn AcceptChain, i64) = (turn, now);
                    readded =
                        Some(readd_disconnected(&mut self.guard, &queued, Some(add), &mut events));
                });
                readded.unwrap_or_else(|| Readded {
                    update: Vec::new(),
                    fault: Some(Fatal::assert("an adding reorg drain without its admission turn")),
                })
            }
        };
        let result = finish_reorg(&mut self.guard, &self.params, readded, cx, &mut events);
        events.release(result.is_ok(), staged, self.pool.signals());
        result
    }
}

/// Core's `GetRand(m_check_ratio) >= 1` skip, inverted: true once in `ratio` calls.
pub(crate) fn sampled(ratio: i32) -> bool {
    ratio > 0 && RandomState::new().hash_one(()).is_multiple_of(ratio as u64)
}

#[cfg(test)]
#[path = "sink_tests.rs"]
mod tests;
