//! The validation actor V (CH1-node): `ChainCommand` over `dash-chain`'s `HeaderChain`, with
//! Core's control flow line by line and CH1-node's own index tables.
//!
//! - [`actor`]: V's loop, command classes, release points and the deferred queue (§2).
//! - `abc`: `ActivateBestChain` and its tip steps over SB4's holds (§4.1–§4.5).
//! - `failure`: `InvalidateBlock`, `ResetBlockFailureFlags`' node half and `PreciousBlock`
//!   (§4.6, §4.7).
//! - [`command`]: [`ChainCommand`] and its replies (§2.2).
//! - `deployments`: CS4's deployments over V's header chain, with Core's versionbits and warning
//!   caches (CH3 §2.16).
//! - [`locks`]: ChainLocks on V: intake, record, enforcement, the [`ClSigs`] mirror and V's
//!   `TipHost` (§5).
//! - [`reply`]: [`ActorReply`] and LO-9's no-wait marker (§8.3).
//! - [`flush`]: [`FlushSite`], CH2b's flush at Core's sites, and the [`FlushHost`] V lends it
//!   (§7.1, §7.2).
//! - [`index`]: [`NodeBlockIndex`], CH1-node's tables (§3).
//! - `proposal`: `TestBlockValidity`'s command on the tip (CH3 §2.14).
//! - [`load`]: `LoadGenesisBlock` and `LoadChainTip` (§7.3).
//! - [`seams`]: the checks, block files, connector, mempool, abort, UI and InstantSend seams
//!   (§8.1, §5.6).
//! - [`signals`]: [`ValidationSignals`], Core's `CMainSignals` (§8.2).
//! - [`verify`]: `VerifyLoadedChainstate`, with the VerifyDB point and the load-time
//!   `ResetBlockFailureFlags(nullptr)` (CH1-node-LR, O2b D32).
//! - [`view`]: [`ChainView`], [`ChainViewHandle`] and [`TipSeqFence`] (§6).
//!
//! Semantics are Dash Core v24.0.0-rc.3's; `validation.cpp` cites are at that tag.

mod abc;
mod accept;
pub mod actor;
mod check;
pub mod command;
pub(crate) mod deployments;
#[cfg(feature = "statedigest")]
pub mod digest_tap;
mod failure;
pub mod flush;
pub mod index;
pub mod load;
pub mod locks;
mod proposal;
mod read_ahead;
pub mod reply;
pub mod seams;
pub mod signals;
pub mod verify;
pub mod view;

pub use accept::MIN_BLOCKS_TO_KEEP;
#[cfg(feature = "test-support")]
pub use actor::BetweenStores;
pub use actor::{
    COMMAND_CAPACITY, ChainHandle, ShutdownFlush, StartError, Stopped, VContext, VExit, VParts,
    spawn, start,
};
pub use command::{
    AttachCoins, BlockNotFound, BlockReply, BlockResult, BlockRpcReply, BlockState, ChainCommand,
    ChainQuery, Class, IsResolution, NotTested, QueryDeployments,
};
pub use flush::{
    AbortCause, CacheUsage, FlushBell, FlushChain, FlushHost, FlushReleases, FlushSite,
    FlushStateMode, NoFlush, SiteDecision,
};
pub use index::{CandKey, NodeBlockIndex};
pub use load::{
    INIT_BLOCK_DB_FAILED, INVALID_TIP, LOAD_BLOCK_DB_FAILED, LoadFailure, LoadedIndex,
    OPEN_BLOCK_DB_FAILED, StoredRecord, WRONG_DEVNET_DATADIR, WRONG_NETWORK_DATADIR,
    check_startup_readiness, devnet_genesis_block, genesis_block, guess_verification_progress,
    load_chain_tip, load_genesis_block, open_block_index,
};
pub use locks::{ChainLockSig, ClIntakeReply, ClReply, ClSigs, ClTrigger};
pub use read_ahead::ReadAheadSetup;
pub use reply::{
    ActorReply, Marked, NoWaitGuard, NodeJoinHandle, ReplySender, WaitError, inherit,
    no_wait_marked, no_wait_task, reply, spawn_thread,
};
pub use seams::{
    AbortPath, AbortSink, AttemptOrigin, BlockChecks, BlockConnector, BlockFiles, BlockSource,
    BlockStatusWriter, ChainLockView, ConnectCx, ConnectOutcome, CoreChecks, DisconnectPool,
    DisconnectResult, EvoTarget, EvoTxn, Fatal, GuardScope, IsConflicts, LocalShutdown,
    MemBlockFiles, MempoolSink, NoInstantSend, NoUi, NullConnector, NullMempool, ProposalChecks,
    RecordingAbort, RemovalReason, ScratchCoins, ScratchView, ShutdownFlag, SyncState,
    UiNotifications, ViewTarget,
};
pub use signals::{
    Barrier, Blocking, NewBlock, Queued, ServiceWait, SignalsThread, Staged, TreeRead,
    ValidationInterface, ValidationSignals,
};
pub use verify::{
    DEFAULT_CHECKBLOCKS, DEFAULT_CHECKLEVEL, VerifyDbArgs, VerifyDbResult, VerifyLoadError,
    VerifyLoadOptions,
};
pub use view::{ChainView, ChainViewHandle, LoadingFlags, TipSeqFence, TipWatch};

// Not under loom: its build runs only the queue model, and loom's locks panic outside a model.
#[cfg(all(test, not(loom)))]
mod tests;
