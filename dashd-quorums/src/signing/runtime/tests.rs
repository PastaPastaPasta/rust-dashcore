use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{OnceLock, Weak};
use std::thread;
use std::time::Duration;

use dash_clock::MockClock;
use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::BlsPkValue;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeQuorum};
use dash_llmq::signing::db::{RecSigBatch, RecSigOp, RecSigScan};
use dash_num::Hash256;
use dash_p2p_core::InvType;
use dash_params::{ChainParamsBuilder, Network, Release};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_chainstate::chain::reply::no_wait_task;

use super::*;
use crate::signing::SigningInterrupt;
use crate::signing::worker::{IDLE_SLEEP, RecSigsAbort, RecSigsWorker, std_spawn};

const T: LlmqType = LlmqType::LlmqTest;
const P: LlmqType = LlmqType::LlmqTestPlatform;
const ME: [u8; 32] = [9; 32];
const QH: [u8; 32] = [4; 32];
const ID: [u8; 32] = [1; 32];
const MSG: [u8; 32] = [2; 32];
const OTHER: [u8; 32] = [3; 32];
const NOW: i64 = 1_700_000_000;
const BASIC: bool = false;
const WAIT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Mem {
    rows: BTreeMap<Vec<u8>, Vec<u8>>,
    fail: Arc<AtomicBool>,
}

impl RecSigKv for Mem {
    type Error = &'static str;

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        if self.fail.load(Ordering::Relaxed) {
            return Err("read");
        }
        Ok(self.rows.get(key).cloned())
    }

    fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
        Ok(Box::new(self.rows.range(start.to_vec()..).map(|(k, v)| Ok((k.clone(), v.clone())))))
    }

    fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
        if self.fail.load(Ordering::Relaxed) {
            return Err("write");
        }
        for op in batch.ops {
            match op {
                RecSigOp::Put(k, v) => {
                    self.rows.insert(k, v);
                }
                RecSigOp::Delete(k) => {
                    self.rows.remove(&k);
                }
            }
        }
        Ok(())
    }
}

impl SigningLookup for FakeLookup {
    fn base_height(&self, quorum: &FakeQuorum) -> i32 {
        i32::from(quorum.quorum_hash.as_bytes()[0]) * 24
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Ban(PeerId, i32),
    Inv(Inventory),
    Tx(TxHash),
    Notify([u8; 32], bool),
    Reannounce([u8; 32]),
    Sign([u8; 32]),
    Listener(u8),
}

#[derive(Default)]
struct Rec {
    events: Mutex<Vec<Ev>>,
    pro_tx: Option<ProTxHash>,
}

impl Rec {
    fn push(&self, ev: Ev) {
        self.events.lock().unwrap().push(ev);
    }

    fn take(&self) -> Vec<Ev> {
        std::mem::take(&mut self.events.lock().unwrap())
    }
}

impl SigningNet for Rec {
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.push(Ev::Ban(peer, score));
    }
    fn relay_inv(&self, inv: &Inventory) {
        self.push(Ev::Inv(*inv));
    }
    fn relay_transaction(&self, txid: &TxHash) {
        self.push(Ev::Tx(*txid));
    }
}

impl RecSigNotify for Rec {
    fn notify_recovered_sig(&self, sig: Arc<RecoveredSig>, proactive_relay: bool) {
        self.push(Ev::Notify(sig.hash(), proactive_relay));
    }
}

impl SignQueue<FakeQuorum> for Rec {
    fn force_reannouncement(&self, q: &Arc<FakeQuorum>, _: LlmqType, id: &[u8; 32], _: &[u8; 32]) {
        assert_eq!(q.quorum_hash, QuorumHash::from(QH));
        self.push(Ev::Reannounce(*id));
    }
    fn async_sign(&self, q: Arc<FakeQuorum>, id: [u8; 32], _: [u8; 32]) {
        assert_eq!(q.quorum_hash, QuorumHash::from(QH));
        self.push(Ev::Sign(id));
    }
}

impl LocalMasternode for Rec {
    fn pro_tx_hash(&self) -> Option<ProTxHash> {
        self.pro_tx
    }
}

type Runtime = SigningRuntime<Mem, FakeLookup>;

fn quorum(t: LlmqType) -> Arc<FakeQuorum> {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    Arc::new(FakeQuorum {
        llmq_type: t,
        quorum_hash: QH.into(),
        params: *setup.consensus.llmq(t).unwrap(),
        members: vec![ProTxHash::from(ME)],
        valid_members: [0].into(),
        has_verification_vector: false,
        quorum_public_key: BlsPkValue::invalid(),
        pk_shares: BTreeMap::new(),
        sk_share: None,
    })
}

fn lookup() -> FakeLookup {
    let mut l = FakeLookup::default();
    for t in [T, P] {
        let q = quorum(t);
        l.quorums.insert((t.to_base(), q.quorum_hash), q.clone());
        l.active.insert((t.to_base(), q.quorum_hash));
        l.selections.insert((t.to_base(), ID, -1, 8), q);
    }
    l
}

