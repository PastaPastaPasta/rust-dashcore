//! `ChainCommand` (§2.2): V's messages, by Core lock class (§2.4).
//!
//! The enum grows with the commits that implement each command: `Mempool` and `OnV` come with
//! their owners. CH5's VerifyDB runs inside [`ChainCommand::VerifyLoaded`] (CH1-node-LR).

use std::sync::Arc;

use dash_chain::{ChainReader, HeaderBatch, HeaderChain, HeadersResult, PeerId};
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::ctx::{BlockId, Deployment, DeploymentPos, DeploymentView, VersionbitsAnswer};
use dash_consensus::deployments::Deployments;
use dash_consensus::deployments::info::DeploymentInfo;
use dash_consensus::{Block, BlockValidationError};
use dash_dml::creditpool::PoolState;
use dash_dml::{MnList, StoreError};
use dash_num::Hash256;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};

use super::dmn::list_at_or_empty;
use super::flush::{FlushSite, FlushStateMode};
use super::index::NodeBlockIndex;
use super::locks::{ChainLockSig, ClIntakeReply, ClReply, ClSigs, ClTrigger};
use super::reply::ReplySender;
use super::seams::{BlockConnector, EvoReadCx, EvoReads};
use super::verify::{VerifyDbArgs, VerifyDbResult, VerifyLoadError, VerifyLoadOptions};
use crate::coins::{CoinsDump, CoinsScan, CoinsScanJob, CoinsStatsJob};

/// A read with cross-entry consistency, run on V (class R; A1 §2.10.2).
pub struct ChainQuery<'a> {
    pub chain: &'a HeaderChain,
    pub idx: &'a NodeBlockIndex,
    pub cl: &'a ClSigs,
    /// Core `m_blockman.m_have_pruned`.
    pub have_pruned: bool,
    /// Core `ShouldCheckBlockIndex()`: `-checkblockindex` as V runs it.
    // Core: validation.h:961
    pub check_block_index: bool,
    /// V's deployments: CS4 over V's chain with `m_versionbitscache`, so a query reads
    /// `ComputeBlockVersion` and `DeploymentActiveAfter` as the connector does (CreateNewBlock,
    /// MN0-1).
    pub deployments: QueryDeployments<'a>,
    /// V's evo state, read through [`Self::evo_view`].
    pub(super) evo: EvoQuery<'a>,
}

/// [`ChainQuery`]'s evo half (CH7 D17).
pub(super) struct EvoQuery<'a> {
    pub(super) reads: &'a dyn EvoReads,
    pub(super) reader: &'a ChainReader,
    pub(super) tip: Option<BlockId>,
    pub(super) dmn_tip: Option<BlockId>,
}

impl ChainQuery<'_> {
    /// The evo state at V's tip, read in this V turn (CH7 D17): what MN0-2b's template reads
    /// (DEC-612), all from one state.
    pub fn evo_view(&self) -> EvoView<'_> {
        EvoView {
            reads: self.evo.reads,
            cx: EvoReadCx {
                reader: self.evo.reader,
                deployments: &self.deployments,
            },
            tip: self.evo.tip,
            dmn_tip: self.evo.dmn_tip,
        }
    }
}

/// [`ChainQuery::evo_view`]: the lists, the credit pool and the mined commitments at V's tip.
/// Valid for the query's V turn.
pub struct EvoView<'a> {
    reads: &'a dyn EvoReads,
    cx: EvoReadCx<'a>,
    /// V's tip, the parent of the next block; `None` before it has one.
    pub tip: Option<BlockId>,
    /// Core's DMN tip (D16), as `ChainView.dmn_tip`.
    pub dmn_tip: Option<BlockId>,
}

