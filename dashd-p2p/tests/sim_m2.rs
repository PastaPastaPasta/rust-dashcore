//! P2a-core in `sim` against SB6's network core: the message loop (P2a §3.1), discouragement
//! (§3.10), the M2 stubs and inline branches (§3.2.3, §3.11), `ProcessGetData` in the M2 state,
//! and the exception boundary (§3.2.4).
//!
//! Most tests run one branch through `PeerLogic::process_admitted` (`ProcessMessage` after SB6's
//! gates) on a peer that has not completed the handshake; the last section goes through SB6's
//! gates and handshake with `Sim::handshake`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dash_p2p::conn::Permissions;
use dash_p2p::policy::{self, ReadPlan, Throw};
use dash_p2p::rng::FastRandom;
use dash_p2p::{
    Address, CommandString, ConnectionType, NetAddr, NetEvents, PeerId, Service, ServiceFlags, Why,
};
use dash_p2p_core::{GetData, InvType, Inventory, NotFound};
use dash_p2p_transport::SERIALIZED_MSG_SIZE;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peer::WantsDsq;
use dashd_p2p::processing::seams::{GetDataBatch, ObjectRequests, StaticOverlayState};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, until};
use dashd_p2p::{HandlerCx, MessageLoop, NetHandler, PeerManager, Registry, Seams, Shared};

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn sim() -> Sim {
    sim_with(Registry::new(), |_| {})
}

fn sim_with(registry: Registry, hook: impl FnOnce(&mut dash_p2p::NetConfig)) -> Sim {
    sim_full(ProcessorOptions::default(), registry, Seams::default(), hook)
}

fn sim_full(
    opts: ProcessorOptions,
    registry: Registry,
    seams: Seams,
    hook: impl FnOnce(&mut dash_p2p::NetConfig),
) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, opts, registry, seams, hook)
}

fn with_seams(seams: Seams) -> Sim {
    sim_full(ProcessorOptions::default(), Registry::new(), seams, |_| {})
}

/// A node without `NODE_BLOOM`.
fn sim_without_bloom() -> Sim {
    sim_with(Registry::new(), |cfg| cfg.local_services = ServiceFlags::NODE_NETWORK)
}

fn inbound(sim: &Sim, n: u8) -> SimPeer {
    sim.connect(n, ConnectionType::Inbound, Permissions::NONE)
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

/// A CompactSize count followed by one 36-byte inventory entry per `(type, hash byte)`.
fn inv_bytes(entries: &[(u32, u8)]) -> Vec<u8> {
    let mut out = Vec::new();
    dash_types::CompactSize::from(entries.len() as u64).encode(&mut out);
    for (ty, tag) in entries {
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&[*tag; 32]);
    }
    out
}

/// `n` inventory entries of `ty`.
fn invs(n: usize, ty: u32) -> Vec<u8> {
    let entries: Vec<_> = (0..n).map(|i| (ty, (i % 251) as u8)).collect();
    inv_bytes(&entries)
}

/// A locator message (`getblocks`/`getheaders`): version, `n` hashes, stop hash.
fn locator(n: usize) -> Vec<u8> {
    let mut out = 70242i32.to_le_bytes().to_vec();
    dash_types::CompactSize::from(n as u64).encode(&mut out);
    out.extend(std::iter::repeat_n(7u8, 32 * n + 32));
    out
}

const MSG_TX: u32 = 1;
const MSG_BLOCK: u32 = 2;
const MSG_SPORK: u32 = 6;
const MSG_ISDLOCK: u32 = 31;

// ---- the loop (P2a §3.1.1)

#[test]
fn idle_peers_pass_without_work_or_sends() {
    let mut sim = sim();
    let peers: Vec<_> = (1..=3).map(|n| inbound(&sim, n)).collect();
    for p in &peers {
        assert!(sim.shared.has_peer(p.handle.id()));
    }
    assert!(!sim.pass());
    assert!(!sim.pass());
    for p in &peers {
        // Nothing is sent before `verack` except what the handshake pushes (section 2).
        assert!(sim.sent(p).is_empty());
        assert!(!p.handle.disconnect_requested());
    }
}

#[test]
fn finalize_removes_state_and_the_loop_drains_it() {
    let mut sim = sim();
    // Outbound peers serving blocks are preferred for download from `verack` on.
    let a = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let b = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    for p in [&a, &b] {
        sim.handshake(p, PeerVersionMsg::v24(SIM_START));
    }
    assert_eq!(sim.lp.logic().counters.num_preferred_download, 2);
    a.sock.close();
    until(|| !sim.shared.has_peer(a.handle.id()));
    assert!(sim.shared.has_peer(b.handle.id()));
    // `finalize` only queued the state: the counters move at the actor's next pass.
    assert_eq!(sim.lp.logic().counters.num_preferred_download, 2);
    assert!(!sim.pass());
    assert_eq!(sim.lp.logic().counters.num_preferred_download, 1);
}

/// Attaches a peer at the routable 93.184.216.`n` (AddrMan keeps no private address) and runs
/// its handshake.
fn routable(sim: &mut Sim, n: u8, conn_type: ConnectionType) -> SimPeer {
    let service = Service {
        addr: NetAddr::ipv4([93, 184, 216, n]),
        port: 19899,
    };
    let p = sim.attach_at(service, conn_type, Permissions::NONE, ServiceFlags::NONE, false, false);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    p
}

#[test]
fn addrman_connected_follows_the_finalize_gates() {
    // np:1848-1853: only a successfully connected, unscored, full outbound peer updates its
    // AddrMan entry (`Connected` stamps `nTime` once it is over 20 minutes old).
    let mut sim = sim();
    let peers = [
        routable(&mut sim, 1, ConnectionType::OutboundFullRelay),
        routable(&mut sim, 2, ConnectionType::OutboundFullRelay),
        routable(&mut sim, 3, ConnectionType::BlockRelay),
        routable(&mut sim, 4, ConnectionType::Inbound),
    ];
    let then = SIM_START - 3600;
    let entries: Vec<Address> = peers
        .iter()
        .map(|p| Address {
            service: p.handle.addr().service.clone(),
            services: ServiceFlags::NODE_NETWORK,
            time: u32::try_from(then).unwrap(),
        })
        .collect();
    assert!(sim.core.addrman().add(&entries, &NetAddr::ipv4([93, 184, 216, 9]), 0, SIM_START));
    sim.shared.misbehaving(peers[1].handle.id(), 1, "test");
    for p in &peers {
        sim.core.disconnect(p.handle.id(), Why::Processor("test"));
    }
    sim.remove_disconnected();
    let times: Vec<i64> = entries
        .iter()
        .map(|e| sim.core.addrman().get_address_info(&e.service).unwrap().time)
        .collect();
    assert_eq!(times, [SIM_START, then, then, then]);
}

