//! [`IsSigner`] (PLAN-LK1c §4 LK1c-2a, 2b): the gates, `CheckCanLock`, the conflicting islock,
//! mempool versus retroactive signing, the input-lock votes, islock creation, the
//! recovered-signature routing, the retry and the clears.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use dash_consensus::primitives::canonical::ASSET_UNLOCK_PAYLOAD_SIZE;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxType};
use dash_consensus::{Tx, TxRef};
use dash_evo::locks::wire::RecoveredSig;
use dash_evo::locks::{isdlock_hash, islock_request_id};
use dash_kv::{ErrorKind, StoreError};
use dash_llmq::signing::api::{
    ListenerId, QuorumSigning, RecoveredSigListener, RecoveredSigResult, SelectedQuorum,
    SignRequest, SigningFault,
};
use dash_params::{ChainParamsBuilder, ChainSetup, LlmqType, Network, Release};
use dash_primitives::{BlockHash, QuorumHash};
use dash_types::Numeric as _;
use parking_lot::Mutex;

use super::{IsLockSink, IsReads, IsSigner, IsSignerParts, IsTxSource};
use crate::is::chain::test_log;
use crate::is::listener::input_lock_request_id;
use crate::is::seams::test_support::{InMemoryIsStore, IsStoreCall};
use crate::is::{IsLockHash, IsTipCache, LockedTx, lock_inputs};
use crate::signing::TxSource;
use crate::signing_fake::{FakeIsLockView, FakeSignShares};

pub(crate) const TIP: i32 = 100;

pub(crate) fn setup(network: Network) -> ChainSetup {
    ChainParamsBuilder::new(network, Release::V24_0).build().expect("chain setup")
}

/// Transactions by txid: `Some(block)` mined, `None` in the mempool; the bodies `get_tx` serves;
/// the `wait_synced` calls.
#[derive(Default)]
pub(crate) struct Txs(
    Mutex<HashMap<TxHash, Option<BlockHash>>>,
    Mutex<HashMap<TxHash, TxRef>>,
    AtomicUsize,
);

impl TxSource for Txs {
    fn get_transaction(&self, txid: &TxHash) -> dash_kv::Result<Option<Option<BlockHash>>> {
        Ok(self.0.lock().get(txid).copied())
    }
}

impl IsTxSource for Txs {
    fn get_tx(&self, txid: &TxHash) -> dash_kv::Result<Option<(TxRef, Option<BlockHash>)>> {
        let block = self.0.lock().get(txid).copied();
        Ok(self.1.lock().get(txid).cloned().zip(block))
    }

    fn wait_synced(&self) {
        self.2.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) struct Reads {
    pub(crate) synced: AtomicBool,
    mempool_signing: AtomicBool,
    pub(crate) heights: Mutex<HashMap<BlockHash, i32>>,
    pub(crate) chainlocked: Mutex<HashSet<BlockHash>>,
    ancestors: Mutex<HashMap<(BlockHash, i32), BlockHash>>,
}

impl IsReads for Reads {
    fn blockchain_synced(&self) -> bool {
        self.synced.load(Ordering::SeqCst)
    }

    fn mempool_signing_enabled(&self) -> bool {
        self.mempool_signing.load(Ordering::SeqCst)
    }

    fn block_height(&self, hash: &BlockHash) -> Option<i32> {
        self.heights.lock().get(hash).copied()
    }

    fn has_chain_lock(&self, height: i32, hash: &BlockHash) -> bool {
        self.heights.lock().get(hash) == Some(&height) && self.chainlocked.lock().contains(hash)
    }

    fn ancestor_hash(&self, hash: &BlockHash, height: i32) -> Option<BlockHash> {
        self.ancestors.lock().get(&(*hash, height)).copied()
    }
}

/// Every islock queued, in order.
#[derive(Default)]
pub(crate) struct Pending(Mutex<Vec<(IsLockHash, LockedTx)>>);

impl IsLockSink for Pending {
    fn try_emplace_pending(&self, hash: IsLockHash, lock: LockedTx) -> dash_kv::Result<()> {
        self.0.lock().push((hash, lock));
        Ok(())
    }
}

pub(crate) struct Rig {
    pub(crate) signer: Arc<IsSigner>,
    pub(crate) view: Arc<FakeIsLockView>,
    pub(crate) store: Arc<InMemoryIsStore>,
    pub(crate) signing: Arc<FakeSignShares>,
    pub(crate) txs: Arc<Txs>,
    pub(crate) reads: Arc<Reads>,
    tip: IsTipCache,
    aborts: Arc<Mutex<Vec<String>>>,
    pending: Arc<Pending>,
    pub(crate) llmq: LlmqType,
}

impl Rig {
    fn new(setup: &ChainSetup) -> Rig {
        let signing = FakeSignShares::new();
        Rig::signing_through(setup, signing.clone(), signing)
    }

