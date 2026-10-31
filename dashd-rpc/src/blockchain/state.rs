//! RP1a-2b's chain-state reads over [`NodeChainState`]: `getblockchaininfo`, `getdifficulty`,
//! `getdeploymentinfo`, `getchaintxstats`, `gettxout`, `getbestchainlock`, `getblockheaders` and
//! `getblockhashes`.

use std::sync::Arc;

use dash_consensus::coins::Coin;
use dash_consensus::deployments::info::{Bip9Info, DeploymentInfo};
use dash_consensus::primitives::OutPoint;
use dash_primitives::BlockHash;

use super::{ABORTED, HeaderFacts, blockheader_to_json, difficulty, no_chainman, parse_hash_v};
use crate::UniValue;
use crate::core_write::script_to_univ;
use crate::descriptor::hex;
use crate::key_io::KeyIoParams;
use crate::protocol::{
    RPC_DATABASE_ERROR, RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER,
    RPC_MISC_ERROR, RpcError,
};
use crate::request::{get_int_i32, get_int_u32, param};
use crate::server::{Command, RpcServer};
use crate::warnings::NodeWarnings;

/// The chain state the commands read: the same `NodeContext::chainman` as
/// [`NodeChain`](super::NodeChain), through a seam of its own. `dashd-node` implements it.
pub trait NodeChainState: Send + Sync {
    /// `None` before the chainstate loads and after shutdown, as
    /// [`NodeChain::chainman`](super::NodeChain::chainman).
    fn chain_state(&self) -> Option<Box<dyn ChainState>>;
}

/// The reads, each one `cs_main` scope (one V turn) unless it says otherwise. A read that needs
/// V fails with [`Aborted`] once an `AbortNode` has stopped the chainstate, and every command
/// answers that as D9's error.
pub trait ChainState {
    /// `getblockchaininfo`'s facts of the active tip and the best header.
    fn tip_info(&self) -> Result<TipInfo, Aborted>;

    /// `m_blockman.CalculateCurrentUsage()`: the bytes of every blk and rev file.
    fn size_on_disk(&self) -> u64;

    /// `DeploymentInfo(blockindex, GetSignalsStage(blockindex))` of `hash`, or of the active tip
    /// for `None`; `None` for a block the index doesn't have.
    fn deployment_info(
        &self,
        hash: Option<&BlockHash>,
    ) -> Result<Option<DeploymentReport>, DeploymentError>;

    /// `getchaintxstats`' final block: `hash`, which must be on the active chain, or the active
    /// tip for `None`.
    fn chain_tx_final(&self, hash: Option<&BlockHash>) -> Result<ChainTxPoint, ChainTxError>;

    /// `GetAncestor(height)` of `of`, which [`Self::chain_tx_final`] returned.
    fn chain_tx_ancestor(
        &self,
        of: &BlockHash,
        height: u32,
    ) -> Result<Option<ChainTxPoint>, Aborted>;

    /// `CoinsTip().GetCoin(out)` (which caches the coin it reads) and `GetBestBlock()`'s entry.
    /// A failed coins read is [`Aborted`] too: Core's error catcher aborts on it.
    fn txout(&self, out: &OutPoint) -> Result<Option<UnspentCoin>, Aborted>;

    /// `chainlocks->GetBestChainLock()`, and whether the index has its block; `None` is Core's
    /// null sig.
    fn best_chainlock(&self) -> Option<BestChainLock>;

    /// `LookupBlockIndex(hash) != nullptr`.
    fn has_block(&self, hash: &BlockHash) -> bool;

    /// `hash`'s facts and then its active successors' (`active_chain.Next`), `count` at most;
    /// one entry for a block off the active chain, none for a block the index doesn't have.
    fn headers_from(&self, hash: &BlockHash, count: usize) -> Result<Vec<HeaderFacts>, Aborted>;

    /// `GetNetworkHashPS`'s walk: the active chain's height, and its block at `height` (the tip
    /// for `None`) followed by up to `back` of that block's ancestors, newest first. No blocks
    /// when `height` is past the tip.
    fn active_window(&self, height: Option<u32>, back: u32) -> Result<ActiveWindow, Aborted>;
}

/// [`ChainState::active_window`]'s answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveWindow {
    /// `active_chain.Height()`.
    pub height: u32,
    pub blocks: Vec<WindowBlock>,
}

