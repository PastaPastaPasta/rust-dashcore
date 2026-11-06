//! `p2p_transport_model` (P1): the transport state machines of `dash-p2p-transport` against
//! reference models. The first input byte picks one of four checks:
//!
//! - **pair**: a V2 initiator and responder with keys and garbage from the input run a script
//!   of queued messages, byte transfers in input-sized chunks and peer-version changes (short-ID
//!   gating). Every message must arrive once, in order and unchanged, with no transport error;
//!   at the end both sides are V2 with one session ID and nothing left to send.
//! - **v1**: the rest of the input, fed in input-sized chunks to a V1 transport, against an
//!   independent parser of Core's V1 framing (`V1Transport::ReceivedBytes`/`GetReceivedMessage`):
//!   the same messages, drops and disconnect.
//! - **responder**: the rest of the input fed to a V2 responder. Its V1 detection must be exact
//!   (V1 if and only if the first 16 bytes are this network's `version` header prefix, after
//!   which it must match the V1 model on the whole input), and a V1 header of another network
//!   must be refused.
//! - **scripted**: a V2 transport (either role) against a bare `Bip324Cipher` peer whose keys,
//!   garbage, terminator, packets (decoys, reserved header bits, version contents, short, long
//!   and malformed types, large contents) and faults (bad tag, body or AAD, forged length
//!   descriptor, stream cut at a decision boundary) come from the input. The messages, drops and
//!   disconnect must be those Core's `V2Transport` gives, worked out here from `net.cpp` without
//!   the transport; and the receive buffer must grow as Core's does (at most once per 256 KiB of
//!   a packet, never more than 256 KiB ahead of what arrived), measured by [`CountingAlloc`]
//!   where it is the global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::VecDeque;

use dash_p2p_transport::cipher::{
    Bip324Cipher, ELLSWIFT_KEY_LEN, EXPANSION, GARBAGE_TERMINATOR_LEN, IGNORE_BIT, LENGTH_LEN,
    SecretKey,
};
use dash_p2p_transport::{
    CommandString, MAX_GARBAGE_LEN, Magic, NetMessage, OutMsg, ProtocolVersion, Reject,
    RejectedMessage, Transport, TransportError, TransportKind, V1_PREFIX_LEN, V2Transport,
};
use dash_sha256::Sha256d;
use node_l1::Divergence;

/// Regtest's message start (the transports take any; one network suffices).
const MAGIC: Magic = Magic::from_bytes([0xfc, 0xc1, 0xb7, 0xdc]);
const MAX_PROTOCOL_MESSAGE_LENGTH: usize = 3 * 1024 * 1024;
/// Script steps per input, and payload bytes per message: enough for rekeying (224 packets)
/// within the fuzzer's time limit.
const MAX_STEPS: usize = 1024;
const MAX_PAYLOAD: usize = 4096;
/// Commands with a short ID from the baseline, the late `platformban`, none (`barfoo`), and the
/// empty type (sent as reserved short ID 5).
const COMMANDS: &[&str] =
    &["ping", "pong", "tx", "spork", "qrinfo", "platformban", "version", "barfoo", ""];
const PEER_VERSIONS: &[i32] = &[209, 70234, 70235, 70239, 70240, 70242];
/// Core's protocol version before the peer's `version` arrives (`INIT_PROTO_VERSION`).
const INIT_VERSION: i32 = 209;

/// The common version from which Core sends `command` by a short ID (`V2_MESSAGE_IDS` and
/// `GetMessageMinVersion` in v24 `net.cpp`; the empty type takes reserved slot 5), written out
/// here rather than taken from `dash-p2p-core` so that the model checks the transport's table.
fn short_from(command: &str) -> Option<i32> {
    match command {
        "ping" | "pong" | "tx" | "spork" | "qrinfo" | "" => Some(70235),
        "platformban" => Some(70240),
        _ => None,
    }
}

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

/// The input as a stream of numbers; zeros once it runs out.
struct Input<'a>(&'a [u8]);

impl Input<'_> {
    fn u8(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }
    fn array32(&mut self) -> [u8; 32] {
        core::array::from_fn(|_| self.u8())
    }
    fn key(&mut self) -> SecretKey {
        let bytes = self.array32();
        SecretKey::from_secret_bytes(bytes)
            .or_else(|_| SecretKey::from_secret_bytes([1; 32]))
            .unwrap_or_else(|_| unreachable!("1 is a valid key"))
    }
    /// A chunk size: mostly small, sometimes everything.
    fn chunk(&mut self) -> usize {
        match self.u8() {
            0 => usize::MAX,
            n => usize::from(n),
        }
    }
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let mut input = Input(data);
    match input.u8() % 4 {
        0 => pair(input),
        3 => scripted(input),
        1 => {
            let chunk = input.chunk();
            let mut t = Transport::v1(MAGIC);
            if feed(&mut t, input.0, chunk) != v1_model(input.0) {
                return Err(diverged("v1 framing"));
            }
            Ok(())
        }
        _ => responder(input),
    }
}

