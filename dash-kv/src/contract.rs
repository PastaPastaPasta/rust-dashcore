//! The engine contract suite: what every [`KvEngine`] must do, run on each engine (SB0 §9.4).
//!
//! An engine's tests implement [`EngineFactory`] (open a database, close it, make its next
//! barrier fail) and expand [`engine_contract_tests!`](crate::engine_contract_tests), which gives
//! one test per check:
//! - [`model_differential`]: `kv_model` with reopens: every committed batch survives a close;
//! - [`delete_range`]: [`KvBatch::delete_range`]'s semantics;
//! - [`schema_versions`]: a new database is stamped, and an older version, a newer one and another
//!   database's record are told apart on open without writing anything;
//! - [`fail_stop`]: after a failed barrier every write and barrier is refused until a reopen, and
//!   reads keep working;
//! - [`batch_atomic_across_reopen`]: a batch over several column families lands whole;
//! - [`snapshot_interleaving`]: SB0 §9.4's write A, snapshot, commit B, commit C: the snapshot
//!   reads A throughout;
//! - [`reads_after_snapshot_reuse`]: a read after a commit sees it, also when the snapshots before
//!   it were taken and dropped (a pooled engine's recycled read connection);
//! - [`prefix_seek`]: [`KvRead::iter_prefix_from`] is the prefix scan from a start key, through
//!   the engine and a snapshot.
//!
//! The checks hold at most one snapshot at a time, and read through the engine while holding it:
//! an engine with a bounded read pool runs them with two readers or more.

use std::sync::{Arc, Mutex};

use crate::engine::{Commit, KvBatch, KvEngine, KvRead};
use crate::error::{ErrorKind, Result, StoreError};
use crate::memory::{Map, MemoryEngine};
use crate::model::{self, Hooks, MODEL_SCHEMA};
use crate::schema::{CfProfile, CfSpec, DbSchema, SchemaState};

/// How the contract opens, closes and breaks an engine's databases.
pub trait EngineFactory {
    type Engine: KvEngine;

    /// The `kv_model` seeds [`model_differential`] runs (each a fresh database with reopens).
    const MODEL_SEEDS: u64 = 64;

    /// Forgets the current database: the next [`open`](Self::open) creates an empty one.
    fn fresh(&self);

    /// Opens the current database with `schema` (creating it if it is new), as the engine's own
    /// open does: it checks or stamps the schema record. At most one engine is open at a time.
    fn open(&self, schema: &'static DbSchema) -> Result<(Self::Engine, SchemaState)>;

    /// Closes `engine` as a process exit does (not a power loss): every committed batch stays.
    fn close(&self, engine: Self::Engine) {
        drop(engine);
    }

    /// Makes `engine`'s next barrier fail with an error other than [`ErrorKind::ReadOnly`]; the
    /// contract calls [`KvEngine::flush_memtables`] next, with data buffered. Returns what removes
    /// the cause again (the engine must stay stopped regardless).
    fn fail_next_barrier(&self, engine: &Self::Engine) -> Box<dyn FnOnce() + '_>;
}

/// The contract's database: data in two column families of different profiles.
pub static CONTRACT_SCHEMA: DbSchema = DbSchema {
    name: "kv-contract",
    version: 2,
    cfs: &[
        CfSpec::new("meta", CfProfile::Ordered),
        CfSpec::new("point", CfProfile::PointLookup),
        CfSpec::new("ordered", CfProfile::Ordered),
    ],
};

/// [`CONTRACT_SCHEMA`] at the version before.
static CONTRACT_V1: DbSchema = DbSchema {
    version: 1,
    ..CONTRACT_SCHEMA
};

/// [`CONTRACT_SCHEMA`] at the version after.
static CONTRACT_V3: DbSchema = DbSchema {
    version: 3,
    ..CONTRACT_SCHEMA
};

/// Another database with the same column families.
static OTHER_SCHEMA: DbSchema = DbSchema {
    name: "kv-contract-other",
    ..CONTRACT_SCHEMA
};

fn point() -> crate::Cf {
    CONTRACT_SCHEMA.cf("point").expect("in the schema")
}

fn ordered() -> crate::Cf {
    CONTRACT_SCHEMA.cf("ordered").expect("in the schema")
}

fn put<E: KvEngine>(engine: &E, key: &[u8], value: &[u8], commit: Commit) -> Result<()> {
    let mut batch = engine.batch();
    batch.put(point(), key, value);
    engine.write(batch, commit)
}

fn get(db: &impl KvRead, key: &[u8]) -> Option<Vec<u8>> {
    db.get(point(), key).expect("reads work")
}

fn scan(db: &impl KvRead, cf: crate::Cf) -> Vec<(Vec<u8>, Vec<u8>)> {
    db.iter_prefix(cf, &[]).collect::<Result<_>>().expect("scans work")
}

fn open<F: EngineFactory>(f: &F, schema: &'static DbSchema, want: SchemaState) -> F::Engine {
    let (engine, state) =
        f.open(schema).unwrap_or_else(|e| panic!("opening `{}`: {e}", schema.name));
    assert_eq!(state, want, "schema `{}` v{}", schema.name, schema.version);
    engine
}

/// `kv_model` on [`MODEL_SCHEMA`], with each `Reopen` op closing and reopening the database.
pub fn model_differential<F: EngineFactory>(f: &F) {
    let mut reopens = 0;
    for seed in 0..F::MODEL_SEEDS {
        f.fresh();
        let engine = open(f, &MODEL_SCHEMA, SchemaState::Created);
        let reopen = |engine| {
            f.close(engine);
            open(f, &MODEL_SCHEMA, SchemaState::Current)
        };
        let ops = model::decode_ops(&model::pseudo_random_bytes(seed, 4096));
        reopens += ops.iter().filter(|op| matches!(op, model::Op::Reopen)).count();
        let hooks = Hooks {
            reopen: Some(&reopen),
            ..Hooks::default()
        };
        let engine = model::check_with(engine, &ops, hooks)
            .unwrap_or_else(|e| panic!("kv_model seed {seed}: {e}"));
        f.close(engine);
    }
    assert!(reopens as u64 > F::MODEL_SEEDS, "the seeds exercise reopening ({reopens})");
}

/// [`model::check_delete_range`] on a fresh database.
pub fn delete_range<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &MODEL_SCHEMA, SchemaState::Created);
    model::check_delete_range(&engine);
    f.close(engine);
}

