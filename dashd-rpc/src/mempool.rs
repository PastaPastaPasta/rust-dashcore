//! `rpc/mempool.cpp`'s `getrawmempool` and `getmempoolinfo` over the node's pool
//! ([`MempoolView`]) and InstantSend's lock reads ([`IsLockView`]), and `sendrawtransaction`
//! over the coins tip and the node's pool and peer manager ([`TxBroadcast`]).

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_consensus::{Tx, TxRef, TxValidationError, TxValidationResult};
use dash_primitives::{OutPoint, TxHash};

use crate::UniValue;
use crate::blockchain::{Aborted, NodeChainState, no_chainman, parse_hash_v};
use crate::mining::decode_hex_tx;
use crate::peers::{amount, p2p_disabled};
use crate::protocol::{
    RPC_CLIENT_MEMPOOL_DISABLED, RPC_DESERIALIZATION_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR,
    RPC_TRANSACTION_ALREADY_IN_CHAIN, RPC_TRANSACTION_ERROR, RPC_TRANSACTION_REJECTED, RpcError,
};
use crate::rawtransaction::{IsLockView, amount_from_value};
use crate::request::{get_array, get_int_i64, param};
use crate::server::{Command, RpcServer};

/// One pool entry as `entryToJSON` reads it, taken under the pool's lock.
// Core: rpc/mempool.cpp:268-313
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MempoolEntryView {
    /// `GetTx().GetHash()`.
    pub txid: TxHash,
    /// `GetTxSize()`.
    pub vsize: u64,
    /// `GetTime()`, in seconds.
    pub time: i64,
    /// `GetHeight()`.
    pub height: u32,
    /// `GetCountWithDescendants()`.
    pub descendant_count: u64,
    /// `GetSizeWithDescendants()`.
    pub descendant_size: u64,
    /// `GetCountWithAncestors()`.
    pub ancestor_count: u64,
    /// `GetSizeWithAncestors()`.
    pub ancestor_size: u64,
    /// `GetFee()`.
    pub fee: i64,
    /// `GetModifiedFee()`.
    pub modified_fee: i64,
    /// `GetModFeesWithAncestors()`.
    pub ancestor_fees: i64,
    /// `GetModFeesWithDescendants()`.
    pub descendant_fees: i64,
    /// The inputs' txids that `pool.exists`, in any order and with repeats.
    pub depends: Vec<TxHash>,
    /// `GetMemPoolChildrenConst()`, in any order.
    pub spent_by: Vec<TxHash>,
    /// `pool.IsUnbroadcastTx`.
    pub unbroadcast: bool,
}

/// The pool's figures `MempoolInfoToJSON` reads under one lock.
// Core: rpc/mempool.cpp:657-677
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MempoolInfo {
    /// `GetLoadTried()`.
    pub loaded: bool,
    /// `size()`.
    pub size: u64,
    /// `GetTotalTxSize()`.
    pub bytes: u64,
    /// `DynamicMemoryUsage()`.
    pub usage: u64,
    /// `GetTotalFee()`.
    pub total_fee: i64,
    /// `m_max_size_bytes`.
    pub max_mempool: i64,
    /// `GetMinFee().GetFeePerK()`.
    pub min_fee_per_k: i64,
    /// `m_min_relay_feerate.GetFeePerK()`.
    pub min_relay_fee_per_k: i64,
    /// `GetUnbroadcastTxs().size()`.
    pub unbroadcast_count: u64,
    /// `GetPendingAssetUnlockAmount()`.
    pub pending_asset_unlocks: i64,
}

/// The pool's reads the mempool commands make, each under one pool lock as Core's
/// `LOCK(pool.cs)`. Before the node has a pool they are a fresh, empty pool's.
// Core: rpc/mempool.cpp:315-352, :657-677, txmempool.h:337
pub trait MempoolView: Send + Sync {
    /// Every entry's txid, and `GetSequence()`.
    // Core: rpc/mempool.cpp:334-341
    fn txids_and_sequence(&self) -> (Vec<TxHash>, u64);
    /// Every entry's `entryToJSON` fields.
    // Core: rpc/mempool.cpp:321-329
    fn entries(&self) -> Vec<MempoolEntryView>;
    /// `MempoolInfoToJSON`'s pool figures.
    // Core: rpc/mempool.cpp:657-677
    fn info(&self) -> MempoolInfo;
}

