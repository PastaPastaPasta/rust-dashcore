//! [`ProbeEngine`]: an engine wrapper that counts its point reads and can fail them, its
//! snapshots' and scans' too, for tests and model targets (the membership tests check that
//! `have_in_cache` reads nothing; the read-error tests need a read that fails, which a
//! [`MemoryEngine`] or a healthy `RocksEngine` never does). It can also crash at a chosen write,
//! fail a chosen barrier, and over a [`MemoryEngine`] restart on the same data (the crash-resume
//! tests), run a hook just after it takes a snapshot (the epoch-order test), and record the
//! capacities its batches were asked for (the flush machine's R).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dashd_store::{
    Cf, Commit, DbSchema, ErrorKind, KvEngine, KvRead, MemoryEngine, Result, StoreError,
};

use super::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink};

pub struct ProbeEngine<E = MemoryEngine> {
    inner: Arc<E>,
    reads: AtomicU64,
    failing: Mutex<Option<ErrorKind>>,
    /// Writes until the one that crashes; 0 once it has, and every later write or barrier fails.
    crash_in: Mutex<Option<u32>>,
    /// Barriers until the one that fails.
    barrier_fails_in: Mutex<Option<u32>>,
    after_snapshot: Mutex<Option<SnapshotHook>>,
    capacities: Mutex<Vec<usize>>,
}

type SnapshotHook = Box<dyn FnOnce() + Send>;

impl ProbeEngine {
    pub fn new(schema: &'static DbSchema) -> Self {
        Self::over(MemoryEngine::new(schema))
    }

    /// The database after a power loss and a restart: the Durable writes, none of the buffered
    /// ones since the last barrier, and no failure.
    pub fn restart(&self) -> Self {
        self.inner.simulate_power_loss(0);
        Self::over_shared(Arc::clone(&self.inner))
    }
}

impl<E: KvEngine> ProbeEngine<E> {
    /// Probes `inner`.
    pub fn over(inner: E) -> Self {
        Self::over_shared(Arc::new(inner))
    }

    fn over_shared(inner: Arc<E>) -> Self {
        Self {
            inner,
            reads: AtomicU64::new(0),
            failing: Mutex::new(None),
            crash_in: Mutex::new(None),
            barrier_fails_in: Mutex::new(None),
            after_snapshot: Mutex::new(None),
            capacities: Mutex::new(Vec::new()),
        }
    }

    /// Makes the `n`-th write from now (1-based) fail with an I/O error before anything reaches
    /// the engine, as a crash there would leave it; the engine refuses later writes and barriers.
    pub fn crash_at_write(&self, n: u32) {
        assert!(n > 0, "the first write is 1");
        *self.crash_in.lock().expect("unpoisoned") = Some(n);
    }

    /// Makes the `n`-th barrier from now (1-based: `sync_wal` or `flush_memtables`) fail with an
    /// I/O error; it does not reach the engine.
    pub fn fail_barrier_at(&self, n: u32) {
        assert!(n > 0, "the first barrier is 1");
        *self.barrier_fails_in.lock().expect("unpoisoned") = Some(n);
    }

    /// The capacity of each batch made with `batch_with_capacity` so far, in order.
    pub fn capacities(&self) -> Vec<usize> {
        self.capacities.lock().expect("unpoisoned").clone()
    }

    /// Runs `f` once, just after the next snapshot is taken and before it is returned.
    pub fn after_next_snapshot(&self, f: impl FnOnce() + Send + 'static) {
        *self.after_snapshot.lock().expect("unpoisoned") = Some(Box::new(f));
    }

    /// Point reads so far (`get` and each key of `multi_get`, the snapshots' included).
    pub fn reads(&self) -> u64 {
        self.reads.load(Ordering::Relaxed)
    }

    /// Makes every later point read and scan fail with `kind`, or work again with `None`.
    pub fn fail_reads(&self, kind: Option<ErrorKind>) {
        *self.failing.lock().expect("unpoisoned") = kind;
    }

    /// The engine beneath: its reads are not counted and do not fail.
    pub fn unprobed(&self) -> &E {
        &self.inner
    }

    fn read(&self, n: usize) -> Result<()> {
        self.reads.fetch_add(n as u64, Ordering::Relaxed);
        match *self.failing.lock().expect("unpoisoned") {
            Some(kind) => Err(probe_error(kind, "injected read failure")),
            None => Ok(()),
        }
    }

