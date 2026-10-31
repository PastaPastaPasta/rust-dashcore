//! The explorer (§3.5, §4.2, §4.5): runs a system under test (SUT) forward, captures a crash
//! image after every event of a window, boots each image after both crash kinds and every
//! decision branch, reopens the SUT and checks it.
//!
//! - **Capture mode** (no faults): one forward run, a copy-on-write image per event.
//! - **Re-execution mode** (a fault plan): one forward run per fault, capturing from the fault.
//! - **Exhaustive** search runs every branch of every image up to a per-image cap and reports
//!   truncation; **witness** search stops once every expected invariant has been reported.
//! - **Nested** (depth 1): every event of each branch's recovery is captured and explored in
//!   turn, with the outer decisions kept (rule L4).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::faulty::check::{InvariantId, RefExtractor, Violation};
use crate::faulty::decide::{Dfs, Fixed, Seeded, SeededDecider};
use crate::faulty::repro::Repro;
use crate::faulty::world::{
    Adversary, AllocateMode, CrashImage, CrashKind, Decider, EventSel, Fault, Granularity, Hash,
    InheritedDecision, Mark, OpKind, Start, Trace, TraceEntry, Var, Work, World, WorldConfig,
};

/// A fatal stop of the system under test, with Core's message where Core has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abort {
    pub message: String,
}

impl Abort {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The reference model: the intended state after each op.
pub trait Reference: Send {
    type Op;
    type Config;
    fn new(cfg: &Self::Config) -> Self;
    fn apply(&mut self, op: &Self::Op);
}

/// A system under test.
pub trait Sut: Sized {
    type Config: Clone + fmt::Debug + fmt::Display + FromStr + Send + Sync;
    type Op: Clone + fmt::Debug + fmt::Display + FromStr + Send + Sync;
    type Ref: Reference<Op = Self::Op, Config = Self::Config>;

    /// Its name in repros.
    const NAME: &'static str;

    /// Opens (and recovers) the system on `world`'s stores.
    fn open(world: &World, cfg: &Self::Config) -> Result<Self, Abort>;
    fn apply(&mut self, op: &Self::Op) -> Result<(), Abort>;
    /// The checks after a reopen (§5.1).
    fn check(&self, world: &World, refm: &Self::Ref, trace: &Trace) -> Result<(), Vec<Violation>>;
    /// An op to run after recovery for R12, if the system supports it.
    fn continue_after_recovery(&mut self) -> Option<Self::Op> {
        None
    }
    /// Whether the system has an R12 continuation: a scenario runs R12 by default exactly
    /// when it does, and asking for R12 without one fails the exploration.
    fn has_continuation() -> bool {
        false
    }
    /// Store-level checks on the booted stores, before the system opens them (§5.1's order:
    /// store-level checks, then R0, then the view-level checks).
    fn store_check(world: &World, cfg: &Self::Config) -> Vec<Violation> {
        let _ = (world, cfg);
        Vec::new()
    }
    /// Preconditions asserted on the forward run before any search (§4.5).
    fn preconditions(&self, world: &World, refm: &Self::Ref, trace: &Trace) -> Result<(), String> {
        let _ = (world, refm, trace);
        Ok(())
    }
    /// Forward assertions (§6.2.1), checked on every forward run.
    fn forward_check(&self, world: &World, refm: &Self::Ref, trace: &Trace) -> Vec<Violation> {
        let _ = (world, refm, trace);
        Vec::new()
    }
    /// The system's dependency declarations for D1 (§5.3).
    fn extractor(cfg: &Self::Config) -> Option<Arc<dyn RefExtractor>> {
        let _ = cfg;
        None
    }
    /// Whether an abort message is sanctioned (R0's allowed refusals, R8's abort strings).
    fn sanctioned(cfg: &Self::Config, message: &str) -> bool {
        let _ = (cfg, message);
        false
    }
    /// Whether an abort message is sanctioned after the epoch's first fatal error `cause`:
    /// Core's message for the call path that failed (DEC-162 (4); by default
    /// [`Sut::sanctioned`]'s, whatever the cause).
    fn sanctioned_after(cfg: &Self::Config, message: &str, cause: &FatalCause) -> bool {
        let _ = cause;
        Self::sanctioned(cfg, message)
    }
    /// A digest of the recovered state for R7a (an interrupted recovery converges).
    fn projection(&self) -> Option<Hash> {
        None
    }
}

/// The events to crash after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashWindow {
    pub from: EventSel,
    /// The last event of the window (inclusive); `None` runs to the end.
    pub to: Option<EventSel>,
}

impl CrashWindow {
    pub fn all() -> Self {
        Self {
            from: EventSel::Index(0),
            to: None,
        }
    }
}

/// Faults: none (capture mode), or one re-execution per fault.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FaultPlan(pub Vec<Fault>);

/// Nested crashes during recovery (§4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nested {
    None,
    Depth1,
    /// Depth 1, plus a second crash during the recovery from the first on a seeded sample of
    /// inner crashes (per mille), each with one branch.
    Depth2Sample(u16),
}

/// How to search (§4.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Exhaustive {
        max_branches_per_image: u32,
    },
    /// Stop once every expected invariant is reported; fail if `max_branches` pass first.
    Witness {
        expect: Vec<InvariantId>,
        max_branches: u64,
    },
}

/// One exploration.
#[derive(Debug)]
pub struct Scenario<S: Sut> {
    pub name: &'static str,
    pub cfg: S::Config,
    pub ops: Vec<S::Op>,
    pub window: CrashWindow,
    pub kinds: Vec<CrashKind>,
    pub faults: FaultPlan,
    pub adversary: Adversary,
    pub granularity: Granularity,
    /// How `allocate` behaves (N7: it fails silently).
    pub allocate: AllocateMode,
    pub nested: Nested,
    /// R12 on every branch, not only each image's all-survived one (nightly, §8.4).
    pub r12_all: bool,
    /// R12 (§5.1): run the continuation on each image's all-survived branch.
    pub continue_r12: bool,
    /// Faults injected after the crash, each in its own pass over every image (§4.4: read
    /// faults in recovery). Each must fire at least once.
    pub recovery_faults: Vec<Fault>,
    /// The fewest images the exploration must cover (non-vacuous; exhaustive mode).
    pub min_images: u64,
    /// R7a's bound on the completions of a nested branch's assignment its references enumerate:
    /// past it the branch is unjudged (DEC-162 (2)).
    pub r7a_assignments: u32,
    /// Nested inner images past the cap: also sampled with this seed, a smoke that can only find;
    /// the image still counts as truncated (DEC-185 (1)). `None` only truncates.
    pub over_cap: Option<OverCap>,
}

/// How a nested inner image past the cap is sampled (DEC-185 (1): a smoke, never a pass).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverCap {
    pub seed: u64,
    /// The most branches sampled per inner image and kind.
    pub samples: u64,
}

// By hand: a derive would require the system type itself to be `Clone`.
impl<S: Sut> Clone for Scenario<S> {
    fn clone(&self) -> Self {
        Self {
            name: self.name,
            cfg: self.cfg.clone(),
            ops: self.ops.clone(),
            window: self.window.clone(),
            kinds: self.kinds.clone(),
            faults: self.faults.clone(),
            adversary: self.adversary,
            granularity: self.granularity,
            allocate: self.allocate,
            nested: self.nested,
            r12_all: self.r12_all,
            continue_r12: self.continue_r12,
            recovery_faults: self.recovery_faults.clone(),
            min_images: self.min_images,
            r7a_assignments: self.r7a_assignments,
            over_cap: self.over_cap,
        }
    }
}

impl<S: Sut> Scenario<S> {
    pub fn new(name: &'static str, cfg: S::Config, ops: Vec<S::Op>) -> Self {
        Self {
            name,
            cfg,
            ops,
            window: CrashWindow::all(),
            kinds: vec![CrashKind::ProcessKill, CrashKind::PowerLoss],
            faults: FaultPlan::default(),
            adversary: Adversary::Realistic,
            granularity: Granularity::Sector,
            allocate: AllocateMode::Fallocate,
            nested: Nested::None,
            r12_all: false,
            continue_r12: S::has_continuation(),
            recovery_faults: Vec::new(),
            min_images: 1,
            r7a_assignments: R7A_ASSIGNMENTS,
            over_cap: None,
        }
    }

    /// The configuration a crash image of this scenario boots with.
    fn boot_cfg(&self) -> WorldConfig {
        boot_cfg(self.adversary, self.granularity, self.allocate)
    }
}

/// R7a's default bound on the completions of a nested branch's assignment (DEC-162 (2)).
pub(crate) const R7A_ASSIGNMENTS: u32 = 64;

/// A violation with how to reproduce it.
#[derive(Debug, Clone)]
pub struct Finding {
    pub violation: Violation,
    pub repro: Repro,
}

/// The outcome of an exploration.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub images: u64,
    pub branches: u64,
    /// Images whose branches exceeded the cap, sampled past it or not: a report with any is never
    /// clean (DEC-185 (1)).
    pub truncated: u64,
    /// Of those, nested inner images sampled past the cap ([`Scenario::over_cap`]): a smoke that
    /// can find a violation and never passes the rest (DEC-185 (1)).
    pub sampled: u64,
    /// The branches those samples ran (also counted in `branches`).
    pub sampled_branches: u64,
    /// The first finding of each (invariant, detail) pair.
    pub findings: Vec<Finding>,
    pub work: Work,
    /// Branches per image, maximum.
    pub max_branches_per_image: u64,
    /// Witness mode: every expected invariant was found.
    pub found_all: bool,
    /// Per image and kind: the crash event, the branches run, and the variables decided on the
    /// image's last branch (for calibration).
    pub per_image: Vec<(u64, CrashKind, u64, Vec<String>)>,
    /// How R7a judged the nested branches.
    pub r7a: R7aCounts,
    /// Per outer image and kind: the nested branches run under its branches (depth 2 included)
    /// and how R7a judged them. A sharded search sums to the unsharded one bucket by bucket
    /// (DEC-185 (2), CP3.5-1).
    pub per_image_nested: BTreeMap<(u64, CrashKind), NestedTotals>,
}

/// The nested branches under one outer image and kind (see [`Report::per_image_nested`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NestedTotals {
    pub branches: u64,
    pub r7a: R7aCounts,
}

/// How R7a judged nested branches (see [`r7a_judge`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct R7aCounts {
    /// The recovery ended as the uninterrupted one did, under the same outer cells.
    pub converged: u64,
    /// Reproduced under every completion of its assignment, some by reference (a′): the
    /// uninterrupted recovery with the inner crash's inherited choices applied afterwards.
    pub witnessed_after: u64,
    /// Reproduced under every completion by reference (b): the outer image crashed before its
    /// recovery's first event, its `^` cells matching.
    pub witnessed_before: u64,
    /// The recovery fail-stopped with a sanctioned abort on a block the inner crash tore
    /// (DEC-161): allowed.
    pub sanctioned_fail_stop: u64,
    /// The recovery completed, passing every other check, where the uninterrupted one
    /// fail-stopped sanctioned (DEC-165 (1)): allowed, as Core may start after a fail-stop.
    pub recovered_after_failstop: u64,
    /// The recovery fail-stopped, sanctioned with R8 holding and every other check passing, where
    /// the uninterrupted one also fail-stopped sanctioned, and no reference reproduced its abort
    /// (I42): allowed, as (1) of DEC-165 for a later start that stops again.
    pub failstop_after_failstop: u64,
    /// More completions of the assignment than the bound, or references that decided its cells
    /// inconsistently: never passed (DEC-162 (2)). A clean report has none.
    pub unjudged: u64,
}

impl R7aCounts {
    pub fn merge(&mut self, o: Self) {
        let Self {
            converged,
            witnessed_after,
            witnessed_before,
            sanctioned_fail_stop,
            recovered_after_failstop,
            failstop_after_failstop,
            unjudged,
        } = o;
        self.converged += converged;
        self.witnessed_after += witnessed_after;
        self.witnessed_before += witnessed_before;
        self.sanctioned_fail_stop += sanctioned_fail_stop;
        self.recovered_after_failstop += recovered_after_failstop;
        self.failstop_after_failstop += failstop_after_failstop;
        self.unjudged += unjudged;
    }
}

