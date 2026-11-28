//! DK1-L4: rdashd's DKG runtime on setup-dashd's masternode network (a controller and four
//! masternodes, dashd at `DASHD_PATH`), against Core's feature_llmq_dkg_intake.py and
//! feature_llmq_simplepose.py `test_no_service` at v24.0.0. rdashd runs in this process, as a
//! `-watchquorums` observer, or as one of the network's masternodes: `new_leaving_out` starts the
//! other three, and rdashd takes the fourth's operator key and registered address, so it is a DKG
//! member whenever `llmq_test` (size 3 of 4) selects it. Raw P2P peers stand in for Core's
//! `P2PInterface` peers, and rdashd's `mnauth` RPC marks them verified as Core's test does.
//!
//! Needs setup-dashd's environment (`DASHD_PATH`, `DASHD_MN_DATADIR`); `SKIP_DASHD_TESTS` skips.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_consensus::hash::sha256d;
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_spv::test_utils::{MasternodeTestContext, retain_test_dir};
use dashd_masternode::sync::{MASTERNODE_SYNC_TICK_SECONDS, MASTERNODE_SYNC_TIMEOUT_SECONDS};
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(180);
/// Regtest's message start (`chainparams.cpp`, CRegTestParams `pchMessageStart`).
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// Core v24.0.0's `PROTOCOL_VERSION` and `MIN_MASTERNODE_PROTO_VERSION` (version.h).
const VERSION: i32 = 70242;
const LLMQ_TEST: u8 = 100;
/// `MSG_QUORUM_CONTRIB` (protocol.h).
const MSG_QUORUM_CONTRIB: u32 = 23;
const NODE_NETWORK: u64 = 1;
const NODE_COMPACT_FILTERS: u64 = 1 << 6;
/// `SPORK_23_QUORUM_POSE` off: a far-future activation time.
const SPORK_OFF: i64 = 4_070_908_800;

// Core: test/functional/feature_llmq_dkg_intake.py:48-53 at v24.0.0
const FAKE_PROTX: &str = "cecf37bf0ec05d2d22cb8227f88074bb882b94cd2081ba318a5a444b1b15b9fd";
const FAKE_PUBKEY: &str = "8e7afdb849e5e2a085b035b62e21c0940c753f2d4501325743894c37162f287bccaffbedd60c36581dabbf127a22e43f";
const DKG_PUSH_TYPES: [&str; 4] = ["qcontrib", "qcomplaint", "qjustify", "qpcommit"];
const INVALID_NONZERO_BLS_PUBKEY: [u8; 48] = [0xff; 48];

/// One rdashd per process at a time: they share the process's logger.
static SERIAL: Mutex<()> = Mutex::new(());

fn logger() -> Arc<Logger> {
    static LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();
    Arc::clone(LOGGER.get_or_init(|| {
        let logger = Arc::new(Logger::new());
        tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)))
            .expect("the only global subscriber");
        logger
    }))
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// `ser_uint256(int(h, 16))`: a hash's display hex as its wire bytes.
fn wire_hash(display: &str) -> Vec<u8> {
    hex(display).into_iter().rev().collect()
}

fn compact_size(n: usize) -> Vec<u8> {
    assert!(n < 0xfd);
    vec![n as u8]
}

fn rand_nonce() -> u64 {
    use std::hash::{BuildHasher as _, RandomState};
    RandomState::new().hash_one(Instant::now())
}

/// One `inv` or `getdata` vector of a single item.
fn inventory(kind: u32, hash: &[u8; 32]) -> Vec<u8> {
    let mut v = vec![1u8];
    v.extend(kind.to_le_bytes());
    v.extend(hash);
    v
}

/// `quorum_hash_prefix`: llmqType and the quorum hash, the prefix of every DKG message.
// Core: test/functional/feature_llmq_dkg_intake.py:133-137 at v24.0.0
fn prefix(quorum: &str) -> Vec<u8> {
    let mut r = vec![LLMQ_TEST];
    r.extend(wire_hash(quorum));
    r
}

/// `qcontrib_payload`: a `CDKGContribution` with `blob_count` encrypted blobs.
// Core: test/functional/feature_llmq_dkg_intake.py:139-152 at v24.0.0
fn qcontrib(quorum: &str, blob_count: usize, vvec_pubkey: &[u8], pro_tx: u64) -> Vec<u8> {
    let valid = hex(FAKE_PUBKEY);
    let mut protx = [0u8; 32];
    protx[..8].copy_from_slice(&pro_tx.to_le_bytes());
    let mut r = prefix(quorum);
    r.extend(protx);
    r.extend(compact_size(2));
    r.extend(vvec_pubkey);
    r.extend(&valid);
    r.extend(&valid);
    r.extend([0u8; 32]);
    r.extend(compact_size(blob_count));
    for _ in 0..blob_count {
        r.extend(compact_size(32));
        r.extend([0u8; 32]);
    }
    r.extend([0u8; 96]);
    r
}

/// A raw v1 P2P peer.
struct Peer {
    stream: TcpStream,
    name: String,
}

impl Peer {
    fn connect(name: &str, port: u16, time: i64) -> Peer {
        Peer::connect_as(name, port, time, 0, true, false)
    }

