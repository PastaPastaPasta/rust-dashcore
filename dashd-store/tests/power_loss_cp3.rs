//! CP3.1 regressions (reviews/ST3a-cp3-sol.md): each reproduction of Sol's review, which fails on
//! 546c4626, and the controls that keep each fix honest.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use dashd_store::faulty::{
    Adversary, CrashKind, FileRef, Fixed, InvariantId, Peek, RefExtractor, Requirement, World,
    WorldConfig, WorldDurableView,
};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine};

static SCHEMA: DbSchema = DbSchema {
    name: "probe",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};
const CF: Cf = Cf::new(0);

fn put(w: &World, key: &[u8], value: &[u8], commit: Commit) {
    let e = w.engine("probe", &SCHEMA);
    let mut b = e.batch();
    b.put(CF, key, value);
    e.write(b, commit).unwrap();
}

fn extended() -> WorldConfig {
    WorldConfig {
        adversary: Adversary::Extended,
        ..WorldConfig::default()
    }
}

// ---------------------------------------------------------------------------------------------
// CP3-1: D1 never certifies a range it cannot read back.

/// A durable record `loc` that references `/dd/f[0, 512)`.
struct Loc;
impl RefExtractor for Loc {
    fn refs(&self, _: &str, _: Cf, k: &[u8], _: &[u8], _: &WorldDurableView) -> Vec<Requirement> {
        if k == b"loc" {
            vec![Requirement::Bytes(FileRef::new("/dd/f", 0, 512))]
        } else {
            vec![]
        }
    }
}

/// 512 unsynced bytes in `/dd/f` (its directory synced), a PowerLoss under `Extended` deciding
/// the cells as `choices` says, then a Durable record referencing the range.
fn unreadable_probe(choices: &[(&str, u32)]) -> (World, bool) {
    let w = World::new(extended());
    let fs = w.fs();
    fs.write_all_at(Path::new("/dd/f"), 0, &[7; 512]).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    let choices: BTreeMap<String, u32> = choices.iter().map(|(n, c)| (n.to_string(), *c)).collect();
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(Fixed::new(choices)), extended());
    let readable = w.fs().read_exact_at(Path::new("/dd/f"), 0, &mut [0; 512]).is_ok();
    w.set_extractor(Arc::new(Loc));
    put(&w, b"loc", b"v", Commit::Durable);
    w.evaluate_d1();
    assert!(matches!(w.durable_view().get("probe", CF, b"loc"), Peek::Known(Some(_))));
    (w, readable)
}

fn d1a(w: &World) -> bool {
    w.d1_violations().iter().any(|v| v.invariant == InvariantId::D1a)
}

/// Sol's reproduction: the block decided unreadable reads back as EIO, so a durable reference to
/// it is a D1(a) violation.
#[test]
fn cp3_1_unreadable_range_is_not_durable() {
    let (w, readable) = unreadable_probe(&[("1:fs:i1:eio0", 1)]);
    assert!(!readable, "choice 1 of the block's cell is the unreadable outcome");
    let why = w.durable_view().range_durable(&FileRef::new("/dd/f", 0, 512));
    assert!(d1a(&w), "a durable reference to an unreadable unit was certified ({why:?})");
}

/// Control: the same reference to the unit's surviving bytes is durable, and D1 is silent.
#[test]
fn cp3_1_readable_range_stays_durable() {
    let (w, readable) = unreadable_probe(&[]);
    assert!(readable);
    assert!(!d1a(&w), "{:?}", w.d1_violations());
}

