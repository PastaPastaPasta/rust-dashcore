//! P5: `import-report.json` and the `Reported` record (§2.3, OD-CP-296 item 3).
use super::fs::ImportFs;
use super::journal::{Intent, Method, Record, RecordBody, ReportedStatus};
use super::{ImportError, Journal, Phase, PlacementPolicy, SourceMode};
use serde_json::json;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipRelation {
    Equal,
    Descendant,
    Ancestor,
    Mismatch,
}
impl TipRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Equal => "equal",
            Self::Descendant => "descendant",
            Self::Ancestor => "ancestor",
            Self::Mismatch => "mismatch",
        }
    }
}

/// What the node's start reported, checked in the slot before any stop half
/// runs (§2.3 P5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartOutcome {
    pub abc_ok: bool,
    pub shutdown: bool,
    pub abort: bool,
    pub relation: TipRelation,
    pub destination_tip: Option<[u8; 32]>,
}
impl StartOutcome {
    /// The reason the import is incomplete, if it is.
    pub fn incomplete_reason(&self) -> Option<&'static str> {
        if self.abort {
            Some("abort")
        } else if self.shutdown {
            Some("shutdown requested")
        } else if !self.abc_ok {
            Some("ActivateBestChain failed")
        } else if self.relation == TipRelation::Ancestor {
            Some("tip is an ancestor of the source's best block")
        } else {
            None
        }
    }
}

pub const NOT_IMPORTED: [&str; 7] =
    ["rev", "chainstate", "evodb", "llmq", "indexes", "flat files", "wallets"];

pub const SNAPSHOT_GUARANTEE: &str = "staged bytes equal the source as hashed at P0 (copies re-hashed at P1). Immutability between and after is the operator's assertion; no inode is shared.";
pub const LOCK_GUARANTEE: &str = "staged bytes equal the source as hashed at P0 (copies re-hashed at P1), and the source was unchanged at P2 under its lock; every file is a reflink or a copy, so no source inode is shared.";
pub const HARDLINK_GUARANTEE: &str = "Core's normal operation never rewrites files below 'l'. A later source -reindex may append to them, or truncate them, after the last record it accepts; that record can lie before records this import indexed. A changed record is detected on read by its hash. Files at or above 'l', and any file whose link failed, are reflinks or copies.";

pub fn guarantee(mode: SourceMode, placement: PlacementPolicy) -> &'static str {
    match (mode, placement) {
        (SourceMode::Snapshot, _) => SNAPSHOT_GUARANTEE,
        (SourceMode::Lock, PlacementPolicy::Reflink) => LOCK_GUARANTEE,
        (SourceMode::Lock, PlacementPolicy::Hardlink) => HARDLINK_GUARANTEE,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportReport {
    /// `complete`, `mismatch` or `incomplete`.
    pub status: String,
    pub reason: Option<String>,
    pub path: PathBuf,
    pub json: serde_json::Value,
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A path as lossy text plus its raw bytes in hex, so a non-UTF-8 path is
/// recorded exactly and never fails serialization.
fn path_fields(key: &str, path: &Path, out: &mut serde_json::Map<String, serde_json::Value>) {
    out.insert(key.into(), path.to_string_lossy().into());
    out.insert(format!("{key}_hex"), hex(path.as_os_str().as_bytes()).into());
}

pub fn report_path(journal: &Journal) -> PathBuf {
    journal
        .path
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."))
        .join("import-report.json")
}

fn build(
    j: &Journal,
    intent: &Intent,
    out: &StartOutcome,
    status: &str,
    reason: Option<&str>,
) -> serde_json::Value {
    let methods = j.methods().unwrap_or_default();
    let mut files = Vec::new();
    let (mut count, mut bytes) = (0u64, 0u64);
    let mut by_method = serde_json::Map::new();
    for m in methods {
        let size = intent.m0.get(&Path::new("blocks").join(&m.name)).map_or(0, |e| e.size);
        count += 1;
        bytes += size;
        let slot =
            by_method.entry(m.method.as_str()).or_insert_with(|| json!({"files": 0, "bytes": 0}));
        slot["files"] = json!(slot["files"].as_u64().unwrap_or(0) + 1);
        slot["bytes"] = json!(slot["bytes"].as_u64().unwrap_or(0) + size);
        let mut row = serde_json::Map::new();
        path_fields("file", &m.name, &mut row);
        row.insert("method".into(), m.method.as_str().into());
        row.insert("bytes".into(), size.into());
        row.insert(
            "reflink_errno".into(),
            (m.method != Method::Reflink).then_some(m.reflink_errno).into(),
        );
        row.insert("link_errno".into(), (m.link_errno != 0).then_some(m.link_errno).into());
        files.push(serde_json::Value::Object(row));
    }
    let mut timings = serde_json::Map::new();
    for pair in j.records().windows(2) {
        timings.insert(
            format!("{:?}", pair[1].phase()),
            json!(pair[1].at_ms.saturating_sub(pair[0].at_ms)),
        );
    }
    let mut value = json!({
        "status": status,
        "reason": reason,
        "source_dev": intent.source.dev,
        "source_ino": intent.source.ino,
        "mode": match intent.mode { SourceMode::Lock => "lock", SourceMode::Snapshot => "snapshot" },
        "placement": match intent.placement { PlacementPolicy::Reflink => "reflink", PlacementPolicy::Hardlink => "hardlink" },
        "files": files,
        "file_count": count,
        "bytes": bytes,
        "by_method": by_method,
        "source_best_block": intent.best_block.map(|b| hex(&b)),
        "source_last_file": intent.last_file,
        "destination_tip": out.destination_tip.map(|b| hex(&b)),
        "relation": out.relation.as_str(),
        "manifest_m0": hex(&intent.m0.digest),
        "manifest_m1": j.m1_digest().map(|d| hex(&d)),
        "phase_ms": timings,
        "not_imported": NOT_IMPORTED,
        "guarantee": guarantee(intent.mode, intent.placement),
    });
    if let serde_json::Value::Object(map) = &mut value {
        path_fields("source", &intent.source.path, map);
        path_fields("datadir", &intent.datadir, map);
    }
    value
}

fn write(path: &Path, value: &serde_json::Value, fs: &dyn ImportFs) -> Result<(), ImportError> {
    let tmp = path.with_extension("json.tmp");
    fs.remove(&tmp)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| ImportError::Io(e.into()))?;
    fs.append(&tmp, &bytes)?;
    fs.sync_data(&tmp)?;
    fs.rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        fs.sync_dir(parent)?;
    }
    Ok(())
}

