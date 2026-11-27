use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::BlockId;
use dash_consensus::primitives::P2pCommitment;
use dash_dml::StoreError;
use dash_evo::llmq::LlmqCommitment;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue, secret_to_public};
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, StoredContributions, cleanup_due, quorum_db_key,
};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig};
use dash_primitives::{BlockHash, DynBitset, QuorumHash, QuorumVvecHash};
use dash_types::Numeric as _;
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr};
use dashd_quorums::{
    ChainQuorums, CleanupFacade, DkgChain, LocalMasternode, PlatformQuorums, QuorumKeyStore,
    QuorumLookup, QuorumManager, VerifiedContributions, WarmExecutor, traits::Contributions,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fault(String);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Default)]
struct StoreState {
    rows: BTreeMap<QuorumDbKey, StoredContributions>,
    reads: usize,
    writes: usize,
    erases: usize,
    read_error: Option<String>,
    write_error: Option<String>,
}

#[derive(Clone, Default)]
struct TestStore(Arc<Mutex<StoreState>>);

impl TestStore {
    fn seed(&self, key: QuorumDbKey, row: StoredContributions) {
        self.0.lock().unwrap().rows.insert(key, row);
    }

    fn set_read_error(&self, error: &str) {
        self.0.lock().unwrap().read_error = Some(error.to_owned());
    }

    fn set_write_error(&self, error: &str) {
        self.0.lock().unwrap().write_error = Some(error.to_owned());
    }

    fn writes(&self) -> usize {
        self.0.lock().unwrap().writes
    }

    fn erases(&self) -> usize {
        self.0.lock().unwrap().erases
    }

    fn contains(&self, key: &QuorumDbKey) -> bool {
        self.0.lock().unwrap().rows.contains_key(key)
    }
}

impl QuorumKeyStore for TestStore {
    type Error = Fault;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.reads += 1;
        if let Some(error) = state.read_error.clone() {
            return Err(Fault(error));
        }
        Ok(state.rows.get(key).cloned())
    }

    fn write(&self, key: &QuorumDbKey, row: &StoredContributions) -> Result<(), Self::Error> {
        let mut state = self.0.lock().unwrap();
        if let Some(error) = state.write_error.clone() {
            return Err(Fault(error));
        }
        state.writes += 1;
        state.rows.insert(*key, row.clone());
        Ok(())
    }

    fn erase_except(
        &self,
        keep: &std::collections::BTreeSet<QuorumDbKey>,
    ) -> Result<CleanupCounts, Self::Error> {
        let mut state = self.0.lock().unwrap();
        state.erases += 1;
        state.rows.retain(|key, _| keep.contains(key));
        Ok(CleanupCounts::default())
    }
}

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
struct TestExecutor {
    jobs: Arc<Mutex<Vec<Job>>>,
}

impl TestExecutor {
    fn len(&self) -> usize {
        self.jobs.lock().unwrap().len()
    }

    fn run_all(&self) {
        while let Some(job) = self.jobs.lock().unwrap().pop() {
            job();
        }
    }
}

impl WarmExecutor for TestExecutor {
    fn spawn_warm(&self, job: Box<dyn FnOnce() + Send>) {
        self.jobs.lock().unwrap().push(job);
    }
}

#[derive(Clone, Default)]
struct TestContributions {
    calls: Arc<AtomicUsize>,
    result: Arc<Mutex<Option<Contributions>>>,
}

impl TestContributions {
    fn successful() -> Self {
        let sk = BlsSkValue::from_canonical(&[1; 32], BlsSchemeKind::Basic).unwrap();
        let pk = secret_to_public(&sk);
        Self {
            calls: Arc::default(),
            result: Arc::new(Mutex::new(Some((vec![0], vec![Arc::new(vec![pk])], vec![])))),
        }
    }

