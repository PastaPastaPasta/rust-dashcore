//! Violations, the standing cross-store dependency checker D1 (§5.3), and the durable view it
//! reads.
//!
//! D1 is evaluated when a world boots, after every completed barrier in any store, and once more
//! when recovery ends, against every durable record of every store; and at every engine append
//! of a finalizing record. It never decides a lazy cell, but it reads every cell the branch has
//! already decided. A record whose presence is still undecided protects its byte ranges (D1(d),
//! D1(e)) as if present, and a requirement it fails is held pending and reported as soon as the
//! branch decides the record present. A requirement whose own cells are undecided is re-checked
//! at the next evaluation; it is never certified.

use std::borrow::Cow;
use std::cell::Cell;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::engine::Cf;
use crate::faulty::world::{Hash, Stores, VarId};

/// The invariants of §5.1, D1's kinds (§5.3), the forward assertions (§6.2.1), model
/// self-checks (§5.4) and scenario preconditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InvariantId {
    R0,
    R1,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7a,
    R7b,
    R8,
    R9,
    R10,
    R11a,
    R11b,
    R11c,
    R12,
    R13,
    D1a,
    D1b,
    D1c,
    D1d,
    D1e,
    D1f,
    FA1,
    FA2,
    FA3,
    FA4,
    FA5,
    FA6,
    /// SB10's SPV store (`faulty::spv`): the header graph is closed and `active` is the chain to
    /// the tip.
    S1,
    /// A subscriber's locator is a stored header or the anchor, and its rows are its data along
    /// the chain to it.
    S2,
    /// Durability: each store recovered a state at or after the last acknowledged boundary.
    S3,
    /// No torn write set.
    S4,
    /// Liveness: after recovery, the continuation catches every subscriber up to the tip.
    S5,
    /// A model self-check (§5.4) failed: a bug in the harness, not the SUT.
    Model,
    /// A scenario precondition (§4.5) failed.
    Precondition,
}

impl InvariantId {
    pub const ALL: [InvariantId; 36] = [
        Self::R0,
        Self::R1,
        Self::R2,
        Self::R3,
        Self::R4,
        Self::R5,
        Self::R6,
        Self::R7a,
        Self::R7b,
        Self::R8,
        Self::R9,
        Self::R10,
        Self::R11a,
        Self::R11b,
        Self::R11c,
        Self::R12,
        Self::R13,
        Self::D1a,
        Self::D1b,
        Self::D1c,
        Self::D1d,
        Self::D1e,
        Self::D1f,
        Self::FA1,
        Self::FA2,
        Self::FA3,
        Self::FA4,
        Self::FA5,
        Self::FA6,
        Self::S1,
        Self::S2,
        Self::S3,
        Self::S4,
        Self::S5,
        Self::Model,
        Self::Precondition,
    ];

    pub fn name(self) -> String {
        format!("{self:?}")
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|i| i.name() == s)
    }

    /// D1's kinds, reported by the standing checker rather than after a reopen.
    pub fn is_d1(self) -> bool {
        matches!(self, Self::D1a | Self::D1b | Self::D1c | Self::D1d | Self::D1e | Self::D1f)
    }
}

/// One failed check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub invariant: InvariantId,
    pub detail: String,
    /// The epoch and event count when it was found, for D1 and forward assertions.
    pub at: Option<(u32, u64)>,
}

impl Violation {
    pub fn new(invariant: InvariantId, detail: impl Into<String>) -> Self {
        Self {
            invariant,
            detail: detail.into(),
            at: None,
        }
    }

    pub fn at(mut self, epoch: u32, events: u64) -> Self {
        self.at = Some((epoch, events));
        self
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.invariant, self.detail)?;
        if let Some((e, n)) = self.at {
            write!(f, " (epoch {e}, after {n} events)")?;
        }
        Ok(())
    }
}

/// A byte range of a flat file.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileRef {
    pub path: PathBuf,
    pub offset: u64,
    pub len: u64,
}

impl FileRef {
    pub fn new(path: impl Into<PathBuf>, offset: u64, len: u64) -> Self {
        Self {
            path: path.into(),
            offset,
            len,
        }
    }

