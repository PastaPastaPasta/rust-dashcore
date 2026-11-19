//! Exercises table, physical log, manifest and WriteBatch decoding through the point reader.
//! Raw malformed inputs, structured source-order comparisons and metadata-only inspection.
//! Inspection creates only zero-length blk files, so this target never decodes block bytes.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use dash_coreimport::leveldb::LevelDbReader;
use dash_coreimport::source::{Refusal, SourceInspection, inspect};
use node_l1::Divergence;

fn checksum(bytes: &[u8]) -> [u8; 4] {
    let mut crc = !0;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f63b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    (!crc).rotate_right(15).wrapping_add(0xa282ead8).to_le_bytes()
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}

fn slice(bytes: &[u8], out: &mut Vec<u8>) {
    varint(bytes.len() as u64, out);
    out.extend_from_slice(bytes);
}

fn log(record: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let fragments = record.len().max(1).div_ceil(32761);
    for n in 0..fragments {
        let begin = n * 32761;
        let end = record.len().min(begin + 32761);
        let payload = &record[begin..end];
        let kind = match (n == 0, n + 1 == fragments) {
            (true, true) => 1,
            (true, false) => 2,
            (false, false) => 3,
            (false, true) => 4,
        };
        let mut checked = vec![kind];
        checked.extend_from_slice(payload);
        out.extend_from_slice(&checksum(&checked));
        out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(payload);
    }
    out
}

fn repair_log(bytes: &mut [u8]) {
    for block in bytes.chunks_mut(32768) {
        let mut pos = 0;
        while let Some(header) = block.get(pos..pos + 7) {
            let len = usize::from(u16::from_le_bytes([header[4], header[5]]));
            let end = pos + 7 + len;
            let Some(checked) = block.get(pos + 6..end) else {
                break;
            };
            let crc = checksum(checked);
            block[pos..pos + 4].copy_from_slice(&crc);
            pos = end;
        }
    }
}

fn internal(key: &[u8]) -> Vec<u8> {
    let mut out = key.to_vec();
    out.extend_from_slice(&257u64.to_le_bytes());
    out
}

fn manifest(table_size: Option<usize>) -> Vec<u8> {
    let mut out = vec![1];
    slice(b"leveldb.BytewiseComparator", &mut out);
    out.extend_from_slice(&[2, 1, 4, 1]);
    if let Some(size) = table_size {
        out.extend_from_slice(&[7, 0, 2]);
        varint(size as u64, &mut out);
        slice(&internal(b""), &mut out);
        slice(&internal(&[255; 64]), &mut out);
    }
    out
}

fn block(data: &[u8], table: &mut Vec<u8>) -> (u64, u64) {
    let handle = (table.len() as u64, data.len() as u64);
    table.extend_from_slice(data);
    table.push(0);
    let crc = checksum(&table[handle.0 as usize..]);
    table.extend_from_slice(&crc);
    handle
}

fn handle(pair: (u64, u64), out: &mut Vec<u8>) {
    varint(pair.0, out);
    varint(pair.1, out);
}

fn restart_table(data: &[u8]) -> Vec<u8> {
    let mut table = Vec::new();
    let data_handle = block(data, &mut table);
    let meta_handle = block(&restart(&[]), &mut table);
    let mut index_value = Vec::new();
    handle(data_handle, &mut index_value);
    let index_handle = block(&restart(&[(internal(&[255; 64]), index_value)]), &mut table);
    let footer = table.len();
    handle(meta_handle, &mut table);
    handle(index_handle, &mut table);
    table.resize(footer + 40, 0);
    table.extend_from_slice(&0xdb4775248b80fb57u64.to_le_bytes());
    table
}

fn bloom_hash(key: &[u8]) -> u32 {
    let m = 0xc6a4a793u32;
    let mut hash = 0xbc9f1d34 ^ (key.len() as u32).wrapping_mul(m);
    let (chunks, tail) = key.as_chunks::<4>();
    for chunk in chunks {
        hash = hash.wrapping_add(u32::from_le_bytes(*chunk)).wrapping_mul(m);
        hash ^= hash >> 16;
    }
    for (index, byte) in tail.iter().enumerate() {
        hash = hash.wrapping_add(u32::from(*byte) << (index * 8));
    }
    if !tail.is_empty() {
        hash = hash.wrapping_mul(m);
        hash ^= hash >> 24;
    }
    hash
}

fn generate_bloom_filter(
    keys: &[&[u8]],
    flip: bool,
    filters: &mut Vec<u8>,
    offsets: &mut Vec<u32>,
) {
    offsets.push(filters.len() as u32);
    if keys.is_empty() {
        return;
    }
    let bits = (keys.len() * 10).max(64);
    let mut bytes = vec![0; bits.div_ceil(8)];
    for key in keys {
        let mut hash = bloom_hash(key);
        let delta = hash.rotate_right(17);
        for _ in 0..6 {
            let bit = hash as usize % (bytes.len() * 8);
            bytes[bit / 8] |= 1 << (bit % 8);
            hash = hash.wrapping_add(delta);
        }
    }
    if flip {
        bytes[0] ^= 1;
    }
    bytes.push(6);
    filters.extend(bytes);
}

fn bloom_filter(
    blocks: &[Vec<(Vec<u8>, Vec<u8>)>],
    data_handles: &[(u64, u64)],
    flip: bool,
) -> Vec<u8> {
    let mut filters = Vec::new();
    let mut offsets = Vec::new();
    let mut keys = Vec::new();
    for (block, &(offset, _)) in blocks.iter().zip(data_handles) {
        let filter_index = offset / 2048;
        while filter_index > offsets.len() as u64 {
            generate_bloom_filter(&keys, flip, &mut filters, &mut offsets);
            keys.clear();
        }
        keys.extend(block.iter().map(|(key, _)| &key[..key.len() - 8]));
    }
    if let Some(&(offset, size)) = data_handles.last() {
        let end_index = (offset + size + 5) / 2048;
        while end_index > offsets.len() as u64 {
            generate_bloom_filter(&keys, flip, &mut filters, &mut offsets);
            keys.clear();
        }
    }
    if !keys.is_empty() {
        generate_bloom_filter(&keys, flip, &mut filters, &mut offsets);
    }
    let array_offset = filters.len() as u32;
    filters.extend(offsets.iter().flat_map(|offset| offset.to_le_bytes()));
    filters.extend_from_slice(&array_offset.to_le_bytes());
    filters.push(11);
    filters
}

