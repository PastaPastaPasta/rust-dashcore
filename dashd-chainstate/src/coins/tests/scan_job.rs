//! `scantxoutset`'s walk (RP1b-1a): Core's `FindScriptPubKey` cadence (progress every 256th
//! coin from the key's first two txid bytes, the interruption point and abort flag every
//! 8192nd), its results in `std::map<COutPoint, Coin>` order, and the snapshot taken before
//! [`start_coins_scan`] returns.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::Coin;
use dash_primitives::OutPoint;
use dash_types::Numeric as _;
use dashd_store::{Commit, MemoryEngine};

use super::*;
use crate::coins::{CoinsScan, CoinsScanError, DB_KEY_MAX, start_coins_scan};
use crate::store::{GlobalFailStop, Meta};

const TIP: [u8; 32] = [7; 32];

fn db_with(
    coins: &[(OutPointKey, Vec<u8>)],
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
        CoinsTable::default().put(&mut b, k, coin);
    }
    if let Some(tip) = tip {
        Meta::put_tip(&mut b, &tip);
    }
    db.write(b, Commit::Buffered).unwrap();
    db
}

fn db_key(k: &OutPointKey) -> Vec<u8> {
    let mut buf = [0; DB_KEY_MAX];
    let n = k.db_key(&mut buf);
    buf[..n].to_vec()
}

/// `n` P2PKH coins at random outpoints, the script of coin `i` filled with `i % 251`, sorted in
/// the column family's order.
fn coins(seed: u64, n: u32) -> Vec<(OutPointKey, Vec<u8>)> {
    let mut r = Rng::new(seed);
    let mut coins: Vec<_> = (0..n)
        .map(|i| (random_key(&mut r, i % 3), p2pkh(u64::from(i % 251)).as_bytes().to_vec()))
        .collect();
    coins.sort_by_key(|(k, _)| db_key(k));
    coins
}

fn script_of(coin: &[u8]) -> Vec<u8> {
    let coin = Coin::decode(&mut &coin[..]).unwrap();
    coin.script_pubkey().as_slice().to_vec()
}

/// A walk for `needles`, with its progress and abort flag, and `interrupted` as the
/// interruption point.
fn scan(
    needles: impl IntoIterator<Item = Vec<u8>>,
    interrupted: Arc<dyn Fn() -> bool + Send + Sync>,
) -> (CoinsScan, Arc<AtomicI32>, Arc<AtomicBool>) {
    let progress = Arc::new(AtomicI32::new(-1));
    let should_abort = Arc::new(AtomicBool::new(false));
    let scan = CoinsScan {
        needles: needles.into_iter().collect::<HashSet<_>>(),
        progress: Arc::clone(&progress),
        should_abort: Arc::clone(&should_abort),
        interrupted,
    };
    (scan, progress, should_abort)
}

fn never() -> Arc<dyn Fn() -> bool + Send + Sync> {
    Arc::new(|| false)
}

/// Core's progress for the key at `k`: `(0x100 * h[0] + h[1]) * 100 / 65536 + 0.5`, truncated.
fn progress_at(k: &OutPointKey) -> i32 {
    let high = 0x100 * u32::from(k.0[0]) + u32::from(k.0[1]);
    (f64::from(high) * 100.0 / 65536.0 + 0.5) as i32
}

/// A complete walk counts every coin, ends at progress 100, and finds each coin whose script
/// is a needle, once however many needles name it.
// Core: rpc/blockchain.cpp:2564-2593
#[test]
fn a_walk_finds_the_needles_coins_and_counts_every_coin() {
    let all = coins(5, 600);
    let db = db_with(&all, Some(BlockHash::from(TIP)));
    let wanted = [3usize, 10, 250, 599];
    let mut needles: Vec<Vec<u8>> = wanted.iter().map(|&i| script_of(&all[i].1)).collect();
    needles.push(script_of(&all[3].1));
    needles.push(vec![0x51]);
    let (s, progress, _) = scan(needles.clone(), never());
    let r = start_coins_scan(db, s).unwrap().wait().unwrap();
    assert!(r.success);
    assert_eq!(r.count, 600);
    assert_eq!(r.best_block, BlockHash::from(TIP));
    assert_eq!(progress.load(Ordering::SeqCst), 100);
    let mut want: Vec<_> = all
        .iter()
        .filter(|(_, c)| needles.contains(&script_of(c)))
        .map(|(k, c)| ((k.0[..32].to_vec(), k.vout()), Coin::decode(&mut &c[..]).unwrap()))
        .collect();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let got: Vec<_> = r.coins.iter().map(|((t, v), c)| ((t.to_vec(), *v), c.clone())).collect();
    assert_eq!(got, want);
    assert!(got.len() >= wanted.len());
}

