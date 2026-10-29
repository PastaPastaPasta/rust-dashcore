//! Startup (§7.3): [`open_block_index`], which is `LoadGenesisBlock` for an empty index, and for
//! SB4's loaded index Core's load rules ([`NodeBlockIndex::load`]), its block-file check and its
//! datadir checks, in Core's order; and `LoadChainTip`.

use std::collections::BTreeSet;
use std::sync::Arc;

use dash_chain::{ChainConfig, FlatFilePos, HeaderChain, LoadError, Services, Status};
use dash_clock::{Clock, SystemClock, format_iso8601_date_time};
use dash_consensus::primitives::{Header, Tx};
use dash_consensus::{Block, BlockId};
use dash_params::{ChainParams, ChainTxData};
use dash_primitives::BlockHash;

use super::deployments::ChainDeployments;
use super::index::{NodeBlockIndex, entry};
use super::seams::{BlockFiles, Fatal, ShutdownFlag};
use crate::connect::{BlockAt, EvoHooks, g1_capability, g1_rule, refusal_text};

/// Core's `LoadChainstate` failures that init reports with `InitError`, never `AbortNode`
/// (`node/chainstate.h` `ChainstateLoadStatus`, D48).
///
/// With shutdown requested, Core's init exits quietly on any of them, without the reindex
/// question or its error (`init.cpp:2119`, `:2143`): `Store` and `Failure` too, not only
/// `Interrupted`. The caller reads its flag first.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoadFailure {
    /// `FAILURE_INCOMPATIBLE_DB`, with Core's message: the datadir is another chain's.
    #[error("{0}")]
    Incompatible(&'static str),
    /// `FAILURE` for a false `LoadBlockIndex`: Core's [`LOAD_BLOCK_DB_FAILED`]. Here a block file
    /// that holds a block's data does not open (BL-127), which its implementation has logged.
    #[error("{LOAD_BLOCK_DB_FAILED}")]
    LoadingBlockDb,
    /// `INTERRUPTED`: the load stopped, or failed, with shutdown requested. Init logs only
    /// "Shutdown requested. Exiting.".
    #[error("the block index load was interrupted")]
    Interrupted,
    /// `FAILURE` for a false `LoadGenesisBlock`: Core's [`INIT_BLOCK_DB_FAILED`]. `cause` is the
    /// line Core logs before it, when V has not logged it already.
    #[error("{INIT_BLOCK_DB_FAILED}")]
    Failure {
        cause: Option<String>,
    },
    /// A store or I/O error inside the load, which Core throws and init catches around
    /// `LoadChainstate`: Core's [`OPEN_BLOCK_DB_FAILED`], with `cause` logged (`init.cpp`).
    #[error("{OPEN_BLOCK_DB_FAILED}")]
    Store {
        cause: String,
    },
    /// The readiness guard's refusal at startup (CH3 §2.17, CH3a D70): a block this build's
    /// incomplete `EvoHooks` cannot judge is already in the chainstate. The text is the
    /// connector's refusal.
    #[error("{0}")]
    NotReady(String),
}

/// Core's init error for a false `LoadBlockIndex` (`node/chainstate.cpp:183-185`).
pub const LOAD_BLOCK_DB_FAILED: &str = "Error loading block database";

/// Core's init error for an exception out of `LoadChainstate`, its cause logged.
pub const OPEN_BLOCK_DB_FAILED: &str = "Error opening block database";

/// Core's init error for a false `LoadGenesisBlock` (`node/chainstate.cpp:210-211`).
pub const INIT_BLOCK_DB_FAILED: &str = "Error initializing block database";

/// Core's init error for a loaded index without the network's genesis block
/// (`node/chainstate.cpp:188-193`).
pub const WRONG_NETWORK_DATADIR: &str =
    "Incorrect or no genesis block found. Wrong datadir for network?";

/// Core's init error for a loaded devnet index without this devnet's genesis block
/// (`node/chainstate.cpp:195-198`).
pub const WRONG_DEVNET_DATADIR: &str =
    "Incorrect or no devnet genesis block found. Wrong datadir for devnet specified?";

