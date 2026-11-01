//! Tor control over a real connection to a local mock control port (P8b): the session's bytes,
//! what it changes in the network core, the reconnection after the port closes, and the loop's
//! two ways to end. Nothing leaves the host: the mock listens on loopback.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_p2p::netaddr::{NetAddr, Network};
use dash_p2p::store::NoStore;
use dash_p2p::{NetConfig, NetCore, NetEvents, PeerHandle, Service, ServiceFlags};
use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};
use dashd_net::SystemResolver;
use dashd_net::torcontrol::{self, CoreTorHost, TorFiles, TorOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

struct Quiet;

impl NetEvents for Quiet {
    fn wake(&self) {}
    fn initialize(&self, _: &NetCore, _: &PeerHandle, _: ServiceFlags) {}
    fn finalize(&self, _: &NetCore, _: &PeerHandle) {}
}

fn core() -> Arc<NetCore> {
    let params =
        Arc::new(ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap());
    let mut cfg = NetConfig::new(params, ServiceFlags::NODE_NETWORK);
    cfg.dns_seed = false;
    NetCore::new(
        cfg,
        tokio::runtime::Handle::current(),
        Arc::new(NoStore),
        Arc::new(SystemResolver::default()),
        Arc::new(dash_clock::SystemClock),
        Arc::new(Quiet),
    )
    .unwrap()
}

#[derive(Default)]
struct Files {
    key: Mutex<Option<Vec<u8>>>,
}

impl TorFiles for Files {
    fn onion_allowed_by_onlynet(&self) -> bool {
        true
    }

    fn read_private_key(&self) -> Option<Vec<u8>> {
        self.key.lock().unwrap().clone()
    }

    fn write_private_key(&self, key: &[u8]) -> bool {
        *self.key.lock().unwrap() = Some(key.to_vec());
        true
    }

    fn private_key_path(&self) -> String {
        "onion_v3_private_key".into()
    }
}

const ONION_ID: &str = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd";

/// Every connection's command lines, in order.
type Sessions = Arc<Mutex<Vec<Vec<String>>>>;

/// A Tor control port that offers NULL authentication and grants everything; each connection
/// closes after `ADD_ONION` when `close_after_onion`.
async fn mock_tor(close_after_onion: bool) -> (Sessions, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap().to_string();
    let sessions = Sessions::default();
    let recorded = Arc::clone(&sessions);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let n = {
                let mut s = recorded.lock().unwrap();
                s.push(Vec::new());
                s.len() - 1
            };
            let (read, mut write) = sock.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                recorded.lock().unwrap()[n].push(line.clone());
                let reply = if line == "PROTOCOLINFO 1" {
                    "250-PROTOCOLINFO 1\r\n250-AUTH METHODS=NULL\r\n250-VERSION Tor=\"0.4.8.13\"\r\n250 OK\r\n".to_owned()
                } else if line == "AUTHENTICATE" {
                    "250 OK\r\n".to_owned()
                } else if line == "GETINFO net/listeners/socks" {
                    "250-net/listeners/socks=\"127.0.0.1:19050\"\r\n250 OK\r\n".to_owned()
                } else if line.starts_with("ADD_ONION ") {
                    format!(
                        "250-ServiceID={ONION_ID}\r\n250-PrivateKey=ED25519-V3:S0VZ\r\n250 OK\r\n"
                    )
                } else {
                    "510 Unrecognized command\r\n".to_owned()
                };
                write.write_all(reply.as_bytes()).await.unwrap();
                if close_after_onion && line.starts_with("ADD_ONION ") {
                    // Long enough for the test to see the service advertised.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    break;
                }
            }
        }
    });
    (sessions, at)
}

fn opts(control: &str) -> TorOptions {
    TorOptions {
        control: control.into(),
        password: String::new(),
        ask_socks_port: true,
        default_port: 19899,
        target: Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19896),
    }
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    for _ in 0..1000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

fn onion_local(core: &NetCore) -> bool {
    core.local_addresses().iter().any(|(a, ..)| a.is_tor())
}

/// The session the mock records; Tor becomes reachable through the proxy Tor names, the service
/// is a local address and its key is cached. When the port closes the service is withdrawn and
/// the controller reconnects after a second, sending the cached key.
// Core: torcontrol.cpp:347-471, :611-657
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_then_a_reconnection() {
    let (sessions, at) = mock_tor(true).await;
    let core = core();
    core.set_reachable(Network::Onion, false);
    let files = Arc::new(Files::default());
    let host = Arc::new(CoreTorHost::new(Arc::clone(&core), Arc::clone(&files)));
    let tor = torcontrol::start(
        &tokio::runtime::Handle::current(),
        Arc::clone(&core),
        opts(&at),
        host,
        false,
    );
    wait_for("the onion service", || onion_local(&core)).await;
    assert!(core.is_reachable(Network::Onion));
    let proxy = core.proxy(Network::Onion).unwrap();
    assert_eq!((proxy.to_string(), proxy.randomize_credentials), ("127.0.0.1:19050".into(), true));
    wait_for("the withdrawal", || !onion_local(&core)).await;
    wait_for("the second session", || {
        sessions.lock().unwrap().len() == 2 && sessions.lock().unwrap()[1].len() == 4
    })
    .await;
    let sessions = sessions.lock().unwrap().clone();
    let add = "ADD_ONION NEW:ED25519-V3 Port=19899,127.0.0.1:19896";
    let common = ["PROTOCOLINFO 1", "AUTHENTICATE", "GETINFO net/listeners/socks"];
    assert_eq!(sessions[0], [&common[..], &[add]].concat());
    assert_eq!(
        sessions[1],
        [&common[..], &["ADD_ONION ED25519-V3:S0VZ Port=19899,127.0.0.1:19896"]].concat()
    );
    assert_eq!(files.key.lock().unwrap().as_deref(), Some(&b"ED25519-V3:S0VZ"[..]));
    tor.interrupt();
    tor.stop().await;
    assert!(!onion_local(&core), "stopping withdraws the service");
}

