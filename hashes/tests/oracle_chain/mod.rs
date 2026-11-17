//! A chain dumped from a reference oracle by `contrib/dump-oracle-headers.py`, for the tests that
//! replay it (`x11_chain.rs`, and `dash/tests/block_hash_x11.rs` by path).
//!
//! `DASH_X11_CHAIN` names the dump directory (`headers.bin`, `hashes.bin`, `tip.txt`). A
//! directory without `hashes.bin` (a bare header dump) is checked by chain linkage: each
//! header's hash must be its successor's previous-block field, and the last must be the
//! `tip.txt` hash.

use std::path::PathBuf;
use std::str::FromStr;
use std::thread;

use dashcore_hashes::hash_x11;

pub struct Chain {
    headers: Vec<u8>,
    /// The oracle's hash of each header, in the node's byte order.
    hashes: Vec<[u8; 32]>,
}

impl Chain {
    /// Reads the chain `DASH_X11_CHAIN` names; panics if it is not set or not a dump.
    pub fn load() -> Chain {
        let dir = PathBuf::from(
            std::env::var_os("DASH_X11_CHAIN").expect("DASH_X11_CHAIN names a dump directory"),
        );
        let headers = std::fs::read(dir.join("headers.bin")).unwrap();
        assert!(
            !headers.is_empty() && headers.len() % 80 == 0,
            "headers.bin is not 80-byte records"
        );
        let n = headers.len() / 80;
        // tip.txt is "<height> <hash>", the hash in display order.
        let tip = std::fs::read_to_string(dir.join("tip.txt")).unwrap_or_default();
        let tip = tip
            .split_whitespace()
            .nth(1)
            .map(|h| <[u8; 32]>::from(hash_x11::Hash::from_str(h).unwrap()));
        let hashes: Vec<[u8; 32]> = match std::fs::read(dir.join("hashes.bin")) {
            Ok(raw) => {
                assert_eq!(raw.len(), n * 32, "hashes.bin does not match headers.bin");
                raw.chunks(32).map(|c| c.try_into().unwrap()).collect()
            }
            Err(_) => (0..n)
                .map(|i| match i + 1 < n {
                    true => headers[(i + 1) * 80 + 4..(i + 1) * 80 + 36].try_into().unwrap(),
                    false => tip.expect("a dump without hashes.bin needs tip.txt"),
                })
                .collect(),
        };
        if let Some(tip) = tip {
            assert_eq!(hashes[n - 1], tip, "tip.txt is not the last hash");
        }
        Chain {
            headers,
            hashes,
        }
    }

    pub fn len(&self) -> usize {
        self.hashes.len()
    }

    /// Calls `check(height, header, oracle hash)` for every header on up to 8 threads. `check`
    /// returns the names of the computations whose result differs; this returns every
    /// `(height, name)` it reported.
    pub fn mismatches(
        &self,
        check: impl Fn(usize, &[u8; 80], &[u8; 32]) -> Vec<&'static str> + Sync,
    ) -> Vec<(usize, &'static str)> {
        let n = self.len();
        let threads = thread::available_parallelism().map_or(1, |p| p.get()).min(8);
        let per = n.div_ceil(threads);
        let check = &check;
        thread::scope(|s| {
            let jobs: Vec<_> = (0..threads)
                .map(|t| {
                    s.spawn(move || {
                        let mut bad = Vec::new();
                        for h in t * per..((t + 1) * per).min(n) {
                            let raw = self.headers[h * 80..(h + 1) * 80].try_into().unwrap();
                            bad.extend(check(h, raw, &self.hashes[h]).into_iter().map(|c| (h, c)));
                        }
                        bad
                    })
                })
                .collect();
            jobs.into_iter().flat_map(|j| j.join().unwrap()).collect()
        })
    }
}
