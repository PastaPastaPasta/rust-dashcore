//! Reading dashd's `blocks/blk*.dat` files directly.
//!
//! Each file is a sequence of `[magic: 4][size: u32 LE][block: size bytes]` records, written
//! in download order (not height order) and zero-padded at the end. Files may be obfuscated
//! with the 8-byte key in `blocks/xor.dat` (Bitcoin Core 28+, `-blocksxor`): byte `i` of a
//! file is stored XORed with `key[i % 8]`. Dash Core v24 does not write `xor.dat`, but a
//! present, non-zero key is honored.
//!
//! Indexing reads only the 8-byte record headers and the 80-byte block headers. Every header
//! is hashed by *both* libraries, and [`Chain::build`] links a block to its parent through
//! the raw `prev_hash` bytes, accepting either library's hash of the parent. A wrong hash in
//! one library therefore cannot break the chain, and every best-chain block gets an expected
//! hash that neither library computed: its child's raw `prev_hash` field (or the oracle's
//! best block hash at the tip).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use dash_primitives::BlockHeader;
use dash_types::codec::BaseCodec;
use dash_types::{Hashable, Numeric};
use dashcore::hashes::Hash;
use rayon::prelude::*;

use crate::check::{H256, rev_hex};

/// Location and identity of one block record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    pub file: u32,
    /// Offset of the block bytes (after the 8-byte record header) within the file.
    pub offset: u64,
    pub len: u32,
    /// The header's `prev_hash` field, read from the raw bytes.
    pub prev: H256,
    /// X11 header hash according to dashcore (`None` if it could not decode the header).
    pub dc_hash: Option<H256>,
    /// X11 header hash according to base-sdk.
    pub bs_hash: Option<H256>,
}

impl Record {
    /// The distinct hashes the libraries computed for this header.
    pub fn hashes(&self) -> impl Iterator<Item = H256> {
        let bs = self.bs_hash.filter(|h| Some(*h) != self.dc_hash);
        self.dc_hash.into_iter().chain(bs)
    }

    /// Hash for display: dashcore's, else base-sdk's.
    pub fn any_hash(&self) -> Option<H256> {
        self.dc_hash.or(self.bs_hash)
    }
}

/// Problems found while scanning files: bytes that are neither records nor the stored-zero
/// tail (garbage, truncated or bogus records, short trailing bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanIssue {
    pub file: u32,
    pub offset: u64,
    pub reason: String,
    /// The discarded bytes run to the end of the file (in the newest file, possibly a record
    /// still being written).
    pub at_end: bool,
}

/// Reads `blocks/xor.dat`; `None` when absent or all zero.
pub fn xor_key(blocks_dir: &Path) -> Result<Option<[u8; 8]>> {
    let path = blocks_dir.join("xor.dat");
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let key: [u8; 8] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} has {} bytes, expected 8", path.display(), raw.len()))?;
    Ok((key != [0; 8]).then_some(key))
}

/// Paths of `blk?????.dat` files, by file number.
pub fn list_files(blocks_dir: &Path) -> Result<Vec<(u32, PathBuf)>> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(blocks_dir)
        .with_context(|| format!("listing {}", blocks_dir.display()))?
    {
        let e = e?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        if let Some(n) = name.strip_prefix("blk").and_then(|s| s.strip_suffix(".dat"))
            && let Ok(n) = n.parse::<u32>()
        {
            out.push((n, e.path()));
        }
    }
    out.sort();
    if out.is_empty() {
        bail!("no blk*.dat files in {}", blocks_dir.display());
    }
    Ok(out)
}

/// A file's generation: size, modification time and inode. A rewritten or replaced file
/// gets a new one, so checkpoints of its old contents are not reused.
pub fn generation(path: &Path) -> Result<[u64; 3]> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let mtime = (m.mtime() as u64).wrapping_mul(1_000_000_000).wrapping_add(m.mtime_nsec() as u64);
    Ok([m.len(), mtime, m.ino()])
}

/// Removes the obfuscation from `data`, which starts at file offset `pos`.
fn unxor(data: &mut [u8], pos: u64, key: Option<[u8; 8]>) {
    if let Some(key) = key {
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= key[((pos + i as u64) % 8) as usize];
        }
    }
}

