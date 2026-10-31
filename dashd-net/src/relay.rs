//! Address relay (P2a §3.6.4–3.6.6) behind P2a-core's [`AddressRelay`] seam: `RelayAddress`,
//! `PushAddress`, `MaybeSendAddr` (`SendMessages` section 5) and the inbound `getaddr` reply with
//! Core's address-response cache (`CConnman::GetAddresses(CNode&, …)`).
//!
//! SB6's intake (`dash_p2p::addr::on_addr`) has already stored the addresses and chosen the ones
//! Core relays; this module picks the targets and fills their send queues. Every random draw
//! comes from the network core's source as Core's site draws it: `GetRand` is
//! [`NetCore::get_rand`], a `FastRandomContext` is one [`NetCore::new_rng`] per Core context.

use std::collections::HashMap;
use std::sync::Arc;

use dash_p2p::addr::{AddrRelay, MAX_ADDR_TO_SEND, setup_address_relay};
use dash_p2p::conn::Permissions;
use dash_p2p::rng::{FastRandom, exponential_rand};
use dash_p2p::siphash::SipHasher;
use dash_p2p::{Address, CommandString, NetCore, PeerHandle, PeerId, Service, ServiceFlags};
use dash_p2p_core::{Addr, AddrV2Entry, AddrV2Msg, TimestampedAddr};
use dash_primitives::{AddrV1, ServiceV1};
use dashd_p2p::processing::logic::{PeerMap, out_msg};
use dashd_p2p::processing::seams::AddressRelay;
use dashd_p2p::{ChainAccess, HandlerCx};

/// Core's `AVG_ADDRESS_BROADCAST_INTERVAL`: 30 seconds.
pub const AVG_ADDRESS_BROADCAST_INTERVAL_SECS: i64 = 30;
/// Core's `AVG_LOCAL_ADDRESS_BROADCAST_INTERVAL`: 24 hours.
pub const AVG_LOCAL_ADDRESS_BROADCAST_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// Core's `ROTATE_ADDR_RELAY_DEST_INTERVAL`: 24 hours.
const ROTATE_ADDR_RELAY_DEST_INTERVAL_SECS: u64 = 24 * 60 * 60;
/// Core's `MAX_PCT_ADDR_TO_SEND`.
pub const MAX_PCT_ADDR_TO_SEND: usize = 23;
/// Core's `RANDOMIZER_ID_ADDRESS_RELAY` (`net_processing.cpp:140`).
const RANDOMIZER_ID_ADDRESS_RELAY: u64 = 0x3cac_0035_b586_6b90;
/// Core's `RANDOMIZER_ID_ADDRCACHE` (`net.cpp:126`).
const RANDOMIZER_ID_ADDRCACHE: u64 = 0x1cf2_e4dd_d306_dda9;
/// The cache lifetime: 21 hours plus `GetRandMillis(6 h)`.
const ADDR_CACHE_BASE_US: i64 = 21 * 60 * 60 * 1_000_000;
const ADDR_CACHE_JITTER_MS: u64 = 6 * 60 * 60 * 1000;

const ADDR: CommandString = CommandString::from_static("addr");
const ADDRV2: CommandString = CommandString::from_static("addrv2");

/// One `m_addr_response_caches` entry (Core's `CachedAddrResponse`).
#[derive(Debug, Default)]
struct CachedAddrResponse {
    addrs: Vec<Address>,
    /// `m_cache_entry_expiration` in µs; 0 for a new entry.
    expiration_us: i64,
}

/// The node's address relay: P2a-core's [`AddressRelay`] seam filled with Core's
/// `PeerManagerImpl` and `CConnman` address functions.
pub struct NodeAddressRelay {
    chain: Arc<dyn ChainAccess>,
    /// `CConnman::m_addr_response_caches`. Keyed by a SipHash, never iterated.
    caches: HashMap<u64, CachedAddrResponse>,
}