fn build(pro_tx: Option<ProTxHash>) -> (Arc<Runtime>, Arc<Rec>, Arc<MockClock>) {
    build_with(pro_tx, Mem::default())
}

fn handles(rec: &Arc<Rec>, clock: &Arc<MockClock>) -> SigningHandles {
    SigningHandles {
        net: rec.clone(),
        notify: rec.clone(),
        masternode: rec.clone(),
        clock: clock.clone(),
    }
}

fn recorder(pro_tx: Option<ProTxHash>) -> (Arc<Rec>, Arc<MockClock>) {
    let rec = Arc::new(Rec {
        pro_tx,
        ..Rec::default()
    });
    (rec, Arc::new(MockClock::at(NOW)))
}

fn build_unqueued(pro_tx: Option<ProTxHash>, mem: Mem) -> (Arc<Runtime>, Arc<Rec>, Arc<MockClock>) {
    let (rec, clock) = recorder(pro_tx);
    let manager = SigningManager::new(mem, P, 60);
    let rt =
        SigningRuntime::new(manager, Arc::new(lookup()), Arc::new(chain()), handles(&rec, &clock));
    (Arc::new(rt), rec, clock)
}

fn build_with(pro_tx: Option<ProTxHash>, mem: Mem) -> (Arc<Runtime>, Arc<Rec>, Arc<MockClock>) {
    let (rt, rec, clock) = build_unqueued(pro_tx, mem);
    assert!(rt.set_sign_queue(rec.clone()).is_ok());
    (rt, rec, clock)
}

fn runtime() -> (Arc<Runtime>, Arc<Rec>, Arc<MockClock>) {
    build(Some(ProTxHash::from(ME)))
}

fn recovered(t: LlmqType, id: [u8; 32], msg: [u8; 32]) -> Arc<RecoveredSig> {
    Arc::new(RecoveredSig {
        llmq_type: t,
        quorum_hash: QH.into(),
        id,
        msg_hash: msg,
        sig: [msg[0]; 96],
    })
}

fn request(t: LlmqType, msg: [u8; 32], allow_resign: bool, allow_diff_msg: bool) -> SignRequest {
    SignRequest {
        llmq_type: t,
        id: ID,
        msg_hash: msg,
        quorum_hash: Some(QH.into()),
        allow_resign,
        allow_diff_msg,
    }
}

fn wire(r: &RecoveredSig) -> Vec<u8> {
    let mut v = vec![r.llmq_type.to_base()];
    v.extend_from_slice(r.quorum_hash.as_bytes());
    v.extend_from_slice(&r.id);
    v.extend_from_slice(&r.msg_hash);
    v.extend_from_slice(&r.sig);
    v
}

fn chain() -> ChainSetup {
    ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()
}

fn time_key(prefix: &[u8], time: i64, t: LlmqType, id: &[u8; 32]) -> Vec<u8> {
    let mut key = vec![prefix.len() as u8];
    key.extend_from_slice(prefix);
    key.extend_from_slice(&(time as u32).to_be_bytes());
    key.push(t.to_base());
    key.extend_from_slice(id);
    key
}

struct Seq(u64);

impl rand_core::TryRng for Seq {
    type Error = Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(self.try_next_u64()? as u32)
    }
    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        Ok(self.0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        dst.fill(0);
        Ok(())
    }
}

/// Runs `f` on another thread; a deadlock fails the test instead of hanging it.
fn within<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    within_for(WAIT, f)
}

fn within_for<R: Send + 'static>(wait: Duration, f: impl FnOnce() -> R + Send + 'static) -> R {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || tx.send(f()).unwrap());
    rx.recv_timeout(wait).expect("call did not return")
}

#[test]
fn vote_matrix_follows_core_and_hands_off_after_the_vote() {
    for t in [T, P] {
        for prior in [None, Some(MSG), Some(OTHER)] {
            for allow_resign in [false, true] {
                for allow_diff_msg in [false, true] {
                    for has_recovered in [false, true] {
                        let (rt, rec, _) = runtime();
                        if let Some(msg) = prior {
                            assert!(
                                rt.async_sign_if_member(&request(t, msg, false, false)).unwrap()
                            );
                        }
                        if has_recovered {
                            assert!(
                                rt.process_recovered_sig(recovered(t, ID, MSG), false).unwrap()
                            );
                        }
                        rec.take();
                        let req = request(t, MSG, allow_resign, allow_diff_msg);
                        let result = rt.async_sign_if_member(&req).unwrap();
                        let refused = match prior {
                            Some(MSG) => !allow_resign,
                            Some(OTHER) => !allow_diff_msg,
                            _ => false,
                        };
                        assert_eq!(result, !refused);
                        let mut expected = Vec::new();
                        if !refused && !has_recovered {
                            if allow_resign {
                                expected.push(Ev::Reannounce(ID));
                            }
                            expected.push(Ev::Sign(ID));
                        }
                        assert_eq!(rec.take(), expected);
                        let vote = if refused || has_recovered {
                            prior
                        } else {
                            Some(MSG)
                        };
                        assert_eq!(rt.get_vote_for_id(t, &ID).unwrap(), vote);
                    }
                }
            }
        }
    }
}

