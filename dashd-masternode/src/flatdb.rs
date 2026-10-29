//! Core's `CFlatDB<T>` (`flat-database.h`) for rdashd's own flat files: `sporks.dat` (SP0),
//! `netfulfilled.dat` and `mncache.dat` (SP1). The outcomes and log lines are Core's; the files
//! are rdashd's (A1 §2.9, SP0 D3, SP1 D3).
//!
//! Layout, every string with a compact-size length:
//! `magic ‖ message start (4) ‖ object ‖ SHA256d of everything before it`. The magic is rdashd's
//! own (`"rdashd-…"`, unlike Core's `"magic…Cache"`), so neither node reads the other's file; the
//! object starts with its own version string ([`FlatObject::write`]).

use std::io::{self, Read as _};
use std::path::Path;
use std::time::Instant;

use dash_consensus::hash::sha256d;
use dash_types::codec::BaseCodec;

/// Core's `CFlatDB::ReadResult` failures.
#[derive(Debug)]
pub enum LoadError {
    /// `FileError`: the file cannot be opened (usually there is none); Core recreates it.
    Missing,
    /// `HashReadError`: shorter than its hash.
    Short,
    /// `IncorrectHash`: corrupted.
    Corrupt,
    /// `IncorrectMagicMessage`.
    Magic,
    /// `IncorrectMagicNumber`: another network's file.
    Network,
    /// `IncorrectFormat`: the header is right but the body is not; Core recreates it.
    Format(ReadError),
    /// `HashReadError` from the file system: it opened but cannot be read.
    Io(io::Error),
}

impl LoadError {
    /// Whether Core starts with an empty object (true) or refuses to start (false).
    // Core: flat-database.h:160-175 CFlatDB::Read
    pub fn recreates(&self) -> bool {
        matches!(self, LoadError::Missing | LoadError::Format(_))
    }

    /// `CoreRead`'s `LogError` line, if it logs one. The `e.what()` texts are rc.4 dashd's
    /// (SP1 D17: `ReadCompactSize(): size too large`, `non-canonical ReadCompactSize()`,
    /// `DataStream::read(): end of data` and, for a file under 32 bytes, `AutoFile::read: end of
    /// file`, each with libstdc++'s `: iostream error`).
    // Core: flat-database.h:76-152 CFlatDB::CoreRead
    pub fn core_read_error(&self) -> Option<String> {
        Some(match self {
            LoadError::Missing => return None,
            LoadError::Short => {
                format!(
                    "CoreRead: Deserialize or I/O error - AutoFile::read: end of file{IOSTREAM_ERROR}"
                )
            }
            LoadError::Corrupt => "CoreRead: Checksum mismatch, data corrupted".to_owned(),
            LoadError::Magic => "CoreRead: Invalid magic message".to_owned(),
            LoadError::Network => "CoreRead: Invalid network magic number".to_owned(),
            LoadError::Format(e) => format!("CoreRead: Deserialize or I/O error - {}", e.what()),
            LoadError::Io(e) => format!("CoreRead: Deserialize or I/O error - {e}"),
        })
    }
}

/// `MAX_SIZE`: the largest compact-size count or length Core's `ReadCompactSize` accepts.
// Core: serialize.h:38-42
pub const MAX_SIZE: u64 = 0x0200_0000;

/// What libstdc++ appends to an `ios_base::failure`'s message (`io_errc::stream`).
const IOSTREAM_ERROR: &str = ": iostream error";

/// Why an object's read failed: the `std::ios_base::failure` Core's stream read throws.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Fewer bytes left than the read needs.
    // Core: streams.h:287 DataStream::read
    EndOfData,
    /// A compact size in a longer form than its value needs.
    // Core: serialize.h:304, :310, :316 ReadCompactSize
    NonCanonical,
    /// A compact size above [`MAX_SIZE`].
    // Core: serialize.h:318-320 ReadCompactSize
    SizeTooLarge,
    /// A `LIMITED_VECTOR` count above its limit.
    // Core: serialize.h:1018-1031 LimitedVectorFormatter
    VectorLimit,
    /// A spork base-sdk refuses for another reason, in base-sdk's words (only rdashd writes
    /// `sporks.dat`).
    Other(String),
}

impl ReadError {
    /// Core's `e.what()`.
    pub fn what(&self) -> String {
        let msg = match self {
            ReadError::EndOfData => "DataStream::read(): end of data",
            ReadError::NonCanonical => "non-canonical ReadCompactSize()",
            ReadError::SizeTooLarge => "ReadCompactSize(): size too large",
            ReadError::VectorLimit => "Vector length limit exceeded",
            ReadError::Other(text) => text,
        };
        format!("{msg}{IOSTREAM_ERROR}")
    }
}

