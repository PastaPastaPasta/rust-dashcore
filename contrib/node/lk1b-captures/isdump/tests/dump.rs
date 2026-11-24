//! isdump against tiny LevelDB databases written here through the same C API.
use leveldb_sys::*;
use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("isdump-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn write_db(path: &Path, rows: &[(Vec<u8>, Vec<u8>)]) {
    let cpath = CString::new(path.to_str().unwrap()).unwrap();
    unsafe {
        let opts = leveldb_options_create();
        leveldb_options_set_create_if_missing(opts, 1);
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        let db = leveldb_open(opts, cpath.as_ptr(), &mut err);
        assert!(err.is_null(), "open: {}", CStr::from_ptr(err).to_string_lossy());
        let wopts = leveldb_writeoptions_create();
        for (k, v) in rows {
            leveldb_put(
                db,
                wopts,
                k.as_ptr().cast(),
                k.len(),
                v.as_ptr().cast(),
                v.len(),
                &mut err,
            );
            assert!(err.is_null(), "put");
        }
        leveldb_writeoptions_destroy(wopts);
        leveldb_close(db);
        leveldb_options_destroy(opts);
    }
}

fn inversed_height_key(prefix: &[u8], height: u32, hash: [u8; 32]) -> Vec<u8> {
    [prefix, &(u32::MAX - height).to_be_bytes(), &hash].concat()
}

/// Rows shaped as Core's `CInstantSendDb` writes them (`instantsend/db.cpp:41-115`), inserted out
/// of key order, plus one foreign key.
fn is_rows() -> Vec<(Vec<u8>, Vec<u8>)> {
    let lock = [0x11; 32];
    let txid = [0x22; 32];
    vec![
        (inversed_height_key(b"\x04is_m", 120, lock), vec![0x01]),
        ([&b"\x04is_i"[..], &lock].concat(), vec![0x01, 0x02, 0x03]),
        (b"\x04is_v".to_vec(), vec![0x01, 0x00, 0x00, 0x00]),
        ([&b"\x05is_tx"[..], &txid].concat(), lock.to_vec()),
        ([&b"\x05is_in"[..], &txid, &1u32.to_le_bytes()].concat(), lock.to_vec()),
        (inversed_height_key(b"\x05is_a1", 7, lock), vec![0x01]),
        ([&b"\x05is_a2"[..], &lock].concat(), vec![0x01]),
        (b"zz-foreign".to_vec(), vec![]),
    ]
}

#[test]
fn dumps_rows_in_key_order_with_tables() {
    let dir = fresh_dir("plain");
    let rows = is_rows();
    write_db(&dir, &rows);
    let dump = isdump::dump(&dir).unwrap();
    assert_eq!(dump.obfuscate_key, None);
    let mut sorted = rows.clone();
    sorted.sort();
    assert_eq!(dump.rows, sorted);
    let tables: Vec<_> = dump.rows.iter().map(|(k, _)| isdump::table(k)).collect();
    assert_eq!(
        tables,
        [
            Some("is_i"),
            Some("is_m"),
            Some("is_v"),
            Some("is_a1"),
            Some("is_a2"),
            Some("is_in"),
            Some("is_tx"),
            None
        ]
    );
    let json = dump.to_json();
    assert!(json.starts_with(
        "{\"format\":\"isdump/1\",\"obfuscated\":false,\"counts\":{\"is_a1\":1,\"is_a2\":1,\"is_i\":1,\
         \"is_in\":1,\"is_m\":1,\"is_tx\":1,\"is_v\":1,\"other\":1},\"rows\":[\n"
    ));
    assert!(json.contains(&format!(
        "{{\"t\":\"is_i\",\"k\":\"0469735f69{}\",\"v\":\"010203\"}},\n",
        "11".repeat(32)
    )));
    assert!(json.contains("{\"t\":\"is_v\",\"k\":\"0469735f76\",\"v\":\"01000000\"}"));
    assert!(json.ends_with("{\"t\":null,\"k\":\"7a7a2d666f726569676e\",\"v\":\"\"}\n]}\n"));
    // A second dump of the same copy renders the same bytes.
    assert_eq!(isdump::dump(&dir).unwrap().to_json(), json);
}

#[test]
fn undoes_a_nonzero_obfuscation_key() {
    let dir = fresh_dir("xor");
    let key = [0xa5, 0x5a, 0x00, 0xff, 0x01, 0x02, 0x03, 0x04];
    let plain = is_rows();
    let mut stored: Vec<_> = plain
        .iter()
        .map(|(k, v)| {
            (k.clone(), v.iter().enumerate().map(|(i, b)| b ^ key[i % key.len()]).collect())
        })
        .collect();
    stored.push((isdump::OBFUSCATE_KEY_KEY.to_vec(), [&[8u8][..], &key].concat()));
    write_db(&dir, &stored);
    let dump = isdump::dump(&dir).unwrap();
    assert_eq!(dump.obfuscate_key.as_deref(), Some(&key[..]));
    let mut sorted = plain;
    sorted.sort();
    assert_eq!(dump.rows, sorted);
    assert!(dump.to_json().contains("\"obfuscated\":true"));
}

#[test]
fn a_zero_obfuscation_key_is_no_obfuscation() {
    let dir = fresh_dir("zero");
    let mut rows = is_rows();
    rows.push((isdump::OBFUSCATE_KEY_KEY.to_vec(), [&[8u8][..], &[0u8; 8]].concat()));
    write_db(&dir, &rows);
    let dump = isdump::dump(&dir).unwrap();
    assert_eq!(dump.obfuscate_key, None);
    assert_eq!(dump.rows.len(), is_rows().len());
}

#[test]
fn an_empty_database_dumps_no_rows() {
    let dir = fresh_dir("empty");
    write_db(&dir, &[]);
    let json = isdump::dump(&dir).unwrap().to_json();
    assert!(json.ends_with("\"other\":0},\"rows\":[\n]}\n"), "{json}");
}

#[test]
fn a_missing_database_is_an_error() {
    let dir = fresh_dir("missing");
    let err = isdump::dump(&dir).unwrap_err();
    assert!(err.starts_with("open "), "{err}");
}
