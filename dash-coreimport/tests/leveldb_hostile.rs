use std::fs;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use dash_coreimport::leveldb::LevelDbReader;
use dash_coreimport::regtest_import::{ObfuscateKey, obfuscate_key};
use dash_coreimport::source::inspect;

fn varint(mut n: u64, out: &mut Vec<u8>) {
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
}

fn slice(bytes: &[u8], out: &mut Vec<u8>) {
    varint(bytes.len() as u64, out);
    out.extend(bytes);
}

fn crc(bytes: &[u8]) -> [u8; 4] {
    let mut crc = !0;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f63b78 & 0u32.wrapping_sub(crc & 1));
        }
    }
    (!crc).rotate_right(15).wrapping_add(0xa282ead8).to_le_bytes()
}

fn append_record(record: &[u8], out: &mut Vec<u8>) {
    let mut pos = 0;
    let mut first = true;
    loop {
        let remaining = 32768 - out.len() % 32768;
        if remaining < 7 {
            out.extend(vec![0; remaining]);
        }
        let available = 32768 - out.len() % 32768 - 7;
        let len = available.min(record.len() - pos);
        let last = pos + len == record.len();
        let kind = match (first, last) {
            (true, true) => 1,
            (true, false) => 2,
            (false, false) => 3,
            (false, true) => 4,
        };
        let mut checked = vec![kind];
        checked.extend(&record[pos..pos + len]);
        out.extend(crc(&checked));
        out.extend((len as u16).to_le_bytes());
        out.push(kind);
        out.extend(&record[pos..pos + len]);
        pos += len;
        first = false;
        if last {
            break;
        }
    }
}

fn physical(record: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    append_record(record, &mut out);
    out
}

fn key(user: &[u8], seq: u64, kind: u8) -> Vec<u8> {
    let mut out = user.to_vec();
    out.extend(((seq << 8) | u64::from(kind)).to_le_bytes());
    out
}

fn entries(rows: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (key, value) in rows {
        out.push(0);
        slice(key, &mut out);
        let key_start = out.len() - key.len();
        let mut len = Vec::new();
        varint(value.len() as u64, &mut len);
        out.splice(key_start..key_start, len);
        out.extend(value);
    }
    out.extend([0, 0, 0, 0, 1, 0, 0, 0]);
    out
}

fn block(bytes: &[u8], out: &mut Vec<u8>) -> Vec<u8> {
    let mut handle = Vec::new();
    varint(out.len() as u64, &mut handle);
    varint(bytes.len() as u64, &mut handle);
    let mut checked = bytes.to_vec();
    checked.push(0);
    out.extend(&checked);
    out.extend(crc(&checked));
    handle
}

fn table(
    rows: &[(Vec<u8>, Vec<u8>)],
    separator: Option<&[u8]>,
    empty_index: bool,
    filter: Option<(&[u8], &[u8])>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let data = block(&entries(rows), &mut out);
    let meta_rows = filter
        .map(|(name, bytes)| {
            let handle = block(bytes, &mut out);
            vec![(name.to_vec(), handle)]
        })
        .unwrap_or_default();
    let meta = block(&entries(&meta_rows), &mut out);
    let index_rows = if empty_index {
        vec![]
    } else {
        vec![(separator.unwrap_or(&rows.last().unwrap().0).to_vec(), data)]
    };
    let index = block(&entries(&index_rows), &mut out);
    let footer = out.len();
    out.extend(meta);
    out.extend(index);
    out.resize(footer + 40, 0);
    out.extend(0xdb4775248b80fb57u64.to_le_bytes());
    out
}

fn header(last: u64) -> Vec<u8> {
    let mut out = vec![1];
    slice(b"leveldb.BytewiseComparator", &mut out);
    out.extend([2, 1, 4]);
    varint(last, &mut out);
    out
}

fn add_table(
    edit: &mut Vec<u8>,
    level: u8,
    number: u64,
    bytes: &[u8],
    smallest: &[u8],
    largest: &[u8],
) {
    edit.extend([7, level]);
    varint(number, edit);
    varint(bytes.len() as u64, edit);
    slice(smallest, edit);
    slice(largest, edit);
}

