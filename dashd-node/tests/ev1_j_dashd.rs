//! EV1-J J0a, J0b-1 and J0b-2 (the EV1-J plan rev4 §3, §5.1-§5.4; EV1 design §6.3 and D27, in
//! `docs/node-program/designs/EV1-specialtx.md`): the block-path rows of
//! `contrib/node/ev1-j-rows.toml`, each judged by dashd v24.0.0-rc.5's `submitblock` and by V on
//! the same bytes. J0a's rows need no quorum with known keys; J0b-1's run on the regtest quorum
//! fixture of `ev1_j/` (constructed commitments the producer mines); J0b-2's BAV paths run on the
//! same fixture below assumevalid (`ev1_j/bav.rs`), each with a mode control that both nodes
//! accept only with their scripts skipped and its positive twin, which both reject with the
//! script error.
//!
//! What is compared. dashd's side is `submitblock`'s answer only: null (accept) or the reject
//! reason. V's side is the state its `BlockChecked` carries, as Core's `submitblock_StateCatcher`
//! reads it: its answer, which must equal dashd's and the row's, and its reject's class, which
//! must equal the class the rows file gives with a Core file:line cite (`<key>_class`). dashd's
//! class is not observed: an RPC caller sees a failed block (`getchaintips` `invalid`) for
//! BLOCK_CONSENSUS and BLOCK_RESULT_UNSET alike (validation.cpp:2860,2875,3660), so the class is
//! code-cited, not measured. Every J0a reject is BLOCK_CONSENSUS at the tag. A row whose reject
//! Core leaves UNSET (`bad-cb-amount`, `bad-cb-payee`) needs the class measured: relay the probe
//! to dashd over P2P from a test peer and read the punishment (net_processing.cpp:1974-1981:
//! UNSET none, CONSENSUS `Misbehaving(100)` and a disconnect). No J0a row has one.
//!
//! V's B-path answer comes from `process_block` and a `BlockChecked` listener in this test, not
//! from the node's `submitblock` handler. The two are equivalent: the handler's catcher
//! (`dashd-node/src/rpc.rs:1230-1240`) reads the same signal, which V sends after ConnectBlock as
//! Core does (`dashd-chainstate/src/chain/abc.rs:696-710`). This certifies the block path, not
//! the RPC.
//!
//! One producer, a dashd with a wallet driven over RPC, mines the setup chain and builds every
//! transaction and block; two validators judge: a dashd and V, neither of which ever mines.
//! Setup blocks reach both validators through `submitblock` (V: `ProcessNewBlock`) and must be
//! accepted, which is checked as setup, apart from the probes. Every probe is built before any
//! validator sees it, and each one is judged by fresh validators on the setup chain: a dashd on a
//! copy of the setup validator's datadir, and a new V that the setup blocks are fed to again. A
//! rejected probe never feeds the next one.
//!
//! The chain (`CHAIN_ARGS`, regtest at the tag, chainparams.cpp): DIP0003 at 48 with
//! `-dip3params` (:1152-1170; the default is 432, :823); DIP0024 moved out to 1000 as in
//! `tests/v24` (default 1, :830); V20 and MN_RR at 100 with `-testactivationheight` (their
//! defaults are DIP0003's default height, :833-834, which `-dip3params` does not move); V24
//! through miner signalling from 150 with `-vbparams`' ninth field turning its `useEHF` off
//! (:852-859, :1097-1139), as `feature_masternode_shares.py` does. `withdrawals` (600, :835) and
//! `evo_shares` stay inactive; no row reads them. The producer mines 200 blocks, block
//! [`FROZEN_HEIGHT`] carrying an ordinary output paying the shared-collateral template script
//! before V24 is active (D13-B2), then a masternode registration in block 201 (the setup tip).
//!
//! Each row also runs its control (`control`, `b_control`): the probe with its winning defect
//! taken out must give the next check's reject on both nodes, which shows the probe reaches the
//! intended check with every earlier one passing. D13-B and D13-B2 ([`STAGE_ROWS`]) must also run
//! an order probe and an order control (`order_probe`/`b_order`, `order_control`/`b_order_control`):
//! blocks with a second defect, a tx spending a missing input, which Core answers from
//! ConnectBlock's CheckTxInputs loop, placed so that the stage answering the probe shows. Rows at a later stage are listed as skipped, with their stage, and
//! never pass.
//!
//! The rows file is checked first against [`INVENTORY`], this test's own copy of plan §4: every
//! row, each path with its stage and outcome, and each path that does not apply with its reason.
//! Then the closure: every EV1 §6.3 branch and pair, and every D13 case, is covered or recorded
//! as excluded, and each cover's reason is its row's reject.
//!
//! Needs dashd v24.0.0-rc.5: `DASHD_V24_PATH`, else setup-dashd's cache
//! (`DASHVERSION=24.0.0-rc.5 python3 contrib/setup-dashd.py`). `SKIP_DASHD_TESTS` skips the
//! dashd test; the rows' closure test always runs.

mod ev1_j;
mod v24;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::{
    Header, OutPoint, Transaction, TxIn, TxOut, core_serialize, decode_block, decode_tx_exact,
};
use dash_consensus::{Block, BlockValidationResult, Tx};
use dash_dml::specialtx::covenant::{SHARED_COLLATERAL_SCRIPT, is_shared_collateral_script};
use dash_primitives::{BlockHash, TxHash, TxType};
use dashd_chainstate::chain::{BlockState, ValidationInterface};
use dashd_mining::{Grind, grind};
use dashd_node::logging::Logger;
use dashd_node::sut::NodeSut;
use dashd_node::ui::Ui;
use serde_json::{Value, json};
use v24::{Dashd, deployment_json, hex};

/// The setup chain's parameters, on all three nodes (see the module docs).
const CHAIN_ARGS: [&str; 5] = [
    "-dip3params=48:48",
    "-testactivationheight=dip0024@1000",
    "-testactivationheight=v20@100",
    "-testactivationheight=mn_rr@100",
    "-vbparams=v24:0:999999999999:150:10:8:6:5:0",
];
/// The producer's: [`CHAIN_ARGS`], and its wallet writing without waiting for the disk
/// (`wallet/init.cpp:136`, a test-only option; validators run no wallet).
const PRODUCER_ARGS: [&str; 6] = [
    CHAIN_ARGS[0],
    CHAIN_ARGS[1],
    CHAIN_ARGS[2],
    CHAIN_ARGS[3],
    CHAIN_ARGS[4],
    "-unsafesqlitesync=1",
];
/// The stages this head runs (EV1-J plan rev4 §3; J0b's sub-stages, [`run_stage`]).
const REACHED: [&str; 3] = ["J0a", "J0b-1", "J0b-2"];
/// J0b's sub-stages: J0b-1 the quorum fixture's block rows, J0b-2 the assumevalid fixture's BAV
/// rows (§5.2), J0b-3 D14's devnet. The rows file and [`INVENTORY`] name them all J0b.
const SUB_STAGES: [&str; 3] = ["J0b-1", "J0b-2", "J0b-3"];
/// The reject of a type 9 row's pool control: the valid unlock in the probe's place on the
/// unchanged template, whose CbTx does not count it (`evo/specialtxman.cpp:1114`).
const POOL_REJECT: &str = "bad-cbtx-assetlocked-amount";
/// The reject of a BAV row's positive twin: its mode control with the scripts checked, whose
/// spend of a P2PKH output with an empty scriptSig fails at `OP_DUP` (`script/interpreter.cpp:703-704`,
/// `script/script_error.cpp:46-47`), reported inline at `-par=1` (`validation.cpp:2234`, `:2763-2765`).
const SCRIPT_REJECT: &str =
    "mandatory-script-verify-flag-failed (Operation not valid with the current stack size)";
