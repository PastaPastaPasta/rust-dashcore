//! `MemHeaderStore`, the in-memory SPV store (SB5 §3.7; SB4's test double, erratum D35): the §7
//! tables as encoded rows, the streamed step commit, durable and buffered layers, power loss and
//! fault injection. SB13's SQLite store passes the same suites.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound::{Excluded, Unbounded};
use std::sync::Arc;

use dash_consensus::BlockId;
use dash_primitives::BlockHash;
use parking_lot::{Mutex, MutexGuard};

// Root paths only: `benches/feeds.rs` includes this file, the crate's names imported at its root.
// PF0 builds the base's benches against the head's library, so a path the bench lacks fails it.
use crate::{
    ActiveDiff, AnchorState, ChainMeta, Durability, HeaderRecord, HeaderRow, HeaderStepSink,
    HeaderStore, StepRow, StepTail, StoreError, Work,
};

/// A read [`Faults::fail_read`] can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReadKind {
    ByHash,
    ById,
    ActiveAt,
    FromHeight,
    Leaves,
    Children,
    ChainMeta,
    ActiveFrom,
    WalkDown,
    MaxId,
}

/// How a read hands back a corrupt record ([`Faults::corrupt`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corrupt {
    /// `skip` names the record itself, which is never at its skip height.
    BadSkip,
    /// `skip` names this id.
    Skip(BlockId),
    /// `prev_hash` names no stored record, so `walk_down` stops after it.
    MissingParent,
    /// `prev_hash` names this hash: a record at another height makes a parent link that does
    /// not step down.
    PrevHash(BlockHash),
    /// The record claims this height and this parent, consistent with each other, so only its
    /// child's step down to it is off its height.
    Reparent(u32, BlockHash),
    /// The record comes back under this id.
    Id(BlockId),
    /// The record comes back with this hash.
    Hash(BlockHash),
    /// Reads skip the record, as if it were never stored.
    Gone,
}

/// The `prev_hash` [`Corrupt::MissingParent`] hands back.
const MISSING_PARENT: [u8; 32] = [0xee; 32];

fn err(msg: impl Into<String>) -> StoreError {
    StoreError::Io(msg.into())
}

/// Counts one call down; true for the call that fails.
fn due(left: &mut u64) -> bool {
    let fire = *left == 0;
    *left = left.saturating_sub(1);
    fire
}

/// Injected faults, shared by every handle and kept across `power_loss`. Each failure fires once.
#[derive(Clone, Debug, Default)]
pub struct Faults {
    /// Reads of each kind still to pass before one fails.
    reads: BTreeMap<ReadKind, u64>,
    /// `rows` batches still to pass before one fails.
    rows: Option<u64>,
    /// Commits still to pass before one fails.
    commit: Option<u64>,
    corrupt: BTreeMap<BlockId, Corrupt>,
    /// Reads of each kind so far, failed ones included.
    seen: BTreeMap<ReadKind, u64>,
    /// Pages of `children_from` and `leaves_from` hold at most this many, as a store that drops
    /// rows from a page returns a short page before its end.
    page_cap: Option<usize>,
}

impl Faults {
    /// Every later page of children or leaves holds at most `n` (at least 1).
    pub fn short_pages(&mut self, n: usize) {
        assert!(n >= 1, "a page of 0 is the end");
        self.page_cap = Some(n);
    }

    /// The `n`-th read of `kind` from now (1 = the next) fails.
    pub fn fail_read(&mut self, kind: ReadKind, n: u64) {
        assert!(n >= 1, "the first read is n = 1");
        self.reads.insert(kind, n - 1);
    }

    /// The `n`-th `rows` batch from now (1 = the next) fails, as SQLite's UNIQUE or I/O errors
    /// do mid-stream. The step is abandoned.
    pub fn fail_rows(&mut self, n: u64) {
        assert!(n >= 1, "the first batch is n = 1");
        self.rows = Some(n - 1);
    }

    /// The `n`-th commit from now (1 = the next) fails, before it changes anything. Every
    /// transaction counts: `commit_step`, `reset`, `establish`, and a `sync_wal` with a Buffered
    /// commit to cover (one with none is no transaction).
    pub fn fail_commit(&mut self, n: u64) {
        assert!(n >= 1, "the first commit is n = 1");
        self.commit = Some(n - 1);
    }

    /// Every later read of `id` hands back a corrupt record.
    pub fn corrupt(&mut self, id: BlockId, how: Corrupt) {
        self.corrupt.insert(id, how);
    }

