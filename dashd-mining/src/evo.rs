//! `CreateNewBlock`'s block once DIP0003 is active after the tip (MN0-2a, MN0-3, MN1-4): the
//! quorum commitment transactions ([`begin`]), then, around the mempool's selection, the CbTx and
//! `FillBlockPayments` ([`finish`]) for the DIP0003, V20, MN_RR and V24 eras. A superblock height
//! whose governance answer is unavailable is refused with [`TemplateError::NotReady`] where it may
//! trigger ([`superblock_may_trigger`]). Pure: everything it reads is in [`EvoChain`], which the
//! template source reads from V in the turn that calls it.

use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_consensus::coins::CoinsView;
use dash_consensus::ctx::{BlockId, ChainAncestry, Deployment, DeploymentView};
use dash_consensus::deployments::EhfReadError;
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::canonical::SPECIAL_VERSION;
use dash_consensus::primitives::{Transaction, Tx, TxOut, TxRef, TxType};
use dash_consensus::script::opcodes::OP_RETURN;
use dash_consensus::{Amount, Block};
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::blockprocessor::{QcCtx, QcHashCache, quorum_root};
use dash_dml::creditpool::{CreditPool, credit_pool_diff_for_block, platform_reward};
use dash_dml::netinfo::NetInfoRules;
use dash_dml::payments::{BlockReward, block_tx_outs};
use dash_dml::specialtx::quorum::QuorumKey;
use dash_dml::specialtx::trivial::{Destination, extract_destination};
use dash_dml::{
    DML_PREV_LIST_MISMATCH, DmlBuildError, DmlParams, DmlRules, KeyId, MnList, MnListBuilder,
    NoTrace, PayeeRules, PrevBlock, StoreError, merkle_root_mn_list_full,
};
use dash_evo::Effect;
use dash_evo::llmq::{BlsSigValue, LlmqCommitment, enabled_quorum_params};
use dash_params::{AddrSetup, BuriedDeployment, DeploymentPos};
use dash_pkc::bls::{BlsEagerSig, BlsGlobalSig, BlsLazyPk};
use dash_primitives::{CoinbaseCommitment, MerkleRoot};
use dash_types::codec::BaseCodec as _;
use dashd_chainstate::connect::SuperblockTemplate;

use crate::assemble::{RESERVED_SIZE, TipFacts, coinbase_input, header};
use crate::{LastBlockStats, TemplateError};

/// `CCbTx::Version`.
// Core: evo/cbtx.h:24-30
const CBTX_MERKLE_ROOT_MNLIST: u16 = 1;
const CBTX_MERKLE_ROOT_QUORUMS: u16 = 2;
const CBTX_CLSIG_AND_BALANCE: u16 = 3;
const CBTX_MERKLE_ROOT_ASSETUNLOCKS: u16 = 4;

/// What the evo part of `CreateNewBlock` reads under `cs_main`, at the tip. The list builder
/// borrows only the `'b` fields, so it outlives the coins and LLMQ reads of one call.
pub struct EvoChain<'a, 'b, C: ?Sized> {
    /// `DeploymentActiveAfter` at the tip.
    pub deployments: &'a dyn DeploymentView,
    /// The quorum block processor at the tip (`m_quorum_block_processor`).
    pub qc: &'a QcCtx<'a>,
    /// Its mineable commitments (`minableCommitments`).
    pub pool: &'a MineablePool,
    /// The tip's masternode list (`GetListForBlock(pindexPrev)`), which pays the block and which
    /// the list after it starts from.
    pub list: &'a MnList,
    /// `m_chainstate.CoinsTip()`, read by the list after the block (`node/miner.cpp:225`).
    pub coins: &'a C,
    /// The block index, as the list build walks it.
    pub ancestry: &'b dyn ChainAncestry,
    pub dml: &'b DmlParams,
    pub net_info: &'b NetInfoRules,
    /// `chainlocks.GetBestChainLock()`: its height and signature, `None` for the null sig.
    pub best_chainlock: Option<(i32, BlsEagerSig)>,
    /// The attempt's BLS scheme, which the CbTx is serialized under: Core's process scheme for
    /// any template that can carry a non-null signature.
    pub bls: BlsCtx,
    /// `GetNonNullCoinbaseChainlock(pindexPrev)`, read only where `CalcCbTxBestChainlock` reads
    /// it.
    pub parent_chainlock: &'a ParentChainlock<'a>,
    /// `DeploymentHeight(V19)`.
    pub v19_height: i32,
    /// `GetCreditPool(pindexPrev)`.
    pub credit_pool: &'a CreditPool,
    pub payments: &'a PaymentParams,
    /// `Params().Base58Prefix`, which `GetMasternodeTxOuts`' log encodes payees with.
    pub addr: &'b AddrSetup,
}

