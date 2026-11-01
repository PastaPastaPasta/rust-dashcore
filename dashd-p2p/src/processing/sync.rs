//! Headers sync and block announcements (P2a §3.4): the `headers`/`headers2` branch, the
//! `getheaders` policy, block availability, the `inv` block path, and `SendMessages` sections 6
//! (sync start), 7 (announcements), 8 (block invs) and 10 (the headers-sync timeout).

use std::sync::Arc;

use dash_consensus::{BlockId, Header};
use dash_p2p::policy::{ReadError, Throw};
use dash_p2p::rng::exponential_rand;
use dash_p2p::{
    CommandString, ConnectionType, NetCore, PeerHandle, Permissions, ServiceFlags, Why,
};
use dash_p2p_core::{GetHeaders, Headers, Headers2, InvType, Inventory, ProtoCtx};
use dash_primitives::{BlockHash, BlockHeader};
use dash_types::Numeric;

use super::blocks::{AfterHeaders, BlockStep};
use super::eviction::HEADERS_RESPONSE_TIME;
use super::headers::{
    self, Decoded, MAX_BLOCKS_TO_ANNOUNCE, MAX_UNCONNECTING_HEADERS, headers_limit,
};
use super::logic::{PeerLogic, can_serve_blocks, is_block_relay_only, out_msg};
use super::misbehavior;
use super::peer::{PeerHeaders, PeerState, RelayList, assert_no_header_cell};
use super::peerman::RecentBlock;
use super::registry::HandlerCx;
use super::seams::{
    ChainAccess, ChainSnapshot, EntryFacts, HeaderBatch, InvBatch, SyncCx, TreeRead, Work,
};

/// Core's `INBOUND_INVENTORY_BROADCAST_INTERVAL`, in seconds.
// Core: net_processing.cpp:187
const INBOUND_INVENTORY_BROADCAST_INTERVAL_S: i64 = 5;
/// Core's `OUTBOUND_INVENTORY_BROADCAST_INTERVAL`, in seconds.
// Core: net_processing.cpp:192
const OUTBOUND_INVENTORY_BROADCAST_INTERVAL_S: i64 = 2;

/// Core's `HEADERS_RESPONSE_TIME` in µs.
pub const HEADERS_RESPONSE_TIME_US: i64 = HEADERS_RESPONSE_TIME * 1_000_000;
/// Core's `HEADERS_DOWNLOAD_TIMEOUT_BASE`: 15 minutes, in µs.
pub const HEADERS_DOWNLOAD_TIMEOUT_BASE_US: i64 = 15 * 60 * 1_000_000;
/// Core's `HEADERS_DOWNLOAD_TIMEOUT_PER_HEADER`: 1 ms, in µs.
pub const HEADERS_DOWNLOAD_TIMEOUT_PER_HEADER_US: i64 = 1000;
/// Core's `MAX_OUTBOUND_PEERS_TO_PROTECT_FROM_DISCONNECT`.
pub const MAX_OUTBOUND_PEERS_TO_PROTECT_FROM_DISCONNECT: i32 = 4;

/// Core's `UsesCompressedHeaders(peer)`: the peer advertised bit 11 (DIP 25).
// Core: net_processing.cpp:1196-1199
pub fn uses_compressed_headers(peer: &PeerHandle) -> bool {
    peer.version_services().has(ServiceFlags::NODE_HEADERS_COMPRESSED)
}

/// The `getheaders` type for `peer`, by its bit 11 (never by the common version).
pub fn getheaders_type(peer: &PeerHandle) -> CommandString {
    if uses_compressed_headers(peer) {
        CommandString::GETHEADERS2
    } else {
        CommandString::GETHEADERS
    }
}

/// Core's `IsOutboundOrBlockRelayConn()`.
pub fn is_outbound_or_block_relay(peer: &PeerHandle) -> bool {
    matches!(peer.conn_type(), ConnectionType::OutboundFullRelay | ConnectionType::BlockRelay)
}

