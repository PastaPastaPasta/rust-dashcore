//! Core v24 transaction intake, including the orphan cases of p2p_invalid_tx and p2p_tx_download.

use std::cell::RefCell;
use std::sync::{Arc, Once};
use std::time::Duration;

use dash_consensus::primitives::CoreSerialize;
use dash_consensus::{Block, Header, Tx, TxRef, TxValidationResult};
use dash_num::Hash256;
use dash_p2p::{ConnectionType, Permissions};
use dash_p2p_core::{DsTx, GetData, Inv, InvType, Inventory};
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::{
    BlockHash, BlockHeader, MerkleRoot, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType,
};
use dash_types::{Numeric, codec::BaseCodec};
use dashd_p2p::fingerprint::{IdentityOptions, local_identity};
use dashd_p2p::processing::download::BlockDownload;
use dashd_p2p::processing::seams::{AcceptResult, AcceptResultType, DstxCheck, TxPool};
use dashd_p2p::sim::txintake::{SimCoinJoinObjects, SimTxIntakeHooks};
use dashd_p2p::sim::{PeerVersionMsg, SIM_START, Sim, SimPeer, SimTxPool};
use dashd_p2p::{ProcessorOptions, Registry, Seams};
use tracing::{Event, Metadata, Subscriber, field::Field, field::Visit, span};

thread_local! {
    static TX_LOGS: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

struct TxLogs;

impl Subscriber for TxLogs {
    fn register_callsite(&self, _: &'static Metadata<'static>) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.is_event()
            && metadata.target() == "mempoolrej"
            && TX_LOGS.with(|logs| logs.borrow().is_some())
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        struct Message(String);
        impl Visit for Message {
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        TX_LOGS.with(|logs| {
            if let Some(lines) = logs.borrow_mut().as_mut() {
                lines.push(message.0);
            }
        });
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}

fn install_tx_logs() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| tracing::subscriber::set_global_default(TxLogs).unwrap());
}

fn capture_tx_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    TX_LOGS.with(|logs| *logs.borrow_mut() = Some(Vec::new()));
    let result = f();
    let lines = TX_LOGS.with(|logs| logs.borrow_mut().take().unwrap_or_default());
    (result, lines)
}

fn tx(tag: u32, parents: &[TxHash]) -> TxRef {
    let inputs =
        parents.iter().map(|p| TxIn::new(OutPoint::new(*p, 0), vec![].into(), u32::MAX)).collect();
    Arc::new(Tx::new(Transaction::new(
        1,
        TxType::Spend,
        inputs,
        vec![TxOut::new(Default::default(), vec![0x51].into())],
        tag,
        vec![],
    )))
}

fn result(validation_result: TxValidationResult) -> AcceptResult {
    AcceptResult {
        result_type: if validation_result == TxValidationResult::Unset {
            AcceptResultType::Valid
        } else {
            AcceptResultType::Invalid
        },
        validation_result,
        reason: "reason".into(),
        debug_message: "debug detail".into(),
        vsize: None,
        base_fee: None,
    }
}

fn node(blocks_only: bool) -> (Sim, SimTxPool, SimCoinJoinObjects, SimTxIntakeHooks) {
    let pool = SimTxPool::default();
    let cj = SimCoinJoinObjects::default();
    let hooks = SimTxIntakeHooks::default();
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let sim = Sim::with(
        params,
        id,
        ProcessorOptions {
            blocks_only,
            ..ProcessorOptions::default()
        },
        Registry::new(),
        Seams {
            txpool: Arc::new(pool.clone()),
            coinjoin: Box::new(cj.clone()),
            tx_intake: Box::new(hooks.clone()),
            block_sync: Box::new(BlockDownload::default()),
            ..Seams::default()
        },
        |_| {},
    );
    sim.chain.set_ibd(false);
    (sim, pool, cj, hooks)
}

fn peer(sim: &mut Sim, n: u8, permissions: Permissions) -> SimPeer {
    let p = sim.connect(n, ConnectionType::OutboundFullRelay, permissions);
    sim.handshake(&p, PeerVersionMsg::v24(SIM_START));
    p
}

