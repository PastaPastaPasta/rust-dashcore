//! Owned signing inputs for tests and replay harnesses. No network or recovery work is implicit.

use std::collections::BTreeSet;
use std::convert::Infallible;

use dash_types::Numeric;

use super::*;

/// A quorum with explicit validity, verification-vector presence and key availability.
pub struct FakeQuorum {
    pub llmq_type: LlmqType,
    pub quorum_hash: QuorumHash,
    pub params: LlmqParams,
    pub members: Vec<ProTxHash>,
    pub valid_members: BTreeSet<u16>,
    pub has_verification_vector: bool,
    pub quorum_public_key: BlsPkValue,
    pub pk_shares: BTreeMap<u16, BlsPkValue>,
    pub sk_share: Option<BlsSkValue>,
}

impl SigningQuorum for FakeQuorum {
    fn llmq_type(&self) -> LlmqType {
        self.llmq_type
    }
    fn quorum_hash(&self) -> &QuorumHash {
        &self.quorum_hash
    }
    fn params(&self) -> &LlmqParams {
        &self.params
    }
    fn members(&self) -> &[ProTxHash] {
        &self.members
    }
    // Core: v24.0.0 l/quorums.cpp:139-142 CQuorum::IsMember
    fn is_member(&self, p: &ProTxHash) -> bool {
        self.members.contains(p)
    }
    // Core: v24.0.0 l/quorums.cpp:144-153 CQuorum::IsValidMember
    fn is_valid_member(&self, p: &ProTxHash) -> bool {
        self.member_index(p).is_some_and(|i| self.valid_members.contains(&i))
    }
    // Core: v24.0.0 l/quorums.cpp:181-190 CQuorum::GetMemberIndex
    fn member_index(&self, p: &ProTxHash) -> Option<u16> {
        self.members.iter().position(|m| m == p).and_then(|i| u16::try_from(i).ok())
    }
    fn has_verification_vector(&self) -> bool {
        self.has_verification_vector
    }
    fn quorum_public_key(&self) -> &BlsPkValue {
        &self.quorum_public_key
    }
    // Core: v24.0.0 l/quorums.cpp:155-163 CQuorum::GetPubKeyShare
    fn pub_key_share(&self, member: u16) -> Option<BlsPkValue> {
        if !self.has_verification_vector
            || usize::from(member) >= self.members.len()
            || !self.valid_members.contains(&member)
        {
            return None;
        }
        self.pk_shares.get(&member).filter(|pk| pk.is_valid()).cloned()
    }
    // Core: v24.0.0 l/quorums.cpp:175-179 CQuorum::GetSkShare
    fn sk_share(&self) -> Option<BlsSkValue> {
        self.sk_share.as_ref().filter(|sk| sk.is_valid()).cloned()
    }
}

/// Keys use `LlmqType::to_base()` because `LlmqType` has no `Ord`.
/// Explicit quorum, activation and selection tables; missing entries return `None` or false.
#[derive(Default)]
pub struct FakeLookup {
    pub quorums: BTreeMap<QuorumKey, Arc<FakeQuorum>>,
    pub active: BTreeSet<QuorumKey>,
    pub selections: BTreeMap<(u8, [u8; 32], i32, i32), Arc<FakeQuorum>>,
}

impl QuorumLookup for FakeLookup {
    type Quorum = FakeQuorum;
    type Error = Infallible;
    fn get_quorum(
        &self,
        t: LlmqType,
        h: &QuorumHash,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        Ok(self.quorums.get(&(t.to_base(), *h)).cloned())
    }
    fn is_quorum_active(&self, t: LlmqType, h: &QuorumHash) -> Result<bool, Infallible> {
        Ok(self.active.contains(&(t.to_base(), *h)))
    }
    fn select_quorum_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        sign_height: i32,
        sign_offset: i32,
    ) -> Result<Option<Arc<FakeQuorum>>, Infallible> {
        Ok(self.selections.get(&(t.to_base(), *id, sign_height, sign_offset)).cloned())
    }
}

/// Recovered-signature keys, with the chain's configured Platform type injected explicitly.
#[derive(Default)]
pub struct FakeRecSigs {
    pub platform_type: Option<LlmqType>,
    pub signatures: BTreeSet<(u8, [u8; 32], [u8; 32])>,
    pub sign_hashes: BTreeSet<[u8; 32]>,
}

