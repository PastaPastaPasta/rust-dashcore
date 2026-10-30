//! Shared ToyChain scenarios, search helpers and the planted-bug table (design §6.2) for
//! `power_loss_toy.rs` (PR gate) and `power_loss_deep.rs` (nightly).

#![allow(dead_code, unused_imports)]

pub use InvariantId::*;
pub use dashd_store::faulty::toy::reference::ToyRef;
pub use dashd_store::faulty::toy::{
    PruneConfig, SubConfig, SubMode, ToyBug, ToyChain, ToyConfig, ToyOp,
};
pub use dashd_store::faulty::{
    Adversary, AllocateMode, CommitKind, CrashKind, CrashWindow, EventSel, Fault, FaultKind,
    FaultOutcome, FaultPlan, FlushCause, Hash, InvariantId, LocatorKind, Mark, MarkKind, Mode,
    Nested, OpKind, OverCap, PruneDue, Reference, Report, Repro, Scenario, Shard, StoreSel, Trace,
    TraceEntry, Violation, WitnessSet, explore, explore_shard, forward_trace, minimize, replay,
    replay_trace, reproduces, shards,
};

/// The point right after the `occurrence`-th mark of a kind.
pub fn after(mark: MarkKind, occurrence: u32) -> EventSel {
    EventSel::AfterMark {
        mark,
        occurrence,
        n: 0,
    }
}

/// The window from the `occurrence`-th mark of a kind to the end.
pub fn from_mark(mark: MarkKind, occurrence: u32) -> CrashWindow {
    CrashWindow {
        from: after(mark, occurrence),
        to: None,
    }
}

/// The window of the single point `at`.
pub fn only_at(at: EventSel) -> CrashWindow {
    CrashWindow {
        from: at.clone(),
        to: Some(at),
    }
}

/// The window from the `n`-th flush's begin to the end.
pub fn from_flush(n: u32) -> CrashWindow {
    from_mark(MarkKind::FlushBegin, n)
}

/// The `n`-th `op` on a store.
pub fn nth(store: StoreSel, op: OpKind, n: u32) -> EventSel {
    EventSel::Nth {
        store,
        op,
        n,
    }
}

/// The chainstate's `n`-th Durable append.
pub fn durable_append(n: u32) -> EventSel {
    nth(StoreSel::Engine("chainstate".into()), OpKind::Append(CommitKind::Durable), n)
}

/// The window from the forward run's last Durable chainstate append to the end.
pub fn from_last_durable_append(sc: &Scenario<ToyChain>) -> CrashWindow {
    let n = forward(sc)
        .events()
        .filter(|(_, store, op, _)| {
            *store == "chainstate" && *op == OpKind::Append(CommitKind::Durable)
        })
        .count() as u32;
    CrashWindow {
        from: durable_append(n - 1),
        to: None,
    }
}

/// A witness search narrowed to the bug's trigger (§4.5): the window starts at flush `n`.
pub fn at_flush(mut sc: Scenario<ToyChain>, n: u32) -> Scenario<ToyChain> {
    sc.window = from_flush(n);
    sc
}

pub fn ops(s: &str) -> Vec<ToyOp> {
    s.split(';').map(|o| o.parse().unwrap()).collect()
}

pub const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 1024,
};

/// S1: 30 blocks, one-batch flushes at 10/20/30, an index-only write at 15, independent
/// effects at 12 and 22, a presumed effect at 25 read by block 31's fold; a synced subscriber
/// (backlog 0).
pub fn s1(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        effects: vec![12, 22],
        presumed_base: Some(25),
        presumed_reader: Some(31),
        sub: Some(SubConfig::synced(0)),
        bug,
        ..ToyConfig::default()
    };
    Scenario::new("S1", cfg, ops("c10;f;c5;i;c5;f;c10;f"))
}

/// S4: a split flush of 20 blocks in 5 coin sub-batches; a synced subscriber.
pub fn s4(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub_batches: 5,
        sub: Some(SubConfig::synced(0)),
        bug,
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S4", cfg, ops("c20;f")), 0)
}

/// S4b: two split flushes, 0 → 20 and 20 → 40; a synced subscriber.
pub fn s4b(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub_batches: 5,
        sub: Some(SubConfig::synced(0)),
        bug,
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S4b", cfg, ops("c20;f;c20;f")), 0)
}

