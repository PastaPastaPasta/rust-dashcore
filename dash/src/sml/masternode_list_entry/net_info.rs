//! Dash Core's `ExtNetInfo`, the extended service addresses of ProTx version 3.
//!
//! Decoding follows Core's `Unserialize` byte for byte (v24.0.0-rc.4), including what it
//! tolerates and what it refuses: the purposes must be known and strictly ascending, or the read
//! fails (#7888, `evo/netinfo.h` rc.4:419-437), and Core reads a service address it can't use as
//! an invalid entry instead of failing. [`ExtNetInfo`] keeps the bytes as they were decoded, so a
//! transaction re-encodes to the same bytes and txid, and [`ExtNetInfo::canonical`] gives the
//! object Core holds, which is what Core serializes again for payload hashes and signatures.
//!
//! Whether the addresses are acceptable for a masternode (`ExtNetInfo::Validate` in Core: entry
//! counts, routability, ports, duplicates) is a validation rule and not checked here.

use std::cmp;
use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};

#[cfg(feature = "bincode")]
use bincode::{Decode, Encode};

use crate::consensus::encode::{Error, MAX_COMPACT_SIZE, VarInt};
use crate::consensus::{Decodable, Encodable};

const EXTNETINFO_CURRENT_VERSION: u8 = 1;

const NET_TYPE_SERVICE: u8 = 0x01;
const NET_TYPE_DOMAIN: u8 = 0x02;
const NET_TYPE_INVALID: u8 = 0xff;

/// Core's `CNetAddr::MAX_ADDRV2_SIZE`.
const MAX_ADDRV2_SIZE: u64 = 512;
/// Core's `DOMAIN_MAX_LEN`, applied when decoding a domain.
const DOMAIN_MAX_LEN: u64 = 253;
const DOMAIN_MIN_LEN: usize = 3;
const DOMAIN_LABEL_MAX_LEN: usize = 63;

const IPV4_IN_IPV6_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];
const TORV2_IN_IPV6_PREFIX: [u8; 6] = [0xfd, 0x87, 0xd8, 0x7e, 0xeb, 0x43];
const INTERNAL_IN_IPV6_PREFIX: [u8; 6] = [0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24];
const RFC3849_PREFIX: [u8; 4] = [0x20, 0x01, 0x0d, 0xb8];
const CJDNS_PREFIX: u8 = 0xfc;

/// BIP155 network identifiers as used inside an ADDRV2-form `CService`.
///
/// Core reads an address with another network id (including Tor v2, id 3) as invalid.
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Bip155Network {
    Ipv4,
    Ipv6,
    TorV3,
    I2p,
    Cjdns,
}

impl Bip155Network {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Bip155Network::Ipv4),
            2 => Some(Bip155Network::Ipv6),
            4 => Some(Bip155Network::TorV3),
            5 => Some(Bip155Network::I2p),
            6 => Some(Bip155Network::Cjdns),
            _ => None,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Bip155Network::Ipv4 => 1,
            Bip155Network::Ipv6 => 2,
            Bip155Network::TorV3 => 4,
            Bip155Network::I2p => 5,
            Bip155Network::Cjdns => 6,
        }
    }

    fn address_len(self) -> usize {
        match self {
            Bip155Network::Ipv4 => 4,
            Bip155Network::Ipv6 => 16,
            Bip155Network::TorV3 => 32,
            Bip155Network::I2p => 32,
            Bip155Network::Cjdns => 16,
        }
    }
}

/// A single network info entry within an `ExtNetInfo` purpose list.
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum NetInfoEntry {
    /// A service address. Decoding only produces addresses `CService::IsValid` accepts.
    Service {
        network: Bip155Network,
        addr: Vec<u8>,
        port: u16,
    },
    /// A domain and port. Decoding only produces domains `DomainPort::IsValid` accepts.
    Domain {
        host: String,
        port: u16,
    },
    /// An invalid entry written as Core writes one: the type byte `0xff` alone.
    Invalid,
    /// An entry Core reads as invalid that isn't written as [`NetInfoEntry::Invalid`]: a service
    /// address Core can't read or rejects, or a domain that fails `DomainPort::IsValid`. Holds the
    /// bytes Core consumed for it, so it re-encodes the same; [`ExtNetInfo::canonical`] turns it
    /// into [`NetInfoEntry::Invalid`].
    Malformed(Vec<u8>),
    /// An entry whose type byte Core doesn't know (none of 1, 2 and `0xff`): Core reads that byte
    /// alone and holds the entry as invalid, so [`ExtNetInfo::canonical`] turns it into
    /// [`NetInfoEntry::Invalid`].
    UnknownType(u8),
}

impl NetInfoEntry {
    /// Whether Core keeps this entry as an address rather than reading it as invalid
    /// (`CService::IsValid` or `DomainPort::IsValid`).
    pub fn is_valid(&self) -> bool {
        match self {
            NetInfoEntry::Service {
                network,
                addr,
                ..
            } => addr.len() == network.address_len() && service_is_valid(*network, addr),
            NetInfoEntry::Domain {
                host,
                port,
            } => domain_is_valid(host.as_bytes(), *port),
            NetInfoEntry::Invalid | NetInfoEntry::Malformed(_) | NetInfoEntry::UnknownType(_) => {
                false
            }
        }
    }