/// The progress the interruption point sees at the 8192nd and 16384th coins is the one set at
/// the 7936th and 16128th, from those keys' first txid bytes: the abort check runs before the
/// progress update at a coin that is both.
// Core: rpc/blockchain.cpp:2572-2583
#[test]
fn progress_follows_cores_cadence_and_formula() {
    let all = coins(11, 17_000);
    let db = db_with(&all, Some(BlockHash::from(TIP)));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (progress, record) = (Arc::new(AtomicI32::new(-1)), Arc::clone(&seen));
    let probe = Arc::clone(&progress);
    let interrupted: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        record.lock().unwrap().push(probe.load(Ordering::SeqCst));
        false
    });
    let (mut s, _, _) = scan([], interrupted);
    s.progress = Arc::clone(&progress);
    let r = start_coins_scan(db, s).unwrap().wait().unwrap();
    assert_eq!(r.count, 17_000);
    let want = vec![progress_at(&all[7935].0), progress_at(&all[16127].0)];
    assert_eq!(*seen.lock().unwrap(), want);
    // The two samples differ, so neither is a constant.
    assert_ne!(want[0], want[1]);
    assert_eq!(progress.load(Ordering::SeqCst), 100);
}

/// An abort set before the walk stops it at the 8192nd coin, counted but not matched,
/// unsuccessful, with the coins found before it and the progress where it was.
// Core: rpc/blockchain.cpp:2573-2579
#[test]
fn an_abort_stops_the_walk_at_the_8192nd_coin() {
    // Distinct scripts for the 8191st, 8192nd and 8193rd coins.
    let mut all = coins(13, 9_000);
    for (i, fill) in [(8190usize, 252u8), (8191, 253), (8192, 254)] {
        let out = dash_primitives::TxOut {
            value: 5,
            script_pubkey: p2pkh_script(fill).into(),
        };
        all[i].1 = PackedCoin::from_output(&out, 9, false).unwrap().as_bytes().to_vec();
    }
    let db = db_with(&all, Some(BlockHash::from(TIP)));
    let needles = [8190usize, 8191, 8192].map(|i| script_of(&all[i].1));
    let (s, progress, abort) = scan(needles, never());
    abort.store(true, Ordering::SeqCst);
    let r = start_coins_scan(db, s).unwrap().wait().unwrap();
    assert!(!r.success);
    assert_eq!(r.count, 8192);
    let found: Vec<u32> = r.coins.keys().map(|(_, v)| *v).collect();
    assert_eq!(r.coins.len(), 1, "only the 8191st coin: {found:?}");
    let ((txid, vout), _) = r.coins.iter().next().unwrap();
    assert_eq!((&txid[..], *vout), (&all[8190].0.0[..32], all[8190].0.vout()));
    assert_eq!(progress.load(Ordering::SeqCst), progress_at(&all[7935].0));
}

/// The interruption point ends the walk with no result, at the 8192nd coin; a walk shorter
/// than that never calls it.
#[test]
fn the_interruption_point_runs_every_8192nd_coin() {
    let calls = Arc::new(Mutex::new(0));
    let counted = Arc::clone(&calls);
    let interrupted: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        *counted.lock().unwrap() += 1;
        true
    });
    let db = db_with(&coins(17, 8_191), Some(BlockHash::from(TIP)));
    let (s, ..) = scan([], Arc::clone(&interrupted));
    assert!(start_coins_scan(db, s).unwrap().wait().unwrap().success);
    assert_eq!(*calls.lock().unwrap(), 0);
    let db = db_with(&coins(17, 8_192), Some(BlockHash::from(TIP)));
    let (s, ..) = scan([], interrupted);
    assert!(matches!(start_coins_scan(db, s).unwrap().wait(), Err(CoinsScanError::Interrupted)));
    assert_eq!(*calls.lock().unwrap(), 1);
}

