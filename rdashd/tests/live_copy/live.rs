//! CH5-5's process tests' shared parts (§6.5): a live source with the hooks, the copy, `dir_state`
//! and the §2.2 counter check.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::common::copier::{Pause, PausedCopy};
use crate::common::*;
use crate::fixture::{Datadir, Expect, Fixture, Submission, call, rdashd, text, twin};

/// Every node's clock: fixed, so no PERIODIC or index-only write comes due (`policy.rs:183-187`).
pub const MOCKTIME: &str = "-mocktime=1793504672";
pub const ARGS: &[&str] = &[MOCKTIME];

/// `dir_state` (§6.1): every file's name and inode, plus its length except for `*.log` and `LOG`.
pub type DirState = BTreeMap<String, (u64, Option<u64>)>;

pub fn dir_state(dir: &Path) -> DirState {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = e.metadata().unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let appended = name.ends_with(".log") || name == "LOG";
            let len = (!appended).then_some(meta.len());
            (name, (meta.ino(), len))
        })
        .collect()
}

/// What changed from `before` to `after`: the files created, the files deleted, and the files
/// whose inode or tracked length changed.
pub fn dir_diff(before: &DirState, after: &DirState) -> (Vec<String>, Vec<String>, Vec<String>) {
    let created = after.keys().filter(|n| !before.contains_key(*n)).cloned().collect();
    let deleted = before.keys().filter(|n| !after.contains_key(*n)).cloned().collect();
    let changed = before
        .iter()
        .filter(|(n, v)| after.get(*n).is_some_and(|w| w != *v))
        .map(|(n, _)| n.clone())
        .collect();
    (created, deleted, changed)
}

/// The column family an SST belongs to: the value of its properties block's
/// `rocksdb.column.family.name`. Block entries are `shared, unshared, value_len` varints, the
/// key's unshared tail, then the value; the properties block is uncompressed and its keys are
/// short, so each varint is one byte. The one entry whose tail completes that key after its shared
/// prefix (22 bytes, after `rocksdb.column.family.id`) is it.
// RocksDB: table/meta_blocks.cc (PropertyBlockBuilder), table/block_based/block_builder.cc
pub fn sst_cf(path: &Path) -> Option<String> {
    const KEY: &[u8] = b"rocksdb.column.family.name";
    let bytes = std::fs::read(path).ok()?;
    let mut found = bytes.windows(3).enumerate().filter_map(|(i, h)| {
        let (shared, unshared, len) = (usize::from(h[0]), usize::from(h[1]), usize::from(h[2]));
        let tail = KEY.get(shared..).filter(|t| t.len() == unshared && unshared > 0)?;
        let at = i + 3;
        (bytes.get(at..at + unshared)? == tail)
            .then(|| bytes.get(at + unshared..at + unshared + len))?
    });
    let value = found.next()?;
    found.next().is_none().then(|| String::from_utf8_lossy(value).into_owned())
}

/// A running source with the live-copy hooks, and the submission log of what it mined or took.
pub struct Source {
    pub dir: Datadir,
    pub node: RpcNode,
    pub hooks: tempfile::TempDir,
    pub log: Vec<Submission>,
    pub extra: &'static [&'static str],
}

