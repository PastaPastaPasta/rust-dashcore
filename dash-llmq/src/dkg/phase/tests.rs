//! `UpdatedBlockTip`'s phase rule and `SleepBeforePhase`'s wait.

use dash_params::{LlmqType, available_params};

use super::*;

const PHASES: [QuorumPhase; 7] = [
    QuorumPhase::Initialized,
    QuorumPhase::Contribute,
    QuorumPhase::Complain,
    QuorumPhase::Justify,
    QuorumPhase::Commit,
    QuorumPhase::Finalize,
    QuorumPhase::Idle,
];

/// `a/dkgsessionhandler.cpp:43-70`, line for line, with the handler's state as arguments.
fn core_updated_block_tip(
    params: &LlmqParams,
    quorum_index: i32,
    height: i32,
    snapshot_unvalidated: bool,
    rotation_enabled: bool,
    phase: QuorumPhase,
) -> Option<(i32, QuorumPhase)> {
    if snapshot_unvalidated {
        return None;
    }
    if quorum_index > 0 && !rotation_enabled {
        return None;
    }
    let quorum_stage_int = (height - quorum_index) % i32::from(params.dkg_interval);
    let base_height = height - quorum_stage_int;
    let f_new_phase = (quorum_stage_int % i32::from(params.dkg_phase_blocks)) == 0;
    let phase_int = quorum_stage_int / i32::from(params.dkg_phase_blocks) + 1;
    let mut phase = phase;
    if f_new_phase && (1..=7).contains(&phase_int) {
        phase = QuorumPhase::from_int(phase_int).expect("1..=7");
    }
    Some((base_height, phase))
}

