//! ST3a's nightly suite (design §8.4; release build): the clean runs §8.2 moves out of the PR
//! gate, full minimisation of the planted bugs' repros, and the deep components.
//!
//! `cargo test --release -p dashd-store --all-features --test power_loss_deep -- --ignored`

#[path = "power_loss/toy_common.rs"]
mod common;
use common::*;

// ---------------------------------------------------------------------------------------------
// Clean runs out of the PR gate (§8.2; DEC-101 Q5). Their preconditions and planted-bug
// witnesses stay in `power_loss_toy.rs`.

#[test]
#[ignore]
fn s5b_clean() {
    clean(&s5b(None));
}

#[test]
#[ignore]
fn s5c_clean() {
    clean(&s5c(None));
}

#[test]
#[ignore]
fn s11r_clean() {
    clean(&s11r(None));
}

#[test]
#[ignore]
fn s13_clean() {
    clean(&s13(None));
}

#[test]
#[ignore]
fn s14b_clean() {
    clean(&s14b(None));
}

/// S11c ends with the sanctioned refusal.
#[test]
#[ignore]
fn s11c_clean() {
    clean(&s11c());
}

// ---------------------------------------------------------------------------------------------
// Full minimisation (§8.4): regenerates the checked-in toy repros. Never run it together with
// `checked_in_toy_repros_replay`, which reads the files.

