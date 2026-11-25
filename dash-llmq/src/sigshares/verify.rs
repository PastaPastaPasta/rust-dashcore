//! Share verification with source-wide rejection: per share, and as Core's weighted batch.
//!
//! Core keys batch-verifier messages by `(source, sign_hash, quorum_member)`;
//! duplicate keys from one source therefore retain the first signature for
//! verification while still counting every pushed share. Core: v24.0.0
//! `src/llmq/net_signing.cpp:398-459`, `src/llmq/signing_shares.cpp:45-55`,
//! `src/bls/bls_batchverifier.h:68-80`.

use std::cell::RefCell;
use std::collections::BTreeSet;

use dash_evo::locks::sign_hash;
use dash_evo::locks::wire::{SigRead, read_lazy_sig};
use dash_types::Numeric;
use rand_core::CryptoRng;

use crate::dkg::bls::{BlsBatchVerifier, BlsCtx, verify_insecure};
use crate::sigenv::{
    BatchVerdict, CapturedShare, PeerId, RecSigReads, ShareBatch, ShareVerifier, SigningQuorum,
    VerifyError,
};

/// Verifies each source's shares and rejects every share from a failing source.
///
/// A source's duplicate `(sign_hash, quorum_member)` keys follow Core's
/// first-wins rule: the duplicate is decoded and counted, but its signature is
/// not checked. The key set is local to each source.
pub struct ReferenceVerifier;

impl ShareVerifier for ReferenceVerifier {
    // Core: v24.0.0 src/llmq/net_signing.cpp:398-459 NetSigning::ProcessPendingSigShares
    // Core: v24.0.0 src/llmq/signing_shares.cpp:45-55 CSigShare::UpdateKey / PushSigShareToBatchVerifier
    // Core: v24.0.0 src/llmq/signing_shares.cpp:583-603 quorum type/hash map
    // Core: v24.0.0 src/bls/bls_batchverifier.h:68-80 PushMessage first-wins message map
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        batch: &ShareBatch<Q>,
        recsigs: &R,
        check: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>> {
        let mut verdict = BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed: 0,
        };
        for (&source, shares) in &batch.by_source {
            let mut pushed_keys = BTreeSet::new();
            for CapturedShare {
                share,
                read,
            } in shares
            {
                let t = share.llmq_type();
                let id = share.id();
                let msg = share.msg_hash();
                if recsigs
                    .has_for_signing(t, id.as_bytes(), msg.as_bytes())
                    .map_err(VerifyError::RecSig)?
                {
                    continue;
                }
                let SigRead::Valid(sig) = read_lazy_sig(share.sig_share().as_bytes(), *read) else {
                    verdict.bad_sources.insert(source);
                    break;
                };
                let hash = share.quorum_hash();
                let key = (t.to_base(), hash);
                let quorum = batch.quorums.get(&key).ok_or(VerifyError::MissingQuorum(key))?;
                let member = share.quorum_member();
                let pk = quorum.pub_key_share(member).ok_or(VerifyError::MissingPubKeyShare {
                    key,
                    member,
                })?;
                verdict.pushed += 1;
                if !pushed_keys.insert((*share.sign_hash().as_bytes(), member)) {
                    continue;
                }
                if !verify_insecure(
                    &sig,
                    &pk,
                    &sign_hash(t, &hash, id.as_bytes(), msg.as_bytes()),
                    check.scheme(),
                ) {
                    verdict.bad_sources.insert(source);
                }
            }
        }
        Ok(verdict)
    }
}

/// Core's `SigShareBatchVerifier`: peers are the sources, and each message id carries its sender.
// Core: v24.0.0 src/llmq/signing_shares.h:92-95 SigShareBatchVerifier
type SigShareBatchVerifier = BlsBatchVerifier<PeerId, (PeerId, [u8; 32], u16)>;

/// `SigShareBatchVerifier(false, true)`: the insecure batch with the per-message fallback.
// Core: v24.0.0 src/llmq/net_signing.cpp:402-404 NetSigning::ProcessPendingSigShares
fn share_batch_verifier(check: BlsCtx) -> SigShareBatchVerifier {
    BlsBatchVerifier::new(false, true, 0, check.scheme())
}

