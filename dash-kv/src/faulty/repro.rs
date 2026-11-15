//! Repro files (§4.8): a line format naming the system under test, its configuration and
//! workload, the fault, the crash point and kind, the decisions, the nested crash and the
//! expected invariants. Replayed by [`replay`](crate::faulty::replay).
//!
//! ```text
//! harness st3a/1
//! sut microlog
//! config <the SUT's config>
//! workload <op>;<op>;...
//! fault nth fs sync-data 1 io applied
//! crash 17 power-loss
//! decide 1:fs:i2:u3=1
//! nested 4 process-kill
//! nested2 2 power-loss
//! adversary realistic
//! granularity sector
//! allocate fail
//! r12 off
//! expect R1,D1a
//! minimality verified
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::faulty::check::InvariantId;
use crate::faulty::harness::{CrashWindow, FaultPlan, Nested, Scenario, Sut};
use crate::faulty::world::{
    Adversary, AllocateMode, CrashKind, EventSel, Fault, FaultKind, FaultOutcome, Granularity,
    MarkKind, OpKind, StoreSel, WitnessSet,
};

const HARNESS: &str = "st3a/1";

/// A replayable failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repro {
    pub sut: String,
    pub config: String,
    pub workload: Vec<String>,
    pub fault: Option<Fault>,
    /// A fault injected after the crash (into recovery).
    pub recovery_fault: Option<Fault>,
    /// The epoch-0 event after which the crash happens; `None` for forward-run violations.
    pub crash_at: Option<u64>,
    pub kind: CrashKind,
    pub decisions: BTreeMap<String, u32>,
    pub nested: Option<(u64, CrashKind)>,
    /// A second nested crash, during the recovery from the first (depth 2, §4.2).
    pub nested2: Option<(u64, CrashKind)>,
    pub adversary: Adversary,
    pub granularity: Granularity,
    /// Written only when it is not `Fallocate`.
    pub allocate: AllocateMode,
    /// R12's continuation; written only when a system that has one runs without it.
    pub r12: bool,
    pub expect: Vec<InvariantId>,
    pub minimality_verified: bool,
}

impl Repro {
    pub fn new<S: Sut>(sc: &Scenario<S>, fault: Option<Fault>) -> Self {
        Self {
            sut: S::NAME.into(),
            config: sc.cfg.to_string(),
            workload: sc.ops.iter().map(|o| o.to_string()).collect(),
            fault,
            recovery_fault: None,
            crash_at: None,
            kind: CrashKind::PowerLoss,
            decisions: BTreeMap::new(),
            nested: None,
            nested2: None,
            adversary: sc.adversary,
            granularity: sc.granularity,
            allocate: sc.allocate,
            r12: sc.continue_r12,
            expect: Vec::new(),
            minimality_verified: false,
        }
    }

