//! Core's network addresses: `CNetAddr`, `CService` and `CSubNet`, with Core's classification
//! predicates (P2a §3.7.3; v24 `netaddress.{h,cpp}`).
//!
//! base-sdk's `AddrV2` and `ServiceV1` are the wire forms; these types add what Core derives from
//! them (the network, the routability predicates, the group and bucket keys) and the
//! `NET_INTERNAL` network Core uses for seed names. `-allowprivatenet` is a parameter, never a
//! global.

use core::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};

use dash_primitives::{AddrV2, ServiceV1};

/// Core's `enum Network` (`netaddress.h`), in Core's order: the index is the one Core uses for
/// its per-network counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Network {
    /// `NET_UNROUTABLE`: addresses that are not public (local, private, documentation, …).
    Unroutable = 0,
    /// `NET_IPV4`.
    Ipv4,
    /// `NET_IPV6`.
    Ipv6,
    /// `NET_ONION` (Tor v3).
    Onion,
    /// `NET_I2P`.
    I2p,
    /// `NET_CJDNS`.
    Cjdns,
    /// `NET_INTERNAL`: a name hashed into `fd6b:88c0:8724::/48` (DNS seed and fixed-seed sources).
    Internal,
}

impl Network {
    /// The number of networks (`NET_MAX`).
    pub const COUNT: usize = 7;

    /// Every network, by index.
    pub const ALL: [Self; Self::COUNT] = [
        Self::Unroutable,
        Self::Ipv4,
        Self::Ipv6,
        Self::Onion,
        Self::I2p,
        Self::Cjdns,
        Self::Internal,
    ];

    /// Core's `GetNetworkName`.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unroutable => "not_publicly_routable",
            Self::Ipv4 => "ipv4",
            Self::Ipv6 => "ipv6",
            Self::Onion => "onion",
            Self::I2p => "i2p",
            Self::Cjdns => "cjdns",
            Self::Internal => "internal",
        }
    }
}

const ADDR_IPV4_SIZE: usize = 4;
const ADDR_IPV6_SIZE: usize = 16;
const ADDR_TORV3_SIZE: usize = 32;
const ADDR_I2P_SIZE: usize = 32;
const ADDR_CJDNS_SIZE: usize = 16;
const ADDR_INTERNAL_SIZE: usize = 10;

const IPV4_IN_IPV6_PREFIX: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF];
const TORV2_IN_IPV6_PREFIX: [u8; 6] = [0xFD, 0x87, 0xD8, 0x7E, 0xEB, 0x43];
const INTERNAL_IN_IPV6_PREFIX: [u8; 6] = [0xFD, 0x6B, 0x88, 0xC0, 0x87, 0x24];
const CJDNS_PREFIX: u8 = 0xFC;

/// Core's `CNetAddr`: a network and its address bytes (4 for IPv4, 16 for IPv6 and CJDNS, 32 for
/// Tor v3 and I2P, 10 for internal names). The default is Core's default-constructed address,
/// the invalid `::`.
///
/// Like Core's, it carries the IPv6 scope id (`m_scope_id`) a numeric lookup or an accepted
/// socket gave it: set on the socket address for bind and dial and printed as `%N`, but never
/// on the wire and ignored by equality, order, hashing and subnet match (P4a D23).
#[derive(Clone)]
pub struct NetAddr {
    net: Network,
    bytes: Vec<u8>,
    // Core: netaddress.h:138 m_scope_id
    scope_id: u32,
}

impl PartialEq for NetAddr {
    /// Core's `operator==`: network and bytes, not the scope id.
    // Core: netaddress.cpp:610-613
    fn eq(&self, other: &Self) -> bool {
        self.net == other.net && self.bytes == other.bytes
    }
}

impl Eq for NetAddr {}

impl core::hash::Hash for NetAddr {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.net.hash(state);
        self.bytes.hash(state);
    }
}

impl Default for NetAddr {
    fn default() -> Self {
        Self {
            net: Network::Ipv6,
            bytes: vec![0; ADDR_IPV6_SIZE],
            scope_id: 0,
        }
    }
}

impl PartialOrd for NetAddr {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NetAddr {
    /// Core's `operator<`: by network, then by `prevector` order (size first, then bytes).
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.net
            .cmp(&other.net)
            .then(self.bytes.len().cmp(&other.bytes.len()))
            .then_with(|| self.bytes.cmp(&other.bytes))
    }
}

/// Core's `CService`: an address and a port.
#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Service {
    /// The address.
    pub addr: NetAddr,
    /// The port.
    pub port: u16,
}

/// Core's `CSubNet`: a network and a mask, or a single address (the unit of a ban).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SubNet {
    network: NetAddr,
    netmask: [u8; 16],
    valid: bool,
}

fn has_prefix(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && bytes[..prefix.len()] == *prefix
}

impl NetAddr {
    /// An address on `net` with its raw bytes. Returns `None` when the length does not fit `net`
    /// or `net` is [`Network::Unroutable`], which is never stored (Core's `SetIP` asserts).
    pub fn new(net: Network, bytes: Vec<u8>) -> Option<Self> {
        let size = match net {
            Network::Ipv4 => ADDR_IPV4_SIZE,
            Network::Ipv6 => ADDR_IPV6_SIZE,
            Network::Onion => ADDR_TORV3_SIZE,
            Network::I2p => ADDR_I2P_SIZE,
            Network::Cjdns => ADDR_CJDNS_SIZE,
            Network::Internal => ADDR_INTERNAL_SIZE,
            Network::Unroutable => return None,
        };
        (bytes.len() == size).then_some(Self {
            net,
            bytes,
            scope_id: 0,
        })
    }

