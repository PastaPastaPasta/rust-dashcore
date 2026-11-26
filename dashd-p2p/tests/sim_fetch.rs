//! BL-503 in `sim` (the plan's §4.1): Core's `FetchBlock` (np:2057-2089) as one actor step,
//! through the real `BlockDownload` and `SimChain`'s block path. Every test observes the node as
//! Core's peers and RPC do: `getpeerinfo`'s `inflight` (`Shared::node_state_stats`), the frames
//! a peer receives, the log, and whether a block reached V with `force_processing`.

use std::cell::RefCell;
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dash_consensus::Block;
use dash_p2p::rng::FastRandom;
use dash_p2p::{CommandString, ConnectionType, PeerId, Why};
use dash_p2p_core::{GetData, InvType};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::fetch::{FetchBlockError, FetchTicket};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::peerman::{LockPoint, MessageLoop, PeerManager};
use dashd_p2p::processing::sync::block_hash;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain, SimPeer};
use dashd_p2p::{Registry, Seams};
use parking_lot::Mutex;

thread_local! {
    /// The `net` lines this thread logged while [`capture`] ran.
    static LINES: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

/// A global subscriber, installed before any callsite registers (P3 D75's trap); events go to
/// the emitting thread's [`LINES`]. The sim runs the message loop on the test's thread.
struct Global;

impl tracing::Subscriber for Global {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && meta.target() == "net" && LINES.with(|l| l.borrow().is_some())
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

fn has(lines: &[String], want: &str) -> bool {
    lines.iter().any(|l| l.contains(want))
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

/// A node with P3's `BlockDownload` on a genesis-only chain (in IBD), and `n` blocks on genesis
/// whose headers it has, but not their data, from no peer: no peer has a best-known block, so
/// section 12 requests nothing.
fn node(n: usize) -> (Sim, Vec<Arc<Block>>) {
    install();
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        block_sync: Box::new(BlockDownload::new()),
        ..Seams::default()
    };
    let sim = Sim::with(p, id, ProcessorOptions::default(), Registry::new(), seams, |_| {});
    let blocks = SimChain::build_blocks(&sim.chain.genesis(), n, 1, 0);
    let headers: Vec<BlockHeader> = blocks.iter().map(|b| *b.header().raw()).collect();
    sim.chain.add(&headers);
    (sim, blocks)
}

/// An outbound full-relay peer at 10.0.0.`n`, handshaken, with what it was sent so far taken.
fn connect(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.connect(n, ConnectionType::OutboundFullRelay, Default::default());
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

/// The `getdata`s `p` was sent since the last read, each as its entries.
fn getdatas(sim: &Sim, p: &SimPeer) -> Vec<Vec<(InvType, BlockHash)>> {
    sim.sent(p)
        .into_iter()
        .filter(|(c, _)| *c == CommandString::GETDATA)
        .map(|(_, payload)| {
            let gd = GetData::decode(&mut &payload[..]).expect("a getdata");
            gd.inventory().iter().map(|i| (i.inv_type(), block_hash(i))).collect()
        })
        .collect()
}

fn id(p: &SimPeer) -> PeerId {
    p.handle.id()
}

fn height(b: &Block, sim: &Sim) -> i32 {
    sim.chain.by_hash(&b.hash()).expect("indexed").height as i32
}

/// The `FetchBlock` step for `b` from `peer`, as the actor runs it.
fn fetch(sim: &mut Sim, b: &Block, peer: PeerId) -> Result<(), FetchBlockError> {
    sim.lp.logic_mut().fetch_block(&b.hash(), peer)
}

/// A pass at mock time `t` seconds plus `us` µs.
fn pass_at(sim: &mut Sim, t: i64, us: u64) {
    sim.clock.set_mock_time(t);
    sim.clock.advance(Duration::from_micros(us));
    sim.pass();
}

/// U1: the loading check comes first (np:2059), before the peer check, for an unknown peer and a
/// live one alike; nothing is sent or recorded.
#[test]
fn u1_loading_blocks_first() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    sim.chain.set_loading_blocks(true);
    assert_eq!(fetch(&mut sim, &blocks[2], -1), Err(FetchBlockError::LoadingBlocks));
    assert_eq!(fetch(&mut sim, &blocks[2], id(&p)), Err(FetchBlockError::LoadingBlocks));
    assert!(getdatas(&sim, &p).is_empty());
    assert!(sim.inflight(&p).is_empty());
}

/// U2: `GetPeerRef` (np:2062-2063) for −1, one past the highest id, and a peer that was closed
/// and finalized. The remove-all comes after it, so another peer's request survives.
#[test]
fn u2_unknown_peer() {
    let (mut sim, blocks) = node(3);
    let a = connect(&mut sim, 1);
    let b = connect(&mut sim, 2);
    let h = &blocks[2];
    assert_eq!(fetch(&mut sim, h, id(&a)), Ok(()));
    assert_eq!(getdatas(&sim, &a), [[(InvType::Block, h.hash())]]);
    sim.core.disconnect(id(&b), Why::Processor("test"));
    sim.remove_disconnected();
    assert!(!sim.shared.has_peer(id(&b)));
    for peer in [-1, id(&b) + 1, id(&b)] {
        assert_eq!(fetch(&mut sim, h, peer), Err(FetchBlockError::PeerDoesNotExist), "{peer}");
    }
    assert_eq!(sim.inflight(&a), [height(h, &sim)]);
    assert!(getdatas(&sim, &a).is_empty());
}

/// U2b: a request queued for P, with P then closed and finalized before the actor drains the
/// queue, answers "Peer does not exist" and records nothing in flight: no peer lists the block,
/// no `getdata` goes out, and a `block` for it from another peer is not force-processed.
#[test]
fn u2b_peer_finalized_before_the_drain() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let q = connect(&mut sim, 2);
    let h = &blocks[2];
    let mut ticket = sim.shared.request_fetch(h.hash(), id(&p));
    sim.core.disconnect(id(&p), Why::Processor("test"));
    sim.remove_disconnected();
    assert!(!sim.shared.has_peer(id(&p)));
    assert_eq!(ticket.try_result(), None, "still queued");
    assert_eq!(sim.lp.drain_fetches(), 1);
    assert_eq!(ticket.try_result(), Some(Err(FetchBlockError::PeerDoesNotExist)));
    assert!(sim.inflight(&q).is_empty());
    assert!(getdatas(&sim, &q).is_empty());
    sim.deliver(&q, &[("block", dash_consensus::core_serialize(&**h))]);
    sim.pass();
    let last = sim.chain.submits().pop().expect("submitted");
    assert_eq!((last.hash, last.force), (h.hash(), false));
}

/// U3: success. One `getdata` with one `MSG_BLOCK` inventory, pushed in the step itself, and
/// the request recorded at the step's own clock read (`BlockRequested`'s `GetTime`, np:1350),
/// not the last pass's or the queueing's: queued at T0 after a pass at T0 and run at T1 with no
/// pass between, P is kept at T0 + 150 s + 1 µs and at T1 + 150 s (np:6789's strict `>`), and
/// disconnected at T1 + 150 s + 1 µs.
// Core: net_processing.cpp:1346-1351, 2068-2088, 6773-6781
#[test]
fn u3_success_and_the_record_clock() {
    let (mut sim, blocks) = node(3);
    let spacing = sim.chain.params().consensus.pow_target_spacing;
    let p = connect(&mut sim, 1);
    let h = &blocks[2];
    let t0 = SIM_START + 10;
    sim.clock.set_mock_time(t0);
    sim.pass();
    sim.sent(&p);
    let mut ticket = sim.shared.request_fetch(h.hash(), id(&p));
    let t1 = t0 + 60;
    sim.clock.set_mock_time(t1);
    let (ran, lines) = capture(|| sim.lp.drain_fetches());
    assert_eq!(ran, 1);
    assert_eq!(ticket.try_result(), Some(Ok(())));
    // np:2086, with one space (np:3239 has two).
    let want = format!("Requesting block {} from peer={}", h.hash(), id(&p));
    assert!(lines.contains(&want), "{lines:?}");
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, h.hash())]]);
    assert_eq!(sim.inflight(&p), [height(h, &sim)]);
    pass_at(&mut sim, t0 + spacing, 1);
    assert!(!p.handle.disconnect_requested(), "kept at T0 + 150 s + 1 µs");
    pass_at(&mut sim, t1 + spacing, 0);
    assert!(!p.handle.disconnect_requested(), "kept at T1 + 150 s");
    let ((), lines) = capture(|| pass_at(&mut sim, t1 + spacing, 1));
    assert!(p.handle.disconnect_requested(), "disconnected at T1 + 150 s + 1 µs");
    let want =
        format!("Timeout downloading block {} from peer={}, disconnecting", h.hash(), id(&p));
    assert!(has(&lines, &want), "{lines:?}");
}

