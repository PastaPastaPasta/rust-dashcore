//! Item 1: the phase a handler takes from each tip over a whole interval, with the
//! `UpdatedBlockTip` line (`a/dkgsessionhandler.cpp:43-70`, v24.0.0); and the manager's gates
//! (`l/dkgsessionmgr.cpp:41-55`).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use dash_params::{LlmqParams, LlmqType};
use dash_primitives::QuorumHash;

use super::fakes::{Rig, Trace, dkg_lines, logged, params, quorum_hash, seams, sessions, tip};
use crate::dkg::{DkgSessions, QuorumPhase, SessionKey};

use QuorumPhase::{Commit, Complain, Contribute, Finalize, Idle, Initialized, Justify};

/// The phases of one interval, stage by stage, from runs of (phase, blocks).
fn expand(runs: &[(QuorumPhase, usize)]) -> Vec<QuorumPhase> {
    runs.iter().flat_map(|&(phase, n)| std::iter::repeat_n(phase, n)).collect()
}

/// `llmq_test` and `llmq_test_dip0024`: 2-block phases in a 24-block interval.
fn table_24_2() -> Vec<QuorumPhase> {
    expand(&[
        (Initialized, 2),
        (Contribute, 2),
        (Complain, 2),
        (Justify, 2),
        (Commit, 2),
        (Finalize, 2),
        (Idle, 12),
    ])
}

/// `llmq_400_60`: 4-block phases in a 288-block interval.
fn table_288_4() -> Vec<QuorumPhase> {
    expand(&[
        (Initialized, 4),
        (Contribute, 4),
        (Complain, 4),
        (Justify, 4),
        (Commit, 4),
        (Finalize, 4),
        (Idle, 264),
    ])
}

/// Drives a fresh handler of `t` at `quorum_index` with every tip from the block before `base`
/// through the interval's last block and the next base, checking each tip's phase, hash, height
/// and line against `table`.
fn check_interval(
    t: LlmqType,
    params: LlmqParams,
    quorum_index: i32,
    base: i32,
    table: &[QuorumPhase],
) {
    let rig = Rig::new(t, params, quorum_index, 150);
    rig.chain.rotation.store(true, Ordering::SeqCst);
    let interval = i32::try_from(table.len()).expect("small");
    let name = t.to_string();
    let mut old = Idle;
    for h in base - 1..=base + interval {
        let (_, lines) = logged(|| rig.tip(h));
        let stage = usize::try_from((h - base).rem_euclid(interval)).expect("a stage");
        let base_h = h - i32::try_from(stage).expect("small");
        let want = if h < base {
            Idle
        } else {
            table[stage]
        };
        assert_eq!(rig.handler.phase(), want, "{name} qi {quorum_index} h {h}");
        assert_eq!(rig.handler.quorum_hash(), quorum_hash(base_h), "{name} h {h}");
        assert_eq!(rig.handler.current_height(), h);
        assert_eq!(
            dkg_lines(&lines),
            [format!(
                "ActiveDKGSessionHandler::UpdatedBlockTip -- {name} qi[{quorum_index}] currentHeight={h}, pQuorumBaseBlockIndex->nHeight={base_h}, oldPhase={}, newPhase={}",
                old as i32, want as i32
            )]
        );
        old = want;
    }
}

#[test]
fn llmq_test_over_an_interval() {
    check_interval(LlmqType::LlmqTest, params(LlmqType::LlmqTest), 0, 48, &table_24_2());
}

#[test]
fn llmq_test_dip0024_index_0_and_1_over_an_interval() {
    let params = params(LlmqType::LlmqTestDip0024);
    check_interval(LlmqType::LlmqTestDip0024, params, 0, 48, &table_24_2());
    check_interval(LlmqType::LlmqTestDip0024, params, 1, 49, &table_24_2());
}

#[test]
fn llmq_400_60_over_an_interval() {
    check_interval(LlmqType::Llmq400_60, params(LlmqType::Llmq400_60), 0, 576, &table_288_4());
}

