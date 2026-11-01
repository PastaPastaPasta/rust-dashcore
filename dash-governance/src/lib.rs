//! Dash Core v24's governance rules, pure (GV1a; notes in
//! `docs/node-program/designs/GV1a-notes.md`).
//!
//! - [`object`] has the object hashes, the collateral fee, local validity and the collateral check.
//! - [`proposal`] is `governance::ValidateProposal`.
//! - [`trigger`] is `CSuperblock`: the trigger format, its parse, heights, limit and expiry.
//! - [`vote`] has the vote hashes, signatures and validity.
//! - [`tally`] has an object's votes, `ProcessVote`, the counts and the sentinel flags.
//! - [`superblocks`] has the held triggers, the best superblock and EV3's `GovernanceView`.
//! - [`rate`] is `CRateCheckBuffer` and `MasternodeRateCheck`.
//! - [`json`] and [`strings`] are the Core helpers these read data with.
//!
//! Time, the masternode list and the collateral lookup come in as arguments: the crate reads no
//! clock, store or network. The stores, sync and message handling are GV1b's (notes D21).
#![forbid(unsafe_code)]

pub mod exception;
pub mod json;
pub mod object;
pub mod proposal;
pub mod rate;
pub mod strings;
pub mod superblocks;
pub mod tally;
pub mod trigger;
pub mod vote;

use dash_evo::payments::{PaymentsParams, PaymentsParamsError};
use dash_params::{ChainSetup, Network};

/// The chain parameters the rules read.
#[derive(Clone, Debug)]
pub struct GovParams {
    chain: ChainSetup,
    payments: PaymentsParams,
}

impl GovParams {
    pub fn from_chain_params(chain: &ChainSetup) -> Result<Self, PaymentsParamsError> {
        Ok(GovParams {
            payments: PaymentsParams::from_chain_params(chain)?,
            chain: chain.clone(),
        })
    }

    pub fn chain(&self) -> &ChainSetup {
        &self.chain
    }

    /// EV3's parameters (`CSuperblock::IsValid`, `GetPaymentsLimit`).
    pub fn payments(&self) -> &PaymentsParams {
        &self.payments
    }

    /// `nSuperblockCycle`.
    pub fn superblock_cycle(&self) -> i32 {
        self.payments.subsidy().config().superblocks.cycle()
    }

    /// `nSuperblockStartBlock`.
    pub fn superblock_start(&self) -> i32 {
        self.payments.superblock_start()
    }

    /// `nGovernanceMinQuorum`.
    pub fn min_quorum(&self) -> i32 {
        self.chain.consensus.governance_min_quorum
    }

    /// `nPowTargetSpacing`.
    pub fn target_spacing(&self) -> i64 {
        self.chain.consensus.pow_target_spacing
    }

    /// `NetworkIDString() == CBaseChainParams::MAIN`.
    pub fn is_mainnet(&self) -> bool {
        self.chain.network == Network::Main
    }

    /// `NetworkIDString() == CBaseChainParams::TESTNET`: ECDSA vote signatures sign the hash.
    pub fn is_testnet(&self) -> bool {
        self.chain.network == Network::Testnet3
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, Network, Release};

    use crate::GovParams;

    pub fn chain(network: Network) -> ChainSetup {
        let mut builder = ChainParamsBuilder::new(network, Release::V24_0);
        if network == Network::Devnet {
            builder = builder.arg(ChainOption::Devnet, "gv1a");
        }
        builder.build().expect("Core's parameters")
    }

    pub fn params(network: Network) -> GovParams {
        GovParams::from_chain_params(&chain(network)).expect("Core's parameters")
    }
}