fn send(sim: &mut Sim, p: &SimPeer, tx: &TxRef) {
    sim.admitted(p, "tx", tx.raw().core_serialize()).unwrap();
}

fn queued(sim: &Sim, p: &SimPeer, hash: TxHash) -> bool {
    sim.shared
        .with_state(p.handle.id(), |s| {
            s.inventory_relay().lock().tx_inventory_to_send.contains(&hash)
        })
        .unwrap()
}

fn score(sim: &Sim, p: &SimPeer) -> i32 {
    sim.shared.with_state(p.handle.id(), |s| s.misbehavior().0).unwrap()
}

fn announce(sim: &mut Sim, p: &SimPeer, kind: InvType, hash: TxHash) {
    let inv = Inv::new(vec![Inventory::new(kind, Hash256::from_lendian(hash.to_lendian()))]);
    let mut raw = vec![];
    inv.encode(&mut raw);
    sim.admitted(p, "inv", raw).unwrap();
}

#[test]
fn ibd_tx_is_ignored_by_enabled_pool_and_out_of_ibd_logs_core_rejection() {
    install_tx_logs();
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let candidate = tx(14, &[]);

    sim.chain.set_ibd(true);
    let (_, ibd_logs) = capture_tx_logs(|| send(&mut sim, &p, &candidate));
    assert!(pool.submissions().is_empty(), "IBD must not call the enabled pool");
    assert!(ibd_logs.iter().all(|line| !line.contains("was not accepted")), "{ibd_logs:?}");

    sim.chain.set_ibd(false);
    pool.push_result(result(TxValidationResult::Consensus));
    let (_, out_of_ibd_logs) = capture_tx_logs(|| send(&mut sim, &p, &candidate));
    assert_eq!(pool.submissions().len(), 1);
    assert_eq!(
        out_of_ibd_logs,
        vec![format!(
            "{} from peer={} was not accepted: reason, debug detail",
            candidate.txid(),
            p.handle.id()
        )]
    );
}

#[test]
fn accepted_tx_queue_applies_relay_filters() {
    let (mut sim, pool, _, _) = node(false);
    let sender = peer(&mut sim, 1, Permissions::NONE);
    let no_relay = peer(&mut sim, 2, Permissions::NONE);
    let before_trickle = peer(&mut sim, 3, Permissions::NONE);
    let ready = peer(&mut sim, 4, Permissions::NONE);
    sim.shared
        .with_state(no_relay.handle.id(), |s| s.inventory_relay().lock().tx_relay_enabled = false)
        .unwrap();
    sim.shared
        .with_state(before_trickle.handle.id(), |s| {
            s.inventory_relay().lock().started = false;
        })
        .unwrap();
    let candidate = tx(15, &[]);
    send(&mut sim, &sender, &candidate);
    assert!(!queued(&sim, &no_relay, candidate.txid()));
    assert!(!queued(&sim, &before_trickle, candidate.txid()));
    assert!(!queued(&sim, &sender, candidate.txid()), "sender's known filter suppresses relay");
    assert!(queued(&sim, &ready, candidate.txid()));
    assert!(pool.exists(&candidate.txid()));
}

#[test]
fn accepted_tx_queues_all_peers_records_sender_and_ignores_duplicate() {
    let (mut sim, pool, _, _) = node(false);
    let a = peer(&mut sim, 1, Permissions::NONE);
    let b = peer(&mut sim, 2, Permissions::NONE);
    let c = peer(&mut sim, 3, Permissions::NONE);
    let t = tx(1, &[TxHash::from_lendian([1; 32])]);
    send(&mut sim, &a, &t);
    assert!(pool.exists(&t.txid()));
    assert!(queued(&sim, &b, t.txid()) && queued(&sim, &c, t.txid()));
    assert_eq!(a.handle.last_tx_time(), SIM_START);
    sim.shared
        .with_state(a.handle.id(), |s| {
            assert!(s.inventory_relay().lock().is_known(&t.txid().to_lendian()))
        })
        .unwrap();
    send(&mut sim, &a, &t);
    assert_eq!(pool.submissions().len(), 1);
    assert_eq!(pool.submissions()[0].1, Some(a.handle.id()));
}