impl NodeAddressRelay {
    /// The relay; `chain` answers `IsInitialBlockDownload()` for `MaybeSendAddr`.
    pub fn new(chain: Arc<dyn ChainAccess>) -> Self {
        Self {
            chain,
            caches: HashMap::new(),
        }
    }

    /// Core's `IsInitialBlockDownload()` now.
    fn in_ibd(&self, core: &NetCore) -> bool {
        let snap = self.chain.snapshot();
        self.chain.is_initial_block_download(&snap, core.clock().node_now())
    }

    /// Core's `CConnman::GetAddresses(CNode& requestor, max_addresses, max_pct)`: one cached
    /// response per requestor network and local socket, refreshed after 21 h + U[0, 6 h).
    // Core: net.cpp:4265-4307 CConnman::GetAddresses
    fn cached_addresses(&mut self, core: &NetCore, requestor: &PeerHandle) -> Vec<Address> {
        let bind = requestor.addr_bind();
        let cache_id = core
            .deterministic_randomizer(RANDOMIZER_ID_ADDRCACHE)
            .write_u64(requestor.network() as u64)
            .write(&bind.addr.addr_bytes())
            // For outbound connections, the port of the bound address is randomly assigned by
            // the OS and would therefore not be useful for seeding.
            .write_u64(if requestor.is_inbound() {
                u64::from(bind.port)
            } else {
                0
            })
            .finish();
        let now_us = core.clock().node_now_micros();
        let entry = self.caches.entry(cache_id).or_default();
        // A new entry has expiration 0.
        if entry.expiration_us < now_us {
            entry.addrs = get_addresses(core, MAX_ADDR_TO_SEND, MAX_PCT_ADDR_TO_SEND);
            // Core: random.h GetRandMillis (`GetRand(6 h in ms)` = `randrange`)
            let jitter_ms = core.get_rand(ADDR_CACHE_JITTER_MS) as i64;
            entry.expiration_us = now_us + ADDR_CACHE_BASE_US + jitter_ms * 1000;
        }
        entry.addrs.clone()
    }
}

/// Core's `CConnman::GetAddresses(max_addresses, max_pct, network = nullopt)`: AddrMan's
/// `GetAddr` without banned or discouraged entries.
// Core: net.cpp:4254-4263 CConnman::GetAddresses
pub fn get_addresses(core: &NetCore, max_addresses: usize, max_pct: usize) -> Vec<Address> {
    let now = core.clock().node_now().0;
    let mut addrs = core.addrman().get_addr(max_addresses, max_pct, None, true, now);
    let bans = core.banlist();
    addrs
        .retain(|a| !bans.is_discouraged(&a.service.addr) && !bans.is_banned(&a.service.addr, now));
    addrs
}

/// Core's `IsAddrCompatible`: the peer reads BIP155, or the address fits the v1 format.
// Core: net_processing.cpp:1152-1155 IsAddrCompatible
fn is_addr_compatible(relay: &AddrRelay, addr: &Address) -> bool {
    relay.wants_addrv2 || addr.service.addr.is_addr_v1_compatible()
}

/// Core's `PushAddress`: queue `addr` unless it is invalid, already known to the peer or not in
/// a format it reads; a full queue overwrites an entry `insecure_rand` picks (the caller's
/// `FastRandomContext`, one per Core call site: [`NetCore::new_rng`]).
// Core: net_processing.cpp:1165-1178 PeerManagerImpl::PushAddress
pub fn push_address(insecure_rand: &mut FastRandom, relay: &mut AddrRelay, addr: &Address) {
    // Known checking here is only to save space from duplicates. Before sending, we'll filter
    // it again for known addresses that were added after addresses were pushed.
    let known = relay.known.as_ref().expect("PushAddress needs m_addr_known");
    if addr.service.addr.is_valid()
        && !known.contains(&addr.service.key())
        && is_addr_compatible(relay, addr)
    {
        if relay.to_send.len() >= MAX_ADDR_TO_SEND {
            let i = insecure_rand.randrange(relay.to_send.len() as u64) as usize;
            relay.to_send[i] = addr.clone();
        } else {
            relay.to_send.push(addr.clone());
        }
    }
}

