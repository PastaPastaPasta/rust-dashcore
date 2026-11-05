//! The EV1-J harness both dashd differentials share (`ev1_j_dashd` for J0a, J0b-1, J0b-2 and J2;
//! `ev1_j_d14_dashd` for J0b-3): the rows file as data and its checks against the plan's
//! inventory and EV1 §6.3's closure, the two validators' verdicts on a probe block, and the stage
//! judge. `ev1_j_dashd`'s module docs say what is compared and why.

// Each test binary uses part of it.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{
    Header, Transaction, TxOut, core_serialize, decode_block, decode_tx_exact,
};
use dash_consensus::{Block, BlockValidationResult, Tx};
use dash_primitives::{BlockHash, TxType};
use dashd_chainstate::chain::{BlockState, ValidationInterface};
use dashd_chainstate::evo::cl::census_under;
use dashd_mining::{Grind, grind};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use serde_json::{Value, json};

use crate::ev1_j;
use crate::v24::{self, Dashd, deployment_json, hex};

/// The setup chain's parameters, on all three nodes (see the module docs).
pub const CHAIN_ARGS: [&str; 5] = [
    "-dip3params=48:48",
    "-testactivationheight=dip0024@1000",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-vbparams=v24:0:999999999999:150:10:8:6:5:0",
];
/// The stages this head runs (EV1-J plan rev4 §3 and its J2 delta; J0b's and J2's sub-stages,
/// [`run_stage`]).
pub const REACHED: [&str; 7] = ["J0a", "J0b-1", "J0b-2", "J0b-3", "J2-C", "J2-R2", "J2-lib"];
/// J0b's sub-stages: J0b-1 the quorum fixture's block rows, J0b-2 the assumevalid fixture's BAV
/// rows (§5.2), J0b-3 D14's devnet. The rows file and [`INVENTORY`] name them all J0b.
pub const SUB_STAGES: [&str; 3] = ["J0b-1", "J0b-2", "J0b-3"];
/// J2's sub-stages (the plan's J2 delta, regtest only): J2-C the L2-C block rows on the ChainLock
/// fixture (`ev1_j::l2`), J2-R2 the stored recordings re-judged by dashd rc.5, J2-lib the rows
/// that run in CH7a-5b's replay, `dashd-node`'s lib tests ([`LIB_WIRED`]). The rows file names
/// them all J2.
pub const J2_SUB_STAGES: [&str; 3] = ["J2-C", "J2-R2", "J2-lib"];
/// The L2 rows CH7a-5b's replay runs (`dashd-node`'s lib `sut::tests::recorded`, merged as
/// a32d095cb), each naming its tests in `l2_tests` ([`lib_problems`]).
pub const LIB_WIRED: [&str; 2] = ["L2-R1", "L2-R3"];
/// The lib test module the J2-lib rows' tests live in.
pub const RECORDED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/sut/tests/recorded.rs");
pub const SUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/sut.rs");
/// L2-R1's census there, a decode of the recordings that runs no replay.
pub const L2_CENSUS: &str = "l2_census_pins_every_type_7_and_9_tx_of_the_recordings";
/// The rows J2-R2's test (`j2_recordings_rejudged_by_dashd`) runs: the rows at J2-R2, exactly.
pub const R2_WIRED: [&str; 1] = ["L2-R2"];
/// The stage of deferrals: evidence another item delivers, never run here and never passed.
pub const DEFERRED_STAGE: &str = "J3";
pub const DEFERRED: &str = "deferred: ";
/// The rows outside J0b-1 whose type 9 reject needs J-D8's pool control, as a J0b-1 type 9 row
/// does: an L2-C row, and D14's unlock under the quorum one past the scan.
pub const POOL_ROWS: [&str; 2] = ["L2-C3.9", "D14-34"];
/// The rows whose block reject needs an age control (`b_age_control`, accept): the probe's own
/// bytes, judged where the quorum it names is still in the scan, which shows that the probe's
/// quorum, key, signature, height window and pool are all good and only its age answers. D14-34's
/// is its unlock in a block on the setup chain's tip before B's commitments, on validators of that
/// tip.
pub const AGE_ROWS: [&str; 1] = ["D14-34"];
/// The reject of a type 9 row's pool control: the valid unlock in the probe's place on the
/// unchanged template, whose CbTx does not count it (`evo/specialtxman.cpp:1114`).
pub const POOL_REJECT: &str = "bad-cbtx-assetlocked-amount";
/// The reject of a BAV row's positive twin: its mode control with the scripts checked, whose
/// spend of a P2PKH output with an empty scriptSig fails at `OP_DUP` (`script/interpreter.cpp:703-704`,
/// `script/script_error.cpp:46-47`), reported inline at `-par=1` (`validation.cpp:2234`, `:2763-2765`).
pub const SCRIPT_REJECT: &str =
    "mandatory-script-verify-flag-failed (Operation not valid with the current stack size)";
pub const ROWS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-j-rows.toml");
pub const PAIRS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-pairs.toml");
pub const COIN: i64 = 100_000_000;
/// A quorum hash no node knows.
pub const UNKNOWN: [u8; 32] = [0x42; 32];
/// Regtest's evo_shares: in its window and an EHF deployment (chainparams.cpp:861-868).
pub const EHF_BIT: u8 = 14;
// ---- rows as data -------------------------------------------------------------------------

/// One `[[row]]` of the rows file: its string fields, its `covers` list, its `excludes` list
/// (closure items the row records as unobservable, with the reason in `notes`), and its
/// `l2_tests` list (a J2-lib row's lib tests, [`lib_problems`]).
#[derive(Debug, Default)]
pub struct Row {
    pub fields: BTreeMap<String, String>,
    pub covers: Vec<String>,
    pub excludes: Vec<String>,
    pub l2_tests: Vec<String>,
}

impl Row {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn id(&self) -> &str {
        self.get("id").expect("every row has an id")
    }
}

pub const PATHS: [&str; 4] = ["b", "bav", "m", "l2"];
/// The block-path outcome keys: each reject among them has `<key>_class`.
pub const BLOCK_OUTCOMES: [&str; 9] = [
    "b",
    "bav",
    "b_control",
    "b_order",
    "b_order_control",
    "b_pool_control",
    "b_age_control",
    "bav_mode_control",
    "bav_twin",
];
/// The keys of a run row's block probes: its own case and each control.
pub const PROBE_KEYS: [&str; 6] =
    ["b", "b_control", "b_order", "b_order_control", "b_pool_control", "b_age_control"];
/// The keys of a run row's BAV probes: its own case below assumevalid, its mode control and the
/// control's positive twin.
pub const BAV_PROBE_KEYS: [&str; 3] = ["bav", "bav_mode_control", "bav_twin"];
/// The rows whose stage evidence is an order probe and an order control (D13's two block rows:
/// both of their checks give `bad-shared-collateral-spend`, so only a second defect shows which
/// stage answers).
pub const STAGE_ROWS: [&str; 2] = ["D13-B", "D13-B2"];
/// The recorded exclusions of plan rev4 §4, with their rows: an item a row lists in `excludes`,
/// or covers while none of its paths applies (9-X unreachable, 9-P2 infeasible). The rows file must
/// record exactly these.
pub const EXCLUSIONS: [(&str, &str); 5] = [
    ("9c:13", "9-X"),
    ("9c:3>9c:13", "9-X"),
    ("9c:5>9c:13", "9-X"),
    ("9c:8>9c:11", "9-P2"),
    ("D13 covenant block view", "D13-B"),
];
pub const KEYS: [&str; 36] = [
    "id",
    "case",
    "recipe",
    "notes",
    "control",
    "b_control",
    "b_control_class",
    "order_probe",
    "b_order",
    "b_order_class",
    "order_control",
    "b_order_control",
    "b_order_control_class",
    "pool_control",
    "b_pool_control",
    "b_pool_control_class",
    "b_age_control",
    "b_age_control_class",
    "b",
    "b_probes",
    "b_stage",
    "b_class",
    "bav",
    "bav_probes",
    "bav_stage",
    "bav_class",
    "mode_control",
    "bav_mode_control",
    "twin",
    "bav_twin",
    "bav_twin_class",
    "m",
    "m_stage",
    "l2",
    "l2_stage",
    "l2_probes",
];
pub const STAGES: [&str; 5] = ["J0a", "J0b", "J1", "J2", "J3"];
/// The prefixes of a path that does not apply.
pub const NOT_APPLICABLE: [&str; 3] = ["n/a: ", "excluded: ", "infeasible: "];