/// Registers `getrawmempool` and `getmempoolinfo` on `server`.
// Core: rpc/mempool.cpp:885-902
pub fn register_mempool_commands(
    server: &RpcServer,
    pool: Arc<dyn MempoolView>,
    islocks: Arc<dyn IsLockView>,
) {
    let (p, is) = (Arc::clone(&pool), Arc::clone(&islocks));
    server.append_command(Command::new("getmempoolinfo", move |_| {
        Ok(mempool_info_to_json(p.info(), &*is))
    }));
    server.append_command(Command::new("getrawmempool", move |r| {
        getrawmempool(&*pool, &*islocks, &r.params)
    }));
}

/// Core's `MempoolInfoToJSON`: `mempoolminfee` is the larger of `GetMinFee()` and the minimum
/// relay feerate.
// Core: rpc/mempool.cpp:657-677
fn mempool_info_to_json(info: MempoolInfo, islocks: &dyn IsLockView) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("loaded", info.loaded.into());
    ret.push_kv_end("size", info.size.into());
    ret.push_kv_end("bytes", info.bytes.into());
    ret.push_kv_end("usage", info.usage.into());
    ret.push_kv_end("total_fee", amount(info.total_fee));
    ret.push_kv_end("maxmempool", info.max_mempool.into());
    ret.push_kv_end("mempoolminfee", amount(info.min_fee_per_k.max(info.min_relay_fee_per_k)));
    ret.push_kv_end("minrelaytxfee", amount(info.min_relay_fee_per_k));
    ret.push_kv_end("instantsendlocks", (islocks.lock_count() as u64).into());
    ret.push_kv_end("unbroadcastcount", info.unbroadcast_count.into());
    ret.push_kv_end("pendingassetunlocks", amount(info.pending_asset_unlocks));
    ret
}

/// Core's `getrawmempool`. Core lists the entries in `mapTx`'s salted-hash order, which is
/// unspecified; ours is ascending txid (`uint256 <`).
// Core: rpc/mempool.cpp:315-406
fn getrawmempool(
    pool: &dyn MempoolView,
    islocks: &dyn IsLockView,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let flag = |i| match param(params, i) {
        v if v.is_null() => Ok(false),
        v => v.get_bool(),
    };
    let verbose = flag(0)?;
    let include_sequence = flag(1)?;
    if verbose {
        if include_sequence {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Verbose results cannot contain mempool sequence values.",
            ));
        }
        let mut entries = pool.entries();
        entries.sort_by_key(|e| e.txid);
        let mut o = UniValue::new_object();
        for e in entries {
            let (txid, info) = entry_to_json(e, islocks);
            o.push_kv_end(txid, info);
        }
        return Ok(o);
    }
    let (mut txids, sequence) = pool.txids_and_sequence();
    txids.sort();
    let a = UniValue::Arr(txids.iter().map(|t| t.to_string().into()).collect());
    if !include_sequence {
        return Ok(a);
    }
    let mut o = UniValue::new_object();
    o.push_kv_end("txids", a);
    o.push_kv_end("mempool_sequence", sequence.into());
    Ok(o)
}

