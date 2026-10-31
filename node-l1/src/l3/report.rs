//! A run's report (plan §3.4's contract with `contrib/node/l3-tier1.sh`): the dashd and source
//! it ran, every case's trials with their planes' results, and each leg's status.
//!
//! `{cases, manifest: [{trial_id, status}], legs: [{name, status, multi_candidate_up_posts}]}`
//! is what the runner reads; a leg is `done` only when it and its replay finished clean.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::evo;
use super::inventory::Binding;
use super::leg::{LegLog, TrialRecord, cases};
use super::planes;
use super::replay::Replayed;
use super::trial::{TrialId, hex};
use super::version::Dashd;
use crate::chain::mutate::Mode;

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
        "multi_candidate_up_posts": log.multi_candidate_up_posts,
        "evo_keys": evo::report(evo::EVO_KEYS, &log.evo_counts),
        "multi_candidate_source": "the §3.7 model (V's candidates: track N's accessors; Core's: shim ABI 33)",
        // m-3: what P7 compares in a10-4.
        "p7": {
            "best_header": "V = the §3.7 model after every planed op; Core = V and Core = the model in the replay",
            "index_digest": "Core = V in the replay",
            "best_invalid": "not compared: the §3.7 model's only, until track N's NodeSide accessor (V) and the ABI 33 pin (Core)",
            "candidates": "not compared: the §3.7 model's only, until track N's NodeSide accessor (V) and the ABI 33 pin (Core)",
        },
        "replay": {
            "abi": replayed.abi,
            "error": replayed.error,
            "proposals": replayed.proposals,
            "proposal_checked": replayed.proposal_checked,
            "seconds": replayed.seconds,
        },
        "trials": trials,
        "manifest": manifest(log, only),
    });
    (value, done)
}

/// The leg's manifest rows: every selected trial, `ran` once its record exists.
fn manifest(log: &LegLog, only: Option<&str>) -> Vec<Value> {
    let ran: BTreeSet<String> = log.trials.iter().map(|t| t.trial.to_string()).collect();
    cases(log.par)
        .iter()
        .filter(|c| only.is_none_or(|o| o == c.id.op || o == c.id.to_string()))
        .flat_map(|c| c.kinds().iter().map(move |&kind| TrialId { case: c.id, kind }.to_string()))
        .map(|id| json!({"trial_id": id, "status": if ran.contains(&id) { "ran" } else { "not run" }}))
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
    json!({
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
        "cases": cases.len(),
        "manifest": manifest,
        "legs": legs.iter().map(|(l, _)| l.clone()).collect::<Vec<_>>(),
    })
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
        }
    }

    fn log(trials: Vec<TrialRecord>) -> LegLog {
        LegLog {
            name: "par1".into(),
            par: Par::One,
            chain_args: Vec::new(),
            dashd_argv: Vec::new(),
            ops: Vec::new(),
            trials,
            multi_candidate_up_posts: 0,
            load_refused: None,
            evo_counts: BTreeMap::new(),
            recorded: Vec::new(),
            seconds: 0.0,
            error: None,
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
}
