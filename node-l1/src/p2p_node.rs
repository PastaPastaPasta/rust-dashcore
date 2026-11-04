//! The node's leg of the `diff_p2p_multi_*` targets (P2a-core; P2a §6.2): a [`Session`] script
//! run on Core's oracle session and over `dashd-p2p`'s processor in its `sim` harness, with both
//! outcomes compared after every step. SB6's [`crate::p2p`] builds the scripts.
//!
//! The node runs on a [`SimChain::validating`] chain (Core's `AcceptBlockHeader` and IBD rule),
//! with Core's services, clock and connection types, and with the peers' network core told what
//! Core's session sets up. Every step is projected onto the fields Core's outcome reports, and
//! the projections must be equal, except where they cannot be yet (P2a-core D40):
//!
//! - Randomness is not an exception: the node's network core draws Core's own `GetRandBytes`
//!   bytes, replayed phase by phase (setup, each `add_peer`, each step), and each phase's calls
//!   are compared with Core's draw log (D60). Nonces, timers and address relay therefore compare
//!   unmasked. The user agent is Core's (DEC-83). Address relay is the host's
//!   ([`Session::addr_relay`]): the address and multi sessions install P2a-net's
//!   `NodeAddressRelay` (`p2p_addr::node_address_relay`, P2a-net D29); `p2p_single` and the
//!   `tests/p2p_node.rs` sessions keep the [`addr_send_timer`] stand-in (D62).
//! - The call log (AddrMan and time-data calls) is not compared: SB6's AddrMan reports no calls.
//! - The send queue's and transport's charges and the queue length are SB6's private state.
//! - A peer that reads at once: Core's session writes to it only at socket-handler turns (Dash's
//!   `PushMessage` has no optimistic write), the network core as soon as a message is queued, so
//!   `nSendBytes` and what the peer has read are compared only for peers that read when told.
//! - `InactivityCheck` is SB6's and has no synchronous entry yet, so the comparison ends before
//!   the first tick that runs it.
//! - Block download is P3's: the comparison ends before the step in which Core first asks a
//!   peer for a block, and `pindexLastCommonBlock`, which `FindNextBlocksToDownload` sets even
//!   when it asks for nothing, is not compared (D51). Requests for announced objects (a `getdata` of no block type) are P5a's
//!   and are not compared.
//!
//! Only V1 peers are modelled; a script with a V2 peer is out of domain.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use dash_consensus::BlockId;
use dash_oracle_sys::{
    Chain, ConnType, Draw, ErrorKind, NodeStateData, Oracle, PeerData, PeerOutcome, SendMode,
    Service, SessionConfig, SessionOutcome, Substitution, TICK_INACTIVITY, TICK_MAINTENANCE,
    Transport,
};
use dash_p2p::addr::AddrRelay;
use dash_p2p::conn::Permissions;
use dash_p2p::rng::{FastRandom, RandSource, StreamRand, exponential_rand};
use dash_p2p::{
    Address, ConnectionType, NetAddr, NetCore, Network, PeerHandle, PeerId, ServiceFlags,
};
use dash_p2p_core::UserAgent;
use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};
use dash_primitives::{AddrV2, BlockHash, BlockHeader};
use dash_types::codec::BaseCodec;
use dashd_p2p::Registry;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::headers::MAX_BLOCKS_TO_ANNOUNCE;
use dashd_p2p::processing::logic::{
    AVG_ADDRESS_BROADCAST_INTERVAL_US, PeerMap, ProcessorOptions, Seams,
};
use dashd_p2p::processing::peer::{PeerState, WantsDsq};
use dashd_p2p::processing::registry::HandlerCx;
use dashd_p2p::processing::seams::{AddressRelay, ChainAccess, NoAddressRelay};
use dashd_p2p::sim::{Sim, SimChain, SimPeer, V1_HEADER};

use crate::{Compared, Outcome, divergence};

/// A scripted peer.
#[derive(Clone, Debug)]
pub struct PeerSpec {
    pub conn: ConnType,
    pub addr: Service,
    pub send_mode: SendMode,
}

impl PeerSpec {
    fn controlled(&self) -> bool {
        self.send_mode == SendMode::Controlled
    }
}

/// One session step.
#[derive(Clone, Debug)]
pub enum Step {
    /// Peer `peer` sends `command`, after the substitutions (`src_peer` is a script index).
    Deliver {
        peer: usize,
        command: &'static str,
        payload: Vec<u8>,
        subs: Vec<Substitution>,
    },
    /// A message-handler pass at this mock time.
    Pass(i64),
    /// Core's timers at this mock time (`TICK_*` bits).
    Tick(i64, u32),
    /// Peer `peer` reads at most `max` bytes of what the node wrote.
    Drain {
        peer: usize,
        max: u64,
    },
}

/// Builds the node leg's address relay over the leg's own chain, so `MaybeSendAddr`'s
/// `IsInitialBlockDownload()` reads the chain the sim validates on (P2a-net's
/// `NodeAddressRelay`; the host installs it, so this crate needs no `dashd-net`).
pub type AddrRelayFactory = fn(Arc<dyn ChainAccess>) -> Box<dyn AddressRelay>;

/// The core-only branch's stand-in for P2a-net's relay (D62): `MaybeSendAddr`'s
/// `m_next_addr_send` timer and nothing else. Nothing relays into `m_addrs_to_send`, so Core
/// would send nothing either; it cannot hide relay, because the comparison sees every `addr`
/// frame, the queue and the draws: a session where Core relays, answers `getaddr` or
/// self-announces (outside IBD) diverges.
pub fn addr_send_timer(_chain: Arc<dyn ChainAccess>) -> Box<dyn AddressRelay> {
    Box::new(SendTimer)
}

struct SendTimer;

impl AddressRelay for SendTimer {
    fn relay(&mut self, _: &NetCore, _: &PeerMap, _: PeerId, _: &[(Address, bool)]) {}

    // Core: net_processing.cpp:6186-6215 MaybeSendAddr (the self-announcement is P2a-net's)
    fn send_section(
        &mut self,
        core: &NetCore,
        _peer: &PeerHandle,
        relay: &mut AddrRelay,
        _our_services: ServiceFlags,
        now_us: i64,
    ) {
        if !relay.enabled || now_us <= relay.next_send_us {
            return;
        }
        const AVERAGE_S: i64 = AVG_ADDRESS_BROADCAST_INTERVAL_US / 1_000_000;
        relay.next_send_us = exponential_rand(now_us, AVERAGE_S, core.get_rand(1 << 48));
    }

