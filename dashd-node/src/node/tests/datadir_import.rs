//! CH6a: `-importcoredatadir` on a node, from a copy of a checked-in Dash Core v24.0.0 regtest
//! data directory into an empty destination: the journal's rows, the source's lock, P4 at the
//! load, P5 in the slot after startup's ABC, and the exclusion of CH5-4b's in-place import on
//! the same start (DEC-1620).

use std::sync::Mutex;

use dash_coreimport::datadir_import::{ImportFs, JOURNAL, Journal, OsImportFs, Phase, STAGING};
use dash_coreimport::regtest_import;
use dashd_store::{Commit, KvBatch as _};

use super::*;
use crate::chain::{BlkFiles, BlkOpen, BlocksDir, ImportHooks};
use crate::core_import::tests::{contents, write_core};
use crate::datadir::tests::another_process_is_refused;
use crate::datadir_import::{ImportStart, OnAppend, SourceLock};

const FIXTURE: &str = "../dash-coreimport/tests/data/leveldb/a-wal";

/// A source base data directory under `dir`: the fixture's `regtest` network directory with an
/// empty `.lock`, and the source's best block (display hex).
fn source(dir: &Path) -> (PathBuf, String) {
    let base = dir.join("core");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    copy_tree(&fixture.join("blocks"), &base.join("regtest/blocks"));
    copy_tree(&fixture.join("chainstate"), &base.join("regtest/chainstate"));
    std::fs::write(base.join("regtest/.lock"), b"").unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.join("expected.json")).unwrap()).unwrap();
    (base, expected["best_block"].as_str().unwrap().to_owned())
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        } else {
            std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
        }
    }
}

/// An empty destination base data directory under `dir`.
fn destination(dir: &Path) -> PathBuf {
    let dest = dir.join("dest");
    std::fs::create_dir(&dest).unwrap();
    dest
}

fn flag(base: &Path) -> String {
    format!("-importcoredatadir={}", base.display())
}

fn journal_path(dest: &Path) -> PathBuf {
    dest.join("regtest").join(STAGING).join(JOURNAL)
}

fn last(dest: &Path) -> Option<Phase> {
    Journal::open(&journal_path(dest), &OsImportFs::new()).unwrap().last()
}

/// The destination's `import-report.json`.
fn report(dest: &Path) -> serde_json::Value {
    let path = dest.join("regtest/import-report.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn tip(node: &Node) -> String {
    node.steps.chain().unwrap().handle().view().snap.tip.tip_hash.to_string()
}

/// A start on `dest` with `argv` through init and the import: whether init succeeded, the
/// debug log, stderr and the tip.
struct Started {
    ok: bool,
    log: Lines,
    stderr: Lines,
    tip: Option<String>,
}

fn run(dest: &Path, argv: &[&str], set: impl FnOnce(&mut Node)) -> Started {
    let (mut node, log, stderr) = start(dest, argv);
    set(&mut node);
    let ok = node.app_init_early() && node.app_init_main();
    node.steps.join_import();
    let tip = node.steps.chain().map(|_| tip(&node));
    node.shutdown();
    Started {
        ok,
        log,
        stderr,
        tip,
    }
}

/// The OS's filesystem, failing the journal append of `phase` (none after the first failure).
fn failing_at(phase: Phase) -> Arc<dyn ImportFs> {
    let failed = Arc::new(Mutex::new(false));
    Arc::new(OnAppend {
        inner: Arc::new(OsImportFs::new()),
        before: Box::new(move |p| {
            let mut failed = failed.lock().unwrap();
            if p == phase as u8 && !*failed {
                *failed = true;
                return Err(std::io::Error::other("planted"));
            }
            Ok(())
        }),
    })
}

/// The statuses of `node`'s block index from its tip down are clean (DEC-1620).
fn clean_index_statuses(node: &Node) {
    let reader = node.steps.chain().unwrap().handle().reader().clone();
    let mut at = reader.lookup(&node.steps.chain().unwrap().handle().view().snap.tip.tip_hash);
    while let Some(id) = at {
        let entry = reader.entry(id).unwrap();
        let mask = dash_chain::Status::FAILED_MASK | dash_chain::Status::CONFLICT_CHAINLOCK;
        assert!(!entry.status.has(mask), "{}", entry.hash);
        at = entry.prev;
    }
}

#[test]
fn an_import_stages_reindexes_and_reports_after_the_abc() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let before = contents(&base);
    let dest = destination(dir.path());
    let at_finish = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&at_finish);
    let probe = dest.clone();
    let started = run(&dest, &[&flag(&base)], |node| {
        node.steps.set_import_hooks(ImportHooks {
            on_reindex_finished: Some(Box::new(move || *seen.lock().unwrap() = Some(last(&probe)))),
            ..ImportHooks::default()
        });
    });
    assert!(started.ok, "{:?}", started.stderr);
    assert_eq!(started.tip.as_deref(), Some(best.as_str()));
    assert_eq!(*at_finish.lock().unwrap(), Some(Some(Phase::ReindexArmed)), "P5 is not there");
    assert_eq!(last(&dest), Some(Phase::Reported));
    assert!(has(&started.log, "Core datadir import: complete, report "), "{:?}", started.log);
    assert_eq!(contents(&base), before, "the source is unchanged");
    assert!(datadir::probe_core_data_dir(&dest.join("regtest")).is_none());
    assert!(!another_process_is_refused(&base.join("regtest")), "the lock is released");

    let again = run(&dest, &[], |_| {});
    assert!(again.ok, "{:?}", again.stderr);
    assert_eq!(again.tip.as_deref(), Some(best.as_str()));
    assert!(!has(&again.log, "Core datadir import"), "a flagless start is an ordinary one");
    assert!(!has(&again.log, "Reindexing"));
}