/// Verifies a batch as Core's worker does: weighted aggregates over every pushed share, then per
/// source when the whole batch fails.
///
/// Each new message's 16-byte weight is drawn from `weights`. The verdict equals
/// [`ReferenceVerifier`]'s. A public-key share the verifier refuses is
/// [`VerifyError::MissingPubKeyShare`], as an absent one is.
pub struct WeightedVerifier<G> {
    weights: RefCell<G>,
}

impl<G: CryptoRng> WeightedVerifier<G> {
    pub fn new(weights: G) -> Self {
        Self {
            weights: RefCell::new(weights),
        }
    }
}

impl<G: CryptoRng> ShareVerifier for WeightedVerifier<G> {
    // Core: v24.0.0 src/llmq/net_signing.cpp:398-449 NetSigning::ProcessPendingSigShares
    // Core: v24.0.0 src/llmq/signing_shares.cpp:45-55 CSigShare::UpdateKey / PushSigShareToBatchVerifier
    fn verify_batch<Q: SigningQuorum, R: RecSigReads>(
        &self,
        batch: &ShareBatch<Q>,
        recsigs: &R,
        check: BlsCtx,
    ) -> Result<BatchVerdict, VerifyError<R::Error>> {
        let mut weights = self.weights.borrow_mut();
        let mut verifier = share_batch_verifier(check);
        let mut verdict = BatchVerdict {
            bad_sources: BTreeSet::new(),
            pushed: 0,
        };
        for (&source, shares) in &batch.by_source {
            for CapturedShare {
                share,
                read,
            } in shares
            {
                let t = share.llmq_type();
                let id = share.id();
                let msg = share.msg_hash();
                if recsigs
                    .has_for_signing(t, id.as_bytes(), msg.as_bytes())
                    .map_err(VerifyError::RecSig)?
                {
                    continue;
                }
                let SigRead::Valid(sig) = read_lazy_sig(share.sig_share().as_bytes(), *read) else {
                    verdict.bad_sources.insert(source);
                    break;
                };
                let hash = share.quorum_hash();
                let key = (t.to_base(), hash);
                let quorum = batch.quorums.get(&key).ok_or(VerifyError::MissingQuorum(key))?;
                let member = share.quorum_member();
                let message = (source, *share.sign_hash().as_bytes(), member);
                let signed = sign_hash(t, &hash, id.as_bytes(), msg.as_bytes());
                if !quorum.pub_key_share(member).is_some_and(|pk| {
                    verifier.push_message(source, message, signed, &sig, &pk, &mut *weights)
                }) {
                    return Err(VerifyError::MissingPubKeyShare {
                        key,
                        member,
                    });
                }
                verdict.pushed += 1;
            }
        }
        let _ = verifier.verify();
        verdict.bad_sources.extend(verifier.bad_sources());
        Ok(verdict)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
    use proptest::prelude::*;

    use super::*;
    use crate::dkg::bls::{
        BlsSchemeKind, BlsSkValue, add_and_subtract_error, secret_to_public, sign,
        verify_insecure_aggregated,
    };
    use crate::dkg::rng::SeededRng;
    use crate::dkg::wire::{QuorumHash, SigShare};
    use crate::sigenv::fakes::{FakeQuorum, FakeRecSigs};
    use crate::sigshares::recovery::tests::r2_requests;

    const TEST: LlmqType = LlmqType::LlmqTest;
    const BASIC: BlsCtx = BlsCtx::from_legacy(false);
    const LEGACY: BlsCtx = BlsCtx::from_legacy(true);
    const ID: [u8; 32] = [11; 32];
    const MSG: [u8; 32] = [12; 32];

    fn secret(n: u8) -> BlsSkValue {
        let mut bytes = [0; 32];
        bytes[31] = n;
        BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap()
    }

    fn quorum(t: LlmqType, first_key: u8) -> FakeQuorum {
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        FakeQuorum {
            llmq_type: t,
            quorum_hash: QuorumHash::from([10; 32]),
            params: *setup.consensus.llmq(t).unwrap(),
            members: (1..=3).map(|n| ProTxHash::from([n; 32])).collect(),
            valid_members: [0, 1, 2].into(),
            has_verification_vector: true,
            quorum_public_key: secret_to_public(&secret(first_key)),
            pk_shares: (0..3)
                .map(|i| (u16::from(i), secret_to_public(&secret(first_key + i))))
                .collect(),
            sk_share: None,
        }
    }

    fn share(q: &FakeQuorum, member: u16, key: u8, msg: [u8; 32], bls: BlsCtx) -> SigShare {
        let hash = sign_hash(q.llmq_type, &q.quorum_hash, &ID, &msg);
        let sig = sign(&secret(key), &hash, bls.is_legacy());
        SigShare::new(
            q.llmq_type,
            q.quorum_hash,
            member,
            ID.into(),
            msg.into(),
            sig.to_bytes(bls.scheme()).into(),
        )
    }

    fn malformed(q: &FakeQuorum) -> SigShare {
        SigShare::new(q.llmq_type, q.quorum_hash, 0, ID.into(), MSG.into(), [0; 96].into())
    }

    fn batch(
        quorums: Vec<FakeQuorum>,
        sources: impl IntoIterator<Item = (PeerId, Vec<SigShare>)>,
    ) -> ShareBatch<FakeQuorum> {
        captured_batch(
            quorums,
            sources.into_iter().map(|(peer, shares)| {
                (peer, shares.into_iter().map(|share| captured(share, BASIC)).collect())
            }),
        )
    }

    fn captured_batch(
        quorums: Vec<FakeQuorum>,
        sources: impl IntoIterator<Item = (PeerId, Vec<CapturedShare>)>,
    ) -> ShareBatch<FakeQuorum> {
        ShareBatch {
            by_source: sources.into_iter().collect(),
            quorums: quorums
                .into_iter()
                .map(|q| ((q.llmq_type.to_base(), q.quorum_hash), Arc::new(q)))
                .collect(),
        }
    }

    fn captured(share: SigShare, read: BlsCtx) -> CapturedShare {
        CapturedShare {
            share,
            read,
        }
    }

    fn outcomes<R: RecSigReads>(
        batch: &ShareBatch<FakeQuorum>,
        recsigs: &R,
        check: BlsCtx,
    ) -> [Result<BatchVerdict, VerifyError<R::Error>>; 2] {
        [
            ReferenceVerifier.verify_batch(batch, recsigs, check),
            WeightedVerifier::new(SeededRng::new(7)).verify_batch(batch, recsigs, check),
        ]
    }

    /// The weighted verdict, after checking that it equals the reference verdict.
    fn agreed<R: RecSigReads>(
        batch: &ShareBatch<FakeQuorum>,
        recsigs: &R,
        check: BlsCtx,
    ) -> BatchVerdict {
        let [reference, weighted] = outcomes(batch, recsigs, check).map(Result::unwrap);
        assert_eq!(weighted.bad_sources, reference.bad_sources);
        assert_eq!(weighted.pushed, reference.pushed);
        weighted
    }

    #[test]
    fn bad_share_marks_its_source_among_three_peers() {
        let q = quorum(TEST, 1);
        let good0 = share(&q, 0, 1, MSG, BASIC);
        let good1 = share(&q, 1, 2, MSG, BASIC);
        let good2 = share(&q, 2, 3, MSG, BASIC);
        let wrong_key = share(&q, 0, 9, MSG, BASIC);
        let batch = batch(
            vec![q],
            [
                (1, vec![good0.clone()]),
                (2, vec![good1, wrong_key, good2.clone()]),
                (3, vec![good2]),
            ],
        );
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [2].into());
        assert_eq!(verdict.pushed, 5);
    }

