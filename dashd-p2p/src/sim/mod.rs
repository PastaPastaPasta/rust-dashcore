//! The deterministic simulation harness (P2a §6.7): a virtual clock, a header-arena chain, and
//! SB6's network core over in-memory sockets, with the message loop driven one pass at a time
//! on the test's thread.
//!
//! The network core's socket tasks run on a private tokio runtime, so bytes reach a peer's
//! receive queue asynchronously; [`Sim::deliver`] waits until the core has read them before the
//! test runs a pass, so what a pass sees is fixed by the script.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dash_clock::{Clock, NodeSeconds};
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::rng::FastRandom;
use dash_p2p::{
    Address, Attach, CommandString, ConnectionType, LocalIdentity, MemSocket, NetAddr, NetBlob,
    NetConfig, NetCore, NetEvents, NetSocket, NetStore, PeerHandle, PeerId, RawMsg, Resolver,
    Service, ServiceFlags, Why,
};
use dash_p2p_transport::{Magic, OutMsg, Transport, TransportKind};
use dash_params::ChainSetup;
use parking_lot::Mutex;

use crate::fingerprint::{IdentityOptions, local_services};
use crate::processing::logic::{ProcessorOptions, Seams};
use crate::processing::peerman::{MessageLoop, Shared};
use crate::processing::registry::Registry;
use crate::processing::seams::Interrupted;
use crate::processing::signals::ParkingWait;

pub mod block_download_model;
pub mod chain;
pub mod cmpct_reconstruct;
pub mod transcript;

pub use chain::{AcceptHook, AnnounceHook, SimChain, SimListener, SimRead};

/// The processor as the `sim` chain's listener (P3 §2.5).
impl SimListener for Shared {
    fn block_checked(
        &self,
        hash: dash_primitives::BlockHash,
        invalid: Option<dash_consensus::BlockValidationError>,
        ibd: bool,
    ) {
        Shared::block_checked(self, hash, invalid, ibd);
    }

    fn published(&self) {
        self.signals().bells.ring_all();
    }

    fn wait_quiescent(&self) -> Result<(), Interrupted> {
        self.signals().queue.wait_quiescent(&mut ParkingWait)
    }

    fn new_pow_valid_block(
        &self,
        facts: &crate::processing::seams::EntryFacts,
        block: &Arc<dash_consensus::Block>,
        tree: &dyn crate::TreeRead,
        stamp: u64,
    ) {
        Shared::new_pow_valid_block(self, facts, block, tree, stamp);
    }

    fn block_connected(&self, now: i64) {
        Shared::block_connected(self, now);
    }
}

/// Core's `NodeClock` under a mock time, with a steady clock that moves only when told.
#[derive(Debug)]
pub struct VirtualClock {
    node_us: AtomicI64,
    base: Instant,
    steady_us: AtomicI64,
}

impl VirtualClock {
    /// Mock time `t` seconds; the steady clock at its origin.
    pub fn at(t: i64) -> Arc<Self> {
        Arc::new(Self {
            node_us: AtomicI64::new(t * 1_000_000),
            base: Instant::now(),
            steady_us: AtomicI64::new(0),
        })
    }

    /// Moves both clocks forward by `d` (Core's `SetMockTime` plus the passage of steady time).
    pub fn advance(&self, d: Duration) {
        let us = d.as_micros() as i64;
        self.node_us.fetch_add(us, Ordering::SeqCst);
        self.steady_us.fetch_add(us, Ordering::SeqCst);
    }

    /// Moves the steady clock alone forward by `d`, leaving the mock time where it is.
    fn advance_steady(&self, d: Duration) {
        self.steady_us.fetch_add(d.as_micros() as i64, Ordering::SeqCst);
    }

    /// Sets the mock time to `t` seconds, leaving the steady clock (and so the scheduler) where
    /// it is: Core's `setmocktime`.
    pub fn set_mock_time(&self, t: i64) {
        self.node_us.store(t * 1_000_000, Ordering::SeqCst);
    }
}

impl Clock for VirtualClock {
    fn node_now(&self) -> NodeSeconds {
        NodeSeconds(self.node_us.load(Ordering::SeqCst).div_euclid(1_000_000))
    }

    fn node_now_micros(&self) -> i64 {
        self.node_us.load(Ordering::SeqCst)
    }