impl EvoView<'_> {
    /// `GetListForBlock(pindexPrev)` for a block on the tip: the list `GetMasternodeTxOuts` pays
    /// from. Core's empty list before the node has a tip.
    // Core: masternode/payments.cpp:152
    pub fn tip_list(&self) -> Result<Arc<MnList>, StoreError> {
        list_at_or_empty(self.reads, self.cx, self.tip)
    }

    /// `GetListAtChainTip()`: the list at the DMN tip, which the superblock arms read. It equals
    /// [`Self::tip_list`] except before init's `InitializeCurrentBlockTip` and after a
    /// disconnect-only step without a mempool (D16).
    // Core: masternode/payments.cpp:366, :423, :450
    pub fn dmn_list(&self) -> Result<Arc<MnList>, StoreError> {
        list_at_or_empty(self.reads, self.cx, self.dmn_tip)
    }

    /// `GetCreditPool(Tip())`: `None` before a tip, or unless V20 is active after it.
    // Core: evo/creditpool.cpp:256, node/miner.cpp:520
    pub fn credit_pool(&self) -> Result<Option<Arc<PoolState>>, StoreError> {
        match self.tip {
            Some(tip) => self.reads.credit_pool_after(self.cx, tip),
            None => Ok(None),
        }
    }

    /// `HasMinedCommitment(llmqType, quorumHash)` on the active chain; `false` before a tip.
    // Core: llmq/blockprocessor.cpp:644-647
    pub fn has_mined_commitment(
        &self,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
    ) -> Result<bool, StoreError> {
        match self.tip {
            Some(tip) => self.reads.has_mined_commitment(self.cx, llmq_type, quorum_hash, tip),
            None => Ok(false),
        }
    }
}

/// [`ChainQuery::deployments`]: V's deployment answers without the cache behind them, so a read
/// can fill `m_versionbitscache` as Core's `CreateNewBlock` does but never clear it.
#[derive(Clone, Copy)]
pub struct QueryDeployments<'a>(&'a Deployments<'a>);

impl<'a> QueryDeployments<'a> {
    pub(super) fn new(deployments: &'a Deployments<'a>) -> Self {
        Self(deployments)
    }

    /// `ComputeBlockVersion(pindexPrev)` for the block after `prev`.
    pub fn compute_block_version(&self, prev: Option<BlockId>) -> i32 {
        self.0.compute_block_version(prev)
    }

    /// `DeploymentInfo(block, GetSignalsStage(block))`: `getdeploymentinfo`'s entries.
    pub fn deployment_info(&self, block: BlockId) -> Vec<DeploymentInfo> {
        self.0.deployment_info(block)
    }
}

impl DeploymentView for QueryDeployments<'_> {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        self.0.active_after(prev, deployment)
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.0.active_at(block, deployment)
    }

    fn versionbits(&self, prev: Option<BlockId>, pos: DeploymentPos) -> Option<VersionbitsAnswer> {
        self.0.versionbits(prev, pos)
    }
}

/// The coins side V takes once the coins open, after `LoadGenesisBlock` (D51): Core's
/// `CanFlushToDisk()` turns true (`validation.cpp:4859-4861`).
pub struct AttachCoins {
    pub connector: Box<dyn BlockConnector>,
    pub flush: Box<dyn FlushSite>,
}

/// Core's `BlockValidationState`: valid, invalid with a result class, or a system error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockState {
    Valid,
    Invalid(BlockValidationError),
    /// `state.Error(msg)`.
    Error(String),
}

impl BlockState {
    pub fn is_valid(&self) -> bool {
        matches!(self, BlockState::Valid)
    }
}

// Core: consensus/validation.h:105 ValidationState::ToString
impl std::fmt::Display for BlockState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockState::Valid => f.write_str("Valid"),
            BlockState::Invalid(e) => e.fmt(f),
            BlockState::Error(msg) => f.write_str(msg),
        }
    }
}

/// The block RPCs' lookup failure (`rpc/blockchain.cpp:1966-1972`, `:2000-2006`, `:2049-2055`):
/// `RPC_INVALID_ADDRESS_OR_KEY`, before any other work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Block not found")]
pub struct BlockNotFound;

/// A proposal `getblocktemplate` answers before `TestBlockValidity`, under the same `cs_main`
/// hold (`rpc/mining.cpp:707-723`), each displayed as Core's string. V answers them before the
/// connector, which only sees a block whose parent is V's tip (CH3a D123).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NotTested {
    /// GBT's lookup: the block is indexed and valid up to its scripts (`rpc/mining.cpp:710-711`).
    #[error("duplicate")]
    Duplicate,
    /// GBT's lookup: the block is indexed and failed (`rpc/mining.cpp:712-713`).
    #[error("duplicate-invalid")]
    DuplicateInvalid,
    /// GBT's lookup: the block is indexed, neither (`rpc/mining.cpp:714`).
    #[error("duplicate-inconclusive")]
    DuplicateInconclusive,
    /// The block's parent is not the tip (`rpc/mining.cpp:717-720`).
    #[error("inconclusive-not-best-prevblk")]
    NotBestPrev,
}

