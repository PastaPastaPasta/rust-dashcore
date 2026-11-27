use std::sync::Mutex;

use dash_args::ArgsManager;
use dash_clock::MockClock;
use dash_dml::testkit::dmn_with;
use dash_dml::{MnList, MnType};
use dash_evo::locks::RecoveredSig;
use dash_llmq::dkg::bls::{
    BlsSkValue, build_pub_key_share, generate_contributions, member_id, sign,
};
use dash_llmq::dkg::wire::SigShareBytes;
use dash_llmq::sigenv::SignRequest;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeQuorum};
use dash_llmq::signing::api::QuorumSigning;
use dash_llmq::signing::manager::SigningManager;
use dash_llmq::sigshares::recovery::LlmqRecovery;
use dash_llmq::sigshares::verify::WeightedVerifier;
use dash_netaddr::{NetAddr, Service};
use dash_p2p::conn::Permissions;
use dash_p2p::{ConnectionType, PeerId};
use dash_p2p_core::{BatchedSigShare, Inventory};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::{AddrV1, NetInfo, NetInfoV1, ServiceV1, TxHash};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec;
use dashd_masternode::active::state::{LocalFacts, TipCtx};
use dashd_masternode::active::{ActiveMasternode, parse};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams};
use dashd_quorums::signing::{
    RecSigNotify, SigningHandles, SigningLookup, SigningNet, SigningRuntime,
};
use dashd_quorums::sigshares::SharesHandles;

use dash_llmq::sigenv::SigningQuorum as _;
use dash_pkc::bls::{BlsPublicKey, BlsScIetf, BlsSignature};
use dashd_rpc::evo::{EvoError, NodeQuorumSign, SignArgs, VerifyArgs, VerifyAt};

use super::*;
use crate::init::recsigs::RecoveredSigsDb;
use crate::rpc::ChainSlot;
use crate::rpc::evo::RpcSign;

const T: LlmqType = LlmqType::LlmqTest;
const QH: [u8; 32] = [4; 32];
const ID: [u8; 32] = [1; 32];
const MSG: [u8; 32] = [2; 32];
/// A ChainLock-shaped request's id, which names no quorum.
const CL_ID: [u8; 32] = [5; 32];
const BASE_HEIGHT: i32 = 96;
/// Heights whose latest quorums hold the rig's: in the active set, or only in the last one.
const SIGN_HEIGHT: i32 = 120;
const LAST_SET_HEIGHT: i32 = 130;
const SHARE_COMMANDS: [&str; 5] = ["qsigsesann", "qsigsinv", "qgetsigs", "qbsigs", "qsigshare"];

fn regtest() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn scalar(n: u8) -> [u8; 32] {
    let mut b = [0; 32];
    b[31] = n;
    b
}

/// The regtest operator key `01…01`, Ready on a list that holds it at the loopback service.
fn ready_active() -> Arc<ActiveMasternode> {
    let args = ArgsManager::new();
    args.force_set_arg("-masternodeblsprivkey", &"01".repeat(32));
    let active = Arc::new(ActiveMasternode::new(parse(&args).unwrap().unwrap()));
    let ours = Service::new(NetAddr::ipv4([127, 0, 0, 1]), 19899);
    let dmn = dmn_with(1, MnType::Regular, |_, cold| {
        cold.version = 2;
        cold.pub_key_operator =
            dash_dml::OperatorKey::from_value(active.pubkey(), BlsSchemeKind::Basic);
        cold.net_info = Arc::new(NetInfo::Legacy(NetInfoV1::new(ServiceV1::new(
            AddrV1::from_bytes(ours.addr.v1_bytes()),
            ours.port,
        ))));
    });
    let mut list = MnList::default_pre_dip3();
    list.insert_for_tests(dmn);
    let tip = TipCtx {
        height: 1000,
        dip3_enforced: true,
        require_routable: false,
    };
    let local = LocalFacts {
        listen: true,
        require_routable: false,
        local_toward_v4: None,
        loopback_listen: ours,
        peers: Vec::new(),
        valid: Vec::new(),
        self_connect_ok: true,
    };
    active.init(&tip, &list, &local);
    assert!(active.pro_tx_hash().is_some(), "{}", active.status());
    active
}

