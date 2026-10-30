//! P4b's block serving (P4b D2): Core's `GETBLOCKS`, `GETBLOCKTXN`, `FILTERLOAD`, `FILTERADD`
//! and `FILTERCLEAR` branches of `ProcessMessage`, as the [`BlockServer`] handler that
//! [`Registry::new`] installs for their rows, and `ProcessGetBlockData`, which
//! `ProcessGetData`'s block slot calls ([`process_get_block_data`]).
//!
//! Every read of the active chain is at the message's snapshot, as Core reads it under
//! `cs_main`, through `serve`'s helpers. The block itself comes from `recent` when its hash
//! matches, else from [`ChainAccess::read_block_at`] at the position read with the data test,
//! outside any lock V holds (P4b D3, D16).
//!
//! [`Registry::new`]: super::registry::Registry::new
//! [`ChainAccess::read_block_at`]: super::seams::ChainAccess::read_block_at

use std::sync::Arc;

use dash_consensus::primitives::write_compact_size;
use dash_consensus::{Block, BlockId, CoreSerialize};
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, Permissions, ServiceFlags};
use dash_p2p_core::{FilterAdd, FilterLoad, GetBlockTxn, GetBlocks, Inv, InvType, Inventory};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use super::bloom::BloomFilter;
use super::compact::{CompactBlock, draw_nonce};
use super::eviction::can_direct_fetch;
use super::logic::out_msg;
use super::merkle::merkle_block;
use super::registry::{HandlerCx, NetHandler};
use super::seams::{BlockPos, BlockRead, ChainSnapshot, EntryFacts, IsLockLookup, ViewExpired};
use super::serve::{active_run, block_request_allowed, entry_at, find_fork_in_global_index};
use super::stubs::MAX_LOCATOR_SZ;
use super::sync::{block_hash, inv_hash};

/// Core's `MAX_CMPCTBLOCK_DEPTH`: the deepest block served as a `cmpctblock`.
// Core: net_processing.cpp:160
pub const MAX_CMPCTBLOCK_DEPTH: i64 = 5;

/// Core's `MAX_BLOCKTXN_DEPTH`: the deepest block `getblocktxn` serves from.
// Core: net_processing.cpp:162
pub const MAX_BLOCKTXN_DEPTH: i64 = 10;

/// Core's `NODE_NETWORK_LIMITED_MIN_BLOCKS`.
// Core: net_processing.cpp:178
pub const NODE_NETWORK_LIMITED_MIN_BLOCKS: i64 = 288;

/// Core's `MIN_BLOCKS_TO_KEEP`.
// Core: validation.h:88
pub const MIN_BLOCKS_TO_KEEP: i64 = 288;

/// The most block invs one `getblocks` queues.
// Core: net_processing.cpp:4673
pub const GETBLOCKS_LIMIT: usize = 500;

/// The five commands P4b serves.
static SERVED: [CommandString; 5] = [
    CommandString::GETBLOCKS,
    CommandString::GETBLOCKTXN,
    CommandString::FILTERLOAD,
    CommandString::FILTERADD,
    CommandString::FILTERCLEAR,
];

/// The owner of the `getblocks`, `getblocktxn` and filter rows.
#[derive(Debug, Default)]
pub struct BlockServer;

impl NetHandler for BlockServer {
    fn commands(&self) -> &'static [CommandString] {
        &SERVED
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        match cmd.as_str() {
            "getblocks" => on_getblocks(cx, payload),
            "getblocktxn" => on_getblocktxn(cx, payload),
            _ => {
                on_filter(cx, cmd, payload);
                Ok(())
            }
        }
    }
}

