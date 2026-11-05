//! Our own addresses (Core's `mapLocalHost`, `net.cpp:180-340`) and the peer-time median
//! (`timedata.cpp`): connection-manager state the handshake and address paths read (D14).

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use crate::conn::Peer;
use crate::netaddr::{NetAddr, Network, Service};
use crate::netcore::NetCore;

/// Core's `LOCAL_*` address scores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i32)]
pub enum LocalScore {
    /// `LOCAL_NONE`: unknown.
    None = 0,
    /// `LOCAL_IF`: an address a local interface listens on.
    If,
    /// `LOCAL_BIND`: an address bound explicitly.
    Bind,
    /// `LOCAL_MAPPED`: reported by UPnP or NAT-PMP.
    Mapped,
    /// `LOCAL_MANUAL`: `-externalip`.
    Manual,
}

/// Core's `LocalServiceInfo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LocalInfo {
    score: i32,
    port: u16,
}

/// Core's `mapLocalHost`.
#[derive(Debug, Default)]
pub(crate) struct LocalHost(BTreeMap<NetAddr, LocalInfo>);

/// Core's `BITCOIN_TIMEDATA_MAX_SAMPLES`.
const TIMEDATA_MAX_SAMPLES: usize = 200;

/// Core's `DEFAULT_MAX_TIME_ADJUSTMENT`: 70 minutes.
pub const DEFAULT_MAX_TIME_ADJUSTMENT: i64 = 70 * 60;

/// Core's time-offset state: `g_sources`, `g_time_offsets` (a `CMedianFilter` of 200 that
/// starts with one 0) and `nTimeOffset`.
#[derive(Debug)]
pub(crate) struct TimeData {
    sources: BTreeSet<NetAddr>,
    values: Vec<i64>,
    offset: i64,
    warned: bool,
}

impl Default for TimeData {
    fn default() -> Self {
        Self {
            sources: BTreeSet::new(),
            values: vec![0],
            offset: 0,
            warned: false,
        }
    }
}

impl TimeData {
    /// Core's `AddTimeData`, with `-maxtimeadjustment`. Returns true when Core would raise its
    /// "check your computer's date and time" warning (once).
    // Core: timedata.cpp:39-104 AddTimeData; timedata.h CMedianFilter
    pub(crate) fn add(&mut self, ip: &NetAddr, sample: i64, max_adjustment: i64) -> bool {
        if self.sources.len() == TIMEDATA_MAX_SAMPLES || !self.sources.insert(ip.clone()) {
            return false;
        }
        if self.values.len() == TIMEDATA_MAX_SAMPLES {
            self.values.remove(0);
        }
        self.values.push(sample);
        tracing::debug!(
            target: "net",
            "added time data, samples {}, offset {sample:+} ({:+} minutes)",
            self.values.len(),
            sample / 60
        );
        let n = self.values.len();
        if n < 5 || n.is_multiple_of(2) {
            return false;
        }
        let mut sorted = self.values.clone();
        sorted.sort_unstable();
        // Odd size: the middle element.
        let median = sorted[n / 2];
        let max_adjustment = max_adjustment.max(0);
        let mut warn = false;
        if (-max_adjustment..=max_adjustment).contains(&median) {
            self.offset = median;
        } else {
            self.offset = 0;
            if !self.warned && !sorted.iter().any(|&o| o != 0 && o > -5 * 60 && o < 5 * 60) {
                self.warned = true;
                warn = true;
            }
        }
        tracing::debug!(
            target: "net",
            "time data samples: {}|  median offset = {:+}  ({:+} minutes)",
            sorted.iter().map(|n| format!("{n:+}  ")).collect::<String>(),
            self.offset,
            self.offset / 60
        );
        warn
    }

    /// `nTimeOffset`.
    pub(crate) fn offset(&self) -> i64 {
        self.offset
    }
}

