//! The E-evo keys (plan §3.6, m-9): how a10-4 observes each one. V's counters
//! (`dashd_chainstate::evo::gates`) count the EV1 seam's keys; the five `evo/node.rs` keys wait
//! for BL-564, so three are derived from what the leg asserts per body and two are not observed.
//! A derived key is never reported as observed, and a key not observed never as 0.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// A derived key's rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Predicate {
    /// `extract_signals` finds no signal in a body, or the body is refused before ConnectBlock
    /// (dash-dml's `extract_signals`, mnhf.rs:77; Core's mh:210-224).
    MnhfSignals,
    /// DIP3 inactive at the body's parent on both sides (V enters the body only with a `view`,
    /// node.rs:462/484; Core's bp:218, st:966).
    Dip3Parent,
}

impl Predicate {
    pub fn name(self) -> &'static str {
        match self {
            Predicate::MnhfSignals => "mnhf_signals (extract_signals per body)",
            Predicate::Dip3Parent => "dip3_parent (getdeploymentinfo and V's post per body)",
        }
    }
}

/// How a key is observed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    Counted,
    Derived(Predicate),
    NotObserved,
}

/// The registry: one entry per fixture E-evo key.
pub const EVO_KEYS: &[(&str, Observation)] = &[
    ("w.stx", Observation::Counted),
    ("w.cbtx", Observation::Counted),
    ("w.quorum", Observation::NotObserved),
    ("w.mnhf", Observation::NotObserved),
    ("g.stx", Observation::Counted),
    ("g.coll", Observation::Counted),
    ("g.cbtx", Observation::Counted),
    ("g.quorum", Observation::Derived(Predicate::Dip3Parent)),
    ("g.dmn", Observation::Derived(Predicate::Dip3Parent)),
    ("g.mnhf", Observation::Derived(Predicate::MnhfSignals)),
];

/// The keys that may stay not observed until BL-564 (OD-L3-18).
const NOT_OBSERVED_ALLOWED: [&str; 2] = ["w.quorum", "w.mnhf"];

/// m-9's rules over a registry, the fixture's E-evo keys and V's counters (`d.` keys are
/// diagnostics, not E-evo keys).
pub fn check(
    registry: &[(&str, Observation)],
    fixture: &BTreeSet<String>,
    counters: &[&str],
) -> Result<(), String> {
    let keys: BTreeSet<String> = registry.iter().map(|(k, _)| (*k).to_owned()).collect();
    if keys.len() != registry.len() {
        return Err("a key twice in the registry".into());
    }
    if &keys != fixture {
        let unknown: Vec<_> = keys.difference(fixture).collect();
        let missing: Vec<_> = fixture.difference(&keys).collect();
        return Err(format!("registry vs fixture: unknown {unknown:?}, missing {missing:?}"));
    }
    for (key, observation) in registry {
        match observation {
            Observation::NotObserved if !NOT_OBSERVED_ALLOWED.contains(key) => {
                return Err(format!("{key} may not be not observed"));
            }
            Observation::Counted if !counters.contains(key) => {
                return Err(format!("{key} is counted, but V has no counter for it"));
            }
            _ => {}
        }
    }
    for counter in counters.iter().filter(|c| !c.starts_with("d.")) {
        if !registry.iter().any(|(k, o)| k == counter && *o == Observation::Counted) {
            return Err(format!("V counts {counter}, which no registry key counts"));
        }
    }
    Ok(())
}

/// V's site of a gated key (§3.6's table), for a failure's message.
pub fn site(key: &str) -> &'static str {
    match key {
        "g.stx" => "dashd-chainstate evo/ev1.rs DmlChecks::check_tx, the DIP3-gated special body",
        "g.coll" => "dashd-chainstate evo/ev1.rs, the V24 shared-collateral checks (template, H8)",
        "g.cbtx" => "dashd-chainstate evo/ev1.rs DmlChecks::check_cbtx with a decoded CbTx",
        "g.quorum" | "g.dmn" | "g.mnhf" => {
            "dashd-chainstate evo/node.rs NodeEvo (derived until BL-564)"
        }
        _ => "no V site",
    }
}

/// The gated counters V reports nonzero.
pub fn entered(counts: &BTreeMap<&'static str, u64>) -> Vec<(&'static str, u64)> {
    counts.iter().filter(|(k, n)| k.starts_with("g.") && **n > 0).map(|(k, n)| (*k, *n)).collect()
}

