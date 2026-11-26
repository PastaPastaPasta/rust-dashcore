//! Init's Step 8 indexes (IX1 e2, d3): the txindex, the basic block filter index and the coin
//! statistics index on `dashd-indexes`' host, their lookup handles and Core's `GetTransaction`
//! over the txindex.
//!
//! The host registers at the end of step 7, so the handle exists for step 7c's services, and
//! opens at Step 8, or once the `loadblk` thread has finished a `-reindex-chainstate` import
//! (Core's `g_indexes_ready_to_sync`). It is interrupted with the node, and stops between
//! shutdown's two chainstate flushes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use dash_chain::{ChainReader, IndexAccess};
use dash_consensus::ctx::{Deployment, DeploymentView};
use dash_consensus::params::genesis_block;
use dash_consensus::subsidy::{SubsidyParams, genesis_reward};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_params::ChainSetup;
use dashd_chainstate::chain::BlockFiles;
use dashd_chainstate::store::{AbortSink, GlobalFailStop, GuardedEngine};
use dashd_chainstate::subscriber::{BlockSource, DriverConfig, RowSchema};
use dashd_chainstate::tuning::EngineTuning;
use dashd_indexes::blockfilter::data::{BLOCKFILTER_DB, BLOCKFILTER_ROWS};
use dashd_indexes::blockfilter::{BlockFilterData, FilterFiles, StoredFilters};
use dashd_indexes::coinstats::data::{COINSTATS_DB, COINSTATS_ROWS};
use dashd_indexes::coinstats::{CoinStatsData, StoredCoinStats};
use dashd_indexes::host::{HostError, IndexHost, IndexReader, IndexSpec};
use dashd_indexes::txindex::{StoredTxIndex, TXINDEX_DB, TXINDEX_ROWS, TxFiles, TxIndexData};
use dashd_indexes::{
    BlockRead, BlockRef, CoinStatsReader, FilterIndexReader, IndexError, IndexStatus, IndexSummary,
    MempoolLookup, NodeBlockSource, NodeSubsidyInputs, NodeTxPositions, RetrievedTx, SubsidyInputs,
    TxIndexReader, Txid,
};
use dashd_store::flatfile::{BlockFileReader, OsFs};
use dashd_store::{DbSchema, RocksConfig, RocksEngine};

use crate::InitFailure;

/// The txindex as node services hold it: `None` with `-txindex=0`. Before the index opens it
/// serves the retained rows; [`IndexReader::consumed_tip`] gives how far it has indexed.
pub type TxIndexHandle = Arc<IndexReader<dyn TxIndexReader>>;

/// The basic block filter index as the network serves it.
pub type FilterIndexHandle = Arc<IndexReader<dyn FilterIndexReader>>;

/// The coin statistics index as `gettxoutsetinfo` reads it.
pub type CoinStatsHandle = Arc<IndexReader<dyn CoinStatsReader>>;

/// The catch-up's capped commit every this many blocks, besides the host's 30 s locator tick.
const CADENCE: u32 = 1000;

/// Core's `GetTransaction(nullptr, mempool, hash, …)`: the mempool, then the txindex; no block
/// is read. A txindex read failure is a miss, as Core's logged `FindTx` failure is.
// Core: v24.0.0 src/node/transaction.cpp:126-156 GetTransaction
pub fn get_transaction(
    mempool: Option<&MempoolLookup<'_>>,
    txindex: Option<&TxIndexHandle>,
    txid: &Txid,
) -> Option<RetrievedTx> {
    let txindex = txindex.map(|t| &**t as &dyn TxIndexReader);
    dashd_indexes::get_transaction(None, mempool, txindex, &NoBlock, txid).ok().flatten()
}

/// No block is named, so none is read.
struct NoBlock;

impl BlockRead for NoBlock {
    fn read_block(&self, block: &BlockRef) -> Result<Arc<dash_consensus::Block>, IndexError> {
        Err(IndexError::Io(format!("no block read for {}", block.hash)))
    }
}

/// Core's `GetOptions(nCacheSize)` for an index database: half the cache for blocks, a quarter
/// per write buffer, two of them.
// Core: v24.0.0 src/dbwrapper.cpp:113-117 GetOptions
fn index_db_config(cache: u64) -> RocksConfig {
    let cache = usize::try_from(cache).unwrap_or(usize::MAX).max(1 << 20);
    RocksConfig {
        block_cache_bytes: cache / 2,
        write_buffer_bytes: cache / 4,
        max_write_buffers: 2,
        total_write_buffer_bytes: cache / 2,
        bloom_bits_per_key: 10.0,
        ..RocksConfig::default()
    }
}

