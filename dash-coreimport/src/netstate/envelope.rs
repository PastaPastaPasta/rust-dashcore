//! Core's `SerializeDB`/`DeserializeDB` envelope (`addrdb.cpp`): `magic ‖ data ‖ SHA256d(magic ‖
//! data)`. The data has no length prefix, so a reader parses it first and then checks the
//! checksum that follows; Core ignores any bytes after the checksum.

use bitcoin_hashes::{Hash, sha256d};
use dash_network::Network;

use super::error::{FormatError, FormatResult};

/// Length of the trailing checksum.
pub(crate) const CHECKSUM_LEN: usize = 32;

/// The magic as it appears on disk: the network's message start.
pub(crate) fn magic_bytes(network: Network) -> [u8; 4] {
    network.magic().to_le_bytes()
}

/// The network a file's leading magic names.
#[cfg(feature = "full")]
pub(crate) fn network_of(magic: [u8; 4]) -> Option<Network> {
    Network::from_magic(u32::from_le_bytes(magic))
}

/// Checks the leading magic against `network` and returns the offset the data starts at.
pub(crate) fn check_magic(
    bytes: &[u8],
    network: Network,
    what: &'static str,
) -> FormatResult<usize> {
    let Some(found) = bytes.first_chunk::<4>() else {
        return Err(FormatError::Truncated {
            what,
            offset: bytes.len(),
        });
    };
    if *found != magic_bytes(network) {
        return Err(FormatError::WrongMagic {
            what,
            found: *found,
        });
    }
    Ok(4)
}

/// Verifies the checksum that follows the data ending at `data_end` and returns the number of
/// bytes after it.
pub(crate) fn check_checksum(
    bytes: &[u8],
    data_end: usize,
    what: &'static str,
) -> FormatResult<usize> {
    let Some(stored) = data_end.checked_add(CHECKSUM_LEN).and_then(|end| bytes.get(data_end..end))
    else {
        return Err(FormatError::Truncated {
            what,
            offset: bytes.len(),
        });
    };
    if sha256d::Hash::hash(&bytes[..data_end]).to_byte_array() != stored {
        return Err(FormatError::ChecksumMismatch {
            what,
        });
    }
    Ok(bytes.len() - data_end - CHECKSUM_LEN)
}

pub(crate) fn check_size(len: usize, limit: usize, what: &'static str) -> FormatResult<()> {
    if len > limit {
        return Err(FormatError::TooLarge {
            what,
            size: len as u64,
            limit: limit as u64,
        });
    }
    Ok(())
}
