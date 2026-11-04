//! Outbound eviction and the stale tip (P2a §3.9): `ConsiderEviction` (`SendMessages` section
//! 11), `EvictExtraOutboundPeers` and `CheckForStaleTipAndEvictPeers` (every 45 s on the actor),
//! and the two ConnMan flags they set, which SB6's dial loop reads through [`DialPolicy`].

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use dash_p2p::dial::DialPolicy;
use dash_p2p::{ConnectionType, NetCore, PeerHandle, Why};

use super::logic::PeerLogic;
use super::peer::PeerState;
use super::seams::ChainSnapshot;
use super::sync::{getheaders_type, is_outbound_or_block_relay, maybe_send_getheaders, work_of};

/// Core's `CHAIN_SYNC_TIMEOUT`: 20 minutes, in seconds.
pub const CHAIN_SYNC_TIMEOUT: i64 = 20 * 60;
/// Core's `HEADERS_RESPONSE_TIME`, in seconds.
pub const HEADERS_RESPONSE_TIME: i64 = 2 * 60;
/// Core's `STALE_CHECK_INTERVAL`: 150 s.
pub const STALE_CHECK_INTERVAL: i64 = 150;
/// Core's `EXTRA_PEER_CHECK_INTERVAL`: the `CheckForStaleTipAndEvictPeers` cadence.
pub const EXTRA_PEER_CHECK_INTERVAL: Duration = Duration::from_secs(45);
/// Core's `MINIMUM_CONNECT_TIME`, in seconds.
pub const MINIMUM_CONNECT_TIME: i64 = 30;

/// ConnMan's flags that the processor sets and SB6's dial loop reads, and `m_last_tip_update`,
/// which the chain side's `BlockConnected` stamps.
#[derive(Debug, Default)]
pub struct OutboundFlags {
    try_another_outbound_peer: AtomicBool,
    start_extra_block_relay_peers: AtomicBool,
    /// `m_last_tip_update` in seconds; 0 until set (`TipMayBeStale` initialises it lazily).
    last_tip_update: AtomicI64,
}

impl OutboundFlags {
    /// Core's `SetTryNewOutboundPeer`.
    // Core: net.cpp:2832-2836
    pub fn set_try_new_outbound_peer(&self, flag: bool) {
        self.try_another_outbound_peer.store(flag, Ordering::SeqCst);
        tracing::debug!(target: "net", "setting try another outbound peer={flag}");
    }

    /// Core's `StartExtraBlockRelayPeers`.
    // Core: net.cpp:2838-2842
    pub fn start_extra_block_relay_peers(&self) {
        tracing::debug!(target: "net", "enabling extra block-relay-only peers");
        self.start_extra_block_relay_peers.store(true, Ordering::SeqCst);
    }

    /// `m_last_tip_update = GetTime()` (Core's `BlockConnected`).
    pub fn set_last_tip_update(&self, now: i64) {
        self.last_tip_update.store(now, Ordering::SeqCst);
    }

    /// `m_last_tip_update` in seconds (0 until set).
    pub fn last_tip_update(&self) -> i64 {
        self.last_tip_update.load(Ordering::SeqCst)
    }
}

impl DialPolicy for OutboundFlags {
    fn try_another_outbound_peer(&self) -> bool {
        self.try_another_outbound_peer.load(Ordering::SeqCst)
    }

    fn start_extra_block_relay_peers(&self) -> bool {
        self.start_extra_block_relay_peers.load(Ordering::SeqCst)
    }
}

/// Core's `NodeFullyConnected`: what `ForEachNode` and `ForNode` visit.
pub fn fully_connected(p: &PeerHandle) -> bool {
    p.successfully_connected() && !p.disconnect_requested()
}

