//! Core's `CSigSharesManager` behind `NetSigning`: the share message entry points, `BanNode`,
//! the `AsyncSign` queue, the recovered-signature listener, one cleaning or dispatcher pass and
//! the dispatcher's sign and verify tasks.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::dkg::bls::{BlsSchemeKind, BlsSigValue};
use dash_llmq::dkg::wire::{BatchedSigShares, QuorumHash, SigSesAnn, SigShare, SigSharesInv};
use dash_llmq::sigenv::{
    Collected, OperatorSigner, PeerId, PendingSign, QuorumKey, QuorumLookup, RecSigReads,
    ShareBatch, ShareRecovery, ShareVerifier, SharesEffect, SharesEnv, SigSharesNet, SigningQuorum,
    VerifyError, select_for_signing_default,
};
use dash_llmq::signing::api::{
    RecoveredSigIntake, RecoveredSigListener, RecoveredSigResult, SigningFault,
};
use dash_llmq::sigshares::manager::SigSharesManager;
use dash_llmq::sigshares::outbound::ConcentratedSend;
use dash_llmq::sigshares::sign::{ShareRefusal, create_share};
use dash_llmq::sigshares::state::SessionInfo;
use dash_llmq::sigshares::ticks::{MAX_SHARES_PER_BATCH, MAX_UNVERIFIED_BATCHES, cleaning_tick};
use dash_params::{ChainSetup, LlmqParamsExt, LlmqType};
use dash_types::Numeric as _;
use dashd_chainstate::chain::reply::no_wait_marked;

use super::facts::{SharesFacts, SharesOut};
use crate::signing::SignQueue;

// Core: v24.0.0 src/llmq/signing_shares.h:462, :464
const MAX_MSGS_CNT_QSIGSESANN: usize = 100;
const MAX_MSGS_SIG_SHARES: usize = 32;

fn share_ban(peer: PeerId) -> SharesEffect {
    SharesEffect::Ban {
        peer,
        mark_shares_banned: true,
    }
}

/// Core's `BanNode` score.
// Core: v24.0.0 src/llmq/net_signing.cpp:311
const BAN_SCORE: i32 = 100;

/// Which `qsigsinv`-family message an inventory came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharesInvKind {
    /// `qsigsinv`.
    Announced,
    /// `qgetsigs`.
    Requested,
}

/// The runtime's handles into the rest of the node.
pub struct SharesHandles<L, I, N, X, F, S> {
    pub lookup: Arc<L>,
    /// SG1a's signing runtime.
    pub intake: Arc<I>,
    pub net: Arc<N>,
    pub out: Arc<X>,
    pub facts: Arc<F>,
    pub recovery: Arc<S>,
    pub operator: Arc<dyn OperatorSigner + Send + Sync>,
    /// The same `ChainSetup` the signing runtime holds.
    pub chain: Arc<ChainSetup>,
    /// The node's abort path, called with the fault's text at most once, possibly with S held:
    /// it must not block on or call back into the runtime.
    pub abort: Arc<Abort>,
}

/// The node's sig-share manager.
///
/// The shares mutex (S) is never held across `process_recovered_sig` or a send: every pure
/// call's effects are applied after S is released, and S is only ever taken before SG1a's
/// signing lock.
pub struct SigSharesRuntime<L: QuorumLookup, I, N, X, F, S> {
    shares: Mutex<SigSharesManager<L::Quorum>>,
    in_flight: Arc<AtomicUsize>,
    h: SharesHandles<L, I, N, X, F, S>,
}

type Abort = dyn Fn(&str) + Send + Sync;

fn fault(abort: &Abort, what: &str) {
    tracing::error!("sig-shares fault: {what}");
    abort(what);
}

/// Every fault but `Stopped` takes the abort path.
fn check<T>(abort: &Abort, r: Result<T, SigningFault>) -> Result<T, SigningFault> {
    if let Err(e) = &r
        && *e != SigningFault::Stopped
    {
        fault(abort, &e.to_string());
    }
    r
}

/// `RecSigReads` that takes the abort path on every fault but `Stopped`. The pure manager
/// reads a fault as "already recovered", so it never reaches the caller otherwise.
struct AbortOnFault<'a, I> {
    inner: &'a I,
    abort: &'a Abort,
}