    /// A rig whose signer signs through `through`, which wraps `signing`.
    fn signing_through(
        setup: &ChainSetup,
        signing: Arc<FakeSignShares>,
        through: Arc<dyn QuorumSigning>,
    ) -> Rig {
        let view = FakeIsLockView::new();
        let store = Arc::new(InMemoryIsStore::default());
        let txs = Arc::new(Txs::default());
        let reads = Arc::new(Reads {
            synced: AtomicBool::new(true),
            mempool_signing: AtomicBool::new(true),
            heights: Mutex::default(),
            chainlocked: Mutex::default(),
            ancestors: Mutex::default(),
        });
        let pending = Arc::new(Pending::default());
        let tip = IsTipCache::new();
        tip.set(TIP);
        let aborts = Arc::new(Mutex::new(Vec::new()));
        let abort_log = Arc::clone(&aborts);
        let signer = Arc::new(IsSigner::new(
            setup,
            IsSignerParts {
                view: view.clone(),
                store: store.clone(),
                signing: through,
                txs: txs.clone(),
                reads: reads.clone(),
                sink: pending.clone(),
                tip: tip.clone(),
                abort: Arc::new(move |e: &str| abort_log.lock().push(e.to_owned())),
            },
        ));
        let llmq = setup.consensus.llmq_type_dip0024_instant_send;
        Rig {
            signer,
            view,
            store,
            signing,
            txs,
            reads,
            tip,
            aborts,
            pending,
            llmq,
        }
    }

    pub(crate) fn regtest() -> Rig {
        Rig::new(&setup(Network::Regtest))
    }

    /// Records `parent` as mined at `height` and returns the block's hash.
    pub(crate) fn mine(&self, parent: TxHash, height: i32) -> BlockHash {
        let block = block_hash(height);
        self.reads.heights.lock().insert(block, height);
        self.txs.0.lock().insert(parent, Some(block));
        block
    }

    pub(crate) fn requests(&self) -> Vec<SignRequest> {
        self.signing.sign_requests()
    }

