//! `MicroLog` (§6.1): the PLAN's toy flush, with planted bugs P1–P8.
//!
//! - Appends buffer `len ‖ bytes ‖ crc32c` records in memory.
//! - A commit writes them to `log?????.dat` files (rolling over at `max_file` bytes, each new
//!   file created explicitly), `sync_data`s every file written, `sync_dir`s the directory if a
//!   file was created, then writes `meta.committed` Durably and marks the flush durable.
//! - A snapshot writes the committed records to `snap.tmp`, syncs it, renames it to `snap.dat`,
//!   syncs the directory and writes `meta.snapshot` Durably.
//! - Recovery reads exactly the committed records: the first `meta.snapshot` from the snapshot,
//!   the rest from the log.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use crate::engine::{Cf, Commit, KvBatch, KvEngine, KvRead};
use crate::faulty::check::{
    FileRef, InvariantId, RefExtractor, Requirement, Violation, WorldDurableView,
};
use crate::faulty::engine::FaultyEngine;
use crate::faulty::fs::FaultyFlatFiles;
use crate::faulty::harness::{Abort, Reference, Sut, allowed_tips};
use crate::faulty::world::{Hash, Mark, Trace, World};
use crate::fs::FlatFs;
use crate::schema::{CfProfile, CfSpec, DbSchema};

/// The engine schema: `meta` only.
pub static MICRO_SCHEMA: DbSchema = DbSchema {
    name: "micro",
    version: 1,
    cfs: &[CfSpec::new("meta", CfProfile::Ordered)],
};
const META: Cf = DbSchema::META;
const COMMITTED: &[u8] = b"committed";
const SNAPSHOT: &[u8] = b"snapshot";
const DIR: &str = "/dd/log";
const ABORT: &str = "MicroLog: a store call failed";

/// MicroLog's planted bugs (§6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicroBug {
    /// The commit record before the data's fsync.
    P1,
    /// The commit record before the data is written.
    P2,
    /// No `sync_dir` after a rollover's create.
    P3,
    /// A Buffered commit record, acknowledged.
    P4,
    /// Retries a failed `sync_data` and carries on.
    P5,
    /// Ignores a short-write ENOSPC.
    P6,
    /// Recovers by CRC scan instead of the commit record.
    P7,
    /// Renames the snapshot without `sync_data`.
    P8,
    /// Syncs only the newest file a commit wrote (checkpoint-1 review X1).
    X1,
    /// Writes `meta.snapshot` before the rename's `sync_dir` (X2).
    X2,
    /// Recovery trims the last log one byte short of its committed end, unsynced (X3).
    X3,
}

impl FromStr for MicroBug {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        Ok(match s {
            "P1" => Self::P1,
            "P2" => Self::P2,
            "P3" => Self::P3,
            "P4" => Self::P4,
            "P5" => Self::P5,
            "P6" => Self::P6,
            "P7" => Self::P7,
            "P8" => Self::P8,
            "X1" => Self::X1,
            "X2" => Self::X2,
            "X3" => Self::X3,
            _ => return Err(()),
        })
    }
}

/// MicroLog's configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicroConfig {
    pub bug: Option<MicroBug>,
    pub max_file: u64,
}

impl fmt::Display for MicroConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.bug {
            Some(b) => write!(f, "bug={b:?} max={}", self.max_file),
            None => write!(f, "bug=none max={}", self.max_file),
        }
    }
}

impl FromStr for MicroConfig {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let mut cfg = Self {
            bug: None,
            max_file: 1024,
        };
        for part in s.split_whitespace() {
            match part.split_once('=').ok_or(())? {
                ("bug", "none") => cfg.bug = None,
                ("bug", b) => cfg.bug = Some(b.parse()?),
                ("max", n) => cfg.max_file = n.parse().map_err(|_| ())?,
                _ => return Err(()),
            }
        }
        Ok(cfg)
    }
}

/// A MicroLog operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicroOp {
    /// A record of `len` bytes of `fill`.
    Append {
        len: u32,
        fill: u8,
    },
    Commit,
    Snapshot,
    /// R12's continuation: append the marker record and commit.
    Continue,
}

/// The continuation's record: no workload may append it.
pub const MARKER: (u32, u8) = (64, 0xc0);

impl fmt::Display for MicroOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Append {
                len,
                fill,
            } => write!(f, "a{len}:{fill}"),
            Self::Commit => f.write_str("c"),
            Self::Snapshot => f.write_str("s"),
            Self::Continue => f.write_str("C"),
        }
    }
}

