//! `getnetworkhashps` and `getmininginfo` (RP1a-4, RP1a D51): reads of the active chain, each in
//! one V turn ([`ChainState::active_window`]), as Core reads them under one `cs_main`.

use std::sync::Arc;

use super::Miner;
use crate::UniValue;
use crate::blockchain::{ChainState, NodeChainState, WindowBlock, difficulty, no_chainman};
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i32, param};
use crate::server::{Command, RpcServer};
use crate::warnings::NodeWarnings;

/// `getnetworkhashps`' `nblocks` default; `height`'s is -1, the tip.
// Core: rpc/mining.cpp:119
const DEFAULT_NBLOCKS: i32 = 120;

/// What the commands read of the node's parameters.
pub struct MiningInfoParams {
    /// `getmininginfo.chain`: `NetworkIDString()`, which is `devnet` on any devnet, where
    /// `getblockchaininfo` prints the devnet's name.
    pub network_id: String,
    /// `DifficultyAdjustmentInterval()`: `nPowTargetTimespan / nPowTargetSpacing`.
    pub difficulty_adjustment_interval: i64,
    pub warnings: Arc<dyn NodeWarnings>,
}

/// Registers `getnetworkhashps` and `getmininginfo` on `server`. `getmininginfo` reads the last
/// assembly from `miner`.
pub fn register_mining_info_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChainState>,
    miner: Arc<dyn Miner>,
    params: MiningInfoParams,
) {
    let params = Arc::new(params);
    let (c, p) = (Arc::clone(&chain), Arc::clone(&params));
    server.append_command(Command::new("getnetworkhashps", move |r| {
        getnetworkhashps(&*c, &p, &r.params)
    }));
    server.append_command(Command::new("getmininginfo", move |_| {
        getmininginfo(&*chain, &*miner, &params)
    }));
}

/// `EnsureAnyChainman`.
fn ensure(chain: &dyn NodeChainState) -> Result<Box<dyn ChainState>, RpcError> {
    chain.chain_state().ok_or_else(no_chainman)
}

fn no_block_at_height() -> RpcError {
    RpcError::new(RPC_INVALID_PARAMETER, "Block does not exist at specified height")
}

/// Core's `getnetworkhashps`.
// Core: rpc/mining.cpp:112-136
fn getnetworkhashps(
    chain: &dyn NodeChainState,
    p: &MiningInfoParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    let lookup = match param(params, 0) {
        v if v.is_null() => DEFAULT_NBLOCKS,
        v => get_int_i32(v)?,
    };
    let height = match param(params, 1) {
        v if v.is_null() => -1,
        v => get_int_i32(v)?,
    };
    get_network_hash_ps(&*state, lookup, height, p.difficulty_adjustment_interval)
}

/// Core's `GetNetworkHashPS`: the argument checks, then the estimate over the active block at
/// `height` (the tip for -1) and its window.
// Core: rpc/mining.cpp:66-110
fn get_network_hash_ps(
    state: &dyn ChainState,
    lookup: i32,
    height: i32,
    interval: i64,
) -> Result<UniValue, RpcError> {
    if lookup < -1 || lookup == 0 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Invalid nblocks. Must be a positive number or -1.",
        ));
    }
    if height < -1 {
        return Err(no_block_at_height());
    }
    // -1's window is at most the interval.
    let back = match lookup {
        -1 => u32::try_from(interval).unwrap_or(u32::MAX),
        n => n.unsigned_abs(),
    };
    let window = state.active_window(u32::try_from(height).ok(), back)?;
    if i64::from(height) > i64::from(window.height) {
        return Err(no_block_at_height());
    }
    Ok(network_hash_ps(&window.blocks, lookup, interval))
}

/// `GetNetworkHashPS`' estimate over `blocks`, a block and at least `lookup` of its ancestors
/// (all of them where it has fewer), newest first: Core's `int` 0 for the genesis block and for
/// a window whose times are all equal, else the chain-work delta's `getdouble()` per second.
// Core: rpc/mining.cpp:81-110
fn network_hash_ps(blocks: &[WindowBlock], lookup: i32, interval: i64) -> UniValue {
    let Some(pb) = blocks.first().filter(|pb| pb.height != 0) else {
        return 0.into();
    };
    let height = i64::from(pb.height);
    let lookup = match lookup {
        // Blocks since the last difficulty change.
        -1 => height % interval + 1,
        n => i64::from(n),
    }
    .min(height);
    let window = &blocks[..=lookup as usize];
    let (min_time, max_time) = window
        .iter()
        .map(|b| i64::from(b.time))
        .fold((i64::MAX, i64::MIN), |(min, max), t| (min.min(t), max.max(t)));
    if min_time == max_time {
        return 0.into();
    }
    let work_diff = pb.chain_work - window[window.len() - 1].chain_work;
    UniValue::from_f64(work_diff.to_f64() / (max_time - min_time) as f64)
        .expect("a finite hash rate")
}

/// Core's `getmininginfo`. The mempool is empty until MP1a, so `pooledtx` is 0 (RP1a D30, D51).
// Core: rpc/mining.cpp:451-495
fn getmininginfo(
    chain: &dyn NodeChainState,
    miner: &dyn Miner,
    p: &MiningInfoParams,
) -> Result<UniValue, RpcError> {
    let state = ensure(chain)?;
    // `getnetworkhashps().HandleRequest(request)` with this command's empty params: the
    // defaults, read in the same turn as the tip.
    let window = state.active_window(None, DEFAULT_NBLOCKS.unsigned_abs())?;
    let tip = window.blocks.first().expect("the active chain has a tip");
    let mut obj = UniValue::new_object();
    obj.push_kv_end("blocks", i64::from(window.height).into());
    if let Some(last) = miner.last_block_stats() {
        obj.push_kv_end("currentblocksize", last.size.into());
        obj.push_kv_end("currentblocktx", last.num_txs.into());
    }
    obj.push_kv_end("difficulty", difficulty(tip.bits));
    obj.push_kv_end(
        "networkhashps",
        network_hash_ps(&window.blocks, DEFAULT_NBLOCKS, p.difficulty_adjustment_interval),
    );
    obj.push_kv_end("pooledtx", 0_u64.into());
    obj.push_kv_end("chain", p.network_id.clone().into());
    obj.push_kv_end("warnings", p.warnings.get_warnings(false).into());
    Ok(obj)
}

#[cfg(test)]
mod tests;
