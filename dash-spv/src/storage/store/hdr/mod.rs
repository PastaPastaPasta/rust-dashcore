//! The active chain's files (SB14 §2, D1–D3, D12–D14): v2's record format in
//! `block_headers/segment_NNNNNN.dat`, [`SEGMENT`] records a file, each the 80-byte header and its
//! 32-byte hash at `(h mod SEGMENT) × 112`. Only `[start, tip]` is meaningful.
//!
//! [`Hdr`] is what the lookup lock guards (D16): the two resident segment buffers, the fd cache,
//! the unsynced set, the index, the active range and its runs. It does no syncing of its own except
//! an fd-cache eviction of an unsynced file (D1); a raise takes a [`Snapshot`] under the lock and
//! syncs outside it.

pub mod index;
pub mod io;
pub mod slot;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use dash_chain::Work;
use dash_num::CompactTarget;

use index::{Index, HEIGHT_CAP};
use io::{HdrFile, HdrIo};
use slot::Committed;

use super::runs::Runs;

/// A record: the header, then its hash.
pub const REC: usize = 112;
pub type Rec = [u8; REC];
/// Records a segment file holds, in production.
pub const SEGMENT: u32 = 10_000;
/// The fd cache's bound (Opus r2 M5: iOS's default soft `RLIMIT_NOFILE` is 256).
pub const FD_CAP: usize = 32;
/// Resident segment buffers (D1, D14).
const RESIDENT: usize = 2;
/// The chain-work and `time_max` bases' spacing (§2).
pub const BASE_EVERY: u32 = 1_024;
/// The slots' file in `block_headers/`.
pub const TIP_FILE: &str = "tip";

pub fn segment_name(id: u32) -> String {
    format!("segment_{id:06}.dat")
}

/// The segment number of a file name, if it is a segment's.
pub fn segment_of(name: &str) -> Option<u32> {
    let n = name.strip_prefix("segment_")?.strip_suffix(".dat")?;
    (n.len() == 6 && n.bytes().all(|b| b.is_ascii_digit())).then(|| n.parse().ok())?
}

/// The record of `(hash, header)`.
pub fn record(hash: &[u8; 32], header: &[u8; 80]) -> Rec {
    let mut r = [0u8; REC];
    r[..80].copy_from_slice(header);
    r[80..].copy_from_slice(hash);
    r
}

pub fn rec_hash(r: &Rec) -> [u8; 32] {
    r[80..].try_into().expect("32 bytes")
}

pub fn rec_header(r: &Rec) -> [u8; 80] {
    r[..80].try_into().expect("80 bytes")
}

pub fn rec_prev(r: &Rec) -> [u8; 32] {
    r[4..36].try_into().expect("32 bytes")
}

/// The header's `nTime` and `nBits`.
pub fn time_and_bits(r: &[u8]) -> (u32, u32) {
    let le = |at: usize| u32::from_le_bytes(r[at..at + 4].try_into().expect("4 bytes"));
    (le(68), le(72))
}

/// Core's `GetBlockProof` of the header in `r`.
pub fn proof(r: &[u8]) -> Work {
    dash_consensus::pow::block_proof(CompactTarget::new(time_and_bits(r).1))
}

/// A failure of the files: I/O, or bytes that contradict the committed slot (corruption).
#[derive(Debug, thiserror::Error)]
pub enum FilesError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("SPV header files corrupt: {0}")]
    Corrupt(String),
}

fn corrupt(what: impl Into<String>) -> FilesError {
    FilesError::Corrupt(what.into())
}

struct Seg {
    id: u32,
    recs: Box<[Rec]>,
    /// The dirty range, `lo..hi` in records (empty when `lo >= hi`).
    lo: u32,
    hi: u32,
    tick: u64,
}

/// A raise's snapshot (§3 Raise step 1, D16): the unsynced files as owned handles, and what the
/// slot commits.
pub struct Snapshot<F: HdrIo> {
    pub files: Vec<Arc<F::File>>,
    /// A file in `block_headers/` was created since the last raise.
    pub created: bool,
    pub committed: Committed,
    /// The runs, when their rows differ from SQLite's, taken under the same lock: a slot over
    /// `committed` must carry them (D5 (3)).
    pub runs: Option<Runs>,
}

impl<F: HdrIo> Snapshot<F> {
    /// Raise step 2: `sync_data` every file.
    pub fn sync(&self) -> std::io::Result<()> {
        for f in &self.files {
            f.sync_data()?;
        }
        Ok(())
    }
}

