use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use bitcoin_hashes::sha256;
use dash_coreimport::leveldb::LevelDbReader;
use dash_coreimport::source::{Refusal, inspect};
use dash_coreimport::{hash_from_hex, hash_to_hex};
use serde_json::Value;

const MAGIC: [u8; 4] = [0xfc, 0xc1, 0xb7, 0xdc];
const CASES: [&str; 7] =
    ["a-wal", "b-l0", "c-overwrite", "d-deleted", "e-multilevel", "f-pruned", "g-reindex"];
const DERIVED_CASES: [&str; 3] = ["h-nonzero-key", "absent-obfuscation-key", "zero-tail-blk"];
const OBFUSCATE_KEY: &[u8] = b"\x0e\0obfuscate_key";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/leveldb")
}

fn fixture(case: &str) -> PathBuf {
    data_dir().join(case)
}

fn expected(case: &str) -> Value {
    serde_json::from_slice(&fs::read(fixture(case).join("expected.json")).unwrap()).unwrap()
}

fn unhex(value: &str) -> Vec<u8> {
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

fn selected<'a>(
    expected: &'a Value,
    db: &str,
    key: &[u8],
) -> Result<Option<&'a Value>, &'static str> {
    let hex: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    let rows = expected["audit"][db]["versions"].as_array().unwrap();
    let mut logs: Vec<_> = rows.iter().filter(|row| row["level"] == "wal").collect();
    logs.sort_by_key(|row| (row["file"].as_str().unwrap(), row["offset"].as_u64().unwrap()));
    if logs
        .windows(2)
        .any(|w| w[0]["sequence"].as_u64().unwrap() >= w[1]["sequence"].as_u64().unwrap())
    {
        return Err("log sequence order");
    }
    let priority = |row: &&Value| {
        let level = row["level"].as_u64().map_or(0, |level| level + 1);
        let number = Path::new(row["file"].as_str().unwrap())
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .parse::<u64>()
            .unwrap();
        (
            level,
            Reverse(if level <= 1 {
                number
            } else {
                0
            }),
            Reverse(row["sequence"].as_u64().unwrap()),
        )
    };
    let mut candidates: Vec<_> =
        rows.iter().filter(|row| row["key"].as_str() == Some(&hex)).collect();
    candidates.sort_by_key(priority);
    let winner = candidates.first().copied();
    if let Some(winner) = winner
        && candidates
            .iter()
            .any(|row| row["sequence"].as_u64().unwrap() > winner["sequence"].as_u64().unwrap())
    {
        return Err("source priority contradiction");
    }
    Ok(winner)
}

fn latest<'a>(expected: &'a Value, db: &str, key: &[u8]) -> Option<&'a Value> {
    selected(expected, db, key).unwrap()
}

fn verify(case: &str) {
    let root = fixture(case);
    let expected = expected(case);
    for (db, keys) in [
        ("chainstate", vec![b"B".as_slice(), OBFUSCATE_KEY]),
        ("blocks/index", vec![b"R".as_slice(), b"l", b"F\x10prunedblockfiles"]),
    ] {
        let reader = LevelDbReader::open(&root.join(db)).unwrap();
        for key in keys {
            let value = latest(&expected, db, key).and_then(|item| {
                (item["kind"] == 1).then(|| unhex(item["value"].as_str().unwrap()))
            });
            assert_eq!(reader.get(key).unwrap(), value, "{case}: {db}: {key:?}");
        }
        assert_eq!(reader.get(b"this-key-is-absent").unwrap(), None);
    }
    let genesis = hash_from_hex(expected["genesis"].as_str().unwrap()).unwrap();
    let source = inspect(&root, MAGIC, genesis).unwrap();
    assert_eq!(
        source.best_block.as_ref().map(hash_to_hex).as_deref(),
        expected["best_block"].as_str(),
        "{case}"
    );
    assert_eq!(source.pruned, expected["pruned"].as_bool().unwrap());
    assert_eq!(source.reindexing, expected["reindexing"].as_bool().unwrap());
    assert_eq!(source.reindexing, expected["decoded"]["R"].as_bool().unwrap());
    assert_eq!(source.pruned, expected["decoded"]["prunedblockfiles"].as_bool().unwrap());
    assert_eq!(source.last_file.map(u64::from), expected["decoded"]["last_file"].as_u64());
    assert_eq!(
        source.obfuscation_key.as_slice(),
        unhex(expected["decoded"]["obfuscation_key"].as_str().unwrap())
    );
    assert_eq!(
        source.best_block.as_ref().map(hash_to_hex).as_deref(),
        expected["decoded"]["B"].as_str()
    );
    assert_eq!(source.tail, source.files.last().unwrap().0);
    assert_eq!(source.net_dir, root);
    let last_file = latest(&expected, "blocks/index", b"l")
        .filter(|item| item["kind"] == 1)
        .map(|item| u32::from_le_bytes(unhex(item["value"].as_str().unwrap()).try_into().unwrap()));
    assert_eq!(source.last_file, last_file);
    let refusal = if source.reindexing {
        Some(Refusal::Reindexing)
    } else if source.pruned {
        Some(Refusal::Pruned)
    } else if source.best_block.is_none() {
        Some(Refusal::MissingBestBlock)
    } else {
        None
    };
    assert_eq!(source.refusal(), refusal, "{case}");
}