/// What the indexes read and write in the node.
#[derive(Clone)]
pub struct IndexParts {
    /// The network's data directory.
    pub data_dir: PathBuf,
    pub blocks_dir: PathBuf,
    /// The chainstate's latch and the node's `AbortNode`: each index's engine fails stop with
    /// the node.
    pub latch: GlobalFailStop,
    pub sink: Arc<dyn AbortSink>,
    /// V's block index and a reader of V's block files.
    pub index: Arc<dyn IndexAccess>,
    pub files: Arc<dyn BlockFiles + Sync>,
    pub driver: DriverConfig,
    /// V's reader and the chain's parameters, for coinstats' `GetBlockSubsidy`.
    pub chain: ChainReader,
    pub params: Arc<ChainSetup>,
}

impl IndexParts {
    /// An index's own guarded engine on the node's latch (c2-fw B2).
    fn engine(
        &self,
        dir: &Path,
        schema: &'static DbSchema,
        config: &RocksConfig,
    ) -> Result<Arc<GuardedEngine<RocksEngine>>, IndexError> {
        let (engine, _) = RocksEngine::open(dir, schema, config).map_err(store_err)?;
        Ok(Arc::new(GuardedEngine::new(engine, self.latch.clone(), Arc::clone(&self.sink))))
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        Arc::new(NodeBlockSource::new(Arc::clone(&self.index), Arc::clone(&self.files)))
    }
}

/// The indexes Step 8 starts, each with its DB cache: `None` when off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IndexCaches {
    pub txindex: Option<u64>,
    pub filters: Option<u64>,
    pub coinstats: Option<u64>,
}

/// Core's fixed coinstats DB cache, not a `CalculateCacheSizes` share.
// Core: v24.0.2 src/init.cpp:2316
pub const COINSTATS_CACHE: u64 = 0;

/// The registered indexes' handles.
#[derive(Default)]
pub struct IndexHandles {
    pub txindex: Option<TxIndexHandle>,
    pub filters: Option<FilterIndexHandle>,
    pub coinstats: Option<CoinStatsHandle>,
}

/// The txindex's [`IndexSpec`]: its RocksDB at `<datadir>/indexes/txindex`, on its own guarded
/// engine (c2-fw B2), its rows verified against the block files.
struct TxIndexSpec {
    dir: PathBuf,
    config: RocksConfig,
    parts: IndexParts,
    tx_files: Arc<dyn TxFiles>,
}

fn store_err(e: dashd_store::StoreError) -> IndexError {
    match e.kind() {
        Some(dashd_store::ErrorKind::Corruption) => IndexError::Corrupt(e.to_string()),
        _ => IndexError::Io(e.to_string()),
    }
}

impl IndexSpec for TxIndexSpec {
    type Data = TxIndexData<RocksEngine>;
    type ReadOnly = RocksEngine;
    type Reader = dyn TxIndexReader;

