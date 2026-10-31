//! The node's RPC server (`dashd-rpc`): what init starts at step 4a with `-server`
//! (`AppInitServers`), and what `Interrupt` and `Shutdown` stop, in Core's order.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use dash_args::ArgsManager;
use dash_chain::{
    BlockId, ChainReader, EntryView, FlatFilePos, HeaderBatch, HeaderStop, HeadersResult, Status,
};
use dash_clock::{Clock as _, MockClock, SystemClock};
use dash_consensus::coins::stats::CoinStatsHashType;
use dash_consensus::coins::{BlockUndo, Coin};
use dash_consensus::ctx::{Deployment, DeploymentView as _};
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_consensus::{Block, Header};
use dash_p2p::dial::AddedNode;
use dash_p2p::permissions::permission_strings;
use dash_p2p::{CommandString, NetCore, Network};
use dash_params::{BuriedDeployment, ChainSetup};
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{
    BlockFiles, BlockNotFound, BlockState, Blocking, ChainHandle, ChainQuery, ChainView, NotTested,
    ValidationInterface, ValidationSignals, VerifyDbArgs, VerifyDbResult, WaitError,
    guess_verification_progress,
};
use dashd_chainstate::coins::{
    BlockRef, CoinsCache, CoinsDump, CoinsScan, CoinsScanError, CoinsStats, CoinsStatsError,
    OutPointKey,
};
use dashd_chainstate::connect::block_subsidy_after;
use dashd_masternode::sync::MasternodeSync;
use dashd_mining::{BlockTemplateSource as _, ChainTemplateSource, TemplateError, TemplateTxs};
use dashd_net::ConnMan;
use dashd_net::stats::NodeStats;
use dashd_p2p::Shared;
use dashd_p2p::processing::bloom::BloomFilter;
use dashd_p2p::processing::merkle::{merkle_block, partial_tree};
use dashd_p2p::processing::stats::NodeStateStats;
use dashd_rpc::blockchain::{
    Aborted, ActiveWindow, BestChainLock, BlockChange, BlockCommandError, BlockReadError,
    BloomFilterData, ChainState, ChainStateParams, ChainTipInfo, ChainTxError, ChainTxPoint,
    Chainman, DeploymentError, DeploymentReport, HeaderFacts, IndexStatus, NodeChain,
    NodeChainState, ProcessedBlock, PublishedChain, ScanControl, TipInfo, TxOutProofError,
    TxOutSetDump, TxOutSetError, TxOutSetInfo, TxOutSetScan, UndoReadError, UnspentCoin,
    WindowBlock,
};
use dashd_rpc::core_write::TxJsonParams;
use dashd_rpc::dash::{MnSync, NodeSync};
use dashd_rpc::http::{HttpServer, InitError};
use dashd_rpc::httprpc::HttpRpc;
use dashd_rpc::key_io::KeyIoParams;
use dashd_rpc::mining::{
    BlockCheck, Grind, LastBlockStats, Miner, MiningInfoParams, StateCatcher, TemplateRefusal,
    ValidationState,
};
use dashd_rpc::peers::{
    AddedNodeInfo, ConnectionCounts, LocalAddress, NetworkInfo, NetworkReach, NodeNet, PeerInfo,
};
use dashd_rpc::server::{NodeControl, RpcServer};
use dashd_store::KvEngine;

use crate::init::consts::{DEFAULT_INCREMENTAL_RELAY_FEE, DEFAULT_MIN_RELAY_TX_FEE};
use crate::logging::Logger;
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;
use crate::ui::Ui;
use crate::warnings::Warnings;
use crate::{CLIENT_VERSION, FULL_VERSION, InitFailure, PACKAGE_NAME, host};

/// `key_io.cpp`'s reads of `Params()`: the chain's Base58 prefixes and Platform HRP.
fn key_io_params(params: &ChainSetup) -> KeyIoParams {
    let a = &params.addr_params;
    KeyIoParams {
        pubkey_address: a.pubkey_addr,
        script_address: a.script_addr,
        secret_key: a.secret_key,
        ext_public_key: a.ext_pubkey,
        ext_secret_key: a.ext_secret,
        platform_hrp: params.platform_hrp.map(str::to_owned),
    }
}

/// Core's `AppInitServers` failing.
// Core: init.cpp:1636-1637
const HTTP_START_FAILED: &str = "Unable to start HTTP server. See debug log for details.";

/// What the commands reach of the node.
struct Control {
    logger: Arc<Logger>,
    shutdown: Arc<ShutdownState>,
    clock: Arc<MockClock>,
    startup_time: i64,
    /// `Params().IsMockableChain()`.
    mockable: bool,
    scheduler: Scheduler,
    /// V, which `setmocktime` and `mockscheduler` order themselves with.
    chain: Arc<ChainSlot>,
}

impl NodeControl for Control {
    fn start_shutdown(&self) {
        self.shutdown.start_shutdown();
    }

    fn shutdown_requested(&self) -> bool {
        self.shutdown.shutdown_requested()
    }

    fn now(&self) -> i64 {
        self.clock.node_now().secs()
    }

    fn startup_time(&self) -> i64 {
        self.startup_time
    }

    fn log_ips(&self) -> bool {
        self.logger.log_ips()
    }

    fn log_path(&self) -> String {
        self.logger.options().file_path.to_string_lossy().into_owned()
    }

    fn enable_log_category(&self, name: &str) -> bool {
        self.logger.enable_category_str(name)
    }

    fn disable_log_category(&self, name: &str) -> bool {
        self.logger.disable_category_str(name)
    }

    fn log_categories(&self) -> Vec<(&'static str, bool)> {
        self.logger.log_categories_list(false)
    }

    fn package_name(&self) -> &'static str {
        PACKAGE_NAME
    }

    fn full_version(&self) -> &'static str {
        FULL_VERSION
    }

    fn is_mockable_chain(&self) -> bool {
        self.mockable
    }

    /// Core holds `cs_main` so no validation sees the time change inside it. V's clock reads
    /// happen inside its turns, so the time is set in one of its own (a class R query) once V
    /// runs; before the load and after shutdown nothing validates. No chain client (wallet)
    /// keeps a clock of its own yet.
    // Core: rpc/node.cpp:266-283
    fn set_mocktime(&self, time: i64) {
        let clock = Arc::clone(&self.clock);
        let chain = self.chain.slot().as_ref().map(|w| w.chain.clone());
        match chain {
            Some(chain) => {
                // A V that stopped never runs the query: set it here then.
                let set = chain.query(move |_| clock.set_mocktime(time)).recv();
                if set.is_err() {
                    self.clock.set_mocktime(time);
                }
            }
            None => clock.set_mocktime(time),
        }
    }

    // Core: rpc/node.cpp:913-914
    fn mock_scheduler(&self, delta: Duration) {
        self.scheduler.mock_forward(delta);
        let signals = self.chain.slot().as_ref().map(|w| w.signals.clone());
        if let Some(signals) = signals {
            let synced = signals.sync(&mut Blocking);
            debug_assert!(
                !matches!(synced, Err(WaitError::WaitOnVFromNoWaitTask)),
                "an RPC worker"
            );
        }
    }
}

/// `NodeContext::mn_sync` for `mnsync`: SP1's sync from its construction at step 7, before the
/// chainstate loads, until shutdown stops the RPC server (Core resets `mn_sync` after it,
/// init.cpp:416).
#[derive(Default)]
pub struct SyncSlot {
    attached: Mutex<Option<Arc<SyncHandle>>>,
}

struct SyncHandle(Arc<MasternodeSync>);

impl SyncSlot {
    fn slot(&self) -> MutexGuard<'_, Option<Arc<SyncHandle>>> {
        self.attached.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Init constructed `mn_sync`.
    pub fn attach(&self, sync: &Arc<MasternodeSync>) {
        let handle = Arc::new(SyncHandle(Arc::clone(sync)));
        assert!(self.slot().replace(handle).is_none(), "init constructs mn_sync once");
    }

    /// Shutdown: the commands no longer reach the sync.
    pub fn detach(&self) {
        self.slot().take();
    }
}

impl NodeSync for SyncSlot {
    fn mn_sync(&self) -> Option<Arc<dyn MnSync>> {
        self.slot().clone().map(|h| h as _)
    }
}

impl MnSync for SyncHandle {
    fn asset_id(&self) -> i32 {
        self.0.asset_id()
    }
    fn asset_name(&self) -> &'static str {
        self.0.asset_name()
    }
    fn asset_start_time(&self) -> i64 {
        self.0.asset_start_time()
    }
    fn attempt(&self) -> i32 {
        self.0.attempt()
    }
    fn is_blockchain_synced(&self) -> bool {
        self.0.is_blockchain_synced()
    }
    fn is_synced(&self) -> bool {
        self.0.is_synced()
    }
    fn switch_to_next_asset(&self) {
        self.0.switch_to_next_asset();
    }
    fn reset(&self, force: bool) {
        self.0.reset(force, true);
    }
}

/// The chain the chain commands use: Core's `NodeContext::chainman`, V and the node's
/// validation signals from step 7 until shutdown stops the RPC server. Commands answer the init
/// message until step 13, so one that runs finds it attached.
#[derive(Default)]
pub struct ChainSlot {
    attached: Mutex<Option<Writer>>,
    /// The server whose commands use the slot: Core's `rpc_interruption_point` is its
    /// `IsRPCRunning()` turning false.
    server: Mutex<Weak<RpcServer>>,
    /// Core's `latestblock`, which the waits read and V's `NotifyBlockTip` feeds (RP1a-2a).
    block_change: Arc<BlockChange>,
}