    /// The scenario this repro runs (its window captures every event).
    pub fn scenario<S: Sut>(&self) -> Result<Scenario<S>, String> {
        if self.sut != S::NAME {
            return Err(format!("the repro is for `{}`, not `{}`", self.sut, S::NAME));
        }
        let cfg = self
            .config
            .parse::<S::Config>()
            .map_err(|_| format!("bad config `{}`", self.config))?;
        let ops = self
            .workload
            .iter()
            .map(|o| o.parse::<S::Op>().map_err(|_| format!("bad op `{o}`")))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Scenario {
            name: "repro",
            cfg,
            ops,
            window: CrashWindow::all(),
            kinds: vec![self.kind],
            faults: FaultPlan::default(),
            adversary: self.adversary,
            granularity: self.granularity,
            allocate: self.allocate,
            nested: Nested::None,
            r12_all: false,
            continue_r12: S::has_continuation() && self.r12,
            recovery_faults: Vec::new(),
            min_images: 0,
            r7a_assignments: crate::faulty::harness::R7A_ASSIGNMENTS,
            over_cap: None,
        })
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "harness {HARNESS}");
        let _ = writeln!(s, "sut {}", self.sut);
        let _ = writeln!(s, "config {}", self.config);
        let _ = writeln!(s, "workload {}", self.workload.join(";"));
        if let Some(f) = &self.fault {
            let _ = writeln!(s, "fault {}", fault_text(f));
        }
        if let Some(f) = &self.recovery_fault {
            let _ = writeln!(s, "recovery-fault {}", fault_text(f));
        }
        if let Some(at) = self.crash_at {
            let _ = writeln!(s, "crash {at} {}", kind_text(self.kind));
        }
        for (k, v) in &self.decisions {
            let _ = writeln!(s, "decide {k}={v}");
        }
        if let Some((j, k)) = self.nested {
            let _ = writeln!(s, "nested {j} {}", kind_text(k));
        }
        if let Some((j, k)) = self.nested2 {
            let _ = writeln!(s, "nested2 {j} {}", kind_text(k));
        }
        let _ = writeln!(s, "adversary {}", adversary_text(self.adversary));
        let _ = writeln!(s, "granularity {}", granularity_text(self.granularity));
        if self.allocate != AllocateMode::Fallocate {
            let _ = writeln!(s, "allocate {}", allocate_text(self.allocate));
        }
        if !self.r12 {
            let _ = writeln!(s, "r12 off");
        }
        let expect: Vec<String> = self.expect.iter().map(|e| e.name()).collect();
        let _ = writeln!(s, "expect {}", expect.join(","));
        let _ = writeln!(
            s,
            "minimality {}",
            if self.minimality_verified {
                "verified"
            } else {
                "best-effort"
            }
        );
        s
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut r = Self {
            sut: String::new(),
            config: String::new(),
            workload: Vec::new(),
            fault: None,
            recovery_fault: None,
            crash_at: None,
            kind: CrashKind::PowerLoss,
            decisions: BTreeMap::new(),
            nested: None,
            nested2: None,
            adversary: Adversary::Realistic,
            granularity: Granularity::Sector,
            allocate: AllocateMode::Fallocate,
            r12: true,
            expect: Vec::new(),
            minimality_verified: false,
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
            let bad = || format!("bad line `{line}`");
            match key {
                "harness" if rest == HARNESS => {}
                "harness" => return Err(format!("unknown harness version `{rest}`")),
                "sut" => r.sut = rest.into(),
                "config" => r.config = rest.into(),
                "workload" => {
                    r.workload = if rest.is_empty() {
                        Vec::new()
                    } else {
                        rest.split(';').map(String::from).collect()
                    }
                }
                "fault" => r.fault = Some(parse_fault(rest).ok_or_else(bad)?),
                "recovery-fault" => r.recovery_fault = Some(parse_fault(rest).ok_or_else(bad)?),
                "crash" => {
                    let (at, kind) = rest.split_once(' ').ok_or_else(bad)?;
                    r.crash_at = Some(at.parse().map_err(|_| bad())?);
                    r.kind = parse_kind(kind).ok_or_else(bad)?;
                }
                "decide" => {
                    let (k, v) = rest.rsplit_once('=').ok_or_else(bad)?;
                    r.decisions.insert(k.into(), v.parse().map_err(|_| bad())?);
                }
                "nested2" => {
                    let (j, kind) = rest.split_once(' ').ok_or_else(bad)?;
                    r.nested2 =
                        Some((j.parse().map_err(|_| bad())?, parse_kind(kind).ok_or_else(bad)?));
                }
                "nested" => {
                    let (j, kind) = rest.split_once(' ').ok_or_else(bad)?;
                    r.nested =
                        Some((j.parse().map_err(|_| bad())?, parse_kind(kind).ok_or_else(bad)?));
                }
                "adversary" => r.adversary = parse_adversary(rest).ok_or_else(bad)?,
                "granularity" => r.granularity = parse_granularity(rest).ok_or_else(bad)?,
                "allocate" => r.allocate = parse_allocate(rest).ok_or_else(bad)?,
                "r12" if rest == "off" => r.r12 = false,
                "expect" => {
                    r.expect = rest
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| InvariantId::parse(s).ok_or_else(bad))
                        .collect::<Result<_, _>>()?
                }
                "minimality" => r.minimality_verified = rest == "verified",
                _ => return Err(bad()),
            }
        }
        if r.sut.is_empty() {
            return Err("no `sut` line".into());
        }
        Ok(r)
    }
}

fn allocate_text(a: AllocateMode) -> &'static str {
    match a {
        AllocateMode::Fallocate => "fallocate",
        AllocateMode::ZeroFill => "zero-fill",
        AllocateMode::Fail => "fail",
    }
}

fn parse_allocate(s: &str) -> Option<AllocateMode> {
    [AllocateMode::Fallocate, AllocateMode::ZeroFill, AllocateMode::Fail]
        .into_iter()
        .find(|a| allocate_text(*a) == s)
}

fn kind_text(k: CrashKind) -> &'static str {
    match k {
        CrashKind::ProcessKill => "process-kill",
        CrashKind::PowerLoss => "power-loss",
    }
}

fn parse_kind(s: &str) -> Option<CrashKind> {
    match s {
        "process-kill" => Some(CrashKind::ProcessKill),
        "power-loss" => Some(CrashKind::PowerLoss),
        _ => None,
    }
}

