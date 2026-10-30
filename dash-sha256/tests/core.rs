//! Differential tests against Dash Core's own SHA-256 code.
//!
//! - `core_fixture`: digests Core computed (`contrib/core-harness`, `fixture` command) for inputs
//!   both sides generate from the same seeds. Always runs.
//! - `core_long_messages` (ignored by default): Core's digests of messages past 2^32 bits.
//! - `core_live`: when `DASH_SHA256_CORE_HARNESS` names a harness command (for example
//!   `qemu-x86_64 -cpu max /path/to/core-harness`, so both sides see the same CPU; it is split on
//!   whitespace, so its paths cannot contain spaces), random inputs go to Core and to this crate
//!   under each of Core's implementation selections, and the selections' names must match too.
//!   `DASH_SHA256_CORE_CASES` sets the number of SHA-256 cases per selection (default 2000).

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::{CORE_SELECTIONS, Rng, data, hex, implementations, sha256_split};
use dash_sha256::Implementation;

#[test]
fn core_fixture() {
    let json: serde_json::Value = serde_json::from_str(&data("core-fixture.json")).unwrap();
    let sha256 = json["sha256"].as_array().unwrap();
    let d64 = json["sha256d64"].as_array().unwrap();
    assert_eq!((sha256.len(), d64.len()), (336, 34));
    for imp in implementations() {
        for case in sha256 {
            let len = case["len"].as_u64().unwrap() as usize;
            let mut rng = Rng(len as u64);
            let input = rng.bytes(len);
            let splits = rng.splits(len);
            let digest = case["digest"].as_str().unwrap();
            assert_eq!(hex(&sha256_split(imp, &input, &[])), digest, "len {len}, {}", imp.name());
            assert_eq!(
                hex(&sha256_split(imp, &input, &splits)),
                digest,
                "len {len}, {}",
                imp.name()
            );
        }
        for case in d64 {
            let n = case["n"].as_u64().unwrap() as usize;
            let input = Rng(1_000_000 + n as u64).bytes(64 * n);
            let input = input.as_chunks::<64>().0;
            let mut out = vec![[0; 32]; n];
            imp.sha256d64(&mut out, input);
            assert_eq!(
                hex(out.as_flattened()),
                case["digests"].as_str().unwrap(),
                "n {n}, {}",
                imp.name()
            );
        }
    }
}

/// Messages around 2^29 bytes, where the bit length passes 2^32 (`core-long.json`). Each
/// implementation hashes 512 MiB, so this runs only when asked:
/// `cargo test -p dash-sha256 --release --test core -- --ignored`.
#[test]
#[ignore = "hashes 512 MiB per implementation; run with --release -- --ignored"]
fn core_long_messages() {
    let json: serde_json::Value = serde_json::from_str(&data("core-long.json")).unwrap();
    let pattern =
        Rng(json["seed"].as_u64().unwrap()).bytes(json["pattern_len"].as_u64().unwrap() as usize);
    let cases = json["sha256"].as_array().unwrap();
    assert_eq!(cases.len(), 7);
    for imp in implementations() {
        let mut h = dash_sha256::Sha256::with_implementation(imp);
        let mut pos = 0;
        for case in cases {
            let len = case["len"].as_u64().unwrap() as usize;
            while pos < len {
                let off = pos % pattern.len();
                let n = (pattern.len() - off).min(len - pos);
                h.update(&pattern[off..off + n]);
                pos += n;
            }
            assert_eq!(hex(&h.clone().finalize()), case["digest"], "len {len}, {}", imp.name());
        }
    }
}

/// A running harness.
struct Harness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Harness {
    fn spawn(command: &str) -> Harness {
        let mut words = command.split_whitespace();
        let mut child = Command::new(words.next().expect("empty DASH_SHA256_CORE_HARNESS"))
            .args(words)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawning {command}: {e}"));
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Harness {
            child,
            stdin,
            stdout,
        }
    }

    fn ask(&mut self, request: &str) -> String {
        writeln!(self.stdin, "{request}").unwrap();
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        let line = line.trim_end().to_owned();
        assert!(!line.starts_with("error"), "harness: {line} (request {:.80})", request);
        line
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A harness argument: `-` stands for an empty one.
fn arg(s: String) -> String {
    if s.is_empty() {
        "-".to_owned()
    } else {
        s
    }
}

/// Lengths around every block boundary up to 4 blocks, then anything up to 20 KB.
fn random_len(rng: &mut Rng) -> usize {
    match rng.next() % 4 {
        0 => (rng.next() % 260) as usize,
        1 => 64 * (rng.next() % 5) as usize + (rng.next() % 9) as usize + 51,
        2 => (rng.next() % 2048) as usize,
        _ => (rng.next() % 20_000) as usize,
    }
}

#[test]
fn core_live() {
    let Ok(command) = std::env::var("DASH_SHA256_CORE_HARNESS") else {
        eprintln!("core_live: DASH_SHA256_CORE_HARNESS is not set, skipping");
        return;
    };
    let cases: usize = std::env::var("DASH_SHA256_CORE_CASES")
        .map_or(2000, |v| v.parse().expect("DASH_SHA256_CORE_CASES"));
    let mut core = Harness::spawn(&command);
    for (core_use, allow) in CORE_SELECTIONS {
        let imp = Implementation::detect(allow);
        assert_eq!(core.ask(&format!("name {core_use}")), imp.name(), "selection {core_use}");

        let mut rng = Rng(0x5348_4131 + core_use);
        for _ in 0..cases {
            let len = random_len(&mut rng);
            let input = rng.bytes(len);
            let splits = if rng.next().is_multiple_of(2) {
                Vec::new()
            } else {
                rng.splits(len)
            };
            let split_list = splits.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
            let request = format!("sha256 {core_use} {} {}", arg(split_list), arg(hex(&input)));
            assert_eq!(
                core.ask(&request),
                hex(&sha256_split(imp, &input, &splits)),
                "len {len}, {}",
                imp.name()
            );
        }
        for n in 0..=40 {
            let input = rng.bytes(64 * n);
            let mut out = vec![[0; 32]; n];
            imp.sha256d64(&mut out, input.as_chunks::<64>().0);
            let request = format!("d64 {core_use} {}", arg(hex(&input)));
            assert_eq!(core.ask(&request), hex(out.as_flattened()), "n {n}, {}", imp.name());
        }
    }
}
