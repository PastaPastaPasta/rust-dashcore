//! The raw-transaction commands without mempool or wallet: `rpc/rawtransaction.cpp`'s
//! `createrawtransaction`, `decoderawtransaction` and `decodescript` (RP1c), which need no
//! coins, and `combinerawtransaction` and `signrawtransactionwithkey` (BL-688), which read the
//! coins tip. `decoderawtransaction` reads only Core's process BLS flag from the chain (D41).
//! With no mempool until MP1a, the coins are the tip's alone.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_consensus::CoreSerialize as _;
use dash_consensus::coins::Coin;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_consensus::script::opcodes::OP_RETURN;
use dash_consensus::script::{
    SIGHASH_ALL, SIGHASH_ANYONECANPAY, SIGHASH_NONE, SIGHASH_SINGLE, is_pay_to_script_hash,
    push_data,
};

use crate::UniValue;
use crate::blockchain::{NodeChain, NodeChainState, no_chainman, parse_hash_v, process_bls};
use crate::core_write::{
    TxJsonParams, TxVerbosity, TxoutType, script_to_asm_str, script_to_univ, solver, tx_to_univ,
};
use crate::descriptor::hex;
use crate::key_io::{
    Destination, KeyIoParams, decode_destination, decode_secret, encode_destination, hash160,
};
use crate::mining::decode_hex_tx;
use crate::protocol::{
    RPC_DESERIALIZATION_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_TYPE_ERROR,
    RPC_VERIFY_ERROR, RpcError,
};
use crate::request::{
    find_value, get_array, get_int_i32, get_int_i64, get_obj, param, parse_hex_v,
};
use crate::server::{Command, NodeControl, RpcServer};
use crate::sign::{
    Creator, DummySigningProvider, FillableSigningProvider, SignatureData, data_from_transaction,
    produce_signature, sign_transaction, update_input,
};

/// `LOCKTIME_MAX`.
// Core: script/script.h:44
const LOCKTIME_MAX: i64 = 0xffff_ffff;
/// `CTxIn::SEQUENCE_FINAL`.
const SEQUENCE_FINAL: u32 = 0xffff_ffff;
/// `CTxIn::MAX_SEQUENCE_NONFINAL`.
const MAX_SEQUENCE_NONFINAL: u32 = SEQUENCE_FINAL - 1;
/// `CMutableTransaction`'s default version.
// Core: primitives/transaction.cpp:71
const DEFAULT_TX_VERSION: i16 = 2;

/// Registers the five commands on `server`.
// Core: rpc/rawtransaction.cpp:2029-2054
pub fn register_rawtransaction_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    state: Arc<dyn NodeChainState>,
    tx_json: TxJsonParams,
) {
    let p = Arc::new(tx_json);
    let control = Arc::clone(server.control());
    let decode = Arc::clone(&p);
    server.append_command(Command::new("decoderawtransaction", move |r| {
        decoderawtransaction(&*chain, &*control, &decode, &r.params)
    }));
    let script = Arc::clone(&p);
    server.append_command(Command::new("decodescript", move |r| {
        decodescript(&script.key_io, &r.params)
    }));
    let create = Arc::clone(&p);
    server.append_command(Command::new("createrawtransaction", move |r| {
        createrawtransaction(&create.key_io, &r.params)
    }));
    let combine = Arc::clone(&state);
    server.append_command(Command::new("combinerawtransaction", move |r| {
        combinerawtransaction(&*combine, &r.params)
    }));
    server.append_command(Command::new("signrawtransactionwithkey", move |r| {
        signrawtransactionwithkey(&*state, &p.key_io, &r.params)
    }));
}

/// Core's `decoderawtransaction`: `DecodeHexTx`, then `TxToUniv` without the hex, its BLS
/// fields under the process flag.
// Core: rpc/rawtransaction.cpp:933-961
fn decoderawtransaction(
    chain: &dyn NodeChain,
    control: &dyn NodeControl,
    p: &TxJsonParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let tx = decode_hex_tx(param(params, 0).get_str()?)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;
    tx_to_univ(&tx, None, false, None, TxVerbosity::Details, p, process_bls(chain))
        .map_err(|e| e.into_rpc_error(control))
}