    /// A write or barrier: fails if the crash has come, and with `write` counts down to it; a
    /// barrier counts down to its own failure.
    fn crash_point(&self, write: bool) -> Result<()> {
        if !write {
            let mut fails_in = self.barrier_fails_in.lock().expect("unpoisoned");
            match *fails_in {
                Some(1) => {
                    *fails_in = None;
                    return Err(probe_error(ErrorKind::Io, "barrier failed"));
                }
                Some(n) => *fails_in = Some(n - 1),
                None => {}
            }
        }
        let mut crash_in = self.crash_in.lock().expect("unpoisoned");
        match *crash_in {
            Some(0) => Err(probe_error(ErrorKind::Io, "refused after the crash")),
            Some(1) if write => {
                *crash_in = Some(0);
                Err(probe_error(ErrorKind::Io, "crashed"))
            }
            Some(n) if write => {
                *crash_in = Some(n - 1);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

fn probe_error(kind: ErrorKind, message: &str) -> StoreError {
    StoreError::Engine {
        engine: "probe",
        kind,
        message: message.into(),
    }
}

/// A scan's `entries`, or `failed`'s injected failure alone.
fn scan_or_fail<I: Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>>(
    failed: Result<()>,
    entries: I,
) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> {
    let failed = failed.err();
    let entries = failed.is_none().then_some(entries).into_iter().flatten();
    failed.map(Err).into_iter().chain(entries)
}

/// A [`ProbeEngine`]'s snapshot: its reads count and fail as the engine's do.
pub struct ProbeSnapshot<'a, E: KvEngine + 'a> {
    probe: &'a ProbeEngine<E>,
    inner: E::Snapshot<'a>,
}

impl<'a, E: KvEngine + 'a> KvRead for ProbeSnapshot<'a, E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.probe.read(1)?;
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        self.probe.read(keys.len())?;
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'s>(
        &'s self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 's {
        scan_or_fail(self.probe.read(0), self.inner.iter_prefix(cf, prefix))
    }

    fn iter_prefix_from<'s>(
        &'s self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 's {
        scan_or_fail(self.probe.read(0), self.inner.iter_prefix_from(cf, prefix, start))
    }
}

impl GuardedEngine<ProbeEngine> {
    /// A chainstate [`ProbeEngine`] behind its own latch, reporting to `sink`.
    pub fn probe(sink: Arc<RecordingSink>) -> Self {
        Self::new(ProbeEngine::new(&CHAINSTATE_SCHEMA), GlobalFailStop::new(), sink)
    }
}

impl<E: KvEngine> KvRead for ProbeEngine<E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.read(1)?;
        self.inner.get(cf, key)
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        self.read(keys.len())?;
        self.inner.multi_get(cf, keys)
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_or_fail(self.read(0), self.inner.iter_prefix(cf, prefix))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_or_fail(self.read(0), self.inner.iter_prefix_from(cf, prefix, start))
    }

    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        scan_or_fail(self.read(0), self.inner.iter_prefix_uncached(cf, prefix))
    }
}

impl<E: KvEngine> KvEngine for ProbeEngine<E> {
    type Batch = E::Batch;
    type Snapshot<'a>
        = ProbeSnapshot<'a, E>
    where
        E: 'a;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> E::Batch {
        self.inner.batch()
    }

    fn batch_with_capacity(&self, bytes: usize) -> E::Batch {
        self.capacities.lock().expect("unpoisoned").push(bytes);
        self.inner.batch_with_capacity(bytes)
    }

    fn write(&self, batch: E::Batch, commit: Commit) -> Result<()> {
        self.crash_point(true)?;
        self.inner.write(batch, commit)
    }

    fn write_barrier(&self, batch: E::Batch) -> Result<()> {
        self.crash_point(true)?;
        self.inner.write_barrier(batch)
    }

    fn sync_wal(&self) -> Result<()> {
        self.crash_point(false)?;
        self.inner.sync_wal()
    }

    fn flush_memtables(&self) -> Result<()> {
        self.crash_point(false)?;
        self.inner.flush_memtables()
    }

    fn snapshot(&self) -> ProbeSnapshot<'_, E> {
        let inner = self.inner.snapshot();
        let hook = self.after_snapshot.lock().expect("unpoisoned").take();
        if let Some(f) = hook {
            f();
        }
        ProbeSnapshot {
            probe: self,
            inner,
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        self.inner.approximate_size(cf)
    }
}
