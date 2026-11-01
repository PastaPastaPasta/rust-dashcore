//! The V2 transport's behaviour (P2a behaviour 9), ported from Dash Core v24.0.0-rc.3's
//! `src/test/net_tests.cpp` (`V2TransportTester`, `v2transport_test`,
//! `v2_short_id_version_negotiation`) with Core's random choices drawn from a seeded generator,
//! plus the cases Core covers elsewhere: the short-ID table, rekeying, `ShouldReconnectV1` and the
//! send-memory charge.

mod common;

#[path = "data/core_short_ids.rs"]
mod core_short_ids;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use common::{mainnet, regtest};
use dash_p2p_transport::cipher::{
    Bip324Cipher, ELLSWIFT_KEY_LEN, EXPANSION, GARBAGE_TERMINATOR_LEN, LENGTH_LEN, SecretKey,
};
use dash_p2p_transport::{
    CommandString, MAX_GARBAGE_LEN, Magic, NetMessage, OutMsg, ProtocolVersion,
    SERIALIZED_MSG_SIZE, TransportError, TransportKind, V1_PREFIX_LEN, V2Transport,
    is_valid_short_id, malloc_usage, message_type,
};

/// The system allocator, counting on the threads that ask (each test runs on its own thread)
/// the allocations and reallocations of at least [`LARGE`] bytes, and the largest size asked for.
struct CountingAlloc;

const LARGE: usize = 16 * 1024;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static GROWTHS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note_alloc(size: usize) {
    let _ = COUNTING.try_with(|on| {
        if on.get() {
            let _ = LARGEST.try_with(|l| l.set(l.get().max(size)));
            if size >= LARGE {
                let _ = GROWTHS.try_with(|g| g.set(g.get() + 1));
            }
        }
    });
}

// SAFETY: every call is forwarded to `System` unchanged.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note_alloc(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Runs `f`, returning the large allocations and reallocations it made on this thread and the
/// largest size it asked for.
fn counting(f: impl FnOnce()) -> (usize, usize) {
    GROWTHS.with(|g| g.set(0));
    LARGEST.with(|l| l.set(0));
    COUNTING.with(|on| on.set(true));
    f();
    COUNTING.with(|on| on.set(false));
    (GROWTHS.with(Cell::get), LARGEST.with(Cell::get))
}

/// Core v24's `PROTOCOL_VERSION`, which `V2TransportTester` sets as the peer's version.
const PROTOCOL_VERSION: ProtocolVersion = ProtocolVersion(70242);
const MAX_PROTOCOL_MESSAGE_LENGTH: usize = 3 * 1024 * 1024;

/// SplitMix64: Core's `InsecureRand*` calls, reproducibly.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn range(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
    fn bytes_below(&mut self, n: usize) -> Vec<u8> {
        let len = self.range(n);
        self.bytes(len)
    }
    fn key(&mut self) -> SecretKey {
        loop {
            let b: [u8; 32] = self.bytes(32).try_into().unwrap();
            if let Ok(k) = SecretKey::from_secret_bytes(b) {
                return k;
            }
        }
    }
    fn array32(&mut self) -> [u8; 32] {
        self.bytes(32).try_into().unwrap()
    }
}

fn cmd(s: &str) -> CommandString {
    let mut b = [0u8; 12];
    b[..s.len()].copy_from_slice(s.as_bytes());
    CommandString::from_bytes(b)
}

/// Core's `V2TransportTester`: the transport under test, and a bare cipher acting as the peer so
/// that the peer can send what a `V2Transport` never would (decoys, bad bytes).
struct Tester {
    transport: V2Transport,
    cipher: Bip324Cipher,
    test_initiator: bool,
    sent_garbage: Vec<u8>,
    recv_garbage: Vec<u8>,
    to_send: Vec<u8>,
    received: Vec<u8>,
    msg_to_send: std::collections::VecDeque<OutMsg>,
    sent_aad: bool,
    rng: Rng,
}

type Interact = Result<Vec<Option<NetMessage>>, TransportError>;

impl Tester {
    fn new(test_initiator: bool, seed: u64) -> Self {
        let mut rng = Rng(seed);
        let garbage = rng.bytes_below(MAX_GARBAGE_LEN + 1);
        let mut transport =
            V2Transport::with_keys(regtest(), test_initiator, rng.key(), rng.array32(), garbage);
        transport.set_peer_version(PROTOCOL_VERSION);
        let cipher = Bip324Cipher::new(regtest(), rng.key(), rng.array32());
        Self {
            transport,
            cipher,
            test_initiator,
            sent_garbage: Vec::new(),
            recv_garbage: Vec::new(),
            to_send: Vec::new(),
            received: Vec::new(),
            msg_to_send: Default::default(),
            sent_aad: false,
            rng,
        }
    }

