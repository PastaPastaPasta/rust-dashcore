//! Core's `SyncManager` (`node/sync_manager.{h,cpp}`): the tick that drives
//! [`MasternodeSync`] and issues the sync requests every node sends (FR-P2P-13, PRD behaviour
//! 8): `mempool` to outbound peers once the blockchain is synced, `govsync(0, empty bloom)` to
//! one more peer per tick while governance syncs, then per-object vote requests. Each is
//! recorded per peer address in [`NetFulfilledRequestManager`]. Core schedules the tick every
//! second (init step 10a); its 6 s gate and timeouts run on `GetTime()`, the mockable clock.
//!
//! At v24.0.0-rc.4 the tick no longer sends `getsporks` (`"spork-sync"`): sporks are announced
//! to each peer at `verack` instead (Core a56ed47c98a; SP1 D7).
//!
//! The tick reads the peers through [`SyncNet`] (the network core in the node, a fake in tests)
//! and governance through [`GovernanceSync`], GV1b's seam; until GV1b, [`EmptyGovernance`] is
//! Core's governance manager with no objects (SP1 D8). Core's function-local statics (`nTick`,
//! `nTimeLastProcess`, the `allow-sync` key, `mapAskedRecently`, the vote-progress counters) are
//! the manager's fields: there is one manager per node, as there is one process.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use dash_clock::Clock;
use dash_netaddr::netaddr::Service;
use dash_p2p_core::FilterLoad;
use parking_lot::Mutex;

use crate::meta::GovHash;
use crate::netfulfilled::{NetFulfilledRequestManager, request};
use crate::sync::{
    MASTERNODE_SYNC_BLOCKCHAIN, MASTERNODE_SYNC_GOVERNANCE, MASTERNODE_SYNC_TICK_SECONDS,
    MASTERNODE_SYNC_TIMEOUT_SECONDS, MasternodeSync, NodeSyncNotifier,
};

/// `MAX_INV_SZ`.
// Core: net_processing.h MAX_INV_SZ
pub const MAX_INV_SZ: usize = 50_000;

/// `REALNET_PEERS_PER_HASH`: peers asked for one object's votes on mainnet, testnet and devnets.
// Core: node/sync_manager.cpp:55
pub const REALNET_PEERS_PER_HASH: usize = 3;
/// `REGTEST_PEERS_PER_HASH`: every peer, on a mockable chain.
// Core: node/sync_manager.cpp:60
pub const REGTEST_PEERS_PER_HASH: usize = usize::MAX;

/// The votes one request is projected to bring (`nProjectedVotes`).
// Core: node/sync_manager.cpp:75
const PROJECTED_VOTES: usize = 2000;

/// How long a peer is not asked again for one object's votes (`nTimeout`).
// Core: node/sync_manager.cpp:65
const ASKED_TIMEOUT: i64 = 60 * 60;

/// `mapAskedRecently`: per governance object, the peers asked for its votes and when that
/// expires.
pub type AskedRecently = BTreeMap<GovHash, BTreeMap<Service, i64>>;

/// A fully connected peer as the tick sees it (`CConnman::NodesSnapshot(FullyConnectedOnly)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPeer {
    pub id: i64,
    /// `pnode->addr`: the netfulfilled key.
    pub addr: Service,
    /// `CanRelay()`: not a masternode connection, or an intra-quorum relay one.
    pub can_relay: bool,
    /// `IsInboundConn()`.
    pub inbound: bool,
    /// `HasPermission(NetPermissionFlags::NoBan) || IsManualConn()`: the peer skips the
    /// netfulfilled restrictions.
    pub unrestricted: bool,
    /// `IsBlockRelayOnly()`: a block-relay-only connection, or `-blocksonly` without `relay`.
    pub block_relay_only: bool,
}

/// A message the tick sends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncMsg {
    /// `MNGOVERNANCESYNC(nHash, filter)`.
    GovSync {
        hash: GovHash,
        filter: FilterLoad,
    },
    /// `MEMPOOL`.
    Mempool,
}

