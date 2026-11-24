//! P3's real-V cases (S6b, D110 (2)): V's waits on P3's queue (C5, C5′), `NewPoWValidBlock` on V
//! (C8), and the actor's drop guard with V parked (D32), each through the node's own V, adapter
//! and listener. The node's network is replaced by one whose actor reads the chain through a
//! [`Gate`], so a test can hold the actor before a drain, or panic it there. The peer cases
//! (D122) are in `realv_peers`.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_clock::{Clock, NodeSeconds};
use dash_consensus::{Block, BlockId};
use dash_p2p::{NetEvents, PeerId};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{
    ActorReply, BlockResult, ChainHandle, RecordingAbort, genesis_block,
};
use dashd_mempool::api::MempoolHandle;
use dashd_p2p::Shared;
use dashd_p2p::processing::seams::{
    BlockPos, BlockRead, BlockSubmit, BlockWait, ChainAccess, ChainSnapshot, CoinJoinObjects,
    EntryFacts, EntryStatus, HeaderBatch, HeadersResult, Interrupted, LiveRead, TreeRead,
    ViewExpired, Work,
};
use dashd_p2p::processing::signals::{SIGNAL_QUEUE_MAX_ENTRIES, SignalQueue};

use super::amb::{branch, net_setup, node, regtest};
use super::chain::lock;
use super::realv_peers::delivered;
use super::{MasternodeRole, Net, NodeChain};
use crate::init::setup_server_args;
use crate::node::Node;

/// Every wait for V or the actor ends on its event; this guard only fails a real hang instead of
/// blocking CI, and no case relies on it (BL-926).
pub(super) const HANG_GUARD: Duration = Duration::from_secs(600);

/// The actor's chain reads, held or panicked at its `drain_snapshot`: the first thing every drain
/// does (P3 §3.6, D47). The peer cases also hold it at `wait_published` (a region's `Ahead`) and
/// at `process_headers`, before the batch reaches V (D122).
#[derive(Default)]
pub(super) struct Gate {
    inner: OnceLock<Arc<NodeChain>>,
    state: Mutex<GateState>,
    cond: Condvar,
    /// The actor's `process_block` calls.
    blocks: AtomicUsize,
}

#[derive(Default)]
pub(super) struct GateState {
    closed: bool,
    panic: bool,
    /// The actor waits at the gate.
    held: bool,
    /// The actor's `wait_published` waits while set.
    pub(super) published_shut: bool,
    /// The actor's `process_headers` waits while set.
    pub(super) headers_shut: bool,
    /// Where the actor waits, other than its drain.
    at: Option<&'static str>,
}

/// Whether this is the actor's thread: V calls `drain_snapshot` too, to stamp `BlockChecked`.
pub(super) fn on_actor() -> bool {
    std::thread::current().name() == Some("msghand")
}

impl Gate {
    fn chain(&self) -> &NodeChain {
        self.inner.get().expect("attached")
    }

    pub(super) fn set(&self, f: impl FnOnce(&mut GateState)) {
        f(&mut lock(&self.state));
        self.cond.notify_all();
    }

    /// How many blocks the actor has submitted.
    pub(super) fn blocks(&self) -> usize {
        self.blocks.load(Ordering::SeqCst)
    }

    /// Blocks until `f` holds on the state the actor sets at the gate.
    fn wait(&self, what: &str, f: impl Fn(&GateState) -> bool) {
        let deadline = Instant::now() + HANG_GUARD;
        let mut st = lock(&self.state);
        while !f(&st) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "hang guard: {what}");
            st = self.cond.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Blocks until the actor waits at the gate.
    pub(super) fn wait_for_the_actor(&self) {
        self.wait("the actor never reached the gate", |g| g.held);
    }

    /// Blocks until the actor waits at `at`, other than its drain.
    pub(super) fn wait_at(&self, at: &'static str) {
        self.wait(at, |g| g.at == Some(at));
    }

    /// The actor waits here while `shut` holds.
    fn hold(&self, at: &'static str, shut: impl Fn(&GateState) -> bool) {
        if !on_actor() {
            return;
        }
        let mut st = lock(&self.state);
        if !shut(&st) {
            return;
        }
        st.at = Some(at);
        self.cond.notify_all();
        while shut(&st) {
            st = self.cond.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.at = None;
    }
}

impl TreeRead for Gate {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.chain().lookup(hash)
    }

