//! SG1-W1-8 phase 2: a recovered signature from Core's quorums, through rdashd, on setup-dashd's
//! masternode network (a controller and four masternodes, dashd at `DASHD_PATH`) with a live DKG
//! cycle; rdashd runs in this process as a non-masternode that syncs from the controller.
//!
//! 1. Raw peer A sends `qsendrecsigs` to a Core masternode of the new `llmq_test` quorum.
//! 2. `quorum sign` runs on the Core masternodes, and A captures the `qsigrec` (INV, GETDATA).
//! 3. Raw peer B delivers it to rdashd.
//! 4. Raw peer C, subscribed on rdashd, gets the INV, sends GETDATA and receives the same bytes.
//! 5. A `qsigrec` with another request's signature scores B +100.
//! 6. After a restart rdashd still serves the signature; after `-reindex` it does not.
//!
//! Needs setup-dashd's environment (`DASHD_PATH`, `DASHD_MN_DATADIR`); `SKIP_DASHD_TESTS` skips.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_consensus::hash::sha256d;
use dash_spv::test_utils::{MasternodeTestContext, retain_test_dir};
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

/// `Consensus::LLMQType::LLMQ_TEST`.
const LLMQ_TEST: u8 = 100;
/// `MSG_QUORUM_RECOVERED_SIG` (`protocol.h` v24.0.2:521).
const MSG_QUORUM_RECOVERED_SIG: u32 = 28;
/// Regtest's message start (`chainparams.cpp`, CRegTestParams `pchMessageStart`).
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// dashd 23.1's `PROTOCOL_VERSION` (`version.h`).
const VERSION: i32 = 70240;
/// `CRecoveredSig`'s serialization: type, quorum hash, id, message hash, then the signature.
const SIG_AT: usize = 1 + 32 * 3;
const DEADLINE: Duration = Duration::from_secs(180);

/// A raw v1 P2P peer.
struct Peer {
    stream: TcpStream,
    name: &'static str,
}

impl Peer {
    /// Connects to `port` and completes the version handshake at mocktime `time`, then waits for
    /// a `pong`: the node polls quorum messages ahead of the normal queue, so a `qsigrec` sent
    /// before it has processed our `verack` would be dropped as sent before the handshake; the
    /// node answers `ping` only once it has processed our `verack`, so the `pong` proves the
    /// handshake is complete.
    fn connect(name: &'static str, port: u16, time: i64) -> Peer {
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("the node listens");
        stream.set_read_timeout(Some(DEADLINE)).unwrap();
        let mut peer = Peer {
            stream,
            name,
        };
        // `msg_version` as test_framework/messages.py writes it, then Dash's MNAUTH challenge
        // and masternode flag.
        let mut version = Vec::new();
        version.extend(VERSION.to_le_bytes());
        version.extend(0u64.to_le_bytes());
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
        version.push(1);
        version.extend([0u8; 32]);
        version.push(0);
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
        // Core: v24.0.2 src/net.cpp:4810, :4829-4873; src/net_processing.cpp:4381-4384, :5474-5488
        peer.barrier();
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

    /// The next message, with a `ping` answered on the way; `None` once the node has closed the
    /// connection.
    fn try_recv(&mut self) -> Option<(String, Vec<u8>)> {
        loop {
            let mut header = [0u8; 24];
            match self.stream.read_exact(&mut header) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return None,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return None,
                Err(e) => panic!("{}: read: {e}", self.name),
            }
            assert_eq!(header[..4], MAGIC, "{}: a v1 message", self.name);
            let command =
                String::from_utf8(header[4..16].iter().copied().take_while(|b| *b != 0).collect())
                    .expect("an ASCII command");
            let len = u32::from_le_bytes(header[16..20].try_into().unwrap());
            let mut payload = vec![0u8; len as usize];
            self.stream.read_exact(&mut payload).expect("the payload");
            assert_eq!(header[20..24], sha256d(&payload)[..4], "{command}'s checksum");
            if command == "ping" {
                self.send("pong", &payload);
                continue;
            }
            return Some((command, payload));
        }
    }

    fn recv(&mut self) -> (String, Vec<u8>) {
        self.try_recv().unwrap_or_else(|| panic!("{}: the node closed the connection", self.name))
    }

