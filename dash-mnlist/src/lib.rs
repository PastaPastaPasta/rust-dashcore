//! `dash-mnlist`: SPV's masternode-list, quorum and lock engine (SB0 §5.3, SB11).
//!
//! The engine holds the simplified masternode lists and quorum sets that `mnlistdiff` and `qrinfo`
//! payloads deliver, each authenticated against an accepted header (SB0 §5.2 steps 1–4); verifies
//! each used commitment with Core's member computation over them (step 5, LQ1 through the EV
//! addendum's views); and checks ChainLocks and InstantSend locks against the verified quorums
//! (step 6, SB8). Applying a verified ChainLock to the header chain (step 7) is the host's.
//!
//! What it lacks it requests ([`MnRequest`]), never substituting one of Core's absence values
//! (EV addendum §2.1); what lies below the anchored chain's floor is `Unavailable`.

mod chain;
mod config;
mod convert;
mod engine;
mod error;
mod view;

#[cfg(test)]
mod tests;

pub use chain::MnChain;
pub use config::{ConfigError, LlmqConfig, Roles, min_lifetime};
pub use engine::{
    Coverage, DEFAULT_LOOKUP_WAITS, ListView, LockOutcome, Lookup, MasternodeListEngine, MnCfs,
    MnChange, MnEvent, MnRequest, MnSnapshot, MnWrites, Mode, QuorumState, RequestKey, SetAt,
    Unavailable,
};
pub use error::{FetchLookupError, MnLoadError, MnReject, MnViewError};