    fn steady(&self) -> Instant {
        self.base + Duration::from_micros(self.steady_us.load(Ordering::SeqCst) as u64)
    }
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

/// The core's store: each image as last saved, in memory (`banlist.json` for P4a's `banlist`
/// records). Nothing is stored at the start, as in a fresh datadir.
#[derive(Debug, Default)]
pub struct SimStore {
    images: Mutex<HashMap<NetBlob, Vec<u8>>>,
}

impl SimStore {
    /// The image as last saved, `None` when never saved or removed.
    pub fn saved(&self, blob: NetBlob) -> Option<Vec<u8>> {
        self.images.lock().get(&blob).cloned()
    }
}

impl NetStore for SimStore {
    fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        Ok(self.saved(blob))
    }

    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
        self.images.lock().insert(blob, bytes.to_vec());
        Ok(())
    }

    fn remove(&self, blob: NetBlob) -> io::Result<()> {
        self.images.lock().remove(&blob);
        Ok(())
    }

    fn backup(&self, blob: NetBlob) -> io::Result<()> {
        self.remove(blob)
    }
}

/// One scripted peer: the far end of its in-memory socket.
pub struct SimPeer {
    /// The node's connection entry.
    pub handle: PeerHandle,
    /// What the scripted peer writes and reads.
    pub sock: MemSocket,
    /// The node's end, whose send budget models a peer that reads only when told.
    node_sock: Arc<MemSocket>,
    /// The bytes [`Sim::sent`] has taken from `sock` so far.
    taken: AtomicU64,
    /// The peer's own transport, V1 or (for [`Sim::connect_v2`], [`Sim::accept_v2`]) V2.
    end: Mutex<PeerEnd>,
    /// Whether the peer speaks V2.
    v2: bool,
}

/// How a V2 message's type went on the wire: Core's one-byte short ID, or the 13-byte long form
/// (a zero byte, then the 12-byte type).
// Core: net.cpp:1566-1599 V2Transport::SetMessageToSend
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeForm {
    /// A short ID.
    Short,
    /// The long form.
    Long,
}

impl TypeForm {
    /// `short` or `long`, the transcript format's word.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Long => "long",
        }
    }
}

/// A message the node sent a scripted peer; `form` is its type's encoding on a V2 connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The message type.
    pub cmd: CommandString,
    /// The payload.
    pub payload: Vec<u8>,
    /// The type's encoding, `None` over V1.
    pub form: Option<TypeForm>,
}

/// A scripted peer's end of the connection: the production transport, which reads the node's
/// bytes as a Core peer does (V1: the network's magic, the length cap, the checksum, the
/// command's charset and padding; V2: BIP324), and what it decoded that [`Sim::sent`] has not
/// returned.
struct PeerEnd {
    t: Transport,
    inbox: Vec<Sent>,
    /// V1 bytes taken that no whole frame used yet: an incomplete trailing frame.
    partial: u64,
}

impl PeerEnd {
    fn new(t: Transport) -> Mutex<Self> {
        Mutex::new(Self {
            t,
            inbox: Vec::new(),
            partial: 0,
        })
    }

    /// Decodes `bytes` the node wrote. Nothing is dropped: a frame, packet or message the
    /// transport refuses (or drops, as Core drops a bad checksum or command) panics, because no
    /// transcript could show it. Only a V2 node that refuses the handshake writes bytes the
    /// peer cannot parse; its closing is what the transcript shows.
    fn take(&mut self, bytes: &[u8]) {
        let Self {
            t,
            inbox,
            partial,
        } = self;
        let v2 = t.info().kind != TransportKind::V1;
        if !v2 {
            *partial += bytes.len() as u64;
        }
        let result = t.receive(bytes, 0, |m| {
            let m = m.unwrap_or_else(|r| panic!("the node sent a message its peer drops: {r:?}"));
            let form = if !v2 {
                *partial -= m.raw_size as u64;
                None
            } else if m.raw_size - m.payload.len() - V2_EXPANSION == 1 {
                Some(TypeForm::Short)
            } else {
                Some(TypeForm::Long)
            };
            inbox.push(Sent {
                cmd: m.command,
                payload: m.payload,
                form,
            });
        });
        // Judged after the call: the chunk that ends a V2 handshake can carry app packets too.
        let kind = t.info().kind;
        if let Err(e) = result {
            assert!(
                kind == TransportKind::Detecting,
                "the node sent bytes its peer refuses: {e:?}"
            );
        }
        assert!(!v2 || kind != TransportKind::V1, "the node spoke V1 to a V2 peer");
    }
}

/// Everything `t` has to send, marked sent.
fn take_bytes(t: &mut Transport) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let data = t.bytes_to_send(false).data.to_vec();
        if data.is_empty() {
            return out;
        }
        t.mark_bytes_sent(data.len());
        out.extend(data);
    }
}

/// BIP324's bytes per packet beyond its contents: the 3-byte length, the header byte and the
/// 16-byte tag (Core's `BIP324Cipher::EXPANSION`).
const V2_EXPANSION: usize = 20;

impl SimPeer {
    /// Hands `bytes` the node wrote to the peer's transport ([`PeerEnd::take`]), counting them
    /// taken.
    fn feed(&self, bytes: &[u8]) {
        self.taken.fetch_add(bytes.len() as u64, Ordering::SeqCst);
        self.end.lock().take(bytes);
    }