#[test]
fn signing_needs_a_protx_permission_and_a_valid_member() {
    let (rt, rec, _) = build(None);
    assert!(!rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    let (rt2, rec2, _) = runtime();
    let rt2 = Arc::into_inner(rt2).unwrap().with_signing_allowed(false);
    assert!(!rt2.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    let (rt3, rec3, _) = build(Some(ProTxHash::from([8; 32])));
    assert!(!rt3.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    for (rt, rec) in [(&*rt, &rec), (&rt2, &rec2), (&*rt3, &rec3)] {
        assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), None);
        assert!(rec.take().is_empty());
    }
}

#[test]
fn selection_uses_the_tip_offset_and_reports_the_base_height() {
    let (rt, rec, _) = runtime();
    let selected = rt.select_quorum_for_signing(T, &ID).unwrap().unwrap();
    assert_eq!(
        selected,
        SelectedQuorum {
            quorum_hash: QH.into(),
            base_height: 96
        }
    );
    assert_eq!(rt.select_quorum_for_signing(T, &[7; 32]).unwrap(), None);
    let mut req = request(T, MSG, false, false);
    req.quorum_hash = None;
    assert!(rt.async_sign_if_member(&req).unwrap());
    assert_eq!(rec.take(), vec![Ev::Sign(ID)]);
    req.id = [7; 32];
    assert!(!rt.async_sign_if_member(&req).unwrap());
}

#[test]
fn queries_conflicts_and_platform_per_message_storage() {
    let (rt, _, _) = runtime();
    let a = recovered(T, ID, MSG);
    assert!(rt.process_recovered_sig(a.clone(), false).unwrap());
    assert!(!rt.process_recovered_sig(recovered(T, ID, OTHER), false).unwrap());
    assert!(rt.has_recovered_sig(T, &ID, &MSG).unwrap());
    assert!(!rt.has_recovered_sig(T, &ID, &OTHER).unwrap());
    assert!(rt.has_recovered_sig_for_id(T, &ID).unwrap());
    assert!(rt.has_recovered_sig_for_session(&a.sign_hash()).unwrap());
    assert!(rt.has_recovered_sig_for_signing(T, &ID, &OTHER).unwrap());
    assert_eq!(rt.get_recovered_sig(T, &ID, &MSG).unwrap().as_ref(), Some(&*a));
    assert!(!rt.is_conflicting(T, &ID, &MSG).unwrap());
    assert!(rt.is_conflicting(T, &ID, &OTHER).unwrap());
    assert!(!rt.is_conflicting(T, &[7; 32], &OTHER).unwrap());

    assert!(rt.process_recovered_sig(recovered(P, ID, MSG), false).unwrap());
    assert!(rt.process_recovered_sig(recovered(P, ID, OTHER), false).unwrap());
    assert!(!rt.is_conflicting(P, &ID, &[7; 32]).unwrap());
    assert!(!rt.has_recovered_sig_for_signing(P, &ID, &[7; 32]).unwrap());
    assert!(RecSigReads::has_for_signing(&*rt, P, &ID, &OTHER).unwrap());

    rt.truncate_recovered_sig(T, &ID).unwrap();
    assert!(!rt.has_recovered_sig_for_id(T, &ID).unwrap());
    assert!(rt.already_have(&a.hash()).unwrap());
}

struct Listen {
    n: u8,
    rec: Arc<Rec>,
    result: RecoveredSigResult,
}

impl RecoveredSigListener for Listen {
    fn handle_new_recovered_sig(&self, _: &RecoveredSig) -> RecoveredSigResult {
        self.rec.push(Ev::Listener(self.n));
        self.result.clone()
    }
}

#[test]
fn listeners_run_in_registration_order_then_the_notify() {
    let (rt, rec, _) = runtime();
    let inv = Inventory::new(InvType::IsdLock, Hash256::from_lendian([5; 32]));
    let tx = TxHash::from([6; 32]);
    let mut ids = Vec::new();
    for (n, result) in [
        (0, RecoveredSigResult::Nothing),
        (1, RecoveredSigResult::RelayInv(inv)),
        (2, RecoveredSigResult::RelayTx(tx)),
        (3, RecoveredSigResult::Nothing),
    ] {
        ids.push(rt.register_listener(Arc::new(Listen {
            n,
            rec: rec.clone(),
            result,
        })));
    }
    rt.unregister_listener(ids[0]);
    let a = recovered(T, ID, MSG);
    assert!(rt.process_recovered_sig(a.clone(), true).unwrap());
    assert_eq!(
        rec.take(),
        vec![
            Ev::Listener(1),
            Ev::Inv(inv),
            Ev::Listener(2),
            Ev::Tx(tx),
            Ev::Listener(3),
            Ev::Notify(a.hash(), true),
        ]
    );
    assert!(!rt.process_recovered_sig(a, true).unwrap());
    assert!(rec.take().is_empty());
}

struct Reenter {
    rt: OnceLock<Weak<Runtime>>,
    seen: Mutex<Vec<(bool, bool)>>,
}

impl RecoveredSigListener for Reenter {
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        let rt = self.rt.get().unwrap().upgrade().unwrap();
        let known = rt.has_recovered_sig_for_id(sig.llmq_type, &sig.id).unwrap();
        let signed = rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap();
        rt.push_reconstructed_recovered_sig(recovered(T, [8; 32], MSG));
        self.seen.lock().unwrap().push((known, signed));
        RecoveredSigResult::Nothing
    }
}