/// Core's `CServiceHash(0, 0)`: SipHash over the network id, the port and the raw address.
// Core: netaddress.h:584-591 CServiceHash::operator()
fn service_hash(s: &Service) -> u64 {
    SipHasher::new(0, 0)
        .write_u64(s.addr.net() as u64)
        .write_u64(u64::from(s.port))
        .write(s.addr.bytes())
        .finish()
}

/// Core's `RelayAddress`: the same one or two peers for each address within a 24-hour window
/// (rotated per address), chosen by the largest SipHash key among peers with relay enabled,
/// other than the source, that read the address's format.
// Core: net_processing.cpp:2686-2734 PeerManagerImpl::RelayAddress
pub fn relay_address(
    core: &NetCore,
    peers: &PeerMap,
    originator: PeerId,
    addr: &Address,
    reachable: bool,
    now_s: i64,
) {
    if !reachable && !addr.service.addr.is_relayable() {
        return;
    }
    // Use deterministic randomness to send to the same nodes for 24 hours at a time so the
    // m_addr_knowns of the chosen nodes prevent repeats.
    let hash_addr = service_hash(&addr.service);
    // Adding the address hash makes the exact rotation time differ per address, while
    // preserving periodicity.
    let time_addr = (now_s as u64).wrapping_add(hash_addr) / ROTATE_ADDR_RELAY_DEST_INTERVAL_SECS;
    let hasher = core
        .deterministic_randomizer(RANDOMIZER_ID_ADDRESS_RELAY)
        .write_u64(hash_addr)
        .write_u64(time_addr);
    // Relay reachable addresses to 2 peers. Unreachable addresses are relayed randomly to 1 or 2.
    let n = if reachable || hasher.finish() & 1 == 1 {
        2
    } else {
        1
    };

    let map = peers.lock();
    // Core: net_processing.cpp:2708 (one context for both targets)
    let mut insecure_rand = core.new_rng();
    let mut best: [(u64, Option<PeerId>); 2] = [(0, None); 2];
    for (&id, slot) in map.iter() {
        if id == originator {
            continue;
        }
        let eligible = {
            let st = slot.state.lock();
            st.net.addr.enabled && is_addr_compatible(&st.net.addr, addr)
        };
        if !eligible {
            continue;
        }
        let key = hasher.clone().write_u64(id as u64).finish();
        for i in 0..n {
            if key > best[i].0 {
                best.copy_within(i..n - 1, i + 1);
                best[i] = (key, Some(id));
                break;
            }
        }
    }
    for &(key, id) in best.iter().take(n) {
        let Some(id) = id.filter(|_| key != 0) else {
            break;
        };
        if let Some(slot) = map.get(&id) {
            push_address(&mut insecure_rand, &mut slot.state.lock().net.addr, addr);
        }
    }
}

/// `addr` or `addrv2` with `addrs`, in the format the peer asked for.
fn addr_message(wants_addrv2: bool, addrs: &[Address]) -> dash_p2p::OutMsg {
    if wants_addrv2 {
        let msg = AddrV2Msg {
            addrs: addrs
                .iter()
                .map(|a| AddrV2Entry {
                    time: a.time,
                    services: a.services,
                    addr: a.service.addr.to_addrv2(),
                    port: a.service.port,
                })
                .collect(),
        };
        out_msg(ADDRV2, &msg)
    } else {
        let msg = Addr {
            addrs: addrs
                .iter()
                .map(|a| TimestampedAddr {
                    time: a.time,
                    services: a.services,
                    addr: ServiceV1 {
                        addr: AddrV1(a.service.addr.v1_bytes()),
                        port: a.service.port,
                    },
                })
                .collect(),
        };
        out_msg(ADDR, &msg)
    }
}

impl AddressRelay for NodeAddressRelay {
    fn relay(&mut self, core: &NetCore, peers: &PeerMap, from: PeerId, addrs: &[(Address, bool)]) {
        // Core: GetTime<std::chrono::seconds>() in RelayAddress
        let now_s = core.clock().node_now().0;
        for (addr, reachable) in addrs {
            relay_address(core, peers, from, addr, *reachable, now_s);
        }
    }

