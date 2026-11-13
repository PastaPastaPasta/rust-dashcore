//! Recovery at startup (CH2 §4.3; E-b1.3): two calls around CH1-node's load of the block index.
//!
//! - [`ChainstateOpen::open`] runs steps 0–3: the open-time `sync_wal`, the decision keys, an
//!   interrupted `-reindex-chainstate` wipe resumed, and the block files from their durable infos.
//! - CH1-node loads the block index (step 4), and runs step 7's startup scan there (E-b1.3).
//! - [`Opened::recover`] runs steps 5, 6, 8, 9 and 10 by looping [`Recovery::step`], the seam's
//!   step machine: the checks, Core's `ReplayBlocks` as blind coin writes, the final Durable batch,
//!   and `T_ack`. b1 has no subscribers, so step 6 registers no prune lock and step 10 opens no
//!   subscriber (E-b1.6).
//!
//! Step 11 (`LoadChainTip`, `VerifyDB`, the first `ActivateBestChain`) is CH1-node's and ND1's.
//!
//! From [`ObsMark::RecoveryBegin`] to [`ObsMark::RecoveryEnd`] every store call that fails stops
//! the node with Core's init error for it (ST3a §4.4): no read failure is taken for absent data.
//! A failed engine read there trips the latch without [`AbortSink::read_failed`]'s abort, as
//! Core's `LoadChainstate` reads its databases directly (`GuardedEngine`'s init region).
//!
//! [`AbortSink::read_failed`]: crate::store::AbortSink::read_failed

mod replay;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use dash_chain::{BlockRef, IndexAccess};
use dash_consensus::pow::PowParams;
use dash_consensus::primitives::BlockHash;
use dashd_store::flatfile::{BlockFileInfo, BlockFiles, BlockFilesConfig, BlockFilesState, FlatFs};
use dashd_store::{Commit, ErrorKind, KvEngine, KvRead as _, StoreError, ValueCodec as _};

use crate::chain::verify::EVODB_WITHOUT_BLOCKS;
use crate::flush::{CRASH_MESSAGE, CrashRoll, FlushObserver, LogCat, ObsMark, SubscriberHub, log};
use crate::planted::planted;
use crate::store::{
    CHAINSTATE_SCHEMA, GuardedEngine, GuardedFiles, InitReads, Markers, Meta, wipe_block_tree,
    wipe_chainstate,
};

pub(crate) use replay::FileRead;
pub use replay::{MarkerState, Recovery, RecoveryStep};

// Core: node/chainstate.cpp:252
const UNABLE_TO_REPLAY: &str =
    "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.";
// Core: init.cpp:2027 at v24.0.0
const OPEN_FAILED: &str = "Error opening block database";

/// Why the chainstate cannot load: Core's init error for the path, which ND1 shows before it exits.
/// Recovery has logged the cause first, as Core does.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    /// Core's `ReplayBlocks` returned false (`node/chainstate.cpp:252`): a marker check failed, or
    /// a block or its undo data could not be read or does not match. `detail` is the line logged.
    #[error("{UNABLE_TO_REPLAY}")]
    UnableToReplay {
        detail: String,
    },
    /// No marker, and the evo state is not at the coins' tip (§4.3 step 5).
    // Core: validation.cpp:1939 at v24.0.0 EvoDbInconsistencyMessage
    #[error("Found EvoDB inconsistency, you must reindex to continue")]
    EvoInconsistent,
    /// No marker, and `meta.tip` is not in the block index: Core's `LoadChainTip` returns false.
    // Core: node/chainstate.cpp:273-275
    #[error("Error initializing block database")]
    UnknownTip(BlockHash),
    /// Neither tip nor marker, yet the evo state holds records or a tip (§4.3 step 5): Core's
    /// evodb with no blocks, which init reports with its reindex question; `detail` is logged
    /// first (CH7-notes D71).
    // Core: node/chainstate.cpp:471-473
    #[error("{EVODB_WITHOUT_BLOCKS}")]
    EvoWithoutBlocks {
        detail: String,
    },
    /// Neither tip nor marker, yet the coins hold records (§4.3 step 5).
    // DEC-165(3) audit: no Core text at this point. Nearest Core line: node/chainstate.cpp:272,
    // where a null best block skips `LoadChainTip`; Core does not check empty coins.
    #[error(
        "Corrupted chainstate database: {0}. You will need to rebuild the database using \
         -reindex-chainstate."
    )]
    Corruption(String),
    /// A store call failed and tripped the latch, or was refused after another failure had (kind
    /// [`ErrorKind::ReadOnly`]): Core's `LoadChainstate` catches the database's exception.
    #[error("{OPEN_FAILED}")]
    Store(StoreError),
    /// `-dbcrashratio`'s roll came up after a sub-batch (§4.6): ND1's `_Exit(0)`, after "Simulating
    /// a crash. Goodbye." has been logged.
    #[error("{CRASH_MESSAGE}")]
    Crashed,
}