/// The lookup lock's state (D16).
pub struct Hdr<F: HdrIo> {
    io: Arc<F>,
    dir: PathBuf,
    per: u32,
    segs: Vec<Seg>,
    spare: Option<Box<[Rec]>>,
    tick: u64,
    /// The fd cache: segment, handle, last use.
    fds: Vec<(u32, Arc<F::File>, u64)>,
    ftick: u64,
    /// Segments known to have a directory entry.
    existing: BTreeSet<u32>,
    created: bool,
    unsynced: Vec<u32>,
    /// Open found the tip segment longer than the tip's record end (step 5 cuts it).
    tip_excess: bool,
    pub start: Option<u32>,
    pub tip: Option<u32>,
    pub tip_hash: [u8; 32],
    /// The record at `start`'s hash (the derived anchor, when SQLite has no `meta`).
    pub start_hash: [u8; 32],
    /// Every active record links to the one below it (D12): cleared by an unlinked append, set
    /// again only by a clear.
    pub linked: bool,
    idx: Index,
    pub runs: Runs,
    /// The committed tip (§3): what the newest slot written names.
    pub committed: Committed,
    /// Off-chain records (D6): SQLite's `hdr_hash` rows.
    pub offchain: u64,
    /// D18's store-wide latch, which [`note`](Self::note) sets on an I/O error in a write path
    /// under the lookup lock (an append's or a write-back's write, an eviction's sync, or a read
    /// or open they needed): no slot may then cover the store's bytes.
    pub latch: Latch,
    /// The start record's chain work and `time_max`, when an override row gives them (dash-chain's
    /// anchor, D21); else its own proof and time.
    pub anchor: Option<(Work, u32)>,
    /// `(Σ proof, max time)` over `(start, b]` for `b` each [`BASE_EVERY`]-aligned height above
    /// `start`, in order; filled on demand.
    bases: Vec<(Work, u32)>,
}

/// D18's store-wide latch: the first I/O error in a write path, or the first failed write, sync
/// or slot-carrying commit. The lookup lock's state, the writer and the store share it, so every
/// mutating entry checks it without taking a lock; the first error set wins.
#[derive(Clone, Debug, Default)]
pub struct Latch(Arc<OnceLock<String>>);

impl Latch {
    pub fn set(&self, what: impl std::fmt::Display) {
        self.0.get_or_init(|| what.to_string());
    }

    pub fn get(&self) -> Option<&str> {
        self.0.get().map(String::as_str)
    }
}

impl<F: HdrIo> std::fmt::Debug for Hdr<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hdr")
            .field("start", &self.start)
            .field("tip", &self.tip)
            .field("committed", &self.committed)
            .field("linked", &self.linked)
            .field("offchain", &self.offchain)
            .finish_non_exhaustive()
    }
}

fn as_bytes(recs: &[Rec]) -> &[u8] {
    recs.as_flattened()
}

fn as_bytes_mut(recs: &mut [Rec]) -> &mut [u8] {
    recs.as_flattened_mut()
}

impl<F: HdrIo> Hdr<F> {
    /// An empty store's files in `dir` (which exists).
    pub fn empty(io: Arc<F>, dir: PathBuf, per: u32) -> Self {
        Hdr {
            io,
            dir,
            per,
            segs: Vec::with_capacity(RESIDENT),
            spare: None,
            tick: 0,
            fds: Vec::new(),
            ftick: 0,
            existing: BTreeSet::new(),
            created: false,
            unsynced: Vec::new(),
            tip_excess: false,
            start: None,
            tip: None,
            tip_hash: [0; 32],
            start_hash: [0; 32],
            linked: true,
            idx: Index::default(),
            runs: Runs::base(None),
            committed: Committed::Empty,
            offchain: 0,
            latch: Latch::default(),
            anchor: None,
            bases: Vec::new(),
        }
    }

    fn seg_path(&self, id: u32) -> PathBuf {
        self.dir.join(segment_name(id))
    }

