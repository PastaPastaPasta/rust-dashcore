//! Type 6, Core's `CheckLLMQCommitment` (EV1 §3.11), with the `CommitmentVerifier` seam of EV1 D5
//! (LQ2 D3): EV1-5 dispatches a quorum-commitment transaction here from its check loop.

use dash_consensus::primitives::Tx;
use dash_consensus::{BlockId, TxValidationError, TxValidationResult};
use dash_evo::effect::EffectSink;
use dash_evo::llmq::{
    CommitmentError, CommitmentReject, LlmqCommitment, LlmqCtx, MemberError,
    verify_commitment_counted, verify_null,
};
use dash_params::LlmqParams;
use dash_primitives::SpecialPayload;
use dash_types::Numeric;

use super::reasons::{
    BAD_QC_COMMITMENT_TYPE, BAD_QC_HEIGHT, BAD_QC_INVALID, BAD_QC_INVALID_NULL, BAD_QC_PAYLOAD,
    BAD_QC_QUORUM_HASH, BAD_QC_VERSION,
};
use crate::blockprocessor::{BlockLookup, QcBlockError, as_block_hash};
use crate::build::payload;
use crate::store::{DmlMemberSource, StoreError};
use crate::trace::{CheckReject, NoQcTrace, QcEvent, QcTrace};

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

    /// Where `CheckLLMQCommitment` reports its lines (LQ2 D26); none by default.
    fn trace(&self) -> &dyn QcTrace {
        &NoQcTrace
    }
}

/// The node's verifier: SB8's `Verify` over the members facade, in the scheme of the context.
pub struct NodeCommitmentVerifier<'a> {
    /// The chain, deployments and the global scheme of the check.
    pub llmq: LlmqCtx<'a, StoreError>,
    /// The members facade.
    pub members: &'a DmlMemberSource<'a>,
    /// Where the lines go.
    pub trace: &'a dyn QcTrace,
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
        let (members, verdict) =
            verify_commitment_counted(&self.llmq, self.members, qc, base, None, effects);
        trace_verify(self.trace, &self.llmq, qc, members, &verdict);
        match verdict {
            Ok(()) => Ok(true),
            Err(CommitmentError::Rejected(_)) => Ok(false),
            // No signature is checked.
            Err(CommitmentError::BlsUnsupported) => Err(QcBlockError::BlsUnsupported),
            Err(CommitmentError::Members(e)) => Err(e.into()),
        }
    }

    fn trace(&self) -> &dyn QcTrace {
        self.trace
    }
}

/// `Verify`'s lines for its `verdict` on `qc`, `members` the count of its member read: the dump
/// whenever the read was made, then `VALID QUORUM` or the reject's line (LQ2 D26, D27).
pub(crate) fn trace_verify(
    trace: &dyn QcTrace,
    llmq: &LlmqCtx<'_, StoreError>,
    qc: &LlmqCommitment,
    members: Option<usize>,
    verdict: &Result<(), CommitmentError<StoreError>>,
) {
    if let Some(members) = members {
        let size = llmq.params().consensus.llmq(qc.llmq_type()).map_or(0, |p| u64::from(p.size));
        trace.event(QcEvent::VerifyDump {
            qc,
            members,
            size,
        });
    }
    match verdict {
        Ok(()) => trace.event(QcEvent::ValidQuorum {
            qc,
        }),
        Err(CommitmentError::Rejected(reject)) => trace.event(QcEvent::VerifyReject {
            qc,
            reject: *reject,
        }),
        Err(_) => {}
    }
}

/// `VerifyNull`'s `VerifySizes` line for a null commitment of a type of `params` that fails it
/// (`llmq/commitment.cpp` v24:226-238): the signers' size first.
pub(crate) fn trace_sizes(trace: &dyn QcTrace, params: &LlmqParams, qc: &LlmqCommitment) {
    let size = u64::from(params.size);
    let reject = if qc.signers().num_bits() != size {
        CommitmentReject::SignersSize
    } else if qc.valid_members().num_bits() != size {
        CommitmentReject::ValidMembersSize
    } else {
        return;
    };
    trace.event(QcEvent::VerifyReject {
        qc,
        reject,
    });
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
    let trace = verifier.trace();
    let view = llmq.chain();
    // The parent's height, read for the line only where Core logs it before this function's own
    // read, and only for a trace that takes lines; a fault there logs nothing and leaves the
    // verdict as it is (LQ2 D26, D27).
    let early = |reject: CheckReject| {
        if !trace.takes_lines() {
            return;
        }
        if let Ok(header) = view.header(prev) {
            trace.event(QcEvent::CheckReject {
                height: header.height,
                reject,
            });
        }
    };
    let Some(SpecialPayload::QuorumCommitment(qc_tx)) = payload(tx) else {
        early(CheckReject::Payload);
        return Ok(reject(BAD_QC_PAYLOAD));
    };
    let qc = qc_tx.commitment();
    let Some(params) = llmq.params().consensus.llmq(qc.llmq_type()) else {
        early(CheckReject::Type(qc.llmq_type().to_base()));
        return Ok(reject(BAD_QC_COMMITMENT_TYPE));
    };
    trace.event(QcEvent::CheckDump {
        qc,
        size: u64::from(params.size),
    });
    if qc_tx.version() == 0 || qc_tx.version() > QC_TX_CURRENT_VERSION {
        early(CheckReject::Version(qc_tx.version()));
        return Ok(reject(BAD_QC_VERSION));
    }
    let prev_height = view.header(prev).map_err(MemberError::View)?.height;
    let late = |reject: CheckReject| {
        trace.event(QcEvent::CheckReject {
            height: prev_height,
            reject,
        });
    };
    // `uint32_t(pindexPrev->nHeight + 1)`.
    if qc_tx.height().to_u32() != prev_height.wrapping_add(1) {
        late(CheckReject::Height(qc_tx.height().to_u32()));
        return Ok(reject(BAD_QC_HEIGHT));
    }
    let Some((base, base_height)) = index.lookup(&as_block_hash(&qc.quorum_hash())) else {
        return Ok(reject(BAD_QC_QUORUM_HASH));
    };
    if view.ancestor(prev, base_height).map_err(MemberError::View)? != Some(base) {
        return Ok(reject(BAD_QC_QUORUM_HASH));
    }
    if qc.is_null() {
        if verifier.verify_null(qc) {
            return Ok(Ok(()));
        }
        trace_sizes(trace, params, qc);
        late(CheckReject::Null(qc.quorum_hash()));
        return Ok(reject(BAD_QC_INVALID_NULL));
    }
    if !verifier.verify(qc, base, effects)? {
        late(CheckReject::Verify(qc.quorum_hash()));
        return Ok(reject(BAD_QC_INVALID));
    }
    trace.event(QcEvent::CheckValid {
        height: prev_height,
    });
    Ok(Ok(()))
}