#[test]
fn p2p_tx_download_fetches_all_unique_parents_as_dstx_and_retries_on_sender() {
    let (mut sim, pool, _, _) = node(false);
    let child_peer = peer(&mut sim, 1, Permissions::NONE);
    let parent_peer = peer(&mut sim, 2, Permissions::NONE);
    let parent = tx(1, &[]);
    let other = tx(2, &[]);
    let child = tx(3, &[parent.txid(), other.txid(), parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &child_peer, &child);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 1);
    assert_eq!(sim.lp.logic().requested_objects().count(child_peer.handle.id()), 2);
    sim.clock.advance(Duration::from_secs(3));
    sim.pass();
    let requests: Vec<_> = sim
        .sent(&child_peer)
        .into_iter()
        .filter(|(c, _)| c.as_str() == "getdata")
        .flat_map(|(_, b)| GetData::decode(&mut b.as_slice()).unwrap().inventory().to_vec())
        .collect();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|i| i.inv_type() == InvType::Dstx));
    send(&mut sim, &parent_peer, &parent);
    assert_eq!(pool.submissions().len(), 2);
    assert!(!sim.lp.logic().tx_orphans.have_more_work(parent_peer.handle.id()));
    assert!(sim.lp.logic().tx_orphans.have_more_work(child_peer.handle.id()));
    sim.pass();
    assert!(pool.exists(&child.txid()));
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    assert_eq!(pool.submissions()[2].1, Some(child_peer.handle.id()));
}

#[test]
fn missing_retry_continues_until_another_candidate_succeeds() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let parent = tx(1, &[]);
    let a = tx(2, &[parent.txid()]);
    let b = tx(3, &[parent.txid()]);
    for child in [&a, &b] {
        pool.push_result(result(TxValidationResult::MissingInputs));
        send(&mut sim, &p, child);
    }
    send(&mut sim, &p, &parent);
    pool.push_result(result(TxValidationResult::MissingInputs));
    pool.push_result(result(TxValidationResult::Unset));
    sim.pass();
    assert_eq!(sim.lp.logic().tx_orphans.size(), 1);
    assert_eq!(pool.submissions().len(), 5);
}

#[test]
fn p2p_invalid_tx_orphan_is_scored_only_on_retry_and_then_forgotten() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NO_BAN);
    let parent = tx(1, &[]);
    let child = tx(2, &[parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    assert_eq!(score(&sim, &p), 0);
    send(&mut sim, &p, &parent);
    pool.push_result(result(TxValidationResult::Consensus));
    sim.pass();
    assert_eq!(score(&sim, &p), 100);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    assert!(sim.lp.logic().recent_rejects.contains(&child.txid().to_lendian()));
}

#[test]
fn orphan_disconnect_expiry_byte_eviction_and_block_conflicts() {
    let (mut sim, pool, _, _) = node(false);
    let a = peer(&mut sim, 1, Permissions::NONE);
    let b = peer(&mut sim, 2, Permissions::NONE);
    let parent = TxHash::from_lendian([8; 32]);
    let child = tx(1, &[parent]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &a, &child);
    sim.core.disconnect(a.handle.id(), dash_p2p::Why::Gate("test close"));
    sim.remove_disconnected();
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    sim.lp.logic_mut().max_orphan_tx_bytes = 0;
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &b, &child);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    sim.lp.logic_mut().max_orphan_tx_bytes = 10_000_000;
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &b, &child);
    let conflict = tx(2, &[parent]);
    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![conflict],
    ));
    sim.shared.transactions_connected(&block);
    sim.lp.logic_mut().drain_sync_signals();
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    assert!(sim.lp.logic().recent_confirmed.contains(&block.txs()[0].txid().to_lendian()));
}

