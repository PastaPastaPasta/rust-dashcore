//! O2b-4 4f2 (OD-L3-128 C-A): every `chain-v2` input without a spork intent runs as it did
//! before spork 19 was modelled and spork intents admitted. `tests/data/chain-v2-spork-free.json`
//! was written at node/o2b-4f 9dccab5bd, before any 4f2 change:
//!
//! `NODE_L1_SPORK_FREE_WRITE=1 cargo test -p node-l1 --features l3 --test l1_chain_spork_free`
//!
//! which writes the file and then fails, so that a write never passes as a comparison.
//!
//! For each of `diff_chain_s0` (S0), `diff_chain_par` (S1) and `diff_chain_av` (S2), over the
//! target's public seed programs and `generated(400)`, Core's session alone (CoreOnly) runs each
//! input with no `Spork` intent and records hashes of its op trace, its step classes and its raw
//! reports, its end, its census rows and a hash of the rest of its census. Each entry must be
//! reproduced exactly. On S0, each input with a spork intent also runs as it did without them
//! up to its first `SPORK` op: what an input means changes only from there (OD-L3-128 Q4).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;

use bitcoin_hashes::{HashEngine, sha256};
use dash_oracle_sys::Pin;
use node_l1::chain::core::Schedule;
use node_l1::chain::intent::{Encoding, Intent, Program, target_seeds};
use node_l1::chain::run::End;
use node_l1::chain::{Mode, Runner};
use serde_json::{Value, json};

const FINGERPRINT: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/chain-v2-spork-free.json");

/// The `chain-v2` targets and their schedules.
const TARGETS: [(&str, Schedule); 3] = [
    ("diff_chain_s0", Schedule::S0),
    ("diff_chain_par", Schedule::S1),
    ("diff_chain_av", Schedule::S2),
];

/// `n` inputs from a fixed xorshift stream, 8 to 207 bytes each (`l1_chain_diff.rs`'s).
fn generated(n: usize) -> Vec<(String, Vec<u8>)> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    (0..n)
        .map(|i| {
            let len = 8 + usize::try_from(next() % 200).unwrap();
            (format!("gen{i:02}"), (0..len).map(|_| next().to_le_bytes()[0]).collect())
        })
        .collect()
}

fn hash(parts: impl IntoIterator<Item = Vec<u8>>) -> String {
    let mut engine = sha256::HashEngine::default();
    for part in parts {
        engine.input(&(part.len() as u64).to_le_bytes());
        engine.input(&part);
    }
    sha256::Hash::from_engine(engine).to_byte_array().iter().map(|b| format!("{b:02x}")).collect()
}

/// The target's inputs: its public seed programs, then `generated(400)`.
fn inputs(target: &str) -> Vec<(String, Vec<u8>)> {
    let progs = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let mut inputs = target_seeds(&progs, target, Encoding::V2).unwrap();
    inputs.extend(generated(400));
    inputs
}

fn has_spork(program: &Program) -> bool {
    program.intents.iter().any(|i| matches!(i, Intent::Spork { .. }))
}

/// One target's entries, by input name, and how many of its inputs have a spork intent.
fn fingerprint(target: &str, schedule: Schedule) -> (BTreeMap<String, Value>, usize) {
    let mut runner = Runner::with_mode(Pin::V24_0, schedule, Mode::CoreOnly).unwrap();
    runner.encoding = Encoding::V2;
    runner.trace(true);
    let (mut entries, mut spork) = (BTreeMap::new(), 0);
    for (name, bytes) in inputs(target) {
        let program = Program::decode_v2(&bytes);
        if has_spork(&program) {
            spork += 1;
            continue;
        }
        let run = runner.run(&program).unwrap_or_else(|e| panic!("{target} {name}: {}", e.0));
        let end = match &run.end {
            End::Done => "done".to_string(),
            End::Tainted(op, sites) => format!("tainted {op} {sites:?}"),
            End::Unexpected(what) => format!("unexpected {what}"),
            other => panic!("{target} {name}: CoreOnly ended {other:?}"),
        };
        let c = &run.census;
        let rest = format!(
            "{:?} {:?} {:?} {} {:?}",
            c.ops, c.rejects, c.avoided, c.deferred_restarts, c.unexpected
        );
        let entry = json!({
            "trace": hash(run.trace.iter().map(|t| format!("{:?}", t.op).into_bytes())),
            "classes": hash(run.trace.iter().map(|t| format!("{:?}", t.step.class(&t.op)).into_bytes())),
            "reports": hash(run.raws.iter().flat_map(|r| {
                [r.reply.clone(), r.records.clone(), r.events.clone(), r.post.clone()]
            })),
            "end": end,
            "rows": c.rows,
            "census": hash([rest.into_bytes()]),
        });
        entries.insert(name, entry);
    }
    (entries, spork)
}