impl ChainSlot {
    fn slot(&self) -> MutexGuard<'_, Option<Writer>> {
        self.attached.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// V has started on the loaded chainstate; `signals` are the node's, which V signals on,
    /// `files` a reader of V's block files of the slot's own (`getblock`), `miner` the node's one
    /// block assembler over `chain` (`generate*`), and `coins` V's coins cache (`gettxout`);
    /// `None`s in tests without them.
    pub fn attach(
        &self,
        chain: &ChainHandle,
        signals: &ValidationSignals,
        files: Option<Box<dyn BlockFiles>>,
        miner: Option<Arc<ChainTemplateSource>>,
        coins: Option<CoinsRead>,
    ) {
        let server = self.server.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let writer = Writer {
            chain: chain.clone(),
            signals: signals.clone(),
            server,
            files: files.map(|f| Arc::new(Mutex::new(f))),
            miner,
            coins,
        };
        assert!(self.slot().replace(writer).is_none(), "the chainstate loads once");
    }

    /// Shutdown: the commands no longer reach V, whose handle (and the stores behind it) is
    /// not kept past the data directory's lock.
    pub fn detach(&self) {
        self.slot().take();
    }

    /// The waits' `latestblock`, for V's `uiInterface` sink.
    pub fn block_change(&self) -> &Arc<BlockChange> {
        &self.block_change
    }

    /// Init's `RPCNotifyBlockChange(ActiveTip())` before the warmup ends: the attached chain's
    /// tip, or none.
    // Core: init.cpp:2761-2769
    pub fn notify_loaded_tip(&self) {
        let tip = self.published().and_then(|chain| {
            let height = chain.height();
            u32::try_from(height).ok().map(|h| (chain.block_hash(height), h))
        });
        self.block_change.set_tip(tip);
    }
}

impl NodeChain for ChainSlot {
    fn published(&self) -> Option<Box<dyn PublishedChain>> {
        let slot = self.slot();
        let chain = &slot.as_ref()?.chain;
        Some(Box::new(Published {
            view: chain.view(),
            reader: chain.reader().clone(),
        }))
    }

    fn chainman(&self) -> Option<Box<dyn Chainman>> {
        Some(Box::new(self.slot().as_ref()?.clone()))
    }
}

impl NodeChainState for ChainSlot {
    fn chain_state(&self) -> Option<Box<dyn ChainState>> {
        Some(Box::new(self.slot().as_ref()?.clone()))
    }
}

/// `gettxout`'s read of V's coins cache, over its engine: `GetCoin`, which caches the coin it
/// reads, and `GetBestBlock`. It runs in a V query turn: the cache is V's leaf (CH3a D50), and
/// between commands V holds no view of it.
pub(crate) type CoinsRead =
    Arc<dyn Fn(&OutPoint) -> Result<(Option<Coin>, BlockRef), Aborted> + Send + Sync>;

/// [`CoinsRead`] over the coins cache V was given. Every holder of the cell locks it on V's
/// thread, so in a V turn the lock is free; only a poisoned cell (a panicked V) fails it.
pub(crate) fn coins_read<E: KvEngine + 'static>(cell: Arc<Mutex<CoinsCache<E>>>) -> CoinsRead {
    Arc::new(move |out| {
        let mut coins = cell.try_lock().map_err(|e| {
            debug_assert!(
                !matches!(e, std::sync::TryLockError::WouldBlock),
                "the coins cache is held outside V's turn"
            );
            Aborted
        })?;
        let coin = coins.fetch(&OutPointKey::new(out)).map_err(|_| Aborted)?;
        let coin = coin.map(|c| c.to_coin());
        Ok((coin, coins.best_block().ok_or(Aborted)?))
    })
}

impl ChainSlot {
    /// The node's block assembler while the chain is attached.
    fn miner(&self) -> Option<Arc<ChainTemplateSource>> {
        self.slot().as_ref()?.miner.clone()
    }
}

/// The `generate*` commands over dashd-mining (PLAN-RP1a §2.2): its one source per node, and V
/// for `generateblock`'s own `TestBlockValidity`. A slot detached by shutdown answers as Core's
/// null `chainman`, and a grind it interrupts as `ShutdownRequested()`.
impl Miner for ChainSlot {
    fn create_new_block(
        &self,
        coinbase_script: &[u8],
        use_mempool: bool,
    ) -> Result<Block, TemplateRefusal> {
        let source = self.miner().ok_or(TemplateRefusal::NoChain)?;
        let txs = if use_mempool {
            TemplateTxs::Mempool
        } else {
            TemplateTxs::None
        };
        match source.create_new_block(coinbase_script, txs) {
            Ok(template) => Ok(template.block),
            Err(TemplateError::NotReady {
                height,
                rule,
                owner,
            }) => Err(TemplateRefusal::NotReady(format!(
                "block {height} needs {rule}, which is not served until {owner}"
            ))),
            Err(TemplateError::Failed(msg)) => Err(TemplateRefusal::Failed(msg)),
            Err(TemplateError::NoChain) => Err(TemplateRefusal::NoChain),
        }
    }

    fn grind(&self, block: &mut Block, max_tries: &mut u64, shutdown: &dyn Fn() -> bool) -> Grind {
        let Some(source) = self.miner() else {
            return Grind::Shutdown;
        };
        let pow = source.pow().expect("the source built the template from these parameters");
        match dashd_mining::grind(block, pow, max_tries, shutdown) {
            dashd_mining::Grind::Found => Grind::Found,
            dashd_mining::Grind::NonceExhausted => Grind::NonceExhausted,
            dashd_mining::Grind::OutOfTries => Grind::OutOfTries,
            dashd_mining::Grind::Shutdown => Grind::Shutdown,
        }
    }

    // Core: rpc/mining.cpp:388-399, validation.cpp:4929-4974
    fn test_block_validity(&self, block: Arc<Block>) -> BlockCheck {
        let Some(chain) = self.slot().as_ref().map(|w| w.chain.clone()) else {
            return BlockCheck::NoChain;
        };
        match chain.test_block_validity(block, false, false).recv() {
            Ok(Ok(BlockState::Valid)) => BlockCheck::Valid,
            Ok(Ok(BlockState::Invalid(e))) => BlockCheck::Invalid(e.reject_reason().to_owned()),
            // `state.Error(msg)` sets the reject reason to `msg`.
            Ok(Ok(BlockState::Error(msg))) => BlockCheck::Invalid(msg),
            Ok(Err(NotTested::NotBestPrev)) => BlockCheck::TipMoved,
            // The duplicate answers are `getblocktemplate`'s lookup's, which this call skips.
            Ok(Err(other)) => BlockCheck::NotTested(other.to_string()),
            Err(_) => BlockCheck::NoChain,
        }
    }

    /// The source's own record, as Core's statics; none while the slot is detached, where
    /// `getmininginfo` answers `EnsureChainman`'s error before it reads them.
    fn last_block_stats(&self) -> Option<LastBlockStats> {
        let last = self.miner()?.last_block_stats()?;
        Some(LastBlockStats {
            size: last.size,
            num_txs: last.num_txs,
        })
    }
}

/// `IsBlockPruned`: the node has pruned, and `e` had transactions but no longer has their data.
// Core: node/blockstorage.cpp:443-447
fn is_block_pruned(have_pruned: bool, e: &EntryView) -> bool {
    have_pruned && !e.status.has(Status::HAVE_DATA) && e.n_tx > 0
}

/// Core's `GetBlockPos`: the entry's data position, or the null position unless it has its data.
fn block_pos(e: &EntryView) -> FlatFilePos {
    match e.file {
        Some(file) if e.status.has(Status::HAVE_DATA) => FlatFilePos {
            file,
            pos: e.data_pos,
        },
        _ => FlatFilePos {
            file: -1,
            pos: 0,
        },
    }
}

/// `blockheaderToJSON(Tip(), pindex)`'s facts of `e`, read in `q`'s V turn.
// Core: rpc/blockchain.cpp:137-166 blockheaderToJSON
fn header_facts(q: &ChainQuery<'_>, e: &EntryView) -> Option<HeaderFacts> {
    let reader = q.chain.reader();
    let (tip, _) = q.chain.private_tip()?;
    let (confirmations, next) = if q.chain.active_contains_private(e.id) {
        let next = (e.height < tip.height)
            .then(|| q.chain.ancestor_private(e.height + 1))
            .flatten()
            .and_then(|id| reader.entry(id))
            .map(|n| n.hash);
        let depth = i32::try_from(tip.height - e.height + 1).expect("a depth fits");
        (depth, next)
    } else {
        (-1, None)
    };
    Some(HeaderFacts {
        hash: e.hash,
        header: e.header,
        height: e.height,
        confirmations,
        median_time_past: e.mtp,
        chain_work: format!("{:064x}", e.chain_work),
        n_tx: e.n_tx,
        next,
        chainlock: matches!(q.chain.has_chainlock(e.height, &e.hash), Ok(true)),
    })
}

/// The writing side of [`ChainSlot`]: V's handle and the signals `submitblock`'s catcher
/// registers on.
#[derive(Clone)]
struct Writer {
    chain: ChainHandle,
    signals: ValidationSignals,
    /// As [`ChainSlot`]'s.
    server: Weak<RpcServer>,
    /// `getblock`'s `ReadBlockFromDisk`: the block files only, no cache of connected blocks
    /// (DEC-606); `None` in tests without files, where every read fails.
    files: Option<Arc<Mutex<Box<dyn BlockFiles>>>>,
    /// The node's one `ChainTemplateSource` (MN0 r1: its stats stand in for Core's statics).
    miner: Option<Arc<ChainTemplateSource>>,
    /// V's coins cache; `None` in tests without one, where every read fails.
    coins: Option<CoinsRead>,
}

