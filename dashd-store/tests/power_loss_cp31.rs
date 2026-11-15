//! CP3.2 regressions (reviews/ST3a-cp3.1-sol.md, DEC-148): each reproduction of Sol's
//! confirmation, which fails on 162fe639, and the controls that keep each fix honest.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use dashd_store::faulty::{
    Abort, Adversary, CrashImage, CrashKind, Dfs, FileRef, Fixed, Hash, InvariantId, Mark, Mode,
    Nested, RefExtractor, Reference, Repro, Requirement, Scenario, Sut, Trace, Violation, World,
    WorldConfig, WorldDurableView, explore, replay,
};
use dashd_store::flatfile::FlatFs;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};

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

fn path(name: &str) -> std::path::PathBuf {
    Path::new("/dd").join(name)
}

// ---------------------------------------------------------------------------------------------
// CP3.1-1: an interrupted recovery whose projection differs passes R7a only if a reference
// reproduces it (DEC-162). A complete overwrite of an inherited pending write leaves nothing a
// reference could reproduce; a partial one keeps the bytes it preserves, and a reference run
// from the same input reproduces the branch.

pub struct NoRef;
impl Reference for NoRef {
    type Config = String;
    type Op = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}

/// CP3's faulty finalizer: Buffered `flag`, then Durable `done`; "fixed" syncs the WAL between
/// them. Its projection byte: 9 once `done` is without `flag`.
fn finalize(w: &World, cfg: &str) -> u8 {
    let e = w.engine("probe", &SCHEMA);
    let done = e.get(CF, b"done").unwrap().is_some();
    let flag = e.get(CF, b"flag").unwrap().is_some();
    if !done {
        put(w, b"flag", b"v", Commit::Buffered);
        if cfg.ends_with("fixed") {
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

/// The forward run of every probe: a durable baseline, then 512 bytes of 7 in `/dd/scratch`,
/// unsynced (its directory synced), which the outer process kill keeps pending.
fn forward(w: &World) {
    put(w, b"baseline", b"v", Commit::Durable);
    w.fs().write_all_at(&path("scratch"), 0, &[7; 512]).unwrap();
    w.fs().sync_dir(Path::new("/dd")).unwrap();
}

/// Sol's probe: recovery promotes the engine, overwrites all 512 bytes of the scratch sector with
/// 8, reads them back (they no longer hold the inherited 7s) and ignores them, then runs the
/// finalizer. Once `done` exists, it also reads the scratch file before overwriting it. "noread"
/// skips the read after the overwrite.
struct Overwriter {
    p: Hash,
    w: World,
}

impl Sut for Overwriter {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp31-overwriter";

    fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            e.sync_wal().unwrap();
            if e.get(CF, b"done").unwrap().is_some() {
                let _ = w.fs().read_exact_at(&path("scratch"), 0, &mut [0; 512]);
            }
            w.fs().write_all_at(&path("scratch"), 0, &[8; 512]).unwrap();
            if cfg != "noread" {
                let mut data = [0; 512];
                w.fs().read_exact_at(&path("scratch"), 0, &mut data).unwrap();
                assert_eq!(data, [8; 512]);
            }
            p[0] = finalize(w, cfg);
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        forward(&self.w);
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

/// The positive control: recovery overwrites byte 0 only, then derives a durable result from byte
/// 1, which still holds the inherited 7: the divergence §5.5 permits before stabilisation.
struct PartialDeriver {
    p: Hash,
    w: World,
}

impl Sut for PartialDeriver {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp31-partial-deriver";

    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            e.sync_wal().unwrap();
            if let Some(v) = e.get(CF, b"result").unwrap() {
                p[0] = v[0];
            } else {
                w.fs().write_all_at(&path("scratch"), 0, &[8]).unwrap();
                // A crash can leave the scratch file shorter: its byte 1 then reads as nothing.
                let mut data = [0; 2];
                let _ = w.fs().read_exact_at(&path("scratch"), 0, &mut data);
                p[0] = data[1];
                put(w, b"result", &[data[1]], Commit::Durable);
            }
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        forward(&self.w);
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

/// The binding analogue: the forward run leaves `/dd/name` created but its directory unsynced;
/// recovery replaces the name by renaming a fresh file over it, looks it up (the lookup sees
/// only the rename's binding), and runs the finalizer.
struct Rebinder {
    p: Hash,
    w: World,
}

impl Sut for Rebinder {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp31-rebinder";

    fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            let e = w.engine("probe", &SCHEMA);
            e.sync_wal().unwrap();
            let fs = w.fs();
            fs.write_all_at(&path("tmp"), 0, &[8; 16]).unwrap();
            fs.rename(&path("tmp"), &path("name")).unwrap();
            assert_eq!(fs.len(&path("name")).unwrap(), Some(16));
            p[0] = finalize(w, cfg);
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        put(&self.w, b"baseline", b"v", Commit::Durable);
        self.w.fs().write_all_at(&path("name"), 0, &[7; 16]).unwrap();
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

fn r7a_found(v: &[Violation]) -> bool {
    v.iter().any(|v| v.invariant == InvariantId::R7a)
}

/// A replay: the outer process kill after forward event `at`, the inner power loss after the
/// recovery's event `j`, with `decisions`.
fn nested<S: Sut<Config = String, Op = String>>(
    cfg: &str,
    at: u64,
    j: u64,
    decisions: &[(&str, u32)],
) -> Vec<Violation> {
    let sc = Scenario::<S>::new("cp31", cfg.into(), vec![String::new()]);
    let mut r = Repro::new::<S>(&sc, None);
    r.crash_at = Some(at);
    r.kind = CrashKind::ProcessKill;
    r.nested = Some((j, CrashKind::PowerLoss));
    r.decisions.extend(decisions.iter().map(|(n, c)| (n.to_string(), *c)));
    replay::<S>(&r).unwrap()
}

fn search<S: Sut<Config = String, Op = String>>(cfg: &str) -> dashd_store::faulty::Report {
    let mut sc = Scenario::<S>::new("cp31", cfg.into(), vec![String::new()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    )
}

/// Sol's reproduction: the inner crash loses `flag` and selects the scratch sector's durable
/// zeros (choice 2), a version the interrupted recovery never read: it read only its own 8s.
#[test]
fn cp31_1_overwritten_input_cannot_exempt_the_bug() {
    let got = nested::<Overwriter>("bug", 3, 3, &[("2:probe:b1", 1), ("2:fs:i1:u0^", 2)]);
    assert!(r7a_found(&got), "an overwritten inherited input hid the finalizer bug: {got:?}");
}

/// Sol's controls: without the read after the overwrite the bug is reported; with the barrier
/// the same branch is clean.
#[test]
fn cp31_1_overwriter_controls() {
    let got = nested::<Overwriter>("noread", 3, 3, &[("2:probe:b1", 1), ("2:fs:i1:u0^", 2)]);
    assert!(r7a_found(&got), "noread: {got:?}");
    let got = nested::<Overwriter>("fixed", 3, 4, &[("2:probe:b1", 1), ("2:fs:i1:u0^", 2)]);
    assert!(got.is_empty(), "fixed: {got:?}");
}

/// The search finds the bug, and the fixed finalizer is clean.
#[test]
fn cp31_1_search_finds_the_overwriter_bug() {
    let bug = search::<Overwriter>("bug");
    eprintln!("bug: {}", bug.summary());
    assert!(bug.findings.iter().any(|f| f.violation.invariant == InvariantId::R7a));
    let fixed = search::<Overwriter>("fixed");
    eprintln!("fixed: {}", fixed.summary());
    assert!(fixed.is_clean(), "{}", fixed.summary());
}

/// The positive control: byte 1 still holds the inherited 7 after a partial overwrite, so a
/// branch that changes it derives from the changed input, which a reference run from the same
/// input reproduces (DEC-162), and the search reports nothing.
#[test]
fn cp31_1_partial_overwrite_keeps_the_dependency() {
    let got = nested::<PartialDeriver>("", 3, 2, &[("2:fs:i1:u0^", 2)]);
    assert!(got.is_empty(), "{got:?}");
    let r = search::<PartialDeriver>("");
    eprintln!("{}", r.summary());
    assert!(r.is_clean(), "{}", r.summary());
    let witnessed = r.r7a.witnessed_after + r.r7a.witnessed_before;
    assert!(witnessed > 0, "a changed inherited byte was witnessed: {}", r.summary());
}

/// The binding audit: the lookup after the rename sees only the rename's binding, so no
/// reference reproduces the finalizer bug's branches, and the search finds it.
#[test]
fn cp31_1_rebound_name_cannot_exempt_the_bug() {
    let bug = search::<Rebinder>("bug");
    eprintln!("bug: {}", bug.summary());
    assert!(
        bug.findings.iter().any(|f| f.violation.invariant == InvariantId::R7a),
        "{}",
        bug.summary()
    );
    let fixed = search::<Rebinder>("fixed");
    assert!(fixed.is_clean(), "{}", fixed.summary());
}

// ---------------------------------------------------------------------------------------------
// CP3.1-2: a block holding durable bytes and an unsynced tail may read back as EIO (Sol's native
// ext4 and XFS controls). The durable bytes stay unchanged but unavailable; every read that
// succeeds returns them, and D1 accepts them (DEC-148 (2)).

fn extended() -> WorldConfig {
    WorldConfig {
        adversary: Adversary::Extended,
        ..WorldConfig::default()
    }
}

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

fn read(w: &World, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0; len];
    w.fs().read_exact_at(&path("f"), offset, &mut buf).map(|_| buf).map_err(|e| e.to_string())
}

/// 512 synced bytes of `0x11` in `/dd/f`, then `tail` unsynced bytes of `0x22` after them in the
/// same block: Sol's native layout with `tail` 3,584.
fn mixed_block(tail: usize) -> CrashImage {
    let w = World::new(extended());
    let fs = w.fs();
    fs.write_all_at(&path("f"), 0, &[0x11; 512]).unwrap();
    fs.sync_data(&path("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&path("f"), 512, &vec![0x22; tail]).unwrap();
    w.capture()
}

fn boot(img: &CrashImage, eio: u32) -> World {
    let choices = BTreeMap::from([("1:fs:i1:eio0".to_string(), eio)]);
    img.boot(CrashKind::PowerLoss, Box::new(Fixed::new(choices)), extended())
}

/// Sol's test: the unreadable tail sector fails a buffered read of the synced prefix, and D1
/// still counts the prefix durable (unavailable, not lost).
#[test]
fn cp31_2_synced_prefix_unsynced_tail_may_be_eio() {
    let w = boot(&mixed_block(3584), 1);
    assert!(read(&w, 0, 512).is_err(), "native buffered pread(0, 512) fails");
    let r = w.durable_view().range_durable(&FileRef::new("/dd/f", 0, 512));
    assert_eq!(r, Ok(()), "the durable prefix is unchanged, though unavailable");
    w.set_extractor(Arc::new(Loc));
    put(&w, b"loc", b"v", Commit::Durable);
    w.evaluate_d1();
    assert!(w.d1_violations().is_empty(), "{:?}", w.d1_violations());
}

/// Every branch: the prefix reads back as EIO or as its durable bytes, never anything else, and
/// D1 is silent; some branches make it unavailable.
#[test]
fn cp31_2_every_read_is_eio_or_the_durable_bytes() {
    let img = mixed_block(3584);
    let dfs = Dfs::new();
    let (mut branches, mut eio) = (0, 0);
    loop {
        let w = img.boot(CrashKind::PowerLoss, dfs.decider(), extended());
        match read(&w, 0, 512) {
            Ok(b) => assert_eq!(b, vec![0x11; 512], "branch {branches}"),
            Err(e) => {
                assert!(e.contains("EIO"), "branch {branches}: {e}");
                eio += 1;
            }
        }
        w.set_extractor(Arc::new(Loc));
        put(&w, b"loc", b"v", Commit::Durable);
        w.evaluate_d1();
        assert!(w.d1_violations().is_empty(), "branch {branches}: {:?}", w.d1_violations());
        branches += 1;
        if !dfs.advance() {
            break;
        }
    }
    eprintln!("{branches} branches, {eio} with the prefix unavailable");
    assert!(eio > 0, "the mixed block can be unreadable");
}

/// Zeroing the rest of an unreadable block reads it first: a truncate or an extension from a
/// size inside it fails rather than zero the durable bytes; on a readable block both succeed
/// and keep them.
#[test]
fn cp31_2_zeroing_part_of_an_unreadable_block_fails() {
    let img = mixed_block(1536);
    // EOF 2048, inside block 0.
    type Op = fn(&World) -> std::io::Result<()>;
    let ops: [(&str, Op); 3] = [
        ("truncate inside", |w| w.fs().truncate(&path("f"), 1024)),
        ("extend by set_len", |w| w.fs().truncate(&path("f"), 6000)),
        ("extend by a write past EOF", |w| w.fs().write_all_at(&path("f"), 6000, &[0x33; 8])),
    ];
    for (what, op) in ops {
        let w = boot(&img, 1);
        assert!(op(&w).is_err(), "{what}: the block is read first");
        let w = boot(&img, 0);
        op(&w).unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(read(&w, 0, 512), Ok(vec![0x11; 512]), "{what}");
    }
}

/// A recovery that reads a pending block a power loss can make unreadable. "swallow" ignores
/// the EIO and carries on (writing `done`); "fail-stop" aborts with its sanctioned message.
struct EioReader {
    w: World,
}

const READ_FAILED: &str = "a block-file read failed";

impl Sut for EioReader {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp31-eio-reader";

    fn open(w: &World, cfg: &String) -> Result<Self, Abort> {
        if w.epoch() > 0 {
            w.mark(Mark::RecoveryBegin);
            if w.fs().read_exact_at(&path("f"), 0, &mut [0; 512]).is_err() && cfg == "fail-stop" {
                w.mark(Mark::Abort {
                    message: READ_FAILED.into(),
                });
                return Err(Abort::new(READ_FAILED));
            }
            put(w, b"done", b"v", Commit::Durable);
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self {
            w: w.clone(),
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        fs.create(&path("f")).unwrap();
        fs.write_all_at(&path("f"), 0, &[0x11; 512]).unwrap();
        fs.sync_data(&path("f")).unwrap();
        fs.sync_dir(Path::new("/dd")).unwrap();
        fs.write_all_at(&path("f"), 512, &[0x22; 3584]).unwrap();
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }

    fn sanctioned(_: &String, message: &str) -> bool {
        message == READ_FAILED
    }
}

/// The required engine behaviour (DEC-148 (2)): a recovery read of durable bytes the mixed block
/// makes unavailable fails, and an engine that carries on regardless violates R8 (§4.4).
#[test]
fn cp31_2_recovery_must_fail_stop_on_unavailable_bytes() {
    let run = |cfg: &str| {
        let mut sc = Scenario::<EioReader>::new("cp31-eio", cfg.into(), vec![String::new()]);
        sc.adversary = Adversary::Extended;
        sc.kinds = vec![CrashKind::PowerLoss];
        explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 1024,
            },
        )
    };
    let swallow = run("swallow");
    assert!(
        swallow.findings.iter().any(|f| f.violation.invariant == InvariantId::R8),
        "{}",
        swallow.summary()
    );
    let fail_stop = run("fail-stop");
    assert!(fail_stop.findings.is_empty(), "{}", fail_stop.summary());
}

// ---------------------------------------------------------------------------------------------
// CP3.1-3: the nightly fails when it cannot write a required artifact.

#[cfg(target_os = "linux")]
#[test]
fn cp31_3_nightly_fails_when_its_summary_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let runner = std::env::var("ST3A_NIGHTLY_RUNNER").unwrap_or_else(|_| {
        format!("{}/../contrib/node/st3a-nightly.sh", env!("CARGO_MANIFEST_DIR"))
    });
    let dir = std::env::temp_dir().join(format!("st3a-nightly-blocked-{}", std::process::id()));
    let bins = dir.join("bin");
    std::fs::create_dir_all(&bins).unwrap();
    let stub = "#!/bin/sh\necho 'running 1 test'\necho 'test result: ok. 1 passed; 0 failed'\n";
    for name in ["power_loss_deep", "power_loss_model"] {
        let path = bins.join(name);
        std::fs::write(&path, stub).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let run = |out: &Path| {
        Command::new("bash")
            .arg(&runner)
            .arg(out)
            .arg("sweep")
            .env("ST3A_NIGHTLY_BIN_DIR", &bins)
            .env("ST3A_SWEEP_SEED", "20261009")
            .output()
            .unwrap()
    };
    // Control: a passing component with writable output passes.
    let ok = dir.join("ok");
    let r = run(&ok);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
    // Sol's control: a directory where summary.txt goes.
    let blocked = dir.join("blocked");
    std::fs::create_dir_all(blocked.join("summary.txt")).unwrap();
    let r = run(&blocked);
    assert!(!r.status.success(), "the summary was not written, yet the nightly passed");
    // A component log that cannot be written fails its component too.
    let no_log = dir.join("no-log");
    std::fs::create_dir_all(no_log.join("sweep.log")).unwrap();
    let r = run(&no_log);
    assert!(!r.status.success(), "the component's log was not written, yet the nightly passed");
    std::fs::remove_dir_all(&dir).unwrap();
}