    /// The reads of `kind` so far, failed ones included: the cold cache's hits and bypasses
    /// show as reads not made.
    pub fn reads(&self, kind: ReadKind) -> u64 {
        self.seen.get(&kind).copied().unwrap_or(0)
    }

    fn read(&mut self, kind: ReadKind) -> Result<(), StoreError> {
        *self.seen.entry(kind).or_default() += 1;
        if let Some(left) = self.reads.get_mut(&kind)
            && due(left)
        {
            self.reads.remove(&kind);
            return Err(err(format!("injected {kind:?} read failure")));
        }
        Ok(())
    }

    fn once(slot: &mut Option<u64>, what: &str) -> Result<(), StoreError> {
        if let Some(left) = slot
            && due(left)
        {
            *slot = None;
            return Err(err(format!("injected {what} failure")));
        }
        Ok(())
    }

    fn apply(&self, mut rec: HeaderRecord) -> Option<HeaderRecord> {
        match self.corrupt.get(&rec.id) {
            Some(Corrupt::BadSkip) => rec.skip = Some(rec.id),
            Some(&Corrupt::Skip(id)) => rec.skip = Some(id),
            Some(Corrupt::MissingParent) => rec.header.prev_hash = BlockHash::from(MISSING_PARENT),
            Some(&Corrupt::PrevHash(h)) => rec.header.prev_hash = h,
            Some(&Corrupt::Reparent(height, h)) => (rec.height, rec.header.prev_hash) = (height, h),
            Some(&Corrupt::Id(id)) => rec.id = id,
            Some(&Corrupt::Hash(h)) => rec.hash = h,
            Some(Corrupt::Gone) => return None,
            None => {}
        }
        Some(rec)
    }
}

/// The §7 tables. Rows are stored encoded, so every read decodes through `from_row` (P3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tables {
    meta: Option<ChainMeta>,
    /// `hdr` by id (`hdr_id`).
    hdr: BTreeMap<BlockId, HeaderRow>,
    /// `hdr`'s primary key.
    by_hash: BTreeMap<[u8; 32], BlockId>,
    /// `hdr_h`.
    by_height: BTreeSet<(u32, BlockId)>,
    /// `hdr_prev`.
    by_prev: BTreeSet<([u8; 32], BlockId)>,
    hdr_leaf: BTreeSet<BlockId>,
    active: BTreeMap<u32, BlockId>,
}

/// What reverses one applied change.
enum Undo {
    Insert(BlockId),
    Status(BlockId, u32),
    LeafAdd(BlockId),
    LeafRemove(BlockId),
    Active(u32, Option<BlockId>),
    Meta(ChainMeta),
}

fn prev_of(row: &HeaderRow) -> [u8; 32] {
    row.header[4..36].try_into().expect("32 bytes")
}

fn decode(row: &HeaderRow) -> Result<HeaderRecord, StoreError> {
    HeaderRecord::from_row(row).map_err(|e| StoreError::Corrupt(format!("hdr row {}: {e}", row.id)))
}

impl Tables {
    fn records(
        &self,
        ids: impl IntoIterator<Item = BlockId>,
    ) -> Result<Vec<HeaderRecord>, StoreError> {
        ids.into_iter().map(|id| decode(&self.hdr[&id])).collect()
    }

    fn row(&self, id: BlockId) -> Result<&HeaderRow, StoreError> {
        self.hdr.get(&id).ok_or_else(|| err(format!("no hdr row for {id:?}")))
    }

    /// One step: its rows in order, then the tail (§7's transaction).
    fn step(
        &mut self,
        rows: &[StepRow],
        tail: &StepTail,
        undo: &mut Vec<Undo>,
    ) -> Result<(), StoreError> {
        for row in rows {
            self.apply(row, undo)?;
        }
        if let Some(d) = tail.active {
            self.apply_active(d, undo)?;
        }
        self.update_meta(undo, |m| {
            m.cl = tail.cl.unwrap_or(m.cl);
            m.next_id = tail.meta.next_id;
            m.tip = tail.meta.tip;
            m.best_header = tail.meta.best_header;
        })?;
        match self.active.last_key_value() {
            Some((_, &top)) if top != tail.meta.tip => {
                Err(err(format!("meta.tip {:?} is not the active top {top:?}", tail.meta.tip)))
            }
            _ => Ok(()),
        }
    }

