//! Shared doubles for the S3 tests: a non-rotated InstantSend chain (regtest with
//! `-llmqtestinstantsenddip0024=llmq_test_instantsend`, as the LQ3a capture runs) whose quorums
//! hold keys the tests sign with, and recording peer, processor and recovered-signature seams.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_clock::MockClock;
use dash_consensus::BlsCtx;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_evo::llmq::{LlmqCommitment, VerifiedQuorum, commitment_version};
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::locks::{
    QuorumScan, QuorumScanView, QuorumStatus, ScanLookup, isdlock_hash, islock_request_id,
    sign_hash,
};
use dash_kv::MemoryEngine;
use dash_params::{ChainOption, ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_pkc::bls::BlsSchemeKind;
use dash_pkc::bls::BlsSigBytes;
use dash_pkc::bls::core::{BlsSkValue, public_key, sign};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric;
use dash_types::codec::BaseCodec;
use rand_core::{TryCryptoRng, TryRng};

use super::batch::{IsPassView, PassBody};
use super::chain::GateSource;
use super::db::{IsDb, SCHEMA};
use super::manager::{IsManager, NodeId, PendingIsLockEntry};
use super::net::{IsNetError, NetInstantSend};
use super::seams::{IsLockProcessor, IsPeers, IsSignerSeam, RecoveredSigs, SignRequestId};
use super::{IsGate, IsLockHash, LockedTx};

pub const BASIC: BlsCtx = BlsCtx::BASIC;

/// Regtest whose InstantSend type is the non-rotated `llmq_test_instantsend` (interval 24): a
/// lock names its quorum by the cycle hash, the same on both passes.
pub fn params() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::LlmqTestInstantSendDip0024, "llmq_test_instantsend")
        .arg(ChainOption::LlmqTestInstantSendParams, "4:3")
        .build()
        .expect("regtest")
}

pub fn is_type() -> LlmqType {
    params().consensus.llmq_type_dip0024_instant_send
}

/// A seeded generator for the verifier's weights (not secure; tests only).
pub struct TestRng(pub u64);

impl TryRng for TestRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok((self.try_next_u64()? >> 32) as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        Ok(self.0)
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for chunk in dst.chunks_mut(8) {
            let word = self.try_next_u64()?.to_le_bytes();
            chunk.copy_from_slice(&word[..chunk.len()]);
        }
        Ok(())
    }
}

impl TryCryptoRng for TestRng {}

pub fn secret(n: u8) -> BlsSkValue {
    let mut bytes = [0; 32];
    bytes[31] = n;
    BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).expect("a scalar")
}

/// Quorum `n`: cycle block `[n; 32]` at height `24 * n`, key `secret(n)`.
pub fn quorum_hash(n: u8) -> [u8; 32] {
    [n; 32]
}

fn mined_quorum(n: u8) -> VerifiedQuorum {
    let mut bytes = Vec::new();
    commitment_version(false, true).encode(&mut bytes);
    bytes.push(is_type().to_base());
    bytes.extend_from_slice(&quorum_hash(n));
    bytes.extend_from_slice(&[0, 0]);
    bytes.extend_from_slice(&public_key(&secret(n)).to_bytes(BlsSchemeKind::Basic));
    bytes.extend_from_slice(&[0; 32 + 96 + 96]);
    VerifiedQuorum::mined(&LlmqCommitment::decode(&mut &bytes[..]).expect("a commitment"))
}

/// The chain the passes read: known cycle blocks and the quorums they name.
#[derive(Clone)]
pub struct FakeView {
    tip: u32,
    pub heights: BTreeMap<[u8; 32], u32>,
    pub quorums: BTreeMap<[u8; 32], VerifiedQuorum>,
}

impl FakeView {
    /// Quorums `1..=n` with their cycle blocks, at tip 200.
    pub fn with_quorums(n: u8) -> FakeView {
        let mut view = FakeView {
            tip: 200,
            heights: BTreeMap::new(),
            quorums: BTreeMap::new(),
        };
        for q in 1..=n {
            view.add_quorum(q);
        }
        view
    }

    pub fn add_quorum(&mut self, n: u8) {
        self.heights.insert(quorum_hash(n), 24 * u32::from(n));
        self.quorums.insert(quorum_hash(n), mined_quorum(n));
    }

    /// The cycle block stays known, its quorum gone: Core's `GetQuorum` is null.
    pub fn drop_quorum(&mut self, n: u8) {
        self.quorums.remove(&quorum_hash(n));
    }
}

impl QuorumScanView for FakeView {
    type Error = String;

    fn tip_height(&self) -> u32 {
        self.tip
    }

    fn scan(&self, _: LlmqType, _: u32) -> Result<ScanLookup, String> {
        Ok(ScanLookup::Held(QuorumScan::default()))
    }

    fn scan_at_tip(&self, _: LlmqType, _: usize) -> Result<ScanLookup, String> {
        Ok(ScanLookup::Held(QuorumScan::default()))
    }