impl<I: RecSigReads<Error = SigningFault>> RecSigReads for AbortOnFault<'_, I> {
    type Error = SigningFault;

    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, SigningFault> {
        check(self.abort, self.inner.has_for_signing(t, id, msg))
    }

    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, SigningFault> {
        check(self.abort, self.inner.has_for_session(sign_hash))
    }
}

/// Quorum reads made before S is taken, falling back to the lookup on a miss. A failed read
/// takes the abort path and reads as absent; a type the chain lacks is never looked up.
struct Prefetched<'a, L: QuorumLookup> {
    lookup: &'a L,
    chain: &'a ChainSetup,
    abort: &'a Abort,
    active: BTreeMap<QuorumKey, bool>,
    quorums: BTreeMap<QuorumKey, Option<Arc<L::Quorum>>>,
}

impl<L: QuorumLookup> Prefetched<'_, L> {
    fn guard<T>(&self, r: Result<T, L::Error>) -> Result<T, L::Error> {
        if let Err(e) = &r {
            fault(self.abort, &format!("quorum lookup fault: {e:?}"));
        }
        r
    }

    // Core: v24.0.0 src/llmq/signing_shares.cpp:253-256 ProcessMessageSigSesAnn
    fn unknown(&self, t: LlmqType) -> bool {
        self.chain.consensus.llmq(t).is_none()
    }

    // Core: v24.0.0 src/llmq/signing_shares.cpp:264-276 ProcessMessageSigSesAnn
    fn fetch(mut self, keys: impl IntoIterator<Item = (LlmqType, QuorumHash)>) -> Self {
        for (t, h) in keys {
            let key = (t.to_base(), h);
            if self.active.contains_key(&key) {
                continue;
            }
            let active = self.is_quorum_active(t, &h).unwrap_or(false);
            self.active.insert(key, active);
            if active {
                let quorum = self.get_quorum(t, &h).ok().flatten();
                self.quorums.insert(key, quorum);
            }
        }
        self
    }
}

impl<L: QuorumLookup> QuorumLookup for Prefetched<'_, L> {
    type Quorum = L::Quorum;
    type Error = L::Error;

    fn get_quorum(&self, t: LlmqType, h: &QuorumHash) -> Result<Option<Arc<L::Quorum>>, L::Error> {
        match self.quorums.get(&(t.to_base(), *h)) {
            Some(quorum) => Ok(quorum.clone()),
            None if self.unknown(t) => Ok(None),
            None => self.guard(self.lookup.get_quorum(t, h)),
        }
    }

    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, L::Error> {
        match self.active.get(&(t.to_base(), *h)) {
            Some(active) => Ok(*active),
            None if self.unknown(t) => Ok(false),
            None => self.guard(self.lookup.is_quorum_active(t, h)),
        }
    }

    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        sel: &[u8; 32],
        height: i32,
        offset: i32,
    ) -> Result<Option<Arc<L::Quorum>>, L::Error> {
        self.guard(self.lookup.select_quorum_for_signing(t, sel, height, offset))
    }
}

struct Operator<'a>(&'a (dyn OperatorSigner + Send + Sync));

impl OperatorSigner for Operator<'_> {
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue> {
        self.0.sign(hash, scheme)
    }
}

