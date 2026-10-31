//! The best chain of a Core `blocks/` directory, as a block source (CH3a a7-2, D150): L2's input
//! is a frozen datadir's block files, not a corpus `blocks.dat`.
//!
//! - **Index:** [`blkfile::index`] reads every `blk?????.dat` file's record and block headers
//!   only, and [`Chain::build`] links them from the chain's genesis to the highest block, or to an
//!   anchor hash. Only the best chain's records are kept, so the source's memory is bounded by the
//!   index, never by the files.
//! - **"Best" is the highest chain**, a tie going to the earliest record, not Core's most-work
//!   chain (D150): a long low-difficulty side branch would win. A golden compare, or the anchor,
//!   keeps a run on Core's chain.
//! - **Refusals:** a byte that is neither a record nor the zero tail, except a record still being
//!   written at the end of the newest file (what `-reindex` skips), is refused: the copy is meant
//!   to be frozen.
//! - **Hashes:** each best-chain block's expected hash is its child's raw `prev_hash` field (the
//!   anchor at the tip, else the tip's hash both libraries agree on, as [`blkfile`] computes it).
//!   A header or block read back must hash to it.
//! - **Reads:** [`BlockDir::headers`] and [`BlockDir::blocks`] read one header or block at a time,
//!   in height order, through [`blkfile::read_header`] and [`blkfile::read_block`], which refuse a
//!   record changed since it was indexed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, bail, ensure};
use dash_consensus::Block;
use dash_consensus::primitives::{Header, decode_block, decode_header};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

use crate::blkfile::{self, Chain, Record};
use crate::check::{H256, rev_hex};

/// A `blocks/` directory's best chain, indexed.
pub struct BlockDir {
    dir: PathBuf,
    files: BTreeMap<u32, PathBuf>,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
    /// `best[h]` is the record of the best-chain block at height `h`.
    best: Vec<Record>,
    /// The tip's hash.
    tip: H256,
}

impl BlockDir {
    /// Indexes `dir`'s block files under the network `magic` and builds the best chain from
    /// `genesis` to the highest block, or to `anchor`.
    pub fn open(
        dir: &Path,
        magic: [u8; 4],
        genesis: BlockHash,
        anchor: Option<BlockHash>,
    ) -> Result<BlockDir> {
        let files = blkfile::list_files(dir)?;
        let key = blkfile::xor_key(dir)?;
        let (records, issues) = blkfile::index(&files, key, magic)?;
        let newest = files.last().expect("list_files refuses an empty directory").0;
        if let Some(i) = issues.iter().find(|i| !(i.at_end && i.file == newest)) {
            bail!("blk{:05}.dat@{}: {}", i.file, i.offset, i.reason);
        }
        let anchor = anchor.map(|a| a.to_lendian());
        let chain = Chain::build(&records, anchor.as_ref(), Some(&genesis.to_lendian()))
            .with_context(|| format!("the best chain of {}", dir.display()))?;
        let best: Vec<Record> = chain.best.iter().map(|&i| records[i]).collect();
        let last = best.last().expect("the chain holds genesis");
        let tip = match anchor {
            Some(a) => a,
            None => match (last.dc_hash, last.bs_hash) {
                (Some(dc), Some(bs)) if dc == bs => bs,
                (dc, bs) => bail!(
                    "the libraries disagree on the tip's hash: {:?} and {:?}",
                    dc.map(|h| rev_hex(&h)),
                    bs.map(|h| rev_hex(&h))
                ),
            },
        };
        Ok(BlockDir {
            dir: dir.to_owned(),
            files: files.into_iter().collect(),
            key,
            magic,
            best,
            tip,
        })
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The best chain's tip height.
    pub fn tip(&self) -> u32 {
        (self.best.len() - 1) as u32
    }

    /// Where height `h`'s block is: its file number and the offset of its bytes, after the
    /// 8-byte record header.
    pub fn location(&self, h: u32) -> Option<(u32, u64)> {
        self.best.get(h as usize).map(|r| (r.file, r.offset))
    }

    /// The hash height `h`'s block must have: its child's `prev_hash`, or the tip's.
    pub fn hash(&self, h: u32) -> Option<BlockHash> {
        let h = h as usize;
        let hash = match self.best.get(h + 1) {
            Some(child) => child.prev,
            None if h < self.best.len() => self.tip,
            None => return None,
        };
        Some(BlockHash::from_lendian(hash))
    }

    /// The files holding the blocks of heights `0..=to`, with their sizes.
    pub fn files(&self, to: u32) -> Result<Vec<(PathBuf, u64)>> {
        let used: BTreeSet<u32> = self.best.iter().take(to as usize + 1).map(|r| r.file).collect();
        used.into_iter()
            .map(|n| {
                let path = self.files[&n].clone();
                let len = std::fs::metadata(&path)
                    .with_context(|| format!("stat {}", path.display()))?
                    .len();
                Ok((path, len))
            })
            .collect()
    }

    /// The headers of heights `from..=to`, one at a time, each with its hash.
    pub fn headers(
        &self,
        from: u32,
        to: u32,
    ) -> impl Iterator<Item = Result<(Header, BlockHash)>> + '_ {
        let mut reader = Reader::new(self);
        (from..=to).map(move |h| {
            let raw = reader.header(h)?;
            let header = decode_header(&mut &raw[..])
                .map_err(|e| anyhow::anyhow!("height {h}: the header does not decode: {e:?}"))?
                .into_value();
            let hash = self.check(h, header.hash())?;
            Ok((header, hash))
        })
    }

