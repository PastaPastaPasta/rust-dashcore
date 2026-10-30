//! The in-memory engine: one `BTreeMap` per column family (unit tests, `kv_model`).
//!
//! It models the durability contract of [`KvEngine`]: buffered batches are kept apart until a
//! barrier, so [`MemoryEngine::simulate_power_loss`] can drop any suffix of them. It also
//! follows the fail-stop contract: [`MemoryEngine::inject_failure`] fails the next write or
//! barrier, after which every write is refused, as on RocksDB, until a restart
//! ([`MemoryEngine::restart`] or a power loss).

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use crate::engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
use crate::error::{ErrorKind, Result, StoreError};
use crate::fail_stop::{Failure, failure_of, refusal};
use crate::schema::{DbSchema, SchemaState, check_schema};

pub(crate) type Map = BTreeMap<Vec<u8>, Vec<u8>>;

/// One operation of a batch (also the model's, which applies it independently).
#[derive(Clone, Debug)]
pub(crate) enum BatchOp {
    Put(Cf, Vec<u8>, Vec<u8>),
    Delete(Cf, Vec<u8>),
    /// Keys in `[start, end)`, with `start < end`.
    DeleteRange(Cf, Vec<u8>, Vec<u8>),
}

pub(crate) type Ops = Vec<BatchOp>;

struct State {
    /// What readers see: every committed batch.
    live: Vec<Arc<Map>>,
    /// What survives a power loss: the state at the last barrier.
    durable: Vec<Arc<Map>>,
    /// Batches committed buffered since the last barrier, in commit order.
    buffered: Vec<Ops>,
    /// Set by [`MemoryEngine::inject_failure`]: the next write or barrier fails with it.
    injected: Option<ErrorKind>,
    /// The fail-stop latch, under the same lock as the data, so a write either precedes the
    /// failure or is refused.
    failed: Option<Failure>,
}

/// An in-memory [`KvEngine`]. Snapshots share the maps copy-on-write, so taking one is O(#CF).
pub struct MemoryEngine {
    schema: &'static DbSchema,
    state: RwLock<State>,
}

const ENGINE: &str = "memory";

pub(crate) fn apply(maps: &mut [Arc<Map>], ops: &Ops) {
    for op in ops {
        match op {
            BatchOp::Put(cf, key, value) => {
                Arc::make_mut(&mut maps[cf.index()]).insert(key.clone(), value.clone());
            }
            BatchOp::Delete(cf, key) => {
                Arc::make_mut(&mut maps[cf.index()]).remove(key);
            }
            BatchOp::DeleteRange(cf, start, end) => {
                let map = Arc::make_mut(&mut maps[cf.index()]);
                // Split off `[start, ..)`, keep its part from `end` on.
                let mut tail = map.split_off(start);
                map.append(&mut tail.split_off(end));
            }
        }
    }
}

impl MemoryEngine {
    /// A new, empty database with the schema version stamped.
    pub fn new(schema: &'static DbSchema) -> Self {
        let engine = Self::with_maps(schema, vec![Arc::default(); schema.cfs.len()]);
        let state = check_schema(&engine).expect("a fresh in-memory database accepts its schema");
        debug_assert_eq!(state, SchemaState::Created);
        engine
    }

    /// The database `maps` hold (one map per column family), all of it durable.
    fn with_maps(schema: &'static DbSchema, maps: Vec<Arc<Map>>) -> Self {
        schema.validate();
        assert_eq!(maps.len(), schema.cfs.len(), "one map per column family of `{}`", schema.name);
        Self {
            schema,
            state: RwLock::new(State {
                live: maps.clone(),
                durable: maps,
                buffered: Vec::new(),
                injected: None,
                failed: None,
            }),
        }
    }

    /// Opens the database [`maps`](Self::maps) gave when it was closed, with `schema`, checking
    /// the schema record as an open does (a seam for the contract suite's factory).
    #[cfg(any(test, feature = "model"))]
    pub(crate) fn reopened(
        schema: &'static DbSchema,
        maps: Vec<Arc<Map>>,
    ) -> Result<(Self, SchemaState)> {
        let engine = Self::with_maps(schema, maps);
        let state = check_schema(&engine)?;
        Ok((engine, state))
    }

