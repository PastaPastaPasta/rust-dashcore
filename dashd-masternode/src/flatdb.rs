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
    decode_with(bytes, message_start, T::MAGIC.as_bytes(), obj, T::read)
}

/// [`decode_into`] with the magic message and the object's read given: the checks and their
/// order are shared with the `core-layout` seam (`core_layout`), so the oracle compares them.
fn decode_with<T: FlatObject>(
    bytes: &[u8],
    message_start: [u8; 4],
    magic: &[u8],
    obj: &mut T,
    read: impl FnOnce(&mut T, &mut &[u8]) -> Result<(), ReadError>,
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
        if read_str(&mut data).map_err(LoadError::Format)? != magic {
            return Err(LoadError::Magic);
        }
        if read_array::<4>(&mut data).map_err(LoadError::Format)? != message_start {
            return Err(LoadError::Network);
        }
        read(obj, &mut data).map_err(LoadError::Format)
    })();
    if matches!(result, Err(LoadError::Format(_))) {
        obj.clear();
    }
    result
}

/// A file layout: the bytes of `T`'s file. [`load`] and [`store`] (and the managers' caches) run
/// rdashd's own, [`Own`]; the test-only `core_layout::Core` runs the same bodies on Core's.
pub(crate) trait Layout<T: FlatObject> {
    fn decode_into(bytes: &[u8], message_start: [u8; 4], obj: &mut T) -> Result<(), LoadError>;
    fn encode(obj: &T, message_start: [u8; 4]) -> Vec<u8>;
}

/// rdashd's layout ([`encode`], [`decode_into`]).
pub(crate) struct Own;

impl<T: FlatObject> Layout<T> for Own {
    fn decode_into(bytes: &[u8], message_start: [u8; 4], obj: &mut T) -> Result<(), LoadError> {
        decode_into(bytes, message_start, obj)
    }

    fn encode(obj: &T, message_start: [u8; 4]) -> Vec<u8> {
        encode(obj, message_start)
    }
}

/// Core's own layout of `netfulfilled.dat` and `mncache.dat` (its magic messages and version
/// strings), for the L1 comparison against Core's `CFlatDB` (BL-451, DEC-626 P2). Test-only
/// (feature `core-layout`, which only node-l1's `oracle` turns on): rdashd reads and writes its
/// own layout (SP1 D3).
#[cfg(any(test, feature = "core-layout"))]
pub mod core_layout {
    use super::*;

    /// A flat object whose file Core also writes, in Core's layout.
    pub trait CoreLayout: FlatObject {
        /// Core's `strMagicMessage`.
        const CORE_MAGIC: &'static str;
        /// rdashd's version string ([`FlatObject::write`] starts with it).
        const VERSION: &'static str;
        /// Core's version string, if its object has one.
        const CORE_VERSION: Option<&'static str>;
    }

    impl CoreLayout for crate::netfulfilled::NetFulfilledStore {
        const CORE_MAGIC: &'static str = "magicFulfilledCache";
        const VERSION: &'static str = crate::netfulfilled::VERSION;
        const CORE_VERSION: Option<&'static str> = None;
    }

    impl CoreLayout for crate::meta::MasternodeMetaStore {
        const CORE_MAGIC: &'static str = "magicMasternodeCache";
        const VERSION: &'static str = crate::meta::VERSION;
        // Core: masternode/meta.cpp:11
        const CORE_VERSION: Option<&'static str> = Some("CMasternodeMetaMan-Version-5");
    }

    /// The object's read on Core's body: Core's version string read as rdashd's, any other as
    /// a mismatch; with no version (netfulfilled) the body is read as if rdashd's preceded it.
    fn read_core<T: CoreLayout>(obj: &mut T, data: &mut &[u8]) -> Result<(), ReadError> {
        let version: &[u8] = match T::CORE_VERSION {
            Some(core) if read_str(data)? != core.as_bytes() => b"",
            _ => T::VERSION.as_bytes(),
        };
        let mut body = Vec::with_capacity(9 + version.len() + data.len());
        push_str(&mut body, version);
        body.extend_from_slice(data);
        obj.read(&mut &body[..])
    }

    /// [`decode_into`] of a file in Core's layout: the same checks in the same order, and
    /// what a failed read leaves in `obj` (`IncorrectFormat` clears it).
    pub fn decode_into_with_magic<T: CoreLayout>(
        bytes: &[u8],
        message_start: [u8; 4],
        obj: &mut T,
    ) -> Result<(), LoadError> {
        decode_with(bytes, message_start, T::CORE_MAGIC.as_bytes(), obj, read_core)
    }

    /// [`decode_into_with_magic`] a fresh object.
    pub fn decode_with_magic<T: CoreLayout>(
        bytes: &[u8],
        message_start: [u8; 4],
    ) -> Result<T, LoadError> {
        let mut obj = T::default();
        decode_into_with_magic(bytes, message_start, &mut obj).map(|()| obj)
    }

