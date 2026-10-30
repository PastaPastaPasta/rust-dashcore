//! Core's `CFlatDB<T>` envelope (`flat-database.h`), shared by `governance.dat`, `mncache.dat`,
//! `netfulfilled.dat` and `sporks.dat` (IM1b).
//!
//! On disk: `string(magic message) ‖ 4-byte network magic ‖ object ‖ SHA256d(everything before)`.
//! The checksum is the last 32 bytes of the file, so the file is checked before anything is
//! decoded (unlike `addrdb.cpp`'s envelope, whose data has no length).
//!
//! # What Core does with each failure (`CFlatDB::CoreRead`, `Read`)
//!
//! | stage | Core's result | here |
//! |---|---|---|
//! | file shorter than 32 bytes, or unreadable | `HashReadError`: startup fails | [`FormatError::Truncated`] |
//! | checksum differs | `IncorrectHash`: startup fails | [`FormatError::ChecksumMismatch`] |
//! | magic message differs | `IncorrectMagicMessage`: startup fails | [`FormatError::Corrupt`] |
//! | network magic differs | `IncorrectMagicNumber`: startup fails | [`FormatError::WrongMagic`] |
//! | the magic message, the magic or the object does not deserialize | `IncorrectFormat`: the object is `Clear()`ed, the load still succeeds, "will try to recreate" | `Ok(`[`Loaded`]`)` with the cleared value and the reason in [`Loaded::discarded`] |
//!
//! The last row is why the parsers return [`Loaded`]: an importer that treats a damaged object as
//! an error would refuse a datadir that Core starts from. The first four are the cases in which
//! Core refuses to start, so they are errors here.
//!
//! Bytes after the object (but before the checksum) are ignored, as in Core.

use bitcoin_hashes::{Hash, sha256d};
use dash_network::Network;

#[cfg(feature = "full")]
use super::envelope::network_of;
use super::envelope::{CHECKSUM_LEN, check_size, magic_bytes};
use super::error::{FormatError, FormatResult};
use super::reader::Reader;

/// What a flat file's parser returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded<T> {
    /// The object Core would hold after loading: what the file said, or the cleared object when
    /// [`discarded`](Self::discarded) is set.
    pub value: T,
    /// Set when Core's `CFlatDB` reports `IncorrectFormat`: the envelope was intact (checksum and
    /// magic message) but the magic or the object did not deserialize. Core clears the object,
    /// logs "data has invalid format, will try to recreate" and starts with it empty; the
    /// importer should do the same and may warn with this reason.
    pub discarded: Option<FormatError>,
    /// Bytes between the end of the object and the checksum (Core ignores them).
    pub trailing_bytes: usize,
}

/// One flat-file object type.
pub(crate) trait Payload: Default {
    const FILE: &'static str;
    /// `CFlatDB`'s `strMagicMessage`.
    const MAGIC_MESSAGE: &'static str;
    /// The version strings the object may start with, the one Core v24 writes first; empty when the
    /// object has none. A file with another version string loads as empty, as in Core. Read by
    /// the format detection, which is `full`'s.
    #[cfg_attr(not(feature = "full"), allow(dead_code))]
    const VERSION_STRINGS: &'static [&'static str];
    /// The object's `Unserialize`, reading into `self` as Core does, so that what it assigned
    /// before a failure is what `Clear()` then leaves behind.
    fn read(&mut self, r: &mut Reader<'_>) -> FormatResult<()>;
    /// The object's `Clear()`.
    fn clear(&mut self);
}

/// `CFlatDB::CoreRead` over the bytes of the file, for the object type `T`.
// Core: flat-database.h CFlatDB::CoreRead, Read
pub(crate) fn load<T: Payload>(
    bytes: &[u8],
    network: Network,
    limit: usize,
) -> FormatResult<Loaded<T>> {
    check_size(bytes.len(), limit, T::FILE)?;
    let Some(data_len) = bytes.len().checked_sub(CHECKSUM_LEN) else {
        return Err(FormatError::Truncated {
            what: T::FILE,
            offset: bytes.len(),
        });
    };
    let (data, stored) = bytes.split_at(data_len);
    if sha256d::Hash::hash(data).to_byte_array() != stored {
        return Err(FormatError::ChecksumMismatch {
            what: T::FILE,
        });
    }
    let mut r = Reader::new(data, T::FILE);
    let mut value = T::default();
    let discard = |mut value: T, reason: FormatError| {
        value.clear();
        Loaded {
            value,
            discarded: Some(reason),
            trailing_bytes: 0,
        }
    };
    // `ssObj >> strMagicMessageTmp`, then the comparison.
    match r.byte_vec() {
        Ok(found) if found == T::MAGIC_MESSAGE.as_bytes() => {}
        Ok(_) => {
            return Err(FormatError::Corrupt {
                what: T::FILE,
                reason: "magic message differs",
            });
        }
        Err(e) => return Ok(discard(value, e)),
    }
    // `ssObj >> pchMsgTmp`, then the comparison.
    match r.array::<4>() {
        Ok(found) if found == magic_bytes(network) => {}
        Ok(found) => {
            return Err(FormatError::WrongMagic {
                what: T::FILE,
                found,
            });
        }
        Err(e) => return Ok(discard(value, e)),
    }
    match value.read(&mut r) {
        Ok(()) => Ok(Loaded {
            value,
            discarded: None,
            trailing_bytes: r.remaining(),
        }),
        Err(e) => Ok(discard(value, e)),
    }
}

/// The header of a flat file without its checksum: the network its magic names and whether the
/// object's version string is one the reader loads (always true for an object without one).
#[cfg(feature = "full")]
pub(crate) fn peek<T: Payload>(bytes: &[u8]) -> FormatResult<(Network, bool)> {
    let mut r = Reader::new(bytes, T::FILE);
    if r.byte_vec()? != T::MAGIC_MESSAGE.as_bytes() {
        return Err(r.corrupt("magic message differs"));
    }
    let found = r.array::<4>()?;
    let network = network_of(found).ok_or(FormatError::WrongMagic {
        what: T::FILE,
        found,
    })?;
    let current = match T::VERSION_STRINGS {
        [] => true,
        known => {
            let found = r.byte_vec()?;
            known.iter().any(|version| version.as_bytes() == found)
        }
    };
    Ok((network, current))
}

/// Core's `ReadCompactSize` count of a container, for a loop that reads one element per
/// iteration. The count is not trusted for any allocation: every element is at least one byte,
/// so a count beyond the bytes left ends in [`FormatError::Truncated`] after at most that many
/// iterations.
pub(crate) fn count(r: &mut Reader<'_>) -> FormatResult<u64> {
    r.compact_size(true)
}