impl InitError {
    /// Whether Core is past `InitCoinsCache` in this state (`node/chainstate.cpp:256-257`), so
    /// `CanFlushToDisk()` holds and its two shutdown flushes run after the init error
    /// (`validation.h:565-569`, `init.cpp:379-387`, :450-457 at v24.0.0). The node then keeps the
    /// chainstate for those flushes only (BL-636).
    pub fn after_coins_cache(&self) -> bool {
        match self {
            // Core: node/chainstate.cpp:272-276 (`LoadChainTip` false)
            InitError::UnknownTip(_) => true,
            // Core: node/chainstate.cpp:467-474 (`VerifyLoadedChainstate`)
            InitError::EvoWithoutBlocks {
                ..
            } => true,
            // Refusals Core does not make (CH2's corruption checks, DEC-165(3)): Core loads these
            // states, past `InitCoinsCache` on every path from them.
            InitError::EvoInconsistent | InitError::Corruption(_) => true,
            // `ReplayBlocks` and its writes come before `InitCoinsCache` (node/chainstate.cpp:251-
            // 252), as do the open's store calls; a failed read in the no-tip scan is CH7 D71's
            // store failure; `-dbcrashratio` ends the process.
            InitError::UnableToReplay {
                ..
            }
            | InitError::Store(_)
            | InitError::Crashed => false,
        }
    }
}

/// Recovery's fixed inputs.
#[derive(Clone, Debug)]
pub struct RecoveryConfig {
    /// The network's proof-of-work parameters, which Core's `ReadBlockFromDisk` checks each block
    /// header it reads against (`node/blockstorage.cpp:828-832` at v24.0.0):
    /// `PowParams::from_chain_params` of the `ChainSetup` the block index was loaded with. Another
    /// network's would accept or refuse other headers.
    pub pow: PowParams,
    /// S: a replay sub-batch closes once it holds more than this (`tuning::sub_batch`, as W's).
    pub sub_batch: u64,
    /// `-dbcrashratio`'s roll, after each sub-batch.
    pub crash: CrashRoll,
}

/// What recovery leaves: the coins are at `tip`, and nothing above it is written.
pub struct RecoveredChainstate<F: FlatFs> {
    /// The block the coins and the evo state are at; `None` for an empty chainstate (Core's null
    /// best block), whose node starts from genesis. `None` also for a chainstate kept only for
    /// the shutdown flushes after a refusal that leaves Core past `InitCoinsCache`
    /// ([`InitError::after_coins_cache`]); `meta.tip` then holds the coins' best block.
    pub tip: Option<BlockRef>,
    /// The blocks replayed coins-only, in order: the forward leg, `fork + 1` to `new` (ST3a
    /// R6(b)). Empty without a marker.
    pub replayed: Vec<BlockHash>,
    /// The decision keys as recovery left them.
    pub meta: Meta,
    pub files: Arc<GuardedFiles<F>>,
}

/// The arguments that wipe at the open (CH5): `-reindex` empties the block tree and the
/// chainstate, `-reindex-chainstate` the chainstate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wipe {
    /// `-reindex`, the argument (Core's `options.reindex`).
    pub reindex: bool,
    /// `-reindex-chainstate`.
    pub reindex_chainstate: bool,
}

/// Steps 0–3 (E-b1.3).
#[derive(Clone, Copy, Debug)]
pub struct ChainstateOpen;

