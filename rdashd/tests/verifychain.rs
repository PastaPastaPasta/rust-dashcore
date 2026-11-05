//! VerifyDB against dashd v24 on non-evo regtest (CH5-1a): `verifychain` at levels 0–4 over
//! depths 1, 3, 6, 0 (the whole chain) and 200 (above the height), and the startup check, on a
//! healthy chain and on the two Core-produced fixtures (`tests/data/verifychain/PROVENANCE.md`):
//!
//! - the L4-only fixture, block 118's undo record restoring block 5's coinbase with another
//!   script: levels 0–3 pass down to block 6, level 4's reconnect of block 118 fails its script
//!   check, and a walk that reaches block 5 finds the output mismatch at level 3;
//! - the L3 fixture, block 118's coinbase output worth one duff more in the coins database:
//!   its disconnect is UNCLEAN, level 3 fails and level 4 never runs.
//!
//! Both nodes replay the same 120 blocks dashd mined, take the same damage, and must answer every
//! call with the same result and VerifyDB lines, and every start with the same exit code and init
//! text. The coins share at the default `-dbcache` is above either node's level-3 figure on this
//! chain, rdashd's by 140 MiB (its shards' ceilings, DEC-1007): every level-3 check runs on both.
//! Just below rdashd's threshold it skips level 3 from the first block where dashd verifies.
//! dashd is the tests' rc.5 pin (`common::CORE_VERSION`), where the fixtures were proved (CH5 D6).
//! The load-time `ResetBlockFailureFlags(nullptr)` after the startup check is compared clause by
//! clause at levels 2 and 3 (DEC-992), with `getchaintips` and the reset's log line.
//! No case relays a transaction or runs ATMP after `verifychain` (DEC-981b). It cannot be
//! skipped: with `SKIP_DASHD_TESTS` set it fails (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::mine::{GENESIS_TIME, Mined, mine, parse_hex, txid};
use common::*;
use dash_consensus::coins::Coin;
use dash_consensus::merkle::compute_merkle_root;
use dashd_store::{Commit, KvBatch as _, KvEngine as _, KvRead as _, RocksConfig, RocksEngine};

const DATA: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/verifychain");

/// The fixture chain's tip and block 118, in RPC (reversed) hex.
const TIP: &str = "04f15abfbccb5e08cbf0cbd3963a260c0e7ca0bd7933fddb187b4eac91523aef";
const B118: &str = "7db3cf2519c990280106291a6d975f0522946c3e64215e824f8e76f322c01b22";
/// The L3 fixture's coin: block 118's coinbase, output 0.
const CB118: &str = "8f7a8c9738de06a7f4d4c86202a09c952c8662f51db91917369c12282f8a5d23";

/// Both nodes' clock, a day past the fixture tip (1793418272, CH5 D9): in IBD whenever the tests run, as
/// a run without it is from 6 h after the fixture was made, so no step depends on the date.
const MOCKTIME: &str = "-mocktime=1793504672";
const MOCKTIME_SECS: i64 = 1_793_504_672;

const REINDEX_QUESTION: &str = "Please restart with -reindex or -reindex-chainstate to recover.";

/// One node under test: its binary, home and datadir, and the RPC node while it runs.
struct Side {
    binary: PathBuf,
    home: tempfile::TempDir,
    datadir: PathBuf,
    extra: Vec<&'static str>,
    rpc: Option<RpcNode>,
}