/// The EV1-J plan rev4 §4 inventory, independent of the rows file: each row, and for each of
/// [`PATHS`] (b, bav, m, l2) either `""` (the row has no such path), `"<stage> <outcome>"`, or
/// `"<n/a|excluded|infeasible>: <words of its reason>"`. §4's assumevalid "—" is `n/a`: BAV runs the
/// bad-signature rows only (§5.2). 9-P5a and 9-P5b are split per second defect as §4's stage
/// column splits them. The L2 rows are the J2 delta's §3 (OD-CP-73), L2-C3 and L2-C4 split per
/// type as its outcome column splits them (`.7`, `.9`); the testnet and mainnet rows are deferrals
/// at J3.
pub const INVENTORY: &[(&str, [&str; 4])] = &[
    // Type 7.
    ("7-OK", ["J0b accept", "J0b accept", "J1 accept", ""]),
    ("7-S1", ["J0a bad-mnhf-non-ehf", BAV_NA, "J1 bad-mnhf-non-ehf", ""]),
    ("7-S2", ["J0b bad-mnhf-quorum-hash", BAV_NA, "J1 bad-mnhf-quorum-hash", ""]),
    ("7-S3", ["J0b bad-mnhf-quorum-hash", BAV_NA, "J1 bad-mnhf-quorum-hash", ""]),
    ("7-S4", ["J0b bad-mnhf-missing-quorum", BAV_NA, "J1 bad-mnhf-missing-quorum", ""]),
    ("7-S5", ["J0b bad-mnhf-invalid", "J0b bad-mnhf-invalid", "J1 bad-mnhf-invalid", ""]),
    ("7-P1", ["J0a bad-mnhf-non-ehf", BAV_NA, "J1 bad-mnhf-non-ehf", ""]),
    ("7-P2", ["J0b bad-mnhf-quorum-hash", BAV_NA, "J1 bad-mnhf-quorum-hash", ""]),
    ("7-P3", ["J0b bad-mnhf-quorum-hash", BAV_NA, "J1 bad-mnhf-quorum-hash", ""]),
    ("7-P4", ["J0b bad-mnhf-missing-quorum", BAV_NA, "J1 bad-mnhf-missing-quorum", ""]),
    // Type 9.
    ("9-OK", ["J0b accept", "J0b accept", "J1 accept", ""]),
    ("9-S1", ["J0b bad-assetunlock-duplicated-index", BAV_NA, "n/a: txn-already-known", ""]),
    ("9-S2", ["J0b bad-assetunlock-quorum-hash", BAV_NA, "J1 bad-assetunlock-quorum-hash", ""]),
    (
        "9-S3a",
        ["J0b bad-assetunlock-too-old-quorum", BAV_NA, "J1 bad-assetunlock-too-old-quorum", ""],
    ),
    (
        "9-S3b",
        ["J0b bad-assetunlock-too-old-quorum", BAV_NA, "J1 bad-assetunlock-too-old-quorum", ""],
    ),
    ("9-S4a", ["J0b bad-assetunlock-too-late", BAV_NA, "J1 bad-assetunlock-too-late", ""]),
    ("9-S4b", ["J0b bad-assetunlock-too-late", BAV_NA, "J1 bad-assetunlock-too-late", ""]),
    (
        "9-S5",
        [
            "J0b bad-assetunlock-not-verified",
            "J0b bad-assetunlock-not-verified",
            "J1 bad-assetunlock-not-verified",
            "",
        ],
    ),
    ("9-X", ["excluded: unreachable on a consistent node", "excluded: as b", "excluded: as b", ""]),
    ("9-P1", ["J0b bad-assetunlock-duplicated-index", BAV_NA, "n/a: as 9-S1", ""]),
    ("9-P2", ["infeasible: one quorumHash field", "infeasible: as b", "infeasible: as b", ""]),
    (
        "9-P3",
        ["J0b bad-assetunlock-too-old-quorum", BAV_NA, "J1 bad-assetunlock-too-old-quorum", ""],
    ),
    ("9-P4", ["J0b bad-assetunlock-too-late", BAV_NA, "J1 bad-assetunlock-too-late", ""]),
    ("9-P5a.1", ["J0a bad-assetunlocktx-version", BAV_NA, "J1 bad-assetunlocktx-version", ""]),
    ("9-P5a.2", ["J0a bad-assetunlocktx-version", BAV_NA, "J1 bad-assetunlocktx-version", ""]),
    ("9-P5a.3", ["J0b bad-assetunlocktx-version", BAV_NA, "J1 bad-assetunlocktx-version", ""]),
    ("9-P5a.4", ["J0b bad-assetunlocktx-version", BAV_NA, "J1 bad-assetunlocktx-version", ""]),
    ("9-P5b.1", ["J0a bad-assetunlocktx-too-many-outs", BAV_NA, TOO_MANY_OUTS_M, ""]),
    ("9-P5b.2", ["J0a bad-assetunlocktx-too-many-outs", BAV_NA, TOO_MANY_OUTS_M, ""]),
    ("9-P5b.3", ["J0b bad-assetunlocktx-too-many-outs", BAV_NA, TOO_MANY_OUTS_M, ""]),
    ("9-P5b.4", ["J0b bad-assetunlocktx-too-many-outs", BAV_NA, TOO_MANY_OUTS_M, ""]),
    ("9-P5c", ["J0b bad-assetunlocktx-version", BAV_NA, "n/a: txn-already-known", ""]),
    ("9-P5d", ["J0b bad-assetunlocktx-too-many-outs", BAV_NA, "n/a: txn-already-known", ""]),
    // Type 9 version 2 expired-instance retry.
    ("9-R1", ["J0b bad-assetunlock-too-late", BAV_NA, "J1 accept", ""]),
    ("9-R2", ["J0b bad-assetunlock-too-late", BAV_NA, "J1 accept", ""]),
    (
        "9-R3",
        [
            "J0b bad-assetunlock-too-late",
            BAV_NA,
            "J1 Core's reject from the last retry actually run, compared exactly",
            "",
        ],
    ),
    // D13.
    (
        "D13-B",
        ["J0a bad-shared-collateral-spend", "n/a: no signature row", "n/a: D13-M1 and D13-M2", ""],
    ),
    ("D13-B2", ["J0a bad-shared-collateral-spend", "n/a: no signature row", "n/a: D13-M1", ""]),
    (
        "D13-M1",
        ["n/a: D13-B and D13-B2", "n/a: D13-B and D13-B2", "J1 bad-shared-collateral-spend", ""],
    ),
    ("D13-M2", ["n/a: D13-B and D13-B2", "n/a: D13-B and D13-B2", "J1 bad-protx-collateral", ""]),
    // D14.
    ("D14-33", ["J0b accept", "n/a: no signature row", "J1 accept", ""]),
    (
        "D14-34",
        [
            "J0b bad-assetunlock-too-old-quorum",
            "n/a: no signature row",
            "J1 bad-assetunlock-too-old-quorum",
            "",
        ],
    ),
    // The operator-signature first-reject row (§3 J0a).
    ("OPSIG-B", ["J0a bad-protx-sig", "n/a: check_sigs", "n/a: block-path property", ""]),
    // L2 replay, J2 (regtest only).
    ("L2-R1", ["", "", "", "J2 accept"]),
    ("L2-R2", ["", "", "", "J2 accept"]),
    ("L2-R3", ["", "", "", "J2 equal"]),
    ("L2-C1", ["J2 accept", L2C_BAV, L2C_M, ""]),
    ("L2-C2", ["J2 bad-cbtx-invalid-clsig", L2C_BAV, L2C_M, ""]),
    ("L2-C3.7", ["J2 bad-mnhf-invalid", L2C_BAV, L2C_M, ""]),
    ("L2-C3.9", ["J2 bad-assetunlock-not-verified", L2C_BAV, L2C_M, ""]),
    ("L2-C4.7", ["J2 bad-mnhf-invalid", L2C_BAV, L2C_M, ""]),
    ("L2-C4.9", ["J2 bad-assetunlock-not-verified", L2C_BAV, L2C_M, ""]),
    ("L2-testnet", ["", "", "", "J3 deferred: CH7b-3a-2 (DEC-1082)"]),
    ("L2-mainnet", ["", "", "", "J3 deferred: E-O8-1"]),
];
pub const BAV_NA: &str = "n/a: bad-signature rows only";
pub const TOO_MANY_OUTS_M: &str = "J1 bad-assetunlocktx-too-many-outs";
/// The L2-C rows' BAV and M paths (the J2 delta §3).
pub const L2C_BAV: &str = "n/a: steps 8–10 run only with fCheckCbTxMerkleRoots";
pub const L2C_M: &str = "n/a: block-path property";

/// `text` in the restricted shape (D29): blank lines, comments, `[[row]]`, `key = "string"`, and
/// `covers = [`, `excludes = [` or `l2_tests = [` with one `"string",` per line up to `]`.
pub fn parse_rows(text: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut list: Option<String> = None;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = &list {
            if line == "]" {
                list = None;
            } else {
                let item = line.strip_prefix("  \"").and_then(|l| l.strip_suffix("\","));
                let item = item.unwrap_or_else(|| panic!("a list item: {line}")).to_owned();
                let row = rows.last_mut().expect("in a row");
                match name.as_str() {
                    "covers" => row.covers.push(item),
                    "excludes" => row.excludes.push(item),
                    "l2_tests" => row.l2_tests.push(item),
                    _ => panic!("the lists are covers, excludes and l2_tests: {name}"),
                }
            }
        } else if line == "[[row]]" {
            rows.push(Row::default());
        } else if let Some(key) = line.strip_suffix(" = [") {
            list = Some(key.to_owned());
        } else {
            let (key, value) = line.split_once(" = \"").unwrap_or_else(|| panic!("{line}"));
            let value = value.strip_suffix('"').unwrap_or_else(|| panic!("{line}"));
            let row = rows.last_mut().expect("in a row");
            assert!(row.fields.insert(key.to_owned(), value.to_owned()).is_none(), "{line}");
        }
    }
    assert!(list.is_none(), "an unclosed list");
    rows
}