    fn name(&self) -> &'static str {
        "txindex"
    }

    // Core: v24.0.0 src/index/txindex.cpp:47-48 (TxIndex::DB)
    fn db_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    fn rows(&self) -> &'static RowSchema {
        &TXINDEX_ROWS
    }

    fn open_read_only(&self) -> Result<Option<Arc<RocksEngine>>, IndexError> {
        let db = RocksEngine::open_read_only(&self.dir, &TXINDEX_DB).map_err(store_err)?;
        Ok(db.map(Arc::new))
    }

    fn read_only_reader(
        &self,
        db: Arc<RocksEngine>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn TxIndexReader> {
        Arc::new(StoredTxIndex::new(db, Arc::clone(&self.tx_files), status))
    }

    fn engine(&self) -> Result<Arc<GuardedEngine<RocksEngine>>, IndexError> {
        self.parts.engine(&self.dir, &TXINDEX_DB, &self.config)
    }

    // b1 n4: on the driver's engine, just before its open.
    fn data(
        &self,
        engine: &Arc<GuardedEngine<RocksEngine>>,
    ) -> Result<TxIndexData<RocksEngine>, IndexError> {
        let positions = Arc::new(NodeTxPositions::new(Arc::clone(&self.parts.index)));
        TxIndexData::new(&**engine, positions)
    }

    fn reader(
        &self,
        engine: Arc<GuardedEngine<RocksEngine>>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn TxIndexReader> {
        Arc::new(StoredTxIndex::new(engine, Arc::clone(&self.tx_files), status))
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        self.parts.blocks()
    }

    fn config(&self) -> DriverConfig {
        self.parts.driver
    }
}

/// The basic block filter index's [`IndexSpec`]: its `fltr?????.dat` files in
/// `<datadir>/indexes/blockfilter/basic` and its RocksDB in `db` there, on its own guarded engine.
struct FilterIndexSpec {
    dir: PathBuf,
    config: RocksConfig,
    parts: IndexParts,
    fs: Arc<OsFs>,
}

impl IndexSpec for FilterIndexSpec {
    type Data = BlockFilterData<RocksEngine>;
    type ReadOnly = RocksEngine;
    type Reader = dyn FilterIndexReader;

    // Core: v24.0.0 src/index/blockfilterindex.cpp:108
    fn name(&self) -> &'static str {
        "basic block filter index"
    }

    // Core: v24.0.0 src/index/blockfilterindex.cpp:114-117
    fn db_dir(&self) -> PathBuf {
        self.dir.join("db")
    }

    // Core: v24.0.0 src/index/blockfilterindex.cpp:114-129 (Core keeps `basic` and its files)
    fn wipe_paths(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name().and_then(|name| name.to_str()).is_some_and(|name| {
                    name.len() == 13 && name.starts_with("fltr") && name.ends_with(".dat")
                })
            })
            .collect()
    }

    fn rows(&self) -> &'static RowSchema {
        &BLOCKFILTER_ROWS
    }

    fn open_read_only(&self) -> Result<Option<Arc<RocksEngine>>, IndexError> {
        let db = RocksEngine::open_read_only(&self.db_dir(), &BLOCKFILTER_DB).map_err(store_err)?;
        Ok(db.map(Arc::new))
    }

    fn read_only_reader(
        &self,
        db: Arc<RocksEngine>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn FilterIndexReader> {
        let files = FilterFiles::new(Arc::new(OsFs::read_only()), self.dir.clone());
        Arc::new(StoredFilters::new(db, files, status))
    }

    fn engine(&self) -> Result<Arc<GuardedEngine<RocksEngine>>, IndexError> {
        self.parts.engine(&self.db_dir(), &BLOCKFILTER_DB, &self.config)
    }

    fn data(
        &self,
        engine: &Arc<GuardedEngine<RocksEngine>>,
    ) -> Result<BlockFilterData<RocksEngine>, IndexError> {
        BlockFilterData::new(&**engine, Arc::clone(&self.fs) as _, self.dir.clone())
    }

    fn reader(
        &self,
        engine: Arc<GuardedEngine<RocksEngine>>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn FilterIndexReader> {
        let files = FilterFiles::new(Arc::clone(&self.fs) as _, self.dir.clone());
        Arc::new(StoredFilters::new(engine, files, status))
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        self.parts.blocks()
    }

    fn config(&self) -> DriverConfig {
        self.parts.driver
    }
}

/// The coin statistics index's [`IndexSpec`]: its RocksDB at `<datadir>/indexes/coinstats/db`,
/// on its own guarded engine.
struct CoinStatsIndexSpec {
    dir: PathBuf,
    config: RocksConfig,
    parts: IndexParts,
    subsidies: Arc<dyn SubsidyInputs>,
}

impl IndexSpec for CoinStatsIndexSpec {
    type Data = CoinStatsData<RocksEngine>;
    type ReadOnly = RocksEngine;
    type Reader = dyn CoinStatsReader;

    // Core: v24.0.2 src/index/coinstatsindex.cpp:109
    fn name(&self) -> &'static str {
        "coinstatsindex"
    }

    // Core: v24.0.2 src/index/coinstatsindex.cpp:111-114
    fn db_dir(&self) -> PathBuf {
        self.dir.join("db")
    }

    fn rows(&self) -> &'static RowSchema {
        &COINSTATS_ROWS
    }

    fn open_read_only(&self) -> Result<Option<Arc<RocksEngine>>, IndexError> {
        let db = RocksEngine::open_read_only(&self.db_dir(), &COINSTATS_DB).map_err(store_err)?;
        Ok(db.map(Arc::new))
    }

    fn read_only_reader(
        &self,
        db: Arc<RocksEngine>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn CoinStatsReader> {
        Arc::new(StoredCoinStats::new(db, status))
    }

    fn engine(&self) -> Result<Arc<GuardedEngine<RocksEngine>>, IndexError> {
        self.parts.engine(&self.db_dir(), &COINSTATS_DB, &self.config)
    }

    fn data(
        &self,
        _: &Arc<GuardedEngine<RocksEngine>>,
    ) -> Result<CoinStatsData<RocksEngine>, IndexError> {
        Ok(CoinStatsData::new(Arc::clone(&self.subsidies)))
    }

    fn reader(
        &self,
        engine: Arc<GuardedEngine<RocksEngine>>,
        status: Arc<dyn IndexStatus>,
    ) -> Arc<dyn CoinStatsReader> {
        Arc::new(StoredCoinStats::new(engine, status))
    }

    fn blocks(&self) -> Arc<dyn BlockSource> {
        self.parts.blocks()
    }

    fn config(&self) -> DriverConfig {
        self.parts.driver
    }
}