    /// V1 bytes the node wrote that no whole frame has used yet: an incomplete trailing frame.
    pub fn partial_bytes(&self) -> u64 {
        self.end.lock().partial
    }

    /// Lets a peer that [`Sim::attach_at`] started with a zero send budget read: the node's end
    /// may write `max` more bytes, then nothing again. Returns once it has written them or has
    /// nothing left to write.
    ///
    /// # Panics
    ///
    /// When the writer does neither within [`PATIENCE`].
    pub fn read(&self, max: u64) {
        let before = self.handle.send_bytes();
        self.node_sock.set_send_budget(Some(usize::try_from(max).unwrap_or(usize::MAX)));
        until(|| self.handle.send_bytes() - before == max || self.handle.send_queue_empty());
        self.node_sock.set_send_budget(Some(0));
    }
}

/// The node's processor over SB6's core, driven a pass at a time.
pub struct Sim {
    /// The runtime the core's socket tasks run on.
    pub rt: tokio::runtime::Runtime,
    /// The network core.
    pub core: Arc<NetCore>,
    /// The processor's shared half.
    pub shared: Arc<Shared>,
    /// The message loop.
    pub lp: MessageLoop,
    /// The clock.
    pub clock: Arc<VirtualClock>,
    /// The chain.
    pub chain: Arc<SimChain>,
    /// The core's images.
    pub store: Arc<SimStore>,
}

/// How long the sim waits for the node's tasks before it calls them stuck: long enough that a
/// starved box (load 190) is slow rather than failing.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// More than the core's disconnect linger (Core's 100 ms).
const LINGER: Duration = Duration::from_millis(101);
/// How long [`Sim::remove_disconnected`] lets the sweep run before it moves the clock again.
const RELINGER: Duration = Duration::from_millis(200);
/// The most [`LINGER`] steps [`Sim::remove_disconnected`] moves the steady clock: under the
/// sweep's 1 s `InactivityCheck` period, so the check never comes due in a removal unless the
/// sweep starts a linger only after the last step.
const MAX_LINGERS: u32 = 9;

/// The mock time `sim` starts at.
pub const SIM_START: i64 = 1_700_000_000;

impl Sim {
    /// A node on `params` with `identity`, the M2 seams and stubs, and a deterministic shuffle.
    pub fn new(params: Arc<ChainSetup>, identity: LocalIdentity) -> Self {
        Self::with(
            params,
            identity,
            ProcessorOptions::default(),
            Registry::new(),
            Seams::default(),
            |_| {},
        )
    }

    /// As [`Sim::new`], with processor options, a registry, the seams and a hook on the core's
    /// config.
    pub fn with(
        params: Arc<ChainSetup>,
        identity: LocalIdentity,
        opts: ProcessorOptions,
        registry: Registry,
        seams: Seams,
        cfg_hook: impl FnOnce(&mut NetConfig),
    ) -> Self {
        let chain = SimChain::new(Arc::clone(&params));
        Self::with_chain(params, identity, opts, registry, seams, SIM_START, |_| chain, cfg_hook)
    }

    /// As [`Sim::with`], with the mock time starting at `start` and the chain `chain` builds on
    /// the clock (e.g. [`SimChain::validating`]).
    #[allow(clippy::too_many_arguments)]
    pub fn with_chain(
        params: Arc<ChainSetup>,
        identity: LocalIdentity,
        opts: ProcessorOptions,
        registry: Registry,
        seams: Seams,
        start: i64,
        chain: impl FnOnce(&Arc<VirtualClock>) -> Arc<SimChain>,
        cfg_hook: impl FnOnce(&mut NetConfig),
    ) -> Self {
        Self::with_chain_seams(params, identity, opts, registry, start, chain, |_| seams, cfg_hook)
    }

