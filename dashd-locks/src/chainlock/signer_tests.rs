//! [`ClSigner`] on doubles: the tip signing gates, the safe-transaction walk, the request, the
//! block txids, the recovered-signature listener, the cleanup and the schedule.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use dash_clock::MockClock;
use dash_consensus::Tx;
use dash_num::Hash256;
use dash_p2p_core::InvType;
use dash_params::{ChainParamsBuilder, ChainSetup, Network, Release};
use dash_primitives::QuorumHash;
use dash_types::Numeric;
use dashd_chainstate::chain::NoWaitGuard;

use super::*;
use crate::chainlock::fakes::{FakeChain, FakeCl, FakeScheduler, coinbase, no_inputs, spend};
use crate::chainlock::first_seen::WAIT_FOR_ISLOCK_TIMEOUT;
use crate::signing_fake::{FakeIsLockView, FakeSignShares};

const T0: i64 = 1_700_000_000;

fn regtest() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("regtest")
}

/// Records each ChainLock and whether the signer's state lock was free during the call.
#[derive(Default)]
struct FakeProcess {
    calls: Mutex<Vec<(ClSig, bool)>>,
    signer: OnceLock<Weak<ClSigner>>,
    inv: Mutex<Option<Inventory>>,
}

impl ClProcess for FakeProcess {
    fn process_local(&self, clsig: ClSig) -> Option<Inventory> {
        let signer = self.signer.get().and_then(Weak::upgrade).expect("the signer");
        let free = signer.state.try_lock().is_some();
        self.calls.lock().push((clsig, free));
        *self.inv.lock()
    }
}

struct Rig {
    chain: Arc<FakeChain>,
    cl: Arc<FakeCl>,
    is: Arc<FakeIsLockView>,
    signing: Arc<FakeSignShares>,
    process: Arc<FakeProcess>,
    first_seen: Arc<TxFirstSeen>,
    synced: Arc<AtomicBool>,
    clock: Arc<MockClock>,
    scheduler: Arc<FakeScheduler>,
    aborts: Arc<Mutex<Vec<String>>>,
    signer: Arc<ClSigner>,
}

/// A synced signer with SPORK_19 and InstantSend on, over a genesis and one empty block.
fn rig() -> Rig {
    rig_with(None)
}

/// [`rig`], with the signer reading `is` instead of the rig's [`FakeIsLockView`].
fn rig_with(is_override: Option<Arc<dyn IsLockView>>) -> Rig {
    let chain = Arc::new(FakeChain::default());
    chain.mine(vec![], 0);
    chain.mine(vec![], 0);
    let cl = FakeCl::new(Arc::clone(&chain));
    let is = FakeIsLockView::new();
    let signing = FakeSignShares::new();
    let process = Arc::new(FakeProcess::default());
    let clock = Arc::new(MockClock::at(T0));
    let synced = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&synced);
    let synced_fn: Synced = Arc::new(move || flag.load(Ordering::Relaxed));
    let scheduler = Arc::new(FakeScheduler::default());
    let aborts = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&aborts);
    let abort: Abort = Arc::new(move |e: &str| sink.lock().push(e.to_owned()));
    let first_seen =
        Arc::new(TxFirstSeen::new(clock.clone(), Arc::clone(&synced_fn), Arc::clone(&abort)));
    let signer = ClSigner::new(
        &regtest(),
        ClSignerParts {
            chain: chain.clone(),
            cl: cl.clone(),
            is: is_override.unwrap_or_else(|| is.clone()),
            signing: signing.clone(),
            process: process.clone(),
            first_seen: Arc::clone(&first_seen),
            synced: synced_fn,
            clock: clock.clone(),
            scheduler: scheduler.clone(),
            abort,
        },
    );
    assert!(process.signer.set(Arc::downgrade(&signer)).is_ok());
    Rig {
        chain,
        cl,
        is,
        signing,
        process,
        first_seen,
        synced,
        clock,
        scheduler,
        aborts,
        signer,
    }
}

impl Rig {
    /// Mines a block on the tip and connects it to the signer and the first-seen map.
    fn connect(&self, txs: Vec<Tx>) -> Arc<Block> {
        let block = self.chain.mine(txs, 0);
        assert!(matches!(self.signer.block_connected(&block, BlockId(0)), Cont::Done));
        assert!(matches!(self.first_seen.block_connected(&block, BlockId(0)), Cont::Done));
        block
    }

