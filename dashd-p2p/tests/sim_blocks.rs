//! P3 S2 in `sim` (P3 §6.2): block download, stalling and timeouts through the real
//! `BlockDownload` and `SimChain`'s block path. S2-a ports `p2p_ibd_stalling.py` step for step;
//! S2-b ports `p2p_block_sync.py`; the rest are not FTs and name the Core lines they pin.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use dash_consensus::{Block, BlockId, BlockValidationError};
use dash_p2p::{CommandString, ConnectionType, PeerId};
use dash_p2p_core::{GetData, Headers, InvType, Inventory};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::{BlockDownload, StallingTimeout};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::{
    BlockCheckedCx, BlockSubmit, BlockSync, ChainAccess, GetDataBatch, NoObjectRequests,
    ObjectRequests, SyncCx,
};
use dashd_p2p::processing::sync::block_hash;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer, SimRead};
use dashd_p2p::{Registry, Seams};

thread_local! {
    /// The `net` and `validation` lines this thread logged while [`capture`] ran.
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// A global subscriber, installed before any callsite registers, so no callsite's interest is
/// cached as "never" by a test that ran first without one (P3 D75's trap); events go to the
/// emitting thread's [`LINES`]. The sim runs the message loop on the test's thread.
struct Global;

impl tracing::Subscriber for Global {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event()
            && ["net", "validation"].contains(&meta.target())
            && LINES.with(|l| l.borrow().is_some())
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut String);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                if f.name() == "message" {
                    *self.0 = format!("{v:?}");
                }
            }
        }
        let mut line = String::new();
        event.record(&mut Message(&mut line));
        LINES.with(|l| l.borrow_mut().as_mut().map(|v| v.push(line)));
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| tracing::subscriber::set_global_default(Global).expect("one subscriber"));
}

/// Runs `f`, returning what it logged on this thread.
fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    LINES.with(|l| *l.borrow_mut() = Some(Vec::new()));
    let r = f();
    (r, LINES.with(|l| l.borrow_mut().take()).unwrap_or_default())
}

/// The index of the first captured line containing `want`.
fn pos(lines: &[String], want: &str) -> usize {
    lines
        .iter()
        .position(|l| l.contains(want))
        .unwrap_or_else(|| panic!("no {want:?} in {lines:?}"))
}

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l.contains(want))
}

fn params() -> Arc<ChainParams> {
    Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap())
}

/// A node with P3's `BlockDownload`, on a chain holding only genesis, in IBD.
fn node() -> Sim {
    node_with(SIM_START, Box::new(NoObjectRequests))
}

/// As [`node`], with the mock time starting at `start` and the object requests `objects`.
fn node_with(start: i64, objects: Box<dyn ObjectRequests>) -> Sim {
    node_seams(start, Box::new(BlockDownload::new()), objects)
}

fn node_seams(start: i64, block_sync: Box<dyn BlockSync>, objects: Box<dyn ObjectRequests>) -> Sim {
    install();
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        block_sync,
        objects,
        ..Seams::default()
    };
    let chain = SimChain::new(Arc::clone(&p));
    Sim::with_chain(
        p,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        seams,
        start,
        |_| chain,
        |_| {},
    )
}

fn headers_msg(blocks: &[Arc<Block>]) -> Vec<u8> {
    let mut buf = Vec::new();
    Headers {
        headers: blocks.iter().map(|b| *b.header().raw()).collect::<Vec<BlockHeader>>(),
    }
    .encode(&mut buf);
    buf
}

/// The block hashes of a `getdata`'s `MSG_BLOCK` entries, and its other entries' count.
///
/// # Panics
///
/// When an entry of another type comes before a block entry: section 12 appends before
/// section 13 (np:6820-6875).
fn getdata_blocks(payload: &[u8]) -> (Vec<BlockHash>, usize) {
    let gd = GetData::decode(&mut &payload[..]).expect("a getdata payload");
    let blocks: Vec<BlockHash> =
        gd.inventory.iter().filter(|i| i.inv_type == InvType::Block).map(block_hash).collect();
    assert!(
        gd.inventory[..blocks.len()].iter().all(|i| i.inv_type == InvType::Block),
        "block entries first"
    );
    let others = gd.inventory.len() - blocks.len();
    (blocks, others)
}

fn hashes(blocks: &[Arc<Block>]) -> Vec<BlockHash> {
    blocks.iter().map(|b| b.hash()).collect()
}

/// A scripted peer that serves blocks: `P2PStaller` (p2p_ibd_stalling.py:33-46) when the rig
/// withholds a block, `P2PDataStore` otherwise. It records every `getdata` and answers each
/// block request it can, unless the rig is mute; it ignores `getheaders`.
struct Server {
    peer: SimPeer,
    getdata_requests: Vec<BlockHash>,
    /// Each `getdata`: its block hashes in order and its other entries' count.
    getdatas: Vec<(Vec<BlockHash>, usize)>,
    /// Finalized by the node.
    gone: bool,
    /// Sends no block.
    mute: bool,
}

/// The node and its scripted peers.
struct Net {
    sim: Sim,
    peers: Vec<Server>,
    store: HashMap<BlockHash, Arc<Block>>,
    /// Blocks no server sends.
    withheld: HashSet<BlockHash>,
    /// No server sends anything.
    mute: bool,
    mocktime: i64,
}

