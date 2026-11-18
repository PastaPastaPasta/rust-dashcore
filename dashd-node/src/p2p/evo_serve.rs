//! P6b's P2P adapter for `getmnlistd`.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::BlockId;
use dash_consensus::primitives::core_serialize;
use dash_dml::MnList;
use dash_dml::mnlistdiff::{
    BlsSignatureBytes, MnListDiffError, MnListDiffRequest, MnListDiffSource, PeerOutcome,
    QuorumBase, QuorumKey, SimplifiedMnListDiff, TargetBlock, build_mnlistdiff,
};
use dash_evo::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV};
use dashd_p2p::processing::seams::{BlockPos, BlockRead, ChainAccess};
use dashd_p2p::processing::serve_mnlistdiff::{EvoServe, ServeRefusal};

type ChainlockSignature = [u8; 96];

/// A block the V turn resolved, with the position and `HAVE_DATA` bit it read in that turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CapturedBlock {
    pub(super) id: BlockId,
    pub(super) hash: BlockHash,
    pub(super) data_pos: Option<BlockPos>,
    pub(super) have_data: bool,
}

/// A block `GetNonNullCoinbaseChainlock` reads: the target's ancestor at `height`, and
/// `DeploymentActiveAt(work block, V20)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CapturedWorkBlock {
    pub(super) block: CapturedBlock,
    pub(super) height: i32,
    pub(super) v20_active_at: bool,
}

/// Every block the adapter may read after V: the target and each distinct work block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedEvoReads {
    pub(super) target: CapturedBlock,
    pub(super) work_blocks: Vec<CapturedWorkBlock>,
}

impl CapturedEvoReads {
    /// Keeps each work block once, by hash.
    #[cfg_attr(not(test), expect(dead_code, reason = "the RP2a V source constructs captures"))]
    pub(super) fn new(
        target: CapturedBlock,
        work_blocks: impl IntoIterator<Item = CapturedWorkBlock>,
    ) -> Self {
        let mut captured: Vec<CapturedWorkBlock> = Vec::new();
        for work in work_blocks {
            if !captured.iter().any(|entry| entry.block.hash == work.block.hash) {
                captured.push(work);
            }
        }
        Self {
            target,
            work_blocks: captured,
        }
    }

    /// The captured work block at `height`. A negative height is Core's `nullptr` ancestor,
    /// never captured; any other height must have exactly one capture.
    fn work_block_at(&self, height: i32) -> Result<Option<&CapturedWorkBlock>, CaptureMismatch> {
        if height < 0 {
            return Ok(None);
        }
        let mut at = self.work_blocks.iter().filter(|work| work.height == height);
        match (at.next(), at.next()) {
            (Some(work), None) => Ok(Some(work)),
            _ => Err(CaptureMismatch(height)),
        }
    }
}

/// The builder asked for a work block the V turn did not capture exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CaptureMismatch(i32);

impl fmt::Display for CaptureMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no single captured work block at height {}", self.0)
    }
}

/// The owned result of the one V turn. It includes the two lists, all quorum inputs and every
/// block position the adapter may read after V has been released.
pub(super) struct EvoVData {
    pub(super) requested_base: BlockHash,
    pub(super) base_block: BlockHash,
    pub(super) block: BlockHash,
    pub(super) base_list: Arc<MnList>,
    pub(super) target_list: Arc<MnList>,
    pub(super) v20_active_after: bool,
    pub(super) bls: BlsCtx,
    pub(super) base_quorums: BTreeSet<QuorumKey>,
    pub(super) target_quorums: BTreeSet<QuorumKey>,
    pub(super) commitments: BTreeMap<QuorumKey, Vec<u8>>,
    pub(super) quorum_bases: BTreeMap<QuorumKey, QuorumBase>,
    pub(super) captures: CapturedEvoReads,
}

