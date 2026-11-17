//! CP3.3 (DEC-162): R7a's witness rule. A nested branch whose projection differs from the
//! uninterrupted one passes only if a reference, run uninterrupted under the same complete input
//! assignment, reproduces it; provenance only selects references, and never grants a pass.
//!
//! - The review's probes (CP3.2-1/-2): an earlier crash's cell, copied or under a Write-granularity
//!   cut, that the interrupted recovery never decides no longer exempts an independent missing
//!   barrier (`CopyFinalizer`, `FloorFinalizer`), with their controls.
//! - CP3.2-3: a failed advisory allocation raises no size floor (`SizeDeriver` converges; the
//!   floors are gone with the provenance machinery).
//! - DEC-162 (2): a branch with more completions than the bound is unjudged, never passed.
//! - DEC-162 (4): the toy's startup replay stops on an unreadable block with Core's replay message.
//! - The validator's outcome hole (CP3.3 r1): a reference witnesses a branch only by ending
//!   exactly as it did. An interrupted recovery's abort is not witnessed by a reference that
//!   aborted with another message or panicked (`Resumer`), nor converges with an outer recovery
//!   that aborted differently.
//! - The validator's projection hole (CP3.3 r2): a branch never converges while an outer cell only
//!   the uninterrupted projection decided stays undecided by it (`Proj`); the finding replays.
//! - DEC-165 (1): a branch that completes where the uninterrupted recovery fail-stopped sanctioned
//!   is allowed and counted apart if every other check passes on it, and fails R7a otherwise
//!   (`Restarter`). So is one that fail-stops again, sanctioned, where no reference reproduces it
//!   (I42), with the S1 V23.1 nightly finding that prompted it.
//! - DEC-165 (2) as DEC-185 (1) amends it: a nested inner image past the cap may be sampled with
//!   a recorded seed, its branches judged like explored ones, but it stays truncation and the
//!   report is never clean; the sample is a smoke. A completion after a stop with no fatal error
//!   is not exempt.
//! - DEC-185 (3): Sol's `Skew` tree, where the capped walk and its sample both miss a dense
//!   subtree: the capped run is truncated, never clean, and the sharded exhaustive enumeration
//!   finds its R1.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use dashd_store::faulty::toy::{ToyChain, ToyConfig, ToyOp};
use dashd_store::faulty::{
    Abort, Adversary, AllSurvive, AllocateMode, CrashImage, CrashKind, EventSel, FS_STORE,
    FatalCause, Fault, FaultKind, FaultOutcome, Granularity, Hash, InvariantId, Mark, Mode, Nested,
    OpKind, OverCap, Reference, Report, Repro, Scenario, StoreSel, Sut, Trace, TraceEntry,
    Violation, World, WorldConfig, explore, explore_shard, forward_trace, replay, reproduces,
    shards,
};
use dashd_store::flatfile::FlatFs;

#[path = "power_loss/shard_check.rs"]
mod shard_check;
use dashd_store::{Cf, CfProfile, CfSpec, Commit, DbSchema, KvBatch, KvEngine, KvRead};

static SCHEMA: DbSchema = DbSchema {
    name: "probe",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};
const CF: Cf = Cf::new(0);

fn put(w: &World, k: &[u8], v: &[u8], commit: Commit) {
    let e = w.engine("probe", &SCHEMA);
    let mut b = e.batch();
    b.put(CF, k, v);
    e.write(b, commit).unwrap();
}

fn p(n: &str) -> std::path::PathBuf {
    Path::new("/dd").join(n)
}

pub struct NoRef;
impl Reference for NoRef {
    type Config = String;
    type Op = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}

/// An earlier power-loss cell: `src`'s first `zero_bytes` are zeros at choice 0, sevens at
/// choice 1. A lazy copy makes it an inherited pending input of `f` over a durable zero block.
fn setup(w: &World, zero_bytes: usize) {
    let fs = w.fs();
    put(w, b"baseline", b"v", Commit::Durable);
    fs.write_all_at(&p("src"), 0, &[7; 4096]).unwrap();
    fs.sync_data(&p("src")).unwrap();
    fs.write_all_at(&p("f"), 0, &[0; 4096]).unwrap();
    fs.sync_data(&p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("src"), 0, &vec![0; zero_bytes]).unwrap();
    w.crash_in_place(CrashKind::PowerLoss, Box::new(AllSurvive));
    fs.copy(&p("src"), &p("f")).unwrap();
    fs.truncate(&p("f"), 4096).unwrap();
    // Event numbers unique across the earlier epoch as well.
    for _ in 0..12 {
        fs.create(&p("f")).unwrap();
    }
}

/// The finalizer's missing barrier: the recovery's `flag` is Buffered under its Durable `done`,
/// so a power loss can keep `done` without `flag`, and the next recovery projects 9 where the
/// uninterrupted one projected 0. The finalizer never depends on `f`. Configs: "bug" copies `f`
/// (taking the earlier cell into the copy without deciding it), "nocopy" does not, "fixed" syncs
/// the WAL between the two, "decide-old" also reads `src` (resolving the earlier cell).
/// `floor`: the Write-granularity variant (Sol's `FloorFinalizer`), which reads `f`'s size.
fn finalize(w: &World, c: &str, floor: bool) -> Hash {
    let mut p0 = [0; 32];
    if w.epoch() > 1 {
        let e = w.engine("probe", &SCHEMA);
        e.sync_wal().unwrap();
        let done = e.get(CF, b"done").unwrap().is_some();
        let flag = e.get(CF, b"flag").unwrap().is_some();
        let fs = w.fs();
        if floor {
            if done {
                let _ = fs.len(&p("f"));
                if c == "decide-old" {
                    let _ = fs.read_exact_at(&p("src"), 512, &mut [0; 1]);
                }
            } else {
                let _ = fs.write_all_at(&p("f"), 8192, &[3; 100]);
                let _ = fs.read_exact_at(&p("f"), 512, &mut [0; 1]);
            }
        } else {
            if done {
                let _ = fs.read_exact_at(&p("f"), 0, &mut [0; 512]);
                if c == "decide-old" {
                    let _ = fs.read_exact_at(&p("src"), 0, &mut [0; 512]);
                }
            }
            if c != "nocopy" {
                let _ = fs.copy(&p("f"), &p("g"));
            }
        }
        if done && !flag {
            p0[0] = 9;
        }
        if !done {
            put(w, b"flag", b"v", Commit::Buffered);
            if c == "fixed" {
                e.sync_wal().unwrap();
            }
            put(w, b"done", b"v", Commit::Durable);
        }
    }
    p0
}

