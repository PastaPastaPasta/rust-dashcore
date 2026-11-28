//! A peered regtest differential. The first 42 recorded blocks fund the signed ProTx stream.
mod v24;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_consensus::header::HeaderParams;
use dash_consensus::params::Superblocks;
use dash_consensus::payments::PaymentParams;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{
    Block, BlockHeader, Header, OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxRef, TxType,
    core_serialize, decode_tx_exact,
};
use dash_p2p::rng::FastRandom;
use dash_types::Numeric as _;
use dashd_mining::{Grind, grind};
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::{CoreSubscriber, Logger};
use dashd_node::ui::Ui;
use serde_json::{Value, json};
use v24::{Dashd, T0, hex, rpc_reply, unhex};

const RECORDING: &str = include_str!("../../dashd-chainstate/tests/data/ch7a5/blocks.txt");
const BOOT: u32 = 200;
const COMMON: &[&str] = &[
    "-dip3params=48:48",
    "-testactivationheight=dip0024@1000",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-budgetparams=240:200:220",
    "-whitelist=127.0.0.1",
    "-txindex=1",
];

struct Pair {
    node: Node,
    core: Dashd,
    port: u16,
    pow: PowParams,
    wait: Duration,
    superblocks: Superblocks,
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.node.interrupt();
        self.node.shutdown();
        logger().disconnect_test_logger();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Rust,
    Core,
}

impl Side {
    fn other(self) -> Side {
        match self {
            Side::Rust => Side::Core,
            Side::Core => Side::Rust,
        }
    }
}

impl Pair {
    fn start(binary: &Path, dir: &Path, fixture: &[Arc<Block>], wait: Duration) -> (Self, TxRef) {
        let mut core_args = COMMON.to_vec();
        core_args.push("-disablewallet");
        let core = Dashd::start_listening(binary, &dir.join("dashd"), T0, &core_args);
        for b in &fixture[..42] {
            assert_eq!(core.call("submitblock", json!([hex(&core_serialize(&**b))])), Value::Null);
        }
        let address = core.call("decodescript", json!(["51"]))["p2sh"].clone();
        let mined = core.call("generatetoaddress", json!([BOOT - 42, address]));
        let funding = core.call("getblock", json!([mined[0], 2]))["tx"][0]["hex"].clone();
        let funding =
            Arc::new(decode_tx_exact(&unhex(funding.as_str().unwrap())).unwrap().into_value());
        let port = node_test_ports::free_port();
        let data = dir.join("rdashd");
        std::fs::create_dir(&data).unwrap();
        let mut argv = vec![
            "rdashd".into(),
            "-regtest".into(),
            format!("-datadir={}", data.display()),
            "-printtoconsole=0".into(),
            "-server=1".into(),
            format!("-rpcport={port}"),
            "-rpcuser=mn0".into(),
            "-rpcpassword=mn0".into(),
            "-listen=0".into(),
            format!("-connect=127.0.0.1:{}", core.p2p_port()),
            format!("-mocktime={T0}"),
            format!("-loadblock={}", core.net_dir().join("blocks/blk00000.dat").display()),
            format!("-statedigestlog={}", dir.join("state.jsonl").display()),
            "-storageprofile=production".into(),
        ];
        argv.extend(COMMON.iter().map(|s| s.to_string()));
        let args = ArgsManager::new();
        setup_server_args(&args);
        args.parse_parameters(&argv).unwrap();
        let params = init_config(&args).unwrap();
        let pow = *HeaderParams::from_chain_params(&params.chain).unwrap().pow();
        let superblocks =
            PaymentParams::from_chain_params(&params.chain).unwrap().config().superblocks;
        let mut node = Node::new(Arc::new(args), Arc::new(Ui::new(logger())), params);
        assert!(
            node.app_init_early() && node.app_init_main(),
            "rdashd startup in {}",
            data.display()
        );
        let pair = Self {
            node,
            core,
            port,
            pow,
            wait,
            superblocks,
        };
        pair.wait_tip(BOOT, Side::Rust, mined.as_array().unwrap().last().unwrap());
        let deadline = Instant::now() + wait;
        while !pair
            .call(Side::Rust, "getpeerinfo", json!([]))
            .as_array()
            .unwrap()
            .iter()
            .any(|peer| peer["version"].as_i64().is_some_and(|v| v > 0))
        {
            assert!(Instant::now() < deadline, "rdashd handshake with dashd");
            std::thread::sleep(Duration::from_millis(10));
        }
        for side in [Side::Rust, Side::Core] {
            let active = pair.call(side, "spork", json!(["active"]));
            assert_eq!(active["SPORK_2_INSTANTSEND_ENABLED"], false, "{side:?} InstantSend");
        }
        (pair, funding)
    }

