//! `gettxoutsetinfo`'s scan of the coins database without an index (RP1a-1, DEC-594 #2): Core's
//! `ComputeUTXOStats` over `CoinsDB()` after `ForceFlushStateToDisk()`.
//!
//! V flushes and takes the snapshot in one class M turn, so no block connects between the two
//! (Core's flush and its `cs_main` read of the best block are two steps). A snapshot borrows its
//! engine, so it cannot leave V's turn: the scan's own thread holds the engine, takes the snapshot
//! and acknowledges it before V's turn ends, then walks it while V serves on.

use std::sync::Arc;

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan, UtxoSetStats};
use dash_primitives::BlockHash;
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
    /// The scan's thread ended without a result.
    #[error("the scan's thread is gone")]
    Gone,
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

/// Starts a scan of `db`'s coins with `hash_type`, returning once its snapshot is taken.
/// `interrupted` is Core's `rpc_interruption_point`, checked at each coin. The thread drops its
/// snapshot and engine before it sends the result, so once `wait` returns it holds neither
/// (shutdown's flushes and the engine's close follow the RPC worker's answer).
///
/// # Errors
///
/// The scan's thread could not be spawned.
pub fn start_coins_stats<E: KvEngine + 'static>(
    db: Arc<GuardedEngine<E>>,
    hash_type: CoinStatsHashType,
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> std::io::Result<CoinsStatsJob> {
    let (taken_tx, taken) = reply();
    let (result_tx, result) = reply();
    // Detached: `wait` reads its result. The thread never waits on V, so V may wait for the
    // snapshot (LO-9).
    let _detached = spawn_thread("coinstats", move || {
        let snap = db.snapshot();
        taken_tx.send(snap.epoch());
        let stats = scan(&db, &snap, hash_type, &*interrupted);
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
