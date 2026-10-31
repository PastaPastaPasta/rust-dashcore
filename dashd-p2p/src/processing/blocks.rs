//! The two-phase block handlers (P3 §2.6): a handler runs under the peer's region (its state
//! lock and header cell) and returns a [`BlockStep`]. The message loop then hands the block to
//! V with both released, services the signal queue while it waits, drains once more and runs
//! the tail in a new region; or it drains and continues after a `ProcessHeaders` reply (I0,
//! D46). `block`, `cmpctblock` and `blocktxn` are the three (P3 §3.4, §3.5.3, §3.5.4).

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use dash_consensus::block::{Mutation, block_mutation};
use dash_consensus::decode_block;
use dash_consensus::{Block, BlockId, DecodeErrorKind, Tx, TxRef};
use dash_p2p::policy::Throw;
use dash_p2p::{CommandString, PeerHandle};
use dash_p2p_core::{
    BlockTxn, CmpctBlock, GetBlockTxn, GetData, InvType, Inventory, P2pDecodeError,
};
use dash_primitives::BlockHash;

use super::compact::CompactBlock;
use super::download::{
    CompactFill, MAX_BLOCKS_IN_TRANSIT_PER_PEER, MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK, PeerFacts,
    TrackedInit,
};
use super::eviction::can_direct_fetch;
use super::headers::Decoded;
use super::logic::{PeerLogic, out_msg};
use super::misbehavior;
use super::peer::assert_no_header_cell;
use super::registry::HandlerCx;
use super::seams::{
    BlockOutcome, BlockSubmit, BlockSync, ChainSnapshot, HeaderBatch, Interrupted, Validity,
};
use super::signals::DoorbellWaker;
use super::sync::{inv_hash, maybe_send_getheaders, update_block_availability};

/// What a handler leaves for the message loop (P3 §2.6).
#[derive(Debug)]
pub enum BlockStep {
    /// The message is done.
    Done,
    /// Hand `submit` to V, then run `tail` with the outcome.
    Process {
        /// The block for `ProcessNewBlock`.
        submit: BlockSubmit,
        /// What runs after V's reply.
        tail: Tail,
    },
    /// A `ProcessHeaders` reply was accepted inside the message: unlock, drain, relock, and
    /// continue on the drained snapshot (I0, D46).
    Redrain(AfterHeaders),
}

/// A handler's continuation after a `ProcessHeaders` reply (P3 §2.6).
#[derive(Debug)]
pub enum AfterHeaders {
    /// `ProcessHeadersMessage` after `ProcessNewBlockHeaders`: the "more" request,
    /// `UpdatePeerStateForReceivedHeaders` and `HeadersDirectFetchBlocks` (np:3370-3383).
    Headers {
        /// The reply's `last_accepted`.
        last: BlockId,
        /// No header of the batch's last hash was indexed before the call.
        received_new_header: bool,
        /// The batch size.
        n: usize,
    },
    /// `cmpctblock` after `ProcessNewBlockHeaders`: steps 6–13 and what follows them
    /// (np:5092-5293).
    Cmpct {
        /// The message.
        cmpct: Box<CompactBlock>,
        /// The reply's `pindex`.
        last: BlockId,
        /// The header was not indexed before the call.
        received_new_header: bool,
    },
}

/// What runs after V's `ProcessNewBlock` reply (P3 §2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tail {
    /// np:3717-3732 alone (`block`, `blocktxn`).
    ProcessBlock,
    /// np:3717-3732, then `RemoveBlockRequest(hash, nullopt)` once the entry is
    /// `IsValid(BLOCK_VALID_TRANSACTIONS)`: a `cmpctblock` reconstructed while the block is in
    /// flight from another peer (np:5284-5290).
    CompactReconstructed {
        /// The block's entry.
        id: BlockId,
    },
}

/// Core's one-entry `getdata(MSG_BLOCK, hash)`, pushed at once rather than batched (P3 §3.5.3).
fn push_getdata_block(cx: &HandlerCx<'_>, hash: &BlockHash) {
    let inventory = vec![Inventory {
        inv_type: InvType::Block,
        hash: inv_hash(hash),
    }];
    cx.push(out_msg(
        CommandString::GETDATA,
        &GetData {
            inventory,
        },
    ));
}

