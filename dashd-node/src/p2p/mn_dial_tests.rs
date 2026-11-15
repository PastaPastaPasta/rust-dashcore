//! P7's node wiring of the masternode connection pieces: step 12's dial policy.

use std::collections::HashSet;
use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_dml::testkit::{dmn_with, pro_tx};
use dash_dml::{MnList, MnType};
use dash_p2p::{ConnectionType, DialLoop, NetAddr, Service, Target};
use dash_params::{ChainParamsBuilder, LlmqType, Network as CoreNetwork, Release};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
use dashd_masternode::sync::NodeSyncNotifier;
use dashd_masternode::tip::TipListRef;
use dashd_net::sim::{DialEvent, DialSim};
use dashd_p2p::sim::{SIM_START, VirtualClock};

use super::*;

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

fn svc(n: u8) -> Service {
    Service::new(NetAddr::ipv4([31, 9, 0, n]), 9999)
}

/// The tip list with masternodes `ns`, each at [`svc`]`(n)`.
fn tip(ns: &[u8]) -> TipMnList {
    let mut list = MnList::default_pre_dip3();
    for &n in ns {
        list.insert_for_tests(dmn_with(n, MnType::Regular, |_, cold| {
            cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
                AddrV1::from([31, 9, 0, n]),
                svc(n).port,
            ))));
        }));
    }
    TipMnList::fixed(TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    })
}

fn synced() -> Arc<MasternodeSync> {
    let sync = Arc::new(MasternodeSync::new(VirtualClock::at(SIM_START), Box::new(Quiet)));
    sync.switch_to_next_asset();
    sync
}

/// P7 W2: step 12's dial policy carries `NodeMnDialPolicy` with the masternode services, over
/// the node's registry and `mn_metaman`: the masternode loop opens a masternode connection to
/// the registered quorum member's primary service and stamps its outbound attempt. Without
/// `mn_sync` the loop has no inputs and ends, as before.
// Core: init.cpp:2757; net.cpp:3458-3502, :3538-3560 at v24.0.0
#[test]
fn p7_w2_node_dial_policy_dials_a_registered_quorum_member() {
    let params =
        Arc::new(ChainParamsBuilder::new(CoreNetwork::Main, Release::V24_0).build().unwrap());
    let mut sim = DialSim::new(params, |cfg| {
        cfg.dns_seed = false;
        cfg.fixed_seeds = false;
    });
    let registry = Arc::new(QuorumNodes::default());
    let metaman = Arc::new(MasternodeMetaMan::new());
    let flags = Arc::new(OutboundFlags::default());
    let tip = tip(&[1, 2]);

    let plain = node_dial_policy(Arc::clone(&flags), &tip, None, &registry, &metaman);
    assert!(plain.masternodes().is_none());
    assert!(!sim.run(DialLoop::MnCon, &*plain, 4));
    assert!(sim.exec.requests.is_empty());

    let sync = synced();
    let policy = node_dial_policy(flags, &tip, Some(&sync), &registry, &metaman);
    assert!(policy.masternodes().is_some());
    let member = *pro_tx(2).as_bytes();
    registry.set_nodes(LlmqType::LlmqTest, [5; 32], HashSet::from([member]));
    sim.run(DialLoop::MnCon, &*policy, 4);
    let opened: Vec<_> = sim
        .exec
        .events
        .iter()
        .filter_map(|e| match e {
            DialEvent::Opened {
                id,
                service,
                conn_type: ConnectionType::OutboundFullRelay,
            } => Some((*id, service.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(opened.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(), [svc(2)]);
    assert!(sim.core.peer(opened[0].0).unwrap().masternode_connection());
    let request = &sim.exec.requests[0];
    assert!(request.masternode && !request.masternode_probe);
    assert!(matches!(&request.target, Target::Addr(a) if a.service == svc(2)));
    // The pass after the loop's first 1 s sleep.
    assert_eq!(metaman.last_outbound_attempt(&pro_tx(2)), SIM_START + 1);
    assert_eq!(metaman.last_outbound_attempt(&pro_tx(1)), 0);
}
