//! The regtest share seam (`QuorumManager::load_test_quorum_shares`, feature `testkit-seams`)
//! over a synthetic DKG: three dealers' polynomials, the quorum vector their sum gives, and each
//! member's aggregated share, the inputs `BuildQuorumContributions` takes.
//!
//! The O12b-4 fixture acceptance (every loaded `pubKeyShare` equals the recorded `quorum info`
//! answer) belongs here once LQ4b sends the fixture path; it loads that file through
//! `load_test_quorum_shares` and compares `pub_key_share(i)` with each recorded member.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::BlockId;
use dash_dml::{Dmn, MnType, StoreError, testkit::dmn};
use dash_evo::llmq::{LlmqCommitment, OperatorKeyView};
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, aggregate_secret_keys,
    build_quorum_verification_vector, generate_contributions, member_id,
};
use dash_llmq::dkg::wire::vvec_hash;
use dash_llmq::quorum::{
    CleanupCounts, QuorumDbKey, QuorumView, StoredContributions, quorum_db_key,
};
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig};
use dash_primitives::{BlockHash, DynBitset, QuorumHash, QuorumVvecHash};
use dash_types::Numeric;
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumBuildHook, QuorumPtr};
use dashd_quorums::{
    LoadError, LocalMasternode, MinedQuorum, QuorumKeyStore, QuorumManager, Rejection,
    ShareSeamView, VerifiedContributions, WarmExecutor, traits::Contributions,
};
use serde_json::{Value, json};

const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;
/// This node is member 2 (index 1).
const ME: usize = 1;

#[derive(Clone, Debug)]
struct Fault(String);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Default)]
struct StoreState {
    rows: BTreeMap<QuorumDbKey, StoredContributions>,
    writes: usize,
    write_error: Option<String>,
}

#[derive(Clone, Default)]
struct TestStore(Arc<Mutex<StoreState>>);

impl TestStore {
    fn writes(&self) -> usize {
        self.0.lock().unwrap().writes
    }

    fn row(&self, key: &QuorumDbKey) -> Option<StoredContributions> {
        self.0.lock().unwrap().rows.get(key).cloned()
    }
}

impl QuorumKeyStore for TestStore {
    type Error = Fault;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Fault> {
        Ok(self.0.lock().unwrap().rows.get(key).cloned())
    }

    fn write(&self, key: &QuorumDbKey, row: &StoredContributions) -> Result<(), Fault> {
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
        _: &std::collections::BTreeSet<QuorumDbKey>,
    ) -> Result<CleanupCounts, Fault> {
        Ok(CleanupCounts::default())
    }
}

type Job = Box<dyn FnOnce() + Send>;

#[derive(Clone, Default)]
struct TestExecutor(Arc<Mutex<Vec<Job>>>);

impl TestExecutor {
    fn len(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn run_all(&self) {
        while let Some(job) = self.0.lock().unwrap().pop() {
            job();
        }
    }
}

impl WarmExecutor for TestExecutor {
    fn spawn_warm(&self, job: Job) {
        self.0.lock().unwrap().push(job);
    }
}

#[derive(Clone, Default)]
struct TestContributions {
    calls: Arc<AtomicUsize>,
    result: Option<Contributions>,
}

impl VerifiedContributions for TestContributions {
    fn verified(&self, _: LlmqType, _: &BlockHash, _: &[bool]) -> Option<Contributions> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.result.clone()
    }
}

#[derive(Clone, Copy)]
struct TestMasternode(Option<ProTxHash>);

impl LocalMasternode for TestMasternode {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0
    }
}

type ViewKey = (u8, [u8; 32]);

#[derive(Default)]
struct TestView {
    mined: Mutex<BTreeMap<ViewKey, MinedQuorum>>,
    cached: Mutex<BTreeMap<ViewKey, QuorumPtr>>,
    reads: AtomicUsize,
}

impl TestView {
    fn mine(&self, q: &QuorumPtr) {
        self.mined.lock().unwrap().insert(
            (q.llmq_type.to_base(), *q.commitment.quorum_hash().as_bytes()),
            MinedQuorum {
                commitment: q.commitment.clone(),
                members: q.members.clone(),
            },
        );
    }

