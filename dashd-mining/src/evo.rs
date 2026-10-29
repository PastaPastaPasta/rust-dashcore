//! `CreateNewBlock`'s block once DIP0003 is active after the tip (MN0-2a): the quorum commitment
//! transactions, the CbTx and the masternode payments, for the DIP0003, V20, MN_RR and V24 eras
//! over an empty masternode list, no ChainLock, no superblock trigger and no real quorum
//! commitment (mined or mineable); the others are refused with [`TemplateError::NotReady`]. Those
//! are the chains that never had masternodes: one whose masternodes are gone keeps their
//! ChainLocks, triggers and commitments. Pure: everything it reads is in [`EvoChain`]. The template
//! source does not call it yet; the readiness guard still refuses these blocks (MN0-2b).

use std::sync::Arc;

use dash_consensus::ctx::{BlockId, Deployment, DeploymentView};
use dash_consensus::payments::{MnRewardEra, PaymentParams};
use dash_consensus::primitives::canonical::SPECIAL_VERSION;
use dash_consensus::primitives::{Transaction, Tx, TxOut, TxRef, TxType};
use dash_consensus::script::opcodes::OP_RETURN;
use dash_consensus::{Amount, Block};
use dash_dml::blockprocessor::pool::MineablePool;
use dash_dml::blockprocessor::{QcCtx, QcHashCache, qc_hashes, quorum_root};
use dash_dml::creditpool::{CreditPool, credit_pool_diff_for_block, platform_reward};
use dash_dml::payments::{BlockReward, block_tx_outs};
use dash_dml::{MnList, PayeeRules, merkle_root_mn_list_full};
use dash_evo::llmq::enabled_quorum_params;
use dash_params::{BuriedDeployment, DeploymentPos};
use dash_pkc::bls::BlsGlobalSig;
use dash_primitives::{CoinbaseCommitment, SpecialPayload};
use dash_types::codec::BaseCodec as _;

use crate::assemble::{RESERVED_SIGOPS, RESERVED_SIZE, TipFacts, coinbase_input, header};
use crate::{LastBlockStats, TemplateError};

/// `CCbTx::Version`.
// Core: evo/cbtx.h:24-30
const CBTX_MERKLE_ROOT_MNLIST: u16 = 1;
const CBTX_MERKLE_ROOT_QUORUMS: u16 = 2;
const CBTX_CLSIG_AND_BALANCE: u16 = 3;
const CBTX_MERKLE_ROOT_ASSETUNLOCKS: u16 = 4;

/// The slice that assembles blocks over registered masternodes and with ChainLocks
/// ([`TemplateError::NotReady`]'s owner).
const REGISTERED_MNS_OWNER: &str = "MN0-3";
/// The slice that pays superblocks (the trigger store).
const SUPERBLOCK_OWNER: &str = "GV1b";
/// The slice that lifts the real-commitment fence: it compares a non-zero quorum root and
/// mined-commitment skips against dashd (MN0-2a's chains have neither).
const COMMITMENTS_OWNER: &str = "MN0-2b";