/// A 2-of-3 test quorum of `me` and two others, from one polynomial: `me` holds its secret
/// share, the test holds the second member's to sign as that peer.
fn quorum(me: ProTxHash) -> (Arc<FakeQuorum>, BlsSkValue) {
    let setup = regtest();
    let params = *setup.consensus.llmq(T).unwrap();
    assert_eq!((params.size, params.threshold), (3, 2));
    let members = vec![me, ProTxHash::from(scalar(11)), ProTxHash::from(scalar(12))];
    let ids: Vec<_> = members.iter().map(member_id).collect();
    let coefficients: Vec<_> = [3, 5]
        .iter()
        .map(|n| BlsSkValue::from_canonical(&scalar(*n), BlsSchemeKind::Basic).unwrap())
        .collect();
    let (vvec, mut sks) = generate_contributions(&coefficients, &ids).unwrap();
    let pk_shares = (0u16..).zip(&ids).map(|(i, id)| (i, build_pub_key_share(&vvec, id))).collect();
    let peer_sk = sks.remove(1);
    let q = FakeQuorum {
        llmq_type: T,
        quorum_hash: QH.into(),
        params,
        members,
        valid_members: [0, 1, 2].into(),
        has_verification_vector: true,
        quorum_public_key: vvec[0].clone(),
        pk_shares,
        sk_share: Some(sks.remove(0)),
    };
    (Arc::new(q), peer_sk)
}

struct Lookup(FakeLookup);

impl QuorumLookup for Lookup {
    type Quorum = FakeQuorum;
    type Error = Infallible;

    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        self.0.get_quorum(t, h)
    }

    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Infallible> {
        self.0.is_quorum_active(t, h)
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        self.0.select_quorum_for_signing(t, id, height, offset)
    }
}

impl SigningLookup for Lookup {
    fn base_height(&self, _: &FakeQuorum) -> i32 {
        BASE_HEIGHT
    }
}

#[derive(Default)]
struct Notified(Mutex<Vec<[u8; 32]>>);

impl SigningNet for Notified {
    fn misbehaving(&self, peer: PeerId, _: i32) {
        panic!("peer {peer} scored by the signing manager");
    }
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

impl RecSigNotify for Notified {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, _: bool) {
        self.0.lock().unwrap().push(sig.id);
    }
}

type Signing = SigningRuntime<RecoveredSigsDb, Lookup>;
type Shares = SigSharesRuntime<Lookup, Signing, SharesNet, SharesNet, NodeShareFacts, LlmqRecovery>;

struct Rig {
    sim: Sim,
    signing: Arc<Signing>,
    shares: Arc<Shares>,
    lookup: Arc<Lookup>,
    quorum: Arc<FakeQuorum>,
    peer_sk: BlsSkValue,
    quorums: Arc<QuorumNodes>,
    notified: Arc<Notified>,
    aborts: Arc<Mutex<Vec<String>>>,
}

