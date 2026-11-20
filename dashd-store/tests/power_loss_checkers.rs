//! Regressions for the checkers themselves (ST3a checkpoint-1 GPT review, majors 1 and 2):
//! D1 after a crash must see every cell the branch has decided and check recovery's own
//! orderings; R8 must see recovery errors and read faults; an exploration must never pass
//! vacuously.

use std::path::Path;
use std::sync::Arc;

use dashd_store::faulty::{
    Abort, AllSurvive, CrashKind, EventSel, Fault, FaultKind, FaultOutcome, FaultPlan, FileRef,
    InvariantId, Mark, Mode, RefExtractor, Reference, Requirement, Scenario, StoreSel, Sut, Trace,
    TraceEntry, Violation, World, WorldConfig, WorldDurableView, explore,
};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};

static SCHEMA: DbSchema = DbSchema {
    name: "probe",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};
const CF: Cf = Cf::new(0);
const F: &str = "/dd/f";

/// `ref` needs the first 512 bytes of `/dd/f`.
struct Refs;

impl RefExtractor for Refs {
    fn refs(&self, _: &str, _: Cf, key: &[u8], _: &[u8], _: &WorldDurableView) -> Vec<Requirement> {
        if key == b"ref" {
            vec![Requirement::Bytes(FileRef::new(F, 0, 512))]
        } else {
            vec![]
        }
    }
}

fn put(w: &World, key: &[u8], commit: Commit) {
    let e = w.engine("probe", &SCHEMA);
    let mut b = e.batch();
    b.put(CF, key, b"v");
    e.write(b, commit).unwrap();
}

fn del(w: &World, key: &[u8], commit: Commit) {
    let e = w.engine("probe", &SCHEMA);
    let mut b = e.batch();
    b.delete(CF, key);
    e.write(b, commit).unwrap();
}

fn synced_file(w: &World) {
    let fs = w.fs();
    fs.write_all_at(Path::new(F), 0, &[7; 512]).unwrap();
    fs.sync_data(Path::new(F)).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
}

fn d1(w: &World) -> Vec<InvariantId> {
    w.d1_violations().iter().map(|v| v.invariant).collect()
}

/// GPT major 1: after a crash, a record the branch has read is known present; unlinking its
/// file is D1(e), and the barrier after it D1(a).
#[test]
fn d1_sees_a_record_decided_present_after_a_crash() {
    let w = World::new(WorldConfig::default());
    w.set_extractor(Arc::new(Refs));
    synced_file(&w);
    put(&w, b"ref", Commit::Buffered);
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
    w.set_extractor(Arc::new(Refs));
    let e = w.engine("probe", &SCHEMA);
    assert!(e.get(CF, b"ref").unwrap().is_some(), "decided: survived");
    w.fs().sync_data(Path::new(F)).unwrap();
    w.fs().remove(Path::new(F)).unwrap();
    w.fs().sync_dir(Path::new("/dd")).unwrap();
    let found = d1(&w);
    assert!(found.contains(&InvariantId::D1e), "{:?}", w.d1_violations());
    assert!(found.contains(&InvariantId::D1a), "{:?}", w.d1_violations());
}

/// GPT major 1, second control: a new Durable record referencing a file the branch has
/// observed absent is D1(a).
#[test]
fn d1_sees_a_file_decided_absent_after_a_crash() {
    let w = World::new(WorldConfig::default());
    synced_file(&w);
    w.fs().remove(Path::new(F)).unwrap();
    let w = w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
    w.set_extractor(Arc::new(Refs));
    assert_eq!(w.fs().len(Path::new(F)).unwrap(), None, "decided: the unlink survived");
    put(&w, b"ref", Commit::Durable);
    assert!(d1(&w).contains(&InvariantId::D1a), "{:?}", w.d1_violations());
    assert!(!w.durable_view().file_durable(Path::new(F)));
}

/// D1 at the start of an epoch: recovery's first mutation is checked against the durable
/// records the crash left, before any barrier of its own.
#[test]
fn d1_checks_recovery_before_its_first_barrier() {
    let w = World::new(WorldConfig::default());
    synced_file(&w);
    put(&w, b"ref", Commit::Durable);
    let w = w.capture().boot(CrashKind::ProcessKill, Box::new(AllSurvive), WorldConfig::default());
    w.set_extractor(Arc::new(Refs));
    w.fs().remove(Path::new(F)).unwrap();
    assert!(d1(&w).contains(&InvariantId::D1e), "{:?}", w.d1_violations());
}

// ---------------------------------------------------------------------------------------------
// A planted bug: recovery finishes an interrupted drop by unlinking before the reference goes.

struct NoRef;

impl Reference for NoRef {
    type Op = String;
    type Config = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}

/// Config `bug` or `ok`. The op `drop` records the intent, deletes `ref` Durably, unlinks the
/// file and syncs the directory. Recovery finishes an interrupted drop; with `bug` it unlinks
/// first.
struct Dropper {
    w: World,
}

