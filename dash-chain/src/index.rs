//! The hash index (SB4 §3.5): block hash → id, append-only in the node, hashing through the
//! arena so the table stores ids only.

use dash_consensus::BlockId;
use dash_primitives::BlockHash;
use hashbrown::HashTable;
use parking_lot::RwLock;

use crate::arena::Arena;
use crate::residency::Residency;

/// Core's `BlockHasher`: `ReadLE64` of the hash's first eight bytes, unsalted. Block hashes are
/// proof-of-work outputs, so their low bytes are uniform.
// Core: util/hasher.h:81-89 BlockHasher
pub(crate) fn cheap_hash(h: &BlockHash) -> u64 {
    let b = h.as_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// `RwLock<HashTable<BlockId>>`: the writer takes the write lock once per insert, readers the
/// read lock once per lookup (R1). It is a leaf lock (§13.1).
#[derive(Default)]
pub(crate) struct HashIndex(RwLock<HashTable<BlockId>>);

impl HashIndex {
    /// Core `LookupBlockIndex` over resident entries.
    pub(crate) fn lookup<R: Residency>(&self, arena: &Arena<R>, h: &BlockHash) -> Option<BlockId> {
        let table = self.0.read();
        table.find(cheap_hash(h), |&id| arena.imm(id, |e| e.hash == *h).unwrap_or(false)).copied()
    }

    /// Publishes `id` under its hash. The writer only, after it set the entry (§13.1), and only
    /// for a hash not in the index.
    pub(crate) fn insert<R: Residency>(&self, arena: &Arena<R>, id: BlockId) {
        let hash_of = |id: &BlockId| {
            arena.imm(*id, |e| cheap_hash(&e.hash)).expect("indexed ids are resident")
        };
        let mut table = self.0.write();
        let h = hash_of(&id);
        debug_assert!(
            table.find(h, |&x| arena.imm(x, |e| e.hash) == arena.imm(id, |e| e.hash)).is_none(),
            "hash indexed twice"
        );
        table.insert_unique(h, id, hash_of);
    }

    /// Unpublishes `id` (SPV eviction, §13.3). The writer only, while `id` is still resident.
    pub(crate) fn remove<R: Residency>(&self, arena: &Arena<R>, id: BlockId) {
        let h = arena.imm(id, |e| cheap_hash(&e.hash)).expect("an indexed id is resident");
        if let Ok(e) = self.0.write().find_entry(h, |&x| x == id) {
            e.remove();
        }
    }

    /// Room for `n` more entries without a rehash (`load` presizes to the record count).
    pub(crate) fn reserve<R: Residency>(&self, arena: &Arena<R>, n: usize) {
        self.0.write().reserve(n, |id| {
            arena.imm(*id, |e| cheap_hash(&e.hash)).expect("indexed ids are resident")
        });
    }

    #[cfg(all(test, not(loom)))]
    pub(crate) fn len(&self) -> usize {
        self.0.read().len()
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use dash_primitives::BlockHeader;
    use dash_types::Numeric as _;

    use super::*;
    use crate::Work;
    use crate::chain::MutableGroup;
    use crate::entry::EntryImm;
    use crate::residency::Permanent;

    fn hash(k: u32) -> BlockHash {
        let mut b = [0u8; 32];
        // Equal low 64 bits for k and k + 2^16: those pairs collide in `cheap_hash`.
        b[..2].copy_from_slice(&(k as u16).to_le_bytes());
        b[8..12].copy_from_slice(&k.to_le_bytes());
        BlockHash::from(b)
    }

    #[test]
    fn finds_every_inserted_hash_including_cheap_hash_collisions() {
        let arena = Arena::<Permanent>::new();
        let index = HashIndex::default();
        let n = 70_000u32;
        index.reserve(&arena, 1000);
        for k in 0..n {
            let imm = EntryImm {
                hash: hash(k),
                header: BlockHeader::default(),
                prev: None,
                skip: None,
                height: k,
                chain_work: Work::ZERO,
                mtp: 0,
                time_max: 0,
                context: false,
            };
            arena.set(BlockId(k), imm, &MutableGroup::default());
            index.insert(&arena, BlockId(k));
        }
        assert_eq!(index.len(), n as usize);
        assert_eq!(cheap_hash(&hash(1)), cheap_hash(&hash(1 + (1 << 16))));
        for k in (0..n).step_by(7) {
            assert_eq!(index.lookup(&arena, &hash(k)), Some(BlockId(k)));
        }
        assert_eq!(index.lookup(&arena, &hash(n)), None);
    }
}
