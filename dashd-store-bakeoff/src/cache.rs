//! The coins cache model (protocol §3.1): Core's DIRTY/FRESH semantics over a pre-allocated map.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

use dash_coreimport::trace::OutKey;

use crate::store::KeyBuf;

/// Accounted bytes per entry: A1 §3.4's estimate for a P2PKH entry with table overhead.
pub const ENTRY_COST: usize = 80;
/// Coins up to this length are stored inline.
const INLINE: usize = 33;
/// Core's `MAX_BLOCK_COINSDB_USAGE_BYTES` (`validation.cpp` `GetCoinsCacheSizeState`).
const MAX_BLOCK_COINSDB_USAGE: usize = 10 << 20;

pub const DIRTY: u8 = 1;
pub const FRESH: u8 = 2;
pub const SPENT: u8 = 4;

pub enum CoinBuf {
    Inline(u8, [u8; INLINE]),
    Heap(Box<[u8]>),
}

impl CoinBuf {
    const EMPTY: Self = Self::Inline(0, [0; INLINE]);

    fn new(bytes: &[u8]) -> Self {
        if bytes.len() <= INLINE {
            let mut b = [0; INLINE];
            b[..bytes.len()].copy_from_slice(bytes);
            Self::Inline(bytes.len() as u8, b)
        } else {
            Self::Heap(bytes.into())
        }
    }

    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Inline(n, b) => &b[..*n as usize],
            Self::Heap(b) => b,
        }
    }

    fn heap_bytes(&self) -> usize {
        match self {
            Self::Inline(..) => 0,
            Self::Heap(b) => b.len(),
        }
    }
}

pub struct Entry {
    pub coin: CoinBuf,
    pub flags: u8,
}

impl Entry {
    /// A spent coin the engine still holds: written as a delete at the next flush.
    const SPENT: Self = Self {
        coin: CoinBuf::EMPTY,
        flags: DIRTY | SPENT,
    };
}

/// Txids are uniformly random: the first 8 bytes mixed with the vout are a good hash.
#[derive(Default)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        // `OutKey` hashes as one 36-byte slice (plus a length prefix, which is constant).
        if bytes.len() == 36 {
            let a = u64::from_le_bytes(bytes[..8].try_into().expect("8 bytes"));
            let vout = u32::from_le_bytes(bytes[32..].try_into().expect("4 bytes"));
            self.0 = (a ^ u64::from(vout).wrapping_mul(0x9e37_79b9_7f4a_7c15))
                .wrapping_mul(0xbf58_476d_1ce4_e5b9);
        }
    }

    fn write_usize(&mut self, _: usize) {}
}

pub struct Cache {
    pub map: HashMap<OutKey, Entry, BuildHasherDefault<KeyHasher>>,
    heap_bytes: usize,
    budget: usize,
    capacity: usize,
}

/// Whether the cache served a spend.
#[derive(Debug)]
pub enum SpendResult {
    Hit,
    /// Not cached: the caller reads the coin from the engine, then calls `insert_spent`.
    Miss,
}

/// Whether a coin (Core's `Coin` bytes) is a coinbase output: the low bit of its first VARINT.
fn is_coinbase(coin: &[u8]) -> bool {
    let mut input = coin;
    dashd_store::codec::read_varint_u64(&mut input).is_ok_and(|code| code & 1 == 1)
}

impl Cache {
    /// A cache with a coins budget of `budget` accounted bytes, pre-allocated for
    /// `min(budget / ENTRY_COST, max_entries)` entries so it never resizes.
    pub fn new(budget: usize, max_entries: usize) -> Self {
        let capacity = (budget / ENTRY_COST).min(max_entries).max(1024);
        Self {
            map: HashMap::with_capacity_and_hasher(capacity, Default::default()),
            heap_bytes: 0,
            budget,
            capacity,
        }
    }

    /// Bytes of the table allocation (buckets + control bytes), as hashbrown sizes it.
    pub fn table_bytes(&self) -> usize {
        let buckets = (self.map.capacity() * 8 / 7).next_power_of_two();
        buckets * (std::mem::size_of::<(OutKey, Entry)>() + 1)
    }

    pub fn accounted(&self) -> usize {
        self.map.len() * ENTRY_COST + self.heap_bytes
    }

    /// Core's LARGE state, `max(0.9 · budget, budget − 10 MiB)`, or the table is full.
    // Core: validation.cpp Chainstate::GetCoinsCacheSizeState
    pub fn needs_flush(&self) -> bool {
        let large = (self.budget / 10 * 9).max(self.budget.saturating_sub(MAX_BLOCK_COINSDB_USAGE));
        self.accounted() > large || self.map.len() >= self.capacity
    }