pub fn clean(sc: &Scenario<ToyChain>) -> Report {
    let r = explore(sc, EXHAUSTIVE);
    eprintln!("{}: {}", sc.name, r.summary());
    assert!(r.is_clean(), "{}: {}", sc.name, r.summary());
    r
}

pub fn witness(sc: Scenario<ToyChain>, expect: &[InvariantId]) -> Repro {
    let name = sc.name;
    let r = explore(
        &sc,
        Mode::Witness {
            expect: expect.to_vec(),
            max_branches: 50_000,
        },
    );
    eprintln!("{name} {:?}: {}", sc.cfg.bug, r.summary());
    assert!(r.found_all, "{name} {:?}: expected {expect:?}\n{}", sc.cfg.bug, r.summary());
    let f = r.first(expect[0]).unwrap();
    let got = replay::<ToyChain>(&f.repro).unwrap();
    assert!(got.iter().any(|v| v.invariant == expect[0]), "the repro replays: {got:?}");
    f.repro.clone()
}

/// A planted bug's witness search (§6.2's table).
pub fn witness_planted(bug: ToyBug) {
    let (sc, expect) = planted(bug);
    witness(sc, &expect);
}

pub fn prune_cfg(bug: Option<ToyBug>) -> ToyConfig {
    ToyConfig {
        sub_batches: 5,
        prune: Some(PruneConfig::default()),
        bug,
        ..ToyConfig::default()
    }
}

/// The files each `PruneSelected` of a trace selected, in order.
pub fn selections(t: &Trace) -> Vec<Vec<u32>> {
    t.marks()
        .filter_map(|m| match m {
            Mark::PruneSelected {
                files,
                ..
            } => Some(files.clone()),
            _ => None,
        })
        .collect()
}

/// The forward run's own violations must be clean; returns its trace.
pub fn forward(sc: &Scenario<ToyChain>) -> Trace {
    let (t, v) = forward_trace(sc);
    assert!(v.is_empty(), "{}: the forward run: {v:?}", sc.name);
    t
}

/// S5: heights 0-72, coins flushed at 5, an index-only write at 40, a split prune flush at 72
/// (bound 64) that selects file 0 (heights 0-62).
pub fn s5(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    at_flush(Scenario::new("S5", prune_cfg(bug), ops("c5;f;c35;i;c32;p72")), 1)
}

/// S5b: S5 with no index-only write: blocks 6-72 are newly dirty at B1.
pub fn s5b(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    at_flush(Scenario::new("S5b", prune_cfg(bug), ops("c5;f;c67;p72")), 1)
}

/// S5c: V ahead. Flush 1 at 60 with V at 70 (prior flush 50) and a manual prune due: the bound
/// from `new` (52) selects nothing, V's tip (62) would select file 0. Flush 2 at 80: the marker
/// [80, 60].
pub fn s5c(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    at_flush(Scenario::new("S5c", prune_cfg(bug), ops("c50;f;c20;q100;F10:0;c10;f")), 1)
}

/// The number of Durable appends to the chainstate before the prune selection (Bk is the next).
pub fn bk_index(sc: &Scenario<ToyChain>) -> u32 {
    let t = forward(sc);
    let mut n = 0;
    for e in &t.entries {
        match e {
            TraceEntry::Event {
                store,
                op: OpKind::Append(CommitKind::Durable),
                ..
            } if store == "chainstate" => n += 1,
            TraceEntry::Mark {
                mark: Mark::PruneSelected {
                    ..
                },
                ..
            } => return n,
            _ => {}
        }
    }
    panic!("no selection");
}

pub fn bk_fault(sc: &Scenario<ToyChain>) -> Fault {
    Fault {
        at: durable_append(bk_index(sc)),
        kind: FaultKind::Io,
        outcome: FaultOutcome::Applied,
    }
}

/// Usage per block: a 1,032-byte blk record and a 280-byte rev record.
pub const PER_BLOCK: u64 = 1032 + 280;

/// ST2's room under the target (16 MiB + 1 MiB).
pub const BUFFER: u64 = (16 << 20) + (1 << 20);

