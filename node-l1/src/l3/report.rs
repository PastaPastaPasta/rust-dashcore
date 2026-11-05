//! A run's report (plan §3.4's contract with `contrib/node/l3-tier1.sh`): the dashd and source
//! it ran, every case's trials with their planes' results, and each leg's status.
//!
//! `{cases, manifest: [{trial_id, status}], legs: [{name, status, multi_candidate_up_posts}]}`
//! is what the runner reads; a leg is `done` only when it and its replay finished clean.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::evo;
use super::inventory::Binding;
use super::leg::{LegLog, Set, TrialRecord, cases};
use super::pairs::Role;
use super::pairset;
use super::planes;
use super::replay::Replayed;
use super::trial::{Kind, TrialId, hex};
use super::version::Dashd;
use crate::chain::mutate::{EXCLUDED, Mode, Par, label};

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

/// The leg's manifest rows: every selected trial, `ran` once its record exists, then (in an
/// unfiltered run of a set with group M) group M's excluded rows with their reasons.
fn manifest(log: &LegLog, only: Option<&str>) -> Vec<Value> {
    let ran: BTreeSet<String> = log.trials.iter().map(|t| t.trial.to_string()).collect();
    let group_m = only.is_none() && log.set != Set::O10Pairs;
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
        .filter(|(c, _)| only.is_none_or(|o| c.id.selected_by(o)))
        .flat_map(|(c, kinds)| kinds.into_iter().map(move |kind| TrialId { case: c.id, kind }.to_string()))
        .map(|id| json!({"trial_id": id, "status": if ran.contains(&id) { "ran" } else { "not run" }}))
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
    value
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
            error: None,
            p8: Vec::new(),
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
}
