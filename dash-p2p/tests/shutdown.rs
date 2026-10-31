//! `shutdown` returns only after every removal has finished, as Core's `Stop` joins its threads
//! (`StopThreads`) before `StopNodes` (v24.0.0-rc.3 `net.cpp:4168-4210`): a peer the sweep is
//! finalizing, an `attach` still in `initialize`, and a second `shutdown` racing the first.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::conn::{ConnectionType, Permissions};
use dash_p2p::store::NoStore;
use dash_p2p::{
    Address, Attach, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, PeerHandle, Resolver,
    Service, ServiceFlags,
};
use dash_params::{ChainParamsBuilder, Network, Release};
use parking_lot::Mutex;

const T0: i64 = 1_700_000_000;

/// A one-shot hold: the first call reports on `entered` and waits for `release`.
type Gate = Mutex<Option<(Sender<()>, Receiver<()>)>>;

fn hold(gate: &Gate) {
    if let Some((entered, release)) = gate.lock().take() {
        entered.send(()).unwrap();
        // On a runtime worker, hand its queue to the runtime, so only the hold is blocked.
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| release.recv().unwrap());
        } else {
            release.recv().unwrap();
        }
    }
}

/// Arms `gate`; returns its start signal and its release.
fn arm(gate: &Gate) -> (Receiver<()>, Sender<()>) {
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    *gate.lock() = Some((entered_tx, release_rx));
    (entered, release)
}

/// Holds the first `initialize` and the first `finalize` when armed.
#[derive(Default)]
struct Events {
    init_gate: Gate,
    final_gate: Gate,
    finalized: AtomicUsize,
}

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {
        hold(&self.init_gate);
    }

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {
        hold(&self.final_gate);
        self.finalized.fetch_add(1, Ordering::SeqCst);
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

fn setup(events: Arc<Events>) -> (Arc<NetCore>, Arc<MockClock>) {
    let clock = Arc::new(MockClock::at(T0));
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
    cfg.peer_timeout = 60;
    let core = NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        clock.clone(),
        events,
    )
    .unwrap();
    (core, clock)
}

fn outbound() -> Attach {
    let (ours, _theirs) = MemSocket::pair();
    Attach {
        sock: Arc::new(ours),
        addr: Address {
            service: Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19899),
            services: ServiceFlags::NODE_NETWORK,
            time: 1_700_000_000,
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
    }
}

async fn wait(entered: Receiver<()>, what: &str) {
    tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(30)))
        .await
        .unwrap()
        .unwrap_or_else(|_| panic!("{what}"));
}

fn shutdown_counting(core: &Arc<NetCore>, events: &Arc<Events>) -> tokio::task::JoinHandle<usize> {
    let (core, events) = (core.clone(), events.clone());
    tokio::spawn(async move {
        core.shutdown().await;
        events.finalized.load(Ordering::SeqCst)
    })
}

/// Pending for a while: the call has not returned early.
async fn pending(stop: &mut tokio::task::JoinHandle<usize>) {
    assert!(tokio::time::timeout(Duration::from_millis(300), stop).await.is_err());
}

/// The sweep has taken a peer that `InactivityCheck` dropped out of the table and is held in
/// its `finalize`: `shutdown`, and a second `shutdown` racing it, wait for it before removing
/// the peers still listed, so `finalize` never runs on two tasks at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_the_sweeps_finalize() {
    let events = Arc::new(Events::default());
    let (entered, release) = arm(&events.final_gate);
    let (core, clock) = setup(events.clone());
    let peer = core.attach(outbound());

    // Silent past -peertimeout: the sweep's InactivityCheck marks it, the linger ends, and the
    // sweep removes it and stops in `finalize`.
    clock.set_mocktime(T0 + 61);
    wait(entered, "the sweep finalizes the inactive peer").await;
    assert!(peer.disconnect_requested());
    assert!(core.peers().is_empty());
    let live = core.attach(outbound());

    let mut first = shutdown_counting(&core, &events);
    // With the table empty, a shutdown that does not join the sweep would return now.
    pending(&mut first).await;
    // A second call must not return while the first is still waiting.
    let mut second = shutdown_counting(&core, &events);
    pending(&mut second).await;
    assert_eq!(events.finalized.load(Ordering::SeqCst), 0, "the live peer waits for the sweep");
    assert_eq!(core.peers().len(), 1);
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap(), 2, "both finalizes had finished when shutdown returned");
    assert_eq!(second.await.unwrap(), 2, "and when the second call returned");
    // The peers' readers and writers have stopped and dropped their handles.
    assert_eq!(Arc::strong_count(&peer), 1);
    assert_eq!(Arc::strong_count(&live), 1);
}

/// An `attach` held in `initialize` while `shutdown` runs: it finds the core interrupted,
/// finalizes its peer, and `shutdown` returns only after that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_an_attach_in_flight() {
    let events = Arc::new(Events::default());
    let (entered, release) = arm(&events.init_gate);
    let (core, _clock) = setup(events.clone());
    let attach = std::thread::spawn({
        let core = core.clone();
        move || core.attach(outbound())
    });
    wait(entered, "attach reaches initialize").await;

    let mut stop = shutdown_counting(&core, &events);
    pending(&mut stop).await;
    release.send(()).unwrap();
    assert_eq!(stop.await.unwrap(), 1, "the refused attach had finalized its peer");
    let peer = attach.join().unwrap();
    assert!(core.peers().is_empty());
    assert_eq!(Arc::strong_count(&peer), 1, "no reader or writer was started");
}

/// A `shutdown` cancelled while it waits for the sweep (a host's timeout) leaves the sweep to the
/// next call, which still waits for it before removing the peers still listed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancelled_shutdown_leaves_the_sweep_to_the_next() {
    let events = Arc::new(Events::default());
    let (entered, release) = arm(&events.final_gate);
    let (core, clock) = setup(events.clone());
    let _inactive = core.attach(outbound());
    clock.set_mocktime(T0 + 61);
    wait(entered, "the sweep finalizes the inactive peer").await;
    let live = core.attach(outbound());

    let cancelled = tokio::time::timeout(Duration::from_millis(300), core.shutdown()).await;
    assert!(cancelled.is_err(), "the first call waits for the sweep");
    let mut next = shutdown_counting(&core, &events);
    pending(&mut next).await;
    assert_eq!(events.finalized.load(Ordering::SeqCst), 0, "the live peer waits for the sweep");
    assert_eq!(core.peers().len(), 1);
    release.send(()).unwrap();
    assert_eq!(next.await.unwrap(), 2);
    assert_eq!(Arc::strong_count(&live), 1);
}
