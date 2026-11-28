//! The signing commands' calls into the node's share manager and quorum selection.

use std::fmt::Display;
use std::sync::{Arc, Weak};

use dash_llmq::dkg::wire::SigShare;
use dash_llmq::sigenv::{
    QuorumLookup, RecSigReads, ShareRecovery, SigSharesNet, SignRequest, SigningQuorum as _,
    quorum_verifies, recovered_sig_readable, select_for_signing_default,
};
use dash_llmq::signing::api::{QuorumSigning, RecoveredSigIntake, SigningFault};
use dash_llmq::topology::select_member_for_recovery;
use dash_params::{ChainSetup, LlmqType};
use dash_primitives::QuorumHash;
use dash_types::Numeric as _;
use dashd_quorums::sigshares::{SharesFacts, SharesOut, SigSharesRuntime};
use dashd_rpc::evo::{
    EvoError, NodeQuorumSign, OwnSigShare, SelectedQuorum, SignArgs, VerifyArgs, VerifyAt,
};

use crate::rpc::ChainSlot;

/// `CSigSharesManager::CreateSigShare` over the quorum named or selected; the error is a
/// lookup's.
pub(crate) trait ShareSigner: Send + Sync {
    fn create(
        &self,
        t: LlmqType,
        id: [u8; 32],
        msg: [u8; 32],
        quorum: Option<QuorumHash>,
    ) -> Result<Option<Option<SigShare>>, String>;
}

impl<L, I, N, X, F, S> ShareSigner for SigSharesRuntime<L, I, N, X, F, S>
where
    L: QuorumLookup<Error: Display> + Send + Sync,
    I: RecoveredSigIntake + RecSigReads<Error = SigningFault> + Send + Sync,
    N: SigSharesNet + Send + Sync,
    X: SharesOut + Send + Sync,
    F: SharesFacts + Send + Sync,
    S: ShareRecovery + Send + Sync,
    L::Quorum: Send + Sync,
{
    fn create(
        &self,
        t: LlmqType,
        id: [u8; 32],
        msg: [u8; 32],
        quorum: Option<QuorumHash>,
    ) -> Result<Option<Option<SigShare>>, String> {
        self.create_sig_share(t, id, msg, quorum).map_err(|e| e.to_string())
    }
}

/// `SelectQuorumForSigning` at the tip, with the selected quorum's recovery members.
pub(crate) trait QuorumSelect: Send + Sync {
    fn select(&self, t: LlmqType, id: &[u8; 32]) -> Result<Option<SelectedQuorum>, String>;
    /// `Ok(None)`: the quorum named was not found.
    fn verify(
        &self,
        a: &VerifyArgs,
        legacy: bool,
        dkg_interval: i32,
    ) -> Result<Option<bool>, String>;
}

// Core: v24.0.0 src/rpc/quorums.cpp:851-866; src/llmq/signing_shares.cpp:782-788
impl<L: QuorumLookup<Error: Display> + Send + Sync> QuorumSelect for L {
    fn select(&self, t: LlmqType, id: &[u8; 32]) -> Result<Option<SelectedQuorum>, String> {
        let Some(q) = select_for_signing_default(self, t, id).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        let (members, count) = (q.members(), u32::from(q.params().recovery_members));
        let recovery_members = (0..count)
            .filter_map(|i| select_member_for_recovery(members, id, i, count))
            .map(|i| *members[i].as_bytes())
            .collect();
        Ok(Some(SelectedQuorum {
            quorum_hash: (*q.quorum_hash()).into(),
            recovery_members,
        }))
    }

    // Core: v24.0.0 src/rpc/quorums.cpp:652-662 VerifyRecoveredSigLatestQuorums, :710-718
    fn verify(
        &self,
        a: &VerifyArgs,
        legacy: bool,
        dkg_interval: i32,
    ) -> Result<Option<bool>, String> {
        let t = LlmqType::from_base(a.llmq_type);
        let verifies = |q: &L::Quorum| quorum_verifies(q, &a.id, &a.msg_hash, &a.signature, legacy);
        let height = match a.at {
            VerifyAt::Quorum(h) => {
                let quorum = self.get_quorum(t, &h.into()).map_err(|e| e.to_string())?;
                return Ok(quorum.map(|q| verifies(&q)));
            }
            VerifyAt::Height(height) => height,
        };
        for offset in [0, dkg_interval] {
            let q = self.select_quorum_for_signing(t, &a.id, height, offset);
            if q.map_err(|e| e.to_string())?.is_some_and(|q| verifies(&q)) {
                return Ok(Some(true));
            }
        }
        Ok(Some(false))
    }
}

