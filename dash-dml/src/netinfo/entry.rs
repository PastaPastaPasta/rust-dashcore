//! Core's `NetInfoEntry` as read from its wire form, `DomainPort`, and the per-entry rules:
//! `ValidateService` for both kinds and `ValidateDomainPort`.

use std::fmt;

use dash_netaddr::{I2P_SAM31_PORT, NetAddr, Service, is_bad_port};
use dash_primitives::{NIEntry, NIPurpose};

use super::{NetInfoRules, NetInfoStatus};

/// Core's `DOMAIN_MAX_LEN`.
// Core: evo/netinfo.h:25
pub const DOMAIN_MAX_LEN: usize = 253;
/// Core's `DOMAIN_MIN_LEN`.
// Core: evo/netinfo.cpp:24
const DOMAIN_MIN_LEN: usize = 3;
/// Core's `DOMAIN_LABEL_MAX_LEN`.
// Core: evo/netinfo.cpp:22
const DOMAIN_LABEL_MAX_LEN: usize = 63;

/// The longest `ToStringAddr()` or `ToStringAddrPort()` NetInfo forms: a maximal domain, or the
/// bracketed IPv6 form, plus `:` and a five-digit port (D15).
pub const NETINFO_ADDR_MAX: usize = DOMAIN_MAX_LEN + 2 + 1 + 5;

// Core: evo/netinfo.cpp:26-42
pub(super) const SAFE_CHARS_ALPHA: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
pub(super) const SAFE_CHARS_IPV4: &str = "1234567890.";
pub(super) const SAFE_CHARS_IPV4_6: &str = "abcdefABCDEF1234567890.:[]";
pub(super) const SAFE_CHARS_RFC1035: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-";
const TLDS_BAD: [&str; 13] = [
    ".mail",
    ".arpa",
    ".example",
    ".invalid",
    ".localhost",
    ".test",
    ".local",
    ".corp",
    ".home",
    ".internal",
    ".intranet",
    ".lan",
    ".private",
];
pub(super) const TLDS_PRIVACY: [&str; 2] = [".i2p", ".onion"];

/// Core's `MatchCharsFilter`: every byte is in `filter` (true for the empty string).
// Core: evo/netinfo.cpp:44-47
pub(super) fn match_chars(input: &str, filter: &str) -> bool {
    input.bytes().all(|c| filter.as_bytes().contains(&c))
}

/// Core's `MatchSuffix`: a non-empty string ending, case-sensitively, with one of `list`.
// Core: evo/netinfo.cpp:49-57
pub(super) fn match_suffix(s: &str, list: &[&str]) -> bool {
    !s.is_empty() && list.iter().any(|suffix| s.ends_with(suffix))
}

/// Core's `IsAllowedPlatformHTTPPort`.
// Core: evo/netinfo.cpp:59-66
fn is_allowed_platform_http_port(port: u16) -> bool {
    port == 443
}

/// `ToStringAddr()` / `ToStringAddrPort()` storage: [`NETINFO_ADDR_MAX`] bytes on the stack.
/// Writing past the end is an error, never a truncation (D15, D39).
#[derive(Clone)]
pub struct AddrStr {
    len: usize,
    buf: [u8; NETINFO_ADDR_MAX],
}

impl AddrStr {
    /// The string written so far.
    pub fn as_str(&self) -> &str {
        // Only whole `&str`s are appended (`write_str`), so the prefix is UTF-8.
        std::str::from_utf8(&self.buf[..self.len]).unwrap_or_default()
    }

    /// `value`'s `Display` form, or `None` when it does not fit.
    pub fn format(value: impl fmt::Display) -> Option<Self> {
        let mut s = Self::default();
        fmt::Write::write_fmt(&mut s, format_args!("{value}")).ok()?;
        Some(s)
    }
}

impl Default for AddrStr {
    fn default() -> Self {
        Self {
            len: 0,
            buf: [0; NETINFO_ADDR_MAX],
        }
    }
}

impl fmt::Write for AddrStr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len.checked_add(s.len()).filter(|&end| end <= NETINFO_ADDR_MAX);
        let end = end.ok_or(fmt::Error)?;
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

impl PartialEq for AddrStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for AddrStr {}

impl fmt::Debug for AddrStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

/// Core's `NetInfoEntry` as Core reads its wire form: a valid service, a domain that passes
/// `DomainPort::Validate`, or the invalid entry. The derived order is not Core's, but its
/// equality is Core's `operator==` (and so the `std::set` equivalence).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Entry<'a> {
    Invalid,
    Service(Service),
    Domain(&'a str, u16),
}

impl<'a> Entry<'a> {
    // Core: evo/netinfo.h:243-263 NetInfoEntry::Unserialize
    pub(crate) fn read(entry: &'a NIEntry) -> Self {
        match entry {
            NIEntry::Service(s) => {
                let service = Service::new(NetAddr::from_addrv2(&s.addr), s.port);
                if service.addr.is_valid() {
                    Entry::Service(service)
                } else {
                    Entry::Invalid
                }
            }
            NIEntry::Domain {
                name,
                port,
            } => match std::str::from_utf8(name) {
                Ok(name) if domain_validate(name, *port) == DomainStatus::Success => {
                    Entry::Domain(name, *port)
                }
                _ => Entry::Invalid,
            },
            NIEntry::Invalid => Entry::Invalid,
        }
    }

