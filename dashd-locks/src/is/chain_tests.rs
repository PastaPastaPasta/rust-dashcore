//! [`IsHelper`], [`IsAttemptLedger`] and [`IsLedgerApplier`] (PLAN-LK1b §6 S1b). The removal
//! cases are the removal bridge's (S1b-R, `chain/removal_tests.rs`); here a removal runs through
//! the bridge over the S1a-2 store.

use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::fs::DirBuilderExt as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use dash_chain::{ChainConfig, FailStop, HeaderChain, Services, StaticSporks};
use dash_clock::{Clock, MockClock};
use dash_consensus::connect::has_lock_inputs;
use dash_consensus::hash::sha256d;
use dash_consensus::header::HeaderParams;
use dash_consensus::merkle::compute_merkle_root;
use dash_consensus::pow::{PowParams, check_proof_of_work};
use dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE;
use dash_consensus::primitives::{
    Block, BlockHash, BlockHeader, Header, MerkleRoot, OutPoint, Transaction, Tx, TxHash, TxIn,
    TxOut, TxType,
};
use dash_consensus::script::opcodes::{OP_RESERVED, OP_TRUE};
use dash_consensus::script::{ScriptNum, push_data};
use dash_consensus::tx::COINBASE_MATURITY;
use dash_evo::llmq::{LlmqCommitment, QuorumSnapshot};
use dash_evo::locks::isdlock_hash;
use dash_evo::types::ProTxHash;
use dash_evo::{Effect, EffectSink};
use dash_kv::{ErrorKind, KvEngine, KvRead, MemoryEngine, StoreError};
use dash_num::{CompactTarget, Hash256};
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::QuorumHash;
use dash_primitives::TxOut as PrimTxOut;
use dash_statedigest::decisions::{Policy, RecordedDecisions};
use dash_statedigest::{
    Chain, Effect as FxEffect, FileKind, Hash256 as DigestHash, Header as DigestHeader, Kind,
    Lines, Record, Writer,
};
use dash_types::Numeric as _;
use dashd_chainstate::chain::{
    AbortPath, ChainHandle, ChainLockSig, ConnectCx, CoreChecks, Fatal, GuardScope, IsConflicts,
    LocalShutdown, NoFlush, NoInstantSend, NoUi, NodeBlockIndex, NodeJoinHandle, NullMempool,
    ReadAheadSetup, RecordingAbort, SignalsThread, VExit, VParts, ValidationSignals, genesis_block,
    load_genesis_block, spawn,
};
use dashd_chainstate::checkqueue::CheckQueue;
use dashd_chainstate::coins::{CoinsBudget, CoinsCache, Ratio, Salt};
use dashd_chainstate::connect::{
    AttemptChoice, AttemptId, BlockAt, ChainHelper, ConnectorParts, CoreConnector, DecisionTap,
    DigestLog, DigestSink, EffectCoverage, EvoHooks, FinishReport, IsLockRef, MissSlot, NoEvo,
    NodeHelper, RecordingHelper, ReplayChainHelper, ScriptedEvo, TapedLog,
};
use dashd_chainstate::evo::effects::{
    EffectAppliers, IsLockApplier, MineableApplier, NotifyApplier, PlatformBanApplier,
    SnapshotApplier,
};
use dashd_chainstate::sigcache::{CacheSizes, ScriptCaches, ScriptExecCache, SigCache};
use dashd_chainstate::store::{
    CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, GuardedFiles, NodeBlockFiles,
};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs};

use super::removal::NO_PENDING_TX;
use super::test_log as capture;
use super::{
    GateSource, IsAttemptLedger, IsHelper, IsLedgerApplier, REPLAY_DIVERGED, SharedLedger,
    UNSTAGED_REMOVAL,
};
use crate::is::db::removal_tests::{READ_FAULT, ReadFault};
use crate::is::db::{CacheSnapshot, IS, IsDb, SCHEMA};
use crate::is::fx::IsFxSource;
use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
use crate::is::seams::{IsFxLedger, IsObservation, IsStore, StageId};
use crate::is::{IsGate, IsLockHash, IsTipCache, LockedTx, lock_inputs};

// Shared fixtures.

fn store_error(text: &str) -> StoreError {
    StoreError::engine("test", ErrorKind::Io, text)
}

fn gate(spork2: bool) -> GateSource {
    Arc::new(move || IsGate::new(spork2, false, false))
}

fn ledger() -> SharedLedger {
    IsAttemptLedger::shared()
}

fn prevout(tag: u8, n: u32) -> OutPoint {
    OutPoint::new(TxHash::from([tag; 32]), n)
}

fn spend_of(prevouts: &[OutPoint]) -> Tx {
    special(2, 0, prevouts, vec![])
}

fn lock_for(txid: TxHash, inputs: Vec<OutPoint>) -> LockedTx {
    LockedTx::new(1, inputs, txid, Default::default(), [0; 96].into())
}

fn lock_ref(hash: IsLockHash, lock: &LockedTx) -> IsLockRef {
    IsLockRef {
        hash: Hash256::from(hash),
        txid: lock.txid(),
    }
}

fn observation(lock_hash: IsLockHash, stage: u64) -> IsObservation {
    IsObservation {
        lock_hash,
        overriding_txid: TxHash::from([0xee; 32]),
        locked_txid: TxHash::from([0xdd; 32]),
        archive_height: 7,
        root_stored: true,
        removed: vec![lock_hash],
        stage: StageId(stage),
    }
}

/// An inner helper for the unit cases: scripted `is_conflict` answers (a replay's recording), and
/// a count of closed attempts. No case here asks a query that needs a [`ConnectCx`].
type Recorded = Arc<StdMutex<VecDeque<Result<Option<IsLockRef>, Fatal>>>>;

#[derive(Clone, Default)]
struct Scripted {
    answers: Recorded,
    ended: Arc<StdMutex<usize>>,
}

impl ChainHelper for Scripted {
    fn cl_conflict(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn ibd(&mut self, _: &ConnectCx<'_>) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn cl_has(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn cl_best(&mut self, _: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        Ok(None)
    }

    fn is_conflict(&mut self, _: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        self.answers.lock().unwrap().pop_front().expect("a recorded answer")
    }

    fn remove_conflicting_is_lock(&mut self, _: &IsLockRef, _: &mut dyn EffectSink) {
        panic!("the inner helper's removal is never asked");
    }

    fn end_attempt(&mut self) {
        *self.ended.lock().unwrap() += 1;
    }

    fn sb_required(&mut self, _: &ConnectCx<'_>, _: &BlockAt) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<PrimTxOut>>, Fatal> {
        Ok(None)
    }
}

/// A store holding one lock `L` for another transaction, which `tx` conflicts with.
fn conflicting_store(tx: &Tx) -> (InMemoryIsStore, IsLockHash, LockedTx) {
    let mut store = InMemoryIsStore::default();
    let lock = lock_for(TxHash::from([0x4c; 32]), lock_inputs(tx));
    let hash = store.insert(lock.clone());
    store.conflicts.insert(tx.txid(), hash);
    (store, hash, lock)
}

fn live(store: Arc<InMemoryIsStore>, spork2: bool) -> (IsHelper<Scripted>, Scripted) {
    let probe = Scripted::default();
    (IsHelper::live(probe.clone(), store, ledger(), gate(spork2), IsTipCache::new()), probe)
}

fn replaying(store: Arc<InMemoryIsStore>, spork2: bool) -> (IsHelper<Scripted>, Scripted) {
    let probe = Scripted::default();
    (IsHelper::replaying(probe.clone(), store, ledger(), gate(spork2), IsTipCache::new()), probe)
}

// `is_conflict`, live (§5.3 item 1; Core evo/chainhelper.cpp:79-85, instantsend.cpp:428-445).

#[test]
fn no_conflict_answers_none_from_one_overlay_read() {
    let tx = spend_of(&[prevout(1, 0)]);
    let store = Arc::new(InMemoryIsStore::default());
    let (mut helper, _) = live(store.clone(), true);
    assert_eq!(helper.is_conflict(&tx).unwrap(), None);
    assert_eq!(store.calls(), [IsStoreCall::ConflictingLockIn(tx.txid())]);
}

#[test]
fn a_conflicting_lock_is_core_s_hash_and_txid_pair() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let (mut helper, _) = live(Arc::new(store), true);
    assert_eq!(helper.is_conflict(&tx).unwrap(), Some(lock_ref(hash, &lock)));
}

#[test]
fn instantsend_disabled_answers_none_without_a_store_read() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, _, _) = conflicting_store(&tx);
    let store = Arc::new(store);
    for flags in [(false, false, false), (true, true, false), (true, false, true)] {
        let gate: GateSource = Arc::new(move || IsGate::new(flags.0, flags.1, flags.2));
        let mut helper =
            IsHelper::live(Scripted::default(), store.clone(), ledger(), gate, IsTipCache::new());
        assert_eq!(helper.is_conflict(&tx).unwrap(), None, "{flags:?}");
    }
    assert_eq!(store.calls(), []);
}

/// The gate is read at each lookup, not at construction (SPORK_2 can change between blocks).
#[test]
fn the_gate_is_read_per_lookup() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let on = Arc::new(AtomicBool::new(false));
    let flag = on.clone();
    let gate: GateSource = Arc::new(move || IsGate::new(flag.load(Ordering::SeqCst), false, false));
    let mut helper =
        IsHelper::live(Scripted::default(), Arc::new(store), ledger(), gate, IsTipCache::new());
    assert_eq!(helper.is_conflict(&tx).unwrap(), None);
    on.store(true, Ordering::SeqCst);
    assert_eq!(helper.is_conflict(&tx).unwrap(), Some(lock_ref(hash, &lock)));
}

/// A read fault is Core's thrown `dbwrapper_error`: a `Fatal` with the store's text, returned
/// at that query and not kept for the next one.
#[test]
fn a_store_read_fault_is_fatal_once_with_the_store_s_text() {
    let tx = spend_of(&[prevout(1, 0)]);
    let store = Arc::new(InMemoryIsStore::default());
    let error = store_error("is_in read failed");
    let text = error.to_string();
    store.scripted.lock().unwrap().conflicting_lock_in.push_back(Err(error));
    let (mut helper, _) = live(store.clone(), true);
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, text);
    assert_eq!(helper.is_conflict(&tx).unwrap(), None);
    assert_eq!(store.calls().len(), 2);
}

// A removal through the bridge over the S1a-2 store (§5.3 items 1, 2 and 4).

/// The removal stages Core's chained removal, observes it and emits its effect; the next query
/// reads through the overlay and finds nothing, and the rows change when the applier commits.
#[test]
fn a_removal_stages_observes_and_emits_and_the_next_query_sees_it() {
    let tx = spend_of(&[prevout(1, 0)]);
    let lock = conflict_lock(&tx);
    let is = Removing::over(Arc::new(MemoryEngine::new(&SCHEMA)), std::slice::from_ref(&lock));
    let (hash, db, ledger) = (is.hashes[0], is.db.clone(), is.ledger.clone());
    let tip = IsTipCache::new();
    tip.set(9);
    let mut helper =
        IsHelper::live(Scripted::default(), db.clone(), ledger.clone(), gate(true), tip);
    let found = helper.is_conflict(&tx).unwrap().expect("the conflict");
    assert_eq!(found, lock_ref(hash, &lock));
    let mut log: Vec<Effect> = Vec::new();
    helper.remove_conflicting_is_lock(&found, &mut log);
    assert_eq!(log, [removal(hash)]);
    assert_eq!(helper.is_conflict(&tx).unwrap(), None, "the overlay hides the staged removal");
    IsLedgerApplier::new(db.clone(), ledger.clone()).remove(&Hash256::from(hash)).unwrap();
    helper.end_attempt();
    let taken = ledger.lock().take();
    let staged = IsObservation {
        lock_hash: hash,
        overriding_txid: tx.txid(),
        locked_txid: lock.txid(),
        archive_height: 9,
        root_stored: true,
        removed: vec![hash],
        stage: taken.observations[0].stage,
    };
    assert_eq!(taken.observations, [staged]);
    assert_eq!(taken.confirmed, [hash]);
    let reopened = is.reopened();
    assert_eq!(reopened.by_hash(&hash, false).unwrap(), None);
    assert!(reopened.known(&hash).unwrap(), "archived");
}

/// A query that fails drops the transaction an earlier query found: a removal after it has no
/// pending transaction.
#[test]
fn a_failed_query_leaves_no_pending_tx() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, _, _) = conflicting_store(&tx);
    let store = Arc::new(store);
    let (mut helper, _) = live(store.clone(), true);
    let found = helper.is_conflict(&tx).unwrap().expect("the conflict");
    store.scripted.lock().unwrap().conflicting_lock_in.push_back(Err(store_error("read failed")));
    assert!(helper.is_conflict(&tx).is_err());
    helper.remove_conflicting_is_lock(&found, &mut Vec::new());
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, NO_PENDING_TX);
}

/// A lock reference for a removal no query found: the placeholder keeps [`NO_PENDING_TX`].
fn any_lock() -> IsLockRef {
    IsLockRef {
        hash: Hash256::from([1; 32]),
        txid: TxHash::from([2; 32]),
    }
}

/// A kept fault comes before the gate and the store: it is the attempt's, whatever the spork.
#[test]
fn a_kept_fault_is_returned_before_the_store_is_read() {
    let tx = spend_of(&[prevout(1, 0)]);
    let store = Arc::new(InMemoryIsStore::default());
    let (mut helper, _) = live(store.clone(), false);
    helper.remove_conflicting_is_lock(&any_lock(), &mut Vec::new());
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, NO_PENDING_TX);
    assert_eq!(store.calls(), []);
}

