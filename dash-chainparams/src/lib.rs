//
// Copyright (c) 2026-present, The Dash Core developers
// SPDX-License-Identifier: MIT
// See the accompanying file LICENSE or https://opensource.org/license/MIT
//

//! `dash-chainparams`: the node's chain parameters, Core's `CChainParams` (POL-3, DEC-1042).
//!
//! base-sdk's `dash-params` carries only stateless chain parameters: the network tables of
//! mainnet, testnet and regtest, their consensus and LLMQ parameters and the genesis block fields.
//! This crate re-exports those names and defines the node-side surface on top of them:
//!
//! - [`Network`], with Core's empty-named devnet beside base-sdk's three networks, and its table;
//! - [`ChainParamsBuilder`], which applies Core's chain-params options and builds a
//!   [`ChainSetup`]: the parameters at a [`Release`] with the overrides, the devnet values, the
//!   spork signers and Core's runtime [`ChainPolicy`];
//! - [`DevnetGenesisParams`], a devnet's block at height 1;
//! - the extension traits [`LlmqTypeExt`], [`LlmqParamsExt`] and [`BuriedDeploymentExt`], with
//!   [`available_params`] and [`MAX_LLMQ_SIZE`], for the values Core derives from base-sdk's types.
//!
//! The crate builds on upstream base-sdk's `dash-params` and on the node's fork of it alike.
//!
//! Consumers depend on this crate under the path name `dash_params`
//! (`dash-params = { package = "dash-chainparams", … }`), so every `use dash_params::…` stays as
//! it is; the name is kept on purpose (DEC-1043).

#![forbid(unsafe_code)]
#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

mod builder;
mod consensus;
mod deployment;
mod devnet;
mod error;
mod genesis;
mod llmq;
mod network;
mod parse;
// Core's per-chain runtime policy, which base-sdk's `dash-params` doesn't carry.
mod policy;
mod prelude;
mod release;
mod setup;
mod spork;

// Upstream's `dash-params` exports.
pub use bsdk_params::{
    AddrParams, BRR_PERIODS_PERMILLE, Bip9Deployment, Bip9Deployments, BuriedDeployment,
    ChainParams, ChainTxData, Checkpoint, ConsensusParams, GenesisParams, LlmqParams, LlmqType,
    MessageStart,
};

pub use builder::{ChainOption, ChainParamsBuilder};
pub use consensus::BuriedDeploymentExt;
pub use deployment::DeploymentPos;
pub use error::{ParamsError, VbField};
pub use genesis::DevnetGenesisParams;
pub use llmq::{LlmqParamsExt, LlmqTypeExt, MAX_LLMQ_SIZE, available_params};
pub use network::Network;
pub use policy::ChainPolicy;
pub use release::Release;
pub use setup::{
    AddrSetup, AssumeutxoData, ChainSetup, ConsensusSetup, HardCodedPins, ReleaseDeployments,
};
pub use spork::SporkSigners;