    fn apply(&mut self, row: &StepRow, undo: &mut Vec<Undo>) -> Result<(), StoreError> {
        match row {
            StepRow::Record(rec) => {
                let row = rec.to_row();
                if self.by_hash.contains_key(&row.hash) || self.hdr.contains_key(&rec.id) {
                    return Err(err(format!("UNIQUE constraint failed inserting {:?}", rec.id)));
                }
                self.by_hash.insert(row.hash, rec.id);
                self.by_height.insert((row.height, rec.id));
                self.by_prev.insert((prev_of(&row), rec.id));
                self.hdr.insert(rec.id, row);
                undo.push(Undo::Insert(rec.id));
            }
            &StepRow::Status(id, status) => {
                let row = self
                    .hdr
                    .get_mut(&id)
                    .ok_or_else(|| err(format!("status for unknown {id:?}")))?;
                undo.push(Undo::Status(id, row.status));
                row.status = status.bits();
            }
            &StepRow::LeafAdd(id) => {
                if self.row(id)?.kind != 0 {
                    return Err(err(format!("leaf_add of context record {id:?}")));
                }
                if !self.hdr_leaf.insert(id) {
                    return Err(err(format!("UNIQUE constraint failed: hdr_leaf {id:?}")));
                }
                undo.push(Undo::LeafAdd(id));
            }
            &StepRow::LeafRemove(id) => {
                if self.hdr_leaf.remove(&id) {
                    undo.push(Undo::LeafRemove(id));
                }
            }
        }
        Ok(())
    }

    /// §3.7's `ActiveDiff`, checked: the fork is active at or below `old_height`, and the walk
    /// from `new_tip` passes accepted records one height apart down to it. The first diff (nothing
    /// active yet) has `old_height` 0 and walks down to the lowest accepted record, whose parent is
    /// a context record or none.
    fn apply_active(&mut self, d: ActiveDiff, undo: &mut Vec<Undo>) -> Result<(), StoreError> {
        let fork = d.from_height.checked_sub(1);
        let first = match self.active.last_key_value() {
            Some((&top, _)) if top != d.old_height => {
                return Err(err(format!("active ends at {top}, not old_height {}", d.old_height)));
            }
            Some(_) if !fork.is_some_and(|f| self.active.contains_key(&f)) => {
                return Err(err(format!("fork height {fork:?} is not active")));
            }
            Some(_) => false,
            None if d.old_height != 0 => {
                return Err(err(format!("nothing active, but old_height {}", d.old_height)));
            }
            None => true,
        };
        if d.from_height > d.new_height.saturating_add(1) {
            return Err(err(format!(
                "from height {} above new height {} + 1",
                d.from_height, d.new_height
            )));
        }
        let above: Vec<u32> =
            self.active.range((Excluded(d.new_height), Unbounded)).map(|(&h, _)| h).collect();
        for h in above {
            self.set_active(h, None, undo);
        }
        // `id` names the record at height `h`, and after the walk the one at the fork height.
        let mut id = Some(d.new_tip);
        for h in (d.from_height..=d.new_height).rev() {
            let at = id.ok_or_else(|| err(format!("active walk: no stored record at {h}")))?;
            let row = self.row(at)?;
            if row.kind != 0 || row.height != h {
                return Err(err(format!(
                    "active walk: {at:?} (kind {}, height {}) at height {h}",
                    row.kind, row.height
                )));
            }
            id = self.by_hash.get(&prev_of(row)).copied();
            self.set_active(h, Some(at), undo);
        }
        if let Some(&at_fork) = fork.and_then(|f| self.active.get(&f))
            && id != Some(at_fork)
        {
            return Err(err(format!("active walk reached {id:?}, not the fork {at_fork:?}")));
        }
        if first
            && let Some(below) = id
            && self.row(below)?.kind == 0
        {
            return Err(err(format!("the first active walk stops above accepted {below:?}")));
        }
        Ok(())
    }

    fn put_active(&mut self, h: u32, id: Option<BlockId>) -> Option<BlockId> {
        match id {
            Some(id) => self.active.insert(h, id),
            None => self.active.remove(&h),
        }
    }

    fn set_active(&mut self, h: u32, id: Option<BlockId>, undo: &mut Vec<Undo>) {
        let old = self.put_active(h, id);
        if old != id {
            undo.push(Undo::Active(h, old));
        }
    }

