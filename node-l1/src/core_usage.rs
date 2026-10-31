//! Core's `CCoinsViewCache::DynamicMemoryUsage()` as arithmetic over the map's history (CH2c D4):
//! the DEC-43 proxy's Core-model arm flushes and holds memory by it (D9, D10), and node-l1's
//! `diff_coins_ops` checks it against `oracle_coins_ops`' USAGE op after every tip op.
//!
//! `DynamicMemoryUsage() = memusage::DynamicUsage(cacheCoins) + cachedCoinsUsage`, where the map's
//! usage is its pool resource's chunks plus its bucket array (`memusage.h:186-203`, v24):
//!
//! - chunks: `PoolResource` carves 104-byte nodes (`pair<const COutPoint, CCoinsCacheEntry>` and
//!   libstdc++'s next pointer, no cached hash) from 256 KiB chunks and reuses freed ones, so the
//!   chunks held are `ceil(peak live nodes / 2,520)` since the map was last reallocated, and at
//!   least the one `PoolResource`'s constructor allocates (`pool.h:177-181`), each
//!   counted as `MallocUsage(262,144) + MallocUsage(3 pointers)`;
//! - buckets: `MallocUsage(8 · bucket_count)`, libstdc++'s `_Prime_rehash_policy` at load factor 1:
//!   11 elements' worth on the first insert (13 buckets), then the next listed prime at or above
//!   twice the count whenever an insert passes it; erases never shrink it;
//! - `cachedCoinsUsage`: each unspent coin's script on the heap, `MallocUsage(size)` above
//!   prevector's 28 inline bytes (`Coin::DynamicMemoryUsage`).
//!
//! A flush ends with `ReallocateCache`: a new empty map (one bucket, the first chunk). The node and chunk
//! sizes were measured through the oracle (`tests/l1_coins_usage.rs`), and `diff_coins_ops`
//! checks this model op by op. The bake-off includes this file (`#[path]`) for its Core-model arm.

/// `MallocUsage` on a 64-bit glibc build (`memusage.h:51-63`).
pub const fn malloc_usage(alloc: u64) -> u64 {
    if alloc == 0 {
        0
    } else {
        ((alloc + 31) >> 4) << 4
    }
}

/// `PoolResource`'s chunk (`pool.h:187`).
pub const CHUNK_BYTES: u64 = 262_144;
/// A `CCoinsMap` node: the pair (96 bytes) and the bucket chain's next pointer.
pub const NODE_BYTES: u64 = 104;
/// Nodes a chunk holds; the remaining 64 bytes go to another size class's free list.
pub const NODES_PER_CHUNK: u64 = CHUNK_BYTES / NODE_BYTES;
/// One chunk's usage: its allocation and its `std::list` node of three pointers.
pub const CHUNK_USAGE: u64 = malloc_usage(CHUNK_BYTES) + malloc_usage(3 * 8);
/// `prevector<28, unsigned char>`: scripts up to this size are stored inline.
const SCRIPT_INLINE: u64 = 28;
/// `MAX_SCRIPT_SIZE`: a longer stored script decodes as a one-byte `OP_RETURN`.
const MAX_SCRIPT_SIZE: u64 = 10_000;

/// libstdc++'s `__prime_list` below 2³² (`hashtable-aux.cc`), read from the system's
/// `libstdc++.so.6` (`std::__detail::__prime_list`); `_M_next_bkt` searches it from its 7th entry.
const PRIMES: [u32; 256] = [
    2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    103, 109, 113, 127, 137, 139, 149, 157, 167, 179, 193, 199, 211, 227, 241, 257, 277, 293, 313,
    337, 359, 383, 409, 439, 467, 503, 541, 577, 619, 661, 709, 761, 823, 887, 953, 1031, 1109,
    1193, 1289, 1381, 1493, 1613, 1741, 1879, 2029, 2179, 2357, 2549, 2753, 2971, 3209, 3469, 3739,
    4027, 4349, 4703, 5087, 5503, 5953, 6427, 6949, 7517, 8123, 8783, 9497, 10273, 11113, 12011,
    12983, 14033, 15173, 16411, 17749, 19183, 20753, 22447, 24281, 26267, 28411, 30727, 33223,
    35933, 38873, 42043, 45481, 49201, 53201, 57557, 62233, 67307, 72817, 78779, 85229, 92203,
    99733, 107897, 116731, 126271, 136607, 147793, 159871, 172933, 187091, 202409, 218971, 236897,
    256279, 277261, 299951, 324503, 351061, 379787, 410857, 444487, 480881, 520241, 562841, 608903,
    658753, 712697, 771049, 834181, 902483, 976369, 1056323, 1142821, 1236397, 1337629, 1447153,
    1565659, 1693859, 1832561, 1982627, 2144977, 2320627, 2510653, 2716249, 2938679, 3179303,
    3439651, 3721303, 4026031, 4355707, 4712381, 5098259, 5515729, 5967347, 6456007, 6984629,
    7556579, 8175383, 8844859, 9569143, 10352717, 11200489, 12117689, 13109983, 14183539, 15345007,
    16601593, 17961079, 19431899, 21023161, 22744717, 24607243, 26622317, 28802401, 31160981,
    33712729, 36473443, 39460231, 42691603, 46187573, 49969847, 54061849, 58488943, 63278561,
    68460391, 74066549, 80131819, 86693767, 93793069, 101473717, 109783337, 118773397, 128499677,
    139022417, 150406843, 162723577, 176048909, 190465427, 206062531, 222936881, 241193053,
    260944219, 282312799, 305431229, 330442829, 357502601, 386778277, 418451333, 452718089,
    489790921, 529899637, 573292817, 620239453, 671030513, 725980837, 785430967, 849749479,
    919334987, 994618837, 1076067617, 1164186217, 1259520799, 1362662261, 1474249943, 1594975441,
    1725587117, 1866894511, 2019773507, 2185171673, 2364114217, 2557710269, 2767159799, 2993761039,
    3238918481, 3504151727, 3791104843, 4101556399, 4294967291,
];

