//! Core's L0 tables (`coins_tests.cpp:556-866`, v24, verbatim): `ccoins_access`, `ccoins_spend`,
//! `ccoins_add` and `ccoins_write`, in CH2 §6.1's two mappings. Core's `base` is the database (a
//! VALUE1 base is a stored coin; SPENT and ABSENT are none) and Core's `cache` is the tip:
//!
//! - (a) the tip's entry in active, F empty: every row matches exactly.
//! - (b) the tip's entry in F (put in active, then frozen) under an empty active. FAIL rows match
//!   exactly. Where the starting state is one Core's caches can reach ([`reachable`]), so do the
//!   coin a read then sees and the database after a flush; the flags may differ (D2).
//!
//! `ccoins_write`'s child is a block overlay's child layer, merged by [`CoinsCache::merge`].

use super::*;
use crate::coins::CoinSrc;
use crate::coins::layer::SlotRef;
use crate::coins::packed::decode_trusted;
use crate::error::CoinsError;

const SPENT: i64 = -1;
const ABSENT: i64 = -2;
const FAIL: i64 = -3;
const VALUE1: i64 = 100;
const VALUE2: i64 = 200;
const VALUE3: i64 = 300;
const DIRTY: i8 = 1;
const FRESH: i8 = 2;
const NO_ENTRY: i8 = -1;
const FLAGS: [i8; 4] = [0, FRESH, DIRTY, DIRTY | FRESH];
const CLEAN_FLAGS: [i8; 2] = [0, FRESH];

const OUTPOINT: OutPointKey = OutPointKey([0x5a; 36]);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// The tip's entry in active.
    A,
    /// The tip's entry in F.
    B,
}