/// A new database is stamped; an older version opens as [`SchemaState::Older`] and a current one
/// as [`SchemaState::Current`]; a newer version and another database's record are refused. None
/// of the opens writes: each refused or older database still opens as what it was.
pub fn schema_versions<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_V1, SchemaState::Created);
    put(&engine, b"k", b"v1 data", Commit::Durable).unwrap();
    f.close(engine);
    f.close(open(f, &CONTRACT_SCHEMA, SchemaState::Older(1)));
    let engine = open(f, &CONTRACT_V1, SchemaState::Current);
    assert_eq!(get(&engine, b"k").as_deref(), Some(&b"v1 data"[..]));
    f.close(engine);

    f.fresh();
    f.close(open(f, &CONTRACT_V3, SchemaState::Created));
    match f.open(&CONTRACT_SCHEMA).map(drop) {
        Err(StoreError::SchemaTooNew {
            found: 3,
            supported: 2,
            ..
        }) => {}
        other => panic!("a newer version opens as {other:?}"),
    }
    f.close(open(f, &CONTRACT_V3, SchemaState::Current));

    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    put(&engine, b"k", b"ours", Commit::Durable).unwrap();
    f.close(engine);
    match f.open(&OTHER_SCHEMA).map(drop) {
        Err(StoreError::UnknownDatabase {
            ..
        }) => {}
        other => panic!("another database's record opens as {other:?}"),
    }
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Current);
    assert_eq!(get(&engine, b"k").as_deref(), Some(&b"ours"[..]));
    f.close(engine);
}