/// Heights 0-200 in files 0-3; at 200 the target leaves usage 516 bytes under it once file 0
/// (63 blocks) is removed; V connects 201 between the decision and the selection.
pub fn s14b(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let target = (201 - 63) * PER_BLOCK + BUFFER + 516;
    at_flush(Scenario::new("S14b", prune_cfg(bug), ops(&format!("c200;A{target};F0:1;c1"))), 0)
}

pub fn forward_finds(sc: Scenario<ToyChain>, inv: InvariantId) {
    let (_, v) = forward_trace(&sc);
    assert!(v.iter().any(|v| v.invariant == inv), "{}: {v:?}", sc.name);
}

/// S6: 900-byte blocks (72 per file), accepted ahead across a rollover so the undo of file 0's
/// blocks lags into the old rev file while the current pair is file 1, and a flush before file
/// 0's last undo (whose write would make ST2 sync the old rev file itself).
pub fn s6(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        block_size: 900,
        bug,
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S6", cfg, ops("c60;f;a20;c5;f")), 1)
}

/// S7: an invalid block (22) persisted before connection, one valid block connected before B1,
/// a validation-only connect, then a split flush.
pub fn s7(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub_batches: 3,
        invalid: vec![22],
        bug,
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S7", cfg, ops("c20;f;a3;v;c1;f")), 1)
}

/// S9 (non-prune): EIO on every call of a 20-block flush window, and read faults in recovery.
pub fn s9_faults() -> Vec<Fault> {
    let t = forward(&s4(None));
    let in_flush = t
        .entries
        .iter()
        .skip_while(|e| {
            !matches!(
                e,
                TraceEntry::Mark {
                    mark: Mark::FlushBegin { .. },
                    ..
                }
            )
        })
        .filter(|e| matches!(e, TraceEntry::Event { .. }))
        .count() as u32;
    assert!(in_flush > 10, "the flush has {in_flush} events");
    (0..in_flush)
        .map(|n| Fault {
            at: EventSel::AfterMark {
                mark: MarkKind::FlushBegin,
                occurrence: 0,
                n,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::Applied,
        })
        .collect()
}

/// The reference after a scenario's workload: block heights for the trace's hashes.
pub fn reference(sc: &Scenario<ToyChain>) -> ToyRef {
    let mut r = ToyRef::new(&sc.cfg);
    sc.ops.iter().for_each(|op| r.apply(op));
    r
}

/// The subscriber's locator commits: (height, kind), in order.
pub fn commits(sc: &Scenario<ToyChain>, t: &Trace) -> Vec<(u32, LocatorKind)> {
    let r = reference(sc);
    t.marks()
        .filter_map(|m| match m {
            Mark::Locator {
                tip,
                kind,
                ..
            } => Some((r.height(tip).unwrap(), *kind)),
            _ => None,
        })
        .collect()
}

pub fn logs(t: &Trace, needle: &str) -> Vec<String> {
    t.marks()
        .filter_map(|m| match m {
            Mark::Log {
                line,
            } if line.contains(needle) => Some(line.clone()),
            _ => None,
        })
        .collect()
}

pub fn prune_sub(sub: SubConfig, target: Option<u64>, bug: Option<ToyBug>) -> ToyConfig {
    ToyConfig {
        prune: Some(PruneConfig {
            target,
            ..PruneConfig::default()
        }),
        sub: Some(sub),
        bug,
        ..ToyConfig::default()
    }
}

/// S2: a reorg of depth 3 inside the third flush window, a synced subscriber with backlog 2
/// (prune mode, for Core's moveback lines; nothing is pruned).
pub fn s2(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::synced(2), None, bug);
    at_flush(Scenario::new("S2", cfg, ops("c10;f;c10;f;c5;r22:8;f")), 1)
}

/// S3p: a fresh pruned IBD of heights 0-72, the first flush one-batch with pruning; the synced
/// subscriber has no locator, commits at 72, and locks at 69.
pub fn s3p(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    at_flush(Scenario::new("S3p", prune_sub(SubConfig::synced(0), None, bug), ops("c72;p72")), 0)
}