    fn on_getaddr(&mut self, _: &mut HandlerCx<'_>) {}
}

/// A session script.
#[derive(Clone, Debug)]
pub struct Session {
    /// Keys Core's draws.
    pub seed: u64,
    /// Our services.
    pub our_services: u64,
    /// The mock time the session starts at.
    pub mocktime: i64,
    /// Whether Core runs `CheckBlockIndex` after each header (O1c's `SessionConfig`); it only
    /// asserts, so no compared output depends on it ([`check_block_index`]).
    pub check_block_index: bool,
    pub peers: Vec<PeerSpec>,
    pub steps: Vec<Step>,
    /// Blocks each connected as the tip before the first peer (O1c's `SessionConfig::blocks`);
    /// the node's chain connects their headers (P2b: `getheaders` serving reads them).
    pub blocks: Vec<Vec<u8>>,
    /// `-maxtipage` in seconds on both legs (Core's chainstate, the node's `ProcessorOptions`
    /// and validating chain); `None` is the 6 h default (P2a-net BL-203).
    pub max_tip_age: Option<i64>,
    /// The node's address relay; `None` is [`NoAddressRelay`] (nothing relayed, sent or
    /// answered), which agrees with Core only while no address reaches it.
    pub addr_relay: Option<AddrRelayFactory>,
}

/// A peer's outcome after a step, projected as the module docs say.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerView {
    /// The messages it received in the step: everything pushed to a peer that reads at once,
    /// what it decoded for one that reads when told.
    sent: Vec<(String, Vec<u8>)>,
    /// Its `Misbehaving` lines, with the peer id taken out.
    misbehaving: Vec<String>,
    removed: bool,
    flags: PeerFlags,
    /// `(nSendBytes, bytes read, messages decoded)`, for peers that read when told.
    read: Option<(u64, u64, u64)>,
    peer: Option<PeerFields>,
    state: Option<StateFields>,
}

/// `CNode`'s fields.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerFlags {
    disconnect: bool,
    successfully_connected: bool,
    version: i32,
    common_version: i32,
    services: u64,
    transport: u8,
    pause_send: bool,
    pause_recv: bool,
    peer_unread_bytes: u64,
}

/// net_processing's `Peer`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PeerFields {
    misbehavior_score: i32,
    should_discourage: bool,
    their_services: u64,
    starting_height: i32,
    ping_nonce_sent: u64,
    ping_start_us: i64,
    ping_queued: bool,
    wants_recsigs: bool,
    blocks_for_inv_relay: Vec<[u8; 32]>,
    blocks_for_headers_relay: Vec<[u8; 32]>,
    continuation_block: [u8; 32],
    addr_relay_enabled: bool,
    /// `m_addrs_to_send`, in order.
    addrs_to_send: Vec<Addr>,
    has_addr_known: bool,
    getaddr_sent: bool,
    getaddr_recvd: bool,
    wants_addrv2: bool,
    next_addr_send_us: i64,
    next_local_addr_send_us: i64,
    addr_token_bucket: u64,
    addr_token_timestamp_us: i64,
    addr_rate_limited: u64,
    addr_processed: u64,
    inv_triggered_getheaders_before_sync: bool,
    last_getheaders_us: i64,
    wants_dsq: u8,
    can_tx_relay: bool,
    relay_txs: bool,
}

/// A block reference: hash and height, or zero and -1.
type Block = ([u8; 32], i32);

/// A `CAddress`: `nTime`, `nServices`, the BIP155 network id and address bytes, and the port.
type Addr = (u32, u64, u8, Vec<u8>, u16);

/// One `GetRandBytes` (`false`) or `GetStrongRandBytes` (`true`) call and the bytes it took.
type RandCall = (bool, usize);

/// `CNodeState`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct StateFields {
    best_known_block: Block,
    last_unknown_block: [u8; 32],
    best_header_sent: Block,
    unconnecting_headers: i32,
    sync_started: bool,
    headers_sync_timeout_us: i64,
    preferred_download: bool,
    prefer_headers: bool,
    prefer_headers_compressed: bool,
    chain_sync_timeout_s: i64,
    chain_sync_work_header: Block,
    chain_sync_sent_getheaders: bool,
    chain_sync_protect: bool,
    last_block_announcement: i64,
    inbound: bool,
}

/// The session's outcome after a step: PeerManager's globals and the tip.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionView {
    sync_started: i32,
    preferred_download_peers: i32,
    outbound_peers_protected: i32,
    best_height: i32,
    initial_sync_finished: bool,
    last_tip_update_s: i64,
    stale_tip_check_time_s: i64,
    last_block_inv_triggering_headers_sync: [u8; 32],
    tip: Block,
    /// The step's draws from O1c's stream, in order.
    draws: Vec<RandCall>,
}

/// A step's outcome on one side: `None` when the step was skipped (a removed peer, or a
/// substitution with nothing to copy).
type StepView = Option<(Vec<PeerView>, SessionView)>;

/// Where a `version` payload's user agent lies (its length byte is at 80).
// Core: net_processing.cpp:1561-1593 PushNodeVersion
fn user_agent(version: &[u8]) -> Option<std::ops::Range<usize>> {
    let len = usize::from(*version.get(80)?);
    (version.len() >= 81 + len).then_some(81..81 + len)
}

/// `line` without its `peer=<id> `.
fn misbehaving_line(line: &str, id: i64) -> String {
    line.replacen(&format!("peer={id} "), "peer=_ ", 1)
}

fn core_block(b: &dash_oracle_sys::BlockRef) -> Block {
    (b.hash, b.height)
}

/// Core's side of one peer, given the messages it received in the step.
fn core_peer(o: &PeerOutcome, id: i64, sent: Vec<(String, Vec<u8>)>, controlled: bool) -> PeerView {
    PeerView {
        sent,
        misbehaving: o.misbehaving.iter().map(|l| misbehaving_line(l, id)).collect(),
        removed: o.removed,
        flags: PeerFlags {
            disconnect: o.disconnect,
            successfully_connected: o.successfully_connected,
            version: o.version,
            common_version: o.common_version,
            services: o.services,
            transport: o.transport,
            pause_send: o.pause_send,
            pause_recv: o.pause_recv,
            peer_unread_bytes: o.peer_unread_bytes,
        },
        read: controlled.then_some((o.send_bytes, o.peer_read_bytes, o.peer_decoded_messages)),
        peer: o.peer.as_ref().map(core_peer_fields),
        state: o.state.as_ref().map(core_state_fields),
    }
}

fn core_addr(a: &dash_oracle_sys::Address) -> Addr {
    let s = &a.service;
    (a.time, a.services, s.addr.network, s.addr.bytes.clone(), s.port)
}

/// One `GetRandBytes` (`false`) or `GetStrongRandBytes` (`true`) call and the bytes O1c's stream
/// gave it.
type RandDraw = (bool, Vec<u8>);

