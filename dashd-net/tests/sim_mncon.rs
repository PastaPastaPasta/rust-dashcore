//! The masternode loop (BL-705; Core's `ThreadOpenMasternodeConnections`, net.cpp:3414-3600 at
//! v24.0.0) in the dial `sim`: the pending queue, the quorum members and the probes against a
//! hand-set [`FakeMnPolicy`], under the virtual clock. Each test checks the opens, the meta
//! stamps and counts, and the next sleep.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_p2p::conn::Why;
use dash_p2p::rng::StreamRand;
use dash_p2p::{
    Address, Attach, ConnectionType, DialLoop, MemSocket, NetAddr, NetConfig, OpenError,
    PeerHandle, Permissions, Service, ServiceFlags, Target,
};
use dash_params::{ChainParamsBuilder, ChainSetup, Network as CoreNetwork, Release};
use dashd_net::sim::{
    DialEvent, DialSim, FakeMeta, FakeMnPolicy, OpenSummary, SIM_START, SimPolicy, full_node,
};

/// Mainnet's `nLLMQConnectionRetryTimeout` (chainparams.cpp:297).
const RETRY: i64 = 60;
/// The mock time of the first pass, one idle sleep after the start.
const T0: i64 = SIM_START + 1;
const IDLE: Duration = Duration::from_millis(1000);
const AFTER_CONNECT: Duration = Duration::from_millis(100);

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(CoreNetwork::Main, Release::V24_0).build().unwrap())
}

fn h(n: u8) -> [u8; 32] {
    [n; 32]
}

