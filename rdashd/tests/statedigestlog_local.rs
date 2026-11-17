//! Production digest option and I/O failures without an oracle binary.
#![cfg(unix)]

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::mine::{coinbase_paying, next_with};
use common::node::{Node, Storage, genesis};
use common::*;

const OFFLINE: [&str; 5] =
    ["-connect=0", "-dnsseed=0", "-fixedseeds=0", "-checklevel=2", "-rpcdoccheck=0"];

fn listing(dir: &Path) -> BTreeSet<PathBuf> {
    fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().into()).collect()
}

#[test]
fn disabled_digest_does_not_create_or_append_a_file() {
    let home = RetainedHome::new("statedigest-off");
    for option in [
        None,
        Some("-statedigestlog"),
        Some("-statedigestlog="),
        Some("-nostatedigestlog"),
        Some("-statedigestlog=unused"),
    ] {
        for preexisting in [false, true] {
            let mut node = Node::rdashd(home.path(), &OFFLINE, Storage::Live);
            if let Some(option) = option {
                node.flags.push(option.to_owned());
                if option.ends_with("unused") {
                    node.flags.push("-nostatedigestlog".to_owned());
                }
            }
            let marker = node.datadir.join("regtest/digest.jsonl");
            private_dir(marker.parent().unwrap());
            if preexisting {
                fs::write(&marker, b"unchanged\n").unwrap();
            }
            let before = listing(marker.parent().unwrap());
            node.start(0);
            let block = next_with(&genesis(&node), 1, coinbase_paying(1, b"off", &[0]));
            assert_eq!(node.submit(&block), "null");
            node.stop();
            if preexisting {
                assert_eq!(fs::read(&marker).unwrap(), b"unchanged\n");
            } else {
                assert!(!marker.exists());
            }
            assert!(!node.datadir.join("regtest/unused").exists());
            assert!(!node.datadir.join("regtest/1").exists());
            let after = listing(marker.parent().unwrap());
            let known: BTreeSet<PathBuf> = [
                ".lock",
                "anchors.dat",
                "banlist.json",
                "blocks",
                "chainstate",
                "debug.log",
                "fee_estimates.dat",
                "mncache.dat",
                "netfulfilled.dat",
                "peers.dat",
                "settings.json",
                "sporks.dat",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect();
            assert!(
                after.difference(&before).all(|name| known.contains(name)),
                "unexpected network files: {:?}",
                after.difference(&before).collect::<Vec<_>>()
            );
            fs::remove_dir_all(&node.datadir).unwrap();
        }
    }
}

#[test]
fn digest_open_and_header_write_failures_refuse_startup() {
    for (path, message) in [
        ("missing/digest.jsonl", "cannot open"),
        ("/", "cannot open"),
        #[cfg(target_os = "linux")]
        ("/dev/full", "statedigest: writing the digest log:"),
    ] {
        let home = RetainedHome::new("statedigest-io-failure");
        let datadir = home.path().join("node");
        private_dir(&datadir);
        let option = format!("-statedigestlog={path}");
        let result = output(regtest(home.path(), &datadir, &["-server=0", &option]));
        assert_eq!(result.status.code(), Some(1), "{}", stderr(&result));
        let log = read(&debug_log(&datadir));
        assert!(
            format!("{}\n{log}", stderr(&result)).contains(message),
            "{}\n{log}",
            stderr(&result)
        );
    }
}

#[test]
fn digest_open_failure_precedes_reindex_chainstate_wipe() {
    let home = RetainedHome::new("statedigest-reindex-failure");
    let mut node = Node::rdashd(home.path(), &OFFLINE, Storage::Live);
    node.start(0);
    let block = next_with(&genesis(&node), 1, coinbase_paying(1, b"keep", &[1]));
    assert_eq!(node.submit(&block), "null");
    node.stop();
    // Coins and evo are column families of the same RocksDB database.
    let database = node.datadir.join("regtest/chainstate");
    let before = tree(&database);
    assert!(!before.is_empty());
    let offset = node.log_len();
    let flags: Vec<_> = node.flags.iter().map(String::as_str).collect();
    let extra = [
        flags.as_slice(),
        &["-server=0", "-reindex-chainstate", "-statedigestlog=missing/d.jsonl"],
    ]
    .concat();
    let result = output(regtest(home.path(), &node.datadir, &extra));
    assert_eq!(result.status.code(), Some(1), "{}", stderr(&result));
    let log = read_since(&node.log(), Some(offset));
    let message = format!(
        "cannot open {} for -statedigestlog",
        node.datadir.join("regtest/missing/d.jsonl").display()
    );
    assert!(format!("{}\n{log}", stderr(&result)).contains(&message), "{}\n{log}", stderr(&result));
    assert!(!log.contains("Loading block index"), "{log}");
    assert_eq!(tree(&database), before, "coins/evo database changed before the open refusal");
}
