//! The comparison (F8, §5.1, D35): a leg's (A − B) from its two records, and the gate, the
//! node leg's (A − B) per block against Core's. Any identity mismatch, unmet condition or
//! missing count refuses; a missing leg is [`Verdict::Pending`], never a pass.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::account::CALIBRATION_PAIRS;
use crate::fixture;
use crate::record::{Identity, Leg, Pass, Record};
use crate::{AUDITED_KERNEL, Result, refuse};

/// The script-check workers the pinned Core's ChainTestingSetup starts
/// (setup_common.cpp's `script_check_threads`, 2 at 08700f95eac5 and at 23ff9c1451fb). A re-pin
/// that changes it must change this too.
pub const CORE_SCRIPT_CHECK_THREADS: usize = 2;

/// Refuses unless `got` holds each of `want` exactly once and nothing else: what H must have
/// read after an A pass's window, one `cmpctblock` per workload block (SOL-2).
pub fn one_each<K: Ord>(got: &BTreeMap<K, usize>, want: impl IntoIterator<Item = K>) -> Result<()> {
    let want: BTreeMap<K, usize> = want.into_iter().map(|k| (k, 1)).collect();
    if *got != want {
        let missing = want.keys().filter(|&k| !got.contains_key(k)).count();
        let extra: usize =
            got.iter().map(|(k, n)| n.saturating_sub(usize::from(want.contains_key(k)))).sum();
        refuse!(
            "H's cmpctblocks after the window are not one per workload block: {missing} of {} \
             missing, {extra} extra",
            want.len()
        );
    }
    Ok(())
}

/// One leg's (A − B).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegReport {
    pub leg: Leg,
    pub identity: Identity,
    pub a_instructions: u64,
    pub b_instructions: u64,
    /// A − B (negative if B counted more).
    pub a_minus_b: i128,
    /// A − B per workload block, rounded toward zero.
    pub per_block: i128,
    pub pids: [u32; 2],
}

/// Refuses unless `r` states the conditions every measured pass needs.
fn conditions(r: &Record) -> Result<()> {
    let what = format!("{:?} {:?}", r.leg, r.pass);
    let a = r.pass == Pass::A;
    if !r.identity.kernel.starts_with(AUDITED_KERNEL) {
        refuse!("{what}: kernel {} is not the audited {AUDITED_KERNEL}x", r.identity.kernel);
    }
    if r.conditions.ibd_at_start {
        refuse!("{what}: in initial block download at the window's start");
    }
    if a && r.conditions.hb_at_start != Some(true) {
        refuse!("{what}: H was not high-bandwidth at the window's start");
    }
    if r.conditions.check_block_index.is_empty() {
        refuse!("{what}: no block-index check flag recorded");
    }
    if let Some((name, _)) = r.conditions.check_block_index.iter().find(|(_, on)| *on) {
        refuse!("{what}: block-index check {name} is on");
    }
    if r.conditions.log_categories != 0 {
        refuse!("{what}: debug categories {:#x}", r.conditions.log_categories);
    }
    if r.conditions.bench != (r.leg == Leg::Core).then_some(true) {
        refuse!("{what}: bench session {:?}", r.conditions.bench);
    }
    if r.conditions.options.iter().any(|o| o.starts_with("-debug") && o != "-debug=0") {
        refuse!("{what}: a debug option");
    }
    if r.leg == Leg::Node
        && (r.conditions.script_check_threads != 0
            || !r.conditions.options.iter().any(|o| o == "-par=1"))
    {
        refuse!(
            "{what}: script checks not serial (-par=1): {} script-check threads",
            r.conditions.script_check_threads
        );
    }
    if r.leg == Leg::Core && r.conditions.script_check_threads != CORE_SCRIPT_CHECK_THREADS {
        refuse!(
            "{what}: {} script-check threads, not the pinned setup's {CORE_SCRIPT_CHECK_THREADS}",
            r.conditions.script_check_threads
        );
    }
    r.completion.check(a).map_err(|e| crate::Refusal(format!("{what}: {}", e.0)))?;
    if r.identity.workload_blocks == 0 {
        refuse!("{what}: no workload blocks");
    }
    shape(r, &what)
}

