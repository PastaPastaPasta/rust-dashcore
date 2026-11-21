use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::Path;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    Cursor, MAX_ALLOCATION, MAX_KEY, Value, compare_internal, crc32c, internal_key, invalid,
    manifest::TableFile, newest, open_file,
};
use crate::{Error, Result};

const MAGIC: [u8; 8] = 0xdb4775248b80fb57u64.to_le_bytes();

#[cfg(test)]
static ENTRY_WORK: AtomicUsize = AtomicUsize::new(0);

#[inline]
fn record_entry_work(bytes: usize) {
    #[cfg(test)]
    ENTRY_WORK.fetch_add(bytes, Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = bytes;
}

#[derive(Clone, Copy)]
struct Handle {
    offset: u64,
    size: u64,
}

fn handle(cursor: &mut Cursor<'_>) -> Result<Handle> {
    Ok(Handle {
        offset: cursor.varint()?,
        size: cursor.varint()?,
    })
}

fn read_at(file: &File, path: &Path, offset: u64, bytes: &mut [u8]) -> Result<()> {
    file.read_exact_at(bytes, offset).map_err(|e| Error::io(path, e))
}

fn block(file: &File, path: &Path, limit: u64, handle: Handle) -> Result<Vec<u8>> {
    let end = handle.offset.checked_add(handle.size).and_then(|n| n.checked_add(5));
    if handle.size > MAX_ALLOCATION as u64 || end.is_none_or(|end| end > limit) {
        return Err(invalid(path, handle.offset, "block handle outside table or over 64 MiB"));
    }
    let mut data = vec![0; handle.size as usize + 5];
    read_at(file, path, handle.offset, &mut data)?;
    let len = handle.size as usize;
    if data[len] != 0 {
        return Err(invalid(
            path,
            handle.offset + handle.size,
            format!("unsupported compression type {}", data[len]),
        ));
    }
    let crc = u32::from_le_bytes(
        data[len + 1..].try_into().map_err(|_| invalid(path, handle.offset, "block checksum"))?,
    );
    if crc != crc32c::mask(crc32c::extend(0, &data[..=len])) {
        return Err(invalid(path, handle.offset, "table checksum mismatch"));
    }
    data.truncate(len);
    Ok(data)
}

struct Bounds {
    smallest: Option<Vec<u8>>,
    largest: Vec<u8>,
}

fn entries(
    data: &[u8],
    path: &Path,
    base: u64,
    internal: bool,
    mut visit: impl FnMut(&[u8], &[u8], u64) -> Result<()>,
) -> Result<Bounds> {
    let count_bytes = data
        .get(data.len().saturating_sub(4)..)
        .ok_or_else(|| invalid(path, base, "short restart block"))?;
    let count = u32::from_le_bytes(
        count_bytes.try_into().map_err(|_| invalid(path, base, "short restart count"))?,
    ) as usize;
    if count == 0 || count > data.len().saturating_sub(4) / 4 {
        return Err(invalid(path, base, "invalid restart count"));
    }
    let end = data.len() - 4 - 4 * count;
    let mut restarts = Vec::with_capacity(count);
    for n in 0..count {
        let offset = end + 4 * n;
        let restart = u32::from_le_bytes(
            data[offset..offset + 4]
                .try_into()
                .map_err(|_| invalid(path, base, "restart offset"))?,
        ) as usize;
        if (n == 0 && restart != 0)
            || (n > 0 && restart <= restarts[n - 1])
            || restart >= end.max(1)
        {
            return Err(invalid(path, base + offset as u64, "invalid restart offset"));
        }
        restarts.push(restart);
    }
    let mut cursor = Cursor::new(&data[..end], path, base);
    let mut key = Vec::new();
    let mut restart_index = 0;
    let mut smallest = None;
    while !cursor.done() {
        let entry_offset = cursor.pos;
        let shared = cursor.varint32()? as usize;
        let unshared = cursor.varint32()? as usize;
        let value_len = cursor.varint32()? as usize;
        if shared > key.len() || shared > MAX_KEY || unshared > MAX_KEY - shared {
            return Err(cursor.error("invalid shared key prefix"));
        }
        if restart_index < count && restarts[restart_index] <= entry_offset {
            if restarts[restart_index] != entry_offset || shared != 0 {
                return Err(cursor.error("restart does not name an unshared entry"));
            }
            restart_index += 1;
        }
        let unshared = cursor.take(unshared)?;
        let order = if entry_offset == 0 {
            None
        } else if internal {
            Some(compare_internal_reconstructed(
                &key,
                shared,
                unshared,
                path,
                base + entry_offset as u64,
            )?)
        } else {
            record_entry_work(key[shared..].len().min(unshared.len()));
            Some(key[shared..].cmp(unshared))
        };
        key.truncate(shared);
        key.extend_from_slice(unshared);
        record_entry_work(unshared.len());
        let value = cursor.take(value_len)?;
        if internal {
            internal_key(&key, path, base + entry_offset as u64)?;
        }
        if let Some(order) = order
            && !order.is_lt()
        {
            return Err(cursor.error("unordered block keys"));
        }
        if smallest.is_none() {
            smallest = Some(key.clone());
        }
        visit(&key, value, base + entry_offset as u64)?;
    }
    if end != 0 && restart_index != count {
        return Err(cursor.error("restart is not an entry boundary"));
    }
    if end == 0 && count != 1 {
        return Err(cursor.error("invalid empty block"));
    }
    Ok(Bounds {
        smallest,
        largest: key,
    })
}

fn compare_internal_reconstructed(
    previous: &[u8],
    shared: usize,
    unshared: &[u8],
    path: &Path,
    offset: u64,
) -> Result<std::cmp::Ordering> {
    let candidate_len = shared
        .checked_add(unshared.len())
        .ok_or_else(|| invalid(path, offset, "key length overflow"))?;
    if previous.len() < 8 || candidate_len < 8 {
        return Err(invalid(path, offset, "short internal key"));
    }
    let previous_user_len = previous.len() - 8;
    let candidate_user_len = candidate_len - 8;
    let common = shared.min(previous_user_len).min(candidate_user_len);
    let candidate_byte = |index: usize| {
        if index < shared {
            previous[index]
        } else {
            unshared[index - shared]
        }
    };
    let mut user_order = std::cmp::Ordering::Equal;
    for (index, &previous_byte) in
        previous.iter().enumerate().take(previous_user_len.min(candidate_user_len)).skip(common)
    {
        let candidate = candidate_byte(index);
        record_entry_work(1);
        user_order = previous_byte.cmp(&candidate);
        if !user_order.is_eq() {
            break;
        }
    }
    if user_order.is_eq() {
        user_order = previous_user_len.cmp(&candidate_user_len);
    }
    if !user_order.is_eq() {
        return Ok(user_order);
    }
    let previous_tag = &previous[previous_user_len..];
    let mut tag_order = std::cmp::Ordering::Equal;
    for index in (0..8).rev() {
        let candidate = candidate_byte(candidate_user_len + index);
        record_entry_work(1);
        tag_order = candidate.cmp(&previous_tag[index]);
        if !tag_order.is_eq() {
            break;
        }
    }
    Ok(tag_order)
}

pub(super) struct Table {
    metadata: TableFile,
    index_handle: Handle,
    meta_handle: Handle,
}

impl Table {
    pub fn path(&self) -> &Path {
        &self.metadata.path
    }

    pub fn covers(&self, user: &[u8]) -> bool {
        let low = &self.metadata.smallest[..self.metadata.smallest.len() - 8];
        let high = &self.metadata.largest[..self.metadata.largest.len() - 8];
        user >= low && user <= high
    }

    pub fn open(metadata: TableFile) -> Result<Self> {
        let path = &metadata.path;
        let file = open_file(path)?;
        let limit = metadata.size - 48;
        let mut footer = [0; 48];
        read_at(&file, path, limit, &mut footer)?;
        if footer[40..] != MAGIC {
            return Err(invalid(path, limit + 40, "invalid table magic"));
        }
        let mut cursor = Cursor::new(&footer[..40], path, limit);
        let meta_handle = handle(&mut cursor)?;
        let index_handle = handle(&mut cursor)?;
        if cursor.data[cursor.pos..].iter().any(|&b| b != 0) {
            return Err(cursor.error("nonzero footer padding"));
        }
        Self::filter(&file, path, limit, meta_handle)?;
        let mut first = None;
        let mut last = None;
        Self::index(&file, path, limit, index_handle, |_, handle| {
            if first.is_none() {
                first = Some(handle);
            }
            last = Some(handle);
            Ok(())
        })?;
        let first = first.ok_or_else(|| invalid(path, 0, "empty index over a live table"))?;
        let last = last.ok_or_else(|| invalid(path, 0, "empty index over a live table"))?;
        let first_data = block(&file, path, limit, first)?;
        let first_bounds = entries(&first_data, path, first.offset, true, |_, _, _| Ok(()))?;
        let last_data = block(&file, path, limit, last)?;
        let last_bounds = entries(&last_data, path, last.offset, true, |_, _, _| Ok(()))?;
        if first_bounds.smallest.as_deref() != Some(metadata.smallest.as_slice())
            || last_bounds.largest != metadata.largest
        {
            return Err(invalid(path, 0, "table bounds differ from manifest bounds"));
        }
        Ok(Self {
            metadata,
            index_handle,
            meta_handle,
        })
    }

    fn index(
        file: &File,
        path: &Path,
        limit: u64,
        index_handle: Handle,
        mut visit: impl FnMut(&[u8], Handle) -> Result<()>,
    ) -> Result<()> {
        let data = block(file, path, limit, index_handle)?;
        let mut previous_end = 0;
        entries(&data, path, index_handle.offset, true, |key, value, offset| {
            let mut cursor = Cursor::new(value, path, offset);
            let data_handle = handle(&mut cursor)?;
            if !cursor.done() {
                return Err(cursor.error("trailing index handle bytes"));
            }
            let end =
                data_handle.offset.checked_add(data_handle.size).and_then(|n| n.checked_add(5));
            if data_handle.offset < previous_end || end.is_none_or(|n| n > limit) {
                return Err(cursor.error("unordered or out-of-range data block handle"));
            }
            previous_end = end.ok_or_else(|| cursor.error("block handle overflow"))?;
            visit(key, data_handle)
        })
        .map(|_| ())
    }

    fn filter(
        file: &File,
        path: &Path,
        limit: u64,
        meta_handle: Handle,
    ) -> Result<Option<Vec<u8>>> {
        let meta = block(file, path, limit, meta_handle)?;
        let mut filter = None;
        entries(&meta, path, meta_handle.offset, false, |key, value, offset| {
            if key != b"filter.leveldb.BuiltinBloomFilter2" {
                return Err(invalid(path, offset, "unsupported table filter name"));
            }
            let mut cursor = Cursor::new(value, path, offset);
            let filter_handle = handle(&mut cursor)?;
            if !cursor.done() {
                return Err(cursor.error("trailing metadata handle bytes"));
            }
            filter = Some(block(file, path, limit, filter_handle)?);
            Ok(())
        })?;
        Ok(filter)
    }

    pub fn lookup(&self, user_key: &[u8], last_sequence: u64) -> Result<Option<Value>> {
        let path = self.path();
        let file = open_file(path)?;
        let limit = self.metadata.size - 48;
        let filter = Self::filter(&file, path, limit, self.meta_handle)?;
        let mut index = Vec::new();
        Self::index(&file, path, limit, self.index_handle, |separator, data_handle| {
            index.push((separator.to_vec(), data_handle));
            Ok(())
        })?;
        let mut target = Vec::with_capacity(user_key.len() + 8);
        target.extend_from_slice(user_key);
        target.extend_from_slice(&((last_sequence << 8) | 1).to_le_bytes());
        let routed = index
            .iter()
            .find(|(separator, _)| !compare_internal(separator, &target).is_lt())
            .map(|(_, handle)| handle.offset);
        let mut found: Option<Value> = None;
        let mut found_block = None;
        let mut previous_separator: Option<Vec<u8>> = None;
        Self::index(&file, path, limit, self.index_handle, |separator, data_handle| {
            let data = block(&file, path, limit, data_handle)?;
            let mut first = true;
            let bounds = entries(&data, path, data_handle.offset, true, |key, value, offset| {
                if first
                    && previous_separator
                        .as_deref()
                        .is_some_and(|sep| !compare_internal(sep, key).is_lt())
                {
                    return Err(invalid(path, offset, "index separator overlaps next block"));
                }
                first = false;
                let (key, sequence, kind) = internal_key(key, path, offset)?;
                if sequence > last_sequence {
                    return Err(invalid(path, offset, "table sequence exceeds LastSequence"));
                }
                if kind == 0 && !value.is_empty() {
                    return Err(invalid(path, offset, "nonempty tombstone"));
                }
                if key == user_key {
                    let candidate = Value {
                        sequence,
                        bytes: (kind == 1).then(|| value.to_vec()),
                    };
                    let replaces = found.as_ref().is_none_or(|old| {
                        candidate.sequence > old.sequence
                            || (candidate.sequence == old.sequence && candidate == *old)
                    });
                    newest(&mut found, candidate, path)?;
                    if replaces {
                        found_block = Some(data_handle.offset);
                    }
                }
                Ok(())
            })?;
            if bounds.smallest.is_none() || compare_internal(&bounds.largest, separator).is_gt() {
                return Err(invalid(
                    path,
                    data_handle.offset,
                    "index separator precedes block last key",
                ));
            }
            previous_separator = Some(separator.to_vec());
            Ok(())
        })?;
        if found.is_some() && found_block != routed {
            return Err(invalid(path, 0, "newest key is outside routed data block"));
        }
        if let Some(routed) = routed
            && let Some(filter) = &filter
            && found.is_some()
            && !filter_may_match(filter, routed, user_key, path)?
        {
            return Err(invalid(path, routed, "filter excludes a present key"));
        }
        Ok(found)
    }
}

fn filter_may_match(data: &[u8], block_offset: u64, key: &[u8], path: &Path) -> Result<bool> {
    if data.len() < 5 {
        return Err(invalid(path, 0, "short filter block"));
    }
    let base = data[data.len() - 1];
    if base >= 64 {
        return Err(invalid(path, 0, "filter base overflow"));
    }
    let array = data.len() - 5;
    let offset = u32::from_le_bytes(
        data[array..array + 4].try_into().map_err(|_| invalid(path, 0, "filter offset"))?,
    ) as usize;
    if offset > array {
        return Err(invalid(path, 0, "filter array outside block"));
    }
    let count = (array - offset) / 4;
    let index = block_offset >> base;
    if index >= count as u64 {
        return Ok(true);
    }
    let start = offset + index as usize * 4;
    let begin = u32::from_le_bytes(
        data[start..start + 4].try_into().map_err(|_| invalid(path, 0, "filter begin"))?,
    ) as usize;
    let end = u32::from_le_bytes(
        data[start + 4..start + 8].try_into().map_err(|_| invalid(path, 0, "filter end"))?,
    ) as usize;
    if begin == end {
        return Ok(false);
    }
    // LevelDB treats malformed offsets as a possible match.
    if begin > end || end > offset {
        return Ok(true);
    }
    let filter = &data[begin..end];
    if filter.len() < 2 {
        return Ok(false);
    }
    let probes = filter[filter.len() - 1];
    if probes > 30 {
        return Ok(true);
    }
    let bits = (filter.len() - 1) * 8;
    let mut hash = bloom_hash(key);
    let delta = hash.rotate_right(17);
    for _ in 0..probes {
        let bit = hash as usize % bits;
        if filter[bit / 8] & (1 << (bit % 8)) == 0 {
            return Ok(false);
        }
        hash = hash.wrapping_add(delta);
    }
    Ok(true)
}

fn bloom_hash(key: &[u8]) -> u32 {
    let m: u32 = 0xc6a4a793;
    let mut h = 0xbc9f1d34 ^ (key.len() as u32).wrapping_mul(m);
    let (chunks, tail) = key.as_chunks::<4>();
    for chunk in chunks {
        let word = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        h = h.wrapping_add(word).wrapping_mul(m);
        h ^= h >> 16;
    }
    for (i, byte) in tail.iter().enumerate() {
        h = h.wrapping_add(u32::from(*byte) << (8 * i));
    }
    if !tail.is_empty() {
        h = h.wrapping_mul(m);
        h ^= h >> 24;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut n: usize, out: &mut Vec<u8>) {
        while n >= 128 {
            out.push(n as u8 | 128);
            n >>= 7;
        }
        out.push(n as u8);
    }

    fn internal(user: &[u8], sequence: u64) -> Vec<u8> {
        let mut key = user.to_vec();
        key.extend(((sequence << 8) | 1).to_le_bytes());
        key
    }

    fn block_bytes(rows: &[(Vec<u8>, Vec<u8>)], table: &mut Vec<u8>) -> Handle {
        let mut data = Vec::new();
        for (key, value) in rows {
            data.extend([0]);
            varint(key.len(), &mut data);
            varint(value.len(), &mut data);
            data.extend_from_slice(key);
            data.extend_from_slice(value);
        }
        data.extend([0, 0, 0, 0, 1, 0, 0, 0]);
        let offset = table.len() as u64;
        let mut checked = data.clone();
        checked.push(0);
        table.extend_from_slice(&checked);
        table.extend_from_slice(&crc32c::mask(crc32c::extend(0, &checked)).to_le_bytes());
        Handle {
            offset,
            size: data.len() as u64,
        }
    }

    fn encoded_handle(handle: Handle) -> Vec<u8> {
        let mut value = Vec::new();
        varint(handle.offset as usize, &mut value);
        varint(handle.size as usize, &mut value);
        value
    }

    fn fixture(
        blocks: &[Vec<(Vec<u8>, Vec<u8>)>],
        separators: &[Vec<u8>],
        filter: Option<Vec<u8>>,
        smallest: Vec<u8>,
        largest: Vec<u8>,
    ) -> (tempfile::TempDir, Table) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("000001.ldb");
        let mut bytes = Vec::new();
        let data_handles: Vec<_> =
            blocks.iter().map(|rows| block_bytes(rows, &mut bytes)).collect();
        let meta_rows = filter.map(|filter| {
            let handle = block_bytes(
                &[(b"filter.leveldb.BuiltinBloomFilter2".to_vec(), filter)],
                &mut bytes,
            );
            vec![(b"filter.leveldb.BuiltinBloomFilter2".to_vec(), encoded_handle(handle))]
        });
        let meta = block_bytes(&meta_rows.unwrap_or_default(), &mut bytes);
        let index_rows: Vec<_> = separators
            .iter()
            .zip(data_handles)
            .map(|(separator, handle)| (separator.clone(), encoded_handle(handle)))
            .collect();
        let index = block_bytes(&index_rows, &mut bytes);
        let mut footer = Vec::new();
        footer.extend_from_slice(&encoded_handle(meta));
        footer.extend_from_slice(&encoded_handle(index));
        footer.resize(40, 0);
        footer.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&footer);
        std::fs::write(&path, &bytes).unwrap();
        let table = Table::open(TableFile {
            path,
            level: 0,
            number: 1,
            size: bytes.len() as u64,
            smallest,
            largest,
        })
        .unwrap();
        (temp, table)
    }

    #[test]
    fn long_shared_prefix_refuses_before_visiting_the_second_entry() {
        let mut data = vec![0];
        varint(MAX_KEY, &mut data);
        data.push(0);
        data.extend(vec![b'B'; MAX_KEY - 8]);
        data.extend(257u64.to_le_bytes());
        for _ in 0..100_000 {
            varint(MAX_KEY - 8, &mut data);
            data.extend([9, 0]);
            data.extend([0; 9]);
        }
        data.extend([0, 0, 0, 0, 1, 0, 0, 0]);
        let mut visited = 0;
        assert!(
            entries(&data, Path::new("prefix"), 0, true, |_, _, _| {
                visited += 1;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(visited, 1);
    }

    #[test]
    fn shared_prefix_decode_work_is_linear_in_block_input() {
        ENTRY_WORK.store(0, Ordering::Relaxed);
        let user = vec![b'C'; MAX_KEY - 17];
        let mut data = Vec::new();
        let mut first = user.clone();
        first.extend((100u64 << 8 | 1).to_le_bytes());
        data.push(0);
        varint(first.len(), &mut data);
        varint(0, &mut data);
        data.extend_from_slice(&first);
        for sequence in (1..100).rev() {
            varint(user.len(), &mut data);
            varint(8, &mut data);
            varint(0, &mut data);
            data.extend_from_slice(&((sequence as u64) << 8 | 1).to_le_bytes());
        }
        data.extend([0, 0, 0, 0, 1, 0, 0, 0]);
        entries(&data, Path::new("linear"), 0, true, |_, _, _| Ok(())).unwrap();
        let work = ENTRY_WORK.load(Ordering::Relaxed);
        assert!(work <= data.len() * 8, "work {work}, input {}", data.len());
    }

    #[test]
    fn lookup_refuses_match_outside_core_routed_block() {
        let first = internal(b"A", 9);
        let hidden = internal(b"B", 4);
        let separator = internal(b"B", 5);
        let (_temp, table) = fixture(
            &[vec![(first.clone(), b"a".to_vec())], vec![(hidden.clone(), vec![7; 32])]],
            &[separator, hidden.clone()],
            None,
            first,
            hidden,
        );
        assert!(table.lookup(b"B", 10).is_err());
    }

    #[test]
    fn lookup_returns_match_from_core_routed_block() {
        let first = internal(b"A", 9);
        let value_key = internal(b"B", 4);
        let (_temp, table) = fixture(
            &[vec![(first.clone(), b"a".to_vec())], vec![(value_key.clone(), b"routed".to_vec())]],
            &[first.clone(), value_key.clone()],
            None,
            first,
            value_key,
        );
        assert_eq!(
            table.lookup(b"B", 10).unwrap().and_then(|value| value.bytes),
            Some(b"routed".to_vec())
        );
    }

    #[test]
    fn lookup_refuses_filter_that_excludes_present_key() {
        let value_key = internal(b"B", 4);
        let empty_filter = vec![0; 12].into_iter().chain(std::iter::once(11)).collect();
        let (_temp, table) = fixture(
            &[vec![(value_key.clone(), b"present".to_vec())]],
            std::slice::from_ref(&value_key),
            Some(empty_filter),
            value_key.clone(),
            value_key.clone(),
        );
        assert!(table.lookup(b"B", 10).is_err());
    }
}