impl PeerLogic {
    /// Section 11: an outbound peer whose chain stays behind our tip is asked once more, then
    /// disconnected (P2a §3.9.1). `now` is `GetTime()` seconds.
    // Core: net_processing.cpp:5949-6005 PeerManagerImpl::ConsiderEviction
    pub(super) fn consider_eviction(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        snap: &ChainSnapshot,
        now: i64,
    ) {
        if st.chain_sync.protect || !is_outbound_or_block_relay(peer) || !st.sync_started {
            return;
        }
        let chain = &*self.chain;
        let core = &*self.core;
        let best_known = st.headers().best_known_block;
        let best_work = work_of(chain, best_known);
        let cs = &mut st.chain_sync;
        if best_work.is_some_and(|w| w >= snap.tip.tip_work) {
            if cs.timeout != 0 {
                cs.timeout = 0;
                cs.work_header = None;
                cs.sent_getheaders = false;
            }
        } else if cs.timeout == 0
            || best_work.is_some_and(|w| work_of(chain, cs.work_header).is_some_and(|h| w >= h))
        {
            cs.timeout = now + CHAIN_SYNC_TIMEOUT;
            cs.work_header = Some(snap.tip.tip);
            cs.sent_getheaders = false;
        } else if cs.timeout > 0 && now > cs.timeout {
            let id = peer.id();
            let best_hash = best_known
                .and_then(|b| chain.entry(b))
                .map_or_else(|| "<none>".to_owned(), |e| e.hash.to_string());
            if cs.sent_getheaders {
                tracing::info!(target: "net", "Disconnecting outbound peer {id} for old chain, best known block = {best_hash}");
                core.disconnect(id, Why::Processor("old chain"));
            } else {
                // Core asserts the work header is set.
                let Some(work_header) = cs.work_header.and_then(|w| chain.entry(w)) else {
                    debug_assert!(false, "ConsiderEviction without a work header");
                    return;
                };
                // A genesis work header has no parent: v24's empty locator (G2).
                maybe_send_getheaders(core, peer, st, chain.locator(work_header.prev));
                tracing::debug!(target: "net", "sending {} to outbound peer={id} to verify chain work (current best known block:{best_hash}, benchmark blockhash: {})", getheaders_type(peer), work_header.hash);
                st.chain_sync.sent_getheaders = true;
                st.chain_sync.timeout = now + HEADERS_RESPONSE_TIME;
            }
        }
    }

    /// Core's `EvictExtraOutboundPeers` (P2a §3.9.2).
    // Core: net_processing.cpp:6007-6119 PeerManagerImpl::EvictExtraOutboundPeers
    pub fn evict_extra_outbound_peers(&mut self, now: i64) {
        let core = &*self.core;
        let cfg = core.config();
        let nodes = core.peers();
        let in_flight = |id| self.seams.block_sync.blocks_in_flight(id);

        // Core: net.cpp:2868-2880 GetExtraBlockRelayCount
        let block_relay: Vec<&PeerHandle> = nodes
            .iter()
            .filter(|p| fully_connected(p) && p.conn_type() == ConnectionType::BlockRelay)
            .collect();
        if block_relay.len() > cfg.max_outbound_block_relay {
            let (mut youngest, mut next) = ((-1, 0), (-1, 0));
            for p in &block_relay {
                if p.id() > youngest.0 {
                    next = youngest;
                    youngest = (p.id(), p.last_block_time());
                }
            }
            // The youngest gave us a block more recently: disconnect the second youngest.
            let to = if youngest.1 > next.1 {
                next.0
            } else {
                youngest.0
            };
            if let Some(p) = block_relay.iter().find(|p| p.id() == to) {
                let has_state = self.peers.lock().contains_key(&to);
                if !has_state || (now - p.connected() >= MINIMUM_CONNECT_TIME && in_flight(to) == 0)
                {
                    core.disconnect(to, Why::Processor("extra block-relay-only peer"));
                    tracing::debug!(target: "net", "disconnecting extra block-relay-only peer={to} (last block received at time {})", p.last_block_time());
                } else {
                    tracing::debug!(target: "net", "keeping block-relay-only peer={to} chosen for eviction (connect time: {}, blocks_in_flight: {})", p.connected(), in_flight(to));
                }
            }
        }

        // Core: net.cpp:2850-2866 GetExtraFullOutboundCount
        let full = nodes
            .iter()
            .filter(|p| {
                !p.masternode_connection()
                    && fully_connected(p)
                    && p.conn_type() == ConnectionType::OutboundFullRelay
                    && !p.masternode_probe()
            })
            .count();
        if full <= cfg.max_outbound_full_relay {
            return;
        }
        let mut worst: Option<(&PeerHandle, i64)> = None;
        let mut onion_count = 0;
        // A second `ForEachNode`: a block-relay peer disconnected above is skipped.
        for p in nodes.iter().filter(|p| fully_connected(p)) {
            if p.addr().service.addr.is_tor() {
                onion_count += 1;
                if onion_count <= cfg.max_outbound_onion {
                    continue;
                }
            }
            if p.masternode_connection() || p.conn_type() != ConnectionType::OutboundFullRelay {
                continue;
            }
            let Some(state) = self.state_of(p.id()) else {
                continue;
            };
            let (protect, announced) = {
                let st = state.lock();
                (st.chain_sync.protect, st.last_block_announcement)
            };
            // Protected, or the only manual or full-relay connection on its network (Core keys
            // the count by `addr.GetNetwork()`, not `ConnectedThroughNetwork()`).
            let net = p.addr().service.addr.network(cfg.allow_private_net);
            if protect || core.network_count(net) <= 1 {
                continue;
            }
            if worst.is_none_or(|(w, oldest)| {
                announced < oldest || (announced == oldest && p.id() > w.id())
            }) {
                worst = Some((p, announced));
            }
        }
        let Some((p, oldest)) = worst else {
            return;
        };
        let id = p.id();
        if now - p.connected() > MINIMUM_CONNECT_TIME && in_flight(id) == 0 {
            tracing::debug!(target: "net", "disconnecting extra outbound peer={id} (last block announcement received at time {oldest})");
            core.disconnect(id, Why::Processor("extra outbound peer"));
            // Don't try more extra peers until the next stale tip (PQ-11).
            self.flags.set_try_new_outbound_peer(false);
        } else {
            tracing::debug!(target: "net", "keeping outbound peer={id} chosen for eviction (connect time: {}, blocks_in_flight: {})", p.connected(), in_flight(id));
        }
    }

