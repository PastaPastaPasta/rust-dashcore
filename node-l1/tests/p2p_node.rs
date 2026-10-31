//! The node leg of `diff_p2p_multi_*` (P2a-core): hand scenarios run on Core's peer session and
//! on the node's processor must agree step for step, and each scenario checks that Core did what
//! it is there to exercise, so an agreement is not a vacuous one.

use dash_consensus::Header;
use dash_consensus::header::{HeaderParams, check_block_header};
use dash_num::Hash256;
use dash_oracle_sys::{
    ConnType, NetAddr, Oracle, Pin, SendMode, Service, Substitution, TICK_INACTIVITY,
    TICK_MAINTENANCE,
};
use dash_p2p_core::{GetHeaders, Headers, InvType, Inventory, NotFound, ProtocolVersion};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::sim::{PeerVersionMsg, SimChain};
use node_l1::p2p_node::{
    PeerSpec, Report, Session, Step, addr_send_timer, compare_session, report_session,
};
use node_l1::{Outcome, TARGETS};

/// Regtest genesis time plus a day, as SB6's scripts start.
const T0: i64 = 1_417_713_337 + 86_400;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

fn peer(conn: ConnType, i: u8) -> PeerSpec {
    PeerSpec {
        conn,
        addr: Service::new(NetAddr::ipv4([1, 2, 3, i]), 19899),
        send_mode: SendMode::Eager,
    }
}

fn session(peers: Vec<PeerSpec>, steps: Vec<Step>) -> Session {
    Session {
        seed: 7,
        our_services: 1 | 4 | 1024,
        mocktime: T0,
        check_block_index: true,
        peers,
        steps,
        blocks: Vec::new(),
        max_tip_age: None,
        addr_relay: Some(addr_send_timer),
    }
}

fn deliver(peer: usize, command: &'static str, payload: Vec<u8>) -> Step {
    Step::Deliver {
        peer,
        command,
        payload,
        subs: Vec::new(),
    }
}

/// A v24 peer's `version` with protocol version `version`.
fn version(version: i32) -> Vec<u8> {
    PeerVersionMsg {
        version,
        ..PeerVersionMsg::v24(T0)
    }
    .payload()
}

/// The peer's side of a handshake at `T0`.
fn handshake(peer: usize) -> [Step; 4] {
    handshake_with(peer, version(70242))
}

/// The peer's side of a handshake at `T0`, with `version` as its `version` payload.
fn handshake_with(peer: usize, version: Vec<u8>) -> [Step; 4] {
    [
        deliver(peer, "version", version),
        Step::Pass(T0),
        deliver(peer, "verack", Vec::new()),
        Step::Pass(T0),
    ]
}

/// Runs `s` on both legs and fails on the first difference.
fn agree(s: &Session) -> Report {
    let oracle = Oracle::new(Pin::V24_0);
    let report = report_session(&oracle, s).expect("both legs run").expect("in domain");
    if let Some(d) = &report.difference {
        panic!("the legs differ at {d}");
    }
    assert_eq!(compare_session(&oracle, s), Ok(Outcome::BothAccept));
    report
}

/// The commands peer `peer` received over every compared step.
fn received(r: &Report, peer: usize) -> Vec<String> {
    r.received.iter().flatten().flat_map(|step| step[peer].clone()).collect()
}

fn headers(hs: &[BlockHeader]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers {
        headers: hs.to_vec(),
    }
    .encode(&mut buf);
    buf
}

#[test]
fn inbound_handshakes_and_ping() {
    let mut steps: Vec<Step> = [handshake(0), handshake(1)].concat();
    // Past the ping interval, then each peer answers with the nonce of our ping.
    steps.push(Step::Pass(T0 + 121));
    steps.extend((0..2).map(pong));
    steps.push(Step::Pass(T0 + 122));
    let r = agree(&session(vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)], steps));
    for p in 0..2 {
        let got = received(&r, p);
        for cmd in ["version", "verack", "ping"] {
            assert!(got.iter().any(|c| c == cmd), "peer {p} got no {cmd}: {got:?}");
        }
    }
}

#[test]
fn outbound_handshakes() {
    let mut steps = vec![Step::Pass(T0)];
    steps.extend([handshake(0), handshake(1)].concat());
    let r = agree(&session(
        vec![peer(ConnType::OutboundFullRelay, 0), peer(ConnType::BlockRelay, 1)],
        steps,
    ));
    // Core greets an outbound peer first, and asks the full-relay one for addresses.
    assert_eq!(r.received[0].as_ref().unwrap()[0].first().map(String::as_str), Some("version"));
    assert!(received(&r, 0).iter().any(|c| c == "getaddr"));
    assert!(!received(&r, 1).iter().any(|c| c == "getaddr"));
}

