use std::io::Read;
use std::path::Path;

use super::{MAX_ALLOCATION, crc32c, invalid, open_file};
use crate::{Error, Result};

const BLOCK_SIZE: usize = 32768;

/// How a log's end is read. `Tolerant` takes LevelDB's two end-of-file cases as the end of the
/// log instead of errors: a header cut short by the end of the file, and a payload cut short by
/// it. A fragmented record still open there is dropped with it. Everything else is as `Strict`.
// Core: src/leveldb/db/log_reader.cc:207-212, :221-232, :144-151 at v24.0.2
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tail {
    Strict,
    Tolerant,
}

pub(super) fn visit(
    path: &Path,
    tail: Tail,
    mut record: impl FnMut(&[u8], u64) -> Result<()>,
) -> Result<()> {
    let mut file = open_file(path)?;
    let mut block = [0u8; BLOCK_SIZE];
    let mut base = 0;
    let mut assembled = Vec::new();
    let mut start = None;
    loop {
        let mut len = 0;
        while len < BLOCK_SIZE {
            let n = file.read(&mut block[len..]).map_err(|e| Error::io(path, e))?;
            if n == 0 {
                break;
            }
            len += n;
        }
        if len == 0 {
            break;
        }
        let mut pos = 0;
        while pos < len {
            let offset = base + pos as u64;
            if len - pos < 7 {
                if len != BLOCK_SIZE && tail == Tail::Tolerant {
                    return Ok(());
                }
                if len != BLOCK_SIZE || block[pos..len].iter().any(|&b| b != 0) {
                    return Err(invalid(path, offset, "torn log header"));
                }
                break;
            }
            let header = &block[pos..pos + 7];
            let crc = u32::from_le_bytes(
                header[..4].try_into().map_err(|_| invalid(path, offset, "log checksum"))?,
            );
            let size = u16::from_le_bytes([header[4], header[5]]) as usize;
            let kind = header[6];
            if kind == 0 && size == 0 && crc == 0 {
                if len != BLOCK_SIZE || block[pos..len].iter().any(|&b| b != 0) {
                    return Err(invalid(path, offset, "torn or nonzero log padding"));
                }
                break;
            }
            let end = pos + 7 + size;
            if end > len {
                if len != BLOCK_SIZE && tail == Tail::Tolerant {
                    return Ok(());
                }
                return Err(invalid(path, offset, "torn log payload"));
            }
            let data = &block[pos + 7..end];
            if crc != crc32c::mask(crc32c::extend(crc32c::extend(0, &[kind]), data)) {
                return Err(invalid(path, offset, "log checksum mismatch"));
            }
            match kind {
                1 if start.is_none() => record(data, offset + 7)?,
                2 if start.is_none() => {
                    assembled.clear();
                    assembled.extend_from_slice(data);
                    start = Some(offset + 7);
                }
                3 | 4 if start.is_some() => {
                    if assembled.len() + data.len() > MAX_ALLOCATION {
                        return Err(invalid(path, offset, "log record exceeds 64 MiB"));
                    }
                    assembled.extend_from_slice(data);
                    if kind == 4 {
                        record(
                            &assembled,
                            start.take().ok_or_else(|| invalid(path, offset, "missing FIRST"))?,
                        )?;
                        assembled.clear();
                    }
                }
                _ => return Err(invalid(path, offset, "invalid log fragment type or order")),
            }
            pos = end;
        }
        base += len as u64;
    }
    if start.is_some() {
        return Err(invalid(path, base, "torn fragmented log record"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut out =
            crc32c::mask(crc32c::extend(crc32c::extend(0, &[kind]), data)).to_le_bytes().to_vec();
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(data);
        out
    }

    fn decode(data: &[u8]) -> Result<Vec<Vec<u8>>> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("000001.log");
        std::fs::write(&path, data).unwrap();
        let mut records = Vec::new();
        visit(&path, Tail::Strict, |record, _| {
            records.push(record.to_vec());
            Ok(())
        })?;
        Ok(records)
    }

    #[test]
    fn full_first_middle_last_and_boundary_padding() {
        let mut bytes = physical(2, &vec![1; BLOCK_SIZE - 7]);
        bytes.extend(physical(3, &vec![2; BLOCK_SIZE - 7]));
        bytes.extend(physical(4, &[3; 9]));
        let mut value = vec![1; BLOCK_SIZE - 7];
        value.extend(vec![2; BLOCK_SIZE - 7]);
        value.extend([3; 9]);
        assert_eq!(decode(&bytes).unwrap(), [value]);
        assert!(decode(&bytes[..2 * BLOCK_SIZE]).is_err());

        let mut padded = physical(1, &vec![4; BLOCK_SIZE - 13]);
        padded.extend([0; 6]);
        padded.extend(physical(1, b"after padding"));
        assert_eq!(decode(&padded).unwrap(), [vec![4; BLOCK_SIZE - 13], b"after padding".to_vec()]);

        let mut empty_first = physical(1, &vec![5; BLOCK_SIZE - 14]);
        empty_first.extend(physical(2, &[]));
        empty_first.extend(physical(4, b"empty FIRST is legal"));
        assert_eq!(decode(&empty_first).unwrap()[1], b"empty FIRST is legal");
    }

    #[test]
    fn corrupt_torn_and_misordered_records_are_errors() {
        let bytes = physical(1, b"valid");
        for n in 1..bytes.len() {
            assert!(decode(&bytes[..n]).is_err());
        }
        let mut bad = bytes.clone();
        bad[7] ^= 1;
        assert!(decode(&bad).is_err());
        for kind in [0, 3, 4, 5, 255] {
            assert!(decode(&physical(kind, b"bad type or order")).is_err());
        }
        let mut unfinished = physical(2, b"first");
        unfinished.extend(physical(1, b"full before last"));
        assert!(decode(&unfinished).is_err());
    }
}