#[test]
fn end_attempt_discards_stages_clears_the_pending_tx_and_closes_the_inner_attempt() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, _, _) = conflicting_store(&tx);
    let store = Arc::new(store);
    let (mut helper, probe) = live(store.clone(), true);
    assert!(helper.is_conflict(&tx).unwrap().is_some());
    helper.end_attempt();
    assert_eq!(store.calls().last(), Some(&IsStoreCall::DiscardStaged));
    assert_eq!(*probe.ended.lock().unwrap(), 1);
    // The pending transaction did not survive the attempt.
    helper
        .remove_conflicting_is_lock(&lock_ref([0; 32], &lock_for(tx.txid(), vec![])), &mut vec![]);
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, NO_PENDING_TX);
}

/// A kept fault no query surfaced fails `end_attempt` loudly in a debug build (§5.3 item 2, r2d).
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "IS fault not surfaced")]
fn end_attempt_with_an_unsurfaced_fault_panics_in_a_debug_build() {
    let (mut helper, _) = live(Arc::new(InMemoryIsStore::default()), true);
    helper.remove_conflicting_is_lock(&any_lock(), &mut Vec::new());
    helper.end_attempt();
}

/// A release build clears an unsurfaced fault: the attempt closes as usual, and the next query
/// answers from the store (§5.3 item 2, r2d).
#[cfg(not(debug_assertions))]
#[test]
fn end_attempt_with_an_unsurfaced_fault_clears_it_in_a_release_build() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let store = Arc::new(store);
    let (mut helper, probe) = live(store.clone(), true);
    helper.remove_conflicting_is_lock(&any_lock(), &mut Vec::new());
    helper.end_attempt();
    assert_eq!(store.calls(), [IsStoreCall::DiscardStaged]);
    assert_eq!(*probe.ended.lock().unwrap(), 1);
    assert_eq!(helper.is_conflict(&tx).unwrap(), Some(lock_ref(hash, &lock)));
}

/// `end_attempt` leaves the observations for the digest's `fx` source, which takes them when it
/// records the attempt, after `end_attempt` (§5.3 item 5).
#[test]
fn observations_survive_end_attempt() {
    let store = Arc::new(InMemoryIsStore::default());
    let ledger = ledger();
    let mut helper =
        IsHelper::live(Scripted::default(), store, ledger.clone(), gate(true), IsTipCache::new());
    ledger.lock().push(observation([1; 32], 0));
    helper.end_attempt();
    assert_eq!(ledger.lock().take().observations, [observation([1; 32], 0)]);
}

// `is_conflict`, replaying (§5.3 item 3, review R2-3).

#[test]
fn a_replay_consumes_the_recorded_answer_and_checks_it_against_the_store() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let store = Arc::new(store);
    let (mut helper, probe) = replaying(store.clone(), true);
    probe.answers.lock().unwrap().push_back(Ok(Some(lock_ref(hash, &lock))));
    assert_eq!(helper.is_conflict(&tx).unwrap(), Some(lock_ref(hash, &lock)));
    assert!(probe.answers.lock().unwrap().is_empty());
    assert_eq!(store.calls(), [IsStoreCall::ConflictingLockIn(tx.txid())]);
}

/// A recorded `None` still runs the live lookup, so the store's caches warm as in the live run.
#[test]
fn a_recorded_none_still_reads_the_store() {
    let tx = spend_of(&[prevout(1, 0)]);
    let store = Arc::new(InMemoryIsStore::default());
    let (mut helper, probe) = replaying(store.clone(), true);
    probe.answers.lock().unwrap().push_back(Ok(None));
    assert_eq!(helper.is_conflict(&tx).unwrap(), None);
    assert_eq!(store.calls(), [IsStoreCall::ConflictingLockIn(tx.txid())]);
}

#[test]
fn a_recorded_answer_the_store_does_not_give_is_fatal() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let store = Arc::new(store);
    let other = lock_for(TxHash::from([0x99; 32]), vec![]);
    let cases = [
        // Recorded a lock, the store has none.
        (Arc::new(InMemoryIsStore::default()), true, Some(lock_ref(hash, &lock))),
        // Recorded none, the store finds L.
        (store.clone(), true, None),
        // Recorded another lock.
        (store.clone(), true, Some(lock_ref([0x77; 32], &other))),
        // Recorded a lock while the spork is off here.
        (store, false, Some(lock_ref(hash, &lock))),
    ];
    for (n, (store, spork2, recorded)) in cases.into_iter().enumerate() {
        let (mut helper, probe) = replaying(store, spork2);
        probe.answers.lock().unwrap().push_back(Ok(recorded));
        let fault = helper.is_conflict(&tx).unwrap_err().0;
        assert!(fault.starts_with(REPLAY_DIVERGED), "case {n}: {fault}");
    }
}

#[test]
fn a_replay_s_decision_miss_is_its_fatal_and_reads_no_store() {
    let tx = spend_of(&[prevout(1, 0)]);
    let store = Arc::new(InMemoryIsStore::default());
    let (mut helper, probe) = replaying(store.clone(), true);
    probe.answers.lock().unwrap().push_back(Err(Fatal::new("decision miss: test")));
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, "decision miss: test");
    assert_eq!(store.calls(), []);
}

/// A kept fault is surfaced before the recorded answer is consumed, so the replay's cursor stays
/// where the live run's query sequence has it.
#[test]
fn a_replay_returns_a_kept_fault_before_consuming_a_decision() {
    let tx = spend_of(&[prevout(1, 0)]);
    let (store, hash, lock) = conflicting_store(&tx);
    let store = Arc::new(store);
    let (mut helper, probe) = replaying(store.clone(), true);
    probe.answers.lock().unwrap().push_back(Ok(Some(lock_ref(hash, &lock))));
    let found = helper.is_conflict(&tx).unwrap().expect("the conflict");
    probe.answers.lock().unwrap().push_back(Ok(None));
    let fault = store_error("second lookup failed");
    let text = fault.to_string();
    store.scripted.lock().unwrap().conflicting_lock_in.push_back(Err(fault));
    helper.remove_conflicting_is_lock(&found, &mut Vec::new());
    assert_eq!(helper.is_conflict(&tx).unwrap_err().0, text);
    assert_eq!(probe.answers.lock().unwrap().len(), 1, "the next decision is still there");
}

// The ledger and its applier (§5.3 items 4 and 5).

/// The appliers of the other effect kinds, which these logs never reach.
struct Unused;

impl SnapshotApplier for Unused {
    fn write(&mut self, _: LlmqType, _: &BlockHash, _: &QuorumSnapshot) -> Result<(), Fatal> {
        Err(Fatal::new("unexpected snapshot"))
    }
}

impl MineableApplier for Unused {
    fn erase(&mut self, _: LlmqType, _: &QuorumHash, _: &[u8; 32]) -> Result<(), Fatal> {
        Err(Fatal::new("unexpected erase"))
    }

    fn reinsert(&mut self, _: &Arc<LlmqCommitment>) -> Result<(), Fatal> {
        Err(Fatal::new("unexpected reinsert"))
    }
}

impl PlatformBanApplier for Unused {
    fn reset(&mut self, _: &ProTxHash, _: i32) -> Result<(), Fatal> {
        Err(Fatal::new("unexpected ban reset"))
    }
}

impl NotifyApplier for Unused {
    fn notify(&mut self, _: &BlockHash, _: bool) -> Result<(), Fatal> {
        Err(Fatal::new("unexpected notify"))
    }
}

fn appliers(store: Arc<dyn IsStore>, ledger: SharedLedger) -> EffectAppliers {
    EffectAppliers {
        snapshot: Box::new(Unused),
        is_lock: Box::new(IsLedgerApplier::new(store, ledger)),
        mineable: Box::new(Unused),
        platform_ban: Box::new(Unused),
        notify: Box::new(Unused),
    }
}

fn removal(hash: IsLockHash) -> Effect {
    Effect::RemoveConflictingIsLock {
        lock_hash: Hash256::from(hash),
    }
}

fn commits(store: &InMemoryIsStore) -> Vec<IsStoreCall> {
    store.calls().into_iter().filter(|c| matches!(c, IsStoreCall::CommitStaged(_))).collect()
}

#[test]
fn the_applier_commits_each_observed_stage_in_log_order() {
    let store = Arc::new(InMemoryIsStore::default());
    let ledger = ledger();
    ledger.lock().push(observation([1; 32], 4));
    ledger.lock().push(observation([2; 32], 5));
    let log = [removal([1; 32]), removal([2; 32])];
    appliers(store.clone(), ledger.clone()).apply_log(AttemptId::next(), &log).unwrap();
    assert_eq!(
        commits(&store),
        [IsStoreCall::CommitStaged(StageId(4)), IsStoreCall::CommitStaged(StageId(5))]
    );
    let taken = ledger.lock().take();
    assert_eq!(taken.observations, [observation([1; 32], 4), observation([2; 32], 5)]);
    assert_eq!(taken.confirmed, [[1; 32], [2; 32]]);
    assert_eq!(taken.failed_at, None);
    assert_eq!(ledger.lock().take(), Default::default(), "take empties the ledger");
}

/// Exactly one `Err` per injected fault, with the store's text; the failed position is kept and
/// the cursor stays before it. Earlier commits stand, as Core's inline writes before a throw do.
#[test]
fn a_commit_write_failure_is_fatal_at_the_applier() {
    let store = Arc::new(InMemoryIsStore::default());
    let error = store_error("commit failed");
    let text = error.to_string();
    store.scripted.lock().unwrap().commit_staged.extend([Ok(()), Err(error)]);
    let ledger = ledger();
    ledger.lock().push(observation([1; 32], 0));
    ledger.lock().push(observation([2; 32], 1));
    let log = [removal([1; 32]), removal([2; 32])];
    let fault = appliers(store.clone(), ledger.clone()).apply_log(AttemptId::next(), &log);
    assert_eq!(fault.unwrap_err().0, text);
    assert_eq!(commits(&store).len(), 2);
    let taken = ledger.lock().take();
    assert_eq!(taken.confirmed, [[1; 32]]);
    assert_eq!(taken.failed_at, Some(1));
    assert_eq!(taken.observations.len(), 2, "the applier never removes an observation");
}

#[test]
fn the_applier_without_an_observation_is_fatal() {
    let store = Arc::new(InMemoryIsStore::default());
    let ledger = ledger();
    let fault =
        appliers(store.clone(), ledger.clone()).apply_log(AttemptId::next(), &[removal([1; 32])]);
    assert_eq!(fault.unwrap_err().0, UNSTAGED_REMOVAL);
    assert_eq!(commits(&store), []);
    assert_eq!(ledger.lock().take().failed_at, Some(0));

    // Past the last observation too.
    ledger.lock().push(observation([1; 32], 0));
    let log = [removal([1; 32]), removal([1; 32])];
    let fault = appliers(store.clone(), ledger.clone()).apply_log(AttemptId::next(), &log);
    assert_eq!(fault.unwrap_err().0, UNSTAGED_REMOVAL);
    assert_eq!(commits(&store).len(), 1);
    let taken = ledger.lock().take();
    assert_eq!((taken.confirmed.len(), taken.failed_at), (1, Some(1)));
}

#[test]
fn the_applier_with_a_mismatched_hash_is_fatal() {
    let store = Arc::new(InMemoryIsStore::default());
    let ledger = ledger();
    ledger.lock().push(observation([1; 32], 0));
    let fault =
        appliers(store.clone(), ledger.clone()).apply_log(AttemptId::next(), &[removal([2; 32])]);
    assert_eq!(fault.unwrap_err().0, UNSTAGED_REMOVAL);
    assert_eq!(commits(&store), []);
    let taken = ledger.lock().take();
    assert_eq!((taken.confirmed.len(), taken.failed_at), (0, Some(0)));
}

#[test]
fn clear_drops_observations_cursor_and_failure() {
    let store = InMemoryIsStore::default();
    let mut ledger = IsAttemptLedger::default();
    ledger.push(observation([1; 32], 0));
    ledger.confirm_next(&[1; 32], &store).unwrap();
    assert!(ledger.confirm_next(&[1; 32], &store).is_err());
    ledger.clear();
    assert_eq!(ledger.take(), Default::default());
}

// `lock_inputs` (Core instantsend/lock.cpp:46-57).

fn special(version: i16, tx_type: u16, inputs: &[OutPoint], payload: Vec<u8>) -> Tx {
    let inputs = inputs.iter().map(|o| TxIn::new(*o, Default::default(), u32::MAX)).collect();
    Tx::new(Transaction::new(version, TxType::new(tx_type), inputs, vec![], 0, payload))
}

