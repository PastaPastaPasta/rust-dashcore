//! The commands over the node's indexes (IX1 f): `rpc/rawtransaction.cpp`'s
//! `getrawtransaction`, which looks up a transaction in the mempool, the txindex or a given
//! block, and `rpc/node.cpp`'s `getindexinfo`. There is no spent index: Core's `spent_index` is
//! null without `-spentindex`, which rdashd does not serve.

use std::sync::Arc;

use dash_consensus::coins::TxUndo;
use dash_consensus::{CoreSerialize as _, Tx};
use dash_primitives::{BlockHash, Transaction, TxHash};

use crate::UniValue;
use crate::blockchain::{
    Chainman, NodeChain, ensure_chainman, parse_hash_v, process_bls, tx_undo_at,
};
use crate::core_write::{TxJsonParams, tx_to_json};
use crate::descriptor::hex;
use crate::protocol::{RPC_INVALID_ADDRESS_OR_KEY, RPC_MISC_ERROR, RpcError};
use crate::rawtransaction::IsLockView;
use crate::request::{get_int_i32, param};
use crate::server::{Command, NodeControl, RpcServer};

/// A transaction and its block, or no block for a mempool transaction.
pub type RetrievedTx = (Arc<Transaction>, Option<BlockHash>);

/// One index's `GetSummary()`.
// Core: v24.0.0 src/index/base.cpp:531-537 GetSummary
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexSummary {
    pub name: &'static str,
    pub synced: bool,
    /// The best block's height, 0 before any block.
    pub best_block_height: u32,
}

/// What the commands read of the node besides the chain: Core's `node.mempool`, `g_txindex`
/// and the running indexes. `dashd-node` implements it.
pub trait TxLookup: Send + Sync {
    /// `g_txindex->BlockUntilSyncedToCurrentChain()`, or `None` without a txindex
    /// (`-txindex=0`). It may wait for the index, so a caller holds no chain lock.
    fn txindex_ready(&self) -> Option<bool>;

    /// `GetTransaction`'s mempool and txindex steps: without `block` the mempool, then the
    /// txindex; with it only a txindex answer in that block. A read failure is a miss.
    fn get_transaction(&self, block: Option<&BlockHash>, txid: &TxHash) -> Option<RetrievedTx>;

    /// Each running index's summary.
    fn summaries(&self) -> Vec<IndexSummary>;
}

/// What `getrawtransaction` reads besides the chain.
pub struct IndexSeams {
    pub lookup: Arc<dyn TxLookup>,
    pub islocks: Arc<dyn IsLockView>,
    /// The genesis block's merkle root, its coinbase's txid.
    pub genesis_coinbase: TxHash,
}

/// Registers `getrawtransaction` and `getindexinfo` on `server`.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:2029-2054, src/rpc/node.cpp:1255-1279
pub fn register_index_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    seams: IndexSeams,
    tx_json: TxJsonParams,
) {
    let lookup = Arc::clone(&seams.lookup);
    server.append_command(Command::new("getindexinfo", move |r| getindexinfo(&*lookup, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("getrawtransaction", move |r| {
        getrawtransaction(&*chain, &*control, &seams, &tx_json, &r.params)
    }));
}

/// `GetTransaction(block_index, mempool, txid)`: the seam's mempool and txindex steps, then the
/// given block's own transactions.
// Core: v24.0.0 src/node/transaction.cpp:126-156 GetTransaction
fn get_transaction(
    lookup: &dyn TxLookup,
    chainman: &dyn Chainman,
    block: Option<&BlockHash>,
    txid: &TxHash,
) -> Option<(Tx, Option<BlockHash>)> {
    if let Some((tx, hash)) = lookup.get_transaction(block, txid) {
        return Some((Tx::new((*tx).clone()), hash));
    }
    let block = block?;
    let (_, data) = chainman.read_block(block).ok()?;
    let tx = data.txs().iter().find(|tx| tx.txid() == *txid)?;
    Some((Tx::new(tx.to_mutable()), Some(*block)))
}

/// Core's `getrawtransaction`: the txid (not the genesis coinbase), the verbosity as a bool or a
/// number, and an optional block to look in; then `GetTransaction`, and on a miss the error its
/// lookup explains. The txindex's readiness is read before the lookup, without a block, and
/// answers only a miss. Verbosity 0 or less is the hex; 1 `TxToJSON`; 2 or more also each
/// input's `prevout` and the `fee`, from the block's undo data, for a non-coinbase transaction
/// of an indexed block.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:338-509 getrawtransaction
fn getrawtransaction(
    chain: &dyn NodeChain,
    control: &dyn NodeControl,
    seams: &IndexSeams,
    p: &TxJsonParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let txid: TxHash = parse_hash_v(param(params, 0), "parameter 1")?;
    // Core: v24.0.0 src/rpc/rawtransaction.cpp:416-419
    if txid == seams.genesis_coinbase {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "The genesis block coinbase is not considered an ordinary transaction and cannot be retrieved",
        ));
    }
    let verbosity = match param(params, 1) {
        UniValue::Null => 0,
        v if v.is_bool() => i32::from(v.get_bool()?),
        v => get_int_i32(v)?,
    };
    // Core: v24.0.0 src/rpc/rawtransaction.cpp:431-439
    let block = match param(params, 2) {
        UniValue::Null => None,
        v => {
            let hash: BlockHash = parse_hash_v(v, "parameter 3")?;
            if chainman.lookup_block_index(&hash).is_none() {
                return Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block hash not found"));
            }
            Some(hash)
        }
    };
    // Core: v24.0.0 src/rpc/rawtransaction.cpp:441-444
    let txindex_ready = if block.is_none() {
        seams.lookup.txindex_ready()
    } else {
        None
    };
    let Some((tx, hash_block)) = get_transaction(&*seams.lookup, &*chainman, block.as_ref(), &txid)
    else {
        return Err(not_found(&*chainman, block.as_ref(), txindex_ready));
    };
    if verbosity <= 0 {
        return Ok(hex(&tx.raw().core_serialize()).into());
    }
    // The block the lookup found, if indexed: a given block is the one it was found in.
    let facts = hash_block.as_ref().and_then(|hash| chainman.header_facts(hash));
    let mut result = UniValue::new_object();
    if block.is_some() {
        let active = facts.as_ref().is_some_and(|f| f.confirmations >= 0);
        result.push_kv_end("in_active_chain", active.into());
    }
    let block_undo = if verbosity >= 2 && facts.is_some() && !tx.is_coinbase() {
        hash_block.as_ref().and_then(|hash| block_undo(&*chainman, hash))
    } else {
        None
    };
    let undo = match &block_undo {
        Some((undo, block)) => tx_undo(&tx, block, undo)?,
        None => None,
    };
    let locked = seams.islocks.is_locked(&tx.txid());
    let json =
        tx_to_json(&tx, hash_block.as_ref(), facts.as_ref(), undo, locked, p, process_bls(chain))
            .map_err(|e| e.into_rpc_error(control))?;
    if let UniValue::Obj(fields) = json {
        for (key, value) in fields {
            result.push_kv_end(key, value);
        }
    }
    Ok(result)
}

