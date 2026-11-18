use super::{BlockArg, BlockFailure, BlockRef, EvoError, NodeEvo, SmlDiffFacts};
use crate::UniValue;
use crate::blockchain::parse_hash_v;
use crate::core_write::TxJsonParams;
use crate::protocol::{RPC_INTERNAL_ERROR, RpcError};
use crate::request::{JsonRpcRequest, bad_block_arg, get_int_i32, param, parse_bool_v};

pub(super) fn diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
    _p: &TxJsonParams,
) -> Result<UniValue, RpcError> {
    read_diff(request, evo)?;
    Err(diff_error(EvoError::Unavailable))
}

pub(super) fn read_diff(
    request: &JsonRpcRequest,
    evo: &dyn NodeEvo,
) -> Result<SmlDiffFacts, RpcError> {
    let base = block_arg(param(&request.params, 0), "baseBlock", false);
    let block = block_arg(param(&request.params, 1), "block", false);
    if let Err(RpcError::Type(message)) = &base.syntax {
        return Err(RpcError::Type(message.clone()));
    }
    let value = param(&request.params, 2);
    let extended = if value.is_null() {
        Ok(false)
    } else {
        parse_bool_v(value, "extended", evo.permissive_bool())
    };
    evo.sml_diff(base, block, extended).map_err(diff_error)
}

// Core: rpc/evo.cpp:1839
pub(super) fn block_arg(value: &UniValue, name: &'static str, index: bool) -> BlockArg {
    let syntax = parse_hash_v(value, name).map(BlockRef::Hash).or_else(|_| {
        if value.is_num() {
            return get_int_i32(value).map(BlockRef::Height);
        }
        let text = value.get_str()?;
        text.parse::<i32>().ok().map(BlockRef::Height).ok_or_else(|| bad_block_arg(name, value))
    });
    BlockArg {
        name,
        value_str: value.get_val_str().into(),
        syntax: if index {
            syntax.map_err(|_| bad_block_arg(name, value))
        } else {
            syntax
        },
    }
}

pub(super) fn diff_error(error: EvoError) -> RpcError {
    error.into_rpc(|error| match error {
        EvoError::Block {
            name,
            value_str,
            failure,
        } => match failure {
            BlockFailure::NotFound(hash) => RpcError::Runtime(format!("block {hash} not found")),
            BlockFailure::BadArg => bad_block_text(name, &value_str),
        },
        _ => RpcError::new(RPC_INTERNAL_ERROR, "Unexpected block argument for protx RPC"),
    })
}

pub(super) fn bad_block_text(name: &str, value_str: &str) -> RpcError {
    RpcError::Runtime(format!("{name} must be a block hash or chain height and not {value_str}"))
}
