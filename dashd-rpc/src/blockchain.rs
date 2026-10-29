//! `rpc/blockchain.cpp`'s commands over the chain the node provides ([`NodeChain`]):
//! `getblockhash` (DEC-279), `getblockcount` (O11: the functional-test framework's first call
//! on every node), and RP1a-1's `getbestblockhash`, `getblockheader`, `getchaintips`,
//! `invalidateblock`, `reconsiderblock`, `preciousblock`, `syncwithvalidationinterfacequeue` and
//! `getblock` at verbosity 0 and 1.

use std::sync::Arc;

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetStats};
use dash_consensus::fp::Difficulty;
use dash_consensus::{Block, Header};
use dash_primitives::{BlockHash, BlockHeader};

use crate::UniValue;
use crate::mining::{StateCatcher, ValidationState};
use crate::protocol::{
    RPC_CLIENT_NOT_CONNECTED, RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY,
    RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError,
};
use crate::request::{get_int_i32, param};
use crate::server::{Command, RpcServer};

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
}

/// Core's `ChainstateManager` as the writing commands use it.
pub trait Chainman {
    /// `m_blockman.LookupBlockIndex(hash)` read under `cs_main`: the entry's status now, or
    /// `None` for a block the index doesn't have.
    fn lookup_block_index(&self, hash: &BlockHash) -> Option<IndexStatus>;

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

    /// `ForceFlushStateToDisk()`, then `ComputeUTXOStats(hash_type, CoinsDB())` at the coins'
    /// best block, with no index.
    ///
    /// # Errors
    ///
    /// [`TxOutSetError`]: the set cannot be read, or the scan's interruption point fired.
    fn txout_set_info(&self, hash_type: CoinStatsHashType) -> Result<TxOutSetInfo, TxOutSetError>;
}

/// Why `gettxoutsetinfo` has no statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxOutSetError {
    /// `ComputeUTXOStats` returned `nullopt`.
    Unreadable,
    /// `rpc_interruption_point` threw: the server stopped running during the scan.
    Interrupted,
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
}

/// Why a block command failed after the lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockCommandError {
    /// `LookupBlockIndex` found no entry.
    NotFound,
    /// The state went invalid. Every such failure is an `AbortNode`, which stops the chainstate
    /// before the command's reply (RP1a D9).
    Aborted,
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
}

/// `ProcessNewBlock`'s outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessedBlock {
    /// Its return: `CheckBlock` and `AcceptBlock` passed.
    pub accepted: bool,
    /// `*new_block`: the block's data was written now.
    pub new_block: bool,
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
}

