use std::collections::BTreeMap;
use std::sync::Mutex;

use dash_clock::MockClock;
use dash_consensus::primitives::{Transaction, TxHash, TxType};
use dash_dml::mnhf::signer::SignRequest as EhfRequest;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::BlsPkValue;
use dash_llmq::sigenv::PeerId;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeQuorum};
use dash_llmq::signing::api::{ListenerId, SelectedQuorum};
use dash_llmq::signing::db::{RecSigBatch, RecSigKv, RecSigOp, RecSigScan};
use dash_llmq::signing::manager::SigningManager;
use dash_p2p_core::Inventory;
use dash_params::{Bip9Deployment, ChainParamsBuilder, DeploymentPos, LlmqType, Network, Release};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::reply::no_wait_task;

use super::*;
use crate::signing::{RecSigNotify, SignQueue, SigningHandles, SigningNet, SigningRuntime};
use crate::traits::LocalMasternode;

const START: i64 = 1_700_000_000;
const END: i64 = START + 10;
const TIP: BlockId = BlockId(7);
const QA: [u8; 32] = [0xa1; 32];
const QB: [u8; 32] = [0xb2; 32];
const ME: [u8; 32] = [9; 32];

/// Regtest with V24 and EvoShares on EHF in `[START, END]`.
fn params() -> ChainSetup {
    let mut p = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    for pos in [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares] {
        let d = p.consensus.deployments.deployment_mut(pos).unwrap();
        d.use_ehf = pos != DeploymentPos::TestDummy;
        d.start_time = START;
        d.timeout = END;
    }
    p
}

fn bit(p: &ChainSetup, pos: DeploymentPos) -> u8 {
    p.consensus.deployments.get(pos).unwrap().bit as u8
}

/// The V24 signal's request id, from the library's signing pass.
fn v24_id(p: &ChainSetup) -> [u8; 32] {
    let signals = EhfSignals::new();
    let tip = TipFacts {
        v20_active: true,
        mtp: START,
        mined_bits: &signals,
    };
    let requests = ehf_sign_requests(p, tip);
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert_eq!(requests[0].0, bit(p, DeploymentPos::V24));
    requests[0].1
}

/// The message hash the library signs for the V24 signal over quorum `q`.
fn msg_over(p: &ChainSetup, q: [u8; 32]) -> [u8; 32] {
    let outcome = ehf_try_sign(
        p,
        bit(p, DeploymentPos::V24),
        &mut EhfSigner::default(),
        |_| Some(q.into()),
        |_, _| false,
    );
    match outcome {
        TrySignOutcome::Sign(EhfRequest {
            msg_hash,
            ..
        }) => msg_hash,
        other => panic!("{other:?}"),
    }
}

#[derive(Default)]
struct Chain {
    tip: Option<EhfTip>,
    signals: Mutex<Option<Result<EhfSignals, SigningFault>>>,
    signal_reads: Mutex<usize>,
    aborts: Mutex<Vec<SigningFault>>,
}

impl EhfChain for Arc<Chain> {
    fn tip(&self, block: BlockId) -> Result<Option<EhfTip>, SigningFault> {
        Ok(self.tip.clone().filter(|_| block == TIP))
    }
    fn active_tip_signals(&self) -> Result<EhfSignals, SigningFault> {
        *self.signal_reads.lock().unwrap() += 1;
        self.signals.lock().unwrap().clone().unwrap_or_else(|| Ok(EhfSignals::new()))
    }
    fn abort(&self, fault: &SigningFault) {
        self.aborts.lock().unwrap().push(fault.clone());
    }
}

fn tip(mtp: i64, v20_active: bool) -> EhfTip {
    EhfTip {
        height: 300,
        v20_active,
        mtp,
        signals: EhfSignals::new(),
    }
}

#[derive(Default)]
struct Pool {
    waits: Mutex<usize>,
    reject: Option<String>,
    fault: Option<SigningFault>,
    seen: Mutex<Vec<TxRef>>,
}