/// The reply of `invalidateblock`, `reconsiderblock` and `preciousblock`. Their other failures
/// follow an `AbortNode`, which stops V, so the reply disconnects instead.
pub type BlockRpcReply = ReplySender<Result<(), BlockNotFound>>;

/// `ProcessNewBlock`'s outcome (§2.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockResult {
    /// `*new_block`: the block's data was written now.
    pub new_block: bool,
    /// The state Core passes to `BlockChecked` on failure (`validation.cpp:4897`).
    pub state: BlockState,
    /// `ProcessNewBlock`'s return before its ABC: `CheckBlock` and `AcceptBlock` passed.
    pub accepted: bool,
}

impl BlockResult {
    /// A failed `CheckBlock` or `AcceptBlock`: no write, not accepted.
    pub fn rejected(state: BlockState) -> BlockResult {
        BlockResult {
            new_block: false,
            state,
            accepted: false,
        }
    }
}

/// How V resolved an InstantSend lock's block conflicts (§5.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsResolution {
    /// No block holds a conflicting transaction.
    NoConflict,
    /// A conflict is ChainLocked: the lock was removed, no block was invalidated (`is:601-606`).
    LockRemoved,
    /// The conflicting blocks were invalidated, then reset, or activated past with
    /// `ActivateBestChain` when the lock's transaction is known (`is:608-641`).
    Invalidated {
        activated: bool,
    },
}

/// Which completion a `ProcessBlock` caller waits for (rev2, R2-M1).
pub enum BlockReply {
    /// Core's `ProcessNewBlock` return: after the block's own `ActivateBestChain`
    /// (`validation.cpp:4905-4912`). Every Core caller's choice (OH-m4). C-dependent.
    Activated(ReplySender<BlockResult>),
    /// Sent when the accept scope ends, before the ABC tail runs. R/M-complete; no Core caller.
    Accepted(ReplySender<BlockResult>),
}