/// Whether a block at `height` over `list` may trigger a superblock: `IsSuperblockTriggered`'s
/// height gate, `CSuperblock::IsValidBlockHeight`, on a chain that has had masternodes, the only
/// chains a trigger can be voted on: where a template without a governance answer is refused.
// Core: v24.0.0 governance/superblock.cpp:119-124, :578-583; masternode/payments.cpp:449-461
pub fn superblock_may_trigger(payments: &PaymentParams, list: &MnList, height: u32) -> bool {
    i32::try_from(height).is_ok_and(|h| payments.config().superblocks.is_superblock_height(h))
        && (list.counts().total() != 0 || list.total_registered() != 0)
}

/// What [`block`] answers in place of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    /// The template's answer to its caller.
    Refused(TemplateError),
    /// A fault of V's own reads in the turn, which connect's class policy (D5) stops the node
    /// on: the template source makes it V's `Fatal` (OD-CP-134 M2), with connect's text.
    Fault(String),
}

impl From<TemplateError> for BlockError {
    fn from(e: TemplateError) -> BlockError {
        BlockError::Refused(e)
    }
}

/// A store error of one of V's own reads in the template's turn, as connect maps it
/// (`store_failure`, D5): a capability not built is the readiness refusal, any other a fault.
pub(crate) fn read_fault(height: u32, what: &str, e: StoreError) -> BlockError {
    match e.capability() {
        Some(_) => BlockError::Refused(crate::source::store_refusal(height, e)),
        None => BlockError::Fault(format!("{what}: {e}")),
    }
}

/// [`EvoChain::parent_chainlock`]: the tip CbTx's non-null ChainLock and its height difference.
pub type ParentChainlock<'a> = dyn Fn() -> Result<Option<(BlsEagerSig, u32)>, TemplateError> + 'a;

/// The deployments `CreateNewBlock` reads after the tip (`node/miner.cpp:199-203`), and MN_RR,
/// which `GetMnRewardEraAfter` and the credit pool's platform reward read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Eras {
    pub dip3: bool,
    pub dip8: bool,
    pub v20: bool,
    pub mn_rr: bool,
    pub v24: bool,
}

impl Eras {
    pub fn after(deployments: &dyn DeploymentView, prev: BlockId) -> Result<Eras, EhfReadError> {
        let buried = |d| deployments.active_after(Some(prev), Deployment::Buried(d));
        let v24 = Deployment::Versionbits(DeploymentPos::V24);
        Ok(Eras {
            dip3: buried(BuriedDeployment::Dip0003),
            dip8: buried(BuriedDeployment::Dip0008),
            v20: buried(BuriedDeployment::V20),
            mn_rr: buried(BuriedDeployment::MnRr),
            v24: deployments.try_active_after(Some(prev), v24)?,
        })
    }

    /// `CCbTx::nVersion` (`node/miner.cpp:285-293`).
    pub fn cbtx_version(self) -> u16 {
        if self.v24 && self.v20 {
            CBTX_MERKLE_ROOT_ASSETUNLOCKS
        } else if self.v20 {
            CBTX_CLSIG_AND_BALANCE
        } else if self.dip8 {
            CBTX_MERKLE_ROOT_QUORUMS
        } else {
            CBTX_MERKLE_ROOT_MNLIST
        }
    }
}

