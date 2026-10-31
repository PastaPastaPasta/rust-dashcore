//! Inbound connections (feature `inbound`; P2b drives the listener): Core's
//! `CreateNodeFromAcceptedSocket`, the inbound limit and `AttemptToEvictConnection` with
//! `SelectNodeToEvict` (v24 `net.cpp:1775-1950`, `node/eviction.cpp`).

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_p2p_core::ServiceFlags;

use crate::conn::{ConnectionType, PeerHandle, PeerId, Permissions};
use crate::netaddr::{Address, Network, Service};
use crate::netcore::{Attach, NETCONN, NetCore};
use crate::sock::NetSocket;
use crate::stdsort;

/// Core's `INBOUND_EVICTION_PROTECTION_TIME`: 1 second.
const INBOUND_EVICTION_PROTECTION_TIME: i64 = 1;

/// What eviction ranks a candidate by (Core's `NodeEvictionCandidate`).
#[derive(Clone, Debug)]
pub struct EvictionCandidate {
    /// The peer.
    pub id: PeerId,
    /// `m_connected`.
    pub connected: i64,
    /// `m_min_ping_time` in microseconds.
    pub min_ping_us: i64,
    /// `m_last_block_time`.
    pub last_block_time: i64,
    /// `m_last_tx_time`.
    pub last_tx_time: i64,
    /// `fRelevantServices`.
    pub relevant_services: bool,
    /// `m_relay_txs`.
    pub relay_txs: bool,
    /// `fBloomFilter`.
    pub bloom_filter: bool,
    /// `nKeyedNetGroup`.
    pub keyed_net_group: u64,
    /// `prefer_evict`.
    pub prefer_evict: bool,
    /// `m_is_local`.
    pub is_local: bool,
    /// `m_network`.
    pub network: Network,
    /// `m_noban`.
    pub noban: bool,
    /// `m_conn_type`.
    pub conn_type: ConnectionType,
}

type Candidates = Vec<EvictionCandidate>;

/// Core's `EraseLastKElements`: sorts by `less` (ascending) with libstdc++'s `std::sort`, whose
/// order of ties decides who survives (D36), and drops those of the last `k` that match `pred`.
// Core: node/eviction.cpp:76-84
fn erase_last_k(
    c: &mut Candidates,
    less: impl Fn(&EvictionCandidate, &EvictionCandidate) -> bool,
    k: usize,
    pred: impl Fn(&EvictionCandidate) -> bool,
) {
    stdsort::sort_by(c, less);
    let start = c.len() - k.min(c.len());
    let mut i = start;
    while i < c.len() {
        if pred(&c[i]) {
            c.remove(i);
        } else {
            i += 1;
        }
    }
}

fn reverse_min_ping(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    a.min_ping_us > b.min_ping_us
}

fn reverse_connected(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    a.connected > b.connected
}

fn netgroup_keyed(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    a.keyed_net_group < b.keyed_net_group
}

fn block_time(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    if a.last_block_time != b.last_block_time {
        return a.last_block_time < b.last_block_time;
    }
    if a.relevant_services != b.relevant_services {
        return b.relevant_services;
    }
    a.connected > b.connected
}

fn tx_time(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    if a.last_tx_time != b.last_tx_time {
        return a.last_tx_time < b.last_tx_time;
    }
    if a.relay_txs != b.relay_txs {
        return b.relay_txs;
    }
    if a.bloom_filter != b.bloom_filter {
        return a.bloom_filter;
    }
    a.connected > b.connected
}

fn block_relay_only_time(a: &EvictionCandidate, b: &EvictionCandidate) -> bool {
    if a.relay_txs != b.relay_txs {
        return a.relay_txs;
    }
    if a.last_block_time != b.last_block_time {
        return a.last_block_time < b.last_block_time;
    }
    if a.relevant_services != b.relevant_services {
        return b.relevant_services;
    }
    a.connected > b.connected
}

