//! Independent re-implementations of the network-dependent renderings dashd uses in
//! `getblock <hash> 2`: base58check and DIP-18 bech32m addresses (`src/key_io.cpp`),
//! `ValueFromAmount` (`src/core_write.cpp`), and network endpoints (`CService::ToStringAddrPort`
//! in `src/netaddress.cpp`, `MnNetInfo`/`ExtNetInfo::ToJson` in `src/evo/netinfo.cpp`,
//! `GetNetInfoWithLegacyFields` in `src/evo/core_write.cpp`), all as of Dash Core v24.0.0-rc.3.
//!
//! Nothing here uses `dashcore` or `dash_primitives` code: the same renderers turn each library's
//! decoded values and the oracle's own source fields into dashd's text, so they must not share
//! a misinterpretation with either library. Only the hash primitives come from
//! `dashcore::hashes`.
//!
//! A renderer that meets a case it does not implement returns `Err(reason)`; callers report
//! that as a `harness-gap` finding rather than skipping the field.

use dashcore::hashes::{Hash, sha256d};

/// A rendering, or the reason the harness cannot produce it.
pub type Rendered = Result<String, String>;

/// The chain parameters that affect renderings (Core's `CChainParams`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Params {
    Main,
    /// testnet, devnet and regtest share every prefix used here.
    Test,
}

impl Params {
    /// The parameters for a report `net` name (`Context::net`).
    pub fn from_net(net: &str) -> Result<Self, String> {
        match net {
            "mainnet" => Ok(Self::Main),
            "testnet" | "regtest" | "devnet" => Ok(Self::Test),
            other => Err(format!("unknown network {other:?}: no address prefixes")),
        }
    }

    /// `base58Prefixes[PUBKEY_ADDRESS]` / `[SCRIPT_ADDRESS]` (src/chainparams.cpp).
    fn base58_prefix(self, d: &Dest) -> u8 {
        match (self, d) {
            (Self::Main, Dest::PkHash(_)) => 76,
            (Self::Main, Dest::ScriptHash(_)) => 16,
            (Self::Test, Dest::PkHash(_)) => 140,
            (Self::Test, Dest::ScriptHash(_)) => 19,
        }
    }

    /// `bech32_platform_hrp` (src/chainparams.cpp).
    fn platform_hrp(self) -> &'static str {
        match self {
            Self::Main => "dash",
            Self::Test => "tdash",
        }
    }
}

/// Core's `CTxDestination` (Dash has no witness destinations).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    PkHash([u8; 20]),
    ScriptHash([u8; 20]),
}

impl Dest {
    fn hash(&self) -> &[u8; 20] {
        match self {
            Self::PkHash(h) | Self::ScriptHash(h) => h,
        }
    }
}

const BASE58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// `EncodeBase58Check` (src/base58.cpp).
pub fn base58check(payload: &[u8]) -> String {
    let mut data = payload.to_vec();
    data.extend_from_slice(&sha256d::Hash::hash(payload).to_byte_array()[..4]);
    let zeros = data.iter().take_while(|b| **b == 0).count();
    // Base-58 digits, least significant first.
    let mut digits: Vec<u8> = Vec::new();
    for &byte in &data[zeros..] {
        let mut carry = u32::from(byte);
        for d in &mut digits {
            carry += u32::from(*d) << 8;
            *d = (carry % 58) as u8;
            carry /= 58;
        }
        while carry > 0 {
            digits.push((carry % 58) as u8);
            carry /= 58;
        }
    }
    let mut out = "1".repeat(zeros);
    out.extend(digits.iter().rev().map(|d| BASE58[*d as usize] as char));
    out
}

/// `EncodeDestination` (src/key_io.cpp).
pub fn encode_destination(d: &Dest, p: Params) -> String {
    let mut payload = vec![p.base58_prefix(d)];
    payload.extend_from_slice(d.hash());
    base58check(&payload)
}

/// `EncodeDestination(PKHash(keyID))`, as dashd prints owner/voting key IDs.
pub fn key_id_address(key_id: &[u8], p: Params) -> Rendered {
    let h: [u8; 20] =
        key_id.try_into().map_err(|_| format!("key ID of {} bytes, not 20", key_id.len()))?;
    Ok(encode_destination(&Dest::PkHash(h), p))
}