/// Core's `uint256::ToString` of [`h`]`(n)`.
fn hex(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn svc(n: u8) -> Service {
    Service::new(NetAddr::ipv4([31, 9, 0, n]), 9999)
}

/// The `net|netconn` lines on the test's thread, which runs the loop's ticks. Every test here
/// installs one, so a callsite's cached interest is never "never".
struct NetconnLines(Mutex<Vec<String>>);

impl tracing::Subscriber for NetconnLines {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && meta.target() == "net|netconn"
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
        self.0.lock().unwrap().push(line);
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// A mainnet sim with no seeds, its masternode policy, the far ends of the connections a test
/// attached (kept open, so the core never sees them close) and the loop's log lines.
struct Mn {
    sim: DialSim,
    mn: FakeMnPolicy,
    far: Vec<MemSocket>,
    lines: Arc<NetconnLines>,
    _log: tracing::subscriber::DefaultGuard,
}

impl Mn {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(f: impl FnOnce(&mut NetConfig)) -> Self {
        let lines = Arc::new(NetconnLines(Mutex::default()));
        let log = tracing::subscriber::set_default(Arc::clone(&lines));
        let sim = DialSim::new(params(), |cfg| {
            cfg.dns_seed = false;
            cfg.fixed_seeds = false;
            f(cfg);
        });
        Self {
            sim,
            mn: FakeMnPolicy::default(),
            far: Vec::new(),
            lines,
            _log: log,
        }
    }

    /// A core whose every random draw comes from the stream keyed by `seed`.
    fn seeded(seed: u64) -> Self {
        Self::with(|cfg| {
            let base = std::mem::replace(cfg, NetConfig::new(params(), ServiceFlags::NONE));
            *cfg = base.with_rand_source(Arc::new(StreamRand::from_seed(seed)));
        })
    }

    /// Performs the loop's next plan; returns what it did.
    fn step(&mut self) -> Vec<DialEvent> {
        let n = self.sim.exec.events.len();
        self.sim.run(DialLoop::MnCon, &self.mn, 1);
        self.sim.exec.events[n..].to_vec()
    }

    /// Runs the loop until the virtual clock has moved by `d`.
    fn run_for(&mut self, d: Duration) {
        self.sim.run_for(DialLoop::MnCon, &self.mn, d);
    }

    /// Puts masternode `n` on the tip list at [`svc`]`(n)`.
    fn mns(&self, ns: &[u8]) {
        for &n in ns {
            self.mn.set_mn(h(n), svc(n), true);
        }
    }

    /// Attaches a connection at `service`; `complete` sets `fSuccessfullyConnected`.
    fn attach(
        &mut self,
        service: Service,
        conn_type: ConnectionType,
        masternode: bool,
        complete: bool,
    ) -> PeerHandle {
        let (ours, theirs) = MemSocket::pair();
        self.far.push(theirs);
        let peer = self.sim.core.attach(Attach {
            sock: Arc::new(ours),
            addr: Address {
                service,
                services: ServiceFlags::NODE_NETWORK,
                time: 0,
            },
            addr_bind: Service::default(),
            dest: None,
            conn_type,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: false,
            grant: None,
            masternode,
            masternode_probe: false,
        });
        if complete {
            peer.set_successfully_connected_for_tests();
        }
        peer
    }

    /// A fully connected leg verified as masternode `n`.
    fn verified_leg(&mut self, service: Service, conn_type: ConnectionType, n: u8) -> PeerHandle {
        let p = self.attach(service, conn_type, false, true);
        assert!(self.sim.core.try_mark_verified(p.id(), h(n), [0xee; 32]));
        p
    }

    fn requests(&self) -> &[OpenSummary] {
        &self.sim.exec.requests
    }

    /// The masternode each open went to, and whether it was a probe.
    fn targets(&self) -> Vec<(Service, bool)> {
        self.requests()
            .iter()
            .map(|r| match &r.target {
                Target::Addr(a) => (a.service.clone(), r.masternode_probe),
                Target::Dest(d) => panic!("a masternode open by name: {d}"),
            })
            .collect()
    }

    fn logged(&self, line: &str) -> usize {
        self.lines.0.lock().unwrap().iter().filter(|l| *l == line).count()
    }

    fn now(&self) -> i64 {
        dash_p2p::Clock::node_now(&*self.sim.clock).0
    }
}

fn opened(service: &Service) -> impl Fn(&DialEvent) -> bool + '_ {
    move |e| matches!(e, DialEvent::Opened { service: s, conn_type: ConnectionType::OutboundFullRelay, .. } if s == service)
}

fn refused(service: &Service, err: OpenError) -> DialEvent {
    DialEvent::Refused {
        target: Target::Addr(Address {
            service: service.clone(),
            services: ServiceFlags::NODE_NETWORK,
            time: 100_000_000,
        }),
        conn_type: ConnectionType::OutboundFullRelay,
        err,
    }
}

fn failed_line(n: u8) -> String {
    format!(
        "CConnman::ThreadOpenMasternodeConnections -- connection failed for masternode  {}, service={}",
        hex(n),
        svc(n)
    )
}

fn meta(last_attempt: i64, last_success: i64, count: i32) -> FakeMeta {
    FakeMeta {
        last_attempt,
        last_success,
        count,
    }
}

/// Test 1: pending masternodes open first in first out (a duplicate add is refused), each a
/// masternode connection of type OUTBOUND_FULL_RELAY with no slot, no failure count and the
/// transport we advertise, stamped once; 100 ms after each open, 1 s once idle.
#[test]
fn t1_pending_masternodes_open_in_order() {
    let mut t = Mn::new();
    t.mns(&[1, 2]);
    assert!(t.sim.core.add_pending_masternode(h(1)));
    assert!(t.sim.core.add_pending_masternode(h(2)));
    assert!(!t.sim.core.add_pending_masternode(h(1)), "already queued");
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    for n in [1, 2] {
        let ev = t.step();
        assert!(ev.len() == 1 && opened(&svc(n))(&ev[0]), "{ev:?}");
        let DialEvent::Opened {
            id,
            ..
        } = ev[0]
        else {
            unreachable!()
        };
        let peer = t.sim.core.peer(id).unwrap();
        assert!(peer.masternode_connection() && !peer.masternode_probe());
        assert_eq!(t.mn.meta(&h(n)), meta(T0, 0, 1));
        assert_eq!(t.step(), [DialEvent::Slept(AFTER_CONNECT)]);
    }
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    let v2 = t.sim.core.we_advertise_v2();
    for (r, n) in t.requests().iter().zip([1, 2]) {
        assert_eq!(
            *r,
            OpenSummary {
                target: Target::Addr(Address {
                    service: svc(n),
                    services: ServiceFlags::NODE_NETWORK,
                    time: 100_000_000,
                }),
                conn_type: ConnectionType::OutboundFullRelay,
                count_failure: false,
                grant: false,
                use_v2: v2,
                masternode: true,
                masternode_probe: false,
            }
        );
    }
    assert_eq!(t.requests().len(), 2);
}

/// Test 2a: a fully connected ordinary peer at the pending masternode's address: popped, no
/// stamp, no open, 1 s.
#[test]
fn t2a_pending_at_a_connected_address_is_dropped() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.attach(svc(1), ConnectionType::OutboundFullRelay, false, true);
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(t.requests().is_empty());
    assert_eq!(t.mn.meta(&h(1)), FakeMeta::default());
    assert!(t.sim.core.add_pending_masternode(h(1)), "popped");
}

