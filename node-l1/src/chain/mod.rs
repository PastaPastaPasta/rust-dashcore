//! `diff_chain` (node-program O2b §3–§6): a Core chain session driven by generated op
//! sequences, compared step by step with a second system (O2b-3b: `NodeSut`).
//!
//! O2b-3a runs the Core side alone: the generator's expectations, the shim's health and
//! same-process replay determinism are checked; nothing here is a certification leg (O2b-3 P6).

// A failed expectation of the harness's own construction is a harness bug: panicking is the
// report.
#![allow(clippy::expect_used)]

pub mod build;
pub mod census;
pub mod compare;
pub mod core;
#[cfg(feature = "sut")]
pub mod diff;
pub mod generate;
pub mod intent;
pub mod mutate;
#[cfg(feature = "sut")]
pub mod node;
#[cfg(feature = "sut")]
pub mod params;
pub mod run;
pub mod shape;
pub mod step;
pub mod ties;
pub mod world;

pub use run::{Mode, Runner, fuzz_one, fuzz_one_in};