/// A block index record as SB4's store reads it: its key's hash and value, or the read's error.
pub type StoredRecord = Result<(BlockHash, Vec<u8>), LoadError>;

/// The block index [`open_block_index`] opens (ND1's step 7).
#[expect(clippy::large_enum_variant, reason = "one value, at init")]
pub enum LoadedIndex<'a> {
    /// An empty store: a new chain's config and services, for `LoadGenesisBlock`.
    Empty {
        cfg: ChainConfig,
        svc: Services,
    },
    /// The store's index records, in its key order, with a new chain's config and services:
    /// [`open_block_index`] loads them with SB4's [`HeaderChain::load_records`] (D54), on
    /// `pool`, polling for shutdown before each record (Core's `LoadBlockIndexGuts`).
    Stored {
        cfg: ChainConfig,
        svc: Services,
        records: Box<dyn Iterator<Item = StoredRecord> + 'a>,
        pool: &'a rayon::ThreadPool,
    },
    /// A chain [`HeaderChain::load_records`] loaded: Core's `!BlockIndex().empty()`. Never one
    /// from [`HeaderChain::load`], whose network check comes before Core's block-file check.
    Loaded(HeaderChain),
}

// Core: node/chainstate.cpp:177-198, :210-212 (LoadChainstate)
/// V's chain and tables from the block index `found`, the entry ND1 calls (§7.3), in Core's
/// order (D53, D54). Stored records are first loaded structurally, as Core's
/// `LoadBlockIndexGuts`; an empty store is an empty index. A loaded index then gets Core's load
/// rules ([`NodeBlockIndex::load`]) and its block-file check (BL-127), each stopping when
/// `shutdown` is requested; then it must hold the
/// network's genesis block and, on a devnet, this devnet's genesis block (all devnets share
/// block 0), or the datadir is another chain's. `LoadGenesisBlock` then returns early, so its
/// devnet step never repeats. An empty index gets [`load_genesis_block`], whose devnet step V
/// runs first ([`start`]). Core's `-prune` state check between them (`:200-204`) is CH8's
/// (BL-129, D49).
///
/// [`start`]: super::start
///
/// # Errors
///
/// [`LoadFailure::LoadingBlockDb`] for a stored record SB4's load refuses, or a block file that
/// does not open, [`LoadFailure::Interrupted`] when shutdown is requested before or during the
/// load, [`LoadFailure::Store`] when a record cannot be read,
/// [`LoadFailure::Incompatible`] for another chain's datadir, and [`LoadFailure::Failure`] when
/// the genesis block cannot be written.
pub fn open_block_index(
    params: Arc<ChainParams>,
    found: LoadedIndex,
    files: &mut dyn BlockFiles,
    shutdown: &dyn ShutdownFlag,
) -> Result<(HeaderChain, NodeBlockIndex), LoadFailure> {
    // :177
    if shutdown.shutdown_requested() {
        return Err(LoadFailure::Interrupted);
    }
    // :183-185: `LoadBlockIndex`, whose failure with shutdown requested is INTERRUPTED.
    let failed = || {
        if shutdown.shutdown_requested() {
            LoadFailure::Interrupted
        } else {
            LoadFailure::LoadingBlockDb
        }
    };
    let chain = match found {
        LoadedIndex::Loaded(chain) => chain,
        LoadedIndex::Empty {
            cfg,
            svc,
        } => return empty_index(params, cfg, svc, files),
        LoadedIndex::Stored {
            cfg,
            svc,
            records,
            pool,
        } => {
            let p = params.clone();
            let (c, s) = (cfg.clone(), svc.clone());
            match HeaderChain::load_records(p, c, s, records, pool, || {
                shutdown.shutdown_requested()
            }) {
                Ok(chain) => chain,
                Err(LoadError::Empty) => return empty_index(params, cfg, svc, files),
                // txdb.cpp:307-309
                Err(LoadError::Interrupted) => return Err(LoadFailure::Interrupted),
                // A store that cannot read throws; init catches it (`init.cpp`).
                Err(LoadError::Read(e)) if !e.is_corrupt() => {
                    return Err(LoadFailure::Store {
                        cause: e.to_string(),
                    });
                }
                // `LoadBlockIndexGuts`' false, its cause logged (`txdb.cpp:300-344`).
                Err(e) => {
                    tracing::error!("LoadBlockIndexGuts: {e}");
                    return Err(failed());
                }
            }
        }
    };
    // Core's second pass runs after the blk-file check and polls before its first entry
    // (`validation.cpp:5342`); the poll after V's check stands for it.
    let idx = NodeBlockIndex::load_until(&chain, || shutdown.shutdown_requested())
        .filter(|_| block_files_present(&chain, files) && !shutdown.shutdown_requested());
    let Some(idx) = idx else {
        return Err(failed());
    };
    // :188-198
    let has = |b: Arc<Block>| chain.reader().lookup(&b.hash()).is_some();
    if !has(genesis_block(&params)) {
        return Err(LoadFailure::Incompatible(WRONG_NETWORK_DATADIR));
    }
    if devnet_genesis_block(&params).is_some_and(|b| !has(b)) {
        return Err(LoadFailure::Incompatible(WRONG_DEVNET_DATADIR));
    }
    Ok((chain, idx))
}

