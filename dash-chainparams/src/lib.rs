//! `dash-chainparams`: the node's chain parameters, Core's `CChainParams` (POL-3, DEC-1042).
//!
//! base-sdk's `dash-params` carries only stateless chain parameters. The node-side surface
//! (chain setup and options, runtime policy, devnets) moves here, and the base-sdk fork's params
//! crate converges on upstream's shape. Today this crate re-exports `dash-params` unchanged.
//!
//! Consumers depend on this crate under the path name `dash_params`
//! (`dash-params = { package = "dash-chainparams", … }`), so every `use dash_params::…` stays as
//! it is; the name is kept on purpose (DEC-1043).

#![forbid(unsafe_code)]
#![no_std]

pub use bsdk_params::*;
