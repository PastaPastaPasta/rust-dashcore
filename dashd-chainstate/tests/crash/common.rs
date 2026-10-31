//! Shared CRASH scenarios and search helpers (CH2 §6.4, §7.2's b1 row) for `crash.rs` (PR gate).

#![allow(dead_code, unused_imports)]

pub use InvariantId::*;
pub use dashd_chainstate::crash::{
    CHAIN_STORE, ChainConfig, ChainOp, ChainRef, ChainSut, SUB_STORE, SubConfig, SubMode, ops,
};
pub use dashd_store::faulty::{
    CommitKind, CrashKind, CrashWindow, EventSel, Fault, FaultKind, FaultOutcome, FaultPlan, Hash,
    InvariantId, Mark, MarkKind, Mode, Nested, OpKind, Reference, Report, Repro, Scenario,
    StoreSel, Sut, Trace, TraceEntry, Violation, explore, forward_trace, replay, replay_trace,
};

pub type Sc = Scenario<ChainSut>;

/// The point right after the `occurrence`-th mark of a kind.
pub fn after(mark: MarkKind, occurrence: u32) -> EventSel {
    EventSel::AfterMark {
        mark,
        occurrence,
        n: 0,
    }
}

/// The window from the `n`-th flush's begin to the end.
pub fn from_flush(n: u32) -> CrashWindow {
    CrashWindow {
        from: after(MarkKind::FlushBegin, n),
        to: None,
    }
}

pub fn at_flush(mut sc: Sc, n: u32) -> Sc {
    sc.window = from_flush(n);
    sc
}

pub const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 1024,
};

/// `coins_replay_blind`'s PR cases and seed, for the smoke and its controls.
pub const BLIND_CASES: u64 = 200;
pub const BLIND_SEED: u64 = 0xb11d;

/// The forward run's own violations must be clean; returns its trace.
pub fn forward(sc: &Sc) -> Trace {
    let (t, v) = forward_trace(sc);
    assert!(v.is_empty(), "{}: the forward run: {v:?}", sc.name);
    t
}

pub fn clean<S: Sut>(sc: &Scenario<S>) -> Report {
    let r = explore(sc, EXHAUSTIVE);
    eprintln!("{}: {}", sc.name, r.summary());
    assert!(r.is_clean(), "{}: {}", sc.name, r.summary());
    assert_eq!(r.truncated, 0, "{}: truncated images", sc.name);
    r
}

