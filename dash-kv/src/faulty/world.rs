//! The world: every faulty store of one system under test, the event counter, marks, the trace,
//! lazy cells and their decisions, and crash images (design §3.2, §4.1–§4.3).
//!
//! All stores of a world share one lock, so an event, the capture it triggers and the decisions it
//! forces are totally ordered. A store handle ([`FaultyEngine`], [`FaultyFlatFiles`]) is a name
//! plus the world.

use std::collections::BTreeMap;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::engine::Commit;
use crate::faulty::check::{D1State, RefExtractor, Violation};
use crate::faulty::engine::{EngineState, FaultyEngine, Recovery};
use crate::faulty::fs::{FaultyFlatFiles, FsState};
use crate::schema::DbSchema;

/// The index of a lazy cell's variable in its world's variable table.
pub type VarId = u32;

/// What a lazy cell decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VarKind {
    /// Whether one pending engine batch survived a power loss (0 = survived).
    BatchSurvives,
    /// How many of an engine's pending batches were lost, newest first (W1 grants A-PREFIX).
    BatchPrefix,
    /// Whether one pending directory operation survived (0 = survived).
    DirOpSurvives,
    /// A file's size after a power loss (0 = the newest size).
    FileSize,
    /// A data unit's version after a power loss: how many of its newest pending versions were
    /// lost (0 = none).
    Sector,
    /// How many pages of a `pwrite` interrupted by a process kill were lost (0 = none).
    InsideWrite,
}

/// A lazy cell's variable: a named choice in `0..domain`. Choice 0 is always "the newest state
/// survived".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Var {
    pub kind: VarKind,
    pub name: String,
    pub domain: u32,
}

/// Chooses the value of each lazy cell when it is first observed (rule L).
pub trait Decider: Send {
    fn choose(&mut self, var: &Var) -> u32;
}

/// Decides every cell 0: everything pending survived.
pub struct AllSurvive;

impl Decider for AllSurvive {
    fn choose(&mut self, _: &Var) -> u32 {
        0
    }
}

/// The two crash kinds (§2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrashKind {
    /// Every completed call is kept, and stays pending and volatile.
    ProcessKill,
    /// The durable image plus an adversarial, lazily resolved selection of the pending state.
    PowerLoss,
}

/// A mutating store call (an event), or a read (an observation, never a crash point).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpKind {
    Append(CommitKind),
    Barrier,
    SyncWal,
    FlushMemtables,
    Create,
    WriteAt,
    Allocate,
    Truncate,
    SyncData,
    SyncDir,
    Copy,
    Rename,
    Remove,
    CreateDir,
    Read,
    Len,
    LinkCount,
}

/// [`Commit`] as an orderable, hashable value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitKind {
    Buffered,
    Durable,
}

impl From<Commit> for CommitKind {
    fn from(c: Commit) -> Self {
        match c {
            Commit::Buffered => Self::Buffered,
            Commit::Durable => Self::Durable,
        }
    }
}

impl OpKind {
    pub fn is_event(self) -> bool {
        !matches!(self, Self::Read | Self::Len | Self::LinkCount)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Append(CommitKind::Buffered) => "append-buffered",
            Self::Append(CommitKind::Durable) => "append-durable",
            Self::Barrier => "barrier",
            Self::SyncWal => "sync-wal",
            Self::FlushMemtables => "flush-memtables",
            Self::Create => "create",
            Self::WriteAt => "write",
            Self::Allocate => "allocate",
            Self::Truncate => "truncate",
            Self::SyncData => "sync-data",
            Self::SyncDir => "sync-dir",
            Self::Copy => "copy",
            Self::Rename => "rename",
            Self::Remove => "remove",
            Self::CreateDir => "create-dir",
            Self::Read => "read",
            Self::Len => "len",
            Self::LinkCount => "link-count",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL_OPS.iter().copied().find(|o| o.name() == s)
    }
}

const ALL_OPS: [OpKind; 18] = [
    OpKind::Append(CommitKind::Buffered),
    OpKind::Append(CommitKind::Durable),
    OpKind::Barrier,
    OpKind::SyncWal,
    OpKind::FlushMemtables,
    OpKind::Create,
    OpKind::WriteAt,
    OpKind::Allocate,
    OpKind::Truncate,
    OpKind::SyncData,
    OpKind::SyncDir,
    OpKind::Copy,
    OpKind::Rename,
    OpKind::Remove,
    OpKind::CreateDir,
    OpKind::Read,
    OpKind::Len,
    OpKind::LinkCount,
];

/// Where a block's prune decision came from (§3.2 `PruneDecision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PruneDue {
    Automatic,
    Manual(u32),
}

/// A cause of a full flush (§6.2 step 0; Core's `fDoFullFlush`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FlushCause {
    Prune,
    Cache,
    Interval,
    Scripted,
    DMax,
}

/// What a subscriber's locator commit was for (§5.6 item 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LocatorKind {
    CommitStep,
    Checkpoint,
    Cadence,
    Rewind,
}

/// Whether a connect ran full script checks (assumevalid).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScriptCheck {
    Full,
    Skipped,
}

/// A 32-byte block or state hash.
pub type Hash = [u8; 32];

