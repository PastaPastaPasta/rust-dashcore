//! `peers.dat`: `AddrManImpl::Serialize`/`Unserialize` (format `V4_MULTIPORT`), the disk form of
//! `CAddress`/`CNetAddr` they use, and `addrdb.cpp`'s file framing (network magic, payload,
//! SHA-256d checksum).
//!
//! Errors carry Core's `what()` text: `std::ios_base::failure` messages get libstdc++'s
//! `": iostream error"` suffix, `std::runtime_error` messages do not.

use super::LoadError;
use super::imp::{AddrInfo, BUCKET_SIZE, Impl, NEW_BUCKET_COUNT, NEW_BUCKETS_PER_ADDRESS};
use super::imp::{TRIED_BUCKET_COUNT, slot_of};
use crate::netaddr::{Address, NetAddr, Network, Service};
use crate::netgroup::NetGroupManager;
use dash_p2p_core::ServiceFlags;

/// `AddrManImpl::Format::V1_DETERMINISTIC`.
const V1_DETERMINISTIC: u8 = 1;
/// `AddrManImpl::Format::V2_ASMAP`.
const V2_ASMAP: u8 = 2;
/// `AddrManImpl::Format::V3_BIP155`.
const V3_BIP155: u8 = 3;
/// `AddrManImpl::FILE_FORMAT` (`V4_MULTIPORT`).
const FILE_FORMAT: u8 = 4;
/// `AddrManImpl::INCOMPATIBILITY_BASE`.
const INCOMPATIBILITY_BASE: u8 = 32;
/// `CAddress::DISK_VERSION_INIT`.
const DISK_VERSION_INIT: u32 = 220_000;
/// `CAddress::DISK_VERSION_IGNORE_MASK`.
const DISK_VERSION_IGNORE_MASK: u32 = 0b0000_0000_0000_0111_1111_1111_1111_1111;
/// `CAddress::DISK_VERSION_ADDRV2` (= `ADDRV2_FORMAT`).
const DISK_VERSION_ADDRV2: u32 = 1 << 29;
/// `MAX_SIZE` (`serialize.h`), the `ReadCompactSize` range check.
const MAX_SIZE: u64 = 0x0200_0000;
/// `CNetAddr::MAX_ADDRV2_SIZE`.
const MAX_ADDRV2_SIZE: u64 = 512;
/// `AutoFile::read` at the end of the file (`streams.h`), as `what()`.
const EOF: &str = "AutoFile::read: end of file: iostream error";

/// An `std::ios_base::failure` with libstdc++'s `what()`.
fn ios(msg: impl core::fmt::Display) -> LoadError {
    LoadError::Corrupt(format!("{msg}: iostream error"))
}