    fn connect_empty(&self, n: usize) {
        for _ in 0..n {
            self.connect(vec![]);
        }
    }

    fn requests(&self) -> usize {
        self.signing.sign_requests().len()
    }

    fn tip(&self) -> ChainEntry {
        self.chain.tip().expect("a tip")
    }

    fn has_entry(&self, hash: &BlockHash) -> bool {
        self.signer.state.lock().block_txs.contains_key(hash)
    }

    fn recsig(&self, id: [u8; 32], msg_hash: [u8; 32]) -> RecoveredSig {
        RecoveredSig {
            llmq_type: regtest().consensus.llmq_type_chain_locks,
            quorum_hash: QuorumHash::from([7; 32]),
            id,
            msg_hash,
            sig: [5; 96],
        }
    }

    /// Signs the tip and returns its recovered signature.
    fn signed_tip(&self) -> RecoveredSig {
        self.signer.try_sign_chain_tip();
        let req = self.signing.sign_requests().pop().expect("a sign request");
        self.recsig(req.id, req.msg_hash)
    }
}

// Core: v24.0.2 src/chainlock/signing.cpp:83-85, :190-192, :296-298
#[test]
fn unsynced_signs_records_and_cleans_nothing() {
    let r = rig();
    r.synced.store(false, Ordering::Relaxed);
    let block = r.connect(vec![spend(1)]);
    r.signer.try_sign_chain_tip();
    let unknown = BlockHash::from([9; 32]);
    r.signer.state.lock().block_txs.insert(unknown, Arc::default());
    r.signer.cleanup();
    assert_eq!(r.requests(), 0);
    assert!(!r.has_entry(&block.hash()));
    assert!(r.has_entry(&unknown));
}

// Core: v24.0.2 src/chainlock/signing.cpp:180-185 BlockDisconnected
#[test]
fn a_disconnect_erases_the_entry_even_while_unsynced() {
    let r = rig();
    let block = r.connect(vec![spend(1)]);
    r.synced.store(false, Ordering::Relaxed);
    r.signer.block_disconnected(&block, BlockId(0));
    assert!(!r.has_entry(&block.hash()));
}

