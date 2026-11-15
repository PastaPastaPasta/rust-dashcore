//! T19 (BL-837): the two held-restart programs in Diff mode on S0, S1 and S2. Each builds a lock
//! on a fork's tip, a side-branch block that conflicts with it and that a reconsider ignoring
//! ChainLocks leaves unmarked in the model, and a tip above the lock on the lock's chain, then
//! restarts (clean, or a KILL after a full flush) and delivers that block's held child.
//!
//! Each input ends done, every op the same on both sides, and no restart is dropped as
//! `restart_marked_tip`. Where the lock is enforced and the restart runs, the held child's answer
//! is one only the dropped lock widened (`cl_dropped_any`): the load kept the lock it held. The
//! schedules where the tie model refuses the enforcement (`cl_unenforced`) are named: S2, whose
//! line also keeps the restart's tie (`restart_tie`), so neither program reaches the load there.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use dash_oracle_sys::Pin;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::Program;
use node_l1::chain::run::{End, Run};
use node_l1::chain::{Mode, Runner};

const HELD_RESTART_CLEAN: &str = "\
extend main 2
fork 0 1 headers hold
fork 0 1 hold
fork 2 2
cl tip#2
invalidate built#0
reconsider built#0 ignore_cl
extend b3 1
restart clean
deliver b2
";

const HELD_RESTART_KILL: &str = "\
extend main 2
fork 0 1 headers hold
fork 0 1 hold
fork 2 2
cl tip#2
invalidate built#0
reconsider built#0 ignore_cl
extend b3 1
flush always
restart kill
deliver b2
";

fn avoided(run: &Run, key: &str) -> u64 {
    run.census.avoided.get(key).copied().unwrap_or(0)
}

#[test]
fn held_restart_programs_run_done_and_keep_the_held_lock_at_the_load() {
    let mut unenforced = Vec::new();
    for schedule in [Schedule::S0, Schedule::S1, Schedule::S2] {
        let mut runner = Runner::with_mode(Pin::V24_0, schedule, Mode::Diff).unwrap();
        for (name, text) in
            [("held_restart_clean", HELD_RESTART_CLEAN), ("held_restart_kill", HELD_RESTART_KILL)]
        {
            let run = runner.run(&Program::parse(text).unwrap()).unwrap();
            let at = format!("{name} on {schedule:?}");
            let restarts = run.kinds.iter().filter(|k| **k == "restart").count();
            let widened = run.census.rows.get("cl_dropped_any").copied().unwrap_or(0);
            println!(
                "T19 {at}: end {:?}, restarts run {restarts}, deferred {}, cl_dropped_any {widened}, \
                 avoided {:?}",
                run.end, run.census.deferred_restarts, run.census.avoided
            );
            assert_eq!(run.end, End::Done, "{at}: after {:?}", run.kinds);
            assert_eq!(avoided(&run, "restart_marked_tip"), 0, "{at}");
            if avoided(&run, "cl_unenforced") == 0 {
                assert_eq!(restarts, 1, "{at}: the restart is planned and run");
                assert!(widened >= 1, "{at}: the load kept the lock");
            } else {
                assert_eq!(avoided(&run, "restart_tie"), 1, "{at}");
                unenforced.push(at);
            }
        }
    }
    assert_eq!(
        unenforced,
        ["held_restart_clean on S2", "held_restart_kill on S2"],
        "the schedules where the tie model refuses the enforcement"
    );
}