/// O1c's draw log as the calls that took bytes from the stream.
fn core_draws(draws: &[Draw]) -> Vec<RandDraw> {
    draws
        .iter()
        .filter_map(|d| match d {
            Draw::RandBytes(b) => Some((false, b.clone())),
            Draw::StrongRandBytes(b) => Some((true, b.clone())),
            Draw::NewContext {
                ..
            }
            | Draw::Keystream {
                ..
            } => None,
        })
        .collect()
}

fn calls(draws: &[RandDraw]) -> Vec<RandCall> {
    draws.iter().map(|(strong, b)| (*strong, b.len())).collect()
}

/// The draws of Core's setup that `NetCore::new` makes too, in its order (D38, D60): each of the
/// two AddrMans (the missing `peers.dat` builds a second) draws its key and then its `mapAddr`
/// salts, two contexts; `DumpPeerAddresses` draws a temporary file name, which the node does not;
/// then BanMan's bloom tweak, `nSeed1` and `nSeed0`. The AddrMans are found by their 32-byte keys.
/// The seeds' places were checked against the `version` nonce Core derives from them. Last come
/// `m_object_request`'s two salts (`PeerLogic::new`, P5a D7), the setup's last two contexts:
/// `PeerManager::make` builds the tracker after its two bloom filters, and nothing in the setup
/// draws after it (checked with a breakpoint on `FastRandomContext::RandomSeed`).
// O1c: src/test/util/setup_common.cpp:262-300 (LoadAddrman, BanMan, the seeds); addrdb.cpp:58;
// txrequest.cpp:113-115
fn netcore_setup_draws(draws: &[Draw]) -> Option<Vec<RandDraw>> {
    // Each self-seeding context's seed draw and how many keystream bytes it then produced.
    let mut contexts: Vec<(RandDraw, usize)> = Vec::new();
    let mut seeding: Option<u32> = None;
    let mut by_ctx: HashMap<u32, usize> = HashMap::new();
    for d in draws {
        match d {
            Draw::NewContext {
                ctx,
                deterministic: false,
            } => seeding = Some(*ctx),
            Draw::RandBytes(b) => {
                if let Some(ctx) = seeding.take() {
                    by_ctx.insert(ctx, contexts.len());
                    contexts.push(((false, b.clone()), 0));
                }
            }
            Draw::Keystream {
                ctx,
                bytes,
            } => {
                if let Some(&i) = by_ctx.get(ctx) {
                    contexts[i].1 += bytes.len();
                }
            }
            Draw::NewContext {
                ..
            }
            | Draw::StrongRandBytes(_) => seeding = None,
        }
    }
    let keys: Vec<usize> =
        contexts.iter().enumerate().filter(|(_, c)| c.1 == 32).map(|(i, _)| i).collect();
    let [first, second, ..] = keys[..] else {
        return None;
    };
    let picked = [
        // Each AddrMan: its key, then its two salts.
        first,
        first + 1,
        first + 2,
        second,
        second + 1,
        second + 2,
        // `second + 3` is `DumpPeerAddresses`' name; then the tweak, `nSeed1` and `nSeed0`.
        second + 4,
        second + 5,
        second + 6,
        // `m_object_request`'s `m_k0` and `m_k1`.
        contexts.len().checked_sub(2)?,
        contexts.len() - 1,
    ];
    picked.iter().map(|&i| contexts.get(i).map(|c| c.0.clone())).collect()
}

fn core_peer_fields(p: &PeerData) -> PeerFields {
    PeerFields {
        misbehavior_score: p.misbehavior_score,
        should_discourage: p.should_discourage,
        their_services: p.their_services,
        starting_height: p.starting_height,
        ping_nonce_sent: p.ping_nonce_sent,
        ping_start_us: p.ping_start_us,
        ping_queued: p.ping_queued,
        wants_recsigs: p.wants_recsigs,
        blocks_for_inv_relay: p.blocks_for_inv_relay.clone(),
        // The port keeps only the last hash past `MAX_BLOCKS_TO_ANNOUNCE`, all section 7 reads
        // then (P3 D44): compare Core's vector in that form.
        blocks_for_headers_relay: relay_list_view(&p.blocks_for_headers_relay),
        continuation_block: p.continuation_block,
        addr_relay_enabled: p.addr_relay_enabled,
        addrs_to_send: p.addrs_to_send.iter().map(core_addr).collect(),
        has_addr_known: p.has_addr_known,
        getaddr_sent: p.getaddr_sent,
        getaddr_recvd: p.getaddr_recvd,
        wants_addrv2: p.wants_addrv2,
        next_addr_send_us: p.next_addr_send_us,
        next_local_addr_send_us: p.next_local_addr_send_us,
        addr_token_bucket: p.addr_token_bucket.to_bits(),
        addr_token_timestamp_us: p.addr_token_timestamp_us,
        addr_rate_limited: p.addr_rate_limited,
        addr_processed: p.addr_processed,
        inv_triggered_getheaders_before_sync: p.inv_triggered_getheaders_before_sync,
        last_getheaders_us: p.last_getheaders,
        wants_dsq: u8::from(p.wants_dsq),
        can_tx_relay: p.can_tx_relay,
        relay_txs: p.relay_txs,
    }
}

fn core_state_fields(s: &NodeStateData) -> StateFields {
    StateFields {
        best_known_block: core_block(&s.best_known_block),
        last_unknown_block: s.last_unknown_block,
        best_header_sent: core_block(&s.best_header_sent),
        unconnecting_headers: s.unconnecting_headers,
        sync_started: s.sync_started,
        headers_sync_timeout_us: s.headers_sync_timeout_us,
        preferred_download: s.preferred_download,
        prefer_headers: s.prefer_headers,
        prefer_headers_compressed: s.prefer_headers_compressed,
        chain_sync_timeout_s: s.chain_sync_timeout_s,
        chain_sync_work_header: core_block(&s.chain_sync_work_header),
        chain_sync_sent_getheaders: s.chain_sync_sent_getheaders,
        chain_sync_protect: s.chain_sync_protect,
        last_block_announcement: s.last_block_announcement,
        inbound: s.inbound,
    }
}

fn core_session_view(s: &SessionOutcome) -> SessionView {
    let g = &s.globals;
    SessionView {
        sync_started: g.sync_started,
        preferred_download_peers: g.preferred_download_peers,
        outbound_peers_protected: g.outbound_peers_protected,
        best_height: g.best_height,
        initial_sync_finished: g.initial_sync_finished,
        last_tip_update_s: g.last_tip_update_s,
        stale_tip_check_time_s: g.stale_tip_check_time_s,
        last_block_inv_triggering_headers_sync: g.last_block_inv_triggering_headers_sync,
        tip: core_block(&s.tip),
        draws: calls(&core_draws(&s.draws)),
    }
}

