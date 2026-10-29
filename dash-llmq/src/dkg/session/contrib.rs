//! The contribution phase (LQ4 §2.3, §2.9): our contribution, each received contribution, and
//! the batched check of the shares they gave us.

use dash_evo::llmq::OperatorKeyView;
use dash_evo::types::ProTxHash;

use super::batch::{DkgInv, DkgMsgKind};
use super::{DkgDebugSink, DkgEnv, DkgFault, DkgSession, Me, MemberStatusBit, SessionStatusBit};
use crate::dkg::bls::{
    BlsPkValue, BlsSkValue, can_encrypt_to, encrypt_shares, generate_contributions,
};
use crate::dkg::rng::KeyDraw;
use crate::dkg::sim::{SimError, should_simulate};
use crate::dkg::wire::Contribution;

/// `pendingContributionVerifications.size()` at which `ReceiveMessage` verifies the pending
/// shares itself (`l/dkgsession.cpp:259-261`).
const VERIFY_PENDING_AT: usize = 32;

impl<E: OperatorKeyView> DkgSession<E> {
    /// `Contribute` then `SendContributions` (`a/dkgsession.cpp:40-113`), with the draws in
    /// Core's order (LQ4 §2.9): `threshold` coefficients, `CONTRIBUTION_OMIT`, the IES seed,
    /// then per member `i ≠ me` `CONTRIBUTION_LIE` and its fresh key. `None` for a non-member,
    /// an omitted contribution, or a recipient whose operator key cannot be encrypted to. The
    /// true shares stay in the session whatever is sent.
    ///
    /// # Errors
    ///
    /// [`DkgFault::PhaseOrder`] on a second call: Core's handler contributes once.
    pub fn contribute(&mut self, env: &mut DkgEnv<'_>) -> Result<Option<Contribution>, DkgFault> {
        if self.contributed {
            return Err(DkgFault::PhaseOrder);
        }
        self.contributed = true;
        let Some(me) = self.me else {
            return Ok(None);
        };
        self.begin_record();
        self.log("Contribute", "generating contributions");
        let threshold = u32::try_from(self.params.threshold).unwrap_or(0);
        let coefficients: Vec<BlsSkValue> =
            (0..threshold).map(|j| env.rng.new_key(KeyDraw::Coefficient(j))).collect();
        let Some((vvec, shares)) = generate_contributions(&coefficients, &self.member_ids) else {
            self.log("Contribute", "GenerateContributions failed");
            return Ok(None);
        };
        self.log("Contribute", "generated contributions");
        self.own_vvec = vvec;
        self.own_shares = shares;
        Ok(self.send_contributions(me, env))
    }

    /// `SendContributions` (`a/dkgsession.cpp:61-113`).
    fn send_contributions(&mut self, me: Me, env: &mut DkgEnv<'_>) -> Option<Contribution> {
        const F: &str = "SendContributions";
        let t = self.params.llmq_type;
        self.begin_record();
        self.log(F, "sending contributions");
        if should_simulate(t, SimError::ContributionOmit, env.sim, env.rng) {
            self.log(F, "omitting");
            return None;
        }
        let seed = env.rng.ies_seed(self.members.len());
        let mut recipients: Vec<(BlsPkValue, BlsSkValue)> = Vec::with_capacity(self.members.len());
        for i in 0..self.members.len() {
            let who = *self.members[i].pro_tx_hash();
            let mut sk = self.own_shares[i].clone();
            if i != me.idx && should_simulate(t, SimError::ContributionLie, env.sim, env.rng) {
                self.log(F, format!("lying for {who}"));
                sk = env.rng.new_key(KeyDraw::ContributionLie(i as u32));
            }
            // Core encrypts here, member by member; the encryption draws nothing, and it fails
            // exactly when the recipient's key is invalid, so checking first stops the draws
            // where Core's do.
            let pk = self.members[i].operator_key();
            if !can_encrypt_to(&pk) {
                self.log(F, format!("failed to encrypt contribution for {who}"));
                return None;
            }
            recipients.push((pk, sk));
        }
        let flag = env.bls.scheme();
        let refs: Vec<(&BlsPkValue, &BlsSkValue)> =
            recipients.iter().map(|(pk, sk)| (pk, sk)).collect();
        let (blobs, encrypted) = encrypt_shares(&seed, &refs, flag);
        debug_assert!(encrypted, "every recipient's key passed can_encrypt_to");
        if !encrypted {
            return None;
        }
        self.log(F, "encrypted contributions");
        let qc =
            Contribution::unsigned(t, &self.base.hash, &me.pro_tx, &self.own_vvec, &blobs, flag);
        let sig = env.key.sign(qc.sign_hash(), self.signs_legacy());
        env.debug.update_local_session_status(
            t,
            self.quorum_index,
            SessionStatusBit::SentContributions,
        );
        Some(qc.with_sig(sig, flag))
    }