/// U5: the same peer twice. The remove-all erases P's own entry, so `BlockRequested` appends
/// it again: `[h1, h2]` becomes `[h2, h1]` and a second `getdata` goes out.
#[test]
fn u5_same_peer_twice() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let (h1, h2) = (&blocks[0], &blocks[1]);
    assert_eq!(fetch(&mut sim, h1, id(&p)), Ok(()));
    assert_eq!(fetch(&mut sim, h2, id(&p)), Ok(()));
    assert_eq!(getdatas(&sim, &p).len(), 2);
    assert_eq!(fetch(&mut sim, h1, id(&p)), Ok(()));
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, h1.hash())]]);
    assert_eq!(sim.inflight(&p), [height(h2, &sim), height(h1, &sim)]);
}

/// U6: a peer that never sent `version` is not fully connected (net.cpp:4864-4867): the answer
/// is "Peer not fully connected", nothing is sent, and the entry stays (np:2071 ran first).
/// Once the handshake completes, section 9 times it out from the fetch's clock.
// Core: net_processing.cpp:2071-2084, 6328-6329, 6773-6781
#[test]
fn u6_not_fully_connected_keeps_the_entry() {
    let (mut sim, blocks) = node(3);
    let spacing = sim.chain.params().consensus.pow_target_spacing;
    let c = sim.accept(3, Default::default()).expect("inbound admitted");
    let h = &blocks[2];
    assert_eq!(fetch(&mut sim, h, id(&c)), Err(FetchBlockError::PeerNotFullyConnected));
    assert!(sim.sent(&c).is_empty());
    assert_eq!(sim.inflight(&c), [height(h, &sim)]);
    sim.handshake(&c, PeerVersionMsg::v24(SIM_START));
    assert!(getdatas(&sim, &c).is_empty());
    pass_at(&mut sim, SIM_START + spacing, 0);
    assert!(!c.handle.disconnect_requested());
    let ((), lines) = capture(|| pass_at(&mut sim, SIM_START + spacing, 1));
    assert!(c.handle.disconnect_requested());
    let want =
        format!("Timeout downloading block {} from peer={}, disconnecting", h.hash(), id(&c));
    assert!(has(&lines, &want), "{lines:?}");
}