    fn failed() -> Self {
        Self::default()
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl VerifiedContributions for TestContributions {
    fn verified(
        &self,
        _: LlmqType,
        _: &BlockHash,
        _: &[dash_evo::types::ProTxHash],
        _: &[bool],
    ) -> Option<Contributions> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.lock().unwrap().clone()
    }
}

#[derive(Clone, Copy)]
struct TestMasternode(Option<[u8; 32]>);

impl LocalMasternode for TestMasternode {
    fn pro_tx_hash(&self) -> Option<dash_evo::types::ProTxHash> {
        self.0.map(Into::into)
    }
}

fn params() -> Arc<dash_params::ChainSetup> {
    Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap())
}

fn quorum(llmq_type: LlmqType, hash: u8) -> QuorumPtr {
    let bits = DynBitset::new(0, Vec::new()).unwrap();
    let commitment = LlmqCommitment::new(
        3,
        llmq_type,
        QuorumHash::from([hash; 32]),
        None,
        bits.clone(),
        bits,
        BlsEagerPk::new(BlsPkValue::invalid(), BlsSchemeKind::Basic),
        QuorumVvecHash::from([0; 32]),
        BlsEagerSig::new(dash_llmq::dkg::bls::BlsSigValue::invalid(), BlsSchemeKind::Basic),
        BlsEagerSig::new(dash_llmq::dkg::bls::BlsSigValue::invalid(), BlsSchemeKind::Basic),
    );
    QuorumPtr(Arc::new(NodeQuorum {
        llmq_type,
        commitment: Arc::new(P2pCommitment::new(commitment)),
        base: BlockId(u32::from(hash)),
        base_hash: BlockHash::from([hash; 32]),
        mined_block: BlockHash::from([hash.wrapping_add(1); 32]),
        members: Arc::from(Vec::<Arc<dash_dml::Dmn>>::new()),
        keys: Mutex::new(Default::default()),
    }))
}

fn key(q: &QuorumPtr) -> QuorumDbKey {
    quorum_db_key(q.llmq_type, &q.commitment.quorum_hash(), &[])
}

fn manager(
    store: TestStore,
    contributions: TestContributions,
    masternode: TestMasternode,
    executor: TestExecutor,
) -> QuorumManager<TestStore, TestContributions, TestMasternode, TestExecutor> {
    QuorumManager::new(params(), store, contributions, masternode, executor)
}

fn row() -> StoredContributions {
    let sk = BlsSkValue::from_canonical(&[1; 32], BlsSchemeKind::Basic).unwrap();
    StoredContributions {
        vvec: Ok(Some(vec![secret_to_public(&sk)])),
        sk_share: None,
    }
}

#[test]
fn hook_order_db_hit_skips_contributions_and_warms() {
    let q = quorum(LlmqType::LlmqTest, 7);
    let store = TestStore::default();
    store.seed(key(&q), row());
    let contributions = TestContributions::failed();
    let executor = TestExecutor::default();
    manager(store, contributions.clone(), TestMasternode(None), executor.clone())
        .built(&q, true, BlsSchemeKind::Basic)
        .unwrap();
    assert_eq!(contributions.calls(), 0, "a stored row skips the contributions");
    assert_eq!(executor.len(), 1, "a stored vvec is warmed");
}

#[test]
fn hook_order_db_hit_without_populate_does_not_warm() {
    let q = quorum(LlmqType::LlmqTest, 70);
    let store = TestStore::default();
    store.seed(key(&q), row());
    let contributions = TestContributions::failed();
    let executor = TestExecutor::default();
    manager(store, contributions.clone(), TestMasternode(None), executor.clone())
        .built(&q, false, BlsSchemeKind::Basic)
        .unwrap();
    assert_eq!(contributions.calls(), 0, "a stored row skips the contributions");
    assert_eq!(executor.len(), 0, "populate_cache=false does not warm");
}

#[test]
fn hook_order_read_error_reaches_caller_without_side_effects() {
    let q = quorum(LlmqType::LlmqTest, 8);
    let store = TestStore::default();
    store.set_read_error("corrupt row");
    let contributions = TestContributions::failed();
    let executor = TestExecutor::default();
    let error =
        manager(store.clone(), contributions.clone(), TestMasternode(None), executor.clone())
            .built(&q, true, BlsSchemeKind::Basic)
            .unwrap_err();
    assert!(matches!(error, StoreError::Read { .. }));
    assert_eq!(contributions.calls(), 0, "a read error never builds");
    assert_eq!(store.writes(), 0);
    assert_eq!(executor.len(), 0);
}

