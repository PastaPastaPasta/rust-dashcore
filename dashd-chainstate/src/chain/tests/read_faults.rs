//! A read-faulting engine for the proposal and template fault sweeps (MN0-2b-2 §10.9, OD-CP-49):
//! a [`MemoryEngine`] whose reads of one column family and key prefix fail with an engine I/O
//! error, answer bytes that decode as nothing ([`Fault::Garbage`]), or hide the row
//! ([`Fault::Absent`]), while armed. Writes pass through. Every matching read is counted, armed
//! or not, so a warm read shows none.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dashd_store::{
    Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, MemoryBatch, MemoryEngine, MemorySnapshot,
    Result, StoreError,
};

/// What an armed read answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// The engine fails the read (`ErrorKind::Io`): Core's `dbwrapper_error`.
    Io,
    /// A stored value that decodes as nothing: Core's `CDBWrapper::Read` returning false.
    Garbage,
    /// The row is not there: a get answers none and a scan skips it.
    Absent,
}

/// Bytes no evo or coins record decodes from.
const GARBAGE: [u8; 3] = [0xff, 0xff, 0xff];

/// The column family and key prefix watched, and the fault their reads answer.
type Target = (Cf, Vec<u8>, Option<Fault>);

/// A scan's rows.
type Rows<'a> = Box<dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a>;

#[derive(Default)]
struct Armed {
    target: Mutex<Option<Target>>,
    hits: AtomicU32,
    keys: Mutex<Vec<Vec<u8>>>,
}

/// The switch a test arms; shared with the engine and its snapshots.
#[derive(Clone, Default)]
pub(super) struct Faults(Arc<Armed>);

impl Faults {
    /// Counts the reads of `cf` under `prefix` from now on, failing them with `fault`.
    pub fn arm(&self, cf: Cf, prefix: &[u8], fault: Fault) {
        self.set(cf, prefix, Some(fault));
    }

    /// Counts the reads of `cf` under `prefix` from now on, answering them unchanged.
    pub fn watch(&self, cf: Cf, prefix: &[u8]) {
        self.set(cf, prefix, None);
    }

    fn set(&self, cf: Cf, prefix: &[u8], fault: Option<Fault>) {
        *self.0.target.lock().expect("faults") = Some((cf, prefix.to_vec(), fault));
        self.0.hits.store(0, Ordering::SeqCst);
        self.0.keys.lock().expect("faults").clear();
    }

    pub fn disarm(&self) {
        *self.0.target.lock().expect("faults") = None;
    }

    /// The matching reads since the last arm or watch.
    pub fn hits(&self) -> u32 {
        self.0.hits.load(Ordering::SeqCst)
    }

    /// The keys of the matching reads since the last arm or watch, in read order.
    pub fn read_keys(&self) -> Vec<Vec<u8>> {
        self.0.keys.lock().expect("faults").clone()
    }

    /// The fault for a read of `key` in `cf` (`None` when it matches nothing or only watches).
    fn on(&self, cf: Cf, key: &[u8]) -> Option<Fault> {
        let target = self.0.target.lock().expect("faults");
        let (t_cf, prefix, fault) = target.as_ref()?;
        if *t_cf != cf || !key.starts_with(prefix) {
            return None;
        }
        self.0.hits.fetch_add(1, Ordering::SeqCst);
        self.0.keys.lock().expect("faults").push(key.to_vec());
        *fault
    }

    /// Whether a scan of `cf` under `scan` can meet the target's keys.
    fn overlaps(&self, cf: Cf, scan: &[u8]) -> bool {
        let target = self.0.target.lock().expect("faults");
        target.as_ref().is_some_and(|(t_cf, prefix, _)| {
            *t_cf == cf && (prefix.starts_with(scan) || scan.starts_with(prefix))
        })
    }

    fn get<R: KvRead>(&self, inner: &R, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.on(cf, key) {
            Some(Fault::Io) => Err(injected()),
            Some(Fault::Garbage) => Ok(inner.get(cf, key)?.map(|_| GARBAGE.to_vec())),
            Some(Fault::Absent) => Ok(None),
            None => inner.get(cf, key),
        }
    }

    fn scan<'a>(
        &'a self,
        cf: Cf,
        scan: &[u8],
        rows: impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a,
    ) -> Rows<'a> {
        if !self.overlaps(cf, scan) {
            return Box::new(rows);
        }
        Box::new(rows.filter_map(move |row| {
            let (key, value) = match row {
                Ok(row) => row,
                Err(e) => return Some(Err(e)),
            };
            match self.on(cf, &key) {
                Some(Fault::Io) => Some(Err(injected())),
                Some(Fault::Garbage) => Some(Ok((key, GARBAGE.to_vec()))),
                Some(Fault::Absent) => None,
                None => Some(Ok((key, value))),
            }
        }))
    }
}

fn injected() -> StoreError {
    StoreError::engine("memory", ErrorKind::Io, "injected read failure")
}

/// A [`MemoryEngine`] behind [`Faults`].
pub(super) struct ReadFaults {
    inner: MemoryEngine,
    faults: Faults,
}

impl ReadFaults {
    pub fn new(schema: &'static DbSchema, faults: Faults) -> Self {
        ReadFaults {
            inner: MemoryEngine::new(schema),
            faults,
        }
    }
}

