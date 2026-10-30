//! `gettxoutsetinfo`'s scan (RP1a D2): it reads the snapshot taken before
//! [`start_coins_stats`] returns, in the column family's order, with Core's statistics.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::coins::stats::{CoinStatsHashType, UtxoSetScan};
use dashd_store::{Commit, KvEngine, MemoryEngine};

use super::*;
use crate::coins::{CoinsStatsError, DB_KEY_MAX, start_coins_stats};
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
        let job = start_coins_stats(Arc::clone(&db), hash_type, never()).unwrap();
        let mut b = db.batch();
        CoinsTable::default().put(&mut b, &random_key(&mut r, 0), p2pkh(99).as_bytes());
        Meta::put_tip(&mut b, &BlockHash::from([8; 32]));
        db.write(b, Commit::Buffered).unwrap();

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
    }
}

/// Core's interruption point stops the scan; coins with no best block are no stats.
#[test]
fn the_scan_stops_when_interrupted_and_needs_a_best_block() {
    let mut r = Rng::new(42);
    let coins: Vec<_> = (0..5).map(|i| (random_key(&mut r, 0), p2pkh(i))).collect();
    let stop = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&stop);
    let db = db_with(&coins, Some(BlockHash::from([1; 32])));
    let job = start_coins_stats(
        db,
        CoinStatsHashType::MuHash,
        Arc::new(move || flag.load(Ordering::SeqCst)),
    );
    assert!(matches!(job.unwrap().wait(), Err(CoinsStatsError::Interrupted)));
    let job = start_coins_stats(db_with(&coins, None), CoinStatsHashType::None, never()).unwrap();
    assert!(matches!(job.wait(), Err(CoinsStatsError::NoBestBlock)));
    // An empty set at a best block is all zeros.
    let job = start_coins_stats(
        db_with(&[], Some(BlockHash::from([1; 32]))),
        CoinStatsHashType::None,
        never(),
    );
    let empty = job.unwrap().wait().unwrap();
    assert_eq!(
        (empty.stats.transactions, empty.stats.txouts, empty.stats.total_amount),
        (0, 0, Some(0))
    );
}
