//! SG1-W1-6: `qsigrec` intake through [`SigningSeam`], `AlreadyHave` and `getdata` serving of
//! recovered signatures, the `FinalizeNode` purge, and SIG-18's live relay on a non-masternode.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use dash_evo::locks::RecoveredSig;
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, ConnectionType, PeerId, Why};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::seams::SigningSeam;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams, Shared};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    Verify(PeerId, [u8; 32], bool),
    Remove(PeerId),
}

struct RecordingSigning {
    params: Arc<ChainSetup>,
    have: Mutex<HashSet<[u8; 32]>>,
    active: Mutex<HashMap<[u8; 32], RecoveredSig>>,
    events: Mutex<Vec<Event>>,
    relay_on_verify: OnceLock<Arc<Shared>>,
}

impl RecordingSigning {
    fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl SigningSeam for RecordingSigning {
    fn llmq_configured(&self, llmq_type: LlmqType) -> bool {
        self.params.consensus.llmq(llmq_type).is_some()
    }

    fn verify_and_process(&self, peer: PeerId, sig: RecoveredSig, legacy: bool) {
        self.events.lock().unwrap().push(Event::Verify(peer, sig.hash(), legacy));
        if let Some(shared) = self.relay_on_verify.get() {
            shared.relay_recovered_sig(&sig, true);
        }
    }

    fn remove_node(&self, peer: PeerId) {
        self.events.lock().unwrap().push(Event::Remove(peer));
    }

    fn already_have(&self, hash: &[u8; 32]) -> bool {
        self.have.lock().unwrap().contains(hash)
    }

    fn for_getdata(&self, hash: &[u8; 32]) -> Option<RecoveredSig> {
        self.active.lock().unwrap().get(hash).cloned()
    }
}

fn make_sim() -> (Sim, Arc<RecordingSigning>) {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let seam = Arc::new(RecordingSigning {
        params: Arc::clone(&params),
        have: Mutex::default(),
        active: Mutex::default(),
        events: Mutex::default(),
        relay_on_verify: OnceLock::new(),
    });
    let seams = Seams {
        signing: Arc::clone(&seam) as _,
        ..Seams::default()
    };
    let sim =
        Sim::with(params, identity, ProcessorOptions::default(), Registry::new(), seams, |_| {});
    assert!(!sim.shared.identity().masternode);
    (sim, seam)
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

fn connect_peer(sim: &mut Sim, n: u8) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    settle(sim);
    sim.sent(&peer);
    peer
}

fn subscribe(sim: &mut Sim, peer: &SimPeer, wants: bool) {
    sim.deliver(peer, &[("qsendrecsigs", vec![u8::from(wants)])]);
    settle(sim);
}

fn score(sim: &Sim, peer: &SimPeer) -> i32 {
    sim.shared.misbehavior(peer.handle.id()).unwrap().0
}

fn recsig(llmq_type: LlmqType, n: u8) -> RecoveredSig {
    RecoveredSig {
        llmq_type,
        quorum_hash: QuorumHash::from([n; 32]),
        id: [n.wrapping_add(1); 32],
        msg_hash: [n.wrapping_add(2); 32],
        sig: [n.wrapping_add(3); 96],
    }
}

fn wire(sig: &RecoveredSig) -> Vec<u8> {
    let mut out = vec![sig.llmq_type.to_base()];
    out.extend_from_slice(sig.quorum_hash.as_bytes());
    out.extend_from_slice(&sig.id);
    out.extend_from_slice(&sig.msg_hash);
    out.extend_from_slice(&sig.sig);
    out
}

fn inv_of(sig: &RecoveredSig) -> Inventory {
    Inventory::new(InvType::QuorumRecoveredSig, Hash256::from_lendian(sig.hash()))
}

fn encoded<T: BaseCodec>(msg: &T) -> Vec<u8> {
    let mut out = Vec::new();
    msg.encode(&mut out);
    out
}

fn inventory_of(sent: &[(CommandString, Vec<u8>)], cmd: CommandString) -> Vec<Inventory> {
    sent.iter()
        .filter(|(c, _)| *c == cmd)
        .flat_map(|(_, bytes)| match cmd {
            CommandString::INV => Inv::decode(&mut &bytes[..]).unwrap().inventory().clone(),
            CommandString::GETDATA => GetData::decode(&mut &bytes[..]).unwrap().inventory().clone(),
            _ => NotFound::decode(&mut &bytes[..]).unwrap().inventory().clone(),
        })
        .collect()
}

fn qsigrecs(sent: &[(CommandString, Vec<u8>)]) -> Vec<Vec<u8>> {
    sent.iter().filter(|(c, _)| *c == CommandString::QSIGREC).map(|(_, b)| b.clone()).collect()
}

#[test]
fn an_unconfigured_llmq_type_scores_100_and_purges_without_intake() {
    let (mut sim, seam) = make_sim();
    let a = connect_peer(&mut sim, 1);
    let b = connect_peer(&mut sim, 2);
    let unknown = recsig(LlmqType::from_base(250), 1);
    assert!(sim.admitted(&a, "qsigrec", wire(&unknown)).is_ok());
    assert_eq!(seam.take_events(), [Event::Remove(a.handle.id())]);
    assert_eq!(score(&sim, &a), 100);
    sim.pass();
    assert!(a.handle.disconnect_requested());

    let known = recsig(LlmqType::LlmqTest, 2);
    assert!(sim.admitted(&b, "qsigrec", wire(&known)).is_ok());
    assert_eq!(seam.take_events(), [Event::Verify(b.handle.id(), known.hash(), false)]);
    assert_eq!(score(&sim, &b), 0);
}

#[test]
fn already_have_stops_the_getdata() {
    let (mut sim, seam) = make_sim();
    sim.chain.set_ibd(false);
    let a = connect_peer(&mut sim, 1);
    let (had, new) = (recsig(LlmqType::LlmqTest, 1), recsig(LlmqType::LlmqTest, 2));
    seam.have.lock().unwrap().insert(had.hash());
    sim.deliver(&a, &[("inv", encoded(&Inv::new(vec![inv_of(&had), inv_of(&new)])))]);
    settle(&mut sim);
    assert_eq!(inventory_of(&sim.sent(&a), CommandString::GETDATA), [inv_of(&new)]);
}

#[test]
fn getdata_serves_only_an_active_quorums_recsig() {
    let (mut sim, seam) = make_sim();
    let a = connect_peer(&mut sim, 1);
    let (active, inactive) = (recsig(LlmqType::LlmqTest, 1), recsig(LlmqType::LlmqTest, 2));
    seam.active.lock().unwrap().insert(active.hash(), active.clone());
    sim.deliver(
        &a,
        &[("getdata", encoded(&GetData::new(vec![inv_of(&active), inv_of(&inactive)])))],
    );
    settle(&mut sim);
    let sent = sim.sent(&a);
    assert_eq!(qsigrecs(&sent), [wire(&active)]);
    assert_eq!(inventory_of(&sent, CommandString::NOTFOUND), [inv_of(&inactive)]);
}

#[test]
fn finalize_purges_pending_recsigs_at_the_discouragement_threshold_only() {
    let (mut sim, seam) = make_sim();
    let (a, b, c) =
        (connect_peer(&mut sim, 1), connect_peer(&mut sim, 2), connect_peer(&mut sim, 3));
    sim.shared.misbehaving(a.handle.id(), 100, "");
    sim.shared.misbehaving(b.handle.id(), 99, "");
    settle(&mut sim);
    assert!(a.handle.disconnect_requested());
    assert!(!b.handle.disconnect_requested());
    assert!(seam.take_events().is_empty(), "the purge waits for FinalizeNode");
    sim.core.disconnect(b.handle.id(), Why::Processor("test"));
    sim.core.disconnect(c.handle.id(), Why::Processor("test"));
    sim.remove_disconnected();
    assert_eq!(seam.take_events(), [Event::Remove(a.handle.id())]);
}

#[test]
fn a_non_mn_node_relays_a_processed_recsig_to_every_subscriber() {
    let (mut sim, _seam) = make_sim();
    let peers: Vec<_> = (1..=3).map(|n| connect_peer(&mut sim, n)).collect();
    for peer in &peers[..2] {
        subscribe(&mut sim, peer, true);
    }
    for peer in &peers {
        assert_eq!(peer.handle.verified_proreg_hash(), [0; 32]);
        sim.sent(peer);
    }

    let pushed = recsig(LlmqType::LlmqTest, 1);
    sim.shared.relay_recovered_sig(&pushed, true);
    settle(&mut sim);
    for peer in &peers[..2] {
        let sent = sim.sent(peer);
        assert_eq!(qsigrecs(&sent), [wire(&pushed)]);
        assert!(inventory_of(&sent, CommandString::INV).is_empty());
    }
    assert!(sim.sent(&peers[2]).is_empty());

    let announced = recsig(LlmqType::LlmqTest, 2);
    sim.shared.relay_recovered_sig(&announced, false);
    settle(&mut sim);
    for peer in &peers[..2] {
        let sent = sim.sent(peer);
        assert!(qsigrecs(&sent).is_empty());
        assert_eq!(inventory_of(&sent, CommandString::INV), [inv_of(&announced)]);
    }
    assert!(sim.sent(&peers[2]).is_empty());

    subscribe(&mut sim, &peers[0], false);
    let later = recsig(LlmqType::LlmqTest, 3);
    sim.shared.relay_recovered_sig(&later, false);
    settle(&mut sim);
    assert!(sim.sent(&peers[0]).is_empty());
    assert_eq!(inventory_of(&sim.sent(&peers[1]), CommandString::INV), [inv_of(&later)]);
}

#[test]
fn a_large_quorums_recsig_is_never_pushed_proactively() {
    let (mut sim, _seam) = make_sim();
    let peer = connect_peer(&mut sim, 1);
    subscribe(&mut sim, &peer, true);
    sim.sent(&peer);
    for (n, t) in
        [LlmqType::Llmq100_67, LlmqType::Llmq400_60, LlmqType::Llmq400_85].into_iter().enumerate()
    {
        let sig = recsig(t, n as u8);
        sim.shared.relay_recovered_sig(&sig, true);
        settle(&mut sim);
        let sent = sim.sent(&peer);
        assert!(qsigrecs(&sent).is_empty(), "{t:?}");
        assert_eq!(inventory_of(&sent, CommandString::INV), [inv_of(&sig)], "{t:?}");
    }
}

#[test]
fn a_relay_from_inside_a_handler_runs_after_the_senders_state_is_released() {
    let (mut sim, seam) = make_sim();
    seam.relay_on_verify.set(Arc::clone(&sim.shared)).ok().unwrap();
    let (a, b) = (connect_peer(&mut sim, 1), connect_peer(&mut sim, 2));
    for peer in [&a, &b] {
        subscribe(&mut sim, peer, true);
        sim.sent(peer);
    }
    let sig = recsig(LlmqType::LlmqTest, 1);
    sim.deliver(&a, &[("qsigrec", wire(&sig))]);
    settle(&mut sim);
    assert_eq!(seam.take_events(), [Event::Verify(a.handle.id(), sig.hash(), false)]);
    for peer in [&a, &b] {
        assert_eq!(qsigrecs(&sim.sent(peer)), [wire(&sig)]);
    }
}
