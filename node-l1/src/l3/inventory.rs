//! The rc.4 inventory (plan §6.2, `contrib/node/l3-census/inventory-v24.0.0-rc.4.json`) and the
//! mechanical occurrence binding over it (plan §3.5).
//!
//! A reply binds in five steps: the rows whose reason template matches it; those on the trial's
//! mode's entry paths; those whose guards the observed facts allow; those whose entry path gives
//! the effects dashd showed; then one survivor is `bound`, several `ambiguous` (no site credit),
//! none operator drift (a harness error).

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::chain::mutate::{Mode, Par};

/// The fixture, compiled in: the census and the binder read one file.
pub const FIXTURE: &str =
    include_str!("../../../contrib/node/l3-census/inventory-v24.0.0-rc.4.json");

/// One row of the fixture. Only the fields the binder reads; the scanner checks the rest.
#[derive(Clone, Debug, Deserialize)]
pub struct Row {
    pub key: String,
    pub kind: String,
    #[serde(default)]
    pub function: String,
    /// The row's line in Core's source at the fixture's tag (an annotated row may have none).
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub callee: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub guards: BTreeMap<String, serde_json::Value>,
    /// The E-evo key the row's site counts under (plan §3.6's table).
    #[serde(default)]
    pub evo_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Fixture {
    provenance: Provenance,
    script_errors: Vec<String>,
    rows: Vec<Row>,
}

#[derive(Debug, Deserialize)]
pub struct Provenance {
    pub tag: String,
    pub commit: String,
}

/// The inventory: its R rows (the binder's candidates), the wrapper classes and `%s`'s set.
#[derive(Debug)]
pub struct Inventory {
    pub provenance: Provenance,
    pub rejects: Vec<Row>,
    /// Wrapped function → the wrapper's outer class (`B_CONSENSUS`).
    wrappers: BTreeMap<String, String>,
    script_errors: Vec<String>,
    /// Every row's E-evo key (§3.6's table).
    pub evo_keys: BTreeSet<String>,
    /// ConnectBlock's call of ProcessSpecialTxsInBlock, and that function's step 11 (the MNHF
    /// ProcessBlock), by line: the anchors of `before_mnhf_step`.
    special_call: u32,
    mnhf_step: u32,
    /// ProcessSpecialTxsInBlock's step 4, the CheckSpecialTxInner loop: the anchor of `reach`.
    stx_step: u32,
}

/// How far a body got into ProcessSpecialTxsInBlock's step 4 (the E-evo hook's loop), from
/// where it was refused (review r1 M-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Refused before step 4: no transaction entered it.
    None,
    /// Refused inside step 4 (CheckSpecialTxInner): the transactions up to the refused one.
    Partial,
    /// Past step 4, or accepted: every transaction.
    Full,
}

/// The fixture rows that anchor `Inventory::before_mnhf_step`.
const SPECIAL_CALL: &str = "validation.cpp:ConnectBlock@E:ProcessSpecialTxsInBlock";
const MNHF_STEP: &str = "specialtxman.cpp:ProcessSpecialTxsInBlock@E-evo:MnhfProcessBlock";
const STX_STEP: &str = "specialtxman.cpp:ProcessSpecialTxsInBlock@W:CheckSpecialTxInner";

/// Where a row's function sits on Core's paths, which fixes what a reject there leaves behind
/// (rev1 §3.3's path table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryPath {
    /// ProcessNewBlock's CheckBlock family: no index entry.
    Check,
    /// AcceptBlockHeader and its ContextualCheckBlockHeader: no index entry.
    Header,
    /// AcceptBlock's ContextualCheckBlock: indexed and marked failed (unless MUTATED), no data.
    Contextual,
    /// ConnectBlock and below: indexed, stored, marked failed (unless MUTATED).
    Connect,
    /// TestBlockValidity's own rows: proposals only.
    Tbv,
}

/// What dashd shows of a submitted block after the op (P2): whether it is in the index, marked
/// failed, and stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effects {
    pub known: bool,
    pub failed: bool,
    pub stored: bool,
}