#[test]
fn a_key_only_in_wal() {
    assert_eq!(latest(&expected("a-wal"), "chainstate", b"B").unwrap()["level"], "wal");
    assert!(
        !expected("a-wal")["audit"]["chainstate"]["versions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["key"] == "42" && item["level"] != "wal")
    );
    verify("a-wal");
}

#[test]
fn b_key_only_in_l0() {
    let expected = expected("b-l0");
    assert_eq!(latest(&expected, "chainstate", OBFUSCATE_KEY).unwrap()["level"], 0);
    assert!(
        !expected["audit"]["chainstate"]["versions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["key"] == "0e006f62667573636174655f6b6579" && item["level"] == "wal")
    );
    verify("b-l0");
}

#[test]
fn c_wal_overwrites_a_table() {
    let expected = expected("c-overwrite");
    let latest = latest(&expected, "chainstate", b"B").unwrap();
    assert_eq!(latest["level"], "wal");
    assert!(expected["audit"]["chainstate"]["versions"].as_array().unwrap().iter().any(
        |item| item["key"] == "42"
            && item["kind"] == 1
            && item["level"].is_number()
            && item["value"] != latest["value"]
    ));
    verify("c-overwrite");
}

#[test]
fn d_tombstone_hides_a_table_value() {
    let expected = expected("d-deleted");
    let latest = latest(&expected, "blocks/index", b"R").unwrap();
    assert_eq!(latest["kind"], 0);
    assert!(
        expected["audit"]["blocks/index"]["versions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["key"] == "52" && item["kind"] == 1 && item["level"].is_number())
    );
    verify("d-deleted");
}

#[test]
fn e_multiple_levels_after_forcecompactdb() {
    let expected = expected("e-multilevel");
    let levels = expected["audit"]["chainstate"]["levels"].as_array().unwrap();
    assert!(levels.iter().any(|level| level == 0));
    assert!(levels.iter().any(|level| level.as_u64().unwrap() > 0));
    assert!(latest(&expected, "chainstate", OBFUSCATE_KEY).unwrap()["level"].as_u64().unwrap() > 0);
    verify("e-multilevel");
}

#[test]
fn f_pruned_source_is_refused() {
    verify("f-pruned");
}

#[test]
fn g_interrupted_reindex_is_refused() {
    verify("g-reindex");
}

#[derive(Debug, PartialEq, Eq)]
struct FileState {
    size: u64,
    modified: SystemTime,
    sha256: [u8; 32],
}

fn file_manifest(root: &Path) -> BTreeMap<PathBuf, FileState> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, FileState>) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                visit(root, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    FileState {
                        size: meta.len(),
                        modified: meta.modified().unwrap(),
                        sha256: sha256::Hash::hash(&fs::read(&path).unwrap()).to_byte_array(),
                    },
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

#[test]
fn open_and_get_never_write_the_source() {
    for case in CASES.into_iter().chain(DERIVED_CASES) {
        let root = fixture(case);
        let before = file_manifest(&root);
        verify(case);
        assert_eq!(before, file_manifest(&root), "source changed: {case}");
        for db in ["chainstate", "blocks/index"] {
            for name in ["LOCK", "LOG", "LOG.old"] {
                assert!(!root.join(db).join(name).exists(), "{case}: created {name}");
            }
        }
    }
}

#[test]
fn mutations_fail_instead_of_returning_stale_values() {
    for (case, message) in
        [("torn-wal", "torn"), ("bad-crc", "checksum"), ("snappy", "compression type 1")]
    {
        let root = fixture(case);
        let before = file_manifest(&root);
        let error = LevelDbReader::open(&root.join("chainstate"))
            .and_then(|reader| reader.get(b"B"))
            .unwrap_err();
        assert!(error.to_string().contains(message), "{case}: {error}");
        assert_eq!(before, file_manifest(&root));
    }
}

fn copy_tree(src: &Path, dest: &Path) {
    fs::create_dir_all(dest).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest.join(entry.file_name()));
        } else {
            fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        }
    }
}

