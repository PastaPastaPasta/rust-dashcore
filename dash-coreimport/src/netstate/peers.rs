//! `peers.dat`: Core's address manager (`AddrManImpl::Serialize`/`Unserialize` in `addrman.cpp`
//! inside the `addrdb.cpp` envelope), every format version Core v24 reads (0 to 4).
//!
//! The reader returns what the file holds. Rebuilding the buckets (hashing with `key` and the
//! network groups) and Core's `IsValid`/collision pruning belong to the address manager that
//! consumes it; [`PeersDat::bucketing_restorable`] says whether the stored placement may be
//! reused.

use dash_network::Network;

use super::envelope::{check_checksum, check_magic, check_size};
use super::error::{FormatError, FormatResult};
use super::netaddr::{Address, MIN_ADDRESS_SIZE, NetAddr};
use super::reader::Reader;

const WHAT: &str = "peers.dat";

/// Larger than any file Core writes (at most 65,536 new and 16,384 tried entries of under 100
/// bytes, and 1,024 buckets of 64 four-byte indexes).
pub const MAX_PEERS_DAT_SIZE: usize = 16 << 20;

// Core: addrman_impl.h
pub const TRIED_BUCKET_COUNT: u32 = 256;
pub const NEW_BUCKET_COUNT: u32 = 1024;
pub const BUCKET_SIZE: u32 = 64;
/// The newest format Core v24 writes (`Format::V4_MULTIPORT`) and reads.
pub const FILE_FORMAT: u8 = 4;
/// `INCOMPATIBILITY_BASE`: the second byte is `32 + lowest_compatible`.
pub(super) const INCOMPATIBILITY_BASE: u8 = 32;

// Core: addrman_impl.h enum Format
const V1_DETERMINISTIC: u8 = 1;
const V2_ASMAP: u8 = 2;
const V3_BIP155: u8 = 3;

/// One `AddrInfo`: an address with where it came from and Core's per-entry statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrInfo {
    pub address: Address,
    /// Where knowledge of this address first came from.
    pub source: NetAddr,
    /// Last successful connection (`int64_t` seconds).
    pub last_success: i64,
    /// Connection attempts since the last success.
    pub attempts: i32,
}

/// A reference from a new-table bucket to an entry of [`PeersDat::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketRef {
    pub bucket: u32,
    pub index: u32,
}

/// A parsed `peers.dat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeersDat {
    pub network: Network,
    /// The first byte: the format the writer used (0 to 4 are known).
    pub format: u8,
    /// The second byte minus 32: the oldest format that can read the file.
    pub lowest_compatible: u8,
    /// The secret key the bucket hashes are salted with.
    pub key: [u8; 32],
    /// The `nNew` entries of the new table.
    pub new: Vec<AddrInfo>,
    /// The `nTried` entries of the tried table.
    pub tried: Vec<AddrInfo>,
    /// `nUBuckets` as stored (1024 when written by Core v24).
    pub new_bucket_count: i32,
    /// The new-table placement: `(bucket, entry)` pairs whose entry index is in range. Core
    /// drops the others silently; [`PeersDat::ignored_bucket_refs`] counts them.
    pub bucket_refs: Vec<BucketRef>,
    pub ignored_bucket_refs: u64,
    /// The asmap checksum the placement was computed under (all zero before format 2, or without
    /// an asmap).
    pub asmap_checksum: [u8; 32],
    /// Bytes after the checksum; Core ignores them.
    pub trailing_bytes: usize,
}

impl PeersDat {
    /// Whether Core would reuse the stored bucket placement: the bucket count is still 1024 and
    /// the asmap is the one in use (`supplied_asmap_checksum` is all zero without an asmap).
    // Core: addrman.cpp AddrManImpl::Unserialize `restore_bucketing`
    pub fn bucketing_restorable(&self, supplied_asmap_checksum: &[u8; 32]) -> bool {
        u32::try_from(self.new_bucket_count) == Ok(NEW_BUCKET_COUNT)
            && self.asmap_checksum == *supplied_asmap_checksum
    }
}

