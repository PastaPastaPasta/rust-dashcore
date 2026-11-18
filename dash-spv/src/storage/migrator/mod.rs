mod v1;
mod v2;
mod v3;

use std::io;
use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::{StorageError, StorageResult};
use crate::storage::io::atomic_write;
use v1::V1Migrator;
use v2::V2Migrator;
pub(crate) use v3::STORE_DIR;
use v3::{V2to4Migrator, V3Migrator};

/// SB14 D11: version 4, the hybrid header layout. Version 3 is SB13's unreleased store or
/// upstream's version 2 layout with the engine snapshot (#1221); `V3Migrator` tells them apart.
pub const CURRENT_VERSION: u32 = 4;

/// The layout this client writes at [`CURRENT_VERSION`], named in `version.json` (DEC-811):
/// upstream dash-spv numbers its own layouts, and version 3 already meant two of them. A
/// current-version `version.json` written before the field existed is this client's; both opens
/// name the layout in it ([`stamp_layout`]), so that it is told apart from an upstream version
/// 4, which would name none (Opus-h r1 m-1).
const LAYOUT: &str = "hybrid";

#[derive(Deserialize, Serialize)]
struct Version {
    version: u32,
    /// D19's intent: a clear started and must finish before the storage is used.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    clearing: bool,
    /// [`LAYOUT`] at the current version; absent before it, and in what this client wrote before
    /// it named the layout, which is this client's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
    /// D52: the generation of the store's current clear epoch.
    #[serde(default, skip_serializing_if = "is_zero")]
    generation: u64,
    /// D52: a confirmed on-disk header mismatch awaiting a clear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mismatch: Option<u64>,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl Version {
    fn new(version: u32) -> Self {
        Self {
            version,
            clearing: false,
            layout: (version == CURRENT_VERSION).then(|| LAYOUT.to_owned()),
            generation: 0,
            mismatch: None,
        }
    }

    /// Why a `version.json` naming another layout is refused, with nothing changed.
    fn foreign_layout(&self, storage_path: &Path) -> Option<String> {
        let layout = self.layout.as_deref().filter(|l| *l != LAYOUT)?;
        Some(format!(
            "{} holds storage layout {layout:?} at version {}, not this client's {LAYOUT:?}",
            storage_path.display(),
            self.version
        ))
    }
}

/// What a clear keeps at the storage path (D19): the logs and the version-2 data a migration
/// set aside (D39), and `version.json`, which holds the intent until the clear is done.
pub(crate) fn kept_by_clear(name: &std::ffi::OsStr) -> bool {
    name == "logs" || name == v3::UNMIGRATED_DIR || name == "version.json"
}

#[async_trait]
trait Migrator {
    /// The version this migration leaves (D11 (a)): V2to4 skips 3.
    fn target(&self) -> u32;
    async fn apply_migration(&self, storage_path: &Path) -> Result<(), MigratorError>;
}

#[derive(Debug, thiserror::Error)]
enum MigratorError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Corruption(String),
    /// The storage holds something no migration may delete: the open fails, nothing deleted.
    #[error("{0}")]
    Refused(String),
    // We cannot recover from this, errors in the storage system itself
    // E.g. atommic_write failing
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<MigratorError> for StorageError {
    fn from(e: MigratorError) -> Self {
        match e {
            MigratorError::Io(e) => StorageError::Io(e),
            MigratorError::Serde(e) => StorageError::Corruption(format!(
                "Storage is corrupted and cannot be recovered: {}",
                e
            )),
            MigratorError::Corruption(e) => StorageError::Corruption(e),
            MigratorError::Refused(e) => StorageError::InvalidArgument(e),
            MigratorError::Storage(e) => e,
        }
    }
}

pub struct StorageMigrator;

impl StorageMigrator {
    /// Marks the storage path as the current version: after a `clear`, which leaves an empty
    /// store.
    #[cfg(test)]
    pub async fn write_current_version(storage_path: &Path) -> StorageResult<()> {
        Self::write_version(storage_path, false).await.map(|_| ())
    }

    /// D19 step 1: `version.json` with the clearing intent.
    pub async fn write_clearing(storage_path: &Path) -> StorageResult<()> {
        Self::write_version(storage_path, true).await.map(|_| ())
    }

    async fn write_version(storage_path: &Path, clearing: bool) -> StorageResult<u64> {
        let path = storage_path.join("version.json");
        let mut version = read_version_struct(&path).await?;
        version.version = CURRENT_VERSION;
        version.clearing = clearing;
        version.layout = Some(LAYOUT.to_owned());
        let generation = version.generation;
        let bytes =
            version_json(&version).map_err(|e| StorageError::Serialization(e.to_string()))?;
        atomic_write(&path, bytes.as_bytes()).await?;
        Ok(generation)
    }