/// Core's `MaybeSendGetHeaders`: a `getheaders`/`getheaders2` with `hashStop = 0` unless one went
/// out within [`HEADERS_RESPONSE_TIME_US`]; the locator's version word is the common version.
// Core: net_processing.cpp:3179-3195 PeerManagerImpl::MaybeSendGetHeaders
pub fn maybe_send_getheaders(
    core: &NetCore,
    peer: &PeerHandle,
    st: &mut PeerState,
    locator: Vec<BlockHash>,
) -> bool {
    let now_us = core.clock().node_now_micros();
    if now_us - st.last_getheaders_us <= HEADERS_RESPONSE_TIME_US {
        return false;
    }
    let msg = GetHeaders {
        version: peer.common_version(),
        locator_hashes: locator,
        hash_stop: BlockHash::ZERO,
    };
    // `getheaders2` has `getheaders`' layout.
    core.push(peer, out_msg(getheaders_type(peer), &msg));
    st.last_getheaders_us = now_us;
    true
}

/// Core's `ProcessBlockAvailability`.
// Core: net_processing.cpp:1442-1456
pub fn process_block_availability(tree: &dyn TreeRead, h: &mut PeerHeaders) {
    let Some(hash) = h.last_unknown_block else {
        return;
    };
    if let Some(e) = tree.entry_by_hash(&hash)
        && e.chain_work > Work::ZERO
    {
        raise_best_known(tree, h, &e);
        h.last_unknown_block = None;
    }
}

/// Core's `UpdateBlockAvailability`.
// Core: net_processing.cpp:1458-1474
pub fn update_block_availability(tree: &dyn TreeRead, h: &mut PeerHeaders, hash: BlockHash) {
    process_block_availability(tree, h);
    match tree.entry_by_hash(&hash) {
        Some(e) if e.chain_work > Work::ZERO => raise_best_known(tree, h, &e),
        _ => h.last_unknown_block = Some(hash),
    }
}

/// `pindexBestKnownBlock = e` unless the best known block has more work.
fn raise_best_known(tree: &dyn TreeRead, h: &mut PeerHeaders, e: &EntryFacts) {
    if work_of(tree, h.best_known_block).is_none_or(|w| e.chain_work >= w) {
        h.best_known_block = Some(e.id);
    }
}

/// The chain work of `block`, if it is set and indexed.
pub(super) fn work_of(tree: &dyn TreeRead, block: Option<BlockId>) -> Option<Work> {
    block.and_then(|b| tree.entry(b)).map(|e| e.chain_work)
}

/// Core's `PeerHasHeader`.
// Core: net_processing.cpp:1433-1440
pub fn peer_has_header(tree: &dyn TreeRead, h: &PeerHeaders, e: &EntryFacts) -> bool {
    [h.best_known_block, h.best_header_sent]
        .into_iter()
        .flatten()
        .any(|b| tree.ancestor(b, e.height) == Some(e.id))
}

/// Core's `ActiveChain()[e.nHeight] == e`.
fn on_active_chain(chain: &dyn ChainAccess, snap: &ChainSnapshot, e: &EntryFacts) -> bool {
    chain.ancestor(snap.tip.tip, e.height) == Some(e.id)
}

/// Core's `CBlockLocator` hashes as an `Inventory` hash.
pub(super) fn inv_hash(h: &BlockHash) -> dash_num::Hash256 {
    dash_num::Hash256::from_lendian(*h.as_bytes())
}

/// An `Inventory` hash as a block hash.
pub fn block_hash(inv: &Inventory) -> BlockHash {
    BlockHash::from_lendian(*inv.hash.as_bytes())
}

impl PeerLogic {
    /// The `headers`/`headers2` branch (P2a §3.4.3 steps 1–4), then `ProcessHeadersMessage`.
    // Core: net_processing.cpp:5309-5346
    pub(super) fn on_headers(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        if cx.chain.loading_blocks() {
            tracing::debug!(target: "net", "Unexpected headers message received from peer {}", cx.id());
            return Ok(BlockStep::Done);
        }
        // Any `headers`/`headers2`, even empty or malformed, re-arms `MaybeSendGetHeaders` (PQ-2).
        cx.state.last_getheaders_us = 0;
        let mut data = payload;
        let count = headers::read_count(&mut data)?;
        // The size check is keyed by the message type received (PQ-4, PQ-24).
        let compressed = *cmd == CommandString::HEADERS2;
        if count > headers_limit(cx.peer.common_version(), compressed) as u64 {
            cx.misbehaving(20, &format!("headers message size = {count}"));
            return Ok(BlockStep::Done);
        }
        let decoded = if compressed {
            // SB6's reader reads the count again and refuses it over the same limit, so a
            // refusal cannot reach here; it would score as Core's handler does.
            match headers::read_headers2(&mut &payload[..], &cx.ctx, |h| Header::new(*h)) {
                Ok(decoded) => decoded,
                Err(ReadError::Throw(t)) => return Err(t),
                Err(refusal) => {
                    if let Some((score, msg)) = refusal.misbehaving() {
                        cx.misbehaving(score, &msg);
                    }
                    return Ok(BlockStep::Done);
                }
            }
        } else {
            headers::read_headers(&mut data, count)?
        };
        Ok(self.process_headers_message(cx, decoded, false))
    }

