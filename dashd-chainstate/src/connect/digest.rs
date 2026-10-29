//! The connector's digest seam (CH3 §2.15; CH3a a7-1): [`CoreConnector`](super::CoreConnector)
//! hands each attempt to a [`DigestSink`] once it is decided, as O8's Core patch writes its record
//! when the `AttemptScope` around `ConnectBlock` or `DisconnectBlock` closes (O8 §8.1).
//!
//! The seam is always compiled; its implementation, the StateDigest log, is `DigestLog` behind the
//! `statedigest` feature. Without a sink the connector records nothing, and nothing else changes.

use dash_consensus::{Amount, Block};

use super::evo::BlockAt;
use crate::chain::{ConnectCx, ConnectOutcome, DisconnectResult, Fatal};

/// What records the connector's attempts (O8 §4.2–§4.4).
pub trait DigestSink: Send {
    /// Records one attempt, after its effect log is applied and before its commit (CH3a D33): the
    /// outcome is the one `ConnectBlock` or `DisconnectBlock` returns. Called only for an attempt
    /// whose `ConnectBlock` or `DisconnectBlock` ran, so a proposal's ChainLock and pre-check
    /// rejections have no record (CH3a D125).
    ///
    /// # Errors
    ///
    /// A failed record: the connector reports it as an `AbortNode` (O8 §8.3) and keeps the
    /// attempt's outcome, as O8's patch does (CH3a D137).
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal>;
}

/// One attempt, as the connector hands it to its [`DigestSink`].
pub enum DigestAttempt<'a> {
    Connect(ConnectAttempt<'a>),
    Disconnect(DisconnectAttempt<'a>),
}

/// A `ConnectBlock` call: a tip connect, a VerifyDB reconnect, or `TestBlockValidity`'s dry run.
pub struct ConnectAttempt<'a> {
    pub block: &'a Block,
    /// The block's place: `at.id` is `None` for a proposal's dummy (CH3 §2.14).
    pub at: &'a BlockAt,
    /// `fJustCheck`.
    pub just_check: bool,
    /// The target is the active coins ([`ViewTarget::Active`](crate::chain::ViewTarget)).
    pub active: bool,
    pub outcome: &'a Result<ConnectOutcome, Fatal>,
    /// For a block connected with `fJustCheck == false` other than genesis: the streamed
    /// `CBlockUndo` as it reached `WriteUndoDataForBlock`'s call site (`validation.cpp:2888`,
    /// where O8's patch hashes it), whether or not the write ran.
    pub undo: Option<&'a [u8]>,
    /// For a block connected to the active coins: `GetBlockSubsidy(pindex)`
    /// (`validation.cpp:1884-1889`), which the coinstats index adds
    /// (`index/coinstatsindex.cpp:124`).
    pub subsidy: Option<Amount>,
    /// `ConnectBlock`'s `fScriptChecks` (`validation.cpp:2542-2571`): `false` when
    /// `-assumevalid` skipped the block's scripts. `None` when the attempt returned before
    /// deciding (genesis, `v:2536-2540`, or an earlier rejection). An observation only: no
    /// record key carries it (CH3a D151).
    pub script_checks: Option<bool>,
}

/// A `DisconnectBlock` call: a tip disconnect, or VerifyDB's or ReplayBlocks' into its own view.
pub struct DisconnectAttempt<'a> {
    pub block: &'a Block,
    pub at: &'a BlockAt,
    pub outcome: &'a Result<DisconnectResult, Fatal>,
    /// Set for a clean disconnect of the active coins, the one the coinstats index rewinds
    /// (`index/coinstatsindex.cpp:293`).
    pub rewind: Option<Rewind<'a>>,
}

/// What the coinstats index's rewind of a disconnected block reads.
pub struct Rewind<'a> {
    /// The undo record the disconnect read, followed by whatever the read buffered after it (its
    /// checksum).
    pub undo: &'a [u8],
    /// `GetBlockSubsidy(pindex)` (`index/coinstatsindex.cpp:408`).
    pub subsidy: Amount,
}
