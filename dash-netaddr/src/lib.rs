//! Core's network addresses and the resolver-free name helpers, shared by dash-p2p and the node's
//! NetInfo rules (EV1 D28): `CNetAddr`, `CService`, `CSubNet` with Core's predicates and
//! `ToStringAddr[Port]`, `SplitHostPort`, `IsBadPort`, the numeric `Lookup`, and
//! `MaybeFlipIPv6toCJDNS` with CJDNS reachability as a parameter.
//!
//! Pure (layer P): no I/O, clocks, threads or globals. Every ported function carries a `// Core:`
//! anchor into Dash Core v24.0.0-rc.3, the sole parity reference (DEC-82). dash-p2p re-exports
//! everything here under its old paths.

#![forbid(unsafe_code)]

pub mod netaddr;
mod netbase;
mod numeric;

pub use netaddr::{NetAddr, Network, Service, SubNet};
pub use netbase::{
    I2P_SAM31_PORT, is_bad_port, lookup, lookup_host_numeric, lookup_numeric,
    maybe_flip_ipv6_to_cjdns, parse_u16, split_host_port, split_host_port_valid,
};
pub use numeric::getaddrinfo_numeric;