    /// An IPv4 address.
    pub fn ipv4(octets: [u8; 4]) -> Self {
        Self {
            net: Network::Ipv4,
            bytes: octets.to_vec(),
            scope_id: 0,
        }
    }

    /// Core's `SetLegacyIPv6`: the v1 wire form. `::ffff:a.b.c.d` is IPv4, the internal prefix is
    /// internal, the Tor v2 prefix is the invalid `::`, anything else is IPv6.
    // Core: netaddress.cpp:135-168 CNetAddr::SetLegacyIPv6
    pub fn from_legacy_ipv6(ipv6: [u8; 16]) -> Self {
        if has_prefix(&ipv6, &IPV4_IN_IPV6_PREFIX) {
            Self {
                net: Network::Ipv4,
                bytes: ipv6[IPV4_IN_IPV6_PREFIX.len()..].to_vec(),
                scope_id: 0,
            }
        } else if has_prefix(&ipv6, &TORV2_IN_IPV6_PREFIX) {
            Self::default()
        } else if has_prefix(&ipv6, &INTERNAL_IN_IPV6_PREFIX) {
            Self {
                net: Network::Internal,
                bytes: ipv6[INTERNAL_IN_IPV6_PREFIX.len()..].to_vec(),
                scope_id: 0,
            }
        } else {
            Self {
                net: Network::Ipv6,
                bytes: ipv6.to_vec(),
                scope_id: 0,
            }
        }
    }

    /// Core's `UnserializeV2Stream` after the read: IPv6 with the internal prefix is internal,
    /// IPv4- or Tor v2-in-IPv6 and unknown network ids are the invalid `::`.
    ///
    /// base-sdk decodes a BIP155 IPv6 address in `fc00::/8` as CJDNS where Core keeps it IPv6
    /// (DDE-SWEEP §5, owned by BS10); this conversion inherits that (SB6 D11). NetInfo entries
    /// are not affected: base-sdk's NetInfo reader keeps `fc00::/8` IPv6, as Core does.
    // Core: netaddress.h:440-500 CNetAddr::UnserializeV2Stream
    pub fn from_addrv2(addr: &AddrV2) -> Self {
        match addr {
            AddrV2::Ipv4(b) => Self::ipv4(*b),
            AddrV2::Ipv6(b) => {
                if has_prefix(b, &INTERNAL_IN_IPV6_PREFIX) {
                    Self {
                        net: Network::Internal,
                        bytes: b[INTERNAL_IN_IPV6_PREFIX.len()..].to_vec(),
                        scope_id: 0,
                    }
                } else if has_prefix(b, &IPV4_IN_IPV6_PREFIX)
                    || has_prefix(b, &TORV2_IN_IPV6_PREFIX)
                {
                    Self::default()
                } else {
                    Self {
                        net: Network::Ipv6,
                        bytes: b.to_vec(),
                        scope_id: 0,
                    }
                }
            }
            AddrV2::TorV3(b) => Self {
                net: Network::Onion,
                bytes: b.to_vec(),
                scope_id: 0,
            },
            AddrV2::I2p(b) => Self {
                net: Network::I2p,
                bytes: b.to_vec(),
                scope_id: 0,
            },
            AddrV2::Cjdns(b) => Self {
                net: Network::Cjdns,
                bytes: b.to_vec(),
                scope_id: 0,
            },
            AddrV2::Unknown {
                ..
            } => Self::default(),
        }
    }

    /// Core's `SerializeV2Stream`: internal addresses go out as IPv6 with the internal prefix.
    pub fn to_addrv2(&self) -> AddrV2 {
        match self.net {
            Network::Ipv4 => AddrV2::Ipv4(self.bytes[..].try_into().expect("4 bytes")),
            Network::Ipv6 => AddrV2::Ipv6(self.bytes[..].try_into().expect("16 bytes")),
            Network::Internal => AddrV2::Ipv6(self.v1_bytes()),
            Network::Onion => AddrV2::TorV3(self.bytes[..].try_into().expect("32 bytes")),
            Network::I2p => AddrV2::I2p(self.bytes[..].try_into().expect("32 bytes")),
            Network::Cjdns => AddrV2::Cjdns(self.bytes[..].try_into().expect("16 bytes")),
            Network::Unroutable => unreachable!("never stored"),
        }
    }

    /// Core's `SerializeV1Array`: Tor, I2P and CJDNS serialize as zeros.
    // Core: netaddress.h:310-341 CNetAddr::SerializeV1Array
    pub fn v1_bytes(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        match self.net {
            Network::Ipv6 => out.copy_from_slice(&self.bytes),
            Network::Ipv4 => {
                out[..12].copy_from_slice(&IPV4_IN_IPV6_PREFIX);
                out[12..].copy_from_slice(&self.bytes);
            }
            Network::Internal => {
                out[..6].copy_from_slice(&INTERNAL_IN_IPV6_PREFIX);
                out[6..].copy_from_slice(&self.bytes);
            }
            Network::Onion | Network::I2p | Network::Cjdns | Network::Unroutable => {}
        }
        out
    }

    /// Core's `CNetAddr::SetInternal(name)`: the first 10 bytes of SHA-256(`name`). Core refuses
    /// an empty name; this returns `None` for it.
    // Core: netaddress.cpp:170-180 CNetAddr::SetInternal
    pub fn internal(name: &str) -> Option<Self> {
        if name.is_empty() {
            return None;
        }
        let hash = dash_sha256::sha256(name.as_bytes());
        Some(Self {
            net: Network::Internal,
            bytes: hash[..ADDR_INTERNAL_SIZE].to_vec(),
            scope_id: 0,
        })
    }

