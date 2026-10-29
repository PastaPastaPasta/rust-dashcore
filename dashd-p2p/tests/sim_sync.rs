//! P2a-core phase E in `sim`: headers sync (P2a §3.4), block announcements (§3.4.8), outbound
//! eviction and the stale tip (§3.9), and `getpeerinfo`'s state stats. Every peer completes
//! SB6's handshake, so `SendMessages` runs all its sections.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_consensus::{BlockId, BlockValidationError};
use dash_p2p::conn::Permissions;
use dash_p2p::dial::DialPolicy;
use dash_p2p::rng::{FastRandom, RandSource, exponential_rand, get_rand};
use dash_p2p::{
    CommandString, ConnectionType, NetAddr, NetConfig, Network, Service, ServiceFlags, Why,
};
use dash_p2p_core::{GetHeaders, Headers, Headers2, Inv, InvType, Inventory};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Numeric};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peerman::RecentBlock;
use dashd_p2p::processing::seams::{
    BlockCheckedCx, BlockSync, ChainAccess, EntryFacts, GetDataBatch, ObjectRequests, SyncCx,
    TreeRead, Work,
};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

fn sim() -> Sim {
    sim_full(Seams::default(), |_| {})
}

fn sim_full(seams: Seams, hook: impl FnOnce(&mut NetConfig)) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, ProcessorOptions::default(), Registry::new(), seams, hook)
}

/// Bit 11 (`NODE_HEADERS_COMPRESSED`).
const BIT11: u64 = 0x800;

/// A v24 peer advertising `services`.
fn v24(services: u64) -> PeerVersionMsg {
    PeerVersionMsg {
        services,
        ..PeerVersionMsg::v24(SIM_START)
    }
}

/// Connects peer `n` as `ct`, completes the handshake and one more pass, and returns what the
/// node sent.
fn ready(
    sim: &mut Sim,
    n: u8,
    ct: ConnectionType,
    perms: Permissions,
    v: PeerVersionMsg,
) -> (SimPeer, Vec<String>) {
    let p = sim.connect(n, ct, perms);
    let mut got = sim.handshake(&p, v);
    sim.pass();
    got.extend(sim.sent(&p));
    let cmds = got.iter().map(|(c, _)| c.as_str().to_owned()).collect();
    (p, cmds)
}

fn outbound(sim: &mut Sim, n: u8) -> (SimPeer, Vec<String>) {
    ready(
        sim,
        n,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        PeerVersionMsg::v24(SIM_START),
    )
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.misbehavior(p.handle.id()).unwrap().0
}

fn cmds(frames: &[(CommandString, Vec<u8>)]) -> Vec<&str> {
    frames.iter().map(|(c, _)| c.as_str()).collect()
}

/// Delivers `cmd` with `payload` from `p`, runs a pass, and returns what the node sent.
fn send(sim: &mut Sim, p: &SimPeer, cmd: &str, payload: Vec<u8>) -> Vec<(CommandString, Vec<u8>)> {
    sim.deliver(p, &[(cmd, payload)]);
    sim.pass();
    sim.sent(p)
}

fn headers_msg(headers: &[BlockHeader]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers {
        headers: headers.to_vec(),
    }
    .encode(&mut buf);
    buf
}

fn headers2_msg(headers: &[BlockHeader]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers2::reply(headers).encode(&mut buf);
    buf
}

fn getheaders(payload: &[u8]) -> GetHeaders {
    GetHeaders::decode(&mut &payload[..]).expect("a getheaders payload")
}

fn hash_of(h: &BlockHeader) -> BlockHash {
    dash_consensus::Header::new(*h).hash()
}

/// An entry that is not in the chain, to build unconnecting headers from.
fn orphan_base(sim: &Sim) -> EntryFacts {
    let g = sim.chain.genesis();
    let lost = SimChain::build(&g, 1, 150, 0xee)[0];
    EntryFacts {
        id: BlockId(u32::MAX),
        hash: hash_of(&lost),
        prev: None,
        height: 1,
        time: lost.time,
        chain_work: Work::ONE,
        header: lost,
    }
}

fn advance(sim: &mut Sim, secs: u64) {
    sim.clock.advance(Duration::from_secs(secs));
    sim.pass();
}

// ---- sync start (section 6) and `MaybeSendGetHeaders`

#[test]
fn the_initial_getheaders2_goes_to_one_peer_from_the_best_header() {
    let mut sim = sim();
    let (a, got) = outbound(&mut sim, 1);
    assert_eq!(got.last().map(String::as_str), Some("getheaders2"));
    let (_b, got) = outbound(&mut sim, 2);
    // The best header is years old and one sync has started: no second sync peer.
    assert!(!got.iter().any(|c| c.starts_with("getheaders")), "{got:?}");
    assert_eq!(sim.lp.logic().counters.sync_started, 1);
    let st = sim.shared.with_state(a.handle.id(), |s| (s.sync_started, s.headers_sync_timeout_us));
    let (started, timeout) = st.unwrap();
    assert!(started);
    // 15 min + 1 ms per 150 s the best header is behind (multiplied before dividing).
    let behind = SIM_START - i64::from(sim.chain.genesis().time);
    assert_eq!(timeout, SIM_START * 1_000_000 + 15 * 60 * 1_000_000 + 1000 * behind / 150);
}

#[test]
fn the_getheaders_locator_carries_the_common_version_and_a_null_stop() {
    let mut sim = sim();
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let mut got = sim.handshake(&p, v24(0x1 | 0x4 | 0x400));
    sim.pass();
    got.extend(sim.sent(&p));
    // Without bit 11 the type is `getheaders`.
    let (cmd, payload) = got.last().unwrap();
    assert_eq!(cmd.as_str(), "getheaders");
    let m = getheaders(payload);
    assert_eq!(m.version.0, 70242);
    assert_eq!(m.locator_hashes, vec![sim.chain.genesis().hash]);
    assert_eq!(m.hash_stop, BlockHash::ZERO);
}

#[test]
fn a_peer_that_cannot_serve_blocks_gets_no_getheaders() {
    let mut sim = sim();
    let (_p, got) =
        ready(&mut sim, 1, ConnectionType::OutboundFullRelay, Permissions::NONE, v24(0x4 | BIT11));
    assert!(!got.iter().any(|c| c.starts_with("getheaders")), "{got:?}");
}

#[test]
fn an_answered_getheaders_rearms_and_the_throttle_holds_otherwise() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    // A block inv while syncing asks again only after a reply or 2 minutes.
    let inv = inv_of(&[hash_of(&SimChain::build(&g, 1, 150, 1)[0])]);
    assert!(send(&mut sim, &p, "inv", inv.clone()).is_empty());
    // Any `headers` (here empty) clears the stamp.
    assert!(send(&mut sim, &p, "headers", vec![0]).is_empty());
    assert_eq!(cmds(&send(&mut sim, &p, "inv", inv)), ["getheaders2"]);
}

fn inv_of(hashes: &[BlockHash]) -> Vec<u8> {
    let inventory = hashes
        .iter()
        .map(|h| Inventory {
            inv_type: InvType::Block,
            hash: dash_num::Hash256::from_lendian(*h.as_bytes()),
        })
        .collect();
    let mut buf = Vec::new();
    Inv {
        inventory,
    }
    .encode(&mut buf);
    buf
}

// ---- processing `headers`/`headers2` (P2a §3.4.3)

