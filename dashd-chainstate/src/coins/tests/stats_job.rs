//! `gettxoutsetinfo`'s scan (RP1a D2): it reads the snapshot taken before
//! [`start_coins_stats`] returns, in the column family's order, with Core's statistics; and
//! `dumptxoutset`'s file, which it writes from that snapshot (RP1b-1b).

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan};
use dash_types::Numeric as _;
use dashd_store::{Commit, KvEngine, MemoryEngine};

use super::*;
use crate::coins::{CoinsDump, CoinsStatsError, DB_KEY_MAX, start_coins_stats};
use crate::store::{GlobalFailStop, Meta};

const TYPES: [CoinStatsHashType; 3] =
    [CoinStatsHashType::HashSerialized, CoinStatsHashType::MuHash, CoinStatsHashType::None];

fn db_with(
    coins: &[(OutPointKey, PackedCoin)],
    tip: Option<BlockHash>,
) -> Arc<GuardedEngine<MemoryEngine>> {
    let engine = MemoryEngine::new(&crate::store::CHAINSTATE_SCHEMA);
    let db = Arc::new(GuardedEngine::new(
        engine,
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    ));
    let mut b = db.batch();
    for (k, coin) in coins {
        CoinsTable::default().put(&mut b, k, coin.as_bytes());
    }
    if let Some(tip) = tip {
        Meta::put_tip(&mut b, &tip);
    }
    db.write(b, Commit::Buffered).unwrap();
    db
}

fn never() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| false)
}

/// A file the test reads back once the scan is done.
#[derive(Clone, Default)]
struct SharedFile(Arc<Mutex<Vec<u8>>>);

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedFile {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }

    /// A dump into this file, recording the best block `on_start` saw.
    fn dump(&self, started: &Arc<Mutex<Option<BlockHash>>>) -> Option<CoinsDump> {
        let started = Arc::clone(started);
        Some(CoinsDump {
            file: Box::new(self.clone()),
            on_start: Box::new(move |best| *started.lock().unwrap() = Some(*best)),
        })
    }
}

/// The file Core writes for `coins` at `base`, the coins in the order given.
fn layout(base: &BlockHash, coins: &[(OutPointKey, PackedCoin)]) -> Vec<u8> {
    let mut bytes = base.to_lendian().to_vec();
    bytes.extend_from_slice(&(coins.len() as u64).to_le_bytes());
    for (k, coin) in coins {
        bytes.extend_from_slice(&k.0);
        coin.as_ref().write(&mut bytes);
    }
    bytes
}

/// Each hash type's statistics are a direct scan's over the coins in database key order, at the
/// meta tip, and a write after the start (a coin and a new tip) is not seen.
#[test]
fn the_scan_reads_the_snapshot_taken_at_its_start() {
    let mut r = Rng::new(41);
    let mut coins: Vec<_> =
        (0..30).map(|i| (random_key(&mut r, i % 4), p2pkh(u64::from(i)))).collect();
    let tip = BlockHash::from([7; 32]);
    let db_key = |k: &OutPointKey| {
        let mut buf = [0; DB_KEY_MAX];
        let n = k.db_key(&mut buf);
        buf[..n].to_vec()
    };
    let unsorted = coins.clone();
    coins.sort_by_key(|(k, _)| db_key(k));
    for hash_type in TYPES {
        let db = db_with(&unsorted, Some(tip));
        // `dumptxoutset` scans with `HASH_SERIALIZED`; the file does not depend on the type.
        let (file, started) = (SharedFile::default(), Arc::default());
        // The scan waits at its first coin until the write below has landed, so a walk of any
        // later snapshot (the statistics' or the file's) would see it.
        let written = Arc::new(AtomicBool::new(false));
        let landed = Arc::clone(&written);
        let held: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            while !landed.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            false
        });
        let job = start_coins_stats(Arc::clone(&db), hash_type, held, file.dump(&started)).unwrap();
        let mut b = db.batch();
        CoinsTable::default().put(&mut b, &random_key(&mut r, 0), p2pkh(99).as_bytes());
        Meta::put_tip(&mut b, &BlockHash::from([8; 32]));
        db.write(b, Commit::Buffered).unwrap();
        written.store(true, Ordering::SeqCst);

        let got = job.wait().unwrap();
        let mut want = UtxoSetScan::new(hash_type, &tip);
        for (k, coin) in &coins {
            want.push(&k.outpoint(), &coin.as_ref());
        }
        assert_eq!(got.best_block, tip, "{hash_type:?}");
        assert_eq!(got.stats, want.finish(), "{hash_type:?}");
        assert_eq!(got.stats.txouts, 30);
        assert_eq!(got.stats.hash.is_some(), hash_type != CoinStatsHashType::None);
        let later = db.snapshot();
        assert_eq!(CoinsTable::default().cursor(&later).count(), 31, "the write landed");
        assert_eq!(file.bytes(), layout(&tip, &coins), "{hash_type:?}: the snapshot's coins");
        assert_eq!(*started.lock().unwrap(), Some(tip));
    }
}