    /// The blocks of heights `0..=to`, one at a time.
    pub fn blocks(&self, to: u32) -> impl Iterator<Item = Result<Arc<Block>>> + '_ {
        self.blocks_from(0, to)
    }

    /// The blocks of heights `from..=to`, one at a time.
    pub fn blocks_from(&self, from: u32, to: u32) -> impl Iterator<Item = Result<Arc<Block>>> + '_ {
        let mut reader = Reader::new(self);
        (from..=to).map(move |h| {
            let raw = reader.block(h)?;
            let mut rest = &raw[..];
            let block = decode_block(&mut rest)
                .map_err(|e| anyhow::anyhow!("height {h}: the block does not decode: {e:?}"))?
                .into_value();
            ensure!(rest.is_empty(), "height {h}: {} bytes after the block", rest.len());
            self.check(h, block.hash())?;
            Ok(Arc::new(block))
        })
    }

    /// `hash`, if it is height `h`'s.
    fn check(&self, h: u32, hash: BlockHash) -> Result<BlockHash> {
        let want = self.hash(h).with_context(|| format!("height {h} is past the tip"))?;
        ensure!(hash == want, "height {h}: the block hashes to {hash}, not {want}");
        Ok(hash)
    }
}

/// Reads best-chain records, keeping the last file open: a chain in height order mostly reads
/// one file after another.
struct Reader<'a> {
    dir: &'a BlockDir,
    open: Option<(u32, File)>,
}