/// U6b: a fully connected peer marked for disconnection is not fully connected either
/// (`!fDisconnect`); the entry is recorded and `FinalizeNode` erases it, so a later `block` for
/// it from another peer is not force-processed.
// Core: net_processing.cpp:1804-1815, 5391
#[test]
fn u6b_marked_peer_keeps_the_entry_until_finalize() {
    let (mut sim, blocks) = node(3);
    let a = connect(&mut sim, 1);
    let b = connect(&mut sim, 2);
    let h = &blocks[2];
    sim.core.disconnect(id(&a), Why::Processor("test"));
    assert_eq!(fetch(&mut sim, h, id(&a)), Err(FetchBlockError::PeerNotFullyConnected));
    assert_eq!(sim.inflight(&a), [height(h, &sim)]);
    assert!(getdatas(&sim, &a).is_empty());
    sim.remove_disconnected();
    sim.deliver(&b, &[("block", dash_consensus::core_serialize(&**h))]);
    sim.pass();
    let last = sim.chain.submits().pop().expect("submitted");
    assert_eq!((last.hash, last.force), (h.hash(), false));
}

/// U7: the fetched block arrives. Its `block` is force-processed (`IsBlockRequested`, np:5391)
/// and its request removed; a `block` for a fetched hash from a different peer is
/// force-processed too.
// Core: net_processing.cpp:5361-5399
#[test]
fn u7_the_received_block_is_forced() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let q = connect(&mut sim, 2);
    let (h, h2) = (&blocks[2], &blocks[1]);
    assert_eq!(fetch(&mut sim, h, id(&p)), Ok(()));
    sim.deliver(&p, &[("block", dash_consensus::core_serialize(&**h))]);
    sim.pass();
    let last = sim.chain.submits().pop().expect("submitted");
    assert_eq!((last.hash, last.force, last.from), (h.hash(), true, id(&p)));
    assert!(sim.inflight(&p).is_empty());

    assert_eq!(fetch(&mut sim, h2, id(&p)), Ok(()));
    sim.deliver(&q, &[("block", dash_consensus::core_serialize(&**h2))]);
    sim.pass();
    let last = sim.chain.submits().pop().expect("submitted");
    assert_eq!((last.hash, last.force, last.from), (h2.hash(), true, id(&q)));
}

