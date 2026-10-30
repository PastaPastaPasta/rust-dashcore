//! The latch's rules for a shared-runtime store (SB0 §6.3 P6; SB10 Q2): an engine whose failures
//! trip the [`GlobalFailStop`] its client's other stores share.
//!
//! The rules are `dashd-chainstate`'s `GuardedEngine`'s, without the chainstate's own parts (its
//! write epoch, load region, allocation tags and coin errors). A failed read trips the latch, then
//! calls [`AbortSink::read_failed`], then returns its error. A write or barrier checks the latch
//! first and is refused with [`ErrorKind::ReadOnly`] once it has tripped, whichever store tripped
//! it; one that fails trips it. A snapshot the engine could not pin fails its every read, so its
//! first read trips the latch (Q7).
//!
//! An engine's misuse error is a read error like any other: a live read while the thread holds a
//! snapshot (see [`KvEngine::snapshot`]), which SQLite's pool refuses, trips the latch. That is a
//! bug of the caller, and fail-stop is its outcome, as for an assert in Core (SB10 Q8).

use std::sync::Arc;

use crate::{
    AbortSink, Cf, Commit, DbSchema, ErrorKind, Failure, GlobalFailStop, KvEngine, KvRead, Result,
    StoreError,
};

/// Where a [`LatchedEngine`]'s failures happen, as its [`Failure`]s name them: `"<database> read"`,
/// `"<database> write"` and `"<database> barrier"`.
#[derive(Debug, Clone, Copy)]
pub struct Sites {
    pub read: &'static str,
    pub write: &'static str,
    pub barrier: &'static str,
}

/// An engine behind a client's latch.
pub struct LatchedEngine<E> {
    inner: E,
    latch: GlobalFailStop,
    abort: Arc<dyn AbortSink>,
    sites: Sites,
}

impl<E: KvEngine> LatchedEngine<E> {
    pub fn new(inner: E, latch: GlobalFailStop, abort: Arc<dyn AbortSink>, sites: Sites) -> Self {
        Self {
            inner,
            latch,
            abort,
            sites,
        }
    }

    pub fn latch(&self) -> &GlobalFailStop {
        &self.latch
    }

    /// The engine itself, bypassing the latch: for its diagnostics. Nothing writes through it.
    pub fn inner(&self) -> &E {
        &self.inner
    }

    pub fn into_inner(self) -> E {
        self.inner
    }

    /// Fail-stop on a read error: trip the latch, tell the sink, return the error.
    fn read_error(&self, e: StoreError) -> StoreError {
        let what = e.to_string();
        self.latch.trip(Failure {
            kind: e.kind().unwrap_or(ErrorKind::Other),
            site: self.sites.read,
            message: what.clone(),
        });
        self.abort.read_failed(&what);
        e
    }

    /// Runs a write or barrier, unless the latch has tripped; its error trips the latch. The error
    /// is [`ErrorKind::ReadOnly`] exactly when the call latched no failure, so its caller reports
    /// nothing; an engine's own refusal that latches first (a failure the latch did not see) is
    /// reported as [`ErrorKind::Io`].
    fn mutate<T>(&self, site: &'static str, op: impl FnOnce(&E) -> Result<T>) -> Result<T> {
        if let Some(first) = self.latch.tripped() {
            return Err(self.refusal(first));
        }
        op(&self.inner).map_err(|e| {
            let e = match e {
                StoreError::Engine {
                    engine,
                    kind: ErrorKind::ReadOnly,
                    message,
                } => StoreError::Engine {
                    engine,
                    kind: ErrorKind::Io,
                    message,
                },
                e => e,
            };
            let latched = self.latch.trip(Failure {
                kind: e.kind().unwrap_or(ErrorKind::Other),
                site,
                message: e.to_string(),
            });
            if latched {
                e
            } else {
                self.refusal(self.latch.tripped().expect("tripped"))
            }
        })
    }

    fn refusal(&self, first: &Failure) -> StoreError {
        StoreError::engine(
            self.inner.schema().name,
            ErrorKind::ReadOnly,
            format!("refusing writes after an earlier failure ({}: {})", first.site, first.message),
        )
    }
}

