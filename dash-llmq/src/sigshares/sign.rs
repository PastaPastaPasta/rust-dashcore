//! SG1b-4: create_sig_share, push_pending_sign and dispatch_pending_signs.
//!
//! Core: v24.0.0 l/signing_shares.cpp:1516-1649, except SignAndProcessSingleShare (SG1b-5).

use dash_evo::locks::sign_hash;
use dash_evo::types::ProTxHash;
use dash_params::LlmqParamsExt;

use crate::dkg::bls::{BlsCtx, BlsSigValue};
use crate::dkg::wire::{SigShare, SigShareBytes};
use crate::sigenv::{OperatorSigner, PendingSign, SignerFacts, SigningQuorum};

use super::manager::SigSharesManager;

/// Why [`create_share`] made no share: each of Core's early returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareRefusal {
    /// `IsQuorumSigningAllowed` is false.
    SigningNotAllowed,
    /// This node is not a valid member, or has no proTxHash.
    NotValidMember,
    /// A multi-member quorum without this node's valid secret key share.
    NoSkShare,
    /// `GetMemberIndex` found no index.
    NoMemberIndex,
    /// The signature over `sign_hash` is not valid.
    InvalidSig {
        sign_hash: [u8; 32],
    },
}

/// This node's signature share for `(id, msg)` in `q`, signed under `bls`'s scheme; `None` when
/// signing is not allowed, this node is not a valid member, it holds no valid secret key share
/// (multi-member quorums), or no valid signature results.
// Core: v24.0.0 l/signing_shares.cpp:1601-1649 CSigSharesManager::CreateSigShare
pub fn create_sig_share<Q: SigningQuorum, O: OperatorSigner>(
    q: &Q,
    id: [u8; 32],
    msg: [u8; 32],
    facts: &SignerFacts,
    op: &O,
    bls: BlsCtx,
) -> Option<SigShare> {
    create_share(q, id, msg, facts, op, bls).ok()
}

/// [`create_sig_share`] with the early return that made no share.
// Core: v24.0.0 l/signing_shares.cpp:1601-1649 CSigSharesManager::CreateSigShare
pub fn create_share<Q: SigningQuorum, O: OperatorSigner>(
    q: &Q,
    id: [u8; 32],
    msg: [u8; 32],
    facts: &SignerFacts,
    op: &O,
    bls: BlsCtx,
) -> Result<SigShare, ShareRefusal> {
    if !facts.signing_allowed {
        return Err(ShareRefusal::SigningNotAllowed);
    }
    let my_pro_tx = facts.my_pro_tx.as_ref().ok_or(ShareRefusal::NotValidMember)?;
    if !q.is_valid_member(my_pro_tx) {
        return Err(ShareRefusal::NotValidMember);
    }
    if q.params().is_single_member() {
        return create_for_single_member(q, id, msg, my_pro_tx, op, bls);
    }
    let sk = q.sk_share().filter(|sk| sk.is_valid()).ok_or(ShareRefusal::NoSkShare)?;
    let member = q.member_index(my_pro_tx).ok_or(ShareRefusal::NoMemberIndex)?;
    let hash = sign_hash(q.llmq_type(), q.quorum_hash(), &id, &msg);
    let sig = crate::dkg::bls::sign(&sk, &hash, bls.is_legacy());
    finish_share(q, member, id, msg, hash, Some(sig), bls)
}

/// The single-member path signs with the operator key, not a quorum key share.
// Core: v24.0.0 l/signing_shares.cpp:1564-1599 CSigSharesManager::CreateSigShareForSingleMember
fn create_for_single_member<Q: SigningQuorum, O: OperatorSigner>(
    q: &Q,
    id: [u8; 32],
    msg: [u8; 32],
    my_pro_tx: &ProTxHash,
    op: &O,
    bls: BlsCtx,
) -> Result<SigShare, ShareRefusal> {
    let member = q.member_index(my_pro_tx).ok_or(ShareRefusal::NoMemberIndex)?;
    let hash = sign_hash(q.llmq_type(), q.quorum_hash(), &id, &msg);
    finish_share(q, member, id, msg, hash, op.sign(&hash, bls.scheme()), bls)
}

