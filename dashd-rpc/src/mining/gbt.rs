//! `getblocktemplate` (RP3a-3): the JSON front end over the node's typed template engine
//! (dashd-mining's `gbt`), reached through [`BlockTemplates`]. This module parses the request,
//! encodes the template in Core's key order, and maps the engine's errors and proposal answers.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_args::strencodings::locale_independent_atoi;
use dash_consensus::CoreSerialize as _;
use dash_consensus::primitives::TxOut;
use dashd_chainstate::chain::NotTested;
use dashd_mining::gbt::{GbtError, GbtRequest, GbtTemplate, LongPoll, Mode};

use super::{ValidationState, bip22_validation_result, decode_hex_blk};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::descriptor::hex;
use crate::key_io::{KeyIoParams, encode_destination, extract_destination};
use crate::protocol::{RPC_DESERIALIZATION_ERROR, RPC_INVALID_PARAMETER, RPC_TYPE_ERROR, RpcError};
use crate::request::{find_value, get_obj, param};
use crate::server::{Command, RpcServer};

/// The engine's answer to a parsed request.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(clippy::large_enum_variant, reason = "one answer per call, moved once into the encoder")]
pub enum GbtAnswer {
    Template(GbtTemplate),
    /// `TestBlockValidity`'s state, or the answer Core gives the proposal without it.
    Proposal(Result<ValidationState, NotTested>),
}

/// The node's `getblocktemplate` engine.
pub trait BlockTemplates: Send + Sync {
    /// Core's `getblocktemplate` on a parsed request. Blocking: it runs on the RPC worker thread.
    fn get_block_template(&self, request: GbtRequest) -> Result<GbtAnswer, GbtError>;
}

/// Registers `getblocktemplate` on `server`, answering from `templates`; payment addresses are
/// encoded with `key_io`.
pub fn register_block_template_command(
    server: &RpcServer,
    templates: Arc<dyn BlockTemplates>,
    key_io: KeyIoParams,
) {
    let package_name = server.control().package_name();
    server.append_command(Command::new("getblocktemplate", move |r| {
        getblocktemplate(&*templates, &key_io, package_name, &r.params)
    }));
}

// Core: v24.0.2 src/rpc/mining.cpp:672-985
fn getblocktemplate(
    templates: &dyn BlockTemplates,
    key_io: &KeyIoParams,
    package_name: &str,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let request = parse_request(param(params, 0))?;
    match templates.get_block_template(request) {
        Ok(GbtAnswer::Template(t)) => Ok(template_json(&t, key_io)),
        Ok(GbtAnswer::Proposal(Ok(state))) => bip22_validation_result(&state),
        Ok(GbtAnswer::Proposal(Err(not_tested))) => Ok(not_tested.to_string().into()),
        Err(e) => Err(RpcError::new(e.code(), e.message(package_name))),
    }
}

// Core: v24.0.2 src/rpc/mining.cpp:677-737, :765-779
fn parse_request(template_request: &UniValue) -> Result<GbtRequest, RpcError> {
    let mut request = GbtRequest {
        mode: Mode::Template,
        rules: BTreeSet::new(),
        longpoll: LongPoll::None,
    };
    if template_request.is_null() {
        return Ok(request);
    }
    let oparam = get_obj(template_request)?;
    let mode = match find_value(oparam, "mode") {
        UniValue::Null => "template",
        UniValue::Str(mode) => mode.as_str(),
        _ => {
            request.mode = Mode::Invalid;
            return Ok(request);
        }
    };
    if mode == "proposal" {
        let UniValue::Str(data) = find_value(oparam, "data") else {
            return Err(RpcError::new(RPC_TYPE_ERROR, "Missing data String key for proposal"));
        };
        let block = decode_hex_blk(data)
            .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
        request.mode = Mode::Proposal(Arc::new(block));
        return Ok(request);
    }
    if let UniValue::Arr(rules) = find_value(oparam, "rules") {
        for rule in rules {
            request.rules.insert(rule.get_str()?.to_owned());
        }
    }
    if mode != "template" {
        request.mode = Mode::Invalid;
        return Ok(request);
    }
    request.longpoll = parse_longpoll(find_value(oparam, "longpollid"))?;
    Ok(request)
}

