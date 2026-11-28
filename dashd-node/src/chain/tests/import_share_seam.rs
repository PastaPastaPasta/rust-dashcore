//! In-process share-seam loading on `loadblk` after the real active masternode startup `Init`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use dash_clock::{Clock, MockClock};
use dash_consensus::BlockId;
use dash_consensus::primitives::P2pCommitment;
use dash_dml::{Dmn, MnType, StoreError, testkit::dmn};
use dash_evo::llmq::LlmqCommitment;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::{
    BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, aggregate_secret_keys,
    build_quorum_verification_vector, generate_contributions, member_id,
};
use dash_llmq::dkg::wire::vvec_hash;
use dash_llmq::quorum::{CleanupCounts, QuorumDbKey, StoredContributions, quorum_db_key};
use dash_params::LlmqType;
use dash_pkc::bls::{BlsEagerPk, BlsEagerSig};
use dash_primitives::{BlockHash, DynBitset, QuorumHash, QuorumVvecHash};
use dash_types::Numeric;
use dashd_chainstate::chain::{RecordingAbort, genesis_block};
use dashd_chainstate::llmq::quorum::{NodeQuorum, QuorumPtr};
use dashd_masternode::active::{ActiveMasternode, State};
use dashd_quorums::{
    LoadError, LoadReport, LocalMasternode, MinedQuorum, NoDkgSessions, QuorumKeyStore,
    QuorumManager, Rejection, ShareSeamView, WarmExecutor,
};
use serde_json::{Value, json};

use super::{Chain, MemBlockFiles, bounded, fresh, imported, regtest, startup};
use crate::p2p::active_mn::tests::tip_worker::{Rig, list_with};
use crate::p2p::active_mn::{ActiveWiring, wire};

const SCHEME: BlsSchemeKind = BlsSchemeKind::Basic;

#[derive(Clone, Debug)]
struct Fault(String);

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// In-memory contributions store for share-seam tests.
#[derive(Clone, Default)]
struct TestStore(Arc<Mutex<BTreeMap<QuorumDbKey, StoredContributions>>>);

impl TestStore {
    fn row(&self, key: &QuorumDbKey) -> Option<StoredContributions> {
        self.0.lock().unwrap().get(key).cloned()
    }
}

impl QuorumKeyStore for TestStore {
    type Error = Fault;

    fn read(&self, key: &QuorumDbKey) -> Result<Option<StoredContributions>, Fault> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }

    fn write(&self, key: &QuorumDbKey, row: &StoredContributions) -> Result<(), Fault> {
        self.0.lock().unwrap().insert(*key, row.clone());
        Ok(())
    }

    fn erase_except(&self, _: &BTreeSet<QuorumDbKey>) -> Result<CleanupCounts, Fault> {
        Ok(CleanupCounts::default())
    }
}

/// No-op warm executor for uncached share-seam loads.
struct NoopExecutor;

impl WarmExecutor for NoopExecutor {
    fn spawn_warm(&self, _: Box<dyn FnOnce() + Send>) {}
}

/// `LocalMasternode` backed by the node's live `ActiveMasternode`.
struct ActiveMn(Arc<ActiveMasternode>);

impl LocalMasternode for ActiveMn {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.0.pro_tx_hash().map(ProTxHash::from)
    }
}

type ViewKey = (u8, [u8; 32]);

/// In-memory share-seam view returning one mined quorum.
#[derive(Default)]
struct TestView {
    mined: Mutex<BTreeMap<ViewKey, MinedQuorum>>,
}

impl TestView {
    fn mine(&self, q: &QuorumPtr) {
        self.mined.lock().unwrap().insert(
            (q.llmq_type.to_base(), *q.commitment.quorum_hash().as_bytes()),
            MinedQuorum {
                commitment: q.commitment.clone(),
                members: q.members.clone(),
                scheme: SCHEME,
            },
        );
    }
}