    /// The entry as Core holds it: itself if valid, else [`NetInfoEntry::Invalid`].
    pub fn canonical(&self) -> NetInfoEntry {
        if self.is_valid() {
            self.clone()
        } else {
            NetInfoEntry::Invalid
        }
    }

    /// The serialized size of the entry in bytes.
    fn size(&self) -> usize {
        match self {
            NetInfoEntry::Service {
                addr,
                ..
            } => 1 + 1 + VarInt(addr.len() as u64).len() + addr.len() + 2,
            NetInfoEntry::Domain {
                host,
                ..
            } => 1 + VarInt(host.len() as u64).len() + host.len() + 2,
            NetInfoEntry::Invalid | NetInfoEntry::UnknownType(_) => 1,
            NetInfoEntry::Malformed(raw) => raw.len(),
        }
    }

    fn consensus_encode_ext<W: Write + ?Sized>(&self, writer: &mut W) -> Result<usize, io::Error> {
        let mut len = 0;
        match self {
            NetInfoEntry::Service {
                network,
                addr,
                port,
            } => {
                len += NET_TYPE_SERVICE.consensus_encode(writer)?;
                len += network.to_u8().consensus_encode(writer)?;
                len += VarInt(addr.len() as u64).consensus_encode(writer)?;
                writer.write_all(addr)?;
                len += addr.len();
                writer.write_all(&port.to_be_bytes())?;
                len += 2;
            }
            NetInfoEntry::Domain {
                host,
                port,
            } => {
                len += NET_TYPE_DOMAIN.consensus_encode(writer)?;
                len += host.consensus_encode(writer)?;
                writer.write_all(&port.to_be_bytes())?;
                len += 2;
            }
            NetInfoEntry::Invalid => {
                len += NET_TYPE_INVALID.consensus_encode(writer)?;
            }
            NetInfoEntry::Malformed(raw) => {
                writer.write_all(raw)?;
                len += raw.len();
            }
            NetInfoEntry::UnknownType(net_type) => {
                len += net_type.consensus_encode(writer)?;
            }
        }
        Ok(len)
    }

    /// Decodes an entry as Core's `NetInfoEntry::Unserialize` does.
    fn consensus_decode_ext<R: Read + ?Sized>(
        reader: &mut CoreStream<'_, R>,
    ) -> Result<Self, Error> {
        let net_type: u8 = Decodable::consensus_decode(reader)?;
        match net_type {
            NET_TYPE_SERVICE => {
                let mut raw = vec![NET_TYPE_SERVICE];
                match decode_service(reader, &mut raw)? {
                    Some(entry) if entry.is_valid() => Ok(entry),
                    _ => Ok(NetInfoEntry::Malformed(raw)),
                }
            }
            NET_TYPE_DOMAIN => decode_domain(reader),
            NET_TYPE_INVALID => Ok(NetInfoEntry::Invalid),
            other => Ok(NetInfoEntry::UnknownType(other)),
        }
    }
}

/// Reads the ADDRV2 `CService` of a service entry, appending every consumed byte to `raw`.
///
/// Returns `None` where Core's read throws: Core catches that, marks the entry invalid and goes
/// on after the bytes it consumed. A read past the end of the data consumes nothing, as in
/// Core's `DataStream`. An unknown network id yields `None` after its address and port were
/// consumed, as Core reads such an address as invalid.
fn decode_service<R: Read + ?Sized>(
    reader: &mut CoreStream<'_, R>,
    raw: &mut Vec<u8>,
) -> Result<Option<NetInfoEntry>, io::Error> {
    let mut take = |len: usize| -> Result<Option<Vec<u8>>, io::Error> {
        let mut buf = vec![0u8; len];
        if !reader.read_all_or_nothing(&mut buf)? {
            return Ok(None);
        }
        raw.extend_from_slice(&buf);
        Ok(Some(buf))
    };
    let Some(network_id) = take(1)? else {
        return Ok(None);
    };
    // Core's `ReadCompactSize` reads the marker and then the value, each failing on its own, and
    // throws on a non-canonical size after reading it.
    let Some(mut size) = take(1)? else {
        return Ok(None);
    };
    let width = match size[0] {
        0xfd => 2,
        0xfe => 4,
        0xff => 8,
        _ => 0,
    };
    let Some(value) = take(width)? else {
        return Ok(None);
    };
    size.extend(value);
    let Ok(VarInt(address_len)) = VarInt::consensus_decode(&mut size.as_slice()) else {
        return Ok(None);
    };
    if address_len > MAX_ADDRV2_SIZE {
        return Ok(None);
    }
    let address_len = address_len as usize;
    let network = match Bip155Network::from_u8(network_id[0]) {
        Some(network) if network.address_len() != address_len => return Ok(None),
        network => network,
    };
    let Some(addr) = take(address_len)? else {
        return Ok(None);
    };
    let Some(port) = take(2)? else {
        return Ok(None);
    };
    Ok(network.map(|network| NetInfoEntry::Service {
        network,
        addr,
        port: u16::from_be_bytes([port[0], port[1]]),
    }))
}

