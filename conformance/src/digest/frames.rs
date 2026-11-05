//! Streaming block input (CH3a a7-2; O8's ruling of 2026-10-13): Core's `-loadblock` framing read
//! from any [`Read`], one block at a time, so a replay holds one block of its input in memory
//! rather than the whole file (CH3a D148).
//!
//! - [`Frames`] reads `[magic][size: LE32][block]` frames, as Core's `-loadblock` writes and reads
//!   them (O8 §13.1). A stored node's `blocks.dat(.zst)` comes through [`super::store::open`],
//!   which decompresses zstd and skippable frames as a stream.
//! - [`Sha256Reader`] hashes what passes through it, so a single streaming pre-pass can check a
//!   stored file against `extract.json` and its shape before V starts ([`prepass`], D21).

use std::io::{BufRead, ErrorKind, Read};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use dash_consensus::Block;
use dash_consensus::primitives::decode_block;
use dash_primitives::BlockHash;
use dashcore::hashes::{Hash as _, HashEngine as _, sha256};

use super::store::{Node, open};

/// The largest frame `-loadblock` reads: `MaxBlockSize()`, whose default argument is the DIP0001
/// size. Core skips a frame outside `80..=MaxBlockSize()` and resynchronises; a replay's input is
/// a file Core wrote, so such a frame is refused.
// Core: validation.cpp:5439 (nMaxBlockSize), :5463 (nSize < 80 || nSize > nMaxBlockSize);
// consensus/consensus.h:10-15 (MAX_DIP0001_BLOCK_SIZE, MaxBlockSize)
pub const MAX_FRAME: u32 = 2_000_000;

/// The smallest frame `-loadblock` reads: a block header.
pub const MIN_FRAME: u32 = 80;

/// The blocks of a `-loadblock` stream, in stream order. Each is decoded from exactly its frame's
/// bytes. The first error (an I/O error, a cut frame, another network's magic, a size out of
/// bounds, a block that does not decode or does not fill its frame) ends the iteration.
pub struct Frames<R> {
    input: R,
    magic: [u8; 4],
    /// The next frame's offset in the (decompressed) stream.
    offset: u64,
    /// The frame being decoded: one block's bytes, reused.
    buf: Vec<u8>,
    done: bool,
}

/// Reads until `buf` is full or the input ends: how many bytes it read.
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

impl<R: Read> Frames<R> {
    pub fn new(input: R, magic: [u8; 4]) -> Frames<R> {
        Frames {
            input,
            magic,
            offset: 0,
            buf: Vec::new(),
            done: false,
        }
    }

    /// The input, for what follows the frames (a hash, the rest of a stream).
    pub fn into_inner(self) -> R {
        self.input
    }

    fn frame(&mut self) -> Result<Option<Arc<Block>>> {
        let at = self.offset;
        let mut head = [0u8; 8];
        let n = read_full(&mut self.input, &mut head).context("reading blocks.dat")?;
        if n == 0 {
            return Ok(None);
        }
        ensure!(n == head.len(), "blocks.dat offset {at}: a cut frame header");
        ensure!(head[..4] == self.magic, "blocks.dat offset {at}: not the network magic");
        let size = u32::from_le_bytes(head[4..].try_into().expect("4 bytes"));
        ensure!(
            (MIN_FRAME..=MAX_FRAME).contains(&size),
            "blocks.dat offset {at}: a frame of {size} bytes, outside -loadblock's \
             {MIN_FRAME}..={MAX_FRAME}"
        );
        self.buf.clear();
        // `take` grows the buffer only as bytes arrive: a cut file costs what it holds.
        (&mut self.input)
            .take(u64::from(size))
            .read_to_end(&mut self.buf)
            .context("reading blocks.dat")?;
        ensure!(self.buf.len() == size as usize, "blocks.dat offset {at}: a cut block");
        let mut raw = self.buf.as_slice();
        let block = decode_block(&mut raw)
            .map_err(|e| anyhow!("blocks.dat offset {at}: {e:?}"))?
            .into_value();
        ensure!(raw.is_empty(), "blocks.dat offset {at}: bytes after the block");
        self.offset += 8 + u64::from(size);
        Ok(Some(Arc::new(block)))
    }
}

impl<R: Read> Iterator for Frames<R> {
    type Item = Result<Arc<Block>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let item = self.frame().transpose();
        if !matches!(item, Some(Ok(_))) {
            self.done = true;
        }
        item
    }
}

/// A reader that hashes the bytes it passes on (SHA-256).
pub struct Sha256Reader<R> {
    inner: R,
    engine: sha256::HashEngine,
}

