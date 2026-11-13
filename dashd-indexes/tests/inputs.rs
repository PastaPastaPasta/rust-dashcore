// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::iter;
use std::path::Path;
use std::sync::Arc;

use dash_chain::{BlockRef, FlatFilePos, IndexAccess, Status};
use dash_consensus::coins::BlockUndo;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::header::HeaderParams;
use dash_consensus::subsidy::{COIN, SubsidyParams};
use dash_consensus::{Block, BlockId, ChainAncestry, HeaderCtx, core_serialize, decode_block};
use dash_num::{Arith256, CompactTarget};
use dash_params::{BuriedDeployment, ChainOption, ChainParamsBuilder, Network, Release};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{BlockFiles as ChainBlockFiles, RecordingAbort};
use dashd_chainstate::store::{GlobalFailStop, GuardedFiles, NodeBlockFiles};
use dashd_chainstate::subscriber::{BlockSource, SubscriberError};
use dashd_indexes::{
    NodeBlockSource, NodeSubsidyInputs, NodeTxPositions, SubsidyInputs, TxPositions,
};
use dashd_store::flatfile::{BlockFiles, BlockFilesConfig, BlockFilesState, OsFs, undo_checksum};
use serde_json::Value;

struct Entry {
    block: Arc<Block>,
    reference: BlockRef,
    header: HeaderCtx,
    parent: Option<BlockId>,
    data: Option<FlatFilePos>,
    undo: Option<FlatFilePos>,
    total_subsidy: i64,
}

struct Index(Vec<Entry>);

impl IndexAccess for Index {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.0.iter().find(|entry| entry.reference.hash == *hash).map(|entry| entry.reference.id)
    }

    fn hash(&self, id: BlockId) -> Option<BlockHash> {
        self.0.get(id.0 as usize).map(|entry| entry.reference.hash)
    }

    fn ancestor(&self, mut id: BlockId, height: u32) -> Option<BlockId> {
        loop {
            let entry = self.0.get(id.0 as usize)?;
            if entry.reference.height == height {
                return Some(id);
            }
            if entry.reference.height < height {
                return None;
            }
            id = entry.parent?;
        }
    }

    fn lca(&self, mut a: BlockId, mut b: BlockId) -> Option<BlockId> {
        while a != b {
            if self.height(a)? >= self.height(b)? {
                a = self.0.get(a.0 as usize)?.parent?;
            } else {
                b = self.0.get(b.0 as usize)?.parent?;
            }
        }
        Some(a)
    }

    fn height(&self, id: BlockId) -> Option<u32> {
        self.0.get(id.0 as usize).map(|entry| entry.reference.height)
    }

    fn have_data_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        self.0.get(id.0 as usize)?.data
    }

    fn undo_pos(&self, id: BlockId) -> Option<FlatFilePos> {
        self.0.get(id.0 as usize)?.undo
    }

    fn status(&self, _: BlockId) -> Option<Status> {
        None
    }
}

impl ChainAncestry for Index {
    fn header(&self, id: BlockId) -> HeaderCtx {
        self.0[id.0 as usize].header
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        IndexAccess::ancestor(self, id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.0[id.0 as usize].parent
    }
}

impl DeploymentView for Index {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        panic!("the provider must query activation at the block");
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        assert_eq!(deployment, Deployment::Buried(BuriedDeployment::V20));
        self.0[block.0 as usize].reference.height >= 100
    }
}

fn duffs(value: &Value) -> i64 {
    let text = value.to_string();
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    assert!(fraction.len() <= 8);
    whole.parse::<i64>().unwrap() * COIN + format!("{fraction:0<8}").parse::<i64>().unwrap()
}

fn records(bytes: &[u8]) -> impl Iterator<Item = (FlatFilePos, &[u8])> {
    let mut offset = 0;
    iter::from_fn(move || {
        let prefix = bytes.get(offset..offset + 8)?;
        if prefix == [0; 8] {
            return None;
        }
        assert_eq!(&prefix[..4], &[0xfc, 0xc1, 0xb7, 0xdc]);
        let size = u32::from_le_bytes(prefix[4..].try_into().unwrap()) as usize;
        let start = offset + 8;
        offset = start + size;
        Some((
            FlatFilePos {
                file: 0,
                pos: start as u32,
            },
            &bytes[start..offset],
        ))
    })
}