/// CH3a D165's order, by hand: Core's `SnapshotMetadata` (base hash, `u64` count), then each
/// `COutPoint` (txid, `u32` index) and `Coin` (`VARINT(height * 2 + coinbase)`, the compressed
/// amount, the compressed script) in the coins database's key order, `'C' ‖ txid ‖ VARINT(n)`:
/// `VARINT(16512)` = `808000` comes before `VARINT(16383)` = `fe7f`.
// Core: rpc/blockchain.cpp:3188-3205; node/utxo_snapshot.h:38; primitives/transaction.h:39;
// coins.h:61; compressor.cpp:149-168 CompressAmount, compressor.h:63-72 (a script of no
// special form: VARINT(size + 6), then its bytes)
#[test]
fn a_dump_writes_core_s_layout_in_key_order() {
    let key = |vout: u32| {
        let mut k = [1; 36];
        k[32..].copy_from_slice(&vout.to_le_bytes());
        OutPointKey(k)
    };
    let op_true = |value, height, coinbase| {
        let out = TxOut {
            value,
            script_pubkey: vec![0x51].into(),
        };
        PackedCoin::from_output(&out, height, coinbase).unwrap()
    };
    let coins = [(key(16_383), op_true(5_000, 3, false)), (key(16_512), op_true(1, 4, true))];
    // An asymmetric hash: the metadata holds it in Core's internal order, not its display order.
    let base = BlockHash::from(std::array::from_fn(|i| i as u8));
    let (file, started) = (SharedFile::default(), Arc::default());
    let job = start_coins_stats(
        db_with(&coins, Some(base)),
        CoinStatsHashType::HashSerialized,
        never(),
        file.dump(&started),
    );
    assert_eq!(job.unwrap().wait().unwrap().stats.txouts, 2);
    let mut want: Vec<u8> = (0..32).collect();
    want.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 0]);
    want.extend_from_slice(&[1; 32]);
    want.extend_from_slice(&[0x80, 0x40, 0, 0, 0x09, 0x01, 0x07, 0x51]);
    want.extend_from_slice(&[1; 32]);
    want.extend_from_slice(&[0xff, 0x3f, 0, 0, 0x06, 0x2c, 0x07, 0x51]);
    assert_eq!(hex(&file.bytes()), hex(&want));
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The file's interruption point is every 5000th coin from the first, after the statistics'
/// own; a failed write is the scan's error.
// Core: rpc/blockchain.cpp:3196-3197
#[test]
fn a_dump_stops_when_interrupted_or_unwritable() {
    let mut r = Rng::new(43);
    let coins: Vec<_> = (0..5_001).map(|i| (random_key(&mut r, 0), p2pkh(i))).collect();
    let base = BlockHash::from([3; 32]);
    let db = db_with(&coins, Some(base));
    // The statistics ask at each coin, the file at coins 0 and 5000.
    for (fire_at, interrupted) in [(5_001, true), (5_002, true), (5_003, false)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&calls);
        let at = move || seen.fetch_add(1, Ordering::SeqCst) == fire_at;
        let (file, started) = (SharedFile::default(), Arc::default());
        let job = start_coins_stats(
            Arc::clone(&db),
            CoinStatsHashType::HashSerialized,
            Arc::new(at),
            file.dump(&started),
        );
        let got = job.unwrap().wait();
        assert_eq!(matches!(got, Err(CoinsStatsError::Interrupted)), interrupted, "{fire_at}");
        assert_eq!(calls.load(Ordering::SeqCst), 5_003.min(fire_at + 1), "{fire_at}");
        assert_eq!(*started.lock().unwrap(), Some(base), "{fire_at}");
    }

    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let full = || {
        Some(CoinsDump {
            file: Box::new(Full),
            on_start: Box::new(|_| {}),
        })
    };
    let job = start_coins_stats(db, CoinStatsHashType::HashSerialized, never(), full());
    assert!(matches!(job.unwrap().wait(), Err(CoinsStatsError::Write(_))));
    // Two coins stay in the buffer until the final flush, the write Core's ignored `fclose`
    // leaves unreported; rdashd reports it (D55's permanent divergence).
    let small = db_with(&coins[..2], Some(base));
    let job = start_coins_stats(small, CoinStatsHashType::HashSerialized, never(), full());
    assert!(matches!(job.unwrap().wait(), Err(CoinsStatsError::Write(_))));
}

/// Core's interruption point stops the scan; coins with no best block are no stats. A dump
/// whose statistics fail writes nothing and never starts its timer: Core throws before its
/// `LOG_TIME_SECONDS` scope and the metadata.
// Core: rpc/blockchain.cpp:3176-3177, :3184-3188
#[test]
fn the_scan_stops_when_interrupted_and_needs_a_best_block() {
    let mut r = Rng::new(42);
    let coins: Vec<_> = (0..5).map(|i| (random_key(&mut r, 0), p2pkh(i))).collect();
    let stop = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&stop);
    let db = db_with(&coins, Some(BlockHash::from([1; 32])));
    let (file, started) = (SharedFile::default(), Arc::default());
    let job = start_coins_stats(
        db,
        CoinStatsHashType::MuHash,
        Arc::new(move || flag.load(Ordering::SeqCst)),
        file.dump(&started),
    );
    assert!(matches!(job.unwrap().wait(), Err(CoinsStatsError::Interrupted)));
    assert_eq!(*started.lock().unwrap(), None, "no timer for interrupted statistics");
    assert!(file.bytes().is_empty(), "no metadata for interrupted statistics");
    let (file, started) = (SharedFile::default(), Arc::default());
    let job = start_coins_stats(
        db_with(&coins, None),
        CoinStatsHashType::None,
        never(),
        file.dump(&started),
    )
    .unwrap();
    assert!(matches!(job.wait(), Err(CoinsStatsError::NoBestBlock)));
    assert_eq!(*started.lock().unwrap(), None, "no timer without a best block");
    assert!(file.bytes().is_empty(), "no metadata without a best block");
    // An empty set at a best block is all zeros.
    let job = start_coins_stats(
        db_with(&[], Some(BlockHash::from([1; 32]))),
        CoinStatsHashType::None,
        never(),
        None,
    );
    let empty = job.unwrap().wait().unwrap();
    assert_eq!(
        (empty.stats.transactions, empty.stats.txouts, empty.stats.total_amount),
        (0, 0, Some(0))
    );
}