fn db(path: &Path, edit: &[u8], wal: &[u8]) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("CURRENT"), b"MANIFEST-000003\n").unwrap();
    fs::write(path.join("MANIFEST-000003"), physical(edit)).unwrap();
    fs::write(path.join("000001.log"), wal).unwrap();
}

fn batch(seq: u64, user: &[u8], value: Option<&[u8]>) -> Vec<u8> {
    let mut out = seq.to_le_bytes().to_vec();
    out.extend(1u32.to_le_bytes());
    out.push(u8::from(value.is_some()));
    slice(user, &mut out);
    if let Some(value) = value {
        slice(value, &mut out);
    }
    out
}

fn refuses(path: &Path, needle: &str) {
    let err = LevelDbReader::open(path).and_then(|r| r.get(b"B")).unwrap_err();
    assert!(err.to_string().contains(needle), "{err}");
}

#[test]
fn contradictory_manifest_bounds_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let actual = key(b"B", 10, 1);
    let bytes = table(&[(actual.clone(), b"new".to_vec())], None, false, None);
    let mut edit = header(10);
    let wrong = key(b"A", 10, 1);
    add_table(&mut edit, 0, 2, &bytes, &wrong, &wrong);
    db(temp.path(), &edit, &[]);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    refuses(temp.path(), "bounds");
}

#[test]
fn contradictory_index_separator_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let actual = key(b"B", 10, 1);
    let bytes = table(&[(actual.clone(), b"new".to_vec())], Some(&key(b"A", 10, 1)), false, None);
    let mut edit = header(10);
    add_table(&mut edit, 0, 2, &bytes, &actual, &actual);
    db(temp.path(), &edit, &[]);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    refuses(temp.path(), "separator");
}

#[test]
fn empty_index_over_data_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let actual = key(b"B", 10, 1);
    let bytes = table(&[(actual.clone(), b"new".to_vec())], None, true, None);
    let mut edit = header(10);
    add_table(&mut edit, 0, 2, &bytes, &actual, &actual);
    db(temp.path(), &edit, &[]);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    refuses(temp.path(), "empty index");
}

#[test]
fn lower_priority_higher_sequence_refuses() {
    for (wal, levels) in [(true, [0, 0]), (false, [0, 0]), (false, [0, 1])] {
        let temp = tempfile::tempdir().unwrap();
        let mut edit = header(30);
        for (number, level, seq) in [(4, levels[0], 20), (2, levels[1], 30)] {
            let k = key(b"B", seq, 1);
            let bytes = table(&[(k.clone(), vec![seq as u8])], None, false, None);
            add_table(&mut edit, level, number, &bytes, &k, &k);
            fs::write(temp.path().join(format!("{number:06}.ldb")), bytes).unwrap();
        }
        db(
            temp.path(),
            &edit,
            &if wal {
                physical(&batch(10, b"B", Some(b"wal")))
            } else {
                vec![]
            },
        );
        refuses(temp.path(), "priority");
    }
}

#[test]
fn above_last_sequence_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let k = key(b"B", 30, 1);
    let bytes = table(&[(k.clone(), b"future".to_vec())], None, false, None);
    let mut edit = header(20);
    add_table(&mut edit, 0, 2, &bytes, &k, &k);
    db(temp.path(), &edit, &[]);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    refuses(temp.path(), "LastSequence");
}

#[test]
fn bloom_excluding_present_key_refuses() {
    let temp = tempfile::tempdir().unwrap();
    let k = key(b"B", 10, 1);
    let filter = [0, 1, 0, 0, 0, 0, 2, 0, 0, 0, 11];
    let bytes = table(
        &[(k.clone(), b"hidden".to_vec())],
        None,
        false,
        Some((b"filter.leveldb.BuiltinBloomFilter2", &filter)),
    );
    let mut edit = header(10);
    add_table(&mut edit, 0, 2, &bytes, &k, &k);
    db(temp.path(), &edit, &[]);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    refuses(temp.path(), "filter");
}