#[test]
fn a_full_batch_is_accepted_and_more_is_requested_by_the_peers_bit_11() {
    let mut sim = sim();
    // No bit 11: the limit is 2000.
    let (p, _) = ready(
        &mut sim,
        1,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        v24(0x1 | 0x4 | 0x400),
    );
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, 2000, 150, 1);
    let got = send(&mut sim, &p, "headers", headers_msg(&hs));
    assert_eq!(sim.chain.batches(), vec![(p.handle.id(), 2000)]);
    assert_eq!(cmds(&got), ["getheaders"]);
    let m = getheaders(&got[0].1);
    assert_eq!(m.locator_hashes[0], hash_of(&hs[1999]));
    let best = sim.shared.with_state(p.handle.id(), |s| s.headers().best_known_block).unwrap();
    assert_eq!(best, sim.chain.by_hash(&hash_of(&hs[1999])).map(|e| e.id));
    let stats = sim.shared.node_state_stats(&p.handle).unwrap();
    assert_eq!(stats.sync_height, 2000);
    assert_eq!(stats.starting_height, 0);
}

#[test]
fn a_bit_11_peer_answering_with_2000_plain_headers_gets_no_more_request() {
    // PQ-24: the "more" rule uses the limit for the peer's bit 11 (8000 at ≥ 70235).
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, 2000, 150, 1);
    assert!(send(&mut sim, &p, "headers", headers_msg(&hs)).is_empty());
    assert_eq!(sim.chain.batches().len(), 1);
}

#[test]
fn headers2_decodes_against_the_message_and_is_accepted() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, 10, 150, 2);
    assert!(send(&mut sim, &p, "headers2", headers2_msg(&hs)).is_empty());
    assert!(sim.chain.by_hash(&hash_of(&hs[9])).is_some());
    assert_eq!(score(&sim, &p), 0);
}

#[test]
fn oversized_counts_score_20_by_the_message_type_received() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let count = |n: u64| {
        let mut b = Vec::new();
        CompactSize::from(n).encode(&mut b);
        b
    };
    send(&mut sim, &p, "headers", count(2001));
    assert_eq!(score(&sim, &p), 20);
    // `headers2` at common ≥ 70235 allows 8000.
    send(&mut sim, &p, "headers2", count(8000));
    assert_eq!(score(&sim, &p), 20);
    send(&mut sim, &p, "headers2", count(8001));
    assert_eq!(score(&sim, &p), 40);
    assert!(sim.chain.batches().is_empty());
}

#[test]
fn few_unconnecting_headers_ask_for_the_gap_and_score_every_tenth() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let base = orphan_base(&sim);
    // np:3162: 20 at every multiple of MAX_UNCONNECTING_HEADERS (10), not at every count past it.
    for i in 1..=20u8 {
        let hs = SimChain::build(&base, 1, 150, i);
        let got = send(&mut sim, &p, "headers", headers_msg(&hs));
        // The receipt cleared the stamp, so the getheaders goes out each time.
        assert_eq!(cmds(&got), ["getheaders2"], "round {i}");
        let unconnecting =
            sim.shared.with_state(p.handle.id(), |s| s.unconnecting_headers).unwrap();
        assert_eq!(unconnecting, i32::from(i));
        assert_eq!(score(&sim, &p), 20 * i32::from(i / 10), "round {i}");
    }
    // The last unknown hash is remembered for block availability.
    let last = sim.shared.with_state(p.handle.id(), |s| s.headers().last_unknown_block).unwrap();
    assert!(last.is_some());
    assert!(sim.chain.batches().is_empty());
}

#[test]
fn many_unconnecting_headers_score_10_and_ask_nothing() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let hs = SimChain::build(&orphan_base(&sim), 9, 150, 1);
    assert!(send(&mut sim, &p, "headers", headers_msg(&hs)).is_empty());
    assert_eq!(score(&sim, &p), 10);
}

#[test]
fn eight_unconnecting_headers_are_still_few() {
    // np:3340: up to MAX_BLOCKS_TO_ANNOUNCE (8) is a possible announcement.
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let hs = SimChain::build(&orphan_base(&sim), 8, 150, 1);
    assert_eq!(cmds(&send(&mut sim, &p, "headers", headers_msg(&hs))), ["getheaders2"]);
    assert_eq!(score(&sim, &p), 0);
}

#[test]
fn a_non_continuous_batch_scores_20() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    let mut hs = SimChain::build(&g, 3, 150, 1);
    hs[2].prev_hash = BlockHash::ZERO;
    send(&mut sim, &p, "headers", headers_msg(&hs));
    assert_eq!(score(&sim, &p), 20);
    assert!(sim.chain.batches().is_empty());
}

#[test]
fn an_invalid_header_is_punished_through_maybe_punish_node_for_block() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, 3, 150, 1);
    sim.chain.mark_invalid(hash_of(&hs[1]));
    send(&mut sim, &p, "headers", headers_msg(&hs));
    // BLOCK_INVALID_HEADER: 100, and the discouragement acts in the same pass.
    assert!(p.handle.disconnect_requested());
}

#[test]
fn an_insufficient_work_chain_disconnects_an_outbound_peer_in_ibd() {
    let mut sim = sim();
    sim.chain.set_min_chain_work(Work::from_u64(1_000_000));
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    send(&mut sim, &p, "headers", headers_msg(&SimChain::build(&g, 5, 150, 1)));
    assert!(p.handle.disconnect_requested());
}

#[test]
fn an_outbound_peer_with_our_tips_work_is_protected() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    send(&mut sim, &p, "headers", headers_msg(&SimChain::build(&g, 5, 150, 1)));
    let protect = sim.shared.with_state(p.handle.id(), |s| s.chain_sync.protect).unwrap();
    assert!(protect);
    assert_eq!(sim.lp.logic().counters.outbound_with_protect, 1);
}

fn protected(sim: &Sim, p: &SimPeer) -> bool {
    sim.shared.with_state(p.handle.id(), |s| s.chain_sync.protect).unwrap()
}

#[test]
fn a_header_with_exactly_our_tips_work_protects_but_is_no_announcement() {
    // np:3314 protects at `>=` the tip's work; np:3283 stamps the announcement only on more.
    let mut sim = sim();
    let g = sim.chain.genesis();
    let tip = sim.chain.add(&SimChain::build(&g, 1, 150, 1));
    sim.chain.set_tip(tip.id);
    let (p, _) = outbound(&mut sim, 1);
    // A sibling of our tip, then its child.
    let fork = SimChain::build(&g, 2, 150, 2);
    send(&mut sim, &p, "headers", headers_msg(&fork[..1]));
    let sibling = sim.chain.by_hash(&hash_of(&fork[0])).unwrap();
    assert_eq!(sibling.chain_work, tip.chain_work);
    assert!(protected(&sim, &p));
    let announced =
        |sim: &Sim| sim.shared.with_state(p.handle.id(), |s| s.last_block_announcement).unwrap();
    assert_eq!(announced(&sim), 0);
    send(&mut sim, &p, "headers", headers_msg(&fork[1..]));
    assert_eq!(announced(&sim), SIM_START);
}