fn adversary_text(a: Adversary) -> String {
    match a {
        Adversary::Realistic => "realistic".into(),
        Adversary::Extended => "extended".into(),
        Adversary::Witness(w) => {
            let mut parts = Vec::new();
            for (on, name) in [
                (w.grant_prefix, "prefix"),
                (w.break_atomic, "atomic"),
                (w.break_rename, "rename"),
                (w.entry_on_file_sync, "entry"),
                (w.break_psow, "psow"),
            ] {
                if on {
                    parts.push(name);
                }
            }
            format!("witness:{}", parts.join(","))
        }
    }
}

fn parse_adversary(s: &str) -> Option<Adversary> {
    match s {
        "realistic" => Some(Adversary::Realistic),
        "extended" => Some(Adversary::Extended),
        _ => {
            let parts = s.strip_prefix("witness:")?;
            let mut w = WitnessSet::default();
            for p in parts.split(',').filter(|p| !p.is_empty()) {
                match p {
                    "prefix" => w.grant_prefix = true,
                    "atomic" => w.break_atomic = true,
                    "rename" => w.break_rename = true,
                    "entry" => w.entry_on_file_sync = true,
                    "psow" => w.break_psow = true,
                    _ => return None,
                }
            }
            Some(Adversary::Witness(w))
        }
    }
}

fn granularity_text(g: Granularity) -> &'static str {
    match g {
        Granularity::Write => "write",
        Granularity::Page => "page",
        Granularity::Sector => "sector",
        Granularity::Byte => "byte",
    }
}

fn parse_granularity(s: &str) -> Option<Granularity> {
    match s {
        "write" => Some(Granularity::Write),
        "page" => Some(Granularity::Page),
        "sector" => Some(Granularity::Sector),
        "byte" => Some(Granularity::Byte),
        _ => None,
    }
}

fn store_text(s: &StoreSel) -> String {
    match s {
        StoreSel::Any => "any".into(),
        StoreSel::Fs => "fs".into(),
        StoreSel::Engine(n) => format!("db:{n}"),
    }
}

fn parse_store(s: &str) -> Option<StoreSel> {
    match s {
        "any" => Some(StoreSel::Any),
        "fs" => Some(StoreSel::Fs),
        _ => Some(StoreSel::Engine(s.strip_prefix("db:")?.into())),
    }
}

pub(crate) fn sel_text(s: &EventSel) -> String {
    match s {
        EventSel::Index(i) => format!("index {i}"),
        EventSel::Nth {
            store,
            op,
            n,
        } => format!("nth {} {} {n}", store_text(store), op.name()),
        EventSel::AfterMark {
            mark,
            occurrence,
            n,
        } => format!("after {} {occurrence} {n}", mark.name()),
    }
}

fn parse_sel(words: &mut std::str::SplitWhitespace<'_>) -> Option<EventSel> {
    Some(match words.next()? {
        "index" => EventSel::Index(words.next()?.parse().ok()?),
        "nth" => EventSel::Nth {
            store: parse_store(words.next()?)?,
            op: OpKind::parse(words.next()?)?,
            n: words.next()?.parse().ok()?,
        },
        "after" => EventSel::AfterMark {
            mark: MarkKind::parse(words.next()?)?,
            occurrence: words.next()?.parse().ok()?,
            n: words.next()?.parse().ok()?,
        },
        _ => return None,
    })
}

pub(crate) fn fault_text(f: &Fault) -> String {
    let kind = match f.kind {
        FaultKind::NoSpace => "nospace",
        FaultKind::Io => "io",
    };
    let outcome = match f.outcome {
        FaultOutcome::NotApplied => "not-applied".to_string(),
        FaultOutcome::Applied => "applied".to_string(),
        FaultOutcome::Partial(n) => format!("partial:{n}"),
    };
    format!("{} {kind} {outcome}", sel_text(&f.at))
}

fn parse_fault(s: &str) -> Option<Fault> {
    let mut w = s.split_whitespace();
    let at = parse_sel(&mut w)?;
    let kind = match w.next()? {
        "nospace" => FaultKind::NoSpace,
        "io" => FaultKind::Io,
        _ => return None,
    };
    let outcome = match w.next()? {
        "not-applied" => FaultOutcome::NotApplied,
        "applied" => FaultOutcome::Applied,
        o => FaultOutcome::Partial(o.strip_prefix("partial:")?.parse().ok()?),
    };
    Some(Fault {
        at,
        kind,
        outcome,
    })
}