impl Dropper {
    fn finish_drop(w: &World, bug: bool) -> Result<(), Abort> {
        let fs = w.fs();
        if bug {
            let _ = fs.remove(Path::new(F)).map_err(|_| Abort::new("remove"))?;
            fs.sync_dir(Path::new("/dd")).map_err(|_| Abort::new("sync_dir"))?;
            del(w, b"ref", Commit::Durable);
        } else {
            del(w, b"ref", Commit::Durable);
            let _ = fs.remove(Path::new(F)).map_err(|_| Abort::new("remove"))?;
            fs.sync_dir(Path::new("/dd")).map_err(|_| Abort::new("sync_dir"))?;
        }
        del(w, b"dropping", Commit::Durable);
        Ok(())
    }
}

impl Sut for Dropper {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "dropper";

    fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
        let e = w.engine("probe", &SCHEMA);
        w.mark(Mark::RecoveryBegin);
        if e.get(CF, b"init").unwrap().is_none() {
            synced_file(w);
            put(w, b"ref", Commit::Durable);
            put(w, b"init", Commit::Durable);
        } else if e.get(CF, b"dropping").unwrap().is_some() {
            Self::finish_drop(w, cfg == "bug")?;
        }
        w.mark(Mark::RecoveryEnd);
        Ok(Self {
            w: w.clone(),
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.w, b"dropping", Commit::Durable);
        Self::finish_drop(&self.w, false)
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn extractor(_: &String) -> Option<Arc<dyn RefExtractor>> {
        Some(Arc::new(Refs))
    }
}

const EXHAUSTIVE: Mode = Mode::Exhaustive {
    max_branches_per_image: 1024,
};

#[test]
fn planted_recovery_unlink_before_the_reference_is_d1e() {
    let r =
        explore(&Scenario::<Dropper>::new("drop", "bug".into(), vec!["drop".into()]), EXHAUSTIVE);
    assert!(r.found().contains(&InvariantId::D1e), "{}", r.summary());
    let r =
        explore(&Scenario::<Dropper>::new("drop", "ok".into(), vec!["drop".into()]), EXHAUSTIVE);
    assert!(r.is_clean(), "{}", r.summary());
}

// ---------------------------------------------------------------------------------------------
// GPT major 2: R8 over recovery, read faults, and no vacuous exploration.

/// Writes a file, then in a recovery region reads it with an injected fault, ignores the error
/// and mutates (GPT's `BadRead`); with config `abort` it aborts instead (the control).
struct BadRead(World);

impl Sut for BadRead {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "bad-read";

    fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
        let p = Path::new("/dd/source");
        w.fs().write_all_at(p, 0, &[1]).unwrap();
        w.mark(Mark::RecoveryBegin);
        if w.fs().read_exact_at(p, 0, &mut [0; 1]).is_err() && cfg == "abort" {
            w.mark(Mark::StoreFailed {
                op: "read",
            });
            w.mark(Mark::Abort {
                message: "read failed".into(),
            });
            return Err(Abort::new("read failed"));
        }
        w.fs().create(Path::new("/dd/illegal-after-fault")).unwrap();
        w.mark(Mark::RecoveryEnd);
        Ok(Self(w.clone()))
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        self.0.fs().create(Path::new("/dd/another-mutation")).unwrap();
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn sanctioned(_: &String, message: &str) -> bool {
        message == "read failed"
    }
}

fn read_fault() -> Fault {
    Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: dashd_store::faulty::OpKind::Read,
            n: 0,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    }
}

#[test]
fn r8_sees_a_mutation_after_a_fatal_read_in_recovery() {
    let mut sc = Scenario::<BadRead>::new("bad-read", "ignore".into(), vec![String::new()]);
    sc.faults = FaultPlan(vec![read_fault()]);
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.found().contains(&InvariantId::R8), "{}", r.summary());
    assert!(r.images > 0, "the read fault's run captured images: {}", r.summary());
}

#[test]
fn a_sanctioned_abort_after_a_fatal_read_is_clean() {
    let mut sc = Scenario::<BadRead>::new("bad-read", "abort".into(), vec![String::new()]);
    sc.faults = FaultPlan(vec![read_fault()]);
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.images > 0, "{}", r.summary());
}

/// Normal in epoch 0; on reopen, a failed rename in recovery is ignored and followed by a
/// mutation (GPT's `BadRecover`).
struct BadRecover(World);

impl Sut for BadRecover {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "bad-recover";

    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        if w.trace().entries.iter().any(|e| matches!(e, TraceEntry::Crash { .. })) {
            w.mark(Mark::RecoveryBegin);
            assert!(w.fs().rename(Path::new("/dd/missing"), Path::new("/dd/renamed")).is_err());
            w.fs().create(Path::new("/dd/illegal-after-failed-rename")).unwrap();
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self(w.clone()))
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        self.0.fs().create(Path::new("/dd/trigger")).unwrap();
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
}

#[test]
fn r8_runs_on_recovery_traces() {
    let sc = Scenario::<BadRecover>::new("bad-recover", String::new(), vec![String::new()]);
    let r = explore(&sc, EXHAUSTIVE);
    assert!(r.found().contains(&InvariantId::R8), "{}", r.summary());
}

// ---------------------------------------------------------------------------------------------
// The Opus checkpoint-1 review's D1 probes, as assertions.

static M: DbSchema = DbSchema {
    name: "m",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered), CfSpec::new("a", CfProfile::Ordered)],
};
const A: Cf = Cf::new(1);