/// S11: a syncing subscriber (cadence 80), durable at 10 with progress 70; heights 0-120 and a
/// flush at 120 with a manual prune due.
pub fn s11(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::syncing(80), None, bug);
    at_flush(Scenario::new("S11", cfg, ops("c10;f;c110;s11;K;s60;q120;f")), 1)
}

/// S11r: a syncing subscriber durable and processed at 70 on branch A; a reorg to B at 50, B to
/// 80, and a prune flush at 80 before the subscriber sees it; then its rewind commit at 50 and
/// catch-up of B51-80, after which it is synced.
pub fn s11r(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::syncing(80), None, bug);
    at_flush(Scenario::new("S11r", cfg, ops("c70;f;s71;K;r50:30;q80;f;s51;f")), 1)
}

/// S11s: a syncing subscriber (cadence 25) from height 0 while chainstate flushes at 20 and 40
/// and V reaches 60; it turns synced at 60, then a flush at 60.
pub fn s11s(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub: Some(SubConfig::syncing(25)),
        bug,
        ..ToyConfig::default()
    };
    // Before the first flush nothing is durable and the subscriber has not started.
    at_flush(Scenario::new("S11s", cfg, ops("c60;F40:0;s26;F20:0;s25;s11;f")), 0)
}

/// S11q: a synced subscriber with backlog 2 and no locator; flushes at 40 (no prune) and 72 (a
/// manual prune), each two blocks behind at step 0.
pub fn s11q(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::synced(2), None, bug);
    // §8.2: the flush at 72, step 0 to the unlink.
    at_flush(Scenario::new("S11q", cfg, ops("c40;f;c32;p72")), 1)
}

/// S11d: the `D_max` cycle. Chainstate and locator durable at 0, `D_max` 96, backlog `b`; no
/// scripted flush. The subscriber reaches 96 with V at 96 + b, a `DMax` flush is due (with a
/// manual prune to 50 whose dry run is empty); automatic checks under a target above usage are
/// empty.
pub fn s11d(b: u32, bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::synced(b), Some(1 << 30), bug);
    at_flush(Scenario::new("S11d", cfg, ops(&format!("f;c{};q50;c1", 95 + b))), 1)
}

/// S13: a syncing subscriber durable at 10 (progress 70); chainstate durable at 120 with
/// 121-130 `HAVE_DATA` through an index-only write; after a crash, activation connects 121-130
/// with init's prune check due under an automatic target.
pub fn s13(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::syncing(80), Some(1), bug);
    at_flush(Scenario::new("S13", cfg, ops("c10;f;c110;s11;K;s60;f;a10;i")), 1)
}

/// A ProcessKill at the end of the workload, replayed: recovery's violations and the trace.
pub fn killed_at_end(sc: &Scenario<ToyChain>) -> (Vec<Violation>, Trace) {
    let mut r = Repro::new(sc, None);
    r.crash_at = Some(forward(sc).events().count() as u64 - 1);
    r.kind = CrashKind::ProcessKill;
    replay_trace::<ToyChain>(&r).unwrap()
}

/// The selections recovery makes after a ProcessKill at the end of the workload.
pub fn recovery_selections(sc: &Scenario<ToyChain>) -> Vec<Vec<u32>> {
    let (_, mut t) = killed_at_end(sc);
    t.entries.retain(|e| {
        matches!(
            e,
            TraceEntry::Mark {
                epoch: 1,
                ..
            }
        )
    });
    selections(&t)
}

/// S14: a synced subscriber durable at 10; V connects to 72 with automatic checks at each chunk
/// allocation (all empty), then a manual prune at 72.
pub fn s14(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::synced(0), Some(1), bug);
    at_flush(Scenario::new("S14", cfg, ops("c10;f;c62;p72")), 1)
}

/// S2b (DEC-80 Q1): `invalidateblock` below a durable flush, the next flush at the fork, with a
/// synced and with a syncing subscriber whose L lies above it.
pub fn s2b(sub: SubConfig, bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub: Some(sub),
        bug,
        ..ToyConfig::default()
    };
    let w = if matches!(sub.mode, SubMode::Synced { .. }) {
        "c20;f;x15;f"
    } else {
        "c20;f;s21;K;x15;f"
    };
    at_flush(Scenario::new("S2b", cfg, ops(w)), 1)
}

