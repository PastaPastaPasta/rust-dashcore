//! A coins cache layer: a map from [`OutPointKey`] to a coin's bytes and its cache flags, in
//! 72-byte [`Slot`]s (CH2 §2.2, §2.3).
//!
//! The layout is [`ShardedLayer`]: 64 hashbrown shards of slots, each growing to its ceiling (the
//! presize, CH2c D29) and doubling one shard at a time past that. CH2a's layout gate kept it over
//! an arena of slots behind a `HashTable<u32>` index
//! (`docs/node-program/research/PF1-CH2a-layout-gate.md`).
//!
//! A layer grows on demand, so an insert always succeeds and a valid block always completes
//! (DEC-95). The budget is checked by the cache at Core's points, never by a layer.
//!
//! Coins longer than [`INLINE_MAX`] bytes live in `LongCoins` beside the slots, one box each; the
//! slot holds their place.
//!
//! Memory is counted from allocation sizes ([`CoinLayerOps::allocated_bytes`]), exactly what the
//! tagging allocator sees (CH2 §6.7): every allocation a layer makes is inside
//! [`crate::alloc_tag::coins`]'s scope.

mod long;
// The shard tables' page allocator: `mmap` and `munmap` behind hashbrown's `Allocator`.
#[allow(unsafe_code)]
mod pages;
mod sharded;

pub(crate) use sharded::{DirtyRef, LayerPos};
pub use sharded::{Rebuilds, ShardSizes, ShardedLayer};

use super::key::OutPointKey;
use super::packed::INLINE_MAX;
use long::LongCoins;

/// A cache entry's flags: Core's `CCoinsCacheEntry` flags (`coins.h:97-102, 117-127`, both pins)
/// plus `SPENT`, which marks a tombstone (a spent entry that carries no coin bytes).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Flags(u8);

impl Flags {
    pub const NONE: Self = Self(0);
    pub const DIRTY: Self = Self(1);
    pub const FRESH: Self = Self(2);
    pub const SPENT: Self = Self(4);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub const fn is_dirty(self) -> bool {
        self.contains(Self::DIRTY)
    }

    pub const fn is_spent(self) -> bool {
        self.contains(Self::SPENT)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl std::fmt::Debug for Flags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = [(Self::DIRTY, "DIRTY"), (Self::FRESH, "FRESH"), (Self::SPENT, "SPENT")];
        let set: Vec<_> =
            names.iter().filter(|(fl, _)| self.contains(*fl)).map(|(_, n)| *n).collect();
        if set.is_empty() {
            f.write_str("∅")
        } else {
            f.write_str(&set.join("|"))
        }
    }
}

/// The slot's `len` for a coin kept in `LongCoins`.
const LEN_LONG: u8 = 0xff;

/// One entry: `key (36) ‖ flags (1) ‖ len (1, 0xFF = long) ‖ coin (34)`, 72 bytes. A long coin's
/// `coin` field holds its `LongRef`.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
pub struct Slot {
    key: OutPointKey,
    flags: u8,
    len: u8,
    coin: [u8; INLINE_MAX],
}

pub const SLOT_BYTES: usize = 72;
const _: () = assert!(size_of::<Slot>() == SLOT_BYTES && align_of::<Slot>() == 8);

impl Slot {
    const EMPTY: Self = Self {
        key: OutPointKey([0; 36]),
        flags: 0,
        len: 0,
        coin: [0; INLINE_MAX],
    };

    fn flags(&self) -> Flags {
        Flags(self.flags)
    }

    /// The coin's bytes (empty for a tombstone), from the slot or `long`.
    fn coin<'a>(&'a self, long: &'a LongCoins) -> &'a [u8] {
        if self.len == LEN_LONG {
            long.get(long::LongRef::read(&self.coin))
        } else {
            &self.coin[..usize::from(self.len)]
        }
    }

    /// Writes `flags` and `coin` into a slot whose previous coin, if long, was already freed.
    fn set(&mut self, flags: Flags, coin: &[u8], long: &mut LongCoins) {
        self.flags = flags.0;
        if coin.len() <= INLINE_MAX {
            self.len = coin.len() as u8;
            self.coin[..coin.len()].copy_from_slice(coin);
        } else {
            self.len = LEN_LONG;
            long.put(coin).write(&mut self.coin);
        }
    }

    /// Writes `flags` and a long coin whose box moves in, into a slot whose previous coin, if long,
    /// was already freed.
    fn set_long(&mut self, flags: Flags, coin: Box<[u8]>, long: &mut LongCoins) {
        debug_assert!(coin.len() > INLINE_MAX);
        self.flags = flags.0;
        self.len = LEN_LONG;
        long.put_box(coin).write(&mut self.coin);
    }

    /// Frees the slot's long coin, if it has one.
    fn free_long(&self, long: &mut LongCoins) {
        if self.len == LEN_LONG {
            long.free(long::LongRef::read(&self.coin));
        }
    }

    fn coin_len(&self) -> usize {
        if self.len == LEN_LONG {
            long::LongRef::read(&self.coin).len()
        } else {
            usize::from(self.len)
        }
    }
}

/// A dirty entry's share of a flush batch: its key and coin.
pub(crate) const fn dirty_bytes_of(coin_len: usize) -> u64 {
    (36 + coin_len) as u64
}

/// An entry as a lookup returns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotRef<'a> {
    pub flags: Flags,
    /// Core's `Coin` bytes; empty for a tombstone ([`Flags::SPENT`]).
    pub coin: &'a [u8],
}

/// One coins cache layer.
pub trait CoinLayerOps {
    fn get(&self, k: &OutPointKey) -> Option<SlotRef<'_>>;

    /// Inserts or overwrites `k`'s entry, growing the layer if it must. `coin` is Core's `Coin`
    /// bytes, empty for a tombstone.
    fn upsert(&mut self, k: &OutPointKey, flags: Flags, coin: &[u8]);

    /// Removes `k`'s entry; whether it had one.
    fn remove(&mut self, k: &OutPointKey) -> bool;

    /// The DIRTY entries in the flush order.
    fn iter_dirty(&self) -> impl Iterator<Item = (OutPointKey, Flags, &[u8])> + '_ {
        self.iter().filter(|(_, e)| e.flags.is_dirty()).map(|(k, e)| (k, e.flags, e.coin))
    }

    /// Every entry, in the flush order.
    fn iter(&self) -> impl Iterator<Item = (OutPointKey, SlotRef<'_>)> + '_;

    /// The layer's live allocations in bytes, exactly as the allocator counts requested sizes.
    fn allocated_bytes(&self) -> u64;

    /// The number of entries.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The number of DIRTY entries.
    fn dirty_len(&self) -> usize;

    /// The DIRTY entries' keys and coin bytes: a flush's size before batch overhead.
    fn dirty_bytes(&self) -> u64;
}

#[cfg(test)]
mod tests;
