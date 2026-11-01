//! The native prefix regression for the SQLite engine (plan §6, SB0 §9.4), beside
//! `dashd-store`'s `rocks_wal_nonprefix.rs`.
//!
//! A scenario runs real commits through a recording VFS. Every crash point after every logged
//! operation gives power-loss images (see [`image`]); each is opened by a fresh engine, which must
//! recover the state after some prefix of the commits, keeping every commit acknowledged durable
//! before the crash. Points with at most [`EXHAUSTIVE`] images are enumerated, larger ones sampled.
//! The same log checks §9.4's durability table: which calls sync, and in which order.
//! Apple's `fullfsync` is checked at the pragma level, in the crate's unit tests. [`spv`] runs the
//! SPV store's workload under the same images, checked by its own invariants.
#![cfg(unix)]

mod image;
mod spv;
mod vfs;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use dash_kv::contract::CONTRACT_SCHEMA;
use dash_kv::{Cf, Commit, KvBatch, KvEngine, KvRead};
use dash_kv_sqlite::{FILE, SqliteConfig, SqliteEngine};

use crate::image::{Choice, Files, Point, State, digest, prefix_of, required};
use crate::vfs::Event;

/// Images enumerated per crash point at most; above this, a point is sampled.
const EXHAUSTIVE: u64 = 1 << 12;
const WAL: &str = "kv.sqlite-wal";

fn config() -> SqliteConfig {
    SqliteConfig {
        vfs: Some(vfs::NAME.to_owned()),
        ..SqliteConfig::default()
    }
}

fn cf(name: &str) -> Cf {
    CONTRACT_SCHEMA.cf(name).expect("in the schema")
}

fn read_state(engine: &impl KvRead) -> State {
    let mut state = State::new();
    for (i, spec) in CONTRACT_SCHEMA.cfs.iter().enumerate() {
        for entry in engine.iter_prefix(cf(spec.name), &[]) {
            let (k, v) = entry.expect("reads");
            state.insert((i, k), v);
        }
    }
    state
}

fn read_files(dir: &Path) -> Files {
    let mut files = Files::new();
    for name in [FILE, WAL] {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            files.insert(name.to_owned(), bytes);
        }
    }
    files
}

/// One write: put (`Some`) or delete (`None`).
type Op<'a> = (&'a str, &'a [u8], Option<&'a [u8]>);

struct Scenario {
    _root: tempfile::TempDir,
    engine: SqliteEngine,
    base: Files,
    states: Vec<State>,
}

impl Scenario {
    /// An engine with one durable commit, then the recording starts.
    fn new() -> Self {
        vfs::register();
        let root = tempfile::tempdir().expect("a scratch dir under TMPDIR");
        let dir = root.path().join("db");
        let (engine, _) = SqliteEngine::open(&dir, &CONTRACT_SCHEMA, &config()).expect("opens");
        let mut seed = engine.batch();
        seed.put(cf("point"), b"seed", b"0");
        engine.write(seed, Commit::Durable).expect("writes");
        let base = read_files(&dir);
        let states = vec![read_state(&engine)];
        vfs::start();
        Self {
            _root: root,
            engine,
            base,
            states,
        }
    }

