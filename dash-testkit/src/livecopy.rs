//! CH5-5's reopen oracle, offline half (§6.3.2 step 1): a copied regtest datadir's persistent rows,
//! never a replay. [`rows`](crate::livecopy::rows) opens a second copy of the chainstate as
//! recovery does (`ChainstateOpen::open`, through C0's
//! [`inspect`](crate::crash::inspect::inspect)), runs C0's decision-key and I1 checks, refuses a
//! marker (nothing may need replaying), and fingerprints each of the 11 column families: its row
//! count and the SHA-256 over every row, key order, as `len(key) ‖ key ‖ len(value) ‖ value` with
//! little-endian u64 lengths.
//!
//! [`read_cf`](crate::livecopy::read_cf) and [`edit_rows`](crate::livecopy::edit_rows) read and
//! damage a copy's rows offline, with `RocksTuning::default()`; each opens and closes its own
//! writer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashd_chainstate::caches::{CacheSizes, IndexArgs};
use dashd_chainstate::chain::RecordingAbort;
use dashd_chainstate::store::{CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine};
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};
use dashd_store::{Commit, KvBatch as _, KvRead as _, RocksEngine};

use crate::crash::inspect::{Keys, inspect};

/// One column family's fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CfRows {
    pub cf: &'static str,
    pub count: u64,
    pub sha256: [u8; 32],
}

/// A copy's rows: its durable tip (display hex) and the 11 fingerprints in schema order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rows {
    pub tip: Option<String>,
    pub cfs: Vec<CfRows>,
}

impl Rows {
    /// The fingerprint of `cf`.
    ///
    /// # Panics
    ///
    /// `cf` is no chainstate column family.
    pub fn cf(&self, cf: &str) -> &CfRows {
        self.cfs.iter().find(|r| r.cf == cf).unwrap_or_else(|| panic!("no column family {cf}"))
    }

    /// The column families whose fingerprints differ from `other`'s.
    pub fn differing(&self, other: &Rows) -> Vec<&'static str> {
        self.cfs.iter().zip(&other.cfs).filter(|(a, b)| a != b).map(|(a, _)| a.cf).collect()
    }
}

/// The chainstate and block files of `datadir`'s regtest network, copied to a new directory so
/// the offline open (which replays the WAL) leaves `datadir` as it was.
fn second_copy(datadir: &Path) -> Result<tempfile::TempDir, String> {
    let dir = tempfile::Builder::new()
        .prefix("livecopy-rows-")
        .tempdir()
        .map_err(|e| format!("a directory for the second copy: {e}"))?;
    for sub in ["chainstate", "blocks"] {
        let from = datadir.join("regtest").join(sub);
        if from.exists() {
            copy_dir(&from, &dir.path().join("regtest").join(sub))?;
        }
    }
    Ok(dir)
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let entries = std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().map_err(|e| format!("{}: {e}", src.display()))?.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(())
}

/// The chainstate engine of the regtest datadir `datadir`, as C0 opens it.
fn open(datadir: &Path) -> Result<GuardedEngine<RocksEngine>, String> {
    let path: PathBuf = datadir.join("regtest").join("chainstate");
    let sizes = CacheSizes::calculate(4, &IndexArgs::default());
    let (engine, _) = RocksTuning::default()
        .open_chainstate(&path, &sizes, 1 << 20)
        .map_err(|e| format!("opening the chainstate: {e}"))?;
    Ok(GuardedEngine::new(engine, GlobalFailStop::default(), Arc::new(RecordingAbort::default())))
}

/// Step 1 on the copy `datadir` (§6.3.2): C0's checks on a second copy, no marker, and the 11
/// fingerprints.
///
/// # Errors
///
/// The second copy could not be made or opened, a C0 check failed, or a marker is set.
pub fn rows(datadir: &Path) -> Result<Rows, String> {
    let second = second_copy(datadir)?;
    let inspection = inspect(second.path(), None)?;
    if !inspection.failures.is_empty() {
        return Err(format!("C0: {}", inspection.failures.join("; ")));
    }
    let tip = match inspection.keys {
        Keys::Empty => None,
        Keys::Tip(tip) => Some(tip),
        Keys::Marker {
            new,
            old,
        } => return Err(format!("a marker [{new}, {old:?}]: the copy needs a replay")),
    };
    let engine = open(second.path())?;
    let mut cfs = Vec::new();
    for (cf, spec) in CHAINSTATE_SCHEMA.all_cfs().zip(CHAINSTATE_SCHEMA.cfs) {
        let (mut count, mut bytes) = (0, Vec::new());
        for row in engine.iter_prefix(cf, &[]) {
            let (key, value) = row.map_err(|e| format!("reading {}: {e}", spec.name))?;
            for part in [&key, &value] {
                bytes.extend((part.len() as u64).to_le_bytes());
                bytes.extend(part);
            }
            count += 1;
        }
        cfs.push(CfRows {
            cf: spec.name,
            count,
            sha256: dash_consensus::hash::sha256(&bytes),
        });
    }
    Ok(Rows {
        tip,
        cfs,
    })
}

/// A row: its key and value.
pub type Row = (Vec<u8>, Vec<u8>);

/// Every row of `cf` in the copy `datadir`, key order, read from a second copy.
///
/// # Errors
///
/// The second copy could not be made or opened, `cf` is no chainstate column family, or a read
/// failed.
pub fn read_cf(datadir: &Path, cf: &str) -> Result<Vec<Row>, String> {
    let second = second_copy(datadir)?;
    let engine = open(second.path())?;
    let id = CHAINSTATE_SCHEMA.cf(cf).ok_or_else(|| format!("no column family {cf}"))?;
    engine.iter_prefix(id, &[]).map(|r| r.map_err(|e| format!("reading {cf}: {e}"))).collect()
}

/// One offline edit: `(cf, key, Some(value))` writes the row, `(cf, key, None)` deletes it.
pub type Edit<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

/// Applies `edits` to the copy `datadir` itself in one synced batch, then closes the writer.
///
/// # Errors
///
/// The chainstate could not be opened, a column family is unknown, or the write failed.
pub fn edit_rows(datadir: &Path, edits: &[Edit<'_>]) -> Result<(), String> {
    let engine = open(datadir)?;
    let mut batch = engine.batch();
    for &(cf, key, value) in edits {
        let id = CHAINSTATE_SCHEMA.cf(cf).ok_or_else(|| format!("no column family {cf}"))?;
        match value {
            Some(value) => batch.put(id, key, value),
            None => batch.delete(id, key),
        }
    }
    engine.write(batch, Commit::Durable).map_err(|e| format!("the edit: {e}"))
}
