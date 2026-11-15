//! `getchainlockbyheight` (BL-689a): a ChainLock a later coinbase on the active chain embeds,
//! read from the block files (Core's `CoinbaseChainLockReader`).

use dash_primitives::BlockHash;

use super::{NodeChain, ensure_chainman};
use crate::UniValue;
use crate::descriptor::hex;
use crate::protocol::{
    RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError,
};
use crate::request::{get_int_i32, param};

/// The certificate `Find(height, height)` found, as the reply prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoricalChainLock {
    /// The signed block's hash.
    pub block_hash: BlockHash,
    /// `Signed().getSig()`'s serialization under the process flag.
    pub signature: [u8; 96],
    /// The carrier's height.
    pub carrier_height: u32,
}

/// Why `getchainlockbyheight` has no certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClLookupError {
    /// `Find` completed without one.
    NotFound,
    /// The carrier left the active chain during the lookup.
    ChainChanged,
    /// What the reader or `Signed()` threw, by its `what()`.
    Failed(String),
}

/// Core's `getchainlockbyheight`: the height, which must be non-negative, before the chainman;
/// then the reader's lookup on the tip as one read.
// Core: rpc/quorums.cpp:1404-1459
pub(super) fn getchainlockbyheight(
    chain: &dyn NodeChain,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let height = get_int_i32(param(params, 0))?;
    if height < 0 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "height must be non-negative"));
    }
    let found = ensure_chainman(chain)?.chainlock_by_height(height).map_err(|e| match e {
        ClLookupError::NotFound => {
            RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Chainlock not found for height")
        }
        ClLookupError::ChainChanged => {
            RpcError::new(RPC_MISC_ERROR, "Chain changed during ChainLock lookup; retry")
        }
        ClLookupError::Failed(what) => RpcError::Runtime(what),
    })?;
    let mut result = UniValue::new_object();
    result.push_kv_end("height", height.into());
    result.push_kv_end("blockhash", found.block_hash.to_string().into());
    result.push_kv_end("signature", hex(&found.signature).into());
    result.push_kv_end("cbtx_height", i64::from(found.carrier_height).into());
    Ok(result)
}

#[cfg(test)]
mod tests;