/// Core's `ProtectEvictionCandidatesByRatio`: protects half of the candidates, up to a quarter
/// by disadvantaged network (CJDNS, I2P, localhost, onion) and the rest by uptime.
// Core: node/eviction.cpp:107-176
pub fn protect_by_ratio(c: &mut Candidates) {
    let initial = c.len();
    let total_protect = initial / 2;
    // (is_local, network, count); `None` is localhost (Core's NET_MAX).
    let mut nets: [(bool, Option<Network>, usize); 4] = [
        (false, Some(Network::Cjdns), 0),
        (false, Some(Network::I2p), 0),
        (true, None, 0),
        (false, Some(Network::Onion), 0),
    ];
    let in_net = |n: &(bool, Option<Network>, usize), e: &EvictionCandidate| {
        if n.0 {
            e.is_local
        } else {
            Some(e.network) == n.1
        }
    };
    for n in &mut nets {
        let probe = *n;
        n.2 = c.iter().filter(|e| in_net(&probe, e)).count();
    }
    nets.sort_by_key(|n| n.2);
    let max_by_network = total_protect / 2;
    let mut protected = 0;
    while protected < max_by_network {
        let num_networks = nets.iter().filter(|n| n.2 > 0).count();
        if num_networks == 0 {
            break;
        }
        let per_network = ((max_by_network - protected) / num_networks).max(1);
        let mut any = false;
        for n in &mut nets {
            if n.2 == 0 {
                continue;
            }
            let probe = *n;
            let before = c.len();
            erase_last_k(
                c,
                |a, b| {
                    if probe.0 && a.is_local != b.is_local {
                        return b.is_local;
                    }
                    let (an, bn) = (Some(a.network) == probe.1, Some(b.network) == probe.1);
                    if an != bn {
                        return bn;
                    }
                    a.connected > b.connected
                },
                per_network,
                |e| in_net(&probe, e),
            );
            let delta = before - c.len();
            if delta > 0 {
                any = true;
                protected += delta;
                if protected >= max_by_network {
                    break;
                }
                n.2 -= delta;
            }
        }
        if !any {
            break;
        }
    }
    debug_assert_eq!(protected, initial - c.len());
    erase_last_k(c, reverse_connected, total_protect - protected, |_| true);
}

/// Core's `SelectNodeToEvict`.
// Core: node/eviction.cpp:178-239 SelectNodeToEvict
pub fn select_node_to_evict(mut c: Candidates) -> Option<PeerId> {
    c.retain(|e| !e.noban);
    c.retain(|e| e.conn_type == ConnectionType::Inbound);
    erase_last_k(&mut c, netgroup_keyed, 4, |_| true);
    erase_last_k(&mut c, reverse_min_ping, 8, |_| true);
    erase_last_k(&mut c, tx_time, 4, |_| true);
    erase_last_k(&mut c, block_relay_only_time, 8, |e| !e.relay_txs && e.relevant_services);
    erase_last_k(&mut c, block_time, 4, |_| true);
    protect_by_ratio(&mut c);
    if c.is_empty() {
        return None;
    }
    if c.iter().any(|e| e.prefer_evict) {
        c.retain(|e| e.prefer_evict);
    }
    // The netgroup with the most connections, the youngest group on a tie (the candidates are
    // sorted by reverse connect time).
    let mut groups: BTreeMap<u64, Vec<&EvictionCandidate>> = BTreeMap::new();
    let (mut most, mut most_count, mut most_time) = (0u64, 0usize, 0i64);
    for e in &c {
        let group = groups.entry(e.keyed_net_group).or_default();
        group.push(e);
        let group_time = group[0].connected;
        if group.len() > most_count || (group.len() == most_count && group_time > most_time) {
            most_count = group.len();
            most_time = group_time;
            most = e.keyed_net_group;
        }
    }
    groups.get(&most).map(|g| g[0].id)
}

impl NetCore {
    /// Core's `GetEvictionCandidates`: every peer not being disconnected, except, when we run an
    /// active masternode, inbound peers still in their first second (until a first message
    /// other than `mnauth`) and verified masternodes.
    // Core: net.cpp:1775-1820 CConnman::GetEvictionCandidates
    pub fn eviction_candidates(&self) -> Vec<EvictionCandidate> {
        let now = self.clock().node_now().0;
        self.peers()
            .iter()
            .filter(|p| !p.disconnect_requested())
            .filter(|p| {
                if !self.config().active_masternode {
                    return true;
                }
                let mut protected = now - p.connected() < INBOUND_EVICTION_PROTECTION_TIME;
                if p.first_message_time() != 0 && !p.first_message_is_mnauth() {
                    protected = false;
                }
                if p.verified_proreg_hash() != [0; 32] {
                    protected = true;
                }
                !protected
            })
            .map(|p| EvictionCandidate {
                id: p.id(),
                connected: p.connected(),
                min_ping_us: p.min_ping_us(),
                last_block_time: p.last_block_time(),
                last_tx_time: p.last_tx_time(),
                relevant_services: p.has_all_wanted_services(),
                relay_txs: p.relays_txs(),
                bloom_filter: p.bloom_filter_loaded(),
                keyed_net_group: p.keyed_net_group(),
                prefer_evict: p.prefer_evict(),
                is_local: p.addr().service.addr.is_local(),
                network: p.network(),
                noban: p.has_permission(Permissions::NO_BAN),
                conn_type: p.conn_type(),
            })
            .collect()
    }