/// One block of an [`ActiveWindow`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowBlock {
    pub height: u32,
    /// `nBits`, for `GetDifficulty`.
    pub bits: u32,
    pub time: u32,
    /// `nChainWork`.
    pub chain_work: dash_num::Arith256,
}

/// `getblockchaininfo`'s reads of the tip.
#[derive(Clone, Debug, PartialEq)]
pub struct TipInfo {
    pub height: u32,
    pub hash: BlockHash,
    /// `nBits`, for `GetDifficulty`.
    pub bits: u32,
    pub time: u32,
    pub median_time_past: i64,
    /// `m_best_header->nHeight`.
    pub headers: u32,
    /// `GuessVerificationProgress(TxData(), tip)`.
    pub verification_progress: f64,
    /// `IsInitialBlockDownload()`.
    pub initial_block_download: bool,
    /// `nChainWork.GetHex()`.
    pub chain_work: String,
}

/// `getdeploymentinfo`'s block and its deployments, in Core's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeploymentReport {
    pub hash: BlockHash,
    pub height: u32,
    pub deployments: Vec<DeploymentInfo>,
}

/// Why [`ChainState::deployment_info`] has no report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeploymentError {
    Aborted,
    /// `CMNHFManager::GetForBlock`'s `std::runtime_error`, its text: the EHF signals of a block
    /// in the stage's ancestry are neither stored nor rebuilt from disk (BL-270, RP1a D52).
    EhfSignals(&'static str),
}

impl From<Aborted> for DeploymentError {
    fn from(_: Aborted) -> DeploymentError {
        DeploymentError::Aborted
    }
}

impl From<DeploymentError> for RpcError {
    fn from(e: DeploymentError) -> RpcError {
        match e {
            DeploymentError::Aborted => Aborted.into(),
            DeploymentError::EhfSignals(what) => RpcError::Runtime(what.into()),
        }
    }
}

/// One block of `getchaintxstats`' window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainTxPoint {
    pub hash: BlockHash,
    pub height: u32,
    pub time: u32,
    pub median_time_past: i64,
    /// `nChainTx`.
    pub chain_tx: u32,
}

/// Why `getchaintxstats` has no final block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChainTxError {
    NotFound,
    NotInMainChain,
    Aborted,
}

/// An unspent coin and the coins' best block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnspentCoin {
    pub coin: Coin,
    pub best_block: BlockHash,
    pub best_height: u32,
}

/// An `AbortNode` stopped the chainstate, or its coins database failed a read (RP1a D9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aborted;

impl From<Aborted> for RpcError {
    fn from(_: Aborted) -> RpcError {
        RpcError::new(RPC_DATABASE_ERROR, ABORTED)
    }
}

/// `GetBestChainLock()`'s fields as the command prints them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BestChainLock {
    pub height: i32,
    pub block_hash: BlockHash,
    /// The serialized `CBLSSignature`.
    pub sig: [u8; 96],
    /// `LookupBlockIndex(hash) != nullptr`.
    pub known_block: bool,
}

/// What the commands read of the node's parameters and options.
pub struct ChainStateParams {
    /// `getblockchaininfo.chain`: `GetDevNetName()` with `-devnet`, else `NetworkIDString()`.
    pub chain: String,
    /// `nPowTargetSpacing`, for `getchaintxstats`' default window.
    pub pow_target_spacing: i64,
    /// `IsDeprecatedRPCEnabled("softforks")`.
    pub softforks: bool,
    /// `gettxout`'s addresses.
    pub key_io: KeyIoParams,
    pub warnings: Arc<dyn NodeWarnings>,
}

/// Registers every command of this module on `server`.
pub fn register_chain_state_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChainState>,
    params: ChainStateParams,
) {
    type Handler =
        fn(&dyn NodeChainState, &ChainStateParams, &UniValue) -> Result<UniValue, RpcError>;
    let commands: [(&str, Handler); 8] = [
        ("getblockchaininfo", getblockchaininfo),
        ("getdifficulty", getdifficulty),
        ("getdeploymentinfo", getdeploymentinfo),
        ("getchaintxstats", getchaintxstats),
        ("gettxout", gettxout),
        ("getbestchainlock", getbestchainlock),
        ("getblockheaders", getblockheaders),
        ("getblockhashes", getblockhashes),
    ];
    let params = Arc::new(params);
    for (name, handler) in commands {
        let (c, p) = (Arc::clone(&chain), Arc::clone(&params));
        server.append_command(Command::new(name, move |r| handler(&*c, &p, &r.params)));
    }
}

