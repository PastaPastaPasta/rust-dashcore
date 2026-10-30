use std::path::PathBuf;

/// Errors from reading a Core datadir or a trace.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Bytes that do not decode as the expected Core structure.
    #[error("malformed {what}: {detail}")]
    Malformed {
        what: &'static str,
        detail: String,
    },
    /// The data decodes but contradicts itself or Core's undo data.
    #[error("inconsistent data: {0}")]
    Inconsistent(String),
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    pub(crate) fn malformed(what: &'static str, detail: impl Into<String>) -> Self {
        Self::Malformed {
            what,
            detail: detail.into(),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
