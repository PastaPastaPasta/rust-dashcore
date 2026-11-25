//! The index host while a `-reindex-chainstate` import is held, and a kill during that import.
//! Core citations are v24.0.2.

#[cfg(debug_assertions)]
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::atomic::Ordering;

use dash_primitives::BlockHash;
#[cfg(debug_assertions)]
use dashd_indexes::IndexStatus as _;
#[cfg(debug_assertions)]
use dashd_rpc::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
};

#[cfg(debug_assertions)]
use super::txindex::{indexed_at, started};
use super::*;

#[cfg(debug_assertions)]
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

#[cfg(debug_assertions)]
fn rpc_err(table: &dashd_rpc::server::RpcServer, method: &str, params: &str) -> (i32, String) {
    crate::chain::tests::rpc(table, method, params).expect_err(method)
}

#[cfg(debug_assertions)]
fn reply(code: i32, message: &str) -> (i32, String) {
    (code, format!(r#"{{"code":{code},"message":"{message}"}}"#))
}

#[cfg(debug_assertions)]
fn wait_all(node: &Node, height: u32) {
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let (tx, filters, coinstats) = (
            node.txindex().expect("txindex"),
            node.filter_index().expect("filters"),
            node.coinstats_index().expect("coinstats"),
        );
        if indexed_at(tx, height) && indexed_at(filters, height) && indexed_at(coinstats, height) {
            return;
        }
        let all = [tx.summary(), filters.summary(), coinstats.summary()];
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
type NamedFile = (String, Vec<u8>);

/// Each index's stored locator, and the block and undo files' bytes.
#[cfg(debug_assertions)]
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

/// Each index's stored locator, as a block hash: txindex, the filter index, coinstats.
fn stored_locators(dir: &Path) -> Vec<Option<String>> {
    let decode = |l: &Option<Vec<u8>>| {
        l.as_deref().and_then(dashd_chainstate::subscriber::decode_locator).map(|h| h.to_string())
    };
    on_disk(dir).0.iter().map(decode).collect()
}

/// IX1-ft-d: indexes latched on an empty datadir and held behind V when shutdown interrupts them
/// drain to the first shutdown flush's tip before their final commits, so a clean stop leaves
/// every locator at `d`. A second run on it connects three blocks and aborts (DEC-995), and a
/// third start finds every locator, none past the last durable flush.
// Core: v24.0.2 src/index/base.cpp:123-128, :435-472; src/init.cpp:379-391, :438-441
#[test]
fn held_fresh_indexes_drain_to_the_shutdown_flush_before_their_final_commits() {
    let dir = tempfile::tempdir().unwrap();
    let gate = Gate::default();
    let mut node = started_held(dir.path(), INDEXES, &gate);
    let d = generate(&node, dir.path(), 2).pop().unwrap();
    node.interrupt();
    open_gate(&gate);
    node.shutdown();
    assert_eq!(node.exit_status(), 0);
    drop(node);
    assert_eq!(stored_locators(dir.path()), vec![Some(d.clone()); 3], "every index at d");

    let (mut node, ..) = settled_with(dir.path(), INDEXES);
    assert_eq!(node.tip().map(|h| h.to_string()), Some(d.clone()), "durable at d");
    let mined = generate(&node, dir.path(), 3);
    node.steps.chainstate().abort_path().abort_node(CORRUPT_BLOCK, None);
    node.shutdown();
    assert_eq!(node.abort.reports(), 1);
    drop(node);
    let stored = stored_locators(dir.path());

    let (mut node, ..) = settled_with(dir.path(), INDEXES);
    let durable = node.tip().expect("a tip").to_string();
    node.shutdown();
    assert_ne!(durable, mined[2], "blocks connected after the last durable flush");
    let mut chain = std::iter::once(&d).chain(&mined);
    assert!(chain.any(|h| *h == durable), "the last durable flush is d or a block on it");
    let past: Vec<&String> = chain.collect();
    for l in &stored {
        let l = l.as_ref().expect("every index keeps a locator");
        assert!(!past.contains(&l), "{stored:?} past {durable}");
    }
}

/// Row e case iii: a kill during a `-reindex-chainstate` import, before V's chain has a tip,
/// leaves a coins view with no best block. A flagless restart skips `LoadChainTip`, so each
/// index's `Init` finds its locator off the empty chain and init refuses with Core's
/// `InitError`, changing no locator and no block file. A `-reindex-chainstate` restart then
/// opens the indexes after its import, and they end at the tip with the answers from before.
// Core: v24.0.2 src/index/base.cpp:125-162; src/node/chainstate.cpp:214-216, :272-277;
// src/init.cpp:2278-2279; src/node/blockstorage.cpp:945-965
#[cfg(debug_assertions)]
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
    let tip: BlockHash = mined[2].parse().unwrap();
    assert_eq!(
        kept.0
            .iter()
            .map(|l| l.as_deref().and_then(dashd_chainstate::subscriber::decode_locator))
            .collect::<Vec<_>>(),
        [Some(tip), Some(tip), Some(tip)],
        "all three index locators are present at the tip after the kill"
    );

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

#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
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

/// The indexes' block reads, held while the gate is shut, so the indexes fall behind V as when
/// a peer feeds blocks faster than they index.
struct HeldReads {
    files: Arc<dyn dashd_chainstate::chain::BlockFiles + Sync>,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

impl dashd_chainstate::chain::BlockFiles for HeldReads {
    fn save_block(
        &mut self,
        _: &Arc<dash_consensus::Block>,
        _: u32,
        _: Option<dash_chain::FlatFilePos>,
    ) -> Result<Option<dash_chain::FlatFilePos>, String> {
        unreachable!("the indexes never write a block")
    }

    fn read_block(
        &self,
        pos: dash_chain::FlatFilePos,
        hash: &BlockHash,
    ) -> Result<Arc<dash_consensus::Block>, String> {
        let (open, opened) = &*self.gate;
        drop(opened.wait_while(open.lock().unwrap(), |open| !*open).unwrap());
        self.files.read_block(pos, hash)
    }

    fn open_block_file(&self, file: i32) -> bool {
        self.files.open_block_file(file)
    }

    fn current_usage(&self) -> u64 {
        self.files.current_usage()
    }

    fn scan_already_pruned_files(&mut self, _: bool) -> Result<(), String> {
        unreachable!("the indexes never prune")
    }

    fn read_undo(
        &self,
        pos: dash_chain::FlatFilePos,
        parent: &BlockHash,
    ) -> Result<dash_consensus::coins::BlockUndo, String> {
        self.files.read_undo(pos, parent)
    }
}

type Gate = Arc<(Mutex<bool>, std::sync::Condvar)>;

fn open_gate(gate: &Gate) {
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
}

/// A node on `dir` through init with the indexes' block reads held behind `gate`.
fn started_held(dir: &Path, argv: &[&str], gate: &Gate) -> Node {
    let (mut node, _, stderr) = start(dir, argv);
    let gate = Arc::clone(gate);
    node.steps.wrap_index_files = Some(Box::new(move |files| {
        Arc::new(HeldReads {
            files,
            gate: Arc::clone(&gate),
        })
    }));
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    assert!(node.steps.join_import(), "loadblk ends");
    node
}

/// One RPC on its own thread, as a client waits on it.
fn call(
    table: &Arc<dashd_rpc::server::RpcServer>,
    method: &'static str,
    params: String,
) -> std::thread::JoinHandle<Result<String, (i32, String)>> {
    let table = Arc::clone(table);
    std::thread::spawn(move || crate::chain::tests::rpc(&table, method, &params))
}

/// Core's `Init` latch: on an empty datadir, and after `-reindex`'s wipe, each index is synced
/// before genesis and gets every block after it. With the indexes held behind V, `getindexinfo`
/// says synced, and `gettxoutsetinfo` (index path), `getblockfilter` and `getrawtransaction`
/// wait for the indexes to reach the tip, then answer instead of "still syncing"; a txid in no
/// block gets the answer of a synced index.
// Core: v24.0.2 src/index/base.cpp:123-128, :474-496; src/rpc/blockchain.cpp:1264-1274,
// :3055-3076; src/rpc/rawtransaction.cpp:441-463; src/rpc/node.cpp:1165-1228
#[test]
fn a_fresh_index_held_behind_the_tip_is_synced_and_its_rpcs_wait() {
    let dir = tempfile::tempdir().unwrap();
    for (argv, start) in
        [(INDEXES.to_vec(), "empty datadir"), ([INDEXES, &["-reindex"]].concat(), "-reindex")]
    {
        let gate = Gate::default();
        let mut node = started_held(dir.path(), &argv, &gate);
        let table = command_table(&node, dir.path());
        let latched = |when: &str| {
            let info = rpc_value(&table, "getindexinfo", "[]");
            for name in ["txindex", "coinstatsindex", "basic block filter index"] {
                assert_eq!(info[name]["synced"], true, "{start}, {when}: {name}: {info}");
                assert_eq!(info[name]["best_block_height"], 0, "{start}, {when}: {name}");
            }
        };
        if let Err(e) =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| latched("at start")))
        {
            open_gate(&gate);
            std::panic::resume_unwind(e);
        }
        let mined = if start == "-reindex" {
            (1..=3)
                .map(|h| rpc_value(&table, "getblockhash", &format!("[{h}]")))
                .map(|h| h.as_str().unwrap().to_owned())
                .collect()
        } else {
            generate(&node, dir.path(), 3)
        };
        let tip = &mined[2];
        let tx = rpc_value(&table, "getblock", &format!(r#"["{tip}", 1]"#))["tx"][0]
            .as_str()
            .unwrap()
            .to_owned();
        let waits = [
            call(&table, "gettxoutsetinfo", r#"["muhash"]"#.to_owned()),
            call(&table, "getblockfilter", format!(r#"["{tip}"]"#)),
            call(&table, "getrawtransaction", format!(r#"["{tx}"]"#)),
            call(&table, "getrawtransaction", format!(r#"["{}"]"#, "07".repeat(32))),
        ];
        std::thread::sleep(Duration::from_millis(200));
        let answered: Vec<_> = waits.iter().map(std::thread::JoinHandle::is_finished).collect();
        let info = rpc_value(&table, "getindexinfo", "[]");
        open_gate(&gate);
        assert_eq!(
            answered, [false; 4],
            "{start}: an RPC answered before its index reached the tip"
        );
        for name in ["txindex", "coinstatsindex", "basic block filter index"] {
            assert_eq!(info[name]["synced"], true, "{start}, held: {name}: {info}");
            assert_eq!(info[name]["best_block_height"], 0, "{start}, held: {name}");
        }
        let [stats, filter, found, missing] = waits.map(|w| w.join().unwrap());
        let stats: serde_json::Value =
            serde_json::from_str(&stats.expect("gettxoutsetinfo")).unwrap();
        assert_eq!(
            (&stats["height"], stats["bestblock"].as_str()),
            (&serde_json::json!(3), Some(tip.as_str())),
            "{start}"
        );
        let scan = command_table(&node, dir.path());
        let scanned = rpc_value(&scan, "gettxoutsetinfo", r#"["muhash", null, false]"#);
        drop(scan);
        assert_eq!(stats["muhash"], scanned["muhash"], "{start}");
        let filter: serde_json::Value =
            serde_json::from_str(&filter.expect("getblockfilter")).unwrap();
        assert!(filter["filter"].as_str().is_some(), "{start}: {filter}");
        let found: serde_json::Value =
            serde_json::from_str(&found.expect("getrawtransaction")).unwrap();
        assert!(found.as_str().is_some(), "{start}: {found}");
        assert_eq!(
            missing.expect_err("no such transaction"),
            reply(
                RPC_INVALID_ADDRESS_OR_KEY,
                "No such mempool or blockchain transaction. Use gettransaction for wallet transactions."
            ),
            "{start}"
        );
        wait_all(&node, 3);
        drop(table);
        node.shutdown();
        drop(node);
    }
}