/// The bech32 alphabet, also `CHECKSUM_CHARSET` of descriptor checksums.
pub(crate) const BECH32_CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";

fn bech32_polymod(values: &[u8]) -> u32 {
    const GEN: [u32; 5] = [0x3b6a_57b2, 0x2650_8e6d, 0x1ea1_19fa, 0x3d42_33dd, 0x2a14_62b3];
    let mut chk: u32 = 1;
    for &v in values {
        let top = chk >> 25;
        chk = ((chk & 0x01ff_ffff) << 5) ^ u32::from(v);
        for (i, g) in GEN.iter().enumerate() {
            if (top >> i) & 1 == 1 {
                chk ^= g;
            }
        }
    }
    chk
}

/// `bech32::Encode(Encoding::BECH32M, hrp, values)` (src/bech32.cpp) for 5-bit `values`.
fn bech32m(hrp: &str, values: &[u8]) -> String {
    const BECH32M_CONST: u32 = 0x2bc8_30a3;
    let mut enc: Vec<u8> = hrp.bytes().map(|c| c >> 5).collect();
    enc.push(0);
    enc.extend(hrp.bytes().map(|c| c & 31));
    enc.extend_from_slice(values);
    enc.extend_from_slice(&[0; 6]);
    let m = bech32_polymod(&enc) ^ BECH32M_CONST;
    let mut out = format!("{hrp}1");
    out.extend(values.iter().map(|v| BECH32_CHARSET[*v as usize] as char));
    out.extend((0..6).map(|i| BECH32_CHARSET[((m >> (5 * (5 - i))) & 31) as usize] as char));
    out
}

/// `ConvertBits<8, 5, true>` (src/util/strencodings.h).
fn to_5bit(data: &[u8]) -> Vec<u8> {
    let (mut acc, mut bits, mut out) = (0u32, 0u32, Vec::new());
    for &b in data {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(((acc >> bits) & 31) as u8);
        }
    }
    if bits > 0 {
        out.push(((acc << (5 - bits)) & 31) as u8);
    }
    out
}

/// `EncodePlatformDestination(PlatformDestinationFromScript(..))` (src/key_io.cpp): DIP-18
/// bech32m with type byte 0xb0 (P2PKH) or 0x80 (P2SH) before the 20-byte hash.
pub fn platform_address(d: &Dest, p: Params) -> String {
    let type_byte = match d {
        Dest::PkHash(_) => 0xb0,
        Dest::ScriptHash(_) => 0x80,
    };
    let mut payload = vec![type_byte];
    payload.extend_from_slice(d.hash());
    bech32m(p.platform_hrp(), &to_5bit(&payload))
}

/// `ValueFromAmount` (src/core_write.cpp): `"%s%d.%08d"`.
pub fn value_from_amount(amount: i64) -> String {
    let sign = if amount < 0 {
        "-"
    } else {
        ""
    };
    let abs = amount.unsigned_abs();
    format!("{sign}{}.{:08}", abs / 100_000_000, abs % 100_000_000)
}

/// `EncodeBase32` (src/util/strencodings.cpp), lowercase RFC 4648 alphabet.
fn base32(data: &[u8], pad: bool) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out: String = to_5bit(data).iter().map(|v| ALPHABET[*v as usize] as char).collect();
    if pad {
        while !out.len().is_multiple_of(8) {
            out.push('=');
        }
    }
    out
}

const IPV4_IN_IPV6: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];
const TORV2_IN_IPV6: [u8; 6] = [0xfd, 0x87, 0xd8, 0x7e, 0xeb, 0x43];
const INTERNAL_IN_IPV6: [u8; 6] = [0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24];

/// Core's `CNetAddr`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetAddr {
    Ipv4([u8; 4]),
    Ipv6([u8; 16]),
    Onion([u8; 32]),
    I2p([u8; 32]),
    Cjdns([u8; 16]),
    Internal([u8; 10]),
}

