//! `getspecialtxes` (RP1b-1c): a block's special transactions, as txids, hex or `TxToJSON`.

use std::sync::Arc;

use dash_consensus::CoreSerialize as _;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::{NodeChain, block_read_error, ensure_chainman, parse_hash_v};
use crate::UniValue;
use crate::core_write::{TxJsonParams, tx_to_json};
use crate::descriptor::hex;
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i32, param};
use crate::server::{Command, RpcServer};

/// Registers `getspecialtxes` on `server`.
pub fn register_specialtx_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    tx_json: TxJsonParams,
) {
    server.append_command(Command::new("getspecialtxes", move |r| {
        getspecialtxes(&*chain, &tx_json, &r.params)
    }));
}

/// An optional integer parameter: `default` when null, else `getInt<int>`.
fn int_or(v: &UniValue, default: i32) -> Result<i32, RpcError> {
    if v.is_null() {
        Ok(default)
    } else {
        get_int_i32(v)
    }
}

/// Core's `getspecialtxes`: the block's transactions with a payload field (`HasExtraPayloadField`)
/// of `type` (−1 for any), after the first `skip` of them, at most `count`. The chainman is
/// ensured first; the mempool and the InstantSend manager Core also ensures there exist whenever
/// the chainman does in dashd, and rdashd has neither yet (no transaction `IsLocked`, D40). The
/// block's facts are read once with the block, where `TxToJSON` looks the block up again for
/// each transaction under its own `cs_main`.
// Core: rpc/blockchain.cpp:2486-2556
fn getspecialtxes(
    chain: &dyn NodeChain,
    p: &TxJsonParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let hash: BlockHash = parse_hash_v(param(params, 0), "blockhash")?;
    let tx_type = int_or(param(params, 1), -1)?;
    let count = int_or(param(params, 2), 10)?;
    if count < 0 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Negative count"));
    }
    let skip = int_or(param(params, 3), 0)?;
    if skip < 0 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Negative skip"));
    }
    let verbosity = int_or(param(params, 4), 0)?;
    if !(0..=2).contains(&verbosity) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Verbosity must be in range 0..2"));
    }
    let (facts, block) = chainman.read_block(&hash).map_err(block_read_error)?;
    let mut result = UniValue::new_array();
    // Core's `nSkip + nCount` is an `int` sum, which dashd's build wraps past `INT_MAX` (signed
    // overflow): a window ending below zero takes nothing (D56).
    let end = skip.wrapping_add(count);
    let mut n = 0_i32;
    for tx in block.txs() {
        if !tx.has_extra_payload_field()
            || (tx_type != -1 && i32::from(tx.tx_type().to_base()) != tx_type)
        {
            continue;
        }
        n += 1;
        if n <= skip {
            continue;
        }
        if n > end {
            break;
        }
        result.push_back(match verbosity {
            0 => tx.txid().to_string().into(),
            1 => hex(&tx.raw().core_serialize()).into(),
            _ => tx_to_json(tx, &facts, p),
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