impl NestedTotals {
    pub fn merge(&mut self, o: Self) {
        self.branches += o.branches;
        self.r7a.merge(o.r7a);
    }
}

impl Report {
    pub fn found(&self) -> BTreeSet<InvariantId> {
        self.findings.iter().map(|f| f.violation.invariant).collect()
    }

    /// No finding, every image exhausted (none truncated, sampled or not), and every nested
    /// branch judged.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
            && self.truncated == 0
            && self.sampled == 0
            && self.r7a.unjudged == 0
    }

    /// The first finding of `inv`.
    pub fn first(&self, inv: InvariantId) -> Option<&Finding> {
        self.findings.iter().find(|f| f.violation.invariant == inv)
    }

    fn add(&mut self, v: Violation, repro: &Repro) {
        let dup = self.findings.iter().any(|f| f.violation.invariant == v.invariant)
            && self.findings.len() > 64;
        if !dup
            && !self
                .findings
                .iter()
                .any(|f| f.violation.invariant == v.invariant && f.violation.detail == v.detail)
        {
            let mut repro = repro.clone();
            repro.expect = vec![v.invariant];
            self.findings.push(Finding {
                violation: v,
                repro,
            });
        }
    }

    /// A readable summary for test failures.
    pub fn summary(&self) -> String {
        let sampled = if self.sampled > 0 {
            format!(" sampled {} ({} branches)", self.sampled, self.sampled_branches)
        } else {
            String::new()
        };
        let mut s = format!(
            "images {} branches {} truncated {}{sampled} max/image {} found {:?}\n",
            self.images,
            self.branches,
            self.truncated,
            self.max_branches_per_image,
            self.found()
        );
        let r = &self.r7a;
        if *r != R7aCounts::default() {
            s.push_str(&format!(
                "  R7a nested branches: converged {} witnessed (a') {} witnessed (b) {} \
                 sanctioned fail-stop {} recovered after fail-stop {} fail-stop after fail-stop {} \
                 unjudged {}\n",
                r.converged,
                r.witnessed_after,
                r.witnessed_before,
                r.sanctioned_fail_stop,
                r.recovered_after_failstop,
                r.failstop_after_failstop,
                r.unjudged
            ));
        }
        for f in self.findings.iter().take(12) {
            s.push_str(&format!(
                "  {}\n    repro: {}\n",
                f.violation,
                f.repro.to_text().replace('\n', " | ")
            ));
        }
        s
    }
}

fn boot_cfg(
    sc_adversary: Adversary,
    granularity: Granularity,
    allocate: AllocateMode,
) -> WorldConfig {
    WorldConfig {
        adversary: sc_adversary,
        granularity,
        allocate,
        d1: true,
        ..WorldConfig::default()
    }
}

fn faulty_boot_cfg(
    sc_adversary: Adversary,
    granularity: Granularity,
    allocate: AllocateMode,
    fault: Option<&Fault>,
) -> WorldConfig {
    WorldConfig {
        faults: fault.cloned().into_iter().collect(),
        ..boot_cfg(sc_adversary, granularity, allocate)
    }
}

/// The allowed recovered tips (§5.1): `T_ack`, the new tip of the last flush with a `Durable`
/// mark (or `None`, the empty state, if none has one), plus the new tip of each later begun
/// flush.
pub fn allowed_tips(trace: &Trace) -> (Option<Hash>, Vec<Option<Hash>>) {
    let mut flushes: Vec<(u64, Hash, bool)> = Vec::new();
    for m in trace.marks() {
        match m {
            Mark::FlushBegin {
                flush,
                new,
                ..
            } => flushes.push((*flush, *new, false)),
            Mark::Durable {
                flush,
            } => {
                if let Some(f) = flushes.iter_mut().rev().find(|f| f.0 == *flush) {
                    f.2 = true;
                }
            }
            _ => {}
        }
    }
    let acked = flushes.iter().rposition(|f| f.2);
    let t_ack = acked.map(|i| flushes[i].1);
    let mut allowed = vec![t_ack];
    for f in &flushes[acked.map_or(0, |i| i + 1)..] {
        allowed.push(Some(f.1));
    }
    (t_ack, allowed)
}

/// What an epoch's first fatal error was, for the abort messages that may follow it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FatalCause {
    /// A failed read (a traced read failure, or a store failure the system marked as one).
    pub read: bool,
    /// The store that failed ([`FS_STORE`](crate::faulty::world::FS_STORE) for block and undo
    /// data); `None` for a failure only the system's [`Mark::StoreFailed`] reports.
    pub store: Option<String>,
    /// The Core call paths it failed inside, outermost first ([`Mark::PathBegin`]).
    pub paths: Vec<&'static str>,
}

impl FatalCause {
    /// On the filesystem (block and undo data), not an engine.
    pub fn fs(&self) -> bool {
        self.store.as_deref() == Some(crate::faulty::world::FS_STORE)
    }

    /// The innermost Core call path it failed inside.
    pub fn path(&self) -> Option<&'static str> {
        self.paths.last().copied()
    }

    /// Whether it failed inside `path`, at any depth.
    pub fn within(&self, path: &str) -> bool {
        self.paths.contains(&path)
    }
}

/// A system's sanctioned abort messages, by the epoch's first fatal error.
fn sanctioned_by<S: Sut>(cfg: &S::Config) -> impl Fn(&str, &FatalCause) -> bool + '_ {
    |m, cause| S::sanctioned_after(cfg, m, cause)
}

/// The first fatal error of an epoch: its index in the trace, and its cause.
#[derive(Clone)]
struct FirstFatal {
    at: usize,
    cause: FatalCause,
}

/// How the system's last epoch ended, for [`fail_stop_scan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
    /// It is still running (a capture mid-run): nothing is owed yet.
    Open,
    /// The call returned normally (or the run stopped on its own).
    Returned,
    /// The call returned an error. That closes every Core path it was in, for the path balance
    /// only: R8 still needs the system's own `Abort` mark after a fatal error, which the harness
    /// never supplies (DEC-185, CP3.3-1).
    Refused,
}

/// R8 (§4.4) over the epochs from `from_epoch` on: after the first fatal error of an epoch, no
/// later mutating call of the epoch succeeds, and the epoch ends with an `Abort` sanctioned for
/// that error's cause (checked unless the last epoch is still [`End::Open`]). A last epoch that
/// returned normally must also leave every Core path it entered (a `Model` violation). Also each
/// epoch's first fatal error.
fn fail_stop_scan(
    trace: &Trace,
    from_epoch: u32,
    end: End,
    sanctioned: &dyn Fn(&str, &FatalCause) -> bool,
) -> (Vec<Violation>, BTreeMap<u32, FirstFatal>) {
    let complete = end != End::Open;
    let mut out = Vec::new();
    let mut first_fatal = BTreeMap::new();
    let mut epochs: BTreeMap<u32, Vec<(usize, &TraceEntry)>> = BTreeMap::new();
    for (i, e) in trace.entries.iter().enumerate() {
        let epoch = match e {
            TraceEntry::Event {
                id,
                ..
            } => id.epoch,
            TraceEntry::Mark {
                epoch: m,
                ..
            } => *m,
            TraceEntry::Crash {
                epoch: c,
                ..
            } => *c,
            TraceEntry::ReadFailed {
                epoch: r,
                ..
            } => *r,
        };
        if epoch >= from_epoch {
            epochs.entry(epoch).or_default().push((i, e));
        }
    }
    let last_epoch = epochs.keys().copied().max().unwrap_or(0);
    for (epoch, entries) in epochs {
        let mut best_effort = 0u32;
        // Recovery, connect and Core-path regions, where a failed read is fatal (§4.4).
        let mut region = 0u32;
        let mut paths: Vec<&'static str> = Vec::new();
        let mut fatal: Option<String> = None;
        let mut cause = FatalCause::default();
        let mut aborted = false;
        let mut reported = false;
        for (i, e) in entries {
            match e {
                TraceEntry::Mark {
                    mark,
                    ..
                } => {
                    match mark {
                        Mark::BestEffortBegin => best_effort += 1,
                        Mark::RecoveryBegin | Mark::ConnectBegin => region += 1,
                        Mark::RecoveryEnd | Mark::ConnectEnd => region = region.saturating_sub(1),
                        Mark::BestEffortEnd => best_effort = best_effort.saturating_sub(1),
                        Mark::PathBegin {
                            path,
                        } => paths.push(path),
                        Mark::PathEnd => {
                            paths.pop();
                        }
                        Mark::StoreFailed {
                            op,
                        } if fatal.is_none() => {
                            fatal = Some(format!("StoreFailed({op})"));
                            cause = FatalCause {
                                read: *op == "read",
                                store: None,
                                paths: paths.clone(),
                            };
                        }
                        Mark::Abort {
                            message,
                        } => {
                            if fatal.is_some() && !sanctioned(message, &cause) {
                                out.push(Violation::new(
                                InvariantId::R8,
                                format!("epoch {epoch} aborted with an unsanctioned message: {message}"),
                            ));
                            }
                            aborted = true;
                        }
                        _ => {}
                    }
                }
                TraceEntry::Event {
                    id,
                    store,
                    op,
                    failed,
                    detail,
                } => {
                    // A failed directory sync is Core's `DirectoryCommit`, whose result is
                    // ignored (DEC-212).
                    let fatal_kind = *op != OpKind::Allocate
                        && (store != crate::faulty::world::FS_STORE
                            || (best_effort == 0 && *op != OpKind::SyncDir));
                    if let Some(first) = &fatal {
                        // Nothing may succeed after the fatal error, before or after the abort.
                        if !failed && !reported {
                            out.push(Violation::new(
                                InvariantId::R8,
                                format!(
                                    "epoch {epoch}: {store} {} {detail} succeeded at event {} after the fatal {first}",
                                    op.name(),
                                    id.index
                                ),
                            ));
                            reported = true;
                        }
                    } else if *failed && fatal_kind {
                        fatal = Some(format!("{store} {} {detail}", op.name()));
                        cause = FatalCause {
                            read: *op == OpKind::Read,
                            store: Some(store.to_string()),
                            paths: paths.clone(),
                        };
                    }
                }
                TraceEntry::ReadFailed {
                    store,
                    op,
                    detail,
                    ..
                } => {
                    if fatal.is_none() && (region > 0 || !paths.is_empty()) {
                        fatal = Some(format!(
                            "failed {store} {} {detail} inside recovery or a connect",
                            op.name()
                        ));
                        cause = FatalCause {
                            read: true,
                            store: Some(store.to_string()),
                            paths: paths.clone(),
                        };
                    }
                }
                TraceEntry::Crash {
                    ..
                } => {}
            }
            if fatal.is_some() {
                first_fatal.entry(epoch).or_insert_with(|| FirstFatal {
                    at: i,
                    cause: cause.clone(),
                });
            }
        }
        if complete && epoch == last_epoch && fatal.is_some() && !aborted {
            out.push(Violation::new(
                InvariantId::R8,
                format!("epoch {epoch} hit a fatal error and did not abort"),
            ));
        }
        // A run that returned without aborting leaves every Core path it entered.
        if end == End::Returned && epoch == last_epoch && !aborted && !paths.is_empty() {
            out.push(Violation::new(
                InvariantId::Model,
                format!("epoch {epoch} ended inside the Core paths {paths:?}"),
            ));
        }
    }
    (out, first_fatal)
}

/// Whether the recovery booted in `world` fail-stopped on unavailable data its own boot's crash
/// tore (DEC-161, §5.5 R7a), with R8 holding. The stop is the operation that failed on an
/// unreadable block at the epoch's first fatal error, or the latest one before it with nothing
/// succeeding in between (an engine reporting the failure as its own error). Its block, and that
/// of every earlier such failure, held the version the boot's crash left with a write unsynced at
/// that crash, under one of that crash's EIO cells: a power loss that keeps the recovery's size
/// and loses its zero-fill exposes an older block under it, still torn.
fn torn_fail_stop(world: &World, sanctioned: &dyn Fn(&str, &FatalCause) -> bool) -> bool {
    let (trace, epoch) = (world.trace(), world.epoch());
    let (violations, first_fatal) = fail_stop_scan(&trace, epoch, End::Refused, sanctioned);
    let Some(fatal) = first_fatal.get(&epoch) else {
        return false;
    };
    let unavailable = world.unavailable();
    let Some(&(at, stop)) = unavailable.iter().rev().find(|(i, _)| *i <= fatal.at) else {
        return false;
    };
    let succeeded = |e: &TraceEntry| {
        matches!(
            e,
            TraceEntry::Event {
                failed: false,
                ..
            }
        )
    };
    violations.is_empty()
        && stop.torn
        && !trace.entries[at..fatal.at].iter().any(succeeded)
        && unavailable.iter().take_while(|(i, _)| *i < at).all(|(_, u)| u.torn)
}

