//! The entry arena (SB4 §3.4): a two-level directory over `u32` ids, 1,024 entries per chunk.

use std::marker::PhantomData;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use arc_swap::ArcSwapOption;
use dash_consensus::BlockId;

use crate::chain::MutableGroup;
use crate::entry::{Entry, EntryImm};
use crate::residency::Residency;

/// Entries per chunk.
pub(crate) const CHUNK: u32 = 1 << 10;
/// Chunk slots per second-level table.
pub(crate) const L2: u32 = 1 << 10;
/// First-level slots: `L1 × L2 × CHUNK = 2^32`.
const L1: u32 = 1 << 12;

/// 1,024 entries, allocated together and, in `Evicting`, released together.
pub(crate) struct Chunk {
    entries: Box<[Entry]>,
    /// `Evicting`: a bit per entry that has left the arena while its chunk stays (SB0 §4.4).
    evicted: [AtomicU64; (CHUNK / 64) as usize],
}

impl Chunk {
    fn new() -> Chunk {
        Chunk {
            entries: (0..CHUNK).map(|_| Entry::default()).collect(),
            evicted: Default::default(),
        }
    }

    fn is_evicted(&self, i: usize) -> bool {
        self.evicted[i / 64].load(Ordering::Acquire) & (1 << (i % 64)) != 0
    }
}

/// The chunk that holds `id`.
pub(crate) fn chunk_of(id: BlockId) -> u32 {
    id.0 / CHUNK
}

/// One second-level table of `L2` chunk slots. `Permanent` slots hold their chunk inline, set
/// once and borrowed for the arena's lifetime. `Evicting` slots can be emptied, and readers hold
/// the chunk while they read it. An `Evicting` slot is one pointer rather than the size of an
/// inline chunk: its tables outlive their chunks, 8 KiB per 2^20 ids (A-31).
enum Table {
    Permanent(Box<[OnceLock<Chunk>]>),
    Evicting(Box<[ArcSwapOption<Chunk>]>),
}

/// The arena. The single writer installs chunks and sets entries; readers resolve published ids
/// without a lock (§13.1).
pub(crate) struct Arena<R: Residency> {
    dir: Box<[OnceLock<Table>]>,
    _residency: PhantomData<R>,
}

fn split(id: BlockId) -> (usize, usize, usize) {
    let id = id.0;
    ((id / (L2 * CHUNK)) as usize, (id / CHUNK % L2) as usize, (id % CHUNK) as usize)
}

impl<R: Residency> Arena<R> {
    pub(crate) fn new() -> Arena<R> {
        Arena {
            dir: (0..L1).map(|_| OnceLock::new()).collect(),
            _residency: PhantomData,
        }
    }

    /// `id`'s table, its slot in it and its index in the chunk.
    fn slot(&self, id: BlockId) -> Option<(&Table, usize, usize)> {
        let (l1, l2, i) = split(id);
        self.dir[l1].get().map(|t| (t, l2, i))
    }

    /// An `Evicting` arena's slot for `id`.
    fn evicting(&self, id: BlockId) -> (&ArcSwapOption<Chunk>, usize) {
        match self.slot(id).expect("an id that was set") {
            (Table::Evicting(slots), l2, i) => (&slots[l2], i),
            (Table::Permanent(_), ..) => unreachable!("only Evicting arenas evict"),
        }
    }

    /// Runs `f` on the entry `id` when it is set and resident.
    pub(crate) fn with<T>(&self, id: BlockId, f: impl FnOnce(&EntryImm, &Entry) -> T) -> Option<T> {
        let (table, l2, i) = self.slot(id)?;
        match table {
            Table::Permanent(slots) => {
                let e = &slots[l2].get()?.entries[i];
                Some(f(e.imm.get()?, e))
            }
            Table::Evicting(slots) => {
                let chunk = slots[l2].load();
                let chunk = chunk.as_ref().filter(|c| !c.is_evicted(i))?;
                let e = &chunk.entries[i];
                Some(f(e.imm.get()?, e))
            }
        }
    }

    /// Copies a field of the immutable part of `id`.
    pub(crate) fn imm<T>(&self, id: BlockId, f: impl FnOnce(&EntryImm) -> T) -> Option<T> {
        self.with(id, |imm, _| f(imm))
    }

    /// One seqlocked read of `id`'s mutable group (R2).
    pub(crate) fn group(&self, id: BlockId) -> Option<MutableGroup> {
        self.with(id, |_, e| e.group.read())
    }

    /// Sets the entry `id`: its group, then its immutable part. The writer only, once per id,
    /// before it publishes the id (§13.1).
    pub(crate) fn set(&self, id: BlockId, imm: EntryImm, group: &MutableGroup) {
        let (l1, l2, i) = split(id);
        let table = self.dir[l1].get_or_init(|| match R::EVICTS {
            false => Table::Permanent((0..L2).map(|_| OnceLock::new()).collect()),
            true => Table::Evicting((0..L2).map(|_| ArcSwapOption::empty()).collect()),
        });
        let store = |e: &Entry| {
            e.group.write(group);
            assert!(e.imm.set(imm).is_ok(), "arena entry {} set twice", id.0);
        };
        match table {
            Table::Permanent(slots) => store(&slots[l2].get_or_init(Chunk::new).entries[i]),
            Table::Evicting(slots) => {
                let chunk = &slots[l2];
                if chunk.load().is_none() {
                    chunk.store(Some(Arc::new(Chunk::new())));
                }
                store(&chunk.load().as_ref().expect("installed above").entries[i]);
            }
        }
    }

