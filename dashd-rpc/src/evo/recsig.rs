//! `quorum hasrecsig`, `quorum getrecsig` and `quorum isconflicting`: the signing manager's
//! recovered-signature reads.

use dash_consensus::hash::sha256d;
use dash_evo::locks::RecoveredSig;
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::{EvoError, NodeEvo, parse_llmq_type};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::descriptor::hex;
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{JsonRpcRequest, param};

/// `llmq_ctx.sigman`'s recovered-signature reads. [`EvoError::LlmqUnavailable`] is
/// `EnsureLLMQContext`'s failure: no signing manager yet, or none any more.
pub trait NodeRecSigs: Send + Sync {
    fn llmq_context(&self) -> Result<(), EvoError>;
    fn has_recovered_sig(
        &self,
        llmq_type: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, EvoError>;
    fn get_recovered_sig(
        &self,
        llmq_type: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<Option<RecoveredSig>, EvoError>;
    fn is_conflicting(
        &self,
        llmq_type: u8,
        id: &[u8; 32],
        msg: &[u8; 32],
    ) -> Result<bool, EvoError>;
}

pub(super) type Handler =
    fn(&JsonRpcRequest, &dyn NodeEvo, &dyn NodeRecSigs) -> Result<UniValue, RpcError>;

// Core: v24.0.3 src/rpc/quorums.cpp:743-749
fn read_args(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    sigs: &dyn NodeRecSigs,
) -> Result<(u8, [u8; 32], [u8; 32]), RpcError> {
    sigs.llmq_context().map_err(rpc_error)?;
    let llmq_type = parse_llmq_type(param(&request.params, 0), evo)?;
    let id: BlockHash = parse_hash_v(param(&request.params, 1), "id")?;
    let msg_hash: BlockHash = parse_hash_v(param(&request.params, 2), "msgHash")?;
    Ok((llmq_type, id.into(), msg_hash.into()))
}

// Core: v24.0.3 src/rpc/quorums.cpp:730-754
pub(super) fn has(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    sigs: &dyn NodeRecSigs,
) -> Result<UniValue, RpcError> {
    let (t, id, msg) = read_args(request, evo, sigs)?;
    Ok(sigs.has_recovered_sig(t, &id, &msg).map_err(rpc_error)?.into())
}

// Core: v24.0.3 src/rpc/quorums.cpp:756-784
pub(super) fn get(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    sigs: &dyn NodeRecSigs,
) -> Result<UniValue, RpcError> {
    let (t, id, msg) = read_args(request, evo, sigs)?;
    match sigs.get_recovered_sig(t, &id, &msg).map_err(rpc_error)? {
        Some(sig) => Ok(recsig_json(&sig)),
        None => Err(RpcError::new(RPC_INVALID_PARAMETER, "recovered signature not found")),
    }
}

// Core: v24.0.3 src/rpc/quorums.cpp:786-810
pub(super) fn is_conflicting(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    sigs: &dyn NodeRecSigs,
) -> Result<UniValue, RpcError> {
    let (t, id, msg) = read_args(request, evo, sigs)?;
    Ok(sigs.is_conflicting(t, &id, &msg).map_err(rpc_error)?.into())
}

/// `hash` is the signature's serialization hash (`CBLSWrapper::GetHash`).
// Core: v24.0.0 src/llmq/core_write.cpp:103-113 CRecoveredSig::ToJson
fn recsig_json(sig: &RecoveredSig) -> UniValue {
    let mut ret = UniValue::new_object();
    ret.push_kv_end("llmqType", i64::from(sig.llmq_type.to_base()).into());
    ret.push_kv_end("quorumHash", sig.quorum_hash.to_string().into());
    ret.push_kv_end("id", BlockHash::from(sig.id).to_string().into());
    ret.push_kv_end("msgHash", BlockHash::from(sig.msg_hash).to_string().into());
    ret.push_kv_end("sig", hex(&sig.sig).into());
    ret.push_kv_end("hash", BlockHash::from(sha256d(&sig.sig)).to_string().into());
    ret
}

fn rpc_error(error: EvoError) -> RpcError {
    error.into_rpc(|_| unreachable!("no block argument"))
}

#[cfg(test)]
mod tests;
