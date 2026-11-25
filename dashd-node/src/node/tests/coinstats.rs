//! IX1 d3: the coin statistics index an in-process regtest node starts at Step 8 with
//! `-coinstatsindex`, across restarts, `-reindex` and `-reindex-chainstate`. Core citations are
//! v24.0.2.

use dash_chain::IndexAccess as _;
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_primitives::BlockHash;
use dashd_chainstate::subscriber::decode_locator;
use dashd_indexes::coinstats::data::{ARCHIVE, BEST_BLOCK, COINSTATS_DB, META};
use dashd_indexes::{BlockRef, CoinStatsEntry, CoinStatsReader as _, IndexStatus as _};
use dashd_rpc::indexes::TxLookup as _;
use dashd_store::{Commit, KvBatch as _, KvEngine as _, KvRead as _, RocksConfig, RocksEngine};

use super::txindex::started;
use super::*;
use crate::indexes::coinstats_dir;

const COINSTATS: &str = "-coinstatsindex";

fn rpc(node: &Node, dir: &Path, method: &str, params: &str) -> serde_json::Value {
    let table = command_table(node, dir);
    let out = crate::chain::tests::rpc(&table, method, params)
        .unwrap_or_else(|e| panic!("{method} {params}: {e:?}"));
    serde_json::from_str(&out).unwrap()
}

fn wait_indexed(node: &Node, height: u32) {
    let index = node.coinstats_index().expect("the coinstats index runs");
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let s = index.summary();
        if s.synced && s.best_block_height == height {
            return;
        }
        assert!(Instant::now() < end, "{s:?}, not synced at {height}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The index's entry for the active block `hash`.
fn entry_at(node: &Node, hash: &BlockHash) -> CoinStatsEntry {
    let index = node.steps.chain().unwrap().handle().reader();
    let id = index.lookup(hash).expect("a known block");
    let block = BlockRef {
        id,
        hash: *hash,
        height: index.height(id).unwrap(),
    };
    let stats = node.coinstats_index().expect("the coinstats index runs");
    stats.look_up_stats(&block).unwrap().expect("indexed")
}

/// Waits for the index at the tip `height`, then checks its tip entry against the chainstate's
/// own MuHash scan of the coins, `gettxoutsetinfo muhash`'s path without an index, and its
/// `GetBlockSubsidy` sum against the mined coinbases.
// Core: src/index/coinstatsindex.cpp:117-234; src/rpc/blockchain.cpp:1175-1326
fn matches_the_coins(node: &Node, height: u32) {
    wait_indexed(node, height);
    let chain = node.steps.chain().unwrap().handle();
    let job = chain.coins_stats(CoinStatsHashType::MuHash, Arc::new(|| false), None);
    let coins = job.recv().unwrap().expect("a scan").wait().unwrap();
    let id = chain.reader().lookup(&coins.best_block).unwrap();
    assert_eq!(chain.reader().height(id), Some(height), "the coins at the tip");
    let entry = entry_at(node, &coins.best_block);
    let stats = coins.stats;
    assert_eq!(stats.hash, Some(entry.muhash));
    assert_eq!((stats.txouts, stats.bogosize), (entry.txouts, entry.bogo_size));
    assert_eq!(stats.total_amount, Some(entry.total_amount));
    assert_eq!(entry.unspendables_unclaimed_rewards, 0, "each coinbase claims its subsidy");
    assert_eq!(entry.total_subsidy, entry.total_amount + entry.total_unspendable);
}

fn db_path(dir: &Path) -> PathBuf {
    coinstats_dir(&dir.join("regtest")).join("db")
}

/// The stopped index's durable locator.
pub(super) fn stored_locator(dir: &Path) -> Option<String> {
    let db = RocksEngine::open_read_only(&db_path(dir), &COINSTATS_DB).unwrap();
    let locator = db.expect("the coinstats DB").get(META, BEST_BLOCK).unwrap();
    locator.as_deref().and_then(decode_locator).map(|h| h.to_string())
}

/// `-coinstatsindex` indexes from genesis to the tip, where its entry is the coins' MuHash and
/// totals; the stop's final commit is durable at the tip. A restart with the index behind the
/// chain resumes from its locator and catches up.
// Core: src/init.cpp:2315-2318; src/index/base.cpp:94-166, :189-268
#[test]
fn the_coinstats_index_matches_the_coins_and_resumes_from_its_locator() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    matches_the_coins(&node, 0);
    let mut mined = generate(&node, dir.path(), 3);
    matches_the_coins(&node, 3);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()), "the final commit at the tip");

    let mut node = started(dir.path(), &["-coinstatsindex=0"]);
    mined.extend(generate(&node, dir.path(), 2));
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()), "behind the chain");

    let mut node = started(dir.path(), &[COINSTATS]);
    matches_the_coins(&node, 5);
    mined.extend(generate(&node, dir.path(), 1));
    matches_the_coins(&node, 6);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[5].clone()));
}