    /// `GetContribution(hash)` (`l/dkgsession.cpp:651-658`): a recorded contribution, which DK1
    /// serves to a `getdata`.
    pub fn contribution(&self, hash: &[u8; 32]) -> Option<&Contribution> {
        self.contributions.get(hash)
    }

    /// `ReceiveMessage(const CDKGContribution&)` (`l/dkgsession.cpp:213-265`) with its
    /// `ReceiveMessagePreamble` (:688-755): the inv to relay, if any.
    ///
    /// A member's first two distinct contributions are recorded and relayed; the second marks
    /// it bad. A contribution whose session hash is already recorded leaves the count as it is
    /// and is processed again, as Core's set does (m6 (d)). When this push makes
    /// [`VERIFY_PENDING_AT`] pending shares, the pending verify runs inside this call.
    pub(crate) fn receive_contribution(
        &mut self,
        qc: Contribution,
        env: &mut DkgEnv<'_>,
    ) -> Option<DkgInv> {
        const F: &str = "ReceiveMessage";
        self.begin_record();
        let mark = self.events.len();
        let (t, qi) = (self.params.llmq_type, self.quorum_index);
        let who = *qc.pro_tx_hash();
        let Some(idx) = self.member_index(&who) else {
            self.log(F, format!("message from non-member {who}"));
            return None;
        };
        self.log(F, format!("received contribution from {who}"));
        if self.member_state[idx].contributions.len() >= 2 {
            return None;
        }
        let hash = *qc.session_hash();
        self.member_state[idx].contributions.insert(hash);
        self.contributions.entry(hash).or_insert_with(|| qc.clone());
        env.debug.update_local_member_status(t, qi, idx, MemberStatusBit::ReceivedContribution);
        let inv = DkgInv {
            kind: DkgMsgKind::Contribution,
            hash,
        };
        if self.member_state[idx].contributions.len() > 1 {
            self.mark_bad(idx, env.debug);
            self.log(F, format!("{who} did send multiple contributions"));
            return Some(inv);
        }

        self.member_state[idx].received_vvec = Some(qc.vvec().to_vec());
        let received = self.member_state.iter().filter(|m| !m.contributions.is_empty()).count();
        let n = self.members.len();
        self.log(F, format!("received and relayed contribution. received={received}/{n}"));
        let Some(me) = self.me else {
            return Some(inv);
        };
        env.store.write_vvec(t, &self.base.hash, &who, qc.vvec());

        let share = env.key.decrypt_share(qc.blobs(), me.idx, env.bls.scheme());
        let complain = match &share {
            None => {
                self.log(F, format!("contribution from {who} could not be decrypted"));
                true
            }
            Some(_)
                if idx != me.idx && should_simulate(t, SimError::ComplainLie, env.sim, env.rng) =>
            {
                self.log(F, format!("lying/complaining for {who}"));
                true
            }
            Some(_) => false,
        };
        if complain {
            self.member_state[idx].we_complain = true;
            env.debug.update_local_member_status(t, qi, idx, MemberStatusBit::WeComplain);
            return Some(inv);
        }
        self.log(F, "decrypted our contribution share");
        let state = &mut self.member_state[idx];
        state.received_sk = share;
        state.encrypted = Some(qc.blobs().clone());
        self.pending.push(idx);
        if self.pending.len() >= VERIFY_PENDING_AT {
            // Core's logger batches this call's lines until it returns, so the verify's record,
            // written when the verify returns, comes first.
            let before = self.events.len();
            self.verify_pending_contributions(env);
            let nested = self.events.len() - before;
            self.events[mark..].rotate_right(nested);
        }
        Some(inv)
    }

