//! O10-2's pair set (O10 plan §4.5) and its counts: every ordered pair of the universe's parts
//! plus the explicit sets (§4.4's W1-W6 and W3b, each with its reversed placement; §7's mutant
//! pairs; the same-transaction classes; class 7's scenario pairs), dry-run on a synthetic chain
//! ([`super::pairs`]), classified into §4.5's classes 1-10, and selected: every explicit pair,
//! then up to [`PER_CLASS`] per class, with classes 2, 9 and 10 in both orders.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use bitcoin_hashes::sha256;
use serde_json::{Value, json};

use super::footprint;
use super::inventory::{Binding, Facts as BindFacts, Inventory};
use super::leg;
use super::measure;
use super::pairs::{
    self, Build, Dry, MODES, MeasureError, O10, Order, PARS, Pair, Part, Role, SameTx, bind_facts,
    binding, build, interned, measures, part_id, part_site, predict, setting_of, universe_parts,
};
use super::precedence::{self, At, Model, Unplaced};
use crate::chain::build::{self, Lock};
use crate::chain::mutate::{BO_BASE, BUDGET_START, DIP0001_HEIGHT, Mode, P1_BASE, Par, Site};

/// What the generator counted (the run report's and REPORT's carry-overs 1-3).
#[derive(Clone, Debug, Default)]
pub struct Counts {
    /// Ordered variant pairs and same-transaction witnesses considered.
    pub candidates: usize,
    /// Refused or excluded, by typed reason.
    pub refused: BTreeMap<String, usize>,
    /// Buildable pairs by class (0: no §4.5 class).
    pub buildable: BTreeMap<u8, usize>,
    /// The pairs the set runs, by class.
    pub selected: BTreeMap<u8, usize>,
    pub solos: usize,
    /// The model-level universe's (O10-1's probe): answers, infeasible by fact,
    /// unreachable-after-return, and item 8b's iteration-guard conflicts at one index.
    pub universe: BTreeMap<String, usize>,
}

impl Counts {
    /// The run report's `o10` counts.
    pub fn json(&self) -> Value {
        let by_class = |m: &BTreeMap<u8, usize>| -> BTreeMap<String, usize> {
            m.iter().map(|(c, n)| (c.to_string(), *n)).collect()
        };
        json!({
            "candidates": self.candidates,
            "refused": self.refused,
            "buildable": by_class(&self.buildable),
            "selected": by_class(&self.selected),
            "solos": self.solos,
            "universe": self.universe,
        })
    }
}

/// The M3 claim's O10-2 cases (census `o10_cases`): `o10.pair.class<n>` and
/// `o10.w.<W>` pass when every member case's credited trials passed P8 on every leg given (a
/// pair's cold and proposal trials, a solo's cold one, a scenario's last step), and their
/// `.solos` when every solo control of their pairs did.
pub fn o10_cases(passed: &[(Par, BTreeSet<String>)]) -> BTreeMap<String, &'static str> {
    let g = generated();
    // Per leg: its cases by id, and the trial ids P8 passed.
    type Leg<'a> = (BTreeMap<(&'static str, &'static str), leg::Case>, &'a BTreeSet<String>);
    let legs: Vec<Leg<'_>> = passed
        .iter()
        .map(|(par, ids)| {
            // A set whose cases cannot be laid out failed its leg, so none of them covers.
            let cases = leg::cases(leg::Set::O10Pairs, *par).unwrap_or_default();
            (cases.into_iter().map(|c| ((c.id.op, c.id.variant), c)).collect(), ids)
        })
        .collect();
    let ok = |o: &O10| -> bool {
        !legs.is_empty()
            && legs.iter().all(|(cases, ids)| {
                cases.get(&(o.op(), o.variant())).is_some_and(|c| {
                    let kinds = c.kinds(leg::Set::O10Pairs);
                    let credited = match c.steps() {
                        Some(_) => kinds.last().copied().into_iter().collect(),
                        None => kinds,
                    };
                    credited.into_iter().all(|kind| {
                        ids.contains(
                            &super::trial::TrialId {
                                case: c.id,
                                kind,
                            }
                            .to_string(),
                        )
                    })
                })
            })
    };
    let solo_case = |id: &str| g.cases.iter().find(|c| c.role != Role::Pair && c.variant() == id);
    let group = |pairs: Vec<&'static Pair>, members: Vec<&O10>| -> [&'static str; 2] {
        let solos: Vec<Option<&O10>> = pairs.iter().flat_map(|p| p.solos).map(&solo_case).collect();
        let pass = |b: bool| {
            if b {
                "pass"
            } else {
                "fail"
            }
        };
        [
            pass(!members.is_empty() && members.iter().copied().all(&ok)),
            pass(!solos.is_empty() && solos.iter().all(|o| o.is_some_and(&ok))),
        ]
    };
    let mut out = BTreeMap::new();
    for n in 1..=10u8 {
        let pairs: Vec<&'static Pair> = g.pairs.iter().copied().filter(|p| p.class == n).collect();
        let members =
            g.cases.iter().filter(|c| c.role == Role::Pair && c.pair.class == n).collect();
        let [pair, solos] = group(pairs, members);
        out.insert(format!("o10.pair.class{n}"), pair);
        out.insert(format!("o10.pair.class{n}.solos"), solos);
    }
    for w in WITNESSES {
        let of = |x: Option<&str>| x.is_some_and(|x| x == w || x == format!("{w}.reversed"));
        let pairs: Vec<&'static Pair> = g.pairs.iter().copied().filter(|p| of(p.witness)).collect();
        let members = g.cases.iter().filter(|c| of(c.witness())).collect();
        let [pair, solos] = group(pairs, members);
        out.insert(format!("o10.w.{w}"), pair);
        out.insert(format!("o10.w.{w}.solos"), solos);
    }
    out
}

/// §4.4's witnesses, as the census names them.
pub const WITNESSES: [&str; 7] = ["W1", "W2", "W3", "W3b", "W4", "W5", "W6"];

/// The set's pairs and cases, built once.
pub struct Generated {
    pub pairs: Vec<&'static Pair>,
    pub cases: Vec<O10>,
    pub counts: Counts,
    /// Each selected pair's measured key, by id.
    pub keys: BTreeMap<&'static str, Key>,
    /// The selected class 9 and 10 obligations.
    obligations: Vec<Obligation>,
}

/// A function's loop: its name, the loop body's rows, the rows before it and after it.
pub type Loop =
    (&'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str]);

/// The rows of a loop, and the block-level rows before and after it, per function (§4.5's
/// classes 2, 9, 10; checked against the model by `the_loop_table_is_the_models`).
pub const LOOPS: &[Loop] = &[
    (
        // v:4416-4425, after v:4395 and v:4405; the legacy sigops after (v:4426-4433).
        "CheckBlock",
        &measure::CHECK_TRANSACTION,
        &[measure::CMR_ROOT, measure::CMR_DUP, measure::CB_LENGTH],
        &[measure::CB_SIGOPS],
    ),
    (
        // v:4569-4582, after v:4563-4564; the sigops and the coinbase height after (v:4585,
        // v:4591).
        "ContextualCheckBlock",
        &[measure::CCB_FINAL, measure::CCT_OVERSIZE],
        &[measure::CCB_LENGTH],
        &[measure::CCB_SIGOPS, measure::CCB_HEIGHT],
    ),
    (
        // v:2699-2780, after BIP30 (v:2615) and ProcessSpecialTxsInBlock (v:2674); the
        // coinbase amount after (v:2857).
        "ConnectBlock",
        &[
            measure::CTI_MISSING,
            measure::CTI_PREMATURE,
            measure::CTI_VALUES,
            measure::CTI_BELOW,
            measure::CONN_FEES,
            measure::CONN_BIP68,
            measure::CONN_SIGOPS,
            measure::CIS_SCRIPT,
        ],
        &[measure::CONN_BIP30, measure::STX_DIP3, measure::STX_CB_PAYLOAD],
        &[measure::CONN_AMOUNT],
    ),
];

const HEADER_ROWS: [&str; 5] = [
    measure::CBH_POW,
    measure::CCBH_BITS,
    measure::CCBH_OLD,
    measure::CCBH_NEW,
    measure::CCBH_VERSION,
];

/// A composed pair's §4.5 class from its rows and the model's answers: mode-divergent (5),
/// `-par`-divergent (6), header-only (8), pre-loop × loop body (9), loop body × post-loop (10),
/// same loop at different transactions (2); 0 for none. With the loop body's rows, for the
/// classes a loop decides.
fn class(
    rows: [&str; 2],
    answers: &BTreeMap<(Mode, Par), Option<(String, bool)>>,
) -> (u8, &'static [&'static str]) {
    let by_mode = |m: Mode| PARS.map(|p| answers[&(m, p)].clone());
    let by_par = |p: Par| MODES.map(|m| answers[&(m, p)].clone());
    if by_mode(Mode::Submit) != by_mode(Mode::Proposal) {
        return (5, &[]);
    }
    if by_par(Par::One) != by_par(Par::Four) {
        return (6, &[]);
    }
    if rows.iter().all(|r| HEADER_ROWS.contains(r)) {
        return (8, &[]);
    }
    for (_, body, pre, post) in LOOPS {
        let has = |set: &[&str], r: &str| set.contains(&r);
        if rows.iter().any(|r| has(pre, r)) && rows.iter().any(|r| has(body, r)) {
            return (9, body);
        }
        if rows.iter().any(|r| has(body, r)) && rows.iter().any(|r| has(post, r)) {
            return (10, body);
        }
        if rows.iter().all(|r| has(body, r)) {
            return (2, body);
        }
    }
    (0, &[])
}