/// Provenance a system under test reports (§3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark {
    FlushBegin {
        flush: u64,
        old: Option<Hash>,
        new: Hash,
    },
    /// `ChainStateFlushed`: after B1 returned (DEC-64).
    Durable {
        flush: u64,
    },
    Locator {
        store: &'static str,
        tip: Hash,
        kind: LocatorKind,
    },
    PruneSelected {
        flush: u64,
        files: Vec<u32>,
        bound: u32,
        rearmed: bool,
        /// After the selection, an automatic target is still unmet (FA2's re-arm rule; an
        /// extension of §3.2's mark).
        target_unmet: bool,
        /// The actual lock bound after the commit step (`None`: no lock; an extension).
        lock: Option<u32>,
    },
    /// V declined the flush's selection, its tip behind it (DEC-211): the flush prunes nothing.
    /// No checker reads it; scenarios assert it (an extension of §3.2's marks).
    PruneDeclined {
        flush: u64,
        files: Vec<u32>,
    },
    PruneDecision {
        due: PruneDue,
        dry_run: Vec<u32>,
        met_target: bool,
        predicted_lock: Option<u32>,
        flush_for_prune: bool,
        full_flush: Vec<FlushCause>,
        /// The decision's tip, and the dry run with no subscriber lock (FA1's and FA5's
        /// reference inputs; extensions of §3.2's mark).
        new: Hash,
        unlocked: Vec<u32>,
    },
    SubscriberState {
        store: &'static str,
        /// Core's `m_synced`: the subscriber's class, never an instantaneous test.
        synced: bool,
        durable: Option<Hash>,
        progress: Option<Hash>,
        /// The submitted-data tip s (GPT r5; an extension of §3.2's mark).
        submitted: Option<Hash>,
        blocked: bool,
    },
    Log {
        line: String,
    },
    Connected {
        block: Hash,
        scripts: ScriptCheck,
        applied: bool,
    },
    BestEffortBegin,
    BestEffortEnd,
    ConnectBegin,
    ConnectEnd,
    RecoveryBegin,
    RecoveryEnd,
    /// A Core call path (`LoadChainstate`, `ReplayBlocks`, `ConnectTip`, a disconnect, ...),
    /// by a system's own label: the abort message Core gives after a fatal error depends on it
    /// (DEC-162 (4), DEC-165 (3)). A failed read inside one is fatal, as inside recovery.
    PathBegin {
        path: &'static str,
    },
    PathEnd,
    StoreFailed {
        op: &'static str,
    },
    Abort {
        message: String,
    },
}

/// The variant of a [`Mark`], for selecting events after one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MarkKind {
    FlushBegin,
    Durable,
    Locator,
    PruneSelected,
    PruneDeclined,
    PruneDecision,
    SubscriberState,
    Log,
    Connected,
    BestEffortBegin,
    BestEffortEnd,
    ConnectBegin,
    ConnectEnd,
    RecoveryBegin,
    RecoveryEnd,
    PathBegin,
    PathEnd,
    StoreFailed,
    Abort,
}

const ALL_MARKS: [MarkKind; 19] = [
    MarkKind::FlushBegin,
    MarkKind::Durable,
    MarkKind::Locator,
    MarkKind::PruneSelected,
    MarkKind::PruneDeclined,
    MarkKind::PruneDecision,
    MarkKind::SubscriberState,
    MarkKind::Log,
    MarkKind::Connected,
    MarkKind::BestEffortBegin,
    MarkKind::BestEffortEnd,
    MarkKind::ConnectBegin,
    MarkKind::ConnectEnd,
    MarkKind::RecoveryBegin,
    MarkKind::RecoveryEnd,
    MarkKind::PathBegin,
    MarkKind::PathEnd,
    MarkKind::StoreFailed,
    MarkKind::Abort,
];

impl MarkKind {
    pub fn name(self) -> String {
        format!("{self:?}")
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL_MARKS.iter().copied().find(|m| m.name() == s)
    }
}

impl Mark {
    pub fn kind(&self) -> MarkKind {
        match self {
            Self::FlushBegin {
                ..
            } => MarkKind::FlushBegin,
            Self::Durable {
                ..
            } => MarkKind::Durable,
            Self::Locator {
                ..
            } => MarkKind::Locator,
            Self::PruneSelected {
                ..
            } => MarkKind::PruneSelected,
            Self::PruneDeclined {
                ..
            } => MarkKind::PruneDeclined,
            Self::PruneDecision {
                ..
            } => MarkKind::PruneDecision,
            Self::SubscriberState {
                ..
            } => MarkKind::SubscriberState,
            Self::Log {
                ..
            } => MarkKind::Log,
            Self::Connected {
                ..
            } => MarkKind::Connected,
            Self::BestEffortBegin => MarkKind::BestEffortBegin,
            Self::BestEffortEnd => MarkKind::BestEffortEnd,
            Self::ConnectBegin => MarkKind::ConnectBegin,
            Self::ConnectEnd => MarkKind::ConnectEnd,
            Self::RecoveryBegin => MarkKind::RecoveryBegin,
            Self::RecoveryEnd => MarkKind::RecoveryEnd,
            Self::PathBegin {
                ..
            } => MarkKind::PathBegin,
            Self::PathEnd => MarkKind::PathEnd,
            Self::StoreFailed {
                ..
            } => MarkKind::StoreFailed,
            Self::Abort {
                ..
            } => MarkKind::Abort,
        }
    }
}

/// Which store an event selector matches.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StoreSel {
    Any,
    Fs,
    Engine(String),
}

impl StoreSel {
    fn matches(&self, store: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Fs => store == FS_STORE,
            Self::Engine(name) => store == name,
        }
    }
}

/// The store name events of [`FaultyFlatFiles`] carry.
pub const FS_STORE: &str = "fs";

/// Selects one event of an epoch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventSel {
    /// The event with this index in its epoch (0-based).
    Index(u64),
    /// The `n`-th (0-based) event of this kind in this store.
    Nth {
        store: StoreSel,
        op: OpKind,
        n: u32,
    },
    /// The `n`-th (0-based) event after the `occurrence`-th (0-based) mark of this kind.
    AfterMark {
        mark: MarkKind,
        occurrence: u32,
        n: u32,
    },
}

