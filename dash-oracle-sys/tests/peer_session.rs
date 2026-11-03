//! Smoke tests of the oracle's peer sessions (oracle.h "peer sessions", ABI 10): one test per
//! entry point against what Core's net_processing does, and the seeded draws replaying across
//! processes. Sessions are process-wide, so the tests take turns (each holds the pin's
//! network lock while its session lives).

use std::process::Command;

use dash_oracle_sys::{
    Call, Chain, ConnType, Draw, ErrorKind, NetAddr, Oracle, PeerOutcome, PeerSession, SendMode,
    Service, SessionConfig, Substitution, TICK_INACTIVITY, TICK_MAINTENANCE, Transport,
};
use dashcore::consensus::deserialize;
use dashcore::hashes::{Hash, sha256, sha256d};

#[path = "support/alloc_fault.rs"]
mod alloc_fault;

/// Regtest genesis time (chainparams.cpp `CreateGenesisBlock(1417713337, ...)`).
const GENESIS_TIME: i64 = 1_417_713_337;
/// The session clock: a day after genesis (the regtest chain stays in IBD, as a fresh node).
const T: i64 = GENESIS_TIME + 86_400;
/// version.h `PROTOCOL_VERSION`.
const PROTOCOL_VERSION: i32 = 70242;
/// NODE_NETWORK | NODE_BLOOM: what an outbound peer must offer.
const PEER_SERVICES: u64 = 1 | 4;

fn service(octets: [u8; 4], port: u16) -> Service {
    Service::new(NetAddr::ipv4(octets), port)
}

fn config(seed: u64, blocks: Vec<Vec<u8>>) -> SessionConfig {
    SessionConfig {
        chain: Chain::regtest(),
        our_services: PEER_SERVICES,
        mocktime: T,
        rng_seed: seed,
        blocks,
        min_chain_work: None,
        max_tip_age: None,
        check_block_index: true,
    }
}

fn session(oracle: &Oracle, seed: u64) -> PeerSession {
    oracle.peer_session_new(&config(seed, Vec::new())).unwrap().expect("an empty fixture")
}

/// A `version` payload (net_processing's VERSION reader, up to fRelay).
fn version_payload(version: i32, nonce: u64, starting_height: i32) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend(version.to_le_bytes());
    p.extend(PEER_SERVICES.to_le_bytes());
    p.extend(T.to_le_bytes());
    for _ in 0..2 {
        // addrMe, then addrFrom: services, IPv6 bytes, port.
        p.extend([0u8; 8]);
        p.extend([0u8; 10]);
        p.extend([0xff, 0xff, 127, 0, 0, 1]);
        p.extend(19899u16.to_be_bytes());
    }
    p.extend(nonce.to_le_bytes());
    p.push(5);
    p.extend(b"/o1c/");
    p.extend(starting_height.to_le_bytes());
    p.push(1);
    p
}

fn commands(outcome: &PeerOutcome) -> Vec<&str> {
    outcome.sent.iter().map(|(c, _)| c.as_str()).collect()
}

/// Run passes until nothing is queued (at most `n`).
fn passes(s: &mut PeerSession, n: usize, mocktime: i64) {
    for _ in 0..n {
        s.send_pass(mocktime).unwrap();
    }
}

/// An outbound full-relay peer at `addr` taken through version/verack; returns its id and
/// everything the node sent it.
fn handshake(s: &mut PeerSession, addr: Service, nonce: u64) -> (i64, PeerOutcome) {
    let peer = s.add_peer(ConnType::OutboundFullRelay, &addr).unwrap();
    let ours = s.peer_outcome(peer).unwrap();
    assert_eq!(
        commands(&ours),
        ["version"],
        "InitializeNode pushes our version to an outbound peer"
    );
    let version = i32::from_le_bytes(ours.sent[0].1[..4].try_into().unwrap());
    s.deliver(peer, "version", &version_payload(version, nonce, 0), &[]).unwrap();
    s.deliver(peer, "verack", &[], &[]).unwrap();
    passes(s, 4, T);
    let outcome = s.peer_outcome(peer).unwrap();
    assert!(outcome.successfully_connected, "{outcome:?}");
    (peer, outcome)
}

#[test]
fn handshake_with_an_outbound_peer() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 1);
        let addr = service([1, 2, 3, 4], 19899);
        let (peer, outcome) = handshake(&mut s, addr.clone(), 7);
        let sent = commands(&outcome);
        assert_eq!(sent[..2], ["sendaddrv2", "verack"], "{sent:?}");
        assert!(sent.contains(&"ping"), "{sent:?}");
        assert_eq!((outcome.version, outcome.common_version), (PROTOCOL_VERSION, PROTOCOL_VERSION));
        assert_eq!(outcome.services, PEER_SERVICES);
        assert!(!outcome.removed && !outcome.disconnect);
        let data = outcome.peer.as_ref().expect("a Peer");
        assert_eq!(data.their_services, PEER_SERVICES);
        assert_eq!(
            data.addr_token_timestamp_us,
            T * 1_000_000,
            "the session clock, not the wall clock"
        );
        let state = outcome.state.as_ref().expect("a CNodeState");
        assert!(!state.inbound);

        // Outbound: the version's services reach AddrMan, its time the time data.
        let session = s.session_outcome().unwrap();
        assert!(
            session.calls.contains(&Call::SetServices {
                addr: addr.clone(),
                services: PEER_SERVICES,
            }),
            "{:?}",
            session.calls
        );
        assert!(
            session.calls.iter().any(|c| matches!(
                c,
                Call::TimeData {
                    offset: 0,
                    ..
                }
            )),
            "{:?}",
            session.calls
        );
        assert_eq!((session.tip.height, session.globals.initial_sync_finished), (0, false));
        assert!(
            !session.draws.is_empty(),
            "the ConnMan seeds, nonces and shuffles are drawn from the seed"
        );
        // Outcomes report what happened since the last call.
        assert_eq!(s.peer_outcome(peer).unwrap().sent, []);
        assert_eq!(s.session_outcome().unwrap().calls, []);
    }
}

#[test]
fn a_pong_marker_answers_our_ping() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 2);
        let (peer, outcome) = handshake(&mut s, service([1, 2, 3, 5], 19899), 8);
        let ping = &outcome.sent.iter().find(|(c, _)| c == "ping").unwrap().1;
        let nonce = outcome.peer.as_ref().unwrap().ping_nonce_sent;
        assert_eq!(ping[..], nonce.to_le_bytes(), "ping carries the nonce Peer records");
        assert_ne!(nonce, 0);

        let echo = |src_command: &str| Substitution {
            dst_offset: 0,
            src_peer: peer,
            src_command: src_command.into(),
            src_offset: 0,
            len: 8,
        };
        // No such message was sent, or the range is outside it: refused before queueing.
        assert_eq!(
            s.deliver(peer, "pong", &[0; 8], &[echo("getdata")]).unwrap_err().kind,
            ErrorKind::Argument
        );
        let mut long = echo("ping");
        long.src_offset = 1;
        assert_eq!(
            s.deliver(peer, "pong", &[0; 8], &[long]).unwrap_err().kind,
            ErrorKind::Argument
        );

        s.deliver(peer, "pong", &[0; 8], &[echo("ping")]).unwrap();
        s.send_pass(T + 1).unwrap();
        let after = s.peer_outcome(peer).unwrap();
        assert_eq!(after.peer.unwrap().ping_nonce_sent, 0, "the pong matched");
        assert!(after.misbehaving.is_empty());
    }
}