impl Chainman for Writer {
    /// The live entry's status: Core's read under `cs_main`, which may be ahead of the
    /// published view's.
    fn lookup_block_index(&self, hash: &BlockHash) -> Option<IndexStatus> {
        let reader = self.chain.reader();
        let status = reader.entry(reader.lookup(hash)?)?.status;
        Some(IndexStatus {
            valid_scripts: status.is_valid(Status::VALID_SCRIPTS),
            failed: status.has(Status::FAILED_MASK),
        })
    }

    /// V replies after the block's `ActivateBestChain`, and calls `BlockChecked` synchronously
    /// before it, so the catcher has seen every check of the block by the reply. A V that stops
    /// on an `AbortNode` drops the reply: neither accepted nor written now, which leaves the
    /// answer to the catcher, as Core's `false` return does. A failed block write is
    /// `AbortNode(state, ...)`: V sends its `BlockChecked(Error)` before it stops, so the answer is
    /// Core's -25 "System error: ..." (BL-279, validation.cpp:4849-4850, :4896-4897).
    fn process_new_block(
        &self,
        block: Arc<Block>,
        force_processing: bool,
        catcher: &Arc<StateCatcher>,
    ) -> ProcessedBlock {
        let result = {
            let _registered = Registered::new(&self.signals, Catcher(Arc::clone(catcher)));
            self.chain.process_block(block, force_processing, None, true).recv()
        };
        // A no-wait task's wait fails at once: the catcher would be gone before V ran.
        debug_assert!(!matches!(result, Err(WaitError::WaitOnVFromNoWaitTask)), "an RPC worker");
        result.map_or(
            ProcessedBlock {
                accepted: false,
                new_block: false,
            },
            |r| ProcessedBlock {
                accepted: r.accepted,
                new_block: r.new_block,
            },
        )
    }

    /// One class R turn: V's live index and private active chain, as Core's `cs_main` scope.
    /// Nothing in the turn panics: an index that cannot answer (no tip, a missing entry) gives
    /// no tips rather than stopping V.
    fn chain_tips(&self) -> Vec<ChainTipInfo> {
        let tips = self.chain.query(|q| {
            let reader = q.chain.reader();
            let Ok(tips) = q.chain.chain_tips_with(&|id| q.idx.have_txs_downloaded(id)) else {
                return Vec::new();
            };
            tips.into_iter()
                .filter_map(|t| {
                    Some(ChainTipInfo {
                        height: t.height,
                        hash: t.hash,
                        bits: reader.entry(t.id)?.header.bits,
                        chain_work: format!("{:064x}", t.chain_work),
                        branchlen: t.branchlen,
                        fork_point: reader.entry(t.fork_point)?.hash,
                        status: t.status.as_str(),
                    })
                })
                .collect()
        });
        tips.recv().unwrap_or_default()
    }

    /// One class R turn, as [`Self::chain_tips`].
    // Core: rpc/blockchain.cpp:99-108 ComputeNextBlockAndDepth, :136-166 blockheaderToJSON
    fn header_facts(&self, hash: &BlockHash) -> Option<HeaderFacts> {
        let hash = *hash;
        let facts = self.chain.query(move |q| {
            let reader = q.chain.reader();
            header_facts(q, &reader.entry(reader.lookup(&hash)?)?)
        });
        facts.recv().ok().flatten()
    }