/// Whether the subscriber's rewind commit to `height` precedes the second flush's B1 (`Durable`).
pub fn rewound_before_b1(sc: &Scenario<ToyChain>, t: &Trace, height: u32) -> bool {
    let r = reference(sc);
    for m in t.marks() {
        match m {
            Mark::Locator {
                tip,
                kind: LocatorKind::Rewind,
                ..
            } if r.height(tip) == Some(height) => return true,
            Mark::Durable {
                flush: 2,
            } => return false,
            _ => {}
        }
    }
    false
}

/// S10: an import hardlinks `blk00000.dat` after the flush at 10; the next append makes ST2
/// replace it by a private copy (copy, `sync_data`, rename, directory sync).
pub fn s10() -> Scenario<ToyChain> {
    let mut sc = Scenario::new("S10", ToyConfig::default(), ops("c10;f;L0;c5;f"));
    sc.window = from_mark(MarkKind::Log, 0);
    sc
}

/// S10b: another process, which never syncs, writes blocks 11-70 across the rollover to file
/// 1; the toy adopts its files, then flushes. ST2's barrier syncs the current pair whatever it
/// knows, so adoption is what covers file 0's records and file 1's directory entry.
pub fn s10b(bug: Option<ToyBug>) -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        bug,
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S10b", cfg, ops("c10;f;I60;f")), 1)
}

/// S1's prefix with blocks 11-12 unsynced past the durable info at a crash.
pub fn s1_trailing(bug: ToyBug) -> Scenario<ToyChain> {
    let mut sc = s1(Some(bug));
    sc.ops = ops("c10;f;c2");
    sc.window = from_mark(MarkKind::Connected, 12);
    sc
}

/// S11c: the synced index is durable at 10, then disabled; pruning passes its locator; on the
/// restart its catch-up path is pruned, and it refuses as Core does (sanctioned).
pub fn s11c() -> Scenario<ToyChain> {
    let cfg = prune_sub(SubConfig::synced(0), None, None);
    at_flush(Scenario::new("S11c", cfg, ops("c10;f;D;c62;p72")), 1)
}

