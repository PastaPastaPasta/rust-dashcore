//! V's seams (CH1-node §8.1): what V calls for work other rows own.
//!
//! - [`BlockChecks`]: CS3's `CheckBlock` and `ContextualCheckBlock`; [`CoreChecks`] runs
//!   `dash-consensus`'s ports.
//! - [`BlockFiles`]: ST2's block files (`SaveBlockToDisk`, `ReadBlockFromDisk`);
//!   [`MemBlockFiles`] until CH2b wires the flat files.
//! - [`BlockConnector`]: CH3a's `ConnectBlock`/`DisconnectBlock`/`TestBlockValidity` (CH7a adds
//!   the evo half) in a [`ConnectCx`], with V's [`VContext`] and a [`ViewTarget`] (CH3 §2.1, D3);
//!   [`NullConnector`] until then.
//! - [`MempoolSink`]: MP1's mempool calls from the tip steps; [`NullMempool`] until then.
//! - [`FlushSite`](super::flush::FlushSite): CH2b's `FlushStateToDisk` call sites, in
//!   [`super::flush`].
//! - [`AbortSink`]: Core's `AbortNode` (ND1's), the store's sink (SB10 Q4); [`RecordingAbort`]
//!   for tests. [`AbortPath`] is the sink a connector reports through (CH3 §2.1.1, D52).
//! - [`ShutdownFlag`]: Core's `StartShutdown`/`ShutdownRequested` (ND1's `ShutdownState`);
//!   [`LocalShutdown`] for tests.
//! - [`UiNotifications`]: `uiInterface` (ND1's), fired after publication (N1).
//! - [`IsConflicts`]: InstantSend's half of `ResolveBlockConflicts` (IX/LK1b's, D42);
//!   [`NoInstantSend`] until then.

use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dash_chain::{ChainReader, FlatFilePos, HeaderChain, Status, Work};
use dash_consensus::coins::BlockUndo;
use dash_consensus::ctx::{CheckpointView, Deployment, DeploymentView, bls_ctx_for_block};
use dash_consensus::header::HeaderParams;
use dash_consensus::{
    Block, BlockId, BlockValidationError, BlsCtx, ChainAncestry, ConsensusCtx, TxRef,
};
use dash_dml::blockprocessor::BlockLookup;
use dash_dml::creditpool::{CreditPool, CreditPoolInfo, PoolError, PoolState};
use dash_dml::mnhf::{EhfSignals, GetForBlockError};
use dash_dml::{MnList, StoreError};
use dash_evo::llmq::{LlmqCtx, LlmqError, LlmqSource, MinedCommitmentReader};
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_params::{ChainSetup, Network};
use dash_primitives::{BlockHash, QuorumHash};
use dashd_store::{ErrorKind, KvEngine};

use super::actor::VContext;
use super::command::OnVMembersError;
use super::flush::AbortCause;
use super::locks::ChainLockSig;
use crate::coins::{BlockOverlay, CoinsCache};
use crate::store::sites::VALIDATION_SITE;
use crate::store::{Failure, GlobalFailStop};

/// A failure Core answers with `AbortNode`, or one of Core's `assert`s on V (D62a): V stops, and
/// the node shuts down. Two compare equal when their messages do: who reports one, and which of
/// Core's ends it is, are not part of what failed.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{0}")]
pub struct Fatal(pub String, Report, Class);

/// Who reports a [`Fatal`]'s `AbortNode` when V stops on it (CH3 §2.1.1, D52; CH3a D60). V's
/// `stop_serving` asks [`Fatal::reported_by_v`] once its flush side has tripped the latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Report {
    /// V, unless the latch had tripped: whoever tripped it reported the failure (L-3).
    UnlessLatched,
    /// Nobody more: the connector reported it through its [`AbortPath`] (a
    /// [`ConnectOutcome::SystemError`]), or a store did, as it tripped the latch.
    Done,
    /// Nobody more, as `Done`, and a flush-side failure Core's next flush meets too (BL-407,
    /// BL-482): Core's `FlushStateToDisk` returns `false` after its `AbortNode(state, …)`. Only
    /// the flush side's funnel makes one ([`Fatal::flush_failed`]); `ActivateBestChainStep`'s
    /// disconnect turns it into `Then` ([`Fatal::then_abort_node`]).
    Flush,
    /// Nobody more for this failure, and then V's stop reports Core's next `AbortNode`, with this
    /// text, latched or not: `ActivateBestChainStep`'s "Failed to disconnect block"
    /// (`validation.cpp:3627`) after the flush's inside `DisconnectTip` (`:3310-3312`, BL-559).
    /// The [`Fatal`]'s own text stays the flush's, as Core's `state` keeps its first reason
    /// (`consensus/validation.h:92-97`).
    Then(&'static str),
    /// V, latched or not: an `AbortNode` Core raises after one already reported, such as
    /// `ActivateBestChainStep`'s "Failed to disconnect block" (`validation.cpp:3627`) after
    /// `DisconnectBlock`'s own (`:2296`).
    Always,
}

/// Which of Core's two ends a [`Fatal`] is (D62a), and whether V carries on past it at a release
/// point (CH3b D213).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    AbortNode,
    /// An `AbortNode` to Core's callers and V's flush side from SB4's contract (`ChainError`): V's
    /// own, never carried on past, as SB4 publishes no snapshot after one and has its host stop
    /// (`dash-chain/src/chain.rs:1226-1231`).
    Contract,
    Assert,
}

impl PartialEq for Fatal {
    fn eq(&self, other: &Fatal) -> bool {
        self.0 == other.0
    }
}

impl Eq for Fatal {}

impl Fatal {
    /// A failure V reports, unless the latch shows another report came first.
    pub fn new(msg: impl Into<String>) -> Fatal {
        Fatal(msg.into(), Report::UnlessLatched, Class::AbortNode)
    }

    /// A failure already reported: a connector's system error, or a store's.
    pub fn reported(msg: impl Into<String>) -> Fatal {
        Fatal(msg.into(), Report::Done, Class::AbortNode)
    }

    /// A flush-side failure already reported, which Core's next flush meets too: Core's
    /// `FlushStateToDisk` `false` (`validation.cpp:3122-3123`, `:3056-3057`, BL-559).
    pub(crate) fn flush_failed(msg: impl Into<String>) -> Fatal {
        Fatal(msg.into(), Report::Flush, Class::AbortNode)
    }

    /// This failure as `ActivateBestChainStep` meets it from `DisconnectTip` (BL-559): a
    /// [`Fatal::flush_failed`] failure becomes [`Report::Then`] with `msg`, which V's stop reports
    /// after it; any other failure is returned as it is.
    pub(crate) fn then_abort_node(self, msg: &'static str) -> Fatal {
        match self.1 {
            Report::Flush => Fatal(self.0, Report::Then(msg), self.2),
            Report::UnlessLatched | Report::Done | Report::Then(_) | Report::Always => self,
        }
    }

    /// An `AbortNode` of V's own that Core raises whatever was reported before it.
    pub fn abort_node(msg: impl Into<String>) -> Fatal {
        Fatal(msg.into(), Report::Always, Class::AbortNode)
    }

    /// One of Core's `assert`s, which ends the process with no flush (D62a): V stops as on its
    /// panic, the store latch tripped, but reports and returns instead of unwinding. V reports
    /// it unless the latch had tripped, as a panic.
    pub fn assert(msg: impl Into<String>) -> Fatal {
        Fatal(msg.into(), Report::UnlessLatched, Class::Assert)
    }

