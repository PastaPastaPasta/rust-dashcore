//! `-dbcrashratio` on the node (CH3 §4.6, CH3b b4b): rdashd crashes in a coins flush or in
//! recovery's replay as dashd v24.0.0 does, recovers to the same UTXO set, and keeps every
//! block and invalid mark it answered. Every node, dashd or rdashd, takes the same flags
//! (`-blocksonly=1`, whose 5 MB `-maxmempool` bounds the coins' unused mempool allowance), runs
//! unconnected, and gets the same test-built blocks by `submitblock`; an uncrashed dashd is the
//! reference. P1 and P2 crash at ratio 1, deterministically, on rdashd and on a dashd crash node
//! alike, and again with rdashd on the production profile (`-storageprofile=production`, BL-634),
//! each launch checking the profile it ran; P3 pins the load-time failed-child derivation the
//! loop's invalidations rely on. P4 crashes and replays Core-made blocks instead, the recorded
//! `feature_asset_locks` run's quorum commitment and asset unlocks. They cannot be skipped: with
//! `SKIP_DASHD_TESTS` set they fail (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::ops::RangeInclusive;
use std::time::Instant;

use common::mine::*;
use common::node::*;
use common::recorded::{RecordedRun, recorded_run};
use common::*;

#[test]
fn the_newest_options_file_is_the_highest_number() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "OPTIONS-000005",
        "OPTIONS-999999",
        "OPTIONS-1000001",
        "OPTIONS-1000002.dbtmp",
        "OPTIONS-",
        "OPTIONS-12x",
        "LOG",
    ] {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    let newest = newest_options(dir.path()).unwrap();
    assert_eq!(newest.file_name().unwrap(), "OPTIONS-1000001");
    std::fs::remove_file(newest).unwrap();
    assert_eq!(newest_options(dir.path()).unwrap().file_name().unwrap(), "OPTIONS-999999");
}

/// P1's nodes: the uncrashed dashd reference, then the crash nodes, rdashd and dashd.
struct P1 {
    reference: Node,
    crash: [Node; 2],
    fork: Fork,
    /// `gettxoutsetinfo` at A104, s0's.
    at_a104: Vec<(String, String)>,
    /// Last, so the nodes are gone before it is.
    _home: RetainedHome,
}

impl P1 {
    /// `submitblock` of `block` on every node: each crash node's reply is the reference's.
    fn submit(&self, block: &Mined) {
        let want = self.reference.submit(block);
        for node in &self.crash {
            assert_eq!(node.submit(block), want, "{}", node.name);
        }
    }

    /// The tip and the tips are the reference's on every crash node.
    fn assert_chain(&self, step: &str) {
        let (best, tips) = (self.reference.best(), self.reference.tips());
        for node in &self.crash {
            assert_eq!(node.best(), best, "{step}: {}'s tip", node.name);
            assert_eq!(node.tips(), tips, "{step}: {}'s tips", node.name);
        }
    }

    /// [`assert_chain`](Self::assert_chain), and the UTXO set is the reference's on every crash
    /// node.
    fn assert_state(&self, step: &str) -> Vec<(String, String)> {
        self.assert_chain(step);
        let set = self.reference.utxo_set();
        for node in &self.crash {
            assert_eq!(node.utxo_set(), set, "{step}: {}'s UTXO set", node.name);
        }
        set
    }

    /// `gettxoutsetinfo` on every crash node, whose copy point's flush crashes at ratio 1: no
    /// reply, exit 0, Core's line and no shutdown after it.
    fn crash_in_the_copy_point(&mut self, step: &str) {
        for node in &mut self.crash {
            if let Ok(set) = node.try_utxo_set() {
                panic!("{step}: {} answered {set:?}", node.name);
            }
            node.crashed();
        }
    }
}

/// P1's steps s0–s3, asserted, with rdashd on `storage`; the nodes are left running at ratio 0
/// at B105.
fn p1(storage: Storage) -> P1 {
    let dashd = require_dashd();
    let home = RetainedHome::new(&format!("b4b-p1-{}-", storage.name()));
    let flags = ["-dbcache=4", "-dbbatchsize=300"];
    let mut reference = Node::new("reference", &dashd, home.path(), &flags);
    let mut crash = [
        Node::rdashd(home.path(), &flags, storage),
        Node::new("dashd-crash", &dashd, home.path(), &flags),
    ];
    reference.start(0);
    for node in &mut crash {
        node.start(0);
    }
    let fork = Fork::new(&genesis(&reference));
    let mut p = P1 {
        reference,
        crash,
        fork,
        at_a104: Vec::new(),
        _home: home,
    };

    // s0, ratio 0: F, then A, durable at A104 through the copy point.
    for block in p.fork.f.iter().chain(&p.fork.a) {
        p.submit(block);
    }
    p.at_a104 = p.assert_state("s0");
    assert_eq!(p.reference.best(), shown(&p.fork.a[2].hash));
    for node in &mut p.crash {
        node.stop();
        node.start(1);
    }

    // s1, ratio 1: B reorgs with no flush; the copy point's flush crashes.
    for block in &p.fork.b {
        p.submit(block);
    }
    let b105 = shown(&p.fork.b[3].hash);
    assert_eq!(p.reference.best(), b105);
    p.assert_chain("s1");
    p.crash_in_the_copy_point("s1");

    // s2, ratio 1: the restart replays across the fork and crashes in recovery.
    for node in &mut p.crash {
        let log = node.launch(1).expect_err("a crash in recovery");
        let replaying = log.find("Replaying blocks").unwrap_or_else(|| panic!("{log}"));
        assert!(replaying < log.find(CRASH_LINE).unwrap(), "s2: {}\n{log}", node.name);
    }

    // s3, ratio 0: recovery completes, as on dashd.
    let mut replays = Vec::new();
    for node in &mut p.crash {
        node.start(0);
        replays.push(replay_lines(&node.new_log()));
    }
    for replay in &replays {
        assert!(replay.iter().any(|l| l.starts_with("Rolling forward ")), "{replay:?}");
        assert!(replay.iter().any(|l| l.starts_with("Rolling back")), "{replay:?}");
    }
    assert_eq!(replays[0], replays[1], "s3: rdashd's replay and dashd's");
    let at_b105 = p.assert_state("s3");
    assert_eq!(value_of(&at_b105, "bestblock"), b105);
    assert_ne!(value_of(&at_b105, "muhash"), value_of(&p.at_a104, "muhash"), "non-vacuous");
    p
}

/// P1: an ALWAYS flush (the `gettxoutsetinfo` copy point) after a reorg crashes at its first
/// threshold batch, the restart crashes in recovery's replay across the fork, and the next
/// recovery completes with dashd's replay lines, tip, tips and UTXO set, on rdashd and on a dashd
/// crash node, each against an uncrashed dashd.
#[test]
fn a_dbcrashratio_crash_and_its_recovery_match_dashd_v24() {
    p1_on(Storage::Live);
}

/// P1 with rdashd on the production profile (BL-634): the profile mainnet and testnet run, whose
/// flush rules and engine settings differ from regtest's live-copy default.
#[test]
fn a_dbcrashratio_crash_and_its_recovery_match_dashd_v24_on_the_production_profile() {
    p1_on(Storage::Production);
}

fn p1_on(storage: Storage) {
    let mut p = p1(storage);
    for node in p.crash.iter_mut().chain([&mut p.reference]) {
        node.stop();
    }
}