/// The first 16 bytes of a V1 `version` header on this network.
fn v1_prefix() -> [u8; V1_PREFIX_LEN] {
    let mut prefix = [0u8; V1_PREFIX_LEN];
    prefix[..4].copy_from_slice(MAGIC.as_bytes());
    prefix[4..11].copy_from_slice(b"version");
    prefix
}

/// What a transport makes of a byte stream: the messages (or drops) it delivered and whether
/// it refused the stream.
#[derive(PartialEq, Eq)]
struct Outcome {
    messages: Vec<Result<(CommandString, Vec<u8>), Reject>>,
    error: Option<TransportError>,
}

fn feed(t: &mut Transport, bytes: &[u8], chunk: usize) -> Outcome {
    let mut out = Outcome {
        messages: Vec::new(),
        error: None,
    };
    for piece in bytes.chunks(chunk.min(bytes.len()).max(1)) {
        let res = t.receive(piece, 0, |m| {
            out.messages
                .push(m.map(|m| (m.command, m.payload)).map_err(|r: RejectedMessage| r.reason));
        });
        if let Err(e) = res {
            out.error = Some(e);
            break;
        }
    }
    out
}

/// Core's V1 framing, written from `net.cpp` independently of `V1Transport`: 24-byte headers
/// (message start, command, LE32 length, checksum); a wrong message start or a length above
/// 3 MiB ends the stream; a bad checksum, then a command `IsCommandValid` refuses, drops the
/// message. A trailing partial message delivers nothing.
fn v1_model(bytes: &[u8]) -> Outcome {
    let mut out = Outcome {
        messages: Vec::new(),
        error: None,
    };
    let mut rest = bytes;
    while rest.len() >= 24 {
        let (hdr, body) = rest.split_at(24);
        let magic = Magic::from_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let mut command = [0u8; 12];
        command.copy_from_slice(&hdr[4..16]);
        let command = CommandString::from_bytes(command);
        let len = u32::from_le_bytes([hdr[16], hdr[17], hdr[18], hdr[19]]);
        if magic != MAGIC {
            out.error = Some(TransportError::WrongMessageStart(magic));
            return out;
        }
        if len as usize > MAX_PROTOCOL_MESSAGE_LENGTH {
            out.error = Some(TransportError::MessageTooLarge {
                command,
                size: len,
            });
            return out;
        }
        if body.len() < len as usize {
            break;
        }
        let (payload, next) = body.split_at(len as usize);
        let mut h = Sha256d::new();
        h.update(payload);
        let checksum = h.finalize();
        let printable_then_nul = {
            let end = command.as_bytes().iter().position(|&b| b == 0).unwrap_or(12);
            command.as_bytes()[..end].iter().all(|b| (0x20..=0x7e).contains(b))
                && command.as_bytes()[end..].iter().all(|&b| b == 0)
        };
        out.messages.push(if checksum[..4] != hdr[20..24] {
            Err(Reject::BadChecksum)
        } else if !printable_then_nul {
            Err(Reject::InvalidCommand)
        } else {
            Ok((command, payload.to_vec()))
        });
        rest = next;
    }
    out
}