    fn cache(&self, q: &QuorumPtr) {
        self.cached
            .lock()
            .unwrap()
            .insert((q.llmq_type.to_base(), *q.commitment.quorum_hash().as_bytes()), q.clone());
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::Relaxed)
    }
}

impl ShareSeamView for TestView {
    fn mined_quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<MinedQuorum>, StoreError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Ok(self.mined.lock().unwrap().get(&(llmq_type.to_base(), *quorum_hash.as_bytes())).cloned())
    }

    fn cached_quorum(&self, llmq_type: LlmqType, quorum_hash: &QuorumHash) -> Option<QuorumPtr> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.cached.lock().unwrap().get(&(llmq_type.to_base(), *quorum_hash.as_bytes())).cloned()
    }
}

type Manager = QuorumManager<TestStore, TestContributions, TestMasternode, TestExecutor>;

fn setup(network: Network) -> Arc<ChainSetup> {
    Arc::new(ChainParamsBuilder::new(network, Release::V24_0).build().unwrap())
}

fn sk(seed: u8) -> BlsSkValue {
    BlsSkValue::from_canonical(&[seed; 32], SCHEME).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A mined LLMQ_TEST quorum of regtest's size, every member valid, with a three-dealer DKG.
struct Fixture {
    llmq: dash_params::LlmqParams,
    hash: QuorumHash,
    members: Arc<[Arc<Dmn>]>,
    commitment: Arc<LlmqCommitment>,
    dealer_vvecs: Vec<Arc<Vec<BlsPkValue>>>,
    /// Each member's contributions from every dealer.
    contributions: Vec<Vec<BlsSkValue>>,
    vvec: Vec<BlsPkValue>,
    /// Each member's aggregated secret key share.
    shares: Vec<BlsSkValue>,
}

impl Fixture {
    fn new() -> Self {
        let llmq = *setup(Network::Regtest).consensus.llmq(LlmqType::LlmqTest).unwrap();
        let size = usize::from(llmq.size);
        let threshold = usize::from(llmq.threshold);
        let members: Arc<[Arc<Dmn>]> =
            (1..=size).map(|n| dmn(n as u8, MnType::Regular)).collect::<Vec<_>>().into();
        let ids: Vec<_> = members.iter().map(|m| member_id(&m.pro_tx_hash)).collect();
        let mut dealer_vvecs = Vec::new();
        let mut contributions = vec![Vec::new(); size];
        for dealer in 0..size {
            let coefficients: Vec<_> =
                (0..threshold).map(|c| sk((dealer * 16 + c + 1) as u8)).collect();
            let (vvec, shares) = generate_contributions(&coefficients, &ids).unwrap();
            dealer_vvecs.push(Arc::new(vvec));
            for (member, share) in shares.into_iter().enumerate() {
                contributions[member].push(share);
            }
        }
        let refs: Vec<&[BlsPkValue]> = dealer_vvecs.iter().map(|v| v.as_slice()).collect();
        let vvec = build_quorum_verification_vector(&refs, SCHEME).unwrap();
        let shares = contributions
            .iter()
            .map(|c| aggregate_secret_keys(&c.iter().collect::<Vec<_>>(), SCHEME))
            .collect();
        // Distinct bytes, so a byte-order slip shows.
        let hash = QuorumHash::from(std::array::from_fn::<u8, 32, _>(|i| i as u8 + 1));
        let mut bits = vec![0u8; size.div_ceil(8)];
        for i in 0..size {
            bits[i / 8] |= 1 << (i % 8);
        }
        let bits = DynBitset::new(size as u64, bits).unwrap();
        let commitment = Arc::new(LlmqCommitment::new(
            3,
            LlmqType::LlmqTest,
            hash,
            None,
            bits.clone(),
            bits,
            BlsEagerPk::new(vvec[0].clone(), SCHEME),
            QuorumVvecHash::from(vvec_hash(&vvec, SCHEME)),
            BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
            BlsEagerSig::new(BlsSigValue::invalid(), SCHEME),
        ));
        Self {
            llmq,
            hash,
            members,
            commitment,
            dealer_vvecs,
            contributions,
            vvec,
            shares,
        }
    }

    fn pro_tx(&self, i: usize) -> ProTxHash {
        self.members[i].pro_tx_hash
    }

    fn me(&self) -> TestMasternode {
        TestMasternode(Some(self.pro_tx(ME)))
    }

    /// A fresh quorum object over the mined commitment, as a build makes it.
    fn quorum(&self) -> QuorumPtr {
        QuorumPtr(Arc::new(NodeQuorum {
            llmq_type: LlmqType::LlmqTest,
            commitment: self.commitment.clone(),
            base: BlockId(1),
            base_hash: BlockHash::from(*self.hash.as_bytes()),
            mined_block: BlockHash::from([0xee; 32]),
            members: self.members.clone(),
            keys: Mutex::new(Default::default()),
        }))
    }

    fn key(&self) -> QuorumDbKey {
        let pro_txs: Vec<_> = self.members.iter().map(|m| m.pro_tx_hash).collect();
        quorum_db_key(LlmqType::LlmqTest, &self.hash, &pro_txs)
    }

    /// Every member's public key share and the secret key share `q` holds.
    fn snapshot(&self, q: &QuorumPtr) -> (Vec<[u8; 48]>, Option<[u8; 32]>) {
        let pro_txs: Vec<_> = self.members.iter().map(|m| m.pro_tx_hash).collect();
        let operator_keys: Vec<_> = self.members.iter().map(|m| m.operator_key()).collect();
        let view = QuorumView {
            llmq: &self.llmq,
            commitment: &self.commitment,
            members: &pro_txs,
            operator_keys: &operator_keys,
        };
        let mut keys = q.keys.lock().unwrap();
        let pks =
            (0..pro_txs.len()).map(|i| keys.pub_key_share(&view, i).to_bytes(SCHEME)).collect();
        (pks, keys.sk_share().map(|sk| sk.to_bytes(SCHEME)))
    }

    /// The quorum the DKG build path gives this node: `BuildQuorumContributions` over the
    /// dealers' vectors and this member's contributions.
    fn dkg_built(&self) -> QuorumPtr {
        let contributions = TestContributions {
            calls: Arc::default(),
            result: Some((
                (0..self.members.len() as u16).collect(),
                self.dealer_vvecs.clone(),
                self.contributions[ME].clone(),
            )),
        };
        let q = self.quorum();
        manager(TestStore::default(), contributions, self.me(), TestExecutor::default())
            .built(&q, false)
            .unwrap();
        q
    }

    fn item(&self, pro_tx: ProTxHash, vvec: &[BlsPkValue], share: &BlsSkValue) -> Value {
        item(LlmqType::LlmqTest.to_base(), &self.hash, &pro_tx, vvec, share)
    }

    fn mine(&self) -> Value {
        self.item(self.pro_tx(ME), &self.vvec, &self.shares[ME])
    }
}

fn item(
    llmq_type: u8,
    hash: &QuorumHash,
    pro_tx: &ProTxHash,
    vvec: &[BlsPkValue],
    share: &BlsSkValue,
) -> Value {
    json!({
        "llmq_type": llmq_type,
        "quorum_hash": hash.to_string(),
        "pro_tx": pro_tx.to_string(),
        "vvec": vvec.iter().map(|k| hex(&k.to_bytes(SCHEME))).collect::<Vec<_>>(),
        "sk_share": hex(&share.to_bytes(SCHEME)),
        "provenance": {
            "source_slot": "twin-a",
            "source_exe_sha256": "5eed",
            "method": "qwatch-qgetdata-vvec + quorum-info-sk",
            "tip_height": 321,
            "captured_seq": 7
        }
    })
}

fn file(items: Vec<Value>) -> String {
    json!({ "v": 1, "items": items }).to_string()
}

fn manager(
    store: TestStore,
    contributions: TestContributions,
    me: TestMasternode,
    executor: TestExecutor,
) -> Manager {
    QuorumManager::new(setup(Network::Regtest), SCHEME, store, contributions, me, executor)
}

/// A manager whose view has the fixture's quorum mined and, with `cached`, built (a failed DKG
/// build: no keys) and cached.
struct Node {
    manager: Manager,
    store: TestStore,
    executor: TestExecutor,
    view: Arc<TestView>,
    quorum: Option<QuorumPtr>,
}

fn node(f: &Fixture, me: TestMasternode, cached: bool) -> Node {
    let store = TestStore::default();
    let executor = TestExecutor::default();
    let view = Arc::new(TestView::default());
    let manager = manager(store.clone(), TestContributions::default(), me, executor.clone())
        .with_share_seam_view(view.clone());
    view.mine(&f.quorum());
    let quorum = cached.then(|| {
        let q = f.quorum();
        manager.built(&q, true).unwrap();
        view.cache(&q);
        q
    });
    Node {
        manager,
        store,
        executor,
        view,
        quorum,
    }
}

#[test]
fn accepts_own_share_loader_then_build() {
    let f = Fixture::new();
    let n = node(&f, f.me(), false);
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Ok(()));
    assert_eq!(report.accepted(), 1);
    assert_eq!(n.store.writes(), 1, "an uncached quorum gets only its row");
    assert_eq!(n.executor.len(), 0, "nothing cached to warm");

    // The later build reads the row through load_or_build_contributions.
    let contributions = TestContributions::default();
    let q = f.quorum();
    manager(n.store.clone(), contributions.clone(), f.me(), TestExecutor::default())
        .built(&q, true)
        .unwrap();
    assert_eq!(contributions.calls.load(Ordering::Relaxed), 0, "the row skips the DKG inputs");
    let dkg = f.snapshot(&f.dkg_built());
    assert!(dkg.1.is_some(), "the DKG build has this node's share");
    assert_eq!(f.snapshot(&q), dkg);
}