#[test]
fn a_listener_may_call_back_into_the_runtime() {
    let (rt, rec, _) = runtime();
    let listener = Arc::new(Reenter {
        rt: OnceLock::new(),
        seen: Mutex::new(Vec::new()),
    });
    listener.rt.set(Arc::downgrade(&rt)).unwrap();
    rt.register_listener(listener.clone());
    let rt2 = rt.clone();
    assert!(within(move || rt2.process_recovered_sig(recovered(T, [7; 32], MSG), false).unwrap()));
    assert_eq!(*listener.seen.lock().unwrap(), vec![(true, true)]);
    assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), Some(MSG));
    assert!(rt.already_have(&recovered(T, [8; 32], MSG).hash()).unwrap());
    assert_eq!(
        rec.take(),
        vec![Ev::Sign(ID), Ev::Notify(recovered(T, [7; 32], MSG).hash(), false)]
    );
}

#[test]
fn proactive_relay_needs_the_flag_and_excludes_large_quorums() {
    for (t, consider, proactive) in [
        (T, true, true),
        (T, false, false),
        (LlmqType::Llmq100_67, true, false),
        (LlmqType::Llmq400_60, true, false),
        (LlmqType::Llmq400_85, true, false),
        (LlmqType::Llmq60_75, true, true),
    ] {
        let (rt, rec, _) = runtime();
        let a = recovered(t, ID, MSG);
        assert!(rt.process_recovered_sig(a.clone(), consider).unwrap());
        assert_eq!(rec.take(), vec![Ev::Notify(a.hash(), proactive)]);
    }
}

#[test]
fn a_reconstructed_sig_suppresses_intake_and_is_processed_first() {
    let (rt, rec, _) = runtime();
    let a = recovered(T, ID, MSG);
    let b = recovered(T, [7; 32], MSG);
    rt.push_reconstructed_recovered_sig(a.clone());
    assert!(rt.already_have(&a.hash()).unwrap());
    let (completion, status) = rt.on_qsigrec(5, &wire(&a), BASIC);
    assert_eq!(status, Ok(IntakeStatus::PendingReconstruction));
    assert_eq!(completion.map(|c| c.hash), Some(a.hash()));
    assert_eq!(rt.on_qsigrec(5, &wire(&b), BASIC).1, Ok(IntakeStatus::Queued));
    assert!(!rt.pending_pass(&mut Seq(1), |_, _| BadSources::new()).unwrap());
    assert_eq!(rec.take(), vec![Ev::Notify(a.hash(), true), Ev::Notify(b.hash(), false)]);
    assert!(rt.already_have(&b.hash()).unwrap());
    assert!(!rt.pending_pass(&mut Seq(1), |_, _| unreachable!()).unwrap());
}

#[test]
fn an_unknown_type_bans_a_remote_peer_only() {
    let (rt, rec, _) = runtime();
    let mut bytes = wire(&recovered(T, ID, MSG));
    bytes[0] = 0xee;
    assert_eq!(rt.on_qsigrec(5, &bytes, BASIC).1, Ok(IntakeStatus::BanUnknownType));
    assert_eq!(rt.on_qsigrec(-1, &bytes, BASIC).1, Ok(IntakeStatus::BanUnknownType));
    assert_eq!(rec.take(), vec![Ev::Ban(5, 100)]);
    assert_eq!(rt.on_qsigrec(5, &bytes[..10], BASIC), (None, Ok(IntakeStatus::Unreadable)));
}

#[test]
fn the_decoded_intake_queues_a_peers_sig_for_the_pending_pass() {
    let (rt, rec, _) = runtime();
    let a = recovered(T, ID, MSG);
    assert_eq!(rt.on_recovered_sig(5, a.clone(), BASIC), Ok(IntakeStatus::Queued));
    assert_eq!(rec.take(), vec![]);
    assert!(!rt.pending_pass(&mut Seq(1), |_, _| BadSources::new()).unwrap());
    assert_eq!(rec.take(), vec![Ev::Notify(a.hash(), false)]);
    assert_eq!(rt.on_recovered_sig(5, a, BASIC), Ok(IntakeStatus::AlreadyKnown));
}

