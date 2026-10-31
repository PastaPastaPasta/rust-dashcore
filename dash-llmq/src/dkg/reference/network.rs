//! An in-process quorum through the justification phase (LQ4 §3.1, LQ4a-1 C-9, LQ4a-2): every
//! member runs a [`DkgSession`] over its own store, debug status and randomness; each message
//! goes to its sender's queue as [`Sender::Own`] and to every other member's as
//! [`Sender::Peer`], and each queue is popped in batches of at most 8, as Core's handler pops
//! the current phase's queue.
//!
//! The mesh is full: every member receives every message directly, where Core relays through
//! `RelayMembers` and fetches on inv. Tests reach the queues to drop, duplicate or reorder.

use std::collections::{BTreeSet, VecDeque};

use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;

use super::{MemStore, RecordingDebug, TestMember, TestOperatorKey, quorum};
use crate::dkg::bls::{BlsCtx, BlsPkValue, BlsSkValue, IesSeed, encrypt_shares};
use crate::dkg::conn::ConnSnapshot;
use crate::dkg::rng::{CoreRng, DkgRng, KeyDraw, SeededRng};
use crate::dkg::session::{
    BatchOutcome, DkgEnv, DkgEvent, DkgInv, DkgMsgKind, DkgSession, Misbehaviour, OperatorKey,
    QuorumBase, RelayTopology, Sender, SerialRunner, SessionInit,
};
use crate::dkg::sim::SimErrorRates;
use crate::dkg::wire::{Complaint, Contribution, Justification, LlmqType, QuorumHash};

/// A queued payload and who sent it.
pub type Queued = (Sender<usize>, Vec<u8>);

/// One member: its session and everything its environment holds.
pub struct Node {
    /// The session.
    pub session: DkgSession<TestMember>,
    /// dkgdb.
    pub store: MemStore,
    /// The debug status.
    pub debug: RecordingDebug,
    /// The simulated error rates.
    pub sim: SimErrorRates,
    /// The session's randomness.
    pub rng: CoreRng<SeededRng>,
    /// The current phase's pending messages, as Core's `CDKGPendingMessages` holds them.
    pub queue: VecDeque<Queued>,
    /// Every inv the session relayed.
    pub relayed: Vec<DkgInv>,
    /// Every score the session gave.
    pub misbehaving: Vec<Misbehaviour<usize>>,
    /// Every line the session logged.
    pub events: Vec<DkgEvent>,
}

impl Node {
    /// Appends the lines the session logged since the last call to `events`.
    fn collect_events(&mut self) {
        self.events.extend(self.session.take_events());
    }
}

/// A topology with no relay members: the mesh delivers directly.
struct NoRelay;

impl RelayTopology for NoRelay {
    fn relay_members(&mut self, _: LlmqType, _: &QuorumHash, _: &ProTxHash) -> BTreeSet<ProTxHash> {
        BTreeSet::new()
    }
}

/// `n` members of one quorum, each a session.
pub struct Network {
    /// The quorum's parameters.
    pub params: LlmqParams,
    /// The members, in the quorum's order.
    pub members: Vec<TestMember>,
    /// Their operator keys.
    pub keys: Vec<TestOperatorKey>,
    /// Their sessions and environments.
    pub nodes: Vec<Node>,
    /// The process-wide BLS scheme.
    pub bls: BlsCtx,
    /// The kind of message the queues hold: the current phase's.
    pub phase: DkgMsgKind,
}

