//! `ActivateBestChain` and its steps (§4.1–§4.5, §4.11) over SB4's holds.
//!
//! Each `cs_main` scope of ABC's outer loop is one hold ([`InHold`], §2.5): every tip step takes
//! its `seq` and fans its event out at the step, V drains the feeds after the step before
//! anything else, and the step's queued signals are staged. Then the release, in N1's order:
//! one snapshot, the `ChainView`, the staged signals spliced into the queue, `TipWatch`, then
//! the UI notifications ([`V::after_release`]).

use std::cell::RefCell;
use std::sync::Arc;
#[cfg(feature = "test-support")]
use std::sync::PoisonError;

use dash_chain::{
    ChainError, ChainReader, EntryView, FlatFilePos, HeaderChain, Hold, Parker, Publish, Status,
    TipStep, Work, updated_block_tip_schedules,
};
use dash_clock::{Clock, SystemClock, format_iso8601_date_time};
use dash_consensus::deployments::ThresholdState;
use dash_consensus::pow::block_proof;
use dash_consensus::{Block, BlockId, BlockValidationError, BlockValidationResult};
use dash_num::CompactTarget;
use dash_params::ChainParams;

#[cfg(test)]
use super::actor::tests::{Hook, Stage};
use super::actor::{V, VContext};
use super::command::BlockState;
use super::deployments::{ChainDeployments, unknown_rules_warning};
use super::flush::{FlushChain, FlushHost, FlushSite, FlushStateMode};
use super::index::{CandKey, NodeBlockIndex, entry};
use super::load::guess_verification_progress;
use super::locks::ClSigs;
use super::read_ahead::ReadAhead;
use super::seams::{
    AttemptOrigin, BlockConnector, BlockFiles, BlockStatusWriter, ConnectCx, ConnectOutcome,
    DisconnectPool, DisconnectResult, Fatal, GuardScope, HeldChainLocks, MempoolSink,
    RemovalReason, SyncState, ViewTarget,
};
use super::signals::{Queued, Staged, ValidationSignals};
use super::view::LoadingFlags;

/// V's test hook at `stage` of a release (§9.4's adversarial schedule), run right after the
/// stage's action so that a reordering moves the report with it.
#[cfg(test)]
fn at_stage(hook: &Option<Hook>, stage: Stage) {
    if let Some(hook) = hook {
        hook(stage);
    }
}

/// ABC's `pblock` (`validation.cpp:3730`) with Core's `fChecked` memo on that object (CH3 D42):
/// whether `CheckBlock` passed it with both checks on.
#[derive(Clone, Copy)]
pub(crate) struct PBlock<'b> {
    pub(crate) block: &'b Arc<Block>,
    pub(crate) checked: bool,
}

/// How a connector call's context gets `is_ibd` (CH3 D3): Core evaluates the latch only where
/// `ConnectBlock` asks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IbdRead {
    /// Evaluated as the call starts: `ConnectBlock` evaluates `IsInitialBlockDownload()` at
    /// `:2807`.
    Evaluate,
    /// The latch as it stands, evaluated nowhere: `DisconnectBlock` asks no IBD question, so a
    /// disconnect latches nothing Core's would not.
    Latched,
}

/// Where `ConnectTip`'s reads lead (CH4-4): ABC's `pindexMostWork`, and the last height the
/// read-ahead reads, below the `pblock` V holds.
#[derive(Clone, Copy)]
struct ReadPath {
    most_work: BlockId,
    end: u32,
}

/// Where a hold's tip steps go.
pub(crate) enum Steps<'h> {
    /// V's own hold (§2.5).
    Hold(Hold<'h>),
    /// SB4's `enforce_block`, which holds the tip mutex itself and lends V the chain for each
    /// `TipHost` call (§5.4).
    Enforce(&'h mut HeaderChain),
    /// Between two `TipHost` calls: SB4 has the chain.
    Lent,
}

impl Steps<'_> {
    pub(super) fn chain(&mut self) -> &mut HeaderChain {
        match self {
            Steps::Hold(h) => h.chain(),
            Steps::Enforce(c) => c,
            Steps::Lent => unreachable!("the chain is lent to SB4"),
        }
    }

    fn connect_tip(&mut self, id: BlockId) -> Result<TipStep, ChainError> {
        match self {
            Steps::Hold(h) => h.connect_tip(id),
            Steps::Enforce(_) | Steps::Lent => unreachable!("EnforceBlock only disconnects"),
        }
    }

    fn disconnect_tip(&mut self) -> Result<TipStep, ChainError> {
        match self {
            Steps::Hold(h) => h.disconnect_tip(),
            Steps::Enforce(c) => c.disconnect_tip(),
            Steps::Lent => unreachable!("the chain is lent to SB4"),
        }
    }
}

/// The flush's chain inside a hold: the fanout drains through the held guard, or through SB4's
/// walk state inside `enforce_block` (§2.5).
impl FlushChain for Steps<'_> {
    fn chain(&mut self) -> &mut HeaderChain {
        Steps::chain(self)
    }

    fn retry_pending(&mut self) -> Publish {
        match self {
            Steps::Hold(h) => h.retry_pending(),
            Steps::Enforce(c) => c.retry_pending(),
            Steps::Lent => unreachable!("the chain is lent to SB4"),
        }
    }
}

