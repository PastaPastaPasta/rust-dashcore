//! CP3.2 regressions from the internal review of the DEC-148 change (1d722bad) that DEC-162
//! keeps: the model's `Extended` garbage outcomes. The provenance ("observed") cases went with the
//! mechanism; R7a's witness rule replaces them (`power_loss_cp33_witness.rs`).
//!
//! - `cp32_garbage_keeps_durable_bytes`: under `Extended`, a unit whose durable version an earlier
//!   power loss wrapped for EIO still gets its garbage option from its durable bytes, so bytes no
//!   pending write touched never turn into garbage (A-PSOW).
//! - `cp32r2_garbage_survives_an_earlier_power_loss`: a unit an earlier `Extended` power loss
//!   wrapped still gets its garbage outcome.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use dashd_store::faulty::{
    Adversary, CrashKind, Dfs, FileRef, Fixed, Granularity, World, WorldConfig,
};
use dashd_store::flatfile::FlatFs;

fn path(name: &str) -> PathBuf {
    Path::new("/dd").join(name)
}

fn cfg(adversary: Adversary, granularity: Granularity) -> WorldConfig {
    WorldConfig {
        adversary,
        granularity,
        ..WorldConfig::default()
    }
}

fn fixed(c: &[(&str, u32)]) -> Box<Fixed> {
    Box::new(Fixed::new(c.iter().map(|(n, v)| (n.to_string(), *v)).collect::<BTreeMap<_, _>>()))
}

/// A durable file of `durable`, synced, then `pending` writes left unsynced.
fn file(c: WorldConfig, durable: &[u8], pending: &[(u64, &[u8])]) -> World {
    let w = World::new(c);
    let fs = w.fs();
    fs.write_all_at(&path("f"), 0, durable).unwrap();
    fs.sync_data(&path("f")).unwrap();
    fs.sync_dir(Path::new("/dd")).unwrap();
    for (at, data) in pending {
        fs.write_all_at(&path("f"), *at, data).unwrap();
    }
    w
}

#[test]
fn cp32_garbage_keeps_durable_bytes() {
    let c = move || cfg(Adversary::Extended, Granularity::Sector);
    let w = file(c(), &[7; 512], &[(0, &[9; 10])]);
    let w = w.capture().boot(CrashKind::PowerLoss, fixed(&[]), c());
    // A partial write reads the block (deciding its readability) and resolves the unit.
    w.fs().write_all_at(&path("f"), 0, &[9; 10]).unwrap();
    let img = w.capture();
    let dfs = Dfs::new();
    let (mut n, mut bad) = (0, Vec::new());
    loop {
        let w = img.boot(CrashKind::PowerLoss, dfs.decider(), c());
        let mut buf = [0u8; 100];
        match w.fs().read_exact_at(&path("f"), 100, &mut buf) {
            Ok(()) if buf == [7; 100] => {}
            Err(e) if e.to_string().starts_with("EIO") => {}
            r => bad.push(format!("{r:?} {:x?}", &buf[..4])),
        }
        let d1 = w.durable_view().range_durable(&FileRef::new("/dd/f", 100, 100));
        assert!(d1.is_ok(), "the untouched bytes are durable: {d1:?}");
        n += 1;
        if !dfs.advance() {
            break;
        }
    }
    assert!(n > 1);
    assert!(
        bad.is_empty(),
        "durable bytes [100,200) changed in {} of {n} branches: {bad:?}",
        bad.len()
    );
}

/// A torn write still has its garbage outcome after an earlier `Extended` power loss wrapped
/// the unit for EIO (as after a process kill).
#[test]
fn cp32r2_garbage_survives_an_earlier_power_loss() {
    let c = || cfg(Adversary::Extended, Granularity::Sector);
    for first in [CrashKind::PowerLoss, CrashKind::ProcessKill] {
        let w = file(c(), &[7; 512], &[(0, &[9; 10])]);
        let w = w.capture().boot(first, fixed(&[]), c());
        w.fs().write_all_at(&path("f"), 20, &[5; 10]).unwrap();
        let img = w.capture();
        let w = img.boot(CrashKind::PowerLoss, fixed(&[]), c());
        let (cell, domain) = w
            .vars()
            .into_iter()
            .map(|(v, _)| v)
            .find(|v| v.name.starts_with("2:fs:i1:u0"))
            .map(|v| (v.name, v.domain))
            .expect("the unit's cell");
        let readable = format!("2:fs:i1:eio0{}", &cell["2:fs:i1:u0".len()..]);
        let garbage = (0..domain).any(|k| {
            let w = img.boot(CrashKind::PowerLoss, fixed(&[(&cell, k), (&readable, 0)]), c());
            let mut b = [0u8; 512];
            w.fs().read_exact_at(&path("f"), 0, &mut b).is_ok()
                && b[20..30] == [0xa5; 10]
                && b[100..] == [7; 412]
        });
        assert!(garbage, "{first:?}: no garbage outcome in {cell}'s {domain} choices");
    }
}