fn responder(mut input: Input<'_>) -> Result<(), Divergence> {
    let chunk = input.chunk();
    let key = input.key();
    let ent = input.array32();
    let bytes = input.0;
    let mut t = Transport::V2(Box::new(V2Transport::with_keys(MAGIC, false, key, ent, Vec::new())));
    let out = feed(&mut t, bytes, chunk);
    let prefix = v1_prefix();
    let n = bytes.len().min(V1_PREFIX_LEN);
    let is_v1 = bytes[..n] == prefix[..n];
    if is_v1 && bytes.len() >= V1_PREFIX_LEN {
        if t.info().kind != TransportKind::V1 {
            return Err(diverged("responder: V1 not detected"));
        }
        if out != v1_model(bytes) {
            return Err(diverged("responder: V1 fallback framing"));
        }
        return Ok(());
    }
    if t.info().kind == TransportKind::V1 {
        return Err(diverged("responder: V1 detected on V2 bytes"));
    }
    // Core checks for a wrong-network V1 peer in the key state, so not when the first 16 bytes
    // arrive in the step that leaves the V1-detection state and nothing follows them.
    let wrong_network = !is_v1
        && bytes.len() >= V1_PREFIX_LEN
        && bytes[4..V1_PREFIX_LEN] == prefix[4..]
        && (bytes.len() > V1_PREFIX_LEN || chunk < V1_PREFIX_LEN);
    match out.error {
        Some(TransportError::V1PeerWrongMessageStart(magic)) if wrong_network => {
            if magic.as_bytes()[..] != bytes[..4] {
                return Err(diverged("responder: wrong-network magic"));
            }
        }
        _ if wrong_network => return Err(diverged("responder: wrong-network V1 peer accepted")),
        Some(TransportError::V1PeerWrongMessageStart(_)) => {
            return Err(diverged("responder: V1 peer refused early"));
        }
        Some(TransportError::MissingGarbageTerminator)
            if bytes.len() < ELLSWIFT_KEY_LEN + MAX_GARBAGE_LEN + GARBAGE_TERMINATOR_LEN =>
        {
            return Err(diverged("responder: garbage refused early"));
        }
        _ => {}
    }
    // The bytes after a key are garbage, and the fuzzer may find the terminator (comparison
    // hooks see it), but the version packet's tag cannot be forged: never a message.
    if !out.messages.is_empty() {
        return Err(diverged("responder: message from garbage"));
    }
    Ok(())
}

/// One side of the pair: its transport, the peer version it was given, the messages queued for
/// it, and the messages it handed to the transport that the other side has yet to receive (with
/// whether they go by short ID).
struct Side {
    t: V2Transport,
    peer_version: i32,
    queue: VecDeque<(&'static str, OutMsg)>,
    in_flight: VecDeque<(OutMsg, bool)>,
}

impl Side {
    fn new(initiating: bool, input: &mut Input<'_>) -> Self {
        let key = input.key();
        let ent = input.array32();
        let len = usize::from(input.u16()) % (MAX_GARBAGE_LEN + 1);
        let garbage = (0..len).map(|i| i as u8).collect();
        let t = V2Transport::with_keys(MAGIC, initiating, key, ent, garbage);
        Self {
            t,
            peer_version: INIT_VERSION,
            queue: VecDeque::new(),
            in_flight: VecDeque::new(),
        }
    }

