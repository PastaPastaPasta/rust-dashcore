//! `oracle_coins_ops` (ABI 15) against Core's own coins-cache behaviour.
//!
//! Provenance: the state tables are transcribed from Core's `src/test/coins_tests.cpp` at the pin
//! (`pins.txt`): `ccoins_access` (`:649-685`), `ccoins_spend` (`:700-735`), `ccoins_add`
//! (`:772-801`) and `ccoins_write` (`:822-879`), each through `SingleEntryCacheTest` (`:620-634`):
//! a base cache over an empty root holding the base value as a DIRTY entry (`WriteCoinsViewEntry`,
//! a `BatchWrite` with the null hash), and a cache over it seeded with the cache value and flags
//! (`InsertCoinsMapEntry`). Here the root is the in-memory `CCoinsViewDB`, empty like Core's
//! `CCoinsView`, the base is the tip and the cache the first child. Every expectation is Core's
//! table; none was read off the oracle. The other tests check the entry point's own contract
//! (oracle.h): the `logic_error` texts (`coins.cpp:84`, `:221`), the assertion outcomes, the
//! database state after a tip flush, `Sync`, determinism and the forced `-dbcrashratio`.

use std::collections::BTreeMap;

use dash_oracle_sys::coins_ops::{DEFAULT_DB_BATCH_SIZE, DIRTY, FRESH, OutPoint, TIP};
use dash_oracle_sys::{
    CacheEntry, Chain, CoinCodec, CoinsOp, CoinsOutcome, CoinsOutput, Oracle, Pin,
};

fn oracle() -> Oracle {
    Oracle::new(Pin::V24_0)
}

/// Core's `OUTPOINT` (`coins_tests.cpp:555`): the null outpoint.
const OUTPOINT: OutPoint = OutPoint {
    txid: [0; 32],
    vout: u32::MAX,
};

// coins_tests.cpp:556-564
const SPENT: i64 = -1;
const ABSENT: i64 = -2;
const FAIL: i64 = -3;
const VALUE1: i64 = 100;
const VALUE2: i64 = 200;
const VALUE3: i64 = 300;
const NO_ENTRY: i16 = -1;
const DF: i16 = (DIRTY | FRESH) as i16;
const D: i16 = DIRTY as i16;
const F: i16 = FRESH as i16;

/// The coin `Coin(CTxOut(value, empty script), height, coinbase)` as the coins database stores it,
/// encoded by Core (`oracle_coin_codec`).
fn coin(value: i64, height: u32, coinbase: bool) -> Vec<u8> {
    let mut input = height.to_le_bytes().to_vec();
    input.push(coinbase.into());
    input.extend(value.to_le_bytes());
    input.push(0);
    let (bytes, _) =
        oracle().coin_codec(CoinCodec::CoinEncode, &input).expect("a coin Core encodes");
    bytes
}

/// `SetCoinsValue` (`:570-580`): height 1, not coinbase.
fn value_coin(value: i64) -> Option<Vec<u8>> {
    (value != SPENT).then(|| coin(value, 1, false))
}

/// `GetCoinsMapEntry` (`:596-614`): the entry's value and flags, from the coins the test made.
fn read_entry(entry: Option<CacheEntry>, values: &BTreeMap<Vec<u8>, i64>) -> (i64, i16) {
    match entry {
        None => (ABSENT, NO_ENTRY),
        Some(CacheEntry {
            flags,
            coin,
        }) => (coin.map_or(SPENT, |c| values[&c]), flags.into()),
    }
}

/// The coins the tables make, by value: `SetCoinsValue`'s, and `ccoins_add`'s coinbase VALUE3.
fn values() -> BTreeMap<Vec<u8>, i64> {
    let mut values: BTreeMap<_, _> =
        [VALUE1, VALUE2, VALUE3].into_iter().map(|v| (coin(v, 1, false), v)).collect();
    values.insert(coin(VALUE3, 1, true), VALUE3);
    values
}

