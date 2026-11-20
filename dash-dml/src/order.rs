//! Core's iteration order (EV2 §2.7; quirk 41): immer's CHAMP pre-order over
//! `ImmerHasher = ReadLE64(proTxHash)` with 5-bit branching on a 64-bit host, collision nodes in
//! most-recently-set-first order.

use crate::ProTxHash;
use crate::list::CollisionBuckets;

/// immer's `default_bits` (`immer/config.hpp:126`): 32-way nodes.
const BITS: u32 = 5;
/// The depth of a collision node, `max_depth` for a 64-bit `hash_t` (`immer/detail/hamts/bits.hpp`):
/// 12 levels of 5 bits and a 13th of 4.
const MAX_DEPTH: u32 = 13;

/// The slot of hash `h` at `depth` (`(h >> 5·depth) & 31`; the last level has 4 bits).
fn slot(h: u64, depth: u32) -> u64 {
    (h >> (BITS * depth)) & 31
}

/// The key that lays the trie's leaves out left to right: the slots from the root down, most
/// significant first.
fn path(h: u64) -> u64 {
    (0..MAX_DEPTH).fold(0u64, |acc, depth| {
        let width = if depth == MAX_DEPTH - 1 {
            64 - BITS * (MAX_DEPTH - 1)
        } else {
            BITS
        };
        (acc << width) | slot(h, depth)
    })
}

/// `ForEachMN`'s order (`deterministicmns.h` v24:301-324) over `keys`, whose 64-bit collision
/// classes of two or more are ordered by `buckets` (front first).
///
/// At each trie node the keys alone in their slot come first, in slot order, then the sub-tries
/// in slot order (`champ_iterator.hpp`); keys with one 64-bit hash sit in a collision node at
/// depth 13 in bucket order.
// Core: evo/deterministicmns.h:301 CDeterministicMNList::ForEachMN
pub(crate) fn core_order<'a>(
    keys: impl Iterator<Item = &'a ProTxHash>,
    buckets: &CollisionBuckets,
) -> Vec<ProTxHash> {
    let mut leaves: Vec<(u64, ProTxHash)> = keys.map(|k| (path(k.immer_hash()), *k)).collect();
    leaves.sort_unstable();
    let mut out = Vec::with_capacity(leaves.len());
    emit(&leaves, 0, buckets, &mut out);
    out
}

/// Emits the node holding `run` (keys sharing the slots above `depth`).
fn emit(
    run: &[(u64, ProTxHash)],
    depth: u32,
    buckets: &CollisionBuckets,
    out: &mut Vec<ProTxHash>,
) {
    if depth == MAX_DEPTH {
        // One 64-bit hash: a collision node.
        match buckets.get(&run[0].1.immer_hash()) {
            Some(bucket) if bucket.len() == run.len() => out.extend(bucket.iter()),
            // Not reachable while I-C holds; keep the key set, in memcmp order.
            _ => out.extend(run.iter().map(|(_, k)| *k)),
        }
        return;
    }
    let groups =
        || run.chunk_by(|a, b| slot(a.1.immer_hash(), depth) == slot(b.1.immer_hash(), depth));
    out.extend(groups().filter(|g| g.len() == 1).map(|g| g[0].1));
    for group in groups().filter(|g| g.len() > 1) {
        emit(group, depth + 1, buckets, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbl::OrdMap;
    use std::collections::VecDeque;
    use std::sync::Arc;

    fn key(h: u64, tail: u8) -> ProTxHash {
        let mut b = [tail; 32];
        b[..8].copy_from_slice(&h.to_le_bytes());
        ProTxHash::from(b)
    }

    /// A naive trie built by insertion, iterated in CHAMP pre-order.
    #[derive(Default)]
    struct Node {
        slots: std::collections::BTreeMap<u64, Entry>,
    }
    enum Entry {
        Value(ProTxHash),
        Child(Node),
        Collision(Vec<ProTxHash>),
    }
    impl Node {
        fn insert(&mut self, k: ProTxHash, depth: u32) {
            let s = slot(k.immer_hash(), depth);
            match self.slots.remove(&s) {
                None => {
                    self.slots.insert(s, Entry::Value(k));
                }
                Some(Entry::Value(old))
                    if depth + 1 == MAX_DEPTH && old.immer_hash() == k.immer_hash() =>
                {
                    self.slots.insert(s, Entry::Collision(vec![k, old]));
                }
                Some(Entry::Value(old)) => {
                    let mut child = Node::default();
                    child.insert(old, depth + 1);
                    child.insert(k, depth + 1);
                    self.slots.insert(s, Entry::Child(child));
                }
                Some(Entry::Child(mut child)) => {
                    child.insert(k, depth + 1);
                    self.slots.insert(s, Entry::Child(child));
                }
                Some(Entry::Collision(mut keys)) => {
                    keys.insert(0, k);
                    self.slots.insert(s, Entry::Collision(keys));
                }
            }
        }
        fn walk(&self, out: &mut Vec<ProTxHash>) {
            for entry in self.slots.values() {
                if let Entry::Value(k) = entry {
                    out.push(*k);
                }
            }
            for entry in self.slots.values() {
                match entry {
                    Entry::Child(c) => c.walk(out),
                    Entry::Collision(keys) => out.extend(keys),
                    Entry::Value(_) => {}
                }
            }
        }
    }

    fn naive(keys: &[ProTxHash]) -> Vec<ProTxHash> {
        let mut root = Node::default();
        for k in keys {
            root.insert(*k, 0);
        }
        let mut out = Vec::new();
        root.walk(&mut out);
        out
    }

    /// Adversarial key sets: shared 5k-bit prefixes for every k, plus a pseudo-random spread.
    #[test]
    fn matches_a_naive_trie() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for shared_bits in [0u32, 5, 10, 25, 40, 55, 59, 60, 63] {
            let mask = if shared_bits == 0 {
                0
            } else {
                u64::MAX >> (64 - shared_bits)
            };
            let base = next() & mask;
            let keys: Vec<ProTxHash> =
                (0..300).map(|i| key(base | (next() & !mask), i as u8)).collect();
            let mut unique = keys.clone();
            unique.sort();
            unique.dedup_by_key(|k| k.immer_hash());
            assert_eq!(
                core_order(unique.iter(), &OrdMap::new()),
                naive(&unique),
                "{shared_bits} bits"
            );
        }
    }

    /// Collision buckets come out in their stored order, after the node's inline keys.
    #[test]
    fn collision_buckets_follow_their_sequence() {
        let (a, b, c, solo) = (key(7, 1), key(7, 2), key(7, 3), key(7 | (1 << 63), 0));
        let mut buckets = OrdMap::new();
        buckets.insert(7, Arc::new(VecDeque::from([c, a, b])));
        let order = core_order([a, b, c, solo].iter(), &buckets);
        assert_eq!(order, [solo, c, a, b]);
        // The naive trie prepends on insertion: inserting a, b, c gives c, b, a.
        assert_eq!(naive(&[a, b, c, solo]), [solo, c, b, a]);
    }
}