    fn commit(&mut self, ops: &[Op<'_>], commit: Commit) {
        let mut batch = self.engine.batch();
        let mut state = self.states.last().expect("a state").clone();
        for &(name, key, value) in ops {
            let index = CONTRACT_SCHEMA.cfs.iter().position(|s| s.name == name).expect("a cf");
            match value {
                Some(v) => {
                    batch.put(cf(name), key, v);
                    state.insert((index, key.to_vec()), v.to_vec());
                }
                None => {
                    batch.delete(cf(name), key);
                    state.remove(&(index, key.to_vec()));
                }
            }
        }
        self.engine.write(batch, commit).expect("writes");
        self.states.push(state);
        vfs::acked(self.states.len() - 1, commit == Commit::Durable);
    }

    fn sync_wal(&mut self) {
        self.engine.sync_wal().expect("syncs");
        vfs::acked(self.states.len() - 1, true);
    }

    /// `flush_memtables`: the barrier, then a checkpoint.
    fn checkpoint(&mut self) {
        self.engine.flush_memtables().expect("flushes");
        vfs::acked(self.states.len() - 1, true);
    }

    fn finish(self) -> Recording {
        let log = vfs::stop();
        assert_eq!(read_state(&self.engine), *self.states.last().expect("a state"));
        drop(self.engine);
        Recording {
            base: self.base,
            log,
            states: self.states,
            scratch: tempfile::tempdir().expect("a scratch dir under TMPDIR"),
            recovered: HashMap::new(),
        }
    }
}

struct Recording {
    base: Files,
    log: Vec<Event>,
    states: Vec<State>,
    scratch: tempfile::TempDir,
    /// Recovered states by image digest.
    recovered: HashMap<u64, State>,
}

impl Recording {
    fn point(&self, at: usize) -> Point {
        Point::new(&self.base, &self.log, at)
    }

    /// The state a fresh engine recovers from `files` (opened through the VFS with the recording
    /// off, so its syncs cost nothing).
    fn recover(&mut self, files: &Files) -> State {
        let key = digest(files);
        if let Some(state) = self.recovered.get(&key) {
            return state.clone();
        }
        let dir: PathBuf = self.scratch.path().join(key.to_string());
        std::fs::create_dir(&dir).expect("a fresh image dir");
        for (name, bytes) in files {
            std::fs::write(dir.join(name), bytes).expect("writes the image");
        }
        let state = {
            let (engine, _) = SqliteEngine::open(&dir, &CONTRACT_SCHEMA, &config())
                .unwrap_or_else(|e| panic!("an image does not open: {e}"));
            read_state(&engine)
        };
        std::fs::remove_dir_all(&dir).expect("removes the image");
        self.recovered.insert(key, state.clone());
        state
    }

    /// Checks the image of `choice` at crash point `at`; the commit it recovered.
    fn check(&mut self, point: &Point, at: usize, choice: &Choice) -> usize {
        let files = point.image(choice);
        let got = self.recover(&files);
        let need = required(&self.log, at);
        prefix_of(&self.states, need, &got)
            .unwrap_or_else(|e| panic!("crash after event {at} of {}: {e}", self.log.len()))
    }

    /// Every crash point: enumerated up to [`EXHAUSTIVE`] images, else sampled. Returns the number
    /// of images checked and how many points were sampled.
    fn verify(&mut self) -> (usize, usize) {
        let (mut images, mut sampled) = (0, 0);
        for at in 0..=self.log.len() {
            let point = self.point(at);
            let choices = if point.choices() <= EXHAUSTIVE {
                point.all()
            } else {
                sampled += 1;
                point.sampled(64, 64, at as u64)
            };
            for choice in &choices {
                self.check(&point, at, choice);
            }
            images += choices.len();
        }
        eprintln!(
            "{} events, {images} images ({} distinct), {sampled} points sampled",
            self.log.len(),
            self.recovered.len()
        );
        (images, sampled)
    }

    /// The events between the `n`-th and the `n+1`-th marker (the work of the `n+1`-th call).
    fn window(&self, n: usize) -> &[Event] {
        let marks: Vec<usize> = std::iter::once(0)
            .chain(
                self.log
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| matches!(e, Event::Acked { .. }))
                    .map(|(i, _)| i + 1),
            )
            .collect();
        &self.log[marks[n]..marks[n + 1] - 1]
    }

    /// The index of the marker that ends commit `commit`'s window, and that of the one before.
    fn commit_events(&self, commit: usize) -> (usize, usize) {
        let mut start = 0;
        for (i, e) in self.log.iter().enumerate() {
            if let Event::Acked {
                commit: c,
                ..
            } = e
            {
                if *c == commit {
                    return (start, i);
                }
                start = i + 1;
            }
        }
        panic!("no commit {commit}")
    }
}

