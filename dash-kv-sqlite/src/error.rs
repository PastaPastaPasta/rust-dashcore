//! SQLite's results as [`StoreError`]s (SB0 §9.4's fail-stop row).

use std::io;

use dash_kv::{ErrorKind, StoreError};
use rusqlite::{Connection, ErrorCode};

pub(crate) const ENGINE: &str = "sqlite";

/// `e`, from a call on `conn`, as an engine error: the kind callers branch on, with SQLite's
/// extended code and the OS error number in the message.
pub(crate) fn engine_error(conn: &Connection, e: rusqlite::Error) -> StoreError {
    // SAFETY: `conn` is open, and `sqlite3_system_errno` only reads the last OS error it saw.
    let errno = unsafe { rusqlite::ffi::sqlite3_system_errno(conn.handle()) };
    let (kind, extended) = match &e {
        rusqlite::Error::SqliteFailure(f, _) => (kind_of(f.code, errno), Some(f.extended_code)),
        _ => (ErrorKind::Other, None),
    };
    let detail = match extended {
        Some(code) => format!("{e} (extended code {code}, errno {errno})"),
        None => e.to_string(),
    };
    StoreError::engine(ENGINE, kind, detail)
}

/// SB0 §9.4: a full disk or quota is `NoSpace`, other I/O failures `Io`, damaged data
/// `Corruption`, and the rest (`BUSY`, `LOCKED`, `NOMEM`, `MISUSE`…) `Other`.
pub(crate) fn kind_of(code: ErrorCode, errno: i32) -> ErrorKind {
    match code {
        ErrorCode::DiskFull => ErrorKind::NoSpace,
        ErrorCode::SystemIoFailure => match io::Error::from_raw_os_error(errno).kind() {
            io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => ErrorKind::NoSpace,
            _ => ErrorKind::Io,
        },
        // `CANTOPEN` is also how iOS reports a protected file while the device is locked (m6).
        ErrorCode::CannotOpen | ErrorCode::ReadOnly => ErrorKind::Io,
        ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => ErrorKind::Corruption,
        _ => ErrorKind::Other,
    }
}

/// Whether `e` is `SQLITE_BUSY_RECOVERY`: another connection is recovering the WAL index, and a
/// retry soon succeeds.
pub(crate) fn busy_recovery(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if f.extended_code == rusqlite::ffi::SQLITE_BUSY_RECOVERY)
}
