//! Core's external block-file scanner (`-reindex`, `-loadblock`), ported exactly.
//!
//! `ChainstateManager::LoadExternalBlockFile` reads a file through a `CBufferedFile` and looks for
//! `magic ‖ u32 size ‖ block` records. Which records it finds in a damaged file depends on details
//! of that stream: where it rewinds after a failure, its read limit, and when it reports end of
//! file (which depends on its fill sizes). [`BufferedFile`] is that stream over any
//! [`Read`] source, with Core's ring buffer (2 × `MaxBlockSize()`, a `MaxBlockSize() + 8` rewind
//! reserve), so a file of any size streams in bounded memory; [`load_external_block_file`] is the
//! scan loop, and its [`ScanReport`] keeps counts and a fixed sample, never a per-record history. What happens to each block (out of order, already stored, accepted) is the caller's
//! [`ExternalBlockSink`], which can also interrupt the scan before every record (Core's
//! `ShutdownRequested()`).
//!
//! The deserializer here deliberately repeats what [`crate::tx`] decodes (`MAX_SIZE`, the
//! transaction layout): it must consume the stream exactly as Core's `operator>>` does, with every
//! read and limit check at the same position, which a decoder over a slice does not model.
//!
//! The scan is identical at v23.1.8 and v24.0.0-rc.3.

use std::io::{ErrorKind, Read};

use crate::Hash256;
use crate::tx::block_hash;

/// Core's `MaxBlockSize()` (DIP0001 active), the upper bound of a record's size field.
// Core: consensus/consensus.h MaxBlockSize
pub const MAX_BLOCK_SIZE: u32 = 2_000_000;
/// Core's `MAX_SIZE`, the largest `CompactSize` a deserializer accepts.
// Core: serialize.h MAX_SIZE
const MAX_SIZE: u64 = 0x0200_0000;
/// Core reads byte vectors in chunks of at most this many bytes.
// Core: serialize.h Unserialize(prevector / std::vector of bytes)
const BYTE_VECTOR_CHUNK: u64 = 5_000_000;

/// The exceptions a `CBufferedFile` read can throw. Core catches them all alike; they are kept
/// apart for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    /// `CBufferedFile::Fill: end of file`.
    EndOfFile,
    /// `CBufferedFile::Fill: fread failed` (a read error from the source).
    ReadFailed,
    /// `Attempt to position past buffer limit`.
    PastLimit,
    /// `non-canonical ReadCompactSize()` or `ReadCompactSize(): size too large`.
    BadCompactSize,
}

/// Core's `CBufferedFile`: a ring buffer over a sequential source, which can rewind up to
/// `rewind` bytes behind the read position.
// Core: streams.h CBufferedFile
pub struct BufferedFile<R> {
    src: R,
    buf: Vec<u8>,
    /// Bytes read from the source (`nSrcPos`).
    src_pos: u64,
    /// The source's end-of-file indicator (`feof(src)`): set by a short `fread` at the end.
    src_eof: bool,
    /// Bytes consumed by the reader (`m_read_pos`).
    read_pos: u64,
    read_limit: u64,
    rewind: u64,
    buf_size: u64,
}

impl<R: Read> BufferedFile<R> {
    pub fn new(src: R, buf_size: u64, rewind: u64) -> Self {
        assert!(rewind < buf_size, "Rewind limit must be less than buffer size");
        Self {
            src,
            buf: vec![0; buf_size as usize],
            src_pos: 0,
            src_eof: false,
            read_pos: 0,
            read_limit: u64::MAX,
            rewind,
            buf_size,
        }
    }

