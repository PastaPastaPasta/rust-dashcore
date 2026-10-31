//! Header checks (CS3; FR-CON-17): Core's `CheckBlockHeader` and `ContextualCheckBlockHeader`,
//! with Core's reject reasons, result classes and debug messages.

use std::borrow::Cow;

use dash_num::CompactTarget;
use dash_params::{BuriedDeployment, ChainParams};

use crate::Header;
use crate::ctx::{ConsensusCtx, Deployment};
use crate::params::ParamsError;
use crate::pow::{PowParams, check_proof_of_work, get_next_work_required, nbits_acceptable};
use crate::validation::{BlockValidationError, BlockValidationResult};

/// `MAX_FUTURE_BLOCK_TIME`: how far past the node's clock a header's time may be.
// Core: chain.h:24
pub const MAX_FUTURE_BLOCK_TIME: i64 = 2 * 60 * 60;

/// What the header and block checks read from the chain parameters: the proof-of-work view, and
/// on a devnet the two genesis hashes of `CheckBlockHeader`'s devnet rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderParams {
    pow: PowParams,
    devnet_genesis: Option<DevnetGenesis>,
}

/// `hashGenesisBlock` and a devnet's `hashDevnetGenesisBlock`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DevnetGenesis {
    genesis: [u8; 32],
    devnet_genesis: [u8; 32],
}

impl HeaderParams {
    /// The view of `chain`; fails where [`PowParams::from_chain_params`] does.
    pub fn from_chain_params(chain: &ChainParams) -> Result<Self, ParamsError> {
        let c = &chain.consensus;
        // Core tests `hashDevnetGenesisBlock.IsNull()`: only a devnet sets it.
        let devnet_genesis = (!c.hash_devnet_genesis_block.is_null()).then(|| DevnetGenesis {
            genesis: *c.hash_genesis_block.as_bytes(),
            devnet_genesis: *c.hash_devnet_genesis_block.as_bytes(),
        });
        Ok(Self {
            pow: PowParams::from_chain_params(chain)?,
            devnet_genesis,
        })
    }

    /// The proof-of-work view.
    pub fn pow(&self) -> &PowParams {
        &self.pow
    }
}

/// Core's `CheckBlockHeader`: the proof of work (when `check_pow`) and, on a devnet, that a block
/// on top of the genesis block is the devnet's genesis block.
// Core: validation.cpp:4307 CheckBlockHeader
pub fn check_block_header(
    header: &Header,
    params: &HeaderParams,
    check_pow: bool,
) -> Result<(), BlockValidationError> {
    let hash = header.hash();
    if check_pow
        && !check_proof_of_work(hash.as_bytes(), CompactTarget::new(header.bits()), &params.pow)
    {
        return Err(BlockValidationError::new(BlockValidationResult::InvalidHeader, "high-hash")
            .with_debug("proof of work failed"));
    }
    if let Some(devnet) = &params.devnet_genesis
        && header.prev_hash().as_bytes() == &devnet.genesis
        && hash.as_bytes() != &devnet.devnet_genesis
    {
        return Err(BlockValidationError::new(BlockValidationResult::Consensus, "devnet-genesis"));
    }
    Ok(())
}

/// Core's `ContextualCheckBlockHeader` for `header` on top of `ctx.prev`: difficulty,
/// checkpoints, the median time past, the future-time limit and the version minimums.
///
/// `ctx.header_check_time` is the node's clock in whole seconds (v24: `NodeClock::now()`,
/// mockable, without a peer offset; `time-too-new`). Truncating Core's sub-second clock does not
/// change the comparison, since header times are whole seconds.
///
/// # Panics
///
/// When `ctx.prev` is `None`: Core asserts a previous block (genesis is never checked here).
// Core: validation.cpp:4469 ContextualCheckBlockHeader
pub fn contextual_check_block_header(
    header: &Header,
    ctx: &ConsensusCtx,
    params: &HeaderParams,
) -> Result<(), BlockValidationError> {
    let Some(prev) = ctx.prev else {
        panic!("ContextualCheckBlockHeader needs a previous block");
    };
    let height = ctx.height;

    // The proof of work. On mainnet up to 68589 the bits need only be near the expected ones
    // (quirk 9), and that rejection carries no debug message.
    let bits = CompactTarget::new(header.bits());
    let expected = get_next_work_required(ctx.chain, prev, header.time(), &params.pow);
    if !nbits_acceptable(height, bits, expected, &params.pow) {
        let rejection = invalid_header("bad-diffbits");
        return Err(if params.pow.config().tolerance_applies(height) {
            rejection
        } else {
            rejection.with_debug(format!("incorrect proof of work at {height}"))
        });
    }

    // No fork below the highest checkpoint the index holds (none when checkpoints are off).
    if let Some(checkpoint) = ctx.checkpoints.highest_checkpoint_in_index()
        && height < checkpoint
    {
        return Err(BlockValidationError::new(
            BlockValidationResult::Checkpoint,
            "bad-fork-prior-to-checkpoint",
        ));
    }

    let time = i64::from(header.time());
    if time <= ctx.median_time_past {
        return Err(invalid_header("time-too-old").with_debug(format!(
            "block's timestamp is too early {time} {}",
            ctx.median_time_past
        )));
    }

    let limit = ctx.header_check_time.saturating_add(MAX_FUTURE_BLOCK_TIME);
    if time > limit {
        return Err(BlockValidationError::new(BlockValidationResult::TimeFuture, "time-too-new")
            .with_debug(format!("block timestamp too far in the future {time} {limit}")));
    }

    let version = header.version();
    let active =
        |deployment| ctx.deployments.active_after(Some(prev), Deployment::Buried(deployment));
    if (version < 2 && active(BuriedDeployment::HeightInCoinbase))
        || (version < 3 && active(BuriedDeployment::DerSig))
        || (version < 4 && active(BuriedDeployment::Cltv))
    {
        // `%08x` of an `int32_t` prints its two's complement bits.
        let version = version as u32;
        return Err(invalid_header(format!("bad-version(0x{version:08x})"))
            .with_debug(format!("rejected nVersion=0x{version:08x} block")));
    }
    Ok(())
}

fn invalid_header(reason: impl Into<Cow<'static, str>>) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::InvalidHeader, reason)
}
