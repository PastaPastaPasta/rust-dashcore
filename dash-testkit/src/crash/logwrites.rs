//! A `dm-log-writes` log: its entries, and the device states a power loss can leave.
//!
//! The format is the kernel's (`drivers/md/dm-log-writes.c`,
//! `Documentation/admin-guide/device-mapper/log-writes.rst`):
//! - block 0 is the super block: `magic`, `version`, `nr_entries` (little-endian u64s), then
//!   `sectorsize` (u32), padded to one block of `sectorsize` bytes;
//! - each entry is one block holding `sector`, `nr_sectors`, `flags` and `data_len` (u64s, the first
//!   two in blocks of `sectorsize`), with a mark's name inline after them, followed by the write's
//!   `nr_sectors` blocks of data (none for a flush, a discard or a mark).
//!
//! Entries are logged in completion order. A flush entry makes every earlier entry durable (the
//! kernel logs the writes it covers just before it, and dm splits a pre-flush write into an empty
//! flush and the write, so a data entry carrying the flush flag does not occur in practice; one is
//! treated as a flush anyway); a FUA write is durable once it completes.

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// `WRITE_LOG_MAGIC`.
pub const MAGIC: u64 = 0x006a_7366_7773_6872;
/// `WRITE_LOG_VERSION`.
pub const VERSION: u64 = 1;

/// The entry flags.
pub const FLUSH: u64 = 1 << 0;
pub const FUA: u64 = 1 << 1;
pub const DISCARD: u64 = 1 << 2;
pub const MARK: u64 = 1 << 3;
pub const METADATA: u64 = 1 << 4;

const HEADER: usize = 32;

/// One logged bio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The first block written, in blocks of the log's sector size.
    pub sector: u64,
    pub nr_sectors: u64,
    pub flags: u64,
    /// Where the write's data starts in the log file (`nr_sectors` blocks); meaningless for
    /// entries without data.
    pub data_offset: u64,
    /// A mark's name.
    pub mark: Option<String>,
}

impl Entry {
    pub fn is(&self, flag: u64) -> bool {
        self.flags & flag != 0
    }

    /// Whether replaying it writes data (a discard zeroes, a flush or a mark writes nothing).
    pub fn has_data(&self) -> bool {
        !self.is(DISCARD) && !self.is(MARK) && self.nr_sectors > 0
    }
}

/// Why a log could not be read.
#[derive(Debug)]
pub enum LogError {
    Io(io::Error),
    /// Not a version-1 log-writes super block.
    Super(String),
    /// The log ends inside entry `index`.
    Truncated {
        index: u64,
    },
    /// Entry `index` is malformed (a mark longer than its block, an unknown flag).
    Entry {
        index: u64,
        why: String,
    },
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogError::Io(e) => write!(f, "reading the log: {e}"),
            LogError::Super(why) => write!(f, "log super block: {why}"),
            LogError::Truncated {
                index,
            } => write!(f, "the log ends inside entry {index}"),
            LogError::Entry {
                index,
                why,
            } => write!(f, "log entry {index}: {why}"),
        }
    }
}

impl std::error::Error for LogError {}

impl From<io::Error> for LogError {
    fn from(e: io::Error) -> Self {
        LogError::Io(e)
    }
}

/// A log's index: every entry's header, with the data left in the file.
#[derive(Clone, Debug)]
pub struct Log {
    pub sector_size: u32,
    pub entries: Vec<Entry>,
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().expect("8 bytes"))
}

impl Log {
    /// Reads the index of the log at `path`.
    pub fn open(path: &Path) -> Result<Log, LogError> {
        Self::read(File::open(path)?)
    }

