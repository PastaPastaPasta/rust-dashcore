//! P2b's `getheaders`/`getheaders2` serving (P2b D5): Core's `GETHEADERS`/`GETHEADERS2` branch
//! of `ProcessMessage`, as the [`HeadersServer`] handler that [`Registry::new`] installs for the
//! two registry rows.
//!
//! The active chain is read through [`ChainAccess`] at the message's snapshot (R8), as Core reads
//! it under `cs_main`: `CChain::Contains`, `Next` and `FindForkInGlobalIndex` are built from
//! `GetAncestor` on the snapshot's tip ([`in_active_chain`], [`find_fork_in_global_index`]).
//!
//! [`Registry::new`]: super::registry::Registry::new

use std::sync::Arc;

use dash_consensus::BlockId;
use dash_consensus::pow::block_proof;
use dash_num::CompactTarget;
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, OutMsg, Permissions};
use dash_p2p_core::{GetHeaders, GetHeaders2, Headers, Headers2};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric;

use super::headers::headers_limit;
use super::logic::out_msg;
use super::registry::{HandlerCx, NetHandler};
use super::seams::{ChainAccess, ChainSnapshot, EntryFacts, ViewExpired, Work};
use super::stubs::MAX_LOCATOR_SZ;
use super::sync::uses_compressed_headers;

/// Core's `STALE_RELAY_AGE_LIMIT`: 30 days, in seconds.
// Core: net_processing.cpp:143
pub const STALE_RELAY_AGE_LIMIT: i64 = 30 * 24 * 60 * 60;

/// The two commands P2b serves.
static SERVED: [CommandString; 2] = [CommandString::GETHEADERS, CommandString::GETHEADERS2];

/// The owner of the `getheaders` and `getheaders2` rows.
#[derive(Debug, Default)]
pub struct HeadersServer;

impl NetHandler for HeadersServer {
    fn commands(&self) -> &'static [CommandString] {
        &SERVED
    }

    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<(), Throw> {
        on_getheaders(cx, cmd, payload)
    }
}

/// Core's `CChain::Contains(pindex)` for the snapshot's active chain.
// Core: chain.h:465-468 CChain::Contains
pub(super) fn in_active_chain(
    chain: &dyn ChainAccess,
    snap: &ChainSnapshot,
    e: &EntryFacts,
) -> bool {
    e.height <= snap.tip.tip_height && chain.ancestor(snap.tip.tip, e.height) == Some(e.id)
}

/// Core's `Chainstate::FindForkInGlobalIndex`: the first locator entry the active chain holds,
/// else genesis.
// Core: validation.cpp:182-197 Chainstate::FindForkInGlobalIndex
pub(super) fn find_fork_in_global_index(
    chain: &dyn ChainAccess,
    snap: &ChainSnapshot,
    locator: &[BlockHash],
) -> EntryFacts {
    locator
        .iter()
        .filter_map(|h| chain.entry_by_hash(h))
        .find(|e| in_active_chain(chain, snap, e))
        .unwrap_or_else(|| entry_at(chain, snap, 0))
}

/// The active chain's entry at `height` (at most the tip's).
pub(super) fn entry_at(chain: &dyn ChainAccess, snap: &ChainSnapshot, height: u32) -> EntryFacts {
    chain
        .ancestor(snap.tip.tip, height)
        .and_then(|id| chain.entry(id))
        .expect("the active chain holds every height up to its tip")
}

/// Core's `GetBlockProofEquivalentTime(to, from, tip, params)`: the chain-work difference
/// `to - from` in seconds of `tip`'s proof at `spacing`, saturated to ±`i64::MAX`.
// Core: chain.cpp:147-162 GetBlockProofEquivalentTime
pub fn block_proof_equivalent_time(to: Work, from: Work, tip_bits: u32, spacing: i64) -> i64 {
    let (r, sign) = if to > from {
        (to - from, 1)
    } else {
        (from - to, -1)
    };
    // `arith_uint256(int64)` keeps the low 64 bits; Core's `*` wraps at 2^256. `GetBlockProof`
    // is zero only for an `nBits` no header check accepts: Core's division would then throw,
    // `Arith256`'s gives zero (so the time is 0).
    let r =
        r.wrapping_mul(Work::from_u64(spacing as u64)) / block_proof(CompactTarget::new(tip_bits));
    if r.bits() > 63 {
        return sign * i64::MAX;
    }
    sign * r.low_u64() as i64
}

