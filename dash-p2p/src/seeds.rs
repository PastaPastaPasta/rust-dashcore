//! DNS and fixed seeds (P2a §3.8.4, §3.8.6).
//!
//! DNS resolution is the host's: a shared crate never calls the system resolver (SB0 L5). The node
//! passes a bounded pool over `getaddrinfo`; SPV its client runtime's blocking pool.

use std::future::Future;
use std::pin::Pin;

use dash_netaddr::I2P_SAM31_PORT;
use dash_p2p_core::ServiceFlags;
use dash_primitives::AddrV2;

use crate::netaddr::{Address, NetAddr, Service};
use crate::rng::FastRandom;

/// A host's DNS resolver.
pub trait Resolver: Send + Sync {
    /// Resolves `host` to at most `max` addresses (Core's `LookupHost(host, max,
    /// allow_lookup)`); with `allow_lookup` false only a numeric `host` resolves. DNS seeds always
    /// pass `true` (net.cpp:2775); a named connection target passes
    /// [`NetConfig::name_lookup`](crate::NetConfig::name_lookup) (`-dns`, net.cpp:405).
    fn resolve<'a>(
        &'a self,
        host: &'a str,
        max: usize,
        allow_lookup: bool,
    ) -> Pin<Box<dyn Future<Output = Vec<NetAddr>> + Send + 'a>>;
}

/// Core's `CChainParams::GetDefaultPort(addr)`: [`I2P_SAM31_PORT`] when the whole string is an
/// I2P name, else the chain's default port.
// Core: chainparams.h:94-102
pub(crate) fn default_port_for(name: &str, default_port: u16) -> u16 {
    match NetAddr::set_special(name) {
        Some(a) if a.is_i2p() => I2P_SAM31_PORT,
        _ => default_port,
    }
}

/// What Core's `LookupIntern` does with a host name before (or instead of) DNS.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Lookup {
    /// A Tor v3 or I2P name (`SetSpecial`), or an IP literal: no resolver call.
    Addr(NetAddr),
    /// No address and no resolver call: an empty name, a NUL or an internal address.
    Refused,
    /// A host name for the [`Resolver`].
    Name,
}

/// The resolver-free part of Core's `LookupIntern`: `ContainsNoNUL`, then `SetSpecial` before any
/// DNS query, then the numeric form `getaddrinfo` decodes ([`crate::lookup::numeric_host`],
/// never an internal address). Any other name goes to the resolver, as in Core, including a
/// `.onion` or `.i2p` name that is not a valid address (an RFC 7686 resolver answers nothing
/// for it).
// Core: netbase.cpp:147-159 LookupIntern; netbase.cpp:195-198 Lookup
pub(crate) fn pre_lookup(host: &str) -> Lookup {
    if host.is_empty() || host.contains('\0') {
        return Lookup::Refused;
    }
    if let Some(addr) = NetAddr::set_special(host) {
        return Lookup::Addr(addr);
    }
    if let Some(addr) = crate::lookup::numeric_host(host) {
        // Core: netbase.cpp:167-169, "Never allow resolving to an internal address".
        return if addr.is_internal() {
            Lookup::Refused
        } else {
            Lookup::Addr(addr)
        };
    }
    Lookup::Name
}

/// Core's `LookupIntern`: [`pre_lookup`], then the resolver for a host name.
// Core: netbase.cpp:147-169 LookupIntern
pub(crate) async fn lookup(
    resolver: &dyn Resolver,
    host: &str,
    max: usize,
    allow_lookup: bool,
) -> Vec<NetAddr> {
    match pre_lookup(host) {
        Lookup::Addr(addr) => vec![addr],
        Lookup::Refused => Vec::new(),
        Lookup::Name => resolver.resolve(host, max, allow_lookup).await,
    }
}

/// Core's `ConvertSeeds`: the BIP155 fixed-seed table of a network as addresses with services
/// `NODE_NETWORK` and a time U(now − 2 w, now − 1 w] drawn from `rng` in table order. Entries
/// that do not decode end the table, as Core's stream read would throw at its end.
// Core: net.cpp:207-224 ConvertSeeds
pub fn convert_seeds(bip155: &[u8], now: i64, rng: &mut FastRandom) -> Vec<Address> {
    const ONE_WEEK: i64 = 7 * 24 * 60 * 60;
    let mut out = Vec::new();
    let mut rest = bip155;
    while !rest.is_empty() {
        let Some((service, tail)) = read_service_v2(rest) else {
            break;
        };
        rest = tail;
        let time = now - ONE_WEEK + rng.rand_uniform(-ONE_WEEK);
        tracing::debug!(target: "net", "Added hardcoded seed: {service}");
        out.push(Address {
            service,
            services: ServiceFlags::NODE_NETWORK,
            time: time as u32,
        });
    }
    out
}

/// One `CService` in BIP155 form: network id, CompactSize length, address bytes, big-endian
/// port, with Core's `UnserializeV2Stream` rules.
fn read_service_v2(bytes: &[u8]) -> Option<(Service, &[u8])> {
    let (&id, rest) = bytes.split_first()?;
    let (&len, rest) = rest.split_first()?;
    // The tables hold addresses of at most 32 bytes: a one-byte CompactSize.
    if len >= 0xFD || rest.len() < usize::from(len) + 2 {
        return None;
    }
    let (addr, rest) = rest.split_at(usize::from(len));
    let port = u16::from_be_bytes([rest[0], rest[1]]);
    let addr = match (id, addr.len()) {
        (1, 4) => AddrV2::Ipv4(addr.try_into().ok()?),
        (2, 16) => AddrV2::Ipv6(addr.try_into().ok()?),
        (4, 32) => AddrV2::TorV3(addr.try_into().ok()?),
        (5, 32) => AddrV2::I2p(addr.try_into().ok()?),
        (6, 16) => AddrV2::Cjdns(addr.try_into().ok()?),
        _ => AddrV2::Unknown {
            network: id,
            addr: addr.to_vec(),
        },
    };
    Some((Service::new(NetAddr::from_addrv2(&addr), port), &rest[2..]))
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};

    use super::*;
    use crate::netaddr::Network;

    fn seeds(net: CoreNetwork) -> Vec<Address> {
        let params = ChainParamsBuilder::new(net, Release::V24_0).build().unwrap();
        convert_seeds(params.fixed_seeds, 1_700_000_000, &mut FastRandom::deterministic())
    }

    /// v24 `chainparamsseeds.h`: mainnet 170 IPv4 and 25 Tor v3, testnet 19 IPv4, none on
    /// regtest (P2a §3.8.6); every time in (now − 2 w, now − 1 w].
    #[test]
    fn fixed_seed_tables() {
        let main = seeds(CoreNetwork::Main);
        let count = |n: Network| main.iter().filter(|a| a.service.addr.net() == n).count();
        assert_eq!((main.len(), count(Network::Ipv4), count(Network::Onion)), (195, 170, 25));
        let week = 7 * 24 * 3600;
        assert!(main.iter().all(|a| {
            let t = i64::from(a.time);
            t > 1_700_000_000 - 2 * week && t <= 1_700_000_000 - week
        }));
        assert!(main.iter().all(|a| a.services == ServiceFlags::NODE_NETWORK));
        let test = seeds(CoreNetwork::Testnet3);
        assert_eq!(test.len(), 19);
        assert!(test.iter().all(|a| a.service.addr.is_ipv4()));
        assert!(seeds(CoreNetwork::Regtest).is_empty());
    }
}