fn fixture() -> Index {
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/dashd-v24/blocks.json")).unwrap();
    let metadata: HashMap<BlockHash, &Value> =
        rows.iter().map(|row| (row["hash"].as_str().unwrap().parse().unwrap(), row)).collect();
    let entries: Vec<Entry> = records(include_bytes!("fixtures/dashd-v24/blocks/blk00000.dat"))
        .enumerate()
        .map(|(id, (pos, bytes))| {
            let block = Arc::new(decode_block(&mut &bytes[..]).unwrap().into_value());
            let row = metadata[&block.hash()];
            let height = row["getblockheader"]["height"].as_u64().unwrap() as u32;
            let stats = &row["gettxoutsetinfo"];
            Entry {
                reference: BlockRef {
                    id: BlockId(id as u32),
                    hash: block.hash(),
                    height,
                },
                header: HeaderCtx {
                    height,
                    hash: block.hash(),
                    time: block.header().time(),
                    bits: CompactTarget::new(block.header().bits()),
                    version: block.header().version(),
                    mtp: row["getblockheader"]["mediantime"].as_i64().unwrap(),
                    chain_work: row["getblockheader"]["chainwork"]
                        .as_str()
                        .unwrap()
                        .parse()
                        .unwrap(),
                },
                block,
                parent: None,
                data: Some(pos),
                undo: None,
                // Core: index/coinstatsindex.cpp:204-218 (RPC omits total_subsidy)
                total_subsidy: duffs(&stats["total_amount"])
                    + duffs(&stats["total_unspendable_amount"]),
            }
        })
        .collect();
    assert_eq!(entries.len(), rows.len());
    let ids: HashMap<_, _> =
        entries.iter().map(|entry| (entry.reference.hash, entry.reference.id)).collect();
    let mut index = Index(entries);
    let rev = include_bytes!("fixtures/dashd-v24/blocks/rev00000.dat");
    for entry in &mut index.0 {
        if entry.reference.height > 0 {
            let parent = entry.block.header().prev_hash();
            entry.parent = Some(ids[&parent]);
            // Rev record lengths exclude their trailing checksum.
            let mut at = 0;
            while rev[at..at + 8] != [0; 8] {
                let size = u32::from_le_bytes(rev[at + 4..at + 8].try_into().unwrap()) as usize;
                let start = at + 8;
                let end = start + size;
                if undo_checksum(parent.as_bytes(), &rev[start..end]) == rev[end..end + 32] {
                    entry.undo = Some(FlatFilePos {
                        file: 0,
                        pos: start as u32,
                    });
                    break;
                }
                at = end + 32;
            }
            assert!(entry.undo.is_some(), "undo for {}", entry.reference.hash);
        }
    }
    index
}

fn files() -> Arc<dyn ChainBlockFiles + Sync> {
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0).build().unwrap();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dashd-v24/blocks");
    let files = BlockFiles::with_fs(
        Arc::new(OsFs::new()),
        BlockFilesConfig::new(path, [0xfc, 0xc1, 0xb7, 0xdc]),
        BlockFilesState::default(),
    );
    Arc::new(NodeBlockFiles::new(
        Arc::new(GuardedFiles::new(files, GlobalFailStop::new())),
        *HeaderParams::from_chain_params(&params).unwrap().pow(),
        Arc::new(RecordingAbort::default()),
    ))
}

#[test]
fn positions_and_block_reads_match_the_core_block_file() {
    let index = Arc::new(fixture());
    let positions = NodeTxPositions::new(index.clone());
    let source = NodeBlockSource::new(index.clone(), files());
    for entry in &index.0 {
        assert_eq!(positions.data_pos(&entry.reference).unwrap(), entry.data.unwrap());
        assert_eq!(
            core_serialize(source.block(&entry.reference).unwrap().as_ref()),
            core_serialize(entry.block.as_ref())
        );
    }
}

#[test]
fn genesis_needs_no_undo_record_and_later_blocks_read_real_undo() {
    let index = Arc::new(fixture());
    let source = NodeBlockSource::new(index.clone(), files());
    let mut nonempty = 0;
    for entry in &index.0 {
        let bytes = source.undo(&entry.reference).unwrap();
        let undo = BlockUndo::decode(&mut &bytes[..]).unwrap();
        if entry.reference.height == 0 {
            assert!(entry.undo.is_none());
            assert_eq!(bytes, [0]);
            assert_eq!(undo, BlockUndo::default());
        } else {
            assert_eq!(undo.txs.len(), entry.block.txs().len() - 1);
            nonempty += usize::from(undo.txs.iter().any(|tx| !tx.coins.is_empty()));
        }
    }
    assert!(nonempty >= 8);
}

