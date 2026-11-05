//! CP3.3 (DEC-161, errata I39): under `Extended` with nested crashes, a recovery that fail-stops
//! with a sanctioned abort on a block the inner power loss tore is an allowed R7a outcome,
//! counted apart. Every other non-completion stays an R7a failure.
//!
//! DEC-185 (CP3.3-1): R8 reads the system's own trace. A recovery that returns an error after
//! a fatal read without marking its abort fails R8; the harness never supplies the mark
//! (`Marker`, Sol's probe, with its marked, wrong-message and late-write controls).

use std::path::Path;

use dashd_store::faulty::{
    Abort, Adversary, CrashKind, EventSel, Fault, FaultKind, FaultOutcome, Hash, InvariantId, Mark,
    Mode, Nested, OpKind, Reference, Report, Repro, Scenario, StoreSel, Sut, Trace, Violation,
    World, explore, forward_trace, replay, replay_trace,
};
use dashd_store::flatfile::FlatFs;

const DIR: &str = "/dd";
const LOG: &str = "/dd/log";
const MARKER: &str = "/dd/marker";
const OTHER: &str = "/dd/other";
/// Core's message for a block it cannot read (the toy chain's sanctioned abort).
const READ_FAILED: &str = "Failed to read block";

pub struct NoRef;
impl Reference for NoRef {
    type Config = String;
    type Op = String;
    fn new(_: &String) -> Self {
        Self
    }
    fn apply(&mut self, _: &String) {}
}

/// Marks the abort, as an engine does before it stops.
fn stop(w: &World, message: &str) -> Abort {
    w.mark(Mark::Abort {
        message: message.into(),
    });
    Abort::new(message)
}

/// A two-record log in one 4 KiB block. The forward run makes record 1 durable ("torn" also
/// writes record 2, unsynced); recovery completes the log with record 2 and syncs it, so an
/// inner power loss after that write can tear the block. Its modes, on a log it cannot read:
///
/// - "sanctioned" aborts with Core's read failure: the fail-stop DEC-161 allows;
/// - "unsanctioned" aborts with a message the design does not sanction;
/// - "late" writes another file first, then aborts: not a fail-stop;
/// - "garbage" also aborts, sanctioned, on a torn record 2 it read successfully: a fail-stop with
///   no unreadable block;
/// - "earlier" reads the log only on a second recovery (after it wrote `marker`), so the block
///   it fails on was torn by the outer crash, not the inner one.
fn recover(w: &World, mode: &str) -> Result<u8, Abort> {
    let fs = w.fs();
    w.mark(Mark::RecoveryBegin);
    let (log, marker) = (Path::new(LOG), Path::new(MARKER));
    if mode == "earlier" && fs.len(marker).unwrap().is_none() {
        fs.write_all_at(marker, 0, &[1]).unwrap();
        fs.sync_data(marker).unwrap();
        fs.sync_dir(Path::new(DIR)).unwrap();
        w.mark(Mark::RecoveryEnd);
        return Ok(1);
    }
    // A lost log (its directory entry torn) is an empty one.
    let len = fs.len(log).unwrap().unwrap_or(0);
    let mut bytes = vec![0; len as usize];
    if len > 0 && fs.read_exact_at(log, 0, &mut bytes).is_err() {
        return Err(match mode {
            "unsanctioned" => stop(w, "the log is unreadable"),
            "late" => {
                let _ = fs.write_all_at(Path::new(OTHER), 0, &[1]);
                stop(w, READ_FAILED)
            }
            _ => stop(w, READ_FAILED),
        });
    }
    if mode == "earlier" {
        w.mark(Mark::RecoveryEnd);
        return Ok(1);
    }
    if len < 1024 {
        fs.write_all_at(log, 512, &[2; 512]).unwrap();
        fs.sync_data(log).unwrap();
    } else if mode == "garbage" && bytes[512..1024] != [2; 512] {
        return Err(stop(w, READ_FAILED));
    }
    w.mark(Mark::RecoveryEnd);
    Ok(2)
}