    fn block_height(&self, hash: &BlockHash) -> Result<Option<u32>, String> {
        Ok(self.heights.get(hash.as_bytes()).copied())
    }

    fn quorum(&self, t: LlmqType, hash: &QuorumHash) -> Result<Option<QuorumStatus>, String> {
        assert_eq!(t, is_type());
        Ok(self.quorums.get(hash.as_bytes()).map(|q| QuorumStatus::Verified(q.clone())))
    }
}

/// A lock on `txid(tag)` naming quorum `q`, signed by `signer` (`q` for a valid signature).
pub fn lock_signed_by(tag: u32, q: u8, signer: u8) -> LockedTx {
    let mut txid = [0u8; 32];
    txid[..4].copy_from_slice(&tag.to_le_bytes());
    txid[31] = 0xee;
    let txid = TxHash::from(txid);
    let inputs = vec![OutPoint::new(txid, 7)];
    let id = islock_request_id(&inputs);
    let hash = sign_hash(is_type(), &QuorumHash::from(quorum_hash(q)), &id, txid.as_bytes());
    let sig = sign(&secret(signer), &hash, BlsSchemeKind::Basic).to_bytes(BlsSchemeKind::Basic);
    LockedTx::new(1, inputs, txid, quorum_hash(q).into(), sig.into())
}

pub fn lock(tag: u32, q: u8) -> LockedTx {
    lock_signed_by(tag, q, q)
}

/// `lock` with signature bytes no scheme reads (`sig.Get()` invalid).
pub fn unreadable(lock: &LockedTx) -> LockedTx {
    LockedTx::new(
        lock.version(),
        lock.inputs().clone(),
        lock.txid(),
        lock.cycle_hash(),
        [0; 96].into(),
    )
}

pub fn entry(node: NodeId, lock: LockedTx) -> PendingIsLockEntry {
    PendingIsLockEntry {
        node_id: node,
        islock_hash: isdlock_hash(&lock),
        islock: Arc::new(lock),
        legacy: false,
    }
}

/// What the seams saw, in one order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Event {
    Misbehaving(NodeId, i32),
    Erase(NodeId, IsLockHash),
    Forget(IsLockHash),
    Process(NodeId, IsLockHash),
    /// The pushed recovered signature's message hash: the lock's txid.
    PushRecSig([u8; 32]),
    Retry(usize),
}

pub type Log = Arc<Mutex<Vec<Event>>>;

pub struct Peers(pub Log);

impl IsPeers for Peers {
    fn misbehaving(&self, node: NodeId, score: i32) {
        self.0.lock().unwrap().push(Event::Misbehaving(node, score));
    }

    fn erase_object_request(&self, node: NodeId, hash: &IsLockHash) {
        self.0.lock().unwrap().push(Event::Erase(node, *hash));
    }

    fn forget_object_request(&self, hash: &IsLockHash) {
        self.0.lock().unwrap().push(Event::Forget(*hash));
    }
}

/// S4's processor: records, optionally marks the lock's request id as having a recovered
/// signature (as a signing manager that recovered it meanwhile), optionally fails.
pub struct Processor {
    pub log: Log,
    pub recsigs: Arc<RecSigs>,
    pub store_id_on_process: AtomicBool,
    pub fail: AtomicBool,
}

impl IsLockProcessor for Processor {
    fn process_instant_send_lock(
        &self,
        from: NodeId,
        hash: &IsLockHash,
        islock: &Arc<LockedTx>,
        _legacy: bool,
    ) -> dash_kv::Result<()> {
        self.log.lock().unwrap().push(Event::Process(from, *hash));
        if self.store_id_on_process.load(Ordering::SeqCst) {
            self.recsigs.has_ids.lock().unwrap().insert(islock_request_id(islock.inputs()));
        }
        if self.fail.swap(false, Ordering::SeqCst) {
            return Err(dash_kv::StoreError::engine("test", dash_kv::ErrorKind::Io, "injected"));
        }
        Ok(())
    }
}

/// `(id, msgHash)` → `(quorumHash, sig)`.
type StoredRecSigs = BTreeMap<(SignRequestId, [u8; 32]), (QuorumHash, BlsSigBytes)>;

/// The signing manager's recovered signatures.
#[derive(Default)]
pub struct RecSigs {
    pub log: Log,
    pub stored: Mutex<StoredRecSigs>,
    pub has_ids: Mutex<BTreeSet<SignRequestId>>,
}

impl RecSigs {
    /// Store `lock`'s recovered signature as quorum `q`'s with `sig`.
    pub fn store(&self, lock: &LockedTx, q: u8, sig: BlsSigBytes) {
        let id = islock_request_id(lock.inputs());
        self.stored
            .lock()
            .unwrap()
            .insert((id, *lock.txid().as_bytes()), (QuorumHash::from(quorum_hash(q)), sig));
        self.has_ids.lock().unwrap().insert(id);
    }
}

impl RecoveredSigs for RecSigs {
    fn truncate(&self, _: LlmqType, _: &SignRequestId) -> dash_kv::Result<()> {
        Ok(())
    }