#[test]
fn a_disconnected_protected_peer_frees_its_slot() {
    // np:1821: FinalizeNode gives the slot back; MAX_OUTBOUND_PEERS_TO_PROTECT_FROM_DISCONNECT
    // is 4 (np:130).
    let mut sim = sim();
    let hs = SimChain::build(&sim.chain.genesis(), 5, 150, 1);
    let mut four = Vec::new();
    for n in 1..=4 {
        let (p, _) = outbound(&mut sim, n);
        send(&mut sim, &p, "headers", headers_msg(&hs));
        assert!(protected(&sim, &p));
        four.push(p);
    }
    let (fifth, _) = outbound(&mut sim, 5);
    send(&mut sim, &fifth, "headers", headers_msg(&hs));
    assert!(!protected(&sim, &fifth));
    for p in &four {
        sim.core.disconnect(p.handle.id(), Why::Processor("test"));
    }
    sim.remove_disconnected();
    assert_eq!(sim.lp.logic().counters.outbound_with_protect, 0);
    send(&mut sim, &fifth, "headers", headers_msg(&hs));
    assert!(protected(&sim, &fifth));
}

/// F4: a chain side that shuts down while V holds the batch answers no `HeadersResult`. The
/// message ends quietly (no panic in a debug build, no score, no reply), and the loop goes on
/// until the host interrupts it.
#[test]
fn headers_while_the_chain_shuts_down_end_quietly() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    sim.chain.set_shutting_down(true);
    let msg = headers_msg(&SimChain::build(&sim.chain.genesis(), 5, 150, 1));
    let sent = send(&mut sim, &p, "headers", msg.clone());
    assert!(sent.is_empty(), "{sent:?}");
    assert!(!sim.shared.panicked());
    assert_eq!(sim.shared.misbehavior(p.handle.id()), Some((0, false)));
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.headers().best_known_block), Some(None));
    sim.chain.set_shutting_down(false);
    send(&mut sim, &p, "headers", msg);
    assert!(
        sim.shared.with_state(p.handle.id(), |s| s.headers().best_known_block).unwrap().is_some()
    );
}

#[test]
fn loading_blocks_drops_headers_unread() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    sim.chain.set_loading_blocks(true);
    // An oversized count is never read, so it is never scored.
    let mut b = Vec::new();
    CompactSize::from(5000u64).encode(&mut b);
    send(&mut sim, &p, "headers", b);
    assert_eq!(score(&sim, &p), 0);
}

// ---- the `inv` block path (P2a §3.4.7)

#[test]
fn a_block_inv_triggers_getheaders_on_one_new_peer_per_block() {
    let mut sim = sim();
    // The first peer syncs; the others have not started.
    let (_a, _) = outbound(&mut sim, 1);
    let (b, _) = outbound(&mut sim, 2);
    let (c, _) = outbound(&mut sim, 3);
    let g = sim.chain.genesis();
    let new = hash_of(&SimChain::build(&g, 1, 150, 9)[0]);
    assert_eq!(cmds(&send(&mut sim, &b, "inv", inv_of(&[new]))), ["getheaders2"]);
    // The same block from another non-syncing peer: no second trigger.
    assert!(send(&mut sim, &c, "inv", inv_of(&[new])).is_empty());
    // A peer that triggered before sync does not trigger again for a new block.
    let newer = hash_of(&SimChain::build(&g, 1, 150, 10)[0]);
    assert!(send(&mut sim, &b, "inv", inv_of(&[newer])).is_empty());
    assert_eq!(cmds(&send(&mut sim, &c, "inv", inv_of(&[newer]))), ["getheaders2"]);
    // A known block triggers nothing.
    let (d, _) = outbound(&mut sim, 4);
    assert!(send(&mut sim, &d, "inv", inv_of(&[g.hash])).is_empty());
}

#[test]
fn a_syncing_peers_inv_leaves_the_pre_sync_trigger_alone() {
    // np:4600: only a peer that has not started sync sets the latch and the last block.
    let mut sim = sim();
    let (a, _) = outbound(&mut sim, 1);
    let (b, _) = outbound(&mut sim, 2);
    assert!(sim.shared.with_state(a.handle.id(), |s| s.sync_started).unwrap());
    let new = hash_of(&SimChain::build(&sim.chain.genesis(), 1, 150, 9)[0]);
    send(&mut sim, &a, "inv", inv_of(&[new]));
    let latched = sim.shared.with_state(a.handle.id(), |s| s.inv_triggered_getheaders_before_sync);
    assert_eq!(latched, Some(false));
    assert_eq!(cmds(&send(&mut sim, &b, "inv", inv_of(&[new]))), ["getheaders2"]);
}

// ---- the cross-peer +C probes (P2a §3.12, §7.2 E; R1-M-1)

/// What peer B was sent in the probe's pass, B's `nUnconnectingHeaders`, and whether the
/// chain then knows `probe_hash`.
struct ProbeOutcome {
    to_b: Vec<String>,
    unconnecting: i32,
    knows_probe: bool,
}

/// Peer A sends valid header `h` and peer B, in the same pass, `second`. The pass shuffles the
/// two peers with `seed`. Both are inbound and advertise no `NODE_NETWORK`, as the session leg's
/// twin (`node-l1/tests/p2p_node.rs`): nothing starts headers sync or asks for a block.
fn probe(seed: u8, h: BlockHeader, second: (&str, Vec<u8>), probe_hash: BlockHash) -> ProbeOutcome {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let chain_params = Arc::clone(&p);
    let max_tip_age = ProcessorOptions::default().max_tip_age;
    let mut sim = Sim::with_chain(
        p,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        SIM_START,
        |clock| SimChain::validating(chain_params, clock.clone(), max_tip_age),
        |_| {},
    );
    let [a, b] = [1, 2].map(|n| {
        let (peer, cmds) = ready(&mut sim, n, ConnectionType::Inbound, Permissions::NONE, v24(4));
        assert!(!cmds.iter().any(|c| c.starts_with("getheaders")), "no sync start");
        peer
    });
    sim.deliver(&a, &[("headers", headers_msg(&[h]))]);
    sim.deliver(&b, &[second]);
    sim.pass_with(&mut FastRandom::with_seed([seed; 32]));
    assert!(sim.sent(&a).is_empty());
    ProbeOutcome {
        to_b: cmds(&sim.sent(&b)).into_iter().map(str::to_owned).collect(),
        unconnecting: sim.shared.with_state(b.handle.id(), |s| s.unconnecting_headers).unwrap(),
        knows_probe: sim.chain.by_hash(&probe_hash).is_some(),
    }
}

#[test]
fn a_header_on_another_peers_header_is_unconnecting_only_when_processed_first() {
    let hs = SimChain::mine(&params(), 2, 150);
    let mut orders = [false; 2];
    for seed in 0..16 {
        let o = probe(seed, hs[0], ("headers", headers_msg(&hs[1..])), hash_of(&hs[1]));
        let b_first = !o.to_b.is_empty();
        if b_first {
            // H is unknown: B is asked for the gap and its header is not stored.
            assert_eq!(o.to_b, ["getheaders"], "seed {seed}");
            assert_eq!((o.unconnecting, o.knows_probe), (1, false), "seed {seed}");
        } else {
            assert_eq!((o.unconnecting, o.knows_probe), (0, true), "seed {seed}");
        }
        orders[usize::from(b_first)] = true;
    }
    assert_eq!(orders, [true, true], "both shuffle orders occur");
}

