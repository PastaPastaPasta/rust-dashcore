//! `sim` at Core's default 125 connections: two cores joined by 125 in-memory sockets (one side
//! inbound, the other outbound, half of them V2), traffic both ways on every connection,
//! disconnects from both sides, then shutdown. Checks the connection table's bookkeeping:
//! every peer is initialized once before it is listed and finalized once after it leaves,
//! nothing is lost or reordered per connection, and the sweep removes what was marked.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::conn::{ConnectionType, Permissions, Why};
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, CommandString, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, OutMsg,
    PeerHandle, PeerId, Resolver, Service, ServiceFlags,
};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;

const PEERS: usize = 125;
const MSGS: usize = 40;
const PING: CommandString = CommandString::from_static("ping");

#[derive(Default)]
struct Events {
    /// Per peer: (initialized, finalized, listed at initialize).
    seen: Mutex<HashMap<PeerId, (u32, u32, bool)>>,
}

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, core: &NetCore, peer: &PeerHandle, _our_services: ServiceFlags) {
        let listed = core.peer(peer.id()).is_some();
        let mut seen = self.seen.lock();
        let e = seen.entry(peer.id()).or_default();
        e.0 += 1;
        e.2 |= listed;
    }

    fn finalize(&self, core: &NetCore, peer: &PeerHandle) {
        assert!(core.peer(peer.id()).is_none(), "finalized while listed");
        self.seen.lock().entry(peer.id()).or_default().1 += 1;
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

fn core(events: Arc<Events>) -> Arc<NetCore> {
    let params =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    cfg.peer_timeout = 3600;
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        Arc::new(MockClock::new()),
        events,
    )
    .unwrap()
}

fn attach(core: &NetCore, sock: MemSocket, i: usize, inbound: bool) -> PeerHandle {
    core.attach(Attach {
        sock: Arc::new(sock),
        addr: Address {
            service: Service {
                addr: NetAddr::ipv4([10, (i / 250) as u8, (i % 250) as u8, 1 + u8::from(inbound)]),
                port: 19899,
            },
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type: if inbound {
            ConnectionType::Inbound
        } else {
            ConnectionType::OutboundFullRelay
        },
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: i.is_multiple_of(2),
        grant: None,
        masternode: false,
        masternode_probe: false,
    })
}

async fn until(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..10_000 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("{what}: not reached in 10 s");
}

/// Drains every message `core` has for `peers`, keyed by the sender's tag in the payload.
fn drain(core: &NetCore, peers: &[PeerHandle], got: &mut HashMap<PeerId, Vec<u32>>) {
    for p in peers {
        while let Some(m) = core.pop(p) {
            assert_eq!(m.msg.cmd, PING);
            got.entry(p.id())
                .or_default()
                .push(u32::from_le_bytes(m.msg.payload[..4].try_into().unwrap()));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sim_125_peers() {
    let (ea, eb) = (Arc::new(Events::default()), Arc::new(Events::default()));
    let (a, b) = (core(ea.clone()), core(eb.clone()));
    let mut out = Vec::new();
    let mut inb = Vec::new();
    for i in 0..PEERS {
        let (sa, sb) = MemSocket::pair();
        out.push(attach(&a, sa, i, false));
        inb.push(attach(&b, sb, i, true));
    }
    assert_eq!(a.peers().len(), PEERS);
    assert_eq!(b.peers().len(), PEERS);

    // Traffic both ways on every connection, from several threads at once.
    let senders: Vec<_> = [(a.clone(), out.clone()), (b.clone(), inb.clone())]
        .into_iter()
        .map(|(core, peers)| {
            std::thread::spawn(move || {
                for n in 0..MSGS as u32 {
                    for p in &peers {
                        let mut payload = n.to_le_bytes().to_vec();
                        payload.resize(8 + (n as usize * 37) % 300, 0xAB);
                        core.push(p, OutMsg::new(PING, payload));
                    }
                }
            })
        })
        .collect();
    for s in senders {
        s.join().unwrap();
    }
    let (mut got_a, mut got_b) = (HashMap::new(), HashMap::new());
    until("all messages delivered", || {
        drain(&a, &out, &mut got_a);
        drain(&b, &inb, &mut got_b);
        got_a.values().map(Vec::len).sum::<usize>() == PEERS * MSGS
            && got_b.values().map(Vec::len).sum::<usize>() == PEERS * MSGS
    })
    .await;
    let in_order: Vec<u32> = (0..MSGS as u32).collect();
    assert!(got_a.values().chain(got_b.values()).all(|v| *v == in_order));

    // Disconnect a third from each side: the sweep lingers, removes, and the far side sees the
    // close and removes its end too.
    for p in out.iter().step_by(3) {
        a.disconnect(p.id(), Why::Processor("sim"));
    }
    for p in inb.iter().skip(1).step_by(3) {
        b.disconnect(p.id(), Why::Processor("sim"));
    }
    let left = PEERS - PEERS.div_ceil(3) - (PEERS - 1).div_ceil(3);
    until("disconnects swept", || a.peers().len() == left && b.peers().len() == left).await;

    // The survivors still carry traffic.
    let survivors: Vec<PeerHandle> = a.peers().iter().cloned().collect();
    for p in &survivors {
        a.push(p, OutMsg::new(PING, 999u32.to_le_bytes().to_vec()));
    }
    let mut late = HashMap::new();
    until("survivor traffic", || {
        drain(&b, &b.peers(), &mut late);
        late.values().map(Vec::len).sum::<usize>() == left
    })
    .await;

    a.shutdown().await;
    b.shutdown().await;
    assert!(a.peers().is_empty() && b.peers().is_empty());
    for (name, events) in [("a", &ea), ("b", &eb)] {
        let seen = events.seen.lock();
        assert_eq!(seen.len(), PEERS, "{name}");
        for (id, &(init, fin, listed)) in seen.iter() {
            assert_eq!((init, fin, listed), (1, 1, false), "{name} peer={id}");
        }
    }
}
