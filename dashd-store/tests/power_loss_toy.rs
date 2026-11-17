//! ToyChain (ST3a design §6.2): scenarios, planted bugs T1–T41, negative controls, forward
//! assertions, witnesses.

#[path = "power_loss/toy_common.rs"]
mod common;
use common::*;
#[path = "power_loss/shard_check.rs"]
mod shard_check;

#[test]
fn n1_s1() {
    clean(&s1(None));
}

#[test]
fn n1_s4b() {
    clean(&s4b(None));
}

#[test]
fn n8_buffered_bk_behind_the_barrier_is_safe_on_one_flush() {
    clean(&s4(Some(ToyBug::N8)));
}

#[test]
fn t1_b1_before_the_files_barrier() {
    witness_planted(ToyBug::T1);
}

#[test]
fn t2_index_write_without_the_files_barrier() {
    witness_planted(ToyBug::T2);
}

#[test]
fn t3_marker_in_bk() {
    witness_planted(ToyBug::T3);
}

#[test]
fn t4_evo_in_bk() {
    witness_planted(ToyBug::T4);
}

#[test]
fn t14_b1_drops_the_block_files_record() {
    witness_planted(ToyBug::T14);
    // "Also D1(d)": R12's continuation writes at the stale size, over durably referenced records.
    let r = explore(&planted(ToyBug::T14).0, EXHAUSTIVE);
    assert!(
        r.findings
            .iter()
            .any(|f| f.violation.invariant == R12 && f.violation.detail.contains("D1d")),
        "{}",
        r.summary()
    );
}

#[test]
fn t17_b1_buffered() {
    witness_planted(ToyBug::T17);
}

#[test]
fn t18_no_pre_bk_barrier_and_bk_buffered() {
    witness_planted(ToyBug::T18);
}

#[test]
fn t26_bk_durable_without_the_pre_bk_barrier() {
    witness_planted(ToyBug::T26);
}

#[test]
fn t29_no_barrier_before_b1_with_a_presumed_effect() {
    witness_planted(ToyBug::T29);
}

#[test]
fn t34_buffered_bk_presumed_by_the_next_b1() {
    witness_planted(ToyBug::T34);
}