/// The runtime, its quorum lookup and, on an active masternode, the share manager, held weakly:
/// the LLMQ context's reset at shutdown drops them, and the commands then find no context.
pub(crate) struct RpcSign {
    runtime: Weak<dyn QuorumSigning>,
    lookup: Weak<dyn QuorumSelect>,
    shares: Option<Weak<dyn ShareSigner>>,
    chain: Arc<ChainSetup>,
    /// `bls::bls_legacy_scheme`: V's published scheme.
    legacy: Box<dyn Fn() -> bool + Send + Sync>,
}

impl RpcSign {
    pub(crate) fn new(
        chain: Arc<ChainSetup>,
        legacy: Box<dyn Fn() -> bool + Send + Sync>,
        runtime: Arc<dyn QuorumSigning>,
        lookup: Arc<dyn QuorumSelect>,
        shares: Option<Arc<dyn ShareSigner>>,
    ) -> Self {
        Self {
            runtime: Arc::downgrade(&runtime),
            lookup: Arc::downgrade(&lookup),
            shares: shares.as_ref().map(Arc::downgrade),
            chain,
            legacy,
        }
    }
}

impl ChainSlot {
    /// Step 7c's signing runtime and share manager.
    pub(crate) fn set_sign(&self, sign: RpcSign) {
        let _ = self.sign.set(sign);
    }

    // Core: v24.0.0 src/rpc/server_util.cpp:113-119 EnsureLLMQContext
    fn sign_part<P: ?Sized>(
        &self,
        part: impl FnOnce(&RpcSign) -> Option<&Weak<P>>,
    ) -> Result<Arc<P>, EvoError> {
        let sign = self.sign.get().ok_or(EvoError::LlmqUnavailable)?;
        part(sign).and_then(Weak::upgrade).ok_or(EvoError::LlmqUnavailable)
    }
}

impl NodeQuorumSign for ChainSlot {
    fn masternode_mode(&self) -> bool {
        self.sign.get().is_some_and(|s| s.shares.is_some())
    }

    fn llmq_context(&self) -> Result<(), EvoError> {
        self.sign_part(|s| Some(&s.runtime)).map(drop)
    }

    fn platform_type(&self) -> u8 {
        self.sign.get().map_or(0, |s| s.chain.consensus.llmq_type_platform.to_base())
    }

    // Core: v24.0.0 src/rpc/quorums.cpp:546-551
    fn async_sign_if_member(&self, a: SignArgs, allow_diff_msg: bool) -> Result<bool, EvoError> {
        let req = SignRequest {
            llmq_type: LlmqType::from_base(a.llmq_type),
            id: a.id,
            msg_hash: a.msg_hash,
            quorum_hash: a.quorum_hash.map(QuorumHash::from),
            allow_resign: false,
            allow_diff_msg,
        };
        let runtime = self.sign_part(|s| Some(&s.runtime))?;
        runtime.async_sign_if_member(&req).map_err(|fault| match fault {
            SigningFault::Stopped => EvoError::LlmqUnavailable,
            fault => EvoError::Failed(fault.to_string()),
        })
    }

    // Core: v24.0.0 src/rpc/quorums.cpp:552-582
    fn create_sig_share(&self, a: SignArgs) -> Result<Option<Option<OwnSigShare>>, EvoError> {
        let shares = self.sign_part(|s| s.shares.as_ref())?;
        let t = LlmqType::from_base(a.llmq_type);
        let share = shares.create(t, a.id, a.msg_hash, a.quorum_hash.map(QuorumHash::from));
        Ok(share.map_err(EvoError::Failed)?.map(|share| {
            share.map(|s| OwnSigShare {
                quorum_hash: s.quorum_hash().into(),
                member: s.quorum_member(),
                sign_hash: s.sign_hash().into(),
                signature: s.sig_share().to_bytes().to_vec(),
            })
        }))
    }

    fn select_quorum(&self, t: u8, id: &[u8; 32]) -> Result<Option<SelectedQuorum>, EvoError> {
        let lookup = self.sign_part(|s| Some(&s.lookup))?;
        lookup.select(LlmqType::from_base(t), id).map_err(EvoError::Failed)
    }

    fn signature_readable(&self, signature: &[u8; 96]) -> bool {
        self.sign.get().is_some_and(|s| recovered_sig_readable(signature, (s.legacy)()))
    }

    fn verify(&self, a: VerifyArgs) -> Result<Option<bool>, EvoError> {
        let lookup = self.sign_part(|s| Some(&s.lookup))?;
        let sign = self.sign.get().ok_or(EvoError::LlmqUnavailable)?;
        let params = sign.chain.consensus.llmq(LlmqType::from_base(a.llmq_type));
        let dkg_interval = params.map_or(0, |p| i32::from(p.dkg_interval));
        lookup.verify(&a, (sign.legacy)(), dkg_interval).map_err(EvoError::Failed)
    }
}