impl Net {
    fn new(sim: Sim, blocks: &[Arc<Block>], mocktime: i64) -> Self {
        Self {
            sim,
            peers: Vec::new(),
            store: blocks.iter().map(|b| (b.hash(), Arc::clone(b))).collect(),
            withheld: HashSet::new(),
            mute: false,
            mocktime,
        }
    }

    /// Connects server `n` as `conn_type` (or inbound when `None`) with version services
    /// `services`, runs the handshake and returns its index.
    fn connect(&mut self, n: u8, conn_type: Option<ConnectionType>, services: u64) -> usize {
        let peer = match conn_type {
            Some(t) => self.sim.connect(n, t, Default::default()),
            None => self.sim.accept(n, Default::default()).expect("inbound admitted"),
        };
        let version = PeerVersionMsg {
            services,
            ..PeerVersionMsg::v24(self.mocktime)
        };
        self.sim.handshake(&peer, version);
        self.peers.push(Server {
            peer,
            getdata_requests: Vec::new(),
            getdatas: Vec::new(),
            gone: false,
            mute: false,
        });
        self.peers.len() - 1
    }

    fn send(&self, i: usize, cmd: &str, payload: Vec<u8>) {
        self.sim.deliver(&self.peers[i].peer, &[(cmd, payload)]);
    }

    /// Message-loop passes, each followed by every connected server's answers, until a pass
    /// has no more work, no server answered and no peer was left to finalize: Core's
    /// `sync_with_ping` on every peer, with `FinalizeNode` between them as the socket thread
    /// runs it.
    fn all_sync_send_with_ping(&mut self) {
        while self.round() {}
    }

    /// One [`Net::all_sync_send_with_ping`] round: whether it finalized a peer.
    fn round(&mut self) -> bool {
        loop {
            let more = self.sim.pass();
            let mut answered = false;
            for s in self.peers.iter_mut().filter(|s| !s.gone) {
                let mut frames = Vec::new();
                for (cmd, payload) in self.sim.sent(&s.peer) {
                    if cmd != CommandString::GETDATA {
                        continue;
                    }
                    let (blocks, others) = getdata_blocks(&payload);
                    for &h in &blocks {
                        s.getdata_requests.push(h);
                        if !self.mute && !s.mute && !self.withheld.contains(&h) {
                            frames
                                .push(("block", dash_consensus::core_serialize(&*self.store[&h])));
                        }
                    }
                    s.getdatas.push((blocks, others));
                }
                if !frames.is_empty() && !s.peer.handle.disconnect_requested() {
                    answered = true;
                    self.sim.deliver(&s.peer, &frames);
                }
            }
            if !more && !answered {
                break;
            }
        }
        self.sim.remove_disconnected();
        let mut finalized = false;
        for s in self.peers.iter_mut().filter(|s| !s.gone) {
            s.gone = s.peer.handle.disconnect_requested();
            finalized |= s.gone;
        }
        finalized
    }

    /// The framework's `wait_until`: Core's message loop keeps calling `SendMessages`, so
    /// rounds run until `cond` holds, at most 8.
    fn wait_until(&mut self, cond: impl Fn(&Self) -> bool) {
        for _ in 0..8 {
            if cond(self) {
                return;
            }
            self.all_sync_send_with_ping();
        }
        assert!(cond(self), "wait_until timed out");
    }

    fn set_mocktime(&mut self, t: i64) {
        self.mocktime = t;
        self.sim.clock.set_mock_time(t);
    }

    fn num_connected(&self) -> usize {
        self.peers.iter().filter(|s| !s.gone).count()
    }

    fn is_block_requested(&self, hash: BlockHash) -> bool {
        self.peers.iter().any(|s| !s.gone && s.getdata_requests.contains(&hash))
    }

    /// `total_bytes_recv_for_blocks` (p2p_ibd_stalling.py:155-160), over every peer.
    fn total_bytes_recv_for_blocks(&self) -> u64 {
        let block = CommandString::from_static("block");
        self.peers
            .iter()
            .map(|s| s.peer.handle.byte_stats().recv.get(&block).copied().unwrap_or(0))
            .sum()
    }

    fn tip_height(&self) -> u32 {
        self.sim.chain.snapshot().tip.tip_height
    }
}

/// `NODE_NETWORK | NODE_BLOOM`, p2p_ibd_stalling's peers.
const NETWORK_BLOOM: u64 = 0x1 | 0x4;