    /// Moves bytes and messages in random chunks and order until nothing more can move.
    fn interact(&mut self) -> Interact {
        let mut ret = Vec::new();
        loop {
            let mut progress = false;
            if !self.to_send.is_empty() {
                let n = 1 + self.rng.range(self.to_send.len());
                let mut chunk = &self.to_send[..n];
                self.transport.received_bytes(&mut chunk)?;
                let used = n - chunk.len();
                if used != 0 {
                    progress = true;
                    self.to_send.drain(..used);
                }
            }
            if self.transport.received_message_complete() && (!progress || self.rng.bool()) {
                ret.push(self.transport.received_message(0).ok());
                progress = true;
            }
            if !self.msg_to_send.is_empty() && (!progress || self.rng.bool()) {
                let msg = self.msg_to_send.pop_front().unwrap();
                match self.transport.set_message_to_send(msg) {
                    Ok(()) => progress = true,
                    Err(msg) => self.msg_to_send.push_front(msg),
                }
            }
            let bytes = self.transport.bytes_to_send(!self.msg_to_send.is_empty()).data;
            if !bytes.is_empty() && (!progress || self.rng.bool()) {
                let n = 1 + self.rng.range(bytes.len());
                self.received.extend_from_slice(&bytes[..n]);
                self.transport.mark_bytes_sent(n);
                progress = true;
            }
            if !progress {
                return Ok(ret);
            }
        }
    }

    fn send(&mut self, data: &[u8]) {
        self.to_send.extend_from_slice(data);
    }

    fn send_v1_version(&mut self, magic: Magic) {
        let len = 126 + self.rng.range(11) as u32;
        let hdr = v1_version_header(magic, len);
        self.send(&hdr);
    }

    fn send_key(&mut self) {
        let key = *self.cipher.our_pubkey();
        self.send(&key);
    }

    fn send_garbage(&mut self, garbage: Vec<u8>) {
        self.send(&garbage);
        self.sent_garbage = garbage;
    }

    fn send_garbage_len(&mut self, len: usize) {
        let g = self.rng.bytes(len);
        self.send_garbage(g);
    }

    fn send_random_garbage(&mut self) {
        let len = self.rng.range(MAX_GARBAGE_LEN + 1);
        self.send_garbage_len(len);
    }

    fn add_message(&mut self, command: &str, payload: Vec<u8>) {
        self.msg_to_send.push_back(OutMsg::new(cmd(command), payload));
    }

    fn receive_key(&mut self) {
        assert!(self.received.len() >= ELLSWIFT_KEY_LEN);
        let theirs: [u8; ELLSWIFT_KEY_LEN] = self.received[..ELLSWIFT_KEY_LEN].try_into().unwrap();
        self.cipher.initialize(&theirs, !self.test_initiator, false);
        self.received.drain(..ELLSWIFT_KEY_LEN);
    }

    fn send_packet(&mut self, contents: &[u8], aad: &[u8], ignore: bool) {
        let mut out = vec![0; contents.len() + EXPANSION];
        self.cipher.encrypt(contents, aad, ignore, &mut out);
        self.send(&out);
    }

    fn send_garbage_term(&mut self) {
        let t = *self.cipher.send_garbage_terminator();
        self.send(&t);
    }

    fn send_version(&mut self, data: &[u8], ignore: bool) {
        let aad = if self.sent_aad {
            Vec::new()
        } else {
            self.sent_garbage.clone()
        };
        self.send_packet(data, &aad, ignore);
        self.sent_aad = true;
    }

    /// The next non-decoy packet's contents; `aad` is expected on the first packet.
    fn receive_packet(&mut self, mut aad: Vec<u8>) -> Vec<u8> {
        loop {
            assert!(self.received.len() >= LENGTH_LEN);
            let len = self.cipher.decrypt_length(self.received[..LENGTH_LEN].try_into().unwrap())
                as usize;
            assert!(self.received.len() >= len + EXPANSION);
            let mut body = self.received[LENGTH_LEN..len + EXPANSION].to_vec();
            let ignore = self.cipher.decrypt(&mut body, &aad).expect("packet authenticates");
            aad.clear();
            self.received.drain(..len + EXPANSION);
            if !ignore {
                return body[1..1 + len].to_vec();
            }
        }
    }

    fn receive_garbage(&mut self) {
        let term = *self.cipher.recv_garbage_terminator();
        let len = (0..=MAX_GARBAGE_LEN)
            .find(|&n| {
                assert!(self.received.len() >= n + GARBAGE_TERMINATOR_LEN);
                self.received[n..n + GARBAGE_TERMINATOR_LEN] == term
            })
            .expect("garbage terminator");
        self.recv_garbage = self.received[..len].to_vec();
        self.received.drain(..len + GARBAGE_TERMINATOR_LEN);
    }