/// A start whose journal append of each phase fails stops there; the next start with the same
/// `-importcoredatadir` resumes from the row it left and reports. A row at `ReindexArmed` or
/// after resumes without it; one below refuses without it (`ImportInProgress`).
#[test]
fn a_start_resumes_from_each_journal_row() {
    for phase in [
        Phase::Intent,
        Phase::Staged,
        Phase::SourceVerified,
        Phase::Placed,
        Phase::ReindexArmed,
        Phase::Reported,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let (base, best) = source(dir.path());
        let dest = destination(dir.path());
        let first =
            run(&dest, &[&flag(&base)], |node| node.steps.import_fs = Some(failing_at(phase)));
        assert_eq!(first.ok, phase == Phase::Reported, "{phase:?}: {:?}", first.stderr);
        let left = if journal_path(&dest).exists() {
            last(&dest)
        } else {
            None
        };
        let expected = match phase {
            Phase::Intent => None,
            Phase::Staged => Some(Phase::Intent),
            Phase::SourceVerified => Some(Phase::Staged),
            Phase::Placed => Some(Phase::SourceVerified),
            Phase::ReindexArmed => Some(Phase::Placed),
            Phase::Reported => Some(Phase::ReindexArmed),
        };
        assert_eq!(left, expected, "{phase:?}");
        if expected.is_some_and(|l| l < Phase::ReindexArmed) {
            let flagless = run(&dest, &[], |_| {});
            assert!(!flagless.ok, "{phase:?}");
            assert!(has(&flagless.stderr, "in progress"), "{phase:?}: {:?}", flagless.stderr);
            assert_eq!(last(&dest), expected, "{phase:?}: a refused start writes nothing");
        }
        let argv = if expected == Some(Phase::ReindexArmed) {
            vec![]
        } else {
            vec![flag(&base)]
        };
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let resumed = run(&dest, &argv, |_| {});
        assert!(resumed.ok, "{phase:?}: {:?}", resumed.stderr);
        assert_eq!(resumed.tip.as_deref(), Some(best.as_str()), "{phase:?}");
        assert_eq!(last(&dest), Some(Phase::Reported), "{phase:?}");
    }
}

#[test]
fn a_flagless_start_at_placed_is_import_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    let first = run(&dest, &[&flag(&base)], |node| {
        node.steps.import_fs = Some(failing_at(Phase::ReindexArmed));
    });
    assert!(!first.ok);
    assert_eq!(last(&dest), Some(Phase::Placed));
    let flagless = run(&dest, &[], |_| {});
    assert!(!flagless.ok);
    assert!(has(&flagless.stderr, "in progress"), "{:?}", flagless.stderr);
    assert_eq!(last(&dest), Some(Phase::Placed));
}

#[test]
fn the_source_lock_is_required_and_held_while_the_source_is_read() {
    for setup in ["missing", "read-only", "held"] {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = source(dir.path());
        let lock = base.join("regtest/.lock");
        let mut holder = None;
        match setup {
            "missing" => std::fs::remove_file(&lock).unwrap(),
            "read-only" => {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o400)).unwrap();
            }
            _ => holder = Some(datadir::tests::spawn_lock_holder(&base.join("regtest"))),
        }
        let dest = destination(dir.path());
        let started = run(&dest, &[&flag(&base)], |_| {});
        assert!(!started.ok, "{setup}");
        let text = match setup {
            "held" => "Dash Core is probably already running",
            _ => "cannot take the source's lock",
        };
        assert!(has(&started.stderr, text), "{setup}: {:?}", started.stderr);
        assert!(!journal_path(&dest).exists(), "{setup}: nothing journaled");
        if let Some(mut holder) = holder {
            drop(holder.stdin.take());
            assert!(holder.wait().unwrap().success());
        }
        let snapshot = run(&dest, &[&flag(&base), "-importsource=snapshot"], |_| {});
        if setup != "held" {
            assert!(snapshot.ok, "{setup}: a snapshot takes no lock: {:?}", snapshot.stderr);
        }
    }
}

