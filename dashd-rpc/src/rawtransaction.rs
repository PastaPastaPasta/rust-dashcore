//! The raw-transaction utilities that need no coins, mempool or wallet (RP1c):
//! `rpc/rawtransaction.cpp`'s `createrawtransaction`, `decoderawtransaction` and `decodescript`.
//! `decoderawtransaction` reads only Core's process BLS flag from the chain (D41).

use std::sync::Arc;

use dash_consensus::CoreSerialize as _;
use dash_consensus::primitives::{OutPoint, Transaction, TxHash, TxIn, TxOut, TxType};
use dash_consensus::script::opcodes::OP_RETURN;
use dash_consensus::script::push_data;

use crate::UniValue;
use crate::blockchain::{NodeChain, parse_hash_v, process_bls};
use crate::core_write::{TxJsonParams, TxVerbosity, TxoutType, script_to_univ, solver, tx_to_univ};
use crate::descriptor::hex;
use crate::key_io::{Destination, KeyIoParams, decode_destination, encode_destination, hash160};
use crate::protocol::{
    RPC_DESERIALIZATION_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RPC_TYPE_ERROR,
    RpcError,
};
use crate::request::{
    find_value, get_array, get_int_i32, get_int_i64, get_obj, param, parse_hex_v,
};
use crate::server::{Command, NodeControl, RpcServer};

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

/// Registers the three commands on `server`.
// Core: rpc/rawtransaction.cpp:2029-2054
pub fn register_rawtransaction_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
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
    server.append_command(Command::new("createrawtransaction", move |r| {
        createrawtransaction(&p.key_io, &r.params)
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
    let tx = crate::mining::decode_hex_tx(param(params, 0).get_str()?)
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

#[cfg(test)]
mod tests;