/// The miss's error: for a given block "Block not available" without its data, else Core's
/// message for where the lookup looked.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:448-466
fn not_found(
    chainman: &dyn Chainman,
    block: Option<&BlockHash>,
    txindex_ready: Option<bool>,
) -> RpcError {
    let message = match (block, txindex_ready) {
        (Some(block), _) => {
            if !chainman.lookup_block_index(block).is_some_and(|s| s.have_data) {
                return RpcError::new(RPC_MISC_ERROR, "Block not available");
            }
            "No such transaction found in the provided block"
        }
        (None, None) => {
            "No such mempool transaction. Use -txindex or provide a block hash to enable blockchain transaction queries"
        }
        (None, Some(false)) => {
            "No such mempool transaction. Blockchain transactions are still in the process of being indexed"
        }
        (None, Some(true)) => "No such mempool or blockchain transaction",
    };
    RpcError::new(
        RPC_INVALID_ADDRESS_OR_KEY,
        format!("{message}. Use gettransaction for wallet transactions."),
    )
}

/// The block's undo data and the block, as Core reads them for verbosity 2: neither pruned,
/// both read; `None` otherwise.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:489-496
fn block_undo(
    chainman: &dyn Chainman,
    hash: &BlockHash,
) -> Option<(dash_consensus::coins::BlockUndo, Arc<dash_consensus::Block>)> {
    let undo = chainman.read_undo(hash).ok()?;
    let (_, block) = chainman.read_block(hash).ok()?;
    Some((undo, block))
}

/// `tx`'s entry of the block's undo data, none if the block does not hold `tx`, by
/// [`tx_undo_at`]'s rule.
// Core: v24.0.0 src/rpc/rawtransaction.cpp:498-505
fn tx_undo<'a>(
    tx: &Tx,
    block: &dash_consensus::Block,
    undo: &'a dash_consensus::coins::BlockUndo,
) -> Result<Option<&'a TxUndo>, RpcError> {
    let Some(i) = block.txs().iter().position(|t| t.txid() == tx.txid()) else {
        return Ok(None);
    };
    // The undo data has no entry for the coinbase: Core's `at(i - 1)`.
    tx_undo_at(undo, i.wrapping_sub(1), tx.raw().inputs().len()).map(Some)
}

/// Core's order of `getindexinfo`'s indexes: the txindex, coinstats, the filter indexes (any
/// other name), then the address, timestamp and spent indexes.
// Core: v24.0.0 src/rpc/node.cpp:1205-1227
fn core_rank(name: &str) -> u8 {
    match name {
        "txindex" => 0,
        "coinstatsindex" => 1,
        "addressindex" => 3,
        "timestampindex" => 4,
        "spentindex" => 5,
        _ => 2,
    }
}

/// Core's `getindexinfo`: each running index's `synced` and `best_block_height`, or only the
/// one named `index_name`; an empty object when none matches.
// Core: v24.0.0 src/rpc/node.cpp:1166-1232 SummaryToJSON, getindexinfo
fn getindexinfo(lookup: &dyn TxLookup, params: &UniValue) -> Result<UniValue, RpcError> {
    let name = match param(params, 0) {
        UniValue::Null => "",
        v => v.get_str()?,
    };
    let mut summaries = lookup.summaries();
    summaries.sort_by_key(|s| core_rank(s.name));
    let mut result = UniValue::new_object();
    for s in summaries.into_iter().filter(|s| name.is_empty() || name == s.name) {
        let mut entry = UniValue::new_object();
        entry.push_kv_end("synced", s.synced.into());
        entry.push_kv_end("best_block_height", i64::from(s.best_block_height).into());
        result.push_kv_end(s.name, entry);
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
