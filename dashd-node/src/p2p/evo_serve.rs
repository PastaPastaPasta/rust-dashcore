//! P6b's P2P adapter for `getmnlistd`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::Block;
use dash_consensus::BlockId;
use dash_consensus::primitives::core_serialize;
use dash_dml::MnList;
use dash_dml::mnlistdiff::{
    BlsSignatureBytes, MnListDiffError, MnListDiffRequest, MnListDiffSource, QuorumBase, QuorumKey,
    TargetBlock, build_mnlistdiff,
};
use dash_evo::BlsCtx;
use dash_evo::cbtx::{CoinbaseFacts, nonnull_coinbase_chainlock};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, ChainQuery, OnV};
use dashd_p2p::processing::seams::{BlockPos, BlockRead};
use dashd_p2p::processing::serve_evo::{EvoServe, ServeRefusal};

type ChainlockSignature = [u8; 96];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CapturedBlock {
    pub(super) id: BlockId,
    pub(super) hash: BlockHash,
    pub(super) data_pos: Option<BlockPos>,
    pub(super) have_data: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedWorkBlock {
    pub(super) block: CapturedBlock,
    pub(super) quorum_indexes: Vec<u16>,
    pub(super) height: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CapturedEvoReads {
    pub(super) target: CapturedBlock,
    pub(super) work_blocks: Vec<CapturedWorkBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkBlockChainlock {
    hash: BlockHash,
    signature: ChainlockSignature,
    quorum_indexes: Vec<u16>,
}

impl CapturedEvoReads {
    #[cfg_attr(not(test), expect(dead_code, reason = "the RP2a V source constructs captures"))]
    /// Captures each distinct work block once; the target is captured separately because Core
    /// reads it for the coinbase transaction after the quorum half.
    pub(super) fn new(
        target: CapturedBlock,
        work_blocks: impl IntoIterator<Item = (CapturedBlock, u16, i32)>,
    ) -> Self {
        let mut captured = Vec::new();
        for (block, quorum_index, height) in work_blocks {
            if let Some(existing) = captured
                .iter_mut()
                .find(|entry: &&mut CapturedWorkBlock| entry.block.hash == block.hash)
            {
                existing.quorum_indexes.push(quorum_index);
            } else {
                captured.push(CapturedWorkBlock {
                    block,
                    quorum_indexes: vec![quorum_index],
                    height,
                });
            }
        }
        Self {
            target,
            work_blocks: captured,
        }
    }

    fn read_work_blocks(
        &self,
        reader: &dyn EvoBlockReader,
        v20_active_after: bool,
        bls: BlsCtx,
    ) -> Vec<WorkBlockChainlock> {
        self.work_blocks
            .iter()
            .map(|work| {
                let signature =
                    match reader.read(&work.block, Some(work.height), v20_active_after, bls) {
                        EvoBlockRead::Block {
                            chainlock,
                            ..
                        } => chainlock.unwrap_or([0; 96]),
                        EvoBlockRead::Pruned | EvoBlockRead::Failed => [0; 96],
                    };
                WorkBlockChainlock {
                    hash: work.block.hash,
                    signature,
                    quorum_indexes: work.quorum_indexes.clone(),
                }
            })
            .collect()
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
    pub(super) work_blocks: BTreeMap<(BlockHash, i32), BlockHash>,
    pub(super) captures: CapturedEvoReads,
}

/// The V-side half supplied by RP2a-P2. The production implementation is still unavailable;
/// fake implementations exercise the complete P2P adapter before that source lands.
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

/// Reads only a `(BlockId, BlockPos)` pair captured by the V-side source.
pub(super) trait EvoBlockReader: Send + Sync {
    fn read(
        &self,
        block: &CapturedBlock,
        height: Option<i32>,
        v20_active_after: bool,
        bls: BlsCtx,
    ) -> EvoBlockRead;
}

struct NoEvoVSource;

impl EvoVSource for NoEvoVSource {
    fn capture(
        &self,
        _on_v: &OnV<'_>,
        _query: &ChainQuery<'_>,
        _base: BlockHash,
        block: BlockHash,
    ) -> Result<EvoVData, ServeRefusal> {
        Err(ServeRefusal {
            score: None,
            text: unavailable(&block),
        })
    }
}

struct NodeBlockReader {
    chain: Arc<super::chain::NodeChain>,
}

impl EvoBlockReader for NodeBlockReader {
    fn read(
        &self,
        captured: &CapturedBlock,
        height: Option<i32>,
        v20_active_after: bool,
        bls: BlsCtx,
    ) -> EvoBlockRead {
        if !captured.have_data {
            return EvoBlockRead::Pruned;
        }
        let Some(pos) = captured.data_pos else {
            return EvoBlockRead::Pruned;
        };
        match self.chain.read_block_at_captured(captured.id, pos) {
            BlockRead::Pruned => EvoBlockRead::Pruned,
            BlockRead::Failed => EvoBlockRead::Failed,
            BlockRead::Block(block) => block_data(&block, height, v20_active_after, bls),
        }
    }
}

fn block_data(
    block: &Block,
    height: Option<i32>,
    v20_active_after: bool,
    bls: BlsCtx,
) -> EvoBlockRead {
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
    let chainlock = height.and_then(|height| {
        u32::try_from(height).ok().and_then(|height| {
            nonnull_coinbase_chainlock(
                &CoinbaseFacts::First(Arc::clone(coinbase)),
                height,
                v20_active_after,
                bls,
            )
            .map(|(signature, _)| signature.to_bytes(bls.scheme()))
        })
    });
    EvoBlockRead::Block {
        target: TargetBlock {
            coinbase: core_serialize(coinbase.as_ref()),
            txids,
        },
        chainlock,
    }
}

struct CapturedSource {
    data: EvoVData,
    work_chainlocks: BTreeMap<BlockHash, Option<ChainlockSignature>>,
    target: Option<TargetBlock>,
}

impl MnListDiffSource for CapturedSource {
    type Error = String;

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

    fn work_block(&self, block: &BlockHash, height: i32) -> Result<Option<BlockHash>, Self::Error> {
        Ok(self.data.work_blocks.get(&(*block, height)).copied())
    }

    fn coinbase_chainlock(
        &self,
        block: &BlockHash,
        _height: i32,
    ) -> Result<Option<BlsSignatureBytes>, Self::Error> {
        Ok(self.work_chainlocks.get(block).copied().flatten())
    }

    fn target_block(&self, _block: &BlockHash) -> Option<TargetBlock> {
        self.target.clone()
    }
}

fn unavailable(block: &BlockHash) -> String {
    format!(
        "block data for block {} is not available (pruned or below an unvalidated snapshot base)",
        rpc_hex(block)
    )
}

fn source_refusal(
    base: BlockHash,
    block: BlockHash,
    error: MnListDiffError<String>,
) -> ServeRefusal {
    let score = match error.peer_outcome() {
        dash_dml::mnlistdiff::PeerOutcome::Unscored => None,
        dash_dml::mnlistdiff::PeerOutcome::Misbehaving(score) => Some(score),
    };
    let _ = (base, block);
    ServeRefusal {
        score,
        text: error.to_string(),
    }
}

fn serve_captured(
    data: EvoVData,
    reader: &dyn EvoBlockReader,
    proto: i32,
) -> Result<Vec<u8>, ServeRefusal> {
    let work_chainlocks = data
        .captures
        .read_work_blocks(reader, data.v20_active_after, data.bls)
        .into_iter()
        .map(|work| (work.hash, Some(work.signature)))
        .collect();

    let target = match reader.read(&data.captures.target, None, data.v20_active_after, data.bls) {
        EvoBlockRead::Block {
            target,
            ..
        } => target,
        EvoBlockRead::Pruned => {
            return Err(ServeRefusal {
                score: None,
                text: unavailable(&data.block),
            });
        }
        EvoBlockRead::Failed => {
            return Err(ServeRefusal {
                score: Some(1),
                text: format!("failed to read block {} from disk", rpc_hex(&data.block)),
            });
        }
    };

    let source = CapturedSource {
        data,
        work_chainlocks,
        target: Some(target),
    };
    let request = MnListDiffRequest {
        requested_base: source.data.requested_base,
        base_block: source.data.base_block,
        base_list: &source.data.base_list,
        block: source.data.block,
        list: &source.data.target_list,
        v20_active_after: source.data.v20_active_after,
    };
    let diff = build_mnlistdiff(&request, &source)
        .map_err(|error| source_refusal(request.requested_base, request.block, error))?;
    let mut bytes = Vec::new();
    diff.encode(proto, &mut bytes);
    Ok(bytes)
}

fn rpc_hex(hash: &BlockHash) -> String {
    hash.as_bytes().iter().rev().map(|byte| format!("{byte:02x}")).collect()
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
            v_source: Arc::new(NoEvoVSource),
            reader: Arc::new(NodeBlockReader {
                chain,
            }),
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
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
    ) -> Result<Vec<u8>, ServeRefusal> {
        let v_source = Arc::clone(&self.v_source);
        let capture = self.v.on_v(move |on_v, query| v_source.capture(on_v, query, base, block));
        let data = capture.recv().map_err(|_| ServeRefusal {
            score: None,
            text: unavailable(&block),
        })??;
        serve_captured(data, self.reader.as_ref(), proto)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(byte: u8, pos: u32) -> CapturedBlock {
        CapturedBlock {
            id: BlockId(u32::from(byte)),
            hash: BlockHash::from([byte; 32]),
            data_pos: Some(BlockPos {
                file: 1,
                pos,
            }),
            have_data: true,
        }
    }

    #[test]
    fn captures_two_distinct_work_blocks() {
        let reads =
            CapturedEvoReads::new(block(9, 90), [(block(1, 10), 0, 10), (block(2, 20), 1, 20)]);
        assert_eq!(reads.work_blocks.len(), 2);
        assert_eq!(reads.work_blocks[0].block.data_pos.unwrap().pos, 10);
        assert_eq!(reads.work_blocks[1].block.data_pos.unwrap().pos, 20);
    }

    fn data(work_blocks: Vec<(CapturedBlock, u16, i32)>) -> EvoVData {
        let base = BlockHash::from([8; 32]);
        let block = BlockHash::from([9; 32]);
        EvoVData {
            requested_base: base,
            base_block: base,
            block,
            base_list: Arc::new(MnList::new(base, 1, 0)),
            target_list: Arc::new(MnList::new(block, 2, 0)),
            v20_active_after: false,
            bls: BlsCtx::BASIC,
            base_quorums: BTreeSet::new(),
            target_quorums: BTreeSet::new(),
            commitments: BTreeMap::new(),
            quorum_bases: BTreeMap::new(),
            work_blocks: BTreeMap::new(),
            captures: CapturedEvoReads::new(
                CapturedBlock {
                    id: BlockId(9),
                    hash: block,
                    data_pos: Some(BlockPos {
                        file: 1,
                        pos: 90,
                    }),
                    have_data: true,
                },
                work_blocks,
            ),
        }
    }

    struct FakeReader {
        failed: BTreeSet<BlockHash>,
        pruned: BTreeSet<BlockHash>,
        calls: std::sync::Mutex<Vec<(BlockId, BlockPos, BlockHash)>>,
    }

    impl FakeReader {
        fn new() -> Self {
            Self {
                failed: BTreeSet::new(),
                pruned: BTreeSet::new(),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl EvoBlockReader for FakeReader {
        fn read(
            &self,
            block: &CapturedBlock,
            height: Option<i32>,
            _v20: bool,
            _bls: BlsCtx,
        ) -> EvoBlockRead {
            self.calls.lock().unwrap().push((block.id, block.data_pos.unwrap(), block.hash));
            if self.pruned.contains(&block.hash) {
                return EvoBlockRead::Pruned;
            }
            if self.failed.contains(&block.hash) {
                return EvoBlockRead::Failed;
            }
            EvoBlockRead::Block {
                target: TargetBlock {
                    coinbase: vec![1, 2, 3],
                    txids: vec![[0; 32]],
                },
                chainlock: height.map(|_| [7; 96]),
            }
        }
    }

    #[test]
    fn prune_race_is_unscored() {
        let reader = FakeReader {
            pruned: [BlockHash::from([9; 32])].into(),
            ..FakeReader::new()
        };
        assert_eq!(serve_captured(data(Vec::new()), &reader, 70242).unwrap_err().score, None);
    }

    #[test]
    fn real_io_failure_scores_one() {
        let reader = FakeReader {
            failed: [BlockHash::from([9; 32])].into(),
            ..FakeReader::new()
        };
        assert_eq!(serve_captured(data(Vec::new()), &reader, 70242).unwrap_err().score, Some(1));
    }

    #[test]
    fn reorg_keeps_captured_hashes_and_positions() {
        let reader = FakeReader::new();
        let _ = serve_captured(
            data(vec![(block(1, 10), 0, 10), (block(2, 20), 1, 20)]),
            &reader,
            70242,
        )
        .unwrap();
        let calls = reader.calls.lock().unwrap();
        assert_eq!(
            calls[0],
            (
                BlockId(1),
                BlockPos {
                    file: 1,
                    pos: 10
                },
                BlockHash::from([1; 32])
            )
        );
        assert_eq!(
            calls[1],
            (
                BlockId(2),
                BlockPos {
                    file: 1,
                    pos: 20
                },
                BlockHash::from([2; 32])
            )
        );
    }

    #[test]
    fn two_work_blocks_are_both_read() {
        let reader = FakeReader::new();
        let _ = serve_captured(
            data(vec![(block(1, 10), 0, 10), (block(2, 20), 1, 20)]),
            &reader,
            70242,
        )
        .unwrap();
        assert_eq!(reader.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn one_failed_work_block_gets_only_its_null_signature() {
        let reader = FakeReader {
            failed: [BlockHash::from([1; 32])].into(),
            ..FakeReader::new()
        };
        let reads = data(vec![(block(1, 10), 0, 10), (block(2, 20), 1, 20)]).captures;
        let locks = reads.read_work_blocks(&reader, false, BlsCtx::BASIC);
        assert_eq!(locks[0].signature, [0; 96]);
        assert_eq!(locks[1].signature, [7; 96]);
        assert_eq!(locks[0].quorum_indexes, [0]);
        assert_eq!(locks[1].quorum_indexes, [1]);
    }

    #[test]
    fn builder_bytes_are_returned_unchanged() {
        let reader = FakeReader::new();
        let output = serve_captured(data(Vec::new()), &reader, 70242).unwrap();
        assert_eq!(&output[0..2], &[1, 0]);
    }
}
