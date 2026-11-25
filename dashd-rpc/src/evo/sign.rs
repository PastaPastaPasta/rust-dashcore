//! `quorum sign`, `platformsign`, `selectquorum` and `verify`: the share manager's signing entry
//! points, the quorum selection and the recovered-signature check.

use dash_primitives::BlockHash;

use super::{EvoError, NodeEvo};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::descriptor::hex;
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RPC_MISC_ERROR, RpcError};
use crate::request::{JsonRpcRequest, get_int_i32, param, parse_bool_v, parse_hex, values};

/// This node's signature share, as `CreateSigShare` returns it: public, never a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnSigShare {
    pub quorum_hash: [u8; 32],
    pub member: u16,
    pub sign_hash: [u8; 32],
    pub signature: Vec<u8>,
}

/// The quorum `SelectQuorumForSigning` picks at the tip, and its recovery members' proTxHashes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectedQuorum {
    pub quorum_hash: [u8; 32],
    pub recovery_members: Vec<[u8; 32]>,
}

/// One signing request's arguments; `quorum_hash` `None` selects the quorum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignArgs {
    pub llmq_type: u8,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub quorum_hash: Option<[u8; 32]>,
}

/// Which quorum `quorum verify` checks a signature against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyAt {
    /// The latest quorums selected at the height (-1: the tip).
    Height(i32),
    /// The quorum named.
    Quorum([u8; 32]),
}

/// `quorum verify`'s arguments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyArgs {
    pub llmq_type: u8,
    pub id: [u8; 32],
    pub msg_hash: [u8; 32],
    pub signature: [u8; 96],
    pub at: VerifyAt,
}

/// `node.active_ctx->shareman` and `llmq_ctx.qman`'s selection, without waiting on the signing
/// or share workers.
pub trait NodeQuorumSign: Send + Sync {
    /// `node.active_ctx` exists: an active masternode's share manager.
    fn masternode_mode(&self) -> bool;
    /// `EnsureLLMQContext`.
    fn llmq_context(&self) -> Result<(), EvoError>;
    /// `Consensus::Params::llmqTypePlatform`.
    fn platform_type(&self) -> u8;
    fn async_sign_if_member(&self, args: SignArgs, allow_diff_msg: bool) -> Result<bool, EvoError>;
    /// `Ok(None)`: no quorum selected or found; `Ok(Some(None))`: no share.
    fn create_sig_share(&self, args: SignArgs) -> Result<Option<Option<OwnSigShare>>, EvoError>;
    fn select_quorum(
        &self,
        llmq_type: u8,
        id: &[u8; 32],
    ) -> Result<Option<SelectedQuorum>, EvoError>;
    /// `CBLSSignature::SetHexStr`'s check under `bls::bls_legacy_scheme`.
    fn signature_readable(&self, signature: &[u8; 96]) -> bool;
    /// `Ok(None)`: the quorum named was not found.
    fn verify(&self, args: VerifyArgs) -> Result<Option<bool>, EvoError>;
}

pub(super) type Handler =
    fn(&JsonRpcRequest, &dyn NodeEvo, &dyn NodeQuorumSign) -> Result<UniValue, RpcError>;

// Core: v24.0.0 src/rpc/quorums.cpp:601-628
pub(super) fn sign(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    node: &dyn NodeQuorumSign,
) -> Result<UniValue, RpcError> {
    let llmq_type = get_int_i32(param(&request.params, 0))? as u8;
    let rest = values(&request.params).into_iter().skip(1).cloned().collect();
    sign_helper(&UniValue::Arr(rest), llmq_type, evo, node)
}

// Core: v24.0.0 src/rpc/quorums.cpp:630-650
pub(super) fn platform_sign(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    node: &dyn NodeQuorumSign,
) -> Result<UniValue, RpcError> {
    sign_helper(&request.params, node.platform_type(), evo, node)
}

