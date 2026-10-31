//! The drivers, the dump schedule and the resolver on a real tokio runtime (no network: in-memory
//! peers, a listener on 127.0.0.1 at most, and recording or numeric-only resolvers).
//!
//! - a peer leaving mid-sleep does not shorten Core's sleep (`CThreadInterrupt::sleep_for`);
//! - `stop_threads` waits for a dump already running, so the final dump is the last write, and
//!   the listening port is free once it returns;
//! - the dump schedule waits a full interval after each dump (`CScheduler::Repeat`);
//! - the DNS-seed thread looks up with `-dns=0` (`LookupHost(host, 32, true)`), named targets
//!   do not;
//! - `-dns=0` decodes numeric hosts as the system's `getaddrinfo(AI_NUMERICHOST)` does.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dash_clock::SystemClock;
use dash_p2p::store::{NetBlob, NetStore, NoStore};
use dash_p2p::{
    Address, Attach, ConnectionType, DialLoop, DialPlan, MemSocket, NetAddr, NetConfig, NetCore,
    NetEvents, OpenError, OpenRequest, PeerHandle, Permissions, Service, ServiceFlags, Target,
};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_net::binds::BindOptions;
use dashd_net::driver::{DialExec, RuntimeExec};
use dashd_net::resolver::DnsLookupFn;
use dashd_net::sim::SimPolicy;
use dashd_net::{ConnMan, ConnManOptions, FileStore, SystemResolver};
use dashd_p2p::fingerprint::{IdentityOptions, local_services};

/// Events for a core with no processor; records when a peer was last finalized (removed).
#[derive(Default)]
struct Quiet(Mutex<Option<Instant>>);

impl NetEvents for Quiet {
    fn wake(&self) {}
    fn initialize(&self, _: &NetCore, _: &PeerHandle, _: ServiceFlags) {}
    fn finalize(&self, _: &NetCore, _: &PeerHandle) {
        *self.0.lock().unwrap() = Some(Instant::now());
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap()
}

/// A core on `kind` that dials nothing by itself (no DNS or fixed seeds, `-connect=0`) and does
/// not listen, unless `hook` says otherwise.
fn core(
    rt: &tokio::runtime::Runtime,
    kind: NetworkKind,
    store: Arc<dyn NetStore>,
    resolver: SystemResolver,
    hook: impl FnOnce(&mut NetConfig),
) -> Arc<NetCore> {
    core_with(rt, kind, store, resolver, Arc::default(), hook)
}

/// [`core`] reporting to `events`.
fn core_with(
    rt: &tokio::runtime::Runtime,
    kind: NetworkKind,
    store: Arc<dyn NetStore>,
    resolver: SystemResolver,
    events: Arc<Quiet>,
    hook: impl FnOnce(&mut NetConfig),
) -> Arc<NetCore> {
    let params = Arc::new(ChainParamsBuilder::new(kind, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, local_services(&IdentityOptions::new()));
    cfg.dns_seed = false;
    cfg.fixed_seeds = false;
    cfg.listen = false;
    cfg.connect = Some(Vec::new());
    hook(&mut cfg);
    let resolver = Arc::new(resolver);
    let core =
        NetCore::new(cfg, rt.handle().clone(), store, resolver, Arc::new(SystemClock), events);
    core.unwrap()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dashd-net-runtime-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A boolean a thread can wait on.
#[derive(Default)]
struct Flag(Mutex<bool>, Condvar);

impl Flag {
    fn set(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }

    fn is_set(&self) -> bool {
        *self.0.lock().unwrap()
    }

    fn wait(&self) {
        let set = self.0.lock().unwrap();
        let (set, _) = self.1.wait_timeout_while(set, Duration::from_secs(60), |s| !*s).unwrap();
        assert!(*set, "timed out (box load?)");
    }
}

/// An `-addnode` loop with nothing to add sleeps 2 s between rounds (Core's
/// `ThreadOpenAddedConnections`); a peer that goes away 150 ms in leaves the sleep at 2 s, and
/// the loop's next plan is the next round's sleep. A removal that load delays past the sleep
/// proves nothing, so the round is repeated (at most three times).
#[test]
fn peer_removal_keeps_the_full_addcon_sleep() {
    let rt = runtime();
    let events = Arc::new(Quiet::default());
    let store = Arc::new(NoStore);
    let core = core_with(
        &rt,
        NetworkKind::Regtest,
        store,
        SystemResolver::default(),
        events.clone(),
        |c| {
            c.connect = None;
        },
    );
    core.start_addresses();
    let policy = SimPolicy::default();
    let mut exec = RuntimeExec::new(rt.handle().clone(), None);
    let d = Duration::from_secs(2);
    for round in 1.. {
        let (ours, remote) = MemSocket::pair();
        core.attach(Attach {
            sock: Arc::new(ours),
            addr: Address {
                service: Service::from_socket_addr("127.0.0.1:12345".parse().unwrap()),
                services: ServiceFlags::NONE,
                time: 0,
            },
            addr_bind: Service::default(),
            dest: None,
            conn_type: ConnectionType::Inbound,
            inbound_onion: false,
            permissions: Permissions::NONE,
            use_v2: false,
            grant: None,
            masternode: false,
            masternode_probe: false,
        });
        *events.0.lock().unwrap() = None;
        // Each round, and the first: nothing to add, so the round's 2 s sleep.
        assert!(matches!(core.dial_tick(DialLoop::AddCon, &policy), DialPlan::Sleep(n) if n == d));

        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            remote.close();
        });
        let start = Instant::now();
        assert!(exec.wait(&core, DialLoop::AddCon, Some(d)), "not interrupted");
        let slept = start.elapsed();
        closer.join().unwrap();
        assert!(slept >= d, "slept {slept:?} of {d:?}");
        let deadline = Instant::now() + Duration::from_secs(60);
        let removed = loop {
            if let Some(at) = *events.0.lock().unwrap() {
                break at;
            }
            assert!(Instant::now() < deadline, "the peer was never removed");
            std::thread::sleep(Duration::from_millis(10));
        };
        if removed < start + d {
            break;
        }
        assert!(round < 3, "the removal never landed inside the sleep (box load?)");
    }
    assert!(matches!(core.dial_tick(DialLoop::AddCon, &policy), DialPlan::Sleep(n) if n == d));

    // Interrupt still ends a sleep at once.
    let c = Arc::clone(&core);
    let interrupter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        c.interrupt();
    });
    let start = Instant::now();
    assert!(!exec.wait(&core, DialLoop::AddCon, Some(Duration::from_secs(60))));
    assert!(start.elapsed() < Duration::from_secs(30));
    interrupter.join().unwrap();
    rt.block_on(core.shutdown());
}