/// Core's `entryToJSON`: `depends` sorted as strings (`std::set<std::string>`), `spentby` in
/// `uint256` order (`CompareIteratorByHash`), and `instantlock` as `IsLocked`'s
/// `util::to_string`, since Core's node always has an InstantSend manager.
// Core: rpc/mempool.cpp:268-313, kernel/mempool_entry.h:39-52
fn entry_to_json(e: MempoolEntryView, islocks: &dyn IsLockView) -> (String, UniValue) {
    let mut info = UniValue::new_object();
    info.push_kv_end("vsize", e.vsize.into());
    info.push_kv_end("time", e.time.into());
    info.push_kv_end("height", u64::from(e.height).into());
    info.push_kv_end("descendantcount", e.descendant_count.into());
    info.push_kv_end("descendantsize", e.descendant_size.into());
    info.push_kv_end("ancestorcount", e.ancestor_count.into());
    info.push_kv_end("ancestorsize", e.ancestor_size.into());
    let mut fees = UniValue::new_object();
    fees.push_kv_end("base", amount(e.fee));
    fees.push_kv_end("modified", amount(e.modified_fee));
    fees.push_kv_end("ancestor", amount(e.ancestor_fees));
    fees.push_kv_end("descendant", amount(e.descendant_fees));
    info.push_kv_end("fees", fees);
    let depends: BTreeSet<String> = e.depends.iter().map(ToString::to_string).collect();
    info.push_kv_end("depends", UniValue::Arr(depends.into_iter().map(Into::into).collect()));
    let mut spent_by = e.spent_by;
    spent_by.sort();
    info.push_kv_end(
        "spentby",
        UniValue::Arr(spent_by.iter().map(|t| t.to_string().into()).collect()),
    );
    info.push_kv_end("instantlock", islocks.is_locked(&e.txid).to_string().into());
    info.push_kv_end("unbroadcast", e.unbroadcast.into());
    (e.txid.to_string(), info)
}

/// `DEFAULT_MAX_RAW_TX_FEE_RATE`, in duffs per kB.
// Core: v24.0.2 src/node/transaction.h:26
const DEFAULT_MAX_RAW_TX_FEE_RATE: i64 = dash_consensus::subsidy::COIN / 10;

// Core: v24.0.2 src/policy/packages.h:16
const MAX_PACKAGE_COUNT: usize = 25;

/// What `BroadcastTransaction` uses beside the coins tip: the node's pool, its validation
/// interface queue and its peer manager. Call it from an RPC worker, never from V's thread or
/// the signals thread (the queue's drain would wait on itself).
// Core: v24.0.2 src/node/transaction.cpp:33-125
pub trait TxBroadcast: Send + Sync {
    /// `node.mempool` is set.
    fn has_mempool(&self) -> bool;
    /// `node.peerman` is set.
    fn has_peerman(&self) -> bool;
    /// The pool holds `tx`'s txid, and `tx` is not a version-2 asset unlock of another instance
    /// (which the pool refreshes in place).
    // Core: v24.0.2 src/node/transaction.cpp:57-62
    fn in_mempool(&self, tx: &Tx) -> bool;
    /// `ProcessTransaction(tx, test_accept, bypass_limits)`'s result.
    fn process_transaction(
        &self,
        tx: &TxRef,
        test_accept: bool,
        bypass_limits: bool,
    ) -> Result<Processed, NoVerdict>;
    /// `mempool.PrioritiseTransaction(txid, fee_delta)`, waiting behind queued admissions until
    /// it applies; `false` when no pool is set.
    // Core: v24.0.2 src/txmempool.cpp:1878-1903
    fn prioritise(&self, txid: &TxHash, fee_delta: i64) -> bool;
    /// `mempool.AddUnbroadcastTx(txid)`.
    fn add_unbroadcast(&self, txid: &TxHash);
    /// Returns once every validation interface callback queued so far has run.
    // Core: v24.0.2 src/validationinterface.cpp:150-174
    fn sync_with_validation_queue(&self);
    /// `peerman.RelayTransaction(txid)`.
    fn relay_transaction(&self, txid: &TxHash);
}

/// A `VALID` result's `m_vsize`, `m_base_fees`, `m_effective_feerate` and
/// `m_txids_fee_calculations`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AcceptedTx {
    pub vsize: Option<i64>,
    pub base_fee: Option<i64>,
    pub effective_feerate: Option<i64>,
    pub effective_includes: Option<Vec<TxHash>>,
}

/// `MempoolAcceptResult` as `BroadcastTransaction` and `testmempoolaccept` read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Processed {
    /// `VALID`, with its size and fee figures.
    Valid(AcceptedTx),
    /// `INVALID`, with its state.
    Invalid(TxValidationError),
    /// `MEMPOOL_ENTRY`: not `VALID`, with a valid state.
    MempoolEntry,
}

/// Why `ProcessTransaction` gave no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoVerdict {
    /// The pool has no admission worker: not attached yet, or stopped.
    NoMempool,
    /// V dropped the turn: a `Fatal` or an `AbortNode` (D9).
    Aborted,
}

