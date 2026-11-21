//! Runtime quorum management for the Rust Dash node.

#![forbid(unsafe_code)]

pub mod chain;
pub mod manager;
pub mod net_quorum;
pub mod store;
pub mod tip;
pub mod traits;

pub use chain::ChainQuorums;
pub use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
pub use manager::QuorumManager;
#[cfg(feature = "testkit-seams")]
pub use manager::share_seam::{ItemReport, LoadError, LoadReport, Rejection};
pub use store::{
    KvQuorumStore, MemKeyStore, NoDkgSessions, NotMasternode, QUORUMDB_SCHEMA, sk_row_key,
    vvec_row_key,
};
pub use traits::{
    CleanupFacade, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup,
    VerifiedContributions, WarmExecutor,
};
#[cfg(feature = "testkit-seams")]
pub use traits::{MinedQuorum, ShareSeamView};
