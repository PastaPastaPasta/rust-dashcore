//! `LookupSubNet`: the text of a ban (`"1.2.0.0/16"`, an onion name, ...) as the network Core
//! bans (`netbase.cpp` `LookupSubNet`, `LookupHost` with `fAllowLookup = false`, `netaddress.cpp`
//! `CNetAddr::SetSpecial`, `SetTor`, `SetI2P`, `SetLegacyIPv6` and the `CSubNet` constructors).
//!
//! The banlist loader needs the **validity** Core decides here: an entry whose address does not
//! name a valid subnet is dropped before its timestamps are read, so a bad timestamp on such an
//! entry must not fail the file (`BanMapFromJson`).
//!
//! Two inputs of Core's decision are not in the file: the platform resolver for numeric hosts
//! (`getaddrinfo` with `AI_NUMERICHOST`: this module follows glibc's `inet_aton` forms, the IPv6
//! syntax of `inet_pton` and `__inet6_scopeid_pton` for a `%scope`; an interface name as a scope
//! is looked up on the importing host, see [`scope_id_ok`]) and `-cjdnsreachable`
//! ([`lookup_subnet`]'s `cjdns_reachable`). A scope id is not part of a subnet's identity or
//! order in Core (`CNetAddr::operator==` and `operator<` compare the network and the address
//! bytes), so it is checked and dropped.

use std::net::Ipv6Addr;
use std::str::FromStr;

/// A banned network. Equality is Core's `CSubNet::operator==` for valid subnets: same network and
/// same netmask. The order is `CSubNet::operator<` (the order of Core's `banmap_t`): the network
/// class (IPv4, IPv6, onion, I2P, CJDNS, as `Network` numbers them), the address bytes, then the
/// netmask bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subnet {
    V4 {
        network: [u8; 4],
        mask: [u8; 4],
    },
    V6 {
        network: [u8; 16],
        mask: [u8; 16],
    },
    /// A TORv3 service (the 32-byte public key); always a single host.
    Onion([u8; 32]),
    /// An I2P destination hash; always a single host.
    I2p([u8; 32]),
    /// A CJDNS address (`fc00::/8`), only when `-cjdnsreachable` is set; a single host.
    Cjdns([u8; 16]),
}

/// A `CNetAddr` that is a candidate for a subnet.
#[derive(Clone, Copy)]
enum Addr {
    V4([u8; 4]),
    V6([u8; 16]),
    Onion([u8; 32]),
    I2p([u8; 32]),
    Cjdns([u8; 16]),
}

// ---------------------------------------------------------------------------------------------
// SHA3-256 (for the TORv3 checksum)

const KECCAK_RC: [u64; 24] = [
    0x0000_0000_0000_0001,
    0x0000_0000_0000_8082,
    0x8000_0000_0000_808a,
    0x8000_0000_8000_8000,
    0x0000_0000_0000_808b,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8009,
    0x0000_0000_0000_008a,
    0x0000_0000_0000_0088,
    0x0000_0000_8000_8009,
    0x0000_0000_8000_000a,
    0x0000_0000_8000_808b,
    0x8000_0000_0000_008b,
    0x8000_0000_0000_8089,
    0x8000_0000_0000_8003,
    0x8000_0000_0000_8002,
    0x8000_0000_0000_0080,
    0x0000_0000_0000_800a,
    0x8000_0000_8000_000a,
    0x8000_0000_8000_8081,
    0x8000_0000_0000_8080,
    0x0000_0000_8000_0001,
    0x8000_0000_8000_8008,
];
const KECCAK_ROT: [u32; 24] =
    [1, 3, 6, 10, 15, 21, 28, 36, 45, 55, 2, 14, 27, 41, 56, 8, 25, 43, 62, 18, 39, 61, 20, 44];
