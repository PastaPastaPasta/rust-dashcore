//! Core's `CNetAddr`/`CService`/`CAddress` as `addrdb` writes them (`netaddress.h`,
//! `protocol.h`): the v1 form (16 bytes) and the BIP155 form (network id, compact-size length,
//! bytes), and the disk `CAddress` that carries its own version word.

use super::error::{FormatError, FormatResult};
use super::reader::Reader;

/// Core's `MAX_ADDRV2_SIZE`.
const MAX_ADDRV2_SIZE: u64 = 512;

// Core: netaddress.h
const IPV4_IN_IPV6_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];
const TORV2_IN_IPV6_PREFIX: [u8; 6] = [0xfd, 0x87, 0xd8, 0x7e, 0xeb, 0x43];
const INTERNAL_IN_IPV6_PREFIX: [u8; 6] = [0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24];
const CJDNS_PREFIX: u8 = 0xfc;

// Core: protocol.h CAddress::DISK_VERSION_IGNORE_MASK, DISK_VERSION_ADDRV2
pub(super) const DISK_VERSION_IGNORE_MASK: u32 = 0b0000_0000_0000_0111_1111_1111_1111_1111;
pub(super) const DISK_VERSION_ADDRV2: u32 = 1 << 29;

/// A network address. [`NetAddr::Invalid`] is what Core's deserializer leaves for an address it
/// ignores (an unknown BIP155 network, TORv2, IPv4 or TORv2 embedded in a BIP155 IPv6): not
/// valid, but the stream continues. Core represents it as the IPv6 address `::`
/// (`SetLegacyIPv6`, `UnserializeV2Stream`), so under `CNetAddr::operator==` and `operator<` it is
/// the same address as a stored `::`: here too, `Invalid == Ipv6([0; 16])`, and they hash alike.
#[derive(Debug, Clone, Copy)]
pub enum NetAddr {
    Ipv4([u8; 4]),
    Ipv6([u8; 16]),
    /// TORv3: the 32-byte public key.
    Onion([u8; 32]),
    I2p([u8; 32]),
    Cjdns([u8; 16]),
    /// Core's `NET_INTERNAL` (a name or FQDN hashed into 10 bytes).
    Internal([u8; 10]),
    Invalid,
}

impl NetAddr {
    /// The address as Core compares it: the network and the address bytes.
    fn key(&self) -> (u8, &[u8]) {
        match self {
            NetAddr::Ipv4(a) => (0, a),
            NetAddr::Ipv6(a) => (1, a),
            NetAddr::Onion(a) => (2, a),
            NetAddr::I2p(a) => (3, a),
            NetAddr::Cjdns(a) => (4, a),
            NetAddr::Internal(a) => (5, a),
            NetAddr::Invalid => (1, &[0; 16]),
        }
    }
}

impl PartialEq for NetAddr {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for NetAddr {}

/// `CNetAddr::operator<`: the network, then the address bytes (Core's `m_net` enum order is the
/// order of the `key` ranks).
// Core: netaddress.h CNetAddr::operator<
impl Ord for NetAddr {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for NetAddr {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::hash::Hash for NetAddr {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl NetAddr {
    /// Core's `CNetAddr::IsValid`.
    // Core: netaddress.cpp CNetAddr::IsValid
    pub fn is_valid(&self) -> bool {
        match self {
            NetAddr::Ipv4(a) => *a != [0; 4] && *a != [0xff; 4],
            // `::` and the documentation prefix 2001:db8::/32 (RFC 3849).
            NetAddr::Ipv6(a) => *a != [0; 16] && a[..4] != [0x20, 0x01, 0x0d, 0xb8],
            NetAddr::Cjdns(a) => a[0] == CJDNS_PREFIX,
            NetAddr::Onion(_) | NetAddr::I2p(_) => true,
            NetAddr::Internal(_) | NetAddr::Invalid => false,
        }
    }

    /// Core's `UnserializeV1Array` (`SetLegacyIPv6`).
    // Core: netaddress.cpp CNetAddr::SetLegacyIPv6
    fn from_legacy(ipv6: [u8; 16]) -> NetAddr {
        if ipv6.starts_with(&IPV4_IN_IPV6_PREFIX) {
            NetAddr::Ipv4(ipv6[12..].try_into().expect("4 bytes"))
        } else if ipv6.starts_with(&TORV2_IN_IPV6_PREFIX) {
            NetAddr::Invalid
        } else if ipv6.starts_with(&INTERNAL_IN_IPV6_PREFIX) {
            NetAddr::Internal(ipv6[6..].try_into().expect("10 bytes"))
        } else {
            NetAddr::Ipv6(ipv6)
        }
    }

    /// Core's `UnserializeV1Stream`.
    pub(crate) fn read_v1(r: &mut Reader<'_>) -> FormatResult<NetAddr> {
        Ok(NetAddr::from_legacy(r.array()?))
    }