/// `rec:<path>` = len: needs `[0, len)` of `<path>`; `fin` finalizes `SinceFlushStart`, `b1`
/// `PreviousFinalizer`; `blk:<n>` needs block `n` (resolved only for `n < 10`).
struct X;

impl RefExtractor for X {
    fn refs(
        &self,
        _: &str,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        _: &WorldDurableView,
    ) -> Vec<Requirement> {
        if cf != A {
            return vec![];
        }
        if let Some(path) = key.strip_prefix(b"rec:") {
            let len = u64::from_le_bytes(value[..8].try_into().unwrap());
            return vec![Requirement::Bytes(FileRef::new(
                String::from_utf8(path.to_vec()).unwrap(),
                0,
                len,
            ))];
        }
        if let Some(n) = key.strip_prefix(b"blk:") {
            let mut hash = [0u8; 32];
            hash[0] = n[0];
            return vec![Requirement::Block {
                hash,
                undo: false,
            }];
        }
        vec![]
    }

    fn finalizes(
        &self,
        _: &str,
        cf: Cf,
        key: &[u8],
        _: Option<&[u8]>,
    ) -> Option<Vec<dashd_store::faulty::Scope>> {
        use dashd_store::faulty::Scope;
        if cf != A {
            return None;
        }
        match key {
            b"fin" => Some(vec![Scope::SinceFlushStart]),
            b"b1" => Some(vec![Scope::PreviousFinalizer]),
            _ => None,
        }
    }

    fn resolve(&self, req: &Requirement, _: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        match req {
            Requirement::Bytes(r) => Ok(vec![r.clone()]),
            Requirement::Block {
                hash,
                ..
            } if hash[0] < 10 => Ok(vec![FileRef::new("/dd/blk", u64::from(hash[0]) * 100, 100)]),
            _ => Err("no durable index entry".into()),
        }
    }
}

fn xworld() -> World {
    let w = World::new(WorldConfig::default());
    w.set_extractor(Arc::new(X));
    w
}

fn xboot(w: &World, kind: CrashKind, d: &[(&str, u32)]) -> World {
    let d = d.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    let w2 = w.capture().boot(
        kind,
        Box::new(dashd_store::faulty::Fixed::new(d)),
        WorldConfig::default(),
    );
    w2.set_extractor(Arc::new(X));
    w2
}

fn xput(w: &World, k: &[u8], v: &[u8], c: Commit) {
    let e = w.engine("m", &M);
    let mut b = e.batch();
    b.put(A, k, v);
    e.write(b, c).unwrap();
}

fn rec(w: &World, path: &str, len: u64) {
    xput(w, &[b"rec:".as_slice(), path.as_bytes()].concat(), &len.to_le_bytes(), Commit::Durable);
}

fn xkinds(w: &World) -> std::collections::BTreeSet<InvariantId> {
    w.d1_violations().iter().map(|v| v.invariant).collect()
}

fn synced(w: &World, path: &str, byte: u8, len: usize) {
    let fs = w.fs();
    fs.write_all_at(Path::new(path), 0, &vec![byte; len]).unwrap();
    fs.sync_data(Path::new(path)).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
}

/// Opus major 1: a replace-rename with no `sync_dir`, then a Durable record over the new
/// content, is D1(a) (the crash can keep the old bytes under the name).
#[test]
fn d1_pending_rebinding_of_a_referenced_name() {
    let w = xworld();
    synced(&w, "/dd/b", 1, 512);
    let fs = w.fs();
    fs.write_all_at(Path::new("/dd/tmp"), 0, &[2u8; 512]).unwrap();
    fs.sync_data(Path::new("/dd/tmp")).unwrap();
    fs.rename(Path::new("/dd/tmp"), Path::new("/dd/b")).unwrap();
    rec(&w, "/dd/b", 512);
    assert!(xkinds(&w).contains(&InvariantId::D1a), "{:?}", w.d1_violations());
    // With the directory synced first, it is clean.
    let w = xworld();
    synced(&w, "/dd/b", 1, 512);
    let fs = w.fs();
    fs.write_all_at(Path::new("/dd/tmp"), 0, &[2u8; 512]).unwrap();
    fs.sync_data(Path::new("/dd/tmp")).unwrap();
    fs.rename(Path::new("/dd/tmp"), Path::new("/dd/b")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    rec(&w, "/dd/b", 512);
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
}

/// `make_private`'s pattern over a referenced file: a byte-identical copy renamed over it is
/// neither D1(d) at the rename nor D1(a) at a barrier before the directory sync.
#[test]
fn d1_byte_identical_replacement_is_clean() {
    let w = xworld();
    synced(&w, "/dd/b", 1, 512);
    rec(&w, "/dd/b", 512);
    let fs = w.fs();
    fs.copy(Path::new("/dd/b"), Path::new("/dd/b.unlink")).unwrap();
    fs.sync_data(Path::new("/dd/b.unlink")).unwrap();
    fs.rename(Path::new("/dd/b.unlink"), Path::new("/dd/b")).unwrap();
    w.engine("m", &M).sync_wal().unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
}

/// Opus major 1: renaming a referenced file away is D1(e) at once, and D1(a) at the next
/// barrier.
#[test]
fn d1_rename_away_from_a_referenced_name() {
    let w = xworld();
    synced(&w, "/dd/f", 1, 512);
    rec(&w, "/dd/f", 512);
    w.fs().rename(Path::new("/dd/f"), Path::new("/dd/g")).unwrap();
    assert!(xkinds(&w).contains(&InvariantId::D1e), "{:?}", w.d1_violations());
    w.engine("m", &M).sync_wal().unwrap();
    assert!(xkinds(&w).contains(&InvariantId::D1a), "{:?}", w.d1_violations());
}

/// Opus major 2: after a crash of either kind, a truncate and an unlink of a referenced file
/// before recovery's first barrier are D1(d) and D1(e).
#[test]
fn d1_after_boot_before_the_first_barrier() {
    for kind in [CrashKind::ProcessKill, CrashKind::PowerLoss] {
        let w = xworld();
        synced(&w, "/dd/f", 1, 1000);
        rec(&w, "/dd/f", 1000);
        let w2 = xboot(&w, kind, &[]);
        w2.fs().truncate(Path::new("/dd/f"), 10).unwrap();
        w2.fs().remove(Path::new("/dd/f")).unwrap();
        let k = xkinds(&w2);
        assert!(k.contains(&InvariantId::D1d) && k.contains(&InvariantId::D1e), "{kind:?}: {k:?}");
    }
}

/// Opus major 3: a file whose create the branch decided lost, recreated without `sync_dir`
/// and then referenced, is D1(a).
#[test]
fn d1_recreated_name_without_sync_dir() {
    let w = xworld();
    w.fs().write_all_at(Path::new("/dd/f"), 0, &[1u8; 512]).unwrap();
    let w2 = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 1)]);
    let fs = w2.fs();
    assert_eq!(fs.len(Path::new("/dd/f")).unwrap(), None, "decided: the create was lost");
    fs.write_all_at(Path::new("/dd/f"), 0, &[2u8; 512]).unwrap();
    fs.sync_data(Path::new("/dd/f")).unwrap();
    rec(&w2, "/dd/f", 512);
    assert!(xkinds(&w2).contains(&InvariantId::D1a), "{:?}", w2.d1_violations());
}