    /// The address's network as stored (`m_net`). [`NetAddr::network`] is Core's `GetNetwork`.
    pub fn net(&self) -> Network {
        self.net
    }

    /// The raw address bytes (`m_addr`).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The address with the IPv6 scope id `scope_id` (Core's `CNetAddr(in6_addr, scope)`, which
    /// sets it whatever `SetLegacyIPv6` decoded; only IPv6 and CJDNS ever use it).
    // Core: netaddress.cpp:299-302
    pub fn with_scope_id(mut self, scope_id: u32) -> Self {
        self.scope_id = scope_id;
        self
    }

    /// The IPv6 scope id (`m_scope_id`), 0 for none.
    pub fn scope_id(&self) -> u32 {
        self.scope_id
    }

    /// `IsIPv4()`.
    pub fn is_ipv4(&self) -> bool {
        self.net == Network::Ipv4
    }

    /// `IsIPv6()`.
    pub fn is_ipv6(&self) -> bool {
        self.net == Network::Ipv6
    }

    /// Core's `MaybeFlipIPv6toCJDNS` on the address (the port plays no part): an IPv6 address in
    /// `fc00::/8` is CJDNS when CJDNS is reachable.
    // Core: netbase.cpp:833-840 MaybeFlipIPv6toCJDNS
    pub fn maybe_flip_to_cjdns(self, cjdns_reachable: bool) -> NetAddr {
        if cjdns_reachable && self.is_ipv6() && self.bytes()[0] == 0xFC {
            // Core flips `m_net` on a copy, so the scope id stays.
            Self {
                net: Network::Cjdns,
                ..self
            }
        } else {
            self
        }
    }

    /// `IsTor()`.
    pub fn is_tor(&self) -> bool {
        self.net == Network::Onion
    }

    /// `IsI2P()`.
    pub fn is_i2p(&self) -> bool {
        self.net == Network::I2p
    }

    /// `IsCJDNS()`.
    pub fn is_cjdns(&self) -> bool {
        self.net == Network::Cjdns
    }

    /// `IsInternal()`.
    pub fn is_internal(&self) -> bool {
        self.net == Network::Internal
    }

    /// `IsPrivacyNet()`: Tor or I2P.
    pub fn is_privacy_net(&self) -> bool {
        self.is_tor() || self.is_i2p()
    }

    fn v4_prefix(&self, prefix: &[u8]) -> bool {
        self.is_ipv4() && has_prefix(&self.bytes, prefix)
    }

    fn v6_prefix(&self, prefix: &[u8]) -> bool {
        self.is_ipv6() && has_prefix(&self.bytes, prefix)
    }

    /// `IsBindAny()`.
    pub fn is_bind_any(&self) -> bool {
        (self.is_ipv4() || self.is_ipv6()) && self.bytes.iter().all(|&b| b == 0)
    }

    /// `IsRFC1918()`: 10/8, 192.168/16, 172.16/12.
    pub fn is_rfc1918(&self) -> bool {
        let a = &self.bytes;
        self.is_ipv4()
            && (a[0] == 10
                || (a[0] == 192 && a[1] == 168)
                || (a[0] == 172 && (16..=31).contains(&a[1])))
    }

    /// `IsRFC2544()`: 198.18/15.
    pub fn is_rfc2544(&self) -> bool {
        self.is_ipv4() && self.bytes[0] == 198 && (self.bytes[1] == 18 || self.bytes[1] == 19)
    }

    /// `IsRFC3927()`: 169.254/16.
    pub fn is_rfc3927(&self) -> bool {
        self.v4_prefix(&[169, 254])
    }

    /// `IsRFC6598()`: 100.64/10.
    pub fn is_rfc6598(&self) -> bool {
        self.is_ipv4() && self.bytes[0] == 100 && (64..=127).contains(&self.bytes[1])
    }

    /// `IsRFC5737()`: the IPv4 documentation ranges.
    pub fn is_rfc5737(&self) -> bool {
        self.v4_prefix(&[192, 0, 2])
            || self.v4_prefix(&[198, 51, 100])
            || self.v4_prefix(&[203, 0, 113])
    }

    /// `IsRFC3849()`: 2001:db8::/32.
    pub fn is_rfc3849(&self) -> bool {
        self.v6_prefix(&[0x20, 0x01, 0x0D, 0xB8])
    }

    /// `IsRFC3964()`: 6to4, 2002::/16.
    pub fn is_rfc3964(&self) -> bool {
        self.v6_prefix(&[0x20, 0x02])
    }

    /// `IsRFC6052()`: 64:ff9b::/96.
    pub fn is_rfc6052(&self) -> bool {
        self.v6_prefix(&[0x00, 0x64, 0xFF, 0x9B, 0, 0, 0, 0, 0, 0, 0, 0])
    }

    /// `IsRFC4380()`: Teredo, 2001::/32.
    pub fn is_rfc4380(&self) -> bool {
        self.v6_prefix(&[0x20, 0x01, 0x00, 0x00])
    }

    /// `IsRFC4862()`: fe80::/64.
    pub fn is_rfc4862(&self) -> bool {
        self.v6_prefix(&[0xFE, 0x80, 0, 0, 0, 0, 0, 0])
    }

