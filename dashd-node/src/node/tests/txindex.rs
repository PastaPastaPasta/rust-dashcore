//! IX1 e2: the txindex an in-process regtest node starts at Step 8, across restarts, `-reindex`
//! and `-reindex-chainstate`. Core citations are v24.0.0.

use dashd_chainstate::subscriber::decode_locator;
use dashd_indexes::txindex::codec::{decode_epoch, decode_seq};
use dashd_indexes::txindex::data::SEQ_BITS;
use dashd_indexes::txindex::{BEST_BLOCK, EPOCH, META, TX, TXINDEX_DB};
use dashd_indexes::{IndexStatus as _, TxIndexReader as _, Txid};
use dashd_store::{Commit, KvBatch as _, KvEngine as _, KvRead as _, RocksConfig, RocksEngine};

use super::*;
use crate::indexes::{get_transaction, txindex_dir};

/// A node on `dir` through init, its `loadblk` joined.
pub(super) fn started(dir: &Path, argv: &[&str]) -> Node {
    let (mut node, _, stderr) = start(dir, argv);
    let started = node.app_init_early() && node.app_init_main();
    let errors = stderr.lock().unwrap().clone();
    assert!(started, "{errors:?}");
    assert!(node.steps.join_import(), "loadblk ends");
    node
}

fn wait_synced(node: &Node, height: u32) {
    let txindex = node.txindex().expect("the txindex runs");
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let s = txindex.summary();
        if s.synced && s.best_block_height == height {
            return;
        }
        assert!(Instant::now() < end, "{s:?}, not synced at {height}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The coinbase txid of the block `hash`, from `getblock`.
fn coinbase(node: &Node, dir: &Path, hash: &str) -> Txid {
    let table = command_table(node, dir);
    let block = crate::chain::tests::rpc(&table, "getblock", &format!(r#"["{hash}", 1]"#));
    let block: serde_json::Value = serde_json::from_str(&block.expect("getblock")).unwrap();
    block["tx"][0].as_str().expect("a coinbase").parse().unwrap()
}

/// `GetTransaction(nullptr, mempool, txid)`'s block, as hex.
fn found_in(node: &Node, txid: &Txid) -> Option<String> {
    let (_, block) = get_transaction(None, node.txindex(), txid)?;
    Some(block.expect("an indexed transaction has a block").to_string())
}

/// The stopped index's durable locator and epoch.
fn stored(dir: &Path) -> (Option<String>, u32) {
    let path = txindex_dir(&dir.join("regtest"));
    let db = RocksEngine::open_read_only(&path, &TXINDEX_DB).unwrap().expect("the txindex DB");
    let locator = db.get(META, BEST_BLOCK).unwrap().as_deref().and_then(decode_locator);
    let epoch = db.get(META, EPOCH).unwrap().map_or(0, |e| decode_epoch(&e).unwrap());
    (locator.map(|h| h.to_string()), epoch)
}

/// `-txindex=1` indexes the mined blocks and finds a transaction by txid; the stop's final
/// commit is durable at the tip. A restart resumes the same DB from its locator, `-reindex`
/// wipes it and rebuilds, and `-reindex-chainstate` keeps it and syncs once the import ends.
// Core: init.cpp:2273-2279, :421-424; index/base.cpp:94-166, :522-529
#[test]
fn the_txindex_syncs_finds_by_txid_and_survives_restarts_and_reindexes() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=1"]);
    let mut mined = generate(&node, dir.path(), 3);
    wait_synced(&node, 3);
    let first = coinbase(&node, dir.path(), &mined[1]);
    assert_eq!(found_in(&node, &first), Some(mined[1].clone()));
    assert_eq!(found_in(&node, &Txid::from([7; 32])), None);
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()), (Some(mined[2].clone()), 1), "the final commit at the tip");

    let mut node = started(dir.path(), &["-txindex=1"]);
    wait_synced(&node, 3);
    mined.extend(generate(&node, dir.path(), 2));
    wait_synced(&node, 5);
    let last = coinbase(&node, dir.path(), &mined[4]);
    assert_eq!(found_in(&node, &last), Some(mined[4].clone()));
    assert_eq!(found_in(&node, &first), Some(mined[1].clone()));
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()), (Some(mined[4].clone()), 2), "the same DB, resumed");

    for (argv, epoch) in [("-reindex", 1), ("-reindex-chainstate", 2)] {
        let mut node = started(dir.path(), &["-txindex=1", argv]);
        wait_synced(&node, 5);
        assert_eq!(found_in(&node, &first), Some(mined[1].clone()), "{argv}");
        assert_eq!(found_in(&node, &last), Some(mined[4].clone()), "{argv}");
        node.shutdown();
        drop(node);
        assert_eq!(stored(dir.path()), (Some(mined[4].clone()), epoch), "{argv}");
    }
}

