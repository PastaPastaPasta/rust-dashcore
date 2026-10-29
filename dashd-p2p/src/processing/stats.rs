//! `getpeerinfo`'s processor fields: Core's `CNodeStateStats` (P2a §2.3 `stats.rs`). The
//! connection fields (`CNodeStats`) are SB6's.

use dash_consensus::BlockId;
use dash_p2p::{PeerHandle, ServiceFlags};

use super::peer::{PeerHeaders, PeerState};
use super::seams::ChainAccess;

/// Core's `CNodeStateStats`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeStateStats {
    /// `nSyncHeight`: the best known block's height, −1 when none.
    pub sync_height: i32,
    /// `nCommonHeight`: the last common block's height, −1 when none (P3 maintains it).
    pub common_height: i32,
    /// `vHeightInFlight`: the heights of P3's blocks in flight, oldest first.
    pub heights_in_flight: Vec<i32>,
    /// `m_misbehavior_score`.
    pub misbehavior_score: i32,
    /// `their_services`.
    pub their_services: ServiceFlags,
    /// `m_starting_height`.
    pub starting_height: i32,
    /// `m_ping_wait` in µs: how long the outstanding ping has waited, 0 when none.
    pub ping_wait_us: i64,
    /// `m_relay_txs`.
    pub relay_txs: bool,
    /// `m_addr_processed`.
    pub addr_processed: u64,
    /// `m_addr_rate_limited`.
    pub addr_rate_limited: u64,
    /// `m_addr_relay_enabled`.
    pub addr_relay_enabled: bool,
}

/// Core's `GetNodeStateStats` for `peer`, whose state is `st` and header fields `headers`, at
/// `now_us` (`GetTime<µs>()`), read under the peer's state lock and header cell. The heights are left at -1, and the two blocks behind them
/// (`pindexBestKnownBlock`, `pindexLastCommonBlock`) are returned for [`resolve_heights`] to read
/// from the chain once the lock is released: a state lock is never held across a chain read
/// that a stats caller makes (D66).
// Core: net_processing.cpp:1877-1920 PeerManagerImpl::GetNodeStateStats
pub fn node_state_stats(
    peer: &PeerHandle,
    st: &PeerState,
    headers: &PeerHeaders,
    now_us: i64,
) -> (NodeStateStats, [Option<BlockId>; 2]) {
    let ping = &st.net.ping;
    let addr = &st.net.addr;
    let stats = NodeStateStats {
        sync_height: -1,
        common_height: -1,
        heights_in_flight: st.heights_in_flight.lock().clone(),
        misbehavior_score: st.misbehavior.lock().score,
        their_services: peer.version_services(),
        starting_height: st.net.starting_height,
        ping_wait_us: if ping.nonce_sent != 0 && ping.start_us != 0 {
            now_us - ping.start_us
        } else {
            0
        },
        relay_txs: st.tx_relay && st.relay_txs,
        addr_processed: addr.processed,
        addr_rate_limited: addr.rate_limited,
        addr_relay_enabled: addr.enabled,
    };
    (stats, [headers.best_known_block, st.last_common_block])
}

/// Fills [`node_state_stats`]' `sync_height` and `common_height` from the chain.
pub fn resolve_heights(
    chain: &dyn ChainAccess,
    stats: &mut NodeStateStats,
    [best_known, last_common]: [Option<BlockId>; 2],
) {
    let height = |b: Option<_>| b.and_then(|b| chain.entry(b)).map_or(-1, |e| e.height as i32);
    stats.sync_height = height(best_known);
    stats.common_height = height(last_common);
}
