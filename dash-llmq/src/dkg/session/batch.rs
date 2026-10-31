//! `ProcessPendingMessageBatch` (`l/net_dkg.cpp:392-445`, LQ4 §2.3): one pop of at most 8
//! messages from one phase's queue, read, pre-verified, signature-checked as a batch, then
//! received in order.

use std::collections::BTreeSet;
use std::fmt;

use dash_evo::llmq::OperatorKeyView;

use dash_params::LlmqParams;

use super::preverify::DkgReject;
use super::{DkgEnv, DkgEvent, DkgSession};
use crate::dkg::bls::{
    BlsCtx, BlsPkValue, BlsSigValue, verify_insecure, verify_insecure_aggregated,
};
use crate::dkg::wire::{Complaint, Contribution, Justification, ReadFailure};

/// Who sent a message in a batch. Equality is Core's `NodeId` equality: one connection is one
/// sender, so the same proTxHash on two connections is two senders (LQ4 §3.3, m6 (a)).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Sender<P> {
    /// The session's own message, which DK1 enqueues as Core's `EnqueueOwn` does (`-1`).
    Own,
    /// A peer connection.
    Peer(P),
}

impl<P: fmt::Display> fmt::Display for Sender<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Own => f.write_str("-1"),
            Self::Peer(p) => p.fmt(f),
        }
    }
}

/// The message kinds `process_batch` takes. Each kind arrives with its `ReceiveMessage`
/// (LQ4a-1 D-1): premature commitments with LQ4b-1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DkgMsgKind {
    /// `qcontrib`.
    Contribution,
    /// `qcomplaint`.
    Complaint,
    /// `qjustify`.
    Justification,
}

/// An inventory to relay, `CInv{MSG_QUORUM_*, sessionHash}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DkgInv {
    /// The message's kind.
    pub kind: DkgMsgKind,
    /// Its session hash, `::SerializeHash` of the message as read.
    pub hash: [u8; 32],
}

/// An inv to relay and where Core relays it: `RelayInvToParticipants` runs right after the
/// `ReceiveMessage` that returned the inv, and writes a record of its own there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relayed {
    /// The inv.
    pub inv: DkgInv,
    /// The number of the outcome's `events` before the relay's record.
    pub at: usize,
}

/// A score Core gives a peer: 100 each (`PeerMisbehaving(nodeId, 100)`). Core drops its own
/// messages' scores (`PeerMisbehaving(-1, …)`), so there is no entry for [`Sender::Own`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Misbehaviour<P> {
    /// The worker's read failed: one per message (`l/net_dkg.cpp:405-408`).
    Decode {
        /// The sender.
        peer: P,
        /// The read's failure.
        reason: DkgReject,
    },
    /// `PreVerifyMessage` refused with `retBan`: one per message (:410-418).
    PreVerifyBan {
        /// The sender.
        peer: P,
        /// The check that failed.
        reason: DkgReject,
    },
    /// A message of the sender failed the batch's signature check: once per sender (:426-431).
    BadSignature {
        /// The sender.
        peer: P,
    },
}

/// A score and where Core gives it: `PeerMisbehaving` runs right after the line that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scored<P> {
    /// The score.
    pub misbehaviour: Misbehaviour<P>,
    /// The number of the outcome's `events` before it.
    pub at: usize,
}

/// What one batch produced.
#[derive(Clone, Debug)]
pub struct BatchOutcome<P> {
    /// The scores, in Core's order.
    pub misbehaving: Vec<Scored<P>>,
    /// The invs to relay (`RelayInvToParticipants`), in receive order.
    pub relay: Vec<Relayed>,
    /// The lines logged since the last [`DkgSession::take_events`], this batch's last.
    pub events: Vec<DkgEvent>,
}

impl<P> Default for BatchOutcome<P> {
    fn default() -> Self {
        Self {
            misbehaving: Vec::new(),
            relay: Vec::new(),
            events: Vec::new(),
        }
    }
}

/// The parts of a message the batch's signature check reads.
pub(crate) trait Signed {
    fn signer(&self) -> &dash_evo::types::ProTxHash;
    fn signature(&self) -> &BlsSigValue;
    fn signed_hash(&self) -> &[u8; 32];
}

