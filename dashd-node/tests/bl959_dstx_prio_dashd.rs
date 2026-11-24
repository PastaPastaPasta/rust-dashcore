//! BL-959: a DSTX's fee delta on rdashd, on setup-dashd's masternode network (a controller and four
//! masternodes, dashd at `DASHD_PATH`) with a live DKG cycle; rdashd runs in this process as a
//! non-masternode that syncs from the controller.
//!
//! 1. The controller's wallet funds three denominated outputs (CJ1's `fund`), and a balanced
//!    zero-fee mix of them (`signed_mix`) is wrapped in a DSTX signed with a network masternode's
//!    operator key.
//! 2. A raw peer sends the `dstx` to rdashd, then to the controller and every masternode.
//!    `ValidateDSTX` prioritises it by `0.1*COIN` before `ProcessTransaction`
//!    (`net_processing.cpp:3716`, `:4959` at v24.0.2), so rdashd pools it at fee 0, modified fee
//!    0.1 DASH, where Core's `min relay fee not met` would otherwise refuse it.
//! 3. A fee-paying child of the DSTX goes to rdashd by P2P and to the controller by RPC. rdashd
//!    pools it (no orphan "with rejected parents"), and once Core's quorum locks it rdashd has the
//!    lock, all before any block.
//!
//! Needs setup-dashd's environment (`DASHD_PATH`, `DASHD_MN_DATADIR`); `SKIP_DASHD_TESTS` skips.

mod v24;

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_coinjoin::dstx::dstx_signature_hash;
use dash_consensus::hash::sha256d;
use dash_consensus::primitives::decode_tx_exact;
use dash_p2p_core::DsTx;
use dash_pkc::bls::{BlsScIetf, BlsSecretKey, BlsSigId};
use dash_primitives::TxHash;
use dash_spv::test_utils::{DashCoreNode, MasternodeTestContext, retain_test_dir};
use dash_types::codec::BaseCodec;
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::ui::Ui;
use serde_json::{Value, json};
use v24::coinjoin::{DENOMINATION, PRIORITISE_DELTA, amount, fund, signed_mix};
use v24::{Rpc, hex, unhex};

/// Regtest's message start (`chainparams.cpp`, CRegTestParams `pchMessageStart`).
const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
/// dashd 23.1's `PROTOCOL_VERSION` (`version.h`).
const VERSION: i32 = 70240;
const DEADLINE: Duration = Duration::from_secs(180);
/// The child's fee, in duffs: above the min relay fee of a one-in, one-out P2PKH spend.
const CHILD_FEE: u64 = 10_000;
/// Mixing participants: above regtest's minimum pool size.
const PARTICIPANTS: usize = 3;

/// A raw v1 P2P peer.
struct Peer {
    stream: TcpStream,
    name: &'static str,
}

impl Peer {
    /// Connects to `port` and completes the version handshake at mocktime `time`.
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
        peer
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
                .unwrap_or_else(|e| panic!("{}: the node closed the connection: {e}", self.name));
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
            return (command, payload);
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
}

fn rand_nonce() -> u64 {
    use std::hash::{BuildHasher as _, RandomState};
    RandomState::new().hash_one(Instant::now())
}

/// The process's one logger, behind the global subscriber; rdashd starts it in its init.
fn logger() -> Arc<Logger> {
    static LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();
    Arc::clone(LOGGER.get_or_init(|| {
        let logger = Arc::new(Logger::new());
        tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)))
            .expect("the only global subscriber");
        logger
    }))
}

/// rdashd in this process, with RPC and a P2P listener on loopback, and the controller as its
/// one outbound peer.
struct Rdashd {
    node: Option<Node>,
    datadir: PathBuf,
    rpc_port: u16,
    p2p_port: u16,
}

impl Rdashd {
    /// Started with `extra` after its own arguments; `extra` names the network's spork address.
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
            "-rpcuser=bl959".to_owned(),
            "-rpcpassword=bl959".to_owned(),
            "-listen=1".to_owned(),
            format!("-bind=127.0.0.1:{p2p_port}"),
            format!("-port={p2p_port}"),
            format!("-connect=127.0.0.1:{controller}"),
            format!("-mocktime={mocktime}"),
            "-debug=net".to_owned(),
            "-debug=mempool".to_owned(),
            "-debug=mempoolrej".to_owned(),
            "-debug=coinjoin".to_owned(),
            "-debug=instantsend".to_owned(),
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
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic Ymw5NTk6Ymw5NTk=\r\n\
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