/// The walk reads the snapshot taken before `start_coins_scan` returns: a coin and a new tip
/// written while it is held at the 8192nd coin are not seen.
#[test]
fn the_walk_reads_the_snapshot_taken_at_its_start() {
    let all = coins(19, 8_500);
    let db = db_with(&all, Some(BlockHash::from(TIP)));
    let needle = p2pkh_script(255);
    let written = Arc::new(AtomicBool::new(false));
    let landed = Arc::clone(&written);
    let held: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        while !landed.load(Ordering::SeqCst) {
            std::thread::yield_now();
        }
        false
    });
    let (s, ..) = scan([needle.clone()], held);
    let job = start_coins_scan(Arc::clone(&db), s).unwrap();
    let mut b = db.batch();
    let out = dash_primitives::TxOut {
        value: 9,
        script_pubkey: needle.into(),
    };
    let k = OutPointKey::new(&OutPoint {
        hash: dash_primitives::TxHash::from_lendian([0; 32]),
        index: 0,
    });
    CoinsTable::default().put(
        &mut b,
        &k,
        PackedCoin::from_output(&out, 9, false).unwrap().as_bytes(),
    );
    Meta::put_tip(&mut b, &BlockHash::from([8; 32]));
    db.write(b, Commit::Buffered).unwrap();
    written.store(true, Ordering::SeqCst);
    let r = job.wait().unwrap();
    assert!(r.success);
    assert_eq!(r.count, 8_500);
    assert!(r.coins.is_empty());
    assert_eq!(r.best_block, BlockHash::from(TIP));
}

/// The coins come in `COutPoint` order, vout numeric within a txid, though the column family
/// puts `VARINT(16512)` before `VARINT(16383)`.
// Core: rpc/blockchain.cpp:2585-2587, 2780; primitives/transaction.h COutPoint::operator<
#[test]
fn the_coins_come_in_coutpoint_order() {
    let script = p2pkh_script(9);
    let out = dash_primitives::TxOut {
        value: 1,
        script_pubkey: script.clone().into(),
    };
    let coin = PackedCoin::from_output(&out, 9, false).unwrap().as_bytes().to_vec();
    let key = |byte: u8, index: u32| {
        OutPointKey::new(&OutPoint {
            hash: dash_primitives::TxHash::from_lendian([byte; 32]),
            index,
        })
    };
    let keys = [key(2, 16383), key(2, 16512), key(2, 128), key(1, 5), key(2, 0)];
    let db =
        db_with(&keys.iter().map(|k| (*k, coin.clone())).collect::<Vec<_>>(), Some(TIP.into()));
    let (s, ..) = scan([script], never());
    let r = start_coins_scan(db, s).unwrap().wait().unwrap();
    let got: Vec<(u8, u32)> = r.coins.keys().map(|(t, v)| (t[0], *v)).collect();
    assert_eq!(got, [(1, 5), (2, 0), (2, 128), (2, 16383), (2, 16512)]);
    assert!(db_key(&key(2, 16512)) < db_key(&key(2, 16383)));
}

/// A record that does not decode stops the walk unsuccessfully, keeping what it found and
/// counted before it (Core's `GetValue` failing); no best block is an error.
#[test]
fn a_bad_record_ends_the_walk_unsuccessfully_and_a_best_block_is_needed() {
    let mut all = coins(23, 40);
    let needles = [script_of(&all[2].1), script_of(&all[30].1)];
    all[20].1 = vec![0xff];
    let db = db_with(&all, Some(BlockHash::from(TIP)));
    let (s, progress, _) = scan(needles, never());
    let r = start_coins_scan(db, s).unwrap().wait().unwrap();
    assert!(!r.success);
    assert_eq!(r.count, 20);
    assert_eq!(r.coins.len(), 1);
    assert_ne!(progress.load(Ordering::SeqCst), 100);

    let db = db_with(&coins(23, 4), None);
    let (s, ..) = scan([], never());
    assert!(matches!(start_coins_scan(db, s).unwrap().wait(), Err(CoinsScanError::NoBestBlock)));
}
