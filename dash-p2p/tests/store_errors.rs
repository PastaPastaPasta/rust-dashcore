//! `NetCore::new` when the store fails, as Core's init handles the files (v24.0.0-rc.3):
//! an unopenable `peers.dat` is a missing one (addrdb.cpp:125-128, :204-208), an unreadable
//! `banlist.json` is recreated (addrdb.cpp:153-178, banman.cpp:35-45), and an unsupported
//! `peers.dat` that cannot be renamed stops init with Core's text (addrdb.cpp:219-220).

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

use dash_clock::MockClock;
use dash_p2p::rng::{RandSource, StreamRand};
use dash_p2p::store::{NetBlob, NetStore};
use dash_p2p::{
    InitError, NetAddr, NetConfig, NetCore, NetEvents, PeerHandle, Resolver, ServiceFlags,
};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use parking_lot::Mutex;

const T0: i64 = 1_700_000_000;

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

/// Seed 42's stream, recording every `GetRandBytes` call.
struct Recorded {
    stream: StreamRand,
    calls: Mutex<Vec<Vec<u8>>>,
}

impl Recorded {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stream: StreamRand::from_seed(42),
            calls: Mutex::default(),
        })
    }
}

impl RandSource for Recorded {
    fn rand_bytes(&self, out: &mut [u8], strong: bool) {
        self.stream.rand_bytes(out, strong);
        self.calls.lock().push(out.to_vec());
    }
}

/// A store whose loads and backup fail on demand, recording what it saves.
#[derive(Default)]
struct Store {
    peers: Option<Vec<u8>>,
    peers_fail: bool,
    bans: Option<Vec<u8>>,
    bans_fail: bool,
    backup_fail: bool,
    saved: Mutex<Vec<NetBlob>>,
}

fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

impl NetStore for Store {
    fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        match blob {
            NetBlob::Peers if self.peers_fail => Err(denied()),
            NetBlob::Peers => Ok(self.peers.clone()),
            NetBlob::Bans if self.bans_fail => Err(denied()),
            NetBlob::Bans => Ok(self.bans.clone()),
            NetBlob::Anchors => Ok(None),
        }
    }

    fn save(&self, blob: NetBlob, _bytes: &[u8]) -> io::Result<()> {
        self.saved.lock().push(blob);
        Ok(())
    }

    fn remove(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }

    fn backup(&self, _blob: NetBlob) -> io::Result<()> {
        if self.backup_fail {
            Err(denied())
        } else {
            Ok(())
        }
    }
}

fn params() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap()
}

fn start(store: Arc<Store>, rand: Arc<Recorded>) -> Result<Arc<NetCore>, InitError> {
    NetCore::new(
        NetConfig::new(Arc::new(params()), ServiceFlags::NODE_NETWORK).with_rand_source(rand),
        tokio::runtime::Handle::current(),
        store,
        Arc::new(NoResolver),
        Arc::new(MockClock::at(T0)),
        Arc::new(Events),
    )
}

/// The state a start leaves: every draw, AddrMan's key and size, what was saved, and the bans.
async fn outcome(store: Store) -> (Vec<Vec<u8>>, [u8; 32], usize, Vec<NetBlob>, usize) {
    let store = Arc::new(store);
    let rand = Recorded::new();
    let core = start(store.clone(), rand.clone()).unwrap();
    let state = (
        rand.calls.lock().clone(),
        core.addrman().canonical().key,
        core.addrman().size(None, None),
        store.saved.lock().clone(),
        core.banlist().banned(T0).len(),
    );
    core.shutdown().await;
    state
}

/// A `peers.dat` Core cannot open is `DbNotFoundError`: a new AddrMan (with its own key, so the
/// same draws as a missing file) written out at once.
#[tokio::test]
async fn unopenable_peers_dat_is_a_missing_one() {
    let missing = outcome(Store::default()).await;
    let unopenable = outcome(Store {
        peers_fail: true,
        ..Store::default()
    })
    .await;
    assert_eq!(unopenable, missing);
    // The constructor and the replacement AddrMan (a context and two salts each), BanMan and
    // CConnman's two seeds.
    assert_eq!(unopenable.0.len(), 9);
    assert_eq!(unopenable.2, 0);
    // `DumpPeerAddresses`, then `BanMan`'s constructor dumps the list it recreated.
    assert_eq!(unopenable.3, [NetBlob::Peers, NetBlob::Bans]);
}

/// A `banlist.json` Core cannot read fails `CBanDB::Read`, so `LoadBanlist` starts empty and
/// dirty ("Recreating the banlist database"), whatever the file held, and `BanMan`'s
/// constructor writes it at once (banman.cpp:17-21); no draw changes. A readable file is not
/// rewritten.
#[tokio::test]
async fn unreadable_banlist_is_recreated() {
    let image = br#"{"banned_nets": [{"version": 1, "ban_created": 1700000000, "banned_until": 4102444800, "address": "1.2.3.4/32"}]}"#;
    let readable = Arc::new(Store {
        bans: Some(image.to_vec()),
        ..Store::default()
    });
    let core = start(readable.clone(), Recorded::new()).unwrap();
    assert_eq!(core.banlist().banned(T0).len(), 1);
    assert_eq!(*readable.saved.lock(), [NetBlob::Peers]);
    core.shutdown().await;

    let unreadable = Arc::new(Store {
        bans: Some(image.to_vec()),
        bans_fail: true,
        ..Store::default()
    });
    let rand = Recorded::new();
    let core = start(unreadable.clone(), rand.clone()).unwrap();
    assert!(core.banlist().banned(T0).is_empty());
    assert_eq!(*unreadable.saved.lock(), [NetBlob::Peers, NetBlob::Bans]);
    core.shutdown().await;
    let missing = outcome(Store::default()).await.0;
    assert_eq!(*rand.calls.lock(), missing);
}

/// An unsupported `peers.dat` whose rename to `peers.dat.bak` fails stops init with Core's
/// message; with the rename it starts on a new AddrMan.
#[tokio::test]
async fn unrenamable_peers_dat_stops_init() {
    // The network magic, format 4 and a compat byte whose `lowest_compatible` (223) is past
    // `FILE_FORMAT`: Core's `InvalidAddrManVersionError` (addrman.cpp:254-260).
    let mut image = params().message_start.to_vec();
    image.extend_from_slice(&[4, 255]);

    let err = start(
        Arc::new(Store {
            peers: Some(image.clone()),
            backup_fail: true,
            ..Store::default()
        }),
        Recorded::new(),
    )
    .err()
    .expect("init fails");
    assert!(matches!(err, InitError::RenamePeers(_)));
    assert_eq!(
        err.to_string(),
        "Failed to rename invalid peers.dat file. Please move or delete it and try again."
    );
    assert!(std::error::Error::source(&err).is_some());

    let renamed = outcome(Store {
        peers: Some(image),
        ..Store::default()
    })
    .await;
    assert_eq!(renamed.2, 0);
    assert_eq!(renamed.3, [NetBlob::Peers, NetBlob::Bans]);
}