/// In hardlink mode the lock is held until `Reported` (a probe from another process is refused
/// during the reindex); in reflink mode it is released once `SourceVerified` is durable.
#[test]
fn hardlink_holds_the_source_lock_until_reported() {
    for placement in ["reflink", "hardlink"] {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = source(dir.path());
        let dest = destination(dir.path());
        let net = base.join("regtest");
        let held = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&held);
        let probe = net.clone();
        let placement_arg = format!("-importplacement={placement}");
        let started = run(&dest, &[&flag(&base), &placement_arg], |node| {
            node.steps.set_import_hooks(ImportHooks {
                on_reindex_finished: Some(Box::new(move || {
                    *seen.lock().unwrap() = Some(another_process_is_refused(&probe));
                })),
                ..ImportHooks::default()
            });
        });
        assert!(started.ok, "{placement}: {:?}", started.stderr);
        assert_eq!(*held.lock().unwrap(), Some(placement == "hardlink"), "{placement}");
        assert!(!another_process_is_refused(&net), "{placement}: released at Reported");
    }
}

/// O1-3: the source's lock is held as `Staged` and `SourceVerified` are appended, and in reflink
/// mode released once `SourceVerified` is durable.
#[test]
fn the_source_lock_is_held_from_p0_until_source_verified() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    let net = base.join("regtest");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = Arc::clone(&seen);
    let fs: Arc<dyn ImportFs> = Arc::new(OnAppend {
        inner: Arc::new(OsImportFs::new()),
        before: Box::new(move |phase| {
            if [Phase::Staged, Phase::SourceVerified, Phase::Placed]
                .map(|p| p as u8)
                .contains(&phase)
            {
                record.lock().unwrap().push((phase, another_process_is_refused(&net)));
            }
            Ok(())
        }),
    });
    let started = run(&dest, &[&flag(&base)], |node| node.steps.import_fs = Some(fs));
    assert!(started.ok, "{:?}", started.stderr);
    let expected = [(Phase::Staged, true), (Phase::SourceVerified, true), (Phase::Placed, false)];
    assert_eq!(*seen.lock().unwrap(), expected.map(|(p, held)| (p as u8, held)));
}

/// In hardlink mode an incomplete P5 keeps the source's lock while the node runs; it is free
/// once the node is gone.
#[test]
fn hardlink_keeps_the_source_lock_after_an_incomplete_report() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    let net = base.join("regtest");
    let (mut node, log, _) = start(&dest, &[&flag(&base), "-importplacement=hardlink"]);
    let shutdown = Arc::clone(&node.shutdown);
    node.steps.set_import_hooks(ImportHooks {
        on_reindex_finished: Some(Box::new(move || shutdown.start_shutdown())),
        ..ImportHooks::default()
    });
    let _ = node.app_init_early() && node.app_init_main();
    node.steps.join_import();
    assert_eq!(last(&dest), Some(Phase::ReindexArmed));
    let line = "Core datadir import: incomplete (shutdown requested), ";
    assert!(has(&log, line), "{log:?}");
    assert!(has(&log, "the source's lock is held while the node runs"), "{log:?}");
    assert!(another_process_is_refused(&net), "held after the incomplete report");
    node.shutdown();
    drop(node);
    assert!(!another_process_is_refused(&net));
}

/// A start the import does not finish leaves the journal at `ReindexArmed`: a shutdown before
/// startup's ABC, or `-stopatheight` below the source's tip. The next start reports.
#[test]
fn an_unfinished_start_is_incomplete_and_the_next_one_reports() {
    for case in ["shutdown", "stopatheight"] {
        let dir = tempfile::tempdir().unwrap();
        let (base, best) = source(dir.path());
        let dest = destination(dir.path());
        let started = match case {
            "shutdown" => run(&dest, &[&flag(&base)], |node| {
                let shutdown = Arc::clone(&node.shutdown);
                node.steps.set_import_hooks(ImportHooks {
                    on_reindex_finished: Some(Box::new(move || shutdown.start_shutdown())),
                    ..ImportHooks::default()
                });
            }),
            _ => run(&dest, &[&flag(&base), "-stopatheight=4"], |_| {}),
        };
        assert_ne!(started.tip.as_deref(), None, "{case}");
        assert_eq!(last(&dest), Some(Phase::ReindexArmed), "{case}: {:?}", started.log);
        assert!(!has(&started.log, "Core datadir import: complete"), "{case}");
        if case == "shutdown" {
            assert!(has(&started.log, "Core datadir import: incomplete (shutdown requested)"));
        }
        let written = report(&dest);
        assert_eq!(written["status"], "incomplete", "{case}: {written}");
        assert_eq!(written["reason"], "shutdown requested", "{case}: {written}");
        let resumed = run(&dest, &[], |_| {});
        assert!(resumed.ok, "{case}: {:?}", resumed.stderr);
        assert_eq!(resumed.tip.as_deref(), Some(best.as_str()), "{case}");
        assert_eq!(last(&dest), Some(Phase::Reported), "{case}");
    }
}

