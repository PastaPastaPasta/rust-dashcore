//! Shared test helpers.

#![allow(dead_code)]

use std::io::Read;
use std::process::{Command, Stdio};

use dash_x11::{Implementation, Use};

/// SplitMix64, as `contrib/core-harness/harness.cpp` implements it.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// The harness's `Generate`: `len` bytes from the next words, least significant byte first.
    pub fn fill(&mut self, out: &mut Vec<u8>, len: usize) {
        out.clear();
        while out.len() < len {
            let w = self.next().to_le_bytes();
            let n = (len - out.len()).min(8);
            out.extend_from_slice(&w[..n]);
        }
    }
}

/// The input lengths of a harness stream kind for case `i`.
pub fn kind_len(kind: &str, i: u64) -> usize {
    match kind {
        "random" => (i % 201) as usize,
        "long" => 201 + (i % 900) as usize,
        "header" => 80,
        "64" => 64,
        _ => panic!("unknown kind {kind}"),
    }
}

/// Every distinct implementation this CPU can run, the portable one first.
/// `DASH_X11_PORTABLE_ONLY=1` keeps only the portable one.
pub fn implementations() -> Vec<&'static Implementation> {
    // Detected once per test binary: each `detect` runs the self-test.
    static ALL: std::sync::OnceLock<Vec<&'static Implementation>> = std::sync::OnceLock::new();
    ALL.get_or_init(|| {
        if std::env::var_os("DASH_X11_PORTABLE_ONLY").is_some() {
            return vec![Implementation::standard()];
        }
        let mut out: Vec<&'static Implementation> = Vec::new();
        for allow in [Use::STANDARD, Use::SSE, Use::SSE | Use::AES, Use::SSE | Use::AVX2, Use::ALL]
        {
            let imp = Implementation::detect(allow);
            if !out.iter().any(|o| o.name() == imp.name()) {
                out.push(imp);
            }
        }
        out
    })
    .clone()
}

/// The harness commands named by `DASH_X11_CORE_HARNESS`, `;`-separated; each is split on
/// whitespace (so `qemu-x86_64 -cpu max /path/harness` works).
pub fn harnesses() -> Vec<Vec<String>> {
    std::env::var("DASH_X11_CORE_HARNESS")
        .map(|v| {
            v.split(';')
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.split_whitespace().map(String::from).collect())
                .collect()
        })
        .unwrap_or_default()
}

/// Runs a harness command and returns its stdout.
pub fn run(harness: &[String], args: &[&str]) -> Vec<u8> {
    let mut child = Command::new(&harness[0])
        .args(&harness[1..])
        .args(args)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("cannot run {harness:?}: {e}"));
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(child.wait().unwrap().success(), "{harness:?} {args:?} failed");
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A hash as Core prints it (`uint256::GetHex`, the bytes reversed).
pub fn display(hash: &[u8]) -> String {
    hash.iter().rev().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2), "odd hex");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

pub fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).map(|v| v.parse().unwrap()).unwrap_or(default)
}