impl EhfMempool for Pool {
    fn block_until_txindex_synced(&self) {
        *self.waits.lock().unwrap() += 1;
    }
    fn process_transaction(&self, tx: TxRef) -> Result<Result<(), String>, SigningFault> {
        self.seen.lock().unwrap().push(tx);
        if let Some(fault) = &self.fault {
            return Err(fault.clone());
        }
        Ok(self.reject.clone().map_or(Ok(()), Err))
    }
}

/// `QuorumSigning` with a fixed recovered-signature answer and selection, recording its calls.
#[derive(Default)]
struct Signing {
    recovered: bool,
    selected: Option<[u8; 32]>,
    fault: Option<SigningFault>,
    calls: Mutex<Vec<String>>,
    signed: Mutex<Vec<SignRequest>>,
}

impl Signing {
    fn call(&self, name: &str) -> Result<(), SigningFault> {
        self.calls.lock().unwrap().push(name.to_owned());
        self.fault.clone().map_or(Ok(()), Err)
    }
}

impl QuorumSigning for Signing {
    fn async_sign_if_member(&self, req: &SignRequest) -> Result<bool, SigningFault> {
        self.call("sign")?;
        self.signed.lock().unwrap().push(req.clone());
        Ok(true)
    }
    fn select_quorum_for_signing(
        &self,
        _: LlmqType,
        _: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, SigningFault> {
        self.call("select")?;
        Ok(self.selected.map(|q| SelectedQuorum {
            quorum_hash: q.into(),
            base_height: 0,
        }))
    }
    fn has_recovered_sig(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unimplemented!()
    }
    fn has_recovered_sig_for_id(&self, _: LlmqType, _: &[u8; 32]) -> Result<bool, SigningFault> {
        self.call("has_for_id")?;
        Ok(self.recovered)
    }
    fn has_recovered_sig_for_session(&self, _: &[u8; 32]) -> Result<bool, SigningFault> {
        unimplemented!()
    }
    fn has_recovered_sig_for_signing(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unimplemented!()
    }
    fn get_recovered_sig(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, SigningFault> {
        unimplemented!()
    }
    fn is_conflicting(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        unimplemented!()
    }
    fn get_vote_for_id(&self, _: LlmqType, _: &[u8; 32]) -> Result<Option<[u8; 32]>, SigningFault> {
        unimplemented!()
    }
    fn truncate_recovered_sig(&self, _: LlmqType, _: &[u8; 32]) -> Result<(), SigningFault> {
        unimplemented!()
    }
    fn register_listener(&self, _: Arc<dyn RecoveredSigListener>) -> ListenerId {
        unimplemented!()
    }
    fn unregister_listener(&self, _: ListenerId) {
        unimplemented!()
    }
}

type Handler = EhfSignalsHandler<Signing, Arc<Chain>>;

fn handler_with(
    p: ChainSetup,
    signing: Signing,
    chain: Chain,
    pool: Pool,
) -> (Handler, Arc<Chain>, Arc<Pool>) {
    let (chain, pool) = (Arc::new(chain), Arc::new(pool));
    let h = EhfSignalsHandler::new(Arc::new(p), Arc::new(signing), chain.clone(), pool.clone());
    (h, chain, pool)
}

fn handler(signing: Signing, pool: Pool) -> (Handler, Arc<Chain>, Arc<Pool>) {
    let chain = Chain {
        tip: Some(tip(START, true)),
        ..Chain::default()
    };
    handler_with(params(), signing, chain, pool)
}

fn selecting() -> Signing {
    Signing {
        selected: Some(QA),
        ..Signing::default()
    }
}

fn calls(h: &Handler) -> Vec<String> {
    h.signing.calls.lock().unwrap().clone()
}

/// The recovered signature of the V24 signal's id over quorum A.
fn recovered(id: [u8; 32]) -> RecoveredSig {
    RecoveredSig {
        llmq_type: params().consensus.llmq_type_mnhf,
        quorum_hash: QA.into(),
        id,
        msg_hash: [3; 32],
        sig: [0; 96],
    }
}

/// No signing before V20, outside `[start, timeout]` or for a NEVER_ACTIVE deployment; inside
/// the window the V24 signal is signed.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:36-40, :44, :57
#[test]
fn the_window_is_closed_for_never_active_before_start_and_after_timeout() {
    for (mtp, v20, signs) in [
        (START, false, false),
        (START - 1, true, false),
        (START, true, true),
        (END, true, true),
        (END + 1, true, false),
    ] {
        let chain = Chain {
            tip: Some(tip(mtp, v20)),
            ..Chain::default()
        };
        let (h, ..) = handler_with(params(), selecting(), chain, Pool::default());
        h.updated_block_tip(TIP).unwrap();
        assert_eq!(h.signing.signed.lock().unwrap().len(), usize::from(signs), "{mtp} {v20}");
    }
    let mut p = params();
    p.consensus.deployments.deployment_mut(DeploymentPos::V24).unwrap().start_time =
        Bip9Deployment::NEVER_ACTIVE;
    let chain = Chain {
        tip: Some(tip(START, true)),
        ..Chain::default()
    };
    let (h, ..) = handler_with(p, selecting(), chain, Pool::default());
    h.updated_block_tip(TIP).unwrap();
    assert!(calls(&h).is_empty());
    let (h, ..) = handler(selecting(), Pool::default());
    h.updated_block_tip(BlockId(8)).unwrap();
    assert!(calls(&h).is_empty(), "an unknown block signs nothing");
}

/// EvoShares, on EHF and in its window, is never signed.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:53-55
#[test]
fn evo_shares_is_never_signed() {
    let mut p = params();
    p.consensus.deployments.deployment_mut(DeploymentPos::V24).unwrap().use_ehf = false;
    assert!(p.consensus.deployments.get(DeploymentPos::EvoShares).unwrap().use_ehf);
    let chain = Chain {
        tip: Some(tip(START, true)),
        ..Chain::default()
    };
    let (h, ..) = handler_with(p, selecting(), chain, Pool::default());
    h.updated_block_tip(TIP).unwrap();
    assert!(calls(&h).is_empty());
}

/// The messages `f` logs on this thread.
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
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = format!("{v:?}");
                    }
                }
            }
            let mut m = Message(String::new());
            event.record(&mut m);
            self.0.lock().unwrap().push(m.0);
        }
        fn enter(&self, _: &tracing::span::Id) {}
        fn exit(&self, _: &tracing::span::Id) {}
    }
    static KEEP: std::sync::OnceLock<tracing::Dispatch> = std::sync::OnceLock::new();
    KEEP.get_or_init(|| tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default()));
    let lines = Arc::new(Lines(Mutex::new(Vec::new())));
    let out = tracing::subscriber::with_default(Arc::clone(&lines), f);
    let lines = std::mem::take(&mut *lines.0.lock().unwrap());
    (out, lines)
}