/// Test 2b: an incomplete ordinary peer there is not in the fully connected set, so the pass
/// stamps the masternode (count 1) and opens; the open is suppressed by that socket, the
/// check after it finds the address connected and logs no failure, and the next pass is
/// 100 ms later.
#[test]
fn t2b_pending_at_an_incomplete_peer_is_stamped_and_suppressed() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.attach(svc(1), ConnectionType::OutboundFullRelay, false, false);
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    assert_eq!(t.step(), [refused(&svc(1), OpenError::AlreadyConnected)]);
    assert_eq!(t.mn.meta(&h(1)), meta(T0, 0, 1));
    assert_eq!(t.step(), [DialEvent::Slept(AFTER_CONNECT)]);
    assert_eq!(t.logged(&failed_line(1)), 0);
    assert_eq!(t.mn.too_many_queries.load(Ordering::SeqCst), 0);
}

/// Test 2c: a masternode connection at the address: popped, no stamp, 1 s.
#[test]
fn t2c_pending_at_a_masternode_connection_is_popped() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.attach(svc(1), ConnectionType::OutboundFullRelay, true, false);
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(t.requests().is_empty());
    assert_eq!(t.mn.meta(&h(1)), FakeMeta::default());
    assert!(t.sim.core.add_pending_masternode(h(1)), "popped");
}

/// Test 2d: a peer marked for disconnection at the address: popped, no stamp, 1 s.
#[test]
fn t2d_pending_at_a_disconnecting_peer_is_popped() {
    let mut t = Mn::new();
    t.mns(&[1]);
    let p = t.attach(svc(1), ConnectionType::OutboundFullRelay, false, true);
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    // Marked after the sleep, so its linger (virtual time) is still running at the pass.
    t.sim.core.disconnect(p.id(), Why::Processor("test"));
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(t.requests().is_empty());
    assert_eq!(t.mn.meta(&h(1)), FakeMeta::default());
    assert!(t.sim.core.add_pending_masternode(h(1)), "popped");
}

/// Test 3: in one pass a pending masternode comes before a quorum member, which comes before a
/// probe.
#[test]
fn t3_pending_then_quorum_then_probe() {
    let mut t = Mn::new();
    t.mns(&[1, 2, 3]);
    t.sim.core.add_pending_masternode(h(1));
    t.mn.set_groups(vec![vec![h(2)]]);
    t.sim.core.add_pending_probes(&[h(3)]);
    for _ in 0..7 {
        t.step();
    }
    assert_eq!(t.targets(), [(svc(1), false), (svc(2), false), (svc(3), true)]);
}

/// Test 4: a quorum member is skipped while `now - last_attempt` is below the retry timeout
/// and opened at exactly the timeout.
#[test]
fn t4_quorum_back_off_ends_at_the_timeout() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.mn.set_groups(vec![vec![h(1)]]);
    t.mn.set_meta(h(1), meta(T0 - (RETRY - 1), 0, 3));
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)], "timeout - 1");
    assert!(t.requests().is_empty());
    assert_eq!(t.now(), T0 + 1);
    let ev = t.step();
    assert!(ev.len() == 1 && opened(&svc(1))(&ev[0]), "timeout: {ev:?}");
    assert_eq!(t.mn.meta(&h(1)), meta(T0 + 1, 0, 4));
}

/// Test 5a: a probe opens with the probe flag and leaves the queue: it is not probed again
/// once its back-off has passed.
#[test]
fn t5a_a_probe_opens_flagged_and_leaves_the_queue() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    let ev = t.step();
    let DialEvent::Opened {
        id,
        ..
    } = ev[0]
    else {
        panic!("{ev:?}")
    };
    assert!(t.sim.core.peer(id).unwrap().masternode_probe());
    t.run_for(Duration::from_secs(3 * RETRY as u64));
    assert_eq!(t.targets(), [(svc(1), true)]);
    assert_eq!(t.mn.meta(&h(1)), meta(T0, 0, 1));
}