/// What is known about an event when selectors are matched against it.
#[derive(Debug, Clone)]
pub struct EventInfo {
    pub index: u64,
    pub store: String,
    pub op: OpKind,
    /// How many earlier events of this store and kind the epoch had.
    pub nth: u32,
    /// For each mark kind seen in this epoch: (occurrences so far, events since the latest).
    pub after: BTreeMap<MarkKind, (u32, u32)>,
}

impl EventSel {
    pub fn matches(&self, ev: &EventInfo) -> bool {
        match self {
            Self::Index(i) => ev.index == *i,
            Self::Nth {
                store,
                op,
                n,
            } => store.matches(&ev.store) && *op == ev.op && ev.nth == *n,
            Self::AfterMark {
                mark,
                occurrence,
                n,
            } => ev
                .after
                .get(mark)
                .is_some_and(|&(seen, since)| seen == occurrence + 1 && since == *n),
        }
    }
}

/// An injected failure (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultKind {
    NoSpace,
    Io,
}

/// What a failing call did before it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultOutcome {
    NotApplied,
    Applied,
    /// The first `n` granules of a write (or a prefix of a copy) were applied.
    Partial(u32),
}

/// One fault: the selected event fails.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fault {
    pub at: EventSel,
    pub kind: FaultKind,
    pub outcome: FaultOutcome,
}

/// The unit of torn data (§2.2): the size of an independently surviving data cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Granularity {
    /// Each file's pending writes survive as a temporal prefix of the whole file.
    Write,
    /// 4 KiB pages.
    Page,
    /// 512-byte sectors (`Realistic`).
    Sector,
    /// Single bytes (`Extended`).
    Byte,
}

impl Granularity {
    /// The unit size in bytes; `Write` keeps sector units with one var per file.
    pub fn unit(self) -> u64 {
        match self {
            Self::Page => 4096,
            Self::Write | Self::Sector => 512,
            Self::Byte => 1,
        }
    }
}

/// One assumption broken or one bonus granted at a time (§6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct WitnessSet {
    /// W1: engine batches survive as a prefix (A-PREFIX, withdrawn by DEC-62).
    pub grant_prefix: bool,
    /// W2: each column family keeps its own subset of a batch (breaks A-ATOMIC).
    pub break_atomic: bool,
    /// W3: a rename's two names survive independently (breaks A-REN).
    pub break_rename: bool,
    /// W4: `sync_data` of a new file also persists its directory entry (ext4's bonus).
    pub entry_on_file_sync: bool,
    /// W5: a power loss may zero the unit shared by synced data and a pending write after it
    /// (breaks A-PSOW).
    pub break_psow: bool,
}

/// How adversarial a crash is (§2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Adversary {
    Realistic,
    /// `Realistic` plus a garbage version of every pending unit and any unit-aligned size in
    /// range.
    Extended,
    Witness(WitnessSet),
}

/// How `allocate` behaves (§2.2): `posix_fallocate`, or ST2 `OsFs`'s zero-writing fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AllocateMode {
    Fallocate,
    ZeroFill,
    /// The allocation silently does nothing (N7: allocate is advisory).
    Fail,
}

/// Callback after every event (used by capture mode).
pub type EventHook = Box<dyn FnMut(&World, EventId) + Send>;

/// The configuration of one epoch of a world.
pub struct WorldConfig {
    pub faults: Vec<Fault>,
    /// Bytes the filesystem can hold (shared with the engines if `shared_capacity`).
    pub capacity: Option<u64>,
    pub shared_capacity: bool,
    pub granularity: Granularity,
    pub adversary: Adversary,
    pub allocate: AllocateMode,
    pub on_event: Option<EventHook>,
    /// Run the standing D1 checker after every completed barrier.
    pub d1: bool,
    /// Record every observation (for M10).
    pub record_observations: bool,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            faults: Vec::new(),
            capacity: None,
            shared_capacity: false,
            granularity: Granularity::Sector,
            adversary: Adversary::Realistic,
            allocate: AllocateMode::Fallocate,
            on_event: None,
            d1: true,
            record_observations: false,
        }
    }
}

impl fmt::Debug for WorldConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldConfig")
            .field("faults", &self.faults)
            .field("capacity", &self.capacity)
            .field("granularity", &self.granularity)
            .field("adversary", &self.adversary)
            .field("allocate", &self.allocate)
            .finish_non_exhaustive()
    }
}

/// The settings of a [`WorldConfig`] the stores read (everything but the hook).
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub faults: Vec<Fault>,
    pub capacity: Option<u64>,
    pub shared_capacity: bool,
    pub granularity: Granularity,
    pub adversary: Adversary,
    pub allocate: AllocateMode,
    pub d1: bool,
    pub record_observations: bool,
}

impl Settings {
    pub fn to_config(&self) -> WorldConfig {
        WorldConfig {
            faults: Vec::new(),
            capacity: self.capacity,
            shared_capacity: self.shared_capacity,
            granularity: self.granularity,
            adversary: self.adversary,
            allocate: self.allocate,
            on_event: None,
            d1: self.d1,
            record_observations: self.record_observations,
        }
    }

    pub fn witness(&self) -> WitnessSet {
        match self.adversary {
            Adversary::Witness(w) => w,
            _ => WitnessSet::default(),
        }
    }
}

/// An event's identity: its epoch (0 for the forward run, +1 per crash) and index in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EventId {
    pub epoch: u32,
    pub index: u64,
}

