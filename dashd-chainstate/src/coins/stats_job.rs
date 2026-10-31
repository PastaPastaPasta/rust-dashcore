//! `gettxoutsetinfo`'s scan of the coins database without an index (RP1a-1, DEC-594 #2): Core's
//! `ComputeUTXOStats` over `CoinsDB()` after `ForceFlushStateToDisk()`.
//!
//! V flushes and takes the snapshot in one class M turn, so no block connects between the two
//! (Core's flush and its `cs_main` read of the best block are two steps). A snapshot borrows its
//! engine, so it cannot leave V's turn: the scan's own thread holds the engine, takes the snapshot
//! and acknowledges it before V's turn ends, then walks it while V serves on.
//!
//! `dumptxoutset` (RP1b-1b) is the same scan with a [`CoinsDump`]: Core flushes, computes the
//! statistics and opens its cursor under one `cs_main`, so the file holds the coins the
//! statistics count. Here both walks read the one snapshot.

use std::io::Write;
use std::sync::Arc;

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan, UtxoSetStats};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;
use dashd_store::KvEngine;

use super::table::CoinsTable;
use crate::chain::reply::{ActorReply, reply, spawn_thread};
use crate::store::{CoinsReadError, GuardedEngine, Meta};

/// A scan's result: the statistics, the block the coins are at (Core's `GetBestBlock()`) and the
/// column family's estimated size (`EstimateSize()`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinsStats {
    pub best_block: BlockHash,
    pub stats: UtxoSetStats,
    pub disk_size: u64,
}