fn table_from_blocks(
    blocks: &[Vec<(Vec<u8>, Vec<u8>)>],
    separators: &[Vec<u8>],
    with_filter: bool,
    flip: bool,
) -> Vec<u8> {
    let mut table = Vec::new();
    let data_handles: Vec<_> =
        blocks.iter().map(|data| block(&restart(data), &mut table)).collect();
    let filter = with_filter.then(|| bloom_filter(blocks, &data_handles, flip));
    let meta_data = if let Some(filter) = filter {
        let filter_handle = block(&filter, &mut table);
        restart(&[(b"filter.leveldb.BuiltinBloomFilter2".to_vec(), {
            let mut value = Vec::new();
            handle(filter_handle, &mut value);
            value
        })])
    } else {
        restart(&[])
    };
    let meta_handle = block(&meta_data, &mut table);
    let mut value = Vec::new();
    let index_rows: Vec<_> = separators
        .iter()
        .zip(data_handles)
        .map(|(separator, data_handle)| {
            value.clear();
            handle(data_handle, &mut value);
            (separator.clone(), value.clone())
        })
        .collect();
    let index_handle = block(&restart(&index_rows), &mut table);
    let footer = table.len();
    handle(meta_handle, &mut table);
    handle(index_handle, &mut table);
    table.resize(footer + 40, 0);
    table.extend_from_slice(&0xdb4775248b80fb57u64.to_le_bytes());
    table
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), Divergence> {
    fs::write(path, bytes).map_err(|_| Divergence {
        field: "LevelDB scratch write",
    })
}

fn probe(dir: &Path, key: &[u8], input_len: usize) -> Result<(), Divergence> {
    let lookup = |key: &[u8]| {
        LevelDbReader::open(dir)
            .and_then(|reader| reader.get(key))
            .map_err(|error| error.to_string())
    };
    for key in
        [b"".as_slice(), b"B", b"R", b"l", b"\x0e\0obfuscate_key", b"F\x10prunedblockfiles", key]
    {
        let first = lookup(key);
        if first != lookup(key) {
            return Err(Divergence {
                field: "LevelDB lookup determinism",
            });
        }
        if first.is_ok_and(|value| value.is_some_and(|bytes| bytes.len() > input_len)) {
            return Err(Divergence {
                field: "LevelDB value length",
            });
        }
    }
    Ok(())
}

fn raw_check(mode: u8, payload: &[u8], dir: &Path) -> Result<(), Divergence> {
    let table = match mode % 5 {
        0 => Some(payload.to_vec()),
        4 => Some(restart_table(payload)),
        _ => None,
    };
    let edit = manifest(table.as_ref().map(Vec::len));
    write(&dir.join("CURRENT"), b"MANIFEST-000003\n")?;
    write(&dir.join("MANIFEST-000003"), &log(&edit))?;
    write(&dir.join("000001.log"), &[])?;
    if let Some(table) = table {
        write(&dir.join("000002.ldb"), &table)?;
    }
    match mode % 5 {
        1 => write(&dir.join("000001.log"), payload)?,
        2 => write(&dir.join("MANIFEST-000003"), payload)?,
        3 => write(&dir.join("000001.log"), &log(payload))?,
        _ => {}
    }
    let key = &payload[..payload.len().min(32)];
    (|| {
        probe(dir, key, payload.len())?;
        if mode % 5 == 1 || mode % 5 == 2 {
            let mut repaired = payload.to_vec();
            repair_log(&mut repaired);
            let name = if mode % 5 == 1 {
                "000001.log"
            } else {
                "MANIFEST-000003"
            };
            write(&dir.join(name), &repaired)?;
            probe(dir, key, payload.len())?;
        }
        if mode % 5 == 2 {
            write(&dir.join("MANIFEST-000003"), &log(payload))?;
            probe(dir, key, payload.len())?;
        }
        if mode.is_multiple_of(5) {
            let mut repaired = payload.to_vec();
            repair_table(&mut repaired);
            write(&dir.join("000002.ldb"), &repaired)?;
            probe(dir, key, payload.len())?;
        }
        Ok(())
    })()
}

fn decode_varint(bytes: &[u8], pos: &mut usize) -> Option<u64> {
    let mut n = 0;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*pos)?;
        *pos += 1;
        if shift == 63 && b > 1 {
            return None;
        }
        n |= u64::from(b & 127) << shift;
        if b < 128 {
            return Some(n);
        }
    }
    None
}

fn decode_handle(bytes: &[u8], pos: &mut usize) -> Option<(usize, usize)> {
    Some((
        usize::try_from(decode_varint(bytes, pos)?).ok()?,
        usize::try_from(decode_varint(bytes, pos)?).ok()?,
    ))
}

fn repair_block(bytes: &mut [u8], pair: (usize, usize)) -> Option<()> {
    let (offset, size) = pair;
    let end = offset.checked_add(size)?;
    let crc = checksum(bytes.get(offset..=end)?);
    bytes.get_mut(end + 1..end.checked_add(5)?)?.copy_from_slice(&crc);
    Some(())
}

