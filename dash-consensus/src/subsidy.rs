//! The block subsidy, its treasury share and the superblock payments limit (CS2 §4).
//!
//! Core computes the subsidy from the *previous* block's `nBits` and height (quirk 11). From V20
//! on, and on devnets past the high-subsidy window, the base is a fixed 5 DASH. Before that it
//! comes from one of three floating-point formulas of the previous block's difficulty, in `fp`;
//! everything else is integer arithmetic.

use core::fmt;

use dash_params::{ChainSetup, Network};
use dash_primitives::Block;
use dash_types::Numeric;

use crate::fp::Difficulty;
use crate::params::{ParamsError, Superblocks, check_devnet_arguments, height};
use crate::{Amount, HeaderCtx};

/// Core's `COIN`: duffs per DASH.
pub const COIN: Amount = 100_000_000;

/// The base subsidy, in DASH, from V20 on (and on devnets past the high-subsidy window).
const FIXED_BASE: Amount = 5;

/// Which of the `NetworkIDString()` checks in Core's `GetBlockSubsidyHelper` hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubsidyNetwork {
    /// Mainnet: the difficulty of blocks up to 4500 is computed with Core's early bug.
    Main,
    /// A devnet: the high-subsidy window applies, and the base is fixed after it.
    Devnet,
    /// Testnet or regtest.
    Other,
}

/// The fields of Core's chain parameters the subsidy reads, and the values Core derives from
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubsidyConfig {
    /// `nSubsidyHalvingInterval`: the decline period (150 on regtest, 210240 elsewhere).
    pub halving_interval: i32,
    /// `nBudgetPaymentsStartBlock` (regtest `-budgetparams`).
    pub budget_payments_start: i32,
    /// `nHighSubsidyBlocks` (devnet `-highsubsidyblocks`; 0 elsewhere).
    pub high_subsidy_blocks: i32,
    /// `nHighSubsidyFactor` (devnet `-highsubsidyfactor`; 1 elsewhere).
    pub high_subsidy_factor: i32,
    /// The network checks.
    pub network: SubsidyNetwork,
    /// `GenesisBlock().vtx[0]->GetValueOut()`, `GetBlockSubsidy`'s answer for the genesis block.
    pub genesis_reward: Amount,
    /// `nSuperblockStartBlock` and `nSuperblockCycle` (regtest `-budgetparams`).
    pub superblocks: Superblocks,
    /// `V20Height`: `GetPaymentsLimit` compares heights with it rather than asking the
    /// deployment.
    pub v20_height: i32,
    /// The `nBits` `GetPaymentsLimit` passes for every superblock: the powLimit compact where
    /// `fPowAllowMinDifficultyBlocks`, else 1 (quirk 15).
    // Core: governance/superblock.cpp:156 (v23.1.8: governance/classes.cpp:152)
    pub payments_limit_bits: u32,
}

/// Validated subsidy parameters (CS2 §2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubsidyParams {
    config: SubsidyConfig,
}

/// Core's `{nSubsidy - nSuperblockPart, nSuperblockPart}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SubsidySplit {
    /// What the block itself may pay out (`GetBlockSubsidyInner`).
    pub block: Amount,
    /// The treasury's share, paid out in superblocks (`GetSuperblockSubsidyInner`).
    pub treasury: Amount,
}

/// An input Core's subsidy code has no answer for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubsidyError {
    /// Core aborts: `assert(isDevnet)` for a previous height below `nHighSubsidyBlocks` off a
    /// devnet. No block reaches it; `superblock_payments_limit` of a height at or below 0
    /// does.
    Unreachable,
}

impl fmt::Display for SubsidyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreachable => f.write_str("an input Core asserts on"),
        }
    }
}

impl std::error::Error for SubsidyError {}

impl SubsidyParams {
    /// The parameters of `chain` (CS2 §3.1).
    ///
    /// `genesis_reward` is the sum of the genesis coinbase's outputs ([`genesis_reward`] of
    /// `chain.genesis`); it is only `GetBlockSubsidy`'s answer for the genesis block, never a
    /// consensus value.
    pub fn from_chain_params(
        chain: &ChainSetup,
        genesis_reward: Amount,
    ) -> Result<Self, ParamsError> {
        check_devnet_arguments(chain)?;
        let c = &chain.consensus;
        if c.subsidy_halving_interval <= 0 {
            return Err(ParamsError::HalvingInterval);
        }
        Ok(Self {
            config: SubsidyConfig {
                halving_interval: c.subsidy_halving_interval,
                budget_payments_start: height(c.budget_payments_start_block)?,
                high_subsidy_blocks: c.high_subsidy_blocks,
                high_subsidy_factor: c.high_subsidy_factor,
                // Core: validation.cpp:1814 and 1821 (v23.1.8: 1488, 1495), NetworkIDString()
                network: match chain.network {
                    Network::Main => SubsidyNetwork::Main,
                    Network::Devnet => SubsidyNetwork::Devnet,
                    Network::Testnet3 | Network::Regtest => SubsidyNetwork::Other,
                },
                genesis_reward,
                superblocks: Superblocks::from_chain_params(chain)?,
                v20_height: height(c.v20_height)?,
                // Core: governance/superblock.cpp:156 (v23.1.8: governance/classes.cpp:152)
                payments_limit_bits: if c.pow_allow_min_difficulty_blocks {
                    c.pow_limit.compact(false).to_base()
                } else {
                    1
                },
            },
        })
    }