/// Two outbound peers finish their handshakes in one pass: only the first in the pass's shuffle
/// order starts headers sync (`getheaders2`: `nSyncStarted` is 0 only for it), so the node must
/// shuffle as Core does. Over these seeds each peer comes first at least once.
#[test]
fn one_pass_shuffles_peers_as_core_does() {
    let base = session(
        vec![peer(ConnType::OutboundFullRelay, 0), peer(ConnType::OutboundFullRelay, 1)],
        vec![
            Step::Pass(T0),
            deliver(0, "version", version(70242)),
            deliver(1, "version", version(70242)),
            Step::Pass(T0),
            deliver(0, "verack", Vec::new()),
            deliver(1, "verack", Vec::new()),
            Step::Pass(T0),
        ],
    );
    let mut first = [false; 2];
    for seed in 0..8 {
        let r = agree(&Session {
            seed,
            ..base.clone()
        });
        // Sync starts only after `verack`, so in the last pass.
        let syncing: Vec<usize> = (0..2)
            .filter(|&p| received(&r, p).iter().any(|c| c.starts_with("getheaders")))
            .collect();
        assert_eq!(syncing.len(), 1, "seed {seed}: {:?}", r.received);
        first[syncing[0]] = true;
    }
    assert_eq!(first, [true, true], "the seeds never put one of the peers first");
}

#[test]
fn obsolete_peer_is_removed_and_its_messages_lost() {
    let steps = vec![
        deliver(0, "version", version(70_000)),
        Step::Pass(T0),
        deliver(0, "verack", Vec::new()),
        Step::Pass(T0),
    ];
    let r = agree(&session(vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)], steps));
    // The deliver to the removed peer is skipped on both sides.
    assert!(r.received[2].is_none(), "{:?}", r.received);
}

#[test]
fn header_chain_and_invalid_headers() {
    let params = regtest();
    let good = SimChain::mine(&params, 12, 150);
    // A header whose parent nobody knows, and one with too little work for its bits.
    let mut orphan = good[11];
    orphan.prev_hash = BlockHash::from([0x11; 32]);
    let hp = HeaderParams::from_chain_params(&params).unwrap();
    let weak = (0..)
        .map(|nonce| BlockHeader {
            nonce,
            ..good[0]
        })
        .find(|h| check_block_header(&Header::new(*h), &hp, true).is_err())
        .unwrap();
    let mut steps = vec![Step::Pass(T0)];
    steps.extend(handshake(0));
    steps.extend(handshake(1));
    steps.extend([
        deliver(1, "headers", headers(&[orphan])),
        Step::Pass(T0 + 1),
        deliver(1, "headers", headers(&[weak])),
        Step::Pass(T0 + 2),
        deliver(0, "headers", headers(&good[..8])),
        Step::Pass(T0 + 3),
        deliver(0, "headers", headers(&good[8..])),
        Step::Pass(T0 + 4),
    ]);
    let n = steps.len();
    let r = agree(&session(
        vec![peer(ConnType::OutboundFullRelay, 0), peer(ConnType::OutboundFullRelay, 1)],
        steps,
    ));
    // Core drops the peer for the weak header, accepts the first good batch and ends the
    // comparison there: it starts downloading the batch's blocks (P3's).
    assert_eq!(r.removed, [false, true]);
    assert_eq!(r.compared, n - 3);
}

/// Peer `peer`'s `pong` of the node's (and Core's) last `ping` to it.
fn pong(peer: usize) -> Step {
    Step::Deliver {
        peer,
        command: "pong",
        payload: vec![0; 8],
        subs: vec![Substitution {
            dst_offset: 0,
            src_peer: peer as i64,
            src_command: "ping".into(),
            src_offset: 0,
            len: 8,
        }],
    }
}