/// One hold (§2.5): SB4's tip lock across a Core `cs_main` scope, with the rest of V borrowed
/// beside it. Its signals and UI notifications wait in it for the release. Inside SB4's
/// `enforce_block` the same parts serve V's `TipHost` ([`Steps::Enforce`]).
pub(crate) struct InHold<'v, 'h> {
    pub(super) hold: Steps<'h>,
    pub(super) idx: &'v mut NodeBlockIndex,
    pub(super) reader: &'v ChainReader,
    params: &'v ChainParams,
    files: &'v dyn BlockFiles,
    /// V's read-ahead (CH4-4).
    read_ahead: &'v mut ReadAhead,
    connector: &'v mut dyn BlockConnector,
    /// V's context, lent into the connector's calls (CH3 §3.2).
    vcx: &'v mut VContext,
    /// V's ChainLock signatures: `GetBestChainLockHeight()` for the connector's context.
    cl: &'v ClSigs,
    guard: GuardScope,
    /// V's deployments and Core's caches (CH3 §2.16): the connector's context, `UpdateTip`'s
    /// warning.
    deployments: &'v mut ChainDeployments,
    pub(super) mempool: &'v mut dyn MempoolSink,
    flush: &'v mut dyn FlushSite,
    /// Core `m_blockman.m_have_pruned`: step 7's clears set it (§7.2).
    have_pruned: &'v mut bool,
    pub(super) signals: &'v ValidationSignals,
    clock: &'v dyn Clock,
    pub(super) loading: &'v LoadingFlags,
    wake: &'v Parker,
    /// The queued signals of the hold, in emission order (DEC-156 (2)).
    pub(super) staged: Staged,
    /// `uiInterface.NotifyBlockTip`s, fired after publication.
    pub(super) ui: Vec<(SyncState, BlockId)>,
    /// `UpdateTip`'s `DoWarning`s, in order, fired after publication before the UI
    /// notifications (N1), as Core's precede `NotifyBlockTip` in the scope. A hold that ends in
    /// `AbortNode` drops them with its signals and UI notifications, where Core's had run.
    warnings: Vec<String>,
    /// `UpdateTip`'s `g_best_block`, set at the release.
    watch: Option<BlockId>,
    /// An `UpdatedBlockTip` passed the ChainLock handler's guards (§5.2).
    cl_tip: bool,
    #[cfg(test)]
    release_hook: Option<Hook>,
}

/// What a released hold leaves to emit after publication (N1).
pub(crate) struct Released {
    staged: Staged,
    ui: Vec<(SyncState, BlockId)>,
    warnings: Vec<String>,
    watch: Option<BlockId>,
    cl_tip: bool,
}

impl<'v, 'h> InHold<'v, 'h> {
    pub(super) fn chain(&mut self) -> &mut HeaderChain {
        self.hold.chain()
    }