/// S2-a: `p2p_ibd_stalling.py` (P3 §6.2), its steps in order with its constants and mocktime
/// steps. Core: net_processing.cpp:1477-1559, 6752-6771, 6826-6837, 2199-2207.
#[test]
fn s2a_ibd_stalling() {
    const NUM_BLOCKS: usize = 1025;
    const NUM_PEERS: u8 = 4;
    let sim = node();
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), NUM_BLOCKS, 1, 0);
    let stall_block = blocks[0].hash();
    let mut t = Net::new(sim, &blocks, SIM_START);
    t.withheld.insert(stall_block);

    // "Check that a staller does not get disconnected if the 1024 block lookahead buffer is
    // filled."
    t.set_mocktime(SIM_START + 1);
    for id in 0..NUM_PEERS {
        let i = t.connect(id + 1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
        t.send(i, "headers", headers_msg(&blocks[..NUM_BLOCKS - 1]));
    }
    // The 1023 blocks other than the withheld one, each received once (Core's 172761 bytes,
    // for its blocks).
    let want: u64 = blocks[1..NUM_BLOCKS - 1]
        .iter()
        .map(|b| (24 + dash_consensus::core_serialize(&**b).len()) as u64)
        .sum();
    t.wait_until(|t| t.total_bytes_recv_for_blocks() == want);
    t.all_sync_send_with_ping();
    // If there was a peer marked for stalling, it would get disconnected.
    t.set_mocktime(t.mocktime + 3);
    t.all_sync_send_with_ping();
    assert_eq!(t.num_connected(), usize::from(NUM_PEERS));

    // "Check that increasing the window beyond 1024 blocks triggers stalling logic."
    let all = headers_msg(&blocks);
    let ((), lines) = capture(|| {
        for i in 0..t.peers.len() {
            t.send(i, "headers", all.clone());
        }
        t.all_sync_send_with_ping();
    });
    assert!(has(&lines, "Stall started"), "{lines:?}");

    // "Check that the stalling peer is disconnected after 2 seconds."
    t.set_mocktime(t.mocktime + 3);
    let ((), lines) = capture(|| t.wait_until(|t| t.peers[0].gone));
    let first = t.peers[0].peer.handle.id();
    // Core logs the disconnect, then raises the timeout and logs that (np:6758-6765).
    let stalling = pos(&lines, &format!("Peer={first} is stalling block download, disconnecting"));
    let raised = pos(&lines, "Increased stalling timeout temporarily to 4 seconds");
    assert!(stalling < raised, "{lines:?}");
    assert_eq!(t.num_connected(), usize::from(NUM_PEERS) - 1);
    t.wait_until(|t| t.is_block_requested(stall_block));
    // Make sure that SendMessages() is invoked, which assigns the missing block to another
    // peer and starts the stalling logic for them.
    t.all_sync_send_with_ping();
    assert_eq!(t.sim.shared.signals().stalling_timeout(), 4);

    // "Check that the stalling timeout gets doubled to 4 seconds for the next staller."
    t.set_mocktime(t.mocktime + 3);
    t.all_sync_send_with_ping();
    assert_eq!(t.num_connected(), usize::from(NUM_PEERS) - 1);
    t.set_mocktime(t.mocktime + 2);
    t.wait_until(|t| t.num_connected() == usize::from(NUM_PEERS) - 2);
    t.wait_until(|t| t.is_block_requested(stall_block));
    t.all_sync_send_with_ping();

    // "Check that the stalling timeout gets doubled to 8 seconds for the next staller."
    t.set_mocktime(t.mocktime + 7);
    t.all_sync_send_with_ping();
    assert_eq!(t.num_connected(), usize::from(NUM_PEERS) - 2);
    t.set_mocktime(t.mocktime + 2);
    t.wait_until(|t| t.num_connected() == usize::from(NUM_PEERS) - 3);
    t.wait_until(|t| t.is_block_requested(stall_block));
    t.all_sync_send_with_ping();
    assert_eq!(t.sim.shared.signals().stalling_timeout(), 16);

    // "Provide the withheld block and check that stalling timeout gets reduced back to 2
    // seconds."
    let ((), lines) = capture(|| {
        let block = dash_consensus::core_serialize(&*blocks[0]);
        for s in t.peers.iter().filter(|s| !s.gone && s.getdata_requests.contains(&stall_block)) {
            t.sim.deliver(&s.peer, &[("block", block.clone())]);
        }
        t.all_sync_send_with_ping();
    });
    assert!(has(&lines, "Decreased stalling timeout to 2 seconds"), "{lines:?}");
    assert_eq!(t.sim.shared.signals().stalling_timeout(), 2);

    // "Check that all outstanding blocks get connected."
    t.wait_until(|t| t.tip_height() == NUM_BLOCKS as u32);
}

