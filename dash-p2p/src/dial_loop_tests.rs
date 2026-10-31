//! Behavioural tests of the dial loops against a seeded deterministic AddrMan and a mock clock
//! (SB6 review M2): `ThreadOpenConnections`' ladder, selection rules, fixed seeds and `-connect`
//! back-off, and `ThreadDNSAddressSeed`'s waits, batches and stop rule. The harness is shared
//! with the inbound tests.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dash_clock::{Clock, MockClock, NodeSeconds, NodeTime};
use dash_p2p_core::ServiceFlags;
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;

use super::{AddedNode, DialLoop, DialPlan, DialPolicy, DnsPhase};
use crate::conn::{ConnectionType, PeerHandle, Permissions};
use crate::netaddr::{Address, NetAddr, Network, Service};
use crate::netcore::{Attach, NetConfig, NetCore, NetEvents, OpenRequest, Target};
use crate::sock::{MemSocket, NetSocket};
use crate::store::NoStore;

/// The mock time the cores start at.
pub(crate) const T0: i64 = 1_700_000_000;
const HALF: Duration = Duration::from_millis(500);

pub(crate) struct Silent;

impl NetEvents for Silent {
    fn wake(&self) {}
    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}
    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
}

/// A resolver with fixed answers per host, recording each query.
#[derive(Default)]
pub(crate) struct Answers {
    answers: HashMap<String, Vec<NetAddr>>,
    pub(crate) asked: Mutex<Vec<String>>,
}

impl crate::Resolver for Answers {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        self.asked.lock().push(host.to_owned());
        let mut found = self.answers.get(host).cloned().unwrap_or_default();
        found.truncate(max);
        Box::pin(async move { found })
    }
}

/// The mock clock with its steady time held at one instant: a test whose steady deadline
/// (the 100 ms linger) must not run out in real time while it waits (BL-244).
struct Held {
    mock: Arc<MockClock>,
    steady: Instant,
}

impl Clock for Held {
    fn node_now(&self) -> NodeSeconds {
        self.mock.node_now()
    }

    fn node_time(&self) -> NodeTime {
        self.mock.node_time()
    }

    fn steady(&self) -> Instant {
        self.steady
    }
}

/// Regtest's parameters (`allow_multiple_addresses_from_group`, no fixed seeds).
pub(crate) fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap()
}

/// Regtest with mainnet's group rule: one outbound per netgroup, so the counts and the
/// netgroup skip apply.
fn one_per_group() -> ChainParams {
    let mut params = regtest();
    params.allow_multiple_addresses_from_group = false;
    params
}

/// A core on the mock clock (at [`T0`]) with deterministic randomness.
pub(crate) struct Net {
    pub(crate) core: Arc<NetCore>,
    pub(crate) clock: Arc<MockClock>,
    /// The far ends of the attached sockets, kept open so no peer sees EOF.
    far: Mutex<Vec<MemSocket>>,
}

impl Net {
    pub(crate) fn new(params: ChainParams, f: impl FnOnce(&mut NetConfig)) -> Net {
        Net::resolving(params, Answers::default(), f)
    }

    /// A core whose steady clock stands still (see [`Held`]).
    pub(crate) fn held(params: ChainParams, f: impl FnOnce(&mut NetConfig)) -> Net {
        Net::build(params, Answers::default(), true, f)
    }

    pub(crate) fn resolving(
        params: ChainParams,
        resolver: Answers,
        f: impl FnOnce(&mut NetConfig),
    ) -> Net {
        Net::build(params, resolver, false, f)
    }

