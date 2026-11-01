//! `getblockstats` and `pruneblockchain`.

use std::collections::BTreeSet;

use dash_consensus::coins::{BlockUndo, is_unspendable};
use dash_consensus::primitives::compact_size_len;
use dash_consensus::{Block, MAX_MONEY};
use dash_primitives::{BlockHash, SpecialPayload};

use super::{
    NodeChain, UndoReadError, block_read_error, ensure_chain, ensure_chainman, parse_hash_v,
    range_check,
};
use crate::UniValue;
use crate::protocol::{
    RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError,
};
use crate::request::{find_value, get_int_i32, param};

/// `NUM_GETBLOCKSTATS_PERCENTILES`.
const PERCENTILES: usize = 5;

/// `PER_UTXO_OVERHEAD`: `sizeof(COutPoint) + sizeof(uint32_t) + sizeof(bool)`, an outpoint
/// (needed for the UTXO index) plus `nHeight` and `fCoinBase`.
// Core: rpc/blockchain.cpp:2208-2209
const PER_UTXO_OVERHEAD: i64 = 36 + 4 + 1;

/// `MaxBlockSize()`, with DIP0001 active: `mintxsize`'s start value.
// Core: consensus/consensus.h:12-15
const MAX_BLOCK_SIZE: i64 = 2_000_000;

/// `ParseHashOrHeight`: a height on the active chain, or any block the index has.
// Core: rpc/blockchain.cpp:108-134
fn parse_hash_or_height(chain: &dyn NodeChain, v: &UniValue) -> Result<BlockHash, RpcError> {
    if v.is_num() {
        let height = get_int_i32(v)?;
        if height < 0 {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Target block height {height} is negative"),
            ));
        }
        let active = ensure_chain(chain)?;
        let tip = active.height();
        if height > tip {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Target block height {height} after current tip {tip}"),
            ));
        }
        return Ok(active.block_hash(height));
    }
    let hash = parse_hash_v(v, "hash_or_height")?;
    match ensure_chainman(chain)?.lookup_block_index(&hash) {
        Some(_) => Ok(hash),
        None => Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found")),
    }
}

/// `CalculateTruncatedMedian`: the middle score, or the two middle ones' mean rounded toward
/// zero; 0 for none.
// Core: rpc/blockchain.cpp:2155-2169
fn truncated_median(scores: &mut [i64]) -> i64 {
    let size = scores.len();
    if size == 0 {
        return 0;
    }
    scores.sort_unstable();
    if size.is_multiple_of(2) {
        (scores[size / 2 - 1] + scores[size / 2]) / 2
    } else {
        scores[size / 2]
    }
}

/// `CalculatePercentilesBySize`: the feerates at the 10th, 25th, 50th, 75th and 90th percentile
/// of `total_size` bytes, `scores` being (feerate, size) pairs; zeros for no scores.
// Core: rpc/blockchain.cpp:2171-2199
fn percentiles_by_size(scores: &mut [(i64, i64)], total_size: i64) -> [i64; PERCENTILES] {
    let mut result = [0; PERCENTILES];
    if scores.is_empty() {
        return result;
    }
    scores.sort_unstable();
    let total = total_size as f64;
    let weights =
        [total / 10.0, total / 4.0, total / 2.0, (total * 3.0) / 4.0, (total * 9.0) / 10.0];
    let mut next = 0;
    let mut cumulative_weight = 0i64;
    for &(feerate, size) in scores.iter() {
        cumulative_weight += size;
        while next < PERCENTILES && cumulative_weight as f64 >= weights[next] {
            result[next] = feerate;
            next += 1;
        }
    }
    let last = scores[scores.len() - 1].0;
    for r in &mut result[next..] {
        *r = last;
    }
    result
}

/// A `CTxOut`'s serialized size: the value, the script's compact size, the script.
fn txout_size(script_len: usize) -> i64 {
    (8 + compact_size_len(script_len as u64) + script_len) as i64
}

/// What `getblockstats` computes, which of it a `stats` selection needs.
struct Needs {
    all: bool,
    median_tx_size: bool,
    median_fee: bool,
    feerate_percentiles: bool,
    inputs: bool,
    outputs: bool,
    size: bool,
}