    /// Open's read-only pass (§3 Open steps 2–3) over `[start, tip]` of `committed`, with the
    /// segments `segments` lists: contiguity and length, the tip hash, the linkage flag; the index.
    /// Nothing is changed on disk; [`truncate`](Self::truncate) is step 5's work.
    pub fn open(
        io: Arc<F>,
        dir: PathBuf,
        per: u32,
        committed: Committed,
        segments: &[u32],
    ) -> Result<Self, FilesError> {
        let mut s = Hdr::empty(io, dir, per);
        s.existing = segments.iter().copied().collect();
        s.committed = committed;
        let Committed::Range {
            start,
            tip,
            tip_hash,
            linked,
        } = committed
        else {
            return Ok(s);
        };
        if start > tip || tip >= HEIGHT_CAP {
            return Err(corrupt(format!("the slot's range {start}..={tip} is out of bounds")));
        }
        let (first, last) = (start / per, tip / per);
        let mut entries = Vec::with_capacity((tip - start + 1) as usize);
        let mut buf = vec![[0u8; REC]; per as usize].into_boxed_slice();
        let mut prev: Option<[u8; 32]> = None;
        let mut chain_linked = true;
        for id in first..=last {
            // Contiguity (r2 M2): every segment from start's to the tip's exists, long enough for
            // its share of [start, tip].
            let f =
                s.io.open(&s.seg_path(id), false)?
                    .ok_or_else(|| corrupt(format!("segment {id} under the slot is missing")))?;
            let lo = start.max(id * per) - id * per;
            let hi = tip.min(id * per + per - 1) - id * per + 1;
            let got = f.read_full(as_bytes_mut(&mut buf), 0)?;
            if got < hi as usize * REC {
                return Err(corrupt(format!(
                    "segment {id} holds {got} bytes, short of the slot's {}",
                    hi as usize * REC
                )));
            }
            s.tip_excess = id == last && got > hi as usize * REC;
            for o in lo..hi {
                let r = &buf[o as usize];
                let h = id * per + o;
                if prev.is_some_and(|p| p != rec_prev(r)) {
                    chain_linked = false;
                }
                let hash = rec_hash(r);
                if h == start {
                    s.start_hash = hash;
                }
                prev = Some(hash);
                entries.push(index::key(&hash) << index::HEIGHT_BITS | u64::from(h));
            }
            s.cache_fd(id, Arc::new(f));
        }
        if prev != Some(tip_hash) {
            return Err(corrupt(format!("the record at {tip} is not the slot's tip")));
        }
        if linked && !chain_linked {
            return Err(corrupt("a linked slot over records that do not link"));
        }
        s.start = Some(start);
        s.tip = Some(tip);
        s.tip_hash = tip_hash;
        s.linked = linked && chain_linked;
        s.idx = Index::from_entries(entries);
        s.runs = Runs::base(Some((start, tip)));
        Ok(s)
    }

    /// Open step 5: the segment files outside `[start, tip]`'s (every one when nothing is
    /// active), and the tip segment's length past the tip.
    pub fn truncate(&mut self, all_segments: &[u32]) -> std::io::Result<()> {
        let range = self.start.zip(self.tip).map(|(s, t)| (s / self.per, t / self.per));
        for &id in all_segments {
            if range.is_none_or(|(a, b)| id < a || id > b) {
                self.drop_fd(id);
                self.io.remove(&self.seg_path(id))?;
                self.existing.remove(&id);
            }
        }
        if let Some(t) = self.tip.filter(|_| std::mem::take(&mut self.tip_excess)) {
            let len = u64::from(t % self.per + 1) * REC as u64;
            self.fd(t / self.per, false)?.set_len(len)?;
        }
        Ok(())
    }

    fn drop_fd(&mut self, id: u32) {
        self.fds.retain(|(i, _, _)| *i != id);
        self.unsynced.retain(|&u| u != id);
    }

    /// Caches `f` for segment `id`, evicting the least recently used fd; an unsynced file is
    /// `sync_data`ed before its fd closes (D1). A failed sync latches (D18) and the fd is evicted
    /// anyway: the latch keeps every slot off the lost bytes, the lookup that evicted carries on,
    /// and no sync runs once one has failed.
    fn cache_fd(&mut self, id: u32, f: Arc<F::File>) {
        if self.fds.len() >= FD_CAP {
            let (i, _) =
                self.fds.iter().enumerate().min_by_key(|(_, (_, _, t))| *t).expect("a full cache");
            let lru = self.fds[i].0;
            if let Some(p) = self.unsynced.iter().position(|&u| u == lru) {
                if self.latch.get().is_none() {
                    let synced = self.fds[i].1.sync_data();
                    // Latched; the lookup goes on.
                    let _ = self.note(synced);
                }
                self.unsynced.swap_remove(p);
            }
            self.fds.swap_remove(i);
        }
        self.ftick += 1;
        self.fds.push((id, f, self.ftick));
    }