#[test]
fn finalize_erases_peer_orphans_and_reconsideration_work() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let parent = tx(1, &[]);
    let child = tx(2, &[parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    pool.push_result(result(TxValidationResult::Unset));
    send(&mut sim, &p, &parent);
    assert!(sim.lp.logic().tx_orphans.have_more_work(p.handle.id()));
    sim.core.disconnect(p.handle.id(), dash_p2p::Why::Gate("finalize test"));
    sim.remove_disconnected();
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    assert!(!sim.lp.logic().tx_orphans.have_more_work(p.handle.id()));
}

#[test]
fn rejected_parent_is_checked_even_when_it_was_not_the_missing_input() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let rejected = tx(1, &[]);
    pool.push_result(result(TxValidationResult::NotStandard));
    send(&mut sim, &p, &rejected);
    let child = tx(2, &[rejected.txid(), TxHash::from_lendian([9; 32])]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
    assert!(sim.lp.logic().recent_rejects.contains(&child.txid().to_lendian()));
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
}

#[test]
fn rejection_scores_only_consensus_and_reinv_is_not_requested() {
    for verdict in [
        TxValidationResult::Consensus,
        TxValidationResult::NotStandard,
        TxValidationResult::InputsNotStandard,
        TxValidationResult::RecentConsensusChange,
        TxValidationResult::PrematureSpend,
        TxValidationResult::Conflict,
        TxValidationResult::ConflictLock,
        TxValidationResult::MempoolPolicy,
        TxValidationResult::BadSpecial,
        TxValidationResult::NoMempool,
    ] {
        let (mut sim, pool, _, _) = node(false);
        let p = peer(&mut sim, 1, Permissions::NO_BAN);
        let t = tx(1, &[]);
        pool.push_result(result(verdict));
        send(&mut sim, &p, &t);
        assert_eq!(
            score(&sim, &p),
            if verdict == TxValidationResult::Consensus {
                100
            } else {
                0
            }
        );
        announce(&mut sim, &p, InvType::Tx, t.txid());
        assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
        send(&mut sim, &p, &t);
        assert_eq!(pool.submissions().len(), 1);
    }
}

#[test]
fn dstx_checks_precede_already_have_and_store_only_after_acceptance() {
    let (mut sim, pool, cj, hooks) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let t = tx(1, &[]);
    let wrapper = DsTx::new(t.raw().clone(), TxHash::from_lendian([1; 32]), vec![], SIM_START);
    let mut bytes = vec![];
    wrapper.encode(&mut bytes);
    cj.push_result(DstxCheck::Reject {
        score: Some(10),
    });
    sim.admitted(&p, "dstx", bytes.clone()).unwrap();
    assert!(pool.submissions().is_empty());
    assert_eq!(score(&sim, &p), 10);
    cj.push_result(DstxCheck::Accept {
        priority_delta: 10_000_000,
    });
    sim.admitted(&p, "dstx", bytes.clone()).unwrap();
    assert_eq!(cj.added().len(), 1);
    assert_eq!(hooks.priorities(), [(t.txid(), 10_000_000)]);
    cj.push_result(DstxCheck::Stop);
    sim.admitted(&p, "dstx", bytes).unwrap();
    assert_eq!(cj.checked().len(), 3);
    assert_eq!(pool.submissions().len(), 1);
    assert_eq!(score(&sim, &p), 10);
}

#[test]
fn forcerelay_relays_only_existing_mempool_transactions() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::FORCE_RELAY);
    let other = peer(&mut sim, 2, Permissions::NONE);
    let t = tx(1, &[]);
    send(&mut sim, &p, &t);
    sim.shared
        .with_state(other.handle.id(), |s| {
            s.inventory_relay().lock().tx_inventory_to_send.clear();
        })
        .unwrap();
    send(&mut sim, &p, &t);
    assert!(queued(&sim, &other, t.txid()));
    assert!(!queued(&sim, &p, t.txid()), "PushInv skips the sender's known hash");
    assert_eq!(pool.submissions().len(), 1);
    let rejected = tx(2, &[]);
    pool.push_result(result(TxValidationResult::NotStandard));
    send(&mut sim, &p, &rejected);
    send(&mut sim, &p, &rejected);
    assert!(!queued(&sim, &other, rejected.txid()));
    assert_eq!(pool.submissions().len(), 2);
}

#[test]
fn blocks_only_tx_requires_relay_permission_and_ibd_is_unread() {
    let (mut sim, pool, _, _) = node(true);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let relay = peer(&mut sim, 2, Permissions::RELAY);
    let t = tx(1, &[]);
    sim.deliver(&p, &[("tx", t.raw().core_serialize())]);
    sim.pass();
    assert!(p.handle.disconnect_requested());
    sim.deliver(&relay, &[("tx", t.raw().core_serialize())]);
    sim.pass();
    assert!(pool.exists(&t.txid()));
    sim.chain.set_ibd(true);
    sim.admitted(&relay, "tx", vec![1]).unwrap();
    assert_eq!(pool.submissions().len(), 1);
}