    /// What V's flush side hears when V stops on it (D62, D62a).
    pub(crate) fn cause(&self) -> AbortCause {
        match self.2 {
            Class::AbortNode | Class::Contract => AbortCause::AbortNode,
            Class::Assert => AbortCause::Panic,
        }
    }

    /// Whether this is an `AbortNode`, whose `false` Core's callers of `ActivateBestChain` log;
    /// one of Core's asserts ends the process first (CH3b D212).
    pub(crate) fn is_abort_node(&self) -> bool {
        self.2 != Class::Assert
    }

    /// Whether V carries on past this failure of a command it ran at a release point inside a
    /// class C operation, as Core's thread does past another's `AbortNode` (CH3b D213): not past
    /// one of Core's asserts, nor past an SB4 contract failure.
    pub(crate) fn carried_past(&self) -> bool {
        self.2 == Class::AbortNode
    }

    /// Whether this is an SB4 contract failure, whose state no shutdown flush writes (BL-271).
    pub(crate) fn is_contract(&self) -> bool {
        self.2 == Class::Contract
    }

    /// This failure once V has stopped on it where it arose (CH3b D213): the stop in V's `run`
    /// reports it no more.
    pub(crate) fn into_reported(self) -> Fatal {
        Fatal(self.0, Report::Done, self.2)
    }

    /// This failure once whoever latched it has reported nothing, as the members facade's trip
    /// (D4): V reports it whatever the latch shows, unless it was reported already.
    pub(crate) fn latched_unreported(self) -> Fatal {
        match self.1 {
            Report::UnlessLatched => Fatal(self.0, Report::Always, self.2),
            Report::Done | Report::Flush | Report::Then(_) | Report::Always => self,
        }
    }

    /// Whether this failure was reported already: by the store that tripped the latch with it,
    /// by a connector's abort path, or at V's stop where it arose. Nobody reports it again.
    pub(crate) fn is_reported(&self) -> bool {
        self.1 == Report::Done
    }

    /// The text V's stop reports for this failure, if it reports one, `latched` saying whether
    /// the latch had tripped before V's stop: the failure's own, or for a failure Core's next
    /// `AbortNode` follows (`Fatal::then_abort_node`), that one's.
    pub fn reported_by_v(&self, latched: bool) -> Option<&str> {
        match self.1 {
            Report::UnlessLatched => (!latched).then_some(self.0.as_str()),
            Report::Done | Report::Flush => None,
            Report::Always => Some(&self.0),
            Report::Then(msg) => Some(msg),
        }
    }
}

impl From<dash_chain::ChainError> for Fatal {
    fn from(e: dash_chain::ChainError) -> Fatal {
        Fatal(format!("chain: {e}"), Report::UnlessLatched, Class::Contract)
    }
}

/// Core's `AbortNode` (`validation.cpp`): logs, sets the user message and requests shutdown.
/// ND1 implements it; V calls it once per stop, with Core's user message where Core passes one
/// (a refusal's text, "Disk space is too low!") and none otherwise (Core's default), and then
/// stops serving. One trait with the stores' sink (`dash_kv`'s, SB10 Q4).
pub use crate::store::AbortSink;

// Core: shutdown.cpp:66-105
/// Core's `StartShutdown()` and `ShutdownRequested()`: the node's one shutdown flag (D52), ND1's
/// `ShutdownState`. V sets it for `-stopatheight` and a listener's panic, and reads it where
/// Core reads `ShutdownRequested()` on V. An `AbortNode` sets it through the [`AbortSink`], as
/// Core's `AbortNode` calls `StartShutdown()`. V's own stops (a `Shutdown` command, an init
/// error) do not set it.
pub trait ShutdownFlag: Send + Sync {
    fn start_shutdown(&self);
    fn shutdown_requested(&self) -> bool;
}

/// A [`ShutdownFlag`] of its own: tests, and callers without the node's.
#[derive(Debug, Default)]
pub struct LocalShutdown(AtomicBool);

impl ShutdownFlag for LocalShutdown {
    fn start_shutdown(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn shutdown_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// An [`AbortSink`] that records the messages (tests, and the node until ND1).
#[derive(Debug, Default)]
pub struct RecordingAbort(Mutex<Vec<(String, Option<String>)>>);

impl RecordingAbort {
    /// The log messages, in order.
    pub fn messages(&self) -> Vec<String> {
        self.reports().into_iter().map(|(log, _)| log).collect()
    }

    /// Each report's log message and `AbortNode`'s user message, in order.
    pub fn reports(&self) -> Vec<(String, Option<String>)> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn record(&self, msg: String, user: Option<&str>) {
        tracing::error!("*** {msg}");
        let report = (msg, user.map(str::to_owned));
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(report);
    }
}

impl AbortSink for RecordingAbort {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        self.record(log_msg.to_owned(), user_msg);
    }

    fn read_failed(&self, what: &str) {
        self.record(format!("Error reading from database: {what}"), None);
    }
}

// Core: shutdown.cpp:27-37 AbortNode; validation.cpp:2245-2249 AbortNode(state, ...), as
// ConnectBlock, DisconnectBlock and WriteUndoDataForBlock call it
/// V's abort path as a connector reports through it (CH3 §2.1.1, D52). Each
/// [`abort_node`](AbortSink::abort_node) trips the node latch if it has not tripped, then logs
/// Core's text through the node's sink: once per call, whoever tripped the latch. V turns a
/// connector's `SystemError` into [`Fatal::reported`], which its stop never reports again,
/// whatever its flush side knows of the latch; a V-raised `AbortNode` after a reported one
/// (`ActivateBestChainStep`'s "Failed to disconnect block", `validation.cpp:3627`, Core's second
/// report on that path) is [`Fatal::abort_node`], which it always reports (CH3a D60). After a
/// failed flush inside `DisconnectTip` the same report follows the flush's, carried by
/// `Fatal::then_abort_node` (BL-559).
#[derive(Clone)]
pub struct AbortPath {
    sink: Arc<dyn AbortSink>,
    latch: GlobalFailStop,
}

impl AbortPath {
    /// The path over the node's `sink` (ND1's `NodeAbort`) and its one latch (the stores' and the
    /// flush side's).
    pub fn new(sink: Arc<dyn AbortSink>, latch: GlobalFailStop) -> AbortPath {
        AbortPath {
            sink,
            latch,
        }
    }

    /// The node latch the path trips.
    pub fn latch(&self) -> &GlobalFailStop {
        &self.latch
    }

    /// Core's `AbortNode` through the node's sink alone, for a caller whose own trip of the latch
    /// won: an EV attempt's guard with no tip hold (CH3a D71, BL-601). It trips nothing.
    pub(crate) fn report_tripped(&self, log_msg: &str, user_msg: Option<&str>) {
        self.sink.abort_node(log_msg, user_msg);
    }
}

impl AbortSink for AbortPath {
    fn abort_node(&self, log_msg: &str, user_msg: Option<&str>) {
        // Before the sink, as V's own stop (D45): from here nothing more is written. A store that
        // tripped it first (`GuardedFiles`' "Disk space is too low!") left the report to us; a
        // failure another thread latched first is reported before this one (BL-554).
        let latched = self.latch.trip(Failure {
            kind: ErrorKind::Other,
            site: VALIDATION_SITE,
            message: log_msg.to_owned(),
        });
        if !latched {
            self.latch.settle();
        }
        self.sink.abort_node(log_msg, user_msg);
    }

