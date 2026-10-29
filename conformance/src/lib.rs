//! Differential conformance harness for Dash chain data.
//!
//! Decodes every block and transaction of a chain through rust-dashcore (`dashcore`) and
//! base-sdk (`dash_primitives`), and checks that both re-encode byte-identically, agree on
//! hashes, merkle roots and special-transaction payload fields, and agree with a reference
//! dashd ("oracle"). A third leg runs the node's `dash_consensus` wrappers against the same
//! references (`consensus`). Blocks come either from the oracle's JSON-RPC interface or straight
//! from dashd's `blk*.dat` files. Mismatches are written to an NDJSON report; see [`report`].

pub mod blkfile;
#[cfg(feature = "bsdk-judges")]
mod bsdk_judges;
pub mod check;
mod consensus;
pub mod digest;
pub mod fingerprint;
pub mod payload;
pub mod render;
pub mod report;
pub mod rpc;
pub mod run;
pub mod scan;
pub mod script;
pub mod split;
pub mod verbose;