    fn receive_version(&mut self) {
        let aad = self.recv_garbage.clone();
        assert!(self.receive_packet(aad).is_empty(), "version packets are empty");
    }

    fn receive_short(&mut self, id: u8, payload: &[u8]) {
        let p = self.receive_packet(Vec::new());
        assert_eq!(p[0], id);
        assert_eq!(&p[1..], payload);
    }

    fn receive_long(&mut self, command: &str, payload: &[u8]) {
        let p = self.receive_packet(Vec::new());
        assert_eq!(p[0], 0);
        assert_eq!(&p[1..13], cmd(command).as_bytes());
        assert_eq!(&p[13..], payload);
    }

    fn send_long(&mut self, command: &[u8; 12], payload: &[u8]) {
        let mut c = vec![0u8];
        c.extend_from_slice(command);
        c.extend_from_slice(payload);
        self.send_packet(&c, &[], false);
    }

    fn send_short(&mut self, id: u8, payload: &[u8]) {
        let mut c = vec![id];
        c.extend_from_slice(payload);
        self.send_packet(&c, &[], false);
    }

    fn compare_session_ids(&self) {
        let info = self.transport.info();
        assert_eq!(info.kind, TransportKind::V2);
        assert_eq!(info.session_id.as_ref(), Some(self.cipher.session_id()));
    }

    fn damage(&mut self) {
        let pos = self.rng.range(self.to_send.len());
        self.to_send[pos] ^= 1 << self.rng.range(8);
    }

    /// The handshake up to and including the version packets, with `garbage` from the peer.
    fn handshake(&mut self) {
        if self.test_initiator {
            assert!(self.interact().unwrap().is_empty());
            self.send_key();
            self.send_random_garbage();
        } else {
            self.send_key();
            self.send_random_garbage();
            assert!(self.interact().unwrap().is_empty());
        }
        self.receive_key();
        self.send_garbage_term();
        self.send_version(&[], false);
        assert!(self.interact().unwrap().is_empty());
        self.receive_garbage();
        self.receive_version();
        self.compare_session_ids();
    }
}

fn long(command: &str) -> [u8; 12] {
    *cmd(command).as_bytes()
}

/// A V1 `version` header announcing `len` payload bytes (Core's `SendV1Version`).
fn v1_version_header(magic: Magic, len: u32) -> [u8; 24] {
    let mut h = [0u8; 24];
    h[..4].copy_from_slice(magic.as_bytes());
    h[4..16].copy_from_slice(&long("version"));
    h[16..20].copy_from_slice(&len.to_le_bytes());
    h
}

fn is(msg: &Option<NetMessage>, command: &str, payload: &[u8]) -> bool {
    msg.as_ref().is_some_and(|m| m.command == cmd(command) && m.payload == payload)
}

#[test]
fn initiator_normal_scenario() {
    for seed in 0..10 {
        let mut t = Tester::new(true, seed);
        t.handshake();
        let d1 = t.rng.bytes_below(100_000);
        let d2 = t.rng.bytes_below(1000);
        t.send_short(4, &d1); // cmpctblock
        t.send_short(0, &[]); // invalidly encoded
        t.send_long(&long("tx"), &d2);
        let ret = t.interact().unwrap();
        assert_eq!(ret.len(), 3);
        assert!(is(&ret[0], "cmpctblock", &d1));
        assert!(ret[1].is_none());
        assert!(is(&ret[2], "tx", &d2));

        // A bit error: the failure may come late (a damaged length), but no message gets through.
        t.send_long(&long("bad"), &d1);
        t.damage();
        loop {
            match t.interact() {
                Err(_) => break,
                Ok(ret) => assert!(ret.is_empty()),
            }
            let d3 = t.rng.bytes_below(10_000);
            t.send_short(12, &d3); // getheaders
        }
    }
}

#[test]
fn responder_normal_scenario() {
    for seed in 100..110 {
        let mut t = Tester::new(false, seed);
        t.handshake();
        let d1 = t.rng.bytes_below(100_000);
        let d2 = t.rng.bytes_below(1000);
        t.send_short(14, &d1); // inv
        t.send_short(19, &d2); // pong
        let ret = t.interact().unwrap();
        assert_eq!(ret.len(), 2);
        assert!(is(&ret[0], "inv", &d1));
        assert!(is(&ret[1], "pong", &d2));

        // A packet above the 3 MiB contents limit.
        let d3 = t.rng.bytes(4_005_000);
        t.send_short(11, &d3);
        assert!(matches!(t.interact(), Err(TransportError::PacketTooLarge(_))));
    }
}

