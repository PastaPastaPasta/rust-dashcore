//! `diff_p2p_tx` / `diff_p2p_block`: base-sdk's `tx`/`block` handler read
//! against Core's deserializer (O1 oracle) at the v24 pin, the only parity
//! reference (DEC-82).
//!
//! Core's `tx` and `block` handlers read one object with `vRecv >>` and ignore
//! the bytes after it, so the handler verdict is the oracle's decode verdict
//! and its `consumed`. Inputs are mutations of real transactions and blocks.
//! A mismatch is printed as one JSON line on stdout; a summary line follows
//! every minute and at the end. The run exits 1 on a mismatch, when no case
//! ran, or when the private classifier fails. A base-sdk re-encoding that differs from
//! Core's is only counted, when the node's `core_bytes` equal Core's: PLAN §7
//! handling (ask the manager). The rule does not tell such differences apart.
//!
//! An optional private classifier, by path from the env: when
//! `BS7_PRIVATE_CLASSIFIER` names an executable, it runs for the whole fuzz run
//! with the target and seed as arguments, each case counted apart goes to its
//! stdin as one line (`pin input ours core`, hex), and the run fails unless it
//! exits 0. Nothing about those cases is printed here.

use std::io::{BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use dash_oracle_sys::{ErrorKind, Oracle, Pin};
use dash_p2p_core::{CommandString, P2pMsg, ProtoCtx, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use serde_json::json;

/// xorshift64*: deterministic for a given seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

/// A CompactSize prefix of `n`, possibly in a non-canonical width.
fn compact(n: u64, width: usize) -> Vec<u8> {
    match width {
        0 if n < 0xfd => vec![n as u8],
        0 | 1 if n <= 0xffff => [&[0xfd][..], &(n as u16).to_le_bytes()].concat(),
        0..=2 if n <= 0xffff_ffff => [&[0xfe][..], &(n as u32).to_le_bytes()].concat(),
        _ => [&[0xff][..], &n.to_le_bytes()].concat(),
    }
}

fn mutate(rng: &mut Rng, seed: &[u8], seeds: &[Vec<u8>]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..=rng.below(4) {
        let len = b.len().max(1);
        let pos = rng.below(len);
        match rng.below(10) {
            0 => b.truncate(pos),
            1 if !b.is_empty() => b[pos] ^= 1 << rng.below(8),
            2 if !b.is_empty() => {
                b[pos] = [0x00, 0xfc, 0xfd, 0xfe, 0xff, 0x01, 0x7f, 0x80][rng.below(8)]
            }
            3 => {
                let n = [0u64, 1, 0xfc, 0xfd, 0xffff, 0x1_0000, 0x0200_0000, 0x0200_0001, u64::MAX]
                    [rng.below(9)];
                let width = rng.below(4);
                let c = compact(n, width);
                let end = (pos + 1 + rng.below(9)).min(b.len());
                b.splice(pos..end, c);
            }
            4 => {
                let extra: Vec<u8> = (0..rng.below(16)).map(|_| rng.next() as u8).collect();
                b.splice(pos..pos, extra);
            }
            5 if !b.is_empty() => {
                let end = (pos + 1 + rng.below(32)).min(b.len());
                b.drain(pos..end);
            }
            6 => {
                // Splice in a slice of another seed.
                let other = &seeds[rng.below(seeds.len())];
                let start = rng.below(other.len());
                let end = (start + rng.below(64)).min(other.len());
                b.splice(pos..pos, other[start..end].iter().copied());
            }
            7 if b.len() >= 4 => {
                // Version and type word: special types and versions around 3.
                let v: u32 =
                    [1u32, 2, 3, 0x0001_0003, 0x0005_0003, 0x0009_0003, 0xffff_0003, 0x0003]
                        [rng.below(8)];
                b[..4].copy_from_slice(&v.to_le_bytes());
            }
            8 => b.extend((0..rng.below(8)).map(|_| rng.next() as u8)),
            _ => {}
        }
    }
    b
}

/// The optional private classifier (see the module docs).
struct Classifier {
    child: Child,
    stdin: Option<BufWriter<ChildStdin>>,
}

impl Classifier {
    fn from_env(target: &str, seed: u64) -> Option<Self> {
        let path = std::env::var_os("BS7_PRIVATE_CLASSIFIER")?;
        let mut child = Command::new(path)
            .args([target, &seed.to_string()])
            .stdin(Stdio::piped())
            .spawn()
            .expect("BS7_PRIVATE_CLASSIFIER");
        let stdin = child.stdin.take().map(BufWriter::new);
        Some(Self { child, stdin })
    }

    /// Sends one case; a classifier that stopped reading fails the run.
    fn send(&mut self, pin: &str, input: &[u8], ours: &[u8], core: &[u8]) {
        let Some(w) = self.stdin.as_mut() else { return };
        let line = format!("{pin} {} {} {}", hex::encode(input), hex::encode(ours), hex::encode(core));
        if writeln!(w, "{line}").is_err() {
            self.stdin = None;
        }
    }

    /// Whether every case reached it and it exited 0.
    fn finish(mut self) -> bool {
        let Some(mut w) = self.stdin.take() else {
            let _ = self.child.wait();
            return false;
        };
        let flushed = w.flush().is_ok();
        drop(w);
        flushed && self.child.wait().is_ok_and(|s| s.success())
    }
}

/// A read's verdict: bytes read and the re-encoding, or the error class
/// (reported, not compared).
type Read = Result<(usize, Vec<u8>), &'static str>;

fn rust_read(cmd: &CommandString, payload: &[u8]) -> Read {
    let ctx = ProtoCtx::new(ProtocolVersion::V24_0, BlsCtx::BASIC);
    match P2pMsg::decode_with_policy(cmd, payload, &ctx) {
        Ok(d) => {
            let mut re = Vec::new();
            d.msg.encode_payload(&mut re);
            Ok((d.consumed, re))
        }
        Err(e) => Err(crate::class::ours(cmd.as_str(), &e)),
    }
}

/// The node's own read (CS0): bytes read and `core_bytes`, what rdashd keeps
/// and relays.
fn node_read(block: bool, payload: &[u8]) -> Option<(usize, Vec<u8>)> {
    let mut cursor = payload;
    if block {
        dash_consensus::decode_block(&mut cursor)
            .ok()
            .map(|d| (d.wire().len(), d.core_bytes().as_slice().to_vec()))
    } else {
        dash_consensus::decode_tx(&mut cursor)
            .ok()
            .map(|d| (d.wire().len(), d.core_bytes().as_slice().to_vec()))
    }
}

/// Compares one input at one pin: `None` when it matches, else the reason.
/// A base-sdk re-encoding that differs from Core's counts as the allowlisted
/// class only when the node's own `core_bytes` equal Core's.
fn compare(
    rust: &Read,
    node: &Option<(usize, Vec<u8>)>,
    core: &dash_oracle_sys::Result<(usize, Vec<u8>)>,
) -> Result<bool, String> {
    match core {
        Ok((cc, cre)) => {
            let Ok((rc, rre)) = rust else { return Err(format!("base-sdk rejected: {rust:?}")) };
            if rc != cc {
                return Err(format!("bytes read {rc} vs {cc}"));
            }
            match node {
                Some((nc, nb)) if nc == cc && nb == cre => {}
                Some((nc, _)) if nc != cc => return Err(format!("node bytes read {nc} vs {cc}")),
                Some(_) => return Err("node serialization differs".into()),
                None => return Err("node rejected".into()),
            }
            // Allowlisted: PLAN §7 handling (ask the manager).
            Ok(rre != cre)
        }
        Err(e) if e.kind == ErrorKind::Decode => {
            // The verdict only: no Core logic observes the error class or
            // where the read stopped (DEC-98).
            if rust.is_ok() {
                return Err(format!("base-sdk read, Core threw {}", crate::class::core(&e.message)));
            }
            if node.is_some() {
                return Err("node accepted".into());
            }
            Ok(false)
        }
        Err(e) => Err(format!("oracle error {e}")),
    }
}

pub fn run(target: &str, args: &[String]) {
    let seed_file = &args[0];
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let seed = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1u64).max(1);
    let mut rng = Rng(seed);
    let seeds: Vec<Vec<u8>> = std::fs::read_to_string(seed_file)
        .expect("seed file")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| hex::decode(l.trim()).expect("hex seed"))
        .collect();
    let block = target == "fuzz-block";
    let cmd = if block { CommandString::BLOCK } else { CommandString::TX };
    let oracles = [(Pin::V24_0, Oracle::new(Pin::V24_0))];
    let mut classifier = Classifier::from_env(target, seed);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let (mut runs, mut accepted, mut mismatches, mut allowlisted) = (0u64, 0u64, 0u64, 0u64);
    let started = Instant::now();
    let mut next_summary = started + Duration::from_secs(60);
    let summary = |runs, accepted, mismatches, allowlisted, done: bool| {
        println!(
            "{}",
            json!({"summary": target, "done": done, "runs": runs, "accepted_pin_verdicts": accepted,
                   "mismatches": mismatches, "allowlisted_plan7": allowlisted,
                   "seconds": started.elapsed().as_secs()})
        );
    };
    while Instant::now() < deadline {
        if Instant::now() >= next_summary {
            summary(runs, accepted, mismatches, allowlisted, false);
            next_summary += Duration::from_secs(60);
        }
        let seed = &seeds[rng.below(seeds.len())];
        let input = if rng.below(16) == 0 { seed.clone() } else { mutate(&mut rng, seed, &seeds) };
        let rust = rust_read(&cmd, &input);
        let node = node_read(block, &input);
        runs += 1;
        for (pin, oracle) in &oracles {
            let core = if block {
                oracle.block_decode(&input).map(|d| (d.consumed, d.reencoded))
            } else {
                oracle.tx_decode(&input).map(|d| (d.consumed, d.reencoded))
            };
            match compare(&rust, &node, &core) {
                Ok(allowed) => {
                    accepted += u64::from(core.is_ok());
                    allowlisted += u64::from(allowed);
                    if let (true, Some(c), Ok((_, ours)), Ok((_, theirs))) =
                        (allowed, classifier.as_mut(), &rust, &core)
                    {
                        c.send(pin.name(), &input, ours, theirs);
                    }
                }
                Err(why) => {
                    mismatches += 1;
                    println!(
                        "{}",
                        json!({"target": target, "pin": pin.name(), "why": why,
                               "input": hex::encode(&input)})
                    );
                }
            }
        }
    }
    summary(runs, accepted, mismatches, allowlisted, true);
    let private = classifier.map(Classifier::finish);
    println!("{}", json!({"private_classifier": match private { None => "off", Some(true) => "pass", Some(false) => "fail" }}));
    if failed(runs, mismatches, private) {
        std::process::exit(1);
    }
}