    /// `IsRFC4193()`: fc00::/7.
    pub fn is_rfc4193(&self) -> bool {
        self.is_ipv6() && (self.bytes[0] & 0xFE) == 0xFC
    }

    /// `IsRFC6145()`: ::ffff:0:0:0/96.
    pub fn is_rfc6145(&self) -> bool {
        self.v6_prefix(&[0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 0, 0])
    }

    /// `IsRFC4843()`: ORCHID, 2001:10::/28.
    pub fn is_rfc4843(&self) -> bool {
        self.v6_prefix(&[0x20, 0x01, 0x00]) && (self.bytes[3] & 0xF0) == 0x10
    }

    /// `IsRFC7343()`: ORCHIDv2, 2001:20::/28.
    pub fn is_rfc7343(&self) -> bool {
        self.v6_prefix(&[0x20, 0x01, 0x00]) && (self.bytes[3] & 0xF0) == 0x20
    }

    /// `IsHeNet()`: 2001:470::/32.
    pub fn is_he_net(&self) -> bool {
        self.v6_prefix(&[0x20, 0x01, 0x04, 0x70])
    }

    /// Core's `IsLocal()`: 127/8, 0/8 and ::1.
    // Core: netaddress.cpp:400-414 CNetAddr::IsLocal
    pub fn is_local(&self) -> bool {
        if self.is_ipv4() && (self.bytes[0] == 127 || self.bytes[0] == 0) {
            return true;
        }
        let mut loopback = [0u8; 16];
        loopback[15] = 1;
        self.is_ipv6() && self.bytes[..] == loopback
    }

    /// Core's `IsValid()`.
    // Core: netaddress.cpp:426-452 CNetAddr::IsValid
    pub fn is_valid(&self) -> bool {
        if self.is_ipv6() && self.bytes.iter().all(|&b| b == 0) {
            return false;
        }
        if self.is_cjdns() && self.bytes[0] != CJDNS_PREFIX {
            return false;
        }
        if self.is_rfc3849() || self.is_internal() {
            return false;
        }
        if self.is_ipv4() {
            let addr = u32::from_be_bytes(self.bytes[..4].try_into().expect("4 bytes"));
            if addr == 0 || addr == u32::MAX {
                return false;
            }
        }
        true
    }

    /// Core's `IsRoutable()`, with `-allowprivatenet` (Dash's `fAllowPrivateNet`).
    // Core: netaddress.cpp:464-471 CNetAddr::IsRoutable
    pub fn is_routable(&self, allow_private_net: bool) -> bool {
        if !self.is_valid() {
            return false;
        }
        if !allow_private_net && self.is_rfc1918() {
            return false;
        }
        !(self.is_rfc2544()
            || self.is_rfc3927()
            || self.is_rfc4862()
            || self.is_rfc6598()
            || self.is_rfc5737()
            || self.is_rfc4193()
            || self.is_rfc4843()
            || self.is_rfc7343()
            || self.is_local()
            || self.is_internal())
    }

    /// Core's `IsAddrV1Compatible()`.
    pub fn is_addr_v1_compatible(&self) -> bool {
        matches!(self.net, Network::Ipv4 | Network::Ipv6 | Network::Internal)
    }

    /// Core's `IsRelayable()`: IPv4, IPv6, Tor, I2P or CJDNS (not internal).
    pub fn is_relayable(&self) -> bool {
        matches!(
            self.net,
            Network::Ipv4 | Network::Ipv6 | Network::Onion | Network::I2p | Network::Cjdns
        )
    }

    /// Core's `GetNetwork()`: internal, else [`Network::Unroutable`] for a non-routable address,
    /// else `m_net`.
    // Core: netaddress.cpp:502-511 CNetAddr::GetNetwork
    pub fn network(&self, allow_private_net: bool) -> Network {
        if self.is_internal() {
            Network::Internal
        } else if !self.is_routable(allow_private_net) {
            Network::Unroutable
        } else {
            self.net
        }
    }

    /// Core's `HasLinkedIPv4()`.
    pub fn has_linked_ipv4(&self, allow_private_net: bool) -> bool {
        self.is_routable(allow_private_net)
            && (self.is_ipv4()
                || self.is_rfc6145()
                || self.is_rfc6052()
                || self.is_rfc3964()
                || self.is_rfc4380())
    }

    /// Core's `GetLinkedIPv4()`. Only meaningful when [`NetAddr::has_linked_ipv4`] holds.
    // Core: netaddress.cpp:664-679 CNetAddr::GetLinkedIPv4
    pub fn linked_ipv4(&self) -> u32 {
        let be = |b: &[u8]| u32::from_be_bytes(b.try_into().expect("4 bytes"));
        if self.is_ipv4() {
            be(&self.bytes)
        } else if self.is_rfc6052() || self.is_rfc6145() {
            be(&self.bytes[12..16])
        } else if self.is_rfc3964() {
            be(&self.bytes[2..6])
        } else if self.is_rfc4380() {
            !be(&self.bytes[12..16])
        } else {
            unreachable!("GetLinkedIPv4 without a linked IPv4 address")
        }
    }

    /// Core's `GetNetClass()`: like `GetNetwork`, but addresses with a linked IPv4 are IPv4.
    // Core: netaddress.cpp:681-697 CNetAddr::GetNetClass
    pub fn net_class(&self, allow_private_net: bool) -> Network {
        if self.is_internal() {
            Network::Internal
        } else if !self.is_routable(allow_private_net) {
            Network::Unroutable
        } else if self.has_linked_ipv4(allow_private_net) {
            Network::Ipv4
        } else {
            self.net
        }
    }