    /// D19's clear in the restart-arm form, with no store open: the intent, then `tip` unlinked
    /// and `block_headers/` synced, then `spv/`, then everything else but what
    /// [`kept_by_clear`] keeps, then the version without the intent.
    pub async fn clear(storage_path: &Path) -> StorageResult<u64> {
        Self::write_clearing(storage_path).await?;
        Self::finish_clear(storage_path).await
    }

    /// D19 steps 2–5 (an open that finds the intent resumes here).
    async fn finish_clear(storage_path: &Path) -> StorageResult<u64> {
        let headers = storage_path.join(crate::storage::store::HEADERS_DIR);
        match tokio::fs::remove_file(headers.join(crate::storage::store::hdr::TIP_FILE)).await {
            Ok(()) => crate::storage::io::sync_dir(&headers)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        match tokio::fs::remove_dir_all(storage_path.join(STORE_DIR)).await {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        let mut entries = tokio::fs::read_dir(storage_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            if kept_by_clear(&entry.file_name()) {
                continue;
            }
            if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(entry.path()).await?;
            } else {
                tokio::fs::remove_file(entry.path()).await?;
            }
        }
        let path = storage_path.join("version.json");
        let mut version = read_version_struct(&path).await?;
        version.version = CURRENT_VERSION;
        version.clearing = false;
        version.layout = Some(LAYOUT.to_owned());
        version.generation = version.generation.wrapping_add(1);
        version.mismatch = None;
        let generation = version.generation;
        let bytes =
            version_json(&version).map_err(|e| StorageError::Serialization(e.to_string()))?;
        atomic_write(&path, bytes.as_bytes()).await?;
        Ok(generation)
    }

    pub async fn migrate(storage_path: &Path) -> StorageResult<u64> {
        // `version.json` is read once (D34): the clearing intent and the version.
        let mut stored = read_version(storage_path).await?;
        let parsed = stored.as_deref().and_then(|b| serde_json::from_slice::<Version>(b).ok());
        // A newer client's storage is refused first, with nothing changed: its clearing intent
        // is that client's to finish under its own rules (Opus validator r6 n1).
        if let Some(v) = parsed.as_ref().filter(|v| v.version > CURRENT_VERSION) {
            return Err(MigratorError::Refused(newer(v.version)).into());
        }
        if let Some(why) = parsed.as_ref().and_then(|v| v.foreign_layout(storage_path)) {
            return Err(MigratorError::Refused(why).into());
        }
        // D19: an interrupted clear finishes before anything else.
        if parsed.is_some_and(|v| v.clearing) {
            Self::finish_clear(storage_path).await?;
            stored = read_version(storage_path).await?;
        }
        let parsed = stored.as_deref().and_then(|b| serde_json::from_slice::<Version>(b).ok());
        if let Some(v) = parsed.as_ref().and_then(|v| v.mismatch) {
            let current = parsed.as_ref().map_or(0, |v| v.generation);
            if v == current {
                Self::write_clearing(storage_path).await?;
                Self::finish_clear(storage_path).await?;
                stored = read_version(storage_path).await?;
            } else if let Some(mut version) = parsed {
                version.mismatch = None;
                let bytes = version_json(&version)
                    .map_err(|e| StorageError::Serialization(e.to_string()))?;
                atomic_write(&storage_path.join("version.json"), bytes.as_bytes()).await?;
                stored = Some(bytes.into_bytes());
            }
        }
        // A current version from before `layout` (after a finished clear, `stored` names it).
        let unnamed = stored
            .as_deref()
            .and_then(|b| serde_json::from_slice::<Version>(b).ok())
            .is_some_and(|v| v.version == CURRENT_VERSION && !v.clearing && v.layout.is_none());
        // First try, retry if corruption is detected, by clearing the storage and starting over
        let e = match migration_loop(storage_path, stored).await {
            Ok(generation) => {
                // A crash after the version-3 write can leave the version-2 folders behind.
                let path = storage_path.to_path_buf();
                tokio::task::spawn_blocking(move || v3::remove_v2_leftovers(&path))
                    .await
                    .map_err(|e| StorageError::Io(io::Error::other(e)))??;
                if unnamed {
                    stamp_layout(storage_path)?;
                }
                return Ok(generation);
            }
            Err(e) => e,
        };

        match e {
            MigratorError::Io(error) => {
                return Err(StorageError::Io(error));
            }
            MigratorError::Storage(error) => {
                return Err(error);
            }
            MigratorError::Refused(why) => {
                return Err(StorageError::InvalidArgument(why));
            }
            MigratorError::Serde(_) | MigratorError::Corruption(_) => {
                // Corrupted storage, clear and start over
            }
        }

        tracing::warn!("Storage migration failed, clearing storage and starting over: {}", e);

        // The version-2 folders here are set aside before anything is cleared, whether or not a
        // set-aside had begun (D42, BL-186). If that fails, the open fails and nothing is
        // deleted.
        let path = storage_path.to_path_buf();
        let version = stored_version(storage_path).await?;
        tokio::task::spawn_blocking(move || v3::set_aside_before_clear(&path, version))
            .await
            .map_err(|e| StorageError::Io(io::Error::other(e)))??;

        // D19's clear (logs and the version-2 data a migration set aside, D39, stay).
        Self::clear(storage_path).await?;

        // Now try again, this time it should succeed
        migration_loop(storage_path, read_version(storage_path).await?).await.map_err(Into::into)
    }
}

/// `version.json`'s contents: at the current version, with this client's [`LAYOUT`].
fn version_json(version: &Version) -> serde_json::Result<String> {
    serde_json::to_string_pretty(version)
}

async fn read_version_struct(path: &Path) -> io::Result<Version> {
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => Some(bytes),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::IsADirectory) => None,
        Err(e) => return Err(e),
    };
    Ok(bytes
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| Version::new(CURRENT_VERSION)))
}

