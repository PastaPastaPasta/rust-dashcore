//! b3's test subscriber on [`ChainSut`](super::ChainSut) (CH2 §7.2 b3, §6.4): a real
//! [`SubscriberDriver`] over a [`CrashSubject`]'s data object on its own `FaultyEngine` store,
//! fed by the scripted producer as V steps its tip, with W's hub and its pump. ST3a's toy
//! subscriber (`dashd_store::faulty::toy::sub`) is the model it replaces.
//!
//! - **Synced** (`index=synced:b:d`): live from genesis; after each tip step the driver processes
//!   until at most `b` events are queued (backlog), and W's step 6 and step 3b pump it.
//! - **Syncing** (`index=syncing:c:d`): catch-up from a snapshot, a step per `s` op, a capped
//!   commit every `c` blocks and at each `k` op; at V's tip it hands off (F10).
//!
//! The subject is the toy index ([`ToyIndex`], every row class) unless the mode is prefixed:
//! `inexact-` for the append-count toy, `files-` for staged file writes (for example
//! `index=files-synced:0:96`), or `rotating-` for staged writes that rotate through numbered
//! files. A subject supplies its data object, block source, checks at L and flat files
//! ([`CrashSubject`]); the boot, steps, disable and checks below serve any subject. One defined
//! outside this module is parsed once [`register_subject`] has named it.
//!
//! At each boot, after activation: R11a (L not ahead of G), R11b (every block the catch-up reads
//! is readable), R11c (i) after open's blind rewind and (ii) after the catch-up, which answers a
//! block with a flush.
//!
//! `D` disables it for the rest of the epoch (S11c, the toy's `D`): it stops, its lock goes, and
//! its store records the disable durably, which waives its locator's requirements and R11b. At
//! the next boot it is enabled again: the open's prune check (Core's `BaseIndex::Init`) refuses a
//! catch-up over pruned blocks, which leaves it not started (sanctioned), or it opens and deletes
//! the record.

use std::any::TypeId;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, RwLock};

use dash_chain::{BlockRef, FlatFilePos as IndexPos, IndexAccess, Needs, Status};
use dash_consensus::BlockId;
use dash_consensus::primitives::BlockHash;
use dashd_store::faulty::{Abort, FaultyEngine, LocatorKind as MarkKind, Mark, World};
use dashd_store::{Cf, Commit, DbSchema, KvBatch, KvRead};

use super::world::{WorldObserver, WorldSink, hash};
use crate::flush::SubscriberHub;
use crate::planted::planted;
use crate::store::{Failure, GuardedEngine};
use crate::subscriber::{
    BlockRows, BlockSource, CfRange, ConnectedBlock, DisconnectedBlock, DriverConfig, DurableBelow,
    FileAppend, FilePos, LocatorKind, MISSING_FILE, NoBlocks, PRUNE_VIOLATION, Row, RowSchema,
    ScriptedFeeds, StateRead, SubStep, SubscriberClass, SubscriberData, SubscriberDriver,
    SubscriberError, SubscriberId, SyncProgress, TOY_BEST, TOY_DB, TOY_INEXACT, TOY_KEYED,
    TOY_META, ToyFiles, ToyIndex, ToyInexact, ToyState, decode_locator, store_err, toy_check_at_l,
    toy_expected, toy_file_path, toy_rows_above,
};
use crate::tuning::{JOURNAL_BUDGET, UNBOUNDED_DELETE_SPAN};

/// The subscriber's store.
pub const SUB_STORE: &str = "subscriber";

/// The disable's record in the subscriber's store (the toy's `K_DISABLED`), in its locator's
/// column family by default.
const SUB_DISABLED: &[u8] = b"disabled";

/// Q: each feed's data-event bound.
const QUEUE: usize = 4;

/// A subscriber's class at its first boot, and its limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubMode {
    /// Live from genesis, with up to `backlog` events queued after each tip step.
    Synced {
        backlog: u32,
    },
    /// Catching up, with a capped commit every `cadence` blocks.
    Syncing {
        cadence: u32,
    },
}

/// `index=` in a repro line.
#[derive(Clone, Copy)]
pub struct SubConfig {
    subject: &'static dyn Slot,
    pub mode: SubMode,
    /// The journal's block bound (§2.9 item 6) and the blind delete's span.
    pub d_max: u32,
}

impl SubConfig {
    pub fn synced(backlog: u32) -> Self {
        Self::with_subject(
            &ToySubject::Toy,
            SubMode::Synced {
                backlog,
            },
            96,
        )
    }

    pub fn syncing(cadence: u32) -> Self {
        Self::with_subject(
            &ToySubject::Toy,
            SubMode::Syncing {
                cadence,
            },
            96,
        )
    }

    /// `subject` in `mode`; its `index=` form is the subject's prefix before the mode's. A
    /// subject outside this module is parsed only once [`register_subject`] has named it.
    pub fn with_subject<S: CrashSubject>(subject: &'static S, mode: SubMode, d_max: u32) -> Self {
        Self {
            subject,
            mode,
            d_max,
        }
    }

    /// The subject's name: its log lines and abort messages (`<name>: …`, D29) carry it.
    pub(super) fn name(&self) -> &'static str {
        self.subject.name()
    }

    /// Whether `file` is one of the subject's own flat files.
    pub(super) fn owns_file(&self, file: &str) -> bool {
        self.subject.file_paths().contains(&file)
    }

    /// Whether `message` is the subscriber's own abort (D29).
    pub(super) fn is_abort(&self, message: &str) -> bool {
        message.strip_prefix(self.name()).is_some_and(|m| m.starts_with(": "))
    }

    /// Whether `message` is the abort of the driver's open refused over a missing flat file of
    /// the subject's ([`MISSING_FILE`]), as [`Subs::fail`] gives it.
    pub(super) fn is_missing_file(&self, message: &str) -> bool {
        !self.subject.file_paths().is_empty()
            && message
                .strip_prefix(self.name())
                .and_then(|m| m.strip_prefix(": "))
                .and_then(|m| missing_file(self.name(), m))
                .is_some()
    }

    /// The disable's log line.
    pub(super) fn disabled_line(&self) -> String {
        format!("{} disabled", self.name())
    }

    /// The locator's place in the subscriber's store.
    pub(super) fn locator(&self) -> (Cf, &'static [u8]) {
        self.subject.locator()
    }

    /// The disable record's place in the subscriber's store.
    pub(super) fn disabled_key(&self) -> (Cf, &'static [u8]) {
        self.subject.disabled_key()
    }

    fn driver(&self) -> DriverConfig {
        DriverConfig {
            journal_budget: JOURNAL_BUDGET,
            unbounded_delete_span: UNBOUNDED_DELETE_SPAN,
            d_max: self.subject.d_max(self.d_max),
            cadence: match self.mode {
                SubMode::Syncing {
                    cadence,
                } => cadence,
                SubMode::Synced {
                    ..
                } => self.d_max,
            },
        }
    }
}

impl PartialEq for SubConfig {
    fn eq(&self, other: &Self) -> bool {
        self.subject.prefix() == other.subject.prefix()
            && self.mode == other.mode
            && self.d_max == other.d_max
    }
}

impl Eq for SubConfig {}

impl fmt::Debug for SubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubConfig")
            .field("subject", &self.subject.name())
            .field("prefix", &self.subject.prefix())
            .field("mode", &self.mode)
            .field("d_max", &self.d_max)
            .finish()
    }
}

impl fmt::Display for SubConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.subject.prefix())?;
        match self.mode {
            SubMode::Synced {
                backlog,
            } => write!(f, "synced:{backlog}:{}", self.d_max),
            SubMode::Syncing {
                cadence,
            } => write!(f, "syncing:{cadence}:{}", self.d_max),
        }
    }
}

/// The subjects of this module that `index=` names by a prefix; a mode without one is
/// [`ToySubject::Toy`]'s. Adding a subject here includes its prefix: [`FromStr`] takes the first
/// prefix that matches, so the prefixes are distinct, none is a prefix of another, and an empty
/// one comes last. Subjects defined elsewhere are [`register_subject`]'s.
static PREFIXED: &[&dyn Slot] = &[
    &ToySubject::Inexact,
    &ToySubject::Files,
    &ToySubject::Rotating,
    #[cfg(test)]
    &tests::Counter,
    #[cfg(test)]
    &tests::Epoch,
];

/// The subjects [`register_subject`] has named, by their type.
static REGISTERED: RwLock<Vec<(TypeId, &'static dyn Slot)>> = RwLock::new(Vec::new());

/// Names `subject`, defined outside this module (an integration test's), for `index=`'s
/// parsing, so the repro lines of its scenarios replay. Registering the same subject again does
/// nothing. Test support behind the `crash` feature.
///
/// # Panics
///
/// An empty prefix, one that would capture a default-toy mode (`synced:`, `syncing:`), or one
/// that equals, extends or is extended by another subject's.
pub fn register_subject<S: CrashSubject>(subject: &'static S) {
    let prefix = CrashSubject::prefix(subject);
    assert!(!prefix.is_empty(), "a registered subject needs a prefix");
    if let Some(mode) = shadowed_mode(prefix) {
        panic!("the prefix {prefix:?} shadows the mode {mode:?}");
    }
    let mut registered = REGISTERED.write().unwrap_or_else(PoisonError::into_inner);
    let id = TypeId::of::<S>();
    if registered.iter().any(|(t, s)| *t == id && s.prefix() == prefix) {
        return;
    }
    let taken = PREFIXED.iter().copied().chain(registered.iter().map(|(_, s)| *s));
    for other in taken.map(|s| s.prefix()) {
        assert!(
            !other.starts_with(prefix) && !prefix.starts_with(other),
            "the prefix {prefix:?} overlaps {other:?}"
        );
    }
    registered.push((id, subject));
}

/// The file number in the driver's open refused over a missing flat file ([`MISSING_FILE`]).
fn missing_file(name: &str, message: &str) -> Option<usize> {
    message.strip_prefix(name)?.strip_prefix(MISSING_FILE)?.parse().ok()
}

/// The default toy's mode whose `index=` form `prefix` would capture.
fn shadowed_mode(prefix: &str) -> Option<&'static str> {
    ["synced", "syncing"].into_iter().find(|m| format!("{m}:").starts_with(prefix))
}

impl FromStr for SubConfig {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let registered = REGISTERED.read().unwrap_or_else(PoisonError::into_inner);
        let (subject, s) = PREFIXED
            .iter()
            .copied()
            .chain(registered.iter().map(|(_, s)| *s))
            .find_map(|sub| Some((sub, s.strip_prefix(sub.prefix())?)))
            .unwrap_or((&ToySubject::Toy, s));
        let mut parts = s.split(':');
        let (mode, n, d) = (parts.next(), parts.next(), parts.next());
        let num = |x: Option<&str>| x.ok_or(())?.parse::<u32>().map_err(|_| ());
        let (n, d_max) = (num(n)?, num(d)?);
        let mode = match mode {
            Some("synced") => SubMode::Synced {
                backlog: n,
            },
            Some("syncing") => SubMode::Syncing {
                cadence: n.max(1),
            },
            _ => return Err(()),
        };
        if parts.next().is_some() || d_max == 0 {
            return Err(());
        }
        Ok(Self {
            subject,
            mode,
            d_max,
        })
    }
}

/// The block index's structure as the hub and the driver read it (F8): the arena's entries,
/// appended as V indexes them, under a lock V never holds while it calls them.
#[derive(Clone, Default)]
pub struct SharedIndex(Arc<RwLock<Nodes>>);

#[derive(Default)]
struct Nodes {
    nodes: Vec<(BlockHash, Option<BlockId>, u32)>,
    by_hash: HashMap<BlockHash, BlockId>,
    /// `HAVE_DATA`, which the open's prune check reads (S11c).
    data: Vec<bool>,
}