/// An assembled block and what `CreateNewBlock` sets for it (`node/miner.cpp:259-261`, `:338`).
pub struct EvoBlock {
    /// The coinbase, the quorum commitments, then the selected transactions; `nNonce` 0 and a
    /// null merkle root.
    pub block: Block,
    pub stats: LastBlockStats,
    /// `FillBlockPayments`' masternode outputs, in the coinbase's order.
    pub masternode_payments: Vec<TxOut>,
    /// `FillBlockPayments`' superblock outputs, after the masternode outputs.
    pub superblock_payments: Vec<TxOut>,
}

/// `FillBlockPayments`' superblock half as a template answers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Superblock {
    /// `IsSuperblockTriggered` false, or no governance answer where no superblock can trigger.
    NotTriggered,
    /// `GetSuperblockPayments`' outputs, in order.
    Triggered(Vec<TxOut>),
    /// No governance answer where the block may trigger one: [`TemplateError::NotReady`].
    Refused {
        owner: &'static str,
    },
}

impl Superblock {
    /// The governance answer for the block at `height`. An unavailable back end refuses only where
    /// the block may trigger a superblock over `dmn_tip_list`: elsewhere Core's
    /// `IsSuperblockTriggered` is false without reading governance.
    // Core: v24.0.2 masternode/payments.cpp:449-454
    pub fn of(
        answer: SuperblockTemplate,
        payments: &PaymentParams,
        dmn_tip_list: &MnList,
        height: u32,
    ) -> Superblock {
        match answer {
            SuperblockTemplate::NotTriggered => Superblock::NotTriggered,
            SuperblockTemplate::Triggered(outs) => Superblock::Triggered(outs),
            SuperblockTemplate::Unavailable {
                owner,
            } if superblock_may_trigger(payments, dmn_tip_list, height) => Superblock::Refused {
                owner,
            },
            SuperblockTemplate::Unavailable {
                ..
            } => Superblock::NotTriggered,
        }
    }

    /// `FillBlockPayments`' superblock outputs for the block at `height`, with its `GOBJECT`
    /// line; a refusal is [`TemplateError::NotReady`].
    // Core: v24.0.2 masternode/payments.cpp:449-454
    pub(crate) fn outputs(self, height: u32) -> Result<Vec<TxOut>, TemplateError> {
        match self {
            Superblock::NotTriggered => Ok(Vec::new()),
            Superblock::Triggered(outs) => {
                tracing::debug!(
                    target: "gobject",
                    "CMNPaymentsProcessor::FillBlockPayments -- Triggered superblock creation at \
                     height {height}"
                );
                Ok(outs)
            }
            Superblock::Refused {
                owner,
            } => Err(TemplateError::NotReady {
                height,
                rule: "superblock",
                owner,
            }),
        }
    }
}

/// `CreateNewBlock` up to its mempool selection.
pub struct Started {
    eras: Eras,
    v19: bool,
    /// The dummy coinbase, then the commitments.
    pub txs: Vec<TxRef>,
    /// `nBlockSize` and `nBlockTx` after the commitments.
    pub stats: LastBlockStats,
}

/// `CreateNewBlock` on an empty mempool once DIP0003 is active after `prev`: [`begin`], then
/// [`finish`] with nothing selected and `superblock` as `FillBlockPayments`' governance answer.
///
/// # Errors
///
/// [`begin`]'s and [`finish`]'s.
// Core: v24.0.2 node/miner.cpp:176-363 BlockAssembler::CreateNewBlock
pub fn block<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    prev: BlockId,
    script: &[u8],
    chain: &EvoChain<'_, '_, C>,
    superblock: Superblock,
) -> Result<EvoBlock, BlockError> {
    let (started, builder) = begin(facts, prev, chain)?;
    let stats = started.stats;
    finish(facts, prev, script, chain, started, &builder, &[], 0, stats, superblock)
}