/// After a failed barrier, every write and barrier is refused with [`ErrorKind::ReadOnly`] (its
/// message naming the first failure) until a reopen, also once the cause is gone; reads keep
/// working; a reopen keeps every committed batch and writes again (A1 §3.5.6).
pub fn fail_stop<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    put(&engine, b"durable", b"1", Commit::Durable).unwrap();
    put(&engine, b"buffered", b"2", Commit::Buffered).unwrap();

    let restore = f.fail_next_barrier(&engine);
    let failed = engine.flush_memtables();
    restore();
    let err = failed.expect_err("the armed barrier fails");
    let original = err.kind().unwrap_or_else(|| panic!("an engine error: {err}"));
    assert_ne!(original, ErrorKind::ReadOnly, "the first error is reported as itself: {err}");

    let refusals = [
        ("buffered write", put(&engine, b"after", b"x", Commit::Buffered)),
        ("durable write", put(&engine, b"after", b"x", Commit::Durable)),
        ("sync_wal", engine.sync_wal()),
        ("flush_memtables", engine.flush_memtables()),
    ];
    for (what, result) in refusals {
        let err = result.expect_err(what);
        assert_eq!(err.kind(), Some(ErrorKind::ReadOnly), "{what}: {err}");
        assert!(err.to_string().contains(&format!("{original:?}")), "{what}: {err}");
    }
    assert_eq!(get(&engine, b"durable").as_deref(), Some(&b"1"[..]));
    assert_eq!(get(&engine, b"buffered").as_deref(), Some(&b"2"[..]));
    assert_eq!(get(&engine, b"after"), None, "a refused write is not visible");

    f.close(engine);
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Current);
    assert_eq!(get(&engine, b"durable").as_deref(), Some(&b"1"[..]));
    assert_eq!(get(&engine, b"buffered").as_deref(), Some(&b"2"[..]));
    put(&engine, b"after reopen", b"3", Commit::Durable).unwrap();
    f.close(engine);
}

/// A buffered batch of puts, a delete and a range delete over two column families is there in
/// full after a reopen; a batch never written leaves nothing.
pub fn batch_atomic_across_reopen<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    let mut setup = engine.batch();
    setup.put(point(), b"gone", b"x");
    for k in [0x10u8, 0x18, 0x20] {
        setup.put(ordered(), &[k], b"x");
    }
    engine.write(setup, Commit::Durable).unwrap();

    let mut batch = engine.batch();
    batch.put(point(), b"p", b"1");
    batch.delete(point(), b"gone");
    batch.delete_range(ordered(), &[0x10], &[0x20]);
    batch.put(ordered(), &[0x30], b"2");
    engine.write(batch, Commit::Buffered).unwrap();
    let mut dropped = engine.batch();
    dropped.put(point(), b"never", b"x");
    drop(dropped);

    f.close(engine);
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Current);
    let entry = |k: &[u8], v: &[u8]| (k.to_vec(), v.to_vec());
    assert_eq!(scan(&engine, point()), [entry(b"p", b"1")]);
    assert_eq!(scan(&engine, ordered()), [entry(&[0x20], b"x"), entry(&[0x30], b"2")]);
    f.close(engine);
}

/// SB0 §9.4: write A, take a snapshot, commit B, commit C (durable): the snapshot reads A, by
/// point read, batched read and scan, while the engine reads B, then C. A later snapshot reads C.
/// The snapshot is first read after B commits, so an engine whose view is fixed at the first read
/// instead of at [`KvEngine::snapshot`] (SQLite's deferred transaction, DEC-104 M6) fails.
pub fn snapshot_interleaving<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    let mut a = engine.batch();
    a.put(point(), b"k", b"A");
    a.put(point(), b"only A", b"A");
    engine.write(a, Commit::Buffered).unwrap();

    let snap = engine.snapshot();
    let frozen = |snap: &<F::Engine as KvEngine>::Snapshot<'_>| {
        assert_eq!(get(snap, b"k").as_deref(), Some(&b"A"[..]));
        assert_eq!(get(snap, b"new in B"), None);
        let keys: [&[u8]; 3] = [b"k", b"new in B", b"only A"];
        let got = snap.multi_get(point(), &keys).expect("reads work");
        assert_eq!(got, [Some(b"A".to_vec()), None, Some(b"A".to_vec())]);
        let entry = |k: &[u8]| (k.to_vec(), b"A".to_vec());
        assert_eq!(scan(snap, point()), [entry(b"k"), entry(b"only A")]);
    };

    let mut b = engine.batch();
    b.put(point(), b"k", b"B");
    b.put(point(), b"new in B", b"B");
    engine.write(b, Commit::Buffered).unwrap();
    frozen(&snap);
    assert_eq!(get(&engine, b"k").as_deref(), Some(&b"B"[..]));

    let mut c = engine.batch();
    c.put(point(), b"k", b"C");
    c.delete(point(), b"only A");
    engine.write(c, Commit::Durable).unwrap();
    frozen(&snap);
    assert_eq!(get(&engine, b"k").as_deref(), Some(&b"C"[..]));
    assert_eq!(get(&engine, b"only A"), None);
    drop(snap);

    let later = engine.snapshot();
    assert_eq!(get(&later, b"k").as_deref(), Some(&b"C"[..]));
    assert_eq!(get(&later, b"new in B").as_deref(), Some(&b"B"[..]));
    drop(later);
    f.close(engine);
}