impl SharedIndex {
    fn nodes(&self) -> std::sync::RwLockReadGuard<'_, Nodes> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// The arena's entry `id`, which it has just pushed.
    pub(super) fn push(
        &self,
        id: BlockId,
        hash: BlockHash,
        prev: Option<BlockId>,
        height: u32,
        data: bool,
    ) {
        let mut n = self.0.write().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(id.0 as usize, n.nodes.len(), "the arena's order");
        n.nodes.push((hash, prev, height));
        n.by_hash.insert(hash, id);
        n.data.push(data);
    }

    /// The arena's prune cleared `id`'s data.
    pub(super) fn clear_data(&self, id: BlockId) {
        self.0.write().unwrap_or_else(PoisonError::into_inner).data[id.0 as usize] = false;
    }

    fn get(&self, id: BlockId) -> Option<(BlockHash, Option<BlockId>, u32)> {
        self.nodes().nodes.get(id.0 as usize).copied()
    }

    /// The path from genesis to `tip`.
    pub(super) fn path(&self, tip: &BlockRef) -> Vec<BlockRef> {
        let mut out = Vec::new();
        let mut cur = Some(tip.id);
        while let Some(id) = cur {
            let Some((hash, prev, height)) = self.get(id) else {
                break;
            };
            out.push(BlockRef {
                id,
                hash,
                height,
            });
            cur = prev;
        }
        out.reverse();
        out
    }
}

impl IndexAccess for SharedIndex {
    fn lookup(&self, h: &BlockHash) -> Option<BlockId> {
        self.nodes().by_hash.get(h).copied()
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.get(id).map(|n| n.0)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        while self.get(id)?.2 > height {
            id = self.get(id)?.1?;
        }
        (self.get(id)?.2 == height).then_some(id)
    }

    fn lca(&self, a: BlockId, b: BlockId) -> Option<BlockId> {
        let h = self.get(a)?.2.min(self.get(b)?.2);
        let (mut a, mut b) = (self.ancestor(a, h)?, self.ancestor(b, h)?);
        while a != b {
            (a, b) = (self.get(a)?.1?, self.get(b)?.1?);
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.get(id).map(|n| n.2)
    }

    fn have_data_pos(&self, _: BlockId) -> Option<IndexPos> {
        None
    }

    fn undo_pos(&self, _: BlockId) -> Option<IndexPos> {
        None
    }

    fn status(&self, id: BlockId) -> Option<Status> {
        let data = *self.nodes().data.get(id.0 as usize)?;
        Some(Status(if data {
            Status::HAVE_DATA
        } else {
            0
        }))
    }
}

/// What the crash harness needs from a subscriber under test: its data object, block source,
/// store and checks. `Subs` runs the boot, steps, disable and R11 checks on any implementor.
pub trait CrashSubject: Sync + 'static {
    /// The data object's state.
    type State: Clone + Send + 'static;
    /// Its flat files, for subjects with any ([`NoFiles`] otherwise).
    type Files: SubjectFiles;

    /// The start of its `index=` mode (empty for the default subject).
    fn prefix(&self) -> &'static str;
    /// Its name in log lines and abort messages.
    fn name(&self) -> &'static str;
    /// Its locator's place in its store, its data object's `RowSchema::locator`.
    fn locator(&self) -> (Cf, &'static [u8]);
    /// The disable record's place in its store.
    fn disabled_key(&self) -> (Cf, &'static [u8]) {
        (self.locator().0, SUB_DISABLED)
    }
    /// The schema of its store, [`SUB_STORE`].
    fn db(&self) -> &'static DbSchema;
    /// The driver's journal bound from `index=`'s `d`; `None` disables the bound.
    fn d_max(&self, d_max: u32) -> Option<u32> {
        Some(d_max)
    }
    /// A fresh data object on `world`, for each open.
    ///
    /// # Errors
    ///
    /// Its construction failed (a recovery fault in a write it makes, such as an epoch row): the
    /// open is refused with it, as the driver's open would be.
    fn data(
        &self,
        world: &World,
    ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = Self::State>>, SubscriberError>;
    /// The block data its catch-up reads.
    fn blocks(&self) -> Arc<dyn BlockSource>;
    /// Its flat files on `world`, if it has any.
    fn files(&self, _world: &World) -> Option<Self::Files> {
        None
    }
    /// Its flat files' paths, as the world's file events name them; none without files.
    fn file_paths(&self) -> &'static [&'static str] {
        &[]
    }
    /// R11c: the store's rows at L, `path` being genesis to L; the first mismatch.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String>;
    /// R11c (i): what the blind rewind must have removed above L, checked before
    /// [`check_at_l`](Self::check_at_l).
    fn check_rewound(&self, _raw: &FaultyEngine, _path: &[BlockRef]) -> Option<String> {
        None
    }
    /// R11c (ii): the driver's reads at V's tip, `path` being genesis to it.
    fn check_tip(&self, _reader: &dyn CrashReader, _path: &[BlockRef]) -> Option<String> {
        None
    }
}

/// A subject's flat files, which the sweep faults as ST2's: their paths, and what they hold at
/// L.
pub trait SubjectFiles {
    /// R11c on the files at L (lengths, torn appends) and the rows that reference them; the
    /// first mismatch.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String>;
}

/// A subject without flat files.
pub enum NoFiles {}

impl SubjectFiles for NoFiles {
    fn check_at_l(&self, _: &FaultyEngine, _: &[BlockRef]) -> Option<String> {
        match *self {}
    }
}

/// The driver's reads, for [`CrashSubject::check_tip`].
pub trait CrashReader {
    /// The read at the driver's progress: the journal over the store.
    ///
    /// # Errors
    ///
    /// The read failed.
    fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError>;
}

/// A subject's data object, under the subject's name.
struct SubjectData<T>(Box<dyn SubscriberData<E = FaultyEngine, State = T>>, &'static str);

impl<T: Clone + Send> SubscriberData for SubjectData<T> {
    type E = FaultyEngine;
    type State = T;
    fn name(&self) -> &'static str {
        self.1
    }
    fn needs(&self) -> Needs {
        self.0.needs()
    }
    fn mandatory(&self) -> bool {
        self.0.mandatory()
    }
    fn start_height(&self) -> u32 {
        self.0.start_height()
    }
    fn schema(&self) -> &'static RowSchema {
        self.0.schema()
    }
    fn initial_state(&self) -> T {
        self.0.initial_state()
    }
    fn connect(&self, b: &ConnectedBlock<'_>, at: &T) -> Result<(BlockRows, T), SubscriberError> {
        self.0.connect(b, at)
    }
    fn stage_disconnected(
        &self,
        b: &DisconnectedBlock<'_>,
        at: &T,
        below: &DurableBelow<'_, FaultyEngine>,
    ) -> Result<(Vec<Row>, T), SubscriberError> {
        self.0.stage_disconnected(b, at, below)
    }
    fn archive(&self, rows: &[Row]) -> Vec<Row> {
        self.0.archive(rows)
    }
    fn with_high_water(&self, at: &T, latest: &T) -> T {
        self.0.with_high_water(at, latest)
    }
    fn with_file_floor(&self, at: &T, end: FilePos) -> T {
        self.0.with_file_floor(at, end)
    }
    fn encode_state(&self, at: &T) -> Vec<Row> {
        self.0.encode_state(at)
    }
    fn decode_state(
        &self,
        rows: &[StateRead],
        below: &DurableBelow<'_, FaultyEngine>,
    ) -> Result<T, SubscriberError> {
        self.0.decode_state(rows, below)
    }
    fn height_ranges(&self, lo: u32, hi: u32) -> Vec<CfRange> {
        self.0.height_ranges(lo, hi)
    }
    fn max_entry_bytes(&self) -> u64 {
        self.0.max_entry_bytes()
    }
    fn state_bytes(&self, state: &T) -> u64 {
        self.0.state_bytes(state)
    }
    fn sync_files(&mut self) -> Result<(), SubscriberError> {
        self.0.sync_files()
    }
    fn append_file(&mut self, append: &FileAppend) -> Result<(), SubscriberError> {
        self.0.append_file(append)
    }
    fn file_len(&self) -> Result<FilePos, SubscriberError> {
        self.0.file_len()
    }
    fn file_pos(&self, s: &T) -> Option<FilePos> {
        self.0.file_pos(s)
    }
    fn file_size(&self, n: u32) -> Result<Option<u64>, SubscriberError> {
        self.0.file_size(n)
    }
}

/// The driver's locator hook.
type OnLocator = Box<dyn Fn(LocatorKind, &BlockRef) + Send>;

/// The driver as [`Subs`] steps it, whatever its subject's state.
trait SlotDriver: CrashReader + Send {
    fn id(&self) -> SubscriberId;
    fn class(&self) -> SubscriberClass;
    fn durable(&self) -> Option<BlockRef>;
    fn progress(&self) -> Option<BlockRef>;
    fn best_block(&self) -> Option<BlockRef>;
    fn set_on_locator(&mut self, f: OnLocator);
    fn step(&mut self) -> Result<SubStep, SubscriberError>;
    fn sync_step(&mut self) -> Result<SyncProgress, SubscriberError>;
    fn tick(&mut self) -> Result<(), SubscriberError>;
    fn finish(&mut self) -> Result<(), SubscriberError>;
}

impl<D: SubscriberData<E = FaultyEngine>> CrashReader for SubscriberDriver<D> {
    fn read(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError> {
        SubscriberDriver::read(self, cf, key)
    }
}

impl<D: SubscriberData<E = FaultyEngine>> SlotDriver for SubscriberDriver<D> {
    fn id(&self) -> SubscriberId {
        SubscriberDriver::id(self)
    }
    fn class(&self) -> SubscriberClass {
        SubscriberDriver::class(self)
    }
    fn durable(&self) -> Option<BlockRef> {
        SubscriberDriver::durable(self)
    }
    fn progress(&self) -> Option<BlockRef> {
        SubscriberDriver::progress(self)
    }
    fn best_block(&self) -> Option<BlockRef> {
        SubscriberDriver::best_block(self)
    }
    fn set_on_locator(&mut self, f: OnLocator) {
        SubscriberDriver::set_on_locator(self, f);
    }
    fn step(&mut self) -> Result<SubStep, SubscriberError> {
        SubscriberDriver::step(self)
    }
    fn sync_step(&mut self) -> Result<SyncProgress, SubscriberError> {
        SubscriberDriver::sync_step(self)
    }
    fn tick(&mut self) -> Result<(), SubscriberError> {
        SubscriberDriver::tick(self)
    }
    fn finish(&mut self) -> Result<(), SubscriberError> {
        SubscriberDriver::finish(self)
    }
}

/// A [`CrashSubject`] with its state erased, as [`SubConfig`] carries it.
trait Slot: Sync {
    fn prefix(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn db(&self) -> &'static DbSchema;
    fn d_max(&self, d_max: u32) -> Option<u32>;
    fn locator(&self) -> (Cf, &'static [u8]);
    fn disabled_key(&self) -> (Cf, &'static [u8]);
    fn open(
        &self,
        world: &World,
        db: Arc<GuardedEngine<FaultyEngine>>,
        hub: Arc<SubscriberHub>,
        cfg: DriverConfig,
    ) -> Result<Box<dyn SlotDriver>, SubscriberError>;
    fn file_paths(&self) -> &'static [&'static str];
    fn has_files(&self, world: &World) -> bool;
    fn check_at_l(&self, world: &World, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String>;
    fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String>;
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String>;
}

impl<S: CrashSubject> Slot for S {
    fn prefix(&self) -> &'static str {
        CrashSubject::prefix(self)
    }
    fn name(&self) -> &'static str {
        CrashSubject::name(self)
    }
    fn db(&self) -> &'static DbSchema {
        CrashSubject::db(self)
    }
    fn d_max(&self, d_max: u32) -> Option<u32> {
        CrashSubject::d_max(self, d_max)
    }
    fn locator(&self) -> (Cf, &'static [u8]) {
        CrashSubject::locator(self)
    }
    fn disabled_key(&self) -> (Cf, &'static [u8]) {
        CrashSubject::disabled_key(self)
    }
    fn open(
        &self,
        world: &World,
        db: Arc<GuardedEngine<FaultyEngine>>,
        hub: Arc<SubscriberHub>,
        cfg: DriverConfig,
    ) -> Result<Box<dyn SlotDriver>, SubscriberError> {
        let data = self.data(world)?;
        let schema = data.schema();
        assert_eq!(schema.locator, CrashSubject::locator(self), "the subject's locator");
        let (dcf, _) = CrashSubject::disabled_key(self);
        debug_assert!(
            !schema.height_cfs.contains(&dcf) && !schema.inexact_cfs.contains(&dcf),
            "the blind rewind's range deletes would erase the disable record"
        );
        debug_assert_eq!(
            self.files(world).is_some(),
            schema.has_files,
            "the subject's files and its schema's"
        );
        debug_assert_eq!(
            self.files(world).is_some(),
            !CrashSubject::file_paths(self).is_empty(),
            "the subject's files and their paths"
        );
        let data = SubjectData(data, CrashSubject::name(self));
        let d = SubscriberDriver::open(data, db, hub, self.blocks(), cfg)?;
        Ok(Box::new(d))
    }
    fn file_paths(&self) -> &'static [&'static str] {
        CrashSubject::file_paths(self)
    }
    fn has_files(&self, world: &World) -> bool {
        self.files(world).is_some()
    }
    fn check_at_l(&self, world: &World, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        CrashSubject::check_at_l(self, raw, path)
            .or_else(|| self.files(world)?.check_at_l(raw, path))
    }
    fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        CrashSubject::check_rewound(self, raw, path)
    }
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
        CrashSubject::check_tip(self, reader, path)
    }
}