    fn update_meta(
        &mut self,
        undo: &mut Vec<Undo>,
        f: impl FnOnce(&mut ChainMeta),
    ) -> Result<(), StoreError> {
        let meta = self.meta.as_mut().ok_or_else(|| err("no meta row"))?;
        undo.push(Undo::Meta(*meta));
        f(meta);
        Ok(())
    }

    fn rollback(&mut self, undo: Vec<Undo>) {
        for u in undo.into_iter().rev() {
            match u {
                Undo::Insert(id) => {
                    let row = self.hdr.remove(&id).expect("inserted");
                    self.by_hash.remove(&row.hash);
                    self.by_height.remove(&(row.height, id));
                    self.by_prev.remove(&(prev_of(&row), id));
                }
                Undo::Status(id, s) => self.hdr.get_mut(&id).expect("updated").status = s,
                Undo::LeafAdd(id) => {
                    self.hdr_leaf.remove(&id);
                }
                Undo::LeafRemove(id) => {
                    self.hdr_leaf.insert(id);
                }
                Undo::Active(h, old) => {
                    self.put_active(h, old);
                }
                Undo::Meta(m) => self.meta = Some(m),
            }
        }
    }
}

#[derive(Default)]
struct Db {
    /// Bumped by each `power_loss`; a handle from an earlier generation panics.
    generation: u64,
    /// What a reader sees: every committed transaction.
    current: Tables,
    /// What survives power loss.
    durable: Tables,
    /// The Buffered steps committed since the last Durable commit, oldest first.
    suffix: Vec<(Vec<StepRow>, StepTail)>,
    faults: Faults,
}

impl Db {
    /// Up to `out.len()` of `ids`' records as `f` maps them, after the read faults: a page
    /// (`HeaderStore::leaves_from`, `children_from`), filled without allocating.
    fn page<T>(
        &self,
        ids: impl Iterator<Item = BlockId>,
        out: &mut [T],
        f: impl Fn(&HeaderRecord) -> T,
    ) -> Result<usize, StoreError> {
        let cap = self.faults.page_cap.unwrap_or(usize::MAX).min(out.len());
        let out = &mut out[..cap];
        let mut n = 0;
        for id in ids {
            let Some(slot) = out.get_mut(n) else {
                break;
            };
            if let Some(rec) = self.faults.apply(decode(&self.current.hdr[&id])?) {
                *slot = f(&rec);
                n += 1;
            }
        }
        Ok(n)
    }

    /// One transaction: all of `f` or, on its error, nothing.
    fn transact(
        &mut self,
        durability: Durability,
        f: impl FnOnce(&mut Tables, &mut Vec<Undo>) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        Faults::once(&mut self.faults.commit, "commit")?;
        let mut undo = Vec::new();
        if let Err(e) = f(&mut self.current, &mut undo) {
            self.current.rollback(undo);
            return Err(e);
        }
        if durability == Durability::Durable {
            self.durable = self.current.clone();
            self.suffix.clear();
        }
        Ok(())
    }
}

/// The in-memory SPV store (SB5 §3.7). A handle: clones share the tables, and each holds its
/// own open step. A step's rows are invisible until `commit_step`.
///
/// `power_loss` invalidates **every** handle (any later use panics), so a test reopens with a
/// fresh `HeaderChain`, `ChainReader` and cache from the handle it returns (Sol r1).
#[derive(Default)]
pub struct MemHeaderStore {
    db: Arc<Mutex<Db>>,
    generation: u64,
    step: Option<Vec<StepRow>>,
}

impl Clone for MemHeaderStore {
    /// A handle on the same tables, with no open step.
    fn clone(&self) -> Self {
        self.assert_live();
        Self::handle(&self.db, self.generation)
    }
}