/// Whether `value` is a reject reason Core prints: a lowercase word list, with a script check's
/// error text in parentheses after it (`validation.cpp:2223`, `:2234`), never empty and with its
/// own parentheses balanced (`script_error.cpp`'s "CHECK(MULTI)SIG").
pub fn is_reason(value: &str) -> bool {
    let (reason, text) = match value.split_once(" (") {
        Some((reason, text)) => {
            (reason, text.strip_suffix(')').filter(|t| !t.is_empty() && balanced(t)))
        }
        None => (value, Some("")),
    };
    !reason.is_empty()
        && reason.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && text.is_some_and(|t| t.bytes().all(|b| b.is_ascii_graphic() && b != b'"' || b == b' '))
}

/// Whether every `)` in `text` closes an earlier `(` and every `(` is closed.
pub fn balanced(text: &str) -> bool {
    text.chars().try_fold(0_usize, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' => depth.checked_sub(1),
        _ => Some(depth),
    }) == Some(0)
}

pub fn is_reject(value: &str) -> bool {
    value != "accept" && is_reason(value)
}

pub fn not_applicable(value: &str) -> bool {
    NOT_APPLICABLE.iter().any(|p| value.starts_with(p))
}

/// The stage that runs `path` of `row`: its `<path>_stage`, with J0b split into
/// [`SUB_STAGES`] (BAV paths J0b-2, D14's rows J0b-3, every other J0b path J0b-1) and J2 into
/// [`J2_SUB_STAGES`] (block paths J2-C, the rows CH7a-5b runs J2-lib, every other L2 path J2-R2).
pub fn run_stage<'a>(row: &'a Row, path: &str) -> Option<&'a str> {
    let stage = row.get(&format!("{path}_stage"))?;
    Some(match (stage, path) {
        ("J0b", "bav") => SUB_STAGES[1],
        ("J0b", _) if row.id().starts_with("D14-") => SUB_STAGES[2],
        ("J0b", _) => SUB_STAGES[0],
        ("J2", "l2") if LIB_WIRED.contains(&row.id()) => J2_SUB_STAGES[2],
        ("J2", "l2") => J2_SUB_STAGES[1],
        ("J2", _) => J2_SUB_STAGES[0],
        (stage, _) => stage,
    })
}

/// The class of a `<key>_class` value, `<Core's BlockValidationResult>: <file:line cite>`.
pub fn class_of(value: &str) -> Option<BlockValidationResult> {
    let (name, cite) = value.split_once(": ")?;
    if !cite.contains(".cpp:") {
        return None;
    }
    Some(match name {
        "BLOCK_RESULT_UNSET" => BlockValidationResult::Unset,
        "BLOCK_CONSENSUS" => BlockValidationResult::Consensus,
        "BLOCK_RECENT_CONSENSUS_CHANGE" => BlockValidationResult::RecentConsensusChange,
        "BLOCK_CACHED_INVALID" => BlockValidationResult::CachedInvalid,
        "BLOCK_INVALID_HEADER" => BlockValidationResult::InvalidHeader,
        "BLOCK_MUTATED" => BlockValidationResult::Mutated,
        "BLOCK_MISSING_PREV" => BlockValidationResult::MissingPrev,
        "BLOCK_INVALID_PREV" => BlockValidationResult::InvalidPrev,
        "BLOCK_TIME_FUTURE" => BlockValidationResult::TimeFuture,
        "BLOCK_CHECKPOINT" => BlockValidationResult::Checkpoint,
        "BLOCK_CHAINLOCK" => BlockValidationResult::ChainLock,
        _ => return None,
    })
}

/// What the rows' shape gets wrong: unknown keys, paths without stages or with stages they
/// cannot have, unknown stages, run rows without a control, and block-path rejects without a
/// cited class.
pub fn row_problems(rows: &[Row]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row.id();
        if !ids.insert(id) {
            problems.push(format!("{id}: a second row with this id"));
        }
        for key in row.fields.keys() {
            if !KEYS.contains(&key.as_str()) {
                problems.push(format!("{id}: unknown key {key}"));
            }
        }
        for key in ["case", "recipe"] {
            if row.get(key).is_none() {
                problems.push(format!("{id}: no {key}"));
            }
        }
        let mut outcomes = 0;
        for path in PATHS {
            let Some(value) = row.get(path) else {
                continue;
            };
            outcomes += 1;
            let stage = row.get(&format!("{path}_stage"));
            if value.starts_with(DEFERRED) != (stage == Some(DEFERRED_STAGE)) {
                problems.push(format!(
                    "{id}: {path} {value} at {}: a deferral is {DEFERRED_STAGE}'s, and only it",
                    stage.unwrap_or("no stage")
                ));
                continue;
            }
            match (not_applicable(value), stage) {
                (true, Some(_)) => {
                    problems.push(format!("{id}: {path} does not apply but has a stage"))
                }
                (false, None) => problems.push(format!("{id}: {path} has no stage")),
                (false, Some(stage)) if !STAGES.contains(&stage) => {
                    problems.push(format!("{id}: {path} stage {stage}"))
                }
                (false, Some(_)) if run_stage(row, path).is_some_and(|s| REACHED.contains(&s)) => {
                    if value != "accept" && !is_reason(value) {
                        problems.push(format!("{id}: {path} runs, and {value} is no outcome"));
                    }
                    // The row's own probe and a control on b; on bav also the control's twin; on l2
                    // the recordings it re-judges.
                    let least = match path {
                        "b" => 2,
                        "bav" => 3,
                        "l2" => 1,
                        _ => {
                            problems.push(format!("{id}: this head runs no {path} path"));
                            continue;
                        }
                    };
                    if !row
                        .get(&format!("{path}_probes"))
                        .and_then(|n| n.parse::<usize>().ok())
                        .is_some_and(|n| n >= least)
                    {
                        problems
                            .push(format!("{id}: a run row pins its probe count in {path}_probes"));
                    }
                    if path == "l2" {
                        continue;
                    }
                    if path == "bav" {
                        if row.get("mode_control").is_none()
                            || row.get("bav_mode_control") != Some("accept")
                        {
                            problems.push(format!(
                                "{id}: a BAV run row needs its mode control, accept"
                            ));
                        }
                        if row.get("twin").is_none() || row.get("bav_twin") != Some(SCRIPT_REJECT) {
                            problems.push(format!(
                                "{id}: a BAV run row needs its positive twin, {SCRIPT_REJECT}"
                            ));
                        }
                        continue;
                    }
                    let control = row.get("b_control");
                    if row.get("control").is_none()
                        || !control.is_some_and(|c| c == "accept" || is_reason(c))
                    {
                        problems.push(format!("{id}: a run row needs control and b_control"));
                    }
                    let pool = [row.get("b_control"), row.get("b_pool_control")];
                    let unlock_row = (run_stage(row, path) == Some(SUB_STAGES[0])
                        && id.starts_with("9-"))
                        || POOL_ROWS.contains(&id);
                    if unlock_row && is_reject(value) && !pool.contains(&Some(POOL_REJECT)) {
                        problems.push(format!("{id}: a type 9 reject needs its pool control"));
                    }
                    if AGE_ROWS.contains(&id) && row.get("b_age_control") != Some("accept") {
                        problems.push(format!("{id}: an age row needs its age control, accept"));
                    }
                }
                _ => {}
            }
        }
        if outcomes == 0 {
            problems.push(format!("{id}: no path"));
        }
        if STAGE_ROWS.contains(&id)
            && (row.get("b_order").is_none() || row.get("b_order_control").is_none())
        {
            problems.push(format!("{id}: a stage row needs b_order and b_order_control"));
        }
        for (what, outcome) in [
            ("control", "b_control"),
            ("order_probe", "b_order"),
            ("order_control", "b_order_control"),
            ("pool_control", "b_pool_control"),
            ("mode_control", "bav_mode_control"),
            ("twin", "bav_twin"),
        ] {
            if row.get(what).is_some() != row.get(outcome).is_some() {
                problems.push(format!("{id}: {what} and {outcome} go together"));
            }
        }
        if row.get("b_age_control").is_some() && !AGE_ROWS.contains(&id) {
            problems.push(format!("{id}: b_age_control on a row that is no age row"));
        }
        if let Some(pool) = row.get("b_pool_control").filter(|c| *c != POOL_REJECT) {
            problems
                .push(format!("{id}: b_pool_control {pool}, a pool control's is {POOL_REJECT}"));
        }
        for key in BLOCK_OUTCOMES {
            let class_key = format!("{key}_class");
            let class = row.get(&class_key);
            match (row.get(key).is_some_and(is_reject), class) {
                (true, None) => {
                    problems.push(format!("{id}: {key} is a reject without {class_key}"))
                }
                (true, Some(class)) if class_of(class).is_none() => {
                    problems.push(format!("{id}: {class_key} {class} is no cited class"))
                }
                (false, Some(_)) => problems.push(format!("{id}: {class_key} without a reject")),
                _ => {}
            }
        }
        if !row.excludes.is_empty() && row.get("notes").is_none() {
            problems.push(format!("{id}: excludes without notes"));
        }
        for key in ["b_stage", "bav_stage", "m_stage", "l2_stage"] {
            if row.get(key).is_some() && row.get(&key[..key.len() - 6]).is_none() {
                problems.push(format!("{id}: {key} without its path"));
            }
        }
        for path in ["b", "bav", "l2"] {
            let key = format!("{path}_probes");
            let runs = run_stage(row, path).is_some_and(|s| REACHED.contains(&s))
                && row.get(path).is_some_and(|v| !not_applicable(v));
            if row.get(&key).is_some() && !runs {
                problems.push(format!("{id}: {key} on a path this head does not run"));
            }
        }
    }
    problems
}

