//! L2 (CH3a a7-2): `digest l2`'s driver, [`l2::run`].
//!
//! - **assumevalid** (CH3 D44; CH3a D151, D153). A synthetic regtest `blocks/` directory: a
//!   window of 110 blocks (a spend at 102) under a best chain of 8,175 headers. Off, every block
//!   checks its scripts. On, with every header preloaded and the preloaded tip as assumevalid,
//!   every block but genesis skips them, and the `C`s are the off run's, record for record.
//!   Condition 5 (`validation.cpp:2568`) is pinned at its boundary: with the best header at 8,166
//!   the first block checked is 102, where the equivalent time is exactly two weeks. A
//!   `defaultAssumeValid` inside the preloaded chain stops the skips at its height, and a
//!   minimum chain work above the preloaded tip's is lowered to it.
//! - **The reverse leg** (CH3 D48; CH3a D152). The non-evo FT nodes (CH3a D143) disconnect back
//!   to genesis, feature_block to 1100, each rewind matching its golden's parent and the coins
//!   scan matching golden(L). A corrupted golden value is reported at its height and key, and the
//!   run leaves no output.
//! - **The CLI** (CH3a D154). `digest l2` exits 0 with its digest, reverse log and manifest, and
//!   2 with none of them on an error.
//! - **The suffix check** (CH3 R11; CH3a D165-D170). A run to 100 saves its coins as a snapshot;
//!   a pruned start there runs 101..=110 and logs the run from genesis's records on every key but
//!   `muhash`, which it finalizes at 110 and at the reverse leg's end only, both equal; the
//!   reverse leg to 100 logs the full run's. A snapshot whose coins differ from golden(100) in
//!   their MuHash alone is refused before V starts, as is each mismatch between the snapshot, its
//!   sidecar, the golden and the blocks directory's chain. The blocks directory is only read:
//!   every run passes with it made read-only, and a test binary killed while it is read-only
//!   leaves it writable (BL-429).
//! - **Testnet** (CH3a D146, D153). With `CH3A_TESTNET_WINDOW` naming the pinned window
//!   directory, heights 0..6999 run off and on. On, about 1.57M headers are preloaded, all 6,999
//!   blocks skip, the `C`s equal the off run's, the log is clean in linear mode against the
//!   golden, and the reverse leg returns to genesis; off, it returns to 2,000, below DERSIG and
//!   CLTV, where the coins scan is not empty. A pruned start at 4,999 runs 5,000..=6,999 on and
//!   off, clean against the golden but for `muhash`, and reverses to 6,000. Without the variable
//!   the tests say they skipped. Run them with `--release`.

mod digest_common;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use dash_conformance::digest::blkdir::BlockDir;
use dash_conformance::digest::compare::{Input, compare};
use dash_conformance::digest::l2::{
    self, L2, MAX_HEADERS_RESULTS, MAX_PAR, Options, Source, reverse_path,
};
use dash_conformance::digest::store::Node;
use dash_conformance::digest::utxo::{self, sidecar_path};
use dash_consensus::coins::Coin;
use dash_consensus::primitives::{Tx, core_serialize};
use dash_params::{ChainParams, ChainParamsBuilder, NetworkKind, Release};
use dash_primitives::BlockHash;
use dash_statedigest::compare::{FieldId, Mode, Options as CompareOptions, Report, Verdict};
use dash_statedigest::decisions::Policy;
use dash_statedigest::{Chain, Class, FIELDS, Kind};
use dash_types::Numeric as _;
use dashd_chainstate::chain::genesis_block;
use digest_common::{coinbase_sig, decode, mine, mine_at, stored, tempdir};
use rayon::prelude::*;

/// The synthetic window's end, and the height of its spend.
const WINDOW: u32 = 110;
const SPEND_AT: u32 = 102;
/// The synthetic best chain's tip: every window block is more than two weeks of work below it.
const BEST: u32 = WINDOW + 8_065;
/// A best header at which condition 5 ends at the spend: `BOUNDARY − 102` is 8,064 blocks of
/// 150 s, exactly 1,209,600 s, which is not more than two weeks.
const BOUNDARY: u32 = SPEND_AT + 8_064;
/// What each coinbase pays to `OP_TRUE`.
const COIN: i64 = 100_000_000;
/// A block V refuses as `time-too-old`, in the preload's second batch.
const TOO_OLD: u32 = 2_100;

/// The non-evo nodes and where their reverse legs end.
const REVERSE: [(&str, u32); 8] = [
    ("feature_block/node0", 1_100),
    ("feature_new_quorum_type_activation/node0", 0),
    ("mining_basic/node0", 0),
    ("mining_basic/node1", 0),
    ("p2p_invalid_block/node0", 0),
    ("rpc_invalidateblock/node0", 0),
    ("rpc_invalidateblock/node1", 0),
    ("rpc_invalidateblock/node2", 0),
];

/// The testnet window (CH3a D146).
const TESTNET_TIP: u32 = 6_999;
/// The keys the testnet window's compare requires (CH3a a7-1 brief).
const WINDOW_REQUIRE: [&str; 10] =
    ["muhash", "undo", "stats", "bi", "txouts", "amount", "unspendable", "bls", "bur", "dep"];
const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

fn regtest() -> ChainParams {
    ChainParamsBuilder::new(NetworkKind::Regtest, Release::V24_0).build().unwrap()
}

fn options() -> Options {
    Options {
        assume_valid: false,
        par: 1,
        coins_cache: 256 << 20,
        reverse_to: None,
        golden: None,
        sha256: false,
        headers: None,
        save_utxo: None,
        start_utxo: None,
    }
}

/// A version-1 transaction with one output of `COIN` to `OP_TRUE`.
fn tx(prevout: ([u8; 32], u32), sig: &[u8]) -> Arc<Tx> {
    let mut raw = 1i32.to_le_bytes().to_vec();
    raw.push(1);
    raw.extend(prevout.0);
    raw.extend(prevout.1.to_le_bytes());
    raw.push(sig.len() as u8);
    raw.extend(sig);
    raw.extend(u32::MAX.to_le_bytes());
    raw.push(1);
    raw.extend(COIN.to_le_bytes());
    raw.extend([1, 0x51]);
    raw.extend(0u32.to_le_bytes());
    decode(&raw)
}

/// The coinbase at `height`: the height and a 4-byte push, paying `COIN` to `OP_TRUE`.
fn coinbase(height: u32) -> Arc<Tx> {
    tx(([0; 32], u32::MAX), &coinbase_sig(height))
}

/// Regtest's chain to `BEST`, with block `SPEND_AT` spending block 1's coinbase, in a
/// `blocks/blk00000.dat` under the returned directory.
fn synthetic(p: &ChainParams) -> (tempfile::TempDir, Vec<BlockHash>) {
    synthetic_to(p, BEST, None)
}

/// How one block of [`synthetic_to`]'s chain differs from [`synthetic`]'s.
#[derive(Clone, Copy)]
enum Odd {
    /// Its median time past is its time, which V refuses (`time-too-old`,
    /// `validation.cpp:4505-4506`).
    TooOld,
    /// It is one second after its parent rather than 150: a valid block, so the chain forks there.
    Fork,
}

/// As [`synthetic`], to `best`. The block at `odd`'s height, if given, differs as `odd` says; the
/// chain goes on from it.
fn synthetic_to(
    p: &ChainParams,
    best: u32,
    odd: Option<(u32, Odd)>,
) -> (tempfile::TempDir, Vec<BlockHash>) {
    let dir = tempdir();
    let blocks = dir.path().join("blocks");
    std::fs::create_dir(&blocks).unwrap();
    let mut file =
        std::io::BufWriter::new(std::fs::File::create(blocks.join("blk00000.dat")).unwrap());
    let mut prev = genesis_block(p);
    let mut hashes = vec![prev.hash()];
    let mut first_coinbase = None;
    for h in 0..=best {
        if h > 0 {
            let spend = match h {
                SPEND_AT => vec![tx((first_coinbase.unwrap(), 0), &[])],
                _ => vec![],
            };
            // Block `h` on `prev`: its coinbase, then `spend`, mined under regtest's limit.
            let txs = std::iter::once(coinbase(h)).chain(spend).collect();
            let b = match odd.filter(|&(at, _)| at == h) {
                // The median of the 11 times to `prev`, 150 s apart (`chain.h:281-295`).
                Some((_, Odd::TooOld)) => mine_at(&prev, txs, prev.header().time() - 5 * 150),
                Some((_, Odd::Fork)) => mine_at(&prev, txs, prev.header().time() + 1),
                None => mine(&prev, txs),
            };
            if h == 1 {
                first_coinbase = Some(*b.txs()[0].txid().as_bytes());
            }
            hashes.push(b.hash());
            prev = b;
        }
        let bytes = core_serialize(&*prev);
        file.write_all(&p.message_start).unwrap();
        file.write_all(&(bytes.len() as u32).to_le_bytes()).unwrap();
        file.write_all(&bytes).unwrap();
    }
    file.into_inner().unwrap().sync_all().unwrap();
    (dir, hashes)
}

