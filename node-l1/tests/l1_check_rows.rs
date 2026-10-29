//! The CS3 boundary rows (`checks_rows.rs`: the rules' edges and quirks 2, 3, 9 and 10)
//! against Dash Core v24's oracle: every row is compared like a fuzz input and must get a verdict
//! (not fall outside the oracle's domain), and Core's verdicts are recorded in
//! `dash-consensus/tests/data/checks_recorded.json`, which dash-consensus's tests replay through
//! the node without the oracle.
//!
//! Regenerate with `NODE_L1_REGENERATE=1 cargo test -p node-l1 --features oracle --test
//! l1_check_rows`, then review the diff.

#[path = "../../dash-consensus/tests/support/checks_recorded.rs"]
mod checks_recorded;
#[path = "../../dash-consensus/tests/support/checks_rows.rs"]
mod checks_rows;

use std::collections::BTreeSet;

use dash_oracle_sys::{Oracle, Pin, Verdict};
use node_l1::Outcome;
use node_l1::check_inputs as checks;
use node_l1::checks::{CoreCheck, core_check, result_code};

use checks::Input;
use checks_recorded::{Recorded, RecordedRejection, RecordedRow, input_digest};
use checks_rows::rows;

fn compute() -> Recorded {
    let rows = rows()
        .into_iter()
        .map(|row| {
            let input = Input::parse(row.target, &row.input).expect("a row parses");
            let verdict = match core_check(&input) {
                Ok(CoreCheck::Verdict(verdict)) => verdict,
                _ => panic!("{}: Core gives no verdict", row.name),
            };
            RecordedRow {
                target: row.target.into(),
                input_sha256: input_digest(&row.input),
                name: row.name,
                rejection: match verdict {
                    Verdict::Accept => None,
                    Verdict::Reject(r) => Some(RecordedRejection {
                        result: result_code(&r.result).expect("a check's result class"),
                        reason: r.reason,
                        debug_message: r.debug_message,
                    }),
                },
            }
        })
        .collect();
    Recorded {
        provenance: format!(
            "Dash Core's verdicts on the CS3 boundary rows (dash-consensus/tests/support/checks.rs, rows()), computed by the oracle (dash-oracle-sys) at pin {}; regenerate with node-l1's l1_check_rows test.",
            Oracle::new(Pin::V24_0).core_version()
        ),
        rows,
    }
}

#[test]
fn rows_agree_with_core() {
    let mut names = BTreeSet::new();
    let mut failures = Vec::new();
    for row in rows() {
        assert!(names.insert(row.name.clone()), "{}: a duplicate name", row.name);
        let target = node_l1::target(row.target).expect("a registered target");
        match (target.compare)(&row.input, Pin::V24_0) {
            Ok(Outcome::BothAccept | Outcome::BothReject) => {}
            Ok(Outcome::OutOfDomain | Outcome::Discarded) => {
                failures.push(format!("{}: not compared", row.name))
            }
            Ok(Outcome::StrictFlag) => failures.push(format!("{}: a strict flag", row.name)),
            Err(divergence) => failures.push(format!("{}: {divergence}", row.name)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The committed verdicts must equal the oracle's. The provenance line names the oracle build,
/// so it changes with every shim revision and is not compared.
#[test]
fn recorded_rows_match_the_oracle() {
    let computed = compute();
    if std::env::var_os("NODE_L1_REGENERATE").is_some() {
        let json = serde_json::to_string_pretty(&computed).expect("serializes") + "\n";
        std::fs::write(checks_recorded::path(), json).expect("writes the fixtures");
        return;
    }
    let mut committed = checks_recorded::load();
    committed.provenance.clone_from(&computed.provenance);
    assert!(
        committed == computed,
        "the committed verdicts differ from the oracle; regenerate and review"
    );
}
