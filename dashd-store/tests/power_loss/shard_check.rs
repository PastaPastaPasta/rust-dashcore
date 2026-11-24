//! Shards against the unsharded search (DEC-185 (2), CP3.4-1, CP3.5-1): at each depth, the shards
//! of a scenario, summed, must be its exhaustive search exactly. The same outer branches per image
//! and kind, the same nested branches and R7a counters per image and kind, every R7a counter and
//! branch in total, and the same findings. Each shard files its nested branches under its own image
//! and kind only (CP3.6-1), so the full search's buckets are pinned too.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use dashd_store::faulty::{
    CrashKind, Mode, NestedTotals, R7aCounts, Report, Scenario, Shard, Sut, explore, explore_shard,
    shards,
};

/// One depth's comparison: its shard count and summed branches (the unsharded search's, or the
/// assertion failed).
pub struct DepthRow {
    pub depth: usize,
    pub shards: usize,
    pub branches: u64,
}

fn per_image(reports: &[Report]) -> BTreeMap<(u64, CrashKind), u64> {
    let mut out = BTreeMap::new();
    for r in reports {
        for (img, kind, n, _) in &r.per_image {
            *out.entry((*img, *kind)).or_default() += n;
        }
    }
    out
}

/// The nested totals per image and kind, summed; a bucket with nothing in it is left out.
fn per_image_nested(reports: &[Report]) -> BTreeMap<(u64, CrashKind), NestedTotals> {
    let mut out: BTreeMap<_, NestedTotals> = BTreeMap::new();
    for r in reports {
        for (key, t) in &r.per_image_nested {
            out.entry(*key).or_default().merge(*t);
        }
    }
    out.retain(|_, t| *t != NestedTotals::default());
    out
}

/// A report keeps one finding per invariant and detail, so shards are compared by those.
fn findings(reports: &[Report]) -> BTreeSet<String> {
    reports.iter().flat_map(|r| r.findings.iter().map(|f| f.violation.to_string())).collect()
}

/// Asserts that the shard reports `reports` of `name` at `depth` sum to `full`, its unsharded
/// search.
pub fn assert_depth_matches(full: &Report, reports: &[Report], name: &str, depth: usize) {
    let whole = std::slice::from_ref(full);
    assert_eq!(
        per_image(reports),
        per_image(whole),
        "{name} depth {depth}: outer branches per image and kind"
    );
    assert_eq!(
        per_image_nested(reports),
        per_image_nested(whole),
        "{name} depth {depth}: nested branches and R7a counters per image and kind"
    );
    let r7a = reports.iter().fold(R7aCounts::default(), |mut a, r| {
        a.merge(r.r7a);
        a
    });
    assert_eq!(r7a, full.r7a, "{name} depth {depth}: R7a counters (the nested branches)");
    let branches: u64 = reports.iter().map(|r| r.branches).sum();
    assert_eq!(branches, full.branches, "{name} depth {depth}: branches");
    assert_eq!(findings(reports), findings(whole), "{name} depth {depth}");
    assert!(reports.iter().all(|r| r.truncated == 0), "{name} depth {depth}");
}

/// Asserts that each shard's report files nested branches only under that shard's image and kind.
pub fn assert_buckets_owned(plan: &[Shard], reports: &[Report], name: &str, depth: usize) {
    for (s, r) in plan.iter().zip(reports) {
        for (key, t) in &r.per_image_nested {
            assert!(
                *t == NestedTotals::default() || *key == (s.image, s.kind),
                "{name} depth {depth}: shard {} files nested branches under {key:?}",
                s.id()
            );
        }
    }
}

/// Asserts that `sc`'s shards at each of `depths` sum to its uncapped exhaustive search, and
/// returns the unsharded report with a row per depth.
pub fn assert_shards_match<S: Sut>(sc: &Scenario<S>, depths: &[usize]) -> (Report, Vec<DepthRow>) {
    let full = explore(
        sc,
        Mode::Exhaustive {
            max_branches_per_image: u32::MAX,
        },
    );
    let name = &sc.name;
    assert_eq!(full.truncated, 0, "{name}: {}", full.summary());
    let mut rows = Vec::new();
    for &depth in depths {
        let plan = shards(sc, depth);
        let reports: Vec<Report> = plan.iter().map(|s| explore_shard(sc, s)).collect();
        assert_buckets_owned(&plan, &reports, name, depth);
        assert_depth_matches(&full, &reports, name, depth);
        let branches: u64 = reports.iter().map(|r| r.branches).sum();
        eprintln!("{name} depth {depth}: {} shards, {branches} branches", plan.len());
        rows.push(DepthRow {
            depth,
            shards: plan.len(),
            branches,
        });
    }
    (full, rows)
}
