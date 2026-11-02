//! Block checks (CS3; FR-CON-10, -16, -17): Core's `CheckBlock`, with its merkle and mutation
//! checks, and `ContextualCheckBlock`, with Core's reject reasons, result classes and debug
//! messages.
//!
//! Core caches a passed `CheckBlock` in the block (`fChecked`, `m_checked_merkle_root`); these
//! functions recompute. A caller that checks a block twice may cache the verdict the same way.

use dash_params::BuriedDeployment;
use dash_primitives::{MerkleRoot, SpecialPayload};
use dash_types::Numeric as _;

use crate::Block;
use crate::ctx::{ConsensusCtx, Deployment};
use crate::header::{HeaderParams, check_block_header};
use crate::merkle::compute_merkle_root;
use crate::primitives::canonical::TRANSACTION_COINBASE;
use crate::script::opcodes::{OP_0, OP_1};
use crate::script::{ScriptNum, push_data};
use crate::tx::{
    check_transaction, contextual_check_transaction, is_final_tx, max_block_sigops, max_block_size,
};
use crate::validation::{BlockValidationError, BlockValidationResult};

/// The CbTx version from which the payload carries `merkleRootAssetUnlocks`
/// (`CCbTx::Version::MERKLE_ROOT_ASSETUNLOCKS`, v24).
const CBTX_MERKLE_ROOT_ASSETUNLOCKS: u16 = 4;

fn consensus(reason: &'static str, debug: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, reason).with_debug(debug)
}

fn mutated(reason: &'static str, debug: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Mutated, reason).with_debug(debug)
}

/// Core's `CheckMerkleRoot`: the header commits to the transactions' merkle root, the tree is
/// not mutated (CVE-2012-2459), and a v4+ CbTx commits to the instance hashes of the block's
/// stable-txid asset unlocks. Every failure is `BLOCK_MUTATED`.
// Core: validation.cpp:4324 CheckMerkleRoot
fn check_merkle_root(block: &Block) -> Result<(), BlockValidationError> {
    let (root, is_mutated) = block.merkle();
    if block.header().merkle_root() != root {
        return Err(mutated("bad-txnmrklroot", "hashMerkleRoot mismatch"));
    }
    if is_mutated {
        return Err(mutated("bad-txns-duplicate", "duplicate transaction"));
    }
    // `GetTxPayload<CCbTx>(*vtx[0], /*assert_type=*/false)`: the type alone selects the payload
    // (a non-special version has none, which fails to decode), and it must decode completely.
    let Some(coinbase) = block.txs().first() else {
        return Ok(());
    };
    if coinbase.tx_type().to_base() != TRANSACTION_COINBASE {
        return Ok(());
    }
    if let Some(Ok(decoded)) = coinbase.payload()
        && decoded.is_complete()
        && let SpecialPayload::CoinbaseCommitment(cbtx) = &decoded.payload
        && cbtx.version >= CBTX_MERKLE_ROOT_ASSETUNLOCKS
        && cbtx.merkle_root_asset_unlocks != Some(asset_unlocks_merkle_root(block))
    {
        return Err(mutated(
            "bad-cbtx-assetunlockmerkleroot",
            "asset unlock instance merkle root mismatch",
        ));
    }
    Ok(())
}

/// Core's `CalcCbTxMerkleRootAssetUnlocks`: the merkle root over the instance hashes of the
/// block's stable-txid asset unlocks, in block order (zero when there are none).
// Core: evo/cbtx.cpp:158 CalcCbTxMerkleRootAssetUnlocks (v24)
pub fn asset_unlocks_merkle_root(block: &Block) -> MerkleRoot {
    let leaves = block
        .txs()
        .iter()
        .filter(|tx| tx.is_asset_unlock_with_stable_txid())
        .map(|tx| *tx.instance_hash().as_bytes())
        .collect();
    MerkleRoot::from_lendian(compute_merkle_root(leaves).0)
}

/// Why [`block_mutation`] judged a block mutated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    /// `CheckMerkleRoot` failed; Core logs `"Block mutated: %s\n"` with this state.
    MerkleRoot(BlockValidationError),
    /// No coinbase first, and some transaction serializes to 64 bytes.
    Tx64,
}

/// Core's `IsBlockMutated`, with the reason: [`check_merkle_root`] fails, or the block has no
/// coinbase first and some transaction serializes to 64 bytes. The net processor runs it on a
/// received `block` before it hands the block to validation, and then marks the merkle root
/// checked (P3 D24). This crate does not log, so the caller writes Core's line for
/// [`Mutation::MerkleRoot`].
// Core: validation.cpp:4434-4457 IsBlockMutated
pub fn block_mutation(block: &Block) -> Option<Mutation> {
    if let Err(e) = check_merkle_root(block) {
        return Some(Mutation::MerkleRoot(e));
    }
    match block.txs().first() {
        Some(first) if first.is_coinbase() => None,
        // 3.1 of "Weaknesses in Bitcoin's Merkle Root Construction": such a block is already
        // invalid without a coinbase, so this is not a consensus change.
        _ => block.txs().iter().any(|tx| tx.size() == 64).then_some(Mutation::Tx64),
    }
}

