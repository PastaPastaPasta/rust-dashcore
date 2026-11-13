// SPDX-License-Identifier: MIT

//! Read interfaces for the node's transaction, block filter and coin statistics indexes.
//! Core citations in this crate refer to Dash Core v24.0.0.

#![forbid(unsafe_code)]

pub mod api;
#[cfg(feature = "testing")]
pub mod testing;

pub use api::*;