    /// `fread`: reads until `out` is full, the source ends (setting the end-of-file indicator),
    /// or it fails (leaving the indicator clear). Returns the bytes read.
    fn fread(&mut self, from: usize, len: usize) -> usize {
        let mut n = 0;
        while n < len {
            match self.src.read(&mut self.buf[from + n..from + len]) {
                Ok(0) => {
                    self.src_eof = true;
                    break;
                }
                Ok(k) => n += k,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        n
    }

    // Core: CBufferedFile::Fill (32-bit unsigned arithmetic, as in Core)
    fn fill(&mut self) -> Result<bool, StreamError> {
        let pos = (self.src_pos % self.buf_size) as u32;
        let mut read_now = (self.buf_size as u32).wrapping_sub(pos);
        let avail =
            self.buf_size.wrapping_sub(self.src_pos - self.read_pos).wrapping_sub(self.rewind)
                as u32;
        if avail < read_now {
            read_now = avail;
        }
        if read_now == 0 {
            return Ok(false);
        }
        let n = self.fread(pos as usize, read_now as usize);
        if n == 0 {
            return Err(if self.src_eof {
                StreamError::EndOfFile
            } else {
                StreamError::ReadFailed
            });
        }
        self.src_pos += n as u64;
        Ok(true)
    }

    // Core: CBufferedFile::AdvanceStream
    fn advance(&mut self, length: u64) -> Result<(usize, usize), StreamError> {
        debug_assert!(self.read_pos <= self.src_pos);
        if self.read_pos + length > self.read_limit {
            return Err(StreamError::PastLimit);
        }
        if self.read_pos == self.src_pos && length > 0 {
            self.fill()?;
        }
        let offset = (self.read_pos % self.buf_size) as usize;
        let buffer_available = self.buf_size - offset as u64;
        let advance = length.min(buffer_available).min(self.src_pos - self.read_pos);
        self.read_pos += advance;
        Ok((offset, advance as usize))
    }

    /// Reads exactly `out.len()` bytes.
    // Core: CBufferedFile::read
    pub fn read(&mut self, out: &mut [u8]) -> Result<(), StreamError> {
        let mut done = 0;
        while done < out.len() {
            let (offset, n) = self.advance((out.len() - done) as u64)?;
            out[done..done + n].copy_from_slice(&self.buf[offset..offset + n]);
            done += n;
        }
        Ok(())
    }

    /// Reads and discards `n` bytes, with the same stream positions as a read.
    fn skip(&mut self, n: u64) -> Result<(), StreamError> {
        let mut left = n;
        while left > 0 {
            left -= self.advance(left)?.1 as u64;
        }
        Ok(())
    }

    // Core: CBufferedFile::SkipTo
    pub fn skip_to(&mut self, file_pos: u64) -> Result<(), StreamError> {
        assert!(file_pos >= self.read_pos);
        while self.read_pos < file_pos {
            self.advance(file_pos - self.read_pos)?;
        }
        Ok(())
    }

    pub fn pos(&self) -> u64 {
        self.read_pos
    }

    // Core: CBufferedFile::SetPos
    pub fn set_pos(&mut self, pos: u64) -> bool {
        if pos + self.buf_size < self.src_pos {
            self.read_pos = self.src_pos - self.buf_size;
            return false;
        }
        if pos > self.src_pos {
            self.read_pos = self.src_pos;
            return false;
        }
        self.read_pos = pos;
        true
    }

    // Core: CBufferedFile::SetLimit
    pub fn set_limit(&mut self, pos: u64) -> bool {
        if pos < self.read_pos {
            return false;
        }
        self.read_limit = pos;
        true
    }

    // Core: CBufferedFile::eof
    pub fn eof(&self) -> bool {
        self.read_pos == self.src_pos && self.src_eof
    }

    /// Advances to the next occurrence of `byte`, staying on it.
    // Core: CBufferedFile::FindByte
    pub fn find_byte(&mut self, byte: u8) -> Result<(), StreamError> {
        loop {
            if self.read_pos == self.src_pos {
                self.fill()?;
            }
            let offset = (self.read_pos % self.buf_size) as usize;
            let len = ((self.buf_size - offset as u64).min(self.src_pos - self.read_pos)) as usize;
            match self.buf[offset..offset + len].iter().position(|&b| b == byte) {
                Some(i) => {
                    self.read_pos += i as u64;
                    return Ok(());
                }
                None => self.read_pos += len as u64,
            }
        }
    }

    /// Copies the already-read bytes `[start, read position)` out of the buffer; they must lie
    /// within the rewind reserve (a block just deserialized does: it is at most
    /// `MaxBlockSize()` bytes and the reserve is larger).
    fn copy_back(&self, start: u64, out: &mut Vec<u8>) {
        assert!(self.read_pos - start <= self.rewind, "bytes beyond the rewind reserve");
        out.clear();
        let mut at = start;
        while at < self.read_pos {
            let offset = (at % self.buf_size) as usize;
            let n = ((self.buf_size - offset as u64).min(self.read_pos - at)) as usize;
            out.extend_from_slice(&self.buf[offset..offset + n]);
            at += n as u64;
        }
    }

    // Core: serialize.h ReadCompactSize (range_check = true)
    fn compact_size(&mut self) -> Result<u64, StreamError> {
        // Each wider form must encode a value the narrower ones cannot (canonical encoding).
        let (n, min) = match self.array::<1>()?[0] {
            first @ 0..=252 => (u64::from(first), 0),
            253 => (u64::from(u16::from_le_bytes(self.array()?)), 253),
            254 => (u64::from(self.u32_le()?), 0x1_0000),
            255 => (u64::from_le_bytes(self.array()?), 0x1_0000_0000),
        };
        if n < min || n > MAX_SIZE {
            return Err(StreamError::BadCompactSize);
        }
        Ok(n)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], StreamError> {
        let mut out = [0u8; N];
        self.read(&mut out)?;
        Ok(out)
    }

    fn u32_le(&mut self) -> Result<u32, StreamError> {
        self.array().map(u32::from_le_bytes)
    }

    /// A `CompactSize`-prefixed byte vector (`CScript`, `vExtraPayload`), read in Core's chunks.
    fn byte_vector(&mut self) -> Result<(), StreamError> {
        let n = self.compact_size()?;
        let mut done = 0;
        while done < n {
            let chunk = (n - done).min(BYTE_VECTOR_CHUNK);
            self.skip(chunk)?;
            done += chunk;
        }
        Ok(())
    }

    /// Deserializes one Dash transaction (consuming exactly its bytes).
    // Core: primitives/transaction.h CMutableTransaction::SERIALIZE_METHODS
    fn transaction(&mut self) -> Result<(), StreamError> {
        let version = self.u32_le()?;
        let n_in = self.compact_size()?;
        for _ in 0..n_in {
            self.skip(32)?; // prevout hash
            self.skip(4)?; // prevout n
            self.byte_vector()?; // scriptSig
            self.skip(4)?; // nSequence
        }
        let n_out = self.compact_size()?;
        for _ in 0..n_out {
            self.skip(8)?; // nValue
            self.byte_vector()?; // scriptPubKey
        }
        self.skip(4)?; // nLockTime
        let tx_version = (version & 0xffff) as u16 as i16;
        let tx_type = (version >> 16) as u16;
        if tx_version >= 3 && tx_type != 0 {
            self.byte_vector()?; // vExtraPayload
        }
        Ok(())
    }

    /// Deserializes one block (`blkdat >> *pblock`).
    // Core: primitives/block.h CBlock::SERIALIZE_METHODS
    fn block(&mut self) -> Result<(), StreamError> {
        for field in [4, 32, 32, 4, 4, 4] {
            self.skip(field)?;
        }
        let n_tx = self.compact_size()?;
        for _ in 0..n_tx {
            self.transaction()?;
        }
        Ok(())
    }
}

/// What the caller's block index says about a scanned header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderStatus {
    /// Not genesis and the parent is not in the block index: skip (`-reindex` remembers the
    /// position for when the parent arrives).
    UnknownParent,
    /// The block is already stored (`BLOCK_HAVE_DATA`): skip.
    Stored,
    /// Read and process the block.
    New,
}

/// Whether the scan goes on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Core's `break` out of the scan (`state.IsError()`, an activation failure).
    Stop,
}