/// The acceptance sweep (LQ4 §5): every type × quorum index × heights `0..3·dkgInterval` ×
/// both gates × every current phase, against Core's rule.
#[test]
fn every_type_index_height_and_gate_matches_core() {
    // `-llmqtestparams` overrides size, minSize and threshold only (§4.1), which the rule
    // does not read; the regtest types stand for them.
    let mut checked = 0usize;
    // Core: llmq/params.h:138 (available_llmqs)
    for t in LlmqType::AVAILABLE {
        let params = &available_params(t).expect("a table type");
        let indexes = if params.use_rotation {
            0..i32::from(params.signing_active_quorum_count)
        } else {
            0..2 // index 1 of a non-rotated type: rotation is off, so no tick
        };
        for quorum_index in indexes {
            for height in 0..3 * i32::from(params.dkg_interval) {
                for snapshot_unvalidated in [false, true] {
                    for rotation in [false, true] {
                        for current in PHASES {
                            let ours = phase_for_tip(
                                params,
                                quorum_index,
                                height,
                                snapshot_unvalidated,
                                rotation,
                                current,
                            )
                            .map(|t| (t.base_height, t.phase));
                            let core = core_updated_block_tip(
                                params,
                                quorum_index,
                                height,
                                snapshot_unvalidated,
                                rotation,
                                current,
                            );
                            assert_eq!(ours, core, "{t} qi {quorum_index} h {height}");
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 100_000);
}

/// Core's static table entry named `name` (`llmq/params.h:138`).
fn params(name: &str) -> LlmqParams {
    let t = LlmqType::AVAILABLE.into_iter().find(|t| t.to_string() == name).expect("a type");
    available_params(t).expect("a table type")
}

#[test]
fn phases_start_at_multiples_of_the_phase_length_and_hold_between() {
    let p = &params("llmq_test"); // interval 24, phase blocks 2
    let tick = |h, current| phase_for_tip(p, 0, h, false, false, current).expect("a tick");
    assert_eq!(
        tick(48, QuorumPhase::Idle),
        PhaseTick {
            base_height: 48,
            phase: QuorumPhase::Initialized
        }
    );
    assert_eq!(tick(50, QuorumPhase::Initialized).phase, QuorumPhase::Contribute);
    // Off a boundary the phase stays what it was, even a wrong one.
    assert_eq!(tick(51, QuorumPhase::Commit).phase, QuorumPhase::Commit);
    assert_eq!(tick(58, QuorumPhase::Commit).phase, QuorumPhase::Finalize);
    assert_eq!(tick(60, QuorumPhase::Finalize).phase, QuorumPhase::Idle);
    // Stage 14 > 6·pb is a boundary of phase 8: Idle stays.
    assert_eq!(tick(62, QuorumPhase::Idle).phase, QuorumPhase::Idle);
    assert_eq!(tick(62, QuorumPhase::Finalize).phase, QuorumPhase::Finalize);
    assert_eq!(tick(71, QuorumPhase::Idle).base_height, 48);
}

#[test]
fn a_rotated_index_offsets_the_stage_and_needs_rotation() {
    let p = &params("llmq_test_dip0024"); // interval 24, phase blocks 2, 2 active quorums
    let tick = phase_for_tip(p, 1, 49, false, true, QuorumPhase::Idle).expect("a tick");
    assert_eq!(
        tick,
        PhaseTick {
            base_height: 49,
            phase: QuorumPhase::Initialized
        }
    );
    assert_eq!(phase_for_tip(p, 1, 49, false, false, QuorumPhase::Idle), None);
    assert!(phase_for_tip(p, 0, 48, false, false, QuorumPhase::Idle).is_some());
    assert_eq!(phase_for_tip(p, 0, 48, true, true, QuorumPhase::Idle), None);
}

/// Finalize starts at `5·pb`; the mining window starts there for non-rotated types and
/// `signingActiveQuorumCount` later for rotated ones (`l/params.h:233`, :311, :390).
#[test]
fn finalize_and_the_mining_window() {
    // Core: llmq/params.h:138 (available_llmqs)
    for t in LlmqType::AVAILABLE {
        let p = &available_params(t).expect("a table type");
        let finalize = 5 * i32::from(p.dkg_phase_blocks);
        let tick = phase_for_tip(
            p,
            0,
            i32::from(p.dkg_interval) + finalize,
            false,
            true,
            QuorumPhase::Commit,
        );
        assert_eq!(tick.map(|t| t.phase), Some(QuorumPhase::Finalize), "{t}");
        let window = if p.use_rotation {
            i32::from(p.signing_active_quorum_count) + finalize
        } else {
            finalize
        };
        assert_eq!(i32::from(p.dkg_mining_window_start), window, "{t}");
    }
    assert_eq!(params("llmq_test_dip0024").dkg_mining_window_start, 12);
    assert_eq!(params("llmq_60_75").dkg_mining_window_start, 42);
}

#[test]
fn only_members_sleep_and_never_on_demand() {
    let p = &params("llmq_50_60"); // phase blocks 2, size 50
    let spacing = 150;
    assert_eq!(sleep_before_phase_ms(p, spacing, None, 0.05, false), 0);
    assert_eq!(sleep_before_phase_ms(p, spacing, Some(7), 0.05, true), 0);
    assert_eq!(sleep_before_phase_ms(p, spacing, Some(0), 0.05, false), 0);
    // (2 − 1) · 150 · 1000 / 50 · 0.05 = 150 ms per index.
    assert_eq!(sleep_before_phase_ms(p, spacing, Some(1), 0.05, false), 150);
    assert_eq!(sleep_before_phase_ms(p, spacing, Some(49), 0.05, false), 7350);
    assert_eq!(sleep_before_phase_ms(p, spacing, Some(49), 0.1, false), 14700);
    // A fraction truncates: 150000 / 400 · 0.05 · 3 = 56.25.
    let big = &params("llmq_400_60");
    let pb = i64::from(big.dkg_phase_blocks);
    assert_eq!(
        sleep_before_phase_ms(big, spacing, Some(3), 0.05, false),
        (pb - 1) * 150_000 * 3 / 400 / 20
    );
}
