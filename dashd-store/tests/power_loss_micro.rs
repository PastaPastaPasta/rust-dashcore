//! MicroLog (ST3a design §6.1): the PLAN's toy flush. The correct log (N0) is clean under an
//! exhaustive search of every crash image, both kinds, with and without faults; each planted
//! bug P1–P8 is found by its named invariant (witness search), and its checked-in repro replays.

use dashd_store::faulty::micro::{MicroBug, MicroConfig, MicroLog, MicroOp};
use dashd_store::faulty::{
    EventSel, Fault, FaultKind, FaultOutcome, FaultPlan, InvariantId, Mode, OpKind, Repro,
    Scenario, StoreSel, explore, replay,
};

fn ops() -> Vec<MicroOp> {
    "a300:1;a300:2;c;a300:3;a300:4;c;s;a200:5;c".split(';').map(|o| o.parse().unwrap()).collect()
}

fn scenario(bug: Option<MicroBug>) -> Scenario<MicroLog> {
    Scenario::new(
        "microlog",
        MicroConfig {
            bug,
            max_file: 1024,
        },
        ops(),
    )
}

fn sync_fault() -> Fault {
    Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::SyncData,
            n: 2,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::Applied,
    }
}

fn write_fault() -> Fault {
    Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::WriteAt,
            n: 1,
        },
        kind: FaultKind::NoSpace,
        // A short write: the record's first sector only.
        outcome: FaultOutcome::Partial(1),
    }
}

const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 1024,
};

#[test]
fn n0_correct_microlog_is_clean() {
    let mut sc = scenario(None);
    sc.min_images = 20;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("N0: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.images > 20 && r.branches > r.images, "{}", r.summary());
    let mut faulty = scenario(None);
    faulty.faults = FaultPlan(vec![sync_fault(), write_fault()]);
    let r = explore(&faulty, EXHAUSTIVE);
    assert!(r.is_clean(), "{}", r.summary());
    // A read fault in recovery: MicroLog stops with its sanctioned abort, and the fault fires.
    let mut sc = scenario(None);
    sc.recovery_faults = vec![Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::Read,
            n: 0,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    }];
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.is_clean(), "{}", r.summary());
}

/// N0 with a second snapshot replacing the first (the review's two-snapshot workload): the
/// rename over `snap.dat` keeps every referenced byte, so it is clean.
#[test]
fn n0_two_snapshots_is_clean() {
    let mut sc = scenario(None);
    sc.ops = "a300:1;a300:2;c;s;a300:3;c;s".split(';').map(|o| o.parse().unwrap()).collect();
    sc.min_images = 20;
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.is_clean(), "{}", r.summary());
}