impl FromStr for MicroOp {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "c" => Ok(Self::Commit),
            "s" => Ok(Self::Snapshot),
            "C" => Ok(Self::Continue),
            _ => {
                let (len, fill) = s.strip_prefix('a').ok_or(())?.split_once(':').ok_or(())?;
                Ok(Self::Append {
                    len: len.parse().map_err(|_| ())?,
                    fill: fill.parse().map_err(|_| ())?,
                })
            }
        }
    }
}

impl MicroOp {
    fn record(len: u32, fill: u8) -> Vec<u8> {
        vec![fill; len as usize]
    }
}

/// CRC-32C (Castagnoli), bitwise: the record checksum.
pub fn crc32c(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82f6_3b78
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn encode(data: &[u8]) -> Vec<u8> {
    let mut r = Vec::with_capacity(data.len() + 8);
    r.extend_from_slice(&(data.len() as u32).to_le_bytes());
    r.extend_from_slice(data);
    r.extend_from_slice(&crc32c(data).to_le_bytes());
    r
}

/// A tip "hash" for `count` committed records.
pub fn tip(count: u32) -> Hash {
    let mut h = [0u8; 32];
    h[..4].copy_from_slice(&count.to_le_bytes());
    h[31] = 0x6d;
    h
}

fn log_path(file: u32) -> PathBuf {
    PathBuf::from(format!("{DIR}/log{file:05}.dat"))
}

fn snap_path() -> PathBuf {
    PathBuf::from(format!("{DIR}/snap.dat"))
}

fn snap_tmp() -> PathBuf {
    PathBuf::from(format!("{DIR}/snap.tmp"))
}

/// `meta.committed`: the count, then each log file's committed end.
fn encode_committed(count: u32, ends: &[u64]) -> Vec<u8> {
    let mut v = count.to_le_bytes().to_vec();
    for e in ends {
        v.extend_from_slice(&e.to_le_bytes());
    }
    v
}

fn decode_committed(v: &[u8]) -> (u32, Vec<u64>) {
    let count = u32::from_le_bytes(v[..4].try_into().expect("4 bytes"));
    let ends =
        v[4..].chunks(8).map(|c| u64::from_le_bytes(c.try_into().expect("8 bytes"))).collect();
    (count, ends)
}

/// The reference: every record appended, and the commits.
pub struct MicroRef {
    pub records: Vec<Vec<u8>>,
}

impl Reference for MicroRef {
    type Op = MicroOp;
    type Config = MicroConfig;

    fn new(_: &MicroConfig) -> Self {
        Self {
            records: Vec::new(),
        }
    }

    fn apply(&mut self, op: &MicroOp) {
        if let MicroOp::Append {
            len,
            fill,
        } = op
        {
            assert_ne!((*len, *fill), MARKER, "the marker record is the continuation's");
            self.records.push(MicroOp::record(*len, *fill));
        }
    }
}

/// The system under test.
pub struct MicroLog {
    cfg: MicroConfig,
    world: World,
    engine: Arc<FaultyEngine>,
    fs: Arc<FaultyFlatFiles>,
    /// Committed records as recovered (`None`: unreadable), then appended ones.
    records: Vec<Option<Vec<u8>>>,
    committed: u32,
    /// Each log file's end.
    ends: Vec<u64>,
    snapshot: u32,
    flushes: u64,
}

impl MicroLog {
    fn fail(&self, op: &'static str) -> Abort {
        self.world.mark(Mark::StoreFailed {
            op,
        });
        self.world.mark(Mark::Abort {
            message: ABORT.into(),
        });
        Abort::new(ABORT)
    }

    fn bug(&self, b: MicroBug) -> bool {
        self.cfg.bug == Some(b)
    }

    fn write_meta(&self, key: &[u8], value: &[u8], commit: Commit) -> Result<(), Abort> {
        let mut b = self.engine.batch();
        b.put(META, key, value);
        self.engine.write(b, commit).map_err(|_| self.fail("meta"))
    }

    /// Writes the uncommitted records to the log; returns the files written and whether one was
    /// created.
    fn write_records(&mut self) -> Result<(BTreeSet<u32>, bool), Abort> {
        let mut written = BTreeSet::new();
        let mut created = false;
        for i in self.committed as usize..self.records.len() {
            let rec = encode(self.records[i].as_ref().expect("an appended record"));
            let mut file = self.ends.len() as u32 - 1;
            if self.ends[file as usize] > 0
                && self.ends[file as usize] + rec.len() as u64 > self.cfg.max_file
            {
                file += 1;
                self.ends.push(0);
                self.fs.create(&log_path(file)).map_err(|_| self.fail("create"))?;
                created = true;
            }
            let at = self.ends[file as usize];
            if self.fs.write_all_at(&log_path(file), at, &rec).is_err() && !self.bug(MicroBug::P6) {
                return Err(self.fail("write"));
            }
            self.ends[file as usize] = at + rec.len() as u64;
            written.insert(file);
        }
        Ok((written, created))
    }

    fn sync(&self, written: &BTreeSet<u32>, created: bool) -> Result<(), Abort> {
        let newest: BTreeSet<u32> = written.last().copied().into_iter().collect();
        let written = if self.bug(MicroBug::X1) {
            &newest
        } else {
            written
        };
        for &f in written {
            if self.fs.sync_data(&log_path(f)).is_err() {
                if self.bug(MicroBug::P5) {
                    // fsyncgate: the retry "succeeds".
                    let _ = self.fs.sync_data(&log_path(f));
                } else {
                    return Err(self.fail("sync_data"));
                }
            }
        }
        if created && !self.bug(MicroBug::P3) {
            self.fs.sync_dir(Path::new(DIR)).map_err(|_| self.fail("sync_dir"))?;
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), Abort> {
        let count = self.records.len() as u32;
        self.flushes += 1;
        let flush = self.flushes;
        self.world.mark(Mark::FlushBegin {
            flush,
            old: Some(tip(self.committed)),
            new: tip(count),
        });
        let commit = if self.bug(MicroBug::P4) {
            Commit::Buffered
        } else {
            Commit::Durable
        };
        if self.bug(MicroBug::P2) {
            let mut ends = self.ends.clone();
            // The index claims where the records will end.
            let mut f = ends.len() - 1;
            for r in &self.records[self.committed as usize..] {
                let n = encode(r.as_ref().expect("appended")).len() as u64;
                if ends[f] > 0 && ends[f] + n > self.cfg.max_file {
                    ends.push(0);
                    f += 1;
                }
                ends[f] += n;
            }
            self.write_meta(COMMITTED, &encode_committed(count, &ends), commit)?;
        }
        let (written, created) = self.write_records()?;
        if self.bug(MicroBug::P1) {
            self.write_meta(COMMITTED, &encode_committed(count, &self.ends), commit)?;
            self.sync(&written, created)?;
        } else {
            self.sync(&written, created)?;
            if !self.bug(MicroBug::P2) {
                self.write_meta(COMMITTED, &encode_committed(count, &self.ends), commit)?;
            }
        }
        self.committed = count;
        self.world.mark(Mark::Durable {
            flush,
        });
        Ok(())
    }

    fn snapshot(&mut self) -> Result<(), Abort> {
        let mut content = Vec::new();
        for r in &self.records[..self.committed as usize] {
            content.extend(encode(r.as_ref().expect("committed records are readable")));
        }
        let tmp = snap_tmp();
        let _ = self.fs.remove(&tmp).map_err(|_| self.fail("remove"))?;
        self.fs.create(&tmp).map_err(|_| self.fail("create"))?;
        self.fs.write_all_at(&tmp, 0, &content).map_err(|_| self.fail("write"))?;
        if !self.bug(MicroBug::P8) {
            self.fs.sync_data(&tmp).map_err(|_| self.fail("sync_data"))?;
        }
        self.fs.rename(&tmp, &snap_path()).map_err(|_| self.fail("rename"))?;
        if !self.bug(MicroBug::X2) {
            self.fs.sync_dir(Path::new(DIR)).map_err(|_| self.fail("sync_dir"))?;
        }
        let mut v = self.committed.to_le_bytes().to_vec();
        v.extend_from_slice(&(content.len() as u64).to_le_bytes());
        self.write_meta(SNAPSHOT, &v, Commit::Durable)?;
        if self.bug(MicroBug::X2) {
            self.fs.sync_dir(Path::new(DIR)).map_err(|_| self.fail("sync_dir"))?;
        }
        self.snapshot = self.committed;
        Ok(())
    }

    /// Reads one record at `at` of `path`; `None` if it is short or its checksum fails.
    /// Reads one record at `at` of `path`: `None` if it is short, absent or its checksum fails.
    /// Any other read error stops recovery (§4.4: a failed read in recovery is fatal).
    fn read_record(&self, path: &Path, at: u64) -> Result<(Option<Vec<u8>>, u64), Abort> {
        let read = |off: u64, buf: &mut [u8]| -> Result<bool, Abort> {
            match self.fs.read_exact_at(path, off, buf) {
                Ok(()) => Ok(true),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::NotFound
                    ) =>
                {
                    Ok(false)
                }
                Err(_) => Err(self.fail("read")),
            }
        };
        let mut len = [0u8; 4];
        if !read(at, &mut len)? {
            return Ok((None, 0));
        }
        let n = u32::from_le_bytes(len) as usize;
        if n > 1 << 20 {
            return Ok((None, 0));
        }
        let mut buf = vec![0u8; n + 4];
        if !read(at + 4, &mut buf)? {
            return Ok((None, 0));
        }
        let (data, crc) = buf.split_at(n);
        if crc32c(data).to_le_bytes() != crc {
            return Ok((None, 0));
        }
        Ok((Some(data.to_vec()), 8 + n as u64))
    }

    /// A fresh handle on the same stores, for re-reading them.
    fn reread(&self) -> Self {
        Self {
            cfg: self.cfg.clone(),
            world: self.world.clone(),
            engine: self.engine.clone(),
            fs: self.fs.clone(),
            records: Vec::new(),
            committed: 0,
            ends: Vec::new(),
            snapshot: 0,
            flushes: 0,
        }
    }

    /// Recovers the committed records (or, under P7, whatever a CRC scan finds).
    fn recover(&mut self) -> Result<(), Abort> {
        let committed = self.engine.get(META, COMMITTED).map_err(|_| self.fail("get"))?;
        let snapshot = self.engine.get(META, SNAPSHOT).map_err(|_| self.fail("get"))?;
        if self.bug(MicroBug::P7) {
            let (mut file, mut at) = (0u32, 0u64);
            loop {
                let len = self.fs.len(&log_path(file)).ok().flatten().unwrap_or(0);
                if at >= len {
                    if self.fs.len(&log_path(file + 1)).ok().flatten().is_none() {
                        break;
                    }
                    self.ends.push(at);
                    file += 1;
                    at = 0;
                    continue;
                }
                match self.read_record(&log_path(file), at)? {
                    (Some(r), n) => {
                        self.records.push(Some(r));
                        at += n;
                    }
                    (None, _) => break,
                }
            }
            self.ends.push(at);
            self.committed = self.records.len() as u32;
            return Ok(());
        }
        let Some(c) = committed else {
            self.ends = vec![0];
            return Ok(());
        };
        let (count, ends) = decode_committed(&c);
        self.committed = count;
        self.ends = ends;
        let from_snapshot =
            snapshot.map_or(0, |s| u32::from_le_bytes(s[..4].try_into().expect("4")));
        let mut at = 0u64;
        for _ in 0..from_snapshot {
            let (r, n) = self.read_record(&snap_path(), at)?;
            self.records.push(r);
            at += n;
            if n == 0 {
                break;
            }
        }
        self.records.resize(from_snapshot as usize, None);
        let (mut file, mut at) = (0u32, 0u64);
        for i in 0..count {
            if at >= self.ends[file as usize] && (file as usize) + 1 < self.ends.len() {
                file += 1;
                at = 0;
            }
            let (r, n) = self.read_record(&log_path(file), at)?;
            if i >= from_snapshot {
                self.records.push(r);
            }
            if n == 0 {
                // Unreadable: the rest cannot be located.
                self.records.resize(count as usize, None);
                break;
            }
            at += n;
        }
        self.snapshot = from_snapshot;
        Ok(())
    }
}

impl Sut for MicroLog {
    type Config = MicroConfig;
    type Op = MicroOp;
    type Ref = MicroRef;
    const NAME: &'static str = "microlog";

    fn open(world: &World, cfg: &MicroConfig) -> Result<Self, Abort> {
        let mut m = Self {
            cfg: cfg.clone(),
            world: world.clone(),
            engine: world.engine("micro", &MICRO_SCHEMA),
            fs: world.fs(),
            records: Vec::new(),
            committed: 0,
            ends: Vec::new(),
            snapshot: 0,
            flushes: 0,
        };
        m.world.mark(Mark::RecoveryBegin);
        m.fs.create_dir_all(Path::new(DIR)).map_err(|_| m.fail("create_dir"))?;
        m.recover()?;
        if m.bug(MicroBug::X3)
            && m.committed > 0
            && let Some(&end) = m.ends.last()
            && end > 0
        {
            let last = log_path(m.ends.len() as u32 - 1);
            m.fs.truncate(&last, end - 1).map_err(|_| m.fail("truncate"))?;
        }
        if m.ends.is_empty() {
            m.ends.push(0);
        }
        if m.committed == 0 && m.ends == [0] && m.fs.len(&log_path(0)).ok().flatten().is_none() {
            m.fs.create(&log_path(0)).map_err(|_| m.fail("create"))?;
        }
        // The log directory and its first file may exist only in the page cache (a process
        // kill during the first open): make their entries durable before anything relies on
        // them.
        m.fs.sync_dir(Path::new(DIR)).map_err(|_| m.fail("sync_dir"))?;
        m.fs.sync_dir(Path::new("/dd")).map_err(|_| m.fail("sync_dir"))?;
        m.world.mark(Mark::RecoveryEnd);
        Ok(m)
    }

    fn apply(&mut self, op: &MicroOp) -> Result<(), Abort> {
        match op {
            MicroOp::Append {
                len,
                fill,
            } => {
                self.records.push(Some(MicroOp::record(*len, *fill)));
                Ok(())
            }
            MicroOp::Commit => self.commit(),
            MicroOp::Snapshot => self.snapshot(),
            MicroOp::Continue => {
                self.records.push(Some(MicroOp::record(MARKER.0, MARKER.1)));
                self.commit()
            }
        }
    }

    fn check(&self, _world: &World, refm: &MicroRef, trace: &Trace) -> Result<(), Vec<Violation>> {
        let mut v = Vec::new();
        // R1's view form: read the committed records from the stores again, so damage done
        // after recovery read them is seen.
        let mut fresh = self.reread();
        if fresh.recover().is_err() {
            return Err(vec![Violation::new(
                InvariantId::R1,
                "the committed records do not read back",
            )]);
        }
        let count = fresh.committed;
        let (t_ack, allowed) = allowed_tips(trace);
        let marker = MicroOp::record(MARKER.0, MARKER.1);
        // R1: every committed record reads back as the reference's (or is a continuation's).
        for (i, r) in fresh.records[..count as usize].iter().enumerate() {
            match (r, refm.records.get(i)) {
                (Some(got), Some(want)) if got == want => {}
                (Some(got), _) if *got == marker => {}
                (None, _) => {
                    v.push(Violation::new(
                        InvariantId::R1,
                        format!("committed record {i} is unreadable"),
                    ));
                    break;
                }
                _ => {
                    v.push(Violation::new(
                        InvariantId::R1,
                        format!("committed record {i} differs"),
                    ));
                    break;
                }
            }
        }
        // R3: the recovered count is an allowed tip (the empty log is `tip(0)`).
        let here = tip(count);
        if !allowed.iter().any(|a| a.unwrap_or(tip(0)) == here) {
            v.push(Violation::new(
                InvariantId::R3,
                format!("recovered {count} records, not an allowed tip"),
            ));
        }
        // R4: at least the acknowledged count.
        let acked = t_ack.map_or(0, |h| u32::from_le_bytes(h[..4].try_into().expect("4")));
        if count < acked {
            v.push(Violation::new(
                InvariantId::R4,
                format!("recovered {count} records, {acked} were acknowledged"),
            ));
        }
        if v.is_empty() {
            Ok(())
        } else {
            Err(v)
        }
    }

    fn continue_after_recovery(&mut self) -> Option<MicroOp> {
        Some(MicroOp::Continue)
    }

    fn has_continuation() -> bool {
        true
    }

    fn extractor(_: &MicroConfig) -> Option<Arc<dyn RefExtractor>> {
        Some(Arc::new(MicroRefs))
    }

    fn sanctioned(_: &MicroConfig, message: &str) -> bool {
        message == ABORT
    }

    fn projection(&self) -> Option<Hash> {
        Some(tip(self.committed))
    }
}

/// MicroLog's dependency declarations: the commit record needs every committed log byte, the
/// snapshot record the snapshot's.
struct MicroRefs;

impl RefExtractor for MicroRefs {
    fn refs(
        &self,
        db: &str,
        cf: Cf,
        key: &[u8],
        value: &[u8],
        _: &WorldDurableView,
    ) -> Vec<Requirement> {
        if db != "micro" || cf != META {
            return Vec::new();
        }
        match key {
            COMMITTED => {
                let (_, ends) = decode_committed(value);
                ends.iter()
                    .enumerate()
                    .filter(|(_, e)| **e > 0)
                    .map(|(f, e)| Requirement::Bytes(FileRef::new(log_path(f as u32), 0, *e)))
                    .collect()
            }
            SNAPSHOT => {
                let len = u64::from_le_bytes(value[4..12].try_into().expect("8"));
                if len == 0 {
                    Vec::new()
                } else {
                    vec![Requirement::Bytes(FileRef::new(snap_path(), 0, len))]
                }
            }
            _ => Vec::new(),
        }
    }
}