/// §3.6 after an op (`at`): no gated body entered on V, or the failure names each entered key
/// and V's site.
pub fn gate_check(counts: &BTreeMap<&'static str, u64>, at: usize) -> Result<(), String> {
    let entered = entered(counts);
    if entered.is_empty() {
        return Ok(());
    }
    let sites: Vec<String> =
        entered.iter().map(|(k, n)| format!("{k} = {n} at {}", site(k))).collect();
    Err(format!("§3.6: V entered gated bodies at op {at}: {}", sites.join("; ")))
}

/// Each key with its state: its count, `derived: <predicate>` or `not observed`.
pub fn report(registry: &[(&str, Observation)], counts: &BTreeMap<&'static str, u64>) -> Value {
    let keys: serde_json::Map<String, Value> = registry
        .iter()
        .map(|(k, o)| {
            let state = match o {
                Observation::Counted => json!(counts.get(k)),
                Observation::Derived(p) => json!(format!("derived: {}", p.name())),
                Observation::NotObserved => json!("not observed"),
            };
            ((*k).to_owned(), state)
        })
        .collect();
    Value::Object(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::inventory::Inventory;

    const COUNTERS: [&str; 6] = ["d.stx_hook", "g.coll", "w.stx", "g.stx", "w.cbtx", "g.cbtx"];

    fn fixture() -> BTreeSet<String> {
        Inventory::load().expect("the fixture").evo_keys
    }

    #[test]
    fn the_registry_matches_the_fixture_and_the_counters() {
        assert_eq!(check(EVO_KEYS, &fixture(), &COUNTERS), Ok(()));
        // V's own list of counters is this one.
        assert_eq!(dashd_chainstate::evo::gates::KEYS, COUNTERS);
    }

    /// m-9's negative witnesses.
    #[test]
    fn a_wrong_registry_fails() {
        let with = |key: &str, o: Observation| -> Vec<(&str, Observation)> {
            EVO_KEYS
                .iter()
                .map(|(k, old)| {
                    (
                        *k,
                        if *k == key {
                            o
                        } else {
                            *old
                        },
                    )
                })
                .collect()
        };
        let e = check(&with("g.stx", Observation::NotObserved), &fixture(), &COUNTERS);
        assert!(e.is_err_and(|e| e.contains("g.stx may not")));
        let e = check(&with("w.stx", Observation::NotObserved), &fixture(), &COUNTERS);
        assert!(e.is_err_and(|e| e.contains("w.stx may not")));
        let e = check(&with("g.quorum", Observation::Counted), &fixture(), &COUNTERS);
        assert!(e.is_err_and(|e| e.contains("no counter")));
        let mut unknown = EVO_KEYS.to_vec();
        unknown.push(("g.new", Observation::Counted));
        assert!(check(&unknown, &fixture(), &COUNTERS).is_err_and(|e| e.contains("unknown")));
        let missing = &EVO_KEYS[1..];
        assert!(check(missing, &fixture(), &COUNTERS).is_err_and(|e| e.contains("missing")));
        let e =
            check(EVO_KEYS, &fixture(), &["w.stx", "w.cbtx", "g.stx", "g.coll", "g.cbtx", "g.x"]);
        assert!(e.is_err_and(|e| e.contains("g.x")));
    }

    #[test]
    fn the_report_never_prints_zero_for_what_it_does_not_count() {
        let counts = BTreeMap::from([("w.stx", 5), ("g.stx", 0)]);
        let r = report(EVO_KEYS, &counts);
        assert_eq!(r["w.stx"], 5);
        assert_eq!(r["w.quorum"], "not observed");
        assert!(r["g.mnhf"].as_str().is_some_and(|s| s.starts_with("derived: mnhf_signals")));
        assert_eq!(
            entered(&BTreeMap::from([("g.stx", 0), ("g.coll", 2), ("w.stx", 3)])),
            [("g.coll", 2)]
        );
        let live = BTreeMap::from([("g.stx", 0), ("g.cbtx", 0), ("w.stx", 4)]);
        assert_eq!(gate_check(&live, 7), Ok(()));
        let e = gate_check(&BTreeMap::from([("g.cbtx", 1), ("w.stx", 4)]), 7).expect_err("entered");
        assert!(e.contains("op 7") && e.contains("g.cbtx = 1 at dashd-chainstate"), "{e}");
        for (key, observation) in EVO_KEYS {
            assert_eq!(key.starts_with("g."), site(key) != "no V site", "{key} {observation:?}");
        }
    }
}