/// Core's `CNetAddr::GetReachabilityFrom`: how well a peer on `partner` reaches `ours`.
// Core: netaddress.cpp:709-790 GetExtNetwork, GetReachabilityFrom
fn reachability_from(ours: &NetAddr, partner: &NetAddr, allow_private_net: bool) -> i32 {
    const UNREACHABLE: i32 = 0;
    const DEFAULT: i32 = 1;
    const TEREDO: i32 = 2;
    const IPV6_WEAK: i32 = 3;
    const IPV4: i32 = 4;
    const IPV6_STRONG: i32 = 5;
    const PRIVATE: i32 = 6;

    /// `GetExtNetwork`: the network, or Teredo as its own.
    #[derive(PartialEq)]
    enum Ext {
        Net(Network),
        Teredo,
    }
    let ext = |a: &NetAddr| {
        if a.is_rfc4380() {
            Ext::Teredo
        } else {
            Ext::Net(a.network(allow_private_net))
        }
    };
    if !ours.is_routable(allow_private_net) || ours.is_internal() {
        return UNREACHABLE;
    }
    let tunnel = ours.is_rfc3964() || ours.is_rfc6052() || ours.is_rfc6145();
    let (our, their) = (ext(ours), ext(partner));
    use Network as N;
    match their {
        Ext::Net(N::Ipv4) => match our {
            Ext::Net(N::Ipv4) => IPV4,
            _ => DEFAULT,
        },
        Ext::Net(N::Ipv6) => match our {
            Ext::Teredo => TEREDO,
            Ext::Net(N::Ipv4) => IPV4,
            Ext::Net(N::Ipv6) if tunnel => IPV6_WEAK,
            Ext::Net(N::Ipv6) => IPV6_STRONG,
            _ => DEFAULT,
        },
        Ext::Net(N::Onion) => match our {
            Ext::Net(N::Ipv4) => IPV4,
            Ext::Net(N::Onion) => PRIVATE,
            _ => DEFAULT,
        },
        Ext::Net(N::I2p) => match our {
            Ext::Net(N::I2p) => PRIVATE,
            _ => DEFAULT,
        },
        Ext::Net(N::Cjdns) => match our {
            Ext::Net(N::Cjdns) => PRIVATE,
            _ => DEFAULT,
        },
        Ext::Teredo => match our {
            Ext::Teredo => TEREDO,
            Ext::Net(N::Ipv6) => IPV6_WEAK,
            Ext::Net(N::Ipv4) => IPV4,
            _ => DEFAULT,
        },
        Ext::Net(N::Unroutable | N::Internal) => match our {
            Ext::Teredo => TEREDO,
            Ext::Net(N::Ipv6) => IPV6_WEAK,
            Ext::Net(N::Ipv4) => IPV4,
            Ext::Net(N::Onion) => PRIVATE,
            _ => DEFAULT,
        },
    }
}

impl NetCore {
    /// Core's `MaybeFlipIPv6toCJDNS`: an IPv6 address in `fc00::/8` is CJDNS when CJDNS is
    /// reachable.
    // Core: netbase.cpp:833-840 MaybeFlipIPv6toCJDNS
    pub fn maybe_flip_ipv6_to_cjdns(&self, s: Service) -> Service {
        dash_netaddr::maybe_flip_ipv6_to_cjdns(s, self.is_reachable(Network::Cjdns))
    }

    /// Core's `AddLocal(addr, score)`: learns one of our addresses, or raises its score.
    // Core: net.cpp:284-310 AddLocal
    pub fn add_local(&self, addr: Service, score: LocalScore) -> bool {
        let cfg = self.config();
        let addr = self.maybe_flip_ipv6_to_cjdns(addr);
        if !addr.addr.is_routable(cfg.allow_private_net) && cfg.params.require_routable_external_ip
        {
            return false;
        }
        let score = score as i32;
        if !cfg.discover && score < LocalScore::Manual as i32 {
            return false;
        }
        if !self.is_reachable(addr.addr.network(cfg.allow_private_net)) {
            return false;
        }
        tracing::info!("AddLocal({addr},{score})");
        let mut local = self.local.lock();
        match local.0.get_mut(&addr.addr) {
            None => {
                local.0.insert(
                    addr.addr.clone(),
                    LocalInfo {
                        score,
                        port: addr.port,
                    },
                );
            }
            Some(info) if score >= info.score => {
                info.score = score + 1;
                info.port = addr.port;
            }
            Some(_) => {}
        }
        true
    }