#[test]
fn the_first_line_matches_cores_text() {
    let rig = Rig::llmq_test();
    let (_, lines) = logged(|| rig.tip(48));
    assert_eq!(
        dkg_lines(&lines),
        [
            "ActiveDKGSessionHandler::UpdatedBlockTip -- llmq_test qi[0] currentHeight=48, pQuorumBaseBlockIndex->nHeight=48, oldPhase=7, newPhase=1"
        ]
    );
}

#[test]
fn index_1_is_not_updated_while_rotation_is_off() {
    let rig = Rig::new(LlmqType::LlmqTestDip0024, params(LlmqType::LlmqTestDip0024), 1, 150);
    for h in 48..=73 {
        let (_, lines) = logged(|| rig.tip(h));
        assert!(lines.is_empty());
    }
    assert_eq!(rig.handler.phase(), Idle);
    assert_eq!(rig.handler.quorum_hash(), QuorumHash::default());
    assert_eq!(rig.handler.current_height(), -1);
    // Index 0 never asks.
    let zero = Rig::new(LlmqType::LlmqTestDip0024, params(LlmqType::LlmqTestDip0024), 0, 150);
    zero.tip(48);
    assert!(!zero.trace.take().contains(&"rotation".to_string()));
    assert_eq!(zero.handler.phase(), Initialized);
}

#[test]
fn a_snapshot_leaves_the_handler_as_it_is_before_the_rotation_check() {
    let rig = Rig::new(LlmqType::LlmqTestDip0024, params(LlmqType::LlmqTestDip0024), 1, 150);
    rig.chain.rotation.store(true, Ordering::SeqCst);
    rig.chain.snapshot.store(true, Ordering::SeqCst);
    let (_, lines) = logged(|| rig.tip(49));
    assert!(lines.is_empty());
    assert_eq!(rig.trace.take(), ["snapshot"]);
    assert_eq!((rig.handler.phase(), rig.handler.current_height()), (Idle, -1));
}

const KEY: SessionKey = SessionKey {
    llmq_type: LlmqType::LlmqTest,
    quorum_index: 0,
};

#[test]
fn the_manager_cleans_the_cache_first_then_gates_on_ibd_dip3_and_spork_17() {
    let trace = Arc::new(Trace::default());
    let (sessions, sporks) = sessions(&trace);
    sessions.updated_block_tip(&tip(48), true);
    assert_eq!(trace.take(), ["cleanup"]);
    assert_eq!(sessions.phase(KEY), Idle);

    sporks.spork17.store(false, Ordering::SeqCst);
    sessions.updated_block_tip(&tip(48), false);
    assert_eq!(trace.take(), ["cleanup", "dip3(48)", "spork17"]);
    assert_eq!(sessions.phase(KEY), Idle);

    sporks.spork17.store(true, Ordering::SeqCst);
    sessions.updated_block_tip(&tip(48), false);
    assert_eq!(trace.take(), ["cleanup", "dip3(48)", "spork17", "snapshot"]);
    assert_eq!(sessions.phase(KEY), Initialized);
}

#[test]
fn before_dip3_no_handler_is_updated() {
    let trace = Arc::new(Trace::default());
    let (seams, _, _) = seams(&trace);
    let mut chain = super::fakes::FakeChain::new(trace.clone());
    chain.dip3_from = 100;
    let seams = crate::dkg::SessionSeams {
        chain: Arc::new(chain),
        ..seams
    };
    let sessions = DkgSessions::new([(LlmqType::LlmqTest, params(LlmqType::LlmqTest))], seams);
    sessions.updated_block_tip(&tip(48), false);
    assert_eq!(trace.take(), ["cleanup", "dip3(48)"]);
    assert_eq!(sessions.phase(KEY), Idle);
    sessions.updated_block_tip(&tip(120), false);
    assert_eq!(sessions.phase(KEY), Initialized);
}