/// Core's "various valid but unusual scenarios": empty or maximal garbage, decoys before the
/// version packet and between messages, version packets with contents, unknown types.
#[test]
fn unusual_scenarios() {
    for seed in 200..250 {
        let mut rng = Rng(seed ^ 0x5555);
        let initiator = rng.bool();
        let garb_len = if rng.bool() {
            0
        } else {
            MAX_GARBAGE_LEN
        };
        let num_ignore_version = rng.range(10);
        let ver_len = if rng.bool() {
            0
        } else {
            rng.range(1000)
        };
        let ver_data = rng.bytes(ver_len);
        let send_immediately = !initiator || rng.bool();
        let (num_decoys_1, num_decoys_2) = (rng.range(1000), rng.range(1000));
        let mut t = Tester::new(initiator, seed);
        if send_immediately {
            t.send_key();
            t.send_garbage_len(garb_len);
        }
        assert!(t.interact().unwrap().is_empty());
        if !send_immediately {
            t.send_key();
            t.send_garbage_len(garb_len);
        }
        t.receive_key();
        t.send_garbage_term();
        for _ in 0..num_ignore_version {
            let len = if rng.bool() {
                0
            } else {
                rng.range(1000)
            };
            let data = rng.bytes(len);
            t.send_version(&data, true);
        }
        t.send_version(&ver_data, false);
        assert!(t.interact().unwrap().is_empty());
        t.receive_garbage();
        t.receive_version();
        t.compare_session_ids();
        for _ in 0..num_decoys_1 {
            let d = rng.bytes_below(1000);
            t.send_packet(&d, &[], true);
        }
        let d1 = rng.bytes_below(MAX_PROTOCOL_MESSAGE_LENGTH);
        t.send_short(28, &d1); // addrv2
        for _ in 0..num_decoys_2 {
            let d = rng.bytes_below(1000);
            t.send_packet(&d, &[], true);
        }
        let d2 = rng.bytes_below(1000);
        t.send_short(13, &d2); // headers
        t.send_long(b"blocktxn\0\0\0a", &[]); // invalidly encoded
        t.send_long(&long("foobar"), &[]); // unknown type: delivered
        t.add_message("barfoo", Vec::new()); // unknown type: sent long
        let ret = t.interact().unwrap();
        assert_eq!(ret.len(), 4);
        assert!(is(&ret[0], "addrv2", &d1));
        assert!(is(&ret[1], "headers", &d2));
        assert!(ret[2].is_none());
        assert!(is(&ret[3], "foobar", &[]));
        t.receive_long("barfoo", &[]);
    }
}

#[test]
fn too_long_garbage() {
    for initiator in [true, false] {
        let mut t = Tester::new(initiator, 300 + u64::from(initiator));
        if initiator {
            assert!(t.interact().unwrap().is_empty());
        }
        t.send_key();
        t.send_garbage_len(MAX_GARBAGE_LEN + 1);
        if !initiator {
            assert!(t.interact().unwrap().is_empty());
        }
        t.receive_key();
        t.send_garbage_term();
        assert_eq!(t.interact().unwrap_err(), TransportError::MissingGarbageTerminator);
    }
}

/// Garbage containing the terminator's first 15 bytes, maximal messages both ways, and short IDs
/// outside Core's two ranges (dropped, connection kept).
#[test]
fn near_terminator_garbage_and_max_messages() {
    for seed in 400..404 {
        let mut t = Tester::new(true, seed);
        assert!(t.interact().unwrap().is_empty());
        t.send_key();
        t.receive_key();
        let before = t.rng.range(MAX_GARBAGE_LEN - 16 + 1);
        let after = t.rng.range(MAX_GARBAGE_LEN - 16 - before + 1);
        let mut garbage = t.rng.bytes(before + 16 + after);
        garbage[before..before + 16].copy_from_slice(t.cipher.send_garbage_terminator());
        garbage[before + 15] ^= 1 << t.rng.range(8);
        t.send_garbage(garbage);
        t.send_garbage_term();
        t.send_version(&[], false);
        assert!(t.interact().unwrap().is_empty());
        t.receive_garbage();
        t.receive_version();
        t.compare_session_ids();
        let d1 = t.rng.bytes(MAX_PROTOCOL_MESSAGE_LENGTH);
        let d2 = t.rng.bytes(MAX_PROTOCOL_MESSAGE_LENGTH);
        let bad = if t.rng.bool() {
            33 + t.rng.range(95) as u8
        } else {
            169 + t.rng.range(87) as u8
        };
        t.send_short(bad, &[]);
        t.send_short(2, &d1); // block
        t.add_message("blocktxn", d2.clone());
        let ret = t.interact().unwrap();
        assert_eq!(ret.len(), 2);
        assert!(ret[0].is_none());
        assert!(is(&ret[1], "block", &d1));
        t.receive_short(3, &d2);
    }
}