fn unlock_payload(index: u64) -> Vec<u8> {
    let mut payload = vec![0xab; ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[0] = 1;
    payload[1..9].copy_from_slice(&index.to_le_bytes());
    payload
}

#[test]
fn lock_inputs_are_the_prevouts_in_input_order() {
    let inputs = [prevout(3, 1), prevout(1, 0), prevout(3, 1), prevout(2, 7)];
    assert_eq!(lock_inputs(&spend_of(&inputs)), inputs);
    assert_eq!(lock_inputs(&spend_of(&[])), []);
    // A coinbase's null prevout is its lock input; `has_lock_inputs` is what excludes it.
    let null = OutPoint::new(TxHash::from([0; 32]), u32::MAX);
    let coinbase = spend_of(&[null]);
    assert_eq!(lock_inputs(&coinbase), [null]);
    assert!(!has_lock_inputs(&coinbase));
}

/// An asset unlock's one lock input is `(SerializeHash(("plwdtx", index)), 0)`, whatever its
/// inputs: `SerializeHash` of a `pair<string_view, uint64_t>` is the compact-size-prefixed
/// string, then the index little-endian.
#[test]
fn an_asset_unlock_s_lock_input_is_its_withdrawal_request_id() {
    for index in [0, 42, u64::MAX] {
        let expected = {
            let mut bytes = vec![6];
            bytes.extend_from_slice(b"plwdtx");
            bytes.extend_from_slice(&index.to_le_bytes());
            OutPoint::new(TxHash::from(sha256d(&bytes)), 0)
        };
        for inputs in [&[][..], &[prevout(5, 0)][..]] {
            for version in [3, 4] {
                let tx = special(version, 9, inputs, unlock_payload(index));
                assert_eq!(lock_inputs(&tx), [expected], "index {index} v{version} {inputs:?}");
                assert!(has_lock_inputs(&tx));
            }
        }
    }
    // Only the index bytes count.
    let mut payload = unlock_payload(42);
    payload[0] = 2;
    payload[9..].fill(0);
    assert_eq!(
        lock_inputs(&special(3, 9, &[], payload)),
        lock_inputs(&special(3, 9, &[], unlock_payload(42)))
    );
}

/// `IsAssetUnlockPayload` reads the version, the type and the payload size: anything else is an
/// ordinary transaction's prevouts.
#[test]
fn a_malformed_asset_unlock_has_its_prevouts() {
    let inputs = [prevout(5, 0)];
    for tx in [
        special(2, 9, &inputs, unlock_payload(1)),
        special(3, 9, &inputs, unlock_payload(1)[..ASSET_UNLOCK_PAYLOAD_SIZE - 1].to_vec()),
        special(3, 9, &inputs, [unlock_payload(1), vec![0]].concat()),
        special(3, 8, &inputs, unlock_payload(1)),
    ] {
        assert_eq!(lock_inputs(&tx), inputs);
    }
    assert_eq!(lock_inputs(&special(3, 9, &[], vec![0; 3])), []);
}

// Through `CoreConnector`: V, `NoEvo`, and the IS helper around `NodeHelper` (§6 S1b).

/// DIP0003, V20 and MN_RR, past any height a case reaches: pre-DIP3 blocks need no CbTx.
const NO_DIP3: u32 = 10_000_000;

const MATURITY: u32 = COINBASE_MATURITY as u32;

const OUTPUT_VALUE: i64 = 50_000;

fn params() -> Arc<ChainSetup> {
    let at = |name: &str| format!("{name}@{NO_DIP3}");
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, format!("{NO_DIP3}:{NO_DIP3}"))
        .arg(ChainOption::TestActivationHeight, at("v20"))
        .arg(ChainOption::TestActivationHeight, at("mn_rr"))
        .build()
        .expect("regtest's parameters");
    Arc::new(params)
}

fn pow_of(p: &ChainSetup) -> PowParams {
    *HeaderParams::from_chain_params(p).expect("regtest's header rules").pow()
}

/// The header chain's fail-stop: a read failure fails the case, unless V is already unwinding
/// from a case's injected panic, when it goes to the abort sink as ND1's `ReadFailStop` does.
struct NoReads(Arc<RecordingAbort>);

impl FailStop for NoReads {
    fn read_failed(&self, what: &str) {
        if std::thread::panicking() {
            dashd_chainstate::chain::AbortSink::read_failed(&*self.0, what);
            return;
        }
        panic!("a header read failed: {what}");
    }

    fn is_tripped(&self) -> bool {
        false
    }
}

fn op_true(value: i64) -> TxOut {
    TxOut::from_duffs(value, vec![OP_TRUE].into())
}

fn null_prevout() -> OutPoint {
    OutPoint::new(TxHash::from([0; 32]), u32::MAX)
}

/// A one-input spend paying `value` to `OP_TRUE`.
fn tx_spending(prevout: OutPoint, value: i64) -> Arc<Tx> {
    tx_spending_each(&[prevout], value)
}

/// A spend of every `prevout`, in order, paying `value` to `OP_TRUE`.
fn tx_spending_each(prevouts: &[OutPoint], value: i64) -> Arc<Tx> {
    let inputs = prevouts.iter().map(|p| TxIn::new(*p, Vec::new().into(), u32::MAX)).collect();
    let tx = Transaction::new(2, TxType::new(0), inputs, vec![op_true(value)], 0, Vec::new());
    Arc::new(Tx::new(tx))
}

// Core: script/script.h:427 CScript::push_int64
fn height_push(n: u32) -> Vec<u8> {
    let mut out = Vec::new();
    match n {
        1..=16 => out.push(OP_RESERVED + n as u8),
        _ => push_data(&mut out, &ScriptNum::new(i64::from(n)).encode()),
    }
    out
}

/// A block on `parent` at height `h`: a coinbase paying `coinbase_value` (and `tag` in its
/// script, so siblings differ), then `txs`.
fn block_on(
    pow: &PowParams,
    parent: &Block,
    h: u32,
    tag: u8,
    coinbase_value: i64,
    txs: &[Arc<Tx>],
) -> Arc<Block> {
    let mut script_sig = height_push(h);
    script_sig.push(tag);
    let coinbase = Arc::new(Tx::new(Transaction::new(
        2,
        TxType::new(0),
        vec![TxIn::new(null_prevout(), script_sig.into(), u32::MAX)],
        vec![op_true(coinbase_value)],
        0,
        Vec::new(),
    )));
    let txs: Vec<Arc<Tx>> = std::iter::once(coinbase).chain(txs.iter().cloned()).collect();
    let prev = parent.header().raw();
    let (root, _) = compute_merkle_root(txs.iter().map(|tx| *tx.txid().as_bytes()).collect());
    let mut raw = BlockHeader::new(
        0x2000_0000,
        parent.hash(),
        MerkleRoot::from_lendian(root),
        prev.time() + 150,
        prev.bits(),
        0,
    );
    let target = CompactTarget::new(raw.bits());
    let header = loop {
        let header = Header::new(raw);
        if check_proof_of_work(header.hash().as_bytes(), target, pow) {
            break header;
        }
        raw.set_nonce(raw.nonce() + 1);
    };
    Arc::new(Block::new(header, txs))
}

/// Genesis, then coinbase-only blocks to height `MATURITY`, so the next block may spend the
/// coinbase at height 1.
struct Matured {
    pow: PowParams,
    chain: Vec<Arc<Block>>,
}

impl Matured {
    fn new() -> Matured {
        Matured::deeper(0)
    }

    /// The chain `extra` blocks past `MATURITY`: the next block may spend the coinbases at
    /// heights `1..=extra + 1`.
    fn deeper(extra: u32) -> Matured {
        let p = params();
        let pow = pow_of(&p);
        let mut chain = vec![genesis_block(&p)];
        for h in 1..=MATURITY + extra {
            let block = block_on(&pow, chain.last().unwrap(), h, 0, OUTPUT_VALUE, &[]);
            chain.push(block);
        }
        Matured {
            pow,
            chain,
        }
    }

    fn tip(&self) -> &Arc<Block> {
        self.chain.last().unwrap()
    }

    /// The matured coinbase output at height 1.
    fn coin(&self) -> OutPoint {
        self.coin_at(1)
    }

    /// The coinbase output at height `h`.
    fn coin_at(&self, h: usize) -> OutPoint {
        OutPoint::new(self.chain[h].txs()[0].txid(), 0)
    }

    /// The next block's height.
    fn next_height(&self) -> u32 {
        u32::try_from(self.chain.len()).expect("a short chain")
    }

    /// A spend of [`Self::coin`].
    fn spend(&self) -> Arc<Tx> {
        tx_spending(self.coin(), 40_000)
    }

    /// The block after the tip holding `txs`.
    fn next(&self, txs: &[Arc<Tx>]) -> Arc<Block> {
        block_on(&self.pow, self.tip(), self.next_height(), 0, OUTPUT_VALUE, txs)
    }
}

/// Core's line for a conflict without a ChainLock (validation.cpp:2825).
fn conflict_line(tx: TxHash, locked: TxHash) -> String {
    format!("ERROR: ConnectBlock(DASH): transaction {tx} conflicts with transaction lock {locked}")
}

/// A shared byte buffer for the digest's two streams.
#[derive(Clone, Default)]
struct Buf(Arc<StdMutex<Vec<u8>>>);

impl Buf {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl Write for Buf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn digest_header(file: FileKind) -> DigestHeader {
    DigestHeader {
        file,
        src: Writer::Rdashd,
        ver: b"s1b".to_vec(),
        chain: Chain::Regtest,
        run: b"1-1".to_vec(),
    }
}

/// One V's node: block and undo files on disk, coins in memory, `NoEvo`, no ChainLocks.
struct Rig {
    latch: GlobalFailStop,
    abort: Arc<RecordingAbort>,
    clock: Arc<MockClock>,
    /// The clock's offset past each submitted block's time: past the tip age, V stays in IBD.
    ahead: i64,
    v: ChainHandle,
    join: NodeJoinHandle<VExit>,
    signals: SignalsThread,
    _dir: tempfile::TempDir,
}

impl Rig {
    fn start(helper: Box<dyn ChainHelper>, digest: Option<Box<dyn DigestSink>>, ahead: i64) -> Rig {
        Rig::start_with(helper, digest, ahead, NoEvo)
    }

    /// [`Self::start`] with `hooks` in place of `NoEvo`.
    fn start_with<H: EvoHooks<Facade = ()> + Send + 'static>(
        helper: Box<dyn ChainHelper>,
        digest: Option<Box<dyn DigestSink>>,
        ahead: i64,
        hooks: H,
    ) -> Rig {
        let abort = Arc::new(RecordingAbort::default());
        Rig::start_on(helper, digest, ahead, hooks, GlobalFailStop::new(), abort)
    }

    /// [`Self::start_with`] on the node latch `latch` and the abort sink `abort`, which a case
    /// may also give its IS engine.
    fn start_on<H: EvoHooks<Facade = ()> + Send + 'static>(
        helper: Box<dyn ChainHelper>,
        digest: Option<Box<dyn DigestSink>>,
        ahead: i64,
        hooks: H,
        latch: GlobalFailStop,
        abort: Arc<RecordingAbort>,
    ) -> Rig {
        Rig::start_is(helper, digest, ahead, hooks, latch, abort, Box::new(NoInstantSend))
    }

    /// [`Self::start_on`] with `is` as V's InstantSend half of `ResolveBlockConflicts`.
    fn start_is<H: EvoHooks<Facade = ()> + Send + 'static>(
        helper: Box<dyn ChainHelper>,
        digest: Option<Box<dyn DigestSink>>,
        ahead: i64,
        hooks: H,
        latch: GlobalFailStop,
        abort: Arc<RecordingAbort>,
        is: Box<dyn IsConflicts>,
    ) -> Rig {
        let p = params();
        let dir = tempfile::tempdir().expect("a temporary directory");
        let blocks_dir = dir.path().join("blocks");
        std::fs::DirBuilder::new().mode(0o700).create(&blocks_dir).expect("the blocks directory");
        let config = BlockFilesConfig::new(blocks_dir, p.message_start);
        let files = BlockFiles::with_fs(Arc::new(OsFs::new()), config, BlockFilesState::default());
        let files = Arc::new(GuardedFiles::new(files, latch.clone()));
        let pow = pow_of(&p);
        let genesis = genesis_block(&p);
        let clock = Arc::new(MockClock::at(i64::from(genesis.header().raw().time()) + ahead));
        let path = || AbortPath::new(abort.clone(), latch.clone());
        let svc = Services {
            clock: clock.clone(),
            sporks: Arc::new(StaticSporks::for_network(&p, Some(false))),
            fail: Arc::new(NoReads(abort.clone())),
        };
        let mut block_files = NodeBlockFiles::new(files.clone(), pow, Arc::new(path()));
        let checks = ChainConfig {
            consistency_checks: false,
            ..ChainConfig::node(&p)
        };
        let (chain, idx): (HeaderChain, NodeBlockIndex) =
            load_genesis_block(p.clone(), checks, svc, &mut block_files).expect("genesis loads");
        let engine = MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let engine = Arc::new(GuardedEngine::new(engine, latch.clone(), abort.clone()));
        let budget = CoinsBudget::new(1 << 30, 0, Ratio::HALF, 0);
        let cache = CoinsCache::new(engine, budget, Salt::fixed(7), None);
        let sizes = CacheSizes::default();
        let caches = Arc::new(ScriptCaches {
            sig: SigCache::with_nonce(sizes.signature_cache_bytes, [0x5a; 32]).expect("sigcache"),
            exec: ScriptExecCache::with_nonce(sizes.script_execution_cache_bytes, [0xa5; 32])
                .expect("script execution cache"),
        });
        let parts = ConnectorParts {
            coins: Arc::new(StdMutex::new(cache)),
            files: files.clone(),
            queue: Arc::new(CheckQueue::serial()),
            caches,
            helper,
            abort: path(),
            assume_valid: None,
            digest,
            xcheck: false,
        };
        let connector = CoreConnector::new(&p, parts, hooks, Arc::new(())).expect("the connector");
        let signals = ValidationSignals::default();
        let signals_thread = signals.start(abort.clone()).expect("the signals thread");
        let (v, join) = spawn(VParts {
            params: p.clone(),
            chain,
            idx,
            checks: Box::new(CoreChecks::new(p.clone()).expect("regtest's checks")),
            files: Box::new(NodeBlockFiles::new(files, pow, Arc::new(path()))),
            read_ahead: ReadAheadSetup::off(),
            connector: Box::new(connector),
            facade: None,
            mempool: Box::new(NullMempool),
            flush: Box::new(NoFlush),
            is,
            ui: Arc::new(NoUi),
            abort: abort.clone(),
            shutdown: Arc::new(LocalShutdown::default()),
            signals: signals.clone(),
            clock: clock.clone(),
            check_block_index: false,
            have_pruned: false,
            stop_at_height: 0,
            reindex: false,
            guard: GuardScope::of(&p),
        })
        .expect("V starts");
        v.activate_best_chain().recv().expect("V").expect("the startup ABC");
        Rig {
            latch,
            abort,
            clock,
            ahead,
            v,
            join,
            signals: signals_thread,
            _dir: dir,
        }
    }

    /// `block` processed: `false` once V has stopped. A block the ABC rejects is still accepted
    /// by `ProcessNewBlock`; [`Self::tip`] shows whether it connected.
    fn submit(&self, block: &Arc<Block>) -> bool {
        self.clock.set_mocktime(i64::from(block.header().raw().time()) + self.ahead);
        self.v.process_block(block.clone(), true, None, true).recv().is_ok()
    }

    /// The active tip V last published.
    fn tip(&self) -> BlockHash {
        self.v.view().snap.tip.tip_hash
    }

    /// Each block processed and connected as the tip.
    fn connect(&self, blocks: &[Arc<Block>]) {
        for block in blocks {
            assert!(self.submit(block), "V stopped at {}", block.hash());
            assert_eq!(self.tip(), block.hash(), "{} is the tip", block.hash());
        }
    }

    /// V stopped: its exit, the tip's hash and the abort reports.
    fn stop(self) -> (VExit, BlockHash, Vec<String>) {
        let latch = self.latch.clone();
        let stopped = self.stop_any();
        assert!(latch.tripped().is_none(), "the stores' latch: {:?}", latch.tripped());
        stopped
    }

    /// V panicked: joined, its abort reports.
    fn stop_panicked(self) -> Vec<String> {
        self.v.shutdown();
        let joined = self.join.join().expect("an unmarked joiner");
        assert!(joined.is_err(), "V panicked");
        let _ = self.signals.stop();
        self.abort.messages()
    }

    /// [`Self::stop`] whether or not the node latch has tripped.
    fn stop_any(self) -> (VExit, BlockHash, Vec<String>) {
        self.v.shutdown();
        let exit = self.join.join().expect("an unmarked joiner").expect("V does not panic");
        let _ = self.signals.stop();
        let (tip, _) = exit.chain.private_tip().expect("a tip");
        let tip = exit.chain.reader().entry(tip.id).expect("the tip's entry").hash;
        (exit, tip, self.abort.messages())
    }
}