/// Core's `BlockRequestAllowed(pindex)`: on the active chain, or fully validated and less than a
/// month older than the best header in both time and work. [`ViewExpired`] when `snap`'s view
/// can no longer be read: a `valid_scripts` read on a newer view against `snap`'s best header
/// could serve a header Core refuses at every view between the two (DEC-292).
// Core: net_processing.cpp:2048-2055 PeerManagerImpl::BlockRequestAllowed
pub(super) fn block_request_allowed(
    cx: &HandlerCx<'_>,
    snap: &ChainSnapshot,
    e: &EntryFacts,
) -> Result<bool, ViewExpired> {
    if in_active_chain(cx.chain, snap, e) {
        return Ok(true);
    }
    let best = &snap.tip;
    let Some(best_entry) = cx.chain.entry(best.best_header) else {
        return Ok(false);
    };
    let spacing = cx.core.config().params.consensus.pow_target_spacing;
    Ok(cx.chain.valid_scripts(snap, e.id)?
        && i64::from(best.best_header_time) - i64::from(e.time) < STALE_RELAY_AGE_LIMIT
        && block_proof_equivalent_time(
            best.best_header_work,
            e.chain_work,
            best_entry.header.bits,
            spacing,
        ) < STALE_RELAY_AGE_LIMIT)
}

/// The entries Core's loop of `ActiveChain().Next` sends from `start`: up to `limit` of them,
/// through `hash_stop`. Also returns Core's `pindex` after the loop: the last entry sent when the
/// limit or the stop ended it, `None` when the chain did.
// Core: net_processing.cpp:4818-4824
fn collect(
    chain: &dyn ChainAccess,
    snap: &ChainSnapshot,
    start: Option<EntryFacts>,
    limit: usize,
    hash_stop: &BlockHash,
) -> (Vec<BlockHeader>, Option<BlockId>) {
    let Some(start) = start else {
        return (Vec::new(), None);
    };
    let run = if in_active_chain(chain, snap, &start) {
        // A stop on the active chain at or after `start` ends the run there, so the walk reads
        // only what is sent.
        let stop_height = chain
            .entry_by_hash(hash_stop)
            .filter(|e| e.height >= start.height && in_active_chain(chain, snap, e))
            .map_or(u32::MAX, |e| e.height);
        let last =
            start.height.saturating_add(limit as u32 - 1).min(snap.tip.tip_height).min(stop_height);
        active_run(chain, snap, &start, last)
    } else {
        // `Next` of an entry off the active chain is null: the loop sends `start` alone.
        vec![start]
    };
    let end = run[run.len() - 1];
    let ended_early = end.hash == *hash_stop || run.len() == limit;
    (run.iter().map(|e| e.header).collect(), ended_early.then_some(end.id))
}

/// The active chain's entries from `start` through height `last`, in order. It walks back from
/// `last` by `prev` (one `ancestor` call, then O(1) per entry; `ancestor` per height would be
/// quadratic in a mirror whose `GetAncestor` walks), and the entries are immutable, so it reads
/// the same entries Core's forward walk does.
pub(super) fn active_run(
    chain: &dyn ChainAccess,
    snap: &ChainSnapshot,
    start: &EntryFacts,
    last: u32,
) -> Vec<EntryFacts> {
    let mut run = Vec::with_capacity((last - start.height + 1) as usize);
    let mut e = entry_at(chain, snap, last);
    while e.id != start.id {
        let prev = e.prev;
        run.push(e);
        e = prev.and_then(|p| chain.entry(p)).expect("an active-chain entry's parent");
    }
    run.push(e);
    run.reverse();
    run
}

/// A `headers2` (`compressed`) or `headers` reply carrying `headers`.
fn headers_msg(compressed: bool, headers: Vec<BlockHeader>) -> OutMsg {
    if compressed {
        // The first header is always uncompressed and stays out of the version list.
        out_msg(CommandString::HEADERS2, &Headers2::reply(&headers))
    } else {
        // `CBlock`s, so each header carries the zero transaction count.
        out_msg(
            CommandString::HEADERS,
            &Headers {
                headers,
            },
        )
    }
}

