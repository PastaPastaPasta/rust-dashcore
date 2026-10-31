//! `undo_decode` (CH3 §7.1; CH3a a5): the undo read's stream (`read_undo_stream`, ST2's
//! `read_undo_record` over an open file) over arbitrary bytes. Its two paths, a first read of the
//! size field's length and the doubling read when the field is no use, never panic and agree:
//! whatever the size field says and whatever the first read, the outcome is the reference's, the
//! shape of Core's stream (`verifier >> blockundo; filein >> hashChecksum`,
//! `node/blockstorage.cpp:552-563`) over the whole file: the decode from the record's position,
//! then 32 checksum bytes right after what it consumed. The reference decodes with the same
//! `BlockUndoRef`, so this checks the read, not the decoder: the decoder's parity with Core's is
//! `diff_undo_decode`'s, and the log lines are D107's oracle's. The buffer never outgrows the
//! file, and a decoded record round-trips.
//!
//! The input is `flags (1) ‖ first (1) ‖ size A (4, LE) ‖ size B (4, LE) ‖ tail`. The file is
//! `magic ‖ size field ‖ tail` with the record at byte 8, or, with flags bit 2, two bytes then the
//! tail with the record at byte 2 (no size field before it). Bit 0 writes the right checksum after
//! the reference's decode, so that the checksum passes; bit 1 makes size A the reference's
//! consumed length (an honest field). The stream runs twice: with size A and a first read of
//! `first` bytes (64 KiB for 0), and with size B and 64 KiB.
//!
//! The round trip is checked where Core's compression keeps every amount, as in
//! `packed_coin_roundtrip`: `DecompressAmount(CompressAmount(v)) == v` fails only past
//! `MAX_MONEY`, once the arithmetic wraps, so no record Core writes is left out.

use std::io;

use dash_consensus::coins::compress::{compress_amount, decompress_amount};
use dash_consensus::coins::{BlockUndo, BlockUndoRef};
use dash_consensus::primitives::ReadErrorKind;
use dashd_store::flatfile::{
    FlatRead, UNDO_CHECKSUM_SIZE, UNDO_STREAM_FIRST_READ, read_undo_stream, undo_checksum,
};
use node_l1::Divergence;

const PREV: [u8; 32] = [0x5a; 32];
const MAGIC: [u8; 4] = [0xbf, 0x0c, 0x6b, 0xbd];
const CHECKSUM: usize = UNDO_CHECKSUM_SIZE as usize;

/// A file in memory.
struct Mem(Vec<u8>);

impl FlatRead for Mem {
    fn len(&self) -> io::Result<u64> {
        Ok(self.0.len() as u64)
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let at =
            usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let bytes = at
            .checked_add(buf.len())
            .and_then(|end| self.0.get(at..end))
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}

/// Core's read of the record at the start of `tail`: the record and the length it consumed, or
/// the line `UndoReadFromDisk` logs after its prefix.
fn reference(tail: &[u8]) -> (Result<BlockUndo, String>, Option<usize>) {
    let eof = "Deserialize or I/O error - AutoFile::read: end of file: iostream error".to_owned();
    let mut rest = tail;
    match BlockUndoRef::decode(&mut rest) {
        Err(e) if e.kind == ReadErrorKind::EndOfData => (Err(eof), None),
        Err(e) => (Err(format!("Deserialize or I/O error - {}: iostream error", e.kind)), None),
        Ok(undo) => {
            let consumed = tail.len() - rest.len();
            let out = if rest.len() < CHECKSUM {
                Err(eof)
            } else if undo_checksum(&PREV, &tail[..consumed])[..] != rest[..CHECKSUM] {
                Err("Checksum mismatch".to_owned())
            } else {
                Ok(undo.to_block_undo())
            };
            (out, Some(consumed))
        }
    }
}

fn file(flags: u8, size: u32, tail: &[u8]) -> (Mem, u32) {
    if flags & 4 != 0 {
        return (Mem([&[0, 0][..], tail].concat()), 2);
    }
    (Mem([&MAGIC[..], &size.to_le_bytes(), tail].concat()), 8)
}

fn read(
    flags: u8,
    size: u32,
    tail: &[u8],
    first: u64,
) -> Result<Result<BlockUndo, String>, Divergence> {
    let (mem, pos) = file(flags, size, tail);
    let mut buf = Vec::new();
    let out = read_undo_stream(&mem, pos, &PREV, &mut buf, first, |u| u.to_block_undo());
    if buf.len() > tail.len() {
        return Err(Divergence {
            field: "the buffer outgrew the file",
        });
    }
    Ok(out.map_err(|e| e.to_string()))
}

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let Some((head, tail)) = data.split_at_checked(10) else {
        return Ok(());
    };
    let flags = head[0];
    let first = match head[1] {
        0 => UNDO_STREAM_FIRST_READ,
        n => u64::from(n),
    };
    let field = |b: &[u8]| u32::from_le_bytes(b.try_into().unwrap_or_default());
    let (mut size_a, size_b) = (field(&head[2..6]), field(&head[6..10]));
    let mut tail = tail.to_vec();
    let (_, consumed) = reference(&tail);
    if let Some(consumed) = consumed {
        if flags & 1 != 0 {
            tail.resize(tail.len().max(consumed + CHECKSUM), 0);
            let sum = undo_checksum(&PREV, &tail[..consumed]);
            tail[consumed..consumed + CHECKSUM].copy_from_slice(&sum);
        }
        if flags & 2 != 0 {
            size_a = u32::try_from(consumed).unwrap_or(u32::MAX);
        }
    }
    let (want, _) = reference(&tail);

    let a = read(flags, size_a, &tail, first)?;
    let b = read(flags, size_b, &tail, UNDO_STREAM_FIRST_READ)?;
    if a != want {
        return Err(Divergence {
            field: "size A against Core's stream",
        });
    }
    if b != want {
        return Err(Divergence {
            field: "size B against Core's stream",
        });
    }
    let amounts_kept = |undo: &BlockUndo| {
        let mut coins = undo.txs.iter().flat_map(|tx| &tx.coins);
        coins.all(|c| decompress_amount(compress_amount(c.value() as u64)) == c.value() as u64)
    };
    if let Ok(undo) = &want
        && amounts_kept(undo)
    {
        let bytes = undo.core_bytes();
        let again = BlockUndo::decode(&mut bytes.as_slice()).map_err(|_| Divergence {
            field: "re-encoded record does not decode",
        })?;
        if &again != undo {
            return Err(Divergence {
                field: "record round trip",
            });
        }
    }
    Ok(())
}
