//! BL-327 D21: a failed `accept` is Core's category-free `LogPrintf` line. Its own test binary,
//! because it lowers the process's descriptor limit so that the kernel refuses `accept`
//! (`EMFILE`).
#![cfg(target_os = "linux")]

use std::fmt;
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use dash_p2p::conn::Permissions;
use dash_p2p::store::NoStore;
use dash_p2p::{NetConfig, NetCore, NetEvents, PeerHandle, ServiceFlags};
use dash_params::{ChainParamsBuilder, NetworkKind, Release};
use dashd_net::SystemResolver;
use dashd_net::binds::{Listener, accept_loop};
use socket2::{Domain, Socket, Type};
use tokio::sync::watch;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, span};

const ACCEPT_FAILED: &str = "socket error accept failed: ";

/// The accept-failure lines as (level, target, message).
static LINES: Mutex<Vec<(Level, String, String)>> = Mutex::new(Vec::new());

/// Keeps [`ACCEPT_FAILED`]'s lines in [`LINES`].
struct AcceptLines;

impl tracing::Subscriber for AcceptLines {
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
        if m.0.starts_with(ACCEPT_FAILED) {
            let meta = e.metadata();
            LINES.lock().unwrap().push((*meta.level(), meta.target().to_owned(), m.0));
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

fn nofile() -> libc::rlimit {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) }, 0);
    lim
}

fn set_nofile(lim: &libc::rlimit) {
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, lim) }, 0);
}

/// Puts the descriptor limit back when dropped, a failed assertion included.
struct Restore(libc::rlimit);

impl Drop for Restore {
    fn drop(&mut self) {
        set_nofile(&self.0);
    }
}

/// With no descriptor left, the accept loop logs Core's line: `LogPrintf` (info, no category,
/// so not on the `net` target) with `NetworkErrorString`'s "message (code)" form.
// Core: net.cpp:1853-1857 CConnman::AcceptConnection
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_failed_accept_logs_cores_line() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| tracing::subscriber::set_global_default(AcceptLines).unwrap());
    let params =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.listen = true;
    cfg.dns_seed = false;
    let core = NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(SystemResolver::default()),
        Arc::new(dash_clock::SystemClock),
        Arc::new(Quiet),
    )
    .unwrap();
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    let listener = Listener {
        socket,
        permissions: Permissions::NONE,
        onion_binds: Arc::new([]),
    };
    let (stop, stopped) = watch::channel(false);
    let task = tokio::spawn(accept_loop(Arc::clone(&core), listener, Arc::new(|| false), stopped));
    // The client's descriptor exists before the limit drops; `connect` needs none.
    let client = Socket::new(Domain::IPV4, Type::STREAM, None).unwrap();
    let was = Restore(nofile());
    // `F_DUPFD` from 0 returns the lowest free descriptor: with the limit there, none is left.
    let lowest = unsafe { libc::fcntl(client.as_raw_fd(), libc::F_DUPFD, 0) };
    assert!(lowest >= 0);
    unsafe { libc::close(lowest) };
    set_nofile(&libc::rlimit {
        rlim_cur: lowest as libc::rlim_t,
        rlim_max: was.0.rlim_max,
    });
    let connected = client.connect(&addr.into());
    let deadline = Instant::now() + Duration::from_secs(30);
    while LINES.lock().unwrap().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    stop.send(true).unwrap();
    task.await.unwrap();
    drop(was);
    connected.unwrap();
    let lines = LINES.lock().unwrap().clone();
    let want = (
        Level::INFO,
        "dashd_net::binds".to_owned(),
        format!("{ACCEPT_FAILED}Too many open files (24)"),
    );
    assert_eq!(lines.first(), Some(&want), "{lines:?}");
    core.interrupt();
}