fn repair_table(bytes: &mut [u8]) {
    let Some(footer) = bytes.len().checked_sub(48) else {
        return;
    };
    let mut pos = footer;
    let Some(meta) = decode_handle(bytes, &mut pos) else {
        return;
    };
    let Some(index) = decode_handle(bytes, &mut pos) else {
        return;
    };
    // Repair the footer handles even if their entry encodings cannot be decoded.
    repair_block(bytes, meta);
    repair_block(bytes, index);
    for pair in [meta, index] {
        let Some(end) = pair.0.checked_add(pair.1) else {
            continue;
        };
        let Some(data) = bytes.get(pair.0..end) else {
            continue;
        };
        let Some(count_pos) = data.len().checked_sub(4) else {
            continue;
        };
        let count = u32::from_le_bytes([
            data[count_pos],
            data[count_pos + 1],
            data[count_pos + 2],
            data[count_pos + 3],
        ]) as usize;
        let Some(entry_end) = count.checked_mul(4).and_then(|n| count_pos.checked_sub(n)) else {
            continue;
        };
        let mut cursor = 0;
        let mut handles = Vec::new();
        while cursor < entry_end {
            let Some(_) = decode_varint(data, &mut cursor) else {
                break;
            };
            let Some(key_len) =
                decode_varint(data, &mut cursor).and_then(|n| usize::try_from(n).ok())
            else {
                break;
            };
            let Some(value_len) =
                decode_varint(data, &mut cursor).and_then(|n| usize::try_from(n).ok())
            else {
                break;
            };
            let Some(start) = cursor.checked_add(key_len) else {
                break;
            };
            let Some(next) = start.checked_add(value_len) else {
                break;
            };
            let Some(value) = data.get(start..next.min(entry_end)) else {
                break;
            };
            if let Some(handle) = decode_handle(value, &mut 0) {
                handles.push(handle);
            }
            cursor = next;
        }
        for pair in handles {
            repair_block(bytes, pair);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    key: Vec<u8>,
    sequence: u64,
    value: Option<Vec<u8>>,
}

struct Source {
    level: Option<u8>,
    number: u64,
    layout: u8,
    rows: Vec<Row>,
}

impl Row {
    fn order_key(&self) -> (&[u8], Reverse<u64>, Reverse<bool>) {
        (self.key.as_slice(), Reverse(self.sequence), Reverse(self.value.is_some()))
    }
}

impl Source {
    fn first_key(&self) -> Option<(&[u8], Reverse<u64>, Reverse<bool>)> {
        self.rows.iter().map(Row::order_key).min()
    }

    fn last_key(&self) -> Option<(&[u8], Reverse<u64>, Reverse<bool>)> {
        self.rows.iter().map(Row::order_key).max()
    }
}

fn row_key(row: &Row) -> Vec<u8> {
    let mut key = row.key.clone();
    key.extend(((row.sequence << 8) | u64::from(row.value.is_some())).to_le_bytes());
    key
}

fn restart(rows: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in rows {
        out.push(0);
        varint(key.len() as u64, &mut out);
        varint(value.len() as u64, &mut out);
        out.extend(key);
        out.extend(value);
    }
    out.extend([0, 0, 0, 0, 1, 0, 0, 0]);
    out
}

fn sorted_rows(rows: &[Row]) -> Vec<Row> {
    let mut rows = rows.to_vec();
    rows.sort_by(|a, b| {
        a.key
            .cmp(&b.key)
            .then_with(|| b.sequence.cmp(&a.sequence))
            .then_with(|| b.value.is_some().cmp(&a.value.is_some()))
    });
    rows
}

fn table_layout(rows: &[Row], layout: u8) -> (Vec<Vec<Row>>, Vec<Vec<u8>>, bool, bool) {
    let rows = sorted_rows(rows);
    let count = rows.len().min(3).min(1 + usize::from(layout % 3));
    let mut blocks = Vec::new();
    for n in 0..count {
        let start = n * rows.len() / count;
        let end = (n + 1) * rows.len() / count;
        blocks.push(rows[start..end].to_vec());
    }
    let mut separators = Vec::new();
    for (n, block) in blocks.iter().enumerate() {
        let Some(last_row) = block.last() else {
            continue;
        };
        let last = row_key(last_row);
        let next_first = blocks.get(n + 1).and_then(|next| next.first());
        let separator = if let Some(first_row) = next_first
            && layout & 4 != 0
        {
            let first = row_key(first_row);
            let user_len = first.len() - 8;
            let mut tag = [0; 8];
            tag.copy_from_slice(&first[user_len..]);
            let first_sequence = u64::from_le_bytes(tag) >> 8;
            let mut candidate = first[..user_len].to_vec();
            candidate.extend(((first_sequence.saturating_add(1) << 8) | 1).to_le_bytes());
            if compare_model_internal(&last, &candidate).is_lt()
                && compare_model_internal(&candidate, &first).is_lt()
            {
                candidate
            } else {
                last
            }
        } else {
            last
        };
        separators.push(separator);
    }
    (blocks, separators, layout & 1 != 0, layout & 2 != 0)
}

fn compare_model_internal(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let (a_user, a_tag) = a.split_at(a.len() - 8);
    let (b_user, b_tag) = b.split_at(b.len() - 8);
    a_user.cmp(b_user).then_with(|| b_tag.iter().rev().cmp(a_tag.iter().rev()))
}

fn structured_table(rows: &[Row], layout: u8) -> Vec<u8> {
    let (blocks, separators, with_filter, flip) = table_layout(rows, layout);
    let encoded: Vec<_> = blocks
        .iter()
        .map(|block| {
            block.iter().map(|row| (row_key(row), row.value.clone().unwrap_or_default())).collect()
        })
        .collect();
    table_from_blocks(&encoded, &separators, with_filter, flip)
}

fn data_block_offsets(blocks: &[Vec<Row>]) -> Vec<u64> {
    let mut offset = 0;
    blocks
        .iter()
        .map(|block| {
            let current = offset;
            let encoded: Vec<_> = block
                .iter()
                .map(|row| (row_key(row), row.value.clone().unwrap_or_default()))
                .collect();
            offset += restart(&encoded).len() as u64 + 5;
            current
        })
        .collect()
}

fn write_batch(row: &Row) -> Vec<u8> {
    let mut out = row.sequence.to_le_bytes().to_vec();
    out.extend(1u32.to_le_bytes());
    out.push(u8::from(row.value.is_some()));
    slice(&row.key, &mut out);
    if let Some(value) = &row.value {
        slice(value, &mut out);
    }
    log(&out)
}

fn write_sources(dir: &Path, sources: &[Source], last_sequence: u64) -> Result<(), Divergence> {
    fs::create_dir_all(dir).map_err(|_| Divergence {
        field: "structured directory",
    })?;
    let mut edit = manifest(None);
    edit.push(4);
    varint(last_sequence, &mut edit);
    for source in sources {
        if let Some(level) = source.level {
            if source.rows.is_empty() {
                continue;
            }
            let rows = sorted_rows(&source.rows);
            let bytes = structured_table(&rows, source.layout);
            edit.extend([7, level]);
            varint(source.number, &mut edit);
            varint(bytes.len() as u64, &mut edit);
            slice(&row_key(&rows[0]), &mut edit);
            slice(&row_key(&rows[rows.len() - 1]), &mut edit);
            write(&dir.join(format!("{:06}.ldb", source.number)), &bytes)?;
        } else {
            let bytes: Vec<_> = source.rows.iter().flat_map(write_batch).collect();
            write(&dir.join(format!("{:06}.log", source.number)), &bytes)?;
        }
    }
    write(&dir.join("000001.log"), &[])?;
    write(&dir.join("CURRENT"), b"MANIFEST-000003\n")?;
    write(&dir.join("MANIFEST-000003"), &log(&edit))
}

type Versions<'a> = BTreeMap<&'a [u8], BTreeMap<(u64, bool), &'a Option<Vec<u8>>>>;

fn oracle(sources: &[Source], last_sequence: u64, key: &[u8]) -> Result<Option<Vec<u8>>, ()> {
    let mut replay: Vec<_> = sources.iter().filter(|s| s.level.is_none()).collect();
    replay.sort_by_key(|s| s.number);
    let mut previous = None;
    let mut effective = last_sequence;
    for source in &replay {
        for row in &source.rows {
            if previous.is_some_and(|seq| row.sequence <= seq) {
                return Err(());
            }
            previous = Some(row.sequence);
            effective = effective.max(row.sequence);
        }
    }
    let mut tables: Vec<_> =
        sources.iter().filter(|s| s.level.is_some() && !s.rows.is_empty()).collect();
    tables.sort_by(|a, b| {
        a.level.cmp(&b.level).then_with(|| {
            if a.level == Some(0) {
                b.number.cmp(&a.number)
            } else {
                a.first_key().cmp(&b.first_key())
            }
        })
    });
    if tables.windows(2).any(|pair| {
        pair[0].level.is_some_and(|level| level > 0)
            && pair[0].level == pair[1].level
            && pair[0].last_key() >= pair[1].first_key()
    }) {
        return Err(());
    }
    // Opening a table checks key order in its index and in its first and last data blocks; a
    // lookup checks the other blocks only for keys inside the table's range, as Core skips a
    // table outside it.
    if tables.iter().any(|source| {
        let (blocks, separators, ..) = table_layout(&source.rows, source.layout);
        separators.windows(2).any(|pair| !compare_model_internal(&pair[0], &pair[1]).is_lt())
            || [blocks.first(), blocks.last()]
                .into_iter()
                .flatten()
                .any(|block| has_duplicate(block))
    }) {
        return Err(());
    }
    replay.reverse();
    let mut winner: Option<(u64, Option<Vec<u8>>)> = None;
    for source in replay.into_iter().chain(tables) {
        let source_rows = if source.level.is_some() {
            let (blocks, separators, with_filter, flip) = table_layout(&source.rows, source.layout);
            let data_offsets = data_block_offsets(&blocks);
            let mut target = key.to_vec();
            target.extend(((effective << 8) | 1).to_le_bytes());
            let routed = separators
                .iter()
                .position(|separator| !compare_model_internal(separator, &target).is_lt());
            let newest_sequence = blocks
                .iter()
                .flat_map(|block| block.iter().filter(|row| row.key == key))
                .map(|row| row.sequence)
                .max();
            let newest_blocks: Vec<_> = newest_sequence.map_or_else(Vec::new, |sequence| {
                blocks
                    .iter()
                    .enumerate()
                    .filter(|(_, block)| {
                        block.iter().any(|row| row.key == key && row.sequence == sequence)
                    })
                    .map(|(index, _)| index)
                    .collect()
            });
            if newest_blocks.iter().any(|index| Some(*index) != routed) {
                return Err(());
            }
            if let Some(index) = routed
                && with_filter
                && blocks[index].iter().any(|row| row.key == key)
                && !model_filter_may_match(&blocks, &data_offsets, data_offsets[index], key, flip)
            {
                return Err(());
            }
            routed.map_or_else(Vec::new, |index| blocks[index].clone())
        } else {
            source.rows.clone()
        };
        let mut versions: Versions = BTreeMap::new();
        for row in &source_rows {
            versions
                .entry(&row.key)
                .or_default()
                .insert((row.sequence, row.value.is_some()), &row.value);
        }
        if source.level.is_some() {
            let low = source.rows.iter().map(|row| row.key.as_slice()).min();
            let high = source.rows.iter().map(|row| row.key.as_slice()).max();
            let (Some(low), Some(high)) = (low, high) else {
                continue;
            };
            if key < low || key > high {
                continue;
            }
            if has_duplicate(&sorted_rows(&source.rows))
                || source.rows.iter().any(|row| row.sequence > effective)
            {
                return Err(());
            }
        }
        let Some(rows) = versions.get(key) else {
            continue;
        };
        let Some(((seq, _), value)) = rows.last_key_value() else {
            continue;
        };
        if rows.iter().any(|((other_seq, _), other)| other_seq == seq && *other != *value) {
            return Err(());
        }
        if let Some((winning_seq, winning_value)) = &winner {
            if seq > winning_seq || (seq == winning_seq && *value != winning_value) {
                return Err(());
            }
        } else {
            winner = Some((*seq, (*value).clone()));
        }
    }
    Ok(winner.and_then(|(_, value)| value))
}

fn has_duplicate(sorted: &[Row]) -> bool {
    sorted.windows(2).any(|pair| pair[0].order_key() == pair[1].order_key())
}

fn compare_lookup(
    expected: Result<Option<Vec<u8>>, ()>,
    actual: Result<Option<Vec<u8>>, ()>,
) -> Result<(), Divergence> {
    if expected != actual {
        return Err(Divergence {
            field: "LevelDB semantic oracle",
        });
    }
    Ok(())
}

fn model_filter_may_match(
    blocks: &[Vec<Row>],
    data_offsets: &[u64],
    routed_offset: u64,
    key: &[u8],
    flip: bool,
) -> bool {
    let window = routed_offset / 2048;
    let keys: Vec<_> = blocks
        .iter()
        .zip(data_offsets)
        .filter(|&(_, &offset)| offset / 2048 == window)
        .flat_map(|(block, _)| block.iter().map(|row| row.key.as_slice()))
        .collect();
    let bits = (keys.len() * 10).max(64);
    let mut filter = vec![0; bits.div_ceil(8)];
    for key in keys {
        let mut hash = bloom_hash(key);
        let delta = hash.rotate_right(17);
        for _ in 0..6 {
            let bit = hash as usize % (filter.len() * 8);
            filter[bit / 8] |= 1 << (bit % 8);
            hash = hash.wrapping_add(delta);
        }
    }
    if flip {
        filter[0] ^= 1;
    }
    let mut hash = bloom_hash(key);
    let delta = hash.rotate_right(17);
    for _ in 0..6 {
        let bit = hash as usize % (filter.len() * 8);
        if filter[bit / 8] & (1 << (bit % 8)) == 0 {
            return false;
        }
        hash = hash.wrapping_add(delta);
    }
    true
}

fn differential(payload: &[u8], dir: &Path) -> Result<(), Divergence> {
    let (sources, last_sequence) = differential_sources(payload);
    write_sources(dir, &sources, last_sequence)?;
    for key in [b"a".as_slice(), b"b", b"c", b"absent"] {
        let actual = LevelDbReader::open(dir).and_then(|reader| reader.get(key)).map_err(|_| ());
        compare_lookup(oracle(&sources, last_sequence, key), actual)?;
    }
    Ok(())
}

fn differential_sources(payload: &[u8]) -> (Vec<Source>, u64) {
    let last_sequence = u64::from(payload.first().copied().unwrap_or(0));
    let mut sources: Vec<_> = (0..7)
        .map(|n| Source {
            level: if n < 3 {
                None
            } else {
                Some(if n >= 5 {
                    1 + payload.get(1).copied().unwrap_or(0) % 6
                } else {
                    0
                })
            },
            number: if n < 3 {
                n + 10
            } else {
                n + 20
            },
            layout: payload.first().copied().unwrap_or(0).wrapping_add(n as u8),
            rows: Vec::new(),
        })
        .collect();
    for bytes in payload.get(1..).unwrap_or_default().as_chunks::<6>().0.iter().take(64) {
        let source = &mut sources[bytes[0] as usize % 7];
        let scale = if source.level.is_some() && bytes[3] & 2 != 0 {
            256
        } else {
            1
        };
        source.rows.push(Row {
            key: vec![b'a' + bytes[1] % 3],
            sequence: u64::from(bytes[2]),
            value: (bytes[3] & 1 != 0).then(|| vec![bytes[4]; bytes[5] as usize % 16 * scale]),
        });
    }
    (sources, last_sequence)
}

#[derive(Debug, PartialEq, Eq)]
struct Inspection {
    reindexing: bool,
    pruned: bool,
    last_file: Option<u32>,
    tail: u32,
    obfuscation: [u8; 8],
    best: Option<[u8; 32]>,
    refusal: Option<Refusal>,
}

impl From<SourceInspection> for Inspection {
    fn from(source: SourceInspection) -> Self {
        let refusal = source.refusal();
        Self {
            reindexing: source.reindexing,
            pruned: source.pruned,
            last_file: source.last_file,
            tail: source.tail,
            obfuscation: source.obfuscation_key,
            best: source.best_block,
            refusal,
        }
    }
}

fn compare_inspection(
    expected: Result<Inspection, ()>,
    actual: Result<Inspection, ()>,
) -> Result<(), Divergence> {
    if expected != actual {
        return Err(Divergence {
            field: "source inspection oracle",
        });
    }
    Ok(())
}

type FileImage = BTreeMap<PathBuf, (Vec<u8>, SystemTime)>;

fn snapshot(dir: &Path) -> Result<FileImage, Divergence> {
    let mut image = BTreeMap::new();
    for entry in fs::read_dir(dir).map_err(|_| Divergence {
        field: "source image listing",
    })? {
        let entry = entry.map_err(|_| Divergence {
            field: "source image entry",
        })?;
        let path = entry.path();
        let metadata = entry.metadata().map_err(|_| Divergence {
            field: "source image metadata",
        })?;
        if metadata.is_dir() {
            image.extend(snapshot(&path)?);
        } else {
            let bytes = fs::read(&path).map_err(|_| Divergence {
                field: "source image read",
            })?;
            let modified = metadata.modified().map_err(|_| Divergence {
                field: "source image time",
            })?;
            image.insert(path, (bytes, modified));
        }
    }
    Ok(image)
}

fn inspect_image(payload: &[u8], dir: &Path) -> Result<(), Divergence> {
    inspect_image_with(payload, dir, |inspection| inspection)
}

fn inspect_image_with(
    payload: &[u8],
    dir: &Path,
    observe: impl FnOnce(Inspection) -> Inspection,
) -> Result<(), Divergence> {
    let byte = |n| payload.get(n).copied().unwrap_or(0);
    let reindex = match byte(0) % 4 {
        0 => None,
        1 => Some(b"1".to_vec()),
        2 => Some(b"0".to_vec()),
        _ => Some(b"11".to_vec()),
    };
    let prune = match byte(1) % 4 {
        0 => None,
        1 => Some(b"0".to_vec()),
        2 => Some(b"1".to_vec()),
        _ => Some(b"2".to_vec()),
    };
    let last = match byte(2) % 4 {
        0 => None,
        1 => Some(u32::from(byte(3) % 3).to_le_bytes().to_vec()),
        2 => Some(vec![0; 3]),
        _ => Some(vec![255; 4]),
    };
    let obfuscation = |mode: u8| match mode % 4 {
        0 => None,
        1 => Some(vec![8, 0, 0, 0, 0, 0, 0, 0, 0]),
        2 => Some(vec![8; 9]),
        _ => Some(vec![7; 8]),
    };
    let index_key = obfuscation(byte(4));
    let chain_key = obfuscation(byte(5));
    let best = match byte(6) % 3 {
        0 => None,
        1 => Some(vec![byte(7); 32]),
        _ => Some(vec![byte(7); 31]),
    };
    let rows = |items: Vec<(&[u8], Option<Vec<u8>>)>| {
        items
            .into_iter()
            .enumerate()
            .filter_map(|(n, (key, value))| {
                value.map(|value| Row {
                    key: key.to_vec(),
                    sequence: n as u64 + 1,
                    value: Some(value),
                })
            })
            .collect()
    };
    write_sources(
        &dir.join("blocks/index"),
        &[Source {
            level: None,
            number: 10,
            layout: 0,
            rows: rows(vec![
                (b"\x0e\0obfuscate_key", index_key),
                (b"R", reindex),
                (b"F\x10prunedblockfiles", prune),
                (b"l", last),
            ]),
        }],
        10,
    )?;
    write_sources(
        &dir.join("chainstate"),
        &[Source {
            level: None,
            number: 10,
            layout: 0,
            rows: rows(vec![(b"\x0e\0obfuscate_key", chain_key), (b"B", best.clone())]),
        }],
        10,
    )?;
    let files: Vec<_> = (0..3).filter(|n| byte(8) & (1 << n) != 0).collect();
    for n in &files {
        write(&dir.join(format!("blocks/blk{n:05}.dat")), &[])?;
    }
    let genesis = [byte(9) & 1; 32];
    let invalid = byte(0) % 4 >= 2
        || byte(1) % 4 == 3
        || byte(2) % 4 >= 2
        || byte(4) % 4 >= 2
        || byte(5) % 4 == 3
        || byte(6) % 3 == 2;
    let expected = if invalid {
        Err(())
    } else {
        let reindexing = byte(0) % 4 == 1;
        let pruned = byte(1) % 4 == 2;
        let xor = if byte(5) % 4 == 2 {
            [8; 8]
        } else {
            [0; 8]
        };
        let best = best.map(|bytes| {
            let mut value = [0; 32];
            for (n, b) in bytes.into_iter().enumerate() {
                value[n] = b ^ xor[n % 8];
            }
            value
        });
        let refusal = if reindexing {
            Some(Refusal::Reindexing)
        } else if pruned {
            Some(Refusal::Pruned)
        } else if files.first() != Some(&0) {
            Some(Refusal::NoGenesisFile)
        } else if genesis != [0; 32] {
            Some(Refusal::WrongGenesis)
        } else if let Some((n, _)) = files.iter().enumerate().find(|(n, file)| *n as u32 != **file)
        {
            Some(Refusal::Gap(n as u32))
        } else if best.is_none() {
            Some(Refusal::MissingBestBlock)
        } else {
            Some(Refusal::BestBlockNotInFiles)
        };
        Ok(Inspection {
            reindexing,
            pruned,
            last_file: (byte(2) % 4 == 1).then(|| u32::from(byte(3) % 3)),
            tail: files.last().copied().unwrap_or(0),
            obfuscation: xor,
            best,
            refusal,
        })
    };
    let before = snapshot(dir)?;
    let actual = inspect(dir, [0; 4], genesis).map(Inspection::from).map(observe).map_err(|_| ());
    if before != snapshot(dir)? {
        return Err(Divergence {
            field: "source bytes and mtime preservation",
        });
    }
    compare_inspection(expected, actual)
}

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

pub(super) fn check(data: &[u8]) -> Result<(), Divergence> {
    let Some((&mode, payload)) = data.split_first() else {
        return Ok(());
    };
    let dir = env::temp_dir().join(format!(
        "coreimport-leveldb-{}-{}",
        process::id(),
        NEXT_DIR.fetch_add(1, Ordering::Relaxed)
    ));
    DirBuilder::new().mode(0o700).create(&dir).map_err(|_| Divergence {
        field: "LevelDB scratch directory",
    })?;
    let result = match mode % 7 {
        5 => differential(payload, &dir),
        6 => inspect_image(payload, &dir),
        raw => raw_check(raw, payload, &dir),
    };
    fs::remove_dir_all(&dir).map_err(|_| Divergence {
        field: "LevelDB scratch cleanup",
    })?;
    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // test code: a failed fixture write or decode is the failure
mod tests {
    use super::*;

    type Lookup = Result<Option<Vec<u8>>, ()>;

    #[test]
    fn coreimport_leveldb_structured_cases() {
        for case in [
            vec![5],
            vec![5, 30, 3, 0, 10, 1, 1, 2, 4, 0, 20, 0, 0, 0],
            vec![5, 30, 3, 0, 30, 1, 1, 1, 4, 0, 20, 1, 2, 1],
            vec![5, 30, 0, 0, 20, 1, 1, 1, 1, 0, 10, 1, 2, 1],
            vec![5, 1, 3, 0, 30, 1, 1, 1],
            vec![5, 30, 5, 0, 30, 1, 1, 1, 6, 0, 20, 1, 2, 1],
        ] {
            check(&case).unwrap();
        }
        for offset in 0..10 {
            for byte in [0, 1, 2, 3, 7, 255] {
                let mut image = vec![0; 11];
                image[0] = 6;
                image[offset + 1] = byte;
                check(&image).unwrap();
            }
        }
        for flags in [[0, 0], [1, 0], [0, 2]] {
            for files in 0..8 {
                check(&[6, flags[0], flags[1], 1, 2, 1, 2, 1, 31, files, 0]).unwrap();
            }
        }
        for n in 0..256 {
            let data: Vec<_> = (0..96).map(|i| ((n * 37 + i * 83) % 256) as u8).collect();
            let mut structured = vec![5];
            structured.extend(&data);
            check(&structured).unwrap();
            let mut image = vec![6];
            image.extend(&data);
            check(&image).unwrap();
        }
    }

    #[test]
    fn opus_r3_probe_filter_index_two_blocks() {
        let mut payload = vec![254u8];
        payload.extend([3, 0, 1, 1, 7, 3]);
        payload.extend([3, 1, 2, 1, 8, 3]);
        let mut input = vec![5u8];
        input.extend(payload);
        let result = check(&input);
        assert!(result.is_ok(), "divergence: {:?}", result.err().map(|d| d.field));
    }

    #[test]
    fn opus_r3_probe_same_layout_without_filter() {
        let mut input = vec![5u8, 7];
        input.extend([3, 0, 1, 1, 7, 3]);
        input.extend([3, 1, 2, 1, 8, 3]);
        assert!(check(&input).is_ok());
    }

    #[test]
    fn opus_r3_probe_sides() {
        let sources = vec![Source {
            level: Some(0),
            number: 10,
            layout: 1,
            rows: vec![
                Row {
                    key: b"a".to_vec(),
                    sequence: 1,
                    value: Some(vec![7; 3]),
                },
                Row {
                    key: b"b".to_vec(),
                    sequence: 2,
                    value: Some(vec![8; 3]),
                },
            ],
        }];
        let dir = env::temp_dir().join(format!("opus-r3-sides-{}", process::id()));
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        write_sources(&dir, &sources, 254).unwrap();
        let actual = LevelDbReader::open(&dir).and_then(|r| r.get(b"b")).map_err(|_| ());
        let expected = oracle(&sources, 254, b"b");
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn coreimport_leveldb_filter_windows_follow_data_offsets() {
        let shared = vec![
            Row {
                key: b"a".to_vec(),
                sequence: 1,
                value: Some(vec![7; 3]),
            },
            Row {
                key: b"b".to_vec(),
                sequence: 2,
                value: Some(vec![8; 3]),
            },
        ];
        let (blocks, _, _, _) = table_layout(&shared, 1);
        let offsets = data_block_offsets(&blocks);
        assert_eq!(offsets[0] / 2048, offsets[1] / 2048);

        let split = vec![
            Row {
                key: b"a".to_vec(),
                sequence: 1,
                value: Some(vec![7; 3]),
            },
            Row {
                key: b"b".to_vec(),
                sequence: 2,
                value: Some(vec![8; 2100]),
            },
            Row {
                key: b"c".to_vec(),
                sequence: 3,
                value: Some(vec![9; 3]),
            },
        ];
        let (blocks, _, _, _) = table_layout(&split, 5);
        let offsets = data_block_offsets(&blocks);
        assert_eq!(offsets[0] / 2048, offsets[1] / 2048);
        assert_ne!(offsets[1] / 2048, offsets[2] / 2048);
    }

    #[test]
    fn coreimport_leveldb_oracle_rejects_ordinal_filter_selection() {
        let source = Source {
            level: Some(0),
            number: 10,
            layout: 5,
            rows: vec![
                Row {
                    key: b"a".to_vec(),
                    sequence: 1,
                    value: Some(vec![7; 3]),
                },
                Row {
                    key: b"b".to_vec(),
                    sequence: 2,
                    value: Some(vec![8; 2100]),
                },
                Row {
                    key: b"c".to_vec(),
                    sequence: 3,
                    value: Some(vec![9; 3]),
                },
            ],
        };
        let (blocks, _, with_filter, flip) = table_layout(&source.rows, source.layout);
        let offsets = data_block_offsets(&blocks);
        assert!(with_filter);
        assert_eq!(offsets[0] / 2048, offsets[1] / 2048);
        assert_ne!(offsets[1] / 2048, offsets[2] / 2048);
        assert!(model_filter_may_match(&blocks, &offsets, offsets[1], b"b", flip));
        let ordinal_filter = model_filter_may_match(&blocks, &offsets, offsets[2], b"b", flip);
        assert!(!ordinal_filter);

        let sources = vec![source];
        let expected = oracle(&sources, 254, b"b");
        let mutant = ordinal_filter.then(|| vec![8; 2100]);
        assert!(compare_lookup(expected, Ok(mutant)).is_err());
    }

    fn split_rows(middle_len: usize) -> Vec<Row> {
        vec![
            Row {
                key: b"a".to_vec(),
                sequence: 1,
                value: Some(vec![7; 3]),
            },
            Row {
                key: b"b".to_vec(),
                sequence: 2,
                value: Some(vec![8; middle_len]),
            },
            Row {
                key: b"c".to_vec(),
                sequence: 3,
                value: Some(vec![9; 3]),
            },
        ]
    }

    fn reader_lookups(
        name: &str,
        sources: &[Source],
        last_sequence: u64,
        keys: &[&[u8]],
    ) -> Vec<(Lookup, Lookup)> {
        let dir = env::temp_dir().join(format!("coreimport-leveldb-{name}-{}", process::id()));
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        write_sources(&dir, sources, last_sequence).unwrap();
        let results = keys
            .iter()
            .map(|key| {
                let actual =
                    LevelDbReader::open(&dir).and_then(|reader| reader.get(key)).map_err(|_| ());
                (oracle(sources, last_sequence, key), actual)
            })
            .collect();
        fs::remove_dir_all(&dir).unwrap();
        results
    }

    #[test]
    fn coreimport_leveldb_reader_matches_oracle_across_filter_windows() {
        for (middle_len, windows) in [(2100, [0, 0, 1]), (4200, [0, 0, 2])] {
            for (layout, flip) in [(17, false), (11, true)] {
                let rows = split_rows(middle_len);
                let (blocks, _, with_filter, layout_flip) = table_layout(&rows, layout);
                let offsets = data_block_offsets(&blocks);
                assert!(with_filter);
                assert_eq!(layout_flip, flip);
                assert_eq!(offsets.iter().map(|offset| offset / 2048).collect::<Vec<_>>(), windows);
                let sources = vec![Source {
                    level: Some(0),
                    number: 10,
                    layout,
                    rows: rows.clone(),
                }];
                let name = format!("windows-{middle_len}-{layout}");
                let keys: [&[u8]; 4] = [b"a", b"b", b"c", b"absent"];
                for (key, (expected, actual)) in
                    keys.iter().zip(reader_lookups(&name, &sources, 254, &keys))
                {
                    let row = rows.iter().find(|row| row.key == *key);
                    assert_eq!(expected, Ok(row.and_then(|row| row.value.clone())));
                    assert_eq!(actual, expected, "{name} key {key:?}");
                }
            }
        }
    }

    #[test]
    fn coreimport_leveldb_differential_reaches_later_filter_windows() {
        for (last_sequence, flip) in [(14u8, false), (8, true)] {
            let mut input = vec![5, last_sequence];
            for (key, fill) in [(0u8, 7u8), (1, 8), (2, 9)] {
                input.extend([3, key, key + 1, 3, fill, 15]);
            }
            let (sources, _) = differential_sources(&input[1..]);
            let source = &sources[3];
            assert_eq!(source.rows.len(), 3);
            assert!(source.rows.iter().all(|row| row.value.as_ref().unwrap().len() == 3840));
            let (blocks, _, with_filter, layout_flip) = table_layout(&source.rows, source.layout);
            assert!(with_filter);
            assert_eq!(layout_flip, flip);
            let windows: Vec<_> =
                data_block_offsets(&blocks).iter().map(|offset| offset / 2048).collect();
            assert_eq!(windows, [0, 1, 3]);
            for key in [b"a".as_slice(), b"b", b"c"] {
                assert!(oracle(&sources, u64::from(last_sequence), key).unwrap().is_some());
            }
            check(&input).unwrap();
        }
    }

    #[test]
    fn coreimport_leveldb_filter_block_writes_core_trailing_filters() {
        for (rows, expected_count) in [
            (vec![(b"a".as_slice(), 4200)], 2),
            (vec![(b"a".as_slice(), 3), (b"b", 4200)], 2),
            (vec![(b"a".as_slice(), 3), (b"b", 6300)], 3),
            (vec![(b"a".as_slice(), 3), (b"b", 3)], 1),
        ] {
            let blocks: Vec<Vec<_>> = rows
                .iter()
                .enumerate()
                .map(|(n, &(key, len))| {
                    let row = Row {
                        key: key.to_vec(),
                        sequence: n as u64 + 1,
                        value: Some(vec![1; len]),
                    };
                    vec![(row_key(&row), row.value.unwrap())]
                })
                .collect();
            let mut table = Vec::new();
            let handles: Vec<_> =
                blocks.iter().map(|rows| block(&restart(rows), &mut table)).collect();
            let &(last_offset, last_size) = handles.last().unwrap();
            let start_window = last_offset / 2048;
            let end_window = (last_offset + last_size + 5) / 2048;
            let core_count = if end_window > start_window {
                end_window
            } else {
                start_window + 1
            };
            assert_eq!(core_count, expected_count);
            let filter = bloom_filter(&blocks, &handles, false);
            let array = filter.len() - 5;
            let array_offset =
                u32::from_le_bytes(filter[array..array + 4].try_into().unwrap()) as usize;
            let count = (array - array_offset) / 4;
            assert_eq!(count as u64, core_count);
            let entry = |n: usize| {
                let start = array_offset + n * 4;
                u32::from_le_bytes(filter[start..start + 4].try_into().unwrap())
            };
            for n in (start_window as usize + 1)..count {
                assert_eq!(entry(n), array_offset as u32);
            }
        }
    }

    #[test]
    fn coreimport_leveldb_duplicate_rows_refuse_only_covered_keys() {
        // Fuzz cluster 9881413be4a8: a table with a duplicate internal key and a log row for "c".
        check(include_bytes!(
            "../../public-seeds/coreimport_leveldb/0ddf28c227aa0b119a6f265cf813211dab77689087e129e9bf13366fb26b6164.leveldb-seed"
        ))
        .unwrap();
        let row = |key: &[u8], sequence, fill| Row {
            key: key.to_vec(),
            sequence,
            value: Some(vec![fill; 3]),
        };
        for (name, layout, rows, open_refuses) in [
            ("dup-boundary", 1, vec![row(b"a", 5, 1), row(b"a", 5, 2), row(b"b", 1, 3)], false),
            (
                "dup-middle",
                2,
                vec![
                    row(b"a", 9, 1),
                    row(b"a", 5, 2),
                    row(b"a", 5, 2),
                    row(b"b", 2, 3),
                    row(b"b", 1, 4),
                ],
                false,
            ),
            ("dup-edge", 0, vec![row(b"a", 5, 1), row(b"a", 5, 2), row(b"b", 1, 3)], true),
            ("dup-index", 2, vec![row(b"a", 9, 1), row(b"a", 5, 2), row(b"a", 5, 2)], true),
        ] {
            let sources = vec![
                Source {
                    level: None,
                    number: 10,
                    layout: 0,
                    rows: vec![row(b"c", 12, 9)],
                },
                Source {
                    level: Some(0),
                    number: 20,
                    layout,
                    rows,
                },
            ];
            let keys: [&[u8]; 3] = [b"a", b"b", b"c"];
            for (key, (expected, actual)) in
                keys.iter().zip(reader_lookups(name, &sources, 10, &keys))
            {
                let covered = *key != b"c";
                let want = if covered || open_refuses {
                    Err(())
                } else {
                    Ok(Some(vec![9; 3]))
                };
                assert_eq!(expected, want, "{name} oracle key {key:?}");
                assert_eq!(actual, expected, "{name} reader key {key:?}");
            }
        }
    }

    #[test]
    fn coreimport_leveldb_oracle_rejects_reader_that_ignores_routing() {
        let sources = vec![Source {
            level: Some(0),
            number: 10,
            layout: 5,
            rows: vec![
                Row {
                    key: b"A".to_vec(),
                    sequence: 9,
                    value: Some(b"a".to_vec()),
                },
                Row {
                    key: b"B".to_vec(),
                    sequence: 4,
                    value: Some(b"hidden".to_vec()),
                },
            ],
        }];
        let ignores_routing = sources
            .iter()
            .flat_map(|source| &source.rows)
            .filter(|row| row.key == b"B")
            .max_by_key(|row| row.sequence)
            .and_then(|row| row.value.clone());
        assert!(compare_lookup(oracle(&sources, 10, b"B"), Ok(ignores_routing)).is_err());
    }

    #[test]
    fn coreimport_leveldb_mutated_sequence_and_tombstone_selection_fail() {
        let sources = vec![
            Source {
                level: Some(0),
                number: 10,
                layout: 0,
                rows: vec![Row {
                    key: b"a".to_vec(),
                    sequence: 10,
                    value: None,
                }],
            },
            Source {
                level: Some(1),
                number: 1,
                layout: 0,
                rows: vec![Row {
                    key: b"a".to_vec(),
                    sequence: 20,
                    value: Some(b"old".to_vec()),
                }],
            },
        ];
        let maximum_sequence = sources
            .iter()
            .flat_map(|s| &s.rows)
            .filter(|row| row.key == b"a")
            .max_by_key(|row| row.sequence)
            .and_then(|row| row.value.clone());
        assert!(compare_lookup(oracle(&sources, 20, b"a"), Ok(maximum_sequence)).is_err());
        let valid = vec![Source {
            level: Some(0),
            number: 10,
            layout: 0,
            rows: vec![
                Row {
                    key: b"a".to_vec(),
                    sequence: 20,
                    value: None,
                },
                Row {
                    key: b"a".to_vec(),
                    sequence: 10,
                    value: Some(b"old".to_vec()),
                },
            ],
        }];
        assert_eq!(oracle(&valid, 20, b"a"), Ok(None));
        let ignored_tombstone = valid
            .iter()
            .flat_map(|s| &s.rows)
            .filter(|row| row.key == b"a" && row.value.is_some())
            .max_by_key(|row| row.sequence)
            .and_then(|row| row.value.clone());
        assert!(compare_lookup(oracle(&valid, 20, b"a"), Ok(ignored_tombstone)).is_err());
    }

    #[test]
    fn coreimport_leveldb_mutated_inspection_refusal_fails() {
        let dir = env::temp_dir().join(format!("coreimport-leveldb-mutant-{}", process::id()));
        DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let result = inspect_image_with(&[1, 0, 0, 0, 0, 0, 0, 0, 1], &dir, |mut source| {
            source.refusal = None;
            source
        });
        fs::remove_dir_all(&dir).unwrap();
        assert!(result.is_err());
        check(&[6, 1, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap();
    }

    #[test]
    fn coreimport_leveldb_crc_repair_reaches_index_and_metadata() {
        let row = Row {
            key: b"a".to_vec(),
            sequence: 1,
            value: Some(vec![1]),
        };
        let original = structured_table(&[row], 0);
        let mut pos = original.len() - 48;
        let meta = decode_handle(&original, &mut pos).unwrap();
        let index = decode_handle(&original, &mut pos).unwrap();
        let mut bytes = original.clone();
        for (offset, _) in [meta, index] {
            bytes[offset] ^= 0x80;
        }
        repair_table(&mut bytes);
        for (offset, size) in [meta, index] {
            assert_eq!(
                bytes[offset + size + 1..offset + size + 5],
                checksum(&bytes[offset..=offset + size])
            );
        }
        for mode in 0..5 {
            check(&[mode, 0, 0, 0, 0, 1, 0, 0, 0]).unwrap();
        }
    }
}
