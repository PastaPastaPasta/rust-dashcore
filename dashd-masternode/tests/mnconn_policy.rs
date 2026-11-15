use std::sync::Arc;

use dash_consensus::BlsCtx;
use dash_dml::testkit::{dmn_with, pro_tx};
use dash_dml::{MnList, MnType};
use dash_netaddr::{NetAddr, Service};
use dash_p2p::DialLoop;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
use dashd_masternode::meta::MasternodeMetaMan;
use dashd_masternode::mnconn::{NodeMnDialPolicy, QuorumNodes};
use dashd_masternode::sync::{MasternodeSync, NodeSyncNotifier};
use dashd_masternode::tip::{TipListRef, TipMnList};
use dashd_net::sim::{DialEvent, DialSim, SIM_START};

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}
    fn sync_finished(&self) {}
}

fn service(n: u8) -> Service {
    Service::new(NetAddr::ipv4([127, 0, 0, n]), 19_899)
}

fn tip_ref(n: u8) -> TipListRef {
    let entry = dmn_with(n, MnType::Regular, |_, cold| {
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from([127, 0, 0, n]),
            19_899,
        ))));
    });
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(entry);
    TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    }
}

#[test]
fn sim_mncon_real_policy_selects_pending_masternode() {
    let params = Arc::new(ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap());
    let mut sim = DialSim::new(params, |config| {
        config.dns_seed = false;
        config.fixed_seeds = false;
    });
    let sync = Arc::new(MasternodeSync::new(sim.clock.clone(), Box::new(Quiet)));
    sync.switch_to_next_asset();
    let policy = NodeMnDialPolicy::new(
        TipMnList::fixed(tip_ref(1)),
        sync,
        Arc::new(QuorumNodes::default()),
        Arc::new(MasternodeMetaMan::new()),
    );
    sim.core.add_pending_masternode(*pro_tx(1).as_bytes());

    sim.run(DialLoop::MnCon, &policy, 2);

    assert!(
        matches!(sim.exec.events.as_slice(), [DialEvent::Slept(_), DialEvent::Opened { service: opened_service, .. }] if opened_service == &service(1))
    );
}

#[test]
fn sim_mncon_real_policy_uses_simulation_time_for_attempts() {
    let params = Arc::new(ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap());
    let mut sim = DialSim::new(params, |config| {
        config.dns_seed = false;
        config.fixed_seeds = false;
    });
    let sync = Arc::new(MasternodeSync::new(sim.clock.clone(), Box::new(Quiet)));
    sync.switch_to_next_asset();
    let meta = Arc::new(MasternodeMetaMan::new());
    let policy = NodeMnDialPolicy::new(
        TipMnList::fixed(tip_ref(1)),
        sync,
        Arc::new(QuorumNodes::default()),
        meta.clone(),
    );
    sim.core.add_pending_masternode(*pro_tx(1).as_bytes());

    sim.run(DialLoop::MnCon, &policy, 2);

    assert_eq!(meta.last_outbound_attempt(&pro_tx(1)), SIM_START + 1);
}