/// `CreateNewBlock` once DIP0003 is active after `prev` (the tip, whose facts are `facts`), up
/// to the mempool: the list builder from the tip's list, then each enabled LLMQ type's mineable
/// commitments, in `GetEnabledQuorumParams` order, each applied to the builder before the next
/// type's are fetched. The builder returned has every commitment applied.
///
/// # Errors
///
/// [`TemplateError::Failed`] where Core throws; [`BlockError::Fault`] for a fault of the list
/// build's reads (OD-CP-134 M2).
// Core: v24.0.2 node/miner.cpp:218-247
pub fn begin<'b, C: CoinsView + ?Sized>(
    facts: &TipFacts,
    prev: BlockId,
    chain: &EvoChain<'_, 'b, C>,
) -> Result<(Started, MnListBuilder<'b, 'b>), BlockError> {
    let eras = Eras::after(chain.deployments, prev).map_err(failed)?;
    if !eras.dip3 {
        return Err(failed(format_args!(
            "DIP0003 is not active after the tip at {}",
            facts.height
        ))
        .into());
    }
    let height = i32::try_from(facts.height).map_err(|_| failed("height"))?;
    let v19 = chain.deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::V19));
    let mut builder = list_builder(facts, height, eras, v19, prev, chain)?;
    let mut dropped: Vec<Effect> = Vec::new();
    let mut txs = vec![placeholder_coinbase()];
    let mut size = RESERVED_SIZE;
    let mut num_txs = 0;
    let enabled = enabled_quorum_params(&chain.qc.llmq, prev)
        .map_err(|e| failed(format_args!("GetEnabledQuorumParams: {e:?}")))?;
    for (llmq_type, params) in enabled {
        let qc_txs = chain
            .pool
            .mineable_commitments_tx(chain.qc, llmq_type, params, prev, facts.height)
            .map_err(|e| failed(format_args!("GetMineableCommitmentsTx: {e:?}")))?;
        for tx in qc_txs.into_iter().flatten() {
            builder
                .apply_tx(&tx, chain.coins, chain.qc.members, &mut dropped, &mut NoTrace)
                .map_err(|e| list_build_refusal(facts.height, APPLY_THROW, e))?;
            size += u64::from(tx.size());
            num_txs += 1;
            txs.push(tx);
        }
    }
    let started = Started {
        eras,
        v19,
        txs,
        stats: LastBlockStats {
            num_txs,
            size,
        },
    };
    Ok((started, builder))
}

/// `CreateNewBlock`'s block through its CbTx, before `FillBlockPayments`: the dummy coinbase,
/// the commitments and the selected transactions, and the CbTx's payload.
pub struct CbTxBlock {
    eras: Eras,
    v19: bool,
    block: Block,
    extra_payload: Vec<u8>,
}

/// `CreateNewBlock` after the mempool: [`cbtx`], then [`pay`] with `superblock` as
/// `FillBlockPayments`' governance answer.
///
/// # Errors
///
/// [`cbtx`]'s and [`pay`]'s.
#[allow(clippy::too_many_arguments, reason = "CreateNewBlock's state, passed through")]
pub fn finish<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    prev: BlockId,
    script: &[u8],
    chain: &EvoChain<'_, '_, C>,
    started: Started,
    builder: &MnListBuilder<'_, '_>,
    selected: &[TxRef],
    fees: Amount,
    stats: LastBlockStats,
    superblock: Superblock,
) -> Result<EvoBlock, BlockError> {
    let block = cbtx(facts, prev, chain, started, builder, selected)?;
    pay(facts, script, chain.list, chain.payments, chain.addr, block, fees, stats, superblock)
}

/// The CbTx over the commitments `started` holds and the `selected` transactions, whose list
/// effects `builder` has applied (`:285-333`): the root of the list after the block (`builder`'s
/// `Finish`), from DIP0008 the quorums' root, from V20 `CalcCbTxBestChainlock`'s ChainLock and
/// the credit pool's balance after the block's asset locks and unlocks, from version 4 the asset
/// unlocks' root. Read under the coins cell and the LLMQ view.
///
/// # Errors
///
/// [`TemplateError::NotReady`] for an evo read V has not built; [`TemplateError::Failed`] where
/// Core throws; [`BlockError::Fault`] for a fault of the list build's reads (OD-CP-134 M2).
// Core: v24.0.2 node/miner.cpp:285-333
pub fn cbtx<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    prev: BlockId,
    chain: &EvoChain<'_, '_, C>,
    started: Started,
    builder: &MnListBuilder<'_, '_>,
    selected: &[TxRef],
) -> Result<CbTxBlock, BlockError> {
    let Started {
        eras,
        v19,
        mut txs,
        ..
    } = started;
    let height = i32::try_from(facts.height).map_err(|_| failed("height"))?;
    let mut qc_cache = QcHashCache::new();
    txs.extend(selected.iter().cloned());
    // Over the block with its dummy coinbase, which every root skips.
    let block = Block::new(header(facts), txs);
    let mn_list_root = list_after(facts, builder, &block)?;
    let cbtx = build_cbtx(facts, height, eras, prev, &block, chain, mn_list_root, &mut qc_cache)?;
    let mut extra_payload = Vec::new();
    cbtx.encode(&mut extra_payload);
    Ok(CbTxBlock {
        eras,
        v19,
        block,
        extra_payload,
    })
}