/// Core's `decodescript`: `ScriptToUniv` with the address, then the P2SH address wrapping the
/// script unless it is P2SH itself.
// Core: rpc/rawtransaction.cpp:963-1009
fn decodescript(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let arg = param(params, 0);
    let script = if arg.get_str()?.is_empty() {
        Vec::new()
    } else {
        parse_hex_v(arg, "argument")?
    };
    let mut r = script_to_univ(&script, false, true, p);
    if solver(&script) != TxoutType::ScriptHash {
        let p2sh = Destination::ScriptHash(hash160(&script));
        r.push_kv_end("p2sh", encode_destination(&p2sh, p).into());
    }
    Ok(r)
}

/// Core's `createrawtransaction`: `ConstructTransaction`, then `EncodeHexTx`.
// Core: rpc/rawtransaction.cpp:905-931; rpc/rawtransaction_util.cpp:126-143
fn createrawtransaction(p: &KeyIoParams, params: &UniValue) -> Result<UniValue, RpcError> {
    let mut lock_time = 0;
    let locktime = param(params, 2);
    if !locktime.is_null() {
        let n = get_int_i64(locktime)?;
        if !(0..=LOCKTIME_MAX).contains(&n) {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Invalid parameter, locktime out of range",
            ));
        }
        lock_time = n as u32;
    }
    let tx = Transaction {
        version: DEFAULT_TX_VERSION,
        tx_type: TxType::Spend,
        inputs: add_inputs(param(params, 0), lock_time)?,
        outputs: add_outputs(param(params, 1), p)?,
        lock_time,
        extra_payload: Vec::new(),
    };
    Ok(hex(&tx.core_serialize()).into())
}

/// `AddInputs`: each input's `txid` and `vout`, and its `sequence` when that is a number, else
/// the default the lock time selects.
// Core: rpc/rawtransaction_util.cpp:24-65
fn add_inputs(inputs: &UniValue, lock_time: u32) -> Result<Vec<TxIn>, RpcError> {
    let inputs = if inputs.is_null() {
        &[]
    } else {
        get_array(inputs)?
    };
    let mut out = Vec::with_capacity(inputs.len());
    for input in inputs {
        let o = get_obj(input)?;
        let txid: TxHash = parse_hash_v(find_value(o, "txid"), "txid")?;
        let vout = find_value(o, "vout");
        if !vout.is_num() {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Invalid parameter, missing vout key",
            ));
        }
        let index = get_int_i32(vout)?;
        if index < 0 {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "Invalid parameter, vout cannot be negative",
            ));
        }
        let mut sequence = if lock_time != 0 {
            MAX_SEQUENCE_NONFINAL
        } else {
            SEQUENCE_FINAL
        };
        let given = find_value(o, "sequence");
        if given.is_num() {
            let n = get_int_i64(given)?;
            if !(0..=i64::from(SEQUENCE_FINAL)).contains(&n) {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Invalid parameter, sequence number is out of range",
                ));
            }
            sequence = n as u32;
        }
        out.push(TxIn {
            prevout: OutPoint {
                hash: txid,
                index: index as u32,
            },
            script_sig: Vec::new().into(),
            sequence,
        });
    }
    Ok(out)
}

