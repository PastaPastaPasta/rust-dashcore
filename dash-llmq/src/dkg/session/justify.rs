//! The justification phase (LQ4 §2.6): our justification, revealing the shares the members
//! complaining about us should have got; each received justification; and the marking of
//! members whose complaints stay open, which starts `VerifyAndCommit`.

use std::collections::BTreeSet;

use dash_evo::llmq::OperatorKeyView;
use dash_evo::types::ProTxHash;

use super::batch::DkgInv;
use super::preverify::MsgPhase;
use super::{DkgDebugSink, DkgEnv, DkgFault, DkgSession, Me, SessionStatusBit, Stage};
use crate::dkg::bls::BlsSkValue;
use crate::dkg::rng::KeyDraw;
use crate::dkg::sim::{SimError, should_simulate};
use crate::dkg::wire::Justification;

impl<E: OperatorKeyView> DkgSession<E> {
    /// `VerifyAndJustify` (`a/dkgsession.cpp:334-374`): each member that is not bad is marked
    /// bad with `dkgBadVotesThreshold` votes against it or more than one complaint; otherwise a
    /// complaint about us from it is answered by `SendJustification`. `None` for a non-member
    /// or when nobody complains about us.
    ///
    /// # Errors
    ///
    /// [`DkgFault::PhaseOrder`] unless the session has complained and not yet justified.
    pub fn verify_and_justify(
        &mut self,
        env: &mut DkgEnv<'_>,
    ) -> Result<Option<Justification>, DkgFault> {
        const F: &str = "VerifyAndJustify";
        self.enter(Stage::Justified)?;
        let Some(me) = self.me else {
            return Ok(None);
        };
        self.begin_record();
        let threshold = usize::try_from(self.params.dkg_bad_votes_threshold).unwrap_or(usize::MAX);
        let mut justify_for = BTreeSet::new();
        for idx in 0..self.members.len() {
            let who = *self.members[idx].pro_tx_hash();
            let state = &self.member_state[idx];
            if state.bad {
                continue;
            }
            let votes = state.bad_member_votes.len();
            if votes >= threshold {
                self.log(F, format!("{who} marked as bad as {votes} other members voted for this"));
                self.mark_bad(idx, env.debug);
                continue;
            }
            let Some(first) = state.complaints.first() else {
                continue;
            };
            if state.complaints.len() != 1 {
                self.log(F, format!("{who} sent multiple complaints"));
                self.mark_bad(idx, env.debug);
                continue;
            }
            let qc = &self.complaints[first];
            if qc.complain_for(me.idx) {
                justify_for.insert(*qc.pro_tx_hash());
            }
        }
        if justify_for.is_empty() {
            return Ok(None);
        }
        Ok(self.send_justification(me, &justify_for, env))
    }

    /// `SendJustification` (`a/dkgsession.cpp:376-422`): our true share for each member in
    /// `for_members`, in member order, with the draws in Core's order: per member `i ≠ me`
    /// `JUSTIFY_LIE` and its fresh key, then `JUSTIFY_OMIT`.
    fn send_justification(
        &mut self,
        me: Me,
        for_members: &BTreeSet<ProTxHash>,
        env: &mut DkgEnv<'_>,
    ) -> Option<Justification> {
        const F: &str = "SendJustification";
        let t = self.params.llmq_type;
        self.begin_record();
        self.log(F, format!("sending justification for {} members", for_members.len()));
        let mut shares: Vec<(u32, BlsSkValue)> = Vec::with_capacity(for_members.len());
        for i in 0..self.members.len() {
            let who = *self.members[i].pro_tx_hash();
            if !for_members.contains(&who) {
                continue;
            }
            self.log(F, format!("justifying for {who}"));
            let index = u32::try_from(i).unwrap_or(u32::MAX);
            let mut sk = self.own_shares.get(i).cloned().unwrap_or_else(BlsSkValue::invalid);
            if i != me.idx && should_simulate(t, SimError::JustifyLie, env.sim, env.rng) {
                self.log(F, format!("lying for {who}"));
                sk = env.rng.new_key(KeyDraw::JustifyLie(index));
            }
            shares.push((index, sk));
        }
        if should_simulate(t, SimError::JustifyOmit, env.sim, env.rng) {
            self.log(F, "omitting");
            return None;
        }
        let flag = env.bls.scheme();
        let qj = Justification::unsigned(t, &self.base.hash, &me.pro_tx, &shares, flag);
        let sig = env.key.sign(qj.sign_hash(), self.signs_legacy());
        env.debug.update_local_session_status(
            t,
            self.quorum_index,
            SessionStatusBit::SentJustification,
        );
        Some(qj.with_sig(sig, flag))
    }