/// While the block's cell is undecided D1 cannot certify the reference either: it holds it
/// pending, and reports it once a read decides the block unreadable.
#[test]
fn cp3_1_undecided_readability_is_unknown() {
    let w = World::new(extended());
    w.fs().write_all_at(Path::new("/dd/f"), 0, &[7; 512]).unwrap();
    w.fs().sync_dir(Path::new("/dd")).unwrap();
    let choices = BTreeMap::from([("1:fs:i1:eio0".to_string(), 1)]);
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(Fixed::new(choices)), extended());
    w.set_extractor(Arc::new(Loc));
    put(&w, b"loc", b"v", Commit::Durable);
    w.evaluate_d1();
    let r = w.durable_view().range_durable(&FileRef::new("/dd/f", 0, 512));
    assert!(
        r.as_ref().is_err_and(|e| e.starts_with("undecided")),
        "an undecided block that may be unreadable is not durable yet: {r:?}"
    );
    assert!(!d1a(&w), "no violation before the cell is decided: {:?}", w.d1_violations());
    assert!(w.fs().read_exact_at(Path::new("/dd/f"), 0, &mut [0; 512]).is_err());
    w.evaluate_d1();
    assert!(d1a(&w), "the decided unreadable block fails the pending reference");
}

// ---------------------------------------------------------------------------------------------
// CP3-4: EIO follows Linux's I/O boundaries (4 KiB filesystem blocks on ext4 and XFS), not the
// adversary's unit, which only chooses where tears fall. Native controls (Linux 7.0.0-38, ext4
// and XFS with 4 KiB blocks, the block's last sector made unreadable): a read anywhere in the
// block and a write covering part of it fail with EIO; a write of the whole block succeeds.

/// `pages` unsynced 4 KiB pages of `0x11` in `/dd/f` (directory synced), then a PowerLoss under
/// `Extended` with `granularity` that makes block `bad` unreadable (choice 1 of its one cell).
fn unreadable_block(granularity: dashd_store::faulty::Granularity, pages: u64, bad: u64) -> World {
    let cfg = || WorldConfig {
        adversary: Adversary::Extended,
        granularity,
        ..WorldConfig::default()
    };
    let w = World::new(cfg());
    w.fs().write_all_at(Path::new("/dd/f"), 0, &vec![0x11; (pages * 4096) as usize]).unwrap();
    w.fs().sync_dir(Path::new("/dd")).unwrap();
    let choices = BTreeMap::from([(format!("1:fs:i1:eio{bad}"), 1)]);
    w.capture().boot(CrashKind::PowerLoss, Box::new(Fixed::new(choices)), cfg())
}

fn read(w: &World, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0; len];
    w.fs()
        .read_exact_at(Path::new("/dd/f"), offset, &mut buf)
        .map(|_| buf)
        .map_err(|e| e.to_string())
}

fn write(w: &World, offset: u64, len: usize) -> Result<(), String> {
    w.fs().write_all_at(Path::new("/dd/f"), offset, &vec![0x22; len]).map_err(|e| e.to_string())
}

/// Sol's reproduction: a 512-byte write at 0 reads the whole block, which is unreadable (natively,
/// through its sector 7).
#[test]
fn cp3_4_subblock_write_reads_the_block() {
    use dashd_store::faulty::Granularity;
    let w = unreadable_block(Granularity::Sector, 1, 0);
    assert!(read(&w, 3584, 512).is_err());
    assert!(write(&w, 0, 512).is_err(), "a sub-block write reads the block first");
}

/// Every boundary of the native control, in sectors and in bytes, then the full-block write that
/// replaces the block and reads back.
#[test]
fn cp3_4_eio_follows_filesystem_blocks() {
    use dashd_store::faulty::Granularity;
    for g in [Granularity::Sector, Granularity::Byte] {
        let w = unreadable_block(g, 1, 0);
        for (offset, len) in [(0, 512), (3584, 512), (0, 4096)] {
            assert!(read(&w, offset, len).is_err(), "{g:?}: pread({offset}, {len})");
        }
        for (offset, len) in [(0, 1), (0, 512), (3584, 512)] {
            assert!(write(&w, offset, len).is_err(), "{g:?}: pwrite({offset}, {len})");
        }
        assert_eq!(write(&w, 0, 4096), Ok(()), "{g:?}: a whole-block write reads nothing");
        assert_eq!(read(&w, 0, 4096), Ok(vec![0x22; 4096]), "{g:?}: the block is repaired");
    }
}

