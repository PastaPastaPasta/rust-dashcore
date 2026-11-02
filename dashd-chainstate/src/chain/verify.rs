//! Core's `VerifyLoadedChainstate` on V (CH1-node-LR, O2b D32): the startup step after the
//! chainstate loads and before init's `ActivateBestChain`, with `CVerifyDB::VerifyDB` (CH5-1a)
//! and the load-time `ResetBlockFailureFlags(nullptr)`.
//!
//! Core runs it in step 7 under `cs_main` (`init.cpp:2101-2108`, `node/chainstate.cpp:412-479`),
//! before step 11's `ThreadImport` sends the first `ActivateBestChain`. On V it is the class M
//! command [`ChainCommand::VerifyLoaded`](super::ChainCommand::VerifyLoaded), exempt from D38's
//! hold as the startup ABC it precedes is, so the order stays Core's.

use std::cell::RefCell;
use std::sync::Arc;

use dash_chain::{FlatFilePos, IndexAccess as _};
use dash_consensus::header::MAX_FUTURE_BLOCK_TIME;
use dash_consensus::{Block, BlockId};
use dash_primitives::BlockHash;

use super::actor::{V, VContext, VERIFY_LOADED_MISUSE, core_int};
use super::index::entry;
use super::seams::{
    AttemptOrigin, BlockConnector, BlockStatusWriter, ConnectCx, ConnectOutcome, DisconnectResult,
    Fatal, HeldChainLocks, ScratchParent, ViewTarget,
};

/// `-checkblocks`' default (`validation.h:89`).
pub const DEFAULT_CHECKBLOCKS: i64 = 6;
/// `-checklevel`'s default (`validation.h:90`).
pub const DEFAULT_CHECKLEVEL: i64 = 3;

/// Core's message for a loaded tip more than two hours ahead of the clock
/// (`node/chainstate.cpp:425-427`).
pub const FUTURE_TIP: &str = "The block database contains a block which appears to be from the \
                              future. This may be due to your computer's date and time being set \
                              incorrectly. Only rebuild the block database if you are sure that \
                              your computer's date and time are correct";
/// `node/chainstate.cpp:446`.
pub const VERIFY_INTERRUPTED: &str = "Block verification was interrupted";
/// `node/chainstate.cpp:448`.
pub const CORRUPTED_BLOCK_DB: &str = "Corrupted block database detected";
/// `node/chainstate.cpp:451`.
pub const INSUFFICIENT_DBCACHE: &str = "Insufficient dbcache for block verification";
/// `node/chainstate.cpp:473`.
pub const EVODB_WITHOUT_BLOCKS: &str = "Error initializing block database";

/// What `VerifyLoadedChainstate` reads of `ChainstateLoadOptions` (`node/chainstate.h:34-58`)
/// and of the loaded stores, as init sets it (`init.cpp:2068-2069`, `:2083-2085`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyLoadOptions {
    /// `-checkblocks`, as `GetIntArg` read it.
    pub check_blocks: i64,
    /// `-checklevel`, as `GetIntArg` read it. The reset compares this value; VerifyDB gets it
    /// narrowed to `int` ([`VerifyDbArgs::new`]).
    pub check_level: i64,
    /// Set when `-checkblocks` or `-checklevel` is: skipped level-3 checks then fail the load.
    pub require_full_verification: bool,
    /// Core's `is_coinsview_empty` (`:415-417`): `-reindex`, `-reindex-chainstate`, or coins
    /// with no best block.
    pub coins_empty: bool,
    /// `evodb.IsEmpty()` (`:471`).
    pub evodb_empty: bool,
}

impl Default for VerifyLoadOptions {
    /// Init's values with neither option set, over a loaded chainstate.
    fn default() -> VerifyLoadOptions {
        VerifyLoadOptions {
            check_blocks: DEFAULT_CHECKBLOCKS,
            check_level: DEFAULT_CHECKLEVEL,
            require_full_verification: false,
            coins_empty: false,
            evodb_empty: true,
        }
    }
}