/// `xworld` whose `n`-th `sync_dir` fails with EIO, leaving its operations pending (§4.4).
fn xworld_failing_sync_dir(n: u32) -> World {
    xworld_failing_sync_dirs(&[n])
}

/// `xworld` whose `sync_dir`s numbered in `ns` fail with EIO.
fn xworld_failing_sync_dirs(ns: &[u32]) -> World {
    let fault = |n| Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: dashd_store::faulty::OpKind::SyncDir,
            n,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::Applied,
    };
    let w = World::new(WorldConfig {
        faults: ns.iter().copied().map(fault).collect(),
        ..WorldConfig::default()
    });
    w.set_extractor(Arc::new(X));
    w
}

/// DEC-212: Core's `DirectoryCommit` ignores a failed directory sync, so a durable reference into
/// a file whose entry the failed `sync_dir` left pending is entitled: D1(a) is silent before the
/// crash and after one that loses the entry, and R1's view form reads the lost file as entitled.
#[test]
fn dec212_an_entry_a_failed_sync_dir_left_pending_is_entitled() {
    let n = Path::new("/dd/n");
    let w = xworld_failing_sync_dir(0);
    let fs = w.fs();
    fs.write_all_at(n, 0, &[1u8; 512]).unwrap();
    fs.sync_data(n).unwrap();
    assert!(fs.sync_dir(Path::new("/dd")).is_err());
    rec(&w, "/dd/n", 512);
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
    let lost = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 1)]);
    assert_eq!(lost.fs().len(n).unwrap(), None, "decided: the create was lost");
    assert!(lost.lost_to_failed_dir_sync(n));
    lost.engine("m", &M).sync_wal().unwrap();
    assert!(xkinds(&lost).is_empty(), "{:?}", lost.d1_violations());
    // A later sync that succeeds does not end it: the reference dangles in Core too (Opus r2 1).
    lost.fs().sync_dir(Path::new("/dd")).unwrap();
    assert!(lost.lost_to_failed_dir_sync(n));
    let kept = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 0)]);
    assert_eq!(kept.fs().len(n).unwrap(), Some(512), "decided: the create survived");
    assert!(!kept.lost_to_failed_dir_sync(n));
    let killed = xboot(&w, CrashKind::ProcessKill, &[]);
    killed.engine("m", &M).sync_wal().unwrap();
    assert!(xkinds(&killed).is_empty(), "{:?}", killed.d1_violations());
}