fn live_node(store: Arc<dyn IsStore>, spork2: bool) -> Box<dyn ChainHelper> {
    Box::new(IsHelper::live(NodeHelper, store, ledger(), gate(spork2), IsTipCache::new()))
}

/// A recording decorator: every IS-relevant call in order, and optionally a ChainLock on every
/// block (`HasChainLock`), which V without LK1a never answers.
struct Probe {
    inner: Box<dyn ChainHelper>,
    events: Arc<StdMutex<Vec<String>>>,
    chainlocked: bool,
    /// Runs on V after each removal returns.
    after_remove: Option<Box<dyn FnMut() + Send>>,
    /// The height whose attempt G2 refuses.
    refuse_at: Option<u32>,
}

/// The capability [`Probe::refuse_at`]'s G2 refusal names.
const G2_MISSING: &str = "a capability the case withholds";

impl Probe {
    fn new(inner: Box<dyn ChainHelper>, chainlocked: bool) -> Probe {
        Probe {
            inner,
            events: Arc::default(),
            chainlocked,
            after_remove: None,
            refuse_at: None,
        }
    }

    fn after_remove(mut self, f: impl FnMut() + Send + 'static) -> Probe {
        self.after_remove = Some(Box::new(f));
        self
    }

    fn refuse_at(mut self, h: u32) -> Probe {
        self.refuse_at = Some(h);
        self
    }

    fn note(&self, event: String) {
        self.events.lock().unwrap().push(event);
    }
}

impl ChainHelper for Probe {
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.inner.cl_conflict(cx, h, hash)
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        self.inner.ibd(cx)
    }

    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        let inner = self.inner.cl_has(cx, h, hash)?;
        Ok(inner || self.chainlocked)
    }

    fn cl_best(&mut self, cx: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        self.inner.cl_best(cx)
    }

    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        self.note(format!("is_conflict {}", tx.txid()));
        self.inner.is_conflict(tx)
    }

    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        self.note("remove".into());
        self.inner.remove_conflicting_is_lock(lock, sink);
        if let Some(f) = self.after_remove.as_mut() {
            f();
        }
    }

    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        self.note(format!("begin {}", at.hash));
        self.inner.begin_attempt(cx, at)
    }

    fn end_attempt(&mut self) {
        self.note("end".into());
        self.inner.end_attempt();
    }

    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        self.inner.sb_required(cx, at)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.inner.gov_valid()
    }

    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
        self.inner.sb_triggered(h)
    }

    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<PrimTxOut>>, Fatal> {
        self.inner.sb_payments(h)
    }

    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        if self.refuse_at == Some(h) {
            return Some(G2_MISSING);
        }
        self.inner.interim_refusal(cx, h)
    }
}

/// Only the spend asks: coinbases have no lock inputs (`has_lock_inputs`), and a lock-free
/// spend connects.
#[test]
fn connector_a_spend_without_a_conflict_connects_after_one_query() {
    let m = Matured::new();
    let spend = m.spend();
    let store = Arc::new(InMemoryIsStore::default());
    let rig = Rig::start(live_node(store.clone(), true), None, 0);
    rig.connect(&m.chain[1..]);
    let b = m.next(std::slice::from_ref(&spend));
    rig.connect(std::slice::from_ref(&b));
    let (exit, tip, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert_eq!(tip, b.hash());
    assert!(reports.is_empty(), "{reports:?}");
    let queries: Vec<_> =
        store.calls().into_iter().filter(|c| !matches!(c, IsStoreCall::DiscardStaged)).collect();
    assert_eq!(queries, [IsStoreCall::ConflictingLockIn(spend.txid())]);
}

/// Core validation.cpp:2810-2829: no ChainLock on the block, so the conflict rejects it.
#[test]
fn connector_a_conflict_without_a_chainlock_is_conflict_tx_lock() {
    let m = Matured::new();
    let spend = m.spend();
    let (store, _, lock) = conflicting_store(&spend);
    capture::install();
    let rig = Rig::start(live_node(Arc::new(store), true), None, 0);
    rig.connect(&m.chain[1..]);
    assert!(rig.submit(&m.next(std::slice::from_ref(&spend))));
    assert_eq!(rig.tip(), m.tip().hash());
    assert_eq!(capture::lines_with(&conflict_line(spend.txid(), lock.txid())).len(), 1);
    let (exit, tip, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert_eq!(tip, m.tip().hash());
    assert!(reports.is_empty(), "{reports:?}");
}

/// Runs `f` on a thread of its own and fails the test unless it returns within `bound` (DEC-1631).
fn within<T: Send + 'static>(bound: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(bound) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => panic!("the case did not end within {bound:?}"),
        Err(RecvTimeoutError::Disconnected) => panic!("the case panicked (see above)"),
    }
}

/// The lock a ChainLocked block's `tx` conflicts with: `tx`'s lock inputs, another txid.
fn conflict_lock(tx: &Tx) -> LockedTx {
    lock_for(TxHash::from([0x4c; 32]), lock_inputs(tx))
}

/// The IS store and ledger of ChainLocked conflicts' removals, over `E`.
struct Removing<E: KvEngine + 'static = MemoryEngine> {
    engine: Arc<E>,
    db: Arc<IsDb<E>>,
    ledger: SharedLedger,
    /// The written locks' hashes, in order.
    hashes: Vec<IsLockHash>,
}

impl Removing {
    /// One lock covering `tx`'s lock inputs, for another transaction.
    fn new(tx: &Tx) -> Removing {
        Removing::over(Arc::new(MemoryEngine::new(&SCHEMA)), &[conflict_lock(tx)])
    }
}

impl<E: KvEngine + 'static> Removing<E> {
    /// `locks` written to a store opened over `engine`.
    fn over(engine: Arc<E>, locks: &[LockedTx]) -> Removing<E> {
        let db = Arc::new(IsDb::open(engine.clone()).unwrap());
        let hashes = locks
            .iter()
            .map(|lock| {
                let hash = isdlock_hash(lock);
                db.write_new(&hash, lock).unwrap();
                hash
            })
            .collect();
        Removing {
            engine,
            db,
            ledger: ledger(),
            hashes,
        }
    }

    /// The live helper over the store, with a ChainLock on every block.
    fn probe(&self) -> Probe {
        let helper = IsHelper::live(
            NodeHelper,
            self.db.clone(),
            self.ledger.clone(),
            gate(true),
            IsTipCache::new(),
        );
        Probe::new(Box::new(helper), true)
    }

    /// The appliers of each attempt's log, the IS removal through [`IsLedgerApplier`].
    fn appliers(&self) -> EffectAppliers {
        appliers(self.db.clone(), self.ledger.clone())
    }

    fn evo(&self) -> ScriptedEvo {
        ScriptedEvo::new(self.appliers())
    }

    /// A digest over `buf` whose `fx` is the ledger's [`IsFxSource`].
    fn digest(&self, buf: &Buf) -> Box<dyn DigestSink> {
        let log = DigestLog::without_golden(buf.clone(), digest_header(FileKind::Digest)).unwrap();
        Box::new(log.with_fx(Box::new(IsFxSource(self.ledger.clone()))))
    }

    /// The store reopened from its engine: the rows the removals committed.
    fn reopened(self) -> IsDb<E> {
        drop(self.db);
        IsDb::open(self.engine).unwrap()
    }
}

/// Every outcome record after the `H`: its kind and its `fx`, parsed back.
fn outcomes(buf: &Buf) -> Vec<(Kind, Option<Vec<FxEffect>>)> {
    Lines::new(&buf.bytes())
        .filter_map(|(_, r)| match r.expect("a record") {
            Record::Outcome(o) => {
                let fx = o.o().get("fx").map(|v| FxEffect::list_from_value(v).expect("fx"));
                Some((o.kind(), fx))
            }
            _ => None,
        })
        .collect()
}

/// O8's `is_rm` entry for `hash` overridden by `overriding`.
fn is_rm(hash: IsLockHash, overriding: TxHash) -> FxEffect {
    FxEffect::IsRm {
        islock: DigestHash::from_wire_bytes(hash),
        txid: DigestHash::from_wire_bytes(*overriding.as_bytes()),
    }
}

/// A ChainLocked conflict's lock is removed and the block connects (validation.cpp:2810-2829):
/// the IS loop asks again after the removal and finds nothing, the applier commits the stage at
/// the effect's place in the finish, and the reopened store has the lock removed and archived.
#[test]
fn connector_a_chainlocked_conflict_removes_the_lock_and_connects() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let probe = is.probe();
        let events = probe.events.clone();
        let evo = is.evo();
        let finishes = evo.handle();
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(probe), Some(is.digest(&buf)), 0, evo);
        rig.connect(&m.chain[1..]);
        let b = m.next(std::slice::from_ref(&spend));
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        let asked = format!("is_conflict {}", spend.txid());
        let events = events.lock().unwrap().clone();
        let tail: Vec<_> = events.iter().skip_while(|e| **e != asked).cloned().collect();
        assert_eq!(tail, [asked.clone(), "remove".into(), asked, "end".into()]);
        let last = finishes.reports().pop().expect("the conflict's finish");
        assert_eq!(last.result, Ok(()));
        assert_eq!(
            last.effects,
            EffectCoverage::Known {
                emitted: vec![removal(hash)],
                completed: 1,
            }
        );
        let record = outcomes(&buf).pop().expect("the conflict's record");
        assert_eq!(record, (Kind::Connect, Some(vec![is_rm(hash, spend.txid())])));
        assert_eq!(is.ledger.lock().take(), Default::default(), "the fx source took the ledger");
        let reopened = is.reopened();
        assert_eq!(reopened.by_hash(&hash, false).unwrap(), None);
        assert!(reopened.known(&hash).unwrap(), "archived");
    });
}

/// DEC-1631 through the connector: with `L` removed as confirmed (Core
/// `instantsend/db.cpp:118-164`), a block spending one of `L`'s inputs connects, no cache entry
/// names `L` afterwards, the removal observed no stored root, and `L` stays archived.
#[test]
fn connector_dec1631_a_confirmed_lock_s_cache_entries_go_with_its_removal() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let lock = is.db.by_hash(&hash, true).unwrap().expect("L");
        is.db.write_mined(&hash, 1).unwrap();
        assert_eq!(is.db.remove_confirmed(1).unwrap().len(), 1);
        let before = is.db.cache_snapshot();
        assert!(before.txids.contains(&(lock.txid(), hash)), "{before:?}");
        assert!(lock.inputs().iter().all(|i| before.inputs.contains(&(*i, hash))), "{before:?}");
        let rig = Rig::start_with(Box::new(is.probe()), None, 0, is.evo());
        rig.connect(&m.chain[1..]);
        let b = m.next(std::slice::from_ref(&spend));
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        let after = is.db.cache_snapshot();
        assert!(!after.locks.iter().any(|(k, _)| *k == hash), "{after:?}");
        assert!(!after.txids.iter().any(|(_, v)| *v == hash), "{after:?}");
        assert!(!after.inputs.iter().any(|(_, v)| *v == hash), "{after:?}");
        let taken = is.ledger.lock().take();
        assert_eq!(taken.confirmed, [hash]);
        assert!(!taken.observations[0].root_stored);
        assert!(is.reopened().known(&hash).unwrap(), "archived");
    });
}