#[test]
fn a_block_inv_of_another_peers_header_asks_only_when_processed_first() {
    let h = SimChain::mine(&params(), 1, 150)[0];
    let mut orders = [false; 2];
    for seed in 0..16 {
        let o = probe(seed, h, ("inv", inv_of(&[hash_of(&h)])), hash_of(&h));
        let b_first = !o.to_b.is_empty();
        if b_first {
            // An unknown block: B is asked for headers (the inv path's `getheaders`).
            assert_eq!(o.to_b, ["getheaders"], "seed {seed}");
        }
        assert_eq!((o.unconnecting, o.knows_probe), (0, true), "seed {seed}");
        orders[usize::from(b_first)] = true;
    }
    assert_eq!(orders, [true, true], "both shuffle orders occur");
}

// ---- the headers-sync timeout (section 10)

#[test]
fn a_stalled_sync_peer_is_dropped_when_another_could_serve() {
    let mut sim = sim();
    // Manual peers are preferred for download and never subject to `ConsiderEviction`.
    let v = PeerVersionMsg::v24(SIM_START);
    let (a, _) = ready(&mut sim, 1, ConnectionType::Manual, Permissions::NONE, v);
    let (b, _) = ready(&mut sim, 2, ConnectionType::Manual, Permissions::NONE, v);
    let timeout = sim.shared.with_state(a.handle.id(), |s| s.headers_sync_timeout_us).unwrap();
    let secs = (timeout / 1_000_000 - SIM_START) as u64;
    advance(&mut sim, secs);
    assert!(!a.handle.disconnect_requested());
    advance(&mut sim, 1);
    assert!(a.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
}

#[test]
fn a_stalled_sync_peer_is_kept_when_no_other_could_serve() {
    // np:6786: another preferred-download peer (`m_num_preferred_download_peers -
    // fPreferredDownload >= 1`) is needed; an inbound peer is not one.
    let mut sim = sim();
    let v = PeerVersionMsg::v24(SIM_START);
    let (a, _) = ready(&mut sim, 1, ConnectionType::Manual, Permissions::NONE, v);
    let (_b, _) = ready(&mut sim, 2, ConnectionType::Inbound, Permissions::NONE, v);
    assert_eq!(sim.lp.logic().counters.num_preferred_download, 1);
    let timeout = sim.shared.with_state(a.handle.id(), |s| s.headers_sync_timeout_us).unwrap();
    advance(&mut sim, (timeout / 1_000_000 - SIM_START) as u64 + 1);
    assert!(!a.handle.disconnect_requested());
    assert!(sim.shared.with_state(a.handle.id(), |s| s.sync_started).unwrap());
}

#[test]
fn a_stalled_noban_sync_peer_is_kept_and_its_sync_reset() {
    let mut sim = sim();
    let v = PeerVersionMsg::v24(SIM_START);
    let (a, _) = ready(&mut sim, 1, ConnectionType::Manual, Permissions::NO_BAN, v);
    let (_b, _) = ready(&mut sim, 2, ConnectionType::Manual, Permissions::NONE, v);
    let timeout = sim.shared.with_state(a.handle.id(), |s| s.headers_sync_timeout_us).unwrap();
    advance(&mut sim, (timeout / 1_000_000 - SIM_START) as u64 + 1);
    assert!(!a.handle.disconnect_requested());
    let reset =
        sim.shared.with_state(a.handle.id(), |s| (s.sync_started, s.headers_sync_timeout_us));
    assert_eq!(reset, Some((false, 0)));
    // Whichever peer the next pass visits first starts a new sync.
    sim.pass();
    assert_eq!(sim.lp.logic().counters.sync_started, 1);
}

// ---- `ConsiderEviction` (section 11, P2a §3.9.1; G2)

#[test]
fn a_behind_outbound_peer_is_asked_once_from_the_tips_parent_then_dropped() {
    let mut sim = sim();
    // Our tip has more work than anything the peer announced.
    let g = sim.chain.genesis();
    let tip = sim.chain.add(&SimChain::build(&g, 3, 150, 1));
    sim.chain.set_tip(tip.id);
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    // The timeout was armed at the first pass after sync start, 20 minutes ahead.
    advance(&mut sim, 20 * 60);
    assert!(sim.sent(&p).iter().all(|(c, _)| !c.as_str().starts_with("getheaders")));
    advance(&mut sim, 1);
    let got = sim.sent(&p);
    let (_, payload) =
        got.iter().find(|(c, _)| c.as_str() == "getheaders2").expect("verify chain work");
    // The work header is the tip; its parent's locator.
    let m = getheaders(payload);
    let parent = sim.chain.entry(tip.prev.unwrap()).unwrap();
    assert_eq!(m.locator_hashes[0], parent.hash);
    advance(&mut sim, 2 * 60);
    assert!(!p.handle.disconnect_requested());
    advance(&mut sim, 1);
    assert!(p.handle.disconnect_requested());
}

#[test]
fn a_genesis_only_node_verifies_chain_work_with_an_empty_locator() {
    // G2: the work header is genesis, whose parent is null: v24 sends no locator entries.
    let mut sim = sim();
    let p = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    advance(&mut sim, 20 * 60 + 1);
    let got = sim.sent(&p);
    let (_, payload) =
        got.iter().find(|(c, _)| c.as_str() == "getheaders2").expect("verify chain work");
    assert!(getheaders(payload).locator_hashes.is_empty());
}

#[test]
fn a_sync_peer_at_exactly_our_tips_work_is_never_asked_to_verify() {
    // np:5962: best known work `>=` the tip's clears the chain-sync timeout. Four other peers
    // hold the protection slots, so the sync peer is not protected (np:3314).
    let mut sim = sim();
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, 3, 150, 1);
    let tip = sim.chain.add(&hs);
    sim.chain.set_tip(tip.id);
    let (p, _) = outbound(&mut sim, 1);
    assert!(sim.shared.with_state(p.handle.id(), |s| s.sync_started).unwrap());
    let chain_sync = |sim: &Sim| {
        sim.shared.with_state(p.handle.id(), |s| (s.chain_sync.timeout, s.chain_sync.work_header))
    };
    // Behind our tip: the timeout is armed against it.
    assert_eq!(chain_sync(&sim), Some((SIM_START + 20 * 60, Some(tip.id))));
    for n in 2..=5 {
        let (q, _) = outbound(&mut sim, n);
        send(&mut sim, &q, "headers", headers_msg(&hs));
    }
    send(&mut sim, &p, "headers", headers_msg(&hs));
    assert!(!protected(&sim, &p));
    let best = sim.shared.with_state(p.handle.id(), |s| s.headers().best_known_block).unwrap();
    assert_eq!(best, Some(tip.id));
    // Caught up to exactly the tip's work: cleared (np:5963-5966), not re-armed by the second
    // branch, which would keep it from ever firing all the same.
    assert_eq!(chain_sync(&sim), Some((0, None)));
    advance(&mut sim, 20 * 60 + 1);
    assert!(sim.sent(&p).iter().all(|(c, _)| !c.as_str().starts_with("getheaders")));
    advance(&mut sim, 2 * 60 + 1);
    assert!(!p.handle.disconnect_requested());
}

// ---- extra outbound peers and the stale tip (P2a §3.9.2–3)

#[test]
fn the_stale_tip_check_asks_for_an_extra_outbound_peer() {
    let mut sim = sim();
    // The first pass arms the 45 s scheduler (Core schedules it at `StartScheduledTasks`).
    sim.pass();
    let flags = sim.shared.dial_policy();
    // The first check, at 45 s, initialises the tip time; the tip is stale once 3 spacings
    // (450 s) older, looked at only every 150 s: checks at 45, 225, 405 and 585 s.
    let mut t = 0;
    while !flags.try_another_outbound_peer() {
        advance(&mut sim, 45);
        t += 45;
        assert!(t <= 600, "no stale tip by {t} s");
    }
    assert_eq!(t, 585);
    // The first check found direct fetch impossible (the tip is years old).
    assert!(!DialPolicy::start_extra_block_relay_peers(&*flags));
}