/// What the tick reaches of the connection manager and the peer manager.
pub trait SyncNet: Send + Sync {
    /// `NodesSnapshot(connman, FullyConnectedOnly)`, in the connection table's order.
    fn fully_connected(&self) -> Vec<SyncPeer>;
    /// The address of every peer, fully connected or not (`ForEachNode(AllNodes, …)`).
    fn all_addrs(&self) -> Vec<Service>;
    /// `PushMessage(pnode, msg)`.
    fn push(&self, peer: i64, msg: SyncMsg);
    /// `pnode->fDisconnect = true`.
    fn disconnect(&self, peer: i64);
    /// `PeerGetRequestedObjectCount(id)`: the peer's announcements in the object tracker.
    fn requested_object_count(&self, peer: i64) -> usize;
    /// `IsActiveMasternode()`.
    fn is_active_masternode(&self) -> bool;
    /// `Shuffle(…, FastRandomContext())`.
    fn shuffle(&self, hashes: &mut [GovHash]);
}

/// The governance manager as the sync sees it: GV1b's seam (SP1 D8). Every method is Core's
/// `CGovernanceManager` method of that name.
pub trait GovernanceSync: Send + Sync {
    /// `IsValid()`: the governance cache loaded (governance is on).
    fn is_valid(&self) -> bool;
    /// `GetVoteCount()`.
    fn vote_count(&self) -> i32;
    /// `GetMNManager().GetListAtChainTip().GetCounts().enabled()`.
    fn enabled_masternode_count(&self) -> i32;
    /// `FetchGovernanceObjectVotes(nPeersPerHashMax, nNow, mapAskedRecently)`: the trigger and
    /// other objects whose votes to request, in `mapObjects` order; it drops the expired entries
    /// of each object it visits from `asked`.
    fn fetch_governance_object_votes(
        &self,
        peers_per_hash_max: usize,
        now: i64,
        asked: &mut AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>);
    /// `GetVoteBloomFilter(nHash)`.
    fn vote_bloom_filter(&self, hash: &GovHash) -> FilterLoad;
}

/// `CBloomFilter{}`: no data, no hash functions, tweak 0, flags 0.
pub fn empty_bloom_filter() -> FilterLoad {
    FilterLoad {
        data: Vec::new(),
        hash_funcs: 0,
        tweak: 0,
        flags: 0,
    }
}

/// Core's governance manager holding nothing: valid when governance is on (its empty cache
/// loaded), with no objects and no votes (SP1 D8). GV1b replaces it.
#[derive(Clone, Copy, Debug)]
pub struct EmptyGovernance {
    /// `!-disablegovernance` (Core's `is_governance_enabled`, which loads the cache).
    pub valid: bool,
}

impl GovernanceSync for EmptyGovernance {
    fn is_valid(&self) -> bool {
        self.valid
    }

    fn vote_count(&self) -> i32 {
        0
    }

    fn enabled_masternode_count(&self) -> i32 {
        0
    }

    fn fetch_governance_object_votes(
        &self,
        _peers_per_hash_max: usize,
        _now: i64,
        _asked: &mut AskedRecently,
    ) -> (Vec<GovHash>, Vec<GovHash>) {
        (Vec::new(), Vec::new())
    }

    fn vote_bloom_filter(&self, _hash: &GovHash) -> FilterLoad {
        empty_bloom_filter()
    }
}

/// Core's `NodeSyncNotifierImpl`.
pub struct FullSyncNotifier {
    pub net: Arc<dyn SyncNet>,
    pub netfulfilled: Arc<NetFulfilledRequestManager>,
}

impl NodeSyncNotifier for FullSyncNotifier {
    // Core: node/sync_manager.cpp:334-337 (the GUI's progress only)
    fn sync_reset(&self) {}

    // Core: node/sync_manager.cpp:339-347
    fn sync_finished(&self) {
        assert!(self.netfulfilled.is_valid(), "netfulfilled loads before the sync can finish");
        for addr in self.net.all_addrs() {
            self.netfulfilled.add_fulfilled_request(&addr, request::FULL_SYNC);
        }
    }
}

/// The network facts the tick reads from `Params()` and `gArgs`.
#[derive(Clone, Copy, Debug)]
pub struct SyncConfig {
    /// `Params().IsMockableChain()` (regtest).
    pub mockable_chain: bool,
    /// `Params().NetworkIDString() == CBaseChainParams::MAIN`.
    pub mainnet: bool,
    /// `-syncmempool` (default `DEFAULT_SYNC_MEMPOOL`, true).
    pub sync_mempool: bool,
}

