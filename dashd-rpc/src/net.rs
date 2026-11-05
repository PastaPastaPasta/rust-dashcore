//! The address handling of `-rpcallowip` and `-rpcbind` (`netbase.cpp` `LookupSubNet`,
//! `SplitHostPort`; `netaddress.cpp` `CSubNet`).
//!
//! Name resolution is the node's (`dashd-node`'s `getaddrinfo` with Core's hints), passed in as
//! a [`Resolve`] function.

use std::fmt;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Resolves a host as Core's `LookupHost(name, allow_lookup)`: the first address, or `None`.
/// Without `allow_lookup` only numeric hosts resolve.
pub type Resolve = dyn Fn(&str, bool) -> Option<IpAddr> + Send + Sync;

/// A numeric resolver: what `getaddrinfo` with `AI_NUMERICHOST` accepts in its common forms.
pub fn resolve_numeric(name: &str, _allow_lookup: bool) -> Option<IpAddr> {
    name.parse().ok()
}

/// An IPv4-mapped IPv6 address is the IPv4 address (Core's `SetLegacyIPv6`).
pub fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    }
}

/// `CService::ToStringAddrPort`.
pub fn addr_port_string(addr: &SocketAddr) -> String {
    match canonical(addr.ip()) {
        IpAddr::V4(v4) => format!("{v4}:{}", addr.port()),
        IpAddr::V6(v6) => format!("[{v6}]:{}", addr.port()),
    }
}

/// Core's `CSubNet` for IPv4 and IPv6 networks: an address and a netmask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subnet {
    network: IpAddr,
    mask: Vec<u8>,
}

impl Subnet {
    /// `CSubNet(addr)`: exactly `addr`.
    pub fn single(addr: IpAddr) -> Subnet {
        let addr = canonical(addr);
        Subnet {
            network: addr,
            mask: vec![0xff; octets(&addr).len()],
        }
    }

    /// `CSubNet(addr, prefix)`: `None` when the prefix exceeds the address's width.
    pub fn with_prefix(addr: IpAddr, prefix: u8) -> Option<Subnet> {
        let addr = canonical(addr);
        let width = octets(&addr).len();
        if usize::from(prefix) > width * 8 {
            return None;
        }
        let mut n = prefix;
        let mask = (0..width)
            .map(|_| {
                let bits = n.min(8);
                n -= bits;
                if bits == 0 {
                    0
                } else {
                    0xffu8 << (8 - bits)
                }
            })
            .collect();
        Some(Subnet::normalized(addr, mask))
    }

    /// `CSubNet(addr, mask)`: the mask must be of the same family and contiguous.
    pub fn with_mask(addr: IpAddr, mask: IpAddr) -> Option<Subnet> {
        let (addr, mask) = (canonical(addr), canonical(mask));
        if addr.is_ipv4() != mask.is_ipv4() {
            return None;
        }
        let mask = octets(&mask);
        let mut zeros_found = false;
        for &b in &mask {
            let bits = netmask_bits(b)?;
            if zeros_found && bits != 0 {
                return None;
            }
            zeros_found |= bits < 8;
        }
        Some(Subnet::normalized(addr, mask))
    }

    fn normalized(addr: IpAddr, mask: Vec<u8>) -> Subnet {
        let bytes: Vec<u8> = octets(&addr).iter().zip(&mask).map(|(a, m)| a & m).collect();
        let network = match addr {
            IpAddr::V4(_) => IpAddr::from(<[u8; 4]>::try_from(bytes).expect("4 bytes")),
            IpAddr::V6(_) => IpAddr::from(<[u8; 16]>::try_from(bytes).expect("16 bytes")),
        };
        Subnet {
            network,
            mask,
        }
    }

    /// `CSubNet::Match`.
    pub fn matches(&self, addr: IpAddr) -> bool {
        let addr = canonical(addr);
        addr.is_ipv4() == self.network.is_ipv4()
            && octets(&addr)
                .iter()
                .zip(&self.mask)
                .zip(octets(&self.network))
                .all(|((a, m), n)| a & m == n)
    }
}

/// `CSubNet::ToString`: the network and its prefix length.
impl fmt::Display for Subnet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix: u32 = self
            .mask
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| u32::from(netmask_bits(b).unwrap_or(0)))
            .sum();
        write!(f, "{}/{prefix}", self.network)
    }
}

fn octets(ip: &IpAddr) -> Vec<u8> {
    match ip {
        IpAddr::V4(v4) => v4.octets().to_vec(),
        IpAddr::V6(v6) => v6.octets().to_vec(),
    }
}

/// `NetmaskBits`: the leading one bits of a contiguous mask byte.
fn netmask_bits(b: u8) -> Option<u8> {
    let ones = b.leading_ones() as u8;
    (b.checked_shl(u32::from(ones)).unwrap_or(0) == 0).then_some(ones)
}

/// `LookupHost(name, false)`: brackets around an IPv6 address are dropped.
fn lookup_host(name: &str, resolve: &Resolve) -> Option<IpAddr> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let name = name.strip_prefix('[').and_then(|n| n.strip_suffix(']')).unwrap_or(name);
    resolve(name, false)
}

/// Core's `LookupSubNet`: `addr`, `addr/prefix` or `addr/netmask`.
// Core: netbase.cpp:774-806
pub fn lookup_subnet(text: &str, resolve: &Resolve) -> Option<Subnet> {
    if text.contains('\0') {
        return None;
    }
    let (addr, mask) = match text.rfind('/') {
        Some(slash) => (&text[..slash], Some(&text[slash + 1..])),
        None => (text, None),
    };
    let addr = lookup_host(addr, resolve)?;
    match mask {
        None => Some(Subnet::single(addr)),
        Some(mask) => match parse_integral::<u8>(mask) {
            Some(prefix) => Subnet::with_prefix(addr, prefix),
            None => Subnet::with_mask(addr, lookup_host(mask, resolve)?),
        },
    }
}

/// Core's `ParseUInt8`/`ParseUInt16`: the whole text is the number, a `+` sign allowed.
pub fn parse_integral<T: std::str::FromStr>(s: &str) -> Option<T> {
    if s.contains('\0') || s.starts_with("+-") {
        return None;
    }
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.starts_with('+') {
        return None;
    }
    digits.parse().ok()
}

/// Core's `SplitHostPort`: a trailing `:port` overrides `port` when the colon follows a
/// bracketed address or is the only one; brackets are dropped from the host.
// Core: util/strencodings.cpp SplitHostPort
pub fn split_host_port(input: &str, port: u16) -> (String, u16) {
    let mut port = port;
    let mut host = input;
    if let Some(colon) = input.rfind(':') {
        let bracketed = input.starts_with('[') && colon > 0 && input.as_bytes()[colon - 1] == b']';
        let multi_colon = colon != 0 && input[..colon].contains(':');
        if (colon == 0 || bracketed || !multi_colon)
            && let Some(n) = parse_integral::<u16>(&input[colon + 1..])
        {
            host = &input[..colon];
            port = n;
        }
    }
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    (host.to_owned(), port)
}

/// Whether `addr` is a wildcard address (`CNetAddr::IsBindAny`).
pub fn is_bind_any(addr: IpAddr) -> bool {
    match canonical(addr) {
        IpAddr::V4(v4) => v4.is_unspecified(),
        IpAddr::V6(v6) => v6 == Ipv6Addr::UNSPECIFIED,
    }
}

#[cfg(test)]
mod tests;
