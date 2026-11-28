use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{OnceLock, Weak};

use dash_evo::types::ProTxHash;
use dash_llmq::dkg::bls::BlsCtx;
use dash_llmq::dkg::bls::{BlsPkValue, BlsSkValue};
use dash_llmq::dkg::wire::SigShareBytes;
use dash_llmq::sigenv::fakes::{FakeLookup, FakeNet, FakeOperatorSigner, FakeQuorum, FakeRecovery};
use dash_llmq::sigenv::{Now, PeerFacts, Recovered, SigMsg, SignerFacts, SporkFacts};
use dash_llmq::sigshares::verify::ReferenceVerifier;
use dash_params::{ChainParamsBuilder, LlmqParams, Network, Release};
use dashd_chainstate::chain::reply::no_wait_task;

use super::*;

const T: LlmqType = LlmqType::LlmqTest;
const QH: [u8; 32] = [4; 32];
const ID: [u8; 32] = [1; 32];
const MSG: [u8; 32] = [2; 32];
const PEER: PeerId = 7;
const NOW_MS: i64 = 1_700_000_000_000;
const NEVER: i64 = 4_070_908_800;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ev {
    Misbehaving(PeerId, i32),
    RemoveNode(PeerId, Option<usize>),
    Recovered {
        consider: bool,
        s_free: bool,
    },
    Send(PeerId, Vec<&'static str>),
    UnderS(&'static str),
}

#[derive(Default)]
struct Log(Mutex<Vec<Ev>>);

impl Log {
    fn push(&self, ev: Ev) {
        self.0.lock().unwrap().push(ev);
    }
    fn take(&self) -> Vec<Ev> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

type RecoverFn = fn(&LlmqParams, &[ProTxHash], &BTreeMap<u16, BlsSigValue>, &mut u32) -> Recovered;
type OrderFn = fn(&[ProTxHash], &[u8; 32]) -> Vec<usize>;
type Rt = SigSharesRuntime<Probe, Intake, FakeNet, Out, Facts, FakeRecovery<RecoverFn, OrderFn>>;

fn rt_of(cell: &OnceLock<Weak<Rt>>) -> Option<Arc<Rt>> {
    cell.get().and_then(Weak::upgrade)
}

fn s_free(rt: &Rt) -> bool {
    rt.shares.try_lock().is_ok()
}

struct Probe {
    inner: FakeLookup,
    rt: OnceLock<Weak<Rt>>,
    log: Arc<Log>,
    fail: AtomicBool,
    gone: AtomicBool,
}

impl Probe {
    fn enter(&self, what: &'static str) -> Result<(), &'static str> {
        if rt_of(&self.rt).is_some_and(|rt| !s_free(&rt)) {
            self.log.push(Ev::UnderS(what));
        }
        if self.fail.load(Ordering::Relaxed) {
            return Err("lookup");
        }
        Ok(())
    }
}

impl QuorumLookup for Probe {
    type Quorum = FakeQuorum;
    type Error = &'static str;

    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Self::Error> {
        self.enter("get_quorum")?;
        if self.gone.load(Ordering::Relaxed) {
            return Ok(None);
        }
        Ok(self.inner.get_quorum(t, h).unwrap())
    }

    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Self::Error> {
        self.enter("is_quorum_active")?;
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

struct Intake {
    rt: OnceLock<Weak<Rt>>,
    log: Arc<Log>,
    quorum: Arc<FakeQuorum>,
    read_fault: Mutex<Option<SigningFault>>,
    process_fault: Mutex<Option<SigningFault>>,
}

impl RecoveredSigIntake for Intake {
    fn process_recovered_sig(
        &self,
        _: Arc<RecoveredSig>,
        consider: bool,
    ) -> Result<bool, SigningFault> {
        let rt = rt_of(&self.rt).unwrap();
        let free = s_free(&rt);
        self.log.push(Ev::Recovered {
            consider,
            s_free: free,
        });
        if free {
            rt.force_reannouncement(&self.quorum, T, &ID, &MSG);
        }
        self.process_fault.lock().unwrap().clone().map_or(Ok(true), Err)
    }

    fn push_reconstructed_recovered_sig(&self, _: Arc<RecoveredSig>) {
        unreachable!("the sig-share runtime never reconstructs")
    }

    fn remove_node(&self, peer: PeerId) {
        let pending = rt_of(&self.rt)
            .and_then(|rt| rt.shares.try_lock().ok().map(|m| m.pending_incoming_count()));
        self.log.push(Ev::RemoveNode(peer, pending));
    }
}

impl RecSigReads for Intake {
    type Error = SigningFault;

    fn has_for_signing(
        &self,
        _: LlmqType,
        _: &[u8; 32],
        _: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        self.read_fault.lock().unwrap().clone().map_or(Ok(false), Err)
    }

    fn has_for_session(&self, _: &[u8; 32]) -> Result<bool, SigningFault> {
        self.read_fault.lock().unwrap().clone().map_or(Ok(false), Err)
    }
}

struct Out(Arc<Log>);

fn tag(m: &SigMsg) -> &'static str {
    match m {
        SigMsg::SesAnn(_) => "qsigsesann",
        SigMsg::GetSigShares(_) => "qgetsigs",
        SigMsg::BSigShares(_) => "qbsigs",
        SigMsg::SigSharesInv(_) => "qsigsinv",
        SigMsg::SigShare(_) => "qsigshare",
    }
}

impl SharesOut for Out {
    fn send(&self, peer: PeerId, msgs: Vec<SigMsg>) {
        self.0.push(Ev::Send(peer, msgs.iter().map(tag).collect()));
    }
    fn misbehaving(&self, peer: PeerId, score: i32) {
        self.0.push(Ev::Misbehaving(peer, score));
    }
}

struct Facts {
    me: ProTxHash,
    spork21_value: i64,
    now_ms: AtomicI64,
}

struct Xs(u64);

impl rand_core::TryRng for Xs {
    type Error = std::convert::Infallible;
    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        Ok((self.try_next_u64()? >> 32) as u32)
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        Ok(self.0)
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        for b in dst {
            *b = self.try_next_u64()? as u8;
        }
        Ok(())
    }
}

impl SharesFacts for Facts {
    type Rng = Xs;
    fn signer(&self) -> SignerFacts {
        SignerFacts {
            my_pro_tx: Some(self.me),
            signing_allowed: true,
        }
    }
    fn sporks(&self) -> SporkFacts {
        SporkFacts {
            spork21_active: true,
            spork21_value: self.spork21_value,
        }
    }
    fn now(&self) -> Now {
        Now {
            ms: self.now_ms.load(Ordering::Relaxed),
        }
    }
    fn bls(&self) -> BlsCtx {
        BlsCtx::BASIC
    }
    fn rng(&self) -> Xs {
        Xs(5)
    }
}

fn scalar(n: u8) -> [u8; 32] {
    let mut b = [0; 32];
    b[31] = n;
    b
}

fn quorum(threshold: u16) -> Arc<FakeQuorum> {
    let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let mut params = *setup.consensus.llmq(T).unwrap();
    params.threshold = threshold;
    Arc::new(FakeQuorum {
        llmq_type: T,
        quorum_hash: QH.into(),
        params,
        members: (1..=3).map(|i| ProTxHash::from(scalar(i))).collect(),
        valid_members: [0, 1, 2].into(),
        has_verification_vector: true,
        quorum_public_key: BlsPkValue::invalid(),
        pk_shares: BTreeMap::new(),
        sk_share: BlsSkValue::from_canonical(&scalar(7), BlsSchemeKind::Basic),
    })
}

struct Fx {
    rt: Arc<Rt>,
    log: Arc<Log>,
    aborts: Arc<Mutex<Vec<String>>>,
    q: Arc<FakeQuorum>,
}

fn first_share(
    _: &LlmqParams,
    _: &[ProTxHash],
    s: &BTreeMap<u16, BlsSigValue>,
    _: &mut u32,
) -> Recovered {
    Recovered::Sig {
        sig: s.values().next().unwrap().clone(),
        self_verify: false,
    }
}

fn in_order(members: &[ProTxHash], _: &[u8; 32]) -> Vec<usize> {
    (0..members.len()).collect()
}

fn fixture(threshold: u16, spork21_value: i64) -> Fx {
    fixture_with(quorum(threshold), spork21_value, Arc::new(FakeOperatorSigner::default()))
}

fn fixture_with(
    q: Arc<FakeQuorum>,
    spork21_value: i64,
    operator: Arc<dyn OperatorSigner + Send + Sync>,
) -> Fx {
    fixture_connected(q, spork21_value, operator, &[])
}

/// `fixture_with`, with `others` also fully connected (verified as member 2), so the cleaning
/// pass keeps their node states as Core keeps a connected peer's.
// Core: v24.0.2 src/llmq/signing_shares.cpp:1432-1443
fn fixture_connected(
    q: Arc<FakeQuorum>,
    spork21_value: i64,
    operator: Arc<dyn OperatorSigner + Send + Sync>,
    others: &[PeerId],
) -> Fx {
    let log = Arc::new(Log::default());
    let mut inner = FakeLookup::default();
    inner.quorums.insert((T.to_base(), q.quorum_hash), q.clone());
    inner.active.insert((T.to_base(), q.quorum_hash));
    let aborts = Arc::new(Mutex::new(Vec::new()));
    let sink = aborts.clone();
    let h = SharesHandles {
        lookup: Arc::new(Probe {
            inner,
            rt: OnceLock::new(),
            log: log.clone(),
            fail: AtomicBool::new(false),
            gone: AtomicBool::new(false),
        }),
        intake: Arc::new(Intake {
            rt: OnceLock::new(),
            log: log.clone(),
            quorum: q.clone(),
            read_fault: Mutex::new(None),
            process_fault: Mutex::new(None),
        }),
        net: Arc::new(FakeNet {
            quorum_peers: [((T.to_base(), q.quorum_hash), vec![PEER])].into(),
            connected: [(PEER, 1)]
                .into_iter()
                .chain(others.iter().map(|&p| (p, 2)))
                .map(|(id, member)| PeerFacts {
                    id,
                    verified_pro_tx: Some(q.members[member]),
                    common_version: 70_235,
                })
                .collect(),
            banned: Default::default(),
        }),
        out: Arc::new(Out(log.clone())),
        facts: Arc::new(Facts {
            me: q.members[0],
            spork21_value,
            now_ms: AtomicI64::new(NOW_MS),
        }),
        recovery: Arc::new(FakeRecovery {
            recover: first_share as RecoverFn,
            order: in_order as OrderFn,
        }),
        operator,
        chain: Arc::new(ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap()),
        abort: Arc::new(move |e: &str| sink.lock().unwrap().push(e.to_owned())),
    };
    let rt = Arc::new(SigSharesRuntime::new(h));
    rt.h.lookup.rt.set(Arc::downgrade(&rt)).unwrap();
    rt.h.intake.rt.set(Arc::downgrade(&rt)).unwrap();
    Fx {
        rt,
        log,
        aborts,
        q,
    }
}

fn share(member: u16) -> SigShare {
    SigShare::new(T, QH.into(), member, ID.into(), MSG.into(), SigShareBytes::from_bytes([0; 96]))
}

fn ann(session: u32) -> SigSesAnn {
    SigSesAnn::new(session, T, QH.into(), ID.into(), MSG.into())
}

fn pending(fx: &Fx) -> usize {
    fx.rt.shares().pending_incoming_count()
}

fn aborts(fx: &Fx) -> usize {
    fx.aborts.lock().unwrap().len()
}

#[test]
fn ban_node_scores_then_purges_signing_then_marks_shares() {
    let fx = fixture(2, 0);
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    assert_eq!(pending(&fx), 1);
    fx.rt.ban_node(PEER, true);
    assert_eq!(fx.log.take(), vec![Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(1))]);
    assert_eq!(pending(&fx), 0);
    fx.rt.on_sig_share(PEER, vec![share(2)]);
    assert_eq!(pending(&fx), 0, "a marked peer's shares are refused");
}

#[test]
fn ban_node_skips_minus_one_and_marks_only_when_asked() {
    let fx = fixture(2, 0);
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    fx.rt.ban_node(-1, true);
    assert!(fx.log.take().is_empty());
    fx.rt.ban_node(PEER, false);
    assert_eq!(fx.log.take(), vec![Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(1))]);
    assert_eq!(pending(&fx), 1);
    fx.rt.ban(PEER);
    assert_eq!(fx.log.take(), vec![Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(1))]);
    assert_eq!(pending(&fx), 0);
}

#[test]
fn a_ban_effect_runs_ban_node_with_its_mark() {
    let fx = fixture(2, 0);
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    fx.rt.on_ses_ann(PEER, vec![ann(u32::MAX)]);
    assert_eq!(fx.log.take(), vec![Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(1))]);
    assert_eq!(pending(&fx), 0);
}

