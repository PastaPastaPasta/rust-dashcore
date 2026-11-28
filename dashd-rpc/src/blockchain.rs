//! `rpc/blockchain.cpp`'s commands over the chain the node provides ([`NodeChain`]):
//! `getblockhash` (DEC-279), `getblockcount` (O11: the functional-test framework's first call
//! on every node), and RP1a-1's `getbestblockhash`, `getblockheader`, `getchaintips`,
//! `invalidateblock`, `reconsiderblock`, `preciousblock`, `syncwithvalidationinterfacequeue` and
//! `getblock` at verbosity 0 and 1. BL-689a's `getchainlockbyheight`, an `evo` command in Core,
//! reads the same chainman.

use std::sync::Arc;

use dash_consensus::coins::BlockUndo;
use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetStats};
use dash_consensus::fp::Difficulty;
use dash_consensus::primitives::CorePayload;
use dash_consensus::{Block, BlsCtx, Header};
use dash_primitives::{BlockHash, BlockHeader, SpecialPayload, TxHash, TxType};

use crate::UniValue;
use crate::core_write::evo::cbtx_to_json;
use crate::core_write::{TxJsonParams, TxVerbosity, tx_to_univ};
use crate::mining::{StateCatcher, ValidationState};
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY,
    RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError,
};
use crate::request::{get_int_i32, param};
use crate::server::{Command, NodeControl, RpcServer};

/// The chain the commands read and write: Core's `NodeContext::chainman`. `dashd-node`
/// implements it.
///
/// Core reads under `cs_main`, so a call that follows a block write or a tip notification sees
/// that block. An implementation must keep that order: the chain it answers is published before
/// V replies to the write or notifies the tip (`submitblock`, and RP1a's `generate*`, rely on
/// it).
pub trait NodeChain: Send + Sync {
    /// The chain as last published; `None` before the chainstate loads and after shutdown,
    /// Core's null `chainman`.
    fn published(&self) -> Option<Box<dyn PublishedChain>>;

    /// The chainstate manager the commands that write reach (`submitblock`); `None` exactly
    /// when [`published`](Self::published) is.
    fn chainman(&self) -> Option<Box<dyn Chainman>>;

    /// Core's `g_coin_stats_index`: `None` without `-coinstatsindex`.
    fn coinstats(&self) -> Option<Box<dyn CoinStatsView>> {
        None
    }
}

/// Core's `ChainstateManager` as the writing commands use it.
pub trait Chainman {
    /// `m_blockman.LookupBlockIndex(hash)` read under `cs_main`: the entry's status and height
    /// now, header-only entries included, or `None` for a block the index doesn't have.
    fn lookup_block_index(&self, hash: &BlockHash) -> Option<IndexStatus>;

    /// `IsPruneMode()`, and when it holds `ActiveTip()->nHeight` read under the chain lock:
    /// `None` for a node that does not prune, which every node is until CH8 enables `-prune` (BL-733).
    fn prune_tip(&self) -> Option<i32> {
        None
    }

    /// Block-writing RPCs refuse while blocks are imported (DEC-1642).
    fn blocks_imported(&self) -> bool;

    /// `ProcessNewBlock(block, force_processing, &new_block)`, with `catcher` registered as a
    /// validation interface around the call (`RegisterSharedValidationInterface` before,
    /// `UnregisterSharedValidationInterface` after). It returns once the block's own
    /// `ActivateBestChain` has, as Core's does.
    fn process_new_block(
        &self,
        block: Arc<Block>,
        force_processing: bool,
        catcher: &Arc<StateCatcher>,
    ) -> ProcessedBlock;

    /// `getchaintips`' tips, read in one `cs_main` scope: the active tip and every entry off
    /// the active chain with no child off it, in Core's `CompareBlocksByHeight` order (height
    /// descending; Core's tie order is by address, the node's by entry id).
    fn chain_tips(&self) -> Vec<ChainTipInfo>;

    /// `LookupBlockIndex(hash)` and `blockheaderToJSON(Tip(), pindex)`'s facts, read in one
    /// `cs_main` scope; `None` for a block the index doesn't have.
    fn header_facts(&self, hash: &BlockHash) -> Option<HeaderFacts>;

    /// `getblock`'s reads in Core's order: `LookupBlockIndex(hash)` and the header's facts, then
    /// `GetBlockChecked`'s `IsBlockPruned`, in one `cs_main` scope (Core takes two; every outcome
    /// here is one of Core's), then `ReadBlockFromDisk` outside it. The read is always from the
    /// block files, never a cache of connected blocks, and a failed read logs its own line.
    fn read_block(&self, hash: &BlockHash) -> Result<(HeaderFacts, Arc<Block>), BlockReadError>;

    /// The undo read of `blockToJSON` (verbosity 2 and 3) and `GetUndoChecked`: `IsBlockPruned`
    /// under `cs_main`, then `UndoReadFromDisk`, whose failure (no undo position, or a failed
    /// read) logs its own line. `getblock` prints no `fee` or `prevout` for either error.
    ///
    /// # Errors
    ///
    /// [`UndoReadError`]: pruned, or not read.
    fn read_undo(&self, hash: &BlockHash) -> Result<BlockUndo, UndoReadError>;

    /// `getmerkleblocks`' walk, after its lookup and count check: `GetBlockChecked(hash)`, then
    /// for `hash` and its successors on the active chain (none when `hash` is off it), at most
    /// `count` blocks, `ReadBlockFromDisk` and `CMerkleBlock(block, filter)` with the one filter,
    /// which matching updates as its flags say. Returns each serialized `merkleblock` with at
    /// least one matched transaction, in chain order.
    ///
    /// # Errors
    ///
    /// [`BlockReadError`]: `hash`'s `GetBlockChecked` failed, or a later read failed
    /// ([`BlockReadError::NotOnDisk`]).
    fn merkle_blocks(
        &self,
        filter: BloomFilterData,
        hash: &BlockHash,
        count: u32,
    ) -> Result<Vec<Vec<u8>>, BlockReadError>;