#[test]
fn source_special_files_refuse_without_a_writer() {
    for fifo in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        db(&temp.path().join("blocks/index"), &header(1), &[]);
        db(&temp.path().join("chainstate"), &header(1), &[]);
        let path = temp.path().join("blocks/blk00001.dat");
        if fifo {
            assert!(Command::new("mkfifo").arg(&path).status().unwrap().success());
        } else {
            fs::write(temp.path().join("outside"), []).unwrap();
            symlink(temp.path().join("outside"), &path).unwrap();
        }
        assert!(inspect(temp.path(), [0; 4], [0; 32]).is_err());
    }
}

#[test]
fn overlong_varint32_fields_refuse_with_valid_checksums() {
    for len in 6..=10 {
        let temp = tempfile::tempdir().unwrap();
        let mut overlong_one = vec![0x81];
        overlong_one.extend(vec![0x80; len - 2]);
        overlong_one.push(0);
        let mut edit = overlong_one.clone();
        edit.extend(&header(1)[1..]);
        db(temp.path(), &edit, &[]);
        refuses(temp.path(), "varint32");

        let mut wal = 1u64.to_le_bytes().to_vec();
        wal.extend(1u32.to_le_bytes());
        wal.push(1);
        wal.extend(&overlong_one);
        wal.extend(b"B\x01v");
        db(temp.path(), &header(1), &physical(&wal));
        refuses(temp.path(), "varint32");

        let k = key(b"B", 1, 1);
        let mut data = vec![0x80; len - 1];
        data.push(0);
        varint(k.len() as u64, &mut data);
        data.push(1);
        data.extend(&k);
        data.push(b'v');
        data.extend([0, 0, 0, 0, 1, 0, 0, 0]);
        let mut bytes = Vec::new();
        let data_handle = block(&data, &mut bytes);
        let meta = block(&entries(&[]), &mut bytes);
        let index = block(&entries(&[(k.clone(), data_handle)]), &mut bytes);
        let footer = bytes.len();
        bytes.extend(meta);
        bytes.extend(index);
        bytes.resize(footer + 40, 0);
        bytes.extend(0xdb4775248b80fb57u64.to_le_bytes());
        let mut edit = header(1);
        add_table(&mut edit, 0, 2, &bytes, &k, &k);
        db(temp.path(), &edit, &[]);
        fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
        refuses(temp.path(), "varint32");
    }
}

#[test]
fn manifest_live_metadata_limits_precede_missing_table_errors() {
    for (count, key_len, needle) in
        [(262_145, 9, "live table limit"), (4097, 8200, "retained manifest key limit")]
    {
        let temp = tempfile::tempdir().unwrap();
        db(temp.path(), &header(1), &[]);
        let mut writer = fs::File::create(temp.path().join("MANIFEST-000003")).unwrap();
        let mut pending = physical(&header(1));
        let k = key(&vec![b'B'; key_len - 8], 1, 1);
        for number in 1..=count {
            let mut edit = Vec::new();
            add_table(&mut edit, 0, number, &[0; 48], &k, &k);
            append_record(&edit, &mut pending);
            while pending.len() >= 32768 {
                writer.write_all(&pending[..32768]).unwrap();
                pending.drain(..32768);
            }
        }
        writer.write_all(&pending).unwrap();
        drop(writer);
        refuses(temp.path(), needle);
    }
}

#[test]
fn oversize_batch_and_manifest_internal_keys_refuse() {
    let temp = tempfile::tempdir().unwrap();
    let wal = batch(1, &vec![0; 65529], None);
    let physical_log = physical(&wal);
    db(temp.path(), &header(1), &physical_log);
    refuses(temp.path(), "64 KiB");
    let mut edit = header(1);
    let k = key(&vec![0; 65529], 1, 1);
    add_table(&mut edit, 0, 2, &[0; 48], &k, &k);
    db(temp.path(), &edit, &[]);
    refuses(temp.path(), "64 KiB");
}

