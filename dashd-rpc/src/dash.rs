//! The Dash commands of `rpc/node.cpp` served so far: `mnsync` (SP1) and `spork` (RP2b).
//! `sporkupdate` is RP2b's.

use std::sync::Arc;

use dash_evo::spork::{SPORK_DEFS, SporkId};

use crate::UniValue;
use crate::protocol::RpcError;
use crate::request::param;
use crate::server::{Command, NodeControl, RpcServer};

/// Core's `CMasternodeSync`, as `mnsync` uses it. `dashd-node` implements it over SP1's sync.
pub trait MnSync {
    /// `GetAssetID()`.
    fn asset_id(&self) -> i32;
    /// `GetAssetName()`.
    fn asset_name(&self) -> &'static str;
    /// `GetAssetStartTime()`.
    fn asset_start_time(&self) -> i64;
    /// `GetAttempt()`.
    fn attempt(&self) -> i32;
    /// `IsBlockchainSynced()`.
    fn is_blockchain_synced(&self) -> bool;
    /// `IsSynced()`.
    fn is_synced(&self) -> bool;
    /// `SwitchToNextAsset()`.
    fn switch_to_next_asset(&self);
    /// `Reset(fForce)`, notifying.
    fn reset(&self, force: bool);
}

/// The node's `NodeContext::mn_sync`.
pub trait NodeSync: Send + Sync {
    /// `None` until init constructs it (step 7, before the chainstate loads), and once shutdown
    /// has stopped the RPC server.
    fn mn_sync(&self) -> Option<Arc<dyn MnSync>>;
}

/// Core's `CSporkManager`, as `spork` reads it. `dashd-node` implements it over SP0's manager.
pub trait SporkRead {
    /// `GetSporkValue(nSporkID)`.
    fn value(&self, id: SporkId) -> i64;
    /// `IsSporkActive(nSporkID)`.
    fn is_active(&self, id: SporkId) -> bool;
}

/// The node's `NodeContext::sporkman`.
pub trait NodeSporks: Send + Sync {
    /// `None` until init constructs it (step 6), and once shutdown has stopped the RPC server.
    fn sporkman(&self) -> Option<Arc<dyn SporkRead>>;
}

/// Registers every command of this module on `server`.
pub fn register_dash_commands(
    server: &Arc<RpcServer>,
    sync: Arc<dyn NodeSync>,
    sporks: Arc<dyn NodeSporks>,
) {
    let control = Arc::clone(server.control());
    server.append_command(Command::new("mnsync", move |r| mnsync(&*control, &*sync, &r.params)));
    let control = Arc::clone(server.control());
    server.append_command(Command::new("spork", move |r| spork(&*control, &*sporks, &r.params)));
}

/// Core's `mnsync`: `status`, `next` or `reset`, and `"failure"` for any other mode. The server
/// has checked that the one argument is a string.
// Core: rpc/node.cpp:95-149
fn mnsync(
    control: &dyn NodeControl,
    sync: &dyn NodeSync,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let mode = param(params, 0).get_str()?;
    let Some(sync) = sync.mn_sync() else {
        return Err(crate::commands::internal_bug(
            control,
            "node.mn_sync",
            file!(),
            line!(),
            "mnsync",
        ));
    };
    Ok(match mode {
        "status" => {
            let mut status = UniValue::new_object();
            status.push_kv_end("AssetID", sync.asset_id().into());
            status.push_kv_end("AssetName", sync.asset_name().into());
            status.push_kv_end("AssetStartTime", sync.asset_start_time().into());
            status.push_kv_end("Attempt", sync.attempt().into());
            status.push_kv_end("IsBlockchainSynced", sync.is_blockchain_synced().into());
            status.push_kv_end("IsSynced", sync.is_synced().into());
            status
        }
        "next" => {
            sync.switch_to_next_asset();
            format!("sync updated to {}", sync.asset_name()).into()
        }
        "reset" => {
            sync.reset(true);
            "success".into()
        }
        _ => "failure".into(),
    })
}

/// Core's `spork`: every `sporkDefs` spork in Core's order with its value (`show`) or whether it
/// is active (`active`), and null for any other command. The server has checked that the one
/// argument is a string.
// Core: rpc/node.cpp:156-204
fn spork(
    control: &dyn NodeControl,
    sporks: &dyn NodeSporks,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let command = param(params, 0).get_str()?;
    let Some(sporks) = sporks.sporkman() else {
        return Err(crate::commands::internal_bug(
            control,
            "node.sporkman",
            file!(),
            line!(),
            "spork",
        ));
    };
    let entry: fn(&dyn SporkRead, SporkId) -> UniValue = match command {
        "show" => |s, id| s.value(id).into(),
        "active" => |s, id| s.is_active(id).into(),
        _ => return Ok(UniValue::Null),
    };
    let mut ret = UniValue::new_object();
    for def in &SPORK_DEFS {
        ret.push_kv_end(def.name, entry(&*sporks, def.id));
    }
    Ok(ret)
}

#[cfg(test)]
mod tests;