/// One flat file's object: Core's `T` with its `Serialize`, `Unserialize`, `Clear` and
/// `ToString`.
pub trait FlatObject: Default {
    /// The file's name in the network's data directory (Core's).
    const FILE: &'static str;
    /// rdashd's magic message (Core's `strMagicMessage` role).
    const MAGIC: &'static str;
    /// The object's serialization, from its version string on.
    fn write(&self, out: &mut Vec<u8>);
    /// The object's `Unserialize` into `self`, as Core reads it: what it assigned before a
    /// failure stays until [`FlatObject::clear`] runs. An error is a throw (`IncorrectFormat`).
    fn read(&mut self, data: &mut &[u8]) -> Result<(), ReadError>;
    /// `Clear()`, after a read that failed.
    fn clear(&mut self);
    /// `ToString()`, logged after a load and a write.
    fn summary(&self) -> String;
}

pub(crate) fn push_compact_size(out: &mut Vec<u8>, n: usize) {
    dash_types::CompactSize::from(n).encode(out);
}

pub(crate) fn push_str(out: &mut Vec<u8>, s: &[u8]) {
    push_compact_size(out, s.len());
    out.extend_from_slice(s);
}

/// `ReadCompactSize` with its range check: the shortest form only, and at most [`MAX_SIZE`].
// Core: serialize.h:292-322 ReadCompactSize
pub(crate) fn read_compact_size(data: &mut &[u8]) -> Result<usize, ReadError> {
    let [first] = read_array::<1>(data)?;
    let (n, min) = match first {
        0..=252 => (u64::from(first), 0),
        253 => (u64::from(u16::from_le_bytes(read_array(data)?)), 253),
        254 => (u64::from(u32::from_le_bytes(read_array(data)?)), 0x1_0000),
        255 => (u64::from_le_bytes(read_array(data)?), 0x1_0000_0000),
    };
    if n < min {
        return Err(ReadError::NonCanonical);
    }
    if n > MAX_SIZE {
        return Err(ReadError::SizeTooLarge);
    }
    Ok(usize::try_from(n).expect("at most MAX_SIZE"))
}

pub(crate) fn read_bytes<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8], ReadError> {
    let (s, rest) = data.split_at_checked(n).ok_or(ReadError::EndOfData)?;
    *data = rest;
    Ok(s)
}

pub(crate) fn read_str<'a>(data: &mut &'a [u8]) -> Result<&'a [u8], ReadError> {
    let n = read_compact_size(data)?;
    read_bytes(data, n)
}

pub(crate) fn read_array<const N: usize>(data: &mut &[u8]) -> Result<[u8; N], ReadError> {
    Ok(read_bytes(data, N)?.try_into().expect("N bytes"))
}

pub(crate) fn read_i32(data: &mut &[u8]) -> Result<i32, ReadError> {
    read_array(data).map(i32::from_le_bytes)
}

pub(crate) fn read_i64(data: &mut &[u8]) -> Result<i64, ReadError> {
    read_array(data).map(i64::from_le_bytes)
}

/// A serialized `bool`: Core reads any nonzero byte as true.
pub(crate) fn read_bool(data: &mut &[u8]) -> Result<bool, ReadError> {
    read_array::<1>(data).map(|[b]| b != 0)
}

/// A compact-size count, capped for the allocation only (Core's vectors grow as they read).
pub(crate) fn capacity(n: usize) -> usize {
    n.min(1024)
}

/// The file's bytes for `obj` on the network with `message_start`.
// Core: flat-database.h:38-48 CFlatDB::CoreWrite
pub fn encode<T: FlatObject>(obj: &T, message_start: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::new();
    push_str(&mut out, T::MAGIC.as_bytes());
    out.extend_from_slice(&message_start);
    obj.write(&mut out);
    let hash = sha256d(&out);
    out.extend_from_slice(&hash);
    out
}

/// Reads a file's bytes into `obj`: Core's `CoreRead` checks, in its order.
// Core: flat-database.h:75-155 CFlatDB::CoreRead
pub fn decode_into<T: FlatObject>(
    bytes: &[u8],
    message_start: [u8; 4],
    obj: &mut T,
) -> Result<(), LoadError> {
    let (data, hash) =
        bytes.split_at_checked(bytes.len().wrapping_sub(32)).ok_or(LoadError::Short)?;
    if sha256d(data) != hash {
        return Err(LoadError::Corrupt);
    }
    // Everything below runs inside Core's `try`: a read cut short throws, `IncorrectFormat`,
    // whose handler clears the object; the two mismatches return before it.
    let mut data = data;
    let result = (|| {
        if read_str(&mut data).map_err(LoadError::Format)? != T::MAGIC.as_bytes() {
            return Err(LoadError::Magic);
        }
        if read_array::<4>(&mut data).map_err(LoadError::Format)? != message_start {
            return Err(LoadError::Network);
        }
        obj.read(&mut data).map_err(LoadError::Format)
    })();
    if matches!(result, Err(LoadError::Format(_))) {
        obj.clear();
    }
    result
}

