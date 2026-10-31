//! LQ2 D15: "the quorum root equals CbTx over all history" on the Core-produced regtest-v24
//! chains (`/work/conformance/regtest-v24/<test>-node0`: dashd v24.0.0-rc.3 functional-test
//! datadirs). `quorum_root_history`'s replay connects each node's active chain and compares
//! `quorum_root` with every CbTx v2+ `merkleRootQuorums`. The chain options are the node's own,
//! read from its `debug.log`, and the tip is its last `UpdateTip`. Skipped when the directory is
//! absent.

#[allow(dead_code)]
#[path = "../examples/quorum_root_history.rs"]
mod quorum_root_history;

use std::collections::BTreeSet;
use std::path::Path;

use dash_coreimport::{Hash256, hash_from_hex};
use dash_params::{ChainOption, NetworkKind};
use quorum_root_history::{params_of, replay};

const CHAINS: &str = "/work/conformance/regtest-v24";

/// Per chain: the floors of the CbTx v2+ roots checked and of the non-null commitments indexed,
/// the counts of the datadirs recorded on 2026-10-08. Three chains mine only null commitments,
/// so they check the empty-root path; `rpc_netinfo`'s blocks 1-199 are the functional tests'
/// cached chain, connected before `-dip3params`, without a CbTx.
const FLOORS: [(&str, u32, u32); 5] = [
    ("feature_asset_locks", 977, 25),
    ("feature_masternode_payout_shares", 126, 0),
    ("feature_masternode_shares", 2026, 0),
    ("feature_protx_version", 387, 9),
    ("rpc_netinfo", 73, 0),
];

/// The chain options dashd ran with, as `-name=value`: each `Command-line arg` and `[regtest]`
/// `Config file arg` naming a chain option. Every start of the node (`Dash Core version`) must
/// log the same set.
fn chain_args(log: &str) -> Vec<String> {
    let mut starts: Vec<BTreeSet<String>> = Vec::new();
    for line in log.lines() {
        if line.contains("[LogPackageVersion] Dash Core version") {
            starts.push(BTreeSet::new());
            continue;
        }
        let Some(arg) = line
            .split_once("Command-line arg: ")
            .or_else(|| line.split_once("Config file arg: [regtest] "))
            .map(|(_, arg)| arg)
        else {
            continue;
        };
        let Some((name, value)) = arg.split_once('=') else {
            continue;
        };
        let name = format!("-{name}");
        if ChainOption::from_name(&name).is_some() {
            let value = value.trim().trim_matches('"');
            starts.last_mut().expect("arguments after a start").insert(format!("{name}={value}"));
        }
    }
    let first = starts.first().cloned().expect("a start");
    assert!(starts.iter().all(|s| *s == first), "the chain options changed across starts");
    first.into_iter().collect()
}

/// The node's last `UpdateTip` (it logs one on every connect and disconnect).
fn last_tip(log: &str) -> Hash256 {
    let line = log.lines().rfind(|l| l.contains("UpdateTip: new best=")).expect("an UpdateTip");
    let (_, rest) = line.split_once("new best=").expect("new best=");
    hash_from_hex(&rest[..64]).expect("a block hash")
}

#[test]
fn quorum_root_equals_cbtx_over_regtest_history() {
    if !Path::new(CHAINS).is_dir() {
        eprintln!("skipped: {CHAINS} is absent");
        return;
    }
    let (mut checked, mut nonnull, mut rotated, mut checked_nonempty) = (0, 0, 0, 0);
    for (name, min_checked, min_nonnull) in FLOORS {
        let datadir = Path::new(CHAINS).join(format!("{name}-node0/regtest"));
        let log = std::fs::read_to_string(datadir.join("debug.log")).expect("debug.log");
        let args = chain_args(&log);
        let params = params_of(NetworkKind::Regtest, &args).expect("the node's chain parameters");
        let report = replay(&params, &datadir.join("blocks"), Some(last_tip(&log)))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        eprintln!("{name} ({}): {report}", args.join(" "));
        assert_eq!(report.mismatches, [], "{name}: quorum roots differ from the CbTx");
        assert!(report.checked >= min_checked, "{name}: {} roots checked", report.checked);
        assert!(report.nonnull() >= min_nonnull, "{name}: {} commitments", report.nonnull());
        checked += report.checked;
        nonnull += report.nonnull();
        rotated += report.commitments.iter().filter(|((_, r), _)| *r).map(|(_, n)| n).sum::<u32>();
        checked_nonempty += report.checked_nonempty;
    }
    assert!(nonnull > 0 && checked_nonempty > 0, "no root covers a non-null commitment");
    // `feature_protx_version` mines LLMQ_TEST_DIP0024 (type 103) under rotation: Core logs one
    // `Built` line per rotated commitment it writes (`llmq/blockprocessor.cpp` rc.3:422-425).
    assert!(rotated >= 4, "{rotated} rotated commitments: the indexed family is not exercised");
    eprintln!(
        "{checked} roots checked, {checked_nonempty} over a non-null commitment; {nonnull} \
         non-null commitments indexed, {rotated} rotated"
    );
}
