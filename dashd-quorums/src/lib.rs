//! Runtime quorum management for the Rust Dash node.

#![forbid(unsafe_code)]

pub mod manager;
pub mod traits;

pub use manager::QuorumManager;
#[cfg(feature = "testkit-seams")]
pub use manager::share_seam::{ItemReport, LoadError, LoadReport, Rejection};
pub use traits::{
    CleanupFacade, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup,
    VerifiedContributions, WarmExecutor,
};
#[cfg(feature = "testkit-seams")]
pub use traits::{MinedQuorum, ShareSeamView};
