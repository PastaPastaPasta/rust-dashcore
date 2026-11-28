//! CH5-4b's regtest datadirs in Dash Core's layout: the framework's cache as dashd v24 builds it
//! (`_initialize_chain`), copied as the framework copies it, and what rdashd's import leaves.
// Core: test_framework.py:948-1022, :632-650; util.py:503-514 at v24.0.2

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::net::{
    ADDRESS_BCRT1_P2SH_OP_TRUE, Binary, Json, NODE_ADDRESSES, Node, TIME_GENESIS_BLOCK,
};
use super::{RetainedHome, net, private_dir};

/// The directories the framework's cache keeps (`test_framework.py:1020`).
pub const CACHE_KEPT: [&str; 5] = ["chainstate", "blocks", "indexes", "evodb", "llmq"];

/// The framework's mocktime once the cache is built and its copies start
/// (`_initialize_mocktime(is_genesis=False)`).
pub const CACHE_MOCKTIME: u64 = TIME_GENESIS_BLOCK + 199 * 156;

/// The journal's file name and its `Complete` phase byte (`CH5RJ1 ‖ version ‖ phase`).
pub const JOURNAL: &str = "core-import.journal";
pub const BACKUP_PREFIX: &str = "core-import-";

/// The members rdashd's import moves, relative to the network directory.
pub const MEMBERS: [&str; 5] = ["chainstate", "blocks/index", "evodb", "llmq", "indexes"];

/// The framework's cache in `home/node0`: dashd mines seven batches of 25 blocks and one of 24 to
/// the rotating addresses, mocktime first advancing by each batch's size × 156 s; then it stops,
/// and every entry of `regtest/` but [`CACHE_KEPT`] is removed. Returns the datadir and the tip.
pub fn build_cache(home: &RetainedHome) -> (PathBuf, String) {
    let addresses =
        [NODE_ADDRESSES[0], NODE_ADDRESSES[1], NODE_ADDRESSES[2], ADDRESS_BCRT1_P2SH_OP_TRUE];
    let mut node = Node::new(&Binary::dashd(), home, 0, &[]);
    node.start_with(Some(TIME_GENESIS_BLOCK), &[]);
    let mut mocktime = TIME_GENESIS_BLOCK;
    for (i, count) in [25u32, 25, 25, 25, 25, 25, 25, 24].into_iter().enumerate() {
        mocktime += u64::from(count) * 156;
        assert!(node.call("setmocktime", &format!("[{mocktime}]")).is_null());
        assert_eq!(node.generate_to(count, addresses[i % 4]).len(), count as usize);
    }
    assert_eq!(node.block_count(), 199);
    let tip = node.best_hash();
    node.stop();
    let regtest = net(&node.datadir);
    for entry in std::fs::read_dir(&regtest).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if CACHE_KEPT.contains(&name.as_str()) {
            continue;
        }
        if path.is_dir() {
            std::fs::remove_dir(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        } else {
            std::fs::remove_file(&path).unwrap();
        }
    }
    (node.datadir.clone(), tip)
}

/// A copy of the tree `from` at `to`: directories and regular files only.
pub fn copy_tree(from: &Path, to: &Path) {
    private_dir(to);
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            assert!(kind.is_file(), "{}", entry.path().display());
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

/// `node{index}` in `home`, a copy of the datadir `from`.
pub fn copy_datadir_to(from: &Path, home: &RetainedHome, index: usize) -> PathBuf {
    let to = home.path().join(format!("node{index}"));
    copy_tree(from, &to);
    to
}

/// Every path under `dir`, relative to it, with each file's bytes.
pub fn contents(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let path = e.unwrap().path();
            let rel = path.strip_prefix(dir).unwrap().to_path_buf();
            if std::fs::symlink_metadata(&path).unwrap().is_dir() {
                todo.push(path);
                out.insert(rel, None);
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    out
}

/// The members' contents under `net`, by member.
pub fn members(net: &Path) -> BTreeMap<&'static str, BTreeMap<PathBuf, Option<Vec<u8>>>> {
    MEMBERS
        .into_iter()
        .filter(|m| net.join(m).is_dir())
        .map(|m| (m, contents(&net.join(m))))
        .collect()
}

/// The backups in `net`, by name.
pub fn backups(net: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(net)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with(BACKUP_PREFIX))
        .collect();
    out.sort();
    out
}

/// Whether `dir` holds the import's journal at `Complete` (phase byte 2).
pub fn complete_journal(dir: &Path) -> bool {
    std::fs::read(dir.join(JOURNAL)).is_ok_and(|j| j.starts_with(b"CH5RJ1") && j.get(7) == Some(&2))
}

/// After an import: `net` holds no journal, and exactly one new backup next to `earlier`, with
/// `core`'s members unchanged and a completed journal. Returns the new backup.
pub fn imported(
    net: &Path,
    core: &BTreeMap<&'static str, BTreeMap<PathBuf, Option<Vec<u8>>>>,
    earlier: &[PathBuf],
) -> PathBuf {
    assert!(!net.join(JOURNAL).exists());
    let made: Vec<PathBuf> = backups(net).into_iter().filter(|b| !earlier.contains(b)).collect();
    assert_eq!(made.len(), 1, "{made:?}");
    let backup = made[0].clone();
    assert!(complete_journal(&backup), "{}", backup.display());
    assert_eq!(&members(&backup), core, "the members moved unchanged");
    backup
}

/// `gettxoutsetinfo` without `disk_size`, which is each engine's own.
pub fn utxo_summary(node: &Node) -> Json {
    let Json::Obj(fields) = node.call("gettxoutsetinfo", "[]") else {
        panic!("gettxoutsetinfo is an object")
    };
    Json::Obj(fields.into_iter().filter(|(k, _)| k != "disk_size").collect())
}

/// Core's `core-import-<yyyymmddThhmmssZ>` name for the epoch second `secs`.
pub fn backup_name(secs: u64) -> String {
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 {
        mp + 3
    } else {
        mp - 9
    };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{BACKUP_PREFIX}{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}