/// A removal survives its block's later rejection (A1 §2.10.1): the coinbase overpays, so the
/// block fails after the IS loop (`bad-cb-amount`, validation.cpp:2846-2850), the finish still
/// commits the removal, and the `R` keeps its `is_rm` (an `Ok(Invalid)` outcome, §5.3 item 7).
#[test]
fn connector_a_removal_survives_its_block_s_later_rejection() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(is.probe()), Some(is.digest(&buf)), 0, is.evo());
        rig.connect(&m.chain[1..]);
        // Its own overpay: the shared log keeps every case's `bad-cb-amount` line.
        let overpaid = OVERPAID + 1;
        let b =
            block_on(&m.pow, m.tip(), m.next_height(), 0, overpaid, std::slice::from_ref(&spend));
        capture::install();
        assert!(rig.submit(&b));
        let rejected =
            format!("coinbase pays too much at height {} (actual={overpaid}", m.next_height());
        assert!(!capture::lines_with(&rejected).is_empty(), "rejected for its coinbase");
        assert_eq!(rig.tip(), m.tip().hash());
        let (exit, _, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert!(reports.is_empty(), "{reports:?}");
        let record = outcomes(&buf).pop().expect("the rejected block's record");
        assert_eq!(record, (Kind::Reject, Some(vec![is_rm(hash, spend.txid())])));
        let reopened = is.reopened();
        assert_eq!(reopened.by_hash(&hash, false).unwrap(), None);
        assert!(reopened.known(&hash).unwrap(), "archived");
    });
}

/// `fJustCheck` attempts remove too (A1 §2.10.1): `TestBlockValidity` and a GBT proposal of a
/// ChainLocked conflicting block leave the tip where it was and the lock removed and archived.
#[test]
fn connector_a_just_check_or_proposal_attempt_removes_the_lock() {
    within(Duration::from_secs(300), || {
        for proposal in [false, true] {
            let m = Matured::new();
            let spend = m.spend();
            let is = Removing::new(&spend);
            let hash = is.hashes[0];
            let rig = Rig::start_with(Box::new(is.probe()), None, 0, is.evo());
            rig.connect(&m.chain[1..]);
            let b = m.next(std::slice::from_ref(&spend));
            let tested = if proposal {
                rig.v.propose_block(b)
            } else {
                rig.v.test_block_validity(b, false, true)
            };
            let state = tested.recv().expect("V answers").expect("tested on the tip");
            assert!(state.is_valid(), "{proposal}: {state:?}");
            assert_eq!(rig.tip(), m.tip().hash());
            let (exit, _, reports) = rig.stop();
            assert!(exit.fatal.is_none(), "{proposal}: {:?}", exit.fatal);
            assert!(reports.is_empty(), "{proposal}: {reports:?}");
            assert_eq!(is.ledger.lock().take().confirmed, [hash], "{proposal}");
            let reopened = is.reopened();
            assert_eq!(reopened.by_hash(&hash, false).unwrap(), None, "{proposal}");
            assert!(reopened.known(&hash).unwrap(), "{proposal}: archived");
        }
    });
}

/// Two spends of the coinbases at heights 1 and 2, each conflicting with its own lock, and the
/// store holding both locks.
fn two_conflicts(m: &Matured) -> ([Arc<Tx>; 2], Removing) {
    let spends = [tx_spending(m.coin_at(1), 40_000), tx_spending(m.coin_at(2), 40_000)];
    let locks = [
        lock_for(TxHash::from([0x4c; 32]), lock_inputs(&spends[0])),
        lock_for(TxHash::from([0x4d; 32]), lock_inputs(&spends[1])),
    ];
    (spends, Removing::over(Arc::new(MemoryEngine::new(&SCHEMA)), &locks))
}

/// R2-2 (PLAN-LK1b §6 S1b's ledger lifecycle, end to end to the DigestLog): two ChainLocked
/// conflicts in one block, each removing its own lock, render two `is_rm` in emission order.
#[test]
fn connector_several_removals_render_is_rm_in_emission_order() {
    within(Duration::from_secs(300), || {
        let m = Matured::deeper(1);
        let (spends, is) = two_conflicts(&m);
        let evo = is.evo();
        let finishes = evo.handle();
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(is.probe()), Some(is.digest(&buf)), 0, evo);
        rig.connect(&m.chain[1..]);
        let b = m.next(&spends);
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        let [l1, l2] = [is.hashes[0], is.hashes[1]];
        assert_eq!(
            finishes.reports().pop().expect("the block's finish").effects,
            EffectCoverage::Known {
                emitted: vec![removal(l1), removal(l2)],
                completed: 2,
            }
        );
        let want = vec![is_rm(l1, spends[0].txid()), is_rm(l2, spends[1].txid())];
        assert_eq!(outcomes(&buf).pop(), Some((Kind::Connect, Some(want))));
    });
}

/// R2-2: a removal whose root is no longer stored renders an empty `fx`, `[]`, on the connect
/// record (O8 §6; DEC-1631).
#[test]
fn connector_a_root_absent_removal_renders_an_empty_fx() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        is.db.write_mined(&hash, 1).unwrap();
        assert_eq!(is.db.remove_confirmed(1).unwrap().len(), 1);
        let evo = is.evo();
        let finishes = evo.handle();
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(is.probe()), Some(is.digest(&buf)), 0, evo);
        rig.connect(&m.chain[1..]);
        let b = m.next(std::slice::from_ref(&spend));
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(
            finishes.reports().pop().expect("the block's finish").effects,
            EffectCoverage::Known {
                emitted: vec![removal(hash)],
                completed: 1,
            }
        );
        assert_eq!(outcomes(&buf).pop(), Some((Kind::Connect, Some(vec![]))));
        assert_eq!(is.ledger.lock().take(), Default::default(), "the fx source took the ledger");
    });
}

/// R2-2: a valid proposal writes no record, though the `fx` source takes its removal's
/// observation, so the next record renders only that attempt's own removal. The proposal's
/// removal is committed all the same.
#[test]
fn connector_an_unrecorded_removal_is_not_rendered_by_the_next_record() {
    within(Duration::from_secs(300), || {
        let m = Matured::deeper(1);
        let (spends, is) = two_conflicts(&m);
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(is.probe()), Some(is.digest(&buf)), 0, is.evo());
        rig.connect(&m.chain[1..]);
        let proposed = m.next(&spends[..1]);
        let state = rig.v.propose_block(proposed).recv().expect("V answers").expect("on the tip");
        assert!(state.is_valid(), "{state:?}");
        let recorded = outcomes(&buf).len();
        let b = m.next(&spends[1..]);
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        let out = outcomes(&buf);
        assert_eq!(out.len(), recorded + 1, "the proposal wrote no record: {out:?}");
        let want = vec![is_rm(is.hashes[1], spends[1].txid())];
        assert_eq!(out.last(), Some(&(Kind::Connect, Some(want))));
        let removed = is.hashes[0];
        let reopened = is.reopened();
        assert_eq!(reopened.by_hash(&removed, false).unwrap(), None);
        assert!(reopened.known(&removed).unwrap(), "archived");
    });
}

/// With no `fx` source (rdashd's own digest log, CP Q9(a)) nothing takes the ledger: each
/// attempt's `begin_attempt` empties it, so after the proposal and the next connect it holds only
/// that connect's observation, confirmed, and not one per removal over the node's life.
#[test]
fn without_an_fx_source_each_attempt_starts_from_an_empty_ledger() {
    within(Duration::from_secs(300), || {
        let m = Matured::deeper(1);
        let (spends, is) = two_conflicts(&m);
        let rig = Rig::start_with(Box::new(is.probe()), None, 0, is.evo());
        rig.connect(&m.chain[1..]);
        let proposed = m.next(&spends[..1]);
        let state = rig.v.propose_block(proposed).recv().expect("V answers").expect("on the tip");
        assert!(state.is_valid(), "{state:?}");
        let b = m.next(&spends[1..]);
        rig.connect(std::slice::from_ref(&b));
        let (exit, tip, reports) = rig.stop();
        assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
        assert_eq!(tip, b.hash());
        assert!(reports.is_empty(), "{reports:?}");
        let left = is.ledger.lock().take();
        let hashes: Vec<_> = left.observations.iter().map(|o| o.lock_hash).collect();
        assert_eq!(hashes, [is.hashes[1]], "only the last attempt's observation");
        assert_eq!(left.confirmed, [is.hashes[1]]);
        assert_eq!(left.failed_at, None);
    });
}

/// The snapshot applier of counterexample S (§5.3), failing or not.
struct Snapshots {
    fail: bool,
}

/// The failing [`Snapshots`]' text.
const SNAPSHOT_FAILED: &str = "snapshot write failed";

impl SnapshotApplier for Snapshots {
    fn write(&mut self, _: LlmqType, _: &BlockHash, _: &QuorumSnapshot) -> Result<(), Fatal> {
        if self.fail {
            Err(Fatal::new(SNAPSHOT_FAILED))
        } else {
            Ok(())
        }
    }
}

fn snapshot_effect() -> Effect {
    Effect::StoreQuorumSnapshot {
        llmq_type: LlmqType::LlmqTestDip0024,
        cycle_base: BlockHash::from([1; 32]),
        snapshot: QuorumSnapshot {
            active_quorum_members: vec![true],
            skip_list_mode: 0,
            skip_list: vec![],
        },
    }
}

/// Counterexample S at the applier level (§6 S1b, mixed-effect failure): the log
/// `[StoreQuorumSnapshot, RemoveConflictingIsLock]` through `EffectAppliers::apply_log`. A
/// failing snapshot applier stops it at the first effect, `end_attempt` discards the stage, and no IS row
/// or archive changed; with the snapshot applier succeeding, the removal commits.
#[test]
fn an_earlier_failing_applier_leaves_no_is_removal_and_its_twin_commits() {
    for fail in [true, false] {
        let tx = spend_of(&[prevout(1, 0)]);
        let lock = lock_for(TxHash::from([0x4c; 32]), lock_inputs(&tx));
        let is = Removing::over(Arc::new(MemoryEngine::new(&SCHEMA)), std::slice::from_ref(&lock));
        let hash = is.hashes[0];
        let before = rows(&is.engine);
        let mut helper = IsHelper::live(
            Scripted::default(),
            is.db.clone(),
            is.ledger.clone(),
            gate(true),
            IsTipCache::new(),
        );
        let found = helper.is_conflict(&tx).unwrap().expect("the conflict");
        let mut log = vec![snapshot_effect()];
        helper.remove_conflicting_is_lock(&found, &mut log);
        assert_eq!(log, [snapshot_effect(), removal(hash)]);
        let mut appliers = is.appliers();
        appliers.snapshot = Box::new(Snapshots {
            fail,
        });
        let applied = appliers.apply_log(AttemptId::next(), &log).map(|_| ());
        helper.end_attempt();
        if fail {
            assert_eq!(applied, Err(Fatal::new(SNAPSHOT_FAILED)));
            assert!(is.db.by_hash(&hash, false).is_err(), "the discard latched the store");
            assert_eq!(rows(&is.engine), before);
            assert_eq!(is.reopened().by_hash(&hash, false).unwrap(), Some(lock));
        } else {
            applied.expect("the log applies");
            let reopened = is.reopened();
            assert_eq!(reopened.by_hash(&hash, false).unwrap(), None);
            assert!(reopened.known(&hash).unwrap(), "archived");
        }
    }
}

/// Waits, on V, until a caller is parked on `db`'s gate.
fn await_parked<E: KvEngine>(db: &IsDb<E>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while db.gate_waiters() == 0 {
        assert!(std::time::Instant::now() < deadline, "no reader parked on the gate");
        std::thread::yield_now();
    }
}

/// A reader thread's lookup of `hash`, started on V after the removal staged.
type Parked = Arc<StdMutex<Option<std::thread::JoinHandle<dash_kv::Result<Option<LockedTx>>>>>>;

/// Counterexample S through the connector (§6 S1b, mixed-effect failure): `ScriptedEvo`'s H4
/// emits a `StoreQuorumSnapshot` before the IS loop, and its applier fails. The finish fails at
/// index 0 (`completed = 0`), `end_attempt` discards the stage and latches the store, so a reader
/// that parked on the gate after the stage gets the latched error. No IS row or archive changed,
/// and the attempt's record has no `fx`.
#[test]
fn connector_a_failing_earlier_applier_discards_the_removal() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let before = rows(&is.engine);
        let parked = Parked::default();
        let probe = is.probe().after_remove({
            let (db, parked) = (is.db.clone(), parked.clone());
            move || {
                let reader = db.clone();
                *parked.lock().unwrap() =
                    Some(std::thread::spawn(move || reader.by_hash(&hash, true)));
                await_parked(&db);
            }
        });
        let mut appliers = is.appliers();
        appliers.snapshot = Box::new(Snapshots {
            fail: true,
        });
        let evo = ScriptedEvo::new(appliers);
        let script = evo.handle();
        let buf = Buf::default();
        let rig = Rig::start_with(Box::new(probe), Some(is.digest(&buf)), 0, evo);
        rig.connect(&m.chain[1..]);
        script.push_h4(vec![snapshot_effect()]);
        let _ = rig.submit(&m.next(std::slice::from_ref(&spend)));
        let (exit, tip, reports) = rig.stop_any();
        assert_eq!(exit.fatal.map(|f| f.0), Some(SNAPSHOT_FAILED.to_owned()));
        assert_eq!(tip, m.tip().hash());
        assert_eq!(reports.len(), 1, "{reports:?}");
        let last = script.reports().pop().expect("the conflict's finish");
        assert_eq!(last.result, Err(Fatal::new(SNAPSHOT_FAILED)));
        assert_eq!(
            last.effects,
            EffectCoverage::Known {
                emitted: vec![snapshot_effect(), removal(hash)],
                completed: 0,
            }
        );
        let reader = parked.lock().unwrap().take().expect("a reader parked");
        let latched = reader.join().unwrap().expect_err("the parked reader gets the latch");
        assert!(latched.to_string().contains("staged removal discarded"), "{latched}");
        assert!(is.db.by_hash(&hash, false).is_err());
        assert!(outcomes(&buf).last().is_some_and(|(_, fx)| fx.is_none()));
        assert_eq!(rows(&is.engine), before);
        assert!(is.reopened().by_hash(&hash, false).unwrap().is_some(), "L stays stored");
    });
}

