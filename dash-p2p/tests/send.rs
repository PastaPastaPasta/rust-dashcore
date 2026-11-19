//! SB0 §3.1's send tests: a handler pushing across `-maxsendbuffer`, concurrent overlay pushes,
//! and partial V1/V2 writes at Core's byte counts (`PushMessage`, `SocketSendData`,
//! net.cpp:1694-1773 and 4857-4905 at v24.0.0-rc.3). Then the receive side: `PollMessage`'s
//! quorum-priority ratio and `fPauseRecv` (net.cpp:4773-4850).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::conn::{ConnectionType, PauseState, Permissions, Why};
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, CommandString, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, OutMsg,
    PeerHandle, Resolver, Service, ServiceFlags,
};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};

const PING: CommandString = CommandString::from_static("ping");
const HEADER: usize = 24;

struct Events {
    wakes: AtomicUsize,
}

impl NetEvents for Events {
    fn wake(&self) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
    }

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
}

struct NoResolver;

impl Resolver for NoResolver {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
        _max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        Box::pin(async { Vec::new() })
    }
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn core(send_max: usize) -> Arc<NetCore> {
    core_with(|cfg| cfg.send_buffer_max = send_max)
}

fn core_with(f: impl FnOnce(&mut NetConfig)) -> Arc<NetCore> {
    let mut cfg = NetConfig::new(params(), ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    cfg.peer_timeout = 3600;
    f(&mut cfg);
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        Arc::new(MockClock::new()),
        Arc::new(Events {
            wakes: AtomicUsize::new(0),
        }),
    )
    .unwrap()
}

fn attach(core: &NetCore, sock: MemSocket, conn_type: ConnectionType, use_v2: bool) -> PeerHandle {
    core.attach(Attach {
        sock: Arc::new(sock),
        addr: Address {
            service: Service {
                addr: NetAddr::ipv4([10, 0, 0, 1]),
                port: 19899,
            },
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2,
        grant: None,
        masternode: false,
        masternode_probe: false,
    })
}

fn msg(len: usize, tag: u8) -> OutMsg {
    OutMsg::new(PING, vec![tag; len])
}

/// Polls `cond` every millisecond for up to five seconds.
async fn until(mut cond: impl FnMut() -> bool) {
    for _ in 0..5000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("condition not reached in 5 s");
}

/// Splits a V1 byte stream into (command, payload) frames.
fn v1_frames(mut bytes: &[u8]) -> Vec<(CommandString, Vec<u8>)> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let cmd = CommandString::from_bytes(bytes[4..16].try_into().unwrap());
        let len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        out.push((cmd, bytes[HEADER..HEADER + len].to_vec()));
        bytes = &bytes[HEADER + len..];
    }
    out
}

/// A V1 frame on regtest.
fn v1_frame(cmd: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = params().message_start.to_vec();
    out.extend_from_slice(CommandString::from_static(cmd).as_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&dash_sha256::sha256d(payload)[..4]);
    out.extend_from_slice(payload);
    out
}

/// A handler pushes into a blocked socket: `fPauseSend` sets exactly when the queued charge
/// plus the transport's crosses `-maxsendbuffer`, every message is still queued, and pause
/// clears once the socket drains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handler_crosses_maxsendbuffer() {
    let cost = msg(1000, 0).memory_usage();
    let send_max = cost * 5 + cost / 2;
    let core = core(send_max);
    let (ours, theirs) = MemSocket::pair();
    ours.set_send_budget(Some(0));
    let peer = attach(&core, ours.clone(), ConnectionType::OutboundFullRelay, false);
    for i in 0..5u8 {
        assert_eq!(core.push(&peer, msg(1000, i)), PauseState::Clear, "push {i}");
    }
    // The sixth crosses: 6 × cost > 5.5 × cost, whether the first sits in the queue or in the
    // transport (a V1 transport charges the same message the same).
    assert_eq!(core.push(&peer, msg(1000, 5)), PauseState::Paused);
    assert!(peer.pause_send());
    // Unconditional: pushes while paused are still queued.
    assert_eq!(core.push(&peer, msg(1000, 6)), PauseState::Paused);
    assert_eq!(theirs.drain().len(), 0);

    ours.set_send_budget(None);
    let mut got = Vec::new();
    until(|| {
        got.extend(theirs.drain());
        got.len() == 7 * (HEADER + 1000)
    })
    .await;
    until(|| !peer.pause_send()).await;
    let frames = v1_frames(&got);
    assert_eq!(frames.len(), 7);
    for (i, (cmd, payload)) in frames.iter().enumerate() {
        assert_eq!(*cmd, PING);
        assert_eq!(payload, &vec![i as u8; 1000]);
    }
    assert_eq!(peer.send_bytes(), got.len() as u64);
    assert_eq!(peer.byte_stats().sent.get(&PING), Some(&(got.len() as u64)));
}

