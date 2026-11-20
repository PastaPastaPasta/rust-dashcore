//! The import journal (§2.3): `CH6AJ1` followed by framed phase records.
//!
//! Each record is `u32 LE len ‖ u32 LE hcrc ‖ payload ‖ u32 LE crc`, where the
//! payload is the phase byte followed by that phase's fields, `crc` is the
//! masked crc32c of the payload, and `hcrc` is the masked crc32c of the length
//! bytes and the phase byte. The first nine bytes (length, `hcrc`, phase) are
//! the record's header. The length must be exactly what the fields define, and
//! never more than the phase's bound, which comes from the decoders' own
//! limits (`MAX_PATH`, `MAX_ENTRIES` manifest entries and as many method rows).
//!
//! A complete header whose `hcrc` fails, or that names the wrong phase or an
//! out-of-bound length, is corruption wherever it sits, unless all nine of its
//! bytes are zero. A torn final record is one of: the file ends inside its
//! header; it ends inside its payload after a valid header; its header is all
//! zeros; or its header is valid, its payload fails its crc, and the payload is
//! zero from its first byte up to the next [`SECTOR`]-aligned file offset (or
//! EOF, if that comes first). The last two are the page holding the record's
//! head never reaching the disk, and lost data always ends on a sector
//! boundary. In each case no complete, valid frame may start anywhere after
//! it, or the journal is corrupt. That scan hashes at most `SCAN_FACTOR`
//! payload bytes per journal byte; past that the journal is corrupt, which
//! only crafted overlapping headers reach. [`Journal::open`] only reads:
//! a torn final record is remembered and removed by [`Journal::repair`], which
//! only the resume path that owns the journal calls.
use super::fs::ImportFs;
use super::manifest::{MAX_ENCODED, MAX_ENTRIES, MAX_PATH, SourceManifest};
use super::{ImportError, Phase, PlacementPolicy, SourceMode, journal_corrupt};
use crate::leveldb::crc32c;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

pub(crate) const MAGIC: &[u8] = b"CH6AJ1";
/// Length, header crc and phase byte.
const HEADER: usize = 9;
/// Lost file data ends on a multiple of this: every page size is one.
pub const SECTOR: usize = 512;
/// Length, header crc and the trailing payload crc.
const OVERHEAD: usize = 12;
/// Linux's `NAME_MAX`: the longest blk file name in the method table.
const MAX_NAME: usize = 255;
/// The longest encoded method-table row.
const MAX_METHOD_ROW: usize = 4 + MAX_NAME + 1 + 4 + 4;
/// The bound on an `Intent` payload: phase, `at_ms`, source path, datadir,
/// `(st_dev, st_ino)`, mode, placement, `'B'`, `'l'` and the encoded M0.
pub(super) const MAX_INTENT: usize = 1 + 8 + 2 * (4 + MAX_PATH) + 16 + 2 + 33 + 5 + 4 + MAX_ENCODED;
/// The bound on a `Staged` payload: phase, `at_ms`, row count and the rows
/// (every staged file is an M0 entry, so there are at most `MAX_ENTRIES`).
pub(super) const MAX_STAGED: usize = 1 + 8 + 4 + MAX_ENTRIES * MAX_METHOD_ROW;
// Every bound fits the u32 length field.
const _: () = assert!(MAX_INTENT > MAX_STAGED && MAX_INTENT <= u32::MAX as usize);

fn masked_crc(payload: &[u8]) -> u32 {
    #[cfg(test)]
    crc_steps::count(payload.len());
    crc32c::mask(crc32c::extend(0, payload))
}

