//! The SQLite database is created by its first write (SB14 D4): a store that holds only headers
//! has none. Only `ENOENT` means absent; a database that exists but cannot be opened or read
//! fails the open (D4 condition 2). After creation the store syncs the storage directory, so
//! `spv/`'s entry is durable before any commit that carries a slot or rows.

use std::path::{Path, PathBuf};

use dash_kv::KvEngine;

use super::{kv, Result};

/// Opens or creates the store's database.
pub trait DbOpener<E: KvEngine>: Send + Sync {
    /// The existing database, or `None` when it is absent (`ENOENT`).
    fn open_existing(&self) -> Result<Option<E>>;
    /// Creates the database (its directory, the file, the schema).
    fn create(&self) -> Result<E>;
}

/// SQLite in `<storage>/spv/` (`kv.sqlite`). `SqliteEngine::open` sets `journal_mode=WAL` before
/// its DDL (D15) and commits the DDL with `synchronous=FULL`, whose WAL sync also syncs `spv/`.
pub struct SqliteOpener {
    pub dir: PathBuf,
    pub config: dash_kv_sqlite::SqliteConfig,
}

impl SqliteOpener {
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            config: dash_kv_sqlite::SqliteConfig::default(),
        }
    }

    fn open(&self) -> Result<dash_kv_sqlite::SqliteEngine> {
        let (engine, _) =
            kv(dash_kv_sqlite::SqliteEngine::open(&self.dir, &super::SPV_SCHEMA, &self.config))?;
        Ok(engine)
    }
}

impl DbOpener<dash_kv_sqlite::SqliteEngine> for SqliteOpener {
    fn open_existing(&self) -> Result<Option<dash_kv_sqlite::SqliteEngine>> {
        let file = self.dir.join(dash_kv_sqlite::FILE);
        match std::fs::symlink_metadata(&file) {
            Ok(_) => self.open().map(Some),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(super::Error::Io(e)),
        }
    }

    fn create(&self) -> Result<dash_kv_sqlite::SqliteEngine> {
        std::fs::create_dir_all(&self.dir)?;
        self.open()
    }
}

/// dash-kv's `MemoryEngine` (unit tests): absent until created, and never reopened.
#[cfg(test)]
#[derive(Default)]
pub struct MemOpener;

#[cfg(test)]
impl DbOpener<dash_kv::MemoryEngine> for MemOpener {
    fn open_existing(&self) -> Result<Option<dash_kv::MemoryEngine>> {
        Ok(None)
    }

    fn create(&self) -> Result<dash_kv::MemoryEngine> {
        Ok(dash_kv::MemoryEngine::new(&super::SPV_SCHEMA))
    }
}