/// With TestDummy and EvoShares both on EHF and in their windows, the signer logs
/// `bit=<testdummy> at height`, signing or already signed, and never `bit=<evo_shares> at height`.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:53-55, :80, :92,
// test/functional/feature_evo_shares_activation.py:46-56
#[test]
fn the_signer_logs_the_signed_bit_and_never_the_evo_shares_bit() {
    let mut p = params();
    let deployments = &mut p.consensus.deployments;
    deployments.deployment_mut(DeploymentPos::TestDummy).unwrap().use_ehf = true;
    deployments.deployment_mut(DeploymentPos::V24).unwrap().use_ehf = false;
    assert!(deployments.get(DeploymentPos::EvoShares).unwrap().use_ehf);
    let (dummy_bit, shares_bit) =
        (bit(&p, DeploymentPos::TestDummy), bit(&p, DeploymentPos::EvoShares));
    assert_eq!((dummy_bit, shares_bit), (28, 14));
    let already_signed = Signing {
        recovered: true,
        ..selecting()
    };
    for (signing, signs) in [(selecting(), 1), (already_signed, 0)] {
        let chain = Chain {
            tip: Some(tip(START, true)),
            ..Chain::default()
        };
        let (h, ..) = handler_with(p.clone(), signing, chain, Pool::default());
        let (result, lines) = logged(|| h.updated_block_tip(TIP));
        result.unwrap();
        let signed = format!("bit={dummy_bit} at height");
        assert!(lines.iter().any(|l| l.contains(&signed)), "{lines:?}");
        let evo = format!("bit={shares_bit} at height");
        assert!(!lines.iter().any(|l| l.contains(&evo)), "{lines:?}");
        assert_eq!(h.signing.signed.lock().unwrap().len(), signs);
    }
}

