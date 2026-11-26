//! Items 2-4: every abort branch of each wait with its line, `SleepBeforePhase`'s length, early
//! exit and wait function, and `HandlePhase`'s order (`a/dkgsessionhandler.cpp:111-255`,
//! v24.0.0).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use dash_params::LlmqType;
use dash_primitives::QuorumHash;

use super::fakes::{Line, Mark, Rig, dkg_lines, logged, logged_across, params, quorum_hash};
use crate::dkg::wait::{
    Abort, handle_phase, sleep_before_phase, wait_for_new_quorum, wait_for_next_phase,
};
use crate::dkg::{QuorumPhase, Served};

use QuorumPhase::{Commit, Complain, Contribute, Initialized, Justify};

const W: &str = "ActiveDKGSessionHandler::WaitForNextPhase -- llmq_test qi[0]";
const N: &str = "ActiveDKGSessionHandler::WaitForNewQuorum -- llmq_test qi[0]";
const S: &str = "ActiveDKGSessionHandler::SleepBeforePhase -- llmq_test qi[0]";
const H: &str = "ActiveDKGSessionHandler::HandlePhase -- llmq_test qi[0]";
const SNAPSHOT: &str = "aborting because snapshot background validation is incomplete";
const STOP: &str = "aborting due to stop/shutdown requested";

/// The lines a wait logged: the scripted tips' own `UpdatedBlockTip` lines left out.
fn wait_lines(lines: &[Line]) -> Vec<String> {
    let mut lines = dkg_lines(lines);
    lines.retain(|l| !l.starts_with("ActiveDKGSessionHandler::UpdatedBlockTip -- "));
    lines
}

fn never() -> impl FnMut() -> bool {
    || false
}

fn null() -> QuorumHash {
    QuorumHash::default()
}

/// A script that delivers the tip at `height` on tick `tick` alone.
fn at(tick: usize, height: i32) -> impl FnMut(usize) -> Vec<i32> + Send + 'static {
    move |n| {
        if n == tick {
            vec![height]
        } else {
            vec![]
        }
    }
}

#[test]
fn next_phase_reached_logs_done_and_resets_only_for_initialized() {
    let rig = Rig::llmq_test();
    rig.tip(47);
    rig.script(at(3, 48));
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, None, Initialized, &null(), &mut never())
    });
    assert_eq!(out, Ok(()));
    assert_eq!(rig.clock.ticks(), 3);
    assert_eq!(
        wait_lines(&lines),
        [
            format!("{W} - starting, curPhase=-1, nextPhase=1"),
            format!("{W} - done, curPhase=-1, nextPhase=1"),
        ]
    );
    assert_eq!(rig.marks.take(), [Mark::Reset(LlmqType::LlmqTest, 0)]);

    rig.script(at(4, 50));
    let q = quorum_hash(48);
    let out = wait_for_next_phase(
        &rig.ctx(),
        &rig.clock,
        Some(Initialized),
        Contribute,
        &q,
        &mut never(),
    );
    assert_eq!(out, Ok(()));
    assert!(rig.marks.take().is_empty(), "no reset outside Initialized");
}

#[test]
fn next_phase_aborts_on_a_snapshot_before_a_stop() {
    let rig = Rig::llmq_test();
    rig.chain.snapshot.store(true, Ordering::SeqCst);
    rig.handler.request_stop();
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, None, Initialized, &null(), &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(
        wait_lines(&lines),
        [format!("{W} - starting, curPhase=-1, nextPhase=1"), format!("{W} - {SNAPSHOT}")]
    );
    assert!(rig.marks.take().is_empty());
}

#[test]
fn next_phase_aborts_on_a_stop() {
    let rig = Rig::llmq_test();
    rig.script({
        let handler = rig.handler.clone();
        move |n| {
            if n == 2 {
                handler.request_stop();
            }
            vec![]
        }
    });
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, None, Initialized, &null(), &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(rig.clock.ticks(), 2);
    assert_eq!(wait_lines(&lines)[1], format!("{W} - {STOP}"));
}

