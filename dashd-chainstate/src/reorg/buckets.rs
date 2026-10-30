//! The bucket count of libstdc++'s `std::unordered_map`, which Core's accounting charges the pool's
//! map for (`MallocUsage(8 · bucket_count)`, `memusage.h:183`): libstdc++'s `_Prime_rehash_policy`
//! at GCC 15.2.0, the oracle's compiler (CH3 §4.3, D25). Citations are GCC 15.2.0's
//! `libstdc++-v3/src/c++11/hashtable_c++0x.cc` (`policy.cc`), `src/shared/hashtable-aux.cc`
//! (`aux.cc`) and the installed `bits/hashtable.h` and `bits/hashtable_policy.h`.
//!
//! [`Buckets`] follows one table through the calls Core's pool makes: `reserve`, which may shrink
//! the table, and the insert of a new key, which may grow it. `erase` and `clear` keep the buckets,
//! and hash values never move the count.
//!
//! At the load factor of 1.0, `_M_next_resize` is always `floor((double)count)`, the count itself
//! below 2^53 (0 while the table has its one inline bucket, `u64::MAX` at the largest prime). So
//! `rehash`'s restore writes back the value `_M_next_bkt` has just written, and
//! `_M_need_rehash`'s branch that keeps the count never runs. Both stay, line for line with
//! libstdc++.

/// `_Prime_rehash_policy`'s maximum load factor: the default, `1.0f` (`hashtable_policy.h:600`).
const MAX_LOAD_FACTOR: f64 = 1.0;

/// `_S_growth_factor`.
const GROWTH_FACTOR: u64 = 2;

/// `__fast_bkt` (`policy.cc:50-51`): the count for a request under 14.
const FAST_BKT: [u8; 14] = [2, 2, 2, 3, 5, 5, 7, 7, 11, 11, 11, 11, 13, 13];

/// `__prime_list` on LP64 (`aux.cc:28-91`), without its trailing sentinel.
#[rustfmt::skip]
const PRIMES: [u64; 304] = [
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
    3238918481, 3504151727, 3791104843, 4101556399, 4294967291, 6442450933, 8589934583, 12884901857,
    17179869143, 25769803693, 34359738337, 51539607367, 68719476731, 103079215087, 137438953447,
    206158430123, 274877906899, 412316860387, 549755813881, 824633720731, 1099511627689,
    1649267441579, 2199023255531, 3298534883309, 4398046511093, 6597069766607, 8796093022151,
    13194139533241, 17592186044399, 26388279066581, 35184372088777, 52776558133177, 70368744177643,
    105553116266399, 140737488355213, 211106232532861, 281474976710597, 562949953421231,
    1125899906842597, 2251799813685119, 4503599627370449, 9007199254740881, 18014398509481951,
    36028797018963913, 72057594037927931, 144115188075855859, 288230376151711717,
    576460752303423433, 1152921504606846883, 2305843009213693951, 4611686018427387847,
    9223372036854775783, 18446744073709551557,
];

/// The search's first index: `_M_next_bkt` looks from `__prime_list + 6` (17), as every smaller
/// request takes [`FAST_BKT`].
const SEARCH_FROM: usize = 6;

/// A table's bucket count and the policy state that moves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Buckets {
    /// `_M_bucket_count`.
    count: u64,
    /// `_M_next_resize`: the element count past which an insert rehashes, 0 before the first.
    next_resize: u64,
}

/// A new table: its one inline bucket, and nothing allocated (`hashtable.h:359`,
/// `hashtable_policy.h:601`).
impl Default for Buckets {
    fn default() -> Self {
        Buckets {
            count: 1,
            next_resize: 0,
        }
    }
}

impl Buckets {
    /// `bucket_count()`.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// `_M_next_resize`, which a test reads after [`Self::next_bkt`].
    pub fn next_resize(&self) -> u64 {
        self.next_resize
    }

