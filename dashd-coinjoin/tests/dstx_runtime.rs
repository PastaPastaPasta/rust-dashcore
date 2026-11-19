#![cfg(feature = "testing")]

use dash_coinjoin::denom::SMALLEST_DENOMINATION;
use dash_coinjoin::dstx::{DstxChain, PoolLimits, dstx_signature_hash, txid};
use dash_coinjoin::queue::{MAX_MIXING_TXES, MixingCounters, MixingMasternode, TipMasternodes};
use dash_coinjoin::relay::TxInvKind;
use dash_p2p_core::{DsTx, ProtocolVersion};
use dash_pkc::bls::core::BlsPkValue;
use dash_pkc::bls::{BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dashd_coinjoin::dstx::DstxRuntime;
use dashd_coinjoin::env::{DstxChainAdapter, MempoolListener, MnLists};
use dashd_coinjoin::testing::{
    ChainViewCall, FakeChainView, FakeMempool, FakeMnLists, MempoolCall,
};

fn limits() -> PoolLimits {
    PoolLimits {
        min_participants: 3,
        max_participants: 20,
    }
}

fn block(height: i32, tag: u8) -> Option<(i32, BlockHash)> {
    Some((height, BlockHash::from([tag; 32])))
}

fn dstx(ins: u32, outs: u32, tag: u8) -> DsTx {
    let inputs = (0..ins)
        .map(|i| TxIn::new(OutPoint::new(TxHash::from([tag; 32]), i), Vec::new().into(), u32::MAX))
        .collect();
    let outputs = (0..outs)
        .map(|_| {
            let mut script = vec![0x76, 0xa9, 0x14];
            script.extend([tag; 20]);
            script.extend([0x88, 0xac]);
            TxOut::from_duffs(SMALLEST_DENOMINATION, script.into())
        })
        .collect();
    DsTx::new(
        Transaction::new(2, TxType::Spend, inputs, outputs, 0, Vec::new()),
        TxHash::from([1; 32]),
        Vec::new(),
        1_760_000_000,
    )
}

fn signed_dstx(ins: u32, outs: u32, tag: u8) -> (DsTx, MixingMasternode) {
    let key = BlsSecretKey::<BlsScIetf>::from_ikm(&[1; 32]).unwrap();
    let d = dstx(ins, outs, tag);
    let signature = key.sign_with(dstx_signature_hash(&d).as_bytes(), BlsSigId::Basic);
    let mn = MixingMasternode {
        pro_tx_hash: d.pro_tx_hash(),
        collateral: OutPoint::new(TxHash::from([2; 32]), 1),
        operator_key: BlsPkValue::from_canonical(
            &key.public_key().to_bytes(),
            BlsSchemeKind::Basic,
        )
        .unwrap(),
    };
    (DsTx::new(d.tx().clone(), d.pro_tx_hash(), signature.to_bytes().to_vec(), d.sig_time()), mn)
}

struct Intake {
    runtime: DstxRuntime,
    chain: FakeChainView,
    mns: FakeMnLists,
    counters: MixingCounters,
    pool: FakeMempool,
}

impl Intake {
    fn new(mn: MixingMasternode) -> Self {
        let mut mns = FakeMnLists::default();
        mns.at_depth.insert((0, mn.pro_tx_hash), mn);
        Self {
            runtime: DstxRuntime::new(limits()),
            chain: FakeChainView {
                tip: block(100, 1),
                ..Default::default()
            },
            mns,
            counters: MixingCounters::default(),
            pool: FakeMempool::default(),
        }
    }

    fn validate(&mut self, d: &DsTx) -> Result<(), i32> {
        self.runtime.validate(
            d,
            &txid(d.tx()),
            &mut self.counters,
            &DstxChainAdapter {
                chain: &self.chain,
                mns: &self.mns,
            },
            &mut self.pool,
        )
    }
}

struct ReentrantChain<'a> {
    inner: DstxChainAdapter<'a, FakeChainView, ReentrantMnLists<'a>>,
    runtime: Option<&'a DstxRuntime>,
    known_txid: TxHash,
}

impl ReentrantChain<'_> {
    fn lookup(&self) {
        if let Some(runtime) = self.runtime {
            assert!(runtime.contains(&self.known_txid));
        }
    }
}

impl DstxChain for ReentrantChain<'_> {
    fn tip_height(&self) -> Option<i32> {
        self.lookup();
        self.inner.tip_height()
    }

    fn v24_active_at_tip(&self) -> bool {
        self.lookup();
        self.inner.v24_active_at_tip()
    }

    fn v24_active_after_tip(&self) -> bool {
        self.lookup();
        self.inner.v24_active_after_tip()
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.lookup();
        self.inner.mn_at_depth(depth, pro_tx_hash)
    }
}