    #[test]
    fn duplicate_key_keeps_the_first_valid_signature() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 0, 9, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn duplicate_key_keeps_the_first_invalid_signature() {
        let q = quorum(TEST, 1);
        let invalid = share(&q, 0, 9, MSG, BASIC);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![invalid, valid])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn duplicate_key_still_decodes_and_stops_on_malformed_share() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let malformed = malformed(&q);
        let batch = batch(vec![q], [(1, vec![valid, malformed])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 1);
    }

    #[test]
    fn distinct_share_keys_from_one_source_are_each_verified() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 1, 9, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn same_member_with_another_message_is_a_distinct_key() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 0, 9, [13; 32], BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn same_share_key_from_two_sources_is_verified_for_each_source() {
        let q = quorum(TEST, 1);
        let good = share(&q, 0, 1, MSG, BASIC);
        let bad = share(&q, 0, 2, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![good]), (2, vec![bad])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [2].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn malformed_stops_its_source_but_crypto_failure_keeps_preparing() {
        let q = quorum(TEST, 1);
        let first = share(&q, 0, 1, [13; 32], BASIC);
        let wrong_key = share(&q, 0, 2, [14; 32], BASIC);
        let next = share(&q, 0, 1, [15; 32], BASIC);
        let last = share(&q, 0, 1, [16; 32], BASIC);
        let malformed = malformed(&q);
        let batch = batch(
            vec![q],
            [(1, vec![first.clone(), wrong_key, next, malformed, last]), (2, vec![first])],
        );
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 4);
    }

