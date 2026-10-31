//! The `coins` column family's reads (CH2 §2.6, §7.1 a3): the cursor, `multi_get`, `disk_size`,
//! and `corrupt_coin_aborts` (DEC-78) on both engines, with X21.

use std::sync::Arc;

use dash_consensus::coins::CoinsView as _;
use dashd_store::{ErrorKind, KvEngine, MemoryEngine};

use super::*;
use crate::coins::{BlockOverlay, DB_KEY_MAX};
use crate::store::{AbortMark, CoinsReadError, GlobalFailStop};

/// The cursor reads a snapshot's coins in the column family's byte order, which is not numeric
/// vout order (`VARINT(16383)` = `fe7f` sorts after `VARINT(16512)` = `808000`), and does not see
/// a later write.
#[test]
fn the_cursor_reads_a_snapshot_in_key_order() {
    let w = World::with_dbcache(16);
    let mut r = Rng::new(31);
    let mut keys: Vec<_> = (0..40).map(|i| random_key(&mut r, i % 3)).collect();
    keys.extend([16_383, 16_512, 0, 127, 128].map(|vout| random_key(&mut Rng::new(1), vout)));
    for (i, k) in keys.iter().enumerate() {
        w.db_set(k, Some(&p2pkh(i as u64)));
    }
    let snap = w.db.snapshot();
    w.db_set(&random_key(&mut r, 0), Some(&p2pkh(99)));
    let db_key = |k: &OutPointKey| {
        let mut buf = [0; DB_KEY_MAX];
        let n = k.db_key(&mut buf);
        buf[..n].to_vec()
    };
    let mut want: Vec<_> =
        keys.iter().enumerate().map(|(i, k)| (db_key(k), *k, p2pkh(i as u64))).collect();
    want.sort_by(|a, b| a.0.cmp(&b.0));
    let got: Vec<_> = CoinsTable::default().cursor(&snap).map(Result::unwrap).collect();
    assert_eq!(got.len(), keys.len(), "the later write is not in the snapshot");
    for ((_, k, coin), (got_k, got_coin)) in want.iter().zip(&got) {
        assert_eq!((k, coin.as_bytes()), (got_k, got_coin.as_bytes()));
    }
    let vouts: Vec<_> = got.iter().map(|(k, _)| k.vout()).filter(|v| *v > 3).collect();
    // `7f` < `8000` < `808000` < `fe7f`.
    assert_eq!(vouts, [127, 128, 16_512, 16_383]);
}

/// `multi_get` answers in the keys' order, absent and repeated keys included, with one point read
/// per key, from the engine and from a snapshot alike.
#[test]
fn multi_get_answers_in_the_keys_order() {
    let w = World::with_dbcache(16);
    let mut r = Rng::new(32);
    let (a, b, absent) = (random_key(&mut r, 0), random_key(&mut r, 5), random_key(&mut r, 1));
    w.db_set(&a, Some(&p2pkh(1)));
    w.db_set(&b, Some(&coin(7)));
    let keys = [b, absent, a, b];
    let table = CoinsTable::default();
    let before = w.db_reads();
    let snap = w.db.snapshot();
    for got in [table.multi_get(&*w.db, &keys).unwrap(), table.multi_get(&snap, &keys).unwrap()] {
        let want = keys.map(|k| w.db_get(&k));
        let bytes = |v: &[Option<PackedCoin>]| -> Vec<_> {
            v.iter().map(|c| c.as_ref().map(|c| c.as_bytes().to_vec())).collect()
        };
        assert_eq!(bytes(&got), bytes(&want));
    }
    // Two multi_gets and the four `db_get`s of each check: four reads apiece.
    assert_eq!(w.db_reads() - before, 4 * 4);
}

/// A snapshot's `multi_get` reads the snapshot: after it, coins are overwritten, erased and
/// created, and it answers as the snapshot's point reads do, repeated and absent keys included.
/// The live engine's `multi_get` sees the later write.
fn check_snapshot_multi_get<E: KvEngine>(engine: E) {
    let db = GuardedEngine::new(engine, GlobalFailStop::new(), Arc::new(RecordingSink::default()));
    let table = CoinsTable::default();
    let mut r = Rng::new(36);
    let [a, b, c, d, absent] = [0, 1, 2, 3, 4].map(|vout| random_key(&mut r, vout));
    let mut batch = db.batch();
    for (k, i) in [(a, 1), (b, 2), (c, 3)] {
        table.put(&mut batch, &k, p2pkh(i).as_bytes());
    }
    db.write(batch, Commit::Buffered).unwrap();
    let snap = db.snapshot();
    let mut batch = db.batch();
    table.put(&mut batch, &a, p2pkh(10).as_bytes());
    table.erase(&mut batch, &b);
    table.put(&mut batch, &d, p2pkh(11).as_bytes());
    db.write(batch, Commit::Buffered).unwrap();

    let keys = [d, a, b, absent, c, a, b];
    let some = |i| Some(p2pkh(i));
    let before = [None, some(1), some(2), None, some(3), some(1), some(2)];
    let point: Vec<_> = keys.iter().map(|k| table.get(&snap, k).unwrap()).collect();
    assert_eq!(point, before, "the snapshot's point reads");
    assert_eq!(table.multi_get(&snap, &keys).unwrap(), before, "the snapshot's multi_get");
    let after = [some(11), some(10), None, None, some(3), some(10), None];
    assert_eq!(table.multi_get(&db, &keys).unwrap(), after, "the live multi_get");
}