/// A [`FileStore`] whose next `peers.dat` save, once armed, holds until released.
#[derive(Default)]
struct GatedStore {
    files: Option<FileStore>,
    arm: AtomicBool,
    started: Flag,
    release: Flag,
    finished: Flag,
    /// `peers.dat` saves completed.
    peer_saves: Mutex<Vec<Instant>>,
}

impl NetStore for GatedStore {
    fn load(&self, blob: NetBlob) -> io::Result<Option<Vec<u8>>> {
        self.files.as_ref().unwrap().load(blob)
    }

    fn save(&self, blob: NetBlob, bytes: &[u8]) -> io::Result<()> {
        let gated = blob == NetBlob::Peers && self.arm.swap(false, Ordering::SeqCst);
        if gated {
            self.started.set();
            self.release.wait();
        }
        let r = self.files.as_ref().unwrap().save(blob, bytes);
        if blob == NetBlob::Peers {
            self.peer_saves.lock().unwrap().push(Instant::now());
        }
        if gated {
            self.finished.set();
        }
        r
    }

    fn remove(&self, blob: NetBlob) -> io::Result<()> {
        self.files.as_ref().unwrap().remove(blob)
    }

    fn backup(&self, blob: NetBlob) -> io::Result<()> {
        self.files.as_ref().unwrap().backup(blob)
    }
}

/// A periodic dump still running at shutdown: `stop_threads` returns only after it, so the final
/// dump in `shutdown` is the last write of `peers.dat`; and the listener is closed, so its port
/// binds again at once.
#[test]
fn stop_waits_for_a_running_dump_and_frees_the_port() {
    let rt = runtime();
    let dir = scratch("gated-dump");
    let store = Arc::new(GatedStore {
        files: Some(FileStore::new(&dir)),
        ..GatedStore::default()
    });
    let core = core(&rt, NetworkKind::Regtest, store.clone(), SystemResolver::default(), |c| {
        c.listen = true;
    });
    store.arm.store(true, Ordering::SeqCst);
    let opts = ConnManOptions {
        binds: BindOptions {
            binds: vec![Service::from_socket_addr("127.0.0.1:0".parse().unwrap())],
            ..BindOptions::default()
        },
        dump_interval: Duration::from_millis(100),
        ..ConnManOptions::default()
    };
    let cm = ConnMan::start(
        Arc::clone(&core),
        rt.handle().clone(),
        Arc::new(SimPolicy::default()),
        opts,
    )
    .unwrap();
    let port = cm.listen_addrs()[0].port();
    store.started.wait();

    // An address the running dump does not have.
    let a = Address {
        service: Service::from_socket_addr("31.2.3.4:9999".parse().unwrap()),
        services: ServiceFlags::NODE_NETWORK,
        time: 1_700_000_000,
    };
    core.addrman().add(std::slice::from_ref(&a), &a.service.addr, 0, 1_700_000_000);
    let s = Arc::clone(&store);
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        s.release.set();
    });
    cm.interrupt();
    cm.stop_threads();
    assert!(store.finished.is_set(), "stop_threads returned before the running dump finished");
    releaser.join().unwrap();

    // The listener is closed: Core's `BindListenPort` (`SO_REUSEADDR`) binds the port again.
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None).unwrap();
    sock.set_reuse_address(true).unwrap();
    let sa: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    sock.bind(&sa.into()).unwrap();
    sock.listen(1).unwrap();
    drop(sock);

    rt.block_on(core.shutdown());
    let saves = store.peer_saves.lock().unwrap().len();
    let last = std::fs::read(dir.join("peers.dat")).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(store.peer_saves.lock().unwrap().len(), saves, "a write after the final dump");
    assert_eq!(std::fs::read(dir.join("peers.dat")).unwrap(), last);
    // The final image has the address the old dump lacked.
    let reloaded = core_from(&rt, &dir);
    assert_eq!(reloaded.addrman().size(None, None), 1);
    rt.block_on(reloaded.shutdown());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A core over the files in `dir` (loads `peers.dat`).