/// `EnsureChainman`.
fn ensure(chain: &dyn NodeChainState) -> Result<Box<dyn ChainState>, RpcError> {
    chain.chain_state().ok_or_else(no_chainman)
}

fn block_not_found() -> RpcError {
    RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")
}

/// Core's `getblockchaininfo`. Init refuses `-prune` other than 0 (CH8), so `pruned` is always
/// false and the prune fields never print (RP1a D31).
// Core: rpc/blockchain.cpp:1546-1636
fn getblockchaininfo(
    chain: &dyn NodeChainState,
    p: &ChainStateParams,
    _: &UniValue,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    let tip = state.tip_info()?;
    let mut o = UniValue::new_object();
    o.push_kv_end("chain", p.chain.clone().into());
    o.push_kv_end("blocks", i64::from(tip.height).into());
    o.push_kv_end("headers", i64::from(tip.headers).into());
    o.push_kv_end("bestblockhash", tip.hash.to_string().into());
    o.push_kv_end("difficulty", difficulty(tip.bits));
    o.push_kv_end("time", i64::from(tip.time).into());
    o.push_kv_end("mediantime", tip.median_time_past.into());
    o.push_kv_end(
        "verificationprogress",
        UniValue::from_f64(tip.verification_progress).expect("a finite progress"),
    );
    o.push_kv_end("initialblockdownload", tip.initial_block_download.into());
    o.push_kv_end("chainwork", tip.chain_work.into());
    o.push_kv_end("size_on_disk", state.size_on_disk().into());
    o.push_kv_end("pruned", false.into());
    if p.softforks {
        // The tip's own deployments: the tip by hash, so a tip that moved since is not read.
        let report = state.deployment_info(Some(&tip.hash))?.ok_or_else(block_not_found)?;
        o.push_kv_end("softforks", deployments_to_json(&report.deployments));
    }
    o.push_kv_end("warnings", p.warnings.get_warnings(false).into());
    Ok(o)
}

/// Core's `getdifficulty`: the active tip's.
// Core: rpc/blockchain.cpp:462-480
fn getdifficulty(
    chain: &dyn NodeChainState,
    _: &ChainStateParams,
    _: &UniValue,
) -> Result<UniValue, RpcError> {
    Ok(difficulty(ensure(chain)?.tip_info()?.bits))
}

/// Core's `getdeploymentinfo`.
// Core: rpc/blockchain.cpp:1696-1739
fn getdeploymentinfo(
    chain: &dyn NodeChainState,
    _: &ChainStateParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    let hash = match param(params, 0) {
        v if v.is_null() => None,
        v => Some(parse_hash_v(v, "blockhash")?),
    };
    let report = state.deployment_info(hash.as_ref())?.ok_or_else(block_not_found)?;
    let mut o = UniValue::new_object();
    o.push_kv_end("hash", report.hash.to_string().into());
    o.push_kv_end("height", i64::from(report.height).into());
    o.push_kv_end("deployments", deployments_to_json(&report.deployments));
    Ok(o)
}

/// Core's `DeploymentInfo`: one object per enabled deployment, by name, in its order.
// Core: rpc/blockchain.cpp:1445-1544 SoftForkDescPushBack, :1664-1693 DeploymentInfo
fn deployments_to_json(deployments: &[DeploymentInfo]) -> UniValue {
    let mut softforks = UniValue::new_object();
    for d in deployments {
        let mut rv = UniValue::new_object();
        match d {
            DeploymentInfo::Buried(b) => {
                rv.push_kv_end("type", "buried".into());
                rv.push_kv_end("active", b.active.into());
                rv.push_kv_end("height", b.height.into());
            }
            DeploymentInfo::Bip9(b) => {
                rv.push_kv_end("type", "bip9".into());
                if let Some(height) = b.height {
                    rv.push_kv_end("height", height.into());
                }
                rv.push_kv_end("active", b.active.into());
                rv.push_kv_end("bip9", bip9_to_json(b));
            }
        }
        softforks.push_kv_end(d.name(), rv);
    }
    softforks
}