#[test]
fn recovered_sigs_are_processed_with_s_released_and_relay_considered() {
    let fx = fixture(1, 0);
    fx.rt.async_sign(fx.q.clone(), ID, MSG);
    assert_eq!(fx.rt.dispatch_pass(&ReferenceVerifier), None);
    assert_eq!(
        fx.log.take(),
        vec![Ev::Recovered {
            consider: true,
            s_free: true,
        }]
    );
    assert_eq!(aborts(&fx), 0);
}

#[test]
fn a_read_fault_aborts_once_and_stopped_never() {
    let fx = fixture(2, 0);
    *fx.rt.h.intake.read_fault.lock().unwrap() = Some(SigningFault::Storage("db".into()));
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    assert_eq!(
        *fx.aborts.lock().unwrap(),
        vec!["recovered-signature storage fault: db".to_owned()]
    );
    assert_eq!(pending(&fx), 0);

    let fx = fixture(2, 0);
    *fx.rt.h.intake.read_fault.lock().unwrap() = Some(SigningFault::Stopped);
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    assert_eq!(pending(&fx), 0);
    assert_eq!(aborts(&fx), 0);
}

fn signed_share(member: u16) -> SigShare {
    let sk = BlsSkValue::from_canonical(&scalar(9), BlsSchemeKind::Basic).unwrap();
    let sig = dash_llmq::dkg::bls::sign(&sk, &[3; 32], false).to_bytes(BlsSchemeKind::Basic);
    SigShare::new(T, QH.into(), member, ID.into(), MSG.into(), SigShareBytes::from_bytes(sig))
}

