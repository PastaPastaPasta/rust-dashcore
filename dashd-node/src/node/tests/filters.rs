//! F3: the basic block filter index an in-process regtest node starts at Step 8 with
//! `-blockfilterindex`, across restarts, `-reindex` and `-reindex-chainstate`, and the BIP157
//! answers a peer gets from it. Core citations are v24.0.0.

use std::io::{Read as _, Write as _};
use std::net::TcpStream;

use dash_chain::IndexAccess as _;
use dash_consensus::coins::{BlockUndo, TxUndo};
use dash_primitives::BlockHash;
use dashd_chainstate::subscriber::{decode_locator, height_key};
use dashd_indexes::blockfilter::build_filter;
use dashd_indexes::blockfilter::codec::{FilterPos, HeightValue};
use dashd_indexes::blockfilter::data::{BEST_BLOCK, BLOCKFILTER_DB, HEIGHT, META};
use dashd_indexes::{BlockRef, FilterHeader, FilterIndexReader as _, IndexStatus as _};
use dashd_p2p::sim::{PeerVersionMsg, v1_frame};
use dashd_store::{Commit, KvBatch as _, KvEngine as _, KvRead as _, RocksConfig, RocksEngine};

use super::txindex::started;
use super::*;
use crate::indexes::filter_index_dir;

const FILTERS: &str = "-blockfilterindex=basic";

// Core: protocol.h:320-322 (NODE_COMPACT_FILTERS)
const NODE_COMPACT_FILTERS: u64 = 1 << 6;

/// One block's filter and filter header.
#[derive(Debug, PartialEq, Eq)]
struct Filter {
    hash: BlockHash,
    encoded: Vec<u8>,
    header: FilterHeader,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// dashd v24's `getblockfilter` for the regtest genesis, from dashd-indexes' g fixture.
fn cores_genesis() -> (Vec<u8>, FilterHeader) {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dashd-indexes/tests/fixtures/dashd-v24/blocks.json"
    );
    let blocks: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let genesis = &blocks[0];
    assert_eq!(genesis["role"], "genesis");
    let filter = &genesis["getblockfilter"];
    (unhex(filter["filter"].as_str().unwrap()), filter["header"].as_str().unwrap().parse().unwrap())
}

fn rpc(table: &dashd_rpc::server::RpcServer, method: &str, params: &str) -> serde_json::Value {
    let out = crate::chain::tests::rpc(table, method, params).expect(method);
    serde_json::from_str(&out).unwrap()
}