impl Effects {
    /// Not in the index.
    pub const NONE: Effects = Effects {
        known: false,
        failed: false,
        stored: false,
    };
}

impl EntryPath {
    pub fn of(function: &str) -> Option<EntryPath> {
        Some(match function {
            "CheckBlockHeader" | "CheckMerkleRoot" | "CheckBlock" | "CheckTransaction" => {
                EntryPath::Check
            }
            "ContextualCheckBlockHeader" | "AcceptBlockHeader" => EntryPath::Header,
            "ContextualCheckBlock" | "ContextualCheckTransaction" => EntryPath::Contextual,
            "ConnectBlock"
            | "CheckInputScripts"
            | "Consensus::CheckTxInputs"
            | "CheckSpecialTxInner"
            | "ProcessSpecialTxsInBlock" => EntryPath::Connect,
            "TestBlockValidity" => EntryPath::Tbv,
            _ => return None,
        })
    }

    /// The effects a submit rejected on this path leaves, for a block unknown before; `mutated`
    /// for a B_MUTATED row, which Core never marks (v:4827, v:2088).
    pub fn effects(self, mutated: bool) -> Option<Effects> {
        let (known, failed, stored) = match self {
            EntryPath::Check | EntryPath::Header => (false, false, false),
            EntryPath::Contextual => (true, !mutated, false),
            EntryPath::Connect => (true, !mutated, true),
            EntryPath::Tbv => return None,
        };
        Some(Effects {
            known,
            failed,
            stored,
        })
    }
}

/// The parent's index state before the trial, read on both sides (plan §3.5 step 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentState {
    /// Not failed, and no failed-set block below it.
    Clean,
    /// Marked BLOCK_FAILED_*.
    Failed,
    /// Unmarked, not VALID_SCRIPTS, descending from a block in Core's failed set.
    UnmarkedFailedAncestor,
}

/// The facts a trial observed, for the guards (plan §3.5 step 3).
#[derive(Clone, Debug)]
pub struct Facts {
    /// `getblockchaininfo.chain`.
    pub network: String,
    /// DIP3 active for the block (`getdeploymentinfo <parent>`).
    pub dip3: bool,
    /// A decoded CbTx in the block.
    pub cbtx: bool,
    pub mnsync_synced: bool,
    pub par: Par,
    pub parent: ParentState,
    /// Facts measured from the block's bytes and chain-only coin heights; a guard whose fact is
    /// absent keeps its row, and a result that rests on one is never `bound`.
    pub measured: BTreeMap<&'static str, bool>,
}

/// A binding (plan §3.5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Binding {
    Bound(String),
    /// More than one row survives, or one survives only on an unmeasured guard: no credit.
    Ambiguous(Vec<String>),
    /// None: operator drift, a harness error.
    Unbound,
}

impl Inventory {
    pub fn load() -> Result<Inventory, String> {
        Self::parse(FIXTURE)
    }

    pub fn parse(json: &str) -> Result<Inventory, String> {
        let fixture: Fixture =
            serde_json::from_str(json).map_err(|e| format!("the inventory fixture: {e}"))?;
        let evo_keys = fixture.rows.iter().filter_map(|r| r.evo_key.clone()).collect();
        let line = |key: &str| {
            fixture
                .rows
                .iter()
                .find(|r| r.key == key)
                .and_then(|r| r.line)
                .ok_or_else(|| format!("the inventory fixture has no line for {key}"))
        };
        let (special_call, mnhf_step, stx_step) =
            (line(SPECIAL_CALL)?, line(MNHF_STEP)?, line(STX_STEP)?);
        let mut wrappers = BTreeMap::new();
        for row in fixture.rows.iter().filter(|r| r.kind == "W") {
            wrappers.insert(row.callee.clone(), row.result.clone());
        }
        let rejects: Vec<Row> = fixture.rows.into_iter().filter(|r| r.kind == "R").collect();
        for row in &rejects {
            if EntryPath::of(&row.function).is_none() {
                return Err(format!("{}: no entry path for {}", row.key, row.function));
            }
            if row.result.starts_with("T_") && !wrappers.contains_key(&row.function) {
                return Err(format!("{}: a tx row no wrapper wraps", row.key));
            }
        }
        Ok(Inventory {
            provenance: fixture.provenance,
            rejects,
            wrappers,
            script_errors: fixture.script_errors,
            evo_keys,
            special_call,
            mnhf_step,
            stx_step,
        })
    }