/// `_Prime_rehash_policy::_M_next_bkt(n)`: the bucket count for at least `n` buckets.
fn next_bkt(n: u64) -> u64 {
    // Core's maps never come near 2³² buckets; past the table's end libstdc++ would continue its
    // 64-bit list.
    const FAST: [u64; 14] = [2, 2, 2, 3, 5, 5, 7, 7, 11, 11, 11, 11, 13, 13];
    if let Some(&b) = FAST.get(n as usize) {
        return if n == 0 {
            1
        } else {
            b
        };
    }
    let primes = &PRIMES[6..];
    let i = primes.partition_point(|&p| u64::from(p) < n);
    // Past the table (2³² buckets): unreachable, as above.
    primes.get(i).map_or(u64::MAX, |&p| u64::from(p))
}

/// A tip `CCoinsViewCache`'s map and `cachedCoinsUsage`, followed through its inserts, erases and
/// reallocations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreUsage {
    /// Live nodes: `GetCacheSize()`.
    entries: u64,
    /// Nodes carved from the chunks since the last reallocation: the peak of `entries`.
    carved: u64,
    buckets: u64,
    /// `_M_next_resize`: the count an insert may reach before the table grows.
    next_resize: u64,
    /// `cachedCoinsUsage`.
    scripts: u64,
}

impl Default for CoreUsage {
    fn default() -> Self {
        Self::new()
    }
}

impl CoreUsage {
    /// An empty map, as constructed and after `ReallocateCache`: one bucket, the first chunk.
    pub const fn new() -> Self {
        Self {
            entries: 0,
            carved: 0,
            buckets: 1,
            next_resize: 0,
            scripts: 0,
        }
    }

    /// `DynamicMemoryUsage()`.
    pub fn dynamic_usage(&self) -> u64 {
        let chunks = self.carved.div_ceil(NODES_PER_CHUNK).max(1);
        chunks * CHUNK_USAGE + malloc_usage(8 * self.buckets) + self.scripts
    }

    /// `GetCacheSize()`.
    pub fn cache_size(&self) -> u64 {
        self.entries
    }

    pub fn bucket_count(&self) -> u64 {
        self.buckets
    }

    /// A node inserted (`emplace`, `try_emplace` that inserts).
    pub fn insert(&mut self) {
        // _Prime_rehash_policy::_M_need_rehash(buckets, entries, 1)
        let n = self.entries + 1;
        if n > self.next_resize {
            let min_bkts = if self.next_resize == 0 {
                n.max(11)
            } else {
                n
            };
            if min_bkts >= self.buckets {
                self.buckets = next_bkt((min_bkts + 1).max(self.buckets * 2));
            }
            self.next_resize = self.buckets;
        }
        self.entries = n;
        self.carved = self.carved.max(n);
    }

    /// An `emplace` that found its key: libstdc++'s `_M_emplace_uniq` allocates the node before the
    /// lookup and frees it after, so with no node in the free list a fresh one is carved (`AddCoin`
    /// on a key the map holds, `coins.cpp:75`).
    pub fn emplace_existing(&mut self) {
        self.carved = self.carved.max(self.entries + 1);
    }

    /// A node erased; its memory stays in the pool's free list.
    pub fn erase(&mut self) {
        self.entries -= 1;
    }