impl ChainstateOpen {
    // Core: node/chainstate.cpp:150-200 (LoadChainstate up to LoadBlockIndex)
    /// Opens the chainstate for CH1-node's index load. Marks [`ObsMark::RecoveryBegin`] first.
    ///
    /// # Errors
    ///
    /// [`InitError::Store`]: a store call failed, and the latch has tripped; or a `block_files`
    /// record does not decode (kind `Corruption`).
    pub fn open<'a, E: KvEngine, F: FlatFs>(
        engine: &'a GuardedEngine<E>,
        fs: Arc<F>,
        blocks: BlockFilesConfig,
        obs: &'a dyn FlushObserver,
    ) -> Result<Opened<'a, E, F>, InitError> {
        Self::open_wiping(engine, fs, blocks, obs, Wipe::default())
    }

    /// The wipe markers and the stored reindex alone, before [`open`](Self::open): the start's
    /// checks that must write nothing and decode no other key.
    ///
    /// # Errors
    ///
    /// [`InitError::Store`]: a marker does not decode, or the read failed; the latch has tripped.
    pub fn markers<E: KvEngine>(
        engine: &GuardedEngine<E>,
        obs: &dyn FlushObserver,
    ) -> Result<Markers, InitError> {
        Markers::read(engine).map_err(|e| store_failed(obs, "read")(e.into_store_error()))
    }

    // Core: node/chainstate.cpp:149-160, :233-236, init.cpp:2031-2033 at v24.0.0
    /// [`open`](Self::open), and step 2 wipes as `wipe` asks or an interrupted wipe left, in this
    /// order (OD-CH5-56): `-reindex`, or an interrupted block-tree wipe, empties the block tree
    /// and the chainstate ([`wipe_block_tree`]); otherwise an interrupted wipe or
    /// `-reindex-chainstate` empties the chainstate ([`wipe_chainstate`]): Core's coins, evo, IS
    /// and LLMQ databases are all in this one. A stored reindex alone wipes nothing. Before a
    /// block-tree wipe only the markers are read, before a chainstate wipe only the keys it keeps;
    /// step 1 then reads the wiped meta.
    ///
    /// # Errors
    ///
    /// As [`open`](Self::open).
    pub fn open_wiping<'a, E: KvEngine, F: FlatFs>(
        engine: &'a GuardedEngine<E>,
        fs: Arc<F>,
        blocks: BlockFilesConfig,
        obs: &'a dyn FlushObserver,
        wipe: Wipe,
    ) -> Result<Opened<'a, E, F>, InitError> {
        obs.mark(ObsMark::RecoveryBegin);
        let init = engine.init_reads();
        // Step 0 (E21): no batch this epoch finalizes was left unsynced by a kill (X6 skips it).
        if !planted!(engine.planted(), X6) {
            engine.sync_wal().map_err(store_failed(obs, "sync_wal"))?;
        }
        // Step 1: the only keys recovery may later write, read before its first write.
        let read_meta = || {
            Meta::read_at_open(engine).map_err(|e| store_failed(obs, "read")(e.into_store_error()))
        };
        let read_err =
            |e: crate::store::CoinsReadError| store_failed(obs, "read")(e.into_store_error());
        // Every wipe decision starts from the markers alone, so that a key a block-tree wipe
        // erases need not decode: Core destroys its block tree database unread
        // (dbwrapper.cpp:144-147).
        let markers = Markers::read(engine).map_err(read_err)?;
        let tree = wipe.reindex || markers.wiping_tree;
        // Under `-reindex-chainstate` only the keys the wipe keeps are read before it: Core
        // destroys its coins and evo databases unread (dbwrapper.cpp:144-147), so the keys the
        // wipe erases need not decode.
        let mut read = if tree || wipe.reindex_chainstate {
            None
        } else {
            Some(read_meta()?)
        };
        if !tree && wipe.reindex_chainstate {
            Meta::read_kept(engine).map_err(read_err)?;
        }
        // Step 2 (BL-72): before the rest of the meta is read, so an interrupted wipe is never
        // corruption.
        let wiped = if tree {
            // DEC-165(3) audit: no Core text at this point. Nearest Core lines:
            // node/chainstate.cpp:155 and init.cpp:2031-2033 at v24.0.0, where every `-reindex`
            // start wipes the block tree, coins and evo databases; Core's wipe is not resumed.
            if !wipe.reindex {
                log(
                    obs,
                    LogCat::Always,
                    "Resuming the interrupted wipe of the block tree and the chainstate".into(),
                );
            }
            wipe_block_tree(engine).map_err(store_failed(obs, "write"))?;
            true
        } else if markers.wiping || wipe.reindex_chainstate {
            // DEC-165(3) audit: no Core text at this point. Nearest Core lines: init.cpp:2031 at
            // v24.0.0 and node/chainstate.cpp:233-236, where every `-reindex-chainstate` start
            // wipes the evo and coins databases; Core's wipe is not resumed (BL-72).
            if markers.wiping {
                log(obs, LogCat::Always, "Resuming the interrupted wipe of the chainstate".into());
            }
            wipe_chainstate(engine).map_err(store_failed(obs, "write"))?;
            true
        } else {
            false
        };
        if wiped {
            read = None;
        }
        let (meta, malformed_marker) = match read {
            Some(read) => read,
            None => read_meta()?,
        };
        // Step 3 (ST2): from the durable infos and `last_file`, never from the files' lengths.
        let infos = file_infos(engine, meta.last_file).map_err(store_failed(obs, "read"))?;
        let state = BlockFilesState {
            infos,
            last_file: meta.last_file,
        };
        let files = BlockFiles::with_fs(fs, blocks, state);
        Ok(Opened {
            engine,
            obs,
            meta,
            malformed_marker,
            files: Arc::new(GuardedFiles::new(files, engine.latch().clone())),
            wiped,
            init,
        })
    }
}