/// P2: after P1, an answered `invalidateblock B103` (the walk disconnects B105..B103, then
/// activation reorgs to A104, the longer valid branch) and a crash in the next flush: recovery
/// rolls back to the fork and forward to A104, B105 stays invalid, and the UTXO set is P1 s0's.
#[test]
fn an_answered_invalidateblock_survives_a_dbcrash_as_on_dashd_v24() {
    p2_on(Storage::Live);
}

/// P2 with rdashd on the production profile (BL-634).
#[test]
fn an_answered_invalidateblock_survives_a_dbcrash_as_on_dashd_v24_on_the_production_profile() {
    p2_on(Storage::Production);
}

fn p2_on(storage: Storage) {
    let mut p = p1(storage);
    for node in &mut p.crash {
        node.stop();
        node.start(1);
    }

    // s4, ratio 1: the invalidation is answered on every node; the copy point's flush crashes.
    let b103 = shown(&p.fork.b[1].hash);
    let want = p.reference.call("invalidateblock", &format!("[{b103}]"));
    for node in &p.crash {
        assert_eq!(node.call("invalidateblock", &format!("[{b103}]")), want, "{}", node.name);
    }
    let a104 = shown(&p.fork.a[2].hash);
    assert_eq!(p.reference.best(), a104);
    p.crash_in_the_copy_point("s4");

    // s5, ratio 0: recovery rolls back B and forward A; the mark held.
    for node in &mut p.crash {
        node.start(0);
        let replay = replay_lines(&node.new_log());
        assert!(replay.iter().any(|l| l.starts_with("Rolling back")), "{}: {replay:?}", node.name);
        assert!(
            replay.iter().any(|l| l.starts_with("Rolling forward")),
            "{}: {replay:?}",
            node.name
        );
    }
    let set = p.assert_state("s5");
    assert_eq!(value_of(&set, "bestblock"), a104);
    let tips = p.reference.tips();
    assert_eq!(status_of(&tips, &p.fork.a[2].hash), Some("active"), "{tips:?}");
    assert_eq!(status_of(&tips, &p.fork.b[3].hash), Some("invalid"), "{tips:?}");
    assert_eq!(set, p.at_a104, "the UTXO set of P1 s0");
    for node in p.crash.iter_mut().chain([&mut p.reference]) {
        node.stop();
    }
}

/// P4's nodes, on the recorded `feature_asset_locks` run: the uncrashed dashd reference, then the
/// crash nodes, rdashd and dashd.
struct P4 {
    run: RecordedRun,
    reference: Node,
    crash: [Node; 2],
    /// Last, so the nodes are gone before it is.
    _home: RetainedHome,
}