    /// Core's `NetInfoEntry::ToStringAddr()`, whole.
    // Core: evo/netinfo.cpp:251-262
    pub(crate) fn to_string_addr(&self) -> AddrStr {
        let s = match self {
            Entry::Invalid => AddrStr::format("[invalid entry]"),
            Entry::Service(s) => AddrStr::format(&s.addr),
            Entry::Domain(name, _) => AddrStr::format(name),
        };
        s.expect("every ToStringAddr form fits NETINFO_ADDR_MAX (tested)")
    }

    /// Core's `NetInfoEntry::ToStringAddrPort()`, whole.
    // Core: evo/netinfo.cpp:264-275
    pub(crate) fn to_string_addr_port(&self) -> AddrStr {
        let s = match self {
            Entry::Invalid => AddrStr::format("[invalid entry]"),
            Entry::Service(s) => AddrStr::format(s),
            Entry::Domain(name, port) => AddrStr::format(format_args!("{name}:{port}")),
        };
        s.expect("every ToStringAddrPort form fits NETINFO_ADDR_MAX (tested)")
    }
}

/// Core's `DomainPort::Status`.
// Core: evo/netinfo.h:120-131
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainStatus {
    BadLen,
    BadChar,
    BadCharPos,
    BadDotless,
    BadLabelLen,
    BadLabelCharPos,
    BadPort,
    Malformed,
    Success,
}

/// Core's `DomainPort::ValidateDomain`.
// Core: evo/netinfo.cpp:90-114
pub(crate) fn validate_domain(addr: &str) -> DomainStatus {
    if addr.len() > DOMAIN_MAX_LEN || addr.len() < DOMAIN_MIN_LEN {
        return DomainStatus::BadLen;
    }
    if !match_chars(addr, SAFE_CHARS_RFC1035) {
        return DomainStatus::BadChar;
    }
    if addr.starts_with('.') || addr.ends_with('.') {
        return DomainStatus::BadCharPos;
    }
    if !addr.contains('.') {
        return DomainStatus::BadDotless;
    }
    for label in addr.split('.') {
        if label.is_empty() || label.len() > DOMAIN_LABEL_MAX_LEN {
            return DomainStatus::BadLabelLen;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return DomainStatus::BadLabelCharPos;
        }
    }
    DomainStatus::Success
}

/// Core's `DomainPort::Validate` on a stored domain and port.
// Core: evo/netinfo.cpp:130-139
pub(crate) fn domain_validate(addr: &str, port: u16) -> DomainStatus {
    if addr.is_empty() || addr.bytes().any(|c| c.is_ascii_uppercase()) {
        return DomainStatus::Malformed;
    }
    if port == 0 {
        return DomainStatus::BadPort;
    }
    validate_domain(addr)
}

/// Core's `MnNetInfo::ValidateService`.
// Core (rc.4): evo/netinfo.cpp:286-305
pub(crate) fn validate_service_mn(service: &Service, rules: &NetInfoRules) -> NetInfoStatus {
    if !service.addr.is_valid() {
        return NetInfoStatus::BadAddress;
    }
    if !service.addr.is_ipv4() {
        return NetInfoStatus::BadType;
    }
    if rules.require_routable && !rules.is_routable(service) {
        return NetInfoStatus::NotRoutable;
    }
    // Must use mainnet port on mainnet; must NOT use mainnet port on other networks.
    if rules.on_mainnet != (service.port == rules.main_default_port) {
        return NetInfoStatus::BadPort;
    }
    NetInfoStatus::Success
}

/// Core's `ExtNetInfo::ValidateService`.
// Core (rc.4): evo/netinfo.cpp:448-474
pub(crate) fn validate_service_ext(
    purpose: NIPurpose,
    service: &Service,
    rules: &NetInfoRules,
) -> NetInfoStatus {
    let addr = &service.addr;
    if !addr.is_valid() {
        return NetInfoStatus::BadAddress;
    }
    if !addr.is_cjdns() && !addr.is_i2p() && !addr.is_ipv4() && !addr.is_ipv6() && !addr.is_tor() {
        return NetInfoStatus::BadType;
    }
    if rules.require_routable && !rules.is_routable(service) {
        return NetInfoStatus::NotRoutable;
    }
    let port = service.port;
    if addr.is_i2p() {
        // I2P SAM 3.1 and earlier don't support arbitrary ports.
        if port != I2P_SAM31_PORT {
            return NetInfoStatus::BadPort;
        }
    } else {
        let allowed_https =
            purpose == NIPurpose::PlatformHttps && is_allowed_platform_http_port(port);
        if port == 0 || (is_bad_port(port) && !allowed_https) {
            return NetInfoStatus::BadPort;
        }
    }
    NetInfoStatus::Success
}

/// Core's `ExtNetInfo::ValidateDomainPort`.
// Core: evo/netinfo.cpp:480-498
pub(crate) fn validate_domain_port(addr: &str, port: u16) -> NetInfoStatus {
    if domain_validate(addr, port) != DomainStatus::Success {
        return NetInfoStatus::BadInput;
    }
    if port == 0 || (is_bad_port(port) && !is_allowed_platform_http_port(port)) {
        return NetInfoStatus::BadPort;
    }
    if match_suffix(addr, &TLDS_BAD) || match_suffix(addr, &TLDS_PRIVACY) {
        return NetInfoStatus::BadInput;
    }
    // A valid domain has at least two labels.
    if !addr.rsplit('.').next().is_some_and(|tld| match_chars(tld, SAFE_CHARS_ALPHA)) {
        return NetInfoStatus::BadInput;
    }
    NetInfoStatus::Success
}