/// The rows at J2-R2 that its test does not run: it judges [`R2_WIRED`] alone, so another row
/// there would read as run and run nowhere.
pub fn unwired_problems(rows: &[Row]) -> Vec<String> {
    let at_r2 = rows.iter().filter(|r| run_stage(r, "l2") == Some(J2_SUB_STAGES[1]));
    let unwired = at_r2.map(Row::id).filter(|id| !R2_WIRED.contains(id));
    unwired.map(|id| format!("{id}: l2 at J2-R2, whose test runs {R2_WIRED:?} only")).collect()
}

/// A top-level `#[test] fn` of a Rust source: its attributes besides `#[test]`, on either side of
/// it, and its text from `fn` to its closing `}` line.
pub struct LibTest<'a> {
    pub attrs: Vec<&'a str>,
    pub text: &'a str,
}

/// The top-level `#[test] fn`s of the Rust source `source`, by name.
pub fn lib_tests(source: &str) -> BTreeMap<&str, LibTest<'_>> {
    const TEST: &str = "\n#[test]\n";
    let mut tests = BTreeMap::new();
    for (at, _) in source.match_indices(TEST) {
        // Back to the blank line above: anything but doc comment lines there (an attribute,
        // however `cargo fmt` wraps it, or a `//` line between one and `#[test]`) is kept.
        let before = source[..=at].lines().rev().take_while(|l| !l.trim().is_empty());
        let mut attrs: Vec<&str> = before.filter(|l| !l.starts_with("///")).collect();
        attrs.reverse();
        let mut rest = &source[at + TEST.len()..];
        while rest.starts_with("#[") {
            let (attr, tail) = rest.split_once('\n').unwrap_or((rest, ""));
            attrs.push(attr);
            rest = tail;
        }
        let Some(name) = rest.strip_prefix("fn ").and_then(|s| s.split_once('(')).map(|s| s.0)
        else {
            continue;
        };
        let end = rest.find("\n}\n").map_or(rest.len(), |e| e + 2);
        tests.insert(
            name,
            LibTest {
                attrs,
                text: &rest[..end],
            },
        );
    }
    tests
}

/// The top-level `const <NAME>: &str = "<value>";` items of `source`, by name.
pub fn str_consts(source: &str) -> BTreeMap<&str, &str> {
    let items = source.lines().filter_map(|line| {
        let (name, value) = line.strip_prefix("const ")?.split_once(": &str = \"")?;
        Some((name, value.strip_suffix("\";")?))
    });
    items.collect()
}

/// The recording test `name` replays, read only from the one shape the gate can be trusted to run:
/// a body that is exactly `assert_eq!(replay(<arg>), (<n>, <n>));`, `<arg>` a string literal or a
/// const of `consts`. Anything else (a commented-out or second call, an early return, a call in a
/// string) is refused, as an argument that is neither literal nor const is.
pub fn replay_of<'a>(
    name: &str,
    test: &LibTest<'a>,
    consts: &BTreeMap<&str, &'a str>,
) -> Result<&'a str, String> {
    let shape = || format!("{name} is not the one-line replay assert");
    let head = format!("fn {name}() {{\n    assert_eq!(replay(");
    let rest = test.text.strip_prefix(head.as_str()).ok_or_else(shape)?;
    let (arg, tail) = rest.split_once("), (").ok_or_else(shape)?;
    let counts = tail.strip_suffix("));\n}").ok_or_else(shape)?;
    let digits = |n: &str| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit());
    let counted = counts.split_once(", ").is_some_and(|(a, b)| digits(a) && digits(b));
    if !counted || arg.contains(['(', ')', '\n']) {
        return Err(shape());
    }
    match arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) {
        Some(literal) => Ok(literal),
        None => consts.get(arg).copied().ok_or_else(|| format!("{name} replays {arg}, no const")),
    }
}

/// Where the J2-lib rows' `l2_tests` miss CH7a-5b's replay in `recorded` (the source of
/// [`RECORDED`]), whose verdicts this test cannot read: a J2-lib row without the list, or
/// another row with one; a name that is no `#[test] fn` there, or one with another attribute
/// (`#[ignore]`, a `cfg`: the gate's lib step runs neither ignored tests nor `--include-ignored`);
/// a test that is not the one-line replay assert ([`replay_of`]), or replays a recording that
/// carries no type 7 or 9 tx; the recordings that carry one
/// ([`ev1_j::l2::RECORDINGS`]) not all replayed, or `l2_probes` not their number; and L2-R1
/// without its census ([`L2_CENSUS`]).
pub fn lib_problems(rows: &[Row], recorded: &str) -> Vec<String> {
    let tests = lib_tests(recorded);
    let consts = str_consts(recorded);
    let mut module = Vec::new();
    if recorded.lines().any(|l| l.starts_with("#![")) {
        module.push(
            "recorded.rs carries an inner attribute, which may keep it out of the gate".to_owned(),
        );
    }
    let sut = std::fs::read_to_string(SUT).expect("sut.rs");
    let declared = sut.split_once("\n    mod recorded;\n").map(|(head, _)| head.lines().last());
    match declared {
        None => module.push("sut.rs declares no `mod recorded;` in its tests".to_owned()),
        Some(Some(line)) if line.trim_start().starts_with("#[") => {
            module.push(format!("sut.rs's `mod recorded;` carries {line:?}"));
        }
        Some(_) => {}
    }
    let carrying: BTreeSet<&str> = ev1_j::l2::RECORDINGS.iter().map(|p| p.name).collect();
    let mut problems = Vec::new();
    for row in rows {
        let id = row.id();
        if run_stage(row, "l2") == Some(J2_SUB_STAGES[2]) {
            problems.extend(module.iter().map(|m| format!("{id}: {m}")));
        }
        if run_stage(row, "l2") != Some(J2_SUB_STAGES[2]) {
            if !row.l2_tests.is_empty() {
                problems.push(format!("{id}: l2_tests on a row not at J2-lib"));
            }
            continue;
        }
        if row.l2_tests.is_empty() {
            problems.push(format!("{id}: a J2-lib row names its lib tests in l2_tests"));
            continue;
        }
        let mut replayed = BTreeSet::new();
        for name in &row.l2_tests {
            let Some(test) = tests.get(name.as_str()) else {
                problems.push(format!("{id}: l2_tests names {name}, no #[test] fn in recorded.rs"));
                continue;
            };
            if !test.attrs.is_empty() {
                problems.push(format!(
                    "{id}: {name} carries {:?}, so the gate may not run it",
                    test.attrs
                ));
            }
            if name == L2_CENSUS {
                continue;
            }
            match replay_of(name, test, &consts) {
                Err(e) => problems.push(format!("{id}: {e}")),
                Ok(run) => {
                    if !carrying.contains(run) {
                        problems.push(format!(
                            "{id}: {name} replays {run}, which carries no type 7 or 9 tx"
                        ));
                    }
                    replayed.insert(run);
                }
            }
        }
        let missing: Vec<&str> = carrying.difference(&replayed).copied().collect();
        if !missing.is_empty() {
            problems.push(format!(
                "{id}: l2_tests replay {} of the {} recordings, missing {missing:?}",
                carrying.len() - missing.len(),
                carrying.len()
            ));
        }
        if let Some(n) = row.get("l2_probes").filter(|n| *n != carrying.len().to_string()) {
            problems.push(format!("{id}: l2_probes {n}, but {} recordings", carrying.len()));
        }
        if id == "L2-R1" && !row.l2_tests.iter().any(|t| t == L2_CENSUS) {
            problems.push(format!("{id}: l2_tests without its census {L2_CENSUS}"));
        }
    }
    problems
}

/// Where the rows differ from [`INVENTORY`]: a §4 row missing or one §4 lacks, a path dropped or
/// added, or a path's stage, outcome or reason for not applying changed.
pub fn inventory_problems(rows: &[Row]) -> Vec<String> {
    let mut problems = Vec::new();
    let by_id: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.id(), r)).collect();
    let listed: BTreeSet<&str> = INVENTORY.iter().map(|(id, _)| *id).collect();
    for id in by_id.keys().filter(|id| !listed.contains(*id)) {
        problems.push(format!("{id}: not a §4 row"));
    }
    for (id, cells) in INVENTORY {
        let Some(row) = by_id.get(id) else {
            problems.push(format!("{id}: the §4 row is missing"));
            continue;
        };
        for (path, cell) in PATHS.iter().zip(cells) {
            let value = row.get(path);
            let stage = row.get(&format!("{path}_stage"));
            match (cell.is_empty(), value) {
                (true, None) => {}
                (true, Some(value)) => {
                    problems.push(format!("{id} {path}: {value}, but §4 has no such path"))
                }
                (false, None) => problems.push(format!("{id} {path}: missing, §4 has {cell}")),
                (false, Some(value)) => {
                    let wrong = match NOT_APPLICABLE.iter().find(|p| cell.starts_with(*p)) {
                        Some(kind) => {
                            !value.starts_with(kind) || !value.contains(&cell[kind.len()..])
                        }
                        None => {
                            let (want_stage, outcome) = cell.split_once(' ').unwrap();
                            value != outcome || stage != Some(want_stage)
                        }
                    };
                    if wrong {
                        problems.push(format!(
                            "{id} {path}: {value} at {}, §4 has {cell}",
                            stage.unwrap_or("no stage")
                        ));
                    }
                }
            }
        }
    }
    problems
}

