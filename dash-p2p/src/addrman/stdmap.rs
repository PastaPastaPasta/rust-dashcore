//! libstdc++'s `std::unordered_map<int, T>` iteration order (`bits/hashtable.h`,
//! `_Prime_rehash_policy`), for Core's `mapInfo`.
//!
//! Core iterates `mapInfo` in three observable places: `Serialize` (the entry order of
//! `peers.dat`), `Unserialize`'s prune loop (which `Delete`s, i.e. permutes `vRandom`, in
//! iteration order) and `CheckAddrman` (which error code wins). The order is the singly linked
//! node list of libstdc++'s hashtable: `std::hash<int>` is the identity, a node goes to the front
//! of its bucket (or of the whole list when its bucket is empty), erasing unlinks, and a rehash
//! relinks every node. This module replays exactly that on the keys, so a dump is byte-identical
//! to dashd's built against libstdc++ (every Linux and Windows release build); a libc++ build
//! (macOS) orders differently.

use std::collections::HashMap;

/// `__fast_bkt` in `_Prime_rehash_policy::_M_next_bkt` (libstdc++ `hashtable_c++0x.cc`).
const FAST_BKT: [usize; 14] = [2, 2, 2, 3, 5, 5, 7, 7, 11, 11, 11, 11, 13, 13];

/// `__prime_list` from index 6 (where `_M_next_bkt`'s `lower_bound` starts), up to the largest
/// 32-bit entry; dumped from libstdc++ with `_Prime_rehash_policy::_M_next_bkt`. AddrMan never
/// holds more than 81,920 entries (+1 while adding), i.e. 85,229 buckets.
#[rustfmt::skip]
const PRIMES: [usize; 250] = [
    17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97, 103, 109, 113, 127,
    137, 139, 149, 157, 167, 179, 193, 199, 211, 227, 241, 257, 277, 293, 313, 337, 359, 383, 409,
    439, 467, 503, 541, 577, 619, 661, 709, 761, 823, 887, 953, 1031, 1109, 1193, 1289, 1381, 1493,
    1613, 1741, 1879, 2029, 2179, 2357, 2549, 2753, 2971, 3209, 3469, 3739, 4027, 4349, 4703, 5087,
    5503, 5953, 6427, 6949, 7517, 8123, 8783, 9497, 10273, 11113, 12011, 12983, 14033, 15173, 16411,
    17749, 19183, 20753, 22447, 24281, 26267, 28411, 30727, 33223, 35933, 38873, 42043, 45481, 49201,
    53201, 57557, 62233, 67307, 72817, 78779, 85229, 92203, 99733, 107897, 116731, 126271, 136607,
    147793, 159871, 172933, 187091, 202409, 218971, 236897, 256279, 277261, 299951, 324503, 351061,
    379787, 410857, 444487, 480881, 520241, 562841, 608903, 658753, 712697, 771049, 834181, 902483,
    976369, 1056323, 1142821, 1236397, 1337629, 1447153, 1565659, 1693859, 1832561, 1982627,
    2144977, 2320627, 2510653, 2716249, 2938679, 3179303, 3439651, 3721303, 4026031, 4355707,
    4712381, 5098259, 5515729, 5967347, 6456007, 6984629, 7556579, 8175383, 8844859, 9569143,
    10352717, 11200489, 12117689, 13109983, 14183539, 15345007, 16601593, 17961079, 19431899,
    21023161, 22744717, 24607243, 26622317, 28802401, 31160981, 33712729, 36473443, 39460231,
    42691603, 46187573, 49969847, 54061849, 58488943, 63278561, 68460391, 74066549, 80131819,
    86693767, 93793069, 101473717, 109783337, 118773397, 128499677, 139022417, 150406843,
    162723577, 176048909, 190465427, 206062531, 222936881, 241193053, 260944219, 282312799,
    305431229, 330442829, 357502601, 386778277, 418451333, 452718089, 489790921, 529899637,
    573292817, 620239453, 671030513, 725980837, 785430967, 849749479, 919334987, 994618837,
    1076067617, 1164186217, 1259520799, 1362662261, 1474249943, 1594975441, 1725587117,
    1866894511, 2019773507, 2185171673, 2364114217, 2557710269, 2767159799, 2993761039,
    3238918481, 3504151727, 3791104843, 4101556399, 4294967291,
];

/// The node a bucket's slot points at: the one *before* the bucket's first node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Before {
    /// `_M_before_begin`.
    Head,
    /// The node with this key.
    Node(i32),
}