#[test]
fn hook_order_absent_row_builds_then_writes() {
    let q = quorum(LlmqType::LlmqTest, 9);
    let store = TestStore::default();
    let contributions = TestContributions::successful();
    let executor = TestExecutor::default();
    manager(store.clone(), contributions.clone(), TestMasternode(None), executor.clone())
        .built(&q, false, BlsSchemeKind::Basic)
        .unwrap();
    assert_eq!(contributions.calls(), 1);
    assert_eq!(store.writes(), 1);
    assert!(store.contains(&key(&q)));
    assert_eq!(executor.len(), 0, "populate_cache=false does not warm");
}

// The chainstate tests (`get_quorum_takes_cores_branches`) cover that this quorum is cached.
#[test]
fn hook_order_failed_build_is_cached_without_write_or_warm() {
    let q = quorum(LlmqType::LlmqTest, 10);
    let store = TestStore::default();
    let contributions = TestContributions::failed();
    let executor = TestExecutor::default();
    manager(store.clone(), contributions.clone(), TestMasternode(None), executor.clone())
        .built(&q, true, BlsSchemeKind::Basic)
        .unwrap();
    assert_eq!(contributions.calls(), 1);
    assert_eq!(store.writes(), 0);
    assert_eq!(executor.len(), 0);
}

#[test]
fn hook_order_vvec_writes_even_when_secret_share_fails() {
    let q = quorum(LlmqType::LlmqTest, 11);
    let store = TestStore::default();
    let contributions = TestContributions::successful();
    let executor = TestExecutor::default();
    manager(store.clone(), contributions, TestMasternode(Some([1; 32])), executor)
        .built(&q, false, BlsSchemeKind::Basic)
        .unwrap();
    assert_eq!(store.writes(), 1);
    assert!(store.contains(&key(&q)));
}

#[test]
fn hook_write_error_reaches_caller_without_warm() {
    let q = quorum(LlmqType::LlmqTest, 14);
    let store = TestStore::default();
    store.set_write_error("disk full");
    let contributions = TestContributions::successful();
    let executor = TestExecutor::default();
    let error =
        manager(store.clone(), contributions.clone(), TestMasternode(None), executor.clone())
            .built(&q, true, BlsSchemeKind::Basic)
            .unwrap_err();
    assert!(matches!(error, StoreError::Read { .. }));
    assert_eq!(contributions.calls(), 1, "the build ran before the write");
    assert_eq!(store.writes(), 0);
    assert!(!store.contains(&key(&q)));
    assert_eq!(executor.len(), 0, "a write error warms nothing");
    // The chainstate test `a_hook_error_is_not_cached_and_reaches_get_quorum` covers that a hook
    // error leaves the quorum uncached.
}

/// Every type's scan finds the quorum keyed `[4; 32]`, except that `fail`'s scan fails and, with
/// `empty`, every scan finds none.
#[derive(Default)]
struct Cleanup {
    scanned_at: Mutex<Vec<BlockId>>,
    fail: Option<LlmqType>,
    empty: bool,
}

impl CleanupFacade for Cleanup {
    fn ancestors(&self, _: BlockId) -> (Option<BlockId>, BlockHash) {
        (None, BlockHash::from([4; 32]))
    }

    fn scan(
        &self,
        llmq_type: LlmqType,
        tip: BlockId,
        count: usize,
    ) -> Option<Vec<(BlockHash, QuorumDbKey)>> {
        self.scanned_at.lock().unwrap().push(tip);
        if self.fail == Some(llmq_type) {
            return None;
        }
        Some(
            (count > 0 && !self.empty)
                .then_some((BlockHash::from([4; 32]), QuorumDbKey([4; 32])))
                .into_iter()
                .collect(),
        )
    }
}

