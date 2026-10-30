//! The in-memory hash index of the active records (SB14 D3): `u64` entries, a 40-bit key from
//! the hash's first five bytes (internal byte order, as stored and hashed) over a 24-bit height.
//! A sorted run plus a hash-map buffer of up to [`BUFFER`] entries, merged in place from the back
//! into reused scratch (D14). Every candidate is verified by the caller against the stored hash,
//! bounded by the tip: an entry is a hint, never an answer.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// Height bits: heights must be below 2²⁴ (D3).
pub const HEIGHT_BITS: u32 = 24;
/// The first height an entry cannot hold.
pub const HEIGHT_CAP: u32 = 1 << HEIGHT_BITS;
const HEIGHT_MASK: u64 = (1 << HEIGHT_BITS) - 1;
/// The buffer's size at which it merges into the sorted run.
const BUFFER: usize = 65_536;

/// A hash's key: its first five bytes, little-endian.
pub fn key(hash: &[u8; 32]) -> u64 {
    u64::from_le_bytes([hash[0], hash[1], hash[2], hash[3], hash[4], 0, 0, 0])
}

/// The buffer's keys are already uniform: multiplying by an odd constant spreads them over the
/// table's low bits, which `HashMap` uses.
#[derive(Default)]
pub struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
        }
    }

    fn write_u64(&mut self, x: u64) {
        self.0 = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

#[derive(Default)]
pub struct Index {
    /// Sorted `key << 24 | height`.
    main: Vec<u64>,
    /// key → height, the newest entry of a key.
    buffer: HashMap<u64, u32, BuildHasherDefault<KeyHasher>>,
    /// Entries a newer one of the same key pushed out of the buffer.
    displaced: Vec<u64>,
    /// The merge's reused buffer (D14).
    scratch: Vec<u64>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Index")
            .field("main", &self.main.len())
            .field("buffer", &self.buffer.len())
            .finish_non_exhaustive()
    }
}

fn entry(key: u64, h: u32) -> u64 {
    key << HEIGHT_BITS | u64::from(h)
}

fn height(e: u64) -> u32 {
    (e & HEIGHT_MASK) as u32
}

impl Index {
    /// The index of `entries` (unsorted `key << 24 | height`), as open builds it.
    pub fn from_entries(mut entries: Vec<u64>) -> Self {
        entries.sort_unstable();
        entries.dedup();
        Index {
            main: entries,
            ..Index::default()
        }
    }

    /// Adds the record at `h` (below [`HEIGHT_CAP`], checked by the caller) whose hash has `key`.
    /// `tip` is the tip after the caller's write: a merge drops entries above it.
    #[inline]
    pub fn insert(&mut self, key: u64, h: u32, tip: u32) {
        debug_assert!(h < HEIGHT_CAP);
        if let Some(old) = self.buffer.insert(key, h) {
            if old != h {
                self.displaced.push(entry(key, old));
            }
        }
        if self.buffer.len() >= BUFFER {
            self.merge(tip);
        }
    }

    /// Drops the buffer's entries above `tip` (a lowering). The sorted run keeps its stale ones,
    /// which verification rejects, until the next merge.
    pub fn lower(&mut self, tip: Option<u32>) {
        let keep = |h: u32| tip.is_some_and(|t| h <= t);
        self.buffer.retain(|_, h| keep(*h));
        self.displaced.retain(|&e| keep(height(e)));
        if tip.is_none() {
            self.main.clear();
        }
    }

    /// Merges the buffer into the sorted run, in place from the back, dropping exact duplicates
    /// and every entry above `tip`.
    pub fn merge(&mut self, tip: u32) {
        let mut p = std::mem::take(&mut self.scratch);
        p.clear();
        p.extend(self.buffer.drain().map(|(k, h)| entry(k, h)));
        p.append(&mut self.displaced);
        p.sort_unstable();
        let n = self.main.len();
        self.main.resize(n + p.len(), 0);
        let (mut i, mut j, mut k) = (n, p.len(), n + p.len());
        while j > 0 {
            if i > 0 && self.main[i - 1] > p[j - 1] {
                self.main[k - 1] = self.main[i - 1];
                i -= 1;
            } else {
                self.main[k - 1] = p[j - 1];
                j -= 1;
            }
            k -= 1;
        }
        self.main.dedup();
        self.main.retain(|&e| height(e) <= tip);
        self.scratch = p;
    }

    /// Calls `f` with each candidate height of `key` until it returns `true`; whether it did.
    #[inline]
    pub fn candidates(&self, key: u64, mut f: impl FnMut(u32) -> bool) -> bool {
        let lo = key << HEIGHT_BITS;
        let mut i = self.main.partition_point(|&e| e < lo);
        while let Some(&e) = self.main.get(i).filter(|&&e| e >> HEIGHT_BITS == key) {
            if f(height(e)) {
                return true;
            }
            i += 1;
        }
        if let Some(&h) = self.buffer.get(&key) {
            if f(h) {
                return true;
            }
        }
        self.displaced.iter().any(|&e| e >> HEIGHT_BITS == key && f(height(e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(ix: &Index, k: u64) -> Vec<u32> {
        let mut v = Vec::new();
        ix.candidates(k, |h| {
            v.push(h);
            false
        });
        v.sort_unstable();
        v
    }

    #[test]
    fn candidates_span_the_run_the_buffer_and_displaced_entries() {
        let mut ix = Index::from_entries(vec![entry(5, 1), entry(5, 9), entry(6, 2)]);
        ix.insert(5, 10, 10);
        ix.insert(5, 11, 11);
        ix.insert(7, 12, 12);
        assert_eq!(all(&ix, 5), [1, 9, 10, 11]);
        ix.merge(12);
        assert_eq!(all(&ix, 5), [1, 9, 10, 11]);
        assert_eq!(all(&ix, 7), [12]);
        assert_eq!(all(&ix, 8), Vec::<u32>::new());
    }

    #[test]
    fn a_lowering_drops_buffered_entries_and_the_merge_the_rest() {
        let mut ix = Index::from_entries(vec![entry(5, 1), entry(5, 20)]);
        ix.insert(5, 21, 21);
        ix.lower(Some(10));
        assert_eq!(all(&ix, 5), [1, 20], "the run's stale entry waits for the merge");
        ix.insert(5, 11, 11);
        ix.merge(11);
        assert_eq!(all(&ix, 5), [1, 11]);
        ix.lower(None);
        assert_eq!(all(&ix, 5), Vec::<u32>::new());
    }

    #[test]
    fn keys_are_the_first_five_bytes() {
        let mut h = [0xff; 32];
        h[..5].copy_from_slice(&[1, 2, 3, 4, 5]);
        assert_eq!(key(&h), 0x05_0403_0201);
    }
}
