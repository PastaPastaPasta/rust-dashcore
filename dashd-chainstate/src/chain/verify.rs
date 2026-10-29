//! Core's `VerifyLoadedChainstate` on V (CH1-node-LR, O2b D32): the startup step after the
//! chainstate loads and before init's `ActivateBestChain`, with the VerifyDB point (CH5 fills
//! it) and the load-time `ResetBlockFailureFlags(nullptr)`.
//!
//! Core runs it in step 7 under `cs_main` (`init.cpp:2101-2108`, `node/chainstate.cpp:412-479`),
//! before step 11's `ThreadImport` sends the first `ActivateBestChain`. On V it is the class M
//! command [`ChainCommand::VerifyLoaded`](super::ChainCommand::VerifyLoaded), exempt from D38's
//! hold as the startup ABC it precedes is, so the order stays Core's.

use super::actor::{V, VERIFY_LOADED_MISUSE, core_int};
use super::index::entry;
use super::seams::Fatal;
use dash_consensus::header::MAX_FUTURE_BLOCK_TIME;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyLoadError {
    /// `ChainstateLoadStatus::FAILURE`: init offers a reindex (`init.cpp:2119-2133`).
    Failure(&'static str),
    /// `INTERRUPTED`: a shutdown was requested.
    Interrupted(&'static str),
    /// `FAILURE_INSUFFICIENT_DBCACHE`: a plain `InitError` (`init.cpp:2115-2117`).
    InsufficientDbcache(&'static str),
}

/// `CVerifyDB::VerifyDB`'s result (`validation.h:383-389`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyDbResult {
    Success,
    CorruptedBlockDb,
    Interrupted,
    SkippedL3Checks,
    SkippedMissingBlocks,
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
    /// after init's pruned-datadir warning (`init.cpp:2102-2105`).
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
        if self.have_pruned && options.check_blocks > i64::from(super::MIN_BLOCKS_TO_KEEP) {
            tracing::warn!(
                "pruned datadir may not have more than {} blocks; only checking available blocks",
                super::MIN_BLOCKS_TO_KEEP
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
            && i64::from(entry(&self.reader, tip).header.time)
                > self.now_secs() + MAX_FUTURE_BLOCK_TIME
        {
            return Ok(Err(VerifyLoadError::Failure(FUTURE_TIP)));
        }
        // `:441-454`
        match self.verify_db(VerifyDbArgs::new(options))? {
            VerifyDbResult::Success | VerifyDbResult::SkippedMissingBlocks => {}
            VerifyDbResult::Interrupted => {
                return Ok(Err(VerifyLoadError::Interrupted(VERIFY_INTERRUPTED)));
            }
            VerifyDbResult::CorruptedBlockDb => {
                return Ok(Err(VerifyLoadError::Failure(CORRUPTED_BLOCK_DB)));
            }
            VerifyDbResult::SkippedL3Checks => {
                if options.require_full_verification {
                    return Ok(Err(VerifyLoadError::InsufficientDbcache(INSUFFICIENT_DBCACHE)));
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

    /// The VerifyDB point: CH5's `CVerifyDB::VerifyDB` goes here. Until then it verifies
    /// nothing, as Core's does below a tip at height 1 (`validation.cpp:5039-5041`).
    fn verify_db(&mut self, _args: VerifyDbArgs) -> Result<VerifyDbResult, Fatal> {
        #[cfg(test)]
        if let Some(result) = self.verify_db_result {
            return Ok(result);
        }
        Ok(VerifyDbResult::Success)
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
