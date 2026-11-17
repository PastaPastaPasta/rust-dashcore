//! OD-L3-18's live witnesses (plan §9) against a real regtest dashd v24 and V
//! ([`node_l1::l3::live`]). `contrib/node/l3-tier1.sh` and the node-l1 nightly (`l3_live`) run
//! both. Each writes its report where its variable names a file (`L3_MNHF_REPORT`,
//! `L3_DIP3_REPORT`), stamped with the rdc head and the dashd; both callers set both variables
//! and fail closed on a missing report (BL-672 N3).

use std::path::Path;

use node_l1::l3::inventory::Binding;
use node_l1::l3::leg::av::Defect;
use node_l1::l3::{live, rdc_head, run_mnhf, version};

fn repo() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
}

/// The body for 1999 passes §3.6; Core's template body for 2000, on a parent both sides report
/// with `dip0003` active, fails the `dip3_parent` cross-check, naming `g.quorum` and `g.dmn`.
#[test]
fn a_body_on_a_dip3_parent_fails_the_cross_check() {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let dashd = version::resolve(repo(), scratch.path()).expect("the run's dashd (plan §3.1)");
    let w = live::dip3_parent(&dashd, scratch.path()).expect("the witness ran");
    println!("dip3 witness: {w:?}");
    if let Ok(path) = std::env::var("L3_DIP3_REPORT") {
        let report = serde_json::json!({
            "witness": "dip3_parent",
            "rdc_head": rdc_head(repo()),
            "dashd": {"tag": dashd.tag, "commit": dashd.commit, "sha256": dashd.sha256},
            "baseline": format!("{:?}", w.baseline),
            "dashd_active": w.dashd_active,
            "v_active": w.v_active,
            "at_dip3": format!("{:?}", w.at_dip3),
        });
        let text = serde_json::to_string_pretty(&report).expect("JSON");
        std::fs::write(&path, text).expect("the report is written");
    }
    assert_eq!(w.baseline, Ok(()), "the baseline at 1999");
    assert_eq!((w.dashd_active, w.v_active), (Some(true), Some(true)), "both sides at 1999");
    let e = w.at_dip3.expect_err("the body for 2000 fails §3.6");
    assert!(e.starts_with("§3.6: dip0003 at "), "{e}");
    assert!(e.contains("g.quorum, g.dmn (dip3_parent)"), "{e}");
}

/// The MNHF witness bodies (OD-L3-18, plan §9): a pre-DIP3 body with a Core-mined MNHF signal
/// is refused at st:228 on both sides, before step 11, so the `g.mnhf` cross-check passes; with
/// two signals for one bit the body records `extract_error(..)`, never zero signals. In a copy
/// with V mutant 29 (V's DIP3 gate passes an MNHF signal) this test fails on `g.mnhf`, naming
/// V's site. `L3_MNHF_REPORT` names a file for the report: `{rdc_head, dashd, legs}`.
#[test]
fn a_body_with_an_mnhf_signal_is_refused_before_step_11() {
    let scratch = tempfile::tempdir().expect("a temp dir");
    let (dashd, legs) = run_mnhf(repo(), scratch.path()).expect("the MNHF bodies ran");
    if let Ok(path) = std::env::var("L3_MNHF_REPORT") {
        let report = serde_json::json!({
            "rdc_head": rdc_head(repo()),
            "dashd": {"tag": dashd.tag, "commit": dashd.commit, "sha256": dashd.sha256},
            "legs": legs.iter().map(|(_, v, _)| v.clone()).collect::<Vec<_>>(),
        });
        let text = serde_json::to_string_pretty(&report).expect("JSON");
        std::fs::write(&path, text).expect("the report is written");
    }
    let st228 = "specialtxman.cpp:CheckSpecialTxInner#1:bad-tx-type-dip3-inactive";
    for ((log, _, done), defect) in legs.iter().zip(Defect::MNHF) {
        assert_eq!(log.error, None, "{}", log.name);
        assert!(done, "{} is done", log.name);
        let [trial] = log.trials.as_slice() else {
            panic!("{}: {} trials", log.name, log.trials.len());
        };
        println!("{}: {:?} {:?} {}", log.name, trial.reply, trial.binding, trial.evo);
        assert_eq!(trial.binding, Some(Binding::Bound(st228.into())), "{}", log.name);
        let signals = &trial.evo["mnhf_signals"];
        match defect {
            Defect::Signal => assert_eq!(signals, 1, "{}", log.name),
            _ => {
                let text = signals.as_str().unwrap_or_default();
                assert!(text.starts_with("extract_error("), "{}: {signals}", log.name);
                assert!(text.contains("bad-mnhf-duplicates-in-block"), "{}: {signals}", log.name);
            }
        }
    }
}
