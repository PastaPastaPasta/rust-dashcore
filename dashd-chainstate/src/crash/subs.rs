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
//! `inexact-` for the append-count toy, or `files-` for staged file writes (for example
//! `index=files-synced:0:96`). A subject supplies its data object, block source, checks at L and
//! flat files ([`CrashSubject`]); the boot, steps, disable and checks below serve any subject.
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

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

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
    FileAppend, LocatorKind, NoBlocks, PRUNE_VIOLATION, Row, RowSchema, ScriptedFeeds, StateRead,
    SubStep, SubscriberClass, SubscriberData, SubscriberDriver, SubscriberError, SubscriberId,
    SyncProgress, TOY_BEST, TOY_DB, TOY_INEXACT, TOY_KEYED, TOY_META, ToyFiles, ToyIndex,
    ToyInexact, ToyState, decode_locator, store_err, toy_check_at_l, toy_expected, toy_rows_above,
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

    /// `subject` in `mode`; its `index=` form is the subject's prefix before the mode's. A new
    /// subject is parsed only once it is registered in `PREFIXED` as well.
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

    /// Whether `message` is the subscriber's own abort (D29).
    pub(super) fn is_abort(&self, message: &str) -> bool {
        message.strip_prefix(self.name()).is_some_and(|m| m.starts_with(": "))
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

/// The subjects `index=` names by a prefix; a mode without one is [`ToySubject::Toy`]'s. Adding
/// a subject includes registering it here: [`FromStr`] takes the first prefix that matches, so
/// the prefixes are distinct, none is a prefix of another, and an empty one comes last.
static PREFIXED: &[&dyn Slot] = &[
    &ToySubject::Inexact,
    &ToySubject::Files,
    #[cfg(test)]
    &tests::Counter,
];

impl FromStr for SubConfig {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (subject, s) = PREFIXED
            .iter()
            .find_map(|&sub| Some((sub, s.strip_prefix(sub.prefix())?)))
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
    fn data(&self, world: &World)
    -> Box<dyn SubscriberData<E = FaultyEngine, State = Self::State>>;
    /// The block data its catch-up reads.
    fn blocks(&self) -> Arc<dyn BlockSource>;
    /// Its flat files on `world`, if it has any.
    fn files(&self, _world: &World) -> Option<Self::Files> {
        None
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
    /// The files' paths, as the world's file events name them.
    fn paths(&self) -> &'static [&'static str];
    /// R11c on the files at L (lengths, torn appends) and the rows that reference them; the
    /// first mismatch.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String>;
}

/// A subject without flat files.
pub enum NoFiles {}

impl SubjectFiles for NoFiles {
    fn paths(&self) -> &'static [&'static str] {
        match *self {}
    }

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
    fn with_file_floor(&self, at: &T, len: u64) -> T {
        self.0.with_file_floor(at, len)
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
    fn file_len(&self) -> Result<u64, SubscriberError> {
        self.0.file_len()
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
    #[cfg(test)]
    fn file_paths(&self, world: &World) -> &'static [&'static str];
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
        let data = self.data(world);
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
        let data = SubjectData(data, CrashSubject::name(self));
        let d = SubscriberDriver::open(data, db, hub, self.blocks(), cfg)?;
        Ok(Box::new(d))
    }
    #[cfg(test)]
    fn file_paths(&self, world: &World) -> &'static [&'static str] {
        self.files(world).map_or(&[], |f| f.paths())
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
/// append-count toy (`inexact-`) and the toy with staged file writes (`files-`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToySubject {
    Toy,
    Inexact,
    Files,
}

const SUB_FILE: &str = "/dd/subscriber.dat";

/// The toy's staged-write file.
struct ToyFileFaults(ToyFiles<FaultyEngine>);

impl SubjectFiles for ToyFileFaults {
    fn paths(&self) -> &'static [&'static str] {
        &[SUB_FILE]
    }

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
    fn data(&self, world: &World) -> Box<dyn SubscriberData<E = FaultyEngine, State = ToyState>> {
        match self {
            ToySubject::Toy => Box::new(ToyIndex::<FaultyEngine>::mandatory()),
            ToySubject::Inexact => Box::new(ToyInexact::<FaultyEngine>::default()),
            ToySubject::Files => Box::new(self.files(world).expect("the file subject").0),
        }
    }
    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(NoBlocks)
    }
    fn files(&self, world: &World) -> Option<ToyFileFaults> {
        (*self == ToySubject::Files)
            .then(|| ToyFileFaults(ToyFiles::new(world.fs(), PathBuf::from(SUB_FILE))))
    }
    /// The file toy's rows are [`ToyFileFaults`]'s to check.
    fn check_at_l(&self, raw: &FaultyEngine, path: &[BlockRef]) -> Option<String> {
        match self {
            ToySubject::Toy => toy_check_at_l(raw, path).err(),
            ToySubject::Files => None,
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
    pub(super) fn step(&self, blocked: &mut bool) -> Result<bool, SubscriberError> {
        let mut guard = lock(&self.driver);
        let Some(d) = guard.as_mut() else {
            return Ok(false);
        };
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
    pub(super) fn fail(&self, e: &SubscriberError) -> Abort {
        let name = self.cfg.name();
        let message = match e {
            SubscriberError::Fatal(f) => format!("{name}: {}", f.0.message),
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
        Fixed, MarkKind, Mode, OpKind, RefExtractor, Repro, Requirement, Scenario, StoreSel, Sut,
        Trace, TraceEntry, WorldConfig, explore, forward_trace, replay, replay_trace,
    };
    use dashd_store::{CfProfile, CfSpec, KvEngine};

    use super::*;
    use crate::crash::stores::sanctioned_after;
    use crate::crash::{ChainConfig, ChainRefs, ChainSut, ops};
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
        fn data(&self, _: &World) -> Box<dyn SubscriberData<E = FaultyEngine, State = Count>> {
            Box::new(CounterData)
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

    #[test]
    fn index_forms_round_trip() {
        for form in [
            "synced:1:96",
            "syncing:3:7",
            "inexact-synced:0:96",
            "files-syncing:5:3",
            "counter-synced:0:96",
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
        let file_paths = index.subject.file_paths(&World::new(Default::default()));
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
        };
        assert!(sanctioned_after(&cfg, &abort, &cause), "{abort}");
        let violations = replay::<ChainSut>(&Repro::new(&sc, Some(fault))).expect("replays");
        assert!(violations.is_empty(), "{violations:?}");
    }
}