/// Whether a fuzz run failed: no case ran, a case mismatched, or the private
/// classifier failed.
fn failed(runs: u64, mismatches: u64, private: Option<bool>) -> bool {
    runs == 0 || mismatches > 0 || private == Some(false)
}

/// Negative controls: the comparison fails on each difference it must catch.
#[cfg(test)]
mod tests {
    use super::*;

    fn core_ok(n: usize, bytes: &[u8]) -> dash_oracle_sys::Result<(usize, Vec<u8>)> {
        Ok((n, bytes.to_vec()))
    }

    fn core_err(what: &str) -> dash_oracle_sys::Result<(usize, Vec<u8>)> {
        Err(dash_oracle_sys::Error { kind: ErrorKind::Decode, message: what.into() })
    }

    #[test]
    fn a_run_fails_on_any_mismatch_or_none_run() {
        assert!(!failed(10, 0, None));
        assert!(!failed(10, 0, Some(true)));
        assert!(failed(10, 1, None));
        assert!(failed(10, 1, Some(true)));
        assert!(failed(0, 0, None));
        assert!(failed(10, 0, Some(false)));
    }

    #[test]
    fn matching_reads_pass() {
        let node = Some((3, vec![1, 2, 3]));
        assert_eq!(compare(&Ok((3, vec![1, 2, 3])), &node, &core_ok(3, &[1, 2, 3])), Ok(false));
        assert_eq!(
            compare(&Err("eof"), &None, &core_err("DataStream::read(): end of data")),
            Ok(false)
        );
    }