#[test]
fn an_oversized_addr_is_misbehaviour() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 3);
        let (peer, _) = handshake(&mut s, service([1, 2, 3, 6], 19899), 9);
        // addr (v1): 1001 entries of time, services, IPv6 bytes, port.
        let mut addr = vec![0xfd, 0xe9, 0x03];
        for i in 0..1001u32 {
            addr.extend((T as u32).to_le_bytes());
            addr.extend(1u64.to_le_bytes());
            addr.extend([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 9, 9]);
            addr.extend((i as u16).to_be_bytes());
            addr.extend(9999u16.to_be_bytes());
        }
        s.deliver(peer, "addr", &addr, &[]).unwrap();
        s.send_pass(T).unwrap();
        let outcome = s.peer_outcome(peer).unwrap();
        assert_eq!(outcome.misbehaving.len(), 1, "{:?}", outcome.misbehaving);
        assert!(outcome.misbehaving[0].starts_with(&format!("Misbehaving: peer={peer} (0 -> 20)")));
        assert!(outcome.misbehaving[0].contains("addr message size = 1001"));
        assert_eq!(outcome.peer.unwrap().misbehavior_score, 20);
    }
}

#[test]
fn an_obsolete_version_disconnects_and_removes_the_peer() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 4);
        let peer = s.add_peer(ConnType::Inbound, &service([5, 6, 7, 8], 40000)).unwrap();
        assert_eq!(s.peer_outcome(peer).unwrap().sent, [], "we wait for an inbound peer's version");
        let duplicate = s.add_peer(ConnType::Inbound, &service([5, 6, 7, 8], 40000)).unwrap_err();
        assert_eq!(duplicate.kind, ErrorKind::Argument);

        s.deliver(peer, "version", &version_payload(70000, 1, 0), &[]).unwrap();
        s.send_pass(T).unwrap();
        let outcome = s.peer_outcome(peer).unwrap();
        assert!(outcome.removed && outcome.disconnect, "{outcome:?}");
        assert_eq!(outcome.sent, []);
        // The snapshot is the peer's state just before FinalizeNode.
        assert!(outcome.peer.is_some() && outcome.state.is_some());
        assert_eq!(s.deliver(peer, "verack", &[], &[]).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(s.peer_outcome(99).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(s.send_pass(0).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(s.add_peer(ConnType::Inbound, &service([5, 6, 7, 9], 1)).unwrap(), peer + 1);
    }
}

/// `n` regtest blocks on `prev` (a coinbase paying nothing to OP_TRUE each), mined against
/// regtest's 0x207fffff target.
fn mine(prev: [u8; 32], first_height: u8, n: u8) -> Vec<Vec<u8>> {
    mine_branch(prev, first_height, n, 0)
}

/// [`mine`], with `branch` seconds added to each block time (a different chain on `prev`).
fn mine_branch(mut prev: [u8; 32], first_height: u8, n: u8, branch: u32) -> Vec<Vec<u8>> {
    let mut blocks = Vec::new();
    for height in first_height..first_height + n {
        assert!((1..=16).contains(&height), "BIP34 height as a small-integer opcode");
        // Coinbase: version 1; the null prevout; scriptSig <height> OP_0 (BlockAssembler's).
        let mut tx = vec![1, 0, 0, 0, 1];
        tx.extend([0; 32]);
        tx.extend([0xff; 4]);
        tx.extend([2, 0x50 + height, 0x00]);
        tx.extend([0xff; 4]);
        tx.push(1);
        tx.extend(0u64.to_le_bytes());
        tx.extend([1, 0x51]);
        tx.extend([0; 4]);
        let merkle_root = sha256d::Hash::hash(&tx).to_byte_array();
        let mut header = Vec::new();
        header.extend(0x2000_0000i32.to_le_bytes());
        header.extend(prev);
        header.extend(merkle_root);
        header.extend((GENESIS_TIME as u32 + 150 * u32::from(height) + branch).to_le_bytes());
        header.extend(0x207f_ffffu32.to_le_bytes());
        let hash = (0u32..)
            .find_map(|nonce| {
                let mut h = header.clone();
                h.extend(nonce.to_le_bytes());
                let hash = deserialize::<dashcore::block::Header>(&h)
                    .unwrap()
                    .block_hash()
                    .to_byte_array();
                (hash[31] < 0x7f).then(|| {
                    header = h;
                    hash
                })
            })
            .unwrap();
        let mut block = header;
        block.push(1);
        block.extend(tx);
        blocks.push(block);
        prev = hash;
    }
    blocks
}

#[test]
fn fixture_blocks_become_the_tip() {
    for oracle in Oracle::all() {
        let genesis = {
            let mut s = session(&oracle, 5);
            let tip = s.session_outcome().unwrap().tip;
            assert_eq!(tip.height, 0);
            tip.hash
        };
        let blocks = mine(genesis, 1, 3);
        {
            let mut s =
                oracle.peer_session_new(&config(5, blocks.clone())).unwrap().expect("accepted");
            let outcome = s.session_outcome().unwrap();
            assert_eq!(outcome.tip.height, 3);
            let last = deserialize::<dashcore::block::Header>(&blocks[2][..80]).unwrap();
            assert_eq!(outcome.tip.hash, last.block_hash().to_byte_array());
            assert_eq!(outcome.globals.best_height, 3, "AppInitMain's SetBestHeight");
        }
        // A block that does not connect is a verdict; bytes that are no block list an error.
        let mut bad = blocks.clone();
        bad[1][40] ^= 1;
        assert!(oracle.peer_session_new(&config(5, bad)).unwrap().is_none());
        let mut garbage = config(5, Vec::new());
        garbage.blocks = vec![vec![0xff]];
        assert_eq!(oracle.peer_session_new(&garbage).err().unwrap().kind, ErrorKind::Argument);
        let mut late = config(5, Vec::new());
        late.mocktime = 0;
        assert_eq!(oracle.peer_session_new(&late).err().unwrap().kind, ErrorKind::Argument);
    }
}

/// Two outbound and one inbound peer through handshakes, pings and a timeout; every raw
/// outcome in order.
fn seeded_script(oracle: &Oracle, seed: u64, check_block_index: bool) -> Vec<Vec<u8>> {
    let config = SessionConfig {
        check_block_index,
        ..config(seed, Vec::new())
    };
    let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
    let mut out = Vec::new();
    let a = handshake(&mut s, service([1, 2, 3, 4], 19899), 11).0;
    let b = handshake(&mut s, service([4, 3, 2, 1], 19899), 12).0;
    let c = s.add_peer(ConnType::Inbound, &service([9, 9, 9, 9], 50000)).unwrap();
    s.deliver(c, "version", &version_payload(PROTOCOL_VERSION, 13, 0), &[]).unwrap();
    for (i, t) in [T, T + 60, T + 121, T + 20 * 60, T + 40 * 60].into_iter().enumerate() {
        passes(&mut s, 2 + i, t);
        for peer in [a, b, c, -1] {
            out.push(s.outcome_raw(peer).unwrap());
        }
    }
    out
}

fn digest(outputs: &[Vec<u8>]) -> String {
    let mut all = Vec::new();
    for o in outputs {
        all.extend((o.len() as u64).to_le_bytes());
        all.extend(o);
    }
    sha256::Hash::hash(&all).to_string()
}

/// Printed by the child processes of `seeded_sessions_replay_across_processes`.
#[test]
fn seeded_session_digest_child() {
    if std::env::var_os("O1C_SESSION_DIGEST_CHILD").is_none() {
        return;
    }
    for oracle in Oracle::all() {
        println!("DIGEST {} {}", oracle.pin(), digest(&seeded_script(&oracle, 42, true)));
    }
}

#[test]
fn seeded_sessions_replay_across_processes() {
    let mut want = Vec::new();
    for oracle in Oracle::all() {
        let first = seeded_script(&oracle, 42, true);
        assert_eq!(first, seeded_script(&oracle, 42, true), "same seed, same session");
        assert_ne!(
            digest(&first),
            digest(&seeded_script(&oracle, 43, true)),
            "the seed reaches the draws"
        );
        want.push(format!("DIGEST {} {}", oracle.pin(), digest(&first)));
    }
    for _ in 0..2 {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "seeded_session_digest_child", "--nocapture", "--test-threads=1"])
            .env("O1C_SESSION_DIGEST_CHILD", "1")
            .output()
            .unwrap();
        assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stderr));
        let lines: Vec<String> = String::from_utf8_lossy(&child.stdout)
            .lines()
            // libtest prints the line after "test seeded_session_digest_child ... ".
            .filter_map(|l| l.find("DIGEST ").map(|at| l[at..].to_owned()))
            .collect();
        assert_eq!(lines, want, "a fresh process replays the session");
    }
}