impl P4 {
    /// `submitblock` of the run's blocks at `heights` on every node: the reference connects each,
    /// and each crash node's reply is the reference's.
    fn submit(&self, heights: RangeInclusive<usize>) {
        for h in heights {
            let params = format!(r#"["{}"]"#, self.run.blocks[h - 1]);
            let want = self.reference.call("submitblock", &params);
            assert_eq!(want, "null", "the reference connects block {h}");
            for node in &self.crash {
                assert_eq!(node.call("submitblock", &params), want, "{} at {h}", node.name);
            }
        }
    }

    /// The tip is the run's block at `height`, and the tips, the UTXO set and
    /// `getcreditpoolinfo` (the evo state's credit pool) are the reference's, on every crash node.
    /// `gettxoutsetinfo` is the copy point: at ratio 0 its flush leaves the coins durable.
    fn assert_state(&self, step: &str, height: usize) {
        let best = format!(r#""{}""#, self.run.hash(height));
        assert_eq!(self.reference.best(), best, "{step}: the reference's tip");
        let (tips, set) = (self.reference.tips(), self.reference.utxo_set());
        let pool = self.reference.call("getcreditpoolinfo", "[]");
        for node in &self.crash {
            assert_eq!(node.best(), best, "{step}: {}'s tip", node.name);
            assert_eq!(node.tips(), tips, "{step}: {}'s tips", node.name);
            assert_eq!(node.utxo_set(), set, "{step}: {}'s UTXO set", node.name);
            let mine = node.call("getcreditpoolinfo", "[]");
            assert_eq!(mine, pool, "{step}: {}'s credit pool", node.name);
        }
    }

    /// From a durable flush: the crash nodes restart at ratio 1, the blocks at `window` are
    /// submitted, and the copy point's flush crashes; the restart at ratio 0 rolls forward exactly
    /// `window`, as on dashd, and the state is the reference's.
    fn crash_and_replay(&mut self, step: &str, window: RangeInclusive<usize>) {
        let pool = pool_amounts(&self.reference.call("getcreditpoolinfo", "[]"));
        for node in &mut self.crash {
            node.stop();
            node.start(1);
        }
        self.submit(window.clone());
        for node in &mut self.crash {
            if let Ok(set) = node.try_utxo_set() {
                panic!("{step}: {} answered {set:?}", node.name);
            }
            node.crashed();
        }
        let want: Vec<String> = std::iter::once("Replaying blocks".to_owned())
            .chain(window.clone().map(|h| format!("Rolling forward {} ({h})", self.run.hash(h))))
            .collect();
        for node in &mut self.crash {
            node.start(0);
            assert_eq!(replay_lines(&node.new_log()), want, "{step}: {}'s replay", node.name);
        }
        self.assert_state(step, *window.end());
        let after = pool_amounts(&self.reference.call("getcreditpoolinfo", "[]"));
        assert_ne!(after, pool, "{step}: the window moves the credit pool");
    }
}

/// A `getcreditpoolinfo` reply's amounts (`balance`, `currentlimit`, `window.unlocked`), without
/// the block it names.
fn pool_amounts(reply: &str) -> [serde_json::Value; 3] {
    let v: serde_json::Value =
        serde_json::from_str(reply).unwrap_or_else(|e| panic!("{e}: {reply}"));
    [v["balance"].clone(), v["currentlimit"].clone(), v["window"]["unlocked"].clone()]
}

/// Whether the run's block at `height` mines a non-null quorum commitment.
fn mines_a_commitment(run: &RecordedRun, height: usize) -> bool {
    let bytes = parse_hex(&run.blocks[height - 1]);
    let block = dash_consensus::primitives::decode_block(&mut bytes.as_slice()).unwrap();
    block.into_value().txs().iter().any(|tx| {
        matches!(
            tx.payload(),
            Some(Ok(decoded)) if matches!(
                decoded.payload(),
                dash_primitives::SpecialPayload::QuorumCommitment(p) if !p.commitment().is_null()
            )
        )
    })
}

/// P4 (CE5-06): Core #7869's two crash cycles on Core-made blocks, the recorded
/// `feature_asset_locks` run (`test-vectors/statedigest/regtest-v24`) under its `chain_args`.
/// Since #7869 dashd's `ReplayBlocks` re-runs each block's special transactions with the chain at
/// the block's parent and under the block's own BLS scheme (`validation.cpp` v24.0.0:5209-5215,
/// :5310-5314); recovery writes coins only, the evo state being at `new` already. Cycle 1 replays
/// the run's first mined commitment (179) through an asset unlock (189); cycle 2 an asset unlock
/// (238) as the only block rolled forward. Each recovery completes with dashd's replay lines, tip,
/// tips, UTXO set and credit pool, on rdashd and on a dashd crash node, each against an uncrashed
/// dashd, and connects the blocks after it.
#[test]
fn a_replay_of_a_quorum_commitment_and_an_asset_unlock_matches_dashd_v24() {
    let run = recorded_run("feature_asset_locks");
    let unlocks: Vec<usize> = run.asset_unlocks().iter().map(|(_, h, _)| *h).collect();
    assert!(unlocks.contains(&189) && unlocks.contains(&238), "{unlocks:?}");
    assert!(!unlocks.iter().any(|h| (179..189).contains(h)), "{unlocks:?}");
    assert!(mines_a_commitment(&run, 179), "block 179");
    assert!(!(1..179).any(|h| mines_a_commitment(&run, h)), "179 is the first");

    let dashd = require_dashd();
    let home = RetainedHome::new("b4b-p4-");
    // A one-byte batch size, as #7869's test: every flush, a one-block flush too, writes a partial
    // batch, at which ratio 1 crashes.
    let flags = [&run.args()[..], &["-dbcache=4", "-dbbatchsize=1"]].concat();
    let mut reference = Node::new("reference", &dashd, home.path(), &flags);
    let mut crash = [
        Node::rdashd(home.path(), &flags, Storage::Live),
        Node::new("dashd-crash", &dashd, home.path(), &flags),
    ];
    reference.start(0);
    for node in &mut crash {
        node.start(0);
    }
    let mut p = P4 {
        run,
        reference,
        crash,
        _home: home,
    };

    // Cycle 1, from a durable flush at 178.
    p.submit(1..=178);
    p.assert_state("cycle 1 s0", 178);
    p.crash_and_replay("cycle 1", 179..=189);

    // Cycle 2, from a durable flush at 237.
    p.submit(190..=237);
    p.assert_state("cycle 2 s0", 237);
    p.crash_and_replay("cycle 2", 238..=238);

    // The recovered state connects what follows, as the reference does.
    p.submit(239..=260);
    p.assert_state("after cycle 2", 260);

    for node in p.crash.iter_mut().chain([&mut p.reference]) {
        node.stop();
    }
}

/// P3: the load-time failed-child derivation the loop's invalidation rule rests on (CH3b b4b
/// invariant 2), on dashd and rdashd. Coinbase-only blocks on genesis: a1 a2 a3; b3 b4 b5 on a2;
/// d2 d3 d4 on a1. `invalidateblock a2` disconnects b5..a2 and activation picks d4, which
/// outranks a3, so a3 keeps its own valid status in memory (`valid-fork`); a restart derives
/// `BLOCK_FAILED_CHILD` for it (`node/blockstorage.cpp v24.0.0:324-326`), and it shows `invalid`.
#[test]
fn a_side_tip_under_an_invalidated_block_turns_invalid_on_load_as_on_dashd_v24() {
    let dashd = require_dashd();
    let home = RetainedHome::new("b4b-p3-");
    let mut nodes = [
        Node::new("dashd", &dashd, home.path(), &[]),
        Node::rdashd(home.path(), &[], Storage::Live),
    ];
    for node in &mut nodes {
        node.start(0);
    }
    let g = genesis(&nodes[0]);
    let next = |prev: &[u8; 32], height: u16, tag: &[u8]| block(prev, height, tag, 1, None);
    let a1 = next(&g, 1, b"a");
    let a2 = next(&a1.hash, 2, b"a");
    let a3 = next(&a2.hash, 3, b"a");
    let b3 = next(&a2.hash, 3, b"b");
    let b4 = next(&b3.hash, 4, b"b");
    let b5 = next(&b4.hash, 5, b"b");
    let d2 = next(&a1.hash, 2, b"d");
    let d3 = next(&d2.hash, 3, b"d");
    let d4 = next(&d3.hash, 4, b"d");
    for block in [&a1, &a2, &a3, &b3, &b4, &b5, &d2, &d3, &d4] {
        let replies = nodes.each_ref().map(|n| n.submit(block));
        assert_eq!(replies[1], replies[0]);
    }
    // The shape, so the control is not vacuous: a3 is a tip off the active path (b5's), below
    // every other tip, so it outranks nothing; and it lies under a2 off the active chain, the
    // invalidation the loop's invariant 2 refuses.
    for node in &nodes {
        assert_eq!(node.best(), shown(&b5.hash), "{}", node.name);
        let tips = node.tips();
        assert_eq!(status_of(&tips, &a3.hash), Some("valid-fork"), "{tips:?}");
        assert_eq!(status_of(&tips, &b5.hash), Some("active"), "{tips:?}");
        assert!(tips.iter().all(|t| t.height > 3 || t.hash == rpc_hex(&a3.hash)));
    }
    let a2_shown = shown(&a2.hash);
    for node in &nodes {
        assert_eq!(node.call("invalidateblock", &format!("[{a2_shown}]")), "null");
    }
    let before = nodes.each_ref().map(Node::tips);
    assert_eq!(before[1], before[0], "rdashd's tips and dashd's");
    assert_eq!(status_of(&before[0], &d4.hash), Some("active"), "{:?}", before[0]);
    assert_eq!(status_of(&before[0], &b5.hash), Some("invalid"), "{:?}", before[0]);
    assert_eq!(status_of(&before[0], &a3.hash), Some("valid-fork"), "{:?}", before[0]);

    for node in &mut nodes {
        node.stop();
        node.start(0);
    }
    let after = nodes.each_ref().map(Node::tips);
    assert_eq!(after[1], after[0], "rdashd's tips and dashd's after the restart");
    assert_eq!(status_of(&after[0], &a3.hash), Some("invalid"), "{:?}", after[0]);
    assert_eq!(status_of(&after[0], &d4.hash), Some("active"), "{:?}", after[0]);
    for node in &mut nodes {
        assert_eq!(node.best(), shown(&d4.hash), "{}", node.name);
        node.stop();
    }
}

// ---- L1: the reorg loop (nightly; `contrib/node/b4b-nightly.sh`) ----

/// splitmix64: the loop's script, from `B4B_SEED`.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A draw in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

/// A loop profile (CH3b b4b §3.2): its blocks, `-dbbatchsize`, `-dbcrashratio`, how often the
/// muhash check runs, and the coverage each run must reach (§3.7).
#[derive(Clone, Copy, Debug)]
struct Profile {
    name: &'static str,
    /// Each loop block's coinbase outputs, and its spend's inputs (and outputs).
    outputs: usize,
    spends: usize,
    batch: &'static str,
    ratio: i64,
    /// `CHECK` after every `check_every`th operation (and after every restart).
    check_every: usize,
    /// The coverage a run with rdashd crashing must reach, and one with dashd crashing.
    min: Counters,
    dashd_min: Counters,
}

/// §3.7's counters for one run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Counters {
    restarts: u32,
    crashed_on_restart: u32,
    recovered_across_fork: u32,
    rolled_back: u32,
    crashed_in_reorg: u32,
    pressure_flushes: u32,
    /// Exits found right after a reply; under a ratio rdashd's flushes are its RPCs' own (D14),
    /// so for rdashd this stays 0.
    died_after_reply: u32,
}

/// The loop's flags beyond [`FLAGS`], the same on every node.
fn loop_flags(profile: &Profile) -> [&'static str; 4] {
    ["-dbcache=4", profile.batch, "-debug=coindb", "-debug=bench"]
}

const NONE: Counters = Counters {
    restarts: 0,
    crashed_on_restart: 0,
    recovered_across_fork: 0,
    rolled_back: 0,
    crashed_in_reorg: 0,
    pressure_flushes: 0,
    died_after_reply: 0,
};

// Calibration (CH3b b4b §3.7), 20 runs per profile and crash node. Each ratio makes a threshold
// sub-batch's draw crash about half the split flushes (measured 0.55 on rdashd and 0.56 on dashd in
// `split`, 0.60 and 0.72 in `pressure`). Each threshold is missed with probability below 1e-6
// under these bounds: a counter that sums independent draws is bounded by a Poisson of its mean,
// and `restarts` and `crashed_on_restart` by the crashed flushes under them (each one restarts the
// node, and its first recovery crashes with the measured rate). `restarts` is the largest such
// value, the others are 1, as in the FT. A counter no node can hold that way is not asserted for
// it: `recovered_across_fork` in `pressure` (counted only), and on dashd in `pressure`,
// `crashed_on_restart` (bound 2e-6) and `crashed_in_reorg` (mean 11.6, bound 1e-5). rdashd's
// `crashed_in_reorg` in `pressure` holds with little room: mean 14.05, bound 7.9e-7.
const SPLIT: Profile = Profile {
    name: "split",
    outputs: 200,
    spends: 50,
    batch: "-dbbatchsize=1000",
    ratio: 40,
    check_every: 1,
    min: Counters {
        restarts: 4,
        crashed_on_restart: 1,
        recovered_across_fork: 1,
        ..NONE
    },
    dashd_min: Counters {
        restarts: 4,
        crashed_on_restart: 1,
        recovered_across_fork: 1,
        ..NONE
    },
};

const PRESSURE: Profile = Profile {
    name: "pressure",
    outputs: 6000,
    spends: 500,
    batch: "-dbbatchsize=200000",
    ratio: 5,
    check_every: 8,
    min: Counters {
        restarts: 8,
        crashed_on_restart: 1,
        crashed_in_reorg: 1,
        pressure_flushes: 1,
        ..NONE
    },
    dashd_min: Counters {
        restarts: 2,
        pressure_flushes: 1,
        ..NONE
    },
};

/// The operations per run, as in `feature_dbcrash.py:247`.
const ITERATIONS: usize = 40;
/// Regtest's `nMasternodePaymentsStartBlock`: the blocks stay below it.
const MAX_HEIGHT: u16 = 239;
/// The trunk every branch shares: blocks at `h` spend coinbase outputs from `h − 100`.
const TRUNK: u16 = 100;

/// One block the script made.
struct Blk {
    mined: Mined,
    parent: [u8; 32],
    height: u16,
    coinbase: [u8; 32],
    outputs: usize,
    spend: Option<[u8; 32]>,
}

/// An invalidation round (§3.4): R, its parent P and R's branch tip H.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Round {
    None,
    /// The replacement branch from P is stored; `INVALIDATE` is next.
    Prepared {
        r: [u8; 32],
        h: [u8; 32],
    },
    /// R and its branch are failed until `RECONSIDER`.
    Window {
        r: [u8; 32],
        h: [u8; 32],
    },
}