/// The steps of `table` in ev1-pairs.toml, in Core's order, each with its reasons.
pub fn table_steps(text: &str, table: &str) -> Vec<(String, Vec<String>)> {
    let header = format!("key = \"{table}\"");
    let start = text.find(&header).unwrap_or_else(|| panic!("table {table}"));
    let steps = &text[start..];
    let steps = &steps[steps.find("steps = [").expect("steps")..];
    let steps = &steps[..steps.find("\n]").expect("the list's end")];
    steps
        .lines()
        .skip(1)
        .map(|l| {
            let mut words = l.trim().trim_start_matches('"').trim_end_matches("\",").split(' ');
            let step = words.next().unwrap().to_owned();
            (step, words.map(str::to_owned).collect())
        })
        .collect()
}

/// The steps of `table` from `first` on: §3.12's context checks.
pub fn context_steps(text: &str, table: &str, first: &str) -> Vec<String> {
    let all: Vec<String> = table_steps(text, table).into_iter().map(|(s, _)| s).collect();
    let at = all.iter().position(|s| s == first).unwrap_or_else(|| panic!("{first}"));
    all[at..].to_vec()
}

/// D13's coins-view cases (EV1-J plan §4 D13), each with its reject: the masternode-list build's
/// collateral-spend reject (`FinishListForBlock`), the covenant on ConnectBlock's block view, the
/// covenant reached on B through an output no masternode holds, the covenant's mempool view, and
/// `ctx.coins`.
pub const D13_ITEMS: [(&str, &str); 5] = [
    ("D13 list build", "bad-shared-collateral-spend"),
    ("D13 covenant block view", "bad-shared-collateral-spend"),
    ("D13 covenant block", "bad-shared-collateral-spend"),
    ("D13 covenant m_view", "bad-shared-collateral-spend"),
    ("D13 ctx.coins", "bad-protx-collateral"),
];

/// EV1 design §6.3's branches and first-reject pairs for types 7 and 9, over §3.12's steps as
/// ev1-pairs.toml's 7c and 9c tables list them: every context step, the variants §6.3 names (both
/// too-old scenarios, both too-late edges, the bad signature below assumevalid, the expired v2
/// retry), the pairs §6.3 lists, and each payload-level reject (9c:3, 9c:5) ahead of every context
/// step; and [`D13_ITEMS`].
pub fn closure_items(pairs: &str) -> BTreeSet<String> {
    let mnhf = context_steps(pairs, "7c", "7c:4");
    let unlock = context_steps(pairs, "9c", "9c:7");
    let mut items: BTreeSet<String> = mnhf.iter().chain(&unlock).cloned().collect();
    items.extend(
        [
            "7c:8 BAV",
            "9c:14 BAV",
            "9c:11 unscanned",
            "9c:11 expired",
            "9c:12 below",
            "9c:12 expiry",
            "9c:12 retry first_height",
            "9c:12 retry last_height",
            "9c:12 retry fails",
            "7c:4>7c:5a",
            "7c:5a>7c:8",
            "7c:5b>7c:7",
            "7c:7>7c:8",
            "9c:7>9c:8",
            "9c:8>9c:11",
            "9c:11>9c:12",
            "9c:12>9c:14",
        ]
        .map(str::to_owned),
    );
    for payload in ["9c:3", "9c:5"] {
        items.extend(unlock.iter().map(|step| format!("{payload}>{step}")));
    }
    items.extend(D13_ITEMS.map(|(item, _)| item.to_owned()));
    items
}

/// Where the rows' `covers` and `excludes` miss the closure: a recorded exclusion other than
/// [`EXCLUSIONS`] (an item in `excludes`, or covered by a row none of whose paths applies), an
/// item named twice (covered and excluded), a name that is no item, or an item no row names.
pub fn closure_problems(rows: &[Row], pairs: &str) -> Vec<String> {
    let items = closure_items(pairs);
    let applies = |r: &Row| PATHS.iter().filter_map(|p| r.get(p)).any(|v| !not_applicable(v));
    let covered: BTreeSet<&str> = rows
        .iter()
        .filter(|r| applies(r))
        .flat_map(|r| r.covers.iter().map(String::as_str))
        .collect();
    let recorded: BTreeSet<(&str, &str)> = rows
        .iter()
        .flat_map(|r| {
            let covers = if applies(r) {
                &[][..]
            } else {
                &r.covers[..]
            };
            r.excludes.iter().chain(covers).map(move |item| (item.as_str(), r.id()))
        })
        .collect();
    let excluded: BTreeSet<&str> = recorded.iter().map(|(item, _)| *item).collect();
    let mut problems = Vec::new();
    let pinned: BTreeSet<(&str, &str)> = EXCLUSIONS.into_iter().collect();
    for (item, row) in recorded.difference(&pinned) {
        problems.push(format!("{item} ({row}): an exclusion §4 does not record"));
    }
    for (item, row) in pinned.difference(&recorded) {
        problems.push(format!("{item} ({row}): §4's exclusion is not recorded"));
    }
    for both in covered.intersection(&excluded) {
        problems.push(format!("{both}: covered and recorded as unobservable"));
    }
    let named: BTreeSet<&str> = covered.union(&excluded).copied().collect();
    for stray in named.iter().filter(|c| !items.contains(**c)) {
        problems.push(format!("{stray}: covers or excludes names no closure item"));
    }
    for open in items.iter().filter(|i| !named.contains(i.as_str())) {
        problems.push(format!("{open}: no row covers or excludes it"));
    }
    problems
}

/// Where a row's cover does not match its outcome: each item a row covers names a reject (a
/// step's, `Xc:N[ variant]`; a pair's winner's, `A>B`; a D13 case's), and every reject outcome
/// of the row's paths that apply must be one of the item's reasons, with at least one there. A
/// row none of whose paths apply (9-X, 9-P2) records its covers as unreachable or infeasible.
/// `accept` and the 9-R rows' M outcomes are the expired-instance retry's own answers, not the
/// step's, so they are not compared.
pub fn cover_problems(rows: &[Row], pairs: &str) -> Vec<String> {
    let steps: BTreeMap<String, Vec<String>> =
        ["7c", "9c"].iter().flat_map(|t| table_steps(pairs, t)).collect();
    let mut problems = Vec::new();
    for row in rows {
        let id = row.id();
        let applies: Vec<&str> =
            PATHS.iter().filter_map(|p| row.get(p)).filter(|v| !not_applicable(v)).collect();
        if applies.is_empty() {
            continue;
        }
        let rejects: Vec<&str> = applies.iter().copied().filter(|v| is_reject(v)).collect();
        for cover in &row.covers {
            let reasons: Vec<&str> = match D13_ITEMS.iter().find(|(item, _)| item == cover) {
                Some((_, reason)) => vec![*reason],
                None => {
                    let winner = cover.split('>').next().unwrap();
                    let step = winner.split(' ').next().unwrap();
                    let Some(reasons) = steps.get(step) else {
                        problems.push(format!("{id}: covers {cover}, no step {step}"));
                        continue;
                    };
                    reasons.iter().map(String::as_str).collect()
                }
            };
            if rejects.is_empty() || rejects.iter().any(|r| !reasons.contains(r)) {
                problems.push(format!(
                    "{id}: covers {cover} ({}), but its rejects are {rejects:?}",
                    reasons.join(" ")
                ));
            }
            // A pair's evidence is its control: the same case without the winner's fault, so
            // the loser answers.
            if let (Some((_, loser)), Some(control)) = (cover.split_once('>'), row.get("b_control"))
            {
                let step = loser.split(' ').next().unwrap();
                let losers = steps.get(step).map(Vec::as_slice).unwrap_or_default();
                if !losers.iter().any(|r| r == control) {
                    problems.push(format!(
                        "{id}: covers {cover}, but b_control {control} is not {step}'s reject"
                    ));
                }
            }
        }
    }
    problems
}

/// Everything the rows file gets wrong, the inventory first: the shape, then [`INVENTORY`], and
/// only when both hold, the closure over §6.3 and the covers' outcomes.
pub fn rows_file_problems(rows_text: &str, pairs: &str) -> Vec<String> {
    let rows = parse_rows(rows_text);
    let mut problems = row_problems(&rows);
    problems.extend(unwired_problems(&rows));
    problems.extend(lib_problems(&rows, &std::fs::read_to_string(RECORDED).expect("recorded.rs")));
    problems.extend(inventory_problems(&rows));
    if problems.is_empty() {
        problems.extend(closure_problems(&rows, pairs));
        problems.extend(cover_problems(&rows, pairs));
    }
    problems
}

pub fn rows_text() -> String {
    std::fs::read_to_string(ROWS).expect("ev1-j-rows.toml")
}

pub fn pairs_text() -> String {
    std::fs::read_to_string(PAIRS).expect("ev1-pairs.toml")
}

// ---- the validators' verdicts ---------------------------------------------------------------

/// `submitblock_StateCatcher` on V's signals: the state of each block checked, by hash.
#[derive(Default)]
pub struct Checked(pub Mutex<Vec<(BlockHash, BlockState)>>);

impl ValidationInterface for Checked {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.0.lock().unwrap().push((block.hash(), state.clone()));
    }
}