impl KvRead for ReadFaults {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.faults.get(&self.inner, cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|k| self.get(cf, k)).collect()
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.faults.scan(cf, prefix, self.inner.iter_prefix(cf, prefix))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.faults.scan(cf, prefix, self.inner.iter_prefix_from(cf, prefix, start))
    }
}

/// A [`ReadFaults`] snapshot, faulting as the engine does.
pub(super) struct FaultSnapshot {
    inner: MemorySnapshot,
    faults: Faults,
}

impl KvRead for FaultSnapshot {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.faults.get(&self.inner, cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        keys.iter().map(|k| self.get(cf, k)).collect()
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.faults.scan(cf, prefix, self.inner.iter_prefix(cf, prefix))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.faults.scan(cf, prefix, self.inner.iter_prefix_from(cf, prefix, start))
    }
}

impl KvEngine for ReadFaults {
    type Batch = MemoryBatch;
    type Snapshot<'a> = FaultSnapshot;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> MemoryBatch {
        self.inner.batch()
    }

    fn write(&self, batch: MemoryBatch, commit: Commit) -> Result<()> {
        self.inner.write(batch, commit)
    }

    fn sync_wal(&self) -> Result<()> {
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> Result<()> {
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> FaultSnapshot {
        FaultSnapshot {
            inner: self.inner.snapshot(),
            faults: self.faults.clone(),
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        self.inner.approximate_size(cf)
    }
}

mod fidelity {
    use std::sync::Arc;

    use dashd_store::{Commit, ErrorKind, KvBatch as _, KvEngine as _, KvRead as _, StoreError};

    use super::{Fault, Faults, ReadFaults};
    use crate::chain::RecordingAbort;
    use crate::store::sites::READ_SITE;
    use crate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine};

    /// How the guarded engine handled one failed read: the error's variant and kind, the latch's
    /// failure (kind, site) and the abort sink's reports.
    fn classify<E: dashd_store::KvEngine>(
        engine: E,
        cf: &str,
        key: &[u8],
    ) -> (bool, Option<ErrorKind>, (ErrorKind, &'static str), usize) {
        let sink = Arc::new(RecordingAbort::default());
        let latch = GlobalFailStop::new();
        let db = GuardedEngine::new(engine, latch.clone(), sink.clone());
        let cf = CHAINSTATE_SCHEMA.cf(cf).expect("a chainstate cf");
        let e = db.get(cf, key).expect_err("the read fails");
        let failure = latch.tripped().expect("the latch trips");
        (
            matches!(e, StoreError::Engine { .. }),
            e.kind(),
            (failure.kind, failure.site),
            sink.reports().len(),
        )
    }

    /// OD-CP-95 c1: an armed I/O fault is the engine's own error value, built as MemoryEngine's
    /// failures (`dash-kv/src/memory.rs`) and RocksDB's statuses (`dashd-store/src/rocks.rs`
    /// `status_error`) are: [`StoreError::Engine`] of kind I/O. Behind the guarded engine it is
    /// handled as a real engine's failed read is: the latch trips with its kind at the read
    /// site, and it is reported once.
    #[test]
    fn an_injected_read_error_is_handled_as_an_engines() {
        let faults = Faults::default();
        let engine = ReadFaults::new(&CHAINSTATE_SCHEMA, faults.clone());
        faults.arm(CHAINSTATE_SCHEMA.cf("mn_diff").expect("mn_diff"), &[], Fault::Io);
        assert_eq!(
            classify(engine, "mn_diff", b"k"),
            (true, Some(ErrorKind::Io), (ErrorKind::Io, READ_SITE), 1)
        );
    }

    /// The same handling for a real RocksDB read error: the first data block of a coins table,
    /// whose checksum then fails.
    #[cfg(feature = "rocksdb")]
    #[test]
    fn a_rocksdb_read_error_is_handled_the_same() {
        let dir = tempfile::tempdir().expect("a dir");
        let cf = CHAINSTATE_SCHEMA.cf("coins").expect("coins");
        {
            let (engine, _) =
                dashd_store::RocksEngine::open(dir.path(), &CHAINSTATE_SCHEMA, &Default::default())
                    .expect("opens");
            let mut batch = engine.batch();
            let mut x = 0x9e37_79b9_u32;
            for i in 0..2_000_u32 {
                let value: Vec<u8> = (0..32)
                    .map(|_| {
                        x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (x >> 24) as u8
                    })
                    .collect();
                batch.put(cf, &i.to_be_bytes(), &value);
            }
            engine.write(batch, Commit::Durable).expect("writes");
            engine.flush_memtables().expect("flushes");
        }
        let largest = std::fs::read_dir(dir.path())
            .expect("lists")
            .map(|entry| entry.expect("an entry").path())
            .filter(|path| path.extension().is_some_and(|e| e == "sst"))
            .max_by_key(|path| std::fs::metadata(path).expect("a size").len())
            .expect("a table file");
        let mut bytes = std::fs::read(&largest).expect("reads");
        assert!(bytes.len() > 8_192, "the coins table: {} bytes", bytes.len());
        bytes[1024..1088].fill(0);
        std::fs::write(&largest, bytes).expect("writes");
        let (engine, _) =
            dashd_store::RocksEngine::open(dir.path(), &CHAINSTATE_SCHEMA, &Default::default())
                .expect("opens");
        let (engine_variant, kind, (latched, site), reports) =
            classify(engine, "coins", &0_u32.to_be_bytes());
        let kind = kind.expect("a kind");
        assert_eq!((engine_variant, latched, site, reports), (true, kind, READ_SITE, 1));
    }
}
