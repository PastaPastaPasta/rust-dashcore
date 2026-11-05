//! The in-memory engine: one `BTreeMap` per column family (unit tests, `kv_model`).
//!
//! It models the durability contract of [`KvEngine`]: buffered batches are kept apart until a
//! barrier, so [`MemoryEngine::simulate_power_loss`] can drop any suffix of them. It also
//! follows the fail-stop contract: [`MemoryEngine::inject_failure`] fails the next write or
//! barrier, after which every write is refused, as on RocksDB, until a restart
//! ([`MemoryEngine::restart`] or a power loss).
//!
//! Prefix scans are lazy (BL-662): a scan clones only the rows its caller pulls, so a caller that
//! stops after k rows pays for k rows and their seeks (one for a snapshot's scan, one per row for
//! a live one), not for the rest of the prefix (D25).

use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Included, Unbounded};
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
/// A live prefix iterator holds its column family's map the same way: it reads the data as of
/// the call, and the first write committed to that column family while it is alive copies the
/// whole column family's map (not only the prefix): once per scan alive at a write. Drop a scan
/// before writing; a `while let` over `iter_prefix(..).next()` keeps a new one alive through
/// each pass of its body.
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

    /// A copy of every committed batch's data (copy-on-write: O(#CF)), as a reopen would find it.
    #[cfg(feature = "image")]
    pub fn image(&self) -> MemoryImage {
        MemoryImage {
            schema: self.schema,
            maps: self.maps(),
        }
    }

    /// Replaces the database with `image`, as a reopen of the storage it was taken from: all of
    /// it durable, the batches buffered since the last barrier dropped and the fail-stop latch
    /// cleared.
    #[cfg(feature = "image")]
    pub fn replace(&self, image: &MemoryImage) {
        assert!(std::ptr::eq(image.schema, self.schema), "an image of `{}`", image.schema.name);
        let mut state = self.state.write().expect("not poisoned");
        state.live = image.maps.clone();
        state.durable = image.maps.clone();
        state.buffered.clear();
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

/// The entries of `prefix` from `start` on (`start` below `prefix` starts at `prefix`), cloned as
/// the caller pulls them: one seek, then a walk (O(log n + k) for k rows).
fn scan_from<'a>(
    map: &'a Map,
    prefix: &[u8],
    start: &[u8],
) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
    let rows = map.range::<[u8], _>((Included(start.max(prefix)), Unbounded));
    let prefix = prefix.to_vec();
    rows.take_while(move |(k, _)| k.starts_with(&prefix)).map(|(k, v)| Ok((k.clone(), v.clone())))
}

/// [`scan_from`] over a map the iterator owns: a live [`MemoryEngine`] scan, which must not hold
/// the engine's lock while its caller iterates. A `Range` cannot borrow from a map its own
/// iterator owns, so each row is a seek past the last key returned: O(log n) per row, cloning
/// only the rows pulled.
struct LiveScan {
    /// The column family's map as of the call.
    map: Arc<Map>,
    prefix: Vec<u8>,
    /// The first row's seek key (`max(start, prefix)`, included), then the last key returned
    /// (excluded): a buffer reused from row to row.
    seek: Vec<u8>,
    past: bool,
}

impl LiveScan {
    fn new(map: Arc<Map>, prefix: &[u8], start: &[u8]) -> Self {
        Self {
            map,
            prefix: prefix.to_vec(),
            seek: start.max(prefix).to_vec(),
            past: false,
        }
    }
}

impl Iterator for LiveScan {
    type Item = Result<(Vec<u8>, Vec<u8>)>;

    fn next(&mut self) -> Option<Self::Item> {
        let from = if self.past {
            Excluded(self.seek.as_slice())
        } else {
            Included(self.seek.as_slice())
        };
        let (k, v) = self.map.range::<[u8], _>((from, Unbounded)).next()?;
        if !k.starts_with(&self.prefix) {
            return None;
        }
        self.seek.clear();
        self.seek.extend_from_slice(k);
        self.past = true;
        Some(Ok((k.clone(), v.clone())))
    }
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
        self.iter_prefix_from(cf, prefix, prefix)
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        let map = Arc::clone(&self.state.read().expect("not poisoned").live[cf.index()]);
        LiveScan::new(map, prefix, start)
    }
}