    /// `GetBlockSubsidy(pindex, consensus)` for the indexed block `hash`: the genesis block's
    /// coinbase value at height 0; `None` for a block the index doesn't have.
    fn block_subsidy(&self, hash: &BlockHash) -> Option<i64>;

    /// `CVerifyDB().VerifyDB(active_chainstate, ..., level, depth) == SUCCESS` under `cs_main`;
    /// `None` when the chainstate's owner has stopped.
    fn verify_db(&self, level: i32, depth: i32) -> Option<bool>;

    /// `InvalidateBlock`, then `ActivateBestChain` if it succeeded.
    fn invalidate_block(&self, hash: BlockHash) -> Result<(), BlockCommandError>;

    /// `ResetBlockFailureFlags(pindex, ignore_chainlocks)`, then `ActivateBestChain`.
    fn reconsider_block(
        &self,
        hash: BlockHash,
        ignore_chainlocks: bool,
    ) -> Result<(), BlockCommandError>;

    /// `PreciousBlock`.
    fn precious_block(&self, hash: BlockHash) -> Result<(), BlockCommandError>;

    /// `SyncWithValidationInterfaceQueue`: returns once every callback queued before the call
    /// has run.
    fn sync_validation_queue(&self);

    /// `ProcessNewBlockHeaders({header}, state)` from no peer: the state it leaves. An
    /// `AbortNode` stops the chainstate before it answers: [`ValidationState::Error`].
    fn process_new_block_header(&self, header: Header) -> ValidationState;

    /// `getcreditpoolinfo`'s reads in one chain state: the active chain's tip (`None`) or its
    /// block at `height`, `GetCreditPool` of it, and `GetBalanceAt` of its ancestor
    /// `CreditPoolPeriodBlocks()` below it.
    ///
    /// # Errors
    ///
    /// [`CreditPoolReadError`]: no such block, or what Core's reads throw.
    // Core: rpc/blockchain.cpp:1795-1810
    fn credit_pool_info(&self, height: Option<i32>)
    -> Result<CreditPoolFacts, CreditPoolReadError>;

    /// `getchainlockbyheight`'s lookup: the active tip under `cs_main`, then
    /// `CoinbaseChainLockReader(tip).Find(height, height)` over the tip's ancestry, each carrier's
    /// block position read under `cs_main` and its block read outside it; then, under `cs_main`,
    /// whether the carrier is still on the active chain; then `Signed()` under the process flag.
    ///
    /// # Errors
    ///
    /// [`ClLookupError`].
    // Core: rpc/quorums.cpp:1433-1451; chainlock/clsig.cpp:147-246
    fn chainlock_by_height(&self, height: i32) -> Result<HistoricalChainLock, ClLookupError>;

    /// `getassetunlockstatuses`' credit pools in one chain state: with `height`, the pool after
    /// the active chain's block there; without, the pool after the best ChainLock's block (by
    /// height on the tip's ancestry), else after the tip coinbase's ChainLock's block, and the
    /// tip's pool unless that block is the tip.
    ///
    /// # Errors
    ///
    /// [`PoolsError`](crate::rawtransaction::PoolsError).
    // Core: rpc/rawtransaction.cpp:827-876
    fn asset_unlock_pools(
        &self,
        height: Option<i32>,
    ) -> Result<crate::rawtransaction::UnlockPools, crate::rawtransaction::PoolsError>;

    /// `ForceFlushStateToDisk()`: an ALWAYS flush, waited for. A disconnected reply is a stopped
    /// chainstate.
    ///
    /// # Errors
    ///
    /// [`Aborted`]: V stopped before the flush replied.
    // Core: v24.0.2 src/rpc/blockchain.cpp:1238 ForceFlushStateToDisk; validation.cpp:3134-3139
    fn force_flush(&self) -> Result<(), crate::blockchain::state::Aborted> {
        Ok(())
    }

    /// `ForceFlushStateToDisk()`, then `ComputeUTXOStats(hash_type, CoinsDB())` at the coins'
    /// best block, with no index.
    ///
    /// # Errors
    ///
    /// [`TxOutSetError`]: the set cannot be read, or the scan's interruption point fired.
    fn txout_set_info(&self, hash_type: CoinStatsHashType) -> Result<TxOutSetInfo, TxOutSetError>;

    // Core: v24.0.2 src/rpc/blockchain.cpp:108-128 ParseHashOrHeight
    fn active_height(&self) -> i32 {
        -1
    }

    fn active_tip(&self) -> Option<TxOutBlock> {
        u32::try_from(self.active_height()).ok().and_then(|height| self.active_block(height))
    }

    fn active_block(&self, _: u32) -> Option<TxOutBlock> {
        None
    }

    /// The active chain's block at `height`, or the tip it is past, in one read.
    // Core: v24.0.2 src/rpc/blockchain.cpp:108-123 ParseHashOrHeight
    fn block_at_height(&self, height: u32) -> ActiveAtHeight {
        let tip = self.active_height();
        if i64::from(height) > i64::from(tip) {
            return ActiveAtHeight::AfterTip {
                tip,
            };
        }
        match self.active_block(height) {
            Some(block) => ActiveAtHeight::Block(block),
            None => ActiveAtHeight::AfterTip {
                tip,
            },
        }
    }

    fn lookup_block(&self, _: &BlockHash) -> Option<TxOutBlock> {
        None
    }

    /// `dumptxoutset`'s `CreateUTXOSnapshot` into `file`, the temporary file at `temppath` that
    /// becomes `path`: `ForceFlushStateToDisk()`, the `HASH_SERIALIZED` statistics and the
    /// coins' cursor at one best block, its `LOG_TIME_SECONDS` lines around the write, then the
    /// file, closed before this returns.
    ///
    /// # Errors
    ///
    /// [`TxOutSetError`]: the set cannot be read, the interruption point fired, or the file
    /// cannot be written.
    fn dump_txout_set(
        &self,
        file: std::fs::File,
        path: &std::path::Path,
        temppath: &std::path::Path,
    ) -> Result<TxOutSetDump, TxOutSetError>;

