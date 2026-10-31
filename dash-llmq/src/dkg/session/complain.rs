//! The complaint phase (LQ4 §2.6): our complaint, from the members that sent nothing, the bad
//! connections and the shares we could not verify; and each received complaint.

use dash_evo::llmq::OperatorKeyView;

use super::batch::DkgInv;
use super::preverify::MsgPhase;
use super::{DkgEnv, DkgFault, DkgSession, Me, SessionStatusBit, Stage};
use crate::dkg::conn::ConnSnapshot;
use crate::dkg::wire::Complaint;

impl<E: OperatorKeyView> DkgSession<E> {
    /// `VerifyAndComplain` (`a/dkgsession.cpp:185-224`) over DK1's connection snapshot `conn`:
    /// the pending verify, then each member that is not bad and sent no contribution is marked
    /// bad, then `VerifyConnectionAndMinProtoVersions`, then `SendComplaint`. `None` for a
    /// non-member or when there is nothing to complain about.
    ///
    /// # Errors
    ///
    /// [`DkgFault::PhaseOrder`] unless the session has contributed and not yet complained.
    pub fn verify_and_complain(
        &mut self,
        conn: &ConnSnapshot,
        env: &mut DkgEnv<'_>,
    ) -> Result<Option<Complaint>, DkgFault> {
        self.enter(Stage::Complained)?;
        let Some(me) = self.me else {
            return Ok(None);
        };
        self.verify_pending_contributions(env);
        self.begin_record();
        for idx in 0..self.members.len() {
            let state = &self.member_state[idx];
            if state.bad || !state.contributions.is_empty() {
                continue;
            }
            let who = *self.members[idx].pro_tx_hash();
            self.log("VerifyAndComplain", format!("{who} did not send any contribution"));
            self.mark_bad(idx, env.debug);
        }
        self.log("VerifyAndComplain", "verified contributions");
        self.verify_connections(conn, me);
        Ok(self.send_complaint(me, env))
    }

    /// `VerifyConnectionAndMinProtoVersions` (`a/dkgsession.cpp:226-290`): with PoSe on, each
    /// member but us with a [`ConnFault`] is a bad connection, one line per fault.
    fn verify_connections(&mut self, conn: &ConnSnapshot, me: Me) {
        const F: &str = "VerifyConnectionAndMinProtoVersions";
        if !conn.pose_enabled {
            return;
        }
        self.begin_record();
        for idx in 0..self.members.len() {
            let who = *self.members[idx].pro_tx_hash();
            if who == me.pro_tx {
                continue;
            }
            for fault in conn.faults(&who) {
                self.member_state[idx].bad_connection = true;
                self.log(F, fault.line(&who));
            }
        }
    }

    /// `SendComplaint` (`a/dkgsession.cpp:292-332`): a bad member or bad connection is voted
    /// bad, else one we complain about is complained about; `None` when neither exists.
    fn send_complaint(&mut self, me: Me, env: &mut DkgEnv<'_>) -> Option<Complaint> {
        self.begin_record();
        let n = self.members.len();
        let size = usize::try_from(self.params.size).unwrap_or(n).max(n);
        let (mut bad, mut complain_for) = (vec![false; size], vec![false; size]);
        for (i, m) in self.member_state.iter().enumerate() {
            if m.bad || m.bad_connection {
                bad[i] = true;
            } else if m.we_complain {
                complain_for[i] = true;
            }
        }
        let bad_count = bad.iter().filter(|b| **b).count();
        let complaint_count = complain_for.iter().filter(|b| **b).count();
        if bad_count == 0 && complaint_count == 0 {
            return None;
        }
        self.log(
            "SendComplaint",
            format!("sending complaint. badCount={bad_count}, complaintCount={complaint_count}"),
        );
        let (t, flag) = (self.params.llmq_type, env.bls.scheme());
        let qc = Complaint::unsigned(t, &self.base.hash, &me.pro_tx, &bad, &complain_for, flag);
        let sig = env.key.sign(qc.sign_hash(), self.signs_legacy());
        env.debug.update_local_session_status(
            t,
            self.quorum_index,
            SessionStatusBit::SentComplaint,
        );
        Some(qc.with_sig(sig, flag))
    }

    /// `GetComplaint(hash)` (`l/dkgsession.cpp:660-667`): a recorded complaint, which DK1
    /// serves to a `getdata`.
    pub fn complaint(&self, hash: &[u8; 32]) -> Option<&Complaint> {
        self.complaints.get(hash)
    }

    /// `ReceiveMessage(const CDKGComplaint&)` (`l/dkgsession.cpp:318-355`) with its preamble:
    /// each vote for a member makes the complainer one of its `badMemberVotes`, each complaint
    /// about a member one of its `complaintsFromOthers`.
    pub(crate) fn receive_complaint(
        &mut self,
        qc: Complaint,
        env: &mut DkgEnv<'_>,
    ) -> Option<DkgInv> {
        const F: &str = "ReceiveMessage";
        self.begin_record();
        let who = *qc.pro_tx_hash();
        let hash = *qc.session_hash();
        let (from, inv) = self.receive_preamble(MsgPhase::Complaint, &who, hash, env.debug)?;
        self.complaints.entry(hash).or_insert_with(|| qc.clone());
        let Some(from) = from else {
            return Some(inv);
        };
        let (t, qi) = (self.params.llmq_type, self.quorum_index);
        let me = self.me.map(|me| me.idx);
        let mut received = 0usize;
        for i in 0..self.members.len() {
            if qc.bad_member(i) {
                let m = *self.members[i].pro_tx_hash();
                self.log(F, format!("{who} voted for {m} to be bad"));
                self.member_state[i].bad_member_votes.insert(who);
                if me == Some(i) {
                    self.log(F, format!("{who} voted for us to be bad"));
                }
            }
            if qc.complain_for(i) {
                let state = &mut self.member_state[i];
                state.complaints_from_others.insert(who);
                state.someone_complain = true;
                env.debug.add_local_member_complaint(t, qi, i, from);
                if me == Some(i) {
                    self.log(F, format!("{who} complained about us"));
                }
            }
            if !self.member_state[i].complaints.is_empty() {
                received += 1;
            }
        }
        self.log(F, format!("received and relayed complaint. received={received}"));
        Some(inv)
    }
}

#[cfg(test)]
mod tests;
