//! The `test-utils` hooks an exact session drives (D35, D38): every random byte from the
//! harness's source in Core's order, `InactivityCheck` on the harness's tick, and the state it
//! compares (AddrMan, the ping fields).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use dash_clock::MockClock;
use dash_p2p::addrman::AddrMan;
use dash_p2p::conn::{ConnectionType, Permissions};
use dash_p2p::handshake::{VersionInputs, version_for};
use dash_p2p::netgroup::NetGroupManager;
use dash_p2p::ping::{PingState, maybe_send_ping};
use dash_p2p::rng::{FastRandom, RandSource, StreamRand};
use dash_p2p::siphash::SipHasher;
use dash_p2p::store::{NetBlob, NetStore, NoStore};
use dash_p2p::{
    Address, Attach, LocalIdentity, MemSocket, NetAddr, NetConfig, NetCore, NetEvents, PeerHandle,
    ProtocolVersion, Resolver, Service, ServiceFlags,
};
use dash_p2p_core::UserAgent;
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;

const T0: i64 = 1_700_000_000;

/// O1c's stream for seed 42, recording each `GetRandBytes` call: its bytes and level.
struct Recorded {
    stream: StreamRand,
    calls: Mutex<Vec<(Vec<u8>, bool)>>,
}

impl Recorded {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stream: StreamRand::from_seed(42),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// The calls since the last `take`.
    fn take(&self) -> Vec<(Vec<u8>, bool)> {
        std::mem::take(&mut self.calls.lock())
    }
}

impl RandSource for Recorded {
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        self.stream.rand_bytes(out, strong);
        self.calls.lock().push((out.to_vec(), strong));
    }
}

/// `GetRand(max)`'s value from the 32 bytes that keyed its context.
fn get_rand_from(key: &[u8], max: u64) -> u64 {
    FastRandom::with_seed(key.try_into().unwrap()).randrange(max)
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

/// Answers every name with two addresses in their own groups, `<n>.1.0.1` and `<n>.2.0.1` for the
/// `n`th lookup.
#[derive(Default)]
struct TwoEach(Mutex<u8>);

impl Resolver for TwoEach {
    fn resolve<'a>(
        &'a self,
        _host: &'a str,
        _max: usize,
        _allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        let mut n = self.0.lock();
        *n += 1;
        let n = *n;
        Box::pin(async move { vec![NetAddr::ipv4([n, 1, 0, 1]), NetAddr::ipv4([n, 2, 0, 1])] })
    }
}

fn core(cfg: impl FnOnce(NetConfig) -> NetConfig) -> (Arc<NetCore>, Arc<MockClock>) {
    core_resolving(Arc::new(NoResolver), cfg)
}

fn core_resolving(
    resolver: Arc<dyn Resolver>,
    cfg: impl FnOnce(NetConfig) -> NetConfig,
) -> (Arc<NetCore>, Arc<MockClock>) {
    core_with(Arc::new(NoStore), resolver, cfg)
}

fn core_with(
    store: Arc<dyn NetStore>,
    resolver: Arc<dyn Resolver>,
    cfg: impl FnOnce(NetConfig) -> NetConfig,
) -> (Arc<NetCore>, Arc<MockClock>) {
    let clock = Arc::new(MockClock::at(T0));
    let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap();
    let core = NetCore::new(
        cfg(NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK)),
        tokio::runtime::Handle::current(),
        store,
        resolver,
        clock.clone(),
        Arc::new(Events),
    )
    .unwrap();
    (core, clock)
}

fn outbound(use_v2: bool) -> Attach {
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
        use_v2,
        grant: None,
        masternode: false,
        masternode_probe: false,
    }
}

/// `NetCore::new` draws as `init.cpp` does: AddrMan's context (its `nKey`) and its `mapAddr`
/// salts, the new AddrMan `LoadAddrman` builds when there is no `peers.dat` (the same three),
/// BanMan's discouragement filter, then `GetRand<uint64_t>()` for nSeed1 and for nSeed0 (right
/// to left, as GCC evaluates `make_unique<CConnman>`'s arguments): 32 bytes each, nothing else.
/// This is O1c's `setup_draws_in_inits_order` from `LoadAddrman` on, with no store draw.
#[tokio::test]
async fn netcore_new_draws_in_inits_order() {
    let rec = Recorded::new();
    let (core, _clock) = core(|c| c.with_rand_source(rec.clone()));
    let calls = rec.take();
    assert_eq!(calls.iter().map(|(b, s)| (b.len(), *s)).collect::<Vec<_>>(), [(32, false); 9]);
    // The second AddrMan's key: the first 32 bytes of its context.
    let key = FastRandom::with_seed(calls[3].0.clone().try_into().unwrap()).rand256();
    assert_eq!(core.addrman().canonical().key, key);
    let (seed1, seed0) =
        (get_rand_from(&calls[7].0, u64::MAX), get_rand_from(&calls[8].0, u64::MAX));
    assert_eq!(
        core.deterministic_randomizer(9).finish(),
        SipHasher::new(seed0, seed1).write_u64(9).finish()
    );
}