/// `WriteCompactSize`.
pub(super) fn write_compact_size(out: &mut Vec<u8>, n: u64) {
    if n < 253 {
        out.push(n as u8);
    } else if n <= 0xFFFF {
        out.push(253);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n <= 0xFFFF_FFFF {
        out.push(254);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    } else {
        out.push(255);
        out.extend_from_slice(&n.to_le_bytes());
    }
}

/// The input stream: the file bytes, read front to back.
pub(super) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], LoadError> {
        if self.data.len() - self.pos < n {
            return Err(LoadError::Corrupt(EOF.to_owned()));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], LoadError> {
        Ok(self.take(N)?.try_into().expect("N bytes"))
    }

    fn u8(&mut self) -> Result<u8, LoadError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, LoadError> {
        self.array().map(u32::from_le_bytes)
    }

    fn i32(&mut self) -> Result<i32, LoadError> {
        self.array().map(i32::from_le_bytes)
    }

    fn i64(&mut self) -> Result<i64, LoadError> {
        self.array().map(i64::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, LoadError> {
        self.array().map(u64::from_le_bytes)
    }

    /// `ReadCompactSize(is, range_check)`.
    fn compact_size(&mut self, range_check: bool) -> Result<u64, LoadError> {
        let ch = self.u8()?;
        let n = match ch {
            0..=252 => u64::from(ch),
            253 => {
                let n = u64::from(u16::from_le_bytes(self.array()?));
                if n < 253 {
                    return Err(ios("non-canonical ReadCompactSize()"));
                }
                n
            }
            254 => {
                let n = u64::from(self.u32()?);
                if n < 0x1_0000 {
                    return Err(ios("non-canonical ReadCompactSize()"));
                }
                n
            }
            255 => {
                let n = self.u64()?;
                if n < 0x1_0000_0000 {
                    return Err(ios("non-canonical ReadCompactSize()"));
                }
                n
            }
        };
        if range_check && n > MAX_SIZE {
            return Err(ios("ReadCompactSize(): size too large"));
        }
        Ok(n)
    }
}

/// `CNetAddr::SerializeV2Stream`: internal addresses go out as IPv6 with the internal prefix.
fn write_netaddr_v2(out: &mut Vec<u8>, addr: &NetAddr) {
    let (id, bytes): (u8, Vec<u8>) = match addr.net() {
        Network::Internal => (2, addr.v1_bytes().to_vec()),
        Network::Ipv4 => (1, addr.bytes().to_vec()),
        Network::Ipv6 => (2, addr.bytes().to_vec()),
        Network::Onion => (4, addr.bytes().to_vec()),
        Network::I2p => (5, addr.bytes().to_vec()),
        Network::Cjdns => (6, addr.bytes().to_vec()),
        Network::Unroutable => unreachable!("never stored"),
    };
    out.push(id);
    write_compact_size(out, bytes.len() as u64);
    out.extend_from_slice(&bytes);
}

/// `CNetAddr::UnserializeV1Stream`.
fn read_netaddr_v1(r: &mut Reader<'_>) -> Result<NetAddr, LoadError> {
    Ok(NetAddr::from_legacy_ipv6(r.array()?))
}

/// `CNetAddr::UnserializeV2Stream` with `SetNetFromBIP155Network`.
// Core: netaddress.h:429 CNetAddr::UnserializeV2Stream
fn read_netaddr_v2(r: &mut Reader<'_>) -> Result<NetAddr, LoadError> {
    let bip155_net = r.u8()?;
    let size = r.compact_size(true)?;
    if size > MAX_ADDRV2_SIZE {
        return Err(ios(format_args!("Address too long: {size} > {MAX_ADDRV2_SIZE}")));
    }
    // Core: netaddress.cpp CNetAddr::SetNetFromBIP155Network
    let (net, expected, name) = match bip155_net {
        1 => (Network::Ipv4, 4, "IPv4"),
        2 => (Network::Ipv6, 16, "IPv6"),
        4 => (Network::Onion, 32, "TORv3"),
        5 => (Network::I2p, 32, "I2P"),
        6 => (Network::Cjdns, 16, "CJDNS"),
        _ => {
            // unknown network id: skip it and read as !IsValid()
            r.take(size as usize)?;
            return Ok(NetAddr::default());
        }
    };
    if size != expected {
        return Err(ios(format_args!(
            "BIP155 {name} address with length {size} (should be {expected})"
        )));
    }
    let bytes = r.take(size as usize)?;
    if net != Network::Ipv6 {
        return Ok(NetAddr::new(net, bytes.to_vec()).expect("length checked"));
    }
    // Do some special checks on IPv6 addresses.
    const INTERNAL_IN_IPV6_PREFIX: [u8; 6] = [0xFD, 0x6B, 0x88, 0xC0, 0x87, 0x24];
    const IPV4_IN_IPV6_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
    const TORV2_IN_IPV6_PREFIX: [u8; 6] = [0xFD, 0x87, 0xD8, 0x7E, 0xEB, 0x43];
    if bytes.starts_with(&INTERNAL_IN_IPV6_PREFIX) {
        return Ok(NetAddr::new(Network::Internal, bytes[6..].to_vec()).expect("10 bytes"));
    }
    if bytes.starts_with(&IPV4_IN_IPV6_PREFIX) || bytes.starts_with(&TORV2_IN_IPV6_PREFIX) {
        // IPv4 and TORv2 are not supposed to be embedded in IPv6: unserialize as !IsValid().
        return Ok(NetAddr::default());
    }
    Ok(NetAddr::new(Network::Ipv6, bytes.to_vec()).expect("16 bytes"))
}

/// `CAddress` in the disk format, written with `ADDRV2_FORMAT` in the stream version: the
/// version word with `DISK_VERSION_ADDRV2`, `nTime`, `nServices` as a CompactSize, the BIP155
/// `CService`.
// Core: protocol.h:432 CAddress::SERIALIZE_METHODS
fn write_caddress(out: &mut Vec<u8>, service: &Service, time: u32, services: ServiceFlags) {
    out.extend_from_slice(&(DISK_VERSION_INIT | DISK_VERSION_ADDRV2).to_le_bytes());
    out.extend_from_slice(&time.to_le_bytes());
    write_compact_size(out, services.0);
    write_netaddr_v2(out, &service.addr);
    out.extend_from_slice(&service.port.to_be_bytes());
}

/// Reads a disk-format `CAddress`; `addrv2` is `ADDRV2_FORMAT` in the stream version, without
/// which a V2 record is refused.
// Core: protocol.h:432 CAddress::SERIALIZE_METHODS
fn read_caddress(r: &mut Reader<'_>, addrv2: bool) -> Result<Address, LoadError> {
    let stored = r.u32()? & !DISK_VERSION_IGNORE_MASK;
    let use_v2 = if stored == 0 {
        false
    } else if stored == DISK_VERSION_ADDRV2 && addrv2 {
        true
    } else {
        return Err(ios("Unsupported CAddress disk format version"));
    };
    let time = r.u32()?;
    let services = if use_v2 {
        r.compact_size(false)?
    } else {
        r.u64()?
    };
    let addr = if use_v2 {
        read_netaddr_v2(r)?
    } else {
        read_netaddr_v1(r)?
    };
    let port = u16::from_be_bytes(r.array()?);
    Ok(Address {
        service: Service::new(addr, port),
        services: ServiceFlags(services),
        time,
    })
}

/// `AddrInfo` in the disk format: `CAddress` (`SER_DISK`, `ADDRV2_FORMAT` set, so the stored
/// version word carries `DISK_VERSION_ADDRV2`), then `source`, `m_last_success`, `nAttempts`.
// Core: addrman_impl.h:66 AddrInfo::SERIALIZE_METHODS, protocol.h:432 CAddress::SERIALIZE_METHODS
fn write_addr_info(out: &mut Vec<u8>, info: &AddrInfo) {
    write_caddress(out, &info.service, info.time as u32, info.services);
    write_netaddr_v2(out, &info.source);
    out.extend_from_slice(&info.last_success.to_le_bytes());
    out.extend_from_slice(&info.attempts.to_le_bytes());
}

/// Reads an `AddrInfo`; `addrv2` is `ADDRV2_FORMAT` in the stream version (format ≥ V3).
fn read_addr_info(r: &mut Reader<'_>, addrv2: bool) -> Result<AddrInfo, LoadError> {
    let a = read_caddress(r, addrv2)?;
    let source = if addrv2 {
        read_netaddr_v2(r)?
    } else {
        read_netaddr_v1(r)?
    };
    let last_success = r.i64()?;
    let attempts = r.i32()?;
    Ok(AddrInfo {
        service: a.service,
        time: i64::from(a.time),
        services: a.services,
        source,
        last_success,
        attempts,
        ..AddrInfo::default()
    })
}

/// `NetGroupManager::GetAsmapChecksum`: `uint256()` without an asmap, else SHA-256d of the
/// serialized `std::vector<bool>` (CompactSize count, one byte per bit).
// Core: netgroup.cpp:11 NetGroupManager::GetAsmapChecksum
pub(super) fn asmap_checksum(netgroup: &NetGroupManager) -> [u8; 32] {
    let bits = netgroup.asmap();
    if bits.is_empty() {
        return [0; 32];
    }
    let mut buf = Vec::with_capacity(bits.len() + 9);
    write_compact_size(&mut buf, bits.len() as u64);
    buf.extend(bits.iter().map(|&b| u8::from(b)));
    dash_sha256::sha256d(&buf)
}

impl Impl {
    // Core: addrman.cpp:134 AddrManImpl::Serialize
    pub(super) fn serialize(&self, out: &mut Vec<u8>) {
        out.push(FILE_FORMAT);
        // Increment `lowest_compatible` iff a newly introduced format is incompatible with
        // the previous one.
        out.push(INCOMPATIBILITY_BASE + FILE_FORMAT);
        out.extend_from_slice(&self.key);
        out.extend_from_slice(&self.n_new.to_le_bytes());
        out.extend_from_slice(&self.n_tried.to_le_bytes());
        out.extend_from_slice(&((NEW_BUCKET_COUNT as i32) ^ (1 << 30)).to_le_bytes());

        let mut unk_ids = std::collections::HashMap::new();
        let mut ids = 0i32;
        for (id, info) in self.map_info.iter() {
            unk_ids.insert(id, ids);
            if info.ref_count != 0 {
                write_addr_info(out, info);
                ids += 1;
            }
        }
        for (_, info) in self.map_info.iter() {
            if info.in_tried {
                write_addr_info(out, info);
            }
        }
        for bucket in 0..NEW_BUCKET_COUNT {
            let row = &self.new[bucket * BUCKET_SIZE..(bucket + 1) * BUCKET_SIZE];
            let size = row.iter().filter(|&&id| id != -1).count() as i32;
            out.extend_from_slice(&size.to_le_bytes());
            for &id in row.iter().filter(|&&id| id != -1) {
                // `mapUnkIds[id]`: operator[] yields 0 for an id it has not seen.
                let index = unk_ids.get(&id).copied().unwrap_or(0);
                out.extend_from_slice(&index.to_le_bytes());
            }
        }
        // Store asmap checksum after bucket entries so that it
        // can be ignored by older clients for backward compatibility.
        out.extend_from_slice(&asmap_checksum(&self.netgroup));
    }

    /// Core's `Unserialize` into a fresh instance (Core asserts `vRandom` is empty).
    // Core: addrman.cpp:233 AddrManImpl::Unserialize
    pub(super) fn unserialize(&mut self, r: &mut Reader<'_>) -> Result<(), LoadError> {
        assert!(self.random.is_empty());

        let format = r.u8()?;
        // Add ADDRV2_FORMAT to the version so that the CNetAddr and CAddress
        // unserialize methods know that an address in addrv2 format is coming.
        let addrv2 = format >= V3_BIP155;

        let compat = r.u8()?;
        if compat < INCOMPATIBILITY_BASE {
            return Err(ios(format_args!(
                "Corrupted addrman database: The compat value ({compat}) is lower than the \
                 expected minimum value {INCOMPATIBILITY_BASE}."
            )));
        }
        let lowest_compatible = compat - INCOMPATIBILITY_BASE;
        if lowest_compatible > FILE_FORMAT {
            return Err(LoadError::Incompatible {
                format,
                lowest_compatible,
            });
        }

        self.key = r.array()?;
        self.n_new = r.i32()?;
        self.n_tried = r.i32()?;
        let mut u_buckets = r.i32()?;
        if format >= V1_DETERMINISTIC {
            u_buckets ^= 1 << 30;
        }

        let max_new = (NEW_BUCKET_COUNT * BUCKET_SIZE) as i32;
        if self.n_new > max_new || self.n_new < 0 {
            return Err(ios(format_args!(
                "Corrupt AddrMan serialization: nNew={}, should be in [0, {max_new}]",
                self.n_new
            )));
        }
        let max_tried = (TRIED_BUCKET_COUNT * BUCKET_SIZE) as i32;
        if self.n_tried > max_tried || self.n_tried < 0 {
            return Err(ios(format_args!(
                "Corrupt AddrMan serialization: nTried={}, should be in [0, {max_tried}]",
                self.n_tried
            )));
        }

        // Deserialize entries from the new table.
        for n in 0..self.n_new {
            let mut info = read_addr_info(r, addrv2)?;
            info.random_pos = self.random.len() as i32;
            let net = self.network_of(&info.service.addr);
            self.map_addr.insert(info.service.clone(), n);
            self.map_info.insert_new(n, info);
            self.random.push(n);
            self.counts_mut(net).n_new += 1;
        }
        self.id_count = self.n_new;

        // Deserialize entries from the tried table.
        let mut lost = 0;
        for _ in 0..self.n_tried {
            let mut info = read_addr_info(r, addrv2)?;
            let k_bucket = info.tried_bucket(&self.key, &self.netgroup);
            let k_bucket_pos = info.bucket_position(&self.key, false, k_bucket);
            if info.service.addr.is_valid() && self.tried[slot_of(k_bucket, k_bucket_pos)] == -1 {
                let id = self.id_count;
                info.random_pos = self.random.len() as i32;
                info.in_tried = true;
                let net = self.network_of(&info.service.addr);
                self.random.push(id);
                self.map_addr.insert(info.service.clone(), id);
                self.map_info.insert_new(id, info);
                self.tried[slot_of(k_bucket, k_bucket_pos)] = id;
                self.id_count += 1;
                self.counts_mut(net).n_tried += 1;
            } else {
                lost += 1;
            }
        }
        self.n_tried -= lost;

        // Store positions in the new table buckets to apply later (if possible).
        let mut bucket_entries: Vec<(i32, i32)> = Vec::new();
        for bucket in 0..u_buckets {
            let num_entries = r.i32()?;
            for _ in 0..num_entries {
                let entry_index = r.i32()?;
                if entry_index >= 0 && entry_index < self.n_new {
                    bucket_entries.push((bucket, entry_index));
                }
            }
        }

        // If the bucket count and asmap checksum haven't changed, then attempt
        // to restore the entries to the buckets/positions they were in before
        // serialization.
        let supplied_asmap_checksum = asmap_checksum(&self.netgroup);
        let serialized_asmap_checksum = if format >= V2_ASMAP {
            r.array()?
        } else {
            [0u8; 32]
        };
        let restore_bucketing = u_buckets == NEW_BUCKET_COUNT as i32
            && serialized_asmap_checksum == supplied_asmap_checksum;

        if !restore_bucketing {
            tracing::debug!(target: "addrman", "Bucketing method was updated, re-bucketing addrman entries from disk");
        }

        for (bucket, entry_index) in bucket_entries {
            let key = self.key;
            let info = self.info(entry_index);

            // Don't store the entry in the new bucket if it's not a valid address for our addrman
            if !info.service.addr.is_valid() {
                continue;
            }

            // The entry shouldn't appear in more than ADDRMAN_NEW_BUCKETS_PER_ADDRESS. If it
            // has already, just skip this bucket_entry.
            if info.ref_count >= NEW_BUCKETS_PER_ADDRESS {
                continue;
            }

            let bucket_position = info.bucket_position(&key, true, bucket);
            if restore_bucketing && self.new[slot_of(bucket, bucket_position)] == -1 {
                // Bucketing has not changed, using existing bucket positions for the new table
                self.new[slot_of(bucket, bucket_position)] = entry_index;
                self.info_mut(entry_index).ref_count += 1;
            } else {
                // In case the new table data cannot be used (bucket count wrong or new asmap),
                // try to give them a reference based on their primary source address.
                let bucket = info.new_bucket(&key, &self.netgroup);
                let bucket_position = info.bucket_position(&key, true, bucket);
                if self.new[slot_of(bucket, bucket_position)] == -1 {
                    self.new[slot_of(bucket, bucket_position)] = entry_index;
                    self.info_mut(entry_index).ref_count += 1;
                }
            }
        }

        // Prune new entries with refcount 0 (as a result of collisions or invalid address), in
        // mapInfo's iteration order (erasing does not reorder the rest).
        let mut lost_unk = 0;
        for id in self.map_info.keys() {
            let info = self.info(id);
            if !info.in_tried && info.ref_count == 0 {
                self.delete(id);
                lost_unk += 1;
            }
        }
        if lost + lost_unk > 0 {
            tracing::debug!(target: "addrman", "addrman lost {lost_unk} new and {lost} tried addresses due to collisions or invalid addresses");
        }

        let check_code = self.check_addrman();
        if check_code != 0 {
            return Err(LoadError::Inconsistent(check_code));
        }
        Ok(())
    }
}

/// `SerializeDB` for `peers.dat`: the magic, the payload, then SHA-256d of both.
// Core: addrdb.cpp:38 SerializeDB
pub(super) fn write_file(magic: &[u8; 4], addrman: &Impl) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 1024 * 4 + 32 + 64 * addrman.random.len());
    out.extend_from_slice(magic);
    addrman.serialize(&mut out);
    let hash = dash_sha256::sha256d(&out);
    out.extend_from_slice(&hash);
    out
}