/// The block-index side of [`load_external_block_file`].
pub trait ExternalBlockSink {
    /// Checked before every record, as Core checks `ShutdownRequested()`: `true` ends the scan.
    fn interrupted(&mut self) -> bool {
        false
    }
    /// Classifies a header found at data position `pos` (Core's out-of-order and
    /// already-have checks, under `cs_main`).
    fn header(&mut self, header: &[u8; 80], hash: &Hash256, pos: u64) -> HeaderStatus;
    /// A [`HeaderStatus::New`] block that deserialized: `block` is exactly its bytes. Core's
    /// `AcceptBlock`; [`Flow::Stop`] for `state.IsError()`.
    fn block(&mut self, block: &[u8], hash: &Hash256, pos: u64) -> Flow;
    /// After a stored or new block: Core's genesis activation, prune-mode activation and the
    /// processing of earlier out-of-order children (`-reindex`).
    fn after(&mut self, _hash: &Hash256) -> Flow {
        Flow::Continue
    }
    /// Every record whose header was read, at its data position (including a record whose rest
    /// then runs past the end of the file). Diagnostics: [`ScanReport`] keeps only counts and the
    /// first positions, so a caller that wants all of them (the differential tests) collects them
    /// here, at its own cost.
    fn header_read(&mut self, _pos: u64) {}
    /// Every `New` record that failed to deserialize, at its data position (diagnostics).
    fn undecodable(&mut self, _pos: u64) {}
    /// Every file offset Core logs as "unexpected data at file offset … continuing"
    /// (diagnostics).
    fn unexpected_data(&mut self, _offset: u64) {}
}