/// Counts crc calls and bytes hashed on this thread, and panics past a set
/// byte limit, so a test can bound the decoder's work by steps.
#[cfg(test)]
pub(super) mod crc_steps {
    use std::cell::Cell;
    thread_local! {
        static CALLS: Cell<u64> = const { Cell::new(0) };
        static BYTES: Cell<u64> = const { Cell::new(0) };
        static LIMIT: Cell<u64> = const { Cell::new(u64::MAX) };
    }
    pub(super) fn count(bytes: usize) {
        CALLS.set(CALLS.get() + 1);
        BYTES.set(BYTES.get() + bytes as u64);
        assert!(BYTES.get() <= LIMIT.get(), "crc byte limit {} exceeded", LIMIT.get());
    }
    /// Zeroes the counters and sets the byte limit.
    pub fn start(limit: u64) {
        CALLS.set(0);
        BYTES.set(0);
        LIMIT.set(limit);
    }
    /// `(calls, bytes)` since `start`; lifts the limit.
    pub fn finish() -> (u64, u64) {
        LIMIT.set(u64::MAX);
        (CALLS.get(), BYTES.get())
    }
}

/// The source the journal is bound to: canonical path plus `(st_dev, st_ino)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceBinding {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// How one blk file reached staging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Reflink,
    Hardlink,
    Copy,
}
impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reflink => "reflink",
            Self::Hardlink => "hardlink",
            Self::Copy => "copy",
        }
    }
}

/// One row of the per-file method table, with the errno of each fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMethod {
    /// The blk file's name.
    pub name: PathBuf,
    pub method: Method,
    /// The FICLONE errno that forced a fallback (0: none).
    pub reflink_errno: i32,
    /// The `link` errno that forced a copy (0: none).
    pub link_errno: i32,
}

/// The fields of the `Intent` record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    pub source: SourceBinding,
    /// The canonical `-importcoredatadir` the operator passed: the base dir,
    /// which is the network dir on mainnet and its parent elsewhere.
    pub datadir: PathBuf,
    pub mode: SourceMode,
    pub placement: PlacementPolicy,
    /// The source's `'B'`.
    pub best_block: Option<[u8; 32]>,
    /// The source's `'l'`.
    pub last_file: Option<u32>,
    pub m0: SourceManifest,
}

/// The status stored in the `Reported` record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportedStatus {
    Complete,
    Mismatch,
}
impl ReportedStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Mismatch => "mismatch",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordBody {
    Intent(Box<Intent>),
    Staged(Vec<FileMethod>),
    SourceVerified {
        m1_digest: [u8; 32],
    },
    Placed,
    ReindexArmed,
    Reported(ReportedStatus),
}

/// One durable phase record; `at_ms` is the wall clock when it was appended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub at_ms: u64,
    pub body: RecordBody,
}