/// R1-m-1's full-section pass, in the session (DEC-56: every section runs on every pass). After
/// three outbound peers have handshaken and answered their first ping at `T0`, the pass at
/// `T0 + 16 min` finds work in several sections at once: section 4 (a ping is due for every
/// peer), section 5 (the address send timer), section 8 (the transaction trickle timer), and
/// section 10 (the sync peer's headers download timed out with other peers left, so it is
/// dropped and the rest of its pass is skipped). The next pass starts headers sync with another
/// peer (section 6) and arms its chain-sync timeout (section 11). The legs agree on each peer's
/// outbound sequence, its state and the draws. Sections 7, 8's block `inv`, 9, 12 and 13 need a
/// new tip, block download or object requests, which no session step can make; `sim_m2`'s
/// twin pass covers them.
#[test]
fn one_pass_with_work_pending_in_several_sections() {
    let peers: Vec<PeerSpec> = (0..3).map(|i| peer(ConnType::OutboundFullRelay, i)).collect();
    let mut steps = vec![Step::Pass(T0)];
    for p in 0..3 {
        steps.extend(handshake(p));
    }
    steps.extend((0..3).map(pong));
    steps.push(Step::Pass(T0));
    let t1 = T0 + 16 * 60;
    steps.extend([Step::Pass(t1), Step::Pass(t1 + 1)]);
    let n = steps.len();
    let r = agree(&session(peers, steps));
    assert_eq!(r.compared, n);
    let step = |i: usize| r.received[i].as_ref().expect("a pass runs");
    // Every peer gets its due ping in the timeout pass, the dropped sync peer too (section 4
    // runs before section 10).
    for p in 0..3 {
        assert!(step(n - 2)[p].iter().any(|c| c == "ping"), "peer {p}: {:?}", step(n - 2)[p]);
    }
    assert_eq!(r.removed.iter().filter(|&&gone| gone).count(), 1, "{:?}", r.removed);
    let restarted = (0..3)
        .filter(|&p| !r.removed[p])
        .any(|p| step(n - 1)[p].iter().any(|c| c.starts_with("getheaders")));
    assert!(restarted, "no peer took over headers sync: {:?}", step(n - 1));
}

/// An `inv` of one block, `hash` (`MSG_BLOCK` = 2).
fn block_inv(hash: &BlockHash) -> Vec<u8> {
    let mut buf = vec![1];
    buf.extend(2u32.to_le_bytes());
    buf.extend(hash.as_bytes());
    buf
}

/// R1-M-1's cross-peer +C probe (P2a §3.12, §7.2 E). Peer 0 sends a valid header H, and peer 1,
/// in the same pass, sends `second` (a header on H, or a block `inv(H)`). Core's shuffle decides
/// which of the two is processed first. Over 16 seeds both orders occur, and the legs agree on
/// each. If peer 1 goes first, H is unknown, so Core asks peer 1 for headers (and counts an
/// unconnecting header for a `headers` message); if peer 0 goes first, peer 1 is asked nothing.
/// The peers are inbound and advertise no `NODE_NETWORK`, so Core keeps them and never asks
/// them for a block (P3's), and the comparison runs to the end.
fn cross_peer_probe(h: BlockHeader, second: (&'static str, Vec<u8>)) {
    let bloom_only = PeerVersionMsg {
        services: 4,
        ..PeerVersionMsg::v24(T0)
    }
    .payload();
    let mut orders = [false; 2];
    for seed in 1..=16 {
        let mut steps = Vec::new();
        for p in 0..2 {
            steps.extend(handshake_with(p, bloom_only.clone()));
        }
        steps.extend([
            deliver(0, "headers", headers(&[h])),
            deliver(1, second.0, second.1.clone()),
            Step::Pass(T0 + 1),
            Step::Pass(T0 + 2),
        ]);
        let n = steps.len();
        let mut s = session(vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)], steps);
        s.seed = seed;
        let r = agree(&s);
        assert_eq!(r.compared, n, "seed {seed}: the comparison ends early");
        assert_eq!(r.removed, [false, false], "seed {seed}");
        let asked = r.received[n - 2].as_ref().expect("the probe pass ran")[1]
            .iter()
            .any(|c| c.starts_with("getheaders"));
        orders[usize::from(asked)] = true;
    }
    assert_eq!(orders, [true, true], "both shuffle orders occur");
}

#[test]
fn a_header_on_another_peers_header_in_the_same_pass() {
    let hs = SimChain::mine(&regtest(), 2, 150);
    cross_peer_probe(hs[0], ("headers", headers(&hs[1..])));
}

#[test]
fn a_block_inv_of_another_peers_header_in_the_same_pass() {
    let h = SimChain::mine(&regtest(), 1, 150)[0];
    cross_peer_probe(h, ("inv", block_inv(&Header::new(h).hash())));
}

