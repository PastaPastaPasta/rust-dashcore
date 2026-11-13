//! `netbase.cpp`'s numeric lookups, never resolving a host name: `LookupHost(name, false)`,
//! `Lookup(name, port, false)` and `LookupSubNet`, as `-whitebind`, `-whitelist`, `setban` and
//! the ban list's load read addresses and subnets (P4a D3, D17), and the numeric host decode
//! every `LookupIntern` makes without a DNS lookup: the platform's own `getaddrinfo` with
//! `AI_NUMERICHOST`, as Core's `WrappedGetAddrInfo` (P4a D18, D20).

use crate::netaddr::{NetAddr, Service, SubNet};
use crate::netcore::split_host_port;
use crate::seeds::{Lookup, pre_lookup};

/// Core's `WrappedGetAddrInfo(name, false)`, first address: the platform's `getaddrinfo` with
/// `AI_NUMERICHOST` for a TCP stream socket, so a host decodes exactly as Core's does on the
/// same libc (glibc's `inet_aton` forms such as `127.1`, `0x7f.1` and octal `011.1`; an IPv6
/// address with a `%` scope id, by number or interface name, kept as the address's scope id,
/// Core's `CNetAddr(sin6_addr, sin6_scope_id)`; D23). Off Unix the standard library's IP
/// literals stand in (D20).
// Core: netbase.cpp:44-95 WrappedGetAddrInfo
#[cfg(unix)]
#[allow(unsafe_code)] // DEC-246
pub fn numeric_host(name: &str) -> Option<NetAddr> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: an all-zero `addrinfo` is a valid hint (null pointers, zero fields).
    let mut hint: libc::addrinfo = unsafe { std::mem::zeroed() };
    hint.ai_family = libc::AF_UNSPEC;
    hint.ai_socktype = libc::SOCK_STREAM;
    hint.ai_protocol = libc::IPPROTO_TCP;
    hint.ai_flags = libc::AI_NUMERICHOST;
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated, `hint` is initialized and `res` receives the list.
    if unsafe { libc::getaddrinfo(name.as_ptr(), std::ptr::null(), &hint, &mut res) } != 0 {
        return None;
    }
    if res.is_null() {
        return None;
    }
    let mut found = None;
    let mut cur = res;
    while found.is_none() && !cur.is_null() {
        // SAFETY: `cur` is a node of the list `getaddrinfo` returned, not yet freed.
        let ai = unsafe { &*cur };
        let len = usize::try_from(ai.ai_addrlen).unwrap_or(0);
        if ai.ai_addr.is_null() {
            // Not a libc's answer; skipped rather than read.
        } else if ai.ai_family == libc::AF_INET && len >= std::mem::size_of::<libc::sockaddr_in>() {
            // SAFETY: an `AF_INET` entry's address is a `sockaddr_in` of the checked length;
            // read unaligned so nothing is assumed of the allocation.
            let sin = unsafe { ai.ai_addr.cast::<libc::sockaddr_in>().read_unaligned() };
            found = Some(NetAddr::ipv4(sin.sin_addr.s_addr.to_ne_bytes()));
        } else if ai.ai_family == libc::AF_INET6 && len >= std::mem::size_of::<libc::sockaddr_in6>()
        {
            // SAFETY: an `AF_INET6` entry's address is a `sockaddr_in6` of the checked length.
            let sin6 = unsafe { ai.ai_addr.cast::<libc::sockaddr_in6>().read_unaligned() };
            // Core: netbase.cpp:88 emplace_back(s6->sin6_addr, s6->sin6_scope_id)
            found = Some(
                NetAddr::from_legacy_ipv6(sin6.sin6_addr.s6_addr).with_scope_id(sin6.sin6_scope_id),
            );
        }
        cur = ai.ai_next;
    }
    // SAFETY: `res` is the non-null list this call's `getaddrinfo` returned, freed once.
    unsafe { libc::freeaddrinfo(res) };
    found
}

/// [`numeric_host`] off Unix: the standard library's IP literals (D20).
#[cfg(not(unix))]
pub fn numeric_host(name: &str) -> Option<NetAddr> {
    name.parse::<std::net::IpAddr>().ok().map(NetAddr::from_ip)
}