/// §2.3: outside hardlink mode a resume at `ReindexArmed` reads no source, so one deleted, or
/// re-created with new inodes, after `ReindexArmed` still lets a flagless start report `equal`.
#[test]
fn a_resume_at_reindexarmed_needs_no_source() {
    for case in ["deleted", "re-created"] {
        let dir = tempfile::tempdir().unwrap();
        let (base, best) = source(dir.path());
        let dest = destination(dir.path());
        run(&dest, &[&flag(&base)], |node| {
            node.steps.import_fs = Some(failing_at(Phase::Reported))
        });
        assert_eq!(last(&dest), Some(Phase::ReindexArmed), "{case}");
        std::fs::remove_dir_all(&base).unwrap();
        if case == "re-created" {
            source(dir.path());
        }
        let resumed = run(&dest, &[], |_| {});
        assert!(resumed.ok, "{case}: {:?}", resumed.stderr);
        assert_eq!(resumed.tip.as_deref(), Some(best.as_str()), "{case}");
        assert_eq!(last(&dest), Some(Phase::Reported), "{case}");
        let written = report(&dest);
        assert_eq!(written["status"], "complete", "{case}: {written}");
        assert_eq!(written["relation"], "equal", "{case}: {written}");
    }
}

/// A flag-set start at `ReindexArmed` naming another `-importcoredatadir`, existing or not, is
/// refused as an import in progress with the destination unchanged; the recorded path resumes
/// with its source gone.
#[test]
fn a_reindexarmed_import_with_another_datadir_is_in_progress() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    run(&dest, &[&flag(&base)], |node| node.steps.import_fs = Some(failing_at(Phase::Reported)));
    assert_eq!(last(&dest), Some(Phase::ReindexArmed));
    let before = tree(&dest);
    let (other, _) = source(&dir.path().join("other"));
    for given in [other, dir.path().join("missing")] {
        let (_, params) = config(&dest, &[&flag(&given)]);
        let err = format!("{:?}", params.unwrap_err());
        assert!(err.contains("an import from") && err.contains("is in progress"), "{err}");
        assert!(err.contains(&base.canonicalize().unwrap().display().to_string()), "{err}");
        assert_eq!(tree(&dest), before, "{given:?}");
        assert_eq!(last(&dest), Some(Phase::ReindexArmed), "{given:?}");
    }
    let recorded = base.canonicalize().unwrap();
    std::fs::remove_dir_all(&base).unwrap();
    let resumed = run(&dest, &[&flag(&recorded)], |_| {});
    assert!(resumed.ok, "{:?}", resumed.stderr);
    assert_eq!(resumed.tip.as_deref(), Some(best.as_str()));
    assert_eq!(last(&dest), Some(Phase::Reported));
}

/// A flag-set resume at `ReindexArmed` resolves the given `-importcoredatadir` as P0 recorded it
/// with the source gone: through a symlinked parent, a `..`, or a symlink that is the path
/// itself.
#[test]
fn a_reindexarmed_resume_resolves_the_given_path_without_its_source() {
    use std::os::unix::fs::symlink;
    for case in ["symlinked-parent", "dotdot", "symlink"] {
        let dir = tempfile::tempdir().unwrap();
        let (real, best, given) = match case {
            "symlinked-parent" => {
                let (base, best) = source(&dir.path().join("real"));
                symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
                (base, best, dir.path().join("link/core"))
            }
            "dotdot" => {
                let (base, best) = source(dir.path());
                std::fs::create_dir(dir.path().join("sub")).unwrap();
                (base, best, dir.path().join("sub/../core"))
            }
            _ => {
                let (base, best) = source(dir.path());
                symlink(&base, dir.path().join("corelink")).unwrap();
                (base, best, dir.path().join("corelink"))
            }
        };
        let dest = destination(dir.path());
        let import = flag(&given);
        run(&dest, &[&import], |node| node.steps.import_fs = Some(failing_at(Phase::Reported)));
        assert_eq!(last(&dest), Some(Phase::ReindexArmed), "{case}");
        std::fs::remove_dir_all(&real).unwrap();
        let resumed = run(&dest, &[&import], |_| {});
        assert!(resumed.ok, "{case}: {:?}", resumed.stderr);
        assert_eq!(resumed.tip.as_deref(), Some(best.as_str()), "{case}");
        assert_eq!(last(&dest), Some(Phase::Reported), "{case}");
    }
}

/// The node's classifier: a tip that is a strict ancestor of the source's best block is
/// `ancestor`, so P5 writes an `incomplete` report naming both heights and appends nothing,
/// and a second P5 repeats it. (DEC-1620: the slice builds no such start; this calls P5 on the
/// best block's parent.)
#[test]
fn an_ancestor_tip_is_incomplete_and_appends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    run(&dest, &[&flag(&base)], |node| node.steps.import_fs = Some(failing_at(Phase::Reported)));
    let (mut node, log, stderr) = start(&dest, &[]);
    node.steps.import_fs = Some(failing_at(Phase::Reported));
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    node.steps.join_import();
    assert_eq!(tip(&node), best);
    let handle = node.steps.chain().unwrap().handle().clone();
    let reader = handle.reader();
    let b = reader.entry(reader.lookup(&handle.view().snap.tip.tip_hash).unwrap()).unwrap();
    let parent = reader.entry(b.prev.unwrap()).unwrap();
    let records = Journal::open(&journal_path(&dest), &OsImportFs::new()).unwrap().records().len();
    for round in 0..2 {
        std::fs::remove_file(dest.join("regtest/import-report.json")).unwrap();
        let seen = log.lock().unwrap().len();
        let import = ImportStart {
            journal: Journal::open(&journal_path(&dest), &OsImportFs::new()).unwrap(),
            fs: Arc::new(OsImportFs::new()),
            lock: SourceLock::default(),
        };
        import.report(&handle, parent.hash, &node.shutdown, node.ui.logger());
        let line = format!(
            "Core datadir import: incomplete (tip is an ancestor of the source's best block), \
             relation ancestor, tip height {}, best block height {}, report ",
            parent.height, b.height
        );
        let new = log.lock().unwrap()[seen..].to_vec();
        assert!(new.iter().any(|l| l.contains(&line)), "{round}: {new:?}");
        let written = report(&dest);
        assert_eq!(written["status"], "incomplete", "{round}");
        assert_eq!(written["relation"], "ancestor", "{round}");
        let journal = Journal::open(&journal_path(&dest), &OsImportFs::new()).unwrap();
        assert_eq!((journal.last(), journal.records().len()), (Some(Phase::ReindexArmed), records));
    }
    node.shutdown();
}

