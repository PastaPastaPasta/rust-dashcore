//! Reads every row of a LevelDB database (a stopped copy of Core's `llmq/isdb`) through LevelDB's
//! own C API (leveldb-sys builds LevelDB from source) and renders the rows as stable JSON.
//!
//! Core's `CInstantSendDb` keys are `(std::string prefix, …)` tuples, so each key starts with the
//! prefix's compact-size length byte and its text (`instantsend/db.cpp:12-18` at v24.0.0). Values
//! are returned as stored, after undoing `CDBWrapper`'s XOR obfuscation if the database has a
//! non-zero obfuscation key (Core does not obfuscate the IS DB; the key row is then absent).
use leveldb_sys::*;
use std::ffi::{CStr, CString};
use std::path::Path;

/// The key of `CDBWrapper`'s obfuscation key row: `ser("\0obfuscate_key")`.
pub const OBFUSCATE_KEY_KEY: &[u8] = b"\x0e\x00obfuscate_key";

/// The IS DB tables, by their serialized key prefix (`instantsend/db.cpp:12-18`).
pub const TABLES: &[(&str, &[u8])] = &[
    ("is_i", b"\x04is_i"),
    ("is_tx", b"\x05is_tx"),
    ("is_in", b"\x05is_in"),
    ("is_m", b"\x04is_m"),
    ("is_a1", b"\x05is_a1"),
    ("is_a2", b"\x05is_a2"),
    ("is_v", b"\x04is_v"),
];

/// Every row of one database, in LevelDB's key order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dump {
    /// The obfuscation key, when the database has a non-zero one.
    pub obfuscate_key: Option<Vec<u8>>,
    /// `(key, value)` pairs in key order, values de-obfuscated, the key row left out.
    pub rows: Vec<(Vec<u8>, Vec<u8>)>,
}

/// The table a key belongs to, by its prefix; `None` for a key of no IS DB table.
pub fn table(key: &[u8]) -> Option<&'static str> {
    TABLES.iter().find(|(_, p)| key.starts_with(p)).map(|(name, _)| *name)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The obfuscation key a stored key row holds: `CDBWrapper` writes it as a serialized
/// `std::vector<unsigned char>` (compact-size length, then the bytes).
fn parse_obfuscate_key(value: &[u8]) -> Result<Vec<u8>, String> {
    let (&len, rest) = value.split_first().ok_or("empty obfuscation key row")?;
    if usize::from(len) != rest.len() || len >= 0xfd {
        return Err(format!("obfuscation key row of unexpected shape: {}", hex(value)));
    }
    Ok(rest.to_vec())
}

fn xor(value: &mut [u8], key: &[u8]) {
    for (i, b) in value.iter_mut().enumerate() {
        *b ^= key[i % key.len()];
    }
}

/// Reads every row of the LevelDB database at `path`. Opening a database can write to it (a new
/// log and manifest), so `path` must be a copy, never a live node's directory.
pub fn dump(path: &Path) -> Result<Dump, String> {
    let cpath = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    // SAFETY: every handle is created, used and destroyed here, on one thread; key and value
    // slices are copied out before the iterator moves.
    unsafe {
        let opts = leveldb_options_create();
        leveldb_options_set_paranoid_checks(opts, 1);
        let mut err: *mut libc::c_char = std::ptr::null_mut();
        let db = leveldb_open(opts, cpath.as_ptr(), &mut err);
        leveldb_options_destroy(opts);
        if !err.is_null() {
            let msg = CStr::from_ptr(err).to_string_lossy().into_owned();
            leveldb_free(err.cast());
            return Err(format!("open {}: {msg}", path.display()));
        }
        let ropts = leveldb_readoptions_create();
        leveldb_readoptions_set_verify_checksums(ropts, 1);
        let it = leveldb_create_iterator(db, ropts);
        leveldb_iter_seek_to_first(it);
        while leveldb_iter_valid(it) != 0 {
            let (mut kl, mut vl) = (0usize, 0usize);
            let k = std::slice::from_raw_parts(
                leveldb_iter_key(it, std::ptr::addr_of_mut!(kl).cast_const()) as *const u8,
                kl,
            );
            let v = std::slice::from_raw_parts(
                leveldb_iter_value(it, std::ptr::addr_of_mut!(vl).cast_const()) as *const u8,
                vl,
            );
            raw.push((k.to_vec(), v.to_vec()));
            leveldb_iter_next(it);
        }
        let mut err: *const libc::c_char = std::ptr::null();
        leveldb_iter_get_error(
            it,
            &mut err as *mut *const libc::c_char as *const *const libc::c_char,
        );
        let iter_err = (!err.is_null()).then(|| {
            let msg = CStr::from_ptr(err).to_string_lossy().into_owned();
            leveldb_free(err as *mut libc::c_void);
            msg
        });
        leveldb_iter_destroy(it);
        leveldb_readoptions_destroy(ropts);
        leveldb_close(db);
        if let Some(msg) = iter_err {
            return Err(format!("iterate {}: {msg}", path.display()));
        }
    }
    let mut out = Dump::default();
    if let Some(pos) = raw.iter().position(|(k, _)| k == OBFUSCATE_KEY_KEY) {
        let key = parse_obfuscate_key(&raw.remove(pos).1)?;
        if key.iter().any(|&b| b != 0) {
            out.obfuscate_key = Some(key);
        }
    }
    if let Some(key) = &out.obfuscate_key {
        raw.iter_mut().for_each(|(_, v)| xor(v, key));
    }
    out.rows = raw;
    Ok(out)
}

impl Dump {
    /// Stable JSON: fixed key order, one row per line, rows in key order, lowercase hex.
    ///
    /// ```text
    /// {"format":"isdump/1","obfuscated":false,"counts":{"is_a1":0,…,"other":0},"rows":[
    /// {"t":"is_i","k":"04…","v":"01…"},
    /// …
    /// ]}
    /// ```
    pub fn to_json(&self) -> String {
        let mut counts: Vec<(&str, usize)> = TABLES.iter().map(|(n, _)| (*n, 0)).collect();
        counts.sort();
        let mut other = 0usize;
        for (k, _) in &self.rows {
            match table(k) {
                Some(t) => counts.iter_mut().find(|(n, _)| *n == t).expect("listed").1 += 1,
                None => other += 1,
            }
        }
        let mut s = String::from("{\"format\":\"isdump/1\",\"obfuscated\":");
        s.push_str(if self.obfuscate_key.is_some() {
            "true"
        } else {
            "false"
        });
        s.push_str(",\"counts\":{");
        for (name, n) in &counts {
            s.push_str(&format!("\"{name}\":{n},"));
        }
        s.push_str(&format!("\"other\":{other}}},\"rows\":["));
        for (i, (k, v)) in self.rows.iter().enumerate() {
            s.push_str(if i == 0 {
                "\n"
            } else {
                ",\n"
            });
            let t = table(k).map_or("null".to_string(), |t| format!("\"{t}\""));
            s.push_str(&format!("{{\"t\":{t},\"k\":\"{}\",\"v\":\"{}\"}}", hex(k), hex(v)));
        }
        s.push_str("\n]}\n");
        s
    }
}