/// Positions a [`Tally`] keeps; its count covers the rest.
pub const REPORT_SAMPLE: usize = 8;

/// How often something happened during a scan, and the first [`REPORT_SAMPLE`] positions.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tally {
    pub count: u64,
    pub first: Vec<u64>,
}

impl Tally {
    fn note(&mut self, at: u64) {
        self.count += 1;
        if self.first.len() < REPORT_SAMPLE {
            self.first.reserve_exact(REPORT_SAMPLE - self.first.len());
            self.first.push(at);
        }
    }
}

/// What a scan saw, for logs and tests. Its size does not grow with the file: the scan of an
/// untrusted file of any length keeps only counts and a fixed sample (the sink's diagnostic
/// hooks see every event).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanReport {
    /// Records whose header was read (data positions).
    pub headers: Tally,
    /// `New` records that failed to deserialize (data positions).
    pub undecodable: Tally,
    /// Offsets Core logs as "unexpected data at file offset … continuing".
    pub unexpected_data: Tally,
    /// The scan ended on a read error from the source (Core: `fread failed`).
    pub read_failed: bool,
    /// The sink interrupted the scan.
    pub interrupted: bool,
}

/// Scans one external block file exactly as Core does, streaming it from `src`.
// Core: validation.cpp ChainstateManager::LoadExternalBlockFile
pub fn load_external_block_file(
    src: impl Read,
    magic: [u8; 4],
    sink: &mut impl ExternalBlockSink,
) -> ScanReport {
    let max = u64::from(MAX_BLOCK_SIZE);
    let mut report = ScanReport::default();
    let mut blkdat = BufferedFile::new(src, 2 * max, max + 8);
    let mut block = Vec::new();
    // Where to resume scanning in case something goes wrong.
    let mut rewind = blkdat.pos();
    while !blkdat.eof() {
        if sink.interrupted() {
            report.interrupted = true;
            break;
        }
        blkdat.set_pos(rewind);
        rewind += 1; // start one byte further next time, in case of failure
        blkdat.set_limit(u64::MAX);
        let size = match find_record(&mut blkdat, magic, max, &mut rewind) {
            // No valid record header found (the end of every blk file).
            Err(e) => {
                report.read_failed = e == StreamError::ReadFailed;
                break;
            }
            Ok(None) => continue,
            Ok(Some(size)) => size,
        };
        match scan_record(&mut blkdat, size, &mut rewind, sink, &mut report, &mut block) {
            Ok(Flow::Continue) => {}
            Ok(Flow::Stop) => break,
            Err(_) => {
                report.unexpected_data.note(rewind - 1);
                sink.unexpected_data(rewind - 1);
            }
        }
    }
    report
}

/// Locates `magic ‖ size`; `Ok(None)` resumes the scan one byte further.
fn find_record<R: Read>(
    blkdat: &mut BufferedFile<R>,
    magic: [u8; 4],
    max: u64,
    rewind: &mut u64,
) -> Result<Option<u64>, StreamError> {
    blkdat.find_byte(magic[0])?;
    *rewind = blkdat.pos() + 1;
    if blkdat.array::<4>()? != magic {
        return Ok(None);
    }
    let size = u64::from(blkdat.u32_le()?);
    if !(80..=max).contains(&size) {
        return Ok(None);
    }
    Ok(Some(size))
}