/// U8: the queue path. A request is answered at the next pass, with U3's frame; two are
/// answered in arrival order.
#[test]
fn u8_queued_requests_run_at_the_next_pass() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let mut ticket = sim.shared.request_fetch(blocks[2].hash(), id(&p));
    assert_eq!(ticket.try_result(), None);
    sim.pass();
    assert_eq!(ticket.try_result(), Some(Ok(())));
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, blocks[2].hash())]]);

    let (h1, h2) = (&blocks[1], &blocks[0]);
    let mut first = sim.shared.request_fetch(h1.hash(), id(&p));
    let mut second = sim.shared.request_fetch(h2.hash(), -1);
    let mut third = sim.shared.request_fetch(h2.hash(), id(&p));
    sim.pass();
    assert_eq!(first.try_result(), Some(Ok(())));
    assert_eq!(second.try_result(), Some(Err(FetchBlockError::PeerDoesNotExist)));
    assert_eq!(third.try_result(), Some(Ok(())));
    let want = [[(InvType::Block, h1.hash())], [(InvType::Block, h2.hash())]];
    assert_eq!(getdatas(&sim, &p), want);
    let heights = [&blocks[2], h1, h2].map(|b| height(b, &sim));
    assert_eq!(sim.inflight(&p), heights);
}

/// U9: a request wakes the actor (Core's `WakeMessageHandler`), so the idle wait returns at
/// once rather than after its 100 ms. The test reads the flag, not the time.
#[test]
fn u9_a_request_wakes_the_loop() {
    let (mut sim, blocks) = node(1);
    let p = connect(&mut sim, 1);
    let bells = Arc::clone(&sim.shared.signals().bells);
    sim.lp.wait(true);
    assert!(!bells.is_woken());
    let _ticket = sim.shared.request_fetch(blocks[0].hash(), id(&p));
    assert!(bells.is_woken());
    sim.lp.wait(false);
    assert!(!bells.is_woken());
}

/// The wall time an answer may take once the processor is interrupted, on a loaded box: a
/// request made after the interrupt answers at once, and the exit drain waits on no socket,
/// block or V.
const STOPPED_ANSWER: Duration = Duration::from_secs(5);

/// `f` on another thread: its answer, or `None` when it takes longer than `limit`.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(f()));
    rx.recv_timeout(limit).ok()
}

