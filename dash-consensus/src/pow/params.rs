//! The consensus parameters proof of work reads (CS2 §3.1).

use dash_num::{Arith256, CompactTarget};
use dash_params::{ChainParams, NetworkKind};

use crate::fp;
use crate::params::{ParamsError, check_devnet_arguments, height};

/// The proof-of-work fields of Core's `Consensus::Params`, and the tolerance pin Core hard-codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowConfig {
    /// `powLimit`.
    pub pow_limit: Arith256,
    /// `nPowTargetSpacing` (devnet `-powtargetspacing`).
    pub target_spacing: i64,
    /// `nPowTargetTimespan`.
    pub target_timespan: i64,
    /// `nPowKGWHeight`.
    pub kgw_height: i32,
    /// `nPowDGWHeight`.
    pub dgw_height: i32,
    /// `fPowAllowMinDifficultyBlocks`.
    pub allow_min_difficulty_blocks: bool,
    /// `fPowNoRetargeting`.
    pub no_retargeting: bool,
    /// `nMinimumDifficultyBlocks` (devnet `-minimumdifficultyblocks`; 0 elsewhere).
    pub minimum_difficulty_blocks: i32,
    /// The last height whose `nBits` only has to be within Core's ±50% tolerance of the expected
    /// value: `dash-params`' hard-coded pin, 68589 on mainnet and `None` elsewhere.
    pub pre_dgw_tolerance_until: Option<i32>,
}

impl PowConfig {
    /// Whether the block at `height` is under Core's ±50% `nBits` tolerance (mainnet ≤ 68589).
    // Core: validation.cpp:4476 (v23.1.8: 4110)
    pub fn tolerance_applies(&self, height: u32) -> bool {
        self.pre_dgw_tolerance_until.is_some_and(|until| i64::from(height) <= i64::from(until))
    }
}

/// Validated proof-of-work parameters: every value Core can run with, and no value where Core's
/// `GetNextWorkRequired` divides by zero or overflows (CS2 §2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowParams {
    pub(super) config: PowConfig,
    /// `UintToArith256(powLimit).GetCompact()`.
    pub(super) pow_limit_bits: CompactTarget,
}

impl PowParams {
    /// The parameters of `chain` (CS2 §3.1).
    pub fn from_chain_params(chain: &ChainParams) -> Result<Self, ParamsError> {
        check_devnet_arguments(chain)?;
        let tolerance = chain.pins.pre_dgw_tolerance_until;
        if tolerance.is_some() != (chain.network == NetworkKind::Mainnet) {
            return Err(ParamsError::TolerancePin);
        }
        let c = &chain.consensus;
        Self::new(PowConfig {
            pow_limit: c.pow_limit,
            target_spacing: c.pow_target_spacing,
            target_timespan: c.pow_target_timespan,
            kgw_height: height(c.pow_kgw_height)?,
            dgw_height: height(c.pow_dgw_height)?,
            allow_min_difficulty_blocks: c.pow_allow_min_difficulty_blocks,
            no_retargeting: c.pow_no_retargeting,
            minimum_difficulty_blocks: c.minimum_difficulty_blocks,
            // Core: validation.cpp:4476 ContextualCheckBlockHeader (v23.1.8: 4110)
            pre_dgw_tolerance_until: tolerance.map(height).transpose()?,
        })
    }

    /// Validates `config` (CS2 §3.1).
    fn new(config: PowConfig) -> Result<Self, ParamsError> {
        let c = &config;
        if !(1..=i64::MAX / 72).contains(&c.target_spacing) {
            return Err(ParamsError::TargetSpacing);
        }
        if !(1..=i64::MAX / 7).contains(&c.target_timespan) {
            return Err(ParamsError::TargetTimespan);
        }
        // GetNextWorkRequired reaches BTC for a last height in
        // [max(0, nMinimumDifficultyBlocks), nPowKGWHeight - 2].
        let btc_reachable =
            i64::from(c.minimum_difficulty_blocks.max(0)) < i64::from(c.kgw_height) - 1;
        if btc_reachable && c.target_timespan / c.target_spacing == 0 {
            return Err(ParamsError::AdjustmentInterval);
        }
        // KGW is reached for a next height in [nPowKGWHeight, nPowDGWHeight) unless
        // fPowNoRetargeting returns first.
        let kgw_reachable = c.kgw_height < c.dgw_height && !c.no_retargeting;
        // PastBlocksMax, where 0 means no limit at all
        let kgw_window = 7 * c.target_timespan / c.target_spacing;
        if kgw_reachable && !(1..=i64::from(fp::KGW_MAX_MASS)).contains(&kgw_window) {
            return Err(ParamsError::KgwWindow);
        }
        Ok(Self {
            config,
            pow_limit_bits: config.pow_limit.compact(false),
        })
    }

    /// The validated values.
    pub fn config(&self) -> &PowConfig {
        &self.config
    }

    /// `UintToArith256(powLimit).GetCompact()`.
    pub fn pow_limit_bits(&self) -> CompactTarget {
        self.pow_limit_bits
    }