/// The send-queue hook (D40) copies what waits: the queue's messages are the newest pushes,
/// in order, charged at their cost, and the transport charges the message it holds in place of its
/// idle buffers; once the socket takes everything both are idle again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_sends_copies_the_send_side() {
    let cost = msg(1000, 0).memory_usage();
    let core = core(cost * 100);
    let (ours, theirs) = MemSocket::pair();
    ours.set_send_budget(Some(0));
    let peer = attach(&core, ours.clone(), ConnectionType::OutboundFullRelay, false);
    // An idle V1 transport still charges its buffers.
    let idle = peer.queued_sends();
    assert!(idle.msgs.is_empty() && idle.memusage == 0, "{idle:?}");
    let pushed: Vec<OutMsg> = (0..3u8).map(|i| msg(1000, i)).collect();
    for m in &pushed {
        core.push(&peer, m.clone());
    }
    let q = peer.queued_sends();
    assert!(pushed.ends_with(&q.msgs) && q.msgs.len() >= 2, "{q:?}");
    assert_eq!(q.memusage, q.msgs.len() * cost);
    // The writer may have moved the oldest into the transport before the socket refused it.
    let held = if q.msgs.len() == pushed.len() {
        idle.transport_memusage
    } else {
        cost
    };
    assert_eq!(q.transport_memusage, held, "{q:?}");
    ours.set_send_budget(None);
    until(|| {
        theirs.drain();
        peer.send_queue_empty()
    })
    .await;
    assert_eq!(peer.queued_sends(), idle);
}

/// The linger hook (D41) reads the deadline the sweep sets: none until the peer is marked, then
/// 100 ms after the sweep first saw it, while bytes still wait.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linger_until_is_the_sweeps_deadline() {
    let core = core(1 << 20);
    let (ours, _theirs) = MemSocket::pair();
    ours.set_send_budget(Some(0));
    let peer = attach(&core, ours.clone(), ConnectionType::OutboundFullRelay, false);
    core.push(&peer, msg(1000, 0));
    assert_eq!(peer.linger_until(), None);
    let marked = std::time::Instant::now();
    core.disconnect(peer.id(), Why::Processor("test"));
    until(|| peer.linger_until().is_some()).await;
    let seen = std::time::Instant::now();
    let until_at = peer.linger_until().unwrap();
    let linger = Duration::from_millis(100);
    assert!(marked + linger <= until_at && until_at <= seen + linger);
}