impl std::fmt::Debug for MemHeaderStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemHeaderStore")
            .field("generation", &self.generation)
            .field("step_rows", &self.step.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

impl MemHeaderStore {
    /// An empty store: no `meta` row (`open` returns `Empty`).
    pub fn new() -> MemHeaderStore {
        MemHeaderStore::default()
    }

    fn handle(db: &Arc<Mutex<Db>>, generation: u64) -> Self {
        MemHeaderStore {
            db: Arc::clone(db),
            generation,
            step: None,
        }
    }

    fn db(&self) -> MutexGuard<'_, Db> {
        let db = self.db.lock();
        assert_eq!(db.generation, self.generation, "a MemHeaderStore handle outlived power_loss");
        db
    }

    fn assert_live(&self) {
        drop(self.db());
    }

    /// Runs `f` on the shared faults.
    pub fn faults<R>(&self, f: impl FnOnce(&mut Faults) -> R) -> R {
        f(&mut self.db().faults)
    }

    /// The committed tables, for byte-identity checks (SB4 A-33).
    pub fn image(&self) -> Tables {
        self.db().current.clone()
    }

    /// The Buffered steps a power loss would lose: `power_loss_keeping`'s bound.
    pub fn buffered_steps(&self) -> usize {
        self.db().suffix.len()
    }

    /// Starts a step transaction; a step still open is discarded (its transaction rolled back).
    pub fn begin_step(&mut self) {
        self.assert_live();
        self.step = Some(Vec::new());
    }

    /// Commits the open step: its rows in order, then `tail` (§7's transaction). All or nothing:
    /// on an error nothing changes and the step is discarded.
    ///
    /// # Panics
    ///
    /// With no open step, including after a failed `rows`.
    pub fn commit_step(
        &mut self,
        tail: &StepTail,
        durability: Durability,
    ) -> Result<(), StoreError> {
        let rows = self.step.take().expect("commit_step without an open step");
        let mut db = self.db();
        db.transact(durability, |t, undo| t.step(&rows, tail, undo))?;
        if durability == Durability::Buffered {
            db.suffix.push((rows, *tail));
        }
        Ok(())
    }

    /// SB0 §9.4's `sync_wal`: a Durable commit of `meta.sync_seq += 1` if a Buffered commit is
    /// newer than the last Durable one, else a no-op.
    pub fn sync_wal(&self) -> Result<(), StoreError> {
        let mut db = self.db();
        if db.suffix.is_empty() {
            return Ok(());
        }
        db.transact(Durability::Durable, |t, undo| t.update_meta(undo, |m| m.sync_seq += 1))
    }

    /// Clears the header tables and writes `meta`, in one Durable transaction: the initial
    /// candidate, and a Candidate fallback (SB0 §9.5).
    pub fn reset(&self, meta: ChainMeta) -> Result<(), StoreError> {
        self.db().transact(Durability::Durable, |t, _| {
            *t = Tables {
                meta: Some(meta),
                ..Tables::default()
            };
            Ok(())
        })
    }

    /// Sets `meta.anchor_state = Established` in one Durable transaction (SB0 §4.3.1).
    pub fn establish(&self) -> Result<(), StoreError> {
        self.db().transact(Durability::Durable, |t, undo| {
            t.update_meta(undo, |m| m.anchor_state = AnchorState::Established)
        })
    }

    /// Power loss: every Buffered step since the last Durable commit is lost, and every handle,
    /// this one included, is invalidated. Returns the handle to reopen with. Faults are kept.
    pub fn power_loss(&self) -> MemHeaderStore {
        self.power_loss_keeping(0)
    }

    /// Power loss that keeps the first `keep` of the Buffered steps since the last Durable
    /// commit (SB0 §9.4: a suffix is lost), so a test can reopen at any committed cut. What
    /// survived is durable from then on.
    ///
    /// # Panics
    ///
    /// If `keep` exceeds [`buffered_steps`](Self::buffered_steps).
    pub fn power_loss_keeping(&self, keep: usize) -> MemHeaderStore {
        let mut db = self.db();
        assert!(keep <= db.suffix.len(), "only {} Buffered steps to keep", db.suffix.len());
        let mut t = db.durable.clone();
        for (rows, tail) in std::mem::take(&mut db.suffix).iter().take(keep) {
            t.step(rows, tail, &mut Vec::new()).expect("a committed step replays");
        }
        db.durable = t.clone();
        db.current = t;
        db.generation += 1;
        Self::handle(&self.db, db.generation)
    }

    fn read<R>(&self, kind: ReadKind, f: impl FnOnce(&Tables) -> R) -> Result<R, StoreError> {
        let mut db = self.db();
        db.faults.read(kind)?;
        Ok(f(&db.current))
    }

    /// Every leaf's record, in id order: [`HeaderStore::leaves_from`] all at once, for tests.
    pub fn leaves(&self) -> Result<Vec<HeaderRecord>, StoreError> {
        self.read_records(ReadKind::Leaves, |t| t.records(t.hdr_leaf.iter().copied()))
    }

    /// Every child's record, in id order: [`HeaderStore::children_from`] all at once, for tests.
    pub fn children(&self, prev: &BlockHash) -> Result<Vec<HeaderRecord>, StoreError> {
        let key = *prev.as_bytes();
        self.read_records(ReadKind::Children, |t| {
            let ids = t.by_prev.range((key, BlockId(0))..=(key, BlockId(u32::MAX)));
            t.records(ids.map(|&(_, id)| id))
        })
    }

    fn read_records(
        &self,
        kind: ReadKind,
        f: impl FnOnce(&Tables) -> Result<Vec<HeaderRecord>, StoreError>,
    ) -> Result<Vec<HeaderRecord>, StoreError> {
        let mut db = self.db();
        db.faults.read(kind)?;
        let recs = f(&db.current)?;
        Ok(recs.into_iter().filter_map(|r| db.faults.apply(r)).collect())
    }
}

