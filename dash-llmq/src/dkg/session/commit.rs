//! The commitment phase (LQ4 §2.8): our premature commitment, and each received one, checked
//! against the quorum verification vector the verified contributions give.

use dash_evo::llmq::OperatorKeyView;
use dash_evo::types::ProTxHash;
use dash_types::Numeric;

use super::batch::{DkgInv, DkgMsgKind};
use super::{DkgEnv, DkgSession, Me, MemberStatusBit, MissingVvec, SessionStatusBit, hex};
use crate::dkg::bls::{
    BlsPkValue, BlsSchemeKind, BlsSigValue, BlsSkValue, aggregate_secret_keys, build_pub_key_share,
    build_quorum_verification_vector, public_keys_equal, secret_to_public, sign, verify_insecure,
};
use crate::dkg::rng::{KeyDraw, RangeDraw};
use crate::dkg::sim::{SimError, should_simulate};
use crate::dkg::wire::{PrematureCommitment, vvec_hash};

/// Byte 5 of `sig`'s bytes under `scheme` +1, then `SetBytes` under `scheme`, as `COMMIT_LIE`'s
/// lie types 3 and 4 do (rc.5 `a/dkgsession.cpp:562-570`; `bls/bls.h:100-123`).
///
/// Under the basic scheme, the changed bytes give their element or the invalid signature. Under
/// the legacy scheme `SetBytes` keeps the object without a round trip
/// (`dashbls/src/elements.cpp:245-310`): changed bytes with the sign bit (`0x80`) set that do
/// not decode give the object the same bytes with the bit clear decode to, which is how that
/// object is written; any other bytes that do not decode give the invalid signature. A changed
/// first coordinate at or above the field modulus gives the invalid signature here; `SetBytes`
/// under the legacy scheme keeps an object for those bytes, which this does not model.
fn perturbed(sig: &BlsSigValue, scheme: BlsSchemeKind) -> BlsSigValue {
    let mut bytes = sig.to_bytes(scheme);
    bytes[5] = bytes[5].wrapping_add(1);
    BlsSigValue::from_canonical(&bytes, scheme)
        .or_else(|| {
            (scheme == BlsSchemeKind::Legacy && bytes[0] & 0x80 != 0).then(|| {
                bytes[0] &= 0x7f;
                BlsSigValue::from_canonical(&bytes, scheme)
            })?
        })
        .unwrap_or_else(BlsSigValue::invalid)
}

/// `CBLSWorker::BuildQuorumVerificationVector(vvecs)` over the verified contributions' vectors.
fn build_quorum_vvec(vvecs: &[Vec<BlsPkValue>], flag: BlsSchemeKind) -> Option<Vec<BlsPkValue>> {
    let vvecs: Vec<&[BlsPkValue]> = vvecs.iter().map(Vec::as_slice).collect();
    build_quorum_verification_vector(&vvecs, flag)
}

impl<E: OperatorKeyView> DkgSession<E> {
    /// `VerifyAndCommit`, which follows v24.0.0 (#8016; v24.0.0 `a/dkgsession.cpp:453-520`):
    /// for a member, the members with open complaints, then those whose received verification
    /// vectors share a coefficient, are marked bad, and then `SendCommitment` runs. `None` for a
    /// non-member and wherever `SendCommitment` sends nothing.
    pub fn verify_and_commit(&mut self, env: &mut DkgEnv<'_>) -> Option<PrematureCommitment> {
        let me = self.me?;
        self.mark_open_complaints(env.bls.scheme(), env.debug);
        self.send_commitment(me, env)
    }