    /// `cachedCoinsUsage += coin.DynamicMemoryUsage()` for a coin in Core's serialization.
    pub fn add_coin(&mut self, coin: &[u8]) {
        self.scripts += coin_usage(coin);
    }

    /// `cachedCoinsUsage -= coin.DynamicMemoryUsage()`.
    pub fn sub_coin(&mut self, coin: &[u8]) {
        self.scripts -= coin_usage(coin);
    }

    /// `cachedCoinsUsage` set outright: the sum of the map's coins' usage.
    pub fn set_cached_coins_usage(&mut self, scripts: u64) {
        self.scripts = scripts;
    }

    /// `ReallocateCache`, at the end of a flush: Core asserts the map was emptied first.
    pub fn reallocate(&mut self) {
        *self = Self::new();
    }
}

/// `Coin::DynamicMemoryUsage()` for a coin in Core's serialization (`VARINT(code)`,
/// `CompressedTxOut`): its decompressed script's heap, nothing up to 28 bytes. Compressed
/// scripts decode to 25 (P2PKH), 23 (P2SH), 35 (compressed P2PK) or 67 bytes (uncompressed P2PK,
/// assumed decompressible); a raw one to its stored size, or to one byte above `MAX_SCRIPT_SIZE`.
pub fn coin_usage(coin: &[u8]) -> u64 {
    let mut rest = coin;
    // The code, then the compressed amount.
    if read_varint(&mut rest).and_then(|_| read_varint(&mut rest)).is_none() {
        return 0;
    }
    let size = match read_varint(&mut rest) {
        Some(0) => 25,
        Some(1) => 23,
        Some(2 | 3) => 35,
        Some(4 | 5) => 67,
        Some(n) if n - 6 > MAX_SCRIPT_SIZE => 1,
        Some(n) => n - 6,
        None => 0,
    };
    if size > SCRIPT_INLINE {
        malloc_usage(size)
    } else {
        0
    }
}

/// Core's `VARINT` (`serialize.h` `ReadVarInt`).
fn read_varint(input: &mut &[u8]) -> Option<u64> {
    let mut n = 0u64;
    loop {
        let (&b, rest) = input.split_first()?;
        *input = rest;
        n = (n << 7) | u64::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some(n);
        }
        n = n.checked_add(1)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The oracle's USAGE after each of the first inserts of 25-byte scripts (no heap): one chunk
    /// from the first, the bucket array growing 13, 29, 59, 127, 257, 541, 1109, 2357, 5087, and
    /// the second chunk at the 2,521st node (`node-l1/tests/l1_coins_usage.rs` asserts these
    /// against the oracle).
    #[test]
    fn growth_follows_the_oracle() {
        let points = [
            (1, 262_336),
            (14, 262_464),
            (30, 262_704),
            (60, 263_248),
            (128, 264_288),
            (258, 266_560),
            (542, 271_104),
            (1110, 281_088),
            (2358, 302_928),
            (2521, 565_136),
        ];
        let mut u = CoreUsage::new();
        assert_eq!(u.dynamic_usage(), CHUNK_USAGE + 32, "an empty map holds a chunk and a bucket");
        let mut n = 0;
        for (count, usage) in points {
            while n < count {
                u.insert();
                n += 1;
            }
            assert_eq!(u.dynamic_usage(), usage, "after {count} inserts");
        }
    }

    #[test]
    fn erases_keep_the_chunks_and_buckets() {
        let mut u = CoreUsage::new();
        for _ in 0..20 {
            u.insert();
        }
        let full = u.dynamic_usage();
        for _ in 0..20 {
            u.erase();
        }
        assert_eq!((u.cache_size(), u.dynamic_usage()), (0, full));
        u.reallocate();
        assert_eq!(u.dynamic_usage(), CHUNK_USAGE + 32);
    }

    #[test]
    fn scripts_above_28_bytes_are_on_the_heap() {
        // code 2 (height 1), amount 0, then the script's size code.
        let coin = |size: &[u8]| [&[0x02, 0x00][..], size].concat();
        assert_eq!(coin_usage(&coin(&[0x00])), 0, "P2PKH, 25 bytes");
        assert_eq!(coin_usage(&coin(&[0x02])), 64, "compressed P2PK, 35 bytes");
        assert_eq!(coin_usage(&coin(&[0x04])), 96, "uncompressed P2PK, 67 bytes");
        assert_eq!(coin_usage(&coin(&[6 + 28])), 0);
        assert_eq!(coin_usage(&coin(&[6 + 29])), 48);
        assert_eq!(coin_usage(&coin(&[6 + 49])), 80);
        // VARINT 0x80 0x7f is 255: a 249-byte script.
        assert_eq!(coin_usage(&coin(&[0x80, 0x7f])), 272);
    }
}