#[test]
fn accepts_own_share_build_then_loader() {
    let f = Fixture::new();
    let n = node(&f, f.me(), true);
    let q = n.quorum.clone().unwrap();
    assert!(!q.keys.lock().unwrap().has_vvec(), "the failed build left no keys");
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Ok(()));
    assert_eq!(n.executor.len(), 1, "the installed quorum is queued for warming");
    let dkg = f.snapshot(&f.dkg_built());
    assert_eq!(f.snapshot(&q), dkg, "the cached keys are installed");

    // The row written is the one the other order writes, so a restart reads the same keys.
    let restarted = f.quorum();
    manager(n.store.clone(), TestContributions::default(), f.me(), TestExecutor::default())
        .built(&restarted, false)
        .unwrap();
    assert_eq!(f.snapshot(&restarted), dkg);
    n.executor.run_all();
    assert_eq!(f.snapshot(&q), dkg);
}

/// Loads `items` into a node whose quorum is cached with empty keys, then into one where it is
/// not cached, and checks the item is rejected for `want` with neither the keys nor the store
/// changed.
fn assert_rejected(f: &Fixture, me: TestMasternode, items: Vec<Value>, want: Rejection) {
    for cached in [true, false] {
        let n = node(f, me, cached);
        let writes = n.store.writes();
        let report = n.manager.load_test_quorum_shares(&file(items.clone())).unwrap();
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].outcome, Err(want.clone()), "cached={cached}");
        assert_eq!(n.store.writes(), writes, "no store write (cached={cached})");
        assert!(n.store.row(&f.key()).is_none(), "no row (cached={cached})");
        assert_eq!(n.executor.len(), 0, "no warming (cached={cached})");
        if let Some(q) = n.quorum {
            let keys = q.keys.lock().unwrap();
            assert!(!keys.has_vvec(), "the cached keys have no vector");
            assert!(keys.sk_share().is_none(), "the cached keys have no share");
        }
    }
}

