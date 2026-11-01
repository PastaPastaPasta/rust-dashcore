//! `scantxoutset` (RP1b-1a): the scan objects' scripts and inferred descriptors, the walk of
//! the coins the chainman runs after `ForceFlushStateToDisk()`, and Core's
//! `CoinsViewScanReserver` over the scan's progress and abort flag.
//!
//! Core's `g_scan_progress`, `g_scan_in_progress` and `g_should_abort_scan` are process
//! globals; here they are the command's own state, one per server, which is one per process.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use dash_consensus::coins::Coin;
use dash_primitives::{BlockHash, TxHash};

use super::{NodeChain, ensure_chainman, txout_set_error};
use crate::UniValue;
use crate::descriptor::{SigningProvider, hex, infer_descriptor_with};
use crate::key_io::KeyIoParams;
use crate::peers::amount;
use crate::protocol::{RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError};
use crate::request::{param, values};
use crate::server::{Command, RpcServer};
use crate::util::eval_descriptor_string_or_object;

/// What the walk reports through: the progress it sets and the abort flag it reads.
#[derive(Clone, Default)]
pub struct ScanControl {
    /// `g_scan_progress`.
    pub progress: Arc<AtomicI32>,
    /// `g_should_abort_scan`.
    pub should_abort: Arc<AtomicBool>,
}

/// A finished walk: `FindScriptPubKey`'s result and count, the tip read with its cursor, and
/// the coins found, in `std::map<COutPoint, Coin>` order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxOutSetScan {
    pub success: bool,
    pub count: i64,
    pub best_block: BlockHash,
    pub height: u32,
    pub unspents: Vec<(TxHash, u32, Coin)>,
}

/// The command's state: `g_scan_in_progress` beside the walk's [`ScanControl`].
#[derive(Default)]
struct ScanState {
    in_progress: AtomicBool,
    control: ScanControl,
}

/// `CoinsViewScanReserver`: holds the scan slot while it lives, and on release clears it and
/// resets the progress to 0.
// Core: rpc/blockchain.cpp:2596-2622
struct Reserver<'a>(&'a ScanState);

impl<'a> Reserver<'a> {
    /// `reserve()`: the slot, unless a scan holds it.
    fn reserve(state: &'a ScanState) -> Option<Reserver<'a>> {
        if state.in_progress.swap(true, Ordering::SeqCst) {
            return None;
        }
        // Core's `CHECK_NONFATAL(g_scan_progress == 0)` is not kept: a reserve between a
        // release's two stores sees the last progress, and its "Internal bug detected" reply
        // names Core's own source lines.
        Some(Reserver(state))
    }
}

impl Drop for Reserver<'_> {
    fn drop(&mut self) {
        self.0.in_progress.store(false, Ordering::SeqCst);
        self.0.control.progress.store(0, Ordering::SeqCst);
    }
}

/// Registers `scantxoutset` on `server`, with its own scan state.
pub fn register_scan_commands(server: &RpcServer, chain: Arc<dyn NodeChain>, key_io: KeyIoParams) {
    let state = Arc::new(ScanState::default());
    server.append_command(Command::new("scantxoutset", move |r| {
        scantxoutset(&state, &*chain, &key_io, &r.params)
    }));
}

/// Core's `scantxoutset`: `status` (null when idle, else the progress), `abort` (whether a scan
/// was running to abort), or `start`: the slot reserved first, then the scan objects, each
/// expanded and inferred over its own signing provider (the first inference of a script kept),
/// then the walk.
// Core: rpc/blockchain.cpp:2658-2836
fn scantxoutset(
    state: &ScanState,
    chain: &dyn NodeChain,
    p: &KeyIoParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let action = param(params, 0).get_str()?;
    match action {
        "status" => Ok(match Reserver::reserve(state) {
            // No scan in progress.
            Some(_) => UniValue::Null,
            None => {
                let mut result = UniValue::new_object();
                let progress = state.control.progress.load(Ordering::SeqCst);
                result.push_kv_end("progress", i64::from(progress).into());
                result
            }
        }),
        "abort" => Ok(match Reserver::reserve(state) {
            // No scan was running.
            Some(_) => false.into(),
            None => {
                state.control.should_abort.store(true, Ordering::SeqCst);
                true.into()
            }
        }),
        "start" => {
            let _reserver = Reserver::reserve(state).ok_or_else(|| {
                RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Scan already in progress, use action \"abort\" or \"status\"",
                )
            })?;
            if values(params).len() < 2 {
                return Err(RpcError::new(
                    RPC_MISC_ERROR,
                    "scanobjects argument is required for the start action",
                ));
            }
            let mut needles = HashSet::new();
            let mut descriptors = HashMap::new();
            // `get_array()`: UniValue's type error for anything else.
            let objects = param(params, 1);
            let UniValue::Arr(objects) = objects else {
                return Err(RpcError::Type(format!(
                    "JSON value of type {} is not of expected type array",
                    objects.value_type().name()
                )));
            };
            for scanobject in objects {
                let mut provider = SigningProvider::default();
                for script in eval_descriptor_string_or_object(scanobject, &mut provider, p)? {
                    let inferred = infer_descriptor_with(&script, &provider, p);
                    needles.insert(script.clone());
                    descriptors.entry(script).or_insert(inferred);
                }
            }
            state.control.should_abort.store(false, Ordering::SeqCst);
            // The walk's errors: its interruption point's "Shutting down", and a set it cannot
            // read, which Core's has no answer for (D58).
            let scan = ensure_chainman(chain)?
                .scan_txout_set(needles, state.control.clone())
                .map_err(txout_set_error)?;
            Ok(scan_to_json(&scan, &descriptors))
        }
        _ => Err(RpcError::new(RPC_INVALID_PARAMETER, format!("Invalid action '{action}'"))),
    }
}

/// The `start` result: Core's keys in its order, each found coin with its inferred descriptor.
// Core: rpc/blockchain.cpp:2775-2805
fn scan_to_json(scan: &TxOutSetScan, descriptors: &HashMap<Vec<u8>, String>) -> UniValue {
    let mut result = UniValue::new_object();
    result.push_kv_end("success", scan.success.into());
    result.push_kv_end("txouts", scan.count.into());
    result.push_kv_end("height", i64::from(scan.height).into());
    result.push_kv_end("bestblock", scan.best_block.to_string().into());
    let mut unspents = UniValue::new_array();
    let mut total_in = 0i64;
    for (txid, vout, coin) in &scan.unspents {
        total_in = total_in.wrapping_add(coin.value());
        let script = coin.script_pubkey().as_slice();
        let mut unspent = UniValue::new_object();
        unspent.push_kv_end("txid", txid.to_string().into());
        // Core's `(int32_t)outpoint.n` and `(int32_t)coin.nHeight`.
        unspent.push_kv_end("vout", i64::from(*vout as i32).into());
        unspent.push_kv_end("scriptPubKey", hex(script).into());
        // `descriptors[txo.scriptPubKey]`: every found script is a needle, so it has one.
        let desc = descriptors.get(script).cloned().unwrap_or_default();
        unspent.push_kv_end("desc", desc.into());
        unspent.push_kv_end("amount", amount(coin.value()));
        unspent.push_kv_end("coinbase", coin.is_coinbase().into());
        unspent.push_kv_end("height", i64::from(coin.height() as i32).into());
        unspents.push_back(unspent);
    }
    result.push_kv_end("unspents", unspents);
    result.push_kv_end("total_amount", amount(total_in));
    result
}

#[cfg(test)]
mod tests;