/// The types of a `getdata`'s entries (`None` for another message).
fn getdata_types(cmd: &str, payload: &[u8]) -> Option<Vec<u32>> {
    let (n, entries) = match (cmd, payload) {
        ("getdata", [0xfd, a, b, rest @ ..]) => (usize::from(u16::from_le_bytes([*a, *b])), rest),
        ("getdata", [n, rest @ ..]) if *n < 0xfd => (usize::from(*n), rest),
        _ => return None,
    };
    let (entries, _) = entries.as_chunks::<36>();
    Some(entries.iter().take(n).map(|e| u32::from_le_bytes([e[0], e[1], e[2], e[3]])).collect())
}

/// `MSG_BLOCK`, `MSG_FILTERED_BLOCK`, `MSG_CMPCT_BLOCK`.
fn is_block_type(t: u32) -> bool {
    matches!(t, 2 | 3 | 20)
}

/// Whether a message Core sent asks for a block (P3's download).
fn requests_blocks(cmd: &str, payload: &[u8]) -> bool {
    getdata_types(cmd, payload).is_some_and(|types| types.into_iter().any(is_block_type))
}

/// Whether a message is a request for announced objects only (P5a's; D46).
fn requests_objects(cmd: &str, payload: &[u8]) -> bool {
    getdata_types(cmd, payload)
        .is_some_and(|types| !types.is_empty() && !types.into_iter().any(is_block_type))
}

/// Whether a message is left out of the comparison: a request for announced objects (D46).
fn left_out(cmd: &str, payload: &[u8]) -> bool {
    requests_objects(cmd, payload)
}

/// A message as compared, or `None` when it is not compared (D46).
fn compared(cmd: &str, payload: &[u8]) -> Option<(String, Vec<u8>)> {
    (!left_out(cmd, payload)).then(|| (cmd.to_owned(), payload.to_vec()))
}

/// Core's run of a script: its step views, and what the node takes from it.
struct CoreRun {
    /// The draws the node replays before step 0 (D60): `NetCore::new`'s, then each peer's.
    setup_draws: Vec<Vec<RandDraw>>,
    /// The compared steps' views: up to the first inactivity tick or block request.
    views: Vec<StepView>,
    /// Each compared step's draws.
    step_draws: Vec<Vec<RandDraw>>,
    /// The user agent of Core's first `version`.
    user_agent: Option<Vec<u8>>,
}

/// Core's pushes to a peer that it has not yet been shown as receiving.
#[derive(Default)]
struct Pushed {
    pending: VecDeque<(String, Vec<u8>)>,
    /// The messages a peer that reads when told has decoded so far.
    decoded: u64,
}

fn oracle_error(_: dash_oracle_sys::Error) -> &'static str {
    "oracle error"
}

/// Whether a step ran: an `Argument` error is Core skipping it (a removed peer, a substitution
/// with nothing to copy).
fn ran<T>(r: dash_oracle_sys::Result<T>) -> Result<bool, &'static str> {
    match r {
        Ok(_) => Ok(true),
        Err(e) if e.kind == ErrorKind::Argument => Ok(false),
        Err(e) => Err(oracle_error(e)),
    }
}

/// Core's `CheckBlockIndex` for a session of a P2a-core target (D55): off in a fuzzing build
/// (`--cfg fuzzing`, run.sh's) unless libFuzzer only replays (`-runs=0`, DEC-186's replays
/// before and after a certification window), on everywhere else. It costs Core O(n²) per header
/// run and only asserts, so fuzzing skips it while replays keep its assertions.
pub fn check_block_index() -> bool {
    static REPLAY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    !cfg!(fuzzing) || *REPLAY.get_or_init(|| std::env::args_os().any(|a| a == "-runs=0"))
}

fn core_run(oracle: &Oracle, s: &Session) -> Result<Option<CoreRun>, &'static str> {
    let config = SessionConfig {
        chain: Chain::regtest(),
        our_services: s.our_services,
        mocktime: s.mocktime,
        rng_seed: s.seed,
        blocks: s.blocks.clone(),
        min_chain_work: None,
        max_tip_age: s.max_tip_age,
        check_block_index: s.check_block_index,
    };
    let Some(mut session) = oracle.peer_session_new(&config).map_err(oracle_error)? else {
        return Ok(None);
    };
    let setup = session.session_outcome().map_err(oracle_error)?;
    let mut setup_draws = vec![netcore_setup_draws(&setup.draws).ok_or("setup draws")?];
    let mut ids = Vec::with_capacity(s.peers.len());
    for p in &s.peers {
        ids.push(
            session
                .add_peer_with(p.conn, Transport::V1, p.send_mode, &p.addr)
                .map_err(oracle_error)?,
        );
        setup_draws.push(core_draws(&session.session_outcome().map_err(oracle_error)?.draws));
    }
    let mut pushed: Vec<Pushed> = s.peers.iter().map(|_| Pushed::default()).collect();
    let mut run = CoreRun {
        setup_draws,
        views: Vec::new(),
        step_draws: Vec::new(),
        user_agent: None,
    };
    for step in &s.steps {
        let done = match step {
            Step::Tick(_, what) if what & TICK_INACTIVITY != 0 => break,
            Step::Deliver {
                peer,
                command,
                payload,
                subs,
            } => {
                let subs: Vec<Substitution> = subs
                    .iter()
                    .map(|sub| Substitution {
                        src_peer: ids[sub.src_peer as usize],
                        ..sub.clone()
                    })
                    .collect();
                ran(session.deliver(ids[*peer], command, payload, &subs))?
            }
            // Nothing in Core skips these: an `Argument` error is the script's.
            Step::Pass(t) => session.send_pass(*t).map(|_| true).map_err(oracle_error)?,
            Step::Tick(t, what) => session.tick(*t, *what).map(|_| true).map_err(oracle_error)?,
            Step::Drain {
                peer,
                max,
            } => ran(session.drain(ids[*peer], *max))?,
        };
        if !done {
            run.views.push(None);
            run.step_draws.push(Vec::new());
            continue;
        }
        let mut peers = Vec::with_capacity(ids.len());
        for ((&id, spec), p) in ids.iter().zip(&s.peers).zip(&mut pushed) {
            let o = session.peer_outcome(id).map_err(oracle_error)?;
            if run.user_agent.is_none() {
                run.user_agent = o
                    .sent
                    .iter()
                    .find(|(c, _)| c == "version")
                    .and_then(|(_, m)| user_agent(m).map(|ua| m[ua].to_vec()));
            }
            p.pending.extend(o.sent.iter().cloned());
            let n = if spec.controlled() {
                let n = o.peer_decoded_messages.saturating_sub(p.decoded);
                p.decoded += n;
                usize::try_from(n).unwrap_or(usize::MAX).min(p.pending.len())
            } else {
                p.pending.len()
            };
            let sent = p.pending.drain(..n).filter_map(|(c, m)| compared(&c, &m)).collect();
            peers.push(core_peer(&o, id, sent, spec.controlled()));
            // A message left out of `sent` (an object request) would still be in a controlled
            // peer's byte and message counts (D49).
            let ends = |(c, m): &(String, Vec<u8>)| {
                requests_blocks(c, m) || (spec.controlled() && left_out(c, m))
            };
            if o.sent.iter().any(ends) {
                return Ok(Some(run));
            }
        }
        let session_outcome = session.session_outcome().map_err(oracle_error)?;
        run.step_draws.push(core_draws(&session_outcome.draws));
        run.views.push(Some((peers, core_session_view(&session_outcome))));
    }
    Ok(Some(run))
}