impl<'a> Reader<'a> {
    fn new(dir: &'a BlockDir) -> Reader<'a> {
        Reader {
            dir,
            open: None,
        }
    }

    fn record(&mut self, h: u32) -> Result<(&File, Record)> {
        let record = *self
            .dir
            .best
            .get(h as usize)
            .with_context(|| format!("height {h} is past the tip"))?;
        if self.open.as_ref().is_none_or(|(n, _)| *n != record.file) {
            let path = &self.dir.files[&record.file];
            let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
            self.open = Some((record.file, file));
        }
        Ok((&self.open.as_ref().expect("opened").1, record))
    }

    fn header(&mut self, h: u32) -> Result<[u8; 80]> {
        let (key, magic) = (self.dir.key, self.dir.magic);
        let (file, record) = self.record(h)?;
        blkfile::read_header(file, &record, key, magic)
    }

    fn block(&mut self, h: u32) -> Result<Vec<u8>> {
        let (key, magic) = (self.dir.key, self.dir.magic);
        let (file, record) = self.record(h)?;
        blkfile::read_block(file, &record, key, magic)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::unix::fs::FileExt as _;

    use super::*;

    const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

    /// A one-coinbase block on `prev`; `nonce` tells siblings apart. Nothing here is mined: the
    /// source checks framing and hashes, V checks the rest.
    fn block(prev: [u8; 32], nonce: u32) -> Vec<u8> {
        let mut b = 0x2000_0000i32.to_le_bytes().to_vec();
        b.extend(prev);
        b.extend([0u8; 32]);
        b.extend(1_700_000_000u32.to_le_bytes());
        b.extend(0x207f_ffffu32.to_le_bytes());
        b.extend(nonce.to_le_bytes());
        b.push(1);
        b.extend(1i32.to_le_bytes());
        b.push(1);
        b.extend([0u8; 32]);
        b.extend(u32::MAX.to_le_bytes());
        b.extend([2, 0x51, 0x51]);
        b.extend(u32::MAX.to_le_bytes());
        b.push(1);
        b.extend(0i64.to_le_bytes());
        b.extend([1, 0x51]);
        b.extend(0u32.to_le_bytes());
        b
    }

    /// The error of a block read that must fail.
    fn err(r: Option<Result<Arc<Block>>>) -> String {
        match r.expect("a read") {
            Ok(b) => panic!("read {}", b.hash()),
            Err(e) => format!("{e:#}"),
        }
    }

    fn hash(b: &[u8]) -> BlockHash {
        decode_block(&mut &b[..]).unwrap().into_value().hash()
    }

    /// Appends `block`'s record to `file`; the offset of its bytes.
    fn record(file: &mut Vec<u8>, block: &[u8]) -> u64 {
        file.extend(MAGIC);
        file.extend((block.len() as u32).to_le_bytes());
        file.extend(block);
        (file.len() - block.len()) as u64
    }

    /// genesis ← a ← b ← c, with a stale sibling of b, out of order over two files, each with a
    /// zero tail. The blocks, the stale one, and each block's (file, offset).
    struct Fixture {
        dir: tempfile::TempDir,
        chain: Vec<Vec<u8>>,
        stale: Vec<u8>,
        at: Vec<(u32, u64)>,
    }

    fn fixture() -> Fixture {
        let g = block([0; 32], 0);
        let a = block(hash(&g).to_lendian(), 1);
        let b = block(hash(&a).to_lendian(), 2);
        let stale = block(hash(&a).to_lendian(), 3);
        let c = block(hash(&b).to_lendian(), 4);
        let (mut f0, mut f1) = (Vec::new(), Vec::new());
        let at_b = record(&mut f0, &b);
        let at_g = record(&mut f0, &g);
        record(&mut f0, &stale);
        let at_c = record(&mut f1, &c);
        let at_a = record(&mut f1, &a);
        f0.extend([0; 64]);
        f1.extend([0; 64]);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blk00000.dat"), &f0).unwrap();
        std::fs::write(dir.path().join("blk00001.dat"), &f1).unwrap();
        // Not block files: ignored.
        std::fs::write(dir.path().join("rev00000.dat"), [1, 2, 3]).unwrap();
        Fixture {
            dir,
            chain: vec![g, a, b, c],
            stale,
            at: vec![(0, at_g), (1, at_a), (0, at_b), (1, at_c)],
        }
    }

    fn open(f: &Fixture, anchor: Option<BlockHash>) -> Result<BlockDir> {
        BlockDir::open(f.dir.path(), MAGIC, hash(&f.chain[0]), anchor)
    }

    #[test]
    fn the_best_chain_comes_out_in_height_order() {
        let f = fixture();
        let dir = open(&f, None).unwrap();
        assert_eq!(dir.tip(), 3);
        for (h, b) in f.chain.iter().enumerate() {
            assert_eq!(dir.location(h as u32), Some(f.at[h]), "h {h}");
            assert_eq!(dir.hash(h as u32), Some(hash(b)), "h {h}");
        }
        assert_eq!((dir.location(4), dir.hash(4)), (None, None));
        let blocks: Vec<_> = dir.blocks(3).map(Result::unwrap).collect();
        let want: Vec<_> = f.chain.iter().map(|b| hash(b)).collect();
        assert_eq!(blocks.iter().map(|b| b.hash()).collect::<Vec<_>>(), want);
        let from: Vec<_> = dir.blocks_from(2, 3).map(|b| b.unwrap().hash()).collect();
        assert_eq!(from, want[2..]);
        assert_eq!(dir.blocks(1).count(), 2);
        let headers: Vec<_> = dir.headers(1, 3).map(Result::unwrap).collect();
        assert_eq!(headers.iter().map(|(_, h)| *h).collect::<Vec<_>>(), want[1..]);
        assert!(headers.iter().all(|(header, h)| header.hash() == *h));
        assert!(err(dir.blocks(4).last()).contains("height 4 is past the tip"));
        let files = dir.files(1).unwrap();
        let names: Vec<_> = files.iter().map(|(p, _)| p.file_name().unwrap().to_owned()).collect();
        assert_eq!(names, ["blk00000.dat", "blk00001.dat"]);
        assert_eq!(files[0].1, std::fs::metadata(f.dir.path().join("blk00000.dat")).unwrap().len());
        assert_eq!(dir.files(0).unwrap().len(), 1);
    }

    #[test]
    fn an_anchor_ends_the_chain() {
        let f = fixture();
        let dir = open(&f, Some(hash(&f.stale))).unwrap();
        assert_eq!((dir.tip(), dir.hash(2)), (2, Some(hash(&f.stale))));
        let last = dir.blocks(2).last().unwrap().unwrap();
        assert_eq!(last.hash(), hash(&f.stale));
        let err = open(&f, Some(BlockHash::from_lendian([7; 32]))).err().unwrap();
        assert!(format!("{err:#}").contains("is not linked to genesis"), "{err:#}");
        let err = BlockDir::open(f.dir.path(), MAGIC, BlockHash::from_lendian([7; 32]), None);
        assert!(format!("{:#}", err.err().unwrap()).contains("not found"));
    }

    #[test]
    fn only_a_record_cut_at_the_newest_files_end_is_skipped() {
        let f = fixture();
        // A cut write in place of the newest file's zero tail.
        let newest = f.dir.path().join("blk00001.dat");
        let len = std::fs::metadata(&newest).unwrap().len();
        std::fs::OpenOptions::new().write(true).open(&newest).unwrap().set_len(len - 64).unwrap();
        let mut file = std::fs::OpenOptions::new().append(true).open(&newest).unwrap();
        file.write_all(&MAGIC).unwrap();
        file.write_all(&1000u32.to_le_bytes()).unwrap();
        file.write_all(&[9; 20]).unwrap();
        drop(file);
        assert_eq!(open(&f, None).unwrap().tip(), 3);

        let older = f.dir.path().join("blk00000.dat");
        let mut file = std::fs::OpenOptions::new().append(true).open(&older).unwrap();
        file.write_all(&[9; 20]).unwrap();
        drop(file);
        let err = format!("{:#}", open(&f, None).err().unwrap());
        assert!(err.starts_with("blk00000.dat@"), "{err}");
    }

    #[test]
    fn a_record_changed_after_indexing_is_refused() {
        let f = fixture();
        let dir = open(&f, None).unwrap();
        // Block a's nonce: the frame and prev_hash still match, the hash does not.
        let (file, off) = f.at[1];
        let path = f.dir.path().join(format!("blk{file:05}.dat"));
        let handle = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        handle.write_all_at(&[0xee], off + 76).unwrap();
        let e = err(dir.blocks(3).nth(1));
        assert!(e.contains("height 1: the block hashes to"), "{e}");
        let e = format!("{:#}", dir.headers(1, 1).next().unwrap().unwrap_err());
        assert!(e.contains("height 1: the block hashes to"), "{e}");
        // Its prev_hash: the record is not the one indexed.
        handle.write_all_at(&[0xee], off + 4).unwrap();
        let e = err(dir.blocks(1).nth(1));
        assert!(e.contains("changed since it was indexed"), "{e}");
        let e = format!("{:#}", dir.headers(1, 1).next().unwrap().unwrap_err());
        assert!(e.contains("changed since it was indexed"), "{e}");
    }
}