/// `ProcessTick`'s and `RequestGovernanceObjectVotes`' function-local statics.
#[derive(Debug, Default)]
struct TickState {
    /// `nTick`.
    tick: i32,
    /// `strAllow`: `"allow-sync-<ms>"` at the first tick, system time (not mockable).
    allow: Option<String>,
    /// `nTimeLastProcess`, set at the first tick.
    time_last_process: Option<i64>,
    /// `nTimeNoObjectsLeft`.
    time_no_objects_left: i64,
    /// `nLastTick`.
    last_tick: i32,
    /// `nLastVotes`.
    last_votes: i32,
    /// `mapAskedRecently`.
    asked_recently: AskedRecently,
}

/// Core's `SyncManager` (its tick; the `ssc` handler is `sync_net`'s).
pub struct SyncManager {
    clock: Arc<dyn Clock>,
    cfg: SyncConfig,
    sync: Arc<MasternodeSync>,
    netfulfilled: Arc<NetFulfilledRequestManager>,
    net: Arc<dyn SyncNet>,
    gov: Arc<dyn GovernanceSync>,
    state: Mutex<TickState>,
}

fn system_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

impl SyncManager {
    pub fn new(
        clock: Arc<dyn Clock>,
        cfg: SyncConfig,
        sync: Arc<MasternodeSync>,
        netfulfilled: Arc<NetFulfilledRequestManager>,
        net: Arc<dyn SyncNet>,
        gov: Arc<dyn GovernanceSync>,
    ) -> SyncManager {
        SyncManager {
            clock,
            cfg,
            sync,
            netfulfilled,
            net,
            gov,
            state: Mutex::default(),
        }
    }

    fn now(&self) -> i64 {
        self.clock.node_now().0
    }

    /// The sync state the tick drives.
    pub fn sync(&self) -> &Arc<MasternodeSync> {
        &self.sync
    }

    /// `Schedule`'s callback, every second: nothing once a shutdown was requested.
    // Core: node/sync_manager.cpp:22-30 SyncManager::Schedule
    pub fn scheduled_tick(&self, shutdown_requested: bool) {
        if shutdown_requested {
            return;
        }
        self.process_tick();
    }

    /// `SendGovernanceSyncRequest`: `govsync(0, empty bloom)`.
    // Core: node/sync_manager.cpp:32-37
    fn send_governance_sync_request(&self, peer: &SyncPeer) {
        self.net.push(
            peer.id,
            SyncMsg::GovSync {
                hash: [0; 32],
                filter: empty_bloom_filter(),
            },
        );
    }

    /// `SendGovernanceObjectSyncRequest(pnode, nHash, fUseFilter = true)`.
    // Core: node/sync_manager.cpp:39-49
    fn send_governance_object_sync_request(&self, peer: &SyncPeer, hash: GovHash) {
        tracing::debug!(
            target: "gobject",
            "SyncManager::SendGovernanceObjectSyncRequest -- nHash {} peer={}",
            hash_hex(&hash),
            peer.id
        );
        let filter = self.gov.vote_bloom_filter(&hash);
        self.net.push(
            peer.id,
            SyncMsg::GovSync {
                hash,
                filter,
            },
        );
    }

    /// Core's `!pnode->CanRelay() || (IsActiveMasternode() && pnode->IsInboundConn())`: a
    /// connection the sync never asks (an outbound masternode connection, or on a masternode
    /// an inbound one, most likely another masternode's).
    fn skips(&self, peer: &SyncPeer) -> bool {
        !peer.can_relay || (self.net.is_active_masternode() && peer.inbound)
    }