/// `Misbehaving` lines the processor logged, by peer id (a thread-local `tracing` subscriber on
/// the thread that runs the passes).
#[derive(Default)]
struct Misbehaving(Mutex<BTreeMap<i64, Vec<String>>>);

impl Misbehaving {
    fn lines(&self) -> MutexGuard<'_, BTreeMap<i64, Vec<String>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Peer `id`'s lines since the last call, without the id.
    fn take(&self, id: i64) -> Vec<String> {
        let lines = self.lines().remove(&id).unwrap_or_default();
        lines.iter().map(|l| misbehaving_line(l, id)).collect()
    }
}

struct MessageVisitor<'a>(&'a mut String);

impl tracing::field::Visit for MessageVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            *self.0 = format!("{value:?}");
        }
    }
}

const MARKER: &str = "Misbehaving: peer=";

impl tracing::Subscriber for Misbehaving {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && meta.target() == "net"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut line = String::new();
        event.record(&mut MessageVisitor(&mut line));
        let Some(rest) = line.find(MARKER).map(|at| &line[at..]) else {
            return;
        };
        let id = rest[MARKER.len()..].split(' ').next().and_then(|n| n.parse().ok());
        if let Some(id) = id {
            self.lines().entry(id).or_default().push(rest.to_owned());
        }
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

fn conn_type(c: ConnType) -> ConnectionType {
    match c {
        ConnType::Inbound => ConnectionType::Inbound,
        ConnType::OutboundFullRelay => ConnectionType::OutboundFullRelay,
        ConnType::Manual => ConnectionType::Manual,
        ConnType::Feeler => ConnectionType::Feeler,
        ConnType::BlockRelay => ConnectionType::BlockRelay,
        ConnType::AddrFetch => ConnectionType::AddrFetch,
    }
}

/// A script address as the network core's; `None` outside the BIP155 networks.
fn service(s: &Service) -> Option<dash_p2p::Service> {
    let b = &s.addr.bytes;
    let addr = match s.addr.network {
        1 => AddrV2::Ipv4(b.as_slice().try_into().ok()?),
        2 => AddrV2::Ipv6(b.as_slice().try_into().ok()?),
        4 => AddrV2::TorV3(b.as_slice().try_into().ok()?),
        5 => AddrV2::I2p(b.as_slice().try_into().ok()?),
        6 => AddrV2::Cjdns(b.as_slice().try_into().ok()?),
        _ => return None,
    };
    Some(dash_p2p::Service {
        addr: NetAddr::from_addrv2(&addr),
        port: s.port,
    })
}

fn hash(h: &BlockHash) -> [u8; 32] {
    *h.as_bytes()
}

/// One scripted peer on the node's side.
struct NodePeer {
    sim: SimPeer,
    controlled: bool,
    /// The far end's bytes not yet a whole frame, and its totals.
    partial: Vec<u8>,
    read_bytes: u64,
    decoded: u64,
    /// The last message of each command the node sent it, unmasked (the substitutions' source).
    last_sent: HashMap<String, Vec<u8>>,
    /// Its view when it was removed (what it received and its lines until they are shown).
    removed: Option<PeerView>,
}

/// The node's `GetRandBytes` (D60): in each phase (the network core's setup, each peer's attach,
/// each step) a call gets the bytes Core's call in the same place drew from O1c's stream, and is
/// logged. Once a call has no such twin (another kind or length, or one call more), the phase's
/// calls take `StreamRand::from_seed(seed)`'s bytes instead, and the logs differ.
struct ReplayStream {
    fallback: StreamRand,
    phase: Mutex<Phase>,
}

#[derive(Default)]
struct Phase {
    core: VecDeque<RandDraw>,
    calls: Vec<RandCall>,
}

impl RandSource for ReplayStream {
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        let mut phase = self.phase();
        phase.calls.push((strong, out.len()));
        match phase.core.pop_front() {
            Some((s, b)) if s == strong && b.len() == out.len() => out.copy_from_slice(&b),
            _ => {
                phase.core.clear();
                self.fallback.rand_bytes(out, strong);
            }
        }
    }
}

impl ReplayStream {
    fn phase(&self) -> MutexGuard<'_, Phase> {
        self.phase.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts a phase on Core's `draws` for it, dropping the calls of the one before.
    fn start(&self, draws: &[RandDraw]) {
        let mut phase = self.phase();
        phase.core = draws.iter().cloned().collect();
        phase.calls.clear();
    }

    /// The calls of the current phase so far.
    fn take_calls(&self) -> Vec<RandCall> {
        std::mem::take(&mut self.phase().calls)
    }
}

struct NodeRun {
    sim: Sim,
    peers: Vec<NodePeer>,
    log: Arc<Misbehaving>,
    draws: Arc<ReplayStream>,
    /// The node's calls before step 0, by [`CoreRun::setup_draws`]' phases.
    setup_draws: Vec<Vec<RandCall>>,
}

