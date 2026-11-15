//! The pure sign-if-member decision used by the sig-shares manager.

use std::sync::Arc;

use crate::sigenv::{
    QuorumLookup, SignRequest, SignerFacts, SigningQuorum, select_for_signing_default,
};

use super::db::{RecSigDb, RecSigKv};

/// Why a sign request was refused before it could enter the sig-shares queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// Snapshot validation does not permit quorum signing yet.
    SigningNotAllowed,
    /// The active masternode has no registered proTxHash.
    NoProTx,
    /// No quorum could be selected or found for the request.
    NoQuorum,
    /// The active masternode is not a valid member of the selected quorum.
    NotValidMember,
    /// A vote for this id already exists for the same message and resigning is disabled.
    AlreadyVoted,
    /// A vote for this id exists for a different message and conflicting signing is disabled.
    ConflictingMessage,
}

/// The result of Core's `AsyncSignIfMember` policy checks.
pub enum SignDecision<Q: SigningQuorum> {
    /// The request was refused; no vote was written.
    Refuse(Reason),
    /// A recovered signature already exists; no vote or reannouncement is needed.
    AlreadyRecovered,
    /// The caller should queue signing for `quorum`.
    Sign {
        quorum: Arc<Q>,
        force_reannounce: bool,
    },
}

impl<Q: SigningQuorum> SignDecision<Q> {
    /// Maps the decision to Core's `AsyncSignIfMember` boolean result.
    // Core: v24.0.0 src/llmq/signing_shares.cpp:858-861 and rpc/quorums.cpp:547-550
    pub fn core_result(&self) -> bool {
        matches!(self, Self::AlreadyRecovered | Self::Sign { .. })
    }
}

/// Storage or quorum-lookup failure while evaluating a sign request.
#[derive(Debug)]
pub enum SignError<DbError, LookupError> {
    /// The recovered-signature database operation failed.
    Database(DbError),
    /// The quorum lookup operation failed.
    Lookup(LookupError),
}

/// A signing decision or its storage/quorum-lookup error.
pub type SignResult<Q, DbError, LookupError> =
    Result<SignDecision<Q>, SignError<DbError, LookupError>>;

