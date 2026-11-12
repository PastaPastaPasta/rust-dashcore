//! Core's `mnauth` (regtest only): marks a connected peer MNAUTH-verified with a given proTxHash
//! and operator key, as a peer's `mnauth` message would ([`NodeNet::try_mark_verified`]).

use std::sync::Arc;

use dash_num::Hash256;
use dash_pkc::bls::core::BlsPkValue;

use super::{NodeNet, p2p_disabled};
use crate::UniValue;
use crate::blockchain::{NodeChain, parse_hash_v, process_bls};
use crate::protocol::{RPC_INVALID_PARAMETER, RpcError};
use crate::request::{get_int_i64, param, parse_hex};
use crate::server::{Command, NodeControl, RpcServer};

/// Registers `mnauth` on `server`, on every network as Core does; the regtest check is the
/// handler's.
pub fn register_mnauth_commands(
    server: &RpcServer,
    net: Arc<dyn NodeNet>,
    chain: Arc<dyn NodeChain>,
) {
    let control = Arc::clone(server.control());
    server.append_command(Command::new("mnauth", move |r| {
        mnauth(&*control, &*net, &*chain, &r.params)
    }));
}

/// Core's `mnauth`, after the wrapper's arity and type checks: the network, the peer id, the
/// proTxHash, then the key, read in the basic scheme. The key's hash is `GetHash()`, the
/// SHA-256d of its serialization under the process flag at the call, not the scheme it was read
/// in. `MineBlocksOnDemand` (`fPowNoRetargeting`) and `IsMockableChain` are both true exactly on
/// regtest. Without a connection manager, which Core never meets while RPC answers, the answer
/// is `EnsureConnman`'s error rather than a null dereference (RP1a-notes D77).
// Core: rpc/node.cpp:291-325; bls/bls.h:141-162; net.cpp:4514-4539; chainparams.h:113, :124
fn mnauth(
    control: &dyn NodeControl,
    net: &dyn NodeNet,
    chain: &dyn NodeChain,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    if !control.is_mockable_chain() {
        return Err(RpcError::Runtime("mnauth for regression testing (-regtest mode) only".into()));
    }
    let node_id = get_int_i64(param(params, 0))?;
    let pro_tx_hash: Hash256 = parse_hash_v(param(params, 1), "proTxHash")?;
    if pro_tx_hash.is_null() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "proTxHash invalid"));
    }
    let key = parse_hex(param(params, 2).get_str()?)
        .map_or_else(BlsPkValue::invalid, |bytes| BlsPkValue::from_set_bytes_basic(&bytes));
    if !key.is_valid() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "publicKey invalid"));
    }
    let pubkey_hash = key.serialize_hash(process_bls(chain).scheme());
    net.try_mark_verified(node_id, pro_tx_hash.into(), pubkey_hash.into())
        .map(UniValue::from)
        .ok_or_else(p2p_disabled)
}

#[cfg(test)]
mod tests;