/// Overlay pushes from many threads at once: nothing is lost, each thread's messages arrive in
/// its order, and the byte count is exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_overlay_pushes() {
    const THREADS: u8 = 8;
    const EACH: usize = 200;
    let core = core(1000 * 1000);
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, ConnectionType::OutboundFullRelay, false);
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let (core, peer) = (core.clone(), peer.clone());
            std::thread::spawn(move || {
                for i in 0..EACH {
                    let mut payload = vec![t];
                    payload.extend_from_slice(&(i as u32).to_le_bytes());
                    core.push(&peer, OutMsg::new(PING, payload));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let total = THREADS as usize * EACH * (HEADER + 5);
    let mut got = Vec::new();
    until(|| {
        got.extend(theirs.drain());
        got.len() >= total
    })
    .await;
    assert_eq!(got.len(), total);
    let mut next = [0u32; THREADS as usize];
    for (_, payload) in v1_frames(&got) {
        let t = payload[0] as usize;
        assert_eq!(u32::from_le_bytes(payload[1..5].try_into().unwrap()), next[t]);
        next[t] += 1;
    }
    assert!(next.iter().all(|&n| n as usize == EACH));
    assert!(!peer.pause_send());
}

/// Partial V1 writes: each `send` is offered the rest of the current header or payload with
/// Core's `MSG_MORE`, takes what the socket has room for, and the next resumes at that byte.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_v1_writes() {
    let core = core(1000 * 1000);
    let (ours, theirs) = MemSocket::pair();
    ours.set_send_budget(Some(0));
    let peer = attach(&core, ours.clone(), ConnectionType::OutboundFullRelay, false);
    core.push(&peer, msg(100, 1));
    core.push(&peer, msg(50, 2));

    let mut taken = 0;
    for budget in [10, 20, 60, 70, 4, 200] {
        ours.set_send_budget(Some(budget));
        taken += budget.min(2 * HEADER + 150 - taken);
        until(|| ours.in_flight() == taken).await;
    }
    assert_eq!(taken, 2 * HEADER + 150);
    // A header goes with MSG_MORE (its payload follows), the first payload too (the next
    // message is queued), the last payload without (nothing follows).
    assert_eq!(
        ours.sends(),
        vec![
            (10, true),  // header 0..10, budget 10
            (14, true),  // header 10..24
            (6, true),   // payload 0..6, budget 20 spent
            (60, true),  // payload 6..66
            (34, true),  // payload 66..100
            (24, true),  // second header
            (12, false), // second payload 0..12, budget 70 spent
            (4, false),  // 12..16, budget 4
            (34, false), // 16..50
        ]
    );
    let frames = v1_frames(&theirs.drain());
    assert_eq!(frames, vec![(PING, vec![1; 100]), (PING, vec![2; 50])]);
    assert_eq!(peer.send_bytes(), (2 * HEADER + 150) as u64);
}

/// Partial V2 writes between two cores: the initiator's socket takes a few bytes at a time
/// through the key exchange and the encrypted messages, and the responder receives every
/// message whole and in order. Every send is offered the rest of the transport's buffer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn partial_v2_writes() {
    let (a, b) = (core(1000 * 1000), core(1000 * 1000));
    let (sa, sb) = MemSocket::pair();
    sa.set_send_budget(Some(0));
    let initiator = attach(&a, sa.clone(), ConnectionType::OutboundFullRelay, true);
    let responder = attach(&b, sb, ConnectionType::Inbound, true);
    for i in 0..4u8 {
        a.push(&initiator, msg(300 + usize::from(i), i));
    }
    // Feed the initiator's socket 37 bytes at a time until the responder holds every message. Not
    // until the initiator goes quiet: a worker descheduled for a few ticks under load looks the
    // same, and the rest would never be offered a budget (BL-366).
    let mut got = Vec::new();
    for _ in 0..5000 {
        while let Some(p) = b.pop(&responder) {
            got.push(p.msg);
        }
        if got.len() == 4 {
            break;
        }
        sa.set_send_budget(Some(37));
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert_eq!(got.len(), 4, "the responder's messages after 5000 budgets");
    let sends = sa.sends();
    assert!(sends.len() > 10, "{sends:?}");
    assert!(sends.iter().all(|&(n, _)| n <= 37));
    // The initiator counts a write once the socket has taken it, after the responder can read it.
    let sent = || sa.sends().iter().map(|s| s.0).sum::<usize>() as u64;
    until(|| initiator.send_bytes() == sent()).await;
    for (i, m) in got.iter().enumerate() {
        assert_eq!(m.cmd, PING);
        assert_eq!(m.payload, vec![i as u8; 300 + i]);
    }
}

/// Injects `frames` from the peer's side and waits until the core has queued all of them: the
/// per-type receive counts are taken under the queue lock (`recv_bytes` is counted before the
/// transport parses a frame).
async fn receive(peer: &PeerHandle, theirs: &MemSocket, frames: &[Vec<u8>]) {
    let bytes = frames.concat();
    let counted = || peer.byte_stats().recv.values().sum::<u64>();
    let before = counted();
    theirs.inject(&bytes);
    until(|| counted() == before + bytes.len() as u64).await;
}

fn pop_all(core: &NetCore, peer: &PeerHandle) -> Vec<(String, Vec<u8>, bool)> {
    std::iter::from_fn(|| core.pop(peer))
        .map(|p| (p.msg.cmd.as_str().to_owned(), p.msg.payload, p.more))
        .collect()
}

/// `PollMessage`: 100 quorum-priority messages per normal one while both queues hold messages,
/// quorum bursts once the normal queue is empty, `more` while either holds one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poll_quorum_ratio() {
    let core = core(1000 * 1000);
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, ConnectionType::OutboundFullRelay, false);
    let mut frames = vec![v1_frame("ping", &[0])];
    frames.extend((0u8..150).map(|i| v1_frame("qsigshare", &[i])));
    frames.push(v1_frame("ping", &[1]));
    receive(&peer, &theirs, &frames).await;

    let got = pop_all(&core, &peer);
    let order: Vec<(&str, u8)> = got.iter().map(|(c, p, _)| (c.as_str(), p[0])).collect();
    let mut expect: Vec<(&str, u8)> = (0..100).map(|i| ("qsigshare", i)).collect();
    expect.push(("ping", 0));
    expect.extend((100..150).map(|i| ("qsigshare", i)));
    expect.push(("ping", 1));
    assert_eq!(order, expect);
    assert!(got[..got.len() - 1].iter().all(|m| m.2));
    assert!(!got.last().unwrap().2);
}

