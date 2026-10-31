//! The dial `sim` (P2a §6.7): SB6's dial loops under P2a-core's virtual clock, with in-memory
//! connections in place of TCP.
//!
//! [`SimExec`] performs [`DialPlan`](dash_p2p::DialPlan)s as the threads' executor does, except
//! that a sleep moves the virtual clock, an open attaches an in-memory socket after Core's
//! `OpenNetworkConnection` checks and `addrman.Attempt`, a DNS seed resolves from a table, and
//! waiting for a slot that no connection will free stops the run instead of blocking. No
//! processor runs: peers never handshake, which is all the ladder counts need. A feeler can be
//! closed right after it opens, as the processor does after its `version`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dash_clock::Clock;
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, ConnectionType, DialLoop, DialPolicy, MemSocket, NetAddr, NetConfig, NetCore,
    NetEvents, OpenError, OpenRequest, PeerHandle, PeerId, Permissions, Resolver, Service,
    ServiceFlags, Target,
};
use dash_params::ChainSetup;
use dashd_p2p::fingerprint::{IdentityOptions, local_services};
use dashd_p2p::sim::VirtualClock;
use parking_lot::Mutex;

use crate::driver::{self, DialExec};

/// How long [`SimExec::close`] waits for the core to finalize a connection.
pub const CLOSE_BUDGET: Duration = Duration::from_secs(60);

/// dash-p2p's linger for a peer we disconnected (`netcore.rs` `LINGER`): steady time that
/// [`SimExec::close`] lets pass, since the virtual clock stands still while it waits.
const LINGER: Duration = Duration::from_millis(100);

/// The mock time the dial `sim` starts at.
pub const SIM_START: i64 = 1_700_000_000;

/// No processor: nothing to initialize or wake; finalizing only records the peer, whose slot
/// is free by then.
#[derive(Default)]
struct NoProcessor {
    finalized: Mutex<HashSet<PeerId>>,
}

impl NetEvents for NoProcessor {
    fn wake(&self) {}
    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}
    fn finalize(&self, _core: &NetCore, peer: &PeerHandle) {
        self.finalized.lock().insert(peer.id());
    }
}

/// The processor's flags (P2a-core's `OutboundFlags` in the node), set by hand.
#[derive(Debug, Default)]
pub struct SimPolicy {
    /// `m_try_another_outbound_peer`.
    pub try_another: AtomicBool,
    /// `m_start_extra_block_relay_peers`.
    pub extra_block_relay: AtomicBool,
}

impl DialPolicy for SimPolicy {
    fn try_another_outbound_peer(&self) -> bool {
        self.try_another.load(Ordering::SeqCst)
    }

    fn start_extra_block_relay_peers(&self) -> bool {
        self.extra_block_relay.load(Ordering::SeqCst)
    }
}

/// DNS answers from a table (`x1.<seed>` → addresses); a table name answers only when the call
/// may look up (`AI_NUMERICHOST` decodes no name).
#[derive(Default)]
pub struct TableResolver(pub Mutex<HashMap<String, Vec<NetAddr>>>);

impl Resolver for TableResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        max: usize,
        allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        let mut found = if allow_lookup {
            self.0.lock().get(host).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        found.truncate(max);
        Box::pin(async move { found })
    }
}

/// One thing a dial loop did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialEvent {
    /// A connection was attached.
    Opened {
        /// The connection.
        id: PeerId,
        /// Where to.
        service: Service,
        /// Its type.
        conn_type: ConnectionType,
    },
    /// `OpenNetworkConnection` returned without a connection.
    Refused {
        /// The target.
        target: Target,
        /// Its type.
        conn_type: ConnectionType,
        /// Why.
        err: OpenError,
    },
    /// The loop slept (the virtual clock moved).
    Slept(Duration),
    /// The loop waits for a slot that no connection will free.
    Blocked,
    /// A DNS seed was resolved.
    Resolved {
        /// The seed.
        seed: String,
        /// How many addresses it gave.
        found: usize,
    },
}

/// The dial `sim`'s executor.
pub struct SimExec {
    clock: Arc<VirtualClock>,
    rt: tokio::runtime::Handle,
    /// The far ends of the open connections.
    remotes: BTreeMap<PeerId, MemSocket>,
    /// Services whose connect fails.
    pub unreachable: HashSet<Service>,
    /// Close each feeler right after it opens.
    pub close_feelers: bool,
    /// What the loops did, in order.
    pub events: Vec<DialEvent>,
    blocked: bool,
    processor: Arc<NoProcessor>,
}