/// Test 5b: a probe off the tip list is erased.
#[test]
fn t5b_a_probe_off_the_list_is_erased() {
    let mut t = Mn::new();
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    t.mns(&[1]);
    t.run_for(Duration::from_secs(3 * RETRY as u64));
    assert!(t.requests().is_empty());
}

/// Test 5c: a probe with only an outbound verified leg is erased with a success stamp, which
/// resets its count, and not opened.
#[test]
fn t5c_an_outbound_verified_probe_is_a_success() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.verified_leg(svc(1), ConnectionType::OutboundFullRelay, 1);
    t.mn.set_meta(h(1), meta(5, 0, 3));
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert_eq!(t.mn.meta(&h(1)), meta(5, T0, 0));
    t.run_for(Duration::from_secs(3 * RETRY as u64));
    assert!(t.requests().is_empty());
}

/// Test 5d: an inbound verified leg first in the table, then an outbound one: the first leg
/// sets the direction, so the probe is kept and opened.
#[test]
fn t5d_the_first_leg_inbound_keeps_the_probe() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.verified_leg(svc(51), ConnectionType::Inbound, 1);
    t.verified_leg(svc(1), ConnectionType::OutboundFullRelay, 1);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    assert_eq!(t.step(), [refused(&svc(1), OpenError::AlreadyConnected)]);
    assert_eq!(t.targets(), [(svc(1), true)]);
    assert_eq!(t.mn.meta(&h(1)), meta(T0, 0, 1));
}

/// Test 5e: an outbound verified leg first, then an inbound one: the probe is a success.
#[test]
fn t5e_the_first_leg_outbound_is_a_success() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.verified_leg(svc(1), ConnectionType::OutboundFullRelay, 1);
    t.verified_leg(svc(51), ConnectionType::Inbound, 1);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(t.requests().is_empty());
    assert_eq!(t.mn.meta(&h(1)), meta(0, T0, 0));
}

/// Test 5f: an outbound verified leg marked for disconnection, then an inbound one: the marked
/// leg is not in the snapshot, so the inbound leg sets the direction and the probe opens.
#[test]
fn t5f_a_disconnecting_outbound_leg_is_not_counted() {
    let mut t = Mn::new();
    t.mns(&[1]);
    let out = t.verified_leg(svc(1), ConnectionType::OutboundFullRelay, 1);
    t.verified_leg(svc(51), ConnectionType::Inbound, 1);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    t.sim.core.disconnect(out.id(), Why::Processor("test"));
    t.step();
    assert_eq!(t.targets(), [(svc(1), true)]);
    assert_eq!(t.mn.meta(&h(1)), meta(T0, 0, 1));
}

/// Test 5g: a probe's back-off, as test 4.
#[test]
fn t5g_probe_back_off_ends_at_the_timeout() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.mn.set_meta(h(1), meta(T0 - (RETRY - 1), 0, 0));
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)], "timeout - 1");
    assert!(t.requests().is_empty());
    t.step();
    assert_eq!(t.targets(), [(svc(1), true)], "timeout");
    assert_eq!(t.mn.meta(&h(1)), meta(T0 + 1, 0, 1));
}

/// Test 6: a fully connected ordinary peer at a quorum member's address whose first message
/// came 6 s before the pass is dropped with Core's line; one 5 s ago, or with no first message,
/// is kept. Either way the member is not opened. An incomplete peer there is never checked: the
/// member is opened as in test 2b.
#[test]
fn t6_a_slow_handshake_at_a_quorum_address_is_dropped() {
    let drop_line = format!(
        "CConnman::ThreadOpenMasternodeConnections -- dropping non-mnauth connection to {}, service={}",
        hex(1),
        svc(1)
    );
    for (first, complete, dropped) in
        [(T0 - 5, true, false), (T0 - 6, true, true), (0, true, false), (T0 - 100, false, false)]
    {
        let mut t = Mn::new();
        t.mns(&[1]);
        t.mn.set_groups(vec![vec![h(1)]]);
        let p = t.attach(svc(1), ConnectionType::OutboundFullRelay, false, complete);
        p.set_first_message_for_tests(first, false);
        t.step();
        let ev = t.step();
        let case = format!("first {first}, complete {complete}");
        assert_eq!(p.disconnect_requested(), dropped, "{case}");
        assert_eq!(t.logged(&drop_line), usize::from(dropped), "{case}");
        if complete {
            assert_eq!(ev, [DialEvent::Slept(IDLE)], "{case}");
            assert_eq!(t.mn.meta(&h(1)), FakeMeta::default(), "{case}");
        } else {
            assert_eq!(ev, [refused(&svc(1), OpenError::AlreadyConnected)], "{case}");
            assert_eq!(t.mn.meta(&h(1)), meta(T0, 0, 1), "{case}");
        }
    }
}

