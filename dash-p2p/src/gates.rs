//! The pre-handshake gates of Core's `ProcessMessage` (P2a §3.2.1; v24
//! `net_processing.cpp:3918-4386`), applied before a command's own branch.

use dash_p2p_core::CommandString;
use dash_p2p_transport::sanitize_command;

use crate::conn::{Peer, PeerHandle, PeerId};
use crate::policy::{self, GateClass};

const VERSION: CommandString = CommandString::from_static("version");
const MNAUTH: CommandString = CommandString::from_static("mnauth");

/// A gate's verdict on one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Run the command's branch.
    Admit,
    /// Gate 1: a `version` after `version`: `"redundant version message from peer=%d"`.
    RedundantVersion,
    /// Gate 2: anything but `version` before it: `"non-version message before version
    /// handshake. Message \"%s\" from peer=%d"`.
    BeforeVersion,
    /// Gate 5: a post-`verack` command before `verack`: `"Unsupported message \"%s\" prior to
    /// verack from peer=%d"`, no score.
    BeforeVerack,
    /// Gate 6: the first message after `verack` of a masternode probe is not `mnauth`;
    /// disconnect.
    ProbeWithoutMnauth,
    /// Gate 7: `RejectIncomingTxs && NetMessageViolatesBlocksOnly`: `"%s sent in violation of
    /// protocol peer=%d"`, disconnect, nothing read.
    BlocksOnlyViolation,
}

impl Gate {
    /// Whether the gate set `fDisconnect` (gates 6 and 7). [`admit`] has marked the peer; the
    /// caller may call [`NetCore::disconnect`](crate::NetCore::disconnect) to wake the sweep.
    pub const fn disconnects(self) -> bool {
        matches!(self, Self::ProbeWithoutMnauth | Self::BlocksOnlyViolation)
    }
}

/// The peer state the gates read and write (Core's `CNode` fields `nVersion`,
/// `fSuccessfullyConnected`, `nTimeFirstMessageReceived`, `fFirstMessageIsMNAUTH`,
/// `m_masternode_probe_connection` and `fDisconnect`). [`PeerHandle`] is the network core's; a
/// model or a test can supply its own.
pub trait GatePeer {
    /// The peer's id, for the log lines.
    fn id(&self) -> PeerId;
    /// `nVersion`: 0 until `version` was processed.
    fn version(&self) -> i32;
    /// `fSuccessfullyConnected`: `verack` was received.
    fn successfully_connected(&self) -> bool;
    /// `nTimeFirstMessageReceived`: 0 until the first message after `verack`.
    fn first_message_time(&self) -> i64;
    /// Records `nTimeFirstMessageReceived` and `fFirstMessageIsMNAUTH`.
    fn set_first_message(&self, time: i64, is_mnauth: bool);
    /// `m_masternode_probe_connection`.
    fn masternode_probe(&self) -> bool;
    /// Sets `fDisconnect`.
    fn mark_disconnect(&self);
}

impl GatePeer for PeerHandle {
    fn id(&self) -> PeerId {
        Peer::id(self)
    }
    fn version(&self) -> i32 {
        Peer::version(self)
    }
    fn successfully_connected(&self) -> bool {
        Peer::successfully_connected(self)
    }
    fn first_message_time(&self) -> i64 {
        Peer::first_message_time(self)
    }
    fn set_first_message(&self, time: i64, is_mnauth: bool) {
        Peer::set_first_message(self, time, is_mnauth);
    }
    fn masternode_probe(&self) -> bool {
        Peer::masternode_probe(self)
    }
    fn mark_disconnect(&self) {
        Peer::mark_disconnect(self);
    }
}

/// Core's gates 1, 2, 5, 6 and 7 for `cmd` from `peer`, in Core's order, with Core's log lines.
/// Gate 6 records `nTimeFirstMessageReceived` and `fFirstMessageIsMNAUTH` on the first admitted
/// message after `verack`, as Core does; gates 6 and 7 set `fDisconnect` on the peer
/// ([`Gate::disconnects`]). `now` is `GetTime<seconds>()` and `reject_incoming_txs` is the
/// processor's `RejectIncomingTxs(peer)`. Gates 3 and 4 admit `verack` and the five control
/// messages before or after `verack`; their branches are the processor's.
// Core: net_processing.cpp:3935-3939, 4178-4182, 4363-4386 ProcessMessage
pub fn admit<P: GatePeer + ?Sized>(
    peer: &P,
    cmd: &CommandString,
    now: i64,
    reject_incoming_txs: bool,
) -> Gate {
    let id = peer.id();
    if *cmd == VERSION {
        if peer.version() != 0 {
            tracing::debug!(target: "net", "redundant version message from peer={id}");
            return Gate::RedundantVersion;
        }
        return Gate::Admit;
    }

    if peer.version() == 0 {
        // Must have a version message before anything else.
        tracing::debug!(
            target: "net",
            "non-version message before version handshake. Message \"{}\" from peer={id}",
            sanitize_command(cmd.as_bytes())
        );
        return Gate::BeforeVersion;
    }

    // Gates 3 and 4: `verack`, `sendheaders`, `sendheaders2`, `sendcmpct`, `sendaddrv2` and
    // `sendtxrcncl` have branches before the `fSuccessfullyConnected` check.
    if policy::lookup(cmd).is_some_and(|row| row.gate == GateClass::BeforeOrAfterVerack) {
        return Gate::Admit;
    }

    if !peer.successfully_connected() {
        tracing::debug!(
            target: "net",
            "Unsupported message \"{}\" prior to verack from peer={id}",
            sanitize_command(cmd.as_bytes())
        );
        return Gate::BeforeVerack;
    }

    if peer.first_message_time() == 0 {
        // First message after VERSION/VERACK.
        let is_mnauth = *cmd == MNAUTH;
        peer.set_first_message(now, is_mnauth);
        // Note: do not break the flow here.
        if peer.masternode_probe() && !is_mnauth {
            tracing::debug!(
                target: "net",
                "connection is a masternode probe but first received message is not MNAUTH, peer={id}"
            );
            peer.mark_disconnect();
            return Gate::ProbeWithoutMnauth;
        }
    }

    // Stop processing non-block data early in blocks only mode and for block-relay-only peers.
    if reject_incoming_txs && policy::violates_blocks_only(cmd) {
        // Core logs `msg_type` unsanitized; every command in the set is printable.
        tracing::debug!(target: "net", "{} sent in violation of protocol peer={id}", cmd.as_str());
        peer.mark_disconnect();
        return Gate::BlocksOnlyViolation;
    }

    Gate::Admit
}