    /// The same parts over `steps`, and the steps they had.
    pub(super) fn with_steps<'s>(self, steps: Steps<'s>) -> (InHold<'v, 's>, Steps<'h>) {
        let InHold {
            hold,
            idx,
            reader,
            params,
            files,
            read_ahead,
            connector,
            vcx,
            cl,
            guard,
            deployments,
            mempool,
            flush,
            have_pruned,
            signals,
            clock,
            loading,
            wake,
            staged,
            ui,
            warnings,
            watch,
            cl_tip,
            #[cfg(test)]
            release_hook,
        } = self;
        let h = InHold {
            hold: steps,
            idx,
            reader,
            params,
            files,
            read_ahead,
            connector,
            vcx,
            cl,
            guard,
            deployments,
            mempool,
            flush,
            have_pruned,
            signals,
            clock,
            loading,
            wake,
            staged,
            ui,
            warnings,
            watch,
            cl_tip,
            #[cfg(test)]
            release_hook,
        };
        (h, hold)
    }

    /// `m_chain.Tip()`, current inside the hold (CH1-A2).
    pub(super) fn tip(&mut self) -> Option<BlockId> {
        self.chain().private_tip().map(|(t, _)| t.id)
    }

    /// The mempool, the connector's evo reads and the deployments, for V's drains (CH7 D60).
    pub(super) fn drain_parts(
        &mut self,
    ) -> (&mut dyn MempoolSink, &dyn super::seams::EvoReads, &ChainDeployments) {
        (&mut *self.mempool, self.connector.evo_reads(), &*self.deployments)
    }

    /// `f` over the connector, its context for a tip-path attempt (CH3 D3), V's context and the
    /// status writer of `id`. The context's IBD is read first, as `ibd` says; the chain is lent
    /// to the status writer and the ChainLock view for the call. The deployments read V's chain
    /// through their own handle, which borrows nothing the cell holds (CH3 §2.16).
    fn with_connector<R>(
        &mut self,
        id: BlockId,
        ibd: IbdRead,
        f: impl FnOnce(
            &mut dyn BlockConnector,
            &ConnectCx<'_>,
            &mut VContext,
            &mut BlockStatusWriter<'_>,
        ) -> R,
    ) -> R {
        let is_ibd = match ibd {
            IbdRead::Evaluate => self.is_ibd(),
            // Core's latch test comes before its `LoadingBlocks()` test (`:1979-1987`), so with
            // loading set the answer is the latch alone and nothing is evaluated.
            IbdRead::Latched => {
                let now = self.clock.node_time();
                self.chain().is_ibd_private(now, true)
            }
        };
        let chain = RefCell::new(self.hold.chain());
        let best_header = chain.borrow().best_header();
        let deployments = self.deployments.view();
        let chainlocks = HeldChainLocks::new(&chain, self.cl.best_height());
        let cx = ConnectCx {
            params: self.params,
            reader: self.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: self.reader.min_chain_work(),
            is_ibd,
            chainlocks: &chainlocks,
            guard: self.guard,
            origin: AttemptOrigin::Tip,
        };
        let mut st = BlockStatusWriter::new(&chain, id);
        f(&mut *self.connector, &cx, &mut *self.vcx, &mut st)
    }

    pub(super) fn key(&self, id: BlockId) -> CandKey {
        self.idx.key_of(self.reader, id)
    }

    /// `IsInitialBlockDownload()` under `cs_main` (CH1-A8).
    pub(super) fn is_ibd(&mut self) -> bool {
        let (now, loading) = (self.clock.node_time(), self.loading.any());
        self.chain().is_ibd_private(now, loading)
    }

    /// The release (§2.5): one snapshot, stored before the tip lock drops. The caller runs the
    /// rest of the sequence ([`V::after_release`]).
    pub(super) fn release(self) -> Released {
        #[cfg(test)]
        at_stage(&self.release_hook, Stage::Release);
        let (h, Steps::Hold(hold)) = self.with_steps(Steps::Lent) else {
            unreachable!("only V's own hold releases");
        };
        hold.release();
        #[cfg(test)]
        at_stage(&h.release_hook, Stage::Snapshot);
        h.emitted()
    }

    /// What the scope left to emit after publication: after [`InHold::release`], or after SB4's
    /// `enforce_block` stored its snapshot.
    pub(super) fn emitted(self) -> Released {
        Released {
            staged: self.staged,
            ui: self.ui,
            warnings: self.warnings,
            watch: self.watch,
            cl_tip: self.cl_tip,
        }
    }

    /// `UpdatedBlockTip(new, fork)` as V emits it: the synchronous listeners inline (LO-5), the
    /// queued signal for the release, and the ChainLock handler's (`h:185-207`) guards here, its
    /// task queued at the release, before V takes anything else, so as if inline (§5.2, D12).
    pub(super) fn updated_block_tip(&mut self, new: BlockId, fork: Option<BlockId>, ibd: bool) {
        self.signals.sync_updated_block_tip(new, fork, ibd);
        self.staged.push(Queued::UpdatedBlockTip {
            new,
            fork,
            ibd,
        });
        self.cl_tip |= updated_block_tip_schedules(new, fork, ibd);
    }

    /// The candidates rescan once a block has left the active chain (`validation.cpp:4034-4038`,
    /// `:4135-4143`): every eligible chain block no worse than `tip` goes back in.
    pub(super) fn readd_candidates(&mut self, tip: BlockId) {
        let tip_key = self.key(tip);
        for c in self.hold.chain().ids() {
            let e = entry(self.reader, c);
            if self.idx.is_candidate_eligible(&e) && self.idx.key(&e) >= tip_key {
                self.idx.candidates_insert(self.reader, c);
            }
        }
    }

    /// SB5's writer rule inside a hold (§2.5, DEC-153 (1)): after every step, before anything
    /// else that could start another, the pending fanout drains through the held guard. V's
    /// only wait here is the in-hold service loop's (H3): it answers W's events (BL-123) and a
    /// subscriber's `FlushDue` at the stepped tip (D47 item 10), and W's bell wakes it beside
    /// the subscribers' space and deregistrations.
    fn drain_fanout(&mut self) -> Result<(), Fatal> {
        while self.hold.retry_pending() == Publish::Blocked {
            self.with_flush(|f, host| f.answer_due(host))?;
            // `answer_due` may have parked on this wake for W and taken the subscriber's room
            // with W's release (b4 D81): retry before parking, as `backpressure_wait` does.
            if self.hold.retry_pending() != Publish::Blocked {
                break;
            }
            #[cfg(test)]
            at_stage(&self.release_hook, Stage::Drain);
            self.wake.park();
        }
        Ok(())
    }

    /// Runs `f` on V's flush side with the hold's chain lent (§7.1): the `ChainStateFlushed`
    /// signals W released meanwhile are staged with the hold's callbacks (DEC-166, N1).
    fn with_flush<T>(
        &mut self,
        f: impl FnOnce(&mut dyn FlushSite, &mut FlushHost<'_>) -> Result<T, Fatal>,
    ) -> Result<T, Fatal> {
        let (now, loading) = (self.clock.node_time(), self.loading.any());
        let host = FlushHost::new(&mut self.hold, self.idx, self.have_pruned, now, loading);
        host.lend(self.flush, self.reader, |q| self.staged.push(q), f)
    }

    /// `ConnectTip`'s `ReadBlockFromDisk` (`v:3414-3422`) of `e`, the next block on `path`, and
    /// whether `CheckBlock` passed it (D42): the read-ahead's block when it has one for `e`
    /// (CH4-4), else V's own read, which no `CheckBlock` has passed.
    fn read_to_connect(
        &mut self,
        e: &EntryView,
        path: ReadPath,
    ) -> Result<(Arc<Block>, bool), Fatal> {
        self.read_ahead.plan(self.reader, e.height, path.most_work, path.end);
        let Some(block) = self.read_ahead.take(e) else {
            return Ok((self.own_read(e.id)?, false));
        };
        self.read_ahead.xcheck(|| self.own_read(e.id), &block)?;
        Ok((block, true))
    }

    /// V's own read for `ConnectTip`: its failure is Core's `AbortNode` (`v:3416-3418`).
    fn own_read(&self, id: BlockId) -> Result<Arc<Block>, Fatal> {
        self.read_block(id).map_err(|msg| {
            tracing::error!("ConnectTip: {msg}");
            Fatal::new("Failed to read block")
        })
    }

    /// `ReadBlockFromDisk(block, pindex)`.
    fn read_block(&self, id: BlockId) -> Result<Arc<Block>, String> {
        let e = entry(self.reader, id);
        let file = e.file.filter(|_| e.status.has(Status::HAVE_DATA));
        let file = file.ok_or_else(|| format!("no data for block {}", e.hash))?;
        self.files.read_block(
            FlatFilePos {
                file,
                pos: e.data_pos,
            },
            &e.hash,
        )
    }

    // ---- §4.3 --------------------------------------------------------------------------------

    /// Core `FindMostWorkChain` (`validation.cpp:3525-3587`).
    fn find_most_work_chain(&mut self) -> Result<Option<BlockId>, Fatal> {
        loop {
            let Some(new) = self.idx.best_candidate() else {
                return Ok(None);
            };
            let mut test = Some(new);
            let mut invalid_ancestor = false;
            while let Some(t) = test.filter(|&t| !self.chain().active_contains_private(t)) {
                let et = entry(self.reader, t);
                debug_assert!(self.idx.have_txs_downloaded(t) || et.height == 0, ":3544");
                let failed = et.status.has(Status::FAILED_MASK);
                let conflicting = et.status.has(Status::CONFLICT_CHAINLOCK);
                let missing = !et.status.has(Status::HAVE_DATA);
                if failed || missing || conflicting {
                    // :3555-3557
                    let new_work = entry(self.reader, new).chain_work;
                    if failed
                        && self
                            .idx
                            .best_invalid()
                            .is_none_or(|b| new_work > entry(self.reader, b).chain_work)
                    {
                        self.idx.set_best_invalid(Some(new));
                    }
                    // :3558-3570: the whole chain from `new` down to `t` leaves the set.
                    let mut f = new;
                    while f != t {
                        let prev = entry(self.reader, f).prev.expect("f is above t");
                        if failed || conflicting {
                            let bit = if failed {
                                Status::FAILED_CHILD
                            } else {
                                Status::CONFLICT_CHAINLOCK
                            };
                            let mut ix = self.chain().node_index()?;
                            ix.modify(f, |g| g.status = Status(g.status.bits() | bit));
                            ix.mark_dirty(f);
                        } else {
                            self.idx.unlinked_insert(prev, f);
                        }
                        self.idx.candidates_erase(f);
                        f = prev;
                    }
                    self.idx.candidates_erase(t);
                    invalid_ancestor = true;
                    break;
                }
                test = et.prev;
            }
            if !invalid_ancestor {
                return Ok(Some(new));
            }
        }
    }

    /// Core `CheckForkWarningConditions` (`validation.cpp:2021-2037`).
    fn check_fork_warning_conditions(&mut self) {
        if self.is_ibd() {
            return;
        }
        let tip = entry(self.reader, self.tip().expect("a tip after IBD"));
        let proof = block_proof(CompactTarget::new(tip.header.bits));
        let large = self
            .idx
            .best_invalid()
            .is_some_and(|b| entry(self.reader, b).chain_work > tip.chain_work + proof * 6u32);
        if large {
            tracing::warn!(
                "CheckForkWarningConditions: Warning: Found invalid chain which has higher work (at \
                 least ~6 blocks worth of work) than our best chain.\nChain state database \
                 corruption likely."
            );
        }
        self.idx.set_large_work_invalid_chain(large);
    }

    /// Core `InvalidChainFound` (`validation.cpp:2040-2062`).
    pub(super) fn invalid_chain_found(&mut self, n: BlockId) -> Result<(), Fatal> {
        let en = entry(self.reader, n);
        if self.idx.best_invalid().is_none_or(|b| en.chain_work > entry(self.reader, b).chain_work)
        {
            self.idx.set_best_invalid(Some(n));
        }
        // :2049-2051, on the writer's private tip.
        self.chain().node_index()?.best_header_invalid_chain(n);
        tracing::info!("InvalidChainFound: invalid block={}  height={}", en.hash, en.height);
        let tip = entry(self.reader, self.tip().expect(":2057 assert(tip)"));
        tracing::info!("InvalidChainFound:  current best={}  height={}", tip.hash, tip.height);
        self.check_fork_warning_conditions();
        Ok(())
    }

    /// Core `ConflictingChainFound` (`validation.cpp:2064-2079`) at `tip`, Core's `m_chain.Tip()`.
    /// `log2_work` and the dates are left out of its lines, as in `UpdateTip` (D30).
    pub(super) fn conflicting_chain_found(&mut self, n: BlockId, tip: BlockId) {
        let (en, et) = (entry(self.reader, n), entry(self.reader, tip));
        tracing::info!(
            "ConflictingChainFound: conflicting block={}  height={}",
            en.hash,
            en.height
        );
        tracing::info!("ConflictingChainFound:  current best={}  height={}", et.hash, et.height);
        self.check_fork_warning_conditions();
    }

    /// Core `InvalidBlockFound` (`validation.cpp:2083-2095`).
    fn invalid_block_found(
        &mut self,
        id: BlockId,
        err: &BlockValidationError,
    ) -> Result<(), Fatal> {
        if err.result() == BlockValidationResult::Mutated {
            return Ok(());
        }
        let mut ix = self.chain().node_index()?;
        ix.modify(id, |g| g.status = Status(g.status.bits() | Status::FAILED_VALID));
        ix.failed_blocks().insert(id);
        ix.mark_dirty(id);
        self.idx.candidates_erase(id);
        self.invalid_chain_found(id)
    }

    // ---- §4.2 --------------------------------------------------------------------------------

    /// Core `ActivateBestChainStep` (`validation.cpp:3607-3690`): whether an invalid block was
    /// found (`fInvalidFound`). `Err` is a system error, after `AbortNode` where Core calls it.
    fn abc_step(
        &mut self,
        most_work: BlockId,
        pblock: Option<PBlock<'_>>,
        trace: &mut Vec<(BlockId, Arc<Block>)>,
    ) -> Result<bool, Fatal> {
        let mut pool = self.mempool.new_disconnect_pool();
        match self.abc_step_moves(most_work, pblock, trace, pool.as_mut()) {
            Ok((invalid_found, disconnected)) => {
                if disconnected {
                    self.reorg_drain(pool, true)?;
                }
                let next = self.tip().map_or(0, |t| entry(self.reader, t).height + 1);
                self.mempool.check(next);
                self.check_fork_warning_conditions();
                Ok(invalid_found)
            }
            Err(f) => {
                // :3622-3627, :3662-3667: every system error leaves the mempool consistent with
                // the tip "in case any observers try to use it before shutdown".
                self.reorg_drain(pool, false)?;
                Err(f)
            }
        }
    }

    /// `ActivateBestChainStep`'s disconnects and connects: (`fInvalidFound`, whether it
    /// disconnected).
    fn abc_step_moves(
        &mut self,
        most_work: BlockId,
        pblock: Option<PBlock<'_>>,
        trace: &mut Vec<(BlockId, Arc<Block>)>,
        pool: &mut dyn DisconnectPool,
    ) -> Result<(bool, bool), Fatal> {
        let old_tip = self.tip();
        let fork = self.chain().find_fork_private(most_work);
        let mut disconnected = false;
        // :3618
        while let Some(t) = self.tip()
            && Some(t) != fork
        {
            if !self.disconnect_tip(Some(&mut *pool))? {
                // Reported whatever `DisconnectBlock` reported before it (CH3 §2.1.1: Core's
                // second `AbortNode` on its evo-check path).
                return Err(Fatal::abort_node(
                    "Failed to disconnect block; see debug.log for details",
                ));
            }
            disconnected = true;
        }
        // :3632-3678
        let most_work_height = entry(self.reader, most_work).height;
        let path = ReadPath {
            most_work,
            end: most_work_height.saturating_sub(u32::from(pblock.is_some())),
        };
        let most_work_height = i64::from(most_work_height);
        let mut height = fork.map_or(-1, |f| i64::from(entry(self.reader, f).height));
        let mut invalid_found = false;
        let mut cont = true;
        while cont && height != most_work_height {
            let target = (height + 32).min(most_work_height);
            // Descending, as Core's `vpindexToConnect`.
            let mut to_connect = Vec::with_capacity((target - height) as usize);
            let mut at = self.reader.ancestor(most_work, target as u32);
            while let Some(b) = at.filter(|&b| i64::from(entry(self.reader, b).height) != height) {
                to_connect.push(b);
                at = entry(self.reader, b).prev;
            }
            height = target;
            for &c in to_connect.iter().rev() {
                let block = pblock.filter(|_| c == most_work);
                match self.connect_tip(c, block, path, trace, &mut *pool)? {
                    ConnectOutcome::Connected => {
                        let tip = self.tip().expect("connected");
                        self.idx.prune_block_index_candidates(self.reader, tip);
                        let more = old_tip.is_none_or(|o| {
                            entry(self.reader, tip).chain_work > entry(self.reader, o).chain_work
                        });
                        if more {
                            cont = false;
                            break;
                        }
                    }
                    ConnectOutcome::Invalid(err) => {
                        // CH4-4: what the read-ahead holds past an invalid block is dead.
                        self.read_ahead.advance_epoch();
                        // :3654-3656: the batch's top, Core's `front()`, not the failed block.
                        if err.result() != BlockValidationResult::Mutated {
                            self.invalid_chain_found(to_connect[0])?;
                        }
                        invalid_found = true;
                        cont = false;
                        break;
                    }
                    // The connector reported it (CH3 §2.1.1).
                    ConnectOutcome::SystemError(msg) => return Err(Fatal::reported(msg)),
                }
            }
        }
        Ok((invalid_found, disconnected))
    }

    // ---- §4.4, §4.5 --------------------------------------------------------------------------

    /// Core `ConnectTip` (`validation.cpp:3401-3519`).
    fn connect_tip(
        &mut self,
        id: BlockId,
        pblock: Option<PBlock<'_>>,
        path: ReadPath,
        trace: &mut Vec<(BlockId, Arc<Block>)>,
        pool: &mut dyn DisconnectPool,
    ) -> Result<ConnectOutcome, Fatal> {
        let e = entry(self.reader, id);
        debug_assert_eq!(e.prev, self.tip(), ":3406");
        // :3415-3423
        let (block, checked) = match pblock {
            Some(p) => (p.block.clone(), p.checked),
            None => self.read_to_connect(&e, path)?,
        };
        // A storage fault is Core's `std::abort()` inside `ConnectBlock` (`CCoinsViewErrorCatcher`,
        // `coins.cpp:356-380`): no `BlockChecked` follows (CH3 §2.1). An `AbortNode` inside it is a
        // `SystemError`, which the connector has reported (CH3 §2.1.1).
        let outcome = self
            .with_connector(id, IbdRead::Evaluate, |connector, cx, v, st| {
                connector.connect_block(cx, v, ViewTarget::Active, &block, checked, id, st)
            })
            .inspect_err(|f| tracing::error!("ConnectTip: ConnectBlock {} failed, {f}", e.hash))?;
        let state = match &outcome {
            ConnectOutcome::Connected => BlockState::Valid,
            ConnectOutcome::Invalid(err) => BlockState::Invalid(err.clone()),
            ConnectOutcome::SystemError(msg) => BlockState::Error(msg.clone()),
        };
        // :3438, inline (LO-5) with V's IBD as the argument (§2.5), before an invalid block or a
        // system error returns.
        let ibd = self.is_ibd();
        self.signals.block_checked(&block, &state, ibd);
        match outcome {
            ConnectOutcome::Connected => {}
            ConnectOutcome::Invalid(err) => {
                self.invalid_block_found(id, &err)?;
                tracing::error!("ConnectTip: ConnectBlock {} failed, {err}", e.hash);
                return Ok(ConnectOutcome::Invalid(err));
            }
            ConnectOutcome::SystemError(msg) => {
                tracing::error!("ConnectTip: ConnectBlock {} failed, {msg}", e.hash);
                return Ok(ConnectOutcome::SystemError(msg));
            }
        }
        // :3463: the decision; its freeze follows the step (§7.1, D3).
        let decided = self.with_flush(|f, host| f.at_site(FlushStateMode::IfNeeded, host))?;
        // :3472-3476
        self.remove_for_block(block.txs(), e.height)?;
        self.mempool.remove_expired_asset_unlock(e.height);
        pool.remove_for_block(block.txs());
        // :3479: SetTip, the step's `seq` and its fanout; the decision at the post-step pair;
        // then SB5's per-step drain.
        self.hold.connect_tip(id)?;
        self.with_flush(|f, host| f.after_step(decided, host))?;
        self.drain_fanout()?;
        self.update_tip(id);
        trace.push((id, block));
        Ok(ConnectOutcome::Connected)
    }

    /// Core `DisconnectTip` (`validation.cpp:3255-3348`): `false` is Core's failure, which the
    /// caller turns into `AbortNode`.
    pub(super) fn disconnect_tip(
        &mut self,
        pool: Option<&mut dyn DisconnectPool>,
    ) -> Result<bool, Fatal> {
        let d = self.tip().expect(":3263 assert(pindexDelete)");
        // CH4-4: the read-ahead's path leaves the active chain here.
        self.read_ahead.advance_epoch();
        let e = entry(self.reader, d);
        let prev = e.prev.expect(":3264 assert(pindexDelete->pprev)");
        let block = match self.read_block(d) {
            Ok(b) => b,
            Err(msg) => {
                tracing::error!("DisconnectTip(): Failed to read block ({msg})");
                return Ok(false);
            }
        };
        // :3285-3288
        let result = self.with_connector(d, IbdRead::Latched, |connector, cx, v, _| {
            connector.disconnect_block(cx, v, ViewTarget::Active, &block, d)
        })?;
        if result != DisconnectResult::Ok {
            tracing::error!("DisconnectTip(): DisconnectBlock {} failed", e.hash);
            return Ok(false);
        }
        // :3298-3307, the prune-lock moveback (D61); what CH2 prunes is still bounded by the
        // durable locators' LCA (§4.5). :3310, the decision; its freeze follows the step (§7.1,
        // D3).
        self.flush.move_prune_locks_back(e.height);
        let decided = self.with_flush(|f, host| f.at_site(FlushStateMode::IfNeeded, host))?;
        if let Some(pool) = pool {
            for tx in pool.add_from_block(block.txs()) {
                self.mempool.remove_recursive(&tx, RemovalReason::Reorg);
            }
        }
        // :3322, then the decision at the post-step pair, `(prev, the step's seq)`.
        self.hold.disconnect_tip()?;
        self.with_flush(|f, host| f.after_step(decided, host))?;
        self.drain_fanout()?;
        self.update_tip(prev);
        // :3326-3328, staged (N1).
        self.staged.push(Queued::BlockDisconnected {
            block,
            id: d,
        });
        Ok(true)
    }

    /// Core `UpdateTip` (`validation.cpp:3196-3243`) for the active chainstate. `g_best_block`
    /// is recorded for the release (N1). Out of IBD, the unknown-versionbits warning (D30, CH3
    /// §2.16): a bit whose unknown rules are active is a `DoWarning`, staged for the release
    /// (N1); one locked in is appended to the log line.
    fn update_tip(&mut self, new: BlockId) {
        self.mempool.add_transactions_updated(1);
        self.watch = Some(new);
        // :3224-3239
        let mut locked_in = Vec::new();
        if !self.is_ibd() {
            for (bit, state) in self.deployments.unknown_rules(new) {
                let warning = unknown_rules_warning(bit);
                if state == ThresholdState::Active {
                    self.warnings.push(warning);
                } else {
                    locked_in.push(warning);
                }
            }
        }
        // :3174-3193, `UpdateTipLog`; the ", " is `AppendWarning`'s (:3167-3171).
        let warning = match locked_in.is_empty() {
            true => String::new(),
            false => format!(" warning='{}'", locked_in.join(", ")),
        };
        let e = entry(self.reader, new);
        let chain_tx = self.idx.n_chain_tx(new);
        let usage = self.flush.cache_usage();
        tracing::info!(
            "UpdateTip: new best={} height={} version=0x{:08x} log2_work={:.6} tx={} date='{}' \
             progress={:.6} cache={:.1}MiB({}utxo) evodb_cache={:.1}MiB{warning}",
            e.hash,
            e.height,
            e.header.version,
            log2_work(e.chain_work),
            chain_tx,
            format_iso8601_date_time(i64::from(e.header.time)),
            // `GuessVerificationProgress` (`:5913-5928`) reads `time(nullptr)`, not mock time.
            guess_verification_progress(
                &self.params.chain_tx_data,
                chain_tx,
                e.header.time,
                SystemClock.node_now().secs()
            ),
            mib(usage.coins_bytes),
            usage.coins,
            mib(usage.evo_bytes),
        );
    }
}