/// Runs `s` on both legs and returns their first difference; fails if they agree.
fn differ(s: &Session) -> String {
    let oracle = Oracle::new(Pin::V24_0);
    let report = report_session(&oracle, s).expect("both legs run").expect("in domain");
    report.difference.expect("the legs agree")
}

/// An `addr` of one routable address, 8.8.8.8:9999 with `NODE_NETWORK`, stamped `time`.
fn one_addr(time: u32) -> Vec<u8> {
    let mut buf = vec![1];
    buf.extend(time.to_le_bytes());
    buf.extend(1u64.to_le_bytes());
    buf.extend([0; 10]);
    buf.extend([0xff, 0xff, 8, 8, 8, 8]);
    buf.extend(9999u16.to_be_bytes());
    buf
}

/// The stand-in relay (D62) cannot hide relay (validator M1 on R1-M-2): Core answers a
/// `getaddr`, and forwards a fresh address from an inbound peer to an outbound peer's queue,
/// while `addr_send_timer` does neither, so both sessions differ until P2a-net's relay is
/// installed. When it is, these become `agree` scenarios.
#[test]
fn the_send_timer_stand_in_differs_where_core_relays() {
    let getaddr = [handshake(0).to_vec(), vec![deliver(0, "getaddr", Vec::new()), Step::Pass(T0)]];
    let d = differ(&session(vec![peer(ConnType::Inbound, 0)], getaddr.concat()));
    let (core, node) = d.split_once("\nnode ").expect("both sides");
    assert!(core.contains("getaddr_recvd: true") && node.contains("getaddr_recvd: false"), "{d}");
    let peers = vec![
        peer(ConnType::Inbound, 0),
        peer(ConnType::OutboundFullRelay, 1),
        peer(ConnType::OutboundFullRelay, 2),
    ];
    let mut steps = vec![Step::Pass(T0)];
    for p in 0..3 {
        steps.extend(handshake(p));
    }
    steps.extend([deliver(0, "addr", one_addr(T0 as u32 + 1)), Step::Pass(T0)]);
    let d = differ(&session(peers, steps));
    let (core, node) = d.split_once("\nnode ").expect("both sides");
    let queued = format!("addrs_to_send: [({}, 1, 1, [8, 8, 8, 8], 9999)]", T0 + 1);
    assert!(core.starts_with("step 14") && core.contains(&queued), "{d}");
    assert!(node.contains("addrs_to_send: []"), "{d}");
}

#[test]
fn maintenance_tick_with_a_stale_tip_and_extra_outbound_peers() {
    let peers: Vec<PeerSpec> = (0..10).map(|i| peer(ConnType::OutboundFullRelay, i)).collect();
    let mut steps = vec![Step::Pass(T0)];
    for p in 0..peers.len() {
        steps.extend(handshake(p));
    }
    for t in [T0 + 46, T0 + 30 * 60, T0 + 31 * 60, T0 + 2 * 60 * 60] {
        steps.push(Step::Tick(t, TICK_MAINTENANCE));
        steps.push(Step::Pass(t));
    }
    let r = agree(&session(peers, steps));
    assert!(r.removed.iter().any(|&gone| gone), "Core evicted no peer");
}

#[test]
fn a_peer_that_reads_when_told() {
    let mut p = peer(ConnType::Inbound, 0);
    p.send_mode = SendMode::Controlled;
    let steps = vec![
        deliver(0, "version", version(70242)),
        Step::Pass(T0),
        Step::Drain {
            peer: 0,
            max: 50,
        },
        Step::Drain {
            peer: 0,
            max: 1000,
        },
        deliver(0, "verack", Vec::new()),
        Step::Pass(T0),
        Step::Drain {
            peer: 0,
            max: 1 << 20,
        },
    ];
    let r = agree(&session(vec![p, peer(ConnType::Inbound, 1)], steps));
    // Nothing until the first drain, the version only once a whole frame is read.
    assert_eq!(r.received[1].as_ref().unwrap()[0], Vec::<String>::new());
    assert!(received(&r, 0).iter().any(|c| c == "verack"));
}

#[test]
fn the_comparison_ends_at_the_first_inactivity_tick() {
    let mut steps = handshake(0).to_vec();
    steps.push(Step::Tick(T0 + 60, TICK_INACTIVITY));
    steps.push(Step::Pass(T0 + 60));
    let r = agree(&session(vec![peer(ConnType::Inbound, 0)], steps));
    assert_eq!(r.compared, 4);
}

