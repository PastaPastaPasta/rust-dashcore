//! The BIP324 V2 transport with Dash's short IDs, ported from Dash Core's `V2Transport`
//! (`net.cpp`, v24.0.0-rc.3 lines 885-1690), including its V1 fallback for responders.
//!
//! ```text
//! receive:  start(responder) -> KEY_MAYBE_V1 -> KEY -> GARB_GARBTERM -> VERSION -> APP <-> APP_READY
//!           start(initiator) ---------------^                 KEY_MAYBE_V1 -> V1
//! send:     start(responder) -> MAYBE_V1 -> AWAITING_KEY -> READY;  MAYBE_V1 -> V1
//!           start(initiator) -------------^
//! ```

use dash_p2p_core::limits::MAX_V2_CONTENTS_LEN;
use dash_p2p_core::{CommandString, Magic, ProtocolVersion, ShortId, V1Header};
use secp256k1::SecretKey;
use zeroize::Zeroizing;

use crate::cipher::{
    Bip324Cipher, ELLSWIFT_KEY_LEN, EXPANSION, GARBAGE_TERMINATOR_LEN, HEADER_LEN, LENGTH_LEN,
};
use crate::message::{
    NetMessage, OutMsg, Reject, RejectedMessage, TransportError, VECTOR_SIZE, malloc_usage,
};
use crate::v1::V1Transport;
use crate::{BytesToSend, RngError, TransportInfo, TransportKind};