    fn build(
        params: ChainParams,
        resolver: Answers,
        held: bool,
        f: impl FnOnce(&mut NetConfig),
    ) -> Net {
        let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
        cfg.deterministic = true;
        f(&mut cfg);
        let clock = Arc::new(MockClock::at(T0));
        let core_clock: Arc<dyn Clock> = if held {
            Arc::new(Held {
                mock: clock.clone(),
                steady: Instant::now(),
            })
        } else {
            clock.clone()
        };
        let core = NetCore::new(
            cfg,
            tokio::runtime::Handle::current(),
            Arc::new(NoStore),
            Arc::new(resolver),
            core_clock,
            Arc::new(Silent),
        )
        .unwrap();
        core.start_addresses();
        Net {
            core,
            clock,
            far: Mutex::new(Vec::new()),
        }
    }

    /// A socket whose far end stays open.
    pub(crate) fn sock(&self) -> Arc<dyn NetSocket> {
        let (ours, theirs) = MemSocket::pair();
        self.far.lock().push(theirs);
        Arc::new(ours)
    }

    pub(crate) fn attach_with(
        &self,
        addr: Address,
        conn_type: ConnectionType,
        f: impl FnOnce(&mut Attach),
    ) -> PeerHandle {
        let mut a = Attach {
            sock: self.sock(),
            addr,
            addr_bind: Service::default(),
            dest: None,
            conn_type,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: false,
            grant: None,
            masternode: false,
            masternode_probe: false,
        };
        f(&mut a);
        self.core.attach(a)
    }

    pub(crate) fn attach(&self, addr: Address, conn_type: ConnectionType) -> PeerHandle {
        self.attach_with(addr, conn_type, |_| {})
    }

    /// Performs a planned open as a successful connect would: the peer, with its grant.
    fn connect(&self, req: OpenRequest) -> PeerHandle {
        let Target::Addr(addr) = req.target else {
            panic!("an AddrMan open");
        };
        self.attach_with(addr, req.conn_type, |a| a.grant = req.grant)
    }

    /// Adds `addrs` to AddrMan's new table, each from its own source.
    fn learn(&self, addrs: &[Address]) {
        for a in addrs {
            self.core.addrman().add(std::slice::from_ref(a), &a.service.addr, 0, T0);
        }
    }

    /// The first `OpenCon` tick: `Start`, no ADDR_FETCH, then the loop head's 500 ms sleep.
    fn start_opencon(&self, policy: &dyn DialPolicy) {
        assert_eq!(plan(self.core.dial_tick(DialLoop::OpenCon, policy)), "sleep 500ms");
    }

    /// One `ThreadOpenConnections` iteration after the loop head's sleep: the grant, the fixed
    /// seeds, the ladder and the selection. The next head's sleep is consumed too, so the loop
    /// is again just past its head.
    fn iterate(&self, policy: &dyn DialPolicy) -> Iter {
        match self.core.dial_tick(DialLoop::OpenCon, policy) {
            DialPlan::Open(req) => {
                self.start_opencon(policy);
                Iter::Open(req)
            }
            // `continue`, or no address: the iteration ends at the next head's sleep.
            DialPlan::Sleep(d) if d == HALF => Iter::Continue,
            DialPlan::Sleep(d) => {
                assert!(d < Duration::from_secs(1), "the feeler's sleep");
                let DialPlan::Open(req) = self.core.dial_tick(DialLoop::OpenCon, policy) else {
                    panic!("the feeler opens after its sleep");
                };
                self.start_opencon(policy);
                Iter::Feeler(req)
            }
            DialPlan::WaitGrant => Iter::WaitGrant,
            other => panic!("unexpected {}", plan(other)),
        }
    }

    /// A DNS-loop trace up to `Done`: each [`DialPlan::Resolve`] is performed.
    async fn dns_trace(&self) -> Vec<String> {
        let mut trace = Vec::new();
        loop {
            let p = self.core.dial_tick(DialLoop::DnsSeed, &Probe::default());
            if let DialPlan::Resolve {
                seed,
                max,
            } = &p
            {
                self.core.resolve_seed(seed, *max).await;
            }
            let done = matches!(p, DialPlan::Done);
            trace.push(plan(p));
            if done || trace.len() > 200 {
                return trace;
            }
        }
    }
}

