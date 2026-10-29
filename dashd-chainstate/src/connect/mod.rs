//! CH3a's connector, around the seam V calls ([`BlockConnector`](crate::chain::BlockConnector),
//! CH3 §2.1): the two seams it calls in turn.
//!
//! - [`EvoHooks`] (CH3 §3.1, §3.2): the evo half of `ConnectBlock` and `DisconnectBlock`, each
//!   call one attempt opened from V's [`VContext`](crate::chain::VContext) and the hooks'
//!   [`Facade`](EvoHooks::Facade); [`NoEvo`] until CH7a.
//! - [`ChainHelper`] (CH3 §3.3): the decisions `ConnectBlock` asks Core's `m_chain_helper`, O8's
//!   recorded queries; [`NodeHelper`] in production, [`IbdHelper`] for replays, and
//!   `RecordedHelper` (feature `statedigest`) for replays of O8's recordings.
//!
//! Beside them, a [`DigestSink`] records each attempt (CH3 §2.15): `DigestLog`, O8's StateDigest
//! log, behind the `statedigest` feature.
//!
//! Semantics are Dash Core v24.0.0-rc.3's; `validation.cpp` cites are at that tag.

pub mod bench;
mod connector;
mod digest;
#[cfg(feature = "statedigest")]
mod digest_log;
mod disconnect;
mod evo;
mod guard;
mod helper;
#[cfg(feature = "statedigest")]
mod recorded;
mod serial;
mod tx_coins;

pub(crate) use connector::lock as lock_cell;
pub use connector::{ChainstateCell, ConnectorParts, CoreConnector};
pub use digest::{ConnectAttempt, DigestAttempt, DigestSink, DisconnectAttempt, Rewind};
#[cfg(feature = "statedigest")]
pub use digest_log::{DigestLog, MuHashAt, RewindObserver, Rewound};
pub use disconnect::UNDO_READ_FAILED;
pub use evo::{
    AttemptId, BlockAt, CreditPoolDigest, EvoAttempt, EvoDigest, EvoHooks, NoEvo, NoEvoAttempt,
    SpecialTxRules, pre_dip3_special_txs,
};
pub use guard::{g1_capability, g1_rule, g2_refusal_text, refusal_text};
pub use helper::{
    ChainHelper, INTERIM_REFUSAL, IbdHelper, IsLockRef, NodeHelper, is_governance_height,
};
#[cfg(feature = "statedigest")]
pub use recorded::{AttemptChoice, MissSlot, RecordedHelper};
#[cfg(test)]
pub(crate) use serial::wrong_superblock_start;
pub use serial::{
    AttemptSteps, BlockScratch, CORRUPT_BLOCK, DISK_SPACE_LOW, EVODB_INCONSISTENCY,
    FLUSH_UNDO_FAILED, MN_PAYEE_CAPABILITY, SUPERBLOCK_CAPABILITY, SerialCore, WRITE_UNDO_FAILED,
    block_subsidy_after,
};
