//! The Rust Dash node's governance runtime (GV1b) around `dash-governance`'s pure manager.
//!
//! - [`exec`] is the chain-command seam: [`GovExec`] runs a governance command on V, which reads
//!   the chain through [`GovChain`] and the time and sync state through [`GovEnv`], and hands
//!   effects nobody waits for to an [`EffectsSink`].
//! - [`collateral`] locates proposal collateral through IX1's transaction index at use, inside
//!   the command that reads it.
//! - [`rand`] is the governance randomness, production and replay.
//! - [`handle`] holds the governance lock and the published [`TriggerView`].
//! - [`store`] is rdashd's `governance.dat`, and [`import`] turns Dash Core's into the manager's
//!   state.
//!
//! Lock order: V, then the governance lock, then the index.
//!
//! [`TriggerView`]: dash_governance::manager::TriggerView
#![forbid(unsafe_code)]

pub mod collateral;
pub mod exec;
pub mod handle;
pub mod import;
pub mod rand;
pub mod store;

pub use collateral::{AtUseCollateral, locate};
pub use exec::{EffectsSink, GovChain, GovCommand, GovEnv, GovExec, GovReply};
pub use handle::{Admitted, GovernanceHandle, TipDone};
pub use import::{ImportError, from_core};
pub use rand::{CoreRng, Draw, GovRng, ReplayRng};

#[cfg(test)]
mod tests;
