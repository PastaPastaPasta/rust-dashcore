//! BS8 differential: base-sdk's reads of the overlay messages against Dash
//! Core's own, from the Core-linked judge built by `../bs8-judge/build.sh`
//! (the v24 pin, DEC-82).
//!
//! usage:
//!   bs8-tool cases <judge> <bls_wrapper.json5> <rdc root> <corpus out>
//!   bs8-tool vectors <judge> <vectors file>
//!   bs8-tool fuzz <judge> <bls_wrapper.json5> <rdc root> <cmd> <seconds> [seed]
//!
//! `cases` reads the fixed cases (`seeds.rs`) and the regenerated O4
//! fixtures, prints a per-command table, and writes the hand-built cases
//! with Core's reads as base-sdk's `core_overlay.json5` body. `vectors`
//! does the same for lines `name cmd version hex` (Core's `messages.py`
//! encodings, `gen_vectors.py`, with the qdata limits). `fuzz` is `fuzz.rs`. Each exits 1 on a
//! mismatch or a write-back difference.

mod fuzz;
mod judge;
mod predict;
mod seeds;

use std::collections::BTreeMap;

use serde_json::{Value, json};

use judge::Judge;
use predict::{Case, Outcome, Read, compare, ours};
use seeds::Bls;

#[derive(Default)]
struct Tally {
    cases: u64,
    core_read: u64,
    matched: u64,
    reencodings: u64,
    mismatches: u64,
    strict_flags: u64,
    hashes: u64,
}

fn core_json(core: &Result<Read, String>, raw: &[u8]) -> Value {
    match core {
        Err(e) => json!({"ok": false, "class": predict::class(e)}),
        Ok(r) => {
            let mut v = json!({"ok": true, "consumed": r.consumed});
            // The write-back, where it is not the bytes read.
            if r.reenc != raw[..r.consumed.min(raw.len())] {
                v["reenc"] = json!(hex::encode(&r.reenc));
            }
            if let Some(c) = r.error_code {
                v["errorCode"] = json!(c);
            }
            if let Some(n) = r.vvec_size {
                v["vvecSize"] = json!(n);
            }
            if let Some(h) = &r.hash {
                v["hash"] = json!(hex::encode(h));
            }
            v
        }
    }
}

/// Judges each case; returns the tally per command and the corpus records.
/// Tallies go under `prefix` and the command.
fn judge_all(
    judge: &mut Judge,
    cases: &[(String, Case)],
    prefix: &str,
    tallies: &mut BTreeMap<String, Tally>,
) -> Vec<Value> {
    let mut records = Vec::new();
    for (name, c) in cases {
        let o = ours(c);
        let k = judge.read(c);
        let t = tallies.entry(format!("{prefix}{}", c.cmd)).or_default();
        t.cases += 1;
        t.core_read += u64::from(k.is_ok());
        t.hashes += u64::from(k.as_ref().is_ok_and(|r| r.hash.is_some()));
        match compare(&o, &k) {
            Outcome::Match => t.matched += 1,
            Outcome::Reencoding => t.reencodings += 1,
            Outcome::Mismatch(_) if predict::strict_flags(c, &o, &k) => t.strict_flags += 1,
            Outcome::Mismatch(why) => {
                t.mismatches += 1;
                println!(
                    "{}",
                    json!({"case": name, "cmd": c.cmd, "version": c.version, "why": why})
                );
            }
        }
        let mut record = json!({
            "name": name, "command": c.cmd, "tag": c.tag, "version": c.version, "legacy": c.legacy,
            "max": [c.max_a, c.max_b], "raw": hex::encode(&c.raw), "core": core_json(&k, &c.raw),
        });
        if predict::strict_flags(c, &o, &k) {
            record["strict"] = json!("flag");
        }
        records.push(record);
    }
    records
}

