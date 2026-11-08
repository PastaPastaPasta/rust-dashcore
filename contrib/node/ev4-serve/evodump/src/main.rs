//! Dumps every record of a LevelDB database (a copy of Core's `evodb/`) as `key_hex value_hex`
//! lines, in key order, through LevelDB's own C API (leveldb-sys builds LevelDB from source).
//! Core's evodb is not obfuscated; the tool refuses a database that holds an obfuscation key.
use leveldb_sys::*;
use std::ffi::{CStr, CString};
use std::io::Write;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let path = CString::new(std::env::args().nth(1).expect("usage: evodump DB")).unwrap();
    unsafe {
        let opts = leveldb_options_create();
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        let db = leveldb_open(opts, path.as_ptr(), &mut err);
        if !err.is_null() {
            panic!("open: {}", CStr::from_ptr(err).to_string_lossy());
        }
        let ropts = leveldb_readoptions_create();
        leveldb_readoptions_set_verify_checksums(ropts, 1);
        let it = leveldb_create_iterator(db, ropts);
        leveldb_iter_seek_to_first(it);
        let out = std::io::stdout();
        let mut out = out.lock();
        while leveldb_iter_valid(it) != 0 {
            let (mut kl, mut vl) = (0usize, 0usize);
            let k = std::slice::from_raw_parts(leveldb_iter_key(it, &mut kl) as *const u8, kl);
            let v = std::slice::from_raw_parts(leveldb_iter_value(it, &mut vl) as *const u8, vl);
            assert!(k != b"\x0e\x00obfuscate_key", "obfuscated database");
            writeln!(out, "{} {}", hex(k), hex(v)).unwrap();
            leveldb_iter_next(it);
        }
        let mut err: *const libc::c_char = std::ptr::null();
        leveldb_iter_get_error(it, &mut err as *mut _ as *const *const libc::c_char);
        assert!(err.is_null(), "iterator error");
        leveldb_iter_destroy(it);
        leveldb_close(db);
    }
}