/// S2-b: `p2p_block_sync.py` (node0 → node1 → node2): each node in IBD syncs a new block from
/// an inbound peer, through the getdata condition's `sync && !limited` branch
/// (np:6822; `sync_blocks_and_headers_from_peer`, np:6350-6366). The `sim` runs one node, so
/// each leg is one node with its neighbours scripted (D82): node1 with inbound node0 and
/// outbound node2, then node2 with inbound node1 alone.
#[test]
fn s2b_block_sync_from_inbound_peers_in_ibd() {
    let block = SimChain::build_blocks(&params_genesis(), 1, 1, 0);
    // node1: an outbound peer (node2) that knows nothing, an inbound one (node0) that mined.
    let mut node1 = Net::new(node(), &block, SIM_START);
    node1.connect(2, Some(ConnectionType::OutboundFullRelay), V24_SERVICES);
    let node0 = node1.connect(1, None, V24_SERVICES);
    node1.send(node0, "headers", headers_msg(&block));
    node1.wait_until(|t| t.tip_height() == 1);
    assert_eq!(node1.peers[node0].getdata_requests, hashes(&block));
    // node2: its only peer is inbound (node1), so no peer is a preferred download.
    let mut node2 = Net::new(node(), &block, SIM_START);
    let node1_peer = node2.connect(1, None, V24_SERVICES);
    node2.send(node1_peer, "headers", headers_msg(&block));
    node2.wait_until(|t| t.tip_height() == 1);
    assert_eq!(node2.peers[node1_peer].getdata_requests, hashes(&block));
    // In IBD a limited sync peer is not asked (`!IsLimitedPeer`, Q1).
    let mut limited = Net::new(node(), &block, SIM_START);
    let p = limited.connect(1, None, NODE_NETWORK_LIMITED);
    limited.send(p, "headers", headers_msg(&block));
    limited.all_sync_send_with_ping();
    assert!(limited.peers[p].getdata_requests.is_empty());
    assert_eq!(limited.tip_height(), 0);
    // Out of IBD it is: the `!IsInitialBlockDownload()` branch (Q1). The tip is older than 20
    // intervals, so this is section 12, not direct fetch.
    let mut limited = Net::new(node(), &block, SIM_START);
    limited.sim.chain.set_ibd(false);
    let p = limited.connect(1, None, NODE_NETWORK_LIMITED);
    limited.send(p, "headers", headers_msg(&block));
    limited.wait_until(|t| t.tip_height() == 1);
    assert_eq!(limited.peers[p].getdata_requests, hashes(&block));
}

