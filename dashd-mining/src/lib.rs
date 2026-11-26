//! The node's block template source and regtest miner (MN0): Core's
//! `BlockAssembler::CreateNewBlock` behind [`BlockTemplateSource`], and `GenerateBlock`'s
//! proof-of-work loop ([`grind`]). The `generate*` RPCs (RP1a) are thin wrappers over the two.
//!
//! The template is the coinbase, from DIP0003 the mineable commitments, then the mempool's
//! packages as `addPackageTxs` selects them under the Dash package rules (MN1-2, MN1-3, MN1-4).
//! From DIP0003 [`evo`] assembles it over V's masternode list, its ChainLocks and its mineable
//! commitments (MN0-2, MN0-3). A height that may trigger a superblock is refused with
//! [`TemplateError::NotReady`] while no governance back end answers (GV1b).

mod assemble;
pub mod evo;
pub mod gbt;
mod grind;
mod held;
pub mod options;
mod package;
mod select;
mod source;
mod tip;
mod turn;
pub mod view;

#[cfg(any(test, feature = "test-support"))]
pub mod testkit;

pub use assemble::TipFacts;
pub use gbt::{GbtEngine, GbtError, GbtReply, GbtRequest, GbtTemplate};
pub use grind::{Grind, grind};
pub use options::{ApplyArgsManOptions, AssemblerOptions, ClampOptions, FeeRate, Options};
pub use package::{
    AcceptedPackage, InstantSendDisabled, InstantSendHooks, PackageInputs, PackageRules, TipReads,
};
#[cfg(any(test, feature = "test-support"))]
pub use select::PassAll;
pub use select::{BlockState, PackageChecks, Selected, Selection, add_package_txs};
pub use source::{ChainTemplateSource, block_version_arg};
pub use view::{EntryId, MiningEntry, MiningMempool, MiningSafety, NodeStatus, PackageView};

use dash_consensus::Block;
use dash_primitives::BlockHash;

/// Core `BlockAssembler(chainstate, node, mempool).CreateNewBlock(scriptPubKeyIn)`
/// (`node/miner.cpp:176-363`) on V's tip at the call, its own `TestBlockValidity` included
/// (`:351-354`). Blocking; call it from an RPC worker thread, never from V's.
pub trait BlockTemplateSource: Send + Sync {
    /// A template paying the coinbase to `coinbase_script`.
    ///
    /// # Errors
    ///
    /// [`TemplateError`]: a block this build cannot assemble yet, Core's `CreateNewBlock`
    /// exceptions, or V gone.
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        txs: TemplateTxs,
    ) -> Result<BlockTemplate, TemplateError>;

    /// `BlockAssembler::m_last_block_num_txs` and `m_last_block_size` (`node/miner.cpp:259-260`),
    /// which every assembly sets, a failed `TestBlockValidity` included; `None` before the first
    /// (`getmininginfo` omits `currentblocktx` and `currentblocksize`, `rpc/mining.cpp:485-486`).
    fn last_block_stats(&self) -> Option<LastBlockStats>;
}

/// The mempool `BlockAssembler` gets: `generateBlocks` passes the node's (`rpc/mining.cpp:173`),
/// `generateblock` none (`:380`). A source without a mempool attached selects nothing either
/// way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateTxs {
    Mempool,
    None,
}

/// Core's `CBlockTemplate` (`node/miner.h:48-56`).
pub struct BlockTemplate {
    /// The coinbase first; `nNonce` 0 and `hashMerkleRoot` null ([`grind`] sets it).
    pub block: Block,
    /// The tip it was built on.
    pub prev: BlockHash,
    pub height: u32,
    /// Fees for each transaction, including the coinbase's negative total at index zero.
    pub tx_fees: Vec<i64>,
    /// Legacy signature-operation counts for each transaction.
    pub tx_sigops: Vec<i64>,
    /// Compact target of the previous block.
    pub prev_bits: u32,
    /// Masternode payment outputs.
    pub masternode_payments: Vec<dash_consensus::primitives::TxOut>,
    /// Superblock payment outputs.
    pub superblock_payments: Vec<dash_consensus::primitives::TxOut>,
}

/// Core's name for [`BlockTemplate`].
pub type CBlockTemplate = BlockTemplate;

/// `BlockAssembler::m_last_block_num_txs` and `m_last_block_size`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LastBlockStats {
    /// `nBlockTx`: the selected transactions, the coinbase not counted.
    pub num_txs: u64,
    /// `nBlockSize`: 1000 reserved for the coinbase, plus the selected transactions.
    pub size: u64,
}

/// Why [`BlockTemplateSource::create_new_block`] made no template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateError {
    /// A block Core would assemble from state this build does not assemble from yet: a height
    /// that may trigger a superblock with no governance answer ([`evo::Superblock`]), or a read
    /// V refuses as not built (a node without evo state or a coins cache, or a capability not
    /// built yet). A recorded divergence (DEC-595 D4).
    NotReady {
        height: u32,
        /// What it lacks: `"superblock"`, or the not-built read's text.
        rule: &'static str,
        /// The slice that lifts the refusal.
        owner: &'static str,
    },
    /// The `std::runtime_error` Core's `CreateNewBlock` throws, its full text: the RPC server
    /// answers it with `RPC_MISC_ERROR` and that text.
    Failed(String),
    /// V is gone: stopped, or shutting down.
    NoChain,
}