impl ShareSeamView for TestView {
    fn mined_quorum(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<Option<MinedQuorum>, StoreError> {
        Ok(self.mined.lock().unwrap().get(&(llmq_type.to_base(), *quorum_hash.as_bytes())).cloned())
    }

    fn cached_quorum(&self, _: LlmqType, _: &QuorumHash) -> Option<QuorumPtr> {
        None
    }
}

type TestManager = QuorumManager<TestStore, NoDkgSessions, ActiveMn, NoopExecutor>;

fn sk(seed: u8) -> BlsSkValue {
    BlsSkValue::from_canonical(&[seed; 32], SCHEME).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A synthetic mined `LLMQ_TEST` quorum whose first member is the active masternode.
struct Fixture {
    hash: QuorumHash,
    members: Arc<[Arc<Dmn>]>,
    commitment: Arc<P2pCommitment>,
    vvec: Vec<BlsPkValue>,
    shares: Vec<BlsSkValue>,
}

impl Fixture {
    fn new(me: Arc<Dmn>) -> Self {
        let llmq = *regtest().consensus.llmq(LlmqType::LlmqTest).unwrap();
        let size = usize::from(llmq.size);
        let threshold = usize::from(llmq.threshold);
        let mut members = Vec::with_capacity(size);
        members.push(me);
        for n in 2..=size {
            members.push(dmn(n as u8, MnType::Regular));
        }
        let members: Arc<[Arc<Dmn>]> = members.into();
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
        let hash = QuorumHash::from(std::array::from_fn::<u8, 32, _>(|i| i as u8 + 1));
        let mut bits = vec![0u8; size.div_ceil(8)];
        for i in 0..size {
            bits[i / 8] |= 1 << (i % 8);
        }
        let bits = DynBitset::new(size as u64, bits).unwrap();
        let commitment = Arc::new(P2pCommitment::new(LlmqCommitment::new(
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
        )));
        Self {
            hash,
            members,
            commitment,
            vvec,
            shares,
        }
    }

    fn pro_tx(&self, i: usize) -> ProTxHash {
        self.members[i].pro_tx_hash
    }

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

    fn mine(&self) -> Value {
        item(LlmqType::LlmqTest.to_base(), &self.hash, &self.pro_tx(0), &self.vvec, &self.shares[0])
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

/// Shared harness: a real active masternode rig, its mined quorum fixture, and a `QuorumManager`
/// reading that active masternode's `pro_tx_hash()`.
struct Harness {
    rig: Rig,
    wiring: ActiveWiring,
    fixture: Fixture,
    store: TestStore,
    manager: Arc<TestManager>,
    json: String,
}

impl Harness {
    fn new() -> Self {
        let rig = Rig::new();
        let list = list_with(&rig.active);
        let me = Arc::clone(list.iter().next().expect("active masternode in list"));
        rig.set(5, Ok(list));
        let wiring = wire(rig.tip(Some(BlockId(5)))).expect("wire active masternode");
        let fixture = Fixture::new(me);
        let json = file(vec![fixture.mine()]);
        let store = TestStore::default();
        let view = Arc::new(TestView::default());
        view.mine(&fixture.quorum());
        let manager = Arc::new(
            QuorumManager::new(
                regtest(),
                store.clone(),
                NoDkgSessions,
                ActiveMn(Arc::clone(&rig.active)),
                NoopExecutor,
            )
            .with_share_seam_view(view),
        );
        Self {
            rig,
            wiring,
            fixture,
            store,
            manager,
            json,
        }
    }
}

/// Running the share-seam loader from `after_mn_init` on `loadblk`, right after the real
/// masternode startup `Init`, sees the masternode's `proTxHash`, accepts the share and writes the
/// contributions row.
#[test]
fn import_loads_own_quorum_share_after_masternode_init() {
    let Harness {
        rig,
        wiring,
        fixture,
        store,
        manager,
        json,
    } = Harness::new();
    let ActiveWiring {
        listener,
        worker,
        startup: mn_startup,
        ..
    } = wiring;
    assert_eq!(rig.active.pro_tx_hash(), None);

    let p = regtest();
    let clock: Arc<dyn Clock> =
        Arc::new(MockClock::at(i64::from(genesis_block(&p).header().raw().time())));
    let abort = Arc::new(RecordingAbort::default());
    let parts = fresh(&p, &clock, &abort, Box::new(MemBlockFiles::default()));
    let loaded: Arc<Mutex<Option<Result<LoadReport, LoadError>>>> = Arc::default();
    let result = Arc::clone(&loaded);
    bounded(move || {
        let mut chain = Chain::start(parts).expect("V starts");
        chain.set_masternode_startup(mn_startup);
        chain.set_after_mn_init(Box::new(move || {
            *result.lock().unwrap() = Some(manager.load_test_quorum_shares(&json));
        }));
        startup(&mut chain);
        imported(&mut chain);
        chain.stop();
        chain.flush_background_callbacks();
    });
    drop(listener);
    worker.join().unwrap();

    assert_eq!(rig.active.snapshot().state, State::Ready);
    assert_eq!(rig.active.pro_tx_hash().map(ProTxHash::from), Some(fixture.pro_tx(0)));
    let report = loaded.lock().unwrap().take().expect("after_mn_init ran").expect("valid file");
    assert_eq!(report.accepted(), 1);
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].outcome, Ok(()));
    assert!(store.row(&fixture.key()).is_some());
    assert!(rig.abort.reports().is_empty());
    assert!(abort.reports().is_empty());
}

/// Fail-first: calling the loader before `Chain::import` (step-7 placement) runs before the
/// masternode's startup `Init` sets `proTxHash`, so the item is rejected with `NotOwnProTx`.
#[test]
fn import_load_before_masternode_init_rejects_with_not_own_pro_tx() {
    let Harness {
        rig,
        wiring,
        fixture,
        store,
        manager,
        json,
    } = Harness::new();
    assert_eq!(rig.active.pro_tx_hash(), None);

    let report = manager.load_test_quorum_shares(&json).expect("valid file");
    assert_eq!(report.accepted(), 0);
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].outcome, Err(Rejection::NotOwnProTx));
    assert!(store.row(&fixture.key()).is_none());

    drop(wiring.listener);
    drop(wiring.startup);
    wiring.worker.join().unwrap();
}
