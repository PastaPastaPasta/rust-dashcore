//! Core-compatible InstantSend rows, batches and cache transitions.

#[cfg(any(test, feature = "test-support"))]
pub mod core_model;
mod hold;
mod keys;
mod lru;
#[cfg(test)]
mod model;
#[cfg(test)]
pub(crate) mod removal_tests;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::thread::ThreadId;

use dash_consensus::Tx;
use dash_consensus::connect::has_lock_inputs;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::locks::isdlock_hash;
use dash_kv::{
    Cf, CfProfile, CfSpec, Commit, DbSchema, ErrorKind, KvBatch, KvEngine, Result, StoreError,
};
use dash_types::codec::BaseCodec;
use parking_lot::{Condvar, Mutex, MutexGuard};

use self::hold::{GateHold, GateRelease};
use self::lru::Lru;
use super::seams::{IsManagerStore, IsStore, StageId};
use super::{IsLockHash, LockedTx, lock_inputs};

/// The engine layout; all Core rows, including `is_v`, live in `is`.
pub static SCHEMA: DbSchema = DbSchema {
    name: "instantsend",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("is", CfProfile::Ordered)],
};

/// The single column family holding Core's key space.
pub const IS: Cf = Cf::new(1);
const CURRENT_VERSION: i32 = 1;

/// Core's removal sequence and the stored-root observation used by digest writers.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ChainedRemoval {
    /// Descendants in Core traversal order, with repeats, followed by the root.
    pub hashes: Vec<IsLockHash>,
    /// Whether the final bypass-cache root lookup found a valid stored lock.
    pub root_present: bool,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheSnapshot {
    pub locks: Vec<(IsLockHash, Option<LockedTx>)>,
    pub txids: Vec<(TxHash, IsLockHash)>,
    pub inputs: Vec<(OutPoint, IsLockHash)>,
}

/// The owner of the in-progress chain attempt's store view: its staged batches and the gate
/// hold they keep. Only this module interprets the fields.
#[derive(Debug, Default)]
pub struct IsOverlay {
    stages: Vec<Staged>,
    gate_owner: Option<ThreadId>,
    /// The gate hold this overlay took; dropping it unreleased latches the store.
    hold: Option<GateHold>,
}

impl IsOverlay {
    /// Whether the overlay holds the attempt gate only while it has stages (§5.3, "Gate hold is
    /// RAII"): `end_attempt`'s check.
    pub(crate) fn gate_held_only_with_stages(&self) -> bool {
        !self.stages.is_empty() || (self.gate_owner.is_none() && self.hold.is_none())
    }
}

/// One attempt-local batch.
#[derive(Debug)]
struct Staged {
    id: StageId,
    ops: Vec<OverlayOp>,
}

#[derive(Debug)]
struct OverlayOp {
    key: Vec<u8>,
    value: Option<Vec<u8>>,
}

#[derive(Default)]
struct State {
    locks: Lru<IsLockHash, Option<LockedTx>>,
    txids: Lru<TxHash, IsLockHash>,
    inputs: Lru<OutPoint, IsLockHash>,
    best_confirmed_height: i32,
}

#[derive(Default)]
struct GateState {
    owner: Option<ThreadId>,
    waiters: usize,
    next_stage_id: u64,
    latched: Option<(ErrorKind, Cow<'static, str>)>,
}

/// The attempt gate: its state under one mutex, and the condvar its waiters park on.
#[derive(Default)]
struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}

/// The latch text of a gate hold dropped before its stages were committed or discarded.
const UNRELEASED_HOLD: &str = "InstantSend attempt ended with its stages pending";

/// A hold on [`Gate`] taken by `owner`'s first stage of an attempt.
struct HeldGate {
    gate: Arc<Gate>,
    owner: ThreadId,
}

impl HeldGate {
    fn end(&self, latch: bool) {
        let mut gate = self.gate.state.lock();
        if latch && gate.latched.is_none() {
            gate.latched = Some((ErrorKind::Other, Cow::Borrowed(UNRELEASED_HOLD)));
        }
        if gate.owner == Some(self.owner) {
            gate.owner = None;
        }
        self.gate.cv.notify_all();
    }
}

