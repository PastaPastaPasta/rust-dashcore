//! BS9 differential: base-sdk's reads of the DKG, signature share, CoinJoin
//! and governance messages against Dash Core's own, from the Core-linked judge built by
//! `../bs8-judge/build.sh <out> ../bs9-judge/judge.cpp` (the v24 pin,
//! DEC-82).
//!
//! usage:
//!   bs9-tool cases <judge> <bls_wrapper.json5> <corpus out>
//!   bs9-tool fuzz <judge> <bls_wrapper.json5> <cmd> <seconds> [seed]
//!
//! `cases` reads the fixed cases (`seeds.rs`), prints a per-command table,
//! and writes them with Core's reads for `make_corpus.py`, which selects
//! base-sdk's `core_overlay_ii.json5` and `core_overlay_iii.json5`. `fuzz`
//! is `fuzz.rs`. Each exits 1 on a mismatch or a write-back difference.

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
            if let Some(h) = &r.hash {
                v["hash"] = json!(hex::encode(h));
            }
            if let Some(h) = &r.sign_hash {
                v["signHash"] = json!(hex::encode(h));
            }
            v
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ok = match args.get(1).map(String::as_str) {
        Some("cases") if args.len() == 5 => {
            let mut judge = Judge::spawn(&args[2]);
            let bls = Bls::load(&args[3]);
            let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
            let mut records = Vec::new();
            for (i, c) in seeds::cases(&bls).iter().enumerate() {
                let name = format!("{}_{i}", c.cmd);
                let o = ours(c);
                let k = judge.read(c);
                let t = tallies.entry(c.cmd.clone()).or_default();
                t.cases += 1;
                t.core_read += u64::from(k.is_ok());
                t.hashes +=
                    u64::from(k.as_ref().is_ok_and(|r| r.hash.is_some() || r.sign_hash.is_some()));
                match compare(&o, &k) {
                    Outcome::Match => t.matched += 1,
                    Outcome::Reencoding => t.reencodings += 1,
                    Outcome::Mismatch(why) => {
                        t.mismatches += 1;
                        println!("{}", json!({"case": name, "tag": c.tag, "why": why}));
                    }
                }
                records.push(json!({
                    "name": name, "command": c.cmd, "tag": c.tag, "legacy": c.legacy,
                    "quorum": c.quorum, "version": c.version, "raw": hex::encode(&c.raw), "core": core_json(&k, &c.raw),
                }));
            }
            std::fs::write(&args[4], serde_json::to_string_pretty(&records).expect("json"))
                .expect("corpus out");
            let mut ok = true;
            for (cmd, t) in &tallies {
                println!(
                    "{}",
                    json!({"command": cmd, "cases": t.cases, "core_read": t.core_read, "match": t.matched,
                           "reencodings_plan7": t.reencodings, "core_hashes": t.hashes,
                           "mismatches": t.mismatches})
                );
                ok &= t.mismatches == 0 && t.reencodings == 0 && t.matched == t.cases;
            }
            ok
        }
        Some("fuzz") if args.len() >= 6 => {
            let mut judge = Judge::spawn(&args[2]);
            let bls = Bls::load(&args[3]);
            let seeds: Vec<Case> =
                seeds::cases(&bls).into_iter().filter(|c| c.raw.len() < 65536).collect();
            let seconds = args[5].parse().expect("seconds");
            let seed = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
            fuzz::run(&mut judge, &bls, &seeds, &args[4], seconds, seed)
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