/// A versionbits deployment's `bip9` object.
// Core: rpc/blockchain.cpp:1480-1528
fn bip9_to_json(b: &Bip9Info) -> UniValue {
    let mut bip9 = UniValue::new_object();
    if let Some(bit) = b.bit {
        bip9.push_kv_end("bit", bit.into());
    }
    bip9.push_kv_end("start_time", b.start_time.into());
    bip9.push_kv_end("timeout", b.timeout.into());
    bip9.push_kv_end("ehf", b.ehf.into());
    if let Some(ehf_height) = b.ehf_height {
        bip9.push_kv_end("ehf_height", ehf_height.into());
    }
    if let Some(activation_height) = b.activation_height {
        bip9.push_kv_end("activation_height", activation_height.into());
    }
    bip9.push_kv_end("min_activation_height", b.min_activation_height.into());
    bip9.push_kv_end("status", b.status.name().into());
    bip9.push_kv_end("since", b.since.into());
    bip9.push_kv_end("status_next", b.status_next.name().into());
    if let Some(s) = &b.statistics {
        let mut stats = UniValue::new_object();
        stats.push_kv_end("period", s.period.into());
        stats.push_kv_end("elapsed", s.elapsed.into());
        stats.push_kv_end("count", s.count.into());
        if let Some(threshold) = s.threshold {
            stats.push_kv_end("threshold", threshold.into());
        }
        if let Some(possible) = s.possible {
            stats.push_kv_end("possible", possible.into());
        }
        bip9.push_kv_end("statistics", stats);
    }
    if let Some(signalling) = &b.signalling {
        bip9.push_kv_end("signalling", signalling.clone().into());
    }
    bip9
}

/// Core's `getchaintxstats`. The window's default is one month of blocks, at most the final
/// block's height − 1.
// Core: rpc/blockchain.cpp:2074-2153
fn getchaintxstats(
    chain: &dyn NodeChainState,
    p: &ChainStateParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    let hash = match param(params, 1) {
        v if v.is_null() => None,
        v => Some(parse_hash_v(v, "blockhash")?),
    };
    let last = state.chain_tx_final(hash.as_ref()).map_err(|e| match e {
        ChainTxError::NotFound => block_not_found(),
        ChainTxError::NotInMainChain => {
            RpcError::new(RPC_INVALID_PARAMETER, "Block is not in main chain")
        }
        ChainTxError::Aborted => Aborted.into(),
    })?;
    let height = i64::from(last.height);
    let blockcount = match param(params, 0) {
        v if v.is_null() => {
            // `int` arithmetic: a month over the spacing, as Core's `int` division.
            let month = 30 * 24 * 60 * 60 / p.pow_target_spacing;
            month.min(height - 1).max(0)
        }
        v => {
            let blockcount = i64::from(get_int_i32(v)?);
            if blockcount < 0 || (blockcount > 0 && blockcount >= height) {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Invalid block count: should be between 0 and the block's height - 1",
                ));
            }
            blockcount
        }
    };
    let past_height = u32::try_from(height - blockcount).expect("0 <= blockcount <= height");
    // Core's `CHECK_NONFATAL`: a block the index has keeps its ancestors, so this is unreachable.
    let past = state.chain_tx_ancestor(&last.hash, past_height)?.ok_or_else(|| {
        RpcError::new(RPC_MISC_ERROR, "Internal bug detected: pindex->GetAncestor(...)")
    })?;
    let time_diff = last.median_time_past - past.median_time_past;
    // Core's `int nTxDiff`.
    let tx_diff = last.chain_tx.wrapping_sub(past.chain_tx).cast_signed();
    let mut o = UniValue::new_object();
    o.push_kv_end("time", i64::from(last.time).into());
    o.push_kv_end("txcount", i64::from(last.chain_tx).into());
    o.push_kv_end("window_final_block_hash", last.hash.to_string().into());
    o.push_kv_end("window_final_block_height", height.into());
    o.push_kv_end("window_block_count", blockcount.into());
    if blockcount > 0 {
        o.push_kv_end("window_tx_count", tx_diff.into());
        o.push_kv_end("window_interval", time_diff.into());
        if time_diff > 0 {
            let rate = f64::from(tx_diff) / time_diff as f64;
            o.push_kv_end("txrate", UniValue::from_f64(rate).expect("a finite rate"));
        }
    }
    Ok(o)
}