/// One entry of a world's trace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceEntry {
    Event {
        id: EventId,
        store: String,
        op: OpKind,
        detail: String,
        /// The call failed (an injected fault or a refusal).
        failed: bool,
    },
    Mark {
        epoch: u32,
        /// The number of events of the epoch before the mark.
        after_events: u64,
        mark: Mark,
    },
    Crash {
        epoch: u32,
        kind: CrashKind,
    },
    /// An injected read failure (§4.4): not an event, fatal inside recovery and connects.
    ReadFailed {
        epoch: u32,
        store: String,
        op: OpKind,
        detail: String,
    },
}

/// Everything that happened, in order, across epochs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trace {
    pub entries: Vec<TraceEntry>,
}

impl Trace {
    pub fn marks(&self) -> impl Iterator<Item = &Mark> {
        self.entries.iter().filter_map(|e| match e {
            TraceEntry::Mark {
                mark,
                ..
            } => Some(mark),
            _ => None,
        })
    }

    /// Marks with their epoch.
    pub fn marks_with_epoch(&self) -> impl Iterator<Item = (u32, &Mark)> {
        self.entries.iter().filter_map(|e| match e {
            TraceEntry::Mark {
                mark,
                epoch,
                ..
            } => Some((*epoch, mark)),
            _ => None,
        })
    }

    pub fn events(&self) -> impl Iterator<Item = (EventId, &str, OpKind, bool)> {
        self.entries.iter().filter_map(|e| match e {
            TraceEntry::Event {
                id,
                store,
                op,
                failed,
                ..
            } => Some((*id, store.as_str(), *op, *failed)),
            _ => None,
        })
    }

    /// A stable hash of the whole trace (M11).
    pub fn hash(&self) -> u64 {
        let mut h = Fnv::default();
        for e in &self.entries {
            h.write(format!("{e:?}").as_bytes());
        }
        h.0
    }
}

/// FNV-1a, 64-bit: a stable hash for traces and observations.
#[derive(Clone, Copy)]
pub(crate) struct Fnv(pub u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// Deterministic work counters (§8.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Work {
    pub events: u64,
    pub fault_reruns: u64,
    pub images: u64,
    pub branches: u64,
    pub recoveries: u64,
    pub bytes_read: u64,
    pub bytes_hashed: u64,
    pub history_scanned: u64,
    pub cloned_units: u64,
    pub scanned_entries: u64,
    pub decisions: u64,
}

impl Work {
    pub fn add(&mut self, o: &Work) {
        self.events += o.events;
        self.fault_reruns += o.fault_reruns;
        self.images += o.images;
        self.branches += o.branches;
        self.recoveries += o.recoveries;
        self.bytes_read += o.bytes_read;
        self.bytes_hashed += o.bytes_hashed;
        self.history_scanned += o.history_scanned;
        self.cloned_units += o.cloned_units;
        self.scanned_entries += o.scanned_entries;
        self.decisions += o.decisions;
    }

    /// One scalar for caps: a weighted sum calibrated so one unit is about a microsecond of debug
    /// work.
    pub fn units(&self) -> u64 {
        self.events * 4
            + self.branches * 50
            + self.bytes_read / 64
            + self.bytes_hashed / 64
            + self.history_scanned
            + self.cloned_units
            + self.scanned_entries
            + self.decisions
    }
}

/// A variable and its decision in the current branch.
#[derive(Debug, Clone)]
pub(crate) struct VarSlot {
    pub var: Var,
    pub decided: Option<u32>,
    /// For a `^` cell: what each choice leaves of the input it inherited (R7a, CP3-3).
    pub inherited: Option<Arc<[Start]>>,
}

/// What a `^` cell's choice leaves of the input it inherited: state already pending before the
/// crashed epoch began (a process kill kept it, and that epoch's recovery did not cover it).
/// R7a's references apply a nested branch's inherited inputs by these, choosing the cells of
/// their own crashes that leave each input alike (DEC-162).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Start {
    /// The input as the outer image held it (perhaps overwritten since by the recovery).
    Intact,
    /// The cell's choice `k` at a crash before the recovery's first event (batch and directory
    /// survival, a block's readability, and temporal prefixes, which count the recovery's own
    /// operations past it).
    Choice(u32),
    /// An older or torn version of the input, by content (its fingerprint).
    Value(u64),
}

/// A decided `^` cell of a branch (see [`Start`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritedDecision {
    pub name: String,
    pub start: Start,
}

/// One observation (M10): what a read returned.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Observation {
    pub store: String,
    pub what: String,
    pub result: u64,
}

/// A block an operation failed on because it reads back as EIO (DEC-161).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unavailable {
    /// The boot's power loss tore it: the block held bytes of a write unsynced at that crash.
    pub torn: bool,
}

/// The non-store state of a world.
pub(crate) struct Ctx {
    pub cfg: Settings,
    pub epoch: u32,
    pub events: u64,
    pub vars: Vec<VarSlot>,
    pub decider: Option<Box<dyn Decider>>,
    pub trace: Trace,
    pub work: Work,
    pub frozen: bool,
    nth: BTreeMap<(String, OpKind), u32>,
    after: BTreeMap<MarkKind, (u32, u32)>,
    pub observations: Vec<Observation>,
    /// The fault that hit the current event, if any.
    pub fault: Option<Fault>,
    /// What selectors know about the latest event (for crash windows).
    pub last_event: Option<EventInfo>,
    /// An injected fault hit a call in this epoch.
    pub fault_fired: bool,
    /// Faults still to fire this epoch.
    pending_faults: Vec<Fault>,
    /// The checker is reading (see [`Ctx::set_checking`]).
    checking: bool,
    pub d1: D1State,
    pub extractor: Option<Arc<dyn RefExtractor>>,
    /// The engines' bytes, refreshed from their state at every filesystem call (shared capacity).
    pub engine_bytes: u64,
    /// The unreadable block that made the current operation fail, until its trace entry.
    unreadable_hit: Option<Unavailable>,
    /// This epoch's operations that failed on an unreadable block: each one's trace entry and
    /// the block (DEC-161).
    unavailable: Vec<(usize, Unavailable)>,
}

