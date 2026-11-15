//! Individual share verification with source-wide rejection.
//!
//! Core keys batch-verifier messages by `(source, sign_hash, quorum_member)`;
//! duplicate keys from one source therefore retain the first signature for
//! verification while still counting every pushed share. Core: v24.0.0
//! `src/llmq/net_signing.cpp:398-459`, `src/llmq/signing_shares.cpp:45-55`,
//! `src/bls/bls_batchverifier.h:68-80`.

use std::collections::BTreeSet;

use dash_evo::locks::sign_hash;
use dash_evo::locks::wire::{SigRead, read_lazy_sig};
use dash_types::Numeric;

use crate::dkg::bls::{BlsCtx, verify_insecure};
use crate::sigenv::{
    BatchVerdict, CapturedShare, RecSigReads, ShareBatch, ShareVerifier, SigningQuorum, VerifyError,
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};

    use super::*;
    use crate::dkg::bls::{
        BlsSchemeKind, BlsSkValue, add_and_subtract_error, secret_to_public, sign,
    };
    use crate::dkg::wire::{QuorumHash, SigShare};
    use crate::sigenv::PeerId;
    use crate::sigenv::fakes::{FakeQuorum, FakeRecSigs};

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
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert_eq!(verdict.bad_sources, [2].into());
        assert_eq!(verdict.pushed, 5);
    }

    #[test]
    fn duplicate_key_keeps_the_first_valid_signature() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 0, 9, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn duplicate_key_keeps_the_first_invalid_signature() {
        let q = quorum(TEST, 1);
        let invalid = share(&q, 0, 9, MSG, BASIC);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![invalid, valid])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn duplicate_key_still_decodes_and_stops_on_malformed_share() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let malformed = malformed(&q);
        let batch = batch(vec![q], [(1, vec![valid, malformed])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 1);
    }

    #[test]
    fn distinct_share_keys_from_one_source_are_each_verified() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 1, 9, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn same_member_with_another_message_is_a_distinct_key() {
        let q = quorum(TEST, 1);
        let valid = share(&q, 0, 1, MSG, BASIC);
        let invalid = share(&q, 0, 9, [13; 32], BASIC);
        let batch = batch(vec![q], [(1, vec![valid, invalid])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert_eq!(verdict.bad_sources, [1].into());
        assert_eq!(verdict.pushed, 2);
    }

    #[test]
    fn same_share_key_from_two_sources_is_verified_for_each_source() {
        let q = quorum(TEST, 1);
        let good = share(&q, 0, 1, MSG, BASIC);
        let bad = share(&q, 0, 2, MSG, BASIC);
        let batch = batch(vec![q], [(1, vec![good]), (2, vec![bad])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
        assert!(matches!(
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC),
            Err(VerifyError::MissingPubKeyShare { key: got, member: 1 }) if got == key
        ));
    }

    #[test]
    fn missing_quorum_returns_an_error_without_banning() {
        let q = quorum(TEST, 1);
        let good = share(&q, 0, 1, MSG, BASIC);
        let key = (TEST.to_base(), q.quorum_hash);
        let batch = batch(vec![], [(1, vec![good])]);
        assert!(matches!(
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC),
            Err(VerifyError::MissingQuorum(got)) if got == key
        ));
    }

    #[test]
    fn malformed_is_rejected_before_quorum_or_key_lookup() {
        let q = quorum(TEST, 1);
        let batch = batch(vec![], [(1, vec![malformed(&q)])]);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
            let verdict =
                ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), bls).unwrap();
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
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
        assert!(verdict.bad_sources.is_empty());
        assert_eq!(verdict.pushed, 1);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), LEGACY).unwrap();
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
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
        let without = ReferenceVerifier.verify_batch(&batch, &recsigs, BASIC).unwrap();
        assert_eq!(without.bad_sources, [1].into());
        assert_eq!(without.pushed, 1);
        recsigs.signatures.insert((TEST.to_base(), ID, MSG));
        let after = ReferenceVerifier.verify_batch(&batch, &recsigs, BASIC).unwrap();
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
        let verdict = ReferenceVerifier.verify_batch(&batch, &recsigs, BASIC).unwrap();
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
        let verdict = ReferenceVerifier.verify_batch(&batch, &recsigs, BASIC).unwrap();
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
        assert!(matches!(
            ReferenceVerifier.verify_batch(&batch, &FailingReads, BASIC),
            Err(VerifyError::RecSig("read failed"))
        ));
    }

    #[test]
    fn empty_batch_has_no_bad_sources_or_pushed_shares() {
        let batch = batch(vec![], []);
        let verdict =
            ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), BASIC).unwrap();
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
            let verdict =
                ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), bls).unwrap();
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
            let verdict =
                ReferenceVerifier.verify_batch(&batch, &FakeRecSigs::default(), bls).unwrap();
            assert_eq!(verdict.bad_sources, [1, 2].into());
            assert_eq!(verdict.pushed, 2);
        }
    }
}