    /// `RequestGovernanceObjectVotes(vNodesCopy)`: asks peers for the votes of up to
    /// `nMaxObjRequestsPerNode` objects, triggers first; the objects left, -1 without peers, -2
    /// with nothing to ask.
    // Core: node/sync_manager.cpp:51-136
    fn request_governance_object_votes(&self, st: &mut TickState, nodes: &[SyncPeer]) -> i32 {
        if nodes.is_empty() {
            return -1;
        }
        let now = self.now();
        let peers_per_hash_max = if self.cfg.mockable_chain {
            REGTEST_PEERS_PER_HASH
        } else {
            REALNET_PEERS_PER_HASH
        };
        let mut max_obj_requests_per_node = 1usize;
        if !self.cfg.mainnet {
            let enabled = usize::try_from(self.gov.enabled_masternode_count().max(1)).unwrap_or(1);
            max_obj_requests_per_node = (PROJECTED_VOTES / enabled).max(1);
        }
        let (mut triggers, mut others) =
            self.gov.fetch_governance_object_votes(peers_per_hash_max, now, &mut st.asked_recently);
        if triggers.is_empty() && others.is_empty() {
            return -2;
        }
        tracing::debug!(
            target: "gobject",
            "RequestGovernanceObjectVotes -- start: vTriggerObjHashes {} vOtherObjHashes {} mapAskedRecently {}",
            triggers.len(),
            others.len(),
            st.asked_recently.len()
        );
        self.net.shuffle(&mut triggers);
        self.net.shuffle(&mut others);
        let mut i = 0;
        while i < max_obj_requests_per_node {
            // Ask for triggers first.
            let Some(&hash) = triggers.last().or(others.last()) else {
                break;
            };
            let mut asked = false;
            for peer in nodes {
                if self.skips(peer) {
                    continue;
                }
                // Stop early to keep the peer's requests under `MAX_INV_SZ`.
                if self.net.requested_object_count(peer.id) + PROJECTED_VOTES > MAX_INV_SZ {
                    continue;
                }
                // Core's `mapAskedRecently[nHashGovobj]` makes the object's entry here.
                let peers = st.asked_recently.entry(hash).or_default();
                if peers.contains_key(&peer.addr) {
                    continue;
                }
                self.send_governance_object_sync_request(peer, hash);
                let peers = st.asked_recently.entry(hash).or_default();
                peers.insert(peer.addr.clone(), now + ASKED_TIMEOUT);
                asked = true;
                if peers.len() >= peers_per_hash_max {
                    break;
                }
            }
            if triggers.is_empty() {
                others.pop();
            } else {
                triggers.pop();
            }
            if asked {
                i += 1;
            }
        }
        tracing::debug!(
            target: "gobject",
            "RequestGovernanceObjectVotes -- end: vTriggerObjHashes {} vOtherObjHashes {} mapAskedRecently {}",
            triggers.len(),
            others.len(),
            st.asked_recently.len()
        );
        i32::try_from(triggers.len() + others.len()).unwrap_or(i32::MAX)
    }