    fn overlaps(&self, path: &Path, range: &Range<u64>) -> bool {
        self.path == path && self.offset < range.end && range.start < self.offset + self.len
    }
}

/// What a durable record needs (§5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Requirement {
    Bytes(FileRef),
    Block {
        hash: Hash,
        undo: bool,
    },
    /// The record (a subscriber locator) is not ahead of `store`'s durable tip.
    DurableTipAtLeast {
        store: &'static str,
        hash: Hash,
    },
}

/// A finalizer's dependency scope (§2.4.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Batches of the finalizer's store appended since the latest `FlushBegin` or
    /// `RecoveryBegin` mark.
    SinceFlushStart,
    /// Batches of `store` appended since its latest `Locator` mark.
    SincePreviousLocator {
        store: &'static str,
    },
    /// Batches of the finalizer's store that write a key of `cf` in `range`.
    Keys {
        cf: Cf,
        range: Range<Vec<u8>>,
    },
    /// Every pending batch of `store` (a cross-store scope).
    Store {
        store: &'static str,
    },
    /// The latest earlier finalizer of the same store.
    PreviousFinalizer,
}

/// A system under test's dependency declarations (§5.3).
pub trait RefExtractor: Send + Sync {
    /// The standing requirements of a durable record, evaluated against every store.
    fn refs(
        &self,
        db: &str,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        view: &WorldDurableView,
    ) -> Vec<Requirement>;

    /// The scopes a record finalizes, or `None`. `value` is `None` for a delete.
    fn finalizes(&self, db: &str, cf: Cf, key: &[u8], value: Option<&[u8]>) -> Option<Vec<Scope>> {
        let _ = (db, cf, key, value);
        None
    }

    /// Maps a block-level requirement to the byte ranges it needs; an error is D1(b).
    fn resolve(&self, req: &Requirement, view: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        let _ = view;
        match req {
            Requirement::Bytes(r) => Ok(vec![r.clone()]),
            other => Err(format!("no resolver for {other:?}")),
        }
    }
}

/// D1's running state in one epoch.
#[derive(Debug, Clone, Default)]
pub(crate) struct D1State {
    pub violations: Vec<Violation>,
    /// The byte ranges the durable records (present or possibly present) required at the latest
    /// evaluation; each conservative one names the undecided record it came from.
    required: Vec<(FileRef, Option<Uncertain>)>,
    /// Failed requirements of records whose presence was undecided: reported once the branch
    /// decides the record present with that value.
    pub pending: Vec<Pending>,
    /// Whether the latest evaluation protected ranges of records whose presence was undecided,
    /// and how many decisions the branch had made then: once it makes more, D1 re-evaluates
    /// before the next mutation, so the protection follows the decisions.
    pub conservative: bool,
    pub at_decisions: u64,
    /// Some record's extractor read an undecided record: after new decisions only a full
    /// evaluation can settle what it requires.
    pub via_reads: bool,
}

/// The record a conservatively protected range came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Uncertain {
    pub db: &'static str,
    pub cf: Cf,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    /// Its own presence is decided, but its extractor read an undecided record (Opus cp1 r2
    /// minor B): only a full evaluation can settle it.
    pub via_reads: bool,
}

/// A failed requirement of a record whose presence was undecided when it failed.
///
/// A nested crash starts a new `D1State`, and a newer Durable write to the key makes the record
/// known without the older bit; both drop the entry. Neither hides a violation: the mutation that
/// made the failure (an unlink, a truncate of a protected range) is D1(d)/D1(e) at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub db: &'static str,
    pub cf: Cf,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub violation: Violation,
    /// Pending because the extractor read an undecided record (not the record's own presence):
    /// the next evaluation re-derives it from the decided reads instead.
    pub via_reads: bool,
}

impl D1State {
    pub fn report(&mut self, v: Violation) {
        if !self.violations.iter().any(|o| o.invariant == v.invariant && o.detail == v.detail) {
            self.violations.push(v);
        }
    }

    pub fn set_required(&mut self, r: Vec<(FileRef, Option<Uncertain>)>, via_reads: bool) {
        self.via_reads = via_reads;
        self.conservative = via_reads || r.iter().any(|(_, u)| u.is_some());
        self.required = r;
    }