/// The toy family (`dashd_store::faulty::toy::sub`'s rows on [`TOY_DB`]): the toy index, the
/// append-count toy (`inexact-`), the toy with staged file writes (`files-`), and that toy with
/// numbered files of [`ROTATE_AT`] bytes each (`rotating-`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToySubject {
    Toy,
    Inexact,
    Files,
    Rotating,
}

const SUB_FILE: &str = "/dd/subscriber.dat";

/// The rotating toy's files the sweeps name ([`ToyFiles::file_path`]), more than its sweeps write:
/// a harness bound, not the toy's. A fault on a later file is not the subject's, so the oracle
/// flags it.
const ROTATED_FILES: u32 = 16;

fn rotating_paths() -> &'static [&'static str] {
    static PATHS: OnceLock<Vec<&'static str>> = OnceLock::new();
    PATHS.get_or_init(|| {
        let base = std::path::Path::new(SUB_FILE);
        let path = |n| &*toy_file_path(base, n).to_string_lossy().into_owned().leak();
        (0..ROTATED_FILES).map(path).collect()
    })
}

/// The rotating toy's file limit: two of its 40-byte appends, so the third rotates.
const ROTATE_AT: u64 = 100;

/// The toy's staged-write files.
struct ToyFileFaults(ToyFiles<FaultyEngine>);

impl SubjectFiles for ToyFileFaults {
    /// The toy checks its rows with their file offsets stripped, and then the file bytes.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        self.0.check_at_l(raw, path).err()
    }
}

impl CrashSubject for ToySubject {
    type State = ToyState;
    type Files = ToyFileFaults;

    fn prefix(&self) -> &'static str {
        match self {
            ToySubject::Toy => "",
            ToySubject::Inexact => "inexact-",
            ToySubject::Files => "files-",
            ToySubject::Rotating => "rotating-",
        }
    }
    fn name(&self) -> &'static str {
        "toyindex"
    }
    fn locator(&self) -> (Cf, &'static [u8]) {
        (TOY_META, TOY_BEST)
    }
    fn db(&self) -> &'static DbSchema {
        &TOY_DB
    }
    fn d_max(&self, d_max: u32) -> Option<u32> {
        (*self != ToySubject::Inexact).then_some(d_max)
    }
    fn data(
        &self,
        world: &World,
    ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = ToyState>>, SubscriberError> {
        Ok(match self {
            ToySubject::Toy => Box::new(ToyIndex::<FaultyEngine>::mandatory()),
            ToySubject::Inexact => Box::new(ToyInexact::<FaultyEngine>::default()),
            ToySubject::Files | ToySubject::Rotating => {
                Box::new(self.files(world).expect("a file subject").0)
            }
        })
    }
    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(NoBlocks)
    }
    fn files(&self, world: &World) -> Option<ToyFileFaults> {
        let path = PathBuf::from(SUB_FILE);
        match self {
            ToySubject::Toy | ToySubject::Inexact => None,
            ToySubject::Files => Some(ToyFileFaults(ToyFiles::new(world.fs(), path))),
            ToySubject::Rotating => {
                Some(ToyFileFaults(ToyFiles::rotating(world.fs(), path, ROTATE_AT)))
            }
        }
    }
    fn file_paths(&self) -> &'static [&'static str] {
        match self {
            ToySubject::Toy | ToySubject::Inexact => &[],
            ToySubject::Files => &[SUB_FILE],
            ToySubject::Rotating => rotating_paths(),
        }
    }
    /// The file toys' rows are [`ToyFileFaults`]'s to check.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        match self {
            ToySubject::Toy => toy_check_at_l(raw, path).err(),
            ToySubject::Files | ToySubject::Rotating => None,
            ToySubject::Inexact => {
                path.iter().find_map(|b| match raw.get(TOY_INEXACT, b.hash.as_bytes()) {
                    Ok(Some(v)) if v.len() == 8 && v.iter().any(|&byte| byte != 0) => None,
                    row => Some(format!("inexact row at {}: {row:?}", b.height)),
                })
            }
        }
    }
    /// No height row above L: a row past `h(L) + D_max` escapes the rewind, as after a lost
    /// checkpoint (T38).
    fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        let l = path.last()?.height;
        let above = toy_rows_above(raw, l);
        (!above.is_empty())
            .then(|| format!("height rows above L = {l} after the blind rewind: {above:?}"))
    }
    /// The keyed rows' reads at the tip (the inexact toy has none).
    fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
        if *self == ToySubject::Inexact {
            return None;
        }
        let (want, _) = toy_expected(path);
        want.iter().filter(|r| r.0 == TOY_KEYED).find_map(|(_, k, v)| {
            match reader.read(TOY_KEYED, k) {
                Ok(got) if got.as_deref() == Some(&v[..]) => None,
                got => Some(format!("the read of keyed {k:?} at the tip: {got:?}")),
            }
        })
    }
}

/// What a boot found, for R11a–c.
#[derive(Debug, Default)]
pub(super) struct Boot {
    /// G, the recovered chainstate tip, and the subscriber's L as open read it.
    pub g: Option<BlockRef>,
    pub l: Option<BlockRef>,
    /// An L the block index does not know (R11a).
    pub unknown: Option<BlockHash>,
    /// R11b: blocks the catch-up needs that are not readable, its rewind's undo and its
    /// connects' data.
    pub unreadable: Vec<String>,
    pub unreadable_data: Vec<String>,
    /// The store recorded a disable since the locator was written (S11c).
    pub disabled: bool,
    /// The open's prune refusal (`index/base.cpp:161-163` at v24.0.0), which leaves the subscriber
    /// stopped.
    pub refused: Option<String>,
    /// R11c (i): the database at L after the blind rewind; (ii) the reads after the catch-up.
    pub rewound: Option<String>,
    pub caught_up: Option<String>,
}