/// Serves one `peers.dat` image and keeps nothing.
struct ImageStore(Vec<u8>);

impl NetStore for ImageStore {
    fn load(&self, blob: NetBlob) -> std::io::Result<Option<Vec<u8>>> {
        Ok(matches!(blob, NetBlob::Peers).then(|| self.0.clone()))
    }

    fn save(&self, _blob: NetBlob, _bytes: &[u8]) -> std::io::Result<()> {
        Ok(())
    }

    fn remove(&self, _blob: NetBlob) -> std::io::Result<()> {
        Ok(())
    }

    fn backup(&self, _blob: NetBlob) -> std::io::Result<()> {
        Ok(())
    }
}

/// `-checkaddrman`: every `Check()` rolls `randrange(3)` on AddrMan's context.
const CHECK_RATIO: u32 = 3;

/// A readable `peers.dat` with a few addresses, and the AddrMan state it holds.
async fn peers_image() -> (Vec<u8>, dash_p2p::addrman::Canonical) {
    let (core, _clock) = core(|c| c.with_rand_source(Arc::new(StreamRand::from_seed(7))));
    for i in 0..8 {
        let addr = Address {
            service: Service::new(NetAddr::ipv4([1, 2, i, 4]), 9999),
            services: ServiceFlags::NODE_NETWORK,
            time: (T0 - 100) as u32,
        };
        core.addrman().add(&[addr], &NetAddr::ipv4([5, 6, i, 8]), 0, T0);
    }
    let image = (core.addrman().serialize(), core.addrman().canonical());
    core.shutdown().await;
    image
}

/// Starts on `image` with every draw recorded.
fn start_on(image: Vec<u8>) -> (Arc<NetCore>, Vec<(Vec<u8>, bool)>) {
    let rec = Recorded::new();
    let (core, _clock) = core_with(Arc::new(ImageStore(image)), Arc::new(NoResolver), |mut c| {
        c.check_addrman = CHECK_RATIO;
        c.with_rand_source(rec.clone())
    });
    let calls = rec.take();
    assert!(calls.iter().all(|(b, strong)| b.len() == 32 && !*strong));
    // CConnman's seeds are the last two draws, nSeed1 first, whatever came before.
    let n = calls.len();
    let (seed0, seed1) =
        (get_rand_from(&calls[n - 1].0, u64::MAX), get_rand_from(&calls[n - 2].0, u64::MAX));
    assert_eq!(
        core.deterministic_randomizer(9).finish(),
        SipHasher::new(seed0, seed1).write_u64(9).finish()
    );
    (core, calls)
}

/// AddrMan's context is `expected` (same keystream position and bit buffer), now and after
/// `Size()`'s two more `Check()` rolls.
fn assert_context(core: &NetCore, expected: &mut FastRandom) {
    let same = |core: &NetCore, expected: &FastRandom| {
        let c = core.addrman().canonical();
        assert_eq!(
            (c.rng_keystream_bytes, (c.rng_bitbuf_size, c.rng_bitbuf)),
            (expected.keystream_bytes(), expected.bitbuf())
        );
    };
    same(core, expected);
    core.addrman().size(None, None);
    expected.randrange(u64::from(CHECK_RATIO));
    expected.randrange(u64::from(CHECK_RATIO));
    same(core, expected);
}

