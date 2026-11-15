use dash_dml::ProTxHash;
use dash_primitives::BlockHash;

use super::diff::{bad_block_text, block_arg};
use super::{BlockFailure, EvoError, MnListRequest, NodeEvo};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::core_write::TxJsonParams;
use crate::protocol::{
    RPC_INTERNAL_ERROR, RPC_INVALID_ADDRESS_OR_KEY, RPC_INVALID_PARAMETER, RpcError,
};
use crate::request::{JsonRpcRequest, get_int_i32, param, parse_bool_v};

pub(super) fn list(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let value = param(&request.params, 0);
    let kind = if value.is_null() {
        "registered"
    } else {
        value.get_str()?
    };
    match kind {
        "wallet" => {
            return Err(RpcError::Runtime(
                "\"protx list wallet\" not supported when wallet is disabled".into(),
            ));
        }
        "registered" | "valid" | "evo" => {}
        _ => return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid type specified")),
    }
    let detailed = param(&request.params, 1);
    if !detailed.is_null() {
        parse_bool_v(detailed, "detailed", evo.permissive_bool())?;
    }
    let height = param(&request.params, 2);
    let height = if height.is_null() {
        None
    } else {
        Some(get_int_i32(height)?)
    };
    evo.mn_list(MnListRequest::Height(height)).map_err(info_error)?;
    Err(info_error(EvoError::Unavailable))
}

pub(super) fn info(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let hash: BlockHash = parse_hash_v(param(&request.params, 0), "proTxHash")?;
    let pro_tx_hash = ProTxHash::from(*hash.as_bytes());
    let value = param(&request.params, 1);
    let block = if value.is_null() {
        None
    } else {
        Some(parse_hash_v(value, "blockHash")?)
    };
    evo.mn_list(MnListRequest::Info {
        pro_tx_hash,
        block,
    })
    .map_err(info_error)?;
    Err(info_error(EvoError::Unavailable))
}

pub(super) fn list_diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    let base = block_arg(param(&request.params, 0), "baseBlock", true);
    let block = block_arg(param(&request.params, 1), "block", true);
    if let Err(error) = &base.syntax {
        return Err(error.clone());
    }
    evo.mn_list_diff(base, block).map_err(list_diff_error)?;
    Err(list_diff_error(EvoError::Unavailable))
}

pub(super) fn info_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            failure: BlockFailure::NotFound(_),
            ..
        } => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"),
        EvoError::Block {
            name,
            value_str,
            ..
        } => bad_block_text(name, &value_str),
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}

pub(super) fn list_diff_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            name,
            value_str,
            ..
        } => bad_block_text(name, &value_str),
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}
