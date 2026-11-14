// SPDX-License-Identifier: MIT

//! Read-only host inputs injected into index data objects.

use std::sync::Arc;

pub use dash_chain::FlatFilePos;
use dash_chain::{BlockRef, IndexAccess};
use dash_consensus::coins::BlockUndo;
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::subsidy::{SubsidyParams, block_subsidy};
use dash_consensus::{Amount, Block, ChainAncestry};
use dash_params::BuriedDeployment;
use dashd_chainstate::chain::BlockFiles;
use dashd_chainstate::subscriber::{BlockSource, SubscriberError};

use crate::IndexError;

/// The block-file position used to construct transaction positions.
pub trait TxPositions: Send + Sync {
    // Core: index/txindex.cpp:63-69
    fn data_pos(&self, block: &BlockRef) -> Result<FlatFilePos, IndexError>;
}

/// Transaction positions from the node's block index.
pub struct NodeTxPositions {
    index: Arc<dyn IndexAccess>,
}

impl NodeTxPositions {
    pub fn new(index: Arc<dyn IndexAccess>) -> Self {
        Self {
            index,
        }
    }
}

impl TxPositions for NodeTxPositions {
    // Core: chain.h:216-225
    fn data_pos(&self, block: &BlockRef) -> Result<FlatFilePos, IndexError> {
        self.index
            .have_data_pos(block.id)
            .ok_or_else(|| IndexError::Io(format!("no block data position for {}", block.hash)))
    }
}

/// The subsidy counted by coinstats, including the genesis reward.
pub trait SubsidyInputs: Send + Sync {
    // Core: index/coinstatsindex.cpp:123-125
    fn subsidy(&self, block: &BlockRef) -> Result<Amount, IndexError>;
}

/// The existing subsidy calculation over the node's ancestry and deployment views.
/// Block references must belong to `chain`, as required by [`ChainAncestry`].
pub struct NodeSubsidyInputs {
    chain: Arc<dyn ChainAncestry + Send>,
    deployments: Arc<dyn DeploymentView + Send>,
    params: SubsidyParams,
}

impl NodeSubsidyInputs {
    pub fn new(
        chain: Arc<dyn ChainAncestry + Send>,
        deployments: Arc<dyn DeploymentView + Send>,
        params: SubsidyParams,
    ) -> Self {
        Self {
            chain,
            deployments,
            params,
        }
    }
}

impl SubsidyInputs for NodeSubsidyInputs {
    // Core: validation.cpp:1895-1899
    fn subsidy(&self, block: &BlockRef) -> Result<Amount, IndexError> {
        let parent = self.chain.prev(block.id).map(|id| self.chain.header(id));
        let v20 = parent.is_some()
            && self.deployments.active_at(block.id, Deployment::Buried(BuriedDeployment::V20));
        block_subsidy(parent.as_ref(), v20, &self.params)
            .map_err(|err| IndexError::Corrupt(format!("subsidy for {}: {err}", block.hash)))
    }
}

/// Subscriber reads over the node's block index and block files.
/// The files are shared for reads; this adapter exposes no write operations.
pub struct NodeBlockSource {
    index: Arc<dyn IndexAccess>,
    files: Arc<dyn BlockFiles + Sync>,
}

impl NodeBlockSource {
    pub fn new(index: Arc<dyn IndexAccess>, files: Arc<dyn BlockFiles + Sync>) -> Self {
        Self {
            index,
            files,
        }
    }
}

impl BlockSource for NodeBlockSource {
    // Core: index/base.cpp:236-242; node/blockstorage.cpp:845-851
    fn block(&self, block: &BlockRef) -> Result<Arc<Block>, SubscriberError> {
        let pos = self.index.have_data_pos(block.id).ok_or_else(|| {
            SubscriberError::Blocks(format!("no block data position for {}", block.hash))
        })?;
        self.files.read_block(pos, &block.hash).map_err(SubscriberError::Blocks)
    }

    fn undo(&self, block: &BlockRef) -> Result<Vec<u8>, SubscriberError> {
        // Core: index/blockfilterindex.cpp:271-277; index/coinstatsindex.cpp:127-130
        let undo = if block.height == 0 {
            BlockUndo::default()
        } else {
            // Core: chain.h:227-236; node/blockstorage.cpp:543-577
            let pos = self.index.undo_pos(block.id).ok_or_else(|| {
                SubscriberError::Blocks(format!("no undo data position for {}", block.hash))
            })?;
            let parent = self
                .index
                .ancestor(block.id, block.height - 1)
                .and_then(|id| self.index.hash(id))
                .ok_or_else(|| SubscriberError::Blocks(format!("no parent for {}", block.hash)))?;
            self.files.read_undo(pos, &parent).map_err(SubscriberError::Blocks)?
        };
        // Core: undo.h:63-68
        let mut bytes = Vec::new();
        undo.write(&mut bytes);
        Ok(bytes)
    }
}