#[test]
fn index_obfuscation_and_last_file_boundaries() {
    for obfuscation in [None, Some(vec![8, 0, 0, 0, 0, 0, 0, 0, 0]), Some(vec![8; 9])] {
        let temp = tempfile::tempdir().unwrap();
        let mut wal = physical(&batch(1, b"l", Some(&0u32.to_le_bytes())));
        if let Some(key) = &obfuscation {
            wal.extend(physical(&batch(2, b"\x0e\0obfuscate_key", Some(key))));
        }
        db(&temp.path().join("blocks/index"), &header(2), &wal);
        db(&temp.path().join("chainstate"), &header(2), &[]);
        fs::write(temp.path().join("blocks/blk00000.dat"), []).unwrap();
        let result = inspect(temp.path(), [0; 4], [0; 32]);
        if obfuscation.as_ref().is_some_and(|key| key[1] != 0) {
            assert!(result.unwrap_err().to_string().contains("index obfuscation"));
        } else {
            let equal = result.unwrap();
            assert_eq!(equal.last_file, Some(0));
            assert_eq!(equal.tail, 0);
            fs::write(temp.path().join("blocks/blk00001.dat"), []).unwrap();
            let below = inspect(temp.path(), [0; 4], [0; 32]).unwrap();
            assert_eq!(below.last_file, Some(0));
            assert_eq!(below.tail, 1);
        }
    }
}

#[test]
fn stale_manifest_and_obsolete_valid_log_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    db(temp.path(), &header(10), &physical(&batch(10, b"B", Some(b"current"))));
    let k = key(b"B", 100, 1);
    let bytes = table(&[(k.clone(), b"stale manifest".to_vec())], None, false, None);
    fs::write(temp.path().join("000009.ldb"), &bytes).unwrap();
    let mut stale = header(100);
    add_table(&mut stale, 0, 9, &bytes, &k, &k);
    fs::write(temp.path().join("MANIFEST-000008"), physical(&stale)).unwrap();
    fs::write(temp.path().join("000000.log"), physical(&batch(200, b"B", Some(b"obsolete log"))))
        .unwrap();
    assert_eq!(
        LevelDbReader::open(temp.path()).unwrap().get(b"B").unwrap(),
        Some(b"current".to_vec())
    );
}

#[test]
fn every_block_and_both_separator_boundaries_are_checked() {
    for mutation in 0..3 {
        let temp = tempfile::tempdir().unwrap();
        let mut bytes = Vec::new();
        let mut index = Vec::new();
        let mut middle = 0;
        for (n, user) in [b"A", b"B", b"C"].into_iter().enumerate() {
            let k = key(user, 10, 1);
            if n == 1 {
                middle = bytes.len();
            }
            let data = block(&entries(&[(k.clone(), vec![n as u8])]), &mut bytes);
            let separator = if mutation == 1 && n == 0 {
                key(b"B", 10, 1)
            } else if mutation == 1 && n == 1 {
                key(b"B", 0, 1)
            } else {
                k
            };
            index.push((separator, data));
        }
        let meta = block(&entries(&[]), &mut bytes);
        let index = block(&entries(&index), &mut bytes);
        let footer = bytes.len();
        bytes.extend(meta);
        bytes.extend(index);
        bytes.resize(footer + 40, 0);
        bytes.extend(0xdb4775248b80fb57u64.to_le_bytes());
        if mutation == 2 {
            bytes[middle] ^= 1;
        }
        let mut edit = header(10);
        add_table(&mut edit, 0, 2, &bytes, &key(b"A", 10, 1), &key(b"C", 10, 1));
        db(temp.path(), &edit, &[]);
        fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
        let reader = LevelDbReader::open(temp.path()).unwrap();
        if mutation == 0 {
            assert_eq!(reader.get(b"B").unwrap(), Some(vec![1]));
        } else {
            let error = reader.get(b"A").unwrap_err();
            assert!(error.to_string().contains(if mutation == 1 {
                "separator"
            } else {
                "checksum"
            }));
        }
    }
}

