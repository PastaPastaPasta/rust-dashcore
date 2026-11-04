//! CH5-5 §6.7 `live_path_isolation` (D2-8, OD-CH5-17; M13, M14): the live-copy write path's
//! work happens on regtest's live profile only. Every other profile `chainstate_profile` picks
//! takes no live lock, reads no sample property and records no sample, and keeps RocksDB's
//! concurrent memtable writes; the live profile does all three on every commit, with them off.
//! The probe is per engine, so parallel tests do not disturb it.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;

use dash_params::Network;
use dashd_chainstate::caches::{CacheSizes, DEFAULT_DB_CACHE, IndexArgs};
use dashd_chainstate::store::CHAINSTATE_SCHEMA;
use dashd_chainstate::tuning::{EngineTuning, RocksTuning};
use dashd_node::chainstate::{ProfileSeam, chainstate_profile};
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

/// The newest `OPTIONS-*` file's `key=value` lines.
fn options(dir: &Path) -> BTreeMap<String, String> {
    let newest = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("OPTIONS-"))
        .max()
        .expect("an OPTIONS file");
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
        (Network::Regtest, Some(ProfileSeam::Production)),
    ];
    for (network, seam) in quiet {
        let (tuning, live) = chainstate_profile(network, seam);
        assert!(!live, "{network:?} {seam:?}");
        let (counts, concurrent) = commit_batches(&tuning);
        assert_eq!(counts, [0, 0, 0], "{network:?} {seam:?}: no live-path work");
        assert_eq!(concurrent, "true", "{network:?} {seam:?}");
    }

    let (tuning, live) = chainstate_profile(Network::Regtest, None);
    assert!(live);
    let ([locks, reads, updates], concurrent) = commit_batches(&tuning);
    assert_eq!(locks, BATCHES, "one live lock a commit");
    assert_eq!(reads, BATCHES * 11, "one sample a column family a commit");
    assert!(updates >= BATCHES, "{updates}");
    assert_eq!(concurrent, "false");
}

/// The seam (CH5-5 Q3) in a test-hooks build: `production` asks for the production profile, an
/// unset variable for none; it is read through the closure, so the test sets no environment.
#[test]
fn the_profile_seam_reads_production_only() {
    use dashd_node::chainstate::profile_seam;

    let var = |value: Option<&'static str>| {
        move |name: &str| {
            assert_eq!(name, "RDASHD_TEST_HOOK_PROFILE");
            value.map(str::to_owned)
        }
    };
    assert_eq!(profile_seam(var(None)), None);
    assert_eq!(profile_seam(var(Some("production"))), Some(ProfileSeam::Production));
    let typo = std::panic::catch_unwind(|| profile_seam(var(Some("prodution"))));
    assert!(typo.is_err(), "a mistyped seam stops the node");
}
