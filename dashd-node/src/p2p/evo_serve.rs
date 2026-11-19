//! P6b's P2P adapter for `getmnlistd`: RP2a-P2's V capture in one turn, then the block reads.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::primitives::core_serialize;
use dash_dml::StoreError;
use dash_dml::mnlistdiff::{
    BlsSignatureBytes, MnListDiffError, MnListDiffRequest, MnListDiffSource, PeerOutcome,
    QuorumBase, QuorumKey, SimplifiedMnListDiff, TargetBlock, build_mnlistdiff,
};
use dash_evo::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV};
use dashd_p2p::processing::seams::BlockRead;
use dashd_p2p::processing::serve_mnlistdiff::{EvoServe, ServeRefusal};

use crate::rpc::evo::sml::{self, CapturedBlock, DiffCapture};

type ChainlockSignature = [u8; 96];

/// The V-side half. One call is one V turn, and everything the adapter uses after it comes from
/// the returned [`DiffCapture`]. In that turn `capture` re-runs `check_request` on V's own view
/// (the handler checked the P2P snapshot), reads both lists and the quorum half, and captures
/// the target and each work block from the same view; it refuses with Core's text and its
/// `peer_outcome` score.
///
/// The capture must be the request's: its target's hash is `block`, its `requested_base` is
/// `base` as the peer sent it, the null hash included, its `base_block` is `base` when `base` is
/// not null, and its target list is `block`'s or Core's default list. A capture of another
/// request, or a work height the builder asks for that the capture lacks, fails the request
/// closed (`BlockRead`, score 1), never with a null signature.
pub(super) trait EvoVSource: Send + Sync {
    fn capture(
        &self,
        on_v: &OnV<'_>,
        query: &ChainQuery<'_>,
        base: BlockHash,
        block: BlockHash,
    ) -> Result<DiffCapture, ServeRefusal>;
}

/// RP2a-P2's capture, the one `protx diff` makes.
struct NodeVSource;

impl EvoVSource for NodeVSource {
    fn capture(
        &self,
        on_v: &OnV<'_>,
        query: &ChainQuery<'_>,
        base: BlockHash,
        block: BlockHash,
    ) -> Result<DiffCapture, ServeRefusal> {
        sml::capture(on_v, query, base, block).map_err(|error| refusal(&error))
    }
}

/// A captured block-file read. `Pruned` is the unscored race; `Failed` is an I/O failure.
pub(super) enum EvoBlockRead {
    Block {
        target: TargetBlock,
        chainlock: Option<ChainlockSignature>,
    },
    Pruned,
    Failed,
}

/// Reads only a block the V turn captured, at its captured position. `chainlock_height` is the
/// work block's height when the caller wants its coinbase ChainLock: the caller has already
/// checked V20 at that block.
pub(super) trait EvoBlockReader: Send + Sync {
    fn read(
        &self,
        block: &CapturedBlock,
        chainlock_height: Option<i32>,
        bls: BlsCtx,
    ) -> EvoBlockRead;
}

struct NodeBlockReader {
    chain: Arc<super::chain::NodeChain>,
}

impl EvoBlockReader for NodeBlockReader {
    /// `ReadBlockFromDisk(block, pindex)` with Core's lines: a block without its data has the
    /// null `GetBlockPos()`, which opens no file; a hash mismatch logs the index variant's line.
    // Core: node/blockstorage.cpp:813-816, :838-849
    fn read(
        &self,
        captured: &CapturedBlock,
        chainlock_height: Option<i32>,
        bls: BlsCtx,
    ) -> EvoBlockRead {
        let Some(pos) = captured.pos else {
            tracing::error!(
                "ReadBlockFromDisk: OpenBlockFile failed for FlatFilePos(nFile=-1, nPos=0)"
            );
            return EvoBlockRead::Pruned;
        };
        match self.chain.read_block_by_index_at(captured.id, pos) {
            BlockRead::Pruned => EvoBlockRead::Pruned,
            BlockRead::Failed => EvoBlockRead::Failed,
            BlockRead::Block(block) => block_data(&block, chainlock_height, bls),
        }
    }
}

fn block_data(block: &Block, chainlock_height: Option<i32>, bls: BlsCtx) -> EvoBlockRead {
    let txids = block.txs().iter().map(|tx| *tx.txid().as_bytes()).collect();
    let Some(coinbase) = block.txs().first() else {
        return EvoBlockRead::Block {
            target: TargetBlock {
                coinbase: Vec::new(),
                txids,
            },
            chainlock: None,
        };
    };
    let chainlock =
        chainlock_height.and_then(|height| u32::try_from(height).ok()).and_then(|height| {
            nonnull_coinbase_chainlock(
                &CoinbaseFacts::First(Arc::clone(coinbase)),
                height,
                true,
                bls,
            )
            .map(|(signature, _)| signature.to_bytes(bls.scheme()))
        });
    EvoBlockRead::Block {
        target: TargetBlock {
            coinbase: core_serialize(coinbase.as_ref()),
            txids,
        },
        chainlock,
    }
}

/// The builder's source over one capture: the quorum half's answers in the builder's order, as
/// `protx diff`'s source gives them, and block files read lazily, at the points Core reads them:
/// each work block in the quorum half, the target last.
struct CapturedSource<'a> {
    capture: &'a DiffCapture,
    reader: &'a dyn EvoBlockReader,
    active_served: Cell<usize>,
    uncaptured: Cell<bool>,
    target_pruned: Cell<bool>,
}