    // Core: net_processing.cpp:6186-6252 PeerManagerImpl::MaybeSendAddr
    fn send_section(
        &mut self,
        core: &NetCore,
        peer: &PeerHandle,
        relay: &mut AddrRelay,
        our_services: ServiceFlags,
        now_us: i64,
    ) {
        // Nothing to do for non-address-relay peers.
        if !relay.enabled {
            return;
        }
        // Periodically advertise our local address to the peer.
        if core.config().listen && !self.in_ibd(core) && relay.next_local_send_us < now_us {
            // If we've sent before, clear the bloom filter for the peer, so that our
            // self-announcement will actually go out.
            if relay.next_local_send_us != 0 {
                // Core: common/bloom.cpp CRollingBloomFilter::reset (`GetRand<unsigned int>()`)
                let tweak = core.get_rand(u64::from(u32::MAX)) as u32;
                relay.known.as_mut().expect("relay is enabled").reset(tweak);
            }
            if let Some(local) = core.local_addr_for_peer(peer) {
                let local = Address {
                    service: local,
                    services: our_services,
                    time: core.clock().node_now().0 as u32,
                };
                // Core: net_processing.cpp:6206
                push_address(&mut core.new_rng(), relay, &local);
            }
            relay.next_local_send_us = exponential_rand(
                now_us,
                AVG_LOCAL_ADDRESS_BROADCAST_INTERVAL_SECS,
                core.get_rand(1 << 48),
            );
        }

        // We sent an `addr` message to this peer recently. Nothing more to do.
        if now_us <= relay.next_send_us {
            return;
        }
        // Core: random.cpp:771-775 GetExponentialRand (`GetRand(1 << 48)`)
        relay.next_send_us =
            exponential_rand(now_us, AVG_ADDRESS_BROADCAST_INTERVAL_SECS, core.get_rand(1 << 48));
        relay.to_send.truncate(MAX_ADDR_TO_SEND);

        // Remove addr records that the peer already knows about, and add new addrs to the
        // m_addr_known filter on the same pass.
        let known = relay.known.as_mut().expect("relay is enabled");
        relay.to_send.retain(|a| {
            let key = a.service.key();
            let seen = known.contains(&key);
            if !seen {
                known.insert(&key);
            }
            !seen
        });
        // No addr messages to send.
        if relay.to_send.is_empty() {
            return;
        }
        core.push(peer, addr_message(relay.wants_addrv2, &relay.to_send));
        relay.to_send.clear();
    }