#[test]
fn rejects_a_vector_whose_hash_is_not_the_mined_one() {
    let f = Fixture::new();
    let wrong = (*f.dealer_vvecs[0]).clone();
    assert_eq!(wrong.len(), f.vvec.len());
    let item = f.item(f.pro_tx(ME), &wrong, &f.shares[ME]);
    assert_rejected(&f, f.me(), vec![item], Rejection::VvecHashMismatch);
}

#[test]
fn rejects_a_vector_of_the_wrong_length() {
    let f = Fixture::new();
    let mut long = f.vvec.clone();
    long.push(f.vvec[0].clone());
    let item = f.item(f.pro_tx(ME), &long, &f.shares[ME]);
    let expected = usize::from(f.llmq.threshold);
    let want = Rejection::WrongVvecLength {
        expected,
        got: expected + 1,
    };
    assert_rejected(&f, f.me(), vec![item], want);
    let short = f.item(f.pro_tx(ME), &f.vvec[..1], &f.shares[ME]);
    let want = Rejection::WrongVvecLength {
        expected,
        got: 1,
    };
    assert_rejected(&f, f.me(), vec![short], want);
}

#[test]
fn rejects_another_members_share() {
    let f = Fixture::new();
    let other = (ME + 1) % f.members.len();
    let item = f.item(f.pro_tx(other), &f.vvec, &f.shares[other]);
    assert_rejected(&f, f.me(), vec![item], Rejection::NotOwnProTx);
    // Another member's share under this node's own proTxHash fails the share check.
    let item = f.item(f.pro_tx(ME), &f.vvec, &f.shares[other]);
    assert_rejected(&f, f.me(), vec![item], Rejection::ShareMismatch);
}

