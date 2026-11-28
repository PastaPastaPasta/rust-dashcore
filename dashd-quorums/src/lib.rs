//! Runtime quorum management for the Rust Dash node.

#![forbid(unsafe_code)]

pub mod chain;
// Off until BL-895 composes the DKG runtime into rdashd.
#[cfg(feature = "dkg-runtime")]
pub mod dkg;
pub mod manager;
pub mod net_quorum;
pub mod observer;
pub mod recovery_rt;
pub mod recsig_store;
pub mod signing;
pub mod sigshares;
pub mod source;
pub mod store;
pub mod tip;
pub mod traits;
// Off until BL-895 composes the DKG runtime into rdashd: it names `dash_llmq::dkg::session`.
#[cfg(feature = "dkg-runtime")]
pub mod dkgdb;

pub use chain::ChainQuorums;
pub use dash_llmq::qdata::source::{OperatorDecrypt, ValidMnsAt};
pub use dash_llmq::qdata::{Role, SyncMode, TipRef};
pub use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions};
pub use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigScan};
#[cfg(feature = "dkg-runtime")]
pub use dkgdb::{DKGDB_SCHEMA, DkgDb, DkgDbError, dkgdb_path};
pub use manager::QuorumManager;
#[cfg(feature = "testkit-seams")]
pub use manager::share_seam::{ItemReport, LoadError, LoadReport, Rejection, check_file};
pub use recovery_rt::{RecoveryPool, default_worker_count};
pub use recsig_store::{KvRecSigStore, RECSIGDB_SCHEMA};
pub use store::{
    KvQuorumStore, MemKeyStore, NoDkgSessions, NotMasternode, QUORUMDB_SCHEMA, sk_row_key,
    vvec_row_key,
};
pub use traits::{
    CleanupFacade, DkgChain, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup,
    VerifiedContributions, WarmExecutor,
};
#[cfg(feature = "testkit-seams")]
pub use traits::{MinedQuorum, ShareSeamView};