/// Why [`request_reindex`] wrote nothing.
#[derive(Debug, thiserror::Error)]
pub enum RequestReindexError {
    /// The block index holds a record: a reindex of it needs `-reindex`'s wipe.
    #[error("the block index is not empty")]
    NotEmpty,
    /// A wipe began and did not finish; the next start resumes it.
    #[error("a wipe is in progress")]
    WipeInProgress,
    /// A read or the write failed; the latch has tripped.
    #[error("{0}")]
    Store(StoreError),
}

/// Records a reindex on a store whose block index is empty (CH6a's entry point, OD-CH5-68/69):
/// one Durable batch sets `reindexing`, Core's `WriteReindexing(true)`, and the next start is a
/// stored-flag resume that scans the block files from `blk00000.dat`. A flag already set is left
/// as it is, and the call answers `Ok` without a write. On a block index with a record, or with
/// `wiping` or `wiping_tree` set, it writes nothing. Called before [`ChainstateOpen::open`]: that
/// open's reads then include the flag.
///
/// # Errors
///
/// [`RequestReindexError`]: a wipe marker is set (checked first), the block index is not empty,
/// or the store failed.
// Core: txdb.cpp:188-193 at v24.0.0
pub fn request_reindex<E: KvEngine>(engine: &GuardedEngine<E>) -> Result<(), RequestReindexError> {
    let store = |e: crate::store::CoinsReadError| RequestReindexError::Store(e.into_store_error());
    let markers = Markers::read(engine).map_err(store)?;
    if markers.wiping || markers.wiping_tree {
        return Err(RequestReindexError::WipeInProgress);
    }
    let cf = CHAINSTATE_SCHEMA.cf("block_index").expect("the chainstate schema has it");
    if let Some(record) = engine.iter_prefix(cf, &[]).next() {
        record.map_err(RequestReindexError::Store)?;
        return Err(RequestReindexError::NotEmpty);
    }
    if markers.reindexing {
        return Ok(());
    }
    let mut b = engine.batch();
    Meta::put_reindexing(&mut b, true);
    engine.write(b, Commit::Durable).map_err(RequestReindexError::Store)
}

/// The chainstate between [`ChainstateOpen::open`] and [`Opened::recover`]: CH1-node loads the
/// block index from the same engine meanwhile.
pub struct Opened<'a, E: KvEngine, F: FlatFs> {
    engine: &'a GuardedEngine<E>,
    obs: &'a dyn FlushObserver,
    meta: Meta,
    /// Step 1 read a `head_blocks` value of a whole number of hashes other than two.
    malformed_marker: bool,
    files: Arc<GuardedFiles<F>>,
    wiped: bool,
    /// The engine's init region, until recovery is dropped.
    init: InitReads<'a>,
}

impl<'a, E: KvEngine, F: FlatFs> Opened<'a, E, F> {
    /// The decision keys (step 1), after any resumed wipe.
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// The block files: CH1-node's step 7 scan runs on them.
    pub fn files(&self) -> &Arc<GuardedFiles<F>> {
        &self.files
    }