#[derive(Debug)]
enum Iter {
    Open(OpenRequest),
    Feeler(OpenRequest),
    Continue,
    WaitGrant,
}

impl Iter {
    fn open(self) -> OpenRequest {
        match self {
            Iter::Open(req) => req,
            other => panic!("expected an open, got {other:?}"),
        }
    }
}

/// A plan as text.
fn plan(p: DialPlan) -> String {
    match p {
        DialPlan::Open(req) => {
            let target = match &req.target {
                Target::Addr(a) => a.service.to_string(),
                Target::Dest(d) => d.clone(),
            };
            format!("open {} {target}", req.conn_type.as_str())
        }
        DialPlan::Sleep(d) => format!("sleep {d:?}"),
        DialPlan::WaitGrant => "wait".into(),
        DialPlan::Resolve {
            seed,
            max,
        } => format!("resolve {seed} {max}"),
        DialPlan::Done => "done".into(),
    }
}

fn addr(octets: [u8; 4], port: u16) -> Address {
    Address {
        service: Service::new(NetAddr::ipv4(octets), port),
        services: ServiceFlags::NODE_NETWORK,
        time: (T0 - 3600) as u32,
    }
}

/// `n` addresses, each in its own /16.
fn spread(first: u8, n: u8) -> Vec<Address> {
    (0..n).map(|i| addr([first + i, 1, 1, 1], 19899)).collect()
}

fn target(req: &OpenRequest) -> &Address {
    match &req.target {
        Target::Addr(a) => a,
        Target::Dest(d) => panic!("an AddrMan open, not {d}"),
    }
}

/// The processor's inputs, recording each address the masternode test (the last selection
/// rule, Dash 9(m)) is asked about, so the tries that pass every earlier rule are counted.
#[derive(Default)]
struct Probe {
    try_another: AtomicBool,
    extra_block_relay: AtomicBool,
    masternode: AtomicBool,
    asked: Mutex<Vec<Address>>,
}

impl Probe {
    fn asked(&self) -> usize {
        std::mem::take(&mut *self.asked.lock()).len()
    }
}

impl DialPolicy for Probe {
    fn try_another_outbound_peer(&self) -> bool {
        self.try_another.load(Ordering::SeqCst)
    }
    fn start_extra_block_relay_peers(&self) -> bool {
        self.extra_block_relay.load(Ordering::SeqCst)
    }
    fn is_connected_masternode(&self, addr: &Address) -> bool {
        self.asked.lock().push(addr.clone());
        self.masternode.load(Ordering::SeqCst)
    }
}

