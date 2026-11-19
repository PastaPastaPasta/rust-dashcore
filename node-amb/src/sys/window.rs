//! A measured window in a pass's child (rev5 masking protocol steps 1–6).
//!
//! [`open`]: census (and no signalfd); the caller's raw mask, every other thread's mask
//! through the handler, and each TID's `SigBlk`; the guard with `TSYNC`; the census again
//! under the guard (it must equal the masked set); the counters, then `SigBlk` again; Core A's
//! calibration; the start reads. [`Window::close`]: the completion rule, every held thread
//! asleep, the end reads, the census, `SigBlk` unchanged and nothing pending, then
//! [`account::count`].

use std::collections::BTreeSet;

use super::calib;
use super::counters::Counter;
use super::{census, gettid, guard, mask};
use crate::account::{
    self, Calibration, CallerPair, Completion, Reading, Segmented, ThreadCounts, WindowCount,
    WindowInput,
};
use crate::{Result, refuse};

pub use crate::account::CALIBRATION_PAIRS;

/// How a window counts.
#[derive(Clone, Debug, Default)]
pub struct Setup {
    /// Threads with no counter (the drivers).
    pub excluded: BTreeSet<i32>,
    /// Core A: the caller counts only inside [`Window::counted`] calls.
    pub segment_caller: bool,
    /// Attach counters. Off: the masks and the guard only (the guard's adversaries, or a
    /// smoke run with no PMU), and the window gives no number.
    pub count: bool,
    /// The mask-gap test's: a thread the mask signal is not sent to.
    pub skip_mask: Option<i32>,
    /// Counted threads that must retire nothing inside the window (the node's flush thread):
    /// any work there refuses the pass.
    pub idle: BTreeSet<i32>,
}

/// An open window.
#[derive(Debug)]
pub struct Window {
    setup: Setup,
    me: i32,
    census: BTreeSet<i32>,
    continuous: Vec<(Counter, Reading)>,
    caller: Option<(Counter, Calibration)>,
    calls: Vec<CallerPair>,
}

/// Opens the window on the calling thread.
pub fn open(setup: Setup) -> Result<Window> {
    let me = gettid();
    if setup.segment_caller && setup.excluded.contains(&me) {
        refuse!("the segmented caller is excluded");
    }
    if setup.segment_caller && !setup.count {
        refuse!("a segmented caller needs counters");
    }
    if setup.count {
        // Prewarm: the wrapper's path bound and paged in before the guard.
        calib::warm(Counter::open(me)?.raw_fd());
    }
    let tids = census()?;
    if let Some(t) = setup.excluded.iter().find(|t| !tids.contains(t)) {
        refuse!("excluded thread {t} is not in the census");
    }
    mask::no_signalfd()?;
    mask::block_caller()?;
    mask::mask_others(&tids, me, setup.skip_mask)?;
    mask::verify(&tids)?;
    // A thread's mask reads full while its mask handler still runs (the handler blocks every
    // signal): a held-idle thread must be back asleep before the guard and its counter.
    for &t in &setup.idle {
        mask::await_asleep(t)?;
    }
    guard::install()?;
    let under = census()?;
    if under != tids {
        refuse!("the census under the guard {under:?} is not the masked {tids:?}");
    }
    mask::no_signalfd()?;
    let mut continuous = Vec::new();
    let mut caller = None;
    if setup.count {
        for &tid in tids.difference(&setup.excluded) {
            let c = Counter::open(tid)?;
            if setup.segment_caller && tid == me {
                caller = Some(c);
            } else {
                continuous.push((c, Reading::default()));
            }
        }
    }
    mask::verify(&tids)?;
    let caller = match caller {
        Some(c) => {
            let mut pairs = Vec::with_capacity(CALIBRATION_PAIRS);
            let mut wrapper = None;
            for _ in 0..CALIBRATION_PAIRS {
                let pair = calib::read_pair(c.raw_fd(), &mut calib::nothing);
                if *wrapper.get_or_insert(pair.wrapper()) != pair.wrapper() {
                    refuse!("the calibration's pairs took two wrappers");
                }
                let (b, a) = pair.readings(me)?;
                pairs.push(account::delta(b, a)?);
            }
            Some((
                c,
                Calibration {
                    pairs,
                    wrapper: wrapper.unwrap_or_default(),
                },
            ))
        }
        None => None,
    };
    for (c, start) in &mut continuous {
        *start = c.read()?;
    }
    Ok(Window {
        setup,
        me,
        census: tids,
        continuous,
        caller,
        calls: Vec::new(),
    })
}

impl Window {
    /// Runs `f`: for a segmented caller, between the wrapper's two reads (a counted call).
    pub fn counted(&mut self, f: &mut dyn FnMut()) -> Result<()> {
        let Some((c, _)) = &self.caller else {
            f();
            return Ok(());
        };
        let pair = calib::read_pair(c.raw_fd(), f);
        let (before, after) = pair.readings(self.me)?;
        self.calls.push(CallerPair {
            before,
            after,
            wrapper: pair.wrapper(),
        });
        Ok(())
    }

    /// The segmented caller's calibration and its calls so far.
    pub fn segments(&self) -> Option<(&Calibration, &[CallerPair])> {
        self.caller.as_ref().map(|(_, c)| (c, self.calls.as_slice()))
    }

    /// Closes the window: refuses unless completion conditions (1)–(3) held for a pass of its
    /// kind (Core's (4) is shown after the window), then reads, checks and counts. `None`
    /// without counters.
    pub fn close(self, completion: &Completion, pass_a: bool) -> Result<Option<WindowCount>> {
        completion.check_in_window(pass_a)?;
        // R2-F1: a job handed to a held thread inside the window makes it runnable until it
        // runs. Waiting for it to sleep again before the end reads puts that work in its count,
        // and the idle hold refuses it; it never starts after the close uncounted.
        for &t in &self.setup.idle {
            mask::await_asleep(t)?;
        }
        let mut ends = Vec::with_capacity(self.continuous.len());
        for (c, start) in &self.continuous {
            ends.push(ThreadCounts {
                tid: c.tid,
                start: *start,
                end: c.read()?,
            });
        }
        let census_end = census()?;
        mask::verify(&census_end)?;
        mask::pending(&census_end)?;
        if !self.setup.count {
            if census_end != self.census {
                refuse!("the threads changed inside the window");
            }
            return Ok(None);
        }
        let segmented = self.caller.map(|(_, calibration)| Segmented {
            tid: self.me,
            calls: self.calls,
            calibration,
        });
        account::count(&WindowInput {
            census: self.census,
            census_end,
            excluded: self.setup.excluded,
            idle: self.setup.idle,
            continuous: ends,
            segmented,
        })
        .map(Some)
    }
}