#[test]
fn rejects_a_non_member() {
    let f = Fixture::new();
    let outsider = dmn(99, MnType::Regular).pro_tx_hash;
    let item = f.item(outsider, &f.vvec, &f.shares[ME]);
    assert_rejected(&f, TestMasternode(Some(outsider)), vec![item], Rejection::NotValidMember);
}

#[test]
fn rejects_an_unmined_quorum() {
    let f = Fixture::new();
    let unmined = QuorumHash::from([0x5a; 32]);
    let item = item(LlmqType::LlmqTest.to_base(), &unmined, &f.pro_tx(ME), &f.vvec, &f.shares[ME]);
    assert_rejected(&f, f.me(), vec![item], Rejection::NotMined);
}

#[test]
fn rejects_a_wrong_type() {
    let f = Fixture::new();
    let platform = LlmqType::LlmqTestPlatform;
    assert!(setup(Network::Regtest).consensus.llmq(platform).is_some());
    let item = item(platform.to_base(), &f.hash, &f.pro_tx(ME), &f.vvec, &f.shares[ME]);
    assert_rejected(&f, f.me(), vec![item], Rejection::NotMined);
    let item = item_with(&f, |item| item["llmq_type"] = json!(250));
    assert_rejected(&f, f.me(), vec![item], Rejection::UnknownLlmqType);
}

fn item_with(f: &Fixture, edit: impl FnOnce(&mut Value)) -> Value {
    let mut item = f.mine();
    edit(&mut item);
    item
}

#[test]
fn rejects_an_undecodable_vector_element() {
    let f = Fixture::new();
    let not_a_point = item_with(&f, |item| item["vvec"][1] = json!("ff".repeat(48)));
    let want = Rejection::UndecodableVvecElement {
        index: 1,
    };
    assert_rejected(&f, f.me(), vec![not_a_point], want.clone());
    let not_hex = item_with(&f, |item| item["vvec"][1] = json!("zz".repeat(48)));
    assert_rejected(&f, f.me(), vec![not_hex], want.clone());
    let short = item_with(&f, |item| item["vvec"][1] = json!("11".repeat(47)));
    assert_rejected(&f, f.me(), vec![short], want);
    // Core's Unserialize: a key canonical only under the opposite scheme, and the all-zero key,
    // are the invalid key, not a decode failure; the vector then fails its hash.
    let legacy = hex(&f.vvec[1].to_bytes(BlsSchemeKind::Legacy));
    assert_ne!(legacy, hex(&f.vvec[1].to_bytes(SCHEME)));
    let opposite = item_with(&f, |item| item["vvec"][1] = json!(legacy));
    assert_rejected(&f, f.me(), vec![opposite], Rejection::VvecHashMismatch);
    let zero = item_with(&f, |item| item["vvec"][1] = json!("00".repeat(48)));
    assert_rejected(&f, f.me(), vec![zero], Rejection::VvecHashMismatch);
}

