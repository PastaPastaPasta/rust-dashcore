#![cfg(feature = "testing")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{PoolLimits, txid};
use dash_coinjoin::queue::{MixingCounters, MixingMasternode};
use dash_coinjoin::server::{ServerEffect, SignedInputs};
use dash_consensus::hash::hash160;
use dash_consensus::script::push_data;
use dash_consensus::script::sighash::signature_hash;
use dash_p2p::conn::Permissions;
use dash_p2p::{CommandString, Service};
use dash_p2p_core::{DsAccept, DsEntry, DsQueue, DsStatusUpdate, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::Hashable;
use dash_types::codec::BaseCodec;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::ChainView;
use dashd_coinjoin::server::{CoinJoinServer, ServerClock};
use dashd_coinjoin::testing::{FakeActiveMn, FakeMempool, FakeMnLists, FakePeerSink, FakeRandom};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::logic::ProcessorOptions;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer};
use dashd_p2p::{NetHandler, Registry, Seams};
use secp256k1::{Message, PublicKey, SecretKey};

const DSA: CommandString = CommandString::from_static("dsa");
const DSI: CommandString = CommandString::from_static("dsi");
const DSS: CommandString = CommandString::from_static("dss");
const DSSU: CommandString = CommandString::from_static("dssu");
const DSQ: CommandString = CommandString::from_static("dsq");

#[derive(Clone)]
struct HostChain(Arc<AtomicBool>);

impl ChainView for HostChain {
    fn tip(&self) -> Option<(i32, BlockHash)> {
        None
    }
    fn v24_active_at_tip(&self) -> bool {
        true
    }
    fn v24_active_after_tip(&self) -> bool {
        true
    }
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        false
    }
    fn blockchain_synced(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Clone, Copy)]
struct Clock(i64);
impl ServerClock for Clock {
    fn now(&self) -> i64 {
        self.0
    }
    fn adjusted_time(&self) -> i64 {
        self.0
    }
}

type Runtime = CoinJoinServer<
    HostChain,
    FakeMnLists,
    FakeActiveMn,
    FakeMempool,
    FakePeerSink,
    Clock,
    FakeRandom,
>;

fn script(tag: u8) -> Vec<u8> {
    let mut s = vec![0x76, 0xa9, 0x14];
    s.extend([tag; 20]);
    s.extend([0x88, 0xac]);
    s
}

fn input_secret(tag: u8) -> SecretKey {
    SecretKey::from_secret_bytes([tag; 32]).unwrap()
}

fn input_script(tag: u8) -> Vec<u8> {
    let key = input_secret(tag);
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend(hash160(&PublicKey::from_secret_key(&key).serialize()));
    script.extend([0x88, 0xac]);
    script
}

fn signed_input(tx: &Transaction, tag: u8) -> TxIn {
    let prevout = OutPoint::new(TxHash::from([50 + tag; 32]), 0);
    let index = tx.inputs().iter().position(|i| i.prevout() == prevout).unwrap();
    let hash = signature_hash(&input_script(tag), tx, index, 0x81);
    let mut signature = secp256k1::ecdsa::sign(Message::from_digest(hash), &input_secret(tag))
        .serialize_der()
        .to_vec();
    signature.push(0x81);
    let mut script_sig = Vec::new();
    push_data(&mut script_sig, &signature);
    push_data(&mut script_sig, &PublicKey::from_secret_key(&input_secret(tag)).serialize());
    TxIn::new(prevout, script_sig.into(), tx.inputs()[index].sequence())
}

fn collateral(tag: u8) -> (Transaction, OutPoint) {
    let prevout = OutPoint::new(TxHash::from([tag; 32]), 0);
    let tx = Transaction::new(
        2,
        TxType::Spend,
        vec![TxIn::new(prevout, Vec::new().into(), u32::MAX)],
        vec![TxOut::from_duffs(10_000, script(tag).into())],
        0,
        Vec::new(),
    );
    (tx, prevout)
}

