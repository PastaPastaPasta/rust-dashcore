//! The dial loops as sans-io step functions (P2a §3.8; v24 `net.cpp` `ThreadOpenConnections`
//! `:2917-3292`, `ThreadOpenAddedConnections` `:3368-3395`, `ThreadDNSAddressSeed`
//! `:2680-2794`).
//!
//! Each loop is a state machine inside [`NetCore`](crate::NetCore). A driver calls
//! [`NetCore::dial_tick`](crate::NetCore::dial_tick) and performs the [`DialPlan`] it gets: the
//! node from Core's `opencon`, `addcon` and `dnsseed` OS threads (`dashd-p2p`), SPV from a tokio
//! interval. The processor's decisions come in through [`DialPolicy`]. Every point where Core's
//! thread blocks (a sleep, a grant, a connect, a DNS lookup) is a [`DialPlan`], so the driver
//! owns all waiting.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use dash_p2p_core::ServiceFlags;
use parking_lot::MutexGuard;
use tokio::sync::Semaphore;

use crate::conn::ConnectionType;
use crate::netaddr::{Address, NetAddr, Network, Service};
use crate::netcore::{NetCore, OpenRequest, Target, split_host_port};
use crate::rng::{FastRandom, exponential_rand};
use crate::seeds::{Lookup, default_port_for, lookup, pre_lookup};

/// Which of Core's loops a tick advances.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DialLoop {
    /// `ThreadOpenConnections`.
    OpenCon,
    /// `ThreadOpenAddedConnections`.
    AddCon,
    /// `ThreadDNSAddressSeed`.
    DnsSeed,
}

/// A connection slot (Core's `CSemaphoreGrant`), returned when its connection closes.
#[derive(Debug)]
pub struct Grant {
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl Grant {
    fn try_acquire(sem: &Arc<Semaphore>) -> Option<Self> {
        sem.clone().try_acquire_owned().ok().map(|permit| Self {
            _permit: permit,
        })
    }
}

/// What the driver does next.
#[derive(Debug)]
pub enum DialPlan {
    /// Open this connection ([`NetCore::open`](crate::NetCore::open), Core's
    /// `OpenNetworkConnection`), then tick again whatever the outcome.
    Open(OpenRequest),
    /// Sleep (Core's `interruptNet.sleep_for`, real time) with
    /// [`NetCore::dial_wait`](crate::NetCore::dial_wait), then tick again.
    Sleep(Duration),
    /// Block on this loop's slot with [`NetCore::dial_wait`](crate::NetCore::dial_wait) and
    /// `None`, then tick again.
    WaitGrant,
    /// Call [`NetCore::resolve_seed`](crate::NetCore::resolve_seed) with these, then tick again.
    Resolve {
        /// The DNS seed (`resolve_seed` queries `x1.<seed>`).
        seed: String,
        /// At most this many answers (Core's `LookupHost(host, 32, true)`).
        max: usize,
    },
    /// The loop is finished (interrupted, the DNS loop after seeding, or a loop Core does not
    /// start: `-dnsseed=0`, `-connect=0`).
    Done,
}

/// The processor's inputs to the connection-type ladder and address choice (P2a §3.8.2 steps 7–9).
pub trait DialPolicy: Send + Sync {
    /// `m_try_another_outbound_peer`: the stale-tip flag.
    fn try_another_outbound_peer(&self) -> bool {
        false
    }

    /// `m_start_extra_block_relay_peers`.
    fn start_extra_block_relay_peers(&self) -> bool {
        false
    }

    /// Dash step 9(m): the address is a masternode in the tip list whose ProRegTx a connected
    /// peer has verified.
    fn is_connected_masternode(&self, addr: &Address) -> bool {
        let _ = addr;
        false
    }
}

/// Core's `FEELER_INTERVAL`: 2 minutes.
const FEELER_INTERVAL_SECS: i64 = 2 * 60;
/// Core's `EXTRA_BLOCK_RELAY_ONLY_PEER_INTERVAL`: 5 minutes.
const EXTRA_BLOCK_RELAY_ONLY_PEER_INTERVAL_SECS: i64 = 5 * 60;
/// Core's `EXTRA_NETWORK_PEER_INTERVAL`: 5 minutes.
const EXTRA_NETWORK_PEER_INTERVAL_SECS: i64 = 5 * 60;
/// Core's `FEELER_SLEEP_WINDOW`: 1 second, in nanoseconds (`steady_clock` ticks).
const FEELER_SLEEP_WINDOW_NS: i64 = 1_000_000_000;
/// Core's `MAX_OUTBOUND_FULL_RELAY_CONNECTIONS`.
const MAX_OUTBOUND_FULL_RELAY_CONNECTIONS: usize = 8;
/// Core's `DNSSEEDS_TO_QUERY_AT_ONCE`.
const DNSSEEDS_TO_QUERY_AT_ONCE: usize = 3;
/// Core's `DNSSEEDS_DELAY_FEW_PEERS`: 11 seconds.
const DNSSEEDS_DELAY_FEW_PEERS: u64 = 11;
/// Core's `DNSSEEDS_DELAY_MANY_PEERS`: 5 minutes.
const DNSSEEDS_DELAY_MANY_PEERS: u64 = 5 * 60;
/// Core's `DNSSEEDS_DELAY_PEER_THRESHOLD`.
const DNSSEEDS_DELAY_PEER_THRESHOLD: usize = 1000;
/// Core's 500 ms loop sleeps.
const HALF_SECOND: Duration = Duration::from_millis(500);

/// An `-addnode` entry (Core's `AddedNodeParams`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedNode {
    /// `m_added_node`: `ip:port` or a name.
    pub node: String,
    /// `m_use_v2transport`.
    pub use_v2: bool,
}

/// Core's `AddedNodeInfo`: an added-node entry and the connection it has, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedNodeInfo {
    /// `m_params`.
    pub node: AddedNode,
    /// `fConnected`; then `resolvedAddress` and `fInbound`.
    pub connection: Option<AddedConnection>,
}

/// A connected added node's `resolvedAddress` and `fInbound`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedConnection {
    /// The entry's numeric `ip:port`, or the address of the peer a name matched.
    pub service: Service,
    pub inbound: bool,
}

/// The dial loops' state and the queues they read.
#[derive(Debug, Default)]
pub(crate) struct DialState {
    open: OpenCon,
    add: AddCon,
    dns: DnsSeed,
    /// `m_addr_fetches`.
    pub(crate) addr_fetches: VecDeque<String>,
    /// `m_added_node_params`.
    pub(crate) added: Vec<AddedNode>,
    /// `m_anchors`.
    pub(crate) anchors: Vec<Address>,
}

#[derive(Debug, Default)]
enum OcPhase {
    #[default]
    Start,
    /// `-connect` mode: dialling entry `idx` of pass `pass`; `opened` once its open was planned.
    Connect {
        pass: i64,
        idx: usize,
        opened: bool,
    },
    /// `-connect` mode: `PerformReconnections` after a pass.
    ConnectReconnect {
        pass: i64,
    },
    AddrFetch,
    Sleep,
    Reconnect,
    Grant,
    /// The feeler's random sleep is planned; open it next.
    Feeler(Box<OpenRequest>),
}

#[derive(Debug, Default)]
struct OpenCon {
    phase: OcPhase,
    start_us: i64,
    next_feeler: i64,
    next_extra_block_relay: i64,
    next_extra_network_peer: i64,
    add_fixed_seeds: bool,
    /// A grant [`NetCore::dial_wait`] acquired for the next tick.
    pending: Option<Grant>,
    /// `ThreadOpenConnections`' `FastRandomContext rng`, which only the feeler's sleep draws.
    // Core: net.cpp:2921, :3271
    rng: Option<FastRandom>,
}