    /// As [`Sim::with_chain`], with the seams built over the chain once it exists (a host's
    /// address relay reads the chain the sim validates on).
    #[allow(clippy::too_many_arguments)]
    pub fn with_chain_seams(
        params: Arc<ChainSetup>,
        identity: LocalIdentity,
        opts: ProcessorOptions,
        registry: Registry,
        start: i64,
        chain: impl FnOnce(&Arc<VirtualClock>) -> Arc<SimChain>,
        seams: impl FnOnce(&Arc<SimChain>) -> Seams,
        cfg_hook: impl FnOnce(&mut NetConfig),
    ) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("sim runtime");
        let clock = VirtualClock::at(start);
        let chain = chain(&clock);
        let seams = seams(&chain);
        let shared = Shared::new(identity, opts.clone(), chain.clone());
        let listener: Arc<dyn SimListener> = shared.clone();
        chain.set_listener(Arc::downgrade(&listener));
        chain.set_clock(clock.clone());
        let mut cfg = NetConfig::new(params, local_services(&IdentityOptions::new()));
        cfg.deterministic = true;
        cfg.peer_timeout = 3600;
        cfg.blocks_only = opts.blocks_only;
        cfg_hook(&mut cfg);
        let events: Arc<dyn NetEvents> = shared.clone();
        let store = Arc::new(SimStore::default());
        let core = NetCore::new(
            cfg,
            rt.handle().clone(),
            store.clone(),
            Arc::new(NoResolver),
            clock.clone(),
            events,
        )
        .expect("sim network core");
        let lp = MessageLoop::new(
            Arc::clone(&core),
            Arc::clone(&shared),
            registry,
            seams,
            FastRandom::deterministic(),
        );
        Self {
            rt,
            core,
            shared,
            lp,
            clock,
            chain,
            store,
        }
    }

    /// Connects a scripted peer at 10.0.0.`n`:19899 as `conn_type` over V1.
    pub fn connect(&self, n: u8, conn_type: ConnectionType, permissions: Permissions) -> SimPeer {
        self.connect_with_services(n, conn_type, permissions, ServiceFlags::NODE_NETWORK)
    }

    /// As [`Sim::connect`], with the services the node believes the peer offers (addrman's
    /// entry, which `version`'s `addr_recv` echoes; Core's `addconnection` and inbound
    /// connections use `NODE_NONE`).
    pub fn connect_with_services(
        &self,
        n: u8,
        conn_type: ConnectionType,
        permissions: Permissions,
        services: ServiceFlags,
    ) -> SimPeer {
        self.attach(n, conn_type, permissions, services, false)
    }

    /// Connects an inbound masternode connection at 10.0.0.`n`:19899 over V1.
    pub fn connect_masternode(&self, n: u8) -> SimPeer {
        self.attach(n, ConnectionType::Inbound, Permissions::NONE, ServiceFlags::NODE_NETWORK, true)
    }

    fn attach(
        &self,
        n: u8,
        conn_type: ConnectionType,
        permissions: Permissions,
        services: ServiceFlags,
        masternode: bool,
    ) -> SimPeer {
        let service = Service {
            addr: NetAddr::ipv4([10, 0, 0, n]),
            port: 19899,
        };
        self.attach_at(service, conn_type, permissions, services, masternode, false)
    }

    /// Connects a scripted peer at 10.0.0.`n`:19899 as `conn_type` over V2 (Core's
    /// `use_v2transport`), with the services the node believes it offers, and runs the BIP324
    /// handshake (the peer responds) until both ends speak V2 or the node gives up on the
    /// connection.
    pub fn connect_v2(&self, n: u8, conn_type: ConnectionType, services: ServiceFlags) -> SimPeer {
        let service = Service {
            addr: NetAddr::ipv4([10, 0, 0, n]),
            port: 19899,
        };
        let mut peer =
            self.attach_with(service, conn_type, Permissions::NONE, services, false, false, true);
        self.v2_handshake(&mut peer, false);
        peer
    }

    /// Connects a scripted peer at `service` over V1, as [`Sim::connect_with_services`] does;
    /// `controlled`: a peer that reads nothing until told to ([`SimPeer::read`]).
    pub fn attach_at(
        &self,
        service: Service,
        conn_type: ConnectionType,
        permissions: Permissions,
        services: ServiceFlags,
        masternode: bool,
        controlled: bool,
    ) -> SimPeer {
        self.attach_with(service, conn_type, permissions, services, masternode, controlled, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_with(
        &self,
        service: Service,
        conn_type: ConnectionType,
        permissions: Permissions,
        services: ServiceFlags,
        masternode: bool,
        controlled: bool,
        use_v2: bool,
    ) -> SimPeer {
        let (ours, theirs) = MemSocket::pair();
        if controlled {
            ours.set_send_budget(Some(0));
        }
        let ours = Arc::new(ours);
        let handle = self.core.attach(Attach {
            sock: ours.clone(),
            addr: Address {
                service,
                services,
                time: 0,
            },
            addr_bind: Service::default(),
            dest: None,
            conn_type,
            inbound_onion: false,
            permissions,
            use_v2,
            grant: None,
            masternode,
            masternode_probe: false,
        });
        SimPeer {
            handle,
            sock: theirs,
            node_sock: ours,
            taken: AtomicU64::new(0),
            end: PeerEnd::new(Transport::v1(self.magic())),
            v2: false,
        }
    }

    /// Peer `n` connects to the node from 127.0.0.1:`40000 + n`, as a capture tool's peer dials a
    /// dashd on loopback, through SB6's [`NetCore::accept`] (Core's
    /// `CreateNodeFromAcceptedSocket`: `-whitelist`, bans, the inbound limit and
    /// `AttemptToEvictConnection`). `None` when the node drops the connection. The address
    /// carries `NODE_NONE`, as Core's `AcceptConnection` gives it (P2b D8).
    pub fn accept(&self, n: u8, permissions: Permissions) -> Option<SimPeer> {
        self.accept_from(n, NetAddr::ipv4([127, 0, 0, 1]), permissions)
    }

    /// As [`Sim::accept`], from `from`:`40000 + n` (P4a's peers dial from public addresses in a
    /// network namespace), with the `permissions` of the listener it dialled (`-whitebind`).
    pub fn accept_from(&self, n: u8, from: NetAddr, permissions: Permissions) -> Option<SimPeer> {
        let (ours, theirs) = MemSocket::pair();
        let ours = Arc::new(ours);
        let addr = Address {
            service: Service {
                addr: from,
                port: 40000 + u16::from(n),
            },
            services: ServiceFlags::NONE,
            time: 0,
        };
        let bind = Service {
            addr: NetAddr::ipv4([127, 0, 0, 1]),
            port: self.core.config().listen_port,
        };
        let handle = self.core.accept(ours.clone(), addr, bind, permissions, false, false)?;
        Some(SimPeer {
            handle,
            sock: theirs,
            node_sock: ours,
            taken: AtomicU64::new(0),
            end: PeerEnd::new(Transport::v1(self.magic())),
            v2: false,
        })
    }

    /// As [`Sim::accept`], with a peer that speaks V2: it sends its key and garbage at once (the
    /// initiator), and the handshake runs until both ends speak V2 or the node gives up on the
    /// connection (a node without `NODE_P2P_V2` reads the key as V1 bytes).
    pub fn accept_v2(&self, n: u8, permissions: Permissions) -> Option<SimPeer> {
        let mut peer = self.accept(n, permissions)?;
        self.v2_handshake(&mut peer, true);
        Some(peer)
    }

    /// The network's message start, as the transports take it.
    fn magic(&self) -> Magic {
        Magic::from_bytes(self.chain.params().message_start)
    }

    /// Gives `peer` a V2 transport (`initiator`: it sends its key first), then moves bytes both
    /// ways between it and the node until both ends speak V2 (the peer has verified the node's
    /// version packet, and the node the peer's) or the node marks the connection to disconnect.
    /// What the node sends after its version packet is kept for [`Sim::sent`].
    fn v2_handshake(&self, peer: &mut SimPeer, initiator: bool) {
        peer.end = PeerEnd::new(Transport::v2(self.magic(), initiator).expect("the random source"));
        peer.v2 = true;
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.pump(peer);
            let peer_ready = peer.end.lock().t.info().kind == TransportKind::V2;
            if peer_ready && peer.handle.transport_info().kind == TransportKind::V2 {
                return;
            }
            if peer.handle.disconnect_requested() {
                return;
            }
            assert!(Instant::now() < deadline, "the V2 handshake did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// One exchange: what the node wrote goes into the peer's transport (whole messages into
    /// its inbox), then what the peer's transport has to send (a V2 handshake) goes to the node.
    fn pump(&self, peer: &SimPeer) {
        peer.feed(&peer.sock.drain());
        let data = take_bytes(&mut peer.end.lock().t);
        if !data.is_empty() {
            peer.sock.inject(&data);
        }
    }

    /// Runs `cmd` from `peer` through `ProcessMessage` after the gates (SB6's gates are the
    /// network core's; this drives the processor's branch directly).
    pub fn admitted(&mut self, peer: &SimPeer, cmd: &str, payload: Vec<u8>) -> Result<(), Throw> {
        let msg = RawMsg {
            cmd: CommandString::from_static(cmd),
            raw_size: V1_HEADER + payload.len(),
            payload,
            recv_us: self.clock.node_now_micros(),
        };
        self.lp.logic_mut().process_admitted(&peer.handle, &msg)
    }

    /// Runs the handshake from `peer`'s side: its `version`, a pass, then (unless the node
    /// disconnected, as it does a feeler) its `verack` and a pass. Returns what the node sent
    /// meanwhile (its own `version` first when outbound).
    pub fn handshake(
        &mut self,
        peer: &SimPeer,
        v: PeerVersionMsg,
    ) -> Vec<(CommandString, Vec<u8>)> {
        self.deliver(peer, &[("version", v.payload())]);
        self.pass();
        if !peer.handle.disconnect_requested() {
            self.deliver(peer, &[("verack", Vec::new())]);
            self.pass();
        }
        self.sent(peer)
    }

    /// One pass of the message loop; returns `fMoreWork`.
    pub fn pass(&mut self) -> bool {
        self.lp.run_one_pass()
    }

    /// One pass shuffled by `rng`, without the scheduled tasks ([`MessageLoop::pass_with`]).
    pub fn pass_with(&mut self, rng: &mut FastRandom) -> bool {
        self.lp.pass_with(rng)
    }

    /// Removes every peer marked for disconnection now, as Core's `DisconnectNodes` and
    /// `FinalizeNode` do at the end of a socket-handler turn: it returns once the processor has
    /// finalized each of them. The core's sweep removes them once their linger has passed, so
    /// this moves the steady clock past it (by less than a second in all), then puts it back:
    /// the sweep's once-a-second `InactivityCheck` never comes due in a script, which runs it
    /// (as Core's sessions do) only when it says so. The mock time stays. A sweep starved past
    /// the last step starts its linger late; the clock then moves to that linger's end, so it
    /// still expires (P2a-net D35: fuzz crash 54c54dd34a18).
    ///
    /// # Panics
    ///
    /// When the sweep and the processor do not finish within [`PATIENCE`].
    pub fn remove_disconnected(&mut self) {
        let mut pending: Vec<PeerId> =
            self.core.peers().iter().filter(|p| p.disconnect_requested()).map(|p| p.id()).collect();
        if pending.is_empty() {
            self.lp.drain_finalized();
            return;
        }
        let steady = self.clock.steady_us.load(Ordering::SeqCst);
        let deadline = Instant::now() + PATIENCE;
        let (mut lingers, mut lingered) = (1, Instant::now());
        self.clock.advance_steady(LINGER);
        loop {
            // The sweep takes a peer out of the table before the processor's `remove_peer`
            // queues it for `FinalizeNode`, so wait for the finalization itself.
            let done = self.lp.drain_finalized();
            pending.retain(|id| !done.contains(id));
            if pending.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "the sweep did not remove {pending:?}");
            // A linger the sweep started after the clock moved needs another step; one started
            // after the last step needs the clock at its end.
            if lingered.elapsed() > RELINGER {
                if lingers < MAX_LINGERS {
                    self.clock.advance_steady(LINGER);
                    (lingers, lingered) = (lingers + 1, Instant::now());
                } else {
                    let now = self.clock.steady();
                    let late = pending.iter().filter_map(|&id| self.core.peer(id)?.linger_until());
                    if let Some(until) = late.filter(|&u| u > now).max() {
                        self.clock.advance_steady(until - now);
                    }
                }
            }
            // Already marked: this only wakes the sweep.
            for &id in &pending {
                self.core.disconnect(id, Why::Processor("sim removal"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.clock.steady_us.store(steady, Ordering::SeqCst);
    }

    /// Writes V1 frames from `peer` and waits until the core has queued them for processing, or
    /// has marked the peer for disconnection.
    ///
    /// `nRecvBytes` counts bytes before the transport parses them, so this waits for the
    /// per-type receive stats instead. SB6 counts them while holding the receive-queue lock, so
    /// `recv_queue_empty` (which takes that lock) then sees the frames queued. The frames are
    /// valid V1 (right checksum, printable command), so none is dropped uncounted (D31).
    pub fn deliver(&self, peer: &SimPeer, frames: &[(&str, Vec<u8>)]) {
        let counted = || peer.handle.byte_stats().recv.values().sum::<u64>();
        let before = counted();
        let bytes: Vec<u8> = if peer.v2 {
            // Each packet whole: Core counts a V2 message's bytes as its packet's.
            let t = &mut peer.end.lock().t;
            let mut out = Vec::new();
            for (cmd, payload) in frames {
                let msg = OutMsg::new(CommandString::from_static(cmd), payload.clone());
                assert!(t.set_message_to_send(msg).is_ok(), "the peer's transport is busy");
                out.extend(take_bytes(t));
            }
            out
        } else {
            frames
                .iter()
                .flat_map(|(cmd, payload)| v1_frame(self.chain.params(), cmd, payload))
                .collect()
        };
        peer.sock.inject(&bytes);
        let want = before + bytes.len() as u64;
        // A paused or marked peer's bytes stay in the socket, as Core leaves them.
        until(|| {
            (counted() >= want && !peer.handle.recv_queue_empty())
                || peer.handle.disconnect_requested()
                || peer.handle.pause_recv()
        });
    }

    /// Writes `bytes` from `peer` as they are (a framing case: a bad checksum or command, an
    /// oversized header, a wrong magic), and waits until the core has read them (`nRecvBytes`:
    /// a header alone completes no message) or has marked the peer for disconnection.
    pub fn deliver_raw(&self, peer: &SimPeer, bytes: &[u8]) {
        let counted = || peer.handle.recv_bytes();
        let want = counted() + bytes.len() as u64;
        peer.sock.inject(bytes);
        until(|| counted() >= want || peer.handle.disconnect_requested());
    }

    /// The frames the node sent `peer` since the last call: the send queue and the transport are
    /// empty, and everything the writer counted in `nSendBytes` has arrived. A peer whose socket
    /// the node has shut (a failed write, or the end of its linger) keeps its queue, so for one
    /// the bytes that arrived are all.
    ///
    /// # Panics
    ///
    /// When the bytes do not arrive within [`PATIENCE`].
    pub fn sent(&self, peer: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
        self.sent_at_least(peer, 0)
    }

    /// The whole V1 frames at the start of `bytes`, read off a peer's socket by the caller,
    /// decoded as the peer's own V1 transport decodes them (this network's magic, the length
    /// cap, the checksum, the command); an incomplete trailing frame is left out.
    ///
    /// # Panics
    ///
    /// On bytes or a message the peer's transport refuses.
    pub fn v1_frames(&self, bytes: &[u8]) -> Vec<(CommandString, Vec<u8>)> {
        let mut end = PeerEnd::new(Transport::v1(self.magic())).into_inner();
        end.take(bytes);
        end.inbox.into_iter().map(|m| (m.cmd, m.payload)).collect()
    }

    /// As [`Sim::sent`], first waiting until at least `n` frames have arrived, for checks that
    /// expect output (the writer task may not run within the quiet period on a loaded box).
    ///
    /// # Panics
    ///
    /// When the frames do not arrive within [`PATIENCE`].
    pub fn sent_at_least(&self, peer: &SimPeer, n: usize) -> Vec<(CommandString, Vec<u8>)> {
        self.sent_messages(peer, n).into_iter().map(|m| (m.cmd, m.payload)).collect()
    }

    /// As [`Sim::sent_at_least`], with each message's type encoding on a V2 connection.
    ///
    /// # Panics
    ///
    /// When the messages do not arrive within [`PATIENCE`].
    pub fn sent_messages(&self, peer: &SimPeer, n: usize) -> Vec<Sent> {
        let deadline = Instant::now() + PATIENCE;
        let inbox = || peer.end.lock().inbox.len();
        while inbox() < n {
            self.pump(peer);
            assert!(Instant::now() < deadline, "{n} messages did not arrive");
            std::thread::sleep(Duration::from_millis(2));
        }
        loop {
            // In this order: an empty queue means its bytes are already counted, and the count
            // read before draining is all that can have arrived.
            let empty = peer.handle.send_queue_empty();
            let counted = peer.handle.send_bytes();
            self.pump(peer);
            let arrived = peer.taken.load(Ordering::SeqCst) == counted;
            let mut late = Vec::new();
            let shut = !empty && node_end_shut(&peer.sock, &mut late);
            peer.feed(&late);
            if arrived && late.is_empty() && (empty || shut) {
                break;
            }
            assert!(Instant::now() < deadline, "the node's sends did not arrive");
            std::thread::sleep(Duration::from_millis(2));
        }
        std::mem::take(&mut peer.end.lock().inbox)
    }
}

/// What a scripted peer says in its `version`.
#[derive(Clone, Copy, Debug)]
pub struct PeerVersionMsg {
    /// `nVersion`.
    pub version: i32,
    /// `nServices` (raw bits).
    pub services: u64,
    /// `nTime`.
    pub time: i64,
    /// `nNonce` (must differ from the node's own, or the node sees itself).
    pub nonce: u64,
    /// `nStartingHeight`.
    pub start_height: i32,
    /// `fRelay`.
    pub relay: bool,
    /// `addr_recv`, what the peer says our address is (Core's `addrMe`): the 16-byte IP
    /// (IPv4-mapped for IPv4) and the port, big-endian. All zero (`::`, port 0) by default.
    pub addr_recv: [u8; 18],
}

impl PeerVersionMsg {
    /// A v24 full node (`NODE_NETWORK | NODE_BLOOM | NODE_NETWORK_LIMITED | NODE_HEADERS_COMPRESSED`)
    /// at `time` with its tip at genesis, relaying transactions.
    pub fn v24(time: i64) -> Self {
        Self {
            version: 70242,
            services: 0x1 | 0x4 | 0x400 | 0x800,
            time,
            nonce: 0x5eed_5eed_5eed_5eed,
            start_height: 0,
            relay: true,
            addr_recv: [0; 18],
        }
    }

    /// The payload in the layout of Core's `PushNodeVersion`: the two pre-31402 addresses
    /// (services, then [`PeerVersionMsg::addr_recv`] and `::` port 0), the user agent
    /// "/sim:0.1/", then Dash's MNAUTH challenge (zero) and masternode flag (false).
    // Core: net_processing.cpp:1583-1586
    pub fn payload(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&self.version.to_le_bytes());
        out.extend_from_slice(&self.services.to_le_bytes());
        out.extend_from_slice(&self.time.to_le_bytes());
        for addr in [self.addr_recv, [0; 18]] {
            out.extend_from_slice(&self.services.to_le_bytes());
            out.extend_from_slice(&addr);
        }
        out.extend_from_slice(&self.nonce.to_le_bytes());
        let ua = b"/sim:0.1/";
        out.push(ua.len() as u8);
        out.extend_from_slice(ua);
        out.extend_from_slice(&self.start_height.to_le_bytes());
        out.push(u8::from(self.relay));
        out.extend_from_slice(&[0; 32]);
        out.push(0);
        out
    }
}

/// A V1 message header's length: magic, command, length and checksum.
pub const V1_HEADER: usize = 24;

/// A V1 frame on `params`' network.
pub fn v1_frame(params: &ChainSetup, cmd: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = params.message_start.to_vec();
    out.extend_from_slice(CommandString::from_static(cmd).as_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&dash_p2p_core::checksum(payload));
    out.extend_from_slice(payload);
    out
}

/// Whether the node has shut its end of `sock` (the peer's end then reads end-of-stream), with
/// any bytes that arrived meanwhile appended to `bytes`.
fn node_end_shut(sock: &MemSocket, bytes: &mut Vec<u8>) -> bool {
    let mut buf = [0; 4096];
    loop {
        match sock.try_read(&mut buf) {
            Ok(0) => return true,
            Ok(n) => bytes.extend(&buf[..n]),
            Err(_) => return false,
        }
    }
}

/// Polls `cond` every millisecond for up to [`PATIENCE`].
///
/// # Panics
///
/// When the condition is not reached.
pub fn until(mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !cond() {
        assert!(Instant::now() < deadline, "sim condition not reached in {PATIENCE:?}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(test)]
mod tests {
    //! Negative controls for the scripted peer's reader (O13a fix round 1): every V1 framing
    //! fault Core's peer refuses or drops (net.cpp:731-739 the magic and the size, 796-807 the
    //! checksum and the command) fails the transcript, and so does an incomplete trailing frame.

    use dash_params::{ChainParamsBuilder, Network, Release};

    use super::*;

    fn params(net: Network) -> ChainSetup {
        ChainParamsBuilder::new(net, Release::V24_0).build().expect("params")
    }

    fn regtest_peer() -> PeerEnd {
        let magic = Magic::from_bytes(params(Network::Regtest).message_start);
        PeerEnd::new(Transport::v1(magic)).into_inner()
    }

    fn ping() -> Vec<u8> {
        v1_frame(&params(Network::Regtest), "ping", &[7; 8])
    }

    #[test]
    fn whole_frames_decode_with_nothing_left() {
        let mut peer = regtest_peer();
        let two = [ping(), ping()].concat();
        let (a, b) = two.split_at(36);
        peer.take(a);
        assert_eq!((peer.inbox.len(), peer.partial), (1, 4));
        peer.take(b);
        assert_eq!((peer.inbox.len(), peer.partial), (2, 0));
        assert_eq!(peer.inbox[1].cmd.as_str(), "ping");
        assert_eq!(peer.inbox[1].form, None);
    }

    #[test]
    fn an_incomplete_trailing_frame_is_counted() {
        let mut peer = regtest_peer();
        let f = ping();
        peer.take(&f[..f.len() - 1]);
        assert!(peer.inbox.is_empty());
        assert_eq!(peer.partial, f.len() as u64 - 1);
    }

    #[test]
    #[should_panic(expected = "bytes its peer refuses")]
    fn a_wrong_magic_fails() {
        let mut f = ping();
        f[0] ^= 1;
        regtest_peer().take(&f);
    }

    #[test]
    #[should_panic(expected = "bytes its peer refuses")]
    fn another_networks_magic_fails() {
        regtest_peer().take(&v1_frame(&params(Network::Testnet3), "ping", &[7; 8]));
    }

    #[test]
    #[should_panic(expected = "bytes its peer refuses")]
    fn a_length_over_the_cap_fails() {
        let mut f = ping();
        f[16..20].copy_from_slice(&(3 * 1024 * 1024 + 1u32).to_le_bytes());
        regtest_peer().take(&f[..24]);
    }

    #[test]
    #[should_panic(expected = "a message its peer drops")]
    fn a_bad_checksum_fails() {
        let mut f = ping();
        f[20] ^= 1;
        regtest_peer().take(&f);
    }

    #[test]
    #[should_panic(expected = "a message its peer drops")]
    fn an_unprintable_command_fails() {
        let mut f = ping();
        f[5] = 0x01;
        regtest_peer().take(&f);
    }

    #[test]
    #[should_panic(expected = "a message its peer drops")]
    fn bytes_after_the_command_padding_fail() {
        let mut f = ping();
        f[4 + 6] = b'x';
        regtest_peer().take(&f);
    }
}