#[test]
fn a_snapshots_multi_get_reads_the_snapshot() {
    check_snapshot_multi_get(memory());
    #[cfg(feature = "rocksdb")]
    {
        let mut dirs = Vec::new();
        check_snapshot_multi_get(rocks_engine(&mut dirs));
    }
}

/// One read path of `corrupt_coin_aborts`: whether it found the coin at `k`.
type ReadPath<E> = fn(
    &mut CoinsCache<E>,
    &GuardedEngine<E>,
    &CoinsTable,
    &OutPointKey,
) -> Result<bool, CoinsReadError>;

/// One read path's outcome: whether it found the coin, or the error it returned, with the
/// database and the sink it read through.
struct PathRead<E: KvEngine> {
    path: &'static str,
    found: Result<bool, CoinsReadError>,
    db: Arc<GuardedEngine<E>>,
    sink: Arc<RecordingSink>,
}

/// Each read path of `corrupt_coin_aborts`, each over a new engine from `open` whose only coin is
/// `value`.
fn read_paths<E: KvEngine>(
    mut open: impl FnMut() -> E,
    plant: bool,
    value: &[u8],
) -> Vec<PathRead<E>> {
    let k = random_key(&mut Rng::new(33), 2);
    let paths: [(&str, ReadPath<E>); 7] = [
        ("fetch", |cache, _, _, k| cache.fetch(k).map(|c| c.is_some())),
        ("the overlay's coin", |cache, _, _, k| {
            BlockOverlay::new(cache).coin(&k.outpoint()).map(|c| c.is_some())
        }),
        ("the overlay's have_coin", |cache, _, _, k| {
            BlockOverlay::new(cache).have_coin(&k.outpoint())
        }),
        ("the cursor", |_, db, table, _| {
            table.cursor(&db.snapshot()).next().transpose().map(|c| c.is_some())
        }),
        ("a snapshot's point read", |_, db, table, k| {
            table.get(&db.snapshot(), k).map(|c| c.is_some())
        }),
        ("multi_get", |_, db, table, k| table.multi_get(db, &[*k]).map(|c| c[0].is_some())),
        ("a snapshot's multi_get", |_, db, table, k| {
            table.multi_get(&db.snapshot(), &[*k]).map(|c| c[0].is_some())
        }),
    ];
    paths
        .into_iter()
        .map(|(path, read)| {
            let sink = Arc::new(RecordingSink::default());
            let db = Arc::new(GuardedEngine::new(open(), GlobalFailStop::new(), sink.clone()));
            #[cfg_attr(not(feature = "planted-bugs"), allow(unused_mut))]
            let mut table = CoinsTable::default();
            let mut b = db.batch();
            table.put(&mut b, &k, value);
            db.write(b, Commit::Buffered).unwrap();
            let mut cache = CoinsCache::new(
                db.clone(),
                CoinsBudget::new(16 * MIB, 0, Ratio::HALF, 0),
                Salt::fixed(3),
                Some(block(1)),
            );
            #[cfg(feature = "planted-bugs")]
            if plant {
                cache.plant(crate::coins::Planted::X21);
                table.plant(crate::coins::Planted::X21);
            }
            let _ = plant;
            let found = read(&mut cache, &db, &table, &k);
            drop(cache);
            PathRead {
                path,
                found,
                db,
                sink,
            }
        })
        .collect()
}