#[test]
fn rejects_an_undecodable_share() {
    let f = Fixture::new();
    for bad in ["ff".repeat(32), "zz".repeat(32), "01".repeat(31), String::new()] {
        let item = item_with(&f, |item| item["sk_share"] = json!(bad));
        assert_rejected(&f, f.me(), vec![item], Rejection::UndecodableShare);
    }
    // The all-zero share is Core's invalid key, which SetSecretKeyShare refuses.
    let zero = item_with(&f, |item| item["sk_share"] = json!("00".repeat(32)));
    assert_rejected(&f, f.me(), vec![zero], Rejection::ShareMismatch);
}

#[test]
fn a_store_write_error_rejects_an_uncached_item() {
    let f = Fixture::new();
    let n = node(&f, f.me(), false);
    n.store.0.lock().unwrap().write_error = Some("disk full".into());
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Err(Rejection::StoreWrite("disk full".into())));
    assert!(n.store.row(&f.key()).is_none());
}

#[test]
fn a_store_write_error_leaves_a_cached_quorums_keys() {
    let f = Fixture::new();
    let n = node(&f, f.me(), true);
    let q = n.quorum.clone().unwrap();
    let wrong = stale_cached(&f, &q);
    let before = f.snapshot(&q);
    assert_eq!(before.0, wrong.0, "the stale keys are cached");
    n.store.0.lock().unwrap().write_error = Some("disk full".into());
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Err(Rejection::StoreWrite("disk full".into())));
    assert!(n.store.row(&f.key()).is_none());
    assert_eq!(n.executor.len(), 0, "no warming");
    assert_eq!(f.snapshot(&q), before, "the cached keys are unchanged");
}

/// Installs into `q` the keys a build over only the first two dealers gives (a vector that is
/// not the mined one, with its consistent share), runs their warming, and returns their snapshot.
fn stale_cached(f: &Fixture, q: &QuorumPtr) -> (Vec<[u8; 48]>, Option<[u8; 32]>) {
    let contributions = TestContributions {
        calls: Arc::default(),
        result: Some((vec![0, 1], f.dealer_vvecs[..2].to_vec(), f.contributions[ME][..2].to_vec())),
    };
    let executor = TestExecutor::default();
    manager(TestStore::default(), contributions, f.me(), executor.clone()).built(q, true).unwrap();
    assert_eq!(executor.len(), 1);
    executor.run_all();
    let stale = f.snapshot(q);
    assert!(stale.1.is_some(), "the stale build has a share");
    assert_ne!(stale, f.snapshot(&f.dkg_built()), "the stale keys differ from the DKG's");
    stale
}

#[test]
fn replaces_stale_cached_keys_whole() {
    let f = Fixture::new();
    let n = node(&f, f.me(), true);
    let q = n.quorum.clone().unwrap();
    stale_cached(&f, &q);
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Ok(()));
    let dkg = f.snapshot(&f.dkg_built());
    assert_eq!(f.snapshot(&q), dkg, "every pub_key_share and the sk_share are the DKG's");
    n.executor.run_all();
    assert_eq!(f.snapshot(&q), dkg);

    let restarted = f.quorum();
    manager(n.store.clone(), TestContributions::default(), f.me(), TestExecutor::default())
        .built(&restarted, false)
        .unwrap();
    assert_eq!(f.snapshot(&restarted), dkg, "the written row gives the same keys");
}

#[test]
fn rejects_a_cached_object_with_another_commitment() {
    let f = Fixture::new();
    let n = node(&f, f.me(), false);
    let mut other = (*f.commitment).clone();
    other.set_version(4);
    assert_ne!(other, *f.commitment);
    let q = QuorumPtr(Arc::new(NodeQuorum {
        llmq_type: LlmqType::LlmqTest,
        commitment: Arc::new(other),
        base: BlockId(1),
        base_hash: BlockHash::from(*f.hash.as_bytes()),
        mined_block: BlockHash::from([0xee; 32]),
        members: f.members.clone(),
        keys: Mutex::new(Default::default()),
    }));
    assert_eq!(q.commitment.quorum_hash(), f.hash, "the same (type, hash)");
    n.view.cache(&q);
    let report = n.manager.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Err(Rejection::CachedCommitmentMismatch));
    assert_eq!(n.store.writes(), 0);
    assert_eq!(n.executor.len(), 0);
    let keys = q.keys.lock().unwrap();
    assert!(!keys.has_vvec() && keys.sk_share().is_none(), "the cached keys are unchanged");
}