    /// Connects to `port` and completes the version handshake at mocktime `time`, advertising
    /// `services`, `relay` and Dash's masternode flag (`fOtherMasternode`).
    fn connect_as(
        name: &str,
        port: u16,
        time: i64,
        services: u64,
        relay: bool,
        masternode: bool,
    ) -> Peer {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("rdashd listens");
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut peer = Peer {
            stream,
            name: name.to_owned(),
        };
        let mut version = Vec::new();
        version.extend(VERSION.to_le_bytes());
        version.extend(services.to_le_bytes());
        version.extend(time.to_le_bytes());
        for _ in 0..2 {
            version.extend(0u64.to_le_bytes());
            version.extend([0u8; 10]);
            version.extend([0xff, 0xff, 127, 0, 0, 1]);
            version.extend(0u16.to_be_bytes());
        }
        version.extend(rand_nonce().to_le_bytes());
        version.push(0);
        version.extend((-1i32).to_le_bytes());
        version.push(u8::from(relay));
        version.extend([0u8; 32]);
        version.push(u8::from(masternode));
        peer.send("version", &version);
        let (mut version_seen, mut verack_seen) = (false, false);
        while !(version_seen && verack_seen) {
            let (command, _) = peer.recv();
            match command.as_str() {
                "version" => {
                    version_seen = true;
                    peer.send("verack", &[]);
                }
                "verack" => verack_seen = true,
                _ => {}
            }
        }
        peer
    }

    fn local_port(&self) -> u16 {
        self.stream.local_addr().unwrap().port()
    }

    fn send(&mut self, command: &str, payload: &[u8]) {
        let mut name = [0u8; 12];
        name[..command.len()].copy_from_slice(command.as_bytes());
        let mut message = MAGIC.to_vec();
        message.extend(name);
        message.extend(u32::try_from(payload.len()).unwrap().to_le_bytes());
        message.extend(&sha256d(payload)[..4]);
        message.extend(payload);
        self.stream.write_all(&message).unwrap_or_else(|e| panic!("{}: send: {e}", self.name));
    }

    /// The next message, with a `ping` answered on the way.
    fn recv(&mut self) -> (String, Vec<u8>) {
        loop {
            let mut header = [0u8; 24];
            self.stream
                .read_exact(&mut header)
                .unwrap_or_else(|e| panic!("{}: read: {e}", self.name));
            assert_eq!(header[..4], MAGIC, "{}: a v1 message", self.name);
            let command =
                String::from_utf8(header[4..16].iter().copied().take_while(|b| *b != 0).collect())
                    .expect("an ASCII command");
            let len = u32::from_le_bytes(header[16..20].try_into().unwrap());
            let mut payload = vec![0u8; len as usize];
            self.stream.read_exact(&mut payload).expect("the payload");
            if command == "ping" {
                self.send("pong", &payload);
                continue;
            }
            return (command, payload);
        }
    }

    /// `sync_with_ping`: rdashd has handled every message sent before it. The messages received
    /// on the way are returned.
    fn barrier(&mut self) -> Vec<(String, Vec<u8>)> {
        let nonce = rand_nonce().to_le_bytes();
        self.send("ping", &nonce);
        let mut seen = Vec::new();
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < DEADLINE, "{}: no pong", self.name);
            let (command, payload) = self.recv();
            if command == "pong" && payload == nonce {
                return seen;
            }
            seen.push((command, payload));
        }
    }

    /// `send_requested_qcontrib`: announce, wait for the `getdata`, then deliver.
    // Core: test/functional/feature_llmq_dkg_intake.py:100-112 at v24.0.0
    fn send_requested_qcontrib(&mut self, payload: &[u8]) {
        let inv = inventory(MSG_QUORUM_CONTRIB, &sha256d(payload));
        self.send("inv", &inv);
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < DEADLINE, "{}: no getdata", self.name);
            let (command, got) = self.recv();
            if command == "getdata" && got == inv {
                break;
            }
        }
        self.send("qcontrib", payload);
        self.barrier();
    }
}

/// rdashd in this process, with RPC and a P2P listener on loopback, the controller as its one
/// outbound peer, and `-whitelist` so a scored peer stays connected (Core's set_test_params).
struct Rdashd {
    node: Option<Node>,
    datadir: PathBuf,
    rpc_port: u16,
    p2p_port: u16,
}

impl Rdashd {
    fn start(datadir: &Path, p2p_port: u16, ctx: &MasternodeTestContext, extra: &[String]) -> Self {
        let rpc_port = node_test_ports::free_port();
        let mut argv: Vec<String> = [
            "rdashd".to_owned(),
            "-regtest".to_owned(),
            format!("-datadir={}", datadir.display()),
            "-printtoconsole=0".to_owned(),
            "-server=1".to_owned(),
            format!("-rpcport={rpc_port}"),
            "-rpcuser=dk1".to_owned(),
            "-rpcpassword=dk1".to_owned(),
            "-listen=1".to_owned(),
            format!("-bind=127.0.0.1:{p2p_port}"),
            format!("-port={p2p_port}"),
            format!("-connect=127.0.0.1:{}", ctx.controller.p2p_port()),
            format!("-mocktime={}", ctx.mocktime()),
            "-whitelist=127.0.0.1".to_owned(),
            "-deprecatedrpc=banscore".to_owned(),
            "-debug=net".to_owned(),
            "-debug=llmq".to_owned(),
            "-debug=llmq-dkg".to_owned(),
        ]
        .into();
        argv.extend(ctx.metadata.dashd_extra_args.iter().cloned());
        argv.extend(extra.iter().cloned());
        std::fs::create_dir_all(datadir).unwrap();
        let args = ArgsManager::new();
        setup_server_args(&args);
        args.parse_parameters(&argv).expect("rdashd's arguments");
        let params = init_config(&args).expect("rdashd's config");
        let mut node = Node::new(Arc::new(args), Arc::new(Ui::new(logger())), params);
        assert!(node.app_init_early() && node.app_init_main(), "rdashd starts");
        let rdashd = Rdashd {
            node: Some(node),
            datadir: datadir.to_owned(),
            rpc_port,
            p2p_port,
        };
        let deadline = Instant::now() + DEADLINE;
        while rdashd.try_call("getblockcount", json!([])).is_none() {
            assert!(Instant::now() < deadline, "rdashd's RPC never came up");
            std::thread::sleep(Duration::from_millis(200));
        }
        rdashd.wait_for_tip(&ctx.controller.get_best_block_hash().to_string());
        rdashd.relay_sporks(ctx);
        rdashd.finish_mnsync();
        rdashd.call("setmocktime", json!([ctx.mocktime()]));
        rdashd
    }