struct ReentrantMnLists<'a> {
    inner: &'a FakeMnLists,
    runtime: Option<&'a DstxRuntime>,
    known_txid: TxHash,
}

impl TipMasternodes for ReentrantMnLists<'_> {
    fn valid_mn(&self, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.inner.valid_mn(pro_tx_hash)
    }

    fn enabled_count(&self) -> i32 {
        self.inner.enabled_count()
    }
}

impl MnLists for ReentrantMnLists<'_> {
    fn valid_by_collateral(&self, outpoint: &OutPoint) -> Option<MixingMasternode> {
        self.inner.valid_by_collateral(outpoint)
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        if let Some(runtime) = self.runtime {
            let stored = runtime.get(&self.known_txid).unwrap();
            assert_eq!(txid(stored.dstx.tx()), self.known_txid);
        }
        self.inner.mn_at_depth(depth, pro_tx_hash)
    }
}

fn assert_reentrant_validation(chain_callbacks: bool) {
    for (ins, outs, v24_at_tip, v24_after_tip, expected) in
        [(3, 3, false, false, Ok(())), (19, 10, false, true, Err(1)), (19, 10, true, true, Ok(()))]
    {
        let (d, mn) = signed_dstx(ins, outs, 1);
        let mut plain = Intake::new(mn.clone());
        plain.chain.v24_at_tip = v24_at_tip;
        plain.chain.v24_after_tip = v24_after_tip;
        let plain_result = plain.validate(&d);
        assert_eq!(plain_result, expected);

        let mut intake = Intake::new(mn);
        intake.chain.v24_at_tip = v24_at_tip;
        intake.chain.v24_after_tip = v24_after_tip;
        let known = dstx(3, 3, 2);
        let known_txid = txid(known.tx());
        intake.runtime.on_accepted(known);
        let mns = ReentrantMnLists {
            inner: &intake.mns,
            runtime: (!chain_callbacks).then_some(&intake.runtime),
            known_txid,
        };
        let chain = ReentrantChain {
            inner: DstxChainAdapter {
                chain: &intake.chain,
                mns: &mns,
            },
            runtime: chain_callbacks.then_some(&intake.runtime),
            known_txid,
        };
        let result = intake.runtime.validate(
            &d,
            &txid(d.tx()),
            &mut intake.counters,
            &chain,
            &mut intake.pool,
        );
        assert_eq!(result, plain_result);
        assert_eq!(intake.counters, plain.counters);
        assert_eq!(*intake.pool.calls.lock().unwrap(), *plain.pool.calls.lock().unwrap());
        assert_eq!(*intake.chain.calls.lock().unwrap(), *plain.chain.calls.lock().unwrap());
        assert_eq!(*intake.mns.calls.lock().unwrap(), *plain.mns.calls.lock().unwrap());
    }
}

#[test]
fn validation_allows_chain_callbacks_into_runtime() {
    assert_reentrant_validation(true);
}

#[test]
fn validation_allows_mn_list_callbacks_into_runtime() {
    assert_reentrant_validation(false);
}

fn confirmed() -> (DstxRuntime, DsTx) {
    let runtime = DstxRuntime::new(limits());
    let d = dstx(3, 3, 1);
    runtime.on_accepted(d.clone());
    runtime.block_connected(&[d.tx().clone()], 100);
    (runtime, d)
}

#[test]
fn updated_tip_gates_skip_before_expiry_and_host_queries() {
    // Core v24.0.0 src/dsnotificationinterface.cpp:57-66: same tip, IBD, then sync gate.
    for (new, fork, ibd, synced, calls) in [
        (block(125, 2), block(125, 2), false, true, vec![]),
        (None, None, false, true, vec![]),
        (block(125, 2), block(99, 3), true, true, vec![]),
        (block(125, 2), block(99, 3), false, false, vec![ChainViewCall::BlockchainSynced]),
    ] {
        let (runtime, d) = confirmed();
        let chain = FakeChainView {
            synced,
            ..Default::default()
        };
        runtime.updated_block_tip(new, fork, ibd, &chain);
        assert!(runtime.contains(&txid(d.tx())));
        assert_eq!(*chain.calls.lock().unwrap(), calls);
    }
}

#[test]
fn updated_tip_without_new_index_skips_check() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:599-604: a null index never calls CheckDSTXes.
    let (runtime, d) = confirmed();
    let chain = FakeChainView {
        synced: true,
        ..Default::default()
    };
    runtime.updated_block_tip(None, block(99, 3), false, &chain);
    assert!(runtime.contains(&txid(d.tx())));
    assert_eq!(*chain.calls.lock().unwrap(), [ChainViewCall::BlockchainSynced]);
}

