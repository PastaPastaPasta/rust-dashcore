//! The E-evo keys (plan §3.6, m-9): V's counters (`dashd_chainstate::evo::gates`) count every
//! one, the EV1 seam's six and, since BL-564, the five `evo/node.rs` keys. A proposal's entries
//! count each key's `p.` mirror instead, at the same point (OD-L3-43), so both modes are counted.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

/// The registry: every fixture E-evo key, each counted by V in both modes.
pub const EVO_KEYS: &[&str] = &[
    "w.stx", "w.cbtx", "w.quorum", "w.mnhf", "g.stx", "g.coll", "g.cbtx", "g.quorum", "g.dmn",
    "g.mnhf",
];

/// The five keys `evo/node.rs` serves (OD-L3-18), counted since BL-564.
pub const NODE_RS_KEYS: [&str; 5] = ["w.quorum", "w.mnhf", "g.quorum", "g.dmn", "g.mnhf"];

/// A proposal's mirror of a key (OD-L3-43).
pub const PROPOSAL: &str = "p.";

/// The report's list of [`NODE_RS_KEYS`]: counted on a submit and, as its `p.` mirror, on a
/// proposal (plan §3.6, OD-L3-18, OD-L3-43).
pub fn node_rs_keys() -> Value {
    let keys: Vec<Value> = NODE_RS_KEYS
        .iter()
        .map(|key| {
            let state = if EVO_KEYS.contains(key) {
                "counted"
            } else {
                "missing"
            };
            json!({
                "key": key,
                "Submit": state,
                "Proposal": state,
                "mirror": format!("{PROPOSAL}{key}"),
                "site": site(key),
            })
        })
        .collect();
    Value::Array(keys)
}

/// m-9's rules over a registry, the fixture's E-evo keys and V's counters: the registry is the
/// fixture's keys, V counts each and its `p.` mirror, and every counter V has is a registry key
/// or a registry key's mirror (`d.` keys and their mirrors are diagnostics, not E-evo keys).
pub fn check(
    registry: &[&str],
    fixture: &BTreeSet<String>,
    counters: &[&str],
) -> Result<(), String> {
    let keys: BTreeSet<String> = registry.iter().map(|k| (*k).to_owned()).collect();
    if keys.len() != registry.len() {
        return Err("a key twice in the registry".into());
    }
    if &keys != fixture {
        let unknown: Vec<_> = keys.difference(fixture).collect();
        let missing: Vec<_> = fixture.difference(&keys).collect();
        return Err(format!("registry vs fixture: unknown {unknown:?}, missing {missing:?}"));
    }
    for key in registry {
        let mirror = format!("{PROPOSAL}{key}");
        for counter in [*key, mirror.as_str()] {
            if !counters.contains(&counter) {
                return Err(format!("{key} is counted, but V has no counter {counter}"));
            }
        }
    }
    for counter in counters {
        let twin = counter.strip_prefix(PROPOSAL).unwrap_or(counter);
        if !twin.starts_with("d.") && !registry.contains(&twin) {
            return Err(format!("V counts {counter}, which no registry key counts"));
        }
    }
    Ok(())
}

/// V's site of a gated key or its mirror (§3.6's table), for a failure's message.
pub fn site(key: &str) -> &'static str {
    match key.strip_prefix(PROPOSAL).unwrap_or(key) {
        "g.stx" => "dashd-chainstate evo/ev1.rs DmlChecks::check_tx, the DIP3-gated special body",
        "g.coll" => "dashd-chainstate evo/ev1.rs, the V24 shared-collateral checks (template, H8)",
        "g.cbtx" => "dashd-chainstate evo/ev1.rs DmlChecks::check_cbtx with a decoded CbTx",
        "g.quorum" => {
            "dashd-chainstate evo/node.rs NodeEvo, H4's step 6, the quorum block processor (BL-564)"
        }
        "g.dmn" => {
            "dashd-chainstate evo/node.rs NodeEvo, H4's step 7, the masternode list (BL-564)"
        }
        "g.mnhf" => "dashd-chainstate evo/node.rs NodeEvo, H4's step 11, the MNHF signals (BL-564)",
        _ => "no V site",
    }
}