/// Sol r2 F1's scenarios: three sessions in one process, the first and third with the same
/// options and input. Each session takes ten copies of an unconnecting header (the tenth is
/// Misbehaving) after a handshake, and records every outcome raw, the handshake's included.
const LOGGER_HISTORIES: [(&str, [&[&str]; 3]); 2] = [
    ("quiet/debug/quiet", [&["-debug=0"], &["-debug=net"], &["-debug=0"]]),
    ("default/net:info/default", [&[], &["-loglevel=net:info"], &[]]),
];

fn logger_history_script(oracle: &Oracle, options: &[&str]) -> Vec<Vec<u8>> {
    let unconnecting = mine([0x77; 32], 1, 1);
    let mut headers = vec![1];
    headers.extend(&unconnecting[0][..80]);
    headers.push(0);
    let config = SessionConfig {
        chain: Chain::new("regtest", options),
        ..config(41, Vec::new())
    };
    let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
    // `handshake`'s, with every outcome raw.
    let peer = s.add_peer(ConnType::OutboundFullRelay, &service([1, 2, 3, 4], 19899)).unwrap();
    let ours = s.outcome_raw(peer).unwrap();
    let sent = PeerOutcome::decode(&ours).unwrap().sent;
    assert_eq!(sent[0].0, "version", "InitializeNode pushes our version to an outbound peer");
    let version = i32::from_le_bytes(sent[0].1[..4].try_into().unwrap());
    let mut out = vec![ours, s.outcome_raw(-1).unwrap()];
    s.deliver(peer, "version", &version_payload(version, 7, 0), &[]).unwrap();
    s.deliver(peer, "verack", &[], &[]).unwrap();
    for _ in 0..4 {
        s.send_pass(T).unwrap();
        out.extend([s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()]);
    }
    assert!(PeerOutcome::decode(&out[out.len() - 2]).unwrap().successfully_connected);
    for _ in 0..10 {
        s.deliver(peer, "headers", &headers, &[]).unwrap();
        s.send_pass(T).unwrap();
        out.extend([s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()]);
    }
    out
}

/// Printed by the child processes of `a_sessions_logging_starts_fresh`.
#[test]
fn logger_history_child() {
    let Ok(history) = std::env::var("O1C_LOGGER_HISTORY_CHILD") else {
        return;
    };
    let (_, sessions) = LOGGER_HISTORIES.iter().find(|(name, _)| *name == history).unwrap();
    for oracle in Oracle::all() {
        for options in sessions {
            println!(
                "DIGEST {} {}",
                oracle.pin(),
                digest(&logger_history_script(&oracle, options))
            );
        }
    }
}

/// Sol r2 F1: a session's logging (which decides what Core computes and records: NET lines hash
/// headers and report Misbehaving) does not depend on an earlier session's options. In a fresh
/// process, the third session's complete raw transcript is the first's, and the second's, whose
/// options log differently, is not.
#[test]
fn a_sessions_logging_starts_fresh() {
    for (history, _) in LOGGER_HISTORIES {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "logger_history_child", "--nocapture", "--test-threads=1"])
            .env("O1C_LOGGER_HISTORY_CHILD", history)
            .output()
            .unwrap();
        assert!(child.status.success(), "{}", String::from_utf8_lossy(&child.stderr));
        let digests: Vec<String> = String::from_utf8_lossy(&child.stdout)
            .lines()
            .filter_map(|l| l.find("DIGEST ").map(|at| l[at..].to_owned()))
            .collect();
        assert_eq!(digests.len(), 3 * Oracle::all().count(), "{history}: {digests:?}");
        for pin in digests.chunks(3) {
            assert_eq!(pin[0], pin[2], "{history}: the same session after another");
            assert_ne!(pin[0], pin[1], "{history}: the options change the transcript");
        }
    }
}

/// O1c ruling: an outcome too big for the first buffer is collected without draining the
/// session's logs (or a peer's messages) twice: a forced retry and a twin session read in one
/// go agree, now and on the next outcome.
#[test]
fn an_oversized_outcome_is_taken_once() {
    for oracle in Oracle::all() {
        let mut outcomes = Vec::new();
        for capacity in [16, 1 << 20] {
            let mut s = session(&oracle, 6);
            let (peer, _) = handshake(&mut s, service([1, 2, 3, 7], 19899), 10);
            s.send_pass(T + 30 * 60).unwrap();
            let first = (
                s.outcome_raw_with_capacity(-1, capacity).unwrap(),
                s.outcome_raw_with_capacity(peer, capacity).unwrap(),
            );
            assert!(first.0.len() > 16 && first.1.len() > 16);
            s.send_pass(T + 31 * 60).unwrap();
            let next = (s.outcome_raw(-1).unwrap(), s.outcome_raw(peer).unwrap());
            outcomes.push((first, next));
        }
        assert!(outcomes[0] == outcomes[1], "a forced retry changed the session's outcomes");
    }
}

/// The block hash (uint256 byte order) of a serialized block.
fn block_hash(block: &[u8]) -> [u8; 32] {
    deserialize::<dashcore::block::Header>(&block[..80]).unwrap().block_hash().to_byte_array()
}

fn genesis_hash(oracle: &Oracle) -> [u8; 32] {
    session(oracle, 0).session_outcome().unwrap().tip.hash
}

/// F1: our services are ConnMan's: they reach our version message and the handlers that check
/// them (`filterload` without NODE_BLOOM disconnects).
#[test]
fn our_services_are_the_nodes() {
    for oracle in Oracle::all() {
        for services in [0, 5, 1024, 2048] {
            let mut config = config(20, Vec::new());
            config.our_services = services;
            let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
            let peer =
                s.add_peer(ConnType::OutboundFullRelay, &service([1, 2, 3, 4], 19899)).unwrap();
            let ours = s.peer_outcome(peer).unwrap();
            assert_eq!(commands(&ours), ["version"]);
            assert_eq!(
                u64::from_le_bytes(ours.sent[0].1[4..12].try_into().unwrap()),
                services,
                "the version's services"
            );
        }
        // filterload: version 0x01 filter of one byte, one hash function, tweak 0, flags 0.
        let filterload = [1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0];
        for (services, kept) in [(1, false), (5, true)] {
            let mut config = config(21, Vec::new());
            config.our_services = services;
            let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
            let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 1);
            s.deliver(peer, "filterload", &filterload, &[]).unwrap();
            s.send_pass(T).unwrap();
            let outcome = s.peer_outcome(peer).unwrap();
            assert_eq!(!outcome.removed, kept, "services {services}: {outcome:?}");
            assert!(outcome.misbehaving.is_empty());
        }
    }
}