impl V {
    /// Opens a hold over V's chain, with V's other parts beside it.
    pub(super) fn hold(&mut self) -> InHold<'_, '_> {
        let (chain, h) = self.lend();
        h.with_steps(Steps::Hold(chain.hold_tip())).0
    }

    /// V's chain, and its other parts with no steps yet: SB4's `enforce_block` takes the one and
    /// lends it back to the other per `TipHost` call (§5.4).
    pub(super) fn lend(&mut self) -> (&mut HeaderChain, InHold<'_, 'static>) {
        #[cfg(test)]
        let release_hook = self.release_hook.clone();
        let V {
            chain,
            idx,
            reader,
            params,
            files,
            read_ahead,
            connector,
            vcx,
            cl,
            guard,
            deployments,
            mempool,
            flush,
            have_pruned,
            signals,
            clock,
            loading,
            wake,
            ..
        } = self;
        let h = InHold {
            hold: Steps::Lent,
            idx,
            reader,
            params,
            files: &**files,
            read_ahead,
            connector: &mut **connector,
            vcx,
            cl: &cl.sigs,
            guard: *guard,
            deployments,
            mempool: &mut **mempool,
            flush: &mut **flush,
            have_pruned,
            signals,
            clock: &**clock,
            loading,
            wake,
            staged: Staged::default(),
            ui: Vec::new(),
            warnings: Vec::new(),
            watch: None,
            cl_tip: false,
            #[cfg(test)]
            release_hook,
        };
        (chain, h)
    }