impl NetAddr {
    /// `CNetAddr::SetLegacyIPv6`: the 16-byte ADDRv1 form.
    pub fn from_legacy(b: [u8; 16]) -> Self {
        if b[..12] == IPV4_IN_IPV6 {
            Self::Ipv4(b[12..].try_into().expect("4 bytes"))
        } else if b[..6] == TORV2_IN_IPV6 {
            // TORv2 is unsupported; Core keeps an all-zero (invalid) IPv6 address.
            Self::Ipv6([0; 16])
        } else if b[..6] == INTERNAL_IN_IPV6 {
            Self::Internal(b[6..].try_into().expect("10 bytes"))
        } else {
            Self::Ipv6(b)
        }
    }

    /// An IPv6 address read in BIP155 form (`CNetAddr::UnserializeV2Stream`): embedded IPv4 or
    /// TORv2 forms are not allowed there and become the invalid all-zero address.
    pub fn from_bip155_ipv6(b: [u8; 16]) -> Self {
        if b[..12] == IPV4_IN_IPV6 || b[..6] == TORV2_IN_IPV6 {
            Self::Ipv6([0; 16])
        } else {
            Self::from_legacy(b)
        }
    }

    /// `CNetAddr::IsValid`.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Ipv6(a) => *a != [0; 16] && a[..4] != [0x20, 0x01, 0x0d, 0xb8],
            Self::Cjdns(a) => a[0] == 0xfc,
            Self::Internal(_) => false,
            Self::Ipv4(a) => *a != [0; 4] && *a != [0xff; 4],
            Self::Onion(_) | Self::I2p(_) => true,
        }
    }

    /// `CNetAddr::ToStringAddr`.
    pub fn to_string_addr(&self) -> Rendered {
        Ok(match self {
            Self::Ipv4(a) => format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]),
            Self::Ipv6(a) | Self::Cjdns(a) => ipv6_to_string(a),
            Self::Onion(_) => {
                return Err("TORv3 .onion rendering (SHA3-256 checksum) is not implemented".into());
            }
            Self::I2p(a) => format!("{}.b32.i2p", base32(a, false)),
            Self::Internal(a) => format!("{}.internal", base32(a, true)),
        })
    }
}

/// `IPv6ToString` (src/netaddress.cpp): RFC 5952 zero compression of the longest (first) run
/// of at least two zero groups.
fn ipv6_to_string(a: &[u8; 16]) -> String {
    let groups: Vec<u16> = a.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
    let (mut best, mut cur) = ((0usize, 0usize), (0usize, 0usize));
    for (i, g) in groups.iter().enumerate() {
        if *g != 0 {
            cur = (i + 1, 0);
            continue;
        }
        cur.1 += 1;
        if cur.1 > best.1 {
            best = cur;
        }
    }
    let mut r = String::new();
    for (i, g) in groups.iter().enumerate() {
        if best.1 >= 2 && i >= best.0 && i < best.0 + best.1 {
            if i == best.0 {
                r.push_str("::");
            }
            continue;
        }
        if !r.is_empty() && !r.ends_with(':') {
            r.push(':');
        }
        r.push_str(&format!("{g:x}"));
    }
    r
}

/// Core's `CService`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Service {
    pub addr: NetAddr,
    pub port: u16,
}

impl Service {
    /// The 18-byte ADDRv1 wire form: 16-byte address, big-endian port.
    pub fn from_legacy(addr: [u8; 16], port: u16) -> Self {
        Self {
            addr: NetAddr::from_legacy(addr),
            port,
        }
    }

    /// `CService::ToStringAddrPort`.
    pub fn to_string_addr_port(&self) -> Rendered {
        let a = self.addr.to_string_addr()?;
        Ok(match self.addr {
            NetAddr::Ipv4(_) | NetAddr::Onion(_) | NetAddr::I2p(_) | NetAddr::Internal(_) => {
                format!("{a}:{}", self.port)
            }
            NetAddr::Ipv6(_) | NetAddr::Cjdns(_) => format!("[{a}]:{}", self.port),
        })
    }
}

/// `CService{}`, rendered `[::]:0`.
fn null_service() -> Service {
    Service {
        addr: NetAddr::Ipv6([0; 16]),
        port: 0,
    }
}

/// Core's `NetInfoEntry` after deserialization: invalid data becomes `Invalid`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    Service(Service),
    Domain {
        name: Vec<u8>,
        port: u16,
    },
    Invalid,
}

