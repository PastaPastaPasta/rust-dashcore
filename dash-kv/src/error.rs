use crate::codec::CodecError;

/// What kind of engine failure occurred: the part of an error callers branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The filesystem is full (`ENOSPC`).
    NoSpace,
    /// Another I/O error.
    Io,
    /// The engine detected corrupt data (checksum mismatch, malformed file).
    Corruption,
    /// The engine refuses writes after an earlier failure.
    ReadOnly,
    /// Anything else (invalid options, unsupported operation, lock held by another process).
    Other,
}

/// Errors from the storage layer.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The engine reported an error.
    #[error("{engine} error ({kind:?}): {message}")]
    Engine {
        engine: &'static str,
        kind: ErrorKind,
        message: String,
    },
    /// The database was written by a newer schema than this build supports. Nothing was written.
    #[error("database `{db}` has schema version {found}, this build supports up to {supported}")]
    SchemaTooNew {
        db: &'static str,
        found: u32,
        supported: u32,
    },
    /// The directory is not this database: another database, data without a schema record, or
    /// files that are not a database at all. Nothing was written. (A damaged or incomplete
    /// database of ours is an engine error of kind [`ErrorKind::Corruption`] instead.)
    #[error("`{db}` directory is not that database ({what}); refusing to use it")]
    UnknownDatabase {
        db: &'static str,
        what: String,
    },
    /// A stored key or value did not decode.
    #[error("codec error in column family `{cf}`: {source}")]
    Codec {
        cf: &'static str,
        #[source]
        source: CodecError,
    },
}

impl StoreError {
    /// An engine failure: what engine implementations return.
    pub fn engine(engine: &'static str, kind: ErrorKind, message: impl Into<String>) -> Self {
        Self::Engine {
            engine,
            kind,
            message: message.into(),
        }
    }

    /// The engine failure kind, or `None` for errors raised by this crate.
    pub fn kind(&self) -> Option<ErrorKind> {
        match self {
            Self::Engine {
                kind,
                ..
            } => Some(*kind),
            _ => None,
        }
    }
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;