    /// `SendCommitment` (rc.5 `a/dkgsession.cpp:477-587`), with the draws in Core's order (LQ4
    /// §2.9): `COMMIT_OMIT` once the valid members reach `minSize`; `COMMIT_LIE` once our share
    /// is built, then the lie type `GetRand<int>(5)` and, for lie type 0, a fresh key.
    ///
    /// `validMembers` has a bit per `size`, set for each member that is not bad. Nothing is sent
    /// below `minSize`, on `COMMIT_OMIT`, when the store has no vector for a valid member, when
    /// the vectors do not build the quorum verification vector, or when the shares do not sum
    /// to a valid secret key, an invalid share row included. Otherwise the quorum public key is
    /// the vector's first key and its hash the vector's; a lie type changes the commitment as
    /// Core's does: 0 the key to a fresh one's, 1 the first hash byte, 2 the first byte of the
    /// commitment hash both signatures sign, 3 and 4 byte 5 of `sig` or of `quorumSig`. `sig` is
    /// the operator's signature and `quorumSig` our share's, both under the session's signing
    /// scheme.
    ///
    /// The store is read first; the vector and our share then come from the session's cache.
    fn send_commitment(&mut self, me: Me, env: &mut DkgEnv<'_>) -> Option<PrematureCommitment> {
        const F: &str = "SendCommitment";
        let t = self.llmq_type;
        self.begin_record();
        let mark = self.events.len();
        self.log(F, "sending commitment");
        let valid: Vec<bool> = (0..usize::from(self.params.size))
            .map(|i| self.member_state.get(i).is_some_and(|m| !m.bad))
            .collect();
        if valid.iter().filter(|v| **v).count() < usize::from(self.params.min_size) {
            self.log(F, "not enough valid members. not sending commitment");
            return None;
        }
        if should_simulate(t, SimError::CommitOmit, env.sim, env.rng) {
            self.log(F, "omitting");
            return None;
        }

        let base = self.base.hash;
        let flag = env.bls.scheme();
        let verified = match env.store.verified_contributions(t, &base, &valid) {
            Ok(verified) => verified,
            Err(MissingVvec {
                who,
            }) => {
                self.log_missing_vvec(mark, &who);
                self.log(F, "failed to get valid contributions");
                return None;
            }
        };
        let key = &verified.member_indexes;
        let Some(quorum_vvec) =
            self.cache.quorum_vvec(key, || build_quorum_vvec(&verified.vvecs, flag))
        else {
            self.log(F, "failed to build quorum verification vector");
            return None;
        };
        let sk_share = self.cache.share_sum(key, || {
            let shares: Vec<&BlsSkValue> = verified.sk_contributions.iter().collect();
            aggregate_secret_keys(&shares, flag)
        });
        if !sk_share.is_valid() {
            self.log(F, "failed to build own secret share");
            return None;
        }
        self.log(F, format!("pubKeyShare={}", hex(&secret_to_public(&sk_share), flag)));

        let mut quorum_pk = quorum_vvec.first().cloned().unwrap_or_else(BlsPkValue::invalid);
        let mut quorum_vvec_hash = vvec_hash(&quorum_vvec, flag);
        let lie = should_simulate(t, SimError::CommitLie, env.sim, env.rng)
            .then(|| env.rng.rand_range(5, RangeDraw::CommitLieType));
        if let Some(lie) = lie {
            self.log(F, format!("lying on commitment. lieType={lie}"));
        }
        match lie {
            Some(0) => quorum_pk = secret_to_public(&env.rng.new_key(KeyDraw::CommitLieKey)),
            Some(1) => quorum_vvec_hash[0] = quorum_vvec_hash[0].wrapping_add(1),
            _ => {}
        }
        let qc = PrematureCommitment::unsigned(
            t,
            &base,
            &me.pro_tx,
            &valid,
            &quorum_pk,
            &quorum_vvec_hash,
            flag,
        );
        let mut hash = *qc.sign_hash();
        if lie == Some(2) {
            hash[0] = hash[0].wrapping_add(1);
        }
        let legacy = self.signs_legacy();
        let mut sig = env.key.sign(&hash, legacy);
        let mut quorum_sig = sign(&sk_share, &hash, legacy);
        let scheme = BlsSchemeKind::from_legacy(legacy);
        match lie {
            Some(3) => sig = perturbed(&sig, scheme),
            Some(4) => quorum_sig = perturbed(&quorum_sig, scheme),
            _ => {}
        }
        self.log(F, "built premature commitment");
        env.debug.update_local_session_status(
            t,
            self.quorum_index,
            SessionStatusBit::SentPrematureCommitment,
        );
        Some(qc.with_sig(sig, flag).with_quorum_sig(quorum_sig, flag))
    }

    /// `GetVerifiedContributions`' line for the member `who` without a `qdkg_V` row (rc.5
    /// `l/dkgsessionmgr.cpp:166-168`), a `LogPrint` Core writes on its own, before the record of
    /// the call it is in: the lines logged since `mark` follow it.
    fn log_missing_vvec(&mut self, mark: usize, who: &ProTxHash) {
        let text = format!(
            "this node does not have vvec for llmq={} block={} protx={who}",
            self.llmq_type.to_base(),
            self.base.hash
        );
        self.log_line("GetVerifiedContributions", text);
        self.events[mark..].rotate_right(1);
    }