#[test]
fn verification_faults_abort_except_stopped_reads() {
    let fx = fixture(2, 0);
    fx.rt.on_sig_share(PEER, vec![signed_share(1)]);
    fx.rt.dispatch_pass(&ReferenceVerifier);
    let aborts = fx.aborts.lock().unwrap().clone();
    assert_eq!(aborts.len(), 1);
    assert!(aborts[0].contains("MissingPubKeyShare"), "{aborts:?}");

    let fx = fixture(2, 0);
    fx.rt.on_sig_share(PEER, vec![signed_share(1)]);
    *fx.rt.h.intake.read_fault.lock().unwrap() = Some(SigningFault::Stopped);
    fx.rt.dispatch_pass(&ReferenceVerifier);
    assert_eq!(pending(&fx), 0);
    assert!(fx.aborts.lock().unwrap().is_empty());
}

#[test]
fn abort_fires_once_however_many_faults() {
    let fx = fixture(2, 0);
    *fx.rt.h.intake.read_fault.lock().unwrap() = Some(SigningFault::Storage("db".into()));
    fx.rt.on_sig_share(PEER, vec![share(1), share(2)]);
    fx.rt.on_sig_share(PEER, vec![share(1)]);
    assert_eq!(aborts(&fx), 1);
}

#[test]
fn a_lookup_fault_under_s_aborts() {
    let fx = fixture(2, 0);
    fx.rt.async_sign(fx.q.clone(), ID, MSG);
    fx.rt.dispatch_pass(&ReferenceVerifier);
    assert_eq!(aborts(&fx), 0);
    fx.rt.h.lookup.fail.store(true, Ordering::Relaxed);
    fx.rt.maint_pass();
    assert_eq!(*fx.aborts.lock().unwrap(), vec!["quorum lookup fault: \"lookup\"".to_owned()]);
}