/// The coinbase and `FillBlockPayments` over `block`, with no coins or LLMQ view held:
///
/// 1. the coinbase (`:263-283`): scriptSig `OP_RETURN`, version 3, type `TRANSACTION_COINBASE`,
///    its first output the subsidy plus the selected transactions' `fees`, `stats` the block's
///    counters;
/// 2. `FillBlockPayments` (`:337-338`): the superblock's trigger first, then `GetBlockTxOuts`'
///    outputs over the tip's `list` with the fees as the fee reward, each subtracted from the
///    first output (Core's `CAmount` arithmetic; a negative miner output is
///    `TestBlockValidity`'s to refuse), then the superblock's outputs.
///
/// # Errors
///
/// [`TemplateError::NotReady`] for a refused superblock (`"superblock"`);
/// [`TemplateError::Failed`] where Core throws.
// Core: v24.0.2 node/miner.cpp:263-283, :337-341, masternode/payments.cpp:444-474
#[allow(clippy::too_many_arguments, reason = "CreateNewBlock's state, passed through")]
pub fn pay(
    facts: &TipFacts,
    script: &[u8],
    list: &MnList,
    payments: &PaymentParams,
    addr: &AddrSetup,
    block: CbTxBlock,
    fees: Amount,
    stats: LastBlockStats,
    superblock: Superblock,
) -> Result<EvoBlock, BlockError> {
    let CbTxBlock {
        eras,
        v19,
        block,
        extra_payload,
    } = block;
    let height = i32::try_from(facts.height).map_err(|_| failed("height"))?;
    let superblock_payments = superblock.outputs(facts.height)?;
    let rules = PayeeRules {
        v19,
        mn_rr: eras.mn_rr,
    };
    let reward = BlockReward {
        height,
        block_subsidy: facts.subsidy,
        fee_reward: fees,
        era: MnRewardEra::after(eras.v20, eras.mn_rr),
    };
    let tx_outs = block_tx_outs(list, rules, &reward, payments)
        .map_err(|e| failed(format_args!("GetBlockTxOuts: {e}")))?;
    log_block_tx_outs(&reward, list, addr, &tx_outs.outputs, tx_outs.computed);
    let miner =
        tx_outs.outputs.iter().fold(facts.subsidy.wrapping_add(fees), |value: Amount, out| {
            value.wrapping_sub(out.value())
        });
    let mut outputs = vec![TxOut::from_duffs(miner, script.to_vec().into())];
    outputs.extend(tx_outs.outputs.iter().cloned());
    outputs.extend(superblock_payments.iter().cloned());

    let coinbase = Transaction::new(
        SPECIAL_VERSION,
        TxType::CoinbaseCommitment,
        vec![coinbase_input(vec![OP_RETURN])],
        outputs,
        0,
        extra_payload,
    );
    let mut txs = block.txs().to_vec();
    txs[0] = Arc::new(Tx::new(coinbase));
    Ok(EvoBlock {
        block: Block::new(*block.header(), txs),
        stats,
        masternode_payments: tx_outs.outputs,
        superblock_payments,
    })
}

