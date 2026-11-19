#![cfg(feature = "testing")]
//! [`RuntimeObjects`] behind the real `on_tx`, relay and getdata paths of the simulated node:
//! a port of Dash Core v24.0.0's `p2p_dstx.py` and the DSTX serving paths of
//! `net_processing.cpp`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use dash_coinjoin::dstx::{DSTX_PRIORITY_DELTA, PoolLimits, dstx_signature_hash, txid};
use dash_coinjoin::queue::{MixingCounters, MixingMasternode};
use dash_num::Hash256;
use dash_p2p::{CommandString, ConnectionType, Permissions};
use dash_p2p_core::{DsQueue, DsTx, GetData, Inv, InvType, Inventory, NotFound};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashd_coinjoin::dsq::DsqRuntime;
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::objects::RuntimeObjects;
use dashd_coinjoin::testing::{FakeChainView, FakeMnLists, Locked};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::seams::TxPool;
use dashd_p2p::sim::txintake::SimTxIntakeHooks;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

/// p2p_dstx.py's constants.
const UNKNOWN_MN_SCORE: i32 = 1;
const INVALID_DSTX_SCORE: i32 = 10;
const PREMATURE_DSTX_SCORE: i32 = 1;
const DISCOURAGEMENT_THRESHOLD: i32 = 100;

struct Node {
    sim: Sim,
    pool: SimTxPool,
    hooks: SimTxIntakeHooks,
    runtime: Arc<DstxRuntime>,
    chain: Arc<Locked<FakeChainView>>,
    mns: Arc<Locked<FakeMnLists>>,
    meta: Arc<Mutex<MixingCounters>>,
}

impl Node {
    fn new() -> Self {
        let params =
            Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
        let pool = SimTxPool::default();
        let hooks = SimTxIntakeHooks::default();
        let runtime = Arc::new(DstxRuntime::new(PoolLimits::of(&params)));
        let chain = Arc::new(Locked::new(FakeChainView {
            tip: Some((1, BlockHash::from([1; 32]))),
            synced: true,
            ..FakeChainView::default()
        }));
        let mns = Arc::new(Locked::<FakeMnLists>::default());
        let meta = Arc::new(Mutex::new(MixingCounters::default()));
        let objects = RuntimeObjects::new(
            Arc::clone(&runtime),
            Arc::clone(&chain),
            Arc::clone(&mns),
            Arc::clone(&meta),
            None,
        );
        let id = local_identity(&params, &IdentityOptions::new()).unwrap();
        let sim = Sim::with(
            params,
            id,
            ProcessorOptions::default(),
            Registry::new(),
            Seams {
                txpool: Arc::new(pool.clone()),
                coinjoin: Box::new(objects),
                tx_intake: Box::new(hooks.clone()),
                block_sync: Box::new(BlockDownload::default()),
                ..Seams::default()
            },
            |_| {},
        );
        sim.chain.set_ibd(false);
        Self {
            sim,
            pool,
            hooks,
            runtime,
            chain,
            mns,
            meta,
        }
    }

    fn peer(&mut self, n: u8, version: i32) -> SimPeer {
        let peer = self.sim.connect(n, ConnectionType::Inbound, Permissions::NONE);
        self.sim.handshake(
            &peer,
            PeerVersionMsg {
                version,
                ..PeerVersionMsg::v24(SIM_START)
            },
        );
        assert!(peer.handle.successfully_connected());
        peer
    }

    fn send(&mut self, peer: &SimPeer, dstx: &DsTx) {
        self.sim.admitted(peer, "dstx", encode(dstx)).unwrap();
    }

    fn score(&self, peer: &SimPeer) -> i32 {
        self.sim.shared.misbehavior(peer.handle.id()).unwrap().0
    }

    fn set_v24(&self, at_tip: bool, after_tip: bool) {
        self.chain.with(|chain| {
            chain.v24_at_tip = at_tip;
            chain.v24_after_tip = after_tip;
        });
    }

    fn list(&self, mn: MixingMasternode) {
        self.mns.with(|mns| mns.at_depth.insert((0, mn.pro_tx_hash), mn));
    }

