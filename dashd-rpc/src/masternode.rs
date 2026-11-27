//! The `masternode` family of `rpc/masternode.cpp`: `connect` (BL-690a) and `status`. Any other
//! subcommand reaches `masternode` itself, as `protx` and `quorum` answer one they do not serve.

use std::sync::Arc;

use dash_dml::{Dmn, ProTxHash};
use dash_netaddr::Service;
use dash_primitives::OutPoint;

use crate::UniValue;
use crate::core_write::TxJsonParams;
use crate::core_write::dmn::{mn_type_description, state_to_json};
use crate::evo::{EvoError, info_error};
use crate::peers::{NodeNet, p2p_disabled};
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RpcError};
use crate::request::{param, parse_bool_v};
use crate::server::{Command, NodeControl, RpcServer};

/// `node.active_ctx->nodeman`'s reads `masternode status` makes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveMnStatus {
    /// `GetOutPoint()`: the null outpoint until READY.
    pub outpoint: OutPoint,
    pub service: Service,
    /// `GetProTxHash()`; `None` stands for the null hash.
    pub pro_tx_hash: Option<ProTxHash>,
    /// `GetStateString()`.
    pub state: &'static str,
    /// `GetStatus()`.
    pub status: String,
}

pub trait NodeMasternode: Send + Sync {
    /// `None` on a node without `active_ctx`.
    fn active_mn(&self) -> Option<ActiveMnStatus>;
    /// `GetListAtChainTip().GetMN(pro_tx_hash)`.
    fn tip_mn(&self, pro_tx_hash: &ProTxHash) -> Result<Option<Arc<Dmn>>, EvoError>;
}

// Core: v24.0.2 src/rpc/masternode.cpp:832-847
pub fn register_masternode_commands(
    server: &RpcServer,
    node: Arc<dyn NodeMasternode>,
    net: Arc<dyn NodeNet>,
    p: TxJsonParams,
    permissive_bool: bool,
) {
    // Core: v24.0.2 src/rpc/masternode.cpp:523-543
    server.append_command(Command::new("masternode", |_| {
        Err(RpcError::new(RPC_INVALID_PARAMETER, "Must be a valid command"))
    }));
    server.append_command(Command::new("masternode connect", move |r| {
        connect(net.as_ref(), &r.params, permissive_bool)
    }));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("masternode status", move |_| {
        status(node.as_ref(), &p, control.as_ref())
    }));
}

/// Core's `masternode connect`: numeric lookup with port default 0 (never DNS), v2transport
/// check, synchronous masternode connection open, then `IsConnected(..., AllNodes)`.
// Core: v24.0.2 src/rpc/masternode.cpp:47-87
fn connect(
    net: &dyn NodeNet,
    params: &UniValue,
    permissive_bool: bool,
) -> Result<UniValue, RpcError> {
    let address = param(params, 0).get_str()?;
    let Some(addr) = net.lookup_service(address) else {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            format!("Incorrect masternode address {address}"),
        ));
    };
    let node_v2 = net.v2_transport().ok_or_else(p2p_disabled)?;
    let v2_arg = param(params, 1);
    let use_v2 = if v2_arg.is_null() {
        node_v2
    } else {
        parse_bool_v(v2_arg, "v2transport", permissive_bool)?
    };
    if use_v2 && !node_v2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Error: Adding v2transport connections requires -v2transport init flag to be set.",
        ));
    }
    let connected = net.open_masternode_connection(&addr, use_v2).ok_or_else(p2p_disabled)?;
    if !connected {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            format!("Couldn't connect to masternode {address}"),
        ));
    }
    Ok("successfully connected".into())
}

/// rdashd has no assumeutxo snapshot, so `quorumParticipation` is always true and `status`
/// never carries the snapshot suffix.
// Core: v24.0.2 src/rpc/masternode.cpp:198-224
fn status(
    node: &dyn NodeMasternode,
    p: &TxJsonParams,
    control: &dyn NodeControl,
) -> Result<UniValue, RpcError> {
    let Some(active) = node.active_mn() else {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "This node does not run an active masternode.",
        ));
    };
    let mut obj = UniValue::new_object();
    let outpoint = format!("{}-{}", active.outpoint.txid(), active.outpoint.index());
    obj.push_kv_end("outpoint", outpoint.into());
    obj.push_kv_end("service", active.service.to_string().into());
    let dmn = match &active.pro_tx_hash {
        Some(pro_tx_hash) => node.tip_mn(pro_tx_hash).map_err(info_error)?,
        None => None,
    };
    if let Some(dmn) = dmn {
        obj.push_kv_end("proTxHash", dmn.pro_tx_hash.to_string().into());
        obj.push_kv_end("type", mn_type_description(dmn.mn_type.0).into());
        obj.push_kv_end("collateralHash", dmn.collateral.txid().to_string().into());
        obj.push_kv_end("collateralIndex", i64::from(dmn.collateral.index()).into());
        let state = state_to_json(dmn.mn_type.0, &dmn.hot, &dmn.cold, p)
            .map_err(|e| e.into_rpc_error(control))?;
        obj.push_kv_end("dmnState", state);
    }
    obj.push_kv_end("state", active.state.into());
    obj.push_kv_end("status", active.status.into());
    obj.push_kv_end("quorumParticipation", true.into());
    Ok(obj)
}

#[cfg(test)]
mod tests;