    /// Reads the index of a log, seeking over each write's data.
    pub fn read<R: Read + Seek>(r: R) -> Result<Log, LogError> {
        let mut r = BufReader::with_capacity(1 << 16, r);
        let mut sup = [0u8; HEADER - 4];
        r.read_exact(&mut sup).map_err(|_| LogError::Super("shorter than its fields".into()))?;
        let (magic, version, count) = (le64(&sup, 0), le64(&sup, 8), le64(&sup, 16));
        let sector_size = u32::from_le_bytes(sup[24..28].try_into().expect("4 bytes"));
        if magic != MAGIC {
            return Err(LogError::Super(format!("magic {magic:#x}")));
        }
        if version != VERSION {
            return Err(LogError::Super(format!("version {version}")));
        }
        if !sector_size.is_power_of_two() || sector_size < 512 {
            return Err(LogError::Super(format!("sector size {sector_size}")));
        }
        let block = u64::from(sector_size);
        let mut entries = Vec::with_capacity(usize::try_from(count).unwrap_or(0).min(1 << 20));
        let mut at = block;
        let mut head = vec![0u8; block as usize];
        r.seek(SeekFrom::Start(at))?;
        for index in 0..count {
            r.read_exact(&mut head).map_err(|_| LogError::Truncated {
                index,
            })?;
            let (sector, nr_sectors, flags, data_len) =
                (le64(&head, 0), le64(&head, 8), le64(&head, 16), le64(&head, 24));
            let bad = |why: String| LogError::Entry {
                index,
                why,
            };
            if flags & !(FLUSH | FUA | DISCARD | MARK | METADATA) != 0 {
                return Err(bad(format!("flags {flags:#x}")));
            }
            let mark = if flags & MARK != 0 {
                let len = usize::try_from(data_len).ok().filter(|&n| n <= head.len() - HEADER);
                let len = len.ok_or_else(|| bad(format!("mark of {data_len} bytes")))?;
                Some(String::from_utf8_lossy(&head[HEADER..HEADER + len]).into_owned())
            } else {
                None
            };
            let entry = Entry {
                sector,
                nr_sectors,
                flags,
                data_offset: at + block,
                mark,
            };
            at += block;
            if entry.has_data() {
                let len = nr_sectors
                    .checked_mul(block)
                    .filter(|&n| at.checked_add(n).is_some() && i64::try_from(n).is_ok())
                    .ok_or_else(|| bad(format!("{nr_sectors} sectors")))?;
                at += len;
                // Relative, so the reader keeps its buffer over small writes.
                r.seek_relative(len as i64)?;
            }
            entries.push(entry);
        }
        // The last entry's data must be in the file too.
        let end = r.seek(SeekFrom::End(0))?;
        if at > end {
            return Err(LogError::Truncated {
                index: count.saturating_sub(1),
            });
        }
        Ok(Log {
            sector_size,
            entries,
        })
    }

    /// The prefix length just past the first mark named `name`.
    pub fn mark(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.mark.as_deref() == Some(name)).map(|i| i + 1)
    }

    /// Every mark, as (prefix length just past it, name).
    pub fn marks(&self) -> impl Iterator<Item = (usize, &str)> {
        self.entries.iter().enumerate().filter_map(|(i, e)| e.mark.as_deref().map(|m| (i + 1, m)))
    }
}

/// A device state a power loss can leave: the first `prefix` entries, plus, for a torn or split
/// point, the listed later entries and, for a split point, part of one more (in log order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Point {
    pub kind: PointKind,
    pub prefix: usize,
    /// Entries past `prefix` that survived whole (torn and split points only).
    pub extra: Vec<usize>,
    /// The entry past `prefix` of which only some sectors survived (split points only).
    pub part: Option<Part>,
}

