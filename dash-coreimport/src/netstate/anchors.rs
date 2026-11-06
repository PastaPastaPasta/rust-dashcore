//! `anchors.dat`: the last outbound block-relay-only peers (`DumpAnchors`/`ReadAnchors` in
//! `addrdb.cpp`): the `addrdb.cpp` envelope around a `std::vector<CAddress>` in the disk form with
//! `ADDRV2_FORMAT` allowed. There is no version byte at file level; each address carries its own
//! disk version word.

use dash_network::Network;

use super::envelope::{check_checksum, check_magic, check_size};
use super::error::FormatResult;
use super::netaddr::{Address, MIN_ADDRESS_SIZE};
use super::reader::Reader;

const WHAT: &str = "anchors.dat";

/// Core keeps two block-relay-only connections; this leaves room for any `std::vector` Core
/// would still accept in a file it wrote.
pub const MAX_ANCHORS_DAT_SIZE: usize = 1 << 20;

/// A parsed `anchors.dat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorsDat {
    pub network: Network,
    pub addresses: Vec<Address>,
    /// Bytes after the checksum; Core ignores them.
    pub trailing_bytes: usize,
}

/// Parses an `anchors.dat` of `network`.
// Core: addrdb.cpp ReadAnchors, DeserializeDB
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<AnchorsDat> {
    check_size(bytes.len(), MAX_ANCHORS_DAT_SIZE, WHAT)?;
    let start = check_magic(bytes, network, WHAT)?;
    let mut r = Reader::at(bytes, start, WHAT);
    let count = r.compact_size(true)?;
    let mut addresses = Vec::with_capacity(r.capacity_for(count, MIN_ADDRESS_SIZE));
    for _ in 0..count {
        addresses.push(Address::read_disk(&mut r, true)?);
    }
    let trailing_bytes = check_checksum(bytes, r.pos(), WHAT)?;
    Ok(AnchorsDat {
        network,
        addresses,
        trailing_bytes,
    })
}