/// A map holding `OUTPOINT` with `value` and `flags`, or nothing for `ABSENT`.
fn entry_map(value: i64, flags: i16) -> Vec<(OutPoint, CacheEntry)> {
    if value == ABSENT {
        return vec![];
    }
    vec![(
        OUTPOINT,
        CacheEntry {
            flags: u8::try_from(flags).expect("flags"),
            coin: value_coin(value),
        },
    )]
}

/// `SingleEntryCacheTest(base_value, cache_value, cache_flags)`.
fn single_entry(base_value: i64, cache_value: i64, cache_flags: i16) -> Vec<CoinsOp> {
    let mut ops = vec![
        CoinsOp::Push,
        CoinsOp::BatchWrite {
            view: TIP,
            entries: entry_map(base_value, D),
            hash: [0; 32],
            erase: true,
        },
    ];
    ops.extend(entry_map(cache_value, cache_flags).into_iter().map(|(outpoint, entry)| {
        CoinsOp::Seed {
            view: 1,
            outpoint,
            entry,
        }
    }));
    ops
}

/// Runs `ops` then reads the first child's entry: `FAIL` if an op threw a `logic_error`.
fn run_then_entry(mut ops: Vec<CoinsOp>) -> (i64, i16) {
    ops.push(CoinsOp::Entry {
        view: 1,
        outpoint: OUTPOINT,
    });
    let outcomes = oracle().coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).expect("the script runs");
    match outcomes.last() {
        Some(CoinsOutcome::Ok(CoinsOutput::Entry(entry))) => read_entry(entry.clone(), &values()),
        Some(CoinsOutcome::LogicError(_)) => (FAIL, NO_ENTRY),
        other => panic!("unexpected outcome {other:?}"),
    }
}