/// The classes whose pairs run in both orders (§4.5): each candidate is one obligation, its two
/// placements deduplicated, budgeted and refused together, never one alone.
const BOTH_ORDERS: [u8; 3] = [2, 9, 10];

/// A §4.4 placement's transaction: an index, the block's last, or none (a block-level row).
#[derive(Clone, Copy)]
enum Tx {
    At(usize),
    Last,
    Block,
}

/// §4.4's placements, each witness's defects at their transaction and input (Sol r1 S-F2), as
/// the pair measures them; W4 is W4.reversed's script solo (tx1's script fault, tx2 clean).
/// A witness's §4.4 placements: each row at its transaction and input.
type Placements = &'static [(&'static str, Tx, Option<usize>)];

const PLACED: &[(&str, Placements)] = {
    use Tx::{At as T, Block, Last};
    let vin_empty = measure::CHECK_TRANSACTION[0];
    &[
        ("W1", &[(measure::CONN_BIP68, T(1), None), (measure::CTI_BELOW, T(2), None)]),
        ("W1.reversed", &[(measure::CTI_BELOW, T(1), None), (measure::CONN_BIP68, T(2), None)]),
        ("W2", &[(measure::CCT_OVERSIZE, T(1), None), (measure::CCB_FINAL, T(2), None)]),
        ("W2.reversed", &[(measure::CCB_FINAL, T(1), None), (measure::CCT_OVERSIZE, T(2), None)]),
        ("W3", &[(measure::CTI_VALUES, T(1), Some(1)), (measure::CTI_PREMATURE, T(1), Some(2))]),
        ("W3.reversed", &[(measure::CTI_PREMATURE, T(1), Some(0))]),
        ("W3b", &[(measure::CTI_PREMATURE, T(1), Some(1))]),
        ("W4", &[(measure::CIS_SCRIPT, T(1), Some(0))]),
        ("W4.reversed", &[(measure::CIS_SCRIPT, T(1), Some(0)), (measure::CONN_BIP68, T(2), None)]),
        ("W5", &[(vin_empty, T(1), None), (measure::CB_LENGTH, Block, None)]),
        ("W5.reversed", &[(vin_empty, Last, None), (measure::CB_LENGTH, Block, None)]),
        ("W6", &[(vin_empty, Last, None), (measure::CB_SIGOPS, Block, None)]),
        ("W6.reversed", &[(vin_empty, T(1), None), (measure::CB_SIGOPS, Block, None)]),
    ]
};

/// Whether `key`, a block of `vtx` transactions, holds `witness`'s §4.4 placements, and W4 its
/// block's shape (tx1's fault and one clean tx2), W6 its at least 12 transactions.
fn placed(witness: &str, key: &Key, vtx: usize) -> Result<(), String> {
    let (_, want) = PLACED
        .iter()
        .find(|(w, _)| *w == witness)
        .ok_or_else(|| format!("{witness}: no §4.4 placement"))?;
    for (row, tx, vin) in *want {
        let at = At {
            vtx: match tx {
                Tx::At(i) => Some(*i),
                Tx::Last => Some(vtx - 1),
                Tx::Block => None,
            },
            vin: *vin,
            vout: None,
        };
        if !key.contains(&((*row).to_owned(), at)) {
            return Err(format!("{witness}: {row} is not at {at:?} in {key:?}"));
        }
    }
    let shaped = match witness {
        "W4" => key.len() == 1 && vtx == 3,
        w if w.starts_with("W6") => vtx >= 12,
        _ => true,
    };
    if !shaped {
        return Err(format!("{witness}: {vtx} transactions holding {key:?}"));
    }
    Ok(())
}

/// Whether a class 9 or 10 obligation's placements hold its loop-body defect at index 1 and at
/// the block's last index (§4.5).
fn covers(placements: &[(usize, &[usize])]) -> bool {
    let at: BTreeSet<usize> = placements.iter().flat_map(|(_, b)| b.iter().copied()).collect();
    !placements.is_empty()
        && placements.iter().all(|(vtx, _)| at.contains(&1) && at.contains(&(vtx - 1)))
}

/// A pair's obligation: a composed pair's two orders share one, a same-transaction or scenario
/// pair is its own.
fn obligation_of(q: &Pair) -> String {
    match q.build {
        Build::Compose {
            a,
            b,
            fill,
            ..
        } => format!("{}~{}.fill{fill}", part_id(a), part_id(b)),
        _ => q.id.to_owned(),
    }
}

/// §4.5 class 2 runs both orders (Opus r1 m2): every selected composed class 2 pair has its
/// reversal in the set; the first without fails, named.
fn reversed_class_2(pairs: &[&Pair]) -> Result<(), String> {
    for p in pairs.iter().filter(|p| p.class == 2 && matches!(p.build, Build::Compose { .. })) {
        let orders = pairs.iter().filter(|q| obligation_of(q) == obligation_of(p)).count();
        if orders < 2 {
            return Err(format!("{}: a class 2 pair without its reversal", p.id));
        }
    }
    Ok(())
}

/// A selected class 9 or 10 obligation: its pairs' placements in the set (§4.5).
#[derive(Clone)]
struct Obligation {
    class: u8,
    /// Its first placement's id, naming it.
    id: &'static str,
    site: Site,
    /// The row before or after the loop that classed it.
    edge: Option<&'static str>,
    /// Each placement's transaction count and loop-body transactions.
    placements: Vec<(usize, Vec<usize>)>,
}

impl Obligation {
    fn covered(&self) -> bool {
        covers(&self.placements.iter().map(|(vtx, body)| (*vtx, &body[..])).collect::<Vec<_>>())
    }
}

/// The rows §4.5 names before or after a loop (OD-L3-30), with their class and pin lines: each
/// needs a selected obligation of that class holding its loop-body defect at index 1 and at
/// the last index.
const NAMED: [(u8, &str, &str); 3] = [
    // W5's: the size check before CheckBlock's CheckTransaction loop.
    (9, measure::CB_LENGTH, "v:4405"),
    // W5's ContextualCheckBlock analogue: the size check before the v:4569 loop.
    (9, measure::CCB_LENGTH, "v:4563-4564"),
    // The sigops after the v:4569 loop.
    (10, measure::CCB_SIGOPS, "v:4585"),
];

/// The [`NAMED`] rows whose site has no room for a block with index 1 short of the last
/// (OD-L3-31): ContextualCheckBlock's sigops at P1, whose ten heights hold its 2-transaction
/// block beside v:4563's two orders. Every other named row needs a longer block.
const SHORT_BLOCK_ONLY: [(&str, Site); 1] = [(measure::CCB_SIGOPS, Site::P1)];

/// Whether every [`NAMED`] row has its obligation; the first without fails, named.
fn named_rows_held(obligations: &[Obligation]) -> Result<(), String> {
    for (class, row, cite) in NAMED {
        let held = |o: &&Obligation| {
            o.class == class
                && o.edge == Some(row)
                && o.covered()
                && (o.placements.iter().any(|(vtx, _)| *vtx > 2)
                    || SHORT_BLOCK_ONLY.contains(&(row, o.site)))
        };
        if !obligations.iter().any(|o| held(&o)) {
            return Err(format!(
                "§4.5 class {class}'s {row} ({cite}): no selected obligation with its loop-body \
                 defect at index 1 and at the last index"
            ));
        }
    }
    Ok(())
}