/// `submitblock`'s answer for a checked block (`rpc/mining.cpp`'s `BIP22ValidationResult`).
pub fn bip22(state: &BlockState) -> Value {
    match state {
        BlockState::Valid => Value::Null,
        BlockState::Invalid(e) => json!(e.reject_reason()),
        BlockState::Error(msg) => json!(msg),
    }
}

/// `submitblock`'s answer as the table shows it.
pub fn show(answer: &Value) -> String {
    answer.as_str().unwrap_or("accept").to_owned()
}

/// What a row expects on a block: `submitblock`'s answer and, for a reject, Core's class as the
/// rows file cites it (`<key>_class`; dashd's class is not observed, see the module docs).
#[derive(Debug, PartialEq)]
pub struct Expected {
    pub answer: Value,
    pub class: Option<BlockValidationResult>,
}

impl Expected {
    /// The outcome under `key` of `row` (`b`, `b_control`, `b_order` or `b_order_control`).
    pub fn of(row: &Row, key: &str) -> Expected {
        match row.get(key).expect("the probe's outcome") {
            "accept" => Expected {
                answer: Value::Null,
                class: None,
            },
            reason => Expected {
                answer: json!(reason),
                class: class_of(row.get(&format!("{key}_class")).expect("a cited class")),
            },
        }
    }
}

/// V's verdict on a block: its `submitblock` answer and the reject's class, both from the state
/// `BlockChecked` carries, and whether `ProcessNewBlock` passed `CheckBlock` and `AcceptBlock`.
#[derive(Debug)]
pub struct Ours {
    pub answer: Value,
    pub class: Option<BlockValidationResult>,
    pub accepted: bool,
    /// The step-10 checks V's verifier made on the block (`ev1_j::l2::ClChecks`), from its
    /// census; `None` where V registered none.
    pub cl_checks: Option<ev1_j::l2::ClChecks>,
    /// Why V is down after the block, if it is ([`v_down`]).
    pub down: Option<String>,
}

impl Ours {
    /// Where V's `ProcessNewBlock` result disagrees with its `BlockChecked` state: a block V calls
    /// valid passed `AcceptBlock`, and a mutated one never passes `CheckBlock`. A block that did
    /// not pass them can be a reject of any class (`CheckBlock`'s `bad-txns-*` are
    /// BLOCK_CONSENSUS), so nothing more is asked.
    pub fn inconsistency(&self) -> Option<&'static str> {
        if self.down.is_some() {
            return Some("V went down on the block");
        }
        match (self.answer.is_null(), self.accepted, self.class) {
            (true, false, _) => Some("V answers valid but did not accept the block"),
            (_, true, Some(BlockValidationResult::Mutated)) => {
                Some("V accepted a block it calls mutated")
            }
            _ => None,
        }
    }
}

/// Why V is not up: its load was refused ([`NodeSut::down`]), or a shutdown was requested, which
/// its `AbortNode` sink and a listener's panic do (`ChainHandle::shutdown_requested`).
pub fn v_down(sut: &NodeSut) -> Option<String> {
    if let Some(down) = sut.down() {
        return Some(down.to_owned());
    }
    let chain = sut.chain()?;
    chain.shutdown_requested().then(|| "a shutdown was requested".to_owned())
}

/// What every dashd of these tests runs as: dashd v24.0.0-rc.7 by its own report
/// (`getnetworkinfo`'s `buildversion`, `rpc/net.cpp:693`), with spork 19 off (`spork active`,
/// `rpc/node.cpp:194-199`), so that it takes no best ChainLock from a CbTx
/// (`chainlock/handler.cpp:104-111`) and verifies every non-null one (the J2 delta's C1).
pub fn pinned(dashd: &Dashd) {
    let info = rpc(dashd, "getnetworkinfo", json!([]));
    assert_eq!(info["buildversion"], json!(v24::CORE_VERSION), "dashd's version");
    let sporks = rpc(dashd, "spork", json!(["active"]));
    assert_eq!(sporks["SPORK_19_CHAINLOCKS_ENABLED"], json!(false), "spork 19 off: {sporks}");
}

pub fn rpc(dashd: &Dashd, method: &str, params: Value) -> Value {
    let reply = dashd.reply(method, params.clone()).expect("a reply");
    assert!(reply["error"].is_null(), "dashd {method} {params}: {}", reply["error"]);
    reply["result"].clone()
}

pub fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

pub fn tx_of(hex: &Value) -> Tx {
    decode_tx_exact(&unhex(hex.as_str().expect("hex"))).expect("a tx").into_value()
}

pub fn block_of(hex: &Value) -> Block {
    let bytes = unhex(hex.as_str().expect("hex"));
    let mut input = bytes.as_slice();
    let block = decode_block(&mut input).expect("a block").into_value();
    assert!(input.is_empty(), "one block");
    block
}

/// The bytes Core keeps a hash in, from its displayed hex.
pub fn internal(display: &Value) -> [u8; 32] {
    let mut bytes: [u8; 32] = unhex(display.as_str().expect("a hash")).try_into().unwrap();
    bytes.reverse();
    bytes
}

pub fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A chain the producer, the validators and V run: dashd's network, V's arguments selecting it,
/// the chain parameters all three take, and the height of the first block a producer mines
/// (regtest 1; a devnet's block 1 is its devnet genesis block, `chainparams.cpp:648-649`, which
/// every node makes itself).
#[derive(Clone, Copy, Debug)]
pub struct Chain {
    pub net: v24::Net,
    pub sut: &'static [&'static str],
    pub args: &'static [&'static str],
    pub first_height: usize,
}

/// The regtest chain of [`CHAIN_ARGS`].
pub const REGTEST: Chain = Chain {
    net: v24::Net::Regtest,
    sut: &["-regtest"],
    args: &CHAIN_ARGS,
    first_height: 1,
};

pub fn sut_argv() -> Vec<&'static str> {
    sut_argv_on(REGTEST)
}

/// V's argv on `chain`.
pub fn sut_argv_on(chain: Chain) -> Vec<&'static str> {
    [chain.sut, &["-printtoconsole=0"], chain.args].concat()
}

/// A new V on `dir`, with `extra` arguments, the setup blocks fed to it (each must be accepted),
/// then the stale blocks (each stored, off the active chain), and its listener.
pub fn fresh_sut(
    dir: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
    extra: &[&str],
) -> (NodeSut, Arc<Checked>) {
    fresh_sut_on(REGTEST, dir, time, setup, stale, extra)
}

/// [`fresh_sut`] on `chain`.
pub fn fresh_sut_on(
    on: Chain,
    dir: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
    extra: &[&str],
) -> (NodeSut, Arc<Checked>) {
    std::fs::create_dir_all(dir).unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let argv = [&sut_argv_on(on)[..], extra].concat();
    let sut = NodeSut::new(dir, &argv, time, ui).expect("V starts");
    let checked = Arc::new(Checked::default());
    assert!(sut.register_listener(checked.clone()));
    let chain = sut.chain().expect("V runs");
    for (height, block) in (on.first_height..).zip(setup) {
        let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted && r.state.is_valid(), "setup block {height} on V: {r:?}");
    }
    for block in stale {
        let r = chain.process_block(Arc::clone(block), true, None, true).recv().expect("V");
        assert!(r.accepted, "stale block {} on V: {r:?}", block.hash());
    }
    if let Some(tip) = setup.last() {
        assert_eq!(chain.view().snap.tip.tip_hash, tip.hash(), "V's setup tip");
    }
    (sut, checked)
}

/// The two validators of one probe, fresh on the setup chain: a dashd on a copy of `base` and a
/// new V.
pub struct Validators<'a> {
    pub chain: Chain,
    pub binary: &'a Path,
    pub base: &'a Path,
    pub setup: &'a [Arc<Block>],
    pub stale: &'a [Arc<Block>],
    pub time: i64,
    pub scratch: &'a Path,
}

