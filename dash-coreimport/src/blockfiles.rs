//! Core's `blk?????.dat` / `rev?????.dat` files (`node/blockstorage.cpp`).
//!
//! Each file is a sequence of `magic ‖ u32 LE size ‖ payload` records followed by a zero-filled
//! preallocated tail. A rev payload is a `CBlockUndo`, and the record continues with
//! `SHA256d(prev_block_hash ‖ undo bytes)`. blk files are in download order, rev files in
//! connection order; a block's undo goes to the rev file with the block's own file number.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::ops::RangeInclusive;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::tx::block_hash;
use crate::{Hash256, hash_to_hex};

pub mod external;

/// Length of the checksum that follows every undo record.
pub const UNDO_CHECKSUM_LEN: usize = 32;

/// The `blk` or `rev` files of `dir`, by file number.
pub fn list_files(dir: &Path, prefix: &str) -> Result<Vec<(u32, PathBuf)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
        let entry = entry.map_err(|e| Error::io(dir, e))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(n) = name.strip_prefix(prefix).and_then(|s| s.strip_suffix(".dat"))
            && n.len() == 5
            && let Ok(n) = n.parse::<u32>()
        {
            out.push((n, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// A record found in a file: payload offset and length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub offset: usize,
    pub len: usize,
}

/// What a scan skipped.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanStats {
    /// Bytes before the zero tail that no record covers (garbage between records).
    pub resync_bytes: usize,
    /// A record at the end whose payload runs past the end of the file (a torn write).
    pub torn_tail: bool,
}

/// Payload sizes of block records Core's scan accepts: a header up to `MaxBlockSize()`.
// Core: consensus/consensus.h MAX_DIP0001_BLOCK_SIZE
pub const BLOCK_SIZES: RangeInclusive<usize> = 80..=2_000_000;
/// Undo records are not scanned by Core; any non-empty size is accepted.
pub const UNDO_SIZES: RangeInclusive<usize> = 1..=u32::MAX as usize;

fn find_magic(data: &[u8], from: usize, magic: [u8; 4]) -> Option<usize> {
    data.get(from..)?.windows(4).position(|w| w == magic).map(|n| from + n)
}

fn record_len(data: &[u8], pos: usize) -> usize {
    u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4 bytes")) as usize
}

/// Finds the block records of a blk file as Core's `-reindex` does.
///
/// At each position Core looks for the magic followed by a size in [`BLOCK_SIZES`]; on anything
/// else it resumes one byte further. A record found is skipped as a whole (the next search starts
/// at its claimed end, even if the block inside does not deserialize). A record whose claimed end
/// lies past the end of the file ends the scan: Core's buffered stream hits the end of the file
/// while skipping it, and `eof()` then stops the loop; nothing after that magic is loaded. The
/// zero-filled preallocated tail ends the scan the same way.
///
/// One difference remains, on records Core never writes: when Core loads a block during the scan
/// (its parent is known and its data is not), it resumes at the end of the deserialized block
/// rather than at the claimed end. A record whose claimed size exceeds its block's serialization
/// therefore has its slack scanned by Core (a record hidden there is loaded) and skipped here.
/// Matching that needs Core's runtime state (which parents are known at that point); parity on
/// corrupted inputs is ST2's.
// Core: validation.cpp LoadExternalBlockFile at v23.1.8 and v24.0.0-rc.3 (CBufferedFile::eof)
pub fn scan_block_records(data: &[u8], magic: [u8; 4]) -> (Vec<Record>, ScanStats) {
    let mut records = Vec::new();
    let mut stats = ScanStats::default();
    let mut pos = 0usize;
    while let Some(found) = find_magic(data, pos, magic) {
        if found + 8 > data.len() {
            stats.resync_bytes += found - pos;
            pos = found;
            break;
        }
        let len = record_len(data, found);
        if !BLOCK_SIZES.contains(&len) {
            stats.resync_bytes += found + 1 - pos;
            pos = found + 1;
            continue;
        }
        stats.resync_bytes += found - pos;
        let end = found + 8 + len;
        if end > data.len() {
            stats.torn_tail = true;
            return (records, stats);
        }
        records.push(Record {
            offset: found + 8,
            len,
        });
        pos = end;
    }
    stats.resync_bytes += data[pos..].iter().rposition(|&b| b != 0).map_or(0, |n| n + 1);
    (records, stats)
}

/// Finds every candidate undo record of a rev file: every magic followed by a size whose payload
/// and checksum fit in the file. Core never scans rev files, so this is the reader's own rule,
/// chosen so that nothing can hide a valid record: candidates may overlap (a stale header left by
/// an overwritten record, or a magic inside undo bytes), and each is only accepted later if its
/// checksum matches ([`RevCursors::take`]). Candidates are in file order.
pub fn scan_undo_candidates(data: &[u8], magic: [u8; 4]) -> (Vec<Record>, ScanStats) {
    let mut records = Vec::new();
    let mut stats = ScanStats::default();
    // End of the bytes covered by a candidate so far.
    let mut covered = 0usize;
    let mut pos = 0usize;
    while let Some(found) = find_magic(data, pos, magic) {
        pos = found + 1;
        if found + 8 > data.len() {
            continue;
        }
        let len = record_len(data, found);
        if !UNDO_SIZES.contains(&len) {
            continue;
        }
        let end = found + 8 + len + UNDO_CHECKSUM_LEN;
        if end > data.len() {
            if found >= covered {
                stats.torn_tail = true;
            }
            continue;
        }
        if found >= covered {
            stats.resync_bytes += found - covered;
            stats.torn_tail = false;
        }
        covered = covered.max(end);
        records.push(Record {
            offset: found + 8,
            len,
        });
    }
    let tail = &data[covered..];
    stats.resync_bytes += tail.iter().rposition(|&b| b != 0).map_or(0, |n| n + 1);
    (records, stats)
}

/// Where a block is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockLoc {
    pub file: u32,
    pub offset: u64,
    pub len: u32,
    pub prev: Hash256,
}

