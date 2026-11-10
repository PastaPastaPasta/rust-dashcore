//! The window's accounting over counter readings, free of the kernel so its tests supply the
//! counts (plan F7).
//!
//! - **Continuous** threads are read once at each end of the window: node A's and node B's
//!   every thread but the drivers, Core A's every thread but the caller, Core B's every thread.
//! - Core A's **segmented** caller is read just before and after each counted call
//!   (`send_pass`, the final barrier pass) through one wrapper; the maximum of the same wrapper's
//!   empty pairs in the same run is subtracted once per call (rev4: conservative, it lowers
//!   Core's A − B).
//! - **Excluded** threads (S's and H's drivers, the submitter) have no counter.
//!
//! Every thread of the census is exactly one of the three, the census at the end equals the one
//! at the start, and every counter ran for its whole enabled time (rev3 N3: pinned, never
//! scaled); anything else refuses.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{Result, refuse};

/// One read of a counter opened with `TOTAL_TIME_ENABLED | TOTAL_TIME_RUNNING`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub value: u64,
    /// Nanoseconds the event was enabled.
    pub enabled: u64,
    /// Nanoseconds it was on the PMU.
    pub running: u64,
}

/// A counter's count from `start` to `end`. Refuses unless it ran for its whole enabled time
/// over the interval (a pinned event that lost its PMU slot, or a multiplexed one), and on a
/// count or time that went back.
pub fn delta(start: Reading, end: Reading) -> Result<u64> {
    let (Some(enabled), Some(running), Some(value)) = (
        end.enabled.checked_sub(start.enabled),
        end.running.checked_sub(start.running),
        end.value.checked_sub(start.value),
    ) else {
        refuse!("a counter went back: {start:?} then {end:?}");
    };
    if running != enabled {
        refuse!("a counter ran {running} of its {enabled} enabled ns (lost coverage)");
    }
    Ok(value)
}

/// A continuously counted thread's readings at the window's two ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadCounts {
    pub tid: i32,
    pub start: Reading,
    pub end: Reading,
}

/// One counted call's caller readings, and the wrapper that took them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallerPair {
    pub before: Reading,
    pub after: Reading,
    /// The read-pair wrapper's address.
    pub wrapper: usize,
}

/// The read-pair wrapper's cost, from N empty pairs in the same run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Calibration {
    /// Each empty pair's count.
    pub pairs: Vec<u64>,
    /// The wrapper's address.
    pub wrapper: usize,
}

impl Calibration {
    /// The maximum empty pair, subtracted once per counted call. Refuses with no pairs.
    pub fn max(&self) -> Result<u64> {
        match self.pairs.iter().max() {
            Some(&m) => Ok(m),
            None => refuse!("no calibration pairs"),
        }
    }
}

/// Core A's caller, counted only inside its counted calls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segmented {
    pub tid: i32,
    pub calls: Vec<CallerPair>,
    pub calibration: Calibration,
}

/// Everything a window's count is made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowInput {
    /// `/proc/self/task` under the guard at the window's start.
    pub census: BTreeSet<i32>,
    /// The same at its end.
    pub census_end: BTreeSet<i32>,
    /// Threads with no counter: the drivers.
    pub excluded: BTreeSet<i32>,
    /// Continuously counted threads that must retire nothing (the node's flush thread).
    pub idle: BTreeSet<i32>,
    pub continuous: Vec<ThreadCounts>,
    pub segmented: Option<Segmented>,
}

/// A window's count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowCount {
    /// `continuous + caller`.
    pub total: u64,
    pub continuous: u64,
    /// The segmented caller's calls, less the calibration per call.
    pub caller: u64,
    pub calls: u32,
    /// The calibration subtracted per call (0 without a segmented caller).
    pub calibration_max: u64,
    pub calibration_pairs: u32,
    /// Threads with a counter.
    pub threads_counted: u32,
    pub threads_excluded: u32,
    /// Counted threads held to zero ([`WindowInput::idle`]).
    pub threads_idle: u32,
}