/// DEC-212's entitlement is narrow. A dropped entry that no failed `sync_dir` left pending is
/// still D1(a), and R1's view form does not read its file as entitled: with no `sync_dir`, with
/// a failed one before the create, and with another operation on the name after the failure.
/// An entitled file's unsynced bytes are still D1(a).
#[test]
fn dec212_other_dropped_entries_are_still_reported() {
    let n = Path::new("/dd/n");
    let write = |w: &World, sync: bool| {
        w.fs().write_all_at(n, 0, &[1u8; 512]).unwrap();
        if sync {
            w.fs().sync_data(n).unwrap();
        }
    };
    let reported = |w: &World, op: &str, why: &str| {
        rec(w, "/dd/n", 512);
        assert!(xkinds(w).contains(&InvariantId::D1a), "{why}: {:?}", w.d1_violations());
        let lost = xboot(w, CrashKind::PowerLoss, &[(op, 1)]);
        assert_eq!(lost.fs().len(n).unwrap(), None, "{why}: decided: the entry was lost");
        assert!(!lost.lost_to_failed_dir_sync(n), "{why}");
        lost.engine("m", &M).sync_wal().unwrap();
        assert!(xkinds(&lost).contains(&InvariantId::D1a), "{why}: {:?}", lost.d1_violations());
    };
    // MicroLog's P3: no `sync_dir` at all.
    let w = xworld();
    write(&w, true);
    reported(&w, "1:fs:/dd:op0", "no sync_dir");
    // The failed `sync_dir` came before the create.
    let w = xworld_failing_sync_dir(0);
    assert!(w.fs().sync_dir(Path::new("/dd")).is_err());
    write(&w, true);
    reported(&w, "1:fs:/dd:op0", "an earlier failed sync_dir");
    // The name was removed and created again after the failure.
    let w = xworld_failing_sync_dir(0);
    write(&w, true);
    assert!(w.fs().sync_dir(Path::new("/dd")).is_err());
    w.fs().remove(n).unwrap();
    write(&w, true);
    reported(&w, "1:fs:/dd:op2", "a later operation on the name");
    // The entitlement covers the entry, not the file's bytes.
    let w = xworld_failing_sync_dir(0);
    write(&w, false);
    assert!(w.fs().sync_dir(Path::new("/dd")).is_err());
    rec(&w, "/dd/n", 512);
    assert!(xkinds(&w).contains(&InvariantId::D1a), "{:?}", w.d1_violations());
}

/// DEC-212 entitles creates only (Opus r2 1): a removal or a rename's source that a failed
/// `sync_dir` left pending loses no file, so a reference into the file it unbound is D1(a), with
/// or without a later sync that succeeds, and R1's view form never reads that file as entitled,
/// not even after a process kill. A durable name removed and created again creates no entry
/// either, whatever the crash keeps.
#[test]
fn dec212_removals_and_rename_sources_are_not_entitled() {
    let dd = Path::new("/dd");
    let reported = |w: &World, path: &str, later_sync: bool, why: &str| {
        assert!(w.fs().sync_dir(dd).is_err());
        if later_sync {
            w.fs().sync_dir(dd).unwrap();
        }
        rec(w, path, 512);
        assert!(xkinds(w).contains(&InvariantId::D1a), "{why}: {:?}", w.d1_violations());
        assert!(!w.lost_to_failed_dir_sync(Path::new(path)), "{why}");
        let killed = xboot(w, CrashKind::ProcessKill, &[]);
        assert!(!killed.lost_to_failed_dir_sync(Path::new(path)), "{why}: after a kill");
    };
    let (n, m) = (Path::new("/dd/n"), Path::new("/dd/m"));
    for later_sync in [false, true] {
        let w = xworld_failing_sync_dir(1);
        synced(&w, "/dd/n", 1, 512);
        w.fs().remove(n).unwrap();
        reported(&w, "/dd/n", later_sync, &format!("a removal, later sync {later_sync}"));
        // Created and removed before the failure: the newest pending change is the removal.
        let w = xworld_failing_sync_dir(0);
        w.fs().write_all_at(m, 0, &[1u8; 512]).unwrap();
        w.fs().sync_data(m).unwrap();
        w.fs().remove(m).unwrap();
        let why = format!("a create removed again, later sync {later_sync}");
        reported(&w, "/dd/m", later_sync, &why);
    }
    let w = xworld_failing_sync_dir(1);
    synced(&w, "/dd/a", 1, 512);
    w.fs().rename(Path::new("/dd/a"), Path::new("/dd/b")).unwrap();
    reported(&w, "/dd/a", true, "a rename's source");
    let w = xworld_failing_sync_dir(1);
    synced(&w, "/dd/n", 1, 512);
    w.fs().remove(n).unwrap();
    w.fs().write_all_at(n, 0, &[2u8; 512]).unwrap();
    w.fs().sync_data(n).unwrap();
    reported(&w, "/dd/n", false, "a durable name created again");
    let lost = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op1", 0), ("1:fs:/dd:op2", 1)]);
    assert_eq!(lost.fs().len(n).unwrap(), None, "decided: only the removal survived");
    assert!(!lost.lost_to_failed_dir_sync(n));
}