#[test]
fn mockscheduler_moves_only_the_scheduled_check() {
    let mut sim = sim();
    sim.pass();
    // Mock time moves 600 s; the steady clock does not, so the 45 s check is not due.
    sim.clock.set_mock_time(SIM_START + 600);
    sim.pass();
    assert_eq!(sim.lp.logic().stale_tip_check_time(), 0);
    sim.shared.mock_scheduler(Duration::from_secs(44));
    sim.pass();
    assert_eq!(sim.lp.logic().stale_tip_check_time(), 0);
    // 45 s in all: the check runs at the next pass, at the mock time.
    sim.shared.mock_scheduler(Duration::from_secs(1));
    sim.pass();
    assert_eq!(sim.lp.logic().stale_tip_check_time(), SIM_START + 600 + 150);
    // And re-arms 45 s of steady time later.
    sim.clock.set_mock_time(SIM_START + 800);
    sim.pass();
    assert_eq!(sim.lp.logic().stale_tip_check_time(), SIM_START + 600 + 150);
}

/// Runs `CheckForStaleTipAndEvictPeers` at mock time `SIM_START + t`.
fn check_at(sim: &mut Sim, t: i64) {
    sim.clock.set_mock_time(SIM_START + t);
    sim.shared.mock_scheduler(Duration::from_secs(45));
    sim.pass();
}

#[test]
fn an_extra_full_relay_peer_is_evicted_after_30_seconds() {
    let mut sim = sim_full(Seams::default(), |cfg| cfg.max_outbound_full_relay = 1);
    let (a, _) = outbound(&mut sim, 1);
    let (b, _) = outbound(&mut sim, 2);
    // Connected more than 30 s (`>`).
    check_at(&mut sim, 30);
    assert!(!a.handle.disconnect_requested() && !b.handle.disconnect_requested());
    check_at(&mut sim, 31);
    // Both announced nothing: the tie goes to the newer connection.
    assert!(!a.handle.disconnect_requested());
    assert!(b.handle.disconnect_requested());
}

#[test]
fn a_protected_peer_is_not_the_extra_one() {
    let mut sim = sim_full(Seams::default(), |cfg| cfg.max_outbound_full_relay = 1);
    let (a, _) = outbound(&mut sim, 1);
    let (b, _) = outbound(&mut sim, 2);
    let g = sim.chain.genesis();
    send(&mut sim, &b, "headers", headers_msg(&SimChain::build(&g, 2, 150, 1)));
    advance(&mut sim, 75);
    assert!(a.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
}

#[test]
fn onion_peers_up_to_the_onion_target_are_kept() {
    // np:6071: `++onion_count <= GetMaxOutboundOnionNodeCount()` keeps the first onion peers.
    let mut sim = sim_full(Seams::default(), |cfg| {
        cfg.max_outbound_full_relay = 1;
        cfg.max_outbound_onion = 1;
    });
    let onion = |sim: &mut Sim, tag: u8| {
        let service = Service {
            addr: NetAddr::new(Network::Onion, vec![tag; 32]).unwrap(),
            port: 9999,
        };
        let p = sim.attach_at(
            service,
            ConnectionType::OutboundFullRelay,
            Permissions::NONE,
            ServiceFlags::NODE_NETWORK,
            false,
            false,
        );
        sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
        sim.pass();
        p
    };
    let (first, second) = (onion(&mut sim, 7), onion(&mut sim, 8));
    // The first onion peer announced least recently, so only the onion count keeps it.
    sim.shared.with_state_mut(second.handle.id(), |s| s.last_block_announcement = SIM_START);
    check_at(&mut sim, 31);
    assert!(!first.handle.disconnect_requested());
    assert!(second.handle.disconnect_requested());
}

#[test]
fn the_youngest_extra_block_relay_peer_goes_unless_it_gave_a_block() {
    let mut sim = sim_full(Seams::default(), |cfg| cfg.max_outbound_block_relay = 1);
    let v = PeerVersionMsg::v24(SIM_START);
    let (a, _) = ready(&mut sim, 1, ConnectionType::BlockRelay, Permissions::NONE, v);
    let (b, _) = ready(&mut sim, 2, ConnectionType::BlockRelay, Permissions::NONE, v);
    b.handle.set_last_block_time(SIM_START);
    // Connected at least 30 s (`>=`, unlike full-relay peers).
    check_at(&mut sim, 29);
    assert!(!a.handle.disconnect_requested());
    check_at(&mut sim, 30);
    assert!(a.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
}

// ---- block announcements (section 7, P2a §3.4.8)

/// Extends the chain by `n`, connects the tip and calls `UpdatedBlockTip` outside IBD.
fn new_tip(sim: &Sim, n: usize) -> Vec<BlockHeader> {
    let g = sim.chain.genesis();
    let hs = SimChain::build(&g, n, 150, 7);
    let tip = sim.chain.add(&hs);
    sim.chain.set_tip(tip.id);
    sim.shared.updated_block_tip(&sim.core, &sim.chain.new_blocks(tip.id, Some(g.id)), false);
    hs
}

fn knows_genesis(sim: &Sim, p: &SimPeer) {
    let g = sim.chain.genesis().id;
    sim.shared.with_state_mut(p.handle.id(), |s| s.headers_mut().best_known_block = Some(g));
}

#[test]
fn a_sendheaders2_peer_gets_new_blocks_as_compressed_announcements() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    send(&mut sim, &p, "sendheaders2", vec![]);
    knows_genesis(&sim, &p);
    let hs = new_tip(&sim, 2);
    sim.pass();
    let got = sim.sent(&p);
    assert_eq!(cmds(&got), ["headers2"]);
    let mut want = Vec::new();
    Headers2::announce(&hs).encode(&mut want);
    assert_eq!(got[0].1, want);
    let sent = sim.shared.with_state(p.handle.id(), |s| s.headers().best_header_sent).unwrap();
    assert_eq!(sent, sim.chain.by_hash(&hash_of(&hs[1])).map(|e| e.id));
    // Announced once.
    sim.pass();
    assert!(sim.sent(&p).is_empty());
}

#[test]
fn a_sendheaders_peer_gets_plain_headers() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    send(&mut sim, &p, "sendheaders", vec![]);
    knows_genesis(&sim, &p);
    let hs = new_tip(&sim, 1);
    sim.pass();
    let got = sim.sent(&p);
    assert_eq!(got, vec![(CommandString::HEADERS, headers_msg(&hs))]);
}

#[test]
fn a_peer_without_headers_preference_gets_an_inv_of_the_tip() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let hs = new_tip(&sim, 3);
    sim.pass();
    let got = sim.sent(&p);
    assert_eq!(got, vec![(CommandString::INV, inv_of(&[hash_of(&hs[2])]))]);
}

#[test]
fn a_gap_reverts_to_an_inv() {
    // The peer knows neither the first new header nor its parent.
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    send(&mut sim, &p, "sendheaders2", vec![]);
    let hs = new_tip(&sim, 2);
    sim.pass();
    assert_eq!(sim.sent(&p), vec![(CommandString::INV, inv_of(&[hash_of(&hs[1])]))]);
}

