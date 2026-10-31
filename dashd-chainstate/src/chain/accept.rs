//! Headers and blocks on V (§4.9): `ProcessNewBlockHeaders`, `ProcessNewBlock`'s accept half,
//! `AcceptBlock` and `NotifyHeaderTip`. `ReceivedBlockTransactions` is [`NodeBlockIndex`]'s.
//!
//! [`NodeBlockIndex`]: super::index::NodeBlockIndex

use std::sync::Arc;

use dash_chain::{
    AcceptOne, ChainError, ChainReader, EntryView, FlatFilePos, HeaderBatch, HeadersResult, PeerId,
    Status,
};
use dash_consensus::{Block, BlockId, BlockValidationResult};
use dash_primitives::BlockHash;

use super::abc::PBlock;
use super::actor::V;
use super::command::{BlockReply, BlockResult, BlockState};
use super::flush::FlushStateMode;
use super::index::entry;
use super::load::{LoadFailure, devnet_genesis_block};
use super::reply::ReplySender;
use super::seams::Fatal;
use super::signals::{NewBlock, TreeRead};

/// Core `MIN_BLOCKS_TO_KEEP` (`validation.h`).
pub const MIN_BLOCKS_TO_KEEP: u32 = 288;

/// V's reader inside an accept scope, lent to a `NewPoWValidBlock` listener for the call (C8): it
/// reads the arena V writes, so the header this accept inserted is found.
struct InScope<'a>(&'a ChainReader);

impl TreeRead for InScope<'_> {
    fn lookup(&self, hash: &BlockHash) -> Option<BlockId> {
        self.0.lookup(hash)
    }

    fn entry(&self, id: BlockId) -> Option<EntryView> {
        self.0.entry(id)
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.0.ancestor(id, height)
    }
}

/// An accepted block's result and the reply its ABC tail sends.
pub(crate) type AcceptedBlock = (BlockResult, Option<ReplySender<BlockResult>>);

impl V {
    fn entry(&self, id: BlockId) -> EntryView {
        entry(&self.reader, id)
    }

    /// `AcceptBlockHeader`'s tail for a new header (`validation.cpp:4712-4730`): the line that
    /// helps spot selfish mining, at debug level in IBD and by default after, then
    /// `AcceptedBlockHeader`.
    fn saw_new_header(&self, id: BlockId) {
        let e = self.entry(id);
        if self.is_ibd() {
            tracing::debug!("Saw new header hash={} height={}", e.hash, e.height);
        } else {
            tracing::info!("Saw new header hash={} height={}", e.hash, e.height);
        }
        self.signals.accepted_block_header(id);
        // cl:156-164: the best lock names this header at another height; nothing moves.
        if let Some(best) = &self.cl.sigs.best
            && best.block_hash == e.hash
            && i64::from(best.height) != i64::from(e.height)
        {
            tracing::debug!(
                "Chainlocks::AcceptedBlockHeader -- block header {} came in late, updating and \
                 enforcing",
                e.hash
            );
            tracing::info!(
                "Chainlocks::AcceptedBlockHeader -- height of CLSIG ({best}) does not match the \
                 specified block's height ({})",
                e.height
            );
        }
    }

    /// Core `ProcessNewBlockHeaders` (`validation.cpp:4735-4762`) over SB4's `accept`.
    pub(crate) fn process_headers(
        &mut self,
        batch: HeaderBatch,
        from: Option<PeerId>,
        reply: ReplySender<HeadersResult>,
    ) -> Result<(), Fatal> {
        // SB4 runs AcceptBlockHeader per header in one scope and publishes before returning
        // (R3); an `Err` is a storage or internal failure, fatal.
        let res = self.chain.accept(batch, from)?;
        // The ChainLock handler's AcceptedBlockHeader, applied per header by SB4 (§5.3).
        self.apply_cl_late_header(res.floor_moved);
        // :4712-4730 once per inserted header, in order (D11).
        for &id in &res.inserted {
            self.saw_new_header(id);
        }
        // D5: once per batch (Core: after each header, :4743).
        self.check_block_index()?;
        self.scope_end();
        // A failed batch returns at :4745-4747, before NotifyHeaderTip (R1-m3).
        if res.stop.is_none()
            && self.notify_header_tip()
            && self.is_ibd()
            && let Some(last) = res.last_accepted
        {
            let e = self.entry(last);
            let spacing = self.params.consensus.pow_target_spacing.max(1);
            let blocks_left = (self.now_secs() - i64::from(e.header.time)) / spacing;
            let h = f64::from(e.height);
            let progress = 100.0 * h / (h + blocks_left as f64);
            tracing::info!("Synchronizing blockheaders, height: {} (~{progress:.2}%)", e.height);
        }
        reply.send(res);
        Ok(())
    }