impl Ctx {
    fn new(cfg: Settings, epoch: u32, trace: Trace, vars: Vec<VarSlot>) -> Self {
        let pending_faults = cfg.faults.clone();
        Self {
            cfg,
            epoch,
            events: 0,
            vars,
            decider: None,
            trace,
            work: Work::default(),
            frozen: false,
            nth: BTreeMap::new(),
            after: BTreeMap::new(),
            observations: Vec::new(),
            fault: None,
            last_event: None,
            fault_fired: false,
            pending_faults,
            checking: false,
            d1: D1State::default(),
            extractor: None,
            engine_bytes: 0,
            unreadable_hit: None,
            unavailable: Vec::new(),
        }
    }

    /// Adds a new undecided variable.
    pub fn new_var(&mut self, kind: VarKind, name: String, domain: u32) -> VarId {
        debug_assert!(domain >= 2, "a variable with one value is not a choice: {name}");
        let id = self.vars.len() as VarId;
        self.vars.push(VarSlot {
            var: Var {
                kind,
                name,
                domain,
            },
            decided: None,
            inherited: None,
        });
        id
    }

    /// Marks `id` a `^` cell: per choice, what it leaves of the input it inherited (R7a's
    /// references match it, DEC-162).
    pub fn inherit(&mut self, id: VarId, start: Vec<Start>) {
        debug_assert_eq!(start.len(), self.vars[id as usize].var.domain as usize);
        self.vars[id as usize].inherited = Some(start.into());
    }

    /// The current operation fails on an unreadable block, `torn` if the boot's crash tore it:
    /// its trace entry ([`Ctx::read_failed`], or a failed event) records the cause (DEC-161).
    pub fn note_unreadable(&mut self, torn: bool) {
        self.unreadable_hit = Some(Unavailable {
            torn,
        });
    }

    /// The variable's value, deciding it now if this branch has not (rule L1, L4).
    pub fn decide(&mut self, id: VarId) -> u32 {
        let slot = &self.vars[id as usize];
        if let Some(v) = slot.decided {
            return v;
        }
        let var = slot.var.clone();
        let v = match self.decider.as_mut() {
            Some(d) => d.choose(&var),
            None => 0,
        };
        assert!(v < var.domain, "decider chose {v} outside {}'s domain {}", var.name, var.domain);
        self.vars[id as usize].decided = Some(v);
        self.work.decisions += 1;
        v
    }

    /// Every variable's decision in this branch (`None`: undecided).
    pub fn decisions(&self) -> Vec<Option<u32>> {
        self.vars.iter().map(|v| v.decided).collect()
    }

    /// The variable's value if this branch has decided it, without deciding.
    pub fn peek(&self, id: VarId) -> Option<u32> {
        self.vars[id as usize].decided
    }

    /// Starts an event: assigns its index, and returns the fault that hits it, if any. Refuses
    /// after `freeze` (§3.2: no later call changes any image).
    pub fn begin_event(&mut self, store: &str, op: OpKind) -> Result<(u64, Option<Fault>), Frozen> {
        debug_assert!(op.is_event());
        if self.frozen {
            return Err(Frozen);
        }
        self.unreadable_hit = None;
        let index = self.events;
        let key = (store.to_string(), op);
        let nth = *self.nth.get(&key).unwrap_or(&0);
        let info = EventInfo {
            index,
            store: store.to_string(),
            op,
            nth,
            after: self.after.clone(),
        };
        let fault = self
            .pending_faults
            .iter()
            .position(|f| f.at.matches(&info))
            .map(|i| self.pending_faults.remove(i));
        self.last_event = Some(info);
        self.nth.insert(key, nth + 1);
        for (_, since) in self.after.values_mut() {
            *since += 1;
        }
        self.events += 1;
        self.work.events += 1;
        self.fault_fired |= fault.is_some();
        self.fault = fault.clone();
        Ok((index, fault))
    }

    pub fn disarm_faults(&mut self) {
        self.pending_faults.clear();
    }

    /// Where the epoch now crashing began in the trace (its boot's `Crash` entry; 0 for the
    /// forward run): batches appended before it were pending across a process kill.
    pub fn crashed_epoch_start(&self) -> usize {
        let crashed = self.epoch.saturating_sub(1);
        if crashed == 0 {
            return 0;
        }
        self.trace
            .entries
            .iter()
            .position(|e| matches!(e, TraceEntry::Crash { epoch, .. } if *epoch == crashed))
            .unwrap_or(0)
    }

    /// Checker reads (§5.1's store-level checks before open): never a fault's target, and not
    /// counted by `Nth` selectors, which name the system's own reads.
    pub fn set_checking(&mut self, on: bool) {
        self.checking = on;
    }

    /// A read the model itself failed (an unreadable block under `Extended`): in the trace, so
    /// R8 judges it by its region as it does an injected read failure (§4.4, DEC-148: an engine
    /// must fail-stop on unavailable data).
    pub fn read_failed(&mut self, store: &str, op: OpKind, detail: impl FnOnce() -> String) {
        let cell = self.unreadable_hit.take();
        if !self.checking {
            if let Some(cell) = cell {
                self.unavailable.push((self.trace.entries.len(), cell));
            }
            self.trace.entries.push(TraceEntry::ReadFailed {
                epoch: self.epoch,
                store: store.to_string(),
                op,
                detail: detail(),
            });
        }
    }