impl HeaderStepSink for MemHeaderStore {
    /// # Panics
    ///
    /// With no open step.
    fn rows(&mut self, batch: &[StepRow]) -> Result<(), StoreError> {
        assert!(self.step.is_some(), "rows without an open step");
        let failed = Faults::once(&mut self.db().faults.rows, "rows");
        match failed {
            Ok(()) => self.step.as_mut().expect("open").extend_from_slice(batch),
            Err(_) => self.step = None,
        }
        failed
    }
}

impl HeaderStore for MemHeaderStore {
    fn by_hash(&self, h: &BlockHash) -> Result<Option<HeaderRecord>, StoreError> {
        let id = |t: &Tables| t.by_hash.get(h.as_bytes()).copied();
        Ok(self.read_records(ReadKind::ByHash, |t| t.records(id(t)))?.pop())
    }

    fn by_id(&self, id: BlockId) -> Result<Option<HeaderRecord>, StoreError> {
        let id = |t: &Tables| t.hdr.contains_key(&id).then_some(id);
        Ok(self.read_records(ReadKind::ById, |t| t.records(id(t)))?.pop())
    }

    fn active_at(&self, height: u32) -> Result<Option<BlockId>, StoreError> {
        self.read(ReadKind::ActiveAt, |t| t.active.get(&height).copied())
    }

    /// In `(height, id)` order.
    fn from_height(&self, h: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        self.read_records(ReadKind::FromHeight, |t| {
            t.records(t.by_height.range((h, BlockId(0))..).map(|&(_, id)| id))
        })
    }

    fn leaves_from(&self, from: BlockId, out: &mut [(BlockId, Work)]) -> Result<usize, StoreError> {
        let mut db = self.db();
        db.faults.read(ReadKind::Leaves)?;
        db.page(db.current.hdr_leaf.range(from..).copied(), out, |r| (r.id, r.chain_work))
    }

    fn children_from(
        &self,
        prev: &BlockHash,
        from: BlockId,
        out: &mut [BlockId],
    ) -> Result<usize, StoreError> {
        let key = *prev.as_bytes();
        let mut db = self.db();
        db.faults.read(ReadKind::Children)?;
        let ids = db.current.by_prev.range((key, from)..=(key, BlockId(u32::MAX)));
        db.page(ids.map(|&(_, id)| id), out, |r| r.id)
    }

    fn chain_meta(&self) -> Result<Option<ChainMeta>, StoreError> {
        self.read(ReadKind::ChainMeta, |t| t.meta)
    }

    fn active_from(&self, lo: u32) -> Result<Vec<(u32, BlockId)>, StoreError> {
        self.read(ReadKind::ActiveFrom, |t| t.active.range(lo..).map(|(&h, &id)| (h, id)).collect())
    }

    fn walk_down(&self, from: BlockId, n: u32) -> Result<Vec<HeaderRecord>, StoreError> {
        let mut db = self.db();
        db.faults.read(ReadKind::WalkDown)?;
        let mut out = Vec::new();
        let mut next = db.current.hdr.contains_key(&from).then_some(from);
        while let Some(id) = next.filter(|_| out.len() < n as usize) {
            let Some(rec) = db.faults.apply(decode(&db.current.hdr[&id])?) else {
                break;
            };
            next = db.current.by_hash.get(rec.header.prev_hash.as_bytes()).copied();
            out.push(rec);
        }
        Ok(out)
    }

    fn max_id(&self) -> Result<Option<BlockId>, StoreError> {
        self.read(ReadKind::MaxId, |t| t.hdr.last_key_value().map(|(&id, _)| id))
    }
}

// The path resolves the same from `benches/feeds.rs`, which includes this file.
#[cfg(test)]
#[path = "mem_store/tests.rs"]
mod tests;