/// `RemoveBlockRequest(hash, pfrom)`, reading the clock as Core's own `GetTime()` does (D86).
fn remove_own_request<S: BlockSync + ?Sized>(cx: &HandlerCx<'_>, sync: &mut S, hash: &BlockHash) {
    sync.remove_block_request(hash, Some(cx.id()), cx.core.clock().node_now_micros());
}

/// `ProcessBlock(pfrom, pblock, /*force_processing=*/true)` for a reconstructed block, whose
/// `FillBlock` ran `IsBlockMutated` and so set `m_checked_merkle_root` (v:4362).
fn reconstructed(block: Block, hash: BlockHash, from: i64, tail: Tail) -> BlockStep {
    BlockStep::Process {
        submit: BlockSubmit {
            block: Arc::new(block),
            hash,
            force: true,
            from,
            merkle_checked: true,
        },
        tail,
    }
}

/// The decode as Core's `vRecv >> *pblock` throws.
fn read_block(payload: &[u8]) -> Result<Arc<dash_consensus::Block>, Throw> {
    let mut cursor = payload;
    match decode_block(&mut cursor) {
        Ok(b) => Ok(Arc::new(b.into_value())),
        Err(e) => Err(match e.kind {
            DecodeErrorKind::Codec(c) => Throw::from_decode(&P2pDecodeError::Consensus(c)),
            other => Throw::ios_failure(&format!("{other:?}")),
        }),
    }
}

impl PeerLogic {
    /// The `block` branch's phase 1 (P3 §3.4).
    // Core: net_processing.cpp:5348-5387
    pub(super) fn on_block(
        &mut self,
        cx: &mut HandlerCx<'_>,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        let id = cx.id();
        if cx.chain.loading_blocks() {
            tracing::debug!(target: "net", "Unexpected block message received from peer {id}");
            return Ok(BlockStep::Done);
        }
        let block = read_block(payload)?;
        let hash = block.hash();
        tracing::debug!(target: "net", "received block {hash} peer={id}");
        let sync = &mut self.seams.block_sync;
        if let Some(mutation) = block_mutation(&block) {
            // Core: validation.cpp:4434-4440 IsBlockMutated
            if let Mutation::MerkleRoot(state) = mutation {
                tracing::debug!(target: "validation", "Block mutated: {state}");
            }
            tracing::debug!(target: "net", "Received mutated block from peer={id}");
            cx.misbehaving(100, "mutated block");
            // `RemoveBlockRequest` reads `GetTime()` itself, after the decode (D86).
            remove_own_request(cx, &mut **sync, &hash);
            return Ok(BlockStep::Done);
        }
        let force = sync.is_block_requested(&hash);
        remove_own_request(cx, &mut **sync, &hash);
        sync.emplace_block_source(hash, id, true);
        Ok(BlockStep::Process {
            submit: BlockSubmit {
                block,
                hash,
                force,
                from: id,
                merkle_checked: true,
            },
            tail: Tail::ProcessBlock,
        })
    }