    fn invs(&self, peer: &SimPeer) -> Vec<Inventory> {
        self.sim
            .sent(peer)
            .into_iter()
            .filter(|(cmd, _)| *cmd == CommandString::INV)
            .flat_map(|(_, bytes)| Inv::decode(&mut &bytes[..]).unwrap().inventory().clone())
            .collect()
    }

    fn getdata(&mut self, peer: &SimPeer, inv: Inventory) -> Vec<(CommandString, Vec<u8>)> {
        self.sim.sent(peer);
        self.sim.admitted(peer, "getdata", encode(&GetData::new(vec![inv]))).unwrap();
        self.sim.sent_at_least(peer, 1)
    }

    fn announce(&mut self, peer: &SimPeer, inv: Inventory) {
        self.sim.admitted(peer, "inv", encode(&Inv::new(vec![inv]))).unwrap();
    }

    fn requested(&self, peer: &SimPeer) -> usize {
        self.sim.lp.logic().requested_objects().count(peer.handle.id())
    }
}

fn encode(value: &impl BaseCodec) -> Vec<u8> {
    let mut bytes = Vec::new();
    value.encode(&mut bytes);
    bytes
}

fn inv(kind: InvType, txid: TxHash) -> Inventory {
    Inventory::new(kind, Hash256::from_lendian(txid.to_lendian()))
}

/// A `uint256` from a small integer, as the functional test framework builds one.
fn uint256(n: u64) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    TxHash::from_lendian(bytes)
}

/// Mirrors the shape of p2p_dstx.py `make_dstx`: three 1:1 entries of the smallest
/// denomination, plus nine more inputs for a promotion; `m_protxHash` 1 and a dummy signature.
fn make_dstx(nonce: u64, promotion: bool) -> DsTx {
    let inputs = if promotion {
        12
    } else {
        3
    };
    let inputs = (1..=inputs)
        .map(|i| TxIn::new(OutPoint::new(uint256(nonce << 8 | i), 0), Vec::new().into(), u32::MAX))
        .collect();
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend([1; 20]);
    script.extend([0x88, 0xac]);
    let outputs = (0..3).map(|_| TxOut::from_duffs(100_001, script.clone().into())).collect();
    DsTx::new(
        Transaction::new(2, TxType::Spend, inputs, outputs, 0, Vec::new()),
        uint256(1),
        vec![1; 96],
        SIM_START,
    )
}

fn with_output_value(dstx: &DsTx, value: i64) -> DsTx {
    let tx = dstx.tx();
    let mut outputs = tx.outputs().to_vec();
    outputs[0] = TxOut::from_duffs(value, outputs[0].script_pubkey().clone());
    let tx = Transaction::new(2, TxType::Spend, tx.inputs().to_vec(), outputs, 0, Vec::new());
    DsTx::new(tx, dstx.pro_tx_hash(), dstx.sig().clone(), dstx.sig_time())
}

/// `make_dstx` signed by a masternode whose operator key the node knows.
fn signed(nonce: u64, promotion: bool) -> (DsTx, MixingMasternode) {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let d = make_dstx(nonce, promotion);
    let signature = key.sign_with(dstx_signature_hash(&d).as_bytes(), BlsSigId::Basic);
    let mn = MixingMasternode {
        pro_tx_hash: d.pro_tx_hash(),
        collateral: OutPoint::new(uint256(2), 1),
        operator_key: BlsPkValue::from_canonical(
            &key.public_key().to_bytes(),
            BlsSchemeKind::Basic,
        )
        .unwrap(),
    };
    (DsTx::new(d.tx().clone(), d.pro_tx_hash(), signature.to_bytes().to_vec(), d.sig_time()), mn)
}

/// Sends `dstx` from a fresh peer and returns its score; nothing reaches the mempool.
fn scored(node: &mut Node, n: u8, dstx: &DsTx) -> i32 {
    let peer = node.peer(n, 70242);
    node.send(&peer, dstx);
    assert!(node.pool.submissions().is_empty());
    assert!(node.hooks.priorities().is_empty());
    node.score(&peer)
}

#[test]
fn p2p_dstx_unknown_masternode_scores_one() {
    let mut node = Node::new();
    assert_eq!(scored(&mut node, 1, &make_dstx(1, false)), UNKNOWN_MN_SCORE);
}