/// `AbortNode` after the reindex's clear and before startup's ABC: the ABC still answers, and
/// P5 writes an `incomplete (abort)` report with the journal left at `ReindexArmed`. The next
/// start reports, with clean index statuses (DEC-1620).
#[test]
fn an_abort_before_startups_abc_is_incomplete_and_the_next_start_reports() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    let abort = Arc::new(Mutex::new(None));
    let (keep, fire) = (Arc::clone(&abort), Arc::clone(&abort));
    let started = run(&dest, &[&flag(&base)], |node| {
        node.steps.before_import = Some(Box::new(move |_, _, abort| {
            *keep.lock().unwrap() = Some(Arc::clone(abort));
        }));
        node.steps.set_import_hooks(ImportHooks {
            on_reindex_finished: Some(Box::new(move || {
                let abort = fire.lock().unwrap().take().unwrap();
                crate::abort::AbortSink::abort_node(
                    &*abort,
                    "test: AbortNode after the clear",
                    None,
                );
            })),
            ..ImportHooks::default()
        });
    });
    assert!(has(&started.log, "Core datadir import: incomplete (abort)"), "{:?}", started.log);
    assert_eq!(report(&dest)["status"], "incomplete");
    assert_eq!(report(&dest)["reason"], "abort");
    assert_eq!(last(&dest), Some(Phase::ReindexArmed));
    let (mut node, _, stderr) = start(&dest, &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    node.steps.join_import();
    assert_eq!(tip(&node), best);
    clean_index_statuses(&node);
    node.shutdown();
    assert_eq!(last(&dest), Some(Phase::Reported));
}

/// A store failure and `AbortNode` after the reindex scan and before its clear: the scan does
/// not finish, and P5 still writes an `incomplete (abort)` report with the journal left at
/// `ReindexArmed`. The next start reports, with clean index statuses (DEC-1620).
#[test]
fn an_unfinished_scan_still_writes_an_incomplete_report() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    let held = Arc::new(Mutex::new(None));
    let (keep, fire) = (Arc::clone(&held), Arc::clone(&held));
    let finished = Arc::new(Mutex::new(false));
    let set = Arc::clone(&finished);
    let started = run(&dest, &[&flag(&base)], |node| {
        node.steps.before_import = Some(Box::new(move |_, latch, abort| {
            *keep.lock().unwrap() = Some((latch.clone(), Arc::clone(abort)));
        }));
        node.steps.set_import_hooks(ImportHooks {
            after_scan: Some(Box::new(move || {
                let (latch, abort) = fire.lock().unwrap().take().unwrap();
                let failure = dash_kv::Failure {
                    kind: dash_kv::ErrorKind::Other,
                    site: "test",
                    message: "test: a store failure after the scan".to_owned(),
                };
                assert!(latch.trip(failure));
                crate::abort::AbortSink::abort_node(&*abort, "test: after the scan", None);
            })),
            on_reindex_finished: Some(Box::new(move || *set.lock().unwrap() = true)),
            ..ImportHooks::default()
        });
    });
    assert!(!*finished.lock().unwrap(), "the scan did not finish");
    assert!(has(&started.log, "Core datadir import: incomplete (abort)"), "{:?}", started.log);
    let written = report(&dest);
    assert_eq!(
        (written["status"].as_str(), written["reason"].as_str()),
        (Some("incomplete"), Some("abort"))
    );
    assert_eq!(last(&dest), Some(Phase::ReindexArmed));
    let (mut node, _, stderr) = start(&dest, &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    node.steps.join_import();
    assert_eq!(tip(&node), best);
    clean_index_statuses(&node);
    node.shutdown();
    assert_eq!(last(&dest), Some(Phase::Reported));
}

#[test]
fn stopafterblockimport_reports_before_it_stops() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    let started = run(&dest, &[&flag(&base), "-stopafterblockimport"], |_| {});
    assert_eq!(last(&dest), Some(Phase::Reported));
    assert!(
        position(&started.log, "Core datadir import: complete")
            < position(&started.log, "Stopping after block import")
    );
}