impl Record {
    pub fn new(body: RecordBody, fs: &dyn ImportFs) -> Self {
        Self {
            at_ms: fs.now_ms(),
            body,
        }
    }
    pub fn phase(&self) -> Phase {
        match self.body {
            RecordBody::Intent(_) => Phase::Intent,
            RecordBody::Staged(_) => Phase::Staged,
            RecordBody::SourceVerified {
                ..
            } => Phase::SourceVerified,
            RecordBody::Placed => Phase::Placed,
            RecordBody::ReindexArmed => Phase::ReindexArmed,
            RecordBody::Reported(_) => Phase::Reported,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![self.phase() as u8];
        out.extend_from_slice(&self.at_ms.to_le_bytes());
        match &self.body {
            RecordBody::Intent(i) => {
                put_bytes(&mut out, i.source.path.as_os_str().as_bytes());
                put_bytes(&mut out, i.datadir.as_os_str().as_bytes());
                out.extend_from_slice(&i.source.dev.to_le_bytes());
                out.extend_from_slice(&i.source.ino.to_le_bytes());
                out.push(match i.mode {
                    SourceMode::Lock => 0,
                    SourceMode::Snapshot => 1,
                });
                out.push(match i.placement {
                    PlacementPolicy::Reflink => 0,
                    PlacementPolicy::Hardlink => 1,
                });
                match i.best_block {
                    Some(b) => {
                        out.push(1);
                        out.extend_from_slice(&b);
                    }
                    None => out.push(0),
                }
                match i.last_file {
                    Some(l) => {
                        out.push(1);
                        out.extend_from_slice(&l.to_le_bytes());
                    }
                    None => out.push(0),
                }
                put_bytes(&mut out, &i.m0.encode());
            }
            RecordBody::Staged(methods) => {
                out.extend_from_slice(&(methods.len() as u32).to_le_bytes());
                for m in methods {
                    put_bytes(&mut out, m.name.as_os_str().as_bytes());
                    out.push(match m.method {
                        Method::Reflink => 0,
                        Method::Hardlink => 1,
                        Method::Copy => 2,
                    });
                    out.extend_from_slice(&m.reflink_errno.to_le_bytes());
                    out.extend_from_slice(&m.link_errno.to_le_bytes());
                }
            }
            RecordBody::SourceVerified {
                m1_digest,
            } => out.extend_from_slice(m1_digest),
            RecordBody::Placed | RecordBody::ReindexArmed => {}
            RecordBody::Reported(s) => out.push(match s {
                ReportedStatus::Complete => 0,
                ReportedStatus::Mismatch => 1,
            }),
        }
        out
    }

    /// Decodes one payload; the payload must be exactly the fields' length.
    pub fn decode(payload: &[u8]) -> Option<Self> {
        let mut r = Reader {
            b: payload,
            p: 0,
        };
        let phase = r.u8()?;
        let at_ms = r.u64()?;
        let body = match phase {
            0 => {
                let path = PathBuf::from(OsStr::from_bytes(r.bytes_max(MAX_PATH)?));
                let datadir = PathBuf::from(OsStr::from_bytes(r.bytes_max(MAX_PATH)?));
                let dev = r.u64()?;
                let ino = r.u64()?;
                let mode = match r.u8()? {
                    0 => SourceMode::Lock,
                    1 => SourceMode::Snapshot,
                    _ => return None,
                };
                let placement = match r.u8()? {
                    0 => PlacementPolicy::Reflink,
                    1 => PlacementPolicy::Hardlink,
                    _ => return None,
                };
                let best_block = match r.u8()? {
                    0 => None,
                    1 => Some(r.take(32)?.try_into().ok()?),
                    _ => return None,
                };
                let last_file = match r.u8()? {
                    0 => None,
                    1 => Some(r.u32()?),
                    _ => return None,
                };
                let m0 = SourceManifest::decode(r.bytes_max(MAX_ENCODED)?)?;
                RecordBody::Intent(Box::new(Intent {
                    source: SourceBinding {
                        path,
                        dev,
                        ino,
                    },
                    datadir,
                    mode,
                    placement,
                    best_block,
                    last_file,
                    m0,
                }))
            }
            1 => {
                let n = r.u32()? as usize;
                // Each row is at least 13 bytes, so the count is bounded by the input.
                if n > MAX_ENTRIES || n > payload.len() / 13 {
                    return None;
                }
                let mut methods = Vec::with_capacity(n);
                for _ in 0..n {
                    let name = PathBuf::from(OsStr::from_bytes(r.bytes_max(MAX_NAME)?));
                    let method = match r.u8()? {
                        0 => Method::Reflink,
                        1 => Method::Hardlink,
                        2 => Method::Copy,
                        _ => return None,
                    };
                    methods.push(FileMethod {
                        name,
                        method,
                        reflink_errno: r.u32()? as i32,
                        link_errno: r.u32()? as i32,
                    });
                }
                RecordBody::Staged(methods)
            }
            2 => RecordBody::SourceVerified {
                m1_digest: r.take(32)?.try_into().ok()?,
            },
            3 => RecordBody::Placed,
            4 => RecordBody::ReindexArmed,
            5 => RecordBody::Reported(match r.u8()? {
                0 => ReportedStatus::Complete,
                1 => ReportedStatus::Mismatch,
                _ => return None,
            }),
            _ => return None,
        };
        (r.p == payload.len()).then_some(Self {
            at_ms,
            body,
        })
    }
}

pub(super) fn header_crc(len: [u8; 4], phase: u8) -> u32 {
    let mut b = [0u8; 5];
    b[..4].copy_from_slice(&len);
    b[4] = phase;
    masked_crc(&b)
}

/// Frames a payload (phase byte first) as it is stored on disk.
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let len = (payload.len() as u32).to_le_bytes();
    let mut rec = len.to_vec();
    rec.extend_from_slice(&header_crc(len, payload.first().copied().unwrap_or(0)).to_le_bytes());
    rec.extend_from_slice(payload);
    rec.extend_from_slice(&masked_crc(payload).to_le_bytes());
    rec
}

