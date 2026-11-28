//! SG1-W2-4 signature-share P2P intake seam and boundary tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dash_evo::locks::RecoveredSig;
use dash_num::Hash256;
use dash_p2p::conn::Permissions;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, ConnectionType, PeerId};
use dash_p2p_core::{
    BatchedSigShare, BatchedSigShares, GetData, Inv, InvType, Inventory, MAX_BATCHED_SIGS,
    MAX_SIG_SES_ANNS, MAX_SIG_SHARES, MAX_SIG_SHARES_INVS, SigSesAnn, SigShare, SigSharesInv,
};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_primitives::{DynBitset, QuorumHash};
use dash_types::codec::BaseCodec;
use dash_types::{CompactSize, Numeric};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::registry::CoreHandler;
use dashd_p2p::processing::seams::{SigSharesInvKind, SigSharesSeam};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{HandlerCx, NetHandler, Registry, Seams, Shared};

const SHARE_COMMANDS: [&str; 5] = ["qsigshare", "qsigsesann", "qsigsinv", "qgetsigs", "qbsigs"];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Event {
    SesAnn(PeerId, Vec<SigSesAnn>),
    SharesInv(PeerId, SigSharesInvKind, Vec<SigSharesInv>),
    Batched(PeerId, Vec<BatchedSigShares>),
    SigShare(PeerId, Vec<SigShare>),
    Ban(PeerId),
}

struct RecordingSigShares {
    spork21: AtomicBool,
    shared: OnceLock<Arc<Shared>>,
    events: Mutex<Vec<Event>>,
}

impl RecordingSigShares {
    fn new(spork21: bool) -> Arc<Self> {
        Arc::new(Self {
            spork21: AtomicBool::new(spork21),
            shared: OnceLock::new(),
            events: Mutex::new(Vec::new()),
        })
    }

    fn set_spork21(&self, active: bool) {
        self.spork21.store(active, Ordering::SeqCst);
    }

    fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}

impl SigSharesSeam for RecordingSigShares {
    fn spork21_active(&self) -> bool {
        self.spork21.load(Ordering::SeqCst)
    }

    fn ses_ann(&self, peer: PeerId, msgs: Vec<SigSesAnn>) {
        self.events.lock().unwrap().push(Event::SesAnn(peer, msgs));
    }

    fn sig_shares_inv(&self, peer: PeerId, kind: SigSharesInvKind, msgs: Vec<SigSharesInv>) {
        self.events.lock().unwrap().push(Event::SharesInv(peer, kind, msgs));
    }

    fn batched(&self, peer: PeerId, msgs: Vec<BatchedSigShares>) {
        self.events.lock().unwrap().push(Event::Batched(peer, msgs));
    }

    fn sig_share(&self, peer: PeerId, msgs: Vec<SigShare>) {
        self.events.lock().unwrap().push(Event::SigShare(peer, msgs));
    }

    fn ban(&self, peer: PeerId) {
        if let Some(shared) = self.shared.get() {
            shared.misbehaving(peer, 100, "");
        }
        self.events.lock().unwrap().push(Event::Ban(peer));
    }
}

fn make_sim(seams: Seams) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    Sim::with(params, identity, ProcessorOptions::default(), Registry::new(), seams, |_| {})
}

