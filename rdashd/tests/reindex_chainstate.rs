//! `-reindex-chainstate` and `-stopafterblockimport` on the node (CH5-2a) against dashd
//! v24.0.0: `feature_reindex`'s `-reindex-chainstate` cycles, the stop after the import and
//! the net caches stored empty (a `-dbcrashratio` crash in the replay is
//! `reindex_chainstate_dbcrash`'s, under test-hooks). Each case runs one rdashd and one dashd on
//! the same flags and test-built blocks (CH3b b4b's harness, `common::node`);
//! every run passes `-txindex=0`, which the flag requires on rc.5 and not on v24.0.0. They cannot
//! be skipped: with `SKIP_DASHD_TESTS` set they fail (`common::require_dashd`).
#![cfg(unix)]

mod common;

use std::process::Output;

use common::mine::*;
use common::node::*;
use common::*;

const FLAG: &str = "-reindex-chainstate";

/// The lines `-reindex-chainstate` changes that both nodes log alike: the replay's tips, and the
/// load and VerifyDB lines it skips. Left out are dashd's storage lines ("Wiping LevelDB in …",
/// "Opening LevelDB in …": rdashd has one RocksDB chainstate, CH5-notes) and its "Skipping evodb
/// repair during reindex", which rdashd logs only once EV2b serves the repair (BL-718).
const COMPARED: [&str; 5] = [
    "UpdateTip:",
    "Loaded best chain",
    "Verifying last",
    "Verification",
    "Resuming the interrupted",
];

/// dashd's line after the import under either reindex flag (BL-718).
// Core: init.cpp:2489-2490
const EVODB_REPAIR_SKIPPED: &str = "Skipping evodb repair during reindex";

/// `log`'s [`COMPARED`] lines without their time, an `UpdateTip` without its cache sizes, which
/// each node measures its own way.
fn compared(log: &str) -> Vec<String> {
    lines_of(log, &COMPARED)
        .into_iter()
        .map(|l| l.split(" cache=").next().unwrap().to_owned())
        .collect()
}

/// The two nodes, rdashd first, started at ratio 0 with `flags`.
fn pair(home: &RetainedHome, flags: &[&str]) -> [Node; 2] {
    let dashd = require_dashd();
    let mut nodes = [
        Node::rdashd(home.path(), flags, Storage::Live),
        Node::new("dashd", &dashd, home.path(), flags),
    ];
    for node in &mut nodes {
        node.start(0);
    }
    nodes
}

/// `submitblock` of `blocks` on both nodes, each answering as dashd does.
fn submit<'a>(nodes: &[Node; 2], blocks: impl IntoIterator<Item = &'a Mined>) {
    for block in blocks {
        let want = nodes[1].submit(block);
        assert_eq!(nodes[0].submit(block), want);
    }
}

/// The tip, the tips and `gettxoutsetinfo muhash`.
type State = (String, Vec<Tip>, Vec<(String, String)>);

fn state(node: &Node) -> State {
    (node.best(), node.tips(), node.utxo_set())
}

/// Both nodes' state, which must be the same.
fn same_state(nodes: &[Node; 2], step: &str) -> State {
    let [r, d] = [state(&nodes[0]), state(&nodes[1])];
    assert_eq!(r, d, "{step}: rdashd's state and dashd's");
    r
}

/// Restarts `node` at ratio 0 with `extra` for this launch and waits until its tip is `tip`.
fn restart_with(node: &mut Node, extra: &[&str], tip: &str) {
    node.stop();
    let base = node.flags.len();
    node.flags.extend(extra.iter().map(|f| (*f).to_owned()));
    node.start(0);
    node.flags.truncate(base);
    wait_until(&format!("{} back at its tip", node.name), || node.best() == tip);
}

/// `node` run to its exit with `extra`: the exit and this run's log.
fn run_to_exit(node: &Node, extra: &[&str]) -> (Output, String) {
    let offset = node.log_len();
    let flags: Vec<&str> = node.flags.iter().map(String::as_str).collect();
    let args = [&["-printtoconsole=0"][..], &flags, extra].concat();
    let home = node.datadir.parent().unwrap();
    let out = output(regtest_on(&node.binary, home, &node.datadir, &args));
    (out, read_since(&node.log(), Some(offset)))
}

