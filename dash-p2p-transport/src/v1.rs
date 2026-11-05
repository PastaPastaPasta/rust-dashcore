//! The V1 transport, ported from Dash Core's `V1Transport` (`net.cpp`, v24.0.0-rc.3 lines
//! 694-882): 24-byte headers with the network magic, the command, the length and a SHA256d
//! checksum, then the payload in the clear.

use dash_p2p_core::{Magic, V1FrameError, V1Header};
use dash_sha256::Sha256d;

use crate::message::{NetMessage, OutMsg, Reject, RejectedMessage, TransportError};
use crate::{BytesToSend, TransportInfo, TransportKind};

/// Payload bytes allocated ahead of what has arrived (Core's 256 KiB).
const MAX_RESERVE_AHEAD: usize = 256 * 1024;

/// Core's `V1Transport`.
pub struct V1Transport {
    magic: Magic,
    // Receive side.
    hdr_buf: [u8; V1Header::SIZE],
    hdr_pos: usize,
    hdr: V1Header,
    in_data: bool,
    data: Vec<u8>,
    data_pos: usize,
    hasher: Sha256d,
    // Send side.
    header_to_send: [u8; V1Header::SIZE],
    message_to_send: OutMsg,
    sending_header: bool,
    bytes_sent: usize,
}

impl V1Transport {
    /// A transport for the network whose message start is `magic`.
    pub fn new(magic: Magic) -> Self {
        Self {
            magic,
            hdr_buf: [0; V1Header::SIZE],
            hdr_pos: 0,
            hdr: V1Header::from_bytes(&[0; V1Header::SIZE]),
            in_data: false,
            data: Vec::new(),
            data_pos: 0,
            hasher: Sha256d::new(),
            header_to_send: [0; V1Header::SIZE],
            message_to_send: OutMsg::default(),
            sending_header: false,
            bytes_sent: 0,
        }
    }

    /// Always V1, without a session ID.
    pub fn info(&self) -> TransportInfo {
        TransportInfo {
            kind: TransportKind::V1,
            session_id: None,
        }
    }

    fn reset(&mut self) {
        self.data = Vec::new();
        self.hdr_pos = 0;
        self.in_data = false;
        self.data_pos = 0;
        self.hasher = Sha256d::new();
    }

    /// Whether a whole message has arrived ([`Self::received_message`] may be called).
    pub fn received_message_complete(&self) -> bool {
        self.in_data && self.hdr.length as usize == self.data_pos
    }

    /// Feeds wire bytes, consuming them from the front of `bytes`: one step per call (the rest of
    /// the header or of the payload), as Core's `ReceivedBytes`, so callers loop until `bytes` is
    /// empty, taking each complete message. On an error the connection must be closed.
    pub fn received_bytes(&mut self, bytes: &mut &[u8]) -> Result<(), TransportError> {
        let res = if self.in_data {
            Ok(self.read_data(bytes))
        } else {
            self.read_header(bytes)
        };
        match res {
            Ok(n) => {
                *bytes = &bytes[n..];
                Ok(())
            }
            Err(e) => {
                self.reset();
                Err(e)
            }
        }
    }

    fn read_header(&mut self, bytes: &[u8]) -> Result<usize, TransportError> {
        let n = (V1Header::SIZE - self.hdr_pos).min(bytes.len());
        self.hdr_buf[self.hdr_pos..self.hdr_pos + n].copy_from_slice(&bytes[..n]);
        self.hdr_pos += n;
        if self.hdr_pos < V1Header::SIZE {
            return Ok(n);
        }
        self.hdr = V1Header::from_bytes(&self.hdr_buf);
        match self.hdr.check(self.magic) {
            Ok(()) => {}
            Err(V1FrameError::WrongMagic {
                magic,
            }) => return Err(TransportError::WrongMessageStart(magic)),
            Err(_) => {
                return Err(TransportError::MessageTooLarge {
                    command: self.hdr.command,
                    size: self.hdr.length,
                });
            }
        }
        self.in_data = true;
        Ok(n)
    }

    fn read_data(&mut self, bytes: &[u8]) -> usize {
        let size = self.hdr.length as usize;
        let n = (size - self.data_pos).min(bytes.len());
        if self.data.len() < self.data_pos + n {
            // Allocate up to 256 KiB ahead, but never more than the total message size.
            self.data.resize(size.min(self.data_pos + n + MAX_RESERVE_AHEAD), 0);
        }
        self.hasher.update(&bytes[..n]);
        self.data[self.data_pos..self.data_pos + n].copy_from_slice(&bytes[..n]);
        self.data_pos += n;
        n
    }

    /// Takes the completed message; call only when [`Self::received_message_complete`]. A
    /// message with a bad checksum or invalid command bytes is dropped (`Err`) and the
    /// connection stays usable. `time_us` is the receive time to stamp it with.
    ///
    /// # Panics
    ///
    /// May panic if no message is complete.
    pub fn received_message(&mut self, time_us: i64) -> Result<NetMessage, RejectedMessage> {
        debug_assert!(self.received_message_complete());
        let size = self.hdr.length as usize;
        let raw_size = size + V1Header::SIZE;
        let hash = core::mem::take(&mut self.hasher).finalize();
        let reason = if hash[..4] != self.hdr.checksum {
            Some(Reject::BadChecksum)
        } else if !self.hdr.command.is_valid_v1() {
            Some(Reject::InvalidCommand)
        } else {
            None
        };
        let payload = core::mem::take(&mut self.data);
        debug_assert_eq!(payload.len(), size);
        let command = self.hdr.command;
        self.reset();
        match reason {
            Some(reason) => Err(RejectedMessage {
                raw_size,
                reason,
            }),
            None => Ok(NetMessage {
                command,
                payload,
                time_us,
                raw_size,
            }),
        }
    }

