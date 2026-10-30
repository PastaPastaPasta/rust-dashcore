//! Core's network addresses (P2a §3.7.3): `CNetAddr`, `CService` and `CSubNet` live in
//! `dash-netaddr` (EV1 D28) and are re-exported here under their old paths; `CAddress` adds the
//! P2P fields.

pub use dash_netaddr::netaddr::*;

use dash_p2p_core::ServiceFlags;

/// Core's `CAddress`: a service with the services and time its source claimed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Address {
    /// The address and port.
    pub service: Service,
    /// `nServices`.
    pub services: ServiceFlags,
    /// `nTime`: seconds since the epoch.
    pub time: u32,
}