    /// `Evicting`: `id` leaves the arena (SB0 §4.4). Readers that already hold its chunk keep
    /// reading it; later reads resolve it cold. The writer only, after it unindexed the hash.
    pub(crate) fn evict(&self, id: BlockId) {
        let (chunk, i) = self.evicting(id);
        if let Some(chunk) = chunk.load().as_ref() {
            chunk.evicted[i / 64].fetch_or(1 << (i % 64), Ordering::Release);
        }
    }

    /// `Evicting`: releases chunk `c` once every entry set in it has been evicted. A later
    /// `set` in it installs a fresh chunk.
    pub(crate) fn release(&self, c: u32) {
        let (slot, _) = self.evicting(BlockId(c * CHUNK));
        slot.store(None);
    }

    /// Writes `id`'s mutable group under its seqlock. The writer only.
    pub(crate) fn write_group(&self, id: BlockId, group: &MutableGroup) -> bool {
        self.with(id, |_, e| e.group.write(group)).is_some()
    }

    /// The writer's own read of `id`'s group (no retry: no write can overlap it).
    pub(crate) fn own_group(&self, id: BlockId) -> Option<MutableGroup> {
        self.with(id, |_, e| e.group.read_own())
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use dash_primitives::{BlockHash, BlockHeader};
    use dash_types::Numeric as _;

    use super::*;
    use crate::Work;
    use crate::residency::{Evicting, Permanent};
    use crate::status::Status;

    fn imm(height: u32) -> EntryImm {
        EntryImm {
            hash: BlockHash::from([height as u8; 32]),
            header: BlockHeader::default(),
            prev: None,
            skip: None,
            height,
            chain_work: Work::ZERO,
            mtp: 0,
            time_max: 0,
            context: false,
        }
    }

    fn check<R: Residency>() {
        let arena = Arena::<R>::new();
        // Ids at chunk, table and directory boundaries, and the last id.
        let ids = [0, 1, CHUNK - 1, CHUNK, L2 * CHUNK - 1, L2 * CHUNK, u32::MAX];
        for (k, &id) in ids.iter().enumerate() {
            assert!(arena.imm(BlockId(id), |i| i.height).is_none());
            let g = MutableGroup {
                status: Status(Status::VALID_TREE),
                n_tx: k as u32,
                ..MutableGroup::default()
            };
            arena.set(BlockId(id), imm(k as u32), &g);
        }
        for (k, &id) in ids.iter().enumerate() {
            assert_eq!(arena.imm(BlockId(id), |i| i.height), Some(k as u32));
            assert_eq!(arena.group(BlockId(id)).map(|g| g.n_tx), Some(k as u32));
        }
        // A set chunk's other entries are not set.
        assert!(arena.imm(BlockId(2), |i| i.height).is_none());
        assert!(arena.write_group(BlockId(1), &MutableGroup::default()));
        assert_eq!(arena.own_group(BlockId(1)), Some(MutableGroup::default()));
        assert!(!arena.write_group(BlockId(2), &MutableGroup::default()));
    }

    #[test]
    fn permanent_resolves_ids_at_every_boundary() {
        check::<Permanent>();
    }

    #[test]
    fn evicting_resolves_ids_at_every_boundary() {
        check::<Evicting>();
    }

    /// SB0 §4.4: a released chunk's slot is empty, so its memory goes (A-31); an evicted entry
    /// stays unreadable, and a later `set` in the chunk installs a fresh one.
    #[test]
    fn release_empties_the_chunk_slot() {
        let arena = Arena::<Evicting>::new();
        let installed = |id: u32| arena.evicting(BlockId(id)).0.load().is_some();
        for id in [CHUNK, CHUNK + 1] {
            arena.set(BlockId(id), imm(id), &MutableGroup::default());
        }
        arena.evict(BlockId(CHUNK));
        arena.evict(BlockId(CHUNK + 1));
        assert!(installed(CHUNK), "evicted entries alone keep the chunk");
        assert!(arena.imm(BlockId(CHUNK), |i| i.height).is_none());
        arena.release(chunk_of(BlockId(CHUNK)));
        assert!(!installed(CHUNK), "released");
        assert!(arena.imm(BlockId(CHUNK + 1), |i| i.height).is_none());
        arena.set(BlockId(CHUNK + 2), imm(7), &MutableGroup::default());
        assert!(installed(CHUNK));
        assert_eq!(arena.imm(BlockId(CHUNK + 2), |i| i.height), Some(7));
        assert!(arena.imm(BlockId(CHUNK), |i| i.height).is_none(), "a fresh chunk");
    }

    #[test]
    #[should_panic(expected = "set twice")]
    fn an_entry_is_set_once() {
        let arena = Arena::<Permanent>::new();
        arena.set(BlockId(5), imm(5), &MutableGroup::default());
        arena.set(BlockId(5), imm(5), &MutableGroup::default());
    }
}