impl Validators<'_> {
    /// dashd's `submitblock` answer and V's verdict on `block`. A BAV probe's (`below`) are
    /// judged below assumevalid: both validators run `-assumevalid` at the last header of
    /// `below` and `-par=1`, and take `block`'s header and `below` first ([`bury_on_dashd`],
    /// [`bury_on_v`]); they run side by side.
    pub fn judge(&self, block: &Block, below: Option<&[Header]>) -> (Value, Ours) {
        let dir = tempfile::tempdir_in(self.scratch).unwrap();
        let bytes = core_serialize(block);
        let hash = block.hash();
        let extra: Vec<String> = below
            .map(|b| vec![format!("-assumevalid={}", b.last().unwrap().hash()), "-par=1".into()])
            .unwrap_or_default();
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();

        let core = || {
            copy_dir(self.base, &dir.path().join("dashd"));
            let args = [self.chain.args, &extra].concat();
            let dashd = Dashd::start_on(
                self.binary,
                &dir.path().join("dashd"),
                self.time,
                self.chain.net,
                &args,
                false,
            );
            pinned(&dashd);
            let tip = rpc(&dashd, "getbestblockhash", json!([]));
            assert_eq!(
                tip,
                json!(self.setup.last().unwrap().hash().to_string()),
                "dashd's setup tip"
            );
            if let Some(below) = below {
                bury_on_dashd(&dashd, block, below, self.setup.len());
            }
            // Only the answer: the class is code-cited (module docs).
            rpc(&dashd, "submitblock", json!([hex(&bytes)]))
        };

        let ours = || {
            let rdashd = dir.path().join("rdashd");
            let (sut, checked) =
                fresh_sut_on(self.chain, &rdashd, self.time, self.setup, self.stale, &extra);
            if let Some(below) = below {
                bury_on_v(&sut, block, below);
            }
            let census = census_under(&rdashd);
            let before = census.as_ref().map(|c| c.checks.lock().len());
            let mut input = bytes.as_slice();
            let ours = decode_block(&mut input).expect("V decodes dashd's bytes").into_value();
            assert!(input.is_empty() && ours.hash() == hash, "the same block");
            let chain = sut.chain().expect("V runs");
            let reply = chain.process_block(Arc::new(ours), true, None, true).recv();
            // A V that went down on the block (a fatal error such as D14's ScanCapacity) answers
            // nothing more, perhaps not even the block's reply: its verdict is that, a failure of
            // the probe.
            let dropped = || reply.is_err().then(|| "it dropped the block's reply".to_owned());
            if let Some(down) = v_down(&sut).or_else(dropped) {
                return Ours {
                    answer: json!(format!("V down: {down}")),
                    class: None,
                    accepted: reply.as_ref().is_ok_and(|r| r.accepted),
                    cl_checks: None,
                    down: Some(down),
                };
            }
            let r = reply.expect("V answers");
            assert!(sut.sync_signals(), "V's signal queue drained");
            let state = checked
                .0
                .lock()
                .unwrap()
                .iter()
                .rev()
                .find(|(b, _)| *b == hash)
                .map(|(_, s)| s.clone());
            let state = state.expect("V checked the block");
            let class = match &state {
                BlockState::Invalid(e) => Some(e.result()),
                _ => None,
            };
            let cl_checks = census.zip(before).map(|(c, n)| c.checks.lock()[n..].to_vec());
            Ours {
                answer: bip22(&state),
                class,
                accepted: r.accepted,
                cl_checks,
                down: None,
            }
        };

        if below.is_none() {
            return (core(), ours());
        }
        std::thread::scope(|s| {
            let core = s.spawn(core);
            let ours = ours();
            (core.join().unwrap_or_else(|e| std::panic::resume_unwind(e)), ours)
        })
    }
}

/// Buries `block` on `dashd` at setup height `height`: its header, then `below`, each through
/// `submitheader`; dashd's best header is then the last, and dashd's own chain work puts it
/// `150 * below.len()` s of proof-equivalent time above `block` (`chain.cpp:147-162`), every
/// block of the same proof.
pub fn bury_on_dashd(dashd: &Dashd, block: &Block, below: &[Header], height: usize) {
    for header in std::iter::once(block.header()).chain(below) {
        let answer = rpc(dashd, "submitheader", json!([hex(&core_serialize(header))]));
        assert_eq!(answer, Value::Null, "dashd takes header {}", header.hash());
    }
    let info = rpc(dashd, "getblockchaininfo", json!([]));
    assert_eq!(info["headers"], json!(height + 1 + below.len()), "dashd's best header");
    assert_eq!(info["blocks"], json!(height), "dashd's tip stays");
    let work = |hash: BlockHash| {
        let header = rpc(dashd, "getblockheader", json!([hash.to_string()]));
        u128::from_str_radix(header["chainwork"].as_str().unwrap(), 16).unwrap()
    };
    let last = below.last().unwrap();
    let proof = work(last.hash()) - work(last.prev_hash());
    assert_eq!(work(block.hash()) - work(block.header().prev_hash()), proof, "one proof per block");
    let equivalent = (work(last.hash()) - work(block.hash())) * 150 / proof;
    assert_eq!(equivalent, 150 * below.len() as u128, "the burial's proof-equivalent time");
}

/// Buries `block` on V: its header, then `below`, through `process_headers` in batches of 2,000,
/// each header inserted; V's best header is then the last.
pub fn bury_on_v(sut: &NodeSut, block: &Block, below: &[Header]) {
    let chain = sut.chain().expect("V runs");
    let headers: Vec<Header> =
        std::iter::once(*block.header()).chain(below.iter().copied()).collect();
    for chunk in headers.chunks(2_000) {
        let batch = dash_chain::HeaderBatch {
            hashes: chunk.iter().map(Header::hash).collect(),
            headers: chunk.to_vec(),
        };
        let r = chain.process_headers(batch, None).recv().expect("V replies");
        assert!(r.stop.is_none() && r.inserted.len() == chunk.len(), "V takes the headers: {r:?}");
    }
    let best = chain.view().snap.tip.best_header_hash;
    assert_eq!(best, headers.last().unwrap().hash(), "V's best header");
}

/// The setup validator on `base`: dashd answers null to every setup block and `inconclusive` to
/// each stale one (stored, not connected: `rpc/mining.cpp` submitblock), and a V fed the same
/// blocks agrees on the tip and the deployments. Returns V's proof-of-work parameters.
pub fn setup_validators(
    binary: &Path,
    scratch: &Path,
    base: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
) -> PowParams {
    setup_validators_on(REGTEST, binary, scratch, base, time, setup, stale, 0)
}

/// [`setup_validators`] on `chain`, the dashd on `base` already at `setup[..from]`.
#[allow(clippy::too_many_arguments)]
pub fn setup_validators_on(
    on: Chain,
    binary: &Path,
    scratch: &Path,
    base: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
    from: usize,
) -> PowParams {
    let tip = setup.last().unwrap().hash();
    let dashd = Dashd::start_on(binary, base, time, on.net, on.args, false);
    pinned(&dashd);
    for (height, block) in (on.first_height + from..).zip(&setup[from..]) {
        let answer = rpc(&dashd, "submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "setup block {height} on dashd");
    }
    for block in stale {
        let answer = rpc(&dashd, "submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, json!("inconclusive"), "stale block {} on dashd", block.hash());
    }
    assert_eq!(rpc(&dashd, "getbestblockhash", json!([])), json!(tip.to_string()));
    let dir = tempfile::tempdir_in(scratch).unwrap();
    let (sut, _) = fresh_sut_on(on, dir.path(), time, setup, stale, &[]);
    let report = sut
        .chain()
        .expect("V runs")
        .query(|q| q.deployments.deployment_info(q.chain.private_tip().expect("a tip").0.id))
        .recv()
        .expect("V serves");
    let ours: serde_json::Map<String, Value> =
        report.iter().map(|d| (d.name().to_owned(), deployment_json(d))).collect();
    let core = rpc(&dashd, "getdeploymentinfo", json!([tip.to_string()]));
    assert_eq!(core["deployments"], Value::Object(ours), "getdeploymentinfo at the setup tip");
    *HeaderParams::from_chain_params(sut.params()).unwrap().pow()
}

// ---- probe blocks and the stage judge -------------------------------------------------------

/// A type 7 signal for `bit` naming `quorum`, with a well-formed signature that no quorum made
/// (`mnhftx.h`'s payload: version 1, the bit, the quorum hash, the signature).
pub fn mnhf(bit: u8, quorum: &[u8; 32], sig: &[u8]) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(7),
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: 0,
        extra_payload: [&[1, bit][..], quorum, sig].concat(),
    }))
}

/// A type 9 unlock with `outputs` P2PKH outputs of `value` (`assetlocktx.h:109-119`'s payload:
/// version, index, fee, requested height, quorum hash, signature).
#[allow(clippy::too_many_arguments)]
pub fn unlock_with(
    version: u8,
    index: u64,
    fee: u32,
    value: i64,
    outputs: usize,
    requested: u32,
    quorum: &[u8; 32],
    sig: &[u8],
    p2pkh: &[u8],
) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(9),
        inputs: Vec::new(),
        outputs: (0..outputs)
            .map(|_| TxOut {
                value,
                script_pubkey: p2pkh.to_vec().into(),
            })
            .collect(),
        lock_time: 0,
        extra_payload: [
            &[version][..],
            &index.to_le_bytes(),
            &fee.to_le_bytes(),
            &requested.to_le_bytes(),
            quorum,
            sig,
        ]
        .concat(),
    }))
}

/// `template` with `extra` appended: the CbTx (version 4 from V24) committing to the stable-txid
/// unlocks' instance hashes (`cbtx.cpp:158-172`, checked in `CheckMerkleRoot`), ground on `pow`.
pub fn probe_block(template: &Block, extra: &[Arc<Tx>], pow: &PowParams) -> Arc<Block> {
    let mut txs = template.txs().to_vec();
    txs.extend(extra.iter().cloned());
    let leaves: Vec<[u8; 32]> = txs
        .iter()
        .filter(|tx| tx.is_asset_unlock_with_stable_txid())
        .map(|tx| *tx.instance_hash().as_bytes())
        .collect();
    let mut coinbase = txs[0].to_mutable();
    assert_eq!(coinbase.extra_payload[..2], [4, 0], "a version 4 CbTx");
    let n = coinbase.extra_payload.len();
    coinbase.extra_payload[n - 32..].copy_from_slice(&compute_merkle_root(leaves).0);
    txs[0] = Arc::new(Tx::new(coinbase));
    let mut block = Block::new(Header::new(*template.header().raw()), txs);
    let mut tries = 1_000_000;
    assert_eq!(grind(&mut block, pow, &mut tries, &|| false), Grind::Found);
    let tail: Vec<_> =
        block.txs()[block.txs().len() - extra.len()..].iter().map(|t| t.txid()).collect();
    let certified: Vec<_> = extra.iter().map(|t| t.txid()).collect();
    assert_eq!(tail, certified, "the probe ends with the txs it certifies");
    Arc::new(block)
}

