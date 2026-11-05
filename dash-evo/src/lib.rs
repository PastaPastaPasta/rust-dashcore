//! The shared evo layer for the Rust Dash node and SPV (node program, A1 §2.5; EV addendum §1):
//! what both products compute from authenticated inputs.
//!
//! Global allocator: the gate proves that none comes from this crate only for the products
//! `contrib/node/layers.toml` registers (`[[product]]`, DEC-181); other consumers are out of scope.
//!
//! Shared (layer P): Core's vocabulary types, the views LQ1 reads through, the effect channel the
//! algorithms emit into, and the BLS scheme context. No `imbl`, no store type, no DML value: the
//! node's authoritative state (the masternode list, the evo store, credit pool, MNHF) is the
//! node-only `dash_dml`, and SPV's lists are `dash_mnlist`'s (EV addendum §1.1).
//!
//! The crate is pure: no I/O, clocks, threads or globals (A1 §1.5, §2.1). Every ported function
//! carries a `// Core:` anchor to Dash Core v24.0.0-rc.3, the one parity reference (DEC-82;
//! A1 Appendix E).
//!
//! EV0 lands the interfaces, types first (PLAN EV0); `API.md` maps each EV2 and LQ1 requirement
//! to the item that serves it.
//!
//! - [`types`]: `ProTxHash`, `MnType`, `RawMnType`, which the DML and the SML share.
//! - [`effect`]: Core's non-transactional actions as [`effect::Effect`]s, recorded through an
//!   [`effect::EffectSink`] that survives an `Err` (A1 §2.10.1).
//! - [`bls`]: the BLS scheme context per call site (A1 §1.4.1, DEC-71).
//! - [`ctx`]: [`ctx::EvoCtx`], the context of a block's evo processing.
//! - [`sml`]: the simplified masternode list, its entry hash and the CbTx roots (SB7).
//! - [`cbtx`]: the CbTx root checks, its partial-merkle proof and the coinbase-ChainLock
//!   predicate (SB7).
//! - [`llmq`]: the member-source trait ([`llmq::QuorumMemberSource`], LQ1 §2.6), the list views
//!   LQ1 reads through ([`llmq::MnListView`], LQ1 §2.2), the fallible chain view
//!   ([`llmq::LlmqChain`], EV addendum §2.2), the mined-commitment reader, and the snapshot and
//!   commitment values.
//! - [`locks`]: ChainLock and InstantSend lock verification over a scan with each entry's status
//!   (SB8; SB0 §5.2 step 6).
//! - [`payments`]: the coinbase value and payee checks (`IsBlockValueValid`,
//!   `IsOldBudgetBlockValueValid`, `IsBlockPayeeValid`) without governance data, read through
//!   [`payments::GovernanceView`]; EV3's governance-backed checks are explicit seams (CH3a).
//! - [`spork`]: Core's spork messages, signatures, validation, store and values, the spork core
//!   the node's spork manager and SPV share (SB4 D29; SP0).
//! - `testkit` (feature `test-utils`): an in-memory block tree.
//!
//! See `ARCHITECTURE.md` for the rules other crates follow when they use these types.

#![forbid(unsafe_code)]
#![deny(clippy::float_arithmetic)]

pub mod bls;
pub mod cbtx;
pub mod ctx;
pub mod effect;
pub mod llmq;
pub mod locks;
pub mod payments;
pub mod sml;
pub mod spork;
#[cfg(any(test, feature = "test-utils"))]
pub mod testkit;
pub mod types;

pub use bls::{BlsCtx, bls_ctx_after, bls_ctx_for_block};
pub use ctx::EvoCtx;
pub use effect::{Effect, EffectKind, EffectSink};
pub use llmq::{MinedCommitment, MinedCommitmentReader, MinedFamily, MinedIndexEntry};