/// A write over two blocks reads only the one it covers partly; the other block is unaffected.
#[test]
fn cp3_4_only_partly_covered_blocks_are_read() {
    use dashd_store::faulty::Granularity;
    let w = unreadable_block(Granularity::Sector, 2, 1);
    assert_eq!(read(&w, 0, 4096), Ok(vec![0x11; 4096]), "block 0 has no unreadable unit");
    assert!(write(&w, 0, 4097).is_err(), "the write covers block 1 only partly");
    assert_eq!(write(&w, 0, 8192), Ok(()));
    assert_eq!(read(&w, 4096, 4096), Ok(vec![0x22; 4096]));
}

/// D1 judges readability by block too: a durable reference to sector 0, whose block a read of
/// sector 7 found unreadable, is not durable.
#[test]
fn cp3_4_d1_judges_readability_by_block() {
    use dashd_store::faulty::Granularity;
    let w = unreadable_block(Granularity::Sector, 1, 0);
    assert!(read(&w, 3584, 512).is_err(), "decides the block unreadable");
    let r = w.durable_view().range_durable(&FileRef::new("/dd/f", 0, 512));
    assert!(r.is_err(), "sector 0 cannot be read back: {r:?}");
}

/// A-PSOW stays granted (I27, I35): an unsynced append never changes durable bytes. Since
/// DEC-148 its torn tail can make their block unavailable (EIO), so every read of them returns
/// EIO or the durable bytes, and D1 still counts them durable. Every branch of the crash.
#[test]
fn cp3_4_append_keeps_the_durable_bytes() {
    use dashd_store::faulty::Dfs;
    let w = World::new(extended());
    let fs = w.fs();
    fs.write_all_at(Path::new("/dd/f"), 0, &[7; 700]).unwrap();
    fs.sync_data(Path::new("/dd/f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    w.set_extractor(Arc::new(Loc));
    put(&w, b"loc", b"v", Commit::Durable);
    w.evaluate_d1();
    assert!(!d1a(&w));
    fs.write_all_at(Path::new("/dd/f"), 700, &[8; 4608 - 700]).unwrap();
    let img = w.capture();
    let dfs = Dfs::new();
    let (mut branches, mut unreadable) = (0, 0);
    loop {
        let w = img.boot(CrashKind::PowerLoss, dfs.decider(), extended());
        let r = read(&w, 0, 700);
        assert!(
            r == Ok(vec![7; 700]) || r.as_ref().is_err_and(|e| e.contains("EIO")),
            "branch {branches}: {r:?}"
        );
        w.evaluate_d1();
        assert!(!d1a(&w), "branch {branches}: {:?}", w.d1_violations());
        let mut tail = [0; 512];
        let r = w.fs().read_exact_at(Path::new("/dd/f"), 4096, &mut tail);
        unreadable += usize::from(r.is_err_and(|e| e.to_string().contains("EIO")));
        branches += 1;
        if !dfs.advance() {
            break;
        }
    }
    assert!(unreadable > 0, "block 1, with nothing durable, can still be unreadable");
    eprintln!("{branches} branches, {unreadable} with block 1 unreadable");
}

/// One cell per block, not per unit: under `Byte` a torn page's readability is one choice, so a
/// 1-byte read has few branches (per-unit cells made it decide all 4,096 units' cells, and the
/// search grew as their product: the nightly's `Byte` witnesses stalled; capped here at 1,000).
#[test]
fn cp3_4_one_eio_cell_per_block() {
    use dashd_store::faulty::{Dfs, Granularity};
    let cfg = || WorldConfig {
        adversary: Adversary::Extended,
        granularity: Granularity::Byte,
        ..WorldConfig::default()
    };
    let w = World::new(cfg());
    w.fs().write_all_at(Path::new("/dd/f"), 0, &[0x11; 4096]).unwrap();
    w.fs().sync_dir(Path::new("/dd")).unwrap();
    let img = w.capture();
    let dfs = Dfs::new();
    let (mut branches, mut eio) = (0, 0);
    loop {
        let w = img.boot(CrashKind::PowerLoss, dfs.decider(), cfg());
        eio += usize::from(read(&w, 0, 1).is_err());
        branches += 1;
        if !dfs.advance() || branches == 1000 {
            break;
        }
    }
    // The size's 9 options (size 0 reads past the end), then the block's cell, then, if the block
    // is readable, the byte's own 3 (the version, the durable zeros, garbage): 1 + 8 * (1 + 3).
    assert_eq!(branches, 33, "branches for a 1-byte read of one page");
    assert!(eio > 0, "the block can still be unreadable");
}

// ---------------------------------------------------------------------------------------------
// CP3-2: R5's prune exemption covers exactly the finalized prune's flag and position resets; a
// block's height, parent and file are checked whether or not its file was pruned.

/// How the probe tampers with block 5 of file 0 while it prunes file 0.
#[derive(Clone, Copy, Debug)]
enum Tamper {
    None,
    Height,
    Parent,
    File,
}

/// Sol's probe: a pruned toy chain of 72 blocks, then a Durable batch that resets every file-0
/// block's data and undo flags and positions (and tampers with block 5 as `tamper` says), after
/// an eligible `PruneSelected` for flush 99. `finalized` logs that flush's finalizer returning,
/// as `ToyChain` does once its Bk or Bp write is Durable. Returns whether R5 fired.
fn prune_probe(tamper: Tamper, finalized: bool) -> bool {
    use dashd_store::KvRead;
    use dashd_store::codec::ValueCodec;
    use dashd_store::faulty::toy::reference::ToyRef;
    use dashd_store::faulty::toy::{
        CHAIN_SCHEMA, FILES, INDEX, PruneConfig, ToyChain, ToyConfig, ToyOp,
    };
    use dashd_store::faulty::{AllSurvive, FlushCause, Mark, PruneDue, Reference, Sut};
    use dashd_store::flatfile::BlockFileInfo;

    let cfg = ToyConfig {
        prune: Some(PruneConfig::default()),
        ..ToyConfig::default()
    };
    let w = World::new(WorldConfig::default());
    w.set_extractor(ToyChain::extractor(&cfg).unwrap());
    let mut t = ToyChain::open(&w, &cfg).unwrap();
    let mut r = ToyRef::new(&cfg);
    for op in [ToyOp::Connect(72), ToyOp::Flush] {
        t.apply(&op).unwrap();
        r.apply(&op);
    }
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
    let t = ToyChain::open(&w, &cfg).unwrap();
    assert!(t.check(&w, &r, &w.trace()).is_ok());
    let e = w.engine("chainstate", &CHAIN_SCHEMA);
    let mut b = e.batch();
    for id in &r.active {
        let mut entry = e.get(INDEX, id).unwrap().unwrap();
        let file = u32::from_le_bytes(entry[37..41].try_into().unwrap());
        if file == 0 {
            entry[36] &= !3;
            entry[41..49].fill(0);
            if id == &r.active[5] {
                match tamper {
                    Tamper::None => {}
                    Tamper::Height => entry[..4].copy_from_slice(&999u32.to_le_bytes()),
                    Tamper::Parent => entry[4] ^= 1,
                    Tamper::File => entry[37..41].copy_from_slice(&7u32.to_le_bytes()),
                }
            }
            b.put(INDEX, id, &entry);
        }
    }
    let mut info = Vec::new();
    BlockFileInfo::default().encode_value(&mut info);
    b.put(FILES, &0u32.to_be_bytes(), &info);
    w.mark(Mark::PruneDecision {
        due: PruneDue::Manual(64),
        dry_run: vec![0],
        met_target: true,
        predicted_lock: None,
        flush_for_prune: true,
        full_flush: vec![FlushCause::Prune],
        new: *r.active.last().unwrap(),
        unlocked: vec![0],
    });
    w.mark(Mark::PruneSelected {
        flush: 99,
        files: vec![0],
        bound: 64,
        rearmed: false,
        target_unmet: false,
        lock: None,
    });
    e.write(b, Commit::Durable).unwrap();
    if finalized {
        // ToyChain's PRUNE_FINALIZED line for flush 99.
        w.mark(Mark::Log {
            line: "prune finalized by flush 99".into(),
        });
    }
    let got = t.check(&w, &r, &w.trace());
    got.is_err_and(|v| v.iter().any(|v| v.invariant == InvariantId::R5))
}

/// Sol's reproduction: the pruned block's height changes to 999.
#[test]
fn cp3_2_prune_keeps_the_height() {
    assert!(prune_probe(Tamper::Height, true), "pruning hid an immutable height change");
}

#[test]
fn cp3_2_prune_keeps_the_parent() {
    assert!(prune_probe(Tamper::Parent, true), "pruning hid an immutable parent change");
}

#[test]
fn cp3_2_prune_keeps_the_file() {
    assert!(prune_probe(Tamper::File, true), "pruning hid an immutable file change");
}

/// Control: the finalized, eligible prune's resets alone are entitled.
#[test]
fn cp3_2_finalized_prune_is_entitled() {
    assert!(!prune_probe(Tamper::None, true));
}

/// A chainstate barrier after the selection does not entitle it: only its own flush's finalizer
/// returning does.
#[test]
fn cp3_2_unfinalized_prune_is_not_entitled() {
    assert!(
        prune_probe(Tamper::None, false),
        "a prune whose finalizer never returned was entitled"
    );
}

// ---------------------------------------------------------------------------------------------
// CP3-3: R7a compares every nested branch with an uninterrupted recovery from the same
// inherited inputs; it skips one only when its interrupted recovery read an inherited input that
// the inner crash then changed (§5.5's pre-promotion divergence, the Deriver control in
// power_loss_phase_d.rs).

mod r7a {
    use super::*;
    use dashd_store::KvRead;
    use dashd_store::faulty::{
        Abort, Hash, Mode, Nested, Reference, Repro, Scenario, Sut, Trace, Violation, explore,
        replay,
    };

    pub struct NoRef;
    impl Reference for NoRef {
        type Config = String;
        type Op = String;
        fn new(_: &String) -> Self {
            Self
        }
        fn apply(&mut self, _: &String) {}
    }

    /// The finalizer every probe's recovery runs: Buffered `flag`, then Durable `done`. "bug"
    /// omits the barrier between them, so a power loss can keep `done` and lose `flag`;
    /// "fixed" syncs the WAL first. Its projection byte: 9 once `done` is without `flag`.
    fn finalize(w: &World, cfg: &str) -> u8 {
        let e = w.engine("probe", &SCHEMA);
        let done = e.get(CF, b"done").unwrap().is_some();
        let flag = e.get(CF, b"flag").unwrap().is_some();
        if !done {
            put(w, b"flag", b"v", Commit::Buffered);
            if cfg == "fixed" {
                e.sync_wal().unwrap();
            }
            put(w, b"done", b"v", Commit::Durable);
        }
        if done && !flag {
            9
        } else {
            0
        }
    }

    fn check(_: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    /// Sol's engine probe: recovery reads an unrelated `scratch` key, pending since the forward
    /// run (Buffered; "durable" commits it), before the finalizer.
    pub struct Finalizer {
        p: Hash,
        w: World,
    }
    impl Sut for Finalizer {
        type Config = String;
        type Op = String;
        type Ref = NoRef;
        const NAME: &'static str = "cp3-finalizer";
        fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
            let mut p = [0; 32];
            if w.epoch() > 0 {
                let _unrelated = w.engine("probe", &SCHEMA).get(CF, b"scratch").unwrap();
                p[0] = finalize(w, cfg);
            }
            Ok(Self {
                p,
                w: w.clone(),
            })
        }
        fn apply(&mut self, op: &String) -> Result<(), Abort> {
            put(&self.w, b"baseline", b"v", Commit::Durable);
            let commit = if op == "durable" {
                Commit::Durable
            } else {
                Commit::Buffered
            };
            put(&self.w, b"scratch", b"irrelevant", commit);
            Ok(())
        }
        fn check(&self, w: &World, r: &NoRef, t: &Trace) -> Result<(), Vec<Violation>> {
            check(w, r, t)
        }
        fn projection(&self) -> Option<Hash> {
            Some(self.p)
        }
    }

    /// Sol's filesystem probe: recovery promotes the engine (§5.5), then reads an unrelated
    /// scratch file, unsynced since the forward run ("durable" syncs it), before the finalizer.
    pub struct FileFinalizer {
        p: Hash,
        w: World,
    }
    impl Sut for FileFinalizer {
        type Config = String;
        type Op = String;
        type Ref = NoRef;
        const NAME: &'static str = "cp3-file-finalizer";
        fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
            let mut p = [0; 32];
            if w.epoch() > 0 {
                w.engine("probe", &SCHEMA).sync_wal().unwrap();
                let _ = w.fs().read_exact_at(Path::new("/dd/scratch"), 0, &mut [0; 512]);
                p[0] = finalize(w, cfg);
            }
            Ok(Self {
                p,
                w: w.clone(),
            })
        }
        fn apply(&mut self, op: &String) -> Result<(), Abort> {
            put(&self.w, b"baseline", b"v", Commit::Durable);
            let fs = self.w.fs();
            fs.write_all_at(Path::new("/dd/scratch"), 0, &[7; 512]).unwrap();
            fs.sync_dir(Path::new("/dd")).unwrap();
            if op == "durable" {
                fs.sync_data(Path::new("/dd/scratch")).unwrap();
            }
            Ok(())
        }
        fn check(&self, w: &World, r: &NoRef, t: &Trace) -> Result<(), Vec<Violation>> {
            check(w, r, t)
        }
        fn projection(&self) -> Option<Hash> {
            Some(self.p)
        }
    }

    /// The scratch file read after the finalizer, into the projection: an inner crash before
    /// that read can change it unread, so only an uninterrupted recovery from the same scratch
    /// is a fair reference.
    pub struct LateReader {
        p: Hash,
        w: World,
    }
    impl Sut for LateReader {
        type Config = String;
        type Op = String;
        type Ref = NoRef;
        const NAME: &'static str = "cp3-late-reader";
        fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
            let mut p = [0; 32];
            if w.epoch() > 0 {
                w.engine("probe", &SCHEMA).sync_wal().unwrap();
                p[0] = finalize(w, cfg);
                let mut scratch = [0; 512];
                if w.fs().read_exact_at(Path::new("/dd/scratch"), 0, &mut scratch).is_ok() {
                    p[1] = 1;
                    p[2] = scratch[0];
                }
            }
            Ok(Self {
                p,
                w: w.clone(),
            })
        }
        fn apply(&mut self, op: &String) -> Result<(), Abort> {
            FileFinalizer {
                p: [0; 32],
                w: self.w.clone(),
            }
            .apply(op)
        }
        fn check(&self, w: &World, r: &NoRef, t: &Trace) -> Result<(), Vec<Violation>> {
            check(w, r, t)
        }
        fn projection(&self) -> Option<Hash> {
            Some(self.p)
        }
    }

    /// `LateReader` reading the scratch file in its projection instead of its recovery: the
    /// cells only the projection decides count as the branch's inputs too.
    pub struct LateProjector {
        p: Hash,
        w: World,
    }
    impl Sut for LateProjector {
        type Config = String;
        type Op = String;
        type Ref = NoRef;
        const NAME: &'static str = "cp3-late-projector";
        fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
            let mut p = [0; 32];
            if w.epoch() > 0 {
                w.engine("probe", &SCHEMA).sync_wal().unwrap();
                p[0] = finalize(w, cfg);
            }
            Ok(Self {
                p,
                w: w.clone(),
            })
        }
        fn apply(&mut self, op: &String) -> Result<(), Abort> {
            FileFinalizer {
                p: [0; 32],
                w: self.w.clone(),
            }
            .apply(op)
        }
        fn check(&self, w: &World, r: &NoRef, t: &Trace) -> Result<(), Vec<Violation>> {
            check(w, r, t)
        }
        fn projection(&self) -> Option<Hash> {
            let mut p = self.p;
            let mut scratch = [0; 512];
            if self.w.epoch() > 0
                && self.w.fs().read_exact_at(Path::new("/dd/scratch"), 0, &mut scratch).is_ok()
            {
                p[1] = 1;
                p[2] = scratch[0];
            }
            Some(p)
        }
    }

    fn r7a_found(v: &[Violation]) -> bool {
        v.iter().any(|v| v.invariant == InvariantId::R7a)
    }

    /// A replay: the outer process kill after forward event `at`, the inner power loss after
    /// the recovery's event `j`, with `decisions`.
    fn nested<S: Sut<Config = String, Op = String>>(
        cfg: &str,
        op: &str,
        at: u64,
        j: u64,
        decisions: &[(&str, u32)],
    ) -> Vec<Violation> {
        let sc = Scenario::<S>::new("cp3-r7a", cfg.into(), vec![op.into()]);
        let mut r = Repro::new::<S>(&sc, None);
        r.crash_at = Some(at);
        r.kind = CrashKind::ProcessKill;
        r.nested = Some((j, CrashKind::PowerLoss));
        r.decisions.extend(decisions.iter().map(|(n, c)| (n.to_string(), *c)));
        replay::<S>(&r).unwrap()
    }

    fn search<S: Sut<Config = String, Op = String>>(
        cfg: &str,
        op: &str,
    ) -> dashd_store::faulty::Report {
        let mut sc = Scenario::<S>::new("cp3-r7a", cfg.into(), vec![op.into()]);
        sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
        sc.nested = Nested::Depth1;
        explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 1024,
            },
        )
    }

    /// Sol's reproduction: after the engine promotion, the scratch file's `^` cells keep its
    /// content (choice 0) and the inner crash loses only the recovery's `flag`.
    #[test]
    fn cp3_3_pending_scratch_after_promotion_does_not_mask_the_bug() {
        let v = nested::<FileFinalizer>("bug", "", 3, 2, &[("2:probe:b1", 1)]);
        assert!(r7a_found(&v), "the unchanged inherited scratch masked R7a: {v:?}");
    }

    /// Sol's engine reproduction, before promotion: the unrelated `scratch` batch survives.
    #[test]
    fn cp3_3_unrelated_pending_read_does_not_mask_the_bug() {
        let v = nested::<Finalizer>("bug", "", 2, 1, &[("2:probe:b2", 1)]);
        assert!(r7a_found(&v), "the unchanged inherited scratch masked R7a: {v:?}");
    }

    /// Controls: the same bugs with the scratch durable, as before.
    #[test]
    fn cp3_3_durable_scratch_controls() {
        assert!(r7a_found(&nested::<Finalizer>("bug", "durable", 3, 1, &[("2:probe:b2", 1)])));
        assert!(r7a_found(&nested::<FileFinalizer>("bug", "durable", 4, 2, &[("2:probe:b1", 1)])));
    }

    /// Control: the pre-final barrier fixes the finalizer.
    #[test]
    fn cp3_3_fixed_barrier_control() {
        assert!(nested::<Finalizer>("fixed", "", 2, 2, &[]).is_empty());
    }

    /// The searches find both bugs, and stay clean once they are fixed.
    #[test]
    fn cp3_3_search_finds_the_masked_bugs() {
        for found in [search::<Finalizer>("bug", ""), search::<FileFinalizer>("bug", "")] {
            assert!(found.found().contains(&InvariantId::R7a), "{}", found.summary());
        }
        for clean in [search::<Finalizer>("fixed", ""), search::<FileFinalizer>("fixed", "")] {
            assert!(clean.is_clean(), "{}", clean.summary());
        }
    }

    /// An inherited input the inner crash changed before recovery read it: R7a compares with the
    /// uninterrupted recovery from the same scratch, so the fixed finalizer is clean (the scratch
    /// differs from the uninterrupted recovery's) and the bug is still found.
    #[test]
    fn cp3_3_changed_unread_input_is_compared_from_the_same_input() {
        let fixed = search::<LateReader>("fixed", "");
        eprintln!("fixed: {}", fixed.summary());
        assert!(fixed.is_clean(), "{}", fixed.summary());
        let bug = search::<LateReader>("bug", "");
        eprintln!("bug: {}", bug.summary());
        assert!(bug.found().contains(&InvariantId::R7a), "{}", bug.summary());
    }

    /// The same with the scratch file read only by the projection (the CP3.1 review's probe).
    #[test]
    fn cp3_3_cells_the_projection_decides_are_inputs_too() {
        let fixed = search::<LateProjector>("fixed", "");
        eprintln!("fixed: {}", fixed.summary());
        assert!(fixed.is_clean(), "{}", fixed.summary());
        let bug = search::<LateProjector>("bug", "");
        assert!(bug.found().contains(&InvariantId::R7a), "{}", bug.summary());
    }
}