/// Each active block's filter, built from the block and chained from Core's genesis header. The
/// mined blocks spend nothing: a coinbase, and on regtest's DKG heights null quorum commitments
/// without inputs, so their undo holds no coins.
fn expected(node: &Node, dir: &Path) -> Vec<Filter> {
    let table = command_table(node, dir);
    let tip = rpc(&table, "getblockcount", "[]").as_u64().unwrap();
    let (genesis_filter, genesis_header) = cores_genesis();
    let mut out: Vec<Filter> = Vec::new();
    for height in 0..=tip {
        let hash = rpc(&table, "getblockhash", &format!("[{height}]"));
        let hash = hash.as_str().unwrap();
        let raw = unhex(rpc(&table, "getblock", &format!(r#"["{hash}", 0]"#)).as_str().unwrap());
        let block = dash_consensus::decode_block(&mut raw.as_slice()).unwrap().into_value();
        let undo = BlockUndo {
            txs: (block.txs()[1..].iter())
                .inspect(|tx| assert!(tx.inputs().is_empty(), "a mined block spends nothing"))
                .map(|_| TxUndo::default())
                .collect(),
        };
        let built = build_filter(&block, &undo).unwrap();
        let header = match out.last() {
            None => {
                assert_eq!(built.encoded(), genesis_filter, "Core's genesis filter");
                genesis_header
            }
            Some(prev) => built.header(&prev.header),
        };
        out.push(Filter {
            hash: hash.parse().unwrap(),
            encoded: built.encoded().to_vec(),
            header,
        });
    }
    out
}

fn wait_indexed(node: &Node, height: u32) {
    let filters = node.filter_index().expect("the filter index runs");
    let end = Instant::now() + Duration::from_secs(120);
    loop {
        let s = filters.summary();
        if s.synced && s.best_block_height == height {
            return;
        }
        assert!(Instant::now() < end, "{s:?}, not synced at {height}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the node's filter index serves for `blocks`.
fn served(node: &Node, blocks: &[Filter]) -> Vec<Filter> {
    let filters = node.filter_index().expect("the filter index runs");
    let index = node.steps.chain().unwrap().handle().reader();
    blocks
        .iter()
        .map(|b| {
            let id = index.lookup(&b.hash).expect("a known block");
            let block = BlockRef {
                id,
                hash: b.hash,
                height: index.height(id).unwrap(),
            };
            Filter {
                hash: b.hash,
                encoded: filters.filter(&block).unwrap().expect("indexed").encoded().to_vec(),
                header: filters.filter_header(&block).unwrap().expect("indexed"),
            }
        })
        .collect()
}

/// Waits for the index at `height`, then checks it serves the built filters.
fn serves_built(node: &Node, dir: &Path, height: u32) -> Vec<Filter> {
    wait_indexed(node, height);
    let want = expected(node, dir);
    assert_eq!(want.len(), height as usize + 1);
    assert_eq!(served(node, &want), want);
    want
}

fn index_dir(dir: &Path) -> PathBuf {
    filter_index_dir(&dir.join("regtest"))
}

/// The stopped index's durable locator.
pub(super) fn stored_locator(dir: &Path) -> Option<String> {
    let db = RocksEngine::open_read_only(&index_dir(dir).join("db"), &BLOCKFILTER_DB).unwrap();
    let locator = db.expect("the filter index DB").get(META, BEST_BLOCK).unwrap();
    locator.as_deref().and_then(decode_locator).map(|h| h.to_string())
}

/// The stopped index's filter file positions for heights `0..=tip`.
fn positions(dir: &Path, tip: u32) -> Vec<FilterPos> {
    let db = RocksEngine::open_read_only(&index_dir(dir).join("db"), &BLOCKFILTER_DB).unwrap();
    let db = db.expect("the filter index DB");
    (0..=tip)
        .map(|h| {
            let row = db.get(HEIGHT, &height_key(h, &[])).unwrap().expect("a height row");
            HeightValue::decode(&row).unwrap().filter.pos
        })
        .collect()
}

/// `-blockfilterindex=basic` on a chain that synced without it catches up and serves Core's
/// filters; the stop's final commit is durable at the tip.
// Core: init.cpp:2305-2310; index/base.cpp:94-166, :189-268
#[test]
fn a_late_enable_catches_up_and_serves_cores_filters() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[]);
    let mined = generate(&node, dir.path(), 3);
    assert!(node.filter_index().is_none());
    node.shutdown();
    drop(node);
    assert!(!index_dir(dir.path()).exists());

    let mut node = started(dir.path(), &[FILTERS]);
    serves_built(&node, dir.path(), 3);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()), "the final commit at the tip");
}

/// A restart with the index behind the chain, as a stop mid catch-up leaves it, resumes from the
/// stored locator: the indexed blocks keep their rows and file positions, and the catch-up
/// appends the rest after them.
// Core: index/base.cpp:94-166 (Init), :189-268 (ThreadSync)
#[test]
fn a_restart_resumes_the_filter_index_from_its_locator() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[FILTERS]);
    let mut mined = generate(&node, dir.path(), 3);
    serves_built(&node, dir.path(), 3);
    node.shutdown();
    drop(node);
    let before = positions(dir.path(), 3);

    let mut node = started(dir.path(), &["-blockfilterindex=0"]);
    mined.extend(generate(&node, dir.path(), 2));
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[2].clone()), "behind the chain");

    let mut node = started(dir.path(), &[FILTERS]);
    serves_built(&node, dir.path(), 5);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[4].clone()));
    let after = positions(dir.path(), 5);
    assert_eq!(after[..4], before[..], "the indexed blocks are not rewritten");
    assert!(after[3].file == after[4].file && after[3].pos < after[4].pos, "{after:?}");
}

/// `-reindex` wipes the index's DB and its `fltr?????.dat` files before the index opens: the
/// rebuilt index writes its filters from the start of `fltr00000.dat` again, a stray filter file
/// is gone, and anything else in the index's directory stays.
// Core: init.cpp:2306 (f_wipe = fReindex); index/blockfilterindex.cpp:114-129
#[test]
fn reindex_wipes_the_filter_db_and_files_before_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[FILTERS]);
    generate(&node, dir.path(), 3);
    let built = serves_built(&node, dir.path(), 3);
    node.shutdown();
    drop(node);
    let before = positions(dir.path(), 3);
    let stray = index_dir(dir.path()).join("fltr00007.dat");
    std::fs::write(&stray, [7; 64]).unwrap();
    let other = index_dir(dir.path()).join("notes.txt");
    std::fs::write(&other, b"kept").unwrap();

    let mut node = started(dir.path(), &[FILTERS, "-reindex"]);
    assert_eq!(serves_built(&node, dir.path(), 3), built);
    node.shutdown();
    drop(node);
    assert!(!stray.exists(), "the filter files are wiped");
    assert!(other.exists(), "only the DB and the filter files are wiped");
    assert_eq!(positions(dir.path(), 3), before, "rebuilt from an empty fltr00000.dat");
}