    /// Core `NotifyHeaderTip` (`validation.cpp:3699-3720`): when the best header moved since the
    /// last notification, the UI notification and the synchronous signal. Whether it fired.
    pub(crate) fn notify_header_tip(&mut self) -> bool {
        let best = self.chain.best_header();
        if !self.idx.take_header_notification(best) {
            return false;
        }
        let ibd = self.is_ibd();
        let e = self.entry(best);
        self.ui.notify_header_tip(self.loading.sync_state(ibd), e.height, e.header.time);
        self.signals.notify_header_tip(best, ibd);
        true
    }

    /// `ProcessNewBlock`'s `cs_main` scope (`validation.cpp:4884-4902`): `CheckBlock`,
    /// `AcceptBlock`, then `NotifyHeaderTip`. `None` when the block failed: Core returns before
    /// its ABC, and the reply is sent. Otherwise the result and the reply that waits for the ABC
    /// tail (an `Accepted` reply is sent here).
    pub(crate) fn process_block_accept(
        &mut self,
        block: &Arc<Block>,
        hash: BlockHash,
        force: bool,
        merkle_checked: bool,
        reply: Option<BlockReply>,
    ) -> Result<Option<AcceptedBlock>, Fatal> {
        debug_assert_eq!(hash, block.hash(), "ProcessBlock's hash is the block's X11");
        // :4891; the verdict is cached for AcceptBlock's own CheckBlock, as Core's `fChecked`.
        // A caller's `m_checked_merkle_root` skips the merkle pass (`v:4332`, A-4b); Core's
        // `fChecked` is still set (`v:4430`), as `fCheckMerkleRoot` stays true.
        let result = match self.checks.check_block(block, true, !merkle_checked) {
            Ok(()) => self.accept_block(block, hash, force, None, true)?,
            Err(e) => BlockResult::rejected(BlockState::Invalid(e)),
        };
        if !result.accepted {
            // :4896-4900
            let ibd = self.is_ibd();
            self.signals.block_checked(block, &result.state, ibd);
            tracing::error!("ProcessNewBlock: AcceptBlock FAILED ({})", result.state);
            // R1-M4: AcceptBlockHeader may have moved the best header or the floor.
            self.scope_end();
            if let Some(BlockReply::Activated(r) | BlockReply::Accepted(r)) = reply {
                r.send(result);
            }
            return Ok(None);
        }
        self.scope_end();
        self.notify_header_tip();
        let reply = match reply {
            Some(BlockReply::Activated(r)) => Some(r),
            Some(BlockReply::Accepted(r)) => {
                r.send(result.clone());
                None
            }
            None => None,
        };
        Ok(Some((result, reply)))
    }

    /// `ProcessNewBlock`'s class C tail (`validation.cpp:4904-4912`): the block's
    /// `ActivateBestChain`, then the reply.
    pub(crate) fn process_block_tail(
        &mut self,
        block: &Arc<Block>,
        result: BlockResult,
        reply: Option<ReplySender<BlockResult>>,
    ) -> Result<(), Fatal> {
        // :4905-4909. Core's `false` follows an AbortNode: V stops and the reply disconnects. The
        // block passed `CheckBlock(true, true)` at :4891, so its `fChecked` is set (CH3 D42).
        let pblock = PBlock {
            block,
            checked: true,
        };
        // After the AbortNode's report, as Core's caller logs (CH3b D212, D213).
        self.composite(|v| v.activate_best_chain(Some(pblock))).inspect_err(|f| {
            if f.is_abort_node() {
                tracing::error!("ProcessNewBlock: ActivateBestChain failed ({f})");
            }
        })?;
        tracing::info!("ProcessNewBlock : ACCEPTED");
        if let Some(r) = reply {
            r.send(result);
        }
        Ok(())
    }

    // Core: validation.cpp:5415-5423 (LoadGenesisBlock's devnet step)
    /// The devnet genesis block on the index `load_genesis_block` made, before anything else V
    /// does (D18): `CheckBlock`, which Core asserts, then `AcceptBlock` as a requested block.
    /// Init's ABC connects it. A rejection is `LoadGenesisBlock`'s `false`, which init reports
    /// ([`LoadFailure::Failure`], D48); `AcceptBlock` has logged why. `Err` is `AbortNode`, as in
    /// `AcceptBlock`, which init then reports too.
    pub(crate) fn load_devnet_genesis(&mut self) -> Result<Result<(), LoadFailure>, Fatal> {
        if !self.idx.take_devnet_pending() {
            return Ok(Ok(()));
        }
        let block = devnet_genesis_block(&self.params).expect("a devnet's index");
        if let Err(e) = self.checks.check_block(&block, true, true) {
            panic!("devnet genesis block is invalid: {e}");
        }
        let result = self.accept_block(&block, block.hash(), true, None, true)?;
        // An `AbortNode` V carried past (the block's write or its flush): init's error follows.
        if let Some(f) = self.interrupted.take() {
            return Err(f);
        }
        self.scope_end();
        Ok(if result.accepted {
            Ok(())
        } else {
            Err(LoadFailure::Failure {
                cause: None,
            })
        })
    }