/// A readable `peers.dat` is read into the AddrMan `LoadAddrman` has just built: its context and
/// two salts, the context kept through the read (Core's `Unserialize` seeds nothing), then
/// `Size()`'s two `Check()` rolls on it, BanMan and the seeds: six draws.
#[tokio::test]
async fn readable_peers_dat_keeps_the_constructors_context() {
    let (image, saved) = peers_image().await;
    let (core, calls) = start_on(image);
    assert_eq!(calls.len(), 6);
    // The file's key and addresses (serialization renumbers the ids, as Core's does).
    let addrs = |c: &dash_p2p::addrman::Canonical| {
        let mut a: Vec<_> = c.entries.iter().map(|(_, e)| e.service.clone()).collect();
        a.sort();
        a
    };
    let c = core.addrman().canonical();
    assert_eq!((c.key, addrs(&c)), (saved.key, addrs(&saved)));
    assert!(!c.entries.is_empty());
    let mut expected = FastRandom::with_seed(calls[0].0.clone().try_into().unwrap());
    expected.rand256();
    expected.randrange(u64::from(CHECK_RATIO));
    expected.randrange(u64::from(CHECK_RATIO));
    assert_context(&core, &mut expected);
    core.shutdown().await;
}

/// An unsupported or inconsistent `peers.dat`: the constructor's three draws, then the
/// replacement AddrMan's (Core's recovery branches build a new one, a new key and salts), BanMan
/// and the seeds: nine.
#[tokio::test]
async fn unreadable_peers_dat_draws_only_for_cores_replacement() {
    let (image, _saved) = peers_image().await;
    let mut unsupported = image.clone();
    // `lowest_compatible` past this version (Core's `InvalidAddrManVersionError`).
    unsupported[5] = 255;
    let mut inconsistent = image;
    // A null `nKey` fails `CheckAddrman` (−16, Core's `DbInconsistentError`).
    inconsistent[6..38].fill(0);
    for image in [unsupported, inconsistent] {
        let (core, calls) = start_on(image);
        assert_eq!(calls.len(), 9);
        let mut expected = FastRandom::with_seed(calls[3].0.clone().try_into().unwrap());
        let c = core.addrman().canonical();
        assert_eq!((c.key, c.entries.len()), (expected.rand256(), 0));
        assert_context(&core, &mut expected);
        core.shutdown().await;
    }
}

/// [`AddrMan::unserialize`] reads in place and draws nothing; [`AddrMan::deserialize`] reads
/// into a newly built AddrMan, whose context and two salts take 32 bytes each.
#[tokio::test]
async fn deserialize_reseeds_and_unserialize_does_not() {
    let (image, _saved) = peers_image().await;
    let rec = Recorded::new();
    let (core, _clock) = core(|c| c.with_rand_source(rec.clone()));
    rec.take();
    let before = core.addrman().canonical().rng_keystream_bytes;
    core.addrman().unserialize(&image).unwrap();
    assert_eq!(rec.take().len(), 0);
    assert_eq!(core.addrman().canonical().rng_keystream_bytes, before);
    core.addrman().deserialize(&image).unwrap();
    assert_eq!(rec.take().iter().map(|(b, _)| b.len()).collect::<Vec<_>>(), [32; 3]);
    assert_eq!(core.addrman().canonical().rng_keystream_bytes, 32);
    core.shutdown().await;
}

/// A deterministic AddrMan's key and context draw nothing, but its `mapAddr` salts still take a
/// `GetRand<uint64_t>()` each (netaddress.h:576-578), at construction and at every `clear`, as
/// Core's replacement `AddrMan` does.
#[test]
fn a_deterministic_addrman_still_draws_its_salts() {
    let rec = Recorded::new();
    let params = ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap();
    let addrman = AddrMan::with_rand_source(
        NetGroupManager::new(Vec::new(), false),
        true,
        0,
        false,
        params.message_start,
        rec.clone(),
    );
    assert_eq!(rec.take().iter().map(|(b, s)| (b.len(), *s)).collect::<Vec<_>>(), [(32, false); 2]);
    assert_eq!(addrman.canonical().rng_keystream_bytes, 0);
    addrman.clear();
    assert_eq!(rec.take().len(), 2);
}