/// F3: nine unprotected outbound full-relay peers (production's target is eight): the
/// maintenance tick disconnects the youngest once it has been connected for more than
/// MINIMUM_CONNECT_TIME (30 s), and only one.
#[test]
fn maintenance_evicts_an_extra_outbound_peer() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 22);
        let peers: Vec<i64> = (0..9u8)
            .map(|i| handshake(&mut s, service([1, 2, 3, 10 + i], 19899), u64::from(i) + 100).0)
            .collect();
        s.tick(T + 30, TICK_MAINTENANCE).unwrap();
        assert!(
            peers.iter().all(|&p| !s.peer_outcome(p).unwrap().removed),
            "not yet connected 30 s"
        );
        s.tick(T + 31, TICK_MAINTENANCE).unwrap();
        let removed: Vec<i64> =
            peers.iter().copied().filter(|&p| s.peer_outcome(p).unwrap().removed).collect();
        assert_eq!(removed, [peers[8]], "the youngest of the tied peers");
        s.tick(T + 76, TICK_MAINTENANCE).unwrap();
        assert!(
            peers[..8].iter().all(|&p| !s.peer_outcome(p).unwrap().removed),
            "eight is the target"
        );
        // The inactivity check leaves connected, talking peers alone.
        s.tick(T + 77, TICK_INACTIVITY).unwrap();
        assert!(peers[..8].iter().all(|&p| !s.peer_outcome(p).unwrap().removed));
        assert_eq!(s.tick(T + 78, 0).unwrap_err().kind, ErrorKind::Argument);
        assert_eq!(s.tick(T + 78, 4).unwrap_err().kind, ErrorKind::Argument);
    }
}

/// F3: CConnman::InactivityCheck after -peertimeout (60 s): a peer that never sent a byte, and
/// one that sent its version but no verack, are disconnected; a connected one is not.
#[test]
fn inactivity_disconnects_silent_and_half_handshaken_peers() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 23);
        let silent = s.add_peer(ConnType::Inbound, &service([9, 9, 9, 1], 50000)).unwrap();
        let half = s.add_peer(ConnType::Inbound, &service([9, 9, 9, 2], 50000)).unwrap();
        s.deliver(half, "version", &version_payload(PROTOCOL_VERSION, 31, 0), &[]).unwrap();
        s.send_pass(T).unwrap();
        let (connected, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 32);
        s.tick(T + 60, TICK_INACTIVITY).unwrap();
        assert!([silent, half, connected].iter().all(|&p| !s.peer_outcome(p).unwrap().removed));
        s.tick(T + 61, TICK_INACTIVITY).unwrap();
        let removed: Vec<bool> =
            [silent, half, connected].iter().map(|&p| s.peer_outcome(p).unwrap().removed).collect();
        assert_eq!(removed, [true, true, false]);
    }
}

/// `pause_send` is CConnman's formula over the reported charges, at every observation.
fn send_state(s: &mut PeerSession, peer: i64, limit: u64) -> PeerOutcome {
    let o = s.peer_outcome(peer).unwrap();
    assert_eq!(
        o.pause_send,
        o.send_queue_memusage + o.transport_memusage > limit,
        "fPauseSend against the charges: {o:?}"
    );
    o
}

/// A controlled peer through version/verack, reading everything; `-maxsendbuffer=1` (a 1000
/// byte send buffer, init.cpp's 1000 * the option) unless `chain` says otherwise.
fn controlled_handshake(
    s: &mut PeerSession,
    transport: Transport,
    addr: Service,
    nonce: u64,
    t: i64,
) -> i64 {
    let peer = s
        .add_peer_with(ConnType::OutboundFullRelay, transport, SendMode::Controlled, &addr)
        .unwrap();
    assert!(s.drain(peer, u64::MAX).unwrap() > 0);
    assert_eq!(commands(&s.peer_outcome(peer).unwrap()), ["version"]);
    s.deliver(peer, "version", &version_payload(PROTOCOL_VERSION, nonce, 0), &[]).unwrap();
    s.deliver(peer, "verack", &[], &[]).unwrap();
    for _ in 0..4 {
        s.send_pass(t).unwrap();
        s.drain(peer, u64::MAX).unwrap();
    }
    let o = s.peer_outcome(peer).unwrap();
    assert!(o.successfully_connected && !o.pause_send && o.queued_messages == 0, "{o:?}");
    peer
}

fn small_buffer(seed: u64, services: u64, blocks: Vec<Vec<u8>>, mocktime: i64) -> SessionConfig {
    SessionConfig {
        chain: Chain::new("regtest", &["-maxsendbuffer=1"]),
        our_services: services,
        mocktime,
        rng_seed: seed,
        blocks,
        min_chain_work: None,
        max_tip_age: None,
        check_block_index: true,
    }
}

/// G3, many small pongs: a peer that does not read fills the node's send buffer one pong per
/// ping until fPauseSend holds, after which its pings wait (ProcessMessages skips it) until it
/// reads.
#[test]
fn unread_pongs_pause_the_peer() {
    for oracle in Oracle::all() {
        let mut s = oracle.peer_session_new(&small_buffer(24, 5, Vec::new(), T)).unwrap().unwrap();
        let peer = controlled_handshake(&mut s, Transport::V1, service([1, 2, 3, 4], 19899), 1, T);
        // A written V1 message still charges its emptied CSerializedNetMsg.
        let idle = send_state(&mut s, peer, 1000).transport_memusage;
        assert!(idle > 0);
        let (mut pongs, mut paused_at) = (0, None);
        for i in 0..40u64 {
            s.deliver(peer, "ping", &i.to_le_bytes(), &[]).unwrap();
            s.send_pass(T).unwrap();
            let o = send_state(&mut s, peer, 1000);
            pongs += o.sent.iter().filter(|(c, _)| c == "pong").count();
            if o.pause_send {
                paused_at.get_or_insert(i);
            }
        }
        let paused_at = paused_at.expect("the buffer fills");
        assert_eq!(pongs as u64, paused_at + 1, "one pong per ping until the pause, none after");
        let before = send_state(&mut s, peer, 1000);
        assert_eq!(before.queued_messages, pongs as u64);

        let written = s.drain(peer, u64::MAX).unwrap();
        let after = send_state(&mut s, peer, 1000);
        assert!(
            !after.pause_send && after.queued_messages == 0 && after.transport_memusage == idle
        );
        assert_eq!(after.send_bytes - before.send_bytes, written);
        assert_eq!(after.peer_decoded_messages - before.peer_decoded_messages, pongs as u64);
        // The held pings are answered now, one per pass.
        s.send_pass(T).unwrap();
        let o = send_state(&mut s, peer, 1000);
        assert_eq!(commands(&o), ["pong"]);
    }
}