/// U10 (P2a-net D51): `Shared::interrupt` leaves the queue open, and `Shared::close_fetches`
/// closes it. A request made after the interrupt is kept and served; one made after the close,
/// and a blocking `Shared::fetch_block` made after it, answer `Stopped` at once. The queued ones
/// get `FetchBlock`'s answer when `run` ends, within [`STOPPED_ANSWER`]: `Ok` with the
/// `getdata` pushed for a fully connected peer that never answers, and "Peer does not exist"
/// for a peer closed and finalized before the drain. A request the actor ran before keeps its
/// answer.
#[test]
fn u10_the_queue_closes_at_stop_and_the_exit_drain_answers_it() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let gone = connect(&mut sim, 2);
    let peer = id(&p);
    let ran = sim.shared.request_fetch(blocks[0].hash(), peer);
    assert_eq!(sim.lp.drain_fetches(), 1);
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, blocks[0].hash())]]);
    let mut slow = sim.shared.request_fetch(blocks[1].hash(), peer);
    let mut dead = sim.shared.request_fetch(blocks[2].hash(), id(&gone));
    sim.shared.interrupt();
    let mut after = sim.shared.request_fetch(blocks[2].hash(), peer);
    sim.shared.close_fetches();
    let stopped = Some(Err(FetchBlockError::Stopped));
    let late = sim.shared.request_fetch(blocks[2].hash(), peer);
    assert_eq!(within(STOPPED_ANSWER, move || late.wait()), stopped, "made after the close");
    let (shared, h) = (Arc::clone(&sim.shared), blocks[2].hash());
    let call = within(STOPPED_ANSWER, move || shared.fetch_block(h, peer));
    assert_eq!(call, stopped, "Shared::fetch_block made after the close");
    let kept = (slow.try_result(), dead.try_result(), after.try_result());
    assert_eq!(kept, (None, None, None), "kept for the exit drain");
    sim.core.disconnect(id(&gone), Why::Processor("test"));
    sim.remove_disconnected();
    assert!(!sim.shared.has_peer(id(&gone)));
    // The actor's way out: `run` skips its loop and its serving, as it is interrupted and the
    // queue is closed, and drains.
    let start = Instant::now();
    sim.lp.run();
    assert!(start.elapsed() < STOPPED_ANSWER, "the exit drain took {:?}", start.elapsed());
    assert_eq!(slow.try_result(), Some(Ok(())), "a slow peer");
    assert_eq!(dead.try_result(), Some(Err(FetchBlockError::PeerDoesNotExist)), "a dead peer");
    assert_eq!(after.try_result(), Some(Ok(())), "made after the interrupt");
    let sent = [[(InvType::Block, blocks[1].hash())], [(InvType::Block, blocks[2].hash())]];
    assert_eq!(getdatas(&sim, &p), sent);
    assert_eq!(within(STOPPED_ANSWER, move || ran.wait()), Some(Ok(())));
    assert_eq!(sim.lp.drain_fetches(), 0);
    let all = [height(&blocks[0], &sim), height(&blocks[1], &sim), height(&blocks[2], &sim)];
    assert_eq!(sim.inflight(&p), all);
}

/// U10e (P2a-net D51): on the actor thread, after `Shared::interrupt` and before
/// `PeerManager::stop`, a blocking `Shared::fetch_block` gets `FetchBlock`'s answer within
/// [`STOPPED_ANSWER`], and its `getdata` goes out. Requests queued for a slow peer and a dead
/// one just before `stop` do not hold it past the bound, and get their answers; after `stop`,
/// one answers `Stopped` at once.
#[test]
fn u10e_served_after_the_interrupt_until_stop() {
    let (mut sim, blocks) = node(2);
    let p = connect(&mut sim, 1);
    let peer = id(&p);
    let lp = MessageLoop::new(
        Arc::clone(&sim.core),
        Arc::clone(&sim.shared),
        Registry::new(),
        Seams::default(),
        FastRandom::deterministic(),
    );
    let pm = PeerManager::spawn(lp);
    sim.shared.interrupt();
    let (shared, h) = (Arc::clone(&sim.shared), blocks[0].hash());
    let call = within(STOPPED_ANSWER, move || shared.fetch_block(h, peer));
    assert_eq!(call, Some(Ok(())), "made after the interrupt");
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, blocks[0].hash())]]);
    let slow = sim.shared.request_fetch(blocks[1].hash(), peer);
    let dead = sim.shared.request_fetch(blocks[1].hash(), peer + 100);
    let start = Instant::now();
    assert!(pm.stop().is_some());
    assert!(start.elapsed() < STOPPED_ANSWER, "stop took {:?}", start.elapsed());
    assert_eq!(within(STOPPED_ANSWER, move || slow.wait()), Some(Ok(())), "a slow peer");
    let dead = within(STOPPED_ANSWER, move || dead.wait());
    assert_eq!(dead, Some(Err(FetchBlockError::PeerDoesNotExist)), "a dead peer");
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, blocks[1].hash())]]);
    let (shared, h) = (Arc::clone(&sim.shared), blocks[1].hash());
    let call = within(STOPPED_ANSWER, move || shared.fetch_block(h, peer));
    assert_eq!(call, Some(Err(FetchBlockError::Stopped)), "made after stop");
    assert_eq!(getdatas(&sim, &p), Vec::<Vec<(InvType, BlockHash)>>::new());
}