/// Every block in a blocks directory, by hash.
pub struct HeaderIndex {
    /// The first copy of each block.
    pub blocks: HashMap<Hash256, BlockLoc>,
    /// Further copies of blocks stored more than once (each has its own undo record, in the rev
    /// file of its own blk file, if it was connected from that copy).
    pub duplicates: HashMap<Hash256, Vec<BlockLoc>>,
    pub files: Vec<(u32, PathBuf)>,
    pub scan: ScanStats,
}

impl HeaderIndex {
    /// Scans every blk file of `dir` (reading each file once) and hashes every header.
    pub fn build(dir: &Path, magic: [u8; 4]) -> Result<Self> {
        let files = list_files(dir, "blk")?;
        if files.is_empty() {
            return Err(Error::Inconsistent(format!("no blk files in {}", dir.display())));
        }
        let mut blocks = HashMap::new();
        let mut duplicates: HashMap<Hash256, Vec<BlockLoc>> = HashMap::new();
        let mut scan = ScanStats::default();
        for (n, path) in &files {
            let data = std::fs::read(path).map_err(|e| Error::io(path, e))?;
            let (records, stats) = scan_block_records(&data, magic);
            scan.resync_bytes += stats.resync_bytes;
            scan.torn_tail |= stats.torn_tail;
            for r in records {
                let header = &data[r.offset..r.offset + 80];
                let loc = BlockLoc {
                    file: *n,
                    offset: r.offset as u64,
                    len: r.len as u32,
                    prev: header[4..36].try_into().expect("32 bytes"),
                };
                match blocks.entry(block_hash(header)) {
                    Entry::Vacant(v) => {
                        v.insert(loc);
                    }
                    Entry::Occupied(o) => duplicates.entry(*o.key()).or_default().push(loc),
                }
            }
        }
        Ok(Self {
            blocks,
            duplicates,
            files,
            scan,
        })
    }