/// V's commands.
pub enum ChainCommand {
    // ---- class M: Core runs these under cs_main only
    /// Core `ProcessNewBlockHeaders` (P2a R7).
    ProcessHeaders {
        batch: HeaderBatch,
        from: Option<PeerId>,
        reply: ReplySender<HeadersResult>,
    },
    /// Core `ProcessNewBlock`: the accept half is class M, its ABC tail class C.
    ProcessBlock {
        block: Arc<Block>,
        hash: BlockHash,
        force: bool,
        from: Option<PeerId>,
        /// The caller checked the merkle root (`IsBlockMutated`, `v:4440-4463`): Core's
        /// `m_checked_merkle_root`, so `CheckBlock` skips that pass (`v:4332`, `v:4368`; A-4b).
        merkle_checked: bool,
        reply: Option<BlockReply>,
    },
    /// `fImporting` / `fReindex` (CH5).
    SetLoading {
        importing: Option<bool>,
        reindex: Option<bool>,
    },
    /// The coins, attached once, before V's first class C operation (D51).
    AttachCoins {
        coins: AttachCoins,
        reply: ReplySender<()>,
    },
    /// `reconsiderblock` (`rpc:2058-2063`): `ResetBlockFailureFlags` is class M, its ABC tail
    /// class C.
    ReconsiderBlock {
        hash: BlockHash,
        ignore_chainlocks: bool,
        reply: BlockRpcReply,
    },
    /// Core `PreciousBlock` (`validation.cpp:3864-3893`): its `cs_main` scope is class M, its ABC
    /// tail class C.
    PreciousBlock {
        hash: BlockHash,
        reply: BlockRpcReply,
    },
    /// `ProcessNewChainLock`'s `CheckActiveState` (`h:136`), before any early return (§5.1).
    ClIntake {
        reply: ReplySender<ClIntakeReply>,
    },
    /// `UpdateBestChainlock` for a lock LK1a has verified (§5.1). `from` is the peer, `None` for
    /// a coinbase lock.
    RecordChainLock {
        sig: Arc<ChainLockSig>,
        sig_hash: Hash256,
        from: Option<PeerId>,
        reply: ReplySender<ClReply>,
    },
    /// Core `VerifyLoadedChainstate` (CH1-node-LR, D64): after the load and the coins' attach,
    /// before init's ABC.
    VerifyLoaded {
        options: VerifyLoadOptions,
        reply: ReplySender<Result<(), VerifyLoadError>>,
    },
    /// Core's startup `InitializeCurrentBlockTip(tip, ibd)` (CH7 §6.3 step 6), sent once init's
    /// block import, its `ActivateBestChain` and (with MP1) the mempool load are done, with
    /// `fImporting` cleared: V reads its tip and `IsInitialBlockDownload()` then, moves the DMN
    /// tip to the tip, publishes the view, then runs the listeners (D16). After V stops on an
    /// `AbortNode` or a shutdown, V's reaper answers it the same way with V's tip, but
    /// publishes no view (CH7-notes D72).
    InitializeCurrentBlockTip {
        reply: ReplySender<()>,
    },
    /// Core `CVerifyDB::VerifyDB` on the active chainstate under `cs_main`, as `verifychain`
    /// calls it (`rpc/blockchain.cpp:1436-1440`): the load path's VerifyDB point.
    VerifyDb {
        args: VerifyDbArgs,
        reply: ReplySender<VerifyDbResult>,
    },
    /// Core `TestBlockValidity(…, block, Tip(), fCheckPOW, fCheckMerkleRoot)` under `cs_main`
    /// (`validation.cpp:4929-4980`; CH3 §2.14), as `getblocktemplate` proposal mode
    /// (`rpc/mining.cpp:717-723`) and `CreateNewBlock` (`node/miner.cpp:339`) call it: on the
    /// tip only, nothing committed or inserted. The reply is Core's `BlockValidationState`, or
    /// [`NotTested`]: GBT's duplicate answers when `lookup`, then a parent that is not the tip
    /// (CH3a D123).
    TestBlockValidity {
        block: Arc<Block>,
        check_pow: bool,
        check_merkle: bool,
        /// GBT's duplicate lookup first (`rpc/mining.cpp:707-715`); `CreateNewBlock` makes none.
        lookup: bool,
        reply: ReplySender<Result<BlockState, NotTested>>,
    },
    /// Core `FlushStateToDisk(state, mode)` from a caller with no tip step, under `cs_main`
    /// (`validation.cpp:3024-3029` at the pin): [`FlushSite::flush`](super::FlushSite::flush)
    /// (CH1-node §2.4, D45; DEC-525). The reply comes once that returns: after the write for
    /// ALWAYS, while W may still be writing what IF_NEEDED or PERIODIC decided (CH2's
    /// asynchronous writes, as at V's tip sites), where Core's returns after every write. Its
    /// failure follows an `AbortNode`, which stops V, so the reply disconnects instead. The
    /// shutdown flushes run on V's [`VExit`](super::VExit) instead, after V stops (BL-271,
    /// DEC-613); `pruneblockchain`'s manual height comes with that RPC's slice.
    Flush {
        mode: FlushStateMode,
        reply: ReplySender<()>,
    },
    /// `gettxoutsetinfo` with no index (`rpc:1212-1220`, RP1a D2), and `dumptxoutset`'s
    /// `CreateUTXOSnapshot` with a `dump` (`rpc:3167-3180`): `ForceFlushStateToDisk()` and the
    /// coins' snapshot under one `cs_main` scope, so the scan reads the coins at the tip it
    /// flushed. The reply is the scan, started, or `None` with no coins database. A
    /// failed flush is an `AbortNode`, which stops V, so the reply disconnects instead.
    CoinsStats {
        hash_type: CoinStatsHashType,
        /// Core's `rpc_interruption_point`, checked at each coin.
        interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
        /// `dumptxoutset`'s file, written from the same snapshot.
        dump: Option<CoinsDump>,
        reply: ReplySender<Option<CoinsStatsJob>>,
    },
    /// `scantxoutset`'s `ForceFlushStateToDisk()` and `CoinsDB().Cursor()` under one `cs_main`
    /// scope (`rpc:2766-2774`): as [`CoinsStats`](Self::CoinsStats), the walk started on the
    /// coins at the tip it flushed, or `None` with no coins database.
    CoinsScan {
        scan: CoinsScan,
        reply: ReplySender<Option<CoinsScanJob>>,
    },
    // ---- class C: Core takes m_chainstate_mutex
    /// Core `ActivateBestChain(state)` with no block (init, the RPCs). Its `false` follows an
    /// `AbortNode`, which V has stopped on where it arose (CH3b D213): the reply is `Err` with
    /// Core's `state.ToString()`, which `ThreadImport`'s second `AbortNode` shows (BL-472). One of
    /// Core's asserts ends the process instead, and the reply disconnects.
    ActivateBestChain {
        reply: Option<ReplySender<Result<(), String>>>,
    },
    /// `invalidateblock` (`rpc:2010-2018`): Core `InvalidateBlock`, then `ActivateBestChain`.
    InvalidateBlock {
        hash: BlockHash,
        reply: BlockRpcReply,
    },
    /// `NetInstantSend::ResolveBlockConflicts` for the lock `lock` (`is:583-646`, §5.6, D42).
    IsConflictInvalidate {
        lock: Hash256,
        /// The lock's `islock.txid`, for Core's log line.
        txid: Hash256,
        reply: ReplySender<IsResolution>,
    },
    /// One of the ChainLock handler's scheduler tasks (`h:63-72`, `h:170-176`, `h:196-205`,
    /// §5.2).
    ClEnforce {
        source: ClTrigger,
    },
    // ---- class R
    Query(Box<dyn FnOnce(&ChainQuery<'_>) + Send>),
    // ---- control
    Shutdown,
    /// A class C operation running `f` on V (tests drive release points and waits by hand).
    #[cfg(test)]
    #[expect(private_interfaces, reason = "test-only")]
    TestOnV(super::actor::tests::OnV),
    /// A class M command running `f` on V, which a release point serves as it would Core's other
    /// threads' `cs_main` work (tests raise a release-point command's failure, CH3b D213).
    #[cfg(test)]
    #[expect(private_interfaces, reason = "test-only")]
    TestUnderMain(super::actor::tests::OnV),
}

/// The Core lock a command's first scope takes (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// `cs_main` (read).
    R,
    /// `cs_main`.
    M,
    /// `m_chainstate_mutex`.
    C,
    /// Shutdown.
    Control,
}

impl ChainCommand {
    pub fn class(&self) -> Class {
        match self {
            ChainCommand::ProcessHeaders {
                ..
            }
            | ChainCommand::ProcessBlock {
                ..
            }
            | ChainCommand::SetLoading {
                ..
            }
            | ChainCommand::AttachCoins {
                ..
            }
            | ChainCommand::ReconsiderBlock {
                ..
            }
            | ChainCommand::PreciousBlock {
                ..
            }
            | ChainCommand::ClIntake {
                ..
            }
            | ChainCommand::RecordChainLock {
                ..
            }
            | ChainCommand::VerifyLoaded {
                ..
            }
            | ChainCommand::InitializeCurrentBlockTip {
                ..
            }
            | ChainCommand::VerifyDb {
                ..
            }
            | ChainCommand::TestBlockValidity {
                ..
            }
            | ChainCommand::Flush {
                ..
            }
            | ChainCommand::CoinsStats {
                ..
            }
            | ChainCommand::CoinsScan {
                ..
            } => Class::M,
            ChainCommand::ActivateBestChain {
                ..
            }
            | ChainCommand::InvalidateBlock {
                ..
            }
            | ChainCommand::IsConflictInvalidate {
                ..
            }
            | ChainCommand::ClEnforce {
                ..
            } => Class::C,
            ChainCommand::Query(_) => Class::R,
            ChainCommand::Shutdown => Class::Control,
            #[cfg(test)]
            ChainCommand::TestOnV(_) => Class::C,
            #[cfg(test)]
            ChainCommand::TestUnderMain(_) => Class::M,
        }
    }

    /// The commands V serves while it holds the rest until startup's ABC completes after
    /// DEC-196 (2) (D38): that ABC, the coins it needs (D51), the step before it in Core's init
    /// (`VerifyLoadedChainstate`, D64), the loading flags its driver sets, and control. A startup
    /// driver must not wait on any other command before it sends that ABC.
    pub fn exempt_from_hold(&self) -> bool {
        self.class() == Class::Control
            || matches!(
                self,
                ChainCommand::ActivateBestChain { .. }
                    | ChainCommand::AttachCoins { .. }
                    | ChainCommand::VerifyLoaded { .. }
                    | ChainCommand::SetLoading { .. }
            )
    }
}