#[test]
fn no_coinjoin_adapter_preserves_unread_stub() {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    sim.admitted(&p, "dstx", vec![1]).unwrap();
    assert_eq!(score(&sim, &p), 0);
}

#[test]
fn no_tx_pool_preserves_unread_tx_stub_outside_ibd() {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let mut sim = Sim::with(
        params,
        id,
        ProcessorOptions::default(),
        Registry::new(),
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    sim.admitted(&p, "tx", vec![1]).unwrap();
    assert_eq!(score(&sim, &p), 0);
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
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

#[test]
fn stable_unlock_rejection_and_confirmation_are_instance_scoped() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let a = unlock(1);
    let b = unlock(2);
    assert_eq!(a.txid(), b.txid());
    assert_ne!(a.instance_hash(), b.instance_hash());
    pool.push_result(result(TxValidationResult::BadSpecial));
    send(&mut sim, &p, &a);
    assert!(sim.lp.logic().recent_rejects.contains(&a.instance_hash().to_lendian()));
    assert!(!sim.lp.logic().recent_rejects.contains(&a.txid().to_lendian()));
    send(&mut sim, &p, &b);
    assert_eq!(pool.info(&a.txid()).unwrap().tx.instance_hash(), b.instance_hash());
    send(&mut sim, &p, &a);
    assert_eq!(pool.submissions().len(), 2);
    sim.shared
        .with_state(p.handle.id(), |s| {
            let relay = s.inventory_relay().lock();
            assert!(relay.is_known(&b.instance_hash().to_lendian()));
            assert!(!relay.is_known(&b.txid().to_lendian()));
        })
        .unwrap();
}

#[test]
fn tx_response_completes_both_tx_and_dstx_requests_and_forget_clears_all_peers() {
    let (mut sim, pool, _, _) = node(false);
    let a = peer(&mut sim, 1, Permissions::NONE);
    let b = peer(&mut sim, 2, Permissions::NONE);
    let t = tx(1, &[]);
    announce(&mut sim, &a, InvType::Tx, t.txid());
    announce(&mut sim, &a, InvType::Dstx, t.txid());
    announce(&mut sim, &b, InvType::Dstx, t.txid());
    assert_eq!(sim.lp.logic().requested_objects().count(a.handle.id()), 2);
    assert_eq!(sim.lp.logic().requested_objects().count(b.handle.id()), 1);
    send(&mut sim, &a, &t);
    assert!(pool.exists(&t.txid()));
    assert_eq!(sim.lp.logic().requested_objects().count(a.handle.id()), 0);
    assert_eq!(sim.lp.logic().requested_objects().count(b.handle.id()), 0);
}

#[test]
fn block_connected_forgets_transaction_and_unlock_instance_requests() {
    let (mut sim, _, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let ordinary = tx(1, &[]);
    let unlock = unlock(7);
    announce(&mut sim, &p, InvType::Tx, ordinary.txid());
    announce(&mut sim, &p, InvType::Dstx, ordinary.txid());
    announce(&mut sim, &p, InvType::AssetUnlock, unlock.instance_hash());
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 3);
    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![ordinary, unlock],
    ));
    sim.shared.transactions_connected(&block);
    sim.lp.logic_mut().drain_sync_signals();
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
}

#[test]
fn block_connected_reconsiders_orphan_with_new_parent() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let relay_peer = peer(&mut sim, 2, Permissions::NONE);
    let parent = tx(1, &[]);
    let child = tx(2, &[parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 1);
    pool.push_result(result(TxValidationResult::Unset));
    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![parent],
    ));
    sim.shared.transactions_connected(&block);
    sim.lp.logic_mut().drain_sync_signals();
    assert!(pool.exists(&child.txid()));
    assert!(!sim.lp.logic().tx_orphans.have_tx(&child.txid()));
    assert!(!queued(&sim, &p, child.txid()), "sender already knows the transaction");
    assert!(queued(&sim, &relay_peer, child.txid()));
}