#[test]
fn a_wake_is_consumed_by_the_next_wait() {
    let sim = sim();
    // A wake before the wait: no wait at all.
    sim.shared.wake();
    let t = Instant::now();
    sim.lp.wait(false);
    assert!(t.elapsed() < Duration::from_millis(80), "{:?}", t.elapsed());
    // The flag was cleared: the next wait times out at 100 ms.
    let t = Instant::now();
    sim.lp.wait(false);
    assert!(t.elapsed() >= Duration::from_millis(95), "{:?}", t.elapsed());
    // More work: no wait, and a pending wake is still cleared.
    sim.shared.wake();
    let t = Instant::now();
    sim.lp.wait(true);
    assert!(t.elapsed() < Duration::from_millis(80));
    let t = Instant::now();
    sim.lp.wait(false);
    assert!(t.elapsed() >= Duration::from_millis(95));
}

#[test]
fn a_message_arrival_wakes_the_loop() {
    let sim = sim();
    let p = inbound(&sim, 1);
    // Clear any wake from the attach.
    sim.lp.wait(true);
    sim.deliver(&p, &[("ping", 1u64.to_le_bytes().to_vec())]);
    let t = Instant::now();
    sim.lp.wait(false);
    assert!(t.elapsed() < Duration::from_millis(80), "{:?}", t.elapsed());
}

// ---- misbehaviour (P2a §3.10)

#[test]
fn discouragement_acts_in_the_next_send_messages() {
    let mut sim = sim();
    let a = inbound(&sim, 1);
    let b = inbound(&sim, 2);
    sim.shared.misbehaving(a.handle.id(), 60, "first");
    assert!(!sim.pass());
    assert!(!a.handle.disconnect_requested());
    sim.shared.misbehaving(a.handle.id(), 40, "second");
    sim.pass();
    assert!(a.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
    assert!(sim.core.banlist().is_discouraged(&a.handle.addr().service.addr));
    // The flag was cleared before acting: another pass does not discourage again.
    assert_eq!(sim.shared.misbehavior(a.handle.id()).map(|m| m.1), Some(false));
}

#[test]
fn noban_peers_keep_their_connection_and_lose_the_flag() {
    let mut sim = sim();
    let p = sim.connect(1, ConnectionType::Inbound, Permissions::NO_BAN);
    sim.shared.misbehaving(p.handle.id(), 100, "");
    sim.pass();
    assert!(!p.handle.disconnect_requested());
    assert_eq!(sim.shared.misbehavior(p.handle.id()), Some((100, false)));
}

#[test]
fn discouraging_one_peer_disconnects_its_whole_address() {
    let mut sim = sim();
    let a = inbound(&sim, 1);
    // Same IP, another connection (Core's `DisconnectNode(addr)`, PQ-16).
    let a2 = inbound(&sim, 1);
    let b = inbound(&sim, 2);
    sim.shared.misbehaving(a.handle.id(), 100, "");
    sim.pass();
    assert!(a.handle.disconnect_requested());
    assert!(a2.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
}

/// Masternode connections are sent to on the first pass at or after each 100 ms boundary of
/// one global steady stamp, which resets only when the check passes.
#[test]
fn masternode_connections_are_throttled_to_100ms() {
    let mut sim = sim();
    let mn = sim.connect_masternode(9);
    let plain = inbound(&sim, 2);
    // The first pass stamps the throttle.
    sim.pass();
    sim.shared.misbehaving(mn.handle.id(), 100, "");
    sim.shared.misbehaving(plain.handle.id(), 100, "");
    sim.clock.advance(Duration::from_millis(60));
    sim.pass();
    assert!(plain.handle.disconnect_requested(), "a plain peer is sent to every pass");
    assert!(!mn.handle.disconnect_requested(), "60 ms after the stamp: skipped");
    sim.clock.advance(Duration::from_millis(40));
    sim.pass();
    assert!(mn.handle.disconnect_requested(), "100 ms after the stamp: sent");
}

// ---- the M2 stubs (P2a §3.11)

#[test]
fn unrequested_mnlistdiff_and_qrinfo_score_100() {
    let mut sim = sim();
    let a = inbound(&sim, 1);
    let b = inbound(&sim, 2);
    sim.admitted(&a, "mnlistdiff", vec![1, 2, 3]).unwrap();
    sim.admitted(&b, "qrinfo", vec![]).unwrap();
    assert_eq!(score(&sim, &a), 100);
    assert_eq!(score(&sim, &b), 100);
    sim.pass();
    assert!(a.handle.disconnect_requested() && b.handle.disconnect_requested());
}

#[test]
fn dkg_and_quorum_data_score_10_unread() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    for cmd in ["qcontrib", "qcomplaint", "qjustify", "qpcommit", "qwatch", "qgetdata", "qdata"] {
        sim.admitted(&p, cmd, vec![0xff; 3]).unwrap();
    }
    assert_eq!(score(&sim, &p), 70);
}

#[test]
fn silent_commands_neither_score_nor_reply() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // Exact: Core returns before reading in the M2 state (or has no handler at all).
    for cmd in [
        "merkleblock",
        "cfilter",
        "cfheaders",
        "cfcheckpt",
        "tx",
        "dstx",
        "getsporks",
        "govsync",
        "govobj",
        "govobjvote",
        "dsa",
        "dsi",
        "dsf",
        "dss",
        "dsc",
        "dssu",
        "dsq",
        "qsigsesann",
        "qsigsinv",
        "qgetsigs",
        "qbsigs",
        "qsigshare",
        "clsig",
        "isdlock",
        "mnauth",
        "platformban",
    ] {
        sim.admitted(&p, cmd, vec![0xee; 5]).unwrap_or_else(|t| panic!("{cmd}: {t:?}"));
    }
    // Deviations (the registry names the closer): Core reads these, and a malformed `spork` or
    // `getqrinfo` scores; the stub ignores them unread.
    for cmd in ["cmpctblock", "spork", "getmnlistd", "getqrinfo"] {
        sim.admitted(&p, cmd, vec![0xee; 5]).unwrap_or_else(|t| panic!("{cmd}: {t:?}"));
    }
    // Exact since P4a (D8): Core reads `qfcommit` first, so a short one throws its stream
    // error, unscored (llmq/blockprocessor.cpp:100-104; the `inbound-qfcommit` fixture scores a
    // whole one).
    assert!(sim.admitted(&p, "qfcommit", vec![0xee; 5]).is_err());
    // Since P5a (D20) the stub reads `qsigrec` as Core does, so a short one throws, unscored
    // (llmq/net_signing.cpp:49-51).
    assert!(sim.admitted(&p, "qsigrec", vec![0xee; 5]).is_err());
    // `block` is P3's since S1: Core's read throws, which scores nothing.
    assert!(sim.admitted(&p, "block", vec![0xee; 5]).is_err());
    assert_eq!(score(&sim, &p), 0);
    sim.pass();
    assert!(!p.handle.disconnect_requested());
    assert!(sim.sent(&p).is_empty());
}

