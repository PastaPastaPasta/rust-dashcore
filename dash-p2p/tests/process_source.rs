//! The process source (BL-502 test 13), in a binary of its own because the install is
//! process-wide: once a source is installed, `process_source()`, `FastRandom::new()`, AddrMan's
//! default and a NetCore's default all draw from it, and a second install leaves it in place.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_clock::MockClock;
use dash_p2p::addrman::AddrMan;
use dash_p2p::netgroup::NetGroupManager;
use dash_p2p::rng::{
    FastRandom, Installed, RandSource, StreamRand, install_process_source, process_source,
};
use dash_p2p::store::NoStore;
use dash_p2p::{NetAddr, NetConfig, NetCore, NetEvents, PeerHandle, Resolver, ServiceFlags};
use dash_params::{ChainParamsBuilder, Network, Release};

/// A stream that counts the bytes and events it is given.
struct Counting {
    stream: StreamRand,
    bytes: AtomicUsize,
    events: AtomicUsize,
}

impl RandSource for Counting {
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        self.bytes.fetch_add(out.len(), Ordering::SeqCst);
        self.stream.rand_bytes(out, strong);
    }

    fn add_event(&self, _info: u32) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }
}

impl Counting {
    fn bytes(&self) -> usize {
        self.bytes.load(Ordering::SeqCst)
    }
}

struct Events;

impl NetEvents for Events {
    fn wake(&self) {}

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {}
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

/// One test, so the steps run in order on the one install.
#[tokio::test]
async fn installed_source_serves_every_default() {
    static FIRST: std::sync::LazyLock<Counting> = std::sync::LazyLock::new(|| Counting {
        stream: StreamRand::from_seed(1),
        bytes: AtomicUsize::new(0),
        events: AtomicUsize::new(0),
    });
    static SECOND: std::sync::LazyLock<Counting> = std::sync::LazyLock::new(|| Counting {
        stream: StreamRand::from_seed(2),
        bytes: AtomicUsize::new(0),
        events: AtomicUsize::new(0),
    });
    install_process_source(&*FIRST).unwrap();

    let mut buf = [0u8; 7];
    process_source().rand_bytes(&mut buf, true);
    assert_eq!(FIRST.bytes(), 7);
    process_source().add_event(5);
    assert_eq!(FIRST.events.load(Ordering::SeqCst), 1);

    // FastRandom::new keys itself with 32 bytes from the source at its first draw.
    let mut rng = FastRandom::new();
    assert_eq!(FIRST.bytes(), 7);
    rng.rand64();
    assert_eq!(FIRST.bytes(), 7 + 32);

    // AddrMan's context draws at construction (its nKey and salts).
    let before = FIRST.bytes();
    let netgroup = NetGroupManager::new(Vec::new(), false);
    let _addrman = AddrMan::new(netgroup, false, 0, false, [0xfc, 0xc1, 0xb7, 0xdc]);
    assert!(FIRST.bytes() > before, "AddrMan::new drew nothing from the process source");

    // A core whose config names no source: init.cpp's nine draws, then `rand()` is the source.
    let before = FIRST.bytes();
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let core = NetCore::new(
        NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK),
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(NoResolver),
        Arc::new(MockClock::at(1_700_000_000)),
        Arc::new(Events),
    )
    .unwrap();
    assert_eq!(FIRST.bytes(), before + 9 * 32);
    core.rand().rand_bytes(&mut buf, false);
    assert_eq!(FIRST.bytes(), before + 9 * 32 + 7);

    // A second install is refused; the first stays.
    assert_eq!(install_process_source(&*SECOND), Err(Installed));
    process_source().rand_bytes(&mut buf, false);
    FastRandom::new().rand64();
    assert_eq!(SECOND.bytes(), 0);
    assert_eq!(FIRST.bytes(), before + 9 * 32 + 7 + 7 + 32);
}