    /// The release's sequence after the snapshot (§2.5, N1): the rest of `scope_end` (the
    /// ChainLock mirror with N5, then the `ChainView`), the staged signals in one splice,
    /// `TipWatch`, `UpdateTip`'s warnings, then the UI notifications in order. A view that is not
    /// published (an evo read fault, `AbortNode`) ends it there.
    pub(super) fn after_release(&mut self, r: Released) {
        let Released {
            mut staged,
            ui,
            warnings,
            watch,
            cl_tip,
        } = r;
        #[cfg(feature = "test-support")]
        {
            let hook = self.between_stores.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        if cl_tip {
            self.cl_schedule_tip();
        }
        if !self.publish_view() {
            // AbortNode: the staged signals, the watch and the UI would name a tip no view shows.
            return;
        }
        #[cfg(test)]
        at_stage(&self.release_hook, Stage::Published);
        self.signals.splice(&mut staged);
        #[cfg(test)]
        at_stage(&self.release_hook, Stage::Spliced);
        if let Some(t) = watch {
            self.tip_watch.set(entry(&self.reader, t).hash);
            #[cfg(test)]
            at_stage(&self.release_hook, Stage::Watched);
        }
        for warning in &warnings {
            self.do_warning(warning);
        }
        for (state, id) in ui {
            let e = entry(&self.reader, id);
            self.ui.notify_block_tip(state, e.hash, e.height);
            #[cfg(test)]
            at_stage(&self.release_hook, Stage::Ui);
        }
    }

    /// Core `DoWarning(warning)` (`validation.cpp:3156-3164`): the misc warning, and the first
    /// time only, `AlertNotify`.
    fn do_warning(&mut self, warning: &str) {
        self.ui.set_misc_warning(warning);
        if !self.warned {
            self.ui.alert_notify(warning);
            self.warned = true;
        }
    }

    /// Core `ActivateBestChain(state, pblock)` (`validation.cpp:3730-3862`), as a class C
    /// operation. `Err` is `AbortNode`, Core's `false`, which V has stopped on before its caller
    /// logs (CH3b D213), or one of Core's asserts.
    pub(crate) fn activate_best_chain(&mut self, pblock: Option<PBlock<'_>>) -> Result<(), Fatal> {
        let r = self.abc(pblock);
        self.abort_at_fault(r)
    }

    fn abc(&mut self, pblock: Option<PBlock<'_>>) -> Result<(), Fatal> {
        debug_assert!(self.in_composite(), "m_chainstate_mutex (:3744)");
        let mut most_work: Option<BlockId> = None;
        let mut new_tip: Option<BlockId> = None;
        loop {
            // :3759: Core's `cs_main` is free here (§2.4).
            self.release_point()?;
            self.limit_queue()?;
            let mut h = self.hold();
            let starting_tip = h.tip();
            let mut blocks_connected = false;
            loop {
                let mut trace = Vec::new();
                if most_work.is_none() {
                    most_work = h.find_most_work_chain()?;
                }
                // :3784-3786
                let Some(mw) = most_work.filter(|&m| Some(m) != h.tip()) else {
                    break;
                };
                let hash = entry(h.reader, mw).hash;
                let block = pblock.filter(|b| b.block.hash() == hash);
                let invalid_found = h.abc_step(mw, block, &mut trace)?;
                blocks_connected = true;
                if invalid_found {
                    most_work = None;
                }
                new_tip = h.tip();
                // :3803-3806, staged (N1).
                for (id, block) in trace {
                    h.staged.push(Queued::BlockConnected {
                        block,
                        id,
                    });
                }
                // :3817: while (!Tip() || (starting_tip && Comparator(Tip(), starting_tip)))
                let worse = match (new_tip, starting_tip) {
                    (None, _) => true,
                    (Some(t), Some(s)) => h.key(t) < h.key(s),
                    (Some(_), None) => false,
                };
                if !worse {
                    break;
                }
            }
            if blocks_connected {
                let fork = starting_tip.and_then(|s| h.chain().find_fork_private(s));
                let ibd = h.is_ibd();
                if fork != new_tip {
                    let new = new_tip.expect("blocks were connected");
                    // :3825-3830: the synchronous listeners inline (LO-5), with V's facts.
                    h.updated_block_tip(new, fork, ibd);
                    h.ui.push((h.loading.sync_state(ibd), new));
                }
            }
            let r = h.release();
            self.after_release(r);
            if !blocks_connected {
                // :3818: no CheckBlockIndex, no PERIODIC flush.
                return Ok(());
            }
            // :3836, Core's test on its `int` (-1 stops at genesis)
            let n = i64::from(self.stop_at_height);
            if n != 0 && new_tip.is_some_and(|t| i64::from(entry(&self.reader, t).height) >= n) {
                self.request_shutdown();
            }
            // :3847
            if self.shutdown_requested() {
                break;
            }
            if new_tip == most_work {
                break;
            }
        }
        self.check_block_index()?;
        // :3857
        self.flush_now(FlushStateMode::Periodic)
    }
}

/// `UpdateTipLog`'s `log(nChainWork.getdouble()) / log(2.0)` (`validation.cpp:3187`), with
/// `getdouble`'s sum of 32-bit words (`arith_uint256.cpp:146-155`).
fn log2_work(work: Work) -> f64 {
    work.to_f64().ln() / 2.0_f64.ln()
}

/// `UpdateTipLog`'s `x * (1.0 / (1 << 20))` (`validation.cpp:3190-3192`).
fn mib(bytes: u64) -> f64 {
    bytes as f64 * (1.0 / f64::from(1_u32 << 20))
}