    /// `DifficultyAdjustmentInterval()`. Never 0 where BTC is reachable.
    pub(super) fn adjustment_interval(&self) -> i64 {
        self.config.target_timespan / self.config.target_spacing
    }
}

#[cfg(test)]
mod tests {
    use dash_params::NetworkKind;

    use super::*;
    use crate::params::test_support::{chain, devnet_chain};

    #[test]
    fn every_dash_params_network_is_valid() {
        for network in NetworkKind::ALL {
            let p = PowParams::from_chain_params(&chain(network)).unwrap();
            assert_eq!(p.pow_limit_bits(), p.config().pow_limit.compact(false));
            let tolerance = (network == NetworkKind::Mainnet).then_some(68_589);
            assert_eq!(p.config().pre_dgw_tolerance_until, tolerance);
        }
    }

    /// The tolerance is `dash-params`' pin, and it must agree with the network: mainnet's
    /// parameters relabelled as another network, or another network's labelled mainnet, would
    /// apply or drop the 68589 tolerance where Core does not.
    #[test]
    fn the_tolerance_is_dash_params_pin() {
        let mut main = chain(NetworkKind::Mainnet);
        main.pins.pre_dgw_tolerance_until = Some(100u32.into());
        let p = PowParams::from_chain_params(&main).unwrap();
        assert!(p.config().tolerance_applies(100) && !p.config().tolerance_applies(101));
        main.pins.pre_dgw_tolerance_until = None;
        assert_eq!(PowParams::from_chain_params(&main), Err(ParamsError::TolerancePin));
        for network in [NetworkKind::Testnet, NetworkKind::Devnet, NetworkKind::Regtest] {
            let mut relabelled = chain(NetworkKind::Mainnet);
            relabelled.network = network;
            let err = Err(ParamsError::TolerancePin);
            assert_eq!(PowParams::from_chain_params(&relabelled), err, "{network:?}");
            let mut other = chain(network);
            other.network = NetworkKind::Mainnet;
            assert_eq!(PowParams::from_chain_params(&other), err, "{network:?}");
        }
    }

    #[test]
    fn rejects_what_core_cannot_run() {
        let check = |f: fn(&mut ChainParams), err| {
            let mut chain = chain(NetworkKind::Mainnet);
            f(&mut chain);
            assert_eq!(PowParams::from_chain_params(&chain), Err(err));
        };
        check(|c| c.consensus.pow_target_spacing = 0, ParamsError::TargetSpacing);
        check(|c| c.consensus.pow_target_spacing = i64::MAX / 72 + 1, ParamsError::TargetSpacing);
        check(|c| c.consensus.pow_target_timespan = 0, ParamsError::TargetTimespan);
        check(|c| c.consensus.pow_target_timespan = i64::MAX / 7 + 1, ParamsError::TargetTimespan);
        // BTC is reachable below KGW: a spacing above the timespan makes the interval 0
        check(
            |c| c.consensus.pow_target_spacing = c.consensus.pow_target_timespan + 1,
            ParamsError::AdjustmentInterval,
        );
        // KGW is reachable: a shorter spacing makes its window longer than 4032 blocks, and one
        // above 7 timespans makes it 0, which Core reads as no limit
        check(|c| c.consensus.pow_target_spacing = 149, ParamsError::KgwWindow);
        check(
            |c| {
                c.consensus.pow_kgw_height = 1u32.into();
                c.consensus.pow_target_spacing = 7 * c.consensus.pow_target_timespan + 1;
            },
            ParamsError::KgwWindow,
        );
        check(|c| c.consensus.pow_kgw_height = (1u32 << 31).into(), ParamsError::Height);
        check(|c| c.consensus.minimum_difficulty_blocks = 1, ParamsError::DevnetOverridesOffDevnet);
        check(|c| c.pins.pre_dgw_tolerance_until = Some((1u32 << 31).into()), ParamsError::Height);
    }

    #[test]
    fn accepts_devnet_spacings_where_btc_is_unreachable() {
        // -minimumdifficultyblocks at or above nPowKGWHeight - 1 skips the BTC rule, so any
        // spacing works (CS2 §2.5); KGW is unreachable on devnets (KGW height = DGW height).
        let mut chain = devnet_chain();
        chain.consensus.pow_kgw_height = 4001u32.into();
        chain.consensus.pow_dgw_height = 4001u32.into();
        chain.consensus.minimum_difficulty_blocks = 4000;
        for spacing in [86_401, (1 << 29) - 1, 1 << 29, 1 << 31, i64::MAX / 72] {
            chain.consensus.pow_target_spacing = spacing;
            let p = PowParams::from_chain_params(&chain).unwrap();
            assert_eq!(p.config().pre_dgw_tolerance_until, None, "{spacing}");
        }
        chain.consensus.minimum_difficulty_blocks = 3999;
        assert_eq!(PowParams::from_chain_params(&chain), Err(ParamsError::AdjustmentInterval));
    }
}