    /// The read's store has tripped the latch (the contract), and the node's sink reports it.
    fn read_failed(&self, what: &str) {
        self.sink.read_failed(what);
    }
}

/// CS3's block checks, as `AcceptBlock` and `ProcessNewBlock` call them.
pub trait BlockChecks: Send {
    /// Core `CheckBlock(block, state, params, fCheckPOW, fCheckMerkleRoot)`.
    fn check_block(
        &self,
        block: &Block,
        pow: bool,
        merkle: bool,
    ) -> Result<(), BlockValidationError>;

    /// Core `ContextualCheckBlock(block, state, chainman, pindexPrev)`; `now` is the node clock
    /// (`time-too-new`'s reference, unused by the block rules but part of the context).
    fn contextual_check_block(
        &self,
        block: &Block,
        prev: Option<BlockId>,
        chain: &HeaderChain,
        now: i64,
    ) -> Result<(), BlockValidationError>;
}

/// `dash-consensus`'s `CheckBlock` and `ContextualCheckBlock` (CS3).
pub struct CoreChecks {
    params: Arc<ChainSetup>,
    hp: HeaderParams,
}

impl CoreChecks {
    pub fn new(params: Arc<ChainSetup>) -> Result<CoreChecks, Fatal> {
        let hp = HeaderParams::from_chain_params(&params)
            .map_err(|e| Fatal::new(format!("header parameters: {e:?}")))?;
        Ok(CoreChecks {
            params,
            hp,
        })
    }
}

/// The buried deployments only: what `ContextualCheckBlock` reads (`block.rs`, `tx.rs`). A
/// versionbits query here is a bug. The connector's context has V's deployments (CH3 §2.16).
struct BuriedOnly<'a> {
    params: &'a ChainSetup,
    chain: &'a dyn ChainAncestry,
}

impl BuriedOnly<'_> {
    fn active_at_height(&self, height: u32, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => height >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(pos) => {
                debug_assert!(false, "a buried-only view reads no versionbits ({pos:?})");
                false
            }
        }
    }
}

impl DeploymentView for BuriedOnly<'_> {
    // Core: deploymentstatus.h DeploymentActiveAfter (buried: nHeight + 1 >= height)
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        let next = prev.map_or(0, |p| self.chain.header(p).height + 1);
        self.active_at_height(next, deployment)
    }

    // Core: deploymentstatus.h DeploymentActiveAt
    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_at_height(self.chain.header(block).height, deployment)
    }
}

impl BlockChecks for CoreChecks {
    // Core: validation.cpp:4372 CheckBlock
    fn check_block(
        &self,
        block: &Block,
        pow: bool,
        merkle: bool,
    ) -> Result<(), BlockValidationError> {
        dash_consensus::block::check_block(block, &self.hp, pow, merkle)
    }

    // Core: validation.cpp:4536 ContextualCheckBlock
    fn contextual_check_block(
        &self,
        block: &Block,
        prev: Option<BlockId>,
        chain: &HeaderChain,
        now: i64,
    ) -> Result<(), BlockValidationError> {
        let ancestry = chain.complete().expect("the node's chain is complete");
        let (height, mtp) = match prev {
            None => (0, 0),
            Some(p) => {
                let h = ancestry.header(p);
                (h.height + 1, h.mtp)
            }
        };
        let deployments = BuriedOnly {
            params: &self.params,
            chain: ancestry,
        };
        let ctx = ConsensusCtx {
            params: &self.params,
            height,
            prev,
            chain: ancestry,
            deployments: &deployments,
            checkpoints: chain as &dyn CheckpointView,
            bls: bls_ctx_for_block(&self.params, height),
            median_time_past: mtp,
            header_check_time: now,
        };
        dash_consensus::block::contextual_check_block(block, &ctx)
    }
}

/// ST2's block files as V uses them (Core's `BlockManager::SaveBlockToDisk` and
/// `ReadBlockFromDisk`).
pub trait BlockFiles: Send {
    /// `SaveBlockToDisk(block, nHeight, dbp)`: the block's position. `Ok(None)` is Core's null
    /// position ("Failed to find position to write new block to disk"); `Err` an I/O failure,
    /// which `AcceptBlock` turns into `AbortNode("System error: …")`. Files whose store latches a
    /// failed write report Core's `AbortNode` themselves, as V's report would follow a tripped
    /// latch, and return `Ok(None)`, or `Err` for Core's exception (CH2b b4 D62).
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String>;

    /// `ReadBlockFromDisk` at `pos`, checked against `hash`.
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String>;

    // Core: node/blockstorage.cpp:386-389, flatfile.cpp:35-46
    /// `OpenBlockFile(FlatFilePos(file, 0), true)` is not null: block file `file` opens
    /// read-only. A file that does not logs Core's `Unable to open file <path>` (BL-127).
    fn open_block_file(&self, file: i32) -> bool;

    // Core: node/blockstorage.cpp:607-616
    /// `CalculateCurrentUsage()`: the bytes every blk and rev file holds (`getblockchaininfo`).
    fn current_usage(&self) -> u64;

    // Core (375c64d2317): node/blockstorage.cpp:429-444, :635-646
    /// `ScanAndUnlinkAlreadyPrunedFiles()` with `m_have_pruned` = `have_pruned`: removes the blk
    /// and rev files of every file below the last that a prune emptied, logging each. A remove
    /// that fails is skipped. `Err` only for files that refuse writes after a failure they have
    /// reported.
    fn scan_already_pruned_files(&mut self, have_pruned: bool) -> Result<(), String>;

    // Core: node/blockstorage.cpp:539-575 UndoReadFromDisk
    /// `UndoReadFromDisk` at `pos`, its checksum over `parent`, the hash of the block's parent:
    /// the block's undo record, or the line Core logs when it cannot read one. Files that keep
    /// no undo records answer as Core's null undo position does.
    fn read_undo(&self, pos: FlatFilePos, parent: &BlockHash) -> Result<BlockUndo, String> {
        let _ = (pos, parent);
        Err("UndoReadFromDisk: no undo data available".into())
    }

    /// The files' reads for V's read-ahead (CH4-4), on another thread: `None`, the default,
    /// leaves V reading every block itself.
    fn read_ahead(&self) -> Option<Arc<dyn BlockSource>> {
        None
    }

    /// After [`save_block`](Self::save_block)'s null position: one of Core's asserts that the
    /// files' latch holds as its first failure (`Kept::Assert`), as the chain `Fatal` its reporter
    /// reported (BL-677). Core's process ended at that assert, so no `AcceptBlock` follows it.
    /// `None` for any other null position, and for files with no latch. A wrapper forwards it.
    fn refused_by_assert(&self) -> Option<Fatal> {
        None
    }
}

/// [`BlockFiles::read_block`] on the read-ahead's thread (CH4-4), without its log lines: V's own
/// read of a block the read-ahead failed logs them, as Core's one read does.
pub trait BlockSource: Send + Sync {
    /// `ReadBlockFromDisk` at `pos`, checked against `hash`.
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String>;
}

/// Block files in memory: one "file" per 1,000 blocks, positions counting up. Until CH2b wires
/// ST2's flat files (D-CH1-3).
#[derive(Default)]
pub struct MemBlockFiles {
    blocks: HashMap<FlatFilePos, Arc<Block>>,
    next: u32,
}