/// S2-c: the block download timeout (np:6773-6781) with N = 0, 1, 2 other peers downloading:
/// a disconnect at `m_downloading_since + spacing·(1 + 0.5N)` + 1 µs, not at it.
#[test]
fn s2c_block_download_timeout_scales_with_other_downloaders() {
    for n in 0..=2u8 {
        let sim = node();
        let spacing = sim.chain.params().consensus.pow_target_spacing;
        let blocks = SimChain::build_blocks(&sim.chain.genesis(), 64, 1, 0);
        let mut t = Net::new(sim, &blocks, SIM_START);
        t.mute = true;
        for id in 0..=n {
            let i = t.connect(id + 1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
            t.send(i, "headers", headers_msg(&blocks));
        }
        t.all_sync_send_with_ping();
        for (i, s) in t.peers.iter().enumerate() {
            assert_eq!(s.getdata_requests, hashes(&blocks[16 * i..16 * (i + 1)]), "n = {n}");
        }
        let deadline = SIM_START + spacing * (2 + i64::from(n)) / 2;
        t.set_mocktime(deadline);
        t.all_sync_send_with_ping();
        assert_eq!(t.num_connected(), usize::from(n) + 1, "n = {n}: not at the deadline");
        let ((), lines) = capture(|| {
            t.sim.clock.advance(Duration::from_micros(1));
            t.all_sync_send_with_ping();
        });
        assert_eq!(t.num_connected(), 0, "n = {n}: every peer's front request timed out");
        let a = t.peers[0].peer.handle.id();
        let want =
            format!("Timeout downloading block {} from peer={a}, disconnecting", blocks[0].hash());
        assert!(has(&lines, &want), "{lines:?}");
    }
}

/// Advances the clock by `d` at the processor's next `read`, once; the returned flag clears
/// when it has.
fn advance_at(t: &Net, read: SimRead, d: Duration) -> Arc<AtomicBool> {
    let armed = Arc::new(AtomicBool::new(true));
    let (flag, clock) = (Arc::clone(&armed), Arc::clone(&t.sim.clock));
    t.sim.chain.set_read_hook(Some(Arc::new(move |r| {
        if r == read && flag.swap(false, Ordering::SeqCst) {
            clock.advance(d);
        }
    })));
    armed
}

/// S2-c's timeout origins (D86): `BlockRequested` and `RemoveBlockRequest` read `GetTime()`
/// themselves (np:1350, np:1310), so time spent in the walk before a request, or in a block's
/// decode before its removal, moves `m_downloading_since`, and section 9's deadline with it
/// (np:6773-6779). Out of IBD with an old tip, so the requests are section 12's and the headers
/// handler reads no `nMinimumChainWork`.
#[test]
fn s2c_timeout_origins_are_read_at_the_request_and_the_removal() {
    let spacing_us = params().consensus.pow_target_spacing * 1_000_000;
    let at = |t: &mut Net, us: i64| {
        t.sim.clock.set_mock_time(SIM_START);
        t.sim.clock.advance(Duration::from_micros(us as u64));
        t.all_sync_send_with_ping();
    };
    let setup = || {
        let sim = node();
        sim.chain.set_ibd(false);
        let blocks = SimChain::build_blocks(&sim.chain.genesis(), 20, 1, 0);
        let mut t = Net::new(sim, &blocks, SIM_START);
        t.mute = true;
        let a = t.connect(1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
        (t, blocks, a)
    };
    // The walk takes 250 ms before section 12's first request: the origin is 0.25 s.
    let (mut t, blocks, a) = setup();
    let armed = advance_at(&t, SimRead::MinChainWork, Duration::from_millis(250));
    t.send(a, "headers", headers_msg(&blocks));
    t.all_sync_send_with_ping();
    assert!(!armed.load(Ordering::SeqCst));
    assert_eq!(t.peers[a].getdata_requests, hashes(&blocks[..16]));
    at(&mut t, spacing_us + 100_000);
    assert!(!t.peers[a].gone, "kept at 150.100 s: Core's deadline is 150.250 s");
    let ((), lines) = capture(|| at(&mut t, spacing_us + 250_001));
    assert!(t.peers[a].gone);
    let id = t.peers[a].peer.handle.id();
    let want =
        format!("Timeout downloading block {} from peer={id}, disconnecting", blocks[0].hash());
    assert!(has(&lines, &want), "{lines:?}");
    // The oldest block arrives at 10 s and its decode takes 200 ms: the next block's origin is
    // 10.2 s.
    let (mut t, blocks, a) = setup();
    t.send(a, "headers", headers_msg(&blocks));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[a].getdata_requests, hashes(&blocks[..16]));
    t.set_mocktime(SIM_START + 10);
    let armed = advance_at(&t, SimRead::LoadingBlocks, Duration::from_millis(200));
    t.send(a, "block", dash_consensus::core_serialize(&*blocks[0]));
    t.all_sync_send_with_ping();
    assert!(!armed.load(Ordering::SeqCst));
    assert_eq!(t.tip_height(), 1);
    at(&mut t, 10_000_000 + spacing_us + 100_000);
    assert!(!t.peers[a].gone, "kept at 160.100 s: Core's deadline is 160.200 s");
    let ((), lines) = capture(|| at(&mut t, 10_000_000 + spacing_us + 200_001));
    assert!(t.peers[a].gone);
    let id = t.peers[a].peer.handle.id();
    let want =
        format!("Timeout downloading block {} from peer={id}, disconnecting", blocks[1].hash());
    assert!(has(&lines, &want), "{lines:?}");
}

/// Section 13 for S2-d: `n` objects per pass.
struct Objects(Arc<AtomicUsize>);

impl ObjectRequests for Objects {
    fn received_response(&mut self, _peer: i64, _inv: &Inventory) {}
    fn send_section(&mut self, batch: &mut GetDataBatch<'_>, _now_us: i64) {
        for i in 0..self.0.load(Ordering::SeqCst) {
            let mut h = [0x0b; 32];
            h[..8].copy_from_slice(&(i as u64).to_le_bytes());
            batch.push(Inventory {
                inv_type: InvType::new(6), // MSG_SPORK
                hash: dash_num::Hash256::from_lendian(h),
            });
        }
    }
    fn finalize(&mut self, _peer: i64) {}
}

/// S2-d: 16 blocks in flight per peer (np:6822), one `getdata` per pass with the block entries
/// before the objects, pushed at `MAX_GETDATA_SZ` and at the end as Core does (np:6826-6829,
/// np:6870-6875, np:6894-6897).
#[test]
fn s2d_in_flight_cap_and_getdata_batching() {
    let objects = Arc::new(AtomicUsize::new(3));
    let sim = node_with(SIM_START, Box::new(Objects(Arc::clone(&objects))));
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), 40, 1, 0);
    let mut t = Net::new(sim, &blocks, SIM_START);
    t.mute = true;
    let a = t.connect(1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    t.peers[a].getdatas.clear();
    objects.store(0, Ordering::SeqCst);
    t.send(a, "headers", headers_msg(&blocks));
    objects.store(3, Ordering::SeqCst);
    t.sim.pass();
    objects.store(0, Ordering::SeqCst);
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[a].getdatas, [(hashes(&blocks[..16]), 3)], "16 blocks, then the objects");
    // One block in: one more request, and no more than 16 in flight.
    let next = |t: &mut Net, i: usize, k: usize| {
        t.peers[a].getdatas.clear();
        t.send(a, "block", dash_consensus::core_serialize(&*blocks[i]));
        objects.store(k, Ordering::SeqCst);
        t.sim.pass();
        objects.store(0, Ordering::SeqCst);
        t.all_sync_send_with_ping();
        std::mem::take(&mut t.peers[a].getdatas)
    };
    assert_eq!(next(&mut t, 0, 0), [(hashes(&blocks[16..17]), 0)]);
    // 1 + 999 entries: pushed at 1000 inside section 13, nothing left for the end.
    assert_eq!(next(&mut t, 1, 999), [(hashes(&blocks[17..18]), 999)]);
    // 1 + 1000: split where Core splits.
    assert_eq!(next(&mut t, 2, 1000), [(hashes(&blocks[18..19]), 999), (vec![], 1)]);
}