/// The gated counters V reports nonzero, a proposal's `p.g.` mirrors included (OD-L3-43).
pub fn entered(counts: &BTreeMap<&'static str, u64>) -> Vec<(&'static str, u64)> {
    let gated = |k: &str| k.strip_prefix(PROPOSAL).unwrap_or(k).starts_with("g.");
    counts.iter().filter(|(k, n)| gated(k) && **n > 0).map(|(k, n)| (*k, *n)).collect()
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

/// Each registry key's count and its `p.` mirror's (null where V reported none).
pub fn report(registry: &[&str], counts: &BTreeMap<&'static str, u64>) -> Value {
    let keys: serde_json::Map<String, Value> = registry
        .iter()
        .flat_map(|k| [(*k).to_owned(), format!("{PROPOSAL}{k}")])
        .map(|k| {
            let n = counts.get(k.as_str()).copied();
            (k, json!(n))
        })
        .collect();
    Value::Object(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l3::inventory::Inventory;

    const COUNTERS: [&str; 22] = [
        "d.stx_hook",
        "g.coll",
        "w.stx",
        "g.stx",
        "w.cbtx",
        "g.cbtx",
        "w.quorum",
        "g.quorum",
        "g.dmn",
        "w.mnhf",
        "g.mnhf",
        "p.d.stx_hook",
        "p.g.coll",
        "p.w.stx",
        "p.g.stx",
        "p.w.cbtx",
        "p.g.cbtx",
        "p.w.quorum",
        "p.g.quorum",
        "p.g.dmn",
        "p.w.mnhf",
        "p.g.mnhf",
    ];

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
        let e = check(EVO_KEYS, &fixture(), &COUNTERS[..6]);
        assert!(e.is_err_and(|e| e.contains("w.stx is counted, but V has no counter p.w.stx")));
        let mut unknown = EVO_KEYS.to_vec();
        unknown.push("g.new");
        assert!(check(&unknown, &fixture(), &COUNTERS).is_err_and(|e| e.contains("unknown")));
        let missing = &EVO_KEYS[1..];
        assert!(check(missing, &fixture(), &COUNTERS).is_err_and(|e| e.contains("missing")));
        let twice = [EVO_KEYS, &["w.stx"]].concat();
        assert!(check(&twice, &fixture(), &COUNTERS).is_err_and(|e| e.contains("twice")));
        let e = check(EVO_KEYS, &fixture(), &[&COUNTERS[..], &["g.x"]].concat());
        assert!(e.is_err_and(|e| e.contains("V counts g.x")));
    }

    /// OD-L3-56: a proposal's `p.` counters bind to the registry as their twins do.
    #[test]
    fn a_proposal_mirror_binds_like_its_twin() {
        // A bound mirror passes: each `p.g.` key of V's list has its counted twin.
        assert!(COUNTERS.iter().any(|c| c.starts_with("p.g.")));
        assert_eq!(check(EVO_KEYS, &fixture(), &COUNTERS), Ok(()));
        // A `p.g.` count with no registry row fails.
        let e = check(EVO_KEYS, &fixture(), &[&COUNTERS[..], &["p.g.x"]].concat());
        assert!(e.is_err_and(|e| e.contains("V counts p.g.x, which no registry key counts")));
        // So does a mirror whose twin left the registry, though the twin is still counted.
        let registry: Vec<&str> = EVO_KEYS.iter().copied().filter(|k| *k != "g.dmn").collect();
        let rows: BTreeSet<String> = fixture().into_iter().filter(|k| k != "g.dmn").collect();
        let counters: Vec<&str> = COUNTERS.iter().copied().filter(|c| *c != "g.dmn").collect();
        let e = check(&registry, &rows, &counters);
        assert!(e.is_err_and(|e| e.contains("V counts p.g.dmn")));
        // A counted key whose mirror V lacks fails.
        let counters: Vec<&str> = COUNTERS.iter().copied().filter(|c| *c != "p.g.mnhf").collect();
        let e = check(EVO_KEYS, &fixture(), &counters);
        assert!(e.is_err_and(|e| e.contains("g.mnhf is counted, but V has no counter p.g.mnhf")));
        // `p.d.` is a diagnostic mirror, as `d.` is.
        assert!(COUNTERS.contains(&"p.d.stx_hook"));
    }

    #[test]
    fn the_report_counts_both_modes() {
        let counts = BTreeMap::from([("w.stx", 5), ("g.stx", 0), ("p.w.stx", 2)]);
        let r = report(&["w.stx", "g.stx"], &counts);
        assert_eq!(r, json!({"w.stx": 5, "p.w.stx": 2, "g.stx": 0, "p.g.stx": null}));
        assert_eq!(
            entered(&BTreeMap::from([("g.stx", 0), ("g.coll", 2), ("w.stx", 3)])),
            [("g.coll", 2)]
        );
        let live = BTreeMap::from([("g.stx", 0), ("g.cbtx", 0), ("w.stx", 4)]);
        assert_eq!(gate_check(&live, 7), Ok(()));
        let e = gate_check(&BTreeMap::from([("g.cbtx", 1), ("w.stx", 4)]), 7).expect_err("entered");
        assert!(e.contains("op 7") && e.contains("g.cbtx = 1 at dashd-chainstate"), "{e}");
        // A proposal's mirror (OD-L3-43) is a gated body entered too, at the same site.
        let p = BTreeMap::from([("p.g.cbtx", 1), ("p.w.stx", 4), ("g.cbtx", 0)]);
        let e = gate_check(&p, 8).expect_err("a proposal entered");
        assert!(e.contains("op 8") && e.contains("p.g.cbtx = 1 at dashd-chainstate"), "{e}");
        for key in EVO_KEYS {
            assert_eq!(key.starts_with("g."), site(key) != "no V site", "{key}");
            assert_eq!(site(key), site(&format!("p.{key}")), "{key}");
        }
    }

    /// OD-L3-18, OD-L3-56: the report lists the five node.rs keys, counted in both modes.
    #[test]
    fn the_report_lists_the_node_rs_keys() {
        let keys = node_rs_keys();
        let rows: Vec<(&str, &str, &str, &str)> = keys
            .as_array()
            .expect("a list")
            .iter()
            .map(|k| {
                let s = |f: &str| k[f].as_str().unwrap_or("");
                (s("key"), s("Submit"), s("Proposal"), s("mirror"))
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("w.quorum", "counted", "counted", "p.w.quorum"),
                ("w.mnhf", "counted", "counted", "p.w.mnhf"),
                ("g.quorum", "counted", "counted", "p.g.quorum"),
                ("g.dmn", "counted", "counted", "p.g.dmn"),
                ("g.mnhf", "counted", "counted", "p.g.mnhf"),
            ]
        );
    }
}