/// The window's count; refuses on anything the module docs rule out.
pub fn count(w: &WindowInput) -> Result<WindowCount> {
    if w.census != w.census_end {
        let born: Vec<_> = w.census_end.difference(&w.census).collect();
        let gone: Vec<_> = w.census.difference(&w.census_end).collect();
        refuse!("the threads changed inside the window: started {born:?}, exited {gone:?}");
    }
    let mut seen = BTreeSet::new();
    let mut claim = |tid: i32, what: &str| -> Result<()> {
        if !w.census.contains(&tid) {
            refuse!("{what} thread {tid} is not in the census");
        }
        if !seen.insert(tid) {
            refuse!("thread {tid} is accounted twice");
        }
        Ok(())
    };
    for &tid in &w.excluded {
        claim(tid, "an excluded")?;
    }
    for t in &w.continuous {
        claim(t.tid, "a counted")?;
    }
    if let Some(s) = &w.segmented {
        claim(s.tid, "the segmented")?;
    }
    if let Some(&missing) = w.census.difference(&seen).next() {
        refuse!("thread {missing} has no counter and is not a driver");
    }

    if let Some(t) = w.idle.iter().find(|t| !w.continuous.iter().any(|c| c.tid == **t)) {
        refuse!("idle thread {t} has no continuous counter");
    }
    let mut continuous = 0u64;
    for t in &w.continuous {
        let n = delta(t.start, t.end)?;
        if n != 0 && w.idle.contains(&t.tid) {
            refuse!("idle thread {} retired {n} instructions inside the window", t.tid);
        }
        continuous += n;
    }
    let (mut caller, mut calibration_max, mut calls, mut calibration_pairs) = (0u64, 0, 0, 0);
    if let Some(s) = &w.segmented {
        calibration_max = s.calibration.max()?;
        calibration_pairs = s.calibration.pairs.len() as u32;
        for pair in &s.calls {
            if pair.wrapper != s.calibration.wrapper {
                refuse!(
                    "a counted call used wrapper {:#x}, the calibration {:#x}",
                    pair.wrapper,
                    s.calibration.wrapper
                );
            }
            let n = delta(pair.before, pair.after)?;
            let Some(net) = n.checked_sub(calibration_max) else {
                refuse!("a counted call retired {n}, less than an empty pair's {calibration_max}");
            };
            caller += net;
            calls += 1;
        }
    }
    Ok(WindowCount {
        total: continuous + caller,
        continuous,
        caller,
        calls,
        calibration_max,
        calibration_pairs,
        threads_counted: (w.continuous.len() + usize::from(w.segmented.is_some())) as u32,
        threads_excluded: w.excluded.len() as u32,
        threads_idle: w.idle.len() as u32,
    })
}

/// Empty pairs in a segmented caller's calibration.
pub const CALIBRATION_PAIRS: usize = 64;

/// The completion rule (F5), as each leg checked it inside its window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completion {
    /// (1) The tip is the last workload block.
    pub tip_is_last: bool,
    /// (2) The validation-signal queue passed a barrier.
    pub signals_barrier: bool,
    /// (3) The P2P actor finished one full message-loop iteration with nothing pending (pass A;
    /// `None` in pass B, which has no actor).
    pub actor_barrier: Option<bool>,
    /// (4) H's last `cmpctblock` is queued (pass A; Core shows it after the window).
    pub hb_last_cmpct: Option<bool>,
    /// (3), the window's end: no block in flight or unserved, and (node) no P3 signal held
    /// (pass A).
    pub quiesced: Option<bool>,
}

impl Completion {
    /// Refuses unless every condition of a pass of `kind` held.
    pub fn check(&self, pass_a: bool) -> Result<()> {
        let want = if pass_a {
            Some(true)
        } else {
            None
        };
        if !self.tip_is_last
            || !self.signals_barrier
            || self.actor_barrier != want
            || self.hb_last_cmpct != want
            || self.quiesced != want
        {
            refuse!("the pass ended before its completion rule held: {self:?}");
        }
        Ok(())
    }