pub struct Log {
    p: Hash,
    w: World,
}

impl Sut for Log {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-log";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            p[0] = recover(w, mode)?;
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }
    fn apply(&mut self, op: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        let log = Path::new(LOG);
        fs.write_all_at(log, 0, &[1; 512]).unwrap();
        fs.sync_data(log).unwrap();
        fs.sync_dir(Path::new(DIR)).unwrap();
        if op == "torn" {
            fs.write_all_at(log, 512, &[2; 512]).unwrap();
        }
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn sanctioned(_: &String, message: &str) -> bool {
        message == READ_FAILED
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

/// Every nested branch of both crash kinds under `Extended`.
fn search(mode: &str, op: &str) -> Report {
    let mut sc = Scenario::<Log>::new("cp33", mode.into(), vec![op.into()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.adversary = Adversary::Extended;
    sc.nested = Nested::Depth1;
    let r = explore(
        &sc,
        Mode::Exhaustive {
            max_branches_per_image: 1024,
        },
    );
    eprintln!("{mode} {op}: {}", r.summary());
    r
}

fn r7a_found(r: &Report) -> bool {
    r.found().contains(&InvariantId::R7a)
}

/// The positive control: the inner power loss tears the recovery's own record 2, the next
/// recovery cannot read the block and stops with Core's read failure. Allowed, and counted.
#[test]
fn cp33_torn_recovery_write_sanctioned_fail_stop_is_allowed() {
    let r = search("sanctioned", "");
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.sanctioned_fail_stop > 0, "no sanctioned fail-stop counted: {}", r.summary());
    assert!(r.r7a.converged > 0, "{}", r.summary());
}

/// The same fail-stop with a message the design does not sanction stays an R7a failure (R0
/// reports the message too).
#[test]
fn cp33_unsanctioned_message_fails() {
    let r = search("unsanctioned", "");
    assert!(r7a_found(&r), "{}", r.summary());
    assert!(r.found().contains(&InvariantId::R0), "{}", r.summary());
    assert_eq!(r.r7a.sanctioned_fail_stop, 0, "{}", r.summary());
}

/// A sanctioned abort with no unreadable block behind it (record 2 read back torn) is not the
/// fail-stop DEC-161 allows: R7a reports it.
#[test]
fn cp33_fail_stop_without_a_torn_block_fails() {
    let r = search("garbage", "");
    assert!(r7a_found(&r), "{}", r.summary());
    assert!(
        r.findings.iter().any(|f| f.violation.detail.contains("did not complete")),
        "{}",
        r.summary()
    );
}

/// A recovery that writes after the failed read has not fail-stopped: R8 and R7a report it.
#[test]
fn cp33_write_after_the_failed_read_fails() {
    let r = search("late", "");
    assert!(r7a_found(&r), "{}", r.summary());
    assert!(r.found().contains(&InvariantId::R8), "{}", r.summary());
    assert_eq!(r.r7a.sanctioned_fail_stop, 0, "{}", r.summary());
}

/// A block an earlier crash tore, which only the interrupted recovery reads: not a fail-stop on
/// the inner crash's EIO, so R7a reports it (decided under DEC-161 in CP3.3). The same block torn
/// by the inner power loss (an outer process kill keeps the write pending) is allowed.
#[test]
fn cp33_block_torn_by_an_earlier_crash_fails() {
    let r = search("earlier", "torn");
    assert!(r7a_found(&r), "{}", r.summary());
    let f = r.first(InvariantId::R7a).unwrap();
    assert_eq!(f.repro.kind, CrashKind::PowerLoss, "{}", r.summary());
    assert!(r.r7a.sanctioned_fail_stop > 0, "{}", r.summary());
}

/// The nightly runs the nested `Extended` searches (DEC-161), and its runner lists every ignored
/// test of the deep suite, so a new component cannot be left out.
#[test]
fn cp33_nightly_runs_every_deep_component() {
    let read = |p: &str| std::fs::read_to_string(format!("{}/{p}", env!("CARGO_MANIFEST_DIR")));
    let runner = read("../contrib/node/st3a-nightly.sh").unwrap();
    let list = runner.split("COMPONENTS=(").nth(1).and_then(|r| r.split(')').next()).unwrap();
    let components: Vec<&str> = list.split_whitespace().collect();
    let deep = read("tests/power_loss_deep.rs").unwrap();
    let ignored: Vec<&str> =
        deep.split("#[ignore]\nfn ").skip(1).map(|f| f.split('(').next().unwrap()).collect();
    assert!(ignored.contains(&"nested_depth1_extended"), "{ignored:?}");
    for t in &ignored {
        assert!(components.contains(t), "{t} is not a nightly component: {components:?}");
    }
}

/// The shard runner (DEC-185 (2)): a failing shard fails the component, a rerun at the same head
/// runs only the shards whose last row there is not ok (CP3.4-2), another head reruns every shard
/// (no carry-over), and
/// the nightly takes the runner's verdict for `nested_depth1_extended`.
#[test]
fn cp33_shard_runner_ledger() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    let contrib = format!("{}/../contrib/node", env!("CARGO_MANIFEST_DIR"));
    let dir = std::env::temp_dir().join(format!("st3a-shards-{}", std::process::id()));
    let bins = dir.join("bin");
    std::fs::create_dir_all(&bins).unwrap();
    // A plan of three shards; a shard whose id contains $STUB_FAIL fails. Every shard run is
    // logged to $STUB_RUNS.
    let stub = r#"#!/bin/sh
if [ -n "${ST3A_SHARD_PLAN:-}" ]; then
  printf '# depth 1\nA:e1-pk-0\nA:e1-pk-1\nB:e2-pl-x\n' > "$ST3A_SHARD_PLAN"
  echo "test result: ok. 1 passed; 0 failed"; exit 0
fi
echo "$ST3A_SHARD" >> "$STUB_RUNS"
case "$ST3A_SHARD" in
  *"$STUB_FAIL"*) echo "SHARD $ST3A_SHARD: 1 finding"
    echo "test result: FAILED. 0 passed; 1 failed"; exit 101 ;;
  *) echo "SHARD $ST3A_SHARD: clean"; echo "test result: ok. 1 passed; 0 failed"; exit 0 ;;
esac
"#;
    let deep = bins.join("power_loss_deep");
    std::fs::write(&deep, stub).unwrap();
    std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runs = dir.join("runs");
    let shards = |out: &str, head: &str, fail: &str| {
        let out = dir.join(out);
        let _ = std::fs::remove_file(&runs);
        let ok = Command::new("bash")
            .arg(format!("{contrib}/st3a-shards.sh"))
            .arg(&out)
            .env("ST3A_SHARDS_BIN", &deep)
            .env("ST3A_SHARDS_HEAD", head)
            .env("ST3A_SHARDS_INLINE", "1")
            .env("ST3A_SHARD_JOBS", "2")
            .env("STUB_RUNS", &runs)
            .env("STUB_FAIL", fail)
            .status()
            .unwrap()
            .success();
        let summary = std::fs::read_to_string(out.join("summary.txt")).unwrap_or_default();
        let mut ran: Vec<String> =
            std::fs::read_to_string(&runs).unwrap_or_default().lines().map(String::from).collect();
        ran.sort();
        (ok, summary, ran)
    };
    let (ok, summary, ran) = shards("out", "h1", "e1-pk-1");
    assert!(!ok, "a failing shard fails the component: {summary}");
    assert_eq!(ran.len(), 3, "{ran:?}");
    assert!(summary.contains("NOT OK A:e1-pk-1 FAILED"), "{summary}");
    assert!(summary.contains("shards=3 ok=2 failed=1"), "{summary}");
    let (ok, summary, ran) = shards("out", "h1", "none");
    assert!(ok, "{summary}");
    assert_eq!(ran, ["A:e1-pk-1"], "only the shard not yet ok at h1 reruns");
    assert!(summary.contains("shards=3 ok=3 failed=0"), "{summary}");
    let (ok, summary, ran) = shards("out", "h2", "none");
    assert!(ok, "{summary}");
    assert_eq!(ran.len(), 3, "a new head reruns every shard: {ran:?}");
    // The last row at the head decides, for the resume as for the verdict (CP3.4-2): an older
    // ok does not skip a shard whose latest run failed.
    let seeded = dir.join("seeded");
    std::fs::create_dir_all(&seeded).unwrap();
    let row = |id: &str, st: &str| format!("h3\t{id}\t{st}\t1\t0\t1\tr\n");
    let ledger = [
        row("A:e1-pk-0", "ok"),
        row("A:e1-pk-1", "ok"),
        row("B:e2-pl-x", "ok"),
        row("A:e1-pk-1", "FAILED"),
    ]
    .concat();
    std::fs::write(seeded.join("ledger.tsv"), ledger).unwrap();
    let (ok, summary, ran) = shards("seeded", "h3", "none");
    assert!(ok, "{summary}");
    assert_eq!(ran, ["A:e1-pk-1"], "only the shard whose last row failed reruns");

    let nightly = |fail: &str| {
        let out = dir.join(format!("nightly-{fail}"));
        let ok = Command::new("bash")
            .arg(format!("{contrib}/st3a-nightly.sh"))
            .arg(&out)
            .arg("nested_depth1_extended")
            .env("ST3A_NIGHTLY_BIN_DIR", &bins)
            .env("ST3A_SHARDS_INLINE", "1")
            .env("STUB_RUNS", &runs)
            .env("STUB_FAIL", fail)
            .status()
            .unwrap()
            .success();
        (ok, std::fs::read_to_string(out.join("summary.txt")).unwrap_or_default())
    };
    let (ok, summary) = nightly("none");
    assert!(ok && summary.contains("nested_depth1_extended exit=0 status=ok"), "{summary}");
    let (ok, summary) = nightly("e2-pl");
    assert!(!ok && summary.contains("nested_depth1_extended exit=1 status=FAILED"), "{summary}");
    std::fs::remove_dir_all(&dir).unwrap();
}

