//! Dash Core's LLMQ distributed key generation for the Rust Dash node (LQ4): a masternode's DKG
//! session as a pure state machine that DK1 drives.
//!
//! The crate is pure: no I/O, clocks, threads or globals (A1 §1.5, §2.1), and its BLS work is
//! serial (DEC-79). Randomness, the operator key, the dkgdb store, the relay topology and the
//! debug status are injected. Every ported function carries a `// Core:` anchor to Dash Core
//! v24.0.0-rc.4 (49caf372bb), the parity reference of `designs/LQ4-dkg.md`; `a/` is
//! `src/active/` and `l/` is `src/llmq/`.
//!
//! - [`dkg`]: the session, its message batch and its phase arithmetic.

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

pub mod dkg;