/// Test 7: the first pass runs 1 s after the start and idle passes 1 s apart; the pass after
/// an open, a failed open or a suppressed one (test 2b) comes 100 ms later. An unsynced chain,
/// an inactive thread or an inactive network opens nothing and leaves the queue as it was;
/// `-connect` (any value), or no masternode policy, ends the loop.
#[test]
fn t7_cadence_and_gates() {
    let mut t = Mn::new();
    for _ in 0..3 {
        assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    }
    assert_eq!(t.now(), SIM_START + 3);

    let mut t = Mn::new();
    t.mns(&[1, 2]);
    t.sim.exec.unreachable.insert(svc(2));
    t.sim.core.add_pending_masternode(h(1));
    t.sim.core.add_pending_masternode(h(2));
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(opened(&svc(1))(&t.step()[0]));
    assert_eq!(t.mn.meta(&h(1)).last_attempt, SIM_START + 1);
    assert_eq!(t.step(), [DialEvent::Slept(AFTER_CONNECT)], "after an open");
    assert!(matches!(
        t.step()[..],
        [DialEvent::Refused {
            err: OpenError::Connect(_),
            ..
        }]
    ));
    assert_eq!(t.step(), [DialEvent::Slept(AFTER_CONNECT)], "after a failed open");
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);

    type Gate = fn(&Mn, bool);
    let gates: [(&str, Gate); 3] = [
        ("unsynced", |t, on| t.mn.synced.store(on, Ordering::SeqCst)),
        ("thread inactive", |t, on| t.sim.core.set_masternode_thread_active(on)),
        ("network inactive", |t, on| t.sim.core.set_network_active(on)),
    ];
    for (name, gate) in gates {
        let mut t = Mn::new();
        t.mns(&[1]);
        t.sim.core.add_pending_masternode(h(1));
        gate(&t, false);
        for _ in 0..4 {
            assert_eq!(t.step(), [DialEvent::Slept(IDLE)], "{name}");
        }
        assert!(t.requests().is_empty(), "{name}");
        assert!(!t.sim.core.add_pending_masternode(h(1)), "{name}: still queued");
        gate(&t, true);
        assert!(opened(&svc(1))(&t.step()[0]), "{name}: open once lifted");
    }
    assert!(Mn::new().sim.core.masternode_thread_active());

    // `-connect=<addr>` and `-connect=0` stop the loop; a bare `-noconnect` (no values) does not.
    for connect in [vec!["31.9.0.9".to_owned()], vec![]] {
        let mut t = Mn::with(|cfg| {
            cfg.connect = Some(connect.clone());
            cfg.connect_listed = true;
        });
        t.mns(&[1]);
        t.sim.core.add_pending_masternode(h(1));
        assert!(t.step().is_empty(), "-connect={connect:?}");
        assert!(t.step().is_empty());
        assert!(t.requests().is_empty());
    }
    let mut t = Mn::with(|cfg| cfg.connect = Some(Vec::new()));
    t.mns(&[1]);
    t.sim.core.add_pending_masternode(h(1));
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)], "-noconnect");
    assert!(opened(&svc(1))(&t.step()[0]), "-noconnect: the loop runs");

    let mut t = Mn::new();
    t.sim.core.add_pending_masternode(h(1));
    t.mns(&[1]);
    assert!(!t.sim.run(DialLoop::MnCon, &SimPolicy::default(), 10), "no policy: done");
    assert!(t.sim.exec.events.is_empty());
}