/// Reads a domain entry. Unlike a service address, Core lets a failure here (a length over
/// [`DOMAIN_MAX_LEN`], the end of the data) fail the whole payload.
fn decode_domain<R: Read + ?Sized>(reader: &mut R) -> Result<NetInfoEntry, Error> {
    let len = VarInt::consensus_decode(reader)?.0;
    if len > DOMAIN_MAX_LEN {
        return Err(Error::ParseFailed("domain longer than 253 bytes"));
    }
    let mut host = vec![0u8; len as usize];
    reader.read_exact(&mut host)?;
    let mut port = [0u8; 2];
    reader.read_exact(&mut port)?;
    let port = u16::from_be_bytes(port);
    if domain_is_valid(&host, port) {
        let host = String::from_utf8(host).expect("valid domains are ASCII");
        return Ok(NetInfoEntry::Domain {
            host,
            port,
        });
    }
    let mut raw = vec![NET_TYPE_DOMAIN];
    VarInt(len).consensus_encode(&mut raw).expect("vectors don't error");
    raw.extend_from_slice(&host);
    raw.extend_from_slice(&port.to_be_bytes());
    Ok(NetInfoEntry::Malformed(raw))
}

/// Core's `CNetAddr::IsValid` for an ADDRV2 address, after the IPv6 special cases of
/// `UnserializeV2Stream` (embedded IPv4, Tor v2 and internal addresses all end up invalid).
fn service_is_valid(network: Bip155Network, addr: &[u8]) -> bool {
    match network {
        Bip155Network::Ipv4 => ipv4_is_valid(addr),
        Bip155Network::Ipv6 => !addr.starts_with(&IPV4_IN_IPV6_PREFIX) && ipv6_is_valid(addr),
        Bip155Network::TorV3 | Bip155Network::I2p => true,
        Bip155Network::Cjdns => addr[0] == CJDNS_PREFIX,
    }
}

/// Core's `CNetAddr::IsValid` for a legacy (ADDRV1) 16-byte address, as `SetLegacyIPv6` reads
/// it: an embedded IPv4 address is checked as IPv4, Tor v2 and internal addresses are invalid.
fn legacy_address_is_valid(addr: &[u8; 16]) -> bool {
    match addr.strip_prefix(&IPV4_IN_IPV6_PREFIX) {
        Some(ipv4) => ipv4_is_valid(ipv4),
        None => ipv6_is_valid(addr),
    }
}

/// A legacy (ADDRV1) service address, 16 address bytes and a big-endian port, as Core serializes
/// it again: `MnNetInfo` holds an address `CService::IsValid` rejects as no address, which it
/// writes as `CService{}`, all zeros.
pub(crate) fn canonical_legacy_service(encoded: [u8; 18]) -> [u8; 18] {
    let addr: &[u8; 16] = encoded[..16].try_into().expect("16 bytes");
    if legacy_address_is_valid(addr) {
        encoded
    } else {
        [0; 18]
    }
}

/// Neither `INADDR_ANY` nor `INADDR_NONE`.
fn ipv4_is_valid(addr: &[u8]) -> bool {
    addr != [0; 4] && addr != [0xff; 4]
}

/// Neither unspecified (`::`), documentation (RFC 3849, `2001:db8::/32`), nor a Tor v2 or
/// internal address in IPv6 form.
fn ipv6_is_valid(addr: &[u8]) -> bool {
    addr != [0; 16]
        && !addr.starts_with(&RFC3849_PREFIX)
        && !addr.starts_with(&TORV2_IN_IPV6_PREFIX)
        && !addr.starts_with(&INTERNAL_IN_IPV6_PREFIX)
}

/// Core's `DomainPort::Validate`: a non-zero port and a lowercase RFC 1035 name of 3 to 253
/// characters, with at least two labels of 1 to 63 characters that don't start or end with `-`.
fn domain_is_valid(host: &[u8], port: u16) -> bool {
    let len_ok = (DOMAIN_MIN_LEN..=DOMAIN_MAX_LEN as usize).contains(&host.len());
    let chars_ok = host
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'-'));
    if port == 0 || !len_ok || !chars_ok || host[0] == b'.' || host[host.len() - 1] == b'.' {
        return false;
    }
    let mut labels = 0;
    for label in host.split(|c| *c == b'.') {
        if label.is_empty()
            || label.len() > DOMAIN_LABEL_MAX_LEN
            || label[0] == b'-'
            || label[label.len() - 1] == b'-'
        {
            return false;
        }
        labels += 1;
    }
    labels >= 2
}

/// Purpose codes keyed in an `ExtNetInfo` map.
#[derive(Clone, Copy, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum NetInfoPurpose {
    CoreP2P,
    PlatformP2P,
    PlatformHttps,
    /// A code Core doesn't define (3 and up). Decoding never makes it: Core's read fails on it
    /// (rc.4).
    Unknown(u8),
}

