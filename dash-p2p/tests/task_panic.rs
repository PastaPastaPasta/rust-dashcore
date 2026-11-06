//! A panic on the core's own tasks ends the process, as Core's `TraceThread` rethrows out of
//! the socket thread (util/thread.cpp:16-27; D44). Each scenario runs in a child process: a
//! host callback that panics on the sweep (`finalize`) or on a peer's reader (`wake`) must
//! abort it, not leave the task dead.

mod common;

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use common::{Events, Parts, attach, core, core_with};
use dash_p2p::{CommandString, MemSocket, NetCore, NetEvents, OutMsg, PeerHandle, ServiceFlags};

const CHILD: &str = "DASH_P2P_TASK_PANIC_CHILD";
/// `SIGABRT` on Linux and macOS.
#[cfg(unix)]
const SIGABRT: i32 = 6;

struct Panicking;

impl NetEvents for Panicking {
    fn wake(&self) {
        panic!("host wake failed");
    }

    fn initialize(&self, _core: &NetCore, _peer: &PeerHandle, _our_services: ServiceFlags) {}

    fn finalize(&self, _core: &NetCore, _peer: &PeerHandle) {
        panic!("host finalize failed");
    }
}

fn panicking_core() -> Arc<NetCore> {
    core_with(
        Parts {
            events: Arc::new(Panicking),
            ..Default::default()
        },
        |_| {},
    )
}

/// The sweep: a peer closes, the sweep removes it and the host's `finalize` panics there.
async fn sweep_child() {
    let core = panicking_core();
    let (ours, theirs) = MemSocket::pair();
    attach(&core, ours, [10, 0, 0, 1]);
    theirs.close();
    outlive_the_panic().await;
}

/// A reader: the far end is another core's peer, whose `ping` reaches a reader whose `wake`
/// panics.
async fn reader_child() {
    let core = panicking_core();
    let other = core_with(
        Parts {
            events: Arc::new(Events),
            ..Default::default()
        },
        |_| {},
    );
    let (ours, theirs) = MemSocket::pair();
    attach(&core, ours, [10, 0, 0, 1]);
    let far = attach(&other, theirs, [10, 0, 0, 2]);
    other.push(&far, OutMsg::new(CommandString::from_static("ping"), vec![0; 8]));
    outlive_the_panic().await;
}

/// Keeps the cores alive (dropping the last handle aborts their tasks): the panic must end the
/// process before this returns.
async fn outlive_the_panic() {
    tokio::time::sleep(Duration::from_secs(10)).await;
}

/// Runs this test binary's `name` with the child variable set, and checks it died of `SIGABRT`
/// after printing `message`.
fn assert_child_aborts(name: &str, message: &str) {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the child survived the panic: {stderr}");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(SIGABRT), "{:?}\n{stderr}", out.status);
    }
    assert!(stderr.contains(message), "{stderr}");
}

#[tokio::test]
async fn a_panic_on_the_sweep_aborts() {
    if std::env::var_os(CHILD).is_some() {
        sweep_child().await;
        return;
    }
    assert_child_aborts("a_panic_on_the_sweep_aborts", "host finalize failed");
}

#[tokio::test]
async fn a_panic_on_a_reader_aborts() {
    if std::env::var_os(CHILD).is_some() {
        reader_child().await;
        return;
    }
    assert_child_aborts("a_panic_on_a_reader_aborts", "host wake failed");
}

/// `core` without panicking callbacks is the control: nothing aborts.
#[tokio::test]
async fn a_quiet_core_runs_on() {
    let core = core();
    let (ours, theirs) = MemSocket::pair();
    attach(&core, ours, [10, 0, 0, 1]);
    theirs.close();
    common::run_until(|| core.peers().is_empty()).await;
}