/// Test 8: a failed open does not count against the address in AddrMan (`fCountFailure` is
/// false: `nAttempts` stays, only `m_last_try` moves, though `m_last_count_attempt` is below
/// `nLastGood`), logs Core's failure line, and asks `OutboundFailedTooManyTimes` without adding
/// to the count: 1, then 2. Past the limit the second line follows.
#[test]
fn t8_a_failed_open_counts_once_in_meta_and_not_in_addrman() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.sim.add_addrs(&[full_node(svc(1), SIM_START)], &NetAddr::ipv4([250, 1, 1, 1]));
    let before = t.sim.core.addrman().get_address_info(&svc(1)).expect("in AddrMan");
    assert_eq!((before.attempts, before.last_count_attempt, before.last_try), (0, 0, 0));
    t.sim.exec.unreachable.insert(svc(1));
    let too_many = format!(
        "CConnman::ThreadOpenMasternodeConnections -- failed to connect to masternode {} too many times",
        hex(1)
    );
    t.step();
    for round in 1..=2 {
        assert!(t.sim.core.add_pending_masternode(h(1)));
        assert!(matches!(
            t.step()[..],
            [DialEvent::Refused {
                err: OpenError::Connect(_),
                ..
            }]
        ));
        let now = t.now();
        assert_eq!(t.step(), [DialEvent::Slept(AFTER_CONNECT)]);
        assert_eq!(t.mn.meta(&h(1)), meta(now, 0, round), "round {round}");
        assert_eq!(t.mn.too_many_queries.load(Ordering::SeqCst), round as usize);
        assert_eq!(t.logged(&failed_line(1)), round as usize);
        let entry = t.sim.core.addrman().get_address_info(&svc(1)).unwrap();
        assert_eq!((entry.attempts, entry.last_try), (0, now), "round {round}");
    }
    assert!(t.requests().iter().all(|r| !r.count_failure));
    assert_eq!(t.logged(&too_many), 0);

    t.mn.set_meta(h(1), meta(0, 0, 5));
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    t.step();
    t.step();
    assert_eq!(t.mn.meta(&h(1)).count, 6);
    assert_eq!(t.logged(&too_many), 1);
}

/// The masternode the first open of a fresh seeded core goes to, with `setup` run first.
fn first_pick(seed: u64, setup: impl FnOnce(&Mn)) -> Service {
    let mut t = Mn::seeded(seed);
    setup(&t);
    t.step();
    t.step();
    t.targets().first().expect("an open").0.clone()
}

/// Test 9: the pick depends on the node's RNG, not on insertion order: one seed picks the same
/// masternode from the probes and from a quorum group whatever order they were added in, and
/// two seeds probe a 4-candidate set in different orders.
#[test]
fn t9_the_pick_is_salted_not_insertion_ordered() {
    let fwd: Vec<[u8; 32]> = (1..=4).map(h).collect();
    let rev: Vec<[u8; 32]> = fwd.iter().rev().copied().collect();
    for seed in [1, 2, 3] {
        let probes = |order: &[[u8; 32]]| {
            let order = order.to_vec();
            first_pick(seed, move |t| {
                t.mns(&[1, 2, 3, 4]);
                t.sim.core.add_pending_probes(&order);
            })
        };
        assert_eq!(probes(&fwd), probes(&rev), "probes, seed {seed}");
        let group = |order: &[[u8; 32]]| {
            let order = order.to_vec();
            first_pick(seed, move |t| {
                t.mns(&[1, 2, 3, 4]);
                t.mn.set_groups(vec![order]);
            })
        };
        assert_eq!(group(&fwd), group(&rev), "quorum, seed {seed}");
    }

    let order = |seed| {
        let mut t = Mn::seeded(seed);
        t.mns(&[1, 2, 3, 4]);
        t.sim.core.add_pending_probes(&fwd);
        for _ in 0..9 {
            t.step();
        }
        t.targets().into_iter().map(|(s, _)| s).collect::<Vec<_>>()
    };
    let (a, b) = (order(1), order(2));
    assert_eq!(a.len(), 4);
    assert_ne!(a, b);
}

/// DEC-1316 (1): the quorum candidates are the groups concatenated with no dedup across
/// groups, and every check runs per occurrence. A member of two groups is looked up and its
/// back-off read twice. At a slow peer, its first occurrence drops the peer (one line) and its
/// second finds the peer marked and skips it, as Core's loop does.
#[test]
fn dec1316_a_member_of_two_groups_is_two_candidates() {
    let mut t = Mn::new();
    t.mns(&[1, 2]);
    t.mn.set_groups(vec![vec![h(1), h(2)], vec![h(1)]]);
    t.step();
    t.step();
    assert_eq!((t.mn.lookups(&h(1)), t.mn.attempt_reads(&h(1))), (2, 2));
    assert_eq!((t.mn.lookups(&h(2)), t.mn.attempt_reads(&h(2))), (1, 1));

    let mut t = Mn::new();
    t.mns(&[1]);
    t.mn.set_groups(vec![vec![h(1)], vec![h(1)]]);
    let p = t.attach(svc(1), ConnectionType::OutboundFullRelay, false, true);
    p.set_first_message_for_tests(T0 - 6, false);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(p.disconnect_requested());
    let line = format!(
        "CConnman::ThreadOpenMasternodeConnections -- dropping non-mnauth connection to {}, service={}",
        hex(1),
        svc(1)
    );
    assert_eq!(t.logged(&line), 1);
    assert_eq!((t.mn.lookups(&h(1)), t.mn.attempt_reads(&h(1))), (2, 0));
}

