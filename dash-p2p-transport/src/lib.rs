//! Dash Core's P2P transports for the shared network stack (SB0 §2.2, layer S; PLAN P1).
//!
//! [`V1Transport`] is the plaintext framing; [`V2Transport`] is BIP324 encryption with Dash's
//! salt (`"bitcoin_v2_shared_secret"` and the network's message start) and Dash's short IDs
//! (128 to 168), with Core's responder-side V1 detection and fallback. Both are ports of Dash
//! Core v24.0.0-rc.3's `Transport` classes (`net.cpp`), kept as sans-io state machines with
//! Core's interface, so a connection driver feeds them socket bytes and drains their output:
//!
//! - receive: [`Transport::received_bytes`] until [`Transport::received_message_complete`],
//!   then [`Transport::received_message`] (or [`Transport::receive`] for the whole loop of
//!   Core's `CNode::ReceiveMsgBytes`);
//! - send: [`Transport::set_message_to_send`], then [`Transport::bytes_to_send`] and
//!   [`Transport::mark_bytes_sent`] as the socket accepts bytes (Core's `SocketSendData`);
//! - [`Transport::send_memory_usage`] is Core's charge towards `fPauseSend`, and
//!   [`Transport::should_reconnect_v1`] says when an outbound V2 attempt is retried with V1.
//!
//! Framing and short IDs come from base-sdk's `dash-p2p-core`; the cipher suite is in
//! [`cipher`]. The crate starts no threads, opens no sockets and keeps no global state (SB0
//! rule L5); randomness comes from the operating system through `getrandom` only in
//! [`V2Transport::new`], a convenience constructor. [`V2Transport::with_rand`] takes the caller's
//! source in Core's draw order, as `dash-p2p` does.
//!
//! Peer management, the handshake messages and their processing are not here: they belong to
//! `dash-p2p` (SB6) and the processors.

pub mod cipher;
mod message;
mod v1;
mod v2;

use core::fmt;

pub use dash_p2p_core::{CommandString, Magic, ProtocolVersion};
pub use message::{
    NetMessage, OutMsg, Reject, RejectedMessage, SERIALIZED_MSG_SIZE, TransportError, malloc_usage,
    sanitize_command,
};
pub use v1::V1Transport;
pub use v2::{
    MAX_GARBAGE_LEN, V1_PREFIX_LEN, V2Transport, is_valid_short_id, message_type, random_garbage,
};

/// What a transport speaks, Core's `TransportProtocolType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportKind {
    /// Plaintext V1.
    V1,
    /// A V2 transport that has not yet verified the peer's version packet.
    Detecting,
    /// BIP324 V2.
    V2,
}

impl TransportKind {
    /// Core's `TransportTypeAsString` (`getpeerinfo`'s `transport_protocol_type`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::Detecting => "detecting",
            Self::V2 => "v2",
        }
    }
}

/// Core's `Transport::Info`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransportInfo {
    /// The protocol.
    pub kind: TransportKind,
    /// The BIP324 session ID, once V2 is established.
    pub session_id: Option<[u8; 32]>,
}

/// Core's `Transport::BytesToSend`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BytesToSend<'a> {
    /// The bytes to write; possibly empty.
    pub data: &'a [u8],
    /// Whether more bytes will follow once these are sent (Core sets `MSG_MORE` from it).
    pub more: bool,
    /// The type of the message the bytes belong to (the empty type for handshake bytes).
    pub command: CommandString,
}

/// The operating system's random source failed.
#[derive(Debug)]
pub struct RngError(pub getrandom::Error);

impl fmt::Display for RngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "random source failed: {}", self.0)
    }
}

impl std::error::Error for RngError {}

/// A connection's transport: V1, or V2 (which may itself fall back to V1 as a responder).
pub enum Transport {
    /// Plaintext V1.
    V1(Box<V1Transport>),
    /// BIP324 V2.
    V2(Box<V2Transport>),
}

