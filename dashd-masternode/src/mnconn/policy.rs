//! The production masternode dial policy.

use std::sync::Arc;

use dash_dml::ProTxHash;
use dash_netaddr::{NetAddr, Service};
use dash_p2p::{DialPolicy, MnDialPolicy, MnTarget, MnTipView};
use dash_primitives::NITrait;

use crate::meta::MasternodeMetaMan;
use crate::mnconn::QuorumNodes;
use crate::sync::MasternodeSync;
use crate::tip::{TipListRef, TipMnList};

/// Core's masternode-connection inputs gathered behind the [`MnDialPolicy`] seam.
///
/// Every method is called while the network core's masternode dial lock is held. The sync state
/// is atomic, the tip source is a wait-free snapshot, and the registry and metadata managers are
/// leaf locks; no method re-enters the network core or takes a lock above either manager.
pub struct NodeMnDialPolicy {
    tip_mn_list: TipMnList,
    sync: Arc<MasternodeSync>,
    quorum_nodes: Arc<QuorumNodes>,
    meta: Arc<MasternodeMetaMan>,
}

impl NodeMnDialPolicy {
    /// Creates a policy over the node's published tip, sync state, quorum registry and metadata.
    pub fn new(
        tip_mn_list: TipMnList,
        sync: Arc<MasternodeSync>,
        quorum_nodes: Arc<QuorumNodes>,
        meta: Arc<MasternodeMetaMan>,
    ) -> Self {
        Self {
            tip_mn_list,
            sync,
            quorum_nodes,
            meta,
        }
    }
}

struct NodeMnTip {
    tip: TipListRef,
}

impl NodeMnTip {
    fn target(&self, pro_tx_hash: &[u8; 32], valid: bool) -> Option<MnTarget> {
        let pro_tx_hash = ProTxHash::from(*pro_tx_hash);
        let dmn = if valid {
            self.tip.list.get_valid(&pro_tx_hash)
        } else {
            self.tip.list.get(&pro_tx_hash)
        }?;
        let service =
            dmn.cold.net_info.primary().map(|primary| {
                Service::new(NetAddr::from_addrv2(primary.addr()), primary.port())
            })?;
        Some(MnTarget {
            pro_tx_hash: *dmn.pro_tx_hash.as_bytes(),
            service,
        })
    }
}

impl MnTipView for NodeMnTip {
    // Core: net.cpp:3538-3541; deterministicmns.h:333-339 at v24.0.0
    fn mn(&self, pro_tx_hash: &[u8; 32]) -> Option<MnTarget> {
        self.target(pro_tx_hash, false)
    }

    // Core: net.cpp:3540-3542; deterministicmns.h:341-347 at v24.0.0
    fn valid_mn(&self, pro_tx_hash: &[u8; 32]) -> Option<MnTarget> {
        self.target(pro_tx_hash, true)
    }
}

impl MnDialPolicy for NodeMnDialPolicy {
    // Core: masternode/sync.cpp:89-92 at v24.0.0
    fn blockchain_synced(&self) -> bool {
        self.sync.is_blockchain_synced()
    }

    // Core: deterministicmns.cpp:941-947 at v24.0.0
    fn tip(&self) -> Arc<dyn MnTipView> {
        Arc::new(NodeMnTip {
            tip: self.tip_mn_list.load(),
        })
    }

    // Core: net.cpp:3458-3502 at v24.0.0
    fn quorum_groups(&self) -> Vec<Vec<[u8; 32]>> {
        self.quorum_nodes.quorum_groups()
    }

    // Core: masternode/meta.cpp:163-167 at v24.0.0
    fn last_outbound_attempt(&self, pro_tx_hash: &[u8; 32]) -> i64 {
        self.meta.last_outbound_attempt(&ProTxHash::from(*pro_tx_hash))
    }

    // Core: masternode/meta.cpp:151-155 at v24.0.0
    fn set_last_outbound_attempt(&self, pro_tx_hash: &[u8; 32], t: i64) {
        self.meta.set_last_outbound_attempt(&ProTxHash::from(*pro_tx_hash), t);
    }

