//! MN1-8a's dashd differential: `getblocktemplate` over JSON-RPC on the whole node (rdashd in
//! this process, one GBT engine behind its RPC server) and on dashd `v24::CORE_VERSION`, with the
//! same chain and the same transactions in both pools.
//!
//! - Proposal mode (`rpc/mining.cpp:697-724`): a block built from dashd's template over a pool
//!   with a parent and its child, an asset lock and an independent spend, and mutations of it.
//!   rdashd's answer (null, or the reject reason) is dashd's for each.
//! - The template cache (`rpc/mining.cpp:809-838`): on one tip, at one mock time on both, each
//!   pool event is followed by a template on each node. The templates are equal, so a cache hit
//!   on one is a hit on the other (the longpoll id keeps the counter of the last build).
//!
//! A ChainLock for the tip is not among the events: this regtest chain registers no masternode,
//! so no quorum can sign one.
//!
//! Needs dashd `v24::CORE_VERSION` (see `v24`).
// Core: v24.0.2 src/rpc/mining.cpp:697-724, :809-838

mod v24;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_consensus::primitives::{
    Header, OutPoint, Transaction, TxIn, TxOut, TxType, core_serialize, decode_block,
};
use dash_consensus::{Block, Tx};
use dashd_node::Node;
use dashd_node::init::{init_config, setup_server_args};
use dashd_node::logging::Logger;
use dashd_node::ui::Ui;
use serde_json::{Value, json};

use v24::{Dashd, T0, dashd_binary, hex, rpc_reply, unhex};

/// Asset locks need V20; MN_RR moves part of each reward into the credit pool (CbTx version 3).
const CHAIN_ARGS: [&str; 3] =
    ["-dip3params=2:2", "-testactivationheight=v20@100", "-testactivationheight=mn_rr@100"];
/// Mined to a P2SH of `OP_TRUE`: the first 20 coinbases are mature at the tip.
const BLOCKS: usize = 120;
/// The templates' mock time.
const NOW: i64 = T0 + 1000;
const FEE: i64 = 10_000;
/// `OP_TRUE`, the P2SH redeem script every spend reveals.
const OP_TRUE: u8 = 0x51;

/// One whole node per process at a time.
static ONE_NODE: Mutex<()> = Mutex::new(());

/// A spendable output: its outpoint and value.
#[derive(Clone, Copy)]
struct Coin {
    outpoint: OutPoint,
    value: i64,
}

/// dashd and rdashd on one chain. Drop shuts rdashd down before dashd stops and the dir goes.
struct Pair {
    node: Option<Node>,
    port: u16,
    dashd: Dashd,
    /// The P2SH of `OP_TRUE`: its address and script.
    address: Value,
    p2sh: Vec<u8>,
    _dir: tempfile::TempDir,
}

