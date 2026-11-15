use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::BlsCtx;
use dash_dml::testkit::{dmn_with, pro_tx};
use dash_dml::{MnList, MnType};
use dash_netaddr::{NetAddr, Service};
use dash_p2p::{ConnectionType, Permissions};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1};
use dashd_masternode::sync::{MasternodeSync, NodeSyncNotifier};
use dashd_masternode::tip::{TipListRef, TipMnList, TipSource};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::{MnListView, QuorumWatch};
use dashd_p2p::sim::{SIM_START, Sim};

use super::*;

struct Quiet;

impl NodeSyncNotifier for Quiet {
    fn sync_reset(&self) {}

    fn sync_finished(&self) {}
}

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn peer() -> (Sim, dashd_p2p::sim::SimPeer) {
    let params = params();
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let sim = Sim::new(params, identity);
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    (sim, peer)
}

fn tip_for(service: &Service, pro_reg_tx_hash: [u8; 32]) -> TipMnList {
    let dmn = dmn_with(1, MnType::Regular, |dmn, cold| {
        dmn.pro_tx_hash = pro_reg_tx_hash.into();
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(service.addr.v1_bytes()),
            service.port,
        ))));
    });
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(dmn);
    TipMnList::fixed(TipListRef {
        list: Arc::new(list),
        bls: BlsCtx::BASIC,
    })
}

fn sync() -> Arc<MasternodeSync> {
    Arc::new(MasternodeSync::new(dashd_p2p::sim::VirtualClock::at(SIM_START), Box::new(Quiet)))
}

struct RecordingWatch {
    watching: bool,
    quorum_node: bool,
    seen: Mutex<Vec<Option<[u8; 32]>>>,
}

impl QuorumWatch for RecordingWatch {
    fn is_watching(&self) -> bool {
        self.watching
    }

    fn is_quorum_node(&self, peer: &PeerHandle, tip: &dyn MnListView) -> bool {
        self.seen.lock().unwrap().push(tip.by_service(&peer.addr().service));
        self.quorum_node
    }
}

fn recording_watch(watching: bool, quorum_node: bool) -> Arc<RecordingWatch> {
    Arc::new(RecordingWatch {
        watching,
        quorum_node,
        seen: Mutex::new(Vec::new()),
    })
}

#[test]
fn blockchain_synced_follows_masternode_sync() {
    let sync = sync();
    let net = NodeMasternodeNet::new(
        tip_for(&Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899), *pro_tx(1).as_bytes()),
        Arc::clone(&sync),
    );
    assert!(!net.blockchain_synced());
    sync.switch_to_next_asset();
    assert!(net.blockchain_synced());
    sync.reset(true, false);
    assert!(!net.blockchain_synced());
}

#[test]
fn wants_qwatch_does_not_call_quorum_watch_when_not_watching() {
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let watch = recording_watch(false, true);
    let net = NodeMasternodeNet::with_watch(
        tip_for(&service, *pro_tx(1).as_bytes()),
        sync(),
        Arc::clone(&watch) as Arc<dyn QuorumWatch>,
    );
    let (_sim, peer) = peer();
    assert!(!net.wants_qwatch(&peer.handle));
    assert_eq!(watch.seen.lock().unwrap().len(), 0);
}

#[test]
fn wants_qwatch_returns_true_for_a_watched_quorum_node() {
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let watch = recording_watch(true, true);
    let net = NodeMasternodeNet::with_watch(
        tip_for(&service, *pro_tx(1).as_bytes()),
        sync(),
        Arc::clone(&watch) as Arc<dyn QuorumWatch>,
    );
    let (_sim, peer) = peer();
    assert!(net.wants_qwatch(&peer.handle));
    assert_eq!(watch.seen.lock().unwrap().len(), 1);
    let watch = recording_watch(true, false);
    let net =
        NodeMasternodeNet::with_watch(tip_for(&service, *pro_tx(1).as_bytes()), sync(), watch);
    assert!(!net.wants_qwatch(&peer.handle));
}

