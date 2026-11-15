//! G7's Core v24 platform-ban intake and serving branches.

use std::sync::{Arc, Mutex};

use dash_evo::locks::sign_hash;
use dash_evo::types::RawMnType;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, ConnectionType};
use dash_p2p_core::{GetData, Inv, InvType, Inventory, PlatformBan};
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsCtx, BlsGlobalSig, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{QuorumHash, TxHash};
use dash_types::Hashable;
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::processing::platformban::PlatformBanHandler;
use dashd_p2p::processing::seams::{
    MasternodeNet, MnAuthSigner, MnListView, OperatorKeyRef, PlatformBans,
};
use dashd_p2p::sim::masternode::{FakeMnList, FakeMnNet, FakePlatformBans, FakePlatformQuorums};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{Registry, Seams};

const PROTX: [u8; 32] = [0x11; 32];
const QUORUM: [u8; 32] = [0x22; 32];
const KEY_SEED: u8 = 7;

fn params() -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn key() -> OperatorKeyRef {
    OperatorKeyRef {
        stored: [1; 48],
        stored_legacy: false,
        pubkey_hash: [1; 32],
    }
}

fn evo_list() -> Arc<dyn MnListView> {
    Arc::new(FakeMnList::new().with(PROTX, key()).with_mn_type(PROTX, RawMnType::EVO))
}

fn regular_list() -> Arc<dyn MnListView> {
    Arc::new(FakeMnList::new().with(PROTX, key()).with_mn_type(PROTX, RawMnType::REGULAR))
}

fn public_key() -> BlsPkValue {
    let secret = BlsSecretKey::<BlsScIetf>::from_ikm(&[KEY_SEED; 32]).unwrap();
    BlsPkValue::from_canonical(&secret.public_key().to_bytes(), BlsSchemeKind::Basic).unwrap()
}

fn ban(height: i32, sig_seed: Option<u8>) -> PlatformBan {
    let llmq_type = params().consensus.llmq_type_platform;
    let protx = TxHash::from(PROTX);
    let quorum = QuorumHash::from(QUORUM);
    let request_id = dashd_p2p::processing::platformban::platform_ban_request_id(&PROTX, height);
    let message_hash =
        dashd_p2p::processing::platformban::platform_ban_message_hash(&PROTX, height);
    let hash = sign_hash(llmq_type, &quorum, &request_id, &message_hash);
    let sig = sig_seed
        .map(|seed| {
            BlsSecretKey::<BlsScIetf>::from_ikm(&[seed; 32])
                .unwrap()
                .sign_with(&hash, BlsSigId::Basic)
                .to_bytes()
        })
        .unwrap_or([0; 96]);
    PlatformBan::new(protx, height, quorum, sig.into())
}

fn encode<T: BaseCodec>(value: &T) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

fn inventory(ban: &PlatformBan) -> Inventory {
    Inventory::new(InvType::PlatformBan, ban.hash())
}

fn make_sim(
    synced: bool,
    list: Arc<dyn MnListView>,
    bans: Arc<FakePlatformBans>,
    quorums: Arc<FakePlatformQuorums>,
) -> Sim {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let bans: Arc<dyn PlatformBans> = bans;
    let mut registry = Registry::new();
    registry.register(Box::new(PlatformBanHandler::new(Arc::clone(&bans), quorums)));
    let mn = dashd_p2p::processing::seams::MasternodeSeams {
        net: Arc::new(FakeMnNet::new(synced, list)) as Arc<dyn MasternodeNet>,
        signer: None::<Arc<dyn MnAuthSigner>>,
    };
    Sim::with_masternode(
        p,
        id,
        ProcessorOptions::default(),
        registry,
        Seams {
            platform_bans: bans,
            ..Seams::default()
        },
        mn,
        |_| {},
    )
}

fn connect_peer(sim: &mut Sim, n: u8) -> SimPeer {
    let peer = sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    while sim.pass() {}
    sim.sent(&peer);
    peer
}

fn score(sim: &Sim, peer: &SimPeer) -> i32 {
    sim.shared.misbehavior(peer.handle.id()).unwrap().0
}

fn make_quorums(with_key: bool) -> Arc<FakePlatformQuorums> {
    Arc::new(FakePlatformQuorums::new(with_key.then_some((
        params().consensus.llmq_type_platform,
        QUORUM,
        public_key(),
    ))))
}

struct PlatformBanLogs(Mutex<Vec<String>>);

impl tracing::Subscriber for PlatformBanLogs {
    fn register_callsite(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, meta: &tracing::Metadata<'_>) -> bool {
        meta.is_event() && meta.target() == "llmq"
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        struct Message<'a>(&'a mut String);
        impl tracing::field::Visit for Message<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    *self.0 = format!("{value:?}");
                }
            }
        }
        let mut line = String::new();
        event.record(&mut Message(&mut line));
        self.0.lock().unwrap().push(line);
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

