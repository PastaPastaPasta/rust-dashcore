use std::path::PathBuf;

/// Why a Core net-state file did not parse. Every reader in [`netstate`](super) returns this for
/// any input and never panics; what it allocates is bounded by the input length and the caps and
/// memory rows of the crate's bounds table.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormatError {
    /// The input ends inside a structure.
    #[error("{what}: truncated at offset {offset}")]
    Truncated {
        what: &'static str,
        offset: usize,
    },
    /// The input is larger than the largest file Core can write (the reader's resource bound).
    #[error("{what}: {size} bytes exceeds the {limit}-byte limit")]
    TooLarge {
        what: &'static str,
        size: u64,
        limit: u64,
    },
    /// The leading network magic is not the expected network's (or, when sniffing, not a known one).
    #[error("{what}: network magic {found:02x?} does not match")]
    WrongMagic {
        what: &'static str,
        found: [u8; 4],
    },
    /// The trailing SHA256d does not match the data.
    #[error("{what}: checksum mismatch")]
    ChecksumMismatch {
        what: &'static str,
    },
    /// A version Core's reader of that release also refuses (or ignores), or a newer one.
    #[error("{what}: unsupported version {version}")]
    UnsupportedVersion {
        what: &'static str,
        version: u64,
    },
    /// Bytes that decode but contradict Core's own sanity checks.
    #[error("{what}: corrupt: {reason}")]
    Corrupt {
        what: &'static str,
        reason: &'static str,
    },
    /// JSON that Core's UniValue/`ReadSettings` would reject (or a banlist it would refuse).
    #[error("{what}: {message}")]
    Json {
        what: &'static str,
        message: String,
    },
    /// A nested structure that another crate's codec decodes (the deterministic masternode list
    /// inside `governance.dat`) was refused by it; `message` is that codec's reason.
    #[error("{what}: {message}")]
    Nested {
        what: &'static str,
        message: String,
    },
    /// `sniff` found no format that parses.
    #[error("not a recognised Core net-state file")]
    Unrecognized,
}

/// A failure of the file helpers: the file could not be read, or its bytes do not parse.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Format(#[from] FormatError),
}

pub type FormatResult<T> = Result<T, FormatError>;
