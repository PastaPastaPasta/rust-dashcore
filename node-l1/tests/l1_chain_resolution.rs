//! O2b-4 4d's fail-first golden: what the generator makes of the shared seed programs and 50
//! generated `chain-v2` inputs on S0 and S1, Core alone, as the runner resolves them (each op's
//! kind and bytes, its expectation, Core's reply class; the intents the tie model drops). S2's
//! line changes the world model's child index and the generator's extension, which must leave
//! S0 and S1 alone: the golden was written by this test at node/integration d72afde72 (4a's
//! merge), before either change, and is held equal since.
//!
//! `NODE_L1_RESOLUTION_GOLDEN_OUT=<path>` writes the streams' digests to `<path>` instead of
//! comparing them.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use bitcoin_hashes::sha256;
use dash_oracle_sys::Pin;
use node_l1::chain::census::Census;
use node_l1::chain::core::{CoreSut, Schedule, T_OLD, T_RECENT};
use node_l1::chain::generate::{Gen, Planned, due_enforce, expect};
use node_l1::chain::intent::{Encoding, Program};
use node_l1::chain::run::op_kind;

const GOLDEN: &str = include_str!("data/chain-resolution-s0-s1.json");

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `n` inputs from a fixed xorshift stream, 8 to 207 bytes each (`l1_chain_diff`'s).
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

/// The shared seed programs (`tests/chain_seeds/*.prog`) by file stem, then the generated inputs.
fn inputs() -> Vec<(String, Program)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let mut out: Vec<(String, Program)> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "prog"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, Program::parse(&std::fs::read_to_string(&p).unwrap()).unwrap())
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(out.len(), 15, "the shared seed programs");
    out.extend(generated(50).into_iter().map(|(n, b)| (n, Program::decode(&b, Encoding::V2))));
    out
}

/// Each input's resolved stream on `schedule`, Core alone, by `make`'s generators: `(ops, sha256 of
/// the stream)`, by name.
fn streams(schedule: Schedule, make: fn() -> Gen) -> BTreeMap<String, (usize, String)> {
    let (mut core, image) = CoreSut::start(Pin::V24_0, schedule).unwrap();
    let mut out = BTreeMap::new();
    for (name, program) in inputs() {
        let mut w = image.clone();
        w.mocktime = if program.old {
            T_OLD
        } else {
            T_RECENT
        };
        let restored = core.restore(w.mocktime).unwrap();
        assert!(!restored.tainted && !restored.step.post.down, "{name}: the restore");
        w.loaded();
        let mut generator = make();
        let mut census = Census::default();
        let (mut text, mut ops_run) = (String::new(), 0);
        'input: for intent in &program.intents {
            let mut ops: VecDeque<Planned> = generator.resolve(&mut w, intent, &mut census).into();
            while let Some(p) = ops.pop_front() {
                let expected = expect(&w, &p);
                let done = core.step(&p.op).unwrap();
                let class = done.step.class(&p.op);
                let op = sha256::Hash::hash(format!("{:?}", p.op).as_bytes()).to_byte_array();
                writeln!(text, "{} {} {} {class:?}", op_kind(&p.op), hex(&op), expected.name())
                    .unwrap();
                ops_run += 1;
                if done.tainted || !expected.met(&class) {
                    break 'input;
                }
                w.observe(&p.op, &done.step);
                match due_enforce(&w, &mut census) {
                    Some(Some(enforce)) => ops.push_front(enforce),
                    Some(None) => break 'input,
                    None => {}
                }
            }
        }
        writeln!(text, "avoided {:?}", census.avoided).unwrap();
        let digest = sha256::Hash::hash(text.as_bytes()).to_byte_array();
        out.insert(name, (ops_run, hex(&digest)));
    }
    out
}

/// S0's and S1's resolution of the shared seeds and the generated inputs is the golden's.
#[test]
fn s0_s1_resolution_is_unchanged() {
    held_to_the_golden(Gen::default);
}

/// 4f test 20: a legacy (`chain-v1`, `diff_chain`) generator resolves as before spork intents
/// were admitted for `chain-v2`: still the golden.
#[test]
fn legacy_v1_resolution_is_unchanged_by_admission() {
    assert!(!Gen::for_encoding(Encoding::V1).spork_admitted);
    held_to_the_golden(|| Gen::for_encoding(Encoding::V1));
}

fn held_to_the_golden(make: fn() -> Gen) {
    let mut now = serde_json::Map::new();
    for schedule in [Schedule::S0, Schedule::S1] {
        now.insert(
            schedule.name().to_owned(),
            serde_json::to_value(streams(schedule, make)).unwrap(),
        );
    }
    let now = serde_json::Value::Object(now);
    if let Some(path) = std::env::var_os("NODE_L1_RESOLUTION_GOLDEN_OUT") {
        std::fs::write(&path, serde_json::to_string_pretty(&now).unwrap() + "\n").unwrap();
        eprintln!("written to {}", Path::new(&path).display());
        return;
    }
    let golden: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    for schedule in ["S0", "S1"] {
        let (want, got) = (&golden[schedule], &now[schedule]);
        let want = want.as_object().expect("the golden's schedule");
        for (name, stream) in want {
            assert_eq!(&got[name], stream, "{schedule} {name}");
        }
        assert_eq!(got.as_object().unwrap().len(), want.len(), "{schedule}: the inputs");
    }
}