impl BlockFiles for MemBlockFiles {
    fn save_block(
        &mut self,
        block: &Arc<Block>,
        _height: u32,
        known: Option<FlatFilePos>,
    ) -> Result<Option<FlatFilePos>, String> {
        if let Some(pos) = known {
            return Ok(Some(pos));
        }
        let pos = FlatFilePos {
            file: (self.next / 1_000) as i32,
            // Core's positions follow the 8-byte record header; never 0 for a block.
            pos: 8 + self.next % 1_000,
        };
        self.next += 1;
        self.blocks.insert(pos, block.clone());
        Ok(Some(pos))
    }

    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        match self.blocks.get(&pos) {
            Some(b) if b.hash() == *hash => Ok(b.clone()),
            Some(_) => Err(format!("block at {pos:?} has the wrong hash")),
            None => Err(format!("no block at {pos:?}")),
        }
    }

    /// A file holds a block written to it.
    fn open_block_file(&self, file: i32) -> bool {
        self.blocks.keys().any(|p| p.file == file)
    }

    /// Each block's record: its size and the 8-byte header before it.
    fn current_usage(&self) -> u64 {
        self.blocks.values().map(|b| u64::from(b.size()) + 8).sum()
    }

    /// Nothing prunes these files.
    fn scan_already_pruned_files(&mut self, _have_pruned: bool) -> Result<(), String> {
        Ok(())
    }
}

impl MemBlockFiles {
    /// The blocks written.
    #[cfg(test)]
    pub(crate) fn stored(&self) -> usize {
        self.blocks.len()
    }
}

/// What `ConnectBlock`, `DisconnectBlock` and `TestBlockValidity` read beside the block (§8.1;
/// CH3 §2.1, D3). The caller builds it for one call; CH7a extends it.
pub struct ConnectCx<'a> {
    pub params: &'a ChainSetup,
    pub reader: &'a ChainReader,
    /// CS4's `Deployments` over V's header chain and its one versionbits cache (CH3 §2.16, D21):
    /// Core's `DeploymentActiveAfter`/`DeploymentActiveAt` with `chainman`'s cache.
    pub deployments: &'a dyn DeploymentView,
    /// Core's `m_best_header` (assumevalid, `validation.cpp:2558`).
    pub best_header: BlockId,
    /// `MinimumChainWork()` (`validation.cpp:2559`).
    pub min_chain_work: Work,
    /// V's `IsInitialBlockDownload()`: evaluated as a connect starts, Core's evaluation at
    /// `validation.cpp:2807`; a disconnect's is the latch as it stands, evaluated nowhere. Only the
    /// helper reads it (`NodeHelper`'s `ibd` and `interim_refusal`, CH3 §3.3, §2.17); connector
    /// code never does.
    pub is_ibd: bool,
    /// V's ChainLock state, for `TestBlockValidity`'s unrecorded pre-check (CH3 §2.14) and
    /// `NodeHelper`'s queries.
    pub chainlocks: &'a dyn ChainLockView,
    /// The readiness guard's scope (CH3 §2.17).
    pub guard: GuardScope,
    /// What kind of attempt this is: a digest tag only (D17). Commit follows the [`ViewTarget`].
    pub origin: AttemptOrigin,
}

/// The kind of a connector call's attempt (O8 §8.2; CH3 §2.15, D17): a digest tag only. The
/// call's [`ViewTarget`] says where its changes go; nothing infers that from the origin (D36).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AttemptOrigin {
    /// `ConnectTip` and `DisconnectTip`: ABC, invalidate, `EnforceBlock`, recovery's activation.
    Tip,
    /// `TestBlockValidity`: a block proposal or the miner's template check.
    Proposal,
    /// CH5's `VerifyDB`.
    VerifyDb,
    /// CH2b's `ReplayBlocks`.
    Replay,
}

/// The readiness guard's network scope (CH3 §2.17, D31).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuardScope {
    /// Mainnet and testnet: a live tip this build cannot judge is refused (G2).
    Live,
    /// Regtest and devnet: D18's declared regime.
    Declared,
}

impl GuardScope {
    /// The scope of `params`' network, as ND1 derives it.
    pub fn of(params: &ChainSetup) -> GuardScope {
        match params.network {
            Network::Main | Network::Testnet3 => GuardScope::Live,
            Network::Regtest | Network::Devnet => GuardScope::Declared,
        }
    }
}

/// V's ChainLock state as the connector's callers read it (CH3 §2.14, §3.3): SB4's floor and V's
/// `bestChainLock`. A read of the floor's ancestry can fail cold, which is a [`Fatal`].
pub trait ChainLockView {
    /// Core `HasChainLock(nHeight, blockHash)` (`chainlock/chainlock.cpp:49-71`).
    fn has_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal>;

    /// Core `HasConflictingChainLock(nHeight, blockHash)` (`chainlock/chainlock.cpp:73-96`).
    fn has_conflicting_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal>;

    /// Core `GetBestChainLockHeight()`: `bestChainLock.nHeight`, -1 with none.
    fn best_height(&self) -> i32;

    /// Core `GetBestChainLock()` (`chainlock/chainlock.cpp:37-41`): `bestChainLock`, `None` for
    /// the null sig.
    fn best_chainlock(&self) -> Option<Arc<ChainLockSig>>;
}

/// The connector's one borrow of V's chain inside a hold, shared by the status writer and the
/// ChainLock view. Each use borrows it for one call and returns it, so they never overlap.
pub(crate) type ChainCell<'a> = RefCell<&'a mut HeaderChain>;

/// [`ChainLockView`] inside V's hold: SB4's floor queries on the held chain, V's best lock
/// (`ClSigs::best`).
pub(crate) struct HeldChainLocks<'a> {
    chain: &'a ChainCell<'a>,
    best: Option<Arc<ChainLockSig>>,
}

impl<'a> HeldChainLocks<'a> {
    pub(crate) fn new(
        chain: &'a ChainCell<'a>,
        best: Option<Arc<ChainLockSig>>,
    ) -> HeldChainLocks<'a> {
        HeldChainLocks {
            chain,
            best,
        }
    }
}

impl ChainLockView for HeldChainLocks<'_> {
    fn has_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        Ok(self.chain.borrow().has_chainlock(height, hash)?)
    }

    fn has_conflicting_chainlock(&self, height: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        Ok(self.chain.borrow().has_conflicting_chainlock(height, hash)?)
    }

    fn best_height(&self) -> i32 {
        self.best.as_ref().map_or(-1, |s| s.height)
    }

    fn best_chainlock(&self) -> Option<Arc<ChainLockSig>> {
        self.best.clone()
    }
}

/// `TestBlockValidity`'s three pre-checks on the dummy index (`validation.cpp:4958-4973`; CH3
/// §2.14 step 3), which the connector runs between its ChainLock check and `ConnectBlock`. V
/// lends them for one call: the header check reads V's chain, which the connector does not hold.
/// Each inserts nothing and changes no state.
pub trait ProposalChecks {
    /// `ContextualCheckBlockHeader(block, state, blockman, chainman, pindexPrev)` (`:4959`):
    /// CH1-A10's `check_header_at` on `prev`. `Err` is a storage fault.
    fn contextual_check_block_header(
        &self,
        block: &Block,
        prev: BlockId,
    ) -> Result<Result<(), BlockValidationError>, Fatal>;

    /// `CheckBlock(block, state, consensus, fCheckPOW, fCheckMerkleRoot)` (`:4964`).
    fn check_block(
        &self,
        block: &Block,
        pow: bool,
        merkle: bool,
    ) -> Result<(), BlockValidationError>;

    /// `ContextualCheckBlock(block, state, chainman, pindexPrev)` (`:4969`).
    fn contextual_check_block(
        &self,
        block: &Block,
        prev: BlockId,
    ) -> Result<(), BlockValidationError>;
}