fn finish_share<Q: SigningQuorum>(
    q: &Q,
    member: u16,
    id: [u8; 32],
    msg: [u8; 32],
    hash: [u8; 32],
    sig: Option<BlsSigValue>,
    bls: BlsCtx,
) -> Result<SigShare, ShareRefusal> {
    let sig = sig.filter(BlsSigValue::is_valid).ok_or(ShareRefusal::InvalidSig {
        sign_hash: hash,
    })?;
    Ok(SigShare::new(
        q.llmq_type(),
        *q.quorum_hash(),
        member,
        id.into(),
        msg.into(),
        SigShareBytes::from_bytes(sig.to_bytes(bls.scheme())),
    ))
}

impl<Q: SigningQuorum> SigSharesManager<Q> {
    /// Queue a signing request for the signing worker.
    // Core: v24.0.0 l/signing_shares.cpp:1553-1557 CSigSharesManager::AsyncSign
    pub fn push_pending_sign(&mut self, w: PendingSign<Q>) {
        self.pending_signs.push(w);
    }

    /// Take every queued signing request, in push order, leaving the queue empty.
    // Core: v24.0.0 l/signing_shares.cpp:1516-1525 CSigSharesManager::DispatchPendingSigns
    pub fn dispatch_pending_signs(&mut self) -> Vec<PendingSign<Q>> {
        std::mem::take(&mut self.pending_signs)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};

    use super::*;
    use crate::dkg::bls::{BlsSchemeKind, BlsSkValue, secret_to_public, verify_insecure};
    use crate::dkg::wire::QuorumHash;
    use crate::sigenv::fakes::{FakeOperatorSigner, FakeQuorum};

    const T: LlmqType = LlmqType::LlmqTest;
    const QH: [u8; 32] = [8; 32];
    const ID: [u8; 32] = [3; 32];
    const MSG: [u8; 32] = [4; 32];

    fn secret(byte: u8) -> BlsSkValue {
        let mut bytes = [0; 32];
        bytes[31] = byte;
        BlsSkValue::from_canonical(&bytes, BlsCtx::BASIC.scheme()).expect("test key")
    }

    fn pro_tx(i: u16) -> [u8; 32] {
        [i as u8 + 1; 32]
    }

    /// A quorum of `size` members (member `i` is `pro_tx(i)`) where only `valid` is valid.
    fn quorum(size: u16, valid: u16, sk_share: Option<BlsSkValue>) -> FakeQuorum {
        let setup =
            ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().expect("params");
        let base = *setup.consensus.llmq(T).expect("test params");
        let params = if size == 1 {
            dash_params::LlmqParams {
                size: 1,
                min_size: 1,
                threshold: 1,
                ..base
            }
        } else {
            base
        };
        let quorum_sk = secret(7);
        let member_pk = secret_to_public(sk_share.as_ref().unwrap_or(&quorum_sk));
        FakeQuorum {
            llmq_type: T,
            quorum_hash: QuorumHash::from(QH),
            params,
            members: (0..size).map(|i| pro_tx(i).into()).collect(),
            valid_members: [valid].into_iter().collect(),
            has_verification_vector: true,
            quorum_public_key: secret_to_public(&quorum_sk),
            pk_shares: [(valid, member_pk)].into_iter().collect(),
            sk_share,
        }
    }

    fn facts(member: [u8; 32]) -> SignerFacts {
        SignerFacts {
            my_pro_tx: Some(member.into()),
            signing_allowed: true,
        }
    }

    fn no_op() -> FakeOperatorSigner {
        FakeOperatorSigner::default()
    }

    /// The share names `q`, `member`, `ID`, `MSG`, and its signature verifies under `sk`'s
    /// public key on Core's sign hash in `bls`'s scheme.
    fn assert_signed(share: &SigShare, member: u16, sk: &BlsSkValue, bls: BlsCtx) {
        assert_eq!(share.llmq_type(), T);
        assert_eq!(*share.quorum_hash().as_bytes(), QH);
        assert_eq!(share.quorum_member(), member);
        assert_eq!(*share.id().as_bytes(), ID);
        assert_eq!(*share.msg_hash().as_bytes(), MSG);
        let hash = sign_hash(T, &QuorumHash::from(QH), &ID, &MSG);
        assert_eq!(share.sign_hash().as_bytes(), &hash);
        let sig = BlsSigValue::from_canonical(&share.sig_share().to_bytes(), bls.scheme())
            .expect("signature bytes");
        assert!(verify_insecure(&sig, &secret_to_public(sk), &hash, bls.scheme()));
    }