fn report(tallies: &BTreeMap<String, Tally>) -> bool {
    let mut ok = true;
    for (cmd, t) in tallies {
        println!(
            "{}",
            json!({"command": cmd, "cases": t.cases, "core_read": t.core_read, "match": t.matched,
                   "strict_flags_a12": t.strict_flags, "reencodings_plan7": t.reencodings,
                   "core_hashes": t.hashes,
                   "mismatches": t.mismatches})
        );
        ok &= t.mismatches == 0 && t.reencodings == 0 && t.matched + t.strict_flags == t.cases;
    }
    ok
}

fn named(cases: Vec<Case>) -> Vec<(String, Case)> {
    cases.into_iter().enumerate().map(|(i, c)| (format!("{}_{i}", c.cmd), c)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ok = match args.get(1).map(String::as_str) {
        Some("cases") if args.len() == 6 => {
            let mut judge = Judge::spawn(&args[2]);
            let bls = Bls::load(&args[3]);
            let hand = named(seeds::cases(&bls));
            let mut tallies = BTreeMap::new();
            let records = judge_all(&mut judge, &hand, "", &mut tallies);
            std::fs::write(&args[5], serde_json::to_string_pretty(&records).expect("json"))
                .expect("corpus out");
            // The O4 fixtures: Core round-trips them exactly, and so must
            // base-sdk without the node's rules.
            let fixtures: Vec<(String, Case)> = seeds::fixtures(&args[4]);
            judge_all(&mut judge, &fixtures, "fixture_", &mut tallies);
            for (name, c) in &fixtures {
                let exact = ours(c).is_ok_and(|r| r.consumed == c.raw.len() && r.reenc == c.raw);
                if !exact {
                    println!(
                        "{}",
                        json!({"fixture": name, "why": "base-sdk does not round-trip it"})
                    );
                }
                tallies.entry(format!("fixture_{}", c.cmd)).or_default().mismatches +=
                    u64::from(!exact);
            }
            report(&tallies)
        }
        Some("vectors") if args.len() == 4 => {
            let mut judge = Judge::spawn(&args[2]);
            let text = std::fs::read_to_string(&args[3]).expect("vectors file");
            let cases: Vec<(String, Case)> = text
                .lines()
                .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                .map(|l| {
                    let f: Vec<&str> = l.split_whitespace().collect();
                    let raw = hex::decode(f[3]).expect("vector hex");
                    let c = Case {
                        cmd: f[1].into(),
                        tag: f[0].into(),
                        version: f[2].parse().expect("version"),
                        legacy: false,
                        raw,
                        max_a: f.get(4).map_or(0, |n| n.parse().expect("max_a")),
                        max_b: f.get(5).map_or(0, |n| n.parse().expect("max_b")),
                    };
                    (f[0].to_string(), c)
                })
                .collect();
            let mut tallies = BTreeMap::new();
            judge_all(&mut judge, &cases, "", &mut tallies);
            // messages.py writes what Core writes: both read it whole.
            let mut whole = true;
            for (name, c) in &cases {
                let all = ours(c).is_ok_and(|r| r.consumed == c.raw.len());
                if !all {
                    println!(
                        "{}",
                        json!({"vector": name, "why": "base-sdk does not read it whole"})
                    );
                }
                whole &= all;
            }
            report(&tallies) && whole
        }
        Some("fuzz") if args.len() >= 7 => {
            let mut judge = Judge::spawn(&args[2]);
            let bls = Bls::load(&args[3]);
            let mut seeds = seeds::cases(&bls);
            seeds.extend(
                seeds::fixtures(&args[4])
                    .into_iter()
                    .map(|(_, c)| c)
                    .filter(|c| c.raw.len() < 65536),
            );
            let seconds = args[6].parse().expect("seconds");
            let seed = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(1);
            fuzz::run(&mut judge, &bls, &seeds, &args[5], seconds, seed)
        }
        _ => {
            eprintln!("usage: see the module docs");
            false
        }
    };
    if !ok {
        std::process::exit(1);
    }
}