/// `AddOutputs`: an object, or an array of one-key objects joined into one (duplicates kept), in
/// key order; `data` once as an `OP_RETURN` output, each address once.
// Core: rpc/rawtransaction_util.cpp:67-124
fn add_outputs(outputs: &UniValue, p: &KeyIoParams) -> Result<Vec<TxOut>, RpcError> {
    if outputs.is_null() {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Invalid parameter, output argument must be non-null",
        ));
    }
    let joined: Vec<(String, UniValue)>;
    let members = if let UniValue::Obj(members) = outputs {
        members.as_slice()
    } else {
        let mut dict = Vec::new();
        for output in get_array(outputs)? {
            let UniValue::Obj(pair) = output else {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Invalid parameter, key-value pair not an object as expected",
                ));
            };
            if pair.len() != 1 {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Invalid parameter, key-value pair must contain exactly one key",
                ));
            }
            dict.extend(pair.iter().cloned());
        }
        joined = dict;
        joined.as_slice()
    };

    let mut destinations = Vec::new();
    let mut has_data = false;
    let mut out = Vec::with_capacity(members.len());
    for (name, _) in members {
        // `outputs[name_]`: the first member of that name.
        let value = find_value(members, name);
        if name == "data" {
            if has_data {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Invalid parameter, duplicate key: data",
                ));
            }
            has_data = true;
            let data = parse_hex_v(&UniValue::Str(value.get_val_str().to_owned()), "Data")?;
            let mut script = vec![OP_RETURN];
            push_data(&mut script, &data);
            out.push(TxOut {
                value: 0,
                script_pubkey: script.into(),
            });
        } else {
            let destination = decode_destination(name, p).ok_or_else(|| {
                RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, format!("Invalid Dash address: {name}"))
            })?;
            if destinations.contains(&destination) {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    format!("Invalid parameter, duplicated address: {name}"),
                ));
            }
            let script = destination.script();
            destinations.push(destination);
            out.push(TxOut {
                value: amount_from_value(value)?,
                script_pubkey: script.into(),
            });
        }
    }
    Ok(out)
}

/// Core's `AmountFromValue(value, 8)`: a number or string whose text `ParseFixedPoint` reads with
/// eight decimals, within `MoneyRange`.
// Core: rpc/util.cpp:68-78
fn amount_from_value(value: &UniValue) -> Result<i64, RpcError> {
    if !value.is_num() && !value.is_str() {
        return Err(RpcError::new(RPC_TYPE_ERROR, "Amount is not a number or string"));
    }
    let amount = dash_governance::strings::parse_fixed_point(value.get_val_str().as_bytes(), 8)
        .ok_or_else(|| RpcError::new(RPC_TYPE_ERROR, "Invalid amount"))?;
    if !dash_consensus::money_range(amount) {
        return Err(RpcError::new(RPC_TYPE_ERROR, "Amount out of range"));
    }
    Ok(amount)
}

/// The coins of `outs` from the coins tip, read in one turn: `EnsureChainman`, then Core's
/// `cs_main` scope. Only the unspent coins are kept.
// Core: rpc/rawtransaction.cpp:1053-1067; node/coin.cpp:12-24
fn find_coins(
    chain: &dyn NodeChainState,
    outs: impl IntoIterator<Item = OutPoint>,
) -> Result<BTreeMap<OutPoint, Coin>, RpcError> {
    let mut outs: Vec<OutPoint> = outs.into_iter().collect();
    outs.sort();
    outs.dedup();
    let coins = chain.chain_state().ok_or_else(no_chainman)?.txouts(&outs)?;
    assert_eq!(coins.len(), outs.len(), "txouts answers each outpoint");
    Ok(outs.into_iter().zip(coins).filter_map(|(out, coin)| Some((out, coin?))).collect())
}

/// Core's `combinerawtransaction`: every variant decoded (the first failure wins) before any
/// coin is read; then each input of the first, its signature data merged from every variant
/// long enough to have it (each checked against its own transaction) and its scriptSig
/// produced over the merged transaction with no keys.
// Core: rpc/rawtransaction.cpp:1011-1095
fn combinerawtransaction(
    chain: &dyn NodeChainState,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let txs = get_array(param(params, 0))?;
    let mut variants = Vec::with_capacity(txs.len());
    for (idx, tx) in txs.iter().enumerate() {
        let tx = decode_hex_tx(tx.get_str()?).ok_or_else(|| {
            RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                format!("TX decode failed for tx {idx}. Make sure the tx has at least one input."),
            )
        })?;
        variants.push(tx.into_raw());
    }
    let Some(first) = variants.first() else {
        return Err(RpcError::new(RPC_DESERIALIZATION_ERROR, "Missing transactions"));
    };
    let mut merged = first.clone();
    let coins = find_coins(chain, merged.inputs.iter().map(|txin| txin.prevout))?;
    for i in 0..merged.inputs.len() {
        let Some(coin) = coins.get(&merged.inputs[i].prevout) else {
            return Err(RpcError::new(RPC_VERIFY_ERROR, "Input not found or already spent"));
        };
        let script_pubkey = coin.script_pubkey().as_slice();
        let mut sigdata = SignatureData::default();
        for variant in variants.iter().filter(|v| v.inputs.len() > i) {
            sigdata.merge(data_from_transaction(variant, i, script_pubkey));
        }
        let creator = Creator {
            tx: &merged,
            n_in: i,
            hash_type: SIGHASH_ALL,
        };
        produce_signature(&DummySigningProvider, &creator, script_pubkey, &mut sigdata);
        update_input(&mut merged, i, &sigdata);
    }
    Ok(hex(&merged.core_serialize()).into())
}