    /// The object's body in Core's layout (what Core's `CoreWrite` puts after the message
    /// start).
    pub fn core_body<T: CoreLayout>(obj: &T) -> Vec<u8> {
        let mut ours = Vec::new();
        obj.write(&mut ours);
        let mut prefix = Vec::new();
        push_str(&mut prefix, T::VERSION.as_bytes());
        let rest = ours.strip_prefix(&prefix[..]).expect("write starts with the version");
        let mut out = Vec::new();
        if let Some(core) = T::CORE_VERSION {
            push_str(&mut out, core.as_bytes());
        }
        out.extend_from_slice(rest);
        out
    }

    /// [`encode`] in Core's layout.
    pub fn encode_with_magic<T: CoreLayout>(obj: &T, message_start: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        push_str(&mut out, T::CORE_MAGIC.as_bytes());
        out.extend_from_slice(&message_start);
        out.extend(core_body(obj));
        let hash = sha256d(&out);
        out.extend_from_slice(&hash);
        out
    }

    /// Core's layout, for the managers' `*_core_layout` caches.
    pub struct Core;

    impl<T: CoreLayout> Layout<T> for Core {
        fn decode_into(bytes: &[u8], message_start: [u8; 4], obj: &mut T) -> Result<(), LoadError> {
            decode_into_with_magic(bytes, message_start, obj)
        }

        fn encode(obj: &T, message_start: [u8; 4]) -> Vec<u8> {
            encode_with_magic(obj, message_start)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::meta::MasternodeMetaStore;
        use crate::netfulfilled::NetFulfilledStore;

        const START: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

        #[test]
        fn each_layout_reads_only_its_own_magic() {
            let meta = MasternodeMetaStore {
                dsq_count: 7,
                ..Default::default()
            };
            let core = encode_with_magic(&meta, START);
            assert_eq!(decode_with_magic::<MasternodeMetaStore>(&core, START).unwrap(), meta);
            assert!(matches!(decode::<MasternodeMetaStore>(&core, START), Err(LoadError::Magic)));
            let ours = encode(&meta, START);
            assert!(matches!(
                decode_with_magic::<MasternodeMetaStore>(&ours, START),
                Err(LoadError::Magic)
            ));
            let fulfilled = NetFulfilledStore::default();
            let core = encode_with_magic(&fulfilled, START);
            assert_eq!(core_body(&fulfilled), [0]);
            assert_eq!(decode_with_magic::<NetFulfilledStore>(&core, START).unwrap(), fulfilled);
        }

        /// Core's `Unserialize` returns on another version string: the object stays empty
        /// and the load succeeds.
        #[test]
        fn another_core_version_is_a_mismatch() {
            let mut file = Vec::new();
            push_str(&mut file, b"magicMasternodeCache");
            file.extend(START);
            push_str(&mut file, crate::meta::VERSION.as_bytes());
            file.extend([0, 9, 0, 0, 0, 0, 0, 0, 0, 0]);
            let hash = sha256d(&file);
            file.extend(hash);
            let read = decode_with_magic::<MasternodeMetaStore>(&file, START).unwrap();
            assert_eq!(read, MasternodeMetaStore::default());
        }
    }
}

/// [`decode_into`] a fresh object.
pub fn decode<T: FlatObject>(bytes: &[u8], message_start: [u8; 4]) -> Result<T, LoadError> {
    let mut obj = T::default();
    decode_into(bytes, message_start, &mut obj).map(|()| obj)
}

/// `CoreRead` from `path` into `obj`, with its log lines.
fn read_into<T: FlatObject, L: Layout<T>>(
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
                Ok(_) => L::decode_into(&bytes, message_start, obj),
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
fn read<T: FlatObject, L: Layout<T>>(
    path: &Path,
    message_start: [u8; 4],
    obj: &mut T,
) -> Result<(), LoadError> {
    match read_into::<T, L>(path, message_start, obj) {
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
    load_as::<T, Own>(path, message_start, obj)
}

/// [`load`] in the layout `L`.
pub(crate) fn load_as<T: FlatObject, L: Layout<T>>(
    path: &Path,
    message_start: [u8; 4],
    obj: &mut T,
) -> Result<(), LoadError> {
    tracing::info!("Reading info from {}...", T::FILE);
    read::<T, L>(path, message_start, obj)
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
    store_as::<T, Own>(obj, path, message_start)
}

/// [`store`] in the layout `L`.
pub(crate) fn store_as<T: FlatObject, L: Layout<T>>(
    obj: &T,
    path: &Path,
    message_start: [u8; 4],
) -> io::Result<bool> {
    tracing::info!("Verifying {} format...", T::FILE);
    if read::<T, L>(path, message_start, &mut T::default()).is_err() {
        return Ok(false);
    }
    let start = Instant::now();
    tracing::info!("Writing info to {}...", T::FILE);
    let bytes = L::encode(obj, message_start);
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
