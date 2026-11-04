//! Interop with a real regtest dashd (PLAN P1 acceptance), in both directions:
//!
//! - we dial dashd over V2 and complete the `version`/`verack` handshake and a short-ID
//!   `ping`/`pong`; dashd reports the connection as `v2` with our session ID;
//! - dashd dials us (`addconnection … true`) and we answer as the V2 responder;
//! - dashd dials us over V1 (`addconnection … false`) and our V2 responder falls back;
//! - we dial a `-v2transport=0` dashd over V2, it drops us, and `should_reconnect_v1` sends us
//!   back over V1, as Core's `CConnman` does;
//! - the garbage lengths dashd sends as a responder and the ones we draw are both uniform on 0 to
//!   4095 (Kolmogorov-Smirnov, two-sample and against the uniform distribution).
//!
//! The dashd is v24 (DEC-82): `DASHD_V24_PATH`, else setup-dashd's cache for
//! `DASHVERSION=24.0.0-rc.5`; another release fails the tests. Set `SKIP_DASHD_TESTS=1` to
//! skip. Each test runs its own dashd under `TMPDIR` on free ports.

mod common;

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use common::regtest;
use dash_p2p_core::ServiceFlags;
use dash_p2p_transport::cipher::{
    Bip324Cipher, ELLSWIFT_KEY_LEN, GARBAGE_TERMINATOR_LEN, SecretKey,
};
use dash_p2p_transport::{
    CommandString, MAX_GARBAGE_LEN, NetMessage, OutMsg, ProtocolVersion, Transport, TransportKind,
    random_garbage,
};
use node_test_ports::free_port;
use serde_json::Value;

const TIMEOUT: Duration = Duration::from_secs(60);
/// How many port pairs a dashd start tries before a lost port race fails the test.
const START_ATTEMPTS: usize = 3;

fn cmd(s: &str) -> CommandString {
    let mut b = [0u8; 12];
    b[..s.len()].copy_from_slice(s.as_bytes());
    CommandString::from_bytes(b)
}

/// The dashd binary to test against, or `None` to skip.
fn dashd_binary() -> Option<PathBuf> {
    if std::env::var_os("SKIP_DASHD_TESTS").is_some() {
        eprintln!("SKIP_DASHD_TESTS is set: skipping the dashd interop tests");
        return None;
    }
    let cached = std::env::var_os("HOME")
        .map(|h| Path::new(&h).join(".rust-dashcore-test/dashcore-24.0.0-rc.5/bin/dashd"));
    let path = std::env::var_os("DASHD_V24_PATH")
        .map(PathBuf::from)
        .or(cached.filter(|p| p.exists()))
        .expect(
            "no dashd: run `DASHVERSION=24.0.0-rc.5 python3 contrib/setup-dashd.py`, set \
             DASHD_V24_PATH, or set SKIP_DASHD_TESTS=1",
        );
    assert!(path.exists(), "{} does not exist", path.display());
    Some(path)
}

/// A regtest dashd of our own, stopped and removed on drop.
struct Dashd {
    child: Child,
    cli: PathBuf,
    datadir: PathBuf,
    rpcport: u16,
    port: u16,
}

impl Dashd {
    /// Starts dashd on free ports. A port is free when picked but not reserved, so another
    /// process can bind it first; dashd then exits during init (BL-221) and is started again on
    /// fresh ports, up to [`START_ATTEMPTS`] times.
    fn start(name: &str, extra: &[&str]) -> Option<Self> {
        let bin = dashd_binary()?;
        let mut attempt = 1;
        let d = loop {
            match Self::try_start(&bin, name, extra) {
                Ok(d) => break d,
                Err(lost) if attempt < START_ATTEMPTS => {
                    eprintln!("{lost}; starting it again on other ports");
                    attempt += 1;
                }
                Err(lost) => panic!("{lost} ({START_ATTEMPTS} attempts)"),
            }
        };
        let version = d.version();
        assert!((240000..250000).contains(&version), "dashd {version} is not v24");
        Some(d)
    }