/// `GetBlockTxOuts`' and `FillBlockPayments`' `MNPAYMENTS` lines, and `GetMasternodeTxOuts`'
/// line for each output, or its error line where no payee was found, in Core's order.
// Core: v24.0.2 masternode/payments.cpp:149, :154, :213, :217-222, :457
fn log_block_tx_outs(
    reward: &BlockReward,
    list: &MnList,
    addr: &AddrSetup,
    outputs: &[TxOut],
    computed: bool,
) {
    if reward.era == MnRewardEra::EvoReward
        && let Some(platform) = outputs.first()
    {
        tracing::debug!(
            target: "mnpayments",
            "CMNPaymentsProcessor::GetBlockTxOuts -- MN reward {} reallocated to credit pool",
            platform.value()
        );
    }
    if list.counts().total() == 0 {
        log_no_masternode_registered();
    }
    if computed {
        for out in outputs {
            tracing::info!(
                "CMNPaymentsProcessor::GetMasternodeTxOuts -- Masternode payment {} to {}",
                out.value(),
                encode_destination(out.script_pubkey().as_bytes(), addr)
            );
        }
    } else {
        tracing::info!("CMNPaymentsProcessor::GetMasternodeTxOuts -- ERROR Failed to get payee");
        tracing::debug!(
            target: "mnpayments",
            "CMNPaymentsProcessor::FillBlockPayments -- No masternode to pay (MN list probably \
             empty)"
        );
    }
}

/// `EncodeDestination(ExtractDestination(script))`: empty for a script with no destination.
// Core: v24.0.2 key_io.cpp:17-40, :185-188; script/standard.cpp:161-189
fn encode_destination(script: &[u8], addr: &AddrSetup) -> String {
    let (prefix, hash) = match extract_destination(script) {
        Some(Destination::PubKeyHash(hash)) => (addr.pubkey_addr, hash),
        Some(Destination::ScriptHash(hash)) => (addr.script_addr, hash),
        None => return String::new(),
    };
    KeyId::from(hash).to_base58c(prefix)
}

/// `GetBlockTxOuts`' line for a list without masternodes.
// Core: v24.0.2 masternode/payments.cpp:153-155
pub(crate) fn log_no_masternode_registered() {
    tracing::debug!(
        target: "mnpayments",
        "CMNPaymentsProcessor::GetBlockTxOuts -- no masternode registered to receive a payment"
    );
}

/// `CreateNewBlock`'s `CMNListBuilder` from the tip's list (`node/miner.cpp:218-225`): the
/// confirmations and the PoSe decay of the template's height. Its debug logs are off, and any
/// effect the commitments' `ApplyTx` emits (a snapshot row a member computation would store) is
/// dropped: the template writes nothing (MN0-3 notes 3-D4).
// Core: v24.0.2 node/miner.cpp:218-225
fn list_builder<'b, C: CoinsView + ?Sized>(
    facts: &TipFacts,
    height: i32,
    eras: Eras,
    v19: bool,
    prev: BlockId,
    chain: &EvoChain<'_, 'b, C>,
) -> Result<MnListBuilder<'b, 'b>, BlockError> {
    // The builder's own precondition, a node fault where the connect calls it (EV2b D8), is a
    // refusal here: a list not at the tip is no read's fault (OD-CP-134 M2). Checked first, so no
    // store error the build returns is it.
    if !(chain.list.is_default() || *chain.list.block_hash() == facts.prev) {
        return Err(failed(StoreError::inconsistent(DML_PREV_LIST_MISMATCH)).into());
    }
    let rules = DmlRules {
        params: chain.dml,
        net_info: chain.net_info,
        v19,
        mn_rr: eras.mn_rr,
        v24: eras.v24,
    };
    let prev_block = PrevBlock {
        id: prev,
        hash: facts.prev,
        height: height.wrapping_sub(1),
        chain: chain.ancestry,
    };
    // The precondition is checked above, so `begin` refuses nothing; its exception passes
    // through as thrown, and the label is never read.
    MnListBuilder::begin(prev_block, chain.list, rules)
        .map_err(|e| list_build_refusal(facts.height, FINISH_THROW, e))
}