/// `feature_reindex`'s `-reindex-chainstate` cycles (`feature_reindex.py:24-30`, `:91-94`), twice,
/// with a reorg between them, then a plain restart. Each cycle wipes the coins and the evo state,
/// skips the chain tip's load and VerifyDB, and reconnects every block from the block files:
/// the tip, the tips (the stale branch's too) and the UTXO set come back as they were, as on
/// dashd, with dashd's tips logged in order.
// Core: node/chainstate.cpp:214-216, :233-236, :272, :415-422; init.cpp:2031-2033;
// node/blockstorage.cpp:950-956
#[test]
fn reindex_chainstate_cycles_match_dashd_v24() {
    let home = RetainedHome::new("ch5-2a-cycles-");
    let mut nodes = pair(&home, &[]);
    let fork = Fork::new(&genesis(&nodes[1]));
    submit(&nodes, fork.f.iter().chain(&fork.a));
    let at_a104 = same_state(&nodes, "A104");
    let cycle = |nodes: &mut [Node; 2], want: &State, step: &str| {
        let mut logs = Vec::new();
        for node in nodes.iter_mut() {
            restart_with(node, &[FLAG], &want.0);
            if node.name == "dashd" {
                wait_until(EVODB_REPAIR_SKIPPED, || node.new_log().contains(EVODB_REPAIR_SKIPPED));
            }
            logs.push(node.new_log());
        }
        assert_eq!(&same_state(nodes, step), want, "{step}: as before the restart");
        let lines = [compared(&logs[0]), compared(&logs[1])];
        assert_eq!(lines[0], lines[1], "{step}: rdashd's lines and dashd's");
        let tips = lines[0].iter().filter(|l| l.starts_with("UpdateTip:")).count();
        assert_eq!(tips, want.1.iter().map(|t| t.height).max().unwrap() as usize + 1, "{step}");
        assert!(lines[0].iter().all(|l| l.starts_with("UpdateTip:")), "{step}: {:?}", lines[0]);
        assert!(!logs[0].contains(EVODB_REPAIR_SKIPPED), "BL-718 is EV2b's");
    };

    cycle(&mut nodes, &at_a104, "cycle 1");
    submit(&nodes, &fork.b);
    let at_b105 = same_state(&nodes, "B105");
    assert_eq!(at_b105.1.len(), 2, "the A branch is a tip too");
    cycle(&mut nodes, &at_b105, "cycle 2");

    // The plain restart loads the tip and verifies it, on both nodes alike.
    let mut logs = Vec::new();
    for node in &mut nodes {
        restart_with(node, &[], &at_b105.0);
        logs.push(compared(&node.new_log()));
    }
    assert_eq!(logs[0], logs[1]);
    assert!(logs[0].iter().any(|l| l.starts_with("Verifying last 6 blocks at level 3")));
    assert!(logs[0].iter().any(|l| l.starts_with("Loaded best chain")));
    assert_eq!(same_state(&nodes, "plain restart"), at_b105);
    for node in &mut nodes {
        node.stop();
    }
}

/// `-stopafterblockimport` stops after startup's ABC with Core's line, its shutdown complete.
/// Under `-reindex-chainstate` the replay outlasts init's last shutdown check on both nodes, so
/// the exit is 0; the stop may still come just before or after "Done loading", so that order is
/// not compared. Without the flag the ABC has nothing to connect and the stop races that check
/// (exit 1 before it, 0 after), so only the line and the tip are compared.
// Core: node/blockstorage.cpp:958-962; init.cpp:2570-2573; bitcoind.cpp:279-283
#[test]
fn stopafterblockimport_matches_dashd_v24() {
    const STOP: &str = "Stopping after block import";
    let home = RetainedHome::new("ch5-2a-stop-");
    let mut nodes = pair(&home, &[]);
    let fork = Fork::new(&genesis(&nodes[1]));
    submit(&nodes, fork.f.iter().chain(&fork.b));
    let want = same_state(&nodes, "B105");
    for node in &mut nodes {
        node.stop();
    }
    for extra in [&["-stopafterblockimport", FLAG][..], &["-stopafterblockimport"]] {
        for node in &mut nodes {
            let (out, log) = run_to_exit(node, extra);
            let stop = log.find(STOP).unwrap_or_else(|| panic!("{} {extra:?}: {log}", node.name));
            assert_shut_down_in_order(&log[stop..]);
            if extra.contains(&FLAG) {
                assert_eq!(out.status.code(), Some(0), "{} {extra:?}: {}", node.name, stderr(&out));
            }
            node.start(0);
            assert_eq!(node.best(), want.0, "{} {extra:?}: the tip", node.name);
        }
        assert_eq!(same_state(&nodes, &format!("{extra:?}")), want);
        for node in &mut nodes {
            node.stop();
        }
    }
}