const A: &str = "/dd/a";
const B: &str = "/dd/b";

fn read_all(w: &World, p: &str) -> Result<(), ()> {
    let fs = w.fs();
    let len = fs.len(Path::new(p)).unwrap().unwrap_or(0);
    let mut buf = vec![0; len as usize];
    if len > 0 && fs.read_exact_at(Path::new(p), 0, &mut buf).is_err() {
        return Err(());
    }
    Ok(())
}

/// The review's two-file log (CP3.3 validator). The forward run makes A's record 1 and B durable,
/// then overwrites B unsynced. The first recovery makes `marker` durable and writes A's record 2
/// (synced by its next event, so an inner crash can tear it); a second recovery runs `mode`:
///
/// - "p1_write": an advisory allocation fails on A's torn block, a write succeeds, then the read of
///   A fails: the stop is the read, on a block the inner crash tore (allowed);
/// - "p2_mask": the same allocation, then the read of B fails, which only the outer crash tore
///   (an R7a failure the earlier, torn, unavailable entry must not mask);
/// - "p3_copy": B copied over A, then the read of A fails on B's outer EIO: the block no longer
///   holds what the inner crash tore.
fn recover_two(w: &World, mode: &str) -> Result<u8, Abort> {
    let fs = w.fs();
    w.mark(Mark::RecoveryBegin);
    let (a, marker) = (Path::new(A), Path::new(MARKER));
    let other = |e: std::io::Error| stop(w, &format!("other {e}"));
    if fs.len(Path::new(B)).map_err(other)?.is_none() {
        w.mark(Mark::RecoveryEnd);
        return Ok(1);
    }
    if fs.len(marker).map_err(other)?.is_none() {
        fs.write_all_at(marker, 0, &[1]).map_err(other)?;
        fs.sync_data(marker).map_err(other)?;
        fs.sync_dir(Path::new(DIR)).map_err(other)?;
        fs.write_all_at(a, 512, &[2; 512]).map_err(other)?;
        fs.sync_data(a).map_err(other)?;
        w.mark(Mark::RecoveryEnd);
        return Ok(1);
    }
    let failed = match mode {
        "p1_write" => {
            fs.allocate(a, 1024, 8192);
            fs.write_all_at(Path::new(OTHER), 0, &[9]).map_err(other)?;
            read_all(w, A).is_err()
        }
        "p2_mask" => {
            fs.allocate(a, 1024, 8192);
            read_all(w, B).is_err()
        }
        "p3_copy" => {
            fs.copy(Path::new(B), a).map_err(other)?;
            read_all(w, A).is_err()
        }
        _ => unreachable!(),
    };
    if failed {
        return Err(stop(w, READ_FAILED));
    }
    w.mark(Mark::RecoveryEnd);
    Ok(1)
}