    /// The lookup, the facts and `IsBlockPruned` in one V query turn, then the read from the
    /// slot's own block files at the position the turn saw. A failed read logs the reader's own
    /// line (`ReadBlockFromDisk`'s, with its hash-mismatch line).
    // Core: rpc/blockchain.cpp:1035-1051, :786-804 GetBlockChecked; node/blockstorage.cpp:443-447
    // IsBlockPruned, :833-846 ReadBlockFromDisk(pindex)
    fn read_block(&self, hash: &BlockHash) -> Result<(HeaderFacts, Arc<Block>), BlockReadError> {
        let hash = *hash;
        let looked = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let e = reader.entry(reader.lookup(&hash)?)?;
            let pruned = is_block_pruned(q.have_pruned, &e);
            Some((header_facts(q, &e)?, pruned, block_pos(&e)))
        });
        let (facts, pruned, pos) = looked.recv().ok().flatten().ok_or(BlockReadError::NotFound)?;
        if pruned {
            return Err(BlockReadError::Pruned);
        }
        let files = self.files.as_ref().ok_or(BlockReadError::NotOnDisk)?;
        let read = files.lock().unwrap_or_else(|p| p.into_inner()).read_block(pos, &hash);
        match read {
            Ok(block) => Ok((facts, block)),
            Err(e) => {
                tracing::error!("{e}");
                Err(BlockReadError::NotOnDisk)
            }
        }
    }

    /// In one V query turn (Core holds `cs_main` for the whole walk): `hash`'s entry and its
    /// `IsBlockPruned`, and the positions of it and of its successors on the active chain, at most
    /// `count` blocks; none after it when it is off the active chain (`CChain::Next`). Then each
    /// block is read from the slot's block files, as [`Self::read_block`]'s, and matched with the
    /// one filter, which each match may update; a block with no match is skipped.
    // Core: rpc/blockchain.cpp:888-922 getmerkleblocks; merkleblock.cpp:30-72
    fn merkle_blocks(
        &self,
        filter: BloomFilterData,
        hash: &BlockHash,
        count: u32,
    ) -> Result<Vec<Vec<u8>>, BlockReadError> {
        use dash_types::codec::BaseCodec as _;

        let hash = *hash;
        let looked = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let e = reader.entry(reader.lookup(&hash)?)?;
            let pruned = is_block_pruned(q.have_pruned, &e);
            let mut walk = vec![(e.hash, block_pos(&e))];
            if q.chain.active_contains_private(e.id) {
                let (tip, _) = q.chain.private_tip()?;
                let last = tip.height.min(e.height.saturating_add(count - 1));
                for height in e.height + 1..=last {
                    let next = reader.entry(q.chain.ancestor_private(height)?)?;
                    walk.push((next.hash, block_pos(&next)));
                }
            }
            Some((pruned, walk))
        });
        let (pruned, walk) = looked.recv().ok().flatten().ok_or(BlockReadError::NotFound)?;
        if pruned {
            return Err(BlockReadError::Pruned);
        }
        let files = self.files.as_ref().ok_or(BlockReadError::NotOnDisk)?;
        let mut filter = BloomFilter::from(dash_p2p_core::FilterLoad {
            data: filter.data,
            hash_funcs: filter.hash_funcs,
            tweak: filter.tweak,
            flags: filter.flags,
        });
        let mut out = Vec::new();
        for (hash, pos) in walk {
            let read = files.lock().unwrap_or_else(|p| p.into_inner()).read_block(pos, &hash);
            let block = read.map_err(|e| {
                tracing::error!("{e}");
                BlockReadError::NotOnDisk
            })?;
            let (msg, matched) = merkle_block(&block, &mut filter);
            if matched.is_empty() {
                continue;
            }
            let mut wire = Vec::new();
            msg.encode(&mut wire);
            out.push(wire);
        }
        Ok(out)
    }

    /// `IsBlockPruned` and the undo position in one V query turn, then the read from the slot's
    /// own block files, as [`Self::read_block`]'s. A block the index doesn't have reads as
    /// unreadable (no caller asks for one).
    // Core: rpc/blockchain.cpp:182-184 blockToJSON, :806-827 GetUndoChecked;
    // node/blockstorage.cpp:539-575 UndoReadFromDisk
    fn read_undo(&self, hash: &BlockHash) -> Result<BlockUndo, UndoReadError> {
        use dash_chain::IndexAccess as _;

        let hash = *hash;
        let looked = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let id = reader.lookup(&hash)?;
            let e = reader.entry(id)?;
            let pruned = is_block_pruned(q.have_pruned, &e);
            Some((pruned, reader.undo_pos(id), e.header.prev_hash))
        });
        let (pruned, pos, parent) =
            looked.recv().ok().flatten().ok_or(UndoReadError::Unreadable)?;
        if pruned {
            return Err(UndoReadError::Pruned);
        }
        // Core's GetUndoPos: the null position unless the entry has undo data.
        let Some(pos) = pos.filter(|pos| pos.file >= 0) else {
            tracing::error!("UndoReadFromDisk: no undo data available");
            return Err(UndoReadError::Unreadable);
        };
        let files = self.files.as_ref().ok_or(UndoReadError::Unreadable)?;
        let read = files.lock().unwrap_or_else(|p| p.into_inner()).read_undo(pos, &parent);
        read.map_err(|e| {
            tracing::error!("{e}");
            UndoReadError::Unreadable
        })
    }

    /// `GetBlockSubsidy(block)` as connect reads it ([`block_subsidy_after`]), in one V query turn,
    /// under the miner's subsidy parameters (the node's one set); `None` without a miner.
    // Core: validation.cpp:1884-1889 GetBlockSubsidy
    fn block_subsidy(&self, hash: &BlockHash) -> Option<i64> {
        let miner = Arc::clone(self.miner.as_ref()?);
        let hash = *hash;
        let looked = self.chain.query(move |q| {
            let subsidy = miner.subsidy_params()?;
            let reader = q.chain.reader();
            let prev = reader.entry(reader.lookup(&hash)?)?.prev;
            block_subsidy_after(&reader, &q.deployments, prev, subsidy).ok()
        });
        looked.recv().ok().flatten()
    }

    fn verify_db(&self, level: i32, depth: i32) -> Option<bool> {
        let args = VerifyDbArgs {
            level,
            depth,
        };
        let result = self.chain.verify_db(args).recv().ok()?;
        Some(result == VerifyDbResult::Success)
    }

    fn invalidate_block(&self, hash: BlockHash) -> Result<(), BlockCommandError> {
        block_command(self.chain.invalidate_block(hash).recv())
    }

    fn reconsider_block(
        &self,
        hash: BlockHash,
        ignore_chainlocks: bool,
    ) -> Result<(), BlockCommandError> {
        block_command(self.chain.reconsider_block(hash, ignore_chainlocks).recv())
    }

    fn precious_block(&self, hash: BlockHash) -> Result<(), BlockCommandError> {
        block_command(self.chain.precious_block(hash).recv())
    }

    /// A barrier on V's signals queue. An RPC worker may wait, so the sync is never refused.
    fn sync_validation_queue(&self) {
        let synced = self.signals.sync(&mut Blocking);
        debug_assert!(!matches!(synced, Err(WaitError::WaitOnVFromNoWaitTask)), "an RPC worker");
    }

    /// A one-header batch from no peer, as Core's `ProcessNewBlockHeaders({h})`.
    // Core: rpc/mining.cpp:1094-1095
    fn process_new_block_header(&self, header: Header) -> ValidationState {
        let batch = HeaderBatch {
            hashes: vec![header.hash()],
            headers: vec![header],
        };
        match self.chain.process_headers(batch, None).recv() {
            Ok(HeadersResult {
                stop:
                    Some(HeaderStop::Invalid {
                        reject,
                        ..
                    }),
                ..
            }) => ValidationState::Invalid(reject),
            Ok(_) => ValidationState::Valid,
            Err(_) => ValidationState::Error(dashd_rpc::blockchain::ABORTED.into()),
        }
    }

    /// V's flush and snapshot in one class M turn (RP1a D2), then the scan on its own thread,
    /// interrupted once the server stops running; the best block's height is read live.
    // Core: rpc/blockchain.cpp:1212-1220, :1260; rpc/server.cpp:325-330 RpcInterruptionPoint
    fn txout_set_info(&self, hash_type: CoinStatsHashType) -> Result<TxOutSetInfo, TxOutSetError> {
        let stats = scan_coins(self, hash_type, None)?;
        let reader = self.chain.reader();
        let entry = reader.lookup(&stats.best_block).and_then(|id| reader.entry(id));
        let height = entry.ok_or(TxOutSetError::Unreadable)?.height;
        Ok(TxOutSetInfo {
            best_block: stats.best_block,
            height,
            stats: stats.stats,
            disk_size: stats.disk_size,
        })
    }

    /// `txout_set_info`'s turn and scan, whose thread then writes the file. Core's
    /// `LOG_TIME_SECONDS` scope starts on that thread, once the statistics are done, and ends
    /// when the scan does, interrupted or not; the base's height is read from the index there
    /// (the thread never waits on V, LO-9), its `nChainTx` from V afterwards.
    // Core: rpc/blockchain.cpp:3147-3210 CreateUTXOSnapshot; logging/timer.h:30-48
    fn dump_txout_set(
        &self,
        file: File,
        path: &Path,
        temppath: &Path,
    ) -> Result<TxOutSetDump, TxOutSetError> {
        let timer = Arc::new(Mutex::new(None));
        let started = Arc::clone(&timer);
        let reader = self.chain.reader().clone();
        let (path, temppath) = (path.display().to_string(), temppath.display().to_string());
        // Core logs both lines on the RPC worker, so the scan's thread takes the worker's name.
        let worker = crate::logging::thread_name();
        let on_start = move |best: &BlockHash| {
            crate::logging::set_thread_name(&worker);
            // The flushed tip always has an entry (Core's `CHECK_NONFATAL`); without one there
            // is no line, and the missing entry after the scan answers "Unable to read UTXO set".
            let Some(e) = reader.lookup(best).and_then(|id| reader.entry(id)) else {
                return;
            };
            let title = format!(
                "CreateUTXOSnapshot: writing UTXO snapshot at height {} ({best}) to file {path} \
                 (via {temppath})",
                e.height
            );
            tracing::info!("{title} started");
            *started.lock().unwrap_or_else(|p| p.into_inner()) = Some((title, Instant::now()));
        };
        let dump = CoinsDump {
            file: Box::new(file),
            on_start: Box::new(on_start),
        };
        let result = scan_coins(self, CoinStatsHashType::HashSerialized, Some(dump));
        let stats = finish_dump_timer(&timer, result, |line| tracing::info!("{line}"))?;
        let reader = self.chain.reader();
        let id = reader.lookup(&stats.best_block).ok_or(TxOutSetError::Unreadable)?;
        let height = reader.entry(id).ok_or(TxOutSetError::Unreadable)?.height;
        let n_chain_tx = self.chain.query(move |q| q.idx.n_chain_tx(id)).recv();
        Ok(TxOutSetDump {
            base: stats.best_block,
            height,
            coins: stats.stats.txouts,
            hash: stats.stats.hash.ok_or(TxOutSetError::Unreadable)?,
            n_chain_tx: n_chain_tx.map_err(|_| TxOutSetError::Unreadable)?.into(),
        })
    }

    /// The block's position in one V query turn: `block`'s entry, or the active chain's block
    /// at the height of the first unspent output `AccessByTxid` reads over V's coins cache, up
    /// to `MAX_OUTPUTS_PER_BLOCK` outputs a txid as Core's does. Then the read from the slot's
    /// block files at that position, as [`Self::read_block`]'s but with no `IsBlockPruned` check
    /// (Core reads the pruned block's null position and logs the failure), the count, and the
    /// proof's partial tree from dashd-p2p.
    // Core: rpc/txoutproof.cpp:76-122; coins.cpp:340-353 AccessByTxid; merkleblock.cpp:30-72
    fn txout_proof(
        &self,
        txids: &[TxHash],
        block: Option<BlockHash>,
    ) -> Result<Vec<u8>, TxOutProofError> {
        use dash_types::codec::BaseCodec as _;

        /// `MaxBlockSize() / GetSerializeSize(CTxOut())`: a null `CTxOut` is 9 bytes.
        // Core: coins.cpp:342
        const MAX_OUTPUTS_PER_BLOCK: u32 = 2_000_000 / 9;

        let coins = self.coins.clone();
        let wanted = txids.to_vec();
        let found = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let id = match block {
                Some(hash) => reader.lookup(&hash).ok_or(TxOutProofError::BlockNotFound)?,
                None => {
                    let coins = coins.ok_or(TxOutProofError::NotYetInBlock)?;
                    // A failed read stops the walk: Core's `CCoinsViewErrorCatcher` aborts the
                    // node, and V's latch has tripped (D57).
                    let mut height = None;
                    'txids: for txid in &wanted {
                        for vout in 0..MAX_OUTPUTS_PER_BLOCK {
                            let out = OutPoint {
                                hash: *txid,
                                index: vout,
                            };
                            let (coin, _) = coins(&out).map_err(|_| TxOutProofError::Aborted)?;
                            if let Some(coin) = coin {
                                height = Some(coin.height());
                                break 'txids;
                            }
                        }
                    }
                    height
                        .and_then(|h| q.chain.ancestor_private(h))
                        .ok_or(TxOutProofError::NotYetInBlock)?
                }
            };
            let e = reader.entry(id).ok_or(TxOutProofError::BlockNotFound)?;
            Ok((e.hash, block_pos(&e)))
        });
        let (hash, pos) = found.recv().unwrap_or(Err(TxOutProofError::Aborted))?;
        let files = self.files.as_ref().ok_or(TxOutProofError::Unreadable)?;
        let read = files.lock().unwrap_or_else(|p| p.into_inner()).read_block(pos, &hash);
        let block = read.map_err(|e| {
            tracing::error!("{e}");
            TxOutProofError::Unreadable
        })?;
        let mut hashes = Vec::with_capacity(block.txs().len());
        let mut matches = Vec::with_capacity(block.txs().len());
        for tx in block.txs() {
            let txid = tx.txid();
            matches.push(txids.binary_search_by_key(txid.as_bytes(), |t| *t.as_bytes()).is_ok());
            hashes.push(*txid.as_bytes());
        }
        if matches.iter().filter(|&&m| m).count() != txids.len() {
            return Err(TxOutProofError::NotAllFound);
        }
        let msg = dash_p2p_core::MerkleBlock {
            header: *block.header().raw(),
            txn: partial_tree(&hashes, &matches),
        };
        let mut wire = Vec::new();
        msg.encode(&mut wire);
        Ok(wire)
    }

    /// `scantxoutset`'s copy-point flush and walk on its own thread, as `gettxoutsetinfo`'s
    /// scan; the interruption point fires once the server stops running. The tip's height is
    /// read from the index, whose entry for a flushed tip never goes away.
    // Core: rpc/blockchain.cpp:2756-2774
    fn scan_txout_set(
        &self,
        needles: HashSet<Vec<u8>>,
        control: ScanControl,
    ) -> Result<TxOutSetScan, TxOutSetError> {
        let server = self.server.clone();
        let interrupted = Arc::new(move || !server.upgrade().is_some_and(|s| s.is_running()));
        let scan = CoinsScan {
            needles,
            progress: control.progress,
            should_abort: control.should_abort,
            interrupted,
        };
        let job = self.chain.coins_scan(scan).recv().ok().flatten();
        let walk = match job.ok_or(TxOutSetError::Unreadable)?.wait() {
            Ok(walk) => walk,
            Err(CoinsScanError::Interrupted) => return Err(TxOutSetError::Interrupted),
            Err(_) => return Err(TxOutSetError::Unreadable),
        };
        let reader = self.chain.reader();
        let entry = reader.lookup(&walk.best_block).and_then(|id| reader.entry(id));
        let height = entry.ok_or(TxOutSetError::Unreadable)?.height;
        Ok(TxOutSetScan {
            success: walk.success,
            count: walk.count,
            best_block: walk.best_block,
            height,
            unspents: walk.into_outpoints().collect(),
        })
    }
}