/// A destination holding an rdashd datadir is `DestinationNotEmpty` before anything is written.
#[test]
fn an_existing_rdashd_datadir_is_destination_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    assert!(run(&dest, &[], |_| {}).ok);
    let before = tree(&dest);
    let (_, params) = config(&dest, &[&flag(&base)]);
    let err = params.unwrap_err();
    assert!(format!("{err:?}").contains("destination is not empty"), "{err:?}");
    assert_eq!(tree(&dest), before);
}

/// CH5-4b and CH6a exclude each other on one start (DEC-1620): with `-importcoredatadir` a Core
/// network directory as the destination is `DestinationNotEmpty`, and nothing is moved aside.
#[test]
fn a_core_destination_with_importcoredatadir_is_destination_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    write_core(&dest.join("regtest"));
    let core = contents(&dest.join("regtest"));
    let (_, params) = config(&dest, &[&flag(&base)]);
    let err = params.unwrap_err();
    assert!(format!("{err:?}").contains("destination is not empty"), "{err:?}");
    assert_eq!(contents(&dest.join("regtest")), core, "no auto-import");
}

/// A start with `-importcoredatadir` whose options are refused still never writes into a Core
/// network directory: init fails before the directory's settings file or any import state.
#[test]
fn refused_import_options_leave_a_core_destination_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let import = flag(&base);
    for argv in [
        vec![import.as_str(), "-importmode=bogus"],
        vec![import.as_str(), "-importmode=fast"],
        vec![import.as_str(), "-importsource=snapshot", "-importplacement=hardlink"],
        vec!["-importcoredatadir="],
    ] {
        let dest = tempfile::tempdir().unwrap();
        let net = dest.path().join("regtest");
        write_core(&net);
        let core = contents(&net);
        let (_, params) = config(dest.path(), &argv);
        assert!(params.is_err(), "{argv:?}");
        assert_eq!(contents(&net), core, "{argv:?}");
        assert!(!net.join(dash_coreimport::regtest_import::JOURNAL).exists(), "{argv:?}");
        assert!(!net.join(STAGING).exists(), "{argv:?}");
        assert!(!net.join("settings.json").exists(), "{argv:?}");
    }
}

/// A Core network directory left at CH5-4b's `Moved`, here with no blk files, in `net`: its
/// contents before the move.
fn moved_core(net: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    write_core(net);
    std::fs::remove_file(net.join("blocks/blk00000.dat")).unwrap();
    let core = contents(net);
    regtest_import::begin(&OsImportFs::new(), net).unwrap();
    core
}

/// The CH5-4b journal's phase in `net`, and the contents of its one backup.
fn ch5_4b_state(net: &Path) -> (regtest_import::Phase, BTreeMap<PathBuf, Option<Vec<u8>>>) {
    let journal = regtest_import::read_journal(&OsImportFs::new(), net).unwrap().unwrap();
    let backups: Vec<PathBuf> = std::fs::read_dir(net)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name().unwrap().to_string_lossy().starts_with(regtest_import::BACKUP_PREFIX)
        })
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    (journal.phase, contents(&backups[0]))
}

/// With `-importcoredatadir`, `init_config` refuses a destination with a Core directory's sign
/// or CH5-4b's journal or backup as `DestinationNotEmpty`, and writes nothing there.
#[test]
fn a_destination_with_a_core_sign_or_ch5_4b_state_is_destination_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    for case in ["evodb", "llmq-entry", "backup", "moved", "moved-blk"] {
        let dest = tempfile::tempdir().unwrap();
        let net = dest.path().join("regtest");
        std::fs::create_dir(&net).unwrap();
        match case {
            "evodb" => std::fs::create_dir(net.join("evodb")).unwrap(),
            "llmq-entry" => std::fs::create_dir_all(net.join("llmq/other")).unwrap(),
            "backup" => std::fs::create_dir(net.join("core-import-1")).unwrap(),
            "moved" => drop(moved_core(&net)),
            _ => {
                write_core(&net);
                regtest_import::begin(&OsImportFs::new(), &net).unwrap();
            }
        }
        let before = contents(&net);
        let (_, params) = config(dest.path(), &[&flag(&base)]);
        let err = params.unwrap_err();
        assert!(format!("{err:?}").contains("destination is not empty"), "{case}: {err:?}");
        assert_eq!(contents(&net), before, "{case}");
        if case.starts_with("moved") {
            assert_eq!(ch5_4b_state(&net).0, regtest_import::Phase::Moved, "{case}");
        }
    }
}

/// Step 7 never runs CH5-4b's import on a start with `-importcoredatadir`: a destination that
/// reaches `Moved` after `init_config`'s check keeps its CH5-4b journal and backup unchanged
/// while the import runs.
#[test]
fn importcoredatadir_suppresses_the_auto_import() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    let net = dest.join("regtest");
    let (mut node, log, stderr) = start(&dest, &[&flag(&base)]);
    moved_core(&net);
    let moved = ch5_4b_state(&net);
    assert_eq!(moved.0, regtest_import::Phase::Moved);
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    node.steps.join_import();
    assert_eq!(tip(&node), best);
    node.shutdown();
    assert!(!has(&log, "Moved the Dash Core databases"));
    assert_eq!(ch5_4b_state(&net), moved);
}