    /// `GetPrematureCommitment(hash)` (rc.5 `l/dkgsession.cpp:685-692`): a recorded premature
    /// commitment that `ReceiveMessage` accepted, which DK1 serves to a `getdata`. One it
    /// rejected stays recorded but is never served.
    pub fn get_premature_commitment(&self, hash: &[u8; 32]) -> Option<&PrematureCommitment> {
        if !self.valid_commitments.contains(hash) {
            return None;
        }
        self.premature_commitments.get(hash)
    }

    /// `ReceiveMessage(const CDKGPrematureCommitment&)` (rc.5 `l/dkgsession.cpp:562-637`), on
    /// members and observers alike, with no preamble: the inv to relay, if any.
    ///
    /// The message is recorded in the session's map and in its member's set, whose count the
    /// next PreVerify reads, whatever follows; an equal session hash leaves both as they are.
    /// Then `GetVerifiedContributions` over its `validMembers` (a members access, LQ4 §3.4.1
    /// row 4) gives the quorum verification vector:
    /// - when the store has no vector for a valid member, or the vectors do not build one, the
    ///   message is accepted unverified and relayed (:589-593);
    /// - otherwise the vector's first key must equal `quorumPublicKey`, its hash
    ///   `quorumVvecHash`, the sender's public key share must build, and `quorumSig` must verify
    ///   under that share over the sign hash. A failure returns nothing: no relay and no score
    ///   (:600-619).
    ///
    /// The store is read on every receive; the vector and the sender's public key share then
    /// come from the session's cache.
    ///
    /// An accepted message joins `validCommitments`, sets the sender's debug bit and is relayed.
    pub(crate) fn receive_premature_commitment(
        &mut self,
        qc: PrematureCommitment,
        env: &mut DkgEnv<'_>,
    ) -> Option<DkgInv> {
        const F: &str = "ReceiveMessage";
        self.begin_record();
        let mark = self.events.len();
        let (t, qi) = (self.llmq_type, self.quorum_index);
        let who = *qc.pro_tx_hash();
        let count = qc.count_valid_members();
        self.log(F, format!("received premature commitment from {who}. validMembers={count}"));
        // PreVerify admitted only members.
        let idx = self.member_index(&who)?;
        let hash = *qc.session_hash();
        self.premature_commitments.entry(hash).or_insert_with(|| qc.clone());
        self.member_state[idx].premature_commitments.insert(hash);

        let base = self.base.hash;
        let flag = env.bls.scheme();
        let built = match env.store.verified_contributions(t, &base, &qc.valid_members()) {
            Ok(verified) => {
                let key = verified.member_indexes;
                let vvec =
                    self.cache.quorum_vvec(&key, || build_quorum_vvec(&verified.vvecs, flag));
                vvec.map(|vvec| (key, vvec))
            }
            Err(MissingVvec {
                who: missing,
            }) => {
                self.log_missing_vvec(mark, &missing);
                None
            }
        };

        match built {
            None => {
                self.log(
                    F,
                    "failed to build quorum verification vector. skipping full verification",
                );
            }
            Some((key, quorum_vvec)) => {
                let pk = qc.quorum_public_key();
                if !quorum_vvec.first().is_some_and(|first| public_keys_equal(first, pk)) {
                    self.log(F, "calculated quorum public key does not match");
                    return None;
                }
                if vvec_hash(&quorum_vvec, flag) != *qc.quorum_vvec_hash() {
                    self.log(F, "calculated quorum vvec hash does not match");
                    return None;
                }
                let id = self.member_ids[idx];
                let share =
                    self.cache.pk_share(&key, &id, || build_pub_key_share(&quorum_vvec, &id));
                if !share.is_valid() {
                    self.log(F, "failed to calculate public key share");
                    return None;
                }
                if !verify_insecure(qc.quorum_sig(), &share, qc.sign_hash(), flag) {
                    self.log(F, "failed to verify quorumSig");
                    return None;
                }
            }
        }

        self.valid_commitments.insert(hash);
        env.debug.update_local_member_status(
            t,
            qi,
            idx,
            MemberStatusBit::ReceivedPrematureCommitment,
        );
        let received =
            self.member_state.iter().filter(|m| !m.premature_commitments.is_empty()).count();
        let n = self.members.len();
        self.log(F, format!("verified premature commitment. received={received}/{n}"));
        Some(DkgInv {
            kind: DkgMsgKind::PrematureCommitment,
            hash,
        })
    }
}

#[cfg(test)]
mod tests;