const KECCAK_PI: [usize; 24] =
    [10, 7, 11, 17, 18, 3, 5, 16, 8, 21, 24, 4, 15, 23, 19, 13, 12, 2, 20, 14, 22, 9, 6, 1];

fn keccak_f(state: &mut [u64; 25]) {
    for rc in KECCAK_RC {
        let mut c = [0u64; 5];
        for (x, cx) in c.iter_mut().enumerate() {
            *cx = state[x] ^ state[x + 5] ^ state[x + 10] ^ state[x + 15] ^ state[x + 20];
        }
        for x in 0..5 {
            let d = c[(x + 4) % 5] ^ c[(x + 1) % 5].rotate_left(1);
            for y in 0..5 {
                state[x + 5 * y] ^= d;
            }
        }
        let mut last = state[1];
        for (rot, pi) in KECCAK_ROT.iter().zip(KECCAK_PI) {
            let next = state[pi];
            state[pi] = last.rotate_left(*rot);
            last = next;
        }
        for y in 0..5 {
            let row = [
                state[5 * y],
                state[5 * y + 1],
                state[5 * y + 2],
                state[5 * y + 3],
                state[5 * y + 4],
            ];
            for x in 0..5 {
                state[5 * y + x] = row[x] ^ (!row[(x + 1) % 5] & row[(x + 2) % 5]);
            }
        }
        state[0] ^= rc;
    }
}

/// SHA3-256 (FIPS 202) of `data`.
pub(crate) fn sha3_256(data: &[u8]) -> [u8; 32] {
    const RATE: usize = 136;
    let mut state = [0u64; 25];
    let absorb = |block: &[u8; RATE], state: &mut [u64; 25]| {
        for (lane, bytes) in state.iter_mut().zip(block.as_chunks::<8>().0) {
            *lane ^= u64::from_le_bytes(*bytes);
        }
        keccak_f(state);
    };
    let (blocks, rest) = data.as_chunks::<RATE>();
    for block in blocks {
        absorb(block, &mut state);
    }
    let mut last = [0u8; RATE];
    last[..rest.len()].copy_from_slice(rest);
    last[rest.len()] ^= 0x06;
    last[RATE - 1] ^= 0x80;
    absorb(&last, &mut state);
    let mut out = [0u8; 32];
    for (bytes, lane) in out.as_chunks_mut::<8>().0.iter_mut().zip(state) {
        *bytes = lane.to_le_bytes();
    }
    out
}

// ---------------------------------------------------------------------------------------------
// base32, `.onion` and `.b32.i2p`