fn scan_record<R: Read>(
    blkdat: &mut BufferedFile<R>,
    size: u64,
    rewind: &mut u64,
    sink: &mut impl ExternalBlockSink,
    report: &mut ScanReport,
    block: &mut Vec<u8>,
) -> Result<Flow, StreamError> {
    let block_pos = blkdat.pos();
    blkdat.set_limit(block_pos + size);
    let header: [u8; 80] = blkdat.array()?;
    report.headers.note(block_pos);
    sink.header_read(block_pos);
    let hash = block_hash(&header);
    // Skip the rest of the record; it can still be re-read from the buffer.
    *rewind = block_pos + size;
    blkdat.skip_to(*rewind)?;
    match sink.header(&header, &hash, block_pos) {
        HeaderStatus::UnknownParent => return Ok(Flow::Continue),
        HeaderStatus::Stored => {}
        HeaderStatus::New => {
            blkdat.set_pos(block_pos);
            if let Err(e) = blkdat.block() {
                report.undecodable.note(block_pos);
                sink.undecodable(block_pos);
                return Err(e);
            }
            *rewind = blkdat.pos();
            blkdat.copy_back(block_pos, block);
            if sink.block(block, &hash, block_pos) == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
    }
    Ok(sink.after(&hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

    /// A minimal block: header, one transaction with no inputs and one output.
    fn block(nonce: u8) -> Vec<u8> {
        let mut b = vec![0u8; 80];
        b[76] = nonce;
        b.push(1); // one tx
        b.extend(1u32.to_le_bytes());
        b.push(0); // no inputs
        b.push(1);
        b.extend(5i64.to_le_bytes());
        b.extend([1, 0x51]);
        b.extend(0u32.to_le_bytes());
        b
    }

    fn record(payload: &[u8], size: u32) -> Vec<u8> {
        let mut r = MAGIC.to_vec();
        r.extend(size.to_le_bytes());
        r.extend(payload);
        r
    }

    #[derive(Default)]
    struct Collect {
        accepted: Vec<(u64, Vec<u8>)>,
    }

    impl ExternalBlockSink for Collect {
        fn header(&mut self, _: &[u8; 80], _: &Hash256, _: u64) -> HeaderStatus {
            HeaderStatus::New
        }
        fn block(&mut self, block: &[u8], _: &Hash256, pos: u64) -> Flow {
            self.accepted.push((pos, block.to_vec()));
            Flow::Continue
        }
    }

    /// A source that returns at most 7 bytes per `read`, as a pipe might.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = out.len().min(7).min(self.0.len());
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    fn scan(file: &[u8]) -> (Vec<(u64, Vec<u8>)>, ScanReport) {
        let mut sink = Collect::default();
        let report = load_external_block_file(file, MAGIC, &mut sink);
        // The same scan through a source that returns a few bytes per read.
        let mut again = Collect::default();
        let trickle = Trickle(file);
        assert_eq!(load_external_block_file(trickle, MAGIC, &mut again), report);
        assert_eq!(again.accepted, sink.accepted);
        (sink.accepted, report)
    }

    #[test]
    fn clean_records_and_zero_tail() {
        let (a, b) = (block(1), block(2));
        let mut f = record(&a, a.len() as u32);
        f.extend(record(&b, b.len() as u32));
        f.extend([0u8; 1000]);
        let (got, report) = scan(&f);
        assert_eq!(got, vec![(8, a.clone()), (16 + a.len() as u64, b)]);
        assert_eq!(report.unexpected_data.count, 0);
    }

    #[test]
    fn oversized_record_rescans_its_tail() {
        // The size field claims 20 more bytes than the block: after the block is read, the scan
        // resumes at the block's end and finds the record hidden in the claimed tail.
        let (a, b) = (block(1), block(2));
        let inner = record(&b, b.len() as u32);
        let mut payload = a.clone();
        payload.extend(&inner);
        let mut f = record(&payload, (a.len() + inner.len()) as u32);
        f.extend([0u8; 16]);
        let (got, _) = scan(&f);
        assert_eq!(got.iter().map(|(_, b)| b.clone()).collect::<Vec<_>>(), vec![a, b]);
    }

    #[test]
    fn undersized_record_fails_and_resumes_after_its_claimed_end() {
        // The size field cuts the block short: deserialization hits the read limit, and the scan
        // resumes at the claimed end, inside the block.
        let (a, b) = (block(1), block(2));
        let mut f = record(&a, a.len() as u32 - 3);
        f.extend(record(&b, b.len() as u32));
        let (got, report) = scan(&f);
        assert_eq!(got, vec![(16 + a.len() as u64, b)]);
        assert_eq!(report.undecodable.first, vec![8]);
        assert_eq!(report.unexpected_data.first, vec![8 + a.len() as u64 - 3 - 1]);
    }

    #[test]
    fn torn_tail_ends_the_scan() {
        let a = block(1);
        let mut f = record(&a, a.len() as u32);
        let torn = record(&block(2), 200);
        // The torn record's header is incomplete: no second header is read.
        let (got, report) = scan(&[&f[..], &torn[..60]].concat());
        assert_eq!((got.len(), report.headers.first.clone()), (1, vec![8]));
        // Its header is complete but the rest runs past the end: the header was read.
        f.extend(&torn[..100]);
        let (got, report) = scan(&f);
        assert_eq!(got.len(), 1);
        assert_eq!(report.headers.first, vec![8, 16 + a.len() as u64]);
        assert_eq!(report.unexpected_data.count, 1);
    }

    #[test]
    fn non_canonical_compact_size_is_rejected() {
        let mut a = block(1);
        a[80] = 0xfd; // tx count as a 3-byte CompactSize of value 1
        a.splice(81..81, [1, 0]);
        let f = record(&a, a.len() as u32);
        let (got, report) = scan(&f);
        assert!(got.is_empty());
        assert_eq!(report.undecodable.first, vec![8]);
    }

    #[test]
    fn buffered_file_end_of_file_follows_fread() {
        let data = [7u8; 10];
        // A fill that reads exactly the rest of the file does not set feof.
        let mut f = BufferedFile::new(&data[..], 10, 0);
        assert_eq!(f.array::<10>().unwrap(), data);
        assert!(!f.eof());
        assert_eq!(f.array::<1>(), Err(StreamError::EndOfFile));
        assert!(f.eof());
        // A short fill does.
        let mut f = BufferedFile::new(&data[..], 16, 0);
        f.array::<10>().unwrap();
        assert!(f.eof());
        // The read limit throws before moving.
        let mut f = BufferedFile::new(&data[..], 16, 0);
        f.set_limit(4);
        assert_eq!(f.array::<5>(), Err(StreamError::PastLimit));
        assert_eq!(f.pos(), 0);
    }

    #[test]
    fn the_ring_buffer_wraps_and_rewinds() {
        // A 16-byte buffer keeping 8 bytes behind the read position, over 40 distinct bytes.
        let data: Vec<u8> = (0..40).collect();
        let mut f = BufferedFile::new(data.as_slice(), 16, 8);
        let mut out = [0u8; 12];
        f.read(&mut out).unwrap();
        assert_eq!(out.to_vec(), (0..12).collect::<Vec<u8>>());
        f.skip_to(30).unwrap();
        // Within the reserve: back to 22, and the bytes there are intact after the wrap.
        assert!(f.set_pos(22));
        f.read(&mut out[..8]).unwrap();
        assert_eq!(out[..8].to_vec(), (22..30).collect::<Vec<u8>>());
        let mut back = Vec::new();
        f.copy_back(23, &mut back);
        assert_eq!(back, (23..30).collect::<Vec<u8>>());
    }

    #[test]
    fn a_read_error_ends_the_scan() {
        struct Failing;
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("EIO"))
            }
        }
        let report = load_external_block_file(Failing, MAGIC, &mut Collect::default());
        assert!(report.read_failed && report.headers.count == 0);
    }

    #[test]
    fn the_sink_can_interrupt_before_every_record() {
        struct Stop(usize);
        impl ExternalBlockSink for Stop {
            fn interrupted(&mut self) -> bool {
                self.0 == 1
            }
            fn header(&mut self, _: &[u8; 80], _: &Hash256, _: u64) -> HeaderStatus {
                HeaderStatus::New
            }
            fn block(&mut self, _: &[u8], _: &Hash256, _: u64) -> Flow {
                self.0 += 1;
                Flow::Continue
            }
        }
        let (a, b) = (block(1), block(2));
        let mut f = record(&a, a.len() as u32);
        f.extend(record(&b, b.len() as u32));
        let mut sink = Stop(0);
        let report = load_external_block_file(f.as_slice(), MAGIC, &mut sink);
        assert!(report.interrupted);
        assert_eq!((sink.0, report.headers.count), (1, 1));
    }

    /// A generated file of `n` copies of one record, never held in memory.
    struct Repeat {
        record: Vec<u8>,
        n: u64,
        at: u64,
    }

    impl Read for Repeat {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let len = self.record.len() as u64;
            let total = len * self.n;
            let mut done = 0;
            while done < out.len() && self.at < total {
                let off = (self.at % len) as usize;
                let k = (self.record.len() - off).min(out.len() - done);
                out[done..done + k].copy_from_slice(&self.record[off..off + k]);
                done += k;
                self.at += k as u64;
            }
            Ok(done)
        }
    }

    /// Classifies every header as given; every `New` record here fails to deserialize.
    struct Fixed(HeaderStatus);

    impl ExternalBlockSink for Fixed {
        fn header(&mut self, _: &[u8; 80], _: &Hash256, _: u64) -> HeaderStatus {
            self.0
        }
        fn block(&mut self, _: &[u8], _: &Hash256, _: u64) -> Flow {
            panic!("no record here deserializes")
        }
    }

    fn retained(report: &ScanReport) -> [usize; 3] {
        [&report.headers, &report.undecodable, &report.unexpected_data].map(|t| t.first.capacity())
    }

    #[test]
    fn the_report_stays_bounded_as_the_record_count_grows() {
        // Unknown-parent headers (GPT ST2-r2 finding 1), and malformed new records: a size of 100
        // over an 80-byte header and 20 bytes of 0xff, whose transaction count runs past the
        // record (an undecodable record and an unexpected-data rewind each).
        let mut unknown = record(&block(1)[..80], 80);
        unknown.truncate(88);
        let mut malformed = record(&block(1)[..80], 100);
        malformed.extend([0xff; 20]);
        for (rec, status) in
            [(unknown, HeaderStatus::UnknownParent), (malformed, HeaderStatus::New)]
        {
            let mut sizes = Vec::new();
            for n in [1_000u64, 30_000] {
                let src = Repeat {
                    record: rec.clone(),
                    n,
                    at: 0,
                };
                let report = load_external_block_file(src, MAGIC, &mut Fixed(status));
                assert_eq!(report.headers.count, n, "{status:?}");
                if status == HeaderStatus::New {
                    assert_eq!((report.undecodable.count, report.unexpected_data.count), (n, n));
                }
                assert!(retained(&report).iter().all(|&c| c <= REPORT_SAMPLE));
                sizes.push(retained(&report));
            }
            assert_eq!(sizes[0], sizes[1], "{status:?}: retained capacity grew with the input");
        }
    }

    #[test]
    fn diagnostic_hooks_see_every_event() {
        #[derive(Default)]
        struct All(Vec<u64>);
        impl ExternalBlockSink for All {
            fn header(&mut self, _: &[u8; 80], _: &Hash256, _: u64) -> HeaderStatus {
                HeaderStatus::UnknownParent
            }
            fn block(&mut self, _: &[u8], _: &Hash256, _: u64) -> Flow {
                Flow::Continue
            }
            fn header_read(&mut self, pos: u64) {
                self.0.push(pos);
            }
        }
        let rec = record(&block(1), block(1).len() as u32);
        let file = rec.repeat(20);
        let mut sink = All::default();
        let report = load_external_block_file(file.as_slice(), MAGIC, &mut sink);
        let want: Vec<u64> = (0..20).map(|i| 8 + i * rec.len() as u64).collect();
        assert_eq!(sink.0, want);
        assert_eq!((report.headers.count, &report.headers.first[..]), (20, &want[..REPORT_SAMPLE]));
    }
}
