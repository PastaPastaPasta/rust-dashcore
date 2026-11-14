use dash_coinjoin::dstx::DstxChain;
use dash_p2p::CommandString;
use dash_p2p_core::InvType;
use dash_primitives::{TxOut, TxType};

use super::*;
use crate::env::DstxChainAdapter;

fn tx() -> Transaction {
    Transaction::new(2, TxType::Spend, Vec::new(), Vec::new(), 0, Vec::new())
}

#[test]
fn active_mn_records_identity_and_signing() {
    let mut mn = FakeActiveMn {
        outpoint: OutPoint::new(TxHash::from([1; 32]), 7),
        pro_tx_hash: TxHash::from([2; 32]),
        signature: vec![3; 96],
        ..Default::default()
    };
    let hash = Hash256::from([4; 32]);
    assert_eq!(mn.outpoint(), mn.outpoint);
    assert_eq!(mn.pro_tx_hash(), mn.pro_tx_hash);
    assert_eq!(mn.sign_basic(&hash), mn.signature);
    assert_eq!(
        *mn.calls.lock().unwrap(),
        [ActiveMnCall::Outpoint, ActiveMnCall::ProTxHash, ActiveMnCall::SignBasic(hash)]
    );
}

#[test]
fn mn_lists_records_tip_collateral_and_history_queries() {
    let mns = FakeMnLists {
        enabled: 42,
        ..Default::default()
    };
    let hash = TxHash::from([5; 32]);
    let outpoint = OutPoint::new(hash, 3);
    assert!(mns.valid_mn(&hash).is_none());
    assert_eq!(mns.enabled_count(), 42);
    assert!(mns.valid_by_collateral(&outpoint).is_none());
    assert!(mns.mn_at_depth(23, &hash).is_none());
    assert_eq!(
        *mns.calls.lock().unwrap(),
        [
            MnListsCall::ValidMn(hash),
            MnListsCall::EnabledCount,
            MnListsCall::ValidByCollateral(outpoint),
            MnListsCall::MnAtDepth(23, hash),
        ]
    );
}

#[test]
fn islocks_records_both_locked_and_unlocked_queries() {
    let mut locks = FakeIsLocks::default();
    let locked = TxHash::from([6; 32]);
    let unlocked = TxHash::from([7; 32]);
    locks.locked.insert(locked);
    assert!(locks.is_locked(&locked));
    assert!(!locks.is_locked(&unlocked));
    assert_eq!(*locks.calls.lock().unwrap(), [locked, unlocked]);
}

#[test]
fn mempool_records_scripted_admission_coins_and_prioritisation() {
    let mut pool = FakeMempool::default();
    let tx = tx();
    let hash = txid(&tx);
    let outpoint = OutPoint::new(hash, 0);
    let coin = ViewCoin {
        out: TxOut::from_duffs(123, Vec::new().into()),
        in_mempool: true,
    };
    assert_eq!(pool.test_accept(&tx), None);
    assert!(!pool.submit(&tx));
    assert_eq!(pool.coin(&outpoint), None);
    pool.accept_fees.insert(hash, 10);
    pool.submit_results.insert(hash, true);
    pool.coins.insert(outpoint, coin.clone());
    pool.locks.locked.insert(hash);
    assert_eq!(pool.test_accept(&tx), Some(10));
    pool.prioritise(&hash, -20);
    assert!(pool.submit(&tx));
    assert_eq!(pool.coin(&outpoint), Some(coin));
    assert!(pool.is_locked(&hash));
    assert_eq!(
        *pool.calls.lock().unwrap(),
        [
            MempoolCall::TestAccept(tx.clone()),
            MempoolCall::Submit(tx.clone()),
            MempoolCall::Coin(outpoint),
            MempoolCall::TestAccept(tx.clone()),
            MempoolCall::Prioritise(hash, -20),
            MempoolCall::Submit(tx),
            MempoolCall::Coin(outpoint),
            MempoolCall::IsInstantSendLocked(hash),
        ]
    );
    assert_eq!(*pool.locks.calls.lock().unwrap(), [hash]);
}

#[test]
fn mempool_listener_records_transaction_added() {
    let listener = FakeMempoolListener::default();
    let tx = tx();
    listener.transaction_added_to_mempool(&tx);
    assert_eq!(*listener.transactions.lock().unwrap(), [tx]);
}