/// S2-e, the `MSG_BLOCK` part (np:3202-3258, np:1428-1431): out of IBD with a tip younger than
/// 20 block intervals, a `headers` message is answered from the headers handler, in IBD too
/// (`CanDirectFetch` does not read it); 17 blocks reaching the active chain are capped at the
/// 16 in flight; a walk of 17 that does not reach it is a large reorg; an old tip leaves it to
/// section 12. The `MSG_CMPCT_BLOCK` swap is S3's.
#[test]
fn s2e_headers_direct_fetch_requests_msg_block() {
    let genesis = params_genesis();
    let start = i64::from(genesis.time) + 10;
    let run_ibd = |n: usize, mocktime: i64, ibd: bool| {
        let sim = node_with(mocktime, Box::new(NoObjectRequests));
        sim.chain.set_ibd(ibd);
        let blocks = SimChain::build_blocks(&genesis, n, 1, 0);
        let mut t = Net::new(sim, &blocks, mocktime);
        t.mute = true;
        let a = t.connect(1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
        t.peers[a].getdatas.clear();
        let ((), lines) = capture(|| {
            t.send(a, "headers", headers_msg(&blocks));
            t.all_sync_send_with_ping();
        });
        let id = t.peers[a].peer.handle.id();
        (blocks, t.peers.swap_remove(a).getdatas, lines, id)
    };
    let run = |n: usize, mocktime: i64| run_ibd(n, mocktime, false);
    // Three blocks: one direct `getdata`, earliest first.
    let (blocks, getdatas, lines, id) = run(3, start);
    assert_eq!(getdatas, [(hashes(&blocks), 0)]);
    for b in &blocks {
        assert!(
            has(&lines, &format!("Requesting block {} from  peer={id}", b.hash())),
            "{lines:?}"
        );
    }
    let last = blocks[2].hash();
    assert!(has(&lines, &format!("Downloading blocks toward {last} (3) via headers direct fetch")));
    // One block: no "toward" line, and `MSG_BLOCK` (S3 swaps it to `MSG_CMPCT_BLOCK`).
    let (blocks, getdatas, lines, _) = run(1, start);
    assert_eq!(getdatas, [(hashes(&blocks), 0)]);
    assert!(!has(&lines, "via headers direct fetch"));
    // In IBD: the same direct `getdata`.
    let (blocks, getdatas, lines, id) = run_ibd(3, start, true);
    assert_eq!(getdatas, [(hashes(&blocks), 0)]);
    assert!(has(&lines, &format!("Requesting block {} from  peer={id}", blocks[0].hash())));
    // 17 blocks: the walk reaches genesis with 17, and 16 are requested (np:3233-3236); section
    // 12 then has no room.
    let (blocks, getdatas, lines, _) = run(17, start);
    assert_eq!(getdatas, [(hashes(&blocks[..16]), 0)]);
    assert!(!has(&lines, "Large reorg"), "{lines:?}");
    // 20 blocks: the walk collects 17 above height 3 and stops; section 12 asks for 16.
    let (blocks, getdatas, lines, id) = run(20, start);
    let last = blocks[19].hash();
    assert!(has(&lines, &format!("Large reorg, won't direct fetch to {last} (20)")), "{lines:?}");
    assert_eq!(getdatas, [(hashes(&blocks[..16]), 0)]);
    assert!(has(&lines, &format!("Requesting block {} (1) peer={id}", blocks[0].hash())));
    // A tip older than 20 intervals: no direct fetch; section 12 asks outside IBD.
    let spacing = params().consensus.pow_target_spacing;
    let (blocks, getdatas, lines, _) = run(3, i64::from(genesis.time) + 20 * spacing + 1);
    assert_eq!(getdatas, [(hashes(&blocks), 0)]);
    assert!(!has(&lines, " from  peer="), "{lines:?}");
}

/// S2-e's equal-work tie (np:3209, `Tip()->nChainWork <= last_header.nChainWork`): a sibling A′
/// of the tip A with the same work is direct-fetched at once. The peer is inbound with
/// `NODE_BLOOM` only, so section 12 never asks it (`CanServeBlocks`) while direct fetch, which
/// does not read it, does (Opus-h r1 T1).
#[test]
fn probe_equal_work_sibling_is_direct_fetched() {
    let genesis = params_genesis();
    let mocktime = i64::from(genesis.time) + 10;
    let a = SimChain::build_blocks(&genesis, 1, 1, 0);
    let a2 = SimChain::build_blocks(&genesis, 1, 1, 7);
    let mut all = a.clone();
    all.extend(a2.iter().cloned());
    let sim = node_with(mocktime, Box::new(NoObjectRequests));
    sim.chain.set_ibd(false);
    let mut t = Net::new(sim, &all, mocktime);
    let p = t.connect(1, None, NODE_BLOOM);
    t.send(p, "headers", headers_msg(&a));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[p].getdata_requests, hashes(&a), "direct fetch of A");
    assert_eq!(t.tip_height(), 1);
    t.peers[p].getdata_requests.clear();
    t.peers[p].mute = true;
    t.send(p, "headers", headers_msg(&a2));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[p].getdata_requests, hashes(&a2), "equal work: Core fetches A′");
}