    /// The controller's sporks, signed anew so that they reach rdashd. dashd 23.1 sends its
    /// sporks only on `getsporks`; rdashd, as Core v24.0.0, sends none and expects a peer to
    /// announce them at `verack` (Core a56ed47c98a).
    fn relay_sporks(&self, ctx: &MasternodeTestContext) {
        let sporks = ctx.controller.try_rpc_call("spork", &["show".into()]).expect("spork show");
        let sporks = sporks.as_object().expect("the sporks").clone();
        for (name, value) in &sporks {
            ctx.controller
                .try_rpc_call("sporkupdate", &[name.clone().into(), value.clone()])
                .unwrap_or_else(|| panic!("sporkupdate {name}"));
        }
        let deadline = Instant::now() + DEADLINE;
        let dkg = "SPORK_17_QUORUM_DKG_ENABLED";
        while self.call("spork", json!(["show"]))[dkg] != sporks[dkg] {
            assert!(Instant::now() < deadline, "rdashd never had the controller's {dkg}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn reply(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"method": method, "params": params, "id": 1}).to_string();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic ZGsxOmRrMQ==\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut stream = TcpStream::connect(("127.0.0.1", self.rpc_port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(300))).ok()?;
        stream.write_all(request.as_bytes()).ok()?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).ok()?;
        let (_, body) = reply.split_once("\r\n\r\n")?;
        serde_json::from_str(body).ok()
    }

    fn try_call(&self, method: &str, params: Value) -> Option<Value> {
        self.reply(method, params).and_then(|r| r["error"].is_null().then(|| r["result"].clone()))
    }

    fn call(&self, method: &str, params: Value) -> Value {
        let reply = self.reply(method, params.clone()).expect("a reply");
        assert!(reply["error"].is_null(), "rdashd {method} {params}: {}", reply["error"]);
        reply["result"].clone()
    }

    fn wait_for_tip(&self, tip: &str) {
        let deadline = Instant::now() + Duration::from_secs(600);
        while self.try_call("getbestblockhash", json!([])).as_ref().and_then(Value::as_str)
            != Some(tip)
        {
            assert!(Instant::now() < deadline, "rdashd never reached {tip}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Core's `force_finish_mnsync` without `mnsync next`, as p7_quorum_conn_dashd does: each
    /// asset times out once mocktime passes `MASTERNODE_SYNC_TIMEOUT_SECONDS`.
    // Core: node/sync_manager.cpp:138-317 at v24.0.0
    fn finish_mnsync(&self) {
        let mut time =
            self.call("getblock", json!([self.call("getbestblockhash", json!([]))]))["time"]
                .as_i64()
                .expect("a block time");
        let deadline = Instant::now() + DEADLINE;
        while !self.debug_log().contains("CMasternodeSync::SwitchToNextAsset -- Sync has finished")
        {
            assert!(Instant::now() < deadline, "rdashd's masternode sync never finished");
            time += MASTERNODE_SYNC_TIMEOUT_SECONDS + 1;
            self.call("setmocktime", json!([time]));
            std::thread::sleep(Duration::from_secs(MASTERNODE_SYNC_TICK_SECONDS as u64));
        }
    }

    fn debug_log(&self) -> String {
        std::fs::read_to_string(self.datadir.join("regtest/debug.log")).unwrap_or_default()
    }

    /// Where the debug log ends now: `assert_debug_log` looks only at what follows.
    fn log_mark(&self) -> usize {
        self.debug_log().len()
    }

    fn log_since(&self, mark: usize) -> String {
        self.debug_log().get(mark..).unwrap_or_default().to_owned()
    }

    /// Waits until the log after `mark` holds `line`; that part of the log.
    fn wait_for_line(&self, mark: usize, line: &str) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let log = self.log_since(mark);
            if log.contains(line) {
                return log;
            }
            assert!(Instant::now() < deadline, "rdashd never logged {line:?}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// rdashd's id for the inbound peer on local port `port`.
    fn peer_id(&self, peer: &Peer) -> i64 {
        let addr = format!("127.0.0.1:{}", peer.local_port());
        let deadline = Instant::now() + DEADLINE;
        loop {
            let peers = self.call("getpeerinfo", json!([]));
            let found = peers.as_array().unwrap().iter().find(|p| p["addr"] == addr.as_str());
            if let Some(id) = found.and_then(|p| p["id"].as_i64()) {
                return id;
            }
            assert!(Instant::now() < deadline, "no peer {addr}: {peers}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn banscore(&self, id: i64) -> Option<i64> {
        let peers = self.call("getpeerinfo", json!([]));
        peers
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .and_then(|p| p["banscore"].as_i64())
    }

    /// `wait_for_banscore`.
    // Core: test/functional/feature_llmq_dkg_intake.py:91-97 at v24.0.0
    fn wait_for_banscore(&self, id: i64, score: i64) {
        let deadline = Instant::now() + DEADLINE;
        while self.banscore(id) != Some(score) {
            assert!(
                Instant::now() < deadline,
                "peer={id}: banscore {:?}, not {score}",
                self.banscore(id)
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `add_verified_peer`: a peer that rdashd's `mnauth` marks verified as `protx`, with
    /// banscore 0.
    // Core: test/functional/feature_llmq_dkg_intake.py:154-158 at v24.0.0
    fn verified_peer(&self, name: &str, time: i64, protx: &str) -> (Peer, i64) {
        let peer = Peer::connect(name, self.p2p_port, time);
        let id = self.peer_id(&peer);
        assert_eq!(self.call("mnauth", json!([id, protx, FAKE_PUBKEY])), json!(true));
        self.wait_for_banscore(id, 0);
        (peer, id)
    }

    /// rdashd's `llmq_test` session status for `quorum`.
    fn session(&self, quorum: &str) -> Option<Value> {
        let status = self.call("quorum", json!(["dkgstatus"]));
        status["session"].as_array()?.iter().find_map(|s| {
            let st = &s["status"];
            (s["llmqType"] == "llmq_test" && st["quorumHash"] == quorum).then(|| st.clone())
        })
    }
}

impl Drop for Rdashd {
    fn drop(&mut self) {
        let status = self.node.take().map(|mut node| {
            node.interrupt();
            node.shutdown();
            logger().disconnect_test_logger();
            node.exit_status()
        });
        retain_test_dir(&self.datadir, "rdashd");
        if !std::thread::panicking() {
            assert_eq!(status.unwrap_or(0), 0, "rdashd's exit status");
        }
    }
}

/// The network around rdashd: the controller mines, the running dashd masternodes and rdashd
/// follow, and rdashd's mock time moves with the network's.
struct Net {
    ctx: MasternodeTestContext,
    rdashd: Rdashd,
    /// The running dashd masternodes' proTxHashes, in `ctx.masternodes` order.
    pro_txs: Vec<String>,
    /// Masternodes (`ctx.masternodes` indexes) whose network is off, which no block wait reads.
    offline: Vec<usize>,
    interval: u64,
}

impl Net {
    fn time(&self) -> i64 {
        self.ctx.mocktime() as i64
    }

    /// `bump_mocktime(1)` on every node, rdashd included.
    fn tick(&mut self) {
        self.ctx.bump_mocktime(1);
        self.rdashd.call("setmocktime", json!([self.ctx.mocktime()]));
        self.rdashd.call("mockscheduler", json!([1]));
    }

    fn controller(&self, method: &str, params: &[Value]) -> Value {
        self.ctx
            .controller
            .try_rpc_call(method, params)
            .unwrap_or_else(|| panic!("controller {method}"))
    }

    fn height(&self) -> u64 {
        self.controller("getblockcount", &[]).as_u64().unwrap()
    }

    /// `move_blocks(nodes, n)`: one mocktime bump, `n` blocks, and every online node at the tip.
    // Core: test/functional/test_framework/test_framework.py DashTestFramework.move_blocks
    fn move_blocks(&mut self, n: u64) {
        self.tick();
        let addr = self.ctx.controller.get_new_address();
        self.ctx.controller.generate_blocks(n, &addr);
        let tip = self.ctx.controller.get_best_block_hash().to_string();
        let deadline = Instant::now() + Duration::from_secs(600);
        for (i, mn) in self.ctx.masternodes.iter().enumerate() {
            if self.offline.contains(&i) {
                continue;
            }
            while mn.try_rpc_call("getbestblockhash", &[]).as_ref().and_then(Value::as_str)
                != Some(tip.as_str())
            {
                assert!(Instant::now() < deadline, "masternode {i} never reached {tip}");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
        self.rdashd.wait_for_tip(&tip);
    }

    /// Waits until rdashd's `llmq_test` session for `quorum` is at `phase`, bumping mock time as
    /// Core's `wait_for_quorum_phase` does.
    fn wait_for_phase(&mut self, quorum: &str, phase: u64) {
        let deadline = Instant::now() + DEADLINE;
        let mut polls = 0u32;
        loop {
            if self.rdashd.session(quorum).is_some_and(|st| st["phase"].as_u64() == Some(phase)) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "rdashd's {quorum} session never reached phase {phase}: {}",
                self.rdashd.call("quorum", json!(["dkgstatus"]))
            );
            if polls.is_multiple_of(5) {
                self.tick();
            }
            polls += 1;
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `_start_fresh_dkg_cycle`, at a cycle whose `llmq_test` quorum has rdashd's masternode
    /// (`me`) as a member: the cycle's quorum hash and its members.
    // Core: test/functional/feature_llmq_dkg_intake.py:255-261 at v24.0.0
    fn fresh_member_cycle(&mut self, me: &str) -> (String, Vec<String>) {
        for _ in 0..12 {
            let skip = self.interval - self.height() % self.interval;
            self.move_blocks(skip);
            let quorum = self.ctx.controller.get_best_block_hash().to_string();
            let members = self.members(&quorum);
            if members.iter().any(|m| m == me) {
                assert_eq!(self.height() % self.interval, 0, "a cycle's base block");
                self.wait_for_phase(&quorum, 1);
                return (quorum, members);
            }
        }
        panic!("rdashd's masternode is in no llmq_test quorum of 12 cycles");
    }

    /// The members of `llmq_test`'s quorum `quorum`, once a dashd member's session for it has
    /// initialized: its `quorum dkgstatus 2` `allMembers` (rdashd serves that only in phase H, and
    /// lists no session it is not a member of). Of llmq_test's three members at least two are
    /// dashd masternodes.
    // Core: llmq/debug.cpp:99-105 at v24.0.0
    fn members(&mut self, quorum: &str) -> Vec<String> {
        let deadline = Instant::now() + DEADLINE;
        loop {
            for (i, mn) in self.ctx.masternodes.iter().enumerate() {
                if self.offline.contains(&i) {
                    continue;
                }
                let Some(status) = mn.try_rpc_call("quorum", &["dkgstatus".into(), 2.into()])
                else {
                    continue;
                };
                let sessions = status["session"].as_array().cloned().unwrap_or_default();
                for session in sessions.iter().filter(|s| s["llmqType"] == "llmq_test") {
                    let st = session.get("status").unwrap_or(session);
                    if st["quorumHash"] != quorum {
                        continue;
                    }
                    if let Some(all) = st["allMembers"].as_array().filter(|a| !a.is_empty()) {
                        return all.iter().map(|m| m.as_str().unwrap().to_owned()).collect();
                    }
                }
            }
            assert!(Instant::now() < deadline, "no dashd member reports quorum {quorum}");
            self.tick();
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn set_network_active(&mut self, pro_tx: &str, active: bool) {
        let i = self.pro_txs.iter().position(|p| p == pro_tx).expect("a running masternode");
        let mn = &self.ctx.masternodes[i];
        mn.try_rpc_call("setnetworkactive", &[active.into()]).expect("setnetworkactive");
        if active {
            let controller = format!("127.0.0.1:{}", self.ctx.controller.p2p_port());
            mn.try_rpc_call("addnode", &[controller.into(), "onetry".into()]);
            self.offline.retain(|o| *o != i);
        } else {
            self.offline.push(i);
        }
    }
}

/// The newest `llmq_test` quorum on the controller (`mine_quorum`'s hash in Core's test).
fn newest_quorum(ctx: &MasternodeTestContext) -> String {
    let list =
        ctx.controller.try_rpc_call("quorum", &["list".into(), 1.into()]).expect("quorum list");
    list["llmq_test"][0].as_str().expect("an llmq_test quorum").to_owned()
}

/// Starts the network without masternode 0, and rdashd as that masternode, active: rdashd binds
/// its own port first, and then the masternode's service moves there, so the address the others
/// connect to is one rdashd listens on.
// Core: active/masternode.cpp:112-173, :175-228 at v24.0.0 (Init retried at each tip until Ready)
fn masternode_net(runtime: &tokio::runtime::Runtime, dir: &Path) -> Option<(Net, String)> {
    let ctx = runtime.block_on(MasternodeTestContext::new_leaving_out(0))?;
    let index = ctx.left_out.expect("a masternode left out");
    let me = ctx.metadata.masternodes[index].pro_tx_hash.clone();
    let key = format!("-masternodeblsprivkey={}", ctx.metadata.masternodes[index].bls_private_key);
    let rdashd = Rdashd::start(&dir.join("rdashd"), node_test_ports::free_port(), &ctx, &[key]);
    let port = rdashd.p2p_port;
    let mark = rdashd.log_mark();
    ctx.move_service(index, port);
    let pro_txs = ctx
        .metadata
        .masternodes
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, m)| m.pro_tx_hash.clone())
        .collect();
    let interval = u64::from(ctx.metadata.dkg_interval);
    let mut net = Net {
        ctx,
        rdashd,
        pro_txs,
        offline: Vec::new(),
        interval,
    };
    net.move_blocks(1);
    let log = net.rdashd.wait_for_line(
        mark,
        &format!(
            "CActiveMasternodeManager::Init -- Checking inbound connection to '127.0.0.1:{port}'"
        ),
    );
    assert!(log.contains(&format!("CActiveMasternodeManager::Init -- proTxHash={me}")), "{log}");
    Some((net, me))
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

/// `test_observer_drops_dkg_messages`: a `-watchquorums` observer neither requests a DKG
/// contribution it is offered nor keeps one it is sent, and does not score the sender.
// Core: test/functional/feature_llmq_dkg_intake.py:181-200 at v24.0.0; llmq/net_dkg.cpp:528-531
#[test]
fn an_observer_ignores_qcontrib_without_requesting_or_scoring_it() {
    let _serial = serial();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Some(ctx) = runtime.block_on(MasternodeTestContext::new(true)) else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let rdashd = Rdashd::start(
        &dir.path().join("rdashd"),
        node_test_ports::free_port(),
        &ctx,
        &["-watchquorums=1".to_owned()],
    );
    let quorum = newest_quorum(&ctx);
    let time = ctx.mocktime() as i64;
    let (mut peer, id) = rdashd.verified_peer("observer", time, FAKE_PROTX);

    let payload = qcontrib(&quorum, 2, &INVALID_NONZERO_BLS_PUBKEY, 0);
    peer.send("inv", &inventory(MSG_QUORUM_CONTRIB, &sha256d(&payload)));
    let seen = peer.barrier();
    assert!(seen.iter().all(|(c, _)| c != "getdata"), "the observer requested it: {seen:?}");

    let mark = rdashd.log_mark();
    peer.send("qcontrib", &payload);
    peer.barrier();
    rdashd.wait_for_line(mark, "ignoring qcontrib in observer mode");
    rdashd.wait_for_banscore(id, 0);
}

/// Core's intake scenarios on an active masternode, in its order: a non-verified sender; the
/// size cap and framing from a verified one; a contribution with an invalid BLS key, queued
/// while the session initializes and scored by the worker in the contribute phase; the
/// per-proTx quota on late contributions, kept across reconnects and cleared without decoding at
/// the next round; fewer blobs than the minimum; and a well-formed push nobody requested.
// Core: test/functional/feature_llmq_dkg_intake.py:160-179 at v24.0.0
#[test]
fn a_member_screens_dkg_messages_before_its_queues_and_scores_bad_bls_in_the_worker() {
    let _serial = serial();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let Some((mut net, me)) = masternode_net(&runtime, dir.path()) else {
        return;
    };
    let regtest = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let llmq = regtest.consensus.llmq(LlmqType::LlmqTest).expect("regtest runs llmq_test");
    let quorum = newest_quorum(&net.ctx);

    unverified_sender_rejected(&net, &quorum);
    oversized_rejected(&net, &quorum);
    malformed_rejected(&net, &quorum);
    trailing_bytes_rejected(&net, &quorum);

    let (quorum, members) = net.fresh_member_cycle(&me);
    assert!(members.contains(&me), "{members:?}");
    malformed_bls_pubkey_rejected_by_worker(&mut net, &quorum);

    let (quorum, members) = net.fresh_member_cycle(&me);
    assert!(members.contains(&me), "{members:?}");
    let quorum = late_messages_bounded(&mut net, &me, &quorum, usize::from(llmq.size));

    under_min_contribution_blobs_rejected(&net, &quorum);
    unrequested_rejected(&net, &quorum);
}

/// `test_unverified_sender_rejected`: each DKG push from a peer that is not MNAuth-verified
/// scores 10.
// Core: test/functional/feature_llmq_dkg_intake.py:202-214 at v24.0.0; llmq/net_dkg.cpp:533-537
fn unverified_sender_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let mut peer = Peer::connect("unverified", rdashd.p2p_port, net.time());
    let id = rdashd.peer_id(&peer);
    rdashd.wait_for_banscore(id, 0);
    let mut score = 0;
    for command in DKG_PUSH_TYPES {
        let mark = rdashd.log_mark();
        peer.send(command, &prefix(quorum));
        peer.barrier();
        rdashd.wait_for_line(mark, "DKG message from non-verified peer");
        score += 10;
        rdashd.wait_for_banscore(id, score);
    }
}

/// `test_oversized_rejected`: past the size cap, even from a verified peer, scores 100.
// Core: test/functional/feature_llmq_dkg_intake.py:216-227 at v24.0.0; llmq/net_dkg.cpp:602-607
fn oversized_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let (mut peer, id) = rdashd.verified_peer("oversized", net.time(), FAKE_PROTX);
    let mut payload = prefix(quorum);
    payload.resize(payload.len() + 1024 * 1024 + 4096, 0);
    let mark = rdashd.log_mark();
    peer.send("qcontrib", &payload);
    peer.barrier();
    rdashd.wait_for_line(mark, "oversized DKG message");
    rdashd.wait_for_banscore(id, 100);
}

/// `test_malformed_rejected`: too few bytes to walk a contribution scores 100.
// Core: test/functional/feature_llmq_dkg_intake.py:229-240 at v24.0.0
fn malformed_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let (mut peer, id) = rdashd.verified_peer("malformed", net.time(), FAKE_PROTX);
    let mark = rdashd.log_mark();
    peer.send("qcontrib", &[prefix(quorum), vec![0; 4]].concat());
    peer.barrier();
    rdashd.wait_for_line(mark, "malformed DKG message");
    rdashd.wait_for_banscore(id, 100);
}

/// `test_trailing_bytes_rejected`: a contribution with a trailing byte scores 100.
// Core: test/functional/feature_llmq_dkg_intake.py:242-253 at v24.0.0
fn trailing_bytes_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let (mut peer, id) = rdashd.verified_peer("trailing", net.time(), FAKE_PROTX);
    let payload = [qcontrib(quorum, 2, &hex(FAKE_PUBKEY), 0), vec![0]].concat();
    let mark = rdashd.log_mark();
    peer.send("qcontrib", &payload);
    peer.barrier();
    rdashd.wait_for_line(mark, "malformed DKG message");
    rdashd.wait_for_banscore(id, 100);
}

/// `test_malformed_bls_pubkey_rejected_by_worker`: a requested contribution with an invalid BLS
/// key passes intake unscored while the session initializes; the contribute phase's drain fails
/// to deserialize it and scores 100. Neither intake line appears.
// Core: test/functional/feature_llmq_dkg_intake.py:263-283 at v24.0.0; llmq/net_dkg.cpp:376-383
fn malformed_bls_pubkey_rejected_by_worker(net: &mut Net, quorum: &str) {
    let (mut peer, id) = net.rdashd.verified_peer("bad-bls", net.time(), FAKE_PROTX);
    let mark = net.rdashd.log_mark();
    peer.send_requested_qcontrib(&qcontrib(quorum, 2, &INVALID_NONZERO_BLS_PUBKEY, 0));
    net.rdashd.wait_for_banscore(id, 0);
    net.move_blocks(2);
    let log = net.rdashd.wait_for_line(mark, "failed to deserialize message");
    for unexpected in ["malformed DKG message", "unrequested DKG message"] {
        assert!(!log.contains(unexpected), "{unexpected:?}: {log}");
    }
    net.rdashd.wait_for_banscore(id, 100);
}

/// `test_late_messages_bounded`: in the complain phase, nothing drains contributions; the
/// per-proTx quota (twice the quorum size) is keyed by the verified proTxHash, so reconnecting
/// does not refill it, and the next message logs the quota line unscored. Another proTx has its
/// own quota. The next round clears the queue without decoding: no decoding line appears up to
/// its initialization (and its first phase, if rdashd is a member), and neither peer is scored.
/// Returns the new cycle's quorum hash.
// Core: test/functional/feature_llmq_dkg_intake.py:295-348 at v24.0.0;
// llmq/dkgsessionhandler.cpp:29-56
fn late_messages_bounded(net: &mut Net, me: &str, quorum: &str, size: usize) -> String {
    assert_eq!(net.height() % net.interval, 0, "a DKG cycle's base");
    let mark = net.rdashd.log_mark();
    net.move_blocks(4);
    assert_eq!(net.height() % net.interval, 4, "the complain stage");
    // The messages are late once rdashd's contribute stage is over: its round either reached the
    // complain phase, marked only after the phase's start function ran, or, having seen the phase
    // jump from 1 past 2, aborted until the next round.
    // Core: active/dkgsessionhandler.cpp:111-139, :248-251 at v24.0.0
    let aborted = "WaitForNextPhase -- llmq_test qi[0] - aborting due unexpected phase change";
    let wait = Instant::now() + DEADLINE;
    while !net.rdashd.log_since(mark).contains(aborted)
        && net.rdashd.session(quorum).is_none_or(|st| st["phase"].as_u64() != Some(3))
    {
        assert!(Instant::now() < wait, "rdashd's contribute stage for {quorum} never ended");
        net.tick();
        std::thread::sleep(Duration::from_millis(500));
    }
    let quota = 2 * size;
    let mut nonce = 0;
    for i in 0..quota {
        nonce += 1;
        let (mut peer, id) =
            net.rdashd.verified_peer(&format!("reconnect-{i}"), net.time(), FAKE_PROTX);
        peer.send_requested_qcontrib(&qcontrib(quorum, 2, &INVALID_NONZERO_BLS_PUBKEY, nonce));
        net.rdashd.wait_for_banscore(id, 0);
        drop(peer);
        let deadline = Instant::now() + DEADLINE;
        while net.rdashd.banscore(id).is_some() {
            assert!(Instant::now() < deadline, "peer={id} stays connected");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    nonce += 1;
    let (mut quota_peer, quota_id) =
        net.rdashd.verified_peer("reconnect-over", net.time(), FAKE_PROTX);
    let mark = net.rdashd.log_mark();
    quota_peer.send_requested_qcontrib(&qcontrib(quorum, 2, &INVALID_NONZERO_BLS_PUBKEY, nonce));
    net.rdashd.wait_for_line(mark, &format!("too many messages from {FAKE_PROTX}"));
    net.rdashd.wait_for_banscore(quota_id, 0);

    nonce += 1;
    let retained_protx = format!("{:064x}", 0xd0);
    let (mut retained, retained_id) =
        net.rdashd.verified_peer("retained", net.time(), &retained_protx);
    retained.send_requested_qcontrib(&qcontrib(quorum, 2, &INVALID_NONZERO_BLS_PUBKEY, nonce));
    net.rdashd.wait_for_banscore(retained_id, 0);

    let mark = net.rdashd.log_mark();
    let remaining = net.interval - net.height() % net.interval;
    net.move_blocks(remaining);
    let next = net.ctx.controller.get_best_block_hash().to_string();
    // Core's window covers the victim's next HandleDKGRound up to InitNewQuorum, after its
    // ClearPendingMessages, member or not; rdashd logs this line after clearing the queue.
    // Core: test/functional/feature_llmq_dkg_intake.py:334-345, llmq/net_dkg.cpp:850-865 at v24.0.0
    let initialized = format!(
        "ActiveDKGSessionHandler::InitNewQuorum -- height[{}] quorum initialization OK for llmq_test qi[",
        net.height()
    );
    let wait = Instant::now() + DEADLINE;
    while !net.rdashd.log_since(mark).contains(&initialized) {
        assert!(Instant::now() < wait, "rdashd never logged {initialized:?}");
        net.tick();
        std::thread::sleep(Duration::from_millis(500));
    }
    if net.members(&next).contains(&me.to_owned()) {
        net.wait_for_phase(&next, 1);
    }
    let log = net.rdashd.log_since(mark);
    for unexpected in
        ["malformed DKG message", "failed to deserialize message", "message failed structure check"]
    {
        assert!(!log.contains(unexpected), "{unexpected:?}: {log}");
    }
    net.rdashd.wait_for_banscore(retained_id, 0);
    net.rdashd.wait_for_banscore(quota_id, 0);
    next
}

/// `test_under_min_contribution_blobs_rejected`: fewer blobs than the quorum's minimum scores
/// 100.
// Core: test/functional/feature_llmq_dkg_intake.py:350-358 at v24.0.0
fn under_min_contribution_blobs_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let (mut peer, id) = rdashd.verified_peer("under-min", net.time(), FAKE_PROTX);
    let mark = rdashd.log_mark();
    peer.send("qcontrib", &qcontrib(quorum, 1, &hex(FAKE_PUBKEY), 0));
    peer.barrier();
    rdashd.wait_for_line(mark, "malformed DKG message");
    rdashd.wait_for_banscore(id, 100);
}

/// `test_unrequested_rejected`: a well-formed contribution the peer never announced and rdashd
/// never requested scores 10.
// Core: test/functional/feature_llmq_dkg_intake.py:360-372 at v24.0.0; llmq/net_dkg.cpp:619-627
fn unrequested_rejected(net: &Net, quorum: &str) {
    let rdashd = &net.rdashd;
    let (mut peer, id) = rdashd.verified_peer("unrequested", net.time(), FAKE_PROTX);
    let mark = rdashd.log_mark();
    peer.send("qcontrib", &qcontrib(quorum, 2, &hex(FAKE_PUBKEY), 0));
    peer.barrier();
    rdashd.wait_for_line(mark, "unrequested DKG message");
    rdashd.wait_for_banscore(id, 10);
}

/// `test_no_service`, from the complaining member's side: rdashd, a member, judges each other
/// member by what its masternode connections to that member advertise. Both other members go
/// offline once the contribute phase starts, and a peer stands in for each, MNAuth-verified as
/// that member: one advertising no transaction relay, one no compact block filters. Under
/// `SPORK_23_QUORUM_POSE` rdashd logs each missing service when the complain phase starts; with
/// the spork off it logs neither.
// Core: test/functional/feature_llmq_simplepose.py:107-111, :162-255 at v24.0.0;
// active/dkgsession.cpp:255-319
#[test]
fn a_member_logs_a_stand_ins_missing_tx_relay_and_filters_only_under_pose() {
    let _serial = serial();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let Some((mut net, me)) = masternode_net(&runtime, dir.path()) else {
        return;
    };
    for (spork, banned) in [(0, true), (SPORK_OFF, false)] {
        // A spork signed at the time of the one rdashd holds is not newer (Core: spork.cpp:112 at v24.0.0).
        net.tick();
        net.controller("sporkupdate", &["SPORK_23_QUORUM_POSE".into(), spork.into()]);
        let deadline = Instant::now() + DEADLINE;
        while net.rdashd.call("spork", json!(["show"]))["SPORK_23_QUORUM_POSE"].as_i64()
            != Some(spork)
        {
            assert!(Instant::now() < deadline, "rdashd never saw SPORK_23 at {spork}");
            std::thread::sleep(Duration::from_millis(200));
        }
        mine_quorum_no_service(&mut net, &me, banned);
    }
}

/// `mine_quorum_no_service` up to the justify phase, with stand-ins on rdashd for both other
/// members: `banned` (the spork on) expects each missing service logged, else neither.
// Core: test/functional/feature_llmq_simplepose.py:162-222 at v24.0.0
fn mine_quorum_no_service(net: &mut Net, me: &str, banned: bool) {
    let (quorum, members) = net.fresh_member_cycle(me);
    let others: Vec<String> = members.into_iter().filter(|m| m != me).collect();
    let [no_relay, no_filters] = <[String; 2]>::try_from(others).expect("two other members");
    net.move_blocks(2);
    net.wait_for_phase(&quorum, 2);

    let mut stand_ins = Vec::new();
    for (pro_tx, services, relay) in
        [(&no_relay, NODE_NETWORK | NODE_COMPACT_FILTERS, false), (&no_filters, NODE_NETWORK, true)]
    {
        let operator_key = {
            let info = net.ctx.metadata.masternodes.iter().find(|m| m.pro_tx_hash == **pro_tx);
            info.expect("a member").bls_public_key.clone()
        };
        net.set_network_active(pro_tx, false);
        let deadline = Instant::now() + DEADLINE;
        while net
            .rdashd
            .call("getpeerinfo", json!([]))
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["verified_proregtx_hash"] == pro_tx.as_str())
        {
            assert!(Instant::now() < deadline, "rdashd keeps a connection to {pro_tx}");
            std::thread::sleep(Duration::from_millis(200));
        }
        let peer = Peer::connect_as(pro_tx, net.rdashd.p2p_port, net.time(), services, relay, true);
        let id = net.rdashd.peer_id(&peer);
        let verified = net.rdashd.call("mnauth", json!([id, pro_tx, operator_key]));
        assert_eq!(verified, json!(true));
        stand_ins.push(peer);
    }

    let mark = net.rdashd.log_mark();
    net.move_blocks(2);
    net.wait_for_phase(&quorum, 3);
    net.move_blocks(2);
    net.wait_for_phase(&quorum, 4);
    let relay_line = format!("{no_relay} does not relay transactions");
    let filters_line = format!("{no_filters} does not serve compact block filters");
    if banned {
        net.rdashd.wait_for_line(mark, &relay_line);
        net.rdashd.wait_for_line(mark, &filters_line);
    } else {
        let log = net.rdashd.log_since(mark);
        for line in ["does not relay transactions", "does not serve compact block filters"] {
            assert!(!log.contains(line), "{line:?}: {log}");
        }
    }

    drop(stand_ins);
    for pro_tx in [&no_relay, &no_filters] {
        net.set_network_active(pro_tx, true);
    }
}