/// Core's `GETBLOCKS` branch: up to 500 of the active chain's blocks after the locator's fork,
/// queued for section 8's `inv`, the 500th remembered as the continuation. Core first runs
/// `ActivateBestChain`, which V has already done for every block the snapshot shows (P4b D11).
// Core: net_processing.cpp:4636-4702 ProcessMessage (GETBLOCKS)
fn on_getblocks(cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
    let mut data = payload;
    let id = cx.id();
    let req: GetBlocks = cx.read(&mut data)?;
    if req.locator_hashes.len() > MAX_LOCATOR_SZ {
        tracing::debug!(target: "net", "getblocks locator size {} > {MAX_LOCATOR_SZ}, disconnect peer={id}", req.locator_hashes.len());
        cx.disconnect("getblocks locator size");
        return Ok(());
    }
    let snap = Arc::clone(&cx.snap);
    let tip_height = snap.tip.tip_height;
    let fork = find_fork_in_global_index(cx.chain, &snap, &req.locator_hashes);
    let start = (fork.height < tip_height).then(|| entry_at(cx.chain, &snap, fork.height + 1));
    let stop_text = if req.hash_stop == BlockHash::ZERO {
        "end".to_owned()
    } else {
        req.hash_stop.to_string()
    };
    tracing::debug!(target: "net", "getblocks {} to {stop_text} limit {GETBLOCKS_LIMIT} from peer={id}", start.map_or(-1, |e| i64::from(e.height)));
    let Some(start) = start else {
        return Ok(());
    };
    let spacing = cx.core.config().params.consensus.pow_target_spacing;
    let likely_to_have = MIN_BLOCKS_TO_KEEP - 3600 / spacing;
    let prune = cx.chain.prune_mode();
    let can_relay = cx.peer.can_relay();
    let last = start.height.saturating_add(GETBLOCKS_LIMIT as u32 - 1).min(tip_height);
    let run = active_run(cx.chain, &snap, &start, last);
    for (i, e) in run.iter().enumerate() {
        if e.hash == req.hash_stop {
            tracing::debug!(target: "net", "  getblocks stopping at {} {}", e.height, e.hash);
            break;
        }
        // If pruning, don't inv blocks unless we have on disk and are likely to still have for
        // some reasonable time window (1 hour) that block relay might require.
        if prune
            && (data_pos(cx, &snap, e.id).is_none()
                || i64::from(e.height) <= i64::from(tip_height) - likely_to_have)
        {
            tracing::debug!(target: "net", " getblocks stopping, pruned or too old block at {} {}", e.height, e.hash);
            break;
        }
        if can_relay {
            cx.state.block_inv.lock().inv_relay.push(e.hash);
        }
        if i + 1 == GETBLOCKS_LIMIT {
            // When this block is requested, we'll send an inv that'll trigger the peer to
            // getblocks the next batch of inventory.
            tracing::debug!(target: "net", "  getblocks stopping at limit {} {}", e.height, e.hash);
            cx.state.block_inv.lock().continuation = Some(e.hash);
            break;
        }
    }
    Ok(())
}

/// `GetBlockPos()` of `id` at `snap` when it has `BLOCK_HAVE_DATA`, read with that bit.
fn data_pos(cx: &HandlerCx<'_>, snap: &ChainSnapshot, id: BlockId) -> Option<BlockPos> {
    cx.chain.status(snap, id).and_then(|s| s.data_pos)
}