impl PlatformBanLogs {
    fn count_containing(&self, needle: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|line| line.contains(needle)).count()
    }
}

#[test]
fn default_wired_handler_is_silent_for_valid_and_malformed_bans() {
    let p = params();
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    let net = Arc::new(FakeMnNet::new(true, evo_list()));
    let mn = dashd_p2p::processing::seams::MasternodeSeams {
        net: net as Arc<dyn MasternodeNet>,
        signer: None,
    };
    let mut registry = Registry::new();
    registry.register(Box::new(PlatformBanHandler::default()));
    let mut sim = Sim::with_masternode(
        p,
        id,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        mn,
        |_| {},
    );
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    sim.admitted(&peer, "platformban", vec![1, 2, 3]).unwrap();
    assert_eq!(score(&sim, &peer), 0);
    assert!(sim.sent(&peer).is_empty());
}

#[test]
fn unsynced_platform_ban_is_silent() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(false, evo_list(), Arc::clone(&bans), Arc::clone(&quorums));
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&ban(0, Some(KEY_SEED)))).unwrap();
    assert_eq!(score(&sim, &peer), 0);
    assert_eq!(quorums.lookups(), 0);
    assert_eq!(bans.set_calls(), 0);
}

#[test]
fn platform_ban_scores_unknown_and_non_evo_masternodes() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim =
        make_sim(true, Arc::new(FakeMnList::new()), Arc::clone(&bans), Arc::clone(&quorums));
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    assert_eq!(score(&sim, &peer), 1);

    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, regular_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 2);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    assert_eq!(score(&sim, &peer), 100);

    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let untyped = Arc::new(FakeMnList::new().with(PROTX, key()));
    let mut sim = make_sim(true, untyped, bans, quorums);
    let peer = connect_peer(&mut sim, 3);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    assert_eq!(score(&sim, &peer), 1);
}

#[test]
fn platform_ban_height_window_scores_one_and_ten() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&ban(5, None))).unwrap();
    sim.admitted(&peer, "platformban", encode(&ban(6, None))).unwrap();
    assert_eq!(score(&sim, &peer), 11);

    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 2);
    sim.admitted(&peer, "platformban", encode(&ban(-576, Some(KEY_SEED)))).unwrap();
    sim.admitted(&peer, "platformban", encode(&ban(-577, None))).unwrap();
    sim.admitted(&peer, "platformban", encode(&ban(-581, None))).unwrap();
    sim.admitted(&peer, "platformban", encode(&ban(-582, None))).unwrap();
    assert_eq!(score(&sim, &peer), 12);
}

#[test]
fn platform_ban_scores_missing_quorum_and_invalid_signature() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(false);
    let mut sim = make_sim(true, evo_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    assert_eq!(score(&sim, &peer), 100);

    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 2);
    sim.admitted(&peer, "platformban", encode(&ban(0, None))).unwrap();
    assert_eq!(score(&sim, &peer), 100);
}

#[test]
fn platform_ban_undecodable_signature_is_dropped_before_hashing() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let peer = connect_peer(&mut sim, 1);
    let logs = Arc::new(PlatformBanLogs(Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&logs));
    let malformed = PlatformBan::new(PROTX.into(), 0, QUORUM.into(), [0xff; 96].into());
    assert!(sim.admitted(&peer, "platformban", encode(&malformed)).is_err());
    assert_eq!(score(&sim, &peer), 0);
    assert_eq!(bans.set_calls(), 0);
    assert_eq!(logs.count_containing("PLATFORMBAN -- hash:"), 0);
}

#[test]
fn platform_ban_legacy_only_signature_decodes_then_scores() {
    let valid = ban(0, Some(KEY_SEED));
    let legacy = BlsGlobalSig::decode(valid.sig().as_bytes())
        .unwrap()
        .get(BlsCtx::BASIC)
        .to_bytes(BlsSchemeKind::Legacy);
    let legacy = PlatformBan::new(PROTX.into(), 0, QUORUM.into(), legacy.into());
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&legacy)).unwrap();
    assert_eq!(score(&sim, &peer), 100);
    assert_eq!(bans.set_calls(), 0);
}

#[test]
fn platform_ban_verification_rejects_a_flipped_signature() {
    let valid = ban(0, Some(KEY_SEED));
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "platformban", encode(&valid)).unwrap();
    assert_eq!(score(&sim, &peer), 0);
    assert_eq!(bans.set_calls(), 1);

    let flipped = (0..96)
        .flat_map(|index| (0..8).map(move |bit| (index, bit)))
        .find_map(|(index, bit)| {
            let mut candidate = *valid.sig().as_bytes();
            candidate[index] ^= 1 << bit;
            BlsGlobalSig::decode(&candidate).ok().map(|_| candidate)
        })
        .expect("a one-bit mutation remains a decodable signature");
    let flipped = PlatformBan::new(PROTX.into(), 0, QUORUM.into(), flipped.into());
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let peer = connect_peer(&mut sim, 2);
    sim.admitted(&peer, "platformban", encode(&flipped)).unwrap();
    assert_eq!(score(&sim, &peer), 100);
    assert_eq!(bans.set_calls(), 0);
}