/// `CanDirectFetch`'s strict boundary (np:1430, `tip_time > now - 20·spacing`): a genesis tip
/// exactly 20 intervals old is not direct-fetched from; one second younger, it is. The peer
/// does not serve blocks, so section 12 stays silent (Opus-h r1 T2).
#[test]
fn s2e_can_direct_fetch_is_strict_at_twenty_intervals() {
    let genesis = params_genesis();
    let spacing = params().consensus.pow_target_spacing;
    let blocks = SimChain::build_blocks(&genesis, 1, 1, 0);
    for (dt, fetched) in [(20 * spacing, false), (20 * spacing - 1, true)] {
        let mocktime = i64::from(genesis.time) + dt;
        let sim = node_with(mocktime, Box::new(NoObjectRequests));
        sim.chain.set_ibd(false);
        let mut t = Net::new(sim, &blocks, mocktime);
        t.mute = true;
        let p = t.connect(1, None, NODE_BLOOM);
        t.send(p, "headers", headers_msg(&blocks));
        t.all_sync_send_with_ping();
        let want = if fetched {
            hashes(&blocks)
        } else {
            vec![]
        };
        assert_eq!(t.peers[p].getdata_requests, want, "dt = {dt}");
    }
}

/// S2-g: a block-relay-only outbound peer is asked for blocks and stalls like a full-relay one
/// (§3.9: no type-specific rule; `fPreferredDownload` np:4109). Its `sendcmpct` is S3's (D82).
#[test]
fn s2g_block_relay_only_peers_download_and_stall() {
    let sim = node();
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), 1025, 1, 0);
    let mut t = Net::new(sim, &blocks, SIM_START);
    let relay = t.connect(1, Some(ConnectionType::BlockRelay), NETWORK_BLOOM);
    let full = t.connect(2, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    // The block-relay-only peer announces first and sends nothing it is asked for.
    t.peers[relay].mute = true;
    t.send(relay, "headers", headers_msg(&blocks));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[relay].getdata_requests, hashes(&blocks[..16]));
    t.send(full, "headers", headers_msg(&blocks));
    let ((), lines) = capture(|| t.all_sync_send_with_ping());
    assert_eq!(t.peers[full].getdata_requests, hashes(&blocks[16..1024]));
    let relay_id = t.peers[relay].peer.handle.id();
    assert!(has(&lines, &format!("Stall started peer={relay_id}")), "{lines:?}");
    t.set_mocktime(SIM_START + 3);
    t.wait_until(|t| t.peers[relay].gone);
    assert!(!t.peers[full].gone);
}

fn params_genesis() -> dashd_p2p::processing::seams::EntryFacts {
    SimChain::new(params()).genesis()
}

/// `PeerVersionMsg::v24`'s services: `NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED |
/// NODE_HEADERS_COMPRESSED`.
const V24_SERVICES: u64 = 0x1 | 0x4 | 0x400 | 0x800;
/// `NODE_BLOOM` alone: not `CanServeBlocks`, so section 12 never asks.
const NODE_BLOOM: u64 = 0x4;
/// `NODE_NETWORK_LIMITED` alone (`IsLimitedPeer`).
const NODE_NETWORK_LIMITED: u64 = 0x400;

/// S2-f, the later-connect case (P3 §3.7): peer A's block 2 is stored with its parent missing;
/// peer B's block 1 connects both, and `ConnectBlock` fails block 2. That `BlockChecked` finds
/// A's `mapBlockSource` entry, which A's tail left (its block was new), so A is punished and B
/// is not (np:2314-2327, np:3717-3732).
#[test]
fn s2f_a_block_connected_by_another_peers_block_credits_its_source() {
    let sim = node();
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), 2, 1, 0);
    sim.chain.mark_bad_connect(blocks[1].hash());
    let mut t = Net::new(sim, &blocks, SIM_START);
    t.withheld.insert(blocks[0].hash());
    let a = t.connect(1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    let b = t.connect(2, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    t.send(a, "headers", headers_msg(&blocks));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[a].getdata_requests, hashes(&blocks));
    assert_eq!(t.tip_height(), 0, "block 2 stored, not connected");
    // B sends block 1 unasked (`force_processing` false); its ABC connects 1, then fails 2.
    t.send(b, "block", dash_consensus::core_serialize(&*blocks[0]));
    t.all_sync_send_with_ping();
    assert_eq!(t.tip_height(), 1);
    assert!(t.peers[a].gone, "A, block 2's source, is punished (BLOCK_CONSENSUS, 100)");
    assert!(!t.peers[b].gone);
}

/// What S2-h's recorder saw.
#[derive(Default)]
struct RaceSeen {
    /// At `BlockChecked(B)`: whether C was in flight, and `mapBlocksInFlight.size()`.
    checked_b: Vec<(bool, usize)>,
    /// At `HeadersDirectFetchBlocks`: `BlockChecked`s so far and the snapshot's tip.
    direct_fetch: Vec<(usize, BlockHash)>,
    checked: usize,
}

/// P3's `BlockDownload` with S2-h's recorder around it.
struct Race {
    inner: BlockDownload,
    seen: Arc<parking_lot::Mutex<RaceSeen>>,
    b: BlockHash,
    c: BlockHash,
}