/// `-txindex` on a chain that synced without it catches up from genesis and finds a mined
/// transaction; the stop's final commit is durable at the tip.
// Core: init.cpp:2277-2279; index/base.cpp:94-166, :189-268
#[test]
fn a_late_enable_catches_up_and_finds_transactions() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=0"]);
    let mined = generate(&node, dir.path(), 3);
    assert!(node.txindex().is_none());
    let txid = coinbase(&node, dir.path(), &mined[1]);
    node.shutdown();
    drop(node);
    assert!(!dir.path().join("regtest").join("indexes").join("txindex").exists());

    let mut node = started(dir.path(), &["-txindex=1"]);
    wait_synced(&node, 3);
    assert_eq!(found_in(&node, &txid), Some(mined[1].clone()));
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()).0, Some(mined[2].clone()), "the final commit at the tip");
}

/// All `(key, value)` rows in the stopped txindex's `TX` column family.
fn tx_rows(dir: &Path) -> Vec<(Vec<u8>, Vec<u8>)> {
    let path = txindex_dir(&dir.join("regtest"));
    let db = RocksEngine::open_read_only(&path, &TXINDEX_DB).unwrap().expect("the txindex DB");
    db.iter_prefix(TX, &[]).map(Result::unwrap).collect()
}

/// A restart with the txindex behind the chain, as a stop mid catch-up leaves it, resumes from
/// the stored locator: the epoch is the next open of the same DB, rows at heights <= 3 keep
/// epoch 1 (not rewritten), and rows mined while the index was off are written in epoch 2.
// Core: index/base.cpp:94-166 (Init), :189-268 (ThreadSync)
#[test]
fn a_restart_resumes_the_txindex_from_its_locator() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=1"]);
    let mut mined = generate(&node, dir.path(), 3);
    wait_synced(&node, 3);
    let early = coinbase(&node, dir.path(), &mined[1]);
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()), (Some(mined[2].clone()), 1));
    let before = tx_rows(dir.path());
    assert_eq!(before.len(), 3, "heights 1..=3");
    assert!(before.iter().all(|(_, v)| (decode_seq(v).unwrap() >> SEQ_BITS) == 1));

    let mut node = started(dir.path(), &["-txindex=0"]);
    mined.extend(generate(&node, dir.path(), 2));
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()).0, Some(mined[2].clone()), "behind the chain");

    let mut node = started(dir.path(), &["-txindex=1"]);
    wait_synced(&node, 5);
    let later = coinbase(&node, dir.path(), &mined[4]);
    assert_eq!(found_in(&node, &early), Some(mined[1].clone()));
    assert_eq!(found_in(&node, &later), Some(mined[4].clone()));
    node.shutdown();
    drop(node);
    assert_eq!(stored(dir.path()), (Some(mined[4].clone()), 2), "the same DB, resumed");
    let after = tx_rows(dir.path());
    assert_eq!(after.len(), 5, "heights 1..=5");
    for row in &before {
        assert!(after.contains(row), "height <= 3 row kept epoch 1 without rewrite");
    }
    let added: Vec<_> = after.iter().filter(|row| !before.contains(row)).collect();
    assert_eq!(added.len(), 2, "only heights 4..=5 written on resume");
    assert!(added.iter().all(|(_, v)| (decode_seq(v).unwrap() >> SEQ_BITS) == 2));
}