/// Inbound V1 detection: this network's `version` header falls back to V1; another network's is
/// refused.
#[test]
fn responder_v1_detection() {
    let mut t = Tester::new(false, 500);
    t.send_v1_version(regtest());
    assert!(t.interact().is_ok());
    assert_eq!(t.transport.info().kind, TransportKind::V1);
    assert!(t.received.is_empty(), "a V1 peer gets nothing before its version");

    let mut t = Tester::new(false, 501);
    t.send_v1_version(mainnet());
    assert_eq!(t.interact().unwrap_err(), TransportError::V1PeerWrongMessageStart(mainnet()));
}

/// After falling back the transport is a plain V1 transport in both directions.
#[test]
fn responder_v1_fallback_round_trip() {
    let mut rng = Rng(510);
    let mut t = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), rng.bytes(100));
    let mut v1 = dash_p2p_transport::V1Transport::new(regtest());
    v1.set_message_to_send(OutMsg::new(cmd("version"), vec![7; 130])).unwrap();
    let mut wire = Vec::new();
    loop {
        let data = v1.bytes_to_send(false).data;
        if data.is_empty() {
            break;
        }
        let n = data.len();
        wire.extend_from_slice(data);
        v1.mark_bytes_sent(n);
    }
    // Feed one byte at a time across the 16-byte detection window.
    for b in &wire {
        let mut s = core::slice::from_ref(b);
        while !s.is_empty() {
            t.received_bytes(&mut s).unwrap();
        }
    }
    assert!(t.received_message_complete());
    let msg = t.received_message(0).unwrap();
    assert_eq!((msg.command, msg.payload.len()), (cmd("version"), 130));
    assert!(t.bytes_to_send(false).data.is_empty(), "no V2 key was sent");
    t.set_message_to_send(OutMsg::new(cmd("verack"), Vec::new())).unwrap();
    let out = t.bytes_to_send(false).data.to_vec();
    assert_eq!(out.len(), 24);
    assert_eq!(&out[..4], regtest().as_bytes());
    assert!(!t.should_reconnect_v1());
}

/// The responder decides on at most 16 bytes: a mismatch in any of them means V2, and it starts
/// sending its key.
#[test]
fn responder_detection_window() {
    let prefix = v1_version_header(regtest(), 0);
    for pos in 0..V1_PREFIX_LEN {
        let mut rng = Rng(600 + pos as u64);
        let mut t = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), Vec::new());
        let mut bytes = prefix[..=pos].to_vec();
        bytes[pos] ^= 1;
        let mut s = &bytes[..];
        t.received_bytes(&mut s).unwrap();
        assert!(s.is_empty());
        assert_eq!(t.bytes_to_send(false).data.len(), ELLSWIFT_KEY_LEN, "mismatch at byte {pos}");
        assert_eq!(t.info().kind, TransportKind::Detecting);
    }
    // A full prefix match with 15 bytes is still undecided.
    let mut rng = Rng(650);
    let mut t = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), Vec::new());
    let mut s = &prefix[..V1_PREFIX_LEN - 1];
    t.received_bytes(&mut s).unwrap();
    assert!(t.bytes_to_send(false).data.is_empty());
    assert_eq!(t.info().kind, TransportKind::Detecting);
}

/// Core's `v2_short_id_version_negotiation`, extended to the gate below the baseline: before the
/// peer's version every message is long; from 70235 baseline IDs are short; `platformban` only
/// from 70240.
#[test]
fn short_id_version_negotiation() {
    let cases: &[(i32, &str, Option<u8>)] = &[
        (209, "spork", None),
        (209, "ping", None),
        (70234, "ping", None),
        (70235, "spork", Some(128)),
        (70235, "ping", Some(18)),
        (70238, "platformban", None),
        (70239, "platformban", None),
        (70240, "platformban", Some(168)),
        (70242, "platformban", Some(168)),
        (70242, "barfoo", None),
        (70242, "", Some(5)),
    ];
    for (n, &(version, command, short)) in cases.iter().enumerate() {
        let mut t = Tester::new(true, 700 + n as u64);
        t.transport.set_peer_version(ProtocolVersion(version));
        t.handshake();
        let data = t.rng.bytes(100);
        t.add_message(command, data.clone());
        assert!(t.interact().unwrap().is_empty());
        match short {
            Some(id) => t.receive_short(id, &data),
            None => t.receive_long(command, &data),
        }
    }
}