    /// The `cmpctblock` branch up to `ProcessNewBlockHeaders` (P3 §3.5.3 steps 1–5). An accepted
    /// header returns [`AfterHeaders::Cmpct`]: the rest runs in [`PeerLogic::after_cmpct`] on a
    /// snapshot drained after the reply (I0, D46). Under M2's `NoBlockSync` the message is
    /// ignored, as the registry row names.
    // Core: net_processing.cpp:5052-5090
    pub(super) fn on_cmpctblock(
        &mut self,
        cx: &mut HandlerCx<'_>,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        if self.seams.block_sync.download().is_none() {
            return Ok(BlockStep::Done);
        }
        let id = cx.id();
        if cx.chain.loading_blocks() {
            tracing::debug!(target: "net", "Unexpected cmpctblock message received from peer {id}");
            return Ok(BlockStep::Done);
        }
        let cmpct = CompactBlock::from_msg(cx.read::<CmpctBlock>(&mut &payload[..])?);
        let hash = cmpct.header().hash();
        if cx.chain.lookup(&cmpct.header().raw().prev_hash).is_none() {
            // Doesn't connect (or is genesis): request deeper headers instead of scoring.
            if !cx.chain.is_initial_block_download(&cx.snap, cx.core.clock().node_now()) {
                let locator = cx.chain.locator(Some(cx.snap.tip.best_header));
                maybe_send_getheaders(cx.core, cx.peer, cx.state, locator);
            }
            return Ok(BlockStep::Done);
        }
        let received_new_header = cx.chain.lookup(&hash).is_none();
        // `ProcessNewBlockHeaders` takes `cs_main` itself: release the header cell first, as the
        // `headers` path does (P3 §3.6).
        cx.state.leave_region();
        assert_no_header_cell();
        let Ok(r) = cx.chain.process_headers(HeaderBatch::new(vec![*cmpct.header()]), id) else {
            return Ok(BlockStep::Done);
        };
        if let Some(state) = &r.invalid {
            misbehavior::maybe_punish_node_for_block(
                cx.state,
                state.result(),
                cx.peer.is_inbound(),
                true,
                "invalid header via cmpctblock",
            );
            return Ok(BlockStep::Done);
        }
        let Some(last) = r.last_accepted else {
            debug_assert!(false, "ProcessHeaders returned no header and no invalid state");
            return Ok(BlockStep::Done);
        };
        Ok(BlockStep::Redrain(AfterHeaders::Cmpct {
            cmpct: Box::new(cmpct),
            last,
            received_new_header,
        }))
    }

    /// `cmpctblock` steps 6–13 on the post-reply drain's snapshot `cx.snap`, then the
    /// `ProcessCompactBlockTxns`, header-processing or reconstructed-block path they chose
    /// (P3 §3.5.3).
    // Core: net_processing.cpp:5092-5293
    pub(super) fn after_cmpct(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmpct: CompactBlock,
        last: BlockId,
        received_new_header: bool,
    ) -> BlockStep {
        let id = cx.id();
        let hash = cmpct.header().hash();
        if received_new_header {
            tracing::info!(target: "net", "Saw new cmpctblock header hash={hash} peer={id}");
        }
        let Some(dl) = self.seams.block_sync.download() else {
            return BlockStep::Done;
        };
        let snap: &ChainSnapshot = &cx.snap;
        let Some(entry) = cx.chain.entry(last) else {
            debug_assert!(false, "ProcessHeaders' last header is indexed");
            return BlockStep::Done;
        };
        update_block_availability(cx.chain, cx.state.headers_mut(), entry.hash);
        if received_new_header && entry.chain_work > snap.tip.tip_work {
            cx.state.last_block_announcement = cx.core.clock().node_now().0;
        }
        let Some(status) = cx.chain.status(snap, entry.id) else {
            debug_assert!(false, "an indexed entry has a published status");
            return BlockStep::Done;
        };
        if status.have_data {
            return BlockStep::Done;
        }
        let facts = dl.in_flight_facts(&entry.hash, id);
        if entry.chain_work <= snap.tip.tip_work || status.n_tx != 0 {
            // We know something better, or had this block and pruned it: a block we asked this
            // peer for comes by plain `getdata`, as the mempool will hardly help.
            if facts.from_this_peer {
                push_getdata_block(cx, &hash);
            }
            return BlockStep::Done;
        }
        // Not close to the tip yet: parallel download does the work.
        if facts.already == 0 && !can_direct_fetch(cx.core, snap) {
            return BlockStep::Done;
        }
        if entry.height > snap.tip.tip_height + 2 {
            if facts.from_this_peer {
                // Requested but far ahead: the mempool will hardly help.
                push_getdata_block(cx, &hash);
                return BlockStep::Done;
            }
            // An announcement: treat it as a `headers` message. A high-bandwidth peer may relay
            // a header before validating the block, so an invalid one is not scored.
            let decoded = Decoded {
                headers: vec![*cmpct.header().raw()],
                memo: vec![Some(*cmpct.header())],
            };
            return self.process_headers_message(cx, decoded, true);
        }
        let queued = dl.blocks_in_flight(id);
        if (facts.already < MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK
            && queued < MAX_BLOCKS_IN_TRANSIT_PER_PEER)
            || facts.from_this_peer
        {
            let now_us = cx.core.clock().node_now_micros();
            let peer = PeerFacts {
                id,
                inbound: cx.peer.is_inbound(),
                heights: &cx.state.heights_in_flight,
            };
            let missing = match dl.init_tracked(peer, &entry, &cmpct, now_us) {
                TrackedInit::AlreadySyncing => {
                    tracing::debug!(target: "net", "Peer sent us compact block we were already syncing!");
                    return BlockStep::Done;
                }
                TrackedInit::Invalid => {
                    // Reset the in-flight state in case the score does not disconnect.
                    remove_own_request(cx, dl, &entry.hash);
                    cx.misbehaving(100, "invalid compact block");
                    return BlockStep::Done;
                }
                TrackedInit::Failed => {
                    if facts.first {
                        // Duplicate short IDs: the block is in flight now, so ask for it whole.
                        push_getdata_block(cx, &hash);
                    } else {
                        // Give up on this peer and wait for the others.
                        remove_own_request(cx, dl, &entry.hash);
                    }
                    return BlockStep::Done;
                }
                TrackedInit::Missing(missing) => missing,
            };
            if missing.is_empty() {
                return self.process_compact_block_txns(cx, hash, Vec::new());
            }
            let hb_relay = cx.peer.bip152_highbandwidth_to()
                && (!cx.peer.is_inbound()
                    || dl.is_block_requested_from_outbound(&hash)
                    || facts.already < MAX_CMPCTBLOCKS_INFLIGHT_PER_BLOCK - 1);
            if facts.first || hb_relay {
                // The first in flight round-trips on failure; so does a high-bandwidth relay
                // peer that is outbound, behind an outbound attempt, or not in the last slot.
                let req = GetBlockTxn::new(entry.hash, missing).expect("ascending indexes");
                cx.push(out_msg(CommandString::GETBLOCKTXN, &req));
            } else {
                // Give up on this peer and wait for the others.
                remove_own_request(cx, dl, &entry.hash);
            }
            return BlockStep::Done;
        }
        // In flight from others, or this peer has too many blocks outstanding: try to
        // reconstruct anyway, since that may need no round trip.
        let Some(block) = dl.reconstruct_untracked(&cmpct) else {
            return BlockStep::Done;
        };
        dl.emplace_block_source(hash, id, false);
        reconstructed(
            block,
            hash,
            id,
            Tail::CompactReconstructed {
                id: entry.id,
            },
        )
    }

    /// The `blocktxn` branch (P3 §3.5.4).
    // Core: net_processing.cpp:5295-5307
    pub(super) fn on_blocktxn(
        &mut self,
        cx: &mut HandlerCx<'_>,
        payload: &[u8],
    ) -> Result<BlockStep, Throw> {
        if cx.chain.loading_blocks() {
            tracing::debug!(target: "net", "Unexpected blocktxn message received from peer {}", cx.id());
            return Ok(BlockStep::Done);
        }
        let resp: BlockTxn = cx.read(&mut &payload[..])?;
        let txn: Vec<TxRef> = resp.txs.into_iter().map(|tx| Arc::new(Tx::new(tx))).collect();
        Ok(self.process_compact_block_txns(cx, resp.block_hash, txn))
    }

    /// Core's `ProcessCompactBlockTxns` (P3 §3.5.4).
    // Core: net_processing.cpp:3734-3816
    fn process_compact_block_txns(
        &mut self,
        cx: &mut HandlerCx<'_>,
        hash: BlockHash,
        txn: Vec<TxRef>,
    ) -> BlockStep {
        let id = cx.id();
        let Some(dl) = self.seams.block_sync.download() else {
            tracing::debug!(target: "net", "Peer {id} sent us block transactions for block we weren't expecting");
            return BlockStep::Done;
        };
        let first = dl.in_flight_facts(&hash, id).first;
        match dl.fill_partial(id, &hash, &txn) {
            CompactFill::NotExpected => {
                tracing::debug!(target: "net", "Peer {id} sent us block transactions for block we weren't expecting");
                BlockStep::Done
            }
            CompactFill::HeaderGone => {
                // An earlier `FillBlock` wiped the header but left the partial: do not try again.
                remove_own_request(cx, dl, &hash);
                cx.misbehaving(100, "previous compact block reconstruction attempt failed");
                tracing::debug!(target: "net", "Peer {id} sent compact block transactions multiple times");
                BlockStep::Done
            }
            CompactFill::Invalid => {
                // Reset the in-flight state in case the score does not disconnect.
                remove_own_request(cx, dl, &hash);
                cx.misbehaving(100, "invalid compact block/non-matching block transactions");
                BlockStep::Done
            }
            CompactFill::Failed => {
                if first {
                    // Might have collided: fall back to `getdata` now. The failed partial stays,
                    // so another announcement from this peer for this block is refused, and the
                    // full download runs under the same `m_downloading_since`.
                    push_getdata_block(cx, &hash);
                } else {
                    remove_own_request(cx, dl, &hash);
                    tracing::debug!(target: "net", "Peer {id} sent us a compact block but it failed to reconstruct, waiting on first download to complete");
                }
                BlockStep::Done
            }
            CompactFill::Block(block) => {
                // The partial goes with the request.
                remove_own_request(cx, dl, &hash);
                // BIP 152 lets peers relay after checking only the header: a reconstructed block's
                // source is never punished for an invalid block.
                dl.emplace_block_source(hash, id, false);
                reconstructed(block, hash, id, Tail::ProcessBlock)
            }
        }
    }

    /// Phase 2: `ProcessNewBlock` through V, with every processor lock released. While the
    /// reply is pending, each doorbell ring drains the signal queue, so V's `BlockChecked`
    /// entries for this and other origins are applied in V's order (P3 §2.6, C1). No timeout,
    /// as Core's call has none; a dropped reply is [`Interrupted`].
    pub(super) fn wait_block(&mut self, submit: BlockSubmit) -> Result<BlockOutcome, Interrupted> {
        assert_no_header_cell();
        let mut wait = self.chain.process_block(submit)?;
        let bells = Arc::clone(&self.signals.bells);
        let waker = Waker::from(Arc::new(DoorbellWaker(Arc::clone(&bells))));
        let mut cx = Context::from_waker(&waker);
        loop {
            // The poll registers the waker before it reads the slot, so a reply after it rings.
            if let Poll::Ready(r) = wait.0.as_mut().poll(&mut cx) {
                return r;
            }
            bells.wait_doorbell();
            self.drain_sync_signals();
        }
    }

    /// Phase 3, in the region opened after the post-reply drain, on that drain's snapshot
    /// (P3 §3.7).
    // Core: net_processing.cpp:3717-3732 PeerManagerImpl::ProcessBlock
    pub(super) fn block_tail(
        &mut self,
        peer: &PeerHandle,
        hash: &BlockHash,
        outcome: BlockOutcome,
        tail: Tail,
        snap: &ChainSnapshot,
    ) {
        let clock = self.core.clock();
        if outcome.new_block {
            peer.set_last_block_time(clock.node_now().0);
            self.seams.block_sync.remove_block_request(hash, None, clock.node_now_micros());
        } else {
            self.seams.block_sync.erase_block_source(hash);
        }
        if let Tail::CompactReconstructed {
            id,
        } = tail
            && self.chain.status(snap, id).is_some_and(|s| s.is_valid(Validity::Transactions))
        {
            // Clear the download state of the block in flight from another peer; after
            // `ProcessNewBlock`, so a malleated `cmpctblock` cannot interfere with relay.
            // Core: net_processing.cpp:5284-5290
            self.seams.block_sync.remove_block_request(hash, None, clock.node_now_micros());
        }
    }
}