#[test]
fn block_connected_drains_all_orphans_and_a_chained_descendant() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let parent = tx(10, &[]);
    let first = tx(11, &[parent.txid()]);
    let second = tx(12, &[parent.txid()]);
    let descendant = tx(13, &[first.txid()]);

    for orphan in [&first, &second, &descendant] {
        pool.push_result(result(TxValidationResult::MissingInputs));
        send(&mut sim, &p, orphan);
    }
    assert_eq!(sim.lp.logic().tx_orphans.size(), 3);

    for _ in 0..3 {
        pool.push_result(result(TxValidationResult::Unset));
    }
    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![parent],
    ));
    sim.shared.transactions_connected(&block);
    sim.lp.logic_mut().drain_sync_signals();
    sim.pass();

    for tx in [&first, &second, &descendant] {
        assert!(pool.exists(&tx.txid()), "{} was not admitted", tx.txid());
    }
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
}

#[test]
fn block_connected_retry_preserves_later_disconnect_order() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let parent = tx(20, &[]);
    let child = tx(21, &[parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    pool.push_result(result(TxValidationResult::Unset));

    let block = Arc::new(Block::new(
        Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
        vec![parent.clone()],
    ));
    sim.shared.transactions_connected(&block);
    sim.shared.block_disconnected();
    sim.lp.logic_mut().drain_sync_signals();

    assert!(pool.exists(&child.txid()));
    assert!(
        !sim.lp.logic().recent_confirmed.contains(&parent.txid().to_lendian()),
        "disconnect must reset confirmations after the block retry"
    );
}

#[test]
fn block_connected_retry_finishes_before_the_next_queued_block() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let parent = tx(30, &[]);
    let child = tx(31, &[parent.txid()]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &child);
    pool.push_result(result(TxValidationResult::Unset));

    let block = |n: u32, txs: Vec<TxRef>| {
        Arc::new(Block::new(
            Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, n, 0, 0)),
            txs,
        ))
    };
    sim.shared.transactions_connected(&block(1, vec![parent]));
    sim.shared.transactions_connected(&block(2, vec![child.clone()]));
    sim.lp.logic_mut().drain_sync_signals();

    assert!(
        pool.exists(&child.txid()),
        "the first block's orphan retry runs before the second block erases the orphan"
    );
    assert!(sim.lp.logic().recent_confirmed.contains(&child.txid().to_lendian()));
}

#[test]
fn orphans_and_small_rejections_enter_compact_extra_transactions() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let t = tx(1, &[TxHash::from_lendian([3; 32])]);
    pool.push_result(result(TxValidationResult::MissingInputs));
    send(&mut sim, &p, &t);
    assert_eq!(sim.lp.logic_mut().download_state().unwrap().extra_txn().len(), 1);
    let rejected = tx(2, &[]);
    pool.push_result(result(TxValidationResult::NotStandard));
    send(&mut sim, &p, &rejected);
    assert_eq!(sim.lp.logic_mut().download_state().unwrap().extra_txn().len(), 2);
    let oversized = Arc::new(Tx::new(Transaction::new(
        1,
        TxType::Spend,
        vec![TxIn::new(
            OutPoint::new(TxHash::from_lendian([4; 32]), 0),
            vec![0x51; 100_000].into(),
            u32::MAX,
        )],
        vec![],
        3,
        vec![],
    )));
    pool.push_result(result(TxValidationResult::NotStandard));
    send(&mut sim, &p, &oversized);
    assert_eq!(sim.lp.logic_mut().download_state().unwrap().extra_txn().len(), 2);
}

#[test]
fn mempool_entry_is_not_a_new_acceptance() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let t = tx(1, &[]);
    let mut existing = result(TxValidationResult::Unset);
    existing.result_type = AcceptResultType::MempoolEntry;
    pool.push_result(existing);
    send(&mut sim, &p, &t);
    assert_eq!(p.handle.last_tx_time(), 0);
    assert!(!queued(&sim, &p, t.txid()));
    assert!(sim.lp.logic().recent_rejects.contains(&t.txid().to_lendian()));
    assert_eq!(score(&sim, &p), 0);
}