impl Needs {
    // Core: rpc/blockchain.cpp:2294-2303
    fn of(stats: &BTreeSet<String>) -> Self {
        let has = |keys: &[&str]| keys.iter().any(|k| stats.contains(*k));
        let all = stats.is_empty();
        let median_tx_size = all || has(&["mediantxsize"]);
        let median_fee = all || has(&["medianfee"]);
        let feerate_percentiles = all || has(&["feerate_percentiles"]);
        let inputs = all
            || median_fee
            || feerate_percentiles
            || has(&[
                "utxo_increase",
                "utxo_increase_actual",
                "utxo_size_inc",
                "utxo_size_inc_actual",
                "totalfee",
                "avgfee",
                "avgfeerate",
                "minfee",
                "maxfee",
                "minfeerate",
                "maxfeerate",
            ]);
        let outputs = all || inputs || has(&["total_out"]);
        let size = all
            || median_tx_size
            || has(&[
                "total_size",
                "avgtxsize",
                "mintxsize",
                "maxtxsize",
                "avgfeerate",
                "feerate_percentiles",
                "minfeerate",
                "maxfeerate",
            ]);
        Self {
            all,
            median_tx_size,
            median_fee,
            feerate_percentiles,
            inputs,
            outputs,
            size,
        }
    }
}

/// Core's `getblockstats`: every statistic of one block, in Core's (alphabetical) order, or the
/// selected ones in `std::set`'s order. There is no transaction index to wait for (IX1). Fees
/// come from the block's undo data, which the genesis block has none of; a Platform transfer
/// pays its payload's fee.
// Core: rpc/blockchain.cpp:2211-2449
pub(super) fn getblockstats(
    chain: &dyn NodeChain,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    let hash = parse_hash_or_height(chain, param(params, 0))?;
    let mut stats = BTreeSet::new();
    let selected = param(params, 1);
    if !selected.is_null() {
        for stat in selected.get_values()? {
            stats.insert(stat.get_str()?.to_owned());
        }
    }

    let (facts, block) = chainman.read_block(&hash).map_err(block_read_error)?;
    // Core: rpc/blockchain.cpp:806-827 GetUndoChecked; the genesis block has no undo data.
    let undo = if facts.height == 0 {
        BlockUndo {
            txs: vec![],
        }
    } else {
        chainman.read_undo(&hash).map_err(undo_read_error)?
    };
    let needs = Needs::of(&stats);
    let s = compute(&block, &undo, facts.height, &needs)?;
    // Core's `GetBlockSubsidy` cannot fail for a block it has read; the node's fails only
    // without subsidy parameters, which init would have refused.
    let subsidy = chainman.block_subsidy(&hash).ok_or_else(|| internal_bug("subsidy"))?;

    let n_tx = block.txs().len() as i64;
    let mut percentiles = UniValue::new_array();
    for feerate in s.feerate_percentiles {
        percentiles.push_back(feerate.into());
    }
    // `totalfee` and `total_size` are sums of in-range fees and sizes, so Core's unsigned
    // `size_t` divisions give the same quotients.
    let mut all = UniValue::new_object();
    all.push_kv_end(
        "avgfee",
        (if n_tx > 1 {
            s.totalfee / (n_tx - 1)
        } else {
            0
        })
        .into(),
    );
    all.push_kv_end(
        "avgfeerate",
        (if s.total_size != 0 {
            s.totalfee / s.total_size
        } else {
            0
        })
        .into(),
    );
    all.push_kv_end(
        "avgtxsize",
        (if n_tx > 1 {
            s.total_size / (n_tx - 1)
        } else {
            0
        })
        .into(),
    );
    all.push_kv_end("blockhash", hash.to_string().into());
    all.push_kv_end("feerate_percentiles", percentiles);
    all.push_kv_end("height", i64::from(facts.height).into());
    all.push_kv_end("ins", s.inputs.into());
    all.push_kv_end("maxfee", s.maxfee.into());
    all.push_kv_end("maxfeerate", s.maxfeerate.into());
    all.push_kv_end("maxtxsize", s.maxtxsize.into());
    all.push_kv_end("medianfee", s.medianfee.into());
    all.push_kv_end("mediantime", facts.median_time_past.into());
    all.push_kv_end("mediantxsize", s.mediantxsize.into());
    all.push_kv_end(
        "minfee",
        (if s.minfee == MAX_MONEY {
            0
        } else {
            s.minfee
        })
        .into(),
    );
    all.push_kv_end(
        "minfeerate",
        (if s.minfeerate == MAX_MONEY {
            0
        } else {
            s.minfeerate
        })
        .into(),
    );
    all.push_kv_end(
        "mintxsize",
        (if s.mintxsize == MAX_BLOCK_SIZE {
            0
        } else {
            s.mintxsize
        })
        .into(),
    );
    all.push_kv_end("outs", s.outputs.into());
    all.push_kv_end("subsidy", subsidy.into());
    all.push_kv_end("time", i64::from(facts.header.time).into());
    all.push_kv_end("total_out", s.total_out.into());
    all.push_kv_end("total_size", s.total_size.into());
    all.push_kv_end("totalfee", s.totalfee.into());
    all.push_kv_end("txs", n_tx.into());
    all.push_kv_end("utxo_increase", (s.outputs - s.inputs).into());
    all.push_kv_end("utxo_size_inc", s.utxo_size_inc.into());
    all.push_kv_end("utxo_increase_actual", (s.utxos - s.inputs).into());
    all.push_kv_end("utxo_size_inc_actual", s.utxo_size_inc_actual.into());

    if needs.all {
        return Ok(all);
    }
    let UniValue::Obj(fields) = &all else {
        unreachable!("an object")
    };
    let mut ret = UniValue::new_object();
    for stat in &stats {
        let value = find_value(fields, stat);
        if value.is_null() {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Invalid selected statistic '{stat}'"),
            ));
        }
        ret.push_kv_end(stat.clone(), value.clone());
    }
    Ok(ret)
}