#[test]
fn n1_s4() {
    let r = clean(&s4(None));
    if std::env::var_os("ST3A_STATS").is_some() {
        for (at, kind, n, vars) in &r.per_image {
            if *n > 24 {
                eprintln!("{at} {kind:?} {n}: {vars:?}");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

// Pruning (§6.2 steps 0, 7, 10, 11; DEC-64), subscriber-free.

#[test]
fn s5_preconditions_and_clean() {
    let t = forward(&s5(None));
    assert_eq!(selections(&t), vec![vec![0]], "S5 selects file 0");
    let mut sc = s5(None);
    sc.min_images = 20;
    clean(&sc);
}

#[test]
fn s5b_preconditions() {
    assert_eq!(selections(&forward(&s5b(None))), vec![vec![0]]);
}

#[test]
fn s5c_preconditions() {
    let t = forward(&s5c(None));
    assert!(selections(&t).is_empty(), "the correct bound selects nothing");
    let t15 = forward_trace(&s5c(Some(ToyBug::T15))).0;
    assert_eq!(selections(&t15), vec![vec![0]], "T15's bound selects file 0");
}

#[test]
fn t5_unlink_before_bk() {
    witness_planted(ToyBug::T5);
}

#[test]
fn t11_buffered_bk_then_unlink() {
    witness_planted(ToyBug::T11);
}

#[test]
fn t13_selection_before_b1_clears_in_b1() {
    witness_planted(ToyBug::T13);
}

#[test]
fn t15_selection_against_v_tip() {
    witness_planted(ToyBug::T15);
}

#[test]
fn t19_selection_before_b1_omits_modified_records() {
    witness_planted(ToyBug::T19);
}

/// S9 on S5's dimensions: an `Applied` failure of Bk aborts with no unlink; EIO on a pruned
/// file's `remove` after Bk is best-effort (N6), the startup scan removes it later (R9).
#[test]
fn s9_prune_faults_and_n6_clean() {
    let mut sc = s5(None);
    let remove = Fault {
        at: nth(StoreSel::Fs, OpKind::Remove, 0),
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    };
    sc.faults = FaultPlan(vec![bk_fault(&s5(None)), remove]);
    clean(&sc);
}

#[test]
fn t25_unlink_after_a_failed_bk() {
    witness_planted(ToyBug::T25);
}

// ---------------------------------------------------------------------------------------------

// S14b: an automatic target, V growth between the decision and the selection.

#[test]
fn s14b_preconditions() {
    let t = forward(&s14b(None));
    // D = {0} and met, the selection is {0} and re-arms, the next due check selects {1}.
    assert_eq!(selections(&t), vec![vec![0], vec![1]]);
    let rearmed: Vec<bool> = t
        .marks()
        .filter_map(|m| match m {
            Mark::PruneSelected {
                rearmed,
                ..
            } => Some(*rearmed),
            _ => None,
        })
        .collect();
    assert!(rearmed[0]);
}

#[test]
fn t39_the_automatic_selector_rerun() {
    witness_planted(ToyBug::T39);
}

#[test]
fn t40_no_rearm_after_a_shortfall() {
    witness_planted(ToyBug::T40);
}

// ---------------------------------------------------------------------------------------------

// prune_selection_protocol (§6.2, GPT r4's fixtures): step 0's retained D and step 7's

// `D ∩ eligible(actual)` with re-arming, over ST2's public selectors.

mod selection_protocol {
    use dashd_store::faulty::toy::{final_selection, prune_dry_run, target_unmet};
    use dashd_store::faulty::{World, WorldConfig};
    use dashd_store::flatfile::{BlockFileInfo, BlockFilesConfig, BlockFilesState};

    use super::BUFFER;

    const K: u32 = 8;

    fn info(blocks: u32, first: u32, last: u32) -> BlockFileInfo {
        BlockFileInfo {
            blocks,
            size: blocks * 1032,
            undo_size: blocks * 280,
            height_first: first,
            height_last: last,
            time_first: u64::from(first),
            time_last: u64::from(last),
        }
    }

    fn usage(infos: &[BlockFileInfo]) -> u64 {
        infos.iter().map(|i| u64::from(i.size) + u64::from(i.undo_size)).sum()
    }

    fn cfg() -> BlockFilesConfig {
        let mut c = BlockFilesConfig::new("/dd/blocks", [0; 4]);
        c.fastprune = true;
        c.prune_mode = true;
        c
    }

    fn dry(
        infos: &[BlockFileInfo],
        new_h: u32,
        lock: Option<u32>,
        manual: Option<u32>,
        target: u64,
    ) -> (Vec<u32>, bool) {
        let state = BlockFilesState {
            infos: infos.to_vec(),
            last_file: infos.len() as u32 - 1,
        };
        prune_dry_run(
            World::new(WorldConfig::default()).fs(),
            cfg(),
            state,
            new_h,
            K,
            lock,
            manual,
            Some(target),
        )
    }

    /// Side branch: closed files with last heights 62 and 40, then the current file; the target
    /// is met by deleting one closed file.
    #[test]
    fn side_branch() {
        let infos = [info(63, 0, 62), info(11, 30, 40), info(1, 63, 600)];
        let target = usage(&infos) - usage(&infos[..1]) + BUFFER + 1000;
        // Step 0 under the predicted bound 597: D = {0}, met.
        let (d, met) = dry(&infos, 597 + K, None, None, target);
        assert_eq!((d.clone(), met), (vec![0], true));
        // Step 7 under the actual lock 47: nothing eligible, and the check re-arms.
        let sel = final_selection(&d, &infos, 2, 47);
        assert!(sel.is_empty());
        assert!(met && target_unmet(usage(&infos), Some(target)), "re-armed");
        // The next check selects {1}.
        let (d2, _) = dry(&infos, 597 + K, Some(47), None, target);
        assert_eq!(final_selection(&d2, &infos, 2, 47), vec![1]);
        // T39: re-running the automatic selector under min(predicted, actual) picks {1},
        // outside D.
        let (t39, _) = dry(&infos, 597 + K, Some(47), None, target);
        assert!(!t39.iter().all(|f| d.contains(f)), "T39 leaves D: {t39:?}");
        // The manual control: {0, 1}, then {1} under the lock: a subset either way.
        let (m, _) = dry(&infos, 597 + K, None, Some(597), target);
        assert_eq!(m, vec![0, 1]);
        assert_eq!(final_selection(&m, &infos, 2, 47), vec![1]);
    }

    /// V ahead: files with last heights 62, 125, 188 and the current one; the target is 516 bytes
    /// above the usage left once file 0 goes; one 1,032-byte append between step 0 and step 7.
    #[test]
    fn v_ahead() {
        let mut infos =
            vec![info(63, 0, 62), info(63, 63, 125), info(63, 126, 188), info(12, 189, 200)];
        let target = usage(&infos) - usage(&infos[..1]) + BUFFER + 516;
        let (d, met) = dry(&infos, 200, None, None, target);
        assert_eq!((d.clone(), met), (vec![0], true));
        infos[3].size += 1032;
        let sel = final_selection(&d, &infos, 3, 200 - K);
        assert_eq!(sel, vec![0]);
        let after = usage(&infos) - usage(&infos[..1]);
        assert!(target_unmet(after, Some(target)), "short by 516 bytes: re-armed");
        // T39: the re-run selector picks {0, 1}, outside D.
        let (t39, _) = dry(&infos, 200, None, None, target);
        assert_eq!(t39, vec![0, 1]);
    }
}

// ---------------------------------------------------------------------------------------------

// S3, S6-S9 and the rest of the non-subscriber bugs.

/// S3: the first flush (old null), 10 blocks; the subscriber has no locator until its first
/// commit.
#[test]
fn n1_s3() {
    clean(&s3());
}

#[test]
fn n1_s6() {
    clean(&s6(None));
}

#[test]
fn t6_barrier_syncs_only_the_current_pair() {
    witness(s6(Some(ToyBug::T6)), &[R1, D1a]);
}

#[test]
fn n1_s7() {
    clean(&s7(None));
}

#[test]
fn t8_recovery_replays_every_block_with_data() {
    witness_planted(ToyBug::T8);
}

#[test]
fn t20_b1_before_the_blocks_were_connected() {
    witness_planted(ToyBug::T20);
}

#[test]
fn t21_an_invalid_block_in_the_marker_path() {
    witness_planted(ToyBug::T21);
}

/// S8: disk full mid-flush (ENOSPC on B1, on a coin sub-batch, and on a block write).
#[test]
fn n1_s8_disk_full() {
    clean(&s8());
}

#[test]
fn n1_s9_eio_per_call_and_recovery_reads() {
    for sc in s9() {
        clean(&sc);
    }
}

#[test]
fn t9_continue_after_a_failed_sync() {
    witness_planted(ToyBug::T9);
}

#[test]
fn t12_have_undo_before_the_undo_write() {
    witness_planted(ToyBug::T12);
}

#[test]
fn t24_evo_committed_after_bk() {
    witness_planted(ToyBug::T24);
}

#[test]
fn t31_recovery_erases_index_entries_above_the_tip() {
    witness_planted(ToyBug::T31);
}

// ---------------------------------------------------------------------------------------------

// Subscribers (§6.2 "Subscriber data", §5.6 item 6; DEC-64, DEC-80 Q1).

#[test]
fn t10_locator_before_b1_returns() {
    witness_planted(ToyBug::T10);
}

#[test]
fn t22_locator_without_its_sync_wal() {
    witness_planted(ToyBug::T22);
}

#[test]
fn s2_moveback_lines_and_clean() {
    let t = forward(&s2(None));
    // V disconnects 25, 24, 23 with the subscriber two events behind: 25's logs nothing, the
    // last names the fork.
    let lines = logs(&t, "prune lock moved back to");
    assert_eq!(
        lines,
        ["toyindex prune lock moved back to 23", "toyindex prune lock moved back to 22"]
    );
    clean(&s2(None));
}

#[test]
fn s3p_preconditions_and_clean() {
    let sc = s3p(None);
    let t = forward(&sc);
    assert_eq!(selections(&t), vec![vec![0]]);
    assert_eq!(commits(&sc, &t), vec![(72, LocatorKind::CommitStep)]);
    let (bound, lock) = t
        .marks()
        .find_map(|m| match m {
            Mark::PruneSelected {
                bound,
                lock,
                ..
            } => Some((*bound, *lock)),
            _ => None,
        })
        .expect("a selection");
    assert_eq!(bound, 64, "new − K binds the selection");
    assert_eq!(lock, Some(69), "the lock is not null after step 6");
    clean(&sc);
}

#[test]
fn n9_s5_with_a_synced_subscriber() {
    let sc = n9();
    let t = forward(&sc);
    assert_eq!(selections(&t), selections(&forward(&s5(None))), "the same files as S5");
    clean(&sc);
}

#[test]
fn s11_preconditions_and_clean() {
    let sc = s11(None);
    let t = forward(&sc);
    assert_eq!(commits(&sc, &t), vec![(10, LocatorKind::Cadence)]);
    assert!(selections(&t).is_empty(), "the durable lock (7) selects nothing");
    let t16 = forward_trace(&s11(Some(ToyBug::T16))).0;
    assert_eq!(selections(&t16), vec![vec![0]], "the in-memory lock (67) selects file 0");
    clean(&sc);
}

#[test]
fn t16_lock_from_the_in_memory_height() {
    witness_planted(ToyBug::T16);
}

#[test]
fn s11r_preconditions() {
    let sc = s11r(None);
    let t = forward(&sc);
    assert!(selections(&t).is_empty(), "the lock at the fork (47) selects nothing");
    let t30 = forward_trace(&s11r(Some(ToyBug::T30))).0;
    assert_eq!(selections(&t30), vec![vec![0]], "the lock at L's height (67) selects file 0");
    assert_eq!(
        commits(&sc, &t),
        vec![
            (70, LocatorKind::Cadence),
            (50, LocatorKind::Rewind),
            (80, LocatorKind::Cadence),
            (80, LocatorKind::Cadence),
            (80, LocatorKind::CommitStep)
        ],
        "the rewind commit at the fork, catch-up on its cadence, the capped commit as it turns \
         synced, then the commit step"
    );
}

#[test]
fn t30_lock_at_the_locator_height() {
    witness_planted(ToyBug::T30);
}

#[test]
fn t36_buffered_rewind_commit() {
    witness_planted(ToyBug::T36);
}

#[test]
fn s11s_preconditions_and_clean() {
    let sc = s11s(None);
    let t = forward(&sc);
    // Capped at 20 (progress 25, T_ack 20) and 40 (progress 50, T_ack 40); turning synced at 60
    // it is capped at 40 again (no new commit), then the flush at 60 commits there.
    assert_eq!(
        commits(&sc, &t),
        vec![
            (20, LocatorKind::Cadence),
            (40, LocatorKind::Cadence),
            (40, LocatorKind::Cadence),
            (60, LocatorKind::CommitStep)
        ]
    );
    let t33 = forward_trace(&s11s(Some(ToyBug::T33))).0;
    assert_eq!(
        commits(&s11s(Some(ToyBug::T33)), &t33),
        vec![
            (25, LocatorKind::Cadence),
            (50, LocatorKind::Cadence),
            (60, LocatorKind::Cadence),
            (60, LocatorKind::CommitStep)
        ]
    );
    clean(&sc);
}

#[test]
fn t33_uncapped_cadence_commit() {
    witness_planted(ToyBug::T33);
    forward_finds(s11s(Some(ToyBug::T33)), FA4);
}

/// FA4's liveness half counts only progress a capped commit could cover: before any flush sets
/// `T_ack` a syncing subscriber cannot commit, so 29 steps past cadence 25 are no violation (the
/// 2 × 10⁴-case sweep, I34).
#[test]
fn fa4_waits_for_t_ack() {
    let cfg = ToyConfig {
        sub: Some(SubConfig::syncing(25)),
        ..ToyConfig::default()
    };
    forward(&Scenario::new("FA4-before-T_ack", cfg, ops("c34;s29;f")));
}

#[test]
fn s11q_preconditions_and_clean() {
    let sc = s11q(None);
    let t = forward(&sc);
    assert_eq!(
        commits(&sc, &t),
        vec![(40, LocatorKind::CommitStep), (72, LocatorKind::CommitStep)]
    );
    assert_eq!(selections(&t), vec![vec![0]]);
    let t35 = forward_trace(&s11q(Some(ToyBug::T35))).0;
    assert!(commits(&sc, &t35).is_empty() && selections(&t35).is_empty());
    clean(&sc);
}

#[test]
fn t35_synced_classified_by_has_processed_new() {
    let (_, v) = forward_trace(&s11q(Some(ToyBug::T35)));
    for inv in [FA3, FA5] {
        assert!(v.iter().any(|x| x.invariant == inv), "{inv:?}: {v:?}");
    }
}

#[test]
fn s11d_preconditions_and_clean() {
    for b in 0..=2 {
        let sc = s11d(b, None);
        let t = forward(&sc);
        let want: Vec<(u32, LocatorKind)> = if b == 0 {
            vec![(0, LocatorKind::CommitStep), (96, LocatorKind::CommitStep)]
        } else {
            vec![
                (0, LocatorKind::CommitStep),
                (96, LocatorKind::Checkpoint),
                (96 + b, LocatorKind::CommitStep),
            ]
        };
        assert_eq!(commits(&sc, &t), want, "b = {b}: FA6, every run reaches its commit");
        // The DMax flush is the only full-flush cause; the manual dry run is empty.
        let causes: Vec<_> = t
            .marks()
            .filter_map(|m| match m {
                Mark::PruneDecision {
                    due: PruneDue::Manual(50),
                    dry_run,
                    full_flush,
                    ..
                } => Some((dry_run.clone(), full_flush.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(causes, vec![(vec![], vec![FlushCause::DMax])]);
        // Staged data: s = 49 and p = 96 at the limit; the checkpoint submits through 96, then
        // syncs, then commits.
        let r = reference(&sc);
        let h = |x: &Option<Hash>| x.as_ref().and_then(|x| r.height(x));
        let at_limit = t.marks().find_map(|m| match m {
            Mark::SubscriberState {
                progress,
                submitted,
                ..
            } if h(progress) == Some(96) => Some(h(submitted)),
            _ => None,
        });
        assert_eq!(at_limit, Some(Some(49)), "b = {b}: the half-interval batch covers 1-49");
        if b > 0 {
            let mut seq = Vec::new();
            let mut on = false;
            for e in &t.entries {
                match e {
                    TraceEntry::Mark {
                        mark:
                            Mark::Durable {
                                ..
                            },
                        ..
                    } => on = true,
                    TraceEntry::Mark {
                        mark:
                            Mark::Locator {
                                ..
                            },
                        ..
                    } if on => break,
                    TraceEntry::Event {
                        store,
                        op,
                        ..
                    } if on && store == "subscriber" => seq.push(*op),
                    _ => {}
                }
            }
            assert_eq!(
                seq,
                [
                    OpKind::Append(CommitKind::Buffered),
                    OpKind::SyncWal,
                    OpKind::Append(CommitKind::Durable),
                    OpKind::Barrier
                ],
                "b = {b}: submit, sync_wal, then the checkpoint"
            );
        }
        clean(&sc);
    }
}

#[test]
fn t37_limit_only_makes_a_flush_due() {
    for b in 1..=2 {
        forward_finds(s11d(b, Some(ToyBug::T37)), FA6);
    }
}

#[test]
fn t38_buffered_checkpoint() {
    witness_planted(ToyBug::T38);
}

#[test]
fn t41_checkpoint_without_submitting_staged_data() {
    for b in 1..=2 {
        witness(s11d(b, Some(ToyBug::T41)), &[R11c]);
    }
}

#[test]
fn s13_preconditions() {
    assert!(selections(&forward(&s13(None))).is_empty());
    // Activation connects 121-130 with init's prune check due: under the locks loaded at open
    // nothing is selected; with T28's late lock, file 0.
    assert!(recovery_selections(&s13(None)).iter().all(|s| s.is_empty()));
    assert_eq!(recovery_selections(&s13(Some(ToyBug::T28))), vec![vec![0]]);
}

#[test]
fn t28_lock_registered_after_activation() {
    witness_planted(ToyBug::T28);
}

#[test]
fn s14_preconditions_and_clean() {
    let sc = s14(None);
    let t = forward(&sc);
    let flushes = t.marks().filter(|m| matches!(m, Mark::FlushBegin { .. })).count();
    assert_eq!(flushes, 2, "the automatic checks are empty and cause no flush");
    assert_eq!(selections(&t), vec![vec![0]]);
    assert_eq!(
        logs(&t, "limited pruning").last().unwrap(),
        "toyindex limited pruning to height 69"
    );
    clean(&sc);
}

#[test]
fn t32_decision_under_the_pre_commit_lock() {
    let sc = s14(Some(ToyBug::T32));
    let (t, v) = forward_trace(&sc);
    assert!(selections(&t).is_empty(), "the durable lock (7) selects nothing");
    for inv in [FA1, FA2, FA5] {
        assert!(v.iter().any(|x| x.invariant == inv), "{inv:?}: {v:?}");
    }
}

#[test]
fn s2b_rewind_gate_clean() {
    // Backlog 2 makes the gate drain queued disconnects (backlog 0 has none queued).
    for sub in [SubConfig::synced(0), SubConfig::synced(2), SubConfig::syncing(80)] {
        let sc = s2b(sub, None);
        let t = forward(&sc);
        assert!(rewound_before_b1(&sc, &t, 14), "{sub}: the gate rewinds to 14 before B1");
        let t42 = forward_trace(&s2b(sub, Some(ToyBug::T42))).0;
        assert!(!rewound_before_b1(&sc, &t42, 14), "{sub}: T42's B1 comes first");
        clean(&sc);
    }
}

#[test]
fn t42_backward_b1_without_the_rewind_gate() {
    for sub in [SubConfig::synced(0), SubConfig::syncing(80)] {
        witness(s2b(sub, Some(ToyBug::T42)), &[R11a, D1b]);
    }
}

// ---------------------------------------------------------------------------------------------

// Imports, adoption and hardlinks (S10, S10b, T23a/b), effects (R13), nesting (S12, N5), the

// silent allocation failure (N7), and the disabled index (S11c).

#[test]
fn n1_s10_hardlinked_blk_file() {
    let t = forward(&s10());
    let ops: Vec<OpKind> =
        t.events().filter(|(_, s, _, _)| *s == "fs").map(|(_, _, o, _)| o).collect();
    for op in [OpKind::Copy, OpKind::Rename] {
        assert!(ops.contains(&op), "the link-count rule copies and renames: {op:?}");
    }
    clean(&s10());
}

#[test]
fn n1_s10b_adopt_existing_files() {
    clean(&s10b(None));
}

/// S10b's non-vacuity: without `adopt_existing_files` the flush references unsynced bytes.
#[test]
fn s10b_without_adoption_is_caught() {
    let mut sc = s10b(Some(ToyBug::NoAdopt));
    sc.kinds = vec![CrashKind::PowerLoss];
    witness(sc, &[R1, D1a]);
}

/// T23a: the adopter reads only each record's header, then syncs; the unread sectors' cells are
/// carried into the durable image (L2), so D1(a) is silent and R1's read forces them.
#[test]
fn t23a_adopter_reads_only_the_header() {
    witness_planted(ToyBug::T23a);
    // D1(a) is silent: after a power loss the surviving sectors are the durable image, whatever
    // they hold, so the adopted range is durable; only R1's read of the body sees it torn.
    let r = explore(&planted(ToyBug::T23a).0, EXHAUSTIVE);
    assert!(r.findings.iter().all(|f| f.violation.invariant != D1a), "{}", r.summary());
}

/// T23b: after a ProcessKill the adopter commits blocks 11-12 without syncing, and a nested
/// PowerLoss loses them.
#[test]
fn t23b_adopter_commits_without_syncing() {
    witness_planted(ToyBug::T23b);
}

/// S12 (N5): ProcessKill → PowerLoss with a buffered effect (12) and unsynced blk appends
/// (11-15) after the flush at 10; nested depth 1. R7b applies, not R7a.
#[test]
fn n5_s12_mixed_kinds() {
    clean(&s12());
}

/// N7: `allocate` fails silently while the data still fits; S1's window (20, 22] holds two
/// pending records (erratum I4's bound: 216 branches per image).
#[test]
fn n7_silent_allocation_failure() {
    let sc = n7();
    // 9 blk sizes × 4 rev sizes × the boundary sectors R1 forces when it reads block 20's
    // record and undo, which share a sector with block 21's (3 and 2 versions). The effect bit
    // is not forced: R13 reads only effects a completed barrier covers.
    let r = clean(&sc);
    assert!(r.max_branches_per_image <= 9 * 4 * 3 * 2, "{}", r.summary());
}

#[test]
fn s11c_preconditions() {
    // The prune passes the disabled index's locator (10): file 0 is selected.
    assert_eq!(selections(&forward(&s11c())), vec![vec![0]]);
    // Re-enabled after a restart, it refuses as Core does.
    let (v, t) = killed_at_end(&s11c());
    assert!(v.is_empty(), "{v:?}");
    assert_eq!(logs(&t, "goes beyond pruned data").len(), 1);
}

// ---------------------------------------------------------------------------------------------

// The planted bugs' witness scenarios, and their checked-in repros (§4.8; §8.2's replays).

#[test]
fn checked_in_toy_repros_replay() {
    for bug in PLANTED {
        let path = format!("{TOY_REGRESS}/{bug:?}.repro");
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let r = Repro::parse(&text).unwrap();
        let expect = planted(bug).1[0];
        assert_eq!(r.expect, vec![expect], "{bug:?}");
        assert_eq!(Repro::parse(&r.to_text()).unwrap(), r);
        let got = replay::<ToyChain>(&r).unwrap();
        assert!(got.iter().any(|v| v.invariant == expect), "{bug:?}: {got:?}");
    }
}

// ---------------------------------------------------------------------------------------------

// DEC-80 Q1 (a)'s controls: R11c sees the archive rows and the state key.

#[test]
fn dec80_archive_rows_of_a_rewind_are_required() {
    witness(s2b(SubConfig::synced(0), Some(ToyBug::ArchiveDropped)), &[R11c]);
}

#[test]
fn dec80_state_key_is_the_commit_targets() {
    let mut sc = s11s(Some(ToyBug::StateAtProgress));
    sc.kinds = vec![CrashKind::ProcessKill];
    witness(sc, &[R11c]);
}

/// T22 on S2's reorg (backlog 2): the archive rows written on disconnects above L are lost with
/// the unsynced batches while the locator survives (DEC-80 Q1 (a)'s archive rule).
#[test]
fn t22_archive_rows_on_a_reorg() {
    let mut sc = s2(Some(ToyBug::T22));
    sc.kinds = vec![CrashKind::PowerLoss];
    witness(sc, &[R11c]);
}

// ---------------------------------------------------------------------------------------------

// Regressions from the Phase C review.

/// A reorg below L with a synced subscriber: the next interval's rewind commit is not step 6's
/// commit (FA3 once took it for one).
#[test]
fn n1_reorg_below_the_locator() {
    for (b, w) in [(0, "c20;f;r15:8;f"), (2, "c20;f;c1;r15:10;f")] {
        let cfg = ToyConfig {
            sub: Some(SubConfig::synced(b)),
            ..ToyConfig::default()
        };
        clean(&at_flush(Scenario::new("reorg-below-L", cfg, ops(w)), 1));
    }
}

/// Recovery's catch-up past `D_max`: activation connects 11-110 behind a locator at 10; the
/// catch-up stops at 106, makes a `DMax` flush, commits and finishes (it once stalled).
#[test]
fn n1_recovery_catch_up_past_d_max() {
    let cfg = ToyConfig {
        sub: Some(SubConfig::synced(0)),
        ..ToyConfig::default()
    };
    let mut sc = Scenario::new("catch-up", cfg, ops("c10;f;a100;i"));
    sc.window = from_last_durable_append(&sc);
    sc.kinds = vec![CrashKind::ProcessKill];
    clean(&sc);
}

/// R10's control: a write into the hardlinked inode is caught.
#[test]
fn r10_a_write_into_the_linked_inode_is_caught() {
    let mut sc = s10();
    sc.cfg.bug = Some(ToyBug::LinkedWrite);
    witness(sc, &[R10]);
}

// ---------------------------------------------------------------------------------------------
// Nested recovery (§4.2, Phase D): T7 and T27 on S4, the outer crash fixed at the durable
// marker, every inner crash point and kind.

#[test]
fn t7_recovery_erases_the_marker_first() {
    witness_planted(ToyBug::T7);
}

#[test]
fn t27_recovery_final_batch_without_a_barrier() {
    witness_planted(ToyBug::T27);
}

// ---------------------------------------------------------------------------------------------
// Assumption witnesses (§6.5): each breaks one assumption, or grants one bonus, at a time.

fn adversary(w: WitnessSet) -> Adversary {
    Adversary::Witness(w)
}

/// W1: granting A-PREFIX (DEC-62 withdrew it), the bugs that depend only on the false prefix
/// assumption leave no post-crash violation; D1(f), which is unconditional, still fires for
/// T18, T22, T26, T27 and T34. T17 is still caught.
#[test]
fn w1_prefix_hides_the_prefix_bugs() {
    use ToyBug::*;
    let prefix = adversary(WitnessSet {
        grant_prefix: true,
        ..Default::default()
    });
    for bug in [T18, T22, T26, T27, T34, T36, T38] {
        let (mut sc, _) = planted(bug);
        sc.adversary = prefix;
        let r = explore(&sc, EXHAUSTIVE);
        let diagnostic = matches!(bug, T18 | T22 | T26 | T27 | T34);
        // D1(f), also when R12's continuation repeats the buggy step (tagged R12).
        let is_d1f = |f: &dashd_store::faulty::Finding| {
            f.violation.invariant == D1f
                || (f.violation.invariant == R12 && f.violation.detail.contains(": D1f:"))
        };
        assert!(
            r.findings.iter().all(|f| diagnostic && is_d1f(f)) && r.truncated == 0,
            "{bug:?} under W1: {}",
            r.summary()
        );
        if diagnostic {
            assert!(r.found().contains(&D1f), "{bug:?}: D1(f) still fires: {}", r.summary());
        }
    }
    let (mut sc, _) = planted(T17);
    sc.adversary = prefix;
    witness(sc, &[R4, R3]);
}

/// W2: breaking A-ATOMIC (each column family keeps its own subset of a batch, an in-flight
/// Durable B1 included), the correct toy fails R3 on S4.
#[test]
fn w2_per_cf_subsets_break_s4() {
    let mut sc = s4(None);
    sc.adversary = adversary(WitnessSet {
        break_atomic: true,
        ..Default::default()
    });
    witness(sc, &[R3]);
}

/// W3: breaking A-REN (a rename's two names survive independently), ST2's private copy of a
/// hardlinked file can leave neither name bound: S10 fails R1.
#[test]
fn w3_non_atomic_rename_breaks_s10() {
    let mut sc = s10();
    sc.adversary = adversary(WitnessSet {
        break_rename: true,
        ..Default::default()
    });
    witness(sc, &[R1]);
}

/// W5: breaking A-PSOW (a power loss may zero the synced unit a pending write shares), the
/// correct toy fails R1 on S1.
#[test]
fn w5_shared_tail_sector_breaks_s1() {
    let mut sc = s1(None);
    sc.adversary = adversary(WitnessSet {
        break_psow: true,
        ..Default::default()
    });
    witness(sc, &[R1]);
}

// ---------------------------------------------------------------------------------------------
// The seeded sweep (§4.6): a smoke run of the correct toy, and a planted bug it must find. The
// nightly runs 2 × 10⁴ cases (I32).

#[test]
fn sweep_smoke_is_clean() {
    let r = dashd_store::faulty::sweep(30, 7, &|rng| random_toy(rng, None));
    eprintln!("sweep: {}", r.summary());
    assert!(r.is_clean() && r.branches > 15, "{}", r.summary());
}

#[test]
fn sweep_finds_a_planted_bug() {
    let r = dashd_store::faulty::sweep(20, 11, &|rng| random_toy(rng, Some(ToyBug::T1)));
    let f = r.first(R1).unwrap_or_else(|| panic!("{}", r.summary()));
    let got = replay::<ToyChain>(&f.repro).unwrap();
    assert!(got.iter().any(|v| v.invariant == R1), "the sweep's repro replays: {got:?}");
}

// ---------------------------------------------------------------------------------------------
// CP2.1 controls (reviews/ST3a-cp2-sol.md) that use this round's additions.

/// CP2-5's negative control: connects below the assumevalid height legitimately skip script
/// checks, and R6 accepts them.
#[test]
fn r6_accepts_skipped_scripts_below_assumevalid() {
    let mut sc = s1(None);
    sc.cfg.assumevalid = Some(15);
    let t = forward(&sc);
    let skipped = t
        .marks()
        .filter(|m| {
            matches!(
                m,
                Mark::Connected {
                    scripts: dashd_store::faulty::ScriptCheck::Skipped,
                    ..
                }
            )
        })
        .count();
    assert_eq!(skipped, 16, "blocks 0-15 skip their script checks");
    // The flush at 20 publishes skipped (11-15) and full (16-20) connects.
    sc.window = CrashWindow {
        from: after(MarkKind::FlushBegin, 1),
        to: Some(after(MarkKind::FlushBegin, 2)),
    };
    clean(&sc);
}

/// CP2-3's controls: a fault on recovery's first engine read, and on its first filesystem read,
/// reaches the system (a sanctioned abort), not the store-level check before it.
#[test]
fn first_recovery_reads_take_the_fault() {
    use dashd_store::faulty::{Sut, World, WorldConfig};
    let read0 = |store: StoreSel| Fault {
        at: nth(store, OpKind::Read, 0),
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    };
    // The engine: recovery's first read is its own, and it aborts with Core's message for an
    // engine error inside `LoadChainstate` (DEC-165 (3)).
    let cfg = ToyConfig::default();
    let w = World::new(WorldConfig::default());
    let mut t = ToyChain::open(&w, &cfg).unwrap();
    t.apply(&ToyOp::Connect(10)).unwrap();
    t.apply(&ToyOp::Flush).unwrap();
    let w = w.capture().boot(
        CrashKind::ProcessKill,
        Box::new(dashd_store::faulty::AllSurvive),
        WorldConfig {
            faults: vec![read0(StoreSel::Engine("chainstate".into()))],
            ..WorldConfig::default()
        },
    );
    let checked = w.checking(|| ToyChain::store_check(&w, &cfg));
    assert!(checked.is_empty(), "the store check is clean: {checked:?}");
    match ToyChain::open(&w, &cfg) {
        Err(a) => assert_eq!(a.message, "Error opening block database"),
        Ok(_) => panic!("recovery did not take the fault"),
    }
    // The filesystem: S4 crashed with its marker durable replays blocks; the first block read
    // fails (the search requires that the fault fires) and recovery stops sanctioned.
    for store in [StoreSel::Engine("chainstate".into()), StoreSel::Fs] {
        let mut sc = s4(None);
        sc.window = only_at(after(MarkKind::Durable, 0));
        sc.recovery_faults = vec![read0(store)];
        clean(&sc);
    }
}

// ---------------------------------------------------------------------------------------------
// Regressions for the sweep's findings (erratum I31).

/// Recovery activation reorgs to the most-work branch: after an index-only write that holds
/// both branches of `c8;r6:3`, a crash before any flush must activate B to 9, not A to 8.
#[test]
fn activation_reorgs_to_the_most_work_branch() {
    let cfg = ToyConfig::default();
    let mut sc = Scenario::new("most-work", cfg, ops("c8;r6:3;i"));
    sc.window = CrashWindow::all();
    clean(&sc);
}

/// Recovery activation's disconnects restore the evo state below the recovered tip (EvoDB's
/// undo): after `c6;f;r4:4;i`, a crash after the index write boots at 6 on A and reorgs to B, and
/// the continuation's flush must write B's state (the 2 × 10⁴-case sweep, I34).
#[test]
fn activation_reorg_restores_the_evo_state_below_the_tip() {
    let cfg = ToyConfig::default();
    let mut sc = Scenario::new("most-work-evo", cfg, ops("c6;f;r4:4;i"));
    sc.window = CrashWindow::all();
    clean(&sc);
}

/// Shards cover the unsharded search exactly, R12 included (DEC-185 (2)): R12 runs on each
/// image's all-survived branch, which only the all-zero-prefix shard holds. S1 under
/// `Extended` around event 53, where a power loss can leave a block unreadable: the first branch
/// of the shard with prefix 1 has the block unreadable, its continuation fail-stops, and running
/// R12 there reported a false R12.
#[test]
fn shards_match_the_search_with_r12() {
    let mut sc = s1(None);
    sc.adversary = Adversary::Extended;
    sc.window = CrashWindow {
        from: EventSel::Index(52),
        to: Some(EventSel::Index(55)),
    };
    assert!(sc.continue_r12);
    let (full, _) = shard_check::assert_shards_match(&sc, &SHARD_DEPTHS);
    assert!(full.is_clean(), "{}", full.summary());
    assert!(shards(&sc, 1).iter().any(|s| s.prefix.iter().any(|&c| c != 0)));
}

/// Depths 0 to past every leaf (no image here decides 16 cells before its last).
const SHARD_DEPTHS: [usize; 5] = [0, 1, 2, 4, 16];

/// Each nested search runs once across shards (CP3.4-1): Sol's S1 event 53 under `Extended`,
/// nested depth 1. Checker-only cells split one recovery over the power-loss
/// shards `0.0`, `0.1`, `0.2` and `1`, and each repeated its 52 nested branches (247 branches
/// at depth 4 against 91); the search belongs to the first, as unsharded.
#[test]
fn shards_own_each_nested_search_once() {
    let mut sc = s1(None);
    sc.adversary = Adversary::Extended;
    sc.nested = Nested::Depth1;
    sc.window = CrashWindow {
        from: EventSel::Index(53),
        to: Some(EventSel::Index(53)),
    };
    let (full, rows) = shard_check::assert_shards_match(&sc, &SHARD_DEPTHS);
    assert!(full.is_clean(), "{}", full.summary());
    assert_eq!(full.branches, 91, "{}", full.summary());
    assert!(rows.iter().any(|r| r.shards > 2), "the recovery must split over shards");
}

/// A recovery that refuses (returns an error with no fatal error before it, and no mark) ended:
/// the returned error closes its Core paths for the path balance, so T4's and T5's refusals
/// inside `LoadChainstate` and `ReplayBlocks` report R0 and no `Model` violation. The harness adds
/// no mark (DEC-185, CP3.3-1); `power_loss_cp33.rs`'s `Marker` keeps R8 on an unmarked fatal
/// return.
#[test]
fn refusals_close_their_paths() {
    for bug in [ToyBug::T4, ToyBug::T5] {
        let r = explore(&planted(bug).0, EXHAUSTIVE);
        assert!(r.found().contains(&R0), "{bug:?}: {}", r.summary());
        assert!(!r.found().contains(&Model), "{bug:?}: {}", r.summary());
    }
}

/// DEC-165 (3) end to end: a failed block or undo read on each Core call path the toy marks
/// stops the node with Core's message for that path (v24.0.0-rc.3). Every image of scenarios
/// that replay, reorg at activation and catch an index up is booted with a read fault on each
/// of its first filesystem reads; the innermost path at the fatal read decides the message.
#[test]
fn recovery_read_faults_use_each_paths_message() {
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{Arc, Mutex};

    use dashd_store::faulty::{
        AllSurvive, CrashImage, FS_STORE, FatalCause, Sut, World, WorldConfig,
    };
    let expected = |path: &str, m: &str| -> bool {
        let hashed = |pre: &str, post: &str| {
            m.strip_prefix(pre).and_then(|r| r.strip_suffix(post)).is_some_and(|h| h.len() == 64)
        };
        match path {
            "ReplayBlocks" => m.starts_with("Unable to replay blocks."),
            "DisconnectTip" => m == "Failed to disconnect block; see debug.log for details",
            "ConnectTip" => m == "Failed to read block",
            "BaseIndex::ThreadSync" => hashed("ThreadSync: Failed to read block ", " from disk"),
            "CoinStatsIndex::CustomAppend" => {
                hashed("ThreadSync: Failed to write block ", " to index database")
            }
            "BaseIndex::Rewind" => {
                m == "ThreadSync: Failed to rewind index toyindex to a previous chain tip"
            }
            _ => false,
        }
    };
    let mut seen: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Activation: index-only writes leave blocks with data above the durable tip, first ahead
    // on its branch (ConnectTip), then on a heavier branch from 6 (DisconnectTip, then ConnectTip).
    let activation =
        Scenario::new("activation", ToyConfig::default(), ops("c10;f;a5;i;c2;r6:12;i"));
    for sc in [s4(None), s11(None), s11r(None), activation] {
        let images = Arc::new(Mutex::new(Vec::<CrashImage>::new()));
        let hook = images.clone();
        let w = World::new(WorldConfig {
            on_event: Some(Box::new(move |w, _| hook.lock().unwrap().push(w.capture()))),
            ..WorldConfig::default()
        });
        let mut toy = ToyChain::open(&w, &sc.cfg).unwrap();
        for op in &sc.ops {
            toy.apply(op).unwrap();
        }
        let images = std::mem::take(&mut *images.lock().unwrap());
        for img in &images {
            for n in 0..6 {
                let cfg = WorldConfig {
                    faults: vec![Fault {
                        at: nth(StoreSel::Fs, OpKind::Read, n),
                        kind: FaultKind::Io,
                        outcome: FaultOutcome::NotApplied,
                    }],
                    ..WorldConfig::default()
                };
                let b = img.boot(CrashKind::ProcessKill, Box::new(AllSurvive), cfg);
                let Err(a) = ToyChain::open(&b, &sc.cfg) else {
                    continue;
                };
                // The Core paths open at the epoch's first failed filesystem read.
                let mut paths: Vec<&'static str> = Vec::new();
                let mut at_fault = None;
                for e in &b.trace().entries {
                    match e {
                        TraceEntry::Mark {
                            epoch,
                            mark,
                            ..
                        } if *epoch == b.epoch() => match mark {
                            Mark::PathBegin {
                                path,
                            } => paths.push(path),
                            Mark::PathEnd => {
                                paths.pop();
                            }
                            _ => {}
                        },
                        TraceEntry::ReadFailed {
                            epoch,
                            store,
                            ..
                        } if *epoch == b.epoch() && store == "fs" => {
                            at_fault = Some(paths.clone());
                            break;
                        }
                        _ => {}
                    }
                }
                let Some(paths) = at_fault.filter(|p| !p.is_empty()) else {
                    continue;
                };
                let path = *paths.last().unwrap();
                assert!(
                    expected(path, &a.message),
                    "{} image {}: a read fault in {path} aborted with {:?}",
                    sc.name,
                    img.at.index,
                    a.message
                );
                let cause = FatalCause {
                    read: true,
                    store: Some(FS_STORE.into()),
                    paths,
                };
                assert!(ToyChain::sanctioned_after(&sc.cfg, &a.message, &cause), "{cause:?}");
                seen.entry(path.to_string()).or_default().insert(sc.name.to_string());
            }
        }
    }
    eprintln!("{seen:?}");
    let paths = [
        "ReplayBlocks",
        "DisconnectTip",
        "ConnectTip",
        "BaseIndex::ThreadSync",
        "CoinStatsIndex::CustomAppend",
        "BaseIndex::Rewind",
    ];
    assert!(paths.iter().all(|p| seen.contains_key(*p)), "paths reached: {seen:?}");
}
