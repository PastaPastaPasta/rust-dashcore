//! Core's `ThreadOpenMasternodeConnections` as a sans-io dial loop
//! ([`DialLoop::MnCon`](crate::DialLoop::MnCon)), and
//! its queues: the pending masternodes (`vPendingMasternodes`) and the probes
//! (`masternodePendingProbes`).
//!
//! The loop is infrastructure only. Which quorum members to connect, the tip's masternode list,
//! the sync state and the per-masternode meta (`CMasternodeMetaMan`) come from the host's
//! [`MnDialPolicy`] ([`DialPolicy::masternodes`]); without one the loop is [`DialPlan::Done`].
//!
//! Core walks its candidates in salted `std::unordered_set` order and picks `GetRand(n)` among
//! them. Here the probes and each quorum group's members are sorted by SipHash-2-4 under a salt
//! drawn once from the node RNG when the loop starts, and the pick is the same `GetRand(n)`:
//! the order is per node random and independent of insertion order, as Core's is, though not
//! the same permutation (DEC-1316). The groups themselves are walked in the policy's order and
//! concatenated with no dedup across groups, so a masternode in `k` groups is `k` candidates,
//! as in Core.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use dash_p2p_core::ServiceFlags;

use crate::conn::ConnectionType;
use crate::dial::{DialPlan, DialPolicy};
use crate::netaddr::{Address, Service};
use crate::netcore::{NETCONN, NetCore, OpenRequest, Target};
use crate::siphash::SipHasher;

/// A masternode the loop may dial: its ProRegTx hash and its primary service
/// (`netInfo->GetPrimary()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MnTarget {
    /// `proTxHash`, in internal byte order.
    pub pro_tx_hash: [u8; 32],
    /// The primary service.
    pub service: Service,
}

/// One pass's view of the masternode list at the chain tip (`GetListAtChainTip`).
pub trait MnTipView: Send + Sync {
    /// `CDeterministicMNList::GetMN`: the masternode, valid or not.
    fn mn(&self, pro_tx_hash: &[u8; 32]) -> Option<MnTarget>;

    /// `CDeterministicMNList::GetValidMN`: the masternode, if it is not PoSe-banned.
    fn valid_mn(&self, pro_tx_hash: &[u8; 32]) -> Option<MnTarget>;
}

/// The host's inputs to [`DialLoop::MnCon`](crate::DialLoop::MnCon): the quorum connection
/// policy (P7), the masternode list, the sync state and the meta counters.
///
/// The loop calls these, and the [`MnTipView`] methods, under the [`NetCore`]'s dial lock. So an
/// implementation must return without waiting and must not call a `NetCore` method that takes
/// that lock (the masternode queue and thread-active methods, the added-node and addr-fetch
/// methods); and no thread may call those while holding a lock these methods take.
pub trait MnDialPolicy: Send + Sync {
    /// `CMasternodeSync::IsBlockchainSynced`.
    fn blockchain_synced(&self) -> bool;

    /// The masternode list at the chain tip, read once per pass.
    fn tip(&self) -> Arc<dyn MnTipView>;

    /// `masternodeQuorumNodes`: one member list per quorum (ProRegTx hashes in internal byte
    /// order), the quorums in `(llmq_type, quorum_hash)` order. A member's order within its list
    /// does not matter, and a member listed twice in one list counts once.
    fn quorum_groups(&self) -> Vec<Vec<[u8; 32]>>;

    /// `CMasternodeMetaMan::GetLastOutboundAttempt`.
    fn last_outbound_attempt(&self, pro_tx_hash: &[u8; 32]) -> i64;

    /// `SetLastOutboundAttempt`: records `t` and counts one more attempt.
    fn set_last_outbound_attempt(&self, pro_tx_hash: &[u8; 32], t: i64);

    /// `SetLastOutboundSuccess`: records `t` and resets the attempt count.
    fn set_last_outbound_success(&self, pro_tx_hash: &[u8; 32], t: i64);

    /// `OutboundFailedTooManyTimes`, a query that changes nothing.
    fn outbound_failed_too_many_times(&self, pro_tx_hash: &[u8; 32]) -> bool;
}

/// The loop's 1 s idle sleep.
const IDLE_SLEEP: Duration = Duration::from_millis(1000);
/// The sleep after a pass that chose a target.
const CONNECT_SLEEP: Duration = Duration::from_millis(100);
/// A connection at a quorum member's address whose first message came longer ago than this, and
/// that has not verified as that masternode, is dropped.
const SLOW_HANDSHAKE_SECS: i64 = 5;
/// `CAddress`'s default `nTime` (`TIME_INIT`).
const TIME_INIT: u32 = 100_000_000;