    #[test]
    fn creates_a_share_with_the_quorum_secret_share() {
        let sk = secret(9);
        let q = quorum(3, 1, Some(sk.clone()));
        let share = create_sig_share(&q, ID, MSG, &facts(pro_tx(1)), &no_op(), BlsCtx::BASIC)
            .expect("share");
        assert_signed(&share, 1, &sk, BlsCtx::BASIC);
    }

    #[test]
    fn signs_under_the_given_scheme() {
        let sk = secret(9);
        let q = quorum(3, 2, Some(sk.clone()));
        let legacy = create_sig_share(&q, ID, MSG, &facts(pro_tx(2)), &no_op(), BlsCtx::LEGACY)
            .expect("legacy share");
        assert_signed(&legacy, 2, &sk, BlsCtx::LEGACY);
        let basic = create_sig_share(&q, ID, MSG, &facts(pro_tx(2)), &no_op(), BlsCtx::BASIC)
            .expect("basic share");
        assert_ne!(legacy.sig_share().to_bytes(), basic.sig_share().to_bytes());
    }

    #[test]
    fn refuses_when_signing_is_disallowed() {
        let q = quorum(3, 0, Some(secret(9)));
        let mut signer = facts(pro_tx(0));
        assert!(create_sig_share(&q, ID, MSG, &signer, &no_op(), BlsCtx::BASIC).is_some());
        signer.signing_allowed = false;
        assert!(create_sig_share(&q, ID, MSG, &signer, &no_op(), BlsCtx::BASIC).is_none());
    }

    #[test]
    fn refuses_a_node_without_a_pro_tx_hash() {
        let q = quorum(3, 0, Some(secret(9)));
        let signer = SignerFacts {
            my_pro_tx: None,
            signing_allowed: true,
        };
        assert!(create_sig_share(&q, ID, MSG, &signer, &no_op(), BlsCtx::BASIC).is_none());
    }

    #[test]
    fn refuses_a_non_member() {
        let q = quorum(3, 0, Some(secret(9)));
        assert!(create_sig_share(&q, ID, MSG, &facts([9; 32]), &no_op(), BlsCtx::BASIC).is_none());
    }

    #[test]
    fn refuses_an_invalid_member() {
        let q = quorum(3, 1, Some(secret(9)));
        assert!(q.is_member(&pro_tx(0).into()));
        assert!(
            create_sig_share(&q, ID, MSG, &facts(pro_tx(0)), &no_op(), BlsCtx::BASIC).is_none()
        );
    }

    #[test]
    fn refuses_without_a_valid_secret_share() {
        let op = FakeOperatorSigner {
            key: Some(secret(11)),
        };
        for sk_share in [None, Some(BlsSkValue::invalid())] {
            let q = quorum(3, 0, sk_share);
            assert!(create_sig_share(&q, ID, MSG, &facts(pro_tx(0)), &op, BlsCtx::BASIC).is_none());
        }
    }

    #[test]
    fn size_one_signs_with_the_operator_key() {
        let q = quorum(1, 0, Some(secret(9)));
        let sk = secret(11);
        let op = FakeOperatorSigner {
            key: Some(sk.clone()),
        };
        for bls in [BlsCtx::BASIC, BlsCtx::LEGACY] {
            let share = create_sig_share(&q, ID, MSG, &facts(pro_tx(0)), &op, bls).expect("share");
            assert_signed(&share, 0, &sk, bls);
        }
    }

    #[test]
    fn size_one_refuses_when_the_operator_cannot_sign() {
        let q = quorum(1, 0, Some(secret(9)));
        assert!(
            create_sig_share(&q, ID, MSG, &facts(pro_tx(0)), &no_op(), BlsCtx::BASIC).is_none()
        );
    }

    struct InvalidSigSigner;

    impl OperatorSigner for InvalidSigSigner {
        fn sign(&self, _: &[u8; 32], _: BlsSchemeKind) -> Option<BlsSigValue> {
            Some(BlsSigValue::invalid())
        }
    }