fn value(n: u8, len: usize) -> Vec<u8> {
    vec![n; len]
}

/// Small commits of every kind, enumerated wherever a point has few images.
#[test]
fn buffered_and_durable_commits_recover_a_prefix() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    let (v1, v2, v3) = (value(1, 100), value(2, 300), value(3, 50));
    s.commit(&[("point", b"a", Some(&v1))], Commit::Buffered);
    s.commit(&[("point", b"b", Some(&v2)), ("ordered", b"x", Some(&v1))], Commit::Buffered);
    s.commit(&[("point", b"a", None), ("ordered", b"y", Some(&v3))], Commit::Durable);
    s.commit(&[("point", b"c", Some(&v3))], Commit::Buffered);
    s.sync_wal();
    s.commit(&[("ordered", b"x", None)], Commit::Buffered);
    s.commit(&[("point", b"b", Some(&v1))], Commit::Durable);
    let mut r = s.finish();
    let (images, _) = r.verify();
    assert!(images > 1000, "{images}");
}

/// A durable commit that writes no page (empty, a delete of a missing key, a put of the stored
/// value) writes no frame of its own: its acknowledgement still covers the buffered commits
/// before it (it commits the barrier counter), at every crash point.
#[test]
fn a_durable_commit_that_changes_nothing_keeps_the_buffered_ones() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    let (v1, v2) = (value(1, 64), value(2, 64));
    s.commit(&[("point", b"a", Some(&v1))], Commit::Buffered);
    s.commit(&[], Commit::Durable);
    s.commit(&[("point", b"b", Some(&v2))], Commit::Buffered);
    s.commit(&[("point", b"missing", None)], Commit::Durable);
    s.commit(&[("point", b"c", Some(&v1))], Commit::Buffered);
    s.commit(&[("point", b"c", Some(&v1))], Commit::Durable);
    let mut r = s.finish();
    for commit in [2, 4, 6] {
        let (from, to) = r.commit_events(commit);
        assert!(syncs(&r.log[from..to], WAL).count() == 1, "commit {commit} synced the WAL");
    }
    r.verify();
}

/// A commit of many pages (a page split), buffered then durable: sampled points.
#[test]
fn multi_page_commits_recover_a_prefix() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    let big = value(7, 400);
    let keys: Vec<[u8; 2]> = (0..40u16).map(u16::to_be_bytes).collect();
    let ops: Vec<Op<'_>> = keys.iter().map(|k| ("ordered", &k[..], Some(&big[..]))).collect();
    s.commit(&ops, Commit::Buffered);
    let small = value(8, 10);
    s.commit(
        &[("ordered", &keys[3][..], Some(&small)), ("point", b"p", Some(&small))],
        Commit::Buffered,
    );
    let deletes: Vec<Op<'_>> = keys[10..30].iter().map(|k| ("ordered", &k[..], None)).collect();
    s.commit(&deletes, Commit::Durable);
    let mut r = s.finish();
    let (_, sampled) = r.verify();
    assert!(sampled > 0, "the scenario reaches sampled points");
}

/// Plan §6 (c): after a checkpoint, the next commit restarts the WAL with a new salt over a
/// longer previous generation; the stale frames after it are never replayed.
#[test]
fn stale_frames_after_a_wal_reset_are_not_replayed() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    for n in 1..=4u8 {
        let v = value(n, 600);
        s.commit(&[("point", b"k", Some(&v)), ("ordered", &[n], Some(&v))], Commit::Buffered);
    }
    s.checkpoint();
    let v = value(9, 10);
    s.commit(&[("point", b"k", Some(&v))], Commit::Buffered);
    let mut r = s.finish();

    let reset = r.window(5);
    assert!(
        reset.iter().any(|e| matches!(e, Event::Write { file, offset: 0, .. } if file == WAL)),
        "the commit after the checkpoint rewrote the WAL header"
    );
    let end = reset
        .iter()
        .filter_map(|e| match e {
            Event::Write {
                file,
                offset,
                data,
            } if file == WAL => Some(offset + data.len() as u64),
            _ => None,
        })
        .max()
        .expect("WAL writes");
    let wal = r.point(r.log.len()).image(&r.point(r.log.len()).choice_where(|_, _| true));
    assert!(wal[WAL].len() as u64 > end, "frames of the old generation follow the new ones");
    r.verify();
}