/// One probe: its row, a name, the row's key for its expected outcome (`b` or `bav` for the
/// row's own case, `b_order` for its order probe, any other key for a control), the block, and
/// for a BAV probe the headers burying it (`ev1_j::bav`): the validators run `-assumevalid` at
/// the last and take them before the block.
pub struct Probe {
    pub row: &'static str,
    pub name: String,
    pub expect: &'static str,
    pub block: Arc<Block>,
    pub below: Option<Vec<Header>>,
    /// The step-10 checks V must make on the block, where the stage asserts them (J2-C).
    pub cl_checks: Option<ev1_j::l2::ClChecks>,
}

impl Probe {
    pub fn control(&self) -> bool {
        !["b", "bav"].contains(&self.expect)
    }
}

/// OD-CP-64's tie between a BAV row's probes, `mine`: the row's own probe and its mode controls
/// sit [`ev1_j::bav::BURIED`] headers deep and its twins [`ev1_j::bav::TWO_WEEKS`], each header
/// chain on its own probe's block, and each twin is a mode control's block under the first of
/// that control's headers. The judges see only the headers they get, so a probe buried shallower
/// than its mode control would have its scripts checked and the control would prove nothing about
/// it.
pub fn bav_depths(id: &str, mine: &[&Probe]) {
    pub fn below(p: &Probe) -> &[Header] {
        p.below.as_deref().expect("a BAV probe has its headers")
    }
    for p in mine {
        let depth = if p.expect == "bav_twin" {
            ev1_j::bav::TWO_WEEKS
        } else {
            ev1_j::bav::BURIED
        };
        assert_eq!(below(p).len(), depth, "{id}: {} is {depth} headers deep", p.name);
        assert_eq!(
            below(p)[0].raw().prev_hash,
            p.block.hash(),
            "{id}: {}'s headers are on its block",
            p.name
        );
    }
    for twin in mine.iter().filter(|p| p.expect == "bav_twin") {
        assert!(
            mine.iter().any(|c| c.expect == "bav_mode_control"
                && c.block.hash() == twin.block.hash()
                && below(c).starts_with(below(twin))),
            "{id}: {} is a mode control's block under the first of its headers",
            twin.name
        );
    }
}

/// Judges `probes`, the probes of the rows whose block path `path` (`b` or `bav`) runs at `stage`,
/// on fresh `validators`: each row has its own case, a control and a probe for each of its
/// outcomes on the path ([`PROBE_KEYS`], [`BAV_PROBE_KEYS`]), as many as its `<path>_probes`,
/// and each probe gets the row's outcome on both nodes, V's reject of the class the rows file
/// cites. Prints the table and the paths this head does not run, with their stage.
pub fn judge_stage(
    rows: &[Row],
    stage: &str,
    path: &str,
    probes: &[Probe],
    validators: &Validators<'_>,
    started: std::time::Instant,
) {
    judge_stage_on(rows, stage, path, probes, |_| validators, started);
}

/// [`judge_stage`], each probe on the validators `on` gives it: D14-34's age control is judged on
/// the setup chain's tip before B's commitments ([`AGE_ROWS`]).
pub fn judge_stage_on<'v>(
    rows: &[Row],
    stage: &str,
    path: &str,
    probes: &[Probe],
    on: impl Fn(&Probe) -> &'v Validators<'v> + Sync,
    started: std::time::Instant,
) {
    let by_id: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.id(), r)).collect();
    let run: BTreeSet<&str> =
        rows.iter().filter(|r| run_stage(r, path) == Some(stage)).map(Row::id).collect();
    let keys = if path == "bav" {
        &BAV_PROBE_KEYS[..]
    } else {
        &PROBE_KEYS[..]
    };
    assert!(
        probes.iter().all(|p| keys.contains(&p.expect) && p.below.is_some() == (path == "bav")),
        "{stage}'s probes are {path} probes"
    );
    let built: BTreeSet<&str> = probes.iter().map(|p| p.row).collect();
    assert_eq!(built, run, "the probes are the rows at {stage}");
    for id in &run {
        let mine = || probes.iter().filter(|p| p.row == *id);
        assert!(
            mine().any(|p| !p.control()) && mine().any(Probe::control),
            "{id}: a run row judges its own case and a control"
        );
        let judged: BTreeSet<&str> = mine().map(|p| p.expect).collect();
        let outcomes: BTreeSet<&str> =
            keys.iter().copied().filter(|k| by_id[id].get(k).is_some()).collect();
        assert_eq!(judged, outcomes, "{id}: a probe for each of the row's block outcomes");
        let names: BTreeSet<&str> = mine().map(|p| p.name.as_str()).collect();
        let count = format!("{path}_probes");
        let pinned: usize = by_id[id].get(&count).and_then(|n| n.parse().ok()).unwrap_or(0);
        assert_eq!(
            (mine().count(), names.len()),
            (pinned, pinned),
            "{id}: the row's {count} distinct probes, one per variant"
        );
        if path == "bav" {
            bav_depths(id, &mine().collect::<Vec<_>>());
        }
    }

    // A BAV probe's judges take its ~8,000 headers, each checked with `-checkblockindex`: these
    // run [`BAV_WORKERS`] at a time, each on validators of its own.
    let workers = if path == "bav" {
        BAV_WORKERS
    } else {
        1
    };
    let verdicts = judge_all(probes, &on, workers, started);
    let mut table = Vec::new();
    let mut wrong = Vec::new();
    for (probe, (core, ours)) in probes.iter().zip(verdicts) {
        let row = by_id[probe.row];
        let want = Expected::of(row, probe.expect);
        let kind = match probe.expect {
            "b" => "",
            "bav" => " below assumevalid",
            "b_control" => " control",
            "b_order" => " order probe",
            "b_pool_control" => " pool control",
            "b_age_control" => " age control",
            "bav_mode_control" => " mode control",
            "bav_twin" => " positive twin",
            _ => " order control",
        };
        let name = format!("{}{kind} ({})", probe.row, probe.name);
        let checks = match &probe.cl_checks {
            Some(_) => {
                format!("  V's step-10 checks {:?}", ours.cl_checks.clone().unwrap_or_default())
            }
            None => String::new(),
        };
        table.push(format!(
            "{name:<96} dashd {:<32} V {:<32} expected {}{checks}",
            show(&core),
            show(&ours.answer),
            show(&want.answer)
        ));
        let mut faults = Vec::new();
        if core != want.answer {
            faults.push(format!("dashd answers {core}"));
        }
        if ours.answer != want.answer || ours.class != want.class {
            faults.push(format!("V answers {} with class {:?}", ours.answer, ours.class));
        }
        if let Some(want) = &probe.cl_checks
            && ours.cl_checks.as_ref() != Some(want)
        {
            faults.push(format!("V's step-10 checks are {:?}, not {want:?}", ours.cl_checks));
        }
        faults.extend(ours.inconsistency().map(str::to_owned));
        if !faults.is_empty() {
            wrong.push(format!(
                "{name}: {}; the row expects {} with class {:?}",
                faults.join(", "),
                want.answer,
                want.class
            ));
        }
    }
    print_skipped(rows);
    eprintln!("{}", table.join("\n"));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Lists the paths this head does not run, by row and stage: none of them passes.
pub fn print_skipped(rows: &[Row]) {
    for row in rows {
        for path in PATHS {
            if let Some(stage) = run_stage(row, path)
                && !REACHED.contains(&stage)
            {
                let why = if stage == DEFERRED_STAGE {
                    " (a deferral)"
                } else {
                    ""
                };
                eprintln!("skipped: {} {path}, stage {stage}{why}", row.id());
            }
        }
    }
}

/// Regtest's proof-of-work parameters, from a V on `dir` with no blocks.
pub fn regtest_pow(dir: &Path) -> PowParams {
    let (sut, _) = fresh_sut(dir, v24::T0, &[], &[], &[]);
    *HeaderParams::from_chain_params(sut.params()).unwrap().pow()
}

/// How many BAV probes are judged at once.
pub const BAV_WORKERS: usize = 5;

/// Each probe's verdicts ([`Validators::judge`]), in order, judged `workers` at a time.
pub fn judge_all<'v>(
    probes: &[Probe],
    on: &(impl Fn(&Probe) -> &'v Validators<'v> + Sync),
    workers: usize,
    started: std::time::Instant,
) -> Vec<(Value, Ours)> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let verdicts: Mutex<Vec<Option<(Value, Ours)>>> =
        Mutex::new(probes.iter().map(|_| None).collect());
    let work = || {
        loop {
            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let Some(probe) = probes.get(at) else {
                return;
            };
            let verdict = on(probe).judge(&probe.block, probe.below.as_deref());
            eprintln!("{} {} at {:?}", probe.row, probe.name, started.elapsed());
            verdicts.lock().unwrap()[at] = Some(verdict);
        }
    };
    std::thread::scope(|s| {
        let handles: Vec<_> = (1..workers).map(|_| s.spawn(work)).collect();
        work();
        for handle in handles {
            handle.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
        }
    });
    verdicts.into_inner().unwrap().into_iter().map(|v| v.expect("judged")).collect()
}