/// Core's `signrawtransactionwithkey`: the transaction, the keys, the coins tip's coins of its
/// inputs, `ParsePrevouts`, then the sighash type and `SignTransaction`.
// Core: rpc/rawtransaction.cpp:1097-1190; rpc/rawtransaction_util.cpp:242-273
fn signrawtransactionwithkey(
    chain: &dyn NodeChainState,
    p: &KeyIoParams,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let mut mtx = decode_hex_tx(param(params, 0).get_str()?)
        .ok_or_else(|| {
            RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                "TX decode failed. Make sure the tx has at least one input.",
            )
        })?
        .into_raw();
    let mut keystore = FillableSigningProvider::default();
    for k in get_array(param(params, 1))? {
        let key = decode_secret(k.get_str()?, p)
            .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key"))?;
        keystore.add_key(key);
    }
    let mut coins = find_coins(chain, mtx.inputs.iter().map(|txin| txin.prevout))?;
    parse_prevouts(param(params, 2), &mut keystore, &mut coins)?;
    let hash_type = parse_sighash_string(param(params, 3))?;
    let errors = sign_transaction(&mut mtx, &keystore, &coins, hash_type);
    Ok(sign_result(&mtx, &errors))
}

/// `SignTransactionResultToJSON`: `hex`, `complete`, then `errors` when there are any, by input
/// index. ("Missing amount" needs segwit, so it never arises.)
// Core: rpc/rawtransaction_util.cpp:144-153, 253-273
fn sign_result(mtx: &Transaction, errors: &BTreeMap<usize, String>) -> UniValue {
    let mut result = UniValue::new_object();
    result.push_kv_end("hex", hex(&mtx.core_serialize()).into());
    result.push_kv_end("complete", errors.is_empty().into());
    if !errors.is_empty() {
        let mut list = UniValue::new_array();
        for (&i, error) in errors {
            let txin = &mtx.inputs[i];
            let mut entry = UniValue::new_object();
            entry.push_kv_end("txid", txin.prevout.hash.to_string().into());
            entry.push_kv_end("vout", u64::from(txin.prevout.index).into());
            entry.push_kv_end("scriptSig", hex(txin.script_sig.as_bytes()).into());
            entry.push_kv_end("sequence", u64::from(txin.sequence).into());
            entry.push_kv_end("error", error.as_str().into());
            list.push_back(entry);
        }
        result.push_kv_end("errors", list);
    }
    result
}

/// `RPCTypeCheckObj(o, {key: type}, fAllowNull=false)` for one key: null or absent is
/// "Missing", another type is named.
// Core: rpc/util.cpp:41-58
fn type_check_field(
    o: &[(String, UniValue)],
    key: &str,
    expected: dash_args::univalue::ValueType,
) -> Result<(), RpcError> {
    let v = find_value(o, key);
    if v.is_null() {
        return Err(RpcError::new(RPC_TYPE_ERROR, format!("Missing {key}")));
    }
    if v.value_type() != expected {
        return Err(RpcError::new(
            RPC_TYPE_ERROR,
            format!(
                "JSON value of type {} for field {key} is not of expected type {}",
                v.value_type().name(),
                expected.name()
            ),
        ));
    }
    Ok(())
}

