use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::BlockId;
use dash_dml::StoreError;
use dash_evo::llmq::LlmqCommitment;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSchemeKind, BlsSkValue, secret_to_public};
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, StoredContributions, cleanup_due, quorum_db_key,
};
use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig};
use dash_primitives::{BlockHash, DynBitset, QuorumHash, QuorumVvecHash};
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr};
use dashd_quorums::{
    CleanupFacade, LocalMasternode, PlatformQuorums, QuorumKeyStore, QuorumLookup, QuorumManager,
    VerifiedContributions, WarmExecutor, traits::Contributions,
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
    fn verified(&self, _: LlmqType, _: &BlockHash, _: &[bool]) -> Option<Contributions> {
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
        commitment: Arc::new(commitment),
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
    QuorumManager::new(params(), BlsSchemeKind::Basic, store, contributions, masternode, executor)
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
        .built(&q, true)
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
        .built(&q, false)
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
            .built(&q, true)
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
        .built(&q, false)
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
        .built(&q, true)
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
        .built(&q, false)
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
            .built(&q, true)
            .unwrap_err();
    assert!(matches!(error, StoreError::Read { .. }));
    assert_eq!(contributions.calls(), 1, "the build ran before the write");
    assert_eq!(store.writes(), 0);
    assert!(!store.contains(&key(&q)));
    assert_eq!(executor.len(), 0, "a write error warms nothing");
    // The chainstate test `a_hook_error_is_not_cached_and_reaches_get_quorum` covers that a hook
    // error leaves the quorum uncached.
}

#[derive(Default)]
struct Cleanup {
    scanned_at: Mutex<Vec<BlockId>>,
}

impl CleanupFacade for Cleanup {
    fn ancestors(&self, _: BlockId) -> (Option<BlockId>, BlockHash) {
        (None, BlockHash::from([4; 32]))
    }

    fn scan(&self, _: LlmqType, tip: BlockId, count: usize) -> Vec<(BlockHash, QuorumDbKey)> {
        self.scanned_at.lock().unwrap().push(tip);
        (count > 0)
            .then_some((BlockHash::from([4; 32]), QuorumDbKey([4; 32])))
            .into_iter()
            .collect()
    }
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
