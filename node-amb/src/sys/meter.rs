//! Where a leg's pass puts its window: [`Guarded`] in a measured child, [`Plain`] in the
//! semantic smoke. Both hold the completion rule at the close.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::window::{self, Setup, Window};
use super::{gettid, threads_named};
use crate::account::{Completion, WindowCount};
use crate::{Result, refuse};

pub trait Meter {
    /// `segment`: the caller counts only inside [`Meter::counted`] calls (Core A).
    fn open(&mut self, segment: bool) -> Result<()>;
    /// Runs `f`, a counted call of a segmented caller.
    fn counted(&mut self, f: &mut dyn FnMut()) -> Result<()>;
    fn close(&mut self, c: &Completion, pass_a: bool) -> Result<Option<WindowCount>>;
}

/// No guard and no counters.
#[derive(Debug, Default)]
pub struct Plain {
    /// Set once the window closed (the held-drain test reads it).
    pub closed: Arc<AtomicBool>,
}

impl Meter for Plain {
    fn open(&mut self, _: bool) -> Result<()> {
        Ok(())
    }

    fn counted(&mut self, f: &mut dyn FnMut()) -> Result<()> {
        f();
        Ok(())
    }

    fn close(&mut self, c: &Completion, pass_a: bool) -> Result<Option<WindowCount>> {
        c.check_in_window(pass_a)?;
        self.closed.store(true, Ordering::SeqCst);
        Ok(None)
    }
}

/// A measured child's window, counted when `count`.
#[derive(Debug)]
pub struct Guarded {
    count: bool,
    /// The caller is a driver (the node leg's scripted peers, or its submitter): no counter.
    exclude_caller: bool,
    /// Names of threads held idle inside the window ([`Setup::idle`]).
    idle: &'static [&'static str],
    window: Option<Window>,
}

impl Guarded {
    pub fn new(count: bool, exclude_caller: bool) -> Guarded {
        Guarded {
            count,
            exclude_caller,
            idle: &[],
            window: None,
        }
    }

    /// Holds every thread with one of `names` idle in each window: there must be one at the
    /// open, and any work it does inside refuses the pass.
    pub fn hold_idle(&mut self, names: &'static [&'static str]) {
        self.idle = names;
    }
}

impl Meter for Guarded {
    fn open(&mut self, segment: bool) -> Result<()> {
        let mut idle = BTreeSet::new();
        for name in self.idle {
            let tids = threads_named(|n| n == *name)?;
            if tids.is_empty() {
                refuse!("no {name} thread to hold idle");
            }
            idle.extend(tids);
        }
        self.window = Some(window::open(Setup {
            excluded: self.exclude_caller.then(gettid).into_iter().collect(),
            segment_caller: segment && self.count,
            count: self.count,
            skip_mask: None,
            idle,
        })?);
        Ok(())
    }

    fn counted(&mut self, f: &mut dyn FnMut()) -> Result<()> {
        match &mut self.window {
            Some(w) => w.counted(f),
            None => refuse!("no window is open"),
        }
    }

    fn close(&mut self, c: &Completion, pass_a: bool) -> Result<Option<WindowCount>> {
        match self.window.take() {
            Some(w) => w.close(c, pass_a),
            None => refuse!("no window is open"),
        }
    }
}
