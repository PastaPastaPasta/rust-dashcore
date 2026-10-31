//! Chainstate of the Rust Dash node (A1 §2.4; CH2).
//!
//! - [`chain`]: the validation actor V over the header chain and CH1-node's index tables.
//! - [`connect`]: the seams CH3a's connector calls: the evo hooks and the chain helper (CH3).
//! - [`coins`]: the coins cache: its keys, values, layers, the tip and the block overlay (CH2a).
//! - [`caches`]: Core's split of `-dbcache` among the databases and the coins cache (CH2a).
//! - [`tuning`]: every engine-specific number and hook, behind [`tuning::EngineTuning`] (CH2a).
//! - [`store`]: the chainstate database's schema and the guarded engine (CH2a).
//! - [`flush`]: the flush decision and what V hands W at a freeze (CH2b).
//! - [`recovery`]: recovery at startup, around CH1-node's load of the block index (CH2b).
//! - [`subscriber`]: the indexes that follow V's tip, and their feed (CH2b b3).
//! - [`sigcache`]: Core's signature cache and script-execution cache (CS6).
//! - [`checkqueue`]: Core's check queue, and the script checks block validation runs on it with
//!   Core's result in each `-par` mode (CS6).
//! - [`reorg`]: Core's disconnect pool, the reorg's transactions for the mempool (CH3b).
//! - [`alloc_tag`]: the tagging allocator that checks the coins budget in bench and test builds.
//!
//! **Core is the reference** (v23.1.8 and v24.0.0-rc.3): `coins.{h,cpp}` and `txdb.cpp`'s coin
//! ranges are identical at both pins apart from line shifts; citations give `v23 / v24` lines
//! where they differ.

#![deny(unsafe_code)]

pub mod alloc_tag;
pub mod caches;
pub mod chain;
pub mod checkqueue;
pub mod coins;
pub mod connect;
#[cfg(feature = "crash")]
pub mod crash;
mod error;
pub mod evo;
pub mod flush;
mod planted;
pub mod recovery;
pub mod reorg;
pub mod sigcache;
pub mod store;
pub mod subscriber;
pub mod tuning;

pub use error::CoinsError;
#[cfg(feature = "planted-bugs")]
pub use planted::Planted;

// Not under Miri, which checks `StableBytes` (CH2 §7.1 a2): Stacked Borrows rejects the allocator's
// header, read in front of a pointer a `Box` has retagged for its own bytes only (Tree Borrows
// accepts it).
#[cfg(all(test, feature = "alloc-tag", not(miri)))]
#[global_allocator]
static ALLOC: alloc_tag::TaggingAllocator = alloc_tag::TaggingAllocator;

// Planted bugs are for test builds only (CH2 §6.4): no optimized build carries them, so neither a
// runtime binary nor a bench can. A planted test arms its bug on the instance it drives.
#[cfg(all(feature = "planted-bugs", not(debug_assertions)))]
compile_error!("feature `planted-bugs` is for test builds only, and this build is optimized");
// The test-support seams, as dashd-node's (CH1-node D70): no optimized build carries them. Clippy
// is exempt, as the pre-push hook lints `--release --all-features`.
#[cfg(all(feature = "test-support", not(debug_assertions), not(clippy)))]
compile_error!("feature `test-support` is for tests only; build without it");