    // Core: masternode/meta.cpp:157-161 at v24.0.0
    fn set_last_outbound_success(&self, pro_tx_hash: &[u8; 32], t: i64) {
        self.meta.set_last_outbound_success(&ProTxHash::from(*pro_tx_hash), t);
    }

    // Core: masternode/meta.cpp:175-179 at v24.0.0
    fn outbound_failed_too_many_times(&self, pro_tx_hash: &[u8; 32]) -> bool {
        self.meta.outbound_failed_too_many_times(&ProTxHash::from(*pro_tx_hash))
    }
}

impl DialPolicy for NodeMnDialPolicy {
    fn masternodes(&self) -> Option<&dyn MnDialPolicy> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use dash_consensus::BlsCtx;
    use dash_dml::testkit::{dmn_with, pro_tx};
    use dash_dml::{MnList, MnType};
    use dash_netaddr::{NetAddr, Service};
    use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
    use dashd_p2p::sim::VirtualClock;

    use super::*;
    use crate::sync::NodeSyncNotifier;
    use crate::tip::TipSource;

    struct Quiet;

    impl NodeSyncNotifier for Quiet {
        fn sync_reset(&self) {}
        fn sync_finished(&self) {}
    }

    fn service(n: u8) -> Service {
        Service::new(NetAddr::ipv4([127, 0, 0, n]), 19_899)
    }

    fn tip_ref(n: u8) -> TipListRef {
        tip_ref_many(&[n])
    }