impl GateRelease for HeldGate {
    fn release(&self) {
        self.end(false);
    }

    fn latch_and_release(&self) {
        self.end(true);
    }
}

/// Serializes DB operations and their three caches under Core's single DB lock.
/// The caller opens a `RocksEngine` with [`SCHEMA`] in production.
pub struct IsDb<E> {
    engine: Arc<E>,
    state: Mutex<State>,
    gate: Arc<Gate>,
}

trait ReadView {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;
    fn iter_prefix_from(&self, prefix: &[u8], start: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>>;
}

struct EngineView<'a, E>(&'a E);

impl<E: KvEngine> ReadView for EngineView<'_, E> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.0.get(IS, key)
    }

    fn iter_prefix_from(&self, prefix: &[u8], start: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        self.0.iter_prefix_from(IS, prefix, start).collect()
    }
}

struct OverlayView<'a, E> {
    engine: &'a E,
    overlay: &'a IsOverlay,
}

#[derive(Default)]
struct OverlayBatch {
    ops: Vec<OverlayOp>,
}

impl KvBatch for OverlayBatch {
    fn put(&mut self, cf: Cf, key: &[u8], value: &[u8]) {
        debug_assert_eq!(cf, IS);
        self.ops.push(OverlayOp {
            key: key.to_vec(),
            value: Some(value.to_vec()),
        });
    }

    fn delete(&mut self, cf: Cf, key: &[u8]) {
        debug_assert_eq!(cf, IS);
        self.ops.push(OverlayOp {
            key: key.to_vec(),
            value: None,
        });
    }

    fn delete_range(&mut self, _cf: Cf, _start: &[u8], _end: &[u8]) {
        panic!("InstantSend overlay does not support range deletes");
    }

    fn len(&self) -> usize {
        self.ops.len()
    }

    fn size_bytes(&self) -> usize {
        self.ops.iter().map(|op| op.key.len() + op.value.as_ref().map_or(0, Vec::len)).sum()
    }

    fn clear(&mut self) {
        self.ops.clear();
    }
}

impl<E: KvEngine> ReadView for OverlayView<'_, E> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        for stage in self.overlay.stages.iter().rev() {
            for op in stage.ops.iter().rev() {
                if op.key == key {
                    return Ok(op.value.clone());
                }
            }
        }
        self.engine.get(IS, key)
    }

    fn iter_prefix_from(&self, prefix: &[u8], start: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut rows: BTreeMap<Vec<u8>, Vec<u8>> = self
            .engine
            .iter_prefix_from(IS, prefix, start)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .collect();
        for stage in &self.overlay.stages {
            for op in &stage.ops {
                if op.key.starts_with(prefix) && op.key.as_slice() >= start {
                    if let Some(value) = &op.value {
                        rows.insert(op.key.clone(), value.clone());
                    } else {
                        rows.remove(&op.key);
                    }
                }
            }
        }
        Ok(rows.into_iter().collect())
    }
}

fn engine_error(kind: ErrorKind, message: impl Into<String>) -> StoreError {
    StoreError::engine("dashd-locks", kind, message)
}

impl<E: KvEngine> IsDb<E> {
    fn latched_error(gate: &GateState) -> Option<StoreError> {
        gate.latched.as_ref().map(|(kind, message)| engine_error(*kind, message.to_string()))
    }

    fn latch(&self, error: &StoreError) {
        let mut gate = self.gate.state.lock();
        if gate.latched.is_none() {
            let kind = error.kind().unwrap_or(ErrorKind::Other);
            gate.latched = Some((kind, error.to_string().into()));
        }
        self.gate.cv.notify_all();
    }