fn put_bytes(v: &mut Vec<u8>, x: &[u8]) {
    v.extend_from_slice(&(x.len() as u32).to_le_bytes());
    v.extend_from_slice(x);
}

pub(crate) struct Reader<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) p: usize,
}
impl<'a> Reader<'a> {
    pub(crate) fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let e = self.p.checked_add(n)?;
        let x = self.b.get(self.p..e)?;
        self.p = e;
        Some(x)
    }
    pub(crate) fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub(crate) fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub(crate) fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub(crate) fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    pub(crate) fn bytes_max(&mut self, max: usize) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        if n > max {
            return None;
        }
        self.take(n)
    }
}

/// What `repair` has to do to make the file match the decoded records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Repair {
    None,
    /// A torn final record: truncate to this length.
    Truncate(u64),
    /// Missing magic (empty, all zero, or a prefix of the magic): rewrite it.
    Rewrite,
}

enum Frame<'a> {
    Good(&'a [u8], usize),
    /// The file ends inside the header, or inside the payload after a valid
    /// header.
    Short,
    /// A frame that does not verify and whose head was lost: an all-zero
    /// header, or a valid header and a payload that is zero up to the next
    /// sector boundary or EOF.
    ZeroHead,
    Bad,
}

/// The fixed payload length of a phase, if it has one.
fn fixed_len(phase: u8) -> Option<usize> {
    match phase {
        2 => Some(1 + 8 + 32),
        3 | 4 => Some(1 + 8),
        5 => Some(1 + 8 + 1),
        _ => None,
    }
}

/// Whether `n` is a payload length the phase's decoder can accept.
fn len_ok(phase: u8, n: usize) -> bool {
    match (phase, fixed_len(phase)) {
        (_, Some(len)) => n == len,
        (0, None) => (1 + 8..=MAX_INTENT).contains(&n),
        (1, None) => (1 + 8 + 4..=MAX_STAGED).contains(&n),
        _ => false,
    }
}

/// The payload length and phase of a complete header whose crc verifies.
fn header(rest: &[u8]) -> Option<(usize, u8)> {
    let h = rest.get(..HEADER)?;
    let len: [u8; 4] = h[..4].try_into().expect("4 bytes");
    let crc = u32::from_le_bytes(h[4..8].try_into().expect("4 bytes"));
    (header_crc(len, h[8]) == crc).then_some((u32::from_le_bytes(len) as usize, h[8]))
}

/// The torn-tail scan hashes at most this many payload bytes per journal
/// byte. Each frame it reaches in a real journal is hashed once, so only
/// crafted overlapping headers can reach the cap.
pub(super) const SCAN_FACTOR: usize = 4;

/// Whether a complete, valid frame starts anywhere in `rest`, hashing at most
/// `budget` payload bytes; `None` when the budget runs out first. A header
/// costs a fixed 5-byte crc, so the scan is linear in `rest`.
fn any_valid_frame(rest: &[u8], mut budget: usize) -> Option<bool> {
    for o in 0..rest.len() {
        let r = &rest[o..];
        let Some((n, phase)) = header(r) else {
            continue;
        };
        if !len_ok(phase, n) || n + OVERHEAD > r.len() {
            continue;
        }
        budget = budget.checked_sub(n)?;
        let crc = u32::from_le_bytes(r[8 + n..n + OVERHEAD].try_into().expect("4 bytes"));
        if masked_crc(&r[8..8 + n]) == crc {
            return Some(true);
        }
    }
    Some(false)
}

/// Whether `raw` is zero from `from` up to the next [`SECTOR`]-aligned
/// offset, or to its end if that comes first.
fn zero_to_sector_end(raw: &[u8], from: usize) -> bool {
    let end = (from / SECTOR + 1) * SECTOR;
    raw[from..end.min(raw.len())].iter().all(|&b| b == 0)
}

