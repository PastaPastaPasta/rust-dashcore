//! P7 W3 (BL-890): rdashd's `CheckQuorumConnections` on setup-dashd's masternode network (a
//! controller and four masternodes, dashd at `DASHD_PATH`) with live DKG cycles. rdashd runs in
//! this process with `-watchquorums`, syncs from the controller, and logs Core's lines as a cycle
//! moves the scan window: the cycle quorum it keeps, and the quorum it drops once it is older than
//! `keepOldConnections` (feature_llmq_connections.py:92-93).
//!
//! Needs setup-dashd's environment (`DASHD_PATH`, `DASHD_MN_DATADIR`); `SKIP_DASHD_TESTS` skips.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_spv::test_utils::{MasternodeTestContext, retain_test_dir};
use dash_types::Numeric as _;
use dashd_masternode::sync::{MASTERNODE_SYNC_TICK_SECONDS, MASTERNODE_SYNC_TIMEOUT_SECONDS};
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::ui::Ui;
use serde_json::{Value, json};

const DEADLINE: Duration = Duration::from_secs(180);

fn logger() -> Arc<Logger> {
    static LOGGER: OnceLock<Arc<Logger>> = OnceLock::new();
    Arc::clone(LOGGER.get_or_init(|| {
        let logger = Arc::new(Logger::new());
        tracing::subscriber::set_global_default(CoreSubscriber::new(Arc::clone(&logger)))
            .expect("the only global subscriber");
        logger
    }))
}