    fn entry(&self, id: BlockId) -> Option<EntryFacts> {
        self.chain().entry(id)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.chain().ancestor(id, height)
    }
}

impl ChainAccess for Gate {
    fn snapshot(&self) -> Arc<ChainSnapshot> {
        self.chain().snapshot()
    }

    fn valid_scripts(&self, snap: &ChainSnapshot, id: BlockId) -> Result<bool, ViewExpired> {
        self.chain().valid_scripts(snap, id)
    }

    fn locator(&self, id: Option<BlockId>) -> Vec<BlockHash> {
        self.chain().locator(id)
    }

    fn loading_blocks(&self) -> bool {
        self.chain().loading_blocks()
    }

    fn min_chain_work(&self) -> Work {
        self.chain().min_chain_work()
    }

    fn is_initial_block_download(&self, snap: &ChainSnapshot, now: NodeSeconds) -> bool {
        self.chain().is_initial_block_download(snap, now)
    }

    fn process_headers(
        &self,
        batch: HeaderBatch,
        from: PeerId,
    ) -> Result<HeadersResult, Interrupted> {
        self.hold("process_headers", |g| g.headers_shut);
        self.chain().process_headers(batch, from)
    }

    fn process_block(&self, req: BlockSubmit) -> Result<BlockWait, Interrupted> {
        self.blocks.fetch_add(1, Ordering::SeqCst);
        self.chain().process_block(req)
    }

    fn status(&self, snap: &ChainSnapshot, id: BlockId) -> Option<EntryStatus> {
        self.chain().status(snap, id)
    }

    fn in_active_chain(&self, snap: &ChainSnapshot, id: BlockId) -> bool {
        self.chain().in_active_chain(snap, id)
    }

    fn active_at(&self, snap: &ChainSnapshot, height: u32) -> Option<BlockId> {
        self.chain().active_at(snap, height)
    }

    fn last_common_ancestor(&self, a: BlockId, b: BlockId) -> BlockId {
        self.chain().last_common_ancestor(a, b)
    }

    /// Only the actor's: V stamps each `BlockChecked` with a `drain_snapshot` too.
    fn drain_snapshot(&self) -> (Arc<ChainSnapshot>, u64) {
        if !on_actor() {
            return self.chain().drain_snapshot();
        }
        let mut st = lock(&self.state);
        st.held = true;
        self.cond.notify_all();
        while st.closed && !st.panic {
            st = self.cond.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        st.held = false;
        assert!(!st.panic, "a planted actor panic");
        drop(st);
        self.chain().drain_snapshot()
    }

    fn wait_published(&self, seq: u64) -> Result<(), Interrupted> {
        self.hold("wait_published", |g| g.published_shut);
        self.chain().wait_published(seq)
    }

    fn read_block(&self, id: BlockId) -> BlockRead {
        self.chain().read_block(id)
    }

    fn read_block_at(&self, id: BlockId, pos: BlockPos) -> BlockRead {
        self.chain().read_block_at(id, pos)
    }

    fn read_live(&self, id: BlockId, failed_at: Option<BlockPos>) -> LiveRead {
        self.chain().read_live(id, failed_at)
    }

    fn prune_mode(&self) -> bool {
        self.chain().prune_mode()
    }
}

/// A node with no peers whose own network is stopped, and a second network over its V, whose
/// actor reads through a [`Gate`], started with its first drain done (the queue `Live`).
/// Dropping it opens the gate first, so a failed assertion stops the test instead of hanging
/// `Net::stop`'s join of a held actor.
pub(super) struct Rig {
    pub(super) gate: Arc<Gate>,
    abort: Arc<RecordingAbort>,
    pub(super) shared: Arc<Shared>,
    pub(super) v: ChainHandle,
    pub(super) net: Option<Net>,
    pub(super) node: Node,
}

/// The network's own arguments when it dials `connect` (`-connect` only; v2 is `NetSetup`'s),
/// with `-blocksonly` when it does not relay transactions.
fn dial_args(connect: &[SocketAddr], relay_txs: bool) -> ArgsManager {
    let args = ArgsManager::new();
    setup_server_args(&args);
    let addrs: Vec<_> = connect.iter().map(|a| format!("-connect={a}")).collect();
    // `-connect` soft-sets `-listen=0` in parameter interaction, which these arguments skip.
    let mut argv = vec!["rdashd", "-regtest", "-dnsseed=0", "-fixedseeds=0", "-listen=0"];
    argv.extend(addrs.iter().map(String::as_str));
    if !relay_txs {
        argv.push("-blocksonly");
    }
    args.parse_parameters(&argv).expect("argv");
    // Step 12 reads the onion target port.
    args.select_base_params(&args.get_chain_name().expect("regtest")).expect("regtest");
    args
}

impl Rig {
    /// A rig under `-blocksonly`; [`Rig::with_txs`] relays transactions.
    fn new(dir: &Path) -> Rig {
        Rig::with(dir, &[])
    }