/// Each planted bug's witness scenario and the invariants it must report: the "Caught by" and
/// "Also" columns of §6.2's table (errata I24: T7's).
pub fn planted(bug: ToyBug) -> (Scenario<ToyChain>, Vec<InvariantId>) {
    use ToyBug::*;
    let kinds = |mut sc: Scenario<ToyChain>, k: &[CrashKind]| {
        sc.kinds = k.to_vec();
        sc
    };
    let pl = [CrashKind::PowerLoss];
    let both = [CrashKind::ProcessKill, CrashKind::PowerLoss];
    match bug {
        T1 => (at_flush(s1(Some(T1)), 1), vec![R1, D1a]),
        T2 => (at_flush(s1(Some(T2)), 0), vec![R1, D1a]),
        T3 => (s4(Some(T3)), vec![R3]),
        T4 => (s4(Some(T4)), vec![R0, R2]),
        T5 => (kinds(s5(Some(T5)), &both), vec![R0, D1e]),
        T6 => (s6(Some(T6)), vec![R1, D1a]),
        T7 => (s4_nested(T7), vec![R7a, R0, R2]),
        T8 => (s7(Some(T8)), vec![R6, R3]),
        T9 => {
            let mut sc = s4(Some(T9));
            let sync = Fault {
                at: nth(StoreSel::Fs, OpKind::SyncData, 0),
                kind: FaultKind::Io,
                outcome: FaultOutcome::Applied,
            };
            sc.faults = FaultPlan(vec![sync]);
            (sc, vec![R8, R1])
        }
        T10 => (at_flush(s1(Some(T10)), 1), vec![R11a, D1b]),
        T11 => (kinds(s5(Some(T11)), &pl), vec![R0, D1e]),
        T12 => (at_flush(s1(Some(T12)), 0), vec![R1, D1a]),
        T13 => (s5(Some(T13)), vec![R0, D1b]),
        T14 => (at_flush(s1(Some(T14)), 1), vec![R1, R12]),
        T15 => (at_flush(s5c(Some(T15)), 2), vec![R0, D1b]),
        T16 => (s11(Some(T16)), vec![R11b, D1b]),
        T17 => (s4(Some(T17)), vec![R3]),
        T18 => (s4(Some(T18)), vec![R3, D1f]),
        T19 => (s5b(Some(T19)), vec![R0, D1b]),
        T20 => (s7(Some(T20)), vec![R6]),
        T21 => {
            let mut sc = s7(Some(T21));
            sc.ops = ops("c20;f;c3;f");
            (sc, vec![R6, R3])
        }
        T22 => (kinds(at_flush(s1(Some(T22)), 1), &pl), vec![R11c, D1f]),
        T23a => (kinds(s1_trailing(T23a), &pl), vec![R1]),
        T23b => {
            let mut sc = kinds(s1_trailing(T23b), &both);
            sc.nested = Nested::Depth1;
            (sc, vec![R1, D1a])
        }
        T24 => (kinds(s4(Some(T24)), &[CrashKind::ProcessKill]), vec![R0, R2]),
        T25 => {
            let mut sc = s5(Some(T25));
            sc.faults = FaultPlan(vec![bk_fault(&s5(None))]);
            (sc, vec![R8, D1e])
        }
        T26 => (s4(Some(T26)), vec![R3, D1f]),
        T27 => (s4_nested(T27), vec![R7a, D1f, R3]),
        T28 => {
            // Images from the index-only write (the last Durable append) recover to 120 and
            // activate 121-130 with the init prune check due.
            let mut sc = s13(Some(T28));
            sc.window = from_last_durable_append(&sc);
            (sc, vec![R11b, D1b])
        }
        T29 => (at_flush(s1(Some(T29)), 2), vec![R12, D1f]),
        T30 => (s11r(Some(T30)), vec![R11b, D1b]),
        T31 => (at_flush(s1(Some(T31)), 1), vec![R5, R6]),
        T32 => (s14(Some(T32)), vec![FA1, FA2, FA5]),
        T33 => (kinds(s11s(Some(T33)), &pl), vec![R11a, D1b]),
        T34 => (at_flush(s4b(Some(T34)), 1), vec![R3, D1f]),
        T35 => (s11q(Some(T35)), vec![FA3, FA5]),
        T36 => (kinds(s11r(Some(T36)), &pl), vec![R11c]),
        T37 => (s11d(2, Some(T37)), vec![FA6]),
        T38 => (kinds(s11d(2, Some(T38)), &pl), vec![R11c]),
        T39 => (s14b(Some(T39)), vec![FA2]),
        T40 => (s14b(Some(T40)), vec![FA2]),
        T41 => (s11d(1, Some(T41)), vec![R11c]),
        T42 => (s2b(SubConfig::synced(0), Some(T42)), vec![R11a, D1b]),
        N8 | NoAdopt | ArchiveDropped | StateAtProgress | LinkedWrite => {
            panic!("{bug:?} has no PR-gate witness here")
        }
    }
}

/// The planted bugs with a checked-in repro: T1-T42 with T23a/b.
pub const PLANTED: [ToyBug; 43] = {
    use ToyBug::*;
    [
        T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12, T13, T14, T15, T16, T17, T18, T19, T20,
        T21, T22, T23a, T23b, T24, T25, T26, T27, T28, T29, T30, T31, T32, T33, T34, T35, T36, T37,
        T38, T39, T40, T41, T42,
    ]
};

pub const TOY_REGRESS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/power_loss/regress/toy");

/// S4 with the outer crash fixed right after the flush's B1 returned (the marker durable) and a
/// nested crash after every recovery event (T7, T27).
pub fn s4_nested(bug: ToyBug) -> Scenario<ToyChain> {
    let mut sc = s4(Some(bug));
    sc.window = only_at(after(MarkKind::Durable, 0));
    sc.nested = Nested::Depth1;
    sc
}