macro_rules! finalizer {
    ($name:ident, $floor:expr) => {
        pub struct $name {
            w: World,
            p: Hash,
        }
        impl Sut for $name {
            type Config = String;
            type Op = String;
            type Ref = NoRef;
            const NAME: &'static str = stringify!($name);
            fn open(w: &World, c: &String) -> Result<Self, Abort> {
                Ok(Self {
                    w: w.clone(),
                    p: finalize(w, c, $floor),
                })
            }
            fn apply(&mut self, _: &String) -> Result<(), Abort> {
                if $floor {
                    setup(&self.w, 4096);
                    self.w.fs().truncate(&p("f"), 512).unwrap();
                } else {
                    setup(&self.w, 512);
                }
                Ok(())
            }
            fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
                Ok(())
            }
            fn projection(&self) -> Option<Hash> {
                Some(self.p)
            }
        }
    };
}
finalizer!(CopyFinalizer, false);
finalizer!(FloorFinalizer, true);

/// The review's repro: an outer process kill after the forward run (event `at`), then a power
/// loss after the recovery's event `j` that keeps `done` and loses `flag` (`3:probe:b1`) and
/// changes the inherited input `cell`.
fn repro<S: Sut<Config = String, Op = String>>(
    c: &str,
    at: u64,
    j: u64,
    cell: (&str, u32),
) -> Repro {
    let sc = Scenario::<S>::new("cp33-witness", c.into(), vec![String::new()]);
    let mut r = Repro::new::<S>(&sc, None);
    r.crash_at = Some(at);
    r.kind = CrashKind::ProcessKill;
    r.nested = Some((j, CrashKind::PowerLoss));
    r.decisions.insert("3:probe:b1".into(), 1);
    r.decisions.insert(cell.0.into(), cell.1);
    r
}

fn r7a(r: &Repro, replay: fn(&Repro) -> Result<Vec<Violation>, String>) -> bool {
    let got = replay(r).unwrap();
    eprintln!("{} {:?}: {got:?}", r.config, r.nested);
    got.iter().any(|v| v.invariant == InvariantId::R7a)
}

/// CP3.2-1: the copied earlier cell, never decided by the interrupted recovery, exempted the
/// missing barrier. Under DEC-162 neither reference projects 9: R7a reports it, also when the
/// branch resolves the earlier cell ("decide-old"); the controls behave as before.
#[test]
fn cp33_copied_earlier_cell_no_longer_masks_the_finalizer() {
    let cell = ("3:fs:i2:u0^", 1);
    for (c, j, want) in
        [("bug", 3, true), ("decide-old", 3, true), ("nocopy", 2, true), ("fixed", 4, false)]
    {
        let r = repro::<CopyFinalizer>(c, 13, j, cell);
        assert_eq!(r7a(&r, replay::<CopyFinalizer>), want, "copy {c}");
    }
}

/// CP3.2-2: the Write-granularity cut over the earlier size cell (I36's undecided earlier cell
/// counted as observed) exempted the same missing barrier. R7a reports it now; controls as before.
#[test]
fn cp33_undecided_earlier_size_no_longer_masks_the_finalizer() {
    let cell = ("3:fs:i2:w^", 2);
    for (c, j, want) in [("bug", 3, true), ("decide-old", 3, true), ("fixed", 4, false)] {
        let mut r = repro::<FloorFinalizer>(c, 14, j, cell);
        r.granularity = Granularity::Write;
        assert_eq!(r7a(&r, replay::<FloorFinalizer>), want, "floor {c}");
    }
}

/// The searches: each finalizer bug is found, the fixed finalizers are clean, and nothing is left
/// unjudged.
#[test]
fn cp33_finalizer_searches() {
    fn search<S: Sut<Config = String, Op = String>>(
        c: &str,
        g: Granularity,
    ) -> dashd_store::faulty::Report {
        let mut sc = Scenario::<S>::new("cp33-witness", c.into(), vec![String::new()]);
        // After the forward run: the finalizers recover only its complete state.
        sc.window.from = EventSel::Index(if g == Granularity::Write {
            14
        } else {
            13
        });
        sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
        sc.granularity = g;
        sc.nested = Nested::Depth1;
        let r = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        );
        eprintln!("{} {c}: {}", S::NAME, r.summary());
        r
    }
    for (c, bug) in [("bug", true), ("fixed", false)] {
        let copy = search::<CopyFinalizer>(c, Granularity::Sector);
        let floor = search::<FloorFinalizer>(c, Granularity::Write);
        for r in [&copy, &floor] {
            assert_eq!(r.found().contains(&InvariantId::R7a), bug, "{c}: {}", r.summary());
            assert_eq!(r.r7a.unjudged, 0, "{c}: {}", r.summary());
            if !bug {
                assert!(r.is_clean(), "{c}: {}", r.summary());
            }
        }
    }
}

/// CP3.2-3's probe: a durably memoised size read from inherited pending metadata, under an
/// advisory allocation that fails (`Extended`). The branch converges to the uninterrupted size
/// (1,024 bytes): no floor is recorded for a failed allocation, nor any floor at all now.
pub struct SizeDeriver {
    w: World,
    p: Hash,
}