#[test]
fn next_phase_aborts_on_a_new_quorum_hash_unless_none_is_expected() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(at(2, 72));
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, Some(Initialized), Contribute, &q, &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(
        wait_lines(&lines),
        [
            format!("{W} - starting, curPhase=1, nextPhase=2"),
            format!("{W} - aborting due unexpected expectedQuorumHash change"),
        ]
    );

    // A null hash expects none: the new interval's Initialized is still the current phase.
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(|n| match n {
        2 => vec![72],
        4 => vec![74],
        _ => vec![],
    });
    let out = wait_for_next_phase(
        &rig.ctx(),
        &rig.clock,
        Some(Initialized),
        Contribute,
        &null(),
        &mut never(),
    );
    assert_eq!((out, rig.clock.ticks()), (Ok(()), 4));
}

#[test]
fn next_phase_aborts_on_a_phase_other_than_the_current_one() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(at(1, 52));
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, Some(Initialized), Contribute, &q, &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(
        wait_lines(&lines)[1],
        format!("{W} - aborting due unexpected phase change, _phase=3, curPhase=1")
    );
}

#[test]
fn next_phase_skips_the_tick_while_the_wait_function_returns_true() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(at(1, 50));
    let mut calls = 0;
    let mut busy = || {
        calls += 1;
        calls <= 4
    };
    let q = quorum_hash(48);
    let out =
        wait_for_next_phase(&rig.ctx(), &rig.clock, Some(Initialized), Contribute, &q, &mut busy);
    assert_eq!(out, Ok(()));
    assert_eq!((calls, rig.clock.ticks()), (5, 1));
}

#[test]
fn new_quorum_waits_for_another_hash_and_aborts_on_a_snapshot_or_a_stop() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(|n| {
        if n == 3 {
            vec![72]
        } else {
            vec![49]
        }
    });
    let q = quorum_hash(48);
    let (out, lines) = logged(|| wait_for_new_quorum(&rig.ctx(), &rig.clock, &q));
    assert_eq!((out, rig.clock.ticks()), (Ok(()), 3));
    assert_eq!(wait_lines(&lines), [format!("{N}- starting"), format!("{N} - done")]);

    let rig = Rig::llmq_test();
    rig.chain.snapshot.store(true, Ordering::SeqCst);
    rig.handler.request_stop();
    let (out, lines) = logged(|| wait_for_new_quorum(&rig.ctx(), &rig.clock, &null()));
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines), [format!("{N}- starting"), format!("{N} - {SNAPSHOT}")]);

    let rig = Rig::llmq_test();
    rig.handler.request_stop();
    let (out, lines) = logged(|| wait_for_new_quorum(&rig.ctx(), &rig.clock, &null()));
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines), [format!("{N}- starting"), format!("{N} - {STOP}")]);
}

/// A `llmq_test` rig in Contribute (tip 50) whose slot holds a session in which we are member
/// `idx`, or an observer for `None`.
fn member(idx: Option<usize>) -> Rig {
    let rig = Rig::llmq_test();
    rig.tip(50);
    rig.handler.set_session(Arc::new(Served::new(idx)));
    rig
}

fn sleep(rig: &Rig, factor: f64, wait: &mut dyn FnMut() -> bool) -> Result<(), Abort> {
    let q = quorum_hash(48);
    sleep_before_phase(&rig.ctx(), &rig.clock, Contribute, &q, factor, wait)
}

#[test]
fn no_sleep_without_a_session_for_a_non_member_or_on_demand() {
    let null_slot = Rig::llmq_test();
    null_slot.tip(50);
    let observer = member(None);
    let mut on_demand = member(Some(2));
    on_demand.clock.on_demand = true;
    for rig in [&null_slot, &observer, &on_demand] {
        let mut calls = 0;
        let (out, lines) = logged(|| {
            sleep(rig, 0.05, &mut || {
                calls += 1;
                false
            })
        });
        assert_eq!(out, Ok(()));
        assert!(lines.is_empty());
        assert_eq!((calls, rig.clock.ticks()), (0, 0));
    }
}