/// A message kind's read, `PreVerifyMessage` and `ReceiveMessage`, for the batch.
trait BatchMsg: Signed + Sized {
    fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure>;
    fn pre_verify<E: OperatorKeyView>(
        &self,
        session: &mut DkgSession<E>,
        bls: BlsCtx,
    ) -> Result<(), DkgReject>;
    fn receive<E: OperatorKeyView>(
        self,
        session: &mut DkgSession<E>,
        env: &mut DkgEnv<'_>,
    ) -> Option<DkgInv>;
}

/// [`Signed`] and [`BatchMsg`] for `$msg`: `$pre_verify` over the session `$s`, the message `$m`
/// and the BLS context `$bls`, and the session method `$receive`.
macro_rules! batch_msg {
    ($msg:ty, |$s:ident, $m:ident, $bls:ident| $pre_verify:expr, $receive:ident) => {
        impl Signed for $msg {
            fn signer(&self) -> &dash_evo::types::ProTxHash {
                self.pro_tx_hash()
            }
            fn signature(&self) -> &BlsSigValue {
                self.sig()
            }
            fn signed_hash(&self) -> &[u8; 32] {
                self.sign_hash()
            }
        }

        impl BatchMsg for $msg {
            fn read(payload: &[u8], params: &LlmqParams, bls: BlsCtx) -> Result<Self, ReadFailure> {
                <$msg>::read(payload, params, bls)
            }
            fn pre_verify<E: OperatorKeyView>(
                &self,
                $s: &mut DkgSession<E>,
                $bls: BlsCtx,
            ) -> Result<(), DkgReject> {
                let $m = self;
                $pre_verify
            }
            fn receive<E: OperatorKeyView>(
                self,
                session: &mut DkgSession<E>,
                env: &mut DkgEnv<'_>,
            ) -> Option<DkgInv> {
                session.$receive(self, env)
            }
        }
    };
}

batch_msg!(Contribution, |s, qc, bls| s.pre_verify_contribution(qc, bls), receive_contribution);
batch_msg!(Complaint, |s, qc, _bls| s.pre_verify_complaint(qc), receive_complaint);
batch_msg!(Justification, |s, qj, _bls| s.pre_verify_justification(qj), receive_justification);

impl<E: OperatorKeyView> DkgSession<E> {
    /// `ProcessPendingMessageBatch` over exactly one pop: at most 8 messages of one kind, FIFO,
    /// own messages included. DK1 hands over exactly Core's pop (LQ4 §3.3).
    ///
    /// 1. Each payload is read; a failure scores its sender, per message.
    /// 2. `PreVerifyMessage` runs on every message read before any `ReceiveMessage`, so its
    ///    "≥ 2 already" counts are those from before the batch (m6 (c)); a refusal with
    ///    `retBan` scores its sender, per message.
    /// 3. `BatchVerifyMessageSigs`: each bad sender scores once, and every message of it in the
    ///    batch is skipped.
    /// 4. `ReceiveMessage` on the rest, in order; each returned inv is relayed.
    ///
    /// The outcome's events start with any the caller had not taken.
    pub fn process_batch<P: Copy + Ord + fmt::Display>(
        &mut self,
        kind: DkgMsgKind,
        batch: &[(Sender<P>, &[u8])],
        env: &mut DkgEnv<'_>,
    ) -> BatchOutcome<P> {
        match kind {
            DkgMsgKind::Contribution => self.process::<P, Contribution>(batch, env),
            DkgMsgKind::Complaint => self.process::<P, Complaint>(batch, env),
            DkgMsgKind::Justification => self.process::<P, Justification>(batch, env),
        }
    }