/// The store `[4; 32]` (every scan's quorum) and `[5; 32]` (none's), after one cleanup job over
/// `facade` at a due height, and the store's erase calls.
fn clean_with(facade: Cleanup) -> (bool, bool, usize) {
    clean_over(Arc::new(facade))
}

/// [`clean_with`] over any cleanup facade.
fn clean_over(facade: Arc<dyn CleanupFacade>) -> (bool, bool, usize) {
    clean_over_logged(facade).0
}

/// [`clean_over`], and the lines the job itself logged.
fn clean_over_logged(facade: Arc<dyn CleanupFacade>) -> ((bool, bool, usize), Vec<String>) {
    let store = TestStore::default();
    let (keep, drop) = (QuorumDbKey([4; 32]), QuorumDbKey([5; 32]));
    store.seed(keep, row());
    store.seed(drop, row());
    let executor = TestExecutor::default();
    let manager =
        manager(store.clone(), TestContributions::failed(), TestMasternode(None), executor.clone())
            .with_cleanup_facade(facade);
    manager.start_cleanup_old_quorum_data(BlockId(58), 58);
    let ((), lines) = logged(|| executor.run_all());
    ((store.contains(&keep), store.contains(&drop), store.erases()), lines)
}

/// The messages `f` logs on this thread, as `share_seam`'s `logged` captures them.
fn logged<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    struct Lines(Mutex<Vec<String>>);
    impl tracing::Subscriber for Lines {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn fmt::Debug) {
                    self.0.push_str(&format!(" {}={v:?}", f.name()));
                }
            }
            let mut m =
                Message(format!("{} {}", event.metadata().level(), event.metadata().target()));
            event.record(&mut m);
            self.0.lock().unwrap().push(m.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    // BL-479: never the only registered dispatcher, or a thread without one that registers a
    // callsite first caches `Interest::never` for it.
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap());
    (out, lines)
}

fn logged_done(lines: &[String]) -> bool {
    lines.iter().any(|line| line.contains("StartCleanupOldQuorumDataThread -- done"))
}

/// Q2: a scan that fails for any one type, the first, a middle one or the last, ends the job
/// before it erases (Core: a throwing `ScanQuorums` ends the ctpl job before
/// `CleanupOldQuorumData` and before the done line, v24.0.0 src/llmq/net_quorum.cpp:719-764): no
/// row goes, the store is not asked to erase, and the job logs no done line. A scan that finds
/// its quorums still logs that line.
#[test]
fn a_failed_scan_of_any_type_erases_no_row() {
    let types: Vec<LlmqType> = params().consensus.llmq_params().map(|(t, _)| t).collect();
    assert!(types.len() >= 3, "{types:?}");
    for fail in [types[0], types[types.len() / 2], types[types.len() - 1]] {
        let (got, lines) = clean_over_logged(Arc::new(Cleanup {
            fail: Some(fail),
            ..Cleanup::default()
        }));
        assert_eq!(got, (true, true, 0), "{fail:?}'s scan failed");
        assert!(!logged_done(&lines), "{fail:?} logged done: {lines:?}");
    }
    let (got, lines) = clean_over_logged(Arc::new(Cleanup::default()));
    assert_eq!(got, (true, false, 1), "no failure: the stale row goes");
    assert!(logged_done(&lines), "a finished scan logs done: {lines:?}");
}

/// Q2: a scan that succeeds and finds no quorum is not a failure: the job erases every row not
/// kept, as before.
#[test]
fn an_empty_scan_still_erases() {
    let got = clean_with(Cleanup {
        empty: true,
        ..Cleanup::default()
    });
    assert_eq!(got, (false, false, 1));
}