/// The connection-type ladder (net.cpp:3095-3148) in Core's order: an anchor, the full-relay
/// peers, the block-relay peers, then with every slot type full the stale-tip extra, the extra
/// block-relay peer, the feeler, the preferred network and the onion-only peer, and with none of
/// them due Core's `continue`.
#[tokio::test]
async fn opencon_ladder_in_core_order() {
    let net = Net::new(one_per_group(), |cfg| cfg.reachable_overlays = vec![Network::Onion]);
    let anchor = addr([50, 1, 1, 1], 19899);
    net.core.dial.lock().anchors = vec![anchor.clone()];
    net.learn(&spread(20, 20));
    let policy = Probe::default();
    net.start_opencon(&policy);

    let req = net.iterate(&policy).open();
    assert_eq!((req.conn_type, target(&req)), (ConnectionType::BlockRelay, &anchor));
    net.connect(req);
    let mut groups = vec![net.core.netgroup().group(&anchor.service.addr)];
    for _ in 0..8 {
        let req = net.iterate(&policy).open();
        assert_eq!(req.conn_type, ConnectionType::OutboundFullRelay);
        let group = net.core.netgroup().group(&target(&req).service.addr);
        assert!(!groups.contains(&group), "one outbound per netgroup");
        groups.push(group);
        net.connect(req);
    }
    let req = net.iterate(&policy).open();
    assert_eq!(req.conn_type, ConnectionType::BlockRelay);
    net.connect(req);

    // Every type full, no timer due: only the onion row (no onion address known) is left.
    // The anchor is not offered to the selection rules.
    assert_eq!(policy.asked(), 9);
    assert!(matches!(net.iterate(&policy), Iter::Continue));
    assert_eq!(policy.asked(), 0, "the onion-only row skips every IPv4 address");

    // All three timers due: the stale-tip extra comes first, then the extra block-relay peer,
    // then the feeler; each later row is reached once the earlier one stops firing.
    net.clock.set_mocktime(T0 + 100_000);
    policy.try_another.store(true, Ordering::SeqCst);
    policy.extra_block_relay.store(true, Ordering::SeqCst);
    assert_eq!(net.iterate(&policy).open().conn_type, ConnectionType::OutboundFullRelay);
    policy.try_another.store(false, Ordering::SeqCst);
    assert_eq!(net.iterate(&policy).open().conn_type, ConnectionType::BlockRelay);
    let Iter::Feeler(req) = net.iterate(&policy) else {
        panic!("the feeler");
    };
    assert_eq!(req.conn_type, ConnectionType::Feeler);
    // The feeler closes, returning its slot.
    drop(req);
    // The extra-network timer is due but no reachable network is empty of peers and known:
    // the onion row again.
    assert!(matches!(net.iterate(&policy), Iter::Continue));

    // Known IPv6 with no IPv6 peer: the preferred-network row picks it.
    let v6 = Address {
        service: Service::new(NetAddr::from_ip("2a01:4f8::1".parse().unwrap()), 19899),
        ..addr([0; 4], 0)
    };
    net.learn(std::slice::from_ref(&v6));
    let req = net.iterate(&policy).open();
    assert_eq!((req.conn_type, target(&req)), (ConnectionType::OutboundFullRelay, &v6));
    drop(req);

    // The onion-only row opens a known onion address.
    let onion = |b: u8| Address {
        service: Service::new(NetAddr::new(Network::Onion, vec![b; 32]).unwrap(), 9999),
        ..addr([0; 4], 0)
    };
    net.learn(&[onion(7)]);
    let req = net.iterate(&policy).open();
    assert_eq!((req.conn_type, target(&req)), (ConnectionType::OutboundFullRelay, &onion(7)));
    drop(req);

    // Two onion peers: nothing is due, Core's `continue`, without a selection.
    net.attach(onion(8), ConnectionType::OutboundFullRelay);
    net.attach(onion(9), ConnectionType::OutboundFullRelay);
    policy.asked();
    assert!(matches!(net.iterate(&policy), Iter::Continue));
    assert_eq!(policy.asked(), 0);
}

/// Regtest's `allow_multiple_addresses_from_group` (net.cpp:3028): nothing is counted, so every
/// pick is full-relay and one netgroup may hold them all, until the outbound slots run out.
#[tokio::test]
async fn opencon_allows_one_group_on_regtest() {
    let net = Net::new(regtest(), |_| {});
    let same_group: Vec<Address> = (1..=20).map(|i| addr([30, 1, 1, i], 19899)).collect();
    net.learn(&same_group);
    let policy = Probe::default();
    net.start_opencon(&policy);
    // `semOutbound`: min(8 + 2 + 1, 125) slots.
    for _ in 0..11 {
        let req = net.iterate(&policy).open();
        assert_eq!(req.conn_type, ConnectionType::OutboundFullRelay);
        net.connect(req);
    }
    assert!(matches!(net.iterate(&policy), Iter::WaitGrant));
}