#[test]
fn ibd_announces_nothing() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let g = sim.chain.genesis();
    let tip = sim.chain.add(&SimChain::build(&g, 1, 150, 7));
    sim.shared.updated_block_tip(&sim.core, &sim.chain.new_blocks(tip.id, Some(g.id)), true);
    sim.pass();
    assert!(sim.sent(&p).is_empty());
}

/// `sendcmpct(high bandwidth, version 1)`.
fn sendcmpct_hb_v1() -> Vec<u8> {
    [vec![1u8], 1u64.to_le_bytes().to_vec()].concat()
}

/// Records `push_compact_announcement` calls.
#[derive(Default)]
struct CompactRecorder(Arc<Mutex<Vec<BlockId>>>);

impl BlockSync for CompactRecorder {
    fn is_block_requested(&self, _hash: &BlockHash) -> bool {
        false
    }
    fn any_in_flight(&self) -> bool {
        false
    }
    fn blocks_in_flight(&self, _peer: i64) -> usize {
        0
    }
    fn stall_section(&mut self, _cx: &mut SyncCx<'_>, _now_us: i64) -> bool {
        false
    }
    fn getdata_section(
        &mut self,
        _cx: &mut SyncCx<'_>,
        _b: &mut GetDataBatch<'_>,
        _sync: bool,
        _now_us: i64,
    ) {
    }
    fn finalize(&mut self, _peer: i64) {}
    fn push_compact_announcement(
        &mut self,
        _cx: &mut SyncCx<'_>,
        block: &EntryFacts,
        _recent: &RecentBlock,
    ) {
        self.0.lock().unwrap().push(block.id);
    }
}

#[test]
fn a_high_bandwidth_peer_gets_one_new_block_as_a_compact_block() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seams = Seams {
        block_sync: Box::new(CompactRecorder(Arc::clone(&calls))),
        ..Seams::default()
    };
    let mut sim = sim_full(seams, |_| {});
    let (p, _) = outbound(&mut sim, 1);
    send(&mut sim, &p, "sendcmpct", sendcmpct_hb_v1());
    knows_genesis(&sim, &p);
    let hs = new_tip(&sim, 1);
    sim.pass();
    assert!(sim.sent(&p).is_empty());
    let want = sim.chain.by_hash(&hash_of(&hs[0])).unwrap().id;
    assert_eq!(*calls.lock().unwrap(), vec![want]);
}

#[test]
fn two_new_blocks_revert_a_high_bandwidth_peer_to_an_inv() {
    // np:6410-6413: a compact-block peer without a headers preference reverts above one
    // block, even when it knows the first and only one header would go out.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seams = Seams {
        block_sync: Box::new(CompactRecorder(Arc::clone(&calls))),
        ..Seams::default()
    };
    let mut sim = sim_full(seams, |_| {});
    let (p, _) = outbound(&mut sim, 1);
    send(&mut sim, &p, "sendcmpct", sendcmpct_hb_v1());
    let hs = new_tip(&sim, 2);
    let first = sim.chain.by_hash(&hash_of(&hs[0])).unwrap().id;
    sim.shared.with_state_mut(p.handle.id(), |s| s.headers_mut().best_known_block = Some(first));
    sim.pass();
    assert_eq!(sim.sent(&p), vec![(CommandString::INV, inv_of(&[hash_of(&hs[1])]))]);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn updated_block_tip_skips_a_masternode_connection() {
    // np:2297: `if (!pnode->CanRelay()) return;` before queueing the new blocks.
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    // An outbound masternode connection (net.cpp:3639); an inbound one is set by `version`.
    let service = Service {
        addr: NetAddr::ipv4([10, 0, 0, 2]),
        port: 19899,
    };
    let m = sim.attach_at(
        service,
        ConnectionType::OutboundFullRelay,
        Permissions::NONE,
        ServiceFlags::NODE_NETWORK,
        true,
        false,
    );
    sim.handshake(&m, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    let hs = new_tip(&sim, 1);
    let queued = |q: &SimPeer| {
        sim.shared.with_state(q.handle.id(), |s| s.blocks_for_relay().1.as_slice().to_vec())
    };
    assert_eq!(queued(&p), Some(vec![hash_of(&hs[0])]));
    assert_eq!(queued(&m), Some(vec![]));
}

/// P3's shape (D65): section 12 moves `pindexLastCommonBlock` to the best known block, as
/// `FindNextBlocksToDownload` does, and `BlockChecked` punishes the block's source. `checked`
/// logs each signal's `ibd` argument with the published view's IBD at delivery.
struct LastCommon {
    source: Arc<AtomicI64>,
    checked: Checked,
}

/// Each delivered `BlockChecked`'s `(ibd argument, published view's IBD)`.
type Checked = Arc<Mutex<Vec<(bool, bool)>>>;

impl LastCommon {
    fn new(source: &Arc<AtomicI64>) -> (Self, Checked) {
        let checked = Arc::default();
        let sync = Self {
            source: Arc::clone(source),
            checked: Arc::clone(&checked),
        };
        (sync, checked)
    }
}

impl BlockSync for LastCommon {
    fn is_block_requested(&self, _hash: &BlockHash) -> bool {
        false
    }
    fn any_in_flight(&self) -> bool {
        false
    }
    fn blocks_in_flight(&self, _peer: i64) -> usize {
        0
    }
    fn stall_section(&mut self, _cx: &mut SyncCx<'_>, _now_us: i64) -> bool {
        false
    }
    fn getdata_section(
        &mut self,
        cx: &mut SyncCx<'_>,
        _b: &mut GetDataBatch<'_>,
        _sync: bool,
        _now_us: i64,
    ) {
        cx.state.last_common_block = cx.state.headers().best_known_block;
    }
    fn block_checked(
        &mut self,
        cx: &mut BlockCheckedCx<'_>,
        _hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
    ) {
        let source = self.source.load(Ordering::SeqCst);
        let view = cx.with_peer(source, |p| {
            p.chain.is_initial_block_download(p.snap, p.core.clock().node_now())
        });
        self.checked.lock().unwrap().push((ibd, view.unwrap()));
        if let Some(state) = invalid {
            cx.with_peer(source, |p| {
                p.maybe_punish_node_for_block(state.result(), false, "bad block")
            });
        }
    }
    fn finalize(&mut self, _peer: i64) {}
}

#[test]
fn block_sync_hooks_write_the_peer_state() {
    let source = Arc::new(AtomicI64::new(-1));
    let (sync, _) = LastCommon::new(&source);
    let seams = Seams {
        block_sync: Box::new(sync),
        ..Seams::default()
    };
    let mut sim = sim_full(seams, |_| {});
    let (p, _) = outbound(&mut sim, 1);
    knows_genesis(&sim, &p);
    sim.pass();
    let g = sim.chain.genesis().id;
    assert_eq!(sim.shared.with_state(p.handle.id(), |s| s.last_common_block), Some(Some(g)));
}

fn block_msg(b: &dash_consensus::Block) -> Vec<u8> {
    dash_consensus::core_serialize(b)
}

/// A `sim` over P3's real [`BlockDownload`].
fn sim_download() -> Sim {
    let seams = Seams {
        block_sync: Box::new(BlockDownload::new()),
        ..Seams::default()
    };
    sim_full(seams, |_| {})
}

#[test]
fn block_checked_punishes_the_blocks_source_through_block_download() {
    // P2a-core D65 on P3's `BlockDownload` (P3 §3.7): a valid block punishes no one; an invalid
    // one scores its `mapBlockSource` peer 100, and that peer's `SendMessages` in the same pass
    // discourages it.
    let mut sim = sim_download();
    let (p, _) = outbound(&mut sim, 1);
    let id = p.handle.id();
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), 2, 150, 3);
    send(&mut sim, &p, "block", block_msg(&blocks[0]));
    assert_eq!(sim.shared.misbehavior(id), Some((0, false)));
    assert_eq!(sim.chain.drain_snapshot().0.tip.tip_hash, blocks[0].hash());
    sim.chain.mark_bad_block(blocks[1].hash());
    send(&mut sim, &p, "block", block_msg(&blocks[1]));
    // `MaybeDiscourageAndDisconnect` cleared `m_should_discourage` as it disconnected.
    assert!(p.handle.disconnect_requested());
    assert_eq!(sim.shared.misbehavior(id), Some((100, false)));
}