/// The panic twin through the connector (§6 S1a-2 panic case, S1b's twin; r2f G1): V panics
/// right after the IS loop staged a removal, with a reader parked on the gate. Only V holds the
/// attempt's ledger (G1-b), so as V unwinds the overlay's gate hold drops unreleased: it latches
/// the store and wakes the reader with the latched error. Later calls get it too, and after a
/// reopen every row is as before the stage.
#[test]
fn connector_a_panic_after_a_staged_removal_latches_and_wakes_a_parked_reader() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let before = rows(&is.engine);
        let parked = Parked::default();
        let probe = is.probe().after_remove({
            let (db, parked) = (is.db.clone(), parked.clone());
            move || {
                let reader = db.clone();
                *parked.lock().unwrap() =
                    Some(std::thread::spawn(move || reader.by_hash(&hash, true)));
                await_parked(&db);
                panic!("injected after the IS loop staged a removal");
            }
        });
        let evo = is.evo();
        let Removing {
            engine,
            db,
            ledger,
            ..
        } = is;
        drop(ledger);
        let rig = Rig::start_with(Box::new(probe), None, 0, evo);
        rig.connect(&m.chain[1..]);
        let _ = rig.submit(&m.next(std::slice::from_ref(&spend)));
        let reports = rig.stop_panicked();
        let unwound = "Error reading from database: a tip hold unwound inside the tip lock";
        let panicked = "V panicked: injected after the IS loop staged a removal";
        assert_eq!(reports, [unwound, panicked], "the hold's read failure and V's panic");
        let reader = parked.lock().unwrap().take().expect("a reader parked");
        let latched = reader.join().unwrap().expect_err("the parked reader gets the latch");
        assert!(latched.to_string().contains("ended with its stages pending"), "{latched}");
        assert!(db.by_hash(&hash, false).is_err());
        assert!(db.write_new(&hash, &conflict_lock(&spend)).is_err());
        assert_eq!(rows(&engine), before);
        drop(db);
        let reopened = IsDb::open(engine).unwrap();
        assert_eq!(reopened.by_hash(&hash, false).unwrap(), Some(conflict_lock(&spend)));
    });
}

/// "Exactly one report" at the connector (§5.3 item 6, r2d): V stops with the injected fault's
/// text, the finish is `Ok`, one abort report names the fault, and no `finish_attempt:` line or
/// `is::chain` line but Core's `RemoveConflictingLock` lines was logged.
fn one_report(exit: &VExit, reports: &[String], finish: &FinishReport) {
    let fault = &exit.fatal.as_ref().expect("V stops on the IS fault").0;
    assert!(fault.contains(READ_FAULT), "{fault}");
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].contains(READ_FAULT), "{reports:?}");
    assert_eq!(finish.result, Ok(()));
    assert_eq!(capture::lines_with(&format!("finish_attempt: {fault}")), Vec::<String>::new());
    let own = capture::lines(|target, line| {
        target.starts_with("dashd_locks::is::chain")
            && !line.starts_with("CInstantSendManager::RemoveConflictingLock")
    });
    assert_eq!(own, Vec::<String>::new());
}

/// The probe's events from `tx`'s first `is_conflict`: Core's IS loop asking, removing, asking
/// again, and nothing after the attempt's end.
fn asked_removed_asked(events: &StdMutex<Vec<String>>, tx: &Tx) -> Vec<String> {
    let asked = format!("is_conflict {}", tx.txid());
    let events = events.lock().unwrap();
    let tail: Vec<_> = events.iter().skip_while(|e| **e != asked).cloned().collect();
    assert_eq!(tail, [asked.clone(), "remove".into(), asked, "end".into()]);
    tail
}

/// The second-lookup fault through the connector (§5.3 item 2, r2c/r2d): the spend's first lock
/// input is unlocked, so both lookups read its `is_in` row (a miss is not cached,
/// `instantsend/db.cpp:303-314`), and the second read fails. Nothing is staged, observed or
/// emitted; the store is neither latched nor gated after the attempt; exactly one report.
#[test]
fn connector_a_second_lookup_fault_stops_v_with_one_report_and_no_removal() {
    within(Duration::from_secs(300), || {
        let m = Matured::deeper(1);
        let (unlocked, locked) = (m.coin_at(1), m.coin_at(2));
        let tx = tx_spending_each(&[unlocked, locked], 40_000);
        let lock = lock_for(TxHash::from([0x5a; 32]), vec![locked]);
        let engine = Arc::new(ReadFault::new());
        let is = Removing::over(engine.clone(), std::slice::from_ref(&lock));
        let hash = is.hashes[0];
        let evo = is.evo();
        let script = evo.handle();
        let probe = is.probe();
        let events = probe.events.clone();
        capture::install();
        let rig = Rig::start_with(Box::new(probe), None, 0, evo);
        rig.connect(&m.chain[1..]);
        engine.fail_input_read(&unlocked, 1);
        let _ = rig.submit(&m.next(std::slice::from_ref(&tx)));
        let (exit, tip, reports) = rig.stop_any();
        assert!(engine.fired());
        assert_eq!(tip, m.tip().hash());
        let finish = script.reports().pop().expect("the attempt's finish");
        one_report(&exit, &reports, &finish);
        assert_eq!(
            finish.effects,
            EffectCoverage::Known {
                emitted: vec![],
                completed: 0,
            }
        );
        asked_removed_asked(&events, &tx);
        let removing = format!("RemoveConflictingLock -- txid={}", lock.txid());
        assert_eq!(capture::lines_with(&removing), Vec::<String>::new());
        assert_eq!(is.ledger.lock().take(), Default::default());
        assert_eq!(is.db.by_hash(&hash, false).unwrap(), Some(lock));
    });
}

/// The staging read fault through the connector (§5.3 item 2, r2d): the chained walk's read of
/// the root's second child fails. Core's first `RemoveConflictingLock` line comes before it, as
/// Core logs it before `RemoveChainedInstantSendLocks` throws; nothing is staged, observed or
/// emitted, every row stays, and exactly one report.
#[test]
fn connector_a_staging_read_fault_stops_v_with_one_report_after_core_s_first_line() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let root = lock_for(TxHash::from([0x5b; 32]), lock_inputs(&spend));
        let children = [0, 1].map(|n| {
            lock_for(TxHash::from([0x5c + n as u8; 32]), vec![OutPoint::new(root.txid(), n)])
        });
        let engine = Arc::new(ReadFault::new());
        let locks = [root.clone(), children[0].clone(), children[1].clone()];
        let is = Removing::over(engine.clone(), &locks);
        let before = is.db.cache_snapshot();
        let evo = is.evo();
        let script = evo.handle();
        capture::install();
        let rig = Rig::start_with(Box::new(is.probe()), None, 0, evo);
        rig.connect(&m.chain[1..]);
        engine.fail_lock_read(&is.hashes[2], 0);
        let _ = rig.submit(&m.next(std::slice::from_ref(&spend)));
        let (exit, tip, reports) = rig.stop_any();
        assert!(engine.fired());
        assert_eq!(tip, m.tip().hash());
        let finish = script.reports().pop().expect("the attempt's finish");
        one_report(&exit, &reports, &finish);
        assert_eq!(
            finish.effects,
            EffectCoverage::Known {
                emitted: vec![],
                completed: 0,
            }
        );
        let removing = format!("RemoveConflictingLock -- txid={}", root.txid());
        let lines = capture::lines_with(&removing);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].ends_with("Removing ISLOCK and its chained children"), "{lines:?}");
        assert_eq!(is.ledger.lock().take(), Default::default());
        // Core's partial transitions stay: the first child's erase, nothing for the failed read.
        let after = is.db.cache_snapshot();
        assert_eq!(names(&after, is.hashes[1]), Default::default(), "{after:?}");
        assert_eq!(names(&after, is.hashes[2]), names(&before, is.hashes[2]), "{after:?}");
        for (hash, lock) in is.hashes.iter().zip(&locks) {
            assert_eq!(is.db.by_hash(hash, false).unwrap().as_ref(), Some(lock));
        }
    });
}

/// A fault after an earlier committed removal in the same attempt (§6 S1b, r2d): tx1's conflict
/// stages and emits `L1`, then tx2's second lookup fails. The body returns the fault and the
/// finish is `Ok`, committing `L1` (Core wrote it inline before its throw); `L2` stays; the
/// attempt's record has no `fx`, and the source consumed the one observation; one report.
#[test]
fn connector_a_fault_after_a_committed_removal_keeps_that_removal_and_renders_no_fx() {
    within(Duration::from_secs(300), || {
        let m = Matured::deeper(2);
        let (a, b, c) = (m.coin_at(1), m.coin_at(2), m.coin_at(3));
        let tx1 = tx_spending(a, 40_000);
        let tx2 = tx_spending_each(&[b, c], 40_000);
        let l1 = lock_for(TxHash::from([0x41; 32]), vec![a]);
        let l2 = lock_for(TxHash::from([0x42; 32]), vec![c]);
        let engine = Arc::new(ReadFault::new());
        let is = Removing::over(engine.clone(), &[l1, l2.clone()]);
        let (h1, h2) = (is.hashes[0], is.hashes[1]);
        let evo = is.evo();
        let script = evo.handle();
        let probe = is.probe();
        let events = probe.events.clone();
        let buf = Buf::default();
        capture::install();
        let rig = Rig::start_with(Box::new(probe), Some(is.digest(&buf)), 0, evo);
        rig.connect(&m.chain[1..]);
        engine.fail_input_read(&b, 1);
        let _ = rig.submit(&m.next(&[tx1, tx2.clone()]));
        let (exit, tip, reports) = rig.stop_any();
        assert!(engine.fired());
        assert_eq!(tip, m.tip().hash());
        let finish = script.reports().pop().expect("the attempt's finish");
        one_report(&exit, &reports, &finish);
        assert_eq!(
            finish.effects,
            EffectCoverage::Known {
                emitted: vec![removal(h1)],
                completed: 1,
            }
        );
        asked_removed_asked(&events, &tx2);
        assert!(outcomes(&buf).last().is_some_and(|(_, fx)| fx.is_none()));
        assert_eq!(is.ledger.lock().take(), Default::default(), "the fx source took it");
        assert_eq!(is.db.by_hash(&h1, false).unwrap(), None);
        let reopened = is.reopened();
        assert_eq!(reopened.by_hash(&h1, false).unwrap(), None);
        assert!(reopened.known(&h1).unwrap(), "L1 archived");
        assert_eq!(reopened.by_hash(&h2, false).unwrap(), Some(l2));
    });
}

/// G2 right after the IS loop staged a removal (PLAN-LK1b errata 11-15, S1f chain 2 minor 2;
/// §5.3 item 7, r2e F3-N3). On the tip path the refusal waits (DEC-1874): nothing reports it, so
/// the node latch stays untripped and the finish commits the stage on either IS engine. Which
/// engine `llmq/isdb` gets is S1d's wiring (`dashd-node/src/node.rs`). Returns the finish, the
/// refused attempt's `fx`, and whether every IS row stayed as it was.
fn g2_after_a_removal<E: KvEngine + 'static>(
    m: &Matured,
    spend: &Arc<Tx>,
    is: Removing<E>,
    latch: GlobalFailStop,
    abort: Arc<RecordingAbort>,
) -> (FinishReport, Option<Vec<FxEffect>>, bool) {
    let before = rows(&is.engine);
    let evo = is.evo();
    let script = evo.handle();
    let buf = Buf::default();
    let probe = is.probe().refuse_at(MATURITY + 1);
    let rig = Rig::start_on(Box::new(probe), Some(is.digest(&buf)), 0, evo, latch, abort);
    rig.connect(&m.chain[1..]);
    let _ = rig.submit(&m.next(std::slice::from_ref(spend)));
    let (_, tip, _) = rig.stop_any();
    assert_eq!(tip, m.tip().hash());
    let finish = script.reports().pop().expect("the refused attempt's finish");
    let (kind, fx) = outcomes(&buf).pop().expect("the refused attempt's record");
    assert_eq!(kind, Kind::Reject);
    (finish, fx, rows(&is.engine) == before)
}

