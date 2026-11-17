//! SB0 L5: the engine starts no threads. Its own test binary, so no other test's threads count.
#![cfg(target_os = "linux")]

use dash_kv::contract::CONTRACT_SCHEMA;
use dash_kv::{Commit, KvBatch, KvEngine, KvRead};
use dash_kv_sqlite::{SqliteConfig, SqliteEngine};

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task").unwrap().count()
}

#[test]
fn sqlite_starts_no_threads() {
    let dir = tempfile::tempdir().unwrap();
    let before = threads();
    let (engine, _) =
        SqliteEngine::open(dir.path(), &CONTRACT_SCHEMA, &SqliteConfig::default()).unwrap();
    let cf = CONTRACT_SCHEMA.cf("ordered").unwrap();
    for round in 0..20u32 {
        let mut batch = engine.batch();
        for i in 0..500u32 {
            batch.put(cf, &[round.to_be_bytes(), i.to_be_bytes()].concat(), &[7; 64]);
        }
        engine.write(batch, Commit::Buffered).unwrap();
    }
    let snap = engine.snapshot();
    assert_eq!(snap.iter_prefix(cf, &[]).count(), 10_000);
    assert_eq!(engine.iter_prefix(cf, &[]).count(), 10_000);
    engine.flush_memtables().unwrap();
    engine.approximate_size(cf).unwrap();
    assert_eq!(threads(), before);
    drop(snap);
    drop(engine);
    assert_eq!(threads(), before);
}