/// An id is tracked when its signature is already recovered (no selection, no sign) and when a
/// quorum is selected (signed with `allowReSign = false`, `allowDiffMsgHashSigning = true`), but
/// not when no quorum is found: a tracked id's recovered signature reaches the mempool.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:76-97
#[test]
fn an_id_is_tracked_on_already_recovered_and_sign_but_not_on_no_quorum() {
    let id = v24_id(&params());
    let recovered_case = Signing {
        recovered: true,
        selected: Some(QA),
        ..Signing::default()
    };
    for (signing, expected_calls, tracked) in [
        (recovered_case, &["has_for_id"][..], true),
        (Signing::default(), &["has_for_id", "select"][..], false),
        (selecting(), &["has_for_id", "select", "sign"][..], true),
    ] {
        let (h, _, pool) = handler(signing, Pool::default());
        h.updated_block_tip(TIP).unwrap();
        assert_eq!(calls(&h), expected_calls);
        let result = h.handle_new_recovered_sig(&recovered(id));
        assert_eq!(pool.seen.lock().unwrap().len(), usize::from(tracked), "{expected_calls:?}");
        assert_eq!(matches!(result, RecoveredSigResult::RelayTx(_)), tracked);
    }
    let (h, ..) = handler(selecting(), Pool::default());
    h.updated_block_tip(TIP).unwrap();
    let signed = h.signing.signed.lock().unwrap().clone();
    assert_eq!(
        signed,
        [SignRequest {
            llmq_type: params().consensus.llmq_type_mnhf,
            id,
            msg_hash: msg_over(&params(), QA),
            quorum_hash: Some(QA.into()),
            allow_resign: false,
            allow_diff_msg: true,
        }]
    );
}

/// A recovered signature for an untracked id is not this handler's: `Nothing`, after the
/// txindex wait and before any chain or mempool call.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:102-109
#[test]
fn a_foreign_id_recovered_sig_gives_nothing() {
    let (h, chain, pool) = handler(selecting(), Pool::default());
    h.updated_block_tip(TIP).unwrap();
    let result = h.handle_new_recovered_sig(&recovered([0x55; 32]));
    assert_eq!(result, RecoveredSigResult::Nothing);
    assert_eq!(*pool.waits.lock().unwrap(), 1);
    assert_eq!(*chain.signal_reads.lock().unwrap(), 0);
    assert!(pool.seen.lock().unwrap().is_empty());
}

/// A tracked id whose deployment is already mined at the active tip gives `Nothing`.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:111-118
#[test]
fn a_mined_signal_gives_nothing() {
    let p = params();
    let id = v24_id(&p);
    let (h, chain, pool) = handler(selecting(), Pool::default());
    h.updated_block_tip(TIP).unwrap();
    *chain.signals.lock().unwrap() =
        Some(Ok([(bit(&p, DeploymentPos::V24), 20)].into_iter().collect()));
    assert_eq!(h.handle_new_recovered_sig(&recovered(id)), RecoveredSigResult::Nothing);
    assert!(pool.seen.lock().unwrap().is_empty());
}