/// Runs the window of `dir` under `opts`, writing `<name>.jsonl` and its manifest in `out`.
fn run_dir(
    dir: &BlockDir,
    p: &ChainParams,
    opts: &Options,
    out: &Path,
    name: &str,
) -> anyhow::Result<L2> {
    run_to(dir, p, opts, out, name, WINDOW)
}

/// [`run_dir`], the window ending at `to`.
fn run_to(
    dir: &BlockDir,
    p: &ChainParams,
    opts: &Options,
    out: &Path,
    name: &str,
    to: u32,
) -> anyhow::Result<L2> {
    let source = Source::Dir {
        dir,
        params: Arc::new(p.clone()),
        chain: Chain::Regtest,
        to,
    };
    let log = out.join(format!("{name}.jsonl"));
    let manifest = out.join(format!("{name}.manifest.json"));
    l2::run(source, opts, &log, &manifest, &out.join(format!("{name}-work")))
}

/// A log's lines after its `H`.
fn records(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines().map(str::to_owned);
    assert!(lines.next().unwrap().starts_with(r#"{"k":"H""#));
    lines.collect()
}

fn runs(skip_to: u32, check_from: Option<u32>) -> Vec<(u32, u32, Option<bool>)> {
    let mut runs = vec![(0, 0, None)];
    if skip_to > 0 {
        runs.push((1, skip_to, Some(false)));
    }
    if let Some(from) = check_from {
        runs.push((from, WINDOW, Some(true)));
    }
    runs
}

/// CH3a D151, D152, D153: with every header preloaded and the preloaded tip as assumevalid, every
/// window block but genesis skips its scripts, and the log is the off run's; the reverse leg
/// rewinds onto the off run's log.
#[test]
fn assumevalid_on_skips_every_block_and_logs_what_off_logs() {
    let p = regtest();
    let (src, hashes) = synthetic(&p);
    let genesis = genesis_block(&p).hash();
    let dir = BlockDir::open(&src.path().join("blocks"), p.message_start, genesis, None).unwrap();
    assert_eq!(dir.tip(), BEST);
    let out = tempdir();

    let off = run_dir(&dir, &p, &options(), out.path(), "off").unwrap();
    assert_eq!(off.forward.runs, runs(0, Some(1)));
    assert_eq!((off.forward.connects, off.forward.checked), (u64::from(WINDOW) + 1, 110));
    assert_eq!(off.manifest["choices"]["assumevalid"]["mode"], "off");
    assert_eq!(off.manifest["preload"], serde_json::Value::Null);

    // On, reversed to genesis against the off run's log.
    let mut opts = options();
    opts.assume_valid = true;
    opts.par = MAX_PAR;
    opts.reverse_to = Some(0);
    opts.golden = Some(out.path().join("off.jsonl"));
    let on = run_dir(&dir, &p, &opts, out.path(), "on").unwrap();
    assert_eq!(on.tip, off.tip);
    assert_eq!(on.forward.runs, runs(WINDOW, None));
    assert_eq!((on.forward.skipped, on.forward.checked, on.forward.undecided), (110, 0, 1));
    assert_eq!(records(&out.path().join("on.jsonl")), records(&out.path().join("off.jsonl")));

    let m = &on.manifest;
    let av = &m["choices"]["assumevalid"];
    assert_eq!(av["hash"], hashes[BEST as usize].to_string());
    assert_eq!(av["height"], BEST);
    assert_eq!(av["reason"], "the chain's defaultAssumeValid is null: the preloaded tip");
    let work = &m["choices"]["minimum_chain_work"];
    assert_eq!(work["reason"], "the chain's default: the preloaded tip's work meets it");
    assert_eq!(work["value"], work["default"]);
    assert_eq!(m["preload"]["headers"], BEST);
    assert_eq!(m["preload"]["batches"], 5);
    assert_eq!(m["par"], MAX_PAR);
    // `-par=16`: 15 more threads, whatever the cores (`init.cpp:1554-1565`).
    assert_eq!(m["script_check_threads"], 15);
    assert!(m["timings"]["v_time_s"].as_f64().unwrap() > 0.0);

    eprintln!(
        "synthetic: off V time {} s; on preload {}, V time {} s, reverse {} s",
        off.manifest["timings"]["v_time_s"],
        m["preload"],
        m["timings"]["v_time_s"],
        m["reverse"]["wall_s"]
    );
    let reverse = on.reverse.as_ref().unwrap();
    assert_eq!(reverse.to, 0);
    assert_eq!((reverse.tally.disconnects, reverse.tally.disconnects_ok), (110, 110));
    assert_eq!(reverse.compared, 110);
    assert_eq!((reverse.end.txouts, reverse.end.amount), (0, 0));
    assert_eq!(m["reverse"]["end_check"]["equal"], true);
    let reversed = records(&reverse_path(&out.path().join("on.jsonl")));
    assert_eq!(reversed.len(), 110);
    for (i, line) in reversed.iter().enumerate() {
        let h = WINDOW as usize - i;
        assert!(line.starts_with(&format!(r#"{{"k":"D","h":{h},"#)), "{line}");
        assert!(line.contains(r#""s":{"res":"ok"}"#), "{line}");
    }
}

/// CH3a D153: condition 5's boundary, pinned. With the best header at `BOUNDARY`, blocks 1..=101
/// skip and the spend at 102, exactly two weeks of work below it, is the first checked.
/// `defaultAssumeValid` in the preloaded chain is the assumevalid block, and the skips stop at its
/// height; a minimum chain work above the tip's is lowered to the tip's.
#[test]
fn condition_5_ends_the_skips_two_weeks_below_the_best_header() {
    let p = regtest();
    let (src, hashes) = synthetic(&p);
    let genesis = genesis_block(&p).hash();
    let blocks = src.path().join("blocks");
    let out = tempdir();
    let mut opts = options();
    opts.assume_valid = true;

    let anchor = Some(hashes[BOUNDARY as usize]);
    let dir = BlockDir::open(&blocks, p.message_start, genesis, anchor).unwrap();
    let r = run_dir(&dir, &p, &opts, out.path(), "boundary").unwrap();
    assert_eq!(r.forward.runs, runs(SPEND_AT - 1, Some(SPEND_AT)));
    assert_eq!(r.manifest["choices"]["assumevalid"]["height"], BOUNDARY);
    assert_eq!(r.manifest["preload"]["headers"], BOUNDARY);
    let one_more = Some(hashes[BOUNDARY as usize + 1]);
    let dir = BlockDir::open(&blocks, p.message_start, genesis, one_more).unwrap();
    let r = run_dir(&dir, &p, &opts, out.path(), "boundary-plus-one").unwrap();
    assert_eq!(r.forward.runs, runs(SPEND_AT, Some(SPEND_AT + 1)));

    // defaultAssumeValid at 50 and a minimum chain work above the tip's.
    let dir = BlockDir::open(&blocks, p.message_start, genesis, None).unwrap();
    let mut q = p.clone();
    q.consensus.default_assume_valid = dash_num::Hash256::from_lendian(*hashes[50].as_bytes());
    q.consensus.minimum_chain_work = dash_num::Arith256::from_u64(u64::from(BEST) * 2 + 3);
    let r = run_dir(&dir, &q, &opts, out.path(), "default").unwrap();
    assert_eq!(r.forward.runs, runs(50, Some(51)));
    let m = &r.manifest["choices"];
    assert_eq!(m["assumevalid"]["hash"], hashes[50].to_string());
    assert_eq!(m["assumevalid"]["reason"], "defaultAssumeValid is in the preloaded chain, at 50");
    // Each regtest block's proof is 2: 0x207fffff's target is just under 2^255.
    let tip_work = dash_num::Arith256::from_u64(u64::from(BEST) * 2 + 2);
    assert_eq!(m["minimum_chain_work"]["value"], tip_work.to_string());
    assert_eq!(m["minimum_chain_work"]["preloaded_tip_work"], tip_work.to_string());
    assert_eq!(
        m["minimum_chain_work"]["reason"],
        "the preloaded tip's work: the chain's default is above it"
    );
    assert_eq!(
        records(&out.path().join("default.jsonl")),
        records(&out.path().join("boundary.jsonl"))
    );
}

/// CH3a D162: a header file's chain, past the blocks directory's tip, is what the preload sends and
/// what assumevalid reads, as on mainnet: the directory anchored at the window's end, the file to
/// `BOUNDARY` + 1 with `defaultAssumeValid` and enough work only there. The spend at 102 skips, as
/// with the directory anchored one past `BOUNDARY`, and V takes every header (the mock clock is the
/// file's: the window's would be hours behind them). Without the file every block is checked. A
/// window past the directory's tip, a file that leaves the window's chain, ends below the
/// directory's tip, lacks `defaultAssumeValid` or has less than the minimum chain work, and a file
/// with assumevalid off are refused. Each refusal's parameters pass every other check, so only its
/// own guard refuses it (Opus a7-3 r1 Nit 2).
#[test]
fn a_header_file_preloads_past_the_directory_s_tip() {
    let p = regtest();
    let (src, hashes) = synthetic(&p);
    let genesis = genesis_block(&p).hash();
    let blocks = src.path().join("blocks");
    let open = |tip: u32| {
        BlockDir::open(&blocks, p.message_start, genesis, Some(hashes[tip as usize])).unwrap()
    };
    let (full, dir) = (open(BEST), open(WINDOW));
    let out = tempdir();
    let write = |name: &str, dir: &BlockDir, to: u32| {
        let path = out.path().join(name);
        let bytes: Vec<u8> =
            dir.headers(0, to).flat_map(|h| core_serialize(&h.unwrap().0)).collect();
        std::fs::write(&path, bytes).unwrap();
        path
    };
    let mut q = p.clone();
    let at = BOUNDARY as usize + 1;
    q.consensus.default_assume_valid = dash_num::Hash256::from_lendian(*hashes[at].as_bytes());
    // Each regtest block's proof is 2: the file's tip has just this work, the directory's less.
    let file_work = dash_num::Arith256::from_u64(u64::from(BOUNDARY + 2) * 2);
    q.consensus.minimum_chain_work = file_work;
    let mut opts = options();
    opts.assume_valid = true;
    opts.headers = Some(write("one-more.bin", &full, BOUNDARY + 1));
    let r = run_dir(&dir, &q, &opts, out.path(), "file").unwrap();
    assert_eq!(r.forward.runs, runs(SPEND_AT, Some(SPEND_AT + 1)));
    let m = &r.manifest;
    assert_eq!(m["preload"]["headers"], BOUNDARY + 1);
    assert_eq!(m["preload"]["source"]["form"], "header-file");
    assert_eq!(m["preload"]["source"]["tip"], BOUNDARY + 1);
    assert_eq!(m["choices"]["assumevalid"]["hash"], hashes[at].to_string());
    let reason = format!("defaultAssumeValid is in the preloaded chain, at {at}");
    assert_eq!(m["choices"]["assumevalid"]["reason"], reason);
    let work = &m["choices"]["minimum_chain_work"];
    assert_eq!(work["value"], file_work.to_string());
    assert_eq!(work["preloaded_tip_work"], file_work.to_string());
    assert_eq!(work["reason"], "the chain's default: the preloaded tip's work meets it");

    // Without the file, the directory's own chain: the best header is the window's end.
    let r = run_dir(
        &dir,
        &q,
        &Options {
            headers: None,
            ..opts.clone()
        },
        out.path(),
        "dir",
    )
    .unwrap();
    assert_eq!(r.forward.runs, runs(0, Some(1)));
    let source = serde_json::json!({"form": "blocks-dir", "tip": WINDOW});
    assert_eq!(r.manifest["preload"]["source"], source);

    // `p` with `defaultAssumeValid` at `hash`, and regtest's zero minimum chain work.
    let av = |hash: BlockHash| {
        let mut p = p.clone();
        p.consensus.default_assume_valid = dash_num::Hash256::from_lendian(*hash.as_bytes());
        p
    };
    // Block 5 differs on the other chain, which is valid and reaches the directory's tip.
    let (other, other_hashes) = synthetic_to(&p, WINDOW, Some((5, Odd::Fork)));
    let other =
        BlockDir::open(&other.path().join("blocks"), p.message_start, genesis, None).unwrap();
    // A window below the directory's tip, so that a file can hold the window and fall short of the
    // tip.
    let (short, past_window) = (open(WINDOW - 10), open(WINDOW + 5));
    let with = |headers| Options {
        headers: Some(headers),
        ..opts.clone()
    };
    let mut above = q.clone();
    let more_work = dash_num::Arith256::from_u64(u64::from(BOUNDARY + 3) * 2);
    above.consensus.minimum_chain_work = more_work;
    let refusals = [
        (
            &short,
            &q,
            opts.clone(),
            format!("the window ends at {WINDOW}, past the best chain's tip {}", WINDOW - 10),
        ),
        (
            &dir,
            &av(other_hashes[WINDOW as usize]),
            with(write("other.bin", &other, WINDOW)),
            format!(
                "height 5: the file has {}, the blocks directory {}",
                other_hashes[5], hashes[5]
            ),
        ),
        (
            &past_window,
            &av(hashes[WINDOW as usize + 2]),
            with(write("short.bin", &full, WINDOW + 2)),
            format!(
                "the headers end at {}, below the blocks directory's tip {}",
                WINDOW + 2,
                WINDOW + 5
            ),
        ),
        (
            &dir,
            &q,
            Options {
                assume_valid: false,
                ..opts.clone()
            },
            "one-more.bin: a header file is preloaded: assumevalid must be on".to_owned(),
        ),
        (
            &dir,
            &av(hashes[BEST as usize]),
            opts.clone(),
            format!(
                "one-more.bin: defaultAssumeValid {} is not in the header file's chain",
                hashes[BEST as usize]
            ),
        ),
        (
            &dir,
            &above,
            opts.clone(),
            format!(
                "one-more.bin: the headers' work {file_work} is below the chain's minimum chain \
                 work {more_work}"
            ),
        ),
    ];
    for (i, (dir, params, opts, want)) in refusals.into_iter().enumerate() {
        let r = run_dir(dir, params, &opts, out.path(), &format!("refused-{i}"));
        let e = format!("{:#}", r.err().unwrap_or_else(|| panic!("{i}: the run was not refused")));
        assert!(e.ends_with(&want), "{i}: {e}");
    }
    let left: Vec<_> =
        std::fs::read_dir(out.path()).unwrap().map(|f| f.unwrap().file_name()).collect();
    assert!(!left.iter().any(|f| f.to_string_lossy().starts_with("refused-")), "{left:?}");
}

/// CH3a D153 (Opus a7-2 r1 N2): a header V refuses fails the preload. Block `TOO_OLD`'s time is
/// its median time past, so V stops the second batch at it, and the run fails with V's reason
/// and leaves no output.
#[test]
fn a_header_v_refuses_fails_the_preload() {
    let p = regtest();
    let (src, _) = synthetic_to(&p, TOO_OLD + 100, Some((TOO_OLD, Odd::TooOld)));
    let genesis = genesis_block(&p).hash();
    let dir = BlockDir::open(&src.path().join("blocks"), p.message_start, genesis, None).unwrap();
    let out = tempdir();
    let mut opts = options();
    opts.assume_valid = true;
    let e = format!("{:#}", run_dir(&dir, &p, &opts, out.path(), "refused").unwrap_err());
    let (first, index) = (MAX_HEADERS_RESULTS + 1, TOO_OLD as usize - MAX_HEADERS_RESULTS - 1);
    let stopped = format!("the headers from {first} stopped: Some(Invalid {{ index: {index},");
    assert!(e.contains(&stopped), "{e}");
    assert!(e.contains("time-too-old"), "{e}");
    let left: Vec<_> =
        std::fs::read_dir(out.path()).unwrap().map(|f| f.unwrap().file_name()).collect();
    assert!(!left.iter().any(|f| f.to_string_lossy().starts_with("refused.")), "{left:?}");
}

fn run_node(node: &Node, opts: &Options, out: &Path) -> anyhow::Result<L2> {
    let source = Source::Node {
        node,
        policy: Policy::Strict,
    };
    let log = out.join("node.jsonl");
    l2::run(source, opts, &log, &out.join("node.manifest.json"), &out.join("work"))
}

/// CH3a D152: every non-evo FT node disconnects back to its end, each rewind and the coins scan
/// matching its golden.
#[test]
fn ft_nodes_reverse_onto_their_goldens() {
    REVERSE.par_iter().for_each(|&(label, to)| {
        let node = stored(label);
        let out = tempdir();
        let mut opts = options();
        opts.reverse_to = Some(to);
        opts.golden = Some(node.golden());
        let r = run_node(&node, &opts, out.path()).unwrap_or_else(|e| panic!("{label}: {e:#}"));
        let n = u64::from(r.tip.0 - to);
        let reverse = r.reverse.unwrap();
        assert_eq!((reverse.to, reverse.compared), (to, n), "{label}");
        assert_eq!((reverse.tally.disconnects, reverse.tally.disconnects_ok), (n, n), "{label}");
        assert_eq!(reverse.tally.connects + reverse.tally.rejects, 0, "{label}");
        eprintln!(
            "{label}: {} -> {to}, {n} rewinds equal, end check txouts {} amount {}",
            r.tip.0, reverse.end.txouts, reverse.end.amount
        );
    });
}

/// `golden` with `key`'s value at height `h` in its `C` replaced by `with`.
fn corrupt(golden: &Path, out: &Path, h: u32, key: &str, with: &str) -> PathBuf {
    let mut reader = dash_conformance::digest::store::open(golden).unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut reader, &mut text).unwrap();
    let prefix = format!(r#"{{"k":"C","h":{h},"#);
    let mut hit = 0;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            if !line.starts_with(&prefix) {
                return line.to_owned();
            }
            let at = line.find(&format!(r#""{key}":"#)).unwrap() + key.len() + 3;
            let end = at + line[at..].find([',', '}']).unwrap();
            hit += 1;
            format!("{}{with}{}", &line[..at], &line[end..])
        })
        .collect();
    assert_eq!(hit, 1);
    let path = out.join("corrupt.jsonl");
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

/// CH3a D152: a corrupted golden value stops the reverse leg at its height, naming the key; the
/// end check alone catches a corrupted golden(L) when the leg has nothing to rewind. A failed run
/// leaves no digest, reverse log or manifest.
#[test]
fn a_corrupted_golden_is_named_by_height_and_key() {
    let node = stored("p2p_invalid_block/node0");
    let out = tempdir();
    let golden = corrupt(&node.golden(), out.path(), 50, "txouts", "7");
    let mut opts = options();
    opts.reverse_to = Some(0);
    opts.golden = Some(golden);
    let e = format!("{:#}", run_node(&node, &opts, out.path()).unwrap_err());
    assert_eq!(e, "reverse leg: at 50, txouts is 50, golden(50) has 7");
    for name in ["node.jsonl", "node.reverse.jsonl", "node.manifest.json"] {
        assert!(!out.path().join(name).exists(), "{name}");
    }

    // At the tip, nothing rewinds: the scan meets golden(103).
    let out = tempdir();
    opts.reverse_to = Some(103);
    opts.golden = Some(node.golden());
    let r = run_node(&node, &opts, out.path()).unwrap();
    let reverse = r.reverse.unwrap();
    assert_eq!((reverse.compared, reverse.tally.disconnects), (0, 0));
    assert_eq!((reverse.end.txouts, reverse.end.amount), (103, 5_105_000_000_000));

    let out = tempdir();
    opts.golden = Some(corrupt(&node.golden(), out.path(), 103, "amount", "1"));
    let e = format!("{:#}", run_node(&node, &opts, out.path()).unwrap_err());
    assert_eq!(e, "end check: the coins scan at 103 gives amount 5105000000000, golden(103) has 1");
}

/// `digest l2`, the CLI over [`l2::run`] (CH3a D154): exit 0 with its digest, reverse log and
/// manifest, and exit 2 with none of them on an error.
#[test]
fn the_binary_runs_l2_and_exits_2_on_an_error() {
    let node = stored("p2p_invalid_block/node0");
    let out = tempdir();
    let log = out.path().join("cli.jsonl");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
            .args(["digest", "l2", "--node"])
            .arg(&node.dir)
            .arg("--golden")
            .arg(node.golden())
            .args(args)
            .args(["--par", "2", "--out"])
            .arg(&log)
            .output()
            .unwrap()
    };

    let o = run(&["--reverse-to", "104"]);
    assert_eq!(o.status.code(), Some(2));
    let stderr = String::from_utf8(o.stderr).unwrap();
    assert_eq!(stderr, "error: --reverse-to 104 is above the window's end 103\n");
    // A header file preloads a blocks directory's run only (D162).
    let o = run(&["--headers", "headers.bin"]);
    assert_eq!(o.status.code(), Some(2));
    let stderr = String::from_utf8(o.stderr).unwrap();
    assert!(stderr.contains("--blocks-dir <BLOCKS_DIR>"), "{stderr}");
    assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);

    let o = run(&["--reverse-to", "100"]);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let manifest = out.path().join("cli.jsonl.manifest.json");
    let stdout = String::from_utf8(o.stdout).unwrap();
    assert!(
        stdout.ends_with(&format!(
            "; reverse to 100: D 3 ok 3, compared 3, end check equal; manifest {}\n",
            manifest.display()
        )),
        "{stdout}"
    );
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
    assert_eq!((&m["par"], &m["script_check_threads"]), (&2.into(), &1.into()));
    assert_eq!(m["input"]["node"], "p2p_invalid_block/node0");
    assert_eq!(m["reverse"]["compared_with_golden"], 3);
    assert_eq!(m["forward"]["checked"], 103);
    assert!(log.exists() && reverse_path(&log).exists());
}

fn field(key: &str) -> FieldId {
    FieldId::find(Kind::Connect, Class::Semantic, key).unwrap_or_else(|| panic!("no key {key}"))
}

/// Linear mode from height 1 with the window's required keys, the evo keys left out.
fn linear(left: &Path, right: &Path) -> Report {
    let mut opts = CompareOptions::new(Mode::Linear);
    opts.from = Some(1);
    opts.to = Some(i64::from(TESTNET_TIP));
    opts.require = WINDOW_REQUIRE.iter().map(|k| field(k)).collect();
    opts.exclude = EVO_KEYS.iter().map(|k| field(k)).collect();
    let mut inputs = [Input::open(left).unwrap(), Input::open(right).unwrap()];
    compare(&mut inputs, opts).unwrap()
}

/// CH3a D153: the testnet window off and on. On, ~1.57M headers are preloaded, all 6,999 blocks
/// skip, the `C`s are the off run's, the log is clean against the golden, and the reverse leg
/// returns to genesis with every rewind and the coins scan matching the golden.
#[test]
fn a_testnet_window_skips_every_block_with_assumevalid_and_reverses_to_genesis() {
    let Some(window) = std::env::var_os("CH3A_TESTNET_WINDOW").map(PathBuf::from) else {
        let _ = writeln!(
            std::io::stderr(),
            "\n*** SKIPPED a_testnet_window_skips_every_block_with_assumevalid_and_reverses_to_\
             genesis: CH3A_TESTNET_WINDOW is not set. Set it to the pinned window directory \
             (CH3a D146) and run with --release. ***\n"
        );
        return;
    };
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Testnet, Release::V24_0).build().unwrap());
    let golden = window.join("golden-0-6999.jsonl.zst");
    let genesis = genesis_block(&p).hash();
    let dir = BlockDir::open(&window.join("blocks"), p.message_start, genesis, None).unwrap();
    assert!(dir.tip() > 1_500_000, "the copy's best chain ends at {}", dir.tip());
    let out = tempdir();
    let run = |opts: &Options, name: &str| {
        let source = Source::Dir {
            dir: &dir,
            params: p.clone(),
            chain: Chain::Test,
            to: TESTNET_TIP,
        };
        let log = out.path().join(format!("{name}.jsonl"));
        let manifest = out.path().join(format!("{name}.manifest.json"));
        let r = l2::run(source, opts, &log, &manifest, &out.path().join(format!("{name}-work")));
        (r.unwrap_or_else(|e| panic!("{name}: {e:#}")), log)
    };

    // Off, reversed to 2,000: below DERSIG (2,075) and CLTV (2,431), with coins left to scan.
    let mut opts = options();
    opts.par = MAX_PAR;
    opts.reverse_to = Some(2_000);
    opts.golden = Some(golden.clone());
    let (off, off_log) = run(&opts, "off");
    assert_eq!(off.forward.runs, [(0, 0, None), (1, TESTNET_TIP, Some(true))]);
    let reverse = off.reverse.as_ref().unwrap();
    let n = u64::from(TESTNET_TIP - 2_000);
    assert_eq!((reverse.compared, reverse.tally.disconnects_ok), (n, n));
    assert!(reverse.end.txouts > 0 && reverse.end.amount > 0, "{:?}", reverse.end);

    opts.assume_valid = true;
    opts.reverse_to = Some(0);
    let (on, on_log) = run(&opts, "on");
    assert_eq!(on.tip, off.tip);
    assert_eq!(on.forward.runs, [(0, 0, None), (1, TESTNET_TIP, Some(false))]);
    assert_eq!(on.forward.skipped, u64::from(TESTNET_TIP));
    assert_eq!(on.manifest["preload"]["headers"], dir.tip());
    assert_eq!(records(&on_log), records(&off_log));

    // Every `C.s` key but the evo ones equal at every height, as a7-1's off run (CH3a D147).
    let compared: Vec<FieldId> = FIELDS
        .iter()
        .filter(|f| f.kind == Kind::Connect && f.class == Class::Semantic)
        .filter(|f| !EVO_KEYS.contains(&f.key))
        .map(|f| field(f.key))
        .collect();
    let pairs = u64::from(TESTNET_TIP);
    for (log, what) in [(&off_log, "off"), (&on_log, "on")] {
        let report = linear(log, &golden);
        assert_eq!(
            report.verdict.exit_code(),
            0,
            "{what}: {:?} {:?}",
            report.verdict,
            report.first()
        );
        assert_eq!(report.pairs, pairs, "{what}");
        assert!(report.uncovered.is_empty(), "{what}: {:?}", report.uncovered);
        for f in &compared {
            let c = report.coverage.get(f).copied().unwrap_or_default();
            assert_eq!((c.equal, c.differ, c.absent()), (pairs, 0, 0), "{what}: {f}");
        }
    }

    let reverse = on.reverse.unwrap();
    let n = u64::from(TESTNET_TIP);
    assert_eq!((reverse.compared, reverse.tally.disconnects_ok), (n, n));
    assert_eq!((reverse.end.txouts, reverse.end.amount), (0, 0));
    for (name, r) in [("off", &off.manifest), ("on", &on.manifest)] {
        eprintln!(
            "testnet {name}: V time {} s, skipped {}, preload {}, reverse {}",
            r["timings"]["v_time_s"], r["forward"]["skipped"], r["preload"], r["reverse"]
        );
    }
}

/// The suffix check's base: below the spend at 102, whose coin (block 1's coinbase) the snapshot
/// holds.
const BASE: u32 = 100;

/// `--exclude`'s list for a suffix log: `muhash`, finalized at two heights only, and the evo keys,
/// which V does not write before CH7 (CH3a D167).
fn suffix_excluded() -> Vec<FieldId> {
    std::iter::once("muhash").chain(EVO_KEYS).map(field).collect()
}

/// Linear mode over `from..=to`, the suffix's keys excluded.
fn linear_suffix(left: &Path, right: &Path, from: u32, to: u32) -> Report {
    let mut opts = CompareOptions::new(Mode::Linear);
    opts.from = Some(i64::from(from));
    opts.to = Some(i64::from(to));
    opts.exclude = suffix_excluded();
    let mut inputs = [Input::open(left).unwrap(), Input::open(right).unwrap()];
    compare(&mut inputs, opts).unwrap()
}

/// `line` without its schedule-class `x`, the last key: its `seq` counts the log's records.
fn without_x(line: &str) -> &str {
    &line[..line.rfind(r#","x":"#).expect("an x key")]
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The synthetic window with no headers above it, and its blocks directory.
fn window(p: &ChainParams) -> (tempfile::TempDir, Vec<BlockHash>, BlockDir) {
    let (src, hashes) = synthetic_to(p, WINDOW, None);
    let genesis = genesis_block(p).hash();
    let dir = BlockDir::open(&src.path().join("blocks"), p.message_start, genesis, None).unwrap();
    (src, hashes, dir)
}

/// The suffix check's inputs in `out`: the window's log from genesis, `g.jsonl`, as the golden,
/// and the snapshot at [`BASE`], `utxo.dat`, saved by a run to `BASE` against it.
fn suffix_inputs(dir: &BlockDir, p: &ChainParams, out: &Path) -> (PathBuf, PathBuf) {
    run_dir(dir, p, &options(), out, "g").unwrap();
    let golden = out.join("g.jsonl");
    let snapshot = out.join("utxo.dat");
    let mut opts = options();
    opts.golden = Some(golden.clone());
    opts.save_utxo = Some(snapshot.clone());
    let saved = run_to(dir, p, &opts, out, "save", BASE).unwrap();
    assert_eq!(saved.manifest["save_utxo"]["coins"], BASE);
    (golden, snapshot)
}

/// CH3a D165, D170 (DEC-348 (1)): a pruned start at 100 from the snapshot a run to 100 saved
/// logs 101..=110 as the run from genesis does on every key but `muhash`, which it holds at 110
/// only, equal there; its reverse leg to 100 logs the full run's, its MuHash finalized at 100
/// only. `-checkblockindex` is on (regtest's default) with `m_have_pruned` set.
#[test]
fn a_pruned_start_logs_what_a_run_from_genesis_logs() {
    let p = regtest();
    let (_src, hashes, dir) = window(&p);
    let out = tempdir();
    let (golden, snapshot) = suffix_inputs(&dir, &p, out.path());

    // The snapshot is Core's format, its sidecar naming it.
    let side = json(&sidecar_path(&snapshot));
    assert_eq!(side["chain"], "regtest");
    assert_eq!(side["base"]["height"], BASE);
    assert_eq!(side["base"]["hash"], hashes[BASE as usize].to_string());
    assert_eq!(side["coins"], BASE);
    let set = utxo::read(&snapshot, BASE).unwrap();
    assert_eq!((set.base, set.coins.len()), (hashes[BASE as usize], BASE as usize));
    assert_eq!(side["sha256"], set.sha256.as_str());
    assert_eq!(
        (&side["scan"]["txouts"], &side["scan"]["amount"]),
        (&BASE.into(), &(i64::from(BASE) * COIN).into())
    );

    let mut opts = options();
    opts.golden = Some(golden.clone());
    opts.reverse_to = Some(BASE);
    let full = run_dir(&dir, &p, &opts, out.path(), "full").unwrap();
    opts.start_utxo = Some(snapshot.clone());
    let suffix = run_dir(&dir, &p, &opts, out.path(), "suffix").unwrap();
    assert_eq!(suffix.tip, full.tip);
    let start = suffix.start.as_ref().unwrap();
    assert_eq!((start.txouts, start.amount), (u64::from(BASE), i64::from(BASE) * COIN));
    assert_eq!(full.start, None);
    // Every block from the base's child checks its scripts, as in the full run.
    assert_eq!(suffix.forward.runs, [(BASE + 1, WINDOW, Some(true))]);

    let m = &suffix.manifest;
    assert_eq!(
        m["window"],
        serde_json::json!({"from": BASE + 1, "to": WINDOW, "tip": suffix.tip.1.to_hex()})
    );
    assert_eq!(m["check_block_index"], true);
    let s = &m["start"];
    assert_eq!(
        (&s["have_pruned"], &s["index_records"], &s["coins"]),
        (&true.into(), &(BASE + 1).into(), &BASE.into())
    );
    assert_eq!(s["sha256"], side["sha256"]);
    assert_eq!(s["base"]["hash"], side["base"]["hash"]);
    assert_eq!(s["check"]["equal"], true);
    assert_eq!(s["check"]["muhash"], side["scan"]["muhash"]);
    assert_eq!(s["muhash_at"], serde_json::json!([BASE, WINDOW]));
    assert_eq!(m["forward_golden"].as_array().unwrap().len(), 1);
    assert_eq!(
        (&m["forward_golden"][0]["height"], &m["forward_golden"][0]["equal"]),
        (&WINDOW.into(), &true.into())
    );
    assert_eq!(full.manifest["start"], serde_json::Value::Null);
    assert_eq!(full.manifest["forward_golden"], serde_json::Value::Null);
    // The binary that ran, here the test's own, in the manifest and the snapshot's sidecar.
    let exe = std::env::current_exe().unwrap();
    assert_eq!(m["exe"]["path"], exe.to_str().unwrap());
    assert_eq!(m["exe"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(side["run"]["exe"], m["exe"]);

    // The forward log: the full run's from 101, `muhash` at 110 only.
    let report = linear_suffix(
        &out.path().join("suffix.jsonl"),
        &out.path().join("full.jsonl"),
        BASE + 1,
        WINDOW,
    );
    assert_eq!(report.verdict, Verdict::Clean, "{:?} {:?}", report.first(), report.uncovered);
    assert_eq!(report.pairs, u64::from(WINDOW - BASE));
    assert_eq!(report.excluded[&field("muhash")], [1, u64::from(WINDOW - BASE)]);
    let at = |log: &str, h: u32| {
        records(&out.path().join(log))
            .into_iter()
            .find(|l| l.starts_with(&format!(r#"{{"k":"C","h":{h},"#)))
            .unwrap()
    };
    assert_eq!(without_x(&at("suffix.jsonl", WINDOW)), without_x(&at("full.jsonl", WINDOW)));
    assert!(!at("suffix.jsonl", WINDOW - 1).contains(r#""muhash""#));

    // The reverse leg: the full run's records, the MuHash finalized at the base only.
    let reverse = suffix.reverse.as_ref().unwrap();
    let n = u64::from(WINDOW - BASE);
    assert_eq!((reverse.compared, reverse.tally.disconnects_ok), (n, n));
    assert_eq!(reverse, full.reverse.as_ref().unwrap());
    assert_eq!(
        (&m["reverse"]["compared_muhash"], &full.manifest["reverse"]["compared_muhash"]),
        (&1.into(), &n.into())
    );
    let logs = ["suffix", "full"].map(|name| {
        records(&reverse_path(&out.path().join(format!("{name}.jsonl"))))
            .iter()
            .map(|l| without_x(l).to_owned())
            .collect::<Vec<_>>()
    });
    assert_eq!(logs[0], logs[1]);
    eprintln!(
        "synthetic suffix: start check {:?}; V time {} s, full {} s",
        start, m["timings"]["v_time_s"], full.manifest["timings"]["v_time_s"]
    );
}

/// `snapshot`'s copy at `name` in `out` after `edit` changes its base or coins, with a sidecar for
/// it that `side` then edits: what a corrupted artefact looks like with its sidecar regenerated.
fn edited(
    snapshot: &Path,
    out: &Path,
    name: &str,
    edit: impl FnOnce(&mut utxo::Snapshot),
    side: impl FnOnce(&mut serde_json::Value),
) -> PathBuf {
    let mut set = utxo::read(snapshot, BASE).unwrap();
    edit(&mut set);
    let path = out.join(name);
    let file = std::fs::File::create_new(&path).unwrap();
    let sha256 = utxo::write(file, &path, set.base, set.coins).unwrap();
    let mut sidecar = json(&sidecar_path(snapshot));
    sidecar["sha256"] = sha256.into();
    side(&mut sidecar);
    std::fs::write(sidecar_path(&path), serde_json::to_vec(&sidecar).unwrap()).unwrap();
    path
}

/// CH3a D166 (DEC-348 (2)): the snapshot is never trusted on its own. A coin whose height alone
/// changed, its sidecar regenerated, keeps `txouts` and `amount`: the start check's MuHash refuses
/// it before V starts. (Block 102 spends that coin and is still past maturity, so without the
/// check the run would pass: the mutant `MS` skips it.) An unspendable coin the cache drops is
/// refused by the count. Each mismatch between the snapshot, its sidecar, the golden, the blocks
/// directory's chain and the window is refused alone, as is a log whose MuHash at the window's
/// end is not the golden's. A refused run leaves no output.
#[test]
fn a_snapshot_that_differs_from_the_golden_is_refused() {
    let p = regtest();
    let (_src, _, dir) = window(&p);
    let out = tempdir();
    let (golden, snapshot) = suffix_inputs(&dir, &p, out.path());
    let refused = |opts: &Options, to: u32| -> String {
        let e = run_to(&dir, &p, opts, out.path(), "refused", to).unwrap_err();
        for name in ["refused.jsonl", "refused.reverse.jsonl", "refused.manifest.json"] {
            assert!(!out.path().join(name).exists(), "{name}");
        }
        format!("{e:#}")
    };
    let start = |path: &Path| {
        let mut opts = options();
        opts.golden = Some(golden.clone());
        opts.start_utxo = Some(path.to_owned());
        opts
    };

    let moved = edited(
        &snapshot,
        out.path(),
        "moved.dat",
        |set| {
            let (_, coin) = set.coins.iter_mut().find(|(_, c)| c.height() == 1).unwrap();
            *coin = coin.as_ref().with_metadata(2, true).to_coin();
        },
        |_| {},
    );
    let e = refused(&start(&moved), WINDOW);
    assert!(
        e.starts_with(&format!("start check: the snapshot's coins at {BASE} gives muhash ")),
        "{e}"
    );

    // The file is not the sidecar's.
    let other = out.path().join("other.dat");
    std::fs::copy(&moved, &other).unwrap();
    std::fs::copy(sidecar_path(&snapshot), sidecar_path(&other)).unwrap();
    let e = refused(&start(&other), WINDOW);
    assert!(e.contains("other.dat: its sha256 ") && e.contains(" is not the sidecar's "), "{e}");

    // The file's base is not the sidecar's, which golden(100) and the header chain agree with.
    let based = edited(
        &snapshot,
        out.path(),
        "based.dat",
        |set| set.base = BlockHash::from_lendian([0x11; 32]),
        |_| {},
    );
    let e = refused(&start(&based), WINDOW);
    assert!(
        e.contains(&format!("based.dat: its base {} is not the sidecar's ", "11".repeat(32))),
        "{e}"
    );

    // An unspendable coin: Core's loader keeps it (`validation.cpp:6242`) and its hash check then
    // refuses the snapshot (`:6326-6330`). V's cache never holds one (`coins.cpp:72`), so the scan
    // gives the golden's values and the count refuses it.
    let extra = edited(
        &snapshot,
        out.path(),
        "extra.dat",
        |set| {
            let mut k = set.coins[0].0;
            k.0[0] ^= 0xff;
            set.coins.push((k, Coin::new(0, &[0x6a], 1, false)));
        },
        |s| s["coins"] = (BASE + 1).into(),
    );
    let e = refused(&start(&extra), WINDOW);
    assert_eq!(e, format!("start check: the cache holds {BASE} coins, the snapshot {}", BASE + 1));

    type Edit = fn(&mut serde_json::Value);
    let cases: [(&str, Edit, &str); 4] = [
        ("chain", |s| s["chain"] = "main".into(), "a main snapshot, not regtest"),
        ("count", |s| s["coins"] = (BASE + 1).into(), "count.dat: 100 coins, the sidecar says 101"),
        (
            "height",
            |s| s["base"]["height"] = (BASE - 1).into(),
            " at 99 is not golden(99)'s block ",
        ),
        (
            "hash",
            |s| s["base"]["hash"] = "11".repeat(32).into(),
            " at 100 is not golden(100)'s block ",
        ),
    ];
    for (name, side, want) in cases {
        let path = edited(&snapshot, out.path(), &format!("{name}.dat"), |_| {}, side);
        let e = refused(&start(&path), WINDOW);
        assert!(e.contains(want), "{name}: {e}");
    }

    // The window must pass the base, the reverse leg stay above the pruned blocks, and a golden
    // be given.
    let e = refused(&start(&snapshot), BASE);
    assert_eq!(e, "--start-utxo: the window ends at 100, not past the snapshot's base 100");
    let mut opts = start(&snapshot);
    opts.reverse_to = Some(BASE - 1);
    let e = refused(&opts, WINDOW);
    assert_eq!(e, "--reverse-to 99 is below the snapshot's base 100: its blocks are pruned");
    opts.golden = None;
    opts.reverse_to = None;
    let e = refused(&opts, WINDOW);
    assert_eq!(e, "--reverse-to, --start-utxo and --save-utxo need a golden");

    // The log's MuHash at the window's end, the seed plus the suffix's changes, against a golden
    // whose muhash there alone differs.
    let mut opts = start(&snapshot);
    opts.golden =
        Some(corrupt(&golden, out.path(), WINDOW, "muhash", &format!("\"{}\"", "00".repeat(32))));
    let e = refused(&opts, WINDOW);
    assert!(e.starts_with("forward log: at 110, muhash is "), "{e}");
    assert!(e.ends_with(&format!(", golden(110) has {}", "00".repeat(32))), "{e}");

    // A blocks directory whose chain forks below the base: golden(100) is the sidecar's block,
    // the header chain's is not.
    let (fork, _) = synthetic_to(&p, WINDOW, Some((50, Odd::Fork)));
    let genesis = genesis_block(&p).hash();
    let forked =
        BlockDir::open(&fork.path().join("blocks"), p.message_start, genesis, None).unwrap();
    let e = format!(
        "{:#}",
        run_to(&forked, &p, &start(&snapshot), out.path(), "refused", WINDOW).unwrap_err()
    );
    assert!(e.contains(" at 100 is not the header chain's block "), "{e}");
}

/// [`ReadOnly`]'s watchdog, `sh -c` with the tree as `$1`: it ignores the signals that stop a
/// systemd unit or a terminal's job, says so, waits for the end of its stdin, then restores.
const WATCHDOG: &str =
    r#"trap '' HUP INT TERM; echo ready; while read -r _; do :; done; exec chmod -R u+w -- "$1""#;

/// Makes a directory and everything under it read-only (`chmod -R a-w`) until dropped, and
/// writable again (`chmod -R u+w`) however the test binary alone ends (BL-429). A watchdog
/// restores it: a shell in its own process group, which the gate's `killpg` of a timed-out binary
/// misses, ignoring the `SIGTERM` a unit's stop sends every process, and reading a pipe whose
/// write end only this process holds. The pipe closes at the drop, or when the process dies
/// without one (an abort, a kill), and the shell then restores the tree. Only a `SIGKILL` of every
/// process at once defeats it; gate-fast wipes its slot's TMPDIR after that itself (SB1 D54).
struct ReadOnly<'a> {
    dir: &'a Path,
    watchdog: Child,
}

impl<'a> ReadOnly<'a> {
    fn new(dir: &'a Path) -> ReadOnly<'a> {
        let mut watchdog = Command::new("sh")
            .args(["-c", WATCHDOG, "sh"])
            .arg(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("the read-only tree's watchdog");
        let mut ready = String::new();
        BufReader::new(watchdog.stdout.take().unwrap()).read_line(&mut ready).unwrap();
        assert_eq!(ready, "ready\n", "the read-only tree's watchdog");
        // Armed first, so that a chmod which fails part way is restored too.
        let read_only = ReadOnly {
            dir,
            watchdog,
        };
        let status = Command::new("chmod").args(["-R", "a-w", "--"]).arg(dir).status();
        assert!(status.unwrap().success(), "chmod -R a-w {}", dir.display());
        read_only
    }
}

impl Drop for ReadOnly<'_> {
    fn drop(&mut self) {
        // `wait` closes the pipe first.
        let status = self.watchdog.wait();
        if !std::thread::panicking() {
            assert!(status.unwrap().success(), "chmod -R u+w {}", self.dir.display());
        }
    }
}

/// [`read_only_child`]'s tree, from the parent (BL-429).
const READ_ONLY_CHILD: &str = "CH3A_DIGEST_L2_READ_ONLY_CHILD";

/// BL-429's child: makes the parent's tree read-only, names its watchdog, and holds the tree until
/// killed or until the parent closes its stdin.
#[test]
fn read_only_child() {
    let Some(dir) = std::env::var_os(READ_ONLY_CHILD) else {
        return;
    };
    let read_only = ReadOnly::new(Path::new(&dir));
    println!("CH3A-READ-ONLY {}", read_only.watchdog.id());
    std::io::stdin().read_to_end(&mut Vec::new()).unwrap();
}

/// Ends [`read_only_child`] however its parent test ends, and leaves the tree writable for the
/// temporary directory's removal.
struct Reaped<'a>(Child, &'a Path);

impl Drop for Reaped<'_> {
    fn drop(&mut self) {
        let _ = self.0.wait();
        let _ = Command::new("chmod").args(["-R", "u+w", "--"]).arg(self.1).status();
    }
}

/// BL-429: a binary killed as the gate kills a timed-out one (`SIGKILL` to its process group, so
/// no drop runs) still leaves its read-only tree writable, its watchdog sent the `SIGTERM` a
/// unit's stop sends every process first: the watchdog restores it.
#[test]
fn a_killed_binary_leaves_its_read_only_tree_writable() {
    let dir = tempdir();
    let blocks = dir.path().join("blocks");
    std::fs::create_dir(&blocks).unwrap();
    let file = blocks.join("blk00000.dat");
    std::fs::write(&file, b"").unwrap();
    let writable = || {
        std::fs::OpenOptions::new().append(true).open(&file).is_ok()
            && std::fs::File::create(blocks.join("probe")).is_ok()
    };
    let spawned = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "read_only_child", "--nocapture", "--test-threads=1"])
        .env(READ_ONLY_CHILD, &blocks)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let mut child = Reaped(spawned, &blocks);
    let stdout = BufReader::new(child.0.stdout.take().unwrap());
    // The harness prints the test's name on the same line.
    let watchdog = stdout
        .lines()
        .map(Result::unwrap)
        .find_map(|l| Some(l.split_once("CH3A-READ-ONLY ")?.1.to_owned()))
        .expect("the child never said the tree is read-only");
    assert!(!writable(), "the tree is writable");
    let kill = |signal: &str, group: String| {
        let status = Command::new("kill").args([signal, "--", &group]).status();
        assert!(status.unwrap().success(), "kill {signal} -- {group}");
    };
    kill("-TERM", format!("-{watchdog}"));
    kill("-KILL", format!("-{}", child.0.id()));
    assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    // The watchdog is not this process's child: wait for its restore.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !writable() {
        assert!(std::time::Instant::now() < deadline, "the tree is still read-only");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// CH3a D168 (DEC-348 (4)): `digest l2` only reads its blocks directory. With it made read-only
/// (`chmod -R a-w`), the run from genesis, the save and the pruned start with its reverse leg
/// all pass.
#[test]
fn every_run_passes_on_a_read_only_blocks_directory() {
    let p = regtest();
    let (src, _) = synthetic_to(&p, WINDOW, None);
    let blocks = src.path().join("blocks");
    let _read_only = ReadOnly::new(&blocks);
    assert!(std::fs::File::create(blocks.join("probe")).is_err(), "the directory is writable");
    assert!(
        std::fs::OpenOptions::new().append(true).open(blocks.join("blk00000.dat")).is_err(),
        "the block file is writable"
    );
    let genesis = genesis_block(&p).hash();
    let dir = BlockDir::open(&blocks, p.message_start, genesis, None).unwrap();
    let out = tempdir();
    let (golden, snapshot) = suffix_inputs(&dir, &p, out.path());
    let mut opts = options();
    opts.golden = Some(golden);
    opts.reverse_to = Some(BASE);
    opts.start_utxo = Some(snapshot);
    let r = run_dir(&dir, &p, &opts, out.path(), "suffix").unwrap();
    assert_eq!(r.tip.0, WINDOW);
}

/// CH3a D165: a save run that fails after its save, here in the reverse leg against a golden whose
/// `txouts` at 105 alone is wrong, leaves neither the snapshot nor its sidecar, as it leaves no
/// log or manifest. An output that exists is refused before V starts and kept as it was. Without
/// a reverse leg its log is not an output: a file there is neither refused nor removed.
#[test]
fn a_run_writes_only_new_files_and_a_failed_save_leaves_none() {
    let p = regtest();
    let (_src, _, dir) = window(&p);
    let out = tempdir();
    let (golden, _) = suffix_inputs(&dir, &p, out.path());
    let mut opts = options();
    opts.save_utxo = Some(out.path().join("s.dat"));
    opts.reverse_to = Some(WINDOW - 5);
    opts.golden = Some(corrupt(&golden, out.path(), WINDOW - 5, "txouts", "1"));
    let e = format!("{:#}", run_to(&dir, &p, &opts, out.path(), "failed", WINDOW).unwrap_err());
    assert!(e.contains("reverse leg: at 105, txouts is "), "{e}");
    for name in
        ["s.dat", "s.dat.json", "failed.jsonl", "failed.reverse.jsonl", "failed.manifest.json"]
    {
        assert!(!out.path().join(name).exists(), "{name}");
    }

    for name in ["kept.jsonl", "kept.reverse.jsonl", "kept.manifest.json", "s.dat", "s.dat.json"] {
        let path = out.path().join(name);
        std::fs::write(&path, "kept").unwrap();
        let e = format!("{:#}", run_to(&dir, &p, &opts, out.path(), "kept", WINDOW).unwrap_err());
        assert_eq!(e, format!("{} exists: the run writes only new files", path.display()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept", "{name}");
        std::fs::remove_file(&path).unwrap();
    }

    let other = out.path().join("other.reverse.jsonl");
    std::fs::write(&other, "kept").unwrap();
    opts.reverse_to = None;
    opts.golden = Some(corrupt(&golden, out.path(), WINDOW, "txouts", "1"));
    let e = format!("{:#}", run_to(&dir, &p, &opts, out.path(), "other", WINDOW).unwrap_err());
    assert!(e.contains("save check: the coins scan at 110 gives txouts "), "{e}");
    assert_eq!(std::fs::read_to_string(&other).unwrap(), "kept");
    for name in ["s.dat", "s.dat.json", "other.jsonl", "other.manifest.json"] {
        assert!(!out.path().join(name).exists(), "{name}");
    }
}

/// `digest l2 --save-utxo` and `--start-utxo` (CH3a D165): exit 0, the start check and the
/// snapshot named on stdout; a pruned start needs a golden.
#[test]
fn the_binary_saves_a_snapshot_and_starts_from_it() {
    let p = regtest();
    let (src, _, dir) = window(&p);
    let out = tempdir();
    run_dir(&dir, &p, &options(), out.path(), "g").unwrap();
    let golden = out.path().join("g.jsonl");
    let snapshot = out.path().join("utxo.dat");
    let run = |name: &str, to: u32, args: &[&std::ffi::OsStr]| {
        Command::new(env!("CARGO_BIN_EXE_dash-conformance"))
            .args(["digest", "l2", "--chain", "regtest", "--blocks-dir"])
            .arg(src.path().join("blocks"))
            .args(["--to", &to.to_string(), "--out"])
            .arg(out.path().join(format!("{name}.jsonl")))
            .args(args)
            .output()
            .unwrap()
    };
    let o = run(
        "save",
        BASE,
        &["--golden".as_ref(), golden.as_os_str(), "--save-utxo".as_ref(), snapshot.as_os_str()],
    );
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8(o.stdout).unwrap();
    assert!(stdout.starts_with("l2: tip 100 "), "{stdout}");
    assert!(stdout.contains(&format!("; saved 100 coins to {};", snapshot.display())), "{stdout}");

    let o = run("suffix", WINDOW, &["--start-utxo".as_ref(), snapshot.as_os_str()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--golden <GOLDEN>"));
    let o = run(
        "suffix",
        WINDOW,
        &[
            "--golden".as_ref(),
            golden.as_os_str(),
            "--start-utxo".as_ref(),
            snapshot.as_os_str(),
            "--reverse-to".as_ref(),
            "100".as_ref(),
        ],
    );
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    let stdout = String::from_utf8(o.stdout).unwrap();
    assert!(stdout.starts_with("l2: start check equal (100 coins); tip 110 "), "{stdout}");
    assert!(
        stdout.contains("; reverse to 100: D 10 ok 10, compared 10, end check equal;"),
        "{stdout}"
    );
}

/// CH3a D169: the testnet window's suffix check. A run to 4,999 with assumevalid on saves the
/// snapshot; a pruned start there runs 5,000..=6,999 on (reversed to 6,000) and off, each clean
/// against the golden on every `C.s` key but `muhash` and the evo keys, with `muhash` equal at
/// 6,999 and 6,000. The timings are printed.
#[test]
fn a_testnet_suffix_from_4999_is_clean_against_the_golden() {
    let Some(window) = std::env::var_os("CH3A_TESTNET_WINDOW").map(PathBuf::from) else {
        let _ = writeln!(
            std::io::stderr(),
            "\n*** SKIPPED a_testnet_suffix_from_4999_is_clean_against_the_golden: \
             CH3A_TESTNET_WINDOW is not set. Set it to the pinned window directory (CH3a D146) \
             and run with --release. ***\n"
        );
        return;
    };
    const SUFFIX_BASE: u32 = 4_999;
    const SUFFIX_L: u32 = 6_000;
    let p =
        Arc::new(ChainParamsBuilder::new(NetworkKind::Testnet, Release::V24_0).build().unwrap());
    let golden = window.join("golden-0-6999.jsonl.zst");
    let genesis = genesis_block(&p).hash();
    let dir = BlockDir::open(&window.join("blocks"), p.message_start, genesis, None).unwrap();
    let out = tempdir();
    let run = |opts: &Options, name: &str, to: u32| {
        let source = Source::Dir {
            dir: &dir,
            params: p.clone(),
            chain: Chain::Test,
            to,
        };
        let log = out.path().join(format!("{name}.jsonl"));
        let manifest = out.path().join(format!("{name}.manifest.json"));
        let r = l2::run(source, opts, &log, &manifest, &out.path().join(format!("{name}-work")));
        (r.unwrap_or_else(|e| panic!("{name}: {e:#}")), log)
    };

    let snapshot = out.path().join("utxo-4999.dat");
    let mut opts = options();
    opts.par = MAX_PAR;
    opts.assume_valid = true;
    opts.golden = Some(golden.clone());
    opts.save_utxo = Some(snapshot.clone());
    let (save, _) = run(&opts, "save", SUFFIX_BASE);
    opts.save_utxo = None;
    opts.start_utxo = Some(snapshot.clone());
    opts.reverse_to = Some(SUFFIX_L);
    let (on, on_log) = run(&opts, "on", TESTNET_TIP);
    opts.assume_valid = false;
    opts.reverse_to = None;
    let (off, off_log) = run(&opts, "off", TESTNET_TIP);
    assert_eq!(on.forward.runs, [(SUFFIX_BASE + 1, TESTNET_TIP, Some(false))]);
    assert_eq!(off.forward.runs, [(SUFFIX_BASE + 1, TESTNET_TIP, Some(true))]);
    let pairs = u64::from(TESTNET_TIP - SUFFIX_BASE);
    // The forward run passes the reverse leg's end: the log finalizes its MuHash there too.
    for (log, what, finalized) in [(&on_log, "on", 2), (&off_log, "off", 1)] {
        let report = linear_suffix(log, &golden, SUFFIX_BASE + 1, TESTNET_TIP);
        assert_eq!(
            report.verdict,
            Verdict::Clean,
            "{what}: {:?} {:?}",
            report.first(),
            report.uncovered
        );
        assert_eq!(report.pairs, pairs, "{what}");
        assert_eq!(report.excluded[&field("muhash")], [finalized, pairs], "{what}");
    }
    let reverse = on.reverse.as_ref().unwrap();
    let n = u64::from(TESTNET_TIP - SUFFIX_L);
    assert_eq!((reverse.compared, reverse.tally.disconnects_ok), (n, n));
    assert_eq!(on.manifest["reverse"]["compared_muhash"], 1);
    let checked: Vec<_> = on.manifest["forward_golden"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["height"].clone())
        .collect();
    assert_eq!(checked, [SUFFIX_L, TESTNET_TIP].map(serde_json::Value::from));
    for (name, r) in [("save", &save.manifest), ("on", &on.manifest), ("off", &off.manifest)] {
        eprintln!(
            "testnet suffix {name}: V time {} s, total {} s, start {}, saved {}, preload {}, \
             reverse {}",
            r["timings"]["v_time_s"],
            r["timings"]["total_wall_s"],
            r["start"],
            r["save_utxo"],
            r["preload"],
            r["reverse"]
        );
    }
}