/// `VerifyLoadedChainstate`'s failing statuses (`node/chainstate.h:66-73`) and their messages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyLoadError {
    /// `ChainstateLoadStatus::FAILURE`: init offers a reindex (`init.cpp:2119-2133`).
    Failure(&'static str),
    /// `INTERRUPTED`: a shutdown was requested.
    Interrupted(&'static str),
    /// `FAILURE_INSUFFICIENT_DBCACHE`: a plain `InitError` (`init.cpp:2115-2117`).
    InsufficientDbcache(&'static str),
    /// DEC-993: with `-checkblocks` or `-checklevel` set, VerifyDB skipped level 3 at a block
    /// this build cannot judge ([`L3Skip::NotReady`]): the load fails with the readiness refusal's
    /// text (ALLOWED-DIFFS D18), a plain `InitError` as the startup readiness guard's, where a
    /// memory skip fails with Core's [`INSUFFICIENT_DBCACHE`].
    NotReady(String),
}

/// `CVerifyDB::VerifyDB`'s result (`validation.h:383-389`), a level-3 skip with its cause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerifyDbResult {
    Success,
    CorruptedBlockDb,
    Interrupted,
    SkippedL3Checks(L3Skip),
    SkippedMissingBlocks,
}

/// Why VerifyDB skipped its level-3 checks (`SKIPPED_L3_CHECKS`), so that startup can choose the
/// text when they were required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum L3Skip {
    /// Core's: the scratch view and the tip outgrew the coins share (`validation.cpp:5119-5136`).
    Dbcache,
    /// DEC-993 (BL-440): a block this build cannot judge, with the readiness refusal's text (CH3
    /// §2.17, ALLOWED-DIFFS D18): at level 3 until CH5-1b every evo block (VerifyDB's G1 gate),
    /// and any refusal a level-4 reconnect meets. Core verifies it; the node neither passes the
    /// block nor calls it corrupt.
    NotReady(String),
}

/// VerifyDB's arguments as it receives them: Core passes the `int64_t` options to `int`
/// parameters (`node/chainstate.cpp:439-440`, `validation.h:401-402`), which keeps their low 32
/// bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VerifyDbArgs {
    pub level: i32,
    pub depth: i32,
}

impl VerifyDbArgs {
    pub fn new(options: &VerifyLoadOptions) -> VerifyDbArgs {
        VerifyDbArgs {
            level: core_int(options.check_level),
            depth: core_int(options.check_blocks),
        }
    }

    /// The level VerifyDB runs at: clamped to 0..=4 (`validation.cpp:5053`).
    pub fn level(&self) -> i32 {
        self.level.clamp(0, 4)
    }

    /// The blocks VerifyDB checks below a tip at `height`: all of them for a depth of 0 or
    /// less, or above the height (`validation.cpp:5050-5052`).
    pub fn depth(&self, height: u32) -> i32 {
        let height = i32::try_from(height).expect("heights fit Core's int");
        if self.depth <= 0 || self.depth > height {
            height
        } else {
            self.depth
        }
    }
}