#[test]
fn block_checked_hands_the_seam_the_signals_ibd_not_the_published_view() {
    // CH1-node rev3.1 (CH1-node.md:962, test (d) at :1070): V evaluates np:2335's
    // `IsInitialBlockDownload()` when it signals, and the view may still disagree when the
    // queued signal is delivered. The test stands in for V: each push is followed by V's next
    // publication (C7 (a)), which makes it eligible (P3 D37).
    let source = Arc::new(AtomicI64::new(-1));
    let (sync, checked) = LastCommon::new(&source);
    let seams = Seams {
        block_sync: Box::new(sync),
        ..Seams::default()
    };
    let mut sim = sim_full(seams, |_| {});
    let (p, _) = outbound(&mut sim, 1);
    source.store(p.handle.id(), Ordering::SeqCst);
    sim.pass();
    let hash = hash_of(&SimChain::build(&sim.chain.genesis(), 1, 150, 3)[0]);
    // The view still says IBD; V's signal says the block ended it.
    sim.chain.set_ibd(true);
    sim.shared.block_checked(hash, None, false);
    sim.chain.republish();
    sim.pass();
    // And the other way round.
    sim.chain.set_ibd(false);
    sim.shared.block_checked(hash, None, true);
    sim.chain.republish();
    sim.pass();
    assert_eq!(*checked.lock().unwrap(), vec![(false, true), (true, false)]);
}

#[test]
fn a_block_checked_inside_process_messages_punishes_in_the_same_pass() {
    // np:2314-2343 runs inside the source's ProcessMessages, so its SendMessages in that pass
    // discourages it (np:5833-5870). On P3's two-phase handler the signal is applied in the
    // block wait or the drain after the reply, before the tail, so `BlockChecked` finds the
    // source and erases it, and the tail's erase finds nothing (P3 §3.7, D1).
    let mut sim = sim_download();
    let (p, _) = outbound(&mut sim, 1);
    let (q, _) = outbound(&mut sim, 2);
    let block = &SimChain::build_blocks(&sim.chain.genesis(), 1, 150, 3)[0];
    sim.chain.mark_bad_block(block.hash());
    sim.deliver(&p, &[("block", block_msg(block))]);
    sim.pass();
    assert!(p.handle.disconnect_requested());
    assert!(!q.handle.disconnect_requested());
    assert_eq!(sim.chain.submits().len(), 1);
    assert!(sim.chain.submits()[0].merkle_checked);
    assert_eq!(sim.chain.merkle_passes(), 0);
}

// ---- `getpeerinfo` (CNodeStateStats)

#[test]
fn state_stats_report_services_ping_wait_and_relay() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    sim.clock.set_mock_time(SIM_START + 3);
    let s = sim.shared.node_state_stats(&p.handle).unwrap();
    assert_eq!(s.sync_height, -1);
    assert_eq!(s.common_height, -1);
    assert_eq!(s.their_services, ServiceFlags(0x1 | 0x4 | 0x400 | BIT11));
    assert_eq!(s.misbehavior_score, 0);
    // The first `SendMessages` pinged at SIM_START.
    assert_eq!(s.ping_wait_us, 3_000_000);
    assert!(s.relay_txs);
    assert!(s.addr_relay_enabled);
}

/// BL-294: `getpeerinfo` reads the clock under the peer's state lock, as Core reads `GetTime()`
/// under `cs_main`. The test holds the lock, as the actor's `SendMessages` does, while
/// `getpeerinfo` waits for it; meanwhile the clock moves 5 s and a ping goes out at the new
/// time. A `now` read before the wait would give a ping wait of −5 s.
// Core: net_processing.cpp:1877-1906 PeerManagerImpl::GetNodeStateStats
#[test]
fn state_stats_read_the_clock_under_the_lock_so_ping_wait_is_never_negative() {
    let mut sim = sim();
    let (p, _) = outbound(&mut sim, 1);
    let (shared, handle, clock) = (Arc::clone(&sim.shared), p.handle.clone(), sim.clock.clone());
    let s = std::thread::scope(|s| {
        sim.shared
            .with_state_mut(p.handle.id(), |st| {
                let rpc = s.spawn(|| shared.node_state_stats(&handle).expect("the peer"));
                // Long enough for the caller to reach the lock; a shorter wait only weakens the
                // check, never fails it.
                std::thread::sleep(Duration::from_millis(200));
                clock.set_mock_time(SIM_START + 5);
                st.net.ping.nonce_sent = 7;
                st.net.ping.start_us = (SIM_START + 5) * 1_000_000;
                rpc
            })
            .expect("the peer")
            .join()
            .expect("getpeerinfo")
    });
    assert_eq!(s.ping_wait_us, 0);
}

// ---- the full-section pass (DEC-56; R1-m-1)

/// Core's `MSG_SPORK`.
const MSG_SPORK: u32 = 6;

/// The block section 12's stub asks for.
fn wanted_block() -> Inventory {
    Inventory {
        inv_type: InvType::Block,
        hash: dash_num::Hash256::from_lendian([0xb1; 32]),
    }
}

/// The object section 13's stub asks for.
fn wanted_object() -> Inventory {
    Inventory {
        inv_type: InvType::new(MSG_SPORK),
        hash: dash_num::Hash256::from_lendian([0x0b; 32]),
    }
}

/// Section 12 asks every peer for one block on every pass (P3's download, stubbed).
struct WantsBlock;

impl BlockSync for WantsBlock {
    fn is_block_requested(&self, _hash: &BlockHash) -> bool {
        false
    }
    fn any_in_flight(&self) -> bool {
        false
    }
    fn blocks_in_flight(&self, _peer: i64) -> usize {
        0
    }
    fn stall_section(&mut self, _cx: &mut SyncCx<'_>, _now_us: i64) -> bool {
        false
    }
    fn getdata_section(
        &mut self,
        _cx: &mut SyncCx<'_>,
        batch: &mut GetDataBatch<'_>,
        _sync: bool,
        _now_us: i64,
    ) {
        batch.push_block(wanted_block());
    }
    fn finalize(&mut self, _peer: i64) {}
}

/// Section 13 asks every peer for one spork on every pass (P5a's, stubbed).
struct WantsObject;

impl ObjectRequests for WantsObject {
    fn received_response(&mut self, _peer: i64, _inv: &Inventory) {}
    fn send_section(&mut self, batch: &mut GetDataBatch<'_>, _now_us: i64) {
        batch.push(wanted_object());
    }
    fn finalize(&mut self, _peer: i64) {}
}