impl Side {
    fn new(binary: PathBuf, extra: &[&'static str]) -> Side {
        let (home, datadir) = home_and_datadir();
        Side {
            binary,
            home,
            datadir,
            extra: extra.to_vec(),
            rpc: None,
        }
    }

    fn is_dashd(&self) -> bool {
        self.binary != Path::new(RDASHD)
    }

    fn args<'a>(&'a self, more: &[&'a str]) -> Vec<&'a str> {
        [&self.extra[..], more].concat()
    }

    /// A start, then a wait for its late lines ([`DASHD_LATE_LINES`], [`RDASHD_LATE_LINES`]): a
    /// later `verifychain`'s window cannot get one.
    fn start(&mut self, more: &[&str]) {
        let args = self.args(more);
        let before = self.log_len();
        self.rpc = Some(RpcNode::start_on(&self.binary, self.home.path(), &self.datadir, &args));
        self.await_late_lines(before);
    }

    fn await_late_lines(&self, before: usize) {
        let want = if self.is_dashd() {
            DASHD_LATE_LINES
        } else {
            RDASHD_LATE_LINES
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let log = read(&debug_log(&self.datadir));
            let new = log.get(before..).unwrap_or_default();
            let missing: Vec<_> =
                want.iter().filter(|w| !new.lines().any(|l| unstamped(l) == **w)).collect();
            if missing.is_empty() {
                return;
            }
            if Instant::now() >= deadline {
                let tail: Vec<_> = new.lines().rev().take(40).collect();
                let tail: Vec<_> = tail.into_iter().rev().collect();
                panic!(
                    "{}: startup lines not logged within 60 s: {missing:#?}\nlog tail:\n{}",
                    self.binary.display(),
                    tail.join("\n")
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn rpc(&self) -> &RpcNode {
        self.rpc.as_ref().expect("running")
    }

    fn stop(&mut self) {
        let rpc = self.rpc.take().expect("running");
        assert!(rpc.call("stop", "[]").result().contains("stopping"));
        let out = rpc.node.wait();
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    }

    /// A start that init refuses: its exit code and stderr.
    fn refused(&self, more: &[&str]) -> (Option<i32>, String) {
        let args = self.args(&[more, &["-printtoconsole=0"]].concat());
        let out = output(regtest_on(&self.binary, self.home.path(), &self.datadir, &args));
        (out.status.code(), stderr(&out))
    }

    /// The fixture chain by `submitblock`, each accepted.
    fn replay(&self) {
        for block in read(&Path::new(DATA).join("blocks.hex")).lines() {
            let reply = self.rpc().call("submitblock", &format!(r#"["{block}"]"#));
            assert_eq!(reply.result(), "null", "{}", self.binary.display());
        }
        assert_eq!(self.rpc().call("getbestblockhash", "[]").result(), format!(r#""{TIP}""#));
    }

    fn log_len(&self) -> usize {
        read(&debug_log(&self.datadir)).len()
    }

    /// `verifychain level depth`: the result, and VerifyDB's lines from the log, their times cut.
    fn verifychain(&self, level: i32, depth: i32) -> (String, Vec<String>) {
        let before = self.log_len();
        let result =
            self.rpc().call("verifychain", &format!("[{level}, {depth}]")).result().to_owned();
        let log = read(&debug_log(&self.datadir));
        (result, verifydb_lines(&log[before..]))
    }

    /// Block 118's undo record and checksum, as dashd wrote them, replaced by the L4-only
    /// fixture's: the record is found by its bytes, so its position in either node's file does
    /// not matter.
    fn damage_undo(&self) {
        let path = net(&self.datadir).join("blocks").join("rev00000.dat");
        let mut rev = std::fs::read(&path).unwrap();
        let (was, now) = (undo_hex("undo118.hex"), undo_hex("undo118-l4.hex"));
        let at: Vec<_> = rev.windows(was.len()).enumerate().filter(|(_, w)| *w == was).collect();
        assert_eq!(at.len(), 1, "block 118's undo record, once: {}", self.binary.display());
        let at = at[0].0;
        rev[at..at + now.len()].copy_from_slice(&now);
        std::fs::write(&path, rev).unwrap();
    }

    /// The L3 fixture's coin damage. dashd: the coins database Core's own code damaged
    /// (`l3-chainstate`, the same chain's coins at the same tip). rdashd: the same coin rewritten
    /// in its coins column family through Core's coin serialization (CH5 D7).
    fn damage_coin(&self) {
        let chainstate = net(&self.datadir).join("chainstate");
        if self.is_dashd() {
            std::fs::remove_dir_all(&chainstate).unwrap();
            std::fs::create_dir(&chainstate).unwrap();
            for f in std::fs::read_dir(Path::new(DATA).join("l3-chainstate")).unwrap() {
                let f = f.unwrap();
                std::fs::copy(f.path(), chainstate.join(f.file_name())).unwrap();
            }
            return;
        }
        use dashd_chainstate::coins::OutPointKey;
        use dashd_chainstate::store::CHAINSTATE_SCHEMA;
        let (db, _) =
            RocksEngine::open(&chainstate, &CHAINSTATE_SCHEMA, &RocksConfig::default()).unwrap();
        let cf = CHAINSTATE_SCHEMA.cf("coins").unwrap();
        let mut txid = parse_hex(CB118);
        txid.reverse();
        let mut outpoint = [0; 36];
        outpoint[..32].copy_from_slice(&txid);
        let mut key = [0; 37];
        let n = OutPointKey(outpoint).db_key(&mut key);
        let key = &key[..n];
        let bytes = db.get(cf, key).unwrap().expect("block 118's coinbase is unspent");
        let coin = Coin::decode(&mut bytes.as_slice()).unwrap();
        assert_eq!(coin.value(), 50_000_000_000, "the fixture's coin");
        let damaged = Coin::new(
            coin.value() + 1,
            coin.script_pubkey().as_slice(),
            coin.height(),
            coin.is_coinbase(),
        );
        let mut batch = db.batch();
        batch.put(cf, key, &damaged.serialize());
        db.write(batch, Commit::Durable).unwrap();
    }
}

fn undo_hex(name: &str) -> Vec<u8> {
    parse_hex(read(&Path::new(DATA).join(name)).trim())
}

/// A log line's text: its time cut, and the ` (mocktime: N)` beside it (`logging.cpp:401-404`).
fn unstamped(line: &str) -> &str {
    let text = line.split_once(' ').map_or(line, |(_, text)| text);
    text.strip_prefix("(mocktime: ").and_then(|t| t.split_once(") ")).map_or(text, |(_, t)| t)
}

/// dashd's lines a start logs once, unstamped, from threads init starts, in their own time: one
/// may follow "Done loading" and land in the first `verifychain`'s window (BL-669). Each thread's
/// start line, the exit line (its last) of one that returns, and opencon's seed line. Not started
/// here: dnsseed (`-dnsseed=0`), mncon (no wallet, masternode or `-watchquorums`), i2paccept (no
/// `-i2psam`), mapport, torcontrol (`-listen=0`), rawsender (no `-statshost`), masternode-only.
// Core: util/thread.cpp:16-27 TraceThread; init.cpp:1576, :2455, :2625-2626, :1037-1049;
// index/base.cpp:415; llmq/quorumsman.cpp:46; chainlock/handler.cpp:52;
// instantsend/net_instantsend.cpp:298; llmq/net_signing.cpp:159; net.cpp:2969-2971, :4084-4119
const DASHD_LATE_LINES: &[&str] = &[
    "scheduler thread start",
    "q-cache thread start",
    "cl-schdlr thread start",
    "txindex thread start",
    "txindex thread exit",
    "isman thread start",
    "recsigs thread start",
    "loadblk thread start",
    "loadblk thread exit",
    "net thread start",
    "addcon thread start",
    "opencon thread start",
    "Fixed seeds are disabled",
    "msghand thread start",
];

/// [`DASHD_LATE_LINES`] for rdashd: it starts every dial loop's thread, and its dnsseed thread
/// logs "DNS seeding disabled", where Core's `Start` logs it before RPC.
// rdashd: dashd-node/src/host.rs:27-30, node.rs:240-241, :436-437, chain.rs:373;
// dashd-net/src/driver.rs:136-138, connman.rs:156; dash-p2p/src/dial.rs:531-533, :932-934
const RDASHD_LATE_LINES: &[&str] = &[
    "scheduler thread start",
    "cl-schdlr thread start",
    "loadblk thread start",
    "loadblk thread exit",
    "[net:info] dnsseed thread start",
    "[net:info] DNS seeding disabled",
    "[net:info] dnsseed thread exit",
    "[net:info] addcon thread start",
    "[net:info] opencon thread start",
    "[net:info] Fixed seeds are disabled",
];

/// The threads either node starts before its startup VerifyDB: a late start line lands in that
/// check's window, which `Side::start`'s callers read and no barrier can guard. VerifyDB and the
/// disconnects and connects of its walk start no thread and log no `<name> thread start`, so
/// such a line in its window is never its own.
// Core: init.cpp:1576 (scheduler), :2098 LoadChainstate (node/chainstate.cpp:159,
// llmq/quorumsman.cpp:46: q-cache) before :2106 VerifyLoadedChainstate; validation.cpp:5065-5179
// rdashd: dashd-node/src/node.rs:240-241 (scheduler), steps.rs:526;
// dashd-chainstate/src/chain/verify.rs:257-555
const STARTED_BEFORE_VERIFYDB: &[&str] = &["scheduler", "q-cache"];

fn started_before_verifydb(line: &str) -> bool {
    line.strip_suffix(" thread start").is_some_and(|name| STARTED_BEFORE_VERIFYDB.contains(&name))
}

/// VerifyDB's lines in a stretch of debug log, their times cut: from its first line through its
/// last, without the bench lines either node may log around a connect or the start lines of
/// [`STARTED_BEFORE_VERIFYDB`].
fn verifydb_lines(log: &str) -> Vec<String> {
    let lines: Vec<String> = log
        .lines()
        .map(|l| unstamped(l).to_owned())
        .filter(|l| !l.starts_with("[bench]") && !started_before_verifydb(l))
        .collect();
    let first = lines.iter().position(|l| l.starts_with("Verifying last"));
    let last = lines.iter().rposition(|l| {
        l.starts_with("Verification: No coin database")
            || l.contains("Verification error:")
            || l.starts_with("Skipped verification of level >=3")
    });
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].to_vec(),
        _ => Vec::new(),
    }
}

const LEVELS: [i32; 5] = [0, 1, 2, 3, 4];
const DEPTHS: [i32; 5] = [1, 3, 6, 0, 200];

/// Every level and depth on both nodes: equal answers and lines; the answers, for the record.
fn matrix(nodes: &[Side; 2], expected: impl Fn(i32, i32) -> bool) -> Vec<Vec<String>> {
    let mut all = Vec::new();
    for level in LEVELS {
        for depth in DEPTHS {
            let [ours, core] = nodes.each_ref().map(|n| n.verifychain(level, depth));
            assert_eq!(ours, core, "verifychain {level} {depth}");
            let want = if expected(level, depth) {
                "true"
            } else {
                "false"
            };
            assert_eq!(ours.0, want, "verifychain {level} {depth}: {:#?}", ours.1);
            all.push(ours.1);
        }
    }
    all
}

fn pair() -> [Side; 2] {
    // No seed threads (`-dnsseed=0 -fixedseeds=0`): their log lines must not land inside the VerifyDB window.
    [
        Side::new(PathBuf::from(RDASHD), &[MOCKTIME, "-dnsseed=0", "-fixedseeds=0"]),
        Side::new(require_dashd(), &["-disablewallet", MOCKTIME, "-dnsseed=0", "-fixedseeds=0"]),
    ]
}

/// `init` refusals on both nodes: equal, and Core's corrupted-database text.
fn both_refuse(nodes: &[Side; 2], args: &[&str]) {
    let [ours, core] = nodes.each_ref().map(|n| n.refused(args));
    assert_eq!(ours, core, "{args:?}");
    let text = format!(": Corrupted block database detected.\n{REINDEX_QUESTION}\n");
    assert_eq!(ours, (Some(1), text), "{args:?}");
}

/// The healthy chain, then the L4-only fixture (CH5-1a acceptance: DIFF, startup).
#[test]
fn verifychain_matches_dashd_on_the_healthy_chain_and_the_level_4_fixture() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&["-debug=reindex"]);
        n.replay();
    }
    let lines = matrix(&nodes, |_, _| true);
    let no_skip = "Skipped verification of level >=3";
    assert!(lines.iter().flatten().all(|l| !l.starts_with(no_skip)), "every level-3 check ran");
    assert!(lines.iter().flatten().any(|l| l == "Verifying last 120 blocks at level 4"));
    // `-debug=reindex` off: the same lines.
    for n in &mut nodes {
        n.stop();
        n.start(&[]);
    }
    assert_eq!(matrix(&nodes, |_, _| true), lines);

    for n in &mut nodes {
        n.stop();
        n.damage_undo();
    }
    // The default checks (level 3, six blocks) pass; level 4 over them, or a walk to block 5,
    // does not.
    for n in &mut nodes {
        n.start(&[]);
    }
    let lines = matrix(&nodes, |level, depth| match depth {
        1 => true,
        3 | 6 => level < 4,
        _ => level < 3,
    });
    let unconnectable = format!(
        "[error] Verification error: found unconnectable block at 118, hash={B118} \
         (block-validation-failed)"
    );
    assert!(lines.iter().flatten().any(|l| *l == unconnectable), "{lines:#?}");
    let unclean = "[error] Verification error: coin database inconsistencies found (last 116 \
                   blocks, 4 good transactions before that)";
    assert!(lines.iter().flatten().any(|l| l == unclean), "{lines:#?}");
    for n in &mut nodes {
        n.stop();
    }
    both_refuse(&nodes, &["-checklevel=4", "-checkblocks=6"]);
    both_refuse(&nodes, &["-checklevel=4", "-checkblocks=0"]);
}

/// The L3 fixture: an UNCLEAN disconnect at block 118 fails level 3 from depth 3 on, and level 4
/// never runs (`validation.cpp:5141-5143`).
#[test]
fn verifychain_matches_dashd_on_the_level_3_fixture() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
        n.replay();
        n.stop();
        n.damage_coin();
    }
    // The default startup check would find it; level 2 starts.
    for n in &mut nodes {
        n.start(&["-checklevel=2"]);
    }
    let lines = matrix(&nodes, |level, depth| level < 3 || matches!(depth, 1 | 2));
    assert!(lines.iter().flatten().all(|l| !l.contains("unconnectable")));
    for n_good in [0, 12, 138] {
        let want = format!(
            "[error] Verification error: coin database inconsistencies found (last 3 blocks, \
             {n_good} good transactions before that)"
        );
        assert!(lines.iter().flatten().any(|l| *l == want), "{want}: {lines:#?}");
    }
    for n in &mut nodes {
        n.stop();
    }
    both_refuse(&nodes, &[]);
    both_refuse(&nodes, &["-checklevel=4", "-checkblocks=0"]);
}

/// A coinbase for `height` paying nothing to `OP_TRUE` (BIP34's push of the height).
fn coinbase_at(height: u32) -> Vec<u8> {
    let mut push = height.to_le_bytes().to_vec();
    while push.len() > 1 && push[push.len() - 1] == 0 && push[push.len() - 2] & 0x80 == 0 {
        push.pop();
    }
    let script: Vec<u8> = match height {
        1..=16 => vec![0x50 + height as u8, 0],
        _ => [&[push.len() as u8][..], &push].concat(),
    };
    let mut tx = 1i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend([0; 32]);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(script.len() as u8);
    tx.extend(&script);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(1);
    tx.extend(0i64.to_le_bytes());
    tx.extend([1, 0x51]);
    tx.extend(0u32.to_le_bytes());
    tx
}

/// Outputs in each fan-out transaction: under DIP0001's 100 kB before it activates.
const FAN_OUT: u16 = 9_500;

/// A transaction spending `prev`'s output 0 (`OP_TRUE`, an empty scriptSig) into [`FAN_OUT`]
/// zero-value `OP_TRUE` outputs.
fn fan_out(prev: &[u8; 32]) -> Vec<u8> {
    let mut tx = 1i32.to_le_bytes().to_vec();
    tx.push(1);
    tx.extend(prev);
    tx.extend(0u32.to_le_bytes());
    tx.push(0);
    tx.extend(u32::MAX.to_le_bytes());
    tx.push(0xfd);
    tx.extend(FAN_OUT.to_le_bytes());
    for _ in 0..FAN_OUT {
        tx.extend(0i64.to_le_bytes());
        tx.extend([1, 0x51]);
    }
    tx.extend(0u32.to_le_bytes());
    tx
}

/// Coinbase-only blocks to 145, then five blocks of nine fan-outs each, spending the first 45
/// coinbases: 427,500 coins, tens of MiB in either node's cache.
fn wide_chain(genesis: &[u8; 32]) -> Vec<Mined> {
    let mut blocks: Vec<Mined> = Vec::new();
    let mut coinbases = Vec::new();
    for height in 1..=150u32 {
        let prev = blocks.last().map_or(genesis, |b| &b.hash);
        let cb = coinbase_at(height);
        coinbases.push(txid(&cb));
        let mut txs = vec![cb];
        if height > 145 {
            let first = 9 * (height - 146) as usize;
            txs.extend(coinbases[first..first + 9].iter().map(fan_out));
        }
        let root = compute_merkle_root(txs.iter().map(|t| txid(t)).collect()).0;
        blocks.push(mine(prev, GENESIS_TIME + 60 * height, &txs, Some(root), true));
    }
    blocks
}

/// Each `UpdateTip`'s height, `cache=` MiB and coin count in a debug log.
fn cache_lines(log: &str) -> Vec<(u32, f64, u64)> {
    log.lines()
        .filter(|l| l.contains("UpdateTip: new best="))
        .map(|l| {
            let field = |name: &str| {
                let at = l.find(name).unwrap() + name.len();
                l[at..].split([' ', 'M', '(']).next().unwrap().to_owned()
            };
            // Core's `(%utxo)` prints `(<n>txo)`; rdashd's `(<n>utxo)` (CH5 D11).
            let coins = l[l.find("MiB(").unwrap() + 4..].split("txo)").next().unwrap();
            let coins = coins.trim_end_matches('u');
            (
                field("height=").parse().unwrap(),
                field("cache=").parse().unwrap(),
                coins.parse().unwrap(),
            )
        })
        .collect()
}

/// DEC-981 conditions 1–3 on regtest, the same wide chain into both nodes. At the default
/// `-dbcache` every level-3 check runs on both nodes, at startup and through `verifychain 3 0`.
/// Over a small coins share each walk skips level 3: the same results on both, a start with the
/// default checks, and Core's "Insufficient dbcache" with explicit ones; above rdashd's DEC-1007
/// threshold, the same lines too. The two nodes' figures are compared by DEC-1005's crossing
/// bands (the conformance replay's usage log), not here. No case relays or runs ATMP (DEC-981b).
#[test]
fn verifychain_level_3_budget_matches_dashd_outside_the_band() {
    let mut nodes = pair();
    nodes[0].start(&[]);
    let genesis = genesis_of(&nodes[0]);
    let blocks = wide_chain(&genesis);
    for (i, n) in nodes.iter_mut().enumerate() {
        if i == 1 {
            n.start(&[]);
        }
        for b in &blocks {
            assert_eq!(n.rpc().call("submitblock", &format!(r#"["{}"]"#, b.hex)).result(), "null");
        }
    }

    // Defaults: every level-3 check runs, by RPC and at startup.
    let skip = MEMORY_SKIP;
    let [ours, core] = nodes.each_ref().map(|n| n.verifychain(3, 0));
    assert_eq!(ours, core);
    assert_eq!(ours.0, "true");
    assert!(!ours.1.iter().any(|l| l == skip), "{:#?}", ours.1);
    for n in &mut nodes {
        n.stop();
        let before = n.log_len();
        n.start(&[]);
        let log = read(&debug_log(&n.datadir));
        let lines = verifydb_lines(&log[before..]);
        assert!(lines.contains(&"Verifying last 6 blocks at level 3".to_owned()), "{lines:#?}");
        assert!(!lines.iter().any(|l| l == skip), "{lines:#?}");
    }

    // Over the coins share. At `-dbcache=12` (a share of 4,816,896 B, above rdashd's DEC-1007
    // threshold) both cold figures fit (rdashd's 4,296,480 B, dashd's 0.3 MiB) and both figures
    // after the tip's disconnect are at least 1.7 times the share, so both skip from the second
    // block with the same lines. At `-dbcache=4`, below the threshold, rdashd's cold figure of
    // 3,002,656 B is over the share of 1,605,632 before the first disconnect, dashd's (0.3 MiB)
    // is not: both skip with the same results, rdashd from the first block and dashd after the
    // tip (CH5 D8).
    for (dbcache, same_lines) in [("-dbcache=12", true), ("-dbcache=4", false)] {
        let mut started = Vec::new();
        for n in &mut nodes {
            if n.rpc.is_some() {
                n.stop();
            }
            let before = n.log_len();
            n.start(&[dbcache]);
            started.push(verifydb_lines(&read(&debug_log(&n.datadir))[before..]));
        }
        let [ours, core] = nodes.each_ref().map(|n| n.verifychain(3, 0));
        assert_eq!(ours.0, core.0, "{dbcache}");
        assert_eq!(ours.0, "false", "{dbcache}");
        for lines in [&started[0], &started[1], &ours.1, &core.1] {
            assert!(lines.iter().any(|l| l == skip), "{dbcache}: {lines:#?}");
        }
        if same_lines {
            assert_eq!(started[0], started[1], "{dbcache}");
            assert_eq!(ours.1, core.1, "{dbcache}");
            let disconnected = "Verification: No coin database inconsistencies in last 6 blocks \
                                (10 transactions)";
            assert_eq!(started[0].last().map(String::as_str), Some(disconnected));
        } else {
            eprintln!(
                "DEC-1007 below the threshold, {dbcache}: startup {:?} / {:?}",
                started[0].last(),
                started[1].last()
            );
            eprintln!(
                "DEC-1007 below the threshold, {dbcache}: verifychain {:?} / {:?}",
                ours.1.last(),
                core.1.last()
            );
        }
        for n in &mut nodes {
            n.stop();
        }
        let args = [dbcache, "-checklevel=3", "-checkblocks=0"];
        let [ours, core] = nodes.each_ref().map(|n| n.refused(&args));
        assert_eq!(ours, core, "{dbcache}");
        let insufficient = "Error: Insufficient dbcache for block verification\n";
        assert_eq!(ours, (Some(1), insufficient.to_owned()), "{dbcache}");
    }
}

/// Core's memory skip of level 3 (`validation.cpp:5144-5146`).
const MEMORY_SKIP: &str = "Skipped verification of level >=3 (insufficient database cache size). \
                           Consider increasing -dbcache.";

/// rdashd's level-3 threshold at its defaults (DEC-1007): `-txindex` on, as dashd's, and
/// `-maxmempool=5` from its default `-blocksonly` (DEC-322). At 0 coins its gate figure (the
/// shards' full ceilings and the scratch layer's first page) is 3,811,296 bytes against a coins
/// share of 3,612,672 at `-dbcache=9`, and 3,973,024 against 4,014,080 at 10
/// (`the_node_s_level_3_gate_at_0_coins`). dashd's figure on this chain is under 1 MiB at both.
const BELOW_THRESHOLD: Budget = Budget {
    args: &["-dbcache=9"],
    share: 3_612_672,
    mempool: 5_000_000,
};
const ABOVE_THRESHOLD: Budget = Budget {
    args: &["-dbcache=10"],
    share: 4_014_080,
    mempool: 5_000_000,
};
/// The threshold with `-blocksonly=0` (Core's default `-maxmempool`, 300 MB; rdashd's from P5,
/// BL-330): 246,688,256 bytes against 246,677,504 at `-dbcache=280` and 247,595,008 at 281.
const RELAYING_BELOW: Budget = Budget {
    args: &["-blocksonly=0", "-dbcache=280"],
    share: 246_677_504,
    mempool: 300_000_000,
};
const RELAYING_ABOVE: Budget = Budget {
    args: &["-blocksonly=0", "-dbcache=281"],
    share: 247_595_008,
    mempool: 300_000_000,
};

/// A start's cache arguments, and the coins share and mempool allowance rdashd logs for them:
/// the log line proves `-txindex` (the share) and `-maxmempool` (the allowance) the run used.
struct Budget {
    args: &'static [&'static str],
    share: u64,
    mempool: u64,
}

impl Budget {
    // Core: node/caches.cpp; init.cpp's "Cache configuration" lines
    fn utxo_line(&self) -> String {
        let mib = |b: u64| b as f64 / f64::from(1 << 20);
        format!(
            "* Using {:.1} MiB for in-memory UTXO set (plus up to {:.1} MiB of unused mempool \
             space)",
            mib(self.share),
            mib(self.mempool)
        )
    }
}

/// Both nodes on the fixture chain, stopped.
fn stopped_pair() -> [Side; 2] {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
        n.replay();
        n.stop();
    }
    nodes
}

/// A start at `budget` on each node: VerifyDB's startup lines. rdashd logs `budget`'s share and
/// allowance.
fn start_both(nodes: &mut [Side; 2], budget: &Budget) -> Vec<Vec<String>> {
    let mut started = Vec::new();
    for n in nodes.iter_mut() {
        let before = n.log_len();
        n.start(budget.args);
        let log = read(&debug_log(&n.datadir))[before..].to_owned();
        if !n.is_dashd() {
            let line = budget.utxo_line();
            assert!(log.lines().any(|l| unstamped(l) == line), "{line}: {log}");
        }
        started.push(verifydb_lines(&log));
        assert_eq!(n.rpc().call("getbestblockhash", "[]").result(), format!(r#""{TIP}""#));
    }
    started
}

/// Below rdashd's threshold at `budget`: its gate is over the coins share from the first
/// block, so the default startup check and `verifychain 3 1` skip level 3 there
/// (`SkippedL3Checks`, cause `Dbcache`): the start proceeds and the RPC is false. dashd verifies
/// both.
fn rdashd_skips_from_the_first_block(budget: &Budget) {
    let mut nodes = stopped_pair();
    let started = start_both(&mut nodes, budget);
    eprintln!(
        "DEC-1007 below {:?} record: rdashd {:#?}\ndashd {:#?}",
        budget.args, started[0], started[1]
    );
    assert_eq!(started[0][0], "Verifying last 6 blocks at level 3");
    assert_eq!(started[0][0], started[1][0]);
    assert!(started[0].iter().any(|l| l == MEMORY_SKIP), "{:#?}", started[0]);
    assert!(!started[1].iter().any(|l| l == MEMORY_SKIP), "{:#?}", started[1]);
    let [ours, core] = nodes.each_ref().map(|n| n.verifychain(3, 1));
    assert_eq!((ours.0.as_str(), core.0.as_str()), ("false", "true"));
    assert!(ours.1.iter().any(|l| l == MEMORY_SKIP), "{:#?}", ours.1);
    assert!(!core.1.iter().any(|l| l == MEMORY_SKIP), "{:#?}", core.1);
    for n in &mut nodes {
        n.stop();
    }
}

/// Above rdashd's threshold at `budget`: every level-3 check runs on both nodes, at startup and
/// through `verifychain 3 1`, with the same lines.
fn both_nodes_verify(budget: &Budget) {
    let mut nodes = stopped_pair();
    let started = start_both(&mut nodes, budget);
    assert_eq!(started[0], started[1]);
    assert!(!started[0].iter().any(|l| l == MEMORY_SKIP), "{:#?}", started[0]);
    let [ours, core] = nodes.each_ref().map(|n| n.verifychain(3, 1));
    assert_eq!(ours, core);
    assert_eq!(ours.0, "true");
    assert!(!ours.1.iter().any(|l| l == MEMORY_SKIP), "{:#?}", ours.1);
    for n in &mut nodes {
        n.stop();
    }
}

/// DEC-1007 (b), just below rdashd's threshold at its defaults.
// Core: validation.cpp:5119-5136, :5181-5183; node/chainstate.cpp:450-456
#[test]
fn just_below_the_level_3_threshold_rdashd_skips_from_the_first_block() {
    rdashd_skips_from_the_first_block(&BELOW_THRESHOLD);
}

/// DEC-1007 (b), just above rdashd's threshold at its defaults.
#[test]
fn just_above_the_level_3_threshold_both_nodes_verify() {
    both_nodes_verify(&ABOVE_THRESHOLD);
}

/// The same pair with `-blocksonly=0`, so that P5's default (BL-330) moves them visibly.
#[test]
fn with_blocksonly_off_the_threshold_is_at_dbcache_281() {
    rdashd_skips_from_the_first_block(&RELAYING_BELOW);
    both_nodes_verify(&RELAYING_ABOVE);
}

/// DEC-1007 (c): below rdashd's threshold an explicit `-checklevel` (`require_full_verification`)
/// fails its load with Core's "Insufficient dbcache for block verification", where dashd
/// verifies and starts.
// Core: node/chainstate.cpp:452-456
#[test]
fn an_explicit_checklevel_below_the_threshold_fails_rdashd_only() {
    let mut nodes = stopped_pair();
    let args = [BELOW_THRESHOLD.args, &["-checklevel=3"]].concat();
    let (code, err) = nodes[0].refused(&args);
    eprintln!("DEC-1007 explicit record: rdashd {code:?} {err:?}");
    let insufficient = "Error: Insufficient dbcache for block verification\n";
    assert_eq!((code, err.as_str()), (Some(1), insufficient));
    nodes[1].start(&args);
    assert_eq!(nodes[1].rpc().call("getbestblockhash", "[]").result(), format!(r#""{TIP}""#));
    nodes[1].stop();
}

/// The coin count of the next `UpdateTip` after `submitblock` of a coinbase-only block on `prev`
/// at `height`.
fn next_block_count(n: &Side, prev: &str, height: u8, time: u32) -> u64 {
    let mut hash: [u8; 32] = parse_hex(prev).try_into().unwrap();
    hash.reverse();
    let block = mine(&hash, time, &[common::mine::coinbase(height, 0)], None, true);
    let before = n.log_len();
    let reply = n.rpc().call("submitblock", &format!(r#"["{}"]"#, block.hex));
    assert_eq!(reply.result(), "null");
    let lines = cache_lines(&read(&debug_log(&n.datadir))[before..]);
    assert_eq!(lines.len(), 1, "{lines:?}");
    lines[0].2
}

/// The verifychain path's parent fills (CH5-1a scope 1, Sol r2 R2): from a cold tip cache (a
/// restart), `verifychain 3 6` reads the coins its walk needs through the tip, which keeps a
/// clean entry for each as Core's `FetchCoin` does, and a repeated walk reads them there. The
/// next block's `UpdateTip` counts the tip's entries (CH5 D10): equal on both nodes after each step, and
/// more after the first walk than before it. `gettxout` answers alike. The resubmission half
/// (Core's `txn-already-known` for a mined T whose outputs the walk filled) waits for rdashd's
/// mempool (MP1). DEC-981b: no transaction is relayed and nothing runs ATMP; the walks run
/// outside the band (level 3, depth 6, the default `-dbcache`: a share far above either figure),
/// with no flush, as the fills are the subject. The resubmission half, when it lands, takes the
/// same level and depth outside the band and must say so.
#[test]
fn verifychain_fills_the_tip_cache_as_dashd_does() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
        n.replay();
        n.stop();
        n.start(&[]);
    }
    let header =
        nodes[0].rpc().call("getblockheader", &format!(r#"["{TIP}"]"#)).result().to_owned();
    let time: u32 =
        header.split(r#""time":"#).nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
    let mut counts = Vec::new();
    let mut tip = TIP.to_owned();
    for (step, height, walk) in
        [("cold", 121, false), ("after a walk", 122, true), ("after another", 123, true)]
    {
        if walk {
            let [ours, core] = nodes.each_ref().map(|n| n.verifychain(3, 6));
            assert_eq!(ours, core, "{step}");
            assert_eq!(ours.0, "true");
        }
        let [ours, core] =
            nodes.each_ref().map(|n| next_block_count(n, &tip, height, time + u32::from(height)));
        assert_eq!(ours, core, "{step}: the tip's coins");
        counts.push(ours);
        tip = nodes[0].rpc().call("getbestblockhash", "[]").result().trim_matches('"').to_owned();
        assert_eq!(nodes[1].rpc().call("getbestblockhash", "[]").result().trim_matches('"'), tip);
    }
    assert!(counts[1] > counts[0] + 1, "the walk filled the tip: {counts:?}");
    for n in 0..2 {
        let [ours, core] = nodes.each_ref().map(|node| {
            node.rpc().call("gettxout", &format!(r#"["{CB118}", {n}]"#)).result().to_owned()
        });
        assert_eq!(ours, core, "gettxout {CB118} {n}");
    }
}

/// Both nodes' genesis hash, internal byte order.
fn genesis_of(n: &Side) -> [u8; 32] {
    let hash = n.rpc().call("getblockhash", "[0]").result().trim_matches('"').to_owned();
    let mut g: [u8; 32] = parse_hex(&hash).try_into().unwrap();
    g.reverse();
    g
}

/// A mined block's hash in RPC (reversed) hex.
fn rpc_hash(m: &Mined) -> String {
    let mut h = m.hash;
    h.reverse();
    common::mine::hex(&h)
}

/// A chain of coinbase-only blocks on `prev` from `height`, each coinbase paying `value` (a
/// branch's tag, so that branches at the same heights differ).
fn blocks_on(prev: &[u8; 32], height: u8, n: u8, value: i64) -> Vec<Mined> {
    let mut out: Vec<Mined> = Vec::new();
    for h in height..height + n {
        let prev = out.last().map_or(prev, |b| &b.hash);
        out.push(common::mine::next(prev, h, value));
    }
    out
}

impl Side {
    /// `submitblock` of each block: the replies (`null`, or `inconclusive` for a block that does
    /// not become the tip).
    fn submit(&self, blocks: &[&Mined]) -> Vec<String> {
        let submit = |b: &&Mined| {
            self.rpc().call("submitblock", &format!(r#"["{}"]"#, b.hex)).result().to_owned()
        };
        blocks.iter().map(submit).collect()
    }

    fn call_on(&self, method: &str, block: &Mined) {
        let reply = self.rpc().call(method, &format!(r#"["{}"]"#, rpc_hash(block)));
        assert_eq!(reply.result(), "null", "{method}: {}", self.binary.display());
    }

    /// The best block and `getchaintips`' (hash, status) pairs, sorted.
    fn tips(&self) -> (String, Vec<(String, String)>) {
        let best = self.rpc().call("getbestblockhash", "[]").result().trim_matches('"').to_owned();
        let reply = self.rpc().call("getchaintips", "[]").result().to_owned();
        let field = |tip: &str, name: &str| {
            let key = format!(r#""{name}":""#);
            let at = tip.find(&key).unwrap_or_else(|| panic!("{name} in {reply}")) + key.len();
            tip[at..].split('"').next().unwrap().to_owned()
        };
        let mut tips: Vec<_> =
            reply.split("},").map(|t| (field(t, "hash"), field(t, "status"))).collect();
        tips.sort();
        (best, tips)
    }

    /// A start with `more` after a clean stop, waited on until startup's ABC, which may follow
    /// init, reaches `best`: the tips, and how many times the start's
    /// `ResetBlockFailureFlags(nullptr)` reconsidered a block.
    fn restart(&mut self, more: &[&str], best: &str) -> ((String, Vec<(String, String)>), usize) {
        self.stop();
        let before = self.log_len();
        self.start(more);
        wait_until(best, || self.tips().0 == best);
        let resets = read(&debug_log(&self.datadir))[before..].matches(RESET).count();
        (self.tips(), resets)
    }
}

/// The line `ResetBlockFailureFlags(nullptr)` logs when it reconsiders a block, on both nodes.
const RESET: &str = "ResetBlockFailureFlags: the best known invalid block";

/// Each node's state after the same restart: equal on both, then the expected best block,
/// (hash, status) tips and reset count.
fn both_restart(
    nodes: &mut [Side; 2],
    more: &[&str],
    best: &Mined,
    tips: &[(&Mined, &str)],
    resets: usize,
) {
    let [ours, core] = nodes.each_mut().map(|n| n.restart(more, &rpc_hash(best)));
    assert_eq!(ours, core, "{more:?}");
    let mut want: Vec<_> = tips.iter().map(|(m, s)| (rpc_hash(m), (*s).to_owned())).collect();
    want.sort();
    assert_eq!(ours, ((rpc_hash(best), want), resets), "{more:?}");
}

const LEVEL_2: &[&str] = &["-checklevel=2"];

/// DEC-992 (d) and (2): `invalidateblock` on the tip, then on a header extending it, each
/// followed by restarts on both nodes. At `-checklevel=2` no reset runs and the block stays
/// invalid; at the default level 3, init's `ResetBlockFailureFlags(nullptr)` reconsiders it, as
/// the best invalid block is the tip's own descendant: the invalidated tip is connected again
/// and the header is a header again. The reset is stored: a start at level 2 after it keeps the
/// reset state. Every start and stop exits cleanly on both. Order (A.1(d)): the loaded tip is
/// valid in each case, so `LoadChainTip` passes it on both nodes before VerifyDB and the reset.
/// The `verifychain` RPC never resets (DEC-992 (5)): after `verifychain 3 6` the invalidated tip
/// stays invalid on both, with no reset line.
// Core: node/chainstate.cpp:463-465; validation.cpp:4163-4242, :3900-4057;
// rpc/blockchain.cpp:1438-1440 (the RPC: VerifyDB alone)
#[test]
fn the_load_time_reset_reconsiders_an_invalidated_tip_and_a_header_past_it() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
    }
    let x = blocks_on(&genesis_of(&nodes[0]), 1, 3, 0);
    for n in &mut nodes {
        assert!(n.submit(&x.iter().collect::<Vec<_>>()).iter().all(|r| r == "null"));
        n.call_on("invalidateblock", &x[2]);
    }
    let invalid = [(&x[1], "active"), (&x[2], "invalid")];
    let [ours, core] = nodes.each_ref().map(|n| {
        let before = n.log_len();
        let (result, _) = n.verifychain(3, 6);
        (result, n.tips(), read(&debug_log(&n.datadir))[before..].matches(RESET).count())
    });
    assert_eq!(ours, core);
    let mut want: Vec<_> = invalid.iter().map(|(m, s)| (rpc_hash(m), (*s).to_owned())).collect();
    want.sort();
    assert_eq!(ours, ("true".to_owned(), (rpc_hash(&x[1]), want), 0));
    both_restart(&mut nodes, LEVEL_2, &x[1], &invalid, 0);
    both_restart(&mut nodes, &[], &x[2], &[(&x[2], "active")], 1);
    both_restart(&mut nodes, LEVEL_2, &x[2], &[(&x[2], "active")], 0);

    // A header past the tip, invalidated.
    let h4 = common::mine::next(&x[2].hash, 4, 0);
    for n in &nodes {
        let reply = n.rpc().call("submitheader", &format!(r#"["{}"]"#, &h4.hex[..160]));
        assert_eq!(reply.result(), "null");
        n.call_on("invalidateblock", &h4);
    }
    let invalid = [(&x[2], "active"), (&h4, "invalid")];
    both_restart(&mut nodes, LEVEL_2, &x[2], &invalid, 0);
    let header = [(&x[2], "active"), (&h4, "headers-only")];
    both_restart(&mut nodes, &[], &x[2], &header, 1);
    both_restart(&mut nodes, LEVEL_2, &x[2], &header, 0);
    for n in &mut nodes {
        n.stop();
    }
}

/// DEC-992 (a) and (c). (a): the best invalid block is on a branch that does not extend the
/// tip (`b2`, after `invalidateblock b1`, under the three-block tip): no reset runs at level 3,
/// and both branches keep their statuses. (c): `invalidateblock p4` with q6 the tip and s5 a side
/// block on p4; after the restart q6 is the best invalid block and extends the tip x3, so level
/// 3's reset clears q6, q5 and p4, and turns p4's `FAILED_CHILD` child s5 into `FAILED_VALID`
/// (`getchaintips` invalid either way; without the promotion `CheckBlockIndex` fails on s5, the
/// failed mask without an invalid parent). Startup's ABC connects q6 again; a later
/// `reconsiderblock s5` makes s5 a valid fork on both.
// Core: validation.cpp:4163-4242 (:4219-4227 the promotion), :5752 (CheckBlockIndex)
#[test]
fn the_load_time_reset_leaves_a_branch_off_the_tip_and_promotes_failed_children() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
    }
    let genesis = genesis_of(&nodes[0]);
    let x = blocks_on(&genesis, 1, 3, 0);
    let b = blocks_on(&genesis, 1, 2, 1);
    let [ours, core] = nodes.each_ref().map(|n| n.submit(&[&x[0], &x[1], &x[2], &b[0], &b[1]]));
    assert_eq!(ours, core);
    for n in &nodes {
        n.call_on("invalidateblock", &b[0]);
    }
    let off = [(&x[2], "active"), (&b[1], "invalid")];
    both_restart(&mut nodes, &[], &x[2], &off, 0);
    both_restart(&mut nodes, LEVEL_2, &x[2], &off, 0);

    let p4 = common::mine::next(&x[2].hash, 4, 0);
    let s5 = common::mine::next(&p4.hash, 5, 1);
    let q = blocks_on(&p4.hash, 5, 2, 0);
    let [ours, core] = nodes.each_ref().map(|n| n.submit(&[&p4, &s5, &q[0], &q[1]]));
    assert_eq!(ours, core);
    for n in &nodes {
        assert_eq!(n.tips().0, rpc_hash(&q[1]));
        n.call_on("invalidateblock", &p4);
    }
    // `-checkblockindex` (regtest's default) is what tells a promotion left undone: the check
    // after startup's ABC fails on s5's failed mask without an invalid parent.
    let reset = [(&q[1], "active"), (&s5, "invalid"), (&b[1], "invalid")];
    both_restart(&mut nodes, &["-checkblockindex=1"], &q[1], &reset, 1);
    let [ours, core] = nodes.each_ref().map(|n| {
        n.call_on("reconsiderblock", &s5);
        n.tips()
    });
    assert_eq!(ours, core);
    let mut want = vec![
        (rpc_hash(&q[1]), "active".to_owned()),
        (rpc_hash(&s5), "valid-fork".to_owned()),
        (rpc_hash(&b[1]), "invalid".to_owned()),
    ];
    want.sort();
    assert_eq!(ours, (rpc_hash(&q[1]), want));
    for n in &mut nodes {
        n.stop();
    }
}

/// DEC-992's order (A.1(d)): `invalidateblock` on the tip, then a kill. Neither node has
/// stored the mark: the index is written with the coins at a flush, or alone by the periodic
/// write an hour on (Core's `fPeriodicWrite`, on the steady clock `setmocktime` leaves), so both
/// come back at the tip, valid, at levels 3 and 2, with no reset. A stored failed tip with no
/// valid block of its work, which only an index write the coins missed leaves, stops Core's
/// `LoadChainTip` at its prune's assert (`v:3603`) before VerifyDB, and rdashd at DEC-196 (1)'s
/// refusal (`dashd-node`'s
/// `an_invalid_stored_tip_fails_load_chain_tip_after_the_coins_at_each_restart`): no reset runs
/// on either.
// Core: validation.cpp:3036-3056 (FlushStateToDisk's timers), :3595-3604, :5003-5029
#[test]
fn a_killed_invalidation_is_not_stored_on_either_node() {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(&[]);
    }
    let x = blocks_on(&genesis_of(&nodes[0]), 1, 3, 0);
    for n in &mut nodes {
        assert!(n.submit(&x.iter().collect::<Vec<_>>()).iter().all(|r| r == "null"));
        // A clean restart flushes the coins and the index at x3.
        n.stop();
        n.start(&[]);
        let later = MOCKTIME_SECS + 2 * 3600;
        assert_eq!(n.rpc().call("setmocktime", &format!("[{later}]")).result(), "null");
        n.call_on("invalidateblock", &x[2]);
        assert_eq!(n.tips().0, rpc_hash(&x[1]));
        let rpc = n.rpc.take().expect("running");
        rpc.node.stop(libc::SIGKILL);
        n.start(&[]);
    }
    both_restart(&mut nodes, &[], &x[2], &[(&x[2], "active")], 0);
    both_restart(&mut nodes, LEVEL_2, &x[2], &[(&x[2], "active")], 0);
    for n in &mut nodes {
        n.stop();
    }
}

/// The evo regtest record's DIP0003 height (CH5-1a acceptance, "Evo regtest").
const EVO: &[&str] = &["-dip3params=5:5"];

/// Both nodes on the evo regtest record: DIP0003 from height 5, dashd's eight blocks; its tip,
/// in RPC hex.
fn evo_pair() -> ([Side; 2], String) {
    let mut nodes = pair();
    for n in &mut nodes {
        n.start(EVO);
    }
    let core = nodes[1].rpc();
    let hashes = core.call("generatetodescriptor", r#"[8, "raw(51)"]"#).result().to_owned();
    for hash in hashes.trim_matches(['[', ']']).split(',') {
        let hash = hash.trim().trim_matches('"');
        let block = core.call("getblock", &format!(r#"["{hash}", 0]"#)).result().to_owned();
        let reply = nodes[0].rpc().call("submitblock", &format!("[{block}]"));
        assert_eq!(reply.result(), "null", "block {hash}");
    }
    let [tip, core_tip] = nodes.each_ref().map(|n| n.tips().0);
    assert_eq!(tip, core_tip);
    (nodes, tip)
}

/// rdashd's level-3 skip reason over the evo tip (DEC-993): the readiness refusal (D18).
fn evo_refusal(tip: &str) -> String {
    format!(
        "rdashd cannot validate block {tip} at height 8 on regtest: VerifyDB's evo checks \
         (CH5-1b) for DIP0003 is not built (ALLOWED-DIFFS D18)"
    )
}

/// DEC-993 on the evo tip: the default startup checks proceed on both nodes. rdashd's skips
/// level 3 with the readiness refusal as its reason, where dashd's verifies; then init's reset
/// runs as at level 3 (no block is failed here) and both serve the same tip.
// Core: node/chainstate.cpp:441-465
#[test]
fn an_evo_tip_starts_with_the_default_checks_on_both_nodes() {
    let (mut nodes, tip) = evo_pair();
    let mut started = Vec::new();
    for n in &mut nodes {
        n.stop();
        let before = n.log_len();
        n.start(EVO);
        started.push(verifydb_lines(&read(&debug_log(&n.datadir))[before..]));
        assert_eq!(n.tips().0, tip);
    }
    eprintln!("DEC-993 startup record: rdashd {:#?}\ndashd {:#?}", started[0], started[1]);
    let skip = format!("Skipped verification of level >=3: {}", evo_refusal(&tip));
    assert!(started[0].contains(&skip), "{:#?}", started[0]);
    assert_eq!(started[0][0], "Verifying last 6 blocks at level 3");
    assert_eq!(started[0][0], started[1][0]);
    let verified =
        "Verification: No coin database inconsistencies in last 6 blocks (6 transactions)";
    assert_eq!(started[1].last().map(String::as_str), Some(verified));
    for n in &mut nodes {
        n.stop();
    }
}

/// DEC-993: an explicit check over the evo tip (`require_full_verification`) fails rdashd's load
/// with the readiness refusal as a plain init error, where dashd verifies and starts. `which` is
/// `-checkblocks` or `-checklevel`.
fn explicit_checks_fail_on_rdashd_only(which: &str) {
    let (mut nodes, tip) = evo_pair();
    for n in &mut nodes {
        n.stop();
    }
    let args = [EVO, &[which]].concat();
    let (code, err) = nodes[0].refused(&args);
    eprintln!("DEC-993 {which} record: rdashd {code:?} {err:?}");
    assert_eq!((code, err), (Some(1), format!("Error: {}\n", evo_refusal(&tip))));
    nodes[1].start(&args);
    assert_eq!(nodes[1].tips().0, tip);
    nodes[1].stop();
}

#[test]
fn explicit_checkblocks_over_an_evo_tip_fail_rdashd_with_the_readiness_refusal() {
    explicit_checks_fail_on_rdashd_only("-checkblocks=6");
}

#[test]
fn explicit_checklevel_over_an_evo_tip_fails_rdashd_with_the_readiness_refusal() {
    explicit_checks_fail_on_rdashd_only("-checklevel=3");
}

/// DEC-993 through the RPC: levels 0 to 2 match dashd's; at levels 3 and 4 rdashd's walk skips
/// level 3 at the evo tip, with the readiness refusal as the reason in its log, and `verifychain`
/// is false where dashd verifies (true). A known gap until CH5-1b, not an allowed diff (PLAN
/// CH5-1a, RP1a D46). No transaction is relayed (DEC-981b).
// Core: rpc/blockchain.cpp:1440; validation.cpp:5119-5136, :5144-5146, :5181-5183
#[test]
fn verifychain_skips_level_3_on_evo_blocks_where_dashd_verifies() {
    let (mut nodes, tip) = evo_pair();
    for level in [0, 1, 2] {
        let [ours, core] = nodes.each_ref().map(|n| n.verifychain(level, 0));
        assert_eq!(ours, core, "{level}");
        assert_eq!(ours.0, "true");
    }
    let skip = format!("Skipped verification of level >=3: {}", evo_refusal(&tip));
    for (level, depth) in [(3, 1), (4, 1), (3, 0), (4, 0)] {
        let [ours, core] = nodes.each_ref().map(|n| n.verifychain(level, depth));
        eprintln!("DEC-993 verifychain {level} {depth} record: rdashd {ours:#?}\ndashd {core:#?}");
        assert_eq!((ours.0.as_str(), core.0.as_str()), ("false", "true"), "{level} {depth}");
        assert_eq!(ours.1[0], core.1[0], "{level} {depth}");
        assert!(ours.1.contains(&skip), "{level} {depth}: {:#?}", ours.1);
        assert!(!ours.1.iter().any(|l| l.contains("Verification error")), "{:#?}", ours.1);
    }
    for n in &mut nodes {
        n.stop();
    }
}