    /// `gettxoutproof`'s block and proof, after its argument checks: `block`'s
    /// `LookupBlockIndex`, or with none the active chain's block at the height of the first
    /// unspent output `AccessByTxid` finds over the coins tip for `txids` in order (one `cs_main`
    /// scope); then `ReadBlockFromDisk`, which logs its own failure, the check that the block has
    /// every txid, and the serialized `CMerkleBlock(block, txids)`. `txids` are distinct and in
    /// `std::set<uint256>`'s order.
    ///
    /// # Errors
    ///
    /// [`TxOutProofError`].
    // Core: rpc/txoutproof.cpp:76-122; coins.cpp:344-353 AccessByTxid
    fn txout_proof(
        &self,
        txids: &[TxHash],
        block: Option<BlockHash>,
    ) -> Result<Vec<u8>, TxOutProofError>;

    /// `scantxoutset`'s walk: `ForceFlushStateToDisk()`, then `FindScriptPubKey` for `needles`
    /// over `CoinsDB().Cursor()`, with the active tip read beside the cursor under one
    /// `cs_main`. The walk sets `control`'s progress and reads its abort flag.
    ///
    /// # Errors
    ///
    /// [`TxOutSetError`]: the set cannot be read, or the interruption point fired.
    // Core: rpc/blockchain.cpp:2564-2593, 2756-2774
    fn scan_txout_set(
        &self,
        needles: std::collections::HashSet<Vec<u8>>,
        control: ScanControl,
    ) -> Result<TxOutSetScan, TxOutSetError>;
}

/// Why `gettxoutsetinfo` has no statistics, or `dumptxoutset` no file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxOutSetError {
    /// `ComputeUTXOStats` returned `nullopt`.
    Unreadable,
    /// `rpc_interruption_point` threw: the server stopped running during the scan.
    Interrupted,
    /// `dumptxoutset`'s file could not be written.
    Unwritable,
}

/// The errors both scans share.
// Core: rpc/blockchain.cpp:1320-1322, :3176-3177; rpc/server.cpp:343-346 RpcInterruptionPoint
fn txout_set_error(e: TxOutSetError) -> RpcError {
    match e {
        TxOutSetError::Interrupted => RpcError::new(RPC_CLIENT_NOT_CONNECTED, "Shutting down"),
        TxOutSetError::Unreadable | TxOutSetError::Unwritable => {
            RpcError::new(RPC_INTERNAL_ERROR, "Unable to read UTXO set")
        }
    }
}

/// A numeric height read with the active tip, under one chain lock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveAtHeight {
    /// The active chain's block at that height.
    Block(TxOutBlock),
    /// The height is above `CChain::Height()` (`-1` with no tip).
    AfterTip {
        /// The active tip's height.
        tip: i32,
    },
}

/// A block `ParseHashOrHeight` resolved, with `pprev`'s hash (`None` at height 0).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOutBlock {
    pub hash: BlockHash,
    pub height: u32,
    pub prev: Option<BlockHash>,
}

/// `LookUpStats`' cumulative fields, in duffs.
// Core: v24.0.2 src/index/coinstatsindex.cpp:323-340
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoinStatsSnapshot {
    pub muhash: [u8; 32],
    pub txouts: u64,
    pub bogo_size: u64,
    pub total_amount: i64,
    pub total_unspendable: i64,
    pub prevout_spent: i64,
    pub coinbase: i64,
    pub new_outputs_ex_coinbase: i64,
    pub unspendables_genesis: i64,
    pub unspendables_scripts: i64,
    pub unspendables_unclaimed_rewards: i64,
}

/// `g_coin_stats_index`. A miss and a read failure are both `None`.
// Core: v24.0.2 src/index/coinstatsindex.cpp:304-340; src/index/base.cpp:474-537
pub trait CoinStatsView: Send + Sync {
    fn block_until_synced(&self) -> bool;
    fn best_height(&self) -> u32;
    fn look_up(&self, hash: &BlockHash, height: u32) -> Option<CoinStatsSnapshot>;
}

/// `ComputeUTXOStats`' `CCoinsStats` as `gettxoutsetinfo` reports it without an index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOutSetInfo {
    /// The coins' best block and its height.
    pub best_block: BlockHash,
    pub height: u32,
    pub stats: UtxoSetStats,
    /// `EstimateSize()`.
    pub disk_size: u64,
    /// The copy point's live-copy counters (CH5-5 §2.2), when its flush ran the barrier and the
    /// live-copy seam is on (OD-CH5-27 (b)).
    #[cfg(feature = "test-hooks")]
    pub livecopy_counters: Option<UniValue>,
}

/// Why a block command failed after the lookup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockCommandError {
    /// `LookupBlockIndex` found no entry.
    NotFound,
    /// The state went invalid in an `AbortNode`, which stops the chainstate before the command's
    /// reply (RP1a D9).
    Aborted,
    /// The command's `ActivateBestChain` returned `false`: its `state.ToString()`.
    AbcFailed(String),
    /// The command's `ActivateBestChain` threw: the exception's text.
    AbcThrown(String),
}

/// One tip `getchaintips` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainTipInfo {
    pub height: u32,
    pub hash: BlockHash,
    /// `nBits`, for `GetDifficulty`.
    pub bits: u32,
    /// `nChainWork.GetHex()`.
    pub chain_work: String,
    /// `nHeight - FindFork(tip).nHeight`.
    pub branchlen: u32,
    pub fork_point: BlockHash,
    /// Core's status string: `active`, `invalid`, `conflicting`, `headers-only`, `valid-fork`,
    /// `valid-headers` or `unknown`.
    pub status: &'static str,
}

/// A `CBloomFilter` as its serialization reads it (`getmerkleblocks`' filter).
// Core: common/bloom.h:47-78
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BloomFilterData {
    /// `vData`, the filter bits.
    pub data: Vec<u8>,
    /// `nHashFuncs`.
    pub hash_funcs: u32,
    /// `nTweak`.
    pub tweak: u32,
    /// `nFlags`, the `BLOOM_UPDATE_*` mode.
    pub flags: u8,
}