/// `sc`'s forward run with `fault` (the harness's own forward run has none): its trace, and the
/// message of the abort that stopped it.
pub fn faulted(sc: &Sc, fault: Fault) -> (Trace, Option<String>) {
    use dashd_store::faulty::{World, WorldConfig};
    let world = World::new(WorldConfig {
        faults: vec![fault],
        ..WorldConfig::default()
    });
    let mut sut = match ChainSut::open(&world, &sc.cfg) {
        Ok(s) => s,
        Err(a) => return (world.trace(), Some(a.message)),
    };
    let err = sc.ops.iter().find_map(|o| sut.apply(o).err()).map(|a| a.message);
    (world.trace(), err)
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

/// S3: the first flush (old null), 10 blocks.
pub fn s3() -> Sc {
    at_flush(Scenario::new("S3", ChainConfig::default(), ops("c10;f")), 0)
}

/// A configuration with S = `sub` bytes, else the defaults.
pub fn split(sub: u64) -> ChainConfig {
    ChainConfig {
        sub_batch: Some(sub),
        ..ChainConfig::default()
    }
}

/// S: 20 blocks' coins in 6 partial sub-batches and a final one.
pub const SUB: u64 = 1000;

/// The window of the single point `at`.
pub fn only_at(at: EventSel) -> CrashWindow {
    CrashWindow {
        from: at.clone(),
        to: Some(at),
    }
}

/// The `n`-th `op` on a store.
pub fn nth(store: StoreSel, op: OpKind, n: u32) -> EventSel {
    EventSel::Nth {
        store,
        op,
        n,
    }
}

/// An EIO at the `n`-th `op` on `store`.
pub fn io(store: StoreSel, op: OpKind, n: u32, outcome: FaultOutcome) -> Fault {
    Fault {
        at: nth(store, op, n),
        kind: FaultKind::Io,
        outcome,
    }
}

pub fn chain() -> StoreSel {
    StoreSel::Engine(CHAIN_STORE.into())
}

/// C1 split: S3's first flush in sub-batches.
pub fn s3_split() -> Sc {
    let mut sc = s3();
    sc.name = "S3-split";
    sc.cfg = split(SUB / 2);
    sc
}

/// S1 (C5): 30 blocks, one-batch flushes at 10/20/30, an index-only write at 15, independent
/// effects at 12 and 22, a presumed effect at 25 read by block 31's fold (R12's continuation).
pub fn s1() -> Sc {
    let cfg = ChainConfig {
        effects: vec![12, 22],
        presumed: Some((25, 31)),
        ..ChainConfig::default()
    };
    Scenario::new("S1", cfg, ops("c10;f;c5;i;c5;f;c10;f"))
}

/// S1 with an effect at 5 too, before the first flush (DEC-202): a kill before that flush's tip
/// is durable leaves the effect's record with no tip.
pub fn s1_early() -> Sc {
    let mut sc = s1();
    sc.name = "S1-early";
    sc.cfg.effects.insert(0, 5);
    sc
}

/// S4 (C2): a split flush of 20 blocks.
pub fn s4() -> Sc {
    at_flush(Scenario::new("S4", split(SUB), ops("c20;f")), 0)
}

/// S4b (C2): two split flushes, 0 → 20 and 20 → 40.
pub fn s4b() -> Sc {
    at_flush(Scenario::new("S4b", split(SUB), ops("c20;f;c20;f")), 0)
}

/// S4 with the outer crash fixed right after the flush's B1 returned (the marker durable) and a
/// nested crash after every recovery event (C4).
pub fn s4_nested() -> Sc {
    let mut sc = s4();
    sc.name = "S4-nested";
    sc.window = only_at(after(MarkKind::Durable, 0));
    sc.nested = Nested::Depth1;
    sc
}

/// S7 (C11's CH2 half): an invalid block (22) persisted before connection, one valid block
/// connected before B1, a validation-only connect, then a split flush.
pub fn s7() -> Sc {
    let cfg = ChainConfig {
        sub_batch: Some(SUB / 4),
        invalid: vec![22],
        ..ChainConfig::default()
    };
    at_flush(Scenario::new("S7", cfg, ops("c20;f;a3;v;c1;f")), 1)
}

/// Core mode (CH3a a3): `sc` with V's connect through the real `ConnectBlock` on regtest blocks.
pub fn core(mut sc: Sc, name: &'static str) -> Sc {
    sc.name = name;
    sc.cfg.core = true;
    sc
}

/// The height of S7-core's invalid block, B: above coinbase maturity, so its bad signature is
/// what the real `ConnectBlock` finds.
pub const CORE_B: u32 = 122;

/// S7-core (C11's connect half, and T20's and T21's, CH3a a3): S7 in core mode, 100 blocks
/// higher. B, accepted with `VALID_TRANSACTIONS`, is persisted before any connect of it, and
/// activation's connect of it after a crash is the real `ConnectBlock`'s, which fails its script
/// check.
pub fn s7_core() -> Sc {
    let mut sc = core(s7(), "S7-core");
    sc.cfg.invalid = vec![CORE_B];
    sc.ops = ops("c120;f;a3;v;c1;f");
    sc
}

/// C11-core (CH3 §4.6 steps 1–3, CH3a a3): one-batch flushes; B accepted with its neighbours
/// after the first flush and persisted by an index-only write, which no connect of it precedes.
/// Every image from the flush's B1 on.
pub fn c11_core() -> Sc {
    let cfg = ChainConfig {
        core: true,
        invalid: vec![CORE_B],
        ..ChainConfig::default()
    };
    let mut sc = Scenario::new("C11-core", cfg, ops("c120;f;a3;i"));
    sc.window = CrashWindow {
        from: after(MarkKind::Durable, 0),
        to: None,
    };
    sc
}

/// S1-core (CH3a a3): S1 in core mode, every connect the real `ConnectBlock`'s, its undo
/// records the ones it streams (empty: below coinbase maturity no block spends a coin).
pub fn s1_core() -> Sc {
    core(s1(), "S1-core")
}

/// C3-real's fork (CH3 §4.6): above coinbase maturity, so each block of either branch spends the
/// coinbase 100 blocks below it, a coin both branches hold, and from its second block its
/// parent's A; B's first block spends the fork's A, which A's disconnect restores.
pub const C3_FORK: u32 = 110;

/// C3-real's S: every flush of the grid splits, so B1's marker is durable inside it and recovery
/// replays the transition. F writes 1, 2 or 7 partial sub-batches for k = 1, 2 or 6 (the connect
/// leg's 1, 1 or 4), the closing flush 2 to 11.
pub const C3_SUB: u64 = 300;

/// The leg of C3-real's reorg that F is in flight across, or none (`Activation`).
#[derive(Clone, Copy, Debug)]
pub enum Leg {
    /// F frozen at A's tip with V's disconnect to the fork inside it (`D k`); B after it.
    Disconnect,
    /// A durable at its tip and V's disconnect with no flush; then F frozen at the fork with B's
    /// connects inside it (`F k+1`), so F's transition starts below `old` (C3 (b)).
    Connect,
    /// F frozen at A's tip with the whole reorg inside it (`R fork:k+1`).
    Both,
    /// A durable at its tip, the reorg with no flush, then an index-only write (`i`) before the
    /// closing flush: an image between them holds B's connected entries above coins at A, so
    /// activation's own reorg disconnects A through the real `DisconnectBlock`. F is A's flush.
    Activation,
}

/// C3-real (CH3 §4.6, CH3b b4a): CH2's C3 in core mode, every connect and disconnect the real
/// `ConnectBlock` and `DisconnectBlock` on regtest blocks, every flush split ([`C3_SUB`]).
/// Durable at the fork, branch A `k` blocks above it, then a reorg to branch B (`k + 1` blocks)
/// with F in flight across `leg`, and a closing flush. Every image from F's begin, both crash
/// kinds; a nested crash at every recovery event, and a second during the recovery from one in
/// ten of those (depth 2 sampled); and R12 on every branch, whose closing flush holds the coins
/// after activation to the reference's.
pub fn c3_real(k: u32, leg: Leg) -> Sc {
    let (fork, b) = (C3_FORK, k + 1);
    let (workload, f) = match leg {
        Leg::Disconnect => (format!("c{fork};f;c{k};D{k};r{fork}:{b};f"), 1),
        Leg::Connect => (format!("c{fork};f;c{k};f;r{fork}:0;F{b};f"), 2),
        Leg::Both => (format!("c{fork};f;c{k};R{fork}:{b};f"), 1),
        Leg::Activation => (format!("c{fork};f;c{k};f;r{fork}:{b};i;f"), 1),
    };
    let name = Box::leak(format!("C3-real {leg:?} k={k}").into_boxed_str());
    let cfg = ChainConfig {
        core: true,
        sub_batch: Some(C3_SUB),
        ..ChainConfig::default()
    };
    let mut sc = at_flush(Scenario::new(name, cfg, ops(&workload)), f);
    sc.nested = Nested::Depth2Sample(100);
    sc.r12_all = true;
    sc
}

/// C6: V ahead. A split flush during whose files barrier V accepts and connects 3 more blocks
/// (`HAVE_DATA` above `new`, their index entries dirty for Bk's d2), then the next flush.
pub fn c6() -> Sc {
    at_flush(Scenario::new("C6", split(SUB), ops("c20;F3;c2;f")), 0)
}

/// S8 (C7): disk full mid-flush (ENOSPC on B1, a coin sub-batch, and a block write).
pub fn s8() -> Sc {
    let mut sc = s4();
    sc.name = "S8";
    sc.window = CrashWindow::all();
    let nospace = |store: StoreSel, op: OpKind, n: u32| Fault {
        at: nth(store, op, n),
        kind: FaultKind::NoSpace,
        outcome: FaultOutcome::NotApplied,
    };
    sc.faults = FaultPlan(vec![
        nospace(chain(), OpKind::Append(CommitKind::Durable), 0),
        nospace(chain(), OpKind::Append(CommitKind::Buffered), 1),
        nospace(StoreSel::Fs, OpKind::WriteAt, 9),
    ]);
    sc
}

/// The events of `sc`'s forward run from its first flush on.
pub fn flush_events(sc: &Sc) -> u32 {
    let t = forward(sc);
    t.entries
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
        .count() as u32
}

/// S9 (C7): EIO, applied and not, on every call of S4's flush; then read faults in recovery:
/// chainstate read 3 (the `pruned_files` meta key), block-file read 0 (the load's open of
/// blk00000.dat, BL-127: "Error loading block database"; one open per blk file the index holds,
/// one here) and 1 (the replay's first block read).
pub fn s9() -> Vec<Sc> {
    let n = flush_events(&s4());
    assert!(n > 10, "the flush has {n} events");
    let mut calls = s4();
    calls.name = "S9";
    calls.faults = FaultPlan(
        (0..n)
            .flat_map(|n| {
                [FaultOutcome::Applied, FaultOutcome::NotApplied].map(|outcome| Fault {
                    at: EventSel::AfterMark {
                        mark: MarkKind::FlushBegin,
                        occurrence: 0,
                        n,
                    },
                    kind: FaultKind::Io,
                    outcome,
                })
            })
            .collect(),
    );
    let mut reads = s4();
    reads.name = "S9-reads";
    let read = |store: StoreSel, n: u32| Fault {
        at: nth(store, OpKind::Read, n),
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    };
    reads.recovery_faults = vec![read(chain(), 3), read(StoreSel::Fs, 0), read(StoreSel::Fs, 1)];
    vec![calls, reads]
}

/// A recovery fault of `kind` at the `n`-th `op` on `store`.
pub fn recovery_fault(
    store: StoreSel,
    op: OpKind,
    n: u32,
    kind: FaultKind,
    outcome: FaultOutcome,
) -> Fault {
    Fault {
        at: nth(store, op, n),
        kind,
        outcome,
    }
}

/// S9's recovery half (C7): EIO and ENOSPC, applied and not, on S4's replay writes (`sync_wal` 0
/// and 1, buffered appends 0 and 3, the durable append 0); and read faults in S1's activation
/// after its index-only write, where blocks 11–15 have data above the coins' tip 10 (the coins
/// fetches are chainstate reads 11 and 12, after recovery's 11; block-file reads 1 and 5, after
/// the load's one open per blk file the index holds, here blk00000.dat, BL-127).
pub fn s9_recovery() -> Vec<Sc> {
    let mut writes = s4();
    writes.name = "S9-recovery-writes";
    let calls = [
        (OpKind::SyncWal, 0),
        (OpKind::SyncWal, 1),
        (OpKind::Append(CommitKind::Buffered), 0),
        (OpKind::Append(CommitKind::Buffered), 3),
        (OpKind::Append(CommitKind::Durable), 0),
    ];
    for (op, n) in calls {
        for kind in [FaultKind::Io, FaultKind::NoSpace] {
            for outcome in [FaultOutcome::Applied, FaultOutcome::NotApplied] {
                writes.recovery_faults.push(recovery_fault(chain(), op, n, kind, outcome));
            }
        }
    }
    let mut reads = s1();
    reads.name = "S9-activation-reads";
    // Right after the index-only write's barrier (the 22nd event after the first flush's B1).
    reads.window = only_at(EventSel::AfterMark {
        mark: MarkKind::Durable,
        occurrence: 0,
        n: 21,
    });
    let read =
        |store, n| recovery_fault(store, OpKind::Read, n, FaultKind::Io, FaultOutcome::NotApplied);
    reads.recovery_faults =
        vec![read(chain(), 11), read(chain(), 12), read(StoreSel::Fs, 1), read(StoreSel::Fs, 5)];
    vec![writes, reads]
}

/// S12 (N5, C4): ProcessKill → PowerLoss with a buffered effect (12) and unsynced blk appends
/// (11-15) after the flush at 10; nested depth 1.
pub fn s12() -> Sc {
    let mut sc = s1();
    sc.name = "S12";
    sc.ops = ops("c10;f;c5");
    sc.window = CrashWindow {
        from: after(MarkKind::Durable, 0),
        to: None,
    };
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    sc
}

/// C9 in W: `-dbcrashratio`'s exit after the `k`-th partial sub-batch of S4's flush; every image
/// up to the exit, ProcessKill only.
pub fn c9_flush(k: u32) -> Sc {
    let mut sc = s4();
    sc.name = "C9-flush";
    sc.cfg.crash_after = Some(k);
    sc.kinds = vec![CrashKind::ProcessKill];
    sc
}

/// C9 in recovery: the marker durable, then the exit after the `k`-th partial sub-batch of the
/// replay. Not nested: R7a compares a nested recovery with the uninterrupted one, which here
/// exits by design; the ProcessKill states after every replay sub-batch are C4's nested images.
pub fn c9_replay(k: u32) -> Sc {
    let mut sc = s4();
    sc.name = "C9-replay";
    sc.cfg.crash_replay = Some(k);
    sc.window = only_at(after(MarkKind::Durable, 0));
    sc.kinds = vec![CrashKind::ProcessKill];
    sc
}

/// C12: `-reindex-chainstate`'s wipe after two flushes, then a flush of the rebuilt chainstate;
/// a crash at every event of the wipe, of its resume (nested), and of the flush.
pub fn c12() -> Sc {
    let mut sc = Scenario::new("C12", split(SUB), ops("c10;f;c5;f;W;f"));
    sc.window = CrashWindow {
        from: after(MarkKind::Durable, 1),
        to: None,
    };
    sc.nested = Nested::Depth1;
    sc
}

// ---------------------------------------------------------------------------------------------
// Pruning (C10) at production scale: `MIN_BLOCKS_TO_KEEP` is 288, and under `-fastprune` file 0
// holds heights 0–122 and file 1 heights 123–243 (the preconditions pin both).

/// `-prune=1`: manual pruning (`p h`) until `A t` sets an automatic target; S = `sub` bytes, or
/// one-batch flushes with `None`.
pub fn prune(sub: Option<u64>) -> ChainConfig {
    ChainConfig {
        prune: Some(0),
        sub_batch: sub,
        ..ChainConfig::default()
    }
}

/// S for the prune scenarios: S5's flush of 405 blocks' coins in two partial sub-batches.
pub const PRUNE_SUB: Option<u64> = Some(50_000);

/// ST2's room under an automatic target: a 16 MiB blk chunk and a 1 MiB rev chunk.
pub const BUFFER: u64 = (16 << 20) + (1 << 20);

/// S5: coins flushed at 5, an index-only write at 105, then `pruneblockchain 410`'s split flush
/// (bound 122), which selects file 0.
pub fn s5() -> Sc {
    at_flush(Scenario::new("S5", prune(PRUNE_SUB), ops("c5;f;c100;i;c305;p410")), 1)
}

/// S5p: S5 one-batch, so the clears go in Bp, not Bk.
pub fn s5p() -> Sc {
    at_flush(Scenario::new("S5p", prune(None), ops("c5;f;c100;i;c305;p410")), 1)
}

/// S5b: S5 with no index-only write: blocks 6–410 are newly dirty at B1.
pub fn s5b() -> Sc {
    at_flush(Scenario::new("S5b", prune(PRUNE_SUB), ops("c5;f;c405;p410")), 1)
}

/// S5c (C6): V ahead. An automatic target no usage meets, then flush 1 at 525 (prior flush 515),
/// during whose files barrier V connects to 535: the bound from `new` (237) selects file 0, V's
/// tip (bound 247) would add file 1. Then the flush at 545.
pub fn s5c() -> Sc {
    let w = format!("c515;f;c10;A{};F10;c10;f", BUFFER + 1);
    let mut sc = Scenario::new("S5c", prune(PRUNE_SUB), ops(&w));
    sc.window = CrashWindow {
        from: after(MarkKind::FlushBegin, 1),
        to: Some(after(MarkKind::FlushBegin, 2)),
    };
    sc
}

/// S5d (DEC-211): S5c's target with V rewinding inside the flush. The automatic decision at 410
/// selects file 0 (bound 122); during the files barrier V disconnects 410, so it answers step 7
/// at 409 (bound 121) and declines. The flush prunes nothing and re-arms, and the check at 410's
/// reconnect selects file 0. `rewind` false puts `F0` in `D1`'s place.
pub fn s5d(rewind: bool) -> Sc {
    let op = if rewind {
        "D1"
    } else {
        "F0"
    };
    let w = format!("c400;f;c10;A{};{op};c2;f", BUFFER + 1);
    let mut sc = Scenario::new("S5d", prune(PRUNE_SUB), ops(&w));
    sc.window = CrashWindow {
        from: after(MarkKind::FlushBegin, 1),
        to: Some(after(MarkKind::FlushBegin, 3)),
    };
    sc
}

/// The flush of each `PruneSelected` of a trace, in order.
pub fn selection_flushes(t: &Trace) -> Vec<u64> {
    t.marks()
        .filter_map(|m| match m {
            Mark::PruneSelected {
                flush,
                ..
            } => Some(*flush),
            _ => None,
        })
        .collect()
}

/// The usage of heights `from..=to` on a chain of `c{to}`: their blk and rev records.
pub fn usage(from: u32, to: u32) -> u64 {
    let cfg = prune(PRUNE_SUB);
    let mut r = <ChainRef as Reference>::new(&cfg);
    for o in ops(&format!("c{to}")) {
        r.apply(&o);
    }
    r.active()[from as usize..=to as usize]
        .iter()
        .map(|id| {
            let b = r.block(id).expect("a block");
            b.data_len() + b.undo_len()
        })
        .sum()
}

/// S14b: an automatic target that removing file 0 meets at 540 by one byte; V connects 541
/// between the decision and the selection, so the selection {0} leaves it unmet and re-arms; the
/// next check, at 542's call site, selects {1}.
pub fn s14b() -> Sc {
    let target = usage(123, 540) + BUFFER + 1;
    at_flush(
        Scenario::new("S14b", prune(PRUNE_SUB), ops(&format!("c530;f;c10;A{target};F1;c1"))),
        1,
    )
}

/// `sc` with its window from the first prune selection to the end.
pub fn from_selection(mut sc: Sc) -> Sc {
    sc.window = CrashWindow {
        from: after(MarkKind::PruneSelected, 0),
        to: None,
    };
    sc
}

/// `sc` with its window from the first unlink to the end.
pub fn from_unlink(mut sc: Sc) -> Sc {
    sc.window = CrashWindow {
        from: after(MarkKind::BestEffortBegin, 0),
        to: None,
    };
    sc
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

/// The flush and files of each `PruneDeclined` of a trace, in order (DEC-211).
pub fn declines(t: &Trace) -> Vec<(u64, Vec<u32>)> {
    t.marks()
        .filter_map(|m| match m {
            Mark::PruneDeclined {
                flush,
                files,
            } => Some((*flush, files.clone())),
            _ => None,
        })
        .collect()
}

/// The bound and re-arm of each `PruneSelected` of a trace.
pub fn selection_bounds(t: &Trace) -> Vec<(u32, bool)> {
    t.marks()
        .filter_map(|m| match m {
            Mark::PruneSelected {
                bound,
                rearmed,
                ..
            } => Some((*bound, *rearmed)),
            _ => None,
        })
        .collect()
}

/// An `Applied` EIO on Bk: the chainstate's Durable append after the prune's selection.
pub fn bk_fault(sc: &Sc) -> Fault {
    let t = forward(sc);
    let mut n = 0;
    for e in &t.entries {
        match e {
            TraceEntry::Event {
                store,
                op: OpKind::Append(CommitKind::Durable),
                ..
            } if store == CHAIN_STORE => n += 1,
            TraceEntry::Mark {
                mark: Mark::PruneSelected {
                    ..
                },
                ..
            } => return io(chain(), OpKind::Append(CommitKind::Durable), n, FaultOutcome::Applied),
            _ => {}
        }
    }
    panic!("{}: no selection", sc.name);
}

pub use dashd_chainstate::crash::CoinsSut;

/// The coins profile: one-batch flushes across a reorg below the durable tip (V's undo), every
/// event from the first flush on.
pub fn coins_profile() -> Scenario<CoinsSut> {
    let mut sc = Scenario::new("coins", ChainConfig::default(), ops("c10;f;c10;f;r15:3;f;c2;f"));
    sc.window = from_flush(0);
    sc
}

// ---------------------------------------------------------------------------------------------
// b3: the subscriber-bearing scenarios (§7.2's b3 cell), ST3a's toy ones on `ChainSut` with a
// real `SubscriberDriver`. The prune ones run at the toy's production scale (its `-prod`
// variants), heights past 20 moved up by 101 for `PRUNE_AFTER_HEIGHT` (100).

/// `cfg` with b3's test subscriber.
pub fn indexed(mut cfg: ChainConfig, index: SubConfig) -> ChainConfig {
    cfg.index = Some(index);
    cfg
}

/// A subscriber with the toy's production `D_max` (1024).
pub fn prod(mut index: SubConfig) -> SubConfig {
    index.d_max = 1024;
    index
}

/// Prune mode (`target` bytes, 0 for manual only) with a subscriber. With an automatic target
/// the automatic checks start past `MIN_BLOCKS_TO_KEEP` (`pah=300`), where the locks bind (D16).
pub fn prune_sub(index: SubConfig, target: u64) -> ChainConfig {
    let mut cfg = indexed(prune(PRUNE_SUB), index);
    cfg.prune = Some(target);
    if target > 0 {
        cfg.prune_after = Some(300);
    }
    cfg
}

fn named(mut sc: Sc, name: &'static str) -> Sc {
    sc.name = name;
    sc
}

/// S1 with a synced subscriber.
pub fn s1_sub() -> Sc {
    let mut sc = named(s1(), "S1-sub");
    sc.cfg.index = Some(SubConfig::synced(0));
    sc
}

/// S4 and S4b with a synced subscriber.
pub fn s4_sub() -> Sc {
    let mut sc = named(s4(), "S4-sub");
    sc.cfg.index = Some(SubConfig::synced(0));
    sc
}

pub fn s4b_sub() -> Sc {
    let mut sc = named(s4b(), "S4b-sub");
    sc.cfg.index = Some(SubConfig::synced(0));
    sc
}

/// S2: a reorg of depth 3 inside the third flush window, a synced subscriber with backlog 2
/// (prune mode, for Core's moveback lines; nothing is pruned).
pub fn s2() -> Sc {
    at_flush(
        Scenario::new("S2", prune_sub(SubConfig::synced(2), 0), ops("c10;f;c10;f;c5;r22:8;f")),
        1,
    )
}

/// S3p: a fresh pruned IBD to 410, the first flush one-batch with pruning; a synced subscriber.
pub fn s3p() -> Sc {
    let cfg = prune_sub(prod(SubConfig::synced(0)), 0);
    at_flush(Scenario::new("S3p", cfg, ops("c410;p410")), 0)
}

/// S11: a syncing subscriber durable at 10 with progress 300; V at 431 and a manual prune at
/// 431 (bound 143), which the subscriber's lock at 10 keeps from file 0.
pub fn s11() -> Sc {
    let cfg = prune_sub(prod(SubConfig::syncing(1000)), 0);
    at_flush(Scenario::new("S11", cfg, ops("c10;f;c421;s11;k;s290;q431;f")), 1)
}

/// S11r: a syncing subscriber durable and processed at 70 on branch A; a reorg to B at 50, B to
/// 80, a flush; the subscriber then steps onto B.
pub fn s11r() -> Sc {
    let cfg = prune_sub(SubConfig::syncing(80), 0);
    at_flush(Scenario::new("S11r", cfg, ops("c70;f;s71;k;r50:30;q80;f;s51;f")), 1)
}

/// S11s: a syncing subscriber (cadence 25) from genesis while the chainstate is durable at 20
/// with V at 60: its cadence commits are capped at the durable tip; a flush at 60, then it
/// catches up and turns synced. (The toy flushes below V's tip, `F40:0`; V flushes its tip.)
pub fn s11s() -> Sc {
    let cfg = indexed(ChainConfig::default(), SubConfig::syncing(25));
    at_flush(Scenario::new("S11s", cfg, ops("c20;f;c40;s26;k;f;s25;s11;f")), 0)
}

/// S11c: a synced subscriber durable at 10, then disabled (`D`); V connects to 410 and a manual
/// prune at 410 passes its locator (file 0, blocks 0–119); on the restart its catch-up path is
/// pruned, and the open refuses as Core's `BaseIndex::Init` does (sanctioned). The toy's
/// `c10;f;D;c62;p72` at D21's scale.
pub fn s11c() -> Sc {
    let cfg = prune_sub(prod(SubConfig::synced(0)), 0);
    at_flush(Scenario::new("S11c", cfg, ops("c10;f;D;c400;p410")), 1)
}

/// S11c at the disable's boundary (D36): a Syncing subscriber at L = 5 with V at 410 is
/// disabled, and W's manual prune to 410 runs as soon as the deregistration returns.
pub fn s11c_w() -> Sc {
    let cfg = prune_sub(prod(SubConfig::syncing(1000)), 0);
    at_flush(Scenario::new("S11cW", cfg, ops("c10;f;c400;s5;Dp410;c1;f")), 1)
}

/// S11o (D45): a synced subscriber durable at 20 is disabled, `invalidateblock 15` and the flush
/// at the fork make G = 14 with no subscriber registered; the restart's open finds L = 20 off
/// G's path and rewinds it to 14 before it deletes the record and catches up.
pub fn s11o() -> Sc {
    let cfg = indexed(ChainConfig::default(), SubConfig::synced(0));
    at_flush(Scenario::new("S11o", cfg, ops("c20;f;D;x15;f")), 1)
}

/// S11q: a synced subscriber with backlog 2; flushes at 40 (no prune) and 410 (a manual prune).
pub fn s11q() -> Sc {
    let cfg = prune_sub(prod(SubConfig::synced(2)), 0);
    at_flush(Scenario::new("S11q", cfg, ops("c40;f;c370;p410")), 1)
}

/// S11d: the `D_max` cycle. Chainstate and locator durable at 0, `D_max` 96, backlog `b`; the
/// subscriber reaches 96 with V at 96 + b, a `DMax` flush is due (with a manual prune to 50 whose
/// dry run is empty).
pub fn s11d(b: u32) -> Sc {
    let cfg = prune_sub(SubConfig::synced(b), 1 << 30);
    at_flush(Scenario::new("S11d", cfg, ops(&format!("f;c{};q50;c1", 95 + b))), 1)
}

/// S13: a syncing subscriber durable at 10 (progress 210); the chainstate durable at 421 with
/// 422-431 `HAVE_DATA` through an index-only write; after a crash, activation connects 422-431
/// with init's prune check due under an automatic target.
pub fn s13() -> Sc {
    let cfg = prune_sub(prod(SubConfig::syncing(1000)), 1);
    at_flush(Scenario::new("S13", cfg, ops("c10;f;c411;s11;k;s200;f;a10;i")), 1)
}

/// S14: a synced subscriber durable at 10; V connects to 410 with automatic checks, then a manual
/// prune at 410.
pub fn s14() -> Sc {
    let cfg = prune_sub(prod(SubConfig::synced(0)), 1);
    at_flush(Scenario::new("S14", cfg, ops("c10;f;c400;p410")), 1)
}

/// N9: S5 with a synced subscriber.
pub fn n9() -> Sc {
    let mut sc = named(s5(), "N9");
    sc.cfg.index = Some(prod(SubConfig::synced(0)));
    sc
}

/// S2b (DEC-80 Q1): `invalidateblock` below a durable flush, the next flush at the fork, with a
/// synced and with a syncing subscriber whose L lies above it.
pub fn s2b(index: SubConfig) -> Sc {
    let w = if matches!(index.mode, SubMode::Synced { .. }) {
        "c20;f;x15;f"
    } else {
        "c20;f;s21;k;x15;f"
    };
    at_flush(Scenario::new("S2b", indexed(ChainConfig::default(), index), ops(w)), 1)
}
