//! `CreateNewBlock`'s block once DIP0003 is active after the tip (MN0-2a, MN0-3): the quorum
//! commitment transactions, the CbTx and the masternode payments, for the DIP0003, V20, MN_RR and
//! V24 eras, on every chain but one whose block may trigger a superblock
//! ([`superblock_may_trigger`]), which is refused with [`TemplateError::NotReady`] (GV1b). Pure: everything it reads is in [`EvoChain`], which the
//! template source reads from V in the turn that calls it.

use std::sync::Arc;

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
use dash_dml::{
    DML_PREV_LIST_MISMATCH, DmlBuildError, DmlParams, DmlRules, MnList, NoTrace, PayeeRules,
    PrevBlock, StoreError, build_new_list_from_block, merkle_root_mn_list_full,
};
use dash_evo::Effect;
use dash_evo::llmq::enabled_quorum_params;
use dash_params::{BuriedDeployment, DeploymentPos};
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{CoinbaseCommitment, MerkleRoot};
use dash_types::codec::BaseCodec as _;

use crate::assemble::{RESERVED_SIZE, TipFacts, coinbase_input, header};
use crate::{LastBlockStats, TemplateError};

/// `CCbTx::Version`.
// Core: evo/cbtx.h:24-30
const CBTX_MERKLE_ROOT_MNLIST: u16 = 1;
const CBTX_MERKLE_ROOT_QUORUMS: u16 = 2;
const CBTX_CLSIG_AND_BALANCE: u16 = 3;
const CBTX_MERKLE_ROOT_ASSETUNLOCKS: u16 = 4;

/// The slice that pays superblocks (the trigger store).
const SUPERBLOCK_OWNER: &str = "GV1b";

/// What the evo part of `CreateNewBlock` reads under `cs_main`, at the tip.
pub struct EvoChain<'a, C: ?Sized> {
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
    pub ancestry: &'a dyn ChainAncestry,
    pub dml: &'a DmlParams,
    pub net_info: &'a NetInfoRules,
    /// `chainlocks.GetBestChainLock()`: its height and signature, `None` for the null sig.
    pub best_chainlock: Option<(i32, BlsGlobalSig)>,
    /// `GetNonNullCoinbaseChainlock(pindexPrev)`, read only where `CalcCbTxBestChainlock` reads
    /// it.
    pub parent_chainlock: &'a ParentChainlock<'a>,
    /// `DeploymentHeight(V19)`.
    pub v19_height: i32,
    /// `IsSuperblockTriggered(tip list, nHeight)`. Until GV1b's trigger store this is
    /// [`superblock_may_trigger`]; only `false` is assembled here.
    pub superblock_triggered: bool,
    /// `GetCreditPool(pindexPrev)`.
    pub credit_pool: &'a CreditPool,
    pub payments: &'a PaymentParams,
}

/// Whether a block at `height` over `list` may trigger a superblock: `IsSuperblockTriggered`'s
/// height gate, `CSuperblock::IsValidBlockHeight`, on a chain that has had masternodes, the only
/// chains a trigger can be voted on. [`EvoChain::superblock_triggered`] until GV1b.
// Core: v24.0.0-rc.5 governance/superblock.cpp:119-124, :578-583; masternode/payments.cpp:449-461
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
pub type ParentChainlock<'a> = dyn Fn() -> Result<Option<(BlsGlobalSig, u32)>, TemplateError> + 'a;

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

/// An assembled block and the counters `CreateNewBlock` sets (and logs) for it
/// (`node/miner.cpp:259-261`; the commitments have no sigops).
pub struct EvoBlock {
    /// The coinbase, then the quorum commitments; `nNonce` 0 and a null merkle root.
    pub block: Block,
    pub stats: LastBlockStats,
}