/// P2P-12 for a commitment the node asked for (P4a D8, Sol r1 P3). Core finds the request by
/// `::SerializeHash(qc)`, the hash of the decoded commitment's re-serialization, not of the
/// payload, so any payload that decodes to the requested commitment spends the request
/// unscored: the fixture's wire bytes (crafted signatures that are not BLS points, which Core
/// re-serializes as zeros), the canonical bytes with trailing bytes, and the canonical bytes.
/// Without a request the canonical payload scores 10. The request is the entry `SendMessages`
/// records after its `getdata` (np:6876-6881), Late in `PeerConsumeGetDataResponse` with no
/// tracker at M2.
// Core: llmq/blockprocessor.cpp:100-119; bls/bls.h:100-132 (SetBytes, ToByteVector);
// net_processing.cpp:6925-6957
#[test]
fn qfcommit_answers_its_request_by_the_reserialized_hash() {
    use dash_primitives::Commitment;
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // A commitment dashd v24 decodes (the `inbound-qfcommit` fixture's peer 1).
    let fixture = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/dashd-v24/inbound-qfcommit.txt"
    ))
    .unwrap();
    let wire = fixture
        .lines()
        .find_map(|l| l.strip_prefix("> 1 qfcommit "))
        .map(dashd_p2p::sim::transcript::unhex)
        .unwrap();
    let mut canonical = Vec::new();
    Commitment::decode(&mut &wire[..]).unwrap().encode(&mut canonical);
    // Core's `CBLSWrapper` re-serializes a signature that is not a point as zeros: here
    // `membersSig`, the last 96 bytes; the rest re-serializes as read.
    let sig = wire.len() - 96;
    assert_eq!(canonical.len(), wire.len());
    assert_eq!(canonical[..sig], wire[..sig]);
    assert!(canonical[sig..].iter().all(|b| *b == 0) && wire[sig..].iter().any(|b| *b != 0));
    let inv = Inventory {
        inv_type: InvType::QuorumFinalCommitment,
        hash: dash_num::Hash256::from_lendian(dash_sha256::sha256d(&canonical)),
    };
    let mut trailing = canonical.clone();
    trailing.extend_from_slice(&[0xee; 3]);
    // Each case's score, so a mutant shows every case it breaks.
    let mut scored = Vec::new();
    for (what, payload) in
        [("wire", wire), ("trailing", trailing), ("canonical", canonical.clone())]
    {
        let before = score(&sim, &p);
        let now = sim.core.clock().node_now_micros();
        sim.shared
            .with_state_mut(p.handle.id(), |s| s.recent_object_requests.record(&inv, now))
            .unwrap();
        sim.admitted(&p, "qfcommit", payload).unwrap();
        scored.push((what, score(&sim, &p) - before));
    }
    assert_eq!(scored, [("wire", 0), ("trailing", 0), ("canonical", 0)]);
    let before = score(&sim, &p);
    // Each answer spent its request.
    sim.admitted(&p, "qfcommit", canonical).unwrap();
    assert_eq!(score(&sim, &p) - before, 10, "unrequested");
}

/// F3: before its masternode sync finishes, Core reads `ssc`'s two counts and logs them; a
/// short payload throws Core's stream error. Neither scores or replies.
// Core: node/sync_manager.cpp:330-343
#[test]
fn ssc_is_read_until_the_masternode_sync_finishes() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    let mut counts = 2i32.to_le_bytes().to_vec();
    counts.extend_from_slice(&7i32.to_le_bytes());
    sim.admitted(&p, "ssc", counts).unwrap();
    let t = sim.admitted(&p, "ssc", vec![0xee; 5]).unwrap_err();
    assert_eq!(
        t.log_line(&CommandString::from_static("ssc"), 5),
        "ProcessMessages(ssc, 5 bytes): Exception 'DataStream::read(): end of data: iostream error' (NSt8ios_base7failureB5cxx11E) caught"
    );
    assert_eq!(score(&sim, &p), 0);
    sim.pass();
    assert!(!p.handle.disconnect_requested());
    assert!(sim.sent(&p).is_empty());
}

#[test]
fn unknown_commands_are_ignored() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    sim.admitted(&p, "bogus", vec![1]).unwrap();
    sim.admitted(&p, "", vec![]).unwrap();
    assert_eq!(score(&sim, &p), 0);
    assert!(!p.handle.disconnect_requested());
}

#[test]
fn locators_over_101_disconnect() {
    let mut sim = sim();
    let a = inbound(&sim, 1);
    sim.admitted(&a, "getblocks", locator(101)).unwrap();
    sim.admitted(&a, "getheaders", locator(101)).unwrap();
    sim.admitted(&a, "getheaders2", locator(101)).unwrap();
    assert!(!a.handle.disconnect_requested());
    for (n, cmd) in [(2, "getblocks"), (3, "getheaders"), (4, "getheaders2")] {
        let p = inbound(&sim, n);
        sim.admitted(&p, cmd, locator(102)).unwrap();
        assert!(p.handle.disconnect_requested(), "{cmd}");
        assert_eq!(score(&sim, &p), 0);
    }
}

#[test]
fn compact_filter_requests_disconnect_without_the_service() {
    let mut sim = sim();
    let a = inbound(&sim, 1);
    let mut getcfilters = vec![0u8];
    getcfilters.extend_from_slice(&0u32.to_le_bytes());
    getcfilters.extend_from_slice(&[0; 32]);
    sim.admitted(&a, "getcfilters", getcfilters).unwrap();
    assert!(a.handle.disconnect_requested());
    let b = inbound(&sim, 2);
    let mut getcfcheckpt = vec![0u8];
    getcfcheckpt.extend_from_slice(&[0; 32]);
    sim.admitted(&b, "getcfcheckpt", getcfcheckpt).unwrap();
    assert!(b.handle.disconnect_requested());
    let c = inbound(&sim, 3);
    let mut getcfheaders = vec![0u8];
    getcfheaders.extend_from_slice(&0u32.to_le_bytes());
    getcfheaders.extend_from_slice(&[0; 32]);
    sim.admitted(&c, "getcfheaders", getcfheaders).unwrap();
    assert!(c.handle.disconnect_requested());
    // The read comes first: a short payload throws instead.
    let d = inbound(&sim, 4);
    assert!(sim.admitted(&d, "getcfilters", vec![0]).is_err());
    assert!(!d.handle.disconnect_requested());
}