// Core: v24.0.2 src/rpc/mining.cpp:765-779
fn parse_longpoll(lpval: &UniValue) -> Result<LongPoll, RpcError> {
    let lpstr = match lpval {
        UniValue::Null => return Ok(LongPoll::None),
        UniValue::Str(lpstr) => lpstr,
        _ => return Ok(LongPoll::NonString),
    };
    let Some(head) = lpstr.get(..lpstr.len().min(64)) else {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!(
                "longpollid must be hexadecimal string (not '{}')",
                String::from_utf8_lossy(&lpstr.as_bytes()[..64])
            ),
        ));
    };
    let tip = parse_hash_v(&UniValue::Str(head.to_owned()), "longpollid")?;
    Ok(LongPoll::Id {
        tip,
        tx_updated: locale_independent_atoi(&lpstr[64..]) as u32,
    })
}

// Core: v24.0.2 src/rpc/mining.cpp:840-982
fn template_json(t: &GbtTemplate, key_io: &KeyIoParams) -> UniValue {
    let mut transactions = UniValue::new_array();
    for tx in &t.transactions {
        let mut entry = UniValue::new_object();
        entry.push_kv_end("data", hex(&tx.tx.core_serialize()).into());
        entry.push_kv_end("hash", tx.txid.to_string().into());
        let depends = tx.depends.iter().map(|&i| i64::from(i).into()).collect();
        entry.push_kv_end("depends", UniValue::Arr(depends));
        entry.push_kv_end("fee", tx.fee.into());
        entry.push_kv_end("sigops", tx.sigops.into());
        transactions.push_back(entry);
    }
    let mut vbavailable = UniValue::new_object();
    for (name, bit) in &t.vbavailable {
        vbavailable.push_kv_end(name.as_str(), i64::from(*bit).into());
    }

    let mut result = UniValue::new_object();
    result.push_kv_end("capabilities", strings(["proposal"]));
    result.push_kv_end("version", t.version.into());
    result.push_kv_end("rules", strings(&t.rules));
    result.push_kv_end("vbavailable", vbavailable);
    result.push_kv_end("vbrequired", t.vbrequired.into());
    result.push_kv_end("previousblockhash", t.previousblockhash.to_string().into());
    result.push_kv_end("transactions", transactions);
    result.push_kv_end("coinbaseaux", UniValue::new_object());
    result.push_kv_end("coinbasevalue", t.coinbasevalue.into());
    let (tip, tx_updated) = &t.longpollid;
    result.push_kv_end("longpollid", format!("{tip}{tx_updated}").into());
    result.push_kv_end("target", format!("{:064x}", t.target).into());
    result.push_kv_end("mintime", t.mintime.into());
    result.push_kv_end("mutable", strings(["time", "transactions", "prevblock"]));
    result.push_kv_end("noncerange", "00000000ffffffff".into());
    result.push_kv_end("sigoplimit", t.sigoplimit.into());
    result.push_kv_end("sizelimit", t.sizelimit.into());
    result.push_kv_end("curtime", t.curtime.into());
    result.push_kv_end("bits", format!("{:08x}", t.bits).into());
    result.push_kv_end("previousbits", format!("{:08x}", t.previousbits).into());
    result.push_kv_end("height", i64::from(t.height).into());
    result.push_kv_end("masternode", payments(&t.masternode, key_io));
    result.push_kv_end("masternode_payments_started", t.masternode_payments_started.into());
    result.push_kv_end("masternode_payments_enforced", t.masternode_payments_enforced.into());
    result.push_kv_end("superblock", payments(&t.superblock, key_io));
    result.push_kv_end("superblocks_started", t.superblocks_started.into());
    result.push_kv_end("superblocks_enabled", t.superblocks_enabled.into());
    result.push_kv_end("coinbase_payload", hex(&t.coinbase_payload).into());
    result
}

fn strings<S: AsRef<str>>(items: impl IntoIterator<Item = S>) -> UniValue {
    UniValue::Arr(items.into_iter().map(|s| s.as_ref().into()).collect())
}

/// Each payment's `payee` (empty when the script has no address), `script` and `amount`.
// Core: v24.0.2 src/rpc/mining.cpp:948-978
fn payments(outs: &[TxOut], key_io: &KeyIoParams) -> UniValue {
    let mut array = UniValue::new_array();
    for out in outs {
        let script = out.script_pubkey().as_bytes();
        let payee = extract_destination(script)
            .map_or_else(String::new, |dest| encode_destination(&dest, key_io));
        let mut obj = UniValue::new_object();
        obj.push_kv_end("payee", payee.into());
        obj.push_kv_end("script", hex(script).into());
        obj.push_kv_end("amount", out.value().into());
        array.push_back(obj);
    }
    array
}

#[cfg(test)]
mod tests;