/// The script's block tree: every block it submitted, the tip the reference reached, the blocks
/// the reference holds as failed, and the round.
struct Model {
    blocks: std::collections::HashMap<[u8; 32], Blk>,
    active: [u8; 32],
    failed: std::collections::HashSet<[u8; 32]>,
    round: Round,
    tags: u32,
}

impl Model {
    /// The script's tree before the trunk: genesis alone, active.
    fn new(genesis: [u8; 32]) -> Model {
        Model {
            blocks: std::collections::HashMap::new(),
            active: genesis,
            failed: std::collections::HashSet::new(),
            round: Round::None,
            tags: 0,
        }
    }

    fn height(&self, hash: &[u8; 32]) -> u16 {
        self.blocks.get(hash).map_or(0, |b| b.height)
    }

    fn parent(&self, hash: &[u8; 32]) -> Option<[u8; 32]> {
        self.blocks.get(hash).map(|b| b.parent)
    }

    /// `hash`'s ancestor at `height` (itself at its own).
    fn ancestor(&self, hash: &[u8; 32], height: u16) -> [u8; 32] {
        let mut at = *hash;
        while self.height(&at) > height {
            at = self.parent(&at).unwrap();
        }
        at
    }

    fn descends(&self, block: &[u8; 32], from: &[u8; 32]) -> bool {
        self.height(block) >= self.height(from) && self.ancestor(block, self.height(from)) == *from
    }

    fn tips(&self) -> Vec<[u8; 32]> {
        let parents: std::collections::HashSet<_> =
            self.blocks.values().map(|b| b.parent).collect();
        self.blocks.keys().filter(|h| !parents.contains(*h)).copied().collect()
    }

    /// The unique most-work tip outside `failed`, or `None` on a tie (invariant 1).
    fn best(&self, failed: &std::collections::HashSet<[u8; 32]>) -> Option<[u8; 32]> {
        let at_top = self.top(failed);
        (at_top.len() == 1).then(|| at_top[0])
    }

    /// The most-work tips outside `failed`.
    fn top(&self, failed: &std::collections::HashSet<[u8; 32]>) -> Vec<[u8; 32]> {
        let valid: Vec<_> = self.tips().into_iter().filter(|t| !failed.contains(t)).collect();
        let top = valid.iter().map(|t| self.height(t)).max().unwrap_or(0);
        valid.into_iter().filter(|t| self.height(t) == top).collect()
    }

    /// The tip activation reaches: the most-work valid tip, the active one among equals (Core
    /// keeps the first it received), as inside a `FORK` before its last block.
    fn activated(&self) -> [u8; 32] {
        let top = self.top(&self.failed);
        if top.contains(&self.active) {
            self.active
        } else {
            assert_eq!(top.len(), 1, "a tie the script never makes: {top:?}");
            top[0]
        }
    }

    /// Invariant 1 after a completed operation: one valid tip at the most work, R's subtree
    /// counted valid in the window, where `RECONSIDER` will make it valid again.
    fn assert_one_best(&self) {
        let failed = match self.round {
            Round::Window {
                ..
            } => std::collections::HashSet::new(),
            _ => self.failed.clone(),
        };
        assert!(self.best(&failed).is_some(), "invariant 1: {:?}", self.top(&failed));
    }

    /// A new block on `prev` at `height`: `outputs` coinbase outputs, and from `TRUNK + 1` a
    /// spend of a seeded slice of the coinbase at `height − 100` and of its parent's spend.
    fn make(&mut self, rng: &mut Rng, prev: [u8; 32], profile: &Profile) -> [u8; 32] {
        let height = self.height(&prev) + 1;
        assert!(height <= MAX_HEIGHT, "height {height}: the loop stays below 240");
        self.tags += 1;
        let outputs = if height <= TRUNK {
            profile.spends
        } else {
            profile.outputs
        };
        let cb = coinbase_paying(height, &self.tags.to_le_bytes(), &vec![0; outputs]);
        let coinbase = txid(&cb);
        let mut txs = vec![cb];
        let mut spend_txid = None;
        if height > TRUNK {
            let source = &self.blocks[&self.ancestor(&prev, height - TRUNK)];
            let first = rng.range(0, (source.outputs - profile.spends) as u64) as u32;
            let mut inputs: Vec<_> =
                (first..first + profile.spends as u32).map(|v| (source.coinbase, v)).collect();
            inputs.extend(self.blocks.get(&prev).and_then(|p| p.spend).map(|txid| (txid, 0)));
            let spend = spend(&inputs, &vec![0; profile.spends]);
            spend_txid = Some(txid(&spend));
            txs.push(spend);
        }
        let mined = mine_txs(&prev, GENESIS_TIME + 60 * u32::from(height), &txs);
        let hash = mined.hash;
        self.blocks.insert(
            hash,
            Blk {
                mined,
                parent: prev,
                height,
                coinbase,
                outputs,
                spend: spend_txid,
            },
        );
        hash
    }