    /// How far a body refused at `row` got into step 4: nowhere before ConnectBlock's
    /// ProcessSpecialTxsInBlock call or above that function's step 4, partway inside
    /// CheckSpecialTxInner, and through it everywhere below (ConnectBlock's transaction loop
    /// and after).
    pub fn reach(&self, row: &Row) -> Reach {
        match (EntryPath::of(&row.function), row.function.as_str()) {
            (
                Some(EntryPath::Check | EntryPath::Header | EntryPath::Contextual | EntryPath::Tbv),
                _,
            ) => Reach::None,
            (_, "CheckSpecialTxInner") => Reach::Partial,
            (_, "ProcessSpecialTxsInBlock") if row.line.is_some_and(|l| l < self.stx_step) => {
                Reach::None
            }
            (_, "ConnectBlock") if row.line.is_some_and(|l| l < self.special_call) => Reach::None,
            _ => Reach::Full,
        }
    }

    /// Whether a reject at `row` comes strictly before ProcessSpecialTxsInBlock's step 11, the
    /// MNHF ProcessBlock (plan §3.6's `g.mnhf`): every check before ConnectBlock, ConnectBlock's
    /// own rows above its ProcessSpecialTxsInBlock call, CheckSpecialTxInner (step 4), and
    /// ProcessSpecialTxsInBlock's rows above step 11. ConnectBlock's transaction loop and every
    /// row below it come after.
    pub fn before_mnhf_step(&self, row: &Row) -> bool {
        match (EntryPath::of(&row.function), row.function.as_str()) {
            (
                Some(EntryPath::Check | EntryPath::Header | EntryPath::Contextual | EntryPath::Tbv),
                _,
            ) => true,
            (_, "CheckSpecialTxInner") => true,
            (_, "ProcessSpecialTxsInBlock") => row.line.is_some_and(|l| l < self.mnhf_step),
            (_, "ConnectBlock") => row.line.is_some_and(|l| l < self.special_call),
            _ => false,
        }
    }

    /// The class a block reject at `row` carries (`BlockChecked`'s, in `REJECT_CLASSES`'
    /// spelling): its own for a block row, its wrapper's for a tx row.
    pub fn class(&self, row: &Row) -> String {
        let outer = if row.result.starts_with("T_") {
            self.wrappers.get(&row.function).map_or(row.result.as_str(), String::as_str)
        } else {
            row.result.as_str()
        };
        outer.trim_start_matches("B_").trim_start_matches("T_").to_ascii_lowercase()
    }

    /// Whether `reason` is an instance of `template`: `%08x` is 8 lowercase hex digits and `%s`
    /// one of ScriptErrorString's strings (plan §6.1).
    pub fn matches(&self, template: &str, reason: &str) -> bool {
        match_from(template, reason, &self.script_errors)
    }

    pub fn row(&self, key: &str) -> Option<&Row> {
        self.rejects.iter().find(|r| r.key == key)
    }

    /// Bind `reason` (a reject reason reply) for a trial in `mode`. `effects` is dashd's P2 view
    /// for a submit (`None` for a proposal, which changes nothing).
    pub fn bind(
        &self,
        reason: &str,
        mode: Mode,
        facts: &Facts,
        effects: Option<Effects>,
    ) -> Binding {
        let mode_name = match mode {
            Mode::Submit => "Submit",
            Mode::Proposal => "Proposal",
            Mode::Header => "Header",
        };
        // Each survivor with whether it rests on a guard no fact decided.
        let survivors: Vec<(&Row, bool)> = self
            .rejects
            .iter()
            .filter(|r| self.matches(&r.template, reason))
            .filter(|r| r.paths.iter().any(|p| p == mode_name))
            .filter_map(|r| match guards_allow(r, facts) {
                Some(false) => None,
                Some(true) => Some((r, false)),
                None => Some((r, true)),
            })
            .filter(|(r, _)| match (effects, EntryPath::of(&r.function)) {
                (Some(seen), Some(path)) => {
                    path.effects(r.result == "B_MUTATED").is_none_or(|want| want == seen)
                }
                _ => true,
            })
            .collect();
        match survivors.as_slice() {
            [] => Binding::Unbound,
            [(one, false)] => Binding::Bound(one.key.clone()),
            many => Binding::Ambiguous(many.iter().map(|(r, _)| r.key.clone()).collect()),
        }
    }
}