#[test]
fn the_sleep_grows_with_the_member_index() {
    for (idx, ms, ticks) in [(0, 0, 0), (1, 2500, 25), (2, 5000, 50)] {
        let rig = member(Some(idx));
        let (out, lines) = logged(|| sleep(&rig, 0.05, &mut never()));
        assert_eq!(out, Ok(()));
        assert_eq!(rig.clock.ticks(), ticks, "idx {idx}");
        assert_eq!(
            wait_lines(&lines),
            [
                format!("{S} - starting sleep for {ms} ms, curPhase=2"),
                format!("{S} - done, curPhase=2"),
            ]
        );
    }
    // `llmq_400_60`'s last member before Commit: 3 · 150 000 / 400 · 0.1 · 399 = 44 887.5.
    let rig = Rig::new(LlmqType::Llmq400_60, params(LlmqType::Llmq400_60), 0, 150);
    rig.tip(592);
    rig.handler.set_session(Arc::new(Served::new(Some(399))));
    let q = quorum_hash(576);
    let (out, lines) =
        logged(|| sleep_before_phase(&rig.ctx(), &rig.clock, Commit, &q, 0.1, &mut never()));
    assert_eq!(out, Ok(()));
    assert_eq!(rig.clock.ticks(), 449);
    assert_eq!(
        wait_lines(&lines)[0],
        "ActiveDKGSessionHandler::SleepBeforePhase -- llmq_400_60 qi[0] - starting sleep for 44887 ms, curPhase=5"
    );
}

#[test]
fn the_sleep_ends_once_the_new_blocks_outrun_it() {
    // 3 · 1000 / 400 · 1.0 · 399 = 2992 ms: two blocks (2000 ms) do not end it, a third does.
    let rig = Rig::new(LlmqType::Llmq400_60, params(LlmqType::Llmq400_60), 0, 1);
    rig.tip(580);
    rig.handler.set_session(Arc::new(Served::new(Some(399))));
    rig.script(|n| vec![580 + i32::try_from(n).expect("small")]);
    let q = quorum_hash(576);
    let (out, lines) =
        logged(|| sleep_before_phase(&rig.ctx(), &rig.clock, Contribute, &q, 1.0, &mut never()));
    assert_eq!(out, Ok(()));
    assert_eq!(rig.clock.ticks(), 3);
    let lines = wait_lines(&lines);
    assert!(lines[0].ends_with("starting sleep for 2992 ms, curPhase=2"), "{lines:?}");
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].ends_with(" - done, curPhase=2"), "{lines:?}");
}

#[test]
fn the_sleep_aborts_on_a_phase_or_hash_change_a_snapshot_or_a_stop() {
    let phase_line = format!("{S} - aborting due unexpected phase/expectedQuorumHash change");
    // At factor 2 the sleep (133 333 ms) outlasts one block (100 000 ms), which moves the
    // phase.
    let rig = Rig::new(LlmqType::LlmqTest, params(LlmqType::LlmqTest), 0, 100);
    rig.tip(50);
    rig.tip(51);
    rig.handler.set_session(Arc::new(Served::new(Some(2))));
    rig.script(at(1, 52));
    let (out, lines) = logged(|| sleep(&rig, 2.0, &mut never()));
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines)[1], phase_line);
    assert_eq!(rig.clock.ticks(), 1);

    let rig = member(Some(2));
    let other = quorum_hash(24);
    let (out, lines) = logged(|| {
        sleep_before_phase(&rig.ctx(), &rig.clock, Contribute, &other, 0.05, &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines)[1], phase_line);

    let rig = member(Some(2));
    rig.chain.snapshot.store(true, Ordering::SeqCst);
    rig.handler.request_stop();
    let (out, lines) = logged(|| sleep(&rig, 0.05, &mut never()));
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines)[1], format!("{S} - {SNAPSHOT}"));

    let rig = member(Some(2));
    rig.handler.request_stop();
    let (out, lines) = logged(|| sleep(&rig, 0.05, &mut never()));
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines)[1], format!("{S} - {STOP}"));
}

#[test]
fn the_wait_function_runs_on_every_pass_and_skips_the_tick_while_true() {
    let rig = member(Some(1));
    let trace = rig.trace.clone();
    let mut calls = 0;
    let out = sleep(&rig, 0.05, &mut || {
        calls += 1;
        trace.push("wait");
        calls <= 3
    });
    assert_eq!(out, Ok(()));
    assert_eq!((calls, rig.clock.ticks()), (28, 25));
    let got: Vec<String> = rig.trace.take().into_iter().filter(|e| e != "snapshot").collect();
    let mut want = vec!["wait"; 3];
    for _ in 0..25 {
        want.extend(["wait", "tick"]);
    }
    assert_eq!(got, want);
}