    /// The payload of the first message `want` accepts, within the deadline.
    fn wait(&mut self, what: &str, mut want: impl FnMut(&str, &[u8]) -> bool) -> Vec<u8> {
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < DEADLINE, "{}: no {what}", self.name);
            let (command, payload) = self.recv();
            if want(&command, &payload) {
                return payload;
            }
        }
    }

    /// A `ping` and its `pong`: the node has handled every message sent before it. The messages
    /// received on the way are returned.
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

    fn subscribe(&mut self) {
        self.send("qsendrecsigs", &[1]);
        self.barrier();
    }

    /// GETDATA for the recovered signature `hash`: its `qsigrec` payload, or `None` when the node
    /// answers `notfound` or nothing before the barrier.
    fn get_recsig(&mut self, hash: &[u8; 32]) -> Option<Vec<u8>> {
        self.send("getdata", &inventory(MSG_QUORUM_RECOVERED_SIG, hash));
        self.barrier().into_iter().find(|(c, _)| c == "qsigrec").map(|(_, p)| p)
    }

    /// The recovered signature of (`id`, `msg`) as a Core masternode relays it to a subscriber:
    /// announced by INV and fetched, or pushed as `qsigrec` when that masternode recovered it
    /// (`net_processing.cpp` v24.0.2 `RelayRecoveredSig`). Other signatures are skipped.
    fn recsig_for(&mut self, id: &[u8; 32], msg: &[u8; 32]) -> Vec<u8> {
        let request = [&id[..], &msg[..]].concat();
        let started = Instant::now();
        loop {
            assert!(started.elapsed() < DEADLINE, "{}: no recovered signature", self.name);
            let (command, payload) = self.recv();
            match command.as_str() {
                "qsigrec" if payload.get(33..SIG_AT) == Some(&request[..]) => return payload,
                "inv" => {
                    let kind = MSG_QUORUM_RECOVERED_SIG.to_le_bytes();
                    for item in payload[1..].chunks(36).filter(|i| i[..4] == kind) {
                        self.send("getdata", &[&[1u8][..], item].concat());
                    }
                }
                _ => {}
            }
        }
    }

    /// Waits for an INV of the recovered signature `hash`, then fetches it.
    fn fetch_announced(&mut self, hash: &[u8; 32]) -> Vec<u8> {
        let inv = inventory(MSG_QUORUM_RECOVERED_SIG, hash);
        self.wait("inv of the recovered signature", |c, p| c == "inv" && has_item(p, &inv[1..]));
        self.send("getdata", &inv);
        self.wait("qsigrec", |c, _| c == "qsigrec")
    }
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

/// Whether the `inv` payload `p` (one-byte count, as every vector here) holds `item`.
fn has_item(p: &[u8], item: &[u8]) -> bool {
    p.len() > 1 && p[1..].chunks(36).any(|c| c == item)
}

/// `uint256::GetHex`: the bytes reversed.
fn display(bytes: &[u8]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// A request's 32 bytes from `tag`.
fn request(tag: &str) -> [u8; 32] {
    sha256d(tag.as_bytes())
}

/// The process's one logger, behind the global subscriber; each rdashd starts it in its init.
fn logger() -> Arc<Logger> {
    static LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();
    Arc::clone(LOGGER.get_or_init(|| {
        let logger = Arc::new(Logger::new());
        tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)))
            .expect("the only global subscriber");
        logger
    }))
}

/// rdashd in this process, with RPC and a P2P listener on loopback, and one outbound peer.
struct Rdashd {
    node: Option<Node>,
    datadir: PathBuf,
    rpc_port: u16,
    p2p_port: u16,
}