#[test]
fn quorum_watch_receives_the_tip_view_for_an_unverified_outbound_peer() {
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let pro_reg_tx_hash = *pro_tx(1).as_bytes();
    let watch = recording_watch(true, true);
    let net = NodeMasternodeNet::with_watch(
        tip_for(&service, pro_reg_tx_hash),
        sync(),
        Arc::clone(&watch) as Arc<dyn QuorumWatch>,
    );
    let (_sim, peer) = peer();
    assert_eq!(peer.handle.addr().service, Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899));
    assert!(!peer.handle.is_inbound());
    assert_eq!(peer.handle.verified_proreg_hash(), [0; 32]);
    assert!(net.wants_qwatch(&peer.handle));
    assert_eq!(*watch.seen.lock().unwrap(), [Some(pro_reg_tx_hash)]);
}

struct CountingTip {
    tip: TipListRef,
    loads: AtomicUsize,
}

impl TipSource for CountingTip {
    fn load(&self) -> TipListRef {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.tip.clone()
    }
}

#[test]
fn tip_list_and_qwatch_each_load_one_publication() {
    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let source = Arc::new(CountingTip {
        tip: tip_for(&service, *pro_tx(1).as_bytes()).load(),
        loads: AtomicUsize::new(0),
    });
    let watch = recording_watch(true, true);
    let net = NodeMasternodeNet::with_watch(TipMnList::new(source.clone()), sync(), watch.clone());
    let view = net.tip_list();
    assert_eq!(source.loads.load(Ordering::SeqCst), 1);
    assert_eq!(view.by_service(&service), Some(*pro_tx(1).as_bytes()));
    assert!(!view.bls_legacy_scheme());
    let (_sim, peer) = peer();
    assert!(net.wants_qwatch(&peer.handle));
    assert_eq!(source.loads.load(Ordering::SeqCst), 2);
    assert_eq!(*watch.seen.lock().unwrap(), [Some(*pro_tx(1).as_bytes())]);
}

#[test]
fn default_watch_and_empty_list_are_inactive() {
    let tip = TipMnList::fixed(TipListRef {
        list: Arc::new(MnList::default_pre_dip3()),
        bls: BlsCtx::BASIC,
    });
    let net = NodeMasternodeNet::new(tip, sync());
    let (_sim, peer) = peer();
    assert!(!net.wants_qwatch(&peer.handle));
    let empty = dashd_p2p::processing::seams::NoMasternodeNet.tip_list();
    assert_eq!(empty.by_service(&peer.handle.addr().service), None);
    assert!(!NoQuorumWatch.is_watching());
    assert!(!NoQuorumWatch.is_quorum_node(&peer.handle, &*empty));
}

#[test]
fn sim_sends_qwatch_after_senddsq_with_the_node_seam() {
    use dashd_p2p::processing::seams::MasternodeSeams;
    use dashd_p2p::sim::PeerVersionMsg;
    use dashd_p2p::{ProcessorOptions, Registry, Seams};

    let service = Service::new(NetAddr::ipv4([10, 0, 0, 1]), 19_899);
    let watch = recording_watch(true, true);
    let net = NodeMasternodeNet::with_watch(
        tip_for(&service, *pro_tx(1).as_bytes()),
        sync(),
        watch.clone(),
    );
    let params = params();
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with_masternode(
        params,
        identity,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        MasternodeSeams {
            net: Arc::new(net),
            signer: None,
        },
        |_| {},
    );
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let mut sent = sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    while sim.pass() {}
    sent.extend(sim.sent(&peer));
    let commands: Vec<_> = sent.iter().map(|(cmd, _)| cmd.as_str()).collect();
    let dsq = commands.iter().position(|cmd| *cmd == "senddsq").unwrap();
    let qwatch = commands.iter().position(|cmd| *cmd == "qwatch").unwrap();
    assert!(dsq < qwatch, "{commands:?}");
    assert_eq!(*watch.seen.lock().unwrap(), [Some(*pro_tx(1).as_bytes())]);
}