#[test]
fn unknown_types_are_banned_without_a_lookup() {
    let fx = fixture(2, 0);
    fx.rt.h.lookup.fail.store(true, Ordering::Relaxed);
    let unknown = LlmqType::Unknown(250);
    fx.rt.on_ses_ann(PEER, vec![SigSesAnn::new(5, unknown, QH.into(), ID.into(), MSG.into())]);
    let bad = SigShare::new(unknown, QH.into(), 0, ID.into(), MSG.into(), [0; 96].into());
    fx.rt.on_sig_share(PEER, vec![bad]);
    let ban = [Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(0))];
    assert_eq!(fx.log.take(), [ban.clone(), ban].concat());
    assert_eq!(aborts(&fx), 0);
}

#[test]
fn a_type_the_chain_does_not_run_is_banned_and_ends_the_message() {
    let unrun = LlmqType::Llmq400_60;
    let fx = fixture(2, 0);
    assert!(fx.rt.h.chain.consensus.llmq(unrun).is_none());
    fx.rt
        .on_ses_ann(PEER, vec![ann(5), SigSesAnn::new(6, unrun, QH.into(), ID.into(), MSG.into())]);
    assert_eq!(fx.log.take(), [Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(0))]);
    assert!(fx.rt.session_info(PEER, 5).is_some());
    assert!(fx.rt.session_info(PEER, 6).is_none());

    let fx = fixture(2, 0);
    let bad = SigShare::new(
        unrun,
        QH.into(),
        0,
        ID.into(),
        MSG.into(),
        SigShareBytes::from_bytes([0; 96]),
    );
    fx.rt.on_sig_share(PEER, vec![share(1), bad, share(2)]);
    assert_eq!(fx.log.take(), [Ev::Misbehaving(PEER, 100), Ev::RemoveNode(PEER, Some(1))]);
    assert_eq!(pending(&fx), 0);
    assert_eq!(aborts(&fx), 0);
}

