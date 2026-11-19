//! The reference [`FinalVerifier`]: `dash_evo::llmq::verify_commitment` with the signatures
//! checked synchronously (Core's `CFinalCommitment::Verify(…, checkSigs = true)`), at a base
//! block built from the session's facts, over a member source that counts its reads.

use std::cell::Cell;
use std::convert::Infallible;
use std::sync::Arc;

use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::primitives::BlockHash;
use dash_consensus::{BlockId, HeaderCtx};
use dash_evo::effect::Effect;
use dash_evo::llmq::{
    CommitmentError, LlmqChain, LlmqCommitment, LlmqCtx, MemberError, QuorumMemberSource,
    QuorumMembers, SigPath, verify_commitment,
};
use dash_num::{Arith256, CompactTarget};
use dash_params::{BuriedDeployment, ChainParamsBuilder, ChainSetup, LlmqParams, Network, Release};
use dash_types::Numeric;

use super::TestMember;
use crate::dkg::bls::BlsCtx;
use crate::dkg::session::FinalVerifier;
use crate::dkg::wire::{LlmqType, QuorumHash};

/// A chain of one block per height up to the base: the block at the base's height has the
/// quorum hash the call names, every other block a synthetic one. `BlockId(h)` is height `h`.
struct BaseChain {
    height: u32,
    hash: BlockHash,
}

impl LlmqChain for BaseChain {
    type Error = Infallible;

    fn header(&self, id: BlockId) -> Result<HeaderCtx, Infallible> {
        let hash = if id.0 == self.height {
            self.hash
        } else {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&id.0.to_le_bytes());
            bytes[31] = 0xe0;
            BlockHash::from(bytes)
        };
        Ok(HeaderCtx {
            height: id.0,
            hash,
            time: 0,
            bits: CompactTarget::new(0),
            version: 0,
            mtp: 0,
            chain_work: Arith256::ZERO,
        })
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Result<Option<BlockId>, Infallible> {
        Ok((height <= id.0).then_some(BlockId(height)))
    }

    fn prev(&self, id: BlockId) -> Result<Option<BlockId>, Infallible> {
        Ok(id.0.checked_sub(1).map(BlockId))
    }
}

/// V19 and DIP0024 as the session sees them at its base, at every block.
struct Flags {
    v19: bool,
    dip24: bool,
}

impl DeploymentView for Flags {
    fn active_after(&self, _: Option<BlockId>, d: Deployment) -> bool {
        match d {
            Deployment::Buried(BuriedDeployment::V19) => self.v19,
            Deployment::Buried(BuriedDeployment::Dip0024) => self.dip24,
            _ => false,
        }
    }

    fn active_at(&self, _: BlockId, d: Deployment) -> bool {
        self.active_after(None, d)
    }
}

/// `GetAllQuorumMembers`: the same members at every base, counting the reads.
struct Counting<'a> {
    members: &'a QuorumMembers<TestMember>,
    reads: Cell<usize>,
}

impl QuorumMemberSource for Counting<'_> {
    type Entry = TestMember;
    type Error = Infallible;

    fn members(
        &self,
        _: LlmqType,
        _: BlockId,
        _: &mut dyn dash_evo::EffectSink,
    ) -> Result<QuorumMembers<TestMember>, MemberError<Infallible>> {
        self.reads.set(self.reads.get() + 1);
        Ok(self.members.clone())
    }
}

/// One [`FinalVerifier::verify`] call.
#[derive(Clone, Debug)]
pub struct VerifyCall {
    /// The commitment.
    pub qc: LlmqCommitment,
    /// The members reads it made (LQ4 §3.4.1 rows 5-6): none when a check before
    /// `l/commitment.cpp:177` refuses it, one when it stops before the signature checks, two
    /// when it reaches them.
    pub member_reads: usize,
    /// `verify_commitment`'s answer.
    pub verdict: Result<(), CommitmentError<Infallible>>,
}

/// The chain parameters [`ReferenceVerifier`] verifies a `t` commitment under: regtest's where
/// regtest runs `t`, else devnet's where devnet does, else regtest's, which refuse it.
fn setup_for(t: LlmqType) -> ChainSetup {
    let build = |network| ChainParamsBuilder::new(network, Release::V24_0).build();
    [Network::Regtest, Network::Devnet]
        .into_iter()
        .map(|network| build(network).expect("the network's parameters"))
        .find(|setup| setup.consensus.llmq(t).is_some())
        .unwrap_or_else(|| build(Network::Regtest).expect("regtest"))
}

/// The reference [`FinalVerifier`] for one session: the type's chain parameters (regtest's or
/// devnet's) with the session's type set to its effective parameters, a base at `height` whose
/// hash is the one each call names, V19 and DIP0024 as given, and `members` at every base. A
/// type neither network runs is refused at every call (`CommitmentReject::LlmqType`).
#[derive(Clone)]
pub struct ReferenceVerifier {
    setup: ChainSetup,
    /// The base's height.
    pub height: u32,
    /// `DeploymentActiveAfter(base, V19)`.
    pub v19_after_base: bool,
    /// DIP0024 active, so a rotated type is rotated.
    pub dip24: bool,
    /// The scheme the call runs under, Core's process-wide one.
    pub bls: BlsCtx,
    /// `GetAllQuorumMembers` at the base.
    pub members: QuorumMembers<TestMember>,
    /// Every call, in order.
    pub calls: Vec<VerifyCall>,
}

impl ReferenceVerifier {
    /// The verifier for a `t` session under `params` with `members`, its base at `height`: V19
    /// active, DIP0024 inactive, the basic scheme.
    ///
    /// # Panics
    ///
    /// When the chain parameters do not build; a call panics when they are outside `LlmqCtx`'s
    /// domain.
    pub fn new(t: LlmqType, params: &LlmqParams, height: u32, members: &[TestMember]) -> Self {
        let mut setup = setup_for(t);
        if let Some(p) = setup.consensus.llmq_mut(t) {
            *p = *params;
        }
        Self {
            setup,
            height,
            v19_after_base: true,
            dip24: false,
            bls: BlsCtx::BASIC,
            members: Arc::from(members),
            calls: Vec::new(),
        }
    }

    /// The members reads of every call, in order.
    pub fn member_reads(&self) -> Vec<usize> {
        self.calls.iter().map(|c| c.member_reads).collect()
    }
}

impl FinalVerifier for ReferenceVerifier {
    fn verify(&mut self, base: &QuorumHash, qc: &LlmqCommitment) -> bool {
        let chain = BaseChain {
            height: self.height,
            hash: BlockHash::from(*base.as_bytes()),
        };
        let flags = Flags {
            v19: self.v19_after_base,
            dip24: self.dip24,
        };
        let source = Counting {
            members: &self.members,
            reads: Cell::new(0),
        };
        let ctx = LlmqCtx::new(&self.setup, &chain, &flags, self.bls)
            .expect("the parameters are in the domain");
        let mut effects: Vec<Effect> = Vec::new();
        let verdict = verify_commitment(
            &ctx,
            &source,
            qc,
            BlockId(self.height),
            Some(SigPath::Sync),
            &mut effects,
        );
        let accepted = verdict.is_ok();
        self.calls.push(VerifyCall {
            qc: qc.clone(),
            member_reads: source.reads.get(),
            verdict,
        });
        accepted
    }
}