/// A one-address AddrMan holding `a`: how many of the 100 tries pass every rule before the
/// masternode test, which refuses them all.
async fn tries_passing(a: Address, f: impl FnOnce(&Net)) -> usize {
    let net = Net::new(one_per_group(), |_| {});
    net.learn(std::slice::from_ref(&a));
    // In the tried table (a quarter of the new table's buckets, so `Select` finds it sooner),
    // last tried long ago.
    assert!(net.core.addrman().good(&a.service, T0 - 100_000));
    f(&net);
    let policy = Probe::default();
    policy.masternode.store(true, Ordering::SeqCst);
    net.start_opencon(&policy);
    assert!(matches!(net.iterate(&policy), Iter::Continue));
    let asked = policy.asked.lock();
    assert!(asked.iter().all(|x| *x == a));
    asked.len()
}

/// The 100-try loop's skip rules (net.cpp:3155-3262): each address is offered to the
/// masternode test only on the tries its rules let through.
#[tokio::test]
async fn opencon_selection_skip_rules() {
    let a = addr([1, 2, 3, 4], 19899);
    // Nothing skips: every try.
    assert_eq!(tries_passing(a.clone(), |_| {}).await, 100);

    // Tried within 10 minutes: skipped while `nTries < 30` (net.cpp:3230), so tries 30–100.
    let recent = |ago: i64| {
        let service = a.service.clone();
        move |net: &Net| net.core.addrman().attempt(&service, false, T0 - ago)
    };
    assert_eq!(tries_passing(a.clone(), recent(60)).await, 71);
    assert_eq!(tries_passing(a.clone(), recent(599)).await, 71);
    assert_eq!(tries_passing(a.clone(), recent(600)).await, 100);

    // A bad port: skipped while `nTries < 50` (net.cpp:3245), so tries 50–100.
    let bad = addr([1, 2, 3, 4], 22);
    assert_eq!(tries_passing(bad.clone(), |_| {}).await, 51);
    let recent_bad = |net: &Net| net.core.addrman().attempt(&bad.service, false, T0 - 60);
    assert_eq!(tries_passing(bad.clone(), recent_bad).await, 51);

    // Without the desirable services.
    let useless = Address {
        services: ServiceFlags::NONE,
        ..a.clone()
    };
    assert_eq!(tries_passing(useless, |_| {}).await, 0);

    // An `-addnode` entry, as `ip:port` or `ip` (net.cpp:4335-4344), while fewer than 24 exist.
    for entry in ["1.2.3.4:19899", "1.2.3.4"] {
        let added = |net: &Net| {
            net.core.add_node(AddedNode {
                node: entry.into(),
                use_v2: false,
            });
        };
        assert_eq!(tries_passing(a.clone(), added).await, 0, "{entry}");
    }
    let many_added = |net: &Net| {
        for i in 0..23 {
            net.core.add_node(AddedNode {
                node: format!("9.9.9.{i}"),
                use_v2: false,
            });
        }
        net.core.add_node(AddedNode {
            node: "1.2.3.4".into(),
            use_v2: false,
        });
    };
    assert_eq!(tries_passing(a.clone(), many_added).await, 100);
}

/// Netgroup diversity (net.cpp:3207): an address in the netgroup of a manual, full-relay or
/// block-relay peer is skipped before any other rule; an inbound peer's group is not counted.
#[tokio::test]
async fn opencon_skips_connected_netgroups() {
    for (conn_type, skipped) in [
        (ConnectionType::OutboundFullRelay, true),
        (ConnectionType::BlockRelay, true),
        (ConnectionType::Manual, true),
        (ConnectionType::Inbound, false),
    ] {
        let net = Net::new(one_per_group(), |_| {});
        net.attach(addr([1, 2, 3, 4], 19899), conn_type);
        let same_group = addr([1, 2, 9, 9], 19899);
        net.learn(std::slice::from_ref(&same_group));
        let policy = Probe::default();
        net.start_opencon(&policy);
        match net.iterate(&policy) {
            Iter::Continue => assert!(skipped, "{conn_type:?}"),
            Iter::Open(req) => {
                assert!(!skipped, "{conn_type:?}");
                assert_eq!(target(&req), &same_group);
            }
            other => panic!("{other:?}"),
        }
        if skipped {
            assert_eq!(policy.asked(), 0, "{conn_type:?}");
            // Another group is opened.
            let other = addr([5, 6, 7, 8], 19899);
            net.learn(std::slice::from_ref(&other));
            assert_eq!(target(&net.iterate(&policy).open()), &other);
        }
    }
}

