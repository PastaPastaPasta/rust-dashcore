//! MN1-7 on the whole node: `generatetodescriptor`'s block assembler selects from the node's own
//! pool, in ancestor order, through `TestBlockValidity` and the connect; the three block-assembly
//! options reach it; an unparsable `-blockmintxfee` refuses startup with Core's text.
//! MN1-7b: `getblocktemplate` on the node's engine: the template, proposals, the refusals on a
//! test chain, the longpoll's wakes, and the shutdown bump.
// Core: v24.0.2 src/node/miner.cpp:66-101, :251-255, :452-456, :511-818; src/init.cpp:1363-1366

use std::sync::mpsc;

use dash_consensus::CoreSerialize as _;
use dash_consensus::primitives::{OutPoint, Transaction, Tx, TxHash, TxIn, TxOut, TxRef, TxType};
use dashd_mempool::types::MempoolAcceptResultType;
use serde_json::Value as Json;

use super::txindex::started;
use super::*;

const OP_TRUE: u8 = 0x51;

fn rpc(table: &dashd_rpc::server::RpcServer, method: &str, params: &str) -> Json {
    let reply = crate::chain::tests::rpc(table, method, params).expect(method);
    serde_json::from_str(&reply).expect("JSON")
}

/// `generatetodescriptor n raw(51)`: the blocks' hashes.
fn mine(table: &dashd_rpc::server::RpcServer, n: u32) -> Vec<String> {
    let mined = rpc(table, "generatetodescriptor", &format!(r#"[{n}, "raw(51)"]"#));
    serde_json::from_value(mined).expect("hashes")
}

/// The txids of block `hash`, the coinbase first.
fn txids(table: &dashd_rpc::server::RpcServer, hash: &str) -> Vec<String> {
    let block = rpc(table, "getblock", &format!(r#"["{hash}", 2]"#));
    block["tx"].as_array().unwrap().iter().map(|t| t["txid"].as_str().unwrap().into()).collect()
}

/// Output 0 of block `hash`'s coinbase and its value.
fn coinbase(table: &dashd_rpc::server::RpcServer, hash: &str) -> (OutPoint, i64) {
    let block = rpc(table, "getblock", &format!(r#"["{hash}", 2]"#));
    let cb = &block["tx"][0];
    let txid: TxHash = cb["txid"].as_str().unwrap().parse().expect("a txid");
    (OutPoint::new(txid, 0), cb["vout"][0]["valueSat"].as_i64().unwrap())
}

/// `(prev, value)` to `OP_TRUE` less `fee`, with a 40-byte `OP_TRUE` output of 1000 duffs so the
/// transaction is above `tx-size-small`.
fn spend((prev, value): (OutPoint, i64), fee: i64) -> TxRef {
    let input = TxIn::new(prev, Vec::new().into(), u32::MAX);
    let outputs = vec![
        TxOut::from_duffs(value - fee - 1_000, vec![OP_TRUE].into()),
        TxOut::from_duffs(1_000, vec![OP_TRUE; 40].into()),
    ];
    Arc::new(Tx::new(Transaction::new(2, TxType::Spend, vec![input], outputs, 0, vec![])))
}

/// Output 0 of `tx` and its value.
fn out0(tx: &TxRef) -> (OutPoint, i64) {
    (OutPoint::new(tx.txid(), 0), tx.outputs()[0].value())
}

fn accept(node: &Node, tx: &TxRef) {
    let pool = node.steps.mempool().expect("step 7 built it").handle();
    let result = pool.accept(Arc::clone(tx), false).expect("V replies");
    assert_eq!(result.result_type, MempoolAcceptResultType::Valid, "{result:?}");
}

/// A node with `argv`, its pool's coinbases mature: four spendable `(outpoint, value)`.
fn matured(
    dir: &Path,
    argv: &[&str],
) -> (Node, Arc<dashd_rpc::server::RpcServer>, Vec<(OutPoint, i64)>) {
    let mut full = vec!["-acceptnonstdtxn"];
    full.extend_from_slice(argv);
    let node = started(dir, &full);
    let table = command_table(&node, dir);
    let hashes = mine(&table, 104);
    let coins = hashes[..4].iter().map(|h| coinbase(&table, h)).collect();
    (node, table, coins)
}

/// One `generatetodescriptor` block and the messages the call logs: the assembler's lines are
/// replayed on the caller's thread once the build counts.
fn mine_one(table: &dashd_rpc::server::RpcServer) -> (String, Vec<String>) {
    let (mut hashes, lines) = crate::test_log::logged(|| mine(table, 1));
    let printed = lines.into_iter().map(|(_, m)| m).filter(|m| m.starts_with("fee rate "));
    (hashes.pop().expect("one block"), printed.collect())
}

/// `generatetoaddress`'s template takes the pool's packages best first and stops at the first
/// below `-blockmintxfee`: a parent below it rides with its child, ahead of it in the block; the
/// block passes `TestBlockValidity` and connects, and the mined transactions leave the pool.
/// `-printpriority` logs each mined transaction's fee rate.
#[test]
fn generate_mines_the_pools_packages_above_blockmintxfee() {
    let dir = tempfile::tempdir().unwrap();
    let (node, table, coins) = matured(dir.path(), &["-blockmintxfee=0.0001", "-printpriority"]);
    // About 110 bytes each: `-blockmintxfee` asks about 1_100 duffs of one, the relay floor 110.
    let parent = spend(coins[0], 300);
    let child = spend(out0(&parent), 20_000);
    let low = spend(coins[1], 700);
    let high = spend(coins[2], 50_000);
    assert!((100..120).contains(&parent.size()), "{}", parent.size());
    for tx in [&parent, &child, &low, &high] {
        accept(&node, tx);
    }
    let (hash, printed) = mine_one(&table);
    assert_eq!(rpc(&table, "getbestblockhash", "[]"), Json::from(hash.clone()), "connected");
    let id = |tx: &TxRef| tx.txid().to_string();
    assert_eq!(txids(&table, &hash)[1..], [id(&high), id(&parent), id(&child)]);
    let pool = node.steps.mempool().unwrap().pool().read();
    assert_eq!(pool.size(), 1);
    assert!(pool.exists(&low.txid()));
    let want: Vec<_> =
        [&high, &parent, &child].iter().map(|t| format!(" txid {}", id(t))).collect();
    assert_eq!(printed.len(), 3, "{printed:?}");
    assert!(printed.iter().zip(&want).all(|(line, tail)| line.ends_with(tail)), "{printed:?}");
}

/// `-blockmaxsize` caps the template: past the coinbase's 1000-byte reserve, 1200 bytes hold one
/// 110-byte transaction, not two. Without `-printpriority` nothing is logged per transaction.
#[test]
fn blockmaxsize_caps_the_template() {
    let dir = tempfile::tempdir().unwrap();
    let (node, table, coins) = matured(dir.path(), &["-blockmaxsize=1200"]);
    let first = spend(coins[0], 50_000);
    let second = spend(coins[1], 40_000);
    accept(&node, &first);
    accept(&node, &second);
    let (hash, printed) = mine_one(&table);
    assert_eq!(txids(&table, &hash)[1..], [first.txid().to_string()]);
    assert!(node.steps.mempool().unwrap().pool().read().exists(&second.txid()));
    assert_eq!(printed, Vec::<String>::new());
}

/// An unparsable `-blockmintxfee` refuses startup with `AmountErrMsg`, before any load.
#[test]
fn an_unparsable_blockmintxfee_refuses_startup() {
    let dir = tempfile::tempdir().unwrap();
    let (mut node, log, stderr) = start(dir.path(), &["-blockmintxfee=x"]);
    assert!(!(node.app_init_early() && node.app_init_main()));
    assert_eq!(
        *stderr.lock().unwrap(),
        ["Error: Invalid amount for -blockmintxfee=<amount>: 'x'\n"]
    );
    assert!(!has(&log, "Loading block index"));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `CreateNewBlock(OP_TRUE)` from the node's pool, as `getblocktemplate` builds it.
fn template_block(node: &Node) -> dash_consensus::Block {
    use dashd_mining::{BlockTemplateSource as _, TemplateTxs};

    let source = node.rpc_chain.miner().expect("step 7 attached it");
    source.create_new_block(&[OP_TRUE], TemplateTxs::Mempool).expect("a template").block
}

/// `getblocktemplate` on a separate thread; its reply comes on the channel.
fn spawn_gbt(
    table: &Arc<dashd_rpc::server::RpcServer>,
    params: String,
) -> mpsc::Receiver<Result<String, (i32, String)>> {
    let (tx, rx) = mpsc::channel();
    let table = Arc::clone(table);
    std::thread::spawn(move || {
        tx.send(crate::chain::tests::rpc(&table, "getblocktemplate", &params))
    });
    rx
}

/// g1: `getblocktemplate` through the node's command table answers the node's template at the
/// tip: the parent, the pool's transaction, the coinbase's value and its CbTx payload are those
/// of `CreateNewBlock` from the same pool at the same tip.
// Core: v24.0.2 src/rpc/mining.cpp:815-982
#[test]
fn getblocktemplate_answers_the_nodes_template() {
    let dir = tempfile::tempdir().unwrap();
    let (node, table, coins) = matured(dir.path(), &[]);
    let pooled = spend(coins[0], 50_000);
    accept(&node, &pooled);
    let gbt = rpc(&table, "getblocktemplate", "[]");
    let block = template_block(&node);
    assert_eq!(gbt["previousblockhash"], Json::from(block.header().prev_hash().to_string()));
    assert_eq!(gbt["height"], Json::from(105));
    let data: Vec<_> = gbt["transactions"]
        .as_array()
        .expect("transactions")
        .iter()
        .map(|t| t["data"].as_str().expect("data").to_owned())
        .collect();
    let want: Vec<_> = block.txs()[1..].iter().map(|tx| hex(&tx.core_serialize())).collect();
    assert_eq!(data, want);
    assert!(data.contains(&hex(&pooled.core_serialize())), "the pool's transaction");
    let value: i64 = block.txs()[0].outputs().iter().map(TxOut::value).sum();
    assert_eq!(gbt["coinbasevalue"], Json::from(value));
    assert_eq!(gbt["coinbase_payload"], Json::from(hex(block.txs()[0].extra_payload())));
}

/// g2: proposal mode answers BIP22's: `null` for a valid block (the template with its merkle
/// root set; proof of work is not checked), the reject reason for an invalid one (the template
/// as `CreateNewBlock` leaves it, with a null merkle root).
// Core: v24.0.2 src/rpc/mining.cpp:697-724
#[test]
fn a_proposal_answers_its_validation_state() {
    let dir = tempfile::tempdir().unwrap();
    let (node, table, _) = matured(dir.path(), &[]);
    let unrooted = template_block(&node);
    let mut raw = *unrooted.header().raw();
    raw.set_merkle_root(unrooted.merkle().0);
    let rooted =
        dash_consensus::Block::new(dash_consensus::Header::new(raw), unrooted.txs().to_vec());
    let propose = |block: &dash_consensus::Block| {
        let params =
            format!(r#"[{{"mode": "proposal", "data": "{}"}}]"#, hex(&block.core_serialize()));
        rpc(&table, "getblocktemplate", &params)
    };
    assert_eq!(propose(&rooted), Json::Null);
    assert_eq!(propose(&unrooted), Json::from("bad-txnmrklroot"));
}

/// g3: on regtest (a test chain) none of the refusals fires: a fresh node is in initial block
/// download, has no peer and its `mn_sync` is not synced, and it answers the template on
/// genesis. The node always has its pool (step 7 builds it), so `MempoolDisabled` has no
/// whole-node case.
// Core: v24.0.2 src/rpc/mining.cpp:741-757
#[test]
fn a_test_chain_skips_the_refusals() {
    use dashd_mining::NodeStatus as _;

    let dir = tempfile::tempdir().unwrap();
    let node = started(dir.path(), &[]);
    let table = command_table(&node, dir.path());
    let info = rpc(&table, "getblockchaininfo", "[]");
    assert_eq!(info["initialblockdownload"], Json::from(true));
    let status = rpc::GbtStatus::new(&node.rpc_chain, &node.rpc_net, &node.rpc_sync);
    assert!(status.has_connman());
    assert_eq!(status.peer_count(), 0);
    assert!(!status.mn_synced());
    assert!(status.rpc_running());
    assert_eq!(rpc(&table, "getblocktemplate", "[]")["height"], Json::from(1));
}

/// g4: a waiting longpoll answers the new tip's template once a block connects.
// Core: v24.0.2 src/rpc/mining.cpp:785-796
#[test]
fn a_longpoll_returns_on_a_new_tip() {
    let dir = tempfile::tempdir().unwrap();
    let (_node, table, _) = matured(dir.path(), &[]);
    let id = rpc(&table, "getblocktemplate", "[]")["longpollid"].as_str().unwrap().to_owned();
    let reply = spawn_gbt(&table, format!(r#"[{{"longpollid": "{id}"}}]"#));
    assert!(reply.recv_timeout(Duration::from_millis(500)).is_err(), "it waits");
    let mined = mine(&table, 1);
    let answer = reply.recv_timeout(Duration::from_secs(30)).expect("woken by the tip");
    let answer: Json = serde_json::from_str(&answer.expect("a template")).unwrap();
    assert_eq!(answer["previousblockhash"], Json::from(mined[0].clone()));
}

/// g4: a waiting longpoll on an unchanged tip ends at a deadline that finds the pool's update
/// counter moved, with the new transaction in its template. The engine is the node's parts
/// with short steps.
// Core: v24.0.2 src/rpc/mining.cpp:785-799
#[test]
fn a_longpoll_returns_on_a_pool_update_after_its_step() {
    use dashd_mining::gbt::{ChainGbt, GbtParams, GbtParts, LongPoll, LongpollTimes, Mode};
    use dashd_mining::{GbtEngine, GbtReply, GbtRequest, MiningMempool as _};

    let dir = tempfile::tempdir().unwrap();
    let (node, _table, coins) = matured(dir.path(), &[]);
    let chain = node.steps.chain().expect("V runs").handle().clone();
    let pool = crate::mining_adapters::MiningPool::new(Arc::clone(node.steps.mempool().unwrap()));
    let watched = (chain.tip_watch().get().expect("a tip"), pool.transactions_updated());
    let step = Duration::from_millis(300);
    let engine = GbtEngine::new(GbtParts {
        chain: Arc::new(ChainGbt::new(chain, &node.params.chain).expect("regtest's PoW")),
        source: node.rpc_chain.miner().expect("attached"),
        mempool: Some(Arc::new(pool)),
        status: Arc::new(rpc::GbtStatus::new(&node.rpc_chain, &node.rpc_net, &node.rpc_sync)),
        clock: Arc::clone(&node.clock) as _,
        params: GbtParams::of(&node.params.chain).expect("regtest's"),
    })
    .with_longpoll_times(LongpollTimes {
        first: step,
        step,
    });
    let pooled = spend(coins[0], 50_000);
    let started = Instant::now();
    let answer = std::thread::scope(|s| {
        let waiting = s.spawn(|| {
            engine.handle(GbtRequest {
                mode: Mode::Template,
                rules: Default::default(),
                longpoll: LongPoll::Id {
                    tip: watched.0,
                    tx_updated: watched.1,
                },
            })
        });
        std::thread::sleep(step / 2);
        accept(&node, &pooled);
        waiting.join().expect("no panic")
    });
    assert!(started.elapsed() >= step, "not before the first deadline");
    let Ok(GbtReply::Template(t)) = answer else {
        panic!("expected a template, got {answer:?}");
    };
    assert_eq!(t.transactions.iter().map(|x| x.txid).collect::<Vec<_>>(), [pooled.txid()]);
}

/// g4: stopping the RPC server wakes a waiting longpoll at once, which answers Core's "Shutting
/// down", well before its 60 s deadline.
// Core: v24.0.2 src/init.cpp:564-569, src/rpc/mining.cpp:802-803
#[test]
fn stopping_rpc_ends_a_waiting_longpoll() {
    let dir = tempfile::tempdir().unwrap();
    let node = started(dir.path(), &[]);
    let table = command_table(&node, dir.path());
    let id = rpc(&table, "getblocktemplate", "[]")["longpollid"].as_str().unwrap().to_owned();
    let reply = spawn_gbt(&table, format!(r#"[{{"longpollid": "{id}"}}]"#));
    assert!(reply.recv_timeout(Duration::from_millis(500)).is_err(), "it waits");
    table.interrupt();
    table.stop();
    let answer = reply.recv_timeout(Duration::from_secs(20)).expect("woken by the stop");
    assert_eq!(answer, Err((-9, r#"{"code":-9,"message":"Shutting down"}"#.to_owned())));
}

/// g5: shutdown bumps the pool's update counter by one, first, so a longpoll on the counter sees
/// a change.
// Core: v24.0.2 src/init.cpp:328
#[test]
fn shutdown_bumps_the_pool_update_counter() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[]);
    let pool = Arc::clone(node.steps.mempool().expect("step 7 built it").pool());
    let before = pool.transactions_updated();
    node.shutdown();
    assert_eq!(pool.transactions_updated(), before.wrapping_add(1));
}
