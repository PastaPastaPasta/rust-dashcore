//! `getcreditpoolinfo` (RP1a-5): the credit pool after a block of the active chain and the asset
//! unlock limit for the block after it, with the window that limit is computed over.

use dash_primitives::BlockHash;

use super::{NodeChain, ensure_chainman};
use crate::UniValue;
use crate::peers::amount;
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i32, param};

/// What `getcreditpoolinfo` reads for one block, in one chain state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditPoolFacts {
    /// The block: `pindex->nHeight` and its hash.
    pub height: u32,
    pub hash: BlockHash,
    /// `GetCreditPool(pindex)`: `locked`, `currentLimit`, `latelyUnlocked`.
    pub locked: i64,
    pub current_limit: i64,
    pub lately_unlocked: i64,
    /// `CreditPoolPeriodBlocks()`.
    pub window_blocks: i32,
    /// The window start's height and `GetBalanceAt` of it; none when the chain is shorter than
    /// the window.
    pub window_start: Option<(u32, i64)>,
}

/// Why `getcreditpoolinfo`'s reads gave no facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreditPoolReadError {
    /// No block at that height on the active chain (or no tip): Core's null `pindex`.
    OutOfRange,
    /// What Core's `GetCreditPool` throws (its `std::runtime_error` text), or a store fault
    /// where Core reads the block files (RP1a D59).
    Failed(String),
}

/// Core's `getcreditpoolinfo`: `pindex` is the tip, or the active chain's block at `height`;
/// then the pool after it and the window start's balance.
// Core: rpc/blockchain.cpp:1759-1823
pub(super) fn getcreditpoolinfo(
    chain: &dyn NodeChain,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let height = match param(params, 0) {
        UniValue::Null => None,
        h => Some(get_int_i32(h)?),
    };
    let facts = chainman.credit_pool_info(height).map_err(|e| match e {
        CreditPoolReadError::OutOfRange => {
            RpcError::new(RPC_INVALID_PARAMETER, "Block height out of range")
        }
        CreditPoolReadError::Failed(what) => RpcError::Runtime(what),
    })?;
    Ok(credit_pool_json(&facts))
}

/// The reply: Core's keys in its order.
// Core: rpc/blockchain.cpp:1812-1822
fn credit_pool_json(facts: &CreditPoolFacts) -> UniValue {
    let mut result = UniValue::new_object();
    result.push_kv_end("height", i64::from(facts.height).into());
    result.push_kv_end("blockhash", facts.hash.to_string().into());
    result.push_kv_end("balance", amount(facts.locked));
    result.push_kv_end("currentlimit", amount(facts.current_limit));
    let mut window = UniValue::new_object();
    window.push_kv_end("blocks", i64::from(facts.window_blocks).into());
    let (start_height, start_balance) =
        facts.window_start.map_or((-1, 0), |(h, balance)| (i64::from(h), balance));
    window.push_kv_end("height", start_height.into());
    window.push_kv_end("balance", amount(start_balance));
    window.push_kv_end("unlocked", amount(facts.lately_unlocked));
    result.push_kv_end("window", window);
    result
}

#[cfg(test)]
mod tests;
