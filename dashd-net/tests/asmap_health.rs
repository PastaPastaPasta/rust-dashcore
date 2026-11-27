//! `ASMapHealthCheck` runs at start and then every `asmap_health_check_interval`, and the
//! schedule ends at stop. Its own test binary, because the check logs from the runtime's
//! blocking pool, which only a global subscriber sees.

use std::fmt;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_p2p::netgroup::NetGroupManager;
use dash_p2p::store::NoStore;
use dash_p2p::{DialPolicy, NetConfig, NetCore, NetEvents, PeerHandle, ServiceFlags};
use dash_params::{ChainParamsBuilder, Network, Release};
use dashd_net::{ConnMan, ConnManOptions, SystemResolver};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, span};

const HEALTH: &str = "ASMap Health Check: ";

/// The health-check lines as (level, message).
static LINES: Mutex<Vec<(Level, String)>> = Mutex::new(Vec::new());

struct HealthLines;

impl tracing::Subscriber for HealthLines {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, e: &Event<'_>) {
        struct Message(String);
        impl Visit for Message {
            fn record_debug(&mut self, f: &Field, v: &dyn fmt::Debug) {
                if f.name() == "message" {
                    self.0 = format!("{v:?}");
                }
            }
        }
        let mut m = Message(String::new());
        e.record(&mut m);
        if m.0.starts_with(HEALTH) {
            LINES.lock().unwrap().push((*e.metadata().level(), m.0));
        }
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

struct Quiet;

impl NetEvents for Quiet {
    fn wake(&self) {}
    fn initialize(&self, _: &NetCore, _: &PeerHandle, _: ServiceFlags) {}
    fn finalize(&self, _: &NetCore, _: &PeerHandle) {}
}

struct NoPolicy;

impl DialPolicy for NoPolicy {}

// Core: net.cpp:4136-4140, netgroup.cpp:114-128
#[test]
fn the_asmap_health_check_repeats_until_stop() {
    tracing::subscriber::set_global_default(HealthLines).unwrap();
    let rt =
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.listen = false;
    cfg.discover = false;
    cfg.dns_seed = false;
    cfg.fixed_seeds = false;
    cfg.connect = Some(Vec::new());
    cfg.connect_listed = true;
    cfg.asmap =
        NetGroupManager::decode_asmap(include_bytes!("../../dash-p2p/tests/data/asmap.raw"));
    assert!(!cfg.asmap.is_empty());
    let core = NetCore::new(
        cfg,
        rt.handle().clone(),
        Arc::new(NoStore),
        Arc::new(SystemResolver::default()),
        Arc::new(dash_clock::SystemClock),
        Arc::new(Quiet),
    )
    .unwrap();
    let opts = ConnManOptions {
        asmap_health_check_interval: Duration::from_millis(50),
        open_filter: Some(Arc::new(|_| false)),
        ..ConnManOptions::default()
    };
    let cm =
        ConnMan::start(Arc::clone(&core), rt.handle().clone(), Arc::new(NoPolicy), opts).unwrap();

    let deadline = Instant::now() + Duration::from_secs(120);
    while LINES.lock().unwrap().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let lines = LINES.lock().unwrap().clone();
    assert!(lines.len() >= 2, "{lines:?}");
    let want =
        "ASMap Health Check: 0 clearnet peers are mapped to 0 ASNs with 0 peers being unmapped";
    assert!(lines.iter().all(|(level, m)| *level == Level::INFO && m == want), "{lines:?}");

    cm.interrupt();
    let (done, stopped) = mpsc::channel();
    let stopper = std::thread::spawn(move || {
        cm.stop_threads();
        done.send(()).unwrap();
    });
    stopped.recv_timeout(Duration::from_secs(120)).expect("stop_threads returns");
    stopper.join().unwrap();
    rt.block_on(core.shutdown());
}
