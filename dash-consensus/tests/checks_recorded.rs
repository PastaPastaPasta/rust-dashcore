//! The CS3 boundary rows (`support/checks.rs`, `rows()`: every check's edges and quirks 2, 3, 9
//! and 10) through the node, against Dash Core's verdicts recorded through the oracle
//! (`data/checks_recorded.json`, written by node-l1's `l1_check_rows`): the same verdict, result
//! class, reject reason and debug message.

#[path = "support/checks.rs"]
mod checks;
#[path = "support/checks_recorded.rs"]
mod checks_recorded;
#[path = "support/checks_replay.rs"]
mod checks_replay;
#[path = "support/checks_rows.rs"]
mod checks_rows;

#[test]
fn rows_reproduce_cores_verdicts() {
    let replayed = checks_replay::replay_all();
    let differing: Vec<&String> =
        replayed.iter().filter(|(_, (core, ours))| core != ours).map(|(name, _)| name).collect();
    assert!(differing.is_empty(), "the node differs from Core on {differing:?}");
    let accepted = replayed.values().filter(|(core, _)| core.is_ok()).count();
    assert!(accepted > 20 && replayed.len() - accepted > 20, "{accepted} of {}", replayed.len());
}