#[test]
fn mempool_needs_bloom_or_the_permission() {
    // Without NODE_BLOOM: a plain peer is disconnected, a `noban` one kept, a `mempool` one
    // served (the flag needs a tx-relay slot, which only `version` creates).
    let mut sim = sim_without_bloom();
    let plain = inbound(&sim, 1);
    let noban = sim.connect(2, ConnectionType::Inbound, Permissions::NO_BAN);
    let mempool = sim.connect(3, ConnectionType::Inbound, Permissions::MEMPOOL);
    for p in [&plain, &noban, &mempool] {
        sim.admitted(p, "mempool", vec![]).unwrap();
    }
    assert!(plain.handle.disconnect_requested());
    assert!(!noban.handle.disconnect_requested());
    assert!(!mempool.handle.disconnect_requested());

    // With NODE_BLOOM (rdashd's default): accepted.
    let mut sim = self::sim();
    let p = inbound(&sim, 1);
    sim.admitted(&p, "mempool", vec![]).unwrap();
    assert!(!p.handle.disconnect_requested());
}

#[test]
fn block_transaction_messages_at_m2() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // getblocktxn for an unknown block: logged, no reply, no score.
    let mut req = vec![0x11; 32];
    req.extend_from_slice(&[1, 0]);
    sim.admitted(&p, "getblocktxn", req).unwrap();
    // blocktxn for a block not in flight: read, then logged.
    let mut txn = vec![0x22; 32];
    txn.push(0);
    sim.admitted(&p, "blocktxn", txn).unwrap();
    // A short read throws where Core reads.
    assert!(sim.admitted(&p, "blocktxn", vec![0x22; 31]).is_err());
    assert!(sim.sent(&p).is_empty());
    assert_eq!(score(&sim, &p), 0);
    assert!(!p.handle.disconnect_requested());
}

#[test]
fn bloom_filter_messages_disconnect_without_node_bloom() {
    let mut sim = sim_without_bloom();
    for (n, cmd) in [(1, "filterload"), (2, "filteradd"), (3, "filterclear")] {
        let p = inbound(&sim, n);
        sim.admitted(&p, cmd, vec![]).unwrap();
        assert!(p.handle.disconnect_requested(), "{cmd}");
    }
    let mut sim = self::sim();
    let p = inbound(&sim, 1);
    sim.admitted(&p, "filterclear", vec![]).unwrap();
    assert!(!p.handle.disconnect_requested());
}

// ---- size checks and ProcessGetData (P2a §3.2.3, §3.11)

#[test]
fn inv_getdata_notfound_over_50000_score_20() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    for cmd in ["inv", "getdata", "notfound"] {
        sim.admitted(&p, cmd, invs(50_000, MSG_SPORK)).unwrap();
    }
    assert_eq!(score(&sim, &p), 0);
    // Drain the notfound the 50,000-entry getdata answered.
    let _ = sim.sent_at_least(&p, 1);
    for cmd in ["inv", "getdata", "notfound"] {
        sim.admitted(&p, cmd, invs(50_001, MSG_SPORK)).unwrap();
    }
    assert_eq!(score(&sim, &p), 60);
}

#[test]
fn getdata_answers_notfound_and_serves_one_block_slot_per_call() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // spork A, tx B (skipped: no tx-relay slot and `tx` violates blocks-only), block C, spork D.
    let req = inv_bytes(&[(MSG_SPORK, 0xa), (MSG_TX, 0xb), (MSG_BLOCK, 0xc), (MSG_SPORK, 0xd)]);
    sim.admitted(&p, "getdata", req).unwrap();
    let sent = sim.sent_at_least(&p, 1);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, CommandString::NOTFOUND);
    let nf = NotFound::decode(&mut &sent[0].1[..]).unwrap();
    assert_eq!(nf.inventory.len(), 1);
    assert_eq!(nf.inventory[0].inv_type, InvType::new(MSG_SPORK));
    assert_eq!(nf.inventory[0].hash.as_bytes(), &[0xa; 32]);
    // D is still queued: the next ProcessMessages serves it before polling and reports more work.
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.getdata_requests.len()), Some(1));
    sim.pass();
    let sent = sim.sent_at_least(&p, 1);
    assert_eq!(sent.len(), 1);
    let nf = NotFound::decode(&mut &sent[0].1[..]).unwrap();
    assert_eq!(nf.inventory[0].hash.as_bytes(), &[0xd; 32]);
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.getdata_requests.len()), Some(0));
}

#[test]
fn pending_getdata_is_more_work_without_polling() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // spork A, block C1 (taken in the first call), spork D, block C2, spork E.
    let req = inv_bytes(&[
        (MSG_SPORK, 0xa),
        (MSG_BLOCK, 0xc1),
        (MSG_SPORK, 0xd),
        (MSG_BLOCK, 0xc2),
        (MSG_SPORK, 0xe),
    ]);
    sim.admitted(&p, "getdata", req).unwrap();
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.getdata_requests.len()), Some(3));
    // D is answered, C2 ends the loop and is taken; E is left, so ProcessMessages returns true
    // before polling a message.
    assert!(sim.lp.logic_mut().process_messages(&p.handle));
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.getdata_requests.len()), Some(1));
    assert!(!sim.lp.logic_mut().process_messages(&p.handle));
    let hashes: Vec<_> = sim
        .sent_at_least(&p, 3)
        .iter()
        .flat_map(|(_, payload)| NotFound::decode(&mut &payload[..]).unwrap().inventory)
        .map(|i| i.hash.as_bytes()[0])
        .collect();
    assert_eq!(hashes, [0xa, 0xd, 0xe]);
}

#[test]
fn getdata_unknown_type_ends_the_batch_silently() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    sim.admitted(&p, "getdata", inv_bytes(&[(99, 1), (MSG_SPORK, 2)])).unwrap();
    // The unknown entry is erased unanswered; the spork waits for the next call.
    assert!(sim.sent(&p).is_empty());
    sim.pass();
    let sent = sim.sent_at_least(&p, 1);
    assert_eq!(sent.len(), 1);
    let nf = NotFound::decode(&mut &sent[0].1[..]).unwrap();
    assert_eq!(
        nf.inventory,
        vec![Inventory {
            inv_type: InvType::new(MSG_SPORK),
            hash: nf.inventory[0].hash,
        }]
    );
}

// ---- negotiation branches (P2a §3.2.3)