/// The subscriber, its producer and W's hub.
pub struct Subs {
    pub cfg: SubConfig,
    pub feeds: ScriptedFeeds,
    pub hub: Arc<SubscriberHub>,
    pub index: SharedIndex,
    /// The store itself, for the checks.
    pub raw: FaultyEngine,
    db: Arc<GuardedEngine<FaultyEngine>>,
    obs: Arc<WorldObserver>,
    world: World,
    driver: Mutex<Option<Box<dyn SlotDriver>>>,
    /// The flush whose commit the subscriber is serving (step 6), between its state marks.
    step6: Mutex<Option<u64>>,
    boot: Mutex<Boot>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Subs {
    /// The subscriber's store under the chainstate's latch (one fail-stop for the node), and the
    /// producer over V's recovered chain `active`.
    pub(super) fn new(
        world: &World,
        cfg: SubConfig,
        chain: &GuardedEngine<FaultyEngine>,
        obs: Arc<WorldObserver>,
        index: SharedIndex,
        active: Vec<BlockRef>,
    ) -> Self {
        let raw = (*world.engine(SUB_STORE, cfg.subject.db())).clone();
        #[allow(unused_mut)]
        let mut db = GuardedEngine::new(
            raw.clone(),
            chain.latch().clone(),
            Arc::new(WorldSink(obs.clone())),
        );
        #[cfg(feature = "planted-bugs")]
        if let Some(bug) = chain.planted() {
            db.plant(bug);
        }
        let feeds = ScriptedFeeds::new(QUEUE, active);
        let hub = SubscriberHub::with_feeds(Arc::new(feeds.clone()), Arc::new(index.clone()));
        #[cfg(feature = "planted-bugs")]
        hub.plant(chain.planted());
        Self {
            cfg,
            feeds,
            hub: Arc::new(hub),
            index,
            raw,
            db: Arc::new(db),
            obs,
            world: world.clone(),
            driver: Mutex::new(None),
            step6: Mutex::default(),
            boot: Mutex::default(),
        }
    }

    /// Open (§2.9 item 7): the barrier, L and the state, the blind rewind, the registration; R11a
    /// and R11c (i) are judged on what it read and left.
    pub(super) fn open(&self, world: &World, g: Option<BlockRef>) -> Result<(), SubscriberError> {
        // A locator ahead of every block the index kept (R11a): the driver would refuse it (D17),
        // so it is not opened, and R11a reports it.
        let (lcf, lkey) = self.cfg.locator();
        let (dcf, dkey) = self.cfg.disabled_key();
        let read = |cf, k| world.checking(|| dashd_store::KvRead::get(&self.raw, cf, k));
        let best = read(lcf, lkey).ok().flatten().as_deref().and_then(decode_locator);
        let disabled = read(dcf, dkey).ok().flatten().is_some();
        if let Some(l) = best
            && self.index.lookup(&l).is_none()
        {
            *lock(&self.boot) = Boot {
                g,
                unknown: Some(l),
                disabled,
                ..Boot::default()
            };
            return Ok(());
        }
        let opened = self.cfg.subject.open(
            &self.world,
            Arc::clone(&self.db),
            Arc::clone(&self.hub),
            self.cfg.driver(),
        );
        let mut d = match opened {
            Err(SubscriberError::Data(m)) if m.ends_with(PRUNE_VIOLATION) => {
                self.world.mark(Mark::Log {
                    line: m.clone(),
                });
                let l = best.and_then(|hash| {
                    let id = self.index.lookup(&hash)?;
                    Some(BlockRef {
                        id,
                        hash,
                        height: self.index.height(id)?,
                    })
                });
                *lock(&self.boot) = Boot {
                    g,
                    l,
                    disabled,
                    refused: Some(m),
                    ..Boot::default()
                };
                return Ok(());
            }
            // BL-127's rule for a blk file (DEC-212): sanctioned only when a failed directory
            // sync lost the file.
            Err(SubscriberError::Data(m)) if missing_file(self.cfg.name(), &m).is_some() => {
                let lost = missing_file(self.cfg.name(), &m)
                    .and_then(|n| self.cfg.subject.file_paths().get(n))
                    .is_some_and(|p| world.lost_to_failed_dir_sync(std::path::Path::new(p)));
                return Err(SubscriberError::Data(if lost {
                    m
                } else {
                    format!("{m} (not lost to a failed directory sync)")
                }));
            }
            r => r?,
        };
        // Enabled again: its registration holds the lock (the toy deletes the record after the
        // catch-up; here the lock comes back at the open).
        if disabled {
            let mut b = self.db.batch();
            b.delete(dcf, dkey);
            self.db.write(b, Commit::Durable).map_err(|e| store_err(&self.db, &e))?;
        }
        let marks = world.clone();
        let mark = move |kind, at: &BlockRef| {
            marks.mark(Mark::Locator {
                store: SUB_STORE,
                tip: hash(&at.hash),
                kind: match kind {
                    LocatorKind::CommitStep | LocatorKind::EagerRewind => MarkKind::CommitStep,
                    LocatorKind::Checkpoint => MarkKind::Checkpoint,
                    LocatorKind::Cadence => MarkKind::Cadence,
                    LocatorKind::Rewind => MarkKind::Rewind,
                },
            });
        };
        let l = d.durable();
        // b4 n-G: the hook is set after `open`, so D45's open rewind (the one locator write an
        // open makes) is marked here, right after it returned.
        if let Some(at) = l
            && best != Some(at.hash)
        {
            mark(LocatorKind::Rewind, &at);
        }
        d.set_on_locator(Box::new(mark));
        let rewound = world.checking(|| self.rewound(l));
        *lock(&self.boot) = Boot {
            g,
            l,
            rewound,
            disabled,
            ..Boot::default()
        };
        *lock(&self.driver) = Some(d);
        Ok(())
    }

    /// `D` (S11c): the operator disables the index for the rest of the epoch. It stops (its
    /// final capped commit, then its lock and registration go), and the store records it.
    ///
    /// The record is durable before the deregistration releases the lock (D36), so no prune can
    /// take the catch-up's data from an image without it; `after` runs once the lock is gone (W's
    /// prune, `Dp h`). LateRecord writes the record after `after`.
    pub(super) fn disable(
        &self,
        after: &mut dyn FnMut() -> Result<(), Abort>,
    ) -> Result<(), Abort> {
        let Some(mut d) = lock(&self.driver).take() else {
            // Already stopped (a refused open): `Dp`'s prune still runs.
            return after();
        };
        d.finish().map_err(|e| self.fail(&e))?;
        let late = planted!(self.db.planted(), LateRecord);
        if !late {
            self.record_disabled()?;
        }
        drop(d);
        after()?;
        if late {
            self.record_disabled()?;
        }
        Ok(())
    }

    fn record_disabled(&self) -> Result<(), Abort> {
        self.world.mark(Mark::Log {
            line: self.cfg.disabled_line(),
        });
        let (dcf, dkey) = self.cfg.disabled_key();
        let mut b = self.db.batch();
        b.put(dcf, dkey, &[1]);
        self.db.write(b, Commit::Durable).map_err(|e| self.fail(&store_err(&self.db, &e)))
    }

    /// R11c (i) after the blind rewind: the database at L, and no height row above it (a row
    /// past `h(L) + D_max` escapes the rewind, as after a lost checkpoint, T38).
    fn rewound(&self, l: Option<BlockRef>) -> Option<String> {
        let path = self.index.path(&l?);
        self.cfg.subject.check_rewound(&self.raw, &path).or_else(|| self.check_at_l(&path))
    }

    /// R11c: the store and the subject's files at L, `path` being genesis to L.
    fn check_at_l(&self, path: &[BlockRef]) -> Option<String> {
        self.cfg.subject.check_at_l(&self.world, &self.raw, path)
    }

    pub(super) fn boot(&self) -> MutexGuard<'_, Boot> {
        lock(&self.boot)
    }

    /// The driver's class, once open.
    pub(super) fn synced(&self) -> bool {
        lock(&self.driver).as_ref().is_some_and(|d| d.class() == SubscriberClass::Synced)
    }

    /// One step of the driver: a Synced step, or a Syncing one. True if it moved; at a limit or
    /// idle, false. `blocked` is set when it waits for a flush (rule 3, or a Syncing block).
    ///
    /// The forward assertions' `Mark::SubscriberState`: a Synced subscriber's when it first sees
    /// a flush's commit, when it parks while serving it, and when it has served it (step 6's
    /// bounds); a Syncing one's after each step.
    ///
    /// A file subject's step that failed with a latched failure is tried again, as a host that
    /// retries instead of stopping would (Core stops: `FatalError`, `index/base.cpp:241-243`,
    /// `:404-406` at v24.0.0): the driver must refuse it as latched, so no retried append writes
    /// over bytes the failure left. An error the caller latches is not retried.
    pub(super) fn step(&self, blocked: &mut bool) -> Result<bool, SubscriberError> {
        let mut guard = lock(&self.driver);
        let Some(d) = guard.as_mut() else {
            return Ok(false);
        };
        let r = self.step_once(d, blocked);
        if let Err(e @ (SubscriberError::Fatal(_) | SubscriberError::Refused(_))) = &r
            && self.cfg.subject.has_files(&self.world)
        {
            let retried = match d.class() {
                SubscriberClass::Synced => d.step().map(drop),
                SubscriberClass::Syncing => d.sync_step().map(drop),
            };
            assert!(
                matches!(retried, Err(SubscriberError::Refused(_))),
                "{}: a retry after `{e}` was not refused: {retried:?}",
                self.cfg.name()
            );
        }
        r
    }

    fn step_once(
        &self,
        d: &mut Box<dyn SlotDriver>,
        blocked: &mut bool,
    ) -> Result<bool, SubscriberError> {
        if d.class() == SubscriberClass::Synced {
            let commit = self.hub.outstanding(d.id()).commit.map(|c| c.flush);
            let mut step6 = lock(&self.step6);
            if commit.is_some() && *step6 != commit {
                *step6 = commit;
                self.mark(&**d, false);
            }
            let moved = match d.step()? {
                SubStep::Progress => true,
                SubStep::Parked => {
                    *blocked = true;
                    false
                }
                SubStep::Idle => false,
            };
            let served = self.hub.outstanding(d.id()).commit.map(|c| c.flush) != *step6;
            if step6.is_some() && (served || *blocked) {
                self.mark(&**d, *blocked);
                *step6 = None;
            }
            Ok(moved)
        } else {
            let moved = match d.sync_step()? {
                SyncProgress::Progress | SyncProgress::Live => true,
                SyncProgress::Blocked => {
                    *blocked = true;
                    false
                }
            };
            self.mark(&**d, *blocked);
            Ok(moved)
        }
    }

    /// A Synced subscriber's state after W's op: FA3 sees a flush whose commit it never served
    /// (T35).
    pub(super) fn mark_synced(&self) {
        if let Some(d) = lock(&self.driver).as_ref()
            && d.class() == SubscriberClass::Synced
            && lock(&self.step6).is_none()
        {
            self.mark(&**d, false);
        }
    }

    fn mark(&self, d: &dyn SlotDriver, blocked: bool) {
        self.world.mark(Mark::SubscriberState {
            store: SUB_STORE,
            synced: d.class() == SubscriberClass::Synced,
            durable: d.durable().map(|l| hash(&l.hash)),
            progress: d.progress().map(|p| hash(&p.hash)),
            submitted: None,
            blocked,
        });
    }

    /// A Synced subscriber after V's tip step: it processes until at most `backlog` events are
    /// queued, or it parks at its limit.
    pub(super) fn keep_up(&self) -> Result<(), SubscriberError> {
        let SubMode::Synced {
            backlog,
        } = self.cfg.mode
        else {
            return Ok(());
        };
        let Some(id) = lock(&self.driver).as_ref().map(|d| d.id()) else {
            return Ok(());
        };
        let mut blocked = false;
        while self.synced() && self.feeds.queued(id) > backlog as usize {
            if !self.step(&mut blocked)? {
                break;
            }
        }
        Ok(())
    }

    /// A Syncing subscriber's cadence timer (`k`).
    pub(super) fn tick(&self) -> Result<(), SubscriberError> {
        let mut guard = lock(&self.driver);
        let Some(d) = guard.as_mut() else {
            return Ok(());
        };
        d.tick()?;
        self.mark(&**d, false);
        Ok(())
    }

    /// R11c (ii) after the catch-up: the database at L, and the reads at p (the journal over the
    /// DB) the path's to V's tip `tip`.
    pub(super) fn check_caught_up(&self, tip: &BlockRef) {
        let guard = lock(&self.driver);
        let Some(d) = guard.as_ref() else {
            return;
        };
        let path = self.index.path(tip);
        let mut err = d.durable().and_then(|l| self.check_at_l(&self.index.path(&l)));
        if err.is_none() {
            err = self.cfg.subject.check_tip(d.as_ref(), &path);
        }
        if err.is_none() && d.best_block() != Some(*tip) {
            err = Some(format!("best_block {:?}, V's tip {tip:?}", d.best_block()));
        }
        lock(&self.boot).caught_up = err;
    }

    /// The subscriber's failure as Core's `FatalError` (`index/base.cpp:33-36` at v24.0.0): the
    /// latch trips, so W and V stop, and the node aborts with its message, `<name>: …` (D29). A
    /// failed store write latched in the store's guard, which reports nothing (only a failed read
    /// reports, through the sink), so the abort is marked here unless one was.
    ///
    /// As the host rule has it, [`SubscriberError::Fatal`] is reported here and must find nothing
    /// reported, and [`SubscriberError::Refused`] must find its failure reported and reports
    /// nothing.
    pub(super) fn fail(&self, e: &SubscriberError) -> Abort {
        let name = self.cfg.name();
        let message = match e {
            SubscriberError::Fatal(f) => {
                if let Some(earlier) = self.obs.aborted() {
                    panic!("{name}: `{f}` latched here after `{earlier}` was reported");
                }
                format!("{name}: {}", f.0.message)
            }
            SubscriberError::Refused(f) => {
                let Some(reported) = self.obs.aborted() else {
                    panic!("{name}: refused after `{f}`, which nothing reported");
                };
                return Abort::new(reported);
            }
            e => {
                let message = format!("{name}: {e}");
                self.db.latch().trip(Failure {
                    kind: dashd_store::ErrorKind::Other,
                    site: name,
                    message: message.clone(),
                });
                message
            }
        };
        if self.obs.aborted().is_none() {
            self.obs.abort(None, &message);
        }
        Abort::new(self.obs.aborted().unwrap_or(message))
    }
}

#[cfg(test)]
mod tests {
    use dashd_store::faulty::{
        CommitKind, CrashKind, CrashWindow, EventSel, FatalCause, Fault, FaultKind, FaultOutcome,
        FaultPlan, Fixed, MarkKind, Mode, OpKind, RefExtractor, Repro, Requirement, Scenario,
        StoreSel, Sut, Trace, TraceEntry, WorldConfig, explore, forward_trace, replay,
        replay_trace,
    };
    use dashd_store::{CfProfile, CfSpec, KvEngine};

    use super::*;
    use crate::crash::stores::sanctioned_after;
    use crate::crash::{ChainConfig, ChainRef, ChainRefs, ChainSut, ops};
    use crate::subscriber::height_key;

    /// A second subject on its own store: a per-block counter, one height row per block (its
    /// hash and the count at it), the count as its state. Its name, locator and disable record
    /// all differ from the toy's.
    pub(super) struct Counter;

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(super) struct Count(u32);