/// Why `getblock` has no block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockReadError {
    /// The index has no entry: "Block not found".
    NotFound,
    /// `IsBlockPruned`: "Block not available (pruned data)".
    Pruned,
    /// `ReadBlockFromDisk` failed: "Block not found on disk".
    NotOnDisk,
}

/// Why a block's undo data was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoReadError {
    /// `IsBlockPruned`: `GetUndoChecked`'s "Undo data not available (pruned data)".
    Pruned,
    /// `UndoReadFromDisk` failed: `GetUndoChecked`'s "Can't read undo data from disk".
    Unreadable,
}

/// What `blockheaderToJSON` reads of an entry and the tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderFacts {
    pub hash: BlockHash,
    pub header: BlockHeader,
    pub height: u32,
    /// `ComputeNextBlockAndDepth`: the depth below the tip, or −1 off the active chain.
    pub confirmations: i32,
    /// `GetMedianTimePast()`.
    pub median_time_past: i64,
    /// `nChainWork.GetHex()`.
    pub chain_work: String,
    pub n_tx: u32,
    /// The active chain's next block, when the entry is on it below the tip.
    pub next: Option<BlockHash>,
    /// `chainlocks.HasChainLock(nHeight, hash)`.
    pub chainlock: bool,
}

/// The status bits of an index entry the commands read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexStatus {
    /// `IsValid(BLOCK_VALID_SCRIPTS)`.
    pub valid_scripts: bool,
    /// `nStatus & BLOCK_FAILED_MASK`.
    pub failed: bool,
    /// `nStatus & BLOCK_HAVE_DATA`.
    pub have_data: bool,
    /// `nHeight`.
    pub height: u32,
}

/// `ProcessNewBlock`'s outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessedBlock {
    /// Its return: `CheckBlock` and `AcceptBlock` passed.
    pub accepted: bool,
    /// `*new_block`: the block's data was written now.
    pub new_block: bool,
    /// The `EvoDbMissingStateError` text out of `ProcessNewBlock` (dashpay/dash#8098), which
    /// the RPC dispatcher answers as RPC_MISC_ERROR.
    // Core: rpc/server.cpp:540-541 at v24.0.3
    pub thrown: Option<String>,
}

/// One published view of the chain. A command reads it as Core reads `chainman` under
/// `cs_main`: every read sees the same chain.
///
/// RP1a widens this trait for the rest of the file's commands (lookups by hash, entries, tips)
/// rather than adding a second seam; reads that can miss return `Option`, which the command maps
/// to Core's error.
pub trait PublishedChain {
    /// The active chain's `CChain::Height()`: −1 for an empty chain.
    fn height(&self) -> i32;
    /// The hash of the active chain's `CChain::operator[](height)`.
    ///
    /// # Panics
    /// Outside `0 <= height <= self.height()`, which a caller checks first as Core does.
    fn block_hash(&self, height: i32) -> BlockHash;
    /// Core's process flag `bls::bls_legacy_scheme` at this view: the node's committed scheme
    /// (CH7 D9), not the height rule at the tip. It is `!DeploymentActiveAfter(tip, V19)`, which
    /// connecting and disconnecting the activation block flip, except on a node started on an
    /// empty coins view, where it stays legacy until the first block past genesis connects
    /// (RP1a D68). It is never the queried block's own rule.
    // Core: bls/bls.cpp:18, node/chainstate.cpp:413-433, evo/specialtxman.cpp:1043-1046, :1068-1071
    fn bls(&self) -> BlsCtx;
}

/// Core's `bls::bls_legacy_scheme` now: the published view's, or the flag's initial legacy
/// value before a chain is published.
// Core: bls/bls.cpp bls_legacy_scheme{true}
pub(crate) fn process_bls(chain: &dyn NodeChain) -> BlsCtx {
    chain.published().map_or(BlsCtx::LEGACY, |p| p.bls())
}

/// Registers every command of this module on `server`.
pub fn register_blockchain_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    tx_json: TxJsonParams,
) {
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockcount", move |_| getblockcount(&*c)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getbestblockhash", move |_| getbestblockhash(&*c)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockhash", move |r| getblockhash(&*c, &r.params)));
    let c = Arc::clone(&chain);
    let control = Arc::clone(server.control());
    server.append_command(Command::new("getblock", move |r| {
        getblock(&*c, &*control, &tx_json, &r.params)
    }));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockstats", move |r| {
        stats::getblockstats(&*c, &r.params)
    }));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("pruneblockchain", move |_| stats::pruneblockchain(&*c)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getmerkleblocks", move |r| {
        merkle::getmerkleblocks(&*c, &r.params)
    }));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("verifychain", move |r| verifychain(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockheader", move |r| getblockheader(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getchaintips", move |r| getchaintips(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("preciousblock", move |r| preciousblock(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server
        .append_command(Command::new("invalidateblock", move |r| invalidateblock(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server
        .append_command(Command::new("reconsiderblock", move |r| reconsiderblock(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getcreditpoolinfo", move |r| {
        creditpool::getcreditpoolinfo(&*c, &r.params)
    }));
    let c = Arc::clone(&chain);
    server
        .append_command(Command::new("gettxoutsetinfo", move |r| gettxoutsetinfo(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getchainlockbyheight", move |r| {
        chainlock::getchainlockbyheight(&*c, &r.params)
    }));
    server.append_command(Command::new("syncwithvalidationinterfacequeue", move |_| {
        syncwithvalidationinterfacequeue(&*chain)
    }));
}

/// `EnsureAnyChainman`'s error.
// Core: rpc/server_util.cpp:87-98
pub(crate) fn no_chainman() -> RpcError {
    RpcError::new(RPC_INTERNAL_ERROR, "Node chainman not found")
}

/// `EnsureAnyChainman`, then `ActiveChain()`.
fn ensure_chain(chain: &dyn NodeChain) -> Result<Box<dyn PublishedChain>, RpcError> {
    chain.published().ok_or_else(no_chainman)
}

/// `EnsureAnyChainman`.
pub(crate) fn ensure_chainman(chain: &dyn NodeChain) -> Result<Box<dyn Chainman>, RpcError> {
    chain.chainman().ok_or_else(no_chainman)
}

/// Core's `getblockcount`: the active chain's height.
// Core: rpc/blockchain.cpp:210-229
fn getblockcount(chain: &dyn NodeChain) -> Result<UniValue, RpcError> {
    Ok(ensure_chain(chain)?.height().into())
}

/// Core's `getblockhash`: the active chain's block at `height`.
// Core: rpc/blockchain.cpp:578-605
fn getblockhash(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let active = ensure_chain(chain)?;
    let height = get_int_i32(param(params, 0))?;
    if height < 0 || height > active.height() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Block height out of range"));
    }
    Ok(active.block_hash(height).to_string().into())
}

/// Core's `getbestblockhash`: the active chain's tip.
// Core: rpc/blockchain.cpp:231-249
fn getbestblockhash(chain: &dyn NodeChain) -> Result<UniValue, RpcError> {
    let active = ensure_chain(chain)?;
    Ok(active.block_hash(active.height()).to_string().into())
}

/// Core's `ParseHashV(v, name)`: a 64-digit hex string, read as `uint256S` reads it, as a block
/// hash or a txid.
// Core: rpc/util.cpp:87-95
pub(crate) fn parse_hash_v<H: std::str::FromStr<Err: std::fmt::Debug>>(
    v: &UniValue,
    name: &str,
) -> Result<H, RpcError> {
    let hex = v.get_str()?;
    if hex.len() != 64 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("{name} must be of length 64 (not {}, for '{hex}')", hex.len()),
        ));
    }
    if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("{name} must be hexadecimal string (not '{hex}')"),
        ));
    }
    Ok(hex.parse().expect("64 hex digits are a hash"))
}

