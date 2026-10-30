//! What crosses the transport: a message to send, a received message, and why a received message
//! or the connection was refused.

use core::fmt;

use dash_p2p_core::{CommandString, Magic};

/// `sizeof(CSerializedNetMsg)` with libstdc++: a `std::vector` and a `std::string`.
#[cfg(target_pointer_width = "64")]
pub const SERIALIZED_MSG_SIZE: usize = 56;
/// `sizeof(CSerializedNetMsg)` with libstdc++: a `std::vector` and a `std::string`.
#[cfg(not(target_pointer_width = "64"))]
pub const SERIALIZED_MSG_SIZE: usize = 36;

/// `sizeof(std::vector<uint8_t>)` with libstdc++.
pub(crate) const VECTOR_SIZE: usize = 3 * core::mem::size_of::<usize>();

/// Core's `memusage::MallocUsage`: what glibc's malloc spends on an allocation of `alloc` bytes.
pub const fn malloc_usage(alloc: usize) -> usize {
    if alloc == 0 {
        0
    } else if cfg!(target_pointer_width = "64") {
        ((alloc + 31) >> 4) << 4
    } else {
        ((alloc + 15) >> 3) << 3
    }
}

/// A message to send: its type and its serialized payload (Core's `CSerializedNetMsg`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutMsg {
    /// The message type.
    pub command: CommandString,
    /// The serialized payload.
    pub payload: Vec<u8>,
}

impl Default for OutMsg {
    /// The empty type with no payload.
    fn default() -> Self {
        Self {
            command: CommandString::EMPTY,
            payload: Vec::new(),
        }
    }
}

impl OutMsg {
    /// A message of type `command` with `payload`.
    pub fn new(command: CommandString, payload: Vec<u8>) -> Self {
        Self {
            command,
            payload,
        }
    }

    /// Core's `CSerializedNetMsg::GetMemoryUsage`: the object plus the payload's allocation, taken
    /// as the payload's capacity. Core charges this to `fPauseSend` while the message is queued
    /// and, in a V1 transport, until its last byte is sent.
    pub fn memory_usage(&self) -> usize {
        SERIALIZED_MSG_SIZE + malloc_usage(self.payload.capacity())
    }
}

/// A message received whole (Core's `CNetMessage`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetMessage {
    /// The message type. A V2 short ID that Core reserves without a message (5, and 29 to 32)
    /// gives the empty type, which Core's handlers treat as unknown.
    pub command: CommandString,
    /// The payload.
    pub payload: Vec<u8>,
    /// The receive time the caller passed in, in microseconds (`m_time`).
    pub time_us: i64,
    /// Bytes the message took on the wire (`m_raw_message_size`).
    pub raw_size: usize,
}

/// Why a received message was dropped. The connection stays usable (Core's `reject_message`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// V1: the payload does not match the header's checksum.
    BadChecksum,
    /// V1: the command has bytes Core's `IsCommandValid` refuses.
    InvalidCommand,
    /// V2: the contents carry no valid message type (empty contents, a short ID outside Core's
    /// ranges, or a malformed long-form type).
    InvalidType,
}

/// A received message the transport dropped. Core counts its size under the `*other*` type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RejectedMessage {
    /// Bytes the message took on the wire.
    pub raw_size: usize,
    /// Why it was dropped.
    pub reason: Reject,
}

/// Why the transport refused the peer's bytes. The connection must be closed (Core's
/// `ReceivedBytes` returning false); the transport can't be used any more.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// V1: the header's message start is not the network's.
    WrongMessageStart(Magic),
    /// V1: the header announces a payload above Core's 3 MiB limit.
    MessageTooLarge {
        /// The header's command, unchecked: peer bytes (`Display` sanitizes them).
        command: CommandString,
        /// The announced payload length.
        size: u32,
    },
    /// V2 responder: the peer opened with another network's V1 `version` header.
    V1PeerWrongMessageStart(Magic),
    /// V2: no garbage terminator in the first 4111 bytes after the key.
    MissingGarbageTerminator,
    /// V2: a packet announces contents above the limit.
    PacketTooLarge(u32),
    /// V2: a packet failed authentication.
    DecryptionFailure(u32),
}

impl fmt::Display for TransportError {
    /// Core's log text for each case.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongMessageStart(magic) => {
                write!(f, "Header error: Wrong MessageStart {} received", hex(magic.as_bytes()))
            }
            Self::MessageTooLarge {
                command,
                size,
            } => {
                let command = sanitize_command(command.as_bytes());
                write!(f, "Header error: Size too large ({command}, {size} bytes)")
            }
            Self::V1PeerWrongMessageStart(magic) => {
                write!(
                    f,
                    "V2 transport error: V1 peer with wrong MessageStart {}",
                    hex(magic.as_bytes())
                )
            }
            Self::MissingGarbageTerminator => {
                f.write_str("V2 transport error: missing garbage terminator")
            }
            Self::PacketTooLarge(len) => {
                write!(f, "V2 transport error: packet too large ({len} bytes)")
            }
            Self::DecryptionFailure(len) => {
                write!(f, "V2 transport error: packet decryption failure ({len} bytes)")
            }
        }
    }
}

impl fmt::Debug for TransportError {
    /// As derived, but with the unchecked command's bytes escaped, so peer bytes can't forge log
    /// lines through `{:?}` either.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongMessageStart(magic) => {
                f.debug_tuple("WrongMessageStart").field(magic).finish()
            }
            Self::MessageTooLarge {
                command,
                size,
            } => f
                .debug_struct("MessageTooLarge")
                .field("command", &format_args!("\"{}\"", command.as_bytes().escape_ascii()))
                .field("size", size)
                .finish(),
            Self::V1PeerWrongMessageStart(magic) => {
                f.debug_tuple("V1PeerWrongMessageStart").field(magic).finish()
            }
            Self::MissingGarbageTerminator => f.write_str("MissingGarbageTerminator"),
            Self::PacketTooLarge(len) => f.debug_tuple("PacketTooLarge").field(len).finish(),
            Self::DecryptionFailure(len) => f.debug_tuple("DecryptionFailure").field(len).finish(),
        }
    }
}

impl std::error::Error for TransportError {}

/// Core's `SanitizeString(hdr.GetCommand())`: the command up to its first NUL, keeping only
/// `SAFE_CHARS_DEFAULT` (letters, digits and ` .,;-_/:?@()`), so peer bytes can't forge log lines.
pub fn sanitize_command(command: &[u8]) -> String {
    command
        .iter()
        .take_while(|&&b| b != 0)
        .filter(|b| b.is_ascii_alphanumeric() || b" .,;-_/:?@()".contains(b))
        .map(|&b| char::from(b))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
