//! CH7b-3 C9 (PLAN C9, §6.3): one block's entry of Core's `masternode payments`, rendered from
//! V's state, which T-TN compares against SW-V24's testnet fixtures field by field. The RPC
//! itself is RP2a's; its addresses are the RPC crate's encoding of [`BlockPayments::payees`]'
//! scripts.

use dash_consensus::coins::BlockUndo;
use dash_consensus::ctx::{Deployment, DeploymentView as _};
use dash_consensus::params::genesis_block;
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::TxHash;
use dash_consensus::subsidy::{SubsidyError, SubsidyParams, genesis_reward};
use dash_consensus::tx::inputs::asset_unlock_payload_fee;
use dash_consensus::{Amount, Block};
use dash_dml::payments::{BlockReward, PayeeError, block_tx_outs};
use dash_dml::{PayeeRules, ProTxHash, StoreError};
use dash_params::{BuriedDeployment, ChainSetup};
use dash_primitives::{BlockHash, TxOut};

use crate::chain::ChainQuery;
use crate::connect::block_subsidy_after;

/// The network's `GetMasternodePayment` and `GetBlockSubsidy` parameters.
pub struct RenderParams {
    payments: PaymentParams,
    subsidy: SubsidyParams,
}

impl RenderParams {
    /// # Errors
    ///
    /// Parameters outside Core's domain.
    pub fn new(chain: &ChainSetup) -> Result<Self, String> {
        let reward = genesis_reward(&genesis_block(chain)).map_err(|e| format!("{e:?}"))?;
        let payments = PaymentParams::from_chain_params(chain).map_err(|e| format!("{e:?}"))?;
        let subsidy =
            SubsidyParams::from_chain_params(chain, reward).map_err(|e| format!("{e:?}"))?;
        Ok(Self {
            payments,
            subsidy,
        })
    }
}

/// One block's entry of `masternode payments`: its `height` and `blockhash`, and its one
/// masternode's `proTxHash` and `payees`, each `{address, script, amount}`. Both `amount`s are
/// [`Self::amount`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockPayments {
    pub height: u32,
    pub block_hash: BlockHash,
    /// `nBlockFees`, as the RPC recomputes them, which `payees` are built from.
    pub fees: Amount,
    /// `GetBlockSubsidy(pindex)`, which `payees` are built from too.
    pub subsidy: Amount,
    /// `GetMNPayee(pindex->pprev)`; `None` where Core prints `""`.
    pub pro_tx_hash: Option<ProTxHash>,
    /// `voutMasternodePayments`, in Core's order.
    pub payees: Vec<TxOut>,
}

impl BlockPayments {
    /// The masternode's `amount`, which is the block's too: the payees' sum.
    pub fn amount(&self) -> Amount {
        self.payees.iter().map(|out| out.value).sum()
    }
}

/// Where the render has no entry, as Core's RPC throws or has none.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The parent is not in V's index: the genesis block, which has no entry, or an unknown one.
    #[error("the block's parent {0} is not in the index")]
    Parent(BlockHash),
    #[error("the undo holds {undo} entries for {txs} transactions after the coinbase")]
    UndoCount {
        txs: usize,
        undo: usize,
    },
    #[error("the undo's entry for transaction {0} does not hold one coin per input")]
    UndoCoins(usize),
    /// `CHECK_NONFATAL(payload)`: an asset unlock whose payload does not decode.
    #[error("the asset unlock {0} has no payload")]
    AssetUnlock(TxHash),
    #[error("the fees leave the amount range")]
    Fees,
    /// `pindexPrev->nHeight + 1` past Core's `int`.
    #[error("the height {0} leaves Core's int range")]
    Height(u32),
    #[error("GetBlockSubsidy: {0:?}")]
    Subsidy(SubsidyError),
    #[error("GetListForBlock: {0}")]
    List(StoreError),
    #[error("GetBlockTxOuts: {0}")]
    Payee(PayeeError),
}