/// `CreateNewBlock` on an empty mempool once DIP0003 is active after `prev` (the tip, whose
/// facts are `facts`), paying the miner's part to `script`:
///
/// 1. each enabled LLMQ type's mineable commitments, in `GetEnabledQuorumParams` order
///    (`node/miner.cpp:222-247`);
/// 2. the coinbase (`:263-279`, `:279-283`): scriptSig `OP_RETURN`, version 3, type
///    `TRANSACTION_COINBASE`, its first output the subsidy;
/// 3. the CbTx (`:285-333`): the root of the list after the block (`CMNListBuilder` from the
///    tip's list), from DIP0008 the quorums' root, from V20 `CalcCbTxBestChainlock`'s ChainLock
///    and the credit pool's balance, from version 4 the asset unlocks' root;
/// 4. `FillBlockPayments` (`:337-338`): `GetBlockTxOuts`' outputs over the tip's list, each
///    subtracted from the first (Core's `CAmount` arithmetic; a negative miner output is
///    `TestBlockValidity`'s to refuse).
///
/// `prev` must name the tip `facts` describe.
///
/// # Errors
///
/// [`TemplateError::NotReady`] for a triggered superblock (`"superblock"`, GV1b), and for an
/// evo read V has not built.
///
/// [`TemplateError::Failed`] where Core throws.
///
/// [`BlockError::Fault`] for a fault of the list build's reads (OD-CP-134 M2).
// Core: node/miner.cpp:176-363 BlockAssembler::CreateNewBlock
pub fn block<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    prev: BlockId,
    script: &[u8],
    chain: &EvoChain<'_, C>,
) -> Result<EvoBlock, BlockError> {
    let eras = Eras::after(chain.deployments, prev).map_err(failed)?;
    if !eras.dip3 {
        return Err(failed(format_args!(
            "DIP0003 is not active after the tip at {}",
            facts.height
        ))
        .into());
    }
    if chain.superblock_triggered {
        return Err(TemplateError::NotReady {
            height: facts.height,
            rule: "superblock",
            owner: SUPERBLOCK_OWNER,
        }
        .into());
    }
    let mut qc_cache = QcHashCache::new();
    let height = i32::try_from(facts.height).map_err(|_| failed("height"))?;
    let v19 = chain.deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::V19));

    // :222-247
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
            size += u64::from(tx.size());
            num_txs += 1;
            txs.push(tx);
        }
    }
    // :285-333, over the block with its dummy coinbase, which every root skips.
    let header = header(facts);
    let block = Block::new(header, txs);
    let mn_list_root = list_after(facts, height, eras, v19, prev, &block, chain)?;
    let cbtx = build_cbtx(facts, height, eras, prev, &block, chain, mn_list_root, &mut qc_cache)?;
    let mut extra_payload = Vec::new();
    cbtx.encode(&mut extra_payload);

    // :337-338
    let rules = PayeeRules {
        v19,
        mn_rr: eras.mn_rr,
    };
    let reward = BlockReward {
        height,
        block_subsidy: facts.subsidy,
        fee_reward: 0,
        era: MnRewardEra::after(eras.v20, eras.mn_rr),
    };
    let payments = block_tx_outs(chain.list, rules, &reward, chain.payments)
        .map_err(|e| failed(format_args!("GetBlockTxOuts: {e}")))?;
    let miner = payments
        .outputs
        .iter()
        .fold(facts.subsidy, |value: Amount, out| value.wrapping_sub(out.value));
    let mut outputs = vec![TxOut {
        value: miner,
        script_pubkey: script.to_vec().into(),
    }];
    outputs.extend(payments.outputs);

    let coinbase = Transaction {
        version: SPECIAL_VERSION,
        tx_type: TxType::CoinbaseCommitment,
        inputs: vec![coinbase_input(vec![OP_RETURN])],
        outputs,
        lock_time: 0,
        extra_payload,
    };
    let mut txs = block.txs().to_vec();
    txs[0] = Arc::new(Tx::new(coinbase));
    Ok(EvoBlock {
        block: Block::new(header, txs),
        stats: LastBlockStats {
            num_txs,
            size,
        },
    })
}

/// The root of the masternode list after `block` (`node/miner.cpp:221-237`, `:299-303`): Core's
/// `CMNListBuilder` from the tip's list, each commitment applied as it is selected, then
/// `Finish`, which is `RebuildListFromBlock` over the same transactions (`evo/specialtxman.h:136-
/// 140`). Its debug logs are off, and any effect the build emits (a snapshot row a member
/// computation would store) is dropped with the call: the template writes nothing (MN0-3 notes
/// 3-D4).
// Core: v24.0.0-rc.5 node/miner.cpp:221-237, :299-306
fn list_after<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    height: i32,
    eras: Eras,
    v19: bool,
    prev: BlockId,
    block: &Block,
    chain: &EvoChain<'_, C>,
) -> Result<MerkleRoot, BlockError> {
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
    let mut dropped: Vec<Effect> = Vec::new();
    let built = build_new_list_from_block(
        block,
        prev_block,
        chain.list,
        chain.coins,
        chain.qc.members,
        &rules,
        &mut dropped,
        &mut NoTrace,
    )
    .map_err(|e| match e {
        // Only commitments follow the coinbase, so a refusal is `ApplyTx`'s (:234-237). They
        // spend nothing, so `Finish` refuses none.
        DmlBuildError::Invalid(state) => {
            failed(format_args!("quorum commitment does not apply to the masternode list: {state}"))
                .into()
        }
        // Core's exception leaves `CreateNewBlock` as thrown.
        DmlBuildError::Exception(e) => failed(e).into(),
        DmlBuildError::Store(e) => read_fault(facts.height, "BuildNewListFromBlock", e),
    })?;
    merkle_root_mn_list_full(&built.list)
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootMNList failed: {e}")).into())
}