/// [`g2_after_a_removal`] on an IS engine the node latch does not guard: the finish commits the
/// removal and the `R` carries its `is_rm`.
#[test]
fn connector_g2_after_a_removal_on_an_unguarded_is_engine_renders_is_rm() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let is = Removing::new(&spend);
        let hash = is.hashes[0];
        let abort = Arc::new(RecordingAbort::default());
        let (finish, fx, kept) = g2_after_a_removal(&m, &spend, is, GlobalFailStop::new(), abort);
        assert_eq!(finish.result, Ok(()));
        assert_eq!(
            finish.effects,
            EffectCoverage::Known {
                emitted: vec![removal(hash)],
                completed: 1,
            }
        );
        assert_eq!(fx, Some(vec![is_rm(hash, spend.txid())]));
        assert!(!kept, "the removal committed");
    });
}

/// [`g2_after_a_removal`] on a [`GuardedEngine`] on the node latch: G2's wait leaves the latch
/// untripped and reports nothing, so the finish commits the removal and the `R` carries its
/// `is_rm`, as on an unguarded engine.
#[test]
fn connector_g2_after_a_removal_on_the_node_latch_s_engine_renders_is_rm() {
    within(Duration::from_secs(300), || {
        let m = Matured::new();
        let spend = m.spend();
        let lock = lock_for(TxHash::from([0x4c; 32]), lock_inputs(&spend));
        let latch = GlobalFailStop::new();
        let abort = Arc::new(RecordingAbort::default());
        let engine = GuardedEngine::new(MemoryEngine::new(&SCHEMA), latch.clone(), abort.clone());
        let is = Removing::over(Arc::new(engine), std::slice::from_ref(&lock));
        let hash = is.hashes[0];
        let (finish, fx, kept) = g2_after_a_removal(&m, &spend, is, latch.clone(), abort.clone());
        assert!(latch.tripped().is_none());
        assert!(abort.messages().is_empty(), "{:?}", abort.messages());
        assert_eq!(finish.result, Ok(()));
        assert_eq!(fx, Some(vec![is_rm(hash, spend.txid())]));
        assert!(!kept, "the removal committed");
    });
}

/// The two skips connect a conflicting spend without one IS read: V in IBD
/// (`IsInitialBlockDownload()`, validation.cpp:2801), or SPORK_2 off (`GetConflictingLock`
/// answers null first, instantsend.cpp:430-432).
fn connects_without_a_query(spork2: bool, ahead: i64) {
    let m = Matured::new();
    let spend = m.spend();
    let (store, _, _) = conflicting_store(&spend);
    let store = Arc::new(store);
    let rig = Rig::start(live_node(store.clone(), spork2), None, ahead);
    rig.connect(&m.chain[1..]);
    rig.connect(&[m.next(&[spend])]);
    let _ = rig.stop();
    assert!(!store.calls().iter().any(|c| matches!(c, IsStoreCall::ConflictingLockIn(_))));
}

#[test]
fn connector_ibd_skips_the_conflict_check() {
    // A clock a year past every block: the tip is always too old to leave IBD.
    connects_without_a_query(true, 365 * 24 * 3600);
}

#[test]
fn connector_spork_off_skips_the_conflict() {
    connects_without_a_query(false, 0);
}

/// An injected IS read fault: the connect is `Err(f)` with the store's text, V stops on it, and
/// V's stop reports it once (§5.3 item 6 "Faults"). No `finish_attempt:` line names it, and
/// `is::chain` logs nothing but Core's RemoveConflictingLock lines (erratum 11-15).
#[test]
fn connector_an_is_read_fault_is_reported_once() {
    let m = Matured::new();
    let store = Arc::new(InMemoryIsStore::default());
    let error = store_error("is_in read failed (s1b fault case)");
    let text = error.to_string();
    store.scripted.lock().unwrap().conflicting_lock_in.push_back(Err(error));
    capture::install();
    let rig = Rig::start(live_node(store, true), None, 0);
    rig.connect(&m.chain[1..]);
    let _ = rig.submit(&m.next(&[m.spend()]));
    let (exit, tip, reports) = rig.stop();
    assert_eq!(exit.fatal.as_ref().map(|f| f.0.as_str()), Some(text.as_str()));
    assert_eq!(tip, m.tip().hash());
    assert_eq!(reports.len(), 1, "{reports:?}");
    assert!(reports[0].contains(&text), "{reports:?}");
    assert_eq!(capture::lines_with(&format!("finish_attempt: {text}")), Vec::<String>::new());
    let own = capture::lines(|target, line| {
        target.starts_with("dashd_locks::is::chain")
            && !line.starts_with("CInstantSendManager::RemoveConflictingLock")
    });
    assert_eq!(own, Vec::<String>::new());
}

/// `begin_attempt` clears what an unrecorded attempt left on the ledger (§5.3 item 5).
#[test]
fn connector_begin_attempt_clears_a_left_ledger() {
    let m = Matured::new();
    let store = Arc::new(InMemoryIsStore::default());
    let ledger = ledger();
    ledger.lock().push(observation([1; 32], 0));
    let helper = IsHelper::live(NodeHelper, store, ledger.clone(), gate(true), IsTipCache::new());
    let rig = Rig::start(Box::new(helper), None, 0);
    rig.connect(&m.chain[1..2]);
    let _ = rig.stop();
    assert_eq!(ledger.lock().take(), Default::default());
}

/// What an earlier attempt left on the helper with no `end_attempt` after it (§5.3 item 5).
#[derive(Clone, Copy, Debug)]
enum Left {
    /// A kept removal fault ([`NO_PENDING_TX`]).
    Fault,
    /// The pending transaction of a found conflict.
    PendingTx,
}

/// What the armed attempt's helper holds once `begin_attempt` returned: whether a pending
/// transaction is kept, and the kept fault's text.
type Started = Arc<StdMutex<Vec<(bool, Option<String>)>>>;

/// The live IS helper, armed to leave [`Left`] on itself just before the next `begin_attempt`,
/// whose pending state it records.
struct Leftover {
    inner: IsHelper<NodeHelper>,
    /// A transaction the store finds a lock for, to leave a pending transaction.
    locked: Tx,
    armed: Arc<StdMutex<Option<Left>>>,
    started: Started,
}

impl ChainHelper for Leftover {
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.inner.cl_conflict(cx, h, hash)
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        self.inner.ibd(cx)
    }

    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        self.inner.cl_has(cx, h, hash)
    }

    fn cl_best(&mut self, cx: &ConnectCx<'_>) -> Result<Option<Arc<ChainLockSig>>, Fatal> {
        self.inner.cl_best(cx)
    }

    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        self.inner.is_conflict(tx)
    }

    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        self.inner.remove_conflicting_is_lock(lock, sink);
    }

    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        let Some(left) = self.armed.lock().unwrap().take() else {
            return self.inner.begin_attempt(cx, at);
        };
        match left {
            Left::Fault => self.inner.remove_conflicting_is_lock(&any_lock(), &mut Vec::new()),
            Left::PendingTx => {
                assert!(self.inner.is_conflict(&self.locked).expect("the store's answer").is_some())
            }
        }
        self.inner.begin_attempt(cx, at)?;
        let held = (
            self.inner.pending_tx.is_some(),
            self.inner.pending_fault.as_ref().map(|f| f.0.clone()),
        );
        self.started.lock().unwrap().push(held);
        Ok(())
    }

    fn end_attempt(&mut self) {
        self.inner.end_attempt();
    }

    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        self.inner.sb_required(cx, at)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.inner.gov_valid()
    }

    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal> {
        self.inner.sb_triggered(h)
    }

    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<PrimTxOut>>, Fatal> {
        self.inner.sb_payments(h)
    }

    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        self.inner.interim_refusal(cx, h)
    }
}

/// `left` is left on the helper just before the spend's attempt begins, with no `end_attempt`
/// after it: `begin_attempt` clears it, so the attempt starts clean and the lock-free spend's
/// block connects (§5.3 item 5).
fn begin_attempt_clears(left: Left) {
    let m = Matured::new();
    let spend = m.spend();
    let locked = spend_of(&[prevout(9, 0)]);
    let (store, _, _) = conflicting_store(&locked);
    let armed = Arc::new(StdMutex::new(None));
    let started = Started::default();
    let helper = Leftover {
        inner: IsHelper::live(NodeHelper, Arc::new(store), ledger(), gate(true), IsTipCache::new()),
        locked,
        armed: armed.clone(),
        started: started.clone(),
    };
    let rig = Rig::start(Box::new(helper), None, 0);
    rig.connect(&m.chain[1..]);
    *armed.lock().unwrap() = Some(left);
    let b = m.next(std::slice::from_ref(&spend));
    let _ = rig.submit(&b);
    let (exit, tip, reports) = rig.stop();
    assert_eq!(*started.lock().unwrap(), [(false, None)], "{left:?}");
    assert!(exit.fatal.is_none(), "{left:?}: {:?}", exit.fatal);
    assert_eq!(tip, b.hash(), "{left:?}");
    assert!(reports.is_empty(), "{left:?}: {reports:?}");
}

#[test]
fn connector_begin_attempt_clears_a_left_fault() {
    begin_attempt_clears(Left::Fault);
}

#[test]
fn connector_begin_attempt_clears_a_left_pending_tx() {
    begin_attempt_clears(Left::PendingTx);
}

/// A reorg under `NoEvo` never asks the IS helper: a block whose spend the store now reports as
/// conflicting is disconnected, and no query or removal is asked after its connect. That the
/// undo log never carries the IS removal effect rests on a grep, not on this case: the only
/// emitter of `RemoveConflictingIsLock` is the helper's removal, which only ConnectBlock's IS
/// loop calls (dashd-chainstate `connect/serial.rs`); the production `apply_log*` site is
/// `NodeEvo::finish_attempt` (`evo/node.rs`), shared by connects and undos, and an undo's log
/// comes from `undo_special_txs` alone.
#[test]
fn connector_a_reorg_never_asks_the_is_helper() {
    let m = Matured::new();
    let spend = m.spend();
    let (store, _, _) = conflicting_store(&spend);
    // The connect's query finds nothing; every later one would find the lock.
    store.scripted.lock().unwrap().conflicting_lock_in.push_back(Ok(None));
    let probe = Probe::new(live_node(Arc::new(store), true), false);
    let events = probe.events.clone();
    let rig = Rig::start(Box::new(probe), None, 0);
    rig.connect(&m.chain[1..]);
    rig.connect(&[m.next(std::slice::from_ref(&spend))]);
    let connected = events.lock().unwrap().len();
    // A longer sibling branch: the spend's block is disconnected.
    let b1 = block_on(&m.pow, m.tip(), MATURITY + 1, 1, OUTPUT_VALUE, &[]);
    let b2 = block_on(&m.pow, &b1, MATURITY + 2, 1, OUTPUT_VALUE, &[]);
    assert!(rig.submit(&b1));
    rig.connect(std::slice::from_ref(&b2));
    let (exit, _, _) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    let events = events.lock().unwrap().clone();
    let asked = format!("is_conflict {}", spend.txid());
    assert_eq!(events.iter().filter(|e| **e == asked).count(), 1, "{events:?}");
    assert!(events[..connected].contains(&asked));
    assert!(!events.iter().any(|e| e == "remove"), "{events:?}");
    let after: Vec<_> =
        events[connected..].iter().filter(|e| e.starts_with("is_conflict")).collect();
    assert!(after.is_empty(), "{after:?}");
}

// The live/replay gate (§6 S1b, review R2-3), on S1a's `IsDb` over a memory engine: a live run
// recorded through `RecordingHelper(IsHelper::live)`, then replayed through
// `IsHelper::replaying(ReplayChainHelper)` into a store with identical rows and cold caches.

/// `L` locks `tx`'s inputs, mined at height 1, in a freshly opened DB (cold caches).
fn db_with_mined_lock(
    tx: &Tx,
) -> (Arc<MemoryEngine>, Arc<IsDb<MemoryEngine>>, IsLockHash, LockedTx) {
    let engine = Arc::new(MemoryEngine::new(&SCHEMA));
    let lock = lock_for(tx.txid(), lock_inputs(tx));
    let hash = dash_evo::locks::isdlock_hash(&lock);
    {
        let db = IsDb::open(engine.clone()).unwrap();
        db.write_new(&hash, &lock).unwrap();
        db.write_mined(&hash, 1).unwrap();
    }
    let db = Arc::new(IsDb::open(engine.clone()).unwrap());
    (engine, db, hash, lock)
}

type Rows = Vec<(Vec<u8>, Vec<u8>)>;

/// The cache entries naming `hash`: its lock entry, then the txid and input entries.
fn names(
    caches: &CacheSnapshot,
    hash: IsLockHash,
) -> (Vec<Option<LockedTx>>, Vec<TxHash>, Vec<OutPoint>) {
    (
        caches.locks.iter().filter(|(k, _)| *k == hash).map(|(_, v)| v.clone()).collect(),
        caches.txids.iter().filter(|(_, v)| *v == hash).map(|(k, _)| *k).collect(),
        caches.inputs.iter().filter(|(_, v)| *v == hash).map(|(k, _)| *k).collect(),
    )
}

fn rows<K: KvRead>(engine: &Arc<K>) -> Rows {
    engine.iter_prefix(IS, &[]).map(Result::unwrap).collect()
}

/// The scenario's blocks after the matured chain: `b1` holds `L`'s own transaction and overpays
/// its coinbase, so it fails after the IS loop (`bad-cb-amount`) and leaves the coin unspent;
/// `b2`, its sibling, holds a competing spend of the same coin.
struct Confirmation {
    m: Matured,
    own: Arc<Tx>,
    rival: Arc<Tx>,
    b1: Arc<Block>,
    b2: Arc<Block>,
}

/// `b1`'s coinbase: far past any subsidy.
const OVERPAID: i64 = 10_000_000 * 100_000_000;