    /// A read: not an event (never a crash point), but `Nth` selectors over read kinds can fail
    /// it (§4.4). Returns the fault that hits it.
    pub fn begin_read(
        &mut self,
        store: &str,
        op: OpKind,
        detail: impl FnOnce() -> String,
    ) -> Option<Fault> {
        self.unreadable_hit = None;
        if self.checking {
            return None;
        }
        let key = (store.to_string(), op);
        let nth = *self.nth.get(&key).unwrap_or(&0);
        self.nth.insert(key, nth + 1);
        let info = EventInfo {
            index: u64::MAX,
            store: store.to_string(),
            op,
            nth,
            after: BTreeMap::new(),
        };
        let i = self
            .pending_faults
            .iter()
            .position(|f| matches!(f.at, EventSel::Nth { .. }) && f.at.matches(&info))?;
        // The failed read is in the trace (R8 judges it by its region), but never a crash point.
        self.trace.entries.push(TraceEntry::ReadFailed {
            epoch: self.epoch,
            store: store.to_string(),
            op,
            detail: detail(),
        });
        self.fault_fired = true;
        Some(self.pending_faults.remove(i))
    }

    pub fn end_event(&mut self, store: &str, op: OpKind, detail: String, failed: bool) -> EventId {
        let id = EventId {
            epoch: self.epoch,
            index: self.events - 1,
        };
        if let Some(cell) = self.unreadable_hit.take()
            && failed
        {
            self.unavailable.push((self.trace.entries.len(), cell));
        }
        self.trace.entries.push(TraceEntry::Event {
            id,
            store: store.to_string(),
            op,
            detail,
            failed,
        });
        self.fault = None;
        id
    }

    pub fn mark(&mut self, mark: Mark) {
        let kind = mark.kind();
        let e = self.after.entry(kind).or_insert((0, 0));
        e.0 += 1;
        e.1 = 0;
        self.trace.entries.push(TraceEntry::Mark {
            epoch: self.epoch,
            after_events: self.events,
            mark,
        });
    }

    pub fn observe(&mut self, store: &str, what: impl FnOnce() -> String, result: u64) {
        if self.cfg.record_observations {
            self.observations.push(Observation {
                store: store.to_string(),
                what: what(),
                result,
            });
        }
    }
}

/// A store call after [`World::freeze`].
#[derive(Debug, Clone, Copy)]
pub struct Frozen;

/// The stores of a world.
#[derive(Clone, Default)]
pub(crate) struct Stores {
    pub engines: BTreeMap<&'static str, EngineState>,
    pub fs: FsState,
}

impl Stores {
    /// Bytes the engines hold (charged against a shared capacity).
    pub fn engine_bytes(&self) -> u64 {
        self.engines.values().map(EngineState::bytes).sum()
    }
}

pub(crate) struct WorldState {
    pub stores: Stores,
    pub ctx: Ctx,
    on_event: Option<EventHook>,
}

/// A deterministic in-memory world of faulty stores (§3.2).
#[derive(Clone)]
pub struct World(pub(crate) Arc<Mutex<WorldState>>);

/// The boot-time D1 evaluations of an image, by crash kind and boot-time decisions.
/// The boot-time D1 state per crash kind, boot settings that shape the evaluated image (the
/// adversary, granularity and allocation mode) and boot decisions.
type BootD1 = Arc<Mutex<BTreeMap<(CrashKind, String, Vec<Option<u32>>), D1State>>>;

/// A copy of a world's durable and pending state after an event (§4.2).
#[derive(Clone)]
pub struct CrashImage {
    stores: Stores,
    vars: Vec<VarSlot>,
    trace: Trace,
    epoch: u32,
    /// The event after which it was captured.
    pub at: EventId,
    pub work: Work,
    last_write: Option<crate::faulty::fs::LastWrite>,
    extractor: Option<Arc<dyn RefExtractor>>,
    /// The boot-time D1 evaluation per crash kind and boot-time decisions: the same for every
    /// branch of the image, so computed once (Opus cp1 r2 minor C).
    boot_d1: BootD1,
}

impl fmt::Debug for CrashImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CrashImage").field("at", &self.at).finish_non_exhaustive()
    }
}

fn split(cfg: WorldConfig) -> (Settings, Option<EventHook>) {
    let WorldConfig {
        faults,
        capacity,
        shared_capacity,
        granularity,
        adversary,
        allocate,
        on_event,
        d1,
        record_observations,
    } = cfg;
    (
        Settings {
            faults,
            capacity,
            shared_capacity,
            granularity,
            adversary,
            allocate,
            d1,
            record_observations,
        },
        on_event,
    )
}

