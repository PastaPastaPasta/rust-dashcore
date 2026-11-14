//! A run's report (plan §3.4's contract with `contrib/node/l3-tier1.sh`): the dashd and source
//! it ran, every case's trials with their planes' results, and each leg's status.
//!
//! `{cases, manifest: [{trial_id, status}], legs: [{name, status, multi_candidate_up_posts}]}`
//! is what the runner reads; a leg is `done` only when it and its replay finished clean.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use super::evo;
use super::generator::{self, FAMILIES};
use super::inventory::Binding;
use super::leg::{GenRecord, LegLog, Set, TrialRecord, cases};
use super::pairs::Role;
use super::pairset;
use super::planes;
use super::replay::Replayed;
use super::trial::{Kind, TrialId, hex};
use super::version::Dashd;
use crate::chain::mutate::{EXCLUDED, Mode, PINNED, Par, TWIN_CONTROLS, label};

/// A trial's class status (plan §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// A submit whose bound class is Core's runtime class (the replay) and V's.
    Runtime,
    /// A proposal whose bound occurrence has a runtime submit in the run, with V's TBV class
    /// equal to it.
    Transferred,
    SiteOnly,
    Unverified,
    /// An accept or an RPC refusal: no occurrence, no class.
    None,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Runtime => "runtime",
            Class::Transferred => "transferred",
            Class::SiteOnly => "site-only",
            Class::Unverified => "unverified",
            Class::None => "n/a",
        }
    }
}

/// The class statuses of a leg's trials, given its replay; `Err` for a class plane that
/// diverges (P4: V's or Core's class is not the bound row's).
pub fn classes(log: &LegLog, replayed: &Replayed) -> Result<Vec<Class>, String> {
    let runtime = |t: &TrialRecord| replayed.runtime.get(&t.op).cloned().flatten();
    let mut runtime_keys = BTreeSet::new();
    // OD-L3-24: a header op has no class on either side (no BlockChecked from
    // ProcessNewBlockHeaders; the shim's reply carries none), so a bound header trial is
    // site-only, and a class from V or the replay is a divergence (a tripwire for BL-600).
    for t in log.trials.iter().filter(|t| t.mode == Mode::Header) {
        if t.v_class.is_some() || runtime(t).is_some() {
            return Err(format!(
                "P4 diverges for {}: a header op with a class (V {:?}, Core {:?})",
                t.trial,
                t.v_class,
                runtime(t)
            ));
        }
    }
    for t in log.trials.iter().filter(|t| t.mode == Mode::Submit) {
        if let (Some(Binding::Bound(key)), Some(class)) = (&t.binding, &t.bound_class) {
            if t.v_class.as_ref() != Some(class) {
                return Err(format!(
                    "P4 diverges for {}: bound {class}, V {:?}",
                    t.trial, t.v_class
                ));
            }
            match runtime(t) {
                Some(core) if &core == class => {
                    runtime_keys.insert(key.clone());
                }
                core => {
                    return Err(format!(
                        "P4 diverges for {}: bound {class}, Core {core:?}",
                        t.trial
                    ));
                }
            }
        }
    }
    log.trials
        .iter()
        .map(|t| match (&t.binding, t.mode) {
            (None, _) => Ok(Class::None),
            (Some(Binding::Bound(_)), Mode::Submit) => Ok(Class::Runtime),
            (Some(Binding::Bound(_)), Mode::Header) => Ok(Class::SiteOnly),
            // A bound proposal: V's TBV class is the bound row's, or the class plane diverges
            // (no class: V's TBV found the block valid where dashd refused it).
            (Some(Binding::Bound(key)), _) => match &t.v_class {
                Some(v) if Some(v) == t.bound_class.as_ref() => Ok(if runtime_keys.contains(key) {
                    Class::Transferred
                } else {
                    Class::SiteOnly
                }),
                v => {
                    Err(format!("P4 diverges for {}: bound {:?}, V {v:?}", t.trial, t.bound_class))
                }
            },
            (Some(_), _) => Ok(Class::Unverified),
        })
        .collect()
}