/// The paths at which a corrupt coin did not abort: a read that did not return a `Corruption`
/// error, trip the latch and tell the sink exactly once.
fn corrupt_reads<E: KvEngine>(mut open: impl FnMut() -> E, plant: bool) -> Vec<String> {
    let valid = p2pkh(3);
    let trailing = [valid.as_bytes(), &[0]].concat();
    let mut bad = Vec::new();
    for (what, value) in [("undecodable", &[0x80][..]), ("trailing bytes", &trailing)] {
        for PathRead {
            path,
            found,
            db,
            sink,
        } in read_paths(&mut open, plant, value)
        {
            let aborted = match &found {
                Err(e) => {
                    e.kind() == ErrorKind::Corruption
                        && db.latch().tripped().is_some_and(|f| f.kind == ErrorKind::Corruption)
                        && matches!(&sink.marks()[..], [AbortMark::ReadFailed(m)]
                            if m.contains("corrupt coin record coins/"))
                }
                Ok(_) => false,
            };
            if !aborted {
                let found = found.map_err(|e| e.to_string());
                bad.push(format!("{what} via {path}: {found:?}, marks {:?}", sink.marks()));
            }
        }
    }
    bad
}

fn memory() -> MemoryEngine {
    MemoryEngine::new(&crate::store::CHAINSTATE_SCHEMA)
}

/// DEC-78 (D7): a stored coin that does not decode, or decodes with bytes left over, aborts on
/// every read path: a `Corruption` error after the latch trips and the sink hears it once, never
/// a missing coin.
#[test]
fn corrupt_coin_aborts() {
    assert_eq!(corrupt_reads(memory, false), Vec::<String>::new());
    #[cfg(feature = "rocksdb")]
    {
        let mut dirs = Vec::new();
        assert_eq!(corrupt_reads(|| rocks_engine(&mut dirs), false), Vec::<String>::new());
    }
}

/// X21 (DEC-78 reversed) reads the corrupt coin as missing: every path is caught.
#[cfg(feature = "planted-bugs")]
#[test]
fn x21_is_caught_by_corrupt_coin_aborts() {
    let caught = corrupt_reads(memory, true);
    assert_eq!(caught.len(), 14, "{caught:#?}");
    assert!(caught.iter().all(|c| c.contains("Ok(false)")), "{caught:#?}");
}

/// A key in `coins` that is not a coin's ends the cursor as corruption; Core's stops there
/// silently (D7).
#[test]
fn the_cursor_aborts_on_a_key_that_is_not_a_coins() {
    let w = World::with_dbcache(16);
    let table = CoinsTable::default();
    let mut b = w.db.batch();
    table.put(&mut b, &random_key(&mut Rng::new(34), 0), p2pkh(1).as_bytes());
    dashd_store::KvBatch::put(&mut b, table.cf(), &[0xff; 31], p2pkh(2).as_bytes());
    w.db.write(b, Commit::Buffered).unwrap();
    let snap = w.db.snapshot();
    let items: Vec<_> = table.cursor(&snap).collect();
    assert_eq!(items.len(), 2, "the bad key is the last item");
    let err = items[1].as_ref().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Corruption);
    assert!(err.to_string().contains("not a coin key"), "{err}");
    assert!(w.db.latch().tripped().is_some());
}

/// A chainstate on RocksDB through `RocksTuning`, in a directory kept until the test ends.
#[cfg(feature = "rocksdb")]
fn rocks_engine(dirs: &mut Vec<tempfile::TempDir>) -> dashd_store::RocksEngine {
    let dir = tempfile::tempdir().unwrap();
    let (e, ..) = crate::tuning::open_default_rocks(dir.path());
    dirs.push(dir);
    e
}

/// `rpc_blockchain.py:372-374`: after `gettxoutsetinfo`'s ALWAYS flush, a 200-block regtest
/// chainstate's `disk_size` is above 6,400 and below 64,000 bytes. Its UTXO set is the 200
/// coinbase outputs (`:370`), P2PKH, and ALWAYS on regtest also runs `flush_memtables` and
/// `sync_wal` (§2.6).
#[cfg(feature = "rocksdb")]
#[test]
fn disk_size_of_a_200_block_regtest_chainstate() {
    let mut dirs = Vec::new();
    let db = GuardedEngine::new(
        rocks_engine(&mut dirs),
        GlobalFailStop::new(),
        Arc::new(RecordingSink::default()),
    );
    let table = CoinsTable::default();
    let mut r = Rng::new(35);
    let mut b = db.batch();
    for height in 1..=200 {
        let out = TxOut {
            value: 500 * 100_000_000,
            script_pubkey: p2pkh_script(height as u8).into(),
        };
        let coin = PackedCoin::from_output(&out, height, true).unwrap();
        table.put(&mut b, &random_key(&mut r, 0), coin.as_bytes());
    }
    crate::store::Meta::put_tip(&mut b, &block(200).hash);
    db.write(b, Commit::Buffered).unwrap();
    db.flush_memtables().unwrap();
    db.sync_wal().unwrap();
    let size = table.estimate_size(&db).unwrap();
    println!("disk_size of 200 regtest coins: {size} B");
    assert!(6_400 < size && size < 64_000, "{size}");
}