/// The adapters over a sim processor whose share seam is the late slot, bound to the runtime as
/// at `AddExtraHandler(NetSigning)`; the signing runtime's queue is the share runtime, through
/// the node's [`ShareQueue`].
fn rig() -> Rig {
    let setup = regtest();
    let active = ready_active();
    let me = ProTxHash::from(active.pro_tx_hash().unwrap());
    let (quorum, peer_sk) = quorum(me);
    let mut lookup = FakeLookup::default();
    lookup.quorums.insert((T.to_base(), QH.into()), Arc::clone(&quorum));
    lookup.active.insert((T.to_base(), QH.into()));
    let selection = (T.to_base(), CL_ID, -1, dash_llmq::sigenv::SIGN_HEIGHT_OFFSET);
    lookup.selections.insert(selection, Arc::clone(&quorum));
    let selection = (T.to_base(), ID, -1, dash_llmq::sigenv::SIGN_HEIGHT_OFFSET);
    lookup.selections.insert(selection, Arc::clone(&quorum));
    let dkg_interval = i32::from(quorum.params.dkg_interval);
    for (height, offset) in [(-1, 0), (SIGN_HEIGHT, 0), (LAST_SET_HEIGHT, dkg_interval)] {
        lookup.selections.insert((T.to_base(), ID, height, offset), Arc::clone(&quorum));
    }
    let lookup = Arc::new(Lookup(lookup));
    let late = Arc::new(LateSigShares::default());
    let identity = local_identity(&setup, &IdentityOptions::new()).unwrap();
    let seams = Seams {
        sig_shares: Arc::clone(&late) as _,
        ..Seams::default()
    };
    let opts = ProcessorOptions::default();
    let sim = Sim::with(Arc::clone(&setup), identity, opts, Registry::new(), seams, |_| {});
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(SIM_START));
    let notified = Arc::new(Notified::default());
    let handles = SigningHandles {
        net: notified.clone(),
        notify: notified.clone(),
        masternode: Arc::new(ActiveIdentity(Arc::clone(&active))),
        clock: Arc::clone(&clock),
    };
    let platform = setup.consensus.llmq_type_platform;
    let manager = SigningManager::new(RecoveredSigsDb::in_memory(), platform, 60);
    let signing =
        Arc::new(SigningRuntime::new(manager, Arc::clone(&lookup), Arc::clone(&setup), handles));
    let quorums = Arc::new(QuorumNodes::default());
    let net = Arc::new(SharesNet {
        shared: Arc::clone(&sim.shared),
        core: Arc::clone(&sim.core),
        quorums: Arc::clone(&quorums),
    });
    let sporks = crate::sporks::spork_manager(&ArgsManager::new(), &setup, Arc::clone(&clock));
    let aborts = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&aborts);
    let shares = Arc::new(SigSharesRuntime::new(SharesHandles {
        lookup: Arc::clone(&lookup),
        intake: Arc::clone(&signing),
        net: Arc::clone(&net),
        out: net,
        facts: Arc::new(NodeShareFacts {
            active: Arc::clone(&active),
            sporks: sporks.unwrap(),
            clock,
            legacy: Box::new(|| false),
        }),
        recovery: Arc::new(LlmqRecovery),
        operator: Arc::new(ActiveOperator(active)),
        chain: setup,
        abort: Arc::new(move |what: &str| sink.lock().unwrap().push(what.to_owned())),
    }));
    assert!(signing.set_sign_queue(Arc::new(ShareQueue(Arc::downgrade(&shares)))).is_ok());
    late.bind(Arc::new(P2pSigShares(Arc::clone(&shares))));
    Rig {
        sim,
        signing,
        shares,
        lookup,
        quorum,
        peer_sk,
        quorums,
        notified,
        aborts,
    }
}

fn settle(sim: &mut Sim) {
    while sim.pass() {}
    sim.pass();
}

/// A handshaken inbound peer, verified as quorum member `member` by MNAUTH.
fn member_peer(rig: &mut Rig, n: u8, member: usize) -> SimPeer {
    let peer = rig.sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    rig.sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    settle(&mut rig.sim);
    rig.sim.sent(&peer);
    let pro_tx = *rig.quorum.members[member].as_bytes();
    assert!(rig.sim.core.try_mark_verified(peer.handle.id(), pro_tx, [n; 32]));
    peer
}

fn share_commands(sent: &[(CommandString, Vec<u8>)]) -> Vec<String> {
    let names = sent.iter().map(|(cmd, _)| cmd.to_string());
    names.filter(|c| SHARE_COMMANDS.contains(&c.as_str())).collect()
}

fn payload(item: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    item.encode(&mut out);
    out
}

fn request() -> SignRequest {
    SignRequest {
        llmq_type: T,
        id: ID,
        msg_hash: MSG,
        quorum_hash: Some(QH.into()),
        allow_resign: false,
        allow_diff_msg: false,
    }
}

/// A member peer of `rig`'s quorum, in the registry, with the handshake's messages drained.
fn quorum_peer(rig: &mut Rig) -> SimPeer {
    let peer = member_peer(rig, 1, 1);
    rig.quorums.set_nodes(T, QH, [scalar(11)].into());
    peer
}

fn sent_shares(rig: &mut Rig, peer: &SimPeer) -> Vec<String> {
    passes(rig);
    settle(&mut rig.sim);
    share_commands(&rig.sim.sent(peer))
}

fn sign_hash() -> [u8; 32] {
    let rs = RecoveredSig {
        llmq_type: T,
        quorum_hash: QH.into(),
        id: ID,
        msg_hash: MSG,
        sig: [0; 96],
    };
    rs.sign_hash()
}

/// The runtime's threads' passes, run here: the dispatcher's sign and verify tasks, then the
/// cleaning thread's `SendMessages`.
fn passes(rig: &Rig) {
    rig.shares.dispatch_pass(&WeightedVerifier::new(StrongRng::default()));
    rig.shares.maint_pass();
}