#[test]
fn a_bad_source_is_banned_once_and_purged() {
    let (rt, rec, _) = runtime();
    let sigs: Vec<_> = (10..13).map(|n| recovered(T, [n; 32], MSG)).collect();
    for s in &sigs[..2] {
        assert_eq!(rt.on_qsigrec(5, &wire(s), BASIC).1, Ok(IntakeStatus::Queued));
    }
    assert_eq!(rt.on_qsigrec(-1, &wire(&sigs[2]), BASIC).1, Ok(IntakeStatus::Queued));
    rt.pending_pass(&mut Seq(3), |batch, keys| {
        assert_eq!(batch.by_source[&5].len(), 2);
        assert_eq!(keys.len(), 1);
        [5, -1].into()
    })
    .unwrap();
    assert_eq!(rec.take(), vec![Ev::Ban(5, 100)]);
    assert!(sigs.iter().all(|s| !rt.already_have(&s.hash()).unwrap()));
    assert!(!rt.pending_pass(&mut Seq(3), |_, _| unreachable!()).unwrap());
}

#[test]
fn reads_never_wait_on_a_verification_in_progress() {
    let (rt, rec, _) = runtime();
    let known = recovered(T, [7; 32], MSG);
    assert!(rt.process_recovered_sig(known.clone(), false).unwrap());
    let pending = recovered(T, ID, MSG);
    assert_eq!(rt.on_qsigrec(5, &wire(&pending), BASIC).1, Ok(IntakeStatus::Queued));
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let worker = {
        let rt = rt.clone();
        thread::spawn(move || {
            rt.pending_pass(&mut Seq(5), |_, _| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                BadSources::new()
            })
        })
    };
    started.recv_timeout(WAIT).unwrap();
    let reader = rt.clone();
    let (k, s) = (known.clone(), pending.clone());
    let answers = within(move || {
        (
            RecSigReads::has_for_signing(&*reader, T, &k.id, &k.msg_hash).unwrap(),
            RecSigReads::has_for_session(&*reader, &k.sign_hash()).unwrap(),
            reader.has_recovered_sig_for_id(T, &s.id).unwrap(),
            reader.already_have(&s.hash()).unwrap(),
            reader.async_sign_if_member(&request(T, MSG, false, false)).unwrap(),
            reader.process_recovered_sig(recovered(T, [8; 32], MSG), false).unwrap(),
        )
    });
    assert_eq!(answers, (true, true, false, false, true, true));
    release.send(()).unwrap();
    assert!(!worker.join().unwrap().unwrap());
    assert!(rt.has_recovered_sig_for_id(T, &ID).unwrap());
    assert_eq!(rec.take().last(), Some(&Ev::Notify(pending.hash(), false)));
}

#[test]
fn the_decoded_intake_completes_while_the_worker_is_parked() {
    let (rt, _, _) = runtime();
    assert_eq!(rt.on_recovered_sig(5, recovered(T, ID, MSG), BASIC), Ok(IntakeStatus::Queued));
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let worker = {
        let rt = rt.clone();
        thread::spawn(move || {
            rt.pending_pass(&mut Seq(5), |_, _| {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                BadSources::new()
            })
        })
    };
    started.recv_timeout(WAIT).unwrap();
    let peer = rt.clone();
    let sig = recovered(T, [7; 32], MSG);
    let status = within(move || {
        let status = peer.on_recovered_sig(6, sig, BASIC);
        peer.remove_node(6);
        status
    });
    assert_eq!(status, Ok(IntakeStatus::Queued));
    release.send(()).unwrap();
    assert!(!worker.join().unwrap().unwrap());
}

#[test]
fn writes_and_cleanup_use_the_mock_clock() {
    let (rt, _, clock) = runtime();
    assert!(rt.process_recovered_sig(recovered(T, [7; 32], MSG), false).unwrap());
    assert!(rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    {
        let m = rt.manager();
        let rows = &m.db().store().rows;
        assert!(rows.contains_key(&time_key(b"rs_t", NOW, T, &[7; 32])));
        assert!(rows.contains_key(&time_key(b"rs_vt", NOW, T, &ID)));
    }
    clock.set_mocktime(NOW + 60);
    assert!(rt.cleanup().unwrap());
    assert!(rt.has_recovered_sig_for_id(T, &[7; 32]).unwrap());
    assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), Some(MSG));
    clock.set_mocktime(NOW + 62);
    assert!(!rt.cleanup().unwrap());
    clock.set_mocktime(NOW + 66);
    assert!(rt.cleanup().unwrap());
    assert!(!rt.has_recovered_sig_for_id(T, &[7; 32]).unwrap());
    assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), None);
}

#[test]
fn getdata_serves_only_active_quorums_and_faults_stay_faults() {
    let fail = Arc::new(AtomicBool::new(false));
    let mem = Mem {
        fail: fail.clone(),
        ..Mem::default()
    };
    let (rt, _, _) = build_with(Some(ProTxHash::from(ME)), mem);
    let a = recovered(T, ID, MSG);
    assert!(rt.process_recovered_sig(a.clone(), false).unwrap());
    assert_eq!(rt.for_getdata(&a.hash()).unwrap().as_ref(), Some(&*a));
    let mut inactive = (*a).clone();
    inactive.quorum_hash = [6; 32].into();
    inactive.id = [7; 32];
    assert!(rt.process_recovered_sig(Arc::new(inactive.clone()), false).unwrap());
    assert_eq!(rt.for_getdata(&inactive.hash()).unwrap(), None);
    fail.store(true, Ordering::Relaxed);
    assert!(matches!(rt.has_recovered_sig_for_id(T, &ID), Err(SigningFault::Storage(_))));
    assert!(matches!(
        rt.process_recovered_sig(recovered(T, [8; 32], MSG), false),
        Err(SigningFault::Storage(_))
    ));
    assert!(matches!(
        rt.async_sign_if_member(&request(T, OTHER, false, true)),
        Err(SigningFault::Storage(_))
    ));
}