/// Core's `GETBLOCKTXN` branch: `blocktxn` from the most recent block, or from a stored block
/// at most `MAX_BLOCKTXN_DEPTH` deep; a deeper one is queued as a `getdata` for the full block.
// Core: net_processing.cpp:4704-4757 ProcessMessage (GETBLOCKTXN)
fn on_getblocktxn(cx: &mut HandlerCx<'_>, payload: &[u8]) -> Result<(), Throw> {
    let mut data = payload;
    let id = cx.id();
    let req: GetBlockTxn = cx.read(&mut data)?;
    let recent = cx.signals.recent();
    if let (Some(hash), Some(block)) = (recent.hash, &recent.block)
        && hash == *req.block_hash()
    {
        send_block_transactions(cx, block, &req);
        return Ok(());
    }
    let snap = Arc::clone(&cx.snap);
    let Some((e, pos)) = cx
        .chain
        .entry_by_hash(req.block_hash())
        .and_then(|e| Some((e, data_pos(cx, &snap, e.id)?)))
    else {
        tracing::debug!(target: "net", "Peer {id} sent us a getblocktxn for a block we don't have");
        return Ok(());
    };
    if i64::from(e.height) >= i64::from(snap.tip.tip_height) - MAX_BLOCKTXN_DEPTH {
        // If height is above MAX_BLOCKTXN_DEPTH then this block cannot get pruned after we
        // release cs_main above, so this read should never fail.
        let block = cx.chain.read_block_at(e.id, pos).block().expect("getblocktxn read (np:4740)");
        send_block_transactions(cx, &block, &req);
        return Ok(());
    }
    // An older block is answered with the full block, through the getdata queue, which the
    // message loop serves next without pausing.
    tracing::debug!(target: "net", "Peer {id} sent us a getblocktxn for a block > {MAX_BLOCKTXN_DEPTH} deep");
    cx.state.getdata_requests.push_back(Inventory {
        inv_type: InvType::Block,
        hash: inv_hash(req.block_hash()),
    });
    Ok(())
}

/// Core's `SendBlockTransactions`: the requested transactions as `blocktxn`, or +100 for an
/// index past the block's end, with nothing sent.
// Core: net_processing.cpp:3113-3124 PeerManagerImpl::SendBlockTransactions
fn send_block_transactions(cx: &mut HandlerCx<'_>, block: &Block, req: &GetBlockTxn) {
    let txs = block.txs();
    if req.indexes().iter().any(|&i| usize::from(i) >= txs.len()) {
        cx.misbehaving(100, "getblocktxn with out-of-bounds tx indices");
        return;
    }
    // `BlockTransactions`: the hash, then the transactions in Core's serialization.
    let mut payload = Vec::new();
    payload.extend_from_slice(req.block_hash().as_bytes());
    write_compact_size(req.indexes().len() as u64, &mut payload);
    for &i in req.indexes() {
        txs[usize::from(i)].core_write(&mut payload);
    }
    cx.push(OutMsg::new(CommandString::BLOCKTXN, payload));
}

/// Core's `FILTERLOAD`, `FILTERADD` and `FILTERCLEAR` branches (P4b D7). The filter exists only
/// on a peer with a tx relay: elsewhere `filterload` still reads and checks it, `filteradd`
/// neither stores nor scores, and `filterclear` returns.
// Core: net_processing.cpp:5529-5607 ProcessMessage (FILTERLOAD, FILTERADD, FILTERCLEAR)
fn on_filter(cx: &mut HandlerCx<'_>, cmd: &CommandString, payload: &[u8]) {
    let mut data = payload;
    let id = cx.id();
    if !cx.state.our_services.has(ServiceFlags::NODE_BLOOM) {
        tracing::debug!(target: "net|netconn", "{cmd} received despite not offering bloom services from peer={id}; disconnecting");
        cx.disconnect("bloom filter message without NODE_BLOOM");
        return;
    }
    match cmd.as_str() {
        "filterload" => {
            // An oversized filter throws before allocation (`LIMITED_VECTOR`); Core scores the
            // throw here, not in the outer catch.
            let msg: FilterLoad = match cx.read(&mut data) {
                Ok(msg) => msg,
                Err(e) => {
                    let why = format!("misformatted bloom filter. peer={id} error={}", e.what);
                    cx.misbehaving(100, &why);
                    return;
                }
            };
            let filter = BloomFilter::from(msg);
            if !filter.is_within_size_constraints() {
                // There is no excuse for sending a too-large filter.
                cx.misbehaving(100, "too-large bloom filter");
            } else if cx.state.tx_relay {
                cx.state.bloom = Some(filter);
                cx.state.relay_txs = true;
                cx.peer.set_bloom_filter_loaded(true);
                cx.peer.set_relays_txs(true);
            }
        }
        "filteradd" => {
            // Nodes must never send a data item larger than MAX_SCRIPT_ELEMENT_SIZE bytes: the
            // read is bounded before allocation.
            let msg: FilterAdd = match cx.read(&mut data) {
                Ok(msg) => msg,
                Err(e) => {
                    let why = format!("bad filteradd message. peer={id} error={}", e.what);
                    cx.misbehaving(100, &why);
                    return;
                }
            };
            if cx.state.tx_relay {
                match &mut cx.state.bloom {
                    Some(filter) => filter.insert(&msg.data),
                    None => cx.misbehaving(100, "bad filteradd message"),
                }
            }
        }
        _ => {
            if !cx.state.tx_relay {
                return;
            }
            cx.state.bloom = None;
            cx.state.relay_txs = true;
            cx.peer.set_bloom_filter_loaded(false);
            cx.peer.set_relays_txs(true);
        }
    }
}

