//! Transaction-filter hooks and per-peer relay state before the transaction handlers land.

use std::sync::{Arc, Mutex};

use dash_consensus::{Block, Header, Tx, TxRef};
use dash_num::Hash256;
use dash_p2p::{ConnectionType, Permissions};
use dash_p2p_core::{Inv, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{
    BlockHash, BlockHeader, MerkleRoot, OutPoint, Transaction, TxHash, TxIn, TxType,
};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::seams::OrphanageHooks;
use dashd_p2p::processing::signals::SyncSignal;
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimChain};
use dashd_p2p::{ProcessorOptions, Registry, Seams};

fn sim(seams: Seams) -> Sim {
    let p = Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&p, &IdentityOptions::new()).unwrap();
    Sim::with(p, id, ProcessorOptions::default(), Registry::new(), seams, |_| {})
}

fn tx(tag: u32, prevouts: &[OutPoint]) -> TxRef {
    let inputs =
        prevouts.iter().map(|prevout| TxIn::new(*prevout, vec![].into(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(1, TxType::Spend, inputs, vec![], tag, vec![])))
}

fn unlock(signature: u8) -> TxRef {
    let mut payload = vec![2];
    payload.extend_from_slice(&7u64.to_le_bytes());
    payload.extend_from_slice(&1000u32.to_le_bytes());
    payload.extend_from_slice(&100u32.to_le_bytes());
    payload.extend_from_slice(&[0xab; 32]);
    payload.extend_from_slice(&[signature; 96]);
    Arc::new(Tx::new(Transaction::new(3, TxType::new(9), vec![], vec![], 0, payload)))
}

fn block(txs: Vec<TxRef>) -> Arc<Block> {
    Arc::new(Block::new(
        Header::new(BlockHeader::new(
            4,
            BlockHash::default(),
            MerkleRoot::default(),
            1,
            0x207fffff,
            0,
        )),
        txs,
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct ErasedBlock {
    txids: Vec<TxHash>,
    prevouts: Vec<OutPoint>,
}

#[derive(Clone, Default)]
struct RecordingOrphanage(Arc<Mutex<Vec<ErasedBlock>>>);

impl OrphanageHooks for RecordingOrphanage {
    fn erase_for_block(&mut self, block: &Block) {
        self.0.lock().unwrap().push(ErasedBlock {
            txids: block.txs().iter().map(|tx| tx.txid()).collect(),
            prevouts: block
                .txs()
                .iter()
                .flat_map(|tx| tx.raw().inputs().iter().map(|input| input.prevout()))
                .collect(),
        });
    }
}

#[test]
fn connect_records_stable_ids_and_only_the_mined_unlock_instance() {
    let orphanage = RecordingOrphanage::default();
    let mut sim = sim(Seams {
        orphanage: Box::new(orphanage.clone()),
        ..Seams::default()
    });
    sim.lp.logic_mut().drain_sync_signals();
    let prevout = OutPoint::new(TxHash::from_lendian([17; 32]), 1);
    let ordinary = tx(1, &[prevout]);
    let mined = unlock(1);
    let other_instance = unlock(2);
    assert_eq!(mined.txid(), other_instance.txid());
    assert_ne!(mined.instance_hash(), other_instance.instance_hash());
    sim.lp.logic_mut().recent_rejects.insert(&[42; 32]);
    sim.shared.transactions_connected(&block(vec![ordinary.clone(), mined.clone()]));
    assert!(!sim.lp.logic().recent_confirmed.contains(&ordinary.txid().to_lendian()));
    sim.lp.logic_mut().drain_sync_signals();
    let confirmed = &sim.lp.logic().recent_confirmed;
    assert!(confirmed.contains(&ordinary.txid().to_lendian()));
    assert!(confirmed.contains(&mined.txid().to_lendian()));
    assert!(confirmed.contains(&mined.instance_hash().to_lendian()));
    assert!(!confirmed.contains(&other_instance.instance_hash().to_lendian()));
    assert!(sim.lp.logic().recent_rejects.contains(&[42; 32]));
    assert_eq!(sim.shared.signals().queue.held(), (0, 0));
    assert_eq!(
        *orphanage.0.lock().unwrap(),
        [ErasedBlock {
            txids: vec![ordinary.txid(), mined.txid()],
            prevouts: vec![prevout]
        }]
    );
}

#[test]
fn disconnect_resets_confirmed_and_reconnect_repopulates_in_signal_order() {
    let mut sim = sim(Seams::default());
    sim.lp.logic_mut().drain_sync_signals();
    let old = tx(1, &[]);
    let new = tx(2, &[]);
    sim.shared.transactions_connected(&block(vec![old.clone()]));
    sim.lp.logic_mut().drain_sync_signals();
    sim.lp.logic_mut().recent_rejects.insert(&[42; 32]);
    sim.shared.block_disconnected();
    sim.shared.transactions_connected(&block(vec![new.clone()]));
    sim.lp.logic_mut().drain_sync_signals();
    assert!(!sim.lp.logic().recent_confirmed.contains(&old.txid().to_lendian()));
    assert!(sim.lp.logic().recent_confirmed.contains(&new.txid().to_lendian()));
    assert!(sim.lp.logic().recent_rejects.contains(&[42; 32]));
    sim.shared.block_disconnected();
    sim.lp.logic_mut().drain_sync_signals();
    assert!(!sim.lp.logic().recent_confirmed.contains(&new.txid().to_lendian()));
}

#[test]
fn transaction_signals_wait_for_their_publication_and_account_for_block_bytes() {
    let mut sim = sim(Seams::default());
    let obs = sim.lp.logic_mut().drain_sync_signals();
    let tx = tx(1, &[]);
    let block = block(vec![tx.clone()]);
    let queue = &sim.shared.signals().queue;
    sim.shared.block_checked(block.hash(), None, true);
    sim.shared.transactions_connected(&block);
    sim.shared.block_disconnected();
    assert!(queue.held().1 >= block.size() as usize);
    assert!(queue.drain(obs.seq).is_empty());
    assert!(!sim.lp.logic().recent_confirmed.contains(&tx.txid().to_lendian()));
    let mut drained = queue.drain(obs.seq + 1);
    let checked = drained.next().unwrap();
    assert!(matches!(&*checked, SyncSignal::BlockChecked { .. }));
    drop(checked);
    let connected = drained.next().unwrap();
    assert!(
        matches!(&*connected, SyncSignal::BlockConnected { block: queued } if Arc::ptr_eq(queued, &block))
    );
    drop(connected);
    let disconnected = drained.next().unwrap();
    assert!(matches!(&*disconnected, SyncSignal::BlockDisconnected));
    drop(disconnected);
    assert!(drained.next().is_none());
    drop(drained);
    assert_eq!(queue.held(), (0, 0));
}

#[test]
fn relay_fields_start_empty_and_are_isolated_per_peer() {
    let sim = sim(Seams::default());
    let a = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    let b = sim.connect(2, ConnectionType::OutboundFullRelay, Permissions::NONE);
    for peer in [&a, &b] {
        sim.shared
            .with_state(peer.handle.id(), |st| {
                assert_eq!(st.last_mempool_req, 0);
                assert!(!st.recently_announced_invs.contains(&[42; 32]));
                assert!(!st.send_mempool);
                assert_eq!(st.next_inv_send_us, 0);
            })
            .unwrap();
    }
    sim.shared.with_state_mut(a.handle.id(), |st| {
        st.last_mempool_req = 42;
        st.recently_announced_invs.insert(&[42; 32]);
    });
    sim.shared
        .with_state(b.handle.id(), |st| {
            assert_eq!(st.last_mempool_req, 0);
            assert!(!st.recently_announced_invs.contains(&[42; 32]));
        })
        .unwrap();
}

#[test]
fn rejected_filter_resets_on_a_new_tip_and_survives_repeated_invs_at_that_tip() {
    let mut sim = sim(Seams::default());
    let peer = sim.connect(1, ConnectionType::OutboundFullRelay, Permissions::NONE);
    sim.handshake(&peer, PeerVersionMsg::v24(SIM_START));
    let inv = Inv::new(vec![Inventory::new(InvType::Tx, Hash256::from_lendian([42; 32]))]);
    let mut bytes = Vec::new();
    inv.encode(&mut bytes);
    sim.lp.logic_mut().recent_rejects.insert(&[42; 32]);
    sim.deliver(&peer, &[("inv", bytes.clone())]);
    sim.pass();
    assert!(!sim.lp.logic().recent_rejects.contains(&[42; 32]));
    sim.lp.logic_mut().recent_rejects.insert(&[42; 32]);
    sim.deliver(&peer, &[("inv", bytes.clone())]);
    sim.pass();
    assert!(sim.lp.logic().recent_rejects.contains(&[42; 32]));
    let tip = sim.chain.add(&SimChain::mine(sim.chain.params(), 1, 1));
    sim.chain.set_tip(tip.id);
    assert!(sim.lp.logic().recent_rejects.contains(&[42; 32]));
    sim.deliver(&peer, &[("inv", bytes)]);
    sim.pass();
    assert!(!sim.lp.logic().recent_rejects.contains(&[42; 32]));
}
