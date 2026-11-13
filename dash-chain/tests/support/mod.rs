//! Shared by the nightly integration tests: H-M and H-T (`headers`) and hex.
//!
//! The `sb4-core-scenarios` vectors are replayed by the lib tests (`src/chain/scenario_tests`),
//! whose loader checks each against the replay register (`scenario_tests/coverage.rs`).

#![allow(dead_code, reason = "each test binary uses part of this module")]

pub mod headers;

pub fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}