/// The ops and Core's step classes of `program`'s run on `runner`, admitting spork intents or not.
fn ops(runner: &mut Runner, program: &Program, admitted: bool) -> Vec<(String, String)> {
    runner.sporks_admitted = admitted;
    let run = runner.run(program).unwrap();
    run.trace
        .iter()
        .map(|t| (format!("{:?}", t.op), format!("{:?}", t.step.class(&t.op))))
        .collect()
}

/// 4f2 test 21 (OD-L3-128 C-A): every spork-free `chain-v2` input reproduces the base
/// fingerprint exactly; on S0 an input with a spork intent runs as without it up to its first
/// `SPORK` op.
#[test]
fn the_flip_changes_only_inputs_with_a_spork_intent() {
    let mut runner = Runner::with_mode(Pin::V24_0, Schedule::S0, Mode::CoreOnly).unwrap();
    runner.encoding = Encoding::V2;
    runner.trace(true);
    let (mut changed, mut unchanged) = (0, 0);
    for (name, bytes) in inputs("diff_chain_s0") {
        let program = Program::decode_v2(&bytes);
        if !has_spork(&program) {
            continue;
        }
        let (off, on) = (ops(&mut runner, &program, false), ops(&mut runner, &program, true));
        let first = on.iter().position(|(op, _)| op.starts_with("Spork"));
        match first {
            Some(k) => {
                assert_eq!(off[..k.min(off.len())], on[..k], "{name}: before its first SPORK op");
                changed += 1;
            }
            None => {
                assert_eq!(off, on, "{name}: no SPORK op ran");
                unchanged += 1;
            }
        }
    }
    eprintln!("S0 inputs with a spork intent: {changed} ran a SPORK op, {unchanged} did not");
    assert!(changed > 0, "a SPORK op ran");
    // One oracle session per thread.
    drop(runner);

    let mut targets = BTreeMap::new();
    for (target, schedule) in TARGETS {
        let (entries, spork) = fingerprint(target, schedule);
        eprintln!("{target}: {} spork-free inputs, {spork} with a spork intent", entries.len());
        targets.insert(target, json!({"schedule": schedule.name(), "entries": entries}));
    }
    let now = json!({"schema": "o2b4-4f2-spork-free/1", "targets": targets});
    if std::env::var_os("NODE_L1_SPORK_FREE_WRITE").is_some() {
        let text = serde_json::to_string_pretty(&now).unwrap() + "\n";
        std::fs::write(FINGERPRINT, text).unwrap();
        panic!("base written; rerun without NODE_L1_SPORK_FREE_WRITE");
    }
    let base: Value = serde_json::from_str(&std::fs::read_to_string(FINGERPRINT).unwrap()).unwrap();
    assert_eq!(base["schema"], now["schema"]);
    let mut differ = Vec::new();
    for (target, _) in TARGETS {
        let (b, n) = (&base["targets"][target]["entries"], &now["targets"][target]["entries"]);
        let (b, n) = (b.as_object().unwrap(), n.as_object().unwrap());
        for name in b.keys().chain(n.keys().filter(|k| !b.contains_key(*k))) {
            if b.get(name) != n.get(name) {
                differ.push(format!("{target} {name}"));
            }
        }
    }
    assert!(
        differ.is_empty(),
        "{} spork-free inputs differ from the base: {differ:?}",
        differ.len()
    );
}