/// Whether the row's guards hold under `facts`: `None` when a measured fact is not available.
fn guards_allow(row: &Row, facts: &Facts) -> Option<bool> {
    let mut decided = true;
    for (name, want) in &row.guards {
        let holds = match name.as_str() {
            "network" => Some(want.as_str() == Some(facts.network.as_str())),
            "dip3" => Some(want.as_bool() == Some(facts.dip3)),
            "cbtx" => Some(want.as_bool() == Some(facts.cbtx)),
            "mnsync" => Some(want.as_bool() == Some(facts.mnsync_synced)),
            // No ChainLock and no checkpoint past genesis on L3's regtest legs.
            "chainlock" | "checkpoint" => Some(want.as_bool() == Some(false)),
            "par" => Some(want.as_u64() == Some(1) && facts.par == Par::One),
            "par_gt" => Some(want.as_u64() == Some(1) && facts.par != Par::One),
            "parent" => Some(match want.as_str() {
                Some("failed") => facts.parent == ParentState::Failed,
                Some("unmarked_failed_ancestor") => {
                    facts.parent == ParentState::UnmarkedFailedAncestor
                }
                _ => false,
            }),
            "measured" => want.as_str().and_then(|m| facts.measured.get(m).copied()),
            _ => Some(false),
        };
        match holds {
            Some(true) => {}
            Some(false) => return Some(false),
            None => decided = false,
        }
    }
    decided.then_some(true)
}

