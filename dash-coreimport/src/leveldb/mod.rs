//! Read-only LevelDB point lookups for a stopped or externally locked Core datadir.
//!
//! No database library is opened, no lock is taken and no recovery is performed. The caller
//! must keep the directory stable throughout the reader's lifetime. Torn logs and unsupported
//! comparators or compression are errors. Individual records and blocks are limited to 64 MiB.

mod batch;
pub(crate) mod crc32c;
mod log;
mod manifest;
mod table;

use std::cmp::Ordering;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

const MAX_ALLOCATION: usize = 64 * 1024 * 1024;
const MAX_KEY: usize = 64 * 1024;
const MAX_SEQUENCE: u64 = (1 << 56) - 1;

fn invalid(path: &Path, offset: u64, detail: impl std::fmt::Display) -> Error {
    Error::LevelDb {
        path: path.to_owned(),
        detail: format!("byte {offset}: {detail}"),
    }
}

pub(crate) fn open_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    if !file.metadata().map_err(|e| Error::io(path, e))?.is_file() {
        return Err(invalid(path, 0, "expected a regular file, without symlinks"));
    }
    Ok(file)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    path: &'a Path,
    base: u64,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], path: &'a Path, base: u64) -> Self {
        Self {
            data,
            pos: 0,
            path,
            base,
        }
    }

    fn error(&self, detail: impl std::fmt::Display) -> Error {
        invalid(self.path, self.base + self.pos as u64, detail)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or_else(|| self.error("length overflow"))?;
        let bytes = self.data.get(self.pos..end).ok_or_else(|| self.error("truncated field"))?;
        self.pos = end;
        Ok(bytes)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0;
        for shift in (0..70).step_by(7) {
            let byte = self.take(1)?[0];
            if shift == 63 && byte > 1 {
                return Err(self.error("varint overflow"));
            }
            value |= u64::from(byte & 127) << shift;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(self.error("varint overflow"))
    }

    fn varint32(&mut self) -> Result<u32> {
        let mut value = 0;
        for shift in (0..35).step_by(7) {
            let byte = self.take(1)?[0];
            if shift == 28 && byte > 15 {
                return Err(self.error("varint32 overflow"));
            }
            value |= u32::from(byte & 127) << shift;
            if byte < 128 {
                return Ok(value);
            }
        }
        Err(self.error("varint32 overflow"))
    }

    fn slice(&mut self) -> Result<&'a [u8]> {
        let len = self.varint32()? as usize;
        self.take(len)
    }

    fn done(&self) -> bool {
        self.pos == self.data.len()
    }
}

fn internal_key<'a>(bytes: &'a [u8], path: &Path, offset: u64) -> Result<(&'a [u8], u64, u8)> {
    if bytes.len() > MAX_KEY {
        return Err(invalid(path, offset, "internal key exceeds 64 KiB"));
    }
    let split =
        bytes.len().checked_sub(8).ok_or_else(|| invalid(path, offset, "short internal key"))?;
    let tag = u64::from_le_bytes(
        bytes[split..].try_into().map_err(|_| invalid(path, offset, "internal key tag"))?,
    );
    let kind = tag as u8;
    if kind > 1 {
        return Err(invalid(path, offset, "unsupported internal key type"));
    }
    Ok((&bytes[..split], tag >> 8, kind))
}

fn compare_internal(a: &[u8], b: &[u8]) -> Ordering {
    let (a_key, a_tag) = a.split_at(a.len() - 8);
    let (b_key, b_tag) = b.split_at(b.len() - 8);
    a_key.cmp(b_key).then_with(|| b_tag.iter().rev().cmp(a_tag.iter().rev()))
}

#[derive(Debug, PartialEq, Eq)]
struct Value {
    sequence: u64,
    bytes: Option<Vec<u8>>,
}

fn newest(found: &mut Option<Value>, value: Value, path: &Path) -> Result<()> {
    if let Some(old) = found.as_ref() {
        if old.sequence > value.sequence {
            return Ok(());
        }
        if old.sequence == value.sequence && old != &value {
            return Err(invalid(path, 0, "conflicting values at the same sequence"));
        }
    }
    *found = Some(value);
    Ok(())
}

/// A point reader over the manifest's live tables and recoverable WAL files.
pub struct LevelDbReader {
    tables: Vec<table::Table>,
    logs: Vec<PathBuf>,
    last_sequence: u64,
    tail: log::Tail,
}