/// Dash 9(m) (net.cpp:3260-3262): a masternode a connected peer verified is skipped.
#[tokio::test]
async fn opencon_skips_connected_masternodes() {
    let a = addr([1, 2, 3, 4], 19899);
    assert_eq!(tries_passing(a.clone(), |_| {}).await, 100);
    let net = Net::new(one_per_group(), |_| {});
    net.learn(std::slice::from_ref(&a));
    let policy = Probe::default();
    net.start_opencon(&policy);
    let req = net.iterate(&policy).open();
    assert_eq!(target(&req), &a);
    assert_eq!(policy.asked(), 1);
}

/// The fixed-seed fallback (net.cpp:2974-3012): with an empty reachable network, after more
/// than 60 seconds; at once under `-dnsseed=0` with no `-addnode` and no `-seednode`; only the
/// reachable empty networks' seeds; once; never under `-fixedseeds=0`.
#[tokio::test]
async fn opencon_fixed_seeds() {
    let with_seeds = || {
        let mut params = regtest();
        let main = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().unwrap();
        params.fixed_seeds = main.fixed_seeds;
        params
    };
    let policy = Probe::default();
    let size = |net: &Net| net.core.addrman().size(None, None);

    let net = Net::new(with_seeds(), |_| {});
    net.start_opencon(&policy);
    for t in [0, 30, 60] {
        net.clock.set_mocktime(T0 + t);
        assert!(matches!(net.iterate(&policy), Iter::Continue));
        assert_eq!(size(&net), 0, "not yet at start + {t} s");
    }
    net.clock.set_mocktime(T0 + 61);
    net.iterate(&policy);
    assert!(size(&net) > 0, "added once 60 s have passed");
    let clearnet = net.core.addrman().size(Some(Network::Ipv4), None)
        + net.core.addrman().size(Some(Network::Ipv6), None);
    assert_eq!(clearnet, size(&net), "only reachable networks' seeds");
    // Once only.
    net.core.addrman().clear();
    net.clock.set_mocktime(T0 + 1000);
    net.iterate(&policy);
    assert_eq!(size(&net), 0);

    // `-dnsseed=0` and nothing to connect to: at once, and only the empty reachable networks'.
    let net = Net::new(with_seeds(), |cfg| {
        cfg.dns_seed = false;
        cfg.only_net = vec![Network::Ipv4];
    });
    net.start_opencon(&policy);
    net.iterate(&policy);
    assert!(size(&net) > 0);
    assert_eq!(net.core.addrman().size(Some(Network::Ipv4), None), size(&net));

    // `-dnsseed=0` with an `-addnode` or a `-seednode`: the 60 s wait applies.
    for seeded in [false, true] {
        let net = Net::new(with_seeds(), |cfg| {
            cfg.dns_seed = false;
            if seeded {
                cfg.seed_nodes = vec!["seed.example".into()];
            } else {
                cfg.added_nodes = vec!["9.9.9.9".into()];
            }
        });
        if seeded {
            // The `-seednode` is opened as ADDR_FETCH first; no grant is held for it here.
            let first = net.core.dial_tick(DialLoop::OpenCon, &policy);
            assert_eq!(plan(first), "open addr-fetch seed.example");
        }
        net.start_opencon(&policy);
        net.iterate(&policy);
        assert_eq!(size(&net), 0, "seeded {seeded}");
        net.clock.set_mocktime(T0 + 61);
        net.iterate(&policy);
        assert!(size(&net) > 0, "seeded {seeded}");
    }

    // `-fixedseeds=0`.
    let net = Net::new(with_seeds(), |cfg| {
        cfg.fixed_seeds = false;
        cfg.dns_seed = false;
    });
    net.start_opencon(&policy);
    net.clock.set_mocktime(T0 + 1000);
    net.iterate(&policy);
    assert_eq!(size(&net), 0);
}