/// `UniValue(double)` of Core's `GetDifficulty(pindex)`: `ConvertBitsToDouble(nBits)`.
// Core: rpc/blockchain.cpp:92-97; validation.cpp:1790-1810
pub(crate) fn difficulty(bits: u32) -> UniValue {
    UniValue::from_f64(f64::from_bits(Difficulty::from_bits(bits).to_bits()))
        .expect("a finite difficulty")
}

/// `state.ToString()` for a command the chainstate stopped under: V keeps no state of an
/// `AbortNode` to answer with, so every such command answers this (RP1a D9).
pub const ABORTED: &str = "Error: A fatal internal error occurred";

/// The block RPCs' errors: the lookup's, then the state's `ToString()`, which for an
/// `AbortNode` V does not return (RP1a D9), and an exception out of the command's
/// `ActivateBestChain`, which the server's catch answers.
// Core: rpc/blockchain.cpp:1967, :1974-1976, :1978, :2006, :2017-2019, :2021, :2052, :2061-2063,
// :2065; rpc/server.cpp:540-541 at v24.0.3
fn block_command(result: Result<(), BlockCommandError>) -> Result<UniValue, RpcError> {
    match result {
        Ok(()) => Ok(UniValue::Null),
        Err(BlockCommandError::NotFound) => {
            Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
        }
        Err(BlockCommandError::Aborted) => Err(RpcError::new(RPC_DATABASE_ERROR, ABORTED)),
        Err(BlockCommandError::AbcFailed(state)) => Err(RpcError::new(RPC_DATABASE_ERROR, state)),
        Err(BlockCommandError::AbcThrown(what)) => Err(RpcError::new(RPC_MISC_ERROR, what)),
    }
}

/// Core's `blockheaderToJSON`.
// Core: rpc/blockchain.cpp:136-166
fn blockheader_to_json(f: &HeaderFacts) -> UniValue {
    let h = &f.header;
    let mut o = UniValue::new_object();
    o.push_kv_end("hash", f.hash.to_string().into());
    o.push_kv_end("confirmations", f.confirmations.into());
    o.push_kv_end("height", i64::from(f.height).into());
    o.push_kv_end("version", h.version().into());
    o.push_kv_end("versionHex", format!("{:08x}", h.version()).into());
    o.push_kv_end("merkleroot", h.merkle_root().to_string().into());
    o.push_kv_end("time", i64::from(h.time()).into());
    o.push_kv_end("mediantime", f.median_time_past.into());
    o.push_kv_end("nonce", u64::from(h.nonce()).into());
    o.push_kv_end("bits", format!("{:08x}", h.bits()).into());
    o.push_kv_end("difficulty", difficulty(h.bits()));
    o.push_kv_end("chainwork", f.chain_work.clone().into());
    o.push_kv_end("nTx", u64::from(f.n_tx).into());
    if f.height > 0 {
        o.push_kv_end("previousblockhash", h.prev_hash().to_string().into());
    }
    if let Some(next) = f.next {
        o.push_kv_end("nextblockhash", next.to_string().into());
    }
    o.push_kv_end("chainlock", f.chainlock.into());
    o
}

/// Core's `getblockheader`. Non-verbose is the header's 80 bytes in hex.
// Core: rpc/blockchain.cpp:607-678
fn getblockheader(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "hash")?;
    let verbose = match param(params, 1) {
        v if v.is_null() => true,
        v => v.get_bool()?,
    };
    let facts = ensure_chainman(chain)?
        .header_facts(&hash)
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))?;
    if !verbose {
        let bytes = dash_consensus::CoreSerialize::core_serialize(&facts.header);
        return Ok(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().into());
    }
    Ok(blockheader_to_json(&facts))
}

/// Core's `getblock`: verbosity 0 (hex), 1 (`blockToJSON` with txids), 2 (each transaction's
/// `TxToUniv`, with `fee` from the undo data) and 3 or more (also each input's `prevout`).
// Core: rpc/blockchain.cpp:954-1067 getblock, :786-804 GetBlockChecked, :168-209 blockToJSON
fn getblock(
    chain: &dyn NodeChain,
    control: &dyn NodeControl,
    p: &TxJsonParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    let verbosity = match param(params, 1) {
        v if v.is_null() => 1,
        v if v.is_bool() => i32::from(v.get_bool()?),
        v => get_int_i32(v)?,
    };
    let chainman = ensure_chainman(chain)?;
    let (facts, block) = chainman.read_block(&hash).map_err(block_read_error)?;
    if verbosity <= 0 {
        let bytes = dash_consensus::CoreSerialize::core_serialize(&*block);
        return Ok(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().into());
    }
    let tx_verbosity = match verbosity {
        1 => None,
        2 => Some(TxVerbosity::Details),
        _ => Some(TxVerbosity::DetailsAndPrevout),
    };
    let bls = process_bls(chain);
    block_to_json(&*chainman, control, &facts, &block, tx_verbosity, p, bls)
}