/// The forward run of a scenario with one fault (or none), capturing images in the window.
struct Forward {
    images: Vec<CrashImage>,
    trace: Trace,
    violations: Vec<Violation>,
    work: Work,
    fault_fired: bool,
}

/// Which images a forward run captures.
#[derive(Clone, Copy)]
enum Capture {
    Window,
    At(u64),
}

fn run_forward<S: Sut>(
    sc: &Scenario<S>,
    fault: Option<&Fault>,
    refm: &S::Ref,
    capture: Capture,
) -> Forward {
    let images = Arc::new(Mutex::new(Vec::new()));
    let window = sc.window.clone();
    let from = match fault {
        Some(f) => f.at.clone(),
        None => window.from.clone(),
    };
    let to = window.to.clone();
    let faulted = fault.is_some();
    let state = Arc::new(Mutex::new((false, false)));
    let hook = {
        let images = images.clone();
        move |w: &World, id: crate::faulty::world::EventId| {
            if let Capture::At(at) = capture {
                if id.index == at {
                    images.lock().unwrap_or_else(|e| e.into_inner()).push(w.capture());
                }
                return;
            }
            let Some(info) = w.last_event_info() else {
                return;
            };
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            // A fault run captures from the moment its fault fires (on a mutation or a read).
            let opens = if faulted {
                w.fault_fired()
            } else {
                from.matches(&info)
            };
            if !st.0 && opens {
                st.0 = true;
            }
            if st.0 && !st.1 {
                images.lock().unwrap_or_else(|e| e.into_inner()).push(w.capture());
                if to.as_ref().is_some_and(|t| t.matches(&info)) {
                    st.1 = true;
                }
            }
        }
    };
    let world = World::new(WorldConfig {
        faults: fault.cloned().into_iter().collect(),
        adversary: sc.adversary,
        granularity: sc.granularity,
        allocate: sc.allocate,
        on_event: Some(Box::new(hook)),
        d1: true,
        ..WorldConfig::default()
    });
    if let Some(x) = S::extractor(&sc.cfg) {
        world.set_extractor(x);
    }
    let mut violations = Vec::new();
    let opened = catch_unwind(AssertUnwindSafe(|| S::open(&world, &sc.cfg)));
    let mut end = End::Returned;
    match opened {
        Ok(Ok(mut sut)) => {
            for op in &sc.ops {
                match catch_unwind(AssertUnwindSafe(|| sut.apply(op))) {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => {
                        end = End::Refused;
                        break;
                    }
                    Err(_) => {
                        violations.push(Violation::new(
                            InvariantId::R0,
                            format!("the forward run panicked at {op}"),
                        ));
                        end = End::Open;
                        break;
                    }
                }
            }
            let trace = world.trace();
            violations.extend(sut.forward_check(&world, refm, &trace));
            if fault.is_none()
                && let Err(e) = sut.preconditions(&world, refm, &trace)
            {
                violations.push(Violation::new(InvariantId::Precondition, e));
            }
        }
        Ok(Err(a)) => {
            end = End::Refused;
            if fault.is_none() {
                violations
                    .push(Violation::new(InvariantId::R0, format!("the first open failed: {a}")));
            }
        }
        Err(_) => violations.push(Violation::new(InvariantId::R0, "the first open panicked")),
    }
    let trace = world.trace();
    let fault_fired = world.fault_fired();
    world.freeze();
    violations.extend(world.d1_violations());
    violations.extend(fail_stop_scan(&trace, 0, end, &sanctioned_by::<S>(&sc.cfg)).0);
    let work = world.work();
    let images = std::mem::take(&mut *images.lock().unwrap_or_else(|e| e.into_inner()));
    Forward {
        images,
        trace,
        violations,
        work,
        fault_fired,
    }
}

/// The trace of a scenario's forward run, for asserting its preconditions in a test before any
/// search (§4.5), and the violations the forward run itself reports.
pub fn forward_trace<S: Sut>(sc: &Scenario<S>) -> (Trace, Vec<Violation>) {
    let refm = reference(sc);
    let fwd = run_forward(sc, None, &refm, Capture::At(u64::MAX));
    (fwd.trace, fwd.violations)
}

fn reference<S: Sut>(sc: &Scenario<S>) -> S::Ref {
    let mut r = S::Ref::new(&sc.cfg);
    for op in &sc.ops {
        r.apply(op);
    }
    r
}

/// What reopening a booted world gave.
struct Reopened {
    violations: Vec<Violation>,
    /// How recovery ended, with its projection before R12's continuation changes it (R7a);
    /// `None` if it panicked.
    outcome: Option<Outcome>,
    /// The cells recovery itself decided, before the checks and R12 forced more (what a nested
    /// crash during that recovery inherits).
    recovered: Vec<(crate::faulty::world::Var, u32)>,
    /// Every cell decided once recovery and its projection had run (R7a's inputs).
    projected: Projected,
    /// Recovery fail-stopped, sanctioned, on a block its boot's crash tore (DEC-161).
    torn_stop: bool,
    /// Recovery fail-stopped sanctioned after a fatal error, R8 holding ([`sanctioned_stop`]).
    fail_stop: bool,
}

/// What a nested search is keyed by: the cells recovery itself decided, which its inner crashes
/// inherit. Branches that differ only in cells the checks forced share one.
type NestedKey = Vec<(String, u32)>;

fn nested_key(recovered: &[(crate::faulty::world::Var, u32)]) -> NestedKey {
    recovered.iter().map(|(v, c)| (v.name.clone(), *c)).collect()
}

/// The cells a recovery and its projection decided: all of them, the `^` cells among them, and
/// what each choice of every `^` cell leaves of its input.
#[derive(Clone, Default)]
struct Projected {
    decided: BTreeMap<String, u32>,
    inherited: Vec<InheritedDecision>,
    starts: BTreeMap<String, Arc<[Start]>>,
}

impl Projected {
    fn of(world: &World) -> Self {
        Self {
            decided: world.vars().into_iter().filter_map(|(v, d)| Some((v.name, d?))).collect(),
            inherited: world.inherited_decisions(),
            starts: world.inherited_starts(),
        }
    }
}

/// Opens the SUT on a booted world and runs every check; also R12's continuation if asked.
fn reopen_and_check<S: Sut>(world: &World, sc: &Scenario<S>, refm: &S::Ref, r12: bool) -> Reopened {
    let mut out = Vec::new();
    // D1 keeps running after the crash (§5.3: D1 over recovery's commits). A booted world
    // already carries the image's extractor, evaluated at boot.
    if !world.has_extractor()
        && let Some(x) = S::extractor(&sc.cfg)
    {
        world.set_extractor(x);
    }
    // The checker's reads never take a recovery fault meant for the system (CP2-3).
    out.extend(world.checking(|| S::store_check(world, &sc.cfg)));
    let opened = catch_unwind(AssertUnwindSafe(|| S::open(world, &sc.cfg)));
    // A recovery fault is for recovery: the checks and the continuation run without it.
    world.disarm_faults();
    let recovered: Vec<_> =
        world.vars().into_iter().filter_map(|(v, d)| d.map(|d| (v, d))).collect();
    let done = |violations, outcome, projected, torn_stop, fail_stop| Reopened {
        violations,
        outcome,
        recovered: recovered.clone(),
        projected,
        torn_stop,
        fail_stop,
    };
    let sut = match opened {
        Ok(Ok(s)) => s,
        Ok(Err(a)) => {
            let sanctioned = sanctioned_by::<S>(&sc.cfg);
            let (_, first_fatal) =
                fail_stop_scan(&world.trace(), world.epoch(), End::Refused, &sanctioned);
            let cause =
                first_fatal.get(&world.epoch()).map(|f| f.cause.clone()).unwrap_or_default();
            let ok = sanctioned(&a.message, &cause);
            if !ok {
                out.push(Violation::new(InvariantId::R0, format!("recovery failed: {a}")));
            }
            let torn_stop = ok && torn_fail_stop(world, &sanctioned);
            let projected = Projected::of(world);
            out.extend(recovery_fail_stop::<S>(world, sc, End::Refused));
            world.evaluate_d1();
            out.extend(world.d1_violations());
            let outcome = Outcome::Aborted(a.message);
            let fail_stop = sanctioned_stop::<S>(world, &sc.cfg, &outcome);
            return done(out, Some(outcome), projected, torn_stop, fail_stop);
        }
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            out.push(Violation::new(InvariantId::R0, format!("recovery panicked: {msg}")));
            return done(out, None, Projected::of(world), false, false);
        }
    };
    let projection = sut.projection();
    let projected = Projected::of(world);
    let trace = world.trace();
    match catch_unwind(AssertUnwindSafe(|| sut.check(world, refm, &trace))) {
        Ok(Ok(())) => {}
        Ok(Err(v)) => out.extend(v),
        Err(_) => out.push(Violation::new(InvariantId::R0, "a check panicked")),
    }
    let mut sut = sut;
    out.extend(recovery_fail_stop::<S>(world, sc, End::Returned));
    world.evaluate_d1();
    out.extend(world.d1_violations());
    // R12 runs whenever recovery opened (§5.1: on every witness branch that lists it), so a
    // continuation that builds on a broken image shows what it breaks next (T14's D1(d)).
    if r12 {
        out.extend(r12_continue(world, sc, refm, &mut sut));
    }
    done(out, Some(Outcome::Completed(projection)), projected, false, false)
}

/// R8 over the booted epoch's own trace: recovery must stop at its first fatal error (§4.4).
fn recovery_fail_stop<S: Sut>(world: &World, sc: &Scenario<S>, end: End) -> Vec<Violation> {
    fail_stop_scan(&world.trace(), world.epoch(), end, &sanctioned_by::<S>(&sc.cfg)).0
}

/// R12: one more op, a clean shutdown and restart, and every check again.
fn r12_continue<S: Sut>(
    world: &World,
    sc: &Scenario<S>,
    refm: &S::Ref,
    sut: &mut S,
) -> Vec<Violation> {
    let Some(op) = sut.continue_after_recovery() else {
        return vec![Violation::new(
            InvariantId::Precondition,
            "R12 is on but the system has no continuation (set `continue_r12 = false`)",
        )];
    };
    let tag =
        |v: Violation| Violation::new(InvariantId::R12, format!("after the continuation: {v}"));
    let reported = world.d1_violations().len();
    match catch_unwind(AssertUnwindSafe(|| sut.apply(&op))) {
        Ok(Ok(())) => {}
        Ok(Err(a)) => {
            return vec![Violation::new(
                InvariantId::R12,
                format!("the continuation aborted: {a}"),
            )];
        }
        Err(_) => return vec![Violation::new(InvariantId::R12, "the continuation panicked")],
    }
    // D1 over the continuation's own writes: the violations after those already reported.
    world.evaluate_d1();
    let continued = world.d1_violations().split_off(reported);
    let before = sut.projection();
    let img = world.capture();
    let w2 = img.boot(
        CrashKind::ProcessKill,
        Box::new(crate::faulty::world::AllSurvive),
        boot_cfg(sc.adversary, sc.granularity, sc.allocate),
    );
    let Reopened {
        violations: mut v,
        outcome,
        ..
    } = reopen_and_check(&w2, sc, refm, false);
    v.extend(continued);
    // The same projection across a clean restart.
    if let (Some(a), Some(Outcome::Completed(Some(b)))) = (before, outcome)
        && a != b
    {
        v.push(Violation::new(InvariantId::R12, "a clean restart changed the recovered state"));
    }
    v.into_iter()
        .map(|v| {
            if v.invariant == InvariantId::R12 {
                v
            } else {
                tag(v)
            }
        })
        .collect()
}