#[derive(Debug, Default)]
enum AcPhase {
    #[default]
    Grant,
    Iterate,
    Sleep,
    Regrant,
    Reconnect,
}

#[derive(Debug, Default)]
struct AddCon {
    phase: AcPhase,
    infos: VecDeque<AddedNode>,
    tried: bool,
    /// The loop's grant, held to the end of the iteration as Core's local is.
    grant: Option<Grant>,
    pending: Option<Grant>,
}

#[derive(Debug, Default)]
enum DnsPhase {
    #[default]
    Start,
    Seed,
    /// Sleeping a step of the wait; check the peers next.
    Waiting,
    /// Waiting for the network to be reactivated.
    Inactive,
    Resolved,
    /// Seeding is over: Core's thread has returned, so every later tick is `Done`.
    // Core: net.cpp:2734-2741, :2793
    Finished,
}

#[derive(Debug, Default)]
struct DnsSeed {
    phase: DnsPhase,
    seeds: Vec<String>,
    idx: usize,
    right_now: usize,
    wait: u64,
    to_wait: u64,
    found: usize,
    /// `ThreadDNSAddressSeed`'s `FastRandomContext rng`: the seed shuffle, then each address's
    /// time.
    // Core: net.cpp:2682-2684, :2779
    rng: Option<FastRandom>,
}

/// What the ladder and the address selection chose.
enum Pick {
    Open(OpenRequest),
    Feeler(OpenRequest, Duration),
}

pub use dash_netaddr::is_bad_port;

/// Core's `AddedNodesContain`: an automatic outbound pick that is also an `-addnode` entry
/// (checked only while fewer than 24 are configured).
// Core: net.cpp:4335-4344 CConnman::AddedNodesContain
fn added_nodes_contain(added: &[AddedNode], addr: &Address) -> bool {
    let (a, ap) = (addr.service.addr.to_string(), addr.service.to_string());
    added.len() < 24 && added.iter().any(|p| p.node == a || p.node == ap)
}

/// Core's `MayHaveUsefulAddressDB`.
fn may_have_useful_address_db(services: ServiceFlags) -> bool {
    services.0 & (ServiceFlags::NODE_NETWORK.0 | ServiceFlags::NODE_NETWORK_LIMITED.0) != 0
}

impl NetCore {
    /// One step of a dial loop (P2a §3.8; sans-io): the action its driver must perform next.
    /// The node runs each loop on its own OS thread (`dashd-p2p`), SPV on a tokio task.
    pub fn dial_tick(&self, lp: DialLoop, policy: &dyn DialPolicy) -> DialPlan {
        match lp {
            DialLoop::OpenCon => self.tick_opencon(policy),
            DialLoop::AddCon => self.tick_addcon(),
            DialLoop::DnsSeed => self.tick_dnsseed(),
        }
    }

    /// Waits for a [`DialPlan::Sleep`] (`Some(d)`) or blocks on `lp`'s slot for a
    /// [`DialPlan::WaitGrant`] (`None`; the grant is kept for the next tick). Returns `false`
    /// on interrupt (Core's `interruptNet.sleep_for` and `CSemaphoreGrant`). Only
    /// [`NetCore::interrupt`] ends a sleep early, as only `interruptNet()` ends Core's: no peer or
    /// network change shortens it, so a loop's sleeps (and the DNS loop's wait) keep Core's
    /// lengths.
    // Core: threadinterrupt.cpp:33-37 CThreadInterrupt::sleep_for; net.cpp:4153-4165 Interrupt
    pub async fn dial_wait(&self, lp: DialLoop, max: Option<Duration>) -> bool {
        let interrupted = self.interrupt_wake.notified();
        tokio::pin!(interrupted);
        interrupted.as_mut().enable();
        if self.interrupted() {
            return false;
        }
        match max {
            Some(d) => {
                tokio::select! {
                    () = tokio::time::sleep(d) => {}
                    () = &mut interrupted => {}
                }
            }
            None => {
                let sem = self.slots(lp).clone();
                tokio::select! {
                    permit = sem.acquire_owned() => {
                        let grant = Grant {
                            _permit: permit.expect("the slot semaphores are never closed"),
                        };
                        if let Some(mut dial) = self.live_dial() {
                            match lp {
                                DialLoop::AddCon => dial.add.pending = Some(grant),
                                _ => dial.open.pending = Some(grant),
                            }
                        }
                    }
                    () = &mut interrupted => {}
                }
            }
        }
        !self.interrupted()
    }

    /// Core's `CSemaphoreGrant(sem, /*fTry=*/true)` on `lp`'s slots: `semAddnode` for
    /// [`DialLoop::AddCon`], `semOutbound` otherwise. `None` when every slot is held. The
    /// `addconnection` RPC takes its outbound slot this way before opening the connection with
    /// the grant.
    // Core: net.cpp:2030-2063 CConnman::AddConnection
    pub fn try_grant(&self, lp: DialLoop) -> Option<Grant> {
        Grant::try_acquire(self.slots(lp))
    }