    /// Core's `GetAddrBytes()`: the v1 form for v1-compatible addresses, else the raw bytes.
    pub fn addr_bytes(&self) -> Vec<u8> {
        if self.is_addr_v1_compatible() {
            self.v1_bytes().to_vec()
        } else {
            self.bytes.clone()
        }
    }

    /// The standard-library form of an IP address (IPv4 and IPv6 only).
    pub fn to_ip(&self) -> Option<IpAddr> {
        match self.net {
            Network::Ipv4 => {
                Some(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(&self.bytes[..]).ok()?)))
            }
            Network::Ipv6 => {
                Some(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(&self.bytes[..]).ok()?)))
            }
            _ => None,
        }
    }

    /// The address of a standard-library IP address (Core's `CNetAddr(in6_addr)` maps
    /// `::ffff:a.b.c.d` to IPv4).
    pub fn from_ip(ip: IpAddr) -> Self {
        match ip {
            IpAddr::V4(v4) => Self::ipv4(v4.octets()),
            IpAddr::V6(v6) => Self::from_legacy_ipv6(v6.octets()),
        }
    }

    /// Core's `CNetAddr::SetSpecial`: a Tor v3 `.onion` name (version 3, checksum checked) or an
    /// I2P `<52 base32 chars>.b32.i2p` name as its address, or `None`. Names are case-sensitive
    /// except the I2P suffix, as Core's.
    // Core: netaddress.cpp:209-290 SetSpecial, SetTor, SetI2P
    pub fn set_special(name: &str) -> Option<Self> {
        if name.contains('\0') {
            return None;
        }
        Self::set_tor(name).or_else(|| Self::set_i2p(name))
    }

    // Core: netaddress.cpp:226-263 SetTor
    fn set_tor(name: &str) -> Option<Self> {
        const SUFFIX: &str = ".onion";
        const TOTAL_LEN: usize = ADDR_TORV3_SIZE + 2 + 1;
        if name.len() <= SUFFIX.len() {
            return None;
        }
        let input = decode_base32(name.strip_suffix(SUFFIX)?)?;
        if input.len() != TOTAL_LEN || input[TOTAL_LEN - 1] != 3 {
            return None;
        }
        let pubkey: [u8; ADDR_TORV3_SIZE] = input[..ADDR_TORV3_SIZE].try_into().ok()?;
        // base-sdk's `TorV3` display computes `torv3::Checksum` (SHA3-256 of ".onion checksum",
        // the key and the version); its first two bytes must match.
        let canonical = AddrV2::TorV3(pubkey).to_string();
        let expected = decode_base32(canonical.strip_suffix(SUFFIX)?)?;
        if input[ADDR_TORV3_SIZE..TOTAL_LEN - 1] != expected[ADDR_TORV3_SIZE..TOTAL_LEN - 1] {
            return None;
        }
        Self::new(Network::Onion, pubkey.to_vec())
    }

    // Core: netaddress.cpp:265-290 SetI2P
    fn set_i2p(name: &str) -> Option<Self> {
        const B32_LEN: usize = 52;
        const SUFFIX: &str = ".b32.i2p";
        if name.len() != B32_LEN + SUFFIX.len()
            || !name.is_char_boundary(B32_LEN)
            || !name[B32_LEN..].eq_ignore_ascii_case(SUFFIX)
        {
            return None;
        }
        let padded = format!("{}====", &name[..B32_LEN]);
        let bytes = decode_base32(&padded)?;
        Self::new(Network::I2p, bytes)
    }
}

impl fmt::Display for NetAddr {
    /// Core's `ToStringAddr()`: an IPv6 address with a scope id ends in `%N` (CJDNS never does).
    // Core: netaddress.cpp:518-571 IPv6ToString, 585-607 CNetAddr::ToStringAddr
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = &self.bytes;
        match self.net {
            Network::Ipv4 => write!(f, "{}.{}.{}.{}", b[0], b[1], b[2], b[3]),
            Network::Ipv6 => {
                write_ipv6(f, b)?;
                if self.scope_id != 0 {
                    write!(f, "%{}", self.scope_id)?;
                }
                Ok(())
            }
            Network::Cjdns => write_ipv6(f, b),
            Network::Onion => write!(f, "{}", self.to_addrv2()),
            Network::I2p => write!(f, "{}.b32.i2p", base32_lower(b, false)),
            Network::Internal => write!(f, "{}.internal", base32_lower(b, true)),
            Network::Unroutable => f.write_str("[unroutable]"),
        }
    }
}

/// Core's `IPv6ToString` without a scope id: lower-case hex groups with the first longest run of
/// two or more zero groups written `::` (RFC 5952), never the dotted IPv4 form.
// Core: netaddress.cpp:518-571 IPv6ToString
fn write_ipv6(f: &mut fmt::Formatter<'_>, a: &[u8]) -> fmt::Result {
    let group = |i: usize| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]);
    // (start, len) of the longest zero run, the first one on a tie.
    let (mut longest, mut current) = ((0, 0), (0, 0));
    for i in 0..8 {
        if group(i) != 0 {
            current = (i + 1, 0);
            continue;
        }
        current.1 += 1;
        if current.1 > longest.1 {
            longest = current;
        }
    }
    let mut last_colon = false;
    let mut empty = true;
    for i in 0..8 {
        if longest.1 >= 2 && (longest.0..longest.0 + longest.1).contains(&i) {
            if i == longest.0 {
                f.write_str("::")?;
                (last_colon, empty) = (true, false);
            }
            continue;
        }
        if !empty && !last_colon {
            f.write_str(":")?;
        }
        write!(f, "{:x}", group(i))?;
        (last_colon, empty) = (false, false);
    }
    Ok(())
}

