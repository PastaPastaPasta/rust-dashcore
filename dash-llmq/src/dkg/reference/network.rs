//! An in-process quorum through finalization (LQ4 §3.1, LQ4a-1 C-9, LQ4a-2, LQ4b-1): every
//! member runs a [`DkgSession`] over its own store, debug status, randomness and final
//! commitment verifier; each message goes to its sender's queue as [`Sender::Own`] and to every
//! other member's as [`Sender::Peer`], and each queue is popped in batches of at most 8, as
//! Core's handler pops the current phase's queue.
//!
//! The mesh is full: every member receives every message directly, where Core relays through
//! `RelayMembers` and fetches on inv. Tests reach the queues to drop, duplicate or reorder, and
//! each [`Node`] steps on its own, so a harness can run the members of a phase in parallel.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;
use rand_core::CryptoRng;

use super::{MemStore, RecordingDebug, ReferenceVerifier, TestMember, TestOperatorKey, quorum};
use crate::dkg::bls::{BlsCtx, BlsPkValue, BlsSkValue, IesSeed, encrypt_shares};
use crate::dkg::conn::ConnSnapshot;
use crate::dkg::rng::{CoreRng, DkgRng, KeyDraw, SeededRng};
use crate::dkg::session::{
    BatchOutcome, DkgEnv, DkgEvent, DkgInv, DkgMsgKind, DkgSession, Misbehaviour, OperatorKey,
    QuorumBase, RelayTopology, Sender, SerialRunner, SessionInit,
};
use crate::dkg::sim::SimErrorRates;
use crate::dkg::wire::{Complaint, Contribution, Justification, LlmqType, QuorumHash};

/// A queued payload and who sent it. Every queue shares the sent bytes.
pub type Queued = (Sender<usize>, Arc<[u8]>);

/// Where a [`Network`]'s quorum is: its base block, its quorum index and the flags its
/// sessions and verifiers read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetworkSetup {
    /// The base block.
    pub base: QuorumBase,
    /// The handler's quorum index.
    pub quorum_index: i32,
    /// `IsQuorumRotationEnabled(params, base)`; the verifiers see DIP0024 active iff it is set.
    pub rotation_enabled: bool,
    /// `DeploymentActiveAfter(base, V19)`.
    pub v19_after_base: bool,
}

impl NetworkSetup {
    /// A quorum at index 0 of a type that does not rotate, V19 active after its base. The base
    /// is two DKG intervals in, so `Verify`'s quorum index (the base height modulo the
    /// interval) is 0.
    pub fn new(params: &LlmqParams) -> Self {
        Self {
            rotation_enabled: false,
            ..Self::rotated(params, 0)
        }
    }

    /// A rotated quorum at index `quorum_index`, V19 active after its base: the base is
    /// `quorum_index` blocks after the second cycle's first block.
    pub fn rotated(params: &LlmqParams, quorum_index: i32) -> Self {
        Self {
            base: QuorumBase {
                hash: QuorumHash::from([0x42; 32]),
                height: 2 * i32::from(params.dkg_interval) + quorum_index,
            },
            quorum_index,
            rotation_enabled: true,
            v19_after_base: true,
        }
    }

    /// The reference verifier of a `t` quorum under `params` with `members` here: at the base's
    /// height, with V19 as set and DIP0024 active iff the quorum rotates.
    pub fn verifier(
        &self,
        t: LlmqType,
        params: &LlmqParams,
        members: &[TestMember],
    ) -> ReferenceVerifier {
        let height = u32::try_from(self.base.height).expect("a height");
        let mut verify = ReferenceVerifier::new(t, params, height, members);
        verify.v19_after_base = self.v19_after_base;
        verify.dip24 = self.rotation_enabled;
        verify
    }
}