#[test]
fn a_process_fault_aborts_once_and_stopped_never() {
    for (fault, expected) in [(SigningFault::Lookup("v".into()), 1), (SigningFault::Stopped, 0)] {
        let fx = fixture(1, 0);
        *fx.rt.h.intake.process_fault.lock().unwrap() = Some(fault);
        fx.rt.async_sign(fx.q.clone(), ID, MSG);
        fx.rt.dispatch_pass(&ReferenceVerifier);
        assert_eq!(aborts(&fx), expected);
    }
}

#[test]
fn a_lookup_fault_aborts_once() {
    let fx = fixture(2, 0);
    fx.rt.h.lookup.fail.store(true, Ordering::Relaxed);
    fx.rt.on_ses_ann(PEER, vec![ann(5)]);
    assert_eq!(*fx.aborts.lock().unwrap(), vec!["quorum lookup fault: \"lookup\"".to_owned()]);
    assert!(fx.rt.session_info(PEER, 5).is_none());
}

#[test]
fn ses_ann_and_sig_share_look_quorums_up_before_s() {
    let fx = fixture(2, 0);
    fx.rt.on_ses_ann(PEER, vec![ann(5), ann(6)]);
    fx.rt.on_sig_share(PEER, vec![share(1), share(2)]);
    assert!(fx.log.take().is_empty());
    assert!(Arc::ptr_eq(&fx.rt.session_info(PEER, 6).unwrap().quorum.unwrap(), &fx.q));
    assert_eq!(pending(&fx), 2);
}

#[test]
fn the_listener_purges_the_recovered_session() {
    let fx = fixture(2, 0);
    fx.rt.on_ses_ann(PEER, vec![ann(5)]);
    let info = fx.rt.session_info(PEER, 5).unwrap();
    let rs = RecoveredSig {
        llmq_type: T,
        quorum_hash: QH.into(),
        id: ID,
        msg_hash: MSG,
        sig: [0; 96],
    };
    assert_eq!(rs.sign_hash(), info.sign_hash);
    assert_eq!(fx.rt.handle_new_recovered_sig(&rs), RecoveredSigResult::Nothing);
    assert!(fx.rt.session_info(PEER, 5).is_none());
}

fn sends(fx: &Fx) -> Vec<Ev> {
    fx.log.take().into_iter().filter(|e| matches!(e, Ev::Send(..))).collect()
}

#[test]
fn force_reannouncement_reannounces_known_shares() {
    let fx = fixture(2, NEVER);
    fx.rt.async_sign(fx.q.clone(), ID, MSG);
    fx.rt.dispatch_pass(&ReferenceVerifier);
    fx.rt.maint_pass();
    assert_eq!(sends(&fx), vec![Ev::Send(PEER, vec!["qsigsesann", "qbsigs"])]);
    fx.rt.maint_pass();
    assert!(sends(&fx).is_empty());
    fx.rt.force_reannouncement(&fx.q, T, &ID, &MSG);
    fx.rt.maint_pass();
    assert_eq!(sends(&fx), vec![Ev::Send(PEER, vec!["qsigsesann", "qsigsinv"])]);
}