    /// Takes `State` and then checks the gate under it, so no caller can pass the check before a
    /// stage and run after it. A caller that must wait drops `State` first.
    fn enter(&self) -> Result<MutexGuard<'_, State>> {
        self.enter_as(None)
    }

    /// [`Self::enter`] for an owner read: an overlay that holds the gate on this thread passes.
    fn enter_as(&self, overlay: Option<&IsOverlay>) -> Result<MutexGuard<'_, State>> {
        let current = std::thread::current().id();
        let owns = overlay.is_some_and(|overlay| overlay.gate_owner == Some(current));
        loop {
            let state = self.state.lock();
            let mut gate = self.gate.state.lock();
            if let Some(error) = Self::latched_error(&gate) {
                return Err(error);
            }
            match gate.owner {
                None => return Ok(state),
                Some(owner) if owner == current && owns => return Ok(state),
                Some(owner) if owner == current => {
                    let error = engine_error(
                        ErrorKind::Other,
                        "InstantSend overlay owner used a non-overlay operation",
                    );
                    if gate.latched.is_none() {
                        gate.latched = Some((ErrorKind::Other, error.to_string().into()));
                    }
                    gate.owner = None;
                    self.gate.cv.notify_all();
                    return Err(error);
                }
                Some(_) => {
                    drop(state);
                    gate.waiters += 1;
                    self.gate.cv.wait(&mut gate);
                    gate.waiters -= 1;
                }
            }
        }
    }

    fn begin_stage(&self, overlay: &mut IsOverlay) -> Result<MutexGuard<'_, State>> {
        let current = std::thread::current().id();
        let state = self.state.lock();
        let mut gate = self.gate.state.lock();
        if let Some(error) = Self::latched_error(&gate) {
            return Err(error);
        }
        match gate.owner {
            Some(owner) if owner != current => {
                return Err(engine_error(
                    ErrorKind::Other,
                    "InstantSend stage attempted by another thread",
                ));
            }
            Some(_) => {}
            None => gate.owner = Some(current),
        }
        let took = overlay.gate_owner.is_none();
        overlay.gate_owner = Some(current);
        drop(gate);
        // The hold is built only after the gate mutex is released (`hold.rs`).
        if took && overlay.hold.is_none() {
            let held = HeldGate {
                gate: self.gate.clone(),
                owner: current,
            };
            overlay.hold = Some(GateHold::new(Arc::new(held)));
        }
        Ok(state)
    }

    fn release_gate(&self, overlay: &mut IsOverlay) {
        let hold = overlay.hold.take();
        overlay.gate_owner = None;
        let current = std::thread::current().id();
        {
            let mut gate = self.gate.state.lock();
            if gate.owner == Some(current) {
                gate.owner = None;
                self.gate.cv.notify_all();
            }
        }
        if let Some(hold) = hold {
            hold.release();
        }
    }

    fn next_stage_id(&self) -> StageId {
        let mut gate = self.gate.state.lock();
        let id = StageId(gate.next_stage_id);
        gate.next_stage_id += 1;
        id
    }

    /// Number of callers currently waiting behind the attempt gate.
    #[cfg(any(test, feature = "test-support"))]
    pub fn gate_waiters(&self) -> usize {
        self.gate.state.lock().waiters
    }

    /// A test-support snapshot of cache contents in recency order.
    #[cfg(any(test, feature = "test-support"))]
    pub fn cache_snapshot(&self) -> CacheSnapshot {
        let state = self.state.lock();
        fn ordered<K: Clone + Eq + std::hash::Hash, V: Clone>(cache: &Lru<K, V>) -> Vec<(K, V)> {
            let mut entries: Vec<_> =
                cache.entries().iter().map(|(k, (v, n))| (k.clone(), v.clone(), *n)).collect();
            entries.sort_by_key(|(_, _, n)| *n);
            entries.into_iter().map(|(k, v, _)| (k, v)).collect()
        }
        CacheSnapshot {
            locks: ordered(&state.locks),
            txids: ordered(&state.txids),
            inputs: ordered(&state.inputs),
        }
    }

    /// Checks the lock version, wiping missing/older versions and refusing newer ones.
    pub fn open(engine: Arc<E>) -> Result<Self> {
        Self::open_wiping(engine, false)
    }

    /// [`open`](Self::open), first emptying the store when `wipe` is set: Core's `CDBWrapper`
    /// wipe of `llmq/isdb` under `-reindex` or `-reindex-chainstate`, which destroys the database
    /// before the version check, so a newer version is wiped rather than refused.
    // Core: instantsend/db.cpp:29-33; init.cpp:2033
    pub fn open_wiping(engine: Arc<E>, wipe: bool) -> Result<Self> {
        if engine.schema() != &SCHEMA {
            return Err(StoreError::UnknownDatabase {
                db: SCHEMA.name,
                what: "InstantSend requires its own engine schema".into(),
            });
        }
        let version = engine
            .get(IS, keys::VERSION)?
            .and_then(|v| v.get(..4).and_then(|v| v.try_into().ok()).map(i32::from_le_bytes));
        if !wipe
            && let Some(version) = version
            && version > CURRENT_VERSION
        {
            return Err(StoreError::SchemaTooNew {
                db: SCHEMA.name,
                found: version as u32,
                supported: CURRENT_VERSION as u32,
            });
        }
        if wipe || version.is_none_or(|v| v < CURRENT_VERSION) {
            let mut batch = engine.batch();
            for row in engine.iter_prefix_uncached(IS, &[]) {
                batch.delete(IS, &row?.0);
            }
            batch.put(IS, keys::VERSION, &CURRENT_VERSION.to_le_bytes());
            engine.write(batch, Commit::Durable)?;
        }
        Ok(Self {
            engine,
            state: Mutex::new(State::default()),
            gate: Arc::default(),
        })
    }

    /// Writes a lock and all its indexes in one buffered batch, then inserts the caches.
    pub fn write_new(&self, hash: &IsLockHash, lock: &LockedTx) -> Result<()> {
        let mut state = self.enter()?;
        let mut batch = self.engine.batch();
        let mut value = Vec::new();
        lock.encode(&mut value);
        batch.put(IS, &keys::hash(keys::LOCK, hash), &value);
        batch.put(IS, &keys::hash(keys::TX, lock.txid().as_bytes()), hash);
        for input in lock.inputs() {
            batch.put(IS, &keys::input(input), hash);
        }
        self.engine.write(batch, Commit::Buffered)?;
        state.locks.insert(*hash, Some(lock.clone()));
        state.txids.insert(lock.txid(), *hash);
        for input in lock.inputs() {
            state.inputs.insert(*input, *hash);
        }
        Ok(())
    }

    /// Marks a lock mined, independently of whether a lock row exists.
    pub fn write_mined(&self, hash: &IsLockHash, height: i32) -> Result<()> {
        let _state = self.enter()?;
        let mut batch = self.engine.batch();
        batch.put(IS, &keys::height(keys::MINED, height, hash), &[1]);
        self.engine.write(batch, Commit::Buffered)
    }

    /// Writes mined marks for transactions with lock inputs, using the txid cache.
    pub fn block_connected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        self.block_mined(transactions, height, true)
    }

    /// Erases mined marks for transactions with lock inputs, using the txid cache.
    pub fn block_disconnected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        self.block_mined(transactions, height, false)
    }

    fn block_mined(&self, transactions: &[Tx], height: i32, connected: bool) -> Result<()> {
        let mut state = self.enter()?;
        let mut batch = self.engine.batch();
        for tx in transactions.iter().filter(|tx| has_lock_inputs(tx)) {
            let hash = state.hash_by_txid(&EngineView(&*self.engine), &tx.txid())?;
            if hash != [0; 32] {
                let key = keys::height(keys::MINED, height, &hash);
                if connected {
                    batch.put(IS, &key, &[1]);
                } else {
                    batch.delete(IS, &key);
                }
            }
        }
        self.engine.write(batch, Commit::Buffered)
    }

    /// Removes mined locks through `height`, retaining caches and archiving at mined height.
    pub fn remove_confirmed(&self, height: i32) -> Result<BTreeMap<IsLockHash, LockedTx>> {
        let mut state = self.enter()?;
        if height <= state.best_confirmed_height {
            return Ok(BTreeMap::new());
        }
        state.best_confirmed_height = height;
        let rows = self.height_rows(&EngineView(&*self.engine), keys::MINED, height)?;
        let mut batch = self.engine.batch();
        let mut removed = BTreeMap::new();
        for (key, mined_height, hash) in rows {
            if let Some(lock) = state.by_hash(&EngineView(&*self.engine), &hash, false)? {
                state.remove(&mut batch, &hash, &lock, true);
                removed.entry(hash).or_insert(lock);
            }
            archive(&mut batch, &hash, mined_height as i32);
            batch.delete(IS, &key);
        }
        self.engine.write(batch, Commit::Buffered)?;
        Ok(removed)
    }

    /// Prunes both archive indexes through a positive cutoff height.
    pub fn remove_archived(&self, height: i32) -> Result<()> {
        let _state = self.enter()?;
        if height <= 0 {
            return Ok(());
        }
        let rows = self.height_rows(&EngineView(&*self.engine), keys::ARCHIVE_HEIGHT, height)?;
        let mut batch = self.engine.batch();
        for (key, _, hash) in rows {
            batch.delete(IS, &keys::hash(keys::ARCHIVE_HASH, &hash));
            batch.delete(IS, &key);
        }
        self.engine.write(batch, Commit::Buffered)
    }

    /// Removes descendants from pre-batch DB reads, then archives the root even if absent.
    ///
    /// `root` is the lock object the caller found for `hash`, as Core passes `*islock`. A stored
    /// root is removed as Core removes it, erasing its cache entries by key. When the root is no
    /// longer stored, DEC-1631 clears the root lock's own cached entries: its islock entry, and
    /// its txid and outpoint entries only while they still name `hash`, so an entry another lock
    /// wrote over them keeps answering for that lock.
    pub fn remove_chained(
        &self,
        hash: &IsLockHash,
        root: &LockedTx,
        height: i32,
    ) -> Result<ChainedRemoval> {
        let mut overlay = IsOverlay::default();
        let (removal, stage) = self.stage_chained(&mut overlay, hash, root, height)?;
        self.commit_staged(&mut overlay, stage)?;
        Ok(removal)
    }

    fn stage_remove_chained(
        &self,
        overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)> {
        self.stage_chained(overlay, hash, root, archive_h)
    }

    /// Core's `RemoveChainedInstantSendLocks` body against the overlay, including the bypass
    /// re-read of the root after the child walk (`instantsend/db.cpp:379`).
    fn stage_chained(
        &self,
        overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)> {
        let mut state = self.begin_stage(overlay)?;
        let view = OverlayView {
            engine: &*self.engine,
            overlay: &*overlay,
        };
        let result = (|| {
            let mut batch = OverlayBatch::default();
            let mut hashes = Vec::new();
            let mut stack = vec![root.txid()];
            let mut added = HashSet::new();
            while let Some(parent) = stack.pop() {
                for child in self.children(&view, &parent)? {
                    let Some(lock) = state.by_hash(&view, &child, false)? else {
                        continue;
                    };
                    state.remove(&mut batch, &child, &lock, false);
                    archive(&mut batch, &child, archive_h);
                    hashes.push(child);
                    if added.insert(lock.txid()) {
                        stack.push(lock.txid());
                    }
                }
            }
            let stored = state.by_hash(&view, hash, false)?;
            match &stored {
                Some(lock) => state.remove(&mut batch, hash, lock, false),
                // DEC-1631: the root lock's own cached entries are cleared when it is not stored too.
                None => state.erase_root_entries(hash, root),
            }
            archive(&mut batch, hash, archive_h);
            hashes.push(*hash);
            Ok((
                ChainedRemoval {
                    hashes,
                    root_present: stored.is_some(),
                },
                batch.ops,
            ))
        })();
        let (removal, ops) = match result {
            Ok(value) => value,
            Err(error) => {
                // Core keeps the partial cache transitions and latches nothing; an earlier
                // stage keeps the gate it holds.
                drop(state);
                if overlay.stages.is_empty() {
                    self.release_gate(overlay);
                }
                return Err(error);
            }
        };
        let id = self.next_stage_id();
        overlay.stages.push(Staged {
            id,
            ops,
        });
        drop(state);
        Ok((removal, id))
    }

    fn commit_staged(&self, overlay: &mut IsOverlay, stage: StageId) -> Result<()> {
        let current = std::thread::current().id();
        {
            let gate = self.gate.state.lock();
            if let Some(error) = Self::latched_error(&gate) {
                return Err(error);
            }
            if gate.owner != Some(current) || overlay.gate_owner != Some(current) {
                return Err(engine_error(
                    ErrorKind::Other,
                    "InstantSend stage is not owned by this thread",
                ));
            }
        }
        let Some(first) = overlay.stages.first() else {
            return Err(engine_error(ErrorKind::Other, "InstantSend stage does not exist"));
        };
        if first.id != stage {
            return Err(engine_error(ErrorKind::Other, "InstantSend stages must commit in order"));
        }
        let mut batch = self.engine.batch();
        for op in &first.ops {
            match &op.value {
                Some(value) => batch.put(IS, &op.key, value),
                None => batch.delete(IS, &op.key),
            }
        }
        if let Err(error) = self.engine.write(batch, Commit::Buffered) {
            self.latch(&error);
            overlay.stages.clear();
            self.release_gate(overlay);
            return Err(error);
        }
        overlay.stages.remove(0);
        if overlay.stages.is_empty() {
            self.release_gate(overlay);
        }
        Ok(())
    }

    fn discard_staged(&self, overlay: &mut IsOverlay) {
        if overlay.stages.is_empty() {
            self.release_gate(overlay);
            return;
        }
        overlay.stages.clear();
        let error = engine_error(ErrorKind::Other, "InstantSend staged removal discarded");
        self.latch(&error);
        self.release_gate(overlay);
    }

    /// Reads a lock; bypass-cache reads still populate the hash cache, including misses.
    pub fn by_hash(&self, hash: &IsLockHash, use_cache: bool) -> Result<Option<LockedTx>> {
        self.enter()?.by_hash(&EngineView(&*self.engine), hash, use_cache)
    }

    /// Reads a txid's lock hash, returning the null hash on a missing index.
    pub fn hash_by_txid(&self, txid: &TxHash) -> Result<IsLockHash> {
        self.enter()?.hash_by_txid(&EngineView(&*self.engine), txid)
    }

    /// Reads a lock through its cache-visible transaction index.
    pub fn by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>> {
        let mut state = self.enter()?;
        let view = EngineView(&*self.engine);
        let hash = state.hash_by_txid(&view, txid)?;
        state.by_hash(&view, &hash, true)
    }

    /// Reads a lock through its cache-visible input index.
    pub fn by_input(&self, input: &OutPoint) -> Result<Option<LockedTx>> {
        let mut state = self.enter()?;
        state.by_input(&EngineView(&*self.engine), input)
    }

    /// Whether a cached/live lock or an archive marker is known.
    pub fn known(&self, hash: &IsLockHash) -> Result<bool> {
        let mut state = self.enter()?;
        let view = EngineView(&*self.engine);
        Ok(state.by_hash(&view, hash, true)?.is_some()
            || view.get(&keys::hash(keys::ARCHIVE_HASH, hash))?.is_some())
    }

    /// Counts stored lock rows without consulting caches.
    pub fn count(&self) -> Result<usize> {
        let _state = self.enter()?;
        let mut count = 0;
        let start = keys::hash(keys::LOCK, &[0; 32]);
        for row in self.engine.iter_prefix_from(IS, keys::LOCK, &start) {
            if keys::read_hash(&row?.0[keys::LOCK.len()..]).is_none() {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Reads child hashes in serialized outpoint byte order, including duplicates.
    pub fn by_parent(&self, parent: &TxHash) -> Result<Vec<IsLockHash>> {
        let _state = self.enter()?;
        self.children(&EngineView(&*self.engine), parent)
    }

    fn children<R: ReadView>(&self, view: &R, parent: &TxHash) -> Result<Vec<IsLockHash>> {
        let prefix = keys::hash(keys::INPUT, parent.as_bytes());
        let start = keys::input(&OutPoint::new(*parent, 0));
        let mut children = Vec::new();
        for (key, value) in view.iter_prefix_from(&prefix, &start)? {
            if key.len() < prefix.len() + 4 {
                break;
            }
            let Some(hash) = keys::read_hash(&value) else {
                break;
            };
            children.push(hash);
        }
        Ok(children)
    }

    fn height_rows<R: ReadView>(
        &self,
        view: &R,
        prefix: &[u8],
        height: i32,
    ) -> Result<Vec<HeightRow>> {
        let start = keys::height(prefix, height, &[0; 32]);
        let mut rows = Vec::new();
        for (key, _) in view.iter_prefix_from(prefix, &start)? {
            let Some((mined_height, hash)) = keys::read_height(prefix, &key) else {
                break;
            };
            if mined_height > height as u32 {
                break;
            }
            rows.push((key, mined_height, hash));
        }
        Ok(rows)
    }
}

impl<E: KvEngine> IsManagerStore for IsDb<E> {
    fn hash_by_txid(&self, txid: &TxHash) -> Result<IsLockHash> {
        IsDb::hash_by_txid(self, txid)
    }

    fn count(&self) -> Result<usize> {
        IsDb::count(self)
    }
}

impl<E: KvEngine> IsStore for IsDb<E> {
    fn conflicting_lock(&self, tx: &Tx) -> Result<Option<(IsLockHash, LockedTx)>> {
        for input in lock_inputs(tx) {
            let Some(lock) = self.by_input(&input)? else {
                continue;
            };
            if lock.txid() != tx.txid() {
                return Ok(Some((isdlock_hash(&lock), lock)));
            }
        }
        Ok(None)
    }

    fn conflicting_lock_in(
        &self,
        overlay: &IsOverlay,
        tx: &Tx,
    ) -> Result<Option<(IsLockHash, LockedTx)>> {
        let mut state = self.enter_as(Some(overlay))?;
        let view = OverlayView {
            engine: &*self.engine,
            overlay,
        };
        for input in lock_inputs(tx) {
            let Some(lock) = state.by_input(&view, &input)? else {
                continue;
            };
            if lock.txid() != tx.txid() {
                return Ok(Some((isdlock_hash(&lock), lock)));
            }
        }
        Ok(None)
    }

    fn by_hash(&self, hash: &IsLockHash, use_cache: bool) -> Result<Option<LockedTx>> {
        IsDb::by_hash(self, hash, use_cache)
    }

    fn by_txid(&self, txid: &TxHash) -> Result<Option<LockedTx>> {
        IsDb::by_txid(self, txid)
    }

    fn by_input(&self, input: &OutPoint) -> Result<Option<LockedTx>> {
        IsDb::by_input(self, input)
    }

    fn known(&self, hash: &IsLockHash) -> Result<bool> {
        IsDb::known(self, hash)
    }

    fn write_new(&self, hash: &IsLockHash, lock: &LockedTx) -> Result<()> {
        IsDb::write_new(self, hash, lock)
    }

    fn write_mined(&self, hash: &IsLockHash, height: i32) -> Result<()> {
        IsDb::write_mined(self, hash, height)
    }

    fn block_connected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        IsDb::block_connected(self, transactions, height)
    }

    fn block_disconnected(&self, transactions: &[Tx], height: i32) -> Result<()> {
        IsDb::block_disconnected(self, transactions, height)
    }

    fn remove_confirmed(&self, height: i32) -> Result<BTreeMap<IsLockHash, LockedTx>> {
        IsDb::remove_confirmed(self, height)
    }

    fn remove_archived(&self, height: i32) -> Result<()> {
        IsDb::remove_archived(self, height)
    }

    fn stage_remove_chained(
        &self,
        overlay: &mut IsOverlay,
        hash: &IsLockHash,
        root: &LockedTx,
        archive_h: i32,
    ) -> Result<(ChainedRemoval, StageId)> {
        IsDb::stage_remove_chained(self, overlay, hash, root, archive_h)
    }

    fn commit_staged(&self, overlay: &mut IsOverlay, stage: StageId) -> Result<()> {
        IsDb::commit_staged(self, overlay, stage)
    }

    fn discard_staged(&self, overlay: &mut IsOverlay) {
        IsDb::discard_staged(self, overlay)
    }
}

type HeightRow = (Vec<u8>, u32, IsLockHash);

impl State {
    fn by_input<R: ReadView>(&mut self, view: &R, input: &OutPoint) -> Result<Option<LockedTx>> {
        let hash = if let Some(hash) = self.inputs.get(input) {
            hash
        } else {
            let Some(hash) = read_hash(view, &keys::input(input))? else {
                return Ok(None);
            };
            self.inputs.insert(*input, hash);
            hash
        };
        self.by_hash(view, &hash, true)
    }

    fn by_hash<R: ReadView>(
        &mut self,
        view: &R,
        hash: &IsLockHash,
        use_cache: bool,
    ) -> Result<Option<LockedTx>> {
        if *hash == [0; 32] {
            return Ok(None);
        }
        if use_cache && let Some(lock) = self.locks.get(hash) {
            return Ok(lock);
        }
        let lock = view
            .get(&keys::hash(keys::LOCK, hash))?
            .and_then(|v| LockedTx::decode(&mut v.as_slice()).ok())
            .filter(|lock| isdlock_hash(lock) == *hash);
        self.locks.insert(*hash, lock.clone());
        Ok(lock)
    }

    fn hash_by_txid<R: ReadView>(&mut self, view: &R, txid: &TxHash) -> Result<IsLockHash> {
        if let Some(hash) = self.txids.get(txid) {
            return Ok(hash);
        }
        let Some(hash) = read_hash(view, &keys::hash(keys::TX, txid.as_bytes()))? else {
            return Ok([0; 32]);
        };
        self.txids.insert(*txid, hash);
        Ok(hash)
    }

    fn remove<B: KvBatch>(
        &mut self,
        batch: &mut B,
        hash: &IsLockHash,
        lock: &LockedTx,
        keep_cache: bool,
    ) {
        batch.delete(IS, &keys::hash(keys::LOCK, hash));
        batch.delete(IS, &keys::hash(keys::TX, lock.txid().as_bytes()));
        for input in lock.inputs() {
            batch.delete(IS, &keys::input(input));
        }
        if !keep_cache {
            self.locks.erase(hash);
            self.txids.erase(&lock.txid());
            for input in lock.inputs() {
                self.inputs.erase(input);
            }
        }
    }

    fn erase_root_entries(&mut self, hash: &IsLockHash, root: &LockedTx) {
        self.locks.erase(hash);
        self.txids.erase_if_maps_to(&root.txid(), hash);
        for input in root.inputs() {
            self.inputs.erase_if_maps_to(input, hash);
        }
    }
}

fn read_hash<R: ReadView>(view: &R, key: &[u8]) -> Result<Option<IsLockHash>> {
    Ok(view.get(key)?.and_then(|value| keys::read_hash(&value)))
}

fn archive<B: KvBatch>(batch: &mut B, hash: &IsLockHash, height: i32) {
    batch.put(IS, &keys::height(keys::ARCHIVE_HEIGHT, height, hash), &[1]);
    batch.put(IS, &keys::hash(keys::ARCHIVE_HASH, hash), &[1]);
}