/// The CbTx of the block whose other transactions `block` holds.
#[allow(clippy::too_many_arguments, reason = "CreateNewBlock's state, passed through")]
fn build_cbtx<C: CoinsView + ?Sized>(
    facts: &TipFacts,
    height: i32,
    eras: Eras,
    prev: BlockId,
    block: &Block,
    chain: &EvoChain<'_, C>,
    merkle_root_mn_list: MerkleRoot,
    qc_cache: &mut QcHashCache,
) -> Result<CoinbaseCommitment, TemplateError> {
    let version = eras.cbtx_version();
    let mut cbtx = CoinbaseCommitment {
        version,
        height,
        merkle_root_mn_list,
        merkle_root_quorums: None,
        best_cl_height_diff: None,
        best_cl_signature: None,
        credit_pool_balance: None,
        merkle_root_asset_unlocks: None,
    };
    if !eras.dip8 {
        return Ok(cbtx);
    }
    // :305-308
    let root = quorum_root(&chain.qc.llmq, chain.qc.mined, qc_cache, block, prev)
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootQuorums: {e:?}")))?
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootQuorums failed: {e}")))?;
    cbtx.merkle_root_quorums = Some(root);
    if !eras.v20 {
        return Ok(cbtx);
    }
    // :309-315
    let tip_height = height.wrapping_sub(1);
    let best = chain.best_chainlock.as_ref().map(|(h, sig)| (*h, sig));
    match best_chainlock(best, chain.v19_height, tip_height, chain.parent_chainlock)? {
        Some((diff, sig)) => {
            tracing::info!(
                "CreateNewBlock() h[{}] CbTx bestCLHeightDiff[{diff}] CLSig[{}]",
                facts.height,
                hex(&sig.to_bytes())
            );
            cbtx.best_cl_height_diff = Some(u64::from(diff));
            cbtx.best_cl_signature = Some(sig);
        }
        None => {
            tracing::info!(
                "CreateNewBlock() h[{}] CbTx failed to find best CL. Inserting null CL",
                facts.height
            );
            cbtx.best_cl_height_diff = Some(0);
            cbtx.best_cl_signature = Some(null_signature());
        }
    }
    // :316-322
    let reward = platform_reward(eras.mn_rr, height, facts.subsidy, chain.payments)
        .map_err(|e| failed(format_args!("PlatformShare: {e}")))?;
    let diff = credit_pool_diff_for_block(block, chain.credit_pool, reward)
        .map_err(|e| failed(format_args!("GetCreditPoolDiffForBlock failed: {e}")))?;
    let balance = diff.total_locked().ok_or_else(|| failed("GetTotalLocked: overflow"))?;
    cbtx.credit_pool_balance = Some(balance);
    // :324-326
    if version >= CBTX_MERKLE_ROOT_ASSETUNLOCKS {
        cbtx.merkle_root_asset_unlocks =
            Some(dash_consensus::block::asset_unlocks_merkle_root(block));
    }
    Ok(cbtx)
}

/// `CalcCbTxBestChainlock(pindexPrev)`: `bestCLHeightDiff` and `bestCLSignature` where it returns
/// true, `None` where it returns false (a null ChainLock). `best` is `GetBestChainLock()`'s height
/// and signature, `None` for the null sig (height −1); `parent` is
/// `GetNonNullCoinbaseChainlock(pindexPrev)`, called only after the best-is-the-tip return, as
/// Core calls it. Core's `int` differences assigned to `uint32_t` wrap, as here: a best ChainLock
/// above the tip (a template on a tip that is not the best chain's) gives a difference near
/// `u32::MAX`.
// Core: v24.0.0-rc.5 node/miner.cpp:117-170 CalcCbTxBestChainlock
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

/// `CCbTx::bestCLSignature` left default: Core's `CBLSSignature()` is invalid, and
/// `CBLSWrapper` writes an invalid object as zeros under either scheme.
fn null_signature() -> BlsGlobalSig {
    BlsGlobalSig::decode(&[0; 96]).expect("zeros decode under either scheme")
}

/// `pblock->vtx.emplace_back()`: the dummy coinbase the roots are computed over.
fn placeholder_coinbase() -> TxRef {
    Arc::new(Tx::new(Transaction {
        version: 0,
        tx_type: TxType::new(0),
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: 0,
        extra_payload: Vec::new(),
    }))
}

fn failed(what: impl std::fmt::Display) -> TemplateError {
    TemplateError::Failed(format!("CreateNewBlock: {what}"))
}

#[cfg(test)]
mod tests;