    /// Every committed batch's data, one map per column family.
    pub(crate) fn maps(&self) -> Vec<Arc<Map>> {
        self.state.read().expect("not poisoned").live.clone()
    }

    /// Simulates a power loss followed by recovery: the state at the last barrier plus the first
    /// `keep_buffered` batches committed buffered since then (the engine may keep any prefix).
    /// Like any restart, it clears the fail-stop latch.
    pub fn simulate_power_loss(&self, keep_buffered: usize) {
        let mut state = self.state.write().expect("not poisoned");
        let mut recovered = state.durable.clone();
        for ops in state.buffered.iter().take(keep_buffered) {
            apply(&mut recovered, ops);
        }
        state.live = recovered.clone();
        state.durable = recovered;
        state.buffered.clear();
        state.failed = None;
        state.injected = None;
    }

    /// Simulates a process restart without a power loss (a reopen): every committed batch
    /// survives, buffered ones included, and the fail-stop latch is cleared.
    pub fn restart(&self) {
        let mut state = self.state.write().expect("not poisoned");
        state.failed = None;
        state.injected = None;
    }

    /// Makes the next write or barrier fail with `kind` without applying anything (as a write
    /// that failed before reaching the log); the engine then refuses all later writes. A seam for
    /// error-path tests and ST3a's `FaultyEngine`.
    pub fn inject_failure(&self, kind: ErrorKind) {
        self.state.write().expect("not poisoned").injected = Some(kind);
    }

    /// The kind of the failure that stopped writes, if one has.
    pub fn failed(&self) -> Option<ErrorKind> {
        self.state.read().expect("not poisoned").failed.as_ref().map(|(kind, _)| *kind)
    }

    /// Runs a write or barrier under the fail-stop latch, with one write lock held throughout, so
    /// a concurrent power loss cannot fall between applying and syncing.
    fn guarded(&self, f: impl FnOnce(&mut State)) -> Result<()> {
        let mut state = self.state.write().expect("not poisoned");
        if let Some(first) = &state.failed {
            return Err(refusal(ENGINE, first));
        }
        if let Some(kind) = state.injected.take() {
            let err = StoreError::engine(ENGINE, kind, "injected failure");
            state.failed = Some(failure_of(&err));
            return Err(err);
        }
        f(&mut state);
        Ok(())
    }
}

fn barrier(state: &mut State) {
    state.durable = state.live.clone();
    state.buffered.clear();
}

fn get(maps: &[Arc<Map>], cf: Cf, key: &[u8]) -> Option<Vec<u8>> {
    maps[cf.index()].get(key).cloned()
}

fn scan(map: &Map, prefix: &[u8]) -> Vec<Result<(Vec<u8>, Vec<u8>)>> {
    scan_from(map, prefix, prefix)
}

/// The entries of `prefix` from `start` on (`start` below `prefix` starts at `prefix`).
fn scan_from(map: &Map, prefix: &[u8], start: &[u8]) -> Vec<Result<(Vec<u8>, Vec<u8>)>> {
    map.range(start.max(prefix).to_vec()..)
        .take_while(|(k, _)| k.starts_with(prefix))
        .map(|(k, v)| Ok((k.clone(), v.clone())))
        .collect()
}

impl KvRead for MemoryEngine {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(get(&self.state.read().expect("not poisoned").live, cf, key))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        let state = self.state.read().expect("not poisoned");
        Ok(keys.iter().map(|k| get(&state.live, cf, k)).collect())
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan(&self.maps()[cf.index()], prefix).into_iter()
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_from(&self.maps()[cf.index()], prefix, start).into_iter()
    }
}

/// A batch for [`MemoryEngine`].
#[derive(Default)]
pub struct MemoryBatch {
    ops: Ops,
    size: usize,
}

impl MemoryBatch {
    /// The batch's operations, in order (a seam for ST3a's `FaultyEngine`).
    #[cfg_attr(not(feature = "faulty"), allow(dead_code))]
    pub(crate) fn into_ops(self) -> Ops {
        self.ops
    }
}

impl KvBatch for MemoryBatch {
    fn put(&mut self, cf: Cf, key: &[u8], value: &[u8]) {
        self.size += key.len() + value.len() + 8;
        self.ops.push(BatchOp::Put(cf, key.to_vec(), value.to_vec()));
    }

