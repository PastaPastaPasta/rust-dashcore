//! Core's DMN tip as V keeps it (CH7 D16): `CDeterministicMNManager::tipIndex`, the block whose
//! list `GetListAtChainTip()` answers. The synchronous tip dispatchers move it
//! ([`ValidationSignals::sync_updated_block_tip`], `initialize_current_block_tip`); with a mempool,
//! [`InHold::reorg_drain`] moves it before an adding drain. V publishes it in `ChainView`.

use std::sync::Arc;

use dash_consensus::ctx::bls_ctx_for_block;
use dash_consensus::{BlockId, ChainAncestry as _, TxRef};
use dash_dml::{MnList, StoreError};

use super::abc::{HeldPeriodic, InHold};
use super::mempool_turn::{HeldMempool, MempoolTurn, ReorgCx, ReorgTurn};
use super::seams::{DisconnectPool, EvoReadCx, EvoReads, Fatal};
use super::signals::{Staged, ValidationSignals};

/// The list at `at`, or Core's empty list while `at` is null: `GetListAtChainTip()` at the DMN
/// tip, and `GetListForBlock` at the chain tip.
// Core: evo/deterministicmns.cpp:941-947 at v24.0.0
pub(crate) fn list_at_or_empty(
    reads: &dyn EvoReads,
    cx: EvoReadCx<'_>,
    at: Option<BlockId>,
) -> Result<Arc<MnList>, StoreError> {
    match at {
        Some(at) => reads.list_at(cx, at),
        None => Ok(Arc::new(MnList::default_pre_dip3())),
    }
}

/// [`list_at_or_empty`] for V's own use: a store fault is `AbortNode`.
pub(crate) fn dmn_list(
    reads: &dyn EvoReads,
    cx: EvoReadCx<'_>,
    dmn_tip: Option<BlockId>,
) -> Result<Arc<MnList>, Fatal> {
    list_at_or_empty(reads, cx, dmn_tip)
        .map_err(|e| crate::evo::node::store_fault("GetListAtChainTip", &e))
}

/// Move the DMN tip before an adding drain and resolve its list exactly once. A non-adding
/// drain reads no list, as Core's `removeForReorg` does not.
// Core: v24.0.0 validation.cpp:425-456, evo/specialtxman.cpp:855
pub(super) fn drain_list(
    signals: &ValidationSignals,
    reads: &dyn EvoReads,
    cx: EvoReadCx<'_>,
    tip: Option<BlockId>,
    add: bool,
) -> Result<Option<Arc<MnList>>, Fatal> {
    if !add {
        return Ok(None);
    }
    if let Some(tip) = tip {
        signals.move_dmn_tip(tip);
    }
    dmn_list(reads, cx, signals.dmn_tip()).map(Some)
}

/// The adding/non-adding contract is encoded solely by `tip_list`.
pub(crate) fn reorg_drain(
    held: &mut dyn HeldMempool,
    pool: Box<dyn DisconnectPool>,
    tip_list: Option<&Arc<MnList>>,
    cx: &mut dyn ReorgCx,
    staged: &mut Staged,
) -> Result<(), Fatal> {
    held.maybe_update_for_reorg_at(pool, tip_list, cx, staged)
}

impl InHold<'_, '_> {
    /// Core `MaybeUpdateMempoolForReorg` (`validation.cpp:425-525` at v24.0.0), with the held
    /// chain tip and ancestry bound to this V turn. An adding drain lends the pool a
    /// [`MempoolTurn`] at the new tip for its re-add: the list at the moved DMN tip
    /// (`UpdatedMNListTip`, `:433-435`), `GetCreditPool` at the tip, the tip's BLS scheme, and
    /// the hold's PERIODIC flush that each re-add's `AcceptToMemoryPool` ends with
    /// (`validation.cpp:1757-1759` at v24.0.2).
    ///
    /// # Errors
    ///
    /// Returns the pool's coin or store fault, or a credit-pool read fault. The caller discards
    /// staged signals and releases the held pool when this fails.
    pub(super) fn reorg_drain(
        &mut self,
        pool: Box<dyn DisconnectPool>,
        add: bool,
    ) -> Result<(), Fatal> {
        if !self.mempool.has_mempool() {
            return Ok(());
        }
        let tip = self.tip();
        let ancestry = self.chain().complete_ancestry().expect("V has complete ancestry");
        let best_header = self.chain().best_header();
        let ibd = add && self.is_ibd();
        let deployments = self.deployments.view();
        let read_cx = EvoReadCx {
            reader: self.reader,
            deployments: &deployments,
        };
        let list = drain_list(self.signals, self.connector.evo_reads(), read_cx, tip, add)?;
        let held = self.held_pool.as_deref_mut().expect("live pool is held");
        let Some(mn_list) = list else {
            let mut cx = ReorgTurn {
                tip,
                ancestry,
                connector: self.connector,
            };
            return reorg_drain(held, pool, None, &mut cx, &mut self.staged);
        };
        // Core: evo/creditpool.cpp:256 at v24.0.0
        let credit_pool = match tip {
            Some(tip) => self
                .connector
                .evo_reads()
                .credit_pool_after(read_cx, tip)
                .map_err(|e| crate::evo::node::store_fault("GetCreditPool", &e))?,
            None => None,
        };
        let next_height = tip.map_or(0, |tip| ancestry.header(tip).height + 1);
        let bls = self
            .connector
            .published_bls()
            .unwrap_or_else(|| bls_ctx_for_block(self.params, next_height));
        let mut periodic = HeldPeriodic {
            hold: &mut self.hold,
            idx: &mut *self.idx,
            have_pruned: &mut *self.have_pruned,
            flush: &mut *self.flush,
            reader: self.reader,
            clock: self.clock,
            loading: self.loading,
        };
        let mut turn = MempoolTurn {
            reorg: ReorgTurn {
                tip,
                ancestry,
                connector: self.connector,
            },
            reader: self.reader,
            deployments: &deployments,
            params: self.params,
            mn_list: mn_list.clone(),
            credit_pool,
            bls,
            best_header,
            ibd,
            clock: self.clock,
            signals: self.signals,
            hold_flush: Some(&mut periodic),
        };
        reorg_drain(held, pool, Some(&mn_list), &mut turn, &mut self.staged)
    }

    /// Core `removeForBlock` (`txmempool.cpp:1321-1350` at v24.0.0); the DMN-list lookups it
    /// performs are at `txmempool.cpp:1069,1250` at v24.0.0.
    ///
    /// # Errors
    ///
    /// Returns a deterministic-masternode-list read fault.
    pub(super) fn remove_for_block(&mut self, vtx: &[TxRef], height: u32) -> Result<(), Fatal> {
        if !self.mempool.has_mempool() {
            return Ok(());
        }
        let deployments = self.deployments.view();
        let list = dmn_list(
            self.connector.evo_reads(),
            EvoReadCx {
                reader: self.reader,
                deployments: &deployments,
            },
            self.signals.dmn_tip(),
        )?;
        self.held_pool.as_deref_mut().expect("live pool is held").remove_for_block_at(
            vtx,
            height,
            &list,
            &mut self.staged,
        );
        Ok(())
    }
}