/// U10b: a panic in the actor's fetch step stops it with no exit drain. The request that
/// panicked, one queued behind it while it ran, and one made after all answer `Stopped`
/// within [`STOPPED_ANSWER`].
#[test]
fn u10b_a_panic_answers_stopped() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let peer = id(&p);
    let behind: Arc<Mutex<Option<FetchTicket>>> = Arc::default();
    let hook = {
        let (shared, behind, h) = (Arc::clone(&sim.shared), Arc::clone(&behind), blocks[1].hash());
        Arc::new(move |at: LockPoint| {
            if at == LockPoint::FetchBetween(peer) {
                *behind.lock() = Some(shared.request_fetch(h, peer));
                panic!("a planted panic in the fetch step");
            }
        })
    };
    sim.shared.signals().set_lock_hook(Some(hook));
    let first = sim.shared.request_fetch(blocks[0].hash(), peer);
    let lp = MessageLoop::new(
        Arc::clone(&sim.core),
        Arc::clone(&sim.shared),
        Registry::new(),
        Seams::default(),
        FastRandom::deterministic(),
    );
    let pm = PeerManager::spawn(lp);
    let deadline = Instant::now() + Duration::from_secs(600);
    while !sim.shared.panicked() {
        assert!(Instant::now() < deadline, "the planted panic");
        std::thread::sleep(Duration::from_millis(5));
    }
    let stopped = Some(Err(FetchBlockError::Stopped));
    assert_eq!(within(STOPPED_ANSWER, move || first.wait()), stopped, "the panicked request");
    let behind = behind.lock().take().expect("queued behind it");
    assert_eq!(within(STOPPED_ANSWER, move || behind.wait()), stopped, "queued behind it");
    let late = sim.shared.request_fetch(blocks[2].hash(), peer);
    assert_eq!(within(STOPPED_ANSWER, move || late.wait()), stopped, "made after");
    sim.shared.signals().set_lock_hook(None);
    assert!(pm.stop().is_some());
}

/// U10c (P2a-net D51): `stop_handlers` on a loop no thread ran starts with the exit drain, so
/// a request queued before the interrupt runs and is answered, and its `getdata` goes out.
#[test]
fn u10c_stop_handlers_on_an_unrun_loop_drains() {
    let (mut sim, blocks) = node(1);
    let p = connect(&mut sim, 1);
    let mut queued = sim.shared.request_fetch(blocks[0].hash(), id(&p));
    sim.shared.interrupt();
    assert_eq!(queued.try_result(), None, "kept for the exit drain");
    sim.lp.stop_handlers();
    assert_eq!(queued.try_result(), Some(Ok(())));
    assert_eq!(getdatas(&sim, &p), [[(InvType::Block, blocks[0].hash())]]);
}

/// U10d: a loop dropped without its exit drain (never run, never stopped) answers what is
/// still queued `Stopped` within [`STOPPED_ANSWER`], and a request made after answers the same.
#[test]
fn u10d_a_dropped_unrun_loop_answers_stopped() {
    let (mut sim, blocks) = node(1);
    let p = connect(&mut sim, 1);
    let peer = id(&p);
    let lp = MessageLoop::new(
        Arc::clone(&sim.core),
        Arc::clone(&sim.shared),
        Registry::new(),
        Seams::default(),
        FastRandom::deterministic(),
    );
    let queued = sim.shared.request_fetch(blocks[0].hash(), peer);
    drop(lp);
    let stopped = Some(Err(FetchBlockError::Stopped));
    assert_eq!(within(STOPPED_ANSWER, move || queued.wait()), stopped, "dropped unrun");
    let late = sim.shared.request_fetch(blocks[0].hash(), peer);
    assert_eq!(within(STOPPED_ANSWER, move || late.wait()), stopped, "made after");
    assert_eq!(getdatas(&sim, &p), Vec::<Vec<(InvType, BlockHash)>>::new());
}

