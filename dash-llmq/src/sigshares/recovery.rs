//! The sig-share manager's recovery: LQ5's threshold recovery and recovery member order.

use std::collections::BTreeMap;

use dash_evo::types::ProTxHash;
use dash_params::LlmqParams;

use crate::dkg::bls::BlsSigValue;
use crate::recovery::{TryRecover, try_recover};
use crate::sigenv::{Recovered, ShareRecovery};
use crate::topology::recovery_member_order;

/// [`ShareRecovery`] over [`try_recover`] and [`recovery_member_order`].
pub struct LlmqRecovery;

impl ShareRecovery for LlmqRecovery {
    // Core: v24.0.0 src/llmq/signing_shares.cpp:681-762 CSigSharesManager::TryRecoverSig
    fn try_recover(
        &self,
        params: &LlmqParams,
        members: &[ProTxHash],
        shares: &BTreeMap<u16, BlsSigValue>,
        counter: &mut u32,
    ) -> Recovered {
        try_recover(params, members, shares, counter).into()
    }

    // Core: v24.0.0 src/llmq/signing_shares.cpp:764-780 CSigSharesManager::GetRecoveryMemberOrder
    fn recovery_member_order(&self, members: &[ProTxHash], id: &[u8; 32]) -> Vec<usize> {
        recovery_member_order(members, id)
    }
}