/// `-txindex=0` runs no index: no handle and no DB. Without the option the index runs, as
/// Core's `DEFAULT_TXINDEX` is on.
// Core: init.cpp:2277; index/txindex.h:10
#[test]
fn txindex_off_runs_no_index_and_the_default_runs_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=0"]);
    generate(&node, dir.path(), 1);
    assert!(node.txindex().is_none());
    node.shutdown();
    drop(node);
    assert!(!dir.path().join("regtest").join("indexes").exists());

    let mut node = started(dir.path(), &[]);
    wait_synced(&node, 1);
    let txindex = node.txindex().expect("on by default");
    let mined = generate(&node, dir.path(), 1);
    wait_synced(&node, 2);
    let txid = coinbase(&node, dir.path(), &mined[0]);
    assert!(txindex.has_tx(&txid).unwrap());
    node.shutdown();
}

/// A masternode needs the txindex: with `-txindex=1` it passes init's check and the index runs.
/// `-txindex=0` is refused as before (`flags.rs`' `masternode_refusals`).
// Core: init.cpp:1389-1391
#[test]
fn a_masternode_with_the_txindex_starts_it() {
    let dir = tempfile::tempdir().unwrap();
    let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
    let argv = [&key, "-txindex=1", "-dnsseed=0", "-fixedseeds=0"];
    let (mut node, _, stderr) = start(dir.path(), &argv);
    let started = node.app_init_early() && node.app_init_main();
    let errors = stderr.lock().unwrap().clone();
    assert!(started, "{errors:?}");
    wait_synced(&node, 0);
    node.interrupt();
    node.shutdown();
}

/// Shutdown closes the txindex's database, as Core's `g_txindex.reset()` does before the data
/// directory's lock is released: a node started while the old one is still alive opens it.
// Core: init.cpp:421-424, :492
#[test]
fn shutdown_closes_the_txindex_db() {
    let dir = tempfile::tempdir().unwrap();
    let mut old = started(dir.path(), &["-txindex=1"]);
    generate(&old, dir.path(), 1);
    wait_synced(&old, 1);
    old.shutdown();
    assert!(old.txindex().expect("the handle").current().is_none());
    let mut node = started(dir.path(), &["-txindex=1"]);
    wait_synced(&node, 1);
    node.shutdown();
    drop(old);
}

/// A Step 8 open failure, a locator the block index does not know, is init's one `InitError`.
/// Under `-reindex-chainstate` the open waits for the import, so the same failure is the node's
/// one `AbortNode` there (Core still runs `Init`'s check at Step 8).
// Core: init.cpp:2277-2279; index/base.cpp:101-107, :194
#[test]
fn an_unknown_locator_is_reported_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=1"]);
    generate(&node, dir.path(), 1);
    wait_synced(&node, 1);
    node.shutdown();
    drop(node);
    let path = txindex_dir(&dir.path().join("regtest"));
    let (db, _) = RocksEngine::open(&path, &TXINDEX_DB, &RocksConfig::default()).unwrap();
    let mut batch = db.batch();
    let mut locator = vec![0, 0, 0, 0, 1];
    locator.extend_from_slice(&[7; 32]);
    batch.put(META, BEST_BLOCK, &locator);
    db.write(batch, Commit::Durable).unwrap();
    drop(db);

    let refusal = "txindex: best block of the index not found. Please rebuild the index.";
    let (mut node, _, stderr) = start(dir.path(), &["-txindex=1"]);
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")]);
    drop(node);

    let (mut node, log, stderr) = start(dir.path(), &["-txindex=1", "-reindex-chainstate"]);
    assert!(node.app_init_early() && node.app_init_main());
    assert!(node.steps.join_import());
    assert_eq!(stderr.lock().unwrap().len(), 1, "{:?}", stderr.lock().unwrap());
    assert_eq!(log.lock().unwrap().iter().filter(|l| l.contains(refusal)).count(), 1);
    node.shutdown();
}

