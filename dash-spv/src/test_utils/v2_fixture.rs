//! A writer of version-2 storage (the segment files the store had before SB13), for the
//! migrator's tests and its mainnet-sized memory run.
//!
//! Provenance: the format is `storage/segments.rs` as of node/integration 6993f2f2f, which SB13
//! removed. Commit 4ce7e14d3 checked the migrator's reader against that writer itself
//! (`real_v2_segments_migrate`): sentinel padding, a truncated boundary segment, sparse filters
//! and blocks, a checkpoint start and segment edges. This writer produces the same bytes: each
//! segment file holds its segment's every slot, consensus-encoded, empty slots as the sentinel.
//! It writes one segment at a time, so a fixture of any size costs one segment of memory.

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Items per segment file of each folder (`Persistable::ITEMS_PER_SEGMENT`).
pub const HEADERS_PER_SEGMENT: u32 = 10_000;
pub const FILTER_HEADERS_PER_SEGMENT: u32 = 50_000;
pub const FILTERS_PER_SEGMENT: u32 = 2_000;
pub const BLOCKS_PER_SEGMENT: u32 = 1_000;

/// The header sentinel's 80 bytes: version `i32::MAX`, every other byte 0xff.
pub fn sentinel_header() -> [u8; 80] {
    let mut h = [0xff; 80];
    h[..4].copy_from_slice(&i32::MAX.to_le_bytes());
    h
}

/// A synthetic header at `height`, linked to `synthetic_hash(height - 1)`.
pub fn synthetic_header(height: u32) -> [u8; 80] {
    let mut h = [0; 80];
    h[..4].copy_from_slice(&0x2000_0000u32.to_le_bytes());
    if let Some(prev) = height.checked_sub(1) {
        h[4..36].copy_from_slice(&synthetic_hash(prev));
    }
    h[36..40].copy_from_slice(&height.to_le_bytes());
    h[68..72].copy_from_slice(&(1_390_000_000 + height * 150).to_le_bytes());
    h[72..76].copy_from_slice(&0x1b10_4be5u32.to_le_bytes());
    h[76..80].copy_from_slice(&height.wrapping_mul(2_654_435_761).to_le_bytes());
    h
}

/// The trusted hash version 2 stored beside [`synthetic_header`] (never recomputed).
pub fn synthetic_hash(height: u32) -> [u8; 32] {
    let mut h = [0x5a; 32];
    h[..4].copy_from_slice(&height.to_le_bytes());
    h[28..].copy_from_slice(&height.wrapping_mul(0x9e37_79b9).to_be_bytes());
    h
}

/// The filter header version 2 stored at `height`.
pub fn synthetic_filter_header(height: u32) -> [u8; 32] {
    let mut h = [0x3c; 32];
    h[..4].copy_from_slice(&height.to_le_bytes());
    h
}

/// Writes the segment files of one folder: `item(height)` is the encoded item at each height
/// of `heights` (ascending), `None` an empty slot; every other slot of a touched segment holds
/// `sentinel`.
pub fn write_segments(
    dir: &Path,
    per_segment: u32,
    heights: impl IntoIterator<Item = u32>,
    mut item: impl FnMut(u32) -> Option<Vec<u8>>,
    sentinel: &[u8],
) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut open: Option<(u32, Vec<Option<Vec<u8>>>)> = None;
    let flush = |seg: u32, slots: Vec<Option<Vec<u8>>>| -> io::Result<()> {
        let file = fs::File::create(dir.join(format!("segment_{seg:06}.dat")))?;
        let mut w = BufWriter::with_capacity(1 << 20, file);
        for slot in &slots {
            w.write_all(slot.as_deref().unwrap_or(sentinel))?;
        }
        w.flush()
    };
    for h in heights {
        let seg = h / per_segment;
        if open.as_ref().is_some_and(|(s, _)| *s != seg) {
            let (s, slots) = open.take().expect("checked");
            flush(s, slots)?;
        }
        let (_, slots) = open.get_or_insert_with(|| (seg, vec![None; per_segment as usize]));
        slots[(h % per_segment) as usize] = item(h);
    }
    if let Some((s, slots)) = open {
        flush(s, slots)?;
    }
    Ok(())
}

/// A version-2 storage path: headers at `lo..hi` and filter headers at `lo..hi` (from
/// `lo - 1` when `lo > 0`: checkpoint sync's `previous_filter_header`), a filter at every
/// `filter_every`-th height from `lo`, a block (header only, no transactions) at each of
/// `blocks`, one metadata value, one masternode log file, and `version.json = 2`.
pub struct V2Fixture {
    pub lo: u32,
    pub hi: u32,
    pub filter_every: u32,
    pub blocks: Vec<u32>,
}

impl V2Fixture {
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let (lo, hi) = (self.lo, self.hi);
        let mut header_sentinel = sentinel_header().to_vec();
        // The sentinel item is `HashedBlockHeader::from(sentinel)`: its X11 hash follows. Its
        // exact bytes do not matter to the reader, which tests the header alone.
        header_sentinel.extend_from_slice(&[0; 32]);
        write_segments(
            &path.join("block_headers"),
            HEADERS_PER_SEGMENT,
            lo..hi,
            |h| Some([&synthetic_header(h)[..], &synthetic_hash(h)[..]].concat()),
            &header_sentinel,
        )?;
        write_segments(
            &path.join("filter_headers"),
            FILTER_HEADERS_PER_SEGMENT,
            lo.saturating_sub(1)..hi,
            |h| Some(synthetic_filter_header(h).to_vec()),
            &[0; 32],
        )?;
        write_segments(
            &path.join("filters"),
            FILTERS_PER_SEGMENT,
            (lo..hi).step_by(self.filter_every as usize),
            |h| {
                let filter = h.to_le_bytes();
                Some([&[filter.len() as u8][..], &filter[..]].concat())
            },
            &[0],
        )?;
        let block_sentinel = [&[0u8; 32][..], &sentinel_header()[..], &[0u8][..]].concat();
        write_segments(
            &path.join("blocks"),
            BLOCKS_PER_SEGMENT,
            self.blocks.iter().copied(),
            |h| Some([&synthetic_hash(h)[..], &synthetic_header(h)[..], &[0u8][..]].concat()),
            &block_sentinel,
        )?;
        fs::create_dir_all(path.join("metadata"))?;
        fs::write(path.join("metadata").join("last_target_height.dat"), b"123")?;
        fs::create_dir_all(path.join("masternodes"))?;
        fs::write(path.join("masternodes").join("diff_5.dat"), b"x")?;
        fs::write(path.join("version.json"), br#"{"version": 2}"#)
    }
}