fn match_from(template: &str, reason: &str, script_errors: &[String]) -> bool {
    if let Some(i) = template.find('%') {
        let (lit, rest) = template.split_at(i);
        let Some(reason) = reason.strip_prefix(lit) else {
            return false;
        };
        if let Some(rest) = rest.strip_prefix("%08x") {
            return reason.len() >= 8
                && reason.as_bytes()[..8]
                    .iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
                && match_from(rest, &reason[8..], script_errors);
        }
        if let Some(rest) = rest.strip_prefix("%s") {
            return script_errors.iter().any(|e| {
                reason
                    .strip_prefix(e.as_str())
                    .is_some_and(|tail| match_from(rest, tail, script_errors))
            });
        }
        return false;
    }
    template == reason
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            network: "regtest".into(),
            dip3: false,
            cbtx: false,
            mnsync_synced: false,
            par: Par::One,
            parent: ParentState::Clean,
            measured: BTreeMap::new(),
        }
    }

    const CONTEXTUAL: Effects = Effects {
        known: true,
        failed: true,
        stored: false,
    };
    const CONNECT: Effects = Effects {
        known: true,
        failed: true,
        stored: true,
    };
    const NOTHING: Effects = Effects::NONE;

    /// The compiled fixture is the census's: 70 reject rows, every tx row wrapped.
    #[test]
    fn the_fixture_loads() {
        let inv = Inventory::load().expect("the fixture");
        assert_eq!(inv.rejects.len(), 70);
        assert_eq!(inv.provenance.tag, "v24.0.0-rc.4");
        assert_eq!(inv.provenance.commit, "0e7382df80d0bc6811068d076b232a5a26d34a3c");
        let row = inv.row("tx_check.cpp:CheckTransaction#5:bad-txns-vout-negative").expect("a row");
        assert_eq!(inv.class(row), "consensus");
        let row = inv.row("validation.cpp:CheckBlockHeader#1:high-hash").expect("a row");
        assert_eq!(inv.class(row), "invalid_header");
    }

    #[test]
    fn templates_match_their_instances_only() {
        let inv = Inventory::load().expect("the fixture");
        assert!(inv.matches("bad-version(0x%08x)", "bad-version(0x00000003)"));
        assert!(!inv.matches("bad-version(0x%08x)", "bad-version(0x0000003)"));
        assert!(!inv.matches("bad-version(0x%08x)", "bad-version(0x0000000G)"));
        let script = "mandatory-script-verify-flag-failed (%s)";
        assert!(inv.matches(
            script,
            "mandatory-script-verify-flag-failed (Script evaluated without error but finished with \
             a false/empty top stack element)"
        ));
        assert!(!inv.matches(script, "mandatory-script-verify-flag-failed (made up)"));
        assert!(inv.matches("bad-cb-amount", "bad-cb-amount"));
        assert!(!inv.matches("bad-cb-amount", "bad-cb-amount "));
    }

    /// The two nonfinal rows: a submit's effects tell them apart; a proposal needs the measured
    /// facts, and without them stays ambiguous.
    #[test]
    fn effects_and_measured_facts_bind_nonfinal() {
        let inv = Inventory::load().expect("the fixture");
        let ctx = "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal";
        let conn = "validation.cpp:ConnectBlock#5:bad-txns-nonfinal";
        let mut f = facts();
        f.measured.insert("nonfinal_locktime", true);
        f.measured.insert("bip68", false);
        assert_eq!(
            inv.bind("bad-txns-nonfinal", Mode::Submit, &f, Some(CONTEXTUAL)),
            Binding::Bound(ctx.into())
        );
        assert_eq!(
            inv.bind("bad-txns-nonfinal", Mode::Proposal, &f, None),
            Binding::Bound(ctx.into())
        );
        f.measured.insert("nonfinal_locktime", false);
        f.measured.insert("bip68", true);
        assert_eq!(
            inv.bind("bad-txns-nonfinal", Mode::Submit, &f, Some(CONNECT)),
            Binding::Bound(conn.into())
        );
        let unknown = facts();
        assert_eq!(
            inv.bind("bad-txns-nonfinal", Mode::Proposal, &unknown, None),
            Binding::Ambiguous(vec![ctx.into(), conn.into()])
        );
        assert_eq!(
            inv.bind("bad-txns-nonfinal", Mode::Submit, &f, Some(NOTHING)),
            Binding::Unbound,
            "effects no row gives are operator drift"
        );
    }

    /// Guards: the network removes :4490, `-par` picks the script row, ChainLock rows never
    /// survive.
    #[test]
    fn guards_follow_the_facts() {
        let inv = Inventory::load().expect("the fixture");
        let f = facts();
        assert_eq!(
            inv.bind("bad-diffbits", Mode::Proposal, &f, None),
            Binding::Bound("validation.cpp:ContextualCheckBlockHeader#2:bad-diffbits".into())
        );
        let reason = "mandatory-script-verify-flag-failed (Script failed an OP_VERIFY operation)";
        assert_eq!(
            inv.bind(reason, Mode::Submit, &f, Some(CONNECT)),
            Binding::Bound(
                "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)"
                    .into()
            )
        );
        let mut par4 = facts();
        par4.par = Par::Four;
        assert_eq!(inv.bind(reason, Mode::Submit, &par4, Some(CONNECT)), Binding::Unbound);
        assert_eq!(
            inv.bind("block-validation-failed", Mode::Submit, &par4, Some(CONNECT)),
            Binding::Bound("validation.cpp:ConnectBlock#7:block-validation-failed".into())
        );
        assert_eq!(inv.bind("bad-chainlock", Mode::Submit, &f, Some(CONNECT)), Binding::Unbound);
        assert_eq!(
            inv.bind("bad-cb-missing", Mode::Proposal, &f, None),
            Binding::Bound("validation.cpp:CheckBlock#2:bad-cb-missing".into())
        );
        assert_eq!(inv.bind("bad-cb-missing", Mode::Submit, &f, Some(NOTHING)), Binding::Unbound);
    }

    /// m-2: CheckSpecialTxInner's rows after its DIP3 gate (st:228) carry the gate, so below
    /// DIP3 a `bad-cbtx-payload` or `bad-cbtx-invalid` binds ProcessSpecialTxsInBlock's row.
    #[test]
    fn the_rows_past_the_dip3_gate_are_guarded() {
        let inv = Inventory::load().expect("the fixture");
        let past_gate: Vec<&Row> = inv
            .rejects
            .iter()
            .filter(|r| r.function == "CheckSpecialTxInner" && !r.key.contains("#1:"))
            .collect();
        assert_eq!(past_gate.len(), 4);
        assert!(past_gate.iter().all(|r| r.guards.get("dip3") == Some(&serde_json::json!(true))));
        let f = facts();
        assert_eq!(
            inv.bind("bad-cbtx-payload", Mode::Submit, &f, Some(CONNECT)),
            Binding::Bound("specialtxman.cpp:ProcessSpecialTxsInBlock#2:bad-cbtx-payload".into())
        );
        assert_eq!(
            inv.bind("bad-cbtx-invalid", Mode::Submit, &f, Some(CONNECT)),
            Binding::Bound("specialtxman.cpp:ProcessSpecialTxsInBlock#1:bad-cbtx-invalid".into())
        );
        let mut dip3 = facts();
        dip3.dip3 = true;
        assert!(matches!(
            inv.bind("bad-cbtx-payload", Mode::Submit, &dip3, Some(CONNECT)),
            Binding::Ambiguous(_)
        ));
    }

    /// M-3: where a refusal leaves step 4, by the fixture's lines (step 4 is st:933).
    #[test]
    fn the_reach_of_each_refusal() {
        let inv = Inventory::load().expect("the fixture");
        let reach = |key: &str| inv.reach(inv.row(key).unwrap_or_else(|| panic!("a row {key}")));
        assert_eq!(reach("validation.cpp:ContextualCheckBlock#4:bad-cb-height"), Reach::None);
        assert_eq!(reach("validation.cpp:ConnectBlock#2:bad-txns-BIP30"), Reach::None);
        assert_eq!(
            reach("specialtxman.cpp:ProcessSpecialTxsInBlock#2:bad-cbtx-payload"),
            Reach::None
        );
        assert_eq!(
            reach("specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive"),
            Reach::Partial
        );
        assert_eq!(reach("validation.cpp:ConnectBlock#5:bad-txns-nonfinal"), Reach::Full);
        assert_eq!(reach("validation.cpp:ConnectBlock#9:bad-cb-amount"), Reach::Full);
        assert_eq!(
            reach("validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)"),
            Reach::Full
        );
    }

    /// m-5: the rows before step 11, by the fixture's lines (ConnectBlock calls
    /// ProcessSpecialTxsInBlock at v:2672; step 11 is st:1033).
    #[test]
    fn the_rows_before_the_mnhf_step() {
        let inv = Inventory::load().expect("the fixture");
        let before =
            |key: &str| inv.before_mnhf_step(inv.row(key).unwrap_or_else(|| panic!("a row {key}")));
        for key in [
            "validation.cpp:CheckBlock#2:bad-cb-missing",
            "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal",
            "validation.cpp:TestBlockValidity#1:bad-chainlock",
            "validation.cpp:ConnectBlock#3:bad-sb-start",
            "specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive",
            "specialtxman.cpp:ProcessSpecialTxsInBlock#3:bad-cbtx-version",
            "specialtxman.cpp:ProcessSpecialTxsInBlock#5:bad-cbtx-quorummerkleroot",
        ] {
            assert!(before(key), "{key}");
        }
        for key in [
            "validation.cpp:ConnectBlock#4:bad-txns-accumulated-fee-outofrange",
            "validation.cpp:ConnectBlock#9:bad-cb-amount",
            "validation.cpp:CheckInputScripts#2:mandatory-script-verify-flag-failed (%s)",
            "specialtxman.cpp:ProcessSpecialTxsInBlock#6:failed-procspectxsinblock",
        ] {
            assert!(!before(key), "{key}");
        }
    }
}