fn operator_key() -> BlsPkValue {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    BlsPkValue::from_canonical(&key.public_key().to_bytes(), BlsSchemeKind::Basic).unwrap()
}

fn signed_queue(denom: i32, ready: bool) -> DsQueue {
    let pro_tx_hash = TxHash::from([9; 32]);
    let unsigned = DsQueue::new(denom, pro_tx_hash, SIM_START, ready, Vec::new());
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let sig = key.sign_with(unsigned.signature_hash().as_bytes(), BlsSigId::Basic);
    DsQueue::new(denom, pro_tx_hash, SIM_START, ready, sig.to_bytes().to_vec())
}

fn server(synced: bool) -> Runtime {
    let active = FakeActiveMn::default();
    let mut mns = FakeMnLists::default();
    mns.by_collateral.insert(
        active.outpoint,
        MixingMasternode {
            pro_tx_hash: TxHash::from([9; 32]),
            collateral: active.outpoint,
            operator_key: operator_key(),
        },
    );
    mns.valid.insert(
        TxHash::from([9; 32]),
        MixingMasternode {
            pro_tx_hash: TxHash::from([9; 32]),
            collateral: active.outpoint,
            operator_key: operator_key(),
        },
    );
    mns.enabled = 10;
    CoinJoinServer::new(
        PoolLimits {
            min_participants: 2,
            max_participants: 2,
        },
        HostChain(Arc::new(AtomicBool::new(synced))),
        mns,
        active,
        FakeMempool::default(),
        FakePeerSink::default(),
        Clock(SIM_START),
        FakeRandom::default(),
        MixingCounters::default(),
        DstxRuntime::new(PoolLimits {
            min_participants: 2,
            max_participants: 2,
        }),
    )
}

fn service(n: u8) -> Service {
    Service {
        addr: dash_p2p::NetAddr::ipv4([10, 0, 0, n]),
        port: 40000 + u16::from(n),
    }
}

fn queue_server() -> (Runtime, Vec<Transaction>) {
    let mut s = server(true);
    let mut collaterals = Vec::new();
    for tag in 1..=2 {
        let (tx, prevout) = collateral(tag);
        s.mempool_mut().coins.insert(
            prevout,
            dash_coinjoin::inouts::ViewCoin {
                out: TxOut::from_duffs(20_000, script(tag).into()),
                in_mempool: false,
            },
        );
        s.mempool_mut().accept_fees.insert(txid(&tx), 0);
        s.process_dsa(
            service(tag),
            dash_p2p::ProtocolVersion::V24_0,
            &DsAccept::new(16, tx.clone(), None),
        );
        collaterals.push(tx);
    }
    (s, collaterals)
}

fn signing_server() -> Runtime {
    let (mut s, collaterals) = queue_server();
    s.tick(false);
    for (tag, collateral) in (1..=2).zip(collaterals) {
        s.peers_mut().connected.insert(service(tag));
        let prevout = OutPoint::new(TxHash::from([50 + tag; 32]), 0);
        s.mempool_mut().coins.insert(
            prevout,
            dash_coinjoin::inouts::ViewCoin {
                out: TxOut::from_duffs(SMALLEST_DENOMINATION, input_script(tag).into()),
                in_mempool: false,
            },
        );
        let input = TxIn::new(prevout, Vec::new().into(), u32::MAX);
        s.process_dsi(
            service(tag),
            DsEntry::new(
                vec![input],
                collateral,
                vec![TxOut::from_duffs(SMALLEST_DENOMINATION, script(tag + 10).into())],
            ),
        );
    }
    s
}