#[test]
fn inspection_refuses_missing_genesis_wrong_genesis_and_numbering_gaps() {
    let source = fixture("a-wal");
    let genesis = hash_from_hex(expected("a-wal")["genesis"].as_str().unwrap()).unwrap();
    assert_eq!(inspect(&source, MAGIC, [0; 32]).unwrap().refusal(), Some(Refusal::WrongGenesis));
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&source, temp.path());
    let blocks = temp.path().join("blocks");
    fs::remove_file(blocks.join("blk00000.dat")).unwrap();
    assert_eq!(
        inspect(temp.path(), MAGIC, genesis).unwrap().refusal(),
        Some(Refusal::NoGenesisFile)
    );
    fs::copy(source.join("blocks/blk00000.dat"), blocks.join("blk00000.dat")).unwrap();
    fs::copy(source.join("blocks/blk00000.dat"), blocks.join("blk00002.dat")).unwrap();
    assert_eq!(inspect(temp.path(), MAGIC, genesis).unwrap().refusal(), Some(Refusal::Gap(1)));
}

#[test]
fn captured_files_match_provenance_checksums() {
    let sums: BTreeMap<String, String> =
        serde_json::from_slice(&fs::read(data_dir().join("SHA256SUMS.json")).unwrap()).unwrap();
    for (path, expected) in sums {
        let hash = sha256::Hash::hash(&fs::read(data_dir().join(&path)).unwrap());
        assert_eq!(hash.to_byte_array().as_slice(), unhex(&expected), "{path}");
    }
}

#[test]
fn missing_wal_and_unsafe_current_are_errors() {
    let temp = tempfile::tempdir().unwrap();
    let db = temp.path().join("db");
    copy_tree(&fixture("a-wal").join("chainstate"), &db);
    fs::write(db.join("CURRENT"), b"../MANIFEST-000001\n").unwrap();
    assert!(LevelDbReader::open(&db).is_err());
    fs::write(db.join("CURRENT"), [b'0'; 129]).unwrap();
    assert!(LevelDbReader::open(&db).is_err());
    fs::copy(fixture("a-wal").join("chainstate/CURRENT"), db.join("CURRENT")).unwrap();
    fs::write(db.join("000000.log"), b"ignored obsolete WAL").unwrap();
    assert!(LevelDbReader::open(&db).unwrap().get(b"B").unwrap().is_some());
    for entry in fs::read_dir(&db).unwrap() {
        let entry = entry.unwrap();
        if entry.path().extension().is_some_and(|ext| ext == "log") {
            fs::remove_file(entry.path()).unwrap();
        }
    }
    assert!(LevelDbReader::open(&db).is_err());
}

#[test]
fn nonzero_and_absent_obfuscation_keys_decode_b() {
    let expected = expected("h-nonzero-key");
    assert!(
        unhex(expected["decoded"]["obfuscation_key"].as_str().unwrap())
            .iter()
            .any(|byte| *byte != 0)
    );
    verify("h-nonzero-key");
    verify("absent-obfuscation-key");
}

#[test]
fn contradictory_logs_and_levels_refuse_without_writes() {
    for case in
        ["i-multiple-logs", "j-l0-older-tombstone", "k-l0-older-value", "l-lower-level-tombstone"]
    {
        assert!(selected(&expected(case), "chainstate", b"B").is_err());
        let root = fixture(case);
        let before = file_manifest(&root);
        assert!(LevelDbReader::open(&root.join("chainstate")).and_then(|r| r.get(b"B")).is_err());
        assert!(inspect(&root, MAGIC, [0; 32]).is_err());
        assert_eq!(before, file_manifest(&root));
    }
}

