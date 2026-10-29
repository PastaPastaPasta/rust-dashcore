//! The overlay targets' recorded inputs: BS8's Core-judged corpus (dash-oracle-sys
//! `tests/data/bs8-core-overlay.json5`) and the O4 mnlistdiff and qrinfo fixtures
//! (`test-vectors/golden`), as [`Case`]s.

#![allow(dead_code)]

use dash_oracle_sys::P2P_COMMANDS;
use node_l1::overlay::{Case, MIN_VERSION, VERSIONS};
use serde_json::Value;

/// The largest input that is mutated or written as a fuzz seed (the O4 qrinfo fixtures are
/// 0.5 MB).
pub const MAX_SEED: usize = 64 * 1024;

/// A recorded input: its name, the case, and whether the corpus marks it `"strict": "flag"`.
pub struct Recorded {
    pub name: String,
    pub case: Case,
    pub strict: bool,
}

fn root() -> String {
    format!("{}/..", env!("CARGO_MANIFEST_DIR"))
}

fn hex(s: &str) -> Vec<u8> {
    assert_eq!(s.len() % 2, 0, "odd hex length");
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

fn command(name: &str) -> &'static str {
    P2P_COMMANDS.into_iter().find(|c| *c == name).expect("an overlay command")
}

/// A corpus payload: hex, or over 4 KiB a list of hex and `{"b": byte, "n": count}` runs.
fn payload(raw: &Value) -> Vec<u8> {
    match raw {
        Value::String(s) => hex(s),
        Value::Array(parts) => parts
            .iter()
            .flat_map(|part| match part {
                Value::String(s) => hex(s),
                run => vec![
                    hex(run["b"].as_str().expect("run byte"))[0];
                    run["n"].as_u64().expect("run count") as usize
                ],
            })
            .collect(),
        other => panic!("payload {other}"),
    }
}

/// BS8's corpus (445 cases, 12 marked strict).
pub fn bs8_corpus() -> Vec<Recorded> {
    let path = format!("{}/dash-oracle-sys/tests/data/bs8-core-overlay.json5", root());
    let text = std::fs::read_to_string(&path).expect(&path);
    let json: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let corpus: Value = serde_json::from_str(&json).expect(&path);
    corpus["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .map(|c| Recorded {
            name: c["name"].as_str().expect("name").to_owned(),
            case: Case {
                cmd: command(c["command"].as_str().expect("command")),
                version: c["version"].as_i64().expect("version") as i32,
                legacy: c["legacy"].as_bool().expect("legacy"),
                max: [0, 1].map(|i| c["max"][i].as_u64().expect("max") as u16),
                raw: payload(&c["raw"]),
            },
            strict: c.get("strict").is_some(),
        })
        .collect()
}

/// The O4 fixtures (37), each on `PROTOCOL_VERSION` with the basic scheme.
pub fn o4_fixtures() -> Vec<Recorded> {
    let mut out = Vec::new();
    for cmd in ["mnlistdiff", "qrinfo"] {
        let path = format!("{}/test-vectors/golden/{cmd}-core-ground-truth.json", root());
        let records: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect(&path)).expect(&path);
        for r in records.as_array().expect("records") {
            let file = format!("{}/{}", root(), r["repoPath"].as_str().expect("repoPath"));
            out.push(Recorded {
                name: r["name"].as_str().expect("name").to_owned(),
                case: Case {
                    cmd: command(cmd),
                    version: MIN_VERSION + VERSIONS - 1,
                    legacy: false,
                    max: [0, 0],
                    raw: std::fs::read(&file).expect(&file),
                },
                strict: false,
            });
        }
    }
    out
}
