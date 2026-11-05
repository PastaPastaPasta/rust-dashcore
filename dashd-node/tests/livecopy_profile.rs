//! CH5-5 §6.7 `live_path_isolation` (D2-8, OD-CH5-17; M13, M14): the live-copy write path's
//! work happens on regtest's live profile only. Every other profile `chainstate_profile` picks
//! takes no live lock, reads no sample property and records no sample, and keeps RocksDB's
//! concurrent memtable writes; the live profile does all three on every commit, with them off.
//! The probe is per engine, so parallel tests do not disturb it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use dash_params::Network;
use dashd_chainstate::caches::{CacheSizes, DEFAULT_DB_CACHE, IndexArgs};
use dashd_chainstate::store::CHAINSTATE_SCHEMA;
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};
use dashd_node::chainstate::{StorageProfile, chainstate_profile};
use dashd_store::{Cf, Commit, KvBatch, KvEngine, ProbeCounters, RocksEngine};

const BATCHES: u64 = 1000;

/// The probe's three live-path counts.
fn counts(p: &ProbeCounters) -> [u64; 3] {
    [
        p.live_lock_acquires.load(Ordering::Relaxed),
        p.live_property_reads.load(Ordering::Relaxed),
        p.live_state_updates.load(Ordering::Relaxed),
    ]
}

/// The number of a final RocksDB options file: `OPTIONS-<digits>`. `OPTIONS-<n>.dbtmp` is a
/// temp file a cut write leaves (RocksDB never purges it), and the width of the number is a
/// minimum, so files are compared by number, not by name.
fn options_number(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("OPTIONS-")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The final options file with the highest number in `dir`.
fn newest_options(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| {
            let path = e.unwrap().path();
            Some((options_number(path.file_name()?.to_str()?)?, path))
        })
        .max_by_key(|(number, _)| *number)
        .map(|(_, path)| path)
}

#[test]
fn the_newest_options_file_is_the_highest_number() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "OPTIONS-000005",
        "OPTIONS-999999",
        "OPTIONS-1000001",
        "OPTIONS-1000002.dbtmp",
        "OPTIONS-",
        "OPTIONS-12x",
        "LOG",
    ] {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    let newest = newest_options(dir.path()).unwrap();
    assert_eq!(newest.file_name().unwrap(), "OPTIONS-1000001");
    std::fs::remove_file(newest).unwrap();
    assert_eq!(newest_options(dir.path()).unwrap().file_name().unwrap(), "OPTIONS-999999");
}

/// The newest options file's `key=value` lines.
fn options(dir: &Path) -> BTreeMap<String, String> {
    let newest = newest_options(dir).expect("an OPTIONS file");
    std::fs::read_to_string(newest)
        .unwrap()
        .lines()
        .filter_map(|l| l.trim().split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Opens the chainstate under `tuning` in a fresh directory, commits [`BATCHES`] batches spread
/// over every column family, and returns the probe's counts over those commits and the
/// database's `allow_concurrent_memtable_write`.
fn commit_batches(tuning: &RocksTuning) -> ([u64; 3], String) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("chainstate");
    let sizes = CacheSizes::calculate(DEFAULT_DB_CACHE, &IndexArgs::default());
    let (db, _): (RocksEngine, _) = tuning.open_chainstate(&path, &sizes, 1 << 20).unwrap();
    let cfs = CHAINSTATE_SCHEMA.cfs.len() as u64;
    assert_eq!(cfs, 11, "the chainstate's column families");
    let before = counts(db.probe());
    for i in 0..BATCHES {
        let mut b = db.batch();
        let cf = Cf::new(u16::try_from(i % cfs).unwrap());
        b.put(cf, format!("live-path-isolation-{i:04}").as_bytes(), &[0x5a; 64]);
        db.write(b, Commit::Buffered).unwrap();
    }
    let after = counts(db.probe());
    let concurrent = options(&path)["allow_concurrent_memtable_write"].clone();
    (std::array::from_fn(|k| after[k] - before[k]), concurrent)
}

#[test]
fn live_path_isolation() {
    let quiet = [
        (Network::Main, None),
        (Network::Testnet3, None),
        (Network::Devnet, None),
        (Network::Regtest, Some(StorageProfile::Production)),
    ];
    for (network, requested) in quiet {
        let (tuning, live) = chainstate_profile(network, requested);
        assert!(!live, "{network:?} {requested:?}");
        let (counts, concurrent) = commit_batches(&tuning);
        assert_eq!(counts, [0, 0, 0], "{network:?} {requested:?}: no live-path work");
        assert_eq!(concurrent, "true", "{network:?} {requested:?}");
    }

    let (tuning, live) = chainstate_profile(Network::Regtest, None);
    assert!(live);
    let ([locks, reads, updates], concurrent) = commit_batches(&tuning);
    assert_eq!(locks, BATCHES, "one live lock a commit");
    assert_eq!(reads, BATCHES * 11, "one sample a column family a commit");
    assert!(updates >= BATCHES, "{updates}");
    assert_eq!(concurrent, "false");
}
