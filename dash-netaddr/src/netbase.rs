//! Core's name and port helpers that need no resolver (v24 `netbase.cpp`,
//! `util/strencodings.cpp`): `SplitHostPort`, `IsBadPort`, the numeric `Lookup` and
//! `MaybeFlipIPv6toCJDNS`. Core's globals are parameters: the default port comes from the caller
//! and `g_reachable_nets.Contains(NET_CJDNS)` is `cjdns_reachable`.

use crate::netaddr::{NetAddr, Service};
use crate::numeric::getaddrinfo_numeric;

/// Core's `I2P_SAM31_PORT`: SAM 3.1 has no ports, so I2P services carry port 0.
// Core: netaddress.h:113
pub const I2P_SAM31_PORT: u16 = 0;

/// Core's `SplitHostPort`: the last `:` splits off a port when it follows `[…]` or is the only
/// one; a port that does not parse leaves the input whole (an unparseable host); then one pair of
/// enclosing brackets is removed. Core's validity result is [`split_host_port_valid`].
// Core: util/strencodings.cpp:101-126 SplitHostPort
pub fn split_host_port(dest: &str, default_port: u16) -> (&str, u16) {
    let mut rest = dest;
    let mut port = default_port;
    if let Some(colon) = port_colon(dest)
        && let Some(n) = parse_u16(&dest[colon + 1..])
    {
        rest = &dest[..colon];
        port = n;
    }
    let host = rest.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(rest);
    (host, port)
}

/// `SplitHostPort`'s result: false when the input has a port separator and the port does not
/// parse or is 0, true when it has none.
// Core: util/strencodings.cpp:101-126 SplitHostPort
pub fn split_host_port_valid(dest: &str) -> bool {
    port_colon(dest).is_none_or(|colon| parse_u16(&dest[colon + 1..]).is_some_and(|n| n != 0))
}

/// The `:` that `SplitHostPort` reads a port after: the last one, when it follows `[…]` or is
/// the only one.
fn port_colon(dest: &str) -> Option<usize> {
    let colon = dest.rfind(':')?;
    let b = dest.as_bytes();
    // `b[0] == '['` implies `colon != 0`.
    let bracketed = b[0] == b'[' && b[colon - 1] == b']';
    let multi_colon = colon != 0 && dest[..colon].contains(':');
    (colon == 0 || bracketed || !multi_colon).then_some(colon)
}

/// Core's `ParseUInt16`: decimal digits only after at most one leading `+` (and never `+-`),
/// in range.
// Core: util/strencodings.cpp:232-271 ParseIntegral, ParseUInt16
pub fn parse_u16(s: &str) -> Option<u16> {
    let digits = s.strip_prefix('+').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Core's `IsBadPort`.
// Core: netbase.cpp:801-832 IsBadPort
pub fn is_bad_port(port: u16) -> bool {
    (port > 0 && port <= 1023)
        || matches!(
            port,
            1719 | 1720 | 1723 | 2049 | 3659 | 4045 | 5060 | 5061 | 6000 | 6566 | 6665
                ..=6669 | 6697 | 8332 | 8333 | 10080 | 18332 | 18333
        )
}

/// Core's `LookupIntern(host, 1, fAllowLookup = false)`: `None` for a name with a NUL, else a
/// Tor v3 or I2P name (`SetSpecial`), else the numeric form, except an internal address ("Never
/// allow resolving to an internal address").
// Core: netbase.cpp:147-169 LookupIntern
pub fn lookup_host_numeric(host: &str) -> Option<NetAddr> {
    if host.contains('\0') {
        return None;
    }
    if let Some(addr) = NetAddr::set_special(host) {
        return Some(addr);
    }
    getaddrinfo_numeric(host).filter(|addr| !addr.is_internal())
}

/// Core's `Lookup(name, port_default, fAllowLookup = false)`: `SplitHostPort`, then
/// [`lookup_host_numeric`] on the host with the port it gave. The address may be invalid (`::`
/// is returned as such); `None` when nothing decodes.
// Core: netbase.cpp:194-217 Lookup
pub fn lookup(name: &str, port_default: u16) -> Option<Service> {
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    let (host, port) = split_host_port(name, port_default);
    lookup_host_numeric(host).map(|addr| Service::new(addr, port))
}

/// Core's `LookupNumeric(name, port_default)`: [`lookup`], or the default (invalid) service. No
/// CJDNS flip: the callers that want one apply [`maybe_flip_ipv6_to_cjdns`].
// Core: netbase.cpp:219-227 LookupNumeric
pub fn lookup_numeric(name: &str, port_default: u16) -> Service {
    lookup(name, port_default).unwrap_or_default()
}

/// Core's `MaybeFlipIPv6toCJDNS`: an IPv6 address in `fc00::/8` is CJDNS when CJDNS is reachable
/// (`g_reachable_nets.Contains(NET_CJDNS)`, here `cjdns_reachable`).
// Core: netbase.cpp:833-840 MaybeFlipIPv6toCJDNS
pub fn maybe_flip_ipv6_to_cjdns(s: Service, cjdns_reachable: bool) -> Service {
    Service::new(s.addr.maybe_flip_to_cjdns(cjdns_reachable), s.port)
}
