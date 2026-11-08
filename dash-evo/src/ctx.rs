//! The context of evo block processing (A1 Appendix A `EvoCtx`).

use dash_consensus::{BlsCtx, ConsensusCtx};

/// The context of one block's evo processing: the consensus context of the block, plus whether
/// signatures are checked.
#[derive(Clone, Copy)]
pub struct EvoCtx<'a> {
    /// The block's consensus context. Its `bls` is `bls_ctx_for_block(h)` (A1 §1.4.1).
    pub cons: &'a ConsensusCtx<'a>,
    /// Core's `fCheckCbTxMerkleRoots`, false below assumevalid (A1 §2.10 step 6). It gates the
    /// CbTx checks, the ProTx signatures and the QC BLS signatures. It **never** gates the
    /// asset-unlock quorum signature or the MNHF signature, which Core verifies whatever the
    /// assumevalid decision (A1 m-1; quirk test).
    pub check_sigs: bool,
}

impl EvoCtx<'_> {
    /// The BLS context of the block being processed.
    pub fn bls(&self) -> BlsCtx {
        self.cons.bls
    }
}