fn poll_once<F: std::future::Future>(f: F) -> std::task::Poll<F::Output> {
    let mut f = std::pin::pin!(f);
    f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "listener dispatch from a no-wait task")]
fn listener_dispatch_from_a_no_wait_task_panics() {
    let (rt, _, _) = runtime();
    let _ =
        poll_once(no_wait_task(
            async move { rt.process_recovered_sig(recovered(T, ID, MSG), false) },
        ));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "signing quorum lookup from a no-wait task")]
fn a_sign_request_from_a_no_wait_task_panics() {
    let (rt, _, _) = runtime();
    let _ =
        poll_once(no_wait_task(
            async move { rt.async_sign_if_member(&request(T, MSG, false, false)) },
        ));
}

#[test]
fn listener_dispatch_from_an_unmarked_caller_runs() {
    let (rt, rec, _) = runtime();
    rt.register_listener(Arc::new(Listen {
        n: 0,
        rec: rec.clone(),
        result: RecoveredSigResult::Nothing,
    }));
    let a = recovered(T, ID, MSG);
    let rt2 = rt.clone();
    let b = a.clone();
    let stored = within(move || {
        assert!(!no_wait_marked());
        rt2.process_recovered_sig(b, false).unwrap()
    });
    assert!(stored);
    assert_eq!(rec.take(), vec![Ev::Listener(0), Ev::Notify(a.hash(), false)]);
}

#[test]
fn the_sign_queue_is_set_once_and_an_unset_queue_refuses_before_the_vote() {
    let (rt, rec, _) = build_unqueued(Some(ProTxHash::from(ME)), Mem::default());
    assert!(!rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    assert!(rec.take().is_empty());
    assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), None);
    assert!(rt.set_sign_queue(rec.clone()).is_ok());
    assert!(rt.set_sign_queue(rec.clone()).is_err());
    assert!(rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap());
    assert_eq!(rec.take(), vec![Ev::Sign(ID)]);
    assert_eq!(rt.get_vote_for_id(T, &ID).unwrap(), Some(MSG));
    assert_eq!(rt.chain().consensus.llmq_type_platform, P);
}

#[derive(Default)]
struct Flaky {
    inner: FakeLookup,
    fault: AtomicBool,
    missing: AtomicBool,
}

impl QuorumLookup for Flaky {
    type Quorum = FakeQuorum;
    type Error = &'static str;
    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        if self.fault.load(Ordering::Relaxed) {
            return Err("lookup");
        }
        if self.missing.load(Ordering::Relaxed) {
            return Ok(None);
        }
        Ok(self.inner.get_quorum(t, h).unwrap())
    }
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error> {
        Ok(self.inner.is_quorum_active(t, h).unwrap())
    }
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        Ok(self.inner.select_quorum_for_signing(t, id, height, offset).unwrap())
    }
}

impl SigningLookup for Flaky {
    fn base_height(&self, _: &FakeQuorum) -> i32 {
        0
    }
}

#[test]
fn a_lookup_fault_stops_the_pass_and_a_missing_quorum_drops_the_sig() {
    let (rec, clock) = recorder(None);
    let lookup = Arc::new(Flaky {
        inner: lookup(),
        ..Flaky::default()
    });
    let manager = SigningManager::new(Mem::default(), P, 60);
    let rt = SigningRuntime::new(manager, lookup.clone(), Arc::new(chain()), handles(&rec, &clock));
    let (a, b) = (recovered(T, ID, MSG), recovered(T, [7; 32], MSG));
    assert_eq!(rt.on_qsigrec(5, &wire(&a), BASIC).1, Ok(IntakeStatus::Queued));
    lookup.fault.store(true, Ordering::Relaxed);
    let fault = rt.pending_pass(&mut Seq(1), |_, _| unreachable!());
    assert_eq!(fault, Err(SigningFault::Lookup("\"lookup\"".into())));
    assert!(matches!(rt.on_qsigrec(5, &wire(&b), BASIC).1, Err(SigningFault::Lookup(_))));

    lookup.fault.store(false, Ordering::Relaxed);
    assert_eq!(rt.on_qsigrec(5, &wire(&b), BASIC).1, Ok(IntakeStatus::Queued));
    lookup.missing.store(true, Ordering::Relaxed);
    assert_eq!(
        rt.pending_pass(&mut Seq(1), |batch, _| {
            assert!(batch.by_source.values().all(Vec::is_empty));
            BadSources::new()
        }),
        Ok(false)
    );
    lookup.missing.store(false, Ordering::Relaxed);
    assert!(!rt.already_have(&b.hash()).unwrap());
    assert!(rec.take().is_empty());
}