/// Each round reads through a snapshot and drops it, commits, and reads the commit through the
/// engine and a new snapshot: a reused read connection must not keep an old read view.
pub fn reads_after_snapshot_reuse<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    for round in 0..4u8 {
        let snap = engine.snapshot();
        let before = (round > 0).then(|| vec![round - 1]);
        assert_eq!(get(&snap, b"k"), before);
        drop(snap);
        let commit = if round % 2 == 0 {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        put(&engine, b"k", &[round], commit).unwrap();
        assert_eq!(get(&engine, b"k"), Some(vec![round]));
        assert_eq!(get(&engine.snapshot(), b"k"), Some(vec![round]));
    }
    f.close(engine);
}

/// [`KvRead::iter_prefix_from`] equals the prefix scan with the keys below `start` dropped, for
/// starts below, inside, between and above the prefix's keys, through the engine and a snapshot.
pub fn prefix_seek<F: EngineFactory>(f: &F) {
    f.fresh();
    let engine = open(f, &CONTRACT_SCHEMA, SchemaState::Created);
    let mut batch = engine.batch();
    for key in [&b"a"[..], b"b", b"b\x00", b"b\x01\x05", b"b\x02", b"b\xff", b"c"] {
        batch.put(ordered(), key, key);
    }
    engine.write(batch, Commit::Durable).unwrap();
    check_seek(&engine);
    let snap = engine.snapshot();
    check_seek(&snap);
    drop(snap);
    f.close(engine);
}

fn check_seek(db: &impl KvRead) {
    let keys = |it: &mut dyn Iterator<Item = Result<(Vec<u8>, Vec<u8>)>>| -> Vec<Vec<u8>> {
        it.map(|r| r.expect("scans work").0).collect()
    };
    let starts = [&b""[..], b"a", b"b", b"b\x01", b"b\x01\x05", b"b\x01\x06", b"b\xff\x00", b"c"];
    for prefix in [&b""[..], b"b", b"b\x01"] {
        let full = keys(&mut db.iter_prefix(ordered(), prefix));
        for start in starts {
            let want: Vec<Vec<u8>> =
                full.iter().filter(|key| key.as_slice() >= start).cloned().collect();
            let got = keys(&mut db.iter_prefix_from(ordered(), prefix, start));
            assert_eq!(got, want, "prefix {prefix:?} from {start:?}");
        }
    }
}

/// One `#[test]` per contract check, on the [`EngineFactory`] `$factory` builds (once per test).
#[macro_export]
macro_rules! engine_contract_tests {
    ($factory:expr) => {
        $crate::engine_contract_tests!(@tests $factory;
            model_differential, delete_range, schema_versions, fail_stop,
            batch_atomic_across_reopen, snapshot_interleaving, reads_after_snapshot_reuse,
            prefix_seek);
    };
    (@tests $factory:expr; $($check:ident),*) => {
        $(
            #[test]
            fn $check() {
                $crate::contract::$check(&$factory);
            }
        )*
    };
}

/// [`MemoryEngine`]'s factory: a closed database is its committed maps.
#[derive(Default)]
pub struct MemoryFactory {
    closed: Mutex<Option<Vec<Arc<Map>>>>,
}

impl EngineFactory for MemoryFactory {
    type Engine = MemoryEngine;

    const MODEL_SEEDS: u64 = 200;

    fn fresh(&self) {
        *self.closed.lock().expect("unpoisoned") = None;
    }

    fn open(&self, schema: &'static DbSchema) -> Result<(MemoryEngine, SchemaState)> {
        match self.closed.lock().expect("unpoisoned").clone() {
            None => Ok((MemoryEngine::new(schema), SchemaState::Created)),
            Some(maps) => MemoryEngine::reopened(schema, maps),
        }
    }

    fn close(&self, engine: MemoryEngine) {
        *self.closed.lock().expect("unpoisoned") = Some(engine.maps());
    }

    fn fail_next_barrier(&self, engine: &MemoryEngine) -> Box<dyn FnOnce() + '_> {
        engine.inject_failure(ErrorKind::Io);
        Box::new(|| {})
    }
}

#[cfg(test)]
mod memory {
    crate::engine_contract_tests!(super::MemoryFactory::default());
}
