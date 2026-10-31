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
use dashd_store::{ErrorKind, KvEngine, KvRead as _, StoreError, ValueCodec as _};

use crate::chain::verify::EVODB_WITHOUT_BLOCKS;
use crate::flush::{CRASH_MESSAGE, CrashRoll, FlushObserver, LogCat, ObsMark, SubscriberHub, log};
use crate::planted::planted;
use crate::store::{
    CHAINSTATE_SCHEMA, GuardedEngine, GuardedFiles, InitReads, Meta, wipe_chainstate,
};

pub(crate) use replay::FileRead;
pub use replay::{MarkerState, Recovery, RecoveryStep};

// Core: node/chainstate.cpp:252
const UNABLE_TO_REPLAY: &str =
    "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.";
// Core: init.cpp:2037
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
    // Core: validation.cpp:1928 EvoDbInconsistencyMessage
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

/// Recovery's fixed inputs.
#[derive(Clone, Debug)]
pub struct RecoveryConfig {
    /// The network's proof-of-work parameters, which Core's `ReadBlockFromDisk` checks each block
    /// header it reads against (`node/blockstorage.cpp:825-829`): `PowParams::from_chain_params`
    /// of the `ChainParams` the block index was loaded with. Another network's would accept or
    /// refuse other headers.
    pub pow: PowParams,
    /// S: a replay sub-batch closes once it holds more than this (`tuning::sub_batch`, as W's).
    pub sub_batch: u64,
    /// `-dbcrashratio`'s roll, after each sub-batch.
    pub crash: CrashRoll,
}

/// What recovery leaves: the coins are at `tip`, and nothing above it is written.
pub struct RecoveredChainstate<F: FlatFs> {
    /// The block the coins and the evo state are at; `None` for an empty chainstate (Core's null
    /// best block), whose node starts from genesis.
    pub tip: Option<BlockRef>,
    /// The blocks replayed coins-only, in order: the forward leg, `fork + 1` to `new` (ST3a
    /// R6(b)). Empty without a marker.
    pub replayed: Vec<BlockHash>,
    /// The decision keys as recovery left them.
    pub meta: Meta,
    pub files: Arc<GuardedFiles<F>>,
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
        let (mut meta, mut malformed_marker) = read_meta()?;
        // Step 2 (BL-72): before any marker is read, so an interrupted wipe is never corruption.
        let wiped = meta.wiping;
        if wiped {
            // DEC-165(3) audit: no Core text at this point. Nearest Core lines: init.cpp:2041 and
            // node/chainstate.cpp:233-236, where every `-reindex-chainstate` start wipes the evo
            // and coins databases; Core's wipe is not resumed (BL-72).
            log(obs, LogCat::Always, "Resuming the interrupted wipe of the chainstate".into());
            wipe_chainstate(engine).map_err(store_failed(obs, "write"))?;
            (meta, malformed_marker) = read_meta()?;
        }
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
    // Core: txdb.cpp:300-344, dbwrapper.cpp:136
    pub fn block_index(&self) -> impl Iterator<Item = Result<(Vec<u8>, Vec<u8>), StoreError>> + '_ {
        self.engine.init_index_scan(&self.init)
    }

    /// Step 2 resumed an interrupted wipe: the chainstate is empty, and the node continues with
    /// CH5's `-reindex-chainstate`. [`recover`](Self::recover) still runs: it finds the empty
    /// chainstate consistent and marks [`ObsMark::RecoveryEnd`].
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

// Core: node/blockstorage.cpp:360-374 (LoadBlockIndexDB)
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
        // node/blockstorage.cpp:365, which ignores the read's result and keeps a default or
        // partly decoded info; past `last_file`, :370 ends the scan. Corruption paths only.
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