/// The `masternode payments` entry of `block`, whose parent is in V's index and whose undo is
/// `undo`. The fees are recomputed as the RPC does, not read off the coinbase, which may claim
/// less: for each transaction after the coinbase its inputs' coins less its outputs, or for an
/// asset unlock, which spends none, its payload's fee. The payees are `GetBlockTxOuts` on the
/// parent's list at the block's subsidy and those fees, in the reward era after the parent.
// Core: rpc/masternode.cpp:430-498 (the loop body); masternode/payments.cpp:206-220
// GetMasternodeTxOuts
pub fn masternode_payments(
    q: &ChainQuery<'_>,
    block: &Block,
    undo: &BlockUndo,
    p: &RenderParams,
) -> Result<BlockPayments, RenderError> {
    // vtx[0] is the coinbase; the undo has one entry per transaction after it.
    let txs = block.txs().get(1..).unwrap_or_default();
    if undo.txs.len() != txs.len() {
        return Err(RenderError::UndoCount {
            txs: txs.len(),
            undo: undo.txs.len(),
        });
    }
    let mut fees: Amount = 0;
    for (i, (tx, spent)) in txs.iter().zip(&undo.txs).enumerate() {
        let fee = if tx.is_platform_transfer() {
            asset_unlock_payload_fee(tx).ok_or(RenderError::AssetUnlock(tx.txid()))?
        } else {
            if spent.coins.len() != tx.inputs().len() {
                return Err(RenderError::UndoCoins(i + 1));
            }
            let value_in =
                spent.coins.iter().try_fold(0, |sum: Amount, c| sum.checked_add(c.value()));
            value_in.and_then(|v| v.checked_sub(tx.value_out())).ok_or(RenderError::Fees)?
        };
        fees = fees.checked_add(fee).ok_or(RenderError::Fees)?;
    }
    payments_at_fees(q, block, fees, p)
}

/// [`masternode_payments`]' entry of `block` were its fees `fees`: the payees `GetBlockTxOuts`
/// builds on them, the rest as the block's own.
// Core: rpc/masternode.cpp:462-466; masternode/payments.cpp:206-220 GetMasternodeTxOuts
pub fn payments_at_fees(
    q: &ChainQuery<'_>,
    block: &Block,
    fees: Amount,
    p: &RenderParams,
) -> Result<BlockPayments, RenderError> {
    let reader = q.chain.reader();
    let parent = block.header().prev_hash();
    let prev = reader.lookup(&parent).ok_or(RenderError::Parent(parent))?;
    let height = reader.entry(prev).ok_or(RenderError::Parent(parent))?.height + 1;
    let after = |d| q.deployments.active_after(Some(prev), Deployment::Buried(d));
    let subsidy = block_subsidy_after(&reader, &q.deployments, Some(prev), &p.subsidy)
        .map_err(RenderError::Subsidy)?;
    let era = MnRewardEra::after(after(BuriedDeployment::V20), after(BuriedDeployment::MnRr));
    let rules = PayeeRules {
        v19: after(BuriedDeployment::V19),
        mn_rr: after(BuriedDeployment::MnRr),
    };
    let list = q.evo_view().list_for_block(prev).map_err(RenderError::List)?;
    let reward = BlockReward {
        height: i32::try_from(height).map_err(|_| RenderError::Height(height))?,
        block_subsidy: subsidy,
        fee_reward: fees,
        era,
    };
    // `FillBlockPayments` hands the RPC what `GetBlockTxOuts` pushed, payable or not.
    let outs = block_tx_outs(&list, rules, &reward, &p.payments).map_err(RenderError::Payee)?;
    Ok(BlockPayments {
        height,
        block_hash: block.hash(),
        fees,
        subsidy,
        pro_tx_hash: list.payee(rules).map(|dmn| dmn.pro_tx_hash),
        payees: outs.outputs,
    })
}