/// Refuses a count whose window was not set up as its pass's: node passes exclude only their
/// driver and hold their flush thread idle, Core A segments its caller (calibrated, at least the
/// workload's pass and the final one), Core B counts every thread continuously.
fn shape(r: &Record, what: &str) -> Result<()> {
    let Some(c) = r.counts else {
        return Ok(());
    };
    let (calls, pairs, excluded, idle) = match (r.leg, r.pass) {
        (Leg::Core, Pass::A) => (c.calls >= 2, CALIBRATION_PAIRS as u32, 0, c.threads_idle == 0),
        (Leg::Core, Pass::B) => (c.calls == 0, 0, 0, c.threads_idle == 0),
        (Leg::Node, _) => (c.calls == 0, 0, 1, c.threads_idle >= 1),
    };
    if !calls
        || !idle
        || c.calibration_pairs != pairs
        || c.threads_excluded != excluded
        || c.threads_counted == 0
    {
        refuse!("{what}: a window shaped {c:?}");
    }
    Ok(())
}

/// A leg's report from its pass A and pass B records.
pub fn leg(a: &Record, b: &Record) -> Result<LegReport> {
    if (a.pass, b.pass) != (Pass::A, Pass::B) || a.leg != b.leg {
        refuse!(
            "records {:?} {:?} and {:?} {:?} are no leg's A and B",
            a.leg,
            a.pass,
            b.leg,
            b.pass
        );
    }
    if a.pid == b.pid {
        refuse!("both passes ran in process {}", a.pid);
    }
    if a.identity != b.identity {
        refuse!("pass A's identity {:?} is not pass B's {:?}", a.identity, b.identity);
    }
    if (a.leg == Leg::Core) != a.identity.core.is_some() {
        refuse!("{:?} leg with Core build {:?}", a.leg, a.identity.core);
    }
    if let Some(c) = &a.identity.core {
        let hex = |h: &str| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit());
        if !c.pinned
            || c.flavor != "plain"
            || !hex(&c.pinned_sha256)
            || c.archive_sha256 != c.pinned_sha256
        {
            refuse!("a Core build that is not the pinned plain archive: {c:?}");
        }
    }
    conditions(a)?;
    conditions(b)?;
    if a.conditions.script_check_threads != b.conditions.script_check_threads {
        refuse!(
            "pass A ran {} script-check threads, pass B {}",
            a.conditions.script_check_threads,
            b.conditions.script_check_threads
        );
    }
    let (Some(ca), Some(cb)) = (a.counts, b.counts) else {
        refuse!("a pass has no count (no PMU)");
    };
    let a_minus_b = i128::from(ca.total) - i128::from(cb.total);
    Ok(LegReport {
        leg: a.leg,
        identity: a.identity.clone(),
        a_instructions: ca.total,
        b_instructions: cb.total,
        a_minus_b,
        per_block: a_minus_b / i128::from(a.identity.workload_blocks),
        pids: [a.pid, b.pid],
    })
}

/// The gate's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    /// The node's (A − B) per block is at most Core's.
    Pass {
        node_per_block: i128,
        core_per_block: i128,
    },
    Fail {
        node_per_block: i128,
        core_per_block: i128,
    },
    /// A leg is missing.
    Pending(String),
}

/// The gate on a node leg and the Core leg; refuses when they were not measured on the same
/// revision, profile, kernel, CPU, build, fixture, prefix, workload and `T0`, on less than the
/// full fixture, or not in a release build.
pub fn gate(node: Option<&LegReport>, core: Option<&LegReport>) -> Result<Verdict> {
    judge(node, core, true)
}

/// [`gate`]'s verdict with every check but the release build's: what a debug run's legs would
/// get. Never the gate.
pub fn debug_verdict(node: Option<&LegReport>, core: Option<&LegReport>) -> Result<Verdict> {
    judge(node, core, false)
}