    /// Every stored copy of a block: the first one, then any duplicates.
    pub fn copies<'a>(&'a self, hash: &Hash256) -> impl Iterator<Item = &'a BlockLoc> + 'a {
        let dups = self.duplicates.get(hash).map(Vec::as_slice).unwrap_or_default();
        self.blocks.get(hash).into_iter().chain(dups)
    }

    /// The chain from genesis to `anchor` (index = height), following `prev` links.
    pub fn chain_to(&self, anchor: &Hash256) -> Result<Vec<(Hash256, BlockLoc)>> {
        let mut chain = Vec::new();
        let mut cur = *anchor;
        loop {
            let loc = self.blocks.get(&cur).ok_or_else(|| {
                Error::Inconsistent(format!(
                    "block {} (height {} below the anchor) is not in the blk files",
                    hash_to_hex(&cur),
                    chain.len()
                ))
            })?;
            chain.push((cur, *loc));
            if loc.prev == [0; 32] {
                break;
            }
            cur = loc.prev;
        }
        chain.reverse();
        Ok(chain)
    }
}

/// Reads blocks by location with `pread`, keeping one handle per file.
pub struct BlockReader {
    paths: HashMap<u32, PathBuf>,
    open: HashMap<u32, File>,
}

impl BlockReader {
    pub fn new(files: &[(u32, PathBuf)]) -> Self {
        Self {
            paths: files.iter().cloned().collect(),
            open: HashMap::new(),
        }
    }

    pub fn read(&mut self, loc: &BlockLoc, buf: &mut Vec<u8>) -> Result<()> {
        let path = self.paths.get(&loc.file).ok_or_else(|| {
            Error::Inconsistent(format!("blk{:05}.dat is not in the file list", loc.file))
        })?;
        let file = match self.open.entry(loc.file) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(v) => v.insert(File::open(path).map_err(|e| Error::io(path, e))?),
        };
        buf.resize(loc.len as usize, 0);
        file.read_exact_at(buf, loc.offset).map_err(|e| Error::io(path, e))
    }
}

/// The undo checksum Core writes after each record: `SHA256d(prev_block_hash ‖ undo)`.
pub use dashd_store::flatfile::undo_checksum;

struct RevFile {
    data: Vec<u8>,
    records: Vec<Record>,
    /// Records before this index are consumed or skipped.
    cursor: usize,
}

/// Forward cursors over the rev files, one per file number, with a small cache of loaded files.
pub struct RevCursors {
    dir: PathBuf,
    magic: [u8; 4],
    loaded: HashMap<u32, RevFile>,
    lru: VecDeque<u32>,
    /// Candidates and cursors of files evicted from the cache: a reload rereads the bytes but
    /// neither rescans them nor counts their resync bytes again.
    parked: HashMap<u32, (Vec<Record>, usize)>,
    capacity: usize,
    pub resync_bytes: usize,
}

impl RevCursors {
    pub fn new(dir: &Path, magic: [u8; 4], capacity: usize) -> Self {
        Self {
            dir: dir.to_path_buf(),
            magic,
            loaded: HashMap::new(),
            lru: VecDeque::new(),
            parked: HashMap::new(),
            capacity: capacity.max(1),
            resync_bytes: 0,
        }
    }