/// Part of one data entry: the blocks of it (offsets from its first, in the log's sectors,
/// ascending) that reached the disk, whole [`SPLIT_UNIT`]s, at least one and not all.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Part {
    pub entry: usize,
    pub blocks: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PointKind {
    /// Just before a flush took effect: everything before it is durable.
    Flush,
    /// Just after a FUA write.
    Fua,
    /// A seeded entry boundary.
    Rand,
    /// A flush point plus a seeded subset of the writes completed before the next flush, every
    /// FUA write among them included (the volatile cache lost the rest in any combination).
    Torn,
    /// A torn point that also keeps part of one more write of the window, not a FUA one, of two or
    /// more [`SPLIT_UNIT`]s: its first k units, or a seeded subset of them (a write torn inside its
    /// bio).
    Split,
}

impl fmt::Display for PointKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            PointKind::Flush => "flush",
            PointKind::Fua => "fua",
            PointKind::Rand => "rand",
            PointKind::Torn => "torn",
            PointKind::Split => "split",
        })
    }
}

/// The last entry whose acknowledgements a point's state must hold: its prefix, or for a torn or
/// split point the next flush. Such a point keeps every FUA write up to that flush, and an fsync is
/// durable once its flush and FUA writes complete, so the acknowledgements before it count too.
pub fn window_end(log: &Log, pt: &Point) -> usize {
    match pt.kind {
        PointKind::Torn | PointKind::Split => (pt.prefix + 1..log.entries.len())
            .find(|&i| log.entries[i].is(FLUSH))
            .unwrap_or(log.entries.len()),
        _ => pt.prefix,
    }
}

/// SplitMix64: a small seeded generator, so a point list is reproducible from its seed.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n` > 0).
    pub fn below(&mut self, n: u64) -> u64 {
        // Rejection keeps it unbiased.
        let zone = u64::MAX - u64::MAX % n;
        loop {
            let x = self.next_u64();
            if x < zone {
                return x % n;
            }
        }
    }
}

/// How many points of each seeded kind to draw.
#[derive(Clone, Copy, Debug, Default)]
pub struct Draw {
    pub rand: usize,
    pub torn: usize,
    pub seed: u64,
    /// Drawn after the others, so a seed's rand and torn points do not depend on it.
    pub split: usize,
}