/// Core's `IsBlockMutated`: [`block_mutation`] found a reason.
pub fn is_block_mutated(block: &Block) -> bool {
    block_mutation(block).is_some()
}

/// Core's `CheckBlock`: the checks that need no chain context. The header ([`check_block_header`],
/// with the proof of work when `check_pow`), the merkle root and its mutations when
/// `check_merkle`, the relaxed (DIP0001) size limits, the coinbase position, every transaction's
/// [`check_transaction`] and the relaxed sigop limit.
// Core: validation.cpp:4366 CheckBlock
pub fn check_block(
    block: &Block,
    params: &HeaderParams,
    check_pow: bool,
    check_merkle: bool,
) -> Result<(), BlockValidationError> {
    check_block_header(block.header(), params, check_pow)?;
    if check_merkle {
        check_merkle_root(block)?;
    }

    // All checks that could mean corruption come before the transaction checks, so a block's
    // header is never marked invalid because its transactions were swapped in transit.

    let txs = block.txs();
    let max_size = max_block_size(true);
    if txs.is_empty() || txs.len() > max_size as usize || block.size() > max_size {
        return Err(consensus("bad-blk-length", "size limits failed"));
    }

    if txs.first().is_none_or(|tx| !tx.is_coinbase()) {
        return Err(consensus("bad-cb-missing", "first tx is not coinbase"));
    }
    if txs[1..].iter().any(|tx| tx.is_coinbase()) {
        return Err(consensus("bad-cb-multiple", "more than one coinbase"));
    }

    for tx in txs {
        // Every CheckTransaction failure is TX_CONSENSUS (Core asserts it).
        check_transaction(tx)
            .map_err(|e| e.into_block_wrapped(BlockValidationResult::Consensus, &tx.txid()))?;
    }

    let sigops = txs.iter().fold(0u32, |n, tx| n.wrapping_add(tx.legacy_sigop_count()));
    if sigops > max_block_sigops(true) {
        return Err(consensus("bad-blk-sigops", "out-of-bounds SigOpCount"));
    }
    Ok(())
}

/// Core's `ContextualCheckBlock` for `block` on top of `ctx.prev` (`None` for genesis): the size
/// and sigop limits of the DIP0001 state, finality against the BIP113 cutoff, every transaction's
/// [`contextual_check_transaction`], the coinbase height prefix from BIP34 until DIP0003
/// (quirk 10), and the CbTx type from DIP0003.
///
/// Core reads the coinbase's first input unchecked in the height rule; it runs after
/// [`check_block`], which rejects a block whose first transaction has no input. Here such a
/// block fails the height rule (`bad-cb-height`) instead of reading out of bounds.
// Core: validation.cpp:4530 ContextualCheckBlock
pub fn contextual_check_block(
    block: &Block,
    ctx: &ConsensusCtx,
) -> Result<(), BlockValidationError> {
    let active =
        |deployment| ctx.deployments.active_after(ctx.prev, Deployment::Buried(deployment));
    let height = ctx.height;

    // BIP113: lock times are compared with the previous block's median time past.
    let lock_time_cutoff = if active(BuriedDeployment::Csv) {
        ctx.median_time_past
    } else {
        i64::from(block.header().time())
    };
    let dip0001 = active(BuriedDeployment::Dip0001);
    let dip0003 = active(BuriedDeployment::Dip0003);

    let txs = block.txs();
    let max_size = max_block_size(dip0001);
    if txs.is_empty() || txs.len() > max_size as usize || block.size() > max_size {
        return Err(consensus("bad-blk-length", "size limits failed"));
    }

    // Core's `nHeight` is an `int`; heights fit it (`ChainAncestry`'s arena issues no others).
    let block_height = i32::try_from(height).unwrap_or(i32::MAX);
    let mut sigops = 0u32;
    for tx in txs {
        if !is_final_tx(tx, block_height, lock_time_cutoff) {
            return Err(consensus("bad-txns-nonfinal", "non-final transaction"));
        }
        // Every ContextualCheckTransaction failure is TX_CONSENSUS (Core asserts it).
        contextual_check_transaction(tx, ctx)
            .map_err(|e| e.into_block_wrapped(BlockValidationResult::Consensus, &tx.txid()))?;
        sigops = sigops.wrapping_add(tx.legacy_sigop_count());
    }
    if sigops > max_block_sigops(dip0001) {
        return Err(consensus("bad-blk-sigops", "out-of-bounds SigOpCount"));
    }

    // The coinbase scriptSig starts with the serialized height, compared as a byte prefix. From
    // DIP0003 the height is in the CbTx payload instead (checked by CheckCbTx).
    if active(BuriedDeployment::HeightInCoinbase) && !dip0003 {
        let expect = script_for_height(i64::from(height));
        let script_sig =
            txs[0].inputs().first().map_or(&[][..], |input| input.script_sig.as_bytes());
        if !script_sig.starts_with(&expect) {
            return Err(consensus("bad-cb-height", "block height mismatch in coinbase"));
        }
    }

    if dip0003 && txs[0].tx_type().to_base() != TRANSACTION_COINBASE {
        return Err(consensus("bad-cb-type", "coinbase is not a CbTx"));
    }
    Ok(())
}

