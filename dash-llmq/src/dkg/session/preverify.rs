//! `CDKGSession::PreVerifyMessage` for the four DKG messages (LQ4 §2.3): the cheap checks the
//! batch runs on every message before any signature or `ReceiveMessage`, in Core's order, with
//! Core's log text.

use std::collections::BTreeSet;

use dash_evo::llmq::OperatorKeyView;
use dash_evo::types::ProTxHash;

use super::DkgSession;
use crate::dkg::bls::{BlsCtx, verify_verification_vector};
use crate::dkg::wire::{
    Complaint, Contribution, Justification, PrematureCommitment, QuorumHash, ReadFailure,
};

/// Why the batch dropped a message before its signature check: the worker's read failed, or
/// `PreVerifyMessage` refused it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DkgReject {
    /// "failed to deserialize message" (`l/net_dkg.cpp:376-383`).
    Deserialize,
    /// "message failed structure check" (`l/net_dkg.cpp:384-387`).
    Structure,
    /// The message is for another quorum: dropped without a score.
    WrongQuorum,
    /// The sender is not a member.
    NotMember,
    /// The member's signature is invalid ("membersSig" for a premature commitment).
    InvalidSig,
    /// A contribution's blob count is not the member count.
    InvalidContributionsCount,
    /// A contribution's verification vector is not `threshold` long.
    InvalidVvecLength,
    /// `VerifyVerificationVector` refused a contribution's vector.
    InvalidVvec,
    /// A complaint's `badMembers` is not `size` bits.
    InvalidBadMembersSize,
    /// A complaint's `complainForMembers` is not `size` bits.
    InvalidComplainForSize,
    /// A justification reveals nothing.
    NoContributions,
    /// A justification names an index past the members.
    InvalidContributionIndex,
    /// A justification names an index twice.
    DuplicateContributionIndex,
    /// A justification reveals an invalid key.
    InvalidContribution,
    /// A premature commitment's `validMembers` is not `size` bits.
    InvalidValidMembersSize,
    /// A premature commitment counts fewer than `minSize` valid members.
    InvalidValidMembersCount,
    /// A premature commitment's `quorumSig` is invalid.
    InvalidQuorumSig,
    /// A premature commitment sets a bit past the members.
    ValidMemberBitPastMembers,
    /// The member already sent two of this kind: dropped without a score, before any BLS work.
    TooMany,
}

impl DkgReject {
    /// Whether Core scores the sender 100 (`retBan`, or a failed read).
    pub const fn bans(self) -> bool {
        !matches!(self, Self::WrongQuorum | Self::TooMany)
    }
}

impl From<ReadFailure> for DkgReject {
    fn from(failure: ReadFailure) -> Self {
        match failure {
            ReadFailure::Deserialize => Self::Deserialize,
            ReadFailure::Structure => Self::Structure,
        }
    }
}

/// The message kinds, for Core's log text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MsgPhase {
    Contribution,
    Complaint,
    Justification,
    #[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4b-1"))]
    PrematureCommitment,
}

impl MsgPhase {
    pub(super) const fn noun(self) -> &'static str {
        match self {
            Self::Contribution => "contribution",
            Self::Complaint => "complaint",
            Self::Justification => "justification",
            Self::PrematureCommitment => "commitment",
        }
    }

    const fn not_member(self) -> &'static str {
        match self {
            Self::Contribution => "contributor not a member of this quorum, rejecting contribution",
            Self::Complaint => "complainer not a member of this quorum, rejecting complaint",
            Self::Justification => "justifier not a member of this quorum, rejecting justification",
            Self::PrematureCommitment => {
                "committer not a member of this quorum, rejecting premature commitment"
            }
        }
    }

    const fn invalid_sig(self) -> &'static str {
        match self {
            Self::Contribution => "invalid contribution signature",
            Self::Complaint => "invalid complaint signature",
            Self::Justification => "invalid justification signature",
            Self::PrematureCommitment => "invalid membersSig",
        }
    }
}

impl<E: OperatorKeyView> DkgSession<E> {
    /// Logs `text` under `PreVerifyMessage` and gives back `reject`.
    fn reject(&mut self, reject: DkgReject, text: impl Into<String>) -> DkgReject {
        self.log("PreVerifyMessage", text);
        reject
    }

    /// The checks every message starts with: the quorum, then membership. The member's index.
    /// Each `PreVerifyMessage` opens its own `CDKGLogger` (`l/dkgsession.cpp:160`, :270, :359,
    /// :496), so this starts the record.
    fn pre_verify_sender(
        &mut self,
        phase: MsgPhase,
        quorum_hash: &QuorumHash,
        pro_tx: &ProTxHash,
    ) -> Result<usize, DkgReject> {
        self.begin_record();
        if *quorum_hash != self.base.hash {
            let text = format!("{} for wrong quorum, rejecting", phase.noun());
            return Err(self.reject(DkgReject::WrongQuorum, text));
        }
        self.member_index(pro_tx)
            .ok_or_else(|| self.reject(DkgReject::NotMember, phase.not_member()))
    }

    /// The "≥ 2 already" drop every kind ends with (`l/dkgsession.cpp:202`, :306, :410, :543).
    fn pre_verify_count(
        &mut self,
        phase: MsgPhase,
        idx: usize,
        count: usize,
    ) -> Result<(), DkgReject> {
        if count < 2 {
            return Ok(());
        }
        let who = self.members[idx].pro_tx_hash().to_string();
        let noun = phase.noun();
        Err(self.reject(
            DkgReject::TooMany,
            format!("dropping {noun} from {who} as we already got {count} {noun}s"),
        ))
    }