impl NetInfoPurpose {
    /// The purpose with code `value`.
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => NetInfoPurpose::CoreP2P,
            1 => NetInfoPurpose::PlatformP2P,
            2 => NetInfoPurpose::PlatformHttps,
            code => NetInfoPurpose::Unknown(code),
        }
    }

    /// The purpose's code.
    pub fn to_u8(self) -> u8 {
        match self {
            NetInfoPurpose::CoreP2P => 0,
            NetInfoPurpose::PlatformP2P => 1,
            NetInfoPurpose::PlatformHttps => 2,
            NetInfoPurpose::Unknown(code) => code,
        }
    }
}

/// Variable-length extended network info introduced for ProTx version 3.
///
/// `purposes` holds the purpose lists as decoded, including [`NetInfoEntry::Malformed`] entries,
/// so that a decode followed by an encode reproduces the original bytes; a decoded value's
/// purposes are known and strictly ascending. Core reads malformed entries as invalid;
/// [`Self::canonical`] and [`Self::entries_for`] give that view. Equality, ordering and hashing
/// compare the wire form: two values Core holds the same may differ, compare their
/// [`Self::canonical`] forms for that.
#[derive(Clone, Ord, PartialOrd, Eq, PartialEq, Debug, Hash)]
#[cfg_attr(feature = "bincode", derive(Encode, Decode))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ExtNetInfo {
    pub version: u8,
    pub purposes: Vec<(NetInfoPurpose, Vec<NetInfoEntry>)>,
}

impl ExtNetInfo {
    /// Whether the version carries the purpose map; Core reads nothing more for other versions.
    fn has_map(&self) -> bool {
        self.version != 0 && self.version <= EXTNETINFO_CURRENT_VERSION
    }

    /// The object Core decodes this into: every entry Core reads as invalid as
    /// [`NetInfoEntry::Invalid`], and no lists for a version without the purpose map.
    ///
    /// Its encoding is what Core serializes again wherever it re-encodes a ProTx payload, such
    /// as the payload hash that ProTx signatures cover.
    pub fn canonical(&self) -> ExtNetInfo {
        let purposes = if self.has_map() {
            self.purposes
                .iter()
                .map(|(purpose, entries)| {
                    (*purpose, entries.iter().map(NetInfoEntry::canonical).collect())
                })
                .collect()
        } else {
            Vec::new()
        };
        ExtNetInfo {
            version: self.version,
            purposes,
        }
    }

    /// The serialized size of the network info in bytes.
    pub fn size(&self) -> usize {
        // Unknown versions carry no further payload, mirroring Core's short-circuit.
        if !self.has_map() {
            return 1;
        }
        1 + VarInt(self.purposes.len() as u64).len()
            + self
                .purposes
                .iter()
                .map(|(_, entries)| {
                    1 + VarInt(entries.len() as u64).len()
                        + entries.iter().map(NetInfoEntry::size).sum::<usize>()
                })
                .sum::<usize>()
    }

    /// Encodes the purpose lists as decoded (see [`ExtNetInfo`]).
    pub(crate) fn consensus_encode_ext<W: Write + ?Sized>(
        &self,
        writer: &mut W,
    ) -> Result<usize, io::Error> {
        let mut len = self.version.consensus_encode(writer)?;
        // Unknown versions carry no further payload, mirroring Core's short-circuit.
        if !self.has_map() {
            return Ok(len);
        }
        len += VarInt(self.purposes.len() as u64).consensus_encode(writer)?;
        for (purpose, entries) in &self.purposes {
            len += purpose.to_u8().consensus_encode(writer)?;
            len += VarInt(entries.len() as u64).consensus_encode(writer)?;
            for entry in entries {
                len += entry.consensus_encode_ext(writer)?;
            }
        }
        Ok(len)
    }

    /// Decodes as Core's `ExtNetInfo::Unserialize` does.
    pub(crate) fn consensus_decode_ext<R: Read + ?Sized>(
        reader: &mut CoreStream<'_, R>,
    ) -> Result<Self, Error> {
        let version: u8 = Decodable::consensus_decode(reader)?;
        let mut info = ExtNetInfo {
            version,
            purposes: Vec::new(),
        };
        if !info.has_map() {
            return Ok(info);
        }
        let n_purposes = read_size(reader)?;
        // Every list takes at least two bytes, so don't trust the count for the allocation.
        info.purposes.reserve(cmp::min(n_purposes, 16) as usize);
        let mut last = None;
        for _ in 0..n_purposes {
            // Core (rc.4): evo/netinfo.h:419-437
            let code: u8 = Decodable::consensus_decode(reader)?;
            let purpose = NetInfoPurpose::from_u8(code);
            if matches!(purpose, NetInfoPurpose::Unknown(_))
                || last.is_some_and(|last| code <= last)
            {
                return Err(Error::ParseFailed("ExtNetInfo: purpose unknown or out of order"));
            }
            last = Some(code);
            let n_entries = read_size(reader)?;
            let mut entries = Vec::with_capacity(cmp::min(n_entries, 16) as usize);
            for _ in 0..n_entries {
                entries.push(NetInfoEntry::consensus_decode_ext(reader)?);
            }
            info.purposes.push((purpose, entries));
        }
        Ok(info)
    }

