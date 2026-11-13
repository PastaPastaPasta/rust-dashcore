// SPDX-License-Identifier: MIT

//! Read interfaces and host inputs for the node's transaction, block filter and coin statistics
//! indexes.
//! Core citations in this crate refer to Dash Core v24.0.0.

#![forbid(unsafe_code)]

pub mod api;
pub mod blockfilter;
pub mod inputs;
#[cfg(feature = "testing")]
pub mod testing;

pub use api::*;
pub use inputs::*;