#[test]
fn cleanup_uses_576_58_keep_set_and_deferred_erase() {
    assert!(cleanup_due(58));
    assert!(cleanup_due(634));
    assert!(!cleanup_due(57));
    assert!(!cleanup_due(59));

    let store = TestStore::default();
    let keep = QuorumDbKey([4; 32]);
    let drop = QuorumDbKey([5; 32]);
    store.seed(keep, row());
    store.seed(drop, row());
    let executor = TestExecutor::default();
    let manager =
        manager(store.clone(), TestContributions::failed(), TestMasternode(None), executor.clone())
            .with_cleanup_facade(Arc::new(Cleanup::default()));
    manager.start_cleanup_old_quorum_data(BlockId(58), 58);
    assert_eq!(executor.len(), 1, "cleanup is deferred");
    assert!(store.contains(&drop), "the caller was not blocked into cleanup");
    assert_eq!(store.erases(), 0, "nothing is erased before the deferred job runs");
    executor.run_all();
    assert!(store.contains(&keep));
    assert!(!store.contains(&drop));
    assert_eq!(store.erases(), 1);
}

#[test]
fn cleanup_gates_on_the_height_not_the_block_id() {
    let executor = TestExecutor::default();
    let manager = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        executor.clone(),
    )
    .with_cleanup_facade(Arc::new(Cleanup::default()));
    manager.start_cleanup_old_quorum_data(BlockId(58), 59);
    assert_eq!(executor.len(), 0, "height 59 is not due, whatever the block id");
    manager.start_cleanup_old_quorum_data(BlockId(7), 634);
    assert_eq!(executor.len(), 1, "height 634 is due, whatever the block id");
}

#[test]
fn cleanup_scans_at_the_captured_tip() {
    let executor = TestExecutor::default();
    let facade = Arc::new(Cleanup::default());
    let manager = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        executor.clone(),
    )
    .with_cleanup_facade(facade.clone());
    manager.start_cleanup_old_quorum_data(BlockId(9), 58);
    executor.run_all();
    let scanned = facade.scanned_at.lock().unwrap().clone();
    assert!(!scanned.is_empty(), "the job scanned");
    assert!(scanned.iter().all(|tip| *tip == BlockId(9)), "every scan is at the captured tip");
}

struct Lookup {
    expected_type: LlmqType,
    expected_hash: QuorumHash,
    result: Mutex<Result<Option<Arc<NodeQuorum>>, StoreError>>,
}

impl QuorumLookup for Lookup {
    fn quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<Arc<NodeQuorum>>, StoreError> {
        if llmq_type != self.expected_type {
            return Ok(None);
        }
        assert_eq!(*quorum_hash, self.expected_hash);
        self.result.lock().unwrap().clone()
    }
}

#[test]
fn platform_quorums_looks_up_any_mined_quorum_by_type_and_hash() {
    let q = quorum(LlmqType::LlmqTestPlatform, 12);
    let hash = q.commitment.quorum_hash();
    let lookup = Arc::new(Lookup {
        expected_type: LlmqType::LlmqTestPlatform,
        expected_hash: hash,
        result: Mutex::new(Ok(Some(q.0.clone()))),
    });
    let qmanager = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        TestExecutor::default(),
    )
    .with_quorum_lookup(lookup);
    let got = qmanager.quorum(LlmqType::LlmqTestPlatform, &hash).unwrap().unwrap();
    assert_eq!(got.commitment.quorum_hash(), hash);
}

#[test]
fn platform_quorums_returns_none_for_missing_and_wrong_type() {
    let hash = QuorumHash::from([13; 32]);
    let missing = Arc::new(Lookup {
        expected_type: LlmqType::LlmqTestPlatform,
        expected_hash: hash,
        result: Mutex::new(Ok(None)),
    });
    let qmanager = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        TestExecutor::default(),
    )
    .with_quorum_lookup(missing);
    assert!(qmanager.quorum(LlmqType::LlmqTestPlatform, &hash).unwrap().is_none());
    assert!(qmanager.quorum(LlmqType::LlmqTest, &hash).unwrap().is_none());

    let unwired = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        TestExecutor::default(),
    );
    assert!(unwired.quorum(LlmqType::LlmqTestPlatform, &hash).unwrap().is_none());
}