impl Network {
    /// `n` fresh members of a quorum under `params` (`n` within `minSize..=size`), each with a
    /// session and randomness from `seed`.
    ///
    /// # Panics
    ///
    /// When `n` is below `minSize`.
    pub fn new(params: &LlmqParams, n: usize, seed: u64) -> Self {
        let (members, keys) = quorum(n, seed);
        let base = QuorumBase {
            hash: QuorumHash::from([0x42; 32]),
            height: 48,
        };
        let nodes = (0..n)
            .map(|i| {
                let mut debug = RecordingDebug::default();
                let init = SessionInit {
                    params,
                    base,
                    quorum_index: 0,
                    members: members.clone().into(),
                    active_pro_tx: Some(members[i].pro_tx_hash),
                    rotation_enabled: false,
                    v19_after_base: true,
                };
                let mut session =
                    DkgSession::init(init, &mut NoRelay, &mut debug).expect("enough members");
                Node {
                    events: session.take_events(),
                    session,
                    store: MemStore::default(),
                    debug,
                    sim: SimErrorRates::default(),
                    rng: CoreRng(SeededRng::new(seed ^ ((i as u64 + 1) << 32))),
                    queue: VecDeque::new(),
                    relayed: Vec::new(),
                    misbehaving: Vec::new(),
                }
            })
            .collect();
        Self {
            params: *params,
            members,
            keys,
            nodes,
            bls: BlsCtx::BASIC,
            phase: DkgMsgKind::Contribution,
        }
    }

    fn env<'a>(
        bls: BlsCtx,
        key: &'a TestOperatorKey,
        node: &'a mut Node,
    ) -> (&'a mut DkgSession<TestMember>, DkgEnv<'a>) {
        let Node {
            session,
            store,
            debug,
            sim,
            rng,
            ..
        } = node;
        let env = DkgEnv {
            bls,
            rng,
            key,
            store,
            debug,
            sim,
            runner: &SerialRunner,
        };
        (session, env)
    }

    /// `f` on member `i`'s session in its environment, then its lines collected.
    fn step<R>(
        &mut self,
        i: usize,
        f: impl FnOnce(&mut DkgSession<TestMember>, &mut DkgEnv<'_>) -> R,
    ) -> R {
        let (session, mut env) = Self::env(self.bls, &self.keys[i], &mut self.nodes[i]);
        let out = f(session, &mut env);
        self.nodes[i].collect_events();
        out
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
            self.params.llmq_type,
            &node.session.base().hash,
            &self.members[i].pro_tx_hash,
            honest.vvec(),
            &blobs,
            flag,
        );
        let sig = self.keys[i].sign(qc.sign_hash(), node.session.signs_legacy());
        let qc = qc.with_sig(sig, flag);
        self.send(i, &qc.payload(flag));
        qc
    }

    /// `payload` from member `from`: its own queue as `Own`, every other as `Peer(from)`.
    pub fn send(&mut self, from: usize, payload: &[u8]) {
        for (j, node) in self.nodes.iter_mut().enumerate() {
            let sender = if j == from {
                Sender::Own
            } else {
                Sender::Peer(from)
            };
            node.queue.push_back((sender, payload.to_vec()));
        }
    }

    /// Member `i` pops at most `max` messages and processes them as one batch. `false` when
    /// its queue was empty.
    pub fn pop(&mut self, i: usize, max: usize) -> bool {
        let node = &mut self.nodes[i];
        let take = node.queue.len().min(max);
        if take == 0 {
            return false;
        }
        let popped: Vec<Queued> = node.queue.drain(..take).collect();
        let batch: Vec<(Sender<usize>, &[u8])> = popped.iter().map(|(s, p)| (*s, &p[..])).collect();
        let (session, mut env) = Self::env(self.bls, &self.keys[i], &mut self.nodes[i]);
        let BatchOutcome {
            misbehaving,
            relay,
            events,
        } = session.process_batch(self.phase, &batch, &mut env);
        let node = &mut self.nodes[i];
        node.misbehaving.extend(misbehaving.into_iter().map(|s| s.misbehaviour));
        node.relayed.extend(relay.into_iter().map(|r| r.inv));
        node.events.extend(events);
        true
    }

    /// Every member pops until its queue is empty, the `k`-th pop of member `i` taking at most
    /// `pop_size(i, k)` messages (clamped to `1..=8`).
    pub fn drain(&mut self, pop_size: impl Fn(usize, usize) -> usize) {
        for i in 0..self.nodes.len() {
            let mut k = 0;
            while self.pop(i, pop_size(i, k).clamp(1, 8)) {
                k += 1;
            }
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