/// Registers every command of this module on `server`.
pub fn register_blockchain_commands(server: &RpcServer, chain: Arc<dyn NodeChain>) {
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockcount", move |_| getblockcount(&*c)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getbestblockhash", move |_| getbestblockhash(&*c)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblockhash", move |r| getblockhash(&*c, &r.params)));
    let c = Arc::clone(&chain);
    server.append_command(Command::new("getblock", move |r| getblock(&*c, &r.params)));
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
    server
        .append_command(Command::new("gettxoutsetinfo", move |r| gettxoutsetinfo(&*c, &r.params)));
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

/// Core's `ParseHashV(v, name)`: a 64-digit hex string, read as `uint256S` reads it.
// Core: rpc/util.cpp:87-95
pub(crate) fn parse_hash_v(v: &UniValue, name: &str) -> Result<BlockHash, RpcError> {
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
fn difficulty(bits: u32) -> UniValue {
    UniValue::from_f64(f64::from_bits(Difficulty::from_bits(bits).to_bits()))
        .expect("a finite difficulty")
}

/// `state.ToString()` for a command the chainstate stopped under: V keeps no state of an
/// `AbortNode` to answer with, so every such command answers this (RP1a D9).
pub const ABORTED: &str = "Error: A fatal internal error occurred";

/// The block RPCs' errors: the lookup's, then the state's `ToString()`, which for an
/// `AbortNode` V does not return (RP1a D9).
// Core: rpc/blockchain.cpp:1966-1981, 2000-2023, 2049-2068
fn block_command(result: Result<(), BlockCommandError>) -> Result<UniValue, RpcError> {
    match result {
        Ok(()) => Ok(UniValue::Null),
        Err(BlockCommandError::NotFound) => {
            Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))
        }
        Err(BlockCommandError::Aborted) => Err(RpcError::new(RPC_DATABASE_ERROR, ABORTED)),
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
    o.push_kv_end("version", h.version.into());
    o.push_kv_end("versionHex", format!("{:08x}", h.version).into());
    o.push_kv_end("merkleroot", h.merkle_root.to_string().into());
    o.push_kv_end("time", i64::from(h.time).into());
    o.push_kv_end("mediantime", f.median_time_past.into());
    o.push_kv_end("nonce", u64::from(h.nonce).into());
    o.push_kv_end("bits", format!("{:08x}", h.bits).into());
    o.push_kv_end("difficulty", difficulty(h.bits));
    o.push_kv_end("chainwork", f.chain_work.clone().into());
    o.push_kv_end("nTx", u64::from(f.n_tx).into());
    if f.height > 0 {
        o.push_kv_end("previousblockhash", h.prev_hash.to_string().into());
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

/// Core's `getblock` at verbosity 0 (hex) and 1 (`blockToJSON` with txids). Verbosity 2 and 3
/// (transaction details, with undo for `fee`), and `cbTx` for a coinbase with an extra payload
/// (the BLS signature's text follows the tip's scheme flag), are refused until RP1a-2 (D14): no
/// block rdashd holds below DIP3 has one.
// Core: rpc/blockchain.cpp:954-1067 getblock, :786-804 GetBlockChecked, :168-209 blockToJSON
fn getblock(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    let verbosity = match param(params, 1) {
        v if v.is_null() => 1,
        v if v.is_bool() => i32::from(v.get_bool()?),
        v => get_int_i32(v)?,
    };
    let (facts, block) = ensure_chainman(chain)?.read_block(&hash).map_err(|e| match e {
        BlockReadError::NotFound => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        BlockReadError::Pruned => {
            RpcError::new(RPC_MISC_ERROR, "Block not available (pruned data)")
        }
        BlockReadError::NotOnDisk => RpcError::new(RPC_MISC_ERROR, "Block not found on disk"),
    })?;
    if verbosity <= 0 {
        let bytes = dash_consensus::CoreSerialize::core_serialize(&*block);
        return Ok(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>().into());
    }
    let not_served = |what: &str| {
        RpcError::new(RPC_INTERNAL_ERROR, format!("getblock: {what} is not served until RP1a-2"))
    };
    if verbosity >= 2 {
        return Err(not_served("verbosity 2 and 3"));
    }
    if block.txs().first().is_some_and(|cb| !cb.raw().extra_payload.is_empty()) {
        return Err(not_served("cbTx"));
    }
    let mut o = blockheader_to_json(&facts);
    o.push_kv_end("size", i64::from(block.size()).into());
    let mut txs = UniValue::new_array();
    for tx in block.txs() {
        txs.push_back(tx.txid().to_string().into());
    }
    o.push_kv_end("tx", txs);
    Ok(o)
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

/// Core's `gettxoutsetinfo` with no coinstats index (IX1 adds it): the coins at their best
/// block only. Core flushes before it rejects `hash_or_height`; with nothing to see of that
/// flush, this node rejects it before the flush and scan (RP1a D10).
// Core: rpc/blockchain.cpp:1175-1326
fn gettxoutsetinfo(chain: &dyn NodeChain, params: &UniValue) -> Result<UniValue, RpcError> {
    let hash_type = match param(params, 0) {
        v if v.is_null() => CoinStatsHashType::HashSerialized,
        v => parse_hash_type(v.get_str()?)?,
    };
    // `use_index`: read for its type check; there is no index to use.
    let use_index = param(params, 2);
    if !use_index.is_null() {
        use_index.get_bool()?;
    }
    let chainman = ensure_chainman(chain)?;
    if !param(params, 1).is_null() {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Querying specific block heights requires coinstatsindex",
        ));
    }
    // Core: rpc/server.cpp:343-346 RpcInterruptionPoint
    let info = chainman.txout_set_info(hash_type).map_err(|e| match e {
        TxOutSetError::Unreadable => RpcError::new(RPC_INTERNAL_ERROR, "Unable to read UTXO set"),
        TxOutSetError::Interrupted => RpcError::new(RPC_CLIENT_NOT_CONNECTED, "Shutting down"),
    })?;
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

mod wait;

pub use wait::{BlockChange, UpdatedBlock, register_wait_commands};

#[cfg(test)]
mod tests;