    /// D1(d): a mutation of `range` of `path` touches a durably referenced range.
    pub fn touched(&self, path: &Path, range: Range<u64>) -> Option<&FileRef> {
        self.required.iter().map(|(r, _)| r).find(|r| r.overlaps(path, &range))
    }

    /// Every referenced range of `path`.
    pub fn ranges_on(&self, path: &Path) -> Vec<FileRef> {
        self.required.iter().map(|(r, _)| r).filter(|r| r.path == path).cloned().collect()
    }

    /// D1(e): a durable record references `path`.
    pub fn referenced(&self, path: &Path) -> Option<&FileRef> {
        self.required.iter().map(|(r, _)| r).find(|r| r.path == path)
    }

    /// Re-checks only the conservatively protected ranges against the branch's new decisions
    /// (Opus cp1 r2 minor C): a record now decided present keeps its ranges as definite, one
    /// decided absent (or rewritten) drops them. Returns `true` if a full evaluation is needed
    /// (a range whose extractor read undecided records).
    pub fn refresh(&mut self, view: &WorldDurableView<'_>) -> bool {
        let mut full = self.via_reads;
        self.required.retain_mut(|(_, u)| {
            let Some(rec) = u else {
                return true;
            };
            if rec.via_reads {
                full = true;
                return true;
            }
            match view.get(rec.db, rec.cf, &rec.key) {
                Peek::Known(Some(v)) if v == rec.value => {
                    *u = None;
                    true
                }
                Peek::Known(_) => false,
                Peek::Unknown => true,
            }
        });
        self.conservative = self.via_reads || self.required.iter().any(|(_, u)| u.is_some());
        full
    }
}

/// Whether a byte range is durable (D1(a)), under a branch's decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Durability {
    Durable,
    NotDurable(String),
    /// A cell the answer depends on is undecided in this branch.
    Unknown(String),
}

/// A presence that may be undecided in this branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peek<T> {
    Known(T),
    Unknown,
}

/// A read-only view of every store's durable state under one branch's decisions: it reads every
/// decided cell and never decides one (§5.3).
pub struct WorldDurableView<'a> {
    stores: Cow<'a, Stores>,
    decided: Vec<Option<u32>>,
    /// A lookup through [`get`](Self::get) met an undecided cell since the last take.
    read_unknown: Cell<bool>,
}

impl<'a> WorldDurableView<'a> {
    pub(crate) fn new(stores: Cow<'a, Stores>, decided: Vec<Option<u32>>) -> Self {
        Self {
            stores,
            decided,
            read_unknown: Cell::new(false),
        }
    }

    /// Whether a lookup met an undecided cell since the last call (and resets it).
    pub(crate) fn take_read_unknown(&self) -> bool {
        self.read_unknown.replace(false)
    }

    fn dec(&self) -> impl Fn(VarId) -> Option<u32> + '_ {
        |v| self.decided.get(v as usize).copied().flatten()
    }

    pub fn engines(&self) -> Vec<&'static str> {
        self.stores.engines.keys().copied().collect()
    }

    /// The durable value of a key (`Unknown` while its presence is undecided). An extractor
    /// that sees `Unknown` may return what it can (or an error); D1 then treats the record it was
    /// evaluating as undecided too: its failures are held pending and its ranges protected.
    pub fn get(&self, db: &str, cf: Cf, key: &[u8]) -> Peek<Option<Vec<u8>>> {
        let r = match self.stores.engines.get(db) {
            Some(e) => e.durable_get(cf, key, &self.dec()),
            None => Peek::Known(None),
        };
        if r == Peek::Unknown {
            self.read_unknown.set(true);
        }
        r
    }

    /// Every durable record of `db` that is present or may be: `(cf, key, value, known)`.
    pub fn records(&self, db: &str) -> Vec<(Cf, Vec<u8>, Vec<u8>, bool)> {
        self.stores.engines.get(db).map(|e| e.durable_records(&self.dec())).unwrap_or_default()
    }

    /// D1(a): whether `[offset, offset + len)` of `path` is durable.
    pub fn range_durability(&self, r: &FileRef) -> Durability {
        self.stores.fs.range_durability(&r.path, r.offset, r.len, &self.dec())
    }

    /// `range_durability`, with `Unknown` reported as not (known to be) durable.
    pub fn range_durable(&self, r: &FileRef) -> Result<(), String> {
        match self.range_durability(r) {
            Durability::Durable => Ok(()),
            Durability::NotDurable(e) => Err(e),
            Durability::Unknown(e) => Err(format!("undecided: {e}")),
        }
    }

    /// Whether `path` is durably bound to a file, with nothing pending on its name.
    pub fn file_durable(&self, path: &Path) -> bool {
        self.stores.fs.durably_bound(path, &self.dec())
    }
}