/// A lookup whose first call after `arm` reports on `entered` and waits for `release`.
struct Parked {
    inner: FakeLookup,
    armed: AtomicBool,
    entered: Mutex<Option<mpsc::Sender<()>>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

impl Parked {
    fn arm(&self) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        *self.entered.lock().unwrap() = Some(entered_tx);
        *self.release.lock().unwrap() = Some(release_rx);
        self.armed.store(true, Ordering::SeqCst);
        (entered, release)
    }

    fn park(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            let release = self.release.lock().unwrap().take().unwrap();
            self.entered.lock().unwrap().take().unwrap().send(()).unwrap();
            let _ = release.recv();
        }
    }
}

impl QuorumLookup for Parked {
    type Quorum = FakeQuorum;
    type Error = &'static str;
    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        self.park();
        Ok(self.inner.get_quorum(t, h).unwrap())
    }
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error> {
        self.park();
        Ok(self.inner.is_quorum_active(t, h).unwrap())
    }
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        self.park();
        Ok(self.inner.select_quorum_for_signing(t, id, height, offset).unwrap())
    }
}

impl SigningLookup for Parked {
    fn base_height(&self, q: &FakeQuorum) -> i32 {
        self.inner.base_height(q)
    }
}

type ParkedRuntime = SigningRuntime<Mem, Parked>;
type ParkedCall = Box<dyn Fn(&ParkedRuntime) + Send>;

#[test]
fn reads_never_wait_on_a_parked_quorum_lookup() {
    let known = recovered(T, [7; 32], MSG);
    let unhashed = SignRequest {
        quorum_hash: None,
        ..request(T, MSG, false, false)
    };
    let cases: Vec<(&str, ParkedCall)> = vec![
        (
            "on_qsigrec",
            Box::new(|rt| {
                let pending = recovered(T, ID, MSG);
                assert_eq!(rt.on_qsigrec(5, &wire(&pending), BASIC).1, Ok(IntakeStatus::Queued));
            }),
        ),
        (
            "async_sign_if_member by quorum hash",
            Box::new(|rt| {
                assert!(rt.async_sign_if_member(&request(T, MSG, false, false)).unwrap())
            }),
        ),
        (
            "async_sign_if_member by selection",
            Box::new(move |rt| assert!(rt.async_sign_if_member(&unhashed).unwrap())),
        ),
        (
            "for_getdata",
            Box::new(|rt| {
                let known = recovered(T, [7; 32], MSG);
                assert_eq!(rt.for_getdata(&known.hash()).unwrap().as_ref(), Some(&*known));
            }),
        ),
    ];
    for (name, call) in cases {
        let (rec, clock) = recorder(Some(ProTxHash::from(ME)));
        let lookup = Arc::new(Parked {
            inner: lookup(),
            armed: AtomicBool::new(false),
            entered: Mutex::new(None),
            release: Mutex::new(None),
        });
        let manager = SigningManager::new(Mem::default(), P, 60);
        let rt = Arc::new(SigningRuntime::new(
            manager,
            lookup.clone(),
            Arc::new(chain()),
            handles(&rec, &clock),
        ));
        assert!(rt.set_sign_queue(rec.clone()).is_ok());
        assert!(rt.process_recovered_sig(known.clone(), false).unwrap());
        let (entered, release) = lookup.arm();
        let worker = {
            let rt = rt.clone();
            thread::spawn(move || call(&rt))
        };
        entered.recv_timeout(WAIT).unwrap_or_else(|_| panic!("{name}: lookup not reached"));
        let reader = rt.clone();
        let k = known.clone();
        let answers = within_for(Duration::from_secs(5), move || {
            (
                reader.has_recovered_sig_for_id(T, &k.id).unwrap(),
                RecSigReads::has_for_signing(&*reader, T, &k.id, &k.msg_hash).unwrap(),
                reader.already_have(&k.hash()).unwrap(),
                reader.get_vote_for_id(T, &ID).unwrap(),
                reader.process_recovered_sig(recovered(T, [8; 32], MSG), false).unwrap(),
            )
        });
        assert_eq!(answers, (true, true, true, None, true), "{name}");
        release.send(()).unwrap();
        worker.join().unwrap_or_else(|_| panic!("{name}: call failed"));
    }
}

/// Records the thread each dispatch runs on, and whether it is marked.
struct OnThread(Mutex<mpsc::Sender<(bool, Option<String>)>>);

impl RecoveredSigListener for OnThread {
    fn handle_new_recovered_sig(&self, _: &RecoveredSig) -> RecoveredSigResult {
        let name = thread::current().name().map(str::to_owned);
        let _ = self.0.lock().unwrap().send((no_wait_marked(), name));
        RecoveredSigResult::Nothing
    }
}

fn work(rt: &Arc<Runtime>) -> Arc<RuntimeWork<Mem, FakeLookup, Seq>> {
    Arc::new(RuntimeWork::new(rt.clone(), Box::new(|| BASIC), Seq(1)))
}

