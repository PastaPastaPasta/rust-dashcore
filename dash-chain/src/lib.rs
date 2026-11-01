//! `dash-chain`: one header index for the Rust Dash node and SPV (SB0 §4; SB4).
//!
//! The node runs [`HeaderChain`] from genesis with no window; SPV runs it anchored and windowed.
//! Consensus semantics are Dash Core v24.0.0-rc.3's; every Core cite in this crate is at that
//! tag. Class S (SB0 §2.3): no threads, no pools built, no statics with network parameters, no
//! `abort`/`exit`.

#![forbid(unsafe_code)]

mod active;
mod ancestry;
mod anchor;
mod arena;
mod batch;
mod chain;
mod chainlock;
mod config;
mod entry;
mod error;
mod feed;
mod host;
mod index;
mod reader;
mod record;
mod residency;
mod shared;
mod snapshot;
mod status;
mod status_journal;
mod store;
#[cfg(all(test, not(loom)))]
mod testing;
// CS3's check inputs and boundary rows (`checks_rows.rs` reads `crate::checks`), for A-1/A-2's
// rule-level replay of Core's recorded header verdicts (`chain/scenario_tests/rule_rows.rs`).
#[cfg(all(test, not(loom)))]
#[path = "../../dash-consensus/tests/support/checks.rs"]
mod checks;
#[cfg(all(test, not(loom)))]
#[path = "../../dash-consensus/tests/support/checks_rows.rs"]
mod checks_rows;
mod tip;
mod walk;

pub use ancestry::AnchoredAncestry;
pub use anchor::{Anchor, AnchorPack, CoverageMiss, anchor_candidate, anchor_fallback};
pub use batch::{
    AcceptOne, HeaderAccepted, HeaderBatch, HeaderReject, HeaderStop, HeadersResult, PeerId,
};
pub use chain::{
    ChainTip, ChainTipStatus, CompleteAncestry, FailedBlocks, HeaderChain, Hold, MutableGroup,
    NodeIndex,
};
pub use chainlock::{
    ChainLockRef, ClActiveState, ClOutcome, ClPersisted, EnforceReport, FloorRelation, ForkTarget,
    ResetReport, updated_block_tip_schedules,
};
pub use config::{
    ChainConfig, ChainMode, DEFAULT_COLD_CACHE, DEFAULT_MAX_TIP_AGE, Services, SporkView,
    StaticSporks,
};
pub use entry::MTP_UNKNOWN;
pub use error::{
    AncestryError, AnchorError, ApplyError, ChainError, CodecError, ConfigError, FailStop,
    IndexInconsistency, LoadError, StoreError,
};
pub use host::{TipHost, VirtualTip};
pub use reader::{ChainReader, EntryView, FirstAncestors, FlatFilePos, IndexAccess, IndexVisitor};
pub use record::{BlockIndexRecord, IndexWrite};
pub use residency::{Evicting, Permanent, Residency};
pub use snapshot::{ActiveChainView, ChainReadSnapshot, TipFacts};
pub use status::Status;
pub use status_journal::{JournalRetention, RETAIN_AT_MOST, SnapshotExpired};
pub use store::{HeaderRecord, HeaderStore, NoStore, RecordKind};
pub use tip::{BlockRef, TipCell, TipChange, TipPlan, TipStep, TipTarget};

pub use dash_consensus::BlockId;

/// Absolute chain work (`nChainWork`).
pub type Work = dash_num::Arith256;

// SB5 (feeds and residency). Kept in its own block so SB4's lists above rebase cleanly.
mod budget;
mod cold;
mod fault;
#[cfg(all(any(test, feature = "test-util"), not(loom)))]
mod mem_store;
mod notify;
mod open;
mod status_feed;
mod step;
mod sync;
#[cfg(all(feature = "test-util", not(loom)))]
#[doc(hidden)]
pub mod test_util;

pub use budget::{ACTIVE_PATHS_BYTES, STEP_FIXED_BYTES, StepBudgetStats, registry_bytes, step_cap};
pub use feed::{
    BlockData, CONTROL_BYTES, CONTROL_PAYLOAD_MAX, ChainEvent, ChainFeeds, ChainSnapshot,
    ControlKind, ControlMsg, FEED_FIXED_BYTES, FeedConfig, FeedConfigError, FeedId, FeedReader,
    FeedSnapshot, FeedSource, Handoff, MAX_CONTROL_KINDS, MAX_FEEDS, Needs, Next, Publish, Refused,
    Registry, RegistryEntry, SendControl, SubscribeError, SubscriberFeed, per_feed_cap,
};
#[cfg(all(any(test, feature = "test-util"), not(loom)))]
#[doc(hidden)]
pub use mem_store::{Corrupt, Faults, MemHeaderStore, ReadKind, Tables};
pub use notify::{Notify, Parker};
pub use open::{Corruption, OpenError};
pub use status_feed::{StatusEvent, StatusReader};
pub use step::{
    ActiveDiff, AnchorState, CHAIN_SCHEMA, ChainMeta, Durability, HeaderRow, HeaderStepSink,
    MetaDelta, RowError, StepRow, StepTail, TipSummary,
};