/// `DecodeBase32` (RFC 4648 alphabet, either case, optional padding, no stray bits).
// Core: util/strencodings.cpp DecodeBase32, ConvertBits<5, 8, false>
fn decode_base32(input: &[u8]) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(8) {
        return None;
    }
    let mut s = input;
    // 1, 3, 4 or 6 padding characters, removed in the order Core tries them.
    if let [rest @ .., b'='] = s {
        s = rest;
    }
    if let [rest @ .., b'=', b'='] = s {
        s = rest;
    }
    if let [rest @ .., b'='] = s {
        s = rest;
    }
    if let [rest @ .., b'=', b'='] = s {
        s = rest;
    }
    let (mut acc, mut bits) = (0u32, 0u32);
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    for &c in s {
        let v = match c {
            b'a'..=b'z' => c - b'a',
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        acc = ((acc << 5) | u32::from(v)) & ((1 << 12) - 1);
        bits += 5;
        while bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    if bits >= 5 || ((acc << (8 - bits)) & 0xff) != 0 {
        return None;
    }
    Some(out)
}

/// `CNetAddr::SetTor`: a TORv3 `.onion` name.
fn set_tor(addr: &[u8]) -> Option<Addr> {
    const SUFFIX: &[u8] = b".onion";
    if addr.len() <= SUFFIX.len() || !addr.ends_with(SUFFIX) {
        return None;
    }
    let input = decode_base32(&addr[..addr.len() - SUFFIX.len()])?;
    if input.len() != 32 + 2 + 1 {
        return None;
    }
    let (pubkey, checksum, version) = (&input[..32], &input[32..34], input[34]);
    if version != 3 {
        return None;
    }
    let mut preimage = b".onion checksum".to_vec();
    preimage.extend_from_slice(pubkey);
    preimage.push(3);
    if checksum != &sha3_256(&preimage)[..2] {
        return None;
    }
    Some(Addr::Onion(pubkey.try_into().ok()?))
}

/// `CNetAddr::SetI2P`: 52 base32 characters and `.b32.i2p`.
fn set_i2p(addr: &[u8]) -> Option<Addr> {
    const B32_LEN: usize = 52;
    const SUFFIX: &[u8] = b".b32.i2p";
    if addr.len() != B32_LEN + SUFFIX.len() || !addr[B32_LEN..].eq_ignore_ascii_case(SUFFIX) {
        return None;
    }
    let mut padded = addr[..B32_LEN].to_vec();
    padded.extend_from_slice(b"====");
    let bytes = decode_base32(&padded)?;
    Some(Addr::I2p(bytes.try_into().ok()?))
}

// ---------------------------------------------------------------------------------------------
// numeric hosts (`getaddrinfo` with `AI_NUMERICHOST`)

/// glibc's `inet_aton` forms without trailing characters: one to four parts, each decimal, octal
/// (leading `0`) or hex (`0x`); the last part fills the remaining bytes.
// glibc: resolv/inet_addr.c __inet_aton_exact
fn inet_aton_exact(s: &[u8]) -> Option<[u8; 4]> {
    let mut parts = [0u32; 3];
    let mut n_parts = 0;
    let mut i = 0;
    let val = loop {
        if !s.get(i).is_some_and(u8::is_ascii_digit) {
            return None;
        }
        let mut base = 10u64;
        if s[i] == b'0' {
            i += 1;
            if matches!(s.get(i), Some(b'x' | b'X')) {
                base = 16;
                i += 1;
                // glibc refuses a `0x` prefix with no hex digit after it.
                if !s.get(i).is_some_and(u8::is_ascii_hexdigit) {
                    return None;
                }
            } else {
                base = 8;
            }
        }
        let mut val = 0u64;
        while let Some(&c) = s.get(i) {
            let digit = match c {
                b'0'..=b'9' if base == 8 && c >= b'8' => return None,
                b'0'..=b'9' => u64::from(c - b'0'),
                b'a'..=b'f' if base == 16 => u64::from(c - b'a') + 10,
                b'A'..=b'F' if base == 16 => u64::from(c - b'A') + 10,
                _ => break,
            };
            val = val * base + digit;
            if val > u64::from(u32::MAX) {
                return None;
            }
            i += 1;
        }
        if s.get(i) == Some(&b'.') {
            if n_parts >= 3 || val > 0xff {
                return None;
            }
            parts[n_parts] = val as u32;
            n_parts += 1;
            i += 1;
        } else {
            break val as u32;
        }
    };
    if i != s.len() {
        return None;
    }
    let packed = match n_parts {
        0 => val,
        1 if val <= 0xff_ffff => (parts[0] << 24) | val,
        2 if val <= 0xffff => (parts[0] << 24) | (parts[1] << 16) | val,
        3 if val <= 0xff => (parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | val,
        _ => return None,
    };
    Some(packed.to_be_bytes())
}

/// `CNetAddr::SetLegacyIPv6` for an address from the resolver: IPv4-in-IPv6 is IPv4, the internal
/// prefix is "internal" (`None`: `LookupIntern` never returns those), the TORv2 prefix is the
/// unspecified IPv6 address.
fn from_in6(octets: [u8; 16]) -> Option<Addr> {
    const IPV4_IN_IPV6: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff];
    const TORV2_IN_IPV6: [u8; 6] = [0xfd, 0x87, 0xd8, 0x7e, 0xeb, 0x43];
    const INTERNAL_IN_IPV6: [u8; 6] = [0xfd, 0x6b, 0x88, 0xc0, 0x87, 0x24];
    if octets.starts_with(&IPV4_IN_IPV6) {
        Some(Addr::V4([octets[12], octets[13], octets[14], octets[15]]))
    } else if octets.starts_with(&TORV2_IN_IPV6) {
        Some(Addr::V6([0; 16]))
    } else if octets.starts_with(&INTERNAL_IN_IPV6) {
        None
    } else {
        Some(Addr::V6(octets))
    }
}

/// `LookupHost(name, fAllowLookup = false)`.
// Core: netbase.cpp LookupHost, LookupIntern, WrappedGetAddrInfo
fn lookup_host(name: &[u8]) -> Option<Addr> {
    if name.is_empty() || name.contains(&0) {
        return None;
    }
    let host = match name {
        [b'[', inner @ .., b']'] => inner,
        _ => name,
    };
    if host.contains(&0) {
        return None;
    }
    if let Some(special) = set_tor(host).or_else(|| set_i2p(host)) {
        return Some(special);
    }
    if let Some(v4) = inet_aton_exact(host) {
        return Some(Addr::V4(v4));
    }
    // `getaddrinfo` splits the name at the first `%`: an address, then a scope id.
    let (address, scope) = match host.iter().position(|&b| b == b'%') {
        Some(at) => (&host[..at], Some(&host[at + 1..])),
        None => (host, None),
    };
    let octets = Ipv6Addr::from_str(std::str::from_utf8(address).ok()?).ok()?.octets();
    if let Some(scope) = scope
        && !scope_id_ok(&octets, scope)
    {
        return None;
    }
    from_in6(octets)
}

/// Whether glibc's `__inet6_scopeid_pton` accepts `scope` for the address: for a link-local
/// (`fe80::/10`), interface-local (`ff?1::`) or link-local multicast (`ff?2::`) address an
/// interface name of the host (`if_nametoindex`, so **a host input**: the importing machine's
/// interfaces; non-Unix targets know none), else a decimal number that fits in 32 bits (digits
/// only: no sign, no space).
// glibc: inet/inet6_scopeid_pton.c (called from sysdeps/posix/getaddrinfo.c)
fn scope_id_ok(octets: &[u8; 16], scope: &[u8]) -> bool {
    let named = (octets[0] == 0xfe && octets[1] & 0xc0 == 0x80)
        || (octets[0] == 0xff && matches!(octets[1] & 0x0f, 1 | 2));
    if named && interface_exists(scope) {
        return true;
    }
    !scope.is_empty()
        && scope.iter().all(u8::is_ascii_digit)
        && scope
            .iter()
            .try_fold(0u64, |n, d| {
                let n = n * 10 + u64::from(d - b'0');
                (n <= u64::from(u32::MAX)).then_some(n)
            })
            .is_some()
}

#[cfg(unix)]
fn interface_exists(name: &[u8]) -> bool {
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: `name` is a NUL-terminated string that outlives the call.
    unsafe { libc::if_nametoindex(name.as_ptr()) != 0 }
}

#[cfg(not(unix))]
fn interface_exists(_: &[u8]) -> bool {
    false
}

// ---------------------------------------------------------------------------------------------
// `CSubNet`

fn net_kind(a: &Addr) -> u8 {
    match a {
        Addr::V4(_) => 0,
        Addr::V6(_) => 1,
        Addr::Onion(_) => 2,
        Addr::I2p(_) => 3,
        Addr::Cjdns(_) => 4,
    }
}

/// A byte whose top `take` bits (0 to 8) are 1.
fn prefix_byte(take: u32) -> u8 {
    (0xff00u16 >> take) as u8
}

/// `CSubNet(addr, uint8_t mask)`.
fn subnet_with_prefix(addr: Addr, bits: u8) -> Option<Subnet> {
    let normalize = |octets: &mut [u8]| {
        let mut n = u32::from(bits);
        for byte in octets.iter_mut() {
            let take = n.min(8);
            *byte &= prefix_byte(take);
            n -= take;
        }
    };
    let masks = |len: usize, out: &mut [u8]| {
        let mut n = u32::from(bits);
        for byte in out.iter_mut().take(len) {
            let take = n.min(8);
            *byte = prefix_byte(take);
            n -= take;
        }
    };
    match addr {
        Addr::V4(mut network) if bits <= 32 => {
            let mut mask = [0; 4];
            masks(4, &mut mask);
            normalize(&mut network);
            Some(Subnet::V4 {
                network,
                mask,
            })
        }
        Addr::V6(mut network) if bits <= 128 => {
            let mut mask = [0; 16];
            masks(16, &mut mask);
            normalize(&mut network);
            Some(Subnet::V6 {
                network,
                mask,
            })
        }
        _ => None,
    }
}

/// `NetmaskBits`: the number of leading 1-bits of a mask byte that is one run from the top.
fn netmask_bits(x: u8) -> Option<u32> {
    (x.leading_ones() + x.trailing_zeros() == 8 || x == 0).then(|| x.leading_ones())
}

/// `CSubNet(addr, CNetAddr mask)`.
fn subnet_with_mask(addr: Addr, mask: Addr) -> Option<Subnet> {
    if !matches!(addr, Addr::V4(_) | Addr::V6(_)) || net_kind(&addr) != net_kind(&mask) {
        return None;
    }
    let (addr_bytes, mask_bytes): (&[u8], &[u8]) = match (&addr, &mask) {
        (Addr::V4(a), Addr::V4(m)) => (a, m),
        (Addr::V6(a), Addr::V6(m)) => (a, m),
        _ => return None,
    };
    let mut zeros_found = false;
    for &b in mask_bytes {
        let bits = netmask_bits(b)?;
        if zeros_found && bits != 0 {
            return None;
        }
        if bits < 8 {
            zeros_found = true;
        }
    }
    let and = |a: &[u8], m: &[u8]| -> Vec<u8> { a.iter().zip(m).map(|(a, m)| a & m).collect() };
    let network = and(addr_bytes, mask_bytes);
    Some(match addr {
        Addr::V4(_) => Subnet::V4 {
            network: network.try_into().ok()?,
            mask: mask_bytes.try_into().ok()?,
        },
        _ => Subnet::V6 {
            network: network.try_into().ok()?,
            mask: mask_bytes.try_into().ok()?,
        },
    })
}

/// `CSubNet(addr)`: a single host.
fn subnet_of_host(addr: Addr) -> Subnet {
    match addr {
        Addr::V4(network) => Subnet::V4 {
            network,
            mask: [0xff; 4],
        },
        Addr::V6(network) => Subnet::V6 {
            network,
            mask: [0xff; 16],
        },
        Addr::Onion(key) => Subnet::Onion(key),
        Addr::I2p(hash) => Subnet::I2p(hash),
        Addr::Cjdns(addr) => Subnet::Cjdns(addr),
    }
}

/// `ParseUInt8`: an optional `+`, then decimal digits for a value up to 255.
fn parse_u8(text: &[u8]) -> Option<u8> {
    if text.starts_with(b"+-") {
        return None;
    }
    let digits = text.strip_prefix(b"+").unwrap_or(text);
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut value = 0u32;
    for d in digits {
        value = (value * 10 + u32::from(d - b'0')).min(256);
    }
    u8::try_from(value).ok()
}

/// `LookupSubNet`: the subnet `text` names, if Core takes it as valid.
///
/// `cjdns_reachable` is `-cjdnsreachable` (default off): `MaybeFlipIPv6toCJDNS` then reads an
/// `fc00::/8` address as CJDNS.
// Core: netbase.cpp LookupSubNet
pub fn lookup_subnet(text: &[u8], cjdns_reachable: bool) -> Option<Subnet> {
    if text.contains(&0) {
        return None;
    }
    let (host, netmask) = match text.iter().rposition(|&b| b == b'/') {
        Some(slash) => (&text[..slash], Some(&text[slash + 1..])),
        None => (text, None),
    };
    let mut addr = lookup_host(host)?;
    if cjdns_reachable
        && let Addr::V6(octets) = addr
        && octets[0] == 0xfc
    {
        addr = Addr::Cjdns(octets);
    }
    let Some(netmask) = netmask else {
        return Some(subnet_of_host(addr));
    };
    match parse_u8(netmask) {
        Some(bits) => subnet_with_prefix(addr, bits),
        None => subnet_with_mask(addr, lookup_host(netmask)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn sha3_256_known_answers() {
        assert_eq!(
            hex(&sha3_256(b"")),
            "a7ffc6f8bf1ed76651c14756a061d662f580ff4de43b49fa82d80a4b80f8434a"
        );
        assert_eq!(
            hex(&sha3_256(b"abc")),
            "3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"
        );
        // Longer than one rate block (136 bytes), and exactly one block (python hashlib).
        assert_eq!(
            hex(&sha3_256(&[0xa3; 200])),
            "79f38adec5c20307a98ef76e8324afbfd46cfd81b22e3973c65fa1bd9de31787"
        );
        assert_eq!(
            hex(&sha3_256(&[0x00; 136])),
            "e772c9cf9eb9c991cdfcf125001b454fdbc0a95f188d1b4c844aa032ad6e075e"
        );
    }

    #[test]
    fn inet_aton_forms() {
        assert_eq!(inet_aton_exact(b"1.2.3.4"), Some([1, 2, 3, 4]));
        assert_eq!(inet_aton_exact(b"1.2.3"), Some([1, 2, 0, 3]));
        assert_eq!(inet_aton_exact(b"1.2"), Some([1, 0, 0, 2]));
        assert_eq!(inet_aton_exact(b"0x7f000001"), Some([127, 0, 0, 1]));
        assert_eq!(inet_aton_exact(b"0177.1"), Some([127, 0, 0, 1]));
        assert_eq!(inet_aton_exact(b"300"), Some([0, 0, 1, 44]));
        assert_eq!(inet_aton_exact(b"4294967295"), Some([255; 4]));
        for bad in [
            &b""[..],
            b"1.2.3.4.5",
            b"1.2.3.",
            b".1",
            b"256.1.1.1",
            b"1.2.3.256",
            b"08",
            b"1.2.3.4 ",
            b"a",
        ] {
            assert_eq!(inet_aton_exact(bad), None, "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn subnets() {
        let v4 = |n: [u8; 4], m: [u8; 4]| {
            Some(Subnet::V4 {
                network: n,
                mask: m,
            })
        };
        assert_eq!(lookup_subnet(b"1.2.3.4", false), v4([1, 2, 3, 4], [255; 4]));
        assert_eq!(lookup_subnet(b"1.2.3.4/16", false), v4([1, 2, 0, 0], [255, 255, 0, 0]));
        assert_eq!(
            lookup_subnet(b"1.2.3.4/255.255.0.0", false),
            v4([1, 2, 0, 0], [255, 255, 0, 0])
        );
        assert_eq!(lookup_subnet(b"1.2.3.4/+8", false), v4([1, 0, 0, 0], [255, 0, 0, 0]));
        assert_eq!(lookup_subnet(b"1.2.3.4/33", false), None);
        assert_eq!(lookup_subnet(b"1.2.3.4/255.0.255.0", false), None);
        assert_eq!(lookup_subnet(b"1.2.3.4/", false), None);
        assert_eq!(lookup_subnet(b"", false), None);
        assert_eq!(lookup_subnet(b"nonsense", false), None);
        assert_eq!(lookup_subnet(b"1.2.3.4\0", false), None);
        // IPv4-in-IPv6 is IPv4; its mask is checked as IPv4.
        assert_eq!(lookup_subnet(b"::ffff:1.2.3.4", false), v4([1, 2, 3, 4], [255; 4]));
        assert_eq!(lookup_subnet(b"::ffff:1.2.3.4/120", false), None);
        // The split at `/` comes first, then `LookupHost` strips the brackets.
        assert_eq!(lookup_subnet(b"[1.2.3.4]/32", false), v4([1, 2, 3, 4], [255; 4]));
        assert_eq!(lookup_subnet(b"[1.2.3.4]", false), v4([1, 2, 3, 4], [255; 4]));
        // The internal prefix is refused; the TORv2 prefix is the unspecified IPv6 address.
        assert_eq!(lookup_subnet(b"fd6b:88c0:8724::1", false), None);
        assert_eq!(
            lookup_subnet(b"fd87:d87e:eb43::1/64", false),
            Some(Subnet::V6 {
                network: [0; 16],
                mask: [0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0, 0, 0, 0, 0]
            })
        );
        // Documentation prefixes are valid subnets (only `CNetAddr::IsValid` rejects them).
        assert!(lookup_subnet(b"2001:db8::/32", false).is_some());
        // A scope id is checked and dropped: the same host with any valid scope is one subnet.
        assert_eq!(lookup_subnet(b"fe80::1%7", false), lookup_subnet(b"fe80::1", false));
        assert!(lookup_subnet(b"fe80::1%7", false).is_some());
        assert!(lookup_subnet(b"fe80::1%no-such-interface0", false).is_none());
        assert!(lookup_subnet(b"2001:db8::1%4294967296", false).is_none());
        assert!(lookup_subnet(b"2001:db8::1%", false).is_none());
        // fc00::/8 is plain IPv6 unless CJDNS is reachable, then a single CJDNS host.
        assert!(matches!(lookup_subnet(b"fc00::1/8", false), Some(Subnet::V6 { .. })));
        assert!(matches!(lookup_subnet(b"fc00::1", true), Some(Subnet::Cjdns(_))));
        assert_eq!(lookup_subnet(b"fc00::1/8", true), None);
    }

    #[test]
    fn onion_and_i2p() {
        let onion = b"pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion";
        assert!(matches!(lookup_subnet(onion, false), Some(Subnet::Onion(_))));
        // The base32 part is case-insensitive; the `.onion` suffix match is not (as in Core).
        let mut upper_name = onion.to_vec();
        upper_name[..56].make_ascii_uppercase();
        assert!(matches!(lookup_subnet(&upper_name, false), Some(Subnet::Onion(_))));
        assert_eq!(lookup_subnet(&onion.to_ascii_uppercase(), false), None);
        // A wrong checksum, a v2-length name and a mask are refused.
        let mut bad = onion.to_vec();
        bad[10] = if bad[10] == b'a' {
            b'b'
        } else {
            b'a'
        };
        assert_eq!(lookup_subnet(&bad, false), None);
        assert_eq!(lookup_subnet(b"expyuzz4wqqyqhjn.onion", false), None);
        let mut masked = onion.to_vec();
        masked.extend_from_slice(b"/24");
        assert_eq!(lookup_subnet(&masked, false), None);
        let i2p = b"ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkdq.b32.i2p";
        assert!(matches!(lookup_subnet(i2p, false), Some(Subnet::I2p(_))));
        assert!(matches!(lookup_subnet(&i2p.to_ascii_uppercase(), false), Some(Subnet::I2p(_))));
        assert_eq!(
            lookup_subnet(b"ukeu3k5oycgaauneqgtnvselmt4yemvoilkln7jpvamvfx7dnkd.b32.i2p", false),
            None
        );
    }
}