/// V's block index as `GetBlockSubsidy` reads it from the coinstats thread: every node entry is
/// resident, and V20 is buried at its height.
struct SubsidyChain {
    reader: ChainReader,
    params: Arc<ChainSetup>,
}

impl SubsidyChain {
    fn entry(&self, id: BlockId) -> dash_chain::EntryView {
        self.reader.entry(id).unwrap_or_else(|| panic!("node entry {} is resident", id.0))
    }

    fn active_at_height(&self, height: u32, deployment: Deployment) -> bool {
        match deployment {
            Deployment::Buried(d) => height >= self.params.consensus.deployment_height(d).to_u32(),
            Deployment::Versionbits(_) => false,
        }
    }
}

impl ChainAncestry for SubsidyChain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        let e = self.entry(id);
        HeaderCtx {
            height: e.height,
            hash: e.hash,
            time: e.header.time(),
            bits: dash_types::Numeric::from_base(e.header.bits()),
            version: e.header.version(),
            mtp: e.mtp,
            chain_work: e.chain_work,
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        self.reader.ancestor(id, height)
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        self.entry(id).prev
    }
}

impl DeploymentView for SubsidyChain {
    fn active_after(&self, prev: Option<BlockId>, deployment: Deployment) -> bool {
        self.active_at_height(prev.map_or(0, |p| self.entry(p).height + 1), deployment)
    }

    fn active_at(&self, block: BlockId, deployment: Deployment) -> bool {
        self.active_at_height(self.entry(block).height, deployment)
    }
}

/// Coinstats' `GetBlockSubsidy` over V's reader and the network's subsidy parameters.
fn node_subsidies(parts: &IndexParts) -> Result<Arc<dyn SubsidyInputs>, InitFailure> {
    let failure = |e| InitFailure::Exception(format!("coinstatsindex subsidy parameters: {e:?}"));
    let reward = genesis_reward(&genesis_block(&parts.params)).map_err(failure)?;
    let params = SubsidyParams::from_chain_params(&parts.params, reward).map_err(failure)?;
    let chain = Arc::new(SubsidyChain {
        reader: parts.chain.clone(),
        params: Arc::clone(&parts.params),
    });
    Ok(Arc::new(NodeSubsidyInputs::new(Arc::clone(&chain) as _, chain, params)))
}

/// The driver's tuning for the node's indexes.
pub fn driver_config(tuning: &impl EngineTuning) -> DriverConfig {
    DriverConfig::from_tuning(tuning, None, CADENCE)
}

/// The node's indexes: the host, which shutdown takes to stop it, and a `-reindex-chainstate`
/// import's deferred open shares. The coinstats handle is kept here so `ChainSlot` can reach it
/// through the host `attach_indexes` already stores.
#[derive(Clone)]
pub struct NodeIndexes {
    host: Arc<Mutex<Option<IndexHost>>>,
    coinstats: Option<CoinStatsHandle>,
}