/// One member: its session and everything its environment holds.
pub struct Node {
    /// The session.
    pub session: DkgSession<TestMember>,
    /// Its operator key.
    pub key: TestOperatorKey,
    /// dkgdb.
    pub store: MemStore,
    /// The debug status.
    pub debug: RecordingDebug,
    /// The simulated error rates.
    pub sim: SimErrorRates,
    /// The session's randomness.
    pub rng: Box<dyn DkgRng + Send>,
    /// The current phase's pending messages, as Core's `CDKGPendingMessages` holds them.
    pub queue: VecDeque<Queued>,
    /// Every inv the session relayed.
    pub relayed: Vec<DkgInv>,
    /// Every score the session gave.
    pub misbehaving: Vec<Misbehaviour<usize>>,
    /// Every line the session logged.
    pub events: Vec<DkgEvent>,
    /// The final commitments' `Verify`.
    pub verify: ReferenceVerifier,
    /// The batch weights' RNG its [`DkgEnv::weights`] borrows.
    pub weights: Box<dyn CryptoRng + Send>,
}

impl Node {
    /// Appends the lines the session logged since the last call to `events`.
    fn collect_events(&mut self) {
        self.events.extend(self.session.take_events());
    }

    /// `f` on the session in its environment under `bls`, then its lines collected.
    pub fn step<R>(
        &mut self,
        bls: BlsCtx,
        f: impl FnOnce(&mut DkgSession<TestMember>, &mut DkgEnv<'_>) -> R,
    ) -> R {
        let Self {
            session,
            key,
            store,
            debug,
            sim,
            rng,
            verify,
            weights,
            ..
        } = self;
        let mut env = DkgEnv {
            bls,
            rng: &mut **rng,
            weights: &mut **weights,
            key,
            store,
            debug,
            sim,
            runner: &SerialRunner,
            verify,
        };
        let out = f(session, &mut env);
        self.collect_events();
        out
    }

    /// Pops at most `max` messages of `phase` and processes them as one batch under `bls`.
    /// `false` when the queue was empty.
    pub fn pop(&mut self, bls: BlsCtx, phase: DkgMsgKind, max: usize) -> bool {
        let take = self.queue.len().min(max);
        if take == 0 {
            return false;
        }
        let popped: Vec<Queued> = self.queue.drain(..take).collect();
        let batch: Vec<(Sender<usize>, &[u8])> = popped.iter().map(|(s, p)| (*s, &p[..])).collect();
        let BatchOutcome {
            misbehaving,
            relay,
            events,
        } = self.step(bls, |session, env| session.process_batch(phase, &batch, env));
        self.misbehaving.extend(misbehaving.into_iter().map(|s| s.misbehaviour));
        self.relayed.extend(relay.into_iter().map(|r| r.inv));
        // `step` collected nothing: the batch's lines came back in its outcome.
        self.events.extend(events);
        true
    }

    /// Pops until the queue is empty, the `k`-th pop taking at most `pop_size(k)` messages
    /// (clamped to `1..=8`).
    pub fn drain(&mut self, bls: BlsCtx, phase: DkgMsgKind, pop_size: impl Fn(usize) -> usize) {
        let mut k = 0;
        while self.pop(bls, phase, pop_size(k).clamp(1, 8)) {
            k += 1;
        }
    }
}

/// A topology with no relay members: the mesh delivers directly.
struct NoRelay;

impl RelayTopology for NoRelay {
    fn relay_members(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash) -> BTreeSet<ProTxHash> {
        BTreeSet::new()
    }
}

/// Gives a member its session's randomness from the member's seed.
pub type RngBuilder = fn(u64) -> Box<dyn DkgRng + Send>;

/// The [`RngBuilder`] of every run that needs reproducible bytes: Core's draws over a
/// [`SeededRng`] of `seed`.
pub fn seeded_rng(seed: u64) -> Box<dyn DkgRng + Send> {
    Box::new(CoreRng(SeededRng::new(seed)))
}

/// The seed a [`Network`] built from `seed` hands member `i`'s [`RngBuilder`].
pub fn node_seed(seed: u64, i: usize) -> u64 {
    seed ^ ((i as u64 + 1) << 32)
}

/// `n` members of one quorum, each a session.
pub struct Network {
    /// The quorum's type.
    pub llmq_type: LlmqType,
    /// The quorum's parameters.
    pub params: LlmqParams,
    /// The members, in the quorum's order.
    pub members: Vec<TestMember>,
    /// Their sessions and environments.
    pub nodes: Vec<Node>,
    /// The process-wide BLS scheme.
    pub bls: BlsCtx,
    /// The kind of message the queues hold: the current phase's.
    pub phase: DkgMsgKind,
    /// Where the quorum is.
    pub setup: NetworkSetup,
}

impl Network {
    /// `n` fresh members of a `llmq_type` quorum under `params` (`n` within `minSize..=size`),
    /// each with a session at [`NetworkSetup::new`]'s base. `weights` gives each member its batch
    /// weights' RNG, an OS RNG, and `rng` its session's randomness from [`node_seed`].
    ///
    /// # Panics
    ///
    /// When `n` is below `minSize`.
    pub fn new(
        llmq_type: LlmqType,
        params: &LlmqParams,
        n: usize,
        seed: u64,
        weights: fn() -> Box<dyn CryptoRng + Send>,
        rng: RngBuilder,
    ) -> Self {
        Self::with_setup(llmq_type, params, n, seed, weights, rng, NetworkSetup::new(params))
    }