/// An explicit pair: `(witness, a, b, order, fill, mutants)`, the parts by variant id.
type Explicit =
    (Option<&'static str>, &'static str, &'static str, Order, usize, &'static [&'static str]);

/// The explicit sets (§4.4, §7's mutants).
const EXPLICIT: &[Explicit] = &[
    (Some("W1"), "bip68_height.reg", "in_below_out.reg", Order::Ab, 0, &[]),
    (Some("W1.reversed"), "bip68_height.reg", "in_below_out.reg", Order::Ba, 0, &[]),
    (Some("W2"), "tx_size_1m.999999", "locktime_height.reg", Order::Ab, 0, &[]),
    (Some("W2.reversed"), "tx_size_1m.999999", "locktime_height.reg", Order::Ba, 0, &[]),
    // W4 itself (tx1's script fault, tx2 clean) is this pair's script solo (`O10::witness`).
    (Some("W4.reversed"), "script_false.reg", "bip68_height.reg", Order::Ab, 0, &["M8"]),
    (Some("W5"), "block_size.2000001", "vin_empty.reg", Order::Ba, 0, &["M9"]),
    (Some("W5.reversed"), "block_size.2000001", "vin_empty.reg", Order::Ab, 0, &["M9"]),
    (Some("W6"), "sigops_check.40001", "vin_empty.reg", Order::Ab, 10, &["M10"]),
    (Some("W6.reversed"), "sigops_check.40001", "vin_empty.reg", Order::Ba, 10, &["M10"]),
    (None, "merkle_root.reg", "bits_minus.reg", Order::Ab, 0, &["M1"]),
    (None, "cb_height.reg", "locktime_height.reg", Order::Ab, 0, &["M2"]),
    (None, "missing_input.reg", "special_tx_type.1", Order::Ab, 0, &["M3"]),
    // Class 9: its loop-body defect at the last index too (§4.5).
    (None, "missing_input.reg", "special_tx_type.1", Order::Ba, 0, &["M3"]),
    (None, "merkle_root.reg", "block_size.2000001", Order::Ab, 0, &["M5"]),
    // OD-L3-30: ContextualCheckBlock's size row (v:4563-4564) against its loop (v:4569), the
    // loop-body defect at index 1 and at the last index.
    (None, "locktime_height.reg", "block_size.1000001", Order::Ab, 0, &[]),
    (None, "locktime_height.reg", "block_size.1000001", Order::Ba, 0, &[]),
];

/// Same-transaction witnesses with their §4.4 names and mutants.
const SAME_TX: &[(SameTx, Option<&str>, &[&str])] = &[
    (SameTx::W3, Some("W3"), &[]),
    (SameTx::W3Reversed, Some("W3.reversed"), &[]),
    (SameTx::W3b, Some("W3b"), &["M7"]),
    (SameTx::BelowOutBip68, None, &["M4"]),
    (SameTx::ImmatureBip68, None, &[]),
    (SameTx::NonfinalBelowOut, None, &[]),
    (SameTx::NegativeNonfinal, None, &[]),
    (SameTx::NegativeBelowOut, None, &[]),
    // OD-L3-30/31: ContextualCheckBlock's sigops (v:4585) after its loop, at P1.
    (SameTx::NonfinalSigops, None, &[]),
];

/// Class 7's scenario pairs: a10-5's S2 and S6 children, each also off its parent's bits.
const SCENARIOS: &[(&str, &str, &[&str])] = &[
    ("s2_child_of_invalid.reg", "bits_minus.reg", &[]),
    ("s6_child_of_unmarked_descendant.submit", "bits_minus.reg", &["M6"]),
    ("s6_child_of_unmarked_descendant.header", "bits_minus.reg", &["M6"]),
];

/// The most pairs a class runs beyond its explicit ones (§4.5: one representative each plus
/// the explicit sets; §8.1's budget), spread over its rows in id order.
pub const PER_CLASS: usize = 16;

fn find_part(id: &str) -> Part {
    footprint::variants()
        .find(|(o, v)| footprint::variant_id(o, v) == id)
        .unwrap_or_else(|| panic!("no catalogue variant {id}"))
}

/// A §4.3 rule as its count label: the rule and what it shares.
fn rule_name(rule: &precedence::Rule) -> String {
    match rule {
        precedence::Rule::Footprint {
            field,
            ..
        } => format!("footprint {field}"),
        precedence::Rule::Guard {
            attr,
            ..
        } => format!("guard {attr}"),
        precedence::Rule::Removed {
            ..
        } => "removed".to_owned(),
    }
}

/// One candidate's dry run: its class and the solos' dedupe keys, or why it is refused.
struct Dried {
    class: u8,
    solos: [String; 2],
    /// The pair's measured instances, sorted: its group M key (class 6's dedupe).
    key: Key,
    /// Each solo's measured instances, sorted.
    solo_keys: [Key; 2],
    /// The block's transactions.
    vtx: usize,
    /// The transactions holding the class's loop-body instances (classes 2, 9, 10).
    body: Vec<usize>,
    /// Classes 9 and 10: the row before or after the loop that classed it (§4.5).
    edge: Option<&'static str>,
    /// The pair's and each solo's control identity (R2-F1).
    controls: Controls,
}

/// A block's site, sorted defects and each transaction's (inputs, outputs).
type Control = (Site, Key, Vec<(usize, usize)>);
/// A pair's and its two solos' [`Control`]s.
type Controls = [Control; 3];

/// R2-F1: a solo credited to `pair` runs at its site and keeps its domains
/// ([`pairs::keeps_domains`]), whichever pair built it; else the set fails, naming both.
fn credit(pair: &str, solo: &str, pair_ctl: &Control, solo_ctl: &Control) -> Result<(), String> {
    if solo_ctl.0 != pair_ctl.0 {
        return Err(format!(
            "{pair}: its control {solo} runs at {:?}, not {:?}",
            solo_ctl.0, pair_ctl.0
        ));
    }
    pairs::keeps_domains((&pair_ctl.1, &pair_ctl.2), (&solo_ctl.1, &solo_ctl.2))
        .map_err(|e| format!("{pair}: its control {solo} {e}"))
}

/// A block's measured instances, sorted: a10 group M's and a pair's dedupe key (§4.5 class 6:
/// rows, placement; the `-par` both run at).
pub type Key = Vec<(String, At)>;

/// a10 group M's keys (its `m_` operators: two defects in one block, plan rev1 §4), each
/// variant measured alone on its dry chain, with its id; a variant measure cannot read is
/// counted, never keyed.
pub fn group_m() -> (BTreeMap<Key, String>, usize) {
    let mut keys = BTreeMap::new();
    let mut unread = 0;
    for (op, v) in footprint::variants().filter(|(o, _)| o.name.starts_with("m_")) {
        let Some(site) = part_site((op, v)) else {
            unread += 1;
            continue;
        };
        let dry = Dry::new(site, &[Lock::P2shTrue; 9], pairs::part_mints((op, v), site));
        let world = dry.world(*b"o10-grpm");
        let mut plan = world.part((op, v), site);
        pairs::tag_coinbase(&mut plan, &world.trial, false);
        match measure::measure(&plan, &plan.build(), &dry.place()) {
            Ok(m) => {
                let mut key = m.defects;
                key.sort();
                keys.insert(key, footprint::variant_id(op, v));
            }
            Err(_) => unread += 1,
        }
    }
    (keys, unread)
}

/// Solo `k`'s control identity, its id's suffix (M-1, R2-F1): its variant's placement and the
/// pair's whole measured setting (facts, iteration facts, loop lengths), every transaction's
/// shape (`facts` keep only the longest domains) and the site, whose rules the control runs
/// under, as `.at<first tx>of<txs>.<digest>`. The dry run names the solo by it; the live trial
/// recomputes it from its own measures (Opus r1 n3).
pub fn solo_identity(site: Site, m: &pairs::Measures, k: usize) -> String {
    let s = &m.solos[k];
    let first = s.defects.iter().filter_map(|(_, a)| a.vtx).min().unwrap_or(0);
    let key = format!("{:?}|{:?}|{:?}|{:?}", site, s.defects, m.pair.facts, s.shapes);
    let digest = sha256::Hash::hash(key.as_bytes()).to_byte_array();
    format!(
        ".at{first}of{}.{}",
        m.pair.facts.lengths.get("block.vtx").copied().unwrap_or(0),
        super::trial::to_hex(&digest[..4])
    )
}

/// A candidate's dry run that yields no pair: a refusal, counted by its typed reason, or a
/// harness error, which fails the set naming the pair (Opus r1 m1).
#[derive(Debug, PartialEq, Eq)]
enum DryError {
    Refused(String),
    Harness(String),
}

impl From<String> for DryError {
    fn from(e: String) -> Self {
        DryError::Refused(e)
    }
}

/// `measures`' error for `pair`: an unsound composition is refused; a block measure cannot read
/// fails the set.
fn measured(pair: &str, e: MeasureError) -> DryError {
    match e {
        MeasureError::Composition(_) => DryError::Refused(format!("broken: {e}")),
        MeasureError::Measurement(_) => DryError::Harness(format!("{pair}: {e}")),
    }
}

/// The walker's `want` against the a10 binder's binding of its reason: equal, or none for an
/// accepted block, holds; ambiguous refuses the pair (`binding-ambiguous`); unbound or another
/// row fails the set, naming the pair and both rows.
fn bound(
    pair: &str,
    role: Role,
    mode: Mode,
    par: Par,
    want: &Option<(String, bool)>,
    got: Option<Binding>,
) -> Result<(), DryError> {
    match (want, got) {
        (_, None) => Ok(()),
        (Some((row, _)), Some(Binding::Bound(b))) if *row == b => Ok(()),
        (_, Some(Binding::Ambiguous(_))) => Err(DryError::Refused("binding-ambiguous".into())),
        (want, Some(got)) => Err(DryError::Harness(format!(
            "{pair}: {role:?} on {mode:?} at {par:?}: the walker's {:?}, the binder's {got:?}",
            want.as_ref().map(|(row, _)| row)
        ))),
    }
}

fn dry_run(model: &Model, inventory: &Inventory, pair: &Pair) -> Result<Dried, DryError> {
    let dry = Dry::new(pair.site, &pair.locks(), pair.mints());
    let world = dry.world(*b"o10-dry\0");
    let place = dry.place();
    let blocks = build(pair, &world)?;
    let m = measures(&blocks, &place).map_err(|e| measured(pair.id, e))?;
    let roles = [Role::Pair, Role::Solo(0), Role::Solo(1)];
    let facts = roles.map(|role| bind_facts(blocks.of(role), &place, Par::One));
    let mut answers = BTreeMap::new();
    let mut solo_fired: [Option<String>; 2] = [None, None];
    for mode in MODES {
        for par in PARS {
            for (role, facts) in roles.iter().copied().zip(&facts) {
                let facts = BindFacts {
                    par,
                    ..facts.clone()
                };
                let (fired, same) = predict(model, mode, par, &m, role).map_err(|e| e.reason())?;
                if !same {
                    return Err(DryError::Refused("key-disagrees".into()));
                }
                let got = binding(inventory, &fired, blocks.of(role), &facts, mode);
                bound(pair.id, role, mode, par, &fired, got)?;
                match role {
                    Role::Pair => drop(answers.insert((mode, par), fired)),
                    Role::Solo(k) if (mode, par) == (Mode::Submit, Par::One) => {
                        solo_fired[k] = fired.map(|(row, _)| row);
                    }
                    Role::Solo(_) => {}
                }
            }
            // §4.3's declared rules, over the instances with their parts' footprints: only these
            // make a pair interacting.
            let (mut instances, _) =
                setting_of(model, mode, par, &m.pair, &m.pair.facts).map_err(|e| e.reason())?;
            pairs::with_footprints(&mut instances, pair, &m)?;
            if let Some(rule) = precedence::interaction(model.root(mode), &instances) {
                return Err(DryError::Refused(format!("interacting:{}", rule_name(&rule))));
            }
        }
    }
    let rows: Vec<&str> = m.pair.rows().into_iter().collect();
    // A pair measuring more rows than its two parts' is classed by the row each solo fires
    // (a part's own row: sigops_ctx fires ContextualCheckBlock's sigops, v:4585, before the
    // ConnectBlock sigops it also measures), never by the first in name order (OD-L3-30).
    let by = match rows[..] {
        [x, y] => [x, y],
        _ => [0, 1].map(|k| {
            solo_fired[k].as_deref().or_else(|| m.solos[k].rows().into_iter().next()).unwrap_or("")
        }),
    };
    let (class, body) = match pair.build {
        // A same-transaction class 9 or 10 (OD-L3-31) takes its loop from its rows, which must
        // class it as declared.
        Build::SameTx(s) if matches!(s.class(), 9 | 10) => match class(by, &answers) {
            (c, body) if c == s.class() => (c, body),
            (c, _) => {
                return Err(DryError::Harness(format!(
                    "{}: class {c}, declared {}",
                    pair.id,
                    s.class()
                )));
            }
        },
        Build::SameTx(s) => (s.class(), &[][..]),
        _ => class(by, &answers),
    };
    // The loop's own row before or after it that met the body (W5's v:4405, its CCB analogue
    // v:4563): never a row the block merely also measures.
    let edge = LOOPS
        .iter()
        .filter(|(_, b, ..)| matches!(class, 9 | 10) && *b == body)
        .flat_map(|(_, _, pre, post)| {
            if class == 9 {
                *pre
            } else {
                *post
            }
        })
        .find(|r| by.contains(r))
        .copied();
    let shape = |k: usize| solo_identity(pair.site, &m, k);
    let sorted = |m: &measure::Measured| {
        let mut key = m.defects.clone();
        key.sort();
        key
    };
    Ok(Dried {
        class,
        solos: [shape(0), shape(1)],
        key: sorted(&m.pair),
        solo_keys: [sorted(&m.solos[0]), sorted(&m.solos[1])],
        vtx: m.pair.shapes.len(),
        body: m
            .pair
            .defects
            .iter()
            .filter(|(r, _)| body.contains(&r.as_str()))
            .filter_map(|(_, a)| a.vtx)
            .collect(),
        edge,
        controls: [&m.pair, &m.solos[0], &m.solos[1]]
            .map(|m| (pair.site, sorted(m), m.shapes.clone())),
    })
}

/// The O10-2 set ([`super::leg::Set::O10Pairs`]): every candidate dry-run once, the explicit
/// sets, and up to [`PER_CLASS`] more per class.
pub fn generated() -> &'static Generated {
    static ONCE: OnceLock<Generated> = OnceLock::new();
    ONCE.get_or_init(|| generate().expect("the O10-2 pair set"))
}

/// `f` over `items` on up to eight threads, in order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
    let chunk = items.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> =
            items.chunks(chunk).map(|c| s.spawn(|| c.iter().map(&f).collect::<Vec<R>>())).collect();
        handles.into_iter().flat_map(|h| h.join().expect("a dry-run thread")).collect()
    })
}