/// IX1 f: `getrawtransaction` serves a mined transaction over the txindex, by txid and by its
/// block, and refuses the genesis coinbase; `getindexinfo` shows the txindex synced at the tip.
/// With `-txindex=0` the same lookup answers Core's error, and `getindexinfo` is empty.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:338-509, src/rpc/node.cpp:1177-1232
#[test]
fn getrawtransaction_and_getindexinfo_serve_the_txindex() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=1"]);
    let mined = generate(&node, dir.path(), 3);
    wait_synced(&node, 3);
    let table = command_table(&node, dir.path());
    let rpc = |method: &str, params: &str| crate::chain::tests::rpc(&table, method, params);
    let txid = coinbase(&node, dir.path(), &mined[1]);
    let hex = rpc("getrawtransaction", &format!(r#"["{txid}"]"#)).expect("found");
    let block = rpc("getblock", &format!(r#"["{}", 0]"#, mined[1])).unwrap();
    assert!(block.contains(hex.trim_matches('"')), "the block's coinbase");
    let verbose: serde_json::Value =
        serde_json::from_str(&rpc("getrawtransaction", &format!(r#"["{txid}", 1]"#)).unwrap())
            .unwrap();
    assert_eq!(verbose["txid"], txid.to_string());
    assert_eq!(verbose["blockhash"], mined[1]);
    assert_eq!((verbose["height"].as_i64(), verbose["confirmations"].as_i64()), (Some(2), Some(2)));
    let in_block = format!(r#"["{txid}", 2, "{}"]"#, mined[1]);
    let by_block: serde_json::Value =
        serde_json::from_str(&rpc("getrawtransaction", &in_block).unwrap()).unwrap();
    assert_eq!(by_block["in_active_chain"], true);
    assert_eq!(by_block["hex"].as_str(), Some(hex.trim_matches('"')));
    let other = format!(r#"["{txid}", 0, "{}"]"#, mined[2]);
    let missing = "No such transaction found in the provided block. Use gettransaction for wallet transactions.";
    assert_eq!(rpc("getrawtransaction", &other).unwrap_err(), reply(-5, missing));
    let genesis_hash = rpc("getblockhash", "[0]").unwrap();
    let genesis: serde_json::Value =
        serde_json::from_str(&rpc("getblock", &format!("[{genesis_hash}]")).unwrap()).unwrap();
    let genesis_coinbase = format!(r#"["{}"]"#, genesis["tx"][0].as_str().unwrap());
    let refused = "The genesis block coinbase is not considered an ordinary transaction and cannot be retrieved";
    assert_eq!(rpc("getrawtransaction", &genesis_coinbase).unwrap_err(), reply(-5, refused));
    let info = r#"{"txindex":{"synced":true,"best_block_height":3}}"#;
    assert_eq!(rpc("getindexinfo", "[]").unwrap(), info);
    assert_eq!(rpc("getindexinfo", r#"["txindex"]"#).unwrap(), info);
    drop(table);
    node.shutdown();
    drop(node);

    let mut node = started(dir.path(), &["-txindex=0"]);
    let table = command_table(&node, dir.path());
    let rpc = |method: &str, params: &str| crate::chain::tests::rpc(&table, method, params);
    let off = "No such mempool transaction. Use -txindex or provide a block hash to enable blockchain transaction queries. Use gettransaction for wallet transactions.";
    assert_eq!(rpc("getrawtransaction", &format!(r#"["{txid}"]"#)).unwrap_err(), reply(-5, off));
    let in_block = format!(r#"["{txid}", 0, "{}"]"#, mined[1]);
    assert_eq!(rpc("getrawtransaction", &in_block).unwrap(), hex, "a block is still read");
    assert_eq!(rpc("getindexinfo", "[]").unwrap(), "{}");
    drop(table);
    node.shutdown();
}

/// Puts a transaction no block holds into the node's pool, unvalidated, and returns its txid
/// and hex.
fn pooled(node: &Node) -> (Txid, String) {
    use dash_consensus::CoreSerialize as _;
    use dash_primitives::{OutPoint, Transaction, TxIn, TxOut, TxType};
    use dashd_mempool::pool::EntryMetadata;

    let input = TxIn::new(OutPoint::new(Txid::from([1; 32]), 0), vec![].into(), u32::MAX);
    let output = TxOut::from_duffs(1, vec![0x51].into());
    let raw = Transaction::new(1, TxType::new(0), vec![input], vec![output], 0, vec![]);
    let tx = Arc::new(dash_consensus::Tx::new(raw));
    let hex = tx.raw().core_serialize().iter().map(|b| format!("{b:02x}")).collect();
    let txid = tx.txid();
    let pool = node.mempool().expect("step 7 built it").pool();
    let mut state = pool.write();
    let entry = state.new_entry(tx, EntryMetadata::default());
    state.add_unchecked(entry, &dash_dml::MnList::default_pre_dip3());
    pool.publish_usage(&state);
    (txid, hex)
}

/// IX1-f-tx2: without a block `getrawtransaction` reads the node's mempool, with or without the
/// txindex: the hex, and `TxToJSON` without a block (dashd v24.0.0's keys for a mempool
/// transaction: no block fields, no prevouts). With a block the pool is skipped and the miss is
/// the block's; a txid in neither keeps Core's miss message. The mempool-before-txindex order is
/// `dashd_indexes::get_transaction`'s, which its own tests cover.
// Core: v24.0.0 src/node/transaction.cpp:128-131; src/rpc/rawtransaction.cpp:446-475
#[test]
fn getrawtransaction_reads_the_mempool_without_a_block() {
    let keys = [
        "txid",
        "version",
        "type",
        "size",
        "locktime",
        "vin",
        "vout",
        "hex",
        "instantlock",
        "instantlock_internal",
        "chainlock",
    ];
    for (argv, miss) in [
        ("-txindex=1", "No such mempool or blockchain transaction"),
        (
            "-txindex=0",
            "No such mempool transaction. Use -txindex or provide a block hash to enable blockchain transaction queries",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut node = started(dir.path(), &[argv]);
        let tip = generate(&node, dir.path(), 1).remove(0);
        if argv == "-txindex=1" {
            wait_synced(&node, 1);
        }
        let table = command_table(&node, dir.path());
        let rpc = |method: &str, params: &str| crate::chain::tests::rpc(&table, method, params);
        let (txid, hex) = pooled(&node);
        let found = rpc("getrawtransaction", &format!(r#"["{txid}"]"#)).expect("in the pool");
        assert_eq!(found, format!(r#""{hex}""#), "{argv}");
        for v in [1, 2] {
            let reply = rpc("getrawtransaction", &format!(r#"["{txid}", {v}]"#)).unwrap();
            let dashd_rpc::UniValue::Obj(fields) =
                dashd_rpc::UniValue::read(reply.as_bytes()).unwrap()
            else {
                panic!("{reply}");
            };
            let got: Vec<_> = fields.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(got, keys, "{argv} verbosity {v}: {reply}");
            let json: serde_json::Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(json["txid"].as_str(), Some(&*txid.to_string()));
            assert_eq!(json["hex"].as_str(), Some(&*hex));
            assert_eq!(json["instantlock"], false);
        }
        let in_block = format!(r#"["{txid}", 1, "{tip}"]"#);
        let skipped = "No such transaction found in the provided block. Use gettransaction for wallet transactions.";
        assert_eq!(rpc("getrawtransaction", &in_block).unwrap_err(), reply(-5, skipped), "{argv}");
        let absent = format!(r#"["{}"]"#, "05".repeat(32));
        let miss = format!("{miss}. Use gettransaction for wallet transactions.");
        assert_eq!(rpc("getrawtransaction", &absent).unwrap_err(), reply(-5, &miss), "{argv}");
        drop(table);
        node.shutdown();
    }
}

fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}