/// What the evo part of `CreateNewBlock` reads under `cs_main`, at the tip.
pub struct EvoChain<'a> {
    /// `DeploymentActiveAfter` at the tip.
    pub deployments: &'a dyn DeploymentView,
    /// The quorum block processor at the tip (`m_quorum_block_processor`).
    pub qc: &'a QcCtx<'a>,
    /// Its mineable commitments (`minableCommitments`).
    pub pool: &'a MineablePool,
    /// The tip's masternode list. Only an empty one is assembled here.
    pub list: &'a MnList,
    /// Whether `CalcCbTxBestChainlock(pindexPrev)` returns true (`node/miner.cpp:117-171`):
    /// either the parent CbTx carries a non-null ChainLock (`GetNonNullCoinbaseChainlock`, carried
    /// forward with its height difference plus one), or the node's best ChainLock
    /// (`GetBestChainLock`) is at or above the V19 deployment height, at any height up to the
    /// tip, not only the tip's (inserted with `bestCLHeightDiff` = the tip's height minus its
    /// height); a best ChainLock below the V19 height is dropped and counts as `false`. Only
    /// `false` is assembled here: a parent's carried ChainLock written as null is
    /// `bad-cbtx-null-clsig`, and a best one written as null differs from Core's block.
    pub chainlock: bool,
    /// `IsSuperblockTriggered(tip list, nHeight)`, which reads triggers voted on while
    /// masternodes existed; only `false` is assembled here.
    pub superblock_triggered: bool,
    /// `GetCreditPool(pindexPrev)`.
    pub credit_pool: &'a CreditPool,
    pub payments: &'a PaymentParams,
}

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
    pub fn after(deployments: &dyn DeploymentView, prev: BlockId) -> Eras {
        let buried = |d| deployments.active_after(Some(prev), Deployment::Buried(d));
        Eras {
            dip3: buried(BuriedDeployment::Dip0003),
            dip8: buried(BuriedDeployment::Dip0008),
            v20: buried(BuriedDeployment::V20),
            mn_rr: buried(BuriedDeployment::MnRr),
            v24: deployments.active_after(Some(prev), Deployment::Versionbits(DeploymentPos::V24)),
        }
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

/// An assembled block and the counters `CreateNewBlock` sets for it.
pub struct EvoBlock {
    /// The coinbase, then the quorum commitments; `nNonce` 0 and a null merkle root.
    pub block: Block,
    pub stats: LastBlockStats,
}

/// `CreateNewBlock` on an empty mempool once DIP0003 is active after `prev` (the tip, whose
/// facts are `facts`), paying the miner's part to `script`:
///
/// 1. each enabled LLMQ type's mineable commitments, in `GetEnabledQuorumParams` order
///    (`node/miner.cpp:222-247`); an empty list takes them unchanged;
/// 2. the coinbase (`:263-279`, `:279-283`): scriptSig `OP_RETURN`, version 3, type
///    `TRANSACTION_COINBASE`, its first output the subsidy;
/// 3. the CbTx (`:285-333`): the root of the list after the block (empty), from DIP0008 the
///    quorums' root, from V20 a null best ChainLock (no ChainLock on a chain without masternodes)
///    and the credit pool's balance, from version 4 the asset unlocks' root;
/// 4. `FillBlockPayments` (`:337-338`): `GetBlockTxOuts`' outputs, each subtracted from the first
///    (Core's `CAmount` arithmetic; a negative miner output is `TestBlockValidity`'s to refuse).
///
/// `prev` must name the tip `facts` describe.
///
/// # Errors
///
/// [`TemplateError::NotReady`] for:
/// - a non-empty list (`"DIP0003"`, MN0-3: payees, `BuildNewListFromBlock`);
/// - a ChainLock to carry forward (`"V20"`, MN0-3; [`EvoChain::chainlock`]);
/// - a triggered superblock (`"superblock"`, GV1b);
/// - a real commitment still active at the tip (`"mined commitments"`, MN0-2b);
/// - a real commitment from the pool for the block (`"mineable commitments"`, MN0-2b).
///
/// [`TemplateError::Failed`] where Core throws or a read faults.
// Core: node/miner.cpp:176-363 BlockAssembler::CreateNewBlock
pub fn block(
    facts: &TipFacts,
    prev: BlockId,
    script: &[u8],
    chain: &EvoChain<'_>,
) -> Result<EvoBlock, TemplateError> {
    let eras = Eras::after(chain.deployments, prev);
    if !eras.dip3 {
        return Err(failed(format_args!(
            "DIP0003 is not active after the tip at {}",
            facts.height
        )));
    }
    let not_ready = |rule, owner| {
        Err(TemplateError::NotReady {
            height: facts.height,
            rule,
            owner,
        })
    };
    if chain.list.counts().total() != 0 {
        return not_ready("DIP0003", REGISTERED_MNS_OWNER);
    }
    if eras.v20 && chain.chainlock {
        return not_ready("V20", REGISTERED_MNS_OWNER);
    }
    if chain.superblock_triggered {
        return not_ready("superblock", SUPERBLOCK_OWNER);
    }
    // A commitment mined while masternodes existed stays among the active ones (none replaces
    // it): it makes the quorum root non-zero and `HasMinedCommitment` skip quorums.
    let mut qc_cache = QcHashCache::new();
    let mined = qc_hashes(&chain.qc.llmq, chain.qc.mined, &mut qc_cache, prev)
        .map_err(|e| failed(format_args!("GetQcHashes: {e:?}")))?;
    if mined.plain.values().any(|h| !h.is_empty()) || mined.indexed.values().any(|h| !h.is_empty())
    {
        return not_ready("mined commitments", COMMITMENTS_OWNER);
    }
    let height = i32::try_from(facts.height).map_err(|_| failed("height"))?;

    // :222-247. `ApplyTx` of a commitment changes no list without masternodes (Core's
    // `HandleQuorumCommitment` skips members the list lacks), so the list after the block is
    // the tip's; MN0-3 brings the builder back.
    let mut txs = vec![placeholder_coinbase()];
    let mut size = RESERVED_SIZE;
    let mut num_txs = 0;
    let enabled = enabled_quorum_params(&chain.qc.llmq, prev)
        .map_err(|e| failed(format_args!("GetEnabledQuorumParams: {e:?}")))?;
    for params in enabled {
        let qc_txs = chain
            .pool
            .mineable_commitments_tx(chain.qc, params, prev, facts.height)
            .map_err(|e| failed(format_args!("GetMineableCommitmentsTx: {e:?}")))?;
        for tx in qc_txs.into_iter().flatten() {
            // A real commitment the pool holds for a quorum signed before the list emptied.
            if !is_null_commitment(&tx) {
                return not_ready("mineable commitments", COMMITMENTS_OWNER);
            }
            size += u64::from(tx.size());
            num_txs += 1;
            txs.push(tx);
        }
    }
    // :259-261; the commitments have no sigops.
    tracing::info!(
        "CreateNewBlock(): total size {size} txs: {num_txs} fees: 0 sigops {RESERVED_SIGOPS}"
    );

    // :285-333, over the block with its dummy coinbase, which every root skips.
    let header = header(facts);
    let block = Block::new(header, txs);
    let cbtx = build_cbtx(facts, height, eras, prev, &block, chain, &mut qc_cache)?;
    let mut extra_payload = Vec::new();
    cbtx.encode(&mut extra_payload);

    // :337-338
    let rules = PayeeRules {
        v19: chain.deployments.active_after(Some(prev), Deployment::Buried(BuriedDeployment::V19)),
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

/// The CbTx of the block whose other transactions `block` holds.
fn build_cbtx(
    facts: &TipFacts,
    height: i32,
    eras: Eras,
    prev: BlockId,
    block: &Block,
    chain: &EvoChain<'_>,
    qc_cache: &mut QcHashCache,
) -> Result<CoinbaseCommitment, TemplateError> {
    let version = eras.cbtx_version();
    // :297-303: no transaction changes an empty list.
    let merkle_root_mn_list = merkle_root_mn_list_full(chain.list)
        .map_err(|e| failed(format_args!("CalcCbTxMerkleRootMNList failed: {e}")))?;
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
    // :309-315: `CalcCbTxBestChainlock` finds none ([`EvoChain::chainlock`]).
    tracing::info!(
        "CreateNewBlock() h[{}] CbTx failed to find best CL. Inserting null CL",
        facts.height
    );
    cbtx.best_cl_height_diff = Some(0);
    cbtx.best_cl_signature = Some(null_signature());
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

/// `CCbTx::bestCLSignature` left default: Core's `CBLSSignature()` is invalid, and
/// `CBLSWrapper` writes an invalid object as zeros under either scheme.
fn null_signature() -> BlsGlobalSig {
    BlsGlobalSig::decode(&[0; 96]).expect("zeros decode under either scheme")
}

/// Whether `tx`, one of `GetMineableCommitmentsTx`'s, carries a null commitment.
fn is_null_commitment(tx: &Tx) -> bool {
    matches!(
        tx.payload(),
        Some(Ok(decoded)) if matches!(
            &decoded.payload,
            SpecialPayload::QuorumCommitment(p) if p.commitment.is_null()
        )
    )
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