/// The refusal a `Reported` journal gives: "already imported", or the stored
/// mismatch's error line.
pub fn reported_refusal(j: &Journal) -> ImportError {
    match j.reported() {
        Some(ReportedStatus::Mismatch) => ImportError::TipMismatch {
            report: report_path(j),
        },
        _ => ImportError::AlreadyImported {
            source_dir: j.intent().map(|i| i.datadir.clone()).unwrap_or_default(),
            status: j.reported().unwrap_or(ReportedStatus::Complete).as_str().into(),
        },
    }
}

/// Runs P5 on a journal at `ReindexArmed`. An incomplete start (abort,
/// shutdown, ABC failure, `ancestor`) writes the report with status
/// `incomplete` and appends nothing; `equal` and `descendant` append
/// `Reported`; `mismatch` appends `Reported` with that status and returns
/// its error line.
pub fn report(
    j: &mut Journal,
    out: StartOutcome,
    fs: &dyn ImportFs,
) -> Result<ImportReport, ImportError> {
    if j.last() == Some(Phase::Reported) {
        return Err(reported_refusal(j));
    }
    if j.last() != Some(Phase::ReindexArmed) {
        return Err(ImportError::PhaseOrder {
            last: j.last(),
            got: Phase::Reported,
        });
    }
    super::check_destination_layout(
        j.path.parent().and_then(Path::parent).unwrap_or(Path::new(".")),
        fs,
    )?;
    j.repair(fs)?;
    let intent = j.intent().expect("a journal at ReindexArmed has an Intent record").clone();
    let path = report_path(j);
    let reason = out.incomplete_reason();
    let stored = match (reason, out.relation) {
        (Some(_), _) => None,
        (None, TipRelation::Mismatch) => Some(ReportedStatus::Mismatch),
        (None, _) => Some(ReportedStatus::Complete),
    };
    let status = stored.map_or("incomplete", ReportedStatus::as_str);
    let value = build(j, &intent, &out, status, reason);
    write(&path, &value, fs)?;
    match stored {
        None => {
            eprintln!("Core datadir import: incomplete ({})", reason.unwrap_or_default());
        }
        Some(s) => {
            j.append(Record::new(RecordBody::Reported(s), fs), fs)?;
            if s == ReportedStatus::Mismatch {
                return Err(ImportError::TipMismatch {
                    report: path,
                });
            }
        }
    }
    Ok(ImportReport {
        status: status.into(),
        reason: reason.map(str::to_owned),
        path,
        json: value,
    })
}
