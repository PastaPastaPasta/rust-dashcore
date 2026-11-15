//! Runtime quorum management for the Rust Dash node.

#![forbid(unsafe_code)]

pub mod manager;
pub mod traits;

pub use manager::QuorumManager;
pub use traits::{
    CleanupFacade, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup,
    VerifiedContributions, WarmExecutor,
};