/// `recv_queue_empty` (`test-utils`): false once a message is counted, true after `PollMessage`
/// takes the last one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_queue_empty() {
    let core = core(1000 * 1000);
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, ConnectionType::OutboundFullRelay, false);
    assert!(peer.recv_queue_empty());
    receive(&peer, &theirs, &[v1_frame("ping", &[0]), v1_frame("ping", &[1])]).await;
    assert!(!peer.recv_queue_empty());
    core.pop(&peer).unwrap();
    assert!(!peer.recv_queue_empty());
    core.pop(&peer).unwrap();
    assert!(peer.recv_queue_empty());
}

/// With `priority_queues` off (SPV, D12) the peer's messages come out in arrival order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn poll_fifo_without_priority() {
    let core = core_with(|cfg| cfg.priority_queues = false);
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, ConnectionType::OutboundFullRelay, false);
    let frames = [v1_frame("ping", &[0]), v1_frame("clsig", &[1]), v1_frame("ping", &[2])];
    receive(&peer, &theirs, &frames).await;
    let payloads: Vec<u8> = pop_all(&core, &peer).iter().map(|m| m.1[0]).collect();
    assert_eq!(payloads, [0, 1, 2]);
}

/// `fPauseRecv`: set when the queued raw sizes exceed `-maxreceivebuffer`, which stops reading;
/// cleared by `PollMessage` once under it, which resumes reading.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recv_pause() {
    let core = core_with(|cfg| cfg.recv_flood_size = 1000);
    let (ours, theirs) = MemSocket::pair();
    let peer = attach(&core, ours, ConnectionType::OutboundFullRelay, false);
    // 40 frames of 24 + 4 bytes: 1120 > 1000 after one read.
    let frames: Vec<Vec<u8>> = (0u32..40).map(|i| v1_frame("ping", &i.to_le_bytes())).collect();
    receive(&peer, &theirs, &frames).await;
    assert!(peer.pause_recv());
    // Paused: the next bytes stay in the socket.
    let late = v1_frame("ping", &40u32.to_le_bytes());
    theirs.inject(&late);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(theirs.in_flight(), late.len());
    // 1120 − 4 × 28 = 1008 is still over; the fifth pop brings it to 980 and resumes reading.
    for _ in 0..4 {
        core.pop(&peer).unwrap();
        assert!(peer.pause_recv());
    }
    core.pop(&peer).unwrap();
    assert!(!peer.pause_recv());
    until(|| theirs.in_flight() == 0).await;
    // The byte count is taken before the message is queued: wait for the queue.
    let mut rest = Vec::new();
    until(|| {
        rest.extend(pop_all(&core, &peer));
        rest.len() >= 36
    })
    .await;
    assert_eq!(rest.len(), 36);
    assert_eq!(peer.recv_bytes(), 41 * 28);
}

/// `fPauseSend` after a send keeps the transport's charge: a message the socket took only part
/// of still pauses the peer, until the rest goes out.
// Core: net.cpp:1765 SocketSendData
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_partly_sent_message_keeps_the_pause() {
    let m = msg(5000, 0);
    let core = core(m.memory_usage() / 2);
    let (ours, theirs) = MemSocket::pair();
    ours.set_send_budget(Some(100));
    let peer = attach(&core, ours.clone(), ConnectionType::OutboundFullRelay, false);
    assert!(core.push(&peer, m).is_paused());
    // `queued_sends` locks what `send_data` holds until it has recomputed the pause.
    until(|| peer.queued_sends().msgs.is_empty() && ours.in_flight() == 100).await;
    assert!(peer.pause_send(), "the transport's partial message no longer pauses");
    ours.set_send_budget(None);
    until(|| {
        theirs.drain();
        peer.send_queue_empty()
    })
    .await;
    assert!(!peer.pause_send());
}