/// `LookupBlockIndex`'s and `GetBlockChecked`'s errors.
// Core: rpc/blockchain.cpp:786-804 GetBlockChecked
fn block_read_error(e: BlockReadError) -> RpcError {
    match e {
        BlockReadError::NotFound => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        BlockReadError::Pruned => {
            RpcError::new(RPC_MISC_ERROR, "Block not available (pruned data)")
        }
        BlockReadError::NotOnDisk => RpcError::new(RPC_MISC_ERROR, "Block not found on disk"),
    }
}

/// `blockToJSON`: the header's fields, the size, and the transactions as txids or, with a
/// detailed verbosity, as `TxToUniv` objects that read the block's undo data where it has any.
/// rdashd has no InstantSend manager yet, so no transaction is `IsLocked` and `instantlock` is
/// the block's `chainlock` (D40). A coinbase payload `GetTxPayload<CCbTx>` reads whole, whatever
/// the coinbase's type, adds `cbTx`. Undo data with fewer entries than the block has spending
/// transactions fails as Core's `vtxundo.at(i - 1)` throws; an entry with fewer coins than its
/// transaction has inputs, where Core's `vprevout[i]` reads out of bounds, fails the same way
/// (D41).
// Core: rpc/blockchain.cpp:168-207
fn block_to_json(
    chainman: &dyn Chainman,
    control: &dyn NodeControl,
    facts: &HeaderFacts,
    block: &Block,
    tx_verbosity: Option<TxVerbosity>,
    p: &TxJsonParams,
    bls: BlsCtx,
) -> Result<UniValue, RpcError> {
    let mut o = blockheader_to_json(facts);
    o.push_kv_end("size", i64::from(block.size()).into());
    let mut txs = UniValue::new_array();
    match tx_verbosity {
        None => {
            for tx in block.txs() {
                txs.push_back(tx.txid().to_string().into());
            }
        }
        Some(verbosity) => {
            let undo = chainman.read_undo(&facts.hash).ok();
            for (i, tx) in block.txs().iter().enumerate() {
                // The coinbase has no undo data.
                let tx_undo = match (i.checked_sub(1), &undo) {
                    (Some(i), Some(undo)) => Some(tx_undo_at(undo, i, tx.raw().inputs().len())?),
                    _ => None,
                };
                let mut obj = tx_to_univ(tx, None, true, tx_undo, verbosity, p, bls)
                    .map_err(|e| e.into_rpc_error(control))?;
                obj.push_kv_end("instantlock", facts.chainlock.into());
                obj.push_kv_end("instantlock_internal", false.into());
                txs.push_back(obj);
            }
        }
    }
    o.push_kv_end("tx", txs);
    if let Some(decoded) =
        block.txs().first().and_then(|cb| coinbase_payload(cb.raw().extra_payload()))
        && let SpecialPayload::CoinbaseCommitment(cbtx) = decoded.payload()
    {
        o.push_kv_end("cbTx", cbtx_to_json(cbtx, decoded.best_cl_signature(), bls));
    }
    Ok(o)
}

/// `blockUndo.vtxundo.at(i)` for a transaction of `inputs` inputs: `vector::at`'s out-of-range
/// error past the end, and the same for an entry with fewer coins than inputs, where Core's
/// `vprevout[i]` reads out of bounds (D41).
pub(crate) fn tx_undo_at(
    undo: &BlockUndo,
    i: usize,
    inputs: usize,
) -> Result<&dash_consensus::coins::TxUndo, RpcError> {
    let entry = undo.txs.get(i).ok_or_else(|| range_check(i, undo.txs.len()))?;
    let coins = entry.coins.len();
    if coins < inputs {
        return Err(range_check(coins, coins));
    }
    Ok(entry)
}

/// The `std::out_of_range` libstdc++'s `vector::at(n)` throws, as the RPC server answers it.
// Core: rpc/server.cpp:528-543 ExecuteCommand's std::exception arm
pub(crate) fn range_check(n: usize, size: usize) -> RpcError {
    RpcError::new(
        RPC_MISC_ERROR,
        format!("vector::_M_range_check: __n (which is {n}) >= this->size() (which is {size})"),
    )
}

/// `GetTxPayload<CCbTx>(vExtraPayload)`: the payload read as a CbTx, if that reads every byte.
// Core: evo/specialtx.h:17-28
fn coinbase_payload(payload: &[u8]) -> Option<CorePayload> {
    if payload.is_empty() {
        return None;
    }
    CorePayload::decode(TxType::CoinbaseCommitment, payload).ok().filter(CorePayload::is_complete)
}

/// Core's `getchaintips`: `count` (default `INT_MAX`) tips at most, of a branch at least
/// `branchlen` (default −1) long.
// Core: rpc/blockchain.cpp:1827-1945
fn getchaintips(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let tips = chainman.chain_tips();
    let mut count_max = match param(params, 0) {
        v if v.is_null() => i32::MAX,
        v => get_int_i32(v)?,
    };
    let branch_min = match param(params, 1) {
        v if v.is_null() => -1,
        v => get_int_i32(v)?,
    };
    let mut res = UniValue::new_array();
    for tip in &tips {
        if i64::from(tip.branchlen) < i64::from(branch_min) {
            continue;
        }
        if count_max < 1 {
            break;
        }
        count_max -= 1;
        let mut o = UniValue::new_object();
        o.push_kv_end("height", i64::from(tip.height).into());
        o.push_kv_end("hash", tip.hash.to_string().into());
        o.push_kv_end("difficulty", difficulty(tip.bits));
        o.push_kv_end("chainwork", tip.chain_work.clone().into());
        o.push_kv_end("branchlen", i64::from(tip.branchlen).into());
        o.push_kv_end("forkpoint", tip.fork_point.to_string().into());
        o.push_kv_end("status", tip.status.into());
        res.push_back(o);
    }
    Ok(res)
}