fn node(runtime: impl NetHandler + 'static) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let identity = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut identity = identity;
    identity.masternode = true;
    let mut registry = Registry::new();
    registry.register(Box::new(runtime));
    let sim = Sim::with(
        params,
        identity,
        ProcessorOptions::default(),
        registry,
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    sim
}

fn peer(sim: &mut Sim, n: u8) -> SimPeer {
    let p = sim.accept_from(n, dash_p2p::NetAddr::ipv4([10, 0, 0, n]), Permissions::NONE).unwrap();
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    sim.pass();
    sim.sent(&p);
    p
}

fn replies(sim: &mut Sim, p: &SimPeer) -> Vec<(CommandString, Vec<u8>)> {
    sim.sent(p).into_iter().filter(|(c, _)| *c == DSSU).collect()
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn message_id(sim: &mut Sim, p: &SimPeer) -> i32 {
    let (_, bytes) = replies(sim, p).pop().expect("status");
    DsStatusUpdate::decode(&mut &bytes[..]).unwrap().message_id()
}

#[test]
fn unsynced_server_commands_are_silent_before_decode() {
    let mut sim = node(server(false));
    let p = peer(&mut sim, 1);
    for cmd in [DSA, DSI, DSS, DSQ] {
        assert!(sim.admitted(&p, cmd.as_str(), vec![0xff]).is_ok());
        sim.pass();
        assert!(sim.sent(&p).is_empty(), "{cmd}");
    }
}

#[test]
fn non_masternode_does_not_route_server_messages() {
    let mut runtime = server(true);
    let queue = signed_queue(1 << 4, false);
    let queue_hash = queue.hash();
    let outcome = runtime.process_dsq(queue);
    assert!(matches!(outcome, dash_coinjoin::queue::DsqOutcome::Relay(hash) if hash == queue_hash));
    let inv = Inventory::new(InvType::Dsq, queue_hash);
    runtime.set_masternode(false);
    assert!(!runtime.already_have(&inv));
    let mut sim = node(runtime);
    let p = peer(&mut sim, 1);
    for cmd in [DSA, DSI, DSS, DSQ] {
        assert!(sim.admitted(&p, cmd.as_str(), vec![0xff]).is_ok());
        sim.pass();
        assert!(sim.sent(&p).is_empty(), "{cmd}");
    }
    sim.deliver(&p, &[("getdata", encode(&dash_p2p_core::GetData::new(vec![inv])))]);
    sim.pass();
    assert!(sim.sent(&p).iter().all(|(cmd, _)| *cmd != DSQ));
}

#[test]
fn every_bl180_gate_precedes_decode_and_score() {
    let mut sim = node(server(true));
    let p = peer(&mut sim, 1);
    assert!(sim.admitted(&p, DSI.as_str(), vec![0xff]).is_ok());
    sim.pass();
    assert_eq!(message_id(&mut sim, &p), 16); // ERR_SESSION
    assert_eq!(replies(&mut sim, &p).len(), 0);
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);

    sim.sent(&p);
    assert!(sim.admitted(&p, DSS.as_str(), vec![0xff]).is_ok());
    sim.pass();
    assert_eq!(message_id(&mut sim, &p), 16);
    assert_eq!(replies(&mut sim, &p).len(), 0);

    let (mut full_runtime, _) = queue_server();
    full_runtime.tick(false);
    let mut full = node(full_runtime);
    let fp = peer(&mut full, 1);
    assert!(full.admitted(&fp, DSA.as_str(), vec![0xff]).is_ok());
    full.pass();
    assert_eq!(message_id(&mut full, &fp), 14); // ERR_QUEUE_FULL
    assert_eq!(replies(&mut full, &fp).len(), 0);
    assert_eq!(full.shared.misbehavior(fp.handle.id()).unwrap().0, 0);
}

#[test]
fn dss_count_gate_distinguishes_over_cap_from_malformed() {
    let runtime = signing_server();
    assert_eq!(runtime.session().entries().len(), 2, "state={:?}", runtime.session().state());
    let mut sim = node(runtime);
    let outsider = peer(&mut sim, 3);
    assert!(sim.admitted(&outsider, DSS.as_str(), vec![0xff]).is_ok());
    sim.pass();
    assert_eq!(message_id(&mut sim, &outsider), 6); // ERR_INVALID_INPUT
    assert_eq!(sim.shared.misbehavior(outsider.handle.id()).unwrap().0, 0);
    let p = peer(&mut sim, 1);
    assert!(sim.admitted(&p, DSS.as_str(), vec![181]).is_ok());
    sim.pass();
    assert_eq!(message_id(&mut sim, &p), 9); // ERR_MAXIMUM
    assert_eq!(replies(&mut sim, &p).len(), 0);
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
    sim.sent(&p);
    let malformed = vec![0xfd, 1, 0];
    assert!(sim.admitted(&p, DSS.as_str(), malformed).is_err());
    sim.pass();
    assert!(replies(&mut sim, &p).is_empty());
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);

    sim.sent(&p);
    for (oversized, decode_error) in
        [(vec![0xfe, 0, 0, 0, 4], true), (vec![0xfe, 0, 0, 0, 2], false)]
    {
        let admitted = sim.admitted(&p, DSS.as_str(), oversized);
        assert_eq!(admitted.is_err(), decode_error);
        sim.pass();
        if decode_error {
            assert!(replies(&mut sim, &p).is_empty());
        } else {
            assert_eq!(message_id(&mut sim, &p), 9); // ERR_MAXIMUM at MAX_SIZE
            assert!(replies(&mut sim, &p).is_empty());
        }
        assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 0);
        sim.sent(&p);
    }
}