impl SimExec {
    /// Core's `OpenNetworkConnection` + `ConnectNode` over an in-memory socket, a name cut at
    /// its first NUL as [`NetCore::open`] cuts it.
    // Core: net.cpp:3586-3665 CConnman::OpenNetworkConnection; net.cpp:370-520 ConnectNode
    fn connect(&mut self, core: &NetCore, mut req: OpenRequest) -> Result<PeerHandle, OpenError> {
        req.cut_dest_at_nul();
        if core.interrupted() || !core.network_active() {
            return Err(OpenError::Interrupted);
        }
        let now = core.clock().node_now().0;
        let addr = match &req.target {
            Target::Addr(a) => a.clone(),
            Target::Dest(dest) => {
                if core.peers().iter().any(|p| !p.disconnect_requested() && p.addr_name() == dest) {
                    return Err(OpenError::AlreadyConnected);
                }
                let ip = dest.parse().map_err(|_| OpenError::Resolve(dest.clone()))?;
                Address {
                    service: Service::from_socket_addr(ip),
                    services: ServiceFlags::NONE,
                    time: 0,
                }
            }
        };
        // Core: the `!pszDest` checks of `OpenNetworkConnection` (net.cpp:3611-3621), and
        // `ConnectNode`'s resolved-service check for a name (net.cpp:415-417).
        if let Target::Addr(_) = req.target {
            let ip = &addr.service.addr;
            if addr.service.port == core.config().listen_port && core.is_local(ip) {
                return Err(OpenError::Local);
            }
            if core.banlist().is_discouraged(ip) || core.banlist().is_banned(ip, now) {
                return Err(OpenError::Banned);
            }
        }
        if core
            .peers()
            .iter()
            .any(|p| !p.disconnect_requested() && p.addr().service == addr.service)
        {
            return Err(OpenError::AlreadyConnected);
        }
        core.addrman().attempt(&addr.service, req.count_failure, now);
        if self.unreachable.contains(&addr.service) {
            return Err(OpenError::Connect("unreachable in sim".into()));
        }
        let (ours, theirs) = MemSocket::pair();
        let dest = match req.target {
            Target::Dest(d) => Some(d),
            Target::Addr(_) => None,
        };
        let peer = core.attach(Attach {
            sock: Arc::new(ours),
            addr,
            addr_bind: Service::default(),
            dest,
            conn_type: req.conn_type,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: req.use_v2,
            grant: req.grant,
            masternode: req.masternode,
            masternode_probe: req.masternode_probe,
        });
        self.remotes.insert(peer.id(), theirs);
        Ok(peer)
    }