struct Explorer<'a, S: Sut> {
    sc: &'a Scenario<S>,
    mode: &'a Mode,
    refm: S::Ref,
    report: Report,
    /// Only this shard's image, kind and outer branches (DEC-185 (2)).
    shard: Option<&'a Shard>,
}

impl<S: Sut> Explorer<'_, S> {
    fn done(&self) -> bool {
        match self.mode {
            Mode::Witness {
                expect,
                max_branches,
            } => {
                let found = self.report.found();
                expect.iter().all(|e| found.contains(e)) || self.report.branches >= *max_branches
            }
            Mode::Exhaustive {
                ..
            } => false,
        }
    }

    fn cap(&self) -> u64 {
        match self.mode {
            Mode::Exhaustive {
                max_branches_per_image,
            } => u64::from(*max_branches_per_image),
            Mode::Witness {
                max_branches,
                ..
            } => *max_branches,
        }
    }

    /// Every branch of one image.
    fn image(&mut self, img: &CrashImage, base: &Repro) {
        self.report.images += 1;
        self.search(img, base, None);
    }

    /// Every branch of one image, both kinds; with `fault`, a recovery-fault pass (§4.4) over
    /// the same image. Returns whether the fault fired in some branch.
    fn search(&mut self, img: &CrashImage, base: &Repro, fault: Option<&Fault>) -> bool {
        let mut fired = false;
        for &kind in &self.sc.kinds {
            // A shard starts with the nested searches its image's earlier branches own, so each
            // runs once, under the representative the unsharded search gives it.
            let (dfs, mut nested_seen) = match self.shard {
                Some(s) if s.kind != kind => continue,
                Some(s) => (Dfs::pinned(s.prefix.clone()), self.owned_before(img, kind, &s.prefix)),
                None => (Dfs::new(), BTreeSet::new()),
            };
            let mut n = 0u64;
            let mut seen: BTreeSet<String> = BTreeSet::new();
            loop {
                if self.done() {
                    return fired;
                }
                n += 1;
                self.report.branches += 1;
                let cfg = faulty_boot_cfg(
                    self.sc.adversary,
                    self.sc.granularity,
                    self.sc.allocate,
                    fault,
                );
                let w = img.boot(kind, dfs.decider(), cfg);
                // R12 runs on each image's all-survived branch, and on every branch of a witness
                // search that expects it (§5.1); not after an injected recovery fault. A shard's
                // first branch is the image's all-survived one only under an all-zero prefix.
                let witness_r12 = matches!(self.mode, Mode::Witness { expect, .. } if expect.contains(&InvariantId::R12));
                let all_survived =
                    n == 1 && self.shard.is_none_or(|s| s.prefix.iter().all(|&c| c == 0));
                let r12 = self.sc.continue_r12
                    && fault.is_none()
                    && (all_survived || witness_r12 || self.sc.r12_all);
                let Reopened {
                    violations: mut v,
                    outcome,
                    recovered,
                    projected,
                    fail_stop,
                    ..
                } = reopen_and_check(&w, self.sc, &self.refm, r12);
                fired |= w.fault_fired();
                if let Some(m) = dfs.mismatch() {
                    v.push(Violation::new(InvariantId::Model, m));
                }
                seen.extend(
                    dfs.decisions().iter().map(|(v, _)| format!("{}/{}", v.name, v.domain)),
                );
                let mut repro = base.clone();
                repro.kind = kind;
                repro.recovery_fault = fault.cloned();
                repro.decisions = nonzero(&dfs.decisions());
                for x in v {
                    self.report.add(x, &repro);
                }
                self.report.work.add(&w.work());
                // One nested search per distinct set of recovery decisions: branches that differ
                // only in cells the checks forced share the same recovery to crash into.
                if self.sc.nested != Nested::None
                    && fault.is_none()
                    && nested_seen.insert(nested_key(&recovered))
                {
                    let choices: BTreeMap<String, u32> =
                        recovered.iter().map(|(v, c)| (v.name.clone(), *c)).collect();
                    let outer = Outer {
                        img,
                        kind,
                        recovered: &choices,
                        decided: &projected.decided,
                        p0: outcome,
                        p0_stop: fail_stop,
                    };
                    let (branches, r7a) = (self.report.branches, mem::take(&mut self.report.r7a));
                    self.nested(&outer, &repro);
                    let nested = NestedTotals {
                        branches: self.report.branches - branches,
                        r7a: mem::replace(&mut self.report.r7a, r7a),
                    };
                    self.report.r7a.merge(nested.r7a);
                    self.report
                        .per_image_nested
                        .entry((img.at.index, kind))
                        .or_default()
                        .merge(nested);
                }
                if !dfs.advance() {
                    break;
                }
                if n >= self.cap() {
                    if matches!(self.mode, Mode::Exhaustive { .. }) {
                        self.report.truncated += 1;
                    }
                    break;
                }
            }
            self.report.max_branches_per_image = self.report.max_branches_per_image.max(n);
            self.report.per_image.push((img.at.index, kind, n, seen.into_iter().collect()));
        }
        fired
    }

    /// The nested-search keys of `img`'s branches (crashed as `kind`) that the unsharded search
    /// visits before `prefix`'s first branch: the searches an earlier shard owns (DEC-185 (2),
    /// CP3.4-1). A nested search is keyed by its recovery's own decisions, and checker or
    /// projection cells after them can put one recovery in several shards; its search belongs to
    /// the first, as unsharded. Replays those outer branches as `search` runs them (R12 on the
    /// first), without nested searches and reporting nothing.
    fn owned_before(
        &self,
        img: &CrashImage,
        kind: CrashKind,
        prefix: &[u32],
    ) -> BTreeSet<NestedKey> {
        let mut owned = BTreeSet::new();
        let dfs = Dfs::new();
        let mut n = 0u64;
        loop {
            n += 1;
            let cfg =
                faulty_boot_cfg(self.sc.adversary, self.sc.granularity, self.sc.allocate, None);
            let w = img.boot(kind, dfs.decider(), cfg);
            let r12 = self.sc.continue_r12 && (n == 1 || self.sc.r12_all);
            let recovered = reopen_and_check(&w, self.sc, &self.refm, r12).recovered;
            let taken = dfs.decisions();
            if taken.len() >= prefix.len() && taken.iter().zip(prefix).all(|((_, c), p)| c == p) {
                return owned;
            }
            owned.insert(nested_key(&recovered));
            assert!(dfs.advance(), "the shard's prefix is not in the image's tree");
        }
    }

    /// Depth 1: crash after every event of this branch's recovery. An inner image whose branches
    /// exceed the cap is truncated, and with [`Scenario::over_cap`] also sampled past it, as a
    /// smoke that can only find (DEC-185 (1)).
    fn nested(&mut self, outer: &Outer<'_>, base: &Repro) {
        let (img, kind, choices) = (outer.img, outer.kind, outer.recovered);
        let inner = recovery_images::<S>(img, kind, choices, self.sc.boot_cfg(), &self.sc.cfg);
        let mut memo = R7aMemo::new();
        for (j, inner_img) in inner.iter().enumerate() {
            for &k2 in &self.sc.kinds {
                let dfs = Dfs::new();
                let mut n = 0u64;
                // The branches explored, by their inner decisions (what sampling skips).
                let mut explored = BTreeSet::new();
                loop {
                    if self.done() {
                        return;
                    }
                    n += 1;
                    // The outer branch's decisions stay part of every inner branch (L4).
                    let decider = Box::new(Chain(Fixed::new(choices.clone()), dfs.clone()));
                    dfs.decider();
                    let mut accept = |d| {
                        explored.insert(d);
                        true
                    };
                    let decisions = || dfs.decisions();
                    let branch = (inner_img, j, k2);
                    let repro = self
                        .nested_branch(
                            outer,
                            branch,
                            decider,
                            &decisions,
                            &mut accept,
                            base,
                            &mut memo,
                        )
                        .expect("every explored branch is reported");
                    // Depth 2 on a seeded sample of inner crashes, from their first branch.
                    if let Nested::Depth2Sample(per_mille) = self.sc.nested
                        && n == 1
                        && Seeded::new(img.at.index ^ ((j as u64) << 20) ^ ((k2 as u64) << 40))
                            .chance(u64::from(per_mille))
                    {
                        let mut fixed = choices.clone();
                        fixed.extend(dfs.decisions().into_iter().map(|(v, c)| (v.name, c)));
                        self.depth2(inner_img, k2, fixed, &repro);
                    }
                    if !dfs.advance() || self.done() {
                        break;
                    }
                    if n >= self.cap() {
                        if matches!(self.mode, Mode::Exhaustive { .. }) {
                            self.report.truncated += 1;
                            if let Some(o) = self.sc.over_cap {
                                let explored = std::mem::take(&mut explored);
                                let branch = (inner_img, j, k2);
                                self.sample_rest(o, outer, branch, explored, base, &mut memo);
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    /// One nested branch of inner image `j` (crashed as `k2`) under `decider`: every check and
    /// R7a, reported with the inner decisions `decisions` lists once it ran, unless `accept`
    /// rejects those (a branch already run). Returns its repro if it was reported.
    #[allow(clippy::too_many_arguments)]
    fn nested_branch(
        &mut self,
        outer: &Outer<'_>,
        (inner_img, j, k2): (&CrashImage, usize, CrashKind),
        decider: Box<dyn Decider>,
        decisions: &dyn Fn() -> Vec<(Var, u32)>,
        accept: &mut dyn FnMut(Vec<(String, u32)>) -> bool,
        base: &Repro,
        memo: &mut R7aMemo,
    ) -> Option<Repro> {
        let w2 = inner_img.boot(k2, decider, self.sc.boot_cfg());
        let Reopened {
            violations: mut v,
            outcome: p1,
            projected,
            torn_stop,
            fail_stop,
            ..
        } = reopen_and_check(&w2, self.sc, &self.refm, false);
        if !accept(names(&decisions())) {
            return None;
        }
        self.report.branches += 1;
        let clean = v.is_empty();
        let verdict =
            r7a_judge(outer, k2, &projected, p1, (torn_stop, fail_stop), clean, self.sc, memo);
        self.report.r7a.add(verdict);
        v.extend(r7a_violation(verdict, j));
        let mut repro = base.clone();
        repro.nested = Some((j as u64, k2));
        repro.decisions.extend(nonzero(&decisions()));
        for x in v {
            self.report.add(x, &repro);
        }
        self.report.work.add(&w2.work());
        Some(repro)
    }

    /// Past the cap: up to `o.samples` more branches of inner image `j`, each drawn from a seed
    /// derived from `o.seed` and the image, every cell the branch decides drawn uniformly from its
    /// domain. A draw that repeats an explored or sampled branch is not reported, and draws stop
    /// at `8 × o.samples`. The walk is uniform per cell, not over the remaining branches: a dense
    /// subtree behind a long path is almost never drawn (Sol's `Skew`), so the sample is a smoke
    /// that can only find, and the image stays truncated (DEC-185 (1), superseding DEC-165 (2)).
    fn sample_rest(
        &mut self,
        o: OverCap,
        outer: &Outer<'_>,
        branch: (&CrashImage, usize, CrashKind),
        mut seen: BTreeSet<Vec<(String, u32)>>,
        base: &Repro,
        memo: &mut R7aMemo,
    ) {
        let (_, j, k2) = branch;
        // The seed, the outer branch (image, kind and the cells its recovery decided) and the
        // inner crash: each outer branch samples its own stream.
        let mut h = o.seed ^ outer.img.at.index.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for (name, c) in outer.recovered {
            for b in name.bytes().chain(c.to_le_bytes()) {
                h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        let mut rng =
            Seeded::new(h ^ ((j as u64) << 24) ^ ((outer.kind as u64) << 52) ^ ((k2 as u64) << 56));
        self.report.sampled += 1;
        let (mut ran, mut draws) = (0u64, 0u64);
        while ran < o.samples && draws < 8 * o.samples && !self.done() {
            draws += 1;
            let sd = SeededDecider::new(rng.next_u64());
            let decider = Box::new(Chain(Fixed::new(outer.recovered.clone()), sd.clone()));
            let mut accept = |d| seen.insert(d);
            let decisions = || sd.decisions();
            if self
                .nested_branch(outer, branch, decider, &decisions, &mut accept, base, memo)
                .is_some()
            {
                ran += 1;
                self.report.sampled_branches += 1;
            }
        }
    }

    /// A second crash after every event of the recovery from an inner crash image, both kinds,
    /// one branch each (§4.2 depth 2; R7b: every check over the combined trace).
    fn depth2(
        &mut self,
        inner_img: &CrashImage,
        k2: CrashKind,
        fixed: BTreeMap<String, u32>,
        base: &Repro,
    ) {
        let images = recovery_images::<S>(inner_img, k2, &fixed, self.sc.boot_cfg(), &self.sc.cfg);
        for (j2, img2) in images.iter().enumerate() {
            for &k3 in &self.sc.kinds {
                if self.done() {
                    return;
                }
                self.report.branches += 1;
                // One seeded branch: Fixed's zero default would always take the all-survived one.
                let seed = img2.at.index ^ ((j2 as u64) << 24) ^ ((k3 as u64) << 48);
                let third = SeededDecider::new(seed);
                let decider = Chain(Fixed::new(fixed.clone()), third.clone());
                let w3 = img2.boot(k3, Box::new(decider), self.sc.boot_cfg());
                let v = reopen_and_check(&w3, self.sc, &self.refm, false).violations;
                let mut repro = base.clone();
                repro.nested2 = Some((j2 as u64, k3));
                repro.decisions.extend(nonzero(&third.decisions()));
                for x in v {
                    self.report.add(x, &repro);
                }
                self.report.work.add(&w3.work());
            }
        }
    }
}

/// The images after every event of the recovery that boots `img` as `kind` under `fixed`
/// decisions (a nested crash's inner images).
fn recovery_images<S: Sut>(
    img: &CrashImage,
    kind: CrashKind,
    fixed: &BTreeMap<String, u32>,
    cfg: WorldConfig,
    sut_cfg: &S::Config,
) -> Vec<CrashImage> {
    let decider = Box::new(Fixed::new(fixed.clone()));
    let (images, ()) = capture_recovery(img, kind, decider, cfg, |w| {
        let _ = catch_unwind(AssertUnwindSafe(|| S::open(w, sut_cfg)));
    });
    images
}

/// Boots `img` as `kind` under `decider`, capturing an image after every event, and runs
/// `recover` on the booted world before freezing it. Returns the images and `recover`'s result.
fn capture_recovery<T>(
    img: &CrashImage,
    kind: CrashKind,
    decider: Box<dyn Decider>,
    cfg: WorldConfig,
    recover: impl FnOnce(&World) -> T,
) -> (Vec<CrashImage>, T) {
    let images = Arc::new(Mutex::new(Vec::new()));
    let hook = {
        let images = images.clone();
        move |w: &World, _| images.lock().unwrap_or_else(|e| e.into_inner()).push(w.capture())
    };
    let w = img.boot(
        kind,
        decider,
        WorldConfig {
            on_event: Some(Box::new(hook)),
            ..cfg
        },
    );
    let out = recover(&w);
    w.freeze();
    let images = std::mem::take(&mut *images.lock().unwrap_or_else(|e| e.into_inner()));
    (images, out)
}

/// How a recovery ended: it completed (with its projection), or aborted with a message.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Outcome {
    Completed(Option<Hash>),
    Aborted(String),
}

/// Opens the system on a booted world: how its recovery ended, `None` if it panicked.
fn open_outcome<S: Sut>(world: &World, cfg: &S::Config) -> Option<Outcome> {
    outcome_of(catch_unwind(AssertUnwindSafe(|| S::open(world, cfg))))
}

fn outcome_of<S: Sut>(opened: std::thread::Result<Result<S, Abort>>) -> Option<Outcome> {
    match opened.ok()? {
        Ok(s) => catch_unwind(AssertUnwindSafe(|| s.projection())).ok().map(Outcome::Completed),
        Err(a) => Some(Outcome::Aborted(a.message)),
    }
}

/// An uninterrupted recovery's run: how it ended, the cells it decided before the projection
/// ran (what an inner crash of that recovery inherits), those decided once it had, and whether it
/// ended in a [`sanctioned_stop`].
type Recorded = (Option<Outcome>, BTreeMap<String, u32>, BTreeMap<String, u32>, bool);

/// [`open_outcome`] on a booted world, recorded as [`Recorded`].
fn open_recorded<S: Sut>(world: &World, cfg: &S::Config) -> Recorded {
    let opened = catch_unwind(AssertUnwindSafe(|| S::open(world, cfg)));
    let recovered = Projected::of(world).decided;
    let outcome = outcome_of(opened);
    let stop = outcome.as_ref().is_some_and(|o| sanctioned_stop::<S>(world, cfg, o));
    (outcome, recovered, Projected::of(world).decided, stop)
}

/// A sanctioned fail-stop: an abort after the epoch's first fatal error, with a message
/// sanctioned for that error's cause, R8 holding. An abort with no fatal error before it is a
/// refusal, not a fail-stop (DEC-165 (1)).
fn sanctioned_stop<S: Sut>(world: &World, cfg: &S::Config, outcome: &Outcome) -> bool {
    let Outcome::Aborted(m) = outcome else {
        return false;
    };
    let sanctioned = sanctioned_by::<S>(cfg);
    let (r8, first_fatal) =
        fail_stop_scan(&world.trace(), world.epoch(), End::Refused, &sanctioned);
    r8.is_empty() && first_fatal.get(&world.epoch()).is_some_and(|f| sanctioned(m, &f.cause))
}

/// A reference's outcome (DEC-162): [`open_outcome`], or `None`, witnessing nothing, if its
/// recovery panicked, broke R8, or aborted with a message not sanctioned for its cause.
fn reference_outcome<S: Sut>(world: &World, cfg: &S::Config) -> Option<Outcome> {
    let outcome = open_outcome::<S>(world, cfg)?;
    let sanctioned = sanctioned_by::<S>(cfg);
    let end = match outcome {
        Outcome::Completed(_) => End::Returned,
        Outcome::Aborted(_) => End::Refused,
    };
    let (r8, first_fatal) = fail_stop_scan(&world.trace(), world.epoch(), end, &sanctioned);
    let ok = match &outcome {
        Outcome::Completed(_) => true,
        Outcome::Aborted(m) => sanctioned(
            m,
            &first_fatal.get(&world.epoch()).map(|f| f.cause.clone()).unwrap_or_default(),
        ),
    };
    (ok && r8.is_empty()).then_some(outcome)
}

/// The outer image of a nested branch: crashed as `kind` at `img`, the cells its uninterrupted
/// recovery decided before the projection ran (`recovered`, which R7a's assignment fixes), those
/// it and its projection decided (`decided`), how it ended (`p0`), and whether that was a
/// [`sanctioned_stop`] (`p0_stop`). Explore's inner branches
/// inherit `recovered`; the sweep's and replay's inherit every outer decision, and R7a enumerates
/// any projection cell an inner branch did not read.
struct Outer<'a> {
    img: &'a CrashImage,
    kind: CrashKind,
    recovered: &'a BTreeMap<String, u32>,
    decided: &'a BTreeMap<String, u32>,
    p0: Option<Outcome>,
    p0_stop: bool,
}

/// R7a's verdict on a nested branch (§5.5, DEC-162).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum R7aVerdict {
    Converged,
    /// Reproduced under every completion of its assignment; `after` if some completion needed
    /// reference (a′).
    Witnessed {
        after: bool,
    },
    SanctionedFailStop,
    RecoveredAfterFailstop,
    FailstopAfterFailstop,
    Unjudged,
    Violation(&'static str),
}

impl R7aCounts {
    fn add(&mut self, v: R7aVerdict) {
        match v {
            R7aVerdict::Converged => self.converged += 1,
            R7aVerdict::Witnessed {
                after: true,
            } => self.witnessed_after += 1,
            R7aVerdict::Witnessed {
                after: false,
            } => self.witnessed_before += 1,
            R7aVerdict::SanctionedFailStop => self.sanctioned_fail_stop += 1,
            R7aVerdict::RecoveredAfterFailstop => self.recovered_after_failstop += 1,
            R7aVerdict::FailstopAfterFailstop => self.failstop_after_failstop += 1,
            R7aVerdict::Unjudged => self.unjudged += 1,
            R7aVerdict::Violation(_) => {}
        }
    }
}

/// What the references made of one branch's assignments (memoised per outer branch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Witness {
    /// Every completion reproduced the branch; `after` if some needed reference (a′).
    All {
        after: bool,
    },
    /// Some completion reproduced it under neither reference.
    Missed,
    /// More completions than the bound: neither passed nor failed.
    Over,
}

/// A branch's witness search, keyed by its inner kind, its assignment of the outer image's
/// cells, the inherited inputs its inner crash decided and left undecided (with what each choice
/// of those leaves), and its outcome.
type R7aKey =
    (CrashKind, BTreeMap<String, u32>, Vec<(String, Start)>, Vec<(String, Arc<[Start]>)>, Outcome);
type R7aMemo = BTreeMap<R7aKey, Witness>;

/// R7a (§5.5) under the witness rule (DEC-162). A nested branch converges if its recovery ended
/// as the uninterrupted one did (`p0`), it decided every outer cell they share alike, and it
/// decided every outer cell the uninterrupted projection alone decided. Otherwise
/// it passes only if a concrete reference, run uninterrupted under the same complete input
/// assignment, ends exactly as it did (the same projection, or the same abort), under every
/// completion of the cells the branch left undecided. A reference that panicked, broke R8 or
/// aborted unsanctioned witnesses nothing, and a branch that panicked has nothing to witness. The
/// assignment fixes the outer image's cells as the branch decided them (else as the uninterrupted
/// recovery, which it inherits, did), and the inherited inputs as its inner crash left them. The
/// references:
///
/// - (a′) the uninterrupted recovery, then a crash of the inner kind whose `^` cells leave the
///   inherited inputs still pending as the branch did (the rest intact), then recovery; with every
///   inherited input intact, the uninterrupted recovery itself;
/// - (b) the outer image crashed as the inner kind before its recovery's first event, its `^`
///   cells matching every input the branch changed, then recovery.
///
/// A completion fixes each undecided cell to one value for every reference; more than
/// [`Scenario::r7a_assignments`] completions, or references that do not decide alike across
/// completions, leave the branch unjudged, never passed. A recovery
/// that fail-stopped, sanctioned, on a block its inner crash tore (`torn_stop`, DEC-161) is
/// allowed and counted apart. So is one that completed where the uninterrupted recovery
/// fail-stopped sanctioned, if every other check passed on it (`checks_clean`, DEC-165 (1)): Core
/// may start after a fail-stop, and the checks, not an inference about the recovery's own cells,
/// judge the state it completed with. If any other check failed, R7a fails it too. A recovery
/// that fail-stopped sanctioned (`p1_stop`) where the uninterrupted one did, but which no
/// reference reproduced, is allowed the same way, counted apart (I42): a later start may also
/// stop on another fatal error.
#[allow(clippy::too_many_arguments)]
fn r7a_judge<S: Sut>(
    outer: &Outer<'_>,
    k2: CrashKind,
    projected: &Projected,
    p1: Option<Outcome>,
    (torn_stop, p1_stop): (bool, bool),
    checks_clean: bool,
    sc: &Scenario<S>,
    memo: &mut R7aMemo,
) -> R7aVerdict {
    let prefix: Arc<str> = format!("{}:", outer.img.epoch() + 2).into();
    let inner = |name: &str| name.starts_with(&*prefix);
    let own = || projected.decided.iter().filter(|(n, _)| !inner(n));
    let differs = own().any(|(n, c)| outer.decided.get(n).is_some_and(|d| d != c));
    // A cell only the uninterrupted projection decided, which the branch left undecided: its other
    // values are judged nowhere else (explore runs one nested search per recovery decisions), so
    // the branch is judged under each of them.
    let unshared = outer.decided.keys().any(|n| {
        !inner(n) && !outer.recovered.contains_key(n) && !projected.decided.contains_key(n)
    });
    let Some(p1) = p1 else {
        return R7aVerdict::Violation("did not complete");
    };
    if Some(&p1) == outer.p0.as_ref() && !differs && !unshared {
        return R7aVerdict::Converged;
    }
    if matches!(p1, Outcome::Aborted(_)) && torn_stop {
        return R7aVerdict::SanctionedFailStop;
    }
    if matches!(p1, Outcome::Completed(_)) && outer.p0_stop {
        return if checks_clean {
            R7aVerdict::RecoveredAfterFailstop
        } else {
            R7aVerdict::Violation(
                "completed after the uninterrupted recovery's fail-stop and failed a check",
            )
        };
    }
    let mut fixed: BTreeMap<String, u32> =
        outer.recovered.iter().filter(|(n, _)| !inner(n)).map(|(n, c)| (n.clone(), *c)).collect();
    fixed.extend(own().map(|(n, c)| (n.clone(), *c)));
    let decided: Vec<(String, Start)> = projected
        .inherited
        .iter()
        .filter(|d| inner(&d.name))
        .map(|d| (d.name.clone(), d.start))
        .collect();
    let free: BTreeMap<String, Arc<[Start]>> = projected
        .starts
        .iter()
        .filter(|(n, _)| inner(n) && !decided.iter().any(|(d, _)| d == *n))
        .map(|(n, s)| (n.clone(), s.clone()))
        .collect();
    let key = (k2, fixed, decided, free.iter().map(|(n, s)| (n.clone(), s.clone())).collect(), p1);
    let key_outcome = key.4.clone();
    let found = match memo.get(&key) {
        Some(w) => *w,
        None => {
            let refs = References {
                outer,
                k2,
                sc,
                prefix,
                fixed: Arc::new(key.1.clone()),
                decided: &key.2,
                free: &free,
                p1: &key.4,
            };
            let w = refs.search();
            memo.insert(key, w);
            w
        }
    };
    match found {
        Witness::All {
            after,
        } => R7aVerdict::Witnessed {
            after,
        },
        Witness::Over => R7aVerdict::Unjudged,
        Witness::Missed if matches!(key_outcome, Outcome::Completed(_)) => {
            R7aVerdict::Violation("diverged")
        }
        Witness::Missed if outer.p0_stop && p1_stop => {
            if checks_clean {
                R7aVerdict::FailstopAfterFailstop
            } else {
                R7aVerdict::Violation(
                    "fail-stopped after the uninterrupted recovery's fail-stop and failed a check",
                )
            }
        }
        Witness::Missed => R7aVerdict::Violation("did not complete"),
    }
}

/// R7a's violation for a verdict, after inner event `j`.
fn r7a_violation(v: R7aVerdict, j: usize) -> Option<Violation> {
    let R7aVerdict::Violation(detail) = v else {
        return None;
    };
    Some(Violation::new(
        InvariantId::R7a,
        format!("nested recovery after inner event {j} {detail}"),
    ))
}

/// The references of one nested branch (see [`r7a_judge`]).
struct References<'a, S: Sut> {
    outer: &'a Outer<'a>,
    k2: CrashKind,
    sc: &'a Scenario<S>,
    /// The name prefix of the inner crash's cells (and of the references' own inner crashes).
    prefix: Arc<str>,
    fixed: Arc<BTreeMap<String, u32>>,
    /// The inherited inputs the inner crash decided, and those it left undecided.
    decided: &'a [(String, Start)],
    free: &'a BTreeMap<String, Arc<[Start]>>,
    p1: &'a Outcome,
}

impl<S: Sut> References<'_, S> {
    /// Every completion of the assignment, depth-first over the cells the references decide.
    fn search(&self) -> Witness {
        let joint = Joint::default();
        let changed = self.decided.iter().any(|(_, s)| *s != Start::Intact);
        let mut after = false;
        for _ in 0..self.sc.r7a_assignments {
            joint.begin();
            let one = self.one(&joint, changed);
            if joint.poisoned() {
                return Witness::Over;
            }
            match one {
                Some(a) => after |= a,
                None => return Witness::Missed,
            }
            let more = joint.advance();
            if joint.poisoned() {
                return Witness::Over;
            }
            if !more {
                return Witness::All {
                    after,
                };
            }
        }
        Witness::Over
    }

    /// One completion: `Some(after)` if a reference reproduced the branch (`after`: (a′)).
    fn one(&self, joint: &Joint, changed: bool) -> Option<bool> {
        let decider = RefDecider {
            fixed: self.fixed.clone(),
            prefix: self.prefix.clone(),
            crash: BTreeMap::new(),
            free: BTreeMap::new(),
            joint: joint.clone(),
            unmatched: Arc::new(AtomicBool::new(false)),
        };
        let boot =
            || self.outer.img.boot(self.outer.kind, Box::new(decider.clone()), self.sc.boot_cfg());
        if !changed {
            let p = reference_outcome::<S>(&boot(), &self.sc.cfg);
            return (p.as_ref() == Some(self.p1)).then_some(true);
        }
        // (b): the cheaper one first.
        let w = boot().capture().boot(self.k2, Box::new(decider.clone()), self.sc.boot_cfg());
        if self.reproduces(&w, &decider, true) {
            return Some(false);
        }
        // (a′): only if the uninterrupted recovery made an event to crash after.
        // Its first leg is a reference itself: one that panicked, broke R8 or aborted unsanctioned
        // witnesses nothing.
        let w = boot();
        if reference_outcome::<S>(&w, &self.sc.cfg).is_some() && w.events() > 0 {
            let w = w.capture().boot(self.k2, Box::new(decider.clone()), self.sc.boot_cfg());
            if self.reproduces(&w, &decider, false) {
                return Some(true);
            }
        }
        None
    }

    /// Whether `w`, booted as the inner kind, reproduces the branch once its `^` cells leave the
    /// inherited inputs as the branch did. `all`: every input the branch changed has a cell here
    /// ((b)); else only those still pending ((a′)).
    fn reproduces(&self, w: &World, base: &RefDecider, all: bool) -> bool {
        let starts = w.inherited_starts();
        let mut decider = base.clone();
        decider.unmatched = Arc::new(AtomicBool::new(false));
        for (name, start) in self.decided {
            match starts.get(name) {
                Some(s) => match s.iter().position(|x| x == start) {
                    Some(c) => {
                        decider.crash.insert(name.clone(), c as u32);
                    }
                    None => return false,
                },
                None if all && *start != Start::Intact => return false,
                None => {}
            }
        }
        for (name, branch) in self.free {
            if let Some(s) = starts.get(name) {
                let per: Vec<Option<u32>> = branch
                    .iter()
                    .map(|b| s.iter().position(|x| x == b).map(|c| c as u32))
                    .collect();
                decider.free.insert(name.clone(), per);
            }
        }
        let unmatched = decider.unmatched.clone();
        w.set_decider(Box::new(decider));
        let p = reference_outcome::<S>(w, &self.sc.cfg);
        p.as_ref() == Some(self.p1) && !unmatched.load(Ordering::Relaxed)
    }
}

/// One completion of a branch's assignment, shared by its references (DEC-162): each cell they
/// decide takes one value across them, enumerated depth-first over completions.
#[derive(Clone, Default)]
struct Joint(Arc<Mutex<JointState>>);

#[derive(Default)]
struct JointState {
    /// The cells decided in order, each with its value and domain.
    stack: Vec<(String, u32, u32)>,
    at: usize,
    /// This completion's values, with their domains.
    chosen: BTreeMap<String, (u32, u32)>,
    /// The references decided a cell out of order, under another domain or outside its own, or
    /// left one unreached that an earlier completion reached: no verdict.
    poisoned: bool,
}

impl Joint {
    fn begin(&self) {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.at = 0;
        s.chosen.clear();
    }

    fn poison(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).poisoned = true;
    }

    fn poisoned(&self) -> bool {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).poisoned
    }

    fn choose(&self, name: &str, domain: u32) -> u32 {
        let mut guard = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let s = &mut *guard;
        if let Some(&(c, d)) = s.chosen.get(name) {
            if d != domain {
                s.poisoned = true;
                return 0;
            }
            return c;
        }
        let c = match s.stack.get(s.at) {
            Some((n, c, d)) if n == name && *d == domain => *c,
            None => {
                s.stack.push((name.to_string(), 0, domain));
                0
            }
            // The references decide deterministically: a different cell here leaves the
            // enumeration unsound, so the branch is unjudged.
            Some(_) => {
                s.poisoned = true;
                s.stack.truncate(s.at);
                s.stack.push((name.to_string(), 0, domain));
                0
            }
        };
        s.at += 1;
        s.chosen.insert(name.to_string(), (c, domain));
        c
    }

    /// The next completion; `false` once every one has been visited.
    fn advance(&self) -> bool {
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // The references decide deterministically, so every completion reaches the cell the last
        // one advanced; one that stopped short is no completion of the same enumeration.
        if s.at < s.stack.len() {
            s.poisoned = true;
            let at = s.at;
            s.stack.truncate(at);
        }
        while let Some((_, c, domain)) = s.stack.last_mut() {
            if *c + 1 < *domain {
                *c += 1;
                return true;
            }
            s.stack.pop();
        }
        false
    }
}

/// A reference's decider: the assignment's fixed cells; its own inner crash's `^` cells by the
/// branch's inherited inputs (`crash`, and `free` per choice of an input the branch left
/// undecided, `None` where this crash cannot leave it alike); the rest of that crash intact; every
/// other cell from the shared completion.
#[derive(Clone)]
struct RefDecider {
    fixed: Arc<BTreeMap<String, u32>>,
    prefix: Arc<str>,
    crash: BTreeMap<String, u32>,
    free: BTreeMap<String, Vec<Option<u32>>>,
    joint: Joint,
    /// Set when a completion gives an input a value this crash cannot leave alike.
    unmatched: Arc<AtomicBool>,
}

impl Decider for RefDecider {
    fn choose(&mut self, var: &crate::faulty::world::Var) -> u32 {
        if let Some(&c) = self.fixed.get(&var.name) {
            if c >= var.domain {
                self.joint.poison();
                return 0;
            }
            return c;
        }
        if !var.name.starts_with(&*self.prefix) {
            return self.joint.choose(&var.name, var.domain);
        }
        if let Some(&c) = self.crash.get(&var.name) {
            return c;
        }
        match self.free.get(&var.name) {
            Some(per) => {
                let v = self.joint.choose(&var.name, per.len() as u32);
                per[v as usize].unwrap_or_else(|| {
                    self.unmatched.store(true, Ordering::Relaxed);
                    0
                })
            }
            None => 0,
        }
    }
}

/// Decides a variable from `0` if it names it, else from `1` (outer, then inner decisions).
struct Chain<D: Decider>(Fixed, D);

impl<D: Decider> Decider for Chain<D> {
    fn choose(&mut self, var: &crate::faulty::world::Var) -> u32 {
        match self.0.choices.get(&var.name) {
            Some(&c) => c.min(var.domain - 1),
            None => self.1.choose(var),
        }
    }
}

fn nonzero(d: &[(crate::faulty::world::Var, u32)]) -> BTreeMap<String, u32> {
    d.iter().filter(|(_, c)| *c != 0).map(|(v, c)| (v.name.clone(), *c)).collect()
}

/// A branch's identity: every cell it decided, by name.
fn names(d: &[(Var, u32)]) -> Vec<(String, u32)> {
    let mut n: Vec<_> = d.iter().map(|(v, c)| (v.name.clone(), *c)).collect();
    n.sort();
    n
}

/// One seeded sweep (§4.6): `cases` scenarios from `generate`, each with one crash point (weighted
/// 10:1 toward flush and recovery windows), a kind, with p = 0.2 one fault at a mutating event,
/// with p = 0.1 a depth-1 nested crash, and a single seeded branch. Findings carry their
/// decisions, so their repros replay.
pub fn sweep<S: Sut>(
    cases: u64,
    seed: u64,
    generate: &dyn Fn(&mut Seeded) -> Scenario<S>,
) -> Report {
    let mut rng = Seeded::new(seed);
    let mut report = Report::default();
    for _ in 0..cases {
        let sc = generate(&mut rng);
        let refm = reference(&sc);
        let plain = run_forward(&sc, None, &refm, Capture::At(u64::MAX));
        let fault = if rng.chance(200) {
            pick_fault(&plain.trace, &mut rng)
        } else {
            None
        };
        let trace = match &fault {
            Some(f) => run_forward(&sc, Some(f), &refm, Capture::At(u64::MAX)).trace,
            None => plain.trace,
        };
        let Some(at) = pick_crash(&trace, &mut rng) else {
            continue;
        };
        let fwd = run_forward(&sc, fault.as_ref(), &refm, Capture::At(at));
        report.work.add(&fwd.work);
        let mut repro = Repro::new::<S>(&sc, fault.clone());
        for v in fwd.violations {
            report.add(v, &repro);
        }
        let Some(img) = fwd.images.first() else {
            continue;
        };
        report.images += 1;
        report.branches += 1;
        let kind = pick_kind(&mut rng);
        repro.crash_at = Some(at);
        repro.kind = kind;
        let outer = SeededDecider::new(rng.next_u64());
        let (v, w) = if rng.chance(100) {
            match sweep_nested(&sc, &refm, img, kind, &outer, &mut rng, &mut repro) {
                Some((v, w, counts)) => {
                    report.r7a.merge(counts);
                    (v, w)
                }
                None => continue,
            }
        } else {
            let w = img.boot(kind, Box::new(outer.clone()), sc.boot_cfg());
            let v = reopen_and_check(&w, &sc, &refm, sc.continue_r12).violations;
            repro.decisions = nonzero(&outer.decisions());
            (v, w)
        };
        report.work.add(&w.work());
        for x in v {
            report.add(x, &repro);
        }
    }
    report
}

/// The sweep's depth-1 case: the outer recovery of `img` under the seeded branch `outer`, then
/// one inner crash with one seeded branch. Returns its violations, the inner world and how R7a
/// judged it, or `None` if the outer recovery made no event to crash after.
fn sweep_nested<S: Sut>(
    sc: &Scenario<S>,
    refm: &S::Ref,
    img: &CrashImage,
    kind: CrashKind,
    outer: &SeededDecider,
    rng: &mut Seeded,
    repro: &mut Repro,
) -> Option<(Vec<Violation>, World, R7aCounts)> {
    let (images, (p0, recovered, decided, p0_stop)) =
        capture_recovery(img, kind, Box::new(outer.clone()), sc.boot_cfg(), |w| {
            open_recorded::<S>(w, &sc.cfg)
        });
    if images.is_empty() {
        return None;
    }
    let j = rng.below(images.len() as u64) as usize;
    let k2 = pick_kind(rng);
    let fixed: BTreeMap<String, u32> =
        outer.decisions().into_iter().map(|(v, c)| (v.name, c)).collect();
    let inner = SeededDecider::new(rng.next_u64());
    let w2 = images[j].boot(
        k2,
        Box::new(Chain(Fixed::new(fixed.clone()), inner.clone())),
        sc.boot_cfg(),
    );
    repro.nested = Some((j as u64, k2));
    let reopened = reopen_and_check(&w2, sc, refm, false);
    let mut v = reopened.violations;
    let outer_img = Outer {
        img,
        kind,
        recovered: &recovered,
        decided: &decided,
        p0,
        p0_stop,
    };
    let verdict = r7a_judge(
        &outer_img,
        k2,
        &reopened.projected,
        reopened.outcome,
        (reopened.torn_stop, reopened.fail_stop),
        v.is_empty(),
        sc,
        &mut R7aMemo::new(),
    );
    let mut counts = R7aCounts::default();
    counts.add(verdict);
    v.extend(r7a_violation(verdict, j));
    repro.decisions = nonzero(&outer.decisions());
    repro.decisions.extend(nonzero(&inner.decisions()));
    Some((v, w2, counts))
}

/// A crash kind, either with p = 0.5.
fn pick_kind(rng: &mut Seeded) -> CrashKind {
    if rng.chance(500) {
        CrashKind::PowerLoss
    } else {
        CrashKind::ProcessKill
    }
}

/// A fault at a random mutating event of the forward trace, with an outcome its call allows
/// (§4.4).
fn pick_fault(trace: &Trace, rng: &mut Seeded) -> Option<Fault> {
    use crate::faulty::world::{CommitKind, FaultKind, FaultOutcome, StoreSel};
    let mut nth: BTreeMap<(String, OpKind), u32> = BTreeMap::new();
    let mut candidates = Vec::new();
    for (_, store, op, _) in trace.events() {
        let n = nth.entry((store.to_string(), op)).or_insert(0);
        let outcomes: &[FaultOutcome] = match op {
            OpKind::WriteAt => {
                &[FaultOutcome::NotApplied, FaultOutcome::Applied, FaultOutcome::Partial(1)]
            }
            OpKind::SyncData | OpKind::SyncDir => &[FaultOutcome::Applied],
            OpKind::Read | OpKind::Len | OpKind::LinkCount | OpKind::Allocate => &[],
            OpKind::Append(CommitKind::Buffered | CommitKind::Durable)
            | OpKind::Barrier
            | OpKind::SyncWal
            | OpKind::FlushMemtables
            | OpKind::Create
            | OpKind::Truncate
            | OpKind::Copy
            | OpKind::Rename
            | OpKind::Remove
            | OpKind::CreateDir => &[FaultOutcome::NotApplied, FaultOutcome::Applied],
        };
        for o in outcomes {
            candidates.push((store.to_string(), op, *n, *o));
        }
        *n += 1;
    }
    if candidates.is_empty() {
        return None;
    }
    let (store, op, n, outcome) =
        candidates.swap_remove(rng.below(candidates.len() as u64) as usize);
    Some(Fault {
        at: EventSel::Nth {
            store: if store == crate::faulty::world::FS_STORE {
                StoreSel::Fs
            } else {
                StoreSel::Engine(store)
            },
            op,
            n,
        },
        kind: if rng.chance(500) {
            FaultKind::Io
        } else {
            FaultKind::NoSpace
        },
        outcome,
    })
}

/// A crash point of the forward run: events inside flush and recovery windows weigh 10, others
/// 1 (§4.6).
fn pick_crash(trace: &Trace, rng: &mut Seeded) -> Option<u64> {
    let mut weighted: Vec<(u64, u64)> = Vec::new();
    let mut inside = false;
    for e in &trace.entries {
        match e {
            TraceEntry::Mark {
                epoch: 0,
                mark,
                ..
            } => match mark {
                Mark::FlushBegin {
                    ..
                }
                | Mark::RecoveryBegin => inside = true,
                Mark::ConnectBegin | Mark::RecoveryEnd => inside = false,
                _ => {}
            },
            TraceEntry::Event {
                id,
                ..
            } if id.epoch == 0 => weighted.push((
                id.index,
                if inside {
                    10
                } else {
                    1
                },
            )),
            _ => {}
        }
    }
    let total: u64 = weighted.iter().map(|(_, w)| w).sum();
    if total == 0 {
        return None;
    }
    let mut r = rng.below(total);
    for (i, w) in weighted {
        if r < w {
            return Some(i);
        }
        r -= w;
    }
    None
}

/// Explores a scenario (§4.5).
pub fn explore<S: Sut>(sc: &Scenario<S>, mode: Mode) -> Report {
    explore_in(sc, mode, None)
}

/// One shard of a sharded exhaustive exploration (DEC-185 (2)): the crash after forward event
/// `image`, booted as `kind`, and the outer branches whose first decisions are `prefix` (fewer
/// than the plan's depth when the branch decided fewer), with every nested branch under them. The
/// shards of [`shards`] partition the search: each branch's first decisions name its shard.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Shard {
    pub image: u64,
    pub kind: CrashKind,
    pub prefix: Vec<u32>,
}