    /// Sets rdashd's mocktime to the controller's tip time and waits until it has that tip.
    fn follow(&self, ctx: &MasternodeTestContext) {
        self.call("setmocktime", json!([tip_time(ctx)]));
        let tip = ctx.controller.get_best_block_hash().to_string();
        let deadline = Instant::now() + Duration::from_secs(600);
        while self.try_call("getbestblockhash", json!([])).as_ref().and_then(Value::as_str)
            != Some(tip.as_str())
        {
            assert!(Instant::now() < deadline, "rdashd never reached {tip}");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// The controller's sporks, fetched with `getsporks` and sent to rdashd, until rdashd has
    /// `SPORK_2_INSTANTSEND_ENABLED` on. dashd 23.1 sends sporks only when asked; rdashd, as
    /// v24.0.2, asks nobody and waits for each peer's announcement at `verack`
    /// (`net_processing.cpp:4233-4239` at v24.0.2).
    fn take_sporks(&self, ctx: &MasternodeTestContext) {
        let time = tip_time(ctx);
        let mut controller = Peer::connect("sporks", ctx.controller.p2p_port(), time);
        controller.send("getsporks", &[]);
        let sporks: Vec<Vec<u8>> = controller
            .barrier()
            .into_iter()
            .filter(|(command, _)| command == "spork")
            .map(|(_, payload)| payload)
            .collect();
        assert!(!sporks.is_empty(), "the controller has sporks");
        let mut relay = Peer::connect("relay", self.p2p_port, time);
        for spork in &sporks {
            relay.send("spork", spork);
        }
        relay.barrier();
        let active = self.call("spork", json!(["active"]));
        assert_eq!(active["SPORK_2_INSTANTSEND_ENABLED"], json!(true), "{active}");
    }

    /// `txid`'s verbose `getrawmempool` entry.
    fn mempool_entry(&self, txid: &str) -> Option<Value> {
        let pool = self.call("getrawmempool", json!([true]));
        pool.get(txid).cloned()
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

/// A Core node's RPC for CJ1's builders, through its wallet when it has one.
struct Core<'a>(&'a DashCoreNode);

impl Rpc for Core<'_> {
    fn call(&self, method: &str, params: Value) -> Value {
        let list = params.as_array().cloned().unwrap_or_default();
        self.0.try_rpc_call(method, &list).unwrap_or_else(|| panic!("Core {method} {params}"))
    }
}

/// The controller's tip block time: the network's mocktime follows it.
fn tip_time(ctx: &MasternodeTestContext) -> i64 {
    let tip = ctx.controller.get_best_block_hash().to_string();
    ctx.controller.try_rpc_call("getblock", &[tip.into()]).expect("getblock")["time"]
        .as_i64()
        .expect("a block time")
}

/// The network's spork address, of the key setup-dashd's Core nodes sign sporks with
/// (`-sporkkey`), so rdashd accepts their sporks, `SPORK_2_INSTANTSEND_ENABLED` among them.
fn spork_address(ctx: &MasternodeTestContext) -> String {
    let core = Core(&ctx.controller);
    let key = format!("pkh({})", ctx.metadata.spork_private_key);
    let descriptor = core.call("getdescriptorinfo", json!([key]))["descriptor"].clone();
    core.call("deriveaddresses", json!([descriptor]))[0].as_str().unwrap().to_owned()
}

/// An RPC amount in duffs.
fn duffs(value: &Value) -> i64 {
    (value.as_f64().expect("an amount") * 1e8).round() as i64
}

/// Whether Core's controller has `txid` InstantSend-locked.
fn core_locked(ctx: &MasternodeTestContext, txid: &str) -> bool {
    ctx.controller
        .try_rpc_call("getrawtransaction", &[txid.into(), true.into()])
        .is_some_and(|tx| tx["instantlock"] == json!(true))
}

/// Polls `done` every 200 ms until it holds or `limit` passes; whether it held.
fn eventually(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    true
}

/// The `dstx` payload wrapping `mix`, signed at `sig_time` by setup-dashd's masternode `at`
/// with its operator key (basic scheme, over `GetSignatureHash()`).
// Core: coinjoin/coinjoin.cpp:77-90 at v24.0.2
fn dstx(ctx: &MasternodeTestContext, at: usize, mix: &[u8], sig_time: i64) -> Vec<u8> {
    let mn = &ctx.metadata.masternodes[at];
    let secret: [u8; 32] = unhex(&mn.bls_private_key).try_into().expect("a 32-byte secret");
    let key = BlsSecretKey::<BlsScIetf>::from_bytes(&secret).expect("a scalar");
    assert_eq!(hex(&key.public_key().to_bytes()), mn.bls_public_key, "network.json's key pair");
    let tx = decode_tx_exact(mix).expect("the mix decodes").into_value().raw().clone();
    let pro = mn.pro_tx_hash.parse::<TxHash>().expect("a proTxHash");
    let hash = dstx_signature_hash(&DsTx::new(tx.clone(), pro, Vec::new(), sig_time));
    let sig = key.sign_with(hash.as_bytes(), BlsSigId::Basic).to_bytes().to_vec();
    let mut payload = Vec::new();
    DsTx::new(tx, pro, sig, sig_time).encode(&mut payload);
    payload
}

/// A wallet-signed spend of the DSTX's output 0 to a new address, paying [`CHILD_FEE`].
fn child(core: &Core<'_>, mix: &Value) -> Vec<u8> {
    let out = &mix["vout"][0];
    assert_eq!(duffs(&out["value"]) as u64, DENOMINATION, "{out}");
    let txid = &mix["txid"];
    let dest = core.call("getnewaddress", json!([]));
    let raw = core.call(
        "createrawtransaction",
        json!([[{"txid": txid, "vout": 0}], {dest.as_str().unwrap(): amount(DENOMINATION - CHILD_FEE)}]),
    );
    let prev = json!([{
        "txid": txid,
        "vout": 0,
        "scriptPubKey": out["scriptPubKey"]["hex"],
        "amount": amount(DENOMINATION),
    }]);
    let signed = core.call("signrawtransactionwithwallet", json!([raw, prev]));
    assert_eq!(signed["complete"], json!(true), "{signed}");
    unhex(signed["hex"].as_str().unwrap())
}

#[test]
fn a_validated_dstx_pools_with_its_delta_and_rdashd_gets_its_childs_islock_before_a_block() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Some(mut ctx) = runtime.block_on(MasternodeTestContext::new(false)) else {
        return;
    };
    let mut extra: Vec<String> = ctx.metadata.dashd_extra_args.clone();
    extra.push(format!("-sporkaddr={}", spork_address(&ctx)));
    let controller = ctx.controller.p2p_port();
    let dir = tempfile::tempdir().unwrap();
    let datadir = dir.path().join("rdashd");
    let rdashd = Rdashd::start(&datadir, controller, tip_time(&ctx), &extra);
    rdashd.follow(&ctx);
    rdashd.take_sporks(&ctx);
    ctx.mine_dkg_cycle_with_hook(|controller, _| {
        let tip = controller.get_best_block_hash().to_string();
        let time = controller.try_rpc_call("getblock", &[tip.into()]).expect("getblock")["time"]
            .as_i64()
            .expect("a block time");
        rdashd.call("setmocktime", json!([time]));
    })
    .expect("a DKG cycle");
    rdashd.follow(&ctx);

    // 1. Denominated outputs, mined deep enough for InstantSend (regtest's
    //    `nInstantSendConfirmationsRequired`, 2), and the signed DSTX.
    let clients = fund(&Core(&ctx.controller), PARTICIPANTS);
    ctx.move_blocks(2);
    rdashd.follow(&ctx);
    let core = Core(&ctx.controller);
    let mix = signed_mix(&core, &clients);
    let decoded = core.call("decoderawtransaction", json!([hex(&mix)]));
    let mix_txid = decoded["txid"].as_str().unwrap().to_owned();
    let payload = dstx(&ctx, 0, &mix, tip_time(&ctx));
    let height = ctx.controller.get_block_count();

    // 2. The DSTX to rdashd, then to every Core node.
    let time = tip_time(&ctx);
    let mut a = Peer::connect("A", rdashd.p2p_port, time);
    a.send("dstx", &payload);
    a.barrier();
    let pooled = rdashd.mempool_entry(&mix_txid);
    for (n, node) in std::iter::once(&ctx.controller).chain(&ctx.masternodes).enumerate() {
        let mut core_peer = Peer::connect("core", node.p2p_port(), time);
        core_peer.send("dstx", &payload);
        core_peer.barrier();
        assert!(
            node.try_rpc_call("getmempoolentry", &[mix_txid.clone().into()]).is_some(),
            "Core node {n} pools the DSTX"
        );
    }
    assert!(eventually(DEADLINE, || core_locked(&ctx, &mix_txid)), "Core locks the DSTX");

    // 3. The child to rdashd and to Core; Core's quorum locks it.
    let child = child(&core, &decoded);
    let child_txid = core.call("decoderawtransaction", json!([hex(&child)]))["txid"]
        .as_str()
        .unwrap()
        .to_owned();
    a.send("tx", &child);
    a.barrier();
    let child_pooled = rdashd.mempool_entry(&child_txid);
    core.call("sendrawtransaction", json!([hex(&child)]));
    assert!(eventually(DEADLINE, || core_locked(&ctx, &child_txid)), "Core locks the child");
    let locked = eventually(DEADLINE, || {
        rdashd.mempool_entry(&child_txid).is_some_and(|e| e["instantlock"] == json!("true"))
    });
    let log = rdashd.debug_log();
    let report = format!(
        "DSTX {mix_txid}: {pooled:?}\nchild {child_txid}: {child_pooled:?}\nrdashd's lines:\n{}",
        log.lines()
            .filter(|l| l.contains(&mix_txid[..16]) || l.contains(&child_txid[..16]))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let pooled = pooled.unwrap_or_else(|| panic!("rdashd pools the DSTX\n{report}"));
    assert_eq!(duffs(&pooled["fees"]["base"]), 0, "{report}");
    assert_eq!(duffs(&pooled["fees"]["modified"]), PRIORITISE_DELTA, "{report}");
    assert!(child_pooled.is_some(), "rdashd pools the child\n{report}");
    assert!(!log.contains("not keeping orphan with rejected parents"), "{report}");
    assert!(locked, "rdashd has Core's lock of the child\n{report}");
    let raw = rdashd.call("getrawtransaction", json!([child_txid, true]));
    assert_eq!(raw["instantlock"], json!(true), "{raw}");
    assert_eq!(ctx.controller.get_block_count(), height, "no block since the DSTX");
    assert_eq!(rdashd.call("getblockcount", json!([])), json!(height), "nor on rdashd");
    drop(a);
}