    /// [`Self::new`] at `setup`'s base, quorum index and flags, each member's verifier
    /// [`NetworkSetup::verifier`]'s.
    ///
    /// # Panics
    ///
    /// When `n` is below `minSize`.
    pub fn with_setup(
        llmq_type: LlmqType,
        params: &LlmqParams,
        n: usize,
        seed: u64,
        weights: fn() -> Box<dyn CryptoRng + Send>,
        rng: RngBuilder,
        setup: NetworkSetup,
    ) -> Self {
        let (members, keys) = quorum(n, seed);
        let verify = setup.verifier(llmq_type, params, &members);
        let store = MemStore::with_members(members.iter().map(|m| m.pro_tx_hash).collect());
        let nodes = keys
            .into_iter()
            .enumerate()
            .map(|(i, key)| {
                let mut debug = RecordingDebug::default();
                let init = SessionInit {
                    llmq_type,
                    params,
                    base: setup.base,
                    quorum_index: setup.quorum_index,
                    members: members.clone().into(),
                    active_pro_tx: Some(members[i].pro_tx_hash),
                    rotation_enabled: setup.rotation_enabled,
                    v19_after_base: setup.v19_after_base,
                };
                let mut session =
                    DkgSession::init(init, &mut NoRelay, &mut debug).expect("enough members");
                Node {
                    events: session.take_events(),
                    session,
                    key,
                    store: store.clone(),
                    debug,
                    sim: SimErrorRates::default(),
                    rng: rng(node_seed(seed, i)),
                    queue: VecDeque::new(),
                    relayed: Vec::new(),
                    misbehaving: Vec::new(),
                    verify: verify.clone(),
                    weights: weights(),
                }
            })
            .collect();
        Self {
            llmq_type,
            params: *params,
            members,
            nodes,
            bls: BlsCtx::BASIC,
            phase: DkgMsgKind::Contribution,
            setup,
        }
    }

    /// `f` on member `i`'s session in its environment, then its lines collected.
    fn step<R>(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut DkgSession<TestMember>, &mut DkgEnv<'_>) -> R,
    ) -> R {
        self.nodes[i].step(self.bls, f)
    }

    /// The process-wide scheme from now on, for the sessions and their verifiers: where the tip
    /// crosses V19 between two steps (LQ4 §4.3, m11).
    pub fn set_bls(&mut self, bls: BlsCtx) {
        self.bls = bls;
        for node in &mut self.nodes {
            node.verify.bls = bls;
        }
    }

    /// Member `i` contributes, and its contribution, if any, is returned unsent.
    fn contribute_unsent(&mut self, i: usize) -> Option<Contribution> {
        self.step(i, |session, env| session.contribute(env).expect("first call"))
    }

    /// Member `i` contributes, and its contribution, if any, is sent.
    pub fn contribute(&mut self, i: usize) -> Option<Contribution> {
        let qc = self.contribute_unsent(i);
        if let Some(qc) = &qc {
            self.send(i, &qc.payload(self.bls.scheme()));
        }
        qc
    }