    /// The port of the first Platform HTTPS entry Core holds, unless that entry is invalid (Core
    /// then reports port 0).
    pub fn platform_https_port(&self) -> Option<u16> {
        match self.entries_for(NetInfoPurpose::PlatformHttps).first()? {
            entry @ (NetInfoEntry::Service {
                port,
                ..
            }
            | NetInfoEntry::Domain {
                port,
                ..
            }) if entry.is_valid() => Some(*port),
            _ => None,
        }
    }

    /// The entries Core holds for `purpose`. Entries that aren't
    /// [valid](NetInfoEntry::is_valid) are invalid for Core.
    pub fn entries_for(&self, purpose: NetInfoPurpose) -> &[NetInfoEntry] {
        if !self.has_map() {
            return &[];
        }
        self.purposes
            .iter()
            .find(|(p, _)| p.to_u8() == purpose.to_u8())
            .map(|(_, entries)| entries.as_slice())
            .unwrap_or(&[])
    }
}

/// A compact size used as a count, which Core's `ReadCompactSize` caps at `MAX_SIZE`.
fn read_size<R: Read + ?Sized>(reader: &mut R) -> Result<u64, Error> {
    let size = VarInt::consensus_decode(reader)?.0;
    if size > MAX_COMPACT_SIZE {
        return Err(Error::ParseFailed("compact size larger than MAX_SIZE"));
    }
    Ok(size)
}

/// A reader that, like Core's `DataStream`, consumes nothing when a read runs past the end of
/// the data.
///
/// Core turns a failure to read the service address of a net info entry into an invalid entry
/// and goes on decoding after the bytes it did consume, so bytes a failed read took from the
/// inner reader must be read again. Decoders of structures containing an [`ExtNetInfo`] read
/// through one of these, with [`CoreStream::decode`].
pub(crate) struct CoreStream<'a, R: Read + ?Sized> {
    inner: &'a mut R,
    /// Bytes taken from `inner` by a failed read, to be read again first.
    unread: VecDeque<u8>,
}