/// The replay points of the entries from `start` on: every flush and FUA point and the seeded
/// rand and torn ones `draw` asks for, in prefix order (duplicates by prefix and extras dropped),
/// then its split points, in prefix order. Split points come last so that a point's index (its
/// `pt<i>` name, shard, `--points` window, C5/C6 cadence and seed) is the same with or without
/// them (ST3b r2 n-1); replay them in [`replay_order`].
pub fn points(log: &Log, start: usize, draw: Draw) -> Vec<Point> {
    let e = &log.entries;
    let mut out = Vec::new();
    let mut flushes = Vec::new();
    for (i, entry) in e.iter().enumerate().skip(start) {
        if entry.is(FLUSH) {
            flushes.push(i);
            out.push(Point {
                kind: PointKind::Flush,
                prefix: i,
                extra: vec![],
                part: None,
            });
        }
        if entry.is(FUA) {
            out.push(Point {
                kind: PointKind::Fua,
                prefix: i + 1,
                extra: vec![],
                part: None,
            });
        }
    }
    let mut rng = Rng::new(draw.seed);
    if e.len() > start {
        for _ in 0..draw.rand {
            let span = (e.len() - start + 1) as u64;
            out.push(Point {
                kind: PointKind::Rand,
                prefix: start + rng.below(span) as usize,
                extra: vec![],
                part: None,
            });
        }
    }
    // A torn point's window: from a flush (or the start) up to the next flush.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    let mut from = start;
    for &f in &flushes {
        if f > from {
            windows.push((from, f));
        }
        from = f;
    }
    if e.len() > from {
        windows.push((from, e.len()));
    }
    // A split point's window and its writes that can tear: data writes of two or more units, not
    // FUA.
    let splittable = |i: usize| {
        let x = &e[i];
        x.has_data() && !x.is(FUA) && units(x, log.sector_size).len() > 1
    };
    let split_windows: Vec<(usize, usize)> =
        windows.iter().copied().filter(|&(a, b)| (a..b).any(splittable)).collect();
    windows.retain(|&(a, b)| e[a..b].iter().filter(|x| x.has_data() || x.is(DISCARD)).count() > 1);
    // The flush entry at `a` itself (a pre-flush write's data included) may be lost.
    let survivors = |rng: &mut Rng, (a, b): (usize, usize), skip: Option<usize>| -> Vec<usize> {
        (a..b)
            .filter(|&i| {
                let x = &e[i];
                Some(i) != skip
                    && (x.has_data() || x.is(DISCARD))
                    && (x.is(FUA) || rng.below(2) == 0)
            })
            .collect()
    };
    if !windows.is_empty() {
        for _ in 0..draw.torn {
            let (a, b) = windows[rng.below(windows.len() as u64) as usize];
            out.push(Point {
                kind: PointKind::Torn,
                prefix: a,
                extra: survivors(&mut rng, (a, b), None),
                part: None,
            });
        }
    }
    let by_prefix = |x: &Point, y: &Point| {
        (x.prefix, x.kind, &x.extra, &x.part).cmp(&(y.prefix, y.kind, &y.extra, &y.part))
    };
    out.sort_by(by_prefix);
    out.dedup_by(|x, y| x.extra.is_empty() && y.extra.is_empty() && x.prefix == y.prefix);
    let mut split = Vec::new();
    if !split_windows.is_empty() {
        for _ in 0..draw.split {
            let (a, b) = split_windows[rng.below(split_windows.len() as u64) as usize];
            let candidates: Vec<usize> = (a..b).filter(|&i| splittable(i)).collect();
            let entry = candidates[rng.below(candidates.len() as u64) as usize];
            let units = units(&e[entry], log.sector_size);
            let n = units.len() as u64;
            let kept: Vec<u64> = if rng.below(2) == 0 {
                (0..1 + rng.below(n - 1)).collect()
            } else {
                let mut kept: Vec<u64> = (0..n).filter(|_| rng.below(2) == 0).collect();
                // At least one unit kept and one lost: toggle unit 0 if not.
                if kept.is_empty() {
                    kept.push(0);
                } else if kept.len() as u64 == n {
                    kept.remove(0);
                }
                kept
            };
            let blocks = kept.iter().flat_map(|&u| units[u as usize].clone()).collect();
            split.push(Point {
                kind: PointKind::Split,
                prefix: a,
                extra: survivors(&mut rng, (a, b), Some(entry)),
                part: Some(Part {
                    entry,
                    blocks,
                }),
            });
        }
    }
    split.sort_by(by_prefix);
    out.extend(split);
    out
}

/// The unit a split point tears at: 4 KiB, aligned on the device, what a 4Kn or 512e device writes
/// atomically. At the log's 512 B sector a split could keep one half of ext4's 1 KiB superblock
/// (ST3b r2: the leak test's ext4 selftest then failed to mount, "invalid superblock checksum"), a
/// state such a device cannot leave.
pub const SPLIT_UNIT: u64 = 4096;

/// A data entry's blocks (offsets from its first, in the log's sectors), grouped by the
/// [`SPLIT_UNIT`] of the device each falls in, in order.
pub fn units(entry: &Entry, sector_size: u32) -> Vec<std::ops::Range<u64>> {
    let per = (SPLIT_UNIT / u64::from(sector_size)).max(1);
    let mut out: Vec<std::ops::Range<u64>> = Vec::new();
    for k in 0..entry.nr_sectors {
        match out.last_mut() {
            Some(r) if !(entry.sector + k).is_multiple_of(per) => r.end = k + 1,
            _ => out.push(k..k + 1),
        }
    }
    out
}

/// The indices of `points` in prefix order (stable), the order a [`Replayer`](super::image::Replayer)
/// can walk them in: it only moves forward.
pub fn replay_order(points: &[Point]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by_key(|&i| points[i].prefix);
    order
}