impl NodeIndexes {
    /// The host with the enabled indexes registered in Step 8's order, or `None` with none:
    /// `wipe` (Core's `fReindex`) removes each one's data first, and each handle serves the
    /// retained rows until its index opens.
    ///
    /// # Errors
    ///
    /// Core's `InitError` when a wipe or a read-only open fails.
    // Core: v24.0.2 src/init.cpp:2280-2281, :2308-2309, :2315-2316 (f_wipe = fReindex)
    pub fn register(
        hub: Arc<dashd_chainstate::flush::SubscriberHub>,
        wipe: bool,
        parts: IndexParts,
        caches: IndexCaches,
    ) -> Result<Option<(Self, IndexHandles)>, InitFailure> {
        if caches == IndexCaches::default() {
            return Ok(None);
        }
        let mut host = IndexHost::new(hub, Arc::clone(&parts.sink), wipe);
        let mut handles = IndexHandles::default();
        if let Some(cache) = caches.txindex {
            let spec = TxIndexSpec {
                dir: txindex_dir(&parts.data_dir),
                config: index_db_config(cache),
                tx_files: Arc::new(BlockFileReader::open_read_only(&parts.blocks_dir)),
                parts: parts.clone(),
            };
            handles.txindex = Some(host.register(spec).map_err(init_failure)?);
        }
        if let Some(cache) = caches.filters {
            let spec = FilterIndexSpec {
                dir: filter_index_dir(&parts.data_dir),
                config: index_db_config(cache),
                fs: Arc::new(OsFs::new()),
                parts: parts.clone(),
            };
            handles.filters = Some(host.register(spec).map_err(init_failure)?);
        }
        if let Some(cache) = caches.coinstats {
            let spec = CoinStatsIndexSpec {
                dir: coinstats_dir(&parts.data_dir),
                config: index_db_config(cache),
                subsidies: node_subsidies(&parts)?,
                parts,
            };
            handles.coinstats = Some(host.register(spec).map_err(init_failure)?);
        }
        Ok(Some((
            Self {
                host: Arc::new(Mutex::new(Some(host))),
                coinstats: handles.coinstats.clone(),
            },
            handles,
        )))
    }

    /// `g_coin_stats_index`: the handle registered with this host, `None` without `-coinstatsindex`.
    pub fn coinstats(&self) -> Option<CoinStatsHandle> {
        self.coinstats.clone()
    }

    fn host(&self) -> std::sync::MutexGuard<'_, Option<IndexHost>> {
        self.host.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Core's `Start` for each index, on a thread that holds no node lock and submits no flush.
    ///
    /// # Errors
    ///
    /// Core's `InitError`; [`InitFailure::Reported`] for one the fail-stop sink has.
    // Core: v24.0.0 src/init.cpp:2277-2318
    pub fn open(&self) -> Result<(), InitFailure> {
        self.host().as_mut().map_or(Ok(()), IndexHost::open_all).map_err(init_failure)
    }

    /// The open after a `-reindex-chainstate` import, on the `loadblk` thread: init has ended,
    /// so a failure is the node's `AbortNode`.
    pub fn open_after_import(&self, abort: &dyn AbortSink) {
        if let Err(InitFailure::Error(message)) = self.open() {
            abort.abort_node(&message, None);
        }
    }

    /// Each registered index's `GetSummary()`, in registration order; none once stopped. It
    /// takes the host's lock, so it waits for an open in progress.
    // Core: v24.0.0 src/index/base.cpp:531-537 (GetSummary)
    pub fn summaries(&self) -> Vec<IndexSummary> {
        self.host().as_ref().map_or_else(Vec::new, IndexHost::summaries)
    }

    // Core: v24.0.0 src/init.cpp:296-310 (Interrupt)
    pub fn interrupt(&self) {
        if let Some(host) = &*self.host() {
            host.interrupt();
        }
    }

    /// Joins the index threads and stops their drivers: their final commits.
    // Core: v24.0.0 src/init.cpp:421-441 (Shutdown)
    pub fn stop(&self) {
        let host = self.host().take();
        if let Some(host) = host {
            host.stop();
        }
    }
}

/// Core's `gArgs.GetDataDirNet() / "indexes" / "txindex"`.
// Core: v24.0.0 src/index/txindex.cpp:47-48
pub fn txindex_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes").join("txindex")
}

/// Core's `gArgs.GetDataDirNet() / "indexes" / "blockfilter" / "basic"`: the filter files, and
/// the DB in `db`.
// Core: v24.0.0 src/index/blockfilterindex.cpp:114
pub fn filter_index_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes").join("blockfilter").join("basic")
}

/// Core's `gArgs.GetDataDirNet() / "indexes" / "coinstats"`: the DB is in `db`.
// Core: v24.0.2 src/index/coinstatsindex.cpp:111
pub fn coinstats_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("indexes").join("coinstats")
}

/// A host error as init reports it: once, if the sink already has it.
fn init_failure(e: HostError) -> InitFailure {
    if e.reported() {
        InitFailure::Reported
    } else {
        InitFailure::Error(e.to_string())
    }
}
