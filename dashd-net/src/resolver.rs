//! The host's DNS resolver behind SB6's [`Resolver`]: Core's `LookupHost` over its
//! `g_dns_lookup` (`netbase.cpp` `WrappedGetAddrInfo`, the system's `getaddrinfo`).
//!
//! Core's `SetSpecial` turns `.onion` and `.i2p` names into addresses instead of looking them
//! up. SB6 cannot reach those networks (no proxy), so they resolve to nothing here, and never go
//! to DNS (D12).
//!
//! Whether a call may look up is the caller's (`fAllowLookup`): SB6 passes `true` for the
//! DNS-seed thread (`LookupHost(host, 32, true)`, `net.cpp:2775`) and `NetConfig::name_lookup`
//! (`-dns`, Core's `fNameLookup`) for named connection targets (D16).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use dash_p2p::{NetAddr, Resolver, Service};

/// Core's `DNSLookupFn`: the addresses `getaddrinfo` returns for `(name, allow_lookup)`, in its
/// order (IPv4 and IPv6 only). Blocking; called on the runtime's blocking pool.
pub type DnsLookupFn = Arc<dyn Fn(&str, bool) -> Vec<NetAddr> + Send + Sync>;

/// `LookupHost` over a [`DnsLookupFn`] ([`wrapped_getaddrinfo`] by default).
#[derive(Clone)]
pub struct SystemResolver {
    /// Core's `g_dns_lookup`.
    pub dns_lookup: DnsLookupFn,
}

impl Default for SystemResolver {
    /// Through the system's `getaddrinfo`.
    fn default() -> Self {
        Self {
            dns_lookup: Arc::new(wrapped_getaddrinfo),
        }
    }
}

impl core::fmt::Debug for SystemResolver {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SystemResolver").finish_non_exhaustive()
    }
}

impl SystemResolver {
    /// Core's `LookupHost(name, max, allow_lookup)`: at most `max` addresses (0: all), never an
    /// internal one.
    // Core: netbase.cpp:142-169 LookupIntern, :181-187 LookupHost
    pub async fn lookup_host(&self, name: &str, max: usize, allow_lookup: bool) -> Vec<NetAddr> {
        // Core: `if (!ContainsNoNUL(name)) return {};`
        if name.is_empty() || name.contains('\0') {
            return Vec::new();
        }
        let host = name.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(name);
        let lower = host.to_ascii_lowercase();
        if lower.ends_with(".onion") || lower.ends_with(".i2p") {
            return Vec::new();
        }
        let lookup = Arc::clone(&self.dns_lookup);
        let host = host.to_owned();
        let found = tokio::task::spawn_blocking(move || lookup(&host, allow_lookup))
            .await
            .unwrap_or_default();
        let limit = if max == 0 {
            usize::MAX
        } else {
            max
        };
        // Never allow resolving to an internal address. Consider any such result invalid.
        found.into_iter().filter(|a| !a.is_internal()).take(limit).collect()
    }
}

impl Resolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        max: usize,
        allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>> {
        Box::pin(self.lookup_host(host, max, allow_lookup))
    }
}

/// Core's `WrappedGetAddrInfo` (`getaddrinfo` for a TCP stream socket, `AI_NUMERICHOST` when
/// not looking up): a numeric host decodes as glibc's `getaddrinfo` decodes it ([`numeric_host`]),
/// anything else is looked up through the standard library's `getaddrinfo` when
/// `allow_lookup` and is no address otherwise. Lookups go without Core's `AI_ADDRCONFIG` (D12).
// Core: netbase.cpp:43-91 WrappedGetAddrInfo
pub fn wrapped_getaddrinfo(name: &str, allow_lookup: bool) -> Vec<NetAddr> {
    use std::net::ToSocketAddrs;
    if let Some(addr) = numeric_host(name) {
        return vec![addr];
    }
    if !allow_lookup {
        return Vec::new();
    }
    match (name, 0).to_socket_addrs() {
        Ok(found) => found.map(|sa| Service::from_socket_addr(sa).addr).collect(),
        Err(_) => Vec::new(),
    }
}

/// What glibc's `getaddrinfo(AI_NUMERICHOST)` decodes: an `inet_aton` form, or an IPv6 address
/// optionally followed by `%` and a numeric scope id, kept as the address's scope id (P4a D23).
/// An interface-name scope id is refused (D17; BL-205).
// glibc: sysdeps/posix/getaddrinfo.c gaih_inet (inet_aton_exact, inet_pton, scope id)
pub fn numeric_host(name: &str) -> Option<NetAddr> {
    if let Some(v4) = inet_aton_exact(name.as_bytes()) {
        return Some(NetAddr::ipv4(v4));
    }
    let (address, scope) = match name.split_once('%') {
        Some((address, scope)) => (address, Some(scope)),
        None => (name, None),
    };
    // Digits only: `u32::from_str` would take `+7`, which glibc refuses.
    let scope_id = match scope {
        None => 0,
        Some(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => s.parse().ok()?,
        Some(_) => return None,
    };
    let v6: std::net::Ipv6Addr = address.parse().ok()?;
    Some(NetAddr::from_legacy_ipv6(v6.octets()).with_scope_id(scope_id))
}

/// glibc's `inet_aton` forms without trailing characters: one to four parts, each decimal, octal
/// (leading `0`) or hex (`0x`); the last part fills the remaining bytes.
// glibc: resolv/inet_addr.c __inet_aton_exact (the port in dash-coreimport's netstate::subnet)
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn literals_and_nul() {
        let r = SystemResolver::default();
        assert_eq!(r.resolve("127.0.0.1", 1, true).await, vec![NetAddr::ipv4([127, 0, 0, 1])]);
        let v6 = vec![NetAddr::from_ip("::1".parse().unwrap())];
        assert_eq!(r.resolve("[::1]", 1, true).await, v6);
        assert!(r.resolve("local\0host", 1, true).await.is_empty());
        assert!(r.resolve("abc.onion", 1, true).await.is_empty());
        // Core's internal range (`fd6b:88c0:8724::/48`) never resolves, even as a literal.
        assert!(r.resolve("fd6b:88c0:8724::1", 1, true).await.is_empty());
        assert!(r.resolve("localhost", 4, false).await.is_empty());
        assert_eq!(r.resolve("127.0.0.1", 1, false).await.len(), 1);
    }

    /// The caller's `allow_lookup` reaches `g_dns_lookup` unchanged, and `max` caps the answer.
    #[tokio::test]
    async fn lookup_permission_is_per_call() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&calls);
        let r = SystemResolver {
            dns_lookup: Arc::new(move |name: &str, allow: bool| {
                log.lock().unwrap().push((name.to_owned(), allow));
                if allow {
                    vec![NetAddr::ipv4([1, 2, 3, 4]), NetAddr::ipv4([5, 6, 7, 8])]
                } else {
                    Vec::new()
                }
            }),
        };
        assert_eq!(r.resolve("x5.dnsseed.example.org", 32, true).await.len(), 2);
        assert_eq!(r.resolve("x5.dnsseed.example.org", 1, true).await.len(), 1);
        assert!(r.resolve("node.example.org", 256, false).await.is_empty());
        let allowed: Vec<bool> = calls.lock().unwrap().iter().map(|c| c.1).collect();
        assert_eq!(allowed, [true, true, false]);
    }
}