#[test]
fn negotiation_messages_set_peer_state() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    sim.admitted(&p, "sendheaders", vec![]).unwrap();
    sim.admitted(&p, "sendheaders2", vec![]).unwrap();
    sim.admitted(&p, "sendcmpct", [vec![1], 2u64.to_le_bytes().to_vec()].concat()).unwrap();
    let st = |sim: &Sim| {
        sim.shared
            .with_state(p.handle.id(), |s| {
                (
                    s.prefer_headers,
                    s.prefer_headers_compressed,
                    s.provides_cmpctblocks,
                    s.wants_dsq,
                    s.wants_recsigs,
                )
            })
            .unwrap()
    };
    // Compact-block version 2 is ignored.
    assert_eq!(st(&sim), (true, true, false, WantsDsq::None, false));
    sim.admitted(&p, "sendcmpct", [vec![1], 1u64.to_le_bytes().to_vec()].concat()).unwrap();
    assert!(p.handle.bip152_highbandwidth_from());
    // Before `version` the common version is 209 < DSQ_INV_VERSION: every DSQ as a message.
    sim.admitted(&p, "senddsq", vec![1]).unwrap();
    sim.admitted(&p, "qsendrecsigs", vec![1]).unwrap();
    assert_eq!(st(&sim), (true, true, true, WantsDsq::All, true));
    sim.admitted(&p, "senddsq", vec![0]).unwrap();
    assert_eq!(st(&sim).3, WantsDsq::None);
}

/// Core reads `sendcmpct`'s high-bandwidth flag as any non-zero byte (`serialize.h:238-239`;
/// BSX D-11): 0x02 and 0xff are `true`, accepted, unscored and kept connected (D39).
#[test]
fn sendcmpct_reads_any_non_zero_flag_byte_as_high_bandwidth() {
    let msg = |hb: u8, version: u64| [vec![hb], version.to_le_bytes().to_vec()].concat();
    for hb in [0x02, 0xff] {
        let mut sim = sim();
        let p = inbound(&sim, 1);
        sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        sim.deliver(&p, &[("sendcmpct", msg(hb, 1))]);
        sim.pass();
        let st = sim
            .shared
            .with_state(p.handle.id(), |s| {
                (s.provides_cmpctblocks, s.headers().requested_hb_cmpctblocks)
            })
            .unwrap();
        assert_eq!(st, (true, true), "flag byte {hb:#04x}");
        assert!(p.handle.bip152_highbandwidth_from());
        assert_eq!(score(&sim, &p), 0);
        assert!(!p.handle.disconnect_requested());
    }
    // The rest of the read is base-sdk's, as Core's: trailing bytes are ignored, a version other
    // than 1 is ignored, 0x00 is low bandwidth, and a short payload throws Core's stream error.
    let mut sim = sim();
    let p = inbound(&sim, 1);
    let st = |sim: &Sim| {
        sim.shared
            .with_state(p.handle.id(), |s| {
                (s.provides_cmpctblocks, s.headers().requested_hb_cmpctblocks)
            })
            .unwrap()
    };
    sim.admitted(&p, "sendcmpct", msg(0x02, 2)).unwrap();
    assert_eq!(st(&sim), (false, false));
    sim.admitted(&p, "sendcmpct", [msg(0x02, 1), vec![0xaa]].concat()).unwrap();
    assert_eq!(st(&sim), (true, true));
    sim.admitted(&p, "sendcmpct", msg(0x00, 1)).unwrap();
    assert_eq!(st(&sim), (true, false));
    let e = sim.admitted(&p, "sendcmpct", vec![0x02, 1]).unwrap_err();
    assert!(e.what.contains("end of data"), "{}", e.what);
}

#[test]
fn inbound_getaddr_goes_to_the_address_relay() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    // P2a-net's reply; none at M2 (deviation).
    sim.admitted(&p, "getaddr", vec![]).unwrap();
    assert!(sim.sent(&p).is_empty());
}

// ---- inv (P2a §3.2.3)

#[test]
fn blocks_only_disconnects_a_tx_inv_and_ignores_an_old_isdlock_inv() {
    let opts = ProcessorOptions {
        blocks_only: true,
        ..ProcessorOptions::default()
    };
    let mut sim = sim_full(opts, Registry::new(), Seams::default(), |_| {});
    let a = inbound(&sim, 1);
    // At common version 209 (≤ 70223) an ISDLOCK inv on a tx-rejecting connection is ignored
    // (`isdlock` is not in the blocks-only set, so this half passes without the branch; the
    // half above 70223 is `a_new_peer_disconnects_for_an_isdlock_inv_on_a_block_relay_connection`).
    sim.admitted(&a, "inv", inv_bytes(&[(MSG_ISDLOCK, 1), (MSG_SPORK, 2)])).unwrap();
    assert!(!a.handle.disconnect_requested());
    sim.admitted(&a, "inv", inv_bytes(&[(MSG_SPORK, 3), (MSG_TX, 4)])).unwrap();
    assert!(a.handle.disconnect_requested());
    assert_eq!(score(&sim, &a), 0);
    // A `relay` peer is not rejected.
    let b = sim.connect(2, ConnectionType::Inbound, Permissions::RELAY);
    sim.admitted(&b, "inv", inv_bytes(&[(MSG_TX, 4)])).unwrap();
    assert!(!b.handle.disconnect_requested());
}

#[derive(Clone, Default)]
struct Responses(Arc<Mutex<Vec<(PeerId, u32)>>>);

impl ObjectRequests for Responses {
    fn received_response(&mut self, peer: PeerId, inv: &Inventory) {
        self.0.lock().unwrap().push((peer, inv.inv_type.to_base()));
    }