// Core: coins_tests.cpp:625-856, the four tables.
const ACCESS: [(i64, i64, i64, i8, i8); 27] = [
    (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
    (ABSENT, SPENT, SPENT, 0, 0),
    (ABSENT, SPENT, SPENT, FRESH, FRESH),
    (ABSENT, SPENT, SPENT, DIRTY, DIRTY),
    (ABSENT, SPENT, SPENT, DIRTY | FRESH, DIRTY | FRESH),
    (ABSENT, VALUE2, VALUE2, 0, 0),
    (ABSENT, VALUE2, VALUE2, FRESH, FRESH),
    (ABSENT, VALUE2, VALUE2, DIRTY, DIRTY),
    (ABSENT, VALUE2, VALUE2, DIRTY | FRESH, DIRTY | FRESH),
    (SPENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
    (SPENT, SPENT, SPENT, 0, 0),
    (SPENT, SPENT, SPENT, FRESH, FRESH),
    (SPENT, SPENT, SPENT, DIRTY, DIRTY),
    (SPENT, SPENT, SPENT, DIRTY | FRESH, DIRTY | FRESH),
    (SPENT, VALUE2, VALUE2, 0, 0),
    (SPENT, VALUE2, VALUE2, FRESH, FRESH),
    (SPENT, VALUE2, VALUE2, DIRTY, DIRTY),
    (SPENT, VALUE2, VALUE2, DIRTY | FRESH, DIRTY | FRESH),
    (VALUE1, ABSENT, VALUE1, NO_ENTRY, 0),
    (VALUE1, SPENT, SPENT, 0, 0),
    (VALUE1, SPENT, SPENT, FRESH, FRESH),
    (VALUE1, SPENT, SPENT, DIRTY, DIRTY),
    (VALUE1, SPENT, SPENT, DIRTY | FRESH, DIRTY | FRESH),
    (VALUE1, VALUE2, VALUE2, 0, 0),
    (VALUE1, VALUE2, VALUE2, FRESH, FRESH),
    (VALUE1, VALUE2, VALUE2, DIRTY, DIRTY),
    (VALUE1, VALUE2, VALUE2, DIRTY | FRESH, DIRTY | FRESH),
];

const SPEND: [(i64, i64, i64, i8, i8); 27] = [
    (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
    (ABSENT, SPENT, SPENT, 0, DIRTY),
    (ABSENT, SPENT, ABSENT, FRESH, NO_ENTRY),
    (ABSENT, SPENT, SPENT, DIRTY, DIRTY),
    (ABSENT, SPENT, ABSENT, DIRTY | FRESH, NO_ENTRY),
    (ABSENT, VALUE2, SPENT, 0, DIRTY),
    (ABSENT, VALUE2, ABSENT, FRESH, NO_ENTRY),
    (ABSENT, VALUE2, SPENT, DIRTY, DIRTY),
    (ABSENT, VALUE2, ABSENT, DIRTY | FRESH, NO_ENTRY),
    (SPENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY),
    (SPENT, SPENT, SPENT, 0, DIRTY),
    (SPENT, SPENT, ABSENT, FRESH, NO_ENTRY),
    (SPENT, SPENT, SPENT, DIRTY, DIRTY),
    (SPENT, SPENT, ABSENT, DIRTY | FRESH, NO_ENTRY),
    (SPENT, VALUE2, SPENT, 0, DIRTY),
    (SPENT, VALUE2, ABSENT, FRESH, NO_ENTRY),
    (SPENT, VALUE2, SPENT, DIRTY, DIRTY),
    (SPENT, VALUE2, ABSENT, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, ABSENT, SPENT, NO_ENTRY, DIRTY),
    (VALUE1, SPENT, SPENT, 0, DIRTY),
    (VALUE1, SPENT, ABSENT, FRESH, NO_ENTRY),
    (VALUE1, SPENT, SPENT, DIRTY, DIRTY),
    (VALUE1, SPENT, ABSENT, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, VALUE2, SPENT, 0, DIRTY),
    (VALUE1, VALUE2, ABSENT, FRESH, NO_ENTRY),
    (VALUE1, VALUE2, SPENT, DIRTY, DIRTY),
    (VALUE1, VALUE2, ABSENT, DIRTY | FRESH, NO_ENTRY),
];

const ADD: [(i64, i64, i64, i8, i8, bool); 18] = [
    (ABSENT, VALUE3, VALUE3, NO_ENTRY, DIRTY | FRESH, false),
    (ABSENT, VALUE3, VALUE3, NO_ENTRY, DIRTY, true),
    (SPENT, VALUE3, VALUE3, 0, DIRTY | FRESH, false),
    (SPENT, VALUE3, VALUE3, 0, DIRTY, true),
    (SPENT, VALUE3, VALUE3, FRESH, DIRTY | FRESH, false),
    (SPENT, VALUE3, VALUE3, FRESH, DIRTY | FRESH, true),
    (SPENT, VALUE3, VALUE3, DIRTY, DIRTY, false),
    (SPENT, VALUE3, VALUE3, DIRTY, DIRTY, true),
    (SPENT, VALUE3, VALUE3, DIRTY | FRESH, DIRTY | FRESH, false),
    (SPENT, VALUE3, VALUE3, DIRTY | FRESH, DIRTY | FRESH, true),
    (VALUE2, VALUE3, FAIL, 0, NO_ENTRY, false),
    (VALUE2, VALUE3, VALUE3, 0, DIRTY, true),
    (VALUE2, VALUE3, FAIL, FRESH, NO_ENTRY, false),
    (VALUE2, VALUE3, VALUE3, FRESH, DIRTY | FRESH, true),
    (VALUE2, VALUE3, FAIL, DIRTY, NO_ENTRY, false),
    (VALUE2, VALUE3, VALUE3, DIRTY, DIRTY, true),
    (VALUE2, VALUE3, FAIL, DIRTY | FRESH, NO_ENTRY, false),
    (VALUE2, VALUE3, VALUE3, DIRTY | FRESH, DIRTY | FRESH, true),
];

const WRITE: [(i64, i64, i64, i8, i8, i8); 45] = [
    (ABSENT, ABSENT, ABSENT, NO_ENTRY, NO_ENTRY, NO_ENTRY),
    (ABSENT, SPENT, SPENT, NO_ENTRY, DIRTY, DIRTY),
    (ABSENT, SPENT, ABSENT, NO_ENTRY, DIRTY | FRESH, NO_ENTRY),
    (ABSENT, VALUE2, VALUE2, NO_ENTRY, DIRTY, DIRTY),
    (ABSENT, VALUE2, VALUE2, NO_ENTRY, DIRTY | FRESH, DIRTY | FRESH),
    (SPENT, ABSENT, SPENT, 0, NO_ENTRY, 0),
    (SPENT, ABSENT, SPENT, FRESH, NO_ENTRY, FRESH),
    (SPENT, ABSENT, SPENT, DIRTY, NO_ENTRY, DIRTY),
    (SPENT, ABSENT, SPENT, DIRTY | FRESH, NO_ENTRY, DIRTY | FRESH),
    (SPENT, SPENT, SPENT, 0, DIRTY, DIRTY),
    (SPENT, SPENT, SPENT, 0, DIRTY | FRESH, DIRTY),
    (SPENT, SPENT, ABSENT, FRESH, DIRTY, NO_ENTRY),
    (SPENT, SPENT, ABSENT, FRESH, DIRTY | FRESH, NO_ENTRY),
    (SPENT, SPENT, SPENT, DIRTY, DIRTY, DIRTY),
    (SPENT, SPENT, SPENT, DIRTY, DIRTY | FRESH, DIRTY),
    (SPENT, SPENT, ABSENT, DIRTY | FRESH, DIRTY, NO_ENTRY),
    (SPENT, SPENT, ABSENT, DIRTY | FRESH, DIRTY | FRESH, NO_ENTRY),
    (SPENT, VALUE2, VALUE2, 0, DIRTY, DIRTY),
    (SPENT, VALUE2, VALUE2, 0, DIRTY | FRESH, DIRTY),
    (SPENT, VALUE2, VALUE2, FRESH, DIRTY, DIRTY | FRESH),
    (SPENT, VALUE2, VALUE2, FRESH, DIRTY | FRESH, DIRTY | FRESH),
    (SPENT, VALUE2, VALUE2, DIRTY, DIRTY, DIRTY),
    (SPENT, VALUE2, VALUE2, DIRTY, DIRTY | FRESH, DIRTY),
    (SPENT, VALUE2, VALUE2, DIRTY | FRESH, DIRTY, DIRTY | FRESH),
    (SPENT, VALUE2, VALUE2, DIRTY | FRESH, DIRTY | FRESH, DIRTY | FRESH),
    (VALUE1, ABSENT, VALUE1, 0, NO_ENTRY, 0),
    (VALUE1, ABSENT, VALUE1, FRESH, NO_ENTRY, FRESH),
    (VALUE1, ABSENT, VALUE1, DIRTY, NO_ENTRY, DIRTY),
    (VALUE1, ABSENT, VALUE1, DIRTY | FRESH, NO_ENTRY, DIRTY | FRESH),
    (VALUE1, SPENT, SPENT, 0, DIRTY, DIRTY),
    (VALUE1, SPENT, FAIL, 0, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, SPENT, ABSENT, FRESH, DIRTY, NO_ENTRY),
    (VALUE1, SPENT, FAIL, FRESH, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, SPENT, SPENT, DIRTY, DIRTY, DIRTY),
    (VALUE1, SPENT, FAIL, DIRTY, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, SPENT, ABSENT, DIRTY | FRESH, DIRTY, NO_ENTRY),
    (VALUE1, SPENT, FAIL, DIRTY | FRESH, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, VALUE2, VALUE2, 0, DIRTY, DIRTY),
    (VALUE1, VALUE2, FAIL, 0, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, VALUE2, VALUE2, FRESH, DIRTY, DIRTY | FRESH),
    (VALUE1, VALUE2, FAIL, FRESH, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, VALUE2, VALUE2, DIRTY, DIRTY, DIRTY),
    (VALUE1, VALUE2, FAIL, DIRTY, DIRTY | FRESH, NO_ENTRY),
    (VALUE1, VALUE2, VALUE2, DIRTY | FRESH, DIRTY, DIRTY | FRESH),
    (VALUE1, VALUE2, FAIL, DIRTY | FRESH, DIRTY | FRESH, NO_ENTRY),
];

fn flags_of(flags: i8) -> Flags {
    let mut f = Flags::NONE;
    if flags & DIRTY != 0 {
        f = f | Flags::DIRTY;
    }
    if flags & FRESH != 0 {
        f = f | Flags::FRESH;
    }
    f
}

/// Core's `GetCoinsMapEntry`: an entry as (value, flags).
fn entry_of(e: Option<SlotRef<'_>>) -> (i64, i8) {
    let Some(e) = e else {
        return (ABSENT, NO_ENTRY);
    };
    let value = if e.flags.is_spent() {
        SPENT
    } else {
        decode_trusted(e.coin).value()
    };
    let flags = i8::from(e.flags.is_dirty()) | i8::from(e.flags.contains(Flags::FRESH)) << 1;
    (value, flags)
}

/// An entry's bytes and flags: a spent one is a tombstone.
fn entry_bytes(value: i64, flags: i8) -> (Flags, Vec<u8>) {
    if value == SPENT {
        (flags_of(flags) | Flags::SPENT, Vec::new())
    } else {
        (flags_of(flags), coin(value).as_bytes().to_vec())
    }
}

/// Whether Core's caches can reach the state over `base`: one of `SanityCheck`'s five
/// combinations, and no FRESH entry over a stored coin.
fn reachable(base: i64, value: i64, flags: i8) -> bool {
    if value == ABSENT || value == FAIL {
        return true;
    }
    let attr = i8::from(flags & DIRTY != 0)
        | i8::from(flags & FRESH != 0) << 1
        | i8::from(value == SPENT) << 2;
    !matches!(attr, 2 | 4 | 7) && !(flags & FRESH != 0 && base == VALUE1)
}

/// What Core's caches give for an entry over `base`: the coin `GetCoin` sees, and the base after
/// the cache's flush.
fn core_outcome(base: i64, value: i64, flags: i8) -> (i64, i64) {
    let base = if base == VALUE1 {
        VALUE1
    } else {
        ABSENT
    };
    let unspent = |v: i64| {
        if v == SPENT {
            ABSENT
        } else {
            v
        }
    };
    let visible = if value == ABSENT {
        base
    } else {
        unspent(value)
    };
    let flushed = if value != ABSENT && flags & DIRTY != 0 {
        unspent(value)
    } else {
        base
    };
    (visible, flushed)
}

/// Core's `SingleEntryCacheTest`: `base` in the database, the tip's entry in active or F.
struct Single {
    w: World,
    mode: Mode,
    base: i64,
    start: (i64, i8),
    /// Whether the start state passes our `sanity_check`.
    sane: bool,
}

impl Single {
    fn new(mode: Mode, base: i64, value: i64, flags: i8) -> Self {
        let mut w = World::with_dbcache(16);
        if base == VALUE1 {
            w.db_set(&OUTPOINT, Some(&coin(VALUE1)));
        }
        if value != ABSENT {
            let (f, bytes) = entry_bytes(value, flags);
            w.cache.put_active(&OUTPOINT, f, &bytes);
        }
        if mode == Mode::B {
            drop(w.cache.freeze_at(1, 0));
        }
        let sane = w.cache.sanity_check().is_ok();
        Self {
            w,
            mode,
            base,
            start: (value, flags),
            sane,
        }
    }

    /// The database after a flush: F's DIRTY entry, then active's, over the stored coin.
    fn flushed(&self) -> i64 {
        let mut v = self.w.db_get(&OUTPOINT).map_or(ABSENT, |c| c.as_ref().value());
        let f = self.w.cache.frozen().map(|f| f.coins());
        for layer in f.into_iter().chain([self.w.cache.active()]) {
            if let Some(e) = layer.get(&OUTPOINT).filter(|e| e.flags.is_dirty()) {
                v = if e.flags.is_spent() {
                    ABSENT
                } else {
                    decode_trusted(e.coin).value()
                };
            }
        }
        v
    }

    /// Checks the row's outcome: `failed` is whether the op threw Core's `logic_error`.
    fn check(mut self, row: impl std::fmt::Debug, failed: bool, want: (i64, i8)) -> Option<String> {
        let got = if failed {
            (FAIL, NO_ENTRY)
        } else {
            entry_of(self.w.cache.cached(&OUTPOINT))
        };
        let mismatch = |what: &str, got: &dyn std::fmt::Debug, want: &dyn std::fmt::Debug| {
            Some(format!("{:?} {row:?}: {what} {got:?}, Core {want:?}", self.mode))
        };
        match self.mode {
            Mode::A if got != want => return mismatch("entry", &got, &want),
            Mode::B if (got.0 == FAIL) != (want.0 == FAIL) => {
                return mismatch("entry", &got, &want);
            }
            _ => {}
        }
        if failed {
            return None;
        }
        if self.sane
            && let Err(e) = self.w.cache.sanity_check()
        {
            return mismatch("sanity", &e, &"Ok");
        }
        if reachable(self.base, self.start.0, self.start.1) && reachable(self.base, want.0, want.1)
        {
            let (visible, flushed) = core_outcome(self.base, want.0, want.1);
            let got_flushed = self.flushed();
            if got_flushed != flushed {
                return mismatch("flushed", &got_flushed, &flushed);
            }
            let got_visible = self.w.cache.fetch(&OUTPOINT).unwrap().map_or(ABSENT, |c| c.value());
            if got_visible != visible {
                return mismatch("visible", &got_visible, &visible);
            }
        }
        None
    }
}

// Core: coins_tests.cpp:625-673 CheckAccessCoin, ccoins_access
fn access(mode: Mode) -> Vec<String> {
    let mut bad = Vec::new();
    for row @ (base, value, want_value, flags, want_flags) in ACCESS {
        let mut s = Single::new(mode, base, value, flags);
        s.w.cache.fetch(&OUTPOINT).unwrap();
        bad.extend(s.check(row, false, (want_value, want_flags)));
    }
    bad
}

// Core: coins_tests.cpp:675-722 CheckSpendCoins, ccoins_spend
fn spend(mode: Mode) -> Vec<String> {
    let mut bad = Vec::new();
    for row @ (base, value, want_value, flags, want_flags) in SPEND {
        let mut s = Single::new(mode, base, value, flags);
        s.w.cache.spend(&OUTPOINT).unwrap();
        bad.extend(s.check(row, false, (want_value, want_flags)));
    }
    bad
}

// Core: coins_tests.cpp:724-798 CheckAddCoinBase, CheckAddCoin, ccoins_add
fn add(mode: Mode, #[allow(unused)] plant: bool) -> Vec<String> {
    let mut bad = Vec::new();
    for row @ (value, modify, want_value, flags, want_flags, coinbase) in ADD {
        for base in [ABSENT, SPENT, VALUE1] {
            let mut s = Single::new(mode, base, value, flags);
            #[cfg(feature = "planted-bugs")]
            if plant {
                s.w.cache.plant(crate::coins::Planted::X9);
            }
            let out = output(modify);
            let src = CoinSrc::Output {
                out: &out,
                height: 1,
                coinbase,
            };
            let failed = match s.w.cache.add(&OUTPOINT, src, coinbase) {
                Ok(()) => false,
                Err(CoinsError::OverwriteUnspent) => true,
                Err(e) => panic!("{row:?}: {e}"),
            };
            bad.extend(s.check((base, row), failed, (want_value, want_flags)));
        }
    }
    bad
}

// Core: coins_tests.cpp:800-866 CheckWriteCoins, ccoins_write
fn write(mode: Mode) -> Vec<String> {
    let mut rows = WRITE.to_vec();
    // Core: coins_tests.cpp:851-856, the child entries that are not DIRTY leave the parent as it
    // was.
    for parent_value in [ABSENT, SPENT, VALUE1] {
        for child_value in [ABSENT, SPENT, VALUE2] {
            let parent_flags: &[i8] = if parent_value == ABSENT {
                &[NO_ENTRY]
            } else {
                &FLAGS
            };
            let child_flags: &[i8] = if child_value == ABSENT {
                &[NO_ENTRY]
            } else {
                &CLEAN_FLAGS
            };
            for &pf in parent_flags {
                for &cf in child_flags {
                    rows.push((parent_value, child_value, parent_value, pf, cf, pf));
                }
            }
        }
    }
    let mut bad = Vec::new();
    for row @ (parent, child, want_value, parent_flags, child_flags, want_flags) in rows {
        let mut s = Single::new(mode, ABSENT, parent, parent_flags);
        // A child map holding the one entry, as Core's `WriteCoinsViewEntry` builds it.
        let mut layer = ShardedLayer::new(0, Salt::fixed(7));
        if child != ABSENT {
            let (f, bytes) = entry_bytes(child, child_flags);
            layer.upsert(&OUTPOINT, f, &bytes);
        }
        let failed = match s.w.cache.merge(&mut layer, block(1)) {
            Ok(()) => false,
            Err(CoinsError::FreshMisapplied) => true,
            Err(e) => panic!("{row:?}: {e}"),
        };
        bad.extend(s.check(row, failed, (want_value, want_flags)));
    }
    bad
}

#[test]
fn l0_a_matches_core_exactly() {
    let bad: Vec<_> =
        [access(Mode::A), spend(Mode::A), add(Mode::A, false), write(Mode::A)].concat();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn l0_b_matches_core_on_failures_reads_and_flushes() {
    let bad: Vec<_> =
        [access(Mode::B), spend(Mode::B), add(Mode::B, false), write(Mode::B)].concat();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// X9 (`add` checks active only) passes (a) and is caught by (b)'s FAIL rows: an unspent coin in F
/// must still refuse an add without `possible_overwrite`.
#[cfg(feature = "planted-bugs")]
#[test]
fn l0_b_catches_x9() {
    assert!(add(Mode::A, true).is_empty());
    let bad = add(Mode::B, true);
    assert_eq!(bad.len(), 4 * 3, "{}", bad.join("\n"));
    assert!(
        bad.iter().all(|m| m.contains("entry (300, 3)") || m.contains("entry (300, 1)")),
        "{}",
        bad.join("\n")
    );
}

/// CH2 §6.1's cursor-order vector (GPT r1 m3): the `coins` column family iterates in Core's
/// serialized-key order, txid ‖ VARINT(vout), not numeric vout order. Of one txid's vouts 16383
/// (`fe7f`) and 16512 (`808000`), added through the tip and flushed, a cursor yields 16512 first.
#[test]
fn the_coins_cf_iterates_in_cores_serialized_key_order() {
    let mut w = World::with_dbcache(16);
    // OUTPOINT's txid, with the vout's little-endian bytes after it.
    let key = |vout: u32| {
        let mut k = OUTPOINT;
        k.0[32..].copy_from_slice(&vout.to_le_bytes());
        k
    };
    for vout in [16383, 16512] {
        let out = output(i64::from(vout));
        let src = CoinSrc::Output {
            out: &out,
            height: 1,
            coinbase: false,
        };
        w.cache.add(&key(vout), src, false).unwrap();
    }
    w.flush(1);
    let order: Vec<_> = db_coins(&w.db).iter().map(|(k, c)| (k.vout(), c.clone())).collect();
    assert_eq!(
        order,
        [(16512, coin(16512).as_bytes().to_vec()), (16383, coin(16383).as_bytes().to_vec())]
    );
}
