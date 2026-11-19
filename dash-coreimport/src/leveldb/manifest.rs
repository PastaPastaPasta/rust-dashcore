use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{
    Cursor, MAX_ALLOCATION, MAX_SEQUENCE, compare_internal, internal_key, invalid, log, open_file,
};
use crate::{Error, Result};

pub(super) struct TableFile {
    pub path: PathBuf,
    pub level: u32,
    pub number: u64,
    pub size: u64,
    pub smallest: Vec<u8>,
    pub largest: Vec<u8>,
}

pub(super) struct Version {
    pub log_number: u64,
    pub prev_log_number: u64,
    pub last_sequence: u64,
    tables: BTreeMap<(u32, u64), TableFile>,
}

pub(super) fn read(path: &Path) -> Result<Version> {
    let mut version = Version {
        log_number: 0,
        prev_log_number: 0,
        last_sequence: 0,
        tables: BTreeMap::new(),
    };
    let mut retained_keys = 0usize;
    let mut comparator_seen = false;
    let mut log_seen = false;
    log::visit(path, |record, offset| {
        let mut cursor = Cursor::new(record, path, offset);
        while !cursor.done() {
            match cursor.varint32()? {
                1 => {
                    if cursor.slice()? != b"leveldb.BytewiseComparator" {
                        return Err(cursor.error("unsupported comparator"));
                    }
                    comparator_seen = true;
                }
                2 => {
                    version.log_number = cursor.varint()?;
                    log_seen = true;
                }
                3 => {
                    cursor.varint()?;
                }
                4 => {
                    version.last_sequence = cursor.varint()?;
                    if version.last_sequence > MAX_SEQUENCE {
                        return Err(cursor.error("manifest sequence overflow"));
                    }
                }
                5 => {
                    level(&mut cursor)?;
                    key(&mut cursor)?;
                }
                6 => {
                    let level = level(&mut cursor)?;
                    let number = cursor.varint()?;
                    if let Some(removed) = version.tables.remove(&(level, number)) {
                        retained_keys -= removed.smallest.len() + removed.largest.len();
                    }
                }
                7 => {
                    let level = level(&mut cursor)?;
                    let number = cursor.varint()?;
                    let size = cursor.varint()?;
                    let smallest = key(&mut cursor)?;
                    let largest = key(&mut cursor)?;
                    if version.tables.len() >= 262_144 {
                        return Err(cursor.error("live table limit exceeded"));
                    }
                    let key_bytes = smallest.len() + largest.len();
                    if key_bytes > MAX_ALLOCATION - retained_keys {
                        return Err(cursor.error("retained manifest key limit exceeded"));
                    }
                    retained_keys += key_bytes;
                    if number == 0 || size < 48 || compare_internal(smallest, largest).is_gt() {
                        return Err(cursor.error("invalid new-file metadata"));
                    }
                    let file = TableFile {
                        path: PathBuf::new(),
                        level,
                        number,
                        size,
                        smallest: smallest.to_vec(),
                        largest: largest.to_vec(),
                    };
                    if version.tables.insert((level, number), file).is_some() {
                        return Err(cursor.error("duplicate live table"));
                    }
                }
                9 => {
                    version.prev_log_number = cursor.varint()?;
                }
                tag => return Err(cursor.error(format!("unsupported manifest tag {tag}"))),
            }
        }
        Ok(())
    })?;
    if !comparator_seen || !log_seen || version.log_number == 0 {
        return Err(invalid(path, 0, "manifest lacks comparator or log number"));
    }
    let mut numbers = std::collections::BTreeSet::new();
    for table in version.tables.values() {
        if !numbers.insert(table.number) {
            return Err(invalid(path, 0, "table number reused across levels"));
        }
    }
    for level in 1..7 {
        let mut files: Vec<_> = version.tables.values().filter(|f| f.level == level).collect();
        files.sort_by(|a, b| compare_internal(&a.smallest, &b.smallest));
        if files.windows(2).any(|w| compare_internal(&w[0].largest, &w[1].smallest).is_ge()) {
            return Err(invalid(path, 0, "overlapping tables above L0"));
        }
    }
    Ok(version)
}

fn level(cursor: &mut Cursor<'_>) -> Result<u32> {
    let level = cursor.varint32()?;
    if level >= 7 {
        return Err(cursor.error("unsupported level"));
    }
    Ok(level)
}

fn key<'a>(cursor: &mut Cursor<'a>) -> Result<&'a [u8]> {
    let bytes = cursor.slice()?;
    internal_key(bytes, cursor.path, cursor.base + cursor.pos as u64)?;
    Ok(bytes)
}

impl Version {
    pub fn resolve(self, dir: &Path) -> Result<Vec<TableFile>> {
        let mut tables = Vec::new();
        for mut table in self.tables.into_values() {
            let ldb = dir.join(format!("{:06}.ldb", table.number));
            table.path = match std::fs::symlink_metadata(&ldb) {
                Ok(_) => ldb,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    dir.join(format!("{:06}.sst", table.number))
                }
                Err(e) => return Err(Error::io(&ldb, e)),
            };
            let size =
                open_file(&table.path)?.metadata().map_err(|e| Error::io(&table.path, e))?.len();
            if size != table.size {
                return Err(invalid(&table.path, 0, "table size differs from manifest"));
            }
            tables.push(table);
        }
        tables.sort_by(|a, b| {
            a.level.cmp(&b.level).then_with(|| {
                if a.level == 0 {
                    b.number.cmp(&a.number)
                } else {
                    compare_internal(&a.smallest, &b.smallest)
                }
            })
        });
        Ok(tables)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::leveldb::crc32c;

    fn decode(record: &[u8]) -> Result<Version> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("MANIFEST-000001");
        let crc = crc32c::mask(crc32c::extend(crc32c::extend(0, &[1]), record));
        let mut data = crc.to_le_bytes().to_vec();
        data.extend_from_slice(&(record.len() as u16).to_le_bytes());
        data.push(1);
        data.extend_from_slice(record);
        std::fs::write(&path, data).unwrap();
        read(&path)
    }

    #[test]
    fn unsupported_and_incomplete_manifests_are_errors() {
        let name = b"leveldb.BytewiseComparator";
        let mut record = vec![1, name.len() as u8];
        record.extend(name);
        record.extend([2, 1, 3, 2, 4, 0, 9, 0]);
        assert!(decode(&record).is_ok());
        let mut unknown = record.clone();
        unknown.push(8);
        assert!(decode(&unknown).is_err());
        let mut comparator = record.clone();
        comparator[2] = b'L';
        assert!(decode(&comparator).is_err());
        assert!(decode(&[]).is_err());
        let mut level = record.clone();
        level.extend([6, 7, 1]);
        assert!(decode(&level).is_err());
        let mut short_key = record;
        short_key.extend([7, 0, 2, 48, 0, 0]);
        assert!(decode(&short_key).is_err());
    }
}