#[test]
fn updated_tip_compares_block_identity_and_uses_notified_tip() {
    // Core v24.0.0 src/dsnotificationinterface.cpp:57,65-66; coinjoin/coinjoin.cpp:97,599-603.
    let (runtime, d) = confirmed();
    let mut chain = FakeChainView {
        synced: true,
        tip: block(150, 9),
        ..Default::default()
    };
    chain.chain_locks.insert((100, BlockHash::from([2; 32])));
    runtime.updated_block_tip(block(100, 2), block(100, 3), false, &chain);
    assert!(!runtime.contains(&txid(d.tx())));
    assert_eq!(
        *chain.calls.lock().unwrap(),
        [
            ChainViewCall::BlockchainSynced,
            ChainViewCall::HasChainLock(100, BlockHash::from([2; 32]))
        ]
    );
}

#[test]
fn chainlock_notification_requires_sync_and_non_null_index() {
    // Core v24.0.0 src/dsnotificationinterface.cpp:101-103; coinjoin/coinjoin.cpp:608-609.
    for (synced, index) in [(false, block(125, 2)), (true, None)] {
        let (runtime, d) = confirmed();
        let chain = FakeChainView {
            synced,
            ..Default::default()
        };
        runtime.notify_chain_lock(index, &chain);
        assert!(runtime.contains(&txid(d.tx())));
        assert_eq!(*chain.calls.lock().unwrap(), [ChainViewCall::BlockchainSynced]);
    }
}

#[test]
fn both_expiry_notifications_keep_depth_24_and_drop_depth_25() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:95-97,599-611: depth must be strictly >24.
    for chainlock_notification in [false, true] {
        let (runtime, d) = confirmed();
        let chain = FakeChainView {
            synced: true,
            ..Default::default()
        };
        for (height, kept) in [(124, true), (125, false)] {
            if chainlock_notification {
                runtime.notify_chain_lock(block(height, 2), &chain);
            } else {
                runtime.updated_block_tip(block(height, 2), block(99, 3), false, &chain);
            }
            assert_eq!(runtime.contains(&txid(d.tx())), kept);
        }
    }
}

#[test]
fn chainlock_expiry_queries_the_exact_notified_height_and_hash() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:97: HasChainLock uses the supplied index's pair.
    for chainlock_notification in [false, true] {
        for (locked_height, locked_hash, kept) in [(99, 2, true), (100, 3, true), (100, 2, false)] {
            let (runtime, d) = confirmed();
            let mut chain = FakeChainView {
                synced: true,
                ..Default::default()
            };
            chain.chain_locks.insert((locked_height, BlockHash::from([locked_hash; 32])));
            if chainlock_notification {
                runtime.notify_chain_lock(block(100, 2), &chain);
            } else {
                runtime.updated_block_tip(block(100, 2), block(99, 3), false, &chain);
            }
            assert_eq!(runtime.contains(&txid(d.tx())), kept);
            assert_eq!(
                *chain.calls.lock().unwrap(),
                [
                    ChainViewCall::BlockchainSynced,
                    ChainViewCall::HasChainLock(100, BlockHash::from([2; 32]))
                ]
            );
        }
    }
}

#[test]
fn unconfirmed_and_above_tip_confirmations_survive_chainlocks() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:95: no height, or height above the index, survives.
    for height in [None, Some(101)] {
        let (runtime, d) = confirmed();
        if let Some(height) = height {
            runtime.block_connected(&[d.tx().clone()], height);
        } else {
            let listener: &dyn MempoolListener = &runtime;
            listener.transaction_added_to_mempool(d.tx());
        }
        let mut chain = FakeChainView {
            synced: true,
            ..Default::default()
        };
        chain.chain_locks.insert((100, BlockHash::from([2; 32])));
        runtime.notify_chain_lock(block(100, 2), &chain);
        assert!(runtime.contains(&txid(d.tx())));
    }
}