/// Plan §6 (a), RocksDB's trap: commit 3's frames never reached the disk, commit 4's did. SQLite
/// stops at the first frame that fails its checksum and recovers commits 1–2; RocksDB's
/// PointInTime recovery skips the zeroed block and keeps commit 4 (`rocks_wal_nonprefix.rs`).
#[test]
fn a_lost_commit_between_kept_ones_recovers_the_prefix_before_it() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    s.commit(&[("point", b"a", Some(&value(1, 64)))], Commit::Durable);
    s.commit(&[("point", b"b", Some(&value(2, 64)))], Commit::Buffered);
    s.commit(&[("point", b"c", Some(&value(3, 64)))], Commit::Buffered);
    s.commit(&[("point", b"d", Some(&value(4, 64)))], Commit::Buffered);
    let mut r = s.finish();
    let (from, to) = r.commit_events(3);
    let at = r.log.len();
    let point = r.point(at);
    let choice = point.choice_where(|event, _| !(from..to).contains(&event));
    let files = point.image(&choice);
    let (_, last) = r.commit_events(4);
    let kept = r.log[..last]
        .iter()
        .rev()
        .find_map(|e| match e {
            Event::Write {
                file,
                offset,
                data,
            } if file == WAL => Some((*offset as usize, data.clone())),
            _ => None,
        })
        .expect("commit 4 wrote frames");
    // Its last sector is commit 4's alone (its first is shared with commit 3's frame).
    let (offset, data) = kept;
    let tail = data.len() - 256;
    assert!(
        files[WAL][offset + tail..offset + data.len()] == data[tail..],
        "commit 4's frame is in the image, after commit 3's lost one"
    );
    assert_eq!(r.check(&point, at, &choice), 2);
}

/// Plan §6 (b): the last frame torn after its first sectors recovers the commit before it.
#[test]
fn a_torn_last_frame_recovers_the_commit_before_it() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    s.commit(&[("point", b"a", Some(&value(1, 64)))], Commit::Durable);
    s.commit(&[("point", b"b", Some(&value(2, 64)))], Commit::Buffered);
    s.commit(&[("point", b"c", Some(&value(3, 2000)))], Commit::Buffered);
    let mut r = s.finish();
    let (_, end) = r.commit_events(3);
    let (last, first_sector) = r.log[..end]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, e)| match e {
            Event::Write {
                file,
                offset,
                data,
            } if file == WAL && data.len() > 1024 => Some((i, offset / image::SECTOR)),
            _ => None,
        })
        .expect("commit 3 wrote a page");
    let at = r.log.len();
    let point = r.point(at);
    let choice =
        point.choice_where(|event, sector| event != last || sector < Some(first_sector + 3));
    assert_eq!(r.check(&point, at, &choice), 2);
}

/// Plan §6 (d): no image carries a `-shm` file; one with a garbage `-shm` recovers as well, as
/// the first connection rebuilds the index from the WAL.
#[test]
fn a_lost_or_garbage_shm_rebuilds_from_the_wal() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    s.commit(&[("point", b"a", Some(&value(1, 64)))], Commit::Buffered);
    s.commit(&[("point", b"b", Some(&value(2, 64)))], Commit::Durable);
    let mut r = s.finish();
    let at = r.log.len();
    let point = r.point(at);
    let mut files = point.image(&point.choice_where(|_, _| false));
    assert!(!files.contains_key("kv.sqlite-shm"));
    assert_eq!(r.recover(&files), r.states[2]);
    files.insert("kv.sqlite-shm".to_owned(), vec![0x5a; 32 << 10]);
    assert_eq!(r.recover(&files), r.states[2]);
}

