use std::io;
use std::path::{Path, PathBuf};

use crate::flatfile::{FlatFilePos, OpenFailed};
use dash_kv::ErrorKind;

/// Errors from block and undo files.
#[derive(Debug, thiserror::Error)]
pub enum FlatFileError {
    /// An I/O call failed. `context` is Core's `AbortNode` message where Core has one.
    #[error("{context}: {path}: {source}")]
    Io {
        context: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// `CheckDiskSpace` failed before an allocation (Core: `AbortNode("Disk space is too low!")`).
    #[error("Disk space is too low!")]
    NoSpace,
    /// An earlier write, allocation or sync failed; nothing more is written (A1 §3.5.6, I5).
    #[error("block files refuse writes after an earlier I/O failure")]
    Failed,
    /// Stored bytes are not what the position or checksum says.
    #[error("{what} at {pos}: {detail}")]
    Corrupt {
        what: &'static str,
        pos: FlatFilePos,
        detail: String,
    },
}

impl FlatFileError {
    pub(crate) fn io(context: &'static str, path: PathBuf, source: io::Error) -> Self {
        Self::Io {
            context,
            path,
            source,
        }
    }

    /// `self` as its file's failed open ([`OpenFailed`]): Core's `Open` inside `Allocate`
    /// (`flatfile.cpp:68`), which fails apart from the space check before it.
    pub(crate) fn into_open_failure(self) -> Self {
        match self {
            Self::Io {
                context,
                path,
                source,
            } if !OpenFailed::is(&source) => Self::Io {
                context,
                path,
                source: OpenFailed::wrap(source),
            },
            e => e,
        }
    }

    /// The file that did not open, if this is a read, or an allocation, whose file did not open
    /// ([`OpenFailed`]), as against a failed read of an open file.
    pub fn open_failed(&self) -> Option<&Path> {
        match self {
            Self::Io {
                path,
                source,
                ..
            } if OpenFailed::is(source) => Some(path),
            _ => None,
        }
    }

    /// The storage-layer failure class.
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::Io {
                source,
                ..
            } if matches!(OpenFailed::os_error(source), Some(libc::ENOSPC | libc::EDQUOT)) => {
                ErrorKind::NoSpace
            }
            Self::Io {
                ..
            } => ErrorKind::Io,
            Self::NoSpace => ErrorKind::NoSpace,
            Self::Failed => ErrorKind::ReadOnly,
            Self::Corrupt {
                ..
            } => ErrorKind::Corruption,
        }
    }
}

pub type FlatResult<T> = std::result::Result<T, FlatFileError>;
