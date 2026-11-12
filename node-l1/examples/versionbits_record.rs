//! Records CS4's boundary rows (`dash-consensus/tests/support/versionbits_rows.rs`) through the
//! oracle: Core's answers to each row's queries (`oracle_versionbits_state`), checked against the
//! node's as they are recorded, in `dash-consensus/tests/data/oracle/versionbits.json` (one case
//! per line; the format is `versionbits_json.rs`'s). `--check` regenerates the file in memory and
//! compares it with the one on disk instead.
//!
//! `cargo run -p node-l1 --features oracle --example versionbits_record [-- --check]`

#[path = "../../dash-consensus/tests/support/versionbits_json.rs"]
mod versionbits_json;
#[path = "../../dash-consensus/tests/support/versionbits_rows.rs"]
mod versionbits_rows;

use node_l1::versionbits::core_answers;
use node_l1::versionbits::scenario as versionbits;

const OUT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../dash-consensus/tests/data/oracle/versionbits.json");

fn main() {
    let check = std::env::args().any(|a| a == "--check");
    let mut lines = Vec::new();
    let mut differing = Vec::new();
    for (label, row) in versionbits_rows::rows() {
        let core =
            core_answers(&row).unwrap_or_else(|e| panic!("{label}: the oracle refused: {e}"));
        match row.ours() {
            Ok(ours) if ours == core => {}
            Ok(ours) => {
                let at = ours.iter().zip(&core).position(|(a, b)| a != b).unwrap_or(core.len());
                differing.push(format!("{label} (query {at})"));
            }
            Err(e) => differing.push(format!("{label} (the node refused: {e:?})")),
        }
        let mut case = versionbits_json::scenario_to_json(&row);
        case["label"] = label.into();
        case["core"] = core.iter().map(versionbits_json::answer_to_json).collect();
        lines.push(case.to_string());
    }
    assert!(differing.is_empty(), "the node differs from Core on {differing:?}");
    let text = format!("[\n{}\n]\n", lines.join(",\n"));
    if check {
        let old = std::fs::read_to_string(OUT).unwrap_or_default();
        assert!(old == text, "{OUT} differs from a fresh recording");
        println!("{} cases match", lines.len());
    } else {
        std::fs::write(OUT, text).expect("write");
        println!("{} cases written to {OUT}", lines.len());
    }
}