impl<L: QuorumLookup, I, N, X, F, S> SigSharesRuntime<L, I, N, X, F, S> {
    fn shares(&self) -> MutexGuard<'_, SigSharesManager<L::Quorum>> {
        self.shares.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Env<'a, L, I, N, S> = SharesEnv<'a, Prefetched<'a, L>, AbortOnFault<'a, I>, N, S>;

impl<L, I, N, X, F, S> SigSharesRuntime<L, I, N, X, F, S>
where
    L: QuorumLookup,
    I: RecoveredSigIntake + RecSigReads<Error = SigningFault>,
    N: SigSharesNet,
    X: SharesOut,
    F: SharesFacts,
    S: ShareRecovery,
{
    pub fn new(mut h: SharesHandles<L, I, N, X, F, S>) -> Self {
        let (abort, fired) = (h.abort, AtomicBool::new(false));
        h.abort = Arc::new(move |e: &str| {
            if !fired.swap(true, Ordering::AcqRel) {
                abort(e);
            }
        });
        Self {
            shares: Mutex::new(SigSharesManager::new()),
            in_flight: Arc::new(AtomicUsize::new(0)),
            h,
        }
    }

    /// Core's `unverified_batches`: share batches being verified off the dispatcher.
    pub fn in_flight(&self) -> &Arc<AtomicUsize> {
        &self.in_flight
    }

    pub fn chain(&self) -> &Arc<ChainSetup> {
        &self.h.chain
    }

    /// `quorum sign`'s share without submitting it: this node's share in the quorum
    /// `quorum_hash` names, or else the one selected at the tip; `Ok(None)` when there is no such
    /// quorum. It takes no lock and waits on no worker.
    // Core: v24.0.0 src/rpc/quorums.cpp:548-564
    pub fn create_sig_share(
        &self,
        t: LlmqType,
        id: [u8; 32],
        msg: [u8; 32],
        quorum_hash: Option<QuorumHash>,
    ) -> Result<Option<Option<SigShare>>, L::Error> {
        let quorum = match quorum_hash {
            None => select_for_signing_default(&*self.h.lookup, t, &id)?,
            Some(hash) => self.h.lookup.get_quorum(t, &hash)?,
        };
        Ok(quorum.map(|q| self.own_share(&q, id, msg)))
    }

    fn own_share(&self, q: &L::Quorum, id: [u8; 32], msg: [u8; 32]) -> Option<SigShare> {
        let (signer, bls) = (self.h.facts.signer(), self.h.facts.bls());
        let timer = Instant::now();
        let made = create_share(q, id, msg, &signer, &Operator(&*self.h.operator), bls);
        log_own_share(q, &id, &msg, &made, timer.elapsed().as_millis());
        made.ok()
    }

    fn recsigs(&self) -> AbortOnFault<'_, I> {
        AbortOnFault {
            inner: &*self.h.intake,
            abort: &*self.h.abort,
        }
    }