#[test]
fn resign_restarts_the_signing_session_timeout_with_the_spork_on() {
    let fx = fixture(2, 0);
    let at = |ms: i64| fx.rt.h.facts.now_ms.store(NOW_MS + ms, Ordering::Relaxed);
    fx.rt.on_ses_ann(PEER, vec![ann(5)]);
    assert!(fx.rt.session_info(PEER, 5).is_some());
    at(50_000);
    fx.rt.force_reannouncement(&fx.q, T, &ID, &MSG);
    at(70_000);
    fx.rt.maint_pass();
    assert!(fx.rt.session_info(PEER, 5).is_some(), "kept after the resign");
    at(110_000);
    fx.rt.maint_pass();
    assert!(fx.rt.session_info(PEER, 5).is_none(), "removed sixty seconds after the resign");
}

fn poll_once<Fut: std::future::Future>(f: Fut) -> std::task::Poll<Fut::Output> {
    let mut f = std::pin::pin!(f);
    f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "sig-share queue call from a no-wait task")]
fn async_sign_from_a_no_wait_task_panics() {
    let fx = fixture(2, 0);
    let _ = poll_once(no_wait_task(async move { fx.rt.async_sign(fx.q.clone(), ID, MSG) }));
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "sig-share queue call from a no-wait task")]
fn force_reannouncement_from_a_no_wait_task_panics() {
    let fx = fixture(2, 0);
    let _ = poll_once(no_wait_task(async move { fx.rt.force_reannouncement(&fx.q, T, &ID, &MSG) }));
}

/// Core's llmq-sigs lines for our own share and its concentrated send.
// Core: v24.0.2 src/llmq/signing_shares.cpp:1053-1075, :1566-1650
mod logs {
    use dash_evo::locks::sign_hash;
    use dash_llmq::dkg::bls::BlsSigValue;
    use dash_llmq::sigshares::verify::ReferenceVerifier;

    use super::workers::logged;
    use super::*;

    const MODULE: &str = "dashd_quorums::sigshares::runtime";

    fn our_sign_hash() -> String {
        hex(&sign_hash(T, &QuorumHash::from(QH), &ID, &MSG))
    }

    /// `line` with its trailing `time=<ms>` checked as a number and dropped.
    fn untimed(line: &str) -> String {
        let (head, ms) = line.rsplit_once("time=").expect("a time field");
        assert!(ms.parse::<u128>().is_ok(), "{line}");
        format!("{head}time=")
    }

    fn share_lines(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .filter(|l| {
                l.contains("CreateSigShare") || l.contains("CollectSigSharesToSendConcentrated")
            })
            .collect()
    }

    fn single(sk_share: Option<BlsSkValue>) -> Arc<FakeQuorum> {
        let mut q = Arc::into_inner(quorum(1)).unwrap();
        q.params.size = 1;
        q.sk_share = sk_share;
        Arc::new(q)
    }

    struct InvalidSig;

    impl OperatorSigner for InvalidSig {
        fn sign(&self, _: &[u8; 32], _: BlsSchemeKind) -> Option<BlsSigValue> {
            Some(BlsSigValue::invalid())
        }
    }

    #[test]
    fn a_sign_task_logs_created_and_its_concentrated_send() {
        let fx = fixture(3, 0);
        fx.rt.async_sign(fx.q.clone(), ID, MSG);
        let ((), lines) = logged(|| {
            fx.rt.dispatch_pass(&ReferenceVerifier);
            fx.rt.maint_pass();
        });
        let lines = share_lines(lines);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let sh = our_sign_hash();
        assert_eq!(
            untimed(&lines[0]),
            format!(
                "DEBUG llmq-sigs message=CSigSharesManager::CreateSigShare -- created sigShare. signHash={sh}, id={}, msgHash={}, llmqType=100, quorum={}, time=",
                hex(&ID),
                hex(&MSG),
                hex(&QH)
            )
        );
        assert_eq!(
            lines[1],
            format!(
                "DEBUG llmq-sigs message=CSigSharesManager::CollectSigSharesToSendConcentrated -- signHash={sh}, sending to {}, attempt=1",
                hex(fx.q.members[0].as_bytes())
            )
        );
        assert_eq!(aborts(&fx), 0);
    }

