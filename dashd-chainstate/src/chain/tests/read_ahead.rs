//! CH4-4: the read-ahead's own rules, on a rig's index and files, with V's side driven by the
//! test: a block whose position has changed since its read is V's to read, the queue's byte
//! bound, a read finished after the epoch moved on, and a plan off the path the reader holds.

use std::sync::{Arc, Barrier};

use dash_chain::{EntryView, FlatFilePos};
use dash_consensus::Block;
use dash_primitives::BlockHash;

use super::support::{Mined, Rig, RigConfig, SharedFiles, branch};
use crate::chain::read_ahead::{Counts, Probe, ReadAhead};
use crate::chain::{BlockSource, ReadAheadSetup};

/// A rig whose files hold genesis and `n` blocks on it, connected.
fn chain(n: u32) -> (Rig, SharedFiles, Vec<Mined>) {
    let files = SharedFiles::default();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(files.clone())),
        ..RigConfig::default()
    });
    let blocks = branch(&rig.p, &rig.genesis, n, 0);
    for m in &blocks {
        rig.accept(m);
    }
    (rig, files, blocks)
}

/// A read-ahead over `source` with `probe`, whose bounds are `depth` blocks and `bytes`, planned
/// from height 1 to the last of `blocks`.
fn read_ahead(
    rig: &Rig,
    blocks: &[Mined],
    source: impl BlockSource + 'static,
    probe: &Arc<Probe>,
    (depth, bytes): (u32, u64),
) -> ReadAhead {
    let setup = ReadAheadSetup::threaded(false).with_bounds(depth, bytes).probed(probe);
    let mut ra = ReadAhead::new(setup, Some(Arc::new(source)), &rig.p);
    let last = blocks.last().expect("blocks");
    ra.plan(rig.v.reader(), 1, rig.id(last), last.height);
    ra
}

fn entry(rig: &Rig, m: &Mined) -> EntryView {
    rig.v.reader().entry(rig.id(m)).expect("indexed")
}

/// A block whose position is no longer the one read is V's to read (the entry must match in
/// hash and position, PLAN-CH4 §CH4-4).
#[test]
fn a_block_whose_position_moved_is_read_by_v() {
    let (rig, files, blocks) = chain(3);
    let probe = Probe::settled();
    let ra = read_ahead(&rig, &blocks, files, &probe, (64, 1 << 20));
    assert!(ra.take(&entry(&rig, &blocks[0])).is_some(), "block 1 as read");
    let mut moved = entry(&rig, &blocks[1]);
    moved.data_pos += 1;
    assert!(ra.take(&moved).is_none(), "block 2 moved");
    assert!(ra.take(&entry(&rig, &blocks[2])).is_some(), "block 3 as read");
    let c = probe.counts();
    assert_eq!((c.reads, c.hits, c.fallbacks), (3, 2, 1));
}

/// The reader starts no read while its queue holds the byte bound: one block at a time here.
#[test]
fn the_reader_holds_at_its_byte_bound() {
    let (rig, files, blocks) = chain(5);
    let probe = Probe::settled();
    let ra = read_ahead(&rig, &blocks, files, &probe, (64, 1));
    for m in &blocks {
        let block = ra.take(&entry(&rig, m)).expect("read ahead");
        assert_eq!(block.hash(), m.hash());
    }
    let c = probe.counts();
    assert_eq!((c.reads, c.hits, c.max_ready), (5, 5, 1), "{c:?}");
}

/// The source, which holds its read of `hold` until the test lets it go.
struct Held {
    files: SharedFiles,
    hold: BlockHash,
    /// Met once the read of `hold` has started, and again to let it finish.
    gate: Arc<Barrier>,
}

impl BlockSource for Held {
    fn read_block(&self, pos: FlatFilePos, hash: &BlockHash) -> Result<Arc<Block>, String> {
        if *hash == self.hold {
            self.gate.wait();
            self.gate.wait();
        }
        BlockSource::read_block(&self.files, pos, hash)
    }
}

/// A read finished after the epoch moved on is dropped: V reads the block itself.
#[test]
fn a_read_finished_after_the_epoch_moved_is_dropped() {
    let (rig, files, blocks) = chain(2);
    let gate = Arc::new(Barrier::new(2));
    let held = Held {
        files,
        hold: blocks[0].hash(),
        gate: gate.clone(),
    };
    let probe = Arc::new(Probe::default());
    let mut ra = read_ahead(&rig, &blocks, held, &probe, (1, 1 << 20));
    // The reader is in block 1's read.
    gate.wait();
    ra.advance_epoch();
    gate.wait();
    // Its read is dropped; it then waits for work, and V reads block 1 itself.
    ra.settle();
    assert!(ra.take(&entry(&rig, &blocks[0])).is_none());
    assert_eq!(
        probe.counts(),
        Counts {
            reads: 1,
            fallbacks: 1,
            epochs: 1,
            ..Counts::default()
        }
    );
}

/// A plan toward a most-work block off the path the reader holds moves the epoch on: with its
/// queue full of the old path's blocks, a branch off V's tip is read ahead all the same.
#[test]
fn a_plan_off_the_path_held_moves_the_epoch() {
    let (rig, files, old) = chain(3);
    let side = branch(&rig.p, &rig.genesis, 4, 1);
    for m in &side {
        rig.accept(m);
    }
    let probe = Probe::settled();
    let mut ra = read_ahead(&rig, &old, files, &probe, (3, 1 << 20));
    ra.settle();
    assert_eq!(probe.counts().max_ready, 3, "the old path fills the queue");
    let tip = side.last().expect("a branch");
    for m in &side {
        // V's connect: the plan, then the take (`read_to_connect`).
        ra.plan(rig.v.reader(), m.height, rig.id(tip), tip.height);
        let block = ra.take(&entry(&rig, m)).expect("read ahead");
        assert_eq!(block.hash(), m.hash());
    }
    let c = probe.counts();
    assert_eq!((c.reads, c.hits, c.fallbacks, c.epochs), (7, 4, 0, 1), "{c:?}");
}

/// The block V holds (`pblock`) is not read ahead: a reorg's last block arrives with it, and the
/// reader reads only the branch's blocks before it, which V connects from disk.
#[test]
fn the_block_v_holds_is_not_read_ahead() {
    let probe = Probe::settled();
    let rig = Rig::with(RigConfig {
        files: Some(Box::new(SharedFiles::default())),
        read_ahead: ReadAheadSetup::threaded(false).probed(&probe),
        ..RigConfig::default()
    });
    for m in &branch(&rig.p, &rig.genesis, 2, 0) {
        rig.accept(m);
    }
    // Genesis, which V's startup connected from disk.
    let before = probe.counts();
    let side = branch(&rig.p, &rig.genesis, 3, 1);
    for m in &side {
        rig.accept(m);
    }
    assert_eq!(super::failure::tip(&rig), rig.id(&side[2]));
    let c = probe.counts();
    assert_eq!((before.reads, before.hits), (1, 1), "{before:?}");
    assert_eq!((c.reads, c.hits, c.fallbacks), (3, 3, 0), "{c:?}");
}