impl<'a, R: Read + ?Sized> CoreStream<'a, R> {
    /// Decodes with `decode` through a stream over `inner`, failing if bytes taken by a failed
    /// read are left unread at the end, which only happens once `inner` has run dry. For a ProTx
    /// payload, decoded from its own bytes, that is Core's `GetTxPayload` failing on data left in
    /// the stream. For an SML entry inside a message it is stricter than Core, which would read
    /// the bytes into the fields after the entry; Core never sends such an entry.
    pub(crate) fn decode<T>(
        inner: &'a mut R,
        decode: impl FnOnce(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut stream = CoreStream::new(inner);
        let value = decode(&mut stream)?;
        stream.finish()?;
        Ok(value)
    }

    fn new(inner: &'a mut R) -> Self {
        CoreStream {
            inner,
            unread: VecDeque::new(),
        }
    }

    /// Fills `buf`, or, if the data ends first, consumes nothing and returns `false`.
    fn read_all_or_nothing(&mut self, buf: &mut [u8]) -> Result<bool, io::Error> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read(&mut buf[filled..]) {
                Ok(0) => {
                    // Only the inner reader can run dry, so `unread` is used up by now.
                    self.unread.extend(&buf[..filled]);
                    return Ok(false);
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    fn finish(self) -> Result<(), Error> {
        if !self.unread.is_empty() {
            return Err(Error::ParseFailed("data left after the net info entries"));
        }
        Ok(())
    }
}

impl<R: Read + ?Sized> Read for CoreStream<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.unread.is_empty() {
            return self.unread.read(buf);
        }
        self.inner.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_entry(bytes: &[u8]) -> (NetInfoEntry, usize) {
        let mut reader = bytes;
        let mut stream = CoreStream::new(&mut reader);
        let entry = NetInfoEntry::consensus_decode_ext(&mut stream).unwrap();
        let left = stream.unread.len() + reader.len();
        (entry, bytes.len() - left)
    }

    fn roundtrip(entry: &NetInfoEntry) -> Vec<u8> {
        let mut buf = Vec::new();
        entry.consensus_encode_ext(&mut buf).unwrap();
        let (decoded, consumed) = decode_entry(&buf);
        assert_eq!(*entry, decoded);
        assert_eq!(consumed, buf.len());
        assert_eq!(entry.size(), buf.len());
        buf
    }

    fn decode_ext(bytes: &[u8]) -> Result<ExtNetInfo, Error> {
        let mut reader = bytes;
        let mut stream = CoreStream::new(&mut reader);
        let info = ExtNetInfo::consensus_decode_ext(&mut stream)?;
        stream.finish()?;
        Ok(info)
    }

    fn roundtrip_ext(info: &ExtNetInfo) -> Vec<u8> {
        let mut buf = Vec::new();
        info.consensus_encode_ext(&mut buf).unwrap();
        assert_eq!(*info, decode_ext(&buf).unwrap());
        assert_eq!(info.size(), buf.len());
        buf
    }

    fn ipv4(addr: [u8; 4], port: u16) -> NetInfoEntry {
        NetInfoEntry::Service {
            network: Bip155Network::Ipv4,
            addr: addr.to_vec(),
            port,
        }
    }

    #[test]
    fn service_ipv4() {
        let bytes = roundtrip(&ipv4([1, 2, 3, 4], 9999));
        assert_eq!(bytes, vec![0x01, 0x01, 0x04, 1, 2, 3, 4, 0x27, 0x0f]);
    }

    #[test]
    fn service_ipv6() {
        let entry = NetInfoEntry::Service {
            network: Bip155Network::Ipv6,
            addr: (1..17).collect(),
            port: 1,
        };
        roundtrip(&entry);
    }

    #[test]
    fn domain_entry() {
        let entry = NetInfoEntry::Domain {
            host: "example.org".to_string(),
            port: 443,
        };
        let bytes = roundtrip(&entry);
        assert_eq!(bytes[0], 0x02);
        assert_eq!(&bytes[bytes.len() - 2..], &443u16.to_be_bytes());
    }

    #[test]
    fn invalid_entry() {
        let bytes = roundtrip(&NetInfoEntry::Invalid);
        assert_eq!(bytes, vec![0xff]);
    }

    #[test]
    fn unknown_type_is_one_byte() {
        for byte in [0x00, 0x03, 0x7f, 0xfe] {
            let (entry, consumed) = decode_entry(&[byte, 0x01, 0x02]);
            assert_eq!(entry, NetInfoEntry::UnknownType(byte));
            assert_eq!(consumed, 1);
        }
    }

    #[test]
    fn addresses_core_rejects_are_malformed() {
        let service = |network: u8, addr: &[u8]| {
            [&[0x01, network, addr.len() as u8][..], addr, &[0x27, 0x0f]].concat()
        };
        let ipv6 = |prefix: &[u8]| {
            let mut addr = [1u8; 16];
            addr[..prefix.len()].copy_from_slice(prefix);
            service(2, &addr)
        };
        let mut cjdns = [1u8; 16];
        cjdns[0] = 0xfd;
        for bytes in [
            service(1, &[0, 0, 0, 0]),
            service(1, &[0xff, 0xff, 0xff, 0xff]),
            service(2, &[0; 16]),
            ipv6(&RFC3849_PREFIX),
            ipv6(&IPV4_IN_IPV6_PREFIX),
            ipv6(&TORV2_IN_IPV6_PREFIX),
            ipv6(&INTERNAL_IN_IPV6_PREFIX),
            service(6, &cjdns),
            // Tor v2 (network id 3) and unknown networks are read as invalid.
            service(3, &[1; 10]),
            service(9, &[1; 20]),
        ] {
            let (entry, consumed) = decode_entry(&bytes);
            assert_eq!(entry, NetInfoEntry::Malformed(bytes.clone()), "{bytes:02x?}");
            assert_eq!(consumed, bytes.len());
            assert_eq!(entry.canonical(), NetInfoEntry::Invalid);
        }
    }

    #[test]
    fn valid_cjdns_and_overlay_addresses() {
        for (network, len) in [(6u8, 16usize), (4, 32), (5, 32)] {
            let mut bytes = vec![0x01, network, len as u8, 0xfc];
            bytes.extend(std::iter::repeat_n(7, len - 1));
            bytes.extend([0x27, 0x0f]);
            let (entry, consumed) = decode_entry(&bytes);
            assert!(matches!(entry, NetInfoEntry::Service { .. }), "{entry:?}");
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn failed_service_reads_stop_where_core_does() {
        // Known network, wrong length: Core throws right after the length.
        let (entry, consumed) = decode_entry(&[0x01, 0x01, 0x05, 1, 2, 3, 4, 5, 0, 1]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x01, 0x05]));
        assert_eq!(consumed, 3);
        // Address longer than 512 bytes.
        let (entry, consumed) = decode_entry(&[0x01, 0x07, 0xfd, 0x01, 0x02, 0xaa]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x07, 0xfd, 0x01, 0x02]));
        assert_eq!(consumed, 5);
        // Non-canonical length: consumed, then thrown.
        let (entry, consumed) = decode_entry(&[0x01, 0x01, 0xfd, 0x04, 0x00, 1, 2, 3, 4]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x01, 0xfd, 0x04, 0x00]));
        assert_eq!(consumed, 5);
        // An unknown network's address running past the end consumes nothing of it.
        let (entry, consumed) = decode_entry(&[0x01, 0x07, 0x0a, 1, 2, 3]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x07, 0x0a]));
        assert_eq!(consumed, 3);
        // A port running past the end.
        let (entry, consumed) = decode_entry(&[0x01, 0x01, 0x04, 1, 2, 3, 4, 0x27]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x01, 0x04, 1, 2, 3, 4]));
        assert_eq!(consumed, 7);
        // A length marker whose value runs past the end.
        let (entry, consumed) = decode_entry(&[0x01, 0x01, 0xfe, 0x00]);
        assert_eq!(entry, NetInfoEntry::Malformed(vec![0x01, 0x01, 0xfe]));
        assert_eq!(consumed, 3);
    }

    #[test]
    fn bytes_left_by_a_failed_read_are_read_again() {
        // CORE_P2P list with one entry claiming a 10-byte address of an unknown network, but only
        // 4 bytes follow: Core leaves them in the stream for whatever comes next.
        let bytes = [0x01, 0x01, 0x00, 0x01, 0x01, 0x07, 0x0a, 0xde, 0xad, 0xbe, 0xef];
        let mut reader = &bytes[..];
        let mut stream = CoreStream::new(&mut reader);
        let info = ExtNetInfo::consensus_decode_ext(&mut stream).unwrap();
        assert_eq!(
            info.purposes,
            vec![(NetInfoPurpose::CoreP2P, vec![NetInfoEntry::Malformed(vec![0x01, 0x07, 0x0a])])]
        );
        let mut rest = [0u8; 4];
        stream.read_exact(&mut rest).unwrap();
        assert_eq!(rest, [0xde, 0xad, 0xbe, 0xef]);
        stream.finish().unwrap();

        // Unread bytes left at the end are trailing data.
        assert!(decode_ext(&bytes).is_err());
    }

    #[test]
    fn domains_core_rejects_are_malformed() {
        for (host, port) in [
            ("Example.org", 443u16),
            ("exa_mple.org", 443),
            ("example", 443),
            ("ex", 443),
            (".example.org", 443),
            ("example.org.", 443),
            ("a..org", 443),
            ("-a.org", 443),
            ("a-.org", 443),
            ("example.org", 0),
        ] {
            let mut bytes = vec![0x02];
            host.to_string().consensus_encode(&mut bytes).unwrap();
            bytes.extend(port.to_be_bytes());
            let (entry, consumed) = decode_entry(&bytes);
            assert_eq!(entry, NetInfoEntry::Malformed(bytes.clone()), "{host}:{port}");
            assert_eq!(consumed, bytes.len());
        }
        let label = "a".repeat(63);
        let (entry, _) = decode_entry(
            &[[0x02, 66].as_slice(), format!("{label}.ab").as_bytes(), &[1, 0xbb]].concat(),
        );
        assert!(entry.is_valid());
        let (entry, _) = decode_entry(
            &[[0x02, 67].as_slice(), format!("{label}a.ab").as_bytes(), &[1, 0xbb]].concat(),
        );
        assert!(!entry.is_valid());
        // Not UTF-8.
        let (entry, _) = decode_entry(&[0x02, 0x05, b'a', 0xff, b'.', b'o', b'g', 1, 0xbb]);
        assert!(matches!(entry, NetInfoEntry::Malformed(_)));
    }

    #[test]
    fn domain_failures_fail_the_payload() {
        // 254 bytes is over Core's limit, even before the data runs out.
        let mut bytes = vec![0x02, 0xfd, 0xfe, 0x00];
        bytes.extend(std::iter::repeat_n(b'a', 254));
        bytes.extend([1, 0xbb]);
        let mut reader = &bytes[..];
        assert!(NetInfoEntry::consensus_decode_ext(&mut CoreStream::new(&mut reader)).is_err());
        // Truncated name or port.
        for bytes in [&[0x02, 0x05, b'a', b'.', b'o'][..], &[0x02, 0x04, b'a', b'.', b'o', b'g', 1]]
        {
            let mut reader = bytes;
            assert!(NetInfoEntry::consensus_decode_ext(&mut CoreStream::new(&mut reader)).is_err());
        }
    }

    #[test]
    fn multi_purpose_map() {
        let info = ExtNetInfo {
            version: 1,
            purposes: vec![
                (NetInfoPurpose::CoreP2P, vec![ipv4([10, 0, 0, 1], 9999)]),
                (NetInfoPurpose::PlatformP2P, vec![ipv4([10, 0, 0, 1], 26656)]),
                (NetInfoPurpose::PlatformHttps, vec![ipv4([10, 0, 0, 1], 443)]),
            ],
        };
        roundtrip_ext(&info);
        assert_eq!(info.canonical(), info);
        assert_eq!(info.platform_https_port(), Some(443));
    }

    #[test]
    fn multiple_entries_one_purpose() {
        let info = ExtNetInfo {
            version: 1,
            purposes: vec![(
                NetInfoPurpose::CoreP2P,
                vec![
                    ipv4([1, 1, 1, 1], 1),
                    NetInfoEntry::Service {
                        network: Bip155Network::Ipv6,
                        addr: (1..17).collect(),
                        port: 2,
                    },
                    NetInfoEntry::Domain {
                        host: "node.example".to_string(),
                        port: 3,
                    },
                    NetInfoEntry::Invalid,
                    NetInfoEntry::UnknownType(0x00),
                ],
            )],
        };
        roundtrip_ext(&info);
    }

    #[test]
    fn canonical_reads_malformed_entries_as_invalid() {
        let core_list = vec![ipv4([1, 2, 3, 4], 9999)];
        let info = ExtNetInfo {
            version: 1,
            purposes: vec![
                (NetInfoPurpose::CoreP2P, core_list.clone()),
                (
                    NetInfoPurpose::PlatformHttps,
                    vec![ipv4([1, 2, 3, 4], 443), NetInfoEntry::UnknownType(0x00)],
                ),
            ],
        };
        let bytes = roundtrip_ext(&info);
        assert_eq!(decode_ext(&bytes).unwrap(), info);
        assert_eq!(
            info.canonical(),
            ExtNetInfo {
                version: 1,
                purposes: vec![
                    (NetInfoPurpose::CoreP2P, core_list.clone()),
                    (
                        NetInfoPurpose::PlatformHttps,
                        vec![ipv4([1, 2, 3, 4], 443), NetInfoEntry::Invalid]
                    ),
                ],
            }
        );
        assert_eq!(info.entries_for(NetInfoPurpose::CoreP2P), core_list.as_slice());
        assert_eq!(info.platform_https_port(), Some(443));
    }

    /// CE4-01: rc.4 reads known purposes in strictly ascending order only; a repeated,
    /// descending or unknown one fails the read where it stands (`evo/netinfo.h` rc.4:419-437;
    /// `extnetinfo_validate_deser`, `evo_netinfo_tests.cpp` rc.4:725-756).
    #[test]
    fn purposes_read_strictly() {
        let list = |purpose: NetInfoPurpose| (purpose, vec![ipv4([1, 2, 3, 4], 9999)]);
        let ascending = ExtNetInfo {
            version: 1,
            purposes: vec![
                list(NetInfoPurpose::CoreP2P),
                list(NetInfoPurpose::PlatformP2P),
                list(NetInfoPurpose::PlatformHttps),
            ],
        };
        roundtrip_ext(&ascending);
        for purposes in [
            vec![NetInfoPurpose::CoreP2P, NetInfoPurpose::CoreP2P],
            vec![NetInfoPurpose::PlatformHttps, NetInfoPurpose::CoreP2P],
            vec![NetInfoPurpose::Unknown(3)],
            vec![NetInfoPurpose::CoreP2P, NetInfoPurpose::Unknown(7)],
        ] {
            let info = ExtNetInfo {
                version: 1,
                purposes: purposes.iter().copied().map(list).collect(),
            };
            let mut bytes = Vec::new();
            info.consensus_encode_ext(&mut bytes).unwrap();
            let err = decode_ext(&bytes).unwrap_err();
            assert!(
                matches!(err, Error::ParseFailed("ExtNetInfo: purpose unknown or out of order")),
                "{purposes:?}: {err:?}"
            );
        }
    }

    #[test]
    fn counts_over_max_size_fail() {
        assert!(decode_ext(&[0x01, 0xfe, 0x01, 0x00, 0x00, 0x02]).is_err());
        assert!(decode_ext(&[0x01, 0x01, 0x00, 0xfe, 0x01, 0x00, 0x00, 0x02]).is_err());
    }

    #[test]
    fn empty_map() {
        let info = ExtNetInfo {
            version: 1,
            purposes: Vec::new(),
        };
        let bytes = roundtrip_ext(&info);
        assert_eq!(bytes, vec![0x01, 0x00]);
    }

    #[test]
    fn unknown_version_short_circuit() {
        for version in [0u8, 2, 255] {
            let info = ExtNetInfo {
                version,
                purposes: Vec::new(),
            };
            let bytes = roundtrip_ext(&info);
            assert_eq!(bytes, vec![version]);
        }
    }

    #[test]
    fn legacy_addresses() {
        let mapped = |ip: [u8; 4]| {
            let mut addr = [0u8; 16];
            addr[..12].copy_from_slice(&IPV4_IN_IPV6_PREFIX);
            addr[12..].copy_from_slice(&ip);
            addr
        };
        assert!(legacy_address_is_valid(&mapped([1, 2, 3, 4])));
        assert!(!legacy_address_is_valid(&mapped([0, 0, 0, 0])));
        assert!(!legacy_address_is_valid(&mapped([255, 255, 255, 255])));
        assert!(!legacy_address_is_valid(&[0; 16]));
        let mut addr = [1u8; 16];
        assert!(legacy_address_is_valid(&addr));
        addr[..4].copy_from_slice(&RFC3849_PREFIX);
        assert!(!legacy_address_is_valid(&addr));
        addr[..6].copy_from_slice(&TORV2_IN_IPV6_PREFIX);
        assert!(!legacy_address_is_valid(&addr));
        addr[..6].copy_from_slice(&INTERNAL_IN_IPV6_PREFIX);
        assert!(!legacy_address_is_valid(&addr));
    }
}