    fn reply(&self, side: Side, method: &str, params: Value) -> Value {
        match side {
            Side::Rust => rpc_reply(self.port, method, params),
            Side::Core => self.core.reply(method, params),
        }
        .unwrap_or_else(|| panic!("{side:?} {method} transport failed"))
    }

    fn call(&self, side: Side, method: &str, params: Value) -> Value {
        let r = self.reply(side, method, params.clone());
        assert!(r["error"].is_null(), "{side:?} {method} {params} answered {r}");
        r["result"].clone()
    }

    fn time(&self, time: i64) {
        for side in [Side::Rust, Side::Core] {
            self.call(side, "setmocktime", json!([time]));
        }
    }

    fn wait_tip(&self, height: u32, side: Side, tip: &Value) -> Value {
        let deadline = Instant::now()
            + if height == BOOT {
                Duration::from_secs(90)
            } else {
                self.wait
            };
        loop {
            let got = self.call(side, "getbestblockhash", json!([]));
            if got == *tip {
                return got;
            }
            assert!(
                Instant::now() < deadline,
                "height {height} getbestblockhash {side:?}={got}, builder={tip}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn submit_tx(&self, height: u32, side: Side, tx: &Tx, time: i64) -> bool {
        let params = json!([hex(&core_serialize(tx))]);
        let r = self.reply(side, "sendrawtransaction", params.clone());
        if !r["error"].is_null() {
            let other = self.reply(side.other(), "sendrawtransaction", params.clone());
            assert_eq!(
                r["error"], other["error"],
                "height {height} sendrawtransaction {side:?}={r}, peer={other}"
            );
            eprintln!("height {height} both refused {}: {}", tx.txid(), r["error"]);
            return false;
        }
        let id = json!(tx.txid().to_string());
        assert_eq!(r["result"], id);
        let deadline = Instant::now() + self.wait;
        let mut tick = 0;
        loop {
            let other = self.call(side.other(), "getrawmempool", json!([]));
            if other.as_array().unwrap().contains(&id) {
                let local = self.call(side, "getrawmempool", json!([]));
                assert!(
                    local.as_array().unwrap().contains(&id),
                    "height {height} getrawmempool {side:?}={local}, peer={other}, tx={id}"
                );
                return true;
            }
            if Instant::now() >= deadline {
                let direct = self.reply(side.other(), "sendrawtransaction", params);
                panic!(
                    "height {height} getrawmempool {side:?}={}, peer={other}, tx={id}, peer's own sendrawtransaction={direct}",
                    self.call(side, "getrawmempool", json!([]))
                );
            }
            tick += 1;
            self.time(time + (tick * 5).min(95));
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn replies(&self, h: u32, method: &str, params: &Value) -> (Value, Value) {
        let r = self.reply(Side::Rust, method, params.clone());
        let c = self.reply(Side::Core, method, params.clone());
        assert!(
            r["error"].is_null() && c["error"].is_null(),
            "height {h} {method} {params} rdashd={r}, dashd={c}"
        );
        (r, c)
    }

    fn compare(
        &self,
        h: u32,
        method: &str,
        params: Value,
        view: impl Fn(&Value) -> Value,
    ) -> Value {
        let (r, c) = self.replies(h, method, &params);
        let (a, b) = (view(&r["result"]), view(&c["result"]));
        assert_eq!(a, b, "height {h} {method} {params} rdashd={r}, dashd={c}");
        a
    }

    fn utxos(&self, height: u32, tip: &Value) {
        let state = self.compare(height, "gettxoutsetinfo", json!(["muhash", null, false]), |v| {
            json!([v["bestblock"], v["txouts"], v["total_amount"], v["muhash"]])
        });
        assert!(
            state[0] == *tip
                && state[1].is_u64()
                && state[2].is_number()
                && state[3].as_str().is_some_and(|s| s.len() == 64),
            "height {height} gettxoutsetinfo {state}"
        );
    }

    /// The entries carrying `collateralAddress`, compared with the rest.
    fn protx(&self, h: u32) -> usize {
        let params = json!(["list", "registered", true]);
        let (rust, core) = self.replies(h, "protx", &params);
        let entries = |r: &Value| {
            let mut entries = r["result"].as_array().unwrap().clone();
            entries.sort_by_key(|e| e["proTxHash"].as_str().unwrap().to_owned());
            entries
        };
        let ours = entries(&rust);
        let theirs = entries(&core);
        assert_eq!(ours, theirs, "height {h} protx {params} rdashd={rust}, dashd={core}");
        theirs.iter().filter(|e| e["collateralAddress"].is_string()).count()
    }

    fn state(&self, height: u32, tip: &Value) -> usize {
        assert_eq!(self.compare(height, "getbestblockhash", json!([]), Value::clone), *tip);
        let addresses = self.protx(height);
        let quorums = self.compare(height, "quorum", json!(["list"]), Value::clone);
        assert!(
            quorums.as_object().unwrap().values().all(|q| q.as_array().unwrap().is_empty()),
            "height {height} quorum list {quorums}"
        );
        let balance = self.compare(height, "getblock", json!([tip, 2]), |v| {
            v["cbTx"]["creditPoolBalance"].clone()
        });
        assert!(balance.is_number(), "height {height} missing CbTx creditPoolBalance");
        addresses
    }
}

fn template_block(t: &Value, pow: &PowParams) -> Block {
    let mut miner = t["coinbasevalue"].as_i64().unwrap();
    let mut outputs = Vec::new();
    for p in t["masternode"].as_array().unwrap().iter().chain(t["superblock"].as_array().unwrap()) {
        let amount = p["amount"].as_i64().unwrap();
        miner -= amount;
        outputs.push(TxOut::from_duffs(amount, unhex(p["script"].as_str().unwrap()).into()));
    }
    outputs.insert(0, TxOut::from_duffs(miner, vec![0x51].into()));
    let input =
        TxIn::new(OutPoint::new(TxHash::from([0; 32]), u32::MAX), vec![0x6a].into(), u32::MAX);
    let coinbase = Transaction::new(
        3,
        TxType::CoinbaseCommitment,
        vec![input],
        outputs,
        0,
        unhex(t["coinbase_payload"].as_str().unwrap()),
    );
    let mut txs = vec![Arc::new(Tx::new(coinbase))];
    for entry in t["transactions"].as_array().unwrap() {
        txs.push(Arc::new(
            decode_tx_exact(&unhex(entry["data"].as_str().unwrap())).unwrap().into_value(),
        ));
    }
    let header = BlockHeader::new(
        t["version"].as_i64().unwrap().try_into().unwrap(),
        t["previousblockhash"].as_str().unwrap().parse().unwrap(),
        BlockHeader::default().merkle_root(),
        t["curtime"].as_u64().unwrap().try_into().unwrap(),
        u32::from_str_radix(t["bits"].as_str().unwrap(), 16).unwrap(),
        0,
    );
    let mut block = Block::new(Header::new(header), txs);
    assert_eq!(grind(&mut block, pow, &mut 1_000_000, &|| false), Grind::Found);
    block
}

fn spend(prev: &Tx, locked: i64) -> TxRef {
    let from = &prev.outputs()[0];
    let mut outputs =
        vec![TxOut::from_duffs(from.value() - 2000 - locked, from.script_pubkey().clone())];
    let mut payload = Vec::new();
    let (version, kind) = if locked > 0 {
        outputs.push(TxOut::from_duffs(locked, vec![0x6a, 0].into()));
        payload.extend([1, 1]);
        payload.extend(locked.to_le_bytes());
        payload.extend([25, 0x76, 0xa9, 0x14]);
        payload.extend([7; 20]);
        payload.extend([0x88, 0xac]);
        (3, TxType::AssetLock)
    } else {
        (2, TxType::new(0))
    };
    let input = TxIn::new(OutPoint::new(prev.txid(), 0), vec![1, 0x51].into(), u32::MAX);
    Arc::new(Tx::new(Transaction::new(version, kind, vec![input], outputs, 0, payload)))
}

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

fn env(name: &str) -> Option<String> {
    match std::env::var(format!("MN1_8C_{name}")) {
        Ok(s) => Some(s),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => panic!("MN1_8C_{name}: {e}"),
    }
}

fn number(name: &str, default: u64) -> u64 {
    env(name).map_or(default, |s| s.parse().unwrap_or_else(|e| panic!("MN1_8C_{name}={s}: {e}")))
}

fn soak(short: bool) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let Some(binary) = v24::dashd_binary() else {
        return;
    };
    let named = env("RUN_DIR");
    let temporary = named.is_none();
    let dir = named.map_or_else(
        || tempfile::Builder::new().prefix("mn1-8c-").tempdir().unwrap().keep(),
        PathBuf::from,
    );
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    assert!(
        !dir.join("rdashd").exists() && !dir.join("dashd").exists(),
        "run directory already used: {}",
        dir.display()
    );
    eprintln!("MN1-8c run directory {}", dir.display());
    let fixture = v24::blocks(RECORDING);
    let long = |name, default| {
        if short {
            default
        } else {
            number(name, default)
        }
    };
    let wait = Duration::from_secs(long("WAIT_SECONDS", 12));
    let (pair, mut coin) = Pair::start(&binary, &dir, &fixture, wait);
    let n: u32 = long(
        "BLOCKS",
        if short {
            60
        } else {
            1000
        },
    )
    .try_into()
    .unwrap();
    let cap = Duration::from_secs(long("WALL_SECONDS", 21600));
    let seed = long("SEED", 8);
    let mut key = [0; 32];
    key[..8].copy_from_slice(&seed.to_le_bytes());
    let mut rng = FastRandom::with_seed(key);
    let mode = if short {
        None
    } else {
        env("UTXO_MODE")
    }
    .unwrap_or_else(|| "every".into());
    assert!(matches!(mode.as_str(), "every" | "every10"));
    let mut counts = BTreeMap::new();
    let mut superblocks = Vec::new();
    let mut refused = 0;
    let mut verified = 0;
    let mut collateral_addresses = 0;
    let mut last_scan = None;
    let mut tip = pair.call(Side::Core, "getbestblockhash", json!([]));
    let start = Instant::now();
    for round in 1..=n {
        if start.elapsed() >= cap {
            break;
        }
        let h = BOOT + round;
        let time = T0 + 600 * i64::from(round);
        let side = if rng.randbits(1) == 0 {
            Side::Rust
        } else {
            Side::Core
        };
        let mut stream = Vec::new();
        let event = match round {
            1 => Some((116, "ProRegTx", 1)),
            2 => Some((117, "ProRegTx", 1)),
            3 => Some((141, "ProUpServTx", 2)),
            4 => Some((161, "ProUpRegTx", 3)),
            5 => Some((181, "ProUpRevTx", 4)),
            _ => None,
        };
        if let Some((at, kind, ty)) = event {
            let special =
                fixture[at - 1].txs().iter().find(|t| t.tx_type().to_base() == ty).unwrap();
            stream.push((kind, special.clone()));
        }
        let locked = if round == 1 || rng.randrange(4) == 0 {
            1000 + rng.randrange(10000) as i64
        } else {
            0
        };
        let parent = spend(&coin, locked);
        stream.push((
            if locked == 0 {
                "ordinary"
            } else {
                "asset_lock"
            },
            parent.clone(),
        ));
        if round == 2 || rng.randrange(3) == 0 {
            stream.push(("child", spend(&parent, 0)));
        }
        let mut accepted = Vec::new();
        for (i, (kind, tx)) in stream.into_iter().enumerate() {
            let send_time = time + 100 * i as i64;
            pair.time(send_time);
            if pair.submit_tx(h, side, &tx, send_time) {
                if matches!(kind, "ordinary" | "asset_lock" | "child") {
                    coin = tx.clone();
                }
                *counts.entry(kind).or_insert(0u32) += 1;
                accepted.push((kind, tx));
            } else {
                refused += 1;
            }
        }
        assert!(!accepted.is_empty(), "height {h}: both pools refused the whole stream");
        pair.time(time + 500);
        let sb = pair.superblocks.is_superblock_height(h.try_into().unwrap());
        let next = pair.call(Side::Core, "getgovernanceinfo", json!([]))["nextsuperblock"].clone();
        assert_eq!(
            sb,
            next == h,
            "height {h}: rdashd superblock={sb}, dashd nextsuperblock={next}"
        );
        let builder = if sb || h.is_multiple_of(2) {
            Side::Core
        } else {
            Side::Rust
        };
        let gbt = if sb {
            let reply = pair.reply(Side::Rust, "getblocktemplate", json!([]));
            assert_eq!(
                reply["error"],
                json!({"code":-1,"message":format!("block {h} needs superblock, which is not served until GV1b")}),
                "height {h} rdashd getblocktemplate {reply}"
            );
            superblocks.push(h);
            pair.call(builder, "getblocktemplate", json!([]))
        } else {
            let view = |t: &Value| {
                json!([t["coinbase_payload"], t["coinbasevalue"], t["masternode"], t["superblock"]])
            };
            let (r, c) = pair.replies(h, "getblocktemplate", &json!([]));
            let (r, c) = (r["result"].clone(), c["result"].clone());
            assert_eq!(view(&r), view(&c), "height {h} getblocktemplate rdashd={r}, dashd={c}");
            if builder == Side::Rust {
                r
            } else {
                c
            }
        };
        assert_eq!(gbt["height"], h);
        assert_eq!(gbt["previousblockhash"], tip);
        let block = template_block(&gbt, &pair.pow);
        for (kind, tx) in &accepted {
            assert!(
                block.txs().iter().any(|t| t.txid() == tx.txid()),
                "height {h} {builder:?} getblocktemplate missing {kind} {}",
                tx.txid()
            );
        }
        let mined = json!(block.hash().to_string());
        assert_eq!(
            pair.call(builder, "submitblock", json!([hex(&core_serialize(&block))])),
            Value::Null,
            "height {h} {builder:?} submitblock"
        );
        tip = pair.wait_tip(h, builder.other(), &mined);
        collateral_addresses += pair.state(h, &tip);
        if mode == "every" || round % 10 == 0 || sb {
            pair.utxos(h, &tip);
            last_scan = Some(h);
        }
        verified = round;
    }
    if last_scan != Some(BOOT + verified) {
        pair.utxos(BOOT + verified, &tip);
    }
    if short {
        assert_eq!(verified, 60);
        assert_eq!(superblocks, [220, 240, 260]);
        assert_eq!(counts.len(), 7);
        assert_eq!(refused, 0);
    }
    let secs = start.elapsed().as_secs_f64();
    let count = |path: &Path, sst: bool| {
        std::fs::read_dir(path).map_or(0, |entries| {
            entries
                .flatten()
                .filter(|e| !sst || e.path().extension().is_some_and(|x| x == "sst"))
                .count()
        })
    };
    let ssts = count(&dir.join("rdashd/regtest/chainstate"), true);
    let fds = count(Path::new("/proc/self/fd"), false);
    eprintln!(
        "MN1-8c blocks={verified} height={} wall_seconds={secs:.3} blocks_per_minute={:.2} utxo={mode} storage=production chainstate_sst={ssts} fds={fds} stream={counts:?} superblocks={superblocks:?} tx_refusals={refused} collateral_addresses_compared={collateral_addresses}",
        BOOT + verified,
        60.0 * f64::from(verified) / secs,
    );
    assert!(collateral_addresses > 0, "no protx entry carried collateralAddress");
    drop(pair);
    if temporary {
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn short_soak() {
    soak(true)
}

#[test]
#[ignore = "long regtest soak"]
fn long_soak() {
    soak(false)
}