/// [`ProposalChecks`] inside V's hold: CH1-A10 on the held chain, CS3's checks as accept runs
/// them, `now` V's node clock (`NodeClock::now()`, `validation.cpp:4515`).
pub(crate) struct HeldProposalChecks<'a> {
    chain: &'a ChainCell<'a>,
    checks: &'a dyn BlockChecks,
    now: i64,
}

impl<'a> HeldProposalChecks<'a> {
    pub(crate) fn new(
        chain: &'a ChainCell<'a>,
        checks: &'a dyn BlockChecks,
        now: i64,
    ) -> HeldProposalChecks<'a> {
        HeldProposalChecks {
            chain,
            checks,
            now,
        }
    }
}

impl ProposalChecks for HeldProposalChecks<'_> {
    fn contextual_check_block_header(
        &self,
        block: &Block,
        prev: BlockId,
    ) -> Result<Result<(), BlockValidationError>, Fatal> {
        Ok(self.chain.borrow().check_header_at(prev, block.header(), self.now)?)
    }

    fn check_block(
        &self,
        block: &Block,
        pow: bool,
        merkle: bool,
    ) -> Result<(), BlockValidationError> {
        self.checks.check_block(block, pow, merkle)
    }

    fn contextual_check_block(
        &self,
        block: &Block,
        prev: BlockId,
    ) -> Result<(), BlockValidationError> {
        self.checks.contextual_check_block(block, Some(prev), &self.chain.borrow(), self.now)
    }
}

/// Where an attempt's changes go (CH3 D36). Commit follows the target, never [`AttemptOrigin`]:
/// hooks H1, H6 and H10 get the same target as the coins ([`ViewTarget::split`]).
pub enum ViewTarget<'a> {
    /// The active coins and evo state: tip connects, ABC, invalidate, recovery's activation.
    /// The prefetch is cleared for this target only.
    Active,
    /// `VerifyDB` levels 3 and 4 (CH5): into the scratch view, never into the active layer, F or
    /// the prefetch.
    Scratch(&'a mut dyn ScratchView),
}

impl ViewTarget<'_> {
    /// The target's coins and evo halves for one attempt: the scratch layer, if any, and the evo
    /// target the hooks get. A scratch view splits once per borrow (Opus r2).
    pub fn split(&mut self) -> (Option<ScratchCoins<'_>>, EvoTarget<'_>) {
        match self {
            ViewTarget::Active => (None, EvoTarget::Active),
            ViewTarget::Scratch(view) => {
                let (coins, evo) = view.split();
                (Some(coins), EvoTarget::Scratch(evo))
            }
        }
    }
}

/// A [`ViewTarget`]'s evo half (CH3 §3.1, D36), as hooks H1, H6 and H10 take it.
pub enum EvoTarget<'a> {
    /// The active evo store.
    Active,
    /// `VerifyDB`'s never-committed evo transaction (`validation.cpp:5053`).
    Scratch(&'a mut dyn EvoTxn),
}

/// `VerifyDB`'s scratch view (CH3 D36): one never-flushed coins layer and one never-committed evo
/// transaction (`validation.cpp:5053`, `:5061`). CH5 builds it with CH2b's owner; CH3a fixes only
/// what the connector calls.
pub trait ScratchView {
    /// One split borrow (Opus r2): hook H4 needs the coins and the evo transaction at once. An
    /// overlay over the coins commits into the scratch layer; the evo transaction is never
    /// committed.
    fn split(&mut self) -> (ScratchCoins<'_>, &mut dyn EvoTxn);

    // Core: validation.cpp:5119-5122
    /// Whether VerifyDB's level 3 disconnects the next block: the scratch layer and the tip within
    /// the coins share (DEC-981a, [`CoinsCache::verify_fits`]). A view with no tip to measure
    /// always fits.
    fn fits(&self) -> Result<bool, Fatal> {
        Ok(true)
    }
}

/// The evo transaction of a [`ScratchView`], as the hooks reach it.
pub trait EvoTxn {
    /// The transaction's evo layer: a copy of the active one the hooks read and commit into and
    /// nothing flushes (CH7 §6.3 step 5, [`EvoLayer::scratch`](crate::evo::EvoLayer::scratch)).
    /// `None` for a view that keeps no evo state: an evo hook given it fails closed.
    fn layer(&mut self) -> Option<&mut crate::evo::EvoLayer> {
        None
    }
}

/// A scratch view's coins layer, the [`BlockOverlay`] parent of a scratch attempt (CH3 D36), with
/// its engine erased so that [`BlockConnector`] stays object-safe: `KvEngine` is not.
pub struct ScratchCoins<'s>(&'s mut dyn Any);

impl<'s> ScratchCoins<'s> {
    pub fn new<E: KvEngine + 'static>(layer: &'s mut CoinsCache<E>) -> ScratchCoins<'s> {
        ScratchCoins(layer)
    }

    /// Whether the scratch layer is over the engine `E`.
    pub fn is<E: KvEngine + 'static>(&self) -> bool {
        self.0.is::<CoinsCache<E>>()
    }

    /// An overlay over the scratch layer for the connector's engine `E`. `None` if the view was
    /// built over another engine: a wiring error, which the connector turns into a [`Fatal`].
    pub fn overlay<E: KvEngine + 'static>(self) -> Option<BlockOverlay<'s, E>> {
        self.0.downcast_mut::<CoinsCache<E>>().map(BlockOverlay::new)
    }
}

/// `ConnectBlock`'s verdict (§8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectOutcome {
    Connected,
    /// `state.Invalid(…)`: a consensus failure.
    Invalid(BlockValidationError),
    /// `state.Error(…)`: a system error (disk, database), which the connector has reported through
    /// its [`AbortPath`] where Core calls `AbortNode` (CH3 §2.1.1), or a readiness refusal, which
    /// it reports the same way on the tip path and not in `TestBlockValidity` (CH3 §2.17).
    SystemError(String),
}

// Core: validation.h:405-409 DisconnectResult
/// `DisconnectBlock`'s result (CH3 D15).
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisconnectResult {
    /// `DISCONNECT_OK`: all good.
    Ok,
    /// `DISCONNECT_UNCLEAN`: rolled back, but the UTXO set was inconsistent with the block.
    Unclean,
    /// `DISCONNECT_FAILED`: something else went wrong.
    Failed,
}

/// The status writes `ConnectBlock` makes to its own entry (§4.8): it reaches no other entry and
/// never the tip. Each write dirties the entry, as Core's does.
pub struct BlockStatusWriter<'a> {
    chain: &'a ChainCell<'a>,
    id: BlockId,
}