#[test]
fn rejects_every_item_without_a_view() {
    let f = Fixture::new();
    let store = TestStore::default();
    let m = manager(store.clone(), TestContributions::default(), f.me(), TestExecutor::default());
    let report = m.load_test_quorum_shares(&file(vec![f.mine()])).unwrap();
    assert_eq!(report.items[0].outcome, Err(Rejection::NoView));
    assert_eq!(store.writes(), 0);
}

#[test]
fn refuses_a_non_regtest_chain() {
    let f = Fixture::new();
    for network in [Network::Main, Network::Testnet3, Network::Devnet] {
        let store = TestStore::default();
        let executor = TestExecutor::default();
        let view = Arc::new(TestView::default());
        view.mine(&f.quorum());
        let m = QuorumManager::new(
            setup(network),
            SCHEME,
            store.clone(),
            TestContributions::default(),
            f.me(),
            executor.clone(),
        )
        .with_share_seam_view(view.clone());
        let error = m.load_test_quorum_shares(&file(vec![f.mine()])).unwrap_err();
        assert!(matches!(error, LoadError::NotRegtest(_)), "{network:?}: {error}");
        assert_eq!(view.reads(), 0, "the view is not read");
        assert_eq!(store.writes(), 0);
        assert_eq!(executor.len(), 0);
    }
}

#[test]
fn refuses_a_malformed_file() {
    let f = Fixture::new();
    let good = f.mine();
    let mut without_pro_tx = good.clone();
    without_pro_tx.as_object_mut().unwrap().remove("pro_tx");
    let mut without_provenance_field = good.clone();
    without_provenance_field["provenance"].as_object_mut().unwrap().remove("captured_seq");
    let ill_typed = item_with(&f, |item| item["llmq_type"] = json!("100"));
    let out_of_range = item_with(&f, |item| item["llmq_type"] = json!(256));
    let short_hash = item_with(&f, |item| item["quorum_hash"] = json!("ab"));
    let vvec_number = item_with(&f, |item| item["vvec"][0] = json!(1));
    let files = [
        "not json".to_owned(),
        "[]".to_owned(),
        json!({ "v": 2, "items": [good.clone()] }).to_string(),
        json!({ "items": [good.clone()] }).to_string(),
        json!({ "v": 1 }).to_string(),
        json!({ "v": 1, "items": {} }).to_string(),
        // A malformed item refuses the whole file, the good item before it too.
        file(vec![good.clone(), without_pro_tx]),
        file(vec![without_provenance_field]),
        file(vec![ill_typed]),
        file(vec![out_of_range]),
        file(vec![short_hash]),
        file(vec![vvec_number]),
        "[".repeat(10_000),
    ];
    for json in files {
        let n = node(&f, f.me(), true);
        let reads = n.view.reads();
        let error = n.manager.load_test_quorum_shares(&json).unwrap_err();
        assert!(matches!(error, LoadError::Malformed(_)), "{error}");
        assert_eq!(n.view.reads(), reads, "the view is not read");
        assert_eq!(n.store.writes(), 0);
        assert!(!n.quorum.unwrap().keys.lock().unwrap().has_vvec());
    }
}