/// Core's `LookupIntern(name, 1, fAllowLookup = false)`: a Tor or I2P name or a numeric host
/// ([`numeric_host`]), never an internal address; a host name does not resolve.
// Core: netbase.cpp:147-174
fn lookup_intern(name: &str) -> Option<NetAddr> {
    match pre_lookup(name) {
        Lookup::Addr(addr) => Some(addr),
        Lookup::Refused | Lookup::Name => None,
    }
}

/// Core's `Lookup(name, 0, false)`: `SplitHostPort`, then the numeric host.
// Core: netbase.cpp:194-217 (`Lookup` over `LookupIntern`)
pub fn lookup(name: &str) -> Option<Service> {
    lookup_port(name, 0)
}

/// Core's `Lookup(name, port_default, false)`: [`lookup`] with `port_default` where `name`
/// has no port (`-bind`).
// Core: netbase.cpp:194-217
pub fn lookup_port(name: &str, port_default: u16) -> Option<Service> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let (host, port) = split_host_port(name, port_default);
    lookup_intern(host).map(|addr| Service::new(addr, port))
}

/// Core's `LookupHost(name, false)`: brackets around the whole name are dropped, then the
/// numeric host.
// Core: netbase.cpp:176-192
pub fn lookup_host(name: &str) -> Option<NetAddr> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let host = name.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(name);
    lookup_intern(host)
}

/// Core's `LookupSubNet`: `addr`, `addr/prefix` or `addr/netmask` (split at the last `/`), the
/// address numeric and flipped to CJDNS when CJDNS is reachable; `None` where Core's subnet is
/// invalid.
// Core: netbase.cpp:760-794
pub fn lookup_subnet(text: &str, cjdns_reachable: bool) -> Option<SubNet> {
    if text.contains('\0') {
        return None;
    }
    let (addr, mask) = match text.rfind('/') {
        Some(slash) => (&text[..slash], Some(&text[slash + 1..])),
        None => (text, None),
    };
    let addr = lookup_host(addr)?.maybe_flip_to_cjdns(cjdns_reachable);
    let subnet = match mask {
        None => SubNet::single(addr),
        Some(mask) => match parse_u8(mask) {
            Some(bits) => SubNet::with_prefix(addr, bits),
            None => with_netmask(addr, &lookup_host(mask)?)?,
        },
    };
    subnet.is_valid().then_some(subnet)
}

/// Core's `CSubNet(addr, mask)`: IPv4 or IPv6, the mask on the same network, its 1-bits
/// contiguous from the top; equal to the prefix form with that many bits.
// Core: netaddress.cpp:966-995
fn with_netmask(addr: NetAddr, mask: &NetAddr) -> Option<SubNet> {
    if !(addr.is_ipv4() || addr.is_ipv6()) || addr.net() != mask.net() {
        return None;
    }
    // Core's `NetmaskBits` per byte (0x00, 0x80, …, 0xFF) and no 1-bit after a 0-bit.
    let mut ones = 0u32;
    let mut zeros_found = false;
    for &b in mask.bytes() {
        let bits = b.leading_ones();
        if bits + b.trailing_zeros() != 8 || (zeros_found && bits != 0) {
            return None;
        }
        zeros_found |= bits < 8;
        ones += bits;
    }
    Some(SubNet::with_prefix(addr, u8::try_from(ones).expect("at most 128")))
}