/// The device block an inversion is judged on: a page-cache page, which writeback logs whole.
pub const INVERSION_BLOCK: u64 = 4096;

/// A stale copy of one [`INVERSION_BLOCK`] logged after a newer one (OD-ST3b-8, BL-902): entry
/// `stale` logged a strict truncation of the content entry `fresh` logged for `block` (an
/// append-only page going backwards), and a state that holds `stale` but not `heal`, the next entry
/// that writes the block with anything else, may be one the device never passed through. Entries
/// are log indices, as a point's prefix counts them.
///
/// The capture can only misorder plain writes of different flush groups, so a window opens only
/// with a FLUSH entry strictly between `fresh` and `stale`, or with `fresh` a FUA write; and never
/// on a FUA `stale`, which is logged when it completes, so in the device's own order. A block
/// that really shrank looks the same, so only a `confirmed` inversion (its heal holds the fresh
/// copy whole, as the next append to the page does) qualifies; even then a campaign judges the
/// point and only replays the state with `fresh_at`'s copy put back after a failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Inversion {
    pub block: u64,
    pub fresh: usize,
    pub stale: usize,
    pub heal: Option<usize>,
    pub confirmed: bool,
    /// Where the fresh copy of the block starts in the log file.
    pub fresh_at: u64,
}

impl Inversion {
    /// Whether `pt`'s state surely holds the stale copy and not the confirming write that heals
    /// it: the stale entry before its prefix, the heal at or past it, and no extra or partial
    /// entry of the point at or past the heal.
    pub fn open_at(&self, pt: &Point) -> bool {
        self.confirmed
            && self.stale < pt.prefix
            && self.heal.is_some_and(|h| {
                h >= pt.prefix
                    && pt.extra.iter().chain(pt.part.as_ref().map(|p| &p.entry)).all(|&x| x < h)
            })
    }
}

/// The length of `b` without its trailing zeros.
fn nonzero_len(b: &[u8]) -> usize {
    b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1)
}