/// The root of the masternode list after `block` (`node/miner.cpp:299-306`): `Finish` on the
/// builder every special transaction of `block` was applied to, which is `RebuildListFromBlock`
/// over the same transactions (`evo/specialtxman.h:138-142`).
// Core: v24.0.2 node/miner.cpp:299-306
fn list_after(
    facts: &TipFacts,
    list: &MnListBuilder<'_, '_>,
    block: &Block,
) -> Result<MerkleRoot, BlockError> {
    let built = list
        .finish(block, &mut NoTrace)
        .map_err(|e| list_build_refusal(facts.height, FINISH_THROW, e))?;
    merkle_root_mn_list_full(&built.list)
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootMNList failed: {e}")).into())
}

/// `CreateNewBlock`'s throw when a commitment's `ApplyTx` fails (`node/miner.cpp:234-236`).
const APPLY_THROW: &str = "quorum commitment does not apply to the masternode list";
/// `CreateNewBlock`'s throw when `Finish` fails (`node/miner.cpp:301-302`).
const FINISH_THROW: &str = "BuildNewListFromBlock failed";

/// A list build's error from the template's builder: a refusal is `CreateNewBlock`'s throw
/// `what` with the state, and a store error is split as connect splits it. An exception
/// (`UpdateMN`/`RemoveMN`'s `std::runtime_error`) passes through `CreateNewBlock` uncaught, and
/// the RPC server answers its `what()` as thrown, so it gets no `CreateNewBlock: ` prefix.
fn list_build_refusal(height: u32, what: &str, e: DmlBuildError) -> BlockError {
    match e {
        DmlBuildError::Invalid(state) => failed(format_args!("{what}: {state}")).into(),
        DmlBuildError::Exception(e) => TemplateError::Failed(e.to_string()).into(),
        DmlBuildError::Store(e) => read_fault(height, "BuildNewListFromBlock", e),
    }
}

/// The CbTx of the block whose other transactions `block` holds.
#[allow(clippy::too_many_arguments, reason = "CreateNewBlock's state, passed through")]
fn build_cbtx<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    height: i32,
    eras: Eras,
    prev: BlockId,
    block: &Block,
    chain: &EvoChain<'_, '_, C>,
    merkle_root_mn_list: MerkleRoot,
    qc_cache: &mut QcHashCache,
) -> Result<CoinbaseCommitment, TemplateError> {
    let version = eras.cbtx_version();
    // The fields after the list root, as each era step adds them (base-sdk's `CoinbaseCommitment`
    // is built whole, so each return builds it from the fields so far). A field the version has
    // and no step set keeps `CCbTx`'s default, as when V20 is active before DIP0008.
    let cbtx = |merkle_root_quorums: Option<dash_primitives::MerkleRoot>,
                best_cl: Option<(u64, BlsEagerSig)>,
                credit_pool_balance: Option<i64>,
                merkle_root_asset_unlocks: Option<dash_primitives::MerkleRoot>| {
        let (best_cl_height_diff, best_cl_signature) =
            best_cl.map(|(diff, sig)| (diff, BlsGlobalSig::from_eager(&sig, chain.bls))).unzip();
        dash_evo::cbtx::at_version(&CoinbaseCommitment::new(
            dash_primitives::CbCommitmentVersion::new(version),
            height.cast_unsigned().into(),
            merkle_root_mn_list,
            merkle_root_quorums,
            best_cl_height_diff,
            best_cl_signature,
            credit_pool_balance,
            merkle_root_asset_unlocks,
        ))
    };
    if !eras.dip8 {
        return Ok(cbtx(None, None, None, None));
    }
    // :305-308
    let root = quorum_root(&chain.qc.llmq, chain.qc.mined, qc_cache, block, prev)
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootQuorums: {e:?}")))?
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootQuorums failed: {e}")))?;
    if !eras.v20 {
        return Ok(cbtx(Some(root), None, None, None));
    }
    // :309-315
    let tip_height = height.wrapping_sub(1);
    let best = chain.best_chainlock.as_ref().map(|(h, sig)| (*h, sig));
    let best_cl =
        Some(match best_chainlock(best, chain.v19_height, tip_height, chain.parent_chainlock)? {
            Some((diff, sig)) => {
                tracing::info!(
                    "CreateNewBlock() h[{}] CbTx bestCLHeightDiff[{diff}] CLSig[{}]",
                    facts.height,
                    hex(&sig.to_bytes(chain.bls.scheme()))
                );
                (u64::from(diff), sig)
            }
            None => {
                tracing::info!(
                    "CreateNewBlock() h[{}] CbTx failed to find best CL. Inserting null CL",
                    facts.height
                );
                (0, BlsEagerSig::new(BlsSigValue::invalid(), chain.bls.scheme()))
            }
        });
    // :316-322
    let reward = platform_reward(eras.mn_rr, height, facts.subsidy, chain.payments)
        .map_err(|e| failed(format_args!("PlatformShare: {e}")))?;
    let diff = credit_pool_diff_for_block(block, chain.credit_pool, reward)
        .map_err(|e| failed(format_args!("GetCreditPoolDiffForBlock failed: {e}")))?;
    let balance = diff.total_locked().ok_or_else(|| failed("GetTotalLocked: overflow"))?;
    // :324-326
    let asset_unlocks = (version >= CBTX_MERKLE_ROOT_ASSETUNLOCKS)
        .then(|| dash_consensus::block::asset_unlocks_merkle_root(block));
    Ok(cbtx(Some(root), best_cl, Some(balance), asset_unlocks))
}