    pub(crate) fn input_ids(&self) -> HashSet<[u8; 32]> {
        self.signer.input_request_ids.lock().clone()
    }
}

fn block_hash(height: i32) -> BlockHash {
    let mut bytes = [0xb0; 32];
    bytes[..4].copy_from_slice(&height.to_le_bytes());
    BlockHash::from(bytes)
}

pub(crate) fn txid(tag: u32) -> TxHash {
    let mut bytes = [0; 32];
    bytes[..4].copy_from_slice(&tag.to_le_bytes());
    TxHash::from(bytes)
}

fn spending(version: i16, tx_type: TxType, inputs: &[OutPoint]) -> Tx {
    let inputs = inputs.iter().map(|i| TxIn::new(*i, Default::default(), u32::MAX)).collect();
    Tx::new(Transaction::new(version, tx_type, inputs, vec![], 0, vec![]))
}

/// A version-1 spend of output 0 of each parent.
pub(crate) fn child(parents: &[TxHash]) -> Tx {
    let inputs: Vec<_> = parents.iter().map(|p| OutPoint::new(*p, 0)).collect();
    spending(1, TxType::new(0), &inputs)
}

pub(crate) fn vote(rig: &Rig, tx: &Tx, retroactive: bool) -> SignRequest {
    let input = lock_inputs(tx)[0];
    SignRequest {
        llmq_type: rig.llmq,
        id: input_lock_request_id(&input),
        msg_hash: tx.txid().into(),
        quorum_hash: None,
        allow_resign: retroactive,
        allow_diff_msg: false,
    }
}

fn instantsend_lines(lines: &[String], txid: TxHash) -> Vec<String> {
    let needle = format!("txid={txid}");
    lines.iter().filter(|l| l.contains(&needle)).cloned().collect()
}

// Core: v24.0.2 src/instantsend/signing.cpp:362-364 ProcessTx
#[test]
fn nothing_is_signed_with_instantsend_disabled_or_the_chain_unsynced() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    let tx = child(&[txid(1)]);
    rig.view.set_enabled(false);
    rig.signer.process_tx(&tx, false);
    rig.view.set_enabled(true);
    rig.reads.synced.store(false, Ordering::SeqCst);
    rig.signer.process_tx(&tx, true);
    assert!(rig.requests().is_empty());
    assert!(rig.store.calls().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:366-368 ProcessTx
#[test]
fn nothing_is_signed_without_an_instantsend_quorum_type() {
    let mut setup = setup(Network::Regtest);
    setup.consensus.llmq_type_dip0024_instant_send = LlmqType::LlmqNone;
    let rig = Rig::new(&setup);
    rig.mine(txid(1), TIP - 10);
    rig.signer.process_tx(&child(&[txid(1)]), true);
    assert!(rig.requests().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:37-52 IsProviderTx, :195-201 CheckCanLock, :370-373
// ProcessTx; test/functional/p2p_instantsend.py:185-186
#[test]
fn provider_txs_are_not_locked() {
    let rig = Rig::regtest();
    for t in 1..=12u16 {
        let parent = txid(100 + u32::from(t));
        rig.mine(parent, TIP - 10);
        let tx = spending(3, TxType::new(t), &[OutPoint::new(parent, 0)]);
        let provider = matches!(t, 1..=4 | 10..=12);
        let ((), lines) = test_log::capture(|| rig.signer.process_tx(&tx, false));
        let expected: Vec<String> = if provider {
            vec![
                format!("DEBUG CheckCanLock -- txid={}: provider txs are not locked", tx.txid()),
                format!("DEBUG ProcessTx -- txid={}: CheckCanLock returned false", tx.txid()),
            ]
        } else {
            Vec::new()
        };
        if t != 9 {
            assert_eq!(instantsend_lines(&lines, tx.txid())[..expected.len()], expected[..], "{t}");
            let signed = rig.requests().iter().any(|r| r.msg_hash == <[u8; 32]>::from(tx.txid()));
            assert_eq!(signed, !provider, "type {t}");
        }
    }
}

// Core: v24.0.2 src/instantsend/signing.cpp:192-193 CheckCanLock, :214-219
// CheckCanLockAssetUnlock's refusal line; abstaining is the module's Q3 deviation
#[test]
fn asset_unlocks_abstain() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    let mut payload = vec![0; ASSET_UNLOCK_PAYLOAD_SIZE];
    payload[1..9].copy_from_slice(&42u64.to_le_bytes());
    let input = TxIn::new(OutPoint::new(txid(1), 0), Default::default(), u32::MAX);
    let tx = Tx::new(Transaction::new(3, TxType::AssetUnlock, vec![input], vec![], 0, payload));
    assert_eq!(lock_inputs(&tx).len(), 1);
    let ((), lines) = test_log::capture(|| rig.signer.process_tx(&tx, true));
    assert!(rig.requests().is_empty());
    assert!(rig.store.calls().is_empty());
    assert_eq!(
        instantsend_lines(&lines, tx.txid()),
        [
            format!(
                "DEBUG CheckCanLockAssetUnlock -- txid={}: asset unlock not lockable: phase H",
                tx.txid()
            ),
            format!("DEBUG ProcessTx -- txid={}: CheckCanLock returned false", tx.txid()),
        ]
    );
}

// Core: v24.0.2 src/instantsend/signing.cpp:202-205 CheckCanLock
#[test]
fn a_tx_without_inputs_is_not_locked() {
    let rig = Rig::regtest();
    rig.signer.process_tx(&child(&[]), true);
    assert!(rig.requests().is_empty());
    assert!(rig.store.calls().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:270-273 CheckCanLock
#[test]
fn a_locked_parent_lets_its_child_lock() {
    let rig = Rig::regtest();
    rig.txs.0.lock().insert(txid(1), None);
    rig.view.lock(txid(1));
    let tx = child(&[txid(1)]);
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.requests(), [vote(&rig, &tx, false)]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:275-282 CheckCanLock
#[test]
fn an_unlocked_mempool_parent_refuses() {
    let rig = Rig::regtest();
    rig.txs.0.lock().insert(txid(1), None);
    let tx = child(&[txid(1)]);
    let ((), lines) = test_log::capture(|| rig.signer.process_tx(&tx, false));
    assert!(rig.requests().is_empty());
    assert_eq!(
        instantsend_lines(&lines, tx.txid())[0],
        format!(
            "DEBUG CheckCanLock -- txid={}: parent mempool TX {} is not locked",
            tx.txid(),
            txid(1)
        )
    );
}

// Core: v24.0.2 src/instantsend/signing.cpp:285-293 CheckCanLock
#[test]
fn a_parent_found_nowhere_refuses() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    rig.signer.process_tx(&child(&[txid(1), txid(2)]), false);
    assert!(rig.requests().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:299-306 CheckCanLock
#[test]
fn a_parent_block_outside_the_index_refuses() {
    let rig = Rig::regtest();
    rig.txs.0.lock().insert(txid(1), Some(block_hash(7)));
    rig.signer.process_tx(&child(&[txid(1)]), false);
    assert!(rig.requests().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:311-319 CheckCanLock
#[test]
fn a_cached_tip_below_the_parent_block_refuses() {
    let rig = Rig::regtest();
    let block = rig.mine(txid(1), TIP + 1);
    rig.reads.chainlocked.lock().insert(block);
    let tx = child(&[txid(1)]);
    rig.signer.process_tx(&tx, false);
    assert!(rig.requests().is_empty());
    rig.tip.set(TIP + 1);
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.requests(), [vote(&rig, &tx, false)]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:321-329 CheckCanLock; regtest
// nInstantSendConfirmationsRequired = 2; test/functional/p2p_instantsend.py:230
#[test]
fn a_parent_younger_than_the_confirmations_needs_a_chainlock() {
    let rig = Rig::regtest();
    let block = rig.mine(txid(1), TIP);
    let tx = child(&[txid(1)]);
    let ((), lines) = test_log::capture(|| rig.signer.process_tx(&tx, false));
    assert!(rig.requests().is_empty());
    assert_eq!(
        instantsend_lines(&lines, tx.txid())[0],
        format!(
            "DEBUG CheckCanLock -- txid={}: outpoint {}-0 too new and not ChainLocked. nTxAge=1, \
             nInstantSendConfirmationsRequired=2",
            tx.txid(),
            txid(1)
        )
    );
    rig.reads.chainlocked.lock().insert(block);
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.requests().len(), 1);

    let aged = child(&[txid(2)]);
    rig.mine(txid(2), TIP - 1);
    rig.signer.process_tx(&aged, false);
    assert_eq!(rig.requests()[1], vote(&rig, &aged, false));
}

// Core: v24.0.2 src/instantsend/signing.cpp:321-329 CheckCanLock; mainnet
// nInstantSendConfirmationsRequired = 6
#[test]
fn mainnet_needs_six_confirmations() {
    let rig = Rig::new(&setup(Network::Main));
    rig.mine(txid(1), TIP - 4);
    rig.mine(txid(2), TIP - 5);
    rig.signer.process_tx(&child(&[txid(1)]), false);
    assert!(rig.requests().is_empty());
    let aged = child(&[txid(2)]);
    rig.signer.process_tx(&aged, false);
    assert_eq!(rig.requests(), [vote(&rig, &aged, false)]);
    assert_eq!(rig.llmq, LlmqType::Llmq60_75);
}

// Core: v24.0.2 src/instantsend/signing.cpp:375-381 ProcessTx
#[test]
fn a_conflicting_islock_refuses() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    let tx = child(&[txid(1)]);
    let other = LockedTx::new(1, lock_inputs(&tx), txid(9), Default::default(), [0; 96].into());
    let hash = dash_evo::locks::isdlock_hash(&other);
    rig.store.scripted.lock().unwrap().conflicting_lock.push_back(Ok(Some((hash, other))));
    rig.signer.process_tx(&tx, true);
    assert!(rig.requests().is_empty());
    assert_eq!(rig.store.calls(), [IsStoreCall::ConflictingLock(tx.txid())]);
}

// Core: v24.0.2 src/instantsend/instantsend.cpp:428-432 GetConflictingLock
#[test]
fn the_conflicting_islock_is_not_read_with_instantsend_disabled() {
    let rig = Rig::regtest();
    rig.view.set_enabled(false);
    let found = rig.signer.conflicting_lock(&child(&[txid(1)]));
    assert!(matches!(found, Ok(None)));
    assert!(rig.store.calls().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:383-387 ProcessTx, :442 TrySignInputLocks;
// test/functional/feature_llmq_is_retroactive.py:45-71
#[test]
fn without_mempool_signing_only_a_retroactive_tx_is_signed_and_may_resign() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    rig.reads.mempool_signing.store(false, Ordering::SeqCst);
    let tx = child(&[txid(1)]);
    rig.signer.process_tx(&tx, false);
    assert!(rig.requests().is_empty());
    rig.signer.process_tx(&tx, true);
    assert_eq!(rig.requests(), [vote(&rig, &tx, true)]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:436-446 TrySignInputLocks
#[test]
fn every_input_is_registered_and_voted_on_in_order() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    rig.mine(txid(2), TIP - 10);
    rig.signing.set_sign_result(Ok(false));
    let tx = child(&[txid(1), txid(2)]);
    rig.signer.process_tx(&tx, false);
    let ids: Vec<_> = lock_inputs(&tx).iter().map(input_lock_request_id).collect();
    let requested: Vec<_> = rig.requests().iter().map(|r| r.id).collect();
    assert_eq!(requested, ids);
    assert_eq!(rig.input_ids(), ids.into_iter().collect());
}

/// Records, at each sign call, whether the signer had already registered the request's id.
struct RegisteredFirst {
    fake: Arc<FakeSignShares>,
    signer: OnceLock<Weak<IsSigner>>,
    registered: Mutex<Vec<bool>>,
}

impl QuorumSigning for RegisteredFirst {
    fn async_sign_if_member(&self, req: &SignRequest) -> Result<bool, SigningFault> {
        let signer = self.signer.get().and_then(Weak::upgrade).expect("the signer");
        let registered = signer.input_request_ids.lock().contains(&req.id);
        self.registered.lock().push(registered);
        self.fake.async_sign_if_member(req)
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault> {
        self.fake.select_quorum_for_signing(t, id)
    }

    fn has_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.fake.has_recovered_sig(t, id, msg)
    }

    fn has_recovered_sig_for_id(&self, t: LlmqType, id: &[u8; 32]) -> Result<bool, SigningFault> {
        self.fake.has_recovered_sig_for_id(t, id)
    }

    fn has_recovered_sig_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        self.fake.has_recovered_sig_for_session(sign_hash)
    }

    fn has_recovered_sig_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.fake.has_recovered_sig_for_signing(t, id, msg)
    }

    fn get_recovered_sig(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        self.fake.get_recovered_sig(t, id, msg)
    }

    fn is_conflicting(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.fake.is_conflicting(t, id, msg)
    }

    fn get_vote_for_id(
        &self,
        t: LlmqType,
        id: &[u8; 32],
    ) -> Result<Option<[u8; 32]>, SigningFault> {
        self.fake.get_vote_for_id(t, id)
    }

    fn truncate_recovered_sig(&self, t: LlmqType, id: &[u8; 32]) -> Result<(), SigningFault> {
        self.fake.truncate_recovered_sig(t, id)
    }

    fn register_listener(&self, l: Arc<dyn RecoveredSigListener>) -> ListenerId {
        self.fake.register_listener(l)
    }

    fn unregister_listener(&self, id: ListenerId) {
        self.fake.unregister_listener(id)
    }
}

// Core: v24.0.2 src/instantsend/signing.cpp:439-442 TrySignInputLocks: the id is registered
// before AsyncSignIfMember, so a recovered sig that arrives during the call is routed (:109)
#[test]
fn each_input_id_is_registered_before_its_sign_call() {
    let fake = FakeSignShares::new();
    let probe = Arc::new(RegisteredFirst {
        fake: fake.clone(),
        signer: OnceLock::new(),
        registered: Mutex::default(),
    });
    let rig = Rig::signing_through(&setup(Network::Regtest), fake, probe.clone());
    let _ = probe.signer.set(Arc::downgrade(&rig.signer));
    rig.mine(txid(1), TIP - 10);
    rig.mine(txid(2), TIP - 10);
    rig.signer.process_tx(&child(&[txid(1), txid(2)]), false);
    assert_eq!(*probe.registered.lock(), [true, true]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:410-416 TrySignInputLocks
#[test]
fn a_vote_for_another_tx_refuses() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    rig.mine(txid(2), TIP - 10);
    let first = child(&[txid(1)]);
    rig.signer.process_tx(&first, false);
    let ids = rig.input_ids();
    let double_spend = child(&[txid(2), txid(1)]);
    rig.signer.process_tx(&double_spend, true);
    assert_eq!(rig.requests(), [vote(&rig, &first, false)]);
    assert_eq!(rig.input_ids(), ids);
}

// Core: v24.0.2 src/instantsend/signing.cpp:420-425 TrySignInputLocks
#[test]
fn a_conflicting_recovered_sig_refuses() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    rig.mine(txid(2), TIP - 10);
    let tx = child(&[txid(1), txid(2)]);
    let id = input_lock_request_id(&OutPoint::new(txid(2), 0));
    rig.signing.insert_recovered(recsig(rig.llmq, id, txid(9).into()));
    rig.signer.process_tx(&tx, true);
    assert!(rig.requests().is_empty());
    assert!(rig.input_ids().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:427-431 TrySignInputLocks
#[test]
fn all_inputs_voted_skips_signing_unless_retroactive() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    let tx = child(&[txid(1)]);
    let id = input_lock_request_id(&OutPoint::new(txid(1), 0));
    rig.signing.set_vote(rig.llmq, id, tx.txid().into());
    rig.signer.process_tx(&tx, false);
    assert!(rig.requests().is_empty());
    assert!(rig.input_ids().is_empty());
    rig.signer.process_tx(&tx, true);
    assert_eq!(rig.requests(), [vote(&rig, &tx, true)]);
}

// Core throws a dbwrapper_error out of ProcessTx; rdashd reports it once and stops the call.
#[test]
fn a_store_or_signing_fault_aborts_once_and_stopped_is_silent() {
    let rig = Rig::regtest();
    rig.mine(txid(1), TIP - 10);
    let tx = child(&[txid(1)]);
    let fault = StoreError::engine("test", ErrorKind::Io, "injected");
    rig.store.scripted.lock().unwrap().conflicting_lock.push_back(Err(fault));
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.aborts.lock().len(), 1);

    rig.signing.set_fault(Some(SigningFault::Storage("injected".into())));
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.aborts.lock().len(), 2);

    rig.signing.set_fault(Some(SigningFault::Stopped));
    rig.signer.process_tx(&tx, false);
    assert_eq!(rig.aborts.lock().len(), 2);
    assert!(rig.requests().is_empty());
}

fn recsig(llmq: LlmqType, id: [u8; 32], msg: [u8; 32]) -> RecoveredSig {
    signed(llmq, id, msg, [0; 96])
}

fn signed(llmq: LlmqType, id: [u8; 32], msg: [u8; 32], sig: [u8; 96]) -> RecoveredSig {
    let mut bytes = vec![llmq.to_base()];
    bytes.extend([7u8; 32]);
    bytes.extend(id);
    bytes.extend(msg);
    bytes.extend(sig);
    RecoveredSig::decode(&mut bytes.as_slice()).expect("a recovered sig")
}

const QUORUM: [u8; 32] = [0x51; 32];

fn dkg_interval(rig: &Rig) -> i32 {
    let setup = setup(Network::Regtest);
    i32::from(setup.consensus.llmq(rig.llmq).expect("the IS quorum").dkg_interval)
}

/// Mines every parent of `tx` deep and serves `tx` from the mempool to `get_tx`.
fn lockable(rig: &Rig, tx: &Tx) {
    for input in lock_inputs(tx) {
        rig.mine(input.txid(), TIP - 10);
    }
    rig.txs.0.lock().insert(tx.txid(), None);
    rig.txs.1.lock().insert(tx.txid(), Arc::new(Tx::new(tx.raw().clone())));
}

/// Selects a quorum for `tx`'s islock whose base block is 6 above a cycle start, and returns it
/// with that cycle start's hash.
fn select(rig: &Rig, tx: &Tx) -> (SelectedQuorum, BlockHash) {
    let interval = dkg_interval(rig);
    let quorum = SelectedQuorum {
        quorum_hash: QuorumHash::from(QUORUM),
        base_height: interval + 6,
    };
    let cycle = block_hash(interval);
    rig.reads.ancestors.lock().insert((BlockHash::from(QUORUM), interval), cycle);
    rig.signing.set_selected_quorum(rig.llmq, islock_id(tx), quorum);
    (quorum, cycle)
}

fn islock_id(tx: &Tx) -> [u8; 32] {
    islock_request_id(&lock_inputs(tx))
}

fn input_id(parent: TxHash) -> [u8; 32] {
    input_lock_request_id(&OutPoint::new(parent, 0))
}

/// Stores the recovered input lock of `parent`'s output 0 for `tx`, without notifying anyone.
fn recover_input(rig: &Rig, tx: &Tx, parent: TxHash) {
    rig.signing.insert_recovered(recsig(rig.llmq, input_id(parent), tx.txid().into()));
}

fn islock_request(rig: &Rig, tx: &Tx, quorum: SelectedQuorum) -> SignRequest {
    SignRequest {
        llmq_type: rig.llmq,
        id: islock_id(tx),
        msg_hash: tx.txid().into(),
        quorum_hash: Some(quorum.quorum_hash),
        allow_resign: false,
        allow_diff_msg: false,
    }
}

fn islock_requests(rig: &Rig) -> Vec<SignRequest> {
    rig.requests().into_iter().filter(|r| r.quorum_hash.is_some()).collect()
}

/// `tx` spending output 0 of `txid(1)`, with its input recovered and its islock in progress.
fn creating(rig: &Rig) -> (Tx, BlockHash) {
    let tx = child(&[txid(1)]);
    lockable(rig, &tx);
    let (_, cycle) = select(rig, &tx);
    recover_input(rig, &tx, txid(1));
    rig.signer.process_tx(&tx, false);
    assert_eq!(islock_requests(rig).len(), 1);
    (tx, cycle)
}

fn creating_is_empty(rig: &Rig) -> bool {
    let creating = rig.signer.creating.lock();
    creating.by_id.is_empty() && creating.by_txid.is_empty()
}

// Core: v24.0.2 src/instantsend/signing.cpp:461-466 TrySignInstantSendLock
#[test]
fn the_islock_waits_for_every_input_recsig() {
    let rig = Rig::regtest();
    let tx = child(&[txid(1), txid(2)]);
    lockable(&rig, &tx);
    let (quorum, _) = select(&rig, &tx);
    recover_input(&rig, &tx, txid(1));
    rig.signer.process_tx(&tx, false);
    assert!(islock_requests(&rig).is_empty());
    recover_input(&rig, &tx, txid(2));
    rig.signer.process_tx(&tx, false);
    assert_eq!(islock_requests(&rig), [islock_request(&rig, &tx, quorum)]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:473-475 TrySignInstantSendLock
#[test]
fn a_recovered_islock_id_is_not_signed() {
    let rig = Rig::regtest();
    let tx = child(&[txid(1)]);
    lockable(&rig, &tx);
    select(&rig, &tx);
    recover_input(&rig, &tx, txid(1));
    rig.signing.insert_recovered(recsig(rig.llmq, islock_id(&tx), tx.txid().into()));
    rig.signer.process_tx(&tx, false);
    assert!(islock_requests(&rig).is_empty());
    assert!(creating_is_empty(&rig));
}

// Core: v24.0.2 src/instantsend/signing.cpp:480-486 TrySignInstantSendLock
#[test]
fn no_quorum_creates_no_islock() {
    let rig = Rig::regtest();
    let tx = child(&[txid(1)]);
    lockable(&rig, &tx);
    recover_input(&rig, &tx, txid(1));
    rig.signer.process_tx(&tx, false);
    assert!(islock_requests(&rig).is_empty());
    assert!(creating_is_empty(&rig));
}

// Core: v24.0.2 src/instantsend/signing.cpp:488-501 TrySignInstantSendLock; the islock request
// never resigns, also when retroactive (:501 against :442)
#[test]
fn the_islock_names_the_quorum_and_its_cycle_start_and_never_resigns() {
    for retroactive in [false, true] {
        let rig = Rig::regtest();
        let tx = child(&[txid(1)]);
        lockable(&rig, &tx);
        let (quorum, cycle) = select(&rig, &tx);
        recover_input(&rig, &tx, txid(1));
        rig.signer.process_tx(&tx, retroactive);
        assert_eq!(
            rig.requests(),
            [vote(&rig, &tx, retroactive), islock_request(&rig, &tx, quorum)]
        );
        let creating = rig.signer.creating.lock();
        let lock = &creating.by_id[&islock_id(&tx)];
        assert_eq!((lock.txid(), lock.cycle_hash()), (tx.txid(), cycle));
        assert_eq!(lock.inputs(), &lock_inputs(&tx));
        assert_eq!(creating.by_txid[&tx.txid()], islock_id(&tx));
    }
}

// Core: v24.0.2 src/instantsend/signing.cpp:492-499 TrySignInstantSendLock
#[test]
fn an_islock_in_progress_is_signed_once() {
    let rig = Rig::regtest();
    let (tx, _) = creating(&rig);
    rig.signer.process_tx(&tx, false);
    assert_eq!(islock_requests(&rig).len(), 1);
}

// Core: v24.0.2 src/instantsend/signing.cpp:109-113 HandleNewRecoveredSig, :125-148
// HandleNewInputLockRecoveredSig
#[test]
fn an_input_recsig_waits_for_the_index_then_attempts_the_islock() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let tx = child(&[txid(1)]);
    lockable(&rig, &tx);
    let (quorum, _) = select(&rig, &tx);
    rig.signer.process_tx(&tx, false);
    assert!(islock_requests(&rig).is_empty());
    let delivered = rig.signing.deliver(recsig(rig.llmq, input_id(txid(1)), tx.txid().into()));
    assert_eq!(delivered, [RecoveredSigResult::Nothing]);
    assert_eq!(rig.txs.2.load(Ordering::SeqCst), 1);
    assert_eq!(islock_requests(&rig), [islock_request(&rig, &tx, quorum)]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:114-115 HandleNewRecoveredSig, :345-357
// HandleNewInstantSendLockRecoveredSig
#[test]
fn the_islock_recsig_queues_the_signed_lock() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let (tx, cycle) = creating(&rig);
    let sig = signed(rig.llmq, islock_id(&tx), tx.txid().into(), [0x5a; 96]);
    assert_eq!(rig.signing.deliver(sig), [RecoveredSigResult::Nothing]);
    let lock = LockedTx::new(1, lock_inputs(&tx), tx.txid(), cycle, [0x5a; 96].into());
    assert_eq!(*rig.pending.0.lock(), [(isdlock_hash(&lock), lock)]);
    assert!(creating_is_empty(&rig));
}

// Core: v24.0.2 src/instantsend/signing.cpp:338-354 HandleNewInstantSendLockRecoveredSig
#[test]
fn an_islock_recsig_for_another_tx_is_dropped_and_cleared() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let (tx, _) = creating(&rig);
    let sig = recsig(rig.llmq, islock_id(&tx), txid(9).into());
    let (delivered, lines) = test_log::capture(|| rig.signing.deliver(sig));
    assert_eq!(delivered, [RecoveredSigResult::Nothing]);
    assert!(rig.pending.0.lock().is_empty());
    assert!(creating_is_empty(&rig));
    let line = &instantsend_lines(&lines, tx.txid())[0];
    assert!(
        line.starts_with("INFO HandleNewInstantSendLockRecoveredSig")
            && line.contains("islock conflicts with")
    );
}

// Core: v24.0.2 src/instantsend/signing.cpp:108-117 HandleNewRecoveredSig
#[test]
fn an_unknown_recsig_is_ignored() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let delivered = rig.signing.deliver(recsig(rig.llmq, [0x77; 32], txid(1).into()));
    assert_eq!(delivered, [RecoveredSigResult::Nothing]);
    assert_eq!(rig.txs.2.load(Ordering::SeqCst), 0);
    assert!(rig.pending.0.lock().is_empty() && rig.requests().is_empty());
}

// Core: v24.0.2 src/instantsend/signing.cpp:100-106 HandleNewRecoveredSig
#[test]
fn no_recsig_is_routed_with_instantsend_disabled_or_no_quorum_type() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    rig.signer.input_request_ids.lock().insert(input_id(txid(1)));
    rig.view.set_enabled(false);
    rig.signing.deliver(recsig(rig.llmq, input_id(txid(1)), txid(2).into()));

    let mut setup = setup(Network::Regtest);
    setup.consensus.llmq_type_dip0024_instant_send = LlmqType::LlmqNone;
    let none = Rig::new(&setup);
    none.signing.register_listener(none.signer.clone());
    none.signer.input_request_ids.lock().insert(input_id(txid(1)));
    none.signing.deliver(recsig(none.llmq, input_id(txid(1)), txid(2).into()));
    assert_eq!(rig.txs.2.load(Ordering::SeqCst) + none.txs.2.load(Ordering::SeqCst), 0);
}

// Core: v24.0.2 src/instantsend/signing.cpp:157-186 ProcessPendingRetryLockTxs
#[test]
fn the_retry_skips_txs_in_progress_locked_or_conflicting() {
    let rig = Rig::regtest();
    let (in_progress, _) = creating(&rig);
    let [locked, conflicting, fresh] = [2, 3, 4].map(|p| child(&[txid(p)]));
    for tx in [&locked, &conflicting, &fresh] {
        lockable(&rig, tx);
    }
    rig.view.lock(locked.txid());
    let other =
        LockedTx::new(1, lock_inputs(&conflicting), txid(9), Default::default(), [0; 96].into());
    let hash = isdlock_hash(&other);
    rig.store.scripted.lock().unwrap().conflicting_lock.push_back(Ok(Some((hash, other))));
    let before = rig.requests().len();
    let fresh_vote = vote(&rig, &fresh, false);
    let txs: Vec<TxRef> =
        [in_progress, locked, conflicting, fresh].map(Arc::new).into_iter().collect();
    let ((), lines) = test_log::capture(|| rig.signer.process_pending_retry(&txs));
    assert_eq!(rig.requests()[before..], [fresh_vote]);
    let summary: Vec<_> = lines.iter().filter(|l| l.contains("retried")).collect();
    assert_eq!(summary, ["DEBUG ProcessPendingRetryLockTxs -- retried 1 TXs."]);
}

// Core: v24.0.2 src/instantsend/signing.cpp:83-89 ClearInputsFromQueue
#[test]
fn a_cleared_input_id_no_longer_routes() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let tx = child(&[txid(1), txid(2)]);
    lockable(&rig, &tx);
    select(&rig, &tx);
    rig.signer.process_tx(&tx, false);
    let ids: Vec<_> = lock_inputs(&tx).iter().map(input_lock_request_id).collect();
    rig.signer.clear_inputs(&ids[..1]);
    assert_eq!(rig.input_ids(), ids[1..].iter().copied().collect());
    rig.signing.deliver(recsig(rig.llmq, ids[0], tx.txid().into()));
    assert_eq!(rig.txs.2.load(Ordering::SeqCst), 0);
}

// Core: v24.0.2 src/instantsend/signing.cpp:91-96 ClearLockFromQueue
#[test]
fn a_cleared_islock_no_longer_completes() {
    let rig = Rig::regtest();
    rig.signing.register_listener(rig.signer.clone());
    let (tx, cycle) = creating(&rig);
    let lock = LockedTx::new(1, lock_inputs(&tx), tx.txid(), cycle, [0x5a; 96].into());
    rig.signer.clear_lock(&lock);
    assert!(creating_is_empty(&rig));
    rig.signing.deliver(recsig(rig.llmq, islock_id(&tx), tx.txid().into()));
    assert!(rig.pending.0.lock().is_empty());
}