/// A full receive queue: past `-maxreceivebuffer` (1000 bytes) of unprocessed messages
/// fPauseRecv holds and the node stops reading the peer's socket, as the socket handler does;
/// what the peer wrote waits there, and is read once the passes have processed enough.
#[test]
fn a_full_receive_queue_stops_reading() {
    for oracle in Oracle::all() {
        let config = SessionConfig {
            chain: Chain::new("regtest", &["-maxsendbuffer=1", "-maxreceivebuffer=1"]),
            ..small_buffer(26, 5, Vec::new(), T)
        };
        let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
        let peer = controlled_handshake(&mut s, Transport::V1, service([1, 2, 3, 4], 19899), 1, T);
        assert!(!s.peer_outcome(peer).unwrap().pause_recv);
        // A V1 ping is 32 bytes: the 32nd takes the queue to 1024 > 1000.
        for i in 0..40u64 {
            s.deliver(peer, "ping", &i.to_le_bytes(), &[]).unwrap();
        }
        let o = s.peer_outcome(peer).unwrap();
        assert!(o.pause_recv, "{o:?}");
        assert_eq!(o.peer_unread_bytes, 8 * 32);
        let mut pongs = Vec::new();
        for _ in 0..100 {
            s.drain(peer, u64::MAX).unwrap();
            s.send_pass(T).unwrap();
            let o = s.peer_outcome(peer).unwrap();
            pongs.extend(o.sent.iter().filter(|(c, _)| c == "pong").map(|(_, p)| p.clone()));
            if pongs.len() == 40 {
                break;
            }
        }
        let nonces: Vec<Vec<u8>> = (0..40u64).map(|i| i.to_le_bytes().to_vec()).collect();
        assert_eq!(pongs, nonces, "every ping answered, in order");
        let o = s.peer_outcome(peer).unwrap();
        assert!(!o.pause_recv && o.peer_unread_bytes == 0, "{o:?}");
    }
}

/// G3, one partly sent large payload: 16 headers (1297 bytes, above the 1000-byte buffer)
/// written a hundred bytes at a time. Once SocketSendData hands the message to the transport its
/// charge moves from the queue to the transport, and fPauseSend holds until the last byte.
#[test]
fn a_part_written_payload_keeps_its_charge() {
    for oracle in Oracle::all() {
        let genesis = genesis_hash(&oracle);
        let blocks = mine(genesis, 1, 16);
        // Ten minutes after the tip: out of initial block download, so getheaders is served.
        let t = GENESIS_TIME + 150 * 16 + 600;
        let mut s =
            oracle.peer_session_new(&small_buffer(25, 5, blocks.clone(), t)).unwrap().unwrap();
        let peer = controlled_handshake(&mut s, Transport::V1, service([1, 2, 3, 4], 19899), 1, t);
        let idle = send_state(&mut s, peer, 1000).transport_memusage;
        let mut getheaders = Vec::new();
        getheaders.extend(PROTOCOL_VERSION.to_le_bytes());
        getheaders.push(1);
        getheaders.extend(genesis);
        getheaders.extend([0; 32]);
        s.deliver(peer, "getheaders", &getheaders, &[]).unwrap();
        s.send_pass(t).unwrap();
        let queued = send_state(&mut s, peer, 1000);
        let headers = queued.sent.iter().find(|(c, _)| c == "headers").expect("headers").1.clone();
        assert_eq!(headers.len(), 1 + 16 * 81);
        assert!(queued.pause_send && queued.transport_memusage == idle, "{queued:?}");

        let mut last = queued.clone();
        let mut written = 0;
        while written < 24 + headers.len() as u64 {
            let n = s.drain(peer, 100).unwrap();
            assert!(n <= 100);
            written += n;
            let o = send_state(&mut s, peer, 1000);
            assert_eq!(o.send_bytes - last.send_bytes, n);
            if written < 24 + headers.len() as u64 {
                assert!(o.pause_send && o.send_queue_memusage == 0, "{o:?}");
                assert!(o.transport_memusage > 1000, "the retained message: {o:?}");
                assert_eq!(o.peer_decoded_messages, last.peer_decoded_messages, "not yet whole");
            }
            last = o;
        }
        assert!(!last.pause_send && last.transport_memusage == idle);
        assert_eq!(last.peer_decoded_messages, queued.peer_decoded_messages + 1);
    }
}

/// G3, the V1/V2 charge boundary: the same pong charges both queues alike; part-written, V1
/// charges the message it retains and V2 its encrypted send buffer; written, V1 still charges the
/// emptied message and V2 the buffer's allocation.
#[test]
fn v1_and_v2_transports_charge_differently() {
    const NODE_P2P_V2: u64 = 1 << 12;
    for oracle in Oracle::all() {
        let mut s = oracle
            .peer_session_new(&small_buffer(26, 5 | NODE_P2P_V2, Vec::new(), T))
            .unwrap()
            .unwrap();
        let v1 = controlled_handshake(&mut s, Transport::V1, service([1, 2, 3, 4], 19899), 1, T);
        let v2 = controlled_handshake(&mut s, Transport::V2, service([1, 2, 3, 5], 19899), 2, T);
        let (o1, o2) = (send_state(&mut s, v1, 1000), send_state(&mut s, v2, 1000));
        assert_eq!((o1.transport, o2.transport), (1, 2));
        assert!(o1.transport_memusage > 0, "sizeof(CSerializedNetMsg): {o1:?}");
        assert!(o2.transport_memusage >= 24, "sizeof(m_send_buffer) at least: {o2:?}");

        let mut charges = Vec::new();
        for peer in [v1, v2] {
            s.deliver(peer, "ping", &[7; 8], &[]).unwrap();
            s.send_pass(T).unwrap();
            let queued = send_state(&mut s, peer, 1000);
            assert_eq!(commands(&queued), ["pong"]);
            assert_eq!(s.drain(peer, 1).unwrap(), 1);
            let part = send_state(&mut s, peer, 1000);
            assert_eq!(part.send_queue_memusage, 0);
            s.drain(peer, u64::MAX).unwrap();
            let written = send_state(&mut s, peer, 1000);
            assert!(!written.pause_send && written.queued_messages == 0);
            charges.push((queued.send_queue_memusage, part.transport_memusage));
        }
        assert_eq!(charges[0].0, charges[1].0, "one pong, one queue charge");
        assert_eq!(charges[0].1, charges[0].0, "V1 retains the message itself");
        assert_ne!(charges[1].1, charges[1].0, "V2 charges its encrypted buffer");
    }
}

/// BIP324 between the node and a peer, both ways, and an inbound V2 node detecting a V1 peer;
/// combinations CConnman never makes are refused.
#[test]
fn v2_transports_handshake() {
    const NODE_P2P_V2: u64 = 1 << 12;
    for oracle in Oracle::all() {
        let mut config = config(27, Vec::new());
        config.our_services = 5 | NODE_P2P_V2;
        let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
        let outbound = s
            .add_peer_with(
                ConnType::OutboundFullRelay,
                Transport::V2,
                SendMode::Eager,
                &service([1, 2, 3, 4], 19899),
            )
            .unwrap();
        let inbound = s
            .add_peer_with(
                ConnType::Inbound,
                Transport::V2,
                SendMode::Eager,
                &service([9, 9, 9, 9], 50000),
            )
            .unwrap();
        let detecting = s
            .add_peer_with(
                ConnType::Inbound,
                Transport::V2DetectingV1Peer,
                SendMode::Eager,
                &service([9, 9, 9, 8], 50000),
            )
            .unwrap();
        for (peer, nonce) in [(outbound, 1), (inbound, 2), (detecting, 3)] {
            s.deliver(peer, "version", &version_payload(PROTOCOL_VERSION, nonce, 0), &[]).unwrap();
            s.deliver(peer, "verack", &[], &[]).unwrap();
        }
        passes(&mut s, 6, T);
        for (peer, transport) in [(outbound, 2), (inbound, 2), (detecting, 1)] {
            let o = s.peer_outcome(peer).unwrap();
            assert!(o.successfully_connected && !o.removed, "{o:?}");
            assert_eq!(o.transport, transport);
            assert!(o.peer_decoded_messages >= 2, "version and verack at least: {o:?}");
        }
        for (conn, transport) in [
            (ConnType::Inbound, Transport::V1),
            (ConnType::OutboundFullRelay, Transport::V2DetectingV1Peer),
        ] {
            let refused = s
                .add_peer_with(conn, transport, SendMode::Eager, &service([5, 5, 5, 5], 1))
                .unwrap_err();
            assert_eq!(refused.kind, ErrorKind::Argument);
        }
        drop(s);
        let mut s = session(&oracle, 27);
        let refused = s
            .add_peer_with(
                ConnType::OutboundFullRelay,
                Transport::V2,
                SendMode::Eager,
                &service([1, 2, 3, 4], 19899),
            )
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Argument, "V2 needs NODE_P2P_V2 in our services");
    }
}