fn syncs<'a>(events: &'a [Event], name: &str) -> impl Iterator<Item = usize> + 'a {
    let name = name.to_owned();
    events
        .iter()
        .enumerate()
        .filter_map(move |(i, e)| matches!(e, Event::Sync { file } if *file == name).then_some(i))
}

fn writes<'a>(events: &'a [Event], name: &str) -> impl Iterator<Item = usize> + 'a {
    let name = name.to_owned();
    events.iter().enumerate().filter_map(move |(i, e)| {
        matches!(e, Event::Write { file, .. } if *file == name).then_some(i)
    })
}

/// SB0 §9.4's durability table, from the log: a buffered commit syncs nothing; a durable one syncs
/// the WAL after its frames; `sync_wal` syncs once with buffered work pending and not at all
/// otherwise; a checkpoint syncs the WAL before writing the database file, and the file after.
#[test]
fn the_durability_table() {
    let _serial = vfs::serial();
    let mut s = Scenario::new();
    let v = value(1, 64);
    s.commit(&[("point", b"a", Some(&v))], Commit::Buffered); // window 0
    s.commit(&[("point", b"b", Some(&v))], Commit::Durable); // 1
    s.sync_wal(); // 2: nothing pending
    s.commit(&[("point", b"c", Some(&v))], Commit::Buffered); // 3
    s.sync_wal(); // 4
    s.commit(&[("point", b"d", Some(&v))], Commit::Buffered); // 5
    s.checkpoint(); // 6
    let r = s.finish();

    let w = r.window(0);
    assert!(writes(w, WAL).count() > 0);
    assert_eq!(
        w.iter().filter(|e| matches!(e, Event::Sync { .. })).count(),
        0,
        "a buffered commit syncs nothing: {w:?}"
    );

    let w = r.window(1);
    let sync: Vec<usize> = syncs(w, WAL).collect();
    assert_eq!(sync.len(), 1, "{w:?}");
    assert!(writes(w, WAL).all(|i| i < sync[0]), "the frames come before the sync");
    assert_eq!(syncs(w, FILE).count(), 0);

    assert!(
        r.window(2).is_empty(),
        "sync_wal with nothing buffered does nothing: {:?}",
        r.window(2)
    );

    let w = r.window(4);
    assert_eq!(syncs(w, WAL).count(), 1, "{w:?}");
    assert!(writes(w, WAL).all(|i| i < syncs(w, WAL).next().unwrap()));

    let w = r.window(6);
    let first_db_write = writes(w, FILE).next().expect("the checkpoint writes the database file");
    let wal_sync = syncs(w, WAL).next().expect("the WAL is synced");
    assert!(wal_sync < first_db_write, "WAL synced before the database file is written: {w:?}");
    let db_sync = syncs(w, FILE).last().expect("the database file is synced");
    assert!(writes(w, FILE).all(|i| i < db_sync), "{w:?}");
}

/// The prefix check rejects a state that mixes commits, and one older than a durable commit.
#[test]
fn the_prefix_check_rejects_a_non_prefix() {
    let entry = |k: &[u8], v: &[u8]| ((1, k.to_vec()), v.to_vec());
    let s0 = State::new();
    let s1 = State::from([entry(b"a", b"1")]);
    let s2 = State::from([entry(b"a", b"1"), entry(b"b", b"2")]);
    let s3 = State::from([entry(b"b", b"2")]);
    let states = [s0.clone(), s1.clone(), s2, s3];
    assert_eq!(prefix_of(&states, 0, &s1), Ok(1));
    assert!(prefix_of(&states, 2, &s1).is_err(), "older than a durable commit");
    let mixed = State::from([entry(b"a", b"1"), entry(b"b", b"2"), entry(b"c", b"x")]);
    assert!(prefix_of(&states, 0, &mixed).is_err());
    let skipped = State::from([entry(b"b", b"2"), entry(b"a", b"0")]);
    assert!(prefix_of(&states, 0, &skipped).is_err());
}