/// Evaluate Core's `CSigSharesManager::AsyncSignIfMember` decision.
// Core: v24.0.0 src/llmq/signing_shares.cpp:790-874 CSigSharesManager::AsyncSignIfMember
pub fn sign_if_member<K, L>(
    db: &mut RecSigDb<K>,
    lookup: &L,
    facts: &SignerFacts,
    req: &SignRequest,
    now_s: i64,
) -> SignResult<L::Quorum, K::Error, L::Error>
where
    K: RecSigKv,
    L: QuorumLookup + ?Sized,
{
    if !facts.signing_allowed {
        return Ok(SignDecision::Refuse(Reason::SigningNotAllowed));
    }

    let Some(pro_tx_hash) = facts.my_pro_tx.as_ref().filter(|hash| !hash.is_null()) else {
        return Ok(SignDecision::Refuse(Reason::NoProTx));
    };

    let quorum = match req.quorum_hash.as_ref() {
        Some(quorum_hash) if !quorum_hash.is_null() => {
            lookup.get_quorum(req.llmq_type, quorum_hash).map_err(SignError::Lookup)?
        }
        _ => {
            select_for_signing_default(lookup, req.llmq_type, &req.id).map_err(SignError::Lookup)?
        }
    };
    let Some(quorum) = quorum else {
        return Ok(SignDecision::Refuse(Reason::NoQuorum));
    };

    if !quorum.is_valid_member(pro_tx_hash) {
        return Ok(SignDecision::Refuse(Reason::NotValidMember));
    }

    let mut has_voted = db.has_voted(req.llmq_type, &req.id).map_err(SignError::Database)?;
    if has_voted {
        // Core's GetVoteForId leaves a zero uint256 when a malformed/missing value is read.
        let previous =
            db.vote_for(req.llmq_type, &req.id).map_err(SignError::Database)?.unwrap_or([0; 32]);
        if previous != req.msg_hash {
            if req.allow_diff_msg {
                has_voted = false;
            } else {
                return Ok(SignDecision::Refuse(Reason::ConflictingMessage));
            }
        } else if !req.allow_resign {
            return Ok(SignDecision::Refuse(Reason::AlreadyVoted));
        }
    }

    if db.has_for_signing(req.llmq_type, &req.id, &req.msg_hash).map_err(SignError::Database)? {
        return Ok(SignDecision::AlreadyRecovered);
    }

    if !has_voted {
        db.write_vote(req.llmq_type, &req.id, &req.msg_hash, now_s).map_err(SignError::Database)?;
    }

    Ok(SignDecision::Sign {
        quorum,
        force_reannounce: req.allow_resign,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use dash_evo::locks::wire::RecoveredSig;
    use dash_evo::types::ProTxHash;
    use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
    use dash_types::Numeric;

    use super::super::db::{RecSigBatch, RecSigOp, RecSigScan};
    use super::*;
    use crate::sigenv::fakes::{FakeLookup, FakeQuorum};

    const T: LlmqType = LlmqType::LlmqTest;
    const P: LlmqType = LlmqType::LlmqTestPlatform;
    const ID: [u8; 32] = [1; 32];
    const MSG: [u8; 32] = [2; 32];
    const OTHER: [u8; 32] = [3; 32];

    #[derive(Default)]
    struct Mem {
        rows: BTreeMap<Vec<u8>, Vec<u8>>,
        writes: usize,
        reads: Cell<usize>,
        fail_read: Option<usize>,
        fail_write: bool,
    }

    impl RecSigKv for Mem {
        type Error = DbFailure;

        fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
            let read = self.reads.get();
            self.reads.set(read + 1);
            if self.fail_read == Some(read) {
                return Err(DbFailure::Read);
            }
            Ok(self.rows.get(key).cloned())
        }

        fn scan_from(&self, start: &[u8]) -> Result<RecSigScan<'_, Self::Error>, Self::Error> {
            Ok(Box::new(
                self.rows
                    .range(start.to_vec()..)
                    .map(|(key, value)| Ok((key.clone(), value.clone()))),
            ))
        }

        fn write(&mut self, batch: RecSigBatch) -> Result<(), Self::Error> {
            if self.fail_write {
                return Err(DbFailure::Write);
            }
            for op in batch.ops {
                match op {
                    RecSigOp::Put(key, value) => {
                        self.rows.insert(key, value);
                    }
                    RecSigOp::Delete(key) => {
                        self.rows.remove(&key);
                    }
                }
            }
            self.writes += 1;
            Ok(())
        }
    }

    fn quorum(t: LlmqType) -> Arc<FakeQuorum> {
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        Arc::new(FakeQuorum {
            llmq_type: t,
            quorum_hash: [4; 32].into(),
            params: *setup.consensus.llmq(t).unwrap(),
            members: vec![ProTxHash::from([9; 32])],
            valid_members: [0].into(),
            has_verification_vector: false,
            quorum_public_key: crate::dkg::bls::BlsPkValue::invalid(),
            pk_shares: BTreeMap::new(),
            sk_share: None,
        })
    }

    fn setup(t: LlmqType) -> (RecSigDb<Mem>, FakeLookup, SignerFacts) {
        let q = quorum(t);
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((t.to_base(), q.quorum_hash), q.clone());
        lookup.selections.insert((t.to_base(), ID, -1, 8), q);
        (
            RecSigDb::new(Mem::default(), P),
            lookup,
            SignerFacts {
                my_pro_tx: Some(ProTxHash::from([9; 32])),
                signing_allowed: true,
            },
        )
    }

    fn request(t: LlmqType, allow_resign: bool, allow_diff_msg: bool) -> SignRequest {
        SignRequest {
            llmq_type: t,
            id: ID,
            msg_hash: MSG,
            quorum_hash: Some([4; 32].into()),
            allow_resign,
            allow_diff_msg,
        }
    }

    fn recovered(t: LlmqType, msg_hash: [u8; 32]) -> RecoveredSig {
        RecoveredSig {
            llmq_type: t,
            quorum_hash: [4; 32].into(),
            id: ID,
            msg_hash,
            sig: [0; 96],
        }
    }

    fn assert_sign(decision: SignDecision<FakeQuorum>, q: &Arc<FakeQuorum>, reannounce: bool) {
        match decision {
            SignDecision::Sign {
                quorum,
                force_reannounce,
            } => {
                assert_eq!(force_reannounce, reannounce);
                assert!(Arc::ptr_eq(&quorum, q));
            }
            SignDecision::Refuse(reason) => panic!("unexpected refusal: {reason:?}"),
            SignDecision::AlreadyRecovered => panic!("unexpected recovered result"),
        }
    }

    fn assert_refuse<Q: SigningQuorum>(decision: SignDecision<Q>, expected: Reason) {
        match decision {
            SignDecision::Refuse(reason) => assert_eq!(reason, expected),
            SignDecision::AlreadyRecovered => panic!("unexpected recovered result"),
            SignDecision::Sign {
                ..
            } => panic!("unexpected sign result"),
        }
    }

    #[test]
    fn vote_table_covers_all_flags_with_and_without_recovered_signature() {
        for t in [T, P] {
            for prior in [None, Some(MSG), Some(OTHER)] {
                for allow_resign in [false, true] {
                    for allow_diff_msg in [false, true] {
                        for has_recovered in [false, true] {
                            let (mut db, lookup, facts) = setup(t);
                            if let Some(msg) = prior {
                                db.write_vote(t, &ID, &msg, 1).unwrap();
                            }
                            if has_recovered {
                                db.write(&recovered(t, MSG), 2).unwrap();
                            }
                            let before = db.store().rows.clone();
                            let writes = db.store().writes;
                            let req = request(t, allow_resign, allow_diff_msg);
                            let decision =
                                sign_if_member(&mut db, &lookup, &facts, &req, 10).unwrap();
                            let refusal = match prior {
                                Some(MSG) if !allow_resign => Some(Reason::AlreadyVoted),
                                Some(OTHER) if !allow_diff_msg => Some(Reason::ConflictingMessage),
                                _ => None,
                            };
                            let should_write =
                                refusal.is_none() && !has_recovered && prior != Some(MSG);
                            if let Some(reason) = refusal {
                                assert_refuse(decision, reason);
                            } else if has_recovered {
                                assert!(matches!(decision, SignDecision::AlreadyRecovered));
                            } else {
                                assert_sign(
                                    decision,
                                    &lookup.quorums[&(t.to_base(), [4; 32].into())],
                                    allow_resign,
                                );
                            }
                            assert_eq!(db.store().writes - writes, usize::from(should_write));
                            if should_write {
                                assert_eq!(db.vote_for(t, &ID).unwrap(), Some(MSG));
                                assert!(
                                    db.store()
                                        .rows
                                        .contains_key(&super::super::keys::vote_time(10, t, &ID))
                                );
                            } else {
                                assert_eq!(db.store().rows, before);
                                assert_eq!(db.vote_for(t, &ID).unwrap(), prior);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn recovered_policy_compares_platform_message_and_other_types_by_id() {
        for t in [T, P] {
            let (mut db, lookup, facts) = setup(t);
            db.write(&recovered(t, OTHER), 1).unwrap();
            let decision =
                sign_if_member(&mut db, &lookup, &facts, &request(t, false, false), 10).unwrap();
            if t == P {
                assert_sign(decision, &lookup.quorums[&(t.to_base(), [4; 32].into())], false);
                assert_eq!(db.vote_for(t, &ID).unwrap(), Some(MSG));
            } else {
                assert!(matches!(decision, SignDecision::AlreadyRecovered));
                assert!(!db.has_voted(t, &ID).unwrap());
            }
        }
    }

    #[test]
    fn matching_platform_recovery_returns_already_recovered_without_writing() {
        let (mut db, lookup, facts) = setup(P);
        db.write(&recovered(P, MSG), 1).unwrap();
        let before = db.store().rows.clone();
        let writes = db.store().writes;
        let decision =
            sign_if_member(&mut db, &lookup, &facts, &request(P, false, false), 10).unwrap();
        assert!(matches!(decision, SignDecision::AlreadyRecovered));
        assert_eq!(db.store().rows, before);
        assert_eq!(db.store().writes, writes);
    }

    #[test]
    fn malformed_vote_is_conflicting_or_replaced() {
        for allow_diff_msg in [false, true] {
            let (mut db, lookup, facts) = setup(T);
            db.store_mut().rows.insert(super::super::keys::vote(T, &ID), vec![0; 31]);
            let before = db.store().rows.clone();
            let writes = db.store().writes;
            let decision =
                sign_if_member(&mut db, &lookup, &facts, &request(T, false, allow_diff_msg), 10)
                    .unwrap();
            if allow_diff_msg {
                assert!(matches!(decision, SignDecision::Sign { .. }));
                assert_eq!(db.vote_for(T, &ID).unwrap(), Some(MSG));
                assert_eq!(db.store().writes - writes, 1);
            } else {
                assert_refuse(decision, Reason::ConflictingMessage);
                assert_eq!(db.store().rows, before);
                assert_eq!(db.store().writes, writes);
            }
        }
    }

    #[test]
    fn recovered_storage_keeps_vote_and_refusal_precedes_recovered_check() {
        let (mut db, lookup, facts) = setup(T);
        db.write_vote(T, &ID, &MSG, 1).unwrap();
        db.write(&recovered(T, MSG), 2).unwrap();
        assert_eq!(db.vote_for(T, &ID).unwrap(), Some(MSG));
        let before = db.store().rows.clone();
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &request(T, false, true), 10).unwrap(),
            Reason::AlreadyVoted,
        );
        assert!(matches!(
            sign_if_member(&mut db, &lookup, &facts, &request(T, true, true), 10).unwrap(),
            SignDecision::AlreadyRecovered
        ));
        assert_eq!(db.store().rows, before);
    }

    #[test]
    fn refuses_without_signing_permission_identity_quorum_or_valid_membership() {
        let (mut db, mut lookup, mut facts) = setup(T);
        let req = request(T, false, false);
        facts.signing_allowed = false;
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(),
            Reason::SigningNotAllowed,
        );
        facts.signing_allowed = true;
        facts.my_pro_tx = None;
        assert_refuse(sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(), Reason::NoProTx);
        facts.my_pro_tx = Some(ProTxHash::from([8; 32]));
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(),
            Reason::NotValidMember,
        );
        facts.my_pro_tx = Some(ProTxHash::from([9; 32]));
        let mut invalid = quorum(T);
        Arc::get_mut(&mut invalid).unwrap().valid_members.clear();
        lookup.quorums.insert((T.to_base(), [4; 32].into()), invalid);
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(),
            Reason::NotValidMember,
        );
        lookup.quorums.clear();
        assert_refuse(sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(), Reason::NoQuorum);
        let no_hash = SignRequest {
            quorum_hash: None,
            ..req
        };
        lookup.selections.clear();
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &no_hash, 0).unwrap(),
            Reason::NoQuorum,
        );
        assert_eq!(db.store().writes, 0);
    }

    #[test]
    fn default_selection_uses_tip_and_offset_eight() {
        let (mut db, mut lookup, facts) = setup(T);
        let req = SignRequest {
            quorum_hash: None,
            ..request(T, false, false)
        };
        lookup.quorums.clear();
        let q = lookup.selections[&(T.to_base(), ID, -1, 8)].clone();
        assert_sign(sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(), &q, false);
    }

    #[test]
    fn null_quorum_hash_uses_default_selection() {
        let (mut db, mut lookup, facts) = setup(T);
        let req = SignRequest {
            quorum_hash: Some([0; 32].into()),
            ..request(T, false, false)
        };
        lookup.quorums.clear();
        let q = lookup.selections[&(T.to_base(), ID, -1, 8)].clone();
        assert_sign(sign_if_member(&mut db, &lookup, &facts, &req, 0).unwrap(), &q, false);
    }

    #[test]
    fn null_signer_identity_is_refused() {
        let (mut db, lookup, mut facts) = setup(T);
        facts.my_pro_tx = Some(ProTxHash::from([0; 32]));
        assert_refuse(
            sign_if_member(&mut db, &lookup, &facts, &request(T, false, false), 0).unwrap(),
            Reason::NoProTx,
        );
    }

    #[test]
    fn core_result_matches_core_boolean() {
        let (mut db, lookup, mut facts) = setup(T);
        facts.signing_allowed = false;
        let refuse =
            sign_if_member(&mut db, &lookup, &facts, &request(T, false, false), 0).unwrap();
        assert!(!refuse.core_result());
        let recovered = {
            let (mut db, lookup, facts) = setup(T);
            db.write(&recovered(T, MSG), 1).unwrap();
            sign_if_member(&mut db, &lookup, &facts, &request(T, false, false), 0).unwrap()
        };
        assert!(recovered.core_result());
        let sign = {
            let (mut db, lookup, facts) = setup(P);
            sign_if_member(&mut db, &lookup, &facts, &request(P, false, false), 0).unwrap()
        };
        assert!(sign.core_result());
    }

    #[derive(Debug, PartialEq, Eq)]
    enum DbFailure {
        Read,
        Write,
    }

    #[test]
    fn storage_errors_propagate_without_signing_or_mutation() {
        for prior in [false, true] {
            let (mut db, lookup, facts) = setup(T);
            if prior {
                db.write_vote(T, &ID, &MSG, 1).unwrap();
            }
            let rows = db.store().rows.clone();
            let read_count = if prior {
                3
            } else {
                2
            };
            for fail_read in 0..read_count {
                let kv = Mem {
                    rows: rows.clone(),
                    fail_read: Some(fail_read),
                    ..Default::default()
                };
                let mut db = RecSigDb::new(kv, P);
                assert!(matches!(
                    sign_if_member(&mut db, &lookup, &facts, &request(T, true, false), 10),
                    Err(SignError::Database(DbFailure::Read))
                ));
                assert_eq!(db.store().rows, rows);
            }
        }
        let (_, lookup, facts) = setup(T);
        let kv = Mem {
            fail_write: true,
            ..Default::default()
        };
        let mut db = RecSigDb::new(kv, P);
        assert!(matches!(
            sign_if_member(&mut db, &lookup, &facts, &request(T, false, false), 10),
            Err(SignError::Database(DbFailure::Write))
        ));
        assert!(db.store().rows.is_empty());
    }

    struct FailingLookup;

    impl QuorumLookup for FailingLookup {
        type Quorum = FakeQuorum;
        type Error = ();
        fn get_quorum(
            &self,
            _: LlmqType,
            _: &crate::dkg::wire::QuorumHash,
        ) -> Result<Option<Arc<FakeQuorum>>, ()> {
            Err(())
        }
        fn is_quorum_active(
            &self,
            _: LlmqType,
            _: &crate::dkg::wire::QuorumHash,
        ) -> Result<bool, ()> {
            panic!("sign-if-member does not check active quorums")
        }
        fn select_quorum_for_signing(
            &self,
            _: LlmqType,
            _: &[u8; 32],
            _: i32,
            _: i32,
        ) -> Result<Option<Arc<FakeQuorum>>, ()> {
            Err(())
        }
    }

    #[test]
    fn lookup_errors_propagate_before_storage_reads() {
        for quorum_hash in [None, Some([4; 32].into())] {
            let (mut db, _, facts) = setup(T);
            let req = SignRequest {
                quorum_hash,
                ..request(T, false, false)
            };
            assert!(matches!(
                sign_if_member(&mut db, &FailingLookup, &facts, &req, 0),
                Err(SignError::Lookup(()))
            ));
            assert_eq!(db.store().reads.get(), 0);
            assert_eq!(db.store().writes, 0);
        }
    }
}