impl<E: KvEngine> KvRead for LatchedEngine<E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.get(cf, key).map_err(|e| self.read_error(e))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys).map_err(|e| self.read_error(e))
    }

    fn iter_prefix<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix(cf, prefix).map(|r| r.map_err(|e| self.read_error(e)))
    }

    fn iter_prefix_from<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix_from(cf, prefix, start).map(|r| r.map_err(|e| self.read_error(e)))
    }

    fn iter_prefix_uncached<'a>(
        &'a self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'a {
        self.inner.iter_prefix_uncached(cf, prefix).map(|r| r.map_err(|e| self.read_error(e)))
    }
}

impl<E: KvEngine> KvEngine for LatchedEngine<E> {
    type Batch = E::Batch;
    type Snapshot<'a>
        = LatchedSnapshot<'a, E>
    where
        E: 'a;

    fn schema(&self) -> &'static DbSchema {
        self.inner.schema()
    }

    fn batch(&self) -> E::Batch {
        self.inner.batch()
    }

    fn batch_with_capacity(&self, bytes: usize) -> E::Batch {
        self.inner.batch_with_capacity(bytes)
    }

    fn write(&self, batch: E::Batch, commit: Commit) -> Result<()> {
        self.mutate(self.sites.write, |e| e.write(batch, commit))
    }

    fn write_barrier(&self, batch: E::Batch) -> Result<()> {
        self.mutate(self.sites.write, |e| e.write_barrier(batch))
    }

    fn sync_wal(&self) -> Result<()> {
        self.mutate(self.sites.barrier, KvEngine::sync_wal)
    }

    fn flush_memtables(&self) -> Result<()> {
        self.mutate(self.sites.barrier, KvEngine::flush_memtables)
    }

    fn snapshot(&self) -> LatchedSnapshot<'_, E> {
        LatchedSnapshot {
            engine: self,
            inner: self.inner.snapshot(),
        }
    }

    fn approximate_size(&self, cf: Cf) -> Result<u64> {
        self.inner.approximate_size(cf).map_err(|e| self.read_error(e))
    }
}

/// A consistent read view of a [`LatchedEngine`]: its reads trip the engine's latch as the
/// engine's do.
pub struct LatchedSnapshot<'a, E: KvEngine + 'a> {
    engine: &'a LatchedEngine<E>,
    inner: E::Snapshot<'a>,
}

