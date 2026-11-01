//! What CS2's parameter views are built from (CS2 §3.1): `dash-params`' [`ChainSetup`],
//! including its devnet arguments and hard-coded pins, and the superblock schedule the subsidy and
//! the payment views share ([`Superblocks`]).
//!
//! Every view ([`PowParams`](crate::pow::PowParams), [`SubsidyParams`](crate::subsidy::SubsidyParams),
//! [`PaymentParams`](crate::payments::PaymentParams)) has one constructor, `from_chain_params`. It
//! reads the network from `ChainSetup::network` and the values Core hard-codes from
//! `ChainSetup::pins` or the network: the mainnet ≤ 68589 tolerance, `GetPaymentsLimit`'s `nBits`
//! (quirk 15) and the subsidy's mainnet and devnet paths. A devnet, or a regtest with
//! `-budgetparams` or `-testactivationheight`, is a [`ChainSetup`] built with those options.

use core::fmt;

use dash_params::{ChainSetup, Network};

/// Core's defaults of the devnet-only consensus arguments: `nMinimumDifficultyBlocks`,
/// `nHighSubsidyBlocks` and `nHighSubsidyFactor`.
// Core: consensus/params.h:192-194 (v23.1.8: 180-182)
const DEVNET_DEFAULTS: (i32, i32, i32) = (0, 0, 1);

/// `chain`'s genesis block. dash-params holds its fields (Core's `CreateGenesisBlock` arguments),
/// not the block.
// Core: chainparams.cpp:78 (CreateGenesisBlock; v23.1.8 the same)
pub fn genesis_block(chain: &ChainSetup) -> dash_primitives::Block {
    dash_primitives::Block::from(&chain.genesis)
}

/// A devnet's devnet genesis block, the block at height 1 that names it (Core's
/// `FindDevNetGenesisBlock`); `None` on other chains.
// Core: chainparams.cpp:43, 85 (CreateDevNetGenesisBlock, FindDevNetGenesisBlock; v23.1.8 the same)
pub fn devnet_genesis_block(chain: &ChainSetup) -> Option<dash_primitives::Block> {
    chain.devnet_genesis.as_ref().map(dash_primitives::Block::from)
}

/// Refuses devnet arguments other than Core's defaults on a chain that is not a devnet: Core reads
/// them only on a devnet (`chainparams.cpp:1251-1255`), so no other chain can have them.
pub(crate) fn check_devnet_arguments(chain: &ChainSetup) -> Result<(), ParamsError> {
    let c = &chain.consensus;
    let set = (c.minimum_difficulty_blocks, c.high_subsidy_blocks, c.high_subsidy_factor);
    if chain.network != Network::Devnet && set != DEVNET_DEFAULTS {
        return Err(ParamsError::DevnetOverridesOffDevnet);
    }
    Ok(())
}

/// Chain parameters Core's proof-of-work, subsidy or payment code cannot run with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamsError {
    /// `nPowTargetSpacing` is outside `[1, i64::MAX / 72]`. Core rejects a spacing below 1 at
    /// startup; above the bound DGW's `nTargetTimespan * 3` overflows.
    TargetSpacing,
    /// `nPowTargetTimespan` is outside `[1, i64::MAX / 7]` (BTC's `* 4`, KGW's `* 7`). Core's
    /// value is 86400 on every network.
    TargetTimespan,
    /// The BTC rule is reachable but `nPowTargetTimespan / nPowTargetSpacing` is 0, so Core's
    /// `% DifficultyAdjustmentInterval()` divides by zero.
    AdjustmentInterval,
    /// KGW is reachable with a window (`PastBlocksMax`) outside 1..=4032 blocks, the domain of
    /// its `pow` table; 0 would mean no limit.
    KgwWindow,
    /// `nSubsidyHalvingInterval` is not positive, so Core's decline loop never ends.
    HalvingInterval,
    /// `nSuperblockCycle` is not positive (Core divides by it).
    SuperblockCycle,
    /// Devnet arguments other than the defaults on a network that is not a devnet.
    DevnetOverridesOffDevnet,
    /// The `nBits` tolerance pin is missing on mainnet or set elsewhere: Core keys it on
    /// `NetworkIDString() == MAIN`.
    TolerancePin,
    /// A height does not fit Core's `int`.
    Height,
    /// The genesis coinbase's outputs do not sum to a `CAmount`.
    GenesisReward,
}

impl fmt::Display for ParamsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TargetSpacing => "nPowTargetSpacing outside [1, i64::MAX / 72]",
            Self::TargetTimespan => "nPowTargetTimespan outside [1, i64::MAX / 7]",
            Self::AdjustmentInterval => {
                "nPowTargetTimespan / nPowTargetSpacing is 0 where the BTC rule is reachable"
            }
            Self::KgwWindow => "KGW is reachable with a window outside 1..=4032 blocks",
            Self::HalvingInterval => "nSubsidyHalvingInterval is not positive",
            Self::SuperblockCycle => "nSuperblockCycle is not positive",
            Self::DevnetOverridesOffDevnet => "devnet arguments set off a devnet",
            Self::TolerancePin => "the nBits tolerance pin does not match the network",
            Self::Height => "height outside Core's int range",
            Self::GenesisReward => "the genesis reward does not fit CAmount",
        })
    }
}