/// A V2 peer's transport draws Core's `V2Transport` constructor's bytes in order (the key at
/// `GetStrongRandBytes`, ElligatorSwift entropy, the garbage context's seed), and the MNAUTH
/// challenge waits for our `version` (`PushNodeVersion`), which draws it.
#[tokio::test]
async fn attach_draws_the_transport_and_version_draws_the_challenge() {
    let rec = Recorded::new();
    let (core, _clock) = core(|c| c.with_rand_source(rec.clone()));
    rec.take();
    let peer = core.attach(outbound(true));
    let calls = rec.take();
    let (key, rest) = calls.split_at(calls.len() - 2);
    assert!(!key.is_empty() && key.iter().all(|(b, strong)| b.len() == 32 && *strong));
    assert!(rest.iter().all(|(b, strong)| b.len() == 32 && !*strong));
    assert_eq!(peer.sent_mnauth_challenge(), [0; 32]);

    let id = LocalIdentity {
        protocol_version: ProtocolVersion::V24_0,
        advertised_version: ProtocolVersion::V24_0.0,
        user_agent: UserAgent::new(b"/Dash Core:24.0.0/".to_vec()).unwrap(),
        masternode: false,
    };
    let inputs = VersionInputs {
        our_services: ServiceFlags::NODE_NETWORK,
        best_height: 0,
        relay: true,
        now: T0,
    };
    let _ = version_for(&core, &peer, &id, &inputs);
    let calls = rec.take();
    assert_eq!(calls.len(), 1);
    assert_eq!(peer.sent_mnauth_challenge().as_slice(), calls[0].0.as_slice());
    core.shutdown().await;
}

/// The ping nonce is `GetRand<uint64_t>()`: a fresh context's first `randrange`, and the per-peer
/// ping fields a session compares are readable.
#[tokio::test]
async fn ping_nonce_is_get_rand_and_ping_fields_read() {
    let rec = Recorded::new();
    let (core, _clock) = core(|c| c.with_rand_source(rec.clone()));
    let peer = core.attach(outbound(false));
    rec.take();
    let mut ping = PingState::default();
    maybe_send_ping(&core, &peer, &mut ping, (T0 + 3) * 1_000_000);
    let calls = rec.take();
    assert_eq!(calls.len(), 1);
    assert_eq!(ping.nonce_sent, get_rand_from(&calls[0].0, u64::MAX));
    assert_eq!(ping.start_us, (T0 + 3) * 1_000_000);
    assert_eq!((peer.last_ping_us(), peer.min_ping_us()), (0, i64::MAX));
    core.shutdown().await;
}

/// With manual inactivity checks the sweep leaves a silent peer alone; the harness's
/// `run_inactivity_checks` marks it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inactivity_checks_run_on_the_harness_tick() {
    let (core, clock) = core(|mut c| {
        c.peer_timeout = 60;
        c.with_manual_inactivity_checks()
    });
    let peer = core.attach(outbound(false));
    clock.set_mocktime(T0 + 61);
    // Several sweep passes (at most 500 ms apart).
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(!peer.disconnect_requested());
    core.run_inactivity_checks();
    assert!(peer.disconnect_requested());
    core.shutdown().await;
}

/// AddrMan's state reads as `diff_addrman` compares it.
#[tokio::test]
async fn addrman_reads_canonically() {
    let (core, _clock) = core(|c| c.with_rand_source(Recorded::new()));
    let addr = Address {
        service: Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999),
        services: ServiceFlags::NODE_NETWORK,
        time: (T0 - 100) as u32,
    };
    assert!(core.addrman().add(&[addr], &NetAddr::ipv4([5, 6, 7, 8]), 0, T0));
    assert_eq!(core.addrman().canonical().entries.len(), 1);
    core.shutdown().await;
}

/// The DNS seeds' addresses are dated from `ThreadDNSAddressSeed`'s one context: 32 bytes at its
/// first draw, then `-randrange(4 days)` per address in resolver order, across seeds.
#[tokio::test]
async fn dns_seed_addresses_share_the_threads_context() {
    let rec = Recorded::new();
    let (core, _clock) =
        core_resolving(Arc::new(TwoEach::default()), |c| c.with_rand_source(rec.clone()));
    rec.take();
    assert_eq!(core.resolve_seed("seed.one", 256).await, 2);
    assert_eq!(core.resolve_seed("seed.two", 256).await, 2);
    let calls = rec.take();
    assert_eq!(calls.len(), 1);
    let mut rng = FastRandom::with_seed(calls[0].0.clone().try_into().unwrap());
    for n in 1..=2 {
        for last in 1..=2 {
            let port = core.config().params.default_port;
            let service = Service::new(NetAddr::ipv4([n, last, 0, 1]), port);
            let entry = core.addrman().get_address_info(&service).expect("added");
            assert_eq!(entry.time, T0 - 3 * 86_400 + rng.rand_uniform(-4 * 86_400));
        }
    }
    core.shutdown().await;
}