    fn file(&mut self, n: u32) -> Result<&mut RevFile> {
        if !self.loaded.contains_key(&n) {
            if self.loaded.len() >= self.capacity
                && let Some(old) = self.lru.pop_front()
                && let Some(f) = self.loaded.remove(&old)
            {
                self.parked.insert(old, (f.records, f.cursor));
            }
            let path = self.dir.join(format!("rev{n:05}.dat"));
            let data = std::fs::read(&path).map_err(|e| Error::io(&path, e))?;
            let (records, cursor) = match self.parked.remove(&n) {
                Some(parked) => parked,
                None => {
                    let (records, stats) = scan_undo_candidates(&data, self.magic);
                    self.resync_bytes += stats.resync_bytes;
                    (records, 0)
                }
            };
            self.loaded.insert(
                n,
                RevFile {
                    data,
                    records,
                    cursor,
                },
            );
        }
        self.lru.retain(|&f| f != n);
        self.lru.push_back(n);
        Ok(self.loaded.get_mut(&n).expect("just loaded"))
    }

    /// Takes the first record at or after the cursor of rev file `n` for which `accept` returns
    /// true (it is given the undo bytes, whose checksum against `prev_block_hash` already
    /// matched), moving the cursor past it. Returns the number of records skipped, or `None` if no
    /// record in the rest of the file is accepted (the cursor does not move).
    pub fn take(
        &mut self,
        n: u32,
        prev_block_hash: &Hash256,
        mut accept: impl FnMut(&[u8]) -> bool,
    ) -> Result<Option<usize>> {
        let file = self.file(n)?;
        for i in file.cursor..file.records.len() {
            let r = file.records[i];
            let undo = &file.data[r.offset..r.offset + r.len];
            let stored = &file.data[r.offset + r.len..r.offset + r.len + UNDO_CHECKSUM_LEN];
            if undo_checksum(prev_block_hash, undo) == stored && accept(undo) {
                let skipped = i - file.cursor;
                file.cursor = i + 1;
                return Ok(Some(skipped));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];

    fn record(payload: &[u8], trailer: usize) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend((payload.len() as u32).to_le_bytes());
        out.extend(payload);
        out.extend(vec![0xee; trailer]);
        out
    }

    fn header(len: u32) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(len.to_le_bytes());
        out
    }

    fn block(fill: u8) -> Vec<u8> {
        record(&[fill; 80], 0)
    }

    #[test]
    fn block_scan_handles_garbage_and_zero_tails() {
        let mut data = block(1);
        // A magic followed by an out-of-range size is garbage: resume one byte after the magic.
        data.extend(header(79));
        data.extend([9, 9, 9]); // garbage
        let second = data.len() + 8;
        data.extend(block(2));
        data.extend(vec![0; 64]); // preallocated tail
        let (records, stats) = scan_block_records(&data, MAGIC);
        let offsets: Vec<_> = records.iter().map(|r| (r.offset, r.len)).collect();
        assert_eq!(offsets, [(8, 80), (second, 80)]);
        assert_eq!(
            stats,
            ScanStats {
                resync_bytes: 11,
                torn_tail: false
            }
        );
    }

    /// Core stops scanning a blk file at a record that runs past its end, even when a valid
    /// record follows the bogus magic (the stream reaches the end of the file while skipping the
    /// claimed span, and `eof()` ends the loop).
    // Core: validation.cpp LoadExternalBlockFile (v23.1.8, v24.0.0-rc.3)
    #[test]
    fn block_scan_stops_at_a_record_past_the_end_like_core() {
        let mut data = block(1);
        data.extend(header(1000)); // in range, runs past the end of the file
        data.extend(block(2));
        let (records, stats) = scan_block_records(&data, MAGIC);
        assert_eq!(records.len(), 1, "the block after the bogus header is not loaded");
        assert!(stats.torn_tail);

        // A torn last record.
        let mut torn = block(1);
        torn.extend(&block(2)[..50]);
        let (records, stats) = scan_block_records(&torn, MAGIC);
        assert_eq!(records.len(), 1);
        assert!(stats.torn_tail);

        // A record ending exactly at the end of the file is complete.
        let (records, stats) = scan_block_records(&block(1), MAGIC);
        assert_eq!((records.len(), stats.torn_tail), (1, false));
    }