/// `CalcCbTxBestChainlock(pindexPrev)`: `bestCLHeightDiff` and `bestCLSignature` where it returns
/// true, `None` where it returns false (a null ChainLock). `best` is `GetBestChainLock()`'s height
/// and signature, `None` for the null sig (height −1); `parent` is
/// `GetNonNullCoinbaseChainlock(pindexPrev)`, called only after the best-is-the-tip return, as
/// Core calls it. Core's `int` differences assigned to `uint32_t` wrap, as here: a best ChainLock
/// above the tip (a template on a tip that is not the best chain's) gives a difference near
/// `u32::MAX`.
// Core: v24.0.0 node/miner.cpp:117-170 CalcCbTxBestChainlock
pub(crate) fn best_chainlock<S: Clone, E>(
    best: Option<(i32, &S)>,
    v19_height: i32,
    tip_height: i32,
    parent: &dyn Fn() -> Result<Option<(S, u32)>, E>,
) -> Result<Option<(u32, S)>, E> {
    // :120-124: no legacy BLS ChainLock in a CbTx; the null sig's −1 is below any height.
    let best = best.filter(|(h, _)| *h >= v19_height);
    // :125-131
    if let Some((h, sig)) = best
        && h == tip_height
    {
        return Ok(Some((0, sig.clone())));
    }
    let behind = |h: i32| tip_height.wrapping_sub(h) as u32;
    // :132-157
    if let Some((parent_sig, parent_diff)) = parent()? {
        let carried = Some((parent_diff.wrapping_add(1), parent_sig));
        let Some((h, sig)) = best else {
            return Ok(carried);
        };
        let prev_cl_height = tip_height.wrapping_sub(parent_diff as i32).wrapping_sub(1);
        if h < prev_cl_height {
            return Ok(carried);
        }
        return Ok(Some((behind(h), sig.clone())));
    }
    // :158-170
    Ok(best.map(|(h, sig)| (behind(h), sig.clone())))
}

/// `CBLSSignature::ToString()`: the serialized bytes in hex.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `pblock->vtx.emplace_back()`: the dummy coinbase the roots are computed over.
fn placeholder_coinbase() -> TxRef {
    Arc::new(Tx::new(Transaction::new(0, TxType::new(0), Vec::new(), Vec::new(), 0, Vec::new())))
}

fn failed(what: impl std::fmt::Display) -> TemplateError {
    TemplateError::Failed(format!("CreateNewBlock: {what}"))
}

#[cfg(test)]
mod tests;

/// `GetQuorum`'s answer for a mined commitment: its hash and `quorumPublicKey`.
pub(crate) fn quorum_key(commitment: &LlmqCommitment) -> QuorumKey {
    let key = commitment.quorum_public_key();
    QuorumKey {
        quorum_hash: commitment.quorum_hash(),
        public_key: BlsLazyPk::from_value(key.value().clone(), key.scheme()),
    }
}