impl From<NoVerdict> for RpcError {
    fn from(e: NoVerdict) -> RpcError {
        match e {
            NoVerdict::NoMempool => no_mempool(),
            NoVerdict::Aborted => Aborted.into(),
        }
    }
}

/// Registers `sendrawtransaction`, `testmempoolaccept` and `prioritisetransaction` on `server`.
// Core: v24.0.2 src/rpc/mempool.cpp:885-902; src/rpc/mining.cpp:1107-1124
pub fn register_broadcast_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChainState>,
    node: Arc<dyn TxBroadcast>,
) {
    let (send_chain, send_node) = (Arc::clone(&chain), Arc::clone(&node));
    server.append_command(Command::new("sendrawtransaction", move |r| {
        sendrawtransaction(&*send_chain, &*send_node, &r.params)
    }));
    let test_node = Arc::clone(&node);
    server.append_command(Command::new("testmempoolaccept", move |r| {
        testmempoolaccept(&*chain, &*test_node, &r.params)
    }));
    server.append_command(Command::new("prioritisetransaction", move |r| {
        prioritisetransaction(&*node, &r.params)
    }));
}

/// Core's `sendrawtransaction`: `DecodeHexTx`, `ParseFeeRate` of `maxfeerate` (its default
/// 0.10) as the fee of the transaction's virtual size (its size: no sigops are counted), then
/// `BroadcastTransaction` with relay and the callback wait. `instantsend` is ignored.
// Core: v24.0.2 src/rpc/mempool.cpp:38-90
fn sendrawtransaction(
    chain: &dyn NodeChainState,
    node: &dyn TxBroadcast,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let tx = decode_hex_tx(param(params, 0).get_str()?).ok_or_else(|| {
        RpcError::new(
            RPC_DESERIALIZATION_ERROR,
            "TX decode failed. Make sure the tx has at least one input.",
        )
    })?;
    let max_fee_rate = match param(params, 1) {
        UniValue::Null => DEFAULT_MAX_RAW_TX_FEE_RATE,
        v => parse_fee_rate(v)?,
    };
    let max_tx_fee = get_fee(max_fee_rate, tx.size());
    let bypass_limits = match param(params, 3) {
        UniValue::Null => false,
        v => v.get_bool()?,
    };
    let tx: TxRef = Arc::new(tx);
    broadcast_transaction(chain, node, &tx, max_tx_fee, bypass_limits)?;
    Ok(tx.txid().to_string().into())
}

/// `ParseFeeRate`: `AmountFromValue`, below one DASH per kB.
// Core: v24.0.2 src/rpc/util.cpp:80-85
fn parse_fee_rate(v: &UniValue) -> Result<i64, RpcError> {
    let val = amount_from_value(v)?;
    if val >= dash_consensus::subsidy::COIN {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Fee rates larger than or equal to 1DASH/kB are not accepted",
        ));
    }
    Ok(val)
}

/// `CFeeRate::GetFee`: the integer product divided as a double and rounded up; at least one
/// duff (of the rate's sign) for a nonzero size.
// Core: v24.0.2 src/policy/feerate.cpp:23-37
fn get_fee(duffs_per_k: i64, size: u32) -> i64 {
    let fee = (duffs_per_k.wrapping_mul(i64::from(size)) as f64 / 1000.0).ceil() as i64;
    if fee == 0 && size != 0 {
        duffs_per_k.signum()
    } else {
        fee
    }
}

/// `TransactionError`'s cases `BroadcastTransaction` returns here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TransactionError {
    MissingInputs,
    AlreadyInChain,
    MempoolRejected,
    MempoolError,
    MaxFeeExceeded,
}