    static COUNTER_DB: DbSchema = DbSchema {
        name: "counter",
        version: 1,
        cfs: &[
            CfSpec::new("meta", CfProfile::Ordered),
            CfSpec::new("height", CfProfile::Ordered),
            CfSpec::new("locator", CfProfile::Ordered),
        ],
    };
    const COUNTER_META: Cf = Cf::new(0);
    const COUNTER_HEIGHT: Cf = Cf::new(1);
    const COUNTER_LOC: Cf = Cf::new(2);
    const COUNTER_TIP: &[u8] = b"tip";
    const COUNTER_STATE: &[u8] = b"count";
    static COUNTER_ROWS: RowSchema = RowSchema {
        height_cfs: &[COUNTER_HEIGHT],
        archive_cfs: &[],
        state_keys: &[(COUNTER_META, COUNTER_STATE)],
        keyed_cfs: &[],
        inexact_cfs: &[],
        has_files: false,
        locator: (COUNTER_LOC, COUNTER_TIP),
    };

    fn counter_value(b: &BlockRef, n: u32) -> Vec<u8> {
        [b.hash.as_bytes(), &n.to_be_bytes()[..]].concat()
    }

    struct CounterData;

    impl SubscriberData for CounterData {
        type E = FaultyEngine;
        type State = Count;