fn refuse(counts: &mut Counts, reason: &str, n: usize) {
    *counts.refused.entry(reason.to_owned()).or_default() += n;
}

fn leak<T>(t: T) -> &'static T {
    Box::leak(Box::new(t))
}

fn generate() -> Result<Generated, String> {
    let model = Model::load()?;
    let inventory = Inventory::load()?;
    let mut counts = Counts::default();
    let parts = universe_parts();
    // Candidates: explicit sets first, then every ordered pair of two universe rows' parts.
    let mut candidates: Vec<(Pair, bool)> = Vec::new();
    let pair_id = |a: Part, b: Part, order: Order, fill: usize| {
        let mut id = format!(
            "{}~{}.{}",
            part_id(a),
            part_id(b),
            match order {
                Order::Ab => "ab",
                Order::Ba => "ba",
            }
        );
        if fill > 0 {
            id.push_str(&format!(".fill{fill}"));
        }
        id
    };
    let site_of = |a: Part, b: Part| -> Option<Site> {
        let (sa, sb) = (part_site(a)?, part_site(b)?);
        match (sa, sb) {
            (x, y) if x == y => Some(x),
            (Site::Reg, y) => Some(y),
            (x, Site::Reg) => Some(x),
            _ => None,
        }
    };
    for (witness, a, b, order, fill, mutants) in EXPLICIT {
        let (a, b) = (find_part(a), find_part(b));
        let site = site_of(a, b).ok_or("an explicit pair on two sites")?;
        candidates.push((
            Pair {
                id: interned(&pair_id(a, b, *order, *fill)),
                build: Build::Compose {
                    a,
                    b,
                    order: *order,
                    fill: *fill,
                },
                site,
                class: 0,
                witness: *witness,
                mutants,
                solos: ["", ""],
            },
            true,
        ));
    }
    for (s, witness, mutants) in SAME_TX {
        candidates.push((
            Pair {
                id: interned(&format!("same.{}", s.name())),
                build: Build::SameTx(*s),
                site: s.site(),
                class: s.class(),
                witness: *witness,
                mutants,
                solos: ["", ""],
            },
            true,
        ));
    }
    for (ra, a, _) in &parts {
        for (rb, b, _) in &parts {
            if ra == rb {
                continue;
            }
            let Some(site) = site_of(*a, *b) else {
                *counts.refused.entry("excluded:two-sites".into()).or_default() += 1;
                counts.candidates += 1;
                continue;
            };
            let id = pair_id(*a, *b, Order::Ab, 0);
            if candidates.iter().any(|(p, _)| p.id == id) {
                continue;
            }
            candidates.push((
                Pair {
                    id: interned(&id),
                    build: Build::Compose {
                        a: *a,
                        b: *b,
                        order: Order::Ab,
                        fill: 0,
                    },
                    site,
                    class: 0,
                    witness: None,
                    mutants: &[],
                    solos: ["", ""],
                },
                false,
            ));
        }
    }
    // The dry runs, in parallel with the model-level universe.
    let mut to_dry: Vec<(Pair, bool)> = Vec::new();
    for (pair, explicit) in candidates {
        counts.candidates += 1;
        to_dry.push((pair, explicit));
    }
    let at = |vtx| At {
        vtx: Some(vtx),
        vin: None,
        vout: None,
    };
    let (universe, same_index, dried) = std::thread::scope(|s| {
        let universe = s.spawn(|| precedence::universe(&model, at(2)));
        let same_index = s.spawn(|| precedence::universe(&model, at(1)));
        let dried = par_map(&to_dry, |(pair, _)| dry_run(&model, &inventory, pair));
        (universe.join().expect("the universe"), same_index.join().expect("the universe"), dried)
    });
    for (.., answer) in universe {
        let key = match answer {
            Ok(_) => "answers".to_owned(),
            Err(e) => e.reason(),
        };
        *counts.universe.entry(key).or_default() += 1;
    }
    for (.., answer) in same_index {
        if let Err(
            e @ Unplaced::IterConflict {
                ..
            },
        ) = answer
        {
            *counts.universe.entry(format!("same-index {}", e.reason())).or_default() += 1;
        }
    }
    // §4.5 class 6's dedupe, for every class: a pair a10 group M already runs (its rows at its
    // placement, both -par) is that case, not a new one.
    let (group_m, unread) = group_m();
    *counts.universe.entry("group-m keys".into()).or_default() += group_m.len();
    *counts.universe.entry("group-m unread".into()).or_default() += unread;
    let mut built: Vec<(Pair, bool, Dried)> = Vec::new();
    for ((mut pair, explicit), dried) in to_dry.into_iter().zip(dried) {
        match dried {
            Ok(d) => {
                if pair.class == 0 {
                    pair.class = d.class;
                }
                // §4.4's witnesses at their placements (W4: W4.reversed's script solo).
                if let Some(w) = pair.witness {
                    placed(w, &d.key, d.vtx)?;
                    if w == "W4.reversed" {
                        placed("W4", &d.solo_keys[0], d.vtx)?;
                    }
                }
                // A class run in both orders is deduplicated whole, at its selection.
                if !explicit && !BOTH_ORDERS.contains(&pair.class) && group_m.contains_key(&d.key) {
                    *counts.refused.entry("excluded:group-m-duplicate".into()).or_default() += 1;
                    continue;
                }
                *counts.buildable.entry(pair.class).or_default() += 1;
                built.push((pair, explicit, d));
            }
            Err(DryError::Harness(e)) => return Err(e),
            Err(DryError::Refused(e)) if explicit => {
                return Err(format!("{}: explicit, but {e}", pair.id));
            }
            Err(DryError::Refused(e)) => {
                // A part pair that sets one header field or coinbase both cannot be built
                // (`merge`): refused at build time, never labelled interacting (§4.3's rules
                // label only what is built).
                let key = if e.starts_with("broken") {
                    "refused:broken-composition".to_owned()
                } else if let Some(field) = e.strip_prefix("both parts set the ") {
                    format!("refused:unmergeable {field}")
                } else {
                    e
                };
                *counts.refused.entry(key).or_default() += 1;
            }
        }
    }
    // Selection: every explicit pair; then per class, up to PER_CLASS spread over the rest. A
    // unit is one obligation's placements, admitted or refused together.
    let mut keys: BTreeMap<&'static str, Key> = BTreeMap::new();
    let mut bodies: BTreeMap<&'static str, (usize, Vec<usize>)> = BTreeMap::new();
    let mut edges: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    type Member = (Pair, [String; 2], Option<Controls>);
    let mut units: Vec<Vec<Member>> = Vec::new();
    let mut unit_of = |members: Vec<(Pair, Dried)>| -> Vec<Member> {
        members
            .into_iter()
            .map(|(pair, d)| {
                keys.insert(pair.id, d.key);
                bodies.insert(pair.id, (d.vtx, d.body));
                if let Some(edge) = d.edge {
                    edges.insert(pair.id, edge);
                }
                (pair, d.solos, Some(d.controls))
            })
            .collect()
    };
    let mut rest: BTreeMap<u8, Vec<(Pair, Dried)>> = BTreeMap::new();
    for (pair, explicit, d) in built {
        if explicit {
            units.push(unit_of(vec![(pair, d)]));
        } else if pair.class != 0 {
            rest.entry(pair.class).or_default().push((pair, d));
        }
    }
    for (_, mut list) in rest {
        list.sort_by(|x, y| x.0.id.cmp(y.0.id));
        let n = list.len();
        let step = n.div_ceil(PER_CLASS).max(1);
        for (i, (pair, d)) in list.into_iter().enumerate() {
            if i % step != 0 {
                continue;
            }
            let (class, dup) = (pair.class, group_m.contains_key(&d.key));
            // Classes 2, 9 and 10 run both orders: the loop body's row at index 1 and at the
            // block's last index (§4.5).
            let members = match (class, pair.build) {
                (
                    2 | 9 | 10,
                    Build::Compose {
                        a,
                        b,
                        order: Order::Ab,
                        fill,
                    },
                ) => {
                    let reversed = Pair {
                        id: interned(&pair_id(a, b, Order::Ba, fill)),
                        build: Build::Compose {
                            a,
                            b,
                            order: Order::Ba,
                            fill,
                        },
                        ..pair
                    };
                    counts.candidates += 1;
                    match dry_run(&model, &inventory, &reversed) {
                        Err(DryError::Harness(e)) => return Err(e),
                        // Both a10 group M's: the obligation is that case, not a new one.
                        Ok(r) if dup && group_m.contains_key(&r.key) => {
                            refuse(&mut counts, "excluded:group-m-duplicate", 2);
                            continue;
                        }
                        Ok(r) => vec![(pair, d), (reversed, r)],
                        // A class 9 or 10 forward placement alone holding index 1 and the last
                        // (a two-transaction block) stands without its reversal; class 2, both
                        // defects the loop's, always needs both orders (Opus r1 m2).
                        Err(_) if dup => {
                            refuse(&mut counts, "excluded:group-m-duplicate", 1);
                            continue;
                        }
                        Err(_) if class != 2 && covers(&[(d.vtx, &d.body)]) => vec![(pair, d)],
                        Err(DryError::Refused(e)) => {
                            let e = e.split(':').next().unwrap_or(&e).to_owned();
                            refuse(&mut counts, &format!("refused:placement reversed {e}"), 2);
                            continue;
                        }
                    }
                }
                _ => vec![(pair, d)],
            };
            if matches!(class, 9 | 10)
                && !covers(&members.iter().map(|(_, d)| (d.vtx, &d.body[..])).collect::<Vec<_>>())
            {
                refuse(&mut counts, "refused:placement index 1 and last", members.len());
                continue;
            }
            units.push(unit_of(members));
        }
    }
    // Class 7: the scenario pairs (their solos: the scenario alone, the header part on the tip).
    for (a, b, mutants) in SCENARIOS {
        let (a, b) = (find_part(a), find_part(b));
        units.push(vec![(
            Pair {
                id: interned(&format!("{}~{}.child", part_id(a), part_id(b))),
                build: Build::Scenario {
                    a,
                    b,
                },
                site: Site::Reg,
                class: 7,
                witness: None,
                mutants,
                solos: ["", ""],
            },
            [String::new(), ".tip".to_owned()],
            None,
        )]);
        *counts.buildable.entry(7).or_default() += 1;
        counts.candidates += 1;
    }
    // The leg's budgets (explicit pairs first): P1's ten heights (P1_BASE..DIP0001_HEIGHT) and
    // the twenty MAX_MONEY mints that mature before BO_BASE ([`super::leg::mints`]).
    let p1_heights = (P1_BASE..DIP0001_HEIGHT).count();
    let mint_budget = (BUDGET_START..BO_BASE)
        .filter(|h| Site::BW.holds(*h) && h + build::MATURITY <= BO_BASE)
        .count();
    let (mut p1, mut minted) = (0, 0);
    let mut pairs: Vec<&'static Pair> = Vec::new();
    let mut cases: Vec<O10> = Vec::new();
    let mut solo_ids: BTreeSet<&'static str> = BTreeSet::new();
    let mut controls: BTreeMap<&'static str, Control> = BTreeMap::new();
    for unit in units {
        // A unit's cost: each placement and its solos not yet in the set.
        let mut ids = solo_ids.clone();
        let mut unit_controls = controls.clone();
        let (mut unit_p1, mut unit_minted) = (0, 0);
        let mut members: Vec<(Pair, Vec<usize>)> = Vec::new();
        for (mut pair, solos, ctl) in unit {
            for k in 0..2 {
                let part = match pair.build {
                    Build::Compose {
                        a,
                        b,
                        ..
                    }
                    | Build::Scenario {
                        a,
                        b,
                    } => part_id([a, b][k]),
                    Build::SameTx(s) => format!("{}.{k}", s.name()),
                };
                pair.solos[k] = interned(&format!("{part}{}", solos[k]));
            }
            // R2-F1: a solo another pair built is credited only if it keeps this pair's domains.
            if let Some(ctl) = &ctl {
                for k in 0..2 {
                    let solo = pair.solos[k];
                    match unit_controls.get(solo) {
                        Some(theirs) => credit(pair.id, solo, &ctl[0], theirs)?,
                        None => drop(unit_controls.insert(solo, ctl[1 + k].clone())),
                    }
                }
            }
            let new: Vec<usize> = (0..2).filter(|k| ids.insert(pair.solos[*k])).collect();
            let n = 1 + new.len();
            if pair.site == Site::P1 {
                unit_p1 += n;
            }
            unit_minted += n * pair.mints();
            members.push((pair, new));
        }
        let over = if p1 + unit_p1 > p1_heights {
            Some("excluded:p1-heights")
        } else if minted + unit_minted > mint_budget {
            Some("excluded:mint-budget")
        } else {
            None
        };
        if let Some(reason) = over {
            if let Some((pair, _)) =
                members.iter().find(|(p, _)| p.witness.is_some() || !p.mutants.is_empty())
            {
                return Err(format!("{}: explicit, but {reason}", pair.id));
            }
            *counts.refused.entry(reason.to_owned()).or_default() += members.len();
            continue;
        }
        (p1, minted, solo_ids, controls) = (p1 + unit_p1, minted + unit_minted, ids, unit_controls);
        for (pair, new) in members {
            *counts.selected.entry(pair.class).or_default() += 1;
            let pair: &'static Pair = leak(pair);
            pairs.push(pair);
            // The solos first: P8 credits a pair after both its controls passed (§4.4).
            for k in new {
                cases.push(O10 {
                    pair,
                    role: Role::Solo(k),
                });
                counts.solos += 1;
            }
            cases.push(O10 {
                pair,
                role: Role::Pair,
            });
        }
    }
    // §4.5 after reversal, dedupe and budgets: class 2 in both orders (Opus r1 m2); every
    // selected class 9 or 10 obligation holds its loop-body defect at index 1 and at the block's
    // last index over its placements in the set (Sol r1 S-F2); one that lost a placement fails
    // the set, named.
    reversed_class_2(&pairs)?;
    let mut obligations: Vec<Obligation> = Vec::new();
    for p in pairs.iter().filter(|p| matches!(p.class, 9 | 10)) {
        let members: Vec<&&Pair> =
            pairs.iter().filter(|q| obligation_of(q) == obligation_of(p)).collect();
        if members.first().is_some_and(|q| q.id != p.id) {
            continue;
        }
        let obligation = Obligation {
            class: p.class,
            id: p.id,
            site: p.site,
            edge: edges.get(p.id).copied(),
            placements: members.iter().filter_map(|q| bodies.get(q.id).cloned()).collect(),
        };
        if !obligation.covered() {
            return Err(format!(
                "{}: a class {} obligation without its loop-body defect at index 1 and at the \
                 last index: {:?}",
                obligation.id, obligation.class, obligation.placements
            ));
        }
        obligations.push(obligation);
    }
    keys.retain(|id, _| pairs.iter().any(|p| p.id == *id));
    let generated = Generated {
        pairs,
        cases,
        counts,
        keys,
        obligations,
    };
    // OD-L3-30: each row §4.5 names before or after a loop has such an obligation.
    named_rows_held(&generated.obligations)?;
    Ok(generated)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loop table's order is the model's: on a cold submit at `-par=1`, a row before a loop
    /// wins over the loop body's, and the body's over a row after it.
    #[test]
    fn the_loop_table_is_the_models() {
        let model = Model::load().expect("the model");
        let at = |vtx| At {
            vtx: Some(vtx),
            vin: None,
            vout: None,
        };
        let mut wrong = Vec::new();
        for (name, body, pre, post) in LOOPS {
            let mut checked = 0;
            let pairs = pre
                .iter()
                .flat_map(|p| body.iter().map(move |b| (*p, *b)))
                .chain(body.iter().flat_map(|b| post.iter().map(move |q| (*b, *q))));
            for (first, second) in pairs {
                for (x, y) in [(first, second), (second, first)] {
                    let got = precedence::predicted(
                        &model,
                        Mode::Submit,
                        Par::One,
                        &[(x, at(1)), (y, at(2))],
                        &|_| {},
                    );
                    match got {
                        Ok((Some((row, _)), _)) if row == first => checked += 1,
                        Ok(other) => wrong.push(format!("{name}: {x} at 1, {y} at 2: {other:?}")),
                        Err(_) => {}
                    }
                }
            }
            assert!(checked > 0, "{name}: no pair placed");
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The set builds (every explicit pair dry-runs, else the build fails), every class 1-10
    /// and every mutant M1-M10 has a pair, every §4.4 witness is a case, and the generator
    /// counts the model universe's refusals by typed reason: 40 infeasible on DIP3, 96
    /// unreachable after AcceptBlockHeader's return (v:4619-4625), and item 8b's iteration-guard
    /// conflicts at one index (v:4410-4412's coinbase skip: 38; v:358's special version: 16).
    #[test]
    fn the_set_generates() {
        let g = generated();
        eprintln!(
            "o10-2 set: {} pairs, {} cases: {}",
            g.pairs.len(),
            g.cases.len(),
            g.counts.json()
        );
        let u = |k: &str| g.counts.universe.get(k).copied().unwrap_or(0);
        assert_eq!(u("infeasible:dip3"), 40);
        assert_eq!(u("unreachable-after-return"), 96);
        assert_eq!(u("same-index iter-conflict:coinbase"), 38);
        assert_eq!(u("same-index iter-conflict:special_version"), 16);
        for class in 1..=10u8 {
            assert!(g.pairs.iter().any(|p| p.class == class), "no class {class} pair");
        }
        for m in 1..=10 {
            let m = format!("M{m}");
            assert!(g.pairs.iter().any(|p| p.mutants.contains(&m.as_str())), "no {m} pair");
        }
        let witnesses: BTreeSet<&str> = g.cases.iter().filter_map(|c| c.witness()).collect();
        for w in ["W1", "W2", "W3", "W4", "W5", "W6"] {
            assert!(witnesses.contains(w), "no {w}");
            assert!(witnesses.contains(format!("{w}.reversed").as_str()), "no {w}.reversed");
        }
        assert!(witnesses.contains("W3b"));
        // Every pair's solos are cases, each once.
        let solos: Vec<&str> =
            g.cases.iter().filter(|c| c.role != Role::Pair).map(|c| c.variant()).collect();
        let unique: BTreeSet<&str> = solos.iter().copied().collect();
        assert_eq!(solos.len(), unique.len());
        assert_eq!(solos.len(), g.counts.solos);
        for p in &g.pairs {
            for s in p.solos {
                assert!(unique.contains(s), "{}: solo {s} is no case", p.id);
            }
        }
        let selected: usize = g.counts.selected.values().sum();
        assert_eq!(selected, g.pairs.len());
    }

    /// OD-L3-30/31: every row §4.5 names before or after a loop has its class 9 or 10 obligation
    /// in the set, at index 1 and the last index. ContextualCheckBlock's size row (v:4563) is
    /// the explicit `locktime_height~block_size.1000001` in both orders; its sigops (v:4585) the
    /// P1 one-transaction `nonfinal~sigops_ctx`, the only named row a 2-transaction block may
    /// hold. Self-checks: the set less either fails, naming the row, and so does a named row
    /// outside [`SHORT_BLOCK_ONLY`] held only by a 2-transaction block.
    #[test]
    fn named_rows_have_their_obligations() {
        let g = generated();
        named_rows_held(&g.obligations).expect("the set holds every named row");
        let size = |o: &&Obligation| o.id.starts_with("locktime_height.reg~block_size.1000001.");
        let held: Vec<&Obligation> = g.obligations.iter().filter(size).collect();
        assert_eq!(held.len(), 1, "one CCB size obligation");
        assert_eq!((held[0].class, held[0].site), (9, Site::P1));
        assert_eq!(held[0].placements.len(), 2, "both orders");
        assert!(held[0].edge == Some(measure::CCB_LENGTH) && held[0].covered());
        let sigops = |o: &&Obligation| o.id == "same.nonfinal~sigops_ctx";
        let held: Vec<&Obligation> = g.obligations.iter().filter(sigops).collect();
        assert_eq!(held.len(), 1, "the CCB sigops obligation");
        assert_eq!((held[0].class, held[0].site), (10, Site::P1));
        assert_eq!(held[0].placements, [(2, vec![1])], "index 1, the last");
        assert_eq!(held[0].edge, Some(measure::CCB_SIGOPS));
        let less = |drop: &dyn Fn(&&Obligation) -> bool| -> Vec<Obligation> {
            g.obligations.iter().filter(|o| !drop(o)).cloned().collect()
        };
        for (drop, row, cite) in [
            (&size as &dyn Fn(&&Obligation) -> bool, measure::CCB_LENGTH, "v:4563-4564"),
            (&sigops, measure::CCB_SIGOPS, "v:4585"),
        ] {
            let Err(e) = named_rows_held(&less(drop)) else {
                panic!("the set without {row}'s obligation passes");
            };
            assert!(e.contains(row) && e.contains(cite), "{e}");
        }
        // CheckBlock's size row (v:4405) held only by a 2-transaction block: refused.
        let short: Vec<Obligation> = g
            .obligations
            .iter()
            .cloned()
            .map(|mut o| {
                if o.edge == Some(measure::CB_LENGTH) {
                    o.placements = vec![(2, vec![1])];
                }
                o
            })
            .collect();
        let Err(e) = named_rows_held(&short) else {
            panic!("a 2-transaction v:4405 obligation passes");
        };
        assert!(e.contains(measure::CB_LENGTH), "{e}");
    }

    /// Opus r1 m2: every selected composed class 2 pair runs both orders; the set less one
    /// pair's reversal fails, naming the pair left alone.
    #[test]
    fn class_2_pairs_run_both_orders() {
        let g = generated();
        reversed_class_2(&g.pairs).expect("both orders of every class 2 pair");
        let composed = |p: &&&Pair| p.class == 2 && matches!(p.build, Build::Compose { .. });
        let first = g.pairs.iter().find(composed).expect("a composed class 2 pair");
        let reversal = g
            .pairs
            .iter()
            .find(|q| q.id != first.id && obligation_of(q) == obligation_of(first))
            .expect("its reversal");
        let less: Vec<&Pair> = g.pairs.iter().copied().filter(|q| q.id != reversal.id).collect();
        let Err(e) = reversed_class_2(&less) else {
            panic!("a class 2 pair alone passes");
        };
        assert!(e.starts_with(first.id) && e.contains("without its reversal"), "{e}");
    }

    /// Opus r1 m1: a dry run's own failures fail the set, never count as typed refusals. A
    /// block `measure` cannot read (here a version-3 type-5 coinbase, whose payload it does not
    /// measure) is a harness error naming the pair, as are an unbound reason and a reason bound
    /// to another row (naming both rows); only an unsound composition and an ambiguous binding
    /// are refusals.
    #[test]
    fn dry_run_harness_errors_fail_the_build() {
        let pair = Pair {
            id: "bip68_height.reg~in_below_out.reg.ab",
            build: Build::Compose {
                a: find_part("bip68_height.reg"),
                b: find_part("in_below_out.reg"),
                order: Order::Ab,
                fill: 0,
            },
            site: Site::Reg,
            class: 0,
            witness: None,
            mutants: &[],
            solos: ["", ""],
        };
        let dry = Dry::new(pair.site, &pair.locks(), pair.mints());
        let place = dry.place();
        let mut blocks = build(&pair, &dry.world(*b"o10-test")).expect("the blocks");
        assert!(measures(&blocks, &place).is_ok(), "the pair measures");
        let coinbase = &mut blocks.solos[1].txs[0];
        (coinbase.version, coinbase.tx_type) = (3, 5);
        let Err(e @ MeasureError::Measurement(_)) = measures(&blocks, &place) else {
            panic!("an unmeasured coinbase is a measurement error");
        };
        let DryError::Harness(e) = measured(pair.id, e) else {
            panic!("a measurement error is refused");
        };
        assert!(e.starts_with(pair.id) && e.contains("type-5 coinbase"), "{e}");
        assert_eq!(
            measured(pair.id, MeasureError::Composition("x".into())),
            DryError::Refused("broken: broken composition: x".into())
        );
        let want = Some((measure::CONN_BIP68.to_owned(), false));
        let b = |got| bound(pair.id, Role::Solo(1), Mode::Submit, Par::One, &want, got);
        assert_eq!(b(None), Ok(()));
        assert_eq!(b(Some(Binding::Bound(measure::CONN_BIP68.into()))), Ok(()));
        assert_eq!(
            b(Some(Binding::Ambiguous(vec![
                measure::CONN_BIP68.into(),
                measure::CCB_FINAL.into()
            ]))),
            Err(DryError::Refused("binding-ambiguous".into()))
        );
        for (got, names) in [
            (Binding::Unbound, "Unbound"),
            (Binding::Bound(measure::CTI_BELOW.into()), measure::CTI_BELOW),
        ] {
            let Err(DryError::Harness(e)) = b(Some(got)) else {
                panic!("{names}: refused, not failed");
            };
            assert!(
                e.starts_with(pair.id) && e.contains(measure::CONN_BIP68) && e.contains(names),
                "{e}"
            );
        }
    }

    /// R2-F1: a control's identity is its site, defects, setting and every transaction's shape.
    /// `vout_toolarge~sigops_check.40001`'s sigops controls ((1, 1) spends) are never
    /// `sigops_check~vin_empty`'s ((1, 2) twins), which the old identity reused; credited the
    /// other's control, the pair fails, and so does a control from another site.
    #[test]
    fn controls_keep_their_pairs_domains() {
        let (model, inventory) =
            (Model::load().expect("the model"), Inventory::load().expect("the fixture"));
        let dried = |a: &str, b: &str, order: Order| {
            let pair = Pair {
                id: interned(&format!("{a}~{b}.{order:?}")),
                build: Build::Compose {
                    a: find_part(a),
                    b: find_part(b),
                    order,
                    fill: 0,
                },
                site: Site::Reg,
                class: 0,
                witness: None,
                mutants: &[],
                solos: ["", ""],
            };
            let d = dry_run(&model, &inventory, &pair).expect("a dry run");
            (pair.id, d)
        };
        let sigops = "sigops_check.40001";
        for order in [Order::Ab, Order::Ba] {
            let (pair, ours) = dried("vout_toolarge.reg", sigops, order);
            let mut credited = 0;
            for theirs_order in [Order::Ab, Order::Ba] {
                let (_, theirs) = dried(sigops, "vin_empty.reg", theirs_order);
                // Our sigops control (solo 1) is never theirs (solo 0).
                assert_ne!(ours.solos[1], theirs.solos[0], "{pair}: a shared control id");
                // Theirs in our place: refused unless it keeps our domains.
                let solo = format!("{sigops}{}", theirs.solos[0]);
                if let Err(e) = credit(pair, &solo, &ours.controls[0], &theirs.controls[1]) {
                    assert!(e.contains("changes"), "{e}");
                    credited += 1;
                }
            }
            assert!(credited > 0, "{pair}: some wrong-shape control is refused");
            // Our own control is creditable; at another site it is not.
            credit(pair, "ours", &ours.controls[0], &ours.controls[2]).expect("our control");
            let mut moved = ours.controls[2].clone();
            moved.0 = Site::P1;
            let Err(e) = credit(pair, "moved", &ours.controls[0], &moved) else {
                panic!("{pair}: a control from another site is credited");
            };
            assert!(e.contains("runs at P1"), "{e}");
        }
        // In the set, a selected such pair shares its sigops controls with no
        // `sigops_check~vin_empty` pair (a pair of equal domains may share one: the set's
        // `credit` check admitted it).
        let g = generated();
        for p in g.pairs.iter().filter(|p| p.id.starts_with("vout_toolarge.reg~sigops_check")) {
            for q in g.pairs.iter().filter(|q| q.solos.contains(&p.solos[1])) {
                assert!(!q.id.starts_with("sigops_check.40001~vin_empty"), "{} {}", p.id, q.id);
            }
        }
    }

    /// Sol r1 S-F2: the set's build checks each witness at §4.4's placement and every class 9
    /// or 10 obligation at index 1 and the last index; the checks refuse a witness one index off,
    /// W4 with its clean tx2 missing, and an obligation with one placement.
    #[test]
    fn placements_are_checked() {
        let at = |row: &str, vtx: Option<usize>, vin: Option<usize>| {
            (
                row.to_owned(),
                At {
                    vtx,
                    vin,
                    vout: None,
                },
            )
        };
        let w1 =
            vec![at(measure::CONN_BIP68, Some(1), None), at(measure::CTI_BELOW, Some(2), None)];
        assert_eq!(placed("W1", &w1, 3), Ok(()));
        assert!(placed("W1.reversed", &w1, 3).is_err());
        let w4 = vec![at(measure::CIS_SCRIPT, Some(1), Some(0))];
        assert_eq!(placed("W4", &w4, 3), Ok(()));
        assert!(placed("W4", &w4, 2).is_err(), "no clean tx2");
        assert!(placed("W4", &[at(measure::CIS_SCRIPT, Some(2), Some(0))].to_vec(), 4).is_err());
        let vin_empty = measure::CHECK_TRANSACTION[0];
        let w6 = vec![at(vin_empty, Some(11), None), at(measure::CB_SIGOPS, None, None)];
        assert_eq!(placed("W6", &w6, 12), Ok(()));
        assert!(placed("W6", &w6, 13).is_err(), "not the last index");
        // A two-transaction block's index 1 is its last; a longer one needs both placements.
        assert!(covers(&[(2, &[1])]));
        assert!(!covers(&[(23, &[1])]));
        assert!(covers(&[(23, &[1]), (23, &[22])]));
        assert!(!covers(&[(23, &[2]), (23, &[22])]), "a funding transaction before the body");
        // The set holds them: every witness case is placed (the build checks it) and present.
        let g = generated();
        for p in g.pairs.iter().filter(|p| matches!(p.class, 9 | 10)) {
            assert!(g.keys.contains_key(p.id), "{}", p.id);
        }
    }

    /// Class 7 (§4.5): S2's child off its failed parent's bits is refused for the parent first
    /// (v:4651 in AcceptBlockHeader before ContextualCheckBlockHeader at v:4660); S6's D′ for
    /// its bits first (v:4660 before the failed-set walk at v:4699); each scenario's last step
    /// alone is its declared occurrence.
    #[test]
    fn the_scenario_pairs_are_the_models() {
        let model = Model::load().expect("the model");
        let bits = "validation.cpp:ContextualCheckBlockHeader#2:bad-diffbits";
        let failed = "validation.cpp:AcceptBlockHeader#4:bad-prevblk";
        let walk = "validation.cpp:AcceptBlockHeader#6:bad-prevblk";
        for (mode, declared, winner) in
            [(Mode::Submit, failed, failed), (Mode::Submit, walk, bits), (Mode::Header, walk, bits)]
        {
            for par in PARS {
                let fired = |header: &[&str]| {
                    pairs::scenario_answer(&model, mode, par, declared, header)
                        .expect("an answer")
                        .0
                        .map(|f| f.0)
                };
                assert_eq!(fired(&[bits]).as_deref(), Some(winner), "{declared} on {mode:?}");
                assert_eq!(fired(&[]).as_deref(), Some(declared));
            }
        }
    }

    /// Opus r1 m3: a class 7 pair's last block measures its header part's row from the bytes:
    /// none on the clean child, bad-diffbits once `bits_minus` sets its bits.
    #[test]
    fn the_scenario_header_row_is_measured() {
        let dry = Dry::new(Site::Reg, &[], 0);
        let world = dry.world(*b"o10-test");
        let mut plan = world.template.clone();
        let place = measure::HeaderPlace {
            height: world.height,
            mtp: world.mtp,
            now: world.mocktime,
            bits: plan.bits,
        };
        assert_eq!(measure::header_rows(&plan, &place), Ok(vec![]));
        let b = find_part("bits_minus.reg");
        pairs::scenario_last(b, &mut plan, world.height, world.mtp, world.mocktime);
        assert_eq!(measure::header_rows(&plan, &place), Ok(vec![measure::CCBH_BITS.to_owned()]));
    }

    /// A pair's measured blocks on its dry chain.
    fn dry_measures(pair: &Pair) -> pairs::Measures {
        let dry = Dry::new(pair.site, &pair.locks(), pair.mints());
        let world = dry.world(*b"o10-dry\0");
        let blocks = build(pair, &world).expect("the blocks");
        measures(&blocks, &dry.place()).expect("a sound composition")
    }

    /// §4.5 class 6's dedupe: a10 group M's every variant is keyed (its measured rows and
    /// placement, at both `-par`), and no generated pair is a group M case unless an explicit
    /// pair names it, or it is one placement of a both-orders obligation whose other placement is
    /// new (the dedupe never removes a required placement). The key holds the placement: W4 reversed has its script fault at tx1
    /// (§4.4; S-F2's prefunded coin), group M's script-first case at tx2 after its funding
    /// transaction, so the two are different cases with the same rows.
    #[test]
    fn class_6_is_deduplicated_against_group_m() {
        let (keys, unread) = group_m();
        let m_variants = footprint::variants().filter(|(o, _)| o.name.starts_with("m_")).count();
        assert_eq!((keys.len(), unread), (m_variants, 0));
        let g = generated();
        let reversal = |p: &Pair| match p.build {
            Build::Compose {
                a,
                b,
                order,
                fill,
            } => g.pairs.iter().find(|q| {
                matches!(q.build, Build::Compose { a: qa, b: qb, order: qo, fill: qf }
                    if part_id(qa) == part_id(a) && part_id(qb) == part_id(b)
                        && qo != order && qf == fill)
            }),
            _ => None,
        };
        let mut kept = Vec::new();
        for p in &g.pairs {
            if let Some(key) = g.keys.get(p.id) {
                if let Some(m) = keys.get(key) {
                    let explicit = p.witness.is_some() || !p.mutants.is_empty();
                    let required = BOTH_ORDERS.contains(&p.class)
                        && reversal(p).is_some_and(|q| !keys.contains_key(&g.keys[q.id]));
                    assert!(explicit || required, "{} is group M's {m}", p.id);
                    if required {
                        kept.push(p.id);
                    }
                }
                assert_eq!(key, &{
                    let mut k = dry_measures(p).pair.defects;
                    k.sort();
                    k
                });
            }
        }
        let w4 = g.pairs.iter().find(|p| p.witness == Some("W4.reversed")).expect("W4 reversed");
        let script_first = keys
            .iter()
            .find(|(_, m)| *m == "m_bip68.script_first")
            .map(|(k, _)| k)
            .expect("group M's script-first case");
        let script_at =
            |key: &Key| key.iter().find(|(r, _)| r == measure::CIS_SCRIPT).and_then(|(_, a)| a.vtx);
        let rows = |key: &Key| key.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>();
        assert_eq!(rows(&g.keys[w4.id]), rows(script_first));
        assert_eq!((script_at(&g.keys[w4.id]), script_at(script_first)), (Some(1), Some(2)));
        eprintln!("group M placements kept for their obligations: {}", kept.join(", "));
    }

    /// Carry-over 4: interacting labels come from §4.3's declared rules only. Every selected
    /// pair, with its parts' footprints, meets no rule on any mode or `-par` (`check_label`
    /// as independent), and parts that set one field meet the footprint rule.
    #[test]
    fn interacting_labels_are_the_declared_rules() {
        let model = Model::load().expect("the model");
        for p in generated().pairs.iter().filter(|p| !matches!(p.build, Build::Scenario { .. })) {
            let m = dry_measures(p);
            for mode in MODES {
                for par in PARS {
                    let (mut instances, _) =
                        setting_of(&model, mode, par, &m.pair, &m.pair.facts).expect("a setting");
                    pairs::with_footprints(&mut instances, p, &m).expect("the footprints");
                    precedence::check_label(model.root(mode), &instances, false)
                        .unwrap_or_else(|e| panic!("{} on {mode:?} at {par:?}: {e}", p.id));
                }
            }
        }
        // The rule is what labels: the same composed instances, the second part also setting
        // block.sigops (as sigops_ctx.20001 does), meet the footprint rule on it.
        let (a, b) = (find_part("sigops_check.40001"), find_part("in_below_out.reg"));
        let pair = Pair {
            id: "test",
            build: Build::Compose {
                a,
                b,
                order: Order::Ab,
                fill: 0,
            },
            site: Site::Reg,
            class: 0,
            witness: None,
            mutants: &[],
            solos: ["", ""],
        };
        let m = dry_measures(&pair);
        let (mut instances, _) =
            setting_of(&model, Mode::Submit, Par::One, &m.pair, &m.pair.facts).expect("a setting");
        pairs::with_footprints(&mut instances, &pair, &m).expect("the footprints");
        let first = |row: &str| instances.iter().position(|i| i.row == row).expect("the row");
        let (x, y) = (first(measure::CB_SIGOPS), first(measure::CTI_BELOW));
        instances[y].footprint.insert(footprint::BLOCK_SIGOPS.to_owned());
        assert!(matches!(
            precedence::interaction(model.root(Mode::Submit), &instances),
            Some(precedence::Rule::Footprint { a, b, field }) if (a, b) == (x.min(y), x.max(y)) && field == footprint::BLOCK_SIGOPS
        ));
        precedence::check_label(model.root(Mode::Submit), &instances, true)
            .expect("labelled interacting");
    }
}