    pub fn spend(&mut self, key: &OutKey) -> Result<SpendResult, String> {
        let Some(e) = self.map.get_mut(key) else {
            return Ok(SpendResult::Miss);
        };
        if e.flags & SPENT != 0 {
            return Err("double spend".into());
        }
        self.heap_bytes -= e.coin.heap_bytes();
        if e.flags & FRESH != 0 {
            self.map.remove(key);
        } else {
            *e = Entry::SPENT;
        }
        Ok(SpendResult::Hit)
    }

    /// Records a coin read from the engine and spent (Core's `FetchCoin` + `SpendCoin`).
    pub fn insert_spent(&mut self, key: OutKey) {
        self.map.insert(key, Entry::SPENT);
    }

    /// Core's `AddCoin`: a coin is FRESH unless an engine copy may exist, which is the case for a
    /// spent entry still to be deleted and for every coinbase output (`possible_overwrite`).
    // Core: coins.cpp CCoinsViewCache::AddCoin, AddCoins
    pub fn create(&mut self, key: OutKey, coin: &[u8]) -> Result<(), String> {
        let coinbase = is_coinbase(coin);
        let coin = CoinBuf::new(coin);
        self.heap_bytes += coin.heap_bytes();
        match self.map.get_mut(&key) {
            Some(e) if e.flags & SPENT != 0 => {
                e.coin = coin;
                e.flags = DIRTY;
            }
            Some(_) => return Err("create overwrites an unspent coin".into()),
            None => {
                let flags = if coinbase {
                    DIRTY
                } else {
                    DIRTY | FRESH
                };
                self.map.insert(
                    key,
                    Entry {
                        coin,
                        flags,
                    },
                );
            }
        }
        Ok(())
    }

    /// Empties the cache (after a flush), keeping the allocation.
    pub fn clear(&mut self) {
        self.map.clear();
        self.heap_bytes = 0;
    }
}

/// The engine key: `txid ‖ VARINT(vout)` (A1 §2.9 `coins` CF).
pub fn engine_key(key: &OutKey) -> KeyBuf {
    let mut buf = [0u8; 37];
    buf[..32].copy_from_slice(&key[..32]);
    let mut vout = Vec::with_capacity(5);
    dashd_store::codec::write_varint(
        &mut vout,
        u64::from(u32::from_le_bytes(key[32..].try_into().expect("4 bytes"))),
    );
    buf[32..32 + vout.len()].copy_from_slice(&vout);
    KeyBuf::new(&buf[..32 + vout.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> OutKey {
        let mut k = [0u8; 36];
        k[0] = n;
        k
    }

    #[test]
    fn fresh_coins_never_reach_the_engine() {
        let mut c = Cache::new(1 << 20, usize::MAX);
        c.create(key(1), &[0x02, 2, 3]).unwrap(); // height 1, not coinbase
        assert!(matches!(c.spend(&key(1)).unwrap(), SpendResult::Hit));
        assert!(c.map.is_empty());
        assert!(matches!(c.spend(&key(2)).unwrap(), SpendResult::Miss));
        c.insert_spent(key(2));
        assert_eq!(c.map[&key(2)].flags, DIRTY | SPENT);
        assert!(c.spend(&key(2)).is_err());
        c.create(key(2), &[2; 40]).unwrap();
        assert_eq!(c.map[&key(2)].flags, DIRTY);
        assert_eq!(c.accounted(), ENTRY_COST + 40);
        assert!(matches!(c.spend(&key(2)).unwrap(), SpendResult::Hit));
        assert_eq!(c.map[&key(2)].flags, DIRTY | SPENT);
        assert_eq!(c.accounted(), ENTRY_COST);
        assert!(c.create(key(3), &[2]).is_ok() && c.create(key(3), &[2]).is_err());
    }

    #[test]
    fn coinbase_coins_are_not_fresh() {
        let mut c = Cache::new(1 << 20, usize::MAX);
        c.create(key(1), &[0x03, 9]).unwrap(); // height 1, coinbase
        assert_eq!(c.map[&key(1)].flags, DIRTY);
        c.spend(&key(1)).unwrap();
        assert_eq!(c.map[&key(1)].flags, DIRTY | SPENT, "a coinbase spend leaves a delete");
    }

    #[test]
    fn flush_threshold_is_cores() {
        let small = Cache::new(40 << 20, usize::MAX);
        let large = Cache::new(4000 << 20, usize::MAX);
        let threshold = |c: &Cache| (c.budget / 10 * 9).max(c.budget.saturating_sub(10 << 20));
        assert_eq!(threshold(&small), (40 << 20) / 10 * 9);
        assert_eq!(threshold(&large), (4000 << 20) - (10 << 20));
    }

    #[test]
    fn engine_key_layout() {
        let mut k = key(7);
        k[32..].copy_from_slice(&300u32.to_le_bytes());
        let out = engine_key(&k);
        assert_eq!(&out.as_slice()[..32], &k[..32]);
        assert_eq!(&out.as_slice()[32..], &[0x81, 0x2c]); // Core VARINT(300)
    }
}
