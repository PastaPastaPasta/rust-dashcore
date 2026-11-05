//! `diff_<cmd>`: mutations of the fixed cases, each read by base-sdk (with
//! the node's BLS rules) and by Core's judge. Mutations splice in BLS samples
//! of every outcome class and `VARINT`s, move a DKG case to another quorum's
//! parameters, and flip the process-wide BLS scheme.
//!
//! A mismatch (verdict or bytes read) is printed as one JSON line. A flag
//! different write-back is only counted (PLAN §7): when
//! `BS8_PRIVATE_CLASSIFIER` names an executable, each such case goes to its
//! stdin as one line (`cmd version legacy input ours core`, hex), and the
//! run fails unless it exits 0. A summary line follows every minute and at
//! the end. The run exits 1 on a mismatch, a write-back difference with no
//! classifier, or when no case ran.

use std::io::{BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::json;

use crate::judge::Judge;
use crate::predict::{Case, Outcome, compare, ours};
use crate::seeds::{Bls, QUORUMS, VERSIONS, varint};

/// xorshift64*: deterministic for a given seed.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
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

fn mutate(rng: &mut Rng, seed: &[u8], seeds: &[Case], bls: &Bls) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..=rng.below(4) {
        let len = b.len().max(1);
        let pos = rng.below(len);
        match rng.below(12) {
            0 => b.truncate(pos),
            1 if !b.is_empty() => b[pos] ^= 1 << rng.below(8),
            2 if !b.is_empty() => {
                b[pos] = [0x00, 0xfc, 0xfd, 0xfe, 0xff, 0x01, 0x02, 0x03, 0x04][rng.below(9)]
            }
            3 => {
                let n = [
                    0u64,
                    1,
                    2,
                    0xfc,
                    0xfd,
                    0xffff,
                    0x1000,
                    0x1001,
                    0x0200_0000,
                    0x0200_0001,
                    u64::MAX,
                ][rng.below(11)];
                let c = compact(n, rng.below(4));
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
                let other = &seeds[rng.below(seeds.len())].raw;
                let start = rng.below(other.len());
                let end = (start + rng.below(64)).min(other.len());
                b.splice(pos..pos, other[start..end].iter().copied());
            }
            7 | 8 => {
                // Overwrite a key or signature-sized window with a BLS sample.
                let pool = [&bls.sigs, &bls.pks, &bls.sks][rng.below(3)];
                let sample = &pool[rng.below(pool.len())].1;
                let end = (pos + sample.len()).min(b.len());
                b.splice(pos..end, sample.iter().copied());
            }
            9 => b.extend((0..rng.below(8)).map(|_| rng.next() as u8)),
            10 => {
                let n = [0, 1, 0x7f, 0x80, 0x4080, u32::MAX][rng.below(6)];
                let end = (pos + 1 + rng.below(5)).min(b.len());
                b.splice(pos..end, varint(n));
            }
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
        let path = std::env::var_os("BS8_PRIVATE_CLASSIFIER")?;
        let mut child = Command::new(path)
            .args([target, &seed.to_string()])
            .stdin(Stdio::piped())
            .spawn()
            .expect("BS8_PRIVATE_CLASSIFIER");
        let stdin = child.stdin.take().map(BufWriter::new);
        Some(Self {
            child,
            stdin,
        })
    }

    fn send(&mut self, c: &Case, ours: &[u8], core: &[u8]) {
        let Some(w) = self.stdin.as_mut() else {
            return;
        };
        let line = format!(
            "{} {} {} {} {} {}",
            c.cmd,
            c.version,
            u8::from(c.legacy),
            hex::encode(&c.raw),
            hex::encode(ours),
            hex::encode(core)
        );
        if writeln!(w, "{line}").is_err() {
            self.stdin = None;
        }
    }

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

/// Whether a run failed: no case ran, a case mismatched, a write-back
/// differed with no classifier to take it, or the classifier failed.
fn failed(runs: u64, mismatches: u64, reencodings: u64, private: Option<bool>) -> bool {
    runs == 0 || mismatches > 0 || private == Some(false) || (reencodings > 0 && private.is_none())
}

pub fn run(
    judge: &mut Judge,
    bls: &Bls,
    seeds: &[Case],
    cmd: &str,
    seconds: u64,
    seed: u64,
) -> bool {
    assert!(crate::seeds::COMMANDS.contains(&cmd), "unknown command {cmd}");
    let seeds: Vec<Case> = seeds.iter().filter(|c| c.cmd == cmd).cloned().collect();
    assert!(!seeds.is_empty(), "no seeds for {cmd}");
    let target = format!("diff_{cmd}");
    let mut rng = Rng(seed.max(1));
    let mut classifier = Classifier::from_env(&target, seed);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(seconds);
    let mut next_summary = started + Duration::from_secs(60);
    let (mut runs, mut accepted, mut mismatches, mut reencodings) = (0u64, 0u64, 0u64, 0u64);
    let summary = |runs, accepted, mismatches, reencodings, done: bool| {
        println!(
            "{}",
            json!({"summary": target, "done": done, "runs": runs, "core_read": accepted,
                   "mismatches": mismatches, "reencodings_plan7": reencodings,
                   "seconds": started.elapsed().as_secs()})
        );
    };
    while Instant::now() < deadline {
        if Instant::now() >= next_summary {
            summary(runs, accepted, mismatches, reencodings, false);
            next_summary += Duration::from_secs(60);
        }
        let s = &seeds[rng.below(seeds.len())];
        let mut c = s.clone();
        if rng.below(16) != 0 {
            c.raw = mutate(&mut rng, &s.raw, &seeds, bls);
        }
        if rng.below(4) == 0 {
            c.legacy = !c.legacy;
        }
        if crate::predict::is_dkg(cmd) && rng.below(4) == 0 {
            c.quorum = QUORUMS[rng.below(QUORUMS.len())];
        }
        if rng.below(4) == 0 {
            c.version = VERSIONS[rng.below(VERSIONS.len())];
        }
        let o = ours(&c);
        let k = judge.read(&c);
        runs += 1;
        accepted += u64::from(k.is_ok());
        match compare(&o, &k) {
            Outcome::Match => {}
            Outcome::Reencoding => {
                reencodings += 1;
                if let (Some(cl), Ok(o), Ok(k)) = (classifier.as_mut(), &o, &k) {
                    cl.send(&c, &o.reenc, &k.reenc);
                }
            }
            Outcome::Mismatch(why) => {
                mismatches += 1;
                println!(
                    "{}",
                    json!({"target": target, "why": why, "legacy": c.legacy, "version": c.version,
                           "quorum": c.quorum, "input": hex::encode(&c.raw)})
                );
            }
        }
    }
    summary(runs, accepted, mismatches, reencodings, true);
    let private = classifier.map(Classifier::finish);
    println!(
        "{}",
        json!({"private_classifier": match private { None => "off", Some(true) => "pass", Some(false) => "fail" }})
    );
    !failed(runs, mismatches, reencodings, private)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_fails_on_any_mismatch_or_none_run() {
        assert!(!failed(10, 0, 0, None));
        assert!(!failed(10, 0, 3, Some(true)));
        assert!(failed(10, 0, 3, None));
        assert!(failed(10, 1, 0, Some(true)));
        assert!(failed(0, 0, 0, None));
        assert!(failed(10, 0, 0, Some(false)));
    }
}