#[test]
fn p2p_dstx_bad_structure_scores_ten() {
    let mut node = Node::new();
    let good = make_dstx(2, false);
    let bad = with_output_value(&good, 100_002);
    assert_eq!(scored(&mut node, 1, &bad), INVALID_DSTX_SCORE);
}

#[test]
fn p2p_dstx_unbalanced_far_from_v24_scores_ten() {
    let mut node = Node::new();
    assert_eq!(scored(&mut node, 1, &make_dstx(3, true)), INVALID_DSTX_SCORE);
}

#[test]
fn p2p_dstx_unbalanced_at_v24_boundary_is_premature() {
    let mut node = Node::new();
    node.set_v24(false, true);
    assert_eq!(scored(&mut node, 1, &make_dstx(4, true)), PREMATURE_DSTX_SCORE);
}

#[test]
fn p2p_dstx_unbalanced_with_v24_active_is_unknown_masternode() {
    let mut node = Node::new();
    node.set_v24(true, true);
    assert_eq!(scored(&mut node, 1, &make_dstx(5, true)), UNKNOWN_MN_SCORE);
}

#[test]
fn p2p_dstx_unknown_masternode_flood_discourages() {
    let mut node = Node::new();
    node.set_v24(true, true);
    let peer = node.peer(1, 70242);
    for nonce in 0..DISCOURAGEMENT_THRESHOLD {
        node.send(&peer, &make_dstx(1000 + u64::try_from(nonce).unwrap(), false));
        if nonce == DISCOURAGEMENT_THRESHOLD - 2 {
            node.sim.pass();
            assert!(!peer.handle.disconnect_requested(), "discouraged below the threshold");
        }
    }
    assert_eq!(node.score(&peer), DISCOURAGEMENT_THRESHOLD);
    node.sim.pass();
    assert!(peer.handle.disconnect_requested());
    assert!(node.sim.core.banlist().is_discouraged(&peer.handle.addr().service.addr));
    assert!(node.pool.submissions().is_empty());
}

#[test]
fn verified_dstx_priority_delta_is_applied_once_and_stored_after_admission() {
    let mut node = Node::new();
    let (dstx, mn) = signed(6, false);
    node.list(mn.clone());
    let peer = node.peer(1, 70242);
    let id = txid(dstx.tx());
    node.send(&peer, &dstx);
    assert_eq!(node.hooks.priorities(), [(id, DSTX_PRIORITY_DELTA)]);
    assert_eq!(node.pool.submissions().len(), 1);
    assert_eq!(node.runtime.get(&id).map(|stored| stored.dstx), Some(dstx.clone()));
    let mut counted = MixingCounters::default();
    counted.disallow_mixing(&mn.pro_tx_hash);
    assert_eq!(*node.meta.lock().unwrap(), counted, "the masternode is counted once");

    node.send(&peer, &dstx);
    assert_eq!(node.hooks.priorities(), [(id, DSTX_PRIORITY_DELTA)]);
    assert_eq!(node.pool.submissions().len(), 1);
    assert_eq!(node.score(&peer), 0);
}

#[test]
fn rejected_admission_keeps_no_dstx() {
    let mut node = Node::new();
    let (dstx, mn) = signed(7, false);
    node.list(mn);
    let peer = node.peer(1, 70242);
    node.pool.push_result(dashd_p2p::processing::seams::AcceptResult {
        result_type: dashd_p2p::processing::seams::AcceptResultType::Invalid,
        validation_result: dash_consensus::TxValidationResult::Consensus,
        reason: "reason".into(),
        debug_message: String::new(),
        vsize: None,
        base_fee: None,
    });
    node.send(&peer, &dstx);
    assert_eq!(node.hooks.priorities().len(), 1);
    assert!(!node.runtime.contains(&txid(dstx.tx())));
}

/// Accepts a V24 promotion from a 70242 sender and connects peers at 70240 and 70241.
fn relayed_promotion() -> (Node, DsTx, SimPeer, SimPeer, SimPeer) {
    let mut node = Node::new();
    node.set_v24(true, true);
    let (dstx, mn) = signed(8, true);
    node.list(mn);
    let sender = node.peer(1, 70242);
    let old = node.peer(2, 70240);
    let new = node.peer(3, 70241);
    node.send(&sender, &dstx);
    assert!(node.runtime.contains(&txid(dstx.tx())));
    node.sim.clock.advance(Duration::from_secs(10));
    node.sim.pass();
    (node, dstx, sender, old, new)
}