/// `-connect` (net.cpp:2925-2944): each pass opens every destination as MANUAL without a grant,
/// sleeping `min(pass, 10) × 500 ms` after each, then 500 ms and the V1 retries.
#[tokio::test]
async fn connect_backs_off_per_pass() {
    let net = Net::new(regtest(), |cfg| {
        cfg.connect = Some(vec!["1.2.3.4:19899".into(), "5.6.7.8".into()]);
    });
    let mut trace = Vec::new();
    let mut want = Vec::new();
    for pass in 0..13u64 {
        for dest in ["1.2.3.4:19899", "5.6.7.8"] {
            want.push(format!("open manual {dest}"));
            if pass > 0 {
                want.push(format!("sleep {:?}", HALF * pass.min(10) as u32));
            }
        }
        want.push("sleep 500ms".into());
    }
    while trace.len() < want.len() {
        let p = net.core.dial_tick(DialLoop::OpenCon, &Probe::default());
        if let DialPlan::Open(req) = &p {
            assert!(req.grant.is_none() && !req.count_failure);
        }
        trace.push(plan(p));
    }
    assert_eq!(trace, want);
}

const SEEDS: &[&str] = &["s1.example.", "s2.example.", "s3.example.", "s4.example.", "s5.example."];

fn with_dns_seeds() -> ChainParams {
    let mut params = regtest();
    params.dns_seeds = SEEDS;
    params
}

/// Five resolves in the shuffled seed order.
fn resolves(trace: &[String]) -> Vec<String> {
    let r: Vec<String> = trace.iter().filter(|p| p.starts_with("resolve")).cloned().collect();
    let mut sorted = r.clone();
    sorted.sort();
    let want: Vec<String> = SEEDS.iter().map(|s| format!("resolve {s} 32")).collect();
    assert_eq!(sorted, want);
    r
}

/// `ThreadDNSAddressSeed` (net.cpp:2680-2794) with a known address: batches of three, each after
/// an 11 s wait in 11 s steps (300 s with 1000 known); the loop is finished for good once done.
#[tokio::test]
async fn dnsseed_waits_and_batches() {
    let net = Net::new(with_dns_seeds(), |_| {});
    net.learn(&[addr([1, 2, 3, 4], 19899)]);
    let trace = net.dns_trace().await;
    let r = resolves(&trace);
    let s11 = "sleep 11s".to_owned();
    assert_eq!(
        trace,
        [&s11, &r[0], &r[1], &r[2], &s11, &r[3], &r[4], "done"].map(|s| s.to_owned())
    );
    // Core's thread has returned (n2): the loop stays finished, with no fresh start (and so no
    // second shuffle draw) behind the `Done`.
    for _ in 0..3 {
        assert!(matches!(net.core.dial_tick(DialLoop::DnsSeed, &Probe::default()), DialPlan::Done));
        assert!(matches!(net.core.dial.lock().dns.phase, DnsPhase::Finished));
    }

    // 1000 known: 300 s, slept as 27 × 11 s and 3 s.
    let net = Net::new(with_dns_seeds(), |_| {});
    let many: Vec<Address> = (0..1500u32)
        .map(|i| addr([20 + (i / 250) as u8, (i % 250) as u8 + 1, 1, 1], 19899))
        .collect();
    net.learn(&many);
    assert!(net.core.addrman().size(None, None) >= 1000);
    let trace = net.dns_trace().await;
    let mut want = vec!["sleep 11s".to_owned(); 27];
    want.push("sleep 3s".into());
    assert_eq!(trace[..28], want[..]);
    assert!(trace[28..31].iter().all(|p| p.starts_with("resolve")));
    assert_eq!(trace[31..59], want[..]);
}