/// A random ToyChain scenario for the seeded sweep (§4.6): a random configuration (subscriber
/// class, split flushes, effects, prune mode) and 3-9 ops (connects, flushes, index writes,
/// shallow reorgs, prune requests, the syncing subscriber's steps), ending in a flush.
pub fn random_toy(
    rng: &mut dashd_store::faulty::Seeded,
    bug: Option<ToyBug>,
) -> Scenario<ToyChain> {
    let sub = match rng.below(4) {
        0 => None,
        1 => Some(SubConfig::synced(0)),
        2 => Some(SubConfig::synced(2)),
        _ => Some(SubConfig::syncing(25)),
    };
    let prune = rng.chance(300);
    let cfg = ToyConfig {
        sub_batches: if rng.chance(500) {
            3
        } else {
            0
        },
        effects: if rng.chance(300) {
            vec![3, 9, 17]
        } else {
            Vec::new()
        },
        prune: prune.then(PruneConfig::default),
        sub,
        bug,
        ..ToyConfig::default()
    };
    let syncing = matches!(sub.map(|s| s.mode), Some(SubMode::Syncing { .. }));
    let mut h = 0u32;
    let mut out = Vec::new();
    for _ in 0..3 + rng.below(7) {
        let op = match rng.below(10) {
            0..=3 => {
                let n = 1 + rng.below(if prune {
                    40
                } else {
                    15
                }) as u32;
                h += n;
                format!("c{n}")
            }
            4 | 5 => "f".into(),
            6 => "i".into(),
            7 if h > 2 => {
                // A reorg onto a branch with more work, as Core makes them (a reorg onto less
                // work is `invalidateblock`, which marks the old branch failed).
                let fork = h - 1 - rng.below(u64::from(h.min(5) - 1)) as u32;
                let len = h - fork + 1 + rng.below(3) as u32;
                h = fork + len;
                format!("r{fork}:{len}")
            }
            8 if prune => format!("p{h}"),
            9 if syncing => {
                if rng.chance(500) {
                    "K".into()
                } else {
                    format!("s{}", 1 + rng.below(30))
                }
            }
            _ => "f".into(),
        };
        out.push(op);
    }
    out.push("f".into());
    let mut sc = Scenario::new("sweep", cfg, ops(&out.join(";")));
    sc.window = CrashWindow::all();
    sc
}

/// S3: the first flush (old null), 10 blocks; the subscriber has no locator until its first
/// commit.
pub fn s3() -> Scenario<ToyChain> {
    let cfg = ToyConfig {
        sub: Some(SubConfig::synced(0)),
        ..ToyConfig::default()
    };
    at_flush(Scenario::new("S3", cfg, ops("c10;f")), 0)
}

/// S8: disk full mid-flush (ENOSPC on B1, a coin sub-batch, and a block write).
pub fn s8() -> Scenario<ToyChain> {
    let mut sc = s4(None);
    sc.name = "S8";
    sc.window = CrashWindow::all();
    let nospace = |store: StoreSel, op: OpKind, n: u32| Fault {
        at: nth(store, op, n),
        kind: FaultKind::NoSpace,
        outcome: FaultOutcome::NotApplied,
    };
    let chain = || StoreSel::Engine("chainstate".into());
    sc.faults = FaultPlan(vec![
        nospace(chain(), OpKind::Append(CommitKind::Durable), 0),
        nospace(chain(), OpKind::Append(CommitKind::Buffered), 1),
        nospace(StoreSel::Fs, OpKind::WriteAt, 9),
    ]);
    sc
}

/// S9 on S4's window: EIO per call, then read faults in recovery.
pub fn s9() -> Vec<Scenario<ToyChain>> {
    let mut calls = s4(None);
    calls.name = "S9";
    calls.faults = FaultPlan(s9_faults());
    let mut reads = s4(None);
    reads.name = "S9-reads";
    let read = |store: StoreSel, n: u32| Fault {
        at: nth(store, OpKind::Read, n),
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    };
    reads.recovery_faults =
        vec![read(StoreSel::Engine("chainstate".into()), 3), read(StoreSel::Fs, 0)];
    vec![calls, reads]
}

/// S12 (N5): ProcessKill → PowerLoss with a buffered effect (12) and unsynced blk appends
/// (11-15) after the flush at 10; nested depth 1.
pub fn s12() -> Scenario<ToyChain> {
    let mut sc = s1(None);
    sc.name = "S12";
    sc.ops = ops("c10;f;c5");
    sc.window = from_mark(MarkKind::Durable, 0);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    sc
}

