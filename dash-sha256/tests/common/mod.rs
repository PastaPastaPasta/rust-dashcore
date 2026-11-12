//! Helpers shared by the integration tests.

#![allow(dead_code)]

use dash_sha256::{Implementation, Use};

/// Core's implementation selections, as `bench_dash` and the harness number them
/// (`sha256_implementation`): STANDARD, USE_SSE4, USE_SSE4_AND_AVX2, USE_SSE4_AND_SHANI, USE_ALL.
pub const CORE_SELECTIONS: [(u64, Use); 5] = [
    (0, Use::STANDARD),
    (1, Use::SSE4),
    (3, Use::SSE4.union(Use::AVX2)),
    (5, Use::SSE4.union(Use::SHANI)),
    (7, Use::ALL),
];

/// Every distinct implementation this CPU runs, the portable one first.
///
/// `DASH_SHA256_EXPECT`, if set, lists the implementation names (separated by `;`) that must be
/// among them, so a CI job fails rather than silently testing fewer backends.
pub fn implementations() -> Vec<&'static Implementation> {
    let mut out: Vec<&'static Implementation> = Vec::new();
    for (_, allow) in CORE_SELECTIONS {
        let imp = Implementation::detect(allow);
        if !out.iter().any(|seen| seen.name() == imp.name()) {
            out.push(imp);
        }
    }
    if let Ok(expected) = std::env::var("DASH_SHA256_EXPECT") {
        for name in expected.split(';').filter(|n| !n.is_empty()) {
            assert!(
                out.iter().any(|imp| imp.name() == name),
                "DASH_SHA256_EXPECT: {name} is not available, only {out:?}"
            );
        }
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// splitmix64, as in `contrib/core-harness/harness.cpp`.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }

    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| (self.next() >> 56) as u8).collect()
    }

    /// Write sizes in the style of Core's `TestVector`: random, often small, possibly zero, covering
    /// `len` bytes.
    pub fn splits(&mut self, len: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos < len {
            let piece = (self.next() % ((len - pos).div_ceil(2) + 1) as u64) as usize;
            out.push(piece);
            pos += piece;
        }
        out
    }
}

/// SHA-256 of `data` written in pieces of `splits`, the rest last.
pub fn sha256_split(imp: &'static Implementation, data: &[u8], splits: &[usize]) -> [u8; 32] {
    let mut h = dash_sha256::Sha256::with_implementation(imp);
    let mut pos = 0;
    for &len in splits {
        h.update(&data[pos..pos + len]);
        pos += len;
    }
    h.update(&data[pos..]);
    h.finalize()
}

/// Reads a file under `tests/data`.
pub fn data(path: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/data/{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("tests/data/{path}: {e}"))
}