impl Shard {
    /// `e{image}-{pk|pl}-{prefix}`, the prefix dot-separated (`x` when empty).
    pub fn id(&self) -> String {
        let kind = match self.kind {
            CrashKind::ProcessKill => "pk",
            CrashKind::PowerLoss => "pl",
        };
        let prefix: Vec<String> = self.prefix.iter().map(u32::to_string).collect();
        let prefix = if prefix.is_empty() {
            "x".to_string()
        } else {
            prefix.join(".")
        };
        format!("e{}-{kind}-{prefix}", self.image)
    }

    pub fn parse(id: &str) -> Option<Self> {
        let mut parts = id.split('-');
        let image = parts.next()?.strip_prefix('e')?.parse().ok()?;
        let kind = match parts.next()? {
            "pk" => CrashKind::ProcessKill,
            "pl" => CrashKind::PowerLoss,
            _ => return None,
        };
        let prefix = match parts.next()? {
            "x" => Vec::new(),
            p => p.split('.').map(str::parse).collect::<Result<_, _>>().ok()?,
        };
        parts.next().is_none().then_some(Self {
            image,
            kind,
            prefix,
        })
    }
}

/// The shards of `sc`'s exhaustive search at `depth` (DEC-185 (2)): for every image and kind,
/// one per distinct choice of the outer search's first `depth` decisions. Each is found by
/// running that branch's recovery and checks, as the search does, with later decisions at 0.
/// Fault passes are not sharded: `sc` must have none.
pub fn shards<S: Sut>(sc: &Scenario<S>, depth: usize) -> Vec<Shard> {
    assert!(
        sc.faults.0.is_empty() && sc.recovery_faults.is_empty(),
        "{}: fault passes are not sharded",
        sc.name
    );
    let refm = reference(sc);
    let fwd = run_forward(sc, None, &refm, Capture::Window);
    let mut out = Vec::new();
    for img in &fwd.images {
        for &kind in &sc.kinds {
            let dfs = Dfs::limited(depth);
            loop {
                let w = img.boot(kind, dfs.decider(), sc.boot_cfg());
                reopen_and_check(&w, sc, &refm, false);
                let taken = dfs.decisions();
                out.push(Shard {
                    image: img.at.index,
                    kind,
                    prefix: taken.iter().take(depth).map(|(_, c)| *c).collect(),
                });
                if !dfs.advance() {
                    break;
                }
            }
        }
    }
    out
}