/// `-reindex` wipes the coinstats DB before the index opens: a row the index never wrote is
/// gone, and the rebuilt index matches the coins again.
// Core: src/init.cpp:2316 (f_wipe = fReindex); src/index/coinstatsindex.cpp:111-114
#[test]
fn reindex_wipes_the_coinstats_db_and_rebuilds_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    let mined = generate(&node, dir.path(), 3);
    matches_the_coins(&node, 3);
    node.shutdown();
    drop(node);
    let (db, _) =
        RocksEngine::open(&db_path(dir.path()), &COINSTATS_DB, &RocksConfig::default()).unwrap();
    let mut batch = db.batch();
    batch.put(ARCHIVE, &[7; 32], b"stray");
    db.write(batch, Commit::Durable).unwrap();
    drop(db);

    let mut node = started(dir.path(), &[COINSTATS, "-reindex"]);
    matches_the_coins(&node, 3);
    node.shutdown();
    drop(node);
    let db = RocksEngine::open_read_only(&db_path(dir.path()), &COINSTATS_DB).unwrap().unwrap();
    assert_eq!(db.get(ARCHIVE, &[7; 32]).unwrap(), None, "the DB is wiped");
    drop(db);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()));
}

/// `-reindex-chainstate` keeps the index and opens it only once the import ends: as step 11
/// starts, `getindexinfo` serves the pre-open summary from the stored locator, and after the
/// import the index catches up and matches the rebuilt coins.
// Core: src/init.cpp:2278-2279, :2315-2318; src/node/blockstorage.cpp:958-965;
// src/index/base.cpp:189-200
#[test]
fn reindex_chainstate_opens_the_coinstats_index_after_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    let mined = generate(&node, dir.path(), 2);
    matches_the_coins(&node, 2);
    node.shutdown();
    drop(node);

    let (mut node, _, stderr) = start(dir.path(), &[COINSTATS, "-reindex-chainstate"]);
    let slot = Arc::clone(&node.rpc_chain);
    let seen = Arc::new(Mutex::new(None));
    let record = Arc::clone(&seen);
    node.steps.before_import = Some(Box::new(move |_, _, _| {
        let summaries = slot.summaries();
        *record.lock().unwrap() = Some(
            summaries.iter().map(|s| (s.name, s.synced, s.best_block_height)).collect::<Vec<_>>(),
        );
    }));
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    assert!(node.steps.join_import(), "loadblk ends");
    let pre_open = seen.lock().unwrap().take().expect("step 11 ran");
    assert!(pre_open.contains(&("coinstatsindex", false, 2)), "{pre_open:?}");
    matches_the_coins(&node, 2);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[1].clone()));
}

