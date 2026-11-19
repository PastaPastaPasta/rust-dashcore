//! The masternode owner's status and commands, which lookups and lock managers wait on.

use std::net::SocketAddr;
use std::sync::Arc;

use dash_mnlist::{LockOutcome, Lookup, MnSnapshot, VerifiedChainLock, VerifiedIsLock};
use dash_p2p_core::{ClSig, IsdLock};
use tokio::sync::{mpsc, oneshot, watch};

use super::anchor::Phase;
use super::quorum_lookup::QuorumQuery;

/// What the owner publishes after each input that changed `gen`, `lookup_waits` or `snapshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct MnStatus {
    /// Rises when the engine's progress or `tip_seq` changed, or the engine was replaced.
    /// Every command's reply carries its value after the call.
    pub gen: u64,
    /// The header tip sequence the owner last pinned.
    pub tip_seq: u64,
    /// What the engine's last retention published.
    pub snapshot: Arc<MnSnapshot>,
    /// How many lookups wait on their fetch.
    pub lookup_waits: usize,
    /// The engine's bound on waiting lookups.
    pub max_lookup_waits: usize,
    /// The initial sync's phase.
    pub phase: Phase,
    /// Whether the owner stopped. It answers no further command.
    pub stopped: bool,
}

/// The answer to [`MnCommand::Resolve`], checked and registered in one owner input.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "returned and matched at once: a box would allocate per lookup"
)]
pub enum Resolved {
    /// The engine's answer. `reasked` is whether a fetch ran on `Unavailable`, spending the
    /// lookup's re-ask.
    Answer {
        lookup: Lookup,
        reasked: bool,
    },
    /// A wait for the lookup is registered: the answer comes at a later `gen`. `reasked` is as
    /// for `Answer`.
    Asked {
        reasked: bool,
    },
    /// The engine refused a new wait: ask again once `lookup_waits` is below its bound.
    Busy,
}

/// A command to the masternode owner. Each reply carries the owner's `gen` after the call. The
/// owner drops the reply sender when it stops instead.
#[derive(Debug)]
pub enum MnCommand {
    VerifyChainLock {
        cl: ClSig,
        peer: SocketAddr,
        reply: oneshot::Sender<(Result<VerifiedChainLock, LockOutcome>, u64)>,
    },
    VerifyIsLock {
        is: IsdLock,
        peer: SocketAddr,
        reply: oneshot::Sender<(Result<VerifiedIsLock, LockOutcome>, u64)>,
    },
    /// Reads the lookup and, on `Fetch` or on `Unavailable` with `reask`, fetches it.
    Resolve {
        query: QuorumQuery,
        reask: bool,
        reply: oneshot::Sender<(Resolved, u64)>,
    },
    /// Clears the held ChainLock if it is `cl`. Sent only after step 7 replied for `cl`.
    ClearPendingChainLock {
        cl: ClSig,
        reply: oneshot::Sender<(bool, u64)>,
    },
    /// Finishes the current input, commits it, and acks.
    Stop(oneshot::Sender<()>),
}

/// The owner's status and command channel, shared by lookups and lock managers.
#[derive(Clone, Debug)]
pub struct MnHandle {
    pub status: watch::Receiver<MnStatus>,
    pub cmd: mpsc::Sender<MnCommand>,
}