    fn get_recovered_sig(
        &self,
        _: LlmqType,
        id: &SignRequestId,
        msg_hash: &[u8; 32],
    ) -> Option<(QuorumHash, BlsSigBytes)> {
        self.stored.lock().unwrap().get(&(*id, *msg_hash)).copied()
    }

    fn has_recovered_sig_for_id(&self, _: LlmqType, id: &SignRequestId) -> bool {
        self.has_ids.lock().unwrap().contains(id)
    }

    fn push_reconstructed_recovered_sig(&self, recsig: Arc<RecoveredSig>) {
        self.log.lock().unwrap().push(Event::PushRecSig(recsig.msg_hash));
    }
}

pub struct Signer(pub Log);

impl IsSignerSeam for Signer {
    fn process_txs(&self, _: Vec<Arc<dash_consensus::Tx>>, _: bool) {}
    fn retry(&self, txs: Vec<Arc<dash_consensus::Tx>>) {
        self.0.lock().unwrap().push(Event::Retry(txs.len()));
    }
    fn clear_inputs(&self, _: Vec<[u8; 32]>) {}
    fn clear_lock(&self, _: Arc<LockedTx>) {}
}

/// Each pass's view and flag: `views[i]` for the i-th pass (the last one repeats).
pub struct Passes {
    views: Vec<FakeView>,
    schemes: Vec<BlsCtx>,
    taken: Mutex<usize>,
}

impl Passes {
    pub fn new(views: Vec<FakeView>, schemes: Vec<BlsCtx>) -> Passes {
        Passes {
            views,
            schemes,
            taken: Mutex::new(0),
        }
    }

    pub fn one(view: FakeView) -> Passes {
        Self::new(vec![view], vec![BASIC])
    }

    pub fn taken(&self) -> usize {
        *self.taken.lock().unwrap()
    }
}

impl IsPassView for Passes {
    type Error = String;

    fn with_pass<T: Send + 'static>(
        &self,
        f: impl PassBody<String, T>,
    ) -> Result<T, IsNetError<String>> {
        let i = {
            let mut taken = self.taken.lock().unwrap();
            *taken += 1;
            *taken - 1
        };
        let view = &self.views[i.min(self.views.len() - 1)];
        f(view, self.schemes[i.min(self.schemes.len() - 1)])
    }
}

/// A NetInstantSend over a fresh store, with its seams.
pub struct Rig {
    pub net: NetInstantSend,
    pub log: Log,
    pub recsigs: Arc<RecSigs>,
    pub processor: Arc<Processor>,
    pub spork2: Arc<AtomicBool>,
}

impl Rig {
    pub fn new(signer: bool) -> Rig {
        Self::with_params(params(), signer)
    }

    pub fn with_params(params: ChainSetup, signer: bool) -> Rig {
        Self::build(params, signer, None)
    }

    /// A rig whose NetInstantSend reads and feeds `sigman`; the processor still marks
    /// [`Rig::recsigs`].
    pub fn with_sigman(sigman: Arc<dyn RecoveredSigs>) -> Rig {
        Self::build(params(), false, Some(sigman))
    }

    fn build(params: ChainSetup, signer: bool, sigman: Option<Arc<dyn RecoveredSigs>>) -> Rig {
        let log: Log = Arc::default();
        let recsigs = Arc::new(RecSigs {
            log: Arc::clone(&log),
            ..RecSigs::default()
        });
        let processor = Arc::new(Processor {
            log: Arc::clone(&log),
            recsigs: Arc::clone(&recsigs),
            store_id_on_process: AtomicBool::new(false),
            fail: AtomicBool::new(false),
        });
        let spork2 = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&spork2);
        let gate: GateSource =
            Arc::new(move || IsGate::new(flag.load(Ordering::SeqCst), false, false));
        let db = Arc::new(IsDb::open(Arc::new(MemoryEngine::new(&SCHEMA))).expect("a store"));
        let manager = Arc::new(IsManager::new(db, gate, Arc::new(MockClock::new())));
        let net = NetInstantSend::new(
            Arc::new(params),
            manager,
            Arc::new(Peers(Arc::clone(&log))),
            sigman.unwrap_or_else(|| Arc::clone(&recsigs) as Arc<dyn RecoveredSigs>),
            Arc::clone(&processor) as Arc<dyn IsLockProcessor>,
            signer.then(|| Arc::new(Signer(Arc::clone(&log))) as Arc<dyn IsSignerSeam>),
        );
        Rig {
            net,
            log,
            recsigs,
            processor,
            spork2,
        }
    }

    pub fn events(&self) -> Vec<Event> {
        self.log.lock().unwrap().clone()
    }

    /// The processed locks, in order.
    pub fn processed(&self) -> Vec<(NodeId, IsLockHash)> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Process(n, h) => Some((n, h)),
                _ => None,
            })
            .collect()
    }

    pub fn clear(&self) {
        self.log.lock().unwrap().clear();
    }
}