    fn process<P: Copy + Ord + fmt::Display, M: BatchMsg>(
        &mut self,
        batch: &[(Sender<P>, &[u8])],
        env: &mut DkgEnv<'_>,
    ) -> BatchOutcome<P> {
        let mut outcome = BatchOutcome::default();
        let mut preverified: Vec<(Sender<P>, M)> = Vec::new();
        for (sender, payload) in batch {
            let msg = match M::read(payload, &self.params, env.bls) {
                Ok(msg) => msg,
                Err(failure) => {
                    let reject = DkgReject::from(failure);
                    let what = match reject {
                        DkgReject::Structure => "message failed structure check",
                        _ => "failed to deserialize message",
                    };
                    self.log_line("DeserializeCheckedDKGMessage", format!("{what}, peer={sender}"));
                    if let Sender::Peer(peer) = sender {
                        self.score(
                            &mut outcome,
                            Misbehaviour::Decode {
                                peer: *peer,
                                reason: reject,
                            },
                        );
                    }
                    continue;
                }
            };
            if let Err(reject) = msg.pre_verify(self, env.bls) {
                if reject.bans() {
                    self.log_line(
                        "ProcessPendingMessageBatch",
                        format!("banning node due to failed preverification, peer={sender}"),
                    );
                    if let Sender::Peer(peer) = sender {
                        self.score(
                            &mut outcome,
                            Misbehaviour::PreVerifyBan {
                                peer: *peer,
                                reason: reject,
                            },
                        );
                    }
                }
                self.log_line(
                    "ProcessPendingMessageBatch",
                    format!("skipping message due to failed preverification, peer={sender}"),
                );
                continue;
            }
            preverified.push((*sender, msg));
        }
        if !preverified.is_empty() {
            let bad = self.batch_verify_message_sigs(&preverified, env.bls);
            for sender in &bad {
                self.log_line(
                    "ProcessPendingMessageBatch",
                    format!("failed to verify signature, peer={sender}"),
                );
                if let Sender::Peer(peer) = sender {
                    self.score(
                        &mut outcome,
                        Misbehaviour::BadSignature {
                            peer: *peer,
                        },
                    );
                }
            }
            for (sender, msg) in preverified {
                if bad.contains(&sender) {
                    continue;
                }
                if let Some(inv) = msg.receive(self, env) {
                    outcome.relay.push(Relayed {
                        inv,
                        at: self.events.len(),
                    });
                }
            }
        }
        outcome.events = self.take_events();
        outcome
    }

    /// Records `misbehaviour` after the lines logged so far.
    fn score<P>(&self, outcome: &mut BatchOutcome<P>, misbehaviour: Misbehaviour<P>) {
        outcome.misbehaving.push(Scored {
            misbehaviour,
            at: self.events.len(),
        });
    }

    /// `BatchVerifyMessageSigs` (`l/net_dkg.cpp:217-321`): the senders whose messages fail.
    ///
    /// An unknown member or an invalid signature marks its sender at once. A repeated sign hash
    /// forces per-message checks. Otherwise the aggregate is checked; when it fails and every
    /// message came from one sender, that sender is bad, and else each message is checked
    /// alone. Core keeps the bad senders in an `unordered_set`; this order is the senders'.
    fn batch_verify_message_sigs<P: Copy + Ord, M: Signed>(
        &self,
        messages: &[(Sender<P>, M)],
        bls: BlsCtx,
    ) -> BTreeSet<Sender<P>> {
        let mut bad = BTreeSet::new();
        let mut revert_to_single = false;
        let mut sigs: Vec<&BlsSigValue> = Vec::new();
        let mut pks: Vec<BlsPkValue> = Vec::new();
        let mut hashes: Vec<[u8; 32]> = Vec::new();
        let mut seen = BTreeSet::new();
        for (sender, msg) in messages {
            let Some(idx) = self.member_index(msg.signer()) else {
                bad.insert(*sender);
                continue;
            };
            if !msg.signature().is_valid() {
                bad.insert(*sender);
                continue;
            }
            sigs.push(msg.signature());
            if !seen.insert(*msg.signed_hash()) {
                revert_to_single = true;
                break;
            }
            pks.push(self.members[idx].operator_key());
            hashes.push(*msg.signed_hash());
        }
        if !revert_to_single {
            if pks.is_empty() {
                return bad;
            }
            let pk_refs: Vec<&BlsPkValue> = pks.iter().collect();
            if verify_insecure_aggregated(&sigs, &pk_refs, &hashes, bls.scheme()) {
                return bad;
            }
            let first = messages[0].0;
            if messages.iter().all(|(sender, _)| *sender == first) {
                bad.insert(first);
                return bad;
            }
        }
        for (sender, msg) in messages {
            if bad.contains(sender) {
                continue;
            }
            let valid = self.member_index(msg.signer()).is_some_and(|idx| {
                msg.signature().is_valid()
                    && verify_insecure(
                        msg.signature(),
                        &self.members[idx].operator_key(),
                        msg.signed_hash(),
                        bls.scheme(),
                    )
            });
            if !valid {
                bad.insert(*sender);
            }
        }
        bad
    }
}

#[cfg(test)]
mod tests;