// Core: v24.0.0 src/rpc/quorums.cpp:514-583 quorum_sign_helper
fn sign_helper(
    params: &UniValue,
    llmq_type: u8,
    evo: &dyn NodeEvo,
    node: &dyn NodeQuorumSign,
) -> Result<UniValue, RpcError> {
    if !node.masternode_mode() {
        return Err(RpcError::new(RPC_INTERNAL_ERROR, "Only available in masternode mode."));
    }
    node.llmq_context().map_err(rpc_error)?;
    if !evo.llmq_type_known(llmq_type) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid LLMQ type"));
    }
    let id: BlockHash = parse_hash_v(param(params, 0), "id")?;
    let msg_hash: BlockHash = parse_hash_v(param(params, 1), "msgHash")?;
    let quorum = param(params, 2);
    let quorum_hash = if quorum.is_null() || quorum.get_str()?.is_empty() {
        None
    } else {
        Some(parse_hash_v::<BlockHash>(quorum, "quorumHash")?.into())
    };
    let submit = param(params, 3);
    let submit = submit.is_null() || parse_bool_v(submit, "submit", evo.permissive_bool())?;
    let args = SignArgs {
        llmq_type,
        id: id.into(),
        msg_hash: msg_hash.into(),
        quorum_hash,
    };
    if submit {
        let allow_diff_msg = llmq_type == node.platform_type();
        return Ok(node.async_sign_if_member(args, allow_diff_msg).map_err(rpc_error)?.into());
    }
    let share = node
        .create_sig_share(args)
        .map_err(rpc_error)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "quorum not found"))?
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "failed to create sigShare"))?;
    let mut obj = UniValue::new_object();
    obj.push_kv_end("llmqType", i64::from(llmq_type).into());
    obj.push_kv_end("quorumHash", display(share.quorum_hash).into());
    obj.push_kv_end("quorumMember", i64::from(share.member).into());
    obj.push_kv_end("id", id.to_string().into());
    obj.push_kv_end("msgHash", msg_hash.to_string().into());
    obj.push_kv_end("signHash", display(share.sign_hash).into());
    obj.push_kv_end("signature", hex(&share.signature).into());
    Ok(obj)
}

// Core: v24.0.0 src/rpc/quorums.cpp:817-868
pub(super) fn select_quorum(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    node: &dyn NodeQuorumSign,
) -> Result<UniValue, RpcError> {
    node.llmq_context().map_err(rpc_error)?;
    let llmq_type = get_int_i32(param(&request.params, 0))? as u8;
    if !evo.llmq_type_known(llmq_type) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid LLMQ type"));
    }
    let id: BlockHash = parse_hash_v(param(&request.params, 1), "id")?;
    let selected = node
        .select_quorum(llmq_type, &id.into())
        .map_err(rpc_error)?
        .ok_or_else(|| RpcError::new(RPC_MISC_ERROR, "no quorums active"))?;
    let mut ret = UniValue::new_object();
    ret.push_kv_end("quorumHash", display(selected.quorum_hash).into());
    let members = selected.recovery_members.into_iter().map(|m| display(m).into()).collect();
    ret.push_kv_end("recoveryMembers", UniValue::Arr(members));
    Ok(ret)
}

// Core: v24.0.0 src/rpc/quorums.cpp:664-724
pub(super) fn verify(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    node: &dyn NodeQuorumSign,
) -> Result<UniValue, RpcError> {
    node.llmq_context().map_err(rpc_error)?;
    let params = &request.params;
    let llmq_type = get_int_i32(param(params, 0))? as u8;
    if !evo.llmq_type_known(llmq_type) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid LLMQ type"));
    }
    let id: BlockHash = parse_hash_v(param(params, 1), "id")?;
    let msg_hash: BlockHash = parse_hash_v(param(params, 2), "msgHash")?;
    let signature = parse_hex(param(params, 3).get_str()?)
        .and_then(|bytes| <[u8; 96]>::try_from(bytes).ok())
        .filter(|sig| node.signature_readable(sig))
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "invalid signature format"))?;
    let (quorum, height) = (param(params, 4), param(params, 5));
    let at = if quorum.is_null() || (quorum.get_str()?.is_empty() && !height.is_null()) {
        VerifyAt::Height(if height.is_null() {
            -1
        } else {
            get_int_i32(height)?
        })
    } else {
        VerifyAt::Quorum(parse_hash_v::<BlockHash>(quorum, "quorumHash")?.into())
    };
    let args = VerifyArgs {
        llmq_type,
        id: id.into(),
        msg_hash: msg_hash.into(),
        signature,
        at,
    };
    let verified = node
        .verify(args)
        .map_err(rpc_error)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "quorum not found"))?;
    Ok(verified.into())
}

fn display(bytes: [u8; 32]) -> String {
    BlockHash::from(bytes).to_string()
}

fn rpc_error(error: EvoError) -> RpcError {
    error.into_rpc(|_| unreachable!("no block argument"))
}

#[cfg(test)]
mod tests;