    /// The `block_index` column family's records in key order, for CH1-node's index load (step
    /// 4), as Core's `LoadBlockIndexGuts` reads them: uncached, and outside the latch, so a failed
    /// read is the load's init error rather than the engine's fail-stop (ND1 2b-D20). The values
    /// are SB4's to decode.
    // Core: txdb.cpp:300-344 at v24.0.0, dbwrapper.cpp:136
    pub fn block_index(&self) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + '_ {
        self.engine.init_index_scan(&self.init)
    }

    /// Step 2 wiped, for `-reindex-chainstate` or to resume an interrupted wipe: the chainstate
    /// is empty, and the node continues as for `-reindex-chainstate`. [`recover`](Self::recover)
    /// still runs: it finds the empty chainstate consistent and marks [`ObsMark::RecoveryEnd`].
    pub fn wiped(&self) -> bool {
        self.wiped
    }

    /// Steps 5–10 as a step machine, for the seam.
    pub fn recovery<'b>(
        self,
        index: &'b dyn IndexAccess,
        hub: &'b SubscriberHub,
        cfg: RecoveryConfig,
    ) -> Recovery<'b, E, F>
    where
        'a: 'b,
    {
        Recovery::new(self, index, hub, cfg)
    }

    /// Steps 5, 6, 8, 9 and 10, once CH1-node has loaded the block index into `index`.
    ///
    /// # Errors
    ///
    /// [`InitError`]: the node stops with its message. [`InitError::Crashed`] is `-dbcrashratio`'s
    /// exit.
    pub fn recover(
        self,
        index: &dyn IndexAccess,
        hub: &SubscriberHub,
        cfg: RecoveryConfig,
    ) -> Result<RecoveredChainstate<F>, InitError> {
        let mut r = self.recovery(index, hub, cfg);
        loop {
            match r.step()? {
                RecoveryStep::Done => return Ok(r.into_recovered()),
                RecoveryStep::Crash => return Err(InitError::Crashed),
                _ => {}
            }
        }
    }
}

// Core: node/blockstorage.cpp:363-378 at v24.0.0 (LoadBlockIndexDB)
/// The block files' infos: files 0 to `last_file`, a missing record default, then the files after
/// it while their records are contiguous. No other record is read.
fn file_infos<E: KvEngine>(
    engine: &GuardedEngine<E>,
    last_file: u32,
) -> Result<Vec<BlockFileInfo>, StoreError> {
    let cf = CHAINSTATE_SCHEMA.cf("block_files").expect("the chainstate schema has it");
    let read = |file: u32| -> Result<Option<BlockFileInfo>, StoreError> {
        let key = file.to_be_bytes();
        let Some(v) = engine.get(cf, &key)? else {
            return Ok(None);
        };
        // DEC-165(3) audit: no Core text at this point (quirk 43, D-b1-f). Nearest Core line:
        // node/blockstorage.cpp:368 at v24.0.0, which ignores the read's result and keeps a default
        // or partly decoded info; past `last_file`, :373 at v24.0.0 ends the scan. Corruption paths
        // only.
        BlockFileInfo::decode_value(&v)
            .map(Some)
            .map_err(|e| engine.corrupt(cf, &key, &e.to_string()).into_store_error())
    };
    let mut infos = (0..=last_file)
        .map(|file| read(file).map(Option::unwrap_or_default))
        .collect::<Result<Vec<_>, _>>()?;
    let mut file = last_file;
    while let Some(next) = file.checked_add(1)
        && let Some(info) = read(next)?
    {
        infos.push(info);
        file = next;
    }
    Ok(infos)
}

/// A failed store call's init error. The failure is reported unless the call was refused (kind
/// [`ErrorKind::ReadOnly`]) after another failure had latched: a refused write stops recovery
/// silently. A read is never refused, so each failed read is reported.
fn store_failed(obs: &dyn FlushObserver, op: &'static str) -> impl FnOnce(StoreError) -> InitError {
    move |e| {
        if e.kind() != Some(ErrorKind::ReadOnly) {
            log(obs, LogCat::Error, format!("{OPEN_FAILED}: {e}"));
            obs.mark(ObsMark::StoreFailed {
                op,
            });
            obs.mark(ObsMark::Abort {
                message: OPEN_FAILED.into(),
            });
        }
        InitError::Store(e)
    }
}