    fn prefetch(&self) -> Prefetched<'_, L> {
        Prefetched {
            lookup: &*self.h.lookup,
            chain: &self.h.chain,
            abort: &*self.h.abort,
            active: BTreeMap::new(),
            quorums: BTreeMap::new(),
        }
    }

    /// Runs `f` under S with the facts sampled now; the guard is released before this returns.
    fn locked<T>(
        &self,
        lookup: &Prefetched<'_, L>,
        f: impl FnOnce(&mut SigSharesManager<L::Quorum>, &Env<'_, L, I, N, S>) -> T,
    ) -> T {
        let recsigs = self.recsigs();
        let signer = self.h.facts.signer();
        let env = SharesEnv {
            lookup,
            recsigs: &recsigs,
            net: &*self.h.net,
            recovery: &*self.h.recovery,
            signer: &signer,
            sporks: self.h.facts.sporks(),
            now: self.h.facts.now(),
            bls: self.h.facts.bls(),
            chain: &self.h.chain,
        };
        f(&mut self.shares(), &env)
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:307-320 NetSigning::BanNode
    pub fn ban_node(&self, peer: PeerId, mark_shares_banned: bool) {
        if peer == -1 {
            return;
        }
        self.h.out.misbehaving(peer, BAN_SCORE);
        self.h.intake.remove_node(peer);
        if mark_shares_banned {
            self.shares().mark_banned(peer);
        }
    }

    /// Applies a pure call's effects in order; S must not be held.
    // Core: v24.0.0 src/llmq/net_signing.cpp:350, :453-459 ProcessPendingSigShares;
    // src/llmq/signing_shares.cpp:1201-1297 SendMessages
    fn apply(&self, effects: Vec<SharesEffect>) {
        for effect in effects {
            match effect {
                SharesEffect::Ban {
                    peer,
                    mark_shares_banned,
                } => self.ban_node(peer, mark_shares_banned),
                SharesEffect::Recovered(rs) => {
                    let _ = check(
                        &*self.h.abort,
                        self.h.intake.process_recovered_sig(Arc::new(rs), true),
                    );
                }
                SharesEffect::Send {
                    peer,
                    msgs,
                } => self.h.out.send(peer, msgs),
            }
        }
    }

    /// Whether `qsigshare` is read at all.
    // Core: v24.0.0 src/llmq/net_signing.cpp:66
    pub fn spork21_active(&self) -> bool {
        self.h.facts.sporks().spork21_active
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:97-114; src/llmq/signing_shares.cpp:251-300 ProcessMessageSigSesAnn
    pub fn on_ses_ann(&self, peer: PeerId, mut msgs: Vec<SigSesAnn>) {
        let cut = self.first_unrun(&msgs, MAX_MSGS_CNT_QSIGSESANN, |a| a.llmq_type());
        msgs.truncate(cut.unwrap_or(msgs.len()));
        let pre = self.prefetch().fetch(msgs.iter().map(|a| (a.llmq_type(), a.quorum_hash())));
        let mut effects = self.locked(&pre, |m, env| m.on_ses_ann(peer, msgs, env));
        if cut.is_some() && !effects.iter().any(|e| matches!(e, SharesEffect::Ban { .. })) {
            effects.push(share_ban(peer));
        }
        self.apply(effects);
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:115-132; src/llmq/signing_shares.cpp:302-349 ProcessMessageSigShares
    pub fn on_sig_shares_inv(&self, peer: PeerId, msgs: Vec<SigSharesInv>, kind: SharesInvKind) {
        self.apply(self.locked(&self.prefetch(), |m, env| match kind {
            SharesInvKind::Announced => m.on_inv(peer, msgs, env),
            SharesInvKind::Requested => m.on_get_sig_shares(peer, msgs, env),
        }));
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:133-148; src/llmq/signing_shares.cpp:350-450 ProcessMessageBatchedSigShares
    pub fn on_batched(&self, peer: PeerId, msgs: Vec<BatchedSigShares>) {
        self.apply(self.locked(&self.prefetch(), |m, env| m.on_batched(peer, msgs, env)));
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:66-95; src/llmq/signing_shares.cpp:451-509 ProcessMessageSigShare
    pub fn on_sig_share(&self, peer: PeerId, mut msgs: Vec<SigShare>) {
        let mut pre = self.prefetch();
        let mut cut = None;
        if self.spork21_active() {
            cut = self.first_unrun(&msgs, MAX_MSGS_SIG_SHARES, |m| m.llmq_type());
            msgs.truncate(cut.unwrap_or(msgs.len()));
            pre = pre.fetch(msgs.iter().map(|m| (m.llmq_type(), m.quorum_hash())));
        }
        let mut effects = self.locked(&pre, |m, env| m.on_sig_share(peer, msgs, env));
        if cut.is_some() {
            effects.push(share_ban(peer));
        }
        self.apply(effects);
    }

    /// The first message whose type this chain doesn't run; Core bans there and stops. An
    /// oversized message is left whole for the manager's size ban.
    // Core: v24.0.0 src/llmq/net_signing.cpp:85-92, :110-113; src/llmq/signing_shares.cpp:253-256
    fn first_unrun<T>(&self, msgs: &[T], cap: usize, t: impl Fn(&T) -> LlmqType) -> Option<usize> {
        if msgs.len() > cap {
            return None;
        }
        msgs.iter().position(|m| self.h.chain.consensus.llmq(t(m)).is_none())
    }

    /// A share message that failed to decode.
    // Core: v24.0.0 src/llmq/net_signing.cpp:72-76
    pub fn ban(&self, peer: PeerId) {
        self.ban_node(peer, true);
    }

    pub fn session_info(&self, peer: PeerId, recv_id: u32) -> Option<SessionInfo<L::Quorum>> {
        self.shares().session_info_by_recv_id(peer, recv_id)
    }

    /// One `WorkThreadCleaning` pass, without its sleep.
    // Core: v24.0.0 src/llmq/net_signing.cpp:322-334 WorkThreadCleaning; :300-305 RemoveBannedNodeStates
    pub fn maint_pass(&self) {
        let mut rng = self.h.facts.rng();
        let (effects, sends) = self.locked(&self.prefetch(), |m, env| {
            let effects = cleaning_tick(m, env, |p| env.net.is_banned(p), &mut rng, env.now.ms).0;
            (effects, m.take_concentrated())
        });
        sends.iter().for_each(log_concentrated);
        self.apply(effects);
    }

    /// One `WorkThreadDispatcher` pass without its sleep, its tasks run after it on this
    /// thread. Returns the quorum whose absence stopped collection.
    pub fn dispatch_pass(&self, verifier: &impl ShareVerifier) -> Option<QuorumKey> {
        let mut work = Vec::new();
        let missing = self.dispatcher_pass(|| false, |w| work.push(w));
        for w in work {
            match w {
                Work::Sign(w) => self.sign_task(w),
                Work::Verify(batch, _in_flight) => self.verify_task(&batch, verifier),
            }
        }
        missing
    }

    /// One `WorkThreadDispatcher` pass without its sleep: each pending sign goes to `spawn`
    /// until `stop`, then batches are collected under S and handed to `spawn` while fewer than
    /// four are in flight. Returns the quorum whose absence stopped collection.
    // Core: v24.0.0 src/llmq/net_signing.cpp:336-387 WorkThreadDispatcher;
    // src/llmq/signing_shares.cpp:592-600 CollectPendingSigSharesToVerify
    pub(super) fn dispatcher_pass(
        &self,
        stop: impl Fn() -> bool,
        mut spawn: impl FnMut(Work<L::Quorum>),
    ) -> Option<QuorumKey> {
        let signs = self.shares().dispatch_pending_signs();
        for w in signs {
            if stop() {
                break;
            }
            spawn(Work::Sign(w));
        }
        let pre = self.prefetch();
        let mut rng = self.h.facts.rng();
        while !stop() && self.in_flight.load(Ordering::Acquire) < MAX_UNVERIFIED_BATCHES {
            let collected = self.shares().collect_to_verify(MAX_SHARES_PER_BATCH, &pre, &mut rng);
            match collected {
                Ok(Collected::Ready {
                    batch,
                    more_work,
                }) => {
                    spawn(Work::Verify(batch, InFlight::new(&self.in_flight)));
                    if !more_work {
                        break;
                    }
                }
                Ok(Collected::MissingQuorum {
                    key: (t, h),
                }) => {
                    tracing::error!(
                        "CollectPendingSigSharesToVerify: ERROR! Unexpected missing quorum with llmqType={t}, quorumHash={h}"
                    );
                    return Some((t, h));
                }
                // A lookup `Err` has already taken the abort path.
                Ok(Collected::Empty) | Err(_) => break,
            }
        }
        None
    }

    /// A `sigsh-work` sign task: our share is created with no lock, processed under S, and its
    /// effects applied with S released.
    // Core: v24.0.0 src/llmq/signing_shares.cpp:1535-1551 SignAndProcessSingleShare;
    // src/llmq/net_signing.cpp:348-351 WorkThreadDispatcher
    pub(super) fn sign_task(&self, w: PendingSign<L::Quorum>) {
        if let Some(share) = self.own_share(&w.quorum, w.id, w.msg_hash) {
            self.apply(
                self.locked(&self.prefetch(), |m, env| m.process_own_share(share, w.quorum, env)),
            );
        }
    }

    /// A `sigsh-work` verify task: the batch is verified with no lock, processed under S, and
    /// its effects applied with S released.
    // Core: v24.0.0 src/llmq/net_signing.cpp:398-462 ProcessPendingSigShares
    pub(super) fn verify_task(&self, batch: &ShareBatch<L::Quorum>, verifier: &impl ShareVerifier) {
        let verdict = match verifier.verify_batch(batch, &self.recsigs(), self.h.facts.bls()) {
            Ok(verdict) => verdict,
            // A read fault has already taken the abort path, unless it was `Stopped`.
            Err(VerifyError::RecSig(_)) => return,
            Err(v) => return fault(&*self.h.abort, &format!("share verification: {v:?}")),
        };
        self.apply(
            self.locked(&self.prefetch(), |m, env| m.process_verified(batch, &verdict, env)),
        );
    }

    pub(super) fn panicked(&self, thread: &str) {
        fault(&*self.h.abort, &format!("{thread} panicked"));
    }
}

/// Core's `uint256::ToString`: the bytes reversed, in hex.
fn hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// Core's lines for our own share: created, failed to sign, and the early returns that log.
// Core: v24.0.2 src/llmq/signing_shares.cpp:1566-1650 CreateSigShareForSingleMember, CreateSigShare
fn log_own_share<Q: SigningQuorum>(
    q: &Q,
    id: &[u8; 32],
    msg: &[u8; 32],
    made: &Result<SigShare, ShareRefusal>,
    ms: u128,
) {
    let func = if q.params().is_single_member() {
        "CreateSigShareForSingleMember"
    } else {
        "CreateSigShare"
    };
    let quorum = hex(q.quorum_hash().as_bytes());
    match made {
        Ok(share) => tracing::debug!(
            target: "llmq-sigs",
            "CSigSharesManager::{func} -- created sigShare. signHash={}, id={}, msgHash={}, llmqType={}, quorum={quorum}, time={ms}",
            hex(share.sign_hash().as_bytes()),
            hex(id),
            hex(msg),
            q.llmq_type().to_base(),
        ),
        Err(ShareRefusal::InvalidSig {
            sign_hash,
        }) => tracing::info!(
            "CSigSharesManager::{func} -- failed to sign sigShare. signHash={}, id={}, msgHash={}, time={ms}",
            hex(sign_hash),
            hex(id),
            hex(msg),
        ),
        Err(ShareRefusal::NoSkShare) => tracing::debug!(
            target: "llmq-sigs",
            "CSigSharesManager::CreateSigShare -- we don't have our skShare for quorum {quorum}"
        ),
        Err(ShareRefusal::SigningNotAllowed) => tracing::debug!(
            target: "llmq",
            "CreateSigShare -- refusing quorum signature while snapshot background validation is incomplete"
        ),
        Err(ShareRefusal::NotValidMember | ShareRefusal::NoMemberIndex) => {}
    }
}

/// Core's lines for a concentrated send.
// Core: v24.0.2 src/llmq/signing_shares.cpp:1053-1075 CollectSigSharesToSendConcentrated
fn log_concentrated(send: &ConcentratedSend) {
    match send {
        ConcentratedSend::Skipped {
            sign_hash,
            count,
        } => tracing::debug!(
            target: "llmq-sigs",
            "CSigSharesManager::CollectSigSharesToSendConcentrated -- signHash={}, skipped {count} unreachable recovery members",
            hex(sign_hash),
        ),
        ConcentratedSend::SendingTo {
            sign_hash,
            pro_tx,
            attempt,
        } => tracing::debug!(
            target: "llmq-sigs",
            "CSigSharesManager::CollectSigSharesToSendConcentrated -- signHash={}, sending to {}, attempt={attempt}",
            hex(sign_hash),
            hex(pro_tx.as_bytes()),
        ),
    }
}

/// A batch counted in Core's `unverified_batches` until dropped.
// Core: v24.0.0 src/llmq/net_signing.cpp:373-378 UnverifiedBatchGuard
pub(super) struct InFlight(Arc<AtomicUsize>);

impl InFlight {
    fn new(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Self(Arc::clone(count))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A task the dispatcher hands to `sigsh-work`.
pub(super) enum Work<Q: SigningQuorum> {
    Sign(PendingSign<Q>),
    Verify(ShareBatch<Q>, InFlight),
}

impl<L: QuorumLookup, I, N, X, F, S> SignQueue<L::Quorum> for SigSharesRuntime<L, I, N, X, F, S>
where
    Self: Send + Sync,
    F: SharesFacts,
{
    // Core: v24.0.3 src/llmq/signing_shares.cpp:890-902 CSigSharesManager::AsyncSignIfMember;
    // :1684-1708 CSigSharesManager::ForceReAnnouncement
    fn force_reannouncement(&self, q: &Arc<L::Quorum>, t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) {
        debug_assert!(!no_wait_marked(), "sig-share queue call from a no-wait task");
        let sporks = self.h.facts.sporks();
        let now = self.h.facts.now();
        let sign_hash = dash_evo::locks::sign_hash(t, q.quorum_hash(), id, msg);
        let mut shares = self.shares();
        shares.restart_session_timeout(&sign_hash, now);
        shares.force_reannouncement(Arc::clone(q), t, *id, *msg, sporks);
    }

    // Core: v24.0.0 src/llmq/signing_shares.cpp:1553-1557 CSigSharesManager::AsyncSign
    fn async_sign(&self, quorum: Arc<L::Quorum>, id: [u8; 32], msg: [u8; 32]) {
        debug_assert!(!no_wait_marked(), "sig-share queue call from a no-wait task");
        self.shares().push_pending_sign(PendingSign {
            quorum,
            id,
            msg_hash: msg,
        });
    }
}

impl<L: QuorumLookup, I, N, X, F, S> RecoveredSigListener for SigSharesRuntime<L, I, N, X, F, S>
where
    Self: Send + Sync,
{
    // Core: v24.0.0 src/llmq/signing_shares.cpp:1678-1684 CSigSharesManager::HandleNewRecoveredSig
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        self.shares().on_recovered_sig(sig);
        RecoveredSigResult::Nothing
    }
}

#[cfg(test)]
mod tests;