/// The block's sums, as Core's loop leaves them for `needs`.
struct Sums {
    maxfee: i64,
    maxfeerate: i64,
    minfee: i64,
    minfeerate: i64,
    total_out: i64,
    totalfee: i64,
    inputs: i64,
    maxtxsize: i64,
    mintxsize: i64,
    outputs: i64,
    total_size: i64,
    utxos: i64,
    utxo_size_inc: i64,
    utxo_size_inc_actual: i64,
    medianfee: i64,
    mediantxsize: i64,
    feerate_percentiles: [i64; PERCENTILES],
}

/// `getblockstats`' loop over the block's transactions and, for the fee statistics, each
/// one's undo entry: `vtxundo.at(i - 1)` throws for undo data short of a transaction.
// Core: rpc/blockchain.cpp:2324-2400
fn compute(block: &Block, undo: &BlockUndo, height: u32, needs: &Needs) -> Result<Sums, RpcError> {
    let mut s = Sums {
        maxfee: 0,
        maxfeerate: 0,
        minfee: MAX_MONEY,
        minfeerate: MAX_MONEY,
        total_out: 0,
        totalfee: 0,
        inputs: 0,
        maxtxsize: 0,
        mintxsize: MAX_BLOCK_SIZE,
        outputs: 0,
        total_size: 0,
        utxos: 0,
        utxo_size_inc: 0,
        utxo_size_inc_actual: 0,
        medianfee: 0,
        mediantxsize: 0,
        feerate_percentiles: [0; PERCENTILES],
    };
    let mut fee_array = Vec::new();
    let mut feerate_array = Vec::new();
    let mut txsize_array = Vec::new();

    for (i, tx) in block.txs().iter().enumerate() {
        let raw = tx.raw();
        s.outputs += raw.outputs.len() as i64;

        let mut tx_total_out = 0i64;
        if needs.outputs {
            for out in &raw.outputs {
                tx_total_out = tx_total_out.wrapping_add(out.value);
                let script = out.script_pubkey.as_bytes();
                let out_size = txout_size(script.len()) + PER_UTXO_OVERHEAD;
                s.utxo_size_inc += out_size;
                // The genesis coinbase does not change the UTXO set; unspendable outputs never
                // enter it.
                if height == 0 || is_unspendable(script) {
                    continue;
                }
                s.utxos += 1;
                s.utxo_size_inc_actual += out_size;
            }
        }

        if tx.is_coinbase() {
            continue;
        }

        // The coinbase's input is not counted, nor its reward.
        s.inputs += raw.inputs.len() as i64;
        s.total_out = s.total_out.wrapping_add(tx_total_out);

        let mut tx_size = 0i64;
        if needs.size {
            tx_size = i64::from(tx.size());
            if needs.median_tx_size {
                txsize_array.push(tx_size);
            }
            s.maxtxsize = s.maxtxsize.max(tx_size);
            s.mintxsize = s.mintxsize.min(tx_size);
            s.total_size += tx_size;
        }

        if needs.inputs {
            let tx_undo = undo.txs.get(i - 1).ok_or_else(|| range_check(i - 1, undo.txs.len()))?;
            let mut tx_total_in = 0i64;
            for coin in &tx_undo.coins {
                tx_total_in = tx_total_in.wrapping_add(coin.value());
                let prevout_size = txout_size(coin.script_pubkey().len()) + PER_UTXO_OVERHEAD;
                s.utxo_size_inc -= prevout_size;
                s.utxo_size_inc_actual -= prevout_size;
            }

            let mut txfee = tx_total_in.wrapping_sub(tx_total_out);
            if tx.is_platform_transfer() {
                // `GetTxPayload<CAssetUnlockPayload>` reads the whole payload or nothing.
                txfee = match tx.payload() {
                    Some(Ok(d)) if d.is_complete() => match &d.payload {
                        SpecialPayload::AssetUnlock(unlock) => Some(i64::from(unlock.fee)),
                        _ => None,
                    },
                    _ => None,
                }
                .ok_or_else(|| internal_bug("payload"))?;
            }
            if !(0..=MAX_MONEY).contains(&txfee) {
                return Err(internal_bug("MoneyRange(txfee)"));
            }
            if needs.median_fee {
                fee_array.push(txfee);
            }
            s.maxfee = s.maxfee.max(txfee);
            s.minfee = s.minfee.min(txfee);
            s.totalfee += txfee;

            let feerate = if tx_size != 0 {
                txfee / tx_size
            } else {
                0
            };
            if needs.feerate_percentiles {
                feerate_array.push((feerate, tx_size));
            }
            s.maxfeerate = s.maxfeerate.max(feerate);
            s.minfeerate = s.minfeerate.min(feerate);
        }
    }

    s.feerate_percentiles = percentiles_by_size(&mut feerate_array, s.total_size);
    s.medianfee = truncated_median(&mut fee_array);
    s.mediantxsize = truncated_median(&mut txsize_array);
    Ok(s)
}

/// `GetUndoChecked`'s errors.
// Core: rpc/blockchain.cpp:806-827
fn undo_read_error(e: UndoReadError) -> RpcError {
    match e {
        UndoReadError::Pruned => {
            RpcError::new(RPC_MISC_ERROR, "Undo data not available (pruned data)")
        }
        UndoReadError::Unreadable => {
            RpcError::new(RPC_MISC_ERROR, "Can't read undo data from disk")
        }
    }
}

/// `CHECK_NONFATAL(condition)`'s error, in the short form the other ports answer (D41).
fn internal_bug(condition: &str) -> RpcError {
    RpcError::new(RPC_MISC_ERROR, format!("Internal bug detected: {condition}"))
}

/// Core's `pruneblockchain`. Init refuses `-prune` other than 0 (RP1a D31), so the node is
/// never in prune mode and every call after `EnsureAnyChainman` fails as Core's does then.
// Core: rpc/blockchain.cpp:1077-1135
pub(super) fn pruneblockchain(chain: &dyn NodeChain) -> Result<UniValue, RpcError> {
    ensure_chainman(chain)?;
    Err(RpcError::new(RPC_MISC_ERROR, "Cannot prune blocks because node is not in prune mode."))
}

#[cfg(test)]
mod tests;