    /// The dial state, or `None` once interrupted. Checked under the lock, so no tick or wait
    /// stores a grant after [`NetCore::release_dial_grants`] has run.
    fn live_dial(&self) -> Option<MutexGuard<'_, DialState>> {
        let dial = self.dial.lock();
        (!self.interrupted()).then_some(dial)
    }

    /// `StopNodes`' `m_reconnections.clear()` and `semOutbound`/`semAddnode` reset: drops every
    /// grant the dial loops still hold (a loop's grant across its sleep, a grant
    /// [`NetCore::dial_wait`] kept, a planned feeler's) and the queued V1 retries, so both slot
    /// semaphores are full again. Runs after the interrupt and the sweep's join (the only task
    /// that queues retries), under the dial lock, so no tick or wait stores or takes another.
    // Core: net.cpp:4230-4232 CConnman::StopNodes
    pub(crate) fn release_dial_grants(&self) {
        let mut dial = self.dial.lock();
        dial.open.pending = None;
        if matches!(dial.open.phase, OcPhase::Feeler(_)) {
            dial.open.phase = OcPhase::AddrFetch;
        }
        dial.add.grant = None;
        dial.add.pending = None;
        self.reconnections.lock().clear();
    }

    fn slots(&self, lp: DialLoop) -> &Arc<Semaphore> {
        match lp {
            DialLoop::AddCon => &self.addnode_slots,
            _ => &self.outbound_slots,
        }
    }

    /// Core's `AddAddrFetch`: a destination for a one-shot ADDR_FETCH connection.
    pub fn add_addr_fetch(&self, dest: String) {
        self.dial.lock().addr_fetches.push_back(dest);
    }

    /// Core's `AddNode`: false when the name, or the numeric address it parses to, is listed.
    // Core: net.cpp:4309-4321 CConnman::AddNode
    pub fn add_node(&self, add: AddedNode) -> bool {
        let resolved = self.lookup_numeric(&add.node);
        let mut dial = self.dial.lock();
        let dup = dial.added.iter().any(|it| {
            it.node == add.node
                || (resolved.addr.is_valid() && self.lookup_numeric(&it.node) == resolved)
        });
        if dup {
            return false;
        }
        dial.added.push(add);
        true
    }

    /// Core's `RemoveAddedNode`.
    pub fn remove_added_node(&self, node: &str) -> bool {
        let mut dial = self.dial.lock();
        let before = dial.added.len();
        if let Some(i) = dial.added.iter().position(|a| a.node == node) {
            dial.added.remove(i);
        }
        dial.added.len() != before
    }

    /// The `-addnode` and `addnode` RPC entries.
    pub fn added_nodes(&self) -> Vec<AddedNode> {
        self.dial.lock().added.clone()
    }

    /// Core's `LookupNumeric(name, GetDefaultPort(name))`: an invalid service unless `name` is a
    /// numeric address or a Tor v3 or I2P name, with Core's CJDNS flip. Never resolves.
    // Core: netbase.cpp:219-227 LookupNumeric; net.cpp:3337, :4311
    pub(crate) fn lookup_numeric(&self, name: &str) -> Service {
        let (host, port) =
            split_host_port(name, default_port_for(name, self.config().params.default_port));
        match pre_lookup(host) {
            Lookup::Addr(addr) => self.maybe_flip_ipv6_to_cjdns(Service::new(addr, port)),
            Lookup::Refused | Lookup::Name => Service::default(),
        }
    }

    /// Core's `GetAddedNodeInfo`: every entry in the list's order with its connection, less the
    /// connected ones unless `include_connected`. A numeric entry matches a peer by `ip:port`,
    /// a name by the peer's non-empty `m_addr_name`; peers being disconnected count. Core keys
    /// its maps by those, so of several matching peers the last in the table gives the
    /// connection.
    // Core: net.cpp:3308-3366 CConnman::GetAddedNodeInfo
    pub fn added_node_info(&self, include_connected: bool) -> Vec<AddedNodeInfo> {
        let added = self.dial.lock().added.clone();
        let peers = self.peers();
        added
            .into_iter()
            .filter_map(|entry| {
                let service = self.lookup_numeric(&entry.node);
                let numeric = service.addr.is_valid();
                let peer = if numeric {
                    peers
                        .iter()
                        .rev()
                        .find(|p| p.addr().service.addr.is_valid() && p.addr().service == service)
                } else {
                    peers
                        .iter()
                        .rev()
                        .find(|p| !p.addr_name().is_empty() && p.addr_name() == entry.node)
                };
                let connection = peer.map(|p| AddedConnection {
                    service: if numeric {
                        service
                    } else {
                        p.addr().service.clone()
                    },
                    inbound: p.is_inbound(),
                });
                (include_connected || connection.is_none()).then_some(AddedNodeInfo {
                    node: entry,
                    connection,
                })
            })
            .collect()
    }

    /// `GetAddedNodeInfo(include_connected = false)`, as the added-node loop reads it.
    // Core: net.cpp:3375 ThreadOpenAddedConnections
    pub(crate) fn unconnected_added_nodes(&self) -> VecDeque<AddedNode> {
        self.added_node_info(false).into_iter().map(|info| info.node).collect()
    }

    /// Core's `GetReachableEmptyNetworks`.
    // Core: net.cpp:2882-2893
    fn reachable_empty_networks(&self) -> Vec<Network> {
        [Network::Ipv4, Network::Ipv6, Network::Onion, Network::I2p, Network::Cjdns]
            .into_iter()
            .filter(|&n| self.is_reachable(n) && self.addrman().size(Some(n), None) == 0)
            .collect()
    }

    /// Core's `MaybePickPreferredNetwork`.
    // Core: net.cpp:2901-2915
    fn maybe_pick_preferred_network(&self) -> Option<Network> {
        let mut nets = [Network::Ipv4, Network::Ipv6, Network::Onion, Network::I2p, Network::Cjdns];
        // Core: `Shuffle(…, FastRandomContext())`
        self.new_rng().shuffle(&mut nets);
        nets.into_iter().find(|&n| {
            self.is_reachable(n)
                && self.network_count(n) == 0
                && self.addrman().size(Some(n), None) != 0
        })
    }

    /// Core's `GetExponentialRand`, whose `GetRand(1 << 48)` is a fresh context.
    fn exponential(&self, now_us: i64, average_secs: i64) -> i64 {
        exponential_rand(now_us, average_secs, self.get_rand(1 << 48))
    }

    /// Core's `use_v2transport(GetLocalServices() & NODE_P2P_V2)`: whether we offer the v2
    /// transport (`-v2transport`), and so the `addnode` RPC's default.
    pub fn we_advertise_v2(&self) -> bool {
        self.config().local_services.0 & ServiceFlags::NODE_P2P_V2.0 != 0
    }

    fn manual(&self, dest: String, conn_type: ConnectionType, grant: Option<Grant>) -> OpenRequest {
        OpenRequest {
            target: Target::Dest(dest),
            conn_type,
            count_failure: false,
            grant,
            use_v2: self.we_advertise_v2(),
            masternode: false,
            masternode_probe: false,
        }
    }

    // Core: net.cpp:2917-3292 CConnman::ThreadOpenConnections
    fn tick_opencon(&self, policy: &dyn DialPolicy) -> DialPlan {
        let Some(mut guard) = self.live_dial() else {
            return DialPlan::Done;
        };
        let dial: &mut DialState = &mut guard;
        loop {
            let st = &mut dial.open;
            match std::mem::take(&mut st.phase) {
                OcPhase::Start => {
                    let cfg = self.config();
                    // Core does not start the thread under `-connect=0`.
                    // Core: net.cpp:4092-4096 CConnman::Start
                    match &cfg.connect {
                        Some(c) if c.is_empty() => return DialPlan::Done,
                        Some(_) => {
                            st.phase = OcPhase::Connect {
                                pass: 0,
                                idx: 0,
                                opened: false,
                            };
                            continue;
                        }
                        None => {}
                    }
                    let start = self.clock().node_now_micros();
                    st.start_us = start;
                    st.next_feeler = self.exponential(start, FEELER_INTERVAL_SECS);
                    st.next_extra_block_relay =
                        self.exponential(start, EXTRA_BLOCK_RELAY_ONLY_PEER_INTERVAL_SECS);
                    st.next_extra_network_peer =
                        self.exponential(start, EXTRA_NETWORK_PEER_INTERVAL_SECS);
                    st.add_fixed_seeds = cfg.fixed_seeds;
                    if !st.add_fixed_seeds {
                        tracing::info!(target: "net", "Fixed seeds are disabled");
                    }
                    st.phase = OcPhase::AddrFetch;
                }
                OcPhase::Connect {
                    pass,
                    idx,
                    opened,
                } => {
                    let connect = self.config().connect.as_deref().unwrap_or_default();
                    if idx == connect.len() {
                        st.phase = OcPhase::ConnectReconnect {
                            pass,
                        };
                        return DialPlan::Sleep(HALF_SECOND);
                    }
                    if !opened {
                        st.phase = OcPhase::Connect {
                            pass,
                            idx,
                            opened: true,
                        };
                        return DialPlan::Open(self.manual(
                            connect[idx].clone(),
                            ConnectionType::Manual,
                            None,
                        ));
                    }
                    st.phase = OcPhase::Connect {
                        pass,
                        idx: idx + 1,
                        opened: false,
                    };
                    let sleeps = pass.min(10);
                    if sleeps > 0 {
                        return DialPlan::Sleep(HALF_SECOND * sleeps as u32);
                    }
                }
                OcPhase::ConnectReconnect {
                    pass,
                } => {
                    if let Some(r) = self.take_reconnection() {
                        st.phase = OcPhase::ConnectReconnect {
                            pass,
                        };
                        return DialPlan::Open(r);
                    }
                    st.phase = OcPhase::Connect {
                        pass: pass + 1,
                        idx: 0,
                        opened: false,
                    };
                }
                // Core: net.cpp:2806-2821 CConnman::ProcessAddrFetch
                OcPhase::AddrFetch => {
                    st.phase = OcPhase::Sleep;
                    if let Some(dest) = dial.addr_fetches.pop_front()
                        && let Some(grant) = Grant::try_acquire(&self.outbound_slots)
                    {
                        return DialPlan::Open(self.manual(
                            dest,
                            ConnectionType::AddrFetch,
                            Some(grant),
                        ));
                    }
                }
                OcPhase::Sleep => {
                    st.phase = OcPhase::Reconnect;
                    return DialPlan::Sleep(HALF_SECOND);
                }
                OcPhase::Reconnect => {
                    if let Some(r) = self.take_reconnection() {
                        st.phase = OcPhase::Reconnect;
                        return DialPlan::Open(r);
                    }
                    st.phase = OcPhase::Grant;
                }
                OcPhase::Grant => {
                    let grant = match st.pending.take() {
                        Some(g) => g,
                        None => match Grant::try_acquire(&self.outbound_slots) {
                            Some(g) => g,
                            None => {
                                st.phase = OcPhase::Grant;
                                return DialPlan::WaitGrant;
                            }
                        },
                    };
                    self.maybe_add_fixed_seeds(dial);
                    dial.open.phase = OcPhase::AddrFetch;
                    match self.pick_outbound(dial, grant, policy) {
                        Some(Pick::Open(req)) => return DialPlan::Open(req),
                        Some(Pick::Feeler(req, delay)) => {
                            dial.open.phase = OcPhase::Feeler(Box::new(req));
                            return DialPlan::Sleep(delay);
                        }
                        None => {}
                    }
                }
                OcPhase::Feeler(req) => {
                    st.phase = OcPhase::AddrFetch;
                    // Logged after the sleep, so an interrupted feeler logs nothing.
                    // Core: net.cpp:3271-3278
                    tracing::debug!(target: "net", "Making feeler connection");
                    return DialPlan::Open(*req);
                }
            }
        }
    }

    /// The fixed-seed fallback after the grant (P2a §3.8.6).
    // Core: net.cpp:2974-3012
    fn maybe_add_fixed_seeds(&self, dial: &mut DialState) {
        let st = &mut dial.open;
        let networks = self.reachable_empty_networks();
        if !st.add_fixed_seeds || networks.is_empty() {
            return;
        }
        let cfg = self.config();
        let now_s = self.clock().node_now().0;
        let add_now = if now_s > st.start_us.div_euclid(1_000_000) + 60 {
            tracing::info!(target: "net", "Adding fixed seeds as 60 seconds have passed and addrman is empty for at least one reachable network");
            true
        } else if !cfg.dns_seed && cfg.seed_nodes.is_empty() && dial.added.is_empty() {
            tracing::info!(target: "net", "Adding fixed seeds as -dnsseed=0 (or IPv4/IPv6 connections are disabled via -onlynet) and neither -addnode nor -seednode are provided");
            true
        } else {
            false
        };
        if !add_now {
            return;
        }
        // `ConvertSeeds`' function-local context.
        let seeds = crate::seeds::convert_seeds(cfg.params.fixed_seeds, now_s, &mut self.new_rng());
        let seeds: Vec<Address> = seeds
            .into_iter()
            .filter(|a| networks.contains(&a.service.addr.network(cfg.allow_private_net)))
            .collect();
        let source = NetAddr::internal("fixedseeds").expect("a name");
        self.addrman().add(&seeds, &source, 0, now_s);
        dial.open.add_fixed_seeds = false;
        tracing::info!(target: "net", "Added {} fixed seeds from reachable networks.", seeds.len());
    }

    /// One `ThreadOpenConnections` iteration after the grant: the connection-type ladder and
    /// the address selection. `None` is Core's `continue` (the grant is released).
    // Core: net.cpp:3014-3290
    fn pick_outbound(
        &self,
        dial: &mut DialState,
        grant: Grant,
        policy: &dyn DialPolicy,
    ) -> Option<Pick> {
        let cfg = self.config();
        let allow_private = cfg.allow_private_net;
        let (mut full, mut block, mut onion, mut privacy) = (0usize, 0usize, 0usize, 0usize);
        let mut netgroups: BTreeSet<Vec<u8>> = BTreeSet::new();
        if !cfg.params.allow_multiple_addresses_from_group {
            for p in self.peers().iter() {
                let ct = p.conn_type();
                if ct == ConnectionType::OutboundFullRelay && !p.masternode_connection() {
                    full += 1;
                }
                if ct == ConnectionType::BlockRelay {
                    block += 1;
                }
                if ct == ConnectionType::OutboundFullRelay && p.network() == Network::Onion {
                    onion += 1;
                }
                if matches!(
                    ct,
                    ConnectionType::Manual
                        | ConnectionType::OutboundFullRelay
                        | ConnectionType::BlockRelay
                ) {
                    let a = &p.addr().service.addr;
                    if a.is_tor() || a.is_i2p() || a.is_cjdns() {
                        privacy += 1;
                    } else {
                        netgroups.insert(self.netgroup().group(a));
                    }
                }
            }
        }

        let st = &mut dial.open;
        let now = self.clock().node_now_micros();
        let mut conn_type = ConnectionType::OutboundFullRelay;
        let (mut anchor, mut feeler, mut onion_only) = (false, false, false);
        let mut preferred_net = None;
        if !dial.anchors.is_empty() && block < cfg.max_outbound_block_relay {
            conn_type = ConnectionType::BlockRelay;
            anchor = true;
        } else if full < cfg.max_outbound_full_relay {
        } else if block < cfg.max_outbound_block_relay {
            conn_type = ConnectionType::BlockRelay;
        } else if policy.try_another_outbound_peer() {
        } else if now > st.next_extra_block_relay && policy.start_extra_block_relay_peers() {
            st.next_extra_block_relay =
                self.exponential(now, EXTRA_BLOCK_RELAY_ONLY_PEER_INTERVAL_SECS);
            conn_type = ConnectionType::BlockRelay;
        } else if now > st.next_feeler {
            st.next_feeler = self.exponential(now, FEELER_INTERVAL_SECS);
            conn_type = ConnectionType::Feeler;
            feeler = true;
        } else if full == cfg.max_outbound_full_relay
            && cfg.max_outbound_full_relay == MAX_OUTBOUND_FULL_RELAY_CONNECTIONS
            && now > st.next_extra_network_peer
            && let Some(net) = self.maybe_pick_preferred_network()
        {
            preferred_net = Some(net);
            st.next_extra_network_peer = self.exponential(now, EXTRA_NETWORK_PEER_INTERVAL_SECS);
        } else if onion < cfg.max_outbound_onion && self.is_reachable(Network::Onion) {
            onion_only = true;
        } else {
            return None;
        }

        let now_s = self.clock().node_now().0;
        self.addrman().resolve_collisions(now_s);

        let mut tries = 0;
        let mut chosen: Option<Address> = None;
        while !self.interrupted() {
            if anchor && let Some(addr) = dial.anchors.pop() {
                let a = &addr.service.addr;
                if !a.is_valid()
                    || self.is_local(a)
                    || !self.is_reachable(a.network(allow_private))
                    || !self.has_all_desirable_services(addr.services)
                    || netgroups.contains(&self.netgroup().group(a))
                {
                    continue;
                }
                tracing::debug!(target: "net", "Trying to make an anchor connection to {}", addr.service);
                chosen = Some(addr);
                break;
            }
            tries += 1;
            if tries > 100 {
                break;
            }
            let picked = if feeler {
                match self.addrman().select_tried_collision() {
                    Some(c) if self.already_connected_to_address(&c.addr) => {
                        self.addrman().good(&c.addr.service, now_s);
                        self.addrman().select(true, None, now_s)
                    }
                    Some(c) => Some(c),
                    None => self.addrman().select(true, None, now_s),
                }
            } else {
                self.addrman().select(false, preferred_net, now_s)
            };
            let (addr, last_try) = picked.map_or((Address::default(), 0), |s| (s.addr, s.last_try));
            let a = &addr.service.addr;
            if !feeler && netgroups.contains(&self.netgroup().group(a)) {
                continue;
            }
            if !a.is_valid() {
                break;
            }
            if addr.service.port == cfg.listen_port && self.is_local(a) {
                break;
            }
            if !self.is_reachable(a.network(allow_private)) {
                continue;
            }
            if onion_only && !a.is_tor() {
                continue;
            }
            if now_s - last_try < 10 * 60 && tries < 30 {
                continue;
            }
            if (!feeler && !self.has_all_desirable_services(addr.services))
                || (feeler && !may_have_useful_address_db(addr.services))
            {
                continue;
            }
            if tries < 50 && is_bad_port(addr.service.port) {
                continue;
            }
            if added_nodes_contain(&dial.added, &addr) {
                tracing::debug!(
                    target: "net",
                    "Not making automatic {}{} connection to {} peer selected for manual (addnode) connection",
                    if preferred_net.is_some() { "network-specific " } else { "" },
                    conn_type.as_str(),
                    a.network(allow_private).name()
                );
                continue;
            }
            if policy.is_connected_masternode(&addr) {
                continue;
            }
            chosen = Some(addr);
            break;
        }

        let addr = chosen.filter(|a| a.service.addr.is_valid())?;
        if let Some(net) = preferred_net {
            tracing::debug!(target: "net", "Making network specific connection to {} on {}.", addr.service, net.name());
        }
        let threshold = (cfg.max_connections as i64 - 1).min(2);
        let count_failure = (netgroups.len() + privacy) as i64 >= threshold;
        let use_v2 = addr.services.0 & cfg.local_services.0 & ServiceFlags::NODE_P2P_V2.0 != 0;
        let req = OpenRequest {
            target: Target::Addr(addr),
            conn_type,
            count_failure,
            grant: Some(grant),
            use_v2,
            masternode: false,
            masternode_probe: false,
        };
        if feeler {
            let rng = dial.open.rng.get_or_insert_with(|| self.new_rng());
            let ns = rng.rand_uniform(FEELER_SLEEP_WINDOW_NS);
            return Some(Pick::Feeler(req, Duration::from_nanos(ns as u64)));
        }
        Some(Pick::Open(req))
    }

    // Core: net.cpp:3368-3395 CConnman::ThreadOpenAddedConnections
    fn tick_addcon(&self) -> DialPlan {
        let Some(mut dial) = self.live_dial() else {
            return DialPlan::Done;
        };
        loop {
            let st = &mut dial.add;
            match std::mem::take(&mut st.phase) {
                AcPhase::Grant => {
                    // The previous iteration's grant goes out of scope here.
                    st.grant = None;
                    let grant = match st.pending.take() {
                        Some(g) => g,
                        None => match Grant::try_acquire(&self.addnode_slots) {
                            Some(g) => g,
                            None => return DialPlan::WaitGrant,
                        },
                    };
                    st.grant = Some(grant);
                    st.tried = false;
                    drop(dial);
                    let infos = self.unconnected_added_nodes();
                    // Released here, the grant just stored is gone and `Iterate` opens nothing;
                    // `Reconnect` finds no retry.
                    dial = self.dial.lock();
                    dial.add.infos = infos;
                    dial.add.phase = AcPhase::Iterate;
                }
                AcPhase::Iterate => {
                    if st.grant.is_some()
                        && let Some(info) = st.infos.pop_front()
                    {
                        st.tried = true;
                        st.phase = AcPhase::Sleep;
                        let grant = st.grant.take();
                        return DialPlan::Open(OpenRequest {
                            use_v2: info.use_v2,
                            ..self.manual(info.node, ConnectionType::Manual, grant)
                        });
                    }
                    st.phase = AcPhase::Reconnect;
                }
                AcPhase::Sleep => {
                    st.phase = AcPhase::Regrant;
                    return DialPlan::Sleep(HALF_SECOND);
                }
                AcPhase::Regrant => {
                    st.grant = Grant::try_acquire(&self.addnode_slots);
                    st.phase = AcPhase::Iterate;
                }
                AcPhase::Reconnect => {
                    if let Some(r) = self.take_reconnection() {
                        st.phase = AcPhase::Reconnect;
                        return DialPlan::Open(r);
                    }
                    st.phase = AcPhase::Grant;
                    return DialPlan::Sleep(Duration::from_secs(if st.tried {
                        60
                    } else {
                        2
                    }));
                }
            }
        }
    }

    // Core: net.cpp:2680-2794 CConnman::ThreadDNSAddressSeed
    fn tick_dnsseed(&self) -> DialPlan {
        let Some(mut dial) = self.live_dial() else {
            return DialPlan::Done;
        };
        loop {
            let st = &mut dial.dns;
            match std::mem::take(&mut st.phase) {
                DnsPhase::Start => {
                    let cfg = self.config();
                    // Core: net.cpp:4073-4076 CConnman::Start
                    if !cfg.dns_seed {
                        tracing::info!(target: "net", "DNS seeding disabled");
                        return DialPlan::Done;
                    }
                    let mut seeds: Vec<String> =
                        cfg.params.dns_seeds.iter().map(|s| (*s).to_owned()).collect();
                    st.rng.get_or_insert_with(|| self.new_rng()).shuffle(&mut seeds);
                    // Two `Size()` calls as in Core (each may run the consistency check).
                    st.right_now = if cfg.force_dns_seed || self.addrman().size(None, None) == 0 {
                        seeds.len()
                    } else {
                        0
                    };
                    st.wait = if self.addrman().size(None, None) >= DNSSEEDS_DELAY_PEER_THRESHOLD {
                        DNSSEEDS_DELAY_MANY_PEERS
                    } else {
                        DNSSEEDS_DELAY_FEW_PEERS
                    };
                    st.seeds = seeds;
                    st.phase = DnsPhase::Seed;
                }
                DnsPhase::Seed => {
                    if st.idx == st.seeds.len() {
                        tracing::info!(target: "net", "{} addresses found from DNS seeds", st.found);
                        st.phase = DnsPhase::Finished;
                        return DialPlan::Done;
                    }
                    if st.right_now == 0 && st.to_wait == 0 {
                        st.right_now += DNSSEEDS_TO_QUERY_AT_ONCE;
                        if self.addrman().size(None, None) > 0 {
                            tracing::info!(target: "net", "Waiting {} seconds before querying DNS seeds.", st.wait);
                            st.to_wait = st.wait;
                        }
                    }
                    if st.to_wait > 0 {
                        let w = st.to_wait.min(DNSSEEDS_DELAY_FEW_PEERS);
                        st.to_wait -= w;
                        st.phase = DnsPhase::Waiting;
                        return DialPlan::Sleep(Duration::from_secs(w));
                    }
                    // Core: net.cpp:2749-2750 (`LogPrintf`: no `[net:info] ` prefix)
                    if !self.network_active() {
                        tracing::info!(
                            "Waiting for network to be reactivated before querying DNS seeds."
                        );
                        st.phase = DnsPhase::Inactive;
                        return DialPlan::Sleep(Duration::from_secs(1));
                    }
                    let seed = st.seeds[st.idx].clone();
                    tracing::info!(target: "net", "Loading addresses from DNS seed {seed}");
                    st.phase = DnsPhase::Resolved;
                    return DialPlan::Resolve {
                        seed,
                        max: 32,
                    };
                }
                DnsPhase::Waiting => {
                    let relevant = self
                        .peers()
                        .iter()
                        .filter(|p| {
                            p.successfully_connected()
                                && p.conn_type() == ConnectionType::OutboundFullRelay
                                && !p.masternode_probe()
                        })
                        .count();
                    if relevant >= 2 {
                        if st.found > 0 {
                            tracing::info!(target: "net", "{} addresses found from DNS seeds", st.found);
                            tracing::info!(target: "net", "P2P peers available. Finished DNS seeding.");
                        } else {
                            tracing::info!(target: "net", "P2P peers available. Skipped DNS seeding.");
                        }
                        st.phase = DnsPhase::Finished;
                        return DialPlan::Done;
                    }
                    st.phase = DnsPhase::Seed;
                }
                DnsPhase::Inactive => {
                    if !self.network_active() {
                        st.phase = DnsPhase::Inactive;
                        return DialPlan::Sleep(Duration::from_secs(1));
                    }
                    let seed = st.seeds[st.idx].clone();
                    tracing::info!(target: "net", "Loading addresses from DNS seed {seed}");
                    st.phase = DnsPhase::Resolved;
                    return DialPlan::Resolve {
                        seed,
                        max: 32,
                    };
                }
                DnsPhase::Resolved => {
                    st.right_now = st.right_now.saturating_sub(1);
                    st.idx += 1;
                    st.phase = DnsPhase::Seed;
                }
                DnsPhase::Finished => {
                    st.phase = DnsPhase::Finished;
                    return DialPlan::Done;
                }
            }
        }
    }

    /// Core's per-seed body of `ThreadDNSAddressSeed` after a [`DialPlan::Resolve`]: resolves
    /// `x1.<seed>` through the host's [`Resolver`](crate::Resolver) and adds the answers to
    /// AddrMan (dated U(now − 7 d, now − 3 d], services `NODE_NETWORK`, source
    /// `internal("x1.<seed>")`), or queues the bare seed as ADDR_FETCH when there are none.
    /// With a name proxy (`-proxy`), the bare seed is queued as ADDR_FETCH without a lookup, so
    /// the proxy resolves it. Returns the number of addresses found.
    // Core: net.cpp:2755-2790 CConnman::ThreadDNSAddressSeed
    pub async fn resolve_seed(&self, seed: &str, max: usize) -> usize {
        if self.name_proxy().is_some() {
            self.add_addr_fetch(seed.to_owned());
            return 0;
        }
        let services = self.desirable_services(ServiceFlags::NONE);
        let host = format!("x{:x}.{seed}", services.0);
        let Some(source) = NetAddr::internal(&host) else {
            return 0;
        };
        let addresses = lookup(&*self.resolver, &host, max, true).await;
        if addresses.is_empty() {
            self.add_addr_fetch(seed.to_owned());
            return 0;
        }
        let now = self.clock().node_now().0;
        let port = self.config().params.default_port;
        let add: Vec<Address> = {
            let mut dial = self.dial.lock();
            let rng = dial.dns.rng.get_or_insert_with(|| self.new_rng());
            addresses
                .into_iter()
                .map(|ip| Address {
                    service: Service::new(ip, port),
                    services,
                    time: (now - 3 * 86_400 + rng.rand_uniform(-4 * 86_400)) as u32,
                })
                .collect()
        };
        self.addrman().add(&add, &source, 0, now);
        let n = add.len();
        self.dial.lock().dns.found += n;
        n
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::Future;
    use std::io;
    use std::pin::Pin;

    use dash_clock::MockClock;
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};
    use parking_lot::Mutex;

    use super::*;
    use crate::addrman::{read_anchors, write_anchors};
    use crate::conn::{PeerHandle, Permissions};
    use crate::netcore::{Attach, InitError, NetConfig, NetEvents, OpenError};
    use crate::sock::MemSocket;
    use crate::store::{NetBlob, NetStore};

    #[derive(Default)]
    struct MemStore {
        blobs: Mutex<HashMap<NetBlob, Vec<u8>>>,
        backups: Mutex<Vec<NetBlob>>,
    }

    impl NetStore for MemStore {
        fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
            Ok(self.blobs.lock().get(&blob).cloned())
        }
        fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
            self.blobs.lock().insert(blob, bytes.to_vec());
            Ok(())
        }
        fn remove(&self, blob: NetBlob) -> io::Result<()> {
            self.blobs.lock().remove(&blob);
            Ok(())
        }
        fn backup(&self, blob: NetBlob) -> io::Result<()> {
            self.backups.lock().push(blob);
            Ok(())
        }
    }

    struct NoPolicy;

    impl DialPolicy for NoPolicy {}

    struct Quiet;

    impl NetEvents for Quiet {
        fn wake(&self) {}
        fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}
        fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
    }

    impl crate::Resolver for Quiet {
        fn resolve<'a>(
            &'a self,
            _host: &'a str,
            _max: usize,
            _allow_lookup: bool,
        ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
            Box::pin(async { Vec::new() })
        }
    }

    fn start(store: &Arc<MemStore>, connect: bool) -> Result<Arc<NetCore>, InitError> {
        start_with(store, |cfg| {
            if connect {
                cfg.connect = Some(vec!["127.0.0.1".into()]);
            }
        })
    }

    fn start_with(
        store: &Arc<MemStore>,
        f: impl FnOnce(&mut NetConfig),
    ) -> Result<Arc<NetCore>, InitError> {
        start_resolving(store, Arc::new(Quiet), f)
    }

    fn start_resolving(
        store: &Arc<MemStore>,
        resolver: Arc<dyn crate::Resolver>,
        f: impl FnOnce(&mut NetConfig),
    ) -> Result<Arc<NetCore>, InitError> {
        let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap();
        let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
        cfg.deterministic = true;
        f(&mut cfg);
        NetCore::new(
            cfg,
            tokio::runtime::Handle::current(),
            store.clone(),
            resolver,
            Arc::new(MockClock::new()),
            Arc::new(Quiet),
        )
        .inspect(|core| core.start_addresses())
    }

    fn attach(core: &NetCore, addr: Address, conn_type: ConnectionType, permissions: Permissions) {
        let (ours, _theirs) = MemSocket::pair();
        core.attach(Attach {
            sock: Arc::new(ours),
            addr,
            addr_bind: Service::default(),
            dest: None,
            conn_type,
            inbound_onion: false,
            permissions,
            use_v2: false,
            grant: None,
            masternode: false,
            masternode_probe: false,
        });
    }

    fn anchor(last: u8) -> Address {
        Address {
            service: Service::new(NetAddr::ipv4([10, 0, 0, last]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 1_700_000_000,
        }
    }

    /// `CConnman::Start` and `StopNodes` with `anchors.dat` and `peers.dat` (net.cpp:4025-4038,
    /// :4187-4200; addrdb.cpp:194-250): at most two anchors are read and the image is removed,
    /// none under `-connect`; a missing `peers.dat` is written at once, an incompatible one
    /// backed up and replaced, a corrupt one refused; shutdown writes the block-relay-only peers.
    /// Before `start_addresses` (a failed bind) neither file is touched again.
    #[tokio::test]
    async fn anchors_and_peers_at_start_and_stop() {
        let magic = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0)
            .build()
            .unwrap()
            .message_start;
        let three = [anchor(1), anchor(2), anchor(3)];

        let store = Arc::new(MemStore::default());
        store.save(NetBlob::Anchors, &write_anchors(&magic, &three)).unwrap();
        let core = start(&store, false).unwrap();
        assert_eq!(core.dial.lock().anchors, three[..2]);
        assert!(store.load(NetBlob::Anchors).unwrap().is_none());
        assert!(store.load(NetBlob::Peers).unwrap().is_some(), "missing peers.dat is created");

        attach(&core, anchor(9), ConnectionType::BlockRelay, Permissions::NONE);
        attach(&core, anchor(10), ConnectionType::Inbound, Permissions::NONE);
        core.shutdown().await;
        let image = store.load(NetBlob::Anchors).unwrap().unwrap();
        assert_eq!(read_anchors(&magic, &image).unwrap(), [anchor(9)]);
        // A second `StopNodes` dumps nothing (`fAddressesInitialized`).
        store.remove(NetBlob::Anchors).unwrap();
        core.shutdown().await;
        assert!(store.load(NetBlob::Anchors).unwrap().is_none());

        // A failed bind: `Start` returns before `ReadAnchors` and `fAddressesInitialized`, so
        // `anchors.dat` survives and `StopNodes` dumps nothing.
        let store = Arc::new(MemStore::default());
        store.save(NetBlob::Anchors, &write_anchors(&magic, &three)).unwrap();
        let core = NetCore::new(
            NetConfig::new(
                Arc::new(
                    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap(),
                ),
                ServiceFlags::NODE_NETWORK,
            ),
            tokio::runtime::Handle::current(),
            store.clone(),
            Arc::new(Quiet),
            Arc::new(MockClock::new()),
            Arc::new(Quiet),
        )
        .unwrap();
        assert!(core.dial.lock().anchors.is_empty());
        store.remove(NetBlob::Peers).unwrap();
        store.remove(NetBlob::Bans).unwrap();
        attach(&core, anchor(13), ConnectionType::BlockRelay, Permissions::NONE);
        core.shutdown().await;
        let image = store.load(NetBlob::Anchors).unwrap().unwrap();
        assert_eq!(read_anchors(&magic, &image).unwrap(), three);
        assert!(store.load(NetBlob::Peers).unwrap().is_none());
        assert!(store.load(NetBlob::Bans).unwrap().is_none());
        // A second `start_addresses` does not read again.
        let store = Arc::new(MemStore::default());
        store.save(NetBlob::Anchors, &write_anchors(&magic, &three)).unwrap();
        let core = start(&store, false).unwrap();
        core.start_addresses();
        assert_eq!(core.dial.lock().anchors, three[..2]);

        // `-blocksonly`: every peer without the Relay permission is block-relay-only
        // (net.cpp:554 `IsBlockRelayOnly`), inbound ones included.
        let store = Arc::new(MemStore::default());
        let core = start_with(&store, |cfg| cfg.blocks_only = true).unwrap();
        attach(&core, anchor(11), ConnectionType::OutboundFullRelay, Permissions::RELAY);
        attach(&core, anchor(12), ConnectionType::Inbound, Permissions::NONE);
        core.shutdown().await;
        let image = store.load(NetBlob::Anchors).unwrap().unwrap();
        assert_eq!(read_anchors(&magic, &image).unwrap(), [anchor(12)]);

        // `-connect`: anchors are neither read nor written.
        let store = Arc::new(MemStore::default());
        store.save(NetBlob::Anchors, &write_anchors(&magic, &three)).unwrap();
        let core = start(&store, true).unwrap();
        assert!(core.dial.lock().anchors.is_empty());
        assert!(store.load(NetBlob::Anchors).unwrap().is_some());

        // A file format newer than ours: backed up, replaced by an empty table.
        let store = Arc::new(MemStore::default());
        let mut newer = magic.to_vec();
        newer.extend_from_slice(&[4, 32 + 5]);
        store.save(NetBlob::Peers, &newer).unwrap();
        start(&store, false).unwrap();
        assert_eq!(*store.backups.lock(), [NetBlob::Peers]);
        assert_ne!(store.load(NetBlob::Peers).unwrap().unwrap(), newer);

        // Corrupt: refused, as Core's "Invalid or corrupt peers.dat" init error.
        let store = Arc::new(MemStore::default());
        let mut corrupt = magic.to_vec();
        corrupt.extend_from_slice(&[4, 31]);
        store.save(NetBlob::Peers, &corrupt).unwrap();
        let err = start(&store, false).err().unwrap();
        assert!(matches!(err, InitError::CorruptPeers { .. }));
        assert_eq!(
            err.to_string(),
            "Invalid or corrupt peers.dat (Corrupted addrman database: The compat value (31) is lower than the expected minimum value 32.: iostream error). If you believe this is a bug, please report it to https://github.com/dashpay/dash/issues. As a workaround, you can move the file (\"peers.dat\") out of the way (rename, move, or delete) to have a new one created on the next start."
        );
    }

    /// `CSemaphoreGrant(sem, true)`: `semOutbound` holds
    /// `min(full-relay + block-relay + feeler, -maxconnections)` slots and `semAddnode`
    /// `-maxaddnode`; a try fails while every slot is held and succeeds once one is dropped.
    #[tokio::test]
    async fn try_grant_takes_a_free_slot() {
        let store = Arc::new(MemStore::default());
        let core = start_with(&store, |cfg| {
            cfg.max_outbound_full_relay = 1;
            cfg.max_outbound_block_relay = 1;
            cfg.max_feeler = 1;
            cfg.max_connections = 2;
            cfg.max_addnode = 1;
        })
        .unwrap();
        let a = core.try_grant(DialLoop::OpenCon).unwrap();
        let _b = core.try_grant(DialLoop::DnsSeed).unwrap();
        assert!(core.try_grant(DialLoop::OpenCon).is_none());
        let add = core.try_grant(DialLoop::AddCon).unwrap();
        assert!(core.try_grant(DialLoop::AddCon).is_none());
        drop(a);
        assert!(core.try_grant(DialLoop::OpenCon).is_some());
        drop(add);
        assert!(core.try_grant(DialLoop::AddCon).is_some());
    }

    /// `interruptNet.sleep_for` (threadinterrupt.cpp:33-37): a dial loop's sleep lasts through a
    /// network change and a peer's removal; only the interrupt ends it early. A sleep with no
    /// event lasts its full length.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_interrupt_ends_a_dial_sleep() {
        let store = Arc::new(MemStore::default());
        let core = start(&store, false).unwrap();
        let sleep = Duration::from_millis(300);
        let started = std::time::Instant::now();
        assert!(core.dial_wait(DialLoop::OpenCon, Some(sleep)).await);
        assert!(started.elapsed() >= sleep);

        attach(&core, anchor(1), ConnectionType::OutboundFullRelay, Permissions::NONE);
        let wait = tokio::spawn({
            let core = core.clone();
            async move { core.dial_wait(DialLoop::DnsSeed, Some(Duration::from_secs(600))).await }
        });
        // Let the wait start first (more time only makes the check stronger).
        tokio::time::sleep(Duration::from_millis(200)).await;
        // With the network inactive the sweep disconnects and removes the peer.
        core.set_network_active(false);
        let removed = async {
            while !core.peers().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(60), removed).await.expect("the peer is removed");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!wait.is_finished(), "neither change ended the sleep");
        core.interrupt();
        let woken = tokio::time::timeout(Duration::from_secs(60), wait).await;
        assert!(!woken.expect("the interrupt ends the sleep").unwrap());
        core.shutdown().await;
    }

    /// Records each lookup's host and permission.
    #[derive(Default)]
    struct Lookups(Mutex<Vec<(String, bool)>>);

    impl crate::Resolver for Lookups {
        fn resolve<'a>(
            &'a self,
            host: &'a str,
            _max: usize,
            allow_lookup: bool,
        ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
            self.0.lock().push((host.to_owned(), allow_lookup));
            Box::pin(async { Vec::new() })
        }
    }

    /// `fNameLookup` (`-dns`) governs a named connection target's lookup (net.cpp:405); a DNS
    /// seed is always looked up (`LookupHost(host, 32, true)`, net.cpp:2775).
    #[tokio::test]
    async fn dns_seeds_look_up_names_whatever_dns_says() {
        for name_lookup in [false, true] {
            let lookups = Arc::new(Lookups::default());
            let store = Arc::new(MemStore::default());
            let core = start_resolving(&store, lookups.clone(), |cfg| {
                cfg.name_lookup = name_lookup;
            })
            .unwrap();
            let req = core.manual("node.example:9999".into(), ConnectionType::Manual, None);
            assert!(matches!(core.open(req).await, Err(OpenError::Resolve(_))));
            assert_eq!(core.resolve_seed("seed.example", 32).await, 0);
            let lookups = lookups.0.lock().clone();
            assert_eq!(lookups[0], ("node.example".to_owned(), name_lookup));
            assert!(lookups[1].0.ends_with(".seed.example") && lookups[1].1);
            assert_eq!(lookups.len(), 2);
            core.shutdown().await;
        }
    }

    /// `StopNodes` (net.cpp:4230-4232): the grant `addcon` holds across its sleep and the one
    /// `dial_wait` kept for `opencon`'s next tick are dropped at shutdown, so both semaphores are
    /// full again; after the interrupt no tick or wait takes another.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_releases_the_loops_grants() {
        let store = Arc::new(MemStore::default());
        let core = start(&store, false).unwrap();
        let full =
            || (core.outbound_slots.available_permits(), core.addnode_slots.available_permits());
        let (outbound, addnode) = full();

        // addcon: the first entry's open (dropped: it failed), the half-second sleep, the
        // regrant, and with no entry left the 60 s sleep, holding the regrant's grant.
        assert!(core.add_node(AddedNode {
            node: "10.0.0.1:9999".into(),
            use_v2: false,
        }));
        assert!(matches!(core.dial_tick(DialLoop::AddCon, &NoPolicy), DialPlan::Open(_)));
        assert!(matches!(core.dial_tick(DialLoop::AddCon, &NoPolicy), DialPlan::Sleep(_)));
        assert!(matches!(
            core.dial_tick(DialLoop::AddCon, &NoPolicy),
            DialPlan::Sleep(d) if d == Duration::from_secs(60)
        ));
        // opencon: every slot held, a wait for one, and one freed.
        let mut held: Vec<Grant> =
            std::iter::from_fn(|| core.try_grant(DialLoop::OpenCon)).collect();
        let wait = tokio::spawn({
            let core = core.clone();
            async move { core.dial_wait(DialLoop::OpenCon, None).await }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        held.pop();
        assert!(wait.await.unwrap());
        drop(held);
        assert_eq!(full(), (outbound - 1, addnode - 1));

        core.shutdown().await;
        assert_eq!(full(), (outbound, addnode));
        assert!(matches!(core.dial_tick(DialLoop::AddCon, &NoPolicy), DialPlan::Done));
        assert!(!core.dial_wait(DialLoop::OpenCon, None).await);
        assert_eq!(full(), (outbound, addnode));
    }

    /// `InactivityCheck` (net.cpp:2293-2333): nothing before `m_connected + -peertimeout`;
    /// after it, no message either way, more than 20 minutes without sending or without
    /// receiving, or no completed handshake disconnects, with the V2 text while the transport is
    /// still detecting. Exactly 20 minutes does not.
    #[tokio::test]
    async fn inactivity_check() {
        const T0: i64 = 1_700_000_000;
        let clock = Arc::new(MockClock::at(T0));
        let mut cfg = NetConfig::new(
            Arc::new(
                ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap(),
            ),
            ServiceFlags::NODE_NETWORK,
        );
        cfg.peer_timeout = 60;
        let core = NetCore::new(
            cfg,
            tokio::runtime::Handle::current(),
            Arc::new(MemStore::default()),
            Arc::new(Quiet),
            clock.clone(),
            Arc::new(Quiet),
        )
        .unwrap();
        let peer = |last: u8, use_v2: bool| {
            let (ours, _theirs) = MemSocket::pair();
            core.attach(Attach {
                sock: Arc::new(ours),
                addr: anchor(last),
                addr_bind: Service::default(),
                dest: None,
                conn_type: ConnectionType::OutboundFullRelay,
                inbound_onion: false,
                permissions: Permissions::NONE,
                use_v2,
                grant: None,
                masternode: false,
                masternode_probe: false,
            })
        };
        let [silent, busy, no_verack, send_stale, recv_stale, edge] =
            [1, 2, 3, 4, 5, 6].map(|last| peer(last, false));
        // A V2 peer whose version packet has not arrived: Core's transport is still detecting.
        let detecting = peer(7, true);
        for p in [&busy, &send_stale, &recv_stale, &edge] {
            p.set_successfully_connected();
        }

        clock.set_mocktime(T0 + 60);
        core.inactivity_checks();
        assert!(core.peers().iter().all(|p| !p.disconnect_requested()));

        let now = T0 + 2000;
        clock.set_mocktime(now);
        busy.set_last_io(now - 10, now - 10);
        no_verack.set_last_io(now - 10, now - 10);
        send_stale.set_last_io(now - 1201, now - 10);
        recv_stale.set_last_io(now - 10, now - 1201);
        edge.set_last_io(now - 1200, now - 1200);
        detecting.set_last_io(now - 10, now - 10);
        let peers = [&silent, &busy, &no_verack, &send_stale, &recv_stale, &edge, &detecting];
        let why: Vec<Option<String>> = peers.map(|p| core.inactivity(p, now)).into();
        let want = [
            Some("socket no message in first 60 seconds, 0 0 peer=0"),
            None,
            Some("version handshake timeout peer=2"),
            Some("socket sending timeout: 1201s peer=3"),
            Some("socket receive timeout: 1201s peer=4"),
            None,
            Some("V2 handshake timeout peer=6"),
        ];
        assert_eq!(why, want.map(|w| w.map(str::to_owned)));
        core.inactivity_checks();
        let dropped: Vec<bool> = peers.map(|p| p.disconnect_requested()).into();
        assert_eq!(dropped, want.map(|w| w.is_some()));
    }

    /// Core does not start `dnsseed` under `-dnsseed=0` nor `opencon` under `-connect=0`
    /// (net.cpp:4073-4096).
    #[tokio::test]
    async fn loops_core_does_not_start() {
        let store = Arc::new(MemStore::default());
        let core = start_with(&store, |cfg| {
            cfg.dns_seed = false;
            cfg.connect = Some(Vec::new());
        })
        .unwrap();
        assert!(matches!(core.dial_tick(DialLoop::DnsSeed, &NoPolicy), DialPlan::Done));
        assert!(matches!(core.dial_tick(DialLoop::OpenCon, &NoPolicy), DialPlan::Done));
    }

    /// `g_reachable_nets`: overlays need the host's routing (D27), and `-onlynet` drops every
    /// other network, the unroutable and internal ones included (`RemoveAll`).
    #[tokio::test]
    async fn reachable_networks() {
        let store = Arc::new(MemStore::default());
        let core = start_with(&store, |_| {}).unwrap();
        for net in [Network::Unroutable, Network::Ipv4, Network::Ipv6, Network::Internal] {
            assert!(core.is_reachable(net), "{net:?}");
        }
        for net in [Network::Onion, Network::I2p, Network::Cjdns] {
            assert!(!core.is_reachable(net), "{net:?}");
        }
        let core = start_with(&store, |cfg| {
            cfg.only_net = vec![Network::Ipv4, Network::Onion, Network::I2p];
            cfg.reachable_overlays = vec![Network::Onion, Network::Cjdns];
        })
        .unwrap();
        for (net, reachable) in [
            (Network::Unroutable, false),
            (Network::Ipv4, true),
            (Network::Ipv6, false),
            (Network::Onion, true),
            (Network::I2p, false),
            (Network::Cjdns, false),
            (Network::Internal, false),
        ] {
            assert_eq!(core.is_reachable(net), reachable, "{net:?}");
        }
    }

    #[test]
    fn bad_ports() {
        for p in [1, 22, 1023, 1719, 6665, 6669, 8333, 18333] {
            assert!(is_bad_port(p), "{p}");
        }
        for p in [0, 1024, 9999, 19999, 6664, 6670] {
            assert!(!is_bad_port(p), "{p}");
        }
    }
}

#[cfg(test)]
#[path = "dial_loop_tests.rs"]
pub(crate) mod loop_tests;