fn confirmation() -> Confirmation {
    let m = Matured::new();
    let own = m.spend();
    let rival = tx_spending(m.coin(), 30_000);
    let b1 = block_on(&m.pow, m.tip(), MATURITY + 1, 0, OVERPAID, std::slice::from_ref(&own));
    let b2 = block_on(&m.pow, m.tip(), MATURITY + 1, 1, OUTPUT_VALUE, std::slice::from_ref(&rival));
    Confirmation {
        m,
        own,
        rival,
        b1,
        b2,
    }
}

/// One step of a run: the tip after it and the IS rows and caches then, `None` once V has
/// stopped.
type Step = Option<(BlockHash, Rows, CacheSnapshot)>;

/// The run's schedule: the matured chain, `b1` (its lookup finds `L`'s own lock: `None`, but the
/// outpoint and lock caches warm), a ChainLock's confirmation of `L` at height 1 (Core keeps the
/// caches, `instantsend/db.cpp:118-164`), then `b2`, whose lookup finds `L` through the caches
/// alone. The state after each of the two attempts.
fn run_schedule(
    rig: &Rig,
    s: &Confirmation,
    engine: &Arc<MemoryEngine>,
    db: &IsDb<MemoryEngine>,
) -> [Step; 2] {
    rig.connect(&s.m.chain[1..]);
    let step = |block| rig.submit(block).then(|| (rig.tip(), rows(engine), db.cache_snapshot()));
    let after_b1 = step(&s.b1);
    db.remove_confirmed(1).unwrap();
    [after_b1, step(&s.b2)]
}

type Answers = (Option<LockedTx>, Option<LockedTx>, Option<LockedTx>, Option<LockedTx>, bool);

/// The cache-visible answers after a run, in this order (the bypass read last but one: it
/// inserts into the lock cache).
fn answers(db: &IsDb<MemoryEngine>, s: &Confirmation, hash: &IsLockHash) -> Answers {
    let input = lock_inputs(&s.own)[0];
    (
        db.by_input(&input).unwrap(),
        db.by_txid(&s.own.txid()).unwrap(),
        db.by_hash(hash, true).unwrap(),
        db.by_hash(hash, false).unwrap(),
        db.known(hash).unwrap(),
    )
}

fn replay_helper(
    recorded: Arc<RecordedDecisions>,
    miss: MissSlot,
    db: Arc<IsDb<MemoryEngine>>,
    ledger: SharedLedger,
) -> Box<dyn ChainHelper> {
    let inner = ReplayChainHelper::new(recorded, Policy::Strict, AttemptChoice::InOrder, miss);
    Box::new(IsHelper::replaying(inner, db, ledger, gate(true), IsTipCache::new()))
}

#[test]
fn connector_live_and_replayed_runs_agree_including_the_confirmation_case() {
    capture::install();
    let s = confirmation();
    let (live_engine, live_db, hash, lock) = db_with_mined_lock(&s.own);
    let initial = rows(&live_engine);
    let digest = Buf::default();
    let mut decisions = Buf::default();
    decisions.write_all(&Record::Header(digest_header(FileKind::Decisions)).to_line()).unwrap();
    let tap = DecisionTap::new();
    let live_ledger = ledger();
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(1_700_000_000));
    let live = IsHelper::live(
        NodeHelper,
        live_db.clone(),
        live_ledger.clone(),
        gate(true),
        IsTipCache::new(),
    );
    let helper = RecordingHelper::new(Box::new(live), tap.clone(), clock);
    let log = DigestLog::without_golden(digest.clone(), digest_header(FileKind::Digest)).unwrap();
    let sink = TapedLog::new(log, decisions.clone(), tap);
    let rig = Rig::start(Box::new(helper), Some(Box::new(sink)), 0);
    let live_steps = run_schedule(&rig, &s, &live_engine, &live_db);
    let (exit, _, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(reports.is_empty(), "{reports:?}");
    // Both blocks are rejected and the tip stays at `parent` (DEC-1631).
    let parent = s.m.tip().hash();
    let tips: Vec<_> = live_steps.iter().map(|step| step.as_ref().map(|(tip, ..)| *tip)).collect();
    assert_eq!(tips, [Some(parent), Some(parent)]);
    let overpaid = format!("coinbase pays too much at height 101 (actual={OVERPAID}");
    assert!(!capture::lines_with(&overpaid).is_empty());
    assert!(!capture::lines_with(&conflict_line(s.rival.txid(), s.own.txid())).is_empty());
    let recorded = Arc::new(RecordedDecisions::load(&digest.bytes(), &decisions.bytes()).unwrap());

    // The replay, into identical rows with cold caches.
    let (replay_engine, replay_db, _, _) = db_with_mined_lock(&s.own);
    assert_eq!(rows(&replay_engine), initial);
    let miss = MissSlot::new();
    let replay_ledger = ledger();
    let helper =
        replay_helper(recorded.clone(), miss.clone(), replay_db.clone(), replay_ledger.clone());
    let rig = Rig::start(helper, None, 0);
    let replay_steps = run_schedule(&rig, &s, &replay_engine, &replay_db);
    let (exit, _, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(reports.is_empty(), "{reports:?}");
    assert_eq!(miss.get(), None);
    assert_eq!(replay_steps, live_steps);
    assert_eq!(rows(&live_engine), rows(&replay_engine));
    assert_eq!(live_ledger.lock().take(), replay_ledger.lock().take());
    let live_answers = answers(&live_db, &s, &hash);
    assert_eq!(live_answers, answers(&replay_db, &s, &hash));
    // `L` is answered through the caches in both, and its rows are gone.
    assert_eq!((&live_answers.0, &live_answers.3), (&Some(lock), &None));

    // Replayed into rows without `L`: `b2`'s recorded conflict is not the store's.
    let other = Arc::new(MemoryEngine::new(&SCHEMA));
    let other_db = Arc::new(IsDb::open(other.clone()).unwrap());
    let rig =
        Rig::start(replay_helper(recorded, MissSlot::new(), other_db.clone(), ledger()), None, 0);
    let _ = run_schedule(&rig, &s, &other, &other_db);
    let (exit, _, reports) = rig.stop();
    let fault = exit.fatal.expect("the replay stops").0;
    assert!(fault.starts_with(REPLAY_DIVERGED), "{fault}");
    assert!(fault.contains(&s.rival.txid().to_string()), "{fault}");
    assert_eq!(reports.len(), 1, "{reports:?}");
}

/// The control for the confirmation case: without `b1`'s lookup the confirmed lock's rows are
/// gone and its caches cold, so `b2` connects. The replay gate therefore depends on the replayed
/// `None` lookup warming the caches as the live one did.
#[test]
fn connector_without_the_warming_lookup_the_confirmed_lock_is_not_found() {
    let s = confirmation();
    let (_, db, _, _) = db_with_mined_lock(&s.own);
    let helper = IsHelper::live(NodeHelper, db.clone(), ledger(), gate(true), IsTipCache::new());
    let rig = Rig::start(Box::new(helper), None, 0);
    rig.connect(&s.m.chain[1..]);
    db.remove_confirmed(1).unwrap();
    rig.connect(std::slice::from_ref(&s.b2));
    let (exit, _, _) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
}

// S4: `ResolveBlockConflicts` end to end (PLAN-LK1b §6 S4): the processor asks V through
// `VChain`, and V answers InstantSend's half from `IsBlockConflicts` over the manager's
// non-locked graph.

struct Found(Option<Arc<Tx>>);

impl crate::is::seams::TxLookup for Found {
    fn get_transaction(&self, _: &TxHash) -> Option<(Arc<Tx>, Option<BlockHash>)> {
        self.0.clone().map(|tx| (tx, None))
    }
}

struct NoChainLocks;

impl crate::is::seams::ChainLockReads for NoChainLocks {
    fn has_chain_lock(&self, _: i32, _: &BlockHash) -> bool {
        false
    }
}

struct NoPeers;

impl crate::is::seams::IsPeers for NoPeers {
    fn misbehaving(&self, _: crate::is::manager::NodeId, _: i32) {}
    fn erase_object_request(&self, _: crate::is::manager::NodeId, _: &IsLockHash) {}
    fn forget_object_request(&self, _: &IsLockHash) {}
}

struct PanicFaults;

impl crate::is::listener::IsFaults for PanicFaults {
    fn failed(&self, site: &'static str, error: &StoreError) {
        panic!("{site}: {error}");
    }
}

/// V over a matured chain whose tip `b` mines `conflict`, tracked as non-locked in `b` as the
/// listener's `BlockConnected` tracks it, and a processor whose lookup finds `own` when given.
/// Returns the rig, the manager, the store, the processor, `b`'s parent and `b`.
fn resolving_rig(
    own: Option<Arc<Tx>>,
    m: &Matured,
    conflict: &Arc<Tx>,
) -> (
    Rig,
    Arc<crate::is::manager::IsManager>,
    Arc<IsDb<MemoryEngine>>,
    crate::is::process::IsProcessor,
    Arc<Block>,
) {
    use crate::is::conflicts::{IsBlockConflicts, IsConflictsParts};
    use crate::is::manager::{IsManager, MinedBlock};
    use crate::is::process::{IsProcessor, IsProcessorParts, NoNotify, ResolvingLocks, VChain};
    let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).unwrap());
    let manager = Arc::new(IsManager::new(db.clone(), gate(true), Arc::new(MockClock::new())));
    let resolving = Arc::new(ResolvingLocks::default());
    let is = IsBlockConflicts::new(IsConflictsParts {
        store: db.clone(),
        manager: Arc::clone(&manager),
        tip: IsTipCache::new(),
        signer: None,
        resolving: Arc::clone(&resolving),
        faults: Arc::new(PanicFaults),
    });
    let abort = Arc::new(RecordingAbort::default());
    let helper = live_node(db.clone(), true);
    let rig = Rig::start_is(helper, None, 0, NoEvo, GlobalFailStop::new(), abort, Box::new(is));
    rig.connect(&m.chain[1..]);
    let b = m.next(std::slice::from_ref(conflict));
    rig.connect(std::slice::from_ref(&b));
    let mined = MinedBlock {
        id: rig.v.reader().lookup(&b.hash()).expect("b is indexed"),
        height: m.next_height(),
        hash: b.hash(),
    };
    manager.add_non_locked_tx(conflict, Some(mined));
    let processor = IsProcessor::new(IsProcessorParts {
        manager: Arc::clone(&manager),
        txs: Arc::new(Found(own)),
        chain: Arc::new(VChain(rig.v.clone())),
        chainlocks: Arc::new(NoChainLocks),
        recsigs: Arc::new(crate::is::seams::NoRecSigs),
        peers: Arc::new(NoPeers),
        signer: None,
        mempool: None,
        notify: Arc::new(NoNotify),
        resolving,
        is_llmq: LlmqType::LlmqTest,
    });
    (rig, manager, db, processor, b)
}

/// A lock for `own`, whose input the tip's `conflict` spends.
fn own_and_conflict(m: &Matured) -> (Arc<Tx>, Arc<Tx>, Arc<LockedTx>) {
    let own = tx_spending(m.coin(), 30_000);
    let conflict = m.spend();
    let lock = Arc::new(lock_for(own.txid(), lock_inputs(&own)));
    (own, conflict, lock)
}

/// The lock's transaction is unknown, so the lock is parked: the conflicting block is
/// invalidated, then reset, and no ABC runs, so the tip stays below it until the next activation,
/// which connects it again (the parked lock is not in the store).
// Core: instantsend/net_instantsend.cpp:395-404, :609-632
#[test]
fn s4_a_lock_for_an_unknown_transaction_invalidates_then_resets_the_conflict() {
    use crate::is::seams::IsLockProcessor;
    let m = Matured::new();
    let (_, conflict, lock) = own_and_conflict(&m);
    let (rig, manager, db, processor, b) = resolving_rig(None, &m, &conflict);
    let hash = isdlock_hash(&lock);
    processor.process_instant_send_lock(1, &hash, &lock, false).unwrap();
    assert_eq!(rig.tip(), m.tip().hash());
    assert!(manager.is_waiting_for_tx(&lock.txid()));
    assert!(!db.known(&hash).unwrap());
    assert_eq!(manager.counts().unwrap().unprotected_tx, 0);
    assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
    assert_eq!(rig.tip(), b.hash());
    let (exit, _, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(reports.is_empty(), "{reports:?}");
}

/// The lock's transaction is known, so the lock is written: the conflicting block is invalidated
/// and the ABC leaves it, so the tip is its parent and stays there.
// Core: instantsend/net_instantsend.cpp:386-394, :609-642
#[test]
fn s4_a_lock_for_a_known_transaction_invalidates_the_conflict_then_activates() {
    use crate::is::seams::IsLockProcessor;
    let m = Matured::new();
    let (own, conflict, lock) = own_and_conflict(&m);
    let (rig, manager, db, processor, _) = resolving_rig(Some(own), &m, &conflict);
    let hash = isdlock_hash(&lock);
    processor.process_instant_send_lock(1, &hash, &lock, false).unwrap();
    assert_eq!(rig.tip(), m.tip().hash());
    assert!(db.known(&hash).unwrap());
    assert!(!manager.is_waiting_for_tx(&lock.txid()));
    assert_eq!(manager.counts().unwrap().unprotected_tx, 0);
    assert_eq!(rig.v.activate_best_chain().recv(), Ok(Ok(())));
    assert_eq!(rig.tip(), m.tip().hash());
    let (exit, _, reports) = rig.stop();
    assert!(exit.fatal.is_none(), "{:?}", exit.fatal);
    assert!(reports.is_empty(), "{reports:?}");
}