#[test]
fn ccoins_access() {
    // coins_tests.cpp:658-684: base, cache, result value; cache flags, result flags.
    let rows: &[(i64, i64, i64, i16, i16)] = &[
        (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
        (ABSENT, SPENT, SPENT, 0, 0),
        (ABSENT, SPENT, SPENT, F, F),
        (ABSENT, SPENT, SPENT, D, D),
        (ABSENT, SPENT, SPENT, DF, DF),
        (ABSENT, VALUE2, VALUE2, 0, 0),
        (ABSENT, VALUE2, VALUE2, F, F),
        (ABSENT, VALUE2, VALUE2, D, D),
        (ABSENT, VALUE2, VALUE2, DF, DF),
        (SPENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
        (SPENT, SPENT, SPENT, 0, 0),
        (SPENT, SPENT, SPENT, F, F),
        (SPENT, SPENT, SPENT, D, D),
        (SPENT, SPENT, SPENT, DF, DF),
        (SPENT, VALUE2, VALUE2, 0, 0),
        (SPENT, VALUE2, VALUE2, F, F),
        (SPENT, VALUE2, VALUE2, D, D),
        (SPENT, VALUE2, VALUE2, DF, DF),
        (VALUE1, ABSENT, VALUE1, NO_ENTRY, 0),
        (VALUE1, SPENT, SPENT, 0, 0),
        (VALUE1, SPENT, SPENT, F, F),
        (VALUE1, SPENT, SPENT, D, D),
        (VALUE1, SPENT, SPENT, DF, DF),
        (VALUE1, VALUE2, VALUE2, 0, 0),
        (VALUE1, VALUE2, VALUE2, F, F),
        (VALUE1, VALUE2, VALUE2, D, D),
        (VALUE1, VALUE2, VALUE2, DF, DF),
    ];
    for &(base, cache, value, cache_flags, flags) in rows {
        let mut ops = single_entry(base, cache, cache_flags);
        ops.push(CoinsOp::Access {
            view: 1,
            outpoint: OUTPOINT,
        });
        assert_eq!(run_then_entry(ops), (value, flags), "row {base} {cache} {cache_flags}");
    }
}

#[test]
fn ccoins_spend() {
    // coins_tests.cpp:709-735
    let rows: &[(i64, i64, i64, i16, i16)] = &[
        (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
        (ABSENT, SPENT, SPENT, 0, D),
        (ABSENT, SPENT, ABSENT, F, NO_ENTRY),
        (ABSENT, SPENT, SPENT, D, D),
        (ABSENT, SPENT, ABSENT, DF, NO_ENTRY),
        (ABSENT, VALUE2, SPENT, 0, D),
        (ABSENT, VALUE2, ABSENT, F, NO_ENTRY),
        (ABSENT, VALUE2, SPENT, D, D),
        (ABSENT, VALUE2, ABSENT, DF, NO_ENTRY),
        (SPENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
        (SPENT, SPENT, SPENT, 0, D),
        (SPENT, SPENT, ABSENT, F, NO_ENTRY),
        (SPENT, SPENT, SPENT, D, D),
        (SPENT, SPENT, ABSENT, DF, NO_ENTRY),
        (SPENT, VALUE2, SPENT, 0, D),
        (SPENT, VALUE2, ABSENT, F, NO_ENTRY),
        (SPENT, VALUE2, SPENT, D, D),
        (SPENT, VALUE2, ABSENT, DF, NO_ENTRY),
        (VALUE1, ABSENT, SPENT, NO_ENTRY, D),
        (VALUE1, SPENT, SPENT, 0, D),
        (VALUE1, SPENT, ABSENT, F, NO_ENTRY),
        (VALUE1, SPENT, SPENT, D, D),
        (VALUE1, SPENT, ABSENT, DF, NO_ENTRY),
        (VALUE1, VALUE2, SPENT, 0, D),
        (VALUE1, VALUE2, ABSENT, F, NO_ENTRY),
        (VALUE1, VALUE2, SPENT, D, D),
        (VALUE1, VALUE2, ABSENT, DF, NO_ENTRY),
    ];
    for &(base, cache, value, cache_flags, flags) in rows {
        let mut ops = single_entry(base, cache, cache_flags);
        ops.push(CoinsOp::Spend {
            view: 1,
            outpoint: OUTPOINT,
            moveout: false,
        });
        assert_eq!(run_then_entry(ops), (value, flags), "row {base} {cache} {cache_flags}");
    }
}

#[test]
fn ccoins_add() {
    // coins_tests.cpp:782-800: cache, write, result value; cache flags, result flags, coinbase
    // (which is also possible_overwrite, CheckAddCoinBase :740-760), each for every base value
    // (CheckAddCoin :765-770).
    let rows: &[(i64, i64, i64, i16, i16, bool)] = &[
        (ABSENT, VALUE3, VALUE3, NO_ENTRY, DF, false),
        (ABSENT, VALUE3, VALUE3, NO_ENTRY, D, true),
        (SPENT, VALUE3, VALUE3, 0, DF, false),
        (SPENT, VALUE3, VALUE3, 0, D, true),
        (SPENT, VALUE3, VALUE3, F, DF, false),
        (SPENT, VALUE3, VALUE3, F, DF, true),
        (SPENT, VALUE3, VALUE3, D, D, false),
        (SPENT, VALUE3, VALUE3, D, D, true),
        (SPENT, VALUE3, VALUE3, DF, DF, false),
        (SPENT, VALUE3, VALUE3, DF, DF, true),
        (VALUE2, VALUE3, FAIL, 0, NO_ENTRY, false),
        (VALUE2, VALUE3, VALUE3, 0, D, true),
        (VALUE2, VALUE3, FAIL, F, NO_ENTRY, false),
        (VALUE2, VALUE3, VALUE3, F, DF, true),
        (VALUE2, VALUE3, FAIL, D, NO_ENTRY, false),
        (VALUE2, VALUE3, VALUE3, D, D, true),
        (VALUE2, VALUE3, FAIL, DF, NO_ENTRY, false),
        (VALUE2, VALUE3, VALUE3, DF, DF, true),
    ];
    for &(cache, write, value, cache_flags, flags, coinbase) in rows {
        for base in [ABSENT, SPENT, VALUE1] {
            let mut ops = single_entry(base, cache, cache_flags);
            ops.push(CoinsOp::Add {
                view: 1,
                outpoint: OUTPOINT,
                coin: coin(write, 1, coinbase),
                possible_overwrite: coinbase,
            });
            assert_eq!(
                run_then_entry(ops),
                (value, flags),
                "row {base} {cache} {cache_flags} {coinbase}"
            );
        }
    }
}

/// `CheckWriteCoins(parent_value, child_value, ..., parent_flags, child_flags, ...)`: a one-entry
/// child map written into the cache.
fn write_coins(parent: i64, child: i64, parent_flags: i16, child_flags: i16) -> (i64, i16) {
    let mut ops = single_entry(ABSENT, parent, parent_flags);
    ops.push(CoinsOp::BatchWrite {
        view: 1,
        entries: entry_map(child, child_flags),
        hash: [0; 32],
        erase: true,
    });
    run_then_entry(ops)
}

#[test]
fn ccoins_write() {
    // coins_tests.cpp:831-876: parent, child, result value; parent, child, result flags.
    let rows: &[(i64, i64, i64, i16, i16, i16)] = &[
        (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY, NO_ENTRY),
        (ABSENT, SPENT, SPENT, NO_ENTRY, D, D),
        (ABSENT, SPENT, ABSENT, NO_ENTRY, DF, NO_ENTRY),
        (ABSENT, VALUE2, VALUE2, NO_ENTRY, D, D),
        (ABSENT, VALUE2, VALUE2, NO_ENTRY, DF, DF),
        (SPENT, ABSENT, SPENT, 0, NO_ENTRY, 0),
        (SPENT, ABSENT, SPENT, F, NO_ENTRY, F),
        (SPENT, ABSENT, SPENT, D, NO_ENTRY, D),
        (SPENT, ABSENT, SPENT, DF, NO_ENTRY, DF),
        (SPENT, SPENT, SPENT, 0, D, D),
        (SPENT, SPENT, SPENT, 0, DF, D),
        (SPENT, SPENT, ABSENT, F, D, NO_ENTRY),
        (SPENT, SPENT, ABSENT, F, DF, NO_ENTRY),
        (SPENT, SPENT, SPENT, D, D, D),
        (SPENT, SPENT, SPENT, D, DF, D),
        (SPENT, SPENT, ABSENT, DF, D, NO_ENTRY),
        (SPENT, SPENT, ABSENT, DF, DF, NO_ENTRY),
        (SPENT, VALUE2, VALUE2, 0, D, D),
        (SPENT, VALUE2, VALUE2, 0, DF, D),
        (SPENT, VALUE2, VALUE2, F, D, DF),
        (SPENT, VALUE2, VALUE2, F, DF, DF),
        (SPENT, VALUE2, VALUE2, D, D, D),
        (SPENT, VALUE2, VALUE2, D, DF, D),
        (SPENT, VALUE2, VALUE2, DF, D, DF),
        (SPENT, VALUE2, VALUE2, DF, DF, DF),
        (VALUE1, ABSENT, VALUE1, 0, NO_ENTRY, 0),
        (VALUE1, ABSENT, VALUE1, F, NO_ENTRY, F),
        (VALUE1, ABSENT, VALUE1, D, NO_ENTRY, D),
        (VALUE1, ABSENT, VALUE1, DF, NO_ENTRY, DF),
        (VALUE1, SPENT, SPENT, 0, D, D),
        (VALUE1, SPENT, FAIL, 0, DF, NO_ENTRY),
        (VALUE1, SPENT, ABSENT, F, D, NO_ENTRY),
        (VALUE1, SPENT, FAIL, F, DF, NO_ENTRY),
        (VALUE1, SPENT, SPENT, D, D, D),
        (VALUE1, SPENT, FAIL, D, DF, NO_ENTRY),
        (VALUE1, SPENT, ABSENT, DF, D, NO_ENTRY),
        (VALUE1, SPENT, FAIL, DF, DF, NO_ENTRY),
        (VALUE1, VALUE2, VALUE2, 0, D, D),
        (VALUE1, VALUE2, FAIL, 0, DF, NO_ENTRY),
        (VALUE1, VALUE2, VALUE2, F, D, DF),
        (VALUE1, VALUE2, FAIL, F, DF, NO_ENTRY),
        (VALUE1, VALUE2, VALUE2, D, D, D),
        (VALUE1, VALUE2, FAIL, D, DF, NO_ENTRY),
        (VALUE1, VALUE2, VALUE2, DF, D, DF),
        (VALUE1, VALUE2, FAIL, DF, DF, NO_ENTRY),
    ];
    for &(parent, child, value, parent_flags, child_flags, flags) in rows {
        assert_eq!(
            write_coins(parent, child, parent_flags, child_flags),
            (value, flags),
            "row {parent} {child} {parent_flags} {child_flags}"
        );
    }
    // :871-876: a child entry that is not DIRTY leaves the parent unchanged.
    for parent in [ABSENT, SPENT, VALUE1] {
        for child in [ABSENT, SPENT, VALUE2] {
            let parent_flags: &[i16] = if parent == ABSENT {
                &[NO_ENTRY]
            } else {
                &[0, F, D, DF]
            };
            let child_flags: &[i16] = if child == ABSENT {
                &[NO_ENTRY]
            } else {
                &[0, F]
            };
            for &pf in parent_flags {
                for &cf in child_flags {
                    assert_eq!(
                        write_coins(parent, child, pf, cf),
                        (parent, pf),
                        "{parent} {child} {pf} {cf}"
                    );
                }
            }
        }
    }
}

fn op(n: u8) -> OutPoint {
    OutPoint {
        txid: [n; 32],
        vout: u32::from(n),
    }
}

fn ok(outcome: &CoinsOutcome) -> &CoinsOutput {
    match outcome {
        CoinsOutcome::Ok(output) => output,
        other => panic!("the op failed: {other:?}"),
    }
}

/// The two `logic_error`s, with Core's texts, each ending the script.
#[test]
fn logic_errors_end_the_script() {
    let o = oracle();
    // coins.cpp:84: an unspent coin added again without possible_overwrite.
    let add = |possible_overwrite| CoinsOp::Add {
        view: TIP,
        outpoint: op(1),
        coin: coin(VALUE1, 5, false),
        possible_overwrite,
    };
    let ops = [add(false), add(false), CoinsOp::DbState];
    let outcomes = o.coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    assert_eq!(
        outcomes,
        [
            CoinsOutcome::Ok(CoinsOutput::None),
            CoinsOutcome::LogicError(
                "Attempted to overwrite an unspent coin (when possible_overwrite is false)".into()
            ),
        ]
    );
    let outcomes = o.coins_ops(&[add(false), add(true)], DEFAULT_DB_BATCH_SIZE).unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(!outcomes[1].ends_script());

    // coins.cpp:221: a FRESH child entry over a coin unspent in the parent. The child knows only
    // its own entries, so its add succeeds and its flush throws.
    let ops = [
        add(false),
        CoinsOp::Push,
        CoinsOp::Add {
            view: 1,
            outpoint: op(1),
            coin: coin(VALUE2, 6, false),
            possible_overwrite: false,
        },
        CoinsOp::Flush {
            view: 1,
        },
        CoinsOp::DbState,
    ];
    let outcomes = o.coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    assert_eq!(outcomes.len(), 4);
    assert_eq!(
        outcomes[3],
        CoinsOutcome::LogicError(
            "FRESH flag misapplied to coin that exists in parent cache".into()
        )
    );
}

/// A tip flush before the tip's best block is set or read would assert in CCoinsViewDB::BatchWrite;
/// the shim reports it without running it.
#[test]
fn null_best_block_flush_is_an_assert_outcome() {
    for flush in [
        CoinsOp::Flush {
            view: TIP,
        },
        CoinsOp::Sync {
            view: TIP,
        },
    ] {
        let outcomes =
            oracle().coins_ops(&[flush, CoinsOp::DbState], DEFAULT_DB_BATCH_SIZE).unwrap();
        assert_eq!(
            outcomes,
            [CoinsOutcome::Assert(
                "txdb.cpp CCoinsViewDB::BatchWrite: assert(!hashBlock.IsNull())".into()
            )]
        );
    }
    // A child's flush into the tip reads no database: no assertion, and the tip's best block
    // stays null (BatchWrite copies the child's).
    let outcomes = oracle()
        .coins_ops(
            &[
                CoinsOp::Push,
                CoinsOp::Flush {
                    view: 1,
                },
                CoinsOp::GetBestBlock {
                    view: TIP,
                },
            ],
            DEFAULT_DB_BATCH_SIZE,
        )
        .unwrap();
    assert_eq!(ok(&outcomes[2]), &CoinsOutput::Hash([0; 32]));
}

/// A block through a child view, a tip flush, and the database it leaves: the coin records, the
/// best block, no head-blocks marker; the same at every -dbbatchsize (partial batches).
#[test]
fn tip_flush_writes_the_database() {
    let block = [7u8; 32];
    let ops = vec![
        CoinsOp::Add {
            view: TIP,
            outpoint: op(1),
            coin: coin(VALUE1, 3, true),
            possible_overwrite: true,
        },
        CoinsOp::Add {
            view: TIP,
            outpoint: op(2),
            coin: coin(VALUE2, 3, false),
            possible_overwrite: false,
        },
        CoinsOp::SetBestBlock {
            view: TIP,
            hash: [6; 32],
        },
        CoinsOp::Flush {
            view: TIP,
        },
        CoinsOp::Push,
        CoinsOp::Spend {
            view: 1,
            outpoint: op(2),
            moveout: true,
        },
        CoinsOp::Add {
            view: 1,
            outpoint: op(3),
            coin: coin(VALUE3, 4, false),
            possible_overwrite: false,
        },
        CoinsOp::SetBestBlock {
            view: 1,
            hash: block,
        },
        CoinsOp::Flush {
            view: 1,
        },
        CoinsOp::Pop,
        CoinsOp::Flush {
            view: TIP,
        },
        CoinsOp::Usage {
            view: TIP,
        },
    ];
    let mut states = Vec::new();
    for batch in [DEFAULT_DB_BATCH_SIZE, 0, 1, 40] {
        let outcomes = oracle().coins_ops(&ops, batch).unwrap();
        assert_eq!(outcomes.len(), ops.len());
        assert_eq!(
            ok(&outcomes[5]),
            &CoinsOutput::Spend {
                spent: true,
                moved: Some(Some(coin(VALUE2, 3, false))),
            }
        );
        let CoinsOutput::Flushed {
            ok: true,
            db: Some(db),
        } = ok(&outcomes[10])
        else {
            panic!("no database state");
        };
        assert_eq!(db.best_block, block);
        assert!(db.head_blocks.is_empty());
        assert_eq!(db.coins(), [(op(1), coin(VALUE1, 3, true)), (op(3), coin(VALUE3, 4, false))]);
        // Flush empties the tip: no coins, and the pool's chunks freed (ReallocateCache).
        assert!(matches!(
            ok(&outcomes[11]),
            CoinsOutput::Usage {
                cache_size: 0,
                ..
            }
        ));
        states.push(db.clone());
    }
    assert!(states.windows(2).all(|w| w[0] == w[1]), "-dbbatchsize changed the database");
}

/// `Sync` writes like `Flush` but keeps the unspent coins, clean, and erases the spent ones
/// (coins.cpp:267-282).
#[test]
fn sync_keeps_unspent_coins_clean() {
    let ops = [
        CoinsOp::Add {
            view: TIP,
            outpoint: op(1),
            coin: coin(VALUE1, 3, false),
            possible_overwrite: false,
        },
        CoinsOp::Add {
            view: TIP,
            outpoint: op(2),
            coin: coin(VALUE2, 3, false),
            possible_overwrite: true,
        },
        CoinsOp::Spend {
            view: TIP,
            outpoint: op(2),
            moveout: false,
        },
        CoinsOp::SetBestBlock {
            view: TIP,
            hash: [9; 32],
        },
        CoinsOp::Sync {
            view: TIP,
        },
        CoinsOp::Entry {
            view: TIP,
            outpoint: op(1),
        },
        CoinsOp::Entry {
            view: TIP,
            outpoint: op(2),
        },
        CoinsOp::HaveInCache {
            view: TIP,
            outpoint: op(1),
        },
    ];
    let outcomes = oracle().coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    let CoinsOutput::Flushed {
        db: Some(db),
        ..
    } = ok(&outcomes[4])
    else {
        panic!("no database state");
    };
    // op(2) was DIRTY, not FRESH (possible_overwrite), so its spend reached the database as an
    // erase of a key it never held.
    assert_eq!(db.coins(), [(op(1), coin(VALUE1, 3, false))]);
    assert_eq!(
        ok(&outcomes[5]),
        &CoinsOutput::Entry(Some(CacheEntry {
            flags: 0,
            coin: Some(coin(VALUE1, 3, false)),
        }))
    );
    assert_eq!(ok(&outcomes[6]), &CoinsOutput::Entry(None));
    assert_eq!(ok(&outcomes[7]), &CoinsOutput::Bool(true));
}

/// Reads through a child cache the coin in the child and in the tip (both `FetchCoin`s), and
/// `HaveCoinInCache` reads only the view's own map.
#[test]
fn child_reads_cache_in_every_view() {
    let ops = [
        CoinsOp::Add {
            view: TIP,
            outpoint: op(1),
            coin: coin(VALUE1, 3, false),
            possible_overwrite: false,
        },
        CoinsOp::SetBestBlock {
            view: TIP,
            hash: [1; 32],
        },
        CoinsOp::Flush {
            view: TIP,
        },
        CoinsOp::Push,
        CoinsOp::HaveInCache {
            view: 1,
            outpoint: op(1),
        },
        CoinsOp::Have {
            view: 1,
            outpoint: op(1),
        },
        CoinsOp::HaveInCache {
            view: 1,
            outpoint: op(1),
        },
        CoinsOp::HaveInCache {
            view: TIP,
            outpoint: op(1),
        },
        CoinsOp::Uncache {
            view: TIP,
            outpoint: op(1),
        },
        CoinsOp::HaveInCache {
            view: TIP,
            outpoint: op(1),
        },
    ];
    let outcomes = oracle().coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    let bools: Vec<_> = [4, 5, 6, 7, 9]
        .into_iter()
        .map(|i| match ok(&outcomes[i]) {
            CoinsOutput::Bool(b) => *b,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(bools, [false, true, true, true, false]);
}

/// The same script gives the same bytes, DynamicMemoryUsage included: nothing outlives a call.
#[test]
fn calls_are_deterministic_and_independent() {
    let mut ops: Vec<_> = (1..=40)
        .map(|n| CoinsOp::Add {
            view: TIP,
            outpoint: op(n),
            coin: coin(i64::from(n) * 1000, u32::from(n), n % 2 == 0),
            possible_overwrite: false,
        })
        .collect();
    ops.push(CoinsOp::Usage {
        view: TIP,
    });
    let script = dash_oracle_sys::coins_ops::encode_script(&ops);
    let o = oracle();
    let first = o.coins_ops_raw(&script, DEFAULT_DB_BATCH_SIZE).unwrap();
    // A different script in between, which leaves coins in its database and a best block.
    o.coins_ops(
        &[
            CoinsOp::Add {
                view: TIP,
                outpoint: op(1),
                coin: coin(5, 1, false),
                possible_overwrite: false,
            },
            CoinsOp::SetBestBlock {
                view: TIP,
                hash: [2; 32],
            },
            CoinsOp::Flush {
                view: TIP,
            },
        ],
        DEFAULT_DB_BATCH_SIZE,
    )
    .unwrap();
    assert_eq!(o.coins_ops_raw(&script, DEFAULT_DB_BATCH_SIZE).unwrap(), first);
    let outcomes = o.coins_ops(&ops, DEFAULT_DB_BATCH_SIZE).unwrap();
    let CoinsOutput::Usage {
        dynamic_usage,
        cache_size,
    } = ok(&outcomes[40])
    else {
        panic!("no usage");
    };
    assert_eq!(*cache_size, 40);
    assert!(*dynamic_usage > 0);
    // A fresh script's database has no best block.
    let outcomes = o.coins_ops(&[CoinsOp::DbState], DEFAULT_DB_BATCH_SIZE).unwrap();
    assert_eq!(ok(&outcomes[0]), &CoinsOutput::Db(Default::default()));
}

/// Malformed scripts fail the whole call.
#[test]
fn malformed_scripts_are_refused() {
    use dash_oracle_sys::ErrorKind;
    let o = oracle();
    let kind = |script: &[u8]| o.coins_ops_raw(script, DEFAULT_DB_BATCH_SIZE).unwrap_err().kind;
    assert_eq!(kind(&[17]), ErrorKind::Argument, "unknown op");
    assert_eq!(kind(&[1]), ErrorKind::Argument, "POP of the tip");
    assert_eq!(kind(&[13, 1]), ErrorKind::Argument, "a view that is not open");
    assert!(o.coins_ops_raw(&[0; 15], DEFAULT_DB_BATCH_SIZE).is_ok(), "15 children");
    assert_eq!(kind(&[0; 16]), ErrorKind::Argument, "a 17th view");
    assert_eq!(kind(&[4, 0, 1]), ErrorKind::Decode, "a short outpoint");
    let mut coin_with_tail = coin(1, 1, false);
    coin_with_tail.push(0);
    let script = dash_oracle_sys::coins_ops::encode_script(&[CoinsOp::Add {
        view: TIP,
        outpoint: op(1),
        coin: coin_with_tail,
        possible_overwrite: false,
    }]);
    assert_eq!(kind(&script), ErrorKind::Trailing);
    let seed = |flags| CoinsOp::Seed {
        view: TIP,
        outpoint: op(1),
        entry: CacheEntry {
            flags,
            coin: None,
        },
    };
    assert_eq!(kind(&dash_oracle_sys::coins_ops::encode_script(&[seed(4)])), ErrorKind::Argument);
    assert_eq!(
        kind(&dash_oracle_sys::coins_ops::encode_script(&[seed(0), seed(0)])),
        ErrorKind::Argument
    );
    assert_eq!(
        o.coins_ops_raw(&[], -1).unwrap_err().kind,
        ErrorKind::Argument,
        "a negative -dbbatchsize"
    );
}

/// No caller can reach Core's crash simulation (`-dbcrashratio`, which `_Exit(0)`s mid-batch): a
/// chain's arguments are parsed against the chain-parameter options only, so the argument is
/// refused, and the shim forces it to 0 over whatever gArgs holds. One-byte batches (every coin a
/// partial batch, where the crash point sits) run to the end.
#[test]
fn crash_ratio_is_unreachable() {
    use dash_oracle_sys::ErrorKind;
    let o = oracle();
    let err = o.chainparams_json(&Chain::new("regtest", &["-dbcrashratio=1"])).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Network);
    let ops: Vec<_> = (1..=4)
        .map(|n| CoinsOp::Add {
            view: TIP,
            outpoint: op(n),
            coin: coin(1000, 1, false),
            possible_overwrite: false,
        })
        .chain([
            CoinsOp::SetBestBlock {
                view: TIP,
                hash: [1; 32],
            },
            CoinsOp::Flush {
                view: TIP,
            },
        ])
        .collect();
    assert_eq!(o.coins_ops(&ops, 1).unwrap().len(), ops.len());
}