/// A [`MemoryEngine`]'s committed data, from [`MemoryEngine::image`].
#[cfg(feature = "image")]
#[derive(Clone)]
pub struct MemoryImage {
    schema: &'static DbSchema,
    maps: Vec<Arc<Map>>,
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
        scan_from(&self.cfs[cf.index()], prefix, prefix)
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_from(&self.cfs[cf.index()], prefix, start)
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

    /// A seeded xorshift64: the same cases on every run.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        /// `max_len` or fewer bytes from {0x00, 0x01, 0x7f, 0xff}: prefix edges are common.
        fn key(&mut self, max_len: u64) -> Vec<u8> {
            const ALPHABET: [u8; 4] = [0x00, 0x01, 0x7f, 0xff];
            let len = self.below(max_len + 1);
            (0..len).map(|_| ALPHABET[self.below(4) as usize]).collect()
        }
    }

    type Rows = Vec<(Vec<u8>, Vec<u8>)>;

    /// What a prefix scan from `start` yields, read off the whole map: the oracle.
    fn expected(map: &Map, prefix: &[u8], start: &[u8]) -> Rows {
        map.iter()
            .filter(|(k, _)| k.starts_with(prefix) && k.as_slice() >= start)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn rows(iter: impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>) -> Rows {
        iter.map(Result::unwrap).collect()
    }

    /// Commits random puts, deletes and range deletes to `engine` and to `mirror`.
    fn random_writes(rng: &mut Rng, engine: &MemoryEngine, cf: Cf, mirror: &mut Map, n: u64) {
        let mut batch = engine.batch();
        let mut ops = Vec::new();
        for _ in 0..n {
            let key = rng.key(4);
            match rng.below(4) {
                0 => {
                    batch.delete(cf, &key);
                    ops.push(BatchOp::Delete(cf, key));
                }
                1 => {
                    let end = rng.key(4);
                    batch.delete_range(cf, &key, &end);
                    if key < end {
                        ops.push(BatchOp::DeleteRange(cf, key, end));
                    }
                }
                _ => {
                    let value = rng.next().to_le_bytes().to_vec();
                    batch.put(cf, &key, &value);
                    ops.push(BatchOp::Put(cf, key, value));
                }
            }
        }
        let commit = if rng.below(2) == 0 {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        engine.write(batch, commit).unwrap();
        for op in ops {
            match op {
                BatchOp::Put(_, k, v) => {
                    mirror.insert(k, v);
                }
                BatchOp::Delete(_, k) => {
                    mirror.remove(&k);
                }
                BatchOp::DeleteRange(_, start, end) => {
                    mirror.retain(|k, _| *k < start || *k >= end)
                }
            }
        }
    }

    /// BL-662: the lazy scans of the engine and of its snapshots yield exactly the entries of the
    /// prefix from `max(start, prefix)` on, in order, as of the call or the snapshot, whatever is
    /// written while they are alive.
    #[test]
    fn lazy_prefix_scans_yield_the_prefix_from_start_as_of_the_call() {
        let cf = MODEL_SCHEMA.cf("point").unwrap();
        let mut rng = Rng(0x0b16_62d5_ca11_e5a1);
        // Cases by where `start` lies: below the prefix, inside it, above it.
        let (mut below, mut inside, mut above, mut empty_prefix) = (0, 0, 0, 0);
        for _ in 0..400 {
            let engine = MemoryEngine::new(&MODEL_SCHEMA);
            let mut mirror = Map::new();
            let rows_n = rng.below(48);
            random_writes(&mut rng, &engine, cf, &mut mirror, rows_n);
            for _ in 0..8 {
                let prefix = rng.key(2);
                let start = match rng.below(4) {
                    // Inside the prefix: the prefix and a random suffix.
                    0 => [prefix.as_slice(), &rng.key(2)].concat(),
                    _ => rng.key(4),
                };
                if prefix.is_empty() {
                    empty_prefix += 1;
                }
                if start.starts_with(&prefix) {
                    inside += 1;
                } else if start < prefix {
                    below += 1;
                } else {
                    above += 1;
                }
                let want = expected(&mirror, &prefix, &start);
                let want_all = expected(&mirror, &prefix, &[]);
                let snapshot = engine.snapshot();
                assert_eq!(rows(engine.iter_prefix_from(cf, &prefix, &start)), want);
                assert_eq!(rows(engine.iter_prefix(cf, &prefix)), want_all);
                assert_eq!(rows(snapshot.iter_prefix_from(cf, &prefix, &start)), want);
                assert_eq!(rows(snapshot.iter_prefix(cf, &prefix)), want_all);

                // Writes while a snapshot and two live iterators (one part way) are alive: none of
                // them sees any of it.
                let mut live = engine.iter_prefix_from(cf, &prefix, &start);
                let mut live_all = engine.iter_prefix(cf, &prefix);
                let first: Rows =
                    live.by_ref().take(rng.below(3) as usize).map(Result::unwrap).collect();
                let writes = rng.below(12);
                random_writes(&mut rng, &engine, cf, &mut mirror, writes);
                let snapshot_from = snapshot.iter_prefix_from(cf, &prefix, &start);
                random_writes(&mut rng, &engine, cf, &mut mirror, writes);
                assert_eq!([first, rows(live)].concat(), want);
                assert_eq!(rows(live_all.by_ref()), want_all);
                assert_eq!(rows(snapshot_from), want);
                assert_eq!(rows(snapshot.iter_prefix(cf, &prefix)), want_all);
                // A scan after the writes sees them.
                assert_eq!(
                    rows(engine.iter_prefix_from(cf, &prefix, &start)),
                    expected(&mirror, &prefix, &start)
                );
                assert_eq!(
                    rows(engine.snapshot().iter_prefix(cf, &prefix)),
                    expected(&mirror, &prefix, &[])
                );
            }
        }
        assert!(
            below > 100 && inside > 100 && above > 100 && empty_prefix > 100,
            "below {below}, inside {inside}, above {above}, empty prefix {empty_prefix}"
        );
    }

    /// An exhausted live scan stays exhausted, and a scan past the column family's end is empty.
    #[test]
    fn a_live_scan_ends_at_the_prefix_end() {
        let engine = MemoryEngine::new(&MODEL_SCHEMA);
        let cf = MODEL_SCHEMA.cf("point").unwrap();
        let mut batch = engine.batch();
        for key in [&b"a"[..], b"b1", b"b2", b"c"] {
            batch.put(cf, key, b"v");
        }
        engine.write(batch, Commit::Durable).unwrap();
        let mut scan = engine.iter_prefix(cf, b"b");
        assert_eq!(scan.by_ref().map(|r| r.unwrap().0).collect::<Vec<_>>(), [b"b1", b"b2"]);
        assert!(scan.next().is_none());
        assert!(engine.iter_prefix_from(cf, b"", b"d").next().is_none());
        assert!(engine.iter_prefix_from(cf, b"b", b"b3").next().is_none());
    }

    #[cfg(feature = "image")]
    #[test]
    fn an_image_is_the_committed_data_and_replace_reopens_it() {
        let engine = MemoryEngine::new(&MODEL_SCHEMA);
        let cf = MODEL_SCHEMA.cf("point").unwrap();
        let put = |key: &[u8], commit| {
            let mut b = engine.batch();
            b.put(cf, key, b"v");
            engine.write(b, commit).unwrap();
        };
        let keys = |engine: &MemoryEngine| -> Vec<Vec<u8>> {
            engine.iter_prefix(cf, b"").map(|kv| kv.unwrap().0).collect()
        };
        put(b"durable", Commit::Durable);
        put(b"buffered", Commit::Buffered);
        let image = engine.image();
        // Later writes are not in the image.
        put(b"later", Commit::Durable);
        put(b"later buffered", Commit::Buffered);
        engine.inject_failure(ErrorKind::Io);
        assert!(engine.sync_wal().is_err());
        engine.replace(&image);
        // Both batches the image saw, buffered included; nothing later; the latch cleared.
        assert_eq!(keys(&engine), [b"buffered".to_vec(), b"durable".to_vec()]);
        assert_eq!(engine.failed(), None);
        // The schema record came with the data: the replaced database opens as itself.
        assert_eq!(check_schema(&engine).unwrap(), SchemaState::Current);
        // All of it is durable: a power loss right after keeps it, and drops later buffered ones.
        put(b"after", Commit::Buffered);
        engine.simulate_power_loss(0);
        assert_eq!(keys(&engine), [b"buffered".to_vec(), b"durable".to_vec()]);
        // The image is unchanged by the writes made after it was put back.
        engine.replace(&image);
        assert_eq!(keys(&engine), [b"buffered".to_vec(), b"durable".to_vec()]);
    }
}