#[derive(Clone, Debug)]
struct Node<V> {
    value: V,
    next: Option<i32>,
}

/// `std::unordered_map<int, V>` with libstdc++'s node order. Lookups go through a `HashMap`;
/// only the order is emulated.
#[derive(Clone, Debug)]
pub(super) struct StdMap<V> {
    nodes: HashMap<i32, Node<V>>,
    /// `_M_before_begin._M_nxt`.
    head: Option<i32>,
    /// `_M_buckets`: `_M_bucket_count` slots, starting with libstdc++'s single bucket.
    buckets: Vec<Option<Before>>,
    /// `_Prime_rehash_policy::_M_next_resize`.
    next_resize: usize,
}

impl<V> Default for StdMap<V> {
    fn default() -> Self {
        Self {
            nodes: HashMap::new(),
            head: None,
            buckets: vec![None],
            next_resize: 0,
        }
    }
}

/// `std::hash<int>` (identity, sign-extended to `size_t`) modulo the bucket count.
fn bucket_of(key: i32, count: usize) -> usize {
    ((key as i64 as u64) % count as u64) as usize
}

impl<V> StdMap<V> {
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.nodes.len()
    }

    pub(super) fn contains(&self, key: i32) -> bool {
        self.nodes.contains_key(&key)
    }

    pub(super) fn get(&self, key: i32) -> Option<&V> {
        self.nodes.get(&key).map(|n| &n.value)
    }

    pub(super) fn get_mut(&mut self, key: i32) -> Option<&mut V> {
        self.nodes.get_mut(&key).map(|n| &mut n.value)
    }

    /// The keys in iteration order.
    pub(super) fn keys(&self) -> Vec<i32> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut cur = self.head;
        while let Some(k) = cur {
            out.push(k);
            cur = self.nodes[&k].next;
        }
        out
    }

    /// The entries in iteration order.
    pub(super) fn iter(&self) -> impl Iterator<Item = (i32, &V)> + '_ {
        let mut cur = self.head;
        core::iter::from_fn(move || {
            let k = cur?;
            let node = &self.nodes[&k];
            cur = node.next;
            Some((k, &node.value))
        })
    }

    fn next_of(&self, before: Before) -> Option<i32> {
        match before {
            Before::Head => self.head,
            Before::Node(k) => self.nodes[&k].next,
        }
    }

    fn set_next(&mut self, before: Before, next: Option<i32>) {
        match before {
            Before::Head => self.head = next,
            Before::Node(k) => self.nodes.get_mut(&k).expect("linked node").next = next,
        }
    }

    /// `_Prime_rehash_policy::_M_next_bkt`.
    fn next_bkt(&mut self, n: usize) -> usize {
        if n < FAST_BKT.len() {
            if n == 0 {
                return 1;
            }
            self.next_resize = FAST_BKT[n];
            return FAST_BKT[n];
        }
        let p = *PRIMES.iter().find(|&&p| p >= n).expect("bucket count beyond AddrMan's range");
        self.next_resize = p;
        p
    }

    /// `_Prime_rehash_policy::_M_need_rehash(n_bkt, n_elt, 1)` (max load factor 1, growth 2).
    fn need_rehash(&mut self) -> Option<usize> {
        let n_bkt = self.buckets.len();
        let n_elt = self.nodes.len();
        if n_elt < self.next_resize {
            return None;
        }
        let floor = if self.next_resize == 0 {
            11
        } else {
            0
        };
        let min_bkts = (n_elt + 1).max(floor) as f64;
        if min_bkts >= n_bkt as f64 {
            Some(self.next_bkt((min_bkts.floor() as usize + 1).max(n_bkt * 2)))
        } else {
            self.next_resize = n_bkt;
            None
        }
    }

    /// `_M_rehash(n, true_type)`.
    fn rehash(&mut self, count: usize) {
        let mut buckets: Vec<Option<Before>> = vec![None; count];
        let mut p = self.head;
        self.head = None;
        let mut bbegin_bkt = 0usize;
        while let Some(cur) = p {
            let next = self.nodes[&cur].next;
            let bkt = bucket_of(cur, count);
            match buckets[bkt] {
                None => {
                    let head = self.head;
                    self.nodes.get_mut(&cur).expect("node").next = head;
                    self.head = Some(cur);
                    buckets[bkt] = Some(Before::Head);
                    if head.is_some() {
                        buckets[bbegin_bkt] = Some(Before::Node(cur));
                    }
                    bbegin_bkt = bkt;
                }
                Some(before) => {
                    let after = self.next_of(before);
                    self.nodes.get_mut(&cur).expect("node").next = after;
                    self.set_next(before, Some(cur));
                }
            }
            p = next;
        }
        self.buckets = buckets;
    }

    /// Inserts a key that is not present (`operator[]` on a new key, then assignment):
    /// `_M_insert_unique_node` and `_M_insert_bucket_begin`.
    pub(super) fn insert_new(&mut self, key: i32, value: V) {
        assert!(!self.nodes.contains_key(&key), "StdMap::insert_new on a present key");
        if let Some(count) = self.need_rehash() {
            self.rehash(count);
        }
        let count = self.buckets.len();
        let bkt = bucket_of(key, count);
        self.nodes.insert(
            key,
            Node {
                value,
                next: None,
            },
        );
        match self.buckets[bkt] {
            Some(before) => {
                let after = self.next_of(before);
                self.nodes.get_mut(&key).expect("node").next = after;
                self.set_next(before, Some(key));
            }
            None => {
                let head = self.head;
                self.nodes.get_mut(&key).expect("node").next = head;
                self.head = Some(key);
                if let Some(next) = head {
                    self.buckets[bucket_of(next, count)] = Some(Before::Node(key));
                }
                self.buckets[bkt] = Some(Before::Head);
            }
        }
    }

    /// `erase(key)`: `_M_find_before_node`, then `_M_erase(bkt, prev, n)`. Returns the value.
    pub(super) fn remove(&mut self, key: i32) -> Option<V> {
        if !self.nodes.contains_key(&key) {
            return None;
        }
        let count = self.buckets.len();
        let bkt = bucket_of(key, count);
        let first = self.buckets[bkt].expect("a present key's bucket is linked");
        let mut prev = first;
        while self.next_of(prev) != Some(key) {
            prev = Before::Node(self.next_of(prev).expect("key is in its bucket's chain"));
        }
        let next = self.nodes[&key].next;
        if prev == first {
            // _M_remove_bucket_begin
            match next {
                None => self.buckets[bkt] = None,
                Some(n) => {
                    let next_bkt = bucket_of(n, count);
                    if next_bkt != bkt {
                        self.buckets[next_bkt] = self.buckets[bkt];
                        self.buckets[bkt] = None;
                    }
                }
            }
        } else if let Some(n) = next {
            let next_bkt = bucket_of(n, count);
            if next_bkt != bkt {
                self.buckets[next_bkt] = Some(prev);
            }
        }
        self.set_next(prev, next);
        self.nodes.remove(&key).map(|n| n.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bucket counts libstdc++ grows through (`grow at size n -> count`, dumped from
    /// `std::unordered_map<int, int>` with GCC 15's libstdc++).
    #[test]
    fn growth_matches_libstdcxx() {
        let mut m = StdMap::default();
        let mut seen = vec![];
        let mut last = m.buckets.len();
        for i in 0..90_000 {
            m.insert_new(i, ());
            if m.buckets.len() != last {
                last = m.buckets.len();
                seen.push((m.len(), last));
            }
        }
        assert_eq!(
            seen,
            [
                (1, 13),
                (14, 29),
                (30, 59),
                (60, 127),
                (128, 257),
                (258, 541),
                (542, 1109),
                (1110, 2357),
                (2358, 5087),
                (5088, 10273),
                (10274, 20753),
                (20754, 42043),
                (42044, 85229),
                (85230, 172933)
            ]
        );
    }

    /// Iteration orders recorded from libstdc++ (GCC 15) for the same operations.
    #[test]
    fn order_matches_libstdcxx() {
        let mut m = StdMap::default();
        for i in 0..20 {
            m.insert_new(i, ());
        }
        // 13 buckets until the 14th key, then 29: 13..=19 went to empty buckets (list front).
        assert_eq!(
            m.keys(),
            [19, 18, 17, 16, 15, 14, 13, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
        );
        for k in [3, 18, 0, 7] {
            m.remove(k);
        }
        for i in [40, 33, 29, 58, 30] {
            m.insert_new(i, ());
        }
        assert_eq!(m.keys(), STDCXX_ORDER_AFTER_EDITS);
    }

    /// `for (auto& e : m)` over a libstdc++ `std::unordered_map<int, int>` after keys 0..20
    /// were inserted, 3, 18, 0, 7 erased, then 40, 33, 29, 58, 30 inserted (29 buckets).
    const STDCXX_ORDER_AFTER_EDITS: [i32; 21] =
        [58, 29, 19, 17, 16, 15, 14, 13, 30, 1, 2, 33, 4, 5, 6, 8, 9, 10, 40, 11, 12];
}