    /// The validated values.
    pub fn config(&self) -> &SubsidyConfig {
        &self.config
    }
}

/// The value of a genesis block's coinbase, `GenesisBlock().vtx[0]->GetValueOut()`: the
/// `genesis_reward` of [`SubsidyParams::from_chain_params`].
pub fn genesis_reward(genesis: &Block) -> Result<Amount, ParamsError> {
    let coinbase = genesis.transactions.first().ok_or(ParamsError::GenesisReward)?;
    coinbase
        .outputs
        .iter()
        .try_fold(0, |sum: Amount, out| sum.checked_add_unsigned(out.amount()?.to_sat()))
        .ok_or(ParamsError::GenesisReward)
}

/// Core's `GetBlockSubsidyHelper`: the subsidy of the block after one with `prev_bits` at
/// `prev_height`, split into the block's and the treasury's part.
// Core: validation.cpp:1809 GetBlockSubsidyHelper (v23.1.8: 1483)
pub fn block_subsidy_helper(
    prev_bits: u32,
    prev_height: i32,
    v20: bool,
    p: &SubsidyParams,
) -> Result<SubsidySplit, SubsidyError> {
    let c = &p.config;
    let is_devnet = c.network == SubsidyNetwork::Devnet;
    // Core: validation.cpp:1858 (v23.1.8: 1532). Checked first: the code before it has no
    // effect, and Core aborts either way.
    if prev_height < c.high_subsidy_blocks && !is_devnet {
        return Err(SubsidyError::Unreachable);
    }

    // Core: validation.cpp:1822 (v23.1.8: 1496). Devnets get the fixed base after the
    // high-subsidy window to mimic mainnet.
    let fixed_base = v20 || (is_devnet && prev_height >= c.high_subsidy_blocks);
    let base = if fixed_base {
        FIXED_BASE
    } else {
        pre_v20_base(prev_bits, prev_height, c.network)
    };

    let mut subsidy = decline(base * COIN, prev_height, c.halving_interval);

    if prev_height < c.high_subsidy_blocks {
        // Only on a devnet (checked above). Core accepts any int factor; where the product
        // leaves int64 its behaviour is undefined and the release binaries' `imul` wraps.
        subsidy = subsidy.wrapping_mul(Amount::from(c.high_subsidy_factor));
    }

    // Core: validation.cpp:1865 (v23.1.8: 1539). The treasury share starts after
    // nBudgetPaymentsStartBlock (strictly: mainnet's first is block 328010) and doubles to 20%
    // with V20.
    let divisor = if v20 {
        5
    } else {
        10
    };
    let treasury = if prev_height > c.budget_payments_start {
        subsidy / divisor
    } else {
        0
    };
    Ok(SubsidySplit {
        block: subsidy - treasury,
        treasury,
    })
}

/// Core's yearly decline of production by ~7.1%: one pass of `subsidy -= subsidy / 14` per
/// positive multiple of `interval` up to `prev_height`, each truncating (quirk 11).
///
/// Once `subsidy / 14` is 0 the value is a fixed point, so the loop stops there: the result is
/// the same, and regtest heights near 2^31 take about 300 passes instead of 14 million.
// Core: validation.cpp:1854 (v23.1.8: 1528)
fn decline(mut subsidy: Amount, prev_height: i32, interval: i32) -> Amount {
    for _ in 0..prev_height.max(0) / interval {
        let step = subsidy / 14;
        if step == 0 {
            break;
        }
        subsidy -= step;
    }
    subsidy
}