impl Pair {
    fn start(binary: &Path) -> Pair {
        let dir = tempfile::tempdir().unwrap();
        let dashd = Dashd::start_with(binary, &dir.path().join("dashd"), &CHAIN_ARGS);
        let datadir = dir.path().join("rdashd");
        std::fs::create_dir_all(&datadir).unwrap();
        let port = node_test_ports::free_port();
        let mut argv: Vec<String> = [
            "rdashd".to_owned(),
            "-regtest".to_owned(),
            format!("-datadir={}", datadir.display()),
            "-printtoconsole=0".to_owned(),
            "-server=1".to_owned(),
            format!("-rpcport={port}"),
            "-rpcuser=mn0".to_owned(),
            "-rpcpassword=mn0".to_owned(),
            "-listen=0".to_owned(),
            "-connect=0".to_owned(),
            "-dnsseed=0".to_owned(),
            "-fixedseeds=0".to_owned(),
            format!("-mocktime={T0}"),
        ]
        .into();
        argv.extend(CHAIN_ARGS.iter().map(|a| (*a).to_owned()));
        let args = ArgsManager::new();
        setup_server_args(&args);
        args.parse_parameters(&argv).expect("rdashd's arguments");
        let params = init_config(&args).expect("rdashd's config");
        let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
        let mut node = Node::new(Arc::new(args), ui, params);
        assert!(node.app_init_early() && node.app_init_main(), "rdashd starts");
        let address = dashd.call("decodescript", json!([hex(&[OP_TRUE])]))["p2sh"].clone();
        let p2sh = dashd.call("validateaddress", json!([address]))["scriptPubKey"].clone();
        let pair = Pair {
            node: Some(node),
            port,
            dashd,
            address,
            p2sh: unhex(p2sh.as_str().unwrap()),
            _dir: dir,
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        while pair.reply("getblockcount", json!([])).is_none_or(|r| !r["error"].is_null()) {
            assert!(Instant::now() < deadline, "rdashd's RPC never came up");
            std::thread::sleep(Duration::from_millis(200));
        }
        pair
    }

    fn reply(&self, method: &str, params: Value) -> Option<Value> {
        rpc_reply(self.port, method, params)
    }

    fn ours(&self, method: &str, params: Value) -> Value {
        let reply = self.reply(method, params.clone()).expect("rdashd replies");
        assert!(reply["error"].is_null(), "rdashd {method} {params}: {}", reply["error"]);
        reply["result"].clone()
    }

    /// Both nodes' whole replies: dashd's, then rdashd's.
    fn both(&self, method: &str, params: Value) -> [Value; 2] {
        let core = self.dashd.reply(method, params.clone()).expect("dashd replies");
        [core, self.reply(method, params).expect("rdashd replies")]
    }

    fn set_time(&self, time: i64) {
        self.dashd.call("setmocktime", json!([time]));
        self.ours("setmocktime", json!([time]));
    }

    /// dashd mines [`BLOCKS`] to the P2SH, each block goes to rdashd; the mature coinbases.
    fn mine(&self) -> Vec<Coin> {
        let hashes = self.dashd.call("generatetoaddress", json!([BLOCKS, self.address]));
        let mut coins = Vec::new();
        for (i, hash) in hashes.as_array().unwrap().iter().enumerate() {
            let wire = self.dashd.call("getblock", json!([hash, 0]));
            assert_eq!(self.ours("submitblock", json!([wire])), Value::Null, "block {}", i + 1);
            let block = decode(wire.as_str().unwrap());
            let coinbase = &block.txs()[0];
            if i < 20 {
                let output = &coinbase.outputs()[0];
                assert_eq!(output.script_pubkey().as_bytes(), self.p2sh.as_slice());
                coins.push(Coin {
                    outpoint: OutPoint::new(coinbase.txid(), 0),
                    value: output.value(),
                });
            }
        }
        let [core, ours] = self.both("getbestblockhash", json!([]));
        assert_eq!(ours["result"], core["result"], "the same tip");
        coins
    }

    /// `tx` to dashd's pool, then rdashd's; both accept it.
    fn send(&self, tx: &Tx) {
        self.send_to_dashd(tx);
        self.send_to_ours(tx);
    }

    fn send_to_dashd(&self, tx: &Tx) {
        let id = self.dashd.call("sendrawtransaction", json!([hex(&core_serialize(tx))]));
        assert_eq!(id, json!(tx.txid().to_string()));
    }

    fn send_to_ours(&self, tx: &Tx) {
        let id = self.ours("sendrawtransaction", json!([hex(&core_serialize(tx))]));
        assert_eq!(id, json!(tx.txid().to_string()));
    }

    /// Both nodes' templates at the mock time `now`, which must be equal; dashd's.
    fn template(&self, now: i64, what: &str) -> Value {
        let [core, ours] = self.both("getblocktemplate", json!([]));
        assert!(core["error"].is_null(), "dashd's template {what}: {core}");
        let (core, ours) = (&core["result"], &ours["result"]);
        for (field, value) in core.as_object().expect("a template") {
            assert_eq!(ours.get(field), Some(value), "{field} {what}");
        }
        assert_eq!(ours, core, "the template {what}");
        let mintime = core["mintime"].as_i64().unwrap();
        assert_eq!(core["curtime"], json!(now.max(mintime)), "UpdateTime {what}");
        core.clone()
    }

    /// Both nodes' proposal answers on `block`, each a `result` beside a null `error`: dashd's,
    /// then rdashd's.
    fn propose(&self, block: &Block) -> [Value; 2] {
        let request = json!([{"mode": "proposal", "data": hex(&core_serialize(block))}]);
        self.both("getblocktemplate", request).map(|reply| {
            let reply = reply.as_object().expect("a JSON-RPC reply");
            assert_eq!(
                reply.get("error"),
                Some(&Value::Null),
                "an answer, not an error: {reply:?}"
            );
            reply.get("result").expect("a result").clone()
        })
    }

    fn pools(&self) -> [Vec<String>; 2] {
        self.both("getrawmempool", json!([])).map(|r| {
            let mut ids: Vec<String> = serde_json::from_value(r["result"].clone()).unwrap();
            ids.sort();
            ids
        })
    }
}

impl Drop for Pair {
    fn drop(&mut self) {
        if let Some(mut node) = self.node.take() {
            node.interrupt();
            node.shutdown();
        }
    }
}

fn decode(wire: &str) -> Block {
    decode_block(&mut unhex(wire).as_slice()).expect("a block").into_value()
}

fn txids(template: &Value) -> Vec<String> {
    let txs = template["transactions"].as_array().unwrap();
    txs.iter().map(|t| t["hash"].as_str().unwrap().to_owned()).collect()
}

fn id(tx: &Tx) -> String {
    tx.txid().to_string()
}

/// `uint256`'s comparison order: the txid's bytes as stored, the reverse of its hex.
fn raw_txid(tx: &Tx) -> Vec<u8> {
    let mut raw = unhex(&id(tx));
    raw.reverse();
    raw
}

fn p2sh_input(coin: Coin) -> TxIn {
    TxIn::new(coin.outpoint, vec![1, OP_TRUE].into(), u32::MAX)
}

/// `coin` to `script` less `fee`.
fn spend(coin: Coin, fee: i64, script: &[u8]) -> Tx {
    let outputs = vec![TxOut::from_duffs(coin.value - fee, script.to_vec().into())];
    Tx::new(Transaction::new(2, TxType::Spend, vec![p2sh_input(coin)], outputs, 0, vec![]))
}

/// Output 0 of `tx`.
fn first_output(tx: &Tx) -> Coin {
    Coin {
        outpoint: OutPoint::new(tx.txid(), 0),
        value: tx.outputs()[0].value(),
    }
}

/// A version-1 asset lock of `coin`: `locked` to an `OP_RETURN` output, whose credit outputs
/// sum to `credit`, and the change less `fee` to `change`.
// Core: v24.0.2 src/evo/assetlocktx.cpp:29-94
fn asset_lock(coin: Coin, locked: i64, credit: i64, fee: i64, change: &[u8]) -> Tx {
    let mut payload = vec![1, 1];
    payload.extend(credit.to_le_bytes());
    let p2pkh = [&[0x76, 0xa9, 0x14][..], &[7; 20], &[0x88, 0xac]].concat();
    payload.push(p2pkh.len() as u8);
    payload.extend(p2pkh);
    let outputs = vec![
        TxOut::from_duffs(coin.value - locked - fee, change.to_vec().into()),
        TxOut::from_duffs(locked, vec![0x6a, 0].into()),
    ];
    Tx::new(Transaction::new(3, TxType::AssetLock, vec![p2sh_input(coin)], outputs, 0, payload))
}

fn compact_size(n: usize, out: &mut Vec<u8>) {
    assert!(n < 0xfd, "short vectors only");
    out.push(n as u8);
}

/// `CScript() << n`: `CScriptNum`'s minimal little-endian bytes, pushed.
fn push_num(n: i64, out: &mut Vec<u8>) {
    assert!(n > 16, "heights past the small-integer opcodes");
    let mut bytes = n.to_le_bytes().to_vec();
    while bytes.last() == Some(&0) {
        bytes.pop();
    }
    if bytes.last().is_some_and(|b| b & 0x80 != 0) {
        bytes.push(0);
    }
    out.push(bytes.len() as u8);
    out.extend(bytes);
}

/// A miner's block from `template`: a miner-built coinbase (scriptSig the height and `OP_0`,
/// `coinbase_payload` as its CbTx, `OP_TRUE` taking the value the payees leave, `extra` duffs
/// more), then the template's transactions.
// Core: v24.0.2 src/rpc/mining.cpp:866-898, :940-1000
fn block_from(template: &Value, extra: i64) -> Block {
    let mut payees: Vec<(i64, Vec<u8>)> = Vec::new();
    for payee in ["masternode", "superblock"].iter().flat_map(|k| template[k].as_array().unwrap()) {
        payees.push((payee["amount"].as_i64().unwrap(), unhex(payee["script"].as_str().unwrap())));
    }
    let paid: i64 = payees.iter().map(|p| p.0).sum();
    let mut outputs =
        vec![(template["coinbasevalue"].as_i64().unwrap() - paid + extra, vec![OP_TRUE])];
    outputs.extend(payees);

    let mut coinbase = vec![3, 0, 5, 0, 1];
    coinbase.extend([0; 32]);
    coinbase.extend(u32::MAX.to_le_bytes());
    let mut script = Vec::new();
    push_num(template["height"].as_i64().unwrap(), &mut script);
    script.push(0);
    compact_size(script.len(), &mut coinbase);
    coinbase.extend(script);
    coinbase.extend(u32::MAX.to_le_bytes());
    compact_size(outputs.len(), &mut coinbase);
    for (value, script) in outputs {
        coinbase.extend(value.to_le_bytes());
        compact_size(script.len(), &mut coinbase);
        coinbase.extend(script);
    }
    coinbase.extend(0u32.to_le_bytes());
    let payload = unhex(template["coinbase_payload"].as_str().unwrap());
    compact_size(payload.len(), &mut coinbase);
    coinbase.extend(payload);

    let mut prev = unhex(template["previousblockhash"].as_str().unwrap());
    prev.reverse();
    let bits = u32::from_str_radix(template["bits"].as_str().unwrap(), 16).unwrap();
    let mut wire =
        i32::try_from(template["version"].as_i64().unwrap()).unwrap().to_le_bytes().to_vec();
    wire.extend(prev);
    wire.extend([0; 32]);
    wire.extend(u32::try_from(template["curtime"].as_i64().unwrap()).unwrap().to_le_bytes());
    wire.extend(bits.to_le_bytes());
    wire.extend(0u32.to_le_bytes());
    let txs = template["transactions"].as_array().unwrap();
    compact_size(txs.len() + 1, &mut wire);
    wire.extend(coinbase);
    for tx in txs {
        wire.extend(unhex(tx["data"].as_str().unwrap()));
    }
    let block = decode(&hex(&wire));
    with_txs(&block, block.txs().to_vec())
}

/// `block` with `txs`, its merkle root recomputed (proposal mode checks it, not the PoW).
fn with_txs(block: &Block, txs: Vec<Arc<Tx>>) -> Block {
    let mut raw = *block.header().raw();
    raw.set_merkle_root(Block::new(Header::new(raw), txs.clone()).merkle().0);
    Block::new(Header::new(raw), txs)
}

/// `block` with its coinbase's CbTx payload edited by `edit`.
fn edit_cbtx(block: &Block, edit: impl FnOnce(&mut Vec<u8>)) -> Block {
    let mut txs = block.txs().to_vec();
    let tx = txs[0].to_mutable();
    let mut payload = tx.extra_payload().clone();
    edit(&mut payload);
    let tx = Transaction::new(
        tx.version(),
        tx.tx_type(),
        tx.inputs().clone(),
        tx.outputs().clone(),
        tx.lock_time(),
        payload,
    );
    txs[0] = Arc::new(Tx::new(tx));
    with_txs(block, txs)
}

/// Adds one to the little-endian `u32` at `at`.
fn bump_u32(bytes: &mut [u8], at: usize) {
    let v = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    bytes[at..at + 4].copy_from_slice(&v.wrapping_add(1).to_le_bytes());
}

fn position(block: &Block, tx: &Tx) -> usize {
    block.txs().iter().position(|t| t.txid() == tx.txid()).expect("in the block")
}

/// Proposals of a block built from dashd's template over a pool with a parent and its child, an
/// asset lock and an independent spend, then of its mutations: rdashd answers each as dashd.
#[test]
fn proposals_with_pooled_transactions_answer_as_dashd() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _one = ONE_NODE.lock().unwrap_or_else(|p| p.into_inner());
    let pair = Pair::start(&binary);
    let coins = pair.mine();
    pair.set_time(NOW);
    let p2sh = pair.p2sh.clone();
    let parent = spend(coins[0], FEE, &p2sh);
    let child = spend(first_output(&parent), FEE, &p2sh);
    let lock = asset_lock(coins[1], 1_000_000, 1_000_000, FEE, &p2sh);
    let independent = spend(coins[2], FEE, &p2sh);
    for tx in [&parent, &child, &lock, &independent] {
        pair.send(tx);
    }
    let template = pair.template(NOW, "over the pool");
    let mut pooled = txids(&template);
    pooled.retain(|t| [&parent, &child, &lock, &independent].iter().any(|tx| id(tx) == *t));
    assert_eq!(pooled.len(), 4, "the template selects the pool");

