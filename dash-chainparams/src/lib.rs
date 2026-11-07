//! `dash-chainparams`: the node's chain parameters, Core's `CChainParams` (POL-3, DEC-1042).
//!
//! base-sdk's `dash-params` carries only stateless chain parameters. The node-side surface
//! (chain setup and options, runtime policy, devnets) moves here, and the base-sdk fork's params
//! crate converges on upstream's shape. Today this crate re-exports `dash-params` item by item,
//! in two lists:
//!
//! - the names upstream's `dash-params` exports, except `Network`, which stay re-exports for good;
//! - the other public items of the fork's `dash-params`, re-exported by name until their
//!   definitions move into this crate, with `Network`, whose fork copy has the extra `Devnet`.
//!
//! Consumers depend on this crate under the path name `dash_params`
//! (`dash-params = { package = "dash-chainparams", … }`), so every `use dash_params::…` stays as
//! it is; the name is kept on purpose (DEC-1043).

#![forbid(unsafe_code)]
#![no_std]

// Upstream's `dash-params` exports.
pub use bsdk_params::{
    AddrParams, BRR_PERIODS_PERMILLE, Bip9Deployment, Bip9Deployments, BuriedDeployment,
    ChainParams, ChainTxData, Checkpoint, ConsensusParams, GenesisParams, LlmqParams, LlmqType,
    MessageStart,
};

// The fork's `dash-params` items upstream lacks, by name, until they are defined here. `Network` is
// among them: the fork's has the extra `Devnet` variant.
pub use bsdk_params::{
    AddrSetup, AssumeutxoData, ChainOption, ChainParamsBuilder, ChainSetup, ConsensusSetup,
    DeploymentPos, DevnetGenesisParams, HardCodedPins, MAX_LLMQ_SIZE, Network, ParamsError,
    Release, ReleaseDeployments, SporkSigners, VbField, available_params,
};