/// The pre-V20 base subsidy, in DASH: one of three formulas of the previous block's difficulty,
/// chosen by its height and the difficulty, each truncated and then clamped (CS2 §4.2 steps 1
/// and 2, quirk 11). `prev_bits` is the raw word: on mainnet up to height 4500 the difficulty
/// ignores the size byte, so different encodings of one target give different bases.
// Core: validation.cpp:1813-1849 (v23.1.8: 1487-1523)
fn pre_v20_base(prev_bits: u32, prev_height: i32, network: SubsidyNetwork) -> Amount {
    let difficulty = if prev_height <= 4500 && network == SubsidyNetwork::Main {
        // "a bug which caused diff to not be correctly calculated"
        Difficulty::from_bits_mainnet_early(prev_bits)
    } else {
        Difficulty::from_bits(prev_bits)
    };
    if prev_height < 5465 {
        // Early ages: 1111/((x+1)^2)
        clamp(difficulty.subsidy_base_early(), 1, 500)
    } else if prev_height < 17000 || (difficulty.at_most_75() && prev_height < 24000) {
        // CPU mining era: 11111/(((x+51)/6)^2)
        clamp(difficulty.subsidy_base_cpu(), 25, 500)
    } else {
        // GPU/ASIC mining era: 2222222/(((x+2600)/9)^2)
        clamp(difficulty.subsidy_base_gpu(), 5, 25)
    }
}

/// Core's clamp of a base, after its truncation: `if (x > hi) x = hi; else if (x < lo) x = lo;`.
fn clamp(base: Amount, lo: Amount, hi: Amount) -> Amount {
    if base > hi {
        hi
    } else if base < lo {
        lo
    } else {
        base
    }
}

/// Core's `GetBlockSubsidyInner`: the block's part of the subsidy.
// Core: validation.cpp:1878 GetBlockSubsidyInner (v23.1.8: 1552)
pub fn block_subsidy_inner(
    prev_bits: u32,
    prev_height: i32,
    v20: bool,
    p: &SubsidyParams,
) -> Result<Amount, SubsidyError> {
    block_subsidy_helper(prev_bits, prev_height, v20, p).map(|split| split.block)
}

/// Core's `GetSuperblockSubsidyInner`: the treasury's part of the subsidy.
// Core: validation.cpp:1872 GetSuperblockSubsidyInner (v23.1.8: 1546)
pub fn superblock_subsidy_inner(
    prev_bits: u32,
    prev_height: i32,
    v20: bool,
    p: &SubsidyParams,
) -> Result<Amount, SubsidyError> {
    block_subsidy_helper(prev_bits, prev_height, v20, p).map(|split| split.treasury)
}

/// Core's `GetBlockSubsidy`: the subsidy of the block after `prev` (its raw `nBits` and its
/// height), or of the genesis block for `None`. `v20_at_block` is `DeploymentActiveAt(block,
/// V20)`.
// Core: validation.cpp:1884 GetBlockSubsidy (v23.1.8: 1558)
pub fn block_subsidy(
    prev: Option<&HeaderCtx>,
    v20_at_block: bool,
    p: &SubsidyParams,
) -> Result<Amount, SubsidyError> {
    let Some(prev) = prev else {
        return Ok(p.config.genesis_reward);
    };
    // A height above Core's int cannot be in a block index.
    let prev_height = i32::try_from(prev.height).map_err(|_| SubsidyError::Unreachable)?;
    block_subsidy_inner(prev.bits.to_base(), prev_height, v20_at_block, p)
}