/// The longest garbage a side may send, Core's `V2Transport::MAX_GARBAGE_LEN`.
pub const MAX_GARBAGE_LEN: usize = 4095;
/// Bytes a responder reads before deciding between V1 and V2, Core's `V1_PREFIX_LEN`.
pub const V1_PREFIX_LEN: usize = 16;
/// Short IDs 0 to 32 are BIP324's slots (`V2_BITCOIN_IDS` has 33 entries).
const BITCOIN_IDS: u8 = 33;
/// Short IDs 128 to 168 are Dash's slots (`V2_DASH_IDS` has 41 entries).
const DASH_IDS: u8 = 41;
/// Bytes allocated ahead of what has arrived in a packet (Core's `MAX_RESERVE_AHEAD`).
const MAX_RESERVE_AHEAD: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecvState {
    KeyMaybeV1,
    Key,
    GarbGarbterm,
    Version,
    App,
    AppReady,
    V1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendState {
    MaybeV1,
    AwaitingKey,
    Ready,
    V1,
}

/// Whether a first contents byte is in one of Core's short-ID ranges (`IsValidV2ShortID`).
/// Slots in a range without a message (5, and 29 to 32) give the empty type.
pub fn is_valid_short_id(id: u8) -> bool {
    id < BITCOIN_IDS || (id >= 128 && id - 128 < DASH_IDS)
}

/// The length of a long message type: a zero byte and the 12-byte command.
const LONG_TYPE_LEN: usize = 13;

/// Splits decrypted contents into the message type and the offset of the payload, Core's
/// `V2Transport::GetMessageType`. `None` means the message is dropped.
pub fn message_type(contents: &[u8]) -> Option<(CommandString, usize)> {
    let (&first, rest) = contents.split_first()?;
    if first != 0 {
        if !is_valid_short_id(first) {
            return None;
        }
        return Some((ShortId(first).to_command().unwrap_or(CommandString::EMPTY), 1));
    }
    let bytes: &[u8; 12] = rest.first_chunk()?;
    let command = CommandString::from_bytes(*bytes);
    command.is_valid_v2().then_some((command, LONG_TYPE_LEN))
}

/// Draws garbage as Core's `GenerateRandomGarbage` does: a length uniform in 0 to 4095, then that
/// many random bytes.
pub fn random_garbage() -> Result<Vec<u8>, RngError> {
    let mut len = [0u8; 2];
    getrandom::fill(&mut len).map_err(RngError)?;
    let mut garbage = vec![0u8; garbage_len(len)];
    getrandom::fill(&mut garbage).map_err(RngError)?;
    Ok(garbage)
}

/// The garbage length for two random bytes: their low 12 bits, uniform in 0 to 4095 because 4096
/// divides 65536.
fn garbage_len(random: [u8; 2]) -> usize {
    usize::from(u16::from_le_bytes(random) & 0x0fff)
}

/// Core's `V2Transport`.
pub struct V2Transport {
    cipher: Bip324Cipher,
    initiating: bool,
    v1_prefix: [u8; V1_PREFIX_LEN],
    v1_fallback: V1Transport,
    // Receive side.
    recv_state: RecvState,
    /// In `Version` and `App`, the decrypted contents length once the length descriptor is in.
    recv_len: u32,
    /// The bytes of the current state; in `AppReady`, the decrypted packet (length descriptor,
    /// header, contents, tag) whose contents [`Self::received_message`] hands out.
    recv_buffer: Vec<u8>,
    /// The garbage received, authenticated with the first packet.
    recv_aad: Vec<u8>,
    // Send side.
    send_state: SendState,
    send_buffer: Vec<u8>,
    /// The capacity Core's `std::vector` send buffer would have (libstdc++ growth), for
    /// [`Self::send_memory_usage`].
    send_capacity: usize,
    send_pos: usize,
    send_garbage: Vec<u8>,
    send_type: CommandString,
    sent_v1_header_worth: bool,
    peer_version: ProtocolVersion,
}

impl V2Transport {
    /// A transport with a fresh random key and random garbage (Core's production constructor).
    /// `magic` is the network's message start; `initiating` is true for outbound connections.
    ///
    /// # Errors
    ///
    /// When the operating system's random source fails.
    pub fn new(magic: Magic, initiating: bool) -> Result<Self, RngError> {
        let mut ent32 = [0u8; 32];
        getrandom::fill(&mut ent32).map_err(RngError)?;
        let garbage = random_garbage()?;
        let mut key = loop {
            let mut bytes = Zeroizing::new([0u8; 32]);
            getrandom::fill(&mut bytes[..]).map_err(RngError)?;
            if let Ok(key) = SecretKey::from_secret_bytes(*bytes) {
                break key;
            }
        };
        let t = Self::with_keys(magic, initiating, key, ent32, garbage);
        key.non_secure_erase();
        Ok(t)
    }

    /// Core's production constructor with every random byte from `rand_bytes(out, strong)`, in
    /// Core's order: the key (`GetStrongRandBytes` until it is a valid secret), the ElligatorSwift
    /// entropy (`GetRandHash`), then `garbage()` (`GenerateRandomGarbage`'s own context).
    ///
    /// # Panics
    ///
    /// If the garbage is longer than [`MAX_GARBAGE_LEN`].
    // Core: net.cpp:1099-1101 (the braced delegating constructor evaluates left to right);
    // key.cpp:160-166 CKey::MakeNewKey; net.cpp:1062-1068 GenerateRandomGarbage
    pub fn with_rand(
        magic: Magic,
        initiating: bool,
        mut rand_bytes: impl FnMut(&mut [u8], bool),
        garbage: impl FnOnce() -> Vec<u8>,
    ) -> Self {
        let mut key = loop {
            let mut bytes = Zeroizing::new([0u8; 32]);
            rand_bytes(&mut bytes[..], true);
            if let Ok(key) = SecretKey::from_secret_bytes(*bytes) {
                break key;
            }
        };
        let mut ent32 = [0u8; 32];
        rand_bytes(&mut ent32, false);
        let t = Self::with_keys(magic, initiating, key, ent32, garbage());
        key.non_secure_erase();
        t
    }

    /// A transport with a given key, ElligatorSwift entropy and garbage (Core's test
    /// constructor).
    ///
    /// # Panics
    ///
    /// If `garbage` is longer than [`MAX_GARBAGE_LEN`].
    pub fn with_keys(
        magic: Magic,
        initiating: bool,
        mut key: SecretKey,
        ent32: [u8; 32],
        garbage: Vec<u8>,
    ) -> Self {
        assert!(garbage.len() <= MAX_GARBAGE_LEN);
        let cipher = Bip324Cipher::new(magic, key, ent32);
        key.non_secure_erase();
        let mut t = Self {
            cipher,
            initiating,
            v1_prefix: V1Header::version_prefix(magic),
            v1_fallback: V1Transport::new(magic),
            recv_state: if initiating {
                RecvState::Key
            } else {
                RecvState::KeyMaybeV1
            },
            recv_len: 0,
            recv_buffer: Vec::new(),
            recv_aad: Vec::new(),
            send_state: if initiating {
                SendState::AwaitingKey
            } else {
                SendState::MaybeV1
            },
            send_buffer: Vec::new(),
            send_capacity: 0,
            send_pos: 0,
            send_garbage: garbage,
            send_type: CommandString::EMPTY,
            sent_v1_header_worth: false,
            peer_version: ProtocolVersion::INIT,
        };
        if initiating {
            t.start_sending_handshake();
        }
        t
    }

    /// Grows the send buffer to `len` bytes, tracking libstdc++'s capacity (`_M_check_len`).
    fn send_resize(&mut self, len: usize) {
        let size = self.send_buffer.len();
        if len > self.send_capacity {
            self.send_capacity = size + size.max(len - size);
        }
        self.send_buffer.resize(len, 0);
    }

    fn send_clear_shrink(&mut self) {
        self.send_buffer = Vec::new();
        self.send_capacity = 0;
    }

    /// Puts our public key and garbage in the send buffer.
    fn start_sending_handshake(&mut self) {
        debug_assert!(self.send_buffer.is_empty());
        self.send_resize(ELLSWIFT_KEY_LEN + self.send_garbage.len());
        self.send_buffer[..ELLSWIFT_KEY_LEN].copy_from_slice(self.cipher.our_pubkey());
        self.send_buffer[ELLSWIFT_KEY_LEN..].copy_from_slice(&self.send_garbage);
    }

    /// Whether a whole message has arrived ([`Self::received_message`] may be called).
    pub fn received_message_complete(&self) -> bool {
        match self.recv_state {
            RecvState::V1 => self.v1_fallback.received_message_complete(),
            state => state == RecvState::AppReady,
        }
    }

    fn process_maybe_v1_bytes(&mut self) {
        let n = self.recv_buffer.len();
        if self.recv_buffer[..] != self.v1_prefix[..n] {
            // Not a V1 `version` header: a V2 key. Start sending ours.
            self.recv_state = RecvState::Key;
            self.send_state = SendState::AwaitingKey;
            self.start_sending_handshake();
        } else if n == V1_PREFIX_LEN {
            // A V1 `version` header: fall back, handing V1 the bytes already read.
            let mut feed = &self.recv_buffer[..];
            let ret = self.v1_fallback.received_bytes(&mut feed);
            debug_assert!(ret.is_ok() && feed.is_empty());
            self.recv_state = RecvState::V1;
            self.send_state = SendState::V1;
            self.recv_buffer = Vec::new();
            self.send_clear_shrink();
        }
    }

    fn process_key_bytes(&mut self) -> Result<(), TransportError> {
        const OFFSET: usize = 4;
        // A responder that sees bytes 4 to 16 of a V1 `version` header after another network's
        // magic disconnects (Core logs the case specially).
        if !self.initiating
            && self.recv_buffer.len() >= V1_PREFIX_LEN
            && self.recv_buffer[OFFSET..V1_PREFIX_LEN] == self.v1_prefix[OFFSET..]
        {
            let mut magic = [0u8; 4];
            magic.copy_from_slice(&self.recv_buffer[..OFFSET]);
            return Err(TransportError::V1PeerWrongMessageStart(Magic::from_bytes(magic)));
        }
        if self.recv_buffer.len() == ELLSWIFT_KEY_LEN {
            let mut theirs = [0u8; ELLSWIFT_KEY_LEN];
            theirs.copy_from_slice(&self.recv_buffer);
            self.cipher.initialize(&theirs, self.initiating, false);
            self.recv_state = RecvState::GarbGarbterm;
            self.recv_buffer.clear();
            self.send_state = SendState::Ready;

            // The garbage terminator, then the version packet with our garbage as AAD.
            let start = self.send_buffer.len();
            self.send_resize(start + GARBAGE_TERMINATOR_LEN);
            let terminator = *self.cipher.send_garbage_terminator();
            self.send_buffer[start..].copy_from_slice(&terminator);
            let start = self.send_buffer.len();
            self.send_resize(start + EXPANSION);
            let garbage = core::mem::take(&mut self.send_garbage);
            self.cipher.encrypt(&[], &garbage, false, &mut self.send_buffer[start..]);
        }
        Ok(())
    }

    fn process_garbage_bytes(&mut self) -> Result<(), TransportError> {
        let n = self.recv_buffer.len();
        if n >= GARBAGE_TERMINATOR_LEN {
            if self.recv_buffer[n - GARBAGE_TERMINATOR_LEN..]
                == self.cipher.recv_garbage_terminator()[..]
            {
                self.recv_buffer.truncate(n - GARBAGE_TERMINATOR_LEN);
                self.recv_aad = core::mem::take(&mut self.recv_buffer);
                self.recv_state = RecvState::Version;
            } else if n == MAX_GARBAGE_LEN + GARBAGE_TERMINATOR_LEN {
                return Err(TransportError::MissingGarbageTerminator);
            }
        }
        Ok(())
    }

    fn process_packet_bytes(&mut self) -> Result<(), TransportError> {
        let n = self.recv_buffer.len();
        if n == LENGTH_LEN {
            let len: &[u8; LENGTH_LEN] =
                self.recv_buffer[..].try_into().unwrap_or_else(|_| unreachable!());
            self.recv_len = self.cipher.decrypt_length(len);
            if self.recv_len as usize > MAX_V2_CONTENTS_LEN {
                return Err(TransportError::PacketTooLarge(self.recv_len));
            }
        } else if n > LENGTH_LEN && n == self.recv_len as usize + EXPANSION {
            let aad = core::mem::take(&mut self.recv_aad);
            let Some(ignore) = self.cipher.decrypt(&mut self.recv_buffer[LENGTH_LEN..], &aad)
            else {
                return Err(TransportError::DecryptionFailure(self.recv_len));
            };
            if !ignore {
                match self.recv_state {
                    // The version packet: its contents are ignored (future extensions).
                    RecvState::Version => self.recv_state = RecvState::App,
                    RecvState::App => self.recv_state = RecvState::AppReady,
                    _ => unreachable!("packet bytes outside VERSION and APP"),
                }
            }
            if self.recv_state != RecvState::AppReady {
                self.recv_buffer = Vec::new();
            }
        }
        Ok(())
    }

    fn max_bytes_to_process(&self) -> usize {
        match self.recv_state {
            // Never more than the V1 prefix, so V1 can still take the bytes whole.
            RecvState::KeyMaybeV1 => V1_PREFIX_LEN - self.recv_buffer.len(),
            // Only the key: locating the terminator needs the key exchange first.
            RecvState::Key => ELLSWIFT_KEY_LEN - self.recv_buffer.len(),
            // One by one: the terminator may end anywhere.
            RecvState::GarbGarbterm => 1,
            RecvState::Version | RecvState::App => {
                if self.recv_buffer.len() < LENGTH_LEN {
                    LENGTH_LEN - self.recv_buffer.len()
                } else {
                    EXPANSION + self.recv_len as usize - self.recv_buffer.len()
                }
            }
            RecvState::AppReady => 0,
            RecvState::V1 => unreachable!("V1 bytes go to the fallback"),
        }
    }

    /// Feeds wire bytes, consuming them from the front of `bytes`. Stops early once a message is
    /// complete, and when the responder falls back to V1; take the message with
    /// [`Self::received_message`] and feed the rest, so callers loop until `bytes` is empty (as
    /// [`crate::Transport::receive`] does). On an error the connection must be closed.
    pub fn received_bytes(&mut self, bytes: &mut &[u8]) -> Result<(), TransportError> {
        if self.recv_state == RecvState::V1 {
            return self.v1_fallback.received_bytes(bytes);
        }
        while !bytes.is_empty() {
            if self.recv_state == RecvState::AppReady {
                return Ok(());
            }
            let max_read = self.max_bytes_to_process();
            let n = max_read.min(bytes.len());
            // Reserve only when the bytes don't fit, as Core does: reserving on every step would
            // reallocate on every fragment of a large packet.
            if self.recv_buffer.len() + n > self.recv_buffer.capacity() {
                let target = match self.recv_state {
                    // Once, to fit the largest key and garbage (4111 bytes).
                    RecvState::KeyMaybeV1 | RecvState::Key | RecvState::GarbGarbterm => {
                        MAX_GARBAGE_LEN + GARBAGE_TERMINATOR_LEN
                    }
                    // As much as the packet still needs, but at most 256 KiB beyond what has
                    // arrived, so a peer can't make us allocate what it never sends.
                    RecvState::Version | RecvState::App => {
                        self.recv_buffer.len() + max_read.min(bytes.len() + MAX_RESERVE_AHEAD)
                    }
                    RecvState::AppReady | RecvState::V1 => unreachable!(),
                };
                self.recv_buffer.reserve_exact(target.saturating_sub(self.recv_buffer.len()));
            }
            self.recv_buffer.extend_from_slice(&bytes[..n]);
            *bytes = &bytes[n..];
            match self.recv_state {
                RecvState::KeyMaybeV1 => {
                    self.process_maybe_v1_bytes();
                    if self.recv_state == RecvState::V1 {
                        return Ok(());
                    }
                }
                RecvState::Key => self.process_key_bytes()?,
                RecvState::GarbGarbterm => self.process_garbage_bytes()?,
                RecvState::Version | RecvState::App => self.process_packet_bytes()?,
                RecvState::AppReady | RecvState::V1 => unreachable!(),
            }
        }
        Ok(())
    }

    /// Takes the completed message; call only when [`Self::received_message_complete`].
    /// Contents without a valid message type are dropped (`Err`); the connection stays usable.
    ///
    /// # Panics
    ///
    /// May panic if no message is complete.
    pub fn received_message(&mut self, time_us: i64) -> Result<NetMessage, RejectedMessage> {
        if self.recv_state == RecvState::V1 {
            return self.v1_fallback.received_message(time_us);
        }
        debug_assert_eq!(self.recv_state, RecvState::AppReady);
        let len = self.recv_len as usize;
        let raw_size = len + EXPANSION;
        let mut buf = core::mem::take(&mut self.recv_buffer);
        self.recv_state = RecvState::App;
        let start = LENGTH_LEN + HEADER_LEN;
        let contents = &buf[start..start + len];
        let Some((command, offset)) = message_type(contents) else {
            return Err(RejectedMessage {
                raw_size,
                reason: Reject::InvalidType,
            });
        };
        // Move the payload to the front of the buffer instead of copying it out.
        buf.copy_within(start + offset..start + len, 0);
        buf.truncate(len - offset);
        Ok(NetMessage {
            command,
            payload: buf,
            time_us,
            raw_size,
        })
    }

    /// Hands the transport the next message to send. Gives it back before the handshake is done
    /// (no cipher yet) and while earlier bytes are still being sent.
    pub fn set_message_to_send(&mut self, msg: OutMsg) -> Result<(), OutMsg> {
        if self.send_state == SendState::V1 {
            return self.v1_fallback.set_message_to_send(msg);
        }
        if !(self.send_state == SendState::Ready && self.send_buffer.is_empty()) {
            return Err(msg);
        }
        let OutMsg {
            command,
            payload,
        } = msg;
        let (type_len, short) = match ShortId::for_send(&command, self.peer_version) {
            Some(id) => (1, Some(id)),
            None => (LONG_TYPE_LEN, None),
        };
        let contents_len = type_len + payload.len();
        self.send_resize(contents_len + EXPANSION);
        let start = LENGTH_LEN + HEADER_LEN;
        let contents = &mut self.send_buffer[start..start + contents_len];
        match short {
            Some(id) => contents[0] = id.0,
            None => {
                contents[0] = 0;
                contents[1..LONG_TYPE_LEN].copy_from_slice(command.as_bytes());
            }
        }
        contents[type_len..].copy_from_slice(&payload);
        self.cipher.encrypt_in_place(&mut self.send_buffer, &[], false);
        self.send_type = command;
        Ok(())
    }

    /// Sets the version short IDs are negotiated by: the connection's common version, which Core
    /// sets once the peer's `version` is processed (`CNode::SetCommonVersion`). Until then every
    /// message goes in the long format.
    pub fn set_peer_version(&mut self, version: ProtocolVersion) {
        self.peer_version = version;
    }

    /// The bytes to write next. `more` is false before the handshake completes, when nothing else
    /// could be sent yet even with a message queued.
    pub fn bytes_to_send(&self, have_next_message: bool) -> BytesToSend<'_> {
        if self.send_state == SendState::V1 {
            return self.v1_fallback.bytes_to_send(have_next_message);
        }
        BytesToSend {
            data: &self.send_buffer[self.send_pos..],
            more: have_next_message && self.send_state == SendState::Ready,
            command: self.send_type,
        }
    }

    /// Reports that the first `n` bytes of the last [`Self::bytes_to_send`] went out.
    ///
    /// # Panics
    ///
    /// A later call may panic if `n` exceeds the length of those bytes.
    pub fn mark_bytes_sent(&mut self, n: usize) {
        if self.send_state == SendState::V1 {
            return self.v1_fallback.mark_bytes_sent(n);
        }
        self.send_pos += n;
        debug_assert!(self.send_pos <= self.send_buffer.len());
        if self.send_pos >= V1Header::SIZE {
            self.sent_v1_header_worth = true;
        }
        if self.send_pos == self.send_buffer.len() {
            self.send_pos = 0;
            self.send_clear_shrink();
        }
    }

    /// Core's `GetSendMemoryUsage`: the send buffer, as Core's `std::vector` would hold it.
    pub fn send_memory_usage(&self) -> usize {
        match self.send_state {
            SendState::V1 => self.v1_fallback.send_memory_usage(),
            _ => VECTOR_SIZE + malloc_usage(self.send_capacity),
        }
    }

    /// Whether to retry the connection with V1 after it closes, Core's `ShouldReconnectV1`: an
    /// outbound connection that received nothing after sending at least a V1 header's worth (24
    /// bytes, enough for a V1 peer to drop us).
    pub fn should_reconnect_v1(&self) -> bool {
        self.initiating
            && self.recv_state == RecvState::Key
            && self.recv_buffer.is_empty()
            && self.sent_v1_header_worth
    }

    /// V2 with the session ID once the peer's version packet is verified, `Detecting` before, or
    /// the fallback's V1.
    pub fn info(&self) -> TransportInfo {
        match self.recv_state {
            RecvState::V1 => self.v1_fallback.info(),
            RecvState::KeyMaybeV1
            | RecvState::Key
            | RecvState::GarbGarbterm
            | RecvState::Version => TransportInfo {
                kind: TransportKind::Detecting,
                session_id: None,
            },
            RecvState::App | RecvState::AppReady => TransportInfo {
                kind: TransportKind::V2,
                session_id: Some(*self.cipher.session_id()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: Magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);

    fn key(n: u8) -> SecretKey {
        SecretKey::from_secret_bytes([n; 32]).unwrap()
    }

    /// Moves `from`'s pending bytes to `to`; whether there were any.
    fn transfer(from: &mut V2Transport, to: &mut V2Transport) -> bool {
        let data = from.bytes_to_send(false).data.to_vec();
        from.mark_bytes_sent(data.len());
        let mut rest = &data[..];
        while !rest.is_empty() {
            to.received_bytes(&mut rest).unwrap();
        }
        !data.is_empty()
    }

    /// Every pair of random bytes gives a length of at most `MAX_GARBAGE_LEN`, and each length
    /// comes from exactly 16 of the 65536 pairs: the draw is uniform on 0 to 4095, as Core's
    /// `GenerateRandomGarbage`. The dashd interop test's KS check only catches gross deviations.
    #[test]
    fn garbage_length_is_uniform_up_to_the_limit() {
        let mut counts = vec![0u32; MAX_GARBAGE_LEN + 1];
        for raw in 0..=u16::MAX {
            counts[garbage_len(raw.to_le_bytes())] += 1;
        }
        assert!(counts.iter().all(|&n| n == 16));
        assert_eq!(garbage_len([0xff, 0xff]), MAX_GARBAGE_LEN);
        assert_eq!(garbage_len([0x34, 0x12]), 0x234);
        assert_eq!(garbage_len([0x00, 0x10]), 0);
        for _ in 0..64 {
            assert!(random_garbage().unwrap().len() <= MAX_GARBAGE_LEN);
        }
    }

    /// The receiver reads only the ignore bit of a packet's header: the bits BIP324 reserves
    /// change nothing (Core's `BIP324Cipher::Decrypt` returns `header & IGNORE_BIT`). Senders
    /// can't set them; the peer here uses the `test-peer` hook.
    #[test]
    fn reserved_header_bits_are_ignored() {
        for (header, delivered) in [(0x7f, true), (0x01, true), (0xff, false), (0x81, false)] {
            let mut a = V2Transport::with_keys(MAGIC, true, key(1), [2; 32], Vec::new());
            let mut b = V2Transport::with_keys(MAGIC, false, key(3), [4; 32], Vec::new());
            while transfer(&mut a, &mut b) | transfer(&mut b, &mut a) {}
            assert_eq!(b.info().kind, TransportKind::V2);

            let mut packet = vec![0; 1 + 4 + EXPANSION];
            packet[LENGTH_LEN + HEADER_LEN..][..5].copy_from_slice(&[18, 1, 2, 3, 4]);
            a.cipher.encrypt_with_header(&mut packet, &[], header);
            let mut rest = &packet[..];
            b.received_bytes(&mut rest).unwrap();
            assert!(rest.is_empty());
            assert_eq!(b.received_message_complete(), delivered, "header {header:#04x}");
            if delivered {
                let msg = b.received_message(0).unwrap();
                assert_eq!(msg.command, CommandString::from_static("ping"));
                assert_eq!(msg.payload, [1, 2, 3, 4]);
            }
        }
    }
}