fn core_from(rt: &tokio::runtime::Runtime, dir: &std::path::Path) -> Arc<NetCore> {
    core(rt, NetworkKind::Regtest, Arc::new(FileStore::new(dir)), SystemResolver::default(), |_| {})
}

/// A store whose `peers.dat` saves take 300 ms, recording when each starts and ends.
#[derive(Default)]
struct SlowStore(Mutex<Vec<(Instant, Instant)>>);

impl NetStore for SlowStore {
    fn load(&self, _: NetBlob) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }

    fn save(&self, blob: NetBlob, _: &[u8]) -> io::Result<()> {
        if blob == NetBlob::Peers {
            let start = Instant::now();
            std::thread::sleep(Duration::from_millis(300));
            self.0.lock().unwrap().push((start, Instant::now()));
        }
        Ok(())
    }

    fn remove(&self, _: NetBlob) -> io::Result<()> {
        Ok(())
    }

    fn backup(&self, _: NetBlob) -> io::Result<()> {
        Ok(())
    }
}

/// Each dump starts a full interval after the previous one ended, however long it took.
#[test]
fn dumps_wait_a_full_interval_after_each_dump() {
    let rt = runtime();
    let store = Arc::new(SlowStore::default());
    let core = core(&rt, NetworkKind::Regtest, store.clone(), SystemResolver::default(), |_| {});
    let every = Duration::from_millis(200);
    let opts = ConnManOptions {
        dump_interval: every,
        ..ConnManOptions::default()
    };
    let cm = ConnMan::start(
        Arc::clone(&core),
        rt.handle().clone(),
        Arc::new(SimPolicy::default()),
        opts,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while store.0.lock().unwrap().len() < 3 {
        assert!(Instant::now() < deadline, "timed out (box load?)");
        std::thread::sleep(Duration::from_millis(20));
    }
    cm.interrupt();
    cm.stop_threads();
    let saves = store.0.lock().unwrap().clone();
    for w in saves.windows(2) {
        let gap = w[1].0 - w[0].1;
        assert!(gap >= every, "next dump {gap:?} after the previous one ended");
    }
    rt.block_on(core.shutdown());
}

/// The `(name, allow_lookup)` calls a recording lookup saw.
type Calls = Arc<Mutex<Vec<(String, bool)>>>;

/// A `g_dns_lookup` that records each call and answers only lookups (as `getaddrinfo` with
/// `AI_NUMERICHOST` answers no name).
fn recording() -> (DnsLookupFn, Calls) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&calls);
    let lookup: DnsLookupFn = Arc::new(move |name: &str, allow: bool| {
        log.lock().unwrap().push((name.to_owned(), allow));
        if allow {
            vec![NetAddr::ipv4([31, 2, 3, 4]), NetAddr::ipv4([41, 5, 6, 7])]
        } else {
            Vec::new()
        }
    });
    (lookup, calls)
}