impl<'a> BlockStatusWriter<'a> {
    pub(crate) fn new(chain: &'a ChainCell<'a>, id: BlockId) -> BlockStatusWriter<'a> {
        BlockStatusWriter {
            chain,
            id,
        }
    }

    /// `pindex->nFile`, `nUndoPos` and `BLOCK_HAVE_UNDO` after `WriteUndoDataForBlock`.
    pub fn set_undo(&mut self, file: i32, pos: u32) -> Result<(), Fatal> {
        let mut chain = self.chain.borrow_mut();
        let mut ix = chain.node_index()?;
        ix.modify(self.id, |g| {
            // `WriteUndoDataForBlock` sets `nUndoPos` only: the undo shares the block's `nFile`.
            debug_assert_eq!(g.file, Some(file), "the undo is in the block's file");
            g.undo_pos = pos;
            g.status = Status(g.status.bits() | Status::HAVE_UNDO);
        });
        ix.mark_dirty(self.id);
        Ok(())
    }

    /// `pindex->RaiseValidity(level)`, dirtying on a change; whether it changed.
    pub fn raise_validity(&mut self, level: u32) -> Result<bool, Fatal> {
        let mut chain = self.chain.borrow_mut();
        let mut ix = chain.node_index()?;
        let mut raised = false;
        ix.modify(self.id, |g| {
            let (s, changed) = g.status.raise_validity(level);
            g.status = s;
            raised = changed;
        });
        if raised {
            ix.mark_dirty(self.id);
        }
        Ok(raised)
    }

    /// `pindex->nTx`.
    pub fn n_tx(&self) -> u32 {
        self.chain.borrow().reader().entry(self.id).map_or(0, |e| e.n_tx)
    }
}

/// CH3a's `Chainstate::ConnectBlock`, `DisconnectBlock` and `TestBlockValidity` with their
/// coins/evo commit (CH7a's half included), as `ConnectTip`, `DisconnectTip` and their callers
/// call them (§4.4, §4.5; CH3 §2.1). `v` is V's context (EV addendum §2.6): every call is one EV
/// attempt opened from it (CH3 §3.2). `Err` is a storage fault: Core's `std::abort()`, after
/// which V stops with no verdict and no `BlockChecked` (CH3 §2.1).
pub trait BlockConnector: Send {
    /// `ConnectBlock(block, state, pindex, view)` and the view's flush and commit into `target`
    /// (`validation.cpp:3433-3454`; CH3 D36). `checked` is Core's `fChecked` memo (D42): true only
    /// for the object `ProcessNewBlock`'s `CheckBlock` passed with both checks on
    /// (`validation.cpp:4380-4381`, `:4430-4431`).
    #[expect(
        clippy::too_many_arguments,
        reason = "CH3 §2.1's seam: Core's arguments, V's context, the target and the memo"
    )]
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        checked: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal>;

    /// `DisconnectBlock(block, pindex, view)` and the commit into `target` (`:3274-3293`; D15).
    /// The active coins take only `Ok` (`DisconnectTip`, `:3287-3293`); a scratch view keeps `Ok`
    /// and `Unclean` (`VerifyDB`, `:5119-5128`); `Failed` commits nothing (CH3a D106).
    fn disconnect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        target: ViewTarget<'_>,
        block: &Block,
        id: BlockId,
    ) -> Result<DisconnectResult, Fatal>;

    /// Core `TestBlockValidity` (`validation.cpp:4929-4980`) on the tip: `prev` must be the tip.
    /// `checks` are its pre-checks on V's chain. Nothing is committed and nothing is inserted
    /// into the index (CH3 §2.14, D16). A readiness refusal is a `SystemError` nothing reports
    /// (§2.17).
    #[expect(
        clippy::too_many_arguments,
        reason = "Core's arguments, V's context and the pre-checks V lends"
    )]
    fn test_block_validity(
        &mut self,
        cx: &ConnectCx<'_>,
        v: &mut VContext,
        checks: &dyn ProposalChecks,
        block: &Block,
        prev: BlockId,
        check_pow: bool,
        check_merkle: bool,
    ) -> Result<ConnectOutcome, Fatal>;

    /// The BLS scheme V publishes as `ChainView.bls` (CH7 D9): the evo hooks', or `None` for the
    /// snapshot's.
    fn published_bls(&self) -> Option<BlsCtx> {
        None
    }

    /// The evo state V reads at its own tip (CH7 D16, D17): the evo hooks', or [`NoEvoReads`]
    /// for a connector that keeps none.
    fn evo_reads(&self) -> &dyn EvoReads {
        &NoEvoReads
    }

    /// The MNHF signals V's versionbits read (DEC-1028): the evo hooks', or `None` for a
    /// connector that keeps no evo state, whose chain holds no signal it could read.
    fn ehf_rows(&self) -> Option<Arc<dyn EhfRows>> {
        None
    }

    // Core: validation.cpp:5053, :5066
    /// VerifyDB's scratch view over `parent` (CH5-1a): a never-flushed coins layer and a
    /// never-committed evo transaction, which the level-3 and level-4 attempts take as
    /// [`ViewTarget::Scratch`]. `None` for a connector that keeps no coins: its levels 3 and 4
    /// have nothing to check.
    fn verify_scratch(
        &mut self,
        _parent: ScratchParent,
    ) -> Result<Option<Box<dyn ScratchView>>, Fatal> {
        Ok(None)
    }

    /// The readiness refusal the last scratch attempt ended in, which nothing reported (BL-440):
    /// `ConnectBlock`'s `SystemError` or `DisconnectBlock`'s `Failed` were the connector's
    /// answer for a block this build cannot judge, not Core's, and VerifyDB skips level 3 on it
    /// (DEC-993). Taking it clears it.
    fn take_scratch_refusal(&mut self) -> Option<String> {
        None
    }

    /// The fault behind the last [`disconnect_block`](Self::disconnect_block)'s `Failed`, which
    /// nobody has reported: its log's application failed, as an undo's own snapshot write that
    /// its store only logs (OD-CP-18), so the node latch has tripped. V's stop on it reports it
    /// whatever the latch shows. `ActivateBestChainStep`'s "Failed to disconnect block" is its one
    /// report on ABC's path; on every other caller's path V stops on it, and that stop is its one
    /// report (OD-CP-30, CH7 D103). A `Failed` of the disconnect's own has none. Taking it clears
    /// it.
    fn take_undo_fault(&mut self) -> Option<Fatal> {
        None
    }
}

/// What VerifyDB's scratch coins read below themselves (`node/chainstate.cpp:437`,
/// `rpc/blockchain.cpp:1440`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScratchParent {
    /// Startup's `VerifyLoadedChainstate`: the coins database, which nothing fills.
    Db,
    /// `verifychain`: the attached tip, through its fetching read, so a read leaves Core's clean
    /// entries in it.
    Tip,
}

/// Core's `CMNHFManager::GetForBlock` over V's index (CH7 §3.5.3, DEC-1028): a block's MNHF
/// signals from the manager's cache, else its row, else rebuilt from disk. Versionbits reads it
/// with its cache's lock held, so it reads no deployment state; its errors are Core's exceptions.
pub trait EhfRows: Send + Sync {
    // Core: evo/mnhftx.cpp:285-321 CMNHFManager::GetForBlock
    fn get_for_block(
        &self,
        reader: &ChainReader,
        block: BlockId,
    ) -> Result<EhfSignals, GetForBlockError>;
}

/// What V's evo reads walk: V's index and deployments, in the V turn that reads.
#[derive(Clone, Copy)]
pub struct EvoReadCx<'a> {
    pub reader: &'a ChainReader,
    pub deployments: &'a dyn DeploymentView,
}

/// What [`EvoReads::with_llmq`] lends its call: the LLMQ context and inputs at V's tip, and the
/// pinned view's mined commitments and V's index, which `GetQuorum` and `ScanQuorums` read.
#[derive(Clone, Copy)]
pub struct TipLlmq<'a, 'c> {
    pub ctx: &'a LlmqCtx<'c, StoreError>,
    pub src: &'a dyn LlmqSource<MnList, Error = StoreError>,
    /// `GetMinedCommitment` and the scans' `GetMinedCommitments*UntilBlock`.
    pub mined: &'a dyn MinedCommitmentReader<Error = StoreError>,
    /// `LookupBlockIndex`: any known header, whatever its chain.
    pub index: &'a dyn BlockLookup,
}