    /// `VerifyPendingContributions` (`a/dkgsession.cpp:120-183`): each pending member that is
    /// neither bad nor complained about gets its `qdkg_E` row, then their shares are checked in
    /// one [`super::DkgBlsRunner`] call; a good share gets its `qdkg_S` row, a bad one a
    /// complaint. A verdict count that does not match leaves the pending list as it is.
    pub(crate) fn verify_pending_contributions(&mut self, env: &mut DkgEnv<'_>) {
        let outer = self.begin_record();
        self.verify_pending_in_record(env);
        self.record = outer;
    }

    fn verify_pending_in_record(&mut self, env: &mut DkgEnv<'_>) {
        const F: &str = "VerifyPendingContributions";
        let (t, qi) = (self.params.llmq_type, self.quorum_index);
        if self.pending.is_empty() {
            return;
        }
        let Some(me) = self.me else {
            return;
        };
        let mut indexes = Vec::with_capacity(self.pending.len());
        for &idx in &self.pending {
            let state = &self.member_state[idx];
            if state.bad || state.we_complain {
                continue;
            }
            indexes.push(idx);
            if let Some(blobs) = &state.encrypted {
                env.store.write_encrypted(
                    t,
                    &self.base.hash,
                    self.members[idx].pro_tx_hash(),
                    blobs,
                );
            }
        }
        if indexes.is_empty() {
            let n = self.pending.len();
            self.log(
                F,
                format!(
                    "all {n} pending contributions were from bad or complained-about members, \
                     nothing to verify"
                ),
            );
            self.pending.clear();
            return;
        }

        let invalid = BlsSkValue::invalid();
        let vvecs: Vec<&[BlsPkValue]> = indexes
            .iter()
            .map(|&idx| self.member_state[idx].received_vvec.as_deref().unwrap_or_default())
            .collect();
        let shares: Vec<&BlsSkValue> = indexes
            .iter()
            .map(|&idx| self.member_state[idx].received_sk.as_ref().unwrap_or(&invalid))
            .collect();
        let verdicts = env.runner.verify_contribution_shares(
            &self.member_ids[me.idx],
            &vvecs,
            &shares,
            env.bls.scheme(),
        );
        if verdicts.len() != indexes.len() {
            let (got, want) = (verdicts.len(), indexes.len());
            self.log(
                F,
                format!(
                    "VerifyContributionShares returned result of size {got} but size {want} was \
                     expected, something is wrong"
                ),
            );
            return;
        }

        for (idx, valid) in indexes.into_iter().zip(verdicts) {
            let who: ProTxHash = *self.members[idx].pro_tx_hash();
            if valid {
                if let Some(sk) = &self.member_state[idx].received_sk {
                    env.store.write_sk(t, &self.base.hash, &who, sk);
                }
            } else {
                self.log(F, format!("invalid contribution from {who}. will complain later"));
                self.member_state[idx].we_complain = true;
                env.debug.update_local_member_status(t, qi, idx, MemberStatusBit::WeComplain);
            }
        }
        let n = self.pending.len();
        self.log(F, format!("verified {n} pending contributions"));
        self.pending.clear();
    }

    /// `MarkBadMember` (`l/dkgsession.cpp:761-772`).
    pub(crate) fn mark_bad(&mut self, idx: usize, debug: &mut dyn DkgDebugSink) {
        if self.member_state[idx].bad {
            return;
        }
        let (t, qi) = (self.params.llmq_type, self.quorum_index);
        debug.update_local_member_status(t, qi, idx, MemberStatusBit::Bad);
        self.member_state[idx].bad = true;
    }
}

#[cfg(test)]
mod tests;
