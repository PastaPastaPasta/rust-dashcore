//! Arena entries (SB4 §3.3) and the per-entry seqlock over the mutable group (§13.2).

use std::sync::OnceLock;

use dash_consensus::BlockId;
use dash_primitives::{BlockHash, BlockHeader};

#[cfg(loom)]
use loom::{
    hint::spin_loop,
    sync::atomic::{AtomicI32, AtomicU32, Ordering, fence},
};
#[cfg(not(loom))]
use std::{
    hint::spin_loop,
    sync::atomic::{AtomicI32, AtomicU32, Ordering, fence},
};

use crate::Work;
use crate::chain::MutableGroup;
use crate::status::Status;

/// The `mtp` of an entry whose eleven times are not all held: the lowest ten anchor-pack
/// entries, and a cold record (records carry no `mtp`; SB4 §8.3).
pub const MTP_UNKNOWN: i64 = i64::MIN;

/// The fields fixed when the entry is inserted (R2). Readers read them lock-free once they hold
/// the id: `imm` is set before the id is published (§13.1).
#[derive(Clone, Debug)]
pub(crate) struct EntryImm {
    pub(crate) hash: BlockHash,
    pub(crate) header: BlockHeader,
    pub(crate) prev: Option<BlockId>,
    /// Core's `pskip` (`BuildSkip`, `chain.cpp:126-130`).
    pub(crate) skip: Option<BlockId>,
    pub(crate) height: u32,
    /// Absolute `nChainWork` (SB0 §4.3).
    pub(crate) chain_work: Work,
    /// `GetMedianTimePast()` at insertion.
    pub(crate) mtp: i64,
    /// Core's `nTimeMax` (`blockstorage.cpp:109`).
    pub(crate) time_max: u32,
    /// An SPV anchor-pack context entry; always false in the node.
    pub(crate) context: bool,
}

/// One arena slot: the immutable fields, set once, and the seqlocked mutable group.
#[derive(Default)]
pub(crate) struct Entry {
    pub(crate) imm: OnceLock<EntryImm>,
    pub(crate) group: SeqGroup,
}

/// The mutable group under a fence-based seqlock (§13.2): one writer, any number of readers,
/// data-race-free in the Rust memory model because every field is an atomic.
pub(crate) struct SeqGroup {
    /// Even when stable, odd while the writer stores.
    seq: AtomicU32,
    status: AtomicU32,
    n_tx: AtomicU32,
    /// Core's `nFile`; -1 is none.
    file: AtomicI32,
    data_pos: AtomicU32,
    undo_pos: AtomicU32,
}

impl Default for SeqGroup {
    fn default() -> SeqGroup {
        SeqGroup::new(&MutableGroup::default())
    }
}

impl SeqGroup {
    pub(crate) fn new(g: &MutableGroup) -> SeqGroup {
        SeqGroup {
            seq: AtomicU32::new(0),
            status: AtomicU32::new(g.status.bits()),
            n_tx: AtomicU32::new(g.n_tx),
            file: AtomicI32::new(g.file.unwrap_or(-1)),
            data_pos: AtomicU32::new(g.data_pos),
            undo_pos: AtomicU32::new(g.undo_pos),
        }
    }