/// The end of `CreateUTXOSnapshot`'s `LOG_TIME_SECONDS` scope: once the file's walk started
/// (`timer` holds the title and start), "completed" is logged whatever `result` is, as Core's
/// timer logs from its destructor while an interruption unwinds; a walk that never started (the
/// statistics failed) logs nothing.
// Core: rpc/blockchain.cpp:3184; logging/timer.h
fn finish_dump_timer<T, E>(
    timer: &Mutex<Option<(String, Instant)>>,
    result: Result<T, E>,
    log: impl FnOnce(String),
) -> Result<T, E> {
    if let Some((title, start)) = timer.lock().unwrap_or_else(|p| p.into_inner()).take() {
        log(format!("{title} completed ({:.2}s)", start.elapsed().as_secs_f64()));
    }
    result
}

/// The coins scan both `gettxoutsetinfo` and `dumptxoutset` run, interrupted once the server
/// stops running; a `dump` is written from its snapshot.
fn scan_coins(
    writer: &Writer,
    hash_type: CoinStatsHashType,
    dump: Option<CoinsDump>,
) -> Result<CoinsStats, TxOutSetError> {
    let server = writer.server.clone();
    let interrupted = Arc::new(move || !server.upgrade().is_some_and(|s| s.is_running()));
    let job = writer.chain.coins_stats(hash_type, interrupted, dump).recv().ok().flatten();
    match job.ok_or(TxOutSetError::Unreadable)?.wait() {
        Ok(stats) => Ok(stats),
        Err(CoinsStatsError::Interrupted) => Err(TxOutSetError::Interrupted),
        Err(CoinsStatsError::Write(_)) => Err(TxOutSetError::Unwritable),
        Err(_) => Err(TxOutSetError::Unreadable),
    }
}

/// `getchaintxstats`' facts of `id`.
fn chain_tx_point(q: &ChainQuery<'_>, id: BlockId) -> Option<ChainTxPoint> {
    let e = q.chain.reader().entry(id)?;
    Some(ChainTxPoint {
        hash: e.hash,
        height: e.height,
        time: e.header.time,
        median_time_past: e.mtp,
        chain_tx: q.idx.n_chain_tx(id),
    })
}

/// RP1a-2b's reads, each one class R turn on V's live index and private active chain (Core's
/// `cs_main` scope) unless it says otherwise. A V that has stopped (`AbortNode`) answers
/// [`Aborted`], as every RP1a read does (D9).
impl ChainState for Writer {
    /// The latch is read for V's own tip in the same turn, as Core reads it under `cs_main`.
    // Core: rpc/blockchain.cpp:1594-1609; validation.cpp:5907-5922 GuessVerificationProgress
    fn tip_info(&self) -> Result<TipInfo, Aborted> {
        let handle = self.chain.view_handle();
        // Core's `time(nullptr)`: the system time, never the mock time.
        let now = SystemClock.node_now().secs();
        let info = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let (tip, _) = q.chain.private_tip()?;
            let e = reader.entry(tip.id)?;
            let headers = reader.entry(q.chain.best_header())?.height;
            let data = &q.chain.params().chain_tx_data;
            Some(TipInfo {
                height: e.height,
                hash: e.hash,
                bits: e.header.bits,
                time: e.header.time,
                median_time_past: e.mtp,
                headers,
                verification_progress: guess_verification_progress(
                    data,
                    q.idx.n_chain_tx(tip.id),
                    e.header.time,
                    now,
                ),
                // V's own tip, read in this turn (RP1a D35). The published view's latch
                // answers the same here, as V publishes its tip at every command's end (D65),
                // but not outside the turn.
                initial_block_download: handle.ibd_at(Some((e.chain_work, e.header.time))),
                chain_work: format!("{:064x}", e.chain_work),
            })
        });
        // V has its genesis tip from start; none is a V that stopped before it.
        info.recv().ok().flatten().ok_or(Aborted)
    }

    /// Read under the block files' own lock.
    fn size_on_disk(&self) -> u64 {
        self.files
            .as_ref()
            .map_or(0, |f| f.lock().unwrap_or_else(|p| p.into_inner()).current_usage())
    }

    /// CS4's report over V's deployments, with EHF signals from none until CH7a (RP1a D34),
    /// once `GetForBlock`'s walk is known to answer (BL-270, RP1a D52).
    fn deployment_info(
        &self,
        hash: Option<&BlockHash>,
    ) -> Result<Option<DeploymentReport>, DeploymentError> {
        let hash = hash.copied();
        let report = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let id = match hash {
                Some(hash) => reader.lookup(&hash),
                None => q.chain.private_tip().map(|(tip, _)| tip.id),
            };
            let Some(e) = id.and_then(|id| reader.entry(id)) else {
                return Ok(None);
            };
            let walk = std::iter::successors(Some(e), |e| e.prev.and_then(|p| reader.entry(p)));
            let v20 = Deployment::Buried(BuriedDeployment::V20);
            ehf_signals_read(walk.map(|e| ehf_row(e.status, q.deployments.active_at(e.id, v20))))
                .map_err(DeploymentError::EhfSignals)?;
            Ok(Some(DeploymentReport {
                hash: e.hash,
                height: e.height,
                deployments: q.deployments.deployment_info(e.id),
            }))
        });
        report.recv().unwrap_or(Err(DeploymentError::Aborted))
    }

    // Core: rpc/blockchain.cpp:2103-2117
    fn chain_tx_final(&self, hash: Option<&BlockHash>) -> Result<ChainTxPoint, ChainTxError> {
        let hash = hash.copied();
        let point = self.chain.query(move |q| {
            let id = match hash {
                Some(hash) => {
                    let id = q.chain.reader().lookup(&hash).ok_or(ChainTxError::NotFound)?;
                    if !q.chain.active_contains_private(id) {
                        return Err(ChainTxError::NotInMainChain);
                    }
                    id
                }
                None => q.chain.private_tip().ok_or(ChainTxError::NotFound)?.0.id,
            };
            chain_tx_point(q, id).ok_or(ChainTxError::NotFound)
        });
        point.recv().unwrap_or(Err(ChainTxError::Aborted))
    }

    fn chain_tx_ancestor(
        &self,
        of: &BlockHash,
        height: u32,
    ) -> Result<Option<ChainTxPoint>, Aborted> {
        let of = *of;
        let point = self.chain.query(move |q| {
            let reader = q.chain.reader();
            chain_tx_point(q, reader.ancestor(reader.lookup(&of)?, height)?)
        });
        point.recv().map_err(|_| Aborted)
    }

    // Core: rpc/blockchain.cpp:1380-1394
    fn txout(&self, out: &OutPoint) -> Result<Option<UnspentCoin>, Aborted> {
        let coins = self.coins.clone().ok_or(Aborted)?;
        let out = *out;
        let read = self.chain.query(move |_| {
            let (coin, best) = coins(&out)?;
            Ok(coin.map(|coin| UnspentCoin {
                coin,
                best_block: best.hash,
                best_height: best.height,
            }))
        });
        read.recv().unwrap_or(Err(Aborted))
    }

    /// The published signatures, as Core reads its handler outside `cs_main`; then the live
    /// index for the block.
    fn best_chainlock(&self) -> Option<BestChainLock> {
        let view = self.chain.view();
        let clsig = view.best_chainlock()?;
        Some(BestChainLock {
            height: clsig.height,
            block_hash: clsig.block_hash,
            sig: clsig.sig,
            known_block: self.chain.reader().lookup(&clsig.block_hash).is_some(),
        })
    }

    fn has_block(&self, hash: &BlockHash) -> bool {
        self.chain.reader().lookup(hash).is_some()
    }

    // Core: rpc/blockchain.cpp:750-781
    fn headers_from(&self, hash: &BlockHash, count: usize) -> Result<Vec<HeaderFacts>, Aborted> {
        let hash = *hash;
        let headers = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let mut headers = Vec::new();
            let mut at = reader.lookup(&hash);
            while let Some(facts) = at.and_then(|id| header_facts(q, &reader.entry(id)?)) {
                at = facts.next.and_then(|next| reader.lookup(&next));
                headers.push(facts);
                if headers.len() >= count {
                    break;
                }
            }
            headers
        });
        headers.recv().map_err(|_| Aborted)
    }

    // Core: rpc/mining.cpp:66-110 GetNetworkHashPS's reads of the active chain
    fn active_window(&self, height: Option<u32>, back: u32) -> Result<ActiveWindow, Aborted> {
        let window = self.chain.query(move |q| {
            let reader = q.chain.reader();
            let (tip, _) = q.chain.private_tip()?;
            let tip_height = reader.entry(tip.id)?.height;
            let at = height.unwrap_or(tip_height);
            let mut blocks = Vec::new();
            if at <= tip_height {
                // `pprev` from the block, as Core walks it.
                let mut e = reader.entry(reader.ancestor(tip.id, at)?)?;
                loop {
                    blocks.push(WindowBlock {
                        height: e.height,
                        bits: e.header.bits,
                        time: e.header.time,
                        chain_work: e.chain_work,
                    });
                    match e.prev {
                        Some(prev) if blocks.len() <= back as usize => e = reader.entry(prev)?,
                        _ => break,
                    }
                }
            }
            Some(ActiveWindow {
                height: tip_height,
                blocks,
            })
        });
        window.recv().ok().flatten().ok_or(Aborted)
    }
}

/// What `CMNHFManager::GetForBlock`'s walk meets at one block (BL-270, RP1a D52).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EhfRow {
    /// `GetFromCache` answers and the walk ends: the block is below V20's height (an empty
    /// map), or its map was stored when it was connected.
    Found,
    /// No stored map: `ProcessBlock` rebuilds it from the block's data, after its parent's.
    FromDisk,
    /// No stored map and no data: `ReadBlockFromDisk` fails.
    Unreadable,
}