impl NodeRun {
    /// The node set up as Core's session, speaking with Core's user agent and drawing what its
    /// draws (D60).
    fn new(s: &Session, core: &CoreRun) -> Option<Self> {
        let user_agent = core.user_agent.as_deref();
        let params =
            Arc::new(ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().ok()?);
        let mut identity = local_identity(&params, &IdentityOptions::new()).ok()?;
        if let Some(ua) = user_agent {
            identity.user_agent = UserAgent::new(ua.to_vec()).ok()?;
        }
        let mut opts = ProcessorOptions::default();
        if let Some(age) = s.max_tip_age {
            opts.max_tip_age = age;
        }
        let max_tip_age = opts.max_tip_age;
        let chain_params = Arc::clone(&params);
        let draws = Arc::new(ReplayStream {
            fallback: StreamRand::from_seed(s.seed),
            phase: Mutex::default(),
        });
        let mut phases = core.setup_draws.iter();
        draws.start(phases.next()?);
        let rand: Arc<dyn RandSource> = draws.clone();
        let sim = Sim::with_chain_seams(
            params,
            identity,
            opts,
            Registry::new(),
            s.mocktime,
            |clock| SimChain::validating(chain_params, clock.clone(), max_tip_age),
            |chain| Seams {
                addr_relay: match s.addr_relay {
                    Some(relay) => relay(chain.clone()),
                    None => Box::new(NoAddressRelay),
                },
                ..Seams::default()
            },
            |cfg| {
                cfg.local_services = ServiceFlags(s.our_services);
                // Core's `-peertimeout`; `sim` lengthens it.
                cfg.peer_timeout = 60;
                // Core's AddrMan is not deterministic: it draws its key (D60).
                cfg.deterministic = false;
                // O1c's session runs no AppInitMain, so Core's `g_reachable_nets` keeps every
                // network (netbase.h:143): the removals of CJDNS (init.cpp:1862, no
                // `-cjdnsreachable`), Onion (:1948, no proxy) and I2P (:2750, no `-i2psam`)
                // never run (D68).
                cfg.reachable_overlays = vec![Network::Onion, Network::I2p, Network::Cjdns];
                // O1c's sessions run `InactivityCheck` only on a tick (TICK_INACTIVITY), so the
                // sweep never does; the comparison ends before such a tick (module docs).
                *cfg = cfg.clone().with_manual_inactivity_checks().with_rand_source(rand);
            },
        );
        for block in &s.blocks {
            let header = BlockHeader::decode(&mut block.get(..80)?).ok()?;
            let tip = sim.chain.add(&[header]).id;
            sim.chain.set_tip(tip);
        }
        let mut setup_draws = vec![draws.take_calls()];
        let mut peers = Vec::with_capacity(s.peers.len());
        for (p, phase) in s.peers.iter().zip(phases) {
            draws.start(phase);
            // `addr_bind` is `Service::default()` (`::`, port 0), Core's session's `CAddress{}`.
            let sim = sim.attach_at(
                service(&p.addr)?,
                conn_type(p.conn),
                Permissions::NONE,
                ServiceFlags(0),
                false,
                p.controlled(),
            );
            peers.push(NodePeer {
                sim,
                controlled: p.controlled(),
                partial: Vec::new(),
                read_bytes: 0,
                decoded: 0,
                last_sent: HashMap::new(),
                removed: None,
            });
            setup_draws.push(draws.take_calls());
        }
        Some(NodeRun {
            sim,
            peers,
            log: Arc::default(),
            draws,
            setup_draws,
        })
    }

    fn id(&self, peer: usize) -> PeerId {
        self.peers[peer].sim.handle.id()
    }

    fn live(&self, peer: usize) -> bool {
        self.peers[peer].removed.is_none()
    }

    /// Runs `step`; `false` when the node skips it as Core does.
    fn step(&mut self, step: &Step) -> bool {
        match step {
            Step::Deliver {
                peer,
                command,
                payload,
                subs,
            } => {
                if !self.live(*peer) {
                    return false;
                }
                let Some(payload) = self.substitute(payload, subs) else {
                    return false;
                };
                let p = &self.peers[*peer].sim;
                // Core: a peer marked to disconnect is no longer read; the message is lost.
                if !p.handle.disconnect_requested() {
                    self.sim.deliver(p, &[(command, payload)]);
                }
            }
            Step::Pass(t) => {
                self.sim.clock.set_mock_time(*t);
                // Core's pass makes a `FastRandomContext{}`, which seeds itself from the stream
                // only to shuffle two or more peers.
                let mut rng = FastRandom::lazy(Arc::clone(self.sim.core.rand()));
                self.sim.pass_with(&mut rng);
                self.remove_disconnected();
            }
            Step::Tick(t, what) => {
                debug_assert_eq!(what & TICK_INACTIVITY, 0, "the comparison ends first");
                self.sim.clock.set_mock_time(*t);
                if what & TICK_MAINTENANCE != 0 {
                    self.sim.lp.logic_mut().check_for_stale_tip_and_evict_peers();
                }
                self.remove_disconnected();
            }
            Step::Drain {
                peer,
                max,
            } => {
                if !self.live(*peer) {
                    return false;
                }
                let p = &self.peers[*peer];
                if p.controlled {
                    p.sim.read(*max);
                }
            }
        }
        true
    }

    /// `payload` with the substitutions' bytes copied in, as Core's `deliver` does; `None` when
    /// one has nothing to copy.
    fn substitute(&self, payload: &[u8], subs: &[Substitution]) -> Option<Vec<u8>> {
        let mut payload = payload.to_vec();
        for sub in subs {
            let from = self.peers[sub.src_peer as usize].last_sent.get(&sub.src_command)?;
            let (s, d, n) = (sub.src_offset as usize, sub.dst_offset as usize, sub.len as usize);
            payload.get_mut(d..d + n)?.copy_from_slice(from.get(s..s + n)?);
        }
        Some(payload)
    }

    /// Core's `RemoveDisconnected`: every marked peer's view is kept, then the peer goes.
    fn remove_disconnected(&mut self) {
        let marked: Vec<usize> = (0..self.peers.len())
            .filter(|&i| self.live(i) && self.peers[i].sim.handle.disconnect_requested())
            .collect();
        if marked.is_empty() {
            return;
        }
        let views: Vec<PeerView> = marked.iter().map(|&i| self.peer_view(i, true)).collect();
        self.sim.remove_disconnected();
        for (i, view) in marked.into_iter().zip(views) {
            self.peers[i].removed = Some(view);
        }
    }

    /// What peer `i` received since the last call.
    fn received(&mut self, i: usize) -> Vec<(String, Vec<u8>)> {
        let p = &mut self.peers[i];
        let frames = if p.controlled {
            let bytes = p.sim.sock.drain();
            p.read_bytes += bytes.len() as u64;
            p.partial.extend(bytes);
            let frames = self.sim.v1_frames(&p.partial);
            let used: usize = frames.iter().map(|(_, m)| V1_HEADER + m.len()).sum();
            p.partial.drain(..used);
            p.decoded += frames.len() as u64;
            frames
        } else {
            self.sim.sent(&p.sim)
        };
        frames
            .into_iter()
            .filter_map(|(c, m)| {
                let shown = compared(c.as_str(), &m);
                p.last_sent.insert(c.as_str().to_owned(), m);
                shown
            })
            .collect()
    }