fn read_version_struct_sync(path: &Path) -> io::Result<Version> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::IsADirectory) => None,
        Err(e) => return Err(e),
    };
    Ok(bytes
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| Version::new(CURRENT_VERSION)))
}

/// D52: mark a confirmed mismatch while preserving the other version fields.
pub(crate) fn mark_mismatch_sync(storage_path: &Path, generation: u64) -> io::Result<()> {
    let path = storage_path.join("version.json");
    let mut version = read_version_struct_sync(&path)?;
    version.mismatch = Some(generation);
    let bytes = version_json(&version).map_err(io::Error::other)?;
    crate::storage::io::atomic_write_sync(&path, bytes.as_bytes())
}

/// D52: write the clearing intent synchronously from the store's blocking job.
pub(crate) fn write_clearing_sync(storage_path: &Path) -> io::Result<()> {
    let path = storage_path.join("version.json");
    let mut version = read_version_struct_sync(&path)?;
    version.clearing = true;
    let bytes = version_json(&version).map_err(io::Error::other)?;
    crate::storage::io::atomic_write_sync(&path, bytes.as_bytes())
}

/// D52: finish an online clear and remove the mismatch intent.
pub(crate) fn write_cleared_sync(storage_path: &Path, generation: u64) -> io::Result<()> {
    let path = storage_path.join("version.json");
    let mut version = read_version_struct_sync(&path)?;
    version.clearing = false;
    version.mismatch = None;
    version.generation = generation;
    version.version = CURRENT_VERSION;
    version.layout = Some(LAYOUT.to_owned());
    let bytes = version_json(&version).map_err(io::Error::other)?;
    crate::storage::io::atomic_write_sync(&path, bytes.as_bytes())
}

/// Names [`LAYOUT`] in the current version's `version.json` (synced): idempotent, and a crash
/// leaves the old file or the new one, both this client's.
fn stamp_layout(storage_path: &Path) -> io::Result<()> {
    let mut value = read_version_struct_sync(&storage_path.join("version.json"))?;
    value.version = CURRENT_VERSION;
    value.clearing = false;
    value.layout = Some(LAYOUT.to_owned());
    let version = version_json(&value).map_err(io::Error::other)?;
    crate::storage::io::atomic_write_sync(&storage_path.join("version.json"), version.as_bytes())
}

