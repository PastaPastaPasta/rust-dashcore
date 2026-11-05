//! Type 6, Core's `CheckLLMQCommitment` (EV1 §3.11), with the `CommitmentVerifier` seam of EV1 D5
//! (LQ2 D3): EV1-5 dispatches a quorum-commitment transaction here from its check loop.

use dash_consensus::primitives::Tx;
use dash_consensus::{BlockId, TxValidationError, TxValidationResult};
use dash_evo::effect::EffectSink;
use dash_evo::llmq::{
    CommitmentError, LlmqCommitment, LlmqCtx, MemberError, verify_commitment, verify_null,
};
use dash_primitives::SpecialPayload;

use super::reasons::{
    BAD_QC_COMMITMENT_TYPE, BAD_QC_HEIGHT, BAD_QC_INVALID, BAD_QC_INVALID_NULL, BAD_QC_PAYLOAD,
    BAD_QC_QUORUM_HASH, BAD_QC_VERSION,
};
use crate::blockprocessor::{BlockLookup, QcBlockError, as_block_hash};
use crate::build::payload;
use crate::store::{DmlMemberSource, StoreError};

/// `CFinalCommitmentTxPayload::CURRENT_VERSION`.
// Core: llmq/commitment.h CFinalCommitmentTxPayload::CURRENT_VERSION
pub const QC_TX_CURRENT_VERSION: u16 = 1;

/// EV1 D5's seam: the two commitment checks `CheckLLMQCommitment` makes after its own.
pub trait CommitmentVerifier {
    /// `qc.VerifyNull()`.
    fn verify_null(&self, qc: &LlmqCommitment) -> bool;

    /// `qc.Verify(base, checkSigs = false)`: `Ok(true)` where Core returns true. The members
    /// it reads may record effects. An error is a fault, never a verdict.
    fn verify(
        &self,
        qc: &LlmqCommitment,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError>;
}

/// The node's verifier: SB8's `Verify` over the members facade, in the scheme of the context.
pub struct NodeCommitmentVerifier<'a> {
    /// The chain, deployments and the global scheme of the check.
    pub llmq: LlmqCtx<'a, StoreError>,
    /// The members facade.
    pub members: &'a DmlMemberSource<'a>,
}

impl CommitmentVerifier for NodeCommitmentVerifier<'_> {
    fn verify_null(&self, qc: &LlmqCommitment) -> bool {
        verify_null(self.llmq.params(), qc)
    }

    fn verify(
        &self,
        qc: &LlmqCommitment,
        base: BlockId,
        effects: &mut dyn EffectSink,
    ) -> Result<bool, QcBlockError> {
        match verify_commitment(&self.llmq, self.members, qc, base, None, effects) {
            Ok(()) => Ok(true),
            Err(CommitmentError::Rejected(_)) => Ok(false),
            // No signature is checked.
            Err(CommitmentError::BlsUnsupported) => Err(QcBlockError::BlsUnsupported),
            Err(CommitmentError::Members(e)) => Err(e.into()),
        }
    }
}

fn reject(reason: &'static str) -> Result<(), TxValidationError> {
    Err(TxValidationError::new(TxValidationResult::Consensus, reason))
}

/// Core's `CheckLLMQCommitment(util_params, tx, state)` for a type-6 transaction checked on top of
/// `prev` (Core's `pindexPrev`, `util_params.m_base_index`). The outer error is a fault; the inner
/// one Core's verdict, all `TX_CONSENSUS`, in Core's order:
/// 1. the payload does not decode (`bad-qc-payload`);
/// 2. the type is not configured (`bad-qc-commitment-type`);
/// 3. the payload version is 0 or above [`QC_TX_CURRENT_VERSION`] (`bad-qc-version`);
/// 4. the payload height is not `prev`'s plus one (`bad-qc-height`);
/// 5. the quorum hash names no known block, or one off `prev`'s chain (`bad-qc-quorum-hash`);
/// 6. a null commitment: `VerifyNull`, else `bad-qc-invalid-null`;
/// 7. `Verify(base, checkSigs = false)`, else `bad-qc-invalid`.
// Core: llmq/commitment.cpp:240 CheckLLMQCommitment
pub fn check_llmq_commitment(
    tx: &Tx,
    prev: BlockId,
    llmq: &LlmqCtx<'_, StoreError>,
    index: &dyn BlockLookup,
    verifier: &dyn CommitmentVerifier,
    effects: &mut dyn EffectSink,
) -> Result<Result<(), TxValidationError>, QcBlockError> {
    let Some(SpecialPayload::QuorumCommitment(qc_tx)) = payload(tx) else {
        return Ok(reject(BAD_QC_PAYLOAD));
    };
    let qc = &qc_tx.commitment;
    if llmq.params().consensus.llmq(qc.llmq_type).is_none() {
        return Ok(reject(BAD_QC_COMMITMENT_TYPE));
    }
    if qc_tx.version == 0 || qc_tx.version > QC_TX_CURRENT_VERSION {
        return Ok(reject(BAD_QC_VERSION));
    }
    let view = llmq.chain();
    let prev_height = view.header(prev).map_err(MemberError::View)?.height;
    // `uint32_t(pindexPrev->nHeight + 1)`.
    if qc_tx.height.to_u32() != prev_height.wrapping_add(1) {
        return Ok(reject(BAD_QC_HEIGHT));
    }
    let Some((base, base_height)) = index.lookup(&as_block_hash(&qc.quorum_hash)) else {
        return Ok(reject(BAD_QC_QUORUM_HASH));
    };
    if view.ancestor(prev, base_height).map_err(MemberError::View)? != Some(base) {
        return Ok(reject(BAD_QC_QUORUM_HASH));
    }
    if qc.is_null() {
        return Ok(if verifier.verify_null(qc) {
            Ok(())
        } else {
            reject(BAD_QC_INVALID_NULL)
        });
    }
    Ok(if verifier.verify(qc, base, effects)? {
        Ok(())
    } else {
        reject(BAD_QC_INVALID)
    })
}