#[test]
fn platform_quorums_returns_a_lookup_error_as_err() {
    let hash = QuorumHash::from([13; 32]);

    let error_lookup = Arc::new(Lookup {
        expected_type: LlmqType::LlmqTestPlatform,
        expected_hash: hash,
        result: Mutex::new(Err(StoreError::read("lookup failed"))),
    });
    let manager = manager(
        TestStore::default(),
        TestContributions::failed(),
        TestMasternode(None),
        TestExecutor::default(),
    )
    .with_quorum_lookup(error_lookup);
    assert!(matches!(
        manager.quorum(LlmqType::LlmqTestPlatform, &hash),
        Err(StoreError::Read { .. })
    ));
}

/// Q3: `ChainQuorums` before a V is bound. A lookup is an error, not "no such quorum"; a scan
/// fails, so the cleanup job that runs it erases nothing; the ancestry has none.
#[test]
fn unbound_chain_quorums_look_up_nothing_and_clean_nothing() {
    let unbound = ChainQuorums::new();
    let t = LlmqType::from_base(100);
    let hash = QuorumHash::from([1; 32]);
    let err = unbound.quorum(t, &hash).expect_err("no V to ask");
    assert!(matches!(err, StoreError::NotBuilt(_)), "{err:?}");
    assert!(unbound.scan(t, BlockId(58), 2).is_none(), "a failed scan, not an empty one");
    assert_eq!(unbound.ancestors(BlockId(58)), (None, BlockHash::from([0; 32])));
    assert_eq!(clean_over(Arc::new(ChainQuorums::new())), (true, true, 0), "no row goes");
}

/// Q3 once V has stopped: a bound `ChainQuorums` answers as it does before the bind. A lookup is
/// an error, a scan fails, and the cleanup job that runs it erases nothing.
#[test]
fn chain_quorums_after_v_has_stopped_look_up_nothing_and_clean_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ui = Arc::new(dashd_node::ui::Ui::with_stderr(
        Arc::new(dashd_node::logging::Logger::new()),
        Box::new(|_| {}),
    ));
    let sut = dashd_node::sut::NodeSut::new(
        dir.path(),
        &["-regtest", "-printtoconsole=0"],
        1_700_000_000,
        ui,
    )
    .expect("V starts");
    let handle = sut.chain().expect("V runs").clone();
    let bound = Arc::new(ChainQuorums::new());
    bound.bind(handle.clone());
    let t = LlmqType::LlmqTest;
    let hash = QuorumHash::from([1; 32]);
    let tip = handle.view().snap.tip.tip;
    assert!(bound.quorum(t, &hash).expect("V serves").is_none(), "no such quorum");
    assert!(bound.scan(t, tip, 1).is_some(), "a served scan, empty or not");
    let view = handle.view();
    let tip_height = i32::try_from(view.snap.tip.tip_height).expect("a regtest height");
    assert_eq!(DkgChain::active_height(bound.as_ref()), Some(tip_height));
    assert_eq!(
        DkgChain::height(bound.as_ref(), &view.snap.tip.tip_hash),
        Some(view.snap.tip.tip_height)
    );
    handle.shutdown();
    let started = std::time::Instant::now();
    while handle.query(|_| ()).recv().is_ok() {
        assert!(started.elapsed() < std::time::Duration::from_secs(10), "V did not stop");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let err = bound.quorum(t, &hash).expect_err("V has stopped");
    assert!(matches!(err, StoreError::NotBuilt(_)), "{err:?}");
    assert!(bound.scan(t, tip, 1).is_none(), "a failed scan, not an empty one");
    assert_eq!(clean_over(bound.clone()), (true, true, 0), "no row goes");
    assert_eq!(DkgChain::active_height(bound.as_ref()), Some(tip_height), "the last publication");
    #[cfg(feature = "testkit-seams")]
    {
        use dashd_quorums::ShareSeamView;
        assert!(ShareSeamView::mined_quorum(bound.as_ref(), t, &hash).expect("no fault").is_none());
        assert!(ShareSeamView::cached_quorum(bound.as_ref(), t, &hash).is_none());
    }
}