    /// Invariant 3 (and the property invariant 2 protects): no failed block descends from the
    /// active tip, and every block under a failed one is failed.
    fn assert_invariants(&self) {
        for f in &self.failed {
            assert!(!self.descends(f, &self.active), "invariant 3");
            for (hash, b) in &self.blocks {
                if b.parent == *f {
                    assert!(self.failed.contains(hash), "a valid block under a failed one");
                }
            }
        }
    }
}

/// One RPC of the script.
#[derive(Clone, Debug)]
enum Op {
    Submit([u8; 32]),
    Invalidate([u8; 32]),
    Reconsider([u8; 32]),
    Check,
}

impl Op {
    /// The RPC's method and params, `blocks` holding a submitted block; `None` for `CHECK`, whose
    /// call is `gettxoutsetinfo "muhash"`.
    fn rpc(
        &self,
        blocks: &std::collections::HashMap<[u8; 32], Blk>,
    ) -> Option<(&'static str, String)> {
        Some(match self {
            Op::Submit(h) => ("submitblock", format!(r#"["{}"]"#, blocks[h].mined.hex)),
            Op::Invalidate(h) => ("invalidateblock", format!("[{}]", shown(h))),
            Op::Reconsider(h) => ("reconsiderblock", format!("[{}]", shown(h))),
            Op::Check => return None,
        })
    }
}

impl std::fmt::Display for Op {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Op::Submit(h) => write!(f, "submitblock {}", shown(h)),
            Op::Invalidate(h) => write!(f, "invalidateblock {}", shown(h)),
            Op::Reconsider(h) => write!(f, "reconsiderblock {}", shown(h)),
            Op::Check => write!(f, "gettxoutsetinfo muhash"),
        }
    }
}

/// Whether `op`, crashed in after the reference answered it, crashed in a reorg: the model is the
/// reference's after `op`, and `before` the tip before it. A submitted block reorgs when it won
/// and its parent was not that tip (an extension does not); an invalidation or reconsideration
/// when the tip moved; `CHECK` never.
fn reorged(model: &Model, op: &Op, before: &[u8; 32]) -> bool {
    match op {
        Op::Submit(hash) => model.active == *hash && model.parent(hash).as_ref() != Some(before),
        Op::Invalidate(_) | Op::Reconsider(_) => model.active != *before,
        Op::Check => false,
    }
}

/// The reference's state a recovery must reach, read once per recovery: its tip, tips and UTXO
/// set.
struct Reference {
    best: String,
    tips: Vec<Tip>,
    set: Vec<(String, String)>,
}

impl Reference {
    fn of(node: &Node) -> Reference {
        Reference {
            best: node.best(),
            tips: node.tips(),
            set: node.utxo_set(),
        }
    }
}

/// The loop: an uncrashed dashd reference and one crash node, driven by the seeded script.
struct Loop {
    /// The profile, its ratio `B4B_RATIO` if set.
    profile: Profile,
    seed: u64,
    reference: Node,
    crash: Node,
    rng: Rng,
    model: Model,
    /// The `FORK` in progress: its blocks still to make, and the last one made (its base before
    /// the first).
    fork: Option<(u16, [u8; 32])>,
    counters: Counters,
    ops: std::fs::File,
    /// The crash node's log length when the operation in progress began.
    op_offset: u64,
    /// Last, so the nodes are gone before it is.
    _home: RetainedHome,
}

impl Loop {
    /// A loop whose crash node is rdashd on `storage`, or dashd when that is `None`.
    fn new(profile: Profile, storage: Option<Storage>) -> Loop {
        let dashd = require_dashd();
        let home = RetainedHome::new(&format!(
            "b4b-{}-{}-",
            profile.name,
            storage.map_or("dashd", Storage::name)
        ));
        let seed = std::env::var("B4B_SEED").map_or(0xb4b, |s| s.parse().expect("B4B_SEED"));
        let profile = Profile {
            ratio: std::env::var("B4B_RATIO")
                .map_or(profile.ratio, |r| r.parse().expect("B4B_RATIO")),
            ..profile
        };
        let ratio = profile.ratio;
        eprintln!("B4B {} seed={seed} ratio={ratio} home={}", profile.name, home.path().display());
        let flags = loop_flags(&profile);
        let mut reference = Node::new("reference", &dashd, home.path(), &flags);
        let mut crash = match storage {
            None => Node::new("dashd-crash", &dashd, home.path(), &flags),
            Some(storage) => Node::rdashd(home.path(), &flags, storage),
        };
        reference.start(0);
        crash.start(0);
        let genesis = genesis(&reference);
        let ops = std::fs::File::create(home.path().join("ops.log")).unwrap();
        let mut l = Loop {
            profile,
            seed,
            reference,
            crash,
            rng: Rng(seed),
            model: Model::new(genesis),
            fork: None,
            counters: Counters::default(),
            ops,
            op_offset: 0,
            _home: home,
        };
        l.log(&format!("seed {seed} ratio {ratio} profile {profile:?}"));
        l
    }

    fn log(&mut self, line: &str) {
        use std::io::Write as _;
        writeln!(self.ops, "{line}").unwrap();
    }

    /// The trunk at ratio 0, made durable, then the crash node at the profile's ratio.
    fn trunk(&mut self) {
        for _ in 0..TRUNK {
            self.extend();
        }
        self.drive(&Op::Check);
        self.crash.stop();
        self.crash.start(self.profile.ratio);
    }

    /// The reference's reply to `op`.
    fn reference_reply(&self, op: &Op) -> String {
        match op.rpc(&self.model.blocks) {
            Some((method, params)) => self.reference.call(method, &params),
            None => format!("{:?}", self.reference.utxo_set()),
        }
    }

    /// The crash node's reply to `op`, or the transport's error.
    fn crash_reply(&self, op: &Op) -> Result<String, String> {
        Ok(match op.rpc(&self.model.blocks) {
            Some((method, params)) => self.crash.try_call(method, &params)?.result().to_owned(),
            None => format!("{:?}", self.crash.try_utxo_set()?),
        })
    }

    /// One RPC on both nodes (§3.5): the reference first, then the crash node, whose reply must
    /// equal it; a crash node that dies is restarted until it has the reference's state.
    fn drive(&mut self, op: &Op) {
        let before = self.model.active;
        let want = self.reference_reply(op);
        self.op_offset = self.crash.log_len();
        self.apply(op);
        assert_eq!(self.reference.best(), shown(&self.model.active), "the model's tip after {op}");
        self.log(&format!("{op} -> {want}"));
        let reply = self.crash_reply(op);
        self.count_pressure(op);
        match reply {
            Ok(got) => {
                assert_eq!(got, want, "{}'s reply to {op}", self.crash.name);
                if self.crash.run.as_mut().unwrap().0.exited() {
                    // A death after the reply: the operation is complete. Under a ratio every
                    // rdashd flush runs inside its RPC (D14), so only dashd may.
                    self.counters.died_after_reply += 1;
                    self.log("died after its reply");
                    let log = self.crash.crashed();
                    assert_ne!(
                        self.crash.name, "rdashd",
                        "died after its reply to {op} (D14)\n{log}"
                    );
                    self.recover(None);
                }
            }
            Err(e) => {
                let reorg = reorged(&self.model, op, &before);
                if reorg {
                    self.counters.crashed_in_reorg += 1;
                }
                self.log(&format!("crashed in {op} ({e}), reorg {reorg}"));
                self.crash.crashed();
                self.recover(Some(op.clone()));
            }
        }
        self.assert_same();
    }