/// Sol r1's backpressure probe: one getdata for two blocks is answered one block per pass, as
/// ProcessGetData serves one block per call ("only process one BLOCK item per call"), but with
/// production's send buffer the first reply no longer pauses the peer.
#[test]
fn getdata_for_two_blocks_is_answered_a_block_per_pass() {
    for oracle in Oracle::all() {
        let blocks = mine(genesis_hash(&oracle), 1, 3);
        let mut s = oracle.peer_session_new(&config(28, blocks.clone())).unwrap().unwrap();
        let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 5);
        let mut getdata = vec![2];
        for b in &blocks[..2] {
            getdata.extend(2u32.to_le_bytes());
            getdata.extend(block_hash(b));
        }
        s.deliver(peer, "getdata", &getdata, &[]).unwrap();
        s.send_pass(T).unwrap();
        let first = s.peer_outcome(peer).unwrap();
        assert_eq!(commands(&first), ["block"]);
        assert_eq!(first.sent[0].1, blocks[0]);
        assert!(!first.pause_send, "{first:?}");
        s.send_pass(T).unwrap();
        let second = s.peer_outcome(peer).unwrap();
        assert_eq!(commands(&second), ["block"]);
        assert_eq!(second.sent[0].1, blocks[1]);
    }
}

/// The keystream output of each self-seeding `FastRandomContext` in a draw log, in order: its
/// construction, its seed from `GetRandHash`, then its first keystream bytes.
fn seeded_draws(draws: &[Draw]) -> Vec<Vec<u8>> {
    draws
        .windows(3)
        .filter_map(|w| match (&w[0], &w[1], &w[2]) {
            (
                Draw::NewContext {
                    ctx,
                    deterministic: false,
                },
                Draw::RandBytes(seed),
                Draw::Keystream {
                    ctx: c,
                    bytes,
                },
            ) if seed.len() == 32 && ctx == c => Some(bytes.clone()),
            _ => None,
        })
        .collect()
}

/// D38: a session draws what a dashd started on a fresh datadir draws, in its order (oracle.h,
/// peer sessions): ECC_Start's 32 bytes, the sanity check's key and the 8 bytes its
/// `VerifyPubKey` signs (`ECC_InitSanityCheck`), the script-execution cache's nonce, LoadAddrman's two AddrMans (each a 32-byte key, then its
/// address map's two hash salts) and peers.dat's temporary name, BanMan's filter tweak, then
/// ConnMan's seeds, nSeed1 first (init.cpp evaluates make_unique<CConnman>'s arguments right to
/// left in a GCC build), and nothing between them. Which draw is which seed shows in our version
/// nonce: CSipHasher(nSeed0, nSeed1) over RANDOMIZER_ID_LOCALHOSTNONCE and the NodeId. From
/// LoadAddrman on this is SB6's D38 table, steps (1) to (6), with the store's step (3) drawn as
/// dashd's peers.dat write draws it.
#[test]
fn setup_draws_in_inits_order() {
    const RANDOMIZER_ID_LOCALHOSTNONCE: u64 = 0xd93e69e2bbfa5735;
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 40);
        let draws = s.session_outcome().unwrap().draws;
        // The tests' own context is seeded without a draw; then ECC_Start, the sanity check's key
        // and its signature's randomness, and the cache nonce.
        assert!(
            matches!(&draws[..5], [
                Draw::NewContext { deterministic: true, .. },
                Draw::RandBytes(ecc),
                Draw::StrongRandBytes(key),
                Draw::RandBytes(signed),
                Draw::RandBytes(cache),
            ] if ecc.len() == 32 && key.len() == 32 && signed.len() == 8 && cache.len() == 32),
            "{draws:?}"
        );
        // Ten self-seeding contexts, each drawing at once, tile the next 30 draws.
        let seeded = seeded_draws(&draws[5..35]);
        let lens: Vec<usize> = seeded.iter().map(Vec::len).collect();
        // AddrMan, AddrMan', peers.dat's name, BanMan, nSeed1, nSeed0.
        assert_eq!(lens, [32, 8, 8, 32, 8, 8, 8, 8, 8, 8], "{draws:?}");
        let seed1 = u64::from_le_bytes(seeded[8][..].try_into().unwrap());
        let seed0 = u64::from_le_bytes(seeded[9][..].try_into().unwrap());

        let peer = s.add_peer(ConnType::OutboundFullRelay, &service([1, 2, 3, 4], 19899)).unwrap();
        let ours = s.peer_outcome(peer).unwrap();
        let version = &ours.sent[0].1;
        // version, services, time, addr_recv and addr_from (26 bytes each), then the nonce.
        let nonce = u64::from_le_bytes(version[72..80].try_into().unwrap());
        let mut data = RANDOMIZER_ID_LOCALHOSTNONCE.to_le_bytes().to_vec();
        data.extend(peer.to_le_bytes());
        let expected =
            bitcoin_hashes::siphash24::Hash::hash_with_keys(seed0, seed1, &data).as_u64();
        assert_eq!(nonce, expected, "our version nonce is keyed (nSeed0, nSeed1)");
    }
}

/// `GetRand` shapes in a draw log (`GetRandInternal`: a self-seeding `FastRandomContext`, its
/// seed from `GetRandHash`, then one `randrange`'s 8 keystream bytes).
fn get_rand_draws(draws: &[Draw]) -> usize {
    draws
        .windows(3)
        .filter(|w| {
            matches!((&w[0], &w[1], &w[2]),
                (Draw::NewContext { ctx, deterministic: false }, Draw::RandBytes(seed), Draw::Keystream { ctx: c, bytes })
                if seed.len() == 32 && ctx == c && bytes.len() == 8)
        })
        .count()
}