fn size_setup(w: &World) {
    let fs = w.fs();
    fs.write_all_at(&p("src"), 0, &[7; 512]).unwrap();
    fs.sync_data(&p("src")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    fs.write_all_at(&p("src"), 512, &[8; 512]).unwrap();
    let eio = BTreeMap::from([("1:fs:i1:eio0".to_string(), 1)]);
    w.crash_in_place(CrashKind::PowerLoss, Box::new(dashd_store::faulty::Fixed::new(eio)));
    assert!(fs.read_exact_at(&p("src"), 0, &mut [0; 1]).is_err());
    // The copy carries the earlier cells (EIO included) into a pending file of 1,024 bytes.
    fs.copy(&p("src"), &p("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    for _ in 0..12 {
        fs.create(&p("f")).unwrap();
    }
}

impl Sut for SizeDeriver {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-size-deriver";
    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        let mut p0 = [0; 32];
        if w.epoch() > 1 {
            w.mark(Mark::RecoveryBegin);
            let e = w.engine("probe", &SCHEMA);
            e.sync_wal().unwrap();
            let n = match e.get(CF, b"result").unwrap() {
                Some(v) => {
                    w.fs().len(&p("f")).unwrap();
                    u64::from_le_bytes(v.try_into().unwrap())
                }
                None => {
                    w.fs().allocate(&p("f"), 0, 8192);
                    let n = w.fs().len(&p("f")).unwrap().unwrap();
                    put(w, b"result", &n.to_le_bytes(), Commit::Durable);
                    n
                }
            };
            p0[..8].copy_from_slice(&n.to_le_bytes());
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self {
            w: w.clone(),
            p: p0,
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        size_setup(&self.w);
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

#[test]
fn cp33_failed_advisory_allocation_sets_no_floor() {
    for mode in [AllocateMode::Fallocate, AllocateMode::Fail] {
        let sc = Scenario::<SizeDeriver>::new("cp33-size", String::new(), vec![String::new()]);
        let mut r = Repro::new::<SizeDeriver>(&sc, None);
        r.adversary = Adversary::Extended;
        r.allocate = mode;
        r.crash_at = Some(13);
        r.kind = CrashKind::ProcessKill;
        r.nested = Some((3, CrashKind::PowerLoss));
        r.decisions.insert("3:fs:i2:len^".into(), 1);
        let got = replay::<SizeDeriver>(&r).unwrap();
        assert!(got.is_empty(), "{mode:?}: {got:?}");
    }
}

/// Two inherited files, both pending across the outer process kill: the first recovery reads
/// `x` (ignoring it) and `y`, projects `y`'s first byte, and makes `marker` durable; a recovery
/// that finds the marker reads only `y`. A branch whose inner power loss loses `y` after the
/// marker projects 0, which reference (b) reproduces whatever `x` holds: `x`'s cells, left
/// undecided by the branch, are enumerated with every value.
pub struct Reader {
    w: World,
    p: Hash,
}

impl Sut for Reader {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-reader";
    fn open(w: &World, _: &String) -> Result<Self, Abort> {
        let mut p0 = [0; 32];
        if w.epoch() > 0 {
            let fs = w.fs();
            let first = |f: &str| {
                let mut b = [0];
                fs.read_exact_at(&p(f), 0, &mut b).map_or(0, |()| b[0])
            };
            if fs.len(&p("marker")).ok().flatten().is_none() {
                first("x");
                p0[0] = first("y");
                let _ = fs.write_all_at(&p("marker"), 0, &[1]);
                let _ = fs.sync_data(&p("marker"));
                let _ = fs.sync_dir(Path::new("/dd"));
            } else {
                p0[0] = first("y");
            }
        }
        Ok(Self {
            w: w.clone(),
            p: p0,
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        fs.write_all_at(&p("x"), 0, &[6; 512]).unwrap();
        fs.write_all_at(&p("y"), 0, &[5; 512]).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

/// DEC-162 (2): a branch whose completions exceed the bound is unjudged, never passed, and the
/// report is not clean; with the default bound every completion is witnessed and it is clean.
#[test]
fn cp33_over_the_bound_is_unjudged() {
    let search = |bound: u32| {
        let mut sc = Scenario::<Reader>::new("cp33-bound", String::new(), vec![String::new()]);
        sc.window.from = EventSel::Index(1);
        sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
        sc.nested = Nested::Depth1;
        sc.r7a_assignments = bound;
        let r = explore(
            &sc,
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        );
        eprintln!("bound {bound}: {}", r.summary());
        r
    };
    let tight = search(1);
    assert!(tight.r7a.unjudged > 0, "{}", tight.summary());
    assert!(tight.findings.is_empty() && !tight.is_clean(), "{}", tight.summary());
    let full = search(64);
    assert!(full.is_clean(), "{}", full.summary());
    assert!(full.r7a.witnessed_before > 0, "{}", full.summary());
}

/// DEC-162 (4): the toy's startup replay that cannot read a block stops with Core's replay
/// message (`node/chainstate.cpp:251`). The messages are sanctioned per call path: the replay
/// message only after a failed block-file read inside the replay, and activation's "Failed to
/// read block" never there; a read the system itself found short (missing data, no failed file
/// read) sanctions neither replay message.
#[test]
fn cp33_toy_replay_read_failure_uses_the_replay_message() {
    const REPLAY: &str =
        "Unable to replay blocks. You will need to rebuild the database using -reindex-chainstate.";
    // Split flushes: a crash between B1 and Bk leaves the head marker, and recovery replays.
    let cfg = ToyConfig {
        sub_batches: 2,
        ..ToyConfig::default()
    };
    // DEC-165 (3): the message follows the innermost Core call path of the first fatal error.
    const BLOCK: &str = "Failed to read block";
    const OPEN: &str = "Error opening block database";
    const DB_READ: &str = "Error reading from database, shutting down.";
    const FLUSH: &str = "Failed to write to coin database";
    const DISCONNECT: &str = "Failed to disconnect block; see debug.log for details";
    const LOAD: &str = "LoadChainstate";
    const RP: &str = "ReplayBlocks";
    const CONN: &str = "ConnectTip";
    const DISC: &str = "DisconnectTip";
    const SYNC: &str = "BaseIndex::ThreadSync";
    const APPEND: &str = "CoinStatsIndex::CustomAppend";
    const REWIND: &str = "BaseIndex::Rewind";
    const CS: &str = "chainstate";
    let cause = |read, store: Option<&str>, paths: &[&'static str]| FatalCause {
        read,
        store: store.map(str::to_string),
        paths: paths.to_vec(),
    };
    let fs = Some(FS_STORE);
    let after = |m: &str, c| ToyChain::sanctioned_after(&cfg, m, &c);
    assert!(after(REPLAY, cause(true, fs, &[LOAD, RP])));
    assert!(!after(BLOCK, cause(true, fs, &[LOAD, RP])), "ConnectTip's message inside the replay");
    assert!(!after(REPLAY, cause(true, fs, &[CONN])), "the replay message outside the replay");
    assert!(
        !after(REPLAY, cause(false, None, &[LOAD, RP])),
        "the replay message after a failed write"
    );
    assert!(
        !after(REPLAY, cause(true, None, &[LOAD, RP])),
        "the replay message after missing data"
    );
    assert!(after(OPEN, cause(true, Some(CS), &[LOAD, RP])), "an engine error inside the replay");
    assert!(after(OPEN, cause(false, Some(CS), &[LOAD])));
    assert!(
        !after(FLUSH, cause(false, Some(CS), &[LOAD, RP])),
        "the flush message inside the load"
    );
    assert!(
        !after(DB_READ, cause(true, Some(CS), &[LOAD])),
        "the coins-read message inside the load"
    );
    assert!(after(BLOCK, cause(true, fs, &[CONN])));
    assert!(!after(BLOCK, cause(true, fs, &[])), "ConnectTip's message off its path");
    assert!(after(DB_READ, cause(true, Some(CS), &[CONN])));
    assert!(!after(FLUSH, cause(true, Some(CS), &[CONN])), "the flush message after a coins read");
    assert!(after(DISCONNECT, cause(true, fs, &[DISC])));
    assert!(!after(BLOCK, cause(true, fs, &[DISC])), "ConnectTip's message in a disconnect");
    assert!(!after(REPLAY, cause(true, fs, &[DISC])), "the replay message in a disconnect");
    let h = "ab".repeat(32);
    let read = format!("ThreadSync: Failed to read block {h} from disk");
    let write = format!("ThreadSync: Failed to write block {h} to index database");
    let rewind = "ThreadSync: Failed to rewind index toyindex to a previous chain tip";
    assert!(after(&read, cause(true, fs, &[SYNC])));
    assert!(!after(&write, cause(true, fs, &[SYNC])), "CustomAppend's message for a block read");
    assert!(after(&write, cause(true, fs, &[SYNC, APPEND])));
    assert!(!after(&read, cause(true, fs, &[SYNC, APPEND])), "the block message for an undo read");
    assert!(after(rewind, cause(true, fs, &[SYNC, REWIND])));
    assert!(!after(&read, cause(true, fs, &[SYNC, REWIND])), "a connect's message in a rewind");
    assert!(!after(&read.replace(&h, "zz"), cause(true, fs, &[SYNC])), "a malformed hash");
    assert!(!after(BLOCK, cause(true, fs, &[SYNC])), "ConnectTip's message in the index sync");
    // Inside the load the engine's message wins over the innermost path; elsewhere it decides.
    assert!(after(OPEN, cause(false, Some(CS), &[LOAD, RP])));
    assert!(!after(BLOCK, cause(true, fs, &[LOAD])), "no Core message for a load's block read");
    assert!(after(FLUSH, cause(false, Some(CS), &[])));
    let images = Arc::new(Mutex::new(Vec::<CrashImage>::new()));
    let hook = images.clone();
    let w = World::new(WorldConfig {
        on_event: Some(Box::new(move |w, _| hook.lock().unwrap().push(w.capture()))),
        ..WorldConfig::default()
    });
    let mut toy = ToyChain::open(&w, &cfg).unwrap();
    for op in [ToyOp::Connect(2), ToyOp::Flush, ToyOp::Connect(2), ToyOp::Flush] {
        toy.apply(&op).unwrap();
    }
    let images = std::mem::take(&mut *images.lock().unwrap());
    let read_fault = |n| WorldConfig {
        faults: vec![Fault {
            at: EventSel::Nth {
                store: StoreSel::Fs,
                op: OpKind::Read,
                n,
            },
            kind: FaultKind::Io,
            outcome: FaultOutcome::NotApplied,
        }],
        ..WorldConfig::default()
    };
    let mut stops = 0;
    for img in &images {
        for n in 1..=8 {
            let b = img.boot(CrashKind::ProcessKill, Box::new(AllSurvive), read_fault(n));
            let Err(a) = ToyChain::open(&b, &cfg) else {
                continue;
            };
            let t = b.trace();
            let epoch = b.epoch();
            let read = t
                .entries
                .iter()
                .any(|e| matches!(e, TraceEntry::ReadFailed { epoch: e, .. } if *e == epoch));
            if a.message == REPLAY && read {
                // Marked like every fail-stop: the store failure, then the abort.
                assert!(t.entries.iter().any(|e| matches!(e, TraceEntry::Mark { mark: Mark::Abort { message }, .. } if message == REPLAY)));
                stops += 1;
            }
        }
    }
    assert!(stops > 0, "no startup replay read a block under the fault");
    // The search with a recovery read fault reports nothing: the replay message is sanctioned
    // after the read, and missing data never meets the fault.
    let mut sc = Scenario::<ToyChain>::new(
        "cp33-replay",
        cfg,
        vec![ToyOp::Connect(2), ToyOp::Flush, ToyOp::Connect(2), ToyOp::Flush],
    );
    sc.kinds = vec![CrashKind::ProcessKill];
    sc.recovery_faults = vec![Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::Read,
            n: 1,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    }];
    let r = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 64,
        },
    );
    assert!(
        !r.findings.iter().any(|f| f.violation.detail.contains("Unable to replay")),
        "{}",
        r.summary()
    );
}

const RESUME_FAILED: &str = "the resumed recovery found a lost record";
const FRESH_FAILED: &str = "the recovery found a lost record";

/// A recovery in two steps, `marker` then `done`, each made durable, after which it checks the
/// forward run's record in `f` (pending at the outer crash, so a power loss may lose it). An
/// interrupted recovery (`marker` without `done`) resumes; on a lost record the resumed path
/// aborts with its own message ("abort") or the fresh path panics ("panic"), where the
/// uninterrupted path aborts with Core's. "fixed" resumes like a fresh recovery.
pub struct Resumer {
    w: World,
    p: Hash,
}

impl Resumer {
    fn recover(w: &World, mode: &str) -> Result<u8, Abort> {
        let fs = w.fs();
        let exists = |f: &str| fs.len(&p(f)).unwrap().is_some();
        let resumed = exists("marker") && !exists("done");
        if !exists("marker") {
            for f in ["marker", "done"] {
                fs.write_all_at(&p(f), 0, &[1]).unwrap();
                fs.sync_data(&p(f)).unwrap();
                fs.sync_dir(Path::new("/dd")).unwrap();
            }
        }
        // A record that does not read back (its file lost or short) is lost.
        let mut b = [0];
        let _ = fs.read_exact_at(&p("f"), 0, &mut b);
        let stop = |m: &str| {
            w.mark(Mark::Abort {
                message: m.into(),
            });
            Err(Abort::new(m))
        };
        match b[0] {
            2 => Ok(2),
            _ if resumed && mode != "fixed" => stop(RESUME_FAILED),
            _ if mode == "panic" => panic!("the record is lost"),
            _ => stop(FRESH_FAILED),
        }
    }
}

impl Sut for Resumer {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-resumer";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        let mut p0 = [0; 32];
        if w.epoch() > 0 {
            p0[0] = Self::recover(w, mode)?;
        }
        Ok(Self {
            w: w.clone(),
            p: p0,
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        fs.write_all_at(&p("f"), 0, &[1; 512]).unwrap();
        fs.sync_data(&p("f")).unwrap();
        fs.sync_dir(Path::new("/dd")).unwrap();
        fs.write_all_at(&p("f"), 0, &[2; 512]).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn sanctioned(_: &String, message: &str) -> bool {
        message == RESUME_FAILED || message == FRESH_FAILED
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

fn resumer(mode: &str) -> dashd_store::faulty::Report {
    let mut sc = Scenario::<Resumer>::new("cp33-resumer", mode.into(), vec![String::new()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    let r = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 4096,
        },
    );
    eprintln!("{mode}: {}", r.summary());
    r
}

/// The resumed path's abort is witnessed by no reference: (b) and (a′) both take a path that
/// aborts with the other message. Before the outcome comparison any two non-completions matched.
#[test]
fn cp33_a_different_abort_does_not_witness_an_abort() {
    let r = resumer("abort");
    assert!(
        r.findings.iter().any(|f| f.violation.invariant == InvariantId::R7a
            && f.violation.detail.contains("did not complete")),
        "{}",
        r.summary()
    );
}

/// A panicking reference witnesses nothing (and a panicking branch fails R7a).
#[test]
fn cp33_a_panicking_reference_does_not_witness_an_abort() {
    let r = resumer("panic");
    assert!(r.found().contains(&InvariantId::R7a), "{}", r.summary());
}

/// The control: a resumed recovery that aborts as the uninterrupted one does converges, or is
/// witnessed by the reference that aborts alike.
#[test]
fn cp33_the_same_abort_is_witnessed() {
    let r = resumer("fixed");
    assert!(r.is_clean(), "{}", r.summary());
    assert!(
        r.r7a.converged + r.r7a.witnessed_before + r.r7a.witnessed_after > 0,
        "{}",
        r.summary()
    );
}

/// The forward run leaves `f`'s 2s pending over durable 1s. Recovery never reads `f`; it writes
/// `marker` then `done`, and only the projection reads `f`, so under an outer power loss only the
/// projection decides whether the 2s survived. A resumed recovery ("bug") brings the 2s back by a
/// synced rename, where the uninterrupted one that lost them projects 1s. "fixed" does not.
pub struct Proj {
    w: World,
}

impl Sut for Proj {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-proj";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        if w.epoch() > 0 {
            let fs = w.fs();
            let exists = |f: &str| fs.len(&p(f)).unwrap().is_some();
            let resumed = exists("marker") && !exists("done");
            if !exists("marker") {
                for f in ["marker", "done"] {
                    fs.write_all_at(&p(f), 0, &[1]).unwrap();
                    fs.sync_data(&p(f)).unwrap();
                    fs.sync_dir(Path::new("/dd")).unwrap();
                }
            }
            if resumed && mode == "bug" {
                fs.write_all_at(&p("g"), 0, &[2; 4096]).unwrap();
                fs.sync_data(&p("g")).unwrap();
                fs.rename(&p("g"), &p("f")).unwrap();
                fs.sync_dir(Path::new("/dd")).unwrap();
            }
        }
        Ok(Self {
            w: w.clone(),
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        fs.write_all_at(&p("f"), 0, &[1; 4096]).unwrap();
        fs.sync_data(&p("f")).unwrap();
        fs.sync_dir(Path::new("/dd")).unwrap();
        fs.write_all_at(&p("f"), 0, &[2; 4096]).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn projection(&self) -> Option<Hash> {
        let mut h = [0; 32];
        let _ = self.w.fs().read_exact_at(&p("f"), 0, &mut h[..1]);
        Some(h)
    }
}

/// Crashes after the forward run's last event only: the earlier images diverge through other
/// cells, which the old judge caught too.
fn proj_scenario(mode: &str) -> Scenario<Proj> {
    let mut sc = Scenario::<Proj>::new("cp33-proj", mode.into(), vec![String::new()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    sc.window.from = EventSel::Index(forward_trace(&sc).0.events().last().unwrap().0.index);
    sc
}

fn proj(mode: &str) -> dashd_store::faulty::Report {
    let r = explore(
        &proj_scenario(mode),
        Mode::Exhaustive {
            max_branches_per_image: 4096,
        },
    );
    eprintln!("{mode}: {}", r.summary());
    r
}

/// The resumed recovery diverges only where the projection lost the 2s, a value of a cell the
/// interrupted branch never decides: R7a judges the branch under each value, and the finding
/// replays (replay and the sweep fix the same outer cells as explore).
#[test]
fn cp33_projection_only_cell_does_not_converge() {
    let r = proj("bug");
    let f = r.first(InvariantId::R7a).unwrap_or_else(|| panic!("{}", r.summary()));
    assert!(f.violation.detail.contains("diverged"), "{}", r.summary());
    assert!(dashd_store::faulty::reproduces::<Proj>(&f.repro), "{}", f.repro.to_text());
    let fixed = proj("fixed");
    assert!(fixed.is_clean(), "{}", fixed.summary());
}

/// Each nested search runs once across shards (CP3.4-1): the projection-only cell splits one
/// recovery over shards, and each repeated its nested search (105 branches against 69 at depth 1
/// and past it). "bug"'s findings and R7a counts match too.
#[test]
fn cp34_projection_cells_do_not_repeat_nested_searches() {
    for mode in ["fixed", "bug"] {
        let (full, rows) = shard_check::assert_shards_match(&proj_scenario(mode), &SHARD_DEPTHS);
        assert_eq!(full.findings.is_empty(), mode == "fixed", "{}", full.summary());
        assert!(rows.iter().any(|r| r.shards > 2), "{mode}: the recovery must split over shards");
    }
    assert_eq!(proj("fixed").branches, 69);
}

/// The shard check compares nested branches and their R7a counters per outer image and kind, not
/// only in total (CP3.5-1): "fixed"'s shards with one converged nested branch moved from one image
/// and kind to another keep every total, outer count and finding, and must fail it there.
#[test]
fn cp35_shard_check_catches_a_nested_miscount_between_buckets() {
    let sc = proj_scenario("fixed");
    let full = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: u32::MAX,
        },
    );
    let plan = shards(&sc, 2);
    let mut reports: Vec<Report> = plan.iter().map(|s| explore_shard(&sc, s)).collect();
    shard_check::assert_buckets_owned(&plan, &reports, sc.name, 2);
    shard_check::assert_depth_matches(&full, &reports, sc.name, 2);
    let (r, from) = reports
        .iter_mut()
        .find_map(|r| {
            let (k, _) = r.per_image_nested.iter().find(|(_, t)| t.r7a.converged > 0)?;
            let k = *k;
            Some((r, k))
        })
        .expect("some shard runs a converged nested branch");
    let to = full.per_image_nested.keys().copied().find(|&k| k != from).expect("two buckets");
    let t = r.per_image_nested.get_mut(&from).unwrap();
    t.branches -= 1;
    t.r7a.converged -= 1;
    let t = r.per_image_nested.entry(to).or_default();
    t.branches += 1;
    t.r7a.converged += 1;
    let err = std::panic::catch_unwind(|| {
        shard_check::assert_depth_matches(&full, &reports, sc.name, 2);
    })
    .expect_err("a nested miscount between buckets passed the shard check");
    let msg = err.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(msg.contains("nested branches and R7a counters per image and kind"), "{msg}");
}

/// Depths 0 to past every leaf.
const SHARD_DEPTHS: [usize; 6] = [0, 1, 2, 3, 4, 16];

const STOPPED: &str = "the recovery found its record unreadable";
const STOPPED_AGAIN: &str = "the later start found its record unreadable";

/// The forward run leaves `f`'s 2s pending over durable 1s; under `Extended` a power loss may
/// leave the block unreadable. A fresh recovery writes `marker` durably, then reads `f` and
/// fail-stops sanctioned if the read fails. A later start (`marker` present, as after an inner
/// crash) rebuilds an unreadable (EIO) `f` as 1s and completes, as Core may start after a fail-stop:
/// "ok" accepts it, "bad" completes with a state its check rejects. "voluntary" also stops, with
/// no failed read and the same sanctioned message, on a lost record (1s). "again" has a later
/// start fail-stop, with its own sanctioned message, on the unreadable `f` instead of rebuilding
/// it; "again-bad" also has a store check that rejects every later start.
pub struct Restarter {
    w: World,
    p: Hash,
    lost: bool,
}

impl Sut for Restarter {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-restarter";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        let mut h = [0; 32];
        let mut lost = false;
        if w.epoch() > 0 {
            let fs = w.fs();
            let restart = fs.len(&p("marker")).unwrap().is_some();
            if !restart {
                fs.write_all_at(&p("marker"), 0, &[1]).unwrap();
                fs.sync_data(&p("marker")).unwrap();
                fs.sync_dir(Path::new("/dd")).unwrap();
            }
            // A later start probes `f` before its recovery (a failed read there is not fatal)
            // and rebuilds an unreadable one (not a short or missing one: that stays a refusal).
            let probe = restart.then(|| fs.read_exact_at(&p("f"), 0, &mut h[..1]));
            let unreadable = matches!(&probe, Some(Err(e)) if e.to_string().starts_with("EIO"));
            if unreadable && mode.starts_with("again") {
                w.mark(Mark::RecoveryBegin);
                let r = fs.read_exact_at(&p("f"), 0, &mut h[..1]);
                assert!(r.is_err(), "the probe found `f` unreadable");
                w.mark(Mark::Abort {
                    message: STOPPED_AGAIN.into(),
                });
                return Err(Abort::new(STOPPED_AGAIN));
            }
            if unreadable {
                fs.write_all_at(&p("g"), 0, &[1; 512]).unwrap();
                fs.sync_data(&p("g")).unwrap();
                fs.rename(&p("g"), &p("f")).unwrap();
                fs.sync_dir(Path::new("/dd")).unwrap();
                lost = mode == "bad";
            }
            w.mark(Mark::RecoveryBegin);
            let stop = || {
                w.mark(Mark::Abort {
                    message: STOPPED.into(),
                });
                Err(Abort::new(STOPPED))
            };
            match fs.read_exact_at(&p("f"), 0, &mut h[..1]) {
                Err(_) => return stop(),
                Ok(()) if mode == "voluntary" && h[0] != 2 && !restart => return stop(),
                Ok(()) => {}
            }
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self {
            w: w.clone(),
            p: h,
            lost,
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        fs.write_all_at(&p("f"), 0, &[1; 512]).unwrap();
        fs.sync_data(&p("f")).unwrap();
        fs.sync_dir(Path::new("/dd")).unwrap();
        fs.write_all_at(&p("f"), 0, &[2; 512]).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        if self.lost {
            return Err(vec![Violation::new(InvariantId::R1, "the later start lost the record")]);
        }
        Ok(())
    }
    fn store_check(w: &World, mode: &String) -> Vec<Violation> {
        if mode == "again-bad" && w.fs().len(&p("marker")).unwrap().is_some() {
            return vec![Violation::new(InvariantId::R1, "a later start")];
        }
        Vec::new()
    }
    fn sanctioned(_: &String, message: &str) -> bool {
        message == STOPPED || message == STOPPED_AGAIN
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

fn restarter_capped(
    mode: &str,
    cap: u32,
    over_cap: Option<OverCap>,
) -> dashd_store::faulty::Report {
    let mut sc = Scenario::<Restarter>::new("cp33-restarter", mode.into(), vec![String::new()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.nested = Nested::Depth1;
    sc.adversary = Adversary::Extended;
    sc.over_cap = over_cap;
    explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: cap,
        },
    )
}

/// The shards at any depth partition the exhaustive search (DEC-185 (2)): over Restarter's
/// images, both kinds, the same branches per image, R7a counts and findings as unsharded.
#[test]
fn cp33_shards_partition_the_search() {
    for mode in ["ok", "bad"] {
        let mut sc = Scenario::<Restarter>::new("cp33-restarter", mode.into(), vec![String::new()]);
        sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
        sc.nested = Nested::Depth1;
        sc.adversary = Adversary::Extended;
        let (full, _) = shard_check::assert_shards_match(&sc, &SHARD_DEPTHS);
        assert_eq!(full.branches, 222, "{}", full.summary());
    }
}

fn restarter(mode: &str) -> dashd_store::faulty::Report {
    let r = restarter_capped(mode, 4096, None);
    eprintln!("{mode}: {}", r.summary());
    r
}

/// No reference reproduces the completion (both fail-stop), so the witness rule alone reports it
/// as diverged; DEC-165 (1) allows it because its completed state passes every other check.
#[test]
fn cp33_completion_after_a_fail_stop_is_allowed() {
    let r = restarter("ok");
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.recovered_after_failstop > 0, "{}", r.summary());
}

/// The negative control: the same completion with a state its check rejects fails R7a too, and
/// the finding replays.
#[test]
fn cp33_completion_after_a_fail_stop_still_faces_the_checks() {
    let r = restarter("bad");
    let f = r
        .findings
        .iter()
        .find(|f| f.violation.invariant == InvariantId::R7a)
        .unwrap_or_else(|| panic!("{}", r.summary()));
    assert!(f.violation.detail.contains("failed a check"), "{}", r.summary());
    assert!(r.found().contains(&InvariantId::R1), "{}", r.summary());
    assert!(dashd_store::faulty::reproduces::<Restarter>(&f.repro), "{}", f.repro.to_text());
    assert_eq!(r.r7a.recovered_after_failstop, 0, "{}", r.summary());
}

/// A sanctioned message with no fatal error before it is a refusal, not a fail-stop: a branch
/// completing after it is judged by the witness rule, which no reference meets.
#[test]
fn cp33_completion_after_a_voluntary_stop_diverges() {
    let r = restarter("voluntary");
    assert!(
        r.findings.iter().any(|f| f.violation.invariant == InvariantId::R7a
            && f.violation.detail.contains("diverged")),
        "{}",
        r.summary()
    );
}

/// A later start that fail-stops again, sanctioned on a failed read but with another message,
/// where the uninterrupted recovery fail-stopped and no reference reproduces it: allowed and
/// counted apart if every other check passes (I42), as the S1 V23.1 nightly's toy branches whose
/// inner crash let the block write land and the undo write then hit an unreadable unit.
#[test]
fn cp33_a_fail_stop_after_a_fail_stop_is_allowed() {
    let r = restarter("again");
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.failstop_after_failstop > 0, "{}", r.summary());
    assert_eq!(r.r7a.recovered_after_failstop, 0, "{}", r.summary());
}

/// The negative control: with a check that fails on those branches, R7a fails them too, and the
/// finding replays.
#[test]
fn cp33_a_fail_stop_after_a_fail_stop_still_faces_the_checks() {
    let r = restarter("again-bad");
    let f = r
        .findings
        .iter()
        .find(|f| f.violation.invariant == InvariantId::R7a)
        .unwrap_or_else(|| panic!("{}", r.summary()));
    assert!(f.violation.detail.contains("fail-stopped after"), "{}", r.summary());
    assert!(dashd_store::faulty::reproduces::<Restarter>(&f.repro), "{}", f.repro.to_text());
    assert_eq!(r.r7a.failstop_after_failstop, 0, "{}", r.summary());
}

/// The S1 nightly finding (`nested_depth1_extended`, e5bd182bf, found at v23.1 and regenerated at
/// v24.0 for RP-V24: the same decisions reach it): the uninterrupted recovery fail-stops on an
/// unreadable block ("Failed to write block"); the inner crash after the block file's allocation
/// drops it, so the nested start writes the block and fail-stops on an unreadable undo unit
/// ("Failed to write undo data") instead. It replays clean, and reports R7a without I42's rule.
#[test]
fn cp33_s1_fail_stop_after_a_fail_stop_replays_clean() {
    for j in [3, 4] {
        let text = format!(
            "sut toychain\nconfig block=1024 undo=240 subbatches=0 invalid=- \
             effects=12,22 presumed=25:31 prune=- sub=synced:0:96 bug=none\n\
             workload c10;f;c5;i;c5;f;c10;f\ncrash 10 power-loss\ndecide 1:fs:i1:eio0=1\n\
             decide 1:fs:i2:eio0=1\ndecide 2:fs:i1:len=2\nnested {j} power-loss\n\
             adversary extended\ngranularity sector\nexpect R7a\n"
        );
        let r = Repro::parse(&text).unwrap();
        assert_eq!(replay::<ToyChain>(&r).unwrap(), Vec::new(), "inner event {j}");
    }
}

/// The cap that truncates only nested inner images of Restarter's search: its outer images' most
/// branches.
fn inner_only_cap(mode: &str) -> u32 {
    let full = restarter_capped(mode, 4096, None);
    full.per_image.iter().map(|(_, _, n, _)| *n as u32).max().unwrap()
}

/// With a cap below some inner images' branches but no outer image's, those images are
/// truncation, sampled or not (DEC-185 (1)): the sample reruns alike from its seed and finds the
/// remainder without repeating an explored branch, but a truncated report is never clean.
#[test]
fn cp33_nested_images_past_the_cap_are_truncated_even_sampled() {
    let cap = inner_only_cap("ok");
    let full = restarter_capped("ok", 4096, None);
    let truncated = restarter_capped("ok", cap, None);
    assert!(truncated.truncated > 0, "cap {cap}: {}", truncated.summary());
    assert!(!truncated.is_clean());
    let o = OverCap {
        seed: 20261010,
        samples: 64,
    };
    let sampled = restarter_capped("ok", cap, Some(o));
    eprintln!(
        "cap {cap}\nfull: {}truncated: {}sampled: {}",
        full.summary(),
        truncated.summary(),
        sampled.summary()
    );
    assert_eq!(sampled.truncated, truncated.truncated, "{}", sampled.summary());
    assert_eq!(sampled.sampled, truncated.truncated, "{}", sampled.summary());
    assert!(sampled.findings.is_empty() && sampled.sampled_branches > 0, "{}", sampled.summary());
    assert!(!sampled.is_clean(), "a sample passes nothing: {}", sampled.summary());
    assert_eq!(sampled.branches, full.branches, "the sample covers the small remainder exactly");
    let again = restarter_capped("ok", cap, Some(o));
    assert_eq!((again.branches, again.r7a), (sampled.branches, sampled.r7a));
}

/// Sampled branches are judged like explored ones: with "bad"'s findings, the sampled search
/// reaches the full search's branches, R7a counts and findings (so as many violations), fails the
/// report, and its findings replay.
#[test]
fn cp33_sampled_branches_are_judged_like_explored_ones() {
    let cap = inner_only_cap("bad");
    let full = restarter_capped("bad", 4096, None);
    let truncated = restarter_capped("bad", cap, None);
    let o = OverCap {
        seed: 7,
        samples: 64,
    };
    let r = restarter_capped("bad", cap, Some(o));
    assert!(r.sampled > 0 && truncated.branches < full.branches, "{}", r.summary());
    assert_eq!((r.branches, r.r7a), (full.branches, full.r7a), "{}", r.summary());
    let details = |r: &dashd_store::faulty::Report| -> Vec<String> {
        r.findings.iter().map(|f| f.violation.to_string()).collect()
    };
    assert_eq!(details(&r), details(&full));
    assert!(!r.is_clean(), "{}", r.summary());
    for f in &r.findings {
        assert!(dashd_store::faulty::reproduces::<Restarter>(&f.repro), "{}", f.repro.to_text());
    }
}

/// Sol's skewed tree (CP3.3-2): after the outer crash, the first recovery makes a marker durable;
/// in the recovery after an inner crash, 15 probe keys that may each be lost give 15 short exits,
/// then the 16th key and 12 more give two dense subtrees of 2^12 leaves. "bug" fails R1 in the
/// second, which a capped depth-first walk never reaches and a seeded walk from its frontier
/// mostly misses.
struct Skew {
    w: World,
    bad: bool,
}

fn put_in(w: &World, store: &'static str, k: &[u8]) {
    let e = w.engine(store, &SCHEMA);
    let mut b = e.batch();
    b.put(CF, k, &[1]);
    e.write(
        b,
        if store == "meta" {
            Commit::Durable
        } else {
            Commit::Buffered
        },
    )
    .unwrap();
}

impl Sut for Skew {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "skew";

    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        let mut bad = false;
        if w.epoch() > 1 {
            let meta = w.engine("meta", &SCHEMA);
            if meta.get(CF, b"marker").unwrap().is_none() {
                put_in(w, "meta", b"marker");
            } else {
                let e = w.engine("probe", &SCHEMA);
                if (0u8..15).all(|i| e.get(CF, &[i]).unwrap().is_some()) {
                    bad = e.get(CF, &[15]).unwrap().is_none() && mode == "bug";
                    for i in 16u8..28 {
                        let _ = e.get(CF, &[i]).unwrap();
                    }
                }
            }
        }
        Ok(Self {
            w: w.clone(),
            bad,
        })
    }

    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        for i in 0u8..28 {
            put_in(&self.w, "probe", &[i]);
        }
        self.w.crash_in_place(CrashKind::PowerLoss, Box::new(AllSurvive));
        for _ in 0..50 {
            self.w.fs().create(&p("f")).unwrap();
        }
        Ok(())
    }

    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        if self.bad {
            return Err(vec![Violation::new(InvariantId::R1, "the second dense subtree")]);
        }
        Ok(())
    }

    fn projection(&self) -> Option<Hash> {
        Some([0; 32])
    }
}

fn skew_scenario(mode: &str, over_cap: Option<OverCap>) -> Scenario<Skew> {
    let mut sc = Scenario::<Skew>::new("skew", mode.into(), vec![String::new()]);
    sc.adversary = Adversary::Extended;
    sc.window.from = EventSel::Index(49);
    sc.kinds = vec![CrashKind::ProcessKill];
    sc.nested = Nested::Depth1;
    sc.over_cap = over_cap;
    sc
}

/// DEC-185 (3): Skew's R1 under a cap of 4096. The capped run, sampled or not, misses it and
/// reports truncation, so it is not clean; the sharded enumeration finds it, uncapped, and is
/// clean without the bug.
#[test]
fn cp33_skew_needs_sharded_enumeration() {
    let capped = |o| {
        explore(
            &skew_scenario("bug", o),
            Mode::Exhaustive {
                max_branches_per_image: 4096,
            },
        )
    };
    for o in [
        None,
        Some(OverCap {
            seed: 20261010,
            samples: 512,
        }),
    ] {
        let r = capped(o);
        eprintln!("capped {o:?}: {}", r.summary());
        assert!(r.truncated > 0 && !r.is_clean(), "{}", r.summary());
    }
    let sharded = |mode: &str| -> Vec<Report> {
        let sc = skew_scenario(mode, None);
        // A process kill decides nothing in the outer search, so its image is one shard; the
        // inner images under it are enumerated uncapped.
        let plan = shards(&sc, 1);
        assert_eq!(plan.len(), 1, "{plan:?}");
        plan.iter().map(|s| explore_shard(&sc, s)).collect()
    };
    let bug = sharded("bug");
    let found: Vec<_> = bug.iter().filter(|r| !r.findings.is_empty()).collect();
    assert!(!found.is_empty(), "no shard finds Skew's R1");
    for r in &found {
        assert!(r.found().contains(&InvariantId::R1), "{}", r.summary());
        assert!(reproduces::<Skew>(&r.first(InvariantId::R1).unwrap().repro));
    }
    for r in sharded("ok") {
        assert!(r.is_clean(), "{}", r.summary());
    }
}