    /// Coins flushes begun during an operation other than `CHECK`, up to its reply or its crash
    /// (`write coins cache to disk`).
    fn count_pressure(&mut self, op: &Op) {
        if matches!(op, Op::Check) {
            return;
        }
        let log = read_since(&self.crash.log(), Some(self.op_offset));
        let flushes = log
            .lines()
            .filter(|l| l.contains("write coins cache to disk") && l.ends_with("started"))
            .count();
        self.counters.pressure_flushes += flushes as u32;
    }

    /// The model after the reference's answer to `op`.
    fn apply(&mut self, op: &Op) {
        let m = &mut self.model;
        match op {
            Op::Submit(_) | Op::Check => {}
            Op::Invalidate(r) => {
                let mut at = m.active;
                loop {
                    m.failed.insert(at);
                    if at == *r {
                        break;
                    }
                    at = m.parent(&at).unwrap();
                }
            }
            Op::Reconsider(_) => m.failed.clear(),
        }
        m.active = m.activated();
        m.assert_invariants();
    }

    /// After every operation (§3.6): the model's tip, which is the reference's, and the
    /// reference's tips.
    fn assert_same(&self) {
        assert_eq!(self.crash.best(), shown(&self.model.active), "{}'s tip", self.crash.name);
        assert_eq!(self.crash.tips(), self.reference.tips(), "{}'s tips", self.crash.name);
    }

    /// §3.5 steps 3–7 after a crash with `inflight` unanswered: restart until the crash node has
    /// the reference's tip, tips and UTXO set, re-sending an in-flight invalidation or
    /// reconsideration until it is answered. A `FORK` the crash cut is finished first: the
    /// reference takes its remaining blocks now, the crash node after its restart, so the checks
    /// see its last block's tip, the unique most-work one.
    fn recover(&mut self, mut inflight: Option<Op>) {
        let mut rest = Vec::new();
        while let Some(hash) = self.fork_next() {
            let op = Op::Submit(hash);
            let want = self.reference_reply(&op);
            self.apply(&op);
            self.log(&format!("{op} -> {want} (the FORK's rest)"));
            rest.push((hash, want));
        }
        let reference = Reference::of(&self.reference);
        assert_eq!(reference.best, shown(&self.model.active), "the model's tip");
        let deadline = Instant::now() + 10 * PATIENCE;
        loop {
            assert!(Instant::now() < deadline, "no recovery");
            self.counters.restarts += 1;
            if let Err(log) = self.crash.launch(self.profile.ratio) {
                self.counters.crashed_on_restart += 1;
                self.oracle(&log, inflight.as_ref());
                self.log("crashed on restart");
                continue;
            }
            let log = self.crash.new_log();
            self.oracle(&log, inflight.as_ref());
            let (back, forward) = (log.contains("Rolling back"), log.contains("Rolling forward"));
            if back && forward {
                self.counters.recovered_across_fork += 1;
            } else if back {
                self.counters.rolled_back += 1;
            }
            self.log(&format!("restarted: rolled back {back}, forward {forward}"));
            if self.after_restart(&mut inflight, &mut rest, &reference).is_ok() {
                return;
            }
            self.log("crashed after the restart");
            self.crash.crashed();
        }
    }

    /// Steps 4–7 on a restarted node, the `FORK`'s `rest` sent before step 5; an error is a
    /// death.
    fn after_restart(
        &mut self,
        inflight: &mut Option<Op>,
        rest: &mut Vec<([u8; 32], String)>,
        reference: &Reference,
    ) -> Result<(), String> {
        // 4: every answered block, and the one in flight, is known.
        for hash in self.model.blocks.keys().filter(|h| rest.iter().all(|(r, _)| r != *h)) {
            let reply = self.crash.try_call("getblockheader", &format!("[{}]", shown(hash)))?;
            assert_eq!(reply.status, 200, "{}: block {} lost", self.crash.name, shown(hash));
        }
        // The FORK's rest, in order, each with the reference's reply. A block leaves `rest` before
        // it is sent: in flight in a crash, it is known after the restart, as step 4 asserts.
        while !rest.is_empty() {
            let (hash, want) = rest.remove(0);
            let op = Op::Submit(hash);
            assert_eq!(self.crash_reply(&op)?, want, "{}'s reply to {op}", self.crash.name);
        }
        // 5: an in-flight invalidation or reconsideration is sent again.
        if let Some(op @ (Op::Invalidate(_) | Op::Reconsider(_))) = inflight.clone() {
            assert_eq!(self.crash_reply(&op)?, "null", "{op} again");
            *inflight = None;
        }
        // 6: the reference's tip.
        let want = &reference.best;
        let deadline = Instant::now() + PATIENCE;
        while self.crash.try_call("getbestblockhash", "[]")?.result() != want {
            assert!(Instant::now() < deadline, "{} never reached {want}", self.crash.name);
            self.crash.try_call("waitforblock", &format!("[{want}, 5000]"))?;
        }
        // 7: the UTXO set, then the tips.
        let set = self.crash.try_utxo_set()?;
        assert_eq!(set, reference.set, "{}'s UTXO set", self.crash.name);
        assert_eq!(self.crash.tips(), reference.tips, "{}'s tips", self.crash.name);
        Ok(())
    }

    /// The restart oracle (§3.6): Core's startup reconsider line only with `invalidateblock R`
    /// in flight, naming R or a failed block above it.
    fn oracle(&self, log: &str, inflight: Option<&Op>) {
        const LINE: &str = "is ahead of our tip, reconsidering";
        let Some(line) = log.lines().find(|l| l.contains(LINE)) else {
            return;
        };
        let named = line.split_once('(').and_then(|(_, rest)| rest.split_once(')')).map(|(h, _)| h);
        let allowed = match inflight {
            Some(Op::Invalidate(r)) => self
                .model
                .failed
                .iter()
                .any(|f| self.model.descends(f, r) && named == Some(rpc_hex(f).as_str())),
            _ => false,
        };
        assert!(
            allowed,
            "{}: startup reconsidered with {inflight:?} in flight: {line}\n{log}",
            self.crash.name
        );
    }

    /// One operation of the script, invariant 1 held after it (§3.4).
    fn step(&mut self) {
        self.operation();
        self.model.assert_one_best();
    }

    fn operation(&mut self) {
        match self.model.round {
            Round::Prepared {
                r,
                h,
            } => {
                assert_eq!(self.model.active, h, "only CHECK between PREPARE and INVALIDATE");
                self.invalidate(r, h);
                return;
            }
            Round::Window {
                r,
                h,
            } => {
                let extend_ok = self.model.height(&self.model.active) + 1 < self.model.height(&h);
                if extend_ok && self.rng.range(0, 1) == 0 {
                    self.extend();
                } else {
                    self.drive(&Op::Reconsider(r));
                    assert_eq!(self.model.active, h, "RECONSIDER returns to R's branch");
                    self.model.round = Round::None;
                }
                return;
            }
            Round::None => {}
        }
        for _ in 0..100 {
            match self.rng.range(0, 6) {
                0 | 1 => return self.extend(),
                2..=4 => {
                    if self.fork() {
                        return;
                    }
                }
                _ => {
                    if self.prepare() {
                        return;
                    }
                }
            }
        }
        self.extend();
    }