pub struct TwoFiles {
    p: Hash,
    w: World,
}

impl Sut for TwoFiles {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-two-files";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        let mut p = [0; 32];
        if w.epoch() > 0 {
            p[0] = recover_two(w, mode)?;
        }
        Ok(Self {
            p,
            w: w.clone(),
        })
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let fs = self.w.fs();
        let (a, b) = (Path::new(A), Path::new(B));
        fs.write_all_at(a, 0, &[1; 512]).unwrap();
        fs.sync_data(a).unwrap();
        fs.write_all_at(b, 0, &[3; 512]).unwrap();
        fs.sync_data(b).unwrap();
        fs.sync_dir(Path::new(DIR)).unwrap();
        fs.write_all_at(b, 0, &[4; 512]).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn sanctioned(_: &String, message: &str) -> bool {
        message == READ_FAILED
    }
    fn projection(&self) -> Option<Hash> {
        Some(self.p)
    }
}

fn search_two(mode: &str) -> Report {
    let mut sc = Scenario::<TwoFiles>::new("cp33-two", mode.into(), vec!["x".into()]);
    sc.kinds = vec![CrashKind::ProcessKill, CrashKind::PowerLoss];
    sc.adversary = Adversary::Extended;
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

/// The stop is the operation at the first fatal error: a successful write after an earlier,
/// non-fatal failure on the same torn block does not disqualify the fail-stop (validator P1).
#[test]
fn cp33_stop_is_the_fatal_read_not_an_earlier_failure() {
    let r = search_two("p1_write");
    assert!(r.is_clean(), "{}", r.summary());
    assert!(r.r7a.sanctioned_fail_stop > 0, "{}", r.summary());
}

/// The validator's masked branch: the outer power loss leaves B unreadable; the inner one, after
/// the recovery's write of A's record 2 (event 3), tears it and leaves A's block unreadable. The
/// next recovery fails on A first, then stops on B, or on B's EIO copied over A.
fn masked(mode: &str) -> Vec<Violation> {
    let sc = Scenario::<TwoFiles>::new("cp33-two", mode.into(), vec!["x".into()]);
    let mut r = Repro::new::<TwoFiles>(&sc, None);
    r.adversary = Adversary::Extended;
    r.crash_at = Some(forward_trace(&sc).0.events().last().unwrap().0.index);
    r.kind = CrashKind::PowerLoss;
    r.nested = Some((3, CrashKind::PowerLoss));
    r.decisions.insert("1:fs:i2:eio0".into(), 1);
    r.decisions.insert("2:fs:i1:eio0".into(), 1);
    replay::<TwoFiles>(&r).unwrap()
}

/// An earlier, non-fatal failure on the torn block A does not make the fatal read of B, which
/// only the outer crash tore, a sanctioned fail-stop (validator P2).
#[test]
fn cp33_earlier_torn_failure_does_not_mask_the_fatal_read() {
    let got = masked("p2_mask");
    assert!(got.iter().any(|v| v.invariant == InvariantId::R7a), "{got:?}");
}

/// A block a copy replaced no longer holds what the inner crash tore: the read of A fails on B's
/// copied outer-crash EIO, which is no torn fail-stop (validator P3).
#[test]
fn cp33_copied_eio_is_not_torn() {
    let got = masked("p3_copy");
    assert!(got.iter().any(|v| v.invariant == InvariantId::R7a), "{got:?}");
}

/// Sol's CP3.3-1 probe: the forward run writes and syncs `/dd/f`; recovery enters `ConnectTip`
/// and reads it, and a recovery fault fails that read (fatal). Its modes, all returning an error:
/// "marked" aborts with Core's message (clean); "wrong" with an unsanctioned one; "late" writes
/// another file first; "unmarked" emits no `Abort` mark at all.
struct Marker(World);

impl Sut for Marker {
    type Config = String;
    type Op = String;
    type Ref = NoRef;
    const NAME: &'static str = "cp33-marker";
    fn open(w: &World, mode: &String) -> Result<Self, Abort> {
        if w.epoch() > 0 {
            w.mark(Mark::RecoveryBegin);
            w.mark(Mark::PathBegin {
                path: "ConnectTip",
            });
            if w.fs().read_exact_at(&Path::new(DIR).join("f"), 0, &mut [0]).is_err() {
                return Err(match mode.as_str() {
                    "wrong" => stop(w, "wrong abort"),
                    "late" => {
                        w.fs().write_all_at(Path::new(OTHER), 0, &[1]).unwrap();
                        stop(w, READ_FAILED)
                    }
                    "unmarked" => Abort::new(READ_FAILED),
                    _ => stop(w, READ_FAILED),
                });
            }
            w.mark(Mark::PathEnd);
            w.mark(Mark::RecoveryEnd);
        }
        Ok(Self(w.clone()))
    }
    fn apply(&mut self, _: &String) -> Result<(), Abort> {
        let (fs, f) = (self.0.fs(), Path::new(DIR).join("f"));
        fs.write_all_at(&f, 0, &[1]).unwrap();
        fs.sync_data(&f).unwrap();
        fs.sync_dir(Path::new(DIR)).unwrap();
        Ok(())
    }
    fn check(&self, _: &World, _: &NoRef, _: &Trace) -> Result<(), Vec<Violation>> {
        Ok(())
    }
    fn sanctioned(_: &String, message: &str) -> bool {
        message == READ_FAILED
    }
}

/// The violations of `Marker`'s recovery after a process kill at the end of the forward run,
/// its first recovery read failing.
fn marker(mode: &str) -> Vec<Violation> {
    let mut sc = Scenario::<Marker>::new("cp33-marker", mode.into(), vec![String::new()]);
    sc.kinds = vec![CrashKind::ProcessKill];
    let mut r = Repro::new::<Marker>(&sc, None);
    r.crash_at = Some(forward_trace(&sc).0.events().last().unwrap().0.index);
    r.kind = CrashKind::ProcessKill;
    r.recovery_fault = Some(Fault {
        at: EventSel::Nth {
            store: StoreSel::Fs,
            op: OpKind::Read,
            n: 0,
        },
        kind: FaultKind::Io,
        outcome: FaultOutcome::NotApplied,
    });
    let (v, _) = replay_trace::<Marker>(&r).unwrap();
    eprintln!("{mode}: {v:?}");
    v
}

fn r8(v: &[Violation]) -> bool {
    v.iter().any(|v| v.invariant == InvariantId::R8)
}

/// The controls: the marked fail-stop is clean; a wrong message and a write after the failed
/// read fail R8.
#[test]
fn cp33_marker_controls() {
    assert_eq!(marker("marked"), Vec::new());
    for mode in ["wrong", "late"] {
        assert!(r8(&marker(mode)), "{mode}");
    }
}

/// An error returned after the fatal read with no `Abort` mark fails R8: the harness does not
/// supply the mark (500dabd09 did, and this branch passed).
#[test]
fn cp33_unmarked_fatal_return_fails_r8() {
    let v = marker("unmarked");
    assert!(r8(&v), "{v:?}");
    assert!(
        v.iter().all(|v| v.invariant != InvariantId::Model),
        "the error closes the path: {v:?}"
    );
}
