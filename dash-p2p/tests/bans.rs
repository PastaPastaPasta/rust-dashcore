//! Ban-list saves with Core's `BanMan::DumpBanlist` ordering (v24.0.0-rc.3 `banman.cpp:47-69`):
//! one save at a time under `dump_mutex`, a change after the snapshot left for the next save,
//! and the flag set again when a write fails. That the snapshot and the dirty-flag clear share
//! one critical section is `BanList::take_dump`'s structure; no test here can reach between them.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use dash_clock::MockClock;
use dash_p2p::ban::{BanEntry, BanList};
use dash_p2p::store::{NetBlob, NetStore};
use dash_p2p::{
    NetAddr, NetConfig, NetCore, NetEvents, PeerHandle, Resolver, ServiceFlags, SubNet,
};
use dash_params::{ChainParamsBuilder, Network, Release};
use parking_lot::Mutex;

const NOW: i64 = 1_700_000_000;

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

/// A store whose ban-list saves can be held: once armed, the next save reports its start on
/// `entered` and waits for a message on `release` (`false` fails the write).
#[derive(Default)]
struct GateStore {
    bans: Mutex<Option<Vec<u8>>>,
    /// Completed ban-list writes.
    writes: AtomicUsize,
    gate: Mutex<Option<(Sender<()>, Receiver<bool>)>>,
    in_save: AtomicBool,
    overlapped: AtomicBool,
}

impl GateStore {
    /// Holds the next ban-list save; returns its start signal and its release.
    fn arm(&self) -> (Receiver<()>, Sender<bool>) {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *self.gate.lock() = Some((entered_tx, release_rx));
        (entered_rx, release_tx)
    }

    fn saved(&self) -> Vec<(SubNet, BanEntry)> {
        let list = BanList::new(0, 1);
        list.load(self.bans.lock().as_deref().expect("a saved ban list"), NOW);
        list.banned(NOW)
    }
}

impl NetStore for GateStore {
    fn load(&self, _blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
        if blob != NetBlob::Bans {
            return Ok(());
        }
        if self.in_save.swap(true, Ordering::SeqCst) {
            self.overlapped.store(true, Ordering::SeqCst);
        }
        let gate = self.gate.lock().take();
        let ok = gate.is_none_or(|(entered, release)| {
            entered.send(()).unwrap();
            release.recv().unwrap()
        });
        if ok {
            *self.bans.lock() = Some(bytes.to_vec());
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        self.in_save.store(false, Ordering::SeqCst);
        if ok {
            Ok(())
        } else {
            Err(io::Error::other("write failed"))
        }
    }

    fn remove(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }

    fn backup(&self, _blob: NetBlob) -> io::Result<()> {
        Ok(())
    }
}

fn core(rt: &tokio::runtime::Runtime, store: Arc<GateStore>) -> Arc<NetCore> {
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let mut cfg = NetConfig::new(Arc::new(params), ServiceFlags::NODE_NETWORK);
    cfg.deterministic = true;
    NetCore::new(
        cfg,
        rt.handle().clone(),
        store,
        Arc::new(NoResolver),
        Arc::new(MockClock::at(NOW)),
        Arc::new(Events),
    )
    .unwrap()
}

fn net(d: u8) -> SubNet {
    SubNet::single(NetAddr::ipv4([10, 0, 0, d]))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap()
}

/// Two `setban add` calls whose saves would finish in the opposite order: the second dump waits
/// for the first, so the newer list is the one on disk.
#[test]
fn saves_do_not_finish_out_of_order() {
    let rt = runtime();
    let store = Arc::new(GateStore::default());
    let core = core(&rt, store.clone());
    let (entered, release) = store.arm();

    let first = thread::spawn({
        let core = core.clone();
        move || core.ban(net(1), 100, false)
    });
    entered.recv().unwrap();
    let second = thread::spawn({
        let core = core.clone();
        move || core.ban(net(2), 100, false)
    });
    while !core.banlist().is_subnet_banned(&net(2), NOW) {
        thread::yield_now();
    }
    // Without the dump mutex the second save (both bans) would now complete before the first
    // (one ban) and be overwritten by it. Give it the chance, then let the first finish.
    let writes = store.writes.load(Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_millis(200);
    while store.writes.load(Ordering::SeqCst) == writes && Instant::now() < deadline {
        thread::yield_now();
    }
    release.send(true).unwrap();
    first.join().unwrap();
    second.join().unwrap();

    assert!(!store.overlapped.load(Ordering::SeqCst), "two ban-list saves ran at once");
    let saved: Vec<SubNet> = store.saved().into_iter().map(|(n, _)| n).collect();
    assert_eq!(saved, vec![net(1), net(2)]);
    assert!(core.banlist().take_dump(NOW).is_none());
}

/// A ban added after the snapshot, while its save is in flight, is not in that save, so the list
/// stays dirty and the next dump writes it.
#[test]
fn change_after_the_snapshot_stays_dirty() {
    let rt = runtime();
    let store = Arc::new(GateStore::default());
    let core = core(&rt, store.clone());
    let (entered, release) = store.arm();

    let first = thread::spawn({
        let core = core.clone();
        move || core.ban(net(1), 100, false)
    });
    entered.recv().unwrap();
    assert!(core.banlist().ban(net(2), 100, false, NOW));
    release.send(true).unwrap();
    first.join().unwrap();

    let saved: Vec<SubNet> = store.saved().into_iter().map(|(n, _)| n).collect();
    assert_eq!(saved, vec![net(1)]);
    let next: Vec<SubNet> =
        core.banlist().take_dump(NOW).expect("still dirty").into_iter().map(|(n, _)| n).collect();
    assert_eq!(next, vec![net(1), net(2)]);
}

/// A failed write sets the dirty flag again (Core: `if (!m_ban_db.Write(banmap)) m_is_dirty =
/// true`), and the next dump retries.
#[test]
fn failed_save_stays_dirty() {
    let rt = runtime();
    let store = Arc::new(GateStore::default());
    let core = core(&rt, store.clone());
    let (entered, release) = store.arm();
    release.send(false).unwrap();
    let writes = store.writes.load(Ordering::SeqCst);
    core.ban(net(1), 100, false);
    entered.recv_timeout(Duration::ZERO).unwrap();
    assert_eq!(store.writes.load(Ordering::SeqCst), writes);
    assert!(store.saved().is_empty());

    // `setban remove` of another subnet changes nothing; the next ban dumps both.
    assert!(!core.unban(&net(9)));
    core.ban(net(2), 100, false);
    let saved: Vec<SubNet> = store.saved().into_iter().map(|(n, _)| n).collect();
    assert_eq!(saved, vec![net(1), net(2)]);
    assert!(core.banlist().take_dump(NOW).is_none());
}