    /// Core's `mapLocalHost`, in the map's order: each of our addresses with its port and
    /// score (`LocalServiceInfo`), as `getnetworkinfo` lists them.
    pub fn local_addresses(&self) -> Vec<(NetAddr, u16, i32)> {
        let local = self.local.lock();
        local.0.iter().map(|(addr, info)| (addr.clone(), info.port, info.score)).collect()
    }

    /// Core's `Discover`: with `-discover`, each of `interfaces` (the name and address of every
    /// interface that is up and not loopback, in `getifaddrs` order, which the host reads: SB0
    /// L5) becomes ours at `LOCAL_IF`, on our listen port.
    // Core: net.cpp:3843-3887 Discover, :312-315 AddLocal(CNetAddr)
    pub fn discover(&self, interfaces: &[(String, IpAddr)]) {
        let cfg = self.config();
        if !cfg.discover {
            return;
        }
        for (name, ip) in interfaces {
            let family = if ip.is_ipv4() {
                "IPv4"
            } else {
                "IPv6"
            };
            let addr = NetAddr::from_ip(*ip);
            if self.add_local(Service::new(addr.clone(), cfg.listen_port), LocalScore::If) {
                tracing::info!("Discover: {family} {name}: {addr}");
            }
        }
    }

    /// Core's `RemoveLocal`.
    pub fn remove_local(&self, addr: &Service) {
        tracing::info!("RemoveLocal({addr})");
        self.local.lock().0.remove(&addr.addr);
    }

    /// Core's `SeenLocal`: a peer reported this address of ours; raises its score if known.
    // Core: net.cpp:325-334 SeenLocal
    pub fn seen_local(&self, addr: &Service) -> bool {
        match self.local.lock().0.get_mut(&addr.addr) {
            Some(info) => {
                info.score += 1;
                true
            }
            None => false,
        }
    }

    /// Core's `IsLocal(addr)`: whether the address (any port) is one of ours.
    pub fn is_local(&self, addr: &NetAddr) -> bool {
        self.local.lock().0.contains_key(addr)
    }

    /// Core's `GetLocal(peer)`: our best address for `peer`, by reachability then score, never
    /// crossing between a privacy network and another network.
    // Core: net.cpp:177-206 GetLocal
    fn get_local(&self, peer: &Peer) -> Option<Service> {
        let cfg = self.config();
        if !cfg.listen {
            return None;
        }
        let through = peer.network();
        let through_privacy = peer.inbound_onion() || peer.addr().service.addr.is_privacy_net();
        let mut best: Option<Service> = None;
        let (mut best_score, mut best_reach) = (-1, -1);
        for (addr, info) in &self.local.lock().0 {
            if addr.network(cfg.allow_private_net) != through
                && (addr.is_privacy_net() || through_privacy)
            {
                continue;
            }
            let reach = reachability_from(addr, &peer.addr().service.addr, cfg.allow_private_net);
            if reach > best_reach || (reach == best_reach && info.score > best_score) {
                best = Some(Service::new(addr.clone(), info.port));
                best_reach = reach;
                best_score = info.score;
            }
        }
        best
    }

    /// Core's `GetLocalAddress(peer)`: [`NetCore::get_local`], else Core's default `CNetAddr()`
    /// (the IPv6 `::`) on our listen port.
    // Core: net.cpp:232-235 GetLocalAddress
    pub fn local_address(&self, peer: &Peer) -> Service {
        self.get_local(peer)
            .unwrap_or_else(|| Service::new(NetAddr::default(), self.config().listen_port))
    }

    /// Core's `IsPeerAddrLocalGood`: the peer's view of our address can teach us our external
    /// address.
    // Core: net.cpp:244-250 IsPeerAddrLocalGood
    pub fn is_peer_addr_local_good(&self, peer: &Peer) -> bool {
        let cfg = self.config();
        let Some(local) = peer.addr_local() else {
            return false;
        };
        cfg.discover
            && peer.addr().service.addr.is_routable(cfg.allow_private_net)
            && local.addr.is_routable(cfg.allow_private_net)
            && self.is_reachable(local.addr.network(cfg.allow_private_net))
    }