    fn send_section(&mut self, _batch: &mut GetDataBatch<'_>, _now_us: i64) {}

    fn finalize(&mut self, _peer: PeerId) {}
}

#[test]
fn notfound_completes_known_types_only() {
    let seen = Responses::default();
    let mut sim = with_seams(Seams {
        objects: Box::new(seen.clone()),
        ..Seams::default()
    });
    let p = inbound(&sim, 1);
    sim.admitted(&p, "notfound", inv_bytes(&[(MSG_TX, 1), (99, 2), (MSG_ISDLOCK, 3)])).unwrap();
    assert_eq!(*seen.0.lock().unwrap(), [(p.handle.id(), MSG_TX), (p.handle.id(), MSG_ISDLOCK)]);
}

// ---- SendMessages sections 12-13 (P2a §3.1.4)

/// The `getdata` frame sizes `peer` gets from one section 12-13 batch of `blocks` blocks, then
/// `objects` object requests.
fn getdata_sizes(blocks: usize, objects: usize) -> Vec<usize> {
    let sim = sim();
    let p = inbound(&sim, 1);
    let mut batch = GetDataBatch::new(&sim.core, &p.handle);
    for i in 0..blocks {
        batch.push_block(Inventory {
            inv_type: InvType::new(MSG_BLOCK),
            hash: dash_num::Hash256::from_bendian([i as u8; 32]),
        });
    }
    for i in 0..objects {
        batch.push(Inventory {
            inv_type: InvType::new(MSG_SPORK),
            hash: dash_num::Hash256::from_bendian([(i % 251) as u8; 32]),
        });
    }
    batch.finish();
    let sent = sim.sent_at_least(&p, 1);
    assert!(sent.iter().all(|(cmd, _)| *cmd == CommandString::GETDATA), "{sent:?}");
    getdata_frame_sizes(&sent)
}

fn getdata_frame_sizes(sent: &[(CommandString, Vec<u8>)]) -> Vec<usize> {
    sent.iter()
        .filter(|(cmd, _)| *cmd == CommandString::GETDATA)
        .map(|(_, payload)| GetData::decode(&mut &payload[..]).unwrap().inventory.len())
        .collect()
}

/// Core pushes the batch after the object append that makes it 1000 long, never from the block
/// section, and once more at the end if anything is left
/// (`net_processing.cpp:6872-6875, 6894-6897`). The counts are literals, not `MAX_GETDATA_SZ`,
/// so a wrong constant fails here (R1-m-2).
#[test]
fn getdata_batches_flush_at_1000_entries() {
    let cases: [(usize, usize, &[usize]); 6] = [
        (0, 999, &[999]),
        (0, 1000, &[1000]),
        (0, 1001, &[1000, 1]),
        (1, 998, &[999]),
        (1, 999, &[1000]),
        (1, 1000, &[1000, 1]),
    ];
    for (blocks, objects, want) in cases {
        assert_eq!(getdata_sizes(blocks, objects), want, "{blocks} blocks, {objects} objects");
    }
}

/// Requests `n` sporks in its first `SendMessages`.
struct RequestsOnce(usize);

impl ObjectRequests for RequestsOnce {
    fn received_response(&mut self, _peer: PeerId, _inv: &Inventory) {}

    fn send_section(&mut self, batch: &mut GetDataBatch<'_>, _now_us: i64) {
        for i in 0..std::mem::take(&mut self.0) {
            batch.push(Inventory {
                inv_type: InvType::new(MSG_SPORK),
                hash: dash_num::Hash256::from_bendian([(i % 251) as u8; 32]),
            });
        }
    }

    fn finalize(&mut self, _peer: PeerId) {}
}

#[test]
fn send_messages_sends_one_getdata_batch_per_pass() {
    let cases: [(usize, &[usize]); 3] = [(999, &[999]), (1000, &[1000]), (1001, &[1000, 1])];
    for (objects, want) in cases {
        let mut sim = with_seams(Seams {
            objects: Box::new(RequestsOnce(objects)),
            ..Seams::default()
        });
        let p = inbound(&sim, 1);
        let sent = sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        assert_eq!(getdata_frame_sizes(&sent), want, "{objects} objects");
    }
}

// ---- the exception boundary (P2a §3.2.4)

#[test]
fn a_short_read_throws_cores_line_and_scores_nothing() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    let t = sim.admitted(&p, "senddsq", vec![]).unwrap_err();
    assert_eq!(
        t.log_line(&CommandString::from_static("senddsq"), 0),
        "ProcessMessages(senddsq, 0 bytes): Exception 'DataStream::read(): end of data: iostream error' (NSt8ios_base7failureB5cxx11E) caught"
    );
    let t = sim.admitted(&p, "getdata", vec![2, 1, 0, 0, 0]).unwrap_err();
    assert!(t.what.starts_with("DataStream::read(): end of data"), "{t:?}");
    assert_eq!(score(&sim, &p), 0);
}

// ---- the registry in a live node (P2a §3.2.6)

struct Counting(Arc<AtomicUsize>);

const SPORK: [CommandString; 2] =
    [CommandString::from_static("spork"), CommandString::from_static("getsporks")];

impl NetHandler for Counting {
    fn commands(&self) -> &'static [CommandString] {
        &SPORK
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if *cmd == SPORK[0] {
            cx.misbehaving(1, "test owner");
        }
        Ok(())
    }
}

#[test]
fn a_registered_owner_replaces_the_stub() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut reg = Registry::new();
    reg.register(Box::new(Counting(calls.clone())));
    let mut sim = sim_with(reg, |_| {});
    let p = inbound(&sim, 1);
    sim.admitted(&p, "spork", vec![]).unwrap();
    sim.admitted(&p, "getsporks", vec![]).unwrap();
    sim.admitted(&p, "govobj", vec![]).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(score(&sim, &p), 1);
}

/// Scores through `Shared` from inside its handler, as a literal port of Core's
/// `m_peer_manager->PeerMisbehaving(...)` in a Dash handler would.
struct ScoresThroughShared(Arc<OnceLock<Arc<Shared>>>);

const QSIGREC: [CommandString; 1] = [CommandString::from_static("qsigrec")];

impl NetHandler for ScoresThroughShared {
    fn commands(&self) -> &'static [CommandString] {
        &QSIGREC
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        self.0.get().unwrap().misbehaving(cx.id(), 10, "from the handler");
        Ok(())
    }
}

#[test]
fn a_handler_may_score_through_shared_while_the_state_is_held() {
    let shared = Arc::new(OnceLock::new());
    let mut reg = Registry::new();
    reg.register(Box::new(ScoresThroughShared(shared.clone())));
    let mut sim = sim_with(reg, |_| {});
    shared.set(Arc::clone(&sim.shared)).ok().unwrap();
    let p = inbound(&sim, 1);
    // Before the misbehaviour cell, this deadlocked on the peer's state lock.
    sim.admitted(&p, "qsigrec", vec![]).unwrap();
    assert_eq!(score(&sim, &p), 10);
}

// ---- the thread (P2a §3.1.1)

struct CountsStops(Arc<AtomicUsize>);

impl NetHandler for CountsStops {
    fn commands(&self) -> &'static [CommandString] {
        &QSIGREC
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }

    fn process_getdata(&mut self, _cx: &mut HandlerCx<'_>, _inv: &Inventory) -> bool {
        panic!("a bug in a handler");
    }

    fn stop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_panic_stops_the_thread_and_still_stops_the_handlers() {
    let sim = sim();
    let p = inbound(&sim, 1);
    // A queued getdata reaches the handlers at the head of the next ProcessMessages.
    sim.shared.with_state_mut(p.handle.id(), |s| {
        s.getdata_requests.push_back(Inventory {
            inv_type: InvType::new(MSG_SPORK),
            hash: dash_num::Hash256::from_bendian([1; 32]),
        })
    });
    let stops = Arc::new(AtomicUsize::new(0));
    let mut reg = Registry::new();
    reg.register(Box::new(CountsStops(stops.clone())));
    let lp = MessageLoop::new(
        Arc::clone(&sim.core),
        Arc::clone(&sim.shared),
        reg,
        Seams::default(),
        FastRandom::deterministic(),
    );
    let pm = PeerManager::spawn(lp);
    until(|| sim.shared.panicked());
    assert!(sim.shared.interrupted());
    assert!(pm.stop().is_some());
    assert_eq!(stops.load(Ordering::SeqCst), 1);
}