    /// [`Completion::check`] at the window's close, where pass A's (4) may still be `None`:
    /// Core shows H's last `cmpctblock` from a drain after the window.
    pub fn check_in_window(&self, pass_a: bool) -> Result<()> {
        match (pass_a, self.hb_last_cmpct) {
            (true, None) => Completion {
                hb_last_cmpct: Some(true),
                ..*self
            }
            .check(true),
            _ => self.check(pass_a),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(value: u64, t: u64) -> Reading {
        Reading {
            value,
            enabled: t,
            running: t,
        }
    }

    fn tc(tid: i32, start: u64, end: u64) -> ThreadCounts {
        ThreadCounts {
            tid,
            start: r(start, 10),
            end: r(end, 20),
        }
    }

    fn set(tids: &[i32]) -> BTreeSet<i32> {
        tids.iter().copied().collect()
    }

    /// Node A's shape: workers 2 and 3 counted, the driver 1 excluded.
    fn node_a() -> WindowInput {
        WindowInput {
            census: set(&[1, 2, 3]),
            census_end: set(&[1, 2, 3]),
            excluded: set(&[1]),
            idle: BTreeSet::new(),
            continuous: vec![tc(2, 1_000, 1_700), tc(3, 50, 350)],
            segmented: None,
        }
    }

    /// A thread held idle (the node's flush thread) refuses the window if it retired anything,
    /// and must be one of the continuous counters.
    #[test]
    fn an_idle_thread_that_worked_refuses() {
        let mut w = node_a();
        w.continuous.push(tc(4, 500, 500));
        w.census.insert(4);
        w.census_end.insert(4);
        w.idle = set(&[4]);
        let c = count(&w).unwrap();
        assert_eq!((c.total, c.threads_counted, c.threads_idle), (1_000, 3, 1));
        w.continuous[2] = tc(4, 500, 501);
        let e = count(&w).unwrap_err();
        assert!(e.0.contains("idle thread 4 retired 1 instructions"), "{e}");
        w.idle = set(&[1]);
        let e = count(&w).unwrap_err();
        assert!(e.0.contains("idle thread 1 has no continuous counter"), "{e}");
    }

    #[test]
    fn a_window_counts_its_workers_from_its_start() {
        let c = count(&node_a()).unwrap();
        assert_eq!((c.total, c.continuous, c.caller), (1_000, 1_000, 0));
        assert_eq!((c.threads_counted, c.threads_excluded), (2, 1));
    }

    /// The prefix is excluded: counts before the window's start reads (the prefix and its
    /// quiesce) are not in the delta, however large.
    #[test]
    fn the_prefix_is_excluded() {
        let mut w = node_a();
        w.continuous = vec![tc(2, 1_000_000_000, 1_000_000_700), tc(3, 900_000_050, 900_000_350)];
        assert_eq!(count(&w).unwrap().total, 1_000);
    }

    #[test]
    fn a_missing_worker_attach_refuses() {
        let mut w = node_a();
        w.continuous.pop();
        let e = count(&w).unwrap_err();
        assert!(e.0.contains("thread 3 has no counter"), "{e}");
    }

    /// A driver counted as a worker changes the delta: the mutant that drops the exclusion is
    /// caught by the count, and a driver both excluded and counted refuses.
    #[test]
    fn a_driver_thread_included_gives_the_wrong_delta() {
        let mut w = node_a();
        w.excluded.clear();
        w.continuous.push(tc(1, 0, 5_000));
        assert_ne!(count(&w).unwrap().total, 1_000);
        let mut w = node_a();
        w.continuous.push(tc(1, 0, 5_000));
        assert!(count(&w).unwrap_err().0.contains("accounted twice"));
    }

    #[test]
    fn a_thread_born_or_gone_inside_the_window_refuses() {
        let mut w = node_a();
        w.census_end.insert(4);
        assert!(count(&w).unwrap_err().0.contains("started [4]"));
        let mut w = node_a();
        w.census_end.remove(&3);
        assert!(count(&w).unwrap_err().0.contains("exited [3]"));
    }

    /// rev3 N3: coverage short of the enabled time refuses, unscaled; so does a count that went
    /// back.
    #[test]
    fn forced_contention_refuses() {
        let mut w = node_a();
        w.continuous[0].end.running -= 1;
        assert!(count(&w).unwrap_err().0.contains("lost coverage"));
        let mut w = node_a();
        w.continuous[1].end.value = 1;
        assert!(count(&w).unwrap_err().0.contains("went back"));
    }

    fn core_a(gap_work: u64) -> WindowInput {
        let wrapper = 0x1000;
        WindowInput {
            census: set(&[1, 2]),
            census_end: set(&[1, 2]),
            excluded: BTreeSet::new(),
            idle: BTreeSet::new(),
            // Worker 2 is read only at the window's two ends: work it does in the caller's gaps
            // (between counted calls) is in its delta.
            continuous: vec![tc(2, 0, 300 + gap_work)],
            segmented: Some(Segmented {
                tid: 1,
                calls: vec![
                    CallerPair {
                        before: r(100, 1),
                        after: r(1_100, 2),
                        wrapper,
                    },
                    // A deliver/drain gap's 5,000 caller instructions are between the pairs.
                    CallerPair {
                        before: r(6_100, 3),
                        after: r(6_600, 4),
                        wrapper,
                    },
                ],
                calibration: Calibration {
                    pairs: vec![40, 47, 45],
                    wrapper,
                },
            }),
        }
    }

    /// rev2 N1: worker work in a Core A caller gap stays in A, the caller's gaps do not, and
    /// the calibrated maximum is subtracted once per counted call.
    #[test]
    fn worker_work_in_a_caller_gap_stays_in_a() {
        let quiet = count(&core_a(0)).unwrap();
        let busy = count(&core_a(9_000)).unwrap();
        assert_eq!(busy.total - quiet.total, 9_000);
        assert_eq!(quiet.caller, (1_000 - 47) + (500 - 47));
        assert_eq!((quiet.calls, quiet.calibration_max, quiet.calibration_pairs), (2, 47, 3));
    }

    /// rev5: the calibration and every counted call take the same wrapper (by address), and a
    /// call cheaper than an empty pair refuses.
    #[test]
    fn the_calibration_bounds_every_counted_pair_through_one_wrapper() {
        let mut w = core_a(0);
        w.segmented.as_mut().unwrap().calls[1].wrapper = 0x2000;
        assert!(count(&w).unwrap_err().0.contains("wrapper"));
        let mut w = core_a(0);
        w.segmented.as_mut().unwrap().calls[0].after = r(130, 2);
        assert!(count(&w).unwrap_err().0.contains("less than an empty pair"));
        let mut w = core_a(0);
        w.segmented.as_mut().unwrap().calibration.pairs.clear();
        assert!(count(&w).unwrap_err().0.contains("no calibration"));
    }

    /// F5: an unfinished final handler (the actor barrier not passed, H's last `cmpctblock` not
    /// queued) refuses; pass B has no actor or H.
    #[test]
    fn an_unfinished_final_handler_refuses() {
        let done = Completion {
            tip_is_last: true,
            signals_barrier: true,
            actor_barrier: Some(true),
            hb_last_cmpct: Some(true),
            quiesced: Some(true),
        };
        assert!(done.check(true).is_ok());
        for early in [
            Completion {
                quiesced: Some(false),
                ..done
            },
            Completion {
                actor_barrier: Some(false),
                ..done
            },
            Completion {
                hb_last_cmpct: None,
                ..done
            },
            Completion {
                signals_barrier: false,
                ..done
            },
            Completion {
                tip_is_last: false,
                ..done
            },
        ] {
            assert!(early.check(true).is_err(), "{early:?}");
        }
        let b = Completion {
            actor_barrier: None,
            hb_last_cmpct: None,
            quiesced: None,
            ..done
        };
        assert!(b.check(false).is_ok() && done.check(false).is_err() && b.check(true).is_err());
        let core_a = Completion {
            hb_last_cmpct: None,
            ..done
        };
        assert!(core_a.check_in_window(true).is_ok() && core_a.check(true).is_err());
        let node_a = Completion {
            hb_last_cmpct: Some(false),
            ..done
        };
        assert!(node_a.check_in_window(true).is_err());
        assert!(b.check_in_window(false).is_ok() && done.check_in_window(false).is_err());
    }
}