/// U10f (P2a-net D51): once the loop ends at the interrupt, V's signal queue is closed while the
/// actor still serves `FetchBlock`, so a V waiting on the queue is not held until
/// `PeerManager::stop` (Opus r3 R3-1).
#[test]
fn u10f_the_signal_queue_closes_when_the_loop_ends() {
    let (mut sim, blocks) = node(1);
    let p = connect(&mut sim, 1);
    let peer = id(&p);
    let lp = MessageLoop::new(
        Arc::clone(&sim.core),
        Arc::clone(&sim.shared),
        Registry::new(),
        Seams::default(),
        FastRandom::deterministic(),
    );
    let pm = PeerManager::spawn(lp);
    assert!(!sim.shared.signals().queue.is_closed(), "open while the loop runs");
    sim.shared.interrupt();
    let deadline = Instant::now() + STOPPED_ANSWER;
    while !sim.shared.signals().queue.is_closed() {
        assert!(Instant::now() < deadline, "the signal queue stayed open after the interrupt");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (shared, h) = (Arc::clone(&sim.shared), blocks[0].hash());
    let call = within(STOPPED_ANSWER, move || shared.fetch_block(h, peer));
    assert_eq!(call, Some(Ok(())), "still served");
    assert!(pm.stop().is_some());
}

/// The test's slot for U11's blocking reader, joined after the fetch returns.
type Reader = Arc<Mutex<Option<JoinHandle<Option<Vec<i32>>>>>>;

/// U11 (§2.1a): a `getpeerinfo` on another thread never sees the target between the remove-all
/// and `BlockRequested`, as Core's under `cs_main` does not (np:2065-2071, np:1880-1888). At the
/// hook point between them, a reader that only tries the state lock finds it held (the same
/// reader gets the state before the step), and a blocking reader started there sees the
/// step's result.
#[test]
fn u11_getpeerinfo_sees_the_step_whole() {
    let (mut sim, blocks) = node(3);
    let p = connect(&mut sim, 1);
    let (h, h2) = (&blocks[0], &blocks[1]);
    assert_eq!(fetch(&mut sim, h, id(&p)), Ok(()));
    assert_eq!(fetch(&mut sim, h2, id(&p)), Ok(()));
    let (hh, hh2) = (height(h, &sim), height(h2, &sim));
    assert_eq!(sim.inflight(&p), [hh, hh2]);

    let tried: Arc<Mutex<Vec<Option<Vec<i32>>>>> = Arc::default();
    let reader: Reader = Arc::default();
    let target = id(&p);
    let hook = {
        let (shared, handle) = (Arc::clone(&sim.shared), p.handle.clone());
        let (tried, reader) = (Arc::clone(&tried), Arc::clone(&reader));
        Arc::new(move |at: LockPoint| {
            if at != LockPoint::FetchBetween(target) {
                return;
            }
            let (s, hd) = (Arc::clone(&shared), handle.clone());
            let got = std::thread::spawn(move || s.try_node_state_stats(&hd))
                .join()
                .expect("the try-reader");
            tried.lock().push(got.map(|s| s.heights_in_flight));
            let (s, hd) = (Arc::clone(&shared), handle.clone());
            *reader.lock() = Some(std::thread::spawn(move || {
                s.node_state_stats(&hd).map(|s| s.heights_in_flight)
            }));
        })
    };
    // The try-reader reads the peer while nothing holds its state, so a `None` at the hook is
    // the step's guard.
    assert_eq!(
        sim.shared.try_node_state_stats(&p.handle).map(|s| s.heights_in_flight),
        Some(vec![hh, hh2])
    );
    sim.shared.signals().set_lock_hook(Some(hook));
    assert_eq!(fetch(&mut sim, h, id(&p)), Ok(()));
    sim.shared.signals().set_lock_hook(None);
    let tried = std::mem::take(&mut *tried.lock());
    assert_eq!(tried.len(), 1, "the hook ran once");
    assert_eq!(tried[0], None, "a reader saw the target between the steps");
    let blocking = reader.lock().take().expect("started").join().expect("the reader");
    assert_eq!(blocking, Some(vec![hh2, hh]));
}

/// U-str: each Core variant's text is Core's literal.
#[test]
fn u_str_core_messages() {
    let cases = [
        // Core: net_processing.cpp:2059
        (FetchBlockError::LoadingBlocks, "Loading blocks ..."),
        // Core: net_processing.cpp:2063
        (FetchBlockError::PeerDoesNotExist, "Peer does not exist"),
        // Core: net_processing.cpp:2071
        (FetchBlockError::AlreadyRequested, "Already requested from this peer"),
        // Core: net_processing.cpp:2084
        (FetchBlockError::PeerNotFullyConnected, "Peer not fully connected"),
        // Core: rpc/blockchain.cpp:515
        (FetchBlockError::HeaderMissing, "Block header missing"),
    ];
    for (e, want) in cases {
        assert_eq!(e.core_message(), Some(want));
        assert_eq!(e.to_string(), want);
    }
    assert_eq!(FetchBlockError::Stopped.core_message(), None);
}
