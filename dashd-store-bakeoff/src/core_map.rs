//! The Core-model arm's coins map (CH2c D10): `CCoinsMap`'s memory layout, so that the arm holds
//! what Core holds. Nodes are 104 bytes, carved from 256 KiB chunks and reused through a free
//! list as v24's `PoolResource` does. The bucket array has 8 bytes per bucket and is sized by
//! libstdc++'s policy (`CoreUsage`). Chunks and buckets live until [`CoreMap::reallocate`]. A
//! node stores the outpoint, the flags and Core's coin bytes inline; Core keeps scripts above 28
//! bytes on the heap, which the arm's ballast stands for (`CoreUsage`'s `cachedCoinsUsage`).

use crate::cache::KeyHasher;
#[cfg(test)]
use crate::core_usage::CHUNK_USAGE;
use crate::core_usage::{CoreUsage, NODES_PER_CHUNK};
use dash_coreimport::trace::OutKey;
use std::hash::Hasher;

/// A coin longer than this is kept in the side table (Core's coins rarely are).
const INLINE: usize = 58;
const NIL: u32 = u32::MAX;

/// `pair<const COutPoint, CCoinsCacheEntry>` and the chain's next pointer: 104 bytes.
#[derive(Clone, Copy)]
#[repr(C, align(8))]
struct Node {
    key: OutKey,
    next: u32,
    flags: u8,
    /// The coin's length; 0 for a spent coin, `u8::MAX` for one in the side table.
    len: u8,
    coin: [u8; INLINE],
}

const _: () = assert!(std::mem::size_of::<Node>() == crate::core_usage::NODE_BYTES as usize);

const EMPTY: Node = Node {
    key: [0; 36],
    next: NIL,
    flags: 0,
    len: 0,
    coin: [0; INLINE],
};

pub struct CoreMap {
    chunks: Vec<Box<[Node]>>,
    /// Nodes carved so far.
    carved: u32,
    free: u32,
    buckets: Vec<u64>,
    side: Vec<Box<[u8]>>,
    usage: CoreUsage,
}

impl Default for CoreMap {
    fn default() -> Self {
        Self::new()
    }
}

fn hash(k: &OutKey) -> u64 {
    let mut h = KeyHasher::default();
    h.write(k);
    h.finish()
}