/// The V-side half supplied by RP2a-P2. One call is one V turn, and everything the adapter
/// uses after it comes from the returned [`EvoVData`]. In that turn `capture` must:
///
/// - re-run `check_request` on V's own view (the handler checked the P2P snapshot), and refuse
///   with Core's text and its `peer_outcome` score;
/// - read both lists, mapping unavailable list data (`ListUnavailable`) to an unscored refusal;
/// - read the quorum half: the active quorums at the base and the target, each new quorum's
///   serialized mined commitment and its quorum base;
/// - set `v20_active_after` (`DeploymentActiveAfter(target, V20)`) and the view's [`BlsCtx`];
/// - capture the target, and, only when `v20_active_after`, every distinct work block the quorum
///   half uses: each resolved by `GetAncestor` from the target (never the active chain), with
///   its hash, position, `HAVE_DATA` and `DeploymentActiveAt(work block, V20)`, all from the
///   same view. A negative work height is Core's `nullptr` ancestor and is not captured.
///
/// A work height the builder asks for that has no single capture fails the request closed
/// (`BlockRead`, score 1), never with a null signature.
pub(super) trait EvoVSource: Send + Sync {
    fn capture(
        &self,
        on_v: &OnV<'_>,
        query: &ChainQuery<'_>,
        base: BlockHash,
        block: BlockHash,
    ) -> Result<EvoVData, ServeRefusal>;
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

/// Reads only a `(BlockId, BlockPos)` pair captured by the V-side source. `chainlock_height` is
/// the work block's height when the caller wants its coinbase ChainLock: the caller has
/// already checked V20 at that block.
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
    fn read(
        &self,
        captured: &CapturedBlock,
        chainlock_height: Option<i32>,
        bls: BlsCtx,
    ) -> EvoBlockRead {
        let (true, Some(pos)) = (captured.have_data, captured.data_pos) else {
            return EvoBlockRead::Pruned;
        };
        match self.chain.read_block_at(captured.id, pos) {
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

/// The builder's source over one capture. Block files are read lazily, at the points Core
/// reads them: each work block in the quorum half, the target last.
struct CapturedSource<'a> {
    data: EvoVData,
    reader: &'a dyn EvoBlockReader,
    target_pruned: Cell<bool>,
}

impl MnListDiffSource for CapturedSource<'_> {
    type Error = CaptureMismatch;

    fn active_quorums(&self, block: &BlockHash) -> Result<BTreeSet<QuorumKey>, Self::Error> {
        if *block == self.data.block {
            Ok(self.data.target_quorums.clone())
        } else {
            Ok(self.data.base_quorums.clone())
        }
    }

    fn mined_commitment(&self, quorum: &QuorumKey) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.data.commitments.get(quorum).cloned())
    }

    fn quorum(&self, quorum: &QuorumKey) -> Result<Option<QuorumBase>, Self::Error> {
        Ok(self.data.quorum_bases.get(quorum).copied())
    }

    fn work_block(
        &self,
        _block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlockHash>, Self::Error> {
        Ok(self.data.captures.work_block_at(height)?.map(|work| work.block.hash))
    }

    // Core: evo/cbtx.cpp:181-197 GetNonNullCoinbaseChainlock(pindex)
    fn coinbase_chainlock(
        &self,
        block: &BlockHash,
        height: i32,
    ) -> Result<Option<BlsSignatureBytes>, Self::Error> {
        let work = self.data.captures.work_block_at(height)?.ok_or(CaptureMismatch(height))?;
        if work.block.hash != *block {
            return Err(CaptureMismatch(height));
        }
        if !work.v20_active_at {
            return Ok(None);
        }
        Ok(match self.reader.read(&work.block, Some(height), self.data.bls) {
            EvoBlockRead::Block {
                chainlock,
                ..
            } => chainlock,
            EvoBlockRead::Pruned | EvoBlockRead::Failed => None,
        })
    }

    fn target_block(&self, _block: &BlockHash) -> Option<TargetBlock> {
        match self.reader.read(&self.data.captures.target, None, self.data.bls) {
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

/// Builds the diff from one capture. A target read that failed with its `HAVE_DATA` bit gone
/// is the prune race, which Core, holding `cs_main`, refuses unscored at the check. A work
/// block missing from the capture fails closed as a block read.
fn build_captured(
    data: EvoVData,
    reader: &dyn EvoBlockReader,
) -> Result<SimplifiedMnListDiff, ServeRefusal> {
    let source = CapturedSource {
        data,
        reader,
        target_pruned: Cell::new(false),
    };
    let request = MnListDiffRequest {
        requested_base: source.data.requested_base,
        base_block: source.data.base_block,
        base_list: &source.data.base_list,
        block: source.data.block,
        list: &source.data.target_list,
        v20_active_after: source.data.v20_active_after,
    };
    build_mnlistdiff(&request, &source).map_err(|error| {
        refusal(&match error {
            MnListDiffError::Source(CaptureMismatch(_)) => {
                MnListDiffError::BlockRead(request.block)
            }
            MnListDiffError::BlockRead(block) if source.target_pruned.get() => {
                MnListDiffError::BlockDataUnavailable(block)
            }
            error => error,
        })
    })
}

fn serve_captured(
    data: EvoVData,
    reader: &dyn EvoBlockReader,
    proto: i32,
) -> Result<Vec<u8>, ServeRefusal> {
    Ok(build_captured(data, reader)?.to_bytes(proto))
}

pub(super) struct NodeEvoServe {
    v: ChainHandle,
    /// `None` until RP2a-P2's V source is wired: a checked request then ends silently.
    v_source: Option<Arc<dyn EvoVSource>>,
    reader: Arc<dyn EvoBlockReader>,
}

impl NodeEvoServe {
    pub(super) fn new(v: ChainHandle, chain: Arc<super::chain::NodeChain>) -> Self {
        Self {
            v,
            v_source: None,
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
            v_source: Some(v_source),
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
        let Some(v_source) = self.v_source.as_ref().map(Arc::clone) else {
            return Ok(None);
        };
        let capture = self.v.on_v(move |on_v, query| v_source.capture(on_v, query, base, block));
        // V is gone only at shutdown: nothing to answer.
        let Ok(data) = capture.recv() else {
            return Ok(None);
        };
        serve_captured(data?, self.reader.as_ref(), proto).map(Some)
    }
}

#[cfg(test)]
mod tests;
