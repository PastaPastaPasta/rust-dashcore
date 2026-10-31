//! The CRASH model's b3 suite (CH2 §6.4, §7.2's b3 row): ST3a's subscriber-bearing scenarios on
//! [`ChainSut`] with b3's test subscriber, a real `SubscriberDriver` over its own store, each
//! exhaustive with both crash kinds.

#[path = "crash/common.rs"]
mod common;
use common::*;
use dashd_chainstate::crash::chainstate_crash;
use dashd_chainstate::subscriber::PRUNE_VIOLATION;

/// The subscriber's locator commits in a trace.
fn locators(t: &Trace) -> usize {
    t.marks()
        .filter(|m| {
            matches!(m, Mark::Locator {
                store,
                ..
            } if *store == SUB_STORE)
        })
        .count()
}

/// The forward run commits the subscriber's locator, then every image is clean.
fn sub_clean(sc: &Sc) -> Trace {
    let t = forward(sc);
    assert!(locators(&t) > 0, "{}: the subscriber never committed", sc.name);
    clean(sc);
    t
}

#[test]
fn s1_synced_subscriber() {
    sub_clean(&s1_sub());
}

#[test]
fn s4_synced_subscriber() {
    sub_clean(&s4_sub());
}

#[test]
fn s4b_synced_subscriber() {
    sub_clean(&s4b_sub());
}

#[test]
fn s2_reorg_moves_the_prune_lock_back() {
    let t = sub_clean(&s2());
    assert!(!logs(&t, "prune lock moved back to").is_empty(), "S2: no moveback line");
}

#[test]
fn s3p_pruned_ibd() {
    sub_clean(&s3p());
}

#[test]
fn s11_syncing_lock_keeps_its_files() {
    sub_clean(&s11());
}

#[test]
fn s11r_syncing_across_a_reorg() {
    sub_clean(&s11r());
}

#[test]
fn s11s_cadence_commits_capped_at_the_durable_tip() {
    sub_clean(&s11s());
}

/// S11c: every image is clean, and a restart after the prune's unlink boots the re-enabled index
/// into the open's refusal.
#[test]
fn s11c_disabled_index_refuses_a_pruned_catch_up() {
    let sc = s11c();
    let t = sub_clean(&sc);
    assert_eq!(logs(&t, "toyindex disabled").len(), 1, "the disable");
    let mut r = Repro::new(&sc, None);
    r.crash_at = t.events().last().map(|(id, ..)| id.index);
    r.kind = CrashKind::ProcessKill;
    let (v, after) = replay_trace::<ChainSut>(&r).expect("replays");
    assert!(v.is_empty(), "{v:?}");
    assert_eq!(logs(&after, PRUNE_VIOLATION).len(), 1, "the refusal");
}

/// S11cW: the disable's record is durable before its lock goes, so W's prune right after the
/// deregistration leaves every image either with the lock or with the record (D36).
#[test]
fn s11c_w_prune_at_the_disables_boundary() {
    let sc = s11c_w();
    let t = sub_clean(&sc);
    assert_eq!(logs(&t, "toyindex disabled").len(), 1, "the disable");
    assert!(selections(&t).iter().any(|f| !f.is_empty()), "W pruned at the boundary");
}

#[test]
fn s11q_backlog_at_a_prune() {
    sub_clean(&s11q());
}

#[test]
fn s11d_dmax_cycle() {
    for b in [1, 2] {
        sub_clean(&s11d(b));
    }
}

#[test]
fn s13_activation_prune_after_a_crash() {
    sub_clean(&s13());
}

#[test]
fn s14_automatic_checks_and_a_manual_prune() {
    sub_clean(&s14());
}

#[test]
fn n9_s5_with_a_synced_subscriber() {
    sub_clean(&n9());
}

#[test]
fn s11o_open_after_a_backward_flush() {
    sub_clean(&s11o());
}

#[test]
fn s2b_invalidate_below_l() {
    sub_clean(&s2b(SubConfig::synced(0)));
    sub_clean(&s2b(SubConfig::syncing(80)));
}

/// `chainstate_crash`'s PR smoke (CH2 §6.5, D28): seeded workloads of §6.4's operation families,
/// half with the subscriber, each crashed once (p = 0.2 a fault, p = 0.1 nested), judged by every
/// invariant and FA1–FA6. `CHAINSTATE_CASES` and `CHAINSTATE_SEED` in the environment make it a
/// soak.
#[test]
fn chainstate_crash_smoke() {
    let var =
        |name, default| std::env::var(name).map_or(default, |v: String| v.parse().expect(name));
    let cases = var("CHAINSTATE_CASES", 150);
    let seed = var("CHAINSTATE_SEED", 0xc4a1);
    let r = dashd_store::faulty::sweep::<ChainSut>(cases, seed, &chainstate_crash);
    eprintln!("chainstate_crash: {}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.images * 10 >= cases * 9, "{}", r.summary());
}

/// Soak failures from fresh seeds (Opus-h r1), replayed clean: seed 81703's catch-up re-climbed
/// past a cadence point a stale-branch rewind had left above p (FA4, D51); seed 81707's drain
/// checkpointed after a rewind landed in step 6, which FA6's bound did not count (D52).
#[test]
fn soak_regressions_replay_clean() {
    for (name, was) in [("fa4-81703", FA4), ("fa6-81707", FA6)] {
        let path = format!("{}/tests/crash/regress/{name}.repro", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let r = Repro::parse(&text).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(r.expect[0], was, "{name}");
        let got = replay::<ChainSut>(&r).expect("a repro");
        assert!(got.is_empty(), "{name}: {got:?}");
    }
}

#[test]
#[ignore]
fn dump_marks() {
    let name = std::env::var("DUMP").unwrap_or_default();
    let mut sc = match name.as_str() {
        "S13" => s13(),
        "S14" => s14(),
        "S11" => s11(),
        "S1" => s1_sub(),
        "S11s" => s11s(),
        "S11q" => s11q(),
        "S11r" => s11r(),
        "S11d2" => s11d(2),
        _ => s11d(1),
    };
    if let Ok(p) = std::env::var("PLANT") {
        let mut line = sc.cfg.to_string();
        line.push_str(&format!(" planted={p}"));
        sc.cfg = line.parse().expect("a config");
    }
    let (t, v) = forward_trace(&sc);
    eprintln!("violations {v:?}");
    for e in &t.entries {
        if let TraceEntry::Mark {
            after_events,
            mark,
            ..
        } = e
        {
            let s = format!("{mark:?}");
            if !s.starts_with("Path")
                && !s.starts_with("Connect")
                && !s.starts_with("Durable")
                && !s.starts_with("SubscriberState { store: \"subscriber\", synced: false")
                && !s.contains("partial batch")
            {
                eprintln!("{after_events}: {}", &s[..s.len().min(160)]);
            }
        }
    }
}