/// `-reindex-chainstate` keeps the index and opens it only once the import ends: a locator the
/// block index does not know is init's one `InitError` at Step 8 without it, and the node's one
/// `AbortNode` after the import with it (e2's deferral: Core still runs `Init`'s check at Step 8).
// Core: init.cpp:2275-2276, :2305-2310; node/blockstorage.cpp:958-965; index/base.cpp:101-107
#[test]
fn reindex_chainstate_keeps_the_filter_index_and_opens_it_after_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[FILTERS]);
    let mined = generate(&node, dir.path(), 2);
    serves_built(&node, dir.path(), 2);
    node.shutdown();
    drop(node);
    let before = positions(dir.path(), 2);

    let mut node = started(dir.path(), &[FILTERS, "-reindex-chainstate"]);
    serves_built(&node, dir.path(), 2);
    node.shutdown();
    drop(node);
    assert_eq!(stored_locator(dir.path()), Some(mined[1].clone()));
    assert_eq!(positions(dir.path(), 2), before, "the index is kept");

    let path = index_dir(dir.path()).join("db");
    let (db, _) = RocksEngine::open(&path, &BLOCKFILTER_DB, &RocksConfig::default()).unwrap();
    let mut batch = db.batch();
    let mut locator = vec![0, 0, 0, 0, 1];
    locator.extend_from_slice(&[7; 32]);
    batch.put(META, BEST_BLOCK, &locator);
    db.write(batch, Commit::Durable).unwrap();
    drop(db);

    let refusal =
        "basic block filter index: best block of the index not found. Please rebuild the index.";
    let (mut node, _, stderr) = start(dir.path(), &[FILTERS]);
    assert!(node.app_init_early());
    assert!(!node.app_init_main());
    assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")]);
    drop(node);

    let (mut node, log, stderr) = start(dir.path(), &[FILTERS, "-reindex-chainstate"]);
    assert!(node.app_init_early() && node.app_init_main(), "{:?}", stderr.lock().unwrap());
    assert!(node.steps.join_import());
    assert_eq!(stderr.lock().unwrap().len(), 1, "{:?}", stderr.lock().unwrap());
    assert_eq!(log.lock().unwrap().iter().filter(|l| l.contains(refusal)).count(), 1);
    node.shutdown();
}

/// A fresh datadir with the index on starts it before genesis has connected; it idles, then
/// indexes genesis (Core's filter) and each mined block.
// Core: index/base.cpp:94-166, :189-268
#[test]
fn a_fresh_datadir_indexes_from_genesis() {
    let dir = tempfile::tempdir().unwrap();
    let mut node = started(dir.path(), &[FILTERS]);
    serves_built(&node, dir.path(), 0);
    generate(&node, dir.path(), 2);
    serves_built(&node, dir.path(), 2);
    node.shutdown();
}