/// Regenerates the checked-in toy repros, minimised (nightly cost): `cargo test -p dashd-store
/// --all-features --test power_loss_deep -- --ignored regenerate`.
#[test]
#[ignore]
fn regenerate_toy_repros() {
    std::fs::create_dir_all(TOY_REGRESS).unwrap();
    for bug in PLANTED {
        let (sc, expect) = planted(bug);
        let mut found = witness(sc, &expect);
        found.expect = vec![expect[0]];
        let m = minimize::<ToyChain>(&found, 2000);
        assert!(reproduces::<ToyChain>(&m.repro), "{bug:?}");
        let text = format!(
            "# ToyChain planted bug {bug:?} (ST3a §6.2), minimised in {} replays.\n{}",
            m.replays,
            m.repro.to_text()
        );
        std::fs::write(format!("{TOY_REGRESS}/{bug:?}.repro"), text).unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// The deep components (§8.4), in its order.

const DEEP: Mode = Mode::Exhaustive {
    max_branches_per_image: 4096,
};

fn clean_deep(sc: &Scenario<ToyChain>) -> Report {
    let r = explore(sc, DEEP);
    eprintln!("{} {:?}: {}", sc.name, sc.adversary, first_line(&r));
    assert!(r.is_clean(), "{} {:?}: {}", sc.name, sc.adversary, r.summary());
    r
}

/// The summary's first line, with R7a's counts when it judged nested branches (DEC-162 (2): the
/// nightly reports how many were unjudged).
fn first_line(r: &Report) -> String {
    let s = r.summary();
    let mut lines = s.lines();
    let first = lines.next().unwrap_or_default().to_string();
    match lines.next().map(str::trim_start) {
        Some(r7a) if r7a.starts_with("R7a nested branches") => format!("{first}; {r7a}"),
        _ => first,
    }
}

/// The clean matrix: every scenario, `Realistic` (with R12 on every branch) and `Extended`.
#[test]
#[ignore]
fn clean_matrix() {
    for base in common::clean_matrix() {
        for adversary in [Adversary::Realistic, Adversary::Extended] {
            let mut sc = base.clone();
            sc.adversary = adversary;
            sc.r12_all = adversary == Adversary::Realistic && sc.continue_r12;
            clean_deep(&sc);
        }
    }
}

/// Production-scale prune and locks; the prune scenarios do prune.
#[test]
#[ignore]
fn production_scale_prune_and_locks() {
    for sc in prod_scenarios() {
        let t = forward(&sc);
        let picked = selections(&t);
        if ["S3p-prod", "S5-prod", "S5b-prod", "S11q-prod", "S14b-prod"].contains(&sc.name) {
            assert!(!picked.is_empty(), "{}: a production-scale prune selects", sc.name);
        }
        if ["S5c-prod", "S11-prod"].contains(&sc.name) {
            assert!(picked.is_empty(), "{}: the correct bound selects nothing", sc.name);
        }
        clean_deep(&sc);
    }
}

/// Nested depth 1 on S1-S4, S7 and S11.
#[test]
#[ignore]
fn nested_depth1() {
    for mut sc in [s1(None), s2(None), s3(), s4(None), s4b(None), s7(None), s11(None)] {
        sc.nested = Nested::Depth1;
        clean_deep(&sc);
    }
}

/// Nested depth 1 under `Extended` on the same scenarios (DEC-161): an inner
/// power loss can tear the recovery's own write, and R7a counts the sanctioned fail-stops after
/// it apart. Its inner images pass any useful cap, so it is enumerated exhaustively in shards
/// (DEC-185 (2)): `contrib/node/st3a-shards.sh` plans them (`ST3A_SHARD_PLAN=<file>`), runs each
/// as its own low-priority unit (`ST3A_SHARD=<component>:<shard>`, uncapped) and keeps the
/// ledger. The component is clean only when every planned shard passed at the same head. Run
/// without either variable, the test fails: it certifies nothing on its own.
#[test]
#[ignore]
fn nested_depth1_extended() {
    if let Ok(path) = std::env::var("ST3A_SHARD_PLAN") {
        let depth = std::env::var("ST3A_SHARD_DEPTH")
            .map_or(SHARD_DEPTH, |d| d.parse().expect("ST3A_SHARD_DEPTH"));
        let mut plan = format!("# depth {depth}\n");
        for sc in extended_component() {
            let key = sc.name.to_string();
            let shards = shards(&sc, depth);
            eprintln!("{key}: {} shards", shards.len());
            for shard in shards {
                plan.push_str(&format!("{key}:{}\n", shard.id()));
            }
        }
        std::fs::write(&path, plan).unwrap();
        return;
    }
    let id = std::env::var("ST3A_SHARD")
        .expect("run it through contrib/node/st3a-shards.sh (ST3A_SHARD or ST3A_SHARD_PLAN)");
    let (key, shard) = id.split_once(':').expect("<component>:<shard>");
    let shard = Shard::parse(shard).unwrap_or_else(|| panic!("bad shard `{shard}`"));
    let sc = extended_component()
        .into_iter()
        .find(|sc| sc.name == key)
        .unwrap_or_else(|| panic!("no component `{key}`"));
    let r = explore_shard(&sc, &shard);
    eprintln!("SHARD {id}: {}", first_line(&r));
    assert!(r.is_clean(), "{id}: {}", r.summary());
}

/// The shard plan's depth (DEC-185 (2)): each shard is an image, an outer crash kind and the
/// outer search's first `SHARD_DEPTH` decisions, with every nested branch under them. Measured
/// on S1 at load 70–150, before RP-V24 (design I43): at depth 1 a power-loss shard of image 18
/// took 791 CPU-s (1.23M branches) and image 36's ran past 13 CPU-min; at depth 6 image 36 split
/// into 76 shards, most about 18 CPU-s, with a tail of single outer branches over 5 CPU-min that
/// no depth splits. Depth 4 (4,720 shards over the component) is near that floor without
/// multiplying units.
const SHARD_DEPTH: usize = 4;

/// The extended nested component: every scenario of `nested_depth1`, `Extended`.
fn extended_component() -> Vec<Scenario<ToyChain>> {
    let mut out = Vec::new();
    for mut sc in [s1(None), s2(None), s3(), s4(None), s4b(None), s7(None), s11(None)] {
        sc.nested = Nested::Depth1;
        sc.adversary = Adversary::Extended;
        out.push(sc);
    }
    out
}

/// A seed from `var`, decimal or `0x` hex, else `default`.
fn env_seed(var: &str, default: u64) -> u64 {
    std::env::var(var).map_or(default, |s| {
        let parsed = match s.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16),
            None => s.parse(),
        };
        parsed.unwrap_or_else(|e| panic!("{var}={s}: {e}"))
    })
}