/// F6: the A1 fixture, then a peer's B1–B3 (headers, then each block): the reorg's
/// BlockDisconnected resets the recent-confirmed-transactions filter (`GetRand` for its tweak) on
/// the session's thread, so the draw is in the log, and equal seeds replay the whole session.
/// Returns every raw outcome, and the GetRand draws of each block step, with and without a reorg.
fn reorg_script(
    oracle: &Oracle,
    seed: u64,
    reorg: bool,
    check_block_index: bool,
) -> (Vec<Vec<u8>>, Vec<usize>) {
    let genesis = genesis_hash(oracle);
    let a1 = mine(genesis, 1, 1);
    let branch = if reorg {
        mine_branch(genesis, 1, 3, 1)
    } else {
        mine(block_hash(&a1[0]), 2, 3)
    };
    let config = SessionConfig {
        check_block_index,
        ..config(seed, a1)
    };
    let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
    let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 6);
    let mut raw = vec![s.outcome_raw(-1).unwrap()];
    let mut headers = vec![3];
    for b in &branch {
        headers.extend(&b[..80]);
        headers.push(0);
    }
    s.deliver(peer, "headers", &headers, &[]).unwrap();
    s.send_pass(T).unwrap();
    raw.extend([s.outcome_raw(peer).unwrap(), s.outcome_raw(-1).unwrap()]);
    let mut gets = Vec::new();
    for b in &branch {
        s.deliver(peer, "block", b, &[]).unwrap();
        s.send_pass(T).unwrap();
        raw.push(s.outcome_raw(peer).unwrap());
        let session = s.outcome_raw(-1).unwrap();
        let decoded = dash_oracle_sys::SessionOutcome::decode(&session).unwrap();
        gets.push(get_rand_draws(&decoded.draws));
        raw.push(session);
    }
    let tip = s.session_outcome().unwrap().tip;
    assert_eq!(
        (tip.height, tip.hash),
        (
            if reorg {
                3
            } else {
                4
            },
            block_hash(branch.last().unwrap())
        )
    );
    (raw, gets)
}

#[test]
fn a_reorgs_draws_are_logged_and_replay() {
    for oracle in Oracle::all() {
        let (first, reorg_gets) = reorg_script(&oracle, 30, true, true);
        let (_, plain_gets) = reorg_script(&oracle, 30, false, true);
        // Every connected block draws the same GetRands. B1 is a side branch (connects
        // nothing); B2 overtakes A1, so its step connects B1 and B2 and resets the filter
        // once; B3 extends the tip.
        let per_block = plain_gets[0];
        assert_eq!(plain_gets, [per_block; 3]);
        assert_eq!(
            reorg_gets,
            [0, 2 * per_block + 1, per_block],
            "the reset's GetRand: {reorg_gets:?} vs {plain_gets:?}"
        );
        assert_eq!(first, reorg_script(&oracle, 30, true, true).0, "same seed, same session");
        assert_ne!(first, reorg_script(&oracle, 31, true, true).0, "the seed reaches the draws");
    }
}

/// O2a-LQ1 Q2: outside an LLMQ context's calls the oracle's wrap of
/// `node::ReadBlockFromDisk(block, index, params)` is Core's function. The reorg reads A1 back to
/// disconnect it (DisconnectTip) and B1 to connect it (ConnectTip of a stored block) through
/// that overload, after an LLMQ context lived and was freed in this process: the tip is B3 only if
/// both reads returned the blocks from disk.
#[test]
fn a_reorg_reads_blocks_through_cores_read_block_from_disk() {
    for oracle in Oracle::all() {
        drop(oracle.llmq_context(&Chain::regtest()).unwrap());
        reorg_script(&oracle, 32, true, false);
    }
}

/// D-row: `-checkblockindex` changes no transcript. Over a fixed seed set, the handshake script
/// and the reorg script (headers, blocks, a side branch overtaking the tip: the block-index
/// changes the check runs after) give byte-identical outcomes, draws and calls included, with
/// the check on and off. CheckBlockIndex reads the block tree and asserts: no draws, no
/// messages, and its header-hash check is commented out (validation.cpp), so no X11 calls.
/// The witness that the option reaches Core: with it on, a `headers` message's allocations
/// include CheckBlockIndex's own, and with it off none do.
#[test]
fn check_block_index_changes_no_transcript() {
    const CHECK: &[&str] = &["15CheckBlockIndex"];
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        for seed in [7, 30, 42, 1 << 40, u64::MAX] {
            assert_eq!(
                seeded_script(&oracle, seed, true),
                seeded_script(&oracle, seed, false),
                "{pin}: seed {seed}"
            );
            for reorg in [false, true] {
                assert_eq!(
                    reorg_script(&oracle, seed, reorg, true).0,
                    reorg_script(&oracle, seed, reorg, false).0,
                    "{pin}: seed {seed}, reorg {reorg}"
                );
            }
        }
        let checked_allocations = |check_block_index| {
            let headers_of = mine(genesis_hash(&oracle), 1, 3);
            let config = SessionConfig {
                check_block_index,
                ..config(41, Vec::new())
            };
            let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
            let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 7);
            let mut headers = vec![3];
            for b in &headers_of {
                headers.extend(&b[..80]);
                headers.push(0);
            }
            let (delivered, outside) = alloc_fault::outside(CHECK, || {
                s.deliver(peer, "headers", &headers, &[])?;
                s.send_pass(T)
            });
            delivered.unwrap();
            assert_eq!(s.session_outcome().unwrap().tip.height, 0, "{pin}: headers only");
            outside.iter().filter(|outside| !**outside).count()
        };
        assert!(checked_allocations(true) > 0, "{pin}: the check runs when on");
        assert_eq!(checked_allocations(false), 0, "{pin}: and not when off");
    }
}

/// The call log records each X11 hash `CBlockHeader::GetHash` computes, in order (G1's
/// hash-call witness): a `headers` message of three new headers hashes them in message order
/// (the continuity check), again in acceptance, and the last once more, and nothing else.
#[test]
fn x11_calls_are_logged_in_order() {
    for oracle in Oracle::all() {
        let headers_of = mine(genesis_hash(&oracle), 1, 3);
        let mut s = session(&oracle, 41);
        let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 7);
        s.session_outcome().unwrap();
        let mut headers = vec![3];
        for b in &headers_of {
            headers.extend(&b[..80]);
            headers.push(0);
        }
        s.deliver(peer, "headers", &headers, &[]).unwrap();
        s.send_pass(T).unwrap();
        let hashes: Vec<[u8; 32]> = s
            .session_outcome()
            .unwrap()
            .calls
            .into_iter()
            .filter_map(|c| match c {
                Call::X11 {
                    hash,
                } => Some(hash),
                _ => None,
            })
            .collect();
        let expected: Vec<[u8; 32]> = headers_of.iter().map(|b| block_hash(b)).collect();
        assert!(hashes.iter().all(|h| expected.contains(h)), "{hashes:x?}");
        let mut first = hashes.clone();
        first.dedup();
        first.truncate(3);
        assert_eq!(first, expected, "first hashed in message order: {hashes:x?}");
        let counts: Vec<usize> =
            expected.iter().map(|e| hashes.iter().filter(|h| *h == e).count()).collect();
        assert_eq!(counts, [2, 2, 3], "{}: {hashes:x?}", oracle.pin());
    }
}