    let block = block_from(&template, 0);
    let [core, ours] = pair.propose(&block);
    assert_eq!(core, Value::Null, "dashd accepts the block built from its template");
    assert_eq!(ours, core, "the valid block");

    let txs = block.txs().to_vec();
    let mut dropped = txs.clone();
    dropped.remove(position(&block, &parent));
    let mut reordered = txs.clone();
    reordered.swap(position(&block, &parent), position(&block, &child));
    let mut double = txs.clone();
    double.push(Arc::new(spend(coins[2], 2 * FEE, &[OP_TRUE])));
    let mut invalid = txs.clone();
    invalid.push(Arc::new(asset_lock(coins[3], 1_000_000, 999_999, FEE, &p2sh)));
    let missing = "bad-txns-inputs-missingorspent";
    let mutants = [
        ("parent dropped, child kept", with_txs(&block, dropped), missing),
        ("child before its parent", with_txs(&block, reordered), missing),
        ("a double spend", with_txs(&block, double), missing),
        ("an invalid asset lock", with_txs(&block, invalid), "bad-assetlocktx-creditamount"),
        ("a coinbase over the fees", block_from(&template, 1), "bad-cb-amount"),
        ("a cbtx height", edit_cbtx(&block, |p| bump_u32(p, 2)), "bad-cbtx-height"),
        ("a cbtx list root", edit_cbtx(&block, |p| p[6] ^= 1), "bad-cbtx-mnmerkleroot"),
        (
            "a cbtx credit pool",
            edit_cbtx(&block, |p| {
                let at = p.len() - 8;
                bump_u32(p, at);
            }),
            "bad-cbtx-assetlocked-amount",
        ),
    ];
    for (name, mutant, reason) in &mutants {
        let [core, ours] = pair.propose(mutant);
        assert_eq!(core, json!(reason), "dashd's reason for {name}");
        assert_eq!(ours, core, "{name}");
    }
    let [core, ours] = pair.propose(&block_from(&template, -1));
    assert_eq!((&core, &ours), (&Value::Null, &Value::Null), "a coinbase under the fees");