/// DEC-212 for a directory's own entry (Opus r2 2): a fresh directory whose create a failed
/// `sync_dir` of its parent left pending may be lost with its files, as Core's `blocks/` may.
/// With the parent's sync failed, a reference into a synced file is entitled whether the file's
/// own entry was synced or its sync failed too, and R1's view form reads the file as entitled once
/// a crash loses the directory, until the directory is created again. With the parent never
/// synced, it is still D1(a), and the loss is not entitled.
#[test]
fn dec212_a_fresh_directory_a_failed_sync_left_pending_is_entitled() {
    let (dd, sub, f) = (Path::new("/dd"), Path::new("/dd/sub"), Path::new("/dd/sub/f"));
    let fresh = |w: &World| {
        let fs = w.fs();
        fs.create_dir_all(sub).unwrap();
        fs.write_all_at(f, 0, &[1u8; 512]).unwrap();
        fs.sync_data(f).unwrap();
    };
    // `sub`'s sync fails or succeeds; the parent's fails.
    for (fails, sub_synced) in [(&[0, 1][..], false), (&[1][..], true)] {
        let w = xworld_failing_sync_dirs(fails);
        fresh(&w);
        assert_eq!(w.fs().sync_dir(sub).is_ok(), sub_synced);
        assert!(w.fs().sync_dir(dd).is_err());
        rec(&w, "/dd/sub/f", 512);
        assert!(xkinds(&w).is_empty(), "sub synced {sub_synced}: {:?}", w.d1_violations());
        let lost = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 1)]);
        assert_eq!(lost.fs().len(f).unwrap(), None, "decided: the directory was lost");
        assert!(lost.lost_to_failed_dir_sync(f), "sub synced {sub_synced}");
        lost.engine("m", &M).sync_wal().unwrap();
        assert!(xkinds(&lost).is_empty(), "sub synced {sub_synced}: {:?}", lost.d1_violations());
        // Recreating the directory is an operation on its name: it ends the entitlement, the
        // directory's and its files'.
        lost.fs().create_dir_all(sub).unwrap();
        assert!(!lost.lost_to_failed_dir_sync(f), "sub synced {sub_synced}: recreated");
        let kept = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 0), ("1:fs:/dd/sub:op1", 0)]);
        assert_eq!(kept.fs().len(f).unwrap(), Some(512), "decided: both entries survived");
        assert!(!kept.lost_to_failed_dir_sync(f));
    }
    // Only `sub`'s sync was attempted, and it failed: the directory's entry is not entitled, and
    // the file's own record does not excuse a crash that loses the directory.
    let w = xworld_failing_sync_dir(0);
    fresh(&w);
    assert!(w.fs().sync_dir(sub).is_err());
    rec(&w, "/dd/sub/f", 512);
    assert!(xkinds(&w).contains(&InvariantId::D1a), "{:?}", w.d1_violations());
    let lost = xboot(&w, CrashKind::PowerLoss, &[("1:fs:/dd:op0", 1)]);
    assert_eq!(lost.fs().len(f).unwrap(), None, "decided: the directory was lost");
    assert!(!lost.lost_to_failed_dir_sync(f));
}

/// Opus minor 5: D1(f)'s scopes.
#[test]
fn d1f_scopes() {
    let begin = |w: &World| {
        w.mark(Mark::FlushBegin {
            flush: 1,
            old: None,
            new: [1; 32],
        })
    };
    // SinceFlushStart: unbarriered is D1(f); barriered is clean.
    let w = xworld();
    begin(&w);
    xput(&w, b"coin", b"1", Commit::Buffered);
    xput(&w, b"fin", b"1", Commit::Durable);
    assert!(xkinds(&w).contains(&InvariantId::D1f));
    let w = xworld();
    begin(&w);
    xput(&w, b"coin", b"1", Commit::Buffered);
    w.engine("m", &M).sync_wal().unwrap();
    xput(&w, b"fin", b"1", Commit::Durable);
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
    // A batch before FlushBegin is outside the scope.
    let w = xworld();
    xput(&w, b"coin", b"1", Commit::Buffered);
    begin(&w);
    xput(&w, b"fin", b"1", Commit::Durable);
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
    // PreviousFinalizer over a Buffered finalizer.
    let w = xworld();
    begin(&w);
    xput(&w, b"fin", b"1", Commit::Buffered);
    xput(&w, b"b1", b"1", Commit::Durable);
    assert!(xkinds(&w).contains(&InvariantId::D1f));
    // A process kill keeps the flush's pending batch in scope, unless recovery opens its own.
    for (recovery_mark, flagged) in [(false, true), (true, false)] {
        let w = xworld();
        begin(&w);
        xput(&w, b"coin", b"1", Commit::Buffered);
        let w2 = xboot(&w, CrashKind::ProcessKill, &[]);
        if recovery_mark {
            w2.mark(Mark::RecoveryBegin);
        }
        xput(&w2, b"fin", b"2", Commit::Durable);
        assert_eq!(
            xkinds(&w2).contains(&InvariantId::D1f),
            flagged,
            "RecoveryBegin {recovery_mark}"
        );
    }
}

/// D1(b) (an unresolvable block requirement) and D1(c) (an unresolvable byte requirement).
#[test]
fn d1b_and_d1c() {
    let w = xworld();
    synced(&w, "/dd/blk", 0, 2000);
    xput(&w, b"blk:\x03", b"", Commit::Durable);
    assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
    xput(&w, b"blk:\x0b", b"", Commit::Durable);
    assert!(xkinds(&w).contains(&InvariantId::D1b), "{:?}", w.d1_violations());
    struct NoBytes;
    impl RefExtractor for NoBytes {
        fn refs(
            &self,
            _: &str,
            _: Cf,
            key: &[u8],
            _: &[u8],
            _: &WorldDurableView,
        ) -> Vec<Requirement> {
            if key == b"r" {
                vec![Requirement::Bytes(FileRef::new("/dd/x", 0, 1))]
            } else {
                vec![]
            }
        }
        fn resolve(&self, _: &Requirement, _: &WorldDurableView) -> Result<Vec<FileRef>, String> {
            Err("refused".into())
        }
    }
    let w = World::new(WorldConfig::default());
    w.set_extractor(Arc::new(NoBytes));
    let e = w.engine("m", &M);
    let mut b = e.batch();
    b.put(A, b"r", b"");
    e.write(b, Commit::Durable).unwrap();
    assert!(xkinds(&w).contains(&InvariantId::D1c), "{:?}", w.d1_violations());
}