/// `CScript() << n`: `OP_0` for 0, `OP_1NEGATE` and `OP_1`..`OP_16` for -1 and 1..=16, otherwise
/// a push of the minimal script number.
// Core: script/script.h:398 CScript::push_int64
fn script_for_height(n: i64) -> Vec<u8> {
    if n == -1 || (1..=16).contains(&n) {
        // OP_1NEGATE is OP_1 - 1; n is in -1..=16, so the sum fits a byte.
        vec![(n + i64::from(OP_1) - 1) as u8]
    } else if n == 0 {
        vec![OP_0]
    } else {
        let mut script = Vec::new();
        push_data(&mut script, &ScriptNum::new(n).encode());
        script
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_params::{ChainSetup, Network};

    use super::*;
    use crate::ctx::{CheckpointView, DeploymentView, bls_ctx_for_block};
    use crate::params::test_support::chain;
    use crate::script::opcodes::{OP_1NEGATE, OP_16};
    use crate::{BlockId, ChainAncestry, Header, HeaderCtx, Tx};

    /// Nothing below genesis: no headers, no checkpoints, and the deployments
    /// `DeploymentActiveAfter(nullptr)` sees (buried ones by height 0, version bits DEFINED).
    struct BelowGenesis<'a>(&'a ChainSetup);

    impl ChainAncestry for BelowGenesis<'_> {
        fn header(&self, id: BlockId) -> HeaderCtx {
            panic!("no header below genesis ({id:?})")
        }
        fn ancestor(&self, _id: BlockId, _height: u32) -> Option<BlockId> {
            None
        }
        fn prev(&self, _id: BlockId) -> Option<BlockId> {
            None
        }
    }

    impl DeploymentView for BelowGenesis<'_> {
        fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
            assert!(prev.is_none(), "only genesis's null pindexPrev");
            match deployment {
                Deployment::Buried(d) => self.0.consensus.deployment_height(d).to_u32() == 0,
                Deployment::Versionbits(_) => false,
            }
        }
        fn active_at(&self, block: BlockId, _deployment: Deployment) -> bool {
            panic!("no block below genesis ({block:?})")
        }
    }

    impl CheckpointView for BelowGenesis<'_> {
        fn highest_checkpoint_in_index(&self) -> Option<u32> {
            None
        }
    }

    /// Core runs `ContextualCheckBlock` on genesis with a null `pindexPrev` (`LoadGenesisBlock`
    /// → `AcceptBlock`): every network's genesis passes both block checks there.
    #[test]
    fn genesis_passes_the_block_checks() {
        for network in [Network::Main, Network::Testnet3, Network::Regtest, Network::Devnet] {
            let params = chain(network);
            let genesis = &crate::params::genesis_block(&params);
            let txs = genesis.transactions.iter().map(|tx| Arc::new(Tx::new(tx.clone()))).collect();
            let block = Block::new(Header::new(genesis.header), txs);
            let header_params = HeaderParams::from_chain_params(&params).expect("pow params");
            assert_eq!(check_block(&block, &header_params, true, true), Ok(()), "{network:?}");
            let view = BelowGenesis(&params);
            let ctx = ConsensusCtx {
                params: &params,
                height: 0,
                prev: None,
                chain: &view,
                deployments: &view,
                checkpoints: &view,
                bls: bls_ctx_for_block(&params, 0),
                median_time_past: 0,
                header_check_time: 0,
            };
            assert_eq!(contextual_check_block(&block, &ctx), Ok(()), "{network:?}");
        }
    }

    /// `CScript() << n` as Core builds the BIP34 prefix (quirk 10's encoding).
    #[test]
    fn height_scripts() {
        assert_eq!(script_for_height(0), [OP_0]);
        assert_eq!(script_for_height(-1), [OP_1NEGATE]);
        assert_eq!(script_for_height(1), [OP_1]);
        assert_eq!(script_for_height(16), [OP_16]);
        assert_eq!(script_for_height(17), [1, 17]);
        assert_eq!(script_for_height(128), [2, 0x80, 0]);
        assert_eq!(script_for_height(951), [2, 0xb7, 0x03]);
        assert_eq!(script_for_height(1_028_160), [3, 0x40, 0xb0, 0x0f]);
    }
}