impl std::error::Error for ParamsError {}

/// A height of `dash-params` as Core's `int`.
pub(crate) fn height(h: impl Into<u32>) -> Result<i32, ParamsError> {
    i32::try_from(h.into()).map_err(|_| ParamsError::Height)
}

/// Core's superblock schedule: `nSuperblockStartBlock` and `nSuperblockCycle`.
///
/// The subsidy's payments limit and the masternode payment's reallocation start both read the
/// cycle. Both views take it from this one constructor, so built from one `ChainSetup` they
/// agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Superblocks {
    start: i32,
    cycle: i32,
}

impl Superblocks {
    pub(crate) fn from_chain_params(chain: &ChainSetup) -> Result<Self, ParamsError> {
        let c = &chain.consensus;
        if c.superblock_cycle <= 0 {
            return Err(ParamsError::SuperblockCycle);
        }
        Ok(Self {
            start: height(c.superblock_start.0)?,
            cycle: c.superblock_cycle,
        })
    }

    /// `nSuperblockStartBlock`.
    pub fn start(&self) -> i32 {
        self.start
    }

    /// `nSuperblockCycle`, positive.
    pub fn cycle(&self) -> i32 {
        self.cycle
    }

    /// Core's `CSuperblock::IsValidBlockHeight`.
    // Core: governance/superblock.cpp:119 CSuperblock::IsValidBlockHeight (v23.1.8:
    // governance/classes.cpp:115)
    pub fn is_superblock_height(&self, height: i32) -> bool {
        height >= self.start && height % self.cycle == 0
    }
}

/// Fixtures for the crate's unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use dash_num::{Arith256, CompactTarget};
    use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
    use dash_primitives::BlockHash;
    use dash_types::Numeric;

    use crate::HeaderCtx;

    /// The parameters of `network` without options. The views read nothing that differs between
    /// the releases.
    pub(crate) fn chain(network: Network) -> ChainSetup {
        ChainParamsBuilder::new(network, Release::V24_0).build().expect("Core's parameters")
    }

    /// Mainnet's parameters on a devnet, with a devnet's pins.
    pub(crate) fn devnet_chain() -> ChainSetup {
        let mut chain = chain(Network::Main);
        chain.network = Network::Devnet;
        chain.pins = self::chain(Network::Devnet).pins;
        chain
    }

    /// A header with the fields `pow` and `subsidy` read; the others zero.
    pub(crate) fn header_ctx(height: u32, time: u32, bits: u32) -> HeaderCtx {
        HeaderCtx {
            height,
            hash: BlockHash::from([0; 32]),
            time,
            bits: CompactTarget::new(bits),
            version: 0,
            mtp: 0,
            chain_work: Arith256::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_params::Network;

    use super::test_support::{chain, devnet_chain};
    use super::*;

    #[test]
    fn devnet_arguments_only_on_devnets() {
        let mut devnet = devnet_chain();
        devnet.consensus.high_subsidy_blocks = 1;
        assert_eq!(check_devnet_arguments(&devnet), Ok(()));
        for network in [Network::Main, Network::Testnet3, Network::Regtest] {
            let mut p = chain(network);
            assert_eq!(check_devnet_arguments(&p), Ok(()));
            p.consensus.high_subsidy_blocks = 1;
            assert_eq!(check_devnet_arguments(&p), Err(ParamsError::DevnetOverridesOffDevnet));
            let mut p = chain(network);
            p.consensus.minimum_difficulty_blocks = -1;
            assert_eq!(check_devnet_arguments(&p), Err(ParamsError::DevnetOverridesOffDevnet));
            let mut p = chain(network);
            p.consensus.high_subsidy_factor = 2;
            assert_eq!(check_devnet_arguments(&p), Err(ParamsError::DevnetOverridesOffDevnet));
        }
    }

    #[test]
    fn superblocks() {
        let mut chain = chain(Network::Main);
        let s = Superblocks::from_chain_params(&chain).unwrap();
        assert_eq!((s.start(), s.cycle()), (614_820, 16_616));
        // the first multiple of the cycle at or above the start; the one below it is too early
        assert!(s.is_superblock_height(38 * 16_616) && s.is_superblock_height(39 * 16_616));
        assert!(!s.is_superblock_height(37 * 16_616) && !s.is_superblock_height(38 * 16_616 + 1));
        chain.consensus.superblock_cycle = 0;
        assert_eq!(Superblocks::from_chain_params(&chain), Err(ParamsError::SuperblockCycle));
        chain.consensus.superblock_cycle = 1;
        chain.consensus.superblock_start.0 = (1u32 << 31).into();
        assert_eq!(Superblocks::from_chain_params(&chain), Err(ParamsError::Height));
    }
}