/// `-dns=0 -dnsseed=1`: the seed thread's `x<services>.<seed>` query is still a lookup and
/// fills AddrMan; a named `-addnode`/`-connect` target is not looked up.
#[test]
fn dns_seeds_look_up_with_dns_off() {
    let rt = runtime();
    let params = ChainParamsBuilder::new(NetworkKind::Mainnet, Release::V24_0).build().unwrap();
    let seed = params.dns_seeds[0].to_owned();
    let (lookup, calls) = recording();
    let resolver = SystemResolver {
        dns_lookup: lookup,
    };
    let core = core(&rt, NetworkKind::Mainnet, Arc::new(NoStore), resolver, |c| {
        c.dns_seed = true;
        c.name_lookup = false;
    });
    assert_eq!(rt.block_on(core.resolve_seed(&seed, 32)), 2);
    assert_eq!(core.addrman().size(None, None), 2);

    let named = OpenRequest {
        target: Target::Dest("node.example.org:9999".to_owned()),
        conn_type: ConnectionType::Manual,
        count_failure: false,
        grant: None,
        use_v2: false,
        masternode: false,
        masternode_probe: false,
    };
    assert!(matches!(rt.block_on(core.open(named)), Err(OpenError::Resolve(_))));
    let calls = calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(calls[0].0.starts_with('x') && calls[0].0.ends_with(&format!(".{seed}")) && calls[0].1);
    assert_eq!(calls[1], ("node.example.org".to_owned(), false));
    rt.block_on(core.shutdown());
}

/// The system's `getaddrinfo(AI_NUMERICHOST, SOCK_STREAM, IPPROTO_TCP)`: the first address, or
/// `None` on failure.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[allow(unsafe_code)]
fn getaddrinfo_numeric(name: &str) -> Option<NetAddr> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: plain data; all-zero is an empty hint.
    let mut hint: libc::addrinfo = unsafe { std::mem::zeroed() };
    hint.ai_socktype = libc::SOCK_STREAM;
    hint.ai_protocol = libc::IPPROTO_TCP;
    hint.ai_family = libc::AF_UNSPEC;
    hint.ai_flags = libc::AI_NUMERICHOST;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: NUL-terminated name, initialised hint, out pointer.
    if unsafe { libc::getaddrinfo(c.as_ptr(), std::ptr::null(), &hint, &mut res) } != 0 {
        return None;
    }
    // SAFETY: `res` is the non-empty list a successful call returned.
    let out = unsafe {
        let ai = &*res;
        match ai.ai_family {
            libc::AF_INET => {
                let sa = &*ai.ai_addr.cast::<libc::sockaddr_in>();
                Some(NetAddr::ipv4(sa.sin_addr.s_addr.to_ne_bytes()))
            }
            libc::AF_INET6 => {
                let sa = &*ai.ai_addr.cast::<libc::sockaddr_in6>();
                Some(
                    NetAddr::from_legacy_ipv6(sa.sin6_addr.s6_addr).with_scope_id(sa.sin6_scope_id),
                )
            }
            _ => None,
        }
    };
    // SAFETY: from a successful `getaddrinfo`.
    unsafe { libc::freeaddrinfo(res) };
    out
}

/// `-dns=0` decodes what glibc's `getaddrinfo(AI_NUMERICHOST)` decodes (Core's
/// `WrappedGetAddrInfo` without lookups on Linux), to the same address, and nothing else.
/// Interface-name scope ids are left out: glibc's answer depends on the host's interfaces
/// (D17). Other C libraries decode differently, so the oracle is glibc only.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
fn numeric_hosts_match_getaddrinfo() {
    let rt = runtime();
    let numeric = SystemResolver::default();
    let hosts = [
        "127.0.0.1",
        "127.1",
        "127.0.1",
        "0x7f000001",
        "0X7F000001",
        "2130706433",
        "0177.0.0.1",
        "0x7f.1",
        "1.2.3.4",
        "255.255.255.255",
        "4294967295",
        "4294967296",
        "127.0.0.256",
        "1.2.3.4.5",
        "0x",
        "0x.1.2.3",
        "08.1.2.3",
        "1..2",
        "1.2.3.",
        ".1.2.3",
        " 1.2.3.4",
        "1.2.3.4 ",
        "+1.2.3.4",
        "1.2.3.4%1",
        "::1",
        "::",
        "::ffff:1.2.3.4",
        "2001:db8::1",
        "2001:db8::1%2",
        "fe80::1%0",
        "fe80::1%",
        "fe80::1%-1",
        "fe80::1%+7",
        "fe80::1%4294967295",
        "fe80::1%4294967296",
        "1::2::3",
        "[::1]x",
        "::1:",
        "localhost",
        "example.org",
        "",
        "abc",
    ];
    for host in hosts {
        let want = getaddrinfo_numeric(host).filter(|a| !a.is_internal());
        let got = rt.block_on(numeric.lookup_host(host, 1, false));
        // The scope id too, which equality ignores (P4a D23).
        let scoped = |a: Option<&NetAddr>| a.map(|a| (a.clone(), a.scope_id()));
        assert_eq!(scoped(got.first()), scoped(want.as_ref()), "{host:?}");
    }
    assert!(rt.block_on(numeric.lookup_host("fe80::1%lo", 1, false)).is_empty());
}