const ROWS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-j-rows.toml");
const PAIRS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../contrib/node/ev1-pairs.toml");
const COIN: i64 = 100_000_000;
/// A quorum hash no node knows.
const UNKNOWN: [u8; 32] = [0x42; 32];
/// The txid of the order controls' missing input: no tx has it.
const MISSING: [u8; 32] = [0x55; 32];
/// The setup block whose hash the probes name as a known active-chain block: no quorum of any
/// type (the setup chain's commitments are all null).
const KNOWN_HEIGHT: u32 = 60;
/// Regtest's testdummy: in its window (start 0, no timeout) and not an EHF deployment
/// (chainparams.cpp:847-850; `useEHF` defaults to false).
const NON_EHF_BIT: u8 = 28;
/// Regtest's evo_shares: in its window and an EHF deployment (chainparams.cpp:861-868).
const EHF_BIT: u8 = 14;
/// The setup block with D13-B2's template output: past coinbase maturity and before V24, which
/// `-vbparams` activates no earlier than 150.
const FROZEN_HEIGHT: usize = 121;

// ---- rows as data -------------------------------------------------------------------------

/// One `[[row]]` of the rows file: its string fields, its `covers` list, and its `excludes` list
/// (closure items the row records as unobservable, with the reason in `notes`).
#[derive(Debug, Default)]
struct Row {
    fields: BTreeMap<String, String>,
    covers: Vec<String>,
    excludes: Vec<String>,
}

impl Row {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    fn id(&self) -> &str {
        self.get("id").expect("every row has an id")
    }
}

const PATHS: [&str; 4] = ["b", "bav", "m", "l2"];
/// The block-path outcome keys: each reject among them has `<key>_class`.
const BLOCK_OUTCOMES: [&str; 8] = [
    "b",
    "bav",
    "b_control",
    "b_order",
    "b_order_control",
    "b_pool_control",
    "bav_mode_control",
    "bav_twin",
];
/// The keys of a run row's block probes: its own case and each control.
const PROBE_KEYS: [&str; 5] = ["b", "b_control", "b_order", "b_order_control", "b_pool_control"];
/// The keys of a run row's BAV probes: its own case below assumevalid, its mode control and the
/// control's positive twin.
const BAV_PROBE_KEYS: [&str; 3] = ["bav", "bav_mode_control", "bav_twin"];
/// The rows whose stage evidence is an order probe and an order control (D13's two block rows:
/// both of their checks give `bad-shared-collateral-spend`, so only a second defect shows which
/// stage answers).
const STAGE_ROWS: [&str; 2] = ["D13-B", "D13-B2"];
/// The recorded exclusions of plan rev4 §4, with their rows: an item a row lists in `excludes`,
/// or covers while none of its paths applies (9-X unreachable, 9-P2 infeasible). The rows file must
/// record exactly these.
const EXCLUSIONS: [(&str, &str); 5] = [
    ("9c:13", "9-X"),
    ("9c:3>9c:13", "9-X"),
    ("9c:5>9c:13", "9-X"),
    ("9c:8>9c:11", "9-P2"),
    ("D13 covenant block view", "D13-B"),
];
const KEYS: [&str; 33] = [
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
];
const STAGES: [&str; 4] = ["J0a", "J0b", "J1", "J2"];
/// The prefixes of a path that does not apply.
const NOT_APPLICABLE: [&str; 3] = ["n/a: ", "excluded: ", "infeasible: "];

/// The EV1-J plan rev4 §4 inventory, independent of the rows file: each row, and for each of
/// [`PATHS`] (b, bav, m, l2) either `""` (the row has no such path), `"<stage> <outcome>"`, or
/// `"<n/a|excluded|infeasible>: <words of its reason>"`. §4's assumevalid "—" is `n/a`: BAV runs the
/// bad-signature rows only (§5.2). 9-P5a and 9-P5b are split per second defect as §4's stage
/// column splits them.
const INVENTORY: &[(&str, [&str; 4])] = &[
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
    // L2 replay.
    ("L2-mainnet", ["", "", "", L2]),
    ("L2-testnet", ["", "", "", L2]),
];
const BAV_NA: &str = "n/a: bad-signature rows only";
const TOO_MANY_OUTS_M: &str = "J1 bad-assetunlocktx-too-many-outs";
const L2: &str = "J2 every type 7 and 9 tx passes; block verdicts equal Core's";

/// `text` in the restricted shape (D29): blank lines, comments, `[[row]]`, `key = "string"`, and
/// `covers = [` or `excludes = [` with one `"string",` per line up to `]`.
fn parse_rows(text: &str) -> Vec<Row> {
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
                    _ => panic!("the lists are covers and excludes: {name}"),
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
fn is_reason(value: &str) -> bool {
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
fn balanced(text: &str) -> bool {
    text.chars().try_fold(0_usize, |depth, c| match c {
        '(' => Some(depth + 1),
        ')' => depth.checked_sub(1),
        _ => Some(depth),
    }) == Some(0)
}

fn is_reject(value: &str) -> bool {
    value != "accept" && is_reason(value)
}

fn not_applicable(value: &str) -> bool {
    NOT_APPLICABLE.iter().any(|p| value.starts_with(p))
}

/// The stage that runs `path` of `row`: its `<path>_stage`, with J0b split into
/// [`SUB_STAGES`] (BAV paths J0b-2, D14's rows J0b-3, every other J0b path J0b-1).
fn run_stage<'a>(row: &'a Row, path: &str) -> Option<&'a str> {
    let stage = row.get(&format!("{path}_stage"))?;
    Some(match (stage, path) {
        ("J0b", "bav") => SUB_STAGES[1],
        ("J0b", _) if row.id().starts_with("D14-") => SUB_STAGES[2],
        ("J0b", _) => SUB_STAGES[0],
        (stage, _) => stage,
    })
}