/// Core's `GETHEADERS`/`GETHEADERS2` branch.
// Core: net_processing.cpp:4760-4855 ProcessMessage (GETHEADERS, GETHEADERS2)
fn on_getheaders(cx: &mut HandlerCx<'_>, cmd: &CommandString, payload: &[u8]) -> Result<(), Throw> {
    let mut data = payload;
    let id = cx.id();
    let compressed = *cmd == CommandString::GETHEADERS2;
    let (locator, hash_stop) = if compressed {
        let m: GetHeaders2 = cx.read(&mut data)?;
        (m.locator_hashes, m.hash_stop)
    } else {
        let m: GetHeaders = cx.read(&mut data)?;
        (m.locator_hashes, m.hash_stop)
    };

    if locator.len() > MAX_LOCATOR_SZ {
        tracing::debug!(target: "net", "{cmd} locator size {} > {MAX_LOCATOR_SZ}, disconnect peer={id}", locator.len());
        cx.disconnect("getheaders locator size");
        return Ok(());
    }

    if cx.chain.loading_blocks() {
        tracing::debug!(target: "net", "Ignoring {cmd} from peer={id} while importing/reindexing");
        return Ok(());
    }

    // Core holds `cs_main` from here to the reply (np:4776), so every read is at one view: the
    // message's snapshot, or the newest one when that view expired before `BlockRequestAllowed`
    // read it (DEC-292). Nothing is sent or stored before that read, so the restart is clean.
    let mut snap = Arc::clone(&cx.snap);
    let mut tries = 0u32;
    while let Err(ViewExpired) = serve_headers(cx, cmd, &snap, &locator, &hash_stop) {
        // Once, unless V writes past the bound between the load and the read.
        tries += 1;
        debug_assert!(tries < 1_000, "the newest view keeps expiring");
        snap = cx.chain.snapshot();
    }
    Ok(())
}

/// The branch under `cs_main`, at `snap`. [`ViewExpired`] only before anything is sent or stored.
// Core: net_processing.cpp:4776-4842
fn serve_headers(
    cx: &mut HandlerCx<'_>,
    cmd: &CommandString,
    snap: &ChainSnapshot,
    locator: &[BlockHash],
    hash_stop: &BlockHash,
) -> Result<(), ViewExpired> {
    let id = cx.id();
    let compressed = *cmd == CommandString::GETHEADERS2;
    // Note that if we were to be on a chain that forks from the checkpointed chain, then serving
    // those headers to a peer that has seen the checkpointed chain would cause that peer to
    // disconnect us. Requiring that our chainwork exceed the minimum chain work is a protection
    // against being fed a bogus chain when we started up for the first time and getting
    // partitioned off the honest network for serving that chain to others.
    if snap.tip.tip_work < cx.chain.min_chain_work()
        && !cx.peer.has_permission(Permissions::DOWNLOAD)
    {
        tracing::debug!(target: "net", "Ignoring {cmd} from peer={id} because active chain has too little work; sending empty response");
        // Just respond with an empty headers message, to tell the peer to go away but not treat
        // us as unresponsive. The type is the peer's bit 11, not the request's.
        let empty = headers_msg(uses_compressed_headers(cx.peer), Vec::new());
        cx.push(empty);
        return Ok(());
    }

    let start = if locator.is_empty() {
        // If locator is null, return the hashStop block.
        let Some(stop) = cx.chain.entry_by_hash(hash_stop) else {
            return Ok(());
        };
        if !block_request_allowed(cx, snap, &stop)? {
            tracing::debug!(target: "net", "ProcessMessage: ignoring request from peer={id} for old block header that isn't in the main chain");
            return Ok(());
        }
        Some(stop)
    } else {
        // Find the last block the caller has in the main chain.
        let fork = find_fork_in_global_index(cx.chain, snap, locator);
        (fork.height < snap.tip.tip_height).then(|| entry_at(cx.chain, snap, fork.height + 1))
    };

    let stop_text = if *hash_stop == BlockHash::ZERO {
        "end".to_owned()
    } else {
        hash_stop.to_string()
    };
    tracing::debug!(target: "net", "{cmd} {} to {stop_text} from peer={id}", start.map_or(-1, |e| i64::from(e.height)));

    // `GetHeadersLimit(pfrom, msg_type_internal == HEADERS2)`: the reply type is the request's.
    let limit = headers_limit(cx.peer.common_version(), compressed);
    let (headers, last_sent) = collect(cx.chain, snap, start, limit, hash_stop);
    // It is important that we simply reset the BestHeaderSent value here, and not
    // max(BestHeaderSent, newHeaderSent) (Core's comment): a re-announcement follows.
    cx.state.headers_mut().best_header_sent = Some(last_sent.unwrap_or(snap.tip.tip));
    cx.push(headers_msg(compressed, headers));
    Ok(())
}