/// Core's `ParseUInt8`: the whole text is the number; one leading `+` is allowed.
// Core: util/strencodings.h ParseIntegral
fn parse_u8(s: &str) -> Option<u8> {
    if s.starts_with("+-") {
        return None;
    }
    let digits = s.strip_prefix('+').unwrap_or(s);
    if !digits.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netaddr::Network;

    fn v4(s: &str) -> NetAddr {
        NetAddr::from_ip(s.parse().unwrap())
    }

    /// `LookupSubNet`'s three forms, Core's contiguity rule and the CJDNS flip.
    #[test]
    fn lookup_subnet_is_cores() {
        assert_eq!(lookup_subnet("1.2.3.4/24", false).unwrap().to_string(), "1.2.3.0/24");
        assert_eq!(lookup_subnet("1.2.3.4/+16", false).unwrap().to_string(), "1.2.0.0/16");
        assert_eq!(lookup_subnet("1.2.3.4/255.255.0.0", false).unwrap().to_string(), "1.2.0.0/16");
        assert!(lookup_subnet("1.2.3.4/255.0.255.0", false).is_none());
        assert!(lookup_subnet("1.2.3.4/33", false).is_none());
        assert!(lookup_subnet("1.2.3.4/++1", false).is_none());
        assert!(lookup_subnet("1.2.3.4/ffff::", false).is_none());
        assert!(lookup_subnet("example.com", false).is_none());
        assert!(lookup_subnet("", false).is_none());
        assert_eq!(lookup_subnet("[::1]", false).unwrap().to_string(), "::1/128");
        assert_eq!(lookup_subnet("2001:db8::1/32", false).unwrap().to_string(), "2001:db8::/32");
        // The last `/` splits, as `find_last_of`.
        assert!(lookup_subnet("1.2.3.4/8/8", false).is_none());
        let cjdns = lookup_subnet("fc00::5", true).unwrap();
        assert_eq!(cjdns.network().net(), Network::Cjdns);
        assert!(lookup_subnet("fc00::5/64", true).is_none());
        assert_eq!(lookup_subnet("fc00::5/64", false).unwrap().to_string(), "fc00::/64");
        assert!(lookup_subnet("1.2.3.4", false).unwrap().matches(&v4("1.2.3.4")));
    }

    /// The numeric forms glibc's `getaddrinfo(AI_NUMERICHOST)` takes beyond `inet_pton`, each
    /// as Core reads it on this libc, and the ones it refuses (Sol P4a r1 P2; D18, D20).
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn numeric_hosts_are_getaddrinfos() {
        assert_eq!(lookup("127.1:9999"), Some(Service::new(v4("127.0.0.1"), 9999)));
        assert_eq!(lookup_subnet("11.1", false).unwrap().to_string(), "11.0.0.1/32");
        assert_eq!(lookup_host("0x7f.1"), Some(v4("127.0.0.1")));
        assert_eq!(lookup_host("011.1"), Some(v4("9.0.0.1")));
        assert_eq!(lookup_host("4294967040"), Some(v4("255.255.255.0")));
        assert_eq!(lookup_host("::ffff:1.2.3.4"), Some(v4("1.2.3.4")));
        // A scope id by interface name or number, kept as the scope id (D23).
        let link_local = NetAddr::from_ip("fe80::1".parse().unwrap());
        assert_eq!(lookup_host("fe80::1%lo"), Some(link_local.clone()));
        assert_eq!(lookup_host("[fe80::1%1]"), Some(link_local));
        assert_eq!(lookup_host("[fe80::1%7]").unwrap().scope_id(), 7);
        assert_eq!(lookup_host("fe80::1").unwrap().scope_id(), 0);
        // DEC-249: this one test-only call is allowed; `numeric_host` stays the crate's only
        // non-test `unsafe` item.
        // SAFETY: a NUL-terminated interface name.
        #[allow(unsafe_code)]
        let lo = unsafe { libc::if_nametoindex(c"lo".as_ptr()) };
        assert_ne!(lo, 0);
        let service = lookup("[fe80::1%lo]:9999").unwrap();
        match service.to_socket_addr() {
            Some(std::net::SocketAddr::V6(sa)) => assert_eq!(sa.scope_id(), lo),
            other => panic!("{other:?}"),
        }
        assert_eq!(service.to_string(), format!("[fe80::1%{lo}]:9999"));
        // The subnet keeps it, prints `%N`, and its text reads back with it (banlist.json).
        let net = lookup_subnet("fe80::1%lo", false).unwrap();
        assert_eq!(net.to_string(), format!("fe80::1%{lo}/128"));
        let back = lookup_subnet(&net.to_string(), false).unwrap();
        assert_eq!((back.network().scope_id(), back.to_string()), (lo, net.to_string()));
        // A host name is never looked up; nor is a form glibc refuses.
        for refused in [
            "localhost",
            "1.2.3.4.",
            "1.2.3.4 x",
            " 1.2.3.4",
            "127.0.0.256",
            "1.2.3.4.5",
            "08.1",
            "fe80::1%",
            "fe80::1%no-such-interface0",
        ] {
            assert_eq!(lookup_host(refused), None, "{refused:?}");
        }
        // An internal address never resolves (`LookupIntern`, netbase.cpp:167-169).
        assert_eq!(lookup_host("fd6b:88c0:8724:1:2:3:4:5"), None);
    }
}