impl Source {
    pub fn start(extra: &'static [&'static str]) -> Source {
        let dir = Datadir::new();
        let hooks = tempfile::tempdir().unwrap();
        let node = rdashd(&dir, extra, Some(hooks.path()));
        Source {
            dir,
            node,
            hooks,
            log: Vec::new(),
            extra,
        }
    }

    pub fn call(&self, method: &str, params: Value) -> Value {
        call(&self.node, method, params)
    }

    /// Mines `n` blocks to `raw(51)`, logging each; their hashes.
    pub fn mine(&mut self, n: u64) -> Vec<String> {
        let mined = self.call("generatetodescriptor", json!([n, "raw(51)"]));
        let mined: Vec<String> = mined.as_array().expect("hashes").iter().map(text).collect();
        for hash in &mined {
            let s = Submission::block(&self.node, hash);
            self.log.push(s);
        }
        mined
    }

    pub fn tip(&self) -> String {
        text(&self.call("getbestblockhash", json!([])))
    }

    pub fn ask(&self, req: &str, body: &str) -> String {
        let answer = ask(self.hooks.path(), req, body);
        assert!(!answer.starts_with("error="), "{req}: {answer}");
        answer
    }

    /// A scripted flush; its `flush.done`.
    pub fn flush(&self) -> String {
        let done = self.ask("flush", "");
        assert!(done.starts_with("steps="), "{done}");
        done
    }

    pub fn quiesce(&self) {
        assert_eq!(self.ask("quiesce", ""), "ok\n");
    }

    /// `livecopycounters`.
    pub fn live(&self) -> Value {
        self.call("livecopycounters", json!([]))
    }

    pub fn jobs_done(&self) -> u64 {
        self.live()["events"]["flush_jobs_done"].as_u64().expect("flush_jobs_done")
    }

    /// Waits (up to `timeout`) until `done` holds on `livecopycounters`; whether it did.
    pub fn wait_live(&self, timeout: Duration, done: impl Fn(&Value) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if done(&self.live()) {
                return true;
            }
            if Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The copy point: `gettxoutsetinfo`'s reply.
    pub fn copy_point(&self) -> Value {
        self.call("gettxoutsetinfo", json!([]))
    }

    pub fn chainstate(&self) -> PathBuf {
        net(&self.dir.datadir).join("chainstate")
    }

    /// A copy of the running datadir, listed now; [`Copying::run`] copies it.
    pub fn list(&self) -> Copying {
        let to = Datadir::new();
        let listed = PausedCopy::list(&self.dir.datadir, &to.datadir).expect("the listing");
        Copying {
            to,
            listed,
        }
    }

    /// The fixture of what the source holds now: its log, its tip's state (a copy point of its
    /// own, so read it only after the copy), and a twin replayed to that tip.
    pub fn fixture_at(&self, expect: Expect) -> Fixture {
        let tip = expect.tip.clone();
        self.fixture_through(expect, &tip)
    }

    /// [`Source::fixture_at`], with the twin replaying the log through `through`'s submission.
    pub fn fixture_through(&self, expect: Expect, through: &str) -> Fixture {
        // Back from the tip by `previousblockhash`: the tip need not be active any more (I5).
        let mut hashes = vec![expect.tip.clone()];
        for _ in 0..expect.height {
            let header = self.call("getblockheader", json!([hashes.last().unwrap()]));
            hashes.push(text(&header["previousblockhash"]));
        }
        hashes.reverse();
        let twin = twin(&self.log, through, self.extra, false);
        Fixture {
            source: Datadir::new(),
            log: self.log.clone(),
            expect,
            extra: self.extra,
            next: None,
            hashes,
            twin,
        }
    }
}

/// A listed copy.
pub struct Copying {
    pub to: Datadir,
    pub listed: PausedCopy,
}

impl Copying {
    /// Copies in `order`, calling `pause` as the copier does.
    pub fn run(
        self,
        order: impl FnOnce(&mut Vec<PathBuf>),
        pause: impl FnMut(Pause<'_>),
    ) -> Datadir {
        self.try_run(order, pause).expect("the copy")
    }

    /// [`Copying::run`], with the copier's error.
    pub fn try_run(
        self,
        order: impl FnOnce(&mut Vec<PathBuf>),
        pause: impl FnMut(Pause<'_>),
    ) -> Result<Datadir, String> {
        self.listed.run(order, pause)?;
        Ok(self.to)
    }

    /// Copies in listing order, never pausing.
    pub fn now(self) -> Datadir {
        self.run(|_| (), |_| ())
    }
}

/// The §2.2 counter check: the tripwire counters and each super version of the copy point's
/// reply against `livecopycounters` after the copy; the fields that moved, with both values.
pub fn moved(reply: &Value, live: &Value) -> Vec<String> {
    let at = &reply["livecopy_counters"];
    let mut moved = Vec::new();
    for field in ["activations", "disconnects", "flatfile_mutations", "undo_writes"] {
        if at[field] != live[field] {
            moved.push(format!("{field} {} -> {}", at[field], live[field]));
        }
    }
    let (sv0, sv1) = (&at["super_versions"], &live["super_versions"]);
    let (sv0, sv1) = (sv0.as_object().expect("super versions"), sv1.as_object().expect("live"));
    let cfs = |m: &serde_json::Map<String, Value>| m.keys().cloned().collect::<BTreeSet<_>>();
    assert_eq!(cfs(sv0), cfs(sv1), "the super versions' column families");
    let schema = dashd_chainstate::store::CHAINSTATE_SCHEMA.cfs.len();
    assert_eq!(sv0.len(), schema, "one super version per column family: {sv0:?}");
    for (cf, v) in sv0 {
        if sv1[cf] != *v {
            moved.push(format!("super version {cf} {v} -> {}", sv1[cf]));
        }
    }
    moved
}

/// A tripwire counter's growth from the reply to `live`.
pub fn delta(reply: &Value, live: &Value, field: &str) -> i64 {
    let n = |v: &Value| v.as_i64().unwrap_or_else(|| panic!("{field}: {v}"));
    n(&live[field]) - n(&reply["livecopy_counters"][field])
}
