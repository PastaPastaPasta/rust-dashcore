use super::*;
use crate::faulty::harness::{Mode, Nested, explore, replay, sweep};

const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 4096,
};

fn op(s: &str) -> SpvOp {
    s.parse().expect("an op")
}

fn ops(s: &str) -> Vec<SpvOp> {
    s.split_whitespace().map(op).collect()
}

fn scenario(recovery: Recovery, bug: Option<SpvBug>) -> Scenario<SpvSut> {
    Scenario::new(
        "spv",
        SpvConfig {
            recovery,
            bug,
        },
        ops(WORKLOAD),
    )
}

#[test]
fn ops_and_configs_round_trip() {
    for op in ops(&format!("{WORKLOAD} r0+1:0 m0 C")) {
        assert_eq!(op.to_string().parse(), Ok(op));
    }
    for bad in ["h0", "r1+0:1", "r1:1", "x3", "f", ""] {
        assert_eq!(bad.parse::<SpvOp>(), Err(()), "{bad}");
    }
    for cfg in ["engine=prefix bug=none", "engine=subset bug=B4"] {
        assert_eq!(cfg.parse::<SpvConfig>().map(|c| c.to_string()).as_deref(), Ok(cfg));
    }
}

#[test]
fn subscribers_follow_a_reorg_from_where_they_leave_the_chain() {
    let mut st = SpvState::default();
    for op in ops("h4 f9 m9 w2 r1+2:7") {
        st = st.step(op);
    }
    let chain = st.chain();
    assert_eq!(chain.len(), 4);
    // `f` and `m` are off the active chain, `w` is ahead of the fork on the old branch.
    let f = st.step(op("f0"));
    assert_eq!(f.loc(Sub::F), chain[1], "a step to below the fork stops at it");
    assert_eq!(f.rows[0], Sub::F.rows(&chain[..2]));
    let m = st.step(op("m9"));
    assert_eq!(m.loc(Sub::M), chain[3]);
    assert_eq!(m.rows[1].len(), 3, "one row per residue");
    assert_eq!(m.digests[2], Some(m.digest(2)));
    assert_eq!(st.step(op("w2")).loc(Sub::W), chain[2]);
    let on = f.step(op("f1"));
    assert_eq!(on, f, "a step to at most the locator on the active chain changes nothing");
}

/// The checks on hand-built states, for the clauses no planted bug isolates.
#[test]
fn the_checks_judge_hand_built_states() {
    let mut refm = SpvRef::new(&SpvConfig::default());
    for op in ops(WORKLOAD) {
        refm.apply(&op);
    }
    let last = refm.states.len() - 1;
    let end = refm.states[last].clone();
    assert_eq!(check_stores(&end), []);
    assert_eq!(check_recovered(&end, &refm, last), []);
    let found = |v: Vec<Violation>| v.iter().map(|v| v.invariant).collect::<Vec<_>>();
    // S1: a stored header whose parent is not, off the active chain.
    let mut orphan = end.clone();
    let stale = orphan.step(op("r1+2:9")).headers;
    let (&child, _) = stale
        .iter()
        .find(|(x, (h, _))| *h == 3 && !end.headers.contains_key(x))
        .expect("the branch's second");
    orphan.headers.insert(child, stale[&child]);
    orphan.digests[0] = Some(orphan.digest(0));
    assert_eq!(found(check_stores(&orphan)), [InvariantId::S1]);
    // S2: a locator at a stored header the workload never made (S3 sees the step is none either).
    let mut forged = end.clone();
    let tip = end.tip();
    forged.headers.insert(0xf0f0, (end.headers[&tip].0 + 1, tip));
    forged.locs[0] = Some(0xf0f0);
    assert_eq!(found(check_recovered(&forged, &refm, last)), [InvariantId::S2, InvariantId::S3]);
    // S3: the wallet back at the anchor though its locator was known at every step since the
    // acknowledged one.
    let mut rewound = end.clone();
    rewound.locs[2] = Some(ANCHOR);
    rewound.rows[2].clear();
    assert_eq!(found(check_recovered(&rewound, &refm, last)), [InvariantId::S3]);
    // Allowed when a locator it held since then is one the SPV store lost.
    rewound.headers.remove(&end.loc(Sub::W));
    rewound.active.clear();
    rewound.tip = None;
    let from = refm.states.iter().rposition(|r| r.loc(Sub::W) == end.loc(Sub::W)).expect("a step");
    let f = check_recovered(&rewound, &refm, from);
    assert!(!f.iter().any(|v| v.detail.starts_with("w's")), "{f:?}");
}

#[test]
fn the_correct_store_is_clean_on_prefix_engines() {
    let mut sc = scenario(Recovery::Prefix, None);
    sc.min_images = 20;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("clean: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.branches > r.images, "{}", r.summary());
}

/// Each planted bug is found, with the invariant expected, and its repro replays.
fn witness(recovery: Recovery, bug: Option<SpvBug>, expect: InvariantId) {
    let sc = scenario(recovery, bug);
    let r = explore(
        &sc,
        Mode::Witness {
            expect: vec![expect],
            max_branches: 50_000,
        },
    );
    assert!(r.found_all, "{bug:?} on {recovery:?}: {expect:?} not found: {}", r.summary());
    let f = r.first(expect).expect("found");
    let got = replay::<SpvSut>(&f.repro).expect("replays");
    assert!(got.iter().any(|v| v.invariant == expect), "{bug:?}: the repro gives {got:?}");
}

#[test]
fn planted_bugs_are_found() {
    use SpvBug::*;
    for (bug, expect) in [
        (B1, InvariantId::S2),
        (B2, InvariantId::S4),
        (B3, InvariantId::S3),
        (B4, InvariantId::S2),
        (B5, InvariantId::S3),
        (B6, InvariantId::S2),
        (B7, InvariantId::S3),
    ] {
        witness(Recovery::Prefix, Some(bug), expect);
    }
}

/// N1: on engines with subset recovery the correct store breaks: it relies on prefix
/// recovery, which SQLite gives (`sqlite_wal_prefix`).
#[test]
fn n1_the_correct_store_breaks_on_subset_engines() {
    witness(Recovery::Subset, None, InvariantId::S1);
}

/// Crashes during recovery too (the wallet's rewind is a Durable write): every interrupted
/// recovery converges (R7a).
#[test]
fn the_correct_store_is_clean_with_nested_crashes() {
    let mut sc = scenario(Recovery::Prefix, None);
    sc.nested = Nested::Depth1;
    let r = explore(&sc, EXHAUSTIVE);
    eprintln!("nested: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.converged > 0, "{}", r.summary());
}

/// The seeded sweep in the PR gate: 5,000 random workloads (about 2 CPU-s in debug, plan §5's
/// budget is 10), with crash points, kinds, faults and nested crashes. The nightly runs more
/// (`dashd-store`'s `power_loss_deep::spv_sweep`).
#[test]
fn sweep_is_clean() {
    let r = sweep(5_000, 0x005b_10c9, &random_scenario);
    eprintln!("sweep: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.images > 4_000, "{}", r.summary());
}
