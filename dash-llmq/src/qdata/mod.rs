//! Dash Core's quorum data exchange (`qgetdata`/`qdata`) and quorum data recovery, as pure code
//! (PLAN-QM1b §2). Time is passed in as `now`, Core's `GetTime()` seconds.
//!
//! This module holds the plain data shared by the `qdata` submodules and the runtime that drives
//! them. Every serve and intake call takes the BLS scheme from the message snapshot it is handed;
//! no function here reads a scheme from anywhere else.
//!
//! - [`tracker`]: the data request map and its inbound caps.
//! - [`observer`]: `NetDKG`'s observer-mode message and inventory answers.
//! - [`serve`]: the answer to a peer's `qgetdata`.
//! - [`intake`]: the handling of a peer's `qdata`.
//! - [`recovery`]: the quorum data recovery planner and per-quorum job.
//! - [`source`]: the seams to the node's quorums, dkgdb, operator key and masternode list.

use dash_consensus::primitives::BlockHash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;

#[cfg(any(test, feature = "testkit"))]
pub mod fakes;
pub mod intake;
pub mod observer;
pub mod recovery;
pub mod serve;
pub mod source;
pub mod tracker;

pub use tracker::Request;

/// `-llmq-qvvec-sync`'s per-type mode.
// Core: v24.0.0 src/llmq/options.h:24-28 QvvecSyncMode
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncMode {
    /// Sync the vvec of every quorum of the type.
    Always = 0,
    /// Sync only if the node is a member of some quorum of the type.
    OnlyIfTypeMember = 1,
}

/// The node's quorum role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Neither a masternode nor a watching observer.
    None,
    /// A masternode; `pro_tx` is None until its registration is known.
    Masternode {
        pro_tx: Option<ProTxHash>,
        watching: bool,
    },
    /// A watching observer (`-watchquorums`).
    Observer,
}

/// A quorum's LLMQ type and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuorumRef {
    pub llmq_type: LlmqType,
    pub quorum_hash: BlockHash,
}

/// A chain tip's height and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TipRef {
    pub height: i32,
    pub hash: BlockHash,
}

/// A peer's connection id (Core's `NodeId`), shared with the sig-share contract.
pub use crate::sigenv::PeerId;

/// What a `qdata` intake asks the runtime to do, emitted in order as it happens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Add `score` to the sending peer's misbehavior with `reason`.
    Score(i32, &'static str),
    /// A log line.
    Log(String),
    /// Queue the quorum's cache warm-up.
    QueueWarm(QuorumRef),
    /// Persist the quorum's recovered data.
    Write(QuorumRef),
}

/// What a recovery step asks the runtime to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Push `qgetdata` to `peer`; the request is already registered.
    Send(PeerId, Request),
    /// Disconnect `peer`.
    Disconnect(PeerId),
    /// Ask the connection manager to connect to a masternode.
    AddPendingMasternode(ProTxHash),
}