#[test]
fn malformed_encodings_of_all_five_values_are_refused_without_writes() {
    for (case, message) in [
        ("malformed-R", "reindex flag"),
        ("malformed-pruned", "prune flag"),
        ("malformed-last-file", "last block file"),
        ("negative-last-file", "negative file number"),
        ("malformed-key", "obfuscation key"),
        ("malformed-key-vector", "obfuscation key"),
        ("malformed-key-length", "obfuscation key"),
        ("malformed-B", "best block"),
    ] {
        let root = fixture(case);
        let before = file_manifest(&root);
        let genesis = hash_from_hex(expected(case)["genesis"].as_str().unwrap()).unwrap();
        let error = inspect(&root, MAGIC, genesis).unwrap_err();
        assert!(error.to_string().contains(message), "{case}: {error}");
        assert_eq!(before, file_manifest(&root), "source changed: {case}");
    }
}

#[test]
fn best_block_must_exist_and_appear_in_the_scanned_headers() {
    for (case, refusal) in [
        ("missing-B", Refusal::MissingBestBlock),
        ("best-not-in-files", Refusal::BestBlockNotInFiles),
    ] {
        let root = fixture(case);
        let before = file_manifest(&root);
        let genesis = hash_from_hex(expected(case)["genesis"].as_str().unwrap()).unwrap();
        assert_eq!(inspect(&root, MAGIC, genesis).unwrap().refusal(), Some(refusal));
        assert_eq!(before, file_manifest(&root));
    }
    let root = fixture("a-wal");
    let genesis = hash_from_hex(expected("a-wal")["genesis"].as_str().unwrap()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&root, temp.path());
    let path = temp.path().join("blocks/blk00000.dat");
    let mut bytes = fs::read(&path).unwrap();
    let first_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    bytes.truncate(8 + first_len);
    fs::write(path, bytes).unwrap();
    assert_eq!(
        inspect(temp.path(), MAGIC, genesis).unwrap().refusal(),
        Some(Refusal::BestBlockNotInFiles)
    );
}

#[test]
fn all_block_records_are_checked_and_only_zero_preallocation_is_ignored() {
    let root = fixture("a-wal");
    let original = fs::read(root.join("blocks/blk00000.dat")).unwrap();
    let (records, _) = dash_coreimport::blockfiles::scan_block_records(&original, MAGIC);
    assert!(records.len() > 2);
    let genesis = hash_from_hex(expected("a-wal")["genesis"].as_str().unwrap()).unwrap();
    for case in ["torn-blk", "corrupt-blk", "malformed-blk", "bad-merkle-blk", "corrupt-later-blk"]
    {
        let root = fixture(case);
        let before = file_manifest(&root);
        let (filename, offset) = match case {
            "torn-blk" => ("blk00000.dat", records.last().unwrap().offset - 8),
            "malformed-blk" | "bad-merkle-blk" => ("blk00000.dat", records[1].offset),
            "corrupt-later-blk" => ("blk00001.dat", records[1].offset - 8),
            _ => ("blk00000.dat", records[1].offset - 8),
        };
        assert_eq!(
            inspect(&root, MAGIC, genesis).unwrap().refusal(),
            Some(Refusal::CorruptBlockFile {
                file: root.join("blocks").join(filename),
                offset: offset as u64
            }),
            "{case}"
        );
        assert_eq!(before, file_manifest(&root));
    }
    verify("zero-tail-blk");
}

#[test]
fn last_file_below_and_equal_to_tail_are_positive_inspections() {
    let root = fixture("a-wal");
    let genesis = hash_from_hex(expected("a-wal")["genesis"].as_str().unwrap()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    copy_tree(&root, temp.path());
    let equal = inspect(temp.path(), MAGIC, genesis).unwrap();
    assert_eq!(equal.last_file, Some(0));
    assert_eq!(equal.tail, 0);
    assert_eq!(equal.refusal(), None);
    fs::write(temp.path().join("blocks/blk00001.dat"), []).unwrap();
    let below = inspect(temp.path(), MAGIC, genesis).unwrap();
    assert_eq!(below.last_file, Some(0));
    assert_eq!(below.tail, 1);
    assert_eq!(below.refusal(), None);
}