/// A flagless start on a destination at `ReindexArmed` finds no Core directory to import in
/// place, and finishes the import.
#[test]
fn a_flagless_start_on_an_import_destination_is_no_auto_import() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    run(&dest, &[&flag(&base)], |node| node.steps.import_fs = Some(failing_at(Phase::Reported)));
    assert_eq!(last(&dest), Some(Phase::ReindexArmed));
    let (_, params) = config(&dest, &[]);
    assert!(!params.unwrap().core_import);
    let started = run(&dest, &[], |_| {});
    assert!(started.ok, "{:?}", started.stderr);
    assert!(!has(&started.log, "Moved the Dash Core databases"));
    assert!(!dest.join("regtest").join(regtest_import::JOURNAL).exists());
    assert_eq!(last(&dest), Some(Phase::Reported));
}

/// A source changed under a resume at `Staged` is refused: P2 compares it with the intent's
/// manifest, then discards every record and clears staging, so the next start begins at P0 and
/// reports.
#[test]
fn a_changed_source_refuses_the_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    let first = run(&dest, &[&flag(&base)], |node| {
        node.steps.import_fs = Some(failing_at(Phase::SourceVerified));
    });
    assert!(!first.ok);
    assert_eq!(last(&dest), Some(Phase::Staged));
    let blk = base.join("regtest/blocks/blk00000.dat");
    let mut bytes = std::fs::read(&blk).unwrap();
    bytes.push(0);
    std::fs::write(&blk, bytes).unwrap();
    let resumed = run(&dest, &[&flag(&base)], |_| {});
    assert!(!resumed.ok);
    assert!(has(&resumed.stderr, "the source changed since staging"), "{:?}", resumed.stderr);
    let journal = Journal::open(&journal_path(&dest), &OsImportFs::new()).unwrap();
    assert!(journal.records().is_empty(), "the journal is at its magic");
    let staging: Vec<PathBuf> = std::fs::read_dir(dest.join("regtest").join(STAGING))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(staging, [journal_path(&dest)], "staging is cleared");
    let again = run(&dest, &[&flag(&base)], |_| {});
    assert!(again.ok, "{:?}", again.stderr);
    assert_eq!(again.tip.as_deref(), Some(best.as_str()));
    assert_eq!(last(&dest), Some(Phase::Reported));
}

/// P4 refuses a store holding a chainstate (a coins record alone, a coins tip, a transition
/// marker or an evo tip) and writes nothing: the journal stays at `Placed`, and the store's
/// reindex flag is not set.
#[test]
fn p4_refuses_a_forged_store_and_writes_nothing() {
    for forged in ["coins", "tip", "head_blocks", "evo_tip"] {
        let dir = tempfile::tempdir().unwrap();
        let (base, _) = source(dir.path());
        let dest = destination(dir.path());
        let kept = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&kept);
        let forged_hash = BlockHash::from([7; 32]);
        let started = run(&dest, &[&flag(&base)], |node| {
            node.steps.before_open = Some(Box::new(move |opening| {
                let engine = opening.engine_for_test();
                let mut batch = engine.batch();
                match forged {
                    "coins" => {
                        let cf = dashd_chainstate::store::CHAINSTATE_SCHEMA.cf("coins").unwrap();
                        batch.put(cf, b"C-forged", b"x");
                    }
                    "tip" => dashd_chainstate::store::Meta::put_tip(&mut batch, &forged_hash),
                    "head_blocks" => dashd_chainstate::store::Meta::put_head_blocks(
                        &mut batch,
                        &forged_hash,
                        None,
                    ),
                    _ => dashd_chainstate::store::Meta::put_evo_tip(&mut batch, &forged_hash),
                }
                engine.write(batch, Commit::Durable).unwrap();
                *seen.lock().unwrap() = Some(Arc::clone(engine));
            }));
        });
        assert!(!started.ok, "{forged}");
        let text = "the destination store is not empty";
        assert!(has(&started.stderr, text), "{forged}: {:?}", started.stderr);
        assert_eq!(last(&dest), Some(Phase::Placed), "{forged}");
        let engine = kept.lock().unwrap().take().unwrap();
        let meta = dashd_chainstate::store::Meta::read(&*engine).unwrap();
        assert!(!meta.reindexing, "{forged}: no reindex flag");
    }
}