/// One leg's part of the report, and whether it is done.
pub fn leg(log: &LegLog, replayed: &Replayed, only: Option<&str>) -> (Value, bool) {
    let classes = classes(log, replayed);
    let error = log
        .error
        .clone()
        .or_else(|| replayed.error.clone())
        .or_else(|| classes.as_ref().err().cloned());
    let classes = classes.unwrap_or_default();
    let trials: Vec<Value> = log
        .trials
        .iter()
        .enumerate()
        .map(|(i, t)| {
            json!({
                "trial_id": t.trial.to_string(),
                "label": label(t.trial.case.op, t.trial.case.variant, t.trial.case.site),
                "hash": hex(&t.hash),
                "reply": planes::outcome_json(&t.reply),
                "occurrence": match &t.binding {
                    None => json!(null),
                    Some(Binding::Bound(k)) => json!({"bound": k}),
                    Some(Binding::Ambiguous(ks)) => json!({"ambiguous": ks}),
                    Some(Binding::Unbound) => json!("unbound"),
                },
                "class": classes.get(i).map_or("unscored", |c| c.name()),
                "bound_class": t.bound_class,
                "v_class": t.v_class,
                "core_class": replayed.runtime.get(&t.op),
                "effects": {"known": t.effects.known, "failed": t.effects.failed, "stored": t.effects.stored},
                "evo": t.evo,
                "declined": t.declined,
                "removed": t.removed,
                "gained": t.gained,
            })
        })
        .collect();
    let done = error.is_none();
    let value = json!({
        "name": log.name,
        "status": if done { "done" } else { "failed" },
        "error": error,
        "chain_args": log.chain_args,
        "dashd_argv": log.dashd_argv,
        "load_refused": log.load_refused,
        "ops": log.ops.len(),
        "seconds": log.seconds,
        "timing": {
            "rpc": log.rpc_timing.iter()
                .map(|(m, (n, s))| (m.clone(), json!({"calls": n, "seconds": s})))
                .collect::<serde_json::Map<_, _>>(),
            "v_steps": {"calls": log.v_steps.0, "seconds": log.v_steps.1},
        },
        "multi_candidate_up_posts": replayed.multi_candidate_ops.len(),
        "multi_candidate_ops": replayed.multi_candidate_ops,
        "evo_keys": evo::report(evo::EVO_KEYS, &log.evo_counts),
        "multi_candidate_source": "Core's post in the shim replay (ABI 33); V's equal by P7",
        // What P7 compares (M-11, OD-L3-15).
        "p7": {
            "best_header": "V = the §3.7 model after every planed op; Core = V and Core = the model in the replay",
            "index_digest": "Core = V in the replay",
            "best_invalid": "Core = V after every op in the replay",
            "candidates": "Core = V after every op in the replay, in set order; a tie on (work, nSequenceId) fails (Q-H)",
            "model": "the §3.7 model's best header, best_invalid and candidates = Core's once Core = V on every plane, or a harness error (OD-L3-35)",
            "order_equivalence": if replayed.multi_candidate_ops.is_empty() { "holds: no up post held more than one candidate" } else { "void (OD-L3-19)" },
        },
        "replay": {
            "abi": replayed.abi,
            "error": replayed.error,
            "proposals": replayed.proposals,
            "proposal_checked": replayed.proposal_checked,
            "model_checked": replayed.model_checked,
            "seconds": replayed.seconds,
        },
        "av": log.av,
        "trials": trials,
        // A filtered run's manifest holds only its selected trials (O10 §5.3: such a report is
        // incomplete, never a claim input).
        "only": only,
        "manifest": manifest(log, only),
        // §4.4's P8: the O10 trials whose bound occurrence was the walker's, V's reply dashd's.
        // O10 §3.4: each generated case's outcome, draws passed over and checked mutations.
        "generated": log.generated.iter().map(generated_json).collect::<Vec<_>>(),
        "p8": log.p8.iter().map(|p| json!({
            "trial_id": p.trial.to_string(),
            "pair": p.pair,
            "role": match p.role {
                Role::Pair => "pair".to_owned(),
                Role::Solo(k) => format!("solo{k}"),
            },
            "want": p.want,
        })).collect::<Vec<_>>(),
    });
    (value, done)
}

/// The generated cases a leg ended in a typed terminal sampling outcome (OD-L3-55): a `noop` or
/// `filtered` record with every one of its [`generator::DRAWS`] draws recorded, each passed over
/// for that outcome's reasons (a `filtered` case's all refusals, a `noop` case's no-ops or
/// refusals with one no-op at least). Such a case completed with no trial to run and no
/// verified coverage.
fn sampled(log: &LegLog) -> BTreeMap<String, &'static str> {
    log.generated
        .iter()
        .filter(|g| {
            let reasons: Vec<&str> = g.tried.iter().map(|t| t.reason.as_str()).collect();
            let noop = |r: &&str| r.starts_with("noop");
            let filtered = |r: &&str| r.starts_with("filtered: ");
            let every = reasons.len() == generator::DRAWS as usize
                && g.tried.iter().zip(0..).all(|(t, r)| t.r == r)
                && g.trials.is_empty();
            every
                && match g.outcome {
                    "noop" => {
                        reasons.iter().all(|r| noop(r) || filtered(r)) && reasons.iter().any(noop)
                    }
                    "filtered" => reasons.iter().all(filtered),
                    _ => false,
                }
        })
        .map(|g| (g.case.to_string(), g.outcome))
        .collect()
}

/// The leg's manifest rows: every selected trial, `ran` once its record exists, `sampled` (with
/// the outcome) when its generated case ended in a typed terminal sampling outcome
/// ([`sampled`]), else `not run`; then (in an unfiltered run of a set with group M) group M's
/// excluded rows with their reasons.
fn manifest(log: &LegLog, only: Option<&str>) -> Vec<Value> {
    let ran: BTreeSet<String> = log.trials.iter().map(|t| t.trial.to_string()).collect();
    let sampled = sampled(log);
    let group_m = only.is_none() && matches!(log.set, Set::Tier1 | Set::Full);
    let excluded = EXCLUDED.iter().filter(|_| group_m).map(|(id, reason)| {
        json!({"trial_id": format!("{id}/{}", log.par.name()), "status": "excluded", "reason": reason})
    });
    // An AV fixture plans its one case's cold submit (plan §7.4).
    let (planned, excluded): (Vec<_>, Vec<_>) = match log.fixture {
        Some(d) => (vec![(d.case(log.par), vec![Kind::Cold])], Vec::new()),
        None => (
            // A set whose cases cannot be laid out failed its leg, which says why.
            cases(log.set, log.par)
                .unwrap_or_default()
                .into_iter()
                .map(|c| (c, c.kinds(log.set)))
                .collect(),
            excluded.collect(),
        ),
    };
    planned
        .into_iter()
        .filter(|(c, _)| only.is_none_or(|o| c.id.selected_by(o)) && c.in_shard(log.shard))
        .flat_map(|(c, kinds)| {
            kinds.into_iter().map(move |kind| TrialId {
                case: c.id,
                kind,
            })
        })
        .map(|t| {
            let id = t.to_string();
            match sampled.get(&t.case.to_string()) {
                _ if ran.contains(&id) => json!({"trial_id": id, "status": "ran"}),
                Some(outcome) => json!({"trial_id": id, "status": "sampled", "outcome": outcome}),
                None => json!({"trial_id": id, "status": "not run"}),
            }
        })
        .chain(excluded)
        .collect()
}