/// A call over the LLMQ context and inputs at V's tip ([`EvoReads::with_llmq`]), which keeps
/// its own answer.
pub type LlmqCall<'a> = dyn FnMut(TipLlmq<'_, '_>) -> Result<(), LlmqError<StoreError>> + 'a;

/// V's reads of the evo state (CH7 D16, D17), for `ChainView`'s list and pool and for
/// [`ChainQuery::evo_view`](super::ChainQuery::evo_view): each answers from the state V has
/// when it is called, so reads made in one V turn agree.
pub trait EvoReads {
    /// `call`, once, over the LLMQ context and inputs at V's tip (route B's binding, EV addendum
    /// §2.6): V's ancestry, index and deployments, `bls`, and one view of the committed layer,
    /// pinned for the call. A node without evo state has none to read.
    ///
    /// # Errors
    ///
    /// The call's; parameters outside the context's domain; or, without evo state, a readiness
    /// refusal, with the call not made.
    fn with_llmq(
        &self,
        cx: EvoReadCx<'_>,
        bls: BlsCtx,
        call: &mut LlmqCall<'_>,
    ) -> Result<(), OnVMembersError> {
        let _ = (cx, bls, call);
        Err(OnVMembersError::not_built())
    }

    /// `GetListForBlock(at)`; below DIP0003 Core's empty list.
    // Core: evo/deterministicmns.cpp:941-947 (the empty list of a null tip is the caller's)
    fn list_at(&self, cx: EvoReadCx<'_>, at: BlockId) -> Result<Arc<MnList>, StoreError>;

    /// `GetCreditPool(at)`, the pool a block after `at` spends from; `None` unless V20 is active
    /// after `at`.
    // Core: evo/creditpool.cpp:256
    fn credit_pool_after(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError>;

    /// `getcreditpoolinfo`'s reads for `at` (EV3 D24), in one call: `GetCreditPool(at)`, the empty
    /// pool unless V20 is active at `at`, and `GetBalanceAt` of the ancestor `window_blocks` below
    /// `at`, none when the chain is shorter (RP1a D60). `Ok(Err(_))` where Core's `GetCreditPool`
    /// throws; a window start the index cannot give is a store fault.
    // Core: rpc/blockchain.cpp:1802-1810, evo/creditpool.cpp:249-275
    fn credit_pool_info(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        window_blocks: i32,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError>;

    /// `HasMinedCommitment(llmqType, quorumHash, chain)` for the chain that ends at `tip`: a
    /// mined row whose block is on it.
    // Core: llmq/blockprocessor.cpp:650-672
    fn has_mined_commitment(
        &self,
        cx: EvoReadCx<'_>,
        llmq_type: LlmqType,
        quorum_hash: &QuorumHash,
        tip: BlockId,
    ) -> Result<bool, StoreError>;
}

/// The evo reads of a node with no evo state (`NoEvo`, [`NullConnector`]): it connects no block
/// past DIP0003 (CH3 §2.17), so every list is Core's empty one, no V20 pool exists and nothing is
/// mined.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEvoReads;

impl EvoReads for NoEvoReads {
    fn list_at(&self, _: EvoReadCx<'_>, _: BlockId) -> Result<Arc<MnList>, StoreError> {
        Ok(Arc::new(MnList::default_pre_dip3()))
    }

    fn credit_pool_after(
        &self,
        _: EvoReadCx<'_>,
        _: BlockId,
    ) -> Result<Option<Arc<PoolState>>, StoreError> {
        Ok(None)
    }

    /// The empty pool, and a window start balance of 0: no block past DIP0003 connects here.
    fn credit_pool_info(
        &self,
        cx: EvoReadCx<'_>,
        at: BlockId,
        window_blocks: i32,
    ) -> Result<Result<CreditPoolInfo, PoolError>, StoreError> {
        let window_start = credit_window_start(cx.reader, at, window_blocks)?
            .map(|start| (crate::chain::index::entry(cx.reader, start).height, 0));
        Ok(Ok(CreditPoolInfo {
            pool: CreditPool::default(),
            window_blocks,
            window_start,
        }))
    }

    fn has_mined_commitment(
        &self,
        _: EvoReadCx<'_>,
        _: LlmqType,
        _: &QuorumHash,
        _: BlockId,
    ) -> Result<bool, StoreError> {
        Ok(false)
    }
}

/// `at->GetAncestor(at->nHeight - window_blocks)`: none when the chain below `at` is shorter
/// than the window (a negative height). An ancestor the index cannot give (below its floor, or a
/// failed read) is a store fault, not Core's -1.
// Core: rpc/blockchain.cpp:1801-1802
pub fn credit_window_start(
    reader: &ChainReader,
    at: BlockId,
    window_blocks: i32,
) -> Result<Option<BlockId>, StoreError> {
    let height = crate::chain::index::entry(reader, at).height;
    let Ok(start) = u32::try_from(i64::from(height) - i64::from(window_blocks)) else {
        return Ok(None);
    };
    match reader.ancestor(at, start) {
        Some(id) => Ok(Some(id)),
        None => Err(StoreError::inconsistent("no credit window start in the index")),
    }
}

/// Connects every block, as a `ConnectBlock` that finds nothing wrong: it raises the entry to
/// `BLOCK_VALID_SCRIPTS` (`validation.cpp:2905-2908`), except genesis, which Core's returns
/// before any check (`:2542-2546`). Until CH3a, and for PF0. It writes no coins, so a target has
/// nothing to receive, and a proposal passes.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullConnector;

impl BlockConnector for NullConnector {
    fn connect_block(
        &mut self,
        cx: &ConnectCx<'_>,
        _: &mut VContext,
        _: ViewTarget<'_>,
        _: &Block,
        _: bool,
        id: BlockId,
        st: &mut BlockStatusWriter<'_>,
    ) -> Result<ConnectOutcome, Fatal> {
        if super::index::entry(cx.reader, id).prev.is_some() {
            st.raise_validity(Status::VALID_SCRIPTS)?;
        }
        Ok(ConnectOutcome::Connected)
    }

    fn disconnect_block(
        &mut self,
        _: &ConnectCx<'_>,
        _: &mut VContext,
        _: ViewTarget<'_>,
        _: &Block,
        _: BlockId,
    ) -> Result<DisconnectResult, Fatal> {
        Ok(DisconnectResult::Ok)
    }

    fn test_block_validity(
        &mut self,
        _: &ConnectCx<'_>,
        _: &mut VContext,
        _: &dyn ProposalChecks,
        _: &Block,
        _: BlockId,
        _: bool,
        _: bool,
    ) -> Result<ConnectOutcome, Fatal> {
        Ok(ConnectOutcome::Connected)
    }
}

// Core: kernel/disconnected_transactions.h (v24)
/// Core's `DisconnectedBlockTransactions` (CH3b's, CH3 §4.3): the disconnected blocks'
/// transactions, kept to re-add to the mempool after a reorg.
pub trait DisconnectPool: Send {
    /// `AddTransactionsFromBlock(vtx)`: what the pool's size bound evicted, which the caller
    /// removes from the mempool with their descendants.
    fn add_from_block(&mut self, vtx: &[TxRef]) -> Vec<TxRef>;

    /// `removeForBlock(vtx)`: a connected block's transactions leave the pool.
    fn remove_for_block(&mut self, vtx: &[TxRef]);

    /// `take()` (`:130-135`): the queued transactions in the pool's order, leaving it empty.
    fn take(&mut self) -> Vec<TxRef>;

    /// `DynamicMemoryUsage()` (`:80-82`): Core's logical budget, by formula.
    fn dynamic_memory_usage(&self) -> usize;
}

/// The null pool: Core without a mempool, which keeps no pool (`validation.cpp:3314`).
impl DisconnectPool for () {
    fn add_from_block(&mut self, _: &[TxRef]) -> Vec<TxRef> {
        Vec::new()
    }

    fn remove_for_block(&mut self, _: &[TxRef]) {}

    fn take(&mut self) -> Vec<TxRef> {
        Vec::new()
    }

    fn dynamic_memory_usage(&self) -> usize {
        0
    }
}

/// Core `MemPoolRemovalReason`, the values the tip steps pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalReason {
    Reorg,
}

/// MP1's mempool, as the tip steps call it (§8.1; Core holds `m_mempool->cs` across ABC's inner
/// loop, so these run inside V's holds).
pub trait MempoolSink: Send {
    /// `DisconnectedBlockTransactions disconnectpool{MAX_DISCONNECTED_TX_POOL_SIZE * 1000}`.
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool>;

    /// Core's `if (!m_mempool) return;` (`validation.cpp:424`): the chainstate has a mempool, so
    /// V's `reorg_drain` moves the DMN tip before an adding drain (CH7 D16).
    fn has_mempool(&self) -> bool {
        true
    }

    /// `MaybeUpdateMempoolForReorg(disconnectpool, fAddToMempool)`, through V's `reorg_drain`
    /// (CH7-notes D60): `tip_list` is `Some` for an adding drain, V's DMN-tip list at the call,
    /// which Core's re-add reads through `GetListAtChainTip()` (`txmempool.cpp:647-699`) while the
    /// DMN tip lags the chain tip (D16); a non-adding drain reads no list, as Core's does not.
    fn maybe_update_for_reorg_at(
        &mut self,
        pool: Box<dyn DisconnectPool>,
        tip_list: Option<&Arc<MnList>>,
    );

    /// `removeForBlock(vtx, nHeight)` with V's DMN-tip list at the call, which Core's ProTx
    /// conflict removal reads (`txmempool.cpp:1069`, `:1250`; D16). V calls it only with a
    /// mempool (`validation.cpp:3466`).
    fn remove_for_block_at(&mut self, vtx: &[TxRef], height: u32, tip_list: &Arc<MnList>);

    /// `removeExpiredAssetUnlock(nHeight)`.
    fn remove_expired_asset_unlock(&mut self, height: u32);

    /// `removeRecursive(tx, reason)`.
    fn remove_recursive(&mut self, tx: &TxRef, reason: RemovalReason);

    /// `AddTransactionsUpdated(n)`.
    fn add_transactions_updated(&mut self, n: u32);

    /// `check(coins_tip, nSpendHeight)` (`-checkmempool`).
    fn check(&mut self, next_height: u32);
}

/// No mempool: MP1 implements [`MempoolSink`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NullMempool;

impl MempoolSink for NullMempool {
    fn new_disconnect_pool(&mut self) -> Box<dyn DisconnectPool> {
        Box::new(())
    }

    fn maybe_update_for_reorg_at(&mut self, _: Box<dyn DisconnectPool>, _: Option<&Arc<MnList>>) {}

    fn remove_for_block_at(&mut self, _: &[TxRef], _: u32, _: &Arc<MnList>) {}

    fn has_mempool(&self) -> bool {
        false
    }

    fn remove_expired_asset_unlock(&mut self, _: u32) {}

    fn remove_recursive(&mut self, _: &TxRef, _: RemovalReason) {}

    fn add_transactions_updated(&mut self, _: u32) {}

    fn check(&mut self, _: u32) {}
}

/// Core's `SynchronizationState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    InitReindex,
    InitDownload,
    PostInit,
}

/// `uiInterface`'s chain notifications (ND1's: `-blocknotify`, `waitfor*`'s `latestblock`).
/// V fires them after publication (N1).
pub trait UiNotifications: Send + Sync {
    /// `uiInterface.NotifyHeaderTip(state, height, time)` (`validation.cpp:3716`).
    fn notify_header_tip(&self, state: SyncState, height: u32, time: u32) {
        let _ = (state, height, time);
    }