    /// Core's `CheckForStaleTipAndEvictPeers`, which the scheduler runs every 45 s; here the
    /// actor runs it at the first pass after it is due (P2a §3.9.3, §3.12).
    // Core: net_processing.cpp:6121-6146 PeerManagerImpl::CheckForStaleTipAndEvictPeers
    pub fn check_for_stale_tip_and_evict_peers(&mut self) {
        let now = self.core.clock().node_now().0;
        self.evict_extra_outbound_peers(now);
        if now > self.stale_tip_check_time {
            let core = &*self.core;
            if !self.chain.loading_blocks()
                && core.network_active()
                && core.config().connect.is_none()
                && self.tip_may_be_stale(now)
            {
                let last = self.flags.last_tip_update.load(Ordering::SeqCst);
                tracing::info!(target: "net", "Potential stale tip detected, will try using extra outbound peer (last tip update: {} seconds ago)", now - last);
                self.flags.set_try_new_outbound_peer(true);
            } else if self.flags.try_another_outbound_peer() {
                self.flags.set_try_new_outbound_peer(false);
            }
            self.stale_tip_check_time = now + STALE_CHECK_INTERVAL;
        }
        if !self.initial_sync_finished && self.can_direct_fetch() {
            self.flags.start_extra_block_relay_peers();
            self.initial_sync_finished = true;
        }
    }

    /// `m_stale_tip_check_time` in seconds: when `CheckForStaleTipAndEvictPeers` next looks at
    /// the tip (0 before its first run).
    pub fn stale_tip_check_time(&self) -> i64 {
        self.stale_tip_check_time
    }

    /// `m_initial_sync_finished`.
    pub fn initial_sync_finished(&self) -> bool {
        self.initial_sync_finished
    }

    /// ConnMan's flags and `m_last_tip_update`.
    pub fn flags(&self) -> &OutboundFlags {
        &self.flags
    }

    /// Core's `TipMayBeStale`.
    // Core: net_processing.cpp:1418-1426
    fn tip_may_be_stale(&self, now: i64) -> bool {
        let last = &self.flags.last_tip_update;
        if last.load(Ordering::SeqCst) == 0 {
            last.store(now, Ordering::SeqCst);
        }
        let spacing = self.core.config().params.consensus.pow_target_spacing;
        last.load(Ordering::SeqCst) < now - spacing * 3 && !self.seams.block_sync.any_in_flight()
    }

    /// Core's `CanDirectFetch` on the last published snapshot (the scheduler holds none).
    pub(super) fn can_direct_fetch(&self) -> bool {
        can_direct_fetch(&self.core, &self.chain.snapshot())
    }
}

/// Core's `CanDirectFetch` at `snap` (v24: `NodeClock::now()`): the tip is less than 20 block
/// intervals old.
// Core: net_processing.cpp:1428-1431
pub(super) fn can_direct_fetch(core: &NetCore, snap: &ChainSnapshot) -> bool {
    let tip_time_us = i64::from(snap.tip.tip_time) * 1_000_000;
    let spacing = core.config().params.consensus.pow_target_spacing;
    tip_time_us > core.clock().node_now_micros() - spacing * 20 * 1_000_000
}