/// Core's `-blockfilterindex` and `-peerblockfilters` values start, with the index on exactly
/// when `-blockfilterindex` names BASIC; Core's refusals keep Core's messages.
// Core: init.cpp:1223-1247
#[test]
fn cores_filter_option_values_start_and_its_refusals_keep_cores_messages() {
    let dir = tempfile::tempdir().unwrap();
    for (argv, on) in [
        (&["-blockfilterindex"][..], true),
        (&["-blockfilterindex=1"], true),
        (&[FILTERS], true),
        (&[FILTERS, "-peerblockfilters"], true),
        (&[FILTERS, "-peerblockfilters=0"], true),
        (&["-blockfilterindex=0"], false),
        (&["-noblockfilterindex"], false),
        (&["-nopeerblockfilters"], false),
    ] {
        let mut node = started(dir.path(), argv);
        assert_eq!(node.filter_index().is_some(), on, "{argv:?}");
        node.shutdown();
    }
    for (argv, refusal) in [
        (&["-blockfilterindex=x"][..], "Unknown -blockfilterindex value x."),
        (&[FILTERS, "-blockfilterindex=y"], "Unknown -blockfilterindex value y."),
        (&["-peerblockfilters"], "Cannot set -peerblockfilters without -blockfilterindex."),
    ] {
        let (mut node, _, stderr) = start(dir.path(), argv);
        assert!(!(node.app_init_early() && node.app_init_main()), "{argv:?}");
        assert_eq!(*stderr.lock().unwrap(), [format!("Error: {refusal}\n")], "{argv:?}");
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// An inbound V1 peer of the node, past `version` and `verack`.
struct Peer {
    stream: TcpStream,
    chain: dash_params::ChainSetup,
    /// The services the node's `version` advertised.
    services: u64,
}

impl Peer {
    fn connect(node: &Node) -> Self {
        let net = node.net.as_ref().expect("step 6 built it");
        let addr = net.connman().expect("started").listen_addrs()[0];
        let chain = node.params.chain.clone();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
        let now = i64::try_from(now.as_secs()).unwrap();
        let mut bytes = v1_frame(&chain, "version", &PeerVersionMsg::v24(now).payload());
        bytes.extend(v1_frame(&chain, "verack", &[]));
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        stream.write_all(&bytes).unwrap();
        let mut peer = Self {
            stream,
            chain,
            services: 0,
        };
        let version = peer.next("version");
        peer.services = u64::from_le_bytes(version[4..12].try_into().unwrap());
        peer
    }

    fn send(&mut self, command: &str, payload: &[u8]) {
        self.stream.write_all(&v1_frame(&self.chain, command, payload)).unwrap();
    }

    /// The payload of the node's next `command` message, skipping others.
    fn next(&mut self, command: &str) -> Vec<u8> {
        loop {
            let mut head = [0; 24];
            self.stream.read_exact(&mut head).unwrap_or_else(|e| panic!("awaiting {command}: {e}"));
            let name = head[4..16].split(|b| *b == 0).next().unwrap().to_vec();
            let len = u32::from_le_bytes(head[16..20].try_into().unwrap());
            let mut payload = vec![0; len as usize];
            self.stream.read_exact(&mut payload).unwrap();
            if name == command.as_bytes() {
                return payload;
            }
        }
    }
}

fn compact_size(bytes: &mut &[u8]) -> usize {
    let (first, rest) = bytes.split_first().unwrap();
    *bytes = rest;
    match *first {
        0xfd => {
            let (n, rest) = bytes.split_first_chunk::<2>().unwrap();
            *bytes = rest;
            usize::from(u16::from_le_bytes(*n))
        }
        n => {
            assert!(n < 0xfd, "fewer than 2^16");
            usize::from(n)
        }
    }
}

fn take32(bytes: &mut &[u8]) -> [u8; 32] {
    let (head, rest) = bytes.split_first_chunk::<32>().unwrap();
    *bytes = rest;
    *head
}

/// `getcfilters`, `getcfheaders` and `getcfcheckpt` over the whole chain `blocks`, each answered
/// from the index as Core answers.
// Core: net_processing.cpp:3435-3601; blockfilter.h:89-92; protocol.h:215-250
fn answers_bip157(peer: &mut Peer, blocks: &[Filter]) {
    let stop = blocks.last().unwrap();
    let request = |start: Option<u32>| {
        let mut payload = vec![0];
        if let Some(start) = start {
            payload.extend(start.to_le_bytes());
        }
        payload.extend_from_slice(stop.hash.as_bytes());
        payload
    };
    let first = blocks.len().saturating_sub(1000);
    peer.send("getcfilters", &request(Some(first as u32)));
    for want in &blocks[first..] {
        let got = peer.next("cfilter");
        let mut rest = &got[1..];
        assert_eq!(got[0], 0);
        assert_eq!(take32(&mut rest), *want.hash.as_bytes());
        assert_eq!(compact_size(&mut rest), want.encoded.len());
        assert_eq!(rest, want.encoded);
    }

    peer.send("getcfheaders", &request(Some(1)));
    let got = peer.next("cfheaders");
    let mut rest = &got[1..];
    assert_eq!(take32(&mut rest), *stop.hash.as_bytes());
    assert_eq!(take32(&mut rest), *blocks[0].header.as_bytes(), "the previous header");
    assert_eq!(compact_size(&mut rest), blocks.len() - 1);
    for want in &blocks[1..] {
        let filter =
            dashd_indexes::BlockFilter::from_encoded(&want.hash, want.encoded.clone(), true);
        assert_eq!(take32(&mut rest), *filter.unwrap().hash().as_bytes());
    }

    peer.send("getcfcheckpt", &request(None));
    let got = peer.next("cfcheckpt");
    let mut rest = &got[1..];
    assert_eq!(take32(&mut rest), *stop.hash.as_bytes());
    let checkpoints = blocks.len() / 1000;
    assert_eq!(compact_size(&mut rest), checkpoints);
    for n in 1..=checkpoints {
        assert_eq!(take32(&mut rest), *blocks[n * 1000].header.as_bytes());
    }
}

/// CH5's SPORK_23 simplepose scenario: masternode arguments alone, no filter option. Core's
/// soft-sets turn on `-peerblockfilters` and `-blockfilterindex=basic`, the index registers and
/// opens, its reader is attached before step 12 starts the connection manager, the node's
/// `version` advertises `NODE_COMPACT_FILTERS`, and a peer's `getcfilters`, `getcfheaders` and
/// `getcfcheckpt` are answered from the index.
// Core: init.cpp:1101-1106, :1407-1409, :2305-2310; active/dkgsession.cpp:304-308;
// net_processing.cpp:3435-3601
#[test]
fn masternode_args_alone_serve_compact_block_filters() {
    let dir = tempfile::tempdir().unwrap();
    let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
    let bind = format!("-bind=127.0.0.1:{}", free_port());
    let argv = [&key, &bind, "-dnsseed=0", "-fixedseeds=0", "-listenonion=0"];
    let (mut node, log, stderr) = start(dir.path(), &argv);
    let started = node.app_init_early() && node.app_init_main();
    assert!(started, "{:?}", stderr.lock().unwrap());
    for soft_set in ["-peerblockfilters=1", "-blockfilterindex=basic"] {
        let line = format!("-masternodeblsprivkey set -> setting {soft_set}");
        assert!(has(&log, &line), "{line}");
    }
    assert!(node.filter_index().is_some(), "the filter index registers");
    let net = node.net.as_ref().unwrap();
    assert_eq!(net.filters_at_start, Some(true), "attached before the connection manager");
    assert!(node.steps.join_import(), "loadblk ends");
    generate(&node, dir.path(), 2);
    let blocks = serves_built(&node, dir.path(), 2);
    // A masternode takes inbound peers once `mn_sync` has the chain; `loadblk`'s tip reaches it
    // in IBD, so the switch follows the signals' barrier.
    // Core: net.cpp:1978-1982 at v24.0.0
    let (tx, rx) = crossbeam_channel::bounded(1);
    node.steps.chain().unwrap().signals().enqueue(dashd_chainstate::chain::Queued::Barrier(tx));
    rx.recv_timeout(Duration::from_secs(60)).expect("the signals thread reaches the barrier");
    node.net.as_ref().unwrap().mn_sync().expect("the masternode services").switch_to_next_asset();

    let mut peer = Peer::connect(&node);
    assert_ne!(peer.services & NODE_COMPACT_FILTERS, 0, "{:#x}", peer.services);
    answers_bip157(&mut peer, &blocks);
    node.interrupt();
    node.shutdown();
}

/// A plain node with `-peerblockfilters` serves its stored filters over P2P, a checkpoint
/// included; without it the node does not advertise `NODE_COMPACT_FILTERS`.
// Core: init.cpp:1242-1249; net_processing.cpp:3435-3601
#[test]
fn a_node_serves_its_stored_filters_over_p2p() {
    let dir = tempfile::tempdir().unwrap();
    for (serve, blocks) in [(false, 0), (true, 1000)] {
        let bind = format!("-bind=127.0.0.1:{}", free_port());
        let peerblockfilters = format!("-peerblockfilters={}", u8::from(serve));
        let argv = [FILTERS, &peerblockfilters, &bind, "-dnsseed=0", "-fixedseeds=0"];
        let mut node = started(dir.path(), &[&argv[..], &["-listenonion=0"]].concat());
        if blocks > 0 {
            generate(&node, dir.path(), blocks);
        }
        let blocks = serves_built(&node, dir.path(), blocks);
        let mut peer = Peer::connect(&node);
        assert_eq!(peer.services & NODE_COMPACT_FILTERS != 0, serve, "{:#x}", peer.services);
        if serve {
            answers_bip157(&mut peer, &blocks);
        }
        node.interrupt();
        node.shutdown();
    }
}

/// A masternode that turns the filter index off is refused: the soft-set still turns
/// `-peerblockfilters` on, and Core's check needs the index for it.
// Core: init.cpp:1102-1106, :1243-1246
#[test]
fn a_masternode_without_the_filter_index_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let key = format!("-masternodeblsprivkey={}", "01".repeat(32));
    let argv = [&key, "-blockfilterindex=0"];
    let (mut node, _, stderr) = start(dir.path(), &argv);
    assert!(!(node.app_init_early() && node.app_init_main()));
    let refusal = "Error: Cannot set -peerblockfilters without -blockfilterindex.\n";
    assert_eq!(*stderr.lock().unwrap(), [refusal]);
}