// ---------------------------------------------------------------------------------------------
// CP3-5, CP3-6: the nightly runner (contrib/node/st3a-nightly.sh) exits non-zero if any component
// fails or runs no test, and records the sweep seed it rotates. Stub binaries stand in for the
// release tests. `ST3A_NIGHTLY_RUNNER` points the test at another runner (to show an old one
// failing it).

// The runner needs bash 4, GNU time and GNU realpath.
#[cfg(target_os = "linux")]
#[test]
fn cp3_5_nightly_exit_is_the_or_of_its_components() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let runner = std::env::var("ST3A_NIGHTLY_RUNNER").unwrap_or_else(|_| {
        format!("{}/../contrib/node/st3a-nightly.sh", env!("CARGO_MANIFEST_DIR"))
    });
    let dir = std::env::temp_dir().join(format!("st3a-nightly-{}", std::process::id()));
    let bins = dir.join("bin");
    std::fs::create_dir_all(&bins).unwrap();
    // libtest's output for `--exact NAME`: "ok" passes, "fails" fails, any other name matches
    // no test.
    let stub = r#"#!/bin/sh
case "$2" in
  ok) echo "running 1 test"; echo "test result: ok. 1 passed; 0 failed"; exit 0 ;;
  fails) echo "running 1 test"; echo "test result: FAILED. 0 passed; 1 failed"; exit 17 ;;
  *) echo "running 0 tests"; echo "test result: ok. 0 passed; 0 failed"; exit 0 ;;