// ---------------------------------------------------------------------------------------------
// Checkpoint-1 round-2 regressions (GPT R2-1/R2-2, Opus minor A).

fn replace(w: &World, copy: bool) {
    if copy {
        w.fs().copy(Path::new("/dd/src"), Path::new("/dd/b")).unwrap();
    } else {
        w.fs().rename(Path::new("/dd/src"), Path::new("/dd/b")).unwrap();
    }
}

/// GPT R2-1 A: a 1-byte zero source replacing a referenced 512 zero bytes leaves the range
/// unreadable; both rename-over and copy-over are D1(d).
#[test]
fn r2_short_zero_source_must_be_d1d() {
    for copy in [false, true] {
        let w = xworld();
        synced(&w, "/dd/b", 0, 512);
        rec(&w, "/dd/b", 512);
        synced(&w, "/dd/src", 0, 1);
        replace(&w, copy);
        assert_eq!(w.fs().len(Path::new("/dd/b")).unwrap(), Some(1));
        assert!(xkinds(&w).contains(&InvariantId::D1d), "copy={copy}: {:?}", w.d1_violations());
    }
}

/// GPT R2-1 B: a source whose current bytes match but are not synced is no safe rename-over:
/// a power loss can keep the binding and recover the source's old bytes.
#[test]
fn r2_matching_current_but_unsynced_source_must_be_d1d() {
    let w = xworld();
    synced(&w, "/dd/b", 1, 512);
    rec(&w, "/dd/b", 512);
    synced(&w, "/dd/src", 2, 512);
    w.fs().write_all_at(Path::new("/dd/src"), 0, &[1; 512]).unwrap();
    replace(&w, false);
    assert!(xkinds(&w).contains(&InvariantId::D1d), "{:?}", w.d1_violations());
}

/// Opus minor A (N1): copy-over truncates first and is never atomic, so even identical bytes
/// are D1(d); and the pending size 0 it leaves is D1(a) at the next barrier, even when the
/// referenced bytes are zeros.
#[test]
fn r2_copy_over_identical_bytes_is_d1d() {
    for byte in [1u8, 0] {
        let w = xworld();
        synced(&w, "/dd/b", byte, 512);
        rec(&w, "/dd/b", 512);
        synced(&w, "/dd/src", byte, 512);
        replace(&w, true);
        assert!(xkinds(&w).contains(&InvariantId::D1d), "byte {byte}: {:?}", w.d1_violations());
        w.engine("m", &M).sync_wal().unwrap();
        assert!(xkinds(&w).contains(&InvariantId::D1a), "byte {byte}: {:?}", w.d1_violations());
    }
}

/// Opus minor A (N2): renaming a durable but shorter source over a referenced range is D1(d)
/// at the rename (the bytes the source lacks cannot read back).
#[test]
fn r2_rename_over_a_shorter_source_is_d1d() {
    let w = xworld();
    let fs = w.fs();
    let mut b = vec![1u8; 256];
    b.resize(512, 0);
    fs.write_all_at(Path::new("/dd/b"), 0, &b).unwrap();
    fs.sync_data(Path::new("/dd/b")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    rec(&w, "/dd/b", 512);
    synced(&w, "/dd/src", 1, 256);
    replace(&w, false);
    assert!(xkinds(&w).contains(&InvariantId::D1d), "{:?}", w.d1_violations());
}

/// Normal recovery reads only the synced file; recovery's read-error handler reads a Buffered
/// key and, if its batch was lost, mutates after the fatal read (GPT's `FaultOnlyBranch`).
struct FaultOnlyBranch(World);

impl Sut for FaultOnlyBranch {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "r2-fault-only-branch";

    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        if w.epoch() == 0 {
            synced_file(w);
        } else {
            w.mark(Mark::RecoveryBegin);
            if w.fs().read_exact_at(Path::new(F), 0, &mut [0; 1]).is_err() {
                if w.engine("probe", &SCHEMA).get(CF, b"k").unwrap().is_none() {
                    w.fs().create(Path::new("/dd/illegal-after-error")).unwrap();
                }
                w.mark(Mark::StoreFailed {
                    op: "read",
                });
                w.mark(Mark::Abort {
                    message: "read failed".into(),
                });
                return Err(Abort::new("read failed"));
            }
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self(w.clone()))
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.0, b"k", Commit::Buffered);
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn sanctioned(_: &String, m: &str) -> bool {
        m == "read failed"
    }
}