/// `DeserializeDB` with the checksum: the magic first, then the whole payload (including
/// `Unserialize`'s version and consistency errors), and only then the trailing hash. Bytes after
/// the hash are ignored.
// Core: addrdb.cpp:96 DeserializeDB
pub(super) fn read_file(
    magic: &[u8; 4],
    bytes: &[u8],
    addrman: &mut Impl,
) -> Result<(), LoadError> {
    let mut r = Reader::new(bytes);
    let file_magic: [u8; 4] = r.array()?;
    if file_magic != *magic {
        return Err(LoadError::Corrupt("Invalid network magic number".to_owned()));
    }
    addrman.unserialize(&mut r)?;
    let hashed = &bytes[..r.pos];
    let stored: [u8; 32] = r.array()?;
    if stored != dash_sha256::sha256d(hashed) {
        return Err(LoadError::Corrupt("Checksum mismatch, data corrupted".to_owned()));
    }
    Ok(())
}

/// `DumpAnchors`'s image: `SerializeDB` of a `std::vector<CAddress>` with `ADDRV2_FORMAT` (the
/// network magic, the vector, SHA-256d of both).
// Core: addrdb.cpp:233-237 DumpAnchors, :38-51 SerializeDB
pub(crate) fn write_anchors(magic: &[u8; 4], anchors: &[Address]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 1 + 64 * anchors.len() + 32);
    out.extend_from_slice(magic);
    write_compact_size(&mut out, anchors.len() as u64);
    for a in anchors {
        write_caddress(&mut out, &a.service, a.time, a.services);
    }
    let hash = dash_sha256::sha256d(&out);
    out.extend_from_slice(&hash);
    out
}

/// `ReadAnchors`'s `DeserializeDB`: the magic, the vector, then the checksum. The caller treats
/// any error as no anchors, as Core's catch-all does.
// Core: addrdb.cpp:239-250 ReadAnchors, :96-119 DeserializeDB
pub(crate) fn read_anchors(magic: &[u8; 4], bytes: &[u8]) -> Result<Vec<Address>, LoadError> {
    let mut r = Reader::new(bytes);
    let file_magic: [u8; 4] = r.array()?;
    if file_magic != *magic {
        return Err(LoadError::Corrupt("Invalid network magic number".to_owned()));
    }
    let n = r.compact_size(true)?;
    let mut anchors = Vec::new();
    for _ in 0..n {
        anchors.push(read_caddress(&mut r, true)?);
    }
    let hashed = &bytes[..r.pos];
    let stored: [u8; 32] = r.array()?;
    if stored != dash_sha256::sha256d(hashed) {
        return Err(LoadError::Corrupt("Checksum mismatch, data corrupted".to_owned()));
    }
    Ok(anchors)
}