    let [core, ours] = pair.propose(&block);
    assert_eq!((&core, &ours), (&Value::Null, &Value::Null), "the valid block again");
}

/// The template cache on one tip, both nodes at each mock time: a pool change within 5 s of the
/// last build is a hit, after 5 s a rebuild; two asset locks of one fee rate, pooled on rdashd
/// against Core's order, are templated in Core's order, ascending raw txid; a conflicting spend
/// of a pooled input, which both pools refuse, leaves the cache as it was, and the next pool
/// change counts from the last build.
// Core: v24.0.2 src/rpc/mining.cpp:812-834, src/txmempool.h:158-178, src/uint256.h:55-59
#[test]
fn same_tip_templates_hit_and_rebuild_as_dashd() {
    let Some(binary) = dashd_binary() else {
        return;
    };
    let _one = ONE_NODE.lock().unwrap_or_else(|p| p.into_inner());
    let pair = Pair::start(&binary);
    let coins = pair.mine();
    let p2sh = pair.p2sh.clone();

    pair.set_time(NOW);
    let built = pair.template(NOW, "first");
    assert_eq!(txids(&built), Vec::<String>::new());

    let ordinary = spend(coins[0], FEE, &p2sh);
    pair.set_time(NOW + 3);
    pair.send(&ordinary);
    let within = pair.template(NOW + 3, "(a) 3 s after the build");
    assert_eq!(txids(&within), txids(&built), "(a) a hit");
    assert_eq!(within["longpollid"], built["longpollid"], "(a) the build's counter");
    pair.set_time(NOW + 5);
    let at_five = pair.template(NOW + 5, "(a) 5 s after the build");
    assert_eq!(at_five["longpollid"], built["longpollid"], "(a) 5 s is still a hit");

    pair.set_time(NOW + 6);
    let rebuilt = pair.template(NOW + 6, "(b) 6 s after the build");
    assert_eq!(txids(&rebuilt), [id(&ordinary)], "(b) a rebuild");
    assert_ne!(rebuilt["longpollid"], built["longpollid"], "(b) the new counter");

    let mut pair_of_locks =
        [1, 2].map(|i| asset_lock(coins[i], 1_000_000, 1_000_000, 4 * FEE, &p2sh));
    pair_of_locks.sort_by_key(raw_txid);
    let [low, high] = &pair_of_locks;
    assert_eq!(core_serialize(low).len(), core_serialize(high).len(), "one fee rate");
    pair.set_time(NOW + 7);
    pair.send_to_dashd(low);
    pair.send_to_dashd(high);
    pair.send_to_ours(high);
    pair.send_to_ours(low);
    let hit = pair.template(NOW + 7, "(c) 1 s after the rebuild");
    assert_eq!(hit["longpollid"], rebuilt["longpollid"], "(c) a hit");
    pair.set_time(NOW + 12);
    let locks = pair.template(NOW + 12, "(c) 6 s after the rebuild");
    let selected = txids(&locks);
    assert_eq!(
        selected,
        [id(low), id(high), id(&ordinary)],
        "(c) the locks by raw txid, then the spend"
    );
    assert_ne!(locks["longpollid"], rebuilt["longpollid"], "(c) the new counter");

    pair.set_time(NOW + 13);
    let alternative = spend(coins[0], 3 * FEE, &p2sh);
    let [core, ours] = pair.both("sendrawtransaction", json!([hex(&core_serialize(&alternative))]));
    assert_eq!(core["error"]["message"], "txn-mempool-conflict", "dashd keeps the pooled spend");
    assert_eq!(ours["error"], core["error"], "(d) the refusal");
    let [core_pool, our_pool] = pair.pools();
    assert_eq!(our_pool, core_pool, "(d) the pools");
    assert!(core_pool.contains(&id(&ordinary)) && !core_pool.contains(&id(&alternative)));
    let kept = pair.template(NOW + 13, "(d) 1 s after the rebuild");
    assert_eq!(kept["longpollid"], locks["longpollid"], "(d) a hit");
    pair.set_time(NOW + 20);
    let unchanged = pair.template(NOW + 20, "(d) 8 s after the rebuild");
    assert_eq!(unchanged["longpollid"], locks["longpollid"], "(d) no pool change, no rebuild");
    assert_eq!(txids(&unchanged), selected);

    let late = spend(coins[3], FEE, &p2sh);
    pair.set_time(NOW + 21);
    pair.send(&late);
    let after = pair.template(NOW + 21, "(d) a pool add 9 s after the last build");
    assert!(txids(&after).contains(&id(&late)), "(d) a rebuild counted from +12, not +20");
}