/// The frame at offset `p` of `raw`; `expected` is the next phase's byte.
fn frame_at(raw: &[u8], p: usize, expected: Option<u8>) -> Frame<'_> {
    let rest = &raw[p..];
    if rest.len() < HEADER {
        return Frame::Short;
    }
    let Some((n, phase)) = header(rest) else {
        return if rest[..HEADER].iter().all(|&b| b == 0) {
            Frame::ZeroHead
        } else {
            Frame::Bad
        };
    };
    if Some(phase) != expected || !len_ok(phase, n) {
        return Frame::Bad;
    }
    let size = n + OVERHEAD;
    if size > rest.len() {
        return Frame::Short;
    }
    let payload = &rest[8..8 + n];
    let crc = u32::from_le_bytes(rest[8 + n..size].try_into().expect("4 bytes"));
    if masked_crc(payload) == crc {
        Frame::Good(payload, size)
    } else if zero_to_sector_end(raw, p + HEADER) {
        Frame::ZeroHead
    } else {
        Frame::Bad
    }
}

#[derive(Clone, Debug)]
pub struct Journal {
    pub path: PathBuf,
    exists: bool,
    records: Vec<Record>,
    repair: Repair,
    ignored_tail: bool,
}

impl Journal {
    /// Reads and checks the journal at `path`. Never writes: a missing file is
    /// an empty journal, and a torn tail is left for [`Journal::repair`].
    pub fn open(path: &Path, fs: &dyn ImportFs) -> Result<Self, ImportError> {
        match fs.metadata(path)? {
            Some(m) if m.is_symlink || m.is_dir => {
                Err(ImportError::DestinationSymlink(path.to_owned()))
            }
            None => Ok(Self {
                path: path.to_owned(),
                exists: false,
                records: Vec::new(),
                repair: Repair::None,
                ignored_tail: false,
            }),
            Some(_) => Self::decode(path, &fs.read(path)?),
        }
    }

    /// Decodes journal bytes; `path` names the journal in errors.
    pub fn decode(path: &Path, raw: &[u8]) -> Result<Self, ImportError> {
        let mut journal = Self {
            path: path.to_owned(),
            exists: true,
            records: Vec::new(),
            repair: Repair::None,
            ignored_tail: false,
        };
        if raw == MAGIC {
            return Ok(journal);
        }
        if raw.iter().all(|&b| b == 0) || (raw.len() < MAGIC.len() && MAGIC.starts_with(raw)) {
            journal.repair = Repair::Rewrite;
            return Ok(journal);
        }
        if !raw.starts_with(MAGIC) {
            return Err(journal_corrupt(path, None));
        }
        let mut p = MAGIC.len();
        while p < raw.len() {
            let last = journal.last();
            let reported = last == Some(Phase::Reported);
            let good = match frame_at(raw, p, next_phase(last).map(|p| p as u8)) {
                Frame::Good(payload, size) => match Record::decode(payload) {
                    Some(r) if Some(r.phase()) == next_phase(last) => Some((r, size)),
                    _ => None,
                },
                // Torn only when no complete, valid frame starts after it, so
                // a repair never removes a durable record. A scan that runs
                // out of budget refuses rather than truncates.
                Frame::Short | Frame::ZeroHead if !reported => {
                    match any_valid_frame(&raw[p + 1..], SCAN_FACTOR * raw.len()) {
                        Some(false) => {
                            journal.repair = Repair::Truncate(p as u64);
                            break;
                        }
                        Some(true) => None,
                        None => return Err(journal_corrupt(path, last)),
                    }
                }
                Frame::Short | Frame::ZeroHead | Frame::Bad => None,
            };
            match good {
                Some((r, size)) => {
                    journal.records.push(r);
                    p += size;
                }
                None if reported => {
                    eprintln!(
                        "warning: ignoring {} corrupt trailing bytes in the import journal at {}",
                        raw.len() - p,
                        path.display()
                    );
                    journal.ignored_tail = true;
                    break;
                }
                None => return Err(journal_corrupt(path, last)),
            }
        }
        Ok(journal)
    }