/// An ATMP reject gives `Nothing`.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:134-141
#[test]
fn an_atmp_reject_gives_nothing() {
    let pool = Pool {
        reject: Some("bad-mnhf".into()),
        ..Pool::default()
    };
    let (h, _, pool) = handler(selecting(), pool);
    h.updated_block_tip(TIP).unwrap();
    let id = v24_id(&params());
    assert_eq!(h.handle_new_recovered_sig(&recovered(id)), RecoveredSigResult::Nothing);
    assert_eq!(pool.seen.lock().unwrap().len(), 1);
}

/// An accepted transaction is relayed: the MNHF special transaction (version 3, type 7, no inputs
/// or outputs) naming the recovered signature's quorum, with its txid.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:124-138, src/evo/mnhftx.cpp:37-45
#[test]
fn an_accepted_tx_gives_relay_tx() {
    let (h, _, pool) = handler(selecting(), Pool::default());
    h.updated_block_tip(TIP).unwrap();
    let id = v24_id(&params());
    let result = h.handle_new_recovered_sig(&recovered(id));
    let seen = pool.seen.lock().unwrap();
    let raw = seen[0].raw();
    assert_eq!((raw.version(), raw.tx_type()), (3, TxType::MnhfSignal));
    assert!(raw.inputs().is_empty() && raw.outputs().is_empty() && raw.lock_time() == 0);
    let payload = raw.extra_payload();
    assert_eq!(payload[1], bit(&params(), DeploymentPos::V24));
    assert_eq!(&payload[2..34], &QA);
    assert_eq!(result, RecoveredSigResult::RelayTx(seen[0].txid()));
}

/// A `Storage` or `Lookup` fault is the tip update's error and the listener's abort; `Stopped`
/// is dropped on the listener path.
#[test]
fn faults_are_returned_or_abort_and_stopped_is_dropped() {
    let signing = Signing {
        fault: Some(SigningFault::Lookup("v".into())),
        ..selecting()
    };
    let (h, ..) = handler(signing, Pool::default());
    assert_eq!(h.updated_block_tip(TIP), Err(SigningFault::Lookup("v".into())));
    let id = v24_id(&params());
    for (fault, aborted) in
        [(SigningFault::Storage("db".into()), true), (SigningFault::Stopped, false)]
    {
        let pool = Pool {
            fault: Some(fault.clone()),
            ..Pool::default()
        };
        let (h, chain, _) = handler(selecting(), pool);
        h.updated_block_tip(TIP).unwrap();
        assert_eq!(h.handle_new_recovered_sig(&recovered(id)), RecoveredSigResult::Nothing);
        let aborts = chain.aborts.lock().unwrap().clone();
        assert_eq!(
            aborts,
            if aborted {
                vec![fault]
            } else {
                vec![]
            }
        );
    }
}

fn poll_once<F: std::future::Future>(f: F) -> std::task::Poll<F::Output> {
    let mut f = std::pin::pin!(f);
    f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "EHF signing from a no-wait task")]
fn a_tip_update_from_a_no_wait_task_panics() {
    let (h, ..) = handler(selecting(), Pool::default());
    let _ = poll_once(no_wait_task(async move { h.updated_block_tip(TIP) }));
}

#[derive(Default)]
struct Mem(BTreeMap<Vec<u8>, Vec<u8>>);

impl RecSigKv for Mem {
    type Error = &'static str;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.0.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        Ok(Box::new(self.0.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => self.0.insert(k, v),
                RecSigOp::Delete(k) => self.0.remove(&k),
            };
        }
        Ok(())
    }
}

/// A queued signing: `(quorum type, quorum, id, msg)`.
type Queued = (LlmqType, QuorumHash, [u8; 32], [u8; 32]);

/// The node's handles; the sign queue records each [`Queued`].
#[derive(Default)]
struct Node {
    signed: Mutex<Vec<Queued>>,
}

impl SigningNet for Node {
    fn misbehaving(&self, _: PeerId, _: i32) {}
    fn relay_inv(&self, _: &Inventory) {}
    fn relay_transaction(&self, _: &TxHash) {}
}