    /// A new block on `prev`, submitted.
    fn submit_new(&mut self, prev: [u8; 32]) -> [u8; 32] {
        let hash = self.model.make(&mut self.rng, prev, &self.profile);
        self.drive(&Op::Submit(hash));
        hash
    }

    fn extend(&mut self) {
        self.submit_new(self.model.active);
    }

    /// FORK(d): d + 1 blocks from d below the tip; the last one reorgs. A crash inside it
    /// finishes it in the recovery.
    fn fork(&mut self) -> bool {
        let d = self.rng.range(1, 6) as u16;
        let tip = self.model.active;
        let tip_height = self.model.height(&tip);
        if tip_height < TRUNK + d {
            return false;
        }
        self.fork = Some((d + 1, self.model.ancestor(&tip, tip_height - d)));
        self.log(&format!("FORK({d})"));
        while let Some(hash) = self.fork_next() {
            self.drive(&Op::Submit(hash));
        }
        let (_, last) = self.fork.take().unwrap();
        assert_eq!(self.model.active, last, "FORK({d}) reorgs");
        true
    }

    /// The `FORK`'s next block, made on its last one; `None` once all are made.
    fn fork_next(&mut self) -> Option<[u8; 32]> {
        let (left, prev) = self.fork.filter(|(left, _)| *left > 0)?;
        let hash = self.model.make(&mut self.rng, prev, &self.profile);
        self.fork = Some((left - 1, hash));
        Some(hash)
    }

    /// PREPARE(d, k): R d below the tip, and a stored replacement branch of k blocks from its
    /// parent P; refused unless R's only tip is the active one (invariant 2) and the
    /// replacement ties no valid tip and leaves one best valid tip once R is failed (invariant 1).
    fn prepare(&mut self) -> bool {
        let (d, k) = {
            let d = self.rng.range(1, 6) as u16;
            (d, self.rng.range(1, u64::from(d)) as u16)
        };
        let m = &self.model;
        let h = m.active;
        let tip_height = m.height(&h);
        if tip_height < TRUNK + d + 1 {
            return false;
        }
        let r = m.ancestor(&h, tip_height - d);
        let p = m.parent(&r).unwrap();
        let under_r: Vec<_> = m.tips().into_iter().filter(|t| m.descends(t, &r)).collect();
        if under_r != [h] {
            return false;
        }
        let replacement_height = m.height(&p) + k;
        let mut subtree = std::collections::HashSet::new();
        let mut at = h;
        while at != p {
            subtree.insert(at);
            at = m.parent(&at).unwrap();
        }
        let others = m.tips().into_iter().filter(|t| !subtree.contains(t)).collect::<Vec<_>>();
        if others.iter().any(|t| m.height(t) == replacement_height) {
            return false;
        }
        self.log(&format!("PREPARE({d}, {k}) R {}", shown(&r)));
        let mut prev = p;
        for _ in 0..k {
            prev = self.submit_new(prev);
        }
        assert_eq!(self.model.active, h, "the replacement is only stored");
        let mut failed = subtree;
        failed.extend(self.model.failed.iter().copied());
        assert!(self.model.best(&failed).is_some(), "invariant 1 after the invalidation");
        self.model.round = Round::Prepared {
            r,
            h,
        };
        true
    }

    fn invalidate(&mut self, r: [u8; 32], h: [u8; 32]) {
        let m = &self.model;
        let under_r: Vec<_> = m.tips().into_iter().filter(|t| m.descends(t, &r)).collect();
        assert_eq!(under_r, [h], "invariant 2 at INVALIDATE");
        self.drive(&Op::Invalidate(r));
        assert!(!self.model.descends(&self.model.active, &r), "off R's branch");
        self.model.round = Round::Window {
            r,
            h,
        };
    }

    /// The run: the trunk, the iterations with their checks, then the last checks and `stop`.
    fn run(mut self) -> Counters {
        self.trunk();
        for i in 0..ITERATIONS {
            let restarts = self.counters.restarts;
            self.step();
            if (i + 1) % self.profile.check_every == 0 || self.counters.restarts != restarts {
                self.drive(&Op::Check);
            }
            let tip = self.model.height(&self.model.active);
            self.log(&format!("iteration {i}: tip height {tip}, {:?}", self.counters));
        }
        while self.model.round != Round::None {
            self.step();
        }
        self.drive(&Op::Check);
        self.finish();
        self.log(&format!("{:?}", self.counters));
        let c = self.counters;
        let summary = format!(
            "B4B-SUMMARY profile={} node={} storage={} seed={} ratio={} {c:?} {}",
            self.profile.name,
            self.crash.name,
            self.crash.storage.map_or("none", Storage::name),
            self.seed,
            self.profile.ratio,
            self.batch_stats()
        );
        eprintln!("{summary}");
        c
    }

    /// `stop` on both nodes; a crash in the crash node's shutdown flush restarts it at ratio 0
    /// for one more check.
    fn finish(&mut self) {
        self.crash.call("stop", "[]");
        let (running, _) = self.crash.run.take().unwrap();
        let out = running.wait();
        let log = self.crash.new_log();
        if out.status.code() == Some(0) && log.contains(CRASH_LINE) {
            simulated_crash(&out, &log).unwrap();
            self.log("crashed in the shutdown flush");
            self.crash.start(0);
            let reference = Reference::of(&self.reference);
            self.after_restart(&mut None, &mut Vec::new(), &reference).unwrap();
            self.crash.stop();
        } else {
            assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        }
        self.reference.stop();
    }