    /// Creates the journal file holding only the magic, durably: the file's
    /// data, its entry in `import-staging`, and `import-staging`'s entry in
    /// the destination network dir.
    pub fn create(path: &Path, fs: &dyn ImportFs) -> Result<Self, ImportError> {
        let staging = path.parent().expect("journal has a parent");
        fs.create_dir_all(staging)?;
        fs.append(path, MAGIC)?;
        fs.sync_data(path)?;
        fs.sync_dir(staging)?;
        if let Some(dest) = staging.parent() {
            fs.sync_dir(dest)?;
        }
        Ok(Self {
            path: path.to_owned(),
            exists: true,
            records: Vec::new(),
            repair: Repair::None,
            ignored_tail: false,
        })
    }

    pub fn exists(&self) -> bool {
        self.exists
    }
    pub fn records(&self) -> &[Record] {
        &self.records
    }
    pub fn phases(&self) -> Vec<Phase> {
        self.records.iter().map(Record::phase).collect()
    }
    pub fn last(&self) -> Option<Phase> {
        self.records.last().map(Record::phase)
    }
    /// Whether `open` found a torn tail (or a missing magic) to repair.
    pub fn needs_repair(&self) -> bool {
        self.repair != Repair::None
    }
    /// Whether corrupt bytes after `Reported` were ignored.
    pub fn ignored_tail(&self) -> bool {
        self.ignored_tail
    }
    pub fn intent(&self) -> Option<&Intent> {
        self.records.iter().find_map(|r| match &r.body {
            RecordBody::Intent(i) => Some(i.as_ref()),
            _ => None,
        })
    }
    pub fn methods(&self) -> Option<&[FileMethod]> {
        self.records.iter().find_map(|r| match &r.body {
            RecordBody::Staged(m) => Some(m.as_slice()),
            _ => None,
        })
    }
    pub fn m1_digest(&self) -> Option<[u8; 32]> {
        self.records.iter().find_map(|r| match r.body {
            RecordBody::SourceVerified {
                m1_digest,
            } => Some(m1_digest),
            _ => None,
        })
    }
    pub fn reported(&self) -> Option<ReportedStatus> {
        self.records.iter().find_map(|r| match r.body {
            RecordBody::Reported(s) => Some(s),
            _ => None,
        })
    }

    /// Removes a torn tail found by `open` (`ftruncate` + `fdatasync`). Only
    /// the resume path that owns the journal calls this.
    pub fn repair(&mut self, fs: &dyn ImportFs) -> Result<(), ImportError> {
        match self.repair {
            Repair::None => return Ok(()),
            Repair::Truncate(len) => fs.truncate(&self.path, len)?,
            Repair::Rewrite => {
                fs.truncate(&self.path, 0)?;
                fs.append(&self.path, MAGIC)?;
            }
        }
        fs.sync_data(&self.path)?;
        self.repair = Repair::None;
        Ok(())
    }

    /// Durably discards every record, keeping the file and its magic.
    pub fn reset(&mut self, fs: &dyn ImportFs) -> Result<(), ImportError> {
        fs.truncate(&self.path, MAGIC.len() as u64)?;
        fs.sync_data(&self.path)?;
        self.records.clear();
        self.repair = Repair::None;
        self.ignored_tail = false;
        Ok(())
    }

    /// Appends one record and `fdatasync`s it. The record's phase must be the
    /// next one, and a pending repair must have been done.
    pub fn append(&mut self, record: Record, fs: &dyn ImportFs) -> Result<(), ImportError> {
        let expected = next_phase(self.last());
        if Some(record.phase()) != expected || self.repair != Repair::None || !self.exists {
            return Err(ImportError::PhaseOrder {
                last: self.last(),
                got: record.phase(),
            });
        }
        fs.append(&self.path, &frame(&record.encode()))?;
        fs.sync_data(&self.path)?;
        self.records.push(record);
        Ok(())
    }
}

pub(crate) fn next_phase(last: Option<Phase>) -> Option<Phase> {
    match last {
        None => Some(Phase::Intent),
        Some(Phase::Intent) => Some(Phase::Staged),
        Some(Phase::Staged) => Some(Phase::SourceVerified),
        Some(Phase::SourceVerified) => Some(Phase::Placed),
        Some(Phase::Placed) => Some(Phase::ReindexArmed),
        Some(Phase::ReindexArmed) => Some(Phase::Reported),
        Some(Phase::Reported) => None,
    }
}