/// `AsyncSignIfMember` on the signing runtime reaches the share manager's `AsyncSign`; the
/// dispatcher signs our share with the quorum key share and `SendMessages` announces it to the
/// quorum's connected members, over the processor's connection table.
// Core: v24.0.2 src/llmq/signing_shares.cpp:792-878, :1144-1297, :1555-1559
#[test]
fn a_sign_request_reaches_the_share_manager_and_its_share_goes_to_quorum_peers() {
    let mut rig = rig();
    let (a, b) = (member_peer(&mut rig, 1, 1), member_peer(&mut rig, 2, 2));
    let outsider = rig.sim.connect(3, ConnectionType::Inbound, Permissions::NONE);
    rig.sim.handshake(&outsider, PeerVersionMsg::v24(SIM_START));
    settle(&mut rig.sim);
    rig.sim.sent(&outsider);
    let members = [11, 12].map(scalar).into();
    rig.quorums.set_nodes(T, QH, members);
    assert!(rig.signing.async_sign_if_member(&request()).unwrap());
    passes(&rig);
    settle(&mut rig.sim);
    let (to_a, to_b) = (share_commands(&rig.sim.sent(&a)), share_commands(&rig.sim.sent(&b)));
    assert!(to_a.contains(&"qsigsesann".to_owned()), "{to_a:?}");
    let carried = |sent: &[String]| sent.iter().any(|c| c == "qbsigs" || c == "qsigshare");
    assert!(carried(&to_a) || carried(&to_b), "{to_a:?} {to_b:?}");
    assert!(share_commands(&rig.sim.sent(&outsider)).is_empty());
    assert!(rig.aborts.lock().unwrap().is_empty());
}

/// A member's `qsigsesann` and `qbsigs` reach the share manager through the processor's seam;
/// with our share, its share completes the 2-of-3 recovery, and the recovered signature reaches
/// the signing runtime's intake.
// Core: v24.0.2 src/llmq/net_signing.cpp:64-150, src/llmq/signing_shares.cpp:681-762
#[test]
fn a_members_share_reaches_the_runtime_and_completes_the_recovery_with_ours() {
    let mut rig = rig();
    let peer_id = recover(&mut rig);
    assert!(rig.signing.has_recovered_sig(T, &ID, &MSG).unwrap());
    assert_eq!(*rig.notified.0.lock().unwrap(), [ID]);
    assert_eq!(rig.sim.shared.misbehavior(peer_id).unwrap().0, 0);
    assert!(rig.aborts.lock().unwrap().is_empty());
}

/// Our share and a member peer's, over the processor, recover the signature for `ID` and `MSG`.
fn recover(rig: &mut Rig) -> PeerId {
    let peer = quorum_peer(rig);
    assert!(rig.signing.async_sign_if_member(&request()).unwrap());
    passes(rig);
    let ann = SigSesAnn::new(9, T, QH.into(), ID.into(), MSG.into());
    let ann = SigSesAnnMsg::new(vec![ann]);
    rig.sim.deliver(&peer, &[("qsigsesann", payload(&ann))]);
    settle(&mut rig.sim);
    let peer_id = peer.handle.id();
    assert!(rig.shares.session_info(peer_id, 9).is_some(), "qsigsesann reached the runtime");
    let share = sign(&rig.peer_sk, &sign_hash(), false).to_bytes(BlsSchemeKind::Basic);
    let batch = BatchedSigShare::new(1, SigShareBytes::from_bytes(share));
    let batches = BatchedSigSharesMsg::new(vec![BatchedSigShares::new(9, vec![batch])]);
    rig.sim.deliver(&peer, &[("qbsigs", payload(&batches))]);
    settle(&mut rig.sim);
    passes(rig);
    peer_id
}

/// Before `AddExtraHandler(NetSigning)` binds it, the late slot answers as no share manager.
// Core: v24.0.2 src/init.cpp:2202, src/llmq/net_signing.cpp:64
#[test]
fn the_late_slot_is_disabled_until_bound() {
    let late = LateSigShares::default();
    assert!(!late.enabled());
    assert!(!late.spork21_active());
    late.ses_ann(1, Vec::new());
    late.bind(Arc::new(dashd_p2p::processing::seams::NoSigShares));
    assert!(!late.enabled());
}