impl World {
    pub fn new(cfg: WorldConfig) -> Self {
        let (settings, hook) = split(cfg);
        let mut stores = Stores::default();
        stores.fs.set_unit(settings.granularity.unit());
        Self(Arc::new(Mutex::new(WorldState {
            stores,
            ctx: Ctx::new(settings, 0, Trace::default(), Vec::new()),
            on_event: hook,
        })))
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, WorldState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The engine `name` of this world, created empty (with its schema record) on first use,
    /// with subset recovery.
    pub fn engine(&self, name: &'static str, schema: &'static DbSchema) -> Arc<FaultyEngine> {
        self.engine_with(name, schema, Recovery::Subset)
    }

    /// [`engine`](Self::engine), with the recovery of the engine it models.
    pub fn engine_with(
        &self,
        name: &'static str,
        schema: &'static DbSchema,
        recovery: Recovery,
    ) -> Arc<FaultyEngine> {
        {
            let mut w = self.lock();
            let e =
                w.stores.engines.entry(name).or_insert_with(|| EngineState::new(schema, recovery));
            assert_eq!(e.schema, schema, "engine `{name}` reopened with another schema");
            assert_eq!(e.recovery, recovery, "engine `{name}` reopened with another recovery");
        }
        Arc::new(FaultyEngine::new(self.clone(), name, schema))
    }

    /// The filesystem of this world.
    pub fn fs(&self) -> Arc<FaultyFlatFiles> {
        Arc::new(FaultyFlatFiles::new(self.clone()))
    }

    pub fn mark(&self, mark: Mark) {
        self.lock().ctx.mark(mark);
    }

    /// Stops the world: every later store call fails without changing anything.
    pub fn freeze(&self) {
        self.lock().ctx.frozen = true;
    }

    pub fn is_frozen(&self) -> bool {
        self.lock().ctx.frozen
    }

    pub fn trace(&self) -> Trace {
        self.lock().ctx.trace.clone()
    }

    pub fn epoch(&self) -> u32 {
        self.lock().ctx.epoch
    }

    /// This epoch's operations that failed on an unreadable block: each one's index in
    /// [`World::trace`] and the block (DEC-161).
    pub fn unavailable(&self) -> Vec<(usize, Unavailable)> {
        self.lock().ctx.unavailable.clone()
    }

    /// Events so far in this epoch.
    pub fn events(&self) -> u64 {
        self.lock().ctx.events
    }

    pub fn work(&self) -> Work {
        self.lock().ctx.work
    }

    /// Whether an injected fault has hit a call in this epoch.
    pub fn fault_fired(&self) -> bool {
        self.lock().ctx.fault_fired
    }

    /// Runs the event hook after an injected read failure, so a fault run captures the state
    /// at a fatal read too (§4.4: capture, then boot repaired).
    pub(crate) fn after_read_fault(&self) {
        let id = {
            let w = self.lock();
            EventId {
                epoch: w.ctx.epoch,
                index: w.ctx.events.saturating_sub(1),
            }
        };
        self.after_event(id);
    }

    /// What selectors know about the latest event of this epoch.
    pub fn last_event_info(&self) -> Option<EventInfo> {
        self.lock().ctx.last_event.clone()
    }

    /// Decides every undecided variable now, in creation order (eager enumeration, M10).
    pub fn force_all(&self) {
        let mut w = self.lock();
        for id in 0..w.ctx.vars.len() as VarId {
            w.ctx.decide(id);
        }
    }

    pub fn add_work(&self, w: &Work) {
        self.lock().ctx.work.add(w);
    }

    pub fn observations(&self) -> Vec<Observation> {
        self.lock().ctx.observations.clone()
    }

    /// Every variable created so far, with this branch's decision.
    pub fn vars(&self) -> Vec<(Var, Option<u32>)> {
        self.lock().ctx.vars.iter().map(|s| (s.var.clone(), s.decided)).collect()
    }

    /// The `^` cells this branch has decided.
    pub fn inherited_decisions(&self) -> Vec<InheritedDecision> {
        let w = self.lock();
        let decided =
            w.ctx.vars.iter().filter_map(|s| Some((s, s.decided?, s.inherited.as_ref()?)));
        decided
            .map(|(s, c, i)| InheritedDecision {
                name: s.var.name.clone(),
                start: i[c as usize],
            })
            .collect()
    }

    /// What each choice of every `^` cell leaves of its inherited input, decided or not.
    pub fn inherited_starts(&self) -> BTreeMap<String, Arc<[Start]>> {
        let w = self.lock();
        let cells = w.ctx.vars.iter().filter_map(|s| Some((s, s.inherited.as_ref()?)));
        cells.map(|(s, i)| (s.var.name.clone(), i.clone())).collect()
    }

    pub fn set_decider(&self, d: Box<dyn Decider>) {
        self.lock().ctx.decider = Some(d);
    }

    /// Declares the system's dependencies, and evaluates D1 against the durable state at once
    /// (so a booted world's recovery is checked before its first barrier).
    pub fn set_extractor(&self, x: Arc<dyn RefExtractor>) {
        self.lock().ctx.extractor = Some(x);
        self.after_barrier();
    }

    /// Re-evaluates D1 if its protected ranges depend on records whose presence the branch has
    /// decided since (before a filesystem mutation).
    pub(crate) fn refresh_d1(&self) {
        let full = {
            let mut guard = self.lock();
            let w = &mut *guard;
            if !(w.ctx.d1.conservative && w.ctx.d1.at_decisions != w.ctx.work.decisions) {
                return;
            }
            let view = crate::faulty::check::WorldDurableView::new(
                std::borrow::Cow::Borrowed(&w.stores),
                w.ctx.decisions(),
            );
            let full = w.ctx.d1.refresh(&view);
            drop(view);
            w.ctx.d1.at_decisions = w.ctx.work.decisions;
            full
        };
        if full {
            self.after_barrier();
        }
    }

    /// Drops the faults this epoch has not fired yet.
    pub fn disarm_faults(&self) {
        self.lock().ctx.disarm_faults();
    }

    /// Runs `f` as the checker: its reads cannot fire or count toward an injected fault.
    pub fn checking<T>(&self, f: impl FnOnce() -> T) -> T {
        self.lock().ctx.set_checking(true);
        let out = catch_unwind(AssertUnwindSafe(f));
        self.lock().ctx.set_checking(false);
        out.unwrap_or_else(|p| resume_unwind(p))
    }

    pub fn has_extractor(&self) -> bool {
        self.lock().ctx.extractor.is_some()
    }

    /// Evaluates D1 once more (at the end of recovery: the decisions made since the last
    /// barrier can resolve pending requirements).
    pub fn evaluate_d1(&self) {
        self.after_barrier();
    }

    /// DEC-212: whether `path` names no file because a crash lost the directory entry a failed
    /// `sync_dir` left pending. R1 entitles a durable reference into it: Core's `DirectoryCommit`
    /// ignores the failure.
    pub fn lost_to_failed_dir_sync(&self, path: &std::path::Path) -> bool {
        let w = self.lock();
        w.stores.fs.lost_to_failed_dir_sync(path, &w.ctx)
    }

    /// D1 violations found so far (§5.3).
    pub fn d1_violations(&self) -> Vec<Violation> {
        self.lock().ctx.d1.violations.clone()
    }

    pub fn durable_view(&self) -> crate::faulty::check::WorldDurableView<'static> {
        let w = self.lock();
        crate::faulty::check::WorldDurableView::new(
            std::borrow::Cow::Owned(w.stores.clone()),
            w.ctx.decisions(),
        )
    }

