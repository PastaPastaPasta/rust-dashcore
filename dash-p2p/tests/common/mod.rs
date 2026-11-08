//! Helpers shared by the disconnect, interrupt, grant and task-panic tests.
#![allow(dead_code)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::conn::{ConnectionType, Permissions};
use dash_p2p::store::{NetStore, NoStore};
use dash_p2p::{
    Address, Attach, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, PeerHandle, Resolver,
    Service, ServiceFlags,
};
use dash_params::{ChainParamsBuilder, Network, Release};

/// Core's `SELECT_TIMEOUT_MILLISECONDS`, the sweep's idle period.
pub const PASS: Duration = Duration::from_millis(500);

pub struct Events;

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
}

pub struct NoResolver;

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

/// What a test core is built from; `Default` is a regtest core with no store and no events.
pub struct Parts {
    pub store: Arc<dyn NetStore>,
    pub clock: Arc<MockClock>,
    pub events: Arc<dyn NetEvents>,
}

impl Default for Parts {
    fn default() -> Self {
        Self {
            store: Arc::new(NoStore),
            clock: Arc::new(MockClock::new()),
            events: Arc::new(Events),
        }
    }
}

/// A deterministic regtest core with inactivity checks an hour out.
pub fn core_with(parts: Parts, f: impl FnOnce(&mut NetConfig)) -> Arc<NetCore> {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    cfg.peer_timeout = 3600;
    f(&mut cfg);
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        parts.store,
        Arc::new(NoResolver),
        parts.clock,
        parts.events,
    )
    .unwrap()
}

pub fn core() -> Arc<NetCore> {
    core_with(Parts::default(), |_| {})
}

/// An outbound full-relay V1 attach of `sock` from `ip`:19899; `f` adjusts it.
pub fn attach_with(
    core: &NetCore,
    sock: MemSocket,
    ip: [u8; 4],
    f: impl FnOnce(&mut Attach),
) -> PeerHandle {
    let mut a = Attach {
        sock: Arc::new(sock),
        addr: Address {
            service: Service {
                addr: NetAddr::ipv4(ip),
                port: 19899,
            },
            services: ServiceFlags::NODE_NETWORK,
            time: 0,
        },
        addr_bind: Service::default(),
        dest: None,
        conn_type: ConnectionType::OutboundFullRelay,
        inbound_onion: false,
        permissions: Permissions::NONE,
        use_v2: false,
        grant: None,
        masternode: false,
        masternode_probe: false,
    };
    f(&mut a);
    core.attach(a)
}

pub fn attach(core: &NetCore, sock: MemSocket, ip: [u8; 4]) -> PeerHandle {
    attach_with(core, sock, ip, |_| {})
}

/// Runs every ready task without moving a paused clock.
pub async fn settle() {
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
}

/// Yields until `cond` holds, without moving a paused clock.
pub async fn settle_until(mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..2000 {
        if cond() {
            return true;
        }
        tokio::task::yield_now().await;
    }
    cond()
}

/// Lets the (paused) clock run until `cond` holds; sweep passes run as time moves. Bounded by
/// real time: the linger deadline is on the real steady clock (`MockClock::steady`).
pub async fn run_until(mut cond: impl FnMut() -> bool) {
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached in 10 s");
}