/// One shard of `sc`'s exhaustive search, uncapped: its image, kind and outer branches, with
/// every nested branch under them (DEC-185 (2)). R12 runs where the unsharded search runs it:
/// on the image's all-survived branch, the first of its all-zero-prefix shard. A nested search
/// runs in the shard holding the first branch of its recovery, as unsharded (CP3.4-1).
pub fn explore_shard<S: Sut>(sc: &Scenario<S>, shard: &Shard) -> Report {
    let mode = Mode::Exhaustive {
        max_branches_per_image: u32::MAX,
    };
    explore_in(sc, mode, Some(shard))
}

fn explore_in<S: Sut>(sc: &Scenario<S>, mode: Mode, shard: Option<&Shard>) -> Report {
    assert!(
        shard.is_none() || (sc.faults.0.is_empty() && sc.recovery_faults.is_empty()),
        "{}: fault passes are not sharded",
        sc.name
    );
    let refm = reference(sc);
    let mut ex = Explorer {
        sc,
        mode: &mode,
        refm,
        report: Report::default(),
        shard,
    };
    let faults: Vec<Option<Fault>> = if sc.faults.0.is_empty() {
        vec![None]
    } else {
        sc.faults.0.iter().cloned().map(Some).collect()
    };
    let mut preconditions_failed = false;
    let mut clean_images = Vec::new();
    for fault in faults {
        let fwd = run_forward(sc, fault.as_ref(), &ex.refm, Capture::Window);
        if fault.is_some() {
            ex.report.work.fault_reruns += 1;
        }
        ex.report.work.add(&fwd.work);
        let base = Repro::new::<S>(sc, fault.clone());
        for v in fwd.violations {
            preconditions_failed |= v.invariant == InvariantId::Precondition;
            ex.report.add(v, &base);
        }
        if preconditions_failed {
            break;
        }
        if let Some(f) = &fault
            && !fwd.fault_fired
        {
            let v = Violation::new(
                InvariantId::Precondition,
                format!("the fault `{}` never fired", crate::faulty::repro::fault_text(f)),
            );
            ex.report.add(v, &base);
            continue;
        }
        if fwd.images.is_empty() {
            let v =
                Violation::new(InvariantId::Precondition, "the crash window captured no images");
            ex.report.add(v, &base);
            continue;
        }
        for img in &fwd.images {
            if ex.done() {
                break;
            }
            if shard.is_some_and(|s| s.image != img.at.index) {
                continue;
            }
            let mut r = base.clone();
            r.crash_at = Some(img.at.index);
            ex.image(img, &r);
        }
        if fault.is_none() {
            clean_images = fwd.images;
        }
    }
    // Faults after the crash: every image again, each with one recovery fault (§4.4).
    for f in &sc.recovery_faults {
        let mut fired = false;
        for img in &clean_images {
            let mut r = Repro::new::<S>(sc, None);
            r.crash_at = Some(img.at.index);
            fired |= ex.search(img, &r, Some(f));
        }
        if !fired {
            let v = Violation::new(
                InvariantId::Precondition,
                format!("the recovery fault `{}` never fired", crate::faulty::repro::fault_text(f)),
            );
            ex.report.add(v, &Repro::new::<S>(sc, None));
        }
    }
    if shard.is_some_and(|_| ex.report.images != 1) {
        let v = Violation::new(InvariantId::Precondition, "the shard's image is not in the window");
        ex.report.add(v, &Repro::new::<S>(sc, None));
    } else if shard.is_none()
        && matches!(mode, Mode::Exhaustive { .. })
        && ex.report.images < sc.min_images
    {
        let v = Violation::new(
            InvariantId::Precondition,
            format!(
                "explored {} images, the scenario expects at least {}",
                ex.report.images, sc.min_images
            ),
        );
        ex.report.add(v, &Repro::new::<S>(sc, None));
    }
    if let Mode::Witness {
        expect,
        ..
    } = &mode
    {
        let found = ex.report.found();
        ex.report.found_all = expect.iter().all(|e| found.contains(e));
    }
    ex.report
}