/// An `inv` of one entry of type `kind`.
fn one_inv(kind: u32, fill: u8) -> Vec<u8> {
    let mut buf = vec![1];
    buf.extend(kind.to_le_bytes());
    buf.extend([fill; 32]);
    buf
}

#[test]
fn a_transaction_inv_resets_the_rejects_filter_once_per_tip() {
    // Core's `AlreadyHave` redraws `m_recent_rejects`' tweak for the first transaction-type
    // entry at a new tip (D69): a governance object draws nothing, the `tx` draws, and the
    // `dstx` and asset unlock at the same tip do not; each pass also fires the trickle timer.
    let mut steps = handshake(0).to_vec();
    for (i, kind) in [17, 1, 16, 34].into_iter().enumerate() {
        steps.push(deliver(0, "inv", one_inv(kind, i as u8)));
        steps.push(Step::Pass(T0 + 60 * (i as i64 + 1)));
    }
    let r = agree(&session(vec![peer(ConnType::Inbound, 0)], steps));
    assert_eq!(r.compared, 12);
    assert!(!r.removed[0]);
}

/// Deterministic pseudo-random inputs (splitmix64), as SB6's `p2p.rs` test.
fn inputs() -> impl Iterator<Item = Vec<u8>> {
    (0u64..6).map(|seed| {
        let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (0..24 + 24 * seed as usize)
            .map(|_| {
                x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                let mut z = x;
                z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                (z ^ (z >> 31)) as u8
            })
            .collect()
    })
}

#[test]
fn single_command_targets_agree_on_scripts() {
    for name in [
        "diff_p2p_headers",
        "diff_p2p_headers2",
        "diff_p2p_inv",
        "diff_p2p_getdata",
        "diff_p2p_notfound",
    ] {
        let target = TARGETS.iter().find(|t| t.name == name).expect("registered");
        assert_eq!((target.compare)(&[], Pin::V24_0), Ok(Outcome::OutOfDomain), "{name}");
        for input in inputs() {
            assert_eq!((target.compare)(&input, Pin::V24_0), Ok(Outcome::BothAccept), "{name}");
        }
    }
}

/// `n` regtest blocks on genesis (at most 16: each coinbase pushes its height as `OP_n`), each
/// paying nothing to `OP_TRUE`, with a nonce Core's header check accepts; and their headers.
fn blocks(params: &ChainSetup, n: u8) -> (Vec<Vec<u8>>, Vec<BlockHeader>) {
    let hp = HeaderParams::from_chain_params(params).unwrap();
    let genesis = Header::new(dash_consensus::params::genesis_block(params).header);
    let (mut prev, mut time) = (genesis.hash(), genesis.time());
    let (mut blocks, mut headers) = (Vec::new(), Vec::new());
    for height in 1..=n {
        let mut tx = vec![1, 0, 0, 0, 1];
        tx.extend([0; 32]);
        tx.extend([0xff; 4]);
        tx.extend([2, 0x50 + height, 0x00]);
        tx.extend([0xff; 4]);
        tx.push(1);
        tx.extend(0u64.to_le_bytes());
        tx.extend([1, 0x51]);
        tx.extend([0; 4]);
        time += 150;
        let root = dash_consensus::hash::sha256d(&tx);
        let mut h = BlockHeader {
            version: 0x2000_0000,
            prev_hash: prev,
            merkle_root: MerkleRoot::decode(&mut &root[..]).unwrap(),
            time,
            bits: genesis.bits(),
            nonce: 0,
        };
        while check_block_header(&Header::new(h), &hp, true).is_err() {
            h.nonce += 1;
        }
        prev = Header::new(h).hash();
        let mut block = Vec::new();
        h.encode(&mut block);
        block.push(1);
        block.extend(&tx);
        blocks.push(block);
        headers.push(h);
    }
    (blocks, headers)
}

/// A `getheaders`/`getheaders2` payload.
fn locator(hashes: &[BlockHash], stop: BlockHash) -> Vec<u8> {
    let mut buf = Vec::new();
    GetHeaders {
        version: ProtocolVersion(70242),
        locator_hashes: hashes.to_vec(),
        hash_stop: stop,
    }
    .encode(&mut buf);
    buf
}