    /// `uiInterface.NotifyBlockTip(state, pindexNewTip)` (`validation.cpp:3831`): ND1's
    /// `-blocknotify` and the `waitfor*` RPCs' `latestblock`.
    fn notify_block_tip(&self, state: SyncState, hash: BlockHash, height: u32) {
        let _ = (state, hash, height);
    }

    /// `uiInterface.NotifyChainLock(bestChainLockHash, bestChainLockHeight)` (`h:320`): ND1's
    /// `-chainlocknotify`.
    fn notify_chain_lock(&self, hash: BlockHash, height: i32) {
        let _ = (hash, height);
    }

    /// `SetMiscWarning(warning)` (`warnings.cpp:24-28`): ND1's `Warnings`, the text
    /// `GetWarnings` returns. V calls it for `UpdateTip`'s `DoWarning`
    /// (`validation.cpp:3156-3164`).
    fn set_misc_warning(&self, warning: &str) {
        let _ = warning;
    }

    /// `AlertNotify(strMessage)` (`validation.cpp:2001-2019`): `uiInterface.NotifyAlertChanged()`
    /// and ND1's `-alertnotify`. V calls it after the first [`set_misc_warning`] of its
    /// `DoWarning`, as Core's `fWarned` allows once per process.
    ///
    /// [`set_misc_warning`]: UiNotifications::set_misc_warning
    fn alert_notify(&self, message: &str) {
        let _ = message;
    }
}

/// No UI.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoUi;

impl UiNotifications for NoUi {}

/// InstantSend's half of `NetInstantSend::ResolveBlockConflicts` (`is:583-646`), which V asks
/// while it resolves an InstantSend lock's block conflicts (§5.6, D42). IX/LK1b implements it
/// over its lock database. V calls it inline, as a synchronous listener (LO-5): it never sends a
/// `ChainCommand` and awaits it.
pub trait IsConflicts: Send {
    /// `RetrieveISConflicts(islockHash, islock)`: the blocks holding a transaction that
    /// conflicts with the lock, in Core's order.
    fn retrieve_conflicts(&mut self, lock: &Hash256) -> Vec<BlockHash>;

    /// `HasTxForLock(islockHash)`.
    fn has_tx_for_lock(&mut self, lock: &Hash256) -> bool;

    /// `ClearConflicting` for the lock's conflicting transactions in `block`, before V
    /// invalidates it.
    fn clear_conflicting(&mut self, lock: &Hash256, block: &BlockHash);

    /// `RemoveConflictingLock(islockHash, islock)`: a conflict is ChainLocked, so the lock and
    /// the locks built on it go.
    fn remove_conflicting_lock(&mut self, lock: &Hash256);
}

/// No InstantSend: no lock has a conflict, as on a node with InstantSend inactive.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInstantSend;

impl IsConflicts for NoInstantSend {
    fn retrieve_conflicts(&mut self, _: &Hash256) -> Vec<BlockHash> {
        Vec::new()
    }

    fn has_tx_for_lock(&mut self, _: &Hash256) -> bool {
        false
    }

    fn clear_conflicting(&mut self, _: &Hash256, _: &BlockHash) {}

    fn remove_conflicting_lock(&mut self, _: &Hash256) {}
}