pub(crate) struct Standing {
    /// Some record was undecided only through its extractor's reads.
    pub via_reads: bool,
    pub required: Vec<(FileRef, Option<Uncertain>)>,
    pub violations: Vec<Violation>,
    pub pending: Vec<Pending>,
    /// Some protected range came from a record whose presence is undecided.
    pub conservative: bool,
}

/// Evaluates the requirements of every durable record of every store (§5.3), resolving the
/// earlier `pending` failures against the decisions made since.
pub(crate) fn standing_check(
    x: &dyn RefExtractor,
    view: &WorldDurableView,
    pending: &[Pending],
) -> Standing {
    let mut out = Standing {
        via_reads: false,
        required: Vec::new(),
        violations: Vec::new(),
        pending: Vec::new(),
        conservative: false,
    };
    for p in pending.iter().filter(|p| !p.via_reads) {
        match view.get(p.db, p.cf, &p.key) {
            Peek::Known(Some(v)) if v == p.value => out.violations.push(p.violation.clone()),
            Peek::Known(_) => {}
            Peek::Unknown => out.pending.push(p.clone()),
        }
    }
    for db in view.engines() {
        for (cf, key, value, own_known) in view.records(db) {
            view.take_read_unknown();
            let mut fails = Vec::new();
            let mut ranges = Vec::new();
            for req in x.refs(db, cf, &key, &value, view) {
                match x.resolve(&req, view) {
                    Ok(rs) => ranges.extend(rs),
                    Err(e) => {
                        let kind = match req {
                            Requirement::Bytes(_) => InvariantId::D1c,
                            _ => InvariantId::D1b,
                        };
                        fails.push(Violation::new(kind, format!("{db}/{cf:?}/{}: {e}", hex(&key))));
                    }
                }
            }
            // Undecided if its own presence is, or its extractor read an undecided record.
            let via_reads = view.take_read_unknown();
            out.via_reads |= via_reads;
            let known = own_known && !via_reads;
            for r in &ranges {
                if let Durability::NotDurable(e) = view.range_durability(r) {
                    fails.push(Violation::new(
                        InvariantId::D1a,
                        format!(
                            "{db}/{cf:?}/{} needs {}[{}, {}): {e}",
                            hex(&key),
                            r.path.display(),
                            r.offset,
                            r.offset + r.len
                        ),
                    ));
                }
            }
            for v in fails {
                if known {
                    out.violations.push(v);
                } else if !out
                    .pending
                    .iter()
                    .any(|p| p.db == db && p.key == key && p.value == value && p.violation == v)
                {
                    out.pending.push(Pending {
                        db,
                        cf,
                        key: key.clone(),
                        value: value.clone(),
                        violation: v,
                        // A failure that came through an undecided read is re-derived when it
                        // is evaluated again, never emitted on the owner's presence alone (even
                        // if the owner was undecided too).
                        via_reads,
                    });
                }
            }
            // A possibly present record protects its ranges too (conservative).
            let tag = (!known).then(|| Uncertain {
                db,
                cf,
                key: key.clone(),
                value: value.clone(),
                via_reads,
            });
            out.conservative |= tag.is_some() && !ranges.is_empty();
            out.required.extend(ranges.into_iter().map(|r| (r, tag.clone())));
        }
    }
    out
}

/// `b` as lowercase hex, for violation messages.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
