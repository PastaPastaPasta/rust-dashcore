//! The finalization phase (LQ4 §2.8): the final commitments a member builds from the premature
//! commitments it accepted, and a single-member quorum's own commitment.

use std::collections::BTreeMap;

use dash_evo::llmq::{LlmqCommitment, OperatorKeyView, commitment_version};

use super::{DkgEnv, DkgFault, DkgSession, hex};
use crate::dkg::bls::{
    BlsPkValue, BlsSchemeKind, BlsSigValue, aggregate_signatures_secure, public_keys_equal,
    recover_signature,
};
use crate::dkg::rng::KeyDraw;
use crate::dkg::wire::{FinalFields, PrematureCommitment, commitment_hash};

fn count(bits: &[bool]) -> usize {
    bits.iter().filter(|b| **b).count()
}

fn final_line(valid: usize, signers: usize, pk: &BlsPkValue, flag: BlsSchemeKind) -> String {
    format!(
        "final commitment: validMembers={valid}, signers={signers}, quorumPublicKey={}",
        hex(pk, flag)
    )
}

impl<E: OperatorKeyView> DkgSession<E> {
    /// `FinalizeCommitments()` (rc.5 `a/dkgsession.cpp:589-696`): one final commitment per group
    /// of the accepted premature commitments that agree on `validMembers`; empty for a
    /// non-member.
    ///
    /// The groups go in `validMembers` order (`std::map<std::vector<bool>, …>`: lexicographic,
    /// `false` before `true`), each holding its commitments in session-hash order. A group of
    /// fewer than `minSize` commitments is skipped before anything else. Its first commitment
    /// gives the final's `validMembers`, `quorumPublicKey` and `quorumVvecHash`, whether or not
    /// the others agree with it. A commitment whose key or vector hash differs from the first's
    /// is skipped with Core's line; each other one sets its member's signer bit and gives its
    /// `sig` with the member's operator key to `membersSig`'s secure aggregate, and its
    /// `quorumSig` with the member's id to the recovered `quorumSig`. A failed recovery skips the
    /// group with Core's line before `Verify`, as does a final that `env.verify` refuses
    /// (`CFinalCommitment::Verify(…, true)`, :680-683).
    ///
    /// The version is `GetVersion(rotation, V19 active after the base)`; `quorumIndex` is the
    /// session's for a rotated type, else 0.
    pub fn finalize_commitments(&mut self, env: &mut DkgEnv<'_>) -> Vec<LlmqCommitment> {
        const F: &str = "FinalizeCommitments";
        if !self.are_we_member() {
            return Vec::new();
        }
        self.begin_record();
        let mut groups: BTreeMap<Vec<bool>, Vec<&PrematureCommitment>> = BTreeMap::new();
        for (hash, qc) in &self.premature_commitments {
            if self.valid_commitments.contains(hash) {
                groups.entry(qc.valid_members()).or_default().push(qc);
            }
        }

        let flag = env.bls.scheme();
        let version = commitment_version(self.rotation_enabled, self.v19_after_base);
        // `int` to `int16_t`, as Core assigns it.
        let quorum_index = if self.rotation_enabled {
            self.quorum_index as i16
        } else {
            0
        };
        let mut lines = Vec::new();
        let mut finals = Vec::new();
        for (valid, cvec) in groups {
            if cvec.len() < usize::from(self.params.min_size) {
                continue;
            }
            let first = cvec[0];
            let pk = first.quorum_public_key();
            let vvec_hash = *first.quorum_vvec_hash();
            let mut signers = vec![false; usize::from(self.params.size)];
            let (mut sigs, mut pks, mut ids, mut shares) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            for qc in &cvec {
                if !public_keys_equal(qc.quorum_public_key(), pk)
                    || *qc.quorum_vvec_hash() != vvec_hash
                {
                    lines.push(
                        "quorumPublicKey or quorumVvecHash does not match, skipping".to_owned(),
                    );
                    continue;
                }
                // Only a member's premature commitment is recorded.
                let Some(idx) = self.member_index(qc.pro_tx_hash()) else {
                    continue;
                };
                signers[idx] = true;
                sigs.push(qc.sig());
                pks.push(self.members[idx].operator_key());
                ids.push(self.member_ids[idx]);
                shares.push(qc.quorum_sig());
            }
            // Core passes the commitment hash too, which `AggregateSecure` does not read.
            let pks: Vec<&BlsPkValue> = pks.iter().collect();
            let members_sig = aggregate_signatures_secure(&sigs, &pks, flag);
            let Some(quorum_sig) = recover_signature(&shares, &ids) else {
                lines.push("failed to recover quorum sig".to_owned());
                continue;
            };
            let qc = FinalFields {
                version,
                llmq_type: self.llmq_type,
                quorum_hash: *first.quorum_hash(),
                quorum_index,
                signers: &signers,
                valid_members: &valid,
                quorum_public_key: pk,
                quorum_vvec_hash: vvec_hash,
                quorum_sig,
                members_sig,
            }
            .commitment();
            if !env.verify.verify(&self.base.hash, &qc) {
                lines.push("failed to verify final commitment".to_owned());
                continue;
            }
            lines.push(final_line(count(&valid), count(&signers), pk, flag));
            finals.push(qc);
        }
        for line in lines {
            self.log(F, line);
        }
        finals
    }

    /// `FinalizeSingleCommitment()` (rc.5 `a/dkgsession.cpp:698-751`), which DK1 calls for a
    /// single-member type in place of the phases (`l/net_dkg.cpp:855-865`): `None` for a
    /// non-member.
    ///
    /// The commitment has the one signer and valid member, the operator public key as its
    /// quorum key and a null vector hash, at version `GetVersion(false, V19 active after the
    /// base)` and index 0. `membersSig` is the operator's signature over the commitment hash,
    /// and `quorumSig` is `membersSig`. Core first draws `sk1` (`MakeNewKey`, :713) and signs
    /// with it, but overwrites both the key's public key and its signature before they are read;
    /// the draw is made here, and no signature with it.
    ///
    /// # Errors
    ///
    /// [`DkgFault::SingleCommitmentUnverified`] when `env.verify` refuses the commitment, where
    /// Core asserts (:740-743); the record's lines are then never written.
    pub fn finalize_single_commitment(
        &mut self,
        env: &mut DkgEnv<'_>,
    ) -> Result<Option<LlmqCommitment>, DkgFault> {
        const F: &str = "FinalizeSingleCommitment";
        if !self.are_we_member() {
            return Ok(None);
        }
        self.begin_record();
        let _sk1 = env.rng.new_key(KeyDraw::SingleMemberSk1);
        let flag = env.bls.scheme();
        let pk = env.key.public_key();
        let vvec_hash = [0u8; 32];
        let valid = [true];
        let hash =
            commitment_hash(self.llmq_type, &self.base.hash, &valid, &pk, &vvec_hash, env.bls);
        let members_sig: BlsSigValue = env.key.sign(&hash, self.signs_legacy());
        let qc = FinalFields {
            version: commitment_version(false, self.v19_after_base),
            llmq_type: self.llmq_type,
            quorum_hash: self.base.hash,
            quorum_index: 0,
            signers: &valid,
            valid_members: &valid,
            quorum_public_key: &pk,
            quorum_vvec_hash: vvec_hash,
            quorum_sig: members_sig.clone(),
            members_sig,
        }
        .commitment();
        if !env.verify.verify(&self.base.hash, &qc) {
            return Err(DkgFault::SingleCommitmentUnverified);
        }
        self.log(F, final_line(1, 1, &pk, flag));
        Ok(Some(qc))
    }
}

#[cfg(test)]
mod tests;