/// The verifier's weights come from the strong source, fresh on every draw.
#[test]
fn the_strong_rng_draws_fresh_weights() {
    use rand_core::Rng as _;
    let mut rng = StrongRng::default();
    assert_ne!(rng.next_u64(), rng.next_u64());
}

/// `AsyncSignIfMember` with `allowReSign` (InstantSend's retroactive vote) forces a
/// re-announcement of our share before `AsyncSign`; without it, a repeated vote signs nothing.
// Core: v24.0.2 src/llmq/signing_shares.cpp:832-873, src/instantsend/signing.cpp:442
#[test]
fn a_resign_reannounces_our_share_to_the_quorum_peer() {
    let mut rig = rig();
    let peer = quorum_peer(&mut rig);
    assert!(rig.signing.async_sign_if_member(&request()).unwrap());
    let first = sent_shares(&mut rig, &peer);
    assert!(first.contains(&"qsigsesann".to_owned()), "{first:?}");
    assert!(!rig.signing.async_sign_if_member(&request()).unwrap());
    assert!(sent_shares(&mut rig, &peer).is_empty());
    let resign = SignRequest {
        allow_resign: true,
        ..request()
    };
    assert!(rig.signing.async_sign_if_member(&resign).unwrap());
    let again = sent_shares(&mut rig, &peer);
    assert!(again.contains(&"qsigsinv".to_owned()), "{again:?}");
    assert!(rig.aborts.lock().unwrap().is_empty());
}

/// A ChainLock-shaped request names no quorum: route B selects it by id at the default sign
/// height, and our share goes to that quorum's peer.
// Core: v24.0.2 src/chainlock/signing.cpp:177, src/llmq/signing_shares.cpp:805-820
#[test]
fn a_request_without_a_quorum_hash_signs_in_the_selected_quorum() {
    let mut rig = rig();
    let peer = quorum_peer(&mut rig);
    let selected = rig.signing.select_quorum_for_signing(T, &CL_ID).unwrap().unwrap();
    assert_eq!((selected.quorum_hash, selected.base_height), (QH.into(), BASE_HEIGHT));
    let cl = SignRequest {
        id: CL_ID,
        quorum_hash: None,
        ..request()
    };
    assert!(rig.signing.async_sign_if_member(&cl).unwrap());
    let sent = sent_shares(&mut rig, &peer);
    assert!(sent.contains(&"qsigsesann".to_owned()), "{sent:?}");
    assert!(rig.aborts.lock().unwrap().is_empty());
}

/// The signing commands' view of `rig`: its runtime, lookup and, on a masternode, share manager.
fn rpc(rig: &Rig, masternode: bool, legacy: bool) -> ChainSlot {
    let slot = ChainSlot::default();
    let shares = masternode.then(|| Arc::clone(&rig.shares) as _);
    let (signing, lookup) = (Arc::clone(&rig.signing) as _, Arc::clone(&rig.lookup) as _);
    slot.set_sign(RpcSign::new(regtest(), Box::new(move || legacy), signing, lookup, shares));
    slot
}

fn sign_args(quorum_hash: Option<[u8; 32]>) -> SignArgs {
    SignArgs {
        llmq_type: T.to_base(),
        id: ID,
        msg_hash: MSG,
        quorum_hash,
    }
}

/// `quorum sign … false` returns our share in the quorum named or selected, unsent, and it
/// verifies under our public key share; `quorum sign` with submit queues it, and the dispatcher
/// sends it to the quorum's peer.
// Core: v24.0.0 src/rpc/quorums.cpp:546-582, src/llmq/signing_shares.cpp:1601-1649
#[test]
fn quorum_sign_returns_our_share_or_submits_it_through_the_share_manager() {
    let mut rig = rig();
    let peer = quorum_peer(&mut rig);
    let slot = rpc(&rig, true, false);
    assert!(slot.masternode_mode());
    let share = slot.create_sig_share(sign_args(Some(QH))).unwrap().unwrap().unwrap();
    assert_eq!(slot.create_sig_share(sign_args(None)).unwrap(), Some(Some(share.clone())));
    assert_eq!((share.quorum_hash, share.member, share.sign_hash), (QH, 0, sign_hash()));
    let pk = rig.quorum.pub_key_share(0).unwrap().to_bytes(BlsSchemeKind::Basic);
    let pk = BlsPublicKey::<BlsScIetf>::from_bytes(&pk).unwrap();
    let sig = BlsSignature::<BlsScIetf>::from_bytes(&share.signature.clone().try_into().unwrap());
    assert!(pk.verify_digest(&share.sign_hash, &sig.unwrap()).is_ok());
    assert_eq!(slot.create_sig_share(sign_args(Some(ID))).unwrap(), None, "quorum not found");
    assert!(sent_shares(&mut rig, &peer).is_empty(), "submit=false sends nothing");
    assert!(slot.async_sign_if_member(sign_args(Some(QH)), false).unwrap());
    let sent = sent_shares(&mut rig, &peer);
    assert!(sent.contains(&"qsigsesann".to_owned()), "{sent:?}");
    assert!(rig.aborts.lock().unwrap().is_empty());
}