    /// A copy-on-write copy of the stores and decisions after the latest event.
    pub fn capture(&self) -> CrashImage {
        let mut w = self.lock();
        w.ctx.work.images += 1;
        let at = EventId {
            epoch: w.ctx.epoch,
            index: w.ctx.events.saturating_sub(1),
        };
        let cloned = w.stores.fs.units() as u64;
        w.ctx.work.cloned_units += cloned;
        let last_write = w.stores.fs.last_write.clone();
        CrashImage {
            stores: w.stores.clone(),
            vars: w.ctx.vars.clone(),
            trace: w.ctx.trace.clone(),
            epoch: w.ctx.epoch,
            at,
            work: w.ctx.work,
            last_write,
            extractor: w.ctx.extractor.clone(),
            boot_d1: Arc::default(),
        }
    }

    /// Crashes this world in place: its stores become what a boot of its latest image holds,
    /// so existing store handles see the new epoch (the `kv_model` hook and model self-tests).
    pub fn crash_in_place(&self, kind: CrashKind, decider: Box<dyn Decider>) {
        let img = self.capture();
        let cfg = self.lock().ctx.cfg.to_config();
        let new = img.boot(kind, decider, cfg);
        let mut theirs = new.lock();
        let mut ours = self.lock();
        std::mem::swap(&mut ours.stores, &mut theirs.stores);
        std::mem::swap(&mut ours.ctx, &mut theirs.ctx);
    }

    /// Runs the event hook (outside the lock) after an event.
    pub(crate) fn after_event(&self, id: EventId) {
        let hook = self.lock().on_event.take();
        if let Some(mut hook) = hook {
            hook(self, id);
            let mut w = self.lock();
            if w.on_event.is_none() {
                w.on_event = Some(hook);
            }
        }
    }

    /// Runs the standing D1 checker after a completed barrier (§5.3).
    pub(crate) fn after_barrier(&self) {
        let mut guard = self.lock();
        let w = &mut *guard;
        if !w.ctx.cfg.d1 {
            return;
        }
        let Some(x) = w.ctx.extractor.clone() else {
            return;
        };
        // The view borrows the stores under the lock (no copy); extractors only read it.
        let view = crate::faulty::check::WorldDurableView::new(
            std::borrow::Cow::Borrowed(&w.stores),
            w.ctx.decisions(),
        );
        let found = crate::faulty::check::standing_check(&*x, &view, &w.ctx.d1.pending);
        drop(view);
        let epoch = w.ctx.epoch;
        let events = w.ctx.events;
        w.ctx.d1.set_required(found.required, found.via_reads);
        w.ctx.d1.pending = found.pending;
        w.ctx.d1.at_decisions = w.ctx.work.decisions;
        for v in found.violations {
            w.ctx.d1.report(v.at(epoch, events));
        }
    }
}

impl CrashImage {
    /// Boots the image after a crash of `kind`: a new epoch whose stores hold what survives, with
    /// every undecided cell resolved by `decider` when first observed.
    pub fn boot(&self, kind: CrashKind, decider: Box<dyn Decider>, cfg: WorldConfig) -> World {
        let (settings, hook) = split(cfg);
        let mut trace = self.trace.clone();
        let epoch = self.epoch + 1;
        trace.entries.push(TraceEntry::Crash {
            epoch,
            kind,
        });
        let mut ctx = Ctx::new(settings, epoch, trace, self.vars.clone());
        ctx.decider = Some(decider);
        let mut stores = self.stores.clone();
        for (name, e) in stores.engines.iter_mut() {
            e.crash(name, kind, &mut ctx);
        }
        if kind == CrashKind::ProcessKill
            && let Some(lw) = &self.last_write
        {
            stores.fs.interrupt_write(lw, self.at.index, &mut ctx);
        }
        stores.fs.crash(kind, &mut ctx);
        ctx.extractor = self.extractor.clone();
        let world = World(Arc::new(Mutex::new(WorldState {
            stores,
            ctx,
            on_event: hook,
        })));
        // D1 knows what the durable records reference before recovery runs (§5.3). A boot with
        // D1 off neither uses nor fills the cache.
        let (key, d1_on) = {
            let w = world.lock();
            let c = &w.ctx.cfg;
            let settings = format!("{:?}/{:?}/{:?}", c.adversary, c.granularity, c.allocate);
            ((kind, settings, w.ctx.decisions()), c.d1)
        };
        if !d1_on {
            return world;
        }
        let cached = self.boot_d1.lock().unwrap_or_else(|e| e.into_inner()).get(&key).cloned();
        match cached {
            Some(d1) => world.lock().ctx.d1 = d1,
            None => {
                world.after_barrier();
                let d1 = world.lock().ctx.d1.clone();
                self.boot_d1.lock().unwrap_or_else(|e| e.into_inner()).insert(key, d1);
            }
        }
        world
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    /// The number of events in the image's epoch (its crash point is after the last).
    pub fn epoch(&self) -> u32 {
        self.epoch
    }
}