    /// Hands the transport the next queued message if it takes one.
    fn offer(&mut self) {
        if let Some((name, msg)) = self.queue.pop_front() {
            match self.t.set_message_to_send(msg.clone()) {
                Ok(()) => {
                    let short = short_from(name).is_some_and(|v| self.peer_version >= v);
                    self.in_flight.push_back((msg, short));
                }
                Err(_) => self.queue.push_front((name, msg)),
            }
        }
    }
}

/// Moves up to `max` bytes from `from` to `to` and checks what `to` delivers. Returns the
/// number of bytes moved.
fn transfer(from: &mut Side, to: &mut Side, max: usize) -> Result<usize, Divergence> {
    from.offer();
    let data = from.t.bytes_to_send(!from.queue.is_empty()).data;
    let n = data.len().min(max);
    let chunk = data[..n].to_vec();
    from.t.mark_bytes_sent(n);
    let mut bytes = &chunk[..];
    while !bytes.is_empty() {
        to.t.received_bytes(&mut bytes).map_err(|_| diverged("pair: transport error"))?;
        if to.t.received_message_complete() {
            let got: NetMessage =
                to.t.received_message(0).map_err(|_| diverged("pair: message dropped"))?;
            let (want, short) =
                from.in_flight.pop_front().ok_or_else(|| diverged("pair: unsent message"))?;
            if got.command != want.command || got.payload != want.payload {
                return Err(diverged("pair: message changed"));
            }
            let type_len = if short {
                1
            } else {
                13
            };
            if got.raw_size != type_len + want.payload.len() + EXPANSION {
                return Err(diverged("pair: short ID gating"));
            }
        }
    }
    Ok(n)
}

fn pair(mut input: Input<'_>) -> Result<(), Divergence> {
    let mut sides = [Side::new(true, &mut input), Side::new(false, &mut input)];
    for _ in 0..MAX_STEPS {
        if input.0.is_empty() {
            break;
        }
        let op = input.u8();
        let s = usize::from(op & 1);
        match (op >> 1) % 4 {
            0 => {
                let name = COMMANDS[usize::from(input.u8()) % COMMANDS.len()];
                let len = usize::from(input.u16()) % (MAX_PAYLOAD + 1);
                let payload = (0..len).map(|i| (i as u8) ^ op).collect();
                let msg = OutMsg::new(CommandString::from_static(name), payload);
                sides[s].queue.push_back((name, msg));
            }
            1 | 2 => {
                let max = input.chunk();
                let [a, b] = &mut sides;
                if s == 0 {
                    transfer(a, b, max)?
                } else {
                    transfer(b, a, max)?
                };
            }
            _ => {
                let v = PEER_VERSIONS[usize::from(input.u8()) % PEER_VERSIONS.len()];
                sides[s].t.set_peer_version(ProtocolVersion(v));
                sides[s].peer_version = v;
            }
        }
    }
    // Drain: both sides send until nothing moves.
    loop {
        let [a, b] = &mut sides;
        if transfer(a, b, usize::MAX)? + transfer(b, a, usize::MAX)? == 0
            && a.queue.is_empty()
            && b.queue.is_empty()
        {
            break;
        }
    }
    let [a, b] = &sides;
    if !(a.in_flight.is_empty() && b.in_flight.is_empty()) {
        return Err(diverged("pair: message lost"));
    }
    let (ia, ib) = (a.t.info(), b.t.info());
    if ia.kind != TransportKind::V2
        || ib.kind != TransportKind::V2
        || ia.session_id != ib.session_id
    {
        return Err(diverged("pair: session"));
    }
    if a.t.should_reconnect_v1() || b.t.should_reconnect_v1() {
        return Err(diverged("pair: V1 reconnect"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use dash_p2p_transport::V1Transport;

    use super::*;

    const OTHER_MAGIC: Magic = Magic::from_bytes([0xbf, 0x0c, 0x6b, 0xbd]);

    #[test]
    fn handles_edge_inputs() {
        for data in [&[][..], &[0], &[1], &[2], &[0; 64], &[1; 64], &[2; 200], &[0, 1, 2, 3, 4, 5]]
        {
            assert!(check(data).is_ok());
        }
    }

    #[test]
    fn v1_model_matches_frames() {
        let mut t = V1Transport::new(MAGIC);
        let mut wire = Vec::new();
        for (c, p) in [("ping", vec![1; 8]), ("pi\x01g", vec![]), ("verack", vec![])] {
            assert!(t.set_message_to_send(OutMsg::new(CommandString::from_static(c), p)).is_ok());
            loop {
                let d = t.bytes_to_send(false).data;
                if d.is_empty() {
                    break;
                }
                let n = d.len();
                wire.extend_from_slice(d);
                t.mark_bytes_sent(n);
            }
        }
        wire[24 + 8 + 20] ^= 1; // the second message's checksum
        let mut input = vec![1, 7];
        input.extend_from_slice(&wire);
        assert!(check(&input).is_ok());
        let m = v1_model(&wire);
        assert_eq!(m.messages.len(), 3);
        assert_eq!(m.messages[1], Err(Reject::BadChecksum));
        let mut bad = wire.clone();
        bad[..4].copy_from_slice(OTHER_MAGIC.as_bytes());
        assert_eq!(v1_model(&bad).error, Some(TransportError::WrongMessageStart(OTHER_MAGIC)));
    }

    #[test]
    fn responder_wrong_network_v1_peer() {
        // Core detects a wrong-network V1 peer in its key state: not for exactly 16 bytes that
        // arrive in the step that leaves V1 detection.
        let mut prefix = OTHER_MAGIC.as_bytes().to_vec();
        prefix.extend_from_slice(b"version\0\0\0\0\0");
        for (chunk, extra) in [(0, 0), (0, 1), (16, 0), (5, 0), (5, 3)] {
            let mut input = vec![2, chunk];
            input.extend_from_slice(&[0x33; 32 + 32]);
            input.extend_from_slice(&prefix);
            input.extend(core::iter::repeat_n(0, extra));
            assert!(check(&input).is_ok(), "chunk {chunk}, extra {extra}");
        }
    }

    /// A scripted input: mode, transport role, chunk size, both sides' keys, the cut, five bytes
    /// of garbage, an intact terminator, then `ops` (each packet's op, its fields, its fault).
    fn script(initiating: bool, chunk: u8, cut: u8, ops: &[u8]) -> Vec<u8> {
        let mut input = vec![3, u8::from(initiating), chunk];
        for b in [0x11, 0x22, 0x33, 0x44] {
            input.extend_from_slice(&[b; 32]);
        }
        input.extend_from_slice(&[cut, 10, 1, 2, 3, 4, 5, 1]);
        input.extend_from_slice(ops);
        input
    }

    /// The version packet, then contents of the most allowed size (Core's 3 MiB plus a long
    /// type) trickled in 1- and 64-byte pieces, then a ping: delivered whole, with the buffer
    /// growing at most once per 256 KiB.
    #[test]
    fn scripted_large_packet_in_fragments() {
        const VERSION: &[u8] = &[2, 0, 31];
        const LARGEST: &[u8] = &[18, 0, 0xf0, 2, 31];
        const PING: &[u8] = &[22, 10, 31];
        for (initiating, chunk) in [(true, 1), (false, 1), (true, 64), (false, 0)] {
            let input = script(initiating, chunk, 0, &[VERSION, LARGEST, PING].concat());
            assert!(check(&input).is_ok(), "chunk {chunk}");
        }
    }

    /// Decoys, reserved header bits, long and malformed types, then each fault, alone and cut
    /// at each boundary: the model's outcome is the transport's.
    #[test]
    fn scripted_faults_and_cuts() {
        let body: &[u8] = &[
            2, 0, 31, // the version packet
            0, 0, 31, // a decoy
            21, 0x7f, 3, 31, // reserved header bits, a ping
            10, 0, b'v', b'e', b'r', b'a', b'c', b'k', 0, 0, 0, 0, 0, 0, 31, // a long type
            10, 0, b'a', 0, b'b', 0, 0, 0, 0, 0, 0, 0, 0, 0, 31, // a malformed long type
            6, 0, 5, 31, // reserved short ID 5: the empty type
            6, 0, 200, 31, // short ID 200: dropped
        ];
        let faults: &[&[u8]] = &[
            &[22, 4, 0],
            &[22, 4, 1],
            &[22, 4, 2],
            &[22, 4, 3, 2, 0],    // more than the limit
            &[22, 4, 3, 3, 0],    // the limit, never sent
            &[22, 4, 3, 0xff, 0], // the limit less 63
            &[22, 4, 3, 8, 0],    // two bytes
            &[22, 4, 31],
        ];
        for fault in faults {
            for cut in 0..4 {
                for initiating in [true, false] {
                    let input = script(initiating, 7, cut, &[body, fault].concat());
                    assert!(check(&input).is_ok(), "fault {fault:?} cut {cut}");
                }
            }
        }
        // A fault in the first packet: its AAD is the garbage.
        for fault in [0, 1, 2] {
            assert!(check(&script(true, 0, 0, &[2, 0, fault])).is_ok());
        }
    }

    #[test]
    fn expected_type_follows_core() {
        let long = |ty: &[u8; 12]| [&[0][..], ty, &[9]].concat();
        assert_eq!(expected_type(&[]), None);
        assert_eq!(expected_type(&[5, 1]), Some((CommandString::EMPTY, &[1][..])));
        assert_eq!(expected_type(&[33]), None);
        assert_eq!(expected_type(&[168]).map(|t| t.1.len()), Some(0));
        assert_eq!(expected_type(&[169]), None);
        assert_eq!(expected_type(&long(b"ping\x7f\0\0\0\0\0\0\0")).map(|t| t.1), Some(&[9][..]));
        assert_eq!(expected_type(&long(b"ping\x80\0\0\0\0\0\0\0")), None);
        assert_eq!(expected_type(&long(b"pi\0g\0\0\0\0\0\0\0\0")), None);
        assert_eq!(expected_type(&long(&[0; 12])).map(|t| t.0), Some(CommandString::EMPTY));
        assert_eq!(expected_type(&[0; 12]), None);
    }

    #[test]
    fn pair_delivers_a_script() {
        // Queue 300 pings on each side and move everything in 7-byte chunks: past a rekey.
        let mut input = vec![0];
        input.extend_from_slice(&[0x11; 32 + 32]);
        input.extend_from_slice(&[0, 1]);
        input.extend_from_slice(&[0x22; 32 + 32]);
        input.extend_from_slice(&[100, 0]);
        for _ in 0..300 {
            input.extend_from_slice(&[0, 0, 8, 0, 1, 0, 8, 0]);
        }
        input.extend_from_slice(&[6, 5, 7, 5]);
        for _ in 0..200 {
            input.extend_from_slice(&[2, 7, 3, 7]);
        }
        assert!(check(&input).is_ok());
    }
}

#[path = "../../../dash-p2p-transport/tests/data/core_short_ids.rs"]
mod core_short_ids;

/// Packets per scripted input, and Core's limits: the receive buffer's reserve-ahead
/// (`MAX_RESERVE_AHEAD`) and the largest packet contents (a long type and 3 MiB).
const MAX_PACKETS: usize = 32;
const RESERVE_AHEAD: usize = 256 * 1024;
const MAX_CONTENTS: usize = 1 + 12 + MAX_PROTOCOL_MESSAGE_LENGTH;
/// Allocations at least this large are counted against the growth bound.
const LARGE_ALLOC: usize = 16 * 1024;

/// The system allocator, counting on the threads that ask (each test runs on its own thread)
/// the allocations and reallocations of at least [`LARGE_ALLOC`] bytes, and the largest size
/// asked for. The target binary and this crate's tests install it; elsewhere nothing is counted
/// and the bounds hold trivially.
pub struct CountingAlloc;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static LARGE_GROWTHS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note_alloc(size: usize) {
    if size >= LARGE_ALLOC {
        let _ = COUNTING.try_with(|on| {
            if on.get() {
                let _ = LARGE_GROWTHS.try_with(|g| g.set(g.get() + 1));
                let _ = LARGEST.try_with(|l| l.set(l.get().max(size)));
            }
        });
    }
}

// SAFETY: every call is forwarded to `System` unchanged.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: the caller's guarantees are `System`'s.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: as above.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc(new_size);
        // SAFETY: as above.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[cfg(test)]
#[global_allocator]
static TEST_ALLOC: CountingAlloc = CountingAlloc;

/// Runs `f`, returning it, the large allocations it made on this thread and the largest of them.
fn count_large<T>(f: impl FnOnce() -> T) -> (T, usize, usize) {
    LARGE_GROWTHS.with(|g| g.set(0));
    LARGEST.with(|l| l.set(0));
    COUNTING.with(|on| on.set(true));
    let out = f();
    COUNTING.with(|on| on.set(false));
    (out, LARGE_GROWTHS.with(Cell::get), LARGEST.with(Cell::get))
}

/// Core's `V2Transport::GetMessageType`, written from v24 `net.cpp`: the type and payload of
/// packet contents, or `None` when the message is dropped. Short IDs come from Core's tables;
/// a long type is 12 bytes, 0x20 to 0x7F up to the first zero and zeros after it.
fn expected_type(contents: &[u8]) -> Option<(CommandString, &[u8])> {
    use core_short_ids::{V2_BITCOIN_IDS, V2_DASH_IDS};
    let (&first, rest) = contents.split_first()?;
    if first != 0 {
        let id = usize::from(first);
        let name = if id < V2_BITCOIN_IDS.len() {
            V2_BITCOIN_IDS[id]
        } else if (128..128 + V2_DASH_IDS.len()).contains(&id) {
            V2_DASH_IDS[id - 128]
        } else {
            return None;
        };
        return Some((CommandString::from_static(name), rest));
    }
    let (ty, payload) = rest.split_first_chunk::<12>()?;
    let len = ty.iter().position(|&b| b == 0).unwrap_or(ty.len());
    if ty[..len].iter().any(|b| !(0x20..=0x7f).contains(b)) || ty[len..].iter().any(|&b| b != 0) {
        return None;
    }
    Some((CommandString::from_bytes(*ty), payload))
}

/// A fault the scripted peer puts in a packet; every one ends the script.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// A bit of the tag, or of the encrypted header, flipped.
    Tag,
    Body,
    /// Authenticated with other associated data than the receiver expects.
    Aad,
    /// The length descriptor rewritten to announce this many contents bytes: a few, more than the
    /// limit, or a legal size near the limit that the peer never sends.
    Length(u32),
}

/// One packet of the script: where it starts in the stream and what the receiver must make of
/// it.
struct Packet {
    start: usize,
    contents: Vec<u8>,
    header: u8,
    aad: Vec<u8>,
    fault: Fault,
}

/// Packet contents from the input: empty, a short ID, a long type (raw bytes or a name), or a
/// ping; rarely (a selector byte of 240 or more, so that input running out reads as small) and
/// once per script, large contents (past the reserve-ahead, 1 MiB, or the most allowed), which
/// the unit tests also script directly.
fn contents(op: u8, input: &mut Input<'_>, large_done: &mut bool) -> Vec<u8> {
    let payload_len = match input.u8() {
        b if b & 0x80 != 0 => usize::from(input.u16()) % (MAX_PAYLOAD + 1),
        b => usize::from(b),
    };
    let payload = (0..payload_len).map(|i| (i as u8).wrapping_mul(31) ^ op);
    let mut c = Vec::new();
    match (op >> 2) % 8 {
        0 => {}
        1 => c.push(input.u8()),
        2 => {
            c.push(0);
            c.extend((0..12).map(|_| input.u8()));
        }
        3 => {
            c.push(0);
            let name = COMMANDS[usize::from(input.u8()) % COMMANDS.len()];
            c.extend_from_slice(CommandString::from_static(name).as_bytes());
        }
        4 if !*large_done && input.u8() >= 240 => {
            *large_done = true;
            let len = match input.u8() % 3 {
                0 => RESERVE_AHEAD + 1 + usize::from(input.u8()),
                1 => 1 << 20,
                _ => MAX_CONTENTS,
            };
            c.push(18);
            c.extend((1..len).map(|i| (i as u8) ^ op));
            return c;
        }
        _ => c.push(18),
    }
    c.extend(payload);
    c
}

fn scripted(mut input: Input<'_>) -> Result<(), Divergence> {
    let initiating = input.u8() & 1 == 1;
    let chunk = input.chunk();
    let key = input.key();
    let ent = input.array32();
    let mut t =
        Transport::V2(Box::new(V2Transport::with_keys(MAGIC, initiating, key, ent, Vec::new())));
    let mut peer = Bip324Cipher::new(MAGIC, input.key(), input.array32());

    // The key exchange. A responder must see a key that is not a V1 header (the responder
    // check covers those).
    let peer_key = *peer.our_pubkey();
    if !initiating && peer_key[4..V1_PREFIX_LEN] == v1_prefix()[4..] {
        return Ok(());
    }
    let out = feed(&mut t, &peer_key, chunk);
    if out.error.is_some() || !out.messages.is_empty() {
        return Err(diverged("scripted: key refused"));
    }
    // The transport's key, empty garbage, terminator and version packet.
    let sent = t.bytes_to_send(false).data.to_vec();
    let Some(their_key) = sent.first_chunk::<ELLSWIFT_KEY_LEN>() else {
        return Err(diverged("scripted: no key sent"));
    };
    peer.initialize(their_key, !initiating, false);
    let rest = &sent[ELLSWIFT_KEY_LEN..];
    let version_len = GARBAGE_TERMINATOR_LEN + EXPANSION;
    if rest.len() != version_len
        || rest[..GARBAGE_TERMINATOR_LEN] != peer.recv_garbage_terminator()[..]
    {
        return Err(diverged("scripted: terminator"));
    }
    let mut len = [0u8; LENGTH_LEN];
    len.copy_from_slice(&rest[GARBAGE_TERMINATOR_LEN..][..LENGTH_LEN]);
    let mut body = rest[GARBAGE_TERMINATOR_LEN + LENGTH_LEN..].to_vec();
    if peer.decrypt_length(&len) != 0 || peer.decrypt(&mut body, &[]) != Some(false) {
        return Err(diverged("scripted: version packet"));
    }

    // The script.
    let cut = input.u8() % 8;
    let garbage: Vec<u8> = match input.u8() {
        b if b & 1 == 0 => (0..usize::from(b >> 1) % 32).map(|_| input.u8()).collect(),
        _ => (0..usize::from(input.u16()) % (MAX_GARBAGE_LEN + 1)).map(|_| input.u8()).collect(),
    };
    let mut wire = garbage.clone();
    let mut term = *peer.send_garbage_terminator();
    let term_op = input.u8();
    if term_op.is_multiple_of(16) {
        term[usize::from(term_op >> 4)] ^= 1;
    }
    wire.extend_from_slice(&term);
    let mut packets: Vec<Packet> = Vec::new();
    let mut first_aad = Some(garbage.clone());
    let mut large_done = false;
    while !input.0.is_empty() && packets.len() < MAX_PACKETS {
        let op = input.u8();
        let header = match op % 4 {
            0 => IGNORE_BIT,
            1 => input.u8(),
            _ => 0,
        };
        let contents = contents(op, &mut input, &mut large_done);
        let mut aad = first_aad.take().unwrap_or_default();
        let mut fault = match input.u8() % 32 {
            0 => Fault::Tag,
            1 => Fault::Body,
            2 => Fault::Aad,
            3 => {
                let f = u32::from(input.u16());
                Fault::Length(match f % 4 {
                    2 => MAX_CONTENTS as u32 + 1 + (f >> 2),
                    3 => MAX_CONTENTS as u32 - (f >> 2),
                    _ => f >> 2,
                })
            }
            _ => Fault::None,
        };
        if fault == Fault::Length(contents.len() as u32) {
            fault = Fault::None;
        }
        let mut packet = vec![0; contents.len() + EXPANSION];
        packet[LENGTH_LEN + 1..LENGTH_LEN + 1 + contents.len()].copy_from_slice(&contents);
        let sent_aad = aad.clone();
        if fault == Fault::Aad {
            aad.push(0);
        }
        peer.encrypt_with_header(&mut packet, &aad, header);
        match fault {
            Fault::Tag => {
                if let Some(b) = packet.last_mut() {
                    *b ^= 1;
                }
            }
            Fault::Body => packet[LENGTH_LEN] ^= 1,
            Fault::Length(forged) => {
                let (real, forged) = ((contents.len() as u32).to_le_bytes(), forged.to_le_bytes());
                for i in 0..LENGTH_LEN {
                    packet[i] ^= real[i] ^ forged[i];
                }
            }
            Fault::None | Fault::Aad => {}
        }
        packets.push(Packet {
            start: wire.len(),
            contents,
            header,
            aad: if fault == Fault::Aad {
                aad
            } else {
                sent_aad
            },
            fault,
        });
        wire.extend_from_slice(&packet);
        if fault != Fault::None {
            break;
        }
    }
    // Stop the stream at a decision boundary of the last packet: inside or right after its
    // length descriptor, or one byte short of its end.
    if let Some(last) = packets.last() {
        let end = match cut {
            1 => last.start + 2,
            2 => last.start + LENGTH_LEN,
            3 => wire.len() - 1,
            _ => wire.len(),
        };
        wire.truncate(end);
    }

    let want = expected_scripted(&wire, &garbage, peer.send_garbage_terminator(), &packets);
    let (got, large, largest) = count_large(|| feed(&mut t, &wire, chunk));
    match want {
        Some(want) if got != want => return Err(diverged("scripted: outcome")),
        // The stream is not aligned to the script (garbage that holds the terminator): no
        // message can authenticate.
        None if !got.messages.is_empty()
            || matches!(got.error, Some(TransportError::MissingGarbageTerminator)) =>
        {
            return Err(diverged("scripted: misaligned stream"));
        }
        _ => {}
    }
    // Core's growth: a large buffer at most once per reserve-ahead of a packet, plus its first
    // allocation (a delivered payload keeps the buffer, so the next packet starts a new one);
    // and never more than the reserve-ahead beyond the bytes that arrived.
    let large_packets =
        packets.iter().filter(|p| p.contents.len() + EXPANSION >= LARGE_ALLOC).count();
    if large > wire.len() / RESERVE_AHEAD + 3 * large_packets + 2 {
        return Err(diverged("scripted: receive buffer growth"));
    }
    if largest > wire.len() + RESERVE_AHEAD {
        return Err(diverged("scripted: receive buffer reserve"));
    }
    Ok(())
}

/// What Core's `V2Transport` makes of the peer's bytes after its key, worked out from the
/// script: `None` when the receiver finds the terminator elsewhere than after the garbage (only
/// garbage that holds it), where the packets it then reads are not the script's.
fn expected_scripted(
    wire: &[u8],
    garbage: &[u8],
    term: &[u8; GARBAGE_TERMINATOR_LEN],
    packets: &[Packet],
) -> Option<Outcome> {
    let mut out = Outcome {
        messages: Vec::new(),
        error: None,
    };
    // The garbage ends at the first terminator within 4095 bytes; Core gives up at 4111 bytes.
    let found =
        wire.windows(GARBAGE_TERMINATOR_LEN).take(MAX_GARBAGE_LEN + 1).position(|w| w == term);
    match found {
        None => {
            if wire.len() >= MAX_GARBAGE_LEN + GARBAGE_TERMINATOR_LEN {
                out.error = Some(TransportError::MissingGarbageTerminator);
            }
            return Some(out);
        }
        Some(n) if n != garbage.len() => return None,
        Some(_) => {}
    }
    let mut version = true;
    let mut aad = Some(garbage.to_vec());
    for p in packets {
        let available = wire.len() - p.start;
        if available < LENGTH_LEN {
            break;
        }
        let len = match p.fault {
            Fault::Length(forged) => forged,
            _ => p.contents.len() as u32,
        };
        if len as usize > MAX_CONTENTS {
            out.error = Some(TransportError::PacketTooLarge(len));
            break;
        }
        if available < len as usize + EXPANSION {
            break;
        }
        let want_aad = aad.take().unwrap_or_default();
        if !matches!(p.fault, Fault::None | Fault::Aad) || p.aad != want_aad {
            out.error = Some(TransportError::DecryptionFailure(len));
            break;
        }
        if p.header & IGNORE_BIT != 0 {
            continue;
        }
        if version {
            version = false;
            continue;
        }
        out.messages.push(
            expected_type(&p.contents)
                .map(|(command, payload)| (command, payload.to_vec()))
                .ok_or(Reject::InvalidType),
        );
    }
    Some(out)
}