/// What `ProcessGetBlockData` decides under `cs_main` before reading the block.
struct Serve {
    entry: EntryFacts,
    /// `block_pos`, taken with the `BLOCK_HAVE_DATA` test: the read uses it even if a prune has
    /// since marked the entry, as Core's does (P4b D16).
    data_pos: BlockPos,
    tip_height: u32,
    tip_hash: BlockHash,
    can_direct_fetch: bool,
}

/// Core's `ProcessGetBlockData` for one `getdata` block item (P4b D10): `block`, a filtered
/// block's `merkleblock` with its matched transactions and their locks, or a `cmpctblock` near
/// the tip; then the continuation `inv` when this was the last block of a `getblocks` batch.
/// An unknown or unstored block is ignored silently. Core first runs `ActivateBestChain` for a
/// block accepted but not yet connected, which V has already connected (P4b D11).
// Core: net_processing.cpp:2736-2900 PeerManagerImpl::ProcessGetBlockData
pub(super) fn process_get_block_data(
    cx: &mut HandlerCx<'_>,
    inv: &Inventory,
    islocks: &dyn IsLockLookup,
) {
    let id = cx.id();
    let recent = cx.signals.recent();
    // Core holds `cs_main` for the decision, so it reads one view: the message's snapshot, or
    // the newest one when that view expired before `BlockRequestAllowed` read it (DEC-292).
    let mut snap = Arc::clone(&cx.snap);
    let serve = loop {
        match decide(cx, &snap, inv) {
            Ok(Some(serve)) => break serve,
            Ok(None) => return,
            Err(ViewExpired) => snap = cx.chain.snapshot(),
        }
    };
    let e = serve.entry;
    let block = match (recent.hash, &recent.block) {
        (Some(hash), Some(block)) if hash == e.hash => Arc::clone(block),
        _ => match cx.chain.read_block_at(e.id, serve.data_pos) {
            BlockRead::Block(block) => block,
            BlockRead::Pruned => {
                tracing::debug!(target: "net", "Block was pruned before it could be read, disconnect peer={id}");
                cx.disconnect("block pruned before it could be read");
                return;
            }
            BlockRead::Failed => {
                tracing::error!("Cannot load block from disk, disconnect peer={id}");
                cx.disconnect("cannot load block from disk");
                return;
            }
        },
    };
    match inv.inv_type {
        InvType::FilteredBlock => send_filtered_block(cx, &block, islocks),
        // If a peer is asking for old blocks, we're almost guaranteed they won't have a useful
        // mempool to match against a compact block, and we don't feel like constructing the
        // object for them, so instead we respond with the full, non-compact block.
        InvType::CompactBlock
            if serve.can_direct_fetch
                && i64::from(e.height) >= i64::from(serve.tip_height) - MAX_CMPCTBLOCK_DEPTH =>
        {
            let payload = match (recent.hash, &recent.cmpct) {
                (Some(hash), Some(cmpct)) if hash == e.hash => cmpct.to_vec(),
                _ => CompactBlock::from_block(&block, draw_nonce(cx.core.rand())).wire_bytes(),
            };
            cx.push(OutMsg::new(CommandString::CMPCTBLOCK, payload));
        }
        _ => {
            cx.push(OutMsg::new(CommandString::BLOCK, block.core_serialize()));
        }
    }
    // Trigger the peer node to send a getblocks request for the next batch of inventory.
    let mut block_inv = cx.state.block_inv.lock();
    if block_inv.continuation == Some(e.hash) {
        // Send immediately. This must send even if redundant, and we want it right after the
        // last block so they don't wait for other stuff first.
        let tip = Inventory {
            inv_type: InvType::Block,
            hash: inv_hash(&serve.tip_hash),
        };
        block_inv.continuation = None;
        drop(block_inv);
        cx.push(out_msg(
            CommandString::INV,
            &Inv {
                inventory: vec![tip],
            },
        ));
    }
}

