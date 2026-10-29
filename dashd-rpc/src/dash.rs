//! The Dash commands of `rpc/node.cpp` served so far: `mnsync` (SP1). `spork` and `sporkupdate`
//! are RP2b's.

use std::sync::Arc;

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

/// Registers every command of this module on `server`.
pub fn register_dash_commands(server: &Arc<RpcServer>, sync: Arc<dyn NodeSync>) {
    let control = Arc::clone(server.control());
    server.append_command(Command::new("mnsync", move |r| mnsync(&*control, &*sync, &r.params)));
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

#[cfg(test)]
mod tests;