impl<R: Read> Sha256Reader<R> {
    pub fn new(inner: R) -> Sha256Reader<R> {
        Sha256Reader {
            inner,
            engine: sha256::Hash::engine(),
        }
    }

    /// Reads the rest of the input, then returns the hash of everything read, as lowercase hex
    /// ([`super::store::sha256_file`]'s form).
    pub fn finish(mut self) -> std::io::Result<String> {
        std::io::copy(&mut self, &mut std::io::sink())?;
        Ok(sha256::Hash::from_engine(self.engine).to_string())
    }
}

impl<R: Read> Read for Sha256Reader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.engine.input(&buf[..n]);
        Ok(n)
    }
}

/// What the pre-pass found in a stored `blocks.dat`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// The blocks in the file, genesis included.
    pub blocks: usize,
    /// The last block's hash.
    pub last: BlockHash,
    /// The decompressed file's SHA-256, in lowercase hex as `extract.json` holds it.
    pub sha256: String,
}

/// The streaming pre-pass over `node`'s `blocks.dat` (CH3a D148), before V starts: the
/// decompressed file must hash to `extract.json`'s `blocks_dat_sha256` ([`Node::verify`]'s rule)
/// and hold `genesis` first and nowhere else. `each` sees every block in file order until the
/// first error. A hash that differs is reported before a framing or shape error, as
/// [`Node::verify`] runs before the shape check.
pub(crate) fn prepass(
    node: &Node,
    json: &serde_json::Value,
    magic: [u8; 4],
    genesis: BlockHash,
    mut each: impl FnMut(&Block),
) -> Result<Shape> {
    let path = node.blocks();
    let Some(want) = json["blocks_dat_sha256"].as_str() else {
        bail!("{}: extract.json has no sha256 for blocks.dat", node.label());
    };
    if !path.exists() {
        bail!("{}: blocks.dat is not stored", node.label());
    }
    let mut frames = Frames::new(Sha256Reader::new(open(&path)?), magic);
    let mut count = 0;
    let mut last = None;
    let shape = loop {
        let block = match frames.next() {
            None => break Ok(()),
            Some(Err(e)) => break Err(e),
            Some(Ok(block)) => block,
        };
        let hash = block.hash();
        if count == 0 && hash != genesis {
            break Err(anyhow!("blocks.dat block 0 is not genesis"));
        }
        if count > 0 && hash == genesis {
            break Err(anyhow!("blocks.dat block {count} is genesis again"));
        }
        each(&block);
        count += 1;
        last = Some(hash);
    };
    let have =
        frames.into_inner().finish().with_context(|| format!("reading {}", path.display()))?;
    if have != want {
        bail!("{}: sha256 {have}, extract.json says {want}", path.display());
    }
    shape?;
    let Some(last) = last else {
        bail!("blocks.dat holds no block");
    };
    Ok(Shape {
        blocks: count,
        last,
        sha256: have,
    })
}

/// A stored file's frames, hashed as they are read.
pub(crate) type FileFrames = Frames<Sha256Reader<Box<dyn BufRead + Send>>>;

/// The replay pass over `node`'s `blocks.dat`: its blocks in file order, hashed again as they are
/// read so that the end of the pass can check the file is the one the pre-pass checked
/// ([`finish_pass`]).
pub(crate) fn replay_pass(node: &Node, magic: [u8; 4]) -> Result<FileFrames> {
    Ok(Frames::new(Sha256Reader::new(open(&node.blocks())?), magic))
}

/// The end of a replay pass that read `read` blocks: the stream must end where the pre-pass's
/// did, on the same bytes.
pub(crate) fn finish_pass(
    path: &Path,
    frames: FileFrames,
    shape: &Shape,
    read: usize,
) -> Result<()> {
    let have =
        frames.into_inner().finish().with_context(|| format!("reading {}", path.display()))?;
    ensure!(
        read == shape.blocks && have == shape.sha256,
        "{} changed between the pre-pass and the replay",
        path.display()
    );
    Ok(())
}

/// The blocks of a whole `-loadblock` file held in memory, in order (a test's convenience; the
/// replays stream).
pub fn frames(bytes: &[u8], magic: [u8; 4]) -> Result<Vec<Arc<Block>>> {
    Frames::new(bytes, magic).collect()
}

#[cfg(test)]
mod tests {
    //! The reader on synthetic frames (the corpus framing cases are in `tests/digest_replay.rs`).

    use std::io::Read;

    use super::*;