#[test]
fn x1_syncs_only_the_newest_file() {
    witness(MicroBug::X1, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

#[test]
fn x2_snapshot_record_before_the_sync_dir() {
    witness(MicroBug::X2, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

#[test]
fn x3_recovery_trims_the_committed_tail() {
    witness(MicroBug::X3, vec![], &[InvariantId::R1, InvariantId::D1d]);
}

fn witness(bug: MicroBug, faults: Vec<Fault>, expect: &[InvariantId]) -> Repro {
    let mut sc = scenario(Some(bug));
    sc.faults = FaultPlan(faults);
    let r = explore(
        &sc,
        Mode::Witness {
            expect: expect.to_vec(),
            max_branches: 20_000,
        },
    );
    eprintln!("{bug:?}: {}", r.summary());
    assert!(r.found_all, "{bug:?}: expected {expect:?}\n{}", r.summary());
    let f = r.first(expect[0]).unwrap();
    let got = replay::<MicroLog>(&f.repro).unwrap();
    assert!(got.iter().any(|v| v.invariant == expect[0]), "{bug:?}: the repro replays");
    f.repro.clone()
}

#[test]
fn p1_commit_before_fsync() {
    witness(MicroBug::P1, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

#[test]
fn p2_index_before_data() {
    witness(MicroBug::P2, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

#[test]
fn p3_no_sync_dir_after_rollover() {
    witness(MicroBug::P3, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

#[test]
fn p4_buffered_commit_acknowledged() {
    witness(MicroBug::P4, vec![], &[InvariantId::R4]);
}

#[test]
fn p5_retries_failed_sync() {
    witness(MicroBug::P5, vec![sync_fault()], &[InvariantId::R1, InvariantId::R8]);
}

#[test]
fn p6_ignores_short_write() {
    witness(MicroBug::P6, vec![write_fault()], &[InvariantId::R8, InvariantId::R1]);
}

#[test]
fn p7_recovers_by_crc_scan() {
    witness(MicroBug::P7, vec![], &[InvariantId::R3]);
}

#[test]
fn p8_rename_without_sync() {
    witness(MicroBug::P8, vec![], &[InvariantId::R1, InvariantId::D1a]);
}

// ---------------------------------------------------------------------------------------------
// Checked-in repros (§4.8), M13's budget case and M14's round trip.

const REGRESS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/power_loss/regress");

fn bugs() -> Vec<(MicroBug, Vec<Fault>, Vec<InvariantId>)> {
    use InvariantId::*;
    vec![
        (MicroBug::P1, vec![], vec![R1]),
        (MicroBug::P2, vec![], vec![R1]),
        (MicroBug::P3, vec![], vec![R1]),
        (MicroBug::P4, vec![], vec![R4]),
        (MicroBug::P5, vec![sync_fault()], vec![R1]),
        (MicroBug::P6, vec![write_fault()], vec![R8]),
        (MicroBug::P7, vec![], vec![R3]),
        (MicroBug::P8, vec![], vec![R1]),
        (MicroBug::X1, vec![], vec![R1]),
        (MicroBug::X2, vec![], vec![R1]),
        (MicroBug::X3, vec![], vec![R1]),
    ]
}

/// Regenerates the checked-in repros: `cargo test -p dashd-store --features faulty --test
/// power_loss_micro -- --ignored regenerate`.
#[test]
#[ignore]
fn regenerate_micro_repros() {
    std::fs::create_dir_all(REGRESS).unwrap();
    for (bug, faults, expect) in bugs() {
        let found = witness(bug, faults, &expect);
        let m = dashd_store::faulty::minimize::<MicroLog>(&found, 2000);
        assert!(dashd_store::faulty::reproduces::<MicroLog>(&m.repro), "{bug:?}");
        let text = format!(
            "# MicroLog planted bug {bug:?} (ST3a §6.1), minimised in {} replays.\n{}",
            m.replays,
            m.repro.to_text()
        );
        std::fs::write(format!("{REGRESS}/{bug:?}.repro"), text).unwrap();
    }
}

#[test]
fn checked_in_micro_repros_replay() {
    for (bug, _, expect) in bugs() {
        let path = format!("{REGRESS}/{bug:?}.repro");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let r = Repro::parse(&text).unwrap();
        assert_eq!(r.expect, expect, "{bug:?}");
        // M14: the format round-trips.
        assert_eq!(Repro::parse(&r.to_text()).unwrap(), r);
        let got = replay::<MicroLog>(&r).unwrap();
        assert!(got.iter().any(|v| v.invariant == expect[0]), "{bug:?}: {got:?}");
    }
}

#[test]
fn m13_minimiser_budget_is_reported() {
    let found = witness(MicroBug::P1, vec![], &[InvariantId::R1]);
    let m = dashd_store::faulty::minimize::<MicroLog>(&found, 3);
    assert!(!m.minimality_verified, "a spent budget never claims 1-minimality");
    assert!(m.replays <= 3);
}

/// W4 (§6.5): granting ext4's bonus (`sync_data` of a new file also persists its directory
/// entry), P3 (no directory sync after creating the log) is no longer caught.
#[test]
fn w4_ext4_bonus_hides_p3() {
    let mut sc = scenario(Some(MicroBug::P3));
    sc.adversary = dashd_store::faulty::Adversary::Witness(dashd_store::faulty::WitnessSet {
        entry_on_file_sync: true,
        ..Default::default()
    });
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.is_clean(), "P3 under W4: {}", r.summary());
}