#[test]
fn block_notifications_update_every_tracked_transaction_idempotently() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:617-622,638-640,647-649: update known txs only.
    let runtime = DstxRuntime::new(limits());
    let first = dstx(3, 3, 1);
    let second = dstx(3, 3, 2);
    let unknown = dstx(3, 3, 3);
    let transactions = [first.tx().clone(), unknown.tx().clone(), second.tx().clone()];
    runtime.on_accepted(first.clone());
    runtime.on_accepted(second.clone());
    for _ in 0..2 {
        runtime.block_connected(&transactions, 101);
        for d in [&first, &second] {
            assert_eq!(runtime.get(&txid(d.tx())).unwrap().confirmed_height, Some(101));
        }
        assert!(!runtime.contains(&txid(unknown.tx())));
    }
    for _ in 0..2 {
        runtime.block_disconnected(&transactions);
        for d in [&first, &second] {
            assert_eq!(runtime.get(&txid(d.tx())).unwrap().confirmed_height, None);
        }
        assert!(!runtime.contains(&txid(unknown.tx())));
    }
}

#[test]
fn mempool_notification_clears_only_the_matching_confirmation() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:617-622,626-631: None for the matching tx only.
    let (runtime, first) = confirmed();
    let second = dstx(3, 3, 2);
    let unknown = dstx(3, 3, 3);
    runtime.on_accepted(second.clone());
    runtime.block_connected(&[second.tx().clone()], 102);
    let listener: &dyn MempoolListener = &runtime;
    for _ in 0..2 {
        listener.transaction_added_to_mempool(first.tx());
        listener.transaction_added_to_mempool(unknown.tx());
        assert_eq!(runtime.get(&txid(first.tx())).unwrap().confirmed_height, None);
        assert_eq!(runtime.get(&txid(second.tx())).unwrap().confirmed_height, Some(102));
        assert!(!runtime.contains(&txid(unknown.tx())));
    }
}

#[test]
fn reorg_expiry_starts_at_the_replacement_branch_confirmation() {
    // Core v24.0.0 src/net_processing.cpp:4964-4967; coinjoin/coinjoin.cpp:95-97,622-649.
    let (d, mn) = signed_dstx(3, 3, 1);
    let mut intake = Intake::new(mn);
    assert_eq!(intake.validate(&d), Ok(()));
    intake.runtime.on_accepted(d.clone());
    let transactions = [d.tx().clone()];
    for _ in 0..2 {
        intake.runtime.block_connected(&transactions, 100);
        assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().confirmed_height, Some(100));
    }
    for _ in 0..2 {
        intake.runtime.block_disconnected(&transactions);
        assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().confirmed_height, None);
    }
    let listener: &dyn MempoolListener = &intake.runtime;
    listener.transaction_added_to_mempool(d.tx());
    assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().confirmed_height, None);
    intake.chain.synced = true;
    intake.runtime.updated_block_tip(block(200, 2), block(99, 3), false, &intake.chain);
    assert!(intake.runtime.contains(&txid(d.tx())));
    for _ in 0..2 {
        intake.runtime.block_connected(&transactions, 210);
        assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().confirmed_height, Some(210));
    }
    for (height, kept) in [(225, true), (234, true), (235, false)] {
        intake.runtime.updated_block_tip(block(height, 2), block(99, 3), false, &intake.chain);
        assert_eq!(intake.runtime.contains(&txid(d.tx())), kept);
    }
}

#[test]
fn validation_prioritises_before_admission_and_does_not_store() {
    // Core v24.0.0 src/net_processing.cpp:3716-3719,4962-4967: +0.1 COIN, store on acceptance.
    let (d, mn) = signed_dstx(3, 3, 1);
    let mut intake = Intake::new(mn);
    assert_eq!(intake.validate(&d), Ok(()));
    assert_eq!(
        *intake.pool.calls.lock().unwrap(),
        [MempoolCall::Prioritise(txid(d.tx()), 10_000_000)]
    );
    assert_eq!(intake.counters.masternodes[&d.pro_tx_hash()].mixing_tx_count, 1);
    assert!(!intake.runtime.contains(&txid(d.tx())));
    intake.runtime.on_accepted(d.clone());
    assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().dstx, d);
    assert_eq!(intake.runtime.get(&txid(d.tx())).unwrap().confirmed_height, None);
}

#[test]
fn validation_maps_invalid_unknown_and_premature_scores_without_priority() {
    // Core v24.0.0 src/net_processing.cpp:3604-3609,3660-3671,3695-3700,3710-3712.
    for (ins, outs, next_active, missing_mn, bad_signature, score) in [
        (3, 2, false, false, false, 10),
        (19, 10, false, false, false, 10),
        (19, 10, true, false, false, 1),
        (3, 3, false, true, false, 1),
        (3, 3, false, false, true, 10),
    ] {
        let (mut d, mn) = signed_dstx(ins, outs, 1);
        let mut intake = Intake::new(mn);
        intake.chain.v24_after_tip = next_active;
        if missing_mn {
            intake.mns.at_depth.clear();
        }
        if bad_signature {
            d = DsTx::new(d.tx().clone(), d.pro_tx_hash(), vec![0; 96], d.sig_time());
        }
        assert_eq!(intake.validate(&d), Err(score));
        assert!(intake.pool.calls.lock().unwrap().is_empty());
        assert!(intake.counters.masternodes.is_empty());
        assert!(!intake.runtime.contains(&txid(d.tx())));
    }
}