#[test]
fn unbalanced_dstx_is_announced_as_dstx_from_70241_and_as_tx_below() {
    let (node, dstx, sender, old, new) = relayed_promotion();
    let id = txid(dstx.tx());
    assert_eq!(node.invs(&old), [inv(InvType::Tx, id)]);
    assert_eq!(node.invs(&new), [inv(InvType::Dstx, id)]);
    assert_eq!(node.invs(&sender), []);
}

#[test]
fn getdata_for_dstx_serves_the_kept_wrapper() {
    let (mut node, dstx, _, old, new) = relayed_promotion();
    let id = txid(dstx.tx());
    assert_eq!(node.getdata(&new, inv(InvType::Dstx, id)), [(CommandString::DSTX, encode(&dstx))]);
    assert_eq!(node.getdata(&old, inv(InvType::Tx, id)), [(CommandString::TX, encode(dstx.tx()))]);
}

#[test]
fn already_have_answers_dstx_inventory_from_the_kept_wrapper() {
    let (mut node, dstx, _, old, new) = relayed_promotion();
    let id = txid(dstx.tx());
    node.pool.remove(&id);
    assert!(!node.pool.exists(&id));
    node.announce(&new, inv(InvType::Dstx, id));
    assert_eq!(node.requested(&new), 0, "MSG_DSTX is already had");
    node.announce(&old, inv(InvType::Tx, id));
    assert_eq!(node.requested(&old), 1, "MSG_TX is not");
}

#[test]
fn p2p_dstx_steps_in_order_on_one_node() {
    let mut node = Node::new();
    let p1 = node.peer(1, 70242);
    node.send(&p1, &make_dstx(1, false));
    assert_eq!(node.score(&p1), UNKNOWN_MN_SCORE);

    let p2 = node.peer(2, 70242);
    let bad = with_output_value(&make_dstx(2, false), 100_002);
    node.send(&p2, &bad);
    assert_eq!([node.score(&p1), node.score(&p2)], [UNKNOWN_MN_SCORE, INVALID_DSTX_SCORE]);

    let p3 = node.peer(3, 70242);
    node.send(&p3, &make_dstx(3, true));
    assert_eq!(
        [node.score(&p1), node.score(&p2), node.score(&p3)],
        [UNKNOWN_MN_SCORE, INVALID_DSTX_SCORE, INVALID_DSTX_SCORE]
    );

    node.set_v24(false, true);
    let p4 = node.peer(4, 70242);
    node.send(&p4, &make_dstx(4, true));
    assert_eq!(
        [node.score(&p1), node.score(&p2), node.score(&p3), node.score(&p4)],
        [UNKNOWN_MN_SCORE, INVALID_DSTX_SCORE, INVALID_DSTX_SCORE, PREMATURE_DSTX_SCORE]
    );

    node.set_v24(true, true);
    let p5 = node.peer(5, 70242);
    node.send(&p5, &make_dstx(5, true));
    assert_eq!(
        [node.score(&p1), node.score(&p2), node.score(&p3), node.score(&p4), node.score(&p5),],
        [
            UNKNOWN_MN_SCORE,
            INVALID_DSTX_SCORE,
            INVALID_DSTX_SCORE,
            PREMATURE_DSTX_SCORE,
            UNKNOWN_MN_SCORE,
        ]
    );

    let flood = node.peer(6, 70242);
    for step in 0..DISCOURAGEMENT_THRESHOLD {
        node.send(&flood, &make_dstx(1000 + u64::try_from(step).unwrap(), false));
        assert_eq!(node.score(&flood), step + 1);
        if step == DISCOURAGEMENT_THRESHOLD - 2 {
            node.sim.pass();
            assert!(!flood.handle.disconnect_requested());
            assert!(!node.sim.core.banlist().is_discouraged(&flood.handle.addr().service.addr));
        }
    }
    node.sim.pass();
    assert!(flood.handle.disconnect_requested());
    assert!(node.sim.core.banlist().is_discouraged(&flood.handle.addr().service.addr));
    for peer in [&p1, &p2, &p3, &p4, &p5] {
        assert!(!peer.handle.disconnect_requested());
    }
    assert!(node.pool.submissions().is_empty());
    assert!(node.hooks.priorities().is_empty());
}