    /// Threshold batches per flush and per recovery on each node (`Writing partial batch`
    /// between a coins flush's or replay's start and its end).
    fn batch_stats(&self) -> String {
        [&self.reference, &self.crash]
            .iter()
            .map(|n| {
                let s = BatchStats::of(&read(&n.log()));
                format!("{}: {s}", n.name)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Partial batches per coins flush and per replay in one debug log, each completed or crashed.
#[derive(Default)]
struct BatchStats {
    flushes: Vec<u32>,
    flush_crashes: Vec<u32>,
    replays: Vec<u32>,
    replay_crashes: Vec<u32>,
}

impl BatchStats {
    fn of(log: &str) -> BatchStats {
        /// What the partial batches count toward.
        enum In {
            Nothing,
            Flush(u32),
            Replay(u32),
        }
        let mut s = BatchStats::default();
        let mut at = In::Nothing;
        let close = |at: &mut In, crashed: bool, s: &mut BatchStats| match (
            std::mem::replace(at, In::Nothing),
            crashed,
        ) {
            (In::Flush(n), false) => s.flushes.push(n),
            (In::Flush(n), true) => s.flush_crashes.push(n),
            (In::Replay(n), false) => s.replays.push(n),
            (In::Replay(n), true) => s.replay_crashes.push(n),
            (In::Nothing, _) => {}
        };
        for line in log.lines() {
            if line.contains("write coins cache to disk") && line.ends_with("started") {
                close(&mut at, false, &mut s);
                at = In::Flush(0);
            } else if line.contains("Replaying blocks") {
                close(&mut at, false, &mut s);
                at = In::Replay(0);
            } else if line.contains("Writing partial batch") {
                if let In::Flush(n) | In::Replay(n) = &mut at {
                    *n += 1;
                }
            } else if line.contains(CRASH_LINE) {
                close(&mut at, true, &mut s);
            } else if line.contains("write coins cache to disk") && line.contains("completed")
                || line.contains("init message: Done loading")
            {
                close(&mut at, false, &mut s);
            }
        }
        close(&mut at, false, &mut s);
        s
    }
}

impl std::fmt::Display for BatchStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mean = |v: &[u32]| v.iter().sum::<u32>() as f64 / v.len().max(1) as f64;
        let rate = |crashed: usize, done: usize| crashed as f64 / (crashed + done).max(1) as f64;
        let split = self.flushes.iter().filter(|n| **n > 0).count();
        write!(
            f,
            "flushes {} (split {split}, partial/flush {:.2}, max {}), crashed {} (rate/split \
             flush {:.2}); replays {} (partial/replay {:.2}, max {}), crashed {} (rate {:.2})",
            self.flushes.len(),
            mean(&self.flushes),
            self.flushes.iter().max().unwrap_or(&0),
            self.flush_crashes.len(),
            rate(self.flush_crashes.len(), split),
            self.replays.len(),
            mean(&self.replays),
            self.replays.iter().max().unwrap_or(&0),
            self.replay_crashes.len(),
            rate(self.replay_crashes.len(), self.replays.len()),
        )
    }
}

/// The run reached `min` (skipped under `B4B_CALIBRATE`, which measures the counters).
fn assert_coverage(profile: &Profile, min: &Counters, c: &Counters) {
    if std::env::var_os("B4B_CALIBRATE").is_some() {
        return;
    }
    let checks = [
        ("restarts", c.restarts, min.restarts),
        ("crashed_on_restart", c.crashed_on_restart, min.crashed_on_restart),
        ("recovered_across_fork", c.recovered_across_fork, min.recovered_across_fork),
        ("crashed_in_reorg", c.crashed_in_reorg, min.crashed_in_reorg),
        ("pressure_flushes", c.pressure_flushes, min.pressure_flushes),
    ];
    for (name, got, want) in checks {
        assert!(got >= want, "{}: {name} {got} < {want} ({c:?})", profile.name);
    }
}

/// One L1 run of `profile`, rdashd or dashd crashing, to its coverage.
fn reorg_loop(profile: Profile, storage: Option<Storage>) {
    let c = Loop::new(profile, storage).run();
    assert_coverage(
        &profile,
        if storage.is_none() {
            &profile.dashd_min
        } else {
            &profile.min
        },
        &c,
    );
}

/// The storage profiles the rdashd loops run: both, the live one first. `B4B_STORAGE=livecopy`
/// or `production` selects one, as the calibration of the live minima does (BL-634 Q-5).
fn nightly_storages() -> Vec<Storage> {
    match std::env::var("B4B_STORAGE").as_deref() {
        Err(_) | Ok("both") => vec![Storage::Live, Storage::Production],
        Ok("livecopy") => vec![Storage::Live],
        Ok("production") => vec![Storage::Production],
        Ok(other) => panic!("B4B_STORAGE is livecopy, production or both, not {other:?}"),
    }
}

/// [`reorg_loop`] with rdashd crashing, on each of [`nightly_storages`]. Each runs under
/// `catch_unwind`, so the other still runs when one fails; the test fails if either does.
fn reorg_loop_on_each_storage(profile: Profile) {
    let mut failed = Vec::new();
    for storage in nightly_storages() {
        if std::panic::catch_unwind(|| reorg_loop(profile, Some(storage))).is_err() {
            failed.push(storage.name());
        }
    }
    assert!(failed.is_empty(), "{}: the loop failed on {failed:?}", profile.name);
}

/// L1, `split` (§3.2): small flushes split into many sub-batches, selected by the check's ALWAYS.
#[test]
#[ignore = "nightly: contrib/node/b4b-nightly.sh"]
fn reorg_loop_split() {
    reorg_loop_on_each_storage(SPLIT);
}

/// L1, `pressure` (§3.2): `-dbcache=4` flushes inside the reorging RPCs.
#[test]
#[ignore = "nightly: contrib/node/b4b-nightly.sh"]
fn reorg_loop_pressure() {
    reorg_loop_on_each_storage(PRESSURE);
}

/// L1 with dashd crashing: §3.6's expectations are Core's own behaviour under crashes. Dashd has
/// no storage profile, so there is one run a profile.
#[test]
#[ignore = "nightly: contrib/node/b4b-nightly.sh"]
fn reorg_loop_on_dashd() {
    for profile in [SPLIT, PRESSURE] {
        reorg_loop(profile, None);
    }
}

/// The `crashed_in_reorg` classifier: a block that extends the tip, or one only stored, is no
/// reorg; a block that wins off the old tip is, as is an invalidation that moves the tip.
#[test]
fn only_a_winning_block_off_the_old_tip_counts_as_a_reorg() {
    let (mut m, mut rng) = (Model::new([0; 32]), Rng(0));
    let mut submit = |m: &mut Model, prev: [u8; 32]| {
        let hash = m.make(&mut rng, prev, &SPLIT);
        let before = m.active;
        m.active = m.activated();
        (hash, reorged(m, &Op::Submit(hash), &before))
    };
    let (a1, reorg) = submit(&mut m, [0; 32]);
    assert!(!reorg, "the first block extends genesis");
    let (a2, reorg) = submit(&mut m, a1);
    assert!(!reorg && m.active == a2, "an extension that moves the tip");
    let (b2, reorg) = submit(&mut m, a1);
    assert!(!reorg && m.active == a2, "a FORK block only stored, a tie");
    let (b3, reorg) = submit(&mut m, b2);
    assert!(reorg && m.active == b3, "the FORK's last block wins off a2");
    assert!(!reorged(&m, &Op::Check, &a2), "CHECK never counts");
    assert!(reorged(&m, &Op::Invalidate(b2), &a2), "a moved tip");
    assert!(!reorged(&m, &Op::Reconsider(b2), &b3), "an unmoved tip");
}

/// The tips oracle compares every field of every `getchaintips` object: `branchlen` or
/// `forkpoint` alone makes two replies unequal, and the order the node lists them in does not.
#[test]
fn the_tips_oracle_compares_every_field() {
    let (a, b) = ("aa".repeat(32), "bb".repeat(32));
    let reply = |branchlen: u32, forkpoint: &str, reversed: bool| {
        let mut objects = [
            format!(
                r#"{{"height":5,"hash":"{a}","difficulty":4.656542373906925e-10,"chainwork":"0c","branchlen":0,"forkpoint":"{a}","status":"active"}}"#
            ),
            format!(
                r#"{{"height":4,"hash":"{b}","difficulty":4.656542373906925e-10,"chainwork":"0a","branchlen":{branchlen},"forkpoint":"{forkpoint}","status":"valid-fork"}}"#
            ),
        ];
        if reversed {
            objects.reverse();
        }
        tips(&format!("[{}]", objects.join(",")))
    };
    let want = reply(2, &a, false);
    assert_eq!(reply(2, &a, true), want, "the listing order");
    assert_ne!(reply(3, &a, false), want, "branchlen alone");
    assert_ne!(reply(2, &b, false), want, "forkpoint alone");
    let hash: [u8; 32] = [0xbb; 32];
    assert_eq!(status_of(&want, &hash), Some("valid-fork"));
    assert_eq!(want.iter().map(|t| t.height).collect::<Vec<_>>(), [4, 5]);
}