/// Reads an `AddrInfo` (`CAddress`, the source `CNetAddr`, `nLastSuccess`, `nAttempts`).
/// `addrv2` is whether the stream carries `ADDRV2_FORMAT`, which is the case from format 3.
fn read_addr_info(r: &mut Reader<'_>, addrv2: bool) -> FormatResult<AddrInfo> {
    let address = Address::read_disk(r, addrv2)?;
    let source = if addrv2 {
        NetAddr::read_v2(r)?
    } else {
        NetAddr::read_v1(r)?
    };
    Ok(AddrInfo {
        address,
        source,
        last_success: r.i64_le()?,
        attempts: r.i32_le()?,
    })
}

fn read_entries(
    r: &mut Reader<'_>,
    count: i32,
    max: u32,
    addrv2: bool,
) -> FormatResult<Vec<AddrInfo>> {
    let n = u32::try_from(count)
        .ok()
        .filter(|n| *n <= max)
        .ok_or_else(|| r.corrupt("entry count out of range"))?;
    // An entry is at least a disk address, a source (two bytes) and the two statistics.
    let mut out = Vec::with_capacity(r.capacity_for(u64::from(n), MIN_ADDRESS_SIZE + 2 + 12));
    for _ in 0..n {
        out.push(read_addr_info(r, addrv2)?);
    }
    Ok(out)
}

/// Parses a `peers.dat` of `network`.
///
/// Mirrors `AddrManImpl::Unserialize` up to the point where it needs the address manager's
/// state: the count checks (`nNew` in `[0, 65536]`, `nTried` in `[0, 16384]`), the compatibility
/// check (a newer, incompatible format is [`FormatError::UnsupportedVersion`], which Core answers
/// by moving the file to `peers.dat.bak`), the checksum, and the network magic. Core's later
/// consistency check (`DbInconsistentError`) cannot be made here.
// Core: addrdb.cpp DeserializeDB, addrman.cpp AddrManImpl::Unserialize
pub fn parse(bytes: &[u8], network: Network) -> FormatResult<PeersDat> {
    check_size(bytes.len(), MAX_PEERS_DAT_SIZE, WHAT)?;
    let start = check_magic(bytes, network, WHAT)?;
    let mut r = Reader::at(bytes, start, WHAT);

    let format = r.u8()?;
    let compat = r.u8()?;
    if compat < INCOMPATIBILITY_BASE {
        return Err(r.corrupt("compat value below the minimum"));
    }
    let lowest_compatible = compat - INCOMPATIBILITY_BASE;
    if lowest_compatible > FILE_FORMAT {
        return Err(FormatError::UnsupportedVersion {
            what: WHAT,
            version: u64::from(format),
        });
    }
    let addrv2 = format >= V3_BIP155;

    let key = r.array::<32>()?;
    let n_new = r.i32_le()?;
    let n_tried = r.i32_le()?;
    let mut n_buckets = r.i32_le()?;
    if format >= V1_DETERMINISTIC {
        n_buckets ^= 1 << 30;
    }
    let new = read_entries(&mut r, n_new, NEW_BUCKET_COUNT * BUCKET_SIZE, addrv2)?;
    let tried = read_entries(&mut r, n_tried, TRIED_BUCKET_COUNT * BUCKET_SIZE, addrv2)?;

    let mut bucket_refs = Vec::new();
    let mut ignored_bucket_refs = 0u64;
    // A negative count runs no iteration, as in Core. Every iteration reads four bytes at
    // least, so a huge count ends in a truncation error and cannot loop long.
    for bucket in 0..n_buckets.max(0) as u32 {
        let entries = r.i32_le()?;
        for _ in 0..entries.max(0) {
            let index = r.i32_le()?;
            match u32::try_from(index).ok().filter(|i| (*i as usize) < new.len()) {
                Some(index) => bucket_refs.push(BucketRef {
                    bucket,
                    index,
                }),
                None => ignored_bucket_refs += 1,
            }
        }
    }
    let asmap_checksum = if format >= V2_ASMAP {
        r.array::<32>()?
    } else {
        [0; 32]
    };

    let trailing_bytes = check_checksum(bytes, r.pos(), WHAT)?;
    Ok(PeersDat {
        network,
        format,
        lowest_compatible,
        key,
        new,
        tried,
        new_bucket_count: n_buckets,
        bucket_refs,
        ignored_bucket_refs,
        asmap_checksum,
        trailing_bytes,
    })
}