/// Why a scan has no result: Core's `ComputeUTXOStats` returning `false` ("Unable to read UTXO
/// set"), or the RPC's interruption point.
#[derive(Debug, thiserror::Error)]
pub enum CoinsStatsError {
    /// A read failed or a record does not decode; the latch has tripped.
    #[error(transparent)]
    Read(#[from] CoinsReadError),
    /// The coins have no best block, which a flushed chainstate always has.
    #[error("the coins have no best block")]
    NoBestBlock,
    /// The interruption point fired (shutdown).
    #[error("interrupted")]
    Interrupted,
    /// A [`CoinsDump`]'s file could not be written.
    #[error("writing the snapshot: {0}")]
    Write(#[from] std::io::Error),
    /// The scan's thread ended without a result.
    #[error("the scan's thread is gone")]
    Gone,
}

/// `dumptxoutset`'s file, which a scan writes from its snapshot once the statistics are done:
/// Core's `SnapshotMetadata` (the best block's hash, then the coin count as a `u64`), then each
/// coin's `COutPoint` and `Coin` in the cursor's order, the coins database's key order.
// Core: rpc/blockchain.cpp:3147-3210 CreateUTXOSnapshot; node/utxo_snapshot.h:38
pub struct CoinsDump {
    /// The file, open for writing; the scan's thread closes it before it sends the result.
    pub file: Box<dyn Write + Send>,
    /// Runs on the scan's thread before the first byte is written, with the coins' best block:
    /// Core's `LOG_TIME_SECONDS` scope starts there.
    pub on_start: Box<dyn FnOnce(&BlockHash) + Send>,
}

/// A scan running on its own thread, its snapshot taken.
pub struct CoinsStatsJob {
    result: ActorReply<Result<CoinsStats, CoinsStatsError>>,
    snapshot_epoch: Option<u64>,
}

impl CoinsStatsJob {
    /// The engine epoch the scan's snapshot is at; `None` if its thread died before taking it.
    pub fn snapshot_epoch(&self) -> Option<u64> {
        self.snapshot_epoch
    }

    /// The scan's result, once it is done.
    ///
    /// # Errors
    ///
    /// [`CoinsStatsError`]; `Gone` too for a caller LO-9 bars from waiting.
    pub fn wait(self) -> Result<CoinsStats, CoinsStatsError> {
        self.result.recv().unwrap_or(Err(CoinsStatsError::Gone))
    }
}

/// Starts a scan of `db`'s coins with `hash_type`, returning once its snapshot is taken, then
/// writes `dump`'s file from the same snapshot. `interrupted` is Core's `rpc_interruption_point`,
/// checked at each coin of the statistics and every 5000th of the file. The thread drops its
/// snapshot, engine and file before it sends the result, so once `wait` returns it holds none
/// (shutdown's flushes and the engine's close follow the RPC worker's answer).
///
/// # Errors
///
/// The scan's thread could not be spawned.
pub fn start_coins_stats<E: KvEngine + 'static>(
    db: Arc<GuardedEngine<E>>,
    hash_type: CoinStatsHashType,
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
    dump: Option<CoinsDump>,
) -> std::io::Result<CoinsStatsJob> {
    let (taken_tx, taken) = reply();
    let (result_tx, result) = reply();
    // Detached: `wait` reads its result. The thread never waits on V, so V may wait for the
    // snapshot (LO-9).
    let _detached = spawn_thread("coinstats", move || {
        let snap = db.snapshot();
        taken_tx.send(snap.epoch());
        let stats = scan(&db, &snap, hash_type, &*interrupted).and_then(|stats| match dump {
            Some(dump) => write_dump(&snap, &stats, dump, &*interrupted).map(|()| stats),
            None => Ok(stats),
        });
        drop(snap);
        drop(db);
        result_tx.send(stats);
    })?;
    // A thread that dies before its snapshot leaves `wait` its `Gone`.
    let snapshot_epoch = taken.recv().ok();
    Ok(CoinsStatsJob {
        result,
        snapshot_epoch,
    })
}

// Core: kernel/coinstats.cpp:116-150 ComputeUTXOStats, :156-158 (the best block's entry)
fn scan<E: KvEngine>(
    db: &GuardedEngine<E>,
    snap: &crate::store::GuardedSnapshot<'_, E>,
    hash_type: CoinStatsHashType,
    interrupted: &dyn Fn() -> bool,
) -> Result<CoinsStats, CoinsStatsError> {
    let best_block = Meta::read(snap)?.tip.ok_or(CoinsStatsError::NoBestBlock)?;
    let table = CoinsTable::default();
    let mut scan = UtxoSetScan::new(hash_type, &best_block);
    for item in table.cursor(snap) {
        if interrupted() {
            return Err(CoinsStatsError::Interrupted);
        }
        let (key, coin) = item?;
        scan.push(&key.outpoint(), &coin.as_ref());
    }
    Ok(CoinsStats {
        best_block,
        stats: scan.finish(),
        disk_size: table.estimate_size(db)?,
    })
}

/// The file's metadata and coins, from the snapshot `stats` counted.
// Core: rpc/blockchain.cpp:3188-3205
fn write_dump<E: KvEngine>(
    snap: &crate::store::GuardedSnapshot<'_, E>,
    stats: &CoinsStats,
    dump: CoinsDump,
    interrupted: &dyn Fn() -> bool,
) -> Result<(), CoinsStatsError> {
    let CoinsDump {
        file,
        on_start,
    } = dump;
    on_start(&stats.best_block);
    let mut out = std::io::BufWriter::new(file);
    // Core's `coins_count`, one a coin, is `nTransactionOutputs` without an index.
    out.write_all(&stats.best_block.to_lendian())?;
    out.write_all(&stats.stats.txouts.to_le_bytes())?;
    let mut bytes = Vec::with_capacity(64);
    for (iter, item) in CoinsTable::default().cursor(snap).enumerate() {
        if iter % 5000 == 0 && interrupted() {
            return Err(CoinsStatsError::Interrupted);
        }
        let (key, coin) = item?;
        bytes.clear();
        bytes.extend_from_slice(&key.0);
        coin.as_ref().write(&mut bytes);
        out.write_all(&bytes)?;
    }
    out.flush()?;
    Ok(())
}