/// `ProcessGetBlockData`'s checks under `cs_main`, at `snap`: `None` when it returns there.
/// [`ViewExpired`] only before anything is logged, sent or set.
// Core: net_processing.cpp:2773-2808
fn decide(
    cx: &HandlerCx<'_>,
    snap: &ChainSnapshot,
    inv: &Inventory,
) -> Result<Option<Serve>, ViewExpired> {
    let id = cx.id();
    let Some(e) = cx.chain.entry_by_hash(&block_hash(inv)) else {
        return Ok(None);
    };
    if !block_request_allowed(cx, snap, &e)? {
        tracing::debug!(target: "net", "ProcessGetBlockData: ignoring request from peer={id} for old block that isn't in the main chain");
        return Ok(None);
    }
    // Core disconnects here once `-maxuploadtarget` is reached; the option is refused at init
    // (P4b D6), so the target is never reached.
    let tip_height = snap.tip.tip_height;
    // Avoid leaking prune-height by never sending blocks below the NODE_NETWORK_LIMITED
    // threshold.
    let services = cx.state.our_services;
    if !cx.peer.has_permission(Permissions::NO_BAN)
        && services.has(ServiceFlags::NODE_NETWORK_LIMITED)
        && !services.has(ServiceFlags::NODE_NETWORK)
        && i64::from(tip_height) - i64::from(e.height) > NODE_NETWORK_LIMITED_MIN_BLOCKS + 2
    {
        tracing::debug!(target: "net", "Ignore block request below NODE_NETWORK_LIMITED threshold, disconnect peer={id}");
        // Disconnect node and prevent it from stalling (would otherwise wait for the missing
        // block).
        cx.disconnect("block request below NODE_NETWORK_LIMITED threshold");
        return Ok(None);
    }
    // Pruned nodes may have deleted the block, so check whether it's available before trying
    // to send.
    let Some(data_pos) = data_pos(cx, snap, e.id) else {
        return Ok(None);
    };
    Ok(Some(Serve {
        entry: e,
        data_pos,
        tip_height,
        tip_hash: snap.tip.tip_hash,
        can_direct_fetch: can_direct_fetch(cx.core, snap),
    }))
}

/// A `MSG_FILTERED_BLOCK` reply: nothing without a filter; else `merkleblock`, then each matched
/// transaction, then the InstantSend lock of each that has one (P4b D9).
// Core: net_processing.cpp:2834-2859
fn send_filtered_block(cx: &mut HandlerCx<'_>, block: &Block, islocks: &dyn IsLockLookup) {
    if !cx.state.tx_relay {
        return;
    }
    let Some(filter) = cx.state.bloom.as_mut() else {
        return;
    };
    let (merkle, matched) = merkle_block(block, filter);
    cx.push(out_msg(CommandString::MERKLEBLOCK, &merkle));
    // CMerkleBlock just contains hashes, so also push any transactions in the block the client
    // did not see.
    for &(i, _) in &matched {
        cx.push(OutMsg::new(CommandString::TX, block.txs()[i].core_serialize()));
    }
    for (_, txid) in &matched {
        if let Some(islock) = islocks.islock_by_txid(txid) {
            cx.push(OutMsg::new(CommandString::ISDLOCK, islock));
        }
    }
}