    /// Stores `g`. The single writer only: two concurrent writers corrupt the sequence.
    pub(crate) fn write(&self, g: &MutableGroup) {
        let s = self.seq.load(Ordering::Relaxed);
        debug_assert!(s.is_multiple_of(2), "a seqlock write is already in progress");
        self.seq.store(s.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.status.store(g.status.bits(), Ordering::Relaxed);
        self.n_tx.store(g.n_tx, Ordering::Relaxed);
        self.file.store(g.file.unwrap_or(-1), Ordering::Relaxed);
        self.data_pos.store(g.data_pos, Ordering::Relaxed);
        self.undo_pos.store(g.undo_pos, Ordering::Relaxed);
        self.seq.store(s.wrapping_add(2), Ordering::Release);
    }

    /// One consistent read of the group, retried while a write overlaps it.
    pub(crate) fn read(&self) -> MutableGroup {
        loop {
            if let Some(g) = self.try_read() {
                return g;
            }
            spin_loop();
        }
    }

    /// One read attempt: `None` when a write overlapped it.
    fn try_read(&self) -> Option<MutableGroup> {
        let s1 = self.seq.load(Ordering::Acquire);
        if s1 % 2 == 1 {
            return None;
        }
        let g = self.load_fields();
        fence(Ordering::Acquire);
        (self.seq.load(Ordering::Relaxed) == s1).then_some(g)
    }

    /// The writer's own read: no write can overlap it, so it needs no retry.
    pub(crate) fn read_own(&self) -> MutableGroup {
        self.load_fields()
    }

    fn load_fields(&self) -> MutableGroup {
        let file = self.file.load(Ordering::Relaxed);
        MutableGroup {
            status: Status(self.status.load(Ordering::Relaxed)),
            n_tx: self.n_tx.load(Ordering::Relaxed),
            file: (file >= 0).then_some(file),
            data_pos: self.data_pos.load(Ordering::Relaxed),
            undo_pos: self.undo_pos.load(Ordering::Relaxed),
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Barrier};

    use super::*;

    /// A group whose fields are all derived from `k`, so a torn read shows as a mismatch.
    fn group(k: u32) -> MutableGroup {
        MutableGroup {
            status: Status(k),
            n_tx: k.wrapping_mul(3),
            file: Some((k % 1000) as i32),
            data_pos: k ^ 0xa5a5_a5a5,
            undo_pos: !k,
        }
    }

    #[test]
    fn round_trips_and_none_file() {
        let g = SeqGroup::default();
        assert_eq!(g.read(), MutableGroup::default());
        assert_eq!(g.read().file, None);
        g.write(&group(7));
        assert_eq!(g.read(), group(7));
        assert_eq!(g.read_own(), group(7));
    }

    /// Readers on other threads never see a mix of two writes (§13.2's stress test). Run it
    /// with `--release` for real contention.
    #[test]
    fn readers_never_see_a_torn_group() {
        let g = Arc::new(SeqGroup::new(&group(0)));
        let done = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicU64::new(0));
        let start = Arc::new(Barrier::new(4));
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let (g, done, reads, start) =
                    (g.clone(), done.clone(), reads.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    let mut last = 0;
                    while !done.load(Ordering::Relaxed) {
                        let r = g.read();
                        assert_eq!(r, group(r.status.bits()), "torn read");
                        assert!(r.status.bits() >= last, "a read went back in time");
                        last = r.status.bits();
                        reads.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        start.wait();
        // At least 200,000 writes, and on until the readers overlapped them (a loaded box can
        // park a reader for a while).
        let mut k = 0;
        while k < 200_000 || (reads.load(Ordering::Relaxed) < 100_000 && k < 50_000_000) {
            k += 1;
            g.write(&group(k));
        }
        done.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().expect("reader");
        }
        assert!(reads.load(Ordering::Relaxed) > 0);
        assert_eq!(g.read(), group(k));
    }
}

#[cfg(all(test, loom))]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    use super::*;

    fn group(k: u32) -> MutableGroup {
        MutableGroup {
            status: Status(k),
            n_tx: k + 100,
            file: Some(k as i32),
            data_pos: k + 200,
            undo_pos: k + 300,
        }
    }

    /// Every accepted read attempt is a group the writer stored, never a mix. The model checks
    /// single attempts: loom has no fairness, so a retry loop could re-read a stale sequence
    /// forever, and `read` only repeats `try_read`. Each relaxed field load may return either
    /// stored value, so the model keeps to one write and two preemptions to finish in minutes;
    /// the release stress test covers long write sequences.
    #[test]
    fn loom_one_writer_two_readers() {
        let mut model = loom::model::Builder::new();
        model.preemption_bound = Some(2);
        model.check(|| {
            let g = Arc::new(SeqGroup::new(&group(0)));
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    let g = g.clone();
                    thread::spawn(move || {
                        if let Some(r) = g.try_read() {
                            assert!(r == group(0) || r == group(1), "torn read {r:?}");
                        }
                    })
                })
                .collect();
            g.write(&group(1));
            for r in readers {
                r.join().unwrap();
            }
            assert_eq!(g.try_read(), Some(group(1)));
        });
    }
}