    #[test]
    fn a_skipped_recovery_member_logs_cores_line() {
        let ((), lines) = logged(|| {
            log_concentrated(&ConcentratedSend::Skipped {
                sign_hash: [0xab; 32],
                count: 2,
            })
        });
        assert_eq!(
            lines,
            [format!(
                "DEBUG llmq-sigs message=CSigSharesManager::CollectSigSharesToSendConcentrated -- signHash={}, skipped 2 unreachable recovery members",
                "ab".repeat(32)
            )]
        );
    }

    #[test]
    fn without_a_secret_key_share_it_logs_cores_line() {
        let mut q = Arc::into_inner(quorum(3)).unwrap();
        q.sk_share = None;
        let fx = fixture_with(Arc::new(q), 0, Arc::new(FakeOperatorSigner::default()));
        let (share, lines) = logged(|| fx.rt.create_sig_share(T, ID, MSG, Some(QH.into())));
        assert!(matches!(share, Ok(Some(None))));
        assert_eq!(
            share_lines(lines),
            [format!(
                "DEBUG llmq-sigs message=CSigSharesManager::CreateSigShare -- we don't have our skShare for quorum {}",
                hex(&QH)
            )]
        );
    }

    #[test]
    fn a_single_member_share_names_its_function() {
        let key = BlsSkValue::from_canonical(&scalar(9), BlsSchemeKind::Basic);
        let op = Arc::new(FakeOperatorSigner {
            key,
        });
        let fx = fixture_with(single(None), 0, op);
        let (share, lines) = logged(|| fx.rt.create_sig_share(T, ID, MSG, Some(QH.into())));
        assert!(matches!(share, Ok(Some(Some(_)))));
        let lines = share_lines(lines);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].starts_with(&format!(
                "DEBUG llmq-sigs message=CSigSharesManager::CreateSigShareForSingleMember -- created sigShare. signHash={}, ",
                our_sign_hash()
            )),
            "{lines:?}"
        );
    }

    #[test]
    fn a_failed_signature_logs_unconditionally() {
        let fx = fixture_with(single(None), 0, Arc::new(InvalidSig));
        let (share, lines) = logged(|| fx.rt.create_sig_share(T, ID, MSG, Some(QH.into())));
        assert!(matches!(share, Ok(Some(None))));
        let lines = share_lines(lines);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(
            untimed(&lines[0]),
            format!(
                "INFO {MODULE} message=CSigSharesManager::CreateSigShareForSingleMember -- failed to sign sigShare. signHash={}, id={}, msgHash={}, time=",
                our_sign_hash(),
                hex(&ID),
                hex(&MSG)
            )
        );

        let q = quorum(3);
        let made = Err(ShareRefusal::InvalidSig {
            sign_hash: [0xcd; 32],
        });
        let ((), lines) = logged(|| log_own_share(&*q, &ID, &MSG, &made, 7));
        assert_eq!(
            lines,
            [format!(
                "INFO {MODULE} message=CSigSharesManager::CreateSigShare -- failed to sign sigShare. signHash={}, id={}, msgHash={}, time=7",
                "cd".repeat(32),
                hex(&ID),
                hex(&MSG)
            )]
        );
    }

    #[test]
    fn refusing_while_signing_is_disallowed_logs_under_llmq_and_a_non_member_logs_nothing() {
        let q = quorum(3);
        let ((), lines) =
            logged(|| log_own_share(&*q, &ID, &MSG, &Err(ShareRefusal::SigningNotAllowed), 0));
        assert_eq!(
            lines,
            [
                "DEBUG llmq message=CreateSigShare -- refusing quorum signature while snapshot background validation is incomplete"
            ]
        );
        for silent in [ShareRefusal::NotValidMember, ShareRefusal::NoMemberIndex] {
            let ((), lines) = logged(|| log_own_share(&*q, &ID, &MSG, &Err(silent), 0));
            assert!(lines.is_empty(), "{silent:?}: {lines:?}");
        }
    }
}

mod workers;