impl fmt::Debug for NetAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Core's `EncodeBase32` (RFC 4648 alphabet, lower case), padded with `=` when `pad`.
fn base32_lower(data: &[u8], pad: bool) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &b in data {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 31) as usize] as char);
    }
    while pad && !out.len().is_multiple_of(8) {
        out.push('=');
    }
    out
}

/// Core's `DecodeBase32`: RFC 4648 letters in either case, a length that is a multiple of 8,
/// 1, 3, 4 or 6 `=` of padding, and zero trailing bits (`ConvertBits<5, 8, false>`).
// Core: util/strencodings.cpp:194-230 DecodeBase32; util/strencodings.h ConvertBits
fn decode_base32(s: &str) -> Option<Vec<u8>> {
    let mut s = s.as_bytes();
    if !s.len().is_multiple_of(8) {
        return None;
    }
    for strip in [1, 2, 1, 2] {
        if s.len() >= strip && s[s.len() - strip..].iter().all(|&c| c == b'=') {
            s = &s[..s.len() - strip];
        }
    }
    const MAX_ACC: u32 = (1 << (5 + 8 - 1)) - 1;
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in s {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = ((acc << 5) | u32::from(v)) & MAX_ACC;
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    if bits >= 5 || (acc << (8 - bits)) & 0xFF != 0 {
        return None;
    }
    Some(out)
}

impl Service {
    /// An address and port.
    pub fn new(addr: NetAddr, port: u16) -> Self {
        Self {
            addr,
            port,
        }
    }

    /// The v1 wire form (Core's `CService` in a v1 `CAddress` or `version`).
    pub fn from_v1(v1: &ServiceV1) -> Self {
        Self::new(NetAddr::from_legacy_ipv6(v1.addr.0), v1.port)
    }

    /// The standard-library form, for IPv4, IPv6 and CJDNS (plain IPv6 on the wire, Core's
    /// `GetSockAddr`), with the address's scope id on IPv6 and CJDNS; `None` for the other
    /// networks.
    // Core: netaddress.cpp:868-893 CService::GetSockAddr
    pub fn to_socket_addr(&self) -> Option<SocketAddr> {
        match self.addr.net {
            Network::Ipv4 => Some(SocketAddr::new(self.addr.to_ip()?, self.port)),
            Network::Ipv6 | Network::Cjdns => {
                let ip = Ipv6Addr::from(<[u8; 16]>::try_from(&self.addr.bytes[..]).ok()?);
                Some(SocketAddr::V6(SocketAddrV6::new(ip, self.port, 0, self.addr.scope_id)))
            }
            _ => None,
        }
    }

    /// From a standard-library socket address, keeping an IPv6 scope id (Core's
    /// `CService(sockaddr_in6)`).
    // Core: netaddress.cpp:808
    pub fn from_socket_addr(sa: SocketAddr) -> Self {
        let addr = match sa {
            SocketAddr::V4(v4) => NetAddr::from_ip(IpAddr::V4(*v4.ip())),
            SocketAddr::V6(v6) => {
                NetAddr::from_ip(IpAddr::V6(*v6.ip())).with_scope_id(v6.scope_id())
            }
        };
        Self::new(addr, sa.port())
    }

    /// Core's `CService::GetKey()`: the address bytes (`GetAddrBytes`), then the port big-endian.
    /// The AddrMan bucket key.
    // Core: netaddress.cpp:900-906 CService::GetKey
    pub fn key(&self) -> Vec<u8> {
        let mut key = self.addr.addr_bytes();
        key.extend_from_slice(&self.port.to_be_bytes());
        key
    }
}

impl fmt::Display for Service {
    /// Core's `ToStringAddrPort()`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.addr.net {
            Network::Ipv4 | Network::Onion | Network::I2p | Network::Internal => {
                write!(f, "{}:{}", self.addr, self.port)
            }
            _ => write!(f, "[{}]:{}", self.addr, self.port),
        }
    }
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl SubNet {
    /// A subnet holding exactly `addr` (Core's `CSubNet(const CNetAddr&)`); internal addresses
    /// give an invalid subnet.
    // Core: netaddress.cpp:997-1022 CSubNet::CSubNet(const CNetAddr&)
    pub fn single(addr: NetAddr) -> Self {
        let mut netmask = [0u8; 16];
        let valid = match addr.net {
            Network::Ipv4 | Network::Ipv6 => {
                netmask[..addr.bytes.len()].fill(0xFF);
                true
            }
            Network::Onion | Network::I2p | Network::Cjdns => true,
            Network::Internal | Network::Unroutable => false,
        };
        Self {
            network: if valid {
                addr
            } else {
                NetAddr::default()
            },
            netmask,
            valid,
        }
    }

    /// Core's `CSubNet(addr, mask_bits)`: IPv4 or IPv6 with a prefix length.
    // Core: netaddress.cpp:925-946 CSubNet::CSubNet(const CNetAddr&, uint8_t)
    pub fn with_prefix(addr: NetAddr, mask: u8) -> Self {
        let valid = (addr.is_ipv4() && usize::from(mask) <= ADDR_IPV4_SIZE * 8)
            || (addr.is_ipv6() && usize::from(mask) <= ADDR_IPV6_SIZE * 8);
        let mut netmask = [0u8; 16];
        if !valid {
            return Self {
                network: NetAddr::default(),
                netmask,
                valid,
            };
        }
        let mut network = addr;
        let mut n = mask;
        for (i, byte) in network.bytes.iter_mut().enumerate() {
            let bits = n.min(8);
            netmask[i] = ((0xFFu16 << (8 - bits)) & 0xFF) as u8;
            *byte &= netmask[i];
            n -= bits;
        }
        Self {
            network,
            netmask,
            valid,
        }
    }

    /// Core's `CSubNet::IsValid`.
    pub fn is_valid(&self) -> bool {
        self.valid
    }

    /// The base address.
    pub fn network(&self) -> &NetAddr {
        &self.network
    }

    /// Core's `CSubNet::Match`.
    // Core: netaddress.cpp:1024-1050 CSubNet::Match
    pub fn matches(&self, addr: &NetAddr) -> bool {
        if !self.valid || !addr.is_valid() || self.network.net != addr.net {
            return false;
        }
        match self.network.net {
            Network::Ipv4 | Network::Ipv6 => {}
            Network::Onion | Network::I2p | Network::Cjdns | Network::Internal => {
                return *addr == self.network;
            }
            Network::Unroutable => return false,
        }
        addr.bytes.iter().zip(&self.netmask).zip(&self.network.bytes).all(|((a, m), n)| a & m == *n)
    }

    /// The prefix length of an IPv4 or IPv6 subnet (Core's CIDR suffix).
    fn cidr(&self) -> u32 {
        self.netmask[..self.network.bytes.len()]
            .iter()
            .take_while(|&&m| m != 0)
            .map(|m| m.count_ones())
            .sum()
    }
}