    /// [`Rig::new`] whose network dials `connect` (`-connect`), V1 only (the scripted peers').
    pub(super) fn with(dir: &Path, connect: &[SocketAddr]) -> Rig {
        Rig::build(dir, connect, false, None, None)
    }

    /// [`Rig::with`] relaying transactions (the default), over `mempool` when given, with
    /// `coinjoin` as the processor's CoinJoin seam when given.
    pub(super) fn with_txs(
        dir: &Path,
        connect: &[SocketAddr],
        mempool: Option<Arc<dyn MempoolHandle>>,
        coinjoin: Option<Box<dyn CoinJoinObjects>>,
    ) -> Rig {
        Rig::build(dir, connect, true, mempool, coinjoin)
    }

    fn build(
        dir: &Path,
        connect: &[SocketAddr],
        relay_txs: bool,
        mempool: Option<Arc<dyn MempoolHandle>>,
        coinjoin: Option<Box<dyn CoinJoinObjects>>,
    ) -> Rig {
        // DIP3 above every test's chain: rdashd's CH3 guard refuses it, and regtest activates it
        // at 432 (P3 §6.3 item 6). V20 and MN_RR cannot be below it.
        let mut argv = vec![
            "-connect=0",
            "-dnsseed=0",
            "-fixedseeds=0",
            "-dip3params=5000:5000",
            "-testactivationheight=v20@5000",
            "-testactivationheight=mn_rr@5000",
        ];
        if !relay_txs {
            argv.push("-blocksonly");
        }
        let mut node = node(dir, &argv);
        node.stop_net();
        let setup = net_setup(125);
        let net_dir = dir.join("gated");
        std::fs::create_dir_all(&net_dir).unwrap();
        let clock: Arc<dyn Clock> = Arc::clone(&node.clock) as _;
        let dials = (!connect.is_empty()).then(|| dial_args(connect, relay_txs));
        let args = dials.as_ref().unwrap_or(&*node.args);
        let role = MasternodeRole::default();
        let mut net =
            Net::new(args, &regtest(), &setup, role, Arc::clone(&clock), &net_dir).unwrap();
        net.check_options(args, &crate::logging::Logger::new()).unwrap();
        net.coinjoin_seam = coinjoin;
        let (gate, abort) = (Arc::new(Gate::default()), Arc::new(RecordingAbort::default()));
        let chain = node.chain().expect("V runs");
        let v = chain.handle().clone();
        let tip = super::tip_mn_list::new(v.view_handle());
        net.attach_with(chain, None, clock, tip, None, None, mempool, |access| {
            assert!(gate.inner.set(access).is_ok(), "attached once");
            Arc::clone(&gate) as _
        })
        .expect("attach without masternode services");
        let ui = crate::ui::Ui::new(crate::init::test_logger().0);
        net.start(
            args,
            &ui,
            Arc::clone(&abort) as _,
            &Arc::new(dashd_masternode::meta::MasternodeMetaMan::new()),
        )
        .unwrap();
        let shared = Arc::clone(net.shared().unwrap());
        until(|| {
            shared.wake();
            shared.signals().queue.is_live()
        });
        Rig {
            gate,
            abort,
            shared,
            v,
            net: Some(net),
            node,
        }
    }

    pub(super) fn queue(&self) -> &SignalQueue {
        &self.shared.signals().queue
    }

    pub(super) fn submit(&self, block: &Arc<Block>) -> ActorReply<BlockResult> {
        self.v.process_block(Arc::clone(block), true, None, true)
    }

    fn tip_height(&self) -> u32 {
        self.v.view_handle().view().snap.tip.tip_height
    }

    /// One class R round trip: V answers it at a release point or inside a wait, each after it
    /// has published (D67).
    pub(super) fn served(&self) {
        self.v.view_handle().query(|_| ()).recv().expect("V serves");
    }