/// `JSONRPCTransactionError`: `RPCErrorFromTransactionError`'s code, with `err_string`, or
/// `TransactionErrorString` when it is empty.
// Core: v24.0.2 src/rpc/util.cpp:1156-1182; src/util/error.cpp:13-45
fn transaction_error(err: TransactionError, err_string: &str) -> RpcError {
    let (code, text) = match err {
        TransactionError::MissingInputs => (RPC_TRANSACTION_ERROR, "Inputs missing or spent"),
        TransactionError::AlreadyInChain => {
            (RPC_TRANSACTION_ALREADY_IN_CHAIN, "Transaction already in block chain")
        }
        TransactionError::MempoolRejected => {
            (RPC_TRANSACTION_REJECTED, "Transaction rejected by mempool")
        }
        TransactionError::MempoolError => (RPC_TRANSACTION_ERROR, "Mempool internal error"),
        TransactionError::MaxFeeExceeded => (
            RPC_TRANSACTION_ERROR,
            "Fee exceeds maximum configured by user (e.g. -maxtxfee, maxfeerate)",
        ),
    };
    RpcError::new(
        code,
        if err_string.is_empty() {
            text
        } else {
            err_string
        },
    )
}

/// `HandleATMPError`: `TX_MISSING_INPUTS` is `MISSING_INPUTS`, any other refusal
/// `MEMPOOL_REJECTED`, each with `state.ToString()`; a valid state is `MEMPOOL_ERROR`, "Valid".
// Core: v24.0.2 src/node/transaction.cpp:21-31; src/consensus/validation.h:105-116
fn handle_atmp_error(state: Option<&TxValidationError>) -> RpcError {
    match state {
        Some(state) if state.result() == TxValidationResult::MissingInputs => {
            transaction_error(TransactionError::MissingInputs, &state.to_string())
        }
        Some(state) => transaction_error(TransactionError::MempoolRejected, &state.to_string()),
        None => transaction_error(TransactionError::MempoolError, "Valid"),
    }
}

/// `EnsureMemPool`'s error: the node's pool is gone or not there yet.
// Core: v24.0.2 src/rpc/server_util.cpp:47-53
fn no_mempool() -> RpcError {
    RpcError::new(RPC_CLIENT_MEMPOOL_DISABLED, "Mempool disabled or instance not found")
}

/// Core's `BroadcastTransaction(node, tx, max_tx_fee, relay = true, wait_callback = true,
/// bypass_limits)`. Core's asserts of the chainman, the pool and the peer manager are their
/// `Ensure*` errors. Core holds `cs_main` from the chain check to the admission; here each step
/// is its own turn, so a refusal is checked again against the coins tip and the pool, and a
/// block or a relayed copy that came between gives the answer Core gives after it.
// Core: v24.0.2 src/node/transaction.cpp:33-125
fn broadcast_transaction(
    chain: &dyn NodeChainState,
    node: &dyn TxBroadcast,
    tx: &TxRef,
    max_tx_fee: i64,
    bypass_limits: bool,
) -> Result<(), RpcError> {
    let coins = chain.chain_state().ok_or_else(no_chainman)?;
    if !node.has_mempool() {
        return Err(no_mempool());
    }
    if !node.has_peerman() {
        return Err(p2p_disabled());
    }
    let txid = tx.txid();
    let outs: Vec<OutPoint> =
        (0..tx.outputs().len() as u32).map(|n| OutPoint::new(txid, n)).collect();
    let in_chain = || -> Result<(), RpcError> {
        if coins.txouts(&outs)?.iter().any(Option::is_some) {
            return Err(transaction_error(TransactionError::AlreadyInChain, ""));
        }
        Ok(())
    };
    in_chain()?;
    if !node.in_mempool(tx) {
        // `None`: refused, and the pool now holds the transaction.
        let process = |test_accept| -> Result<Option<Option<i64>>, RpcError> {
            let state = match node.process_transaction(tx, test_accept, bypass_limits)? {
                Processed::Valid(accepted) => return Ok(Some(accepted.base_fee)),
                Processed::Invalid(state) => Some(state),
                Processed::MempoolEntry => None,
            };
            in_chain()?;
            if node.in_mempool(tx) {
                return Ok(None);
            }
            Err(handle_atmp_error(state.as_ref()))
        };
        if max_tx_fee > 0 {
            let Some(fee) = process(true)? else {
                node.relay_transaction(&txid);
                return Ok(());
            };
            // Core's `m_base_fees.value()` on a valid result without one throws.
            let fee = fee.ok_or_else(|| RpcError::Runtime("bad optional access".into()))?;
            if fee > max_tx_fee {
                return Err(transaction_error(TransactionError::MaxFeeExceeded, ""));
            }
        }
        if process(false)?.is_some() {
            node.add_unbroadcast(&txid);
            node.sync_with_validation_queue();
        }
    }
    node.relay_transaction(&txid);
    Ok(())
}