    /// `ProcessTick()`.
    // Core: node/sync_manager.cpp:138-317
    pub fn process_tick(&self) {
        assert!(self.netfulfilled.is_valid(), "netfulfilled loads before the tick runs");
        let mut st = self.state.lock();
        st.tick += 1;
        let tick = st.tick;
        let allow =
            st.allow.get_or_insert_with(|| format!("allow-sync-{}", system_millis())).clone();

        // Reset the sync if the last tick was more than an hour ago (the machine slept).
        let last_process = *st.time_last_process.get_or_insert(self.now());
        if !self.cfg.mockable_chain
            && self.now() - last_process > 60 * 60
            && !self.net.is_active_masternode()
        {
            tracing::info!("Sync Tick -- WARNING: no actions for too long, restarting sync...");
            self.sync.reset(true, true);
            st.time_last_process = Some(self.now());
            return;
        }
        if self.now() - last_process < MASTERNODE_SYNC_TICK_SECONDS {
            // Too early, nothing to do here.
            return;
        }
        st.time_last_process = Some(self.now());
        let snap = self.net.fully_connected();

        // Gradually request the rest of the votes after the sync finished.
        if self.sync.is_synced() {
            self.request_governance_object_votes(&mut st, &snap);
            return;
        }

        let attempt = self.sync.attempt();
        let asset_id = self.sync.asset_id();
        let progress = f64::from(attempt + (asset_id - 1) * 8) / f64::from(8 * 4);
        tracing::debug!(
            target: "mnsync",
            "Sync Tick -- nTick {tick} asset_id {asset_id} nTriedPeerCount {attempt} nSyncProgress {progress:.6}"
        );

        let nf = &self.netfulfilled;
        for peer in &snap {
            if self.skips(peer) {
                continue;
            }
            if peer.unrestricted && !nf.has_fulfilled_request(&peer.addr, &allow) {
                nf.remove_all_fulfilled_requests(&peer.addr);
                nf.add_fulfilled_request(&peer.addr, &allow);
                tracing::info!("Sync Tick -- skipping mnsync restrictions for peer={}", peer.id);
            }
            if nf.has_fulfilled_request(&peer.addr, request::FULL_SYNC) {
                // Fully synced from this peer recently: free its slot for another.
                self.net.disconnect(peer.id);
                tracing::info!("Sync Tick -- disconnecting from recently synced peer={}", peer.id);
                continue;
            }

            if asset_id == MASTERNODE_SYNC_BLOCKCHAIN {
                let timeout = if snap.len() > 3 {
                    MASTERNODE_SYNC_TICK_SECONDS
                } else {
                    MASTERNODE_SYNC_TIMEOUT_SECONDS
                };
                // At the best header, with no block, tip or header for the timeout: at the tip.
                if self.sync.is_reached_best_header()
                    && self.now() - self.sync.last_bump() > timeout
                {
                    self.sync.switch_to_next_asset();
                    if self.cfg.sync_mempool {
                        // The blockchain is synced: ask the outbound peers for their mempools.
                        for other in &snap {
                            let earlier =
                                nf.has_fulfilled_request(&other.addr, request::MEMPOOL_SYNC);
                            if !other.inbound && !earlier && !other.block_relay_only {
                                nf.add_fulfilled_request(&other.addr, request::MEMPOOL_SYNC);
                                self.net.push(other.id, SyncMsg::Mempool);
                                tracing::debug!(
                                    target: "mnsync",
                                    "Sync Tick -- nTick {tick} asset_id {asset_id} -- syncing mempool from peer={}",
                                    other.id
                                );
                            }
                        }
                    }
                }
            }

            if asset_id == MASTERNODE_SYNC_GOVERNANCE {
                if !self.gov.is_valid() {
                    self.sync.switch_to_next_asset();
                    return;
                }
                let (last_bump, now) = (self.sync.last_bump(), self.now());
                tracing::debug!(
                    target: "gobject",
                    "Sync Tick -- nTick {tick} asset_id {asset_id} last_bump {last_bump} GetTime() {now} diff {}",
                    now - last_bump
                );
                // Check for the timeout first.
                if self.now() - self.sync.last_bump() > MASTERNODE_SYNC_TIMEOUT_SECONDS {
                    tracing::debug!(target: "mnsync", "Sync Tick -- nTick {tick} asset_id {asset_id} -- timeout");
                    if attempt == 0 {
                        tracing::info!(
                            "Sync Tick -- WARNING: failed to sync {}",
                            self.sync.asset_name()
                        );
                    }
                    self.sync.switch_to_next_asset();
                    return;
                }
                // Only one object sync request per peer; the votes per object follow below.
                if nf.has_fulfilled_request(&peer.addr, request::GOVERNANCE_SYNC) {
                    continue;
                }
                nf.add_fulfilled_request(&peer.addr, request::GOVERNANCE_SYNC);
                self.sync.bump_attempt();
                self.send_governance_sync_request(peer);
                // One request per tick, for each peer in turn.
                break;
            }
        }

        if asset_id != MASTERNODE_SYNC_GOVERNANCE {
            return;
        }

        // Request the votes per object from each peer.
        for peer in &snap {
            if !nf.has_fulfilled_request(&peer.addr, request::GOVERNANCE_SYNC) {
                continue;
            }
            let left = self.request_governance_object_votes(&mut st, std::slice::from_ref(peer));
            if left != 0 {
                continue;
            }
            if st.time_no_objects_left == 0 {
                // Asked every object for votes for the first time.
                st.time_no_objects_left = self.now();
            }
            // Check the condition below once per tick.
            if st.last_tick == tick {
                continue;
            }
            let votes = self.gov.vote_count();
            // C++'s `int(0.0001 * nLastVotes)` truncates toward zero.
            let floor = ((0.0001 * f64::from(st.last_votes)) as i32)
                .max(MASTERNODE_SYNC_TICK_SECONDS as i32);
            if self.now() - st.time_no_objects_left > MASTERNODE_SYNC_TIMEOUT_SECONDS
                && votes - st.last_votes < floor
            {
                // Everything asked, waited the timeout, and fewer than 0.01% (or one per second)
                // votes arrived in the last tick: done.
                tracing::info!(
                    "Sync Tick -- nTick {tick} asset_id {MASTERNODE_SYNC_GOVERNANCE} -- asked for all objects, nothing to do"
                );
                st.time_no_objects_left = 0;
                self.sync.switch_to_next_asset();
                return;
            }
            st.last_tick = tick;
            st.last_votes = self.gov.vote_count();
        }
    }
}

#[cfg(test)]
mod tests;

/// `uint256::ToString()`: the bytes reversed, in hex.
fn hash_hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}