    fn recent(&self) -> Option<BlockHash> {
        self.shared.signals().recent().hash
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.gate.set(|g| {
            g.closed = false;
            g.published_shut = false;
            g.headers_shut = false;
        });
        drop(self.net.take());
    }
}

/// Polls `f` until it holds: for state no event announces to the test (the queue's counts, V's
/// view), under [`HANG_GUARD`].
pub(super) fn until(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + HANG_GUARD;
    while !f() {
        assert!(Instant::now() < deadline, "hang guard: timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A submitted block's reply: it fails, rather than hangs, if V never wakes.
pub(super) fn new_block(reply: &ActorReply<BlockResult>) -> bool {
    reply.recv_timeout(HANG_GUARD).expect("hang guard: V woke").expect("V replied").new_block
}

/// C5 and D32 (P3 §2.5 m-6): with the actor held before its drain, V's `BlockChecked` entries
/// fill the queue past its bound and V parks at its next release point, still serving queries
/// and, in IBD, accepts (CH1 A-1b R2-1). The actor's panic closes the queue through its drop
/// guard, and V resumes and connects every block.
#[test]
fn v_parks_at_a_full_queue_until_the_actor_s_panic_closes_it() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let p = regtest();
    let n = SIGNAL_QUEUE_MAX_ENTRIES + 64;
    let blocks = branch(&p, *genesis_block(&p).header(), 0, &vec![1; n + 3], 0x31);

    rig.gate.set(|g| g.closed = true);
    let mut replies: Vec<_> = blocks[..n].iter().map(|b| rig.submit(b)).collect();
    // Held at a closed gate, the actor stays there until the panic.
    rig.gate.wait_for_the_actor();
    until(|| {
        let full = rig.queue().held().0 > SIGNAL_QUEUE_MAX_ENTRIES;
        assert!(full || rig.tip_height() < n as u32, "V connected every block");
        full
    });
    rig.served();
    let parked = rig.tip_height();
    assert!((parked as usize) < n, "parked at {parked}");
    // Inside the wait V accepts blocks submitted after it parked (IBD), and connects none.
    replies.extend(blocks[n..].iter().map(|b| rig.submit(b)));
    let last = blocks.last().unwrap().hash();
    until(|| rig.v.view_handle().lookup(&last).is_some());
    rig.served();
    assert_eq!(rig.tip_height(), parked);
    assert!(!rig.queue().is_closed() && !rig.shared.panicked());

    rig.gate.set(|g| g.panic = true);
    assert!(replies.iter().all(new_block));
    assert_eq!(rig.tip_height(), n as u32 + 3);
    assert!(rig.queue().is_closed() && rig.shared.panicked());
    assert_eq!(rig.abort.messages(), ["The message handler thread panicked"]);
}

/// C5′ and C8 outside IBD: V accepts a block only once the actor has applied every earlier
/// `BlockChecked` (DEC-240, D55), and the accept runs `NewPoWValidBlock` on V, so `recent` is
/// that block before its reply.
#[test]
fn an_accept_outside_ibd_waits_for_the_actor_then_runs_new_pow_valid_block() {
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let p = regtest();
    let blocks = branch(&p, *genesis_block(&p).header(), 0, &[1; 5], 0x32);
    // Out of IBD: the tip is a minute old on the node's clock.
    let clock = &rig.node.clock;
    clock.set_mocktime(i64::from(blocks[4].header().raw().time()) + 60);
    assert!(blocks[..3].iter().all(|b| new_block(&rig.submit(b))));
    let view = rig.v.view_handle();
    // A reply can come before the view that clears the latch is published.
    until(|| {
        rig.served();
        !view.is_initial_block_download(&view.view().snap, clock.node_time())
    });
    // Each block's `BlockConnected` reaches the queue from the signals thread, after its reply
    // (BL-926): every one has, and the actor has applied it, before the gate closes.
    let signals = rig.node.chain().expect("V runs").signals().clone();
    delivered(&signals);
    until(|| rig.queue().held().0 == 0);

    // X's `BlockChecked` and `BlockConnected` stay outstanding while the actor is held.
    rig.gate.set(|g| g.closed = true);
    assert!(new_block(&rig.submit(&blocks[3])));
    assert_eq!(rig.recent(), Some(blocks[3].hash()));
    delivered(&signals);
    rig.gate.wait_for_the_actor();
    assert_eq!(rig.queue().held().0, 2);

    // Y waits before its accept, while V serves a query sent after it: not indexed, not
    // announced.
    let y = rig.submit(&blocks[4]);
    rig.served();
    assert!(view.lookup(&blocks[4].hash()).is_none());
    assert_eq!(rig.recent(), Some(blocks[3].hash()));

    rig.gate.set(|g| g.closed = false);
    assert!(new_block(&y));
    assert_eq!(rig.recent(), Some(blocks[4].hash()));
    assert_eq!(rig.tip_height(), 5);
    assert!(rig.abort.messages().is_empty());
}

/// `getnetworkinfo`'s node half on a real network (BL-391 review r1 finding 1): the RPC slot
/// reads the core's `mapLocalHost` (address, port and score), the peer manager's identity and
/// relay (off: the rig runs `-blocksonly`), and no connection.
#[test]
fn the_rpc_slot_reads_the_network_it_is_attached_to() {
    use dash_p2p::local::LocalScore;
    use dash_p2p::{NetAddr, Service};
    use dashd_rpc::peers::{ConnectionCounts, LocalAddress, NodeNet as _};

    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::new(dir.path());
    let net = rig.net.as_ref().unwrap();
    let slot = crate::rpc::NetSlot::default();
    slot.attach(net.core(), &rig.shared, net.connman().expect("started"));
    let ours = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999);
    assert!(net.core().add_local(ours, LocalScore::Manual));
    let info = slot.network_info().expect("attached");
    let expected = LocalAddress {
        address: "1.2.3.4".into(),
        port: 9999,
        score: LocalScore::Manual as i32,
    };
    assert_eq!(info.local_addresses, [expected]);
    assert_eq!(info.subversion, format!("/rdashd:{}/", env!("CARGO_PKG_VERSION")));
    assert!(!info.local_relay);
    assert_eq!(info.connections, ConnectionCounts::default());
    assert!(info.network_active);
    slot.detach();
}

/// `getnetworkinfo`'s counts and `getpeerinfo`'s peers skip a peer marked for disconnection, as
/// Core's `GetNodeCount` and `GetNodeStats` skip `fDisconnect` (BL-391 review r1 finding 1,
/// BL-394 r2 n-3): the slot reads the table while the marked peer is still in it, before the
/// sweep removes it. The peer is a `-connect` to a listener that never answers, an outbound
/// connection whatever its handshake.
// Core: net.cpp:4520-4540 CConnman::GetNodeCount; net.cpp:4563-4576 CConnman::GetNodeStats
#[test]
fn the_rpc_slot_skips_a_peer_being_disconnected() {
    use dash_p2p::Why;
    use dashd_rpc::peers::{ConnectionCounts, NodeNet as _, PeerInfo};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let dir = tempfile::tempdir().unwrap();
    let rig = Rig::with(dir.path(), &[listener.local_addr().expect("addr")]);
    let net = rig.net.as_ref().unwrap();
    let core = net.core();
    let slot = crate::rpc::NetSlot::default();
    slot.attach(net.core(), &rig.shared, net.connman().expect("started"));
    let live = || core.peers().iter().find(|p| !p.disconnect_requested()).map(|p| p.id());
    let listed = |infos: &[PeerInfo], peer| infos.iter().any(|info| info.id == peer);
    let one_out = ConnectionCounts {
        inbound: 0,
        outbound: 1,
    };
    // The sweep may remove the marked peer before the read; `-connect` dials it again.
    let skipped = (0..50).any(|_| {
        until(|| live().is_some());
        let peer = live().expect("dialled");
        assert_eq!(slot.network_info().expect("attached").connections, one_out);
        assert!(listed(&slot.peer_info().expect("attached"), peer), "the peer before its mark");
        core.disconnect(peer, Why::Processor("test"));
        let counts = slot.network_info().expect("attached").connections;
        let infos = slot.peer_info().expect("attached");
        // Removal is final: a peer still listed after the reads was listed during them.
        if !core.peers().iter().any(|p| p.id() == peer) {
            return false;
        }
        assert_eq!(counts, ConnectionCounts::default(), "the marked peer was counted");
        assert!(!listed(&infos, peer), "getpeerinfo listed the marked peer");
        true
    });
    assert!(skipped, "the sweep removed the marked peer before every read");
    slot.detach();
}