/// Core's `testmempoolaccept` for one transaction; more than one is refused until package
/// acceptance is built. Its `CHECK_NONFATAL` answers in the short form (D41).
// Core: v24.0.2 src/rpc/mempool.cpp:96-238
fn testmempoolaccept(
    chain: &dyn NodeChainState,
    node: &dyn TxBroadcast,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let raw_txs = get_array(param(params, 0))?;
    if raw_txs.is_empty() || raw_txs.len() > MAX_PACKAGE_COUNT {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("Array must contain between 1 and {MAX_PACKAGE_COUNT} transactions."),
        ));
    }
    let max_fee_rate = match param(params, 1) {
        UniValue::Null => DEFAULT_MAX_RAW_TX_FEE_RATE,
        v => parse_fee_rate(v)?,
    };
    let mut txns = Vec::with_capacity(raw_txs.len());
    for rawtx in raw_txs {
        let hex = rawtx.get_str()?;
        let tx = decode_hex_tx(hex).ok_or_else(|| {
            RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                format!("TX decode failed: {hex} Make sure the tx has at least one input."),
            )
        })?;
        txns.push(Arc::new(tx));
    }
    if !node.has_mempool() {
        return Err(no_mempool());
    }
    let _coins = chain.chain_state().ok_or_else(no_chainman)?;
    if txns.len() > 1 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Package testmempoolaccept is not built"));
    }
    let tx = &txns[0];
    let result = node.process_transaction(tx, true, false)?;
    let mut inner = UniValue::new_object();
    inner.push_kv_end("txid", tx.txid().to_string().into());
    match result {
        Processed::MempoolEntry => {
            return Err(RpcError::new(
                RPC_MISC_ERROR,
                "Internal bug detected: tx_result.m_result_type != MempoolAcceptResult::ResultType::MEMPOOL_ENTRY",
            ));
        }
        Processed::Valid(accepted) => {
            let opt = || RpcError::Runtime("bad optional access".into());
            let fee = accepted.base_fee.ok_or_else(opt)?;
            let vsize = accepted.vsize.ok_or_else(opt)?;
            let max_raw_tx_fee = get_fee(max_fee_rate, vsize as u32);
            if max_raw_tx_fee != 0 && fee > max_raw_tx_fee {
                inner.push_kv_end("allowed", false.into());
                inner.push_kv_end("reject-reason", "max-fee-exceeded".into());
            } else {
                let effective_feerate = accepted.effective_feerate.ok_or_else(opt)?;
                let includes = accepted.effective_includes.ok_or_else(opt)?;
                inner.push_kv_end("allowed", true.into());
                inner.push_kv_end("vsize", vsize.into());
                let mut fees = UniValue::new_object();
                fees.push_kv_end("base", amount(fee));
                fees.push_kv_end("effective-feerate", amount(effective_feerate));
                fees.push_kv_end(
                    "effective-includes",
                    UniValue::Arr(includes.into_iter().map(|id| id.to_string().into()).collect()),
                );
                inner.push_kv_end("fees", fees);
            }
        }
        Processed::Invalid(state) => {
            inner.push_kv_end("allowed", false.into());
            let reason = if state.result() == TxValidationResult::MissingInputs {
                "missing-inputs"
            } else {
                state.reject_reason()
            };
            inner.push_kv_end("reject-reason", reason.into());
        }
    }
    Ok(UniValue::Arr(vec![inner]))
}

// Core: v24.0.2 src/rpc/mining.cpp:499-528
fn prioritisetransaction(node: &dyn TxBroadcast, params: &UniValue) -> Result<UniValue, RpcError> {
    let txid: TxHash = parse_hash_v(param(params, 0), "txid")?;
    let fee_delta = get_int_i64(param(params, 1))?;
    if !node.prioritise(&txid, fee_delta) {
        return Err(no_mempool());
    }
    Ok(true.into())
}

#[cfg(test)]
mod broadcast_tests;
#[cfg(test)]
mod tests;