#[test]
fn replayed_sequence_raises_last_sequence_and_batches_must_increase() {
    let temp = tempfile::tempdir().unwrap();
    let k = key(b"B", 20, 1);
    let bytes = table(&[(k.clone(), b"table".to_vec())], None, false, None);
    let mut edit = header(1);
    add_table(&mut edit, 0, 2, &bytes, &k, &k);
    db(temp.path(), &edit, &physical(&batch(30, b"other", Some(b"replayed"))));
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    assert_eq!(
        LevelDbReader::open(temp.path()).unwrap().get(b"B").unwrap(),
        Some(b"table".to_vec())
    );
    let mut log = physical(&batch(30, b"other", Some(b"replayed")));
    append_record(&batch(29, b"B", Some(b"older")), &mut log);
    fs::write(temp.path().join("000001.log"), log).unwrap();
    refuses(temp.path(), "strictly increasing");
}

#[test]
fn supported_bloom_accepts_and_other_filter_names_refuse() {
    for name in [b"filter.leveldb.BuiltinBloomFilter2".as_slice(), b"filter.other"] {
        let temp = tempfile::tempdir().unwrap();
        let k = key(b"B", 10, 1);
        let filter = [255, 1, 0, 0, 0, 0, 2, 0, 0, 0, 11];
        let bytes = table(&[(k.clone(), b"present".to_vec())], None, false, Some((name, &filter)));
        let mut edit = header(10);
        add_table(&mut edit, 0, 2, &bytes, &k, &k);
        db(temp.path(), &edit, &[]);
        fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
        if name == b"filter.other" {
            refuses(temp.path(), "filter name");
        } else {
            assert_eq!(
                LevelDbReader::open(temp.path()).unwrap().get(b"B").unwrap(),
                Some(b"present".to_vec())
            );
        }
    }
}

#[test]
fn adjacent_higher_level_files_follow_internal_key_order() {
    for numbers in [[2, 4], [4, 2]] {
        let temp = tempfile::tempdir().unwrap();
        let mut edit = header(30);
        for (number, sequence) in [(numbers[0], 30), (numbers[1], 20)] {
            let k = key(b"B", sequence, 1);
            let bytes = table(&[(k.clone(), vec![sequence as u8])], None, false, None);
            add_table(&mut edit, 1, number, &bytes, &k, &k);
            fs::write(temp.path().join(format!("{number:06}.ldb")), bytes).unwrap();
        }
        db(temp.path(), &edit, &[]);
        assert_eq!(LevelDbReader::open(temp.path()).unwrap().get(b"B").unwrap(), Some(vec![30]));
    }
}

#[test]
fn empty_batches_use_their_last_sequence_without_allowing_equal_starts() {
    let temp = tempfile::tempdir().unwrap();
    let k = key(b"B", 30, 1);
    let bytes = table(&[(k.clone(), b"table".to_vec())], None, false, None);
    let mut edit = header(1);
    add_table(&mut edit, 0, 2, &bytes, &k, &k);
    fs::write(temp.path().join("000002.ldb"), bytes).unwrap();
    for start in [30u64, 31] {
        let mut empty = start.to_le_bytes().to_vec();
        empty.extend(0u32.to_le_bytes());
        db(temp.path(), &edit, &physical(&empty));
        if start == 30 {
            refuses(temp.path(), "LastSequence");
        } else {
            assert_eq!(
                LevelDbReader::open(temp.path()).unwrap().get(b"B").unwrap(),
                Some(b"table".to_vec())
            );
            let mut replay = physical(&empty);
            append_record(&empty, &mut replay);
            fs::write(temp.path().join("000001.log"), replay).unwrap();
            refuses(temp.path(), "strictly increasing");
        }
    }
    db(temp.path(), &edit, &physical(&[0; 12]));
    refuses(temp.path(), "range underflow");
}

const OBFUSCATE: &[u8] = b"\x0e\0obfuscate_key";