/// P2b: `getheaders`/`getheaders2` serving (net_processing.cpp:4760-4855) from a 16-block active
/// chain to inbound peers with and without bit 11, at 70242 and 70234: genesis and mid-chain
/// locators, unknown and stale entries, `hashStop`, null locators (known, unknown stop), a
/// locator at the tip, and the 102-entry disconnect. Core's replies must be the node's, byte
/// for byte, step by step.
#[test]
fn inbound_getheaders_served_as_core_does() {
    let params = regtest();
    let (chain, hs) = blocks(&params, 16);
    let hash = |i: usize| Header::new(hs[i - 1]).hash();
    let genesis = Header::new(dash_consensus::params::genesis_block(&params).header).hash();
    let unknown = BlockHash::from([0x11; 32]);
    let zero = BlockHash::from([0; 32]);
    let peer_version = |version, services| {
        PeerVersionMsg {
            version,
            services,
            ..PeerVersionMsg::v24(T0)
        }
        .payload()
    };
    let mut steps = Vec::new();
    steps.extend(handshake_with(0, peer_version(70242, 1 | 4 | 0x800)));
    steps.extend(handshake_with(1, peer_version(70234, 1 | 4)));
    let requests = [
        (0, "getheaders", locator(&[genesis], zero)),
        (0, "getheaders2", locator(&[genesis], zero)),
        (1, "getheaders2", locator(&[genesis], zero)),
        (1, "getheaders", locator(&[unknown, hash(9), hash(4)], zero)),
        (0, "getheaders2", locator(&[hash(3)], hash(7))),
        (0, "getheaders", locator(&[unknown], zero)),
        (1, "getheaders", locator(&[], hash(5))),
        (0, "getheaders2", locator(&[], genesis)),
        (1, "getheaders", locator(&[], unknown)),
        (0, "getheaders", locator(&[hash(16)], zero)),
        (1, "getheaders2", locator(&[genesis; 102], zero)),
    ];
    let n_requests = requests.len();
    for (i, (p, cmd, payload)) in requests.into_iter().enumerate() {
        steps.push(deliver(p, cmd, payload));
        steps.push(Step::Pass(T0 + 1 + i as i64));
    }
    let n = steps.len();
    let r = agree(&Session {
        blocks: chain,
        ..session(vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)], steps)
    });
    assert_eq!(r.compared, n, "every step compared");
    // Core served each request but the unknown null-locator stop, and dropped peer 1 for the
    // oversized locator.
    let replies = |p| received(&r, p).into_iter().filter(|c| c.starts_with("headers")).count();
    assert_eq!(replies(0) + replies(1), n_requests - 2);
    assert_eq!(r.removed, [false, true]);
}

/// A `notfound` of `entries` (inv type, 32-byte hash).
fn notfound_of(entries: impl IntoIterator<Item = (u32, [u8; 32])>) -> Vec<u8> {
    let inventory = entries
        .into_iter()
        .map(|(kind, hash)| Inventory {
            inv_type: InvType::from_base(kind),
            hash: Hash256::from_lendian(hash),
        })
        .collect();
    let mut buf = Vec::new();
    NotFound {
        inventory,
    }
    .encode(&mut buf);
    buf
}

/// A `notfound` of `n` distinct entries of inv type `kind`.
fn notfound(n: u32, kind: u32) -> Vec<u8> {
    notfound_of((0..n).map(|i| {
        let mut hash = [0; 32];
        hash[..4].copy_from_slice(&i.to_le_bytes());
        (kind, hash)
    }))
}

#[test]
fn notfound_limit_is_v24s() {
    // P4a D9: at v24, up to `MAX_INV_SZ` (50,000) entries are read and each known-type entry
    // completes our request (none at M2); 50,001 cost +20 (`notfound message size = 50001`),
    // and the fifth such message discourages the peer. Known and unknown types are mixed below
    // the limit.
    let mixed = notfound_of([(1, [1; 32]), (2, [2; 32]), (99, [3; 32]), (29, [4; 32])]);
    let mut steps = handshake(0).to_vec();
    steps.extend(handshake(1));
    for (peer, payload) in [(0, notfound(50_000, 1)), (0, mixed), (1, notfound(50_001, 29))] {
        steps.push(deliver(peer, "notfound", payload));
        steps.push(Step::Pass(T0));
    }
    let r = agree(&session(
        vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)],
        steps.clone(),
    ));
    assert_eq!(r.removed, [false, false]);
    for _ in 0..4 {
        steps.push(deliver(1, "notfound", notfound(50_001, 1)));
        steps.push(Step::Pass(T0));
    }
    let r = agree(&session(vec![peer(ConnType::Inbound, 0), peer(ConnType::Inbound, 1)], steps));
    assert_eq!(r.removed, [false, true]);
}