impl Entry {
    /// A service entry; Core's `NetInfoEntry(CService)` drops invalid addresses.
    pub fn service(s: Service) -> Self {
        if s.addr.is_valid() {
            Self::Service(s)
        } else {
            Self::Invalid
        }
    }

    /// A domain entry; Core's `NetInfoEntry` deserialization drops domains that fail
    /// `DomainPort::Validate`.
    pub fn domain(name: Vec<u8>, port: u16) -> Self {
        if domain_valid(&name, port) {
            Self::Domain {
                name,
                port,
            }
        } else {
            Self::Invalid
        }
    }

    /// `NetInfoEntry::ToStringAddrPort`.
    fn to_string_addr_port(&self) -> Rendered {
        match self {
            Self::Service(s) => s.to_string_addr_port(),
            // Validated ASCII (see `domain_valid`).
            Self::Domain {
                name,
                port,
            } => Ok(format!("{}:{port}", String::from_utf8_lossy(name))),
            Self::Invalid => Ok("[invalid entry]".into()),
        }
    }
}

/// `DomainPort::Validate` (src/evo/netinfo.cpp): lowercase, non-zero port, and
/// `ValidateDomain`: 3 to 253 characters of `SAFE_CHARS_RFC1035`, no leading or trailing dot,
/// at least two labels of 1 to 63 characters that neither start nor end with `-`.
fn domain_valid(name: &[u8], port: u16) -> bool {
    let rfc1035 = |c: &u8| c.is_ascii_alphanumeric() || *c == b'.' || *c == b'-';
    if port == 0
        || !(3..=253).contains(&name.len())
        || name.iter().any(u8::is_ascii_uppercase)
        || !name.iter().all(rfc1035)
        || name.first() == Some(&b'.')
        || name.last() == Some(&b'.')
    {
        return false;
    }
    let labels: Vec<&[u8]> = name.split(|c| *c == b'.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty() && l.len() <= 63 && l.first() != Some(&b'-') && l.last() != Some(&b'-')
        })
}

impl Entry {}

/// Core's `MnNetInfo` (`Legacy`) or `ExtNetInfo` (`Extended`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetInfo {
    Legacy(Entry),
    Extended {
        version: u8,
        /// `(purpose, entries)` in wire order.
        groups: Vec<(u8, Vec<Entry>)>,
    },
}

/// `PurposeToString` (src/evo/netinfo.h).
fn purpose_name(purpose: u8) -> Result<&'static str, String> {
    match purpose {
        0 => Ok("core_p2p"),
        1 => Ok("platform_p2p"),
        2 => Ok("platform_https"),
        p => Err(format!("unknown netinfo purpose {p}")),
    }
}

impl NetInfo {
    /// The purpose groups as Core's `std::map` holds them: ordered by purpose, first group
    /// wins on duplicates; an unknown `ExtNetInfo` version reads no groups.
    fn groups(&self) -> Vec<(u8, &[Entry])> {
        match self {
            Self::Legacy(Entry::Invalid) => Vec::new(),
            Self::Legacy(e) => vec![(0, std::slice::from_ref(e))],
            Self::Extended {
                version,
                groups,
            } => {
                if *version != 1 {
                    return Vec::new();
                }
                let mut out: Vec<(u8, &[Entry])> = Vec::new();
                for (p, entries) in groups {
                    if !out.iter().any(|(q, _)| q == p) {
                        out.push((*p, entries));
                    }
                }
                out.sort_by_key(|(p, _)| *p);
                out
            }
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Self::Legacy(e) => *e == Entry::Invalid,
            Self::Extended {
                version,
                ..
            } => *version == 1 && self.groups().is_empty(),
        }
    }

    /// The first entry of the `purpose` group, if any.
    fn first_entry(&self, purpose: u8) -> Option<&Entry> {
        self.groups().into_iter().find(|(p, _)| *p == purpose).and_then(|(_, e)| e.first())
    }

    /// `GetPrimary()`: the first core P2P entry when it is a service, else `CService{}`.
    fn primary(&self) -> Service {
        match self.first_entry(0) {
            Some(Entry::Service(s)) => s.clone(),
            _ => null_service(),
        }
    }
}