/// [`EhfRow`] of a block with `status`, where V20 is active at it or not. Core stores a block's
/// map when `ConnectBlock` processes it, so a block that reached `BLOCK_VALID_SCRIPTS` has one,
/// invalidated since or not. Until CH7a keeps the maps (RP1a D52) that level stands for the
/// stored row.
// Core: evo/mnhftx.cpp:323-362 GetFromCache
fn ehf_row(status: Status, v20_active: bool) -> EhfRow {
    let connected = status.bits() & Status::VALID_MASK >= Status::VALID_SCRIPTS;
    if connected || !v20_active {
        EhfRow::Found
    } else if status.has(Status::HAVE_DATA) {
        EhfRow::FromDisk
    } else {
        EhfRow::Unreadable
    }
}

/// Whether `GetForBlock` answers for a block, given [`EhfRow`] of the block and then of each
/// parent for as long as the walk reads them: its `std::runtime_error` where a block it must
/// rebuild has no data. A rebuild is taken to succeed (`failed-getehfforblock-construct` and
/// Core's rebuild come with CH7a, RP1a D52).
// Core: evo/mnhftx.cpp:285-321 CMNHFManager::GetForBlock
fn ehf_signals_read(rows: impl IntoIterator<Item = EhfRow>) -> Result<(), &'static str> {
    for row in rows {
        match row {
            EhfRow::Found => return Ok(()),
            EhfRow::FromDisk => {}
            EhfRow::Unreadable => return Err("failed-getehfforblock-read"),
        }
    }
    // Past genesis: `GetFromCache(nullptr)` answers.
    Ok(())
}

/// A block command's reply: V answers the lookup, and drops the reply when an `AbortNode`
/// stops it (`BlockRpcReply`).
fn block_command(
    reply: Result<Result<(), BlockNotFound>, WaitError>,
) -> Result<(), BlockCommandError> {
    match reply {
        Ok(Ok(())) => Ok(()),
        Ok(Err(BlockNotFound)) => Err(BlockCommandError::NotFound),
        Err(_) => Err(BlockCommandError::Aborted),
    }
}

/// A listener registered on the node's signals until dropped: Core's
/// `RegisterSharedValidationInterface` and `UnregisterSharedValidationInterface` around a call,
/// the second on every path out of it.
struct Registered<'a> {
    signals: &'a ValidationSignals,
    listener: Arc<dyn ValidationInterface>,
}

impl<'a> Registered<'a> {
    fn new(signals: &'a ValidationSignals, listener: impl ValidationInterface + 'static) -> Self {
        let listener: Arc<dyn ValidationInterface> = Arc::new(listener);
        signals.register(Arc::clone(&listener));
        Registered {
            signals,
            listener,
        }
    }
}

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        self.signals.unregister(&self.listener);
    }
}

/// `submitblock_StateCatcher` as a listener on V's signals.
struct Catcher(Arc<StateCatcher>);

impl ValidationInterface for Catcher {
    fn block_checked(&self, block: &Block, state: &BlockState, _ibd: bool) {
        self.0.block_checked(block, || match state {
            BlockState::Valid => ValidationState::Valid,
            BlockState::Invalid(e) => ValidationState::Invalid(e.clone()),
            BlockState::Error(msg) => ValidationState::Error(msg.clone()),
        });
    }
}

/// V's last published view, read as Core reads `ActiveChain()` under `cs_main`. Its active chain
/// is the view's own (never the live chain); an entry's hash is immutable, so the live index
/// answers it for any block of the view.
struct Published {
    view: Arc<ChainView>,
    reader: ChainReader,
}

impl PublishedChain for Published {
    fn height(&self) -> i32 {
        let snap = &self.view.snap;
        if snap.has_tip() {
            i32::try_from(snap.active.height()).expect("a height fits")
        } else {
            -1
        }
    }

    fn block_hash(&self, height: i32) -> BlockHash {
        // A node's active chain starts at 0 and its index never evicts (`ChainConfig::node`):
        // every height in `0..=height()` has an entry. Anchored or windowed chains are SPV's.
        let at = u32::try_from(height).ok().and_then(|h| self.view.snap.active.at(h));
        let id = at.expect("a height of the active chain");
        self.reader.entry(id).expect("an active block is indexed").hash
    }
}

/// The network the network commands use: Core's `NodeContext::connman` and `peerman`, from
/// step 12 (the connection manager's threads started, which `addnode onetry` opens through)
/// until shutdown stops the RPC server. Core builds the connection manager at step 6, but no
/// command runs before step 13 ends the warmup, so one attachment serves them all.
#[derive(Default)]
pub struct NetSlot {
    attached: Mutex<Option<NetParts>>,
}

#[derive(Clone)]
struct NetParts {
    core: Arc<NetCore>,
    shared: Arc<Shared>,
    connman: Arc<ConnMan>,
}

impl NetSlot {
    fn slot(&self) -> MutexGuard<'_, Option<NetParts>> {
        self.attached.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Step 12: the network core, the peer manager's shared half and the started connection
    /// manager.
    pub fn attach(&self, core: &Arc<NetCore>, shared: &Arc<Shared>, connman: &Arc<ConnMan>) {
        let parts = NetParts {
            core: Arc::clone(core),
            shared: Arc::clone(shared),
            connman: Arc::clone(connman),
        };
        assert!(self.slot().replace(parts).is_none(), "the network attaches once");
    }

    fn core(&self) -> Option<Arc<NetCore>> {
        self.slot().as_ref().map(|parts| Arc::clone(&parts.core))
    }

    /// Shutdown: the commands no longer reach the network, which stops next.
    pub fn detach(&self) {
        self.slot().take();
    }
}

impl NodeNet for NetSlot {
    /// `GetNodeStats` (every peer not marked for disconnection, in table order), then each
    /// peer's `GetNodeStateStats`, skipping a peer whose state is gone as Core does. The slot's
    /// lock is not held across the reads: the state stats may wait for V's next publication.
    // Core: rpc/net.cpp:215-231, net.cpp:4563-4576 CConnman::GetNodeStats
    fn peer_info(&self) -> Option<Vec<PeerInfo>> {
        let NetParts {
            core,
            shared,
            ..
        } = self.slot().clone()?;
        let peers = core
            .peers()
            .iter()
            .filter(|peer| !peer.disconnect_requested())
            .filter_map(|peer| {
                let stats = NodeStats::of(&core, peer);
                let state = shared.node_state_stats(peer)?;
                Some(peer_info_from(stats, state))
            })
            .collect();
        Some(peers)
    }

    /// The core's and the peer manager's state. Two fields hold Core's value for the options
    /// rdashd refuses (BL-391 D-rows): the socket events are the platform's default
    /// (`-socketevents`), and the relay fees are the mempool's defaults (`-minrelaytxfee`,
    /// `-incrementalrelayfee`, MP1a). Each network's proxy is the configured one (P8a).
    // Core: rpc/net.cpp:690-737, net.cpp:4520-4540 CConnman::GetNodeCount
    fn network_info(&self) -> Option<NetworkInfo> {
        let NetParts {
            core,
            shared,
            ..
        } = self.slot().clone()?;
        let mut connections = ConnectionCounts::default();
        let mut connections_mn = ConnectionCounts::default();
        for peer in core.peers().iter().filter(|peer| !peer.disconnect_requested()) {
            connections.add(peer.is_inbound());
            if peer.verified_proreg_hash() != [0; 32] {
                connections_mn.add(peer.is_inbound());
            }
        }
        let identity = shared.identity();
        Some(NetworkInfo {
            version: CLIENT_VERSION,
            build_version: FULL_VERSION.to_owned(),
            subversion: String::from_utf8_lossy(identity.user_agent.as_bytes()).into_owned(),
            protocol_version: identity.protocol_version.0,
            local_services: core.config().local_services.0,
            local_relay: !shared.ignores_incoming_txs(),
            time_offset: core.time_offset(),
            network_active: core.network_active(),
            connections,
            connections_mn,
            socket_events: DEFAULT_SOCKET_EVENTS,
            networks: [Network::Ipv4, Network::Ipv6, Network::Onion, Network::I2p, Network::Cjdns]
                .into_iter()
                .map(|net| NetworkReach {
                    name: net.name(),
                    reachable: core.is_reachable(net),
                    proxy: core.proxy(net).map(|p| (p.to_string(), p.randomize_credentials)),
                })
                .collect(),
            relay_fees: Some((DEFAULT_MIN_RELAY_TX_FEE, DEFAULT_INCREMENTAL_RELAY_FEE)),
            local_addresses: core
                .local_addresses()
                .into_iter()
                .map(|(addr, port, score)| LocalAddress {
                    address: addr.to_string(),
                    port,
                    score,
                })
                .collect(),
        })
    }

    /// rdashd has no masternode sync to reset (Core's `mn_sync->Reset()`).
    fn set_network_active(&self, active: bool) -> Option<bool> {
        let core = self.core()?;
        core.set_network_active(active);
        Some(core.network_active())
    }

    fn v2_transport(&self) -> Option<bool> {
        Some(self.core()?.we_advertise_v2())
    }

    /// The slot's lock is not held across the connect, which may take `-timeout`. The result is
    /// dropped, as Core's RPC drops it.
    // Core: rpc/net.cpp:355-360, net.cpp:3586-3665 CConnman::OpenNetworkConnection
    fn open_one_try(&self, node: &str, use_v2: bool) {
        let connman = self.slot().as_ref().map(|parts| Arc::clone(&parts.connman));
        if let Some(connman) = connman {
            let _ = connman.open_one_try_blocking(node, use_v2);
        }
    }

    fn add_node(&self, node: &str, use_v2: bool) -> bool {
        self.core().is_some_and(|core| {
            core.add_node(AddedNode {
                node: node.to_owned(),
                use_v2,
            })
        })
    }

    fn remove_added_node(&self, node: &str) -> bool {
        self.core().is_some_and(|core| core.remove_added_node(node))
    }