    #[test]
    fn every_difference_fails() {
        let node = Some((3, vec![1, 2, 3]));
        let core = core_ok(3, &[1, 2, 3]);
        // Wrong bytes read, base-sdk rejecting, the node differing or rejecting.
        assert!(compare(&Ok((2, vec![1, 2, 3])), &node, &core).is_err());
        assert!(compare(&Err("eof"), &node, &core).is_err());
        assert!(compare(&Ok((3, vec![1, 2, 3])), &Some((3, vec![9, 2, 3])), &core).is_err());
        assert!(compare(&Ok((3, vec![1, 2, 3])), &Some((2, vec![1, 2, 3])), &core).is_err());
        assert!(compare(&Ok((3, vec![1, 2, 3])), &None, &core).is_err());
        // Core rejecting: base-sdk accepting, the node accepting.
        let core = core_err("ReadCompactSize(): size too large");
        assert!(compare(&Ok((3, vec![])), &None, &core).is_err());
        assert!(compare(&Err("size"), &node, &core).is_err());
    }

    #[test]
    fn a_rejection_matches_whatever_its_class() {
        let core = core_err("ReadCompactSize(): size too large");
        assert_eq!(compare(&Err("eof"), &None, &core), Ok(false));
        assert_eq!(compare(&Err("other"), &None, &core_err("some new text")), Ok(false));
    }

    #[test]
    fn only_a_difference_the_node_reconciles_is_allowlisted() {
        let core = core_ok(3, &[1, 2, 3]);
        assert_eq!(compare(&Ok((3, vec![1, 9, 3])), &Some((3, vec![1, 2, 3])), &core), Ok(true));
        assert!(compare(&Ok((3, vec![1, 9, 3])), &Some((3, vec![1, 9, 3])), &core).is_err());
    }
}