#[test]
fn seeded_fixture_utxos_keep_orphan_missing_until_every_parent_arrives() {
    let (mut sim, pool, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    let coin_a = TxHash::from_lendian([11; 32]);
    let coin_b = TxHash::from_lendian([12; 32]);
    pool.seed_utxo(OutPoint::new(coin_a, 0));
    pool.seed_utxo(OutPoint::new(coin_b, 0));
    let a = tx(1, &[coin_a]);
    let b = tx(2, &[coin_b]);
    let child = tx(3, &[a.txid(), b.txid()]);
    send(&mut sim, &p, &child);
    assert_eq!(sim.lp.logic().tx_orphans.size(), 1);
    send(&mut sim, &p, &a);
    sim.pass();
    assert!(!pool.exists(&child.txid()));
    assert_eq!(sim.lp.logic().tx_orphans.size(), 1);
    send(&mut sim, &p, &b);
    sim.pass();
    assert!(pool.exists(&child.txid()));
    assert_eq!(sim.lp.logic().tx_orphans.size(), 0);
}

fn no_pool_node(blocks_only: bool) -> Sim {
    let params =
        Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap());
    let id = local_identity(&params, &IdentityOptions::new()).unwrap();
    let sim = Sim::with(
        params,
        id,
        ProcessorOptions {
            blocks_only,
            ..ProcessorOptions::default()
        },
        Registry::new(),
        Seams::default(),
        |_| {},
    );
    sim.chain.set_ibd(false);
    sim
}

fn transaction_invs() -> [(InvType, TxHash); 3] {
    [
        (InvType::Tx, TxHash::from([1; 32])),
        (InvType::Dstx, TxHash::from([2; 32])),
        (InvType::AssetUnlock, TxHash::from([3; 32])),
    ]
}

/// Every inventory `p` was asked for, past the non-preferred delay and the next pass.
fn requested_after_delay(sim: &mut Sim, p: &SimPeer) -> Vec<Inventory> {
    sim.clock.advance(Duration::from_secs(3));
    sim.pass();
    sim.sent(p)
        .into_iter()
        .filter(|(c, _)| c.as_str() == "getdata")
        .flat_map(|(_, b)| GetData::decode(&mut b.as_slice()).unwrap().inventory().to_vec())
        .collect()
}

/// Relay on (`-blocksonly=0`) with no pool installed, out of IBD: announced transactions,
/// CoinJoin transactions and asset unlocks are had, so none is requested or left in flight.
#[test]
fn relay_on_without_a_pool_requests_no_transaction() {
    let mut sim = no_pool_node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    for (kind, hash) in transaction_invs() {
        announce(&mut sim, &p, kind, hash);
    }
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
    assert_eq!(requested_after_delay(&mut sim, &p), []);
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
    assert!(!p.handle.disconnect_requested());
}

/// Relay on with a pool installed: the same announcements are requested as before.
#[test]
fn relay_on_with_a_pool_requests_every_transaction() {
    let (mut sim, _, _, _) = node(false);
    let p = peer(&mut sim, 1, Permissions::NONE);
    for (kind, hash) in transaction_invs() {
        announce(&mut sim, &p, kind, hash);
    }
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 3);
    let want: Vec<_> = transaction_invs()
        .map(|(kind, hash)| Inventory::new(kind, Hash256::from_lendian(hash.to_lendian())))
        .into();
    assert_eq!(requested_after_delay(&mut sim, &p), want);
}

/// `-blocksonly` with no pool: a `relay`-permission peer may announce transactions (DEC-332),
/// and none is requested or left in flight.
#[test]
fn blocks_only_relay_peer_without_a_pool_leaves_no_transaction_request() {
    let mut sim = no_pool_node(true);
    let p = peer(&mut sim, 1, Permissions::RELAY);
    for (kind, hash) in transaction_invs() {
        announce(&mut sim, &p, kind, hash);
    }
    assert_eq!(sim.lp.logic().requested_objects().count(p.handle.id()), 0);
    assert_eq!(requested_after_delay(&mut sim, &p), []);
    assert!(!p.handle.disconnect_requested());
}