        fn name(&self) -> &'static str {
            "counter"
        }
        fn needs(&self) -> Needs {
            Needs::default()
        }
        fn start_height(&self) -> u32 {
            0
        }
        fn schema(&self) -> &'static RowSchema {
            &COUNTER_ROWS
        }
        fn initial_state(&self) -> Count {
            Count::default()
        }
        fn connect(
            &self,
            b: &ConnectedBlock<'_>,
            at: &Count,
        ) -> Result<(BlockRows, Count), SubscriberError> {
            let n =
                at.0.checked_add(1)
                    .ok_or_else(|| SubscriberError::Data("the count overflows".into()))?;
            let row = Row::Put(
                COUNTER_HEIGHT,
                height_key(b.block.height, &[]),
                counter_value(&b.block, n),
            );
            Ok((
                BlockRows {
                    rows: vec![row],
                    file: None,
                },
                Count(n),
            ))
        }
        fn stage_disconnected(
            &self,
            _: &DisconnectedBlock<'_>,
            at: &Count,
            _: &DurableBelow<'_, FaultyEngine>,
        ) -> Result<(Vec<Row>, Count), SubscriberError> {
            let n = at
                .0
                .checked_sub(1)
                .ok_or_else(|| SubscriberError::Data("a disconnect below a zero count".into()))?;
            Ok((Vec::new(), Count(n)))
        }
        fn archive(&self, _: &[Row]) -> Vec<Row> {
            Vec::new()
        }
        fn encode_state(&self, s: &Count) -> Vec<Row> {
            vec![Row::Put(COUNTER_META, COUNTER_STATE.to_vec(), s.0.to_be_bytes().to_vec())]
        }
        fn decode_state(
            &self,
            rows: &[StateRead],
            _: &DurableBelow<'_, FaultyEngine>,
        ) -> Result<Count, SubscriberError> {
            match rows {
                [(_, _, Some(v))] => v
                    .as_slice()
                    .try_into()
                    .map(|v| Count(u32::from_be_bytes(v)))
                    .map_err(|_| SubscriberError::Data("a bad count".into())),
                _ => Err(SubscriberError::Data("a locator without its count".into())),
            }
        }
        fn height_ranges(&self, lo_exclusive: u32, hi_inclusive: u32) -> Vec<CfRange> {
            if lo_exclusive >= hi_inclusive {
                return Vec::new();
            }
            let end = hi_inclusive.checked_add(1).map_or(vec![0xff; 5], |h| height_key(h, &[]));
            vec![(COUNTER_HEIGHT, height_key(lo_exclusive + 1, &[]), end)]
        }
        fn max_entry_bytes(&self) -> u64 {
            1024
        }
    }

    impl CrashSubject for Counter {
        type State = Count;
        type Files = NoFiles;

        fn prefix(&self) -> &'static str {
            "counter-"
        }
        fn name(&self) -> &'static str {
            "counter"
        }
        fn locator(&self) -> (Cf, &'static [u8]) {
            COUNTER_ROWS.locator
        }
        fn db(&self) -> &'static DbSchema {
            &COUNTER_DB
        }
        fn data(
            &self,
            _: &World,
        ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = Count>>, SubscriberError>
        {
            Ok(Box::new(CounterData))
        }
        fn blocks(&self) -> Arc<dyn BlockSource> {
            Arc::new(NoBlocks)
        }
        fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
            let l = path.last()?;
            let Ok(len) = u32::try_from(path.len()) else {
                return Some(format!("a path of {} blocks", path.len()));
            };
            let want: Vec<_> = (1..)
                .zip(path)
                .map(|(n, b)| (height_key(b.height, &[]), counter_value(b, n)))
                .collect();
            let got: Result<Vec<_>, _> = raw
                .iter_prefix(COUNTER_HEIGHT, &[])
                .filter(|r| {
                    r.as_ref().map_or(true, |(k, _)| {
                        k.get(..4).is_none_or(|h| h <= &l.height.to_be_bytes()[..])
                    })
                })
                .collect();
            match got {
                Ok(got) if got == want => {}
                got => return Some(format!("height rows at L = {}: {got:?}", l.height)),
            }
            let count = raw.get(COUNTER_META, COUNTER_STATE).ok().flatten();
            if count.as_deref() != Some(&len.to_be_bytes()[..]) {
                return Some(format!("count {count:?}, want {}", path.len()));
            }
            let best = raw.get(COUNTER_LOC, COUNTER_TIP).ok().flatten();
            (best.as_deref().and_then(decode_locator) != Some(l.hash))
                .then(|| format!("locator {best:?}, want {}", l.hash))
        }
        fn check_rewound(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
            let l = path.last()?.height;
            let above: Vec<_> = raw
                .iter_prefix(COUNTER_HEIGHT, &[])
                .filter(|r| {
                    r.as_ref()
                        .map_or(true, |(k, _)| k.get(..4).is_none_or(|h| h > &l.to_be_bytes()[..]))
                })
                .collect();
            (!above.is_empty()).then(|| format!("height rows above L = {l}: {above:?}"))
        }
        fn check_tip(&self, reader: &dyn CrashReader, path: &[BlockRef]) -> Option<String> {
            let tip = path.last()?;
            let Ok(len) = u32::try_from(path.len()) else {
                return Some(format!("a path of {} blocks", path.len()));
            };
            let got = reader.read(COUNTER_HEIGHT, &height_key(tip.height, &[]));
            (got.as_ref().ok().and_then(Option::as_ref) != Some(&counter_value(tip, len)))
                .then(|| format!("the read of the tip's row: {got:?}"))
        }
    }

    /// The toy with an epoch row its data object writes Durable at each open, as txindex's `E`:
    /// a recovery fault in that write fails the data object's construction.
    pub(super) struct Epoch;

    const EPOCH: &[u8] = b"epoch";

    impl CrashSubject for Epoch {
        type State = ToyState;
        type Files = NoFiles;

        fn prefix(&self) -> &'static str {
            "epoch-"
        }
        fn name(&self) -> &'static str {
            "epochtoy"
        }
        fn locator(&self) -> (Cf, &'static [u8]) {
            (TOY_META, TOY_BEST)
        }
        fn db(&self) -> &'static DbSchema {
            &TOY_DB
        }
        fn data(
            &self,
            world: &World,
        ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = ToyState>>, SubscriberError>
        {
            let engine = world.engine(SUB_STORE, &TOY_DB);
            let mut b = engine.batch();
            b.put(TOY_META, EPOCH, &[1]);
            engine
                .write(b, Commit::Durable)
                .map_err(|e| SubscriberError::Data(format!("the epoch row: {e}")))?;
            Ok(Box::new(ToyIndex::<FaultyEngine>::mandatory()))
        }
        fn blocks(&self) -> Arc<dyn BlockSource> {
            Arc::new(NoBlocks)
        }
        fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
            toy_check_at_l(raw, path).err()
        }
    }

    #[test]
    fn index_forms_round_trip() {
        for form in [
            "synced:1:96",
            "syncing:3:7",
            "inexact-synced:0:96",
            "files-syncing:5:3",
            "rotating-synced:0:96",
            "counter-synced:0:96",
            "epoch-syncing:2:96",
        ] {
            let cfg: SubConfig = form.parse().expect(form);
            assert_eq!(cfg.to_string(), form);
        }
        assert_eq!("syncing:0:5".parse(), Ok(SubConfig::syncing(1).with_d_max(5)));
        assert_eq!("synced:2:96".parse(), Ok(SubConfig::synced(2)));
        assert_ne!(
            "counter-synced:2:96".parse::<SubConfig>(),
            Ok(SubConfig::synced(2)),
            "the subject is part of the config"
        );
        for bad in ["", "synced:1", "synced:1:0", "synced:1:2:3", "counter-:1:2", "x-synced:1:2"] {
            assert_eq!(bad.parse::<SubConfig>(), Err(()), "{bad}");
        }
    }

    /// `FromStr` takes the first prefix that matches, so no entry may shadow another.
    #[test]
    fn prefixed_subjects_are_unambiguous() {
        let prefixes: Vec<_> = PREFIXED.iter().map(|s| s.prefix()).collect();
        for (i, a) in prefixes.iter().enumerate() {
            for (j, b) in prefixes.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert_ne!(a, b, "a repeated prefix");
                assert!(a.is_empty() || !b.starts_with(a), "{a:?} shadows {b:?}");
            }
            assert!(!a.is_empty() || i + 1 == prefixes.len(), "the empty prefix is not last");
        }
    }

    /// A registered prefix may not capture a default-toy mode's `index=` form.
    #[test]
    fn a_registered_prefix_may_not_shadow_a_mode() {
        for prefix in ["s", "sync", "synced", "synced:", "syncing"] {
            assert!(shadowed_mode(prefix).is_some(), "{prefix}");
        }
        for prefix in ["rotating-", "synced-", "txindex-"] {
            assert_eq!(shadowed_mode(prefix), None, "{prefix}");
        }
    }

    /// A registered prefix may not shadow one of this module's subjects.
    #[test]
    #[should_panic(expected = "overlaps")]
    fn a_registered_prefix_may_not_overlap_another() {
        register_subject(&Counter);
    }

    /// [`Epoch`] under a prefix of its own, for the registry.
    struct Renamed(&'static str);

    impl CrashSubject for Renamed {
        type State = ToyState;
        type Files = NoFiles;

        fn prefix(&self) -> &'static str {
            self.0
        }
        fn name(&self) -> &'static str {
            CrashSubject::name(&Epoch)
        }
        fn locator(&self) -> (Cf, &'static [u8]) {
            CrashSubject::locator(&Epoch)
        }
        fn db(&self) -> &'static DbSchema {
            CrashSubject::db(&Epoch)
        }
        fn data(
            &self,
            world: &World,
        ) -> Result<Box<dyn SubscriberData<E = FaultyEngine, State = ToyState>>, SubscriberError>
        {
            CrashSubject::data(&Epoch, world)
        }
        fn blocks(&self) -> Arc<dyn BlockSource> {
            CrashSubject::blocks(&Epoch)
        }
        fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
            CrashSubject::check_at_l(&Epoch, raw, path)
        }
    }

    /// Registering a subject again does nothing, and its type under another prefix is named too.
    #[test]
    fn a_subject_registers_once_and_its_type_again_under_another_prefix() {
        static A: Renamed = Renamed("renamed-a-");
        static B: Renamed = Renamed("renamed-b-");
        register_subject(&A);
        register_subject(&A);
        register_subject(&B);
        for form in ["renamed-a-synced:0:96", "renamed-b-synced:0:96"] {
            let cfg: SubConfig = form.parse().expect(form);
            assert_eq!(cfg.to_string(), form);
        }
    }

    /// A driver whose first sync step fails latched and whose retry goes through.
    struct RetryAccepted(bool);

    impl CrashReader for RetryAccepted {
        fn read(&self, _: Cf, _: &[u8]) -> Result<Option<Vec<u8>>, SubscriberError> {
            unreachable!()
        }
    }

    impl SlotDriver for RetryAccepted {
        fn id(&self) -> SubscriberId {
            unreachable!()
        }
        fn class(&self) -> SubscriberClass {
            SubscriberClass::Syncing
        }
        fn durable(&self) -> Option<BlockRef> {
            unreachable!()
        }
        fn progress(&self) -> Option<BlockRef> {
            unreachable!()
        }
        fn best_block(&self) -> Option<BlockRef> {
            unreachable!()
        }
        fn set_on_locator(&mut self, _: OnLocator) {
            unreachable!()
        }
        fn step(&mut self) -> Result<SubStep, SubscriberError> {
            unreachable!()
        }
        fn sync_step(&mut self) -> Result<SyncProgress, SubscriberError> {
            if std::mem::replace(&mut self.0, true) {
                return Ok(SyncProgress::Progress);
            }
            Err(SubscriberError::Fatal(crate::store::Fatal(Failure {
                kind: dashd_store::ErrorKind::Io,
                site: "retry-accepted",
                message: "a failed append".into(),
            })))
        }
        fn tick(&mut self) -> Result<(), SubscriberError> {
            unreachable!()
        }
        fn finish(&mut self) -> Result<(), SubscriberError> {
            unreachable!()
        }
    }

    /// `form`'s subscriber stepping a [`RetryAccepted`] driver.
    fn step_retry_accepted(form: &str) -> Result<bool, SubscriberError> {
        let cfg: SubConfig = form.parse().expect(form);
        let world = World::new(WorldConfig::default());
        let obs = Arc::new(WorldObserver::new(world.clone()));
        let chain = GuardedEngine::new(
            (*world.engine(SUB_STORE, cfg.subject.db())).clone(),
            crate::store::GlobalFailStop::new(),
            Arc::new(WorldSink(obs.clone())),
        );
        let subs = Subs::new(&world, cfg, &chain, obs, SharedIndex::default(), Vec::new());
        *lock(&subs.driver) = Some(Box::new(RetryAccepted(false)));
        subs.step(&mut false)
    }

    /// The harness tries a file subject's latched step again and requires the retry refused.
    #[test]
    #[should_panic(expected = "was not refused")]
    fn a_file_subjects_retry_after_a_latched_failure_must_be_refused() {
        let _ = step_retry_accepted("rotating-synced:0:96");
    }

    /// A subject without files is not tried again.
    #[test]
    fn a_subject_without_files_is_not_tried_again() {
        let r = step_retry_accepted("synced:0:96");
        assert!(matches!(r, Err(SubscriberError::Fatal(_))), "{r:?}");
    }

    impl SubConfig {
        fn with_d_max(mut self, d_max: u32) -> Self {
            self.d_max = d_max;
            self
        }
    }

    fn sweep(name: &'static str, index: &str, operations: &str) {
        let index: SubConfig = index.parse().expect("subject config");
        let sc = Scenario::<ChainSut>::new(
            name,
            ChainConfig {
                index: Some(index),
                ..ChainConfig::default()
            },
            ops(operations),
        );
        sweep_scenario(&sc);
    }

    /// The exhaustive sweep of `sc`, its subscriber's file events checked; the forward trace.
    fn sweep_scenario(sc: &Scenario<ChainSut>) -> Trace {
        let name = sc.name;
        let index = sc.cfg.index.expect("a subscriber");
        assert_eq!(index.to_string().parse(), Ok(index));
        let (trace, violations) = forward_trace(sc);
        assert!(violations.is_empty(), "forward: {violations:?}");
        let file_paths = index.subject.file_paths();
        if !file_paths.is_empty() {
            let events = trace
                .entries
                .iter()
                .filter_map(|e| match e {
                    TraceEntry::Event {
                        id,
                        store,
                        op,
                        detail,
                        ..
                    } => Some((*id, store.as_str(), *op, detail.as_str())),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let is_file = |detail: &str| file_paths.contains(&detail);
            let append_then_row = events.windows(2).any(|pair| {
                pair[0].2 == OpKind::WriteAt
                    && is_file(pair[0].3)
                    && pair[1].1 == SUB_STORE
                    && pair[1].2 == OpKind::Append(CommitKind::Buffered)
            });
            assert!(append_then_row, "no append before a Buffered row");
            let archive = events
                .windows(3)
                .position(|seq| {
                    seq[0].2 == OpKind::SyncData
                        && is_file(seq[0].3)
                        && seq[1].2 == OpKind::SyncDir
                        && seq[2].1 == SUB_STORE
                        && seq[2].2 == OpKind::Append(CommitKind::Buffered)
                })
                .expect("no archive after syncing files");
            assert!(
                events[archive + 3..].iter().any(|e| e.2 == OpKind::WriteAt && is_file(e.3)),
                "no append after the archive"
            );
        }
        let report = explore(
            sc,
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        );
        eprintln!("{name}: {}", report.summary());
        assert!(report.is_clean(), "{}", report.summary());
        assert_eq!(report.truncated, 0);
        let events = trace.entries.iter().filter(|e| matches!(e, TraceEntry::Event { .. })).count();
        let covered = |kind| report.per_image.iter().filter(|(_, k, ..)| *k == kind).count();
        let points = if sc.window == CrashWindow::all() {
            events
        } else {
            let points = covered(CrashKind::ProcessKill);
            assert!((1..events).contains(&points), "the window's crash points: {points}");
            points
        };
        for kind in [CrashKind::ProcessKill, CrashKind::PowerLoss] {
            assert_eq!(covered(kind), points, "not every crash point was covered for {kind:?}");
        }
        trace
    }

    #[test]
    fn inexact_subscriber_crash_sweep() {
        sweep("inexact", "inexact-syncing:8:96", "c5;f;c5;s11;k;s2;r8:3;s20;f");
    }

    /// Behind-p commits are covered by `inexact-syncing:8:96`; the synced sweep cannot leave a
    /// strict ancestor of p queued.
    #[test]
    fn inexact_synced_subscriber_crash_sweep() {
        sweep("inexact-synced", "inexact-synced:0:96", "c2;f;c2;r2:3;f");
    }

    #[test]
    fn file_subscriber_crash_sweep() {
        sweep("files", "files-synced:0:96", "c2;f;c2;r2:3;f");
    }

    #[test]
    fn file_syncing_subscriber_crash_sweep() {
        sweep("files-syncing", "files-syncing:5:3", "c5;f;c5;s11;k;s2;r8:3;s20;f");
    }

    /// The forward trace's appends to the rotating toy's files: the file numbers in order.
    fn rotated_files(trace: &Trace) -> Vec<usize> {
        trace
            .entries
            .iter()
            .filter_map(|e| match e {
                TraceEntry::Event {
                    op: OpKind::WriteAt,
                    detail,
                    ..
                } => rotating_paths().iter().position(|p| p == detail),
                _ => None,
            })
            .collect()
    }

    /// c2-fw: the toy whose appends rotate through numbered files, live: every crash point of a
    /// run whose reorg archives rows in earlier files and appends past them into later ones,
    /// both crash kinds, including the torn appends a power loss leaves.
    #[test]
    fn rotating_file_subscriber_crash_sweep() {
        let index: SubConfig = "rotating-synced:0:96".parse().expect("rotating");
        let sc = Scenario::<ChainSut>::new(
            "rotating",
            ChainConfig {
                index: Some(index),
                ..ChainConfig::default()
            },
            ops("c2;f;c2;r2:3;f"),
        );
        let trace = sweep_scenario(&sc);
        let files = rotated_files(&trace);
        assert!(files.windows(2).any(|w| w[1] == w[0] + 1), "no rotation: {files:?}");
        assert!(files.iter().max() < Some(&(ROTATED_FILES as usize - 1)), "{files:?}");
    }

    /// c2-fw: the rotating toy in catch-up with `D_max` 3, so entries are refused at the limit
    /// and retried at the same position, across rotations, cadence commits and a reorg below L.
    #[test]
    fn rotating_file_syncing_subscriber_crash_sweep() {
        let index: SubConfig = "rotating-syncing:5:3".parse().expect("rotating");
        let sc = Scenario::<ChainSut>::new(
            "rotating-syncing",
            ChainConfig {
                index: Some(index),
                ..ChainConfig::default()
            },
            ops("c5;f;c5;s11;k;s2;r8:3;s20;f"),
        );
        let trace = sweep_scenario(&sc);
        let files = rotated_files(&trace);
        assert!(files.windows(2).any(|w| w[1] == w[0] + 1), "no rotation: {files:?}");
        assert!(files.iter().max() < Some(&(ROTATED_FILES as usize - 1)), "{files:?}");
    }

    /// c2-fw: file faults on the rotating toy, each its own forward run swept at every crash
    /// point with both kinds. Its appends: one rejected (nothing written), whose retry the driver
    /// must refuse, one torn and one applied before it failed, at the first append into a rotated
    /// file and at a later one. Its file syncs and its next file's create, at the rotation and at
    /// the commits: each aborts with the subject's own message, and recovery skips what the
    /// failed call left. Its directory syncs, at the rotation and at the commits: ignored, as
    /// Core's `DirectoryCommit` (DEC-212), so the run goes on and, under one fault per run, every
    /// crash resumes cleanly. Two failed ones can lose L's file, more an earlier one: the open
    /// refuses either (`a_subject_file_lost_to_failed_dir_syncs_refuses_the_open`,
    /// `an_earlier_subject_file_lost_to_failed_dir_syncs_refuses_the_open`).
    #[test]
    fn rotating_file_faults_fail_closed() {
        let index: SubConfig = "rotating-synced:0:96".parse().expect("rotating");
        let scenario = |name| {
            Scenario::<ChainSut>::new(
                name,
                ChainConfig {
                    index: Some(index),
                    ..ChainConfig::default()
                },
                ops("c2;f;c2;r2:3;f"),
            )
        };
        let (trace, _) = forward_trace(&scenario("rotating-faults"));
        let dir = std::path::Path::new(SUB_FILE)
            .parent()
            .expect("the files' directory")
            .to_string_lossy();
        let file = |detail: &str| rotating_paths().iter().position(|p| *p == detail);
        let events: Vec<(u64, OpKind, &str)> = trace
            .entries
            .iter()
            .filter_map(|e| match e {
                TraceEntry::Event {
                    id,
                    store,
                    op,
                    detail,
                    ..
                } if store == dashd_store::faulty::FS_STORE => {
                    Some((id.index, *op, detail.as_str()))
                }
                _ => None,
            })
            .skip_while(|(_, _, detail)| file(detail).is_none())
            .collect();
        let at = |op: OpKind, on: &dyn Fn(&str) -> bool| {
            events.iter().filter(|e| e.1 == op && on(e.2)).map(|e| e.0).collect::<Vec<_>>()
        };
        let appends: Vec<(u64, usize)> = events
            .iter()
            .filter(|e| e.1 == OpKind::WriteAt)
            .filter_map(|e| Some((e.0, file(e.2)?)))
            .collect();
        let rotated = appends.iter().position(|&(_, file)| file == 1).expect("a rotation");
        let own = |d: &str| file(d).is_some();
        let kinds: [(&str, Vec<u64>, &[FaultOutcome]); 4] = [
            (
                "write",
                vec![appends[rotated].0, appends.last().expect("appends").0],
                &[FaultOutcome::NotApplied, FaultOutcome::Partial(1), FaultOutcome::Applied],
            ),
            (
                "sync",
                at(OpKind::SyncData, &own),
                &[FaultOutcome::NotApplied, FaultOutcome::Applied],
            ),
            (
                "create",
                at(OpKind::Create, &own),
                &[FaultOutcome::NotApplied, FaultOutcome::Applied],
            ),
            (
                "dir-sync",
                at(OpKind::SyncDir, &|d| d == dir),
                &[FaultOutcome::NotApplied, FaultOutcome::Applied],
            ),
        ];
        for (kind, at, outcomes) in kinds {
            assert!(!at.is_empty(), "no {kind} event");
            let mut sc = scenario(kind);
            sc.faults = FaultPlan(
                at.iter()
                    .flat_map(|&i| {
                        outcomes.iter().map(move |&outcome| Fault {
                            at: EventSel::Index(i),
                            kind: FaultKind::Io,
                            outcome,
                        })
                    })
                    .collect(),
            );
            if kind == "dir-sync" {
                for fault in &sc.faults.0 {
                    let world = World::new(WorldConfig {
                        faults: vec![fault.clone()],
                        d1: true,
                        ..WorldConfig::default()
                    });
                    let run = ChainSut::open(&world, &sc.cfg)
                        .and_then(|mut sut| sc.ops.iter().try_for_each(|op| sut.apply(op)));
                    assert!(world.fault_fired(), "{fault:?} never fired");
                    let aborted = world.trace().marks().any(|m| matches!(m, Mark::Abort { .. }));
                    assert!(
                        run.is_ok() && !aborted,
                        "a failed directory sync stopped the run: {fault:?}"
                    );
                }
            }
            let report = explore(
                &sc,
                Mode::Exhaustive {
                    max_branches_per_image: 4096,
                },
            );
            eprintln!("rotating-faults {kind} ({} events): {}", at.len(), report.summary());
            assert!(report.is_clean(), "{kind}: {}", report.summary());
            assert_eq!(report.truncated, 0);
        }
    }

    /// Decides every pending operation on the subject's directory lost, and the rest survived.
    struct LoseSubjectDir;

    impl dashd_store::faulty::Decider for LoseSubjectDir {
        fn choose(&mut self, var: &dashd_store::faulty::Var) -> u32 {
            u32::from(var.name.contains(":fs:/dd:op")).min(var.domain - 1)
        }
    }

    /// Loses only the `n`-th of the subject's directory's pending operations a power loss decides.
    struct LoseNthSubjectDirOp(usize);

    impl dashd_store::faulty::Decider for LoseNthSubjectDirOp {
        fn choose(&mut self, var: &dashd_store::faulty::Var) -> u32 {
            if !var.name.contains(":fs:/dd:op") {
                return 0;
            }
            let lose = self.0 == 0;
            self.0 = self.0.wrapping_sub(1);
            u32::from(lose).min(var.domain - 1)
        }
    }

    /// `workload` on `cfg`, with the last `failed` syncs of the subject's directory failed (each
    /// logged, the run goes on); the world before its crash, and those syncs' `Nth`s.
    fn failing_dir_syncs(cfg: &ChainConfig, workload: &str, failed: usize) -> (World, Vec<u32>) {
        let workload = ops(workload);
        let sc = Scenario::<ChainSut>::new("lost-file", cfg.clone(), workload.clone());
        let (trace, _) = forward_trace(&sc);
        let dir = std::path::Path::new(SUB_FILE).parent().expect("the files' directory");
        let dir = dir.to_string_lossy();
        let syncs: Vec<u32> = trace
            .entries
            .iter()
            .filter_map(|e| match e {
                TraceEntry::Event {
                    store,
                    op: OpKind::SyncDir,
                    detail,
                    ..
                } if store == dashd_store::faulty::FS_STORE => Some(detail.as_str()),
                _ => None,
            })
            .enumerate()
            .filter(|(_, d)| *d == dir)
            .map(|(n, _)| u32::try_from(n).expect("few syncs"))
            .collect();
        let first = syncs.len().checked_sub(failed).expect("fewer directory syncs than faults");
        let nths = syncs[first..].to_vec();
        let world = World::new(WorldConfig {
            faults: nths
                .iter()
                .map(|&n| Fault {
                    at: EventSel::Nth {
                        store: StoreSel::Fs,
                        op: OpKind::SyncDir,
                        n,
                    },
                    kind: FaultKind::Io,
                    outcome: FaultOutcome::NotApplied,
                })
                .collect(),
            ..WorldConfig::default()
        });
        let mut sut = ChainSut::open(&world, cfg).expect("the first open");
        for op in &workload {
            sut.apply(op).expect("the forward run");
        }
        drop(sut);
        let failed_syncs = |e: &&TraceEntry| {
            matches!(e, TraceEntry::Event { op: OpKind::SyncDir, failed: true, detail, .. }
                if *detail == dir)
        };
        let n = world.trace().entries.iter().filter(failed_syncs).count();
        assert_eq!(n, failed, "the failed directory syncs");
        (world, nths)
    }

    /// `c2;f;c2;r2:3;f` on `cfg`, with the last `failed` syncs of the subject's directory failed
    /// (two: the rotation into file 3's and the last commit's, L durable over file 3), then a
    /// power loss that loses that directory's pending operations; the booted world. One fault
    /// per run cannot do it: the next directory sync commits the create.
    fn lost_to_failed_dir_syncs(cfg: &ChainConfig, failed: usize) -> World {
        let (world, _) = failing_dir_syncs(cfg, "c2;f;c2;r2:3;f", failed);
        world.capture().boot(CrashKind::PowerLoss, Box::new(LoseSubjectDir), WorldConfig::default())
    }

    /// c2-fw (BL-127's rule for the subject's files, DEC-212): a restart whose flat file at L two
    /// failed directory syncs lost is refused at the subscriber's open, with the subject's
    /// message, sanctioned with no fatal error before it. The same file lost without them (removed
    /// outright) refuses the open with a message no rule sanctions.
    #[test]
    fn a_subject_file_lost_to_failed_dir_syncs_refuses_the_open() {
        use dashd_store::flatfile::FlatFs;
        let index: SubConfig = "rotating-synced:0:96".parse().expect("rotating");
        let cfg = ChainConfig {
            index: Some(index),
            ..ChainConfig::default()
        };
        let refusal = format!("{0}: {0}{MISSING_FILE}3", index.name());
        let booted = lost_to_failed_dir_syncs(&cfg, 2);
        let a = ChainSut::open(&booted, &cfg).err().expect("the open refuses");
        assert_eq!(a.message, refusal);
        assert!(ChainSut::sanctioned(&cfg, &a.message));
        assert!(sanctioned_after(&cfg, &a.message, &FatalCause::default()));
        let file3 = std::path::Path::new(rotating_paths()[3]);
        assert!(booted.lost_to_failed_dir_sync(file3), "file 3 is lost");

        let booted = lost_to_failed_dir_syncs(&cfg, 0);
        assert!(booted.fs().remove(file3).expect("removes file 3"), "file 3 was there");
        let a = ChainSut::open(&booted, &cfg).err().expect("the open refuses");
        assert_eq!(a.message, format!("{refusal} (not lost to a failed directory sync)"));
        assert!(!ChainSut::sanctioned(&cfg, &a.message));
        assert!(!sanctioned_after(&cfg, &a.message, &FatalCause::default()));
    }

    /// c2-fw: a power loss decides each pending directory operation on its own, so it can lose an
    /// earlier file and keep L's. The open refuses, naming the first missing file, sanctioned:
    /// file 2 after five failed syncs (`c2;f;c2;r2:3;f`), and file 0, created by the toy's first
    /// write, after all four (`c3;f`).
    #[test]
    fn an_earlier_subject_file_lost_to_failed_dir_syncs_refuses_the_open() {
        use dashd_store::flatfile::FlatFs;
        let index: SubConfig = "rotating-synced:0:96".parse().expect("rotating");
        let cfg = ChainConfig {
            index: Some(index),
            ..ChainConfig::default()
        };
        let cases: [(&str, &[u32], usize); 2] =
            [("c2;f;c2;r2:3;f", &[6, 7, 8, 9, 12], 2), ("c3;f", &[0, 2, 4, 5], 0)];
        for (workload, nths, file) in cases {
            let (world, failed) = failing_dir_syncs(&cfg, workload, nths.len());
            assert_eq!(failed, nths, "{workload}: the failed syncs");
            let present = |w: &World| -> Vec<bool> {
                rotating_paths()[..=file + 1]
                    .iter()
                    .map(|p| w.fs().len(std::path::Path::new(p)).ok().flatten().is_some())
                    .collect()
            };
            let booted = (0..16)
                .map(|n| {
                    world.capture().boot(
                        CrashKind::PowerLoss,
                        Box::new(LoseNthSubjectDirOp(n)),
                        WorldConfig::default(),
                    )
                })
                .find(|b| {
                    let p = present(b);
                    p.iter().enumerate().all(|(k, &there)| there == (k != file))
                })
                .expect("a power loss that loses only that file");
            let a = match ChainSut::open(&booted, &cfg) {
                Err(a) => a,
                Ok(sut) => panic!(
                    "{workload}: opened over lost file {file}: {:?}",
                    sut.check(&booted, &ChainRef::checks_view(&cfg), &booted.trace())
                ),
            };
            assert_eq!(a.message, format!("{0}: {0}{MISSING_FILE}{file}", index.name()));
            assert!(ChainSut::sanctioned(&cfg, &a.message));
            assert!(sanctioned_after(&cfg, &a.message, &FatalCause::default()));
            let path = std::path::Path::new(rotating_paths()[file]);
            assert!(booted.lost_to_failed_dir_sync(path), "{workload}: file {file} is lost");
        }
    }

    /// c2-fw: each read of the subject's store, failed alone in the forward run, aborts once with
    /// the store's own report: the driver returns the read refused, and `fail` reports nothing
    /// more (a read returned as latched here would find that report and panic).
    #[test]
    fn a_subjects_failed_store_read_is_reported_once() {
        use crate::crash::world::DB_READ_FAILED;
        let index: SubConfig = "rotating-synced:0:96".parse().expect("rotating");
        let cfg = ChainConfig {
            index: Some(index),
            ..ChainConfig::default()
        };
        let workload = ops("c2;f;c2;r2:3;f");
        let mut n = 0;
        loop {
            let world = World::new(WorldConfig {
                faults: vec![Fault {
                    at: EventSel::Nth {
                        store: StoreSel::Engine(SUB_STORE.into()),
                        op: OpKind::Read,
                        n,
                    },
                    kind: FaultKind::Io,
                    outcome: FaultOutcome::NotApplied,
                }],
                ..WorldConfig::default()
            });
            let run = ChainSut::open(&world, &cfg)
                .and_then(|mut sut| workload.iter().try_for_each(|op| sut.apply(op)));
            if !world.fault_fired() {
                assert!(run.is_ok(), "{run:?}");
                break;
            }
            assert!(run.is_err(), "read {n}: the run went on");
            let aborts: Vec<String> = world
                .trace()
                .marks()
                .filter_map(|m| match m {
                    Mark::Abort {
                        message,
                    } => Some(message.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(aborts, [DB_READ_FAILED], "read {n}");
            n += 1;
        }
        assert!(n > 0, "no read of the subject's store");
        eprintln!("sub-reads: {n} reads, each aborted once");
    }

    /// c2-fw: a recovery fault in the epoch write the data object makes refuses the open with
    /// the subject's own abort (sanctioned: the fault is in its store), never a panic (R0), at
    /// every crash point of the run.
    #[test]
    fn a_recovery_fault_in_a_subjects_data_refuses_the_open() {
        let index: SubConfig = "epoch-synced:0:96".parse().expect("epoch");
        let mut sc = Scenario::<ChainSut>::new(
            "epoch",
            ChainConfig {
                index: Some(index),
                ..ChainConfig::default()
            },
            ops("c2;f;c2;f"),
        );
        let epoch_write = EventSel::Nth {
            store: StoreSel::Engine(SUB_STORE.into()),
            op: OpKind::Append(CommitKind::Durable),
            n: 0,
        };
        sc.recovery_faults = [FaultOutcome::NotApplied, FaultOutcome::Applied]
            .into_iter()
            .map(|outcome| Fault {
                at: epoch_write.clone(),
                kind: FaultKind::Io,
                outcome,
            })
            .collect();
        let report = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        );
        eprintln!("epoch: {}", report.summary());
        assert!(report.is_clean(), "{}", report.summary());
        let mut r = Repro::new(&sc, None);
        r.crash_at = forward_trace(&sc).0.events().last().map(|(id, ..)| id.index);
        r.kind = CrashKind::ProcessKill;
        r.recovery_fault = sc.recovery_faults.first().cloned();
        let (v, after) = replay_trace::<ChainSut>(&r).expect("replays");
        assert!(v.is_empty(), "{v:?}");
        let abort = after.marks().find_map(|m| match m {
            Mark::Abort {
                message,
            } => Some(message.clone()),
            _ => None,
        });
        assert!(
            abort.as_deref().is_some_and(|m| m.starts_with("epochtoy: the epoch row: ")
                && !m.ends_with("(not lost to a failed directory sync)")),
            "{abort:?}"
        );
    }

    /// A subject whose state is not the toy's sweeps through the same harness.
    #[test]
    fn counter_subscriber_crash_sweep() {
        sweep("counter", "counter-synced:0:96", "c2;f;c2;r2:3;f");
    }

    #[test]
    fn counter_syncing_subscriber_crash_sweep() {
        sweep("counter-syncing", "counter-syncing:5:3", "c5;f;c5;s11;k;s2;r8:3;s20;f");
    }

    /// The counter in S11c's shape: durable at 10, disabled, V to 410 and a manual prune past its
    /// locator. Every image is clean only if the disable record is the counter's own (the
    /// extractor waives the locator's requirements on it) and FA1 reads the counter's line.
    #[test]
    fn counter_disabled_crash_sweep() {
        let mut sc = Scenario::<ChainSut>::new(
            "counter-disabled",
            ChainConfig {
                prune: Some(0),
                sub_batch: Some(50_000),
                index: Some("counter-synced:0:1024".parse().expect("counter")),
                ..ChainConfig::default()
            },
            ops("c10;f;D;c400;p410"),
        );
        sc.window = CrashWindow {
            from: EventSel::AfterMark {
                mark: MarkKind::FlushBegin,
                occurrence: 1,
                n: 0,
            },
            to: None,
        };
        let trace = sweep_scenario(&sc);
        let disabled: Vec<_> = trace
            .marks()
            .filter(|m| matches!(m, Mark::Log { line } if line.contains(" disabled")))
            .collect();
        assert_eq!(disabled.len(), 1, "{disabled:?}");
        assert!(
            matches!(disabled[0], Mark::Log { line } if *line == "counter disabled"),
            "{disabled:?}"
        );
        let mut r = Repro::new(&sc, None);
        r.crash_at = trace.events().last().map(|(id, ..)| id.index);
        r.kind = CrashKind::ProcessKill;
        let (v, after) = replay_trace::<ChainSut>(&r).expect("replays");
        assert!(v.is_empty(), "{v:?}");
        let refused = after.marks().filter(|m| {
            matches!(m, Mark::Log { line } if line.starts_with("counter") && line.ends_with(PRUNE_VIOLATION))
        });
        assert_eq!(refused.count(), 1, "the counter's refusal");
    }

    /// The disable writes the counter's record at its own key, and the next open reads it and
    /// deletes it as the counter is enabled again.
    #[test]
    fn counter_disable_record_round_trip() {
        let cfg = ChainConfig {
            index: Some("counter-synced:0:96".parse().expect("counter")),
            ..ChainConfig::default()
        };
        let sub = cfg.index.expect("counter");
        let (dcf, dkey) = sub.disabled_key();
        assert_eq!((dcf, dkey), (COUNTER_LOC, SUB_DISABLED));
        let world = World::new(WorldConfig::default());
        let mut sut = ChainSut::open(&world, &cfg).expect("opens");
        for op in &ops("c10;f;D;c5;f") {
            sut.apply(op).expect("applies");
        }
        drop(sut);
        let engine = world.engine(SUB_STORE, &COUNTER_DB);
        let record = |e: &FaultyEngine| e.get(dcf, dkey).expect("reads");
        assert_eq!(record(&engine), Some(vec![1]), "the counter's record");
        assert_eq!(engine.get(TOY_META, SUB_DISABLED).expect("reads"), None, "the toy's key");
        world.crash_in_place(CrashKind::ProcessKill, Box::new(Fixed::default()));
        assert_eq!(record(&engine), Some(vec![1]), "the record is durable");
        let _sut = ChainSut::open(&world, &cfg).expect("opens again");
        assert_eq!(record(&engine), None, "the record outlived the re-enable");
    }

    /// The extractor reads the configured subject's locator and disable record, not the toy's.
    #[test]
    fn counter_locator_requirements() {
        let cfg: SubConfig = "counter-synced:0:96".parse().expect("counter");
        let refs = ChainRefs::new(Some(cfg));
        let locator = [&0i32.to_le_bytes()[..], &[1], &[7; 32]].concat();
        assert!(decode_locator(&locator).is_some());
        let world = World::new(Default::default());
        let (cf, key) = cfg.locator();
        assert_eq!((cf, key), (COUNTER_LOC, COUNTER_TIP));
        assert!(
            !refs.refs(SUB_STORE, cf, key, &locator, &world.durable_view()).is_empty(),
            "no requirements for the counter's locator"
        );
        assert!(refs.finalizes(SUB_STORE, cf, key, Some(&locator)).is_some());
        let engine = world.engine(SUB_STORE, &COUNTER_DB);
        // R11a's gate reads the counter's durable locator: it is ahead of an empty chainstate.
        let mut b = engine.batch();
        b.put(cf, key, &locator);
        engine.write(b, Commit::Durable).expect("the locator");
        let view = world.durable_view();
        let tip = refs
            .refs(SUB_STORE, cf, key, &locator, &view)
            .into_iter()
            .find(|r| matches!(r, Requirement::DurableTipAtLeast { .. }))
            .expect("the locator's tip requirement");
        let gate = refs.resolve(&tip, &view);
        assert!(gate.is_err(), "the counter's locator ahead of the chainstate: {gate:?}");
        let (dcf, dkey) = cfg.disabled_key();
        let mut b = engine.batch();
        b.put(dcf, dkey, &[1]);
        engine.write(b, Commit::Durable).expect("the disable record");
        assert!(
            refs.refs(SUB_STORE, cf, key, &locator, &world.durable_view()).is_empty(),
            "a disabled counter's locator requires nothing"
        );
    }

    /// A counter store failure aborts with the counter's name, and the abort is sanctioned.
    #[test]
    fn counter_store_failure_aborts_with_its_name() {
        let cfg = ChainConfig {
            index: Some("counter-synced:0:96".parse().expect("counter")),
            ..ChainConfig::default()
        };
        let sc = Scenario::<ChainSut>::new("counter-failure", cfg.clone(), ops("c2;f;c2;f"));
        let fault = Fault {
            at: EventSel::Nth {
                store: StoreSel::Engine(SUB_STORE.into()),
                op: OpKind::Append(CommitKind::Buffered),
                n: 0,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        };
        let world = World::new(WorldConfig {
            faults: vec![fault.clone()],
            d1: true,
            ..WorldConfig::default()
        });
        let run = ChainSut::open(&world, &cfg)
            .and_then(|mut sut| sc.ops.iter().try_for_each(|op| sut.apply(op)));
        assert!(run.is_err(), "the fault never stopped it");
        let abort = world
            .trace()
            .marks()
            .find_map(|m| match m {
                Mark::Abort {
                    message,
                } => Some(message.clone()),
                _ => None,
            })
            .expect("an abort");
        assert!(abort.starts_with("counter: "), "{abort}");
        let cause = FatalCause {
            read: false,
            store: Some(SUB_STORE.into()),
            paths: Vec::new(),
            file: None,
        };
        assert!(sanctioned_after(&cfg, &abort, &cause), "{abort}");
        let violations = replay::<ChainSut>(&Repro::new(&sc, Some(fault))).expect("replays");
        assert!(violations.is_empty(), "{violations:?}");
    }

    /// c2-fw: a fault on one of the subject's own files sanctions its abort, and V's chainstate
    /// flush when V's next write is refused first; a block or undo file's fault sanctions
    /// neither, and the subject's file sanctions nothing else.
    #[test]
    fn only_the_subjects_own_file_sanctions_its_outcomes() {
        use crate::crash::stores::{FLUSH_FAILED, OPEN_FAILED};
        use crate::crash::world::DB_READ_FAILED;
        use crate::crash::{
            BLOCKS_DIR, PATH_ACCEPT, PATH_CONNECT, PATH_DISCONNECT, PATH_FLUSH, PATH_LOAD,
        };
        let cfg = ChainConfig {
            index: Some("rotating-synced:0:96".parse().expect("rotating")),
            ..ChainConfig::default()
        };
        let on = |file: &str| FatalCause {
            read: false,
            store: Some(dashd_store::faulty::FS_STORE.into()),
            paths: vec![PATH_CONNECT],
            file: Some(file.into()),
        };
        let abort = "toyindex: toy file: Input/output error (os error 5)";
        let flush = format!("{FLUSH_FAILED}a write refused after the latch");
        let own = on(rotating_paths()[1]);
        assert!(sanctioned_after(&cfg, abort, &own));
        assert!(sanctioned_after(&cfg, &flush, &own));
        for unrelated in
            ["counter: a write failed", "Failed to write undo data", "Failed to read block"]
        {
            assert!(!sanctioned_after(&cfg, unrelated, &own), "{unrelated}");
        }
        let on_path = |read, path| FatalCause {
            read,
            paths: vec![path],
            ..own.clone()
        };
        assert!(sanctioned_after(&cfg, &flush, &on_path(false, PATH_FLUSH)));
        assert!(!sanctioned_after(&cfg, &flush, &on_path(false, PATH_ACCEPT)));
        assert!(!sanctioned_after(&cfg, &flush, &on_path(true, PATH_CONNECT)));
        for path in [PATH_CONNECT, PATH_DISCONNECT] {
            assert!(!sanctioned_after(&cfg, DB_READ_FAILED, &on_path(true, path)), "{path}");
        }
        assert!(!sanctioned_after(&cfg, OPEN_FAILED, &on_path(false, PATH_LOAD)));
        for block in [format!("{BLOCKS_DIR}/blk00000.dat"), format!("{BLOCKS_DIR}/rev00000.dat")] {
            assert!(!sanctioned_after(&cfg, abort, &on(&block)), "{block}");
            assert!(!sanctioned_after(&cfg, &flush, &on(&block)), "{block}");
        }
        let fileless = ChainConfig {
            index: Some("synced:0:96".parse().expect("toy")),
            ..ChainConfig::default()
        };
        assert!(!sanctioned_after(&fileless, abort, &on(SUB_FILE)));
    }
}