/// At `Reported`: the same source logs once and starts normally, also once it is gone; a
/// mismatch repeats its error line and starts; another source is `DestinationNotEmpty`.
#[test]
fn a_reported_import_with_the_flag_again() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    assert!(run(&dest, &[&flag(&base)], |_| {}).ok);
    let same = run(&dest, &[&flag(&base)], |_| {});
    assert!(same.ok, "{:?}", same.stderr);
    assert_eq!(same.tip.as_deref(), Some(best.as_str()));
    assert!(has(&same.log, "Core datadir import: already imported from"), "{:?}", same.log);
    assert!(!has(&same.log, "Reindexing"));

    let other = dir.path().join("other");
    let (other_base, _) = source(&other);
    let (_, params) = config(&dest, &[&flag(&other_base)]);
    let err = params.unwrap_err();
    assert!(format!("{err:?}").contains("destination is not empty"), "{err:?}");

    std::fs::remove_dir_all(&base).unwrap();
    let gone = run(&dest, &[&flag(&base)], |_| {});
    assert!(gone.ok, "the same path with its source gone: {:?}", gone.stderr);
    assert_eq!(gone.tip.as_deref(), Some(best.as_str()));
    assert!(has(&gone.log, "Core datadir import: already imported from"), "{:?}", gone.log);
    assert!(!has(&gone.log, "Reindexing"));
}

/// The source's blk file cut short of its best block: the scan completes, the ABC succeeds,
/// and the tip does not hold the best block, so the import is `mismatch`: `Reported` with its
/// error line, repeated on the next start with the flag, which then starts normally.
#[test]
fn a_tip_that_misses_the_best_block_is_a_mismatch() {
    struct Cut(PathBuf);
    impl BlkFiles for Cut {
        fn open(&self, file: u32) -> BlkOpen {
            let path = self.0.join(format!("blk{file:05}.dat"));
            let Ok(bytes) = std::fs::read(&path) else {
                return BlocksDir(self.0.clone()).open(file);
            };
            let cut = bytes.len() / 2;
            BlkOpen::Open(Box::new(std::io::Cursor::new(bytes[..cut].to_vec())))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    let blocks = dest.join("regtest/blocks");
    let started =
        run(&dest, &[&flag(&base)], |node| node.steps.blk_files = Some(Box::new(Cut(blocks))));
    assert!(started.ok, "{:?}", started.stderr);
    assert_ne!(started.tip.as_deref(), Some(best.as_str()));
    assert_eq!(last(&dest), Some(Phase::Reported));
    let line = "Error: Core datadir import: the import did not reproduce the source's tip";
    assert!(has(&started.log, line), "{:?}", started.log);
    let again = run(&dest, &[&flag(&base)], |_| {});
    assert!(again.ok, "{:?}", again.stderr);
    assert!(has(&again.log, line));
}

#[test]
fn the_import_options_need_the_directory_and_known_values() {
    let dir = tempfile::tempdir().unwrap();
    let (base, _) = source(dir.path());
    let dest = destination(dir.path());
    let import = flag(&base);
    // `init_config` checks the options of a start with `-importcoredatadir`; step 2 the rest.
    for (argv, text, at_config) in [
        (vec!["-importmode=safe"], "-importmode requires -importcoredatadir.", false),
        (vec!["-importsource=lock"], "-importsource requires -importcoredatadir.", false),
        (vec!["-importplacement=reflink"], "-importplacement requires -importcoredatadir.", false),
        (vec!["-importcoredatadir="], "-importcoredatadir requires a directory.", true),
        (vec![import.as_str(), "-importmode=fast"], "-importmode=fast is not supported by", true),
        (vec![import.as_str(), "-importmode=x"], "Unknown -importmode value x.", true),
        (vec![import.as_str(), "-importsource=x"], "Unknown -importsource value x.", true),
        (vec![import.as_str(), "-importplacement=x"], "Unknown -importplacement value x.", true),
        (
            vec![import.as_str(), "-importsource=snapshot", "-importplacement=hardlink"],
            "Core datadir import: ",
            true,
        ),
        (
            vec![import.as_str(), "-reindex-chainstate"],
            "-reindex-chainstate is incompatible with -importcoredatadir.",
            false,
        ),
    ] {
        let (_, params) = config(&dest, &argv);
        if at_config {
            let err = params.unwrap_err();
            assert!(format!("{err:?}").contains(text), "{argv:?}: {err:?}");
        } else {
            assert!(params.is_ok(), "{argv:?}");
            let (mut node, _, stderr) = start(&dest, &argv);
            assert!(!node.app_init_early(), "{argv:?}");
            node.shutdown();
            assert!(has(&stderr, text), "{argv:?}: {stderr:?}");
        }
        assert!(!dest.join("regtest").join(STAGING).exists(), "{argv:?}");
    }
    let started = run(&dest, &[&import, "-reindex"], |_| {});
    assert!(started.ok, "{:?}", started.stderr);
    assert!(has(&started.log, "-importcoredatadir: the import reindexes its block files itself"));
    assert_eq!(last(&dest), Some(Phase::Reported));
}

#[test]
fn the_restart_after_an_import_has_clean_index_statuses() {
    let dir = tempfile::tempdir().unwrap();
    let (base, best) = source(dir.path());
    let dest = destination(dir.path());
    assert!(run(&dest, &[&flag(&base)], |_| {}).ok);
    let (mut node, _, stderr) = start(&dest, &[]);
    assert!(node.app_init_early() && node.app_init_main(), "{stderr:?}");
    node.steps.join_import();
    assert_eq!(tip(&node), best);
    clean_index_statuses(&node);
    node.shutdown();
}