    /// Core `AcceptBlock` (`validation.cpp:4765-4871`). `checked`: this block already passed
    /// `CheckBlock` (Core's `fChecked`). `Err` is `AbortNode` or one of Core's asserts.
    fn accept_block(
        &mut self,
        block: &Arc<Block>,
        hash: BlockHash,
        requested: bool,
        dbp: Option<FlatFilePos>,
        checked: bool,
    ) -> Result<BlockResult, Fatal> {
        // :4783 AcceptBlockHeader; SB4 does not publish (scope_end does).
        let accepted = self.chain.accept_header(block.header())?;
        self.apply_cl_late_header(accepted.floor_moved);
        let id = match accepted.one {
            AcceptOne::Accepted(id) => {
                self.saw_new_header(id);
                Ok(id)
            }
            AcceptOne::Known(id) => Ok(id),
            AcceptOne::Invalid(reject) => Err(reject),
            AcceptOne::BelowAnchor | AcceptOne::ContextSkipped => {
                return Err(ChainError::Internal("an SPV verdict on the node").into());
            }
        };
        // :4784
        self.check_block_index()?;
        let id = match id {
            Ok(id) => id,
            Err(reject) => return Ok(BlockResult::rejected(BlockState::Invalid(reject))),
        };
        let e = self.entry(id);
        debug_assert_eq!(e.hash, hash);
        // A known block "accepted" without a write: Core's early `return true`s.
        let skipped = BlockResult {
            new_block: false,
            state: BlockState::Valid,
            accepted: true,
        };

        let tip = self.tip();
        // :4794, :4800 (a null tip: more-or-same work, ActiveHeight() = -1)
        let more_or_same = tip.is_none_or(|t| e.chain_work >= self.entry(t).chain_work);
        let active_height = tip.map_or(-1, |t| i64::from(self.entry(t).height));
        let too_far = i64::from(e.height) > active_height + i64::from(MIN_BLOCKS_TO_KEEP);
        // :4810-4821
        if e.status.has(Status::HAVE_DATA) {
            return Ok(skipped);
        }
        if !requested
            && (e.n_tx != 0
                || !more_or_same
                || too_far
                || e.chain_work < self.reader.min_chain_work())
        {
            return Ok(skipped);
        }
        // :4825-4832
        let now = self.now_secs();
        let checks = if checked {
            Ok(())
        } else {
            self.checks.check_block(block, true, true)
        }
        .and_then(|()| self.checks.contextual_check_block(block, e.prev, &self.chain, now));
        if let Err(err) = checks {
            if err.result() != BlockValidationResult::Mutated {
                let mut ix = self.chain.node_index()?;
                ix.modify(id, |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
                ix.mark_dirty(id);
            }
            tracing::error!("AcceptBlock: {err}");
            return Ok(BlockResult::rejected(BlockState::Invalid(err)));
        }
        // :4836-4838, inline (C8). Core's guard first: `IsInitialBlockDownload()` latches here.
        if !self.is_ibd() && tip == e.prev && self.signals.has_listeners() {
            self.signals.new_pow_valid_block(&NewBlock {
                id,
                entry: self.entry(id),
                block,
                stamp: self.next_view_seq(),
                tree: &InScope(&self.reader),
            });
        }
        // :4840-4852; `*fNewBlock = true` before the write.
        let pos = match self.files.save_block(block, e.height, dbp) {
            Ok(Some(pos)) => pos,
            Ok(None) => {
                return Ok(BlockResult {
                    new_block: true,
                    ..BlockResult::rejected(BlockState::Error(
                        "AcceptBlock: Failed to find position to write new block to disk".into(),
                    ))
                });
            }
            // :4849-4850: `AbortNode(state, ...)` reports and requests shutdown, then fails the
            // block with its error, which `ProcessNewBlock` hands to `BlockChecked` (:4896-4897,
            // `submitblock`'s -25). V's loop ends once the command has (BL-279).
            Err(msg) => {
                let f = Fatal::new(format!("System error: {msg}"));
                let state = BlockState::Error(f.0.clone());
                self.carry_past(Err(f))?;
                return Ok(BlockResult {
                    new_block: true,
                    ..BlockResult::rejected(state)
                });
            }
        };
        let n_tx = u32::try_from(block.txs().len()).expect("CheckBlock bounds the count");
        self.idx.received_block_transactions(&mut self.chain, &self.reader, id, n_tx, pos)?;
        // :4860-4862 (NONE: only pruning can happen). Core ignores the result: an `AbortNode` in
        // the flush has reported and requested shutdown, and the block, accepted, goes on to its
        // ABC, which stops at the shutdown check after one step (BL-279).
        let flushed = self.flush_now(FlushStateMode::None);
        self.carry_past(flushed)?;
        // :4864
        self.check_block_index()?;
        Ok(BlockResult {
            new_block: true,
            ..skipped
        })
    }
}