    /// Hands the transport the next message to send. Gives it back while the previous one is
    /// still being sent.
    pub fn set_message_to_send(&mut self, msg: OutMsg) -> Result<(), OutMsg> {
        if self.sending_header || self.bytes_sent < self.message_to_send.payload.len() {
            return Err(msg);
        }
        let mut hasher = Sha256d::new();
        hasher.update(&msg.payload);
        let hash = hasher.finalize();
        let header = V1Header {
            magic: self.magic,
            command: msg.command,
            // Core writes the size as a `uint32_t`; a caller never builds a payload of 4 GiB.
            length: msg.payload.len() as u32,
            checksum: [hash[0], hash[1], hash[2], hash[3]],
        };
        self.header_to_send = header.to_bytes();
        self.message_to_send = msg;
        self.sending_header = true;
        self.bytes_sent = 0;
        Ok(())
    }

    /// The bytes to write next. `more` says whether more bytes follow once these are sent;
    /// `have_next_message` says whether the caller has another message queued.
    pub fn bytes_to_send(&self, have_next_message: bool) -> BytesToSend<'_> {
        let command = self.message_to_send.command;
        if self.sending_header {
            BytesToSend {
                data: &self.header_to_send[self.bytes_sent..],
                more: have_next_message || !self.message_to_send.payload.is_empty(),
                command,
            }
        } else {
            BytesToSend {
                data: &self.message_to_send.payload[self.bytes_sent..],
                more: have_next_message,
                command,
            }
        }
    }

    /// Reports that the first `n` bytes of the last [`Self::bytes_to_send`] went out.
    ///
    /// # Panics
    ///
    /// A later call may panic if `n` exceeds the length of those bytes.
    pub fn mark_bytes_sent(&mut self, n: usize) {
        self.bytes_sent += n;
        if self.sending_header && self.bytes_sent == self.header_to_send.len() {
            self.sending_header = false;
            self.bytes_sent = 0;
        } else if !self.sending_header && self.bytes_sent == self.message_to_send.payload.len() {
            self.message_to_send.payload = Vec::new();
            self.bytes_sent = 0;
        }
    }

    /// Core's `GetSendMemoryUsage`: the message being sent, kept whole until its last byte goes.
    /// The payload's allocation is its `Vec`'s capacity, so the charge follows how the caller
    /// built the payload (Core's serializer sizes its own vector).
    pub fn send_memory_usage(&self) -> usize {
        self.message_to_send.memory_usage()
    }

    /// Never: a V1 connection is not retried.
    pub fn should_reconnect_v1(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use dash_p2p_core::CommandString;

    use super::*;

    const MAGIC: Magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);

    fn wire(t: &mut V1Transport, msg: OutMsg) -> Vec<u8> {
        t.set_message_to_send(msg).unwrap();
        let mut out = Vec::new();
        loop {
            let b = t.bytes_to_send(false);
            if b.data.is_empty() {
                break;
            }
            let n = b.data.len();
            out.extend_from_slice(b.data);
            t.mark_bytes_sent(n);
        }
        out
    }

    #[test]
    fn round_trip_byte_by_byte() {
        let mut tx = V1Transport::new(MAGIC);
        let mut rx = V1Transport::new(MAGIC);
        let bytes = wire(&mut tx, OutMsg::new(CommandString::PING, vec![1, 2, 3, 4, 5, 6, 7, 8]));
        assert_eq!(bytes.len(), 32);
        for (i, b) in bytes.iter().enumerate() {
            let mut s: &[u8] = core::slice::from_ref(b);
            rx.received_bytes(&mut s).unwrap();
            assert!(s.is_empty());
            assert_eq!(rx.received_message_complete(), i == bytes.len() - 1);
        }
        let msg = rx.received_message(7).unwrap();
        assert_eq!(msg.command, CommandString::PING);
        assert_eq!(msg.payload, [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((msg.time_us, msg.raw_size), (7, 32));
    }

    #[test]
    fn empty_payload_completes_at_header() {
        let mut tx = V1Transport::new(MAGIC);
        let mut rx = V1Transport::new(MAGIC);
        let bytes = wire(&mut tx, OutMsg::new(CommandString::VERACK, Vec::new()));
        let mut s = &bytes[..];
        rx.received_bytes(&mut s).unwrap();
        assert!(rx.received_message_complete());
        assert_eq!(rx.received_message(0).unwrap().command, CommandString::VERACK);
    }

    #[test]
    fn send_blocks_until_message_is_out() {
        let mut t = V1Transport::new(MAGIC);
        assert_eq!(t.send_memory_usage(), 56);
        t.set_message_to_send(OutMsg::new(CommandString::PING, vec![0; 8])).unwrap();
        assert!(t.set_message_to_send(OutMsg::default()).is_err());
        assert!(t.bytes_to_send(false).more, "the payload follows the header");
        t.mark_bytes_sent(24);
        assert!(!t.bytes_to_send(false).more);
        assert!(t.bytes_to_send(true).more);
        t.mark_bytes_sent(3);
        // A partly sent payload still counts whole (Core's charge).
        assert!(t.set_message_to_send(OutMsg::default()).is_err());
        assert!(t.send_memory_usage() > 56);
        t.mark_bytes_sent(5);
        assert_eq!(t.send_memory_usage(), 56);
        assert!(t.set_message_to_send(OutMsg::default()).is_ok());
    }
}