/// `-coinstatsindex` on a chain that synced without it catches up and matches the coins; the
/// stop's final commit is durable at the tip.
// Core: src/init.cpp:2315-2318; src/index/base.cpp:94-166, :189-268
#[test]
fn a_late_enable_catches_up_and_serves_coinstats() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-coinstatsindex=0"]);
    let mined = generate(&node, dir.path(), 3);
    assert!(node.coinstats_index().is_none());
    node.shutdown();
    drop(node);
    assert!(!coinstats_dir(&dir.path().join("regtest")).exists());

    let mut node = started(dir.path(), &[COINSTATS]);
    matches_the_coins(&node, 3);
    let indexed = txout(&node, dir.path(), r#"["muhash"]"#);
    let scanned = txout(&node, dir.path(), r#"["muhash", null, false]"#);
    same_coins(&indexed, &scanned);
    assert_eq!(indexed["bestblock"], mined[2]);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()), "the final commit at the tip");
}

/// A Step 8 open failure, a locator the block index does not know, is init's one `InitError`.
/// Under `-reindex-chainstate` the open waits for the import, so the same failure is the node's
/// one `AbortNode` there (divergence: Core still runs `Init`'s check at Step 8 even with
/// `-reindex-chainstate`).
// Core: v24.0.2 src/init.cpp:2278-2320; src/index/base.cpp:101-108, :194, :511;
// src/node/blockstorage.cpp:965
#[test]
fn reindex_chainstate_defers_an_unknown_coinstats_locator_until_after_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    generate(&node, dir.path(), 2);
    matches_the_coins(&node, 2);
    node.shutdown();
    drop(node);

    let (db, _) =
        RocksEngine::open(&db_path(dir.path()), &COINSTATS_DB, &RocksConfig::default()).unwrap();
    let mut batch = db.batch();
    let mut locator = vec![0, 0, 0, 0, 1];
    locator.extend_from_slice(&[7; 32]);
    batch.put(META, BEST_BLOCK, &locator);
    db.write(batch, Commit::Durable).unwrap();
    drop(db);

    let refusal = "coinstatsindex: best block of the index not found. Please rebuild the index.";
    let (mut node, _, stderr) = start(dir.path(), &[COINSTATS]);
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")]);
    drop(node);

    let (mut node, log, stderr) = start(dir.path(), &[COINSTATS, "-reindex-chainstate"]);
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    assert!(node.steps.join_import());
    assert_eq!(stderr.lock().unwrap().len(), 1, "{:?}", stderr.lock().unwrap());
    assert_eq!(log.lock().unwrap().iter().filter(|l| l.contains(refusal)).count(), 1);
    node.shutdown();
}

/// The index is off by default and with `-coinstatsindex=0`: no handle, no DB and no
/// `getindexinfo` entry.
// Core: src/init.cpp:2315; src/index/coinstatsindex.h DEFAULT_COINSTATSINDEX
#[test]
fn the_coinstats_index_is_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    for argv in [&[][..], &["-coinstatsindex=0"]] {
        let mut node = started(dir.path(), argv);
        generate(&node, dir.path(), 1);
        assert!(node.coinstats_index().is_none(), "{argv:?}");
        let info = rpc(&node, dir.path(), "getindexinfo", "[]");
        assert!(info.get("coinstatsindex").is_none(), "{argv:?}: {info}");
        node.shutdown();
        drop(node);
        assert!(!coinstats_dir(&dir.path().join("regtest")).exists(), "{argv:?}");
    }
}