#[test]
fn subsidy_matches_recorded_totals_on_both_branches_through_v20() {
    let index = Arc::new(fixture());
    let params = ChainParamsBuilder::new(Network::Regtest, Release::V24_0)
        .arg(ChainOption::Dip3Params, "2:2")
        .arg(ChainOption::TestActivationHeight, "v20@100")
        .build()
        .unwrap();
    let inputs = NodeSubsidyInputs::new(
        index.clone(),
        index.clone(),
        SubsidyParams::from_chain_params(&params, 50 * COIN).unwrap(),
    );
    for entry in &index.0 {
        let parent_total = entry.parent.map_or(0, |id| index.0[id.0 as usize].total_subsidy);
        assert_eq!(
            inputs.subsidy(&entry.reference).unwrap(),
            entry.total_subsidy - parent_total,
            "height {}, hash {}",
            entry.reference.height,
            entry.reference.hash
        );
    }
    assert_eq!(inputs.subsidy(&index.0[99].reference).unwrap(), 500 * COIN);
    assert_eq!(inputs.subsidy(&index.0[100].reference).unwrap(), 5 * COIN);
}

#[test]
fn unavailable_inputs_return_errors_and_genesis_still_has_empty_undo() {
    let mut index = fixture();
    index.0[1].data = None;
    index.0[2].undo = None;
    index.0[3].parent = None;
    index.0[4].data.as_mut().unwrap().file = -1;
    index.0[5].undo.as_mut().unwrap().file = -1;
    let index = Arc::new(index);
    let source = NodeBlockSource::new(index.clone(), files());
    assert!(NodeTxPositions::new(index.clone()).data_pos(&index.0[1].reference).is_err());
    assert!(matches!(source.block(&index.0[1].reference), Err(SubscriberError::Blocks(_))));
    for id in [2, 3] {
        assert!(matches!(source.undo(&index.0[id].reference), Err(SubscriberError::Blocks(_))));
    }
    assert!(matches!(source.block(&index.0[4].reference), Err(SubscriberError::Blocks(_))));
    assert!(matches!(source.undo(&index.0[5].reference), Err(SubscriberError::Blocks(_))));
    assert_eq!(source.undo(&index.0[0].reference).unwrap(), [0]);
}

struct MainnetHeaders;

impl ChainAncestry for MainnetHeaders {
    fn header(&self, id: BlockId) -> HeaderCtx {
        HeaderCtx {
            height: id.0,
            hash: BlockHash::default(),
            time: 0,
            bits: CompactTarget::new(if id.0 == 999_999 {
                0x1923_ae4b
            } else {
                0x1e0f_ffff
            }),
            version: 1,
            mtp: 0,
            chain_work: Arith256::default(),
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

impl DeploymentView for MainnetHeaders {
    fn active_after(&self, _: Option<BlockId>, _: Deployment) -> bool {
        panic!("activation must be queried at the block");
    }

    fn active_at(&self, _: BlockId, deployment: Deployment) -> bool {
        assert_eq!(deployment, Deployment::Buried(BuriedDeployment::V20));
        false
    }
}

#[test]
fn subsidy_uses_the_parent_header_and_injected_network_parameters() {
    let params = ChainParamsBuilder::new(Network::Main, Release::V24_0).build().unwrap();
    let source = Arc::new(MainnetHeaders);
    let inputs = NodeSubsidyInputs::new(
        source.clone(),
        source,
        SubsidyParams::from_chain_params(&params, 50 * COIN).unwrap(),
    );
    let block = BlockRef {
        id: BlockId(1_000_000),
        hash: BlockHash::default(),
        height: 1_000_000,
    };
    // Core: validation.cpp:1820-1881,1895-1899
    assert_eq!(inputs.subsidy(&block).unwrap(), 334_559_821);
    assert_eq!(
        inputs
            .subsidy(&BlockRef {
                id: BlockId(0),
                height: 0,
                ..block
            })
            .unwrap(),
        50 * COIN
    );
}