impl LevelDbReader {
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with(dir, log::Tail::Strict)
    }

    /// [`open`](Self::open), except that each WAL ends where LevelDB's recovery ends it: a
    /// header or a payload cut short by the end of the file ends that file, and a fragmented
    /// record still open there is dropped. A checksum failure, a bad fragment order or a torn
    /// manifest is still an error. For a datadir a writer may have been copied from mid-append.
    pub fn open_tolerant(dir: &Path) -> Result<Self> {
        Self::open_with(dir, log::Tail::Tolerant)
    }

    fn open_with(dir: &Path, tail: log::Tail) -> Result<Self> {
        let current_path = dir.join("CURRENT");
        let mut current = Vec::new();
        open_file(&current_path)?
            .take(129)
            .read_to_end(&mut current)
            .map_err(|e| Error::io(&current_path, e))?;
        if current.len() > 128 {
            return Err(invalid(&current_path, 0, "CURRENT exceeds 128 bytes"));
        }
        let name = std::str::from_utf8(&current)
            .map_err(|_| invalid(&current_path, 0, "non-UTF8 CURRENT"))?;
        let name = name
            .strip_suffix('\n')
            .ok_or_else(|| invalid(&current_path, 0, "CURRENT lacks newline"))?;
        let number = name
            .strip_prefix("MANIFEST-")
            .ok_or_else(|| invalid(&current_path, 0, "invalid manifest filename"))?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid(&current_path, 0, "invalid manifest filename"));
        }
        let version = manifest::read(&dir.join(name))?;
        let mut logs = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| Error::io(dir, e))? {
            let entry = entry.map_err(|e| Error::io(dir, e))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(number) = name.strip_suffix(".log") {
                if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid(&entry.path(), 0, "invalid WAL filename"));
                }
                let number = number
                    .parse::<u64>()
                    .map_err(|_| invalid(&entry.path(), 0, "WAL number overflow"))?;
                if number >= version.log_number
                    || (version.prev_log_number != 0 && number == version.prev_log_number)
                {
                    logs.push((number, entry.path()));
                }
            }
        }
        if !logs.iter().any(|(number, _)| *number == version.log_number)
            || (version.prev_log_number != 0
                && !logs.iter().any(|(number, _)| *number == version.prev_log_number))
        {
            return Err(invalid(dir, 0, "manifest-referenced WAL is missing"));
        }
        for (_, path) in &logs {
            open_file(path)?;
        }
        logs.sort_by_key(|entry| entry.0);
        let mut last_sequence = version.last_sequence;
        let mut previous = None;
        for (_, path) in &logs {
            log::visit(path, tail, |record, offset| {
                let (start, end) = batch::lookup(record, path, offset, b"", &mut None)?;
                if previous.is_some_and(|sequence| start <= sequence) {
                    return Err(invalid(
                        path,
                        offset,
                        "batch sequences not strictly increasing in replay order",
                    ));
                }
                previous = Some(start.max(end));
                last_sequence = last_sequence.max(end);
                Ok(())
            })?;
        }
        let logs = logs.into_iter().map(|(_, path)| path).collect();
        let tables =
            version.resolve(dir)?.into_iter().map(table::Table::open).collect::<Result<_>>()?;
        Ok(Self {
            tables,
            logs,
            last_sequence,
            tail,
        })
    }

    pub fn get(&self, user_key: &[u8]) -> Result<Option<Vec<u8>>> {
        let mut found = None;
        for path in &self.logs {
            log::visit(path, self.tail, |record, offset| {
                batch::lookup(record, path, offset, user_key, &mut found).map(|_| ())
            })?;
        }
        for table in &self.tables {
            if table.covers(user_key)
                && let Some(value) = table.lookup(user_key, self.last_sequence)?
            {
                if let Some(winner) = &found {
                    if value.sequence > winner.sequence {
                        return Err(invalid(
                            table.path(),
                            0,
                            "lower-priority source has higher sequence",
                        ));
                    }
                    if value.sequence == winner.sequence && value != *winner {
                        return Err(invalid(
                            table.path(),
                            0,
                            "conflicting values at the same sequence",
                        ));
                    }
                } else {
                    found = Some(value);
                }
            }
        }
        Ok(found.and_then(|value| value.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_rejects_overflow_and_truncation() {
        let path = Path::new("cursor");
        assert_eq!(Cursor::new(&[255, 1], path, 0).varint().unwrap(), 255);
        for bytes in [&[128][..], &[255; 10], &[128, 128, 128, 128, 128, 128, 128, 128, 128, 2]] {
            assert!(Cursor::new(bytes, path, 0).varint().is_err());
        }
        assert_eq!(Cursor::new(&[255, 255, 255, 255, 15], path, 0).varint32().unwrap(), u32::MAX);
        assert_eq!(Cursor::new(&[128, 128, 128, 128, 0], path, 0).varint32().unwrap(), 0);
        for len in 6..=10 {
            let mut bytes = vec![128; len - 1];
            bytes.push(0);
            assert!(Cursor::new(&bytes, path, 0).varint32().is_err());
        }
        assert!(Cursor::new(&[255, 255, 255, 255, 31], path, 0).varint32().is_err());
        assert!(Cursor::new(&[255, 255, 255, 255, 15], path, 0).slice().is_err());
        assert!(Cursor::new(&[], path, 0).take(usize::MAX).is_err());
        assert!(internal_key(&[0; 7], path, 0).is_err());
        assert!(internal_key(&[255; 8], path, 0).is_err());
    }
}