impl BlockSync for Race {
    fn is_block_requested(&self, hash: &BlockHash) -> bool {
        self.inner.is_block_requested(hash)
    }
    fn any_in_flight(&self) -> bool {
        BlockSync::any_in_flight(&self.inner)
    }
    fn blocks_in_flight(&self, peer: PeerId) -> usize {
        BlockSync::blocks_in_flight(&self.inner, peer)
    }
    fn stall_section(&mut self, cx: &mut SyncCx<'_>, now_us: i64) -> bool {
        self.inner.stall_section(cx, now_us)
    }
    fn getdata_section(
        &mut self,
        cx: &mut SyncCx<'_>,
        b: &mut GetDataBatch<'_>,
        sync: bool,
        now_us: i64,
    ) {
        self.inner.getdata_section(cx, b, sync, now_us);
    }
    fn headers_direct_fetch(&mut self, cx: &mut SyncCx<'_>, last: BlockId) {
        let mut seen = self.seen.lock();
        let n = seen.checked;
        seen.direct_fetch.push((n, cx.snap.tip.tip_hash));
        drop(seen);
        self.inner.headers_direct_fetch(cx, last);
    }
    fn block_checked(
        &mut self,
        cx: &mut BlockCheckedCx<'_>,
        hash: &BlockHash,
        invalid: Option<&BlockValidationError>,
        ibd: bool,
    ) {
        let mut seen = self.seen.lock();
        seen.checked += 1;
        if *hash == self.b {
            let at = (self.inner.is_block_requested(&self.c), self.inner.in_flight_entries());
            seen.checked_b.push(at);
        }
        drop(seen);
        self.inner.block_checked(cx, hash, invalid, ibd);
    }
    fn finalize(&mut self, peer: PeerId) {
        self.inner.finalize(peer);
    }
    fn remove_block_request(&mut self, hash: &BlockHash, from: Option<PeerId>, now_us: i64) {
        self.inner.remove_block_request(hash, from, now_us);
    }
    fn emplace_block_source(&mut self, hash: BlockHash, peer: PeerId, full: bool) {
        self.inner.emplace_block_source(hash, peer, full);
    }
    fn erase_block_source(&mut self, hash: &BlockHash) {
        self.inner.erase_block_source(hash);
    }
    fn bind_source_counter(&mut self, counter: Arc<AtomicUsize>) {
        BlockSync::bind_source_counter(&mut self.inner, counter);
    }
    fn bind_stalling_timeout(&mut self, timeout: StallingTimeout) {
        BlockSync::bind_stalling_timeout(&mut self.inner, timeout);
    }
}

/// S2-h, direct fetch racing `BlockChecked` (M2, D46): B, stored from peer R with its parent A
/// missing, is connected by another origin (an RPC `submitblock A`) between Q's message-start
/// drain and V serving Q's `ProcessHeaders` for B's child C. The post-reply drain applies
/// `BlockChecked(B)` before `headers_direct_fetch` puts C in flight, so `count(B) == size()`
/// reads Core's values there, and the hook's snapshot X′ has tip B. Which snapshot the walk
/// itself reads is not pinned here: from the old one it also yields `[C]` (D83).
#[test]
fn s2h_direct_fetch_after_another_origins_connect_reads_the_redrain() {
    let genesis = params_genesis();
    let mocktime = i64::from(genesis.time) + 10;
    let blocks = SimChain::build_blocks(&genesis, 3, 1, 0);
    let (a_blk, b, c) = (Arc::clone(&blocks[0]), blocks[1].hash(), blocks[2].hash());
    let seen = Arc::new(parking_lot::Mutex::new(RaceSeen::default()));
    let race = Race {
        inner: BlockDownload::new(),
        seen: Arc::clone(&seen),
        b,
        c,
    };
    let sim = node_seams(mocktime, Box::new(race), Box::new(NoObjectRequests));
    sim.chain.set_ibd(false);
    let mut t = Net::new(sim, &blocks, mocktime);
    t.withheld.insert(a_blk.hash());
    let r = t.connect(1, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    let q = t.connect(2, Some(ConnectionType::OutboundFullRelay), NETWORK_BLOOM);
    // R announces A and B; the node fetches both from R directly, and R sends only B.
    t.send(r, "headers", headers_msg(&blocks[..2]));
    t.all_sync_send_with_ping();
    assert_eq!(t.peers[r].getdata_requests, hashes(&blocks[..2]));
    assert_eq!(t.tip_height(), 0);
    // While V serves Q's `headers(C)`, an RPC `submitblock A` connects A and B.
    let q_id = t.peers[q].peer.handle.id();
    let chain = Arc::clone(&t.sim.chain);
    let armed = Arc::new(AtomicBool::new(true));
    let hook_armed = Arc::clone(&armed);
    t.sim.chain.set_headers_hook(Some(Arc::new(move |from| {
        if from == q_id && hook_armed.swap(false, Ordering::SeqCst) {
            chain.accept_block(&BlockSubmit {
                hash: a_blk.hash(),
                block: Arc::clone(&a_blk),
                force: true,
                from: -1,
                merkle_checked: false,
            });
        }
    })));
    t.send(q, "headers", headers_msg(&blocks[2..]));
    t.all_sync_send_with_ping();
    assert!(!armed.load(Ordering::SeqCst));
    let seen = seen.lock();
    // A is still in flight from R (an RPC block is not R's); C is not yet.
    assert_eq!(seen.checked_b, [(false, 1)]);
    // `BlockChecked(A)` and `(B)` both before the walk, which starts at tip B; C's comes later.
    assert_eq!(seen.direct_fetch.last(), Some(&(2, b)));
    assert_eq!(seen.checked, 3);
    assert_eq!(t.peers[q].getdata_requests, [c]);
}