/// The run's report.
pub fn run(dashd: &Dashd, rdc_head: &str, set: &str, legs: &[(Value, bool)]) -> Value {
    let manifest: Vec<Value> = legs
        .iter()
        .flat_map(|(l, _)| l["manifest"].as_array().cloned().unwrap_or_default())
        .collect();
    let cases: BTreeSet<String> = manifest
        .iter()
        .filter_map(|m| {
            m["trial_id"].as_str().and_then(|t| t.rsplit_once('+')).map(|(c, _)| c.to_owned())
        })
        .collect();
    let mut value = json!({
        "set": set,
        "dashd": {
            "version": dashd.version,
            "binary": dashd.binary,
            "line": dashd.line,
            "sha256": dashd.sha256,
            "tag": dashd.tag,
            "commit": dashd.commit,
        },
        "rdc_head": rdc_head,
        "node_rs_keys": evo::node_rs_keys(),
        "cases": cases.len(),
        "manifest": manifest,
        "legs": legs.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
    });
    if set == Set::O10Pairs.name() {
        // §5.3: complete when every leg is done, unfiltered, and ran its whole manifest.
        let complete = legs.iter().all(|(l, done)| *done && l["only"].is_null())
            && manifest.iter().all(|m| m["status"] == "ran");
        let passed: Vec<(Par, BTreeSet<String>)> = legs
            .iter()
            .filter_map(|(l, _)| {
                let par = [Par::One, Par::Four].into_iter().find(|p| l["name"] == p.name())?;
                let ids = l["p8"].as_array()?.iter();
                Some((par, ids.filter_map(|p| p["trial_id"].as_str().map(str::to_owned)).collect()))
            })
            .collect();
        value["complete"] = json!(complete);
        value["o10_cases"] = json!(pairset::o10_cases(&passed));
        value["o10"] = pairset::generated().counts.json();
    }
    if set == Set::O10Gen.name() {
        let ran = trials_complete(legs, &manifest);
        let mut cases = twin_cases(legs);
        let (summary, met) = match generator::config() {
            Ok(config) => generated_summary(legs, config),
            Err(e) => (json!({"error": e}), false),
        };
        cases.insert(
            MINIMUMS.into(),
            if ran && met {
                "pass"
            } else {
                "fail"
            },
        );
        let pinned = pinned_cases(legs);
        let pinned_pass = pinned.values().all(|s| *s == "pass");
        // §3.4: a run below a minimum is incomplete, never green; so is one whose pinned case
        // (DEC-1059) did not pass.
        value["complete"] = json!(ran && met && pinned_pass);
        value["o10_cases"] = json!(cases);
        value["pinned"] = json!(pinned);
        value["generator"] = summary;
    }
    value
}

/// Whether an o10-gen run's legs are done and unfiltered, and every manifest trial ran or
/// belongs to a case that ended in a typed terminal sampling outcome (OD-L3-55). A missing
/// outcome record or a missing picked trial is `not run`, so incomplete; a harness error leaves
/// its leg not done.
fn trials_complete(legs: &[(Value, bool)], manifest: &[Value]) -> bool {
    legs.iter().all(|(l, done)| *done && l["only"].is_null())
        && manifest.iter().all(|m| m["status"] == "ran" || m["status"] == "sampled")
}

/// The census case §3.4's minimums are claimed as.
pub const MINIMUMS: &str = "o10.gen.minimums";
/// §3.4's minimums at the nightly's N per `-par`: every family's verified non-no-op cases at
/// each `-par`, and the distinct bound occurrences across the run.
pub const MIN_N: usize = 2_000;
pub const MIN_VERIFIED: u64 = 40;
pub const MIN_DISTINCT_BOUND: usize = 25;