    // Core: net_processing.cpp:5400-5424 (GETADDR, inbound)
    fn on_getaddr(&mut self, cx: &mut HandlerCx<'_>) {
        let (core, peer) = (cx.core, cx.peer);
        let relay = &mut cx.state.net.addr;
        // Since this must be an inbound connection, SetupAddressRelay will never fail.
        let enabled = setup_address_relay(core, peer, relay);
        debug_assert!(enabled, "getaddr reaches the relay only from inbound peers");

        // Only send one GetAddr response per connection to reduce resource waste and discourage
        // addr stamping of INV announcements.
        if relay.getaddr_recvd {
            tracing::debug!(target: "net", "Ignoring repeated \"getaddr\". peer={}", peer.id());
            return;
        }
        relay.getaddr_recvd = true;
        relay.to_send.clear();
        let addrs = if peer.has_permission(Permissions::ADDR) {
            get_addresses(core, MAX_ADDR_TO_SEND, MAX_PCT_ADDR_TO_SEND)
        } else {
            self.cached_addresses(core, peer)
        };
        let relay = &mut cx.state.net.addr;
        // Core: net_processing.cpp:5419
        let mut insecure_rand = core.new_rng();
        for addr in &addrs {
            push_address(&mut insecure_rand, relay, addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_p2p::NetAddr;
    use dashd_p2p::fingerprint::{IdentityOptions, local_services};

    /// `CServiceHash(0, 0)` over Core's field order (network id, port, raw bytes): an IPv4
    /// service hashes its four bytes, not the v1 mapping.
    #[test]
    fn service_hash_writes_raw_bytes() {
        let s = Service::new(NetAddr::ipv4([1, 2, 3, 4]), 9999);
        let want = SipHasher::new(0, 0).write_u64(1).write_u64(9999).write(&[1, 2, 3, 4]).finish();
        assert_eq!(service_hash(&s), want);
    }

    /// The reply cache is keyed by `ConnectedThroughNetwork`: inbound peers on one local socket
    /// share a reply, except that an inbound onion peer (`inbound_onion`, set from the onion
    /// binds by the accept loop) gets its own.
    #[test]
    fn onion_inbound_has_its_own_cached_reply() {
        use dash_p2p::store::NoStore;
        use dash_p2p::{Attach, ConnectionType, MemSocket, NetConfig, NetEvents, Network};
        use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};

        struct Quiet;
        impl NetEvents for Quiet {
            fn wake(&self) {}
            fn initialize(&self, _: &NetCore, _: &PeerHandle, _: ServiceFlags) {}
            fn finalize(&self, _: &NetCore, _: &PeerHandle) {}
        }

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let params = Arc::new(
            ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap(),
        );
        let mut cfg = NetConfig::new(Arc::clone(&params), local_services(&IdentityOptions::new()));
        cfg.listen = false;
        cfg.connect = Some(Vec::new());
        let core = NetCore::new(
            cfg,
            rt.handle().clone(),
            Arc::new(NoStore),
            Arc::new(crate::SystemResolver::default()),
            Arc::new(dash_clock::SystemClock),
            Arc::new(Quiet),
        )
        .unwrap();
        let now = core.clock().node_now().0;
        let addrs: Vec<Address> = (0..400u32)
            .map(|i| Address {
                service: Service::new(
                    NetAddr::ipv4([31 + (i / 64) as u8, (i % 64) as u8, 1, 1]),
                    9999,
                ),
                services: ServiceFlags::NODE_NETWORK,
                time: now as u32,
            })
            .collect();
        core.addrman().add(&addrs, &NetAddr::ipv4([250, 1, 1, 1]), 0, now);

        // Tor forwards to the onion bind: every inbound peer arrives from 127.0.0.1 on it.
        let bind = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 9996);
        let mut keep = Vec::new();
        let mut attach = |port: u16, inbound_onion: bool| {
            let (ours, theirs) = MemSocket::pair();
            keep.push(theirs);
            core.attach(Attach {
                sock: Arc::new(ours),
                addr: Address {
                    service: Service::new(NetAddr::ipv4([127, 0, 0, 1]), port),
                    services: ServiceFlags::NONE,
                    time: 0,
                },
                addr_bind: bind.clone(),
                dest: None,
                conn_type: ConnectionType::Inbound,
                inbound_onion,
                permissions: Permissions::NONE,
                use_v2: false,
                grant: None,
                masternode: false,
                masternode_probe: false,
            })
        };
        let onion = attach(40001, true);
        let plain = attach(40002, false);
        let plain2 = attach(40003, false);
        assert_eq!(onion.network(), Network::Onion);
        assert_ne!(plain.network(), Network::Onion);

        let mut relay = NodeAddressRelay::new(dashd_p2p::sim::SimChain::new(params));
        let services = |v: Vec<Address>| v.into_iter().map(|a| a.service).collect::<Vec<_>>();
        let via_onion = services(relay.cached_addresses(&core, &onion));
        let via_plain = services(relay.cached_addresses(&core, &plain));
        assert_eq!(via_plain.len(), core.addrman().size(None, None) * 23 / 100);
        assert_eq!(services(relay.cached_addresses(&core, &plain2)), via_plain);
        assert_ne!(via_onion, via_plain, "the onion peer shares the clearnet reply");
        assert_eq!(relay.caches.len(), 2);
    }
}