/// The receive table is Core's `V2_BITCOIN_IDS` and `V2_DASH_IDS`: reserved slots (5, 29 to 32)
/// deliver the empty type, IDs outside both ranges are dropped, and long-form types follow
/// `GetMessageType`.
#[test]
fn short_id_table_matches_core() {
    use core_short_ids::{V2_BITCOIN_IDS, V2_DASH_IDS};
    for id in 1..=255u8 {
        let expected = match id {
            0..=32 => Some(V2_BITCOIN_IDS[usize::from(id)]),
            128..=168 => Some(V2_DASH_IDS[usize::from(id - 128)]),
            _ => None,
        };
        assert_eq!(is_valid_short_id(id), expected.is_some(), "id {id}");
        assert_eq!(message_type(&[id, 9, 9]), expected.map(|c| (cmd(c), 1)), "id {id}");
    }
    assert_eq!(message_type(&[]), None);
    assert_eq!(message_type(&[0; 12]), None, "long form needs 12 type bytes");
    assert_eq!(message_type(&[0; 13]), Some((CommandString::EMPTY, 13)));
    assert_eq!(message_type(b"\0tx\0\0\0\0\0\0\0\0\0\0xyz"), Some((cmd("tx"), 13)));
    assert_eq!(message_type(b"\0t\x7f\0\0\0\0\0\0\0\0\0\0"), Some((cmd("t\x7f"), 13)));
    assert_eq!(message_type(b"\0t\x1f\0\0\0\0\0\0\0\0\0\0"), None);
    assert_eq!(message_type(b"\0t\x80\0\0\0\0\0\0\0\0\0\0"), None);
    assert_eq!(message_type(b"\0t\0x\0\0\0\0\0\0\0\0\0"), None);
}

/// Two transports talk across several rekeys (224 packets each) in both directions.
#[test]
fn rekey_across_many_packets() {
    let mut rng = Rng(800);
    let mut a = V2Transport::with_keys(regtest(), true, rng.key(), rng.array32(), rng.bytes(17));
    let mut b = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), rng.bytes(4095));
    a.set_peer_version(PROTOCOL_VERSION);
    b.set_peer_version(PROTOCOL_VERSION);
    let pump = |from: &mut V2Transport, to: &mut V2Transport| -> Vec<NetMessage> {
        let mut got = Vec::new();
        loop {
            let data = from.bytes_to_send(false).data.to_vec();
            if data.is_empty() {
                return got;
            }
            from.mark_bytes_sent(data.len());
            let mut s = &data[..];
            while !s.is_empty() {
                to.received_bytes(&mut s).unwrap();
                if to.received_message_complete() {
                    got.push(to.received_message(1).unwrap());
                }
            }
        }
    };
    for _ in 0..3 {
        pump(&mut a, &mut b);
        pump(&mut b, &mut a);
    }
    assert_eq!(a.info().kind, TransportKind::V2);
    assert_eq!(a.info().session_id, b.info().session_id);
    for i in 0..1000u32 {
        let payload = i.to_le_bytes().to_vec();
        a.set_message_to_send(OutMsg::new(cmd("ping"), payload.clone())).unwrap();
        let got = pump(&mut a, &mut b);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].command, &got[0].payload), (cmd("ping"), &payload));
        assert_eq!(got[0].raw_size, 1 + 4 + EXPANSION, "short ID, 4-byte payload");
        b.set_message_to_send(OutMsg::new(cmd("pong"), payload.clone())).unwrap();
        assert_eq!(pump(&mut b, &mut a)[0].payload, payload);
    }
}

/// `ShouldReconnectV1`: an outbound connection that got nothing back after sending at least 24
/// bytes.
#[test]
fn should_reconnect_v1() {
    let mut rng = Rng(900);
    let mut t = V2Transport::with_keys(regtest(), true, rng.key(), rng.array32(), Vec::new());
    assert_eq!(t.bytes_to_send(false).data.len(), ELLSWIFT_KEY_LEN);
    assert!(!t.should_reconnect_v1());
    t.mark_bytes_sent(23);
    assert!(!t.should_reconnect_v1(), "less than a V1 header");
    t.mark_bytes_sent(1);
    assert!(t.should_reconnect_v1());
    let mut s: &[u8] = &[0x12];
    t.received_bytes(&mut s).unwrap();
    assert!(!t.should_reconnect_v1(), "the peer answered");

    let mut t = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), Vec::new());
    let mut s: &[u8] = &[0x12; 40];
    t.received_bytes(&mut s).unwrap();
    t.mark_bytes_sent(ELLSWIFT_KEY_LEN);
    assert!(!t.should_reconnect_v1(), "inbound connections are never retried");
}