    /// `GetJustification(hash)` (`l/dkgsession.cpp:669-676`): a recorded justification, which
    /// DK1 serves to a `getdata`.
    pub fn justification(&self, hash: &[u8; 32]) -> Option<&Justification> {
        self.justifications.get(hash)
    }

    /// `ReceiveMessage(const CDKGJustification&)` (`l/dkgsession.cpp:422-492`) with its
    /// preamble. A justifier that is bad, or that reveals a share for a member that did not
    /// complain about it, gets no further look. Each revealed share is checked against the
    /// justifier's verification vector: a bad one marks the justifier bad, a good one closes
    /// that member's complaint and, revealed to us, becomes our verified share from it.
    pub(crate) fn receive_justification(
        &mut self,
        qj: Justification,
        env: &mut DkgEnv<'_>,
    ) -> Option<DkgInv> {
        const F: &str = "ReceiveMessage";
        self.begin_record();
        let who = *qj.pro_tx_hash();
        let hash = *qj.session_hash();
        let (idx, inv) = self.receive_preamble(MsgPhase::Justification, &who, hash, env.debug)?;
        self.justifications.entry(hash).or_insert_with(|| qj.clone());
        let Some(idx) = idx else {
            return Some(inv);
        };
        if self.member_state[idx].bad {
            return Some(inv);
        }
        // Each index is a member's: `PreVerifyMessage` checked them (`l/dkgsession.cpp:390-395`).
        let revealed: Vec<(usize, &BlsSkValue)> = qj
            .keys()
            .iter()
            .map(|(index, sk)| (usize::try_from(*index).unwrap_or(usize::MAX), sk))
            .collect();
        for &(i, _) in &revealed {
            let m = *self.members[i].pro_tx_hash();
            if !self.member_state[idx].complaints_from_others.contains(&m) {
                self.log(
                    F,
                    format!("got justification from {who} for {m} even though he didn't complain"),
                );
                self.mark_bad(idx, env.debug);
            }
        }
        if self.member_state[idx].bad {
            return Some(inv);
        }

        let shares: Vec<(&[u8; 32], &BlsSkValue)> =
            revealed.iter().map(|&(i, sk)| (&self.member_ids[i], sk)).collect();
        let vvec = self.member_state[idx].received_vvec.as_deref();
        let verdicts = env.runner.verify_justification_shares(vvec, &shares, env.bls.scheme());
        let t = self.params.llmq_type;
        for (k, &(i, sk)) in revealed.iter().enumerate() {
            let m = *self.members[i].pro_tx_hash();
            if !verdicts.get(k).copied().unwrap_or(false) {
                self.log(F, format!("  {who} did send an invalid justification for {m}"));
                self.mark_bad(idx, env.debug);
                continue;
            }
            self.log(F, format!("  {who} justified for {m}"));
            if self.me.is_some_and(|me| self.member_ids[i] == self.member_ids[me.idx]) {
                let state = &mut self.member_state[idx];
                state.received_sk = Some(sk.clone());
                state.we_complain = false;
                env.store.write_sk(t, &self.base.hash, &who, sk);
            }
            self.member_state[idx].complaints_from_others.remove(&m);
        }
        let received = self.member_state.iter().filter(|m| !m.justifications.is_empty()).count();
        let expected = self.member_state.iter().filter(|m| m.someone_complain).count();
        self.log(F, format!("verified justification: received={received}/{expected}"));
        Some(inv)
    }

    /// `VerifyAndCommit`'s marking (`a/dkgsession.cpp:424-466`): each member that is not bad
    /// but still has open complaints is marked bad, and the result is logged. The commitment
    /// that follows arrives with LQ4b-1.
    #[cfg_attr(not(test), expect(dead_code, reason = "VerifyAndCommit arrives with LQ4b-1"))]
    pub(crate) fn mark_open_complaints(&mut self, debug: &mut dyn DkgDebugSink) {
        const F: &str = "VerifyAndCommit";
        self.begin_record();
        let mut bad = Vec::new();
        let mut open = Vec::new();
        for idx in 0..self.members.len() {
            if self.member_state[idx].bad {
                bad.push(idx);
            } else if !self.member_state[idx].complaints_from_others.is_empty() {
                self.mark_bad(idx, debug);
                open.push(idx);
            }
        }
        if !bad.is_empty() || !open.is_empty() {
            self.log(F, "verification result:");
        }
        for (list, title) in [
            (bad, "  members previously determined as bad:"),
            (open, "  members with open complaints and now marked as bad:"),
        ] {
            if list.is_empty() {
                continue;
            }
            self.log(F, title);
            for idx in list {
                let who = *self.members[idx].pro_tx_hash();
                self.log(F, format!("    {who}"));
            }
        }
    }
}

#[cfg(test)]
mod tests;