/// Core's `gettxout`. With no mempool until MP1a, `include_mempool` answers as Core's over an
/// empty one: the coins tip's coin (RP1a D30).
// Core: rpc/blockchain.cpp:1328-1411
fn gettxout(
    chain: &dyn NodeChainState,
    p: &ChainStateParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    let hash = parse_hash_v(param(params, 0), "txid")?;
    let index = get_int_u32(param(params, 1))?;
    let include_mempool = param(params, 2);
    if !include_mempool.is_null() {
        include_mempool.get_bool()?;
    }
    let out = OutPoint {
        hash,
        index,
    };
    let Some(found) = state.txout(&out)? else {
        return Ok(UniValue::Null);
    };
    let coin = &found.coin;
    let mut o = UniValue::new_object();
    o.push_kv_end("bestblock", found.best_block.to_string().into());
    let confirmations = i64::from(found.best_height) - i64::from(coin.height()) + 1;
    o.push_kv_end("confirmations", confirmations.into());
    o.push_kv_end("value", crate::peers::amount(coin.value()));
    o.push_kv_end("scriptPubKey", script_to_univ(coin.script_pubkey(), true, true, &p.key_io));
    o.push_kv_end("coinbase", coin.is_coinbase().into());
    Ok(o)
}

/// Core's `getbestchainlock`. The signature prints as its stored bytes (RP1a D32).
// Core: rpc/blockchain.cpp:251-297
fn getbestchainlock(
    chain: &dyn NodeChainState,
    _: &ChainStateParams,
    _: &UniValue,
) -> Result<UniValue, RpcError> {
    let clsig = ensure(chain)?
        .best_chainlock()
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "Unable to find any ChainLock"))?;
    let mut o = UniValue::new_object();
    o.push_kv_end("blockhash", clsig.block_hash.to_string().into());
    o.push_kv_end("height", clsig.height.into());
    o.push_kv_end("signature", hex(&clsig.sig).into());
    o.push_kv_end("known_block", clsig.known_block.into());
    // `ChainLockSig`'s serialization: `nHeight`, `blockHash`, `sig`.
    let mut bytes = clsig.height.to_le_bytes().to_vec();
    bytes.extend_from_slice(clsig.block_hash.as_bytes());
    bytes.extend_from_slice(&clsig.sig);
    o.push_kv_end("hex", hex(&bytes).into());
    Ok(o)
}

/// `MAX_HEADERS_UNCOMPRESSED_RESULT`.
// Core: net_processing.h:28
const MAX_HEADERS_UNCOMPRESSED_RESULT: i32 = 2000;

/// Core's `getblockheaders`: `count` headers from `blockhash` along the active chain.
// Core: rpc/blockchain.cpp:680-784
fn getblockheaders(
    chain: &dyn NodeChainState,
    _: &ChainStateParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let hash = parse_hash_v(param(params, 0), "blockhash")?;
    let state = ensure(chain)?;
    if !state.has_block(&hash) {
        return Err(block_not_found());
    }
    let count = match param(params, 1) {
        v if v.is_null() => MAX_HEADERS_UNCOMPRESSED_RESULT,
        v => get_int_i32(v)?,
    };
    if count <= 0 || count > MAX_HEADERS_UNCOMPRESSED_RESULT {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Count is out of range"));
    }
    let verbose = match param(params, 2) {
        v if v.is_null() => true,
        v => v.get_bool()?,
    };
    let count = usize::try_from(count).expect("1..=2000");
    let mut headers = UniValue::new_array();
    for facts in state.headers_from(&hash, count)? {
        headers.push_back(if verbose {
            blockheader_to_json(&facts)
        } else {
            hex(&dash_consensus::CoreSerialize::core_serialize(&facts.header)).into()
        });
    }
    Ok(headers)
}

/// Core's `getblockhashes` without `-timestampindex`, which init accepts only off until IX1
/// (RP1a D33).
// Core: rpc/blockchain.cpp:536-576
fn getblockhashes(
    _: &dyn NodeChainState,
    _: &ChainStateParams,
    _: &UniValue,
) -> Result<UniValue, RpcError> {
    Err(RpcError::new(
        RPC_MISC_ERROR,
        "Timestamp index is not enabled. Start with -timestampindex to enable.",
    ))
}

#[cfg(test)]
mod tests;