/// DEC-1316 (2): over 2,000 seeds, each quorum candidate is picked in proportion to its
/// multiplicity and each probe uniformly. Groups `[1, 2]` and `[1, 3]` make 1 a 2× entry
/// (p = 1/2, 2 and 3 p = 1/4); probes 11–14 have p = 1/4 each.
///
/// Tolerance: |frequency − p| ≤ 0.05. At n = 2,000 a pick count's standard deviation is
/// √(n·p·(1−p)) ≤ 22.4 (p = 1/2), so 0.05·n = 100 picks is at least 4.4σ for every target: a
/// correct loop fails it with a chance below 1e-5 per target. Picking uniformly over the unique
/// members would give 1 a frequency of 1/3, 0.17 away.
#[test]
fn dec1316_picks_follow_multiplicity() {
    const SEEDS: u64 = 2000;
    let mut quorum: BTreeMap<Service, u64> = BTreeMap::new();
    let mut probes: BTreeMap<Service, u64> = BTreeMap::new();
    for seed in 0..SEEDS {
        let mut t = Mn::seeded(seed);
        t.mns(&[1, 2, 3, 11, 12, 13, 14]);
        t.mn.set_groups(vec![vec![h(1), h(2)], vec![h(1), h(3)]]);
        t.sim.core.add_pending_probes(&[h(11), h(12), h(13), h(14)]);
        t.step();
        t.step();
        t.mn.set_groups(Vec::new());
        t.step();
        t.step();
        let picks = t.targets();
        assert_eq!(picks.len(), 2, "seed {seed}: {picks:?}");
        assert!(!picks[0].1 && picks[1].1, "seed {seed}: {picks:?}");
        *quorum.entry(picks[0].0.clone()).or_default() += 1;
        *probes.entry(picks[1].0.clone()).or_default() += 1;
    }
    let check = |counts: &BTreeMap<Service, u64>, want: &[(u8, f64)]| {
        assert_eq!(counts.len(), want.len(), "{counts:?}");
        for &(n, p) in want {
            let f = counts.get(&svc(n)).copied().unwrap_or(0) as f64 / SEEDS as f64;
            assert!((f - p).abs() <= 0.05, "masternode {n}: frequency {f}, want {p} ± 0.05");
        }
    };
    check(&quorum, &[(1, 0.5), (2, 0.25), (3, 0.25)]);
    check(&probes, &[(11, 0.25), (12, 0.25), (13, 0.25), (14, 0.25)]);
}

/// DEC-1316 (2), probes under one salt: across seeds a probe pick that ignored the RNG would
/// still look uniform, as the salt moves the order. So one node probes the same four masternodes
/// 2,000 times, the set refilled and the back-off cleared before each pass, and each is picked
/// with p = 1/4.
///
/// Tolerance: |frequency − 1/4| ≤ 0.05, as above: σ = √(2000·¼·¾) ≈ 19.4 picks, so 100 picks
/// is 5.2σ. A pick fixed to one salted rank would give one masternode a frequency of 1.
#[test]
fn dec1316_one_node_picks_each_probe_uniformly() {
    const PASSES: usize = 2000;
    let all = [h(11), h(12), h(13), h(14)];
    let mut t = Mn::seeded(7);
    t.mns(&[11, 12, 13, 14]);
    t.step();
    for _ in 0..PASSES {
        t.sim.core.add_pending_probes(&all);
        for x in all {
            t.mn.set_meta(x, meta(0, 0, 0));
        }
        t.step();
        t.step();
    }
    let picks = t.targets();
    assert_eq!(picks.len(), PASSES);
    let mut counts: BTreeMap<Service, usize> = BTreeMap::new();
    for (s, probe) in picks {
        assert!(probe);
        *counts.entry(s).or_default() += 1;
    }
    for n in 11..=14 {
        let f = counts.get(&svc(n)).copied().unwrap_or(0) as f64 / PASSES as f64;
        assert!((f - 0.25).abs() <= 0.05, "masternode {n}: frequency {f}, want 0.25 ± 0.05");
    }
}