/// Messages can't be queued before the key exchange, nor while bytes are pending; `more` only
/// once the handshake is done.
#[test]
fn send_gating() {
    let mut t = Tester::new(true, 950);
    assert!(t.transport.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).is_err());
    assert!(!t.transport.bytes_to_send(true).more, "nothing but the handshake can go yet");
    t.handshake();
    assert!(t.transport.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).is_ok());
    assert!(t.transport.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).is_err());
    let b = t.transport.bytes_to_send(true);
    assert!(b.more);
    assert_eq!(b.command, cmd("ping"));
    let n = b.data.len();
    t.transport.mark_bytes_sent(n - 1);
    assert!(t.transport.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).is_err());
    t.transport.mark_bytes_sent(1);
    assert!(t.transport.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).is_ok());
}

/// The send-memory charge (SB0 §3.1): 24 bytes for the buffer object plus glibc's cost of the
/// capacity libstdc++ would hold, released once everything is sent.
#[test]
fn send_memory_usage() {
    let mut rng = Rng(990);
    let mut t = V2Transport::with_keys(regtest(), true, rng.key(), rng.array32(), vec![1; 100]);
    assert_eq!(t.send_memory_usage(), 24 + malloc_usage(ELLSWIFT_KEY_LEN + 100));
    // The peer's key arrives before ours has gone: terminator and version packet are appended
    // (two `resize` calls, so libstdc++ doubles: 164 -> 328).
    let peer = Bip324Cipher::new(regtest(), rng.key(), rng.array32());
    let theirs = *peer.our_pubkey();
    let mut s = &theirs[..];
    t.received_bytes(&mut s).unwrap();
    assert_eq!(t.bytes_to_send(false).data.len(), 164 + GARBAGE_TERMINATOR_LEN + EXPANSION);
    assert_eq!(t.send_memory_usage(), 24 + malloc_usage(328));
    let n = t.bytes_to_send(false).data.len();
    t.mark_bytes_sent(n);
    assert_eq!(t.send_memory_usage(), 24);
    // The key arrived, so the send side is ready: one exact allocation per message.
    t.set_peer_version(PROTOCOL_VERSION);
    t.set_message_to_send(OutMsg::new(cmd("ping"), vec![0; 8])).unwrap();
    assert_eq!(t.send_memory_usage(), 24 + malloc_usage(1 + 8 + EXPANSION));
}

/// Core checks for a wrong-network V1 peer in `ProcessReceivedKeyBytes`, which runs on bytes
/// that arrive after the V1-detection state is left: 16 such bytes in one step are not yet an
/// error, a 17th byte (or the same 16 in two steps) is.
#[test]
fn responder_wrong_network_needs_a_key_step() {
    let header = v1_version_header(mainnet(), 0);
    let fresh = |seed| {
        let mut rng = Rng(seed);
        V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), Vec::new())
    };
    let wrong = Err(TransportError::V1PeerWrongMessageStart(mainnet()));

    let mut t = fresh(1000);
    let mut s = &header[..V1_PREFIX_LEN];
    assert_eq!(t.received_bytes(&mut s), Ok(()));
    assert!(s.is_empty());
    let mut s = &header[V1_PREFIX_LEN..=V1_PREFIX_LEN];
    assert_eq!(t.received_bytes(&mut s), wrong);

    let mut t = fresh(1001);
    let mut s = &header[..8];
    while !s.is_empty() {
        t.received_bytes(&mut s).unwrap();
    }
    let mut s = &header[8..V1_PREFIX_LEN];
    assert_eq!(t.received_bytes(&mut s), wrong);
}

/// Until the peer's version packet is verified the transport reports `Detecting` with no session
/// ID, through the key exchange and the garbage.
#[test]
fn detecting_until_version_packet() {
    let mut t = Tester::new(true, 1100);
    assert!(t.interact().unwrap().is_empty());
    assert_eq!(t.transport.info().kind, TransportKind::Detecting);
    t.send_key();
    t.send_random_garbage();
    assert!(t.interact().unwrap().is_empty());
    t.receive_key();
    assert_eq!(t.transport.info().kind, TransportKind::Detecting);
    assert_eq!(t.transport.info().session_id, None);
    t.send_garbage_term();
    assert!(t.interact().unwrap().is_empty());
    assert_eq!(t.transport.info().kind, TransportKind::Detecting);
    assert_eq!(t.transport.info().session_id, None);
    t.send_version(&[], false);
    assert!(t.interact().unwrap().is_empty());
    assert_eq!(t.transport.info().kind, TransportKind::V2);
    assert!(t.transport.info().session_id.is_some());
}