    /// Starts dashd once and waits until it answers RPC. `Err` when it exits because another
    /// process holds its P2P port or its RPC port (bound on 127.0.0.1 only, so that losing it
    /// fails init rather than leaving dashd on `::1`); any other exit fails the test.
    // Core: init.cpp:1661 (no RPC endpoint bound fails init)
    fn try_start(bin: &Path, name: &str, extra: &[&str]) -> Result<Self, String> {
        let datadir =
            std::env::temp_dir().join(format!("p2p-transport-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&datadir);
        std::fs::create_dir_all(&datadir).unwrap();
        let (port, rpcport) = (free_port(), free_port());
        let child = Command::new(bin)
            .arg("-regtest")
            .arg(format!("-datadir={}", datadir.display()))
            .arg(format!("-port={port}"))
            .arg(format!("-rpcport={rpcport}"))
            .args(["-rpcbind=127.0.0.1", "-rpcallowip=127.0.0.1"])
            .args(["-bind=127.0.0.1", "-listen=1", "-listenonion=0", "-dnsseed=0", "-fixedseeds=0"])
            .args(["-discover=0", "-natpmp=0", "-server=1", "-printtoconsole=0", "-debug=net"])
            .args(extra)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut d = Self {
            child,
            cli: bin.with_file_name("dash-cli"),
            datadir,
            rpcport,
            port,
        };
        let deadline = Instant::now() + TIMEOUT;
        loop {
            // Before each call: once dashd lost its RPC port, the call reaches the port's owner.
            if d.child.try_wait().unwrap().is_some() {
                let log = std::fs::read_to_string(d.datadir.join("regtest").join("debug.log"))
                    .unwrap_or_default();
                let lost_lines = [
                    // Core: net.cpp:3817
                    format!("Unable to bind to 127.0.0.1:{port} on this computer."),
                    // Core: net.cpp:3827 (bound beside another SO_REUSEADDR socket, which
                    // listened first); the P2P listener is the only one, as `-bind` is given.
                    "Listening for incoming connections failed (listen returned error".to_owned(),
                    // Core: httpserver.cpp:433
                    format!("Binding RPC on address 127.0.0.1 port {rpcport} failed."),
                ];
                return match lost_lines.into_iter().find(|line| log.contains(line)) {
                    Some(line) => Err(format!("dashd {name} lost a port: {line}")),
                    None => panic!("dashd exited during init; see {}", d.datadir.display()),
                };
            }
            if d.try_cli(&["getnetworkinfo"]).is_some() {
                return Ok(d);
            }
            assert!(Instant::now() < deadline, "dashd did not start; see {}", d.datadir.display());
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn try_cli(&self, args: &[&str]) -> Option<String> {
        let out = Command::new(&self.cli)
            .arg("-regtest")
            .arg(format!("-datadir={}", self.datadir.display()))
            .arg(format!("-rpcport={}", self.rpcport))
            // Bounded: a process that took a lost RPC port may accept and never answer.
            .arg(format!("-rpcclienttimeout={}", TIMEOUT.as_secs()))
            .args(args)
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8(out.stdout).unwrap())
    }

    fn rpc(&self, args: &[&str]) -> Value {
        let out = self.try_cli(args).unwrap_or_else(|| panic!("dash-cli {args:?} failed"));
        serde_json::from_str(&out).unwrap_or(Value::String(out.trim().to_owned()))
    }

    fn version(&self) -> i64 {
        self.rpc(&["getnetworkinfo"])["version"].as_i64().unwrap()
    }

    /// The `getpeerinfo` entry whose `field` is `value`, once its transport type is `kind`.
    fn peer(&self, field: &str, value: &str, kind: &str) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let peers = self.rpc(&["getpeerinfo"]);
            let found = peers.as_array().unwrap().iter().find(|p| p[field] == value).cloned();
            if let Some(p) = found.filter(|p| p["transport_protocol_type"] == kind) {
                return p;
            }
            assert!(Instant::now() < deadline, "no {kind} peer with {field} {value}: {peers:#}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Dashd {
    fn drop(&mut self) {
        // Only a running dashd: once it exited, its RPC port may be another process's.
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.try_cli(&["stop"]);
        }
        let deadline = Instant::now() + TIMEOUT;
        while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.datadir);
        }
    }
}

/// A socket driven through a transport.
struct Conn {
    sock: TcpStream,
    t: Transport,
    inbox: VecDeque<NetMessage>,
}

impl Conn {
    fn new(sock: TcpStream, t: Transport) -> Self {
        sock.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        Self {
            sock,
            t,
            inbox: VecDeque::new(),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        loop {
            let data = self.t.bytes_to_send(false).data;
            if data.is_empty() {
                return Ok(());
            }
            let n = self.sock.write(data)?;
            self.t.mark_bytes_sent(n);
        }
    }

    /// Sends what is pending and feeds what arrives; `Err` on EOF or a transport error.
    fn pump(&mut self) -> std::io::Result<()> {
        self.flush()?;
        let mut buf = [0u8; 65536];
        match self.sock.read(&mut buf) {
            Ok(0) => Err(ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                let inbox = &mut self.inbox;
                self.t
                    .receive(&buf[..n], 0, |m| {
                        inbox.push_back(m.expect("dashd sends valid messages"))
                    })
                    .map_err(|e| std::io::Error::new(ErrorKind::InvalidData, e))
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn send(&mut self, command: &str, payload: Vec<u8>) {
        let mut msg = OutMsg::new(cmd(command), payload);
        let deadline = Instant::now() + TIMEOUT;
        while let Err(m) = self.t.set_message_to_send(msg) {
            msg = m;
            self.pump().unwrap();
            assert!(Instant::now() < deadline, "transport never took {command}");
        }
        self.flush().unwrap();
    }

    /// The next message of type `command`; others are dropped.
    fn expect(&mut self, command: &str) -> NetMessage {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            while let Some(m) = self.inbox.pop_front() {
                if m.command == cmd(command) {
                    return m;
                }
            }
            self.pump().unwrap();
            assert!(Instant::now() < deadline, "no {command} from dashd");
        }
    }

    /// The `version`/`verack` exchange. `we_start` sends our `version` first (we dialled);
    /// otherwise we answer dashd's. The common version then gates short IDs, as in Core.
    fn handshake(&mut self, we_start: bool) {
        if we_start {
            self.send("version", version_payload());
        }
        let theirs = self.expect("version");
        let their_version = i32::from_le_bytes(theirs.payload[..4].try_into().unwrap());
        self.t.set_peer_version(ProtocolVersion(their_version.min(ProtocolVersion::V24_0.0)));
        if !we_start {
            self.send("version", version_payload());
        }
        self.send("verack", Vec::new());
        self.expect("verack");
    }

    /// A `ping` and its `pong`; returns the `pong`'s wire size.
    fn ping(&mut self) -> usize {
        let nonce = 0x0123_4567_89ab_cdef_u64.to_le_bytes().to_vec();
        self.send("ping", nonce.clone());
        let pong = self.expect("pong");
        assert_eq!(pong.payload, nonce);
        pong.raw_size
    }
}

/// Our `version` (Core's `PushNodeVersion` layout for 70242).
fn version_payload() -> Vec<u8> {
    let services = (ServiceFlags::NODE_NETWORK
        | ServiceFlags::NODE_BLOOM
        | ServiceFlags::NODE_HEADERS_COMPRESSED
        | ServiceFlags::NODE_P2P_V2)
        .0;
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64;
    let addr = |p: &mut Vec<u8>| {
        p.extend_from_slice(&0u64.to_le_bytes());
        p.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 127, 0, 0, 1]);
        p.extend_from_slice(&0u16.to_be_bytes());
    };
    let mut p = Vec::new();
    p.extend_from_slice(&ProtocolVersion::V24_0.0.to_le_bytes());
    p.extend_from_slice(&services.to_le_bytes());
    p.extend_from_slice(&now.to_le_bytes());
    addr(&mut p);
    addr(&mut p);
    p.extend_from_slice(&0x5eed_0fd1_u64.to_le_bytes()); // nonce
    let agent = b"/dash-p2p-transport:test/";
    p.push(agent.len() as u8);
    p.extend_from_slice(agent);
    p.extend_from_slice(&0i32.to_le_bytes()); // start height
    p.push(1); // relay
    p.extend_from_slice(&[7; 32]); // mnauth challenge
    p.push(0); // not a masternode connection
    p
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn dial(d: &Dashd, t: Transport) -> Conn {
    let sock = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    sock.set_nodelay(true).unwrap();
    Conn::new(sock, t)
}

/// Waits for dashd's `addconnection` to reach `listener`.
fn accept(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match listener.accept() {
            Ok((sock, _)) => {
                sock.set_nonblocking(false).unwrap();
                return sock;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "dashd never connected");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn we_dial_dashd_over_v2() {
    let Some(d) = Dashd::start("out-v2", &[]) else {
        return;
    };
    eprintln!("dashd version {}", d.version());
    let mut c = dial(&d, Transport::v2(regtest(), true).unwrap());
    c.handshake(true);
    assert_eq!(c.t.info().kind, TransportKind::V2);
    // A short-ID pong: 1 type byte, the 8-byte nonce, and the 20 bytes of BIP324 expansion.
    assert_eq!(c.ping(), 1 + 8 + 20);
    let ours = c.sock.local_addr().unwrap().to_string();
    let peer = d.peer("addr", &ours, "v2");
    assert_eq!(peer["session_id"], hex(&c.t.info().session_id.unwrap()));
    assert_eq!(peer["inbound"], true);
}

#[test]
fn dashd_dials_us_over_v2() {
    let Some(d) = Dashd::start("in-v2", &[]) else {
        return;
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    d.rpc(&["addconnection", &addr, "outbound-full-relay", "true"]);
    let mut c = Conn::new(accept(&listener), Transport::v2(regtest(), false).unwrap());
    c.handshake(false);
    assert_eq!(c.t.info().kind, TransportKind::V2);
    assert_eq!(c.ping(), 1 + 8 + 20);
    let peer = d.peer("addr", &addr, "v2");
    assert_eq!(peer["session_id"], hex(&c.t.info().session_id.unwrap()));
    assert_eq!(peer["connection_type"], "outbound-full-relay");
}

#[test]
fn dashd_dials_us_over_v1_and_we_detect_it() {
    let Some(d) = Dashd::start("in-v1", &[]) else {
        return;
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    d.rpc(&["addconnection", &addr, "outbound-full-relay", "false"]);
    let mut c = Conn::new(accept(&listener), Transport::v2(regtest(), false).unwrap());
    c.handshake(false);
    assert_eq!(c.t.info().kind, TransportKind::V1);
    // V1: a 24-byte header and the nonce.
    assert_eq!(c.ping(), 24 + 8);
    d.peer("addr", &addr, "v1");
}

#[test]
fn v1_only_dashd_makes_us_reconnect_over_v1() {
    let Some(d) = Dashd::start("out-v1", &["-v2transport=0"]) else {
        return;
    };
    let mut c = dial(&d, Transport::v2(regtest(), true).unwrap());
    // Our key and garbage reach a V1 node, which drops the connection on the bad magic.
    let deadline = Instant::now() + TIMEOUT;
    let err = loop {
        if let Err(e) = c.pump() {
            break e;
        }
        assert!(Instant::now() < deadline, "the V1 dashd kept the connection");
    };
    assert!(matches!(err.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset), "{err}");
    assert!(c.t.should_reconnect_v1());
    let mut c = dial(&d, Transport::v1(regtest()));
    c.handshake(true);
    assert_eq!(c.ping(), 24 + 8);
    let ours = c.sock.local_addr().unwrap().to_string();
    d.peer("addr", &ours, "v1");
}

/// One garbage length from dashd as the V2 responder: our key, then its key, garbage and
/// terminator.
fn dashd_garbage_len(d: &Dashd) -> usize {
    let key = loop {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).unwrap();
        if let Ok(k) = SecretKey::from_secret_bytes(b) {
            break k;
        }
    };
    let mut ent = [0u8; 32];
    getrandom::fill(&mut ent).unwrap();
    let mut cipher = Bip324Cipher::new(regtest(), key, ent);
    let mut sock = TcpStream::connect(("127.0.0.1", d.port)).unwrap();
    sock.set_read_timeout(Some(TIMEOUT)).unwrap();
    sock.write_all(cipher.our_pubkey()).unwrap();
    let mut got = Vec::new();
    let mut buf = [0u8; 8192];
    while got.len() < ELLSWIFT_KEY_LEN {
        let n = sock.read(&mut buf).unwrap();
        assert!(n > 0, "dashd closed before its key");
        got.extend_from_slice(&buf[..n]);
    }
    cipher.initialize(&got[..ELLSWIFT_KEY_LEN].try_into().unwrap(), true, false);
    let term = *cipher.recv_garbage_terminator();
    loop {
        let rest = &got[ELLSWIFT_KEY_LEN..];
        if let Some(len) = rest.windows(GARBAGE_TERMINATOR_LEN).position(|w| w == term) {
            return len;
        }
        assert!(rest.len() <= MAX_GARBAGE_LEN + GARBAGE_TERMINATOR_LEN, "no terminator");
        let n = sock.read(&mut buf).unwrap();
        assert!(n > 0, "dashd closed before its terminator");
        got.extend_from_slice(&buf[..n]);
    }
}

/// The Kolmogorov-Smirnov statistic between two samples of lengths in 0 to 4095.
fn ks_two(a: &[usize], b: &[usize]) -> f64 {
    let cdf = |s: &[usize], x: usize| s.iter().filter(|&&v| v <= x).count() as f64 / s.len() as f64;
    (0..=MAX_GARBAGE_LEN).map(|x| (cdf(a, x) - cdf(b, x)).abs()).fold(0.0, f64::max)
}

/// The statistic against the uniform distribution on 0 to 4095.
fn ks_uniform(a: &[usize]) -> f64 {
    let mut counts = vec![0usize; MAX_GARBAGE_LEN + 1];
    a.iter().for_each(|&v| counts[v] += 1);
    let mut seen = 0;
    let mut d: f64 = 0.0;
    for (x, c) in counts.iter().enumerate() {
        seen += c;
        d = d.max((seen as f64 / a.len() as f64 - (x + 1) as f64 / 4096.0).abs());
    }
    d
}

#[test]
fn garbage_lengths_match_dashd() {
    let Some(d) = Dashd::start("garbage", &[]) else {
        return;
    };
    const N: usize = 1000;
    let theirs: Vec<usize> = (0..N).map(|_| dashd_garbage_len(&d)).collect();
    let ours: Vec<usize> = (0..N).map(|_| random_garbage().unwrap().len()).collect();
    assert!(theirs.iter().chain(&ours).all(|&l| l <= MAX_GARBAGE_LEN));
    // Critical values for a false-failure rate of 1e-6: c = sqrt(-ln(5e-7) / 2) = 2.70.
    let c = (-(0.5e-6f64).ln() / 2.0).sqrt();
    let n = N as f64;
    let (two, d_theirs, d_ours) = (ks_two(&theirs, &ours), ks_uniform(&theirs), ks_uniform(&ours));
    eprintln!(
        "KS: two-sample {two:.4}, dashd vs uniform {d_theirs:.4}, ours vs uniform {d_ours:.4}"
    );
    assert!(two <= c * (2.0 / n).sqrt(), "our garbage lengths differ from dashd's: D = {two}");
    assert!(d_theirs <= c / n.sqrt(), "dashd's lengths are not uniform: D = {d_theirs}");
    assert!(d_ours <= c / n.sqrt(), "our lengths are not uniform: D = {d_ours}");
}
