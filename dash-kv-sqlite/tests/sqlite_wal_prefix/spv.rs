//! The SPV store's workload (`dash_kv::faulty::spv`) on SQLite under recorded power-loss images,
//! checked by S1–S4 (SB10 plan §6, C10).
//!
//! The SPV database runs through the recording VFS; every crash point gives sampled images of it.
//! The wallet's database has a directory of its own, outside the recording: an image pairs the SPV
//! image with the wallet's files as a process kill leaves them after the last step acknowledged
//! before the crash point, so the wallet is ahead of the SPV store wherever that lost Buffered
//! steps (the skew recovery's rewind is for). The wallet's own losses and crashes during recovery
//! are `dash_kv::faulty::spv`'s explorer's alone.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use dash_kv::DbSchema;
use dash_kv::faulty::spv::{
    SPV_SCHEMA, SpvBug, SpvOp, SpvRef, SpvState, SpvStore, WALLET_SCHEMA, WORKLOAD,
    check_recovered, check_stores,
};
use dash_kv::faulty::{InvariantId, Reference, Violation};
use dash_kv_sqlite::{SqliteConfig, SqliteEngine};

use crate::image::{Files, Point, digest, required};
use crate::{config, read_files, vfs};

/// Images enumerated per crash point at most; above this, a point is sampled. Far fewer than the
/// plain scenarios' `EXHAUSTIVE`: each image opens two engines, recovers and closes them, and a
/// close's checkpoint truncates the database file, which XFS flushes (about 50 ms at load 100).
const ENUMERATED: u64 = 32;

/// The workload's wallet runs outside the recording (`recorded` false); images open through the
/// recording VFS with the recording off, where syncs cost nothing.
fn open(dir: &Path, schema: &'static DbSchema, recorded: bool) -> Arc<SqliteEngine> {
    let config = if recorded {
        config()
    } else {
        SqliteConfig::default()
    };
    Arc::new(SqliteEngine::open(dir, schema, &config).expect("opens").0)
}

fn write_files(dir: &Path, files: &Files) {
    std::fs::create_dir_all(dir).expect("a fresh image dir");
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).expect("writes the image");
    }
}

/// Runs the workload with `bug` and checks every crash point's images, or until `until` is found;
/// the violations found, at most one per invariant, with where.
fn run(bug: Option<SpvBug>, until: Option<InvariantId>) -> Vec<(Violation, usize)> {
    let _serial = vfs::serial();
    vfs::register();
    let root = tempfile::tempdir().expect("a scratch dir under TMPDIR");
    let (spv_dir, wallet_dir) = (root.path().join("spv"), root.path().join("wallet"));
    let mut store = SpvStore::open(
        open(&spv_dir, &SPV_SCHEMA, true),
        open(&wallet_dir, &WALLET_SCHEMA, false),
        bug,
    )
    .expect("opens");
    let base = read_files(&spv_dir);
    let mut wallets = vec![read_files(&wallet_dir)];
    let mut refm = SpvRef::new(&Default::default());
    vfs::start();
    for (i, op) in
        WORKLOAD.split_whitespace().map(|o| o.parse::<SpvOp>().expect("an op")).enumerate()
    {
        match op {
            SpvOp::Boundary => store.boundary(),
            op => store.step(op),
        }
        .expect("the step commits");
        refm.apply(&op);
        wallets.push(read_files(&wallet_dir));
        vfs::acked(i + 1, op == SpvOp::Boundary);
    }
    let log = vfs::stop();
    drop(store);
    let (mut found, mut seen, mut images) = (Vec::<(Violation, usize)>::new(), HashSet::new(), 0);
    for at in 0..=log.len() {
        let point = Point::new(&base, &log, at);
        let choices = if point.choices() <= ENUMERATED {
            point.all()
        } else {
            point.sampled(6, 6, at as u64)
        };
        // The wallet as the last step acknowledged before the crash left it.
        let wallet_files =
            &wallets[log[..at].iter().filter(|e| matches!(e, vfs::Event::Acked { .. })).count()];
        for choice in &choices {
            let files = point.image(choice);
            if !seen.insert((digest(&files), digest(wallet_files))) {
                continue;
            }
            images += 1;
            let dir = root.path().join(format!("image-{images}"));
            write_files(&dir.join("spv"), &files);
            write_files(&dir.join("wallet"), wallet_files);
            let spv = open(&dir.join("spv"), &SPV_SCHEMA, true);
            let wallet = open(&dir.join("wallet"), &WALLET_SCHEMA, true);
            let booted = SpvState::load(&*spv, &*wallet).expect("reads");
            let mut v = check_stores(&booted);
            let recovered = SpvStore::open(spv, wallet, bug).expect("recovers");
            let st = recovered.persisted().expect("reads");
            v.extend(check_recovered(&st, &refm, required(&log, at)));
            drop(recovered);
            std::fs::remove_dir_all(&dir).expect("removes the image");
            for v in v {
                if !found.iter().any(|(f, _)| f.invariant == v.invariant) {
                    found.push((v, at));
                }
            }
            if found.iter().any(|(v, _)| Some(v.invariant) == until) {
                return found;
            }
        }
    }
    eprintln!("{bug:?}: {} events, {images} distinct images, found {found:?}", log.len());
    assert!(images > 400, "{images}");
    found
}

#[test]
fn the_spv_workload_recovers_on_sqlite() {
    let found = run(None, None);
    assert!(found.is_empty(), "{found:?}");
}

/// The images find what the explorer finds: B1's torn subscriber step (S2) and B5's Buffered
/// boundary (S3).
#[test]
fn planted_spv_bugs_show_on_sqlite() {
    for (bug, expect) in [(SpvBug::B1, InvariantId::S2), (SpvBug::B5, InvariantId::S3)] {
        let found = run(Some(bug), Some(expect));
        assert!(found.iter().any(|(v, _)| v.invariant == expect), "{bug:?}: {found:?}");
    }
}
