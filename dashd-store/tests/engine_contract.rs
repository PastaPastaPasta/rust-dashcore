//! The engine contract suite (`dash_kv::contract`) on [`RocksEngine`].

#![cfg(feature = "rocksdb")]

use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use dash_kv::contract::EngineFactory;
use dashd_store::{DbSchema, Result, RocksConfig, RocksEngine, SchemaState};

/// Each database is a directory of its own under one temporary root.
struct Rocks {
    root: tempfile::TempDir,
    db: Cell<u32>,
}

impl Rocks {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().unwrap(),
            db: Cell::new(0),
        }
    }

    fn dir(&self) -> PathBuf {
        self.root.path().join(self.db.get().to_string())
    }
}

fn chmod(dir: &Path, mode: u32) {
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
}

impl EngineFactory for Rocks {
    type Engine = RocksEngine;

    /// Every reopen is a RocksDB open (tens of milliseconds); `rocks.rs`'s own `model_check`
    /// runs the same number.
    const MODEL_SEEDS: u64 = 6;

    fn fresh(&self) {
        self.db.set(self.db.get() + 1);
    }

    fn open(&self, schema: &'static DbSchema) -> Result<(RocksEngine, SchemaState)> {
        RocksEngine::open(&self.dir(), schema, &RocksConfig::default())
    }

    /// The flush cannot create its table file in a read-only directory (as `fail_stop.rs`; not
    /// as root).
    fn fail_next_barrier(&self, _engine: &RocksEngine) -> Box<dyn FnOnce() + '_> {
        let dir = self.dir();
        chmod(&dir, 0o555);
        Box::new(move || chmod(&dir, 0o755))
    }
}

dash_kv::engine_contract_tests!(Rocks::new());