macro_rules! dispatch {
    ($self:ident, $t:ident => $e:expr) => {
        match $self {
            Transport::V1($t) => $e,
            Transport::V2($t) => $e,
        }
    };
}

impl Transport {
    /// A V1 transport.
    pub fn v1(magic: Magic) -> Self {
        Self::V1(Box::new(V1Transport::new(magic)))
    }

    /// A V2 transport with a fresh key and garbage. An inbound (`initiating == false`) V2
    /// transport also accepts V1 peers.
    ///
    /// # Errors
    ///
    /// When the operating system's random source fails.
    pub fn v2(magic: Magic, initiating: bool) -> Result<Self, RngError> {
        Ok(Self::V2(Box::new(V2Transport::new(magic, initiating)?)))
    }

    /// See [`V2Transport::info`].
    pub fn info(&self) -> TransportInfo {
        dispatch!(self, t => t.info())
    }

    /// See [`V2Transport::received_message_complete`].
    pub fn received_message_complete(&self) -> bool {
        dispatch!(self, t => t.received_message_complete())
    }

    /// See [`V2Transport::received_bytes`].
    ///
    /// # Errors
    ///
    /// When the peer's bytes break the protocol; close the connection.
    pub fn received_bytes(&mut self, bytes: &mut &[u8]) -> Result<(), TransportError> {
        dispatch!(self, t => t.received_bytes(bytes))
    }

    /// See [`V2Transport::received_message`].
    ///
    /// # Errors
    ///
    /// When the message is dropped; the connection stays usable.
    pub fn received_message(&mut self, time_us: i64) -> Result<NetMessage, RejectedMessage> {
        dispatch!(self, t => t.received_message(time_us))
    }

    /// Core's `CNode::ReceiveMsgBytes` loop: feeds all of `bytes` and passes every message they
    /// complete, in order and stamped with `time_us`, to `on_message` (dropped ones as `Err`).
    ///
    /// # Errors
    ///
    /// When the peer's bytes break the protocol; close the connection. Messages completed
    /// before the bad bytes have been passed on.
    pub fn receive(
        &mut self,
        mut bytes: &[u8],
        time_us: i64,
        mut on_message: impl FnMut(Result<NetMessage, RejectedMessage>),
    ) -> Result<(), TransportError> {
        while !bytes.is_empty() {
            self.received_bytes(&mut bytes)?;
            if self.received_message_complete() {
                on_message(self.received_message(time_us));
            }
        }
        Ok(())
    }

    /// See [`V2Transport::set_message_to_send`].
    ///
    /// # Errors
    ///
    /// Gives the message back when the transport can't take it yet.
    pub fn set_message_to_send(&mut self, msg: OutMsg) -> Result<(), OutMsg> {
        dispatch!(self, t => t.set_message_to_send(msg))
    }

    /// See [`V2Transport::bytes_to_send`].
    pub fn bytes_to_send(&self, have_next_message: bool) -> BytesToSend<'_> {
        dispatch!(self, t => t.bytes_to_send(have_next_message))
    }

    /// See [`V2Transport::mark_bytes_sent`].
    pub fn mark_bytes_sent(&mut self, n: usize) {
        dispatch!(self, t => t.mark_bytes_sent(n))
    }

    /// See [`V2Transport::send_memory_usage`].
    pub fn send_memory_usage(&self) -> usize {
        dispatch!(self, t => t.send_memory_usage())
    }

    /// See [`V2Transport::set_peer_version`]; V1 ignores it.
    pub fn set_peer_version(&mut self, version: ProtocolVersion) {
        if let Self::V2(t) = self {
            t.set_peer_version(version);
        }
    }

    /// See [`V2Transport::should_reconnect_v1`].
    pub fn should_reconnect_v1(&self) -> bool {
        dispatch!(self, t => t.should_reconnect_v1())
    }
}
