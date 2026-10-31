//! `scantxoutset`'s walk of the coins database (RP1b-1a): Core's `FindScriptPubKey` over the
//! cursor `CoinsDB().Cursor()` opens after `ForceFlushStateToDisk()`.
//!
//! As [`start_coins_stats`](super::start_coins_stats)'s scan, V flushes and the walk's own thread
//! takes the snapshot in one class M turn, then walks it while V serves on: Core's cursor is a
//! LevelDB snapshot too, so coins a block connects after the start are not seen. The column
//! family's order is Core's serialized-key order (`txid ‖ VARINT(vout)`), so the progress
//! samples, the abort boundary and the coins an aborted walk found are Core's.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use dash_consensus::coins::Coin;
use dash_consensus::coins::compress::MAX_SPECIAL_SCRIPT_LEN;
use dash_primitives::{BlockHash, TxHash};
use dash_types::Numeric as _;
use dashd_store::KvEngine;

use super::table::CoinsTable;
use crate::chain::reply::{ActorReply, reply, spawn_thread};
use crate::store::{CoinsReadError, GuardedEngine, GuardedSnapshot, Meta};

/// Every this many coins the walk calls the interruption point and reads the abort flag.
const ABORT_INTERVAL: i64 = 8192;
/// Every this many coins the walk updates its progress.
const PROGRESS_INTERVAL: i64 = 256;

/// What a walk looks for and reports through: Core's `needles`, `g_scan_progress` and
/// `g_should_abort_scan`, and `rpc_interruption_point`.
pub struct CoinsScan {
    /// The output scripts to find.
    pub needles: HashSet<Vec<u8>>,
    /// The approximate percent done, which the walk sets to 0 at its start, from the current
    /// key's first two txid bytes every 256th coin, and to 100 at a complete end.
    pub progress: Arc<AtomicI32>,
    /// Ends the walk early, unsuccessfully, at the next 8192nd coin.
    pub should_abort: Arc<AtomicBool>,
    /// Core's `rpc_interruption_point`, called every 8192nd coin: `true` ends the walk with
    /// [`CoinsScanError::Interrupted`].
    pub interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// A walk's result. A walk that stops early, aborted or at a record it cannot read, is not
/// successful but keeps what it found and counted (Core's `false` return).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinsScanResult {
    /// Whether the walk read every coin.
    pub success: bool,
    /// The coins read, Core's `count`: the one at which an abort stopped the walk included.
    pub count: i64,
    /// The block the coins are at (Core's active tip, read with the cursor under `cs_main`).
    pub best_block: BlockHash,
    /// The coins whose script is a needle, in Core's `std::map<COutPoint, Coin>` order: txid
    /// (its bytes as stored), then vout.
    pub coins: BTreeMap<([u8; 32], u32), Coin>,
}

impl CoinsScanResult {
    /// The found coins with their outpoints, in order.
    pub fn into_outpoints(self) -> impl Iterator<Item = (TxHash, u32, Coin)> {
        self.coins.into_iter().map(|((txid, vout), coin)| (TxHash::from_lendian(txid), vout, coin))
    }
}

/// Why a walk has no result.
#[derive(Debug, thiserror::Error)]
pub enum CoinsScanError {
    /// The coins' meta record could not be read.
    #[error(transparent)]
    Read(#[from] CoinsReadError),
    /// The coins have no best block, which a flushed chainstate always has.
    #[error("the coins have no best block")]
    NoBestBlock,
    /// The interruption point fired (shutdown): Core's throws "Shutting down".
    #[error("interrupted")]
    Interrupted,
    /// The walk's thread ended without a result.
    #[error("the scan's thread is gone")]
    Gone,
}

/// A walk running on its own thread, its snapshot taken.
pub struct CoinsScanJob {
    result: ActorReply<Result<CoinsScanResult, CoinsScanError>>,
    snapshot_epoch: Option<u64>,
}

impl CoinsScanJob {
    /// The engine epoch the walk's snapshot is at; `None` if its thread died before taking it.
    pub fn snapshot_epoch(&self) -> Option<u64> {
        self.snapshot_epoch
    }

    /// The walk's result, once it is done.
    ///
    /// # Errors
    ///
    /// [`CoinsScanError`]; `Gone` too for a caller LO-9 bars from waiting.
    pub fn wait(self) -> Result<CoinsScanResult, CoinsScanError> {
        self.result.recv().unwrap_or(Err(CoinsScanError::Gone))
    }
}

/// Starts a walk of `db`'s coins for `scan`, returning once its snapshot is taken. The thread
/// drops its snapshot and engine before it sends the result.
///
/// # Errors
///
/// The walk's thread could not be spawned.
pub fn start_coins_scan<E: KvEngine + 'static>(
    db: Arc<GuardedEngine<E>>,
    scan: CoinsScan,
) -> std::io::Result<CoinsScanJob> {
    let (taken_tx, taken) = reply();
    let (result_tx, result) = reply();
    // Detached: `wait` reads its result. The thread never waits on V, so V may wait for the
    // snapshot (LO-9).
    let _detached = spawn_thread("coinscan", move || {
        let snap = db.snapshot();
        taken_tx.send(snap.epoch());
        let found = find_script_pubkey(&snap, &scan);
        drop(snap);
        drop(db);
        result_tx.send(found);
    })?;
    let snapshot_epoch = taken.recv().ok();
    Ok(CoinsScanJob {
        result,
        snapshot_epoch,
    })
}

/// `FindScriptPubKey`, with the tip Core reads beside the cursor.
// Core: rpc/blockchain.cpp:2564-2593, 2766-2774
fn find_script_pubkey<E: KvEngine>(
    snap: &GuardedSnapshot<'_, E>,
    scan: &CoinsScan,
) -> Result<CoinsScanResult, CoinsScanError> {
    let best_block = Meta::read(snap)?.tip.ok_or(CoinsScanError::NoBestBlock)?;
    let mut result = CoinsScanResult {
        success: false,
        count: 0,
        best_block,
        coins: BTreeMap::new(),
    };
    scan.progress.store(0, Ordering::SeqCst);
    let mut buf = [0; MAX_SPECIAL_SCRIPT_LEN];
    for item in CoinsTable::default().cursor(snap) {
        // Core's `GetKey`/`GetValue` failing: the walk stops, unsuccessful.
        let Ok((key, coin)) = item else {
            return Ok(result);
        };
        result.count += 1;
        if result.count % ABORT_INTERVAL == 0 {
            if (scan.interrupted)() {
                return Err(CoinsScanError::Interrupted);
            }
            if scan.should_abort.load(Ordering::SeqCst) {
                return Ok(result);
            }
        }
        if result.count % PROGRESS_INTERVAL == 0 {
            let high = 0x100 * u32::from(key.0[0]) + u32::from(key.0[1]);
            scan.progress.store((f64::from(high) * 100.0 / 65536.0 + 0.5) as i32, Ordering::SeqCst);
        }
        let coin = coin.as_ref();
        if scan.needles.contains(coin.script_pubkey().expand(&mut buf)) {
            let txid: [u8; 32] = key.0[..32].try_into().expect("32 of 36 bytes");
            result.coins.entry((txid, key.vout())).or_insert_with(|| coin.to_coin());
        }
    }
    scan.progress.store(100, Ordering::SeqCst);
    result.success = true;
    Ok(result)
}