/// A generated case's record, as the leg's report lists it.
fn generated_json(g: &GenRecord) -> Value {
    json!({
        "case": g.case.to_string(),
        "outcome": g.outcome,
        "r": g.r,
        "drawn": g.drawn,
        "unground": g.unground,
        "trials": g.trials.iter().map(|(t, checked)| json!({
            "trial_id": t.to_string(),
            "checked": checked.iter().map(|c| json!({
                "family": c.family,
                "target": format!("{:?}", c.target),
                "field": format!("{:?}", c.field),
                "old": c.old,
                "new": c.new,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "tried": g.tried.iter().map(|t| json!({
            "r": t.r,
            "drawn": t.drawn,
            "mutations": t.mutations,
            "reason": t.reason,
        })).collect::<Vec<_>>(),
    })
}

/// The generator's histograms (§3.4), read from the legs' reports: per `-par` and family the
/// draws that drew it, the mutations applied, the cases whose mutation of it was verified on the
/// final bytes of every trial that ran, and the draws passed over as no-ops or refused by the
/// regime filter (sampling losses, reported); per bound occurrence its trials by (mode, `-par`);
/// per (mode, `-par`) the binder's outcomes over the generated trials (OD-L3-64: `bound`,
/// `ambiguous` with their keys, `accepted`, `rpc-answer`, `rpc-error`, and `unbound`, which a leg
/// refuses as a harness error); and whether the minimums hold.
pub fn generated_summary(legs: &[(Value, bool)], config: &generator::Config) -> (Value, bool) {
    #[derive(Default)]
    struct Family {
        drawn: u64,
        applied: u64,
        verified: u64,
        noop: u64,
        filtered: u64,
        /// Of `filtered`, DEC-1059's pre-DIP3 `nType == 6` refusals.
        filtered_qc: u64,
    }
    /// A `-par`'s families, case outcomes and unground cases.
    #[derive(Default)]
    struct ParCounts {
        families: BTreeMap<&'static str, Family>,
        outcomes: BTreeMap<&'static str, u64>,
        unground: u64,
    }
    let mut pars: BTreeMap<String, ParCounts> = BTreeMap::new();
    let mut bound: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut binder: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for (leg, _) in legs {
        let name = leg["name"].as_str().unwrap_or_default();
        let par = name.split('.').next().unwrap_or_default().to_owned();
        let counts = pars.entry(par.clone()).or_insert_with(|| ParCounts {
            families: FAMILIES.iter().map(|f| (*f, Family::default())).collect(),
            ..ParCounts::default()
        });
        let (families, outcomes, unground) =
            (&mut counts.families, &mut counts.outcomes, &mut counts.unground);
        let strs = |v: &Value| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        };
        for g in leg["generated"].as_array().into_iter().flatten() {
            let outcome = match g["outcome"].as_str() {
                Some("picked") => "picked",
                Some("noop") => "noop",
                Some("filtered") => "filtered",
                _ => "unknown",
            };
            *outcomes.entry(outcome).or_default() += 1;
            for t in g["tried"].as_array().into_iter().flatten() {
                let reason = t["reason"].as_str().unwrap_or_default();
                for f in strs(&t["drawn"]) {
                    if let Some(x) = families.get_mut(f.as_str()) {
                        x.drawn += 1;
                        if reason.starts_with("noop") {
                            x.noop += 1;
                        } else if let Some(why) = reason.strip_prefix("filtered: ") {
                            x.filtered += 1;
                            x.filtered_qc +=
                                u64::from(why.starts_with(generator::QC_TYPE_PRE_DIP3));
                        }
                    }
                }
                for f in strs(&t["mutations"]) {
                    if let Some(x) = families.get_mut(f.as_str()) {
                        x.applied += 1;
                    }
                }
            }
            if outcome != "picked" {
                continue;
            }
            if g["unground"] == true {
                *unground += 1;
            }
            for f in strs(&g["drawn"]) {
                if let Some(x) = families.get_mut(f.as_str()) {
                    x.drawn += 1;
                }
            }
            let trials: Vec<BTreeSet<String>> = g["trials"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|t| {
                    t["checked"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| c["family"].as_str().map(str::to_owned))
                        .collect()
                })
                .collect();
            if let Some(first) = g["trials"][0]["checked"].as_array() {
                for c in first {
                    if let Some(x) = c["family"].as_str().and_then(|f| families.get_mut(f)) {
                        x.applied += 1;
                    }
                }
            }
            // A case's family is verified when every trial checked it (each trial in the
            // record ran), counted once a case.
            if let Some((first, rest)) = trials.split_first() {
                for f in first.iter().filter(|f| rest.iter().all(|t| t.contains(*f))) {
                    if let Some(x) = families.get_mut(f.as_str()) {
                        x.verified += 1;
                    }
                }
            }
        }
        for t in leg["trials"].as_array().into_iter().flatten() {
            let id = t["trial_id"].as_str().unwrap_or_default();
            if !id.starts_with(&format!("{}.", generator::GEN_OPERATOR.name)) {
                continue;
            }
            let mode = match id.rsplit_once('+').map(|(_, k)| k) {
                Some("cold") => "Submit",
                Some("prop") => "Proposal",
                _ => "unknown",
            };
            let occurrence = &t["occurrence"];
            let outcome = if occurrence["bound"].is_string() {
                "bound"
            } else if occurrence["ambiguous"].is_array() {
                "ambiguous"
            } else if occurrence.as_str() == Some("unbound") {
                "unbound"
            } else if t["reply"].get("error").is_some() {
                "rpc-error"
            } else if t["reply"]["result"].is_null() {
                "accepted"
            } else {
                "rpc-answer"
            };
            let counts = binder.entry(format!("{mode}/{par}")).or_default();
            let count = counts.entry(outcome.to_owned()).or_insert(json!(0));
            *count = json!(count.as_u64().unwrap_or_default() + 1);
            if let Some(keys) = occurrence["ambiguous"].as_array() {
                let keys: Vec<&str> = keys.iter().filter_map(Value::as_str).collect();
                let by_keys =
                    counts.entry("ambiguous_keys".to_owned()).or_insert_with(|| json!({}));
                let n = &mut by_keys[keys.join(" | ")];
                *n = json!(n.as_u64().unwrap_or_default() + 1);
            }
            if let Some(key) = occurrence["bound"].as_str() {
                *bound
                    .entry(key.to_owned())
                    .or_default()
                    .entry(format!("{mode}/{par}"))
                    .or_default() += 1;
            }
        }
    }
    let mut short = Vec::new();
    if config.n < MIN_N {
        short.push(format!("N = {} per -par, the minimums are at {MIN_N}", config.n));
    }
    for want in ["par1", "par4"] {
        match pars.get(want) {
            None => short.push(format!("no {want} leg")),
            Some(counts) => {
                for (f, x) in &counts.families {
                    if x.verified < MIN_VERIFIED {
                        short.push(format!(
                            "{want} {f}: {} verified, under {MIN_VERIFIED}",
                            x.verified
                        ));
                    }
                }
            }
        }
    }
    if bound.len() < MIN_DISTINCT_BOUND {
        short.push(format!(
            "{} distinct bound occurrences, under {MIN_DISTINCT_BOUND}",
            bound.len()
        ));
    }
    let value = json!({
        "seed": format!("{:#x}", config.seed),
        "n_per_par": config.n,
        "shards": config.shards,
        "per_par": pars.iter().map(|(par, counts)| (par.clone(), json!({
            "cases": counts.outcomes,
            "unground": counts.unground,
            "families": counts.families.iter().map(|(f, x)| ((*f).to_owned(), json!({
                "drawn": x.drawn,
                "applied": x.applied,
                "verified": x.verified,
                "noop": x.noop,
                "filtered": x.filtered,
                "filtered_qc_type_pre_dip3": x.filtered_qc,
            }))).collect::<BTreeMap<_, _>>(),
        }))).collect::<BTreeMap<_, _>>(),
        "bound": bound,
        "binder": binder,
        "distinct_bound": bound.len(),
        "minimums": {
            "n_per_par": MIN_N,
            "min_verified_per_family": MIN_VERIFIED,
            "min_distinct_bound": MIN_DISTINCT_BOUND,
            "met": short.is_empty(),
            "short": short,
        },
    });
    (value, short.is_empty())
}

/// The M3 claim's twin cases (O10 plan §3.1): a twin passes when every leg is done and ran every
/// trial of the twin and of its reject control. Each trial's answer is self-checked against its
/// expectation (accepted in Submit, Proposal and Header; the control refused) and V's reply is
/// dashd's (P1), so a leg that is done has both.
pub fn twin_cases(legs: &[(Value, bool)]) -> BTreeMap<String, &'static str> {
    let ran = |op: &str| ran_every_trial(legs, op);
    let done = !legs.is_empty() && legs.iter().all(|(_, d)| *d);
    TWIN_CONTROLS
        .iter()
        .map(|(twin, control)| {
            let pass = done && ran(twin) && ran(control);
            (
                (*twin).to_owned(),
                if pass {
                    "pass"
                } else {
                    "fail"
                },
            )
        })
        .collect()
}

/// Whether the legs list a trial of `op` and every one of them ran.
fn ran_every_trial(legs: &[(Value, bool)], op: &str) -> bool {
    let prefix = format!("{op}.");
    let mut ids = legs
        .iter()
        .flat_map(|(l, _)| l["manifest"].as_array().cloned().unwrap_or_default())
        .filter(|m| m["trial_id"].as_str().is_some_and(|t| t.starts_with(&prefix)))
        .peekable();
    ids.peek().is_some() && ids.all(|m| m["status"] == "ran")
}

/// DEC-1059's pinned cases ([`PINNED`]): a pinned case passes when every leg is done and ran its
/// proposal, which the leg self-checks against the pinned verdict. Not a census case: the run's
/// completeness requires it.
pub fn pinned_cases(legs: &[(Value, bool)]) -> BTreeMap<String, &'static str> {
    let done = !legs.is_empty() && legs.iter().all(|(_, d)| *d);
    PINNED
        .iter()
        .map(|op| {
            (
                (*op).to_owned(),
                if done && ran_every_trial(legs, op) {
                    "pass"
                } else {
                    "fail"
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::chain::mutate::{Par, Site};
    use crate::l3::inventory::Effects;
    use crate::l3::trial::{CaseId, Kind};

    const KEY: &str = "validation.cpp:ContextualCheckBlockHeader#4:time-too-new";

    fn trial(kind: Kind, op: usize, v_class: Option<&str>) -> TrialRecord {
        TrialRecord {
            trial: TrialId {
                case: CaseId {
                    op: "time_too_new",
                    variant: "reg",
                    site: Site::Reg,
                    par: Par::One,
                },
                kind,
            },
            hash: [0; 32],
            op,
            mode: kind.mode(),
            reply: Ok(Value::from("time-too-new")),
            binding: Some(Binding::Bound(KEY.into())),
            bound_class: Some("B_TIME_FUTURE".into()),
            v_class: v_class.map(str::to_owned),
            effects: Effects::NONE,
            evo: Value::Null,
            declined: Vec::new(),
            removed: Vec::new(),
            gained: Vec::new(),
        }
    }

    fn log(trials: Vec<TrialRecord>) -> LegLog {
        LegLog {
            name: "par1".into(),
            set: crate::l3::leg::Set::Tier1,
            par: Par::One,
            chain_args: Vec::new(),
            dashd_argv: Vec::new(),
            ops: Vec::new(),
            trials,
            load_refused: None,
            evo_counts: BTreeMap::new(),
            recorded: Vec::new(),
            fixture: None,
            av: None,
            seconds: 0.0,
            rpc_timing: BTreeMap::new(),
            v_steps: (0, 0.0),
            error: None,
            p8: Vec::new(),
            shard: None,
            generated: Vec::new(),
        }
    }

    fn core(class: &str) -> Replayed {
        Replayed {
            runtime: BTreeMap::from([(0, Some(class.to_owned()))]),
            ..Replayed::default()
        }
    }

    /// M-1: a proposal's class is the bound row's on V, or the leg fails; `site-only` only
    /// where no submit gave the occurrence a runtime class.
    #[test]
    fn a_proposal_class_divergence_fails() {
        let cold = trial(Kind::Cold, 0, Some("B_TIME_FUTURE"));
        let prop = |v| trial(Kind::Prop, 1, v);
        let both = log(vec![cold.clone(), prop(Some("B_TIME_FUTURE"))]);
        assert_eq!(
            classes(&both, &core("B_TIME_FUTURE")),
            Ok(vec![Class::Runtime, Class::Transferred])
        );
        let alone = log(vec![prop(Some("B_TIME_FUTURE"))]);
        assert_eq!(classes(&alone, &Replayed::default()), Ok(vec![Class::SiteOnly]));
        // The review's probe: V's TBV class differs from the bound row's.
        let mismatch = log(vec![cold.clone(), prop(Some("B_INVALID_HEADER"))]);
        let e = classes(&mismatch, &core("B_TIME_FUTURE")).expect_err("a class divergence");
        assert!(e.contains("P4 diverges for time_too_new.reg@Reg/par1+prop"), "{e}");
        // V's TBV reached no invalid state.
        let none = log(vec![cold, prop(None)]);
        assert!(classes(&none, &core("B_TIME_FUTURE")).is_err());
        // Without a runtime submit, too.
        assert!(classes(&log(vec![prop(Some("B_INVALID_HEADER"))]), &Replayed::default()).is_err());
    }

    /// A header trial signals no BlockChecked on either side: bound, it is site-only; a class
    /// on one side only, or one that is not the bound row's, is a divergence.
    #[test]
    fn a_header_trial_is_site_only_and_never_classed() {
        let header = |v| trial(Kind::Header, 0, v);
        assert_eq!(
            classes(&log(vec![header(None)]), &Replayed::default()),
            Ok(vec![Class::SiteOnly])
        );
        let none = Replayed {
            runtime: BTreeMap::from([(0, None)]),
            ..Replayed::default()
        };
        assert_eq!(classes(&log(vec![header(None)]), &none), Ok(vec![Class::SiteOnly]));
        assert!(classes(&log(vec![header(Some("B_TIME_FUTURE"))]), &none).is_err());
        assert!(classes(&log(vec![header(None)]), &core("B_TIME_FUTURE")).is_err());
        // OD-L3-24: even both sides agreeing is a divergence; no Core class exists for it.
        assert!(
            classes(&log(vec![header(Some("B_TIME_FUTURE"))]), &core("B_TIME_FUTURE")).is_err()
        );
    }

    /// Group M's excluded rows are in an unfiltered manifest of a set that runs group M, and
    /// in no other: the o10-pairs set has none, so its §5.3 completeness is its own rows.
    #[test]
    fn group_m_exclusions_are_only_in_group_m_sets() {
        let excluded = |rows: Vec<Value>| rows.iter().filter(|m| m["status"] == "excluded").count();
        assert_eq!(excluded(manifest(&log(Vec::new()), None)), EXCLUDED.len());
        assert_eq!(excluded(manifest(&log(Vec::new()), Some("m_x"))), 0);
        let o10 = LegLog {
            set: crate::l3::leg::Set::O10Pairs,
            ..log(Vec::new())
        };
        let rows = manifest(&o10, None);
        assert!(!rows.is_empty());
        assert_eq!(excluded(rows), 0);
    }

    /// §3.4's histograms and minimums, from the legs' generated records: a family is verified
    /// for a case only where every trial checked it; drawn, applied, no-op and filtered count
    /// every draw, the passed-over ones included; bound occurrences are counted by (mode, par).
    #[test]
    fn the_generator_summary_counts_and_gates_on_the_minimums() {
        let config = generator::Config {
            seed: 7,
            n: MIN_N,
            shards: 2,
        };
        let checked =
            |fams: &[&str]| json!(fams.iter().map(|f| json!({"family": f})).collect::<Vec<_>>());
        // Every family verified MIN_VERIFIED times at each par, one case per family and par.
        let leg = |name: &str, par: &str, keys: usize| {
            let mut generated = Vec::new();
            for f in FAMILIES {
                for _ in 0..MIN_VERIFIED {
                    generated.push(json!({"outcome": "picked", "drawn": [f], "unground": false,
                        "trials": [{"checked": checked(&[f])}, {"checked": checked(&[f])}],
                        "tried": []}));
                }
            }
            let trials: Vec<Value> = (0..keys)
                .map(|k| {
                    json!({"trial_id": format!("o10.gen.7.{k}@Reg/{par}+cold"),
                                "occurrence": {"bound": format!("k{k}")}})
                })
                .collect();
            (json!({"name": name, "generated": generated, "trials": trials}), true)
        };
        let legs = vec![leg("par1.s0", "par1", MIN_DISTINCT_BOUND), leg("par4.s1", "par4", 0)];
        let (value, met) = generated_summary(&legs, &config);
        assert!(met, "{value}");
        let fam = &value["per_par"]["par1"]["families"]["tx.value"];
        assert_eq!(
            (fam["drawn"].as_u64(), fam["verified"].as_u64()),
            (Some(MIN_VERIFIED), Some(MIN_VERIFIED))
        );
        assert_eq!(value["bound"]["k0"]["Submit/par1"], 1);
        assert_eq!(value["distinct_bound"], MIN_DISTINCT_BOUND);
        // One trial missing a family's check: that case does not verify it, so par1 falls short.
        let mut short = legs.clone();
        short[0].0["generated"][0]["trials"][1]["checked"] = json!([]);
        let (value, met) = generated_summary(&short, &config);
        assert!(!met);
        assert_eq!(
            value["per_par"]["par1"]["families"]["hdr.version"]["verified"],
            MIN_VERIFIED - 1
        );
        // Passed-over draws are reported as sampling losses, never as verified.
        let mut lost = legs.clone();
        lost[0].0["generated"][0] = json!({"outcome": "noop", "tried": [
            {"drawn": ["hdr.version"], "mutations": ["hdr.version"], "reason": "noop: the base's bytes"},
            {"drawn": ["hdr.version"], "mutations": [], "reason": "filtered: x"},
            {"drawn": ["hdr.version"], "mutations": ["hdr.version"],
             "reason": format!("filtered: {}: tx 1", generator::QC_TYPE_PRE_DIP3)}]});
        let (value, met) = generated_summary(&lost, &config);
        let v = &value["per_par"]["par1"]["families"]["hdr.version"];
        assert!(!met);
        assert_eq!(
            (v["noop"].as_u64(), v["filtered"].as_u64(), v["verified"].as_u64()),
            (Some(1), Some(2), Some(MIN_VERIFIED - 1))
        );
        // DEC-1059's refusals are counted apart, inside `filtered`.
        assert_eq!(v["filtered_qc_type_pre_dip3"], 1);
        assert_eq!(value["per_par"]["par1"]["cases"]["noop"], 1);
        // Too few distinct occurrences, a missing par, or a small N: not met.
        assert!(!generated_summary(&[leg("par1.s0", "par1", 24), legs[1].clone()], &config).1);
        assert!(!generated_summary(&legs[..1], &config).1);
        let small = generator::Config {
            n: 40,
            ..config
        };
        assert!(!generated_summary(&legs, &small).1);
    }

    /// OD-L3-64 (m1): the binder's outcomes over the generated trials are counted per (mode,
    /// par), ambiguous ones with their keys, so a binding that earns no credit is visible; other
    /// operators' trials are not counted.
    #[test]
    fn the_generator_summary_counts_binder_outcomes() {
        let config = generator::Config {
            seed: 7,
            n: MIN_N,
            shards: 1,
        };
        let (ctx, conn) = (
            "validation.cpp:ContextualCheckBlock#2:bad-txns-nonfinal",
            "validation.cpp:ConnectBlock#5:bad-txns-nonfinal",
        );
        let trial = |k: usize, kind: &str, occurrence: Value, reply: Value| {
            json!({"trial_id": format!("o10.gen.7.{k}@Reg/par1+{kind}"),
                   "occurrence": occurrence, "reply": reply})
        };
        let nonfinal = json!({"result": "bad-txns-nonfinal"});
        let trials = vec![
            trial(0, "cold", json!({"bound": conn}), nonfinal.clone()),
            trial(1, "cold", json!({"ambiguous": [ctx, conn]}), nonfinal.clone()),
            trial(1, "prop", json!({"ambiguous": [ctx, conn]}), nonfinal.clone()),
            trial(2, "cold", json!({"ambiguous": [ctx, conn]}), nonfinal),
            trial(3, "cold", Value::Null, json!({"result": null})),
            trial(4, "prop", Value::Null, json!({"result": "inconclusive-not-best-prevblk"})),
            trial(5, "cold", Value::Null, json!({"error": {"code": -22, "message": "x"}})),
            trial(6, "cold", json!("unbound"), json!({"result": "bad-x"})),
            json!({"trial_id": "time_at_mtp.reg@Reg/par1+cold", "occurrence": {"bound": ctx},
                   "reply": {"result": "time-too-old"}}),
        ];
        let legs = [(json!({"name": "par1.s0", "generated": [], "trials": trials}), true)];
        let (value, _) = generated_summary(&legs, &config);
        let submit = &value["binder"]["Submit/par1"];
        assert_eq!(submit["bound"], 1);
        assert_eq!(submit["ambiguous"], 2);
        assert_eq!(submit["ambiguous_keys"][format!("{ctx} | {conn}")], 2);
        assert_eq!(submit["accepted"], 1);
        assert_eq!(submit["rpc-error"], 1);
        assert_eq!(submit["unbound"], 1);
        let proposal = &value["binder"]["Proposal/par1"];
        assert_eq!(proposal["ambiguous"], 1);
        assert_eq!(proposal["rpc-answer"], 1);
        assert_eq!(proposal["bound"], Value::Null, "{proposal}");
        assert_eq!(value["bound"][conn]["Submit/par1"], 1);
        assert_eq!(value["bound"][ctx], Value::Null, "another operator's trial counted");
    }

    /// A sharded leg's manifest is its shard's cases: the generated `n ≡ k`, the twins on shard
    /// 0 only, so the shards' manifests partition the set's.
    #[test]
    fn a_sharded_legs_manifest_is_its_shards_cases() {
        let ids = |shard| {
            let o10 = LegLog {
                set: Set::O10Gen,
                shard,
                ..log(Vec::new())
            };
            manifest(&o10, None)
                .into_iter()
                .map(|m| m["trial_id"].as_str().expect("an id").to_owned())
                .collect::<Vec<_>>()
        };
        let all = ids(None);
        let (s0, s1) = (ids(Some((0, 2))), ids(Some((1, 2))));
        assert!(!s1.is_empty() && s1.iter().all(|t| t.starts_with("o10.gen.")));
        assert!(s0.iter().any(|t| t.starts_with("o10.time_mtp_edge.")));
        let mut both: Vec<String> = s0.into_iter().chain(s1).collect();
        both.sort();
        let mut all = all;
        all.sort();
        assert_eq!(both, all, "the shards partition the set");
        // An o10-gen manifest runs no group M, so it lists no excluded rows.
        let o10 = LegLog {
            set: Set::O10Gen,
            ..log(Vec::new())
        };
        assert!(manifest(&o10, None).iter().all(|m| m["status"] == "not run"));
    }

    /// DEC-1059: a pinned case passes only when every leg is done and ran its proposal.
    #[test]
    fn a_pinned_case_passes_only_when_every_leg_ran_it() {
        let leg = |status: &str| {
            json!({"manifest": [
                {"trial_id": "o10.qc_type_pre_dip3.reg@Reg/par1+prop", "status": status},
                {"trial_id": "o10.time_mtp_edge.reg@Reg/par1+cold", "status": "ran"},
            ]})
        };
        let state = |legs: &[(Value, bool)]| pinned_cases(legs)["o10.qc_type_pre_dip3"];
        assert_eq!(state(&[(leg("ran"), true), (leg("ran"), true)]), "pass");
        assert_eq!(state(&[(leg("ran"), true), (leg("skipped"), true)]), "fail");
        assert_eq!(state(&[(leg("ran"), true), (leg("ran"), false)]), "fail");
        assert_eq!(state(&[(json!({"manifest": []}), true)]), "fail");
        assert_eq!(state(&[]), "fail");
    }

    /// OD-L3-55: a generated case that ended in a typed terminal no-op or refusal, every draw
    /// recorded, completed with no trial to run: its trials are `sampled`, not `not run`, and
    /// the run can be complete with the minimums met. Without its outcome record (or with a
    /// draw missing, or a picked case's trial missing) it is `not run`, so incomplete.
    #[test]
    fn a_terminal_sampling_outcome_completes_its_trials() {
        let config = generator::config().as_ref().expect("the generator's settings");
        let case = |n: usize| crate::l3::trial::CaseId {
            op: generator::GEN_OPERATOR.name,
            variant: generator::variants()[n].as_str(),
            site: crate::chain::mutate::Site::Reg,
            par: Par::One,
        };
        let tried = |reasons: &[&str]| {
            reasons
                .iter()
                .zip(0..)
                .map(|(reason, r)| generator::Tried {
                    r,
                    drawn: vec!["tx.version"],
                    mutations: vec!["tx.version"],
                    reason: (*reason).to_owned(),
                })
                .collect::<Vec<_>>()
        };
        let record = |n: usize, outcome: &'static str, tried| GenRecord {
            case: case(n),
            outcome,
            r: None,
            drawn: Vec::new(),
            trials: Vec::new(),
            unground: false,
            tried,
        };
        let noop = tried(&[
            "noop: a known hash 00",
            "filtered: x",
            "noop: the base's bytes",
            "noop: a known hash 01",
        ]);
        let filtered = tried(&["filtered: a", "filtered: b", "filtered: c", "filtered: d"]);
        let o10 = LegLog {
            set: Set::O10Gen,
            generated: vec![
                record(0, "noop", noop.clone()),
                record(1, "filtered", filtered.clone()),
                // A draw missing, and a filtered case with a no-op: not terminal outcomes.
                record(2, "noop", noop[..3].to_vec()),
                record(3, "filtered", noop.clone()),
            ],
            ..log(Vec::new())
        };
        let rows = manifest(&o10, None);
        let status = |n: usize| -> Vec<String> {
            let prefix = format!("{}+", case(n));
            rows.iter()
                .filter(|m| m["trial_id"].as_str().is_some_and(|t| t.starts_with(&prefix)))
                .map(|m| m["status"].as_str().expect("a status").to_owned())
                .collect()
        };
        assert_eq!(status(0), ["sampled", "sampled"]);
        assert_eq!(status(1), ["sampled", "sampled"]);
        assert_eq!(status(2), ["not run", "not run"]);
        assert_eq!(status(3), ["not run", "not run"]);
        assert_eq!(status(4), ["not run", "not run"], "no outcome record");
        assert!(
            rows.iter().any(|m| m["outcome"] == "noop")
                && rows.iter().any(|m| m["outcome"] == "filtered")
        );
        // The run's completeness over the two terminal cases: every other trial ran.
        let leg =
            |rows: Vec<Value>| (json!({"name": "par1.s0", "only": null, "manifest": rows}), true);
        let terminal: Vec<Value> = rows
            .iter()
            .filter(|m| {
                [0, 1].iter().any(|n| {
                    m["trial_id"].as_str().is_some_and(|t| t.starts_with(&format!("{}+", case(*n))))
                })
            })
            .cloned()
            .collect();
        let others =
            vec![json!({"trial_id": "o10.time_mtp_edge.reg@Reg/par1+cold", "status": "ran"})];
        let complete = [terminal.clone(), others.clone()].concat();
        assert!(trials_complete(&[leg(complete.clone())], &complete));
        // The same with the outcome record missing: its trials are not run.
        let missing = LegLog {
            set: Set::O10Gen,
            generated: vec![record(1, "filtered", filtered)],
            ..log(Vec::new())
        };
        let rows = manifest(&missing, None);
        let lost: Vec<Value> = rows
            .iter()
            .filter(|m| {
                m["trial_id"].as_str().is_some_and(|t| t.starts_with(&format!("{}+", case(0))))
            })
            .cloned()
            .collect();
        assert!(lost.iter().all(|m| m["status"] == "not run"));
        let incomplete = [lost, others].concat();
        assert!(!trials_complete(&[leg(incomplete.clone())], &incomplete));
        // A leg not done is incomplete whatever its manifest.
        assert!(!trials_complete(&[(leg(complete.clone()).0, false)], &complete));
        // With the minimums met around the two terminal cases, the run is complete: they add
        // sampling losses, no verified coverage, and no shortfall.
        let summary_leg = |name: &str, par: &str, keys: usize, terminal: &[GenRecord]| {
            let mut generated: Vec<Value> = terminal.iter().map(generated_json).collect();
            for f in FAMILIES {
                for _ in 0..MIN_VERIFIED {
                    generated.push(json!({"outcome": "picked", "drawn": [f], "unground": false,
                        "trials": [{"checked": [{"family": f}]}, {"checked": [{"family": f}]}],
                        "tried": []}));
                }
            }
            let trials: Vec<Value> = (0..keys)
                .map(|k| {
                    json!({"trial_id": format!("o10.gen.7.{k}@Reg/{par}+cold"),
                                 "occurrence": {"bound": format!("k{k}")}})
                })
                .collect();
            (json!({"name": name, "generated": generated, "trials": trials}), true)
        };
        let terminal = [
            record(0, "noop", noop),
            record(
                1,
                "filtered",
                tried(&["filtered: a", "filtered: b", "filtered: c", "filtered: d"]),
            ),
        ];
        let summary = generator::Config {
            n: MIN_N,
            ..*config
        };
        let (value, met) = generated_summary(
            &[
                summary_leg("par1.s0", "par1", MIN_DISTINCT_BOUND, &terminal),
                summary_leg("par4.s0", "par4", 0, &[]),
            ],
            &summary,
        );
        assert!(met, "{value}");
        assert_eq!(value["per_par"]["par1"]["cases"]["noop"], 1);
        assert_eq!(value["per_par"]["par1"]["cases"]["filtered"], 1);
        assert!(met && trials_complete(&[leg(complete.clone())], &complete));
    }
}