/// With the three indexes on, `getindexinfo` lists them in Core's order: the txindex, coinstats,
/// then the basic filter index, each synced at the tip.
// Core: src/rpc/node.cpp:1205-1227
#[test]
fn getindexinfo_lists_the_three_indexes_in_cores_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &["-txindex=1", "-blockfilterindex=basic", COINSTATS]);
    generate(&node, dir.path(), 2);
    matches_the_coins(&node, 2);
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let all = [node.txindex().unwrap().summary(), node.filter_index().unwrap().summary()];
        if all.iter().all(|s| s.synced && s.best_block_height == 2) {
            break;
        }
        assert!(Instant::now() < end, "{all:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
    let table = command_table(&node, dir.path());
    let info = crate::chain::tests::rpc(&table, "getindexinfo", "[]").unwrap();
    let synced = r#"{"synced":true,"best_block_height":2}"#;
    let want = format!(
        r#"{{"txindex":{synced},"coinstatsindex":{synced},"basic block filter index":{synced}}}"#
    );
    assert_eq!(info, want);
    let one = crate::chain::tests::rpc(&table, "getindexinfo", r#"["coinstatsindex"]"#).unwrap();
    assert_eq!(one, format!(r#"{{"coinstatsindex":{synced}}}"#));
    drop(table);
    node.shutdown();
}

fn txout(node: &Node, dir: &Path, params: &str) -> serde_json::Value {
    rpc(node, dir, "gettxoutsetinfo", params)
}

fn dash_amount(duffs: i64) -> serde_json::Value {
    let text = format!("{}.{:08}", duffs / 100_000_000, duffs.unsigned_abs() % 100_000_000);
    serde_json::from_str(&text).unwrap()
}

fn same_coins(indexed: &serde_json::Value, scanned: &serde_json::Value) {
    for key in ["muhash", "txouts", "bogosize", "total_amount"] {
        assert_eq!(indexed[key], scanned[key], "{key}");
    }
    assert!(indexed.get("transactions").is_none() && indexed.get("disk_size").is_none());
    assert!(scanned.get("transactions").is_some() && scanned.get("disk_size").is_some());
}

/// `gettxoutsetinfo muhash` at the tip matches the coins scan (`use_index` false). An earlier
/// block answers by height and by hash. dashd v24's recorded genesis answer matches exactly.
/// A block removed with `invalidateblock` still answers by hash.
// Core: v24.0.2 src/rpc/blockchain.cpp:1175-1326
#[test]
fn gettxoutsetinfo_serves_muhash_for_the_tip_an_earlier_block_and_a_reorg() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    wait_indexed(&node, 0);
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dashd-indexes/tests/fixtures/dashd-v24/blocks.json"
    );
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let genesis = txout(&node, dir.path(), r#"["muhash", 0]"#);
    assert_eq!(genesis, rows[0]["gettxoutsetinfo"], "dashd v24's genesis gettxoutsetinfo");

    let mined = generate(&node, dir.path(), 3);
    wait_indexed(&node, 3);
    let tip = txout(&node, dir.path(), r#"["muhash"]"#);
    let scanned = txout(&node, dir.path(), r#"["muhash", null, false]"#);
    same_coins(&tip, &scanned);
    assert_eq!(tip["height"], 3);
    assert_eq!(tip["bestblock"], mined[2]);

    let earlier = txout(&node, dir.path(), r#"["muhash", 1]"#);
    let hash = rpc(&node, dir.path(), "getblockhash", "[1]");
    let by_hash = txout(&node, dir.path(), &format!(r#"["muhash", "{}"]"#, hash.as_str().unwrap()));
    assert_eq!(earlier, by_hash);
    assert_eq!(earlier["height"], 1);
    assert_eq!(earlier["bestblock"], hash);
    assert_ne!(earlier["muhash"], tip["muhash"]);

    rpc(&node, dir.path(), "invalidateblock", &format!(r#"["{}"]"#, mined[2]));
    wait_indexed(&node, 2);
    let stale = txout(&node, dir.path(), &format!(r#"["muhash", "{}"]"#, mined[2]));
    assert_eq!(stale, tip, "the reorged block still answers by hash");
    let now = txout(&node, dir.path(), r#"["muhash"]"#);
    assert_eq!(now["height"], 2);
    assert_eq!(now["bestblock"], mined[1]);
    node.shutdown();
}

/// `gettxoutsetinfo muhash` flushes the chainstate at the tip it returns, on the index path.
/// The flush is V's `ChainStateFlushed` best block, not a killed-process restart: that restart
/// is `test-support`, and a kill during `-reindex-chainstate` does not resume here.
// Core: v24.0.2 src/rpc/blockchain.cpp:1238 ForceFlushStateToDisk
#[test]
fn gettxoutsetinfo_flushes_the_chainstate_at_the_answered_tip() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[COINSTATS]);
    let mined = generate(&node, dir.path(), 2);
    wait_indexed(&node, 2);
    let flushed = Arc::new(Mutex::new(Vec::<String>::new()));
    struct Hear(Arc<Mutex<Vec<String>>>);
    impl dashd_chainstate::chain::ValidationInterface for Hear {
        fn chain_state_flushed(&self, locator: &[dash_primitives::BlockHash]) {
            if let Some(tip) = locator.first() {
                self.0.lock().unwrap().push(tip.to_string());
            }
        }
    }
    node.steps.chain().unwrap().signals().register(Arc::new(Hear(Arc::clone(&flushed))));
    let info = txout(&node, dir.path(), r#"["muhash"]"#);
    let table = command_table(&node, dir.path());
    crate::chain::tests::rpc(&table, "syncwithvalidationinterfacequeue", "[]").unwrap();
    drop(table);
    assert_eq!(info["bestblock"], mined[1]);
    assert_eq!(info["height"], 2);
    let seen = flushed.lock().unwrap().clone();
    assert_eq!(seen, [mined[1].clone()], "flushed once at the answered tip");
    node.shutdown();
}

/// Crossing V20, every block's `unclaimed_rewards` is 0, each later block's `coinbase` is that
/// block's subsidy, and V20 is active at the test height. The tip matches the coins scan.
// Core: v24.0.2 src/index/coinstatsindex.cpp:204-218; src/rpc/blockchain.cpp:1308-1314
#[test]
fn gettxoutsetinfo_unclaimed_rewards_are_zero_across_v20() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(
        dir.path(),
        &[COINSTATS, "-dip3params=2:2", "-testactivationheight=v20@4", "-deprecatedrpc=softforks"],
    );
    generate(&node, dir.path(), 6);
    matches_the_coins(&node, 6);
    let scheduler = node.scheduler.as_ref().unwrap().clone();
    let info_table = crate::rpc::command_table(
        &node.params.chain,
        node.ui.logger(),
        &node.shutdown,
        &node.clock,
        (true, &scheduler),
        &node.rpc_chain,
        &node.rpc_net,
        &node.rpc_sync,
        &node.warnings,
        crate::rpc::DeprecatedRpc {
            softforks: true,
            ..Default::default()
        },
        dir.path().join("regtest"),
    );
    info_table.start();
    info_table.set_warmup_finished();
    let chain = {
        let out = crate::chain::tests::rpc(&info_table, "getblockchaininfo", "[]").unwrap();
        serde_json::from_str::<serde_json::Value>(&out).unwrap()
    };
    drop(info_table);
    assert_eq!(chain["softforks"]["v20"]["active"], true, "{chain}");
    assert_eq!(chain["softforks"]["v20"]["height"], 4, "{chain}");
    for height in 0..=6 {
        let info = txout(&node, dir.path(), &format!(r#"["muhash", {height}]"#));
        let rewards = &info["block_info"]["unspendables"]["unclaimed_rewards"];
        assert_eq!(rewards, &serde_json::json!(0.0), "height {height}: {info}");
        let hash: dash_primitives::BlockHash = info["bestblock"].as_str().unwrap().parse().unwrap();
        let subsidy = node.rpc_chain.chainman().unwrap().block_subsidy(&hash).unwrap();
        assert_ne!(subsidy, 0, "height {height}");
        let coinbase = &info["block_info"]["coinbase"];
        if height == 0 {
            assert_eq!(coinbase, &serde_json::json!(0.0), "{info}");
            assert_eq!(
                info["block_info"]["unspendables"]["genesis_block"],
                dash_amount(subsidy),
                "{info}"
            );
        } else {
            assert_eq!(coinbase, &dash_amount(subsidy), "height {height}: {info}");
        }
    }
    let indexed = txout(&node, dir.path(), r#"["muhash"]"#);
    let scanned = txout(&node, dir.path(), r#"["muhash", null, false]"#);
    same_coins(&indexed, &scanned);
    node.shutdown();
}