/// Core's `CSuperblock::GetPaymentsLimit`: the most a superblock at `height` may pay, 0 off
/// superblock heights.
///
/// It is a subsidy formula, not a difficulty one: every superblock uses
/// [`SubsidyConfig::payments_limit_bits`] in place of the real `nBits` (quirk 15).
// Core: governance/superblock.cpp:145 CSuperblock::GetPaymentsLimit (v23.1.8:
// governance/classes.cpp:141)
pub fn superblock_payments_limit(height: i32, p: &SubsidyParams) -> Result<Amount, SubsidyError> {
    let c = &p.config;
    if !c.superblocks.is_superblock_height(height) {
        return Ok(0);
    }
    let v20 = height >= c.v20_height;
    let prev_height = height.checked_sub(1).ok_or(SubsidyError::Unreachable)?;
    let treasury = superblock_subsidy_inner(c.payments_limit_bits, prev_height, v20, p)?;
    // `CAmount * int`. Only a devnet's high-subsidy factor can make this overflow, where Core's
    // behaviour is undefined; the release binaries' `imul` wraps.
    Ok(treasury.wrapping_mul(Amount::from(c.superblocks.cycle())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::test_support::{chain, devnet_chain, header_ctx};

    /// Core's loop, verbatim: `for (int i = interval; i <= nPrevHeight; i += interval)`.
    fn core_decline(mut subsidy: Amount, prev_height: i32, interval: i32) -> Amount {
        let mut i = i64::from(interval);
        while i <= i64::from(prev_height) {
            subsidy -= subsidy / 14;
            i += i64::from(interval);
        }
        subsidy
    }

    #[test]
    fn decline_matches_core_loop_over_the_regtest_height_range() {
        // Regtest's interval (150) gives the most passes: up to i32::MAX / 150, about 14.3M.
        // The bases: the formulas' clamp bounds and the fixed 5 DASH (the devnet factor comes
        // after the loop).
        for base in [1, 5, 25, 153, 500] {
            let start = base * COIN;
            // the plain sequence, pass by pass, over the whole height range
            let mut plain = start;
            let mut fixed_at = None;
            for passes in 0..=(i32::MAX / 150) {
                let h = passes * 150;
                if fixed_at.is_none() || passes % 9973 == 0 {
                    assert_eq!(decline(start, h, 150), plain, "base {base} passes {passes}");
                    assert_eq!(
                        decline(start, h + 149, 150),
                        plain,
                        "base {base} height {}",
                        h + 149
                    );
                }
                let next = plain - plain / 14;
                if next == plain && fixed_at.is_none() {
                    fixed_at = Some(passes);
                }
                plain = next;
            }
            assert!(
                fixed_at.is_some_and(|p| p < 400),
                "base {base}: fixed point after {fixed_at:?} passes"
            );
            assert_eq!(decline(start, i32::MAX, 150), plain);
        }
    }

    fn params(chain: &ChainSetup) -> Result<SubsidyParams, ParamsError> {
        SubsidyParams::from_chain_params(chain, 50 * COIN)
    }

    #[test]
    fn rejects_what_core_cannot_run() {
        let check = |f: fn(&mut ChainSetup), err| {
            let mut chain = chain(Network::Main);
            f(&mut chain);
            assert_eq!(params(&chain), Err(err));
        };
        check(|c| c.consensus.subsidy_halving_interval = 0, ParamsError::HalvingInterval);
        check(|c| c.consensus.superblock_cycle = 0, ParamsError::SuperblockCycle);
        check(|c| c.consensus.v20_height = (1u32 << 31).into(), ParamsError::Height);
        check(|c| c.consensus.high_subsidy_blocks = 1000, ParamsError::DevnetOverridesOffDevnet);
        // Core runs a devnet with any int factor
        let mut devnet = devnet_chain();
        devnet.consensus.high_subsidy_blocks = 1000;
        for high_subsidy_factor in [i32::MIN, -1, 0, i32::MAX] {
            devnet.consensus.high_subsidy_factor = high_subsidy_factor;
            let p = params(&devnet).unwrap();
            assert_eq!(p.config().network, SubsidyNetwork::Devnet);
            assert_eq!(p.config().high_subsidy_factor, high_subsidy_factor);
        }
    }

    /// The values Core derives per network: the `NetworkIDString()` checks and quirk 15's
    /// `nBits`. Core's own evidence for them is in the recorded corpus (`tests/`).
    #[test]
    fn derived_per_network() {
        let main = chain(Network::Main);
        let p = SubsidyParams::from_chain_params(
            &main,
            genesis_reward(&crate::params::genesis_block(&main)).unwrap(),
        )
        .unwrap();
        assert_eq!(p.config().network, SubsidyNetwork::Main);
        assert_eq!(p.config().payments_limit_bits, 1);
        assert_eq!(p.config().genesis_reward, 50 * COIN);
        for network in [Network::Testnet3, Network::Regtest] {
            let c = chain(network);
            let p = params(&c).unwrap();
            assert_eq!(p.config().network, SubsidyNetwork::Other);
            let limit = c.consensus.pow_limit.compact(false).to_base();
            assert_eq!(p.config().payments_limit_bits, limit);
        }
        let p = params(&chain(Network::Devnet)).unwrap();
        assert_eq!(p.config().network, SubsidyNetwork::Devnet);
    }

    #[test]
    fn heights_core_cannot_hold() {
        let p = params(&chain(Network::Main)).unwrap();
        let prev = header_ctx(u32::MAX, 0, 1);
        assert_eq!(block_subsidy(Some(&prev), true, &p), Err(SubsidyError::Unreachable));
        // GetPaymentsLimit at height 0, a superblock height under -budgetparams=...:0, asks
        // for the subsidy after height -1, where Core asserts
        let mut chain = chain(Network::Regtest);
        chain.consensus.superblock_start.0 = 0u32.into();
        let p = params(&chain).unwrap();
        assert_eq!(superblock_payments_limit(0, &p), Err(SubsidyError::Unreachable));
    }

    #[test]
    fn decline_equals_core_loop_on_short_ranges() {
        for interval in [1, 150, 210_240] {
            for h in (-3..2000).chain([210_239, 210_240, 210_241, 420_480, 2_000_000]) {
                assert_eq!(
                    decline(5 * COIN, h, interval),
                    core_decline(5 * COIN, h, interval),
                    "{interval} {h}"
                );
            }
        }
    }
}
