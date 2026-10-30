//! Shared by the crate's bench targets (`chain`, SB5's `feeds`). Each target compiles its own
//! copy and uses part of it.
#![allow(dead_code, reason = "each bench target uses part of the shared support")]

// PF0 measures production code: `test-util` turns on the residency and slot-section probes, so
// a bench build with it fails. Debug builds of the benches (clippy and tests with
// `--all-features`) still compile.
#[cfg(all(feature = "test-util", not(debug_assertions)))]
compile_error!(
    "dash-chain's benches must not build with `test-util`: PF0 measures production code"
);

pub mod fixture;
pub mod real;