impl PartialOrd for SubNet {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SubNet {
    /// Core's `operator<`: by network address, then netmask.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.network.cmp(&other.network).then_with(|| self.netmask.cmp(&other.netmask))
    }
}

impl fmt::Display for SubNet {
    /// Core's `CSubNet::ToString`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.network.net {
            Network::Ipv4 | Network::Ipv6 => write!(f, "{}/{}", self.network, self.cidr()),
            _ => write!(f, "{}", self.network),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8) -> NetAddr {
        NetAddr::ipv4([a, b, c, d])
    }

    fn v6(s: &str) -> NetAddr {
        NetAddr::from_ip(s.parse().unwrap())
    }

    /// Core's `netbase_tests/netbase_properties`.
    #[test]
    fn core_properties() {
        assert!(v4(10, 0, 0, 1).is_rfc1918());
        assert!(v4(192, 168, 1, 1).is_rfc1918());
        assert!(v4(172, 31, 255, 255).is_rfc1918());
        assert!(v4(198, 18, 0, 0).is_rfc2544());
        assert!(v4(198, 19, 255, 255).is_rfc2544());
        assert!(v6("2001:0DB8::").is_rfc3849());
        assert!(v4(169, 254, 1, 1).is_rfc3927());
        assert!(v6("2002::1").is_rfc3964());
        assert!(v6("FC00::").is_rfc4193());
        assert!(v6("2001::2").is_rfc4380());
        assert!(v6("2001:10::").is_rfc4843());
        assert!(v6("2001:20::").is_rfc7343());
        assert!(v6("FE80::").is_rfc4862());
        assert!(v6("64:FF9B::").is_rfc6052());
        // A Tor v2 prefix in the v1 form is the invalid `::` (`SetLegacyIPv6`).
        assert!(!v6("FD87:D87E:EB43:edb1:8e4:3588:e546:35ca").is_valid());
        assert!(v4(127, 0, 0, 1).is_local());
        assert!(v6("::1").is_local());
        assert!(v4(8, 8, 8, 8).is_routable(false));
        assert!(v6("2001::1").is_routable(false));
        assert!(v4(127, 0, 0, 1).is_valid());
        assert!(!v4(10, 0, 0, 1).is_routable(false));
        assert!(v4(10, 0, 0, 1).is_routable(true));
        assert!(!v6("::").is_valid());
        assert!(!v4(0, 0, 0, 0).is_valid());
        assert!(!v4(255, 255, 255, 255).is_valid());
    }

    #[test]
    fn legacy_and_bip155_forms() {
        let mapped = v6("::ffff:1.2.3.4");
        assert_eq!(mapped, v4(1, 2, 3, 4));
        assert_eq!(mapped.v1_bytes(), [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 1, 2, 3, 4]);
        let tor2 = NetAddr::from_legacy_ipv6([
            0xFD, 0x87, 0xD8, 0x7E, 0xEB, 0x43, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
        ]);
        assert!(!tor2.is_valid());
        // IPv4 embedded in a BIP155 IPv6 entry is the invalid `::`.
        let mut b = [0u8; 16];
        b[10] = 0xff;
        b[11] = 0xff;
        assert_eq!(NetAddr::from_addrv2(&AddrV2::Ipv6(b)), NetAddr::default());
        let internal = NetAddr::internal("dnsseed.dash.org").unwrap();
        assert_eq!(NetAddr::from_addrv2(&internal.to_addrv2()), internal);
        assert_eq!(NetAddr::from_legacy_ipv6(internal.v1_bytes()), internal);
        assert!(NetAddr::internal("").is_none());
    }

    /// Core's `netbase_tests/netbase_getgroup`-adjacent linked-IPv4 rules and `GetNetClass`.
    #[test]
    fn linked_ipv4_and_net_class() {
        assert_eq!(v6("2002:102:304:9999::").linked_ipv4(), 0x0102_0304);
        assert_eq!(v6("2001:0:9999:9999:9999:9999:FEFD:FCFB").linked_ipv4(), 0x0102_0304);
        assert_eq!(v6("::FFFF:0:102:304").linked_ipv4(), 0x0102_0304);
        assert_eq!(v6("64:FF9B::102:304").linked_ipv4(), 0x0102_0304);
        assert_eq!(v6("2002:102:304:9999::").net_class(false), Network::Ipv4);
        assert_eq!(v6("2002:102:304:9999::").network(false), Network::Ipv6);
        assert_eq!(v4(127, 0, 0, 1).network(false), Network::Unroutable);
    }

    #[test]
    fn service_key_and_display() {
        let s = Service::new(v4(1, 2, 3, 4), 9999);
        let mut key = s.addr.v1_bytes().to_vec();
        key.extend_from_slice(&[0x27, 0x0F]);
        assert_eq!(s.key(), key);
        assert_eq!(s.to_string(), "1.2.3.4:9999");
        assert_eq!(Service::new(v6("2001::1"), 1).to_string(), "[2001::1]:1");
    }

    /// Core's `netbase_tests/subnet_test` (a selection).
    #[test]
    fn subnets() {
        assert!(SubNet::with_prefix(v4(1, 2, 3, 0), 24).matches(&v4(1, 2, 3, 4)));
        assert!(!SubNet::with_prefix(v4(1, 2, 2, 0), 24).matches(&v4(1, 2, 3, 4)));
        assert!(SubNet::single(v4(1, 2, 3, 4)).matches(&v4(1, 2, 3, 4)));
        assert!(!SubNet::single(v4(1, 2, 3, 4)).matches(&v4(1, 2, 3, 5)));
        assert!(SubNet::with_prefix(v4(0, 0, 0, 0), 0).matches(&v4(1, 2, 3, 4)));
        assert!(!SubNet::single(NetAddr::internal("x").unwrap()).is_valid());
        assert_eq!(SubNet::with_prefix(v4(1, 2, 3, 4), 24).to_string(), "1.2.3.0/24");
        assert_eq!(SubNet::single(v4(1, 2, 3, 4)).to_string(), "1.2.3.4/32");
    }

    /// Core's `m_scope_id`: on the socket address and in the text, never in equality, order,
    /// hashing, subnet match or on the wire (Sol P4a r2 P2; D23).
    #[test]
    fn scope_id_is_cores() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let hash = |a: &NetAddr| {
            let mut h = DefaultHasher::new();
            a.hash(&mut h);
            h.finish()
        };
        let plain = v6("fe80::1");
        let scoped = plain.clone().with_scope_id(3);
        assert_eq!((plain.scope_id(), scoped.scope_id()), (0, 3));
        // operator== and operator< ignore the scope id (netaddress.cpp:610-617).
        assert_eq!(scoped, plain);
        assert_eq!(scoped.cmp(&plain), core::cmp::Ordering::Equal);
        assert_eq!(hash(&scoped), hash(&plain));
        // CSubNet::Match compares bytes; a subnet keeps its address's scope id.
        for net in [SubNet::single(scoped.clone()), SubNet::with_prefix(scoped.clone(), 64)] {
            assert!(net.matches(&plain));
            assert_eq!(net.network().scope_id(), 3);
        }
        assert!(SubNet::single(plain.clone()).matches(&scoped));
        assert_eq!(SubNet::single(scoped.clone()), SubNet::single(plain.clone()));
        // `%N` in ToStringAddr and CSubNet::ToString, IPv6 only.
        assert_eq!(scoped.to_string(), "fe80::1%3");
        assert_eq!(plain.to_string(), "fe80::1");
        assert_eq!(SubNet::single(scoped.clone()).to_string(), "fe80::1%3/128");
        assert_eq!(SubNet::with_prefix(scoped.clone(), 64).to_string(), "fe80::%3/64");
        assert_eq!(Service::new(scoped.clone(), 9999).to_string(), "[fe80::1%3]:9999");
        let cjdns = v6("fc00::1").maybe_flip_to_cjdns(true).with_scope_id(3);
        assert_eq!(cjdns.to_string(), "fc00::1");
        assert_eq!(v4(1, 2, 3, 4).with_scope_id(3).to_string(), "1.2.3.4");
        // MaybeFlipIPv6toCJDNS keeps it.
        assert_eq!(v6("fc00::1").with_scope_id(3).maybe_flip_to_cjdns(true).scope_id(), 3);
        // GetSockAddr sets sin6_scope_id for IPv6 and CJDNS; CService(sockaddr_in6) keeps it.
        let scope_of = |a: NetAddr| match Service::new(a, 1).to_socket_addr() {
            Some(SocketAddr::V6(sa)) => sa.scope_id(),
            other => panic!("{other:?}"),
        };
        assert_eq!(scope_of(scoped.clone()), 3);
        assert_eq!(scope_of(cjdns), 3);
        let sa = SocketAddr::V6(SocketAddrV6::new("fe80::1".parse().unwrap(), 9, 0, 3));
        assert_eq!(Service::from_socket_addr(sa).addr.scope_id(), 3);
        assert_eq!(Service::from_socket_addr(sa).to_socket_addr(), Some(sa));
        // The wire carries no scope id: ADDRv2 and the v1 form decode with none.
        assert_eq!(NetAddr::from_addrv2(&scoped.to_addrv2()).scope_id(), 0);
        assert_eq!(NetAddr::from_legacy_ipv6(scoped.v1_bytes()).scope_id(), 0);
        let v1 = Service::new(scoped, 9);
        assert_eq!(v1.key(), Service::new(plain, 9).key());
    }
}