/// [`open_block_index`] on an empty index: Core's `LoadBlockIndexDB` runs on it too (no entry,
/// no file to check), then `LoadGenesisBlock`.
fn empty_index(
    params: Arc<ChainParams>,
    cfg: ChainConfig,
    svc: Services,
    files: &mut dyn BlockFiles,
) -> Result<(HeaderChain, NodeBlockIndex), LoadFailure> {
    tracing::info!("{CHECKING_BLK_FILES}");
    load_genesis_block(params, cfg, svc, files).map_err(|f| LoadFailure::Failure {
        cause: Some(f.0),
    })
}

/// `LoadBlockIndexDB`'s line before the check (`node/blockstorage.cpp:378`).
const CHECKING_BLK_FILES: &str = "Checking all blk files are present...";

// Core: node/blockstorage.cpp:377-390 (LoadBlockIndexDB)
/// Every block file that holds a block's data (`BLOCK_HAVE_DATA`) opens, in file order.
fn block_files_present(chain: &HeaderChain, files: &dyn BlockFiles) -> bool {
    tracing::info!("{CHECKING_BLK_FILES}");
    let r = chain.reader();
    let held: BTreeSet<i32> = chain
        .ids()
        .map(|id| entry(&r, id))
        .filter(|e| e.status.has(Status::HAVE_DATA))
        .filter_map(|e| e.file)
        .collect();
    held.into_iter().all(|file| files.open_block_file(file))
}

/// Core's genesis block from the chain parameters.
pub fn genesis_block(params: &ChainParams) -> Arc<Block> {
    block_of(&params.genesis)
}

/// Core's `DevNetGenesisBlock()`, block 1 of a devnet; `None` on the other networks.
pub fn devnet_genesis_block(params: &ChainParams) -> Option<Arc<Block>> {
    params.devnet_genesis.as_ref().map(block_of)
}

fn block_of(b: &dash_primitives::Block) -> Arc<Block> {
    let txs = b.transactions.iter().map(|t| Arc::new(Tx::new(t.clone()))).collect();
    Arc::new(Block::new(Header::new(b.header), txs))
}

/// Core `LoadGenesisBlock` on an empty index (`needs_init`, `validation.cpp:5362-5424`):
/// `SaveBlockToDisk(genesis)`, `AddToBlockIndex` (SB4's `new_genesis`), then
/// `ReceivedBlockTransactions`. The active chain stays empty (CH1-A11); the first ABC connects
/// genesis through `ConnectTip`. On a devnet, V accepts the devnet genesis block when it starts
/// (`:5408-5416`, D18): Core's `AcceptBlock` is V's. ND1 reaches it through
/// [`open_block_index`].
pub fn load_genesis_block(
    params: Arc<ChainParams>,
    cfg: ChainConfig,
    svc: Services,
    files: &mut dyn BlockFiles,
) -> Result<(HeaderChain, NodeBlockIndex), Fatal> {
    let block = genesis_block(&params);
    let devnet = params.devnet_genesis.is_some();
    // Core: AddGenesisBlock writes the block first (:5390).
    let pos = files
        .save_block(&block, 0, None)
        .map_err(|e| Fatal::new(format!("System error: {e}")))?
        .ok_or_else(|| Fatal::new("AddGenesisBlock: writing genesis block to disk failed"))?;
    let mut chain = HeaderChain::new_genesis(params, cfg, svc, dash_chain::NoStore)?;
    let reader = chain.reader();
    let id = chain.best_header();
    let mut idx = NodeBlockIndex::new();
    if devnet {
        idx.set_devnet_pending();
    }
    let n_tx = u32::try_from(block.txs().len()).expect("genesis has one transaction");
    idx.received_block_transactions(&mut chain, &reader, id, n_tx, pos)?;
    Ok((chain, idx))
}

