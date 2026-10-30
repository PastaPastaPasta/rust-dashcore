//! Core's DMN tip as V keeps it (CH7 D16): `CDeterministicMNManager::tipIndex`, the block whose
//! list `GetListAtChainTip()` answers. The synchronous tip dispatchers move it
//! ([`ValidationSignals::sync_updated_block_tip`], `initialize_current_block_tip`); with a mempool,
//! [`reorg_drain`] moves it before an adding drain. V publishes it in `ChainView`.

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_dml::{MnList, StoreError};

use super::seams::{DisconnectPool, EvoReadCx, EvoReads, Fatal, MempoolSink};
use super::signals::ValidationSignals;

/// The list at `at`, or Core's empty list while `at` is null: `GetListAtChainTip()` at the DMN
/// tip, and `GetListForBlock` at the chain tip.
// Core: evo/deterministicmns.cpp:941-947
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
        .map_err(|e| Fatal::abort_node(format!("GetListAtChainTip: {e}")))
}

/// `MaybeUpdateMempoolForReorg(disconnectpool, fAddToMempool)` as D16 has it: without a mempool
/// nothing; with one and `add`, the DMN tip moves to the chain tip `tip` first (Core's
/// `UpdatedMNListTip`), then the sink drains with the list at the DMN tip as it then stands.
/// Without `add` no list is read: Core removes every queued transaction (`:441-451`) and its
/// `removeForReorg` reads none. The hold that runs the drain publishes the move with its
/// release. CH7a-4 moves V's six drain sites onto it (CH7-notes D60).
///
/// # Errors
///
/// The evo store's fault reading the list for an adding drain, as `AbortNode`; the pool is then
/// dropped unread.
// Core: validation.cpp:420-433, evo/specialtxman.cpp:855
#[cfg_attr(not(test), expect(dead_code, reason = "CH7a-4 calls it at V's drain sites (D60)"))]
pub(crate) fn reorg_drain(
    mempool: &mut dyn MempoolSink,
    signals: &ValidationSignals,
    reads: &dyn EvoReads,
    cx: EvoReadCx<'_>,
    tip: Option<BlockId>,
    pool: Box<dyn DisconnectPool>,
    add: bool,
) -> Result<(), Fatal> {
    if !mempool.has_mempool() {
        return Ok(());
    }
    if !add {
        mempool.maybe_update_for_reorg(pool, false);
        return Ok(());
    }
    if let Some(tip) = tip {
        signals.move_dmn_tip(tip);
    }
    let list = dmn_list(reads, cx, signals.dmn_tip())?;
    mempool.maybe_update_for_reorg_at(pool, true, &list);
    Ok(())
}
