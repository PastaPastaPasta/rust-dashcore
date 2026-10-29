//! The CS3 boundary rows replayed through the node against Core's recorded verdicts
//! (`data/checks_recorded.json`). The including test declares `checks` (`checks.rs`),
//! `checks_rows` (`checks_rows.rs`) and `checks_recorded` (`checks_recorded.rs`).

#![allow(dead_code, reason = "each includer uses a subset")]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::checks::{Verdict, ours};
use super::checks_recorded::{input_digest, load};
use super::checks_rows::rows;

/// Every row by name: Core's recorded verdict and the node's, computed once per test binary. The
/// recorded rows must be exactly the rows `rows()` builds, in its order.
pub fn replay_all() -> &'static BTreeMap<String, (Verdict, Verdict)> {
    static REPLAYED: OnceLock<BTreeMap<String, (Verdict, Verdict)>> = OnceLock::new();
    REPLAYED.get_or_init(replay)
}

fn replay() -> BTreeMap<String, (Verdict, Verdict)> {
    let recorded = load().rows;
    let rows = rows();
    let names = |names: Vec<&str>| names.join("\n");
    assert_eq!(
        names(recorded.iter().map(|r| r.name.as_str()).collect()),
        names(rows.iter().map(|r| r.name.as_str()).collect()),
        "the recorded rows are not rows(); regenerate with node-l1's l1_check_rows"
    );
    rows.into_iter()
        .zip(recorded)
        .map(|(row, recorded)| {
            assert_eq!(row.target, recorded.target, "{}", row.name);
            assert_eq!(
                input_digest(&row.input),
                recorded.input_sha256,
                "{}: the row's bytes are not the ones Core judged; regenerate with node-l1's l1_check_rows",
                row.name
            );
            let core = match recorded.rejection {
                None => Ok(()),
                Some(r) => Err((r.result, r.reason, r.debug_message)),
            };
            let ours = ours(row.target, &row.input)
                .unwrap_or_else(|| panic!("{}: the row does not decode", row.name));
            (row.name, (core, ours))
        })
        .collect()
}

/// Core's verdict on the row named `name`, after checking that the node's is the same.
pub fn core_verdict(name: &str) -> Verdict {
    let (core, ours) = replay_all().get(name).unwrap_or_else(|| panic!("no row named {name}"));
    assert_eq!(ours, core, "{name}");
    core.clone()
}

/// A rejection with `reason` and `debug` (`BLOCK_CONSENSUS`/`TX_CONSENSUS`, both 1).
pub fn consensus(reason: &str, debug: &str) -> Verdict {
    Err((1, reason.into(), debug.into()))
}