/// With nothing known, or `-forcednsseed`, every seed is queried at once.
#[tokio::test]
async fn dnsseed_at_once_when_empty_or_forced() {
    for forced in [false, true] {
        let net = Net::new(with_dns_seeds(), |cfg| cfg.force_dns_seed = forced);
        if forced {
            net.learn(&[addr([1, 2, 3, 4], 19899)]);
        }
        let trace = net.dns_trace().await;
        let mut want = resolves(&trace);
        want.push("done".into());
        assert_eq!(trace, want, "forced {forced}");
    }
}

/// The stop rule (net.cpp:2727-2741): two handshaken full-outbound non-probe peers end the loop
/// at the first wait step, for good. Block-relay, manual, inbound, probe and unfinished peers do
/// not count.
#[tokio::test]
async fn dnsseed_stops_at_two_full_outbound_peers() {
    for relevant in [1u8, 2] {
        let net = Net::new(with_dns_seeds(), |_| {});
        net.learn(&[addr([1, 2, 3, 4], 19899)]);
        for i in 0..relevant {
            net.attach(addr([20 + i, 1, 1, 1], 19899), ConnectionType::OutboundFullRelay)
                .set_successfully_connected();
        }
        for (i, ct) in [ConnectionType::BlockRelay, ConnectionType::Manual, ConnectionType::Inbound]
            .into_iter()
            .enumerate()
        {
            net.attach(addr([40 + i as u8, 1, 1, 1], 19899), ct).set_successfully_connected();
        }
        net.attach_with(addr([50, 1, 1, 1], 19899), ConnectionType::OutboundFullRelay, |a| {
            a.masternode_probe = true;
        })
        .set_successfully_connected();
        net.attach(addr([51, 1, 1, 1], 19899), ConnectionType::OutboundFullRelay);
        let trace = net.dns_trace().await;
        if relevant == 2 {
            assert_eq!(trace, ["sleep 11s", "done"]);
            for _ in 0..3 {
                let p = net.core.dial_tick(DialLoop::DnsSeed, &Probe::default());
                assert!(matches!(p, DialPlan::Done), "finished for good (n2)");
            }
        } else {
            let r = resolves(&trace);
            let s11 = "sleep 11s".to_owned();
            assert_eq!(
                trace,
                [&s11, &r[0], &r[1], &r[2], &s11, &r[3], &r[4], "done"].map(|s| s.to_owned())
            );
        }
    }
}

/// A network made inactive holds the query in 1 s steps until it is active again.
#[tokio::test]
async fn dnsseed_waits_for_the_network() {
    let net = Net::new(with_dns_seeds(), |_| {});
    net.core.set_network_active(false);
    let tick = || plan(net.core.dial_tick(DialLoop::DnsSeed, &Probe::default()));
    for _ in 0..3 {
        assert_eq!(tick(), "sleep 1s");
    }
    net.core.set_network_active(true);
    assert!(tick().starts_with("resolve"));
}

/// `resolve_seed`: the answers are added to AddrMan with the default port and `NODE_NETWORK`;
/// none queues the bare seed as ADDR_FETCH.
#[tokio::test]
async fn dnsseed_resolve_adds_or_fetches() {
    let mut answers = Answers::default();
    answers.answers.insert(
        "x1.s1.example.".into(),
        vec![NetAddr::ipv4([1, 2, 3, 4]), NetAddr::ipv4([5, 6, 7, 8])],
    );
    let net = Net::resolving(with_dns_seeds(), answers, |_| {});
    assert_eq!(net.core.resolve_seed("s1.example.", 32).await, 2);
    let entry =
        net.core.addrman().get_address_info(&Service::new(NetAddr::ipv4([1, 2, 3, 4]), 19899));
    assert_eq!(entry.unwrap().services, ServiceFlags::NODE_NETWORK);
    assert_eq!(net.core.resolve_seed("s2.example.", 32).await, 0);
    assert_eq!(net.core.dial.lock().addr_fetches, ["s2.example."]);
}