    /// Core's `ProcessHeadersMessage` with the hash-once path (P2a §3.4.3 steps 5–12, §3.4.9),
    /// up to `ProcessNewBlockHeaders`. An accepted reply ends the region: the rest runs in
    /// [`PeerLogic::after_headers`] on a snapshot drained after the reply (I0, D46).
    /// `via_compact` is `cmpctblock`'s fall-back to header processing (P3 A-10).
    // Core: net_processing.cpp:3322-3386 PeerManagerImpl::ProcessHeadersMessage
    pub(super) fn process_headers_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        decoded: Decoded,
        via_compact: bool,
    ) -> BlockStep {
        let n = decoded.headers.len();
        let Some(first) = decoded.headers.first() else {
            // Nothing interesting: stop asking this peer for more headers.
            return BlockStep::Done;
        };
        // A field compare: no hash.
        if cx.chain.lookup(&first.prev_hash).is_none() {
            if n <= MAX_BLOCKS_TO_ANNOUNCE {
                self.handle_few_unconnecting_headers(cx, decoded);
            } else {
                cx.misbehaving(10, "invalid header received");
            }
            return BlockStep::Done;
        }
        let Ok(hdrs) = headers::check_continuous(decoded, |h| Header::new(*h)) else {
            cx.misbehaving(20, "non-continuous headers sequence");
            return BlockStep::Done;
        };
        let last_hash = hdrs[n - 1].hash();
        let received_new_header = cx.chain.lookup(&last_hash).is_none();
        // `ProcessNewBlockHeaders` takes `cs_main` itself (np:3362): the header cell is released
        // before the send, so V's visit never waits behind a holder that waits on V (P3 §3.6).
        cx.state.leave_region();
        assert_no_header_cell();
        let Ok(r) = cx.chain.process_headers(HeaderBatch::new(hdrs), cx.id()) else {
            tracing::debug!(target: "net", "headers from peer={} not processed: the chain is shutting down", cx.id());
            return BlockStep::Done;
        };
        if let Some(state) = &r.invalid {
            misbehavior::maybe_punish_node_for_block(
                cx.state,
                state.result(),
                cx.peer.is_inbound(),
                via_compact,
                "invalid header received",
            );
            return BlockStep::Done;
        }
        // Core dereferences `pindexLast` here; a failure that is not an invalid state leaves it
        // null only when nothing was accepted or known, which CH1 never reports (D23).
        let Some(last) = r.last_accepted else {
            debug_assert!(false, "ProcessHeaders returned no last header and no invalid state");
            return BlockStep::Done;
        };
        // The reply contributes only its own facts; the rest reads a drained snapshot (D46).
        BlockStep::Redrain(AfterHeaders::Headers {
            last,
            received_new_header,
            n,
        })
    }

    /// The continuation after a `ProcessHeaders` reply, in the region opened after the
    /// post-reply drain, on that drain's snapshot (P3 §2.6, I0, D46).
    pub(super) fn after_headers(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        after: AfterHeaders,
        snap: &Arc<ChainSnapshot>,
    ) -> BlockStep {
        let chain = Arc::clone(&self.chain);
        let core = Arc::clone(&self.core);
        let signals = Arc::clone(&self.signals);
        let mut cx = HandlerCx {
            core: &core,
            peer,
            state: st,
            chain: &*chain,
            snap: Arc::clone(snap),
            ctx: ProtoCtx::new(peer.common_version(), snap.bls),
            now_us: core.clock().node_now_micros(),
            signals: &signals,
            requests: Arc::clone(&self.object_request),
            post: Vec::new(),
        };
        let cx = &mut cx;
        let (last, received_new_header, n) = match after {
            AfterHeaders::Headers {
                last,
                received_new_header,
                n,
            } => (last, received_new_header, n),
            AfterHeaders::Cmpct {
                cmpct,
                last,
                received_new_header,
            } => return self.after_cmpct(cx, *cmpct, last, received_new_header),
        };
        // The "more" request and `may_have_more_headers` are keyed by the peer's bit 11 (PQ-24).
        let limit = headers_limit(cx.peer.common_version(), uses_compressed_headers(cx.peer));
        if n == limit {
            let locator = cx.chain.locator(Some(last));
            if maybe_send_getheaders(cx.core, cx.peer, cx.state, locator) {
                let height = cx.chain.entry(last).map_or(0, |e| e.height);
                tracing::debug!(target: "net", "more {} ({height}) to end to peer={} (startheight:{})", getheaders_type(cx.peer), cx.id(), cx.state.net.starting_height);
            }
        }
        self.update_peer_state_for_received_headers(
            cx,
            last,
            received_new_header,
            n == limit,
            snap,
        );
        let mut sync = SyncCx::new(cx.core, cx.peer, cx.state, cx.chain, snap);
        self.seams.block_sync.headers_direct_fetch(&mut sync, last);
        BlockStep::Done
    }

    /// Core's `HandleFewUnconnectingHeaders`: hashes `headers.back()`, and `headers[0]` only for
    /// the log line (P2a §3.4.9 rule 1).
    // Core: net_processing.cpp:3138-3165 PeerManagerImpl::HandleFewUnconnectingHeaders
    fn handle_few_unconnecting_headers(&mut self, cx: &mut HandlerCx<'_>, decoded: Decoded) {
        let Decoded {
            headers,
            memo,
        } = decoded;
        let hash_at =
            |i: usize| memo[i].map_or_else(|| Header::new(headers[i]).hash(), |h| h.hash());
        cx.state.unconnecting_headers += 1;
        let tip = &cx.snap.tip;
        let locator = cx.chain.locator(Some(tip.best_header));
        if maybe_send_getheaders(cx.core, cx.peer, cx.state, locator)
            && tracing::enabled!(target: "net", tracing::Level::DEBUG)
        {
            tracing::debug!(target: "net", "received header {}: missing prev block {}, sending {} ({}) to end (peer={}, nUnconnectingHeaders={})", hash_at(0), headers[0].prev_hash, getheaders_type(cx.peer), tip.best_header_height, cx.id(), cx.state.unconnecting_headers);
        }
        update_block_availability(cx.chain, cx.state.headers_mut(), hash_at(headers.len() - 1));
        if cx.state.unconnecting_headers % MAX_UNCONNECTING_HEADERS == 0 {
            let msg = format!("{} non-connecting headers", cx.state.unconnecting_headers);
            cx.misbehaving(20, &msg);
        }
    }

    /// Core's `UpdatePeerStateForReceivedHeaders`; `snap` is the one CH1 published before its
    /// reply (R3).
    // Core: net_processing.cpp:3267-3320 PeerManagerImpl::UpdatePeerStateForReceivedHeaders
    fn update_peer_state_for_received_headers(
        &mut self,
        cx: &mut HandlerCx<'_>,
        last: BlockId,
        received_new_header: bool,
        may_have_more_headers: bool,
        snap: &ChainSnapshot,
    ) {
        let (chain, st, peer, id) = (cx.chain, &mut *cx.state, cx.peer, cx.peer.id());
        if st.unconnecting_headers > 0 {
            tracing::debug!(target: "net", "peer={id}: resetting nUnconnectingHeaders ({} -> 0)", st.unconnecting_headers);
        }
        st.unconnecting_headers = 0;
        let Some(last) = chain.entry(last) else {
            return;
        };
        update_block_availability(chain, st.headers_mut(), last.hash);
        let now = cx.core.clock().node_now();
        if received_new_header && last.chain_work > snap.tip.tip_work {
            st.last_block_announcement = now.0;
        }
        let best_work = work_of(chain, st.headers().best_known_block);
        if chain.is_initial_block_download(snap, now)
            && !may_have_more_headers
            && best_work.is_some_and(|w| w < chain.min_chain_work())
            && is_outbound_or_block_relay(peer)
        {
            tracing::info!(target: "net", "Disconnecting outbound peer {id} -- headers chain has insufficient work");
            cx.core.disconnect(id, Why::Processor("headers chain has insufficient work"));
        }
        if !peer.disconnect_requested()
            && peer.conn_type() == ConnectionType::OutboundFullRelay
            && self.counters.outbound_with_protect < MAX_OUTBOUND_PEERS_TO_PROTECT_FROM_DISCONNECT
            && best_work.is_some_and(|w| w >= snap.tip.tip_work)
            && !st.chain_sync.protect
        {
            tracing::debug!(target: "net", "Protecting outbound peer={id} from eviction");
            st.chain_sync.protect = true;
            self.counters.outbound_with_protect += 1;
        }
    }

    /// A block entry of an `inv`: updates the peer's availability and returns the hash when it
    /// is a candidate for the `getheaders` trigger after the loop (P2a §3.4.7).
    // Core: net_processing.cpp:4531-4545
    pub(super) fn inv_block_entry(
        &self,
        cx: &mut HandlerCx<'_>,
        inv: &Inventory,
    ) -> Option<BlockHash> {
        let hash = block_hash(inv);
        let have = cx.chain.lookup(&hash).is_some();
        tracing::debug!(target: "net", "got inv: {inv}  {} peer={}", if have { "have" } else { "new" }, cx.id());
        update_block_availability(cx.chain, cx.state.headers_mut(), hash);
        (!have && !cx.chain.loading_blocks() && !self.seams.block_sync.is_block_requested(&hash))
            .then_some(hash)
    }

    /// After the `inv` loop: the block-inv `getheaders` trigger, one new peer per new block
    /// before sync starts.
    // Core: net_processing.cpp:4582-4608
    pub(super) fn inv_trigger_getheaders(&mut self, cx: &mut HandlerCx<'_>, best_block: BlockHash) {
        let st = &mut *cx.state;
        if !(st.sync_started
            || (!st.inv_triggered_getheaders_before_sync
                && best_block != self.counters.last_block_inv_triggering_sync))
        {
            return;
        }
        let tip = &cx.snap.tip;
        let locator = cx.chain.locator(Some(tip.best_header));
        if maybe_send_getheaders(cx.core, cx.peer, st, locator) {
            tracing::debug!(target: "net", "{} ({}) {best_block} to peer={}", getheaders_type(cx.peer), tip.best_header_height, cx.peer.id());
        }
        if !st.sync_started {
            st.inv_triggered_getheaders_before_sync = true;
            self.counters.last_block_inv_triggering_sync = best_block;
        }
    }

    /// Section 6: start headers sync from this peer. Returns Core's
    /// `sync_blocks_and_headers_from_peer` for section 12.
    // Core: net_processing.cpp:6342-6396
    pub(super) fn start_sync_section(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        snap: &ChainSnapshot,
    ) -> bool {
        let core = &*self.core;
        let chain = &*self.chain;
        let tip = &snap.tip;
        let serves = can_serve_blocks(peer.version_services());
        let sync_from_peer = st.preferred_download
            || (serves
                && peer.conn_type() != ConnectionType::AddrFetch
                && (self.counters.num_preferred_download == 0
                    || !self.seams.block_sync.any_in_flight()));
        if st.sync_started || !serves || chain.loading_blocks() || !peer.can_relay() {
            return sync_from_peer;
        }
        let now_us = core.clock().node_now_micros();
        let best_time_us = i64::from(tip.best_header_time) * 1_000_000;
        if !((self.counters.sync_started == 0 && sync_from_peer)
            || best_time_us > now_us - self.opts.max_tip_age * 1_000_000)
        {
            return sync_from_peer;
        }
        // Start at the best header's parent, so an up-to-date peer answers with at least one.
        let start = chain.entry(tip.best_header).and_then(|e| e.prev).unwrap_or(tip.best_header);
        if maybe_send_getheaders(core, peer, st, chain.locator(Some(start))) {
            let height = chain.entry(start).map_or(0, |e| e.height);
            tracing::debug!(target: "net", "initial {} ({height}) to peer={} (startheight:{})", getheaders_type(peer), peer.id(), st.net.starting_height);
            st.sync_started = true;
            // Multiply before the integer division (core n-2).
            let behind_secs = (now_us - best_time_us) / 1_000_000;
            let spacing = core.config().params.consensus.pow_target_spacing;
            st.headers_sync_timeout_us = now_us
                + HEADERS_DOWNLOAD_TIMEOUT_BASE_US
                + HEADERS_DOWNLOAD_TIMEOUT_PER_HEADER_US * behind_secs / spacing;
            self.counters.sync_started += 1;
        }
        sync_from_peer
    }

    /// Section 7: announce new blocks by `headers`/`headers2`/`cmpctblock`, or queue an `inv`
    /// of the tip (P2a §3.4.8).
    // Core: net_processing.cpp:6398-6551
    /// `queue` is the list the head of `send_messages` moved out of the `block_inv` cell with
    /// `snap` (P3 §3.6, DEC-236, D44); `recent` is the head's copy of `m_most_recent_*`, which
    /// the cmpctblock branch reads.
    pub(super) fn announce_section(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        snap: &ChainSnapshot,
        queue: &RelayList,
        recent: &RecentBlock,
    ) {
        if !peer.can_relay() {
            return;
        }
        let chain = &*self.chain;
        let core = &*self.core;
        let id = peer.id();
        let len = queue.len_capped();
        let queue = queue.as_slice();
        let mut revert = (!st.prefer_headers
            && !st.prefer_headers_compressed
            && (!st.headers().requested_hb_cmpctblocks || len > 1))
            || len > MAX_BLOCKS_TO_ANNOUNCE;
        let mut best: Option<BlockId> = None;
        process_block_availability(chain, st.headers_mut());
        let mut to_send: Vec<EntryFacts> = Vec::new();
        if !revert {
            let devnet_genesis = core.config().params.consensus.hash_devnet_genesis_block;
            let mut found_start = false;
            for hash in queue {
                let Some(e) = chain.entry_by_hash(hash) else {
                    // Core asserts the block is indexed.
                    debug_assert!(false, "announced block {hash} is not in the index");
                    revert = true;
                    break;
                };
                if !on_active_chain(chain, snap, &e) {
                    revert = true;
                    break;
                }
                if best.is_some_and(|b| e.prev != Some(b)) {
                    revert = true;
                    break;
                }
                best = Some(e.id);
                let prev = e.prev.and_then(|p| chain.entry(p));
                let prev_is_devnet_genesis = !devnet_genesis.is_null()
                    && prev.is_some_and(|p| p.hash.as_bytes() == devnet_genesis.as_bytes());
                if found_start {
                    to_send.push(e);
                } else if peer_has_header(chain, st.headers(), &e) {
                    continue;
                } else if prev.is_none_or(|p| peer_has_header(chain, st.headers(), &p))
                    || prev_is_devnet_genesis
                {
                    found_start = true;
                    to_send.push(e);
                } else {
                    revert = true;
                    break;
                }
            }
        }
        let mut compact = None;
        if !revert && let (Some(best), Some(front)) = (best, to_send.first()) {
            let raws: Vec<BlockHeader> = to_send.iter().map(|e| e.header).collect();
            if to_send.len() == 1 && st.headers().requested_hb_cmpctblocks {
                tracing::debug!(target: "net", "SendMessages sending header-and-ids {} to peer={id}", front.hash);
                compact = Some(*front);
                st.headers_mut().best_header_sent = Some(best);
            } else if st.prefer_headers_compressed {
                core.push(peer, out_msg(CommandString::HEADERS2, &Headers2::announce(&raws)));
                st.headers_mut().best_header_sent = Some(best);
            } else if st.prefer_headers {
                match to_send.as_slice() {
                    [one] => {
                        tracing::debug!(target: "net", "SendMessages: sending header {} to peer={id}", one.hash)
                    }
                    [first, .., last] => {
                        tracing::debug!(target: "net", "SendMessages: {} headers, range ({}, {}), to peer={id}", to_send.len(), first.hash, last.hash)
                    }
                    [] => {}
                }
                core.push(
                    peer,
                    out_msg(
                        CommandString::HEADERS,
                        &Headers {
                            headers: raws,
                        },
                    ),
                );
                st.headers_mut().best_header_sent = Some(best);
            } else {
                revert = true;
            }
        }
        if revert && let Some(&hash) = queue.last() {
            if let Some(e) = chain.entry_by_hash(&hash) {
                if !on_active_chain(chain, snap, &e) {
                    tracing::debug!(target: "net", "Announcing block {hash} not on main chain (tip={})", snap.tip.tip_hash);
                }
                if !peer_has_header(chain, st.headers(), &e) {
                    st.block_inv.lock().inv_relay.push(hash);
                    tracing::debug!(target: "net", "SendMessages: sending inv peer={id} hash={hash}");
                }
            } else {
                debug_assert!(false, "announced block {hash} is not in the index");
            }
        }
        if let Some(block) = compact {
            let mut cx = SyncCx::new(core, peer, st, chain, snap);
            self.seams.block_sync.push_compact_announcement(&mut cx, &block, recent);
        }
    }

    /// Section 8: the queued block invs, then P5's trickle, in one batch.
    // Core: net_processing.cpp:6555-6750
    pub(super) fn inv_section(&mut self, peer: &PeerHandle, st: &mut PeerState, now_us: i64) {
        let mut batch = InvBatch::new(&self.core, peer);
        for hash in std::mem::take(&mut st.block_inv.lock().inv_relay) {
            batch.push_block(Inventory {
                inv_type: InvType::Block,
                hash: inv_hash(&hash),
            });
        }
        if st.tx_relay {
            let inbounds = &mut self.counters.next_inv_to_inbounds_us;
            trickle_timer(&self.core, inbounds, peer, st, now_us);
        }
        self.seams.objects.inv_section(&mut batch, now_us);
        // Core: net_processing.cpp:6719-6747
        let block_relay_only = is_block_relay_only(peer, self.opts.blocks_only);
        st.inv_relay.lock().send_other(&mut batch, st.relay_txs, block_relay_only);
        batch.finish();
    }

    /// Section 10: the headers-sync timeout. `true` when it disconnected the peer (Core returns
    /// from `SendMessages`).
    // Core: net_processing.cpp:6782-6813
    pub(super) fn headers_timeout_section(
        &mut self,
        peer: &PeerHandle,
        st: &mut PeerState,
        snap: &ChainSnapshot,
    ) -> bool {
        if !st.sync_started || st.headers_sync_timeout_us == i64::MAX {
            return false;
        }
        let core = &*self.core;
        let now_us = core.clock().node_now_micros();
        let best_time_us = i64::from(snap.tip.best_header_time) * 1_000_000;
        if best_time_us > now_us - self.opts.max_tip_age * 1_000_000 {
            // Caught up once: never time out again.
            st.headers_sync_timeout_us = i64::MAX;
            return false;
        }
        let others = self.counters.num_preferred_download - i32::from(st.preferred_download);
        if now_us <= st.headers_sync_timeout_us || self.counters.sync_started != 1 || others < 1 {
            return false;
        }
        let id = peer.id();
        if !peer.has_permission(Permissions::NO_BAN) {
            tracing::info!(target: "net", "Timeout downloading headers from peer={id}, disconnecting");
            core.disconnect(id, Why::Processor("headers download timeout"));
            return true;
        }
        tracing::info!(target: "net", "Timeout downloading headers from noban peer={id}, not disconnecting");
        st.sync_started = false;
        self.counters.sync_started -= 1;
        st.headers_sync_timeout_us = 0;
        false
    }
}