/// V's refusal to start on a chainstate tip the index marks invalid while no valid block has at
/// least its work (DEC-196 (1)): a model message (DEC-165 (3)), not Core's. It advises `-reindex`
/// first, as `-reindex-chainstate` is refused while `-txindex` is on, Dash's default; `-reindex`
/// rebuilds the index without `invalidateblock`'s marks, so those calls are repeated.
pub const INVALID_TIP: &str = "The chainstate tip is invalid and no valid block has at least its \
     work; restart with -reindex, then repeat any invalidateblock calls (or use \
     -reindex-chainstate, which keeps them, if -txindex is off).";

/// Core `LoadChainTip` (`validation.cpp:4992-5018`) for the coins' best block `tip`: the active
/// chain, then `PruneBlockIndexCandidates` and Core's log line. An unknown block is Core's
/// `false`, an init error.
///
/// A tip the index marks failed or ChainLock-conflicting is never a candidate, so the prune
/// can leave none (an index written after `invalidateblock` or a ChainLock reorg, the coins
/// before it). DEC-196 decides that case, reading "valid block" as a candidate (its data is
/// stored, and it is neither failed nor conflicting):
/// - when a candidate has more work than the tip, Core's prune keeps the candidates no worse
///   than the tip and ABC reorgs (3);
/// - when the best candidate has the tip's work, the prune keeps every candidate with that work,
///   whatever the tie-break between them and the tip, and ABC reorgs to one (2). If some rank
///   below the tip, a state Core's prune never leaves, V serves no other command until
///   startup's ABC completes (D38);
/// - otherwise V refuses to start with [`INVALID_TIP`] (1).
///
/// SB4's load spreads `FAILED_CHILD` to the descendants of a failed block but not
/// `CONFLICT_CHAINLOCK`, which `FindMostWorkChain` marks lazily. So a stored descendant of a
/// conflicting tip is a candidate with more work, and (3) lets ABC extend the conflicting chain
/// from the tip, as Core's own `LoadChainTip` and ABC would.
pub fn load_chain_tip(
    chain: &mut HeaderChain,
    idx: &mut NodeBlockIndex,
    tip: BlockId,
) -> Result<(), Fatal> {
    if chain.private_tip().is_some_and(|(t, _)| t.id == tip) {
        return Ok(());
    }
    let reader = chain.reader();
    let e = reader.entry(tip).ok_or_else(|| Fatal::new("LoadChainTip: unknown tip"))?;
    let best_work = idx.best_candidate().map(|b| entry(&reader, b).chain_work);
    let prune_to_tip = idx.is_candidate(tip) || best_work.is_some_and(|w| w > e.chain_work);
    if !prune_to_tip {
        debug_assert!(
            e.status.has(Status::FAILED_MASK | Status::CONFLICT_CHAINLOCK),
            "a loaded chainstate tip is a candidate unless the index marks it invalid"
        );
        if best_work != Some(e.chain_work) {
            return Err(Fatal::new(INVALID_TIP));
        }
    }
    chain.init_active_tip(tip)?;
    if prune_to_tip {
        idx.prune_block_index_candidates(&reader, tip);
    } else {
        idx.prune_candidates_below(&reader, tip);
    }
    // :5012-5016, `%f` six places. The coins' tip was connected, so its `nChainTx` is not 0 and
    // the progress is finite. The system time, as Core's `time(nullptr)` (never the mock time).
    debug_assert!(idx.have_txs_downloaded(tip), "the coins' tip has its transactions");
    let progress = guess_verification_progress(
        &chain.params().chain_tx_data,
        idx.n_chain_tx(tip),
        e.header.time,
        SystemClock.node_now().secs(),
    );
    tracing::info!(
        "Loaded best chain: hashBestChain={} height={} date={} progress={:.6}",
        e.hash,
        e.height,
        format_iso8601_date_time(i64::from(e.header.time)),
        progress
    );
    Ok(())
}