/// R1-m-1's full-section `SendMessages` twin (P2a §7.2 E, DEC-56): in one pass, two outbound
/// peers have work pending in section 4 (a queued ping), section 7 (a new tip: peer A prefers
/// headers), section 8 (peer B does not, so the tip goes out as an `inv`; the trickle timer
/// fires), section 11 (A is the sync peer and now behind our tip) and sections 12-13 (a block
/// and an object, one `getdata`). Each peer's whole outbound sequence follows Core's section
/// order (net_processing.cpp:6300-6898): ping, announcement, getdata. The session leg's twin
/// (`node-l1/tests/p2p_node.rs`, `one_pass_with_work_pending_in_several_sections`) compares
/// sections 4, 5, 6, 8, 10 and 11 with Core; this pass covers the sections no session step
/// can reach.
#[test]
fn one_pass_runs_every_section_with_its_work() {
    let seams = Seams {
        block_sync: Box::new(WantsBlock),
        objects: Box::new(WantsObject),
        ..Seams::default()
    };
    let mut sim = sim_full(seams, |_| {});
    let (a, _) = outbound(&mut sim, 1);
    let (b, _) = outbound(&mut sim, 2);
    send(&mut sim, &a, "sendheaders", vec![]);
    for p in [&a, &b] {
        knows_genesis(&sim, p);
        sim.shared.with_state_mut(p.handle.id(), |s| s.net.ping.queued = true);
    }
    assert!(sim.shared.with_state(a.handle.id(), |s| s.sync_started).unwrap());
    // What the setup passes sent (the stubs ask on every pass) is not the twin pass's.
    for p in [&a, &b] {
        sim.sent(p);
        // Due again, so the twin pass must redraw it.
        sim.shared.with_state_mut(p.handle.id(), |s| s.next_inv_send_us = 0);
    }
    let hs = new_tip(&sim, 1);
    sim.pass();
    let getdata = {
        let mut buf = Vec::new();
        Inv {
            inventory: vec![wanted_block(), wanted_object()],
        }
        .encode(&mut buf);
        buf
    };
    let (to_a, to_b) = (sim.sent(&a), sim.sent(&b));
    assert_eq!(cmds(&to_a), ["ping", "headers", "getdata"]);
    assert_eq!(to_a[1].1, headers_msg(&hs));
    assert_eq!(to_a[2].1, getdata);
    assert_eq!(cmds(&to_b), ["ping", "inv", "getdata"]);
    assert_eq!(to_b[1].1, inv_of(&[hash_of(&hs[0])]));
    assert_eq!(to_b[2].1, getdata);
    let now_s = sim.core.clock().node_now().0;
    for p in [&a, &b] {
        sim.shared
            .with_state(p.handle.id(), |s| {
                assert!(!s.net.ping.queued && s.net.ping.nonce_sent != 0);
                assert!(s.next_inv_send_us > now_s * 1_000_000, "the trickle timer was redrawn");
            })
            .unwrap();
    }
    // Section 11: the sync peer is behind our new tip, so its chain-sync timeout is armed.
    let cs = sim.shared.with_state(a.handle.id(), |s| s.chain_sync.timeout).unwrap();
    assert_eq!(cs, now_s + dashd_p2p::processing::eviction::CHAIN_SYNC_TIMEOUT);
}

// ---- the trickle timer (section 8, D61)

/// Every draw reads zero bytes, so every `GetRand` returns the same value.
struct ZeroRand;

impl RandSource for ZeroRand {
    fn rand_bytes(&self, out: &mut [u8], _strong: bool) {
        out.fill(0);
    }
}

#[test]
fn the_trickle_timer_draws_cores_average_per_connection() {
    // np:6599-6607: due strictly before now; inbound peers share a 5 s draw, outbound peers
    // draw 2 s, and an outbound peer with a verified masternode 1 s.
    let src: Arc<dyn RandSource> = Arc::new(ZeroRand);
    let u = get_rand(&src, 1 << 48);
    let mut sim = sim_full(Seams::default(), |cfg| *cfg = cfg.clone().with_rand_source(src));
    let (o, _) = outbound(&mut sim, 1);
    let mn = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    mn.handle.set_verified_proreg_hash([1; 32]);
    sim.handshake(&mn, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    let v = PeerVersionMsg::v24(SIM_START);
    let (i, _) = ready(&mut sim, 3, ConnectionType::Inbound, Permissions::NONE, v);
    let now = SIM_START * 1_000_000;
    let next = |sim: &Sim, p: &SimPeer| {
        sim.shared.with_state(p.handle.id(), |s| s.next_inv_send_us).unwrap()
    };
    assert_eq!(next(&sim, &o), exponential_rand(now, 2, u));
    assert_eq!(next(&sim, &mn), exponential_rand(now, 1, u));
    assert_eq!(next(&sim, &i), exponential_rand(now, 5, u));
    // Due exactly now is not yet due.
    sim.shared.with_state_mut(o.handle.id(), |s| s.next_inv_send_us = now);
    sim.pass();
    assert_eq!(next(&sim, &o), now);
    sim.shared.with_state_mut(o.handle.id(), |s| s.next_inv_send_us = now - 1);
    sim.pass();
    assert_eq!(next(&sim, &o), exponential_rand(now, 2, u));
}

// ---- `AlreadyHave`'s rejects-filter reset (D69)

/// Counts the draws and answers each with distinct bytes.
#[derive(Default)]
struct CountingRand(AtomicI64);

impl RandSource for CountingRand {
    fn rand_bytes(&self, out: &mut [u8], _strong: bool) {
        let n = self.0.fetch_add(1, Ordering::Relaxed);
        out.iter_mut().enumerate().for_each(|(i, b)| *b = (n as u8).wrapping_add(i as u8));
    }
}

#[test]
fn a_transaction_inv_redraws_the_rejects_filter_once_per_tip() {
    // np:2355-2366: the first `tx`, `dstx` or asset-unlock entry at a new tip resets
    // `m_recent_rejects`, a draw (common/bloom.cpp:387); other types never do.
    let src = Arc::new(CountingRand::default());
    let rand: Arc<dyn RandSource> = src.clone();
    let mut sim = sim_full(Seams::default(), |cfg| *cfg = cfg.clone().with_rand_source(rand));
    let v = PeerVersionMsg::v24(SIM_START);
    let (p, _) = ready(&mut sim, 1, ConnectionType::Inbound, Permissions::NONE, v);
    let draws = |sim: &mut Sim, inv_type: InvType, fill: u8| {
        let before = src.0.load(Ordering::Relaxed);
        let mut buf = Vec::new();
        Inv {
            inventory: vec![Inventory {
                inv_type,
                hash: dash_num::Hash256::from_lendian([fill; 32]),
            }],
        }
        .encode(&mut buf);
        send(sim, &p, "inv", buf);
        src.0.load(Ordering::Relaxed) - before
    };
    assert_eq!(draws(&mut sim, InvType::GovernanceObject, 1), 0);
    assert_eq!(draws(&mut sim, InvType::Tx, 2), 1);
    assert_eq!(draws(&mut sim, InvType::Dstx, 3), 0);
    assert_eq!(draws(&mut sim, InvType::AssetUnlock, 4), 0);
    let g = sim.chain.genesis();
    let tip = sim.chain.add(&SimChain::build(&g, 1, 150, 1));
    sim.chain.set_tip(tip.id);
    assert_eq!(draws(&mut sim, InvType::AssetUnlock, 5), 1);
    assert_eq!(draws(&mut sim, InvType::Tx, 6), 0);
}
