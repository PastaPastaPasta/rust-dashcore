use std::path::PathBuf;

use super::*;
use crate::init::quorums::{QUORUM_DB_DIR, QuorumDb, WipeStep};

pub(crate) const ROW: (&[u8], &[u8]) = (b"\x04rs_s-recsig-row", &[1]);

/// Writes the test row through `db`.
pub(crate) fn plant(db: &mut RecoveredSigsDb) {
    let mut batch = RecSigBatch::default();
    batch.put(ROW.0.to_vec(), ROW.1.to_vec());
    db.write(batch).unwrap();
}

/// Whether `db` holds the test row.
pub(crate) fn planted(db: &RecoveredSigsDb) -> bool {
    db.get(ROW.0).unwrap().is_some_and(|v| v == ROW.1)
}

fn opened(net: &Path, wipe: bool) -> RecoveredSigsDb {
    let db = RecoveredSigsDb::unopened();
    db.open(net, wipe).unwrap();
    db
}

/// Before the open each call is an error, not a panic; a clone shares the open, and a second
/// open is refused.
#[test]
fn an_unopened_handle_errors_and_never_panics() {
    let mut db = RecoveredSigsDb::unopened();
    assert!(db.get(ROW.0).is_err());
    assert!(db.scan_from(&[]).is_err());
    assert!(db.write(RecSigBatch::default()).is_err());
    let mut kept = db.clone();
    let dir = tempfile::tempdir().unwrap();
    db.open(dir.path(), false).unwrap();
    plant(&mut kept);
    assert!(planted(&db));
    let again = db.open(dir.path(), false).unwrap_err();
    assert!(matches!(again, LlmqDbError::AlreadyOpen { .. }), "{again:?}");
    assert_eq!(again.to_string(), "the recovered signature database is already open");
}

/// Rows survive a reopen without a wipe; a wipe empties the database, and a later open without
/// one finds it empty. The quorum database beside it keeps its rows through both.
// Core: v24.0.2 src/dbwrapper.cpp:143-158, src/node/chainstate.cpp:149, :159-160
#[test]
fn rows_survive_a_reopen_and_a_wipe_empties_only_the_recsig_database() {
    let dir = tempfile::tempdir().unwrap();
    QuorumDb::unopened().open(dir.path(), false).unwrap();
    let quorum_files = || {
        let mut files: Vec<_> = std::fs::read_dir(dir.path().join(QUORUM_DB_DIR))
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                (path.clone(), std::fs::read(path).unwrap())
            })
            .collect();
        files.sort();
        files
    };
    let before = quorum_files();
    plant(&mut opened(dir.path(), false));
    assert!(planted(&opened(dir.path(), false)));
    assert!(dir.path().join(RECSIG_DB_DIR).join("CURRENT").is_file());
    assert!(!planted(&opened(dir.path(), true)));
    assert!(!planted(&opened(dir.path(), false)));
    assert_eq!(quorum_files(), before, "the quorum database is untouched");
}

/// A wipe a crash left in `recsigdb.wipe` is finished by any open and the live database is kept;
/// a failed rename keeps the database, a failed sync or delete leaves the moved copy for the next
/// open, which finishes it.
#[test]
fn an_interrupted_wipe_is_finished_at_the_next_open() {
    let dir = tempfile::tempdir().unwrap();
    let leftover = dir.path().join(WIPING_DIR);
    plant(&mut opened(dir.path(), false));
    std::fs::create_dir_all(leftover.join("sub")).unwrap();
    std::fs::write(leftover.join("sub").join("000004.log"), "old").unwrap();
    assert!(planted(&opened(dir.path(), false)));
    assert!(!leftover.exists(), "the interrupted wipe is finished");

    let db = RecoveredSigsDb::unopened();
    db.fail_wipe_at(WipeStep::Rename);
    let open = db.open(dir.path(), true).unwrap_err();
    assert!(matches!(open, LlmqDbError::Wipe { .. }), "{open:?}");
    assert!(open.to_string().contains("injected recovered signature database Rename failure"));
    assert!(planted(&opened(dir.path(), false)), "a failed rename keeps the rows");
    for step in [WipeStep::Sync, WipeStep::Delete] {
        plant(&mut opened(dir.path(), false));
        let db = RecoveredSigsDb::unopened();
        db.fail_wipe_at(step);
        let open = db.open(dir.path(), true).unwrap_err();
        let path = match &open {
            LlmqDbError::Sync {
                path,
                ..
            } if step == WipeStep::Sync => path,
            LlmqDbError::Wipe {
                path,
                ..
            } if step == WipeStep::Delete => path,
            _ => panic!("{step:?}: {open:?}"),
        };
        assert_eq!(*path, leftover, "{step:?}");
        assert!(!dir.path().join(RECSIG_DB_DIR).exists() && leftover.exists(), "{step:?}");
        assert!(!planted(&opened(dir.path(), false)), "{step:?}");
        assert!(!leftover.exists(), "{step:?}: finished by the next open");
    }
}

/// A non-directory at `recsigdb` fails the open with or without a wipe, and a wipe leaves it.
// Core: v24.0.2 src/dbwrapper.cpp:143-156, src/leveldb/db/db_impl.cc:1525-1532
#[test]
fn a_non_directory_fails_the_open() {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join(RECSIG_DB_DIR);
    std::fs::write(&path, "not a database").unwrap();
    let open = RecoveredSigsDb::unopened().open(dir.path(), false).unwrap_err();
    assert!(open.to_string().starts_with("Error opening the recovered signature database"));
    let open = RecoveredSigsDb::unopened().open(dir.path(), true).unwrap_err();
    assert!(matches!(&open, LlmqDbError::NotADirectory { path: p, .. } if *p == path), "{open:?}");
    assert_eq!(std::fs::read(&path).unwrap(), b"not a database");
    assert!(!dir.path().join(WIPING_DIR).exists());
}

/// The in-memory database is open and empty when made, keeps its rows through an open with a
/// wipe, and writes no file.
#[test]
fn in_memory_ignores_wipe_and_starts_empty() {
    let dir = tempfile::tempdir().unwrap();
    let mut db = RecoveredSigsDb::in_memory();
    plant(&mut db);
    db.open(dir.path(), true).unwrap();
    assert!(planted(&db));
    assert!(!planted(&RecoveredSigsDb::in_memory()));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

/// The directories are not ones rdashd takes for Dash Core's (`evodb`, `llmq`).
#[test]
fn the_recsig_database_is_not_a_core_sign() {
    let dir = tempfile::tempdir().unwrap();
    plant(&mut opened(dir.path(), false));
    assert_ne!(RECSIG_DB_DIR, QUORUM_DB_DIR);
    assert!(crate::datadir::probe_core_data_dir(dir.path()).is_none());
}

/// `-maxrecsigsage` is `GetIntArg` with a week's default: unchecked, so a negative or
/// non-numeric value passes as Core reads it.
// Core: v24.0.2 src/init.cpp:2075, src/llmq/options.h:30-31
#[test]
fn max_recsigs_age_is_get_int_arg_with_a_weeks_default() {
    for (argv, age) in [
        (&[][..], 604_800),
        (&["-maxrecsigsage=60"][..], 60),
        (&["-maxrecsigsage=-5"][..], -5),
        (&["-maxrecsigsage=abc"][..], 0),
    ] {
        let args = crate::init::test_args(argv);
        assert_eq!(max_recsigs_age(&args).unwrap(), age, "{argv:?}");
    }
}