    /// Closes connection `id` from the far side and waits until the core has finalized it (its
    /// slot is free again), then leaves the clocks [`LINGER`] later.
    ///
    /// A send can fail on the closed socket before the reader sees the close: the core then
    /// marks the peer without `fOtherSideDisconnected` and lingers on its steady clock, as Core
    /// does after a send error. Core's linger passes in real time; here the clock moves to the
    /// linger's end. It moves by [`LINGER`] either way, so a script's clocks do not depend on
    /// which task saw the close first (P2a-net D33).
    ///
    /// # Panics
    ///
    /// When the core keeps it for [`CLOSE_BUDGET`].
    pub fn close(&mut self, core: &NetCore, id: PeerId) {
        let peer = core.peers().iter().find(|p| p.id() == id).cloned();
        let marked = peer.as_ref().is_some_and(|p| p.disconnect_requested());
        if let Some(s) = self.remotes.remove(&id) {
            s.close();
        }
        let end = self.clock.steady() + LINGER;
        // Generous: the fuzz farm runs this at a load of 100–200, and a slow finalize there must
        // not pass for a finding.
        let deadline = std::time::Instant::now() + CLOSE_BUDGET;
        while !self.processor.finalized.lock().contains(&id) {
            assert!(
                std::time::Instant::now() < deadline,
                "sim harness: peer={id} not finalized within {CLOSE_BUDGET:?} (box load?)"
            );
            if let Some(p) = &peer
                && let Some(until) = p.linger_until()
            {
                // Core lingers only after our own mark or a send that failed on their close,
                // whose bytes stay queued; their clean close never lingers (net.cpp:2619-2627).
                assert!(
                    marked || !p.send_queue_empty(),
                    "sim harness: peer={id} lingers after a far-side close with nothing to send"
                );
                assert!(until <= end, "sim harness: peer={id} lingers past {LINGER:?}");
                self.clock.advance(until.saturating_duration_since(self.clock.steady()));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        self.clock.advance(end.saturating_duration_since(self.clock.steady()));
    }
}

impl DialExec for SimExec {
    fn open(&mut self, core: &NetCore, req: OpenRequest) {
        let (target, conn_type) = (req.target.clone(), req.conn_type);
        match self.connect(core, req) {
            Ok(peer) => {
                self.events.push(DialEvent::Opened {
                    id: peer.id(),
                    service: peer.addr().service.clone(),
                    conn_type,
                });
                if conn_type == ConnectionType::Feeler && self.close_feelers {
                    self.close(core, peer.id());
                }
            }
            Err(err) => self.events.push(DialEvent::Refused {
                target,
                conn_type,
                err,
            }),
        }
    }

    fn wait(&mut self, core: &NetCore, lp: DialLoop, max: Option<Duration>) -> bool {
        match max {
            Some(d) => {
                self.clock.advance(d);
                self.events.push(DialEvent::Slept(d));
            }
            // A slot is free only if a closed connection released it: try without blocking.
            None => {
                let got = self.rt.block_on(async {
                    tokio::time::timeout(Duration::from_millis(20), core.dial_wait(lp, None)).await
                });
                if got.is_err() {
                    self.blocked = true;
                    self.events.push(DialEvent::Blocked);
                }
            }
        }
        !core.interrupted()
    }

    fn resolve(&mut self, core: &NetCore, seed: &str, max: usize) {
        let found = self.rt.block_on(core.resolve_seed(seed, max));
        self.events.push(DialEvent::Resolved {
            seed: seed.to_owned(),
            found,
        });
    }
}

/// A network core with no processor, its dial loops driven on the test's thread.
pub struct DialSim {
    /// The network core.
    pub core: Arc<NetCore>,
    /// The virtual clock.
    pub clock: Arc<VirtualClock>,
    /// The executor and its event log.
    pub exec: SimExec,
    /// DNS answers.
    pub resolver: Arc<TableResolver>,
    /// The runtime the core's socket tasks run on; declared last, so it is dropped after the
    /// core.
    rt: tokio::runtime::Runtime,
}

impl DialSim {
    /// A core on `params` in deterministic mode with inactivity checks off; `cfg_hook` edits the
    /// config first.
    ///
    /// # Panics
    ///
    /// When the core cannot be built.
    pub fn new(params: Arc<ChainSetup>, cfg_hook: impl FnOnce(&mut NetConfig)) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("sim runtime");
        let clock = VirtualClock::at(SIM_START);
        let mut cfg = NetConfig::new(params, local_services(&IdentityOptions::new()));
        cfg.deterministic = true;
        // Core's functional tests' `-peertimeout=999999999`: peers here never speak.
        cfg.peer_timeout = 999_999_999;
        cfg_hook(&mut cfg);
        let resolver = Arc::new(TableResolver::default());
        let processor = Arc::new(NoProcessor::default());
        let core = NetCore::new(
            cfg,
            rt.handle().clone(),
            Arc::new(NoStore),
            resolver.clone(),
            clock.clone(),
            processor.clone(),
        )
        .expect("sim network core");
        // No binds to fail: Core's `Start` reaches `ReadAnchors` at once.
        core.start_addresses();
        let exec = SimExec {
            clock: clock.clone(),
            rt: rt.handle().clone(),
            remotes: BTreeMap::new(),
            unreachable: HashSet::new(),
            close_feelers: true,
            events: Vec::new(),
            blocked: false,
            processor,
        };
        Self {
            core,
            clock,
            exec,
            resolver,
            rt,
        }
    }

    /// The runtime's handle.
    pub fn handle(&self) -> &tokio::runtime::Handle {
        self.rt.handle()
    }

    /// Steps `lp` until it finishes, blocks on a slot, or `max_steps` plans have run. Returns
    /// whether it is blocked.
    pub fn run(&mut self, lp: DialLoop, policy: &dyn DialPolicy, max_steps: usize) -> bool {
        let mut n = 0;
        self.drive(lp, policy, |_| {
            n += 1;
            n <= max_steps
        })
    }

    /// Steps `lp` until the virtual clock has moved by `d`, the loop finishes or it blocks.
    /// Returns whether it is blocked.
    pub fn run_for(&mut self, lp: DialLoop, policy: &dyn DialPolicy, d: Duration) -> bool {
        let end = self.clock.node_now_micros() + d.as_micros() as i64;
        self.drive(lp, policy, |clock| clock.node_now_micros() < end)
    }

    /// Steps `lp` while `more` holds, until it finishes or blocks. Returns whether it is blocked.
    fn drive(
        &mut self,
        lp: DialLoop,
        policy: &dyn DialPolicy,
        mut more: impl FnMut(&VirtualClock) -> bool,
    ) -> bool {
        self.exec.blocked = false;
        while more(&self.clock)
            && driver::step(&self.core, lp, policy, &mut self.exec)
            && !self.exec.blocked
        {}
        self.exec.blocked
    }

    /// Closes connection `id` from the far side (see [`SimExec::close`]).
    pub fn close(&mut self, id: PeerId) {
        self.exec.close(&self.core, id);
    }

    /// The open connections of `conn_type`.
    pub fn of_type(&self, conn_type: ConnectionType) -> Vec<PeerHandle> {
        self.core.peers().iter().filter(|p| p.conn_type() == conn_type).cloned().collect()
    }

    /// Adds `addrs` to AddrMan from `source` with no penalty.
    pub fn add_addrs(&self, addrs: &[Address], source: &NetAddr) {
        let now = self.clock.node_now().0;
        self.core.addrman().add(addrs, source, 0, now);
    }
}

/// A `NODE_NETWORK` address on the default port, dated `time`.
pub fn full_node(service: Service, time: i64) -> Address {
    Address {
        service,
        services: ServiceFlags::NODE_NETWORK,
        time: time as u32,
    }
}

#[cfg(test)]
mod tests {
    use dash_p2p::conn::Why;
    use dash_p2p::{CommandString, OutMsg};
    use dash_params::{ChainParamsBuilder, Network, Release};