    fn disconnect_by_address(&self, address: &str) -> bool {
        self.core().is_some_and(|core| core.disconnect_by_name(address))
    }

    fn disconnect_by_id(&self, id: i64) -> bool {
        self.core().is_some_and(|core| core.disconnect_by_id(id))
    }

    fn added_node_info(&self) -> Option<Vec<AddedNodeInfo>> {
        let to_rpc = |info: dash_p2p::dial::AddedNodeInfo| AddedNodeInfo {
            added_node: info.node.node,
            connected: info.connection.map(|c| (c.service.to_string(), c.inbound)),
        };
        self.core().map(|core| core.added_node_info(true).into_iter().map(to_rpc).collect())
    }
}

/// Core's `DEFAULT_SOCKETEVENTS`: `-socketevents` is refused, so the mode is always the
/// platform's default, which `compat.h` picks by the target (`__linux__` includes Android).
// Core: compat/compat.h:102-112, util/sock.h:18-26
const DEFAULT_SOCKET_EVENTS: &str = if cfg!(target_os = "linux") || cfg!(target_os = "android") {
    "epoll"
} else if cfg!(target_os = "freebsd") || cfg!(target_vendor = "apple") {
    "kqueue"
} else {
    "select"
};

/// The plain-data view of one peer's stats, rendered with Core's helpers. Two fields hold only
/// their defaults until their layers exist (2b-D38): the MNAUTH hashes stay zero, so
/// `verified_*` are never written (no MNAUTH handling), and `last_tx_time` stays 0 (no
/// transaction is accepted from a peer).
fn peer_info_from(stats: NodeStats, state: NodeStateStats) -> PeerInfo {
    PeerInfo {
        id: stats.id,
        addr_name: stats.addr_name,
        addr_bind: stats.addr_bind.addr.is_valid().then(|| stats.addr_bind.to_string()),
        addr_local: stats.addr_local,
        network: stats.network.name(),
        mapped_as: stats.mapped_as,
        services: state.their_services.0,
        relay_txs: state.relay_txs,
        verified_proregtx_hash: stats.verified_proreg_hash,
        verified_pubkey_hash: stats.verified_pubkey_hash,
        last_send: stats.last_send,
        last_recv: stats.last_recv,
        last_tx_time: stats.last_tx_time,
        last_block_time: stats.last_block_time,
        send_bytes: stats.send_bytes,
        recv_bytes: stats.recv_bytes,
        connected: stats.connected,
        time_offset: stats.time_offset,
        last_ping_us: stats.last_ping_us,
        min_ping_us: stats.min_ping_us,
        ping_wait_us: state.ping_wait_us,
        version: stats.version,
        clean_subver: stats.clean_subver,
        inbound: stats.inbound,
        bip152_highbandwidth_to: stats.bip152_highbandwidth_to,
        bip152_highbandwidth_from: stats.bip152_highbandwidth_from,
        masternode_connection: stats.masternode_connection,
        misbehavior_score: state.misbehavior_score,
        starting_height: state.starting_height,
        sync_height: state.sync_height,
        common_height: state.common_height,
        heights_in_flight: state.heights_in_flight,
        addr_relay_enabled: state.addr_relay_enabled,
        addr_processed: state.addr_processed,
        addr_rate_limited: state.addr_rate_limited,
        permissions: permission_strings(stats.permissions),
        send_bytes_per_msg: by_name(stats.send_bytes_per_msg),
        recv_bytes_per_msg: by_name(stats.recv_bytes_per_msg),
        conn_type: stats.conn_type.as_str(),
        transport_type: stats.transport.kind.as_str(),
        // `HexStr` of the session ID's bytes, in order.
        session_id: stats
            .transport
            .session_id
            .map(|id| id.iter().map(|b| format!("{b:02x}")).collect())
            .unwrap_or_default(),
    }
}

/// A message-type map keyed by the types' names.
fn by_name(map: HashMap<CommandString, u64>) -> BTreeMap<String, u64> {
    map.into_iter().map(|(msg, bytes)| (msg.to_string(), bytes)).collect()
}

/// Core's command table (`tableRPC`) with the commands registered so far. Core registers them
/// whether or not `-server` starts the servers.
#[expect(
    clippy::too_many_arguments,
    reason = "Core's NodeContext as the commands reach it: the control parts, the mocking pair, one slot per part init builds later, the deprecation flag and the network's data directory"
)]
pub fn command_table(
    params: &ChainSetup,
    logger: &Arc<Logger>,
    shutdown: &Arc<ShutdownState>,
    clock: &Arc<MockClock>,
    mock: (bool, &Scheduler),
    chain: &Arc<ChainSlot>,
    net: &Arc<NetSlot>,
    sync: &Arc<SyncSlot>,
    warnings: &Arc<Warnings>,
    deprecated: DeprecatedRpc,
    data_dir_net: PathBuf,
) -> Arc<RpcServer> {
    let control = Control {
        logger: Arc::clone(logger),
        shutdown: Arc::clone(shutdown),
        clock: Arc::clone(clock),
        // Core's `GetStartupTime`: the system time at static initialization, before any mock
        // time is set, which init follows closely. `uptime` subtracts it from the mockable clock.
        startup_time: dash_clock::SystemClock.node_now().secs(),
        mockable: mock.0,
        scheduler: mock.1.clone(),
        chain: Arc::clone(chain),
    };
    let table = Arc::new(RpcServer::new(Arc::new(control)));
    *chain.server.lock().unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(&table);
    dashd_rpc::commands::register_control_commands(&table);
    dashd_rpc::blockchain::register_blockchain_commands(
        &table,
        Arc::clone(chain) as _,
        TxJsonParams {
            key_io: key_io_params(params),
            service: deprecated.service,
        },
    );
    dashd_rpc::blockchain::register_wait_commands(&table, Arc::clone(&chain.block_change));
    dashd_rpc::blockchain::register_dump_commands(&table, Arc::clone(chain) as _, data_dir_net);
    dashd_rpc::blockchain::register_chain_state_commands(
        &table,
        Arc::clone(chain) as _,
        ChainStateParams {
            chain: params
                .devnet_name
                .clone()
                .unwrap_or_else(|| params.network.network_id().to_owned()),
            pow_target_spacing: params.consensus.pow_target_spacing,
            softforks: deprecated.softforks,
            key_io: key_io_params(params),
            warnings: Arc::clone(warnings) as _,
        },
    );
    dashd_rpc::mining::register_mining_commands(
        &table,
        Arc::clone(chain) as _,
        Arc::clone(chain) as _,
        key_io_params(params),
    );
    dashd_rpc::mining::register_mining_info_commands(
        &table,
        Arc::clone(chain) as _,
        Arc::clone(chain) as _,
        MiningInfoParams {
            network_id: params.network.network_id().to_owned(),
            difficulty_adjustment_interval: params.consensus.pow_target_timespan
                / params.consensus.pow_target_spacing,
            warnings: Arc::clone(warnings) as _,
        },
    );
    dashd_rpc::util::register_util_commands(&table, key_io_params(params));
    dashd_rpc::blockchain::register_specialtx_commands(
        &table,
        Arc::clone(chain) as _,
        TxJsonParams {
            key_io: key_io_params(params),
            service: deprecated.service,
        },
    );
    dashd_rpc::blockchain::register_txoutproof_commands(&table, Arc::clone(chain) as _);
    dashd_rpc::blockchain::register_scan_commands(
        &table,
        Arc::clone(chain) as _,
        key_io_params(params),
    );
    dashd_rpc::peers::register_net_commands(
        &table,
        Arc::clone(net) as _,
        Arc::clone(warnings) as _,
        deprecated.banscore,
    );
    dashd_rpc::dash::register_dash_commands(&table, Arc::clone(sync) as _);
    table
}

/// The `-deprecatedrpc` methods the commands read: Core's `IsDeprecatedRPCEnabled`.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeprecatedRpc {
    /// `getpeerinfo`'s `banscore`.
    pub banscore: bool,
    /// `getblockchaininfo`'s `softforks`.
    pub softforks: bool,
    /// A ProTx's `service` and Evo platform ports (`IsServiceDeprecatedRPCEnabled`).
    pub service: bool,
}

/// The running servers.
pub struct Servers {
    table: Arc<RpcServer>,
    http: HttpServer,
    rpc: Arc<HttpRpc>,
}

impl Servers {
    /// Core's `AppInitServers`: the HTTP server, the command table, the JSON-RPC endpoint, then
    /// the HTTP server's threads. `InitMessage` sets the warmup status from here on.
    // Core: init.cpp:983-997, 1634-1638
    pub fn start(
        ui: &Ui,
        args: &ArgsManager,
        table: &Arc<RpcServer>,
        shutdown: &Arc<ShutdownState>,
    ) -> Result<Servers, InitFailure> {
        let warmup = Arc::clone(table);
        ui.connect_init_message(move |msg| warmup.set_warmup_status(msg));
        let resolve = |name: &str, allow_lookup: bool| {
            host::get_addr_info(name, allow_lookup).first().map(|(ip, _)| *ip)
        };
        let requested = Arc::clone(shutdown);
        let mut http =
            HttpServer::init(args, &resolve, Arc::new(move || requested.shutdown_requested()))
                .map_err(|e| {
                    if let InitError::AllowIp(text) = e {
                        // `ThreadSafeMessageBox(MSG_ERROR)` without a GUI.
                        ui.init_error(&text);
                    }
                    InitFailure::Error(HTTP_START_FAILED.to_owned())
                })?;
        table.start();
        let rpc = HttpRpc::start(Arc::clone(table), &http, args).map_err(|_| {
            table.interrupt();
            InitFailure::Error(HTTP_START_FAILED.to_owned())
        })?;
        http.start();
        Ok(Servers {
            table: Arc::clone(table),
            http,
            rpc,
        })
    }