/// The class of a `<key>_class` value, `<Core's BlockValidationResult>: <file:line cite>`.
fn class_of(value: &str) -> Option<BlockValidationResult> {
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
fn row_problems(rows: &[Row]) -> Vec<String> {
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
                    // The row's own probe and a control on b; on bav also the control's twin.
                    let least = match path {
                        "b" => 2,
                        "bav" => 3,
                        _ => {
                            problems.push(format!("{id}: this head runs block paths only"));
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
                    if run_stage(row, path) == Some(SUB_STAGES[0])
                        && id.starts_with("9-")
                        && is_reject(value)
                        && !pool.contains(&Some(POOL_REJECT))
                    {
                        problems.push(format!("{id}: a type 9 reject needs its pool control"));
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
        for path in ["b", "bav"] {
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

/// Where the rows differ from [`INVENTORY`]: a §4 row missing or one §4 lacks, a path dropped or
/// added, or a path's stage, outcome or reason for not applying changed.
fn inventory_problems(rows: &[Row]) -> Vec<String> {
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
fn table_steps(text: &str, table: &str) -> Vec<(String, Vec<String>)> {
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
fn context_steps(text: &str, table: &str, first: &str) -> Vec<String> {
    let all: Vec<String> = table_steps(text, table).into_iter().map(|(s, _)| s).collect();
    let at = all.iter().position(|s| s == first).unwrap_or_else(|| panic!("{first}"));
    all[at..].to_vec()
}

/// D13's coins-view cases (EV1-J plan §4 D13), each with its reject: the masternode-list build's
/// collateral-spend reject (`FinishListForBlock`), the covenant on ConnectBlock's block view, the
/// covenant reached on B through an output no masternode holds, the covenant's mempool view, and
/// `ctx.coins`.
const D13_ITEMS: [(&str, &str); 5] = [
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
fn closure_items(pairs: &str) -> BTreeSet<String> {
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
fn closure_problems(rows: &[Row], pairs: &str) -> Vec<String> {
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
fn cover_problems(rows: &[Row], pairs: &str) -> Vec<String> {
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
fn rows_file_problems(rows_text: &str, pairs: &str) -> Vec<String> {
    let rows = parse_rows(rows_text);
    let mut problems = row_problems(&rows);
    problems.extend(inventory_problems(&rows));
    if problems.is_empty() {
        problems.extend(closure_problems(&rows, pairs));
        problems.extend(cover_problems(&rows, pairs));
    }
    problems
}

fn rows_text() -> String {
    std::fs::read_to_string(ROWS).expect("ev1-j-rows.toml")
}

fn pairs_text() -> String {
    std::fs::read_to_string(PAIRS).expect("ev1-pairs.toml")
}

// ---- the validators' verdicts ---------------------------------------------------------------

/// `submitblock_StateCatcher` on V's signals: the state of each block checked, by hash.
#[derive(Default)]
struct Checked(Mutex<Vec<(BlockHash, BlockState)>>);

impl ValidationInterface for Checked {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.0.lock().unwrap().push((block.hash(), state.clone()));
    }
}

/// `submitblock`'s answer for a checked block (`rpc/mining.cpp`'s `BIP22ValidationResult`).
fn bip22(state: &BlockState) -> Value {
    match state {
        BlockState::Valid => Value::Null,
        BlockState::Invalid(e) => json!(e.reject_reason()),
        BlockState::Error(msg) => json!(msg),
    }
}

/// `submitblock`'s answer as the table shows it.
fn show(answer: &Value) -> String {
    answer.as_str().unwrap_or("accept").to_owned()
}

/// What a row expects on a block: `submitblock`'s answer and, for a reject, Core's class as the
/// rows file cites it (`<key>_class`; dashd's class is not observed, see the module docs).
#[derive(Debug, PartialEq)]
struct Expected {
    answer: Value,
    class: Option<BlockValidationResult>,
}

impl Expected {
    /// The outcome under `key` of `row` (`b`, `b_control`, `b_order` or `b_order_control`).
    fn of(row: &Row, key: &str) -> Expected {
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
struct Ours {
    answer: Value,
    class: Option<BlockValidationResult>,
    accepted: bool,
}

impl Ours {
    /// Where V's `ProcessNewBlock` result disagrees with its `BlockChecked` state: a block V calls
    /// valid passed `AcceptBlock`, and a mutated one never passes `CheckBlock`. A block that did
    /// not pass them can be a reject of any class (`CheckBlock`'s `bad-txns-*` are
    /// BLOCK_CONSENSUS), so nothing more is asked.
    fn inconsistency(&self) -> Option<&'static str> {
        match (self.answer.is_null(), self.accepted, self.class) {
            (true, false, _) => Some("V answers valid but did not accept the block"),
            (_, true, Some(BlockValidationResult::Mutated)) => {
                Some("V accepted a block it calls mutated")
            }
            _ => None,
        }
    }
}

fn rpc(dashd: &Dashd, method: &str, params: Value) -> Value {
    let reply = dashd.reply(method, params.clone()).expect("a reply");
    assert!(reply["error"].is_null(), "dashd {method} {params}: {}", reply["error"]);
    reply["result"].clone()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn tx_of(hex: &Value) -> Tx {
    decode_tx_exact(&unhex(hex.as_str().expect("hex"))).expect("a tx").into_value()
}

fn block_of(hex: &Value) -> Block {
    let bytes = unhex(hex.as_str().expect("hex"));
    let mut input = bytes.as_slice();
    let block = decode_block(&mut input).expect("a block").into_value();
    assert!(input.is_empty(), "one block");
    block
}

/// The bytes Core keeps a hash in, from its displayed hex.
fn internal(display: &Value) -> [u8; 32] {
    let mut bytes: [u8; 32] = unhex(display.as_str().expect("a hash")).try_into().unwrap();
    bytes.reverse();
    bytes
}

fn copy_dir(from: &Path, to: &Path) {
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

fn sut_argv() -> Vec<&'static str> {
    [&["-regtest", "-printtoconsole=0"][..], &CHAIN_ARGS].concat()
}

/// A new V on `dir`, with `extra` arguments, the setup blocks fed to it (each must be accepted),
/// then the stale blocks (each stored, off the active chain), and its listener.
fn fresh_sut(
    dir: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
    extra: &[&str],
) -> (NodeSut, Arc<Checked>) {
    std::fs::create_dir_all(dir).unwrap();
    let ui = Arc::new(Ui::with_stderr(Arc::new(Logger::new()), Box::new(|_| {})));
    let argv = [&sut_argv()[..], extra].concat();
    let sut = NodeSut::new(dir, &argv, time, ui).expect("V starts");
    let checked = Arc::new(Checked::default());
    assert!(sut.register_listener(checked.clone()));
    let chain = sut.chain().expect("V runs");
    for (height, block) in (1..).zip(setup) {
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
struct Validators<'a> {
    binary: &'a Path,
    base: &'a Path,
    setup: &'a [Arc<Block>],
    stale: &'a [Arc<Block>],
    time: i64,
    scratch: &'a Path,
}

impl Validators<'_> {
    /// dashd's `submitblock` answer and V's verdict on `block`. A BAV probe's (`below`) are
    /// judged below assumevalid: both validators run `-assumevalid` at the last header of
    /// `below` and `-par=1`, and take `block`'s header and `below` first ([`bury_on_dashd`],
    /// [`bury_on_v`]); they run side by side.
    fn judge(&self, block: &Block, below: Option<&[Header]>) -> (Value, Ours) {
        let dir = tempfile::tempdir_in(self.scratch).unwrap();
        let bytes = core_serialize(block);
        let hash = block.hash();
        let extra: Vec<String> = below
            .map(|b| vec![format!("-assumevalid={}", b.last().unwrap().hash()), "-par=1".into()])
            .unwrap_or_default();
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();

        let core = || {
            copy_dir(self.base, &dir.path().join("dashd"));
            let args = [&CHAIN_ARGS[..], &extra].concat();
            let dashd = Dashd::start_at(self.binary, &dir.path().join("dashd"), self.time, &args);
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
            let (sut, checked) =
                fresh_sut(&dir.path().join("rdashd"), self.time, self.setup, self.stale, &extra);
            if let Some(below) = below {
                bury_on_v(&sut, block, below);
            }
            let mut input = bytes.as_slice();
            let ours = decode_block(&mut input).expect("V decodes dashd's bytes").into_value();
            assert!(input.is_empty() && ours.hash() == hash, "the same block");
            let chain = sut.chain().expect("V runs");
            let r = chain.process_block(Arc::new(ours), true, None, true).recv().expect("V");
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
            Ours {
                answer: bip22(&state),
                class,
                accepted: r.accepted,
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
fn bury_on_dashd(dashd: &Dashd, block: &Block, below: &[Header], height: usize) {
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
fn bury_on_v(sut: &NodeSut, block: &Block, below: &[Header]) {
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
fn setup_validators(
    binary: &Path,
    scratch: &Path,
    base: &Path,
    time: i64,
    setup: &[Arc<Block>],
    stale: &[Arc<Block>],
) -> PowParams {
    let tip = setup.last().unwrap().hash();
    let dashd = Dashd::start_at(binary, base, time, &CHAIN_ARGS);
    for (height, block) in (1..).zip(setup) {
        let answer = rpc(&dashd, "submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, Value::Null, "setup block {height} on dashd");
    }
    for block in stale {
        let answer = rpc(&dashd, "submitblock", json!([hex(&core_serialize(&**block))]));
        assert_eq!(answer, json!("inconclusive"), "stale block {} on dashd", block.hash());
    }
    assert_eq!(rpc(&dashd, "getbestblockhash", json!([])), json!(tip.to_string()));
    let dir = tempfile::tempdir_in(scratch).unwrap();
    let (sut, _) = fresh_sut(dir.path(), time, setup, stale, &[]);
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

// ---- the producer ---------------------------------------------------------------------------

/// The producer and what it built for the probes.
struct Produced {
    /// Blocks 1 to 201.
    setup: Vec<Arc<Block>>,
    /// The time every validator runs at, past every block the producer made.
    time: i64,
    /// An unsubmitted template on the setup tip (`generateblock` with no transactions).
    template: Block,
    /// A block of the setup chain, no quorum of any type.
    known: [u8; 32],
    /// The setup masternode's revocations, reason 0 and 1, neither broadcast.
    revocations: [Arc<Tx>; 2],
    /// The producer's wallet signing `tx`'s inputs.
    resign: Box<dyn Fn(&Tx) -> Tx>,
    /// The block mining the shared ProReg on the producer alone (invalidated there since), and
    /// its collateral outpoint.
    registration: (Arc<Block>, OutPoint),
    /// D13-B2: the template output of setup block [`FROZEN_HEIGHT`], and that tx's change output
    /// (a P2PKH output of the producer's wallet, locked there so no later setup tx spends it).
    frozen: (OutPoint, i64),
    change: (OutPoint, i64),
    p2pkh: Vec<u8>,
}

fn produce(producer: Dashd) -> Produced {
    let producer = Arc::new(producer);
    let p = &*producer;
    rpc(p, "createwallet", json!(["producer"]));
    let address = rpc(p, "getnewaddress", json!([]));
    let time = Cell::new(v24::T0);
    let tick = || {
        time.set(time.get() + 150);
        rpc(p, "setmocktime", json!([time.get()]));
    };
    let mine = |n: usize| -> Vec<Value> {
        (0..n)
            .map(|_| {
                tick();
                rpc(p, "generatetoaddress", json!([1, address]))[0].clone()
            })
            .collect()
    };
    let mut hashes = mine(FROZEN_HEIGHT - 1);

    // D13-B2: an ordinary tx paying the template script, mined while V24 is inactive, when no
    // creation rule applies (`CheckSharedCollateralTemplateOutputs` runs under `rules.v24`,
    // specialtxman.cpp:915-917). The output is no masternode's collateral. It is nonstandard
    // (policy.cpp:119-130), so `generateblock` mines it without the mempool.
    let frozen_value = 10 * COIN;
    let raw = Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: Vec::new(),
        outputs: vec![TxOut {
            value: frozen_value,
            script_pubkey: SHARED_COLLATERAL_SCRIPT.to_vec().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    });
    let funded =
        rpc(p, "fundrawtransaction", json!([hex(&core_serialize(&raw)), {"feeRate": 0.0001}]));
    let signed = rpc(p, "signrawtransactionwithwallet", json!([funded["hex"]]));
    assert_eq!(signed["complete"], json!(true));
    let frozen_tx = tx_of(&signed["hex"]);
    let outputs = &frozen_tx.raw().outputs;
    assert_eq!(outputs.len(), 2, "the template output and the change");
    let at = |template: bool| {
        let index = outputs
            .iter()
            .position(|o| is_shared_collateral_script(o.script_pubkey.as_bytes()) == template);
        let index = index.unwrap();
        let outpoint = OutPoint {
            hash: frozen_tx.txid(),
            index: u32::try_from(index).unwrap(),
        };
        (outpoint, outputs[index].value)
    };
    let (frozen, change) = (at(true), at(false));
    let parent = hashes.last().unwrap().clone();
    let at_parent = rpc(p, "getdeploymentinfo", json!([parent]))["deployments"]["v24"].clone();
    assert_eq!(at_parent["active"], json!(false), "V24 inactive for the next block: {at_parent}");
    tick();
    let mined = rpc(p, "generateblock", json!([address, [signed["hex"]]]))["hash"].clone();
    hashes.push(mined);
    rpc(
        p,
        "lockunspent",
        json!([false, [{"txid": frozen_tx.txid().to_string(), "vout": change.0.index}]]),
    );
    assert_eq!(
        rpc(p, "listlockunspent", json!([])),
        json!([{"txid": frozen_tx.txid().to_string(), "vout": change.0.index}]),
        "the change output is locked"
    );
    hashes.extend(mine(200 - FROZEN_HEIGHT));

    // A masternode with a known operator key, registered in block 201.
    let operator = rpc(p, "bls", json!(["generate"]));
    let fee_source = rpc(p, "getnewaddress", json!([]));
    rpc(p, "sendtoaddress", json!([fee_source, 10]));
    let new_address = || rpc(p, "getnewaddress", json!([]));
    let pro_tx = rpc(
        p,
        "protx",
        json!([
            "register_fund",
            new_address(),
            "127.0.0.1:19999",
            new_address(),
            operator["public"],
            new_address(),
            0,
            new_address(),
            address
        ]),
    );
    hashes.extend(mine(1));
    assert_eq!(rpc(p, "getrawmempool", json!([])), json!([]), "setup mined every tx");
    let deployments = rpc(p, "getdeploymentinfo", json!([]))["deployments"].clone();
    for name in ["dip0003", "v20", "mn_rr", "v24"] {
        assert_eq!(deployments[name]["active"], json!(true), "{name}: {}", deployments[name]);
    }
    let setup: Vec<Arc<Block>> =
        hashes.iter().map(|h| Arc::new(block_of(&rpc(p, "getblock", json!([h, 0]))))).collect();

    let revoke = |reason: u16| {
        let hex = rpc(
            p,
            "protx",
            json!(["revoke", pro_tx, operator["secret"], reason, fee_source, false]),
        );
        Arc::new(tx_of(&hex))
    };
    let revocations = [revoke(0), revoke(1)];
    let known = internal(&rpc(p, "getblockhash", json!([KNOWN_HEIGHT])));
    let p2pkh =
        unhex(rpc(p, "validateaddress", json!([new_address()]))["scriptPubKey"].as_str().unwrap());

    tick();
    let template = block_of(&rpc(p, "generateblock", json!([address, [], false]))["hex"]);

    // A shared ProReg (feature_masternode_shares.py's register_shared), never sent to a
    // validator: a funding tx without its collateral output, which shared_register_prepare adds.
    let collateral = 1000 * COIN;
    let mut pay = serde_json::Map::new();
    pay.insert(new_address().as_str().unwrap().to_owned(), json!(1000));
    let raw = rpc(p, "createrawtransaction", json!([[], pay]));
    let funded = tx_of(&rpc(p, "fundrawtransaction", json!([raw, {"feeRate": 0.0001}]))["hex"]);
    let mut funding = funded.to_mutable();
    funding.outputs.retain(|o| o.value != collateral);
    assert_eq!(funding.outputs.len(), funded.raw().outputs.len() - 1);
    let shares = json!([
        {"amount": 600 * COIN, "refundAddress": new_address(), "ownerAddress": new_address()},
        {"amount": 400 * COIN, "refundAddress": new_address(), "ownerAddress": new_address()},
    ]);
    let prepared = rpc(
        p,
        "protx",
        json!([
            "shared_register_prepare",
            hex(&core_serialize(&Tx::new(funding))),
            shares,
            "127.0.0.1:19998",
            rpc(p, "bls", json!(["generate"]))["public"],
            new_address(),
            0,
            30,
            50 * COIN
        ]),
    );
    let signatures = rpc(p, "protx", json!(["shared_sign", prepared["tx"]]))["signatures"].clone();
    let combined = rpc(p, "protx", json!(["shared_combine", prepared["tx"], signatures]));
    let signed = rpc(p, "signrawtransactionwithwallet", json!([combined]));
    assert_eq!(signed["complete"], json!(true));
    let shared = tx_of(&signed["hex"]);
    let index = u32::try_from(prepared["collateralIndex"].as_u64().unwrap()).unwrap();

    // Mined on the producer alone, so the block's coinbase commits to the registration as dashd's
    // miner does, then invalidated there: the producer is back on the setup tip.
    rpc(p, "sendrawtransaction", json!([signed["hex"]]));
    let mined = mine(1).remove(0);
    let registration = Arc::new(block_of(&rpc(p, "getblock", json!([mined, 0]))));
    assert!(registration.txs().iter().any(|tx| tx.txid() == shared.txid()));
    rpc(p, "invalidateblock", json!([mined]));
    assert_eq!(rpc(p, "getbestblockhash", json!([])), *hashes.last().unwrap());

    let signer = Arc::clone(&producer);
    let resign = Box::new(move |tx: &Tx| {
        let signed =
            rpc(&signer, "signrawtransactionwithwallet", json!([hex(&core_serialize(tx))]));
        assert_eq!(signed["complete"], json!(true));
        tx_of(&signed["hex"])
    });
    Produced {
        setup,
        time: time.get(),
        template,
        known,
        revocations,
        resign,
        registration: (
            registration,
            OutPoint {
                hash: shared.txid(),
                index,
            },
        ),
        frozen,
        change,
        p2pkh,
    }
}

// ---- the probes -----------------------------------------------------------------------------

/// A type 7 signal for `bit` naming `quorum`, with a well-formed signature that no quorum made
/// (`mnhftx.h`'s payload: version 1, the bit, the quorum hash, the signature).
fn mnhf(bit: u8, quorum: &[u8; 32], sig: &[u8]) -> Arc<Tx> {
    Arc::new(Tx::new(Transaction {
        version: 3,
        tx_type: TxType::new(7),
        inputs: Vec::new(),
        outputs: Vec::new(),
        lock_time: 0,
        extra_payload: [&[1, bit][..], quorum, sig].concat(),
    }))
}

/// A type 9 unlock of J0a's: index 77, a fee of 1000 and `outputs` P2PKH outputs of 1000.
fn unlock(
    version: u8,
    outputs: usize,
    requested: u32,
    quorum: &[u8; 32],
    sig: &[u8],
    p2pkh: &[u8],
) -> Arc<Tx> {
    unlock_with(version, 77, 1000, 1000, outputs, requested, quorum, sig, p2pkh)
}

/// A type 9 unlock with `outputs` P2PKH outputs of `value` (`assetlocktx.h:109-119`'s payload:
/// version, index, fee, requested height, quorum hash, signature).
#[allow(clippy::too_many_arguments)]
fn unlock_with(
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
fn probe_block(template: &Block, extra: &[Arc<Tx>], pow: &PowParams) -> Arc<Block> {
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
struct Probe {
    row: &'static str,
    name: String,
    expect: &'static str,
    block: Arc<Block>,
    below: Option<Vec<Header>>,
}

impl Probe {
    fn control(&self) -> bool {
        !["b", "bav"].contains(&self.expect)
    }
}

/// The J0a rows' probes and controls, built before any validator sees them.
fn probes(made: &Produced, pow: &PowParams) -> Vec<Probe> {
    let mut out = Vec::new();
    let mut add = |row: &'static str,
                   name: &str,
                   expect: &'static str,
                   txs: Vec<Arc<Tx>>,
                   template: &Block| {
        out.push(Probe {
            row,
            name: name.to_owned(),
            expect,
            block: probe_block(template, &txs, pow),
            below: None,
        })
    };
    let t = &made.template;
    let [rev0, rev1] = &made.revocations;
    // A well-formed basic-scheme BLS signature: the setup masternode's operator's.
    let sig = &rev0.raw().extra_payload[rev0.raw().extra_payload.len() - 96..];
    let tip = made.setup.len() as u32;
    let known = &made.known;
    let p2pkh = &made.p2pkh;

    add("7-S1", "bit 28, known block", "b", vec![mnhf(NON_EHF_BIT, known, sig)], t);
    add("7-S1", "bit 14, known block", "b_control", vec![mnhf(EHF_BIT, known, sig)], t);
    add("7-P1", "bit 28, unknown hash", "b", vec![mnhf(NON_EHF_BIT, &UNKNOWN, sig)], t);
    add("7-P1", "bit 14, unknown hash", "b_control", vec![mnhf(EHF_BIT, &UNKNOWN, sig)], t);
    for (row, quorum, controls) in
        [("9-P5a.1", &UNKNOWN, &[1][..]), ("9-P5a.2", known, &[1, 2][..])]
    {
        for version in [0, 3] {
            let tx = unlock(version, 1, tip, quorum, sig, p2pkh);
            add(row, &format!("payload version {version}"), "b", vec![tx], t);
        }
        for &version in controls {
            let tx = unlock(version, 1, tip, quorum, sig, p2pkh);
            add(row, &format!("payload version {version}"), "b_control", vec![tx], t);
        }
    }
    for (row, quorum) in [("9-P5b.1", &UNKNOWN), ("9-P5b.2", known)] {
        add(row, "33 outputs", "b", vec![unlock(1, 33, tip, quorum, sig, p2pkh)], t);
        add(row, "32 outputs", "b_control", vec![unlock(1, 32, tip, quorum, sig, p2pkh)], t);
    }

    // The reason-0 revocation with the reason-1 one's signature: the operator's key over another
    // payload. Its inputs signed again, as the payload is part of what they sign.
    let tail = |tx: &Tx| tx.raw().extra_payload[tx.raw().extra_payload.len() - 96..].to_vec();
    let mut bad = rev0.to_mutable();
    let n = bad.extra_payload.len();
    bad.extra_payload[n - 96..].copy_from_slice(&tail(rev1));
    let bad = Arc::new((made.resign)(&Tx::new(bad)));
    // The same payload naming no masternode (`CProUpRevTx`: version, proTxHash, ...), on an input
    // no check before CheckProUpRevTx's masternode lookup reads.
    let mut nobody = rev0.to_mutable();
    nobody.extra_payload[2..34].copy_from_slice(&UNKNOWN);
    nobody.inputs = vec![TxIn {
        prevout: OutPoint {
            hash: TxHash::from([0x77; 32]),
            index: 0,
        },
        script_sig: Vec::new().into(),
        sequence: u32::MAX,
    }];
    let nobody = Arc::new(Tx::new(nobody));
    add("OPSIG-B", "bad signature, then no masternode", "b", vec![bad.clone(), nobody.clone()], t);
    add(
        "OPSIG-B",
        "good signature, then no masternode",
        "b_control",
        vec![rev0.clone(), nobody.clone()],
        t,
    );
    add("OPSIG-B", "no masternode, then bad signature", "b_control", vec![nobody, bad], t);

    // D13-B2: an ordinary tx spending setup block FROZEN_HEIGHT's template output (anyone can
    // spend it at the script layer: an empty scriptSig), on the setup tip, where V24 is active.
    // No masternode has it as collateral, so FinishListForBlock passes it and ConnectBlock's
    // covenant answers (validation.cpp:2716-2725). The control spends the same tx's change output
    // instead, signed by the producer's wallet. Neither pays a fee: the template's coinbase pays the
    // masternode its share of a block without fees, and a fee would give `bad-cb-payee`.
    let spend_of = |(prevout, value): (OutPoint, i64)| Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value,
            script_pubkey: p2pkh.clone().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    };
    let frozen = Arc::new(Tx::new(spend_of(made.frozen)));
    let ordinary = Arc::new((made.resign)(&Tx::new(spend_of(made.change))));
    // The order controls' second defect: an ordinary tx spending an outpoint no tx made, which
    // Core answers from ConnectBlock's CheckTxInputs (validation.cpp:2709-2713), after
    // ProcessSpecialTxsInBlock (:2674) and, in the per-tx loop, before that tx's covenant
    // (:2719-2725).
    let missing = Arc::new(Tx::new(spend_of((
        OutPoint {
            hash: TxHash::from(MISSING),
            index: 0,
        },
        COIN,
    ))));
    add("D13-B2", "a spend of the pre-V24 template output", "b", vec![frozen.clone()], t);
    add(
        "D13-B2",
        "the template spend, then a missing input",
        "b_order",
        vec![frozen.clone(), missing.clone()],
        t,
    );
    add("D13-B2", "a spend of an ordinary output", "b_control", vec![ordinary], t);
    add(
        "D13-B2",
        "a missing input, then the template spend",
        "b_order_control",
        vec![missing.clone(), frozen],
        t,
    );

    let (registration, collateral) = &made.registration;
    let spend = Arc::new(Tx::new(Transaction {
        version: 2,
        tx_type: TxType::new(0),
        inputs: vec![TxIn {
            prevout: *collateral,
            script_sig: Vec::new().into(),
            sequence: u32::MAX,
        }],
        outputs: vec![TxOut {
            value: 999 * COIN,
            script_pubkey: p2pkh.clone().into(),
        }],
        lock_time: 0,
        extra_payload: Vec::new(),
    }));
    // The list build (ProcessSpecialTxsInBlock, validation.cpp:2674) runs before ConnectBlock's
    // CheckTxInputs loop (:2699-2713), so it answers the spend even behind a missing input.
    add(
        "D13-B",
        "registration, then a spend of its collateral",
        "b",
        vec![spend.clone()],
        registration,
    );
    add(
        "D13-B",
        "registration, a missing input, then the collateral spend",
        "b_order",
        vec![missing.clone(), spend],
        registration,
    );
    add("D13-B", "the registration block", "b_control", Vec::new(), registration);
    add(
        "D13-B",
        "registration, then a missing input",
        "b_order_control",
        vec![missing],
        registration,
    );
    out
}

/// OD-CP-64's tie between a BAV row's probes, `mine`: the row's own probe and its mode controls
/// sit [`ev1_j::bav::BURIED`] headers deep and its twins [`ev1_j::bav::TWO_WEEKS`], each header
/// chain on its own probe's block, and each twin is a mode control's block under the first of
/// that control's headers. The judges see only the headers they get, so a probe buried shallower
/// than its mode control would have its scripts checked and the control would prove nothing about
/// it.
fn bav_depths(id: &str, mine: &[&Probe]) {
    fn below(p: &Probe) -> &[Header] {
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
fn judge_stage(
    rows: &[Row],
    stage: &str,
    path: &str,
    probes: &[Probe],
    validators: &Validators<'_>,
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
    let verdicts = judge_all(probes, validators, workers, started);
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
            "bav_mode_control" => " mode control",
            "bav_twin" => " positive twin",
            _ => " order control",
        };
        let name = format!("{}{kind} ({})", probe.row, probe.name);
        table.push(format!(
            "{name:<96} dashd {:<32} V {:<32} expected {}",
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
    for row in rows {
        for path in PATHS {
            if let Some(stage) = run_stage(row, path)
                && !REACHED.contains(&stage)
            {
                eprintln!("skipped: {} {path}, stage {stage}", row.id());
            }
        }
    }
    eprintln!("{}", table.join("\n"));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// How many BAV probes are judged at once.
const BAV_WORKERS: usize = 5;

/// Each probe's verdicts ([`Validators::judge`]), in order, judged `workers` at a time.
fn judge_all(
    probes: &[Probe],
    validators: &Validators<'_>,
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
            let verdict = validators.judge(&probe.block, probe.below.as_deref());
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

/// Under `sut::`, the gate's feature-step filter (`[package.metadata.gate]`).
mod sut {
    use super::*;

    #[test]
    fn rows_close_over_ev1_6_3() {
        let problems = rows_file_problems(&rows_text(), &pairs_text());
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[test]
    fn row_problems_are_found() {
        let text = "[[row]]\nid = \"x\"\ncase = \"c\"\nrecipe = \"r\"\nb = \"bad-x\"\nb_stage = \"J0a\"\n\
                    m = \"n/a: no\"\nm_stage = \"J1\"\nbav = \"bad-y\"\nfoo = \"z\"\n\
                    order_control = \"o\"\nb_control_class = \"BLOCK_CONSENSUS: x.cpp:1\"\n\
                    excludes = [\n  \"D13 list build\",\n]\n";
        assert_eq!(
            row_problems(&parse_rows(text)),
            [
                "x: unknown key foo",
                "x: a run row pins its probe count in b_probes",
                "x: a run row needs control and b_control",
                "x: bav has no stage",
                "x: m does not apply but has a stage",
                "x: order_control and b_order_control go together",
                "x: b is a reject without b_class",
                "x: bav is a reject without bav_class",
                "x: b_control_class without a reject",
                "x: excludes without notes",
            ]
        );
    }

    /// The rows file with `edit` applied to row `id`'s text, from its `[[row]]` to the next.
    fn edit_row(text: &str, id: &str, edit: impl Fn(&str) -> String) -> String {
        let marker = format!("id = \"{id}\"\n");
        let mut parts: Vec<String> = text.split("[[row]]\n").map(str::to_owned).collect();
        let at = parts.iter().position(|p| p.starts_with(&marker)).expect("the row");
        let edited = edit(&parts[at]);
        assert_ne!(edited, parts[at], "the edit changes {id}");
        parts[at] = edited;
        parts.join("[[row]]\n")
    }

    /// The rows file without row `id`.
    fn without_row(text: &str, id: &str) -> String {
        let marker = format!("id = \"{id}\"\n");
        let parts: Vec<&str> = text.split("[[row]]\n").collect();
        let kept: Vec<&str> = parts.iter().copied().filter(|p| !p.starts_with(&marker)).collect();
        assert_eq!(kept.len() + 1, parts.len(), "one row {id}");
        kept.join("[[row]]\n")
    }

    /// `row` without `lines`, each there once.
    fn cut(row: &str, lines: &str) -> String {
        assert_eq!(row.matches(lines).count(), 1, "{lines}");
        row.replace(lines, "")
    }

    /// What the check finds in the rows file with one edit.
    fn problems_after(edit: impl FnOnce(&str) -> String) -> Vec<String> {
        rows_file_problems(&edit(&rows_text()), &pairs_text())
    }

    #[test]
    fn a_dropped_m_path_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S1", |r| cut(r, "m = \"bad-mnhf-non-ehf\"\nm_stage = \"J1\"\n"))
        });
        assert_eq!(problems, ["7-S1 m: missing, §4 has J1 bad-mnhf-non-ehf"]);
    }

    #[test]
    fn a_moved_stage_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S1", |r| r.replace("b_stage = \"J0a\"", "b_stage = \"J0b\""))
        });
        assert_eq!(problems, ["7-S1 b: bad-mnhf-non-ehf at J0b, §4 has J0a bad-mnhf-non-ehf"]);
    }

    #[test]
    fn a_dropped_d14_row_fails() {
        for id in ["D14-33", "D14-34"] {
            let problems = problems_after(|t| without_row(t, id));
            assert_eq!(problems, [format!("{id}: the §4 row is missing")]);
        }
    }

    #[test]
    fn a_dropped_pair_fails() {
        let problems = problems_after(|t| edit_row(t, "7-P1", |r| cut(r, "  \"7c:4>7c:5a\",\n")));
        assert_eq!(problems, ["7c:4>7c:5a: no row covers or excludes it"]);
    }

    #[test]
    fn an_unrecorded_exclusion_fails() {
        // 7c:8 moved from 7-S5 to 9-X, which no path of applies, and D13's list build moved from
        // D13-B's covers to its excludes.
        let problems = problems_after(|t| {
            let t = edit_row(t, "7-S5", |r| cut(r, "  \"7c:8\",\n"));
            edit_row(&t, "9-X", |r| r.replace("  \"9c:13\",\n", "  \"9c:13\",\n  \"7c:8\",\n"))
        });
        assert_eq!(problems, ["7c:8 (9-X): an exclusion §4 does not record"]);
        let problems = problems_after(|t| {
            edit_row(t, "D13-B", |r| {
                cut(r, "covers = [\n  \"D13 list build\",\n]\n")
                    .replace("excludes = [\n", "excludes = [\n  \"D13 list build\",\n")
            })
        });
        assert_eq!(problems, ["D13 list build (D13-B): an exclusion §4 does not record"]);
    }

    #[test]
    fn a_dropped_order_probe_fails() {
        for id in STAGE_ROWS {
            let problems = problems_after(|t| {
                edit_row(t, id, |r| {
                    r.lines()
                        .filter(|l| !l.starts_with("order_probe =") && !l.starts_with("b_order ="))
                        .filter(|l| !l.starts_with("b_order_class ="))
                        .map(|l| format!("{l}\n"))
                        .collect()
                })
            });
            assert_eq!(problems, [format!("{id}: a stage row needs b_order and b_order_control")]);
        }
    }

    #[test]
    fn a_cover_on_another_reject_fails() {
        // 7c:8 (bad-mnhf-invalid) moved from 7-S5 to 7-S1 (bad-mnhf-non-ehf).
        let problems = problems_after(|t| {
            let t = edit_row(t, "7-S5", |r| cut(r, "  \"7c:8\",\n"));
            edit_row(&t, "7-S1", |r| r.replace("  \"7c:4\",\n", "  \"7c:4\",\n  \"7c:8\",\n"))
        });
        assert_eq!(
            problems,
            ["7-S1: covers 7c:8 (bad-mnhf-invalid), but its rejects are \
                 [\"bad-mnhf-non-ehf\", \"bad-mnhf-non-ehf\"]"]
        );
    }

    /// `row` without the lines starting with any of `keys` and " =".
    fn without_keys(row: &str, keys: &[&str]) -> String {
        row.lines()
            .filter(|l| !keys.iter().any(|k| l.starts_with(&format!("{k} ="))))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn a_dropped_pool_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| {
                without_keys(r, &["pool_control", "b_pool_control", "b_pool_control_class"])
            })
        });
        assert_eq!(problems, ["9-P1: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn a_pool_control_without_its_outcome_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| without_keys(r, &["b_pool_control", "b_pool_control_class"]))
        });
        assert_eq!(
            problems,
            [
                "9-P1: a type 9 reject needs its pool control",
                "9-P1: pool_control and b_pool_control go together",
            ]
        );
    }

    #[test]
    fn a_pool_control_on_another_reject_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P1", |r| {
                r.replace(
                    "b_pool_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_pool_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-P1: a type 9 reject needs its pool control",
                "9-P1: b_pool_control bad-assetunlock-not-verified, a pool control's is \
                 bad-cbtx-assetlocked-amount",
            ]
        );
    }

    #[test]
    fn a_one_defect_unlock_row_without_its_pool_control_fails() {
        // 9-S5's control, the valid unlock in its place, swapped for an unlock signed by another
        // key: a reject of the special-tx loop, which shows nothing about the stage.
        let problems = problems_after(|t| {
            edit_row(t, "9-S5", |r| {
                r.replace(
                    "b_control = \"bad-cbtx-assetlocked-amount\"",
                    "b_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(problems, ["9-S5: a type 9 reject needs its pool control"]);
    }

    #[test]
    fn a_run_row_without_its_probe_count_fails() {
        let problems =
            problems_after(|t| edit_row(t, "9-OK", |r| r.replace("b_probes = \"4\"\n", "")));
        assert_eq!(problems, ["9-OK: a run row pins its probe count in b_probes"]);
    }

    #[test]
    fn a_pair_control_off_the_loser_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-P4", |r| {
                r.replace(
                    "b_control = \"bad-assetunlock-not-verified\"",
                    "b_control = \"bad-assetunlock-too-late\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-P4: covers 9c:12>9c:14, but b_control bad-assetunlock-too-late is not 9c:14's reject"
            ]
        );
    }

    #[test]
    fn a_script_error_is_a_reason() {
        assert!(is_reason(SCRIPT_REJECT));
        assert!(is_reason("block-validation-failed"));
        assert!(is_reason(
            "mandatory-script-verify-flag-failed (Signature must be zero for failed CHECK(MULTI)SIG operation)"
        ));
        for not in [
            "",
            "Bad-x",
            "bad-x (open",
            "bad-x (\"quoted\")",
            " (text)",
            "bad x",
            "bad-x ()",
            "bad-x (a) (b)",
            "bad-x (a)b)",
        ] {
            assert!(!is_reason(not), "{not}");
        }
    }

    #[test]
    fn a_bav_run_row_without_its_mode_control_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-S5", |r| without_keys(r, &["mode_control", "bav_mode_control"]))
        });
        assert_eq!(problems, ["7-S5: a BAV run row needs its mode control, accept"]);
    }

    #[test]
    fn a_mode_control_that_rejects_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-S5", |r| {
                r.replace(
                    "bav_mode_control = \"accept\"",
                    "bav_mode_control = \"bad-assetunlock-not-verified\"",
                )
            })
        });
        assert_eq!(
            problems,
            [
                "9-S5: a BAV run row needs its mode control, accept",
                "9-S5: bav_mode_control is a reject without bav_mode_control_class",
            ]
        );
    }

    #[test]
    fn a_bav_run_row_without_its_twin_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-OK", |r| without_keys(r, &["twin", "bav_twin", "bav_twin_class"]))
        });
        assert_eq!(
            problems,
            [format!("9-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}")]
        );
        let problems = problems_after(|t| edit_row(t, "9-OK", |r| without_keys(r, &["twin"])));
        assert_eq!(
            problems,
            [
                format!("9-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}"),
                "9-OK: twin and bav_twin go together".to_owned(),
            ]
        );
    }

    #[test]
    fn a_twin_on_another_reject_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "7-OK", |r| {
                r.replace(
                    &format!("bav_twin = \"{SCRIPT_REJECT}\""),
                    "bav_twin = \"block-validation-failed\"",
                )
            })
        });
        assert_eq!(
            problems,
            [format!("7-OK: a BAV run row needs its positive twin, {SCRIPT_REJECT}")]
        );
    }

    #[test]
    fn a_bav_run_row_without_its_probe_count_fails() {
        for edit in ["", "bav_probes = \"2\"\n"] {
            let problems = problems_after(|t| {
                edit_row(t, "9-S5", |r| r.replace("bav_probes = \"3\"\n", edit))
            });
            assert_eq!(problems, ["9-S5: a run row pins its probe count in bav_probes"]);
        }
    }

    #[test]
    fn a_probe_count_on_a_path_not_run_fails() {
        let problems = problems_after(|t| {
            edit_row(t, "9-R3", |r| {
                r.replace("b_probes = \"5\"\n", "b_probes = \"5\"\nbav_probes = \"3\"\n")
            })
        });
        assert_eq!(problems, ["9-R3: bav_probes on a path this head does not run"]);
        let problems = problems_after(|t| {
            edit_row(t, "D14-34", |r| {
                r.replace("b_stage = \"J0b\"\n", "b_stage = \"J0b\"\nb_probes = \"2\"\n")
            })
        });
        assert_eq!(problems, ["D14-34: b_probes on a path this head does not run"]);
    }

    #[test]
    fn block_paths_map_to_their_sub_stages() {
        let rows = parse_rows(&rows_text());
        let row = |id: &str| rows.iter().find(|r| r.id() == id).expect("the row");
        assert_eq!(run_stage(row("9-S5"), "b"), Some("J0b-1"));
        assert_eq!(run_stage(row("9-S5"), "bav"), Some("J0b-2"));
        assert_eq!(run_stage(row("D14-33"), "b"), Some("J0b-3"));
        assert_eq!(run_stage(row("7-S1"), "b"), Some("J0a"));
    }

    #[test]
    fn j0b1_block_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = ev1_j::produce(&binary, &scratch.path().join("quorums"));
        eprintln!("quorum chain produced at {:?}", started.elapsed());
        let base = scratch.path().join("validator");
        let pow =
            setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &made.stale);
        eprintln!("quorum chain on fresh validators at {:?}", started.elapsed());
        let probes = ev1_j::probes::probes(&made, &binary, scratch.path(), &pow);
        let validators = Validators {
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &made.stale,
            time: made.time,
            scratch: scratch.path(),
        };
        let rows = parse_rows(&rows_text());
        judge_stage(&rows, "J0b-1", "b", &probes, &validators, started);
    }

    /// J0b-2: the BAV rows on fresh validators below assumevalid (`ev1_j::bav`).
    #[test]
    fn j0b2_bav_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = ev1_j::produce(&binary, &scratch.path().join("quorums"));
        let base = scratch.path().join("validator");
        let pow =
            setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &made.stale);
        eprintln!("quorum chain on fresh validators at {:?}", started.elapsed());
        let probes = ev1_j::bav::probes(&made, &binary, scratch.path(), &pow);
        eprintln!("BAV probes and their headers built at {:?}", started.elapsed());
        let validators = Validators {
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &made.stale,
            time: made.time,
            scratch: scratch.path(),
        };
        let rows = parse_rows(&rows_text());
        judge_stage(&rows, "J0b-2", "bav", &probes, &validators, started);
    }

    #[test]
    fn j0a_block_rows_equal_dashds() {
        assert!(rows_file_problems(&rows_text(), &pairs_text()).is_empty());
        let rows = parse_rows(&rows_text());
        let Some(binary) = v24::dashd_binary() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let made = produce(Dashd::start_at(
            &binary,
            &scratch.path().join("producer"),
            v24::T0,
            &PRODUCER_ARGS,
        ));
        eprintln!("produced at {:?}", started.elapsed());

        let base = scratch.path().join("validator");
        let pow = setup_validators(&binary, scratch.path(), &base, made.time, &made.setup, &[]);

        eprintln!("setup validators at {:?}", started.elapsed());
        let probes = probes(&made, &pow);
        let validators = Validators {
            binary: &binary,
            base: &base,
            setup: &made.setup,
            stale: &[],
            time: made.time,
            scratch: scratch.path(),
        };
        judge_stage(&rows, "J0a", "b", &probes, &validators, started);
    }
}