#[test]
fn platform_ban_dedupes_before_quorum_lookup_and_store() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), Arc::clone(&quorums));
    let peer = connect_peer(&mut sim, 1);
    let payload = encode(&ban(0, Some(KEY_SEED)));
    sim.admitted(&peer, "platformban", payload.clone()).unwrap();
    sim.admitted(&peer, "platformban", payload).unwrap();
    assert_eq!(score(&sim, &peer), 0);
    assert_eq!(bans.set_calls(), 1);
    assert_eq!(quorums.lookups(), 1);
}

#[test]
fn platform_ban_logs_once_before_deduping_the_resend() {
    let logs = Arc::new(PlatformBanLogs(Mutex::default()));
    let _log = tracing::subscriber::set_default(Arc::clone(&logs));
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let sender = connect_peer(&mut sim, 1);
    let receiver = connect_peer(&mut sim, 2);
    let valid = ban(0, Some(KEY_SEED));
    let payload = encode(&valid);
    sim.admitted(&sender, "platformban", payload.clone()).unwrap();
    while sim.pass() {}
    let first_line = format!("PLATFORMBAN -- hash: {} protx_hash:", valid.hash());
    assert_eq!(logs.count_containing(&first_line), 1);
    assert_eq!(sim.sent(&receiver).len(), 1);

    sim.admitted(&sender, "platformban", payload).unwrap();
    while sim.pass() {}
    assert_eq!(logs.count_containing(&first_line), 1);
    assert_eq!(score(&sim, &sender), 0);
    assert_eq!(bans.set_calls(), 1);
    assert!(sim.sent(&receiver).is_empty());
}

#[test]
fn platform_ban_stores_and_relays_to_a_second_peer() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let sender = connect_peer(&mut sim, 1);
    let receiver = connect_peer(&mut sim, 2);
    let ban = ban(0, Some(KEY_SEED));
    sim.admitted(&sender, "platformban", encode(&ban)).unwrap();
    while sim.pass() {}
    let frames = sim.sent_at_least(&receiver, 1);
    assert_eq!(frames[0].0, CommandString::INV);
    let announced = Inv::decode(&mut &frames[0].1[..]).unwrap();
    assert_eq!(announced.inventory(), &[inventory(&ban)]);
    assert_eq!(bans.get(&ban.hash().to_lendian()), Some(ban));
}

#[test]
fn platform_ban_getdata_serves_and_notfound() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let ban = ban(0, Some(KEY_SEED));
    let known = inventory(&ban);
    let missing = Inventory::new(InvType::PlatformBan, dash_num::Hash256::from([0x44; 32]));
    bans.insert(ban.clone());
    let mut sim = make_sim(true, evo_list(), bans, quorums);
    let peer = connect_peer(&mut sim, 1);
    sim.admitted(&peer, "getdata", encode(&GetData::new(vec![known]))).unwrap();
    let frames = sim.sent(&peer);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, CommandString::PLATFORMBAN);
    assert_eq!(frames[0].1, encode(&ban));

    sim.admitted(&peer, "getdata", encode(&GetData::new(vec![missing]))).unwrap();
    let frames = sim.sent(&peer);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, CommandString::NOTFOUND);
}

#[test]
fn platform_ban_accepted_by_intake_is_served_from_the_same_store() {
    let bans = Arc::new(FakePlatformBans::new());
    let quorums = make_quorums(true);
    let mut sim = make_sim(true, evo_list(), Arc::clone(&bans), quorums);
    let sender = connect_peer(&mut sim, 1);
    let requester = connect_peer(&mut sim, 2);
    let ban = ban(0, Some(KEY_SEED));
    sim.admitted(&sender, "platformban", encode(&ban)).unwrap();
    while sim.pass() {}
    assert_eq!(bans.set_calls(), 1);
    let _ = sim.sent(&requester);

    sim.admitted(&requester, "getdata", encode(&GetData::new(vec![inventory(&ban)]))).unwrap();
    let frames = sim.sent(&requester);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, CommandString::PLATFORMBAN);
    assert_eq!(frames[0].1, encode(&ban));

    let missing = Inventory::new(InvType::PlatformBan, dash_num::Hash256::from([0x44; 32]));
    sim.admitted(&requester, "getdata", encode(&GetData::new(vec![missing]))).unwrap();
    let frames = sim.sent(&requester);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0].0, CommandString::NOTFOUND);
}