    /// Core's `UnserializeV2Stream` (BIP155).
    // Core: netaddress.h CNetAddr::UnserializeV2Stream, netaddress.cpp SetNetFromBIP155Network
    pub(crate) fn read_v2(r: &mut Reader<'_>) -> FormatResult<NetAddr> {
        let net = r.u8()?;
        let size = r.compact_size(true)?;
        if size > MAX_ADDRV2_SIZE {
            return Err(r.corrupt("address longer than MAX_ADDRV2_SIZE"));
        }
        // `size` <= 512 here.
        let size = size as usize;
        let expected = match net {
            1 => Some(4),
            2 | 6 => Some(16),
            4 | 5 => Some(32),
            // TORv2 (3) is no longer supported and ids above 6 are unknown: skipped, not an error.
            _ => None,
        };
        let Some(expected) = expected else {
            r.take(size)?;
            return Ok(NetAddr::Invalid);
        };
        if size != expected {
            return Err(r.corrupt("BIP155 address with the wrong length for its network"));
        }
        let bytes = r.take(size)?;
        Ok(match net {
            1 => NetAddr::Ipv4(bytes.try_into().expect("4 bytes")),
            2 => {
                let a: [u8; 16] = bytes.try_into().expect("16 bytes");
                if a.starts_with(&INTERNAL_IN_IPV6_PREFIX) {
                    NetAddr::Internal(a[6..].try_into().expect("10 bytes"))
                } else if a.starts_with(&IPV4_IN_IPV6_PREFIX)
                    || a.starts_with(&TORV2_IN_IPV6_PREFIX)
                {
                    NetAddr::Invalid
                } else {
                    NetAddr::Ipv6(a)
                }
            }
            4 => NetAddr::Onion(bytes.try_into().expect("32 bytes")),
            5 => NetAddr::I2p(bytes.try_into().expect("32 bytes")),
            _ => NetAddr::Cjdns(bytes.try_into().expect("16 bytes")),
        })
    }
}

/// Core's `CService`: an address and a port. The derived order is `CService::operator<`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Service {
    pub addr: NetAddr,
    pub port: u16,
}

impl Service {
    /// A `CService` in a stream without `ADDRV2_FORMAT` (a `CDataStream(SER_DISK,
    /// CLIENT_VERSION)`): the 16-byte form and a big-endian port.
    // Core: netaddress.h CService::SERIALIZE_METHODS, CNetAddr::Unserialize
    pub(crate) fn read_v1(r: &mut Reader<'_>) -> FormatResult<Service> {
        let addr = NetAddr::read_v1(r)?;
        let port = r.u16_be()?;
        Ok(Service {
            addr,
            port,
        })
    }
}

/// Core's disk `CAddress`: a service (address and port) with the time it was last seen and the
/// services it offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Address {
    /// Seconds since the epoch (`uint32_t`).
    pub time: u32,
    pub services: u64,
    pub addr: NetAddr,
    pub port: u16,
    /// Whether this entry was stored in the BIP155 form (its disk version word said so).
    pub v2: bool,
}

/// The smallest a disk `CAddress` can be: version word, time, a one-byte service count, network id
/// and empty address, port.
pub(crate) const MIN_ADDRESS_SIZE: usize = 4 + 4 + 1 + 1 + 1 + 2;

impl Address {
    /// Reads one disk `CAddress`. `allow_v2` is whether the stream carries `ADDRV2_FORMAT`; a
    /// stored form that needs it is an error otherwise.
    // Core: protocol.h CAddress::SERIALIZE_METHODS (SER_DISK)
    pub(crate) fn read_disk(r: &mut Reader<'_>, allow_v2: bool) -> FormatResult<Address> {
        let stored = r.u32_le()? & !DISK_VERSION_IGNORE_MASK;
        let v2 = if stored == 0 {
            false
        } else if stored == DISK_VERSION_ADDRV2 && allow_v2 {
            true
        } else {
            return Err(FormatError::Corrupt {
                what: r.what(),
                reason: "unsupported CAddress disk format version",
            });
        };
        let time = r.u32_le()?;
        // `CompactSizeFormatter<false>`: canonical, no MAX_SIZE check.
        let services = if v2 {
            r.compact_size(false)?
        } else {
            r.u64_le()?
        };
        let addr = if v2 {
            NetAddr::read_v2(r)?
        } else {
            NetAddr::read_v1(r)?
        };
        let port = r.u16_be()?;
        Ok(Address {
            time,
            services,
            addr,
            port,
            v2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2(bytes: &[u8]) -> FormatResult<NetAddr> {
        NetAddr::read_v2(&mut Reader::new(bytes, "test"))
    }

    #[test]
    fn v1_prefixes() {
        let mut v4 = [0u8; 16];
        v4[10] = 0xff;
        v4[11] = 0xff;
        v4[12..].copy_from_slice(&[1, 2, 3, 4]);
        assert_eq!(NetAddr::from_legacy(v4), NetAddr::Ipv4([1, 2, 3, 4]));
        let mut tor2 = [7u8; 16];
        tor2[..6].copy_from_slice(&TORV2_IN_IPV6_PREFIX);
        assert!(matches!(NetAddr::from_legacy(tor2), NetAddr::Invalid));
        let mut internal = [9u8; 16];
        internal[..6].copy_from_slice(&INTERNAL_IN_IPV6_PREFIX);
        assert_eq!(NetAddr::from_legacy(internal), NetAddr::Internal([9; 10]));
        assert_eq!(NetAddr::from_legacy([0x20; 16]), NetAddr::Ipv6([0x20; 16]));
    }

    #[test]
    fn v2_networks_and_lengths() {
        assert_eq!(v2(&[1, 4, 1, 2, 3, 4]), Ok(NetAddr::Ipv4([1, 2, 3, 4])));
        // A wrong length for a founding network is an error; an unknown network is skipped.
        assert!(matches!(v2(&[1, 5, 1, 2, 3, 4, 5]), Err(FormatError::Corrupt { .. })));
        assert!(matches!(v2(&[9, 2, 0xaa, 0xbb]), Ok(NetAddr::Invalid)));
        assert!(matches!(v2(&[3, 1, 0xaa]), Ok(NetAddr::Invalid)), "TORv2");
        // Truncated payloads and oversized lengths are typed errors.
        assert!(matches!(v2(&[9, 2, 0xaa]), Err(FormatError::Truncated { .. })));
        assert!(matches!(v2(&[9, 0xfd, 0x01, 0x02]), Err(FormatError::Corrupt { .. })));
        // IPv4 embedded in a BIP155 IPv6 is ignored.
        let mut embedded = vec![2, 16];
        embedded.extend_from_slice(&IPV4_IN_IPV6_PREFIX);
        embedded.extend_from_slice(&[1, 2, 3, 4]);
        assert!(matches!(v2(&embedded), Ok(NetAddr::Invalid)));
    }

    /// Opus-high n3: Core keeps an ignored address as the IPv6 address `::`, which is equal to a
    /// stored `::` (`CNetAddr::operator==`, `operator<`: network and address bytes), and to nothing
    /// else.
    #[test]
    fn an_ignored_address_is_the_ipv6_unspecified_address() {
        use std::collections::HashSet;
        let zero = NetAddr::Ipv6([0; 16]);
        assert_eq!(NetAddr::Invalid, zero);
        assert_eq!(zero, NetAddr::Invalid);
        assert_eq!(NetAddr::Invalid, NetAddr::Invalid);
        let set: HashSet<_> = [NetAddr::Invalid, zero].into_iter().collect();
        assert_eq!(set.len(), 1, "they hash alike");
        // The same bytes on another network, and other IPv6 addresses, are different addresses.
        assert_ne!(NetAddr::Invalid, NetAddr::Cjdns([0; 16]));
        assert_ne!(NetAddr::Invalid, NetAddr::Ipv4([0; 4]));
        assert_ne!(NetAddr::Invalid, NetAddr::Ipv6([1; 16]));
        // Both are invalid, as in Core.
        assert!(!NetAddr::Invalid.is_valid() && !zero.is_valid());
    }

    #[test]
    fn validity() {
        assert!(NetAddr::Ipv4([8, 8, 8, 8]).is_valid());
        assert!(!NetAddr::Ipv4([0; 4]).is_valid());
        assert!(!NetAddr::Ipv4([255; 4]).is_valid());
        assert!(!NetAddr::Ipv6([0; 16]).is_valid());
        let mut doc = [0u8; 16];
        doc[..4].copy_from_slice(&[0x20, 0x01, 0x0d, 0xb8]);
        assert!(!NetAddr::Ipv6(doc).is_valid());
        assert!(!NetAddr::Cjdns([1; 16]).is_valid());
        assert!(NetAddr::Cjdns([0xfc; 16]).is_valid());
        assert!(NetAddr::Onion([1; 32]).is_valid());
        assert!(!NetAddr::Internal([0; 10]).is_valid());
    }

    #[test]
    fn disk_address_versions() {
        // v1 form, no ADDRV2 in the stream.
        let mut v1 = Vec::new();
        v1.extend_from_slice(&220_000u32.to_le_bytes());
        v1.extend_from_slice(&5u32.to_le_bytes());
        v1.extend_from_slice(&1u64.to_le_bytes());
        v1.extend_from_slice(&[0; 10]);
        v1.extend_from_slice(&[0xff, 0xff, 9, 9, 9, 9]);
        v1.extend_from_slice(&9999u16.to_be_bytes());
        let a = Address::read_disk(&mut Reader::new(&v1, "t"), false).unwrap();
        assert_eq!(
            (a.time, a.services, a.addr, a.port, a.v2),
            (5, 1, NetAddr::Ipv4([9; 4]), 9999, false)
        );
        // The v2 word is refused when the stream does not allow it, and so is any other word.
        let mut word = v1.clone();
        word[..4].copy_from_slice(&(DISK_VERSION_ADDRV2 | 220_000).to_le_bytes());
        assert!(Address::read_disk(&mut Reader::new(&word, "t"), false).is_err());
        word[..4].copy_from_slice(&(1u32 << 30).to_le_bytes());
        assert!(Address::read_disk(&mut Reader::new(&word, "t"), true).is_err());
    }
}