    /// The known difference from Core (see `scan_block_records`): a record claiming more bytes
    /// than its block has a slack that Core scans after loading the block, and this scanner
    /// skips. Here the slack hides a whole record.
    #[test]
    fn block_scan_skips_the_claimed_span_even_with_a_record_in_its_slack() {
        let hidden = block(2);
        let mut data = header(80 + hidden.len() as u32);
        data.extend([1; 80]);
        data.extend(&hidden);
        data.extend(block(3));
        let (records, _) = scan_block_records(&data, MAGIC);
        let lens: Vec<_> = records.iter().map(|r| r.len).collect();
        assert_eq!(lens, [80 + hidden.len(), 80], "the record in the slack is not found");
    }

    #[test]
    fn undo_scan_finds_records_a_stale_header_covers() {
        // An old record's header whose claimed span covers newer records, then two records.
        let mut rev = header(100);
        let first = rev.len() + 8;
        rev.extend(record(&[7; 5], UNDO_CHECKSUM_LEN));
        let second = rev.len() + 8;
        rev.extend(record(&[8; 5], UNDO_CHECKSUM_LEN));
        rev.extend(vec![0; 200]);
        let (records, stats) = scan_undo_candidates(&rev, MAGIC);
        let offsets: Vec<_> = records.iter().map(|r| (r.offset, r.len)).collect();
        assert_eq!(offsets, [(8, 100), (first, 5), (second, 5)]);
        assert_eq!(stats.resync_bytes, 0);

        let one = record(&[7; 5], UNDO_CHECKSUM_LEN);
        let (records, stats) = scan_undo_candidates(&one[..one.len() - 1], MAGIC);
        assert!(records.is_empty() && stats.torn_tail);
    }

    #[test]
    fn rev_cursor_skips_wrong_records_and_moves_forward() {
        let dir = tempfile::tempdir().unwrap();
        let prev = [3u8; 32];
        let mut file = Vec::new();
        for undo in [&[1u8][..], &[2], &[3]] {
            let mut rec = MAGIC.to_vec();
            rec.extend((undo.len() as u32).to_le_bytes());
            rec.extend(undo);
            rec.extend(undo_checksum(&prev, undo));
            file.extend(rec);
        }
        std::fs::write(dir.path().join("rev00007.dat"), &file).unwrap();
        let mut revs = RevCursors::new(dir.path(), MAGIC, 1);
        assert_eq!(revs.take(7, &prev, |u| u == [2]).unwrap(), Some(1));
        // The cursor is past record 2: record 1 is no longer reachable.
        assert_eq!(revs.take(7, &prev, |u| u == [1]).unwrap(), None);
        assert_eq!(revs.take(7, &[0; 32], |_| true).unwrap(), None, "checksum binds prev hash");
        assert_eq!(revs.take(7, &prev, |_| true).unwrap(), Some(0));
    }

    #[test]
    fn rev_cursors_survive_eviction() {
        let dir = tempfile::tempdir().unwrap();
        let prev = [5u8; 32];
        for n in [1u32, 2] {
            let mut file = Vec::new();
            for undo in [[n as u8, 1], [n as u8, 2]] {
                file.extend(MAGIC);
                file.extend(2u32.to_le_bytes());
                file.extend(undo);
                file.extend(undo_checksum(&prev, &undo));
            }
            std::fs::write(dir.path().join(format!("rev{n:05}.dat")), file).unwrap();
        }
        // One file cached at a time: each take on the other file evicts this one.
        let mut revs = RevCursors::new(dir.path(), MAGIC, 1);
        let mut seen = Vec::new();
        for n in [1u32, 2, 1, 2] {
            revs.take(n, &prev, |u| {
                seen.push(u.to_vec());
                true
            })
            .unwrap()
            .unwrap();
        }
        assert_eq!(seen, [vec![1, 1], vec![2, 1], vec![1, 2], vec![2, 2]]);
    }
}