impl RecSigReads for FakeRecSigs {
    type Error = Infallible;
    // Core: v24.0.0 l/signing.cpp:664-668 CSigningManager::HasRecoveredSigForSigning
    fn has_for_signing(
        &self,
        t: LlmqType,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, Infallible> {
        Ok(if self.platform_type == Some(t) {
            self.signatures.contains(&(t.to_base(), *id, *msg))
        } else {
            self.signatures.iter().any(|(ty, key, _)| *ty == t.to_base() && key == id)
        })
    }
    fn has_for_session(&self, sign_hash: &[u8; 32]) -> Result<bool, Infallible> {
        Ok(self.sign_hashes.contains(sign_hash))
    }
}

/// Quorum/observer peers, fully connected peers in connman order, and bans.
#[derive(Default)]
pub struct FakeNet {
    pub quorum_peers: BTreeMap<QuorumKey, Vec<PeerId>>,
    pub connected: Vec<PeerFacts>,
    pub banned: BTreeSet<PeerId>,
}

impl SigSharesNet for FakeNet {
    fn quorum_nodes(&self, t: LlmqType, h: &QuorumHash) -> Vec<PeerId> {
        self.quorum_peers.get(&(t.to_base(), *h)).cloned().unwrap_or_default()
    }
    fn fully_connected(&self) -> Vec<PeerFacts> {
        self.connected.clone()
    }
    fn is_banned(&self, p: PeerId) -> bool {
        self.banned.contains(&p)
    }
}

/// A deterministic operator signer; no key or an invalid key refuses signing.
#[derive(Default)]
pub struct FakeOperatorSigner {
    pub key: Option<BlsSkValue>,
}

impl OperatorSigner for FakeOperatorSigner {
    // Core: v24.0.0 l/signing_shares.cpp:1578-1581 CSigSharesManager::CreateSigShareForSingleMember
    fn sign(&self, hash: &[u8; 32], scheme: BlsSchemeKind) -> Option<BlsSigValue> {
        let key = self.key.as_ref().filter(|key| key.is_valid())?;
        let sig = crate::dkg::bls::sign(key, hash, scheme == BlsSchemeKind::Legacy);
        sig.is_valid().then_some(sig)
    }
}

/// Inject both recovery and recovery-member ordering, including any counter updates.
pub struct FakeRecovery<F, G> {
    pub recover: F,
    pub order: G,
}

impl<F, G> ShareRecovery for FakeRecovery<F, G>
where
    F: Fn(&LlmqParams, &[ProTxHash], &BTreeMap<u16, BlsSigValue>, &mut u32) -> Recovered,
    G: Fn(&[ProTxHash], &[u8; 32]) -> Vec<usize>,
{
    fn try_recover(
        &self,
        params: &LlmqParams,
        members: &[ProTxHash],
        shares: &BTreeMap<u16, BlsSigValue>,
        counter: &mut u32,
    ) -> Recovered {
        (self.recover)(params, members, shares, counter)
    }
    fn recovery_member_order(&self, members: &[ProTxHash], id: &[u8; 32]) -> Vec<usize> {
        (self.order)(members, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dkg::bls::{secret_to_public, verify_insecure};
    use dash_params::{ChainParamsBuilder, Network, Release};

    const TEST: LlmqType = LlmqType::LlmqTest;

    fn quorum() -> FakeQuorum {
        let mut bytes = [0; 32];
        bytes[31] = 7;
        let sk = BlsSkValue::from_canonical(&bytes, BlsSchemeKind::Basic).unwrap();
        let pk = secret_to_public(&sk);
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        FakeQuorum {
            llmq_type: TEST,
            quorum_hash: QuorumHash::from([8; 32]),
            params: *setup.consensus.llmq(TEST).unwrap(),
            members: (1..=3).map(|i| ProTxHash::from([i; 32])).collect(),
            valid_members: [0, 2].into(),
            has_verification_vector: true,
            quorum_public_key: pk.clone(),
            pk_shares: [(0, pk), (2, BlsPkValue::invalid())].into(),
            sk_share: Some(sk),
        }
    }

    #[test]
    fn spork_raw_value_matrix_keeps_global_activation_separate() {
        for (raw, enabled) in [
            (0, [true, true]),
            (1, [false, true]),
            (2, [false, false]),
            (4070908800, [false, false]),
        ] {
            for active in [false, true] {
                let sporks = SporkFacts {
                    spork21_active: active,
                    spork21_value: raw,
                };
                for (t, expected) in [LlmqType::Llmq400_60, TEST].into_iter().zip(enabled) {
                    assert_eq!(sporks.all_members_connected(t), expected, "{raw}, {t:?}");
                }
                assert_eq!(sporks.spork21_active, active);
            }
        }
    }

    #[test]
    fn default_selection_uses_tip_and_offset_eight() {
        let q = Arc::new(quorum());
        let mut lookup = FakeLookup::default();
        lookup.selections.insert((TEST.to_base(), [4; 32], -1, 8), q.clone());
        assert!(Arc::ptr_eq(
            &select_for_signing_default(&lookup, TEST, &[4; 32]).unwrap().unwrap(),
            &q
        ));
        assert!(lookup.select_quorum_for_signing(TEST, &[4; 32], -1, 0).unwrap().is_none());
        assert!(lookup.select_quorum_for_signing(TEST, &[4; 32], 0, 8).unwrap().is_none());
        assert!(select_for_signing_default(&lookup, TEST, &[5; 32]).unwrap().is_none());
    }

    #[test]
    fn fake_quorum_and_lookup_round_trip_and_reject_unavailable_keys() {
        let mut q = quorum();
        assert_eq!(q.llmq_type(), TEST);
        assert_eq!(q.params().size, 3);
        assert_eq!(q.members(), q.members.as_slice());
        assert!(q.is_member(&q.members[1]));
        assert!(!q.is_valid_member(&q.members[1]));
        assert_eq!(q.member_index(&q.members[2]), Some(2));
        assert!(!q.is_member(&ProTxHash::from([9; 32])));
        assert!(!q.is_valid_member(&ProTxHash::from([9; 32])));
        assert_eq!(q.member_index(&ProTxHash::from([9; 32])), None);
        assert_eq!(q.pub_key_share(0).as_ref(), Some(q.quorum_public_key()));
        assert!(q.pub_key_share(1).is_none());
        assert!(q.pub_key_share(2).is_none());
        assert!(q.pub_key_share(3).is_none());
        q.pk_shares.clear();
        assert!(q.pub_key_share(0).is_none());
        q.has_verification_vector = false;
        assert!(!q.has_verification_vector());
        q.pk_shares.insert(0, q.quorum_public_key.clone());
        assert!(q.pub_key_share(0).is_none());
        assert!(q.sk_share().unwrap().is_valid());
        q.sk_share = Some(BlsSkValue::invalid());
        assert!(q.sk_share().is_none());
        q.sk_share = None;
        assert!(q.sk_share().is_none());
        let q = Arc::new(q);
        let hash = *q.quorum_hash();
        let mut lookup = FakeLookup::default();
        lookup.quorums.insert((TEST.to_base(), hash), q.clone());
        lookup.active.insert((TEST.to_base(), hash));
        assert!(Arc::ptr_eq(&lookup.get_quorum(TEST, &hash).unwrap().unwrap(), &q));
        assert!(lookup.is_quorum_active(TEST, &hash).unwrap());
        assert!(lookup.get_quorum(TEST, &QuorumHash::from([9; 32])).unwrap().is_none());
        assert!(!lookup.is_quorum_active(LlmqType::Llmq400_60, &hash).unwrap());
    }

    #[test]
    fn fake_recsigs_use_the_configured_platform_type() {
        let platform = LlmqType::LlmqTestPlatform;
        let mut recsigs = FakeRecSigs {
            platform_type: Some(platform),
            ..Default::default()
        };
        for t in [TEST, platform] {
            recsigs.signatures.insert((t.to_base(), [1; 32], [2; 32]));
        }
        recsigs.sign_hashes.insert([3; 32]);
        assert!(recsigs.has_for_signing(TEST, &[1; 32], &[9; 32]).unwrap());
        assert!(recsigs.has_for_signing(platform, &[1; 32], &[2; 32]).unwrap());
        assert!(!recsigs.has_for_signing(platform, &[1; 32], &[9; 32]).unwrap());
        assert!(!recsigs.has_for_signing(TEST, &[9; 32], &[2; 32]).unwrap());
        assert!(!recsigs.has_for_signing(LlmqType::Llmq400_60, &[1; 32], &[2; 32]).unwrap());
        assert!(recsigs.has_for_session(&[3; 32]).unwrap());
        assert!(!recsigs.has_for_session(&[4; 32]).unwrap());
        recsigs.signatures.insert((platform.to_base(), [1; 32], [9; 32]));
        assert!(recsigs.has_for_signing(platform, &[1; 32], &[9; 32]).unwrap());
    }

    #[test]
    fn fake_net_preserves_connection_order_and_quorum_peer_sets() {
        let hash = QuorumHash::from([8; 32]);
        let net = FakeNet {
            quorum_peers: [((TEST.to_base(), hash), vec![9, 2])].into(),
            connected: vec![
                PeerFacts {
                    id: 2,
                    verified_pro_tx: None,
                    common_version: 70242,
                },
                PeerFacts {
                    id: 9,
                    verified_pro_tx: Some(ProTxHash::from([1; 32])),
                    common_version: 70243,
                },
            ],
            banned: [9].into(),
        };
        assert_eq!(net.quorum_nodes(TEST, &hash), vec![9, 2]);
        assert!(net.quorum_nodes(LlmqType::Llmq400_60, &hash).is_empty());
        assert_eq!(net.fully_connected(), net.connected);
        assert!(net.is_banned(9));
        assert!(!net.is_banned(2));
    }

    #[test]
    fn fake_operator_signer_round_trips_both_schemes_and_refuses_without_key() {
        let key = quorum().sk_share;
        let pk = secret_to_public(key.as_ref().unwrap());
        let op = FakeOperatorSigner {
            key,
        };
        for scheme in [BlsSchemeKind::Basic, BlsSchemeKind::Legacy] {
            let sig = op.sign(&[6; 32], scheme).unwrap();
            assert!(verify_insecure(&sig, &pk, &[6; 32], scheme));
        }
        assert!(FakeOperatorSigner::default().sign(&[6; 32], BlsSchemeKind::Basic).is_none());
        let invalid = FakeOperatorSigner {
            key: Some(BlsSkValue::invalid()),
        };
        assert!(invalid.sign(&[6; 32], BlsSchemeKind::Basic).is_none());
    }

    #[test]
    fn fake_recovery_forwards_inputs_and_exclusively_owns_counter_updates() {
        let q = quorum();
        let sig = FakeOperatorSigner {
            key: q.sk_share(),
        }
        .sign(&[4; 32], BlsSchemeKind::Basic)
        .unwrap();
        let shares = [(0, sig.clone())].into();
        let recovery = FakeRecovery {
            recover: |params: &LlmqParams,
                      members: &[ProTxHash],
                      given: &BTreeMap<u16, BlsSigValue>,
                      counter: &mut u32| {
                assert_eq!(params, &q.params);
                assert_eq!(members, q.members);
                assert_eq!(given, &shares);
                assert_eq!(*counter, 41);
                *counter += 1;
                Recovered::Sig {
                    sig: given[&0].clone(),
                    self_verify: true,
                }
            },
            order: |members: &[ProTxHash], id: &[u8; 32]| {
                assert_eq!(members, q.members);
                assert_eq!(id, &[4; 32]);
                vec![2, 0, 1]
            },
        };
        let mut counter = 41;
        let Recovered::Sig {
            sig: got,
            self_verify,
        } = recovery.try_recover(&q.params, &q.members, &shares, &mut counter)
        else {
            panic!("injected recovery")
        };
        assert_eq!(got, sig);
        assert!(self_verify);
        assert_eq!(counter, 42);
        assert_eq!(recovery.recovery_member_order(&q.members, &[4; 32]), vec![2, 0, 1]);
    }

    #[test]
    fn share_batch_keeps_different_types_at_the_same_quorum_hash() {
        let first = Arc::new(quorum());
        let mut second = quorum();
        second.llmq_type = LlmqType::LlmqTestPlatform;
        second.params = *ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
            .build()
            .unwrap()
            .consensus
            .llmq(second.llmq_type)
            .unwrap();
        let second = Arc::new(second);
        let hash = *first.quorum_hash();
        assert_eq!(second.quorum_hash(), &hash);
        let batch: ShareBatch<FakeQuorum> = ShareBatch {
            by_source: [(
                7,
                [
                    SigShare::new(TEST, hash, 0, [1; 32].into(), [2; 32].into(), [0; 96].into()),
                    SigShare::new(
                        second.llmq_type(),
                        hash,
                        0,
                        [1; 32].into(),
                        [2; 32].into(),
                        [0; 96].into(),
                    ),
                ]
                .into_iter()
                .map(|share| CapturedShare {
                    share,
                    read: BlsCtx::BASIC,
                })
                .collect(),
            )]
            .into(),
            quorums: [
                ((TEST.to_base(), hash), first.clone()),
                ((second.llmq_type().to_base(), hash), second.clone()),
            ]
            .into(),
        };
        assert_eq!(batch.quorums.len(), 2);
        assert!(Arc::ptr_eq(&batch.quorums[&(TEST.to_base(), hash)], &first));
        assert!(Arc::ptr_eq(&batch.quorums[&(second.llmq_type().to_base(), hash)], &second));
    }

    #[test]
    fn now_seconds_use_euclidean_division() {
        for (ms, secs) in [(-1001, -2), (-1, -1), (0, 0), (999, 0), (1000, 1)] {
            let now = Now {
                ms,
            };
            assert_eq!(now.secs(), secs);
        }
    }
}
