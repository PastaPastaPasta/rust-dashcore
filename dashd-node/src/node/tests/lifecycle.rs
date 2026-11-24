//! The index host while a `-reindex-chainstate` import is held, and a kill during that import.
//! Core citations are v24.0.2.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use dash_primitives::BlockHash;
use dashd_indexes::IndexStatus as _;
use dashd_rpc::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
};

use super::txindex::started;
use super::*;

const INDEXES: &[&str] = &["-txindex=1", "-blockfilterindex=basic", "-coinstatsindex"];

fn rpc_value(
    table: &dashd_rpc::server::RpcServer,
    method: &str,
    params: &str,
) -> serde_json::Value {
    let out = crate::chain::tests::rpc(table, method, params)
        .unwrap_or_else(|e| panic!("{method} {params}: {e:?}"));
    serde_json::from_str(&out).unwrap()
}

fn rpc_err(table: &dashd_rpc::server::RpcServer, method: &str, params: &str) -> (i32, String) {
    crate::chain::tests::rpc(table, method, params).expect_err(method)
}

fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

fn wait_all(node: &Node, height: u32) {
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let all = [
            node.txindex().expect("txindex").summary(),
            node.filter_index().expect("filters").summary(),
            node.coinstats_index().expect("coinstats").summary(),
        ];
        if all.iter().all(|s| s.synced && s.best_block_height == height) {
            return;
        }
        assert!(Instant::now() < end, "{all:?}, not synced at {height}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A header on genesis that loses to the active chain: stored, never connected.
pub(super) fn lesser_sibling(node: &Node, dir: &Path) -> String {
    let table = command_table(node, dir);
    let genesis = rpc_value(&table, "getblockhash", "[0]");
    let header =
        rpc_value(&table, "getblockheader", &format!(r#"["{}"]"#, genesis.as_str().unwrap()));
    let prev: BlockHash = genesis.as_str().unwrap().parse().unwrap();
    let time = u32::try_from(header["time"].as_u64().unwrap()).unwrap().saturating_add(1);
    let block = mine_on(prev.as_bytes(), time, 1, b"side");
    let header = &block.hex[..160];
    let submitted = rpc_value(&table, "submitheader", &format!(r#"["{header}"]"#));
    assert_eq!(submitted, serde_json::Value::Null, "a lesser header is stored");
    let hash = BlockHash::from(block.hash).to_string();
    assert_ne!(rpc_value(&table, "getbestblockhash", "[]").as_str().unwrap(), hash);
    drop(table);
    hash
}

struct MinedBlock {
    hex: String,
    hash: [u8; 32],
}

/// A coinbase-only regtest block on `prev` at `height`, distinct by `tag`.
fn mine_on(prev: &[u8; 32], time: u32, height: u16, tag: &[u8]) -> MinedBlock {
    use dash_consensus::primitives::{decode_header, decode_tx};
    let tx = coinbase(height, tag);
    let txid = *decode_tx(&mut tx.as_slice()).unwrap().into_value().txid().as_bytes();
    for nonce in 0u32.. {
        let mut header = 0x2000_0000i32.to_le_bytes().to_vec();
        header.extend(prev);
        header.extend(txid);
        header.extend(time.to_le_bytes());
        header.extend(0x207f_ffffu32.to_le_bytes());
        header.extend(nonce.to_le_bytes());
        let hash = *decode_header(&mut header.as_slice()).unwrap().into_value().hash().as_bytes();
        if hash[31] >= 0x7f {
            continue;
        }
        let mut block = header;
        block.push(1);
        block.extend(tx);
        return MinedBlock {
            hex: block.iter().map(|b| format!("{b:02x}")).collect(),
            hash,
        };
    }
    unreachable!("a nonce meets the regtest target")
}

fn coinbase(height: u16, tag: &[u8]) -> Vec<u8> {
    let mut script = if (1..=16).contains(&height) {
        vec![0x50 + height as u8]
    } else {
        vec![1, height as u8]
    };
    script.push(u8::try_from(tag.len()).unwrap());
    script.extend(tag);
    let mut tx = 1i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend([0; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(u8::try_from(script.len()).unwrap());
    tx.extend(script);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(1);
    tx.extend(50_000_000_000i64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// `-reindex-chainstate` held before its replay: the indexes are not open and still serve what
/// they retained, the coinstats height bound is the rebuild's tip, and a block the indexes never
/// saw gets each RPC's syncing or miss answer.
// Core: v24.0.2 src/init.cpp:2278-2279, :2305-2318; src/node/blockstorage.cpp:945-965;
// src/rpc/blockchain.cpp:108-134, :1249-1274, :3007-3085; src/rpc/rawtransaction.cpp:441-461
#[test]
fn answers_during_a_held_reindex_chainstate_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), INDEXES);
    let mined = generate(&node, dir.path(), 3);
    wait_all(&node, 3);
    let table = command_table(&node, dir.path());
    let kept_tx = rpc_value(&table, "getblock", &format!(r#"["{}", 1]"#, mined[1]))["tx"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let kept_filter = rpc_value(&table, "getblockfilter", &format!(r#"["{}"]"#, mined[1]));
    let kept_stats =
        rpc_value(&table, "gettxoutsetinfo", &format!(r#"["muhash", "{}"]"#, mined[1]));
    let side = lesser_sibling(&node, dir.path());
    drop(table);
    node.shutdown();
    drop(node);

    let mut node = started(dir.path(), &["-txindex=0", "-blockfilterindex=0", "-coinstatsindex=0"]);
    let extra = generate(&node, dir.path(), 2);
    let extra_for_hold = extra.clone();
    let table = command_table(&node, dir.path());
    let fresh_tx = rpc_value(&table, "getblock", &format!(r#"["{}", 1]"#, extra[1]))["tx"][0]
        .as_str()
        .unwrap()
        .to_owned();
    drop(table);
    node.shutdown();
    drop(node);

    let mut argv: Vec<&str> = INDEXES.to_vec();
    argv.push("-reindex-chainstate");
    let (mut node, _, stderr) = start(dir.path(), &argv);
    let chain = Arc::clone(&node.rpc_chain);
    let forgive = Arc::clone(&node.steps.forgive_import_sends);
    let seen = Arc::new(Mutex::new(None));
    let record = Arc::clone(&seen);
    let params = node.params.chain.clone();
    let logger = Arc::clone(node.ui.logger());
    let shutdown = Arc::clone(&node.shutdown);
    let clock = Arc::clone(&node.clock);
    let net = Arc::clone(&node.rpc_net);
    let sync = Arc::clone(&node.rpc_sync);
    let warnings = Arc::clone(&node.warnings);
    let data = dir.path().join("regtest");
    node.steps.before_import = Some(Box::new(move |v, _, _| {
        let sent = v.commands_sent();
        let scheduler = crate::scheduler::Scheduler::new();
        let table = crate::rpc::command_table(
            &params,
            &logger,
            &shutdown,
            &clock,
            (true, &scheduler),
            &chain,
            &net,
            &sync,
            &warnings,
            Default::default(),
            data,
        );
        table.start();
        table.set_warmup_finished();
        *record.lock().unwrap() = Some(held_answers(
            &table,
            &Held {
                mined,
                kept_tx,
                kept_filter,
                kept_stats,
                side,
                extra: extra_for_hold,
                fresh_tx,
            },
        ));
        drop(table);
        forgive.fetch_add(v.commands_sent().saturating_sub(sent), Ordering::Relaxed);
    }));
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    assert!(node.steps.join_import());
    assert!(seen.lock().unwrap().take().unwrap(), "the import was held");
    wait_all(&node, 5);
    let table = command_table(&node, dir.path());
    let tip = rpc_value(&table, "gettxoutsetinfo", r#"["muhash"]"#);
    assert_eq!(tip["height"], 5);
    assert_eq!(tip["bestblock"], extra[1]);
    node.shutdown();
}

/// A file's name and bytes.
type NamedFile = (String, Vec<u8>);

/// Each index's stored locator, and the block and undo files' bytes.
fn on_disk(dir: &Path) -> (Vec<Option<Vec<u8>>>, Vec<NamedFile>) {
    use dashd_indexes::{blockfilter, coinstats, txindex};
    use dashd_store::{KvRead as _, RocksEngine};
    let data = dir.join("regtest");
    let locator = |path: PathBuf, desc, (cf, key)| {
        let db = RocksEngine::open_read_only(&path, desc).unwrap().expect("the index DB");
        db.get(cf, key).unwrap()
    };
    let locators = vec![
        locator(
            crate::indexes::txindex_dir(&data),
            &txindex::TXINDEX_DB,
            (txindex::META, txindex::BEST_BLOCK),
        ),
        locator(
            crate::indexes::filter_index_dir(&data).join("db"),
            &blockfilter::data::BLOCKFILTER_DB,
            (blockfilter::data::META, blockfilter::data::BEST_BLOCK),
        ),
        locator(
            crate::indexes::coinstats_dir(&data).join("db"),
            &coinstats::data::COINSTATS_DB,
            (coinstats::data::META, coinstats::data::BEST_BLOCK),
        ),
    ];
    let mut files: Vec<_> = std::fs::read_dir(data.join("blocks"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "dat"))
        .map(|p| {
            (p.file_name().unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap())
        })
        .collect();
    files.sort();
    assert!(files.iter().any(|(name, _)| name.starts_with("blk")), "{files:?}");
    (locators, files)
}

/// Row e case iii: a kill during a `-reindex-chainstate` import, before V's chain has a tip,
/// leaves a coins view with no best block. A flagless restart skips `LoadChainTip`, so each
/// index's `Init` finds its locator off the empty chain and init refuses with Core's
/// `InitError`, changing no locator and no block file. A `-reindex-chainstate` restart then
/// opens the indexes after its import, and they end at the tip with the answers from before.
// Core: v24.0.2 src/index/base.cpp:125-162; src/node/chainstate.cpp:213-215, :272-277;
// src/init.cpp:2278-2279; src/node/blockstorage.cpp:945-965
#[test]
fn a_kill_during_a_reindex_chainstate_import_refuses_a_flagless_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), INDEXES);
    let mined = generate(&node, dir.path(), 3);
    wait_all(&node, 3);
    let answers = |node: &Node| {
        let table = command_table(node, dir.path());
        let block = rpc_value(&table, "getblock", &format!(r#"["{}", 1]"#, mined[1]));
        let tx = block["tx"][0].as_str().unwrap().to_owned();
        let tip = &mined[2];
        (
            rpc_value(&table, "getrawtransaction", &format!(r#"["{tx}"]"#)),
            rpc_value(&table, "getblockfilter", &format!(r#"["{tip}"]"#)),
            rpc_value(&table, "gettxoutsetinfo", &format!(r#"["muhash", "{tip}"]"#)),
        )
    };
    let before = answers(&node);
    node.shutdown();
    drop(node);

    let mut argv: Vec<&str> = INDEXES.to_vec();
    argv.push("-reindex-chainstate");
    let (mut node, _, stderr) = start(dir.path(), &argv);
    let forgive = Arc::clone(&node.steps.forgive_import_sends);
    let shutdown = Arc::clone(&node.shutdown);
    node.steps.before_import = Some(Box::new(move |v, _, _| {
        v.shutdown();
        forgive.fetch_add(1, Ordering::Relaxed);
        shutdown.start_shutdown();
    }));
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    assert!(stderr.lock().unwrap().is_empty(), "{:?}", stderr.lock().unwrap());
    node.steps.kill();
    drop(node);
    let kept = on_disk(dir.path());

    let (mut node, _, stderr) = start(dir.path(), INDEXES);
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    let refusal = "txindex best block of the index goes beyond pruned data. Please disable the \
                   index or reindex (which will download the whole blockchain again)";
    assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")]);
    node.shutdown();
    drop(node);
    assert_eq!(on_disk(dir.path()), kept, "the refused start changed no data");

    let node = started(dir.path(), &argv);
    wait_all(&node, 3);
    assert_eq!(answers(&node), before);
}

struct Held {
    mined: Vec<String>,
    kept_tx: String,
    kept_filter: serde_json::Value,
    kept_stats: serde_json::Value,
    side: String,
    extra: Vec<String>,
    fresh_tx: String,
}

/// What the held import answers. `false` when a check fails; the asserts panic first.
fn held_answers(table: &dashd_rpc::server::RpcServer, held: &Held) -> bool {
    let mined = &held.mined;
    let extra = &held.extra;
    let info = rpc_value(table, "getindexinfo", "[]");
    for name in ["txindex", "coinstatsindex", "basic block filter index"] {
        assert_eq!(info[name]["synced"], false, "{name} is not open: {info}");
        assert_eq!(info[name]["best_block_height"], 3, "{name}");
    }
    let count = rpc_value(table, "getblockcount", "[]");
    let tip = count.as_i64().unwrap_or_else(|| panic!("getblockcount: {count}"));
    assert!(tip < 3, "the rebuild has not reached the retained blocks, tip {tip}");

    assert_eq!(
        rpc_value(table, "getblockfilter", &format!(r#"["{}"]"#, mined[1])),
        held.kept_filter,
        "a retained filter"
    );
    let found = rpc_value(table, "getrawtransaction", &format!(r#"["{}"]"#, held.kept_tx));
    assert!(found.as_str().is_some(), "a retained transaction: {found}");

    assert_eq!(
        rpc_err(table, "gettxoutsetinfo", &format!(r#"["muhash", {}]"#, tip + 1)),
        reply(
            RPC_INVALID_PARAMETER,
            &format!("Target block height {} after current tip {tip}", tip + 1),
        )
    );
    assert_eq!(
        rpc_value(table, "gettxoutsetinfo", &format!(r#"["muhash", "{}"]"#, mined[1])),
        held.kept_stats,
        "a retained block ahead of the rebuild tip"
    );
    assert_eq!(
        rpc_err(table, "gettxoutsetinfo", &format!(r#"["muhash", "{}"]"#, extra[1])),
        reply(
            RPC_INTERNAL_ERROR,
            "Unable to get data because coinstatsindex is still syncing. Current height: 3",
        )
    );
    assert_eq!(
        rpc_err(table, "gettxoutsetinfo", &format!(r#"["muhash", "{}"]"#, held.side)),
        reply(RPC_INTERNAL_ERROR, "Unable to read UTXO set")
    );

    assert_eq!(
        rpc_err(table, "getblockfilter", &format!(r#"["{}"]"#, extra[1])),
        reply(
            RPC_MISC_ERROR,
            "Filter not found. Block filters are still in the process of being indexed.",
        )
    );
    let syncing = "No such mempool transaction. Blockchain transactions are still in the process of being indexed. Use gettransaction for wallet transactions.";
    assert_eq!(
        rpc_err(table, "getrawtransaction", &format!(r#"["{}"]"#, held.fresh_tx)),
        reply(RPC_INVALID_ADDRESS_OR_KEY, syncing)
    );
    true
}