impl CoreMap {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            carved: 0,
            free: NIL,
            buckets: vec![u64::from(NIL); 1],
            side: Vec::new(),
            usage: CoreUsage::new(),
        }
    }

    pub fn usage(&self) -> &CoreUsage {
        &self.usage
    }

    /// The bytes this map holds: its chunks, buckets and side table.
    pub fn bytes(&self) -> u64 {
        let side: usize = self.side.iter().map(|c| c.len()).sum();
        (self.chunks.len() * std::mem::size_of::<Node>() * NODES_PER_CHUNK as usize
            + self.buckets.len() * 8
            + side) as u64
    }

    fn node(&self, i: u32) -> &Node {
        let per = NODES_PER_CHUNK as u32;
        &self.chunks[(i / per) as usize][(i % per) as usize]
    }

    fn node_mut(&mut self, i: u32) -> &mut Node {
        let per = NODES_PER_CHUNK as u32;
        &mut self.chunks[(i / per) as usize][(i % per) as usize]
    }

    fn bucket(&self, k: &OutKey) -> usize {
        (hash(k) % self.buckets.len() as u64) as usize
    }

    fn find(&self, k: &OutKey) -> Option<u32> {
        let mut i = self.buckets[self.bucket(k)] as u32;
        while i != NIL {
            let n = self.node(i);
            if n.key == *k {
                return Some(i);
            }
            i = n.next;
        }
        None
    }

    /// The entry's flags and coin (`None` once spent).
    pub fn get(&self, k: &OutKey) -> Option<(u8, Option<&[u8]>)> {
        self.find(k).map(|i| self.entry(i))
    }

    fn entry(&self, i: u32) -> (u8, Option<&[u8]>) {
        let n = self.node(i);
        let coin = match n.len {
            0 => None,
            u8::MAX => Some(
                &*self.side
                    [n.coin[0] as usize | (n.coin[1] as usize) << 8 | (n.coin[2] as usize) << 16],
            ),
            l => Some(&n.coin[..l as usize]),
        };
        (n.flags, coin)
    }

    fn set_coin(&mut self, i: u32, coin: Option<&[u8]>) {
        let side_ix = match coin {
            Some(c) if c.len() > INLINE => {
                // The node holds the side index in 24 bits.
                assert!(self.side.len() < 1 << 24, "the side table outgrew its index");
                self.side.push(c.into());
                Some(self.side.len() - 1)
            }
            _ => None,
        };
        let n = self.node_mut(i);
        match (coin, side_ix) {
            (None, _) => n.len = 0,
            (Some(_), Some(ix)) => {
                n.len = u8::MAX;
                n.coin[..3].copy_from_slice(&(ix as u32).to_le_bytes()[..3]);
            }
            (Some(c), None) => {
                n.len = c.len() as u8;
                n.coin[..c.len()].copy_from_slice(c);
            }
        }
    }

    /// `cachedCoinsUsage` without node `i`'s coin, if it holds one.
    fn sub_coin_at(&mut self, i: u32) {
        if let (_, Some(old)) = self.entry(i) {
            let old = old.to_vec();
            self.usage.sub_coin(&old);
        }
    }

    /// Sets an existing entry's flags and coin, `cachedCoinsUsage` following the coin.
    pub fn update(&mut self, k: &OutKey, flags: u8, coin: Option<&[u8]>) {
        let i = self.find(k).expect("an existing entry");
        self.sub_coin_at(i);
        if let Some(c) = coin {
            self.usage.add_coin(c);
        }
        self.set_coin(i, coin);
        self.node_mut(i).flags = flags;
    }

    /// Inserts a new entry (`emplace`).
    pub fn insert(&mut self, k: OutKey, flags: u8, coin: Option<&[u8]>) {
        debug_assert!(self.find(&k).is_none());
        let before = self.usage.bucket_count();
        self.usage.insert();
        if self.usage.bucket_count() != before {
            self.rehash(self.usage.bucket_count() as usize);
        }
        let i = if self.free != NIL {
            let i = self.free;
            self.free = self.node(i).next;
            i
        } else {
            if u64::from(self.carved).is_multiple_of(NODES_PER_CHUNK) {
                self.chunks.push(vec![EMPTY; NODES_PER_CHUNK as usize].into_boxed_slice());
            }
            self.carved += 1;
            self.carved - 1
        };
        let b = self.bucket(&k);
        let head = self.buckets[b] as u32;
        *self.node_mut(i) = Node {
            key: k,
            next: head,
            flags,
            ..EMPTY
        };
        self.buckets[b] = u64::from(i);
        if let Some(c) = coin {
            self.usage.add_coin(c);
        }
        self.set_coin(i, coin);
    }

    /// Erases an entry; its node goes to the free list.
    pub fn remove(&mut self, k: &OutKey) {
        let b = self.bucket(k);
        let mut prev = NIL;
        let mut i = self.buckets[b] as u32;
        while self.node(i).key != *k {
            prev = i;
            i = self.node(i).next;
        }
        self.sub_coin_at(i);
        let next = self.node(i).next;
        if prev == NIL {
            self.buckets[b] = u64::from(next);
        } else {
            self.node_mut(prev).next = next;
        }
        let free = self.free;
        *self.node_mut(i) = Node {
            next: free,
            ..EMPTY
        };
        self.free = i;
        self.usage.erase();
    }

    fn rehash(&mut self, buckets: usize) {
        let old = std::mem::replace(&mut self.buckets, vec![u64::from(NIL); buckets]);
        for head in old {
            let mut i = head as u32;
            while i != NIL {
                let next = self.node(i).next;
                let b = self.bucket(&self.node(i).key);
                let h = self.buckets[b] as u32;
                self.node_mut(i).next = h;
                self.buckets[b] = u64::from(i);
                i = next;
            }
        }
    }

    /// Every entry, in bucket order (Core's flush walks its map's node list).
    pub fn for_each(&self, mut f: impl FnMut(&OutKey, u8, Option<&[u8]>)) {
        for &head in &self.buckets {
            let mut i = head as u32;
            while i != NIL {
                let (flags, coin) = self.entry(i);
                f(&self.node(i).key, flags, coin);
                i = self.node(i).next;
            }
        }
    }

    /// `ReallocateCache`: an empty map, its chunks and buckets released.
    pub fn reallocate(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u32) -> OutKey {
        let mut k = [0; 36];
        k[..4].copy_from_slice(&n.to_le_bytes());
        k[8..12].copy_from_slice(&n.wrapping_mul(0x9e37_79b9).to_le_bytes());
        k
    }

    #[test]
    fn holds_what_core_accounts() {
        let mut m = CoreMap::new();
        let long = vec![7u8; 80];
        for n in 0..6000 {
            let coin: &[u8] = if n % 100 == 0 {
                &long
            } else {
                &[0x02, 0x00, 0x00, 1, 2]
            };
            m.insert(key(n), 1, Some(coin));
        }
        for n in (0..6000).step_by(3) {
            m.remove(&key(n));
        }
        for n in 6000..7000 {
            m.insert(key(n), 3, None);
        }
        for n in 0..7000 {
            let found = m.get(&key(n)).is_some();
            assert_eq!(found, n >= 6000 || n % 3 != 0, "{n}");
        }
        assert_eq!(m.get(&key(100)).unwrap().1, Some(&long[..]));
        m.update(&key(100), 1, None);
        assert_eq!(m.get(&key(100)), Some((1, None)));
        let u = m.usage();
        assert_eq!(u.cache_size(), 6000 - 2000 + 1000);
        // The chunks and the bucket array are Core's, byte for byte.
        let chunks = (6000u64).div_ceil(NODES_PER_CHUNK);
        assert_eq!(m.chunks.len() as u64, chunks);
        assert_eq!(m.buckets.len() as u64, u.bucket_count());
        let mut seen = 0;
        m.for_each(|_, _, _| seen += 1);
        assert_eq!(seen, 5000);
        m.reallocate();
        assert_eq!((m.chunks.len(), m.usage().dynamic_usage()), (0, CHUNK_USAGE + 32));
    }
}