#[test]
fn handle_phase_sleeps_checks_the_snapshot_starts_marks_then_waits() {
    let rig = member(Some(1));
    rig.trace.take();
    rig.script(at(26, 52));
    let trace = rig.trace.clone();
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        handle_phase(
            &rig.ctx(),
            &rig.clock,
            Contribute,
            Complain,
            &q,
            0.05,
            || trace.push("start"),
            &mut || {
                trace.push("wait");
                false
            },
        )
    });
    assert_eq!(out, Ok(()));
    let mut want = Vec::new();
    for _ in 0..25 {
        want.extend(["snapshot", "wait", "tick"]);
    }
    // The tick's tip asks the snapshot gate too.
    want.extend([
        "snapshot", "start", "mark:2", "snapshot", "wait", "tick", "snapshot", "snapshot",
    ]);
    assert_eq!(rig.trace.take(), want);
    assert_eq!(rig.marks.take(), [Mark::Advanced(LlmqType::LlmqTest, 0, Contribute)]);
    assert_eq!(
        wait_lines(&lines),
        [
            format!("{H} - starting, curPhase=2, nextPhase=3"),
            format!("{S} - starting sleep for 2500 ms, curPhase=2"),
            format!("{S} - done, curPhase=2"),
            format!("{W} - starting, curPhase=2, nextPhase=3"),
            format!("{W} - done, curPhase=2, nextPhase=3"),
            format!("{H} - done, curPhase=2, nextPhase=3"),
        ]
    );
}

#[test]
fn handle_phase_refuses_after_the_sleep_while_a_snapshot_is_unvalidated() {
    let rig = member(None);
    rig.chain.snapshot.store(true, Ordering::SeqCst);
    let mut started = false;
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        handle_phase(
            &rig.ctx(),
            &rig.clock,
            Contribute,
            Complain,
            &q,
            0.05,
            || started = true,
            &mut never(),
        )
    });
    assert_eq!(out, Err(Abort));
    assert!(!started);
    assert!(rig.marks.take().is_empty());
    assert_eq!(
        wait_lines(&lines),
        [
            format!("{H} - starting, curPhase=2, nextPhase=3"),
            "HandlePhase -- refusing DKG participation while snapshot background validation is incomplete".to_string(),
        ]
    );
}

#[test]
fn handle_phase_aborted_in_its_sleep_starts_nothing() {
    let rig = member(Some(2));
    rig.handler.request_stop();
    let mut started = false;
    let q = quorum_hash(48);
    let out = handle_phase(
        &rig.ctx(),
        &rig.clock,
        Contribute,
        Justify,
        &q,
        0.05,
        || started = true,
        &mut never(),
    );
    assert_eq!(out, Err(Abort));
    assert!(!started);
    assert!(rig.marks.take().is_empty());
}

#[test]
fn next_phase_checks_the_hash_before_the_phase_it_waits_for() {
    // Blocks connected together report only the last tip: the next interval's Contribute.
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.script(at(1, 74));
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, Some(Initialized), Contribute, &q, &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(
        wait_lines(&lines)[1],
        format!("{W} - aborting due unexpected expectedQuorumHash change")
    );
}

#[test]
fn next_phase_checks_the_stop_before_the_hash() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.tip(72);
    rig.handler.request_stop();
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        wait_for_next_phase(&rig.ctx(), &rig.clock, Some(Initialized), Contribute, &q, &mut never())
    });
    assert_eq!(out, Err(Abort));
    assert_eq!(wait_lines(&lines)[1], format!("{W} - {STOP}"));
}