esac
"#;
    for name in ["power_loss_deep", "power_loss_model"] {
        let path = bins.join(name);
        std::fs::write(&path, stub).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |components: &[&str]| {
        let out = dir.join(components.join("+"));
        std::fs::create_dir_all(&out).unwrap();
        let status = Command::new("bash")
            .arg(&runner)
            .arg(&out)
            .args(components)
            .env("ST3A_NIGHTLY_BIN_DIR", &bins)
            // The checkpoint-3 scratch runner's output setting (ST3A_NIGHTLY_RUNNER).
            .env("ST3A_NIGHTLY_OUT", &out)
            .env("ST3A_SWEEP_SEED", "20261009")
            .status()
            .unwrap();
        let summary = std::fs::read_to_string(out.join("summary.txt")).unwrap_or_default();
        (status.success(), summary)
    };
    let (ok, summary) = run(&["fails", "ok"]);
    assert!(!ok, "a failing component fails the nightly: {summary}");
    assert!(summary.contains("fails exit=17") && summary.contains(" ok exit=0"), "{summary}");
    let (ok, summary) = run(&["ok", "misspelled"]);
    assert!(!ok, "a component that runs no test fails the nightly: {summary}");
    let (ok, summary) = run(&["ok"]);
    assert!(ok, "{summary}");
    assert!(summary.contains("ST3A_SWEEP_SEED=20261009"), "the seed is recorded: {summary}");
    std::fs::remove_dir_all(&dir).unwrap();
}