    /// `PreVerifyMessage(const CDKGContribution&)` (`l/dkgsession.cpp:158-211`), under the
    /// process-wide scheme `bls` for `VerifyVerificationVector`.
    pub(crate) fn pre_verify_contribution(
        &mut self,
        qc: &Contribution,
        bls: BlsCtx,
    ) -> Result<(), DkgReject> {
        let phase = MsgPhase::Contribution;
        let idx = self.pre_verify_sender(phase, qc.quorum_hash(), qc.pro_tx_hash())?;
        if !qc.sig().is_valid() {
            return Err(self.reject(DkgReject::InvalidSig, phase.invalid_sig()));
        }
        if qc.blobs().blobs.len() != self.members.len() {
            return Err(
                self.reject(DkgReject::InvalidContributionsCount, "invalid contributions count")
            );
        }
        if qc.vvec().len() != usize::try_from(self.params.threshold).unwrap_or(usize::MAX) {
            return Err(
                self.reject(DkgReject::InvalidVvecLength, "invalid verification vector length")
            );
        }
        if !verify_verification_vector(qc.vvec(), bls.scheme()) {
            return Err(self.reject(DkgReject::InvalidVvec, "invalid verification vector"));
        }
        let count = self.member_state[idx].contributions.len();
        self.pre_verify_count(phase, idx, count)
    }

    /// `PreVerifyMessage(const CDKGComplaint&)` (`l/dkgsession.cpp:268-316`).
    pub(crate) fn pre_verify_complaint(&mut self, qc: &Complaint) -> Result<(), DkgReject> {
        let phase = MsgPhase::Complaint;
        let idx = self.pre_verify_sender(phase, qc.quorum_hash(), qc.pro_tx_hash())?;
        if !qc.sig().is_valid() {
            return Err(self.reject(DkgReject::InvalidSig, phase.invalid_sig()));
        }
        let size = u64::try_from(self.params.size).unwrap_or(u64::MAX);
        if qc.bad_members_len() != size {
            return Err(
                self.reject(DkgReject::InvalidBadMembersSize, "invalid badMembers bitset size")
            );
        }
        if qc.complain_for_len() != size {
            return Err(self.reject(
                DkgReject::InvalidComplainForSize,
                "invalid complainForMembers bitset size",
            ));
        }
        let count = self.member_state[idx].complaints.len();
        self.pre_verify_count(phase, idx, count)
    }

    /// `PreVerifyMessage(const CDKGJustification&)` (`l/dkgsession.cpp:357-420`).
    pub(crate) fn pre_verify_justification(&mut self, qj: &Justification) -> Result<(), DkgReject> {
        let phase = MsgPhase::Justification;
        let idx = self.pre_verify_sender(phase, qj.quorum_hash(), qj.pro_tx_hash())?;
        if !qj.sig().is_valid() {
            return Err(self.reject(DkgReject::InvalidSig, phase.invalid_sig()));
        }
        if qj.keys().is_empty() {
            return Err(
                self.reject(DkgReject::NoContributions, "justification with no contributions")
            );
        }
        let mut seen = BTreeSet::new();
        for (index, key) in qj.keys() {
            if usize::try_from(*index).map_or(true, |i| i >= self.members.len()) {
                return Err(
                    self.reject(DkgReject::InvalidContributionIndex, "invalid contribution index")
                );
            }
            if !seen.insert(*index) {
                return Err(self.reject(
                    DkgReject::DuplicateContributionIndex,
                    "duplicate contribution index",
                ));
            }
            if !key.is_valid() {
                return Err(self.reject(DkgReject::InvalidContribution, "invalid contribution"));
            }
        }
        let count = self.member_state[idx].justifications.len();
        self.pre_verify_count(phase, idx, count)
    }

    /// `PreVerifyMessage(const CDKGPrematureCommitment&)` (`l/dkgsession.cpp:494-553`).
    #[cfg_attr(not(test), expect(dead_code, reason = "batched with its Receive in LQ4b-1"))]
    pub(crate) fn pre_verify_premature_commitment(
        &mut self,
        qc: &PrematureCommitment,
    ) -> Result<(), DkgReject> {
        let phase = MsgPhase::PrematureCommitment;
        let idx = self.pre_verify_sender(phase, qc.quorum_hash(), qc.pro_tx_hash())?;
        let size = u64::try_from(self.params.size).unwrap_or(u64::MAX);
        if qc.valid_members_len() != size {
            return Err(
                self.reject(DkgReject::InvalidValidMembersSize, "invalid validMembers bitset size")
            );
        }
        let count = qc.count_valid_members();
        if count < u64::try_from(self.params.min_size).unwrap_or(0) {
            return Err(self.reject(
                DkgReject::InvalidValidMembersCount,
                format!("invalid validMembers count. validMembersCount={count}"),
            ));
        }
        if !qc.sig().is_valid() {
            return Err(self.reject(DkgReject::InvalidSig, phase.invalid_sig()));
        }
        if !qc.quorum_sig().is_valid() {
            return Err(self.reject(DkgReject::InvalidQuorumSig, "invalid quorumSig"));
        }
        for i in self.members.len() as u64..size {
            if qc.valid_member(i) {
                return Err(self.reject(
                    DkgReject::ValidMemberBitPastMembers,
                    format!("invalid validMembers bitset. bit {i} should not be set"),
                ));
            }
        }
        let count = self.member_state[idx].premature_commitments.len();
        self.pre_verify_count(phase, idx, count)
    }
}

#[cfg(test)]
mod tests;