// ---- through SB6's gates and handshake

#[test]
fn a_new_peer_disconnects_for_an_isdlock_inv_on_a_block_relay_connection() {
    // Core np:4563-4572: above 70223 the inv is a protocol violation, once InstantSend is on;
    // while it is off `AlreadyHave` holds and the inv is skipped first (instantsend.cpp:354-356).
    for instantsend_enabled in [false, true] {
        let mut sim = with_seams(Seams {
            overlay: Box::new(StaticOverlayState {
                instantsend_enabled,
                ..StaticOverlayState::default()
            }),
            ..Seams::default()
        });
        let p = sim.connect(1, ConnectionType::BlockRelay, Permissions::NONE);
        sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        sim.admitted(&p, "inv", inv_bytes(&[(MSG_ISDLOCK, 1)])).unwrap();
        assert_eq!(p.handle.disconnect_requested(), instantsend_enabled);
    }
}

#[test]
fn outbound_getaddr_is_ignored() {
    let mut sim = sim();
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let _version = sim.sent_at_least(&p, 1);
    sim.admitted(&p, "getaddr", vec![]).unwrap();
    assert!(sim.sent(&p).is_empty());
}

#[test]
fn outbound_peers_get_our_version_on_initialize() {
    let sim = sim();
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let sent = sim.sent_at_least(&p, 1);
    assert_eq!(sent[0].0, CommandString::from_static("version"));
}

#[test]
fn messages_before_version_are_dropped_by_the_gates() {
    let mut sim = sim();
    let p = inbound(&sim, 1);
    sim.deliver(&p, &[("mnlistdiff", vec![])]);
    sim.pass();
    // Gate 2 runs before the stub: no score.
    assert_eq!(score(&sim, &p), 0);
}