#[test]
fn hashes_are_read_in_core_rpc_order() {
    let f = Fixture::new();
    let mut reversed = *f.hash.as_bytes();
    reversed.reverse();
    let rpc_hex = hex(&reversed);
    assert_eq!(f.hash.to_string(), rpc_hex, "Display is Core's GetHex");
    let mut pro_tx = *f.pro_tx(ME).as_bytes();
    pro_tx.reverse();
    let pro_tx_hex = hex(&pro_tx);
    let n = node(&f, f.me(), false);
    let item = item_with(&f, |item| {
        item["quorum_hash"] = json!(rpc_hex);
        item["pro_tx"] = json!(pro_tx_hex);
    });
    let report = n.manager.load_test_quorum_shares(&file(vec![item])).unwrap();
    assert_eq!(report.items[0].quorum_hash, f.hash);
    assert_eq!(report.items[0].pro_tx, f.pro_tx(ME));
    assert_eq!(report.items[0].outcome, Ok(()));

    // Only quorum_hash is swapped here, to internal byte order, which is unmined; pro_tx's order
    // is pinned by the acceptance above.
    let item = item_with(&f, |item| item["quorum_hash"] = json!(hex(f.hash.as_bytes())));
    let report = n.manager.load_test_quorum_shares(&file(vec![item])).unwrap();
    assert_eq!(report.items[0].outcome, Err(Rejection::NotMined));
}

/// The messages `f` logs on this thread, as `dashd-masternode`'s `test_log::logged` captures
/// them.
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

#[test]
fn log_lines_carry_no_key_bytes() {
    let f = Fixture::new();
    let n = node(&f, f.me(), true);
    let wrong = f.item(f.pro_tx(ME), &f.dealer_vvecs[0], &f.shares[ME]);
    let (report, log) =
        logged(|| n.manager.load_test_quorum_shares(&file(vec![f.mine(), wrong])).unwrap());
    assert_eq!(report.accepted(), 1);
    let log = log.join("\n");
    let lines: Vec<_> =
        log.lines().filter(|l| l.contains("QuorumManager::LoadTestQuorumShares")).collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(lines[0].starts_with("DEBUG llmq "), "{}", lines[0]);
    assert!(lines[0].contains(" accepted reason[]"), "{}", lines[0]);
    assert!(lines[1].contains(" rejected reason[verification vector hash mismatch]"));
    for line in &lines {
        assert!(line.contains(&format!("llmqType[{}]", LlmqType::LlmqTest.to_base())));
        assert!(line.contains(&format!("quorumHash[{}]", f.hash)));
        assert!(line.contains(&format!("proTxHash[{}]", f.pro_tx(ME))));
        assert!(line.contains(r#"source_slot["twin-a"] source_exe_sha256["5eed"]"#));
        assert!(line.contains(r#"method["qwatch-qgetdata-vvec + quorum-info-sk"]"#));
        assert!(line.contains("tip_height[321] captured_seq[7]"));
    }
    // No run of 16 hex digits (8 bytes) from any share or vector key, at any offset, in lower or
    // upper case.
    let secrets = f.shares.iter().map(|s| hex(&s.to_bytes(SCHEME)));
    let keys = f.vvec.iter().chain(f.dealer_vvecs[0].iter()).map(|k| hex(&k.to_bytes(SCHEME)));
    for needle in secrets.chain(keys) {
        for window in needle.as_bytes().windows(16) {
            let window = std::str::from_utf8(window).unwrap();
            for case in [window.to_lowercase(), window.to_uppercase()] {
                assert!(!log.contains(&case), "key bytes {case} in the log: {log}");
            }
        }
    }
}

#[test]
fn never_panics_on_odd_items() {
    let f = Fixture::new();
    let n = node(&f, f.me(), true);
    let odd = [
        json!({ "v": 1, "items": [] }).to_string(),
        file(vec![item_with(&f, |item| item["vvec"] = json!([]))]),
        file(vec![item_with(&f, |item| item["vvec"] = json!(vec!["00".repeat(48); 2]))]),
        file(vec![item_with(&f, |item| item["llmq_type"] = json!(0))]),
        json!({ "v": u64::MAX, "items": [] }).to_string(),
        json!({ "v": -1, "items": [] }).to_string(),
        json!({ "v": 1.0, "items": [] }).to_string(),
    ];
    for json in odd {
        let _ = n.manager.load_test_quorum_shares(&json);
    }
    let q = n.quorum.unwrap();
    assert!(!q.keys.lock().unwrap().has_vvec());
}