/// `ParsePrevouts`: each previous output given replaces the coin of its outpoint (amount 0
/// unless `amount` is present, height 1), after a known coin's script is checked; a P2SH one
/// adds its redeem script to the keystore, whose refusal of an oversized script is ignored.
// Core: rpc/rawtransaction_util.cpp:156-240
fn parse_prevouts(
    prevtxs: &UniValue,
    keystore: &mut FillableSigningProvider,
    coins: &mut BTreeMap<OutPoint, Coin>,
) -> Result<(), RpcError> {
    use dash_args::univalue::ValueType;

    if prevtxs.is_null() {
        return Ok(());
    }
    for p in get_array(prevtxs)? {
        let UniValue::Obj(prev_out) = p else {
            return Err(RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                "expected object with {\"txid'\",\"vout\",\"scriptPubKey\"}",
            ));
        };
        // `RPCTypeCheckObj` walks its `std::map`: the keys in byte order.
        type_check_field(prev_out, "scriptPubKey", ValueType::Str)?;
        type_check_field(prev_out, "txid", ValueType::Str)?;
        type_check_field(prev_out, "vout", ValueType::Num)?;

        let txid: TxHash = parse_hash_v(find_value(prev_out, "txid"), "txid")?;
        let n_out = get_int_i32(find_value(prev_out, "vout"))?;
        if n_out < 0 {
            return Err(RpcError::new(RPC_DESERIALIZATION_ERROR, "vout cannot be negative"));
        }
        let out = OutPoint {
            hash: txid,
            index: n_out as u32,
        };
        let script_pubkey = parse_hex_v(find_value(prev_out, "scriptPubKey"), "scriptPubKey")?;

        if let Some(coin) = coins.get(&out)
            && coin.script_pubkey().as_slice() != script_pubkey.as_slice()
        {
            return Err(RpcError::new(
                RPC_DESERIALIZATION_ERROR,
                format!(
                    "Previous output scriptPubKey mismatch:\n{}\nvs:\n{}",
                    script_to_asm_str(coin.script_pubkey().as_slice(), false),
                    script_to_asm_str(&script_pubkey, false)
                ),
            ));
        }
        let amount = if prev_out.iter().any(|(k, _)| k == "amount") {
            amount_from_value(find_value(prev_out, "amount"))?
        } else {
            0
        };
        coins.insert(out, Coin::new(amount, &script_pubkey, 1, false));

        // A redeem script given with the keys is added to the keystore so it can sign.
        if is_pay_to_script_hash(&script_pubkey) {
            // Absent or null is "Missing redeemScript" here, so Core's later -8 "Missing
            // redeemScript" checks never fire.
            type_check_field(prev_out, "redeemScript", ValueType::Str)?;
            let script = parse_hex_v(find_value(prev_out, "redeemScript"), "redeemScript")?;
            keystore.add_cscript(&script);
            if script_pubkey != Destination::ScriptHash(hash160(&script)).script() {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "redeemScript does not match scriptPubKey",
                ));
            }
        }
    }
    Ok(())
}

/// `ParseSighashString`: null is `SIGHASH_ALL`; else `SighashFromStr`'s six names.
// Core: rpc/util.cpp:307-317; core_read.cpp:170-187
fn parse_sighash_string(sighash: &UniValue) -> Result<i32, RpcError> {
    if sighash.is_null() {
        return Ok(SIGHASH_ALL);
    }
    let s = sighash.get_str()?;
    let anyone = SIGHASH_ANYONECANPAY;
    match s {
        "ALL" => Ok(SIGHASH_ALL),
        "ALL|ANYONECANPAY" => Ok(SIGHASH_ALL | anyone),
        "NONE" => Ok(SIGHASH_NONE),
        "NONE|ANYONECANPAY" => Ok(SIGHASH_NONE | anyone),
        "SINGLE" => Ok(SIGHASH_SINGLE),
        "SINGLE|ANYONECANPAY" => Ok(SIGHASH_SINGLE | anyone),
        _ => Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!("{s} is not a valid sighash parameter."),
        )),
    }
}

#[cfg(test)]
mod tests;