/// Every [`Inversion`] in `log`, whose data `data` holds, in order of the stale entry. Only whole,
/// aligned blocks are compared; a write covering part of a block, or a discard, ends what is known
/// of it (an open inversion on it heals there, unconfirmed). An all-zero copy is no truncation:
/// zeroing a block is something a filesystem does.
pub fn inversions<R: Read + Seek>(log: &Log, data: &mut R) -> io::Result<Vec<Inversion>> {
    #[derive(Clone, Copy)]
    struct Logged {
        at: u64,
        len: usize,
        entry: usize,
        /// FLUSH entries up to and including this one.
        flushes: usize,
        fua: bool,
    }
    let read_at = |data: &mut R, at: u64, buf: &mut [u8]| -> io::Result<()> {
        data.seek(SeekFrom::Start(at))?;
        data.read_exact(buf)
    };
    let sector = u64::from(log.sector_size);
    let mut last: HashMap<u64, Logged> = HashMap::new();
    let mut open: HashMap<u64, (Logged, usize)> = HashMap::new();
    let mut out = Vec::new();
    let close =
        |out: &mut Vec<Inversion>, block, (fresh, stale): (Logged, usize), heal, confirmed| {
            out.push(Inversion {
                block,
                fresh: fresh.entry,
                stale,
                heal,
                confirmed,
                fresh_at: fresh.at,
            })
        };
    let (mut buf, mut old) = (Vec::new(), vec![0u8; INVERSION_BLOCK as usize]);
    let mut flushes = 0;
    for (i, e) in log.entries.iter().enumerate() {
        let flushes_before = flushes;
        flushes += usize::from(e.is(FLUSH));
        if !e.has_data() && !e.is(DISCARD) {
            continue;
        }
        let from = e.sector.saturating_mul(sector);
        let to = e.sector.saturating_add(e.nr_sectors).saturating_mul(sector);
        let blocks = from / INVERSION_BLOCK..to.div_ceil(INVERSION_BLOCK);
        if !e.has_data() {
            last.retain(|b, _| !blocks.contains(b));
            for b in open.keys().copied().filter(|b| blocks.contains(b)).collect::<Vec<_>>() {
                let w = open.remove(&b).expect("listed");
                close(&mut out, b, w, Some(i), false);
            }
            continue;
        }
        buf.resize((to - from) as usize, 0);
        read_at(data, e.data_offset, &mut buf)?;
        for b in blocks {
            let start = b * INVERSION_BLOCK;
            if start < from || start + INVERSION_BLOCK > to {
                last.remove(&b);
                if let Some(w) = open.remove(&b) {
                    close(&mut out, b, w, Some(i), false);
                }
                continue;
            }
            let off = (start - from) as usize;
            let new = &buf[off..off + INVERSION_BLOCK as usize];
            let len = nonzero_len(new);
            let truncates =
                |old: &[u8], old_len: usize| len > 0 && len < old_len && new[..len] == old[..len];
            if let Some(&(fresh, stale)) = open.get(&b) {
                read_at(data, fresh.at, &mut old)?;
                if !truncates(&old, fresh.len) {
                    open.remove(&b);
                    let holds_fresh = len >= fresh.len && new[..fresh.len] == old[..fresh.len];
                    close(&mut out, b, (fresh, stale), Some(i), holds_fresh);
                }
            } else if let Some(&prev) = last.get(&b)
                && len > 0
                && len < prev.len
                && !e.is(FUA)
                && (prev.fua || flushes_before > prev.flushes)
            {
                read_at(data, prev.at, &mut old)?;
                if truncates(&old, prev.len) {
                    open.insert(b, (prev, i));
                }
            }
            last.insert(
                b,
                Logged {
                    at: e.data_offset + off as u64,
                    len,
                    entry: i,
                    flushes,
                    fua: e.is(FUA),
                },
            );
        }
    }
    for (b, w) in open {
        close(&mut out, b, w, None, false);
    }
    out.sort_by_key(|w| (w.stale, w.block));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A log of 512 B sectors whose data entries each write whole 4 KiB blocks, and its data.
    #[derive(Default)]
    struct Synth {
        entries: Vec<Entry>,
        data: Vec<u8>,
    }

    impl Synth {
        /// A write of `blocks` (each a block's leading bytes, the rest zeros) from 4 KiB block `at`.
        fn write(&mut self, at: u64, blocks: &[&[u8]]) -> &mut Self {
            self.write_sectors(at * 8, blocks.len() as u64 * 8, blocks)
        }

        fn write_sectors(&mut self, sector: u64, nr_sectors: u64, blocks: &[&[u8]]) -> &mut Self {
            let data_offset = self.data.len() as u64;
            let mut bytes = vec![0u8; nr_sectors as usize * 512];
            for (k, b) in blocks.iter().enumerate() {
                bytes[k * 4096..k * 4096 + b.len()].copy_from_slice(b);
            }
            self.data.extend(bytes);
            self.entries.push(Entry {
                sector,
                nr_sectors,
                flags: 0,
                data_offset,
                mark: None,
            });
            self
        }

        fn flag(&mut self, flags: u64, sector: u64, nr_sectors: u64) -> &mut Self {
            self.entries.push(Entry {
                sector,
                nr_sectors,
                flags,
                data_offset: 0,
                mark: None,
            });
            self
        }

        fn flush(&mut self) -> &mut Self {
            self.flag(FLUSH, 0, 0)
        }

        /// The last entry made FUA.
        fn fua(&mut self) -> &mut Self {
            self.entries.last_mut().expect("an entry").flags |= FUA;
            self
        }

        fn scan(&self) -> Vec<Inversion> {
            let log = Log {
                sector_size: 512,
                entries: self.entries.clone(),
            };
            inversions(&log, &mut io::Cursor::new(&self.data)).expect("a scan")
        }

        /// The scan as (block, fresh, stale, heal, confirmed).
        fn found(&self) -> Vec<(u64, usize, usize, Option<usize>, bool)> {
            self.scan().iter().map(|w| (w.block, w.fresh, w.stale, w.heal, w.confirmed)).collect()
        }
    }

    fn inv(block: u64, fresh: usize, stale: usize, heal: Option<usize>) -> Inversion {
        Inversion {
            block,
            fresh,
            stale,
            heal,
            confirmed: true,
            fresh_at: 0,
        }
    }

    fn at(prefix: usize, extra: &[usize]) -> Point {
        Point {
            kind: if extra.is_empty() {
                PointKind::Flush
            } else {
                PointKind::Torn
            },
            prefix,
            extra: extra.to_vec(),
            part: None,
        }
    }

    #[test]
    fn a_stale_copy_after_a_fresh_one_opens_until_a_healing_write() {
        let mut s = Synth::default();
        // 0: genesis; 1: blocks 0-5 (fresh); 2: flush; 3: an unrelated block; 4: blocks 0-1
        // (stale); 5: flush; 6: blocks 0-6 (heals).
        s.write(10, &[b"g"])
            .write(10, &[b"g12345"])
            .flush()
            .write(11, &[b"other"])
            .write(10, &[b"g1"])
            .flush()
            .write(10, &[b"g123456"]);
        let found = s.scan();
        assert_eq!(found.len(), 1);
        let w = &found[0];
        assert_eq!((w.block, w.fresh, w.stale, w.heal, w.confirmed), (10, 1, 4, Some(6), true));
        // The fresh copy, where the log holds it.
        let at_fresh = w.fresh_at as usize;
        assert_eq!(&s.data[at_fresh..at_fresh + 7], b"g12345\0");
        // Prefixes 5 and 6 hold the stale copy and not the heal; 4 and 7 do not.
        let open: Vec<usize> = (0..=7).filter(|&p| w.open_at(&at(p, &[]))).collect();
        assert_eq!(open, [5, 6]);
    }

    #[test]
    fn a_truncation_in_the_same_flush_group_is_no_inversion() {
        let mut s = Synth::default();
        // BL-902's bytes with no flush between the fresh and the stale copy: the log's order is
        // the device's, whatever it says (the flush after the stale copy does not count).
        s.write(10, &[b"g12345"])
            .write(11, &[b"x"])
            .write(10, &[b"g1"])
            .flush()
            .write(10, &[b"g123456"]);
        assert!(s.scan().is_empty());
        // A flush entry that is itself the fresh write's own does not count either.
        let mut s = Synth::default();
        s.write(10, &[b"g12345"]);
        s.entries.last_mut().unwrap().flags |= FLUSH;
        s.write(10, &[b"g1"]).flush().write(10, &[b"g123456"]);
        assert!(s.scan().is_empty());
    }

    #[test]
    fn a_fua_fresh_copy_needs_no_flush_between() {
        let mut s = Synth::default();
        s.write(10, &[b"g12345"]).fua().write(10, &[b"g1"]).write(10, &[b"g123456"]);
        assert_eq!(s.found(), [(10, 0, 1, Some(2), true)]);
    }

    #[test]
    fn a_fua_stale_copy_is_no_inversion() {
        let mut s = Synth::default();
        s.write(10, &[b"g12345"]).flush().write(10, &[b"g1"]).fua().write(10, &[b"g123456"]);
        assert!(s.scan().is_empty());
    }

    #[test]
    fn only_a_heal_that_holds_the_fresh_copy_confirms_an_inversion() {
        let mut s = Synth::default();
        // Never rewritten (0-2); rewritten with the fresh copy again (3-6); a bitmap that shrank
        // and then got another bit (7-10: the heal does not hold the fresh copy).
        s.write(3, &[b"abcdef"])
            .flush()
            .write(3, &[b"abc"])
            .write(4, &[b"abcdef"])
            .flush()
            .write(4, &[b"abc"])
            .write(4, &[b"abcdef"])
            .write(5, &[b"\xff\xff\x0f"])
            .flush()
            .write(5, &[b"\xff\xff"])
            .write(5, &[b"\xff\xff\xff"]);
        assert_eq!(
            s.found(),
            [(3, 0, 2, None, false), (4, 3, 5, Some(6), true), (5, 7, 9, Some(10), false)]
        );
        let found = s.scan();
        let open = |w: &Inversion| (0..=11).filter(|&p| w.open_at(&at(p, &[]))).collect::<Vec<_>>();
        assert!(open(&found[0]).is_empty());
        assert_eq!(open(&found[1]), [6]);
        assert!(open(&found[2]).is_empty());
    }

    #[test]
    fn overwrites_that_do_not_truncate_are_no_inversion() {
        let mut s = Synth::default();
        // Growing, changed, the same, rewritten inside its length, and zeroed: none counts.
        s.write(0, &[b"abc"])
            .flush()
            .write(0, &[b"abcdef"])
            .flush()
            .write(0, &[b"abXdef"])
            .flush()
            .write(0, &[b"abXdef"])
            .flush()
            .write(0, &[b"zz"])
            .flush()
            .write(0, &[b""]);
        assert!(s.scan().is_empty());
    }

    #[test]
    fn a_further_truncation_keeps_the_window_and_a_non_truncation_ends_it() {
        let mut s = Synth::default();
        // 0: fresh; 2: stale; 3: staler (still a truncation of 0); 4: another block; 5: a copy
        // that is not a truncation of 0 (heals, though shorter than 0).
        s.write(7, &[b"abcdef"])
            .flush()
            .write(7, &[b"abcd"])
            .write(7, &[b"ab"])
            .write(8, &[b"a"])
            .write(7, &[b"abX"]);
        assert_eq!(s.found(), [(7, 0, 2, Some(5), false)]);
    }

    #[test]
    fn blocks_of_one_write_are_judged_apart() {
        let mut s = Synth::default();
        // A two-block write, then one copying block 5 forward and block 4 back: only block 4.
        s.write(4, &[b"0123", b"x"]).flush().write(4, &[b"01", b"xy"]);
        assert_eq!(s.found(), [(4, 0, 2, None, false)]);
    }

    #[test]
    fn a_partial_write_or_a_discard_ends_what_is_known() {
        let mut s = Synth::default();
        // 0, 2: an inversion on block 2; 3: half of block 2 (heals it, unconfirmed); 4: block 2
        // whole again; 6: a truncation of 4 (an inversion); 7: a discard over it (heals).
        s.write(2, &[b"abcdef"])
            .flush()
            .write(2, &[b"abc"])
            .write_sectors(16, 4, &[b"q"])
            .write(2, &[b"mnopq"])
            .flush()
            .write(2, &[b"mn"])
            .flag(DISCARD, 16, 8)
            .write(2, &[b"m"]);
        assert_eq!(s.found(), [(2, 0, 2, Some(3), false), (2, 4, 6, Some(7), false)]);
    }

    #[test]
    fn a_torn_point_is_inside_only_without_a_kept_entry_at_or_past_the_heal() {
        let w = inv(9, 1, 4, Some(8));
        assert!(w.open_at(&at(5, &[5, 7])));
        assert!(!w.open_at(&at(5, &[5, 8])));
        assert!(!w.open_at(&at(5, &[9])));
        let mut split = at(5, &[6]);
        split.part = Some(Part {
            entry: 8,
            blocks: vec![0],
        });
        assert!(!w.open_at(&split));
        // A stale copy among the extras only does not count: it must come before the prefix.
        assert!(!w.open_at(&at(4, &[4])));
    }
}
