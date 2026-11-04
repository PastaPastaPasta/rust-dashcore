//! The node's block template source and regtest miner (MN0): Core's
//! `BlockAssembler::CreateNewBlock` behind [`BlockTemplateSource`], and `GenerateBlock`'s
//! proof-of-work loop ([`grind`]). The `generate*` RPCs (RP1a) are thin wrappers over the two.
//!
//! MN0-1 assembles the blocks below the readiness guard's boundary (G1, CH3 §2.17): a coinbase
//! paying the subsidy to the caller's script and nothing else, which is what Core mines there on
//! an empty mempool. Above it a template is refused with [`TemplateError::NotReady`]: [`evo`]
//! assembles those blocks over an empty masternode list (MN0-2a), and the source calls it once
//! connect accepts them (MN0-2b).

mod assemble;
pub mod evo;
mod grind;
mod source;

pub use assemble::TipFacts;
pub use grind::{Grind, grind};
pub use source::{ChainTemplateSource, block_version_arg};

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
/// `generateblock` none (`:380`). Until MP1a the node's mempool is empty, so both assemble the
/// same block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemplateTxs {
    Mempool,
    None,
}

/// Core's `CBlockTemplate`, as far as MN0-1 fills it.
pub struct BlockTemplate {
    /// The coinbase first; `nNonce` 0 and `hashMerkleRoot` null ([`grind`] sets it).
    pub block: Block,
    /// The tip it was built on.
    pub prev: BlockHash,
    pub height: u32,
}

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
    /// The readiness guard's boundary (CH3a's `g1_rule`, CH3 §2.17): the block after the tip has
    /// one of its deployments active, where Core would assemble a CbTx and payments this build
    /// does not yet. A recorded divergence (DEC-595 D4).
    NotReady {
        height: u32,
        /// `g1_rule`'s name for it ("DIP0003", "V20", "MN_RR", "V24" or "EVO_SHARES").
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