    /// Core's `AttemptToEvictConnection`: marks the selected peer for disconnection. Its line,
    /// like `accept`'s, is Core's `NET_NETCONN` (`-debug=net` or `-debug=netconn`; BL-327 D14).
    // Core: net.cpp:1830-1846 CConnman::AttemptToEvictConnection
    pub fn attempt_to_evict_connection(&self) -> bool {
        let Some(id) = select_node_to_evict(self.eviction_candidates()) else {
            return false;
        };
        let Some(p) =
            self.peers().iter().find(|p| !p.disconnect_requested() && p.id() == id).cloned()
        else {
            return false;
        };
        tracing::debug!(target: NETCONN, "selected {} connection for eviction peer={}; disconnecting", p.conn_type().as_str(), p.id());
        p.mark_disconnect();
        self.sweep_wake.notify_one();
        true
    }

    /// Core's `CreateNodeFromAcceptedSocket`: applies `-whitelist`, refuses banned peers (and
    /// discouraged ones when nearly full), evicts down to the inbound limit, and attaches the
    /// peer with V2 iff we advertise it. `refuse_unsynced` is Core's `m_active_masternode &&
    /// !IsBlockchainSynced()`. Returns `None` when the connection is dropped.
    ///
    /// `addr` and `addr_bind` are the socket's peer and local addresses as the OS reports them;
    /// `accept` flips each to CJDNS when it is one (`fc00::/8` with CJDNS reachable), as Core's
    /// `AcceptConnection` does before this point, so `-whitelist` and the peer see CJDNS.
    ///
    /// The host calls it from an accept task per listening socket, where Core accepts on its
    /// one socket thread. The admission lock is held from the inbound count through the
    /// attach's insertion into the table, so each decision sees every peer admitted before it
    /// (BL-327 D20). Nothing under it awaits; the log lines and the host's `initialize`
    /// callback run under it, as on Core's socket thread.
    // Core: net.cpp:1864-1867 AcceptConnection; net.cpp:1880-2010 CreateNodeFromAcceptedSocket
    pub fn accept(
        &self,
        sock: Arc<dyn NetSocket>,
        addr: Address,
        addr_bind: Service,
        mut permissions: Permissions,
        inbound_onion: bool,
        refuse_unsynced: bool,
    ) -> Option<PeerHandle> {
        // A listening socket's readiness ends Core's wait, so a pass follows even a refusal (D43).
        // Core: net.cpp:2423, 2579-2590
        self.sweep_wake.notify_one();
        let addr = Address {
            service: self.maybe_flip_ipv6_to_cjdns(addr.service),
            ..addr
        };
        let addr_bind = self.maybe_flip_ipv6_to_cjdns(addr_bind);
        let cfg = self.config();
        let ip = &addr.service.addr;
        // Core: net.cpp AddWhitelistPermissionFlags
        for (net, flags) in &cfg.whitelist {
            if net.matches(ip) {
                permissions = Permissions(permissions.0 | flags.0);
            }
        }
        if permissions.has(Permissions::IMPLICIT) {
            permissions = Permissions(permissions.0 & !Permissions::IMPLICIT.0);
            for (on, flag) in [
                (cfg.whitelist_force_relay, Permissions::FORCE_RELAY),
                (cfg.whitelist_relay, Permissions::RELAY),
                (true, Permissions::MEMPOOL),
                (true, Permissions::NO_BAN),
            ] {
                if on {
                    permissions = Permissions(permissions.0 | flag.0);
                }
            }
        }
        // Core's `m_max_outbound`, not capped: below 11 connections `nMaxInbound` is negative.
        // Core: net.h:1269
        let max_outbound =
            cfg.max_outbound_full_relay + cfg.max_outbound_block_relay + cfg.max_feeler;
        let max_inbound = cfg.max_connections as i64 - max_outbound as i64;
        let _admission = self.admission.lock();
        let peers = self.peers();
        let mut inbound = peers.iter().filter(|p| p.is_inbound()).count() as i64;
        let verified_mn =
            peers.iter().filter(|p| p.is_inbound() && p.verified_proreg_hash() != [0; 32]).count()
                as i64;
        let dropped = "connection dropped";
        if !self.network_active() {
            tracing::debug!(target: NETCONN, "{dropped}: not accepting new connections");
            return None;
        }
        let now = self.clock().node_now().0;
        let noban = permissions.has(Permissions::NO_BAN);
        if !noban && self.banlist().is_banned(ip, now) {
            tracing::debug!(target: NETCONN, "{dropped} (banned)");
            return None;
        }
        let discouraged = self.banlist().is_discouraged(ip);
        if !noban && inbound + 1 >= max_inbound && discouraged {
            tracing::debug!(target: NETCONN, "connection from {} dropped (discouraged)", addr.service);
            return None;
        }
        while inbound - verified_mn >= max_inbound {
            if !self.attempt_to_evict_connection() {
                tracing::debug!(target: NETCONN, "failed to find an eviction candidate - connection dropped (full)");
                return None;
            }
            inbound -= 1;
        }
        if refuse_unsynced {
            tracing::debug!(target: NETCONN, "AcceptConnection -- blockchain is not synced yet, skipping inbound connection attempt");
            return None;
        }
        let mut services = cfg.local_services;
        if permissions.has(Permissions::BLOOM_FILTER) {
            services = ServiceFlags(services.0 | ServiceFlags::NODE_BLOOM.0);
        }
        let use_v2 = services.0 & ServiceFlags::NODE_P2P_V2.0 != 0;
        #[cfg(test)]
        if let Some(hook) = self.admission_hook.get() {
            hook();
        }
        // `Err`: the core was interrupted first.
        let peer = self
            .attach_inner(
                Attach {
                    sock,
                    addr,
                    addr_bind,
                    dest: None,
                    conn_type: ConnectionType::Inbound,
                    inbound_onion,
                    permissions,
                    use_v2,
                    grant: None,
                    masternode: false,
                    masternode_probe: false,
                },
                discouraged,
            )
            .ok()?;
        // Core's `fLogIPs = false` form, without the socket (D29).
        tracing::debug!(target: NETCONN, "connection accepted, peer={}", peer.id());
        Some(peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: PeerId, f: impl FnOnce(&mut EvictionCandidate)) -> EvictionCandidate {
        let mut c = EvictionCandidate {
            id,
            connected: id,
            min_ping_us: 1000,
            last_block_time: 0,
            last_tx_time: 0,
            relevant_services: false,
            relay_txs: true,
            bloom_filter: false,
            keyed_net_group: id as u64,
            prefer_evict: false,
            is_local: false,
            network: Network::Ipv4,
            noban: false,
            conn_type: ConnectionType::Inbound,
        };
        f(&mut c);
        c
    }

    /// Core's `net_peer_eviction_tests/peer_eviction_test` invariants: noban and outbound are
    /// never chosen, and with fewer candidates than the protected slots nothing is.
    #[test]
    fn protections() {
        let few: Vec<_> = (0..20).map(|i| cand(i, |_| {})).collect();
        assert_eq!(select_node_to_evict(few), None);
        let many: Vec<_> = (0..120)
            .map(|i| {
                cand(i, |c| {
                    c.noban = i % 3 == 0;
                    if i % 3 == 1 {
                        c.conn_type = ConnectionType::OutboundFullRelay;
                    }
                    c.keyed_net_group = (i % 5) as u64;
                })
            })
            .collect();
        let chosen = select_node_to_evict(many.clone()).unwrap();
        let c = many.iter().find(|c| c.id == chosen).unwrap();
        assert!(!c.noban && c.conn_type == ConnectionType::Inbound);
    }

    /// The youngest member of the largest netgroup is evicted, and `prefer_evict` wins.
    #[test]
    fn largest_group_youngest() {
        let c: Vec<_> = (0..40)
            .map(|i| {
                cand(i, |c| {
                    c.keyed_net_group = if i >= 30 {
                        7
                    } else {
                        100 + i as u64
                    };
                    c.min_ping_us = 1000 + i;
                })
            })
            .collect();
        assert_eq!(select_node_to_evict(c.clone()), Some(39));
        let mut c = c;
        c[33].prefer_evict = true;
        assert_eq!(select_node_to_evict(c), Some(33));
    }

    /// Core's `protect_by_ratio` worked cases: half protected, a quarter by network.
    #[test]
    fn ratio_onion() {
        let mut c: Vec<_> = (0..16)
            .map(|i| {
                cand(i, |c| {
                    if i < 4 {
                        c.network = Network::Onion;
                    }
                })
            })
            .collect();
        protect_by_ratio(&mut c);
        assert_eq!(c.len(), 8);
        // Four onion slots: all four onion peers are protected; then the 4 oldest others.
        assert!(c.iter().all(|e| e.network == Network::Ipv4));
        let mut left: Vec<_> = c.iter().map(|e| e.id).collect();
        left.sort();
        assert_eq!(left, (8..16).collect::<Vec<_>>());
    }
}

#[cfg(test)]
#[path = "inbound_core_tests.rs"]
mod core_tests;

#[cfg(test)]
#[path = "inbound_accept_tests.rs"]
mod accept_tests;