const SUMMARIES: [&str; 2] = ["     Nodes with fulfilled requests:", "     Masternodes:"];

/// The lines of `log` about `netfulfilled.dat` and `mncache.dat`, and the managers' summaries
/// with their counts, without the time or a line's trailing duration.
// Core: flat-database.h:70, :154, :202 (the "  <n>ms" durations)
fn cache_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(|l| l.split_once(' ').map_or(l, |(_, text)| text))
        .filter(|l| {
            l.contains("netfulfilled.dat")
                || l.contains("mncache.dat")
                || SUMMARIES.iter().any(|s| l.starts_with(s))
        })
        .map(|l| without_duration(l).to_owned())
        .collect()
}

fn without_duration(line: &str) -> &str {
    line.strip_suffix("ms")
        .map(|l| l.trim_end_matches(|c: char| c.is_ascii_digit()))
        .filter(|l| l.ends_with("  "))
        .map_or(line, str::trim_end)
}

/// Under `-reindex-chainstate` the net caches are not loaded: each manager's file is verified,
/// then stored from an empty manager, as on dashd; a plain restart reads them. The managers'
/// summaries show what each holds. The files themselves are each node's own layout (SP1 D3), so
/// they are compared through those lines, not byte for byte.
// Core: init.cpp:2239-2257, flat-database.h:205-245
#[test]
fn reindex_chainstate_stores_the_net_caches_empty_as_dashd_v24() {
    let home = RetainedHome::new("ch5-2a-caches-");
    let mut nodes = pair(&home, &[]);
    let fork = Fork::new(&genesis(&nodes[1]));
    submit(&nodes, &fork.f);
    let tip = same_state(&nodes, "F101").0;
    let mut by_case = Vec::new();
    for extra in [&[FLAG][..], &[]] {
        let mut lines = Vec::new();
        for node in &mut nodes {
            restart_with(node, extra, &tip);
            node.stop();
            lines.push(cache_lines(&node.new_log()));
            node.start(0);
        }
        assert_eq!(lines[0], lines[1], "{extra:?}: rdashd's cache lines and dashd's");
        by_case.push(lines.swap_remove(0));
    }
    let [flagged, plain] = [&by_case[0], &by_case[1]];
    assert!(flagged.contains(&"Verifying netfulfilled.dat format...".to_owned()), "{flagged:?}");
    assert!(flagged.contains(&"Writing info to mncache.dat...".to_owned()), "{flagged:?}");
    assert!(!flagged.iter().any(|l| l.starts_with("Reading info from")), "{flagged:?}");
    for summary in SUMMARIES {
        let held: Vec<_> = flagged.iter().filter(|l| l.starts_with(summary)).collect();
        assert!(!held.is_empty(), "{summary:?} in {flagged:?}");
        for line in held {
            let counts: Vec<_> =
                line.split(|c: char| !c.is_ascii_digit()).filter(|n| !n.is_empty()).collect();
            assert!(!counts.is_empty() && counts.iter().all(|n| *n == "0"), "stored empty: {line}");
        }
    }
    assert!(plain.contains(&"Reading info from netfulfilled.dat...".to_owned()), "{plain:?}");
    assert!(plain.contains(&"Reading info from mncache.dat...".to_owned()), "{plain:?}");
    for node in &mut nodes {
        node.stop();
    }
}
