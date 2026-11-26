//! MN1-7 on the whole node: `generatetodescriptor`'s block assembler selects from the node's own
//! pool, in ancestor order, through `TestBlockValidity` and the connect; the three block-assembly
//! options reach it; an unparsable `-blockmintxfee` refuses startup with Core's text.
// Core: v24.0.2 src/node/miner.cpp:66-101, :251-255, :452-456, :511-818; src/init.cpp:1363-1366

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