impl MnListDiffSource for CapturedSource<'_> {
    type Error = StoreError;

    fn active_quorums(&self, _: &BlockHash) -> Result<BTreeSet<QuorumKey>, StoreError> {
        let served = self.active_served.get();
        self.active_served.set(served + 1);
        self.capture.quorums.active.get(served).cloned().unwrap_or_else(|| Err(sml::not_captured()))
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, StoreError> {
        self.capture.quorums.mined.get(quorum).cloned().unwrap_or_else(|| Err(sml::not_captured()))
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, StoreError> {
        self.capture.quorums.bases.get(quorum).cloned().unwrap_or_else(|| Err(sml::not_captured()))
    }

    // Core: evo/cbtx.cpp:181-198 GetNonNullCoinbaseChainlock(pindex)
    fn coinbase_chainlock(
        &self,
        _: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, StoreError> {
        let Some(work) = self.capture.quorums.work.get(&height) else {
            self.uncaptured.set(true);
            return Err(sml::not_captured());
        };
        let Some(work) = work.filter(|work| work.v20) else {
            return Ok(None);
        };
        Ok(match self.reader.read(&work.block, Some(height), self.capture.bls) {
            EvoBlockRead::Block {
                chainlock,
                ..
            } => chainlock,
            EvoBlockRead::Pruned | EvoBlockRead::Failed => None,
        })
    }

    fn target_block(&self, _: &BlockHash) -> Option<TargetBlock> {
        match self.reader.read(&self.capture.target, None, self.capture.bls) {
            EvoBlockRead::Block {
                target,
                ..
            } => Some(target),
            EvoBlockRead::Pruned => {
                self.target_pruned.set(true);
                None
            }
            EvoBlockRead::Failed => None,
        }
    }
}

/// Core's refusal text and score for a builder error.
fn refusal<E: fmt::Display>(error: &MnListDiffError<E>) -> ServeRefusal {
    let score = match error.peer_outcome() {
        PeerOutcome::Unscored => None,
        PeerOutcome::Misbehaving(score) => Some(score),
    };
    ServeRefusal {
        score,
        text: error.to_string(),
    }
}

/// Builds the diff for the request (`base`, `block`) from its capture. A capture of another
/// request, or a work block missing from it, fails closed as a block read. A target read that
/// failed with its `HAVE_DATA` bit gone is the prune race, which Core, holding `cs_main`, refuses
/// unscored at the check.
fn build_captured(
    capture: &DiffCapture,
    base: BlockHash,
    block: BlockHash,
    reader: &dyn EvoBlockReader,
) -> Result<SimplifiedMnListDiff, ServeRefusal> {
    if capture.target.hash != block
        || capture.requested_base != base
        || (!base.is_null() && capture.base_block != base)
        || (*capture.list.block_hash() != block && !capture.list.is_default())
    {
        return Err(refusal(&MnListDiffError::<StoreError>::BlockRead(block)));
    }
    let source = CapturedSource {
        capture,
        reader,
        active_served: Cell::new(0),
        uncaptured: Cell::new(false),
        target_pruned: Cell::new(false),
    };
    let request = MnListDiffRequest {
        requested_base: base,
        base_block: capture.base_block,
        base_list: &capture.base_list,
        block,
        list: &capture.list,
        v20_active_after: capture.v20_active_after,
    };
    build_mnlistdiff(&request, &source).map_err(|error| {
        refusal(&match error {
            MnListDiffError::Source(_) if source.uncaptured.get() => {
                MnListDiffError::BlockRead(block)
            }
            MnListDiffError::BlockRead(block) if source.target_pruned.get() => {
                MnListDiffError::BlockDataUnavailable(block)
            }
            error => error,
        })
    })
}

fn serve_captured(
    capture: &DiffCapture,
    base: BlockHash,
    block: BlockHash,
    reader: &dyn EvoBlockReader,
    proto: i32,
) -> Result<Vec<u8>, ServeRefusal> {
    Ok(build_captured(capture, base, block, reader)?.to_bytes(proto))
}

pub(super) struct NodeEvoServe {
    v: ChainHandle,
    v_source: Arc<dyn EvoVSource>,
    reader: Arc<dyn EvoBlockReader>,
}

impl NodeEvoServe {
    pub(super) fn new(v: ChainHandle, chain: Arc<super::chain::NodeChain>) -> Self {
        Self {
            v,
            v_source: Arc::new(NodeVSource),
            reader: Arc::new(NodeBlockReader {
                chain,
            }),
        }
    }

    #[cfg(test)]
    fn with_sources(
        v: ChainHandle,
        v_source: Arc<dyn EvoVSource>,
        reader: Arc<dyn EvoBlockReader>,
    ) -> Self {
        Self {
            v,
            v_source,
            reader,
        }
    }
}

impl EvoServe for NodeEvoServe {
    fn mnlistdiff(
        &mut self,
        base: BlockHash,
        block: BlockHash,
        proto: i32,
    ) -> Result<Option<Vec<u8>>, ServeRefusal> {
        let v_source = Arc::clone(&self.v_source);
        let capture = self.v.on_v(move |on_v, query| v_source.capture(on_v, query, base, block));
        // V is gone, holds no evo state (D77) or has latched (D74): nothing to answer.
        let Ok(capture) = capture.recv() else {
            return Ok(None);
        };
        serve_captured(&capture?, base, block, self.reader.as_ref(), proto).map(Some)
    }
}

#[cfg(test)]
mod tests;