fn obfuscation_value(byte: u8) -> Vec<u8> {
    let mut value = vec![8];
    value.extend([byte; 8]);
    value
}

/// A chainstate whose table holds the key at sequence 1, with `wal` as its log.
fn table_key_db(path: &Path, wal: &[u8]) {
    let k = key(OBFUSCATE, 1, 1);
    let bytes = table(&[(k.clone(), obfuscation_value(0xaa))], None, false, None);
    let mut edit = header(1);
    add_table(&mut edit, 0, 2, &bytes, &k, &k);
    db(path, &edit, wal);
    fs::write(path.join("000002.ldb"), bytes).unwrap();
}

fn key_of(path: &Path) -> ObfuscateKey {
    obfuscate_key(path).unwrap()
}

fn tolerant_refuses(path: &Path, needle: &str) {
    let err = obfuscate_key(path).unwrap_err();
    assert!(err.to_string().contains(needle), "{err}");
}

fn strict_refuses(path: &Path, needle: &str) {
    let err = LevelDbReader::open(path).and_then(|r| r.get(OBFUSCATE)).unwrap_err();
    assert!(err.to_string().contains(needle), "{err}");
}

#[test]
fn key_in_a_table_only_is_found() {
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &physical(&batch(2, b"B", Some(b"other"))));
    assert_eq!(key_of(temp.path()), ObfuscateKey::Present([0xaa; 8]));
}

#[test]
fn key_in_the_wal_only_is_found() {
    let temp = tempfile::tempdir().unwrap();
    db(temp.path(), &header(1), &physical(&batch(5, OBFUSCATE, Some(&obfuscation_value(0xbb)))));
    assert_eq!(key_of(temp.path()), ObfuscateKey::Present([0xbb; 8]));
}

#[test]
fn a_tombstoned_key_is_absent() {
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &physical(&batch(2, OBFUSCATE, None)));
    assert_eq!(key_of(temp.path()), ObfuscateKey::Absent);
    let temp = tempfile::tempdir().unwrap();
    db(temp.path(), &header(1), &physical(&batch(2, b"B", Some(b"other"))));
    assert_eq!(key_of(temp.path()), ObfuscateKey::Absent);
}

#[test]
fn newest_sequence_wins_across_table_and_wal() {
    let temp = tempfile::tempdir().unwrap();
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    append_record(&batch(3, OBFUSCATE, Some(&obfuscation_value(0xcc))), &mut wal);
    table_key_db(temp.path(), &wal);
    assert_eq!(key_of(temp.path()), ObfuscateKey::Present([0xcc; 8]));
}

#[test]
fn a_value_other_than_eight_bytes_is_malformed() {
    let mut short = obfuscation_value(1);
    short.pop();
    let mut long = obfuscation_value(1);
    long.push(1);
    let mut prefix = obfuscation_value(1);
    prefix[0] = 7;
    for value in [vec![], vec![8], short, long, prefix, vec![0xfd, 8, 0, 1, 1, 1, 1, 1, 1, 1, 1]] {
        let temp = tempfile::tempdir().unwrap();
        table_key_db(temp.path(), &physical(&batch(2, OBFUSCATE, Some(&value))));
        assert_eq!(key_of(temp.path()), ObfuscateKey::Malformed, "{value:02x?}");
    }
}

#[test]
fn a_truncated_final_wal_record_is_dropped_as_leveldb_does() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    let first = wal.len();
    append_record(&batch(3, OBFUSCATE, Some(&obfuscation_value(0xcc))), &mut wal);
    for len in 1..wal.len() {
        let temp = tempfile::tempdir().unwrap();
        table_key_db(temp.path(), &wal[..len]);
        let want = if len < first {
            0xaa
        } else {
            0xbb
        };
        assert_eq!(key_of(temp.path()), ObfuscateKey::Present([want; 8]), "len {len}");
        if len == first {
            assert!(LevelDbReader::open(temp.path()).is_ok());
        } else {
            let header = if len < first {
                len
            } else {
                len - first
            };
            strict_refuses(
                temp.path(),
                if header < 7 {
                    "torn log header"
                } else {
                    "torn log payload"
                },
            );
        }
    }
}

