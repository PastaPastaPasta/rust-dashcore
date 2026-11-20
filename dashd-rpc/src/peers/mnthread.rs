//! Core's `setmnthreadactive` (regtest only): pauses or resumes the masternode connection
//! loop's opens ([`NodeNet::set_masternode_thread_active`]).

use std::sync::Arc;

use super::{NodeNet, p2p_disabled};
use crate::UniValue;
use crate::protocol::RpcError;
use crate::request::param;
use crate::server::{Command, NodeControl, RpcServer};

/// Registers `setmnthreadactive` on `server`, on every network as Core does; the regtest check
/// is the handler's.
pub fn register_mnthread_commands(server: &RpcServer, net: Arc<dyn NodeNet>) {
    let control = Arc::clone(server.control());
    server.append_command(Command::new("setmnthreadactive", move |r| {
        setmnthreadactive(&*control, &*net, &r.params)
    }));
}

/// Core's `setmnthreadactive`, after the wrapper's arity and type checks: the regtest check,
/// then the flag set and read back, `None` being `EnsureConnman`'s -31. Core checks the network
/// ID; `is_mockable_chain` is true on regtest only (RP1a-notes D78).
// Core: rpc/net.cpp:1103-1126; net.h:1325-1326
fn setmnthreadactive(
    control: &dyn NodeControl,
    net: &dyn NodeNet,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    if !control.is_mockable_chain() {
        return Err(RpcError::Runtime(
            "setmnthreadactive is for regression testing (-regtest mode) only.".into(),
        ));
    }
    let active = param(params, 0).get_bool()?;
    Ok(net.set_masternode_thread_active(active).ok_or_else(p2p_disabled)?.into())
}

#[cfg(test)]
mod tests;