/// GPT R2-2: the recovery-fault search reports truncation at its cap, and finds R8 without one.
#[test]
fn r2_recovery_fault_search_reports_truncation() {
    let mut sc = Scenario::<FaultOnlyBranch>::new("r2-cap", String::new(), vec![String::new()]);
    sc.kinds = vec![CrashKind::PowerLoss];
    sc.window.from = EventSel::Nth {
        store: StoreSel::Engine("probe".into()),
        op: dashd_store::faulty::OpKind::Append(dashd_store::faulty::CommitKind::Buffered),
        n: 0,
    };
    sc.recovery_faults = vec![read_fault()];
    let full = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    assert!(full.found().contains(&InvariantId::R8), "{}", full.summary());
    assert_eq!(full.truncated, 0, "{}", full.summary());
    let capped = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1,
        },
    );
    assert!(capped.truncated > 0 && !capped.is_clean(), "{}", capped.summary());
}

/// `loc` needs the block whose file the record `idx` names: a locator-style requirement that
/// resolves through another, possibly undecided, record. On `Unknown` the extractor returns
/// every candidate (`all`) or an error (`err`).
struct ViaIndex(&'static str);

impl RefExtractor for ViaIndex {
    fn refs(
        &self,
        _: &str,
        cf: Cf,
        key: &[u8],
        _: &[u8],
        _: &WorldDurableView,
    ) -> Vec<Requirement> {
        if cf == A && key == b"loc" {
            vec![Requirement::Block {
                hash: [1; 32],
                undo: false,
            }]
        } else {
            vec![]
        }
    }

    fn resolve(&self, req: &Requirement, view: &WorldDurableView) -> Result<Vec<FileRef>, String> {
        let path = |v: Vec<u8>| FileRef::new(String::from_utf8(v).unwrap(), 0, 512);
        match req {
            Requirement::Bytes(r) => Ok(vec![r.clone()]),
            _ => match view.get("m", A, b"idx") {
                dashd_store::faulty::Peek::Known(Some(v)) => Ok(vec![path(v)]),
                dashd_store::faulty::Peek::Known(None) => Err("no index entry".into()),
                dashd_store::faulty::Peek::Unknown if self.0 == "err" => {
                    Err("index entry undecided".into())
                }
                dashd_store::faulty::Peek::Unknown => Ok(view
                    .records("m")
                    .into_iter()
                    .filter(|(cf, k, _, _)| *cf == A && k == b"idx")
                    .map(|(_, _, v, _)| path(v))
                    .collect()),
            },
        }
    }
}

/// Opus cp1 r2 minor B: a record whose requirement read an undecided record is itself treated
/// as undecided: no D1(b) false positive at boot, its ranges protected until the read is
/// decided, and no stale protection after it.
#[test]
fn r2_extractor_reads_of_undecided_records_are_tracked() {
    for mode in ["err", "all"] {
        let x: Arc<dyn RefExtractor> = Arc::new(ViaIndex(mode));
        let w = World::new(WorldConfig::default());
        w.set_extractor(x.clone());
        synced(&w, "/dd/f1", 1, 512);
        synced(&w, "/dd/f2", 2, 512);
        xput(&w, b"idx", b"/dd/f1", Commit::Durable);
        xput(&w, b"loc", b"", Commit::Durable);
        xput(&w, b"idx", b"/dd/f2", Commit::Buffered);
        assert!(xkinds(&w).is_empty(), "{:?}", w.d1_violations());
        let w2 =
            w.capture().boot(CrashKind::PowerLoss, Box::new(AllSurvive), WorldConfig::default());
        assert!(
            xkinds(&w2).is_empty(),
            "{mode}: no false positive at boot: {:?}",
            w2.d1_violations()
        );
        if mode == "all" {
            // Undecided: the candidate f1 is protected.
            assert!(w2.durable_view().range_durable(&FileRef::new("/dd/f1", 0, 512)).is_ok());
        }
        // Recovery reads idx (decided: f2), then truncates the stale f1 (fine) and f2 (not).
        let _ = w2.engine("m", &M).get(A, b"idx").unwrap();
        w2.fs().truncate(Path::new("/dd/f1"), 0).unwrap();
        assert!(xkinds(&w2).is_empty(), "{mode}: stale f1 is free: {:?}", w2.d1_violations());
        w2.fs().truncate(Path::new("/dd/f2"), 0).unwrap();
        assert!(xkinds(&w2).contains(&InvariantId::D1d), "{mode}: {:?}", w2.d1_violations());
    }
}

/// I31: a failed (poisoned) write sharing a sector with synced bytes leaves those bytes durable
/// (A-PSOW): D1(a) compares the range's bytes, as it does for any pending write.
#[test]
fn poisoned_write_in_a_shared_sector_leaves_the_synced_range_durable() {
    use dashd_store::faulty::{Fault, FaultKind, FaultOutcome, OpKind, StoreSel};
    let w = World::new(WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::WriteAt,
                n: 1,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::Applied,
        }],
        ..WorldConfig::default()
    });
    let fs = w.fs();
    let f = Path::new("/dd/f");
    fs.write_all_at(f, 0, &[7; 516]).unwrap();
    fs.sync_data(f).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    // The second write fails after reaching the page cache, and its sync is poisoned.
    assert!(fs.write_all_at(f, 516, &[9; 100]).is_err());
    let _ = fs.sync_data(f);
    let view = w.durable_view();
    assert!(view.range_durable(&FileRef::new(f, 0, 516)).is_ok(), "the synced bytes stay durable");
    assert!(view.range_durable(&FileRef::new(f, 0, 600)).is_err(), "the failed bytes are not");
}