    #[test]
    fn size_one_refuses_an_invalid_operator_signature() {
        let q = quorum(1, 0, Some(secret(9)));
        assert!(
            create_sig_share(&q, ID, MSG, &facts(pro_tx(0)), &InvalidSigSigner, BlsCtx::BASIC)
                .is_none()
        );
    }

    #[test]
    fn size_one_keeps_the_signing_gates() {
        let q = quorum(1, 0, None);
        let op = FakeOperatorSigner {
            key: Some(secret(11)),
        };
        let mut signer = facts(pro_tx(0));
        signer.signing_allowed = false;
        assert!(create_sig_share(&q, ID, MSG, &signer, &op, BlsCtx::BASIC).is_none());
        assert!(create_sig_share(&q, ID, MSG, &facts([9; 32]), &op, BlsCtx::BASIC).is_none());
        let invalid = quorum(1, 1, None);
        assert!(
            create_sig_share(&invalid, ID, MSG, &facts(pro_tx(0)), &op, BlsCtx::BASIC).is_none()
        );
    }

    #[test]
    fn refusals_name_cores_early_returns() {
        let op = FakeOperatorSigner {
            key: Some(secret(11)),
        };
        let q = quorum(3, 0, Some(secret(9)));
        let mut signer = facts(pro_tx(0));
        signer.signing_allowed = false;
        fn refusal(q: &FakeQuorum, f: &SignerFacts, op: &impl OperatorSigner) -> ShareRefusal {
            create_share(q, ID, MSG, f, op, BlsCtx::BASIC).unwrap_err()
        }
        assert_eq!(refusal(&q, &signer, &op), ShareRefusal::SigningNotAllowed);
        assert_eq!(refusal(&q, &facts([9; 32]), &op), ShareRefusal::NotValidMember);
        let none = SignerFacts {
            my_pro_tx: None,
            signing_allowed: true,
        };
        assert_eq!(refusal(&q, &none, &op), ShareRefusal::NotValidMember);
        assert_eq!(
            refusal(&quorum(3, 1, Some(secret(9))), &facts(pro_tx(0)), &op),
            ShareRefusal::NotValidMember
        );
        for sk_share in [None, Some(BlsSkValue::invalid())] {
            assert_eq!(
                refusal(&quorum(3, 0, sk_share), &facts(pro_tx(0)), &op),
                ShareRefusal::NoSkShare
            );
        }
        let single = quorum(1, 0, None);
        let hash = sign_hash(T, &QuorumHash::from(QH), &ID, &MSG);
        let invalid = ShareRefusal::InvalidSig {
            sign_hash: hash,
        };
        assert_eq!(refusal(&single, &facts(pro_tx(0)), &no_op()), invalid);
        assert_eq!(refusal(&single, &facts(pro_tx(0)), &InvalidSigSigner), invalid);
        assert!(create_share(&single, ID, MSG, &facts(pro_tx(0)), &op, BlsCtx::BASIC).is_ok());
    }

    #[test]
    fn pending_signs_drain_in_push_order() {
        let a = Arc::new(quorum(3, 0, Some(secret(9))));
        let b = Arc::new(quorum(3, 1, Some(secret(9))));
        let mut manager = SigSharesManager::new();
        assert!(manager.dispatch_pending_signs().is_empty());
        let pushed = [(&a, [1; 32], [2; 32]), (&b, [3; 32], [4; 32]), (&a, [1; 32], [7; 32])];
        for (q, id, msg_hash) in pushed {
            manager.push_pending_sign(PendingSign {
                quorum: Arc::clone(q),
                id,
                msg_hash,
            });
        }
        let drained = manager.dispatch_pending_signs();
        assert_eq!(drained.len(), pushed.len());
        for (got, (q, id, msg_hash)) in drained.iter().zip(pushed) {
            assert!(Arc::ptr_eq(&got.quorum, q));
            assert_eq!((got.id, got.msg_hash), (id, msg_hash));
        }
        assert!(manager.dispatch_pending_signs().is_empty());
        manager.push_pending_sign(PendingSign {
            quorum: a,
            id: [5; 32],
            msg_hash: [6; 32],
        });
        assert_eq!(manager.dispatch_pending_signs().len(), 1);
    }
}