/// Core's `GuessVerificationProgress` (`validation.cpp:5907-5922`): the share of the chain's
/// transactions up to a block with `chain_tx` (`nChainTx`) and `time`, against the parameters'
/// estimate of all there are at `now` (`time(nullptr)`).
pub fn guess_verification_progress(data: &ChainTxData, chain_tx: u32, time: u32, now: i64) -> f64 {
    let total = if i64::from(chain_tx) <= data.tx_count {
        data.tx_count as f64 + (now - data.timestamp) as f64 * data.tx_rate
    } else {
        f64::from(chain_tx) + (now - i64::from(time)) as f64 * data.tx_rate
    };
    // `std::min<double>(x, 1.0)` is `1.0 < x ? 1.0 : x`: a NaN stays.
    let p = f64::from(chain_tx) / total;
    if 1.0 < p {
        1.0
    } else {
        p
    }
}

/// The readiness guard at startup (CH3 §2.17, CH3a D70): with an incomplete `H`, refuses to start
/// when G1 holds for any of `blocks`: the active tip and the blocks of recovery's transition.
/// ND1 calls it in Core's order: after recovery's replay (`ReplayBlocks`,
/// `node/chainstate.cpp:251`) and [`load_chain_tip`], which gives the active tip (`LoadChainTip`,
/// `:272-277`), over the replayed blocks and then the tip, and before init's ABC. The deployments
/// are V's (CS4's over `chain`, a8's D80), with fresh caches. They are asked first; the block's
/// data is read only when they pass and its coinbase decides, and a block without data is judged
/// on its deployments alone. So is the tip (the last of `blocks`) when its data does not read:
/// Core reads nothing here, and its `VerifyDB`, next, reads the tip and fails the load with
/// Core's lines (RP1a D47). Recovery read the transition's blocks just before, so one of those
/// that no longer reads fails the load here.
///
/// # Errors
///
/// [`LoadFailure::NotReady`] with the connector's refusal text; [`LoadFailure::Store`] when the
/// data of a block before the tip does not read.
pub fn check_startup_readiness<H: EvoHooks>(
    params: &Arc<ChainParams>,
    chain: &HeaderChain,
    files: &dyn BlockFiles,
    blocks: &[BlockId],
) -> Result<(), LoadFailure> {
    if H::COMPLETE {
        return Ok(());
    }
    let reader = chain.reader();
    let deployments = ChainDeployments::new(params.clone(), chain);
    let deployments = deployments.view();
    for (i, &id) in blocks.iter().enumerate() {
        let e = entry(&reader, id);
        // The deployments first: the block's data is read only when its coinbase decides.
        let mut rule = g1_rule(&deployments, e.prev, None);
        if rule.is_none()
            && let Some(file) = e.file.filter(|_| e.status.has(Status::HAVE_DATA))
        {
            let pos = FlatFilePos {
                file,
                pos: e.data_pos,
            };
            match files.read_block(pos, &e.hash) {
                Ok(block) => {
                    rule = g1_rule(&deployments, e.prev, block.txs().first().map(|cb| &**cb));
                }
                Err(cause) if i + 1 < blocks.len() => {
                    return Err(LoadFailure::Store {
                        cause,
                    });
                }
                Err(_) => {}
            }
        }
        if let Some(rule) = rule {
            let at = BlockAt {
                hash: e.hash,
                height: e.height,
                prev: e.prev,
                id: Some(id),
            };
            return Err(LoadFailure::NotReady(refusal_text(params, &at, &g1_capability(rule))));
        }
    }
    Ok(())
}