    /// Peer `i`'s view now (`removed`: as Core reports it once removed).
    fn peer_view(&mut self, i: usize, removed: bool) -> PeerView {
        let sent = self.received(i);
        let id = self.id(i);
        let p = &self.peers[i];
        let h = &p.sim.handle;
        let chain: &dyn ChainAccess = &*self.sim.chain;
        let (peer, state) = self
            .sim
            .shared
            .with_state(id, |st| (node_peer_fields(st, h), node_state_fields(st, h, chain)))
            .unzip();
        PeerView {
            sent,
            misbehaving: self.log.take(id),
            removed,
            flags: PeerFlags {
                disconnect: h.disconnect_requested(),
                successfully_connected: h.successfully_connected(),
                version: h.version(),
                common_version: h.common_version().0,
                services: h.version_services().0,
                transport: transport_type(h),
                pause_send: h.pause_send(),
                pause_recv: h.pause_recv(),
                peer_unread_bytes: p.sim.sock.in_flight() as u64,
            },
            read: p.controlled.then_some((h.send_bytes(), p.read_bytes, p.decoded)),
            peer,
            state,
        }
    }

    fn view(&mut self) -> (Vec<PeerView>, SessionView) {
        let peers = (0..self.peers.len())
            .map(|i| match self.peers[i].removed.as_mut() {
                // What it received and its lines show once, in the step that removed it.
                Some(v) => {
                    let shown = v.clone();
                    v.sent.clear();
                    v.misbehaving.clear();
                    shown
                }
                None => self.peer_view(i, false),
            })
            .collect();
        let logic = self.sim.lp.logic();
        let c = &logic.counters;
        let tip = self.sim.chain.snapshot().tip;
        let session = SessionView {
            sync_started: c.sync_started,
            preferred_download_peers: c.num_preferred_download,
            outbound_peers_protected: c.outbound_with_protect,
            best_height: tip.tip_height as i32,
            initial_sync_finished: logic.initial_sync_finished(),
            last_tip_update_s: logic.flags().last_tip_update(),
            stale_tip_check_time_s: logic.stale_tip_check_time(),
            last_block_inv_triggering_headers_sync: hash(&c.last_block_inv_triggering_sync),
            tip: (hash(&tip.tip_hash), tip.tip_height as i32),
            draws: self.draws.take_calls(),
        };
        (peers, session)
    }
}

/// The node's `TransportProtocolType`: 0 detecting, 1 V1, 2 V2.
fn transport_type(h: &PeerHandle) -> u8 {
    match h.transport_info().kind {
        dash_p2p_transport::TransportKind::Detecting => 0,
        dash_p2p_transport::TransportKind::V1 => 1,
        dash_p2p_transport::TransportKind::V2 => 2,
    }
}

/// A queued address as O1c reports one (BIP155 network ids, `SerializeV2Stream`'s bytes).
fn node_addr(a: &dash_p2p::Address) -> Addr {
    let (network, bytes) = match a.service.addr.to_addrv2() {
        AddrV2::Ipv4(b) => (1, b.to_vec()),
        AddrV2::Ipv6(b) => (2, b.to_vec()),
        AddrV2::TorV3(b) => (4, b.to_vec()),
        AddrV2::I2p(b) => (5, b.to_vec()),
        AddrV2::Cjdns(b) => (6, b.to_vec()),
        AddrV2::Unknown {
            ..
        } => unreachable!("never queued"),
    };
    (a.time, a.services.0, network, bytes, a.service.port)
}

fn node_peer_fields(st: &PeerState, h: &PeerHandle) -> PeerFields {
    let (inv, headers) = st.blocks_for_relay();
    let (score, discourage) = st.misbehavior();
    let addr = &st.net.addr;
    PeerFields {
        misbehavior_score: score,
        should_discourage: discourage,
        their_services: h.version_services().0,
        starting_height: st.net.starting_height,
        ping_nonce_sent: st.net.ping.nonce_sent,
        ping_start_us: st.net.ping.start_us,
        ping_queued: st.net.ping.queued,
        wants_recsigs: st.wants_recsigs,
        blocks_for_inv_relay: inv.iter().map(hash).collect(),
        blocks_for_headers_relay: headers.as_slice().iter().map(hash).collect(),
        // `getblocks` is P4's; nothing sets it at M2.
        continuation_block: [0; 32],
        addr_relay_enabled: addr.enabled,
        addrs_to_send: addr.to_send.iter().map(node_addr).collect(),
        has_addr_known: addr.known.is_some(),
        getaddr_sent: addr.getaddr_sent,
        getaddr_recvd: addr.getaddr_recvd,
        wants_addrv2: addr.wants_addrv2,
        next_addr_send_us: addr.next_send_us,
        next_local_addr_send_us: addr.next_local_send_us,
        addr_token_bucket: addr.token_bucket.to_bits(),
        addr_token_timestamp_us: addr.token_timestamp_us,
        addr_rate_limited: addr.rate_limited,
        addr_processed: addr.processed,
        inv_triggered_getheaders_before_sync: st.inv_triggered_getheaders_before_sync,
        last_getheaders_us: st.last_getheaders_us,
        // Core's `Peer::WantsDSQ` order: NONE, INV, ALL.
        wants_dsq: match st.wants_dsq {
            WantsDsq::None => 0,
            WantsDsq::Inv => 1,
            WantsDsq::All => 2,
        },
        can_tx_relay: st.tx_relay,
        relay_txs: st.tx_relay && st.relay_txs,
    }
}

/// Core's `m_blocks_for_headers_relay` as the port's `RelayList` keeps it: whole while it holds
/// at most `MAX_BLOCKS_TO_ANNOUNCE` hashes, else only the last.
fn relay_list_view(v: &[[u8; 32]]) -> Vec<[u8; 32]> {
    match v {
        [.., last] if v.len() > MAX_BLOCKS_TO_ANNOUNCE => vec![*last],
        _ => v.to_vec(),
    }
}

/// A block of `chain`'s as Core's snapshot gives it.
fn node_block(chain: &dyn ChainAccess, b: Option<BlockId>) -> Block {
    b.and_then(|id| chain.entry(id)).map_or(([0; 32], -1), |e| (hash(&e.hash), e.height as i32))
}