    /// Segment `id`'s fd, opened (and with `create`, created) if not cached.
    fn fd(&mut self, id: u32, create: bool) -> std::io::Result<Arc<F::File>> {
        self.ftick += 1;
        let t = self.ftick;
        if let Some(e) = self.fds.iter_mut().find(|(i, _, _)| *i == id) {
            e.2 = t;
            return Ok(Arc::clone(&e.1));
        }
        let path = self.seg_path(id);
        let f = match self.io.open(&path, create)? {
            Some(f) => Arc::new(f),
            None => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{} is missing", path.display()),
                ))
            }
        };
        if create && self.existing.insert(id) {
            self.created = true;
        }
        self.cache_fd(id, Arc::clone(&f));
        Ok(f)
    }

    /// Writes segment buffer `i`'s dirty range (one `pwrite`).
    fn write_back(&mut self, i: usize) -> std::io::Result<()> {
        let (id, lo, hi) = {
            let s = &self.segs[i];
            (s.id, s.lo, s.hi)
        };
        if lo >= hi {
            return Ok(());
        }
        let f = self.fd(id, true)?;
        let s = &self.segs[i];
        let done =
            f.write_all_at(as_bytes(&s.recs[lo as usize..hi as usize]), u64::from(lo) * REC as u64);
        // A failed write keeps its range dirty, and latches (D18).
        self.note(done)?;
        let s = &mut self.segs[i];
        s.lo = u32::MAX;
        s.hi = 0;
        if !self.unsynced.contains(&id) {
            self.unsynced.push(id);
        }
        Ok(())
    }

    /// The latched error (D18), if any.
    pub fn check_failed(&self) -> std::io::Result<()> {
        match self.latch.get() {
            Some(what) => Err(std::io::Error::other(format!("an earlier write failed: {what}"))),
            None => Ok(()),
        }
    }

    /// Latches an I/O error in a write path (D18; see [`latch`](Self::latch)), returning it.
    pub fn note<T>(&mut self, r: std::io::Result<T>) -> std::io::Result<T> {
        if let Err(e) = &r {
            self.latch.set(e);
        }
        r
    }

    /// The resident buffer of segment `id`, loaded (evicting the least recently used one) if
    /// needed.
    fn seg_mut(&mut self, id: u32) -> std::io::Result<&mut Seg> {
        self.tick += 1;
        let t = self.tick;
        if let Some(i) = self.segs.iter().position(|s| s.id == id) {
            self.segs[i].tick = t;
            return Ok(&mut self.segs[i]);
        }
        if self.segs.len() >= RESIDENT {
            let (i, _) =
                self.segs.iter().enumerate().min_by_key(|(_, s)| s.tick).expect("resident");
            self.write_back(i)?;
            self.spare = Some(self.segs.swap_remove(i).recs);
        }
        let mut recs = match self.spare.take() {
            Some(b) => b,
            None => vec![[0u8; REC]; self.per as usize].into_boxed_slice(),
        };
        if self.existing.contains(&id) {
            let f = self.fd(id, false)?;
            let raw = as_bytes_mut(&mut recs);
            let n = f.read_full(raw, 0)?;
            raw[n..].fill(0);
        } else {
            as_bytes_mut(&mut recs).fill(0);
        }
        self.segs.push(Seg {
            id,
            recs,
            lo: u32::MAX,
            hi: 0,
            tick: t,
        });
        Ok(self.segs.last_mut().expect("just pushed"))
    }

    /// The record at `h`, which the caller checked is in `[start, tip]`.
    pub fn rec(&mut self, h: u32) -> std::io::Result<Rec> {
        let (id, o) = (h / self.per, h % self.per);
        if let Some(s) = self.segs.iter().find(|s| s.id == id) {
            return Ok(s.recs[o as usize]);
        }
        let mut r = [0u8; REC];
        self.fd(id, false)?.read_at_exact(&mut r, u64::from(o) * REC as u64)?;
        Ok(r)
    }

    /// The hash at `h` (in `[start, tip]`).
    pub fn hash_at(&mut self, h: u32) -> std::io::Result<[u8; 32]> {
        let (id, o) = (h / self.per, h % self.per);
        if let Some(s) = self.segs.iter().find(|s| s.id == id) {
            return Ok(rec_hash(&s.recs[o as usize]));
        }
        let mut b = [0u8; 32];
        self.fd(id, false)?.read_at_exact(&mut b, u64::from(o) * REC as u64 + 80)?;
        Ok(b)
    }

    /// The records at `range` (within `[start, tip]`): one read a segment.
    pub fn recs(&mut self, range: std::ops::Range<u32>) -> std::io::Result<Vec<Rec>> {
        let mut out = vec![[0u8; REC]; range.len()];
        let mut h = range.start;
        while h < range.end {
            let (id, o) = (h / self.per, h % self.per);
            let n = (self.per - o).min(range.end - h);
            let at = (h - range.start) as usize;
            let dst = &mut out[at..at + n as usize];
            match self.segs.iter().find(|s| s.id == id) {
                Some(s) => dst.copy_from_slice(&s.recs[o as usize..(o + n) as usize]),
                None => self
                    .fd(id, false)?
                    .read_at_exact(as_bytes_mut(dst), u64::from(o) * REC as u64)?,
            }
            h += n;
        }
        Ok(out)
    }

    /// Whether the active range holds `h`.
    pub fn holds(&self, h: u32) -> bool {
        self.start.is_some_and(|s| h >= s) && self.tip.is_some_and(|t| h <= t)
    }

    /// The active height of `hash`: every index candidate verified against the stored hash,
    /// bounded by the tip (D3).
    pub fn find(&mut self, hash: &[u8; 32]) -> std::io::Result<Option<u32>> {
        let mut cands: Vec<u32> = Vec::new();
        let tip = self.tip;
        self.idx.candidates(index::key(hash), |h| {
            if tip.is_some_and(|t| h <= t) {
                cands.push(h);
            }
            false
        });
        cands.sort_unstable_by(|a, b| b.cmp(a));
        cands.dedup();
        for h in cands {
            if self.holds(h) && self.hash_at(h)? == *hash {
                return Ok(Some(h));
            }
        }
        Ok(None)
    }

    /// Whether `headers` link to the tip and through themselves (the first to anything on an
    /// empty store).
    pub fn links(&self, headers: &[([u8; 32], [u8; 80])]) -> bool {
        let mut prev = match self.tip {
            Some(_) => self.tip_hash,
            None => match headers.first() {
                Some((_, h)) => h[4..36].try_into().expect("32 bytes"),
                None => return true,
            },
        };
        for (hash, header) in headers {
            if header[4..36] != prev {
                return false;
            }
            prev = *hash;
        }
        true
    }

    /// Writes `headers` at `h0` (the tip + 1, or any height on an empty store) into the
    /// resident buffers and the index; the caller has checked the height cap and the stored-
    /// header rule, and assigns the ids. Clears `linked` for a run that does not link.
    pub fn append(&mut self, h0: u32, headers: &[([u8; 32], [u8; 80])]) -> std::io::Result<()> {
        let Some(last) = headers.len().checked_sub(1) else {
            return Ok(());
        };
        if !self.links(headers) {
            self.linked = false;
        }
        let top = h0 + last as u32;
        for (i, (hash, header)) in headers.iter().enumerate() {
            let h = h0 + i as u32;
            let (id, o) = (h / self.per, h % self.per);
            let s = self.seg_mut(id)?;
            s.recs[o as usize] = record(hash, header);
            s.lo = s.lo.min(o);
            s.hi = s.hi.max(o + 1);
            self.idx.insert(index::key(hash), h, top);
            if self.start.is_none() {
                self.start = Some(h);
                self.start_hash = *hash;
            }
            self.tip = Some(h);
        }
        self.tip_hash = headers[last].0;
        Ok(())
    }

    /// Lowers the active chain to `tip` (`None`: nothing active) in memory: the index's buffer,
    /// the dirty ranges above it, the bases. The files keep their bytes above it: they are
    /// garbage, and open truncates them.
    pub fn lower(&mut self, tip: Option<u32>) -> std::io::Result<()> {
        let tip = tip.filter(|&t| self.start.is_some_and(|s| t >= s));
        self.idx.lower(tip);
        let per = self.per;
        for s in &mut self.segs {
            let cut = match tip {
                Some(t) if t / per > s.id => continue,
                Some(t) if t / per == s.id => t % per + 1,
                _ => 0,
            };
            s.hi = s.hi.min(cut);
        }
        match tip {
            Some(t) => {
                self.tip = Some(t);
                self.tip_hash = self.hash_at(t)?;
                let keep = self.base_index(t).map_or(0, |k| k + 1);
                self.bases.truncate(keep);
            }
            None => {
                self.start = None;
                self.tip = None;
                self.tip_hash = [0; 32];
                self.start_hash = [0; 32];
                self.linked = true;
                self.bases.clear();
                self.anchor = None;
            }
        }
        Ok(())
    }

    /// Raise step 1 (D16): writes every dirty range, and takes the unsynced set as owned
    /// handles with what the slot will commit.
    pub fn snapshot(&mut self) -> std::io::Result<Snapshot<F>> {
        self.check_failed()?;
        for i in 0..self.segs.len() {
            self.write_back(i)?;
        }
        let mut files = Vec::with_capacity(self.unsynced.len());
        for id in std::mem::take(&mut self.unsynced) {
            files.push(self.fd(id, false)?);
        }
        Ok(Snapshot {
            files,
            created: std::mem::take(&mut self.created),
            committed: self.live(),
            runs: self.runs.dirty.then(|| self.runs.clone()),
        })
    }

    /// Whether a raise has work: a dirty range or an unsynced file.
    pub fn dirty(&self) -> bool {
        !self.unsynced.is_empty() || self.segs.iter().any(|s| s.lo < s.hi)
    }

    /// What a slot written now would commit: the in-memory range.
    pub fn live(&self) -> Committed {
        match (self.start, self.tip) {
            (Some(start), Some(tip)) => Committed::Range {
                start,
                tip,
                tip_hash: self.tip_hash,
                linked: self.linked,
            },
            _ => Committed::Empty,
        }
    }

    /// The `(work, time_max)` base index of height `h` (above `start`): the base at the highest
    /// aligned height at or below `h`, `None` when that is `start` itself.
    fn base_index(&self, h: u32) -> Option<usize> {
        let start = self.start?;
        let first = start / BASE_EVERY + 1;
        let k = h / BASE_EVERY;
        (k >= first).then(|| (k - first) as usize)
    }

    /// The chain work and `time_max` of the active record at `h` (in `[start, tip]`), derived
    /// (§2): the start record's, plus every proof above it, and the running maximum of the
    /// times. Bases each [`BASE_EVERY`] heights are filled as reads reach them.
    pub fn derived(&mut self, h: u32) -> std::io::Result<(Work, u32)> {
        let start = self.start.expect("an active height");
        let (w0, t0) = match self.anchor {
            Some(a) => a,
            None => {
                let r = self.rec(start)?;
                (proof(&r), time_and_bits(&r).0)
            }
        };
        // Fill bases up to h's.
        let want = self.base_index(h);
        while want.is_some_and(|w| self.bases.len() <= w) {
            let k = self.bases.len();
            let from = match k {
                0 => start + 1,
                _ => (start / BASE_EVERY + k as u32) * BASE_EVERY + 1,
            };
            let to = (start / BASE_EVERY + k as u32 + 1) * BASE_EVERY;
            let (mut w, mut t) = match k {
                0 => (Work::from_u64(0), 0),
                _ => self.bases[k - 1],
            };
            for r in self.recs(from..to + 1)? {
                w += proof(&r);
                t = t.max(time_and_bits(&r).0);
            }
            self.bases.push((w, t));
        }
        let (mut w, mut t, from) = match want {
            Some(k) => {
                let (w, t) = self.bases[k];
                (w, t, (start / BASE_EVERY + k as u32 + 1) * BASE_EVERY + 1)
            }
            None => (Work::from_u64(0), 0, start + 1),
        };
        if from <= h {
            for r in self.recs(from..h + 1)? {
                w += proof(&r);
                t = t.max(time_and_bits(&r).0);
            }
        }
        Ok((w0 + w, t0.max(t)))
    }

    /// Merges the index's buffer (tests).
    #[cfg(test)]
    pub fn merge_index(&mut self) {
        if let Some(t) = self.tip {
            self.idx.merge(t);
        }
    }
}

/// `read_at` that fills `buf` or fails.
trait ReadExact {
    fn read_at_exact(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
}

impl<T: HdrFile + ?Sized> ReadExact for T {
    fn read_at_exact(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let n = self.read_full(buf, offset)?;
        if n < buf.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("a header record at {offset} is short ({n} of {} bytes)", buf.len()),
            ));
        }
        Ok(())
    }
}

/// The segment numbers among `names`, sorted.
pub fn segments_in(names: &[String]) -> Vec<u32> {
    let mut v: Vec<u32> = names.iter().filter_map(|n| segment_of(n)).collect();
    v.sort_unstable();
    v
}
