//! `kv_model`: the RocksDB and in-memory engines against the reference model (A1 §2.8): RocksDB
//! including closing and reopening the database, the in-memory engine including simulated power
//! loss. The key-value layer does not depend on the Core release.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use dashd_store::model::{self, Hooks, MODEL_SCHEMA};
use dashd_store::{MemoryEngine, RocksConfig, RocksEngine, SchemaState};
use node_l1::Divergence;

/// Reopening costs a few milliseconds; more per input would hit the fuzzer's time limit.
const MAX_REOPENS: usize = 4;
const DIR_PREFIX: &str = "dashd_store_kv_model_";

/// This process's database directory, `<tmp>/dashd_store_kv_model_<pid>`, where `<tmp>` is
/// `/dev/shm` when it exists (RocksDB syncs on every input; a database on a busy disk slows the
/// target several-fold) and `$TMPDIR` otherwise. The inputs are public (a public-route target).
/// On first use, the directories of dead processes (inputs killed on a timeout) are removed.
fn db_dir() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let shm = Path::new("/dev/shm");
        let tmp = if shm.is_dir() {
            shm.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        for entry in std::fs::read_dir(&tmp).into_iter().flatten().flatten() {
            let name = entry.file_name();
            let pid = name.to_str().and_then(|n| n.strip_prefix(DIR_PREFIX));
            if pid.is_some_and(|pid| !Path::new("/proc").join(pid).exists()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        tmp.join(format!("{DIR_PREFIX}{}", std::process::id()))
    })
}

fn diverged(field: &'static str) -> Divergence {
    Divergence {
        field,
    }
}

/// A failure to open the database is the environment's, not a finding: it panics, and the run
/// reports a crash.
fn open(dir: &Path) -> (RocksEngine, SchemaState) {
    RocksEngine::open(dir, &MODEL_SCHEMA, &RocksConfig::default())
        .unwrap_or_else(|_| panic!("kv_model: cannot open the database"))
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let ops = model::decode_ops(data);
    let power_loss = |e: &MemoryEngine, keep: usize| e.simulate_power_loss(keep);
    let hooks = Hooks {
        power_loss: Some(&power_loss),
        ..Hooks::default()
    };
    model::check_with(MemoryEngine::new(&MODEL_SCHEMA), &ops, hooks)
        .map_err(|_| diverged("memory engine"))?;

    // A fresh database per input, so that a failure reproduces from the input alone.
    let dir = db_dir();
    let _ = std::fs::remove_dir_all(dir);
    let (engine, state) = open(dir);
    if state != SchemaState::Created {
        return Err(diverged("schema state on create"));
    }
    let reopens = Cell::new(0);
    let reopened_state = Cell::new(SchemaState::Current);
    let reopen = |engine: RocksEngine| {
        if reopens.replace(reopens.get() + 1) >= MAX_REOPENS {
            return engine;
        }
        drop(engine);
        let (engine, state) = open(dir);
        if state != SchemaState::Current {
            reopened_state.set(state);
        }
        engine
    };
    let hooks = Hooks {
        reopen: Some(&reopen),
        ..Hooks::default()
    };
    // Closes the database (dropping the returned engine) before removing it.
    let checked = model::check_with(engine, &ops, hooks).map(drop);
    let _ = std::fs::remove_dir_all(dir);
    if reopened_state.get() != SchemaState::Current {
        return Err(diverged("schema state on reopen"));
    }
    checked.map_err(|_| diverged("rocksdb engine"))
}