/// SB14 D39: what the public direct open, which never migrates, may open. `version.json` at
/// the current version with no clearing intent and no version-2 folder beside it; or no
/// `version.json` and nothing at the path but `logs`, which it then makes the current version
/// (synced) so that a later open knows it. A current version written before `layout` gets it
/// ([`stamp_layout`]).
/// Anything else (version-2 or -3 data, a clear to finish, unversioned data) is the reason it is
/// refused, with nothing changed: `DiskStorageManager::new` migrates it.
#[cfg(any(test, feature = "test-utils"))]
pub(crate) fn admit_direct_open(storage_path: &Path) -> crate::storage::store::Result<u64> {
    let refuse = |why: String| {
        crate::storage::store::Error::Refused(format!(
            "{} holds {why}: open it through DiskStorageManager::new, which migrates it",
            storage_path.display()
        ))
    };
    let path = storage_path.join("version.json");
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Version>(&bytes) {
            Ok(v) if v.layout.as_deref().is_some_and(|l| l != LAYOUT) => {
                Err(crate::storage::store::Error::Refused(
                    v.foreign_layout(storage_path).unwrap_or_default(),
                ))
            }
            // Version-2 folders beside version 4 are a set-aside to finish (or leftovers to
            // remove), which only `DiskStorageManager::new` does: the store the direct open
            // creates would make it delete them (Opus-h r1 m3).
            Ok(Version {
                version: CURRENT_VERSION,
                clearing: false,
                layout,
                generation,
                mismatch: None,
            }) => match v3::any_v2_folder(storage_path)? {
                true => Err(refuse("version-2 folders to set aside".into())),
                false if layout.is_none() => {
                    stamp_layout(storage_path)?;
                    Ok(generation)
                }
                false => Ok(generation),
            },
            Ok(v) if v.version > CURRENT_VERSION => {
                Err(crate::storage::store::Error::Refused(format!(
                    "{} holds storage version {}, newer than this client's {CURRENT_VERSION}",
                    storage_path.display(),
                    v.version
                )))
            }
            Ok(v) if v.clearing || v.mismatch.is_some() => Err(refuse("a clear to finish".into())),
            Ok(v) => Err(refuse(format!("storage version {}", v.version))),
            Err(e) => Err(refuse(format!("a version.json that does not decode ({e})"))),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if let Some(name) = foreign_entry(storage_path)? {
                return Err(refuse(format!("unversioned data ({})", name.to_string_lossy())));
            }
            crate::storage::io::create_dir_synced(storage_path)?;
            let version = version_json(&Version {
                version: CURRENT_VERSION,
                clearing: false,
                layout: Some(LAYOUT.to_owned()),
                generation: 0,
                mismatch: None,
            })
            .map_err(io::Error::other)?;
            crate::storage::io::atomic_write_sync(&path, version.as_bytes())?;
            Ok(0)
        }
        Err(e) => Err(e.into()),
    }
}

/// The first entry of `dir` other than `logs` and a temporary `version.json` a crash left
/// (`None`: there is none, or no `dir`).
#[cfg(any(test, feature = "test-utils"))]
fn foreign_entry(dir: &Path) -> io::Result<Option<std::ffi::OsString>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let name = entry?.file_name();
        let temp =
            name.to_str().is_some_and(|n| n.starts_with(".version.json.") && n.ends_with(".tmp"));
        if name != "logs" && !temp {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// The version `version.json` holds, `None` when it is absent or does not parse. A failed read
/// is an error, not "no version" (D44): the fallback chooses what it sets aside by it.
async fn stored_version(storage_path: &Path) -> io::Result<Option<u32>> {
    let bytes = read_version(storage_path).await?;
    Ok(bytes.and_then(|b| serde_json::from_slice::<Version>(&b).ok()).map(|v| v.version))
}

/// `version.json`'s bytes, or `None` when it is absent or no file (version 0). A failed read
/// fails the open: read as "no version", it would run every migration over data it could not
/// see (SB13 D44).
async fn read_version(storage_path: &Path) -> io::Result<Option<Vec<u8>>> {
    match tokio::fs::read(storage_path.join("version.json")).await {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::IsADirectory) => {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Why a storage version newer than this client's is refused.
fn newer(version: u32) -> String {
    format!("Storage version {version} is newer than current version {CURRENT_VERSION}")
}

/// The migrations from the version `stored` (`version.json`'s bytes, [`read_version`]) to the
/// current one.
async fn migration_loop(
    storage_path: &Path,
    stored: Option<Vec<u8>>,
) -> Result<u64, MigratorError> {
    let version_path = storage_path.join("version.json");
    let mut generation = 0;
    let mut version = match stored {
        Some(bytes) => {
            let stored = serde_json::from_slice::<Version>(&bytes)?;
            // A defence: unreachable while `migrate` checks first, as the only other bytes read
            // here are the current version this client just wrote (Opus-h r1 m-3).
            if let Some(why) = stored.foreign_layout(storage_path) {
                return Err(MigratorError::Refused(why));
            }
            generation = stored.generation;
            stored.version
        }
        None => 0,
    };

    // A newer client's storage is refused, not cleared, as the direct open refuses it: a
    // downgrade must not lose the store (Opus-h r1 n2).
    if version > CURRENT_VERSION {
        return Err(MigratorError::Refused(newer(version)));
    }

    while version < CURRENT_VERSION {
        tracing::info!("Migrating storage from version {version}");

        // Indexed by the source version (D11 (a)).
        const MIGRATORS: &[&dyn Migrator; CURRENT_VERSION as usize] =
            &[&V1Migrator, &V2Migrator, &V2to4Migrator, &V3Migrator];

        let m = MIGRATORS[version as usize];
        m.apply_migration(storage_path).await?;

        version = m.target();

        let mut value = read_version_struct(&version_path).await?;
        generation = value.generation;
        value.version = version;
        value.clearing = false;
        value.layout = (version == CURRENT_VERSION).then(|| LAYOUT.to_owned());
        let version = version_json(&value)?;

        atomic_write(&version_path, version.as_bytes()).await?;
    }

    Ok(generation)
}