/// [`decode_into`] a fresh object.
pub fn decode<T: FlatObject>(bytes: &[u8], message_start: [u8; 4]) -> Result<T, LoadError> {
    let mut obj = T::default();
    decode_into(bytes, message_start, &mut obj).map(|()| obj)
}

/// `CoreRead` from `path` into `obj`, with its log lines.
fn read_into<T: FlatObject>(
    path: &Path,
    message_start: [u8; 4],
    obj: &mut T,
) -> Result<(), LoadError> {
    let start = Instant::now();
    // Core's `fopen` failing for any reason is `FileError` ("missing"); a read failing after it
    // opened is `HashReadError`.
    let read = match std::fs::File::open(path) {
        Ok(mut file) => {
            let mut bytes = Vec::new();
            match file.read_to_end(&mut bytes) {
                Ok(_) => decode_into(&bytes, message_start, obj),
                Err(e) => Err(LoadError::Io(e)),
            }
        }
        Err(_) => Err(LoadError::Missing),
    };
    match &read {
        Ok(()) => {
            tracing::info!("Loaded info from {}  {}ms", T::FILE, start.elapsed().as_millis());
            tracing::info!("     {}", obj.summary());
        }
        Err(e) => {
            if let Some(line) = e.core_read_error() {
                tracing::error!("{line}");
            }
        }
    }
    read
}

/// Core's `CFlatDB::Read`: `Ok` when Core goes on (the file loaded, or Core recreates it), or
/// the failure that stops the node.
// Core: flat-database.h:158-175
fn read<T: FlatObject>(path: &Path, message_start: [u8; 4], obj: &mut T) -> Result<(), LoadError> {
    match read_into(path, message_start, obj) {
        Ok(()) => Ok(()),
        Err(LoadError::Missing) => {
            tracing::info!("Missing file {}, will try to recreate", T::FILE);
            Ok(())
        }
        Err(e) if e.recreates() => {
            tracing::info!(
                "ERROR: CFlatDB::Read Error reading {}: Read: Magic is ok but data has invalid format, will try to recreate",
                T::FILE
            );
            Ok(())
        }
        Err(e) => {
            tracing::info!(
                "ERROR: CFlatDB::Read Error reading {}: Read: File format is unknown or invalid, please fix it manually",
                T::FILE
            );
            Err(e)
        }
    }
}

/// Core's `CFlatDB::Load`: reads `path` into `obj`. `Err` stops the node; a missing or badly
/// formatted file leaves `obj` cleared.
// Core: flat-database.h:186-190 CFlatDB::Load
pub fn load<T: FlatObject>(
    path: &Path,
    message_start: [u8; 4],
    obj: &mut T,
) -> Result<(), LoadError> {
    tracing::info!("Reading info from {}...", T::FILE);
    read(path, message_start, obj)
}

/// Writes `bytes` to `tmp`, syncs it, renames it over `path` and syncs the directory, so a crash
/// leaves the old file or the new one.
fn write_durably(tmp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(tmp, bytes)?;
    std::fs::File::open(tmp)?.sync_all()?;
    std::fs::rename(tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        std::fs::File::open(if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        })?
        .sync_all()?;
    }
    Ok(())
}

/// Core's `CFlatDB::Store`: verifies the file on disk first and leaves one Core would refuse to
/// load untouched (`Ok(false)`). Writes a temporary file and renames it over the old one (SP0
/// D13); a failed write is `Err`, Core's `CoreWrite` returning false.
// Core: flat-database.h:36-73 CoreWrite, 192-205 Store
pub fn store<T: FlatObject>(obj: &T, path: &Path, message_start: [u8; 4]) -> io::Result<bool> {
    tracing::info!("Verifying {} format...", T::FILE);
    if read(path, message_start, &mut T::default()).is_err() {
        return Ok(false);
    }
    let start = Instant::now();
    tracing::info!("Writing info to {}...", T::FILE);
    let bytes = encode(obj, message_start);
    let tmp = path.with_extension("dat.new");
    let written = write_durably(&tmp, path, &bytes);
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written?;
    tracing::info!("Written info to {}  {}ms", T::FILE, start.elapsed().as_millis());
    tracing::info!("     {}", obj.summary());
    tracing::info!("{} dump finished  {}ms", T::FILE, start.elapsed().as_millis());
    Ok(true)
}

/// `CoreWrite`'s error line for a failed write; the result Core's `Store` returns.
// Core: flat-database.h:58-61
pub(crate) fn log_write(r: io::Result<bool>) -> bool {
    r.unwrap_or_else(|e| {
        tracing::error!("CoreWrite: Serialize or I/O error - {e}");
        false
    })
}
