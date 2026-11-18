//! `getblockfrompeer` (BL-503): ask one peer for a block whose header the index has.

use std::sync::Arc;

use dash_primitives::BlockHash;

use super::{NodeChain, ensure_chainman, parse_hash_v};
use crate::UniValue;
use crate::peers::{NodeNet, p2p_disabled};
use crate::protocol::{RPC_MISC_ERROR, RpcError};
use crate::request::{get_int_i64, param};
use crate::server::{Command, RpcServer};

/// Registers `getblockfrompeer`, a blockchain command that reaches the chainman and the peer
/// manager.
pub fn register_fetch_commands(
    server: &RpcServer,
    chain: Arc<dyn NodeChain>,
    net: Arc<dyn NodeNet>,
) {
    server.append_command(Command::new("getblockfrompeer", move |r| {
        getblockfrompeer(&*chain, &*net, &r.params)
    }));
}

const PRUNED_AHEAD: &str = "In prune mode, only blocks that the node has already synced previously can be fetched from a peer";

/// Core's `getblockfrompeer`: the chainman, then the peer manager ([`NodeNet::v2_transport`]
/// is `None` without the network attachment that holds it, as for `addnode`), the hash, the
/// peer id, the header lookup, the prune and data checks, then `FetchBlock`. Core reads the
/// entry's data bit in a second `cs_main` section; the one read here answers as Core does when
/// the block arrives before the first. The prune check runs only for a chainman that prunes
/// ([`Chainman::prune_tip`](super::Chainman::prune_tip)), none until CH8 (BL-733).
// Core: rpc/blockchain.cpp:483-534
pub(super) fn getblockfrompeer(
    chain: &dyn NodeChain,
    net: &dyn NodeNet,
    params: &UniValue,
) -> Result<UniValue, RpcError> {
    let chainman = ensure_chainman(chain)?;
    net.v2_transport().ok_or_else(p2p_disabled)?;
    let hash: BlockHash = parse_hash_v(param(params, 0), "blockhash")?;
    let peer_id = get_int_i64(param(params, 1))?;
    let index = chainman
        .lookup_block_index(&hash)
        .ok_or_else(|| RpcError::new(RPC_MISC_ERROR, "Block header missing"))?;
    if let Some(tip) = chainman.prune_tip()
        && i64::from(index.height) > i64::from(tip)
    {
        return Err(RpcError::new(RPC_MISC_ERROR, PRUNED_AHEAD));
    }
    if index.have_data {
        return Err(RpcError::new(RPC_MISC_ERROR, "Block already downloaded"));
    }
    match net.fetch_block(&hash, peer_id) {
        None => Err(p2p_disabled()),
        Some(Err(msg)) => Err(RpcError::new(RPC_MISC_ERROR, msg)),
        Some(Ok(())) => Ok(UniValue::new_object()),
    }
}

#[cfg(test)]
mod tests;