#[test]
fn masternode_dsq_forgets_and_relays_valid_queue() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut runtime = server(true);
    runtime.peers_mut().observer = Some(Arc::clone(&calls));
    let mut sim = node(runtime);
    let p = peer(&mut sim, 1);
    let queue = signed_queue(1 << 4, false);
    let inv = Inventory::new(InvType::Dsq, queue.hash());
    assert!(sim.admitted(&p, DSQ.as_str(), encode(&queue)).is_ok());
    sim.pass();
    let calls = calls.lock().unwrap();
    assert!(matches!(calls.as_slice(), [
        dashd_coinjoin::testing::PeerCall::ForgetObjectRequest(forget),
        dashd_coinjoin::testing::PeerCall::RelayDsq(relay),
    ] if *forget == inv && relay == &queue));
}

#[test]
fn masternode_dsq_invalid_denomination_scores_ten() {
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut runtime = server(true);
    runtime.peers_mut().observer = Some(Arc::clone(&calls));
    let mut sim = node(runtime);
    let p = peer(&mut sim, 1);
    let queue = signed_queue(3, false);
    assert!(sim.admitted(&p, DSQ.as_str(), encode(&queue)).is_ok());
    sim.pass();
    assert_eq!(sim.shared.misbehavior(p.handle.id()).unwrap().0, 10);
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn tick_has_sync_shutdown_gates_and_core_effect_order() {
    let mut s = server(false);
    s.tick(false);
    assert!(s.peers().calls.is_empty());
    let (mut s, _) = queue_server();
    s.mempool().calls.lock().unwrap().clear();
    s.peers_mut().calls.clear();
    s.tick(false);
    assert!(matches!(
        s.peers().calls.as_slice(),
        [dashd_coinjoin::testing::PeerCall::RelayDsq(queue)] if queue.ready()
    ));
    let mut gated = server(false);
    gated.tick(true);
    assert!(gated.peers().calls.is_empty());
}

#[test]
fn err_mn_list_is_emitted_after_dsa_decode() {
    let mut s = server(true);
    s.mns_mut().by_collateral.clear();
    let (tx, prevout) = collateral(3);
    s.mempool_mut().coins.insert(
        prevout,
        dash_coinjoin::inouts::ViewCoin {
            out: TxOut::from_duffs(20_000, script(3).into()),
            in_mempool: false,
        },
    );
    s.mempool_mut().accept_fees.insert(txid(&tx), 0);
    s.process_dsa(service(3), dash_p2p::ProtocolVersion::V24_0, &DsAccept::new(1, tx, None));
    let Some(dashd_coinjoin::testing::PeerCall::PushToAddr(_, msg)) = s.peers().calls.first()
    else {
        panic!("missing ERR_MN_LIST");
    };
    let status = DsStatusUpdate::decode(&mut &msg.payload[..]).unwrap();
    assert_eq!(status.message_id(), 10); // ERR_MN_LIST
}

#[test]
fn consume_collateral_has_four_admission_outcomes_and_no_priority_bump() {
    for (fee, submit, relay) in [
        (None, false, false),
        (Some(10_000_001), true, false),
        (Some(0), false, false),
        (Some(0), true, true),
    ] {
        let mut s = server(true);
        let (tx, prevout) = collateral(4);
        s.mempool_mut().coins.insert(
            prevout,
            dash_coinjoin::inouts::ViewCoin {
                out: TxOut::from_duffs(20_000, script(4).into()),
                in_mempool: true,
            },
        );
        let id = txid(&tx);
        if let Some(fee) = fee {
            s.mempool_mut().accept_fees.insert(id, fee);
        }
        s.mempool_mut().submit_results.insert(id, submit);
        s.execute_effects(vec![ServerEffect::ConsumeCollateral(tx)]);
        assert!(
            !s.mempool()
                .calls
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, dashd_coinjoin::testing::MempoolCall::Prioritise(_, _)))
        );
        assert_eq!(
            s.peers()
                .calls
                .iter()
                .filter(|c| matches!(c, dashd_coinjoin::testing::PeerCall::RelayTransaction(_)))
                .count(),
            usize::from(relay)
        );
        let calls = s.mempool().calls.lock().unwrap().clone();
        match (fee, submit) {
            (None, _) | (Some(10_000_001), _) => {
                assert!(matches!(
                    calls.as_slice(),
                    [dashd_coinjoin::testing::MempoolCall::TestAccept(_)]
                ));
            }
            (Some(0), false) => {
                assert!(matches!(
                    calls.as_slice(),
                    [
                        dashd_coinjoin::testing::MempoolCall::TestAccept(_),
                        dashd_coinjoin::testing::MempoolCall::Submit(_)
                    ]
                ));
            }
            (Some(0), true) => {
                assert!(matches!(
                    calls.as_slice(),
                    [
                        dashd_coinjoin::testing::MempoolCall::TestAccept(_),
                        dashd_coinjoin::testing::MempoolCall::Submit(_)
                    ]
                ));
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn effect_wire_bytes_and_disconnected_push_are_preserved() {
    let mut s = server(true);
    let connected = service(7);
    s.peers_mut().connected.insert(connected.clone());
    let status = DsStatusUpdate::new(1, 2, 1, 20);
    s.execute_effects(vec![
        ServerEffect::Status {
            peer: connected.clone(),
            update: status.clone(),
        },
        ServerEffect::FinalTx {
            peer: connected.clone(),
            session_id: 1,
            tx: Transaction::new(2, TxType::Spend, Vec::new(), Vec::new(), 0, Vec::new()),
        },
        ServerEffect::Complete {
            peer: service(8),
            session_id: 1,
            message: dash_coinjoin::pool::PoolMessage::MsgSuccess,
        },
    ]);
    assert!(matches!(s.peers().calls.as_slice(), [
        dashd_coinjoin::testing::PeerCall::PushToAddr(_, a),
        dashd_coinjoin::testing::PeerCall::PushToAddr(_, b),
        dashd_coinjoin::testing::PeerCall::PushToAddr(_, c),
    ] if a.command == DSSU && b.command == CommandString::from_static("dsf") && c.command == CommandString::from_static("dsc")));
    assert_eq!(
        match &s.peers().calls[0] {
            dashd_coinjoin::testing::PeerCall::PushToAddr(_, msg) => msg.payload.clone(),
            _ => unreachable!(),
        },
        encode(&status)
    );
    assert_eq!(
        match &s.peers().calls[1] {
            dashd_coinjoin::testing::PeerCall::PushToAddr(_, msg) => msg.payload.clone(),
            _ => unreachable!(),
        },
        encode(&dash_p2p_core::DsFinalTx::new(
            1,
            Transaction::new(2, TxType::Spend, Vec::new(), Vec::new(), 0, Vec::new())
        ))
    );
    assert_eq!(
        match &s.peers().calls[2] {
            dashd_coinjoin::testing::PeerCall::PushToAddr(_, msg) => msg.payload.clone(),
            _ => unreachable!(),
        },
        encode(&dash_p2p_core::DsComplete::new(
            1,
            dash_coinjoin::pool::PoolMessage::MsgSuccess as i32
        ))
    );
    assert_eq!(s.peers().calls.len(), 3);
}

#[test]
fn scripted_random_source_is_used_for_session_and_multiple_charges() {
    let mut s = server(true);
    s.random_mut().draws = vec![123];
    let (tx, prevout) = collateral(5);
    s.mempool_mut().coins.insert(
        prevout,
        dash_coinjoin::inouts::ViewCoin {
            out: TxOut::from_duffs(20_000, script(5).into()),
            in_mempool: false,
        },
    );
    s.mempool_mut().accept_fees.insert(txid(&tx), 0);
    s.process_dsa(service(5), dash_p2p::ProtocolVersion::V24_0, &DsAccept::new(1, tx, None));
    assert_eq!(s.session().collateral_count(), 1);
    assert_eq!(s.session().session_id(), 124);
    assert_eq!(*s.random().calls.lock().unwrap(), vec![999_999]);
}

#[test]
fn successful_session_charges_several_collaterals_with_scripted_draws() {
    let mut runtime = signing_server();
    runtime.random_mut().draws = vec![99, 0];
    runtime.mempool_mut().default_accept_fee = Some(0);
    runtime.mempool_mut().default_submit = Some(true);
    let final_tx = runtime.session().final_tx().clone();
    for tag in 1..=2 {
        let (collateral, _) = collateral(tag);
        runtime.mempool_mut().accept_fees.insert(txid(&collateral), 0);
        runtime.mempool_mut().submit_results.insert(txid(&collateral), true);
    }
    runtime.mempool().calls.lock().unwrap().clear();
    runtime.peers_mut().calls.clear();
    runtime.random().calls.lock().unwrap().clear();
    let i1 = signed_input(&final_tx, 1);
    let i2 = signed_input(&final_tx, 2);
    runtime.process_dss(service(1), SignedInputs::Inputs(vec![i1]));
    runtime.process_dss(service(2), SignedInputs::Inputs(vec![i2]));
    let mempool_calls = runtime.mempool().calls.lock().unwrap().clone();
    let final_pos = mempool_calls
        .iter()
        .enumerate()
        .find_map(|(index, call)| {
            let dashd_coinjoin::testing::MempoolCall::Prioritise(hash, 10_000_000) = call else {
                return None;
            };
            matches!(
                mempool_calls.get(index + 1),
                Some(dashd_coinjoin::testing::MempoolCall::TestAccept(tx))
                    if txid(tx) == *hash
            )
            .then_some(index)
        })
        .expect("final transaction priority before test-accept");
    let final_id = match &mempool_calls[final_pos] {
        dashd_coinjoin::testing::MempoolCall::Prioritise(hash, _) => *hash,
        _ => unreachable!(),
    };
    let committed_tx = match &mempool_calls[final_pos + 1] {
        dashd_coinjoin::testing::MempoolCall::TestAccept(tx) => tx,
        _ => unreachable!(),
    };
    assert!(matches!(
        mempool_calls.get(final_pos..final_pos + 3),
        Some([
            dashd_coinjoin::testing::MempoolCall::Prioritise(hash, 10_000_000),
            dashd_coinjoin::testing::MempoolCall::TestAccept(tx),
            dashd_coinjoin::testing::MempoolCall::Submit(submitted),
        ]) if *hash == final_id && tx == committed_tx && submitted == committed_tx
    ));
    assert_eq!(
        mempool_calls
            .iter()
            .filter(|call| matches!(call, dashd_coinjoin::testing::MempoolCall::Submit(_)))
            .count(),
        2
    );
    let relay_pos = runtime
        .peers()
        .calls
        .iter()
        .position(|call| matches!(call, dashd_coinjoin::testing::PeerCall::RelayTxInv(inv) if inv.inv_type() == InvType::Dstx && inv.inv_hash() == final_id.into()))
        .expect("DSTX inventory relay");
    let dsc_pos = runtime
        .peers()
        .calls
        .iter()
        .position(|call| matches!(call, dashd_coinjoin::testing::PeerCall::PushToAddr(_, msg) if msg.command == CommandString::from_static("dsc")))
        .expect("completion push");
    assert!(relay_pos < dsc_pos);
    assert!(
        !runtime
            .peers()
            .calls
            .iter()
            .any(|call| matches!(call, dashd_coinjoin::testing::PeerCall::RelayInv(_)))
    );
    assert!(runtime.dstx().contains(&final_id));
    assert_eq!(*runtime.random().calls.lock().unwrap(), vec![100, 100]);
}

#[test]
fn final_commit_prioritises_by_point_one_dash() {
    let mut s = server(true);
    let (tx, prevout) = collateral(9);
    s.mempool_mut().coins.insert(
        prevout,
        dash_coinjoin::inouts::ViewCoin {
            out: TxOut::from_duffs(20_000, script(9).into()),
            in_mempool: true,
        },
    );
    s.mempool_mut().accept_fees.insert(txid(&tx), 0);
    s.mempool_mut().submit_results.insert(txid(&tx), true);
    assert!(s.commit_to_mempool(&tx));
    assert!(
        s.mempool()
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| matches!(c, dashd_coinjoin::testing::MempoolCall::Prioritise(_, 10_000_000)))
    );
}

#[test]
fn execute_effects_preserves_call_order_and_consume_collateral_contract() {
    let mut s = server(true);
    let (tx, prevout) = collateral(1);
    s.mempool_mut().coins.insert(
        prevout,
        dash_coinjoin::inouts::ViewCoin {
            out: TxOut::from_duffs(20_000, script(1).into()),
            in_mempool: true,
        },
    );
    let id = txid(&tx);
    s.mempool_mut().accept_fees.insert(id, 0);
    s.mempool_mut().submit_results.insert(id, true);
    let peer = Service::default();
    s.execute_effects(vec![ServerEffect::ConsumeCollateral(tx.clone())]);
    assert!(matches!(
        s.mempool().calls.lock().unwrap().as_slice(),
        [
            dashd_coinjoin::testing::MempoolCall::TestAccept(_),
            dashd_coinjoin::testing::MempoolCall::Submit(_),
        ]
    ));
    assert!(
        matches!(s.peers().calls.as_slice(), [dashd_coinjoin::testing::PeerCall::RelayTransaction(h)] if *h == id)
    );
    let _ = peer;
}

#[test]
fn commit_priority_is_point_one_dash_but_collateral_has_no_priority() {
    let mut s = server(true);
    let (tx, prevout) = collateral(2);
    s.mempool_mut().coins.insert(
        prevout,
        dash_coinjoin::inouts::ViewCoin {
            out: TxOut::from_duffs(20_000, script(2).into()),
            in_mempool: true,
        },
    );
    let id = txid(&tx);
    s.mempool_mut().accept_fees.insert(id, 0);
    s.mempool_mut().submit_results.insert(id, true);
    s.execute_effects(vec![ServerEffect::ConsumeCollateral(tx.clone())]);
    assert!(
        !s.mempool()
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| matches!(c, dashd_coinjoin::testing::MempoolCall::Prioritise(_, _)))
    );
}