/// An interrupt ends the loop, and the controller's end withdraws the service it advertised.
// Core: torcontrol.cpp:336-345, :689-706
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_withdraws_the_service() {
    let (_, at) = mock_tor(false).await;
    let core = core();
    let host = Arc::new(CoreTorHost::new(Arc::clone(&core), Arc::new(Files::default())));
    let tor = torcontrol::start(
        &tokio::runtime::Handle::current(),
        Arc::clone(&core),
        opts(&at),
        host,
        false,
    );
    wait_for("the onion service", || onion_local(&core)).await;
    assert!(!tor.is_finished());
    tor.interrupt();
    tor.stop().await;
    assert!(!onion_local(&core));
}

/// The loop ends by itself when nothing is left to wait for: a control port that cannot be
/// looked up (no reconnection is scheduled), or a line over `MAX_LINE_LENGTH` (the connection
/// is freed without its disconnect callback).
// Core: torcontrol.cpp:113-119, :139-149, :315-327
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_loop_ends_with_nothing_to_wait_for() {
    let core = core();
    let host = Arc::new(CoreTorHost::new(Arc::clone(&core), Arc::new(Files::default())));
    let handle = tokio::runtime::Handle::current();
    let tor =
        torcontrol::start(&handle, Arc::clone(&core), opts("tor.invalid"), host.clone(), false);
    wait_for("the end after a failed lookup", || tor.is_finished()).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap().to_string();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        sock.write_all(&vec![b'2'; 100_001]).await.unwrap();
        tokio::time::sleep(Duration::from_secs(60)).await;
    });
    let tor = torcontrol::start(&handle, core, opts(&at), host, false);
    wait_for("the end after a long line", || tor.is_finished()).await;
}

/// Over TCP, a line of exactly `MAX_LINE_LENGTH` keeps the connection up and, once it ends,
/// `PROTOCOLINFO`'s reply is answered (the unit test `a_line_of_the_maximum_length_is_kept` pins
/// the boundary whatever the reads' sizes).
// Core: torcontrol.cpp:114-118
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_line_of_the_maximum_length_over_tcp() {
    let core = core();
    let host = Arc::new(CoreTorHost::new(Arc::clone(&core), Arc::new(Files::default())));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let at = listener.local_addr().unwrap().to_string();
    let tor = torcontrol::start(&tokio::runtime::Handle::current(), core, opts(&at), host, false);
    let (mut sock, _) = listener.accept().await.unwrap();
    sock.write_all(&vec![b'2'; 100_000]).await.unwrap();
    // A continuation line (its fourth byte is not a space), then the rest of the reply.
    sock.write_all(b"\r\n250-AUTH METHODS=NULL\r\n250 OK\r\n").await.unwrap();
    let mut got = Vec::new();
    let mut buf = [0; 256];
    while !got.ends_with(b"AUTHENTICATE\r\n") {
        let n = tokio::time::timeout(Duration::from_secs(30), sock.read(&mut buf))
            .await
            .expect("AUTHENTICATE")
            .unwrap();
        assert_ne!(n, 0, "closed after {:?}", String::from_utf8_lossy(&got));
        got.extend_from_slice(&buf[..n]);
    }
    assert_eq!(got, b"PROTOCOLINFO 1\r\nAUTHENTICATE\r\n");
    tor.interrupt();
    wait_for("the end after the interrupt", || tor.is_finished()).await;
    tor.stop().await;
}

/// A connect that fails at once (the broadcast address is unreachable) leaves the loop waiting
/// for the interrupt, with no reconnection (`tor_diff` checks the log lines against dashd).
// Core: torcontrol.cpp:168-172, :315-327
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_connect_waits_for_the_interrupt() {
    let core = core();
    let host = Arc::new(CoreTorHost::new(Arc::clone(&core), Arc::new(Files::default())));
    let handle = tokio::runtime::Handle::current();
    let tor = torcontrol::start(&handle, core, opts("255.255.255.255:9051"), host, false);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!tor.is_finished());
    tor.interrupt();
    wait_for("the end after the interrupt", || tor.is_finished()).await;
    tor.stop().await;
}
