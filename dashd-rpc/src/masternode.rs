//! The `masternode` family of `rpc/masternode.cpp`, `status` only so far. Any other
//! subcommand reaches `masternode` itself, as `protx` and `quorum` answer one they do not serve.

use std::sync::Arc;

use dash_dml::{Dmn, ProTxHash};
use dash_netaddr::Service;
use dash_primitives::OutPoint;

use crate::UniValue;
use crate::core_write::TxJsonParams;
use crate::core_write::dmn::{mn_type_description, state_to_json};
use crate::evo::{EvoError, info_error};
use crate::protocol::{RPC_INTERNAL_ERROR, RPC_INVALID_PARAMETER, RpcError};
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

// Core: v24.0.2 src/rpc/masternode.cpp:840-853
pub fn register_masternode_commands(
    server: &RpcServer,
    node: Arc<dyn NodeMasternode>,
    p: TxJsonParams,
) {
    // Core: v24.0.2 src/rpc/masternode.cpp:523-543
    server.append_command(Command::new("masternode", |_| {
        Err(RpcError::new(RPC_INVALID_PARAMETER, "Must be a valid command"))
    }));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("masternode status", move |_| {
        status(node.as_ref(), &p, control.as_ref())
    }));
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
