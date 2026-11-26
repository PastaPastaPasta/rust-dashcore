//! The production chain seam: governance commands run on V as class R queries
//! ([`ChainExec`]), read the chain through the query ([`QueryChain`]), and the off-V readers
//! take the DMN-tip list from V's published view.

use std::sync::{Arc, OnceLock};

use dash_dml::MnList;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, ChainQuery, ChainViewHandle, Fatal};

use crate::exec::{GovChain, GovCommand, GovExec};
use crate::sync::DmnTip;

/// [`GovExec`] on V: each command is one class R query, which V serves idle or at a release
/// point, never between the blocks of one batch. A poster carrying the no-wait mark posts on the
/// marked lane and never blocks.
///
/// It is bound to V once V has spawned, as the connector holding the superblock back end is
/// built first. A command posted before the binding, or after V stopped, is dropped unrun.
#[derive(Clone, Default)]
pub struct ChainExec(Arc<OnceLock<ChainHandle>>);

impl ChainExec {
    /// Binds V: `false`, and nothing changes, when V is already bound.
    pub fn bind(&self, v: ChainHandle) -> bool {
        self.0.set(v).is_ok()
    }
}

impl GovExec for ChainExec {
    /// A fault reading the DMN-tip list stops V, as V's own reads of it do, and drops the
    /// command unrun.
    fn post(&self, command: GovCommand) {
        let Some(v) = self.0.get() else {
            return;
        };
        drop(v.query_or_stop(move |q| {
            let chain = QueryChain::read(q)?;
            command(&chain);
            Ok(())
        }));
    }
}

/// [`GovChain`] over V's query: the active chain as V holds it in this turn, and the list at
/// V's DMN tip, read once when the command starts.
pub struct QueryChain<'q> {
    query: &'q ChainQuery<'q>,
    list: Arc<MnList>,
}

impl<'q> QueryChain<'q> {
    /// # Errors
    ///
    /// A store fault reading the DMN-tip list.
    pub fn read(query: &'q ChainQuery<'q>) -> Result<Self, Fatal> {
        let list = query
            .evo_view()
            .dmn_list()
            .map_err(|e| Fatal::new(format!("GetListAtChainTip: {e}")))?;
        Ok(Self {
            query,
            list,
        })
    }
}

impl GovChain for QueryChain<'_> {
    // Core: governance/object.cpp v24.0.2:855-859
    fn active_height(&self, block: &BlockHash) -> Option<u32> {
        let chain = self.query.chain;
        let reader = chain.reader();
        let id = reader.lookup(block)?;
        if !chain.active_contains_private(id) {
            return None;
        }
        reader.entry(id).map(|e| e.height)
    }

    fn tip_height(&self) -> u32 {
        self.query.chain.private_tip().map_or(0, |(tip, _)| tip.height)
    }

    fn dmn_list(&self) -> Arc<MnList> {
        Arc::clone(&self.list)
    }
}

/// The DMN-tip list from V's last published view: `GetListAtChainTip()` without waiting for
/// V. Inside a batch V's DMN tip stays at the last notified tip, which is the published one.
impl DmnTip for ChainViewHandle {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        Arc::clone(&self.view().mn_list)
    }
}

/// [`ChainViewHandle`]'s list through the bound V. Unbound, no block connects and no command
/// runs, so nothing reads it; it answers Core's empty list.
impl DmnTip for ChainExec {
    fn dmn_tip_list(&self) -> Arc<MnList> {
        self.0.get().map_or_else(
            || Arc::new(MnList::default_pre_dip3()),
            |v| v.view_handle().dmn_tip_list(),
        )
    }
}

// `collateral::locate` binds to V's own query inside a query closure: nothing implements
// IX1's `Sync` lookup over it.
const _: fn(&ChainHandle, Arc<dyn dashd_indexes::TxIndexReader>, dashd_indexes::Txid) =
    |v, index, txid| {
        drop(v.query_or_stop(move |q| {
            let chain = QueryChain::read(q)?;
            Ok(crate::collateral::locate(&*index, &chain, &txid))
        }));
    };