#[test]
fn the_sleep_ends_on_fast_blocks_before_it_checks_the_phase() {
    // 1000 / 3 · 1.0 · 2 = 666 ms; one block (1000 ms) that starts Complain ends the sleep.
    let rig = Rig::new(LlmqType::LlmqTest, params(LlmqType::LlmqTest), 0, 1);
    rig.tip(50);
    rig.tip(51);
    rig.handler.set_session(Arc::new(Served::new(Some(2))));
    rig.script(at(1, 52));
    let (out, lines) = logged(|| sleep(&rig, 1.0, &mut never()));
    assert_eq!(out, Ok(()));
    assert_eq!(rig.clock.ticks(), 1);
    assert_eq!(
        wait_lines(&lines),
        [format!("{S} - starting sleep for 666 ms, curPhase=2"), format!("{S} - done, curPhase=2"),]
    );
}

#[test]
fn blocks_exactly_as_long_as_the_sleep_do_not_end_it() {
    // `llmq_test_dip0024`: 1000 / 4 · 4.0 · 1 = 1000 ms, one block's time: the sleep runs on.
    let t = LlmqType::LlmqTestDip0024;
    let rig = Rig::new(t, params(LlmqType::LlmqTestDip0024), 0, 1);
    rig.tip(50);
    rig.handler.set_session(Arc::new(Served::new(Some(1))));
    rig.script(at(1, 51));
    let (out, lines) = logged(|| sleep(&rig, 4.0, &mut never()));
    assert_eq!(out, Ok(()));
    assert_eq!(rig.clock.ticks(), 10);
    assert!(wait_lines(&lines)[0].ends_with("starting sleep for 1000 ms, curPhase=2"));
}

#[test]
fn handle_phase_marks_the_phase_even_when_the_wait_after_it_aborts() {
    let rig = member(None);
    rig.script(at(1, 54));
    let mut started = false;
    let q = quorum_hash(48);
    let (out, lines) = logged(|| {
        handle_phase(
            &rig.ctx(),
            &rig.clock,
            Contribute,
            Complain,
            &q,
            0.05,
            || started = true,
            &mut never(),
        )
    });
    assert_eq!(out, Err(Abort));
    assert!(started);
    assert_eq!(rig.marks.take(), [Mark::Advanced(LlmqType::LlmqTest, 0, Contribute)]);
    let lines = wait_lines(&lines);
    assert_eq!(
        lines.last().map(String::as_str),
        Some(format!("{W} - aborting due unexpected phase change, _phase=4, curPhase=2").as_str())
    );
}

/// Core logs `UpdatedBlockTip` under `cs_phase_qhash`, so a waiter that sees the new phase logs
/// its `done` after the tip's line. The capture holds the tip's line before recording it, which
/// lets a waiter that could read the phase meanwhile log first.
#[test]
fn the_tip_line_comes_before_the_done_of_a_waiter_that_sees_its_phase() {
    let rig = Rig::llmq_test();
    rig.tip(48);
    rig.tip(49);
    assert_eq!(rig.handler.phase(), Initialized);
    let spinning = AtomicBool::new(false);
    let q = quorum_hash(48);
    let (out, lines) = logged_across(Duration::from_millis(300), |dispatch| {
        std::thread::scope(|s| {
            let waiter = s.spawn(|| {
                tracing::dispatcher::with_default(dispatch, || {
                    let mut spin = || {
                        spinning.store(true, Ordering::SeqCst);
                        std::thread::yield_now();
                        true
                    };
                    wait_for_next_phase(
                        &rig.ctx(),
                        &rig.clock,
                        Some(Initialized),
                        Contribute,
                        &q,
                        &mut spin,
                    )
                })
            });
            while !spinning.load(Ordering::SeqCst) {
                assert!(!waiter.is_finished(), "the waiter ended before it waited");
                std::thread::yield_now();
            }
            rig.tip(50);
            waiter.join().expect("the waiter")
        })
    });
    assert_eq!(out, Ok(()));
    let order: Vec<_> = dkg_lines(&lines)
        .into_iter()
        .filter(|l| l.contains("currentHeight=50") || l.starts_with(&format!("{W} - done")))
        .collect();
    assert_eq!(
        order,
        [
            "ActiveDKGSessionHandler::UpdatedBlockTip -- llmq_test qi[0] currentHeight=50, pQuorumBaseBlockIndex->nHeight=48, oldPhase=1, newPhase=2".to_string(),
            format!("{W} - done, curPhase=1, nextPhase=2"),
        ]
    );
}