/// `min_chain_work` is the chainstate's `-minimumchainwork`: with it above the active tip's
/// work the node answers `getheaders` with no headers ("active chain has too little work",
/// net_processing.cpp:4796-4800), otherwise with the headers after the locator. Regtest's
/// blocks have work 2 each, so a tip at height 2 has 6: 6 is served and 7 is not (a
/// byte-swapped encoding would refuse both).
#[test]
fn min_chain_work_empties_the_getheaders_reply() {
    for oracle in Oracle::all() {
        let genesis = genesis_hash(&oracle);
        let answers = |min_chain_work: Option<[u8; 32]>| {
            let config = SessionConfig {
                min_chain_work,
                ..config(42, mine(genesis, 1, 2))
            };
            let mut s = oracle.peer_session_new(&config).unwrap().unwrap();
            let (peer, _) = handshake(&mut s, service([1, 2, 3, 4], 19899), 9);
            let mut getheaders = PROTOCOL_VERSION.to_le_bytes().to_vec();
            getheaders.push(1);
            getheaders.extend(genesis);
            getheaders.extend([0; 32]);
            s.deliver(peer, "getheaders", &getheaders, &[]).unwrap();
            s.send_pass(T).unwrap();
            let reply = s.peer_outcome(peer).unwrap();
            reply.sent.iter().find(|(c, _)| c == "headers").expect("a headers reply").1[0]
        };
        let work = |w: u8| {
            let mut be = [0; 32];
            be[31] = w;
            Some(be)
        };
        assert_eq!(answers(None), 2, "{}", oracle.pin());
        assert_eq!(answers(work(6)), 2, "{}", oracle.pin());
        assert_eq!(answers(work(7)), 0, "{}", oracle.pin());
        // Core's own refusal of a bad value.
        let bad = SessionConfig {
            chain: Chain::new("regtest", &["-minimumchainwork=xyz"]),
            ..config(42, Vec::new())
        };
        assert_eq!(oracle.peer_session_new(&bad).err().unwrap().kind, ErrorKind::Exception);
        assert!(oracle.peer_session_new(&config(42, Vec::new())).unwrap().is_some(), "freed");
    }
}

/// A controlled peer's markers copy only what its end has decoded: nothing before its first
/// drain, and after that the last decoded message of the command, not one still unread.
#[test]
fn a_controlled_peers_markers_copy_what_it_decoded() {
    for oracle in Oracle::all() {
        let mut s = session(&oracle, 43);
        let peer = s
            .add_peer_with(
                ConnType::OutboundFullRelay,
                Transport::V1,
                SendMode::Controlled,
                &service([1, 2, 3, 4], 19899),
            )
            .unwrap();
        let echo = |src_command: &str| Substitution {
            dst_offset: 0,
            src_peer: peer,
            src_command: src_command.into(),
            src_offset: 0,
            len: 4,
        };
        // Our version was pushed but not read.
        assert_eq!(commands(&s.peer_outcome(peer).unwrap()), ["version"]);
        let version = version_payload(PROTOCOL_VERSION, 3, 0);
        assert_eq!(
            s.deliver(peer, "version", &version, &[echo("version")]).unwrap_err().kind,
            ErrorKind::Argument
        );
        s.drain(peer, u64::MAX).unwrap();
        s.deliver(peer, "version", &version, &[echo("version")]).unwrap();
        s.deliver(peer, "verack", &[], &[]).unwrap();
        for _ in 0..4 {
            s.send_pass(T).unwrap();
            s.drain(peer, u64::MAX).unwrap();
        }
        let first = s.peer_outcome(peer).unwrap().peer.unwrap().ping_nonce_sent;
        assert_ne!(first, 0, "our first ping, decoded");
        let pong = |src_command: &str| Substitution {
            len: 8,
            ..echo(src_command)
        };
        s.deliver(peer, "pong", &[0; 8], &[pong("ping")]).unwrap();
        s.send_pass(T).unwrap();
        assert_eq!(s.peer_outcome(peer).unwrap().peer.unwrap().ping_nonce_sent, 0);
        // Two minutes on, a second ping is pushed but not read: a marker copies the first.
        s.send_pass(T + 121).unwrap();
        let second = s.peer_outcome(peer).unwrap().peer.unwrap().ping_nonce_sent;
        assert!(second != 0 && second != first);
        s.deliver(peer, "pong", &[0; 8], &[pong("ping")]).unwrap();
        s.send_pass(T + 121).unwrap();
        assert_eq!(
            s.peer_outcome(peer).unwrap().peer.unwrap().ping_nonce_sent,
            second,
            "the stale nonce does not match"
        );
        s.drain(peer, u64::MAX).unwrap();
        s.deliver(peer, "pong", &[0; 8], &[pong("ping")]).unwrap();
        s.send_pass(T + 121).unwrap();
        assert_eq!(s.peer_outcome(peer).unwrap().peer.unwrap().ping_nonce_sent, 0);
    }
}

/// oracle.h allows no other oracle call while a session lives. Another thread's call waits until
/// the session is freed; a call on the session's own thread is refused (`SessionLive`), where it
/// would otherwise wait for itself.
#[test]
fn other_calls_wait_for_a_live_session() {
    use std::sync::mpsc;
    use std::time::Duration;
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        let s = session(&oracle, 1);
        let refused =
            |e: dash_oracle_sys::Error| assert_eq!(e.kind, ErrorKind::SessionLive, "{pin}");
        refused(oracle.merkle_root(&[[1; 32]]).unwrap_err());
        refused(oracle.chainparams_json(&Chain::main()).unwrap_err());
        refused(oracle.peer_session_new(&config(2, Vec::new())).err().expect("refused"));
        let (done, finished) = mpsc::channel();
        let decoder = std::thread::spawn(move || {
            let root = oracle.merkle_root(&[[1; 32]]);
            done.send(()).unwrap();
            root
        });
        assert!(
            finished.recv_timeout(Duration::from_millis(500)).is_err(),
            "{pin}: a decoder ran during the session"
        );
        drop(s);
        finished
            .recv_timeout(Duration::from_secs(120))
            .expect("the call runs once the session is freed");
        assert_eq!(decoder.join().unwrap().unwrap(), ([1; 32], false), "{pin}");
    }
}

/// Run by `a_failed_teardown_poisons_the_pin` in a child process, with the shim's teardown
/// fault set (oracle.h): the session it leaks stays for the rest of the process, which ends
/// without running Core's static destructors.
#[test]
fn a_failed_teardown_poisons_the_pin_child() {
    let Some(path) = std::env::var_os("O1C_POISON_CHILD") else {
        return;
    };
    for oracle in Oracle::all() {
        let pin = oracle.pin();
        if path == "free" {
            drop(session(&oracle, 1));
        } else {
            // A refused start tears its session down too.
            let refused = config(1, vec![[vec![0; 80], vec![0]].concat()]);
            let e = oracle.peer_session_new(&refused).err().expect("the teardown failed");
            assert_eq!(e.kind, ErrorKind::Exception, "{pin}: {e}");
            assert!(e.message.starts_with("teardown failed: "), "{pin}: {e}");
        }
        let poisoned =
            |e: dash_oracle_sys::Error| assert_eq!(e.kind, ErrorKind::Poisoned, "{pin}: {e}");
        poisoned(oracle.merkle_root(&[[1; 32]]).unwrap_err());
        poisoned(oracle.chainparams_json(&Chain::main()).unwrap_err());
        poisoned(oracle.peer_session_new(&config(2, Vec::new())).err().expect("refused"));
        println!("POISONED {pin}");
    }
    // The leaked node's script-check threads still run, so Core's static destructors would
    // assert at exit (CCheckQueue): end without them.
    use std::io::Write;
    std::io::stdout().flush().unwrap();
    unsafe extern "C" {
        fn _exit(status: i32) -> !;
    }
    unsafe { _exit(0) }
}

/// A session whose teardown fails (at free, or after a refused start) leaves Core's state
/// unknown: every later call of its pin fails with `Poisoned` instead of running on that state.
#[test]
fn a_failed_teardown_poisons_the_pin() {
    for path in ["free", "start"] {
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_failed_teardown_poisons_the_pin_child", "--nocapture"])
            .env("O1C_POISON_CHILD", path)
            .env("DASH_ORACLE_FAULT_TEARDOWN", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success(),
            "{path}: {stdout}{}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert_eq!(stdout.matches("POISONED ").count(), Oracle::all().count(), "{path}: {stdout}");
    }
}