    fn tip_ref_many(ns: &[u8]) -> TipListRef {
        let mut list = MnList::default_pre_dip3();
        for &n in ns {
            let entry = dmn_with(n, MnType::Regular, |_, cold| {
                let service = service(n);
                cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
                    AddrV1::from([127, 0, 0, n]),
                    service.port,
                ))));
            });
            list.insert_for_tests(entry);
        }
        TipListRef {
            list: Arc::new(list),
            bls: BlsCtx::BASIC,
        }
    }

    fn policy(tip: TipMnList) -> NodeMnDialPolicy {
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(1_700_000_000), Box::new(Quiet)));
        sync.switch_to_next_asset();
        NodeMnDialPolicy::new(
            tip,
            sync,
            Arc::new(QuorumNodes::default()),
            Arc::new(MasternodeMetaMan::new()),
        )
    }

    #[test]
    fn core_policy_tip_reads_one_snapshot_per_pass() {
        struct Source {
            current: Mutex<TipListRef>,
            loads: AtomicUsize,
        }

        impl TipSource for Source {
            fn load(&self) -> TipListRef {
                self.loads.fetch_add(1, Ordering::SeqCst);
                self.current.lock().unwrap().clone()
            }
        }

        let first = tip_ref(1);
        let second = tip_ref(2);
        let source = Arc::new(Source {
            current: Mutex::new(first),
            loads: AtomicUsize::new(0),
        });
        let tip = TipMnList::new(source.clone());
        let policy = policy(tip);
        let view = policy.tip();
        *source.current.lock().unwrap() = second;
        assert_eq!(source.loads.load(Ordering::SeqCst), 1);
        assert_eq!(view.mn(pro_tx(1).as_bytes()).unwrap().service, service(1));
        assert_eq!(view.valid_mn(pro_tx(1).as_bytes()).unwrap().service, service(1));
        assert!(view.mn(pro_tx(2).as_bytes()).is_none());
    }

    #[test]
    fn core_policy_meta_table() {
        let pro = pro_tx(7);
        let policy = policy(TipMnList::fixed(tip_ref(1)));
        assert_eq!(policy.last_outbound_attempt(pro.as_bytes()), 0);
        assert!(!policy.outbound_failed_too_many_times(pro.as_bytes()));
        for t in 1..=5 {
            policy.set_last_outbound_attempt(pro.as_bytes(), t);
            assert_eq!(policy.last_outbound_attempt(pro.as_bytes()), t);
            assert!(!policy.outbound_failed_too_many_times(pro.as_bytes()));
        }
        policy.set_last_outbound_attempt(pro.as_bytes(), 6);
        assert!(policy.outbound_failed_too_many_times(pro.as_bytes()));
        policy.set_last_outbound_success(pro.as_bytes(), 7);
        assert!(!policy.outbound_failed_too_many_times(pro.as_bytes()));
    }

    #[test]
    fn core_policy_quorum_groups_are_type_hash_ordered() {
        let registry = Arc::new(QuorumNodes::default());
        let low = dash_params::LlmqType::LlmqTest;
        let high = dash_params::LlmqType::Llmq50_60;
        registry.set_nodes(high, [2; 32], HashSet::from([[20; 32]]));
        registry.set_nodes(low, [3; 32], HashSet::from([[30; 32]]));
        registry.set_nodes(low, [1; 32], HashSet::from([[10; 32]]));
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(1_700_000_000), Box::new(Quiet)));
        let policy = NodeMnDialPolicy::new(
            TipMnList::fixed(tip_ref(1)),
            sync,
            registry,
            Arc::new(MasternodeMetaMan::new()),
        );
        assert_eq!(policy.quorum_groups(), vec![vec![[20; 32]], vec![[10; 32]], vec![[30; 32]]]);
    }

    // Core: net.cpp:3458-3502; deterministicmns.h:333-339 at v24.0.0
    #[test]
    fn core_policy_replays_quorum_registry_sets_to_targets() {
        let registry = Arc::new(QuorumNodes::default());
        let hash = |n| *pro_tx(n).as_bytes();
        registry.set_nodes(
            dash_params::LlmqType::LlmqTest,
            [2; 32],
            HashSet::from([hash(10), hash(11)]),
        );
        registry.set_nodes(dash_params::LlmqType::LlmqTest, [1; 32], HashSet::from([hash(12)]));
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(1_700_000_000), Box::new(Quiet)));
        let policy = NodeMnDialPolicy::new(
            TipMnList::fixed(tip_ref_many(&[10, 11, 12])),
            sync,
            registry,
            Arc::new(MasternodeMetaMan::new()),
        );

        let groups = policy.quorum_groups();
        assert_eq!(groups.len(), 2);
        let tip = policy.tip();
        let mut targets: Vec<_> = groups
            .into_iter()
            .flatten()
            .filter_map(|member| tip.mn(&member))
            .map(|target| target.service)
            .collect();
        targets.sort();
        assert_eq!(targets, vec![service(10), service(11), service(12)]);
    }

    #[test]
    fn core_policy_leaf_lock_calls_complete_under_contention() {
        let registry = Arc::new(QuorumNodes::default());
        let meta = Arc::new(MasternodeMetaMan::new());
        let sync = Arc::new(MasternodeSync::new(VirtualClock::at(1_700_000_000), Box::new(Quiet)));
        let policy = Arc::new(NodeMnDialPolicy::new(
            TipMnList::fixed(tip_ref(1)),
            sync,
            registry.clone(),
            meta.clone(),
        ));
        let workers = (0..4)
            .map(|n| {
                let policy = Arc::clone(&policy);
                let registry = Arc::clone(&registry);
                let meta = Arc::clone(&meta);
                std::thread::spawn(move || {
                    let hash = [n; 32];
                    for _ in 0..100 {
                        registry.set_nodes(
                            dash_params::LlmqType::LlmqTest,
                            hash,
                            HashSet::from([hash]),
                        );
                        let _ = policy.quorum_groups();
                        policy.set_last_outbound_attempt(&hash, 1);
                        let _ = policy.last_outbound_attempt(&hash);
                        meta.set_last_outbound_success(&ProTxHash::from(hash), 2);
                        let _ = policy.outbound_failed_too_many_times(&hash);
                    }
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().unwrap();
        }
    }
}