#[derive(Debug, Default)]
enum MnPhase {
    #[default]
    Start,
    /// Sleep before the next pass.
    Sleep,
    Pass,
    /// The open of `pro_tx_hash` at `service` was planned: check whether it connected.
    Opened {
        pro_tx_hash: [u8; 32],
        service: Service,
    },
    /// Core's thread has returned (`-connect`).
    Finished,
}

/// The masternode loop's state and queues.
#[derive(Debug)]
pub(crate) struct MnCon {
    phase: MnPhase,
    /// `vPendingMasternodes`.
    pending: VecDeque<[u8; 32]>,
    /// `masternodePendingProbes`: a set, kept in insertion order; the loop walks it in salted
    /// order only.
    probes: Vec<[u8; 32]>,
    /// The candidate-order salt, drawn when the loop starts.
    salt: Option<(u64, u64)>,
    /// `didConnect`.
    did_connect: bool,
    /// `m_masternode_thread_active`.
    thread_active: bool,
}

impl Default for MnCon {
    fn default() -> Self {
        Self {
            phase: MnPhase::default(),
            pending: VecDeque::new(),
            probes: Vec::new(),
            salt: None,
            did_connect: false,
            thread_active: true,
        }
    }
}

/// Core's `uint256::ToString`: the bytes reversed, in hex.
fn hash_hex(h: &[u8; 32]) -> String {
    h.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// `hashes` in salted order: by SipHash-2-4 under `salt`, then by value, with repeats removed.
fn salted(salt: (u64, u64), mut hashes: Vec<[u8; 32]>) -> Vec<[u8; 32]> {
    hashes.sort_unstable_by_key(|h| (SipHasher::new(salt.0, salt.1).write(h).finish(), *h));
    hashes.dedup();
    hashes
}

impl NetCore {
    /// Core's `AddPendingMasternode`: queues `pro_tx_hash` for the masternode loop's next
    /// passes, first in first out. False when it is already queued.
    // Core: net.cpp:4368-4377 CConnman::AddPendingMasternode
    pub fn add_pending_masternode(&self, pro_tx_hash: [u8; 32]) -> bool {
        let mut dial = self.dial.lock();
        let mn = &mut dial.mn;
        if mn.pending.contains(&pro_tx_hash) {
            return false;
        }
        mn.pending.push_back(pro_tx_hash);
        true
    }

    /// Core's `AddPendingProbeConnections`: adds masternodes to probe (a set).
    // Core: net.cpp:4508-4512 CConnman::AddPendingProbeConnections
    pub fn add_pending_probes(&self, pro_tx_hashes: &[[u8; 32]]) {
        let mut dial = self.dial.lock();
        let probes = &mut dial.mn.probes;
        for h in pro_tx_hashes {
            if !probes.contains(h) {
                probes.push(*h);
            }
        }
    }

    /// Core's `SetMasternodeThreadActive` (the `setmnthreadactive` RPC): while false, the
    /// masternode loop's passes open nothing. True at start.
    // Core: net.h:1325-1326
    pub fn set_masternode_thread_active(&self, active: bool) {
        self.dial.lock().mn.thread_active = active;
    }

    /// Core's `GetMasternodeThreadActive`.
    pub fn masternode_thread_active(&self) -> bool {
        self.dial.lock().mn.thread_active
    }

    /// Core's `IsMasternodeOrDisconnectRequested`: whether the first connection at `service`,
    /// marked for disconnection or not, is a masternode connection or marked for disconnection.
    /// False when there is none.
    // Core: net.cpp:4961-4966; net.cpp:4940-4945 ForNode; net.h:1710-1719 FindNode
    pub fn is_masternode_or_disconnect_requested(&self, service: &Service) -> bool {
        self.peers()
            .iter()
            .find(|p| p.addr().service == *service)
            .is_some_and(|p| p.masternode_connection() || p.disconnect_requested())
    }

    // Core: net.cpp:3414-3600 CConnman::ThreadOpenMasternodeConnections
    pub(crate) fn tick_mncon(&self, policy: &dyn DialPolicy) -> DialPlan {
        let Some(mn) = policy.masternodes() else {
            return DialPlan::Done;
        };
        let Some(mut dial) = self.live_dial() else {
            return DialPlan::Done;
        };
        let st = &mut dial.mn;
        loop {
            match std::mem::take(&mut st.phase) {
                MnPhase::Start => {
                    // Core: net.cpp:3417-3418 (`-connect` with any value, `-connect=0` too, but
                    // not a bare `-noconnect`)
                    if self.config().connect_listed {
                        st.phase = MnPhase::Finished;
                        return DialPlan::Done;
                    }
                    if st.salt.is_none() {
                        let k0 = self.get_rand(u64::MAX);
                        st.salt = Some((k0, self.get_rand(u64::MAX)));
                    }
                    st.phase = MnPhase::Sleep;
                }
                // Core: net.cpp:3425-3431
                MnPhase::Sleep => {
                    st.phase = MnPhase::Pass;
                    return DialPlan::Sleep(if st.did_connect {
                        CONNECT_SLEEP
                    } else {
                        IDLE_SLEEP
                    });
                }
                MnPhase::Pass => {
                    st.did_connect = false;
                    st.phase = MnPhase::Sleep;
                    // Core: net.cpp:3435
                    if !self.network_active() || !st.thread_active || !mn.blockchain_synced() {
                        continue;
                    }
                    let Some((target, probe, now)) = self.pick_masternode(st, mn) else {
                        if self.interrupted() {
                            return DialPlan::Done;
                        }
                        continue;
                    };
                    // Core: net.cpp:3580-3582 (`nANow`, the pass's time)
                    st.did_connect = true;
                    mn.set_last_outbound_attempt(&target.pro_tx_hash, now);
                    st.phase = MnPhase::Opened {
                        pro_tx_hash: target.pro_tx_hash,
                        service: target.service.clone(),
                    };
                    // Core: net.cpp:3686-3689 OpenMasternodeConnection
                    return DialPlan::Open(OpenRequest {
                        target: Target::Addr(Address {
                            service: target.service,
                            services: ServiceFlags::NODE_NETWORK,
                            time: TIME_INIT,
                        }),
                        conn_type: ConnectionType::OutboundFullRelay,
                        count_failure: false,
                        grant: None,
                        use_v2: self.we_advertise_v2(),
                        masternode: true,
                        masternode_probe: probe,
                    });
                }
                // Core: net.cpp:3585-3598
                MnPhase::Opened {
                    pro_tx_hash,
                    service,
                } => {
                    st.phase = MnPhase::Sleep;
                    let connected = self
                        .peers()
                        .iter()
                        .find(|p| p.addr().service == service)
                        .is_some_and(|p| !p.disconnect_requested());
                    if !connected {
                        let hash = hash_hex(&pro_tx_hash);
                        tracing::debug!(
                            target: NETCONN,
                            "CConnman::ThreadOpenMasternodeConnections -- connection failed for masternode  {hash}, service={service}"
                        );
                        if mn.outbound_failed_too_many_times(&pro_tx_hash) {
                            tracing::debug!(
                                target: NETCONN,
                                "CConnman::ThreadOpenMasternodeConnections -- failed to connect to masternode {hash} too many times"
                            );
                        }
                    }
                }
                MnPhase::Finished => {
                    st.phase = MnPhase::Finished;
                    return DialPlan::Done;
                }
            }
        }
    }

    /// One pass's target, whether it is a probe, and the pass's time (`nANow`): the front
    /// pending masternode, else a quorum member, else a probe. `None` when there is none, or when
    /// the core was interrupted after the tip was read.
    // Core: net.cpp:3437-3576
    fn pick_masternode(
        &self,
        st: &mut MnCon,
        mn: &dyn MnDialPolicy,
    ) -> Option<(MnTarget, bool, i64)> {
        // `ForEachNode`'s default filter, in table order: the first leg of a ProRegTx sets its
        // direction (`emplace`).
        // Core: net.cpp:3437-3444; net.h:1353-1359, :1432-1435; net.cpp:4874-4877
        let mut connected: HashSet<Service> = HashSet::new();
        let mut verified: HashMap<[u8; 32], bool> = HashMap::new();
        for p in
            self.peers().iter().filter(|p| p.successfully_connected() && !p.disconnect_requested())
        {
            connected.insert(p.addr().service.clone());
            let h = p.verified_proreg_hash();
            if h != [0; 32] {
                verified.entry(h).or_insert(p.is_inbound());
            }
        }
        let tip = mn.tip();
        if self.interrupted() {
            return None;
        }
        let now = self.clock().node_now().0;
        let retry = i64::from(self.config().params.llmq_connection_retry_timeout);
        let salt = st.salt.expect("drawn at the loop's start");
        // Core's later lookups under `m_nodes_mutex`: `FindNode(addr, false)`, the first
        // connection at the address, marked or not.
        let nodes = self.peers();
        let at = |s: &Service| nodes.iter().find(|p| p.addr().service == *s).cloned();

        // Core: net.cpp:3538-3553
        if let Some(h) = st.pending.pop_front()
            && let Some(dmn) = tip.valid_mn(&h)
            && !connected.contains(&dmn.service)
            && at(&dmn.service)
                .is_none_or(|p| !p.masternode_connection() && !p.disconnect_requested())
        {
            tracing::debug!(
                target: NETCONN,
                "CConnman::ThreadOpenMasternodeConnections -- opening pending masternode connection to {}, service={}",
                hash_hex(&dmn.pro_tx_hash),
                dmn.service
            );
            return Some((dmn, false, now));
        }

        // Core: net.cpp:3458-3502 getPendingQuorumNodes, :3554-3560
        let mut quorum = Vec::new();
        for group in mn.quorum_groups() {
            for h in salted(salt, group) {
                if verified.contains_key(&h) {
                    continue;
                }
                let Some(dmn) = tip.mn(&h) else {
                    continue;
                };
                let node = at(&dmn.service);
                if node
                    .as_ref()
                    .is_some_and(|p| p.masternode_connection() || p.disconnect_requested())
                {
                    continue;
                }
                if connected.contains(&dmn.service) {
                    if let Some(p) = node {
                        let first = p.first_message_time();
                        if first != 0 && self.clock().node_now().0 - first > SLOW_HANDSHAKE_SECS {
                            tracing::debug!(
                                target: NETCONN,
                                "CConnman::ThreadOpenMasternodeConnections -- dropping non-mnauth connection to {}, service={}",
                                hash_hex(&h),
                                dmn.service
                            );
                            p.mark_disconnect();
                        }
                    }
                    continue;
                }
                if now - mn.last_outbound_attempt(&h) < retry {
                    continue;
                }
                quorum.push(dmn);
            }
        }
        if !quorum.is_empty() {
            let dmn = quorum.swap_remove(self.get_rand(quorum.len() as u64) as usize);
            tracing::debug!(
                target: NETCONN,
                "CConnman::ThreadOpenMasternodeConnections -- opening quorum connection to {}, service={}",
                hash_hex(&dmn.pro_tx_hash),
                dmn.service
            );
            return Some((dmn, false, now));
        }

        // Core: net.cpp:3504-3531 getPendingProbes, :3562-3570
        let mut probes = Vec::new();
        for h in salted(salt, st.probes.clone()) {
            let Some(dmn) = tip.mn(&h) else {
                st.probes.retain(|p| *p != h);
                continue;
            };
            if verified.get(&h) == Some(&false) {
                mn.set_last_outbound_success(&h, now);
                st.probes.retain(|p| *p != h);
                continue;
            }
            if now - mn.last_outbound_attempt(&h) < retry {
                continue;
            }
            probes.push(dmn);
        }
        if probes.is_empty() {
            return None;
        }
        let dmn = probes.swap_remove(self.get_rand(probes.len() as u64) as usize);
        st.probes.retain(|p| *p != dmn.pro_tx_hash);
        tracing::debug!(
            target: NETCONN,
            "CConnman::ThreadOpenMasternodeConnections -- probing masternode {}, service={}",
            hash_hex(&dmn.pro_tx_hash),
            dmn.service
        );
        Some((dmn, true, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The salted order depends only on the set: the same members in any order, or repeated,
    /// sort the same, and another salt gives another order.
    #[test]
    fn the_salted_order_ignores_insertion_order_and_repeats() {
        let hs: Vec<[u8; 32]> = (1..=6u8).map(|i| [i; 32]).collect();
        let mut rev = hs.clone();
        rev.reverse();
        rev.push(hs[2]);
        assert_eq!(salted((1, 2), hs.clone()), salted((1, 2), rev));
        assert_eq!(salted((1, 2), hs.clone()).len(), 6);
        assert_ne!(salted((1, 2), hs.clone()), salted((3, 4), hs));
    }
}