impl RecSigNotify for Node {
    fn notify_recovered_sig(&self, _: Arc<RecoveredSig>, _: bool) {}
}

impl LocalMasternode for Node {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        Some(ME.into())
    }
}

impl SignQueue<FakeQuorum> for Node {
    fn force_reannouncement(&self, _: &Arc<FakeQuorum>, _: LlmqType, _: &[u8; 32], _: &[u8; 32]) {}
    fn async_sign(&self, q: Arc<FakeQuorum>, id: [u8; 32], msg: [u8; 32]) {
        self.signed.lock().unwrap().push((q.llmq_type, q.quorum_hash, id, msg));
    }
}

fn fake_quorum(p: &ChainSetup, hash: [u8; 32]) -> Arc<FakeQuorum> {
    let t = p.consensus.llmq_type_mnhf;
    Arc::new(FakeQuorum {
        llmq_type: t,
        quorum_hash: hash.into(),
        params: *p.consensus.llmq(t).unwrap(),
        members: vec![ME.into()],
        valid_members: [0].into(),
        has_verification_vector: false,
        quorum_public_key: BlsPkValue::invalid(),
        pk_shares: BTreeMap::new(),
        sk_share: None,
    })
}

/// The handler over the node's signing runtime, with the runtime's selector
/// (`select_for_signing_default`): the lookup answers quorum A only at Core's offset (-1, 8) and
/// quorum B at (-1, 0). The sign request reaches the sig-share queue for the MNHF quorum type,
/// the V24 request id and quorum A, whose hash the signed message hash commits to; the vote is
/// written for that message.
// Core: v24.0.2 src/llmq/ehf_signals.cpp:85-97, src/llmq/quorumsman.h:214-223,
// src/llmq/signing_shares.cpp:792-876
#[test]
fn the_composed_runtime_signs_over_the_quorum_selected_at_the_tip_offset() {
    let p = params();
    let t = p.consensus.llmq_type_mnhf;
    let id = v24_id(&p);
    let mut lookup = FakeLookup::default();
    for (hash, offset) in [(QA, 8), (QB, 0)] {
        let q = fake_quorum(&p, hash);
        lookup.quorums.insert((t.to_base(), q.quorum_hash), q.clone());
        lookup.active.insert((t.to_base(), q.quorum_hash));
        lookup.selections.insert((t.to_base(), id, -1, offset), q);
    }
    let node = Arc::new(Node::default());
    let handles = SigningHandles {
        net: node.clone(),
        notify: node.clone(),
        masternode: node.clone(),
        clock: Arc::new(MockClock::at(START)),
    };
    let manager = SigningManager::new(Mem::default(), p.consensus.llmq_type_platform, 60);
    let rt = SigningRuntime::new(manager, Arc::new(lookup), Arc::new(p.clone()), handles);
    assert!(rt.set_sign_queue(node.clone()).is_ok());
    let rt = Arc::new(rt);
    let chain = Arc::new(Chain {
        tip: Some(tip(START, true)),
        ..Chain::default()
    });
    let h =
        EhfSignalsHandler::new(Arc::new(p.clone()), rt.clone(), chain, Arc::new(Pool::default()));
    h.updated_block_tip(TIP).unwrap();
    let msg = msg_over(&p, QA);
    assert_ne!(msg, msg_over(&p, QB), "the quorum reaches the message hash");
    assert_eq!(*node.signed.lock().unwrap(), [(t, QuorumHash::from(QA), id, msg)]);
    assert_eq!(rt.get_vote_for_id(t, &id).unwrap(), Some(msg));
    let unsigned = Transaction::new(
        3,
        TxType::MnhfSignal,
        vec![],
        vec![],
        0,
        [&[1, bit(&p, DeploymentPos::V24)][..], &QA, &[0; 96]].concat(),
    );
    assert_eq!(*Tx::new(unsigned).txid().as_bytes(), msg, "MNHFTxPayload::PrepareTx's hash");
}