/// Core's `preciousblock`.
// Core: rpc/blockchain.cpp:1947-1985
fn preciousblock(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    block_command(ensure_chainman(chain)?.precious_block(hash))
}

/// Core's `invalidateblock`.
// Core: rpc/blockchain.cpp:1987-2028
fn invalidateblock(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    block_command(ensure_chainman(chain)?.invalidate_block(hash))
}

/// `DEFAULT_CHECKLEVEL` and `DEFAULT_CHECKBLOCKS`, `verifychain`'s defaults.
// Core: validation.h:89-90
const DEFAULT_CHECKLEVEL: i32 = 3;
const DEFAULT_CHECKBLOCKS: i32 = 6;

/// Core's `verifychain`: the arguments (`getInt<int>`) before `EnsureChainman`, then VerifyDB on
/// the active chainstate; true only for `SUCCESS`. The node runs it at its VerifyDB point (D46).
// Core: rpc/blockchain.cpp:1413-1443
fn verifychain(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let int_or = |i, default| match param(params, i) {
        v if v.is_null() => Ok(default),
        v => get_int_i32(v),
    };
    let level = int_or(0, DEFAULT_CHECKLEVEL)?;
    let depth = int_or(1, DEFAULT_CHECKBLOCKS)?;
    let verified = ensure_chainman(chain)?.verify_db(level, depth);
    Ok(verified.ok_or_else(|| RpcError::new(RPC_DATABASE_ERROR, ABORTED))?.into())
}

/// Core's `reconsiderblock`. Core reaches the chainman before parsing.
// Core: rpc/blockchain.cpp:2030-2072
fn reconsiderblock(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    let ignore_chainlocks = match param(params, 1) {
        v if v.is_null() => false,
        v => v.get_bool()?,
    };
    block_command(chainman.reconsider_block(hash, ignore_chainlocks))
}

/// Core's `ParseHashType`.
// Core: rpc/blockchain.cpp:1137-1148
fn parse_hash_type(name: &str) -> Result<CoinStatsHashType, RpcError> {
    match name {
        "hash_serialized_2" => Ok(CoinStatsHashType::HashSerialized),
        "muhash" => Ok(CoinStatsHashType::MuHash),
        "none" => Ok(CoinStatsHashType::None),
        _ => {
            Err(RpcError::new(RPC_INVALID_PARAMETER, format!("'{name}' is not a valid hash_type")))
        }
    }
}

/// A 32-byte hash in Core's internal order, as `uint256::GetHex()` prints it.
fn get_hex(hash: &[u8; 32]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

/// Core's `ParseHashOrHeight`.
// Core: v24.0.2 src/rpc/blockchain.cpp:108-134
fn parse_hash_or_height(param: &UniValue, chainman: &dyn Chainman) -> Result<TxOutBlock, RpcError> {
    if param.is_num() {
        let height = get_int_i32(param)?;
        if height < 0 {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Target block height {height} is negative"),
            ));
        }
        let Ok(height) = u32::try_from(height) else {
            return Err(txout_set_error(TxOutSetError::Unreadable));
        };
        return match chainman.block_at_height(height) {
            ActiveAtHeight::Block(block) => Ok(block),
            ActiveAtHeight::AfterTip {
                tip,
            } => Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Target block height {height} after current tip {tip}"),
            )),
        };
    }
    let hash = parse_hash_v(param, "hash_or_height")?;
    chainman
        .lookup_block(&hash)
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
}

fn amount_delta(now: i64, prev: i64) -> Result<UniValue, RpcError> {
    now.checked_sub(prev)
        .map(crate::peers::amount)
        .ok_or_else(|| txout_set_error(TxOutSetError::Unreadable))
}

// Core: v24.0.2 src/rpc/blockchain.cpp:1277-1318
fn indexed_txout_set(
    view: &dyn CoinStatsView,
    hash_type: CoinStatsHashType,
    block: &TxOutBlock,
) -> Result<UniValue, RpcError> {
    let unable = || txout_set_error(TxOutSetError::Unreadable);
    let stats = view.look_up(&block.hash, block.height).ok_or_else(unable)?;
    let prev = if block.height == 0 {
        CoinStatsSnapshot::default()
    } else {
        let prev = block.prev.as_ref().ok_or_else(unable)?;
        view.look_up(prev, block.height - 1).ok_or_else(unable)?
    };
    let mut o = UniValue::new_object();
    o.push_kv_end("height", i64::from(block.height).into());
    o.push_kv_end("bestblock", block.hash.to_string().into());
    o.push_kv_end("txouts", stats.txouts.into());
    o.push_kv_end("bogosize", stats.bogo_size.into());
    if hash_type == CoinStatsHashType::MuHash {
        o.push_kv_end("muhash", get_hex(&stats.muhash).into());
    }
    o.push_kv_end("total_amount", crate::peers::amount(stats.total_amount));
    o.push_kv_end("total_unspendable_amount", crate::peers::amount(stats.total_unspendable));
    let mut info = UniValue::new_object();
    info.push_kv_end("prevout_spent", amount_delta(stats.prevout_spent, prev.prevout_spent)?);
    info.push_kv_end("coinbase", amount_delta(stats.coinbase, prev.coinbase)?);
    info.push_kv_end(
        "new_outputs_ex_coinbase",
        amount_delta(stats.new_outputs_ex_coinbase, prev.new_outputs_ex_coinbase)?,
    );
    info.push_kv_end("unspendable", amount_delta(stats.total_unspendable, prev.total_unspendable)?);
    let mut unspendables = UniValue::new_object();
    unspendables.push_kv_end(
        "genesis_block",
        amount_delta(stats.unspendables_genesis, prev.unspendables_genesis)?,
    );
    unspendables.push_kv_end(
        "scripts",
        amount_delta(stats.unspendables_scripts, prev.unspendables_scripts)?,
    );
    unspendables.push_kv_end(
        "unclaimed_rewards",
        amount_delta(stats.unspendables_unclaimed_rewards, prev.unspendables_unclaimed_rewards)?,
    );
    info.push_kv_end("unspendables", unspendables);
    o.push_kv_end("block_info", info);
    Ok(o)
}