/// Replays a repro: the forward run (with its fault), the crash after its event, its decisions,
/// and its nested crash. Returns every violation.
/// [`replay`] of a single-crash repro that also returns the recovered world's trace (the forward
/// epoch, the crash and the recovery), for preconditions on what recovery did.
pub fn replay_trace<S: Sut>(r: &Repro) -> Result<(Vec<Violation>, Trace), String> {
    if r.nested.is_some() {
        return Err("a nested repro has no single recovered world".into());
    }
    let sc = r.scenario::<S>()?;
    let refm = reference(&sc);
    let at = r.crash_at.ok_or("the repro has no crash")?;
    let fwd = run_forward(&sc, r.fault.as_ref(), &refm, Capture::At(at));
    let img = fwd.images.iter().find(|i| i.at.index == at).ok_or("no image after the event")?;
    let cfg = faulty_boot_cfg(r.adversary, r.granularity, r.allocate, r.recovery_fault.as_ref());
    let w = img.boot(r.kind, Box::new(Fixed::new(r.decisions.clone())), cfg);
    let mut out = reopen_and_check(&w, &sc, &refm, false).violations;
    out.splice(0..0, fwd.violations);
    Ok((out, w.trace()))
}

pub fn replay<S: Sut>(r: &Repro) -> Result<Vec<Violation>, String> {
    let sc = r.scenario::<S>()?;
    let refm = reference(&sc);
    let capture = Capture::At(r.crash_at.unwrap_or(u64::MAX));
    let fwd = run_forward(&sc, r.fault.as_ref(), &refm, capture);
    let mut out = fwd.violations;
    let Some(at) = r.crash_at else {
        return Ok(out);
    };
    let Some(img) = fwd.images.iter().find(|i| i.at.index == at) else {
        return Err(format!("no image after event {at}"));
    };
    let decider = Fixed::new(r.decisions.clone());
    match r.nested {
        None => {
            let cfg =
                faulty_boot_cfg(r.adversary, r.granularity, r.allocate, r.recovery_fault.as_ref());
            let w = img.boot(r.kind, Box::new(decider), cfg);
            out.extend(
                reopen_and_check(&w, &sc, &refm, sc.continue_r12 && r.recovery_fault.is_none())
                    .violations,
            );
        }
        Some((j, k2)) => {
            let cfg = || boot_cfg(r.adversary, r.granularity, r.allocate);
            // The uninterrupted recovery and its projection (R7a).
            let (inner, (p0, recovered, decided, p0_stop)) =
                capture_recovery(img, r.kind, Box::new(decider.clone()), cfg(), |w| {
                    open_recorded::<S>(w, &sc.cfg)
                });
            let img2 = inner.get(j as usize).ok_or_else(|| format!("no inner image {j}"))?;
            let w2 = img2.boot(k2, Box::new(decider), cfg());
            if let Some((j2, k3)) = r.nested2 {
                let images = recovery_images::<S>(img2, k2, &r.decisions, cfg(), &sc.cfg);
                let img3 = images.get(j2 as usize).ok_or_else(|| format!("no image {j2}"))?;
                let w3 = img3.boot(k3, Box::new(Fixed::new(r.decisions.clone())), cfg());
                out.extend(reopen_and_check(&w3, &sc, &refm, false).violations);
                return Ok(out);
            }
            let reopened = reopen_and_check(&w2, &sc, &refm, false);
            let clean = reopened.violations.is_empty();
            out.extend(reopened.violations);
            // As in explore: the outer cells the recovery decided are fixed, the projection's own
            // are judged under each of their values.
            let outer_img = Outer {
                img,
                kind: r.kind,
                recovered: &recovered,
                decided: &decided,
                p0,
                p0_stop,
            };
            let verdict = r7a_judge(
                &outer_img,
                k2,
                &reopened.projected,
                reopened.outcome,
                (reopened.torn_stop, reopened.fail_stop),
                clean,
                &sc,
                &mut R7aMemo::new(),
            );
            out.extend(r7a_violation(verdict, j as usize));
        }
    }
    Ok(out)
}