impl Rdashd {
    fn start(datadir: &Path, controller: u16, mocktime: i64, extra: &[String]) -> Rdashd {
        let rpc_port = node_test_ports::free_port();
        let p2p_port = node_test_ports::free_port();
        let mut argv: Vec<String> = [
            "rdashd".to_owned(),
            "-regtest".to_owned(),
            format!("-datadir={}", datadir.display()),
            "-printtoconsole=0".to_owned(),
            "-server=1".to_owned(),
            format!("-rpcport={rpc_port}"),
            "-rpcuser=sg1a".to_owned(),
            "-rpcpassword=sg1a".to_owned(),
            "-listen=1".to_owned(),
            format!("-bind=127.0.0.1:{p2p_port}"),
            format!("-port={p2p_port}"),
            format!("-connect=127.0.0.1:{controller}"),
            format!("-mocktime={mocktime}"),
            "-debug=net".to_owned(),
            "-debug=llmq".to_owned(),
        ]
        .into();
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
        rdashd
    }

    fn stop(&mut self) {
        if let Some(mut node) = self.node.take() {
            node.interrupt();
            node.shutdown();
            assert_eq!(node.exit_status(), 0, "rdashd's exit status");
            logger().disconnect_test_logger();
        }
    }

    fn reply(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"method": method, "params": params, "id": 1}).to_string();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic c2cxYTpzZzFh\r\n\
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

    /// Waits until rdashd's tip is `tip`.
    fn wait_for_tip(&self, tip: &str) {
        let deadline = Instant::now() + Duration::from_secs(600);
        while self.try_call("getbestblockhash", json!([])).as_ref().and_then(Value::as_str)
            != Some(tip)
        {
            assert!(Instant::now() < deadline, "rdashd never reached {tip}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// rdashd's id for the inbound peer on local port `port`.
    fn peer_id(&self, port: u16) -> i64 {
        let addr = format!("127.0.0.1:{port}");
        let peers = self.call("getpeerinfo", json!([]));
        peers
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["addr"] == addr.as_str())
            .and_then(|p| p["id"].as_i64())
            .unwrap_or_else(|| panic!("no peer {addr}: {peers}"))
    }

    fn debug_log(&self) -> String {
        std::fs::read_to_string(self.datadir.join("regtest/debug.log")).unwrap_or_default()
    }
}

impl Drop for Rdashd {
    fn drop(&mut self) {
        self.stop();
        retain_test_dir(&self.datadir, "rdashd");
    }
}

/// The controller's tip block time: the network's mocktime follows it.
fn tip_time(ctx: &MasternodeTestContext) -> i64 {
    let tip = ctx.controller.get_best_block_hash().to_string();
    ctx.controller.try_rpc_call("getblock", &[tip.into()]).expect("getblock")["time"]
        .as_i64()
        .expect("a block time")
}

fn controller_tip(ctx: &MasternodeTestContext) -> String {
    ctx.controller.get_best_block_hash().to_string()
}

/// Runs `quorum sign` for (`id`, `msg`) on every Core masternode, under `quorum`.
fn sign(ctx: &MasternodeTestContext, quorum: &str, id: &[u8; 32], msg: &[u8; 32]) {
    for mn in &ctx.masternodes {
        mn.try_rpc_call(
            "quorum",
            &[
                "sign".into(),
                LLMQ_TEST.into(),
                display(id).into(),
                display(msg).into(),
                quorum.into(),
            ],
        );
    }
}

#[test]
fn a_core_recovered_sig_relays_through_rdashd_and_survives_a_restart_not_a_reindex() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Some(mut ctx) = runtime.block_on(MasternodeTestContext::new(false)) else {
        return;
    };
    let extra: Vec<String> = ctx.metadata.dashd_extra_args.clone();
    let controller = ctx.controller.p2p_port();
    let dir = tempfile::tempdir().unwrap();
    let datadir = dir.path().join("rdashd");

    let mut rdashd = Rdashd::start(&datadir, controller, tip_time(&ctx), &extra);
    rdashd.wait_for_tip(&controller_tip(&ctx));
    let quorum = ctx
        .mine_dkg_cycle_with_hook(|controller, _| {
            let tip = controller.get_best_block_hash().to_string();
            let time =
                controller.try_rpc_call("getblock", &[tip.into()]).expect("getblock")["time"]
                    .as_i64()
                    .expect("a block time");
            rdashd.call("setmocktime", json!([time]));
        })
        .expect("a DKG cycle")
        .to_string();
    rdashd.call("setmocktime", json!([tip_time(&ctx)]));
    rdashd.wait_for_tip(&controller_tip(&ctx));

    // 1. A subscribes on a member of the new quorum.
    let info = ctx
        .controller
        .try_rpc_call("quorum", &["info".into(), LLMQ_TEST.into(), quorum.clone().into()])
        .expect("quorum info");
    let member = info["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["valid"].as_bool() == Some(true))
        .and_then(|m| m["proTxHash"].as_str())
        .expect("a valid member")
        .to_owned();
    let at = ctx
        .metadata
        .masternodes
        .iter()
        .position(|mn| mn.pro_tx_hash == member)
        .expect("a member of setup-dashd's network");
    let time = tip_time(&ctx);
    let mut a = Peer::connect("A", ctx.masternodes[at].p2p_port(), time);
    a.subscribe();
    let mut c = Peer::connect("C", rdashd.p2p_port, time);
    c.subscribe();

    // 2. Two signing requests; A captures each `qsigrec`.
    let mut capture = |tag: &str| {
        let (id, msg) = (request(&format!("{tag} id")), request(&format!("{tag} msg")));
        sign(&ctx, &quorum, &id, &msg);
        let sig = a.recsig_for(&id, &msg);
        assert_eq!(sig.len(), SIG_AT + 96, "{tag}: a recovered signature");
        assert_eq!(sig[0], LLMQ_TEST, "{tag}: its type");
        assert_eq!(display(&sig[1..33]), quorum, "{tag}: its quorum");
        sig
    };
    let first = capture("sg1a w1-8 first");
    let second = capture("sg1a w1-8 second");
    let first_hash = sha256d(&first);

    // 3. B delivers the first to rdashd; 4. C gets it announced and fetches the same bytes.
    let mut b = Peer::connect("B", rdashd.p2p_port, time);
    b.send("qsigrec", &first);
    b.barrier();
    assert_eq!(c.fetch_announced(&first_hash), first, "SIG-18: C's qsigrec is A's capture");

    // 5. The second request with the first's signature: verification fails and B scores 100.
    let b_id = rdashd.peer_id(b.local_port());
    let mut forged = second[..SIG_AT].to_vec();
    forged.extend(&first[SIG_AT..]);
    b.send("qsigrec", &forged);
    let started = Instant::now();
    while b.try_recv().is_some() {
        assert!(started.elapsed() < DEADLINE, "B is never disconnected");
    }
    let misbehaving = format!("Misbehaving: peer={b_id} (0 -> 100)");
    assert!(rdashd.debug_log().contains(&misbehaving), "rdashd's log has no {misbehaving:?}");
    let mut d = Peer::connect("D", rdashd.p2p_port, time);
    assert_eq!(d.get_recsig(&sha256d(&forged)), None, "the forged signature is not stored");
    assert_eq!(d.get_recsig(&first_hash), Some(first.clone()), "the first is served");
    drop(d);

    // 6. A restart keeps the signature; `-reindex` drops it.
    rdashd.stop();
    let tip = controller_tip(&ctx);
    rdashd = Rdashd::start(&datadir, controller, tip_time(&ctx), &extra);
    rdashd.wait_for_tip(&tip);
    let mut e = Peer::connect("E", rdashd.p2p_port, tip_time(&ctx));
    assert_eq!(e.get_recsig(&first_hash), Some(first.clone()), "known after a restart");
    drop(e);
    rdashd.stop();
    let reindex = [extra.clone(), vec!["-reindex".to_owned()]].concat();
    rdashd = Rdashd::start(&datadir, controller, tip_time(&ctx), &reindex);
    rdashd.wait_for_tip(&tip);
    let mut f = Peer::connect("F", rdashd.p2p_port, tip_time(&ctx));
    assert_eq!(f.get_recsig(&first_hash), None, "gone after -reindex");
    // A stored signature is dropped unannounced, so the INV proves the wipe.
    // Core: v24.0.2 src/llmq/signing.cpp:435-437
    f.subscribe();
    let mut g = Peer::connect("G", rdashd.p2p_port, tip_time(&ctx));
    g.send("qsigrec", &first);
    g.barrier();
    assert_eq!(f.fetch_announced(&first_hash), first, "accepted anew after -reindex");
    drop(g);
    drop(f);
    rdashd.stop();
}