/// The legacy (pre-ExtAddr) platform ports of an Evo masternode.
#[derive(Clone, Copy, Debug)]
pub struct LegacyPorts {
    pub p2p: u16,
    pub http: u16,
}

/// The `addresses`, `service`, `platformP2PPort` and `platformHTTPPort` fields dashd prints
/// for a ProRegTx/ProUpServTx, as `(path relative to the payload object, rendering)`.
///
/// `ports` are the payload's platform ports of an Evo masternode with legacy netinfo (for which
/// the fields that need them are left out when `None`); `evo` is the masternode type. `service` and the two port fields are printed
/// only with `-deprecatedrpc=service`; callers compare them only when the oracle has them.
pub fn netinfo_fields(
    ni: &NetInfo,
    evo: bool,
    ports: Option<LegacyPorts>,
) -> Vec<(String, Rendered)> {
    let mut out = Vec::new();
    // MnNetInfo/ExtNetInfo::ToJson
    for (purpose, entries) in ni.groups() {
        let name = match purpose_name(purpose) {
            Ok(n) => n,
            Err(e) => {
                out.push((format!("addresses.<purpose {purpose}>"), Err(e)));
                continue;
            }
        };
        for (i, e) in entries.iter().enumerate() {
            out.push((format!("addresses.{name}[{i}]"), e.to_string_addr_port()));
        }
    }
    let legacy = matches!(ni, NetInfo::Legacy(_));
    // GetNetInfoWithLegacyFields: legacy Evo entries get platform arrays from the primary
    // address and the payload's platform ports. A library that decoded an Evo legacy payload
    // without its ports gets nothing here, so the oracle's values are `verbose.missing`.
    let legacy_ports = ports.filter(|_| evo && legacy);
    if let Some(p) = legacy_ports
        && !ni.is_empty()
    {
        let addr = ni.primary().addr;
        for (name, port) in [("platform_https", p.http), ("platform_p2p", p.p2p)] {
            let r = Service {
                addr: addr.clone(),
                port,
            }
            .to_string_addr_port();
            out.push((format!("addresses.{name}[0]"), r));
        }
    }
    // -deprecatedrpc=service fields.
    out.push(("service".into(), ni.primary().to_string_addr_port()));
    if evo {
        let deprecated_port = |purpose: u8| -> Rendered {
            if ni.is_empty() {
                return Ok("-1".into());
            }
            match ni.first_entry(purpose) {
                Some(Entry::Service(s)) => Ok(s.port.to_string()),
                Some(Entry::Domain {
                    port,
                    ..
                }) => Ok(port.to_string()),
                Some(Entry::Invalid) => Ok("0".into()),
                None => Err(format!("no purpose-{purpose} entry (dashd would fail the RPC)")),
            }
        };
        let (p2p, http) = match (legacy, legacy_ports) {
            (true, Some(p)) => (Ok(p.p2p.to_string()), Ok(p.http.to_string())),
            (true, None) => return out,
            (false, _) => (deprecated_port(1), deprecated_port(2)),
        };
        out.push(("platformP2PPort".into(), p2p));
        out.push(("platformHTTPPort".into(), http));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex20(s: &str) -> [u8; 20] {
        hex_conservative::decode_to_array(s).unwrap()
    }

    #[test]
    fn base58_addresses() {
        let enc = encode_destination(&Dest::PkHash(hex20(&"11".repeat(20))), Params::Test);
        assert!(enc.starts_with('y'), "{enc}");
        // Leading zero bytes become '1's.
        assert_eq!(base58check(&[0, 0, 1]), "11BwW2qR");
        assert_eq!(
            encode_destination(&Dest::PkHash([0; 20]), Params::Main),
            "XagqqFetxiDb9wbartKDrXgnqLah6SqX2S"
        );
    }

    #[test]
    fn bech32m_matches_bip350_vector() {
        // BIP-350 test vector "a1lqfn3a" (empty data, hrp "a").
        assert_eq!(bech32m("a", &[]), "a1lqfn3a");
        // "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx" (data = 31 down to 0).
        let data: Vec<u8> = (0..32).rev().collect();
        assert_eq!(bech32m("abcdef", &data), "abcdef1l7aum6echk45nj3s0wdvt2fg8x9yrzpqzd3ryx");
    }

    #[test]
    fn domains() {
        let ok = |n: &str| domain_valid(n.as_bytes(), 443);
        assert!(ok("example.com") && ok("a-b.c0.io"));
        for bad in
            ["localhost", "a..b", "-x.com", "x-.com", ".ab.c", "ab.c.", "Ab.com", "a_b.com", "ab"]
        {
            assert!(!ok(bad), "{bad}");
        }
        assert!(!ok(&format!("{}.com", "a".repeat(64))));
        assert!(!domain_valid(b"example.com", 0));
        assert_eq!(Entry::domain(b"a..b".to_vec(), 1), Entry::Invalid);
    }

    #[test]
    fn platform_address() {
        // dashd's rendering in tests/data/regtest-feature_asset_locks-750.
        let h: [u8; 20] = hex20("c71b73778695ccd1dc6b072f9f8d6ff027a10fcf");
        assert_eq!(
            super::platform_address(&Dest::PkHash(h), Params::Test),
            "tdash1krr3kumhs62ue5wudvrjl8uddlcz0gg0eunyxauu"
        );
    }

    #[test]
    fn amounts() {
        assert_eq!(value_from_amount(0), "0.00000000");
        assert_eq!(value_from_amount(50_000_000_000), "500.00000000");
        assert_eq!(value_from_amount(1), "0.00000001");
        assert_eq!(value_from_amount(-150_000_000), "-1.50000000");
    }

    #[test]
    fn endpoints() {
        let mut v4 = [0u8; 16];
        v4[10] = 0xff;
        v4[11] = 0xff;
        v4[12..].copy_from_slice(&[127, 0, 0, 1]);
        let s = Service::from_legacy(v4, 9999);
        assert_eq!(s.to_string_addr_port().unwrap(), "127.0.0.1:9999");
        let mut v6 = [0u8; 16];
        v6[0] = 0x2a;
        v6[1] = 0x01;
        v6[15] = 1;
        assert_eq!(Service::from_legacy(v6, 1).to_string_addr_port().unwrap(), "[2a01::1]:1");
        assert_eq!(null_service().to_string_addr_port().unwrap(), "[::]:0");
        assert!(!NetAddr::from_legacy([0; 16]).is_valid());
        // "1:0:0:1::1": the first of two equal zero runs is not chosen when the second is longer.
        let mut g = [0u8; 16];
        g[1] = 1;
        g[7] = 1;
        g[15] = 1;
        assert_eq!(ipv6_to_string(&g), "1:0:0:1::1");
        assert_eq!(base32(&[0xff; 32], false).len(), 52);
    }

    #[test]
    fn legacy_evo_netinfo_fields() {
        let mut v4 = [0u8; 16];
        v4[10..].copy_from_slice(&[0xff, 0xff, 10, 0, 0, 1]);
        let ni = NetInfo::Legacy(Entry::service(Service::from_legacy(v4, 19999)));
        let f = netinfo_fields(
            &ni,
            true,
            Some(LegacyPorts {
                p2p: 26656,
                http: 443,
            }),
        );
        let get = |k: &str| f.iter().find(|(p, _)| p == k).unwrap().1.clone().unwrap();
        assert_eq!(get("addresses.core_p2p[0]"), "10.0.0.1:19999");
        assert_eq!(get("platformHTTPPort"), "443");
        assert_eq!(get("addresses.platform_https[0]"), "10.0.0.1:443");
        assert_eq!(get("addresses.platform_p2p[0]"), "10.0.0.1:26656");
        assert_eq!(get("service"), "10.0.0.1:19999");
        assert_eq!(get("platformP2PPort"), "26656");
        // Without the payload's ports, the fields that need them are left to `verbose.missing`.
        let f = netinfo_fields(&ni, true, None);
        let keys: Vec<&str> = f.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["addresses.core_p2p[0]", "service"]);
        // An empty legacy netinfo prints no addresses and the null service.
        let f = netinfo_fields(&NetInfo::Legacy(Entry::Invalid), false, None);
        assert_eq!(f, vec![("service".to_string(), Ok("[::]:0".to_string()))]);
    }
}
