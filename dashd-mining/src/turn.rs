//! `CreateNewBlock`'s query turn in its read order (plan §5): the commitments and the list
//! builder under the coins cell and the LLMQ view; `GetListAtChainTip()` before the pool's read
//! lock; `addPackageTxs` under that lock, each check taking the coins for itself; the CbTx under
//! the coins cell and the LLMQ view again; then `FillBlockPayments`, its governance read with no
//! pool guard and no coins held. All in one V turn, so nothing they read changes between them.

use std::sync::Arc;

use dash_consensus::primitives::TxRef;
use dash_dml::{MnList, MnListBuilder};
use dashd_chainstate::connect::SuperblockTemplate;

use crate::evo::BlockError;
use crate::options::Options;
use crate::package::{PackageInputs, PackageRules, TipReads};
use crate::select::{BlockState, Selection, add_package_txs};
use crate::view::{MiningMempool, PackageView};

/// One template turn's steps, called by [`run`] in order.
pub(crate) trait Turn {
    /// What the steps before the mempool leave: the commitments and the list builder.
    type Started;
    /// The block through its CbTx.
    type CbTx;
    type Out;

    /// The list builder and the mineable commitments (Core v24.0.2 src/node/miner.cpp:218-247),
    /// under the coins cell then the LLMQ view, both released before it returns.
    fn begin(&mut self) -> Result<Self::Started, BlockError>;

    /// `GetListAtChainTip()`, read before the pool's lock and kept for the turn.
    fn dmn_tip_list(&mut self) -> Result<Arc<MnList>, BlockError>;

    /// `addPackageTxs` over the pool's read guard (Core v24.0.2 src/node/miner.cpp:251-255).
    fn select(
        &mut self,
        pool: &dyn PackageView,
        dmn_tip_list: &MnList,
        started: &mut Self::Started,
    ) -> Result<(), BlockError>;

    /// The CbTx (Core v24.0.2 src/node/miner.cpp:259-333), under the coins cell then the LLMQ
    /// view, both released before it returns.
    fn cbtx(&mut self, started: Self::Started) -> Result<Self::CbTx, BlockError>;

    /// `IsSuperblockTriggered` and `GetSuperblockPayments` on `dmn_tip_list`
    /// (Core v24.0.2 src/masternode/payments.cpp:449-453).
    fn superblock(&mut self, dmn_tip_list: &MnList) -> Result<SuperblockTemplate, BlockError>;

    /// The coinbase and the rest of `FillBlockPayments` (Core v24.0.2 src/node/miner.cpp:337-349).
    fn finish(
        &mut self,
        cbtx: Self::CbTx,
        dmn_tip_list: &MnList,
        superblock: SuperblockTemplate,
    ) -> Result<Self::Out, BlockError>;
}

/// The turn's steps in order; the mempool's selection only where `CreateNewBlock` has a mempool.
pub(crate) fn run<T: Turn>(
    turn: &mut T,
    mempool: Option<&dyn MiningMempool>,
) -> Result<T::Out, BlockError> {
    let mut started = turn.begin()?;
    let dmn_tip_list = turn.dmn_tip_list()?;
    if let Some(mempool) = mempool {
        let mut selected = None;
        mempool.with_read(&mut |pool| {
            selected = Some(turn.select(pool, &dmn_tip_list, &mut started));
        });
        selected.ok_or_else(|| BlockError::Fault("the mempool's read lent no view".into()))??;
    }
    let cbtx = turn.cbtx(started)?;
    let superblock = turn.superblock(&dmn_tip_list)?;
    turn.finish(cbtx, &dmn_tip_list, superblock)
}

/// What `addPackageTxs` left: `txs` in block order, `builder` with every selected package
/// applied.
pub(crate) struct Picked<'b> {
    pub selection: Selection,
    pub txs: Vec<TxRef>,
    pub builder: Option<MnListBuilder<'b, 'b>>,
}

/// `addPackageTxs` with the Dash package rules over `pool`, the block starting at `start`.
/// [`PackageRules::finish`] runs before the selection is read: a read fault ends the template
/// with that fault, never a partial selection.
pub(crate) fn select<'b, T: TipReads>(
    pool: &dyn PackageView,
    inputs: PackageInputs<'_, 'b, T>,
    options: &Options,
    start: BlockState,
) -> Result<Picked<'b>, BlockError> {
    let mut rules = PackageRules::new(inputs);
    let selection = add_package_txs(pool, &mut rules, options, start);
    let builder = rules.finish()?;
    let txs = selection.entries.iter().map(|s| Arc::clone(pool.mining_entry(s.id).tx)).collect();
    Ok(Picked {
        selection,
        txs,
        builder,
    })
}

#[cfg(test)]
mod tests;