/// What the node sends up to its first `getheaders`, per connection type, in Core's push order
/// (np:4056-4232: `sendaddrv2`, `verack`, `getaddr` to outbound address-relay peers, the feeler
/// disconnect; then `sendheaders2`, `sendcmpct`, `senddsq` after `verack`; then `SendMessages`'
/// first `ping` and, section 6, the initial `getheaders2` to a block-serving bit-11 peer while
/// no sync has started: never to an ADDR_FETCH peer, np:6352-6372).
#[test]
fn handshake_transcripts() {
    let pre = ["sendaddrv2", "verack"];
    let post = ["sendheaders2", "sendcmpct", "senddsq", "ping", "getheaders2"];
    fn join(parts: &[&[&'static str]]) -> Vec<&'static str> {
        parts.concat()
    }
    for (n, ct, want) in [
        (1, ConnectionType::Inbound, join(&[&["version"], &pre, &post])),
        (2, ConnectionType::OutboundFullRelay, join(&[&["version"], &pre, &["getaddr"], &post])),
        (
            3,
            ConnectionType::BlockRelay,
            join(&[&["version"], &pre, &["sendheaders2", "sendcmpct", "ping", "getheaders2"]]),
        ),
        (4, ConnectionType::Manual, join(&[&["version"], &pre, &["getaddr"], &post])),
        (5, ConnectionType::Feeler, join(&[&["version"], &pre, &["getaddr"]])),
        (6, ConnectionType::AddrFetch, join(&[&["version"], &pre, &["getaddr"], &post[..4]])),
    ] {
        let mut sim = sim();
        let p = sim.connect(n, ct, Permissions::NONE);
        let mut got = sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        sim.pass();
        got.extend(sim.sent(&p));
        let cmds: Vec<_> = got.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(cmds, want, "{ct:?}");
        assert_eq!(p.handle.disconnect_requested(), ct == ConnectionType::Feeler, "{ct:?}");
        assert_eq!(p.handle.successfully_connected(), ct != ConnectionType::Feeler, "{ct:?}");
    }
}

/// SB6's rule (1d6b0b97a): a row Core's handler does not read (`ReadPlan::Nothing`) is never
/// decoded, so it never throws, whatever its payload. base-sdk's decoder would refuse some of
/// these payloads (`mnlistdiff`, `qrinfo`, `merkleblock`, the `cf*` replies).
#[test]
fn rows_that_read_nothing_never_throw() {
    let rows: Vec<_> = policy::table().iter().filter(|r| r.read == ReadPlan::Nothing).collect();
    assert!(rows.len() >= 15);
    for row in rows {
        let mut sim = sim();
        let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
        sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        let cmd = row.cmd.as_str();
        // A truncated CompactSize: every decoder that reads anything throws on it.
        assert!(sim.admitted(&p, cmd, vec![0xfd, 0x01]).is_ok(), "{cmd}");
    }
}

// ---- send-buffer backpressure and the interrupt (P2a §3.1.1)

/// A send buffer that an idle peer's transport (its empty `CSerializedNetMsg`) just fits and
/// any queued message overflows (net.cpp:1765, :4888): `fPauseSend` from a push until the peer
/// reads.
fn tight_send_buffer(cfg: &mut dash_p2p::NetConfig) {
    cfg.send_buffer_max = SERIALIZED_MSG_SIZE;
}

/// An inbound peer that reads only when told, on a [`tight_send_buffer`] node, with the
/// handshake done.
fn slow_reader(sim: &mut Sim) -> SimPeer {
    let service = Service {
        addr: NetAddr::ipv4([10, 0, 0, 1]),
        port: 19899,
    };
    let p = sim.attach_at(
        service,
        ConnectionType::Inbound,
        Permissions::NONE,
        ServiceFlags::NODE_NETWORK,
        false,
        true,
    );
    for (cmd, payload) in
        [("version", PeerVersionMsg::v24(SIM_START).payload()), ("verack", vec![])]
    {
        sim.deliver(&p, &[(cmd, payload)]);
        sim.pass();
        read_all(sim, &p);
    }
    sim.pass();
    read_all(sim, &p);
    assert!(p.handle.successfully_connected());
    p
}

/// Lets `p` read everything queued for it, which clears `fPauseSend`, and returns it.
fn read_all(sim: &Sim, p: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
    p.read(u64::MAX);
    sim.sent(p)
}

fn pongs(frames: &[(CommandString, Vec<u8>)]) -> Vec<u64> {
    frames
        .iter()
        .filter(|(c, _)| c.as_str() == "pong")
        .map(|(_, payload)| u64::from_le_bytes(payload[..8].try_into().unwrap()))
        .collect()
}

#[test]
fn a_paused_peer_is_neither_polled_nor_more_work() {
    // np:5901 returns before `PollMessage` while `fPauseSend`; net.cpp:3704 counts a peer's
    // more work only when it is not paused.
    let mut sim = sim_with(Registry::new(), tight_send_buffer);
    let p = slow_reader(&mut sim);
    let ping = |nonce: u64| ("ping", nonce.to_le_bytes().to_vec());
    sim.deliver(&p, &[ping(1), ping(2)]);
    // The first pong pauses the peer: the second ping waits and is no more work.
    assert!(!sim.pass());
    assert!(p.handle.pause_send());
    assert!(!sim.pass());
    assert!(!p.handle.recv_queue_empty());
    assert_eq!(pongs(&read_all(&sim, &p)), [1]);
    sim.pass();
    assert_eq!(pongs(&read_all(&sim, &p)), [2]);
}

#[test]
fn a_paused_peer_keeps_its_next_getdata_entry() {
    // np:2958 stops serving while `fPauseSend`; np:3082 erases the next entry only when not
    // paused, so a paused peer's request is answered once it reads.
    let mut sim = sim_with(Registry::new(), tight_send_buffer);
    let p = slow_reader(&mut sim);
    // Spork A is answered; the unknown entry ends the loop and is erased (not paused yet);
    // the `notfound` for A then pauses the peer before spork B.
    let req = inv_bytes(&[(MSG_SPORK, 0xa), (99, 0xe), (MSG_SPORK, 0xb)]);
    sim.deliver(&p, &[("getdata", req)]);
    sim.pass();
    assert!(p.handle.pause_send());
    sim.pass();
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.getdata_requests.len()), Some(1));
    let notfound = |frames: Vec<(CommandString, Vec<u8>)>| -> Vec<u8> {
        frames
            .iter()
            .filter(|(c, _)| *c == CommandString::NOTFOUND)
            .flat_map(|(_, payload)| NotFound::decode(&mut &payload[..]).unwrap().inventory)
            .map(|i| i.hash.as_bytes()[0])
            .collect()
    };
    assert_eq!(notfound(read_all(&sim, &p)), [0xa]);
    sim.pass();
    assert_eq!(notfound(read_all(&sim, &p)), [0xb]);
}

/// Raises the message handler's interrupt from `qsigrec`'s handler (`InterruptHandler`) or
/// from the first `SendMessages`' section 13 (`InterruptSend`), and logs both.
struct InterruptHandler(Arc<OnceLock<Arc<Shared>>>, Arc<Mutex<Vec<&'static str>>>);

impl NetHandler for InterruptHandler {
    fn commands(&self) -> &'static [CommandString] {
        &QSIGREC
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        self.1.lock().unwrap().push("interrupt");
        self.0.get().unwrap().interrupt();
        Ok(())
    }
}

struct InterruptSend(Arc<OnceLock<Arc<Shared>>>, Arc<Mutex<Vec<&'static str>>>);

impl ObjectRequests for InterruptSend {
    fn received_response(&mut self, _peer: PeerId, _inv: &Inventory) {}

    fn send_section(&mut self, _batch: &mut GetDataBatch<'_>, _now_us: i64) {
        self.1.lock().unwrap().push("send");
        if let Some(shared) = self.0.get() {
            shared.interrupt();
        }
    }

    fn finalize(&mut self, _peer: PeerId) {}
}

#[test]
fn an_interrupt_from_a_handler_skips_the_peers_send_messages() {
    // net.cpp:3705: the interrupt is checked between ProcessMessages and SendMessages.
    let shared = Arc::new(OnceLock::new());
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut reg = Registry::new();
    reg.register(Box::new(InterruptHandler(shared.clone(), log.clone())));
    let seams = Seams {
        objects: Box::new(InterruptSend(Arc::default(), log.clone())),
        ..Seams::default()
    };
    let mut sim = sim_full(ProcessorOptions::default(), reg, seams, |_| {});
    shared.set(Arc::clone(&sim.shared)).ok().unwrap();
    let p = inbound(&sim, 1);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    log.lock().unwrap().clear();
    sim.deliver(&p, &[("qsigrec", vec![])]);
    sim.pass();
    assert_eq!(*log.lock().unwrap(), ["interrupt"]);
}

#[test]
fn an_interrupt_from_send_messages_skips_the_next_peer() {
    // net.cpp:3712: and again after SendMessages, before the next peer.
    let shared = Arc::new(OnceLock::new());
    let seams = Seams {
        objects: Box::new(InterruptSend(shared.clone(), Arc::default())),
        ..Seams::default()
    };
    let mut sim = with_seams(seams);
    let peers = [inbound(&sim, 1), inbound(&sim, 2)];
    for p in &peers {
        sim.handshake(p, PeerVersionMsg::v24(SIM_START));
    }
    shared.set(Arc::clone(&sim.shared)).ok().unwrap();
    for p in &peers {
        sim.deliver(p, &[("ping", 1u64.to_le_bytes().to_vec())]);
    }
    sim.pass();
    // Whichever peer the shuffle put first was processed; the other was not polled.
    let waiting = peers.iter().filter(|p| !p.handle.recv_queue_empty()).count();
    assert_eq!(waiting, 1);
}

#[test]
fn a_v2_peer_whose_socket_closes_with_sends_queued_returns_what_arrived() {
    // O13a fix round 1 (BL-359): the node shuts a V2 peer's socket while messages are still
    // queued for it (the peer stopped reading; net.cpp:1694-1741 leaves them unsent), so
    // `sent_messages` returns the bytes that reached the peer instead of waiting for the queue.
    let mut sim = sim();
    let p = sim.connect_v2(1, ConnectionType::OutboundFullRelay, ServiceFlags::NODE_NETWORK);
    let first = sim.sent_messages(&p, 1);
    assert_eq!(first[0].cmd.as_str(), "version");
    p.read(0);
    sim.deliver(&p, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    assert!(!p.handle.send_queue_empty(), "the node's answers stay queued");
    sim.core.disconnect(p.handle.id(), Why::Processor("O13a closed-socket case"));
    sim.remove_disconnected();
    assert!(sim.sent_messages(&p, 0).is_empty());
}

#[test]
fn a_v1_peer_counts_the_bytes_of_a_frame_cut_short() {
    // O13a fix round 1: the node's socket closes after 10 bytes of a frame, which a Core peer
    // never reads as a message; the peer counts them, so a transcript ending here fails.
    let mut sim = sim();
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    assert_eq!(sim.sent_messages(&p, 1)[0].cmd.as_str(), "version");
    assert_eq!(p.partial_bytes(), 0);
    p.read(0);
    sim.deliver(&p, &[("version", PeerVersionMsg::v24(SIM_START).payload())]);
    sim.pass();
    p.read(10);
    sim.core.disconnect(p.handle.id(), Why::Processor("O13a cut frame"));
    sim.remove_disconnected();
    assert!(sim.sent_messages(&p, 0).is_empty());
    assert_eq!(p.partial_bytes(), 10);
}