    fn delete(&mut self, cf: Cf, key: &[u8]) {
        self.size += key.len() + 8;
        self.ops.push(BatchOp::Delete(cf, key.to_vec()));
    }

    fn delete_range(&mut self, cf: Cf, start: &[u8], end: &[u8]) {
        if start < end {
            self.size += start.len() + end.len() + 8;
            self.ops.push(BatchOp::DeleteRange(cf, start.to_vec(), end.to_vec()));
        }
    }

    fn len(&self) -> usize {
        self.ops.len()
    }

    fn size_bytes(&self) -> usize {
        self.size
    }

    fn clear(&mut self) {
        self.ops.clear();
        self.size = 0;
    }
}

/// A snapshot of a [`MemoryEngine`].
pub struct MemorySnapshot {
    cfs: Vec<Arc<Map>>,
}

impl MemorySnapshot {
    /// A snapshot over the given maps, one per column family (a seam for ST3a's `FaultyEngine`).
    #[cfg_attr(not(feature = "faulty"), allow(dead_code))]
    pub(crate) fn from_maps(cfs: Vec<Arc<Map>>) -> Self {
        Self {
            cfs,
        }
    }
}

impl KvRead for MemorySnapshot {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(get(&self.cfs, cf, key))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        Ok(keys.iter().map(|k| get(&self.cfs, cf, k)).collect())
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan(&self.cfs[cf.index()], prefix).into_iter()
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_from(&self.cfs[cf.index()], prefix, start).into_iter()
    }
}

impl KvEngine for MemoryEngine {
    type Batch = MemoryBatch;
    type Snapshot<'a> = MemorySnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.schema
    }

    fn batch(&self) -> MemoryBatch {
        MemoryBatch::default()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> Result<()> {
        self.guarded(|state| {
            apply(&mut state.live, &batch.ops);
            state.buffered.push(batch.ops);
            if commit == Commit::Durable {
                barrier(state);
            }
        })
    }

    fn sync_wal(&self) -> Result<()> {
        self.guarded(barrier)
    }

    fn flush_memtables(&self) -> Result<()> {
        self.guarded(barrier)
    }

    fn snapshot(&self) -> MemorySnapshot {
        MemorySnapshot {
            cfs: self.maps(),
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        let state = self.state.read().expect("not poisoned");
        Ok(state.live[cf.index()].iter().map(|(k, v)| (k.len() + v.len()) as u64).sum())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MODEL_SCHEMA;

    #[test]
    fn an_injected_failure_stops_all_later_writes() {
        let engine = MemoryEngine::new(&MODEL_SCHEMA);
        let cf = MODEL_SCHEMA.cf("point").unwrap();
        let put = |key: &[u8], commit| {
            let mut b = engine.batch();
            b.put(cf, key, b"v");
            engine.write(b, commit)
        };
        put(b"before", Commit::Buffered).unwrap();
        engine.inject_failure(ErrorKind::NoSpace);
        let err = put(b"failed", Commit::Durable).unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::NoSpace));
        assert_eq!(engine.failed(), Some(ErrorKind::NoSpace));
        for result in [
            put(b"after", Commit::Buffered),
            put(b"after", Commit::Durable),
            engine.sync_wal(),
            engine.flush_memtables(),
        ] {
            let err = result.unwrap_err();
            assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
            assert!(err.to_string().contains("NoSpace"), "{err}");
        }
        assert_eq!(engine.get(cf, b"before").unwrap(), Some(b"v".to_vec()));
        assert_eq!(engine.get(cf, b"failed").unwrap(), None);
        assert_eq!(engine.get(cf, b"after").unwrap(), None);

        // A restart (or a power loss) clears the latch; committed batches survive a restart.
        engine.restart();
        assert_eq!(engine.failed(), None);
        put(b"after restart", Commit::Durable).unwrap();
        assert_eq!(engine.get(cf, b"before").unwrap(), Some(b"v".to_vec()));
        engine.inject_failure(ErrorKind::Io);
        assert!(engine.sync_wal().is_err());
        engine.simulate_power_loss(0);
        put(b"after power loss", Commit::Buffered).unwrap();
    }
}