fn node_state_fields(st: &PeerState, h: &PeerHandle, chain: &dyn ChainAccess) -> StateFields {
    let block = |b| node_block(chain, b);
    StateFields {
        best_known_block: block(st.headers().best_known_block),
        last_unknown_block: st.headers().last_unknown_block.as_ref().map_or([0; 32], hash),
        best_header_sent: block(st.headers().best_header_sent),
        unconnecting_headers: st.unconnecting_headers,
        sync_started: st.sync_started,
        headers_sync_timeout_us: st.headers_sync_timeout_us,
        preferred_download: st.preferred_download,
        prefer_headers: st.prefer_headers,
        prefer_headers_compressed: st.prefer_headers_compressed,
        chain_sync_timeout_s: st.chain_sync.timeout,
        chain_sync_work_header: block(st.chain_sync.work_header),
        chain_sync_sent_getheaders: st.chain_sync.sent_getheaders,
        chain_sync_protect: st.chain_sync.protect,
        last_block_announcement: st.last_block_announcement,
        inbound: h.is_inbound(),
    }
}

/// The node's setup draws and its views of the steps Core's run compares.
fn node_run(s: &Session, core: &CoreRun) -> Option<(Vec<Vec<RandCall>>, Vec<StepView>)> {
    let mut run = NodeRun::new(s, core)?;
    let _log = tracing::subscriber::set_default(Arc::clone(&run.log));
    let views = s.steps.iter().zip(&core.step_draws).map(|(step, draws)| {
        run.draws.start(draws);
        run.step(step).then(|| run.view())
    });
    let views = views.collect();
    Some((std::mem::take(&mut run.setup_draws), views))
}

/// Whether the leg models `s`: V1 peers at BIP155 addresses.
fn in_domain(s: &Session) -> bool {
    s.peers.iter().all(|p| service(&p.addr).is_some())
}

/// The first difference between the two legs: `(step, label, detail)`.
type Difference = (usize, &'static str, String);

/// A field of both legs' views: its label, both values and whether they agree.
type Row<'a> = (&'static str, &'a dyn fmt::Debug, &'a dyn fmt::Debug, bool);

fn row<'a, T: PartialEq + fmt::Debug>(label: &'static str, c: &'a T, n: &'a T) -> Row<'a> {
    (label, c, n, c == n)
}

/// The first difference between the two legs, by field: the setup's draws, then step by step.
fn first_difference(legs: &Legs) -> Option<Difference> {
    let (core, node) = (&legs.core, &legs.node);
    if legs.core_setup_draws != legs.node_setup_draws {
        let detail = format!("core {:?}\nnode {:?}", legs.core_setup_draws, legs.node_setup_draws);
        return Some((0, "setup draws", detail));
    }
    for (i, (c, n)) in core.iter().zip(node).enumerate() {
        let ((cp, cs), (np, ns)) = match (c, n) {
            (None, None) => continue,
            (Some(c), Some(n)) => (c, n),
            _ => {
                return Some((
                    i,
                    "skipped step",
                    format!("core {:?} node {:?}", c.is_some(), n.is_some()),
                ));
            }
        };
        for (k, (c, n)) in cp.iter().zip(np).enumerate() {
            let fields = [
                row("peer sent", &c.sent, &n.sent),
                row("peer misbehaving", &c.misbehaving, &n.misbehaving),
                row("peer removed", &c.removed, &n.removed),
                row("peer flags", &c.flags, &n.flags),
                row("peer reads", &c.read, &n.read),
                row("peer state", &c.peer, &n.peer),
                row("node state", &c.state, &n.state),
            ];
            if let Some((label, cf, nf, _)) = fields.into_iter().find(|f| !f.3) {
                let detail =
                    format!("peer {k}: core {cf:?}\nnode {nf:?}\n\ncore {c:?}\nnode {n:?}");
                return Some((i, label, detail));
            }
        }
        if cs.draws != ns.draws {
            return Some((i, "draws", format!("core {:?}\nnode {:?}", cs.draws, ns.draws)));
        }
        if cs != ns {
            return Some((i, "session", format!("core {cs:?}\nnode {ns:?}")));
        }
    }
    None
}

/// Both legs' setup draws (by phase: the network core, then each peer) and step views.
struct Legs {
    core_setup_draws: Vec<Vec<RandCall>>,
    node_setup_draws: Vec<Vec<RandCall>>,
    core: Vec<StepView>,
    node: Vec<StepView>,
}

/// Runs `s` on both legs: `None` when out of domain.
fn run_both(oracle: &Oracle, s: &Session) -> Result<Option<Legs>, &'static str> {
    if !in_domain(s) {
        return Ok(None);
    }
    let Some(core) = core_run(oracle, s)? else {
        return Ok(None);
    };
    let (node_setup_draws, node) = node_run(s, &core).ok_or("node setup")?;
    Ok(Some(Legs {
        core_setup_draws: core.setup_draws.iter().map(|d| calls(d)).collect(),
        node_setup_draws,
        core: core.views,
        node,
    }))
}

/// `diff_p2p_multi_*`'s comparison of `s` (module docs).
pub fn compare_session(oracle: &Oracle, s: &Session) -> Compared {
    match run_both(oracle, s).map_err(divergence)? {
        None => Ok(Outcome::OutOfDomain),
        Some(legs) => match first_difference(&legs) {
            None => Ok(Outcome::BothAccept),
            Some((_, label, _)) => Err(divergence(label)),
        },
    }
}

/// A comparison spelled out, for tests and triage.
#[derive(Debug)]
pub struct Report {
    /// The steps compared (up to the first inactivity tick or block request); skipped steps
    /// count.
    pub compared: usize,
    /// For each step Core ran, the commands each peer received (`None`: skipped).
    pub received: Vec<Option<Vec<Vec<String>>>>,
    /// Whether Core had removed each peer by the last compared step.
    pub removed: Vec<bool>,
    /// The first difference: its step, the field and both sides.
    pub difference: Option<String>,
}

/// As [`compare_session`], spelled out; `None` when out of domain.
///
/// # Errors
///
/// When either leg fails to run.
pub fn report_session(oracle: &Oracle, s: &Session) -> Result<Option<Report>, &'static str> {
    let Some(legs) = run_both(oracle, s)? else {
        return Ok(None);
    };
    let difference = first_difference(&legs).map(|(step, label, detail)| {
        format!("step {step} ({:?}): {label}\n{detail}", s.steps[step])
    });
    let received = legs
        .core
        .iter()
        .map(|v| {
            v.as_ref().map(|(peers, _)| {
                peers.iter().map(|p| p.sent.iter().map(|(c, _)| c.clone()).collect()).collect()
            })
        })
        .collect();
    let removed = legs
        .core
        .iter()
        .rev()
        .find_map(Option::as_ref)
        .map(|(peers, _)| peers.iter().map(|p| p.removed).collect())
        .unwrap_or_default();
    Ok(Some(Report {
        compared: legs.node.len(),
        received,
        removed,
        difference,
    }))
}