/// N7: `allocate` fails silently while the data still fits; S1's window (20, 22].
pub fn n7() -> Scenario<ToyChain> {
    let mut sc = s1(None);
    sc.name = "N7";
    sc.allocate = AllocateMode::Fail;
    // The bound is the crash state's; R12's continuation overwrites block 21's first sector and
    // so forces sector cells too (S1 runs R12).
    sc.continue_r12 = false;
    sc.window = CrashWindow {
        from: after(MarkKind::Locator, 1),
        to: Some(after(MarkKind::Connected, 22)),
    };
    sc
}

/// N9: S5 with a synced mandatory subscriber.
pub fn n9() -> Scenario<ToyChain> {
    let mut sc = s5(None);
    sc.cfg.sub = Some(SubConfig::synced(0));
    sc.name = "N9";
    sc
}

/// Every clean scenario of §6.2 (the nightly matrix runs each under `Realistic` and
/// `Extended`).
pub fn clean_matrix() -> Vec<Scenario<ToyChain>> {
    let mut v = vec![
        s1(None),
        s2(None),
        s3(),
        s3p(None),
        s4(None),
        s4b(None),
        s5(None),
        s5b(None),
        s5c(None),
        s6(None),
        s7(None),
        s8(),
        s10(),
        s10b(None),
        s11(None),
        s11r(None),
        s11s(None),
        s11q(None),
        s11c(),
        s12(),
        s13(None),
        s14(None),
        s14b(None),
        n7(),
        n9(),
    ];
    v.extend(s9());
    v.extend((0..=2).map(|b| s11d(b, None)));
    v.extend(
        [SubConfig::synced(0), SubConfig::synced(2), SubConfig::syncing(80)]
            .map(|sub| s2b(sub, None)),
    );
    v
}

// ---------------------------------------------------------------------------------------------
// Production-scale prune and locks (§8.4): K = 288, lock buffer 10, `D_max` 1,024, 3 KiB blocks
// (21 per 64 KiB file). File 0 holds heights 0-20, so a prune at 309 can select it.

/// A production-scale prune configuration.
pub fn prod_cfg(sub: Option<SubMode>, target: Option<u64>) -> ToyConfig {
    ToyConfig {
        block_size: 3072,
        sub_batches: 5,
        prune: Some(PruneConfig {
            k: 288,
            buffer: 10,
            target,
        }),
        sub: sub.map(|mode| SubConfig {
            mode,
            d_max: 1024,
        }),
        ..ToyConfig::default()
    }
}

/// One record and its undo at production scale: a 3,080-byte blk record and a 280-byte rev one.
pub const PROD_PER_BLOCK: u64 = 3080 + 280;

/// S3p, S5, S5b, S5c, S11, S11q, S13, S14 and S14b at production scale.
pub fn prod_scenarios() -> Vec<Scenario<ToyChain>> {
    let synced = |b| {
        Some(SubMode::Synced {
            backlog: b,
        })
    };
    let syncing = Some(SubMode::Syncing {
        cadence: 1000,
    });
    let sc = |name, cfg, w: &str, flush| at_flush(Scenario::new(name, cfg, ops(w)), flush);
    let s14b_target = (331 - 21) * PROD_PER_BLOCK + BUFFER + 516;
    vec![
        sc("S3p-prod", prod_cfg(synced(0), None), "c309;p309", 0),
        sc("S5-prod", prod_cfg(None, None), "c5;f;c35;i;c269;p309", 1),
        sc("S5b-prod", prod_cfg(None, None), "c5;f;c304;p309", 1),
        sc("S5c-prod", prod_cfg(None, None), "c290;f;c20;q400;F10:0;c10;f", 1),
        sc("S11-prod", prod_cfg(syncing, None), "c10;f;c320;s11;K;s290;q330;f", 1),
        sc("S11q-prod", prod_cfg(synced(2), None), "c40;f;c269;p309", 1),
        sc("S13-prod", prod_cfg(syncing, Some(1)), "c10;f;c310;s11;K;s200;f;a10;i", 1),
        sc("S14-prod", prod_cfg(synced(0), Some(1)), "c10;f;c300;p310", 1),
        sc(
            "S14b-prod",
            prod_cfg(None, Some(s14b_target)),
            &format!("c330;A{s14b_target};F0:1;c1"),
            0,
        ),
    ]
}