impl<E: KvEngine> KvRead for LatchedSnapshot<'_, E> {
    fn get(&self, cf: Cf, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.get(cf, key).map_err(|e| self.engine.read_error(e))
    }

    fn multi_get(&self, cf: Cf, keys: &[&[u8]]) -> Result<Vec<Option<Vec<u8>>>> {
        self.inner.multi_get(cf, keys).map_err(|e| self.engine.read_error(e))
    }

    fn iter_prefix<'b>(
        &'b self,
        cf: Cf,
        prefix: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'b {
        self.inner.iter_prefix(cf, prefix).map(|r| r.map_err(|e| self.engine.read_error(e)))
    }

    fn iter_prefix_from<'b>(
        &'b self,
        cf: Cf,
        prefix: &[u8],
        start: &[u8],
    ) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + 'b {
        self.inner
            .iter_prefix_from(cf, prefix, start)
            .map(|r| r.map_err(|e| self.engine.read_error(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{CONTRACT_SCHEMA, EngineFactory, MemoryFactory};
    use crate::{AbortMark, KvBatch, MemoryEngine, RecordingSink, SchemaState};

    const SITES: Sites = Sites {
        read: "test read",
        write: "test write",
        barrier: "test barrier",
    };

    /// Each open gets a fresh latch, as a reopened client does.
    #[derive(Default)]
    struct Latched(MemoryFactory);

    impl EngineFactory for Latched {
        type Engine = LatchedEngine<MemoryEngine>;

        fn fresh(&self) {
            self.0.fresh();
        }

        fn open(&self, schema: &'static DbSchema) -> Result<(Self::Engine, SchemaState)> {
            let (engine, state) = self.0.open(schema)?;
            let sink = Arc::new(RecordingSink::default());
            Ok((LatchedEngine::new(engine, GlobalFailStop::new(), sink, SITES), state))
        }

        fn close(&self, engine: Self::Engine) {
            self.0.close(engine.into_inner());
        }

        fn fail_next_barrier(&self, engine: &Self::Engine) -> Box<dyn FnOnce() + '_> {
            self.0.fail_next_barrier(engine.inner())
        }
    }

    mod contract {
        crate::engine_contract_tests!(super::Latched::default());
    }

    fn point() -> Cf {
        CONTRACT_SCHEMA.cf("point").unwrap()
    }

    fn put(engine: &impl KvEngine, value: &[u8]) -> Result<()> {
        let mut batch = engine.batch();
        batch.put(point(), b"k", value);
        engine.write(batch, Commit::Buffered)
    }

    /// Two stores of one client: a failed write in one refuses writes and barriers in the other,
    /// whose reads keep working; nothing calls the sink for a write.
    #[test]
    fn a_failure_in_one_store_refuses_the_others_writes() {
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        let open = || {
            LatchedEngine::new(
                MemoryEngine::new(&CONTRACT_SCHEMA),
                latch.clone(),
                sink.clone(),
                SITES,
            )
        };
        let (a, b) = (open(), open());
        put(&b, b"before").unwrap();

        a.inner().inject_failure(ErrorKind::NoSpace);
        let err = put(&a, b"x").unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::NoSpace), "the first failure is itself: {err}");
        assert_eq!(
            latch.tripped().map(|f| (f.kind, f.site)),
            Some((ErrorKind::NoSpace, "test write"))
        );

        for refused in [put(&b, b"after"), b.sync_wal(), b.flush_memtables(), put(&a, b"y")] {
            let err = refused.unwrap_err();
            assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
            assert!(err.to_string().contains("test write"), "names the first failure: {err}");
        }
        assert_eq!(b.get(point(), b"k").unwrap().as_deref(), Some(&b"before"[..]));
        assert_eq!(b.snapshot().get(point(), b"k").unwrap().as_deref(), Some(&b"before"[..]));
        assert_eq!(sink.marks(), []);
    }

    /// The engine refusing on its own (it latched a failure the latch did not see) trips the latch
    /// as an I/O failure: the caller must report it, so it is not `ReadOnly`.
    #[test]
    fn an_engines_own_refusal_trips_as_io() {
        let latch = GlobalFailStop::new();
        let engine = LatchedEngine::new(
            MemoryEngine::new(&CONTRACT_SCHEMA),
            latch.clone(),
            Arc::new(RecordingSink::default()),
            SITES,
        );
        engine.inner().inject_failure(ErrorKind::Io);
        put(engine.inner(), b"x").unwrap_err();
        assert!(latch.tripped().is_none());
        let err = put(&engine, b"y").unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::Io), "{err}");
        assert_eq!(latch.tripped().map(|f| f.kind), Some(ErrorKind::Io));
    }

    /// A call that fails after another failure latched while it ran (here, the inner engine's own
    /// latch, shared) reports a refusal: the first failure was reported by whoever latched it.
    #[test]
    fn a_failure_after_another_latched_is_a_refusal() {
        let latch = GlobalFailStop::new();
        let sink: Arc<RecordingSink> = Arc::default();
        let inner = LatchedEngine::new(
            MemoryEngine::new(&CONTRACT_SCHEMA),
            latch.clone(),
            sink.clone(),
            Sites {
                write: "inner write",
                ..SITES
            },
        );
        let outer = LatchedEngine::new(inner, latch.clone(), sink, SITES);
        outer.inner().inner().inject_failure(ErrorKind::NoSpace);
        let err = put(&outer, b"x").unwrap_err();
        assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{err}");
        assert!(err.to_string().contains("inner write"), "{err}");
        assert_eq!(latch.tripped().map(|f| f.site), Some("inner write"));
    }

    /// A read error trips the latch and reaches the sink before the caller sees it.
    #[test]
    fn a_read_error_trips_the_latch_then_tells_the_sink() {
        let latch = GlobalFailStop::new();
        let sink = Arc::new(RecordingSink::default());
        let engine = LatchedEngine::new(
            MemoryEngine::new(&CONTRACT_SCHEMA),
            latch.clone(),
            sink.clone(),
            SITES,
        );
        let err =
            engine.read_error(StoreError::engine("memory", ErrorKind::Corruption, "bad page"));
        assert_eq!(err.kind(), Some(ErrorKind::Corruption));
        assert_eq!(
            latch.tripped().map(|f| (f.kind, f.site)),
            Some((ErrorKind::Corruption, "test read"))
        );
        assert!(
            matches!(&sink.marks()[..], [AbortMark::ReadFailed(what)] if what.contains("bad page"))
        );
        assert_eq!(put(&engine, b"x").unwrap_err().kind(), Some(ErrorKind::ReadOnly));
    }
}