#[test]
fn validation_stops_known_bad_wrappers_before_chain_or_counter_checks() {
    // Core v24.0.0 src/net_processing.cpp:3647-3650: known tx stops with zero, before structure.
    let (d, mn) = signed_dstx(3, 3, 1);
    let mut intake = Intake::new(mn);
    intake.runtime.on_accepted(d.clone());
    let bad = DsTx::new(d.tx().clone(), TxHash::default(), Vec::new(), d.sig_time());
    assert_eq!(intake.validate(&bad), Err(0));
    assert!(intake.chain.calls.lock().unwrap().is_empty());
    assert!(intake.mns.calls.lock().unwrap().is_empty());
    assert!(intake.counters.masternodes.is_empty());
    assert!(intake.pool.calls.lock().unwrap().is_empty());
}

#[test]
fn validation_stops_a_masternode_over_the_mixing_limit_without_priority() {
    // Core v24.0.0 src/net_processing.cpp:3703-3705; masternode/meta.cpp:118-128.
    let (d, mn) = signed_dstx(3, 3, 1);
    let mut intake = Intake::new(mn);
    for _ in 0..=MAX_MIXING_TXES {
        intake.counters.disallow_mixing(&d.pro_tx_hash());
    }
    let counters_before = intake.counters.clone();
    assert_eq!(intake.validate(&d), Err(0));
    assert!(intake.pool.calls.lock().unwrap().is_empty());
    assert_eq!(intake.counters, counters_before);
}

#[test]
fn duplicate_acceptance_preserves_the_first_wrapper_and_confirmation() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:568: map.insert preserves an existing value.
    let (runtime, first) = confirmed();
    let replacement = DsTx::new(first.tx().clone(), first.pro_tx_hash(), vec![1; 96], 99);
    let mut snapshot = runtime.get(&txid(first.tx())).unwrap();
    runtime.on_accepted(replacement);
    assert_eq!(runtime.get(&txid(first.tx())).unwrap().dstx, first);
    assert_eq!(runtime.get(&txid(first.tx())).unwrap().confirmed_height, Some(100));
    snapshot.confirmed_height = None;
    assert_eq!(runtime.get(&txid(first.tx())).unwrap().confirmed_height, Some(100));
}

#[test]
fn announcement_kind_uses_storage_and_the_peer_rebalance_version() {
    // Core v24.0.0 src/net_processing.cpp:3617-3622,6653-6655,6720-6722.
    let runtime = DstxRuntime::new(limits());
    let old_peer = ProtocolVersion::new(ProtocolVersion::COINJOIN_REBALANCE.get() - 1);
    for (ins, outs, old_kind) in
        [(3, 3, TxInvKind::Dstx), (19, 10, TxInvKind::Tx), (181, 181, TxInvKind::Tx)]
    {
        let d = dstx(ins, outs, 1);
        let hash = txid(d.tx());
        assert_eq!(runtime.announce_kind(&hash, old_peer), TxInvKind::Tx);
        assert!(runtime.get(&hash).is_none());
        runtime.on_accepted(d);
        assert_eq!(runtime.announce_kind(&hash, old_peer), old_kind);
        assert_eq!(
            runtime.announce_kind(&hash, ProtocolVersion::COINJOIN_REBALANCE),
            TxInvKind::Dstx
        );
    }
}

#[test]
fn validation_continues_a_rebalance_once_v24_is_active_at_the_tip() {
    // Core v24.0.0 src/coinjoin/coinjoin.cpp:144-154; net_processing.cpp:3660-3671,3716-3719.
    let (d, mn) = signed_dstx(19, 10, 1);
    let mut intake = Intake::new(mn);
    intake.chain.v24_after_tip = true;
    assert_eq!(intake.validate(&d), Err(1));
    assert!(intake.pool.calls.lock().unwrap().is_empty());
    intake.chain.v24_at_tip = true;
    assert_eq!(intake.validate(&d), Ok(()));
    assert_eq!(intake.counters.masternodes[&d.pro_tx_hash()].mixing_tx_count, 1);
    assert_eq!(
        *intake.pool.calls.lock().unwrap(),
        [MempoolCall::Prioritise(txid(d.tx()), 10_000_000)]
    );
}