impl V {
    /// Core `VerifyLoadedChainstate` for the active chainstate (`node/chainstate.cpp:412-479`),
    /// after init's pruned-datadir warning (`init.cpp:2102-2105`), at the network's retention.
    pub(super) fn verify_loaded(
        &mut self,
        options: &VerifyLoadOptions,
    ) -> Result<Result<(), VerifyLoadError>, Fatal> {
        if !self.verify_loadable {
            return Err(Fatal::new(VERIFY_LOADED_MISUSE));
        }
        self.verify_loadable = false;
        // Core's `assert` after `LoadChainTip` put the tip at the coins' best block (`cs:272-277`):
        // non-empty coins have a tip. That the two agree V checks with the stored-index load
        // (CH3a a10, O2b-3b).
        debug_assert!(options.coins_empty || self.tip().is_some(), "cs:277 assert");
        let keep = self.params.min_blocks_to_keep;
        if self.have_pruned && options.check_blocks > i64::from(keep) {
            tracing::warn!(
                "pruned datadir may not have more than {keep} blocks; only checking available blocks"
            );
        }
        if options.coins_empty {
            // `:471-474`: one chainstate, so the active one.
            if !options.evodb_empty {
                return Ok(Err(VerifyLoadError::Failure(EVODB_WITHOUT_BLOCKS)));
            }
            return Ok(Ok(()));
        }
        // `:423-428`
        if let Some(tip) = self.tip()
            && i64::from(entry(&self.reader, tip).header.time())
                > self.now_secs() + MAX_FUTURE_BLOCK_TIME
        {
            return Ok(Err(VerifyLoadError::Failure(FUTURE_TIP)));
        }
        // `:441-454`
        match self.verify_db(VerifyDbArgs::new(options), ScratchParent::Db)? {
            VerifyDbResult::Success | VerifyDbResult::SkippedMissingBlocks => {}
            VerifyDbResult::Interrupted => {
                return Ok(Err(VerifyLoadError::Interrupted(VERIFY_INTERRUPTED)));
            }
            VerifyDbResult::CorruptedBlockDb => {
                return Ok(Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB)));
            }
            VerifyDbResult::SkippedL3Checks(skip) => {
                if options.require_full_verification {
                    return Ok(Err(match skip {
                        L3Skip::Dbcache => {
                            VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE)
                        }
                        L3Skip::NotReady(refusal) => VerifyLoadError::NotReady(refusal),
                    }));
                }
            }
        }
        // `:463-465`
        if options.check_level >= 3
            && self.reset_failure_flags_null()?
            && let Some(tip) = self.tip()
            && self.idx.is_candidate(tip)
        {
            // D64: a loaded failed tip the reset brings back can outrank every tied block
            // DEC-196 (2) kept below it (D36), a set Core's `LoadChainTip` never leaves; Core's
            // prune (`v:3584-3593`) drops them, or startup's ABC, which moves nowhere, would not.
            // A ChainLock-conflicting tip keeps its mark (the reset does not ignore ChainLocks,
            // `v:4196-4200`) and stays out of the set: its fallback stays (DEC-272 amended).
            self.idx.prune_block_index_candidates(&self.reader, tip);
        }
        Ok(Ok(()))
    }

    // Core: validation.cpp:5041-5188 CVerifyDB::VerifyDB
    /// Core `CVerifyDB::VerifyDB`, the load path's and `verifychain`'s
    /// ([`ChainCommand::VerifyDb`]): the last `depth` blocks of the active chain, from the tip
    /// back, read (level 0), `CheckBlock`ed (1), their undo data read with its checksum (2) and
    /// disconnected over a scratch view (3) while it and the tip fit the coins share (DEC-981a),
    /// then reconnected from the last block the walk reached (4). The scratch view's coins read
    /// the database below them at startup and the tip on the `verifychain` path (`parent`); it
    /// and its evo transaction are dropped at the end (CH5-1a). A block this build cannot judge
    /// at level 3 or 4 skips level 3 ([`L3Skip::NotReady`], DEC-993), never a corruption. The first
    /// skip, of either cause, ends level 3 for the walk: the scratch view stays at the block it
    /// was not disconnected over, and levels 0 to 2 go on.
    ///
    /// [`ChainCommand::VerifyDb`]: super::ChainCommand::VerifyDb
    pub(super) fn verify_db(
        &mut self,
        args: VerifyDbArgs,
        parent: ScratchParent,
    ) -> Result<VerifyDbResult, Fatal> {
        use dash_chain::Status;

        #[cfg(test)]
        if let Some(result) = self.verify_db_result.clone() {
            return Ok(result);
        }
        // v:5050-5052
        let Some(tip) = self.tip() else {
            return Ok(VerifyDbResult::Success);
        };
        let tip_height = entry(&self.reader, tip).height;
        if tip_height == 0 {
            return Ok(VerifyDbResult::Success);
        }
        // v:5055 `ScopedBLSLegacyScheme`: the connector's scheme is per attempt (H1), and a
        // scratch attempt commits none (CH3 D36), so nothing is left to restore.
        // v:5061-5065
        let (depth, level) = (args.depth(tip_height), args.level());
        let tip_height = i32::try_from(tip_height).expect("heights fit Core's int");
        tracing::info!("Verifying last {depth} blocks at level {level}");
        // v:5058 and v:5066: the evo transaction and the scratch coins, both dropped unflushed.
        let mut scratch = if level >= 3 {
            self.connector.verify_scratch(parent)?
        } else {
            None
        };
        let mut failure: Option<i32> = None;
        let mut n_good: usize = 0;
        let mut report_done = 0;
        let mut skipped_no_block_data = false;
        let mut l3_skip: Option<L3Skip> = None;
        tracing::info!("Verification progress: 0%");
        let mut at = tip;
        // v:5080-5139: down to, not including, genesis.
        loop {
            let e = entry(&self.reader, at);
            let Some(prev) = e.prev else {
                break;
            };
            let height = i32::try_from(e.height).expect("heights fit Core's int");
            // v:5081-5087
            let scale = if level >= 4 {
                50.0
            } else {
                100.0
            };
            let done = (f64::from(tip_height - height) / f64::from(depth) * scale) as i32;
            let percentage_done = done.clamp(1, 99);
            if report_done < percentage_done / 10 {
                tracing::info!("Verification progress: {percentage_done}%");
                report_done = percentage_done / 10;
            }
            if height <= tip_height - depth {
                break;
            }
            // v:5091-5097: `IsPruneMode()`, which `m_have_pruned` implies while `-prune` is 0
            // (D31); no snapshot chainstate exists.
            if self.have_pruned && !e.status.has(Status::HAVE_DATA) {
                tracing::info!(
                    "VerifyDB(): block verification stopping at height {height} (no data). This \
                     could be due to pruning or use of an assumeutxo snapshot."
                );
                skipped_no_block_data = true;
                break;
            }
            // v:5098-5102 check level 0: read from disk
            let Some(block) = self.verify_read(at, height, &e.hash) else {
                return Ok(VerifyDbResult::CorruptedBlockDb);
            };
            // v:5103-5107 check level 1: verify block validity
            if level >= 1
                && let Err(state) = self.checks.check_block(&block, true, true)
            {
                tracing::error!(
                    "Verification error: found bad block at {height}, hash={} ({state})",
                    e.hash
                );
                return Ok(VerifyDbResult::CorruptedBlockDb);
            }
            // v:5108-5116 check level 2: verify undo validity
            if level >= 2
                && let Some(pos) = self.reader.undo_pos(at).filter(|pos| pos.file >= 0)
                && let Err(line) = self.files.read_undo(pos, &e.header.prev_hash())
            {
                tracing::error!("{line}");
                tracing::error!(
                    "Verification error: found bad undo data at {height}, hash={}",
                    e.hash
                );
                return Ok(VerifyDbResult::CorruptedBlockDb);
            }
            // v:5117-5137 check level 3: check for inconsistencies during memory-only
            // disconnect of tip blocks
            if l3_skip.is_none()
                && let Some(view) = scratch.as_deref_mut()
            {
                if view.fits()? {
                    let res = self.scratch_attempt(false, at, |connector, cx, v, _| {
                        connector.disconnect_block(cx, v, ViewTarget::Scratch(view), &block, at)
                    })?;
                    if let Some(refusal) = self.connector.take_scratch_refusal() {
                        l3_skip = Some(L3Skip::NotReady(refusal));
                    } else {
                        match res {
                            DisconnectResult::Failed => {
                                tracing::error!(
                                    "Verification error: irrecoverable inconsistency in block \
                                     data at {height}, hash={}",
                                    e.hash
                                );
                                return Ok(VerifyDbResult::CorruptedBlockDb);
                            }
                            DisconnectResult::Unclean => {
                                n_good = 0;
                                failure = Some(height);
                            }
                            DisconnectResult::Ok => n_good += block.txs().len(),
                        }
                    }
                } else {
                    l3_skip = Some(L3Skip::Dbcache);
                }
            }
            // v:5138
            if self.shutdown_requested() {
                return Ok(VerifyDbResult::Interrupted);
            }
            at = prev;
        }
        // v:5140-5143: an unclean disconnect ends here, before level 4 (r12).
        if let Some(failure) = failure {
            tracing::error!(
                "Verification error: coin database inconsistencies found (last {} blocks, {n_good} \
                 good transactions before that)",
                tip_height - failure + 1
            );
            return Ok(VerifyDbResult::CorruptedBlockDb);
        }
        // v:5144-5146; the evo skip's reason is the node's own line (CH5 D3).
        if let Some(skip) = &l3_skip {
            log_l3_skip(skip);
        }
        // v:5148-5149
        let block_count = tip_height - i32::try_from(entry(&self.reader, at).height).unwrap();
        // v:5151-5177 check level 4: try reconnecting blocks, from the walk's last block, the
        // predecessor of the range it disconnected (r12).
        if level >= 4
            && l3_skip.is_none()
            && let Some(view) = scratch.as_deref_mut()
        {
            while at != tip {
                let height = i32::try_from(entry(&self.reader, at).height).unwrap();
                let done = (f64::from(tip_height - height) / f64::from(depth) * 50.0) as i32;
                let percentage_done = (100 - done).clamp(1, 99);
                if report_done < percentage_done / 10 {
                    tracing::info!("Verification progress: {percentage_done}%");
                    report_done = percentage_done / 10;
                }
                // `m_chain.Next(pindex)`
                at = self
                    .chain
                    .ancestor_private(entry(&self.reader, at).height + 1)
                    .expect("a block below the tip has a successor");
                let e = entry(&self.reader, at);
                let height = height + 1;
                let Some(block) = self.verify_read(at, height, &e.hash) else {
                    return Ok(VerifyDbResult::CorruptedBlockDb);
                };
                // Not `fJustCheck`: the tip connect's checks, undo and status writes (R3).
                let out = self.scratch_attempt(true, at, |connector, cx, v, st| {
                    connector.connect_block(cx, v, ViewTarget::Scratch(view), &block, false, at, st)
                })?;
                // A readiness refusal here (any cause: the walk's G1 gate keeps evo blocks out)
                // ends as the same skip.
                if let Some(refusal) = self.connector.take_scratch_refusal() {
                    let skip = L3Skip::NotReady(refusal);
                    log_l3_skip(&skip);
                    return Ok(VerifyDbResult::SkippedL3Checks(skip));
                }
                let state = match out {
                    ConnectOutcome::Connected => None,
                    ConnectOutcome::Invalid(err) => Some(err.to_string()),
                    ConnectOutcome::SystemError(msg) => Some(msg),
                };
                if let Some(state) = state {
                    tracing::error!(
                        "Verification error: found unconnectable block at {height}, hash={} \
                         ({state})",
                        e.hash
                    );
                    return Ok(VerifyDbResult::CorruptedBlockDb);
                }
                if self.shutdown_requested() {
                    return Ok(VerifyDbResult::Interrupted);
                }
            }
        }
        // v:5179
        tracing::info!(
            "Verification: No coin database inconsistencies in last {block_count} blocks \
             ({n_good} transactions)"
        );
        // v:5181-5187
        Ok(if let Some(skip) = l3_skip {
            VerifyDbResult::SkippedL3Checks(skip)
        } else if skipped_no_block_data {
            VerifyDbResult::SkippedMissingBlocks
        } else {
            VerifyDbResult::Success
        })
    }

    // Core: validation.cpp:5098-5102, :5165-5169
    /// `ReadBlockFromDisk(block, pindex)` for VerifyDB, with Core's lines when it fails.
    fn verify_read(&self, at: BlockId, height: i32, hash: &BlockHash) -> Option<Arc<Block>> {
        // `GetBlockPos()`: null (`nFile=-1, nPos=0`) without `HAVE_DATA` (chain.h).
        let pos = self.reader.have_data_pos(at).unwrap_or(FlatFilePos {
            file: -1,
            pos: 0,
        });
        match self.files.read_block(pos, hash) {
            Ok(block) => Some(block),
            Err(line) => {
                tracing::error!("{line}");
                tracing::error!(
                    "Verification error: ReadBlockFromDisk failed at {height}, hash={hash}"
                );
                None
            }
        }
    }

    /// `f` over the connector and its context for one of VerifyDB's scratch attempts on `id`:
    /// V's chain lent to the status writer and the ChainLock view, as for a proposal. A connect
    /// evaluates `IsInitialBlockDownload()` (`v:2807`); a disconnect evaluates nothing and reads
    /// the latch.
    fn scratch_attempt<R>(
        &mut self,
        connect: bool,
        id: BlockId,
        f: impl FnOnce(
            &mut dyn BlockConnector,
            &ConnectCx<'_>,
            &mut VContext,
            &mut BlockStatusWriter<'_>,
        ) -> R,
    ) -> R {
        let is_ibd = if connect {
            self.is_ibd()
        } else {
            self.chain.is_ibd_private(self.now(), true)
        };
        let chain = RefCell::new(&mut self.chain);
        let best_header = chain.borrow().best_header();
        let deployments = self.deployments.view();
        let chainlocks = HeldChainLocks::new(&chain, self.cl.sigs.best_height());
        let cx = ConnectCx {
            params: &self.params,
            reader: &self.reader,
            deployments: &deployments,
            best_header,
            min_chain_work: self.reader.min_chain_work(),
            is_ibd,
            chainlocks: &chainlocks,
            guard: self.guard,
            origin: AttemptOrigin::VerifyDb,
        };
        let mut st = BlockStatusWriter::new(&chain, id);
        f(&mut *self.connector, &cx, &mut self.vcx, &mut st)
    }

    /// Core `ResetBlockFailureFlags(nullptr)` (`validation.cpp:4155-4163`; CH1-node §4.6): the
    /// best known invalid block, when it descends from the tip, reset as `reconsiderblock` resets
    /// it. Core's test compares its ancestor at the tip's height with the tip, so with no tip
    /// (`GetAncestor(-1)` and `Tip()` both null) it resets too. Whether it reset.
    pub(super) fn reset_failure_flags_null(&mut self) -> Result<bool, Fatal> {
        let Some(best_invalid) = self.idx.best_invalid() else {
            return Ok(false);
        };
        let ahead = match self.tip() {
            Some(tip) => {
                self.reader.ancestor(best_invalid, entry(&self.reader, tip).height) == Some(tip)
            }
            None => true,
        };
        if !ahead {
            return Ok(false);
        }
        tracing::info!(
            "ResetBlockFailureFlags: the best known invalid block ({}) is ahead of our tip, \
             reconsidering",
            entry(&self.reader, best_invalid).hash
        );
        self.reconsider_block(best_invalid, false)?;
        Ok(true)
    }
}

// Core: validation.cpp:5144-5146
/// The level-3 skip's line: Core's for a memory skip; for an evo skip the readiness refusal as
/// its reason (CH5 D3), which `verifychain`'s false points to ("check debug.log for reason").
fn log_l3_skip(skip: &L3Skip) {
    match skip {
        L3Skip::Dbcache => tracing::info!(
            "Skipped verification of level >=3 (insufficient database cache size). Consider \
             increasing -dbcache."
        ),
        L3Skip::NotReady(refusal) => tracing::info!("Skipped verification of level >=3: {refusal}"),
    }
}