/// A quorum member already verified on some leg (here inbound, from another address) is not
/// dialed (net.cpp:3464-3466): no open, no stamp.
#[test]
fn a_verified_quorum_member_is_not_dialed() {
    let mut t = Mn::new();
    t.mns(&[1]);
    t.mn.set_groups(vec![vec![h(1)]]);
    t.verified_leg(svc(51), ConnectionType::Inbound, 1);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(t.requests().is_empty());
    assert_eq!(t.mn.meta(&h(1)), FakeMeta::default());
}

/// A fully connected masternode connection at a quorum member's address, not yet verified and
/// quiet past the slow-handshake bound, is skipped before that check (net.cpp:3473-3476): kept,
/// no drop line, no open.
#[test]
fn a_masternode_connection_at_a_quorum_address_is_kept() {
    let drop_line = format!(
        "CConnman::ThreadOpenMasternodeConnections -- dropping non-mnauth connection to {}, service={}",
        hex(1),
        svc(1)
    );
    let mut t = Mn::new();
    t.mns(&[1]);
    t.mn.set_groups(vec![vec![h(1)]]);
    let p = t.attach(svc(1), ConnectionType::OutboundFullRelay, true, true);
    p.set_first_message_for_tests(T0 - 6, false);
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)]);
    assert!(!p.disconnect_requested());
    assert_eq!(t.logged(&drop_line), 0);
    assert!(t.requests().is_empty());
}

/// A PoSe-banned masternode: the pending queue reads `GetValidMN` (net.cpp:3539), so it is
/// popped with no stamp and no open; quorum members and probes read `GetMN` (net.cpp:3467,
/// :3508), so it is dialed there.
#[test]
fn a_pose_banned_masternode_is_dialed_only_as_a_quorum_member_or_probe() {
    let mut t = Mn::new();
    t.mn.set_mn(h(1), svc(1), false);
    t.sim.core.add_pending_masternode(h(1));
    t.step();
    assert_eq!(t.step(), [DialEvent::Slept(IDLE)], "pending");
    assert!(t.requests().is_empty(), "pending");
    assert_eq!(t.mn.meta(&h(1)), FakeMeta::default(), "pending");
    assert!(t.sim.core.add_pending_masternode(h(1)), "pending: popped");

    let mut t = Mn::new();
    t.mn.set_mn(h(1), svc(1), false);
    t.mn.set_groups(vec![vec![h(1)]]);
    t.step();
    assert!(opened(&svc(1))(&t.step()[0]), "quorum");
    assert_eq!(t.targets(), [(svc(1), false)], "quorum");

    let mut t = Mn::new();
    t.mn.set_mn(h(1), svc(1), false);
    t.sim.core.add_pending_probes(&[h(1)]);
    t.step();
    assert!(opened(&svc(1))(&t.step()[0]), "probe");
    assert_eq!(t.targets(), [(svc(1), true)], "probe");
}

/// `IsMasternodeOrDisconnectRequested` (net.cpp:4961-4966) reads the first connection at the
/// address, a disconnecting one included.
#[test]
fn is_masternode_or_disconnect_requested_reads_the_first_connection() {
    let mut t = Mn::new();
    assert!(!t.sim.core.is_masternode_or_disconnect_requested(&svc(1)), "none");
    t.attach(svc(1), ConnectionType::OutboundFullRelay, false, true);
    assert!(!t.sim.core.is_masternode_or_disconnect_requested(&svc(1)), "ordinary");
    t.attach(svc(2), ConnectionType::OutboundFullRelay, true, false);
    assert!(t.sim.core.is_masternode_or_disconnect_requested(&svc(2)), "masternode connection");
    let first = t.attach(svc(3), ConnectionType::OutboundFullRelay, false, true);
    t.attach(svc(3), ConnectionType::Inbound, false, true);
    t.sim.core.disconnect(first.id(), Why::Processor("test"));
    assert!(
        t.sim.core.is_masternode_or_disconnect_requested(&svc(3)),
        "a disconnecting first connection ahead of a live ordinary one"
    );
}