    /// Member `i` contributes as a modified sender would: the shares for `lie_to` are fresh
    /// keys, each other share is true, and the message is properly signed. It is sent.
    pub fn contribute_modified(&mut self, i: usize, lie_to: &BTreeSet<usize>) -> Contribution {
        let honest = self.contribute_unsent(i).expect("a contribution");
        let node = &mut self.nodes[i];
        let shares: Vec<BlsSkValue> = node
            .session
            .own_shares()
            .iter()
            .enumerate()
            .map(|(j, sk)| {
                if lie_to.contains(&j) {
                    node.rng.new_key(KeyDraw::ContributionLie(j as u32))
                } else {
                    sk.clone()
                }
            })
            .collect();
        let seed: IesSeed = node.rng.ies_seed(shares.len());
        let pks: Vec<BlsPkValue> = self.members.iter().map(|m| m.operator_pk.clone()).collect();
        let recipients: Vec<(&BlsPkValue, &BlsSkValue)> = pks.iter().zip(&shares).collect();
        let flag = self.bls.scheme();
        let (blobs, encrypted) = encrypt_shares(&seed, &recipients, flag);
        assert!(encrypted, "every member's key is valid");
        let qc = Contribution::unsigned(
            self.llmq_type,
            &node.session.base().hash,
            &self.members[i].pro_tx_hash,
            honest.vvec(),
            &blobs,
            flag,
        );
        let sig = node.key.sign(qc.sign_hash(), node.session.signs_legacy());
        let qc = qc.with_sig(sig, flag);
        self.send(i, &qc.payload(flag));
        qc
    }

    /// `payload` from member `from`: its own queue as `Own`, every other as `Peer(from)`.
    pub fn send(&mut self, from: usize, payload: &[u8]) {
        let payload: Arc<[u8]> = payload.into();
        for (j, node) in self.nodes.iter_mut().enumerate() {
            let sender = if j == from {
                Sender::Own
            } else {
                Sender::Peer(from)
            };
            node.queue.push_back((sender, payload.clone()));
        }
    }

    /// Every member pops until its queue is empty, the `k`-th pop of member `i` taking at most
    /// `pop_size(i, k)` messages (clamped to `1..=8`).
    pub fn drain(&mut self, pop_size: impl Fn(usize, usize) -> usize) {
        let (bls, phase) = (self.bls, self.phase);
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.drain(bls, phase, |k| pop_size(i, k));
        }
    }

    /// The pending verify every member runs when the Complain phase starts
    /// (`VerifyAndComplain`'s first step, `a/dkgsession.cpp:185-195`), which ends phase 2's work.
    pub fn verify_pending(&mut self) {
        for i in 0..self.nodes.len() {
            self.step(i, |session, env| session.verify_pending_contributions(env));
        }
    }

    /// The next phase, whose messages the queues hold from now.
    ///
    /// # Panics
    ///
    /// When a queue still holds the current phase's messages.
    pub fn enter(&mut self, phase: DkgMsgKind) {
        assert!(self.nodes.iter().all(|n| n.queue.is_empty()), "the last phase is drained");
        self.phase = phase;
    }

    /// Member `i` runs `VerifyAndComplain` over its connection snapshot `conn`, and its
    /// complaint, if any, is sent.
    pub fn complain(&mut self, i: usize, conn: &ConnSnapshot) -> Option<Complaint> {
        let qc = self.step(i, |s, env| s.verify_and_complain(conn, env).expect("in phase order"));
        if let Some(qc) = &qc {
            self.send(i, &qc.payload(self.bls.scheme()));
        }
        qc
    }

    /// Member `i` runs `VerifyAndJustify`, and its justification, if any, is sent when `send`.
    /// An unsent justification is lost on the way, not omitted: the session has already set
    /// `sentJustification`, as `JUSTIFY_OMIT` does not.
    pub fn justify(&mut self, i: usize, send: bool) -> Option<Justification> {
        let qj = self.step(i, |s, env| s.verify_and_justify(env).expect("in phase order"));
        if let Some(qj) = qj.as_ref().filter(|_| send) {
            self.send(i, &qj.payload(self.bls.scheme()));
        }
        qj
    }

    /// The members member `i` complains about.
    pub fn we_complain(&self, i: usize) -> BTreeSet<usize> {
        (0..self.members.len()).filter(|&j| self.nodes[i].session.we_complain(j)).collect()
    }
}

#[cfg(test)]
mod tests;