fn judge(node: Option<&LegReport>, core: Option<&LegReport>, release: bool) -> Result<Verdict> {
    let (Some(node), Some(core)) = (node, core) else {
        return Ok(Verdict::Pending(
            match (node, core) {
                (None, None) => "no leg",
                (None, _) => "no node leg",
                _ => "no Core leg",
            }
            .into(),
        ));
    };
    if (node.leg, core.leg) != (Leg::Node, Leg::Core) {
        refuse!("legs {:?} and {:?}", node.leg, core.leg);
    }
    let shared = |i: &Identity| Identity {
        core: None,
        ..i.clone()
    };
    if shared(&node.identity) != shared(&core.identity) {
        refuse!("the node leg's identity {:?} is not Core's {:?}", node.identity, core.identity);
    }
    let i = &node.identity;
    if (i.prefix_blocks as usize, i.workload_blocks as usize, i.workload_txs)
        != (fixture::PREFIX_BLOCKS, fixture::WORKLOAD_BLOCKS, fixture::WORKLOAD_TXS)
    {
        refuse!(
            "a gate on {} prefix and {} workload blocks ({} txs), not the full fixture's",
            i.prefix_blocks,
            i.workload_blocks,
            i.workload_txs
        );
    }
    let rev = &node.identity.revision;
    if rev == "unknown" || rev.ends_with("-dirty") || rev.ends_with("-stale") {
        refuse!("revision {rev} is not a clean build of a commit");
    }
    if release && node.identity.profile != "release" {
        refuse!("a {} build", node.identity.profile);
    }
    let (n, c) = (node.per_block, core.per_block);
    Ok(if node.a_minus_b <= core.a_minus_b {
        Verdict::Pass {
            node_per_block: n,
            core_per_block: c,
        }
    } else {
        Verdict::Fail {
            node_per_block: n,
            core_per_block: c,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::CoreBuild;
    use crate::record::tests::record;

    fn report(leg_: Leg, a: u64, b: u64) -> LegReport {
        leg(&record(leg_, Pass::A, a), &record(leg_, Pass::B, b)).unwrap()
    }

    #[test]
    fn one_each_refuses_a_missing_extra_or_repeated_relay() {
        let map = |kv: &[(u8, usize)]| kv.iter().copied().collect::<BTreeMap<_, _>>();
        assert!(one_each(&map(&[(1, 1), (2, 1)]), [1, 2]).is_ok());
        for (got, why) in [
            (map(&[(1, 1)]), "1 of 2 missing, 0 extra"),
            (map(&[(1, 1), (2, 2)]), "0 of 2 missing, 1 extra"),
            (map(&[(1, 1), (2, 1), (3, 1)]), "0 of 2 missing, 1 extra"),
            (map(&[(1, 1), (3, 1)]), "1 of 2 missing, 1 extra"),
            (map(&[(1, 1), (2, 0)]), "0 of 2 missing, 0 extra"),
        ] {
            let e = one_each(&got, [1, 2]).expect_err(why);
            assert!(e.0.contains(why), "{e}");
        }
    }

    #[test]
    fn a_leg_is_a_minus_b_per_block() {
        let r = report(Leg::Core, 201_000 + 2010, 1_000);
        assert_eq!((r.a_minus_b, r.per_block), (202_010, 1_005));
        assert_eq!(report(Leg::Node, 0, 201).per_block, -1);
    }

    #[test]
    fn a_leg_refuses_any_mismatch() {
        let a = record(Leg::Node, Pass::A, 10);
        let b = record(Leg::Node, Pass::B, 5);
        let refused = |a: &Record, b: &Record, why: &str| {
            let e = leg(a, b).unwrap_err();
            assert!(e.0.contains(why), "{why}: {e}");
        };
        refused(&b, &a, "no leg's");
        refused(
            &a,
            &Record {
                pid: a.pid,
                ..b.clone()
            },
            "both passes",
        );
        let mut other = b.clone();
        other.identity.fixture_sha256 = "0".repeat(64);
        refused(&a, &other, "identity");
        let mut other = b.clone();
        other.identity.t0 += 1;
        refused(&a, &other, "identity");
        refused(&record(Leg::Core, Pass::A, 1), &record(Leg::Node, Pass::B, 1), "no leg's");
        let mut no_count = b.clone();
        no_count.counts = None;
        refused(&a, &no_count, "no count");
    }

    /// SOL-R2-1: each Core pass must run the pinned setup's script-check workers, so matching
    /// wrong counts refuse as well as a one-sided one.
    #[test]
    fn a_core_leg_refuses_passes_with_other_script_check_threads() {
        let a = record(Leg::Core, Pass::A, 10);
        let b = record(Leg::Core, Pass::B, 5);
        assert!(leg(&a, &b).is_ok());
        for (na, nb) in [(0, 0), (1, 1), (3, 3), (15, 15), (2, 0), (0, 2), (2, 3), (3, 2)] {
            let (mut a, mut b) = (a.clone(), b.clone());
            a.conditions.script_check_threads = na;
            b.conditions.script_check_threads = nb;
            let e = leg(&a, &b).unwrap_err();
            assert!(e.0.contains("not the pinned setup's 2"), "{na}/{nb}: {e}");
        }
    }

    /// rev2 N2: any block-index check on, any debug category, IBD or a missing HB at the window's
    /// start, an unfinished completion, an unaudited kernel, parallel node script checks or a
    /// node window without its held flush thread refuses.
    #[test]
    fn a_leg_refuses_unmet_conditions() {
        let b = record(Leg::Node, Pass::B, 5);
        type Case = (&'static str, Box<dyn Fn(&mut Record)>);
        let cases: Vec<Case> = vec![
            (
                "block-index check",
                Box::new(|r| r.conditions.check_block_index.push(("header".into(), true))),
            ),
            ("no block-index", Box::new(|r| r.conditions.check_block_index.clear())),
            ("debug categories", Box::new(|r| r.conditions.log_categories = 1)),
            ("a debug option", Box::new(|r| r.conditions.options.push("-debug=net".into()))),
            ("bench session", Box::new(|r| r.conditions.bench = Some(false))),
            ("initial block download", Box::new(|r| r.conditions.ibd_at_start = true)),
            ("high-bandwidth", Box::new(|r| r.conditions.hb_at_start = Some(false))),
            ("completion", Box::new(|r| r.completion.actor_barrier = Some(false))),
            ("kernel", Box::new(|r| r.identity.kernel = "6.8.0-1-generic".into())),
            ("shaped", Box::new(|r| r.counts.as_mut().unwrap().threads_excluded = 0)),
            ("shaped", Box::new(|r| r.counts.as_mut().unwrap().calls = 1)),
            ("shaped", Box::new(|r| r.counts.as_mut().unwrap().threads_idle = 0)),
            ("not serial", Box::new(|r| r.conditions.script_check_threads = 2)),
            ("not serial", Box::new(|r| r.conditions.options.retain(|o| o != "-par=1"))),
        ];
        for (why, f) in cases {
            let mut a = record(Leg::Node, Pass::A, 10);
            f(&mut a);
            let mut b = b.clone();
            b.identity = a.identity.clone();
            let e = leg(&a, &b).unwrap_err();
            assert!(e.0.contains(why), "{why}: {e}");
        }
    }

    /// A missing leg is pending, never a pass; the gate compares per block and refuses legs
    /// measured on different inputs.
    #[test]
    fn the_gate_compares_like_with_like() {
        let node = report(Leg::Node, 3_000, 1_000);
        let core = report(Leg::Core, 5_000, 1_000);
        assert!(matches!(gate(None, Some(&core)).unwrap(), Verdict::Pending(_)));
        assert!(matches!(gate(Some(&node), None).unwrap(), Verdict::Pending(_)));
        assert!(matches!(gate(Some(&node), Some(&core)).unwrap(), Verdict::Pass { .. }));
        assert!(gate(Some(&core), Some(&node)).is_err());
        let slow = report(Leg::Node, 9_000, 1_000);
        assert!(matches!(gate(Some(&slow), Some(&core)).unwrap(), Verdict::Fail { .. }));
        let mut other = node.clone();
        other.identity.revision = "def".into();
        assert!(gate(Some(&other), Some(&core)).unwrap_err().0.contains("identity"));
        for (revision, profile) in [
            ("abc-dirty", "release"),
            ("abc-stale", "release"),
            ("unknown", "release"),
            ("abc", "debug"),
        ] {
            let (mut n, mut c) = (node.clone(), core.clone());
            for i in [&mut n.identity, &mut c.identity] {
                (i.revision, i.profile) = (revision.into(), profile.into());
            }
            assert!(gate(Some(&n), Some(&c)).is_err(), "{revision} {profile}");
            let debug = debug_verdict(Some(&n), Some(&c));
            assert_eq!(
                matches!(debug, Ok(Verdict::Pass { .. })),
                profile == "debug",
                "{revision} {profile}: {debug:?}"
            );
        }
    }

    /// The Core leg's shape: a caller segmented with its calibration; B counts every thread.
    #[test]
    fn core_windows_are_shaped_as_their_pass() {
        let mut a = record(Leg::Core, Pass::A, 10);
        let b = record(Leg::Core, Pass::B, 5);
        assert!(leg(&a, &b).is_ok());
        a.counts.as_mut().unwrap().calibration_pairs = 0;
        assert!(leg(&a, &b).unwrap_err().0.contains("shaped"));
        let mut b2 = b.clone();
        b2.counts.as_mut().unwrap().threads_excluded = 1;
        assert!(leg(&record(Leg::Core, Pass::A, 10), &b2).unwrap_err().0.contains("shaped"));
    }

    /// Opus F1: a Core leg refuses any archive but the pinned plain one: unpinned, another
    /// flavor, a sha256 other than `pins.txt`'s plain column, or none.
    #[test]
    fn a_core_leg_refuses_an_unpinned_or_other_archive() {
        let cases: [fn(&mut CoreBuild); 6] = [
            |c| c.pinned = false,
            |c| c.flavor = "ubsan".into(),
            |c| c.flavor = "fuzz".into(),
            |c| c.archive_sha256 = "c".repeat(64),
            |c| c.archive_sha256 = "none".into(),
            |c| (c.archive_sha256, c.pinned_sha256) = ("-".into(), "-".into()),
        ];
        for f in cases {
            let (mut a, mut b) = (record(Leg::Core, Pass::A, 10), record(Leg::Core, Pass::B, 5));
            f(a.identity.core.as_mut().unwrap());
            b.identity = a.identity.clone();
            let e = leg(&a, &b).unwrap_err();
            assert!(e.0.contains("pinned plain archive"), "{e}");
        }
    }

    /// Opus M1: legs measured on another CPU or build environment do not compare.
    #[test]
    fn the_gate_refuses_another_cpu_or_build() {
        let core = report(Leg::Core, 5_000, 1_000);
        let cases: [fn(&mut Identity); 2] =
            [|i| i.cpu = "other".into(), |i| i.build = "other".into()];
        for f in cases {
            let mut node = report(Leg::Node, 3_000, 1_000);
            f(&mut node.identity);
            assert!(gate(Some(&node), Some(&core)).unwrap_err().0.contains("identity"));
        }
    }

    /// Opus N2: the gate takes only the full fixture's legs.
    #[test]
    fn the_gate_requires_the_full_fixture() {
        let cases: [fn(&mut Identity); 3] =
            [|i| i.workload_blocks = 8, |i| i.prefix_blocks = 99, |i| i.workload_txs -= 1];
        for f in cases {
            let (mut node, mut core) =
                (report(Leg::Node, 3_000, 1_000), report(Leg::Core, 5_000, 1_000));
            f(&mut node.identity);
            f(&mut core.identity);
            assert!(gate(Some(&node), Some(&core)).unwrap_err().0.contains("full fixture"));
        }
    }
}