fn make_recorded_sim(spork21: bool) -> (Sim, Arc<RecordingSigShares>) {
    let seam = RecordingSigShares::new(spork21);
    let sim = make_sim(Seams {
        sig_shares: Arc::clone(&seam) as _,
        ..Seams::default()
    });
    seam.shared.set(Arc::clone(&sim.shared)).ok().unwrap();
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

fn score(sim: &Sim, peer: &SimPeer) -> i32 {
    sim.shared.misbehavior(peer.handle.id()).unwrap().0
}

fn encode_list<T: BaseCodec>(items: &[T]) -> Vec<u8> {
    let mut out = Vec::new();
    CompactSize::from(items.len() as u64).encode(&mut out);
    for item in items {
        item.encode(&mut out);
    }
    out
}

fn count_only(n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    CompactSize::from(n as u64).encode(&mut out);
    out
}

fn sample_share(member: u16) -> SigShare {
    SigShare::new(
        LlmqType::new(100),
        QuorumHash::from([0x11; 32]),
        member,
        Hash256::from_lendian([0x22; 32]),
        Hash256::from_lendian([0x33; 32]),
        [0x44; 96].into(),
    )
}

fn sample_ann(session_id: u32) -> SigSesAnn {
    SigSesAnn::new(
        session_id,
        LlmqType::new(100),
        QuorumHash::from([0x11; 32]),
        Hash256::from_lendian([0x22; 32]),
        Hash256::from_lendian([0x33; 32]),
    )
}

fn sample_inv(session_id: u32) -> SigSharesInv {
    SigSharesInv::new(session_id, DynBitset::new(8, vec![0b0000_0101]).unwrap())
}

fn sample_batch(session_id: u32, inner_count: usize) -> BatchedSigShares {
    let shares = (0..inner_count)
        .map(|i| BatchedSigShare::new(i as u16, [(i & 0xff) as u8; 96].into()))
        .collect();
    BatchedSigShares::new(session_id, shares)
}

#[test]
fn no_sig_shares_leaves_all_five_commands_unread_and_unscored() {
    let mut sim = make_sim(Seams::default());
    let peer = connect_peer(&mut sim, 1);
    for cmd in SHARE_COMMANDS {
        for bad in [vec![], vec![0x01, 0xee], vec![0xfd, 0x01, 0x00], count_only(401)] {
            assert_eq!(sim.admitted(&peer, cmd, bad.clone()), Ok(()), "{cmd} read payload");
            sim.deliver(&peer, &[(cmd, bad)]);
            settle(&mut sim);
            assert_eq!(score(&sim, &peer), 0, "{cmd} scored");
            assert!(!peer.handle.disconnect_requested(), "{cmd} disconnected");
            assert!(sim.sent(&peer).is_empty(), "{cmd} replied");
        }
    }
}

#[test]
fn qsigshare_spork21_gate_precedes_the_read() {
    let (mut sim, seam) = make_recorded_sim(false);
    let peer = connect_peer(&mut sim, 1);
    let peer_id = peer.handle.id();

    for bad in [vec![], vec![0x01, 0xff], count_only(MAX_SIG_SHARES + 1)] {
        assert_eq!(sim.admitted(&peer, "qsigshare", bad), Ok(()));
        assert!(seam.take_events().is_empty());
        assert_eq!(score(&sim, &peer), 0);
    }
    assert_eq!(sim.admitted(&peer, "qsigshare", encode_list(&[sample_share(0)])), Ok(()));
    assert!(seam.take_events().is_empty());
    assert_eq!(score(&sim, &peer), 0);

    seam.set_spork21(true);
    let bad_peer = connect_peer(&mut sim, 2);
    assert!(sim.admitted(&bad_peer, "qsigshare", vec![]).is_err());
    assert_eq!(seam.take_events(), [Event::Ban(bad_peer.handle.id())]);
    assert_eq!(score(&sim, &bad_peer), 100);

    let wire_peer = connect_peer(&mut sim, 3);
    sim.deliver(&wire_peer, &[("qsigshare", vec![0x01, 0xff])]);
    settle(&mut sim);
    assert_eq!(seam.take_events(), [Event::Ban(wire_peer.handle.id())]);
    assert_eq!(score(&sim, &wire_peer), 100);

    let shares = vec![sample_share(7)];
    assert_eq!(sim.admitted(&peer, "qsigshare", encode_list(&shares)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SigShare(peer_id, shares)]);
    assert_eq!(score(&sim, &peer), 0);
}

#[test]
fn non_qsigshare_commands_run_when_spork21_is_off_and_ban_on_malformed() {
    let (mut sim, seam) = make_recorded_sim(false);
    let ok_peer = connect_peer(&mut sim, 1);
    let ok_id = ok_peer.handle.id();

    let anns = vec![sample_ann(1)];
    assert_eq!(sim.admitted(&ok_peer, "qsigsesann", encode_list(&anns)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SesAnn(ok_id, anns)]);

    let invs = vec![sample_inv(2)];
    assert_eq!(sim.admitted(&ok_peer, "qsigsinv", encode_list(&invs)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SharesInv(ok_id, SigSharesInvKind::Inv, invs.clone())]);

    assert_eq!(sim.admitted(&ok_peer, "qgetsigs", encode_list(&invs)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SharesInv(ok_id, SigSharesInvKind::GetSigs, invs)]);

    let batches = vec![sample_batch(3, 2)];
    assert_eq!(sim.admitted(&ok_peer, "qbsigs", encode_list(&batches)), Ok(()));
    assert_eq!(seam.take_events(), [Event::Batched(ok_id, batches)]);
    assert_eq!(score(&sim, &ok_peer), 0);

    for (i, cmd) in ["qsigsesann", "qsigsinv", "qgetsigs", "qbsigs"].into_iter().enumerate() {
        let p = connect_peer(&mut sim, 10 + i as u8);
        assert!(sim.admitted(&p, cmd, vec![0x01]).is_err(), "{cmd} did not throw");
        assert_eq!(seam.take_events(), [Event::Ban(p.handle.id())], "{cmd} did not ban");
        assert_eq!(score(&sim, &p), 100, "{cmd} did not score");
    }
}

#[test]
fn caps_at_n_and_n_plus_one_for_every_share_command_including_qbsigs_inner_total() {
    let (mut sim, seam) = make_recorded_sim(true);
    let ok_peer = connect_peer(&mut sim, 1);
    let ok_id = ok_peer.handle.id();

    let shares_n: Vec<_> = (0..MAX_SIG_SHARES as u16).map(sample_share).collect();
    assert_eq!(sim.admitted(&ok_peer, "qsigshare", encode_list(&shares_n)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SigShare(ok_id, shares_n)]);

    let shares_over: Vec<_> = (0..=MAX_SIG_SHARES as u16).map(sample_share).collect();
    let p_share = connect_peer(&mut sim, 2);
    assert!(sim.admitted(&p_share, "qsigshare", encode_list(&shares_over)).is_err());
    assert_eq!(seam.take_events(), [Event::Ban(p_share.handle.id())]);
    assert_eq!(score(&sim, &p_share), 100);

    let anns_n: Vec<_> = (0..MAX_SIG_SES_ANNS as u32).map(sample_ann).collect();
    assert_eq!(sim.admitted(&ok_peer, "qsigsesann", encode_list(&anns_n)), Ok(()));
    assert_eq!(seam.take_events(), [Event::SesAnn(ok_id, anns_n)]);

    let anns_over: Vec<_> = (0..=MAX_SIG_SES_ANNS as u32).map(sample_ann).collect();
    let p_ann = connect_peer(&mut sim, 3);
    assert!(sim.admitted(&p_ann, "qsigsesann", encode_list(&anns_over)).is_err());
    assert_eq!(seam.take_events(), [Event::Ban(p_ann.handle.id())]);
    assert_eq!(score(&sim, &p_ann), 100);

    let invs_n: Vec<_> = (0..MAX_SIG_SHARES_INVS as u32).map(sample_inv).collect();
    let invs_over: Vec<_> = (0..=MAX_SIG_SHARES_INVS as u32).map(sample_inv).collect();
    for (idx, (cmd, kind)) in
        [("qsigsinv", SigSharesInvKind::Inv), ("qgetsigs", SigSharesInvKind::GetSigs)]
            .into_iter()
            .enumerate()
    {
        assert_eq!(sim.admitted(&ok_peer, cmd, encode_list(&invs_n)), Ok(()));
        assert_eq!(seam.take_events(), [Event::SharesInv(ok_id, kind, invs_n.clone())]);

        let p_inv = connect_peer(&mut sim, 4 + idx as u8);
        assert!(sim.admitted(&p_inv, cmd, encode_list(&invs_over)).is_err());
        assert_eq!(seam.take_events(), [Event::Ban(p_inv.handle.id())]);
        assert_eq!(score(&sim, &p_inv), 100);
    }

    let outer_n: Vec<_> = (0..MAX_BATCHED_SIGS as u32).map(|i| sample_batch(i, 1)).collect();
    assert_eq!(sim.admitted(&ok_peer, "qbsigs", encode_list(&outer_n)), Ok(()));
    assert_eq!(seam.take_events(), [Event::Batched(ok_id, outer_n)]);

    let outer_over: Vec<_> = (0..=MAX_BATCHED_SIGS as u32).map(|i| sample_batch(i, 0)).collect();
    let p_outer = connect_peer(&mut sim, 6);
    assert!(sim.admitted(&p_outer, "qbsigs", encode_list(&outer_over)).is_err());
    assert_eq!(seam.take_events(), [Event::Ban(p_outer.handle.id())]);
    assert_eq!(score(&sim, &p_outer), 100);

    let inner_n = vec![sample_batch(0, 200), sample_batch(1, 200)];
    assert_eq!(sim.admitted(&ok_peer, "qbsigs", encode_list(&inner_n)), Ok(()));
    assert_eq!(seam.take_events(), [Event::Batched(ok_id, inner_n)]);
    assert_eq!(score(&sim, &ok_peer), 0);

    let inner_over = vec![sample_batch(0, 200), sample_batch(1, 201)];
    let p_inner = connect_peer(&mut sim, 7);
    assert!(sim.admitted(&p_inner, "qbsigs", encode_list(&inner_over)).is_err());
    assert_eq!(seam.take_events(), [Event::Ban(p_inner.handle.id())]);
    assert_eq!(score(&sim, &p_inner), 100);
}

#[test]
fn qsigrec_still_takes_its_existing_path_with_sig_shares_installed() {
    let (mut sim, seam) = make_recorded_sim(true);
    sim.chain.set_ibd(false);
    let a = connect_peer(&mut sim, 1);
    let b = connect_peer(&mut sim, 2);

    let mut payload = vec![0x5a; RecoveredSig::LEN];
    payload[0] = 100;
    let inv = Inventory::new(
        InvType::QuorumRecoveredSig,
        Hash256::from_lendian(dash_sha256::sha256d(&payload)),
    );
    let mut inv_bytes = Vec::new();
    Inv::new(vec![inv]).encode(&mut inv_bytes);

    sim.deliver(&a, &[("inv", inv_bytes.clone())]);
    settle(&mut sim);
    let requested: Vec<_> = sim
        .sent(&a)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::GETDATA)
        .flat_map(|(_, bytes)| GetData::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .collect();
    assert_eq!(requested, [inv]);

    sim.deliver(&b, &[("inv", inv_bytes)]);
    settle(&mut sim);
    sim.deliver(&a, &[("qsigrec", payload)]);
    settle(&mut sim);

    let fallback: Vec<_> = sim
        .sent(&b)
        .into_iter()
        .filter(|(cmd, _)| *cmd == CommandString::GETDATA)
        .flat_map(|(_, bytes)| GetData::decode(&mut &bytes[..]).unwrap().inventory().clone())
        .collect();
    assert_eq!(fallback, [inv]);
    assert!(seam.take_events().is_empty());
    assert_eq!(score(&sim, &a), 0);

    assert!(sim.admitted(&a, "qsigrec", vec![0xee; 5]).is_err());
    assert!(seam.take_events().is_empty());
    assert_eq!(score(&sim, &a), 0);
}

struct DummyNetSigning(&'static [CommandString]);

impl NetHandler for DummyNetSigning {
    fn commands(&self) -> &'static [CommandString] {
        self.0
    }

    fn core_handler(&self) -> Option<CoreHandler> {
        Some(CoreHandler::NetSigning)
    }

    fn process_message(
        &mut self,
        _cx: &mut HandlerCx<'_>,
        _cmd: &CommandString,
        _payload: &[u8],
    ) -> Result<(), Throw> {
        Ok(())
    }
}

#[test]
#[should_panic(expected = "NetSigning registered after NetSigning")]
fn registering_a_second_net_signing_handler_panics() {
    const QSIGREC: [CommandString; 1] = [CommandString::QSIGREC];
    const QSIGSHARE: [CommandString; 1] = [CommandString::QSIGSHARE];
    let mut reg = Registry::new();
    reg.register(Box::new(DummyNetSigning(&QSIGREC)));
    reg.register(Box::new(DummyNetSigning(&QSIGSHARE)));
}