/// A responder charges an empty send buffer while it detects, and the V1 charge (Core's
/// `CSerializedNetMsg::GetMemoryUsage`) once it has fallen back.
#[test]
fn responder_send_memory_usage() {
    let mut rng = Rng(1200);
    let mut t = V2Transport::with_keys(regtest(), false, rng.key(), rng.array32(), vec![1; 100]);
    assert_eq!(t.send_memory_usage(), 24);
    let header = v1_version_header(regtest(), 0);
    let mut s = &header[..];
    while !s.is_empty() {
        t.received_bytes(&mut s).unwrap();
    }
    assert_eq!(t.info().kind, TransportKind::V1);
    assert_eq!(t.send_memory_usage(), SERIALIZED_MSG_SIZE);
    let msg = OutMsg::new(cmd("verack"), Vec::with_capacity(40));
    let charge = msg.memory_usage();
    t.set_message_to_send(msg).unwrap();
    assert_eq!(t.send_memory_usage(), charge);
    assert_eq!(charge, SERIALIZED_MSG_SIZE + malloc_usage(40));
}

/// Feeds `wire` to `t` in `frag`-byte pieces, as a peer trickling a packet would.
fn feed_fragments(t: &mut V2Transport, wire: &[u8], frag: usize) {
    for piece in wire.chunks(frag) {
        let mut s = piece;
        while !s.is_empty() {
            t.received_bytes(&mut s).unwrap();
        }
    }
}

/// Legal packets of the largest size (a version packet with the most contents, then a 3 MiB
/// message) trickled in 1- and 64-byte pieces grow the receive buffer as Core's
/// `ReceivedBytes` does: only when the bytes don't fit, by up to 256 KiB at a time. That is
/// about 13 growths per 3 MiB packet; reserving on every piece made millions.
///
/// `large_packet_reserves_ahead_of_what_arrived` checks the other half of Core's rule: what is
/// reserved never runs more than 256 KiB ahead of the bytes that arrived.
#[test]
fn large_packets_grow_the_buffer_in_steps() {
    const RESERVE_AHEAD: usize = 256 * 1024;
    for frag in [1, 64] {
        let mut t = Tester::new(false, 1300 + frag as u64);
        t.send_key();
        t.send_random_garbage();
        assert!(t.interact().unwrap().is_empty());
        t.receive_key();
        t.send_garbage_term();
        assert!(t.interact().unwrap().is_empty());

        let version = t.rng.bytes(MAX_PROTOCOL_MESSAGE_LENGTH + 13);
        t.send_version(&version, false);
        let wire = std::mem::take(&mut t.to_send);
        let (growths, largest) = counting(|| feed_fragments(&mut t.transport, &wire, frag));
        let steps = wire.len().div_ceil(RESERVE_AHEAD);
        assert!(growths <= steps + 1, "version packet, {frag}-byte pieces: {growths} growths");
        assert!(largest <= wire.len(), "version packet: a {largest}-byte allocation");
        assert_eq!(t.transport.info().kind, TransportKind::V2);
        assert!(!t.transport.received_message_complete());

        let payload = t.rng.bytes(MAX_PROTOCOL_MESSAGE_LENGTH);
        t.send_short(18, &payload);
        let wire = std::mem::take(&mut t.to_send);
        let (growths, largest) = counting(|| feed_fragments(&mut t.transport, &wire, frag));
        let steps = wire.len().div_ceil(RESERVE_AHEAD);
        assert!(growths <= steps + 1, "app packet, {frag}-byte pieces: {growths} growths");
        assert!(largest <= wire.len(), "app packet: a {largest}-byte allocation");
        assert!(t.transport.received_message_complete());
        let msg = t.transport.received_message(0).unwrap();
        assert_eq!(msg.command, cmd("ping"));
        assert!(msg.payload == payload, "payload changed");
    }
}

/// A peer that announces the largest packet and sends 1 KiB of it makes the receiver reserve
/// 256 KiB beyond what arrived (Core's `MAX_RESERVE_AHEAD`), not the announced size.
#[test]
fn large_packet_reserves_ahead_of_what_arrived() {
    const RESERVE_AHEAD: usize = 256 * 1024;
    let mut t = Tester::new(true, 1500);
    t.handshake();
    t.send_short(18, &vec![7; MAX_PROTOCOL_MESSAGE_LENGTH]);
    let wire = std::mem::take(&mut t.to_send);
    let sent = &wire[..1024];
    let (_, largest) = counting(|| feed_fragments(&mut t.transport, sent, 1024));
    assert!(largest <= sent.len() + RESERVE_AHEAD, "a {largest}-byte allocation for 1 KiB");
    assert!(!t.transport.received_message_complete());
}