impl From<TryRecover> for Recovered {
    // Core: v24.0.0 src/llmq/signing_shares.cpp:729-762 CSigSharesManager::TryRecoverSig
    fn from(outcome: TryRecover) -> Self {
        match outcome {
            TryRecover::NotEnough => Recovered::NotEnough,
            TryRecover::Failed => Recovered::Failed,
            TryRecover::Recovered {
                sig,
                self_verify,
            } => Recovered::Sig {
                sig,
                self_verify,
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use dash_evo::locks::sign_hash;
    use dash_params::{ChainParamsBuilder, LlmqType, Network, Release};
    use dash_types::Numeric;
    use serde_json::Value;

    use super::*;
    use crate::dkg::bls::{BlsSchemeKind, verify_insecure};
    use crate::dkg::wire::QuorumHash;
    use crate::recovery::tests::{
        transcript_pk, transcript_quorum, transcript_rpc, transcript_sig,
    };
    use crate::sigenv::fakes::FakeQuorum;

    /// A lane-c request: its quorum with the recorded key shares, its offline shares and its
    /// recovered signature.
    pub(crate) struct R2Request {
        pub(crate) quorum: FakeQuorum,
        pub(crate) id: [u8; 32],
        pub(crate) msg_hash: [u8; 32],
        pub(crate) shares: BTreeMap<u16, BlsSigValue>,
        pub(crate) recsig: BlsSigValue,
    }

    pub(crate) fn r2_requests() -> Vec<R2Request> {
        let bytes = include_bytes!("../../../test-vectors/lane-c/regtest-v24.0.0.json");
        let data: Value = serde_json::from_slice(bytes).expect("lane-c JSON");
        let setup = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
        let requests = data["requests"].as_array().expect("requests");
        requests
            .iter()
            .map(|request| {
                let t = u8::try_from(request["type"].as_u64().expect("type")).unwrap();
                let hash = request["quorumHash"].as_str().expect("quorum hash");
                let variant = transcript_quorum(&data, t, hash)["quorum_info"]["variants"]
                    .as_array()
                    .expect("quorum variants")
                    .iter()
                    .find(|variant| {
                        variant["members"].as_array().is_some_and(|members| {
                            members.iter().all(|member| member["pubKeyShare"].as_str().is_some())
                        })
                    })
                    .expect("quorum public key shares");
                let members = variant["members"].as_array().expect("quorum members");
                let llmq_type = LlmqType::from_base(t);
                let index = |i: usize| u16::try_from(i).unwrap();
                let threshold = request["threshold"].as_u64().expect("threshold");
                let params = LlmqParams {
                    size: index(members.len()),
                    threshold: u16::try_from(threshold).unwrap(),
                    ..*setup.consensus.llmq(llmq_type).expect("a regtest type")
                };
                let quorum = FakeQuorum {
                    llmq_type,
                    quorum_hash: QuorumHash::from(transcript_rpc(&request["quorumHash"])),
                    params,
                    members: members
                        .iter()
                        .map(|member| ProTxHash::from(transcript_rpc(&member["proTxHash"])))
                        .collect(),
                    valid_members: (0..members.len()).map(index).collect(),
                    has_verification_vector: true,
                    quorum_public_key: transcript_pk(&variant["quorumPublicKey"]),
                    pk_shares: members
                        .iter()
                        .enumerate()
                        .map(|(i, member)| (index(i), transcript_pk(&member["pubKeyShare"])))
                        .collect(),
                    sk_share: None,
                };
                let shares = request["offline_shares"]
                    .as_object()
                    .expect("recorded shares")
                    .values()
                    .map(|share| {
                        let member = share["quorumMember"].as_u64().expect("member index");
                        (u16::try_from(member).unwrap(), transcript_sig(&share["signature"]))
                    })
                    .collect();
                R2Request {
                    quorum,
                    id: transcript_rpc(&request["id"]),
                    msg_hash: transcript_rpc(&request["msgHash"]),
                    shares,
                    recsig: transcript_sig(&request["recsig"]["sig"]),
                }
            })
            .collect()
    }

    fn recover(r: &R2Request, shares: &BTreeMap<u16, BlsSigValue>, counter: &mut u32) -> Recovered {
        LlmqRecovery.try_recover(&r.quorum.params, &r.quorum.members, shares, counter)
    }

    #[test]
    fn r2_shares_recover_the_recorded_signature() {
        let requests = r2_requests();
        assert_eq!(requests.len(), 55);
        for r in &requests {
            let q = &r.quorum;
            let mut counter = 0;
            let Recovered::Sig {
                sig,
                self_verify,
            } = recover(r, &r.shares, &mut counter)
            else {
                panic!("the recorded shares recover")
            };
            assert!(self_verify);
            assert_eq!(counter, 1);
            assert_eq!(sig.to_bytes(BlsSchemeKind::Basic), r.recsig.to_bytes(BlsSchemeKind::Basic));
            let hash = sign_hash(q.llmq_type, &q.quorum_hash, &r.id, &r.msg_hash);
            assert!(verify_insecure(&sig, &q.quorum_public_key, &hash, BlsSchemeKind::Basic));
        }
    }

    #[test]
    fn self_verify_is_requested_on_recoveries_1_and_101_only() {
        let requests = r2_requests();
        let r = requests.iter().min_by_key(|r| r.quorum.params.threshold).unwrap();
        let threshold = usize::from(r.quorum.params.threshold);
        let mut counter = 0;
        let few = r.shares.iter().take(threshold - 1).map(|(i, s)| (*i, s.clone())).collect();
        assert!(matches!(recover(r, &few, &mut counter), Recovered::NotEnough));
        let mut broken = r.shares.clone();
        *broken.values_mut().next().unwrap() = BlsSigValue::invalid();
        assert!(matches!(recover(r, &broken, &mut counter), Recovered::Failed));
        assert_eq!(counter, 0);
        for n in 1..=150u32 {
            let Recovered::Sig {
                sig,
                self_verify,
            } = recover(r, &r.shares, &mut counter)
            else {
                panic!("recovery #{n}")
            };
            assert_eq!(self_verify, n == 1 || n == 101, "recovery #{n}");
            assert_eq!(sig.to_bytes(BlsSchemeKind::Basic), r.recsig.to_bytes(BlsSchemeKind::Basic));
        }
        assert_eq!(counter, 150);
    }

    #[test]
    fn member_order_is_the_topology_order() {
        for r in r2_requests() {
            let members = &r.quorum.members;
            let order = LlmqRecovery.recovery_member_order(members, &r.id);
            assert_eq!(order, recovery_member_order(members, &r.id));
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..members.len()).collect::<Vec<_>>());
        }
    }
}