    /// `_M_next_bkt(n)` (`policy.cc:46-88`): the smallest count at least `n` that the policy uses
    /// (1 for 0), recording the element count it holds. Past the second-largest prime it answers
    /// the largest, and no insert grows the table again.
    pub fn next_bkt(&mut self, n: u64) -> u64 {
        if n < FAST_BKT.len() as u64 {
            if n == 0 {
                return 1;
            }
            let count = u64::from(FAST_BKT[n as usize]);
            self.next_resize = floor_load(count);
            return count;
        }
        let last = PRIMES.len() - 1;
        let at = SEARCH_FROM + PRIMES[SEARCH_FROM..last].partition_point(|&p| p < n);
        self.next_resize = if at == last {
            u64::MAX
        } else {
            floor_load(PRIMES[at])
        };
        PRIMES[at]
    }

    /// `reserve(n)` on a table of `len` entries: `rehash(_M_bkt_for_elements(n))`
    /// (`hashtable_policy.h:997-1002`).
    pub fn reserve(&mut self, len: u64, n: u64) {
        self.rehash(len, bkt_for_elements(n));
    }

    /// `rehash(n)` on a table of `len` entries (`hashtable.h:2729-2742`): the count for
    /// `max(_M_bkt_for_elements(len + 1), n)`, taken whenever it differs from the current one,
    /// **smaller included**. Otherwise the guard restores the `_M_next_resize` that `_M_next_bkt`
    /// overwrote.
    fn rehash(&mut self, len: u64, n: u64) {
        let saved = self.next_resize;
        let count = self.next_bkt(bkt_for_elements(len + 1).max(n));
        if count != self.count {
            self.count = count;
        } else {
            self.next_resize = saved;
        }
    }

    /// The insert of a key absent from a table of `len` entries: `_M_insert_unique_node`'s
    /// `_M_need_rehash(bucket_count, len, 1)` (`hashtable.h:2468-2490`). An `emplace` of a key the
    /// table holds inserts nothing and leaves the count.
    pub fn insert(&mut self, len: u64) {
        if let Some(count) = self.need_rehash(len, 1) {
            self.count = count;
        }
    }

    /// `_M_need_rehash(count, n_elt, n_ins)` (`policy.cc:100-123`): the new count if the inserts
    /// would load the table past 1.0, which at least doubles it. The first insert into a table with
    /// nothing allocated asks for at least 11 buckets, and gets 13.
    fn need_rehash(&mut self, n_elt: u64, n_ins: u64) -> Option<u64> {
        if n_elt + n_ins <= self.next_resize {
            return None;
        }
        let first = if self.next_resize == 0 {
            11
        } else {
            0
        };
        let min_bkts = (n_elt + n_ins).max(first) as f64 / MAX_LOAD_FACTOR;
        if min_bkts >= self.count as f64 {
            return Some(
                self.next_bkt(((min_bkts.floor() + 1.0) as u64).max(self.count * GROWTH_FACTOR)),
            );
        }
        self.next_resize = floor_load(self.count);
        None
    }
}

/// `_M_bkt_for_elements(n)` (`hashtable_policy.h:614-615`): `ceil(n / max_load_factor)`.
fn bkt_for_elements(n: u64) -> u64 {
    (n as f64 / MAX_LOAD_FACTOR).ceil() as u64
}

/// `__builtin_floor(count * (double)_M_max_load_factor)`: the element count `count` buckets hold.
fn floor_load(count: u64) -> u64 {
    (count as f64 * MAX_LOAD_FACTOR).floor() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prime_table_is_sorted_and_ends_below_two_to_the_64() {
        assert!(PRIMES.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(PRIMES[SEARCH_FROM], 17);
        assert_eq!(PRIMES[PRIMES.len() - 1], 18_446_744_073_709_551_557);
    }

    /// The first insert into a new table allocates 13 buckets (11 asked, plus one), and reserving
    /// for fewer entries after the table has grown shrinks it.
    #[test]
    fn a_new_table_starts_at_13_and_reserve_can_shrink() {
        let mut buckets = Buckets::default();
        buckets.insert(0);
        assert_eq!((buckets.count(), buckets.next_resize()), (13, 13));
        buckets.reserve(1, 1000);
        assert_eq!(buckets.count(), 1031);
        buckets.reserve(1, 1);
        assert_eq!(buckets.count(), 2);
    }
}