#[test]
fn a_fragmented_record_torn_at_the_end_is_dropped() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    append_record(&batch(3, OBFUSCATE, Some(&[0x77; 40_000])), &mut wal);
    assert!(wal.len() > 32768 + 7);
    for len in [32768 + 3, 32768 + 7, 32768 + 100, wal.len() - 1] {
        let temp = tempfile::tempdir().unwrap();
        table_key_db(temp.path(), &wal[..len]);
        assert_eq!(key_of(temp.path()), ObfuscateKey::Present([0xbb; 8]), "len {len}");
        strict_refuses(temp.path(), "torn log");
    }
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &wal);
    assert_eq!(key_of(temp.path()), ObfuscateKey::Malformed);
}

#[test]
fn a_fragment_open_at_a_clean_end_still_refuses() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    append_record(&batch(3, OBFUSCATE, Some(&[0x77; 40_000])), &mut wal);
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &wal[..32768]);
    tolerant_refuses(temp.path(), "torn fragmented log record");
    strict_refuses(temp.path(), "torn fragmented log record");
}

#[test]
fn a_checksum_failure_in_a_complete_record_is_an_error() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    let first = wal.len();
    append_record(&batch(3, OBFUSCATE, Some(&obfuscation_value(0xcc))), &mut wal);
    for at in [0, 4, 7, first - 1, first + 7, wal.len() - 1] {
        let mut bad = wal.clone();
        bad[at] ^= 0x01;
        let temp = tempfile::tempdir().unwrap();
        table_key_db(temp.path(), &bad);
        tolerant_refuses(temp.path(), "log");
        assert!(LevelDbReader::open(temp.path()).is_err());
    }
}

#[test]
fn a_zero_filled_tail_is_not_an_end_of_file_case() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    wal.extend([0; 64]);
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &wal);
    tolerant_refuses(temp.path(), "torn or nonzero log padding");
    strict_refuses(temp.path(), "torn or nonzero log padding");
}

#[test]
fn a_record_after_a_full_block_trailer_is_read() {
    let mut head = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    let filler = 32768 - 3 - 7 - head.len() - 18;
    append_record(&batch(3, b"F", Some(&vec![0x55; filler])), &mut head);
    assert_eq!(head.len(), 32768 - 3);
    for (newer, want) in [
        (Some(obfuscation_value(0xcc)), ObfuscateKey::Present([0xcc; 8])),
        (None, ObfuscateKey::Absent),
    ] {
        let mut wal = head.clone();
        append_record(&batch(4, OBFUSCATE, newer.as_deref()), &mut wal);
        assert_eq!(wal[32768 - 3..32768], [0; 3]);
        let temp = tempfile::tempdir().unwrap();
        table_key_db(temp.path(), &wal);
        assert_eq!(key_of(temp.path()), want);
        assert_eq!(
            LevelDbReader::open(temp.path()).unwrap().get(OBFUSCATE).unwrap().is_some(),
            newer.is_some()
        );
    }
}

#[test]
fn a_length_overrunning_a_full_block_refuses() {
    let mut wal = physical(&batch(2, OBFUSCATE, Some(&obfuscation_value(0xbb))));
    let first = wal.len();
    append_record(&batch(3, OBFUSCATE, Some(&[0x77; 40_000])), &mut wal);
    assert!(wal.len() > 32768 + 7);
    let size = u16::from_le_bytes([wal[first + 4], wal[first + 5]]);
    assert_eq!(first + 7 + usize::from(size), 32768);
    wal[first + 4..first + 6].copy_from_slice(&(size + 1).to_le_bytes());
    let temp = tempfile::tempdir().unwrap();
    table_key_db(temp.path(), &wal);
    tolerant_refuses(temp.path(), "torn log payload");
    strict_refuses(temp.path(), "torn log payload");
}