/// Nested depth 2 on a seeded 0.3% of S1-S4's and S7's inner crashes.
#[test]
#[ignore]
fn nested_depth2_sample() {
    for mut sc in [s1(None), s2(None), s3(), s4(None), s7(None)] {
        sc.nested = Nested::Depth2Sample(3);
        clean_deep(&sc);
    }
}

/// The seeded sweep of random workloads and configurations: 2 × 10⁴ cases at about 29 ms each
/// in release, §8.4's sweep allowance (§8.4 assumed 2 × 10⁵ cases at 3 ms; erratum I32).
/// `ST3A_SWEEP_CASES` overrides the count, and `ST3A_SWEEP_SEED` (decimal or `0x` hex) the seed,
/// which the nightly rotates (errata I37).
#[test]
#[ignore]
fn sweep() {
    let cases =
        std::env::var("ST3A_SWEEP_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(20_000);
    let seed = env_seed("ST3A_SWEEP_SEED", 0x5733_a5ee);
    eprintln!("sweep seed {seed} ({seed:#x}), {cases} cases");
    let r = dashd_store::faulty::sweep(cases, seed, &|rng| random_toy(rng, None));
    eprintln!("sweep: {}", first_line(&r));
    assert!(r.is_clean(), "{}", r.summary());
}

/// SB10's SPV store (`faulty::spv`, plan §5): its seeded sweep at 2 × 10⁵ cases (about 0.3 ms
/// each, the PR gate runs 5,000). `ST3A_SPV_SWEEP_CASES` overrides the count; the seed is the
/// nightly's `ST3A_SWEEP_SEED`.
#[test]
#[ignore]
fn spv_sweep() {
    let cases =
        std::env::var("ST3A_SPV_SWEEP_CASES").ok().and_then(|c| c.parse().ok()).unwrap_or(200_000);
    let seed = env_seed("ST3A_SWEEP_SEED", 0x005b_10c9);
    eprintln!("spv sweep seed {seed} ({seed:#x}), {cases} cases");
    let r = dashd_store::faulty::sweep(cases, seed, &dashd_store::faulty::spv::random_scenario);
    eprintln!("spv sweep: {}", first_line(&r));
    assert!(r.is_clean(), "{}", r.summary());
}

/// Witness searches with 3 and 8 KiB records under `Extended` and byte granularity: every planted
/// bug is still found. Prune scenarios and S6 keep their record sizes, which their file layouts
/// depend on.
#[test]
#[ignore]
fn witnesses_with_large_records_extended_byte() {
    for (i, bug) in PLANTED.into_iter().enumerate() {
        let (mut sc, expect) = planted(bug);
        if sc.cfg.prune.is_none() && sc.name != "S6" {
            sc.cfg.block_size = if i % 2 == 0 {
                3072
            } else {
                8192
            };
        }
        sc.adversary = Adversary::Extended;
        sc.granularity = dashd_store::faulty::Granularity::Byte;
        witness(sc, &[expect[0]]);
    }
}

/// S11d nested at production `D_max` 1,024, backlog 1 and 2.
#[test]
#[ignore]
fn s11d_nested_at_production_d_max() {
    for b in 1..=2 {
        let sub = SubConfig {
            mode: SubMode::Synced {
                backlog: b,
            },
            d_max: 1024,
        };
        let cfg = prune_sub(sub, Some(1 << 30), None);
        let mut sc =
            at_flush(Scenario::new("S11d-prod", cfg, ops(&format!("f;c{};q50;c1", 1023 + b))), 1);
        sc.nested = Nested::Depth1;
        clean_deep(&sc);
    }
}