    const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];

    /// A frame of `body` with `size` declared.
    fn frame(size: u32, body: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        out.extend(size.to_le_bytes());
        out.extend(body);
        out
    }

    fn err(bytes: &[u8]) -> String {
        match frames(bytes, MAGIC) {
            Ok(b) => panic!("accepted {} blocks", b.len()),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn an_empty_stream_has_no_frame() {
        assert!(frames(&[], MAGIC).unwrap().is_empty());
    }

    /// A size outside `80..=MaxBlockSize()` is refused before any of the block is read: Core's
    /// bounds, both ends inclusive.
    #[test]
    fn a_size_outside_loadblocks_bounds_is_refused() {
        for size in [0, 79, MAX_FRAME + 1, u32::MAX] {
            assert_eq!(
                err(&frame(size, &[])),
                format!(
                    "blocks.dat offset 0: a frame of {size} bytes, outside -loadblock's \
                     80..=2000000"
                )
            );
        }
        // The bounds themselves pass the size check and fail later, on the missing bytes.
        for size in [MIN_FRAME, MAX_FRAME] {
            assert_eq!(err(&frame(size, &[])), "blocks.dat offset 0: a cut block");
        }
    }

    /// A declared size larger than the stream does not allocate it.
    #[test]
    fn a_cut_block_reads_only_what_is_there() {
        let bytes = frame(MAX_FRAME, &[0; 100]);
        let mut f = Frames::new(&bytes[..], MAGIC);
        assert!(f.next().unwrap().is_err());
        assert!(f.buf.capacity() < 1 << 16, "{}", f.buf.capacity());
        assert!(f.next().is_none(), "the first error ends the frames");
    }

    /// An I/O error is the iteration's error, with what was being read. It is not retried: the
    /// reader fails the test if it is read again.
    #[test]
    fn a_read_error_ends_the_frames() {
        struct Failing(bool);
        impl Read for Failing {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                assert!(!std::mem::replace(&mut self.0, true), "read again after an error");
                Err(std::io::Error::other("planted"))
            }
        }
        let mut f = Frames::new(Failing(false), MAGIC);
        let Some(Err(e)) = f.next() else {
            panic!("no error")
        };
        assert_eq!(format!("{e:#}"), "reading blocks.dat: planted");
        assert!(f.next().is_none());
    }

    /// A reader that returns one byte per call, and is interrupted before each: the frame
    /// header and the block are read whole across calls.
    #[test]
    fn short_and_interrupted_reads_are_resumed() {
        struct Trickle<'a>(&'a [u8], bool);
        impl Read for Trickle<'_> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                self.1 = !self.1;
                if self.1 {
                    return Err(ErrorKind::Interrupted.into());
                }
                let Some((first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                buf[0] = *first;
                self.0 = rest;
                Ok(1)
            }
        }
        let bytes = frame(80, &[0; 80]);
        // 80 zero bytes are a header and no transaction count: a decode error, at offset 0,
        // after the whole frame was read.
        let mut f = Frames::new(Trickle(&bytes, false), MAGIC);
        let Some(Err(e)) = f.next() else {
            panic!("no error")
        };
        let e = format!("{e:#}");
        assert!(e.starts_with("blocks.dat offset 0: "), "{e}");
        assert!(!e.contains("cut"), "{e}");
    }

    #[test]
    fn the_sha256_reader_hashes_what_it_passes_and_the_rest() {
        let data = b"some bytes to hash".repeat(1000);
        let mut r = Sha256Reader::new(&data[..]);
        // One read passes on what the inner reader gave, all of it.
        let mut first = [0; 10];
        assert_eq!(r.read(&mut first).unwrap(), 10);
        assert_eq!(&first, &data[..10]);
        assert_eq!(r.finish().unwrap(), super::super::store::sha256_hex(&data));
    }

    /// The end of a replay pass: the frames read and the stream's hash must be the pre-pass's.
    #[test]
    fn a_pass_that_differs_from_the_pre_pass_is_refused() {
        let pass = |bytes: &'static [u8]| -> FileFrames {
            Frames::new(Sha256Reader::new(Box::new(bytes)), MAGIC)
        };
        let shape = Shape {
            blocks: 0,
            last: BlockHash::from([0; 32]),
            sha256: super::super::store::sha256_hex(b""),
        };
        let path = Path::new("blocks.dat");
        finish_pass(path, pass(b""), &shape, 0).unwrap();
        let changed = "blocks.dat changed between the pre-pass and the replay";
        let e = finish_pass(path, pass(b"x"), &shape, 0).unwrap_err();
        assert_eq!(format!("{e:#}"), changed);
        let e = finish_pass(path, pass(b""), &shape, 1).unwrap_err();
        assert_eq!(format!("{e:#}"), changed);
    }
}