    /// Core's `GetLocalAddrForPeer`: the address to advertise to `peer`, sometimes the one the
    /// peer says it sees, or none when unroutable. Draws from a fresh context, as Core's
    /// function-local `FastRandomContext`.
    // Core: net.cpp:252-281 GetLocalAddrForPeer
    pub fn local_addr_for_peer(&self, peer: &Peer) -> Option<Service> {
        let cfg = self.config();
        let mut local = self.local_address(peer);
        if self.is_peer_addr_local_good(peer) {
            let score = self.local.lock().0.get(&local.addr).map_or(0, |i| i.score);
            let bits = if score > LocalScore::Manual as i32 {
                3
            } else {
                1
            };
            if !local.addr.is_routable(cfg.allow_private_net) || self.new_rng().randbits(bits) == 0
            {
                let seen = peer.addr_local().expect("checked by is_peer_addr_local_good");
                if peer.is_inbound() {
                    local = seen;
                } else {
                    local.addr = seen.addr;
                }
            }
        }
        if local.addr.is_routable(cfg.allow_private_net) {
            tracing::debug!(target: "net", "Advertising address {local} to peer={}", peer.id());
            Some(local)
        } else {
            None
        }
    }

    /// Core's `AddTimeData(ip, offset)`.
    pub fn add_time_data(&self, ip: &NetAddr, offset: i64) {
        if self.timedata.lock().add(ip, offset, self.config().max_time_adjustment) {
            tracing::warn!(target: "net", "Please check that your computer's date and time are correct! If your clock is wrong, Dash Core will not work properly.");
            self.clock_warning.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// Core's `GetTimeOffset()`: the peers' median offset, 0 until five samples.
    pub fn time_offset(&self) -> i64 {
        self.timedata.lock().offset()
    }

    /// Core's `GetAdjustedTime()`: the node clock plus [`NetCore::time_offset`].
    pub fn adjusted_time(&self) -> i64 {
        self.clock().node_now().0 + self.time_offset()
    }

    /// Whether the peers' clocks disagreed with ours enough for Core's clock warning
    /// (`SetMiscWarning`).
    pub fn clock_warning(&self) -> bool {
        self.clock_warning.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> NetAddr {
        NetAddr::ipv4([1, 2, 3, n])
    }

    /// Core's `timedata_tests/addtimedata`, transcribed.
    #[test]
    fn addtimedata() {
        let mut t = TimeData::default();
        let mut cnt = 0;
        let mut multi = |t: &mut TimeData, n: usize, offset: i64| {
            let mut warned = false;
            for _ in 0..n {
                cnt += 1;
                let addr = NetAddr::internal(&cnt.to_string()).unwrap();
                warned |= t.add(&addr, offset, DEFAULT_MAX_TIME_ADJUSTMENT);
            }
            warned
        };
        assert_eq!(t.offset(), 0);
        // Part 1: large offsets raise the clock warning once the filter holds 5.
        assert!(!multi(&mut t, 3, DEFAULT_MAX_TIME_ADJUSTMENT + 1));
        assert!(multi(&mut t, 1, DEFAULT_MAX_TIME_ADJUSTMENT + 1));
        assert_eq!(t.offset(), 0);
        // Part 2: positive and negative medians.
        multi(&mut t, 4, 100);
        assert_eq!(t.offset(), 100);
        multi(&mut t, 10, -100);
        assert_eq!(t.offset(), -100);
        // Part 3: the filter reaches its maximum.
        let nfill = (200 - 3 - 19) / 2;
        multi(&mut t, nfill, 100);
        multi(&mut t, nfill, -100);
        assert_eq!(t.offset(), -100);
        multi(&mut t, 2, 100);
        assert_eq!(t.offset(), 0);
        multi(&mut t, 2, 100);
        assert_eq!(t.offset(), 0);
    }

    #[test]
    fn reachability() {
        let v4 = ip(1);
        let v6 = NetAddr::from_ip("2001:db9::1".parse().unwrap());
        assert_eq!(reachability_from(&v4, &v4, false), 4);
        assert_eq!(reachability_from(&v6, &v6, false), 5);
        assert_eq!(reachability_from(&v6, &v4, false), 1);
        assert_eq!(reachability_from(&NetAddr::ipv4([10, 0, 0, 1]), &v4, false), 0);
    }
}