    #[test]
    fn missing_public_key_share_returns_the_key_and_member() {
        let mut q = quorum(TEST, 1);
        let good = share(&q, 1, 2, MSG, BASIC);
        let key = (TEST.to_base(), q.quorum_hash);
        q.pk_shares.remove(&1);
        let batch = batch(vec![q], [(1, vec![good])]);
        for outcome in outcomes(&batch, &FakeRecSigs::default(), BASIC) {
            assert!(
                matches!(outcome, Err(VerifyError::MissingPubKeyShare { key: got, member: 1 }) if got == key)
            );
        }
    }

    #[test]
    fn missing_quorum_returns_an_error_without_banning() {
        let q = quorum(TEST, 1);
        let good = share(&q, 0, 1, MSG, BASIC);
        let key = (TEST.to_base(), q.quorum_hash);
        let batch = batch(vec![], [(1, vec![good])]);
        for outcome in outcomes(&batch, &FakeRecSigs::default(), BASIC) {
            assert!(matches!(outcome, Err(VerifyError::MissingQuorum(got)) if got == key));
        }
    }

    #[test]
    fn malformed_is_rejected_before_quorum_or_key_lookup() {
        let q = quorum(TEST, 1);
        let batch = batch(vec![], [(1, vec![malformed(&q)])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 0);
    }

    #[test]
    fn scheme_comes_only_from_the_supplied_context() {
        let q = quorum(TEST, 1);
        let basic = share(&q, 0, 1, MSG, BASIC);
        let legacy = share(&q, 0, 1, MSG, LEGACY);
        let batch = captured_batch(
            vec![q],
            [(1, vec![captured(basic, BASIC)]), (2, vec![captured(legacy, LEGACY)])],
        );
        for (bls, bad) in [(BASIC, 2), (LEGACY, 1), (BASIC, 2)] {
            let verdict = agreed(&batch, &FakeRecSigs::default(), bls);
            assert_eq!(verdict.bad_sources, [bad].into());
            assert_eq!(verdict.pushed, 2);
        }
    }

    #[test]
    fn captured_read_contexts_are_applied_per_share() {
        let q = quorum(TEST, 1);
        let basic = share(&q, 0, 1, MSG, BASIC);
        let sig = sign(&secret(2), &sign_hash(TEST, &q.quorum_hash, &ID, &MSG), false);
        let legacy = SigShare::new(
            TEST,
            q.quorum_hash,
            1,
            ID.into(),
            MSG.into(),
            sig.to_bytes(BlsSchemeKind::Legacy).into(),
        );
        let batch =
            captured_batch(vec![q], [(1, vec![captured(basic, BASIC), captured(legacy, LEGACY)])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn verification_uses_check_context_instead_of_captured_read() {
        let q = quorum(TEST, 1);
        let sig = sign(&secret(1), &sign_hash(TEST, &q.quorum_hash, &ID, &MSG), false);
        let legacy = SigShare::new(
            TEST,
            q.quorum_hash,
            0,
            ID.into(),
            MSG.into(),
            sig.to_bytes(BlsSchemeKind::Legacy).into(),
        );
        let batch = captured_batch(vec![q], [(1, vec![captured(legacy, LEGACY)])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 1);
        let verdict = agreed(&batch, &FakeRecSigs::default(), LEGACY);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 1);
    }

    #[test]
    fn mixed_types_at_one_hash_use_distinct_public_key_shares() {
        let first = quorum(TEST, 1);
        let second = quorum(LlmqType::LlmqTestInstantsend, 4);
        assert_eq!(first.quorum_hash, second.quorum_hash);
        let good0 = share(&first, 0, 1, MSG, BASIC);
        let good1 = share(&second, 0, 4, MSG, BASIC);
        let relabelled = SigShare::new(
            second.llmq_type,
            first.quorum_hash,
            0,
            ID.into(),
            MSG.into(),
            good0.sig_share(),
        );
        let batch = batch(vec![first, second], [(1, vec![good0, good1]), (2, vec![relabelled])]);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [2].into());
        assert_eq!(verdict.pushed, 3);
    }

    #[test]
    fn recovered_after_collection_skips_malformed_before_decode() {
        let q = quorum(TEST, 1);
        let old = malformed(&q);
        let live = SigShare::new(
            TEST,
            q.quorum_hash,
            0,
            [14; 32].into(),
            MSG.into(),
            sign(&secret(1), &sign_hash(TEST, &q.quorum_hash, &[14; 32], &MSG), false)
                .to_bytes(BlsSchemeKind::Basic)
                .into(),
        );
        let batch = batch(vec![q], [(1, vec![old, live.clone()]), (2, vec![live])]);
        let mut recsigs = FakeRecSigs::default();
        let without = agreed(&batch, &recsigs, BASIC);
        assert_eq!(without.bad_sources, [1].into());
        assert_eq!(without.pushed, 1);
        recsigs.signatures.insert((TEST.to_base(), ID, MSG));
        let after = agreed(&batch, &recsigs, BASIC);
        assert!(after.bad_sources.is_empty());
        assert_eq!(after.pushed, 2);
    }

    #[test]
    fn platform_recovery_skips_only_the_matching_message() {
        let platform = LlmqType::LlmqTestPlatform;
        let q = quorum(platform, 1);
        let msg2 = [13; 32];
        let valid = share(&q, 0, 1, msg2, BASIC);
        let wrong_key = share(&q, 0, 2, msg2, BASIC);
        let old = malformed(&q);
        let batch = batch(vec![q], [(1, vec![old, valid]), (2, vec![wrong_key])]);
        let recsigs = FakeRecSigs {
            platform_type: Some(platform),
            signatures: [(platform.to_base(), ID, MSG)].into(),
            ..Default::default()
        };
        let verdict = agreed(&batch, &recsigs, BASIC);
        assert_eq!(verdict.bad_sources, [2].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn recovered_share_does_not_require_a_quorum_or_key() {
        let q = quorum(TEST, 1);
        let batch = batch(vec![], [(1, vec![malformed(&q)])]);
        let recsigs = FakeRecSigs {
            signatures: [(TEST.to_base(), ID, [99; 32])].into(),
            ..Default::default()
        };
        let verdict = agreed(&batch, &recsigs, BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 0);
    }

    #[test]
    fn recsig_read_error_is_propagated_before_decode() {
        struct FailingReads;
        impl RecSigReads for FailingReads {
            type Error = &'static str;
            fn has_for_signing(
                &self,
                _: LlmqType,
                _: &[u8; 32],
                _: &[u8; 32],
            ) -> Result<bool, Self::Error> {
                Err("read failed")
            }
            fn has_for_session(&self, _: &[u8; 32]) -> Result<bool, Self::Error> {
                panic!("verification uses has_for_signing")
            }
        }
        let q = quorum(TEST, 1);
        let batch = batch(vec![], [(1, vec![malformed(&q)])]);
        for outcome in outcomes(&batch, &FailingReads, BASIC) {
            assert!(matches!(outcome, Err(VerifyError::RecSig("read failed"))));
        }
    }

    #[test]
    fn empty_batch_has_no_bad_sources_or_pushed_shares() {
        let batch = batch(vec![], []);
        let verdict = agreed(&batch, &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 0);
    }

    #[test]
    fn all_members_verify_and_a_bit_flip_rejects_only_its_source() {
        for bls in [LEGACY, BASIC] {
            let q = quorum(TEST, 1);
            let shares: Vec<_> = (0..3)
                .map(|member| share(&q, member, u8::try_from(member).unwrap() + 1, MSG, bls))
                .collect();
            let mut bytes = *shares[0].sig_share().as_bytes();
            bytes[48] ^= 1;
            let flipped =
                SigShare::new(TEST, q.quorum_hash, 0, ID.into(), MSG.into(), bytes.into());
            let batch = captured_batch(
                vec![q],
                [
                    (1, shares.into_iter().map(|share| captured(share, bls)).collect()),
                    (2, vec![captured(flipped, bls)]),
                ],
            );
            let verdict = agreed(&batch, &FakeRecSigs::default(), bls);
            assert_eq!(verdict.bad_sources, [2].into());
            assert_eq!(verdict.pushed, 3);
        }
    }

    #[test]
    fn cancelling_signature_errors_are_rejected_individually() {
        for bls in [LEGACY, BASIC] {
            let q = quorum(TEST, 1);
            let hash = sign_hash(TEST, &q.quorum_hash, &ID, &MSG);
            let a = sign(&secret(1), &hash, bls.is_legacy());
            let b = sign(&secret(2), &hash, bls.is_legacy());
            let error = sign(&secret(3), &[99; 32], bls.is_legacy());
            let (a, b) = add_and_subtract_error(&a, &b, &error);
            let shares: Vec<_> = [a, b]
                .into_iter()
                .enumerate()
                .map(|(member, sig)| {
                    assert!(sig.is_valid());
                    SigShare::new(
                        TEST,
                        q.quorum_hash,
                        u16::try_from(member).unwrap(),
                        ID.into(),
                        MSG.into(),
                        sig.to_bytes(bls.scheme()).into(),
                    )
                })
                .collect();
            let batch = captured_batch(
                vec![q],
                [
                    (1, vec![captured(shares[0].clone(), bls)]),
                    (2, vec![captured(shares[1].clone(), bls)]),
                ],
            );
            let verdict = agreed(&batch, &FakeRecSigs::default(), bls);
            assert_eq!(verdict.bad_sources, [1, 2].into());
            assert_eq!(verdict.pushed, 2);
        }
    }

    #[test]
    fn share_batch_verifier_is_cores_insecure_batch_with_message_fallback() {
        for (bls, scheme) in [(LEGACY, BlsSchemeKind::Legacy), (BASIC, BlsSchemeKind::Basic)] {
            assert_eq!(share_batch_verifier(bls), BlsBatchVerifier::new(false, true, 0, scheme));
            assert_ne!(share_batch_verifier(bls), BlsBatchVerifier::new(true, true, 0, scheme));
            assert_ne!(share_batch_verifier(bls), BlsBatchVerifier::new(false, false, 0, scheme));
        }
    }

    #[test]
    fn cancelling_errors_in_one_source_pass_unweighted_but_not_weighted() {
        for bls in [LEGACY, BASIC] {
            let q = quorum(TEST, 1);
            let msg2 = [13; 32];
            let first = sign_hash(TEST, &q.quorum_hash, &ID, &MSG);
            let second = sign_hash(TEST, &q.quorum_hash, &ID, &msg2);
            let error = sign(&secret(3), &[99; 32], bls.is_legacy());
            let (a, b) = add_and_subtract_error(
                &sign(&secret(1), &first, bls.is_legacy()),
                &sign(&secret(2), &second, bls.is_legacy()),
                &error,
            );
            let (pk0, pk1) = (&q.pk_shares[&0], &q.pk_shares[&1]);
            assert!(!verify_insecure(&a, pk0, &first, bls.scheme()));
            assert!(!verify_insecure(&b, pk1, &second, bls.scheme()));
            assert!(verify_insecure_aggregated(
                &[&a, &b],
                &[pk0, pk1],
                &[first, second],
                bls.scheme()
            ));
            let shares = [(0, MSG, a), (1, msg2, b)]
                .into_iter()
                .map(|(member, msg, sig)| {
                    let share = SigShare::new(
                        TEST,
                        q.quorum_hash,
                        member,
                        ID.into(),
                        msg.into(),
                        sig.to_bytes(bls.scheme()).into(),
                    );
                    captured(share, bls)
                })
                .collect();
            let batch = captured_batch(vec![q], [(1, shares)]);
            let verdict = agreed(&batch, &FakeRecSigs::default(), bls);
            assert_eq!(verdict.bad_sources, [1].into());
            assert_eq!(verdict.pushed, 2);
        }
    }

    #[test]
    fn single_source_batch() {
        let shares = |q: &FakeQuorum, wrong: Option<u16>| -> Vec<_> {
            (0..3)
                .map(|m| {
                    let key = if wrong == Some(m) {
                        9
                    } else {
                        u8::try_from(m).unwrap() + 1
                    };
                    share(q, m, key, MSG, BASIC)
                })
                .collect()
        };
        let q = quorum(TEST, 1);
        let good = batch(vec![quorum(TEST, 1)], [(1, shares(&q, None))]);
        let verdict = agreed(&good, &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 3);
        let bad = batch(vec![quorum(TEST, 1)], [(1, shares(&q, Some(1)))]);
        let verdict = agreed(&bad, &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 3);
    }

    #[test]
    fn r2_offline_shares_verify_and_a_bad_share_bans_only_its_source() {
        let requests = r2_requests();
        let mut sources: Vec<(PeerId, Vec<SigShare>)> = Vec::new();
        let mut quorums = BTreeMap::new();
        for (peer, r) in (0..).zip(requests) {
            let q = r.quorum;
            let shares = r
                .shares
                .iter()
                .map(|(&member, sig)| {
                    SigShare::new(
                        q.llmq_type,
                        q.quorum_hash,
                        member,
                        r.id.into(),
                        r.msg_hash.into(),
                        sig.to_bytes(BlsSchemeKind::Basic).into(),
                    )
                })
                .collect();
            sources.push((peer, shares));
            quorums.insert((q.llmq_type.to_base(), q.quorum_hash), Arc::new(q));
        }
        let total: usize = sources.iter().map(|(_, shares)| shares.len()).sum();
        let r2_batch = |sources: &[(PeerId, Vec<SigShare>)]| ShareBatch {
            by_source: sources
                .iter()
                .map(|(peer, shares)| {
                    (*peer, shares.iter().map(|share| captured(share.clone(), BASIC)).collect())
                })
                .collect(),
            quorums: quorums.clone(),
        };
        let verdict = agreed(&r2_batch(&sources), &FakeRecSigs::default(), BASIC);
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, total);

        let mut flipped = sources.clone();
        let target = &mut flipped[5].1[1];
        let mut bytes = *target.sig_share().as_bytes();
        bytes[48] ^= 1;
        *target = SigShare::new(
            target.llmq_type(),
            target.quorum_hash(),
            target.quorum_member(),
            target.id(),
            target.msg_hash(),
            bytes.into(),
        );
        let verdict = agreed(&r2_batch(&flipped), &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [5].into());

        let mut swapped = flipped;
        let other = swapped[9].1[0].sig_share();
        let target = &mut swapped[9].1[1];
        *target = SigShare::new(
            target.llmq_type(),
            target.quorum_hash(),
            target.quorum_member(),
            target.id(),
            target.msg_hash(),
            other,
        );
        let verdict = agreed(&r2_batch(&swapped), &FakeRecSigs::default(), BASIC);
        assert_eq!(verdict.bad_sources, [5, 9].into());
    }

    const GOOD: u8 = 0;
    const WRONG_KEY: u8 = 1;
    const MALFORMED: u8 = 2;
    const SIGNED_IN_OTHER_SCHEME: u8 = 3;
    const ENCODED_IN_OTHER_SCHEME: u8 = 4;

    fn arb_kind() -> impl Strategy<Value = u8> {
        prop_oneof![
            10 => Just(GOOD),
            1 => Just(WRONG_KEY),
            1 => Just(MALFORMED),
            1 => Just(SIGNED_IN_OTHER_SCHEME),
            1 => Just(ENCODED_IN_OTHER_SCHEME),
        ]
    }

    fn arb_member() -> impl Strategy<Value = u16> {
        prop_oneof![30 => 0u16..3, 1 => Just(3u16)]
    }

    type Spec = (bool, u16, u8, u8, u8, bool);

    fn arb_spec() -> impl Strategy<Value = Spec> {
        (any::<bool>(), arb_member(), 0u8..2, 0u8..2, arb_kind(), any::<bool>())
    }

    fn ctx(legacy: bool) -> BlsCtx {
        if legacy {
            LEGACY
        } else {
            BASIC
        }
    }

    fn spec_share(spec: Spec, check: BlsCtx) -> CapturedShare {
        let (instantsend, member, id, msg, kind, read_legacy) = spec;
        let (t, first_key) = if instantsend {
            (LlmqType::LlmqTestInstantsend, 4)
        } else {
            (TEST, 1)
        };
        let quorum_hash = QuorumHash::from([10; 32]);
        let id = [20 + id; 32];
        let msg = [30 + msg; 32];
        let read = ctx(read_legacy);
        let bytes = if kind == MALFORMED {
            [0; 96]
        } else {
            let key = if kind == WRONG_KEY {
                9
            } else {
                first_key + u8::try_from(member).unwrap()
            };
            let sign_legacy = check.is_legacy() != (kind == SIGNED_IN_OTHER_SCHEME);
            let encoding = ctx(read.is_legacy() != (kind == ENCODED_IN_OTHER_SCHEME));
            let sig = sign(&secret(key), &sign_hash(t, &quorum_hash, &id, &msg), sign_legacy);
            sig.to_bytes(encoding.scheme())
        };
        captured(SigShare::new(t, quorum_hash, member, id.into(), msg.into(), bytes.into()), read)
    }

    fn summary<E: core::fmt::Debug>(
        outcome: Result<BatchVerdict, VerifyError<E>>,
    ) -> Result<(BTreeSet<PeerId>, usize), String> {
        outcome.map(|v| (v.bad_sources, v.pushed)).map_err(|e| format!("{e:?}"))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn weighted_verdict_equals_the_reference_verdict(
            sources in prop::collection::btree_map(
                1i64..=4,
                prop::collection::vec(arb_spec(), 1..=6),
                1..=4,
            ),
            recovered in prop::collection::btree_set((any::<bool>(), 0u8..2), 0..=1),
            check_legacy in any::<bool>(),
            seed in any::<u64>(),
        ) {
            let check = ctx(check_legacy);
            let batch = captured_batch(
                vec![quorum(TEST, 1), quorum(LlmqType::LlmqTestInstantsend, 4)],
                sources.into_iter().map(|(peer, specs)| {
                    (peer, specs.into_iter().map(|spec| spec_share(spec, check)).collect())
                }),
            );
            let recsigs = FakeRecSigs {
                signatures: recovered
                    .into_iter()
                    .map(|(instantsend, id)| {
                        let t = if instantsend { LlmqType::LlmqTestInstantsend } else { TEST };
                        (t.to_base(), [20 + id; 32], MSG)
                    })
                    .collect(),
                ..Default::default()
            };
            let reference = ReferenceVerifier.verify_batch(&batch, &recsigs, check);
            let weighted =
                WeightedVerifier::new(SeededRng::new(seed)).verify_batch(&batch, &recsigs, check);
            prop_assert_eq!(summary(weighted), summary(reference));
        }
    }
}