    /// Core's `SetRPCWarmupFinished`.
    pub fn set_warmup_finished(&self) {
        self.table.set_warmup_finished();
    }

    /// Core's `InterruptHTTPServer`, `InterruptHTTPRPC`, `InterruptRPC`.
    // Core: init.cpp:284-286
    pub fn interrupt(&self) {
        self.http.interrupt();
        self.rpc.interrupt();
        self.table.interrupt();
    }

    /// Core's `StopHTTPRPC`, `StopRPC` (which removes the cookie), `StopHTTPServer`.
    // Core: init.cpp:329-332
    pub fn stop(&mut self) {
        self.rpc.stop(&self.http);
        tracing::debug!(target: "rpc", "Stopping RPC");
        self.rpc.delete_cookie();
        self.table.stop();
        self.http.stop();
    }
}

#[cfg(test)]
mod tests {
    use dash_consensus::validation::BlockValidationResult;
    use dash_consensus::{BlockValidationError, primitives::Header};
    use dash_params::{ChainParamsBuilder, Network as CoreNetwork, Release};

    use super::*;

    /// "completed" follows a started walk whether it succeeded or failed (an interruption
    /// unwinds through Core's timer), and nothing follows a walk that never started.
    // Core: rpc/blockchain.cpp:3184; logging/timer.h
    #[test]
    fn the_dump_timer_completes_after_a_started_walk_whatever_its_result() {
        let started = || Mutex::new(Some(("dump".to_owned(), Instant::now())));
        for result in [Ok(()), Err(TxOutSetError::Interrupted), Err(TxOutSetError::Unwritable)] {
            let timer = started();
            let mut lines = Vec::new();
            assert_eq!(finish_dump_timer(&timer, result, |l| lines.push(l)), result);
            assert_eq!(lines.len(), 1, "{result:?}");
            assert!(lines[0].starts_with("dump completed ("), "{}", lines[0]);
            assert!(timer.lock().unwrap().is_none(), "taken");
        }
        let mut lines = Vec::new();
        let never = Mutex::new(None);
        let failed: Result<(), _> = Err(TxOutSetError::Unreadable);
        assert_eq!(finish_dump_timer(&never, failed, |l| lines.push(l)), failed);
        assert!(lines.is_empty(), "no walk, no line");
    }

    /// The catcher keeps each `BlockState` as `BIP22ValidationResult` reads it: the invalid
    /// state whole (its reason and debug message) and the error's message (review r1 m1).
    #[test]
    fn the_catcher_keeps_each_state_of_its_block() {
        let p = ChainParamsBuilder::new(CoreNetwork::Regtest, Release::V24_0).build().unwrap();
        let block = dashd_chainstate::chain::genesis_block(&Arc::new(p));
        let invalid = BlockValidationError::new(BlockValidationResult::Consensus, "bad-cb-amount")
            .with_debug("coinbase pays too much");
        for (state, caught) in [
            (BlockState::Valid, ValidationState::Valid),
            (BlockState::Invalid(invalid.clone()), ValidationState::Invalid(invalid)),
            (
                BlockState::Error("System error: disk full".into()),
                ValidationState::Error("System error: disk full".into()),
            ),
        ] {
            let sc = Arc::new(StateCatcher::new(block.hash()));
            Catcher(Arc::clone(&sc)).block_checked(&block, &state, false);
            assert_eq!(sc.state(), Some(caught), "{state:?}");
        }
        // Another block's check is not this catcher's.
        let other = dash_consensus::Block::new(
            Header::new(dash_primitives::BlockHeader {
                nonce: 1,
                ..*block.header().raw()
            }),
            block.txs().to_vec(),
        );
        let sc = Arc::new(StateCatcher::new(block.hash()));
        Catcher(Arc::clone(&sc)).block_checked(&other, &BlockState::Valid, false);
        assert_eq!(sc.state(), None);
    }

    /// `getnetworkinfo`'s fixed fields (BL-391 D-rows) are Core's for the only state a node can
    /// reach while the options that would change them are refused: the mempool's default fees
    /// (rc.3 shows `0.00001000` for both, as the mempool options default to), the platform's
    /// socket events. Its proxies are the configured ones (P8a, `rpc_diff`'s proxy configs).
    #[test]
    fn the_fixed_network_fields_are_cores_while_their_options_are_refused() {
        // Core: policy/policy.h:35, :49
        assert_eq!((DEFAULT_MIN_RELAY_TX_FEE, DEFAULT_INCREMENTAL_RELAY_FEE), (1000, 1000));
        for option in ["-minrelaytxfee=0.0001", "-incrementalrelayfee=0.0001", "-socketevents=poll"]
        {
            let args = crate::init::test_args(&[option]);
            assert!(crate::init::unsupported_option(&args).unwrap().is_some(), "{option}");
        }
    }

    /// A coins cache over an empty probe engine, at block 7.
    fn coins_cell() -> Arc<Mutex<CoinsCache<dashd_store::MemoryEngine>>> {
        use dashd_chainstate::coins::{CoinsBudget, Ratio, Salt};
        use dashd_chainstate::store::{
            CHAINSTATE_SCHEMA, GlobalFailStop, GuardedEngine, RecordingSink,
        };

        let engine = dashd_store::MemoryEngine::new(&CHAINSTATE_SCHEMA);
        let sink = Arc::new(RecordingSink::default());
        let db = Arc::new(GuardedEngine::new(engine, GlobalFailStop::default(), sink));
        let budget = CoinsBudget::new(2 << 20, 0, Ratio::HALF, 0);
        let mut cache = CoinsCache::new(db, budget, Salt::fixed(7), None);
        cache.set_best_block(Some(BlockRef {
            id: BlockId(7),
            hash: BlockHash::from([7; 32]),
            height: 7,
        }));
        Arc::new(Mutex::new(cache))
    }

    /// An outpoint the cache has no coin for.
    fn no_coin() -> OutPoint {
        OutPoint {
            hash: dash_primitives::TxHash::from([1; 32]),
            index: 0,
        }
    }

    /// gettxout's coins read on a cell a panicked V left poisoned answers D9's fatal error in
    /// every build, without the debug assert (RP1a-2b r1 Sol n-2).
    #[test]
    fn a_poisoned_coins_cell_answers_aborted() {
        let cell = coins_cell();
        let read = coins_read(Arc::clone(&cell));
        let (coin, best) = read(&no_coin()).expect("a free cell reads");
        assert_eq!((coin, best.height), (None, 7));
        let held = Arc::clone(&cell);
        let panicked = std::thread::spawn(move || {
            let _coins = held.lock().unwrap();
            panic!("V panics holding the coins cache");
        });
        assert!(panicked.join().is_err());
        let error = dashd_rpc::protocol::RpcError::from(read(&no_coin()).unwrap_err());
        assert_eq!(
            error.to_object().write(),
            r#"{"code":-20,"message":"Error: A fatal internal error occurred"}"#
        );
    }

    /// A cell held outside V's turn, which no holder does (each takes it on V's thread), trips
    /// the debug assert; a release build answers D9's fatal error rather than reading it.
    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "the coins cache is held outside V's turn")
    )]
    fn a_held_coins_cell_answers_aborted_in_release() {
        let cell = coins_cell();
        let read = coins_read(Arc::clone(&cell));
        let _held = cell.lock().unwrap();
        assert!(matches!(read(&no_coin()), Err(Aborted)));
    }

    /// BL-270: a store that lacks a block's EHF row rebuilds it from the block's data, and a
    /// block with neither is `GetForBlock`'s read failure, wherever it is on the walk.
    #[test]
    fn a_missing_ehf_row_is_rebuilt_from_data_or_fails_the_read() {
        use EhfRow::{Found, FromDisk, Unreadable};
        let read_failure = Err("failed-getehfforblock-read");
        for (rows, expected) in [
            (vec![Found], Ok(())),
            (vec![FromDisk, FromDisk, Found], Ok(())),
            // The walk ends at a stored row: older blocks are not read.
            (vec![FromDisk, Found, Unreadable], Ok(())),
            (vec![Unreadable, Found], read_failure),
            (vec![FromDisk, Unreadable, Found], read_failure),
            // Down past genesis, every block rebuilt.
            (vec![FromDisk, FromDisk], Ok(())),
            (vec![], Ok(())),
        ] {
            assert_eq!(ehf_signals_read(rows.clone()), expected, "{rows:?}");
        }
    }

    /// A block's row: stored once it was connected (failed since or not), else rebuilt from its
    /// data, else unreadable; below V20's height, always found (validator m-2).
    #[test]
    fn a_block_s_ehf_row_follows_its_validity_and_data() {
        let (tree, txs, scripts) =
            (Status::VALID_TREE, Status::VALID_TRANSACTIONS, Status::VALID_SCRIPTS);
        let data = Status::HAVE_DATA | Status::HAVE_UNDO;
        for (bits, v20_active, expected) in [
            (scripts | data, true, EhfRow::Found),
            // Invalidated after it was connected: Core's map stays.
            (scripts | data | Status::FAILED_VALID, true, EhfRow::Found),
            (scripts | data | Status::FAILED_CHILD, true, EhfRow::Found),
            (txs | Status::HAVE_DATA, true, EhfRow::FromDisk),
            (txs | Status::HAVE_DATA | Status::FAILED_VALID, true, EhfRow::FromDisk),
            (tree, true, EhfRow::Unreadable),
            (tree | Status::FAILED_VALID, true, EhfRow::Unreadable),
            (tree, false, EhfRow::Found),
            (txs | Status::HAVE_DATA, false, EhfRow::Found),
        ] {
            assert_eq!(ehf_row(Status(bits), v20_active), expected, "{bits:#x} {v20_active}");
        }
    }
}