fn abort_channel() -> (RecSigsAbort, mpsc::Receiver<SigningFault>) {
    let (tx, rx) = mpsc::channel();
    let abort: RecSigsAbort = Box::new(move |fault| {
        let _ = tx.send(fault);
    });
    (abort, rx)
}

/// The worker runs listener dispatch on its own thread, "recsigs", with no no-wait marker.
// Core: v24.0.2 src/llmq/net_signing.cpp:152-160, :198-212
#[test]
fn the_worker_dispatches_listeners_on_its_unmarked_thread() {
    let (rt, rec, _) = runtime();
    let (tx, seen) = mpsc::channel();
    rt.register_listener(Arc::new(OnThread(Mutex::new(tx))));
    let (abort, aborted) = abort_channel();
    let interrupt = SigningInterrupt::new();
    let mut worker = RecSigsWorker::start(work(&rt), interrupt.clone(), abort, std_spawn).unwrap();
    let a = recovered(T, ID, MSG);
    rt.push_reconstructed_recovered_sig(a.clone());
    assert_eq!(seen.recv_timeout(WAIT), Ok((false, Some("recsigs".into()))));
    interrupt.trigger();
    worker.stop();
    assert!(aborted.try_recv().is_err());
    assert_eq!(rec.take(), vec![Ev::Notify(a.hash(), true)]);
}

/// Through the worker's verification, a source whose signatures fail is scored 100 once and its
/// queue purged; failing signatures from the node itself (-1) are dropped unscored.
// Core: v24.0.2 src/llmq/net_signing.cpp:236-268, :307-320
#[test]
fn the_worker_bans_a_bad_source_once_and_never_the_node_itself() {
    let (rt, rec, _) = runtime();
    let sigs: Vec<_> = (10..13).map(|n| recovered(T, [n; 32], MSG)).collect();
    for s in &sigs[..2] {
        assert_eq!(rt.on_qsigrec(5, &wire(s), BASIC).1, Ok(IntakeStatus::Queued));
    }
    assert_eq!(rt.on_qsigrec(-1, &wire(&sigs[2]), BASIC).1, Ok(IntakeStatus::Queued));
    let work = work(&rt);
    assert!(!work.process_pending().unwrap());
    assert_eq!(rec.take(), vec![Ev::Ban(5, 100)]);
    assert!(sigs.iter().all(|s| !rt.has_recovered_sig_for_id(T, &s.id).unwrap()));
    assert!(sigs.iter().all(|s| !rt.already_have(&s.hash()).unwrap()));
    assert!(!work.process_pending().unwrap());
    assert!(rec.take().is_empty());
}

/// Over the worker's loop on the mock clock, one second per idle tick: no pending work sleeps
/// every tick, and `Cleanup` runs at the first tick and then every five seconds.
// Core: v24.0.2 src/llmq/net_signing.cpp:283-298; src/util/time.h:40-66 CleanupThrottler
#[test]
fn the_worker_loop_cleans_up_every_five_seconds_of_mock_time() {
    struct Counting(Arc<RuntimeWork<Mem, FakeLookup, Seq>>, Mutex<Vec<i64>>, Arc<MockClock>);
    impl RecSigsWork for Counting {
        fn process_pending(&self) -> Result<bool, SigningFault> {
            self.0.process_pending()
        }
        fn cleanup(&self) -> Result<bool, SigningFault> {
            let ran = self.0.cleanup()?;
            if ran {
                self.1.lock().unwrap().push(self.2.mocktime() - NOW);
            }
            Ok(ran)
        }
    }
    let (rt, _, clock) = runtime();
    let counting = Counting(work(&rt), Mutex::default(), clock.clone());
    let mut ticks = Vec::new();
    let sleep = |d| {
        ticks.push(d);
        clock.set_mocktime(clock.mocktime() + 1);
        ticks.len() < 12
    };
    assert_eq!(crate::signing::worker::run(&counting, &SigningInterrupt::new(), sleep), Ok(()));
    assert_eq!(ticks, [IDLE_SLEEP; 12]);
    assert_eq!(*counting.1.lock().unwrap(), [0, 5, 10]);
}

/// A storage fault in the worker's pass reaches the abort and ends the thread.
#[test]
fn a_worker_storage_fault_reaches_the_abort() {
    let fail = Arc::new(AtomicBool::new(false));
    let (rt, _, _) = build_with(
        Some(ProTxHash::from(ME)),
        Mem {
            fail: fail.clone(),
            ..Mem::default()
        },
    );
    fail.store(true, Ordering::Relaxed);
    rt.push_reconstructed_recovered_sig(recovered(T, ID, MSG));
    let (abort, aborted) = abort_channel();
    let interrupt = SigningInterrupt::new();
    let mut worker = RecSigsWorker::start(work(&rt), interrupt.clone(), abort, std_spawn).unwrap();
    assert!(matches!(aborted.recv_timeout(WAIT), Ok(SigningFault::Storage(_))));
    interrupt.trigger();
    worker.stop();
    assert!(aborted.try_recv().is_err());
}