fn node_for_queues(
    masternode: bool,
    chain: Arc<Locked<FakeChainView>>,
    mns: Arc<Locked<FakeMnLists>>,
    meta: Arc<Mutex<MixingCounters>>,
    queues: Option<DsqRuntime<Locked<FakeChainView>, Locked<FakeMnLists>>>,
) -> Node {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let pool = SimTxPool::default();
    let hooks = SimTxIntakeHooks::default();
    let runtime = Arc::new(DstxRuntime::new(PoolLimits::of(&params)));
    let objects = RuntimeObjects::new(
        Arc::clone(&runtime),
        Arc::clone(&chain),
        Arc::clone(&mns),
        Arc::clone(&meta),
        queues,
    );
    let mut id = local_identity(&params, &IdentityOptions::new()).unwrap();
    id.masternode = masternode;
    let sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            coinjoin: Box::new(objects),
            tx_intake: Box::new(hooks.clone()),
            block_sync: Box::new(BlockDownload::default()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    Node {
        sim,
        pool,
        hooks,
        runtime,
        chain,
        mns,
        meta,
    }
}

#[test]
fn getdata_for_dsq_uses_queue_by_hash_on_non_mn_only() {
    let chain = Arc::new(Locked::new(FakeChainView {
        tip: Some((1, BlockHash::from([1; 32]))),
        synced: true,
        ..FakeChainView::default()
    }));
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[7; 32]).unwrap();
    let pro_tx = uint256(1);
    let mn = MixingMasternode {
        pro_tx_hash: pro_tx,
        collateral: OutPoint::new(uint256(2), 1),
        operator_key: BlsPkValue::from_canonical(
            &key.public_key().to_bytes(),
            BlsSchemeKind::Basic,
        )
        .unwrap(),
    };
    let mns = Arc::new(Locked::new(FakeMnLists {
        enabled: 100,
        valid: [(pro_tx, mn)].into_iter().collect(),
        ..FakeMnLists::default()
    }));
    let meta = Arc::new(Mutex::new(MixingCounters::default()));
    let dsq = DsqRuntime::new(true, Arc::clone(&chain), Arc::clone(&mns), Arc::clone(&meta));

    let unsigned = DsQueue::new(1 << 4, pro_tx, SIM_START, false, Vec::new());
    let sig = key.sign_with(unsigned.signature_hash().as_bytes(), BlsSigId::Basic);
    let queue = DsQueue::new(unsigned.denom(), pro_tx, SIM_START, false, sig.to_bytes().to_vec());
    {
        let params =
            Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
        let id = local_identity(&params, &IdentityOptions::new()).unwrap();
        let mut registry = Registry::new();
        registry.register(Box::new(dsq.clone()));
        let mut seeder =
            Sim::with(params, id, ProcessorOptions::default(), registry, Seams::default(), |_| {});
        seeder.chain.set_ibd(false);
        let peer = seeder.connect(1, ConnectionType::Inbound, Permissions::NONE);
        seeder.handshake(&peer, PeerVersionMsg::v24(SIM_START));
        seeder.admitted(&peer, "dsq", encode(&queue)).unwrap();
        assert_eq!(dsq.get(&queue.hash()), Some(queue.clone()));
    }

    let held = Inventory::new(InvType::Dsq, queue.hash());
    let unknown = Inventory::new(InvType::Dsq, Hash256::from_lendian([9; 32]));
    let mut wallet_node =
        node_for_queues(false, Arc::clone(&chain), Arc::clone(&mns), Arc::clone(&meta), Some(dsq));
    let peer = wallet_node.peer(1, 70242);
    assert_eq!(wallet_node.getdata(&peer, held), [(CommandString::DSQ, encode(&queue))]);
    assert_eq!(
        wallet_node.getdata(&peer, unknown),
        [(CommandString::NOTFOUND, encode(&NotFound::new(vec![unknown])))]
    );

    let mut mn_node = node_for_queues(true, chain, mns, meta, None);
    let mn_peer = mn_node.peer(1, 70242);
    assert_eq!(
        mn_node.getdata(&mn_peer, held),
        [(CommandString::NOTFOUND, encode(&NotFound::new(vec![held])))]
    );
}