    use super::*;

    /// A peer we marked lingers until its deadline when their close comes before its queue
    /// flushes (Core's `only_poll` spin, net.cpp:2121-2145, 2376-2415): `close` moves the clocks
    /// to the linger's end, exactly [`LINGER`] on, and the peer is finalized (P2a-net D33, Opus
    /// wire r1 O2). It fails if `close` lets no time pass, if a linger never expires, or if it
    /// runs longer than [`LINGER`].
    #[test]
    fn a_lingering_peer_closes_at_its_deadline() {
        let params =
            ChainParamsBuilder::new(Network::Main, Release::V24_0).build().expect("params");
        let mut sim = DialSim::new(Arc::new(params), |cfg| cfg.dns_seed = false);
        let (ours, theirs) = MemSocket::pair();
        ours.set_send_budget(Some(0));
        let peer = sim.core.attach(Attach {
            sock: Arc::new(ours),
            addr: Address {
                service: Service::new(NetAddr::ipv4([10, 1, 1, 1]), 9999),
                services: ServiceFlags::NODE_NETWORK,
                time: 0,
            },
            addr_bind: Service::default(),
            dest: None,
            conn_type: ConnectionType::Manual,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: false,
            grant: None,
            masternode: false,
            masternode_probe: false,
        });
        sim.exec.remotes.insert(peer.id(), theirs);
        sim.core.push(&peer, OutMsg::new(CommandString::from_static("ping"), vec![0; 8]));
        sim.core.disconnect(peer.id(), Why::Processor("test"));
        let deadline = std::time::Instant::now() + CLOSE_BUDGET;
        while peer.linger_until().is_none() {
            assert!(std::time::Instant::now() < deadline, "the sweep never lingered");
            std::thread::sleep(Duration::from_millis(1));
        }
        let (steady, node) = (sim.clock.steady(), sim.clock.node_now_micros());
        assert_eq!(peer.linger_until(), Some(steady + LINGER));

        sim.close(peer.id());
        assert!(sim.core.peers().is_empty());
        assert_eq!(sim.clock.steady(), steady + LINGER);
        assert_eq!(sim.clock.node_now_micros(), node + LINGER.as_micros() as i64);
    }
}