/// Reads one indexed block and checks that it is still framed as indexed (magic, length and
/// `prev_hash` unchanged), so a file truncated or replaced since indexing is an error rather
/// than a silently different block.
pub fn read_block(
    file: &File,
    record: &Record,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<Vec<u8>> {
    read_record(file, record, key, magic, record.len as usize)
}

/// Reads one indexed block's 80-byte header, checked as [`read_block`] checks the block.
pub fn read_header(
    file: &File,
    record: &Record,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<[u8; 80]> {
    let data = read_record(file, record, key, magic, 80)?;
    Ok(data.try_into().expect("80 bytes"))
}

/// The first `want` bytes of `record`'s block, after checking its record header and `prev_hash`.
fn read_record(
    file: &File,
    record: &Record,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
    want: usize,
) -> Result<Vec<u8>> {
    let start = record.offset.checked_sub(8).context("record offset below its header")?;
    let mut data = vec![0u8; 8 + want];
    file.read_exact_at(&mut data, start).with_context(|| {
        format!("blk{:05}.dat@{}: reading {} bytes", record.file, start, data.len())
    })?;
    unxor(&mut data, start, key);
    let len = u32::from_le_bytes(data[4..8].try_into().expect("4 bytes"));
    if data[..4] != magic || len != record.len || data[12..44] != record.prev {
        bail!("blk{:05}.dat@{start}: the record changed since it was indexed", record.file);
    }
    data.drain(..8);
    Ok(data)
}

fn header_record(file: u32, offset: u64, len: u32, header: &[u8]) -> Record {
    let dc_hash = dashcore::consensus::deserialize::<dashcore::block::Header>(header)
        .ok()
        .map(|h| h.block_hash().to_byte_array());
    let bs_hash = BlockHeader::decode(&mut &header[..]).ok().map(|h| h.hash().to_lendian());
    Record {
        file,
        offset,
        len,
        prev: header[4..36].try_into().expect("80-byte header"),
        dc_hash,
        bs_hash,
    }
}

/// Finds the block records in one file's (de-obfuscated) bytes. `raw_zero(pos)` says whether
/// the *stored* bytes from `pos` on are all zero (the pre-allocated tail). Every other byte
/// that is not part of a record is reported.
fn scan_bytes(
    file: u32,
    data: &[u8],
    magic: [u8; 4],
    raw_zero: impl Fn(usize) -> bool,
) -> (Vec<Record>, Vec<ScanIssue>) {
    let mut records = Vec::new();
    let mut issues = Vec::new();
    let mut pos = 0usize;
    while pos < data.len() && !raw_zero(pos) {
        let reason = if pos + 8 > data.len() {
            format!("{} trailing bytes, too short for a record", data.len() - pos)
        } else {
            let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().expect("4 bytes"));
            let end = pos + 8 + len as usize;
            if data[pos..pos + 4] != magic {
                "no magic".to_string()
            } else if (len as usize) < 80 {
                format!("bogus record of {len} bytes")
            } else if end > data.len() {
                format!("record of {len} bytes runs past the end of the file")
            } else {
                records.push(header_record(file, pos as u64 + 8, len, &data[pos + 8..pos + 88]));
                pos = end;
                continue;
            }
        };
        // Like dashd's -reindex, resynchronise on the next magic.
        let next = data.get(pos + 1..).and_then(|d| d.windows(4).position(|w| w == magic));
        let skip = next.map_or(data.len() - pos, |n| n + 1);
        issues.push(ScanIssue {
            file,
            offset: pos as u64,
            reason: format!("{reason}: {skip} bytes skipped"),
            at_end: next.is_none(),
        });
        pos += skip;
    }
    (records, issues)
}

/// Indexes one file reading only record and block headers. Falls back to reading the rest of
/// the file when it finds anything else than a well-formed record or the zero tail.
fn scan_file(
    file: u32,
    path: &Path,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<(Vec<Record>, Vec<ScanIssue>)> {
    let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let size = f.metadata()?.len();
    let mut records = Vec::new();
    let mut pos = 0u64;
    let mut buf = [0u8; 88];
    while pos < size {
        let head = (size - pos).min(8) as usize;
        f.read_exact_at(&mut buf[..head], pos)?;
        // Zeros are the pre-allocated tail (stored zeros, even when obfuscated), unless
        // something else follows them (a hole after a crash).
        if buf[..head].iter().all(|b| *b == 0) {
            let mut rest = vec![0u8; (size - pos) as usize];
            f.read_exact_at(&mut rest, pos)?;
            if rest.iter().all(|b| *b == 0) {
                break;
            }
        }
        unxor(&mut buf[..head], pos, key);
        let len = if head == 8 {
            u32::from_le_bytes(buf[4..8].try_into().expect("4 bytes"))
        } else {
            0
        };
        let end = pos + 8 + u64::from(len);
        if head < 8 || buf[..4] != magic || len < 80 || end > size {
            // Let the byte-wise scanner resynchronise and report what it skips.
            let mut data = vec![0u8; (size - pos) as usize];
            f.read_exact_at(&mut data, pos)?;
            // The stored bytes from `p` on are all zero (the preallocated tail) iff `p` is past
            // the last nonzero stored byte; computed on the raw bytes before unxor.
            let zero_from = data.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
            unxor(&mut data, pos, key);
            let (mut tail, mut issues) = scan_bytes(file, &data, magic, |p| p >= zero_from);
            tail.iter_mut().for_each(|r| r.offset += pos);
            issues.iter_mut().for_each(|i| i.offset += pos);
            records.extend(tail);
            return Ok((records, issues));
        }
        f.read_exact_at(&mut buf[8..88], pos + 8)?;
        unxor(&mut buf[8..88], pos + 8, key);
        records.push(header_record(file, pos + 8, len, &buf[8..88]));
        pos = end;
    }
    Ok((records, Vec::new()))
}

/// Indexes every file in parallel.
pub fn index(
    files: &[(u32, PathBuf)],
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<(Vec<Record>, Vec<ScanIssue>)> {
    let per_file: Vec<(Vec<Record>, Vec<ScanIssue>)> =
        files.par_iter().map(|(n, path)| scan_file(*n, path, key, magic)).collect::<Result<_>>()?;
    let mut records = Vec::new();
    let mut issues = Vec::new();
    for (r, i) in per_file {
        records.extend(r);
        issues.extend(i);
    }
    Ok((records, issues))
}

/// Heights and best-chain membership derived from the records.
#[derive(Debug)]
pub struct Chain {
    /// `best[h]` is the index (into the record list) of the best-chain block at height `h`.
    pub best: Vec<usize>,
    /// Height of every record reachable from genesis (`None` for orphans).
    pub height: Vec<Option<u32>>,
    /// Records whose hash duplicates an earlier record (the same block stored twice).
    pub duplicates: usize,
    /// `false` for those duplicate copies.
    pub primary: Vec<bool>,
}

impl Chain {
    /// Links records starting from `genesis` (or, if `None`, from the last record whose
    /// `prev` is all zeros). The best chain ends at `anchor` when given (e.g. the oracle's
    /// best block hash), otherwise at the highest block, ties broken by record order. Without
    /// an anchor this can differ from dashd only when a shorter fork carries more work.
    pub fn build(
        records: &[Record],
        anchor: Option<&H256>,
        genesis: Option<&H256>,
    ) -> Result<Self> {
        let mut by_hash: HashMap<H256, usize> = HashMap::with_capacity(records.len());
        let mut duplicates = 0;
        let mut primary = vec![true; records.len()];
        for (i, r) in records.iter().enumerate() {
            let mut dup = false;
            for h in r.hashes() {
                match by_hash.entry(h) {
                    Entry::Occupied(_) => dup = true,
                    Entry::Vacant(e) => {
                        e.insert(i);
                    }
                }
            }
            if dup {
                duplicates += 1;
                primary[i] = false;
            }
        }
        let mut children: HashMap<H256, Vec<usize>> = HashMap::with_capacity(records.len());
        let mut root = None;
        for (i, r) in records.iter().enumerate() {
            if !primary[i] {
                continue;
            }
            if r.prev == [0; 32] {
                if genesis.is_none_or(|g| r.hashes().any(|h| h == *g)) {
                    root = Some(i);
                }
            } else {
                children.entry(r.prev).or_default().push(i);
            }
        }
        let Some(root) = root else {
            match genesis {
                Some(g) => bail!("genesis block {} not found in blk files", rev_hex(g)),
                None => bail!("no genesis block (prev = 0) in blk files"),
            }
        };
        let mut height = vec![None; records.len()];
        height[root] = Some(0);
        let mut stack = vec![root];
        let mut tip = root;
        while let Some(i) = stack.pop() {
            let h = height[i].expect("set before push");
            let tip_h = height[tip].expect("set");
            if h > tip_h || (h == tip_h && i < tip) {
                tip = i;
            }
            for parent_hash in records[i].hashes() {
                for &k in children.get(&parent_hash).map(Vec::as_slice).unwrap_or_default() {
                    if height[k].is_none() {
                        height[k] = Some(h + 1);
                        stack.push(k);
                    }
                }
            }
        }
        if let Some(a) = anchor {
            match by_hash.get(a) {
                Some(&i) if height[i].is_some() => tip = i,
                _ => bail!("anchor block {} is not linked to genesis in blk files", rev_hex(a)),
            }
        }
        let mut best = vec![0; height[tip].expect("set") as usize + 1];
        let mut cur = tip;
        loop {
            best[height[cur].expect("set") as usize] = cur;
            if cur == root {
                break;
            }
            cur = by_hash[&records[cur].prev];
        }
        Ok(Self {
            best,
            height,
            duplicates,
            primary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: [u8; 4] = [0xce, 0xe2, 0xca, 0xff];

    fn header(prev: H256, nonce: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&1i32.to_le_bytes());
        h.extend_from_slice(&prev);
        h.extend_from_slice(&[0; 32]);
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&nonce.to_le_bytes());
        h
    }

    fn record(out: &mut Vec<u8>, block: &[u8]) {
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&(block.len() as u32).to_le_bytes());
        out.extend_from_slice(block);
    }

    fn hash(h: &[u8]) -> H256 {
        BlockHeader::decode(&mut &h[..]).unwrap().hash().to_lendian()
    }

    #[test]
    fn scan_xor_and_chain() {
        // genesis <- a <- b, plus a stale sibling of b; written out of order, with junk.
        let g = header([0; 32], 0);
        let a = header(hash(&g), 1);
        let b = header(hash(&a), 2);
        let stale = header(hash(&a), 3);
        let mut file = Vec::new();
        record(&mut file, &b);
        record(&mut file, &g);
        file.extend_from_slice(&[1, 2, 3]);
        record(&mut file, &stale);
        record(&mut file, &a);
        let content_len = file.len();
        file.extend_from_slice(&[0; 64]);

        let dir = tempfile::tempdir().unwrap();
        let key = [0x5a, 1, 2, 3, 4, 5, 6, 0xff];
        let mut stored: Vec<u8> = file.iter().enumerate().map(|(i, b)| b ^ key[i % 8]).collect();
        // dashd pre-allocates with zeros that are not obfuscated
        stored[content_len..].fill(0);
        std::fs::write(dir.path().join("blk00000.dat"), stored).unwrap();
        std::fs::write(dir.path().join("xor.dat"), key).unwrap();

        let key = xor_key(dir.path()).unwrap();
        assert!(key.is_some());
        let files = list_files(dir.path()).unwrap();
        let (records, issues) = index(&files, key, MAGIC).unwrap();
        assert_eq!(records.len(), 4, "{issues:?}");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(records.iter().all(|r| r.dc_hash.is_some() && r.dc_hash == r.bs_hash));
        let chain = Chain::build(&records, None, Some(&hash(&g))).unwrap();
        assert_eq!(chain.best.len(), 3);
        assert_eq!(records[chain.best[0]].any_hash(), Some(hash(&g)));
        assert_eq!(records[chain.best[1]].any_hash(), Some(hash(&a)));
        // b and the stale sibling both sit at height 2; the first in record order wins
        assert_eq!(records[chain.best[2]].any_hash(), Some(hash(&b)));
        assert_eq!(chain.height.iter().filter(|h| **h == Some(2)).count(), 2);
        let anchored = Chain::build(&records, Some(&hash(&stale)), None).unwrap();
        assert_eq!(records[anchored.best[2]].any_hash(), Some(hash(&stale)));
        assert!(Chain::build(&records, None, Some(&[7; 32])).is_err());
    }

    fn scan(content: &[u8]) -> (Vec<Record>, Vec<ScanIssue>) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blk00000.dat"), content).unwrap();
        index(&list_files(dir.path()).unwrap(), None, MAGIC).unwrap()
    }

    #[test]
    fn every_discarded_byte_range_is_reported() {
        // Review r1 finding 5: garbage after the last record, with no later magic.
        let g = header([0; 32], 0);
        let mut file = Vec::new();
        record(&mut file, &g);
        let clean = file.clone();
        file.extend_from_slice(&[1; 20]);
        let (records, issues) = scan(&file);
        assert_eq!(records.len(), 1);
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].at_end && issues[0].offset == 88, "{issues:?}");
        // Fewer than 8 trailing bytes.
        let mut short = clean.clone();
        short.extend_from_slice(&[0, 0, 7]);
        let (records, issues) = scan(&short);
        assert_eq!((records.len(), issues.len()), (1, 1), "{issues:?}");
        // A truncated last record, a bogus length, and records after it are still found.
        let mut trunc = clean.clone();
        trunc.extend_from_slice(&MAGIC);
        trunc.extend_from_slice(&200u32.to_le_bytes());
        trunc.extend_from_slice(&g[..40]);
        let (_, issues) = scan(&trunc);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].reason.contains("runs past the end") && issues[0].at_end);
        let mut bogus = clean.clone();
        bogus.extend_from_slice(&MAGIC);
        bogus.extend_from_slice(&10u32.to_le_bytes());
        record(&mut bogus, &header(hash(&g), 1));
        let (records, issues) = scan(&bogus);
        assert_eq!((records.len(), issues.len()), (2, 1), "{issues:?}");
        assert!(!issues[0].at_end);
        // The stored-zero tail is not an issue.
        let mut padded = clean;
        padded.extend_from_slice(&[0; 3]);
        assert_eq!(scan(&padded).1, vec![]);
    }

    #[test]
    fn read_block_rejects_changed_files() {
        let g = header([0; 32], 0);
        let mut file = Vec::new();
        record(&mut file, &g);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blk00000.dat");
        std::fs::write(&path, &file).unwrap();
        let (records, _) = index(&list_files(dir.path()).unwrap(), None, MAGIC).unwrap();
        let f = File::open(&path).unwrap();
        assert_eq!(read_block(&f, &records[0], None, MAGIC).unwrap(), g);
        // Replaced with other contents of the same size, then truncated: errors, no panic.
        let mut other = Vec::new();
        record(&mut other, &header([5; 32], 0));
        std::fs::write(&path, &other).unwrap();
        assert!(read_block(&File::open(&path).unwrap(), &records[0], None, MAGIC).is_err());
        std::fs::write(&path, &file[..50]).unwrap();
        assert!(read_block(&File::open(&path).unwrap(), &records[0], None, MAGIC).is_err());
    }

    #[test]
    fn chain_links_through_either_library_hash() {
        // If one library hashes `a` wrongly, its child still links through the other hash.
        let g = header([0; 32], 0);
        let a = header(hash(&g), 1);
        let b = header(hash(&a), 2);
        let mut records: Vec<Record> =
            [&g, &a, &b].iter().map(|h| header_record(0, 0, 80, h)).collect();
        records[1].bs_hash = Some([9; 32]);
        let chain = Chain::build(&records, None, None).unwrap();
        assert_eq!(chain.best, vec![0, 1, 2]);
        records[1].dc_hash = Some([8; 32]);
        let chain = Chain::build(&records, None, None).unwrap();
        assert_eq!(chain.best, vec![0, 1], "both wrong: b becomes an orphan");
        assert_eq!(chain.height[2], None);
    }
}