/// A node that is not a masternode has no share manager, but selects the quorum; once the
/// runtime is gone, the commands find no LLMQ context.
// Core: v24.0.0 src/rpc/quorums.cpp:517-519, :851-866
#[test]
fn a_node_without_shares_selects_but_does_not_sign() {
    let rig = rig();
    let slot = rpc(&rig, false, false);
    assert!(!slot.masternode_mode());
    assert_eq!(slot.platform_type(), regtest().consensus.llmq_type_platform.to_base());
    let selected = slot.select_quorum(T.to_base(), &ID).unwrap().unwrap();
    assert_eq!(selected.quorum_hash, QH);
    let recovery = usize::from(rig.quorum.params.recovery_members);
    assert_eq!(selected.recovery_members.len(), recovery);
    assert!(selected.recovery_members.iter().all(|m| rig.quorum.is_member(&ProTxHash::from(*m))));
    assert_eq!(slot.select_quorum(T.to_base(), &MSG).unwrap(), None);
    assert_eq!(slot.create_sig_share(sign_args(Some(QH))), Err(EvoError::LlmqUnavailable));
    drop(rig);
    assert_eq!(slot.llmq_context(), Err(EvoError::LlmqUnavailable));
    assert_eq!(slot.select_quorum(T.to_base(), &ID), Err(EvoError::LlmqUnavailable));
}

/// `quorum verify` over the recovered signature the runtime stored: true in the quorum named,
/// and in the latest quorums at the tip, at a height, and at a height where only the last active
/// set holds it; false for another message hash, at the quorum's own height, or under the other
/// scheme; a quorum not held is not found.
// Core: v24.0.0 src/rpc/quorums.cpp:652-662, :697-718; test/functional/feature_llmq_signing.py:119-133
#[test]
fn quorum_verify_checks_a_stored_recovered_sig_in_the_quorum_named_or_selected() {
    let mut rig = rig();
    recover(&mut rig);
    let sig = rig.signing.get_recovered_sig(T, &ID, &MSG).unwrap().unwrap().sig;
    let slot = rpc(&rig, false, false);
    assert!(slot.signature_readable(&sig));
    assert!(!slot.signature_readable(&[0; 96]));
    let verify = |slot: &ChainSlot, msg_hash, at| {
        let args = VerifyArgs {
            llmq_type: T.to_base(),
            id: ID,
            msg_hash,
            signature: sig,
            at,
        };
        slot.verify(args)
    };
    let other = [3; 32];
    for (msg, at, want) in [
        (MSG, VerifyAt::Height(-1), Some(true)),
        (MSG, VerifyAt::Height(SIGN_HEIGHT), Some(true)),
        (MSG, VerifyAt::Height(LAST_SET_HEIGHT), Some(true)),
        (MSG, VerifyAt::Height(BASE_HEIGHT), Some(false)),
        (other, VerifyAt::Height(-1), Some(false)),
        (other, VerifyAt::Height(SIGN_HEIGHT), Some(false)),
        (MSG, VerifyAt::Quorum(QH), Some(true)),
        (other, VerifyAt::Quorum(QH), Some(false)),
        (MSG, VerifyAt::Quorum(ID), None),
    ] {
        assert_eq!(verify(&slot, msg, at), Ok(want), "{at:?}");
    }
    let legacy = rpc(&rig, false, true);
    assert_eq!(verify(&legacy, MSG, VerifyAt::Quorum(QH)), Ok(Some(false)));
    assert!(rig.aborts.lock().unwrap().is_empty());
    drop(rig);
    assert_eq!(verify(&slot, MSG, VerifyAt::Height(-1)), Err(EvoError::LlmqUnavailable));
}