/// Core's `gettxoutsetinfo`. Every call flushes the chainstate once, as Core does at
/// `blockchain.cpp:1238`, before `hash_or_height` is checked. A call that scans the coins makes
/// that flush in the scan's own flush-and-snapshot ([`Chainman::txout_set_info`]), so it is
/// still the only one. With no index, `hash_or_height` is still rejected before the scan (RP1a
/// D10).
// Core: v24.0.2 src/rpc/blockchain.cpp:1150-1170 GetUTXOStats, :1175-1326, :1238 ForceFlushStateToDisk
fn gettxoutsetinfo(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash_type = match param(params, 0) {
        v if v.is_null() => CoinStatsHashType::HashSerialized,
        v => parse_hash_type(v.get_str()?)?,
    };
    // Core: v24.0.2 src/rpc/blockchain.cpp:1233
    let index_requested = match param(params, 2) {
        v if v.is_null() => true,
        v => v.get_bool()?,
    };
    let chainman = ensure_chainman(chain)?;
    let index = chain.coinstats();
    let specific = param(params, 1);
    // Core: v24.0.2 src/rpc/blockchain.cpp:1158-1163
    let indexed =
        index_requested && matches!(hash_type, CoinStatsHashType::MuHash | CoinStatsHashType::None);
    let scans = specific.is_null() && !(indexed && index.is_some());
    // Core: v24.0.2 src/rpc/blockchain.cpp:1238
    if !scans {
        chainman.force_flush()?;
    }
    // Core: v24.0.2 src/rpc/blockchain.cpp:1249-1262
    let target = if !specific.is_null() {
        if index.is_none() {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Querying specific block heights requires coinstatsindex",
            ));
        }
        if hash_type == CoinStatsHashType::HashSerialized {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "hash_serialized_2 hash type cannot be queried for a specific block",
            ));
        }
        if !index_requested {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Cannot set use_index to false when querying for a specific block",
            ));
        }
        Some(parse_hash_or_height(specific, chainman.as_ref())?)
    } else if index.is_some() && index_requested {
        chainman.active_tip()
    } else {
        None
    };
    // Core: v24.0.2 src/rpc/blockchain.cpp:1264-1274
    if index_requested
        && let Some(view) = index.as_deref()
        && !view.block_until_synced()
    {
        let height = target.as_ref().map_or(-1, |block| i64::from(block.height));
        let best = view.best_height();
        if height > i64::from(best) {
            if scans {
                chainman.force_flush()?;
            }
            return Err(RpcError::new(
                RPC_INTERNAL_ERROR,
                format!(
                    "Unable to get data because coinstatsindex is still syncing. Current height: {best}"
                ),
            ));
        }
    }
    if indexed && let Some(view) = index.as_deref() {
        let block = target.as_ref().ok_or_else(|| txout_set_error(TxOutSetError::Unreadable))?;
        return indexed_txout_set(view, hash_type, block);
    }
    let info = chainman.txout_set_info(hash_type).map_err(txout_set_error)?;
    let s = &info.stats;
    let mut o = UniValue::new_object();
    o.push_kv_end("height", i64::from(info.height).into());
    o.push_kv_end("bestblock", info.best_block.to_string().into());
    o.push_kv_end("txouts", s.txouts.into());
    o.push_kv_end("bogosize", s.bogosize.into());
    match (hash_type, &s.hash) {
        (CoinStatsHashType::HashSerialized, Some(h)) => {
            o.push_kv_end("hash_serialized_2", get_hex(h).into());
        }
        (CoinStatsHashType::MuHash, Some(h)) => o.push_kv_end("muhash", get_hex(h).into()),
        _ => {}
    }
    // Core's `CHECK_NONFATAL(stats.total_amount.has_value())`.
    let total = s.total_amount.ok_or_else(|| {
        RpcError::new(RPC_MISC_ERROR, "Internal bug detected: stats.total_amount.has_value()")
    })?;
    o.push_kv_end("total_amount", crate::peers::amount(total));
    o.push_kv_end("transactions", s.transactions.into());
    o.push_kv_end("disk_size", info.disk_size.into());
    #[cfg(feature = "test-hooks")]
    if let Some(counters) = info.livecopy_counters {
        o.push_kv_end("livecopy_counters", counters);
    }
    Ok(o)
}

/// Core's `syncwithvalidationinterfacequeue`. Its queue is global in Core; the node's is V's,
/// which outlives every command that can run (none runs before the load).
// Core: rpc/blockchain.cpp:444-460
fn syncwithvalidationinterfacequeue(chain: &dyn NodeChain) -> Result<UniValue, RpcError> {
    if let Some(chainman) = chain.chainman() {
        chainman.sync_validation_queue();
    }
    Ok(UniValue::Null)
}

mod chainlock;
mod creditpool;
mod dump;
mod fetch;
mod merkle;
mod scan;
mod state;
mod stats;
mod wait;

pub use chainlock::{ClLookupError, HistoricalChainLock};
pub use creditpool::{CreditPoolFacts, CreditPoolReadError};
pub use dump::{TxOutSetDump, register_dump_commands};
pub use fetch::register_fetch_commands;
pub use scan::{ScanControl, TxOutSetScan, register_scan_commands};
mod specialtx;
mod txoutproof;

pub use specialtx::register_specialtx_commands;
pub use txoutproof::{TxOutProofError, register_txoutproof_commands};

pub use state::{
    Aborted, ActiveWindow, BestChainLock, ChainState, ChainStateParams, ChainTxError, ChainTxPoint,
    DeploymentError, DeploymentReport, NodeChainState, TipInfo, UnspentCoin, WindowBlock,
    register_chain_state_commands,
};
pub use wait::{BlockChange, UpdatedBlock, register_wait_commands};

#[cfg(test)]
pub(crate) mod tests;