/// rdashd in this process, watching quorums, with RPC on loopback and the controller as its one
/// outbound peer.
struct Rdashd {
    node: Option<Node>,
    datadir: PathBuf,
    rpc_port: u16,
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
            "-rpcuser=p7".to_owned(),
            "-rpcpassword=p7".to_owned(),
            format!("-bind=127.0.0.1:{p2p_port}"),
            format!("-port={p2p_port}"),
            format!("-connect=127.0.0.1:{controller}"),
            format!("-mocktime={mocktime}"),
            "-watchquorums=1".to_owned(),
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
        };
        let deadline = Instant::now() + DEADLINE;
        while rdashd.try_call("getblockcount", json!([])).is_none() {
            assert!(Instant::now() < deadline, "rdashd's RPC never came up");
            std::thread::sleep(Duration::from_millis(200));
        }
        rdashd
    }

    fn reply(&self, method: &str, params: Value) -> Option<Value> {
        let body = json!({"method": method, "params": params, "id": 1}).to_string();
        let request = format!(
            "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Basic cDc6cDc=\r\n\
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

    /// Core's `force_finish_mnsync` (test_framework/util.py:578) without `mnsync next`: with no
    /// block coming, each asset times out once mocktime passes `MASTERNODE_SYNC_TIMEOUT_SECONDS`
    /// (node/sync_manager.cpp:138-317 at v24.0.2), and `CheckQuorumConnections` runs only once it has
    /// (llmq/net_quorum.cpp:395).
    fn finish_mnsync(&self, mut time: i64) {
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

    /// Waits until the debug log holds `line`.
    fn wait_for_line(&self, line: &str) -> String {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let log = self.debug_log();
            if log.contains(line) {
                return log;
            }
            assert!(Instant::now() < deadline, "rdashd never logged {line:?}");
            std::thread::sleep(Duration::from_millis(200));
        }
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

fn tip_time(ctx: &MasternodeTestContext) -> i64 {
    let tip = ctx.controller.get_best_block_hash().to_string();
    ctx.controller.try_rpc_call("getblock", &[tip.into()]).expect("getblock")["time"]
        .as_i64()
        .expect("a block time")
}

fn controller(ctx: &MasternodeTestContext, method: &str, params: &[Value]) -> Value {
    ctx.controller.try_rpc_call(method, params).unwrap_or_else(|| panic!("controller {method}"))
}

/// The controller's newest `count` quorum hashes of `llmq_type` at its tip (`quorum list`).
fn newest_quorums(ctx: &MasternodeTestContext, llmq_type: LlmqType, count: u16) -> Vec<String> {
    let list = controller(ctx, "quorum", &["list".into(), count.into()]);
    let hashes = list[llmq_type.to_string()].as_array().cloned().unwrap_or_default();
    hashes.iter().map(|h| h.as_str().expect("a quorum hash").to_owned()).collect()
}

/// The height of the block that mined quorum `hash` of `llmq_type` (`quorum info`).
fn mined_height(ctx: &MasternodeTestContext, llmq_type: LlmqType, hash: &str) -> i64 {
    let info = controller(ctx, "quorum", &["info".into(), llmq_type.to_base().into(), hash.into()]);
    let block = info["minedBlock"].as_str().expect("a mined block").to_owned();
    controller(ctx, "getblockheader", &[block.into()])["height"].as_i64().expect("a height")
}

/// Where `llmq_type`'s check at `height` starts in `log`, and its lines: from its
/// `GetQuorumsToDelete` line, which each type's check prints first, to the next check's (rdashd's
/// tip worker runs one check at a time).
fn check_at(log: &str, llmq_type: LlmqType, height: i64) -> (usize, &str) {
    const FIRST: &str = "NetQuorum::GetQuorumsToDelete -- llmqType[";
    let first = format!("{FIRST}{}] h[{height}] ", llmq_type.to_base());
    let at = log.find(&first).unwrap_or_else(|| panic!("rdashd ran no check {first:?}"));
    let rest = &log[at + first.len()..];
    (at, &rest[..rest.find(FIRST).unwrap_or(rest.len())])
}

#[test]
fn a_watching_rdashd_keeps_the_cycle_quorum_and_removes_one_past_its_window() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let Some(mut ctx) = runtime.block_on(MasternodeTestContext::new(false)) else {
        return;
    };
    const REMOVING: &str =
        "NetQuorum::CheckQuorumConnections -- removing masternodes quorum connections for quorum ";
    const TEST: LlmqType = LlmqType::LlmqTest;
    let regtest = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let params = regtest.consensus.llmq(TEST).expect("regtest runs llmq_test");
    let extra = ctx.metadata.dashd_extra_args.clone();
    let dir = tempfile::tempdir().unwrap();
    let rdashd = Rdashd::start(
        &dir.path().join("rdashd"),
        ctx.controller.p2p_port(),
        tip_time(&ctx),
        &extra,
    );
    rdashd.wait_for_tip(&ctx.controller.get_best_block_hash().to_string());
    rdashd.finish_mnsync(tip_time(&ctx));

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
    rdashd.wait_for_tip(&ctx.controller.get_best_block_hash().to_string());

    // Core: llmq/net_quorum.cpp:408-436 at v24.0.2. At the tip, llmq_test (not rotated) keeps the
    // quorum of the cycle the tip is in.
    let tip = controller(&ctx, "getblockcount", &[]).as_i64().unwrap();
    let base = tip - tip % i64::from(params.dkg_interval);
    let base_hash = controller(&ctx, "getblockhash", &[base.into()]);
    let log = rdashd.wait_for_line(&format!(
        "NetQuorum::GetQuorumsToDelete -- llmqType[{}] h[{tip}] keeping mn quorum connections for quorum: [{base}:{}]",
        TEST.to_base(),
        base_hash.as_str().unwrap()
    ));

    // Core: llmq/net_quorum.cpp:438-497. The block that mines the cycle's quorum moves llmq_test's
    // scan (`ScanQuorums(type, tip, keepOldConnections)`, :444) past its oldest quorum, which rdashd
    // has watched since its masternode sync finished: that check drops it, and no earlier one.
    let keep = params.keep_old_connections;
    let newest = newest_quorums(&ctx, TEST, keep + 1);
    assert_eq!(newest.first(), Some(&quorum), "the cycle's quorum is llmq_test's newest");
    let dropped = newest.get(usize::from(keep)).expect("a quorum past the window");
    let removing = format!("{REMOVING}{dropped}:");
    let mined = mined_height(&ctx, TEST, &quorum);
    let (at, check) = check_at(&log, TEST, mined);
    assert!(check.contains(&removing), "h[{mined}] drops {dropped}: {check}");
    assert!(!log[..at].contains(&removing), "{dropped} is dropped before h[{mined}]");
}