/// Section 8's transaction trickle timer: once `m_next_inv_send_time` has passed, Core sets
/// `fSendTrickle` and draws the next time. Inbound peers share `m_next_inv_to_inbounds`;
/// an outbound peer with a verified masternode waits half as long. Transaction relay, which
/// owns the queue `fSendTrickle` releases, is a later task; the timer keeps Core's draws
/// (D61).
// Core: net_processing.cpp:6592-6609; :1259-1269 NextInvToInbounds
fn trickle_timer(
    core: &NetCore,
    next_inv_to_inbounds_us: &mut i64,
    peer: &PeerHandle,
    st: &mut PeerState,
    now_us: i64,
) {
    if st.next_inv_send_us >= now_us {
        return;
    }
    let draw = || core.get_rand(1 << 48);
    st.next_inv_send_us = if peer.is_inbound() {
        if *next_inv_to_inbounds_us < now_us {
            *next_inv_to_inbounds_us =
                exponential_rand(now_us, INBOUND_INVENTORY_BROADCAST_INTERVAL_S, draw());
        }
        *next_inv_to_inbounds_us
    } else {
        let average_s = if peer.verified_proreg_hash() == [0; 32] {
            OUTBOUND_INVENTORY_BROADCAST_INTERVAL_S
        } else {
            OUTBOUND_INVENTORY_BROADCAST_INTERVAL_S / 2
        };
        exponential_rand(now_us, average_s, draw())
    };
}