/// Whether a replay of `r` reports every invariant `r.expect` lists.
pub fn reproduces<S: Sut>(r: &Repro) -> bool {
    match replay::<S>(r) {
        Ok(v) => {
            let found: BTreeSet<InvariantId> = v.iter().map(|v| v.invariant).collect();
            r.expect.iter().all(|e| found.contains(e))
        }
        Err(_) => false,
    }
}

/// The outcome of [`minimize`].
#[derive(Debug, Clone)]
pub struct Minimized {
    pub repro: Repro,
    pub replays: u32,
    /// A final pass found every remaining loss necessary (1-minimal decisions).
    pub minimality_verified: bool,
}

/// Minimises a failing repro (§4.7): the workload (ddmin), the earliest crash point, the
/// fault, decisions toward 0 to a fixed point, coarser granularity, nesting and adversary.
pub fn minimize<S: Sut>(failing: &Repro, budget: u32) -> Minimized {
    let mut replays = 0u32;
    let mut best = failing.clone();
    let fails = |r: &Repro, replays: &mut u32| -> bool {
        if *replays >= budget {
            return false;
        }
        *replays += 1;
        reproduces::<S>(r)
    };
    // 1. Workload: drop chunks of ops (ddmin).
    let mut chunk = best.workload.len().div_ceil(2).max(1);
    while chunk >= 1 && !best.workload.is_empty() {
        let mut i = 0;
        let mut changed = false;
        while i < best.workload.len() {
            let mut cand = best.clone();
            let end = (i + chunk).min(cand.workload.len());
            cand.workload.drain(i..end);
            if fails(&cand, &mut replays) {
                best = cand;
                changed = true;
            } else {
                i += chunk;
            }
        }
        if chunk == 1 && !changed {
            break;
        }
        if !changed {
            chunk /= 2;
        }
    }
    // 2. The earliest crash point that still fails.
    if let Some(at) = best.crash_at {
        for e in 0..at {
            let mut cand = best.clone();
            cand.crash_at = Some(e);
            if fails(&cand, &mut replays) {
                best = cand;
                break;
            }
        }
    }
    // 3. The fault.
    if best.fault.is_some() {
        let mut cand = best.clone();
        cand.fault = None;
        if fails(&cand, &mut replays) {
            best = cand;
        }
    }
    // 4. Decisions toward 0, to a fixed point.
    let (d, ok) = minimize_decisions(&best.decisions, |d| {
        let mut cand = best.clone();
        cand.decisions = d.clone();
        fails(&cand, &mut replays)
    });
    best.decisions = d;
    // 5. Coarser granularity.
    if best.granularity != Granularity::Write {
        let mut cand = best.clone();
        cand.granularity = Granularity::Write;
        if fails(&cand, &mut replays) {
            best = cand;
        }
    }
    // 6. Nesting (depth 2 first, then depth 1 with it), then the adversary.
    if best.nested2.is_some() {
        let mut cand = best.clone();
        cand.nested2 = None;
        if fails(&cand, &mut replays) {
            best = cand;
        }
    }
    if best.nested.is_some() {
        let mut cand = best.clone();
        cand.nested = None;
        cand.nested2 = None;
        if fails(&cand, &mut replays) {
            best = cand;
        }
    }
    if best.adversary != Adversary::Realistic {
        let mut cand = best.clone();
        cand.adversary = Adversary::Realistic;
        if fails(&cand, &mut replays) {
            best = cand;
        }
    }
    // Steps 5–6 can rename or retire variables: verify 1-minimality on the final repro.
    let _ = ok;
    let (d, ok) = minimize_decisions(&best.decisions, |d| {
        let mut cand = best.clone();
        cand.decisions = d.clone();
        fails(&cand, &mut replays)
    });
    best.decisions = d;
    best.minimality_verified = ok && replays < budget && reproduces::<S>(&best);
    Minimized {
        minimality_verified: best.minimality_verified,
        repro: best,
        replays,
    }
}

/// Moves decisions toward 0 while `fails` holds, repeating to a fixed point (§4.7; M13). The
/// result is 1-minimal (`true`) when a final pass that clears each remaining nonzero decision
/// alone finds every such attempt passing.
pub fn minimize_decisions(
    start: &BTreeMap<String, u32>,
    mut fails: impl FnMut(&BTreeMap<String, u32>) -> bool,
) -> (BTreeMap<String, u32>, bool) {
    let mut cur = start.clone();
    loop {
        let mut changed = false;
        let keys: Vec<String> = cur.keys().cloned().collect();
        for k in keys {
            let v = cur[&k];
            for target in 0..v {
                let mut cand = cur.clone();
                if target == 0 {
                    cand.remove(&k);
                } else {
                    cand.insert(k.clone(), target);
                }
                if fails(&cand) {
                    cur = cand;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Verification pass: clearing any remaining decision alone must make the failure go away.
    let verified = cur.keys().cloned().collect::<Vec<_>>().into_iter().all(|k| {
        let mut cand = cur.clone();
        cand.remove(&k);
        !fails(&cand)
    });
    (cur, verified)
}

#[cfg(test)]
mod tests {
    use super::Joint;

    /// Every completion of two cells, depth-first.
    #[test]
    fn joint_enumerates_every_completion() {
        let j = Joint::default();
        let mut seen = Vec::new();
        loop {
            j.begin();
            seen.push((j.choose("a", 2), j.choose("b", 3)));
            if !j.advance() {
                break;
            }
        }
        assert!(!j.poisoned());
        assert_eq!(seen, [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2)]);
    }

    /// References that decide out of order, under another domain, or stop short of a cell an
    /// earlier completion reached poison the enumeration (DEC-162: no verdict).
    #[test]
    fn joint_poisons_inconsistent_references() {
        let order = Joint::default();
        order.begin();
        order.choose("a", 2);
        order.choose("b", 2);
        order.advance();
        order.begin();
        order.choose("b", 2);
        assert!(order.poisoned());

        let domain = Joint::default();
        domain.begin();
        domain.choose("a", 2);
        domain.choose("a", 3);
        assert!(domain.poisoned());

        let short = Joint::default();
        short.begin();
        short.choose("a", 2);
        short.choose("b", 2);
        short.advance();
        short.begin();
        short.choose("a", 2);
        short.advance();
        assert!(short.poisoned());
    }
}