#[test]
fn chain_view_records_tip_deployments_chainlocks_and_sync() {
    let hash = BlockHash::from([8; 32]);
    let mut chain = FakeChainView {
        tip: Some((100, hash)),
        v24_at_tip: false,
        v24_after_tip: true,
        synced: true,
        ..Default::default()
    };
    chain.chain_locks.insert((100, hash));
    assert_eq!(chain.tip(), Some((100, hash)));
    assert!(!chain.v24_active_at_tip());
    assert!(chain.v24_active_after_tip());
    assert!(chain.has_chain_lock(100, &hash));
    assert!(!chain.has_chain_lock(99, &hash));
    assert!(chain.blockchain_synced());
    assert_eq!(
        *chain.calls.lock().unwrap(),
        [
            ChainViewCall::Tip,
            ChainViewCall::V24ActiveAtTip,
            ChainViewCall::V24ActiveAfterTip,
            ChainViewCall::HasChainLock(100, hash),
            ChainViewCall::HasChainLock(99, hash),
            ChainViewCall::BlockchainSynced,
        ]
    );
}

#[test]
fn peer_sink_records_pushes_scores_requests_and_relays_in_order() {
    let mut peers = FakePeerSink::default();
    let addr = Service::default();
    let hash = TxHash::from([9; 32]);
    let inv = Inventory::new(InvType::new(1), Hash256::from([9; 32]));
    let msg = OutMsg::new(CommandString::from_static("dssu"), vec![1, 2, 3]);
    let dsq = DsQueue::new(1, hash, 123, false, Vec::new());
    peers.push_to(42, msg.clone());
    assert!(!peers.is_connected(&addr));
    assert!(!peers.push_to_addr(&addr, msg.clone()));
    peers.connected.insert(addr.clone());
    assert!(peers.is_connected(&addr));
    assert!(peers.push_to_addr(&addr, msg.clone()));
    peers.misbehaving(42, 10, "bad signature");
    peers.erase_object_request(42, inv);
    peers.forget_object_request(inv);
    peers.relay_inv(inv);
    peers.relay_tx_inv(inv);
    peers.relay_dsq(&dsq);
    peers.relay_transaction(&hash);
    assert_eq!(*peers.queries.lock().unwrap(), [addr.clone(), addr.clone()]);
    assert_eq!(
        peers.calls,
        [
            PeerCall::PushTo(42, msg.clone()),
            PeerCall::PushToAddr(addr.clone(), msg.clone()),
            PeerCall::PushToAddr(addr, msg),
            PeerCall::Misbehaving(42, 10, "bad signature".to_owned()),
            PeerCall::EraseObjectRequest(42, inv),
            PeerCall::ForgetObjectRequest(inv),
            PeerCall::RelayInv(inv),
            PeerCall::RelayTxInv(inv),
            PeerCall::RelayDsq(dsq),
            PeerCall::RelayTransaction(hash),
        ]
    );
}

#[test]
fn dstx_chain_adapter_forwards_to_chain_and_mn_list_seams() {
    let mut chain = FakeChainView::default();
    let mns = FakeMnLists::default();
    let hash = TxHash::from([10; 32]);
    {
        let adapter = DstxChainAdapter {
            chain: &chain,
            mns: &mns,
        };
        assert_eq!(adapter.tip_height(), None);
    }
    chain.tip = Some((123, BlockHash::from([11; 32])));
    chain.v24_at_tip = true;
    let adapter = DstxChainAdapter {
        chain: &chain,
        mns: &mns,
    };
    assert_eq!(adapter.tip_height(), Some(123));
    assert!(adapter.v24_active_at_tip());
    assert!(!adapter.v24_active_after_tip());
    assert!(adapter.mn_at_depth(12, &hash).is_none());
    assert_eq!(
        *chain.calls.lock().unwrap(),
        [
            ChainViewCall::Tip,
            ChainViewCall::Tip,
            ChainViewCall::V24ActiveAtTip,
            ChainViewCall::V24ActiveAfterTip,
        ]
    );
    assert_eq!(*mns.calls.lock().unwrap(), [MnListsCall::MnAtDepth(12, hash)]);
}