// Core: v24.0.2 src/chainlock/signing.cpp:87-89 TrySignChainTip; feature_llmq_chainlocks.py:99-108
#[test]
fn spork19_with_a_nonzero_value_does_not_sign() {
    let r = rig();
    *r.cl.signing_enabled.lock() = false;
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    *r.cl.signing_enabled.lock() = true;
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:87-89 TrySignChainTip
#[test]
fn spork19_inactive_does_not_sign() {
    let r = rig();
    *r.cl.enabled.lock() = false;
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:93-95 TrySignChainTip
#[test]
fn a_tip_without_a_parent_is_not_signed() {
    let r = rig();
    let genesis = r.chain.mine_on(None, vec![], 0);
    r.chain.set_tip(genesis.hash());
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:105-108 TrySignChainTip
#[test]
fn the_same_height_is_signed_once() {
    let r = rig();
    r.signer.try_sign_chain_tip();
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:111-114 TrySignChainTip
#[test]
fn a_chainlock_at_the_tip_height_stops_signing() {
    let r = rig();
    r.cl.lock_at(&r.tip().hash);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

/// Core reads the best height and the best block under separate locks; the fake's height below
/// the conflicting block's models a lock arriving between them.
// Core: v24.0.2 src/chainlock/signing.cpp:116-120 TrySignChainTip
#[test]
fn a_conflicting_chainlock_stops_signing() {
    let r = rig();
    let parent = r.tip().prev.expect("a parent");
    let tip = r.tip().hash;
    let other = r.chain.mine_on(Some(parent), vec![], 1).hash();
    r.chain.set_tip(tip);
    r.cl.lock_at(&other);
    r.cl.heights.lock().push_back(0);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:150-157 TrySignChainTip; handler.cpp:40, :262-274
#[test]
fn an_unlocked_transaction_is_signed_over_from_ten_minutes() {
    let r = rig();
    r.connect(vec![spend(1)]);
    r.connect_empty(3);
    r.clock.set_mocktime(T0 + WAIT_FOR_ISLOCK_TIMEOUT - 1);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    r.clock.set_mocktime(T0 + WAIT_FOR_ISLOCK_TIMEOUT);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/handler.cpp:264-273 IsTxSafeForMining
#[test]
fn an_unseen_transaction_is_unsafe() {
    let r = rig();
    let block = r.chain.mine(vec![spend(1)], 0);
    assert!(matches!(r.signer.block_connected(&block, BlockId(0)), Cont::Done));
    r.clock.set_mocktime(T0 + 10 * WAIT_FOR_ISLOCK_TIMEOUT);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:151 TrySignChainTip
#[test]
fn a_locked_transaction_seen_now_is_signed_over() {
    let r = rig();
    r.connect(vec![spend(1)]);
    r.is.lock(spend(1).txid());
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:138-142 TrySignChainTip
#[test]
fn a_chainlocked_block_ends_the_walk() {
    let r = rig();
    r.connect(vec![spend(1)]);
    let locked = r.connect(vec![]).hash();
    r.connect(vec![]);
    r.cl.lock_at(&locked);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:132-136 TrySignChainTip
#[test]
fn the_walk_ends_below_five_blocks_under_the_tip() {
    let r = rig();
    r.connect(vec![spend(1)]);
    r.connect_empty(5);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    r.connect(vec![]);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:129 TrySignChainTip
#[test]
fn instantsend_disabled_skips_the_walk() {
    let r = rig();
    r.connect(vec![spend(1)]);
    r.is.set_enabled(false);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:144-148 TrySignChainTip, :239-242 GetBlockTxs
#[test]
fn an_unreadable_block_moves_the_walk_to_its_parent() {
    let r = rig();
    r.connect(vec![spend(1)]);
    let tip = r.chain.mine(vec![], 0).hash();
    r.chain.forget_body(&tip);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    assert!(!r.has_entry(&tip));
}

// Core: v24.0.2 src/chainlock/signing.cpp:163-177 TrySignChainTip
#[test]
fn the_request_is_cores() {
    let r = rig();
    let tip = r.tip();
    r.signer.try_sign_chain_tip();
    assert_eq!(
        r.signing.sign_requests(),
        [SignRequest {
            llmq_type: regtest().consensus.llmq_type_chain_locks,
            id: chainlock_request_id(tip.height),
            msg_hash: *tip.hash.as_bytes(),
            quorum_hash: None,
            allow_resign: false,
            allow_diff_msg: false,
        }]
    );
    let state = r.signer.state.lock();
    assert_eq!(state.last_signed_height, tip.height);
}

// Core: v24.0.2 src/chainlock/signing.cpp:199-209 BlockConnected
#[test]
fn a_connected_block_gets_an_entry_of_its_lockable_txids() {
    let r = rig();
    let only_coinbase = r.connect(vec![coinbase(0)]).hash();
    let mixed = r.connect(vec![coinbase(1), no_inputs(2), spend(3)]).hash();
    let state = r.signer.state.lock();
    assert!(state.block_txs[&only_coinbase].is_empty());
    assert_eq!(*state.block_txs[&mixed], HashSet::from([spend(3).txid()]));
}

// Core: v24.0.2 src/chainlock/signing.cpp:226-258 GetBlockTxs
#[test]
fn a_missing_entry_is_read_from_disk_and_seeds_first_seen_at_the_block_time() {
    let r = rig();
    let time = u32::try_from(T0 - WAIT_FOR_ISLOCK_TIMEOUT).unwrap();
    let tip = r.chain.mine(vec![coinbase(0), spend(1)], time).hash();
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
    assert_eq!(*r.signer.state.lock().block_txs[&tip], HashSet::from([spend(1).txid()]));
    assert!(r.first_seen.is_tx_safe_for_mining(&spend(1).txid()));
}

// Core: v24.0.2 src/chainlock/signing.cpp:273-276 HandleNewRecoveredSig
#[test]
fn a_recovered_sig_for_another_request_or_message_does_nothing() {
    let r = rig();
    let sig = r.signed_tip();
    for other in [r.recsig([1; 32], sig.msg_hash), r.recsig(sig.id, [1; 32])] {
        assert_eq!(r.signer.handle_new_recovered_sig(&other), RecoveredSigResult::Nothing);
    }
    assert!(r.process.calls.lock().is_empty());
}

// Core: v24.0.2 src/chainlock/signing.cpp:277-280 HandleNewRecoveredSig
#[test]
fn a_recovered_sig_under_an_equal_chainlock_does_nothing() {
    let r = rig();
    let sig = r.signed_tip();
    r.cl.lock_at(&r.tip().hash);
    assert_eq!(r.signer.handle_new_recovered_sig(&sig), RecoveredSigResult::Nothing);
    assert!(r.process.calls.lock().is_empty());
}

// Core: v24.0.2 src/chainlock/signing.cpp:265-267 HandleNewRecoveredSig
#[test]
fn a_recovered_sig_with_spork19_inactive_does_nothing() {
    let r = rig();
    let sig = r.signed_tip();
    *r.cl.enabled.lock() = false;
    assert_eq!(r.signer.handle_new_recovered_sig(&sig), RecoveredSigResult::Nothing);
    assert!(r.process.calls.lock().is_empty());
}

// Core: v24.0.2 src/chainlock/signing.cpp:282-290 HandleNewRecoveredSig
#[test]
fn a_matching_recovered_sig_is_processed_unlocked_and_relayed() {
    let r = rig();
    let tip = r.tip();
    let sig = r.signed_tip();
    let inv = Inventory::new(InvType::ClSig, Hash256::from_lendian([3; 32]));
    *r.process.inv.lock() = Some(inv);
    r.signing.register_listener(r.signer.clone());
    assert_eq!(r.signing.deliver(sig), [RecoveredSigResult::RelayInv(inv)]);
    let calls = r.process.calls.lock();
    assert_eq!(*calls, [(ClSig::new(tip.height, tip.hash, BlsSigBytes::from([5; 96])), true)]);
}

// Core: v24.0.2 src/chainlock/signing.cpp:287-290 HandleNewRecoveredSig
#[test]
fn a_processed_chainlock_without_an_inventory_relays_nothing() {
    let r = rig();
    let sig = r.signed_tip();
    assert_eq!(r.signer.handle_new_recovered_sig(&sig), RecoveredSigResult::Nothing);
    assert_eq!(r.process.calls.lock().len(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:300-302 Cleanup; util/time.h:50-64 TryCleanup
#[test]
fn cleanup_runs_at_most_every_thirty_seconds() {
    let r = rig();
    let unknown = BlockHash::from([9; 32]);
    r.signer.cleanup();
    r.signer.state.lock().block_txs.insert(unknown, Arc::default());
    r.clock.set_mocktime(T0 + 29);
    r.signer.cleanup();
    assert!(r.has_entry(&unknown));
    r.clock.set_mocktime(T0 + 30);
    r.signer.cleanup();
    assert!(!r.has_entry(&unknown));
}

// Core: v24.0.2 src/chainlock/signing.cpp:307-320 Cleanup
#[test]
fn cleanup_drops_unknown_chainlocked_and_conflicting_blocks() {
    let r = rig();
    let parent = r.tip().hash;
    let locked = r.connect(vec![spend(1)]).hash();
    let above = r.connect(vec![spend(2)]).hash();
    let fork = r.chain.mine_on(Some(parent), vec![spend(3)], 1);
    assert!(matches!(r.signer.block_connected(&fork, BlockId(0)), Cont::Done));
    assert!(matches!(r.first_seen.block_connected(&fork, BlockId(0)), Cont::Done));
    let gone = r.connect(vec![]).hash();
    r.chain.forget(&gone);
    r.cl.lock_at(&locked);
    r.signer.cleanup();
    let state = r.signer.state.lock();
    assert_eq!(state.block_txs.keys().collect::<Vec<_>>(), [&above]);
    assert_eq!(r.first_seen.len(), 2);
}

// Core: v24.0.2 src/chainlock/signing.cpp:78-81 TrySignChainTip
#[test]
fn a_busy_try_sign_returns_at_once() {
    let r = rig();
    let held = r.signer.try_sign.lock();
    r.signer.try_sign_chain_tip();
    drop(held);
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:177 TrySignChainTip
#[test]
fn storage_and_lookup_signing_faults_abort_and_a_stop_is_silent() {
    let r = rig();
    r.signing.set_sign_result(Err(SigningFault::Storage("disk".into())));
    r.signer.try_sign_chain_tip();
    r.signing.set_sign_result(Err(SigningFault::Stopped));
    r.connect(vec![]);
    r.signer.try_sign_chain_tip();
    r.signing.set_sign_result(Err(SigningFault::Lookup("quorum".into())));
    r.connect(vec![]);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 3);
    assert_eq!(r.aborts.lock().len(), 2);
}

/// An IS store whose every read fails.
struct FailingIs;

impl IsLockView for FailingIs {
    fn is_enabled(&self) -> bool {
        true
    }

    fn is_locked(&self, _: &TxHash) -> dash_kv::Result<bool> {
        Err(dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "scripted"))
    }
}

// Core: v24.0.2 src/chainlock/signing.cpp:151 TrySignChainTip; instantsend/instantsend.cpp:406
#[test]
fn an_is_store_fault_in_the_walk_aborts_without_signing() {
    let r = rig_with(Some(Arc::new(FailingIs)));
    r.connect(vec![spend(1)]);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    assert_eq!(r.aborts.lock().len(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:166-169 TrySignChainTip
#[test]
fn a_chainlock_arriving_during_the_walk_stops_signing() {
    let r = rig();
    let tip = r.tip().height;
    r.cl.heights.lock().extend([-1, tip]);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
    assert_eq!(r.signer.state.lock().last_signed_height, -1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:131-160 TrySignChainTip
#[test]
fn a_missing_parent_inside_the_window_refuses_the_tip() {
    let r = rig();
    let parent = r.connect(vec![]).hash();
    r.connect(vec![]);
    r.chain.forget(&parent);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 0);
}

// Core: v24.0.2 src/chainlock/signing.cpp:132-136 TrySignChainTip
#[test]
fn a_missing_parent_below_the_window_does_not_refuse_the_tip() {
    let r = rig();
    let below = r.tip().hash;
    r.connect_empty(6);
    r.chain.forget(&below);
    r.signer.try_sign_chain_tip();
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/signing.cpp:43-59 Start, Stop
#[test]
fn start_retries_every_five_seconds_while_signing_is_enabled() {
    let r = rig();
    r.signer.start();
    assert_eq!(r.scheduler.every(), Some(SIGN_INTERVAL));
    *r.cl.signing_enabled.lock() = false;
    r.scheduler.run();
    assert_eq!(r.requests(), 0);
    *r.cl.signing_enabled.lock() = true;
    r.scheduler.run();
    assert_eq!(r.requests(), 1);
    r.signer.stop();
    assert_eq!(*r.scheduler.stops.lock(), 1);
}

/// The signals thread is no-wait marked, so its hooks make no sign call there; the new tip is
/// signed when the scheduler runs the task the hook queued.
// Core: v24.0.2 src/chainlock/signing.cpp:71-74 UpdatedBlockTip
#[test]
fn a_new_tip_is_signed_on_the_scheduler_not_the_signals_thread() {
    let r = rig();
    {
        let _marked = NoWaitGuard::set();
        let block = r.chain.mine(vec![spend(1)], 0);
        assert!(matches!(r.signer.block_connected(&block, BlockId(0)), Cont::Done));
        r.signer.updated_block_tip(BlockId(0), None, false);
        r.signer.block_disconnected(&block, BlockId(0));
        assert_eq!(r.requests(), 0);
    }
    assert_eq!(r.scheduler.run_once(), 1);
    assert_eq!(r.requests(), 1);
}

// Core: v24.0.2 src/chainlock/chainlock.cpp:49-96 HasChainLock, HasConflictingChainLock
#[test]
fn chainlock_reads_follow_the_best_blocks_ancestry() {
    let r = rig();
    let (parent, tip) = (r.tip().prev.unwrap(), r.tip());
    let other = r.chain.mine_on(Some(parent), vec![], 1).hash();
    let chain = &*r.chain;
    let parent_h = tip.height - 1;
    assert!(has_chainlock(Some(&tip), parent_h, &parent, chain));
    assert!(!has_conflicting_chainlock(Some(&tip), parent_h, &parent, chain));
    assert!(has_conflicting_chainlock(Some(&tip), tip.height, &other, chain));
    assert!(!has_chainlock(Some(&tip), tip.height + 1, &other, chain));
    assert!(!has_conflicting_chainlock(Some(&tip), tip.height + 1, &other, chain));
    assert!(!has_conflicting_chainlock(None, 0, &other, chain));
}

// Core: v24.0.2 src/chainlock/chainlock.cpp:93-94 HasConflictingChainLock
#[test]
fn a_missing_ancestor_is_no_conflict() {
    let r = rig();
    let best = ChainEntry {
        hash: BlockHash::from([9; 32]),
        height: 5,
        prev: None,
    };
    assert!(!has_conflicting_chainlock(Some(&best), 3, &r.tip().hash, &*r.chain));
    assert!(!has_chainlock(Some(&best), 3, &r.tip().hash, &*r.chain));
}
