//! T6 Q-4b-0: the 136 `qfcommit` cases of base-sdk's Core overlay corpus
//! (`tests/data/core_overlay-qfcommit-136.json5`, provenance in `tests/data/PROVENANCE.md`) through
//! the node's `qfcommit` read. Core refuses 72 of them, and the read refuses each; for the 64 Core
//! reads, the side-car's re-encode is Core's write-back.

use dash_consensus::primitives::P2pCommitment;
use dash_p2p::read::{self, Msg, PeerCtx};
use dash_p2p_core::{CommandString, P2pMsg, ProtocolVersion};
use dash_pkc::bls::BlsCtx;
use serde_json::Value;

const CORPUS: &str = include_str!("data/core_overlay-qfcommit-136.json5");

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex")).collect()
}

/// A case's payload: hex, or a list of hex and `{"b": byte, "n": count}` runs.
fn raw(v: &Value) -> Vec<u8> {
    match v {
        Value::String(s) => hex(s),
        Value::Array(parts) => parts
            .iter()
            .flat_map(|p| match p {
                Value::String(s) => hex(s),
                run => {
                    let byte =
                        u8::try_from(run["b"].as_u64().expect("a run byte")).expect("a byte");
                    vec![byte; usize::try_from(run["n"].as_u64().expect("a run count")).expect("n")]
                }
            })
            .collect(),
        other => panic!("a payload: {other}"),
    }
}

/// The corpus's cases: its header is `//` lines, its body JSON.
fn cases() -> Vec<Value> {
    let body: String =
        CORPUS.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let corpus: Value = serde_json::from_str(&body).expect("the corpus");
    corpus["cases"].as_array().expect("cases").clone()
}

#[test]
fn the_qfcommit_read_matches_cores_overlay_cases() {
    let cases = cases();
    let (mut refused, mut reencoded) = (0, 0);
    let mut wrong = Vec::new();
    for case in &cases {
        let name = case["name"].as_str().expect("a name");
        assert_eq!(case["command"], "qfcommit", "{name}");
        let version = i32::try_from(case["version"].as_i64().expect("a version")).expect("i32");
        let legacy = case["legacy"].as_bool().expect("legacy");
        let ctx = PeerCtx::new(ProtocolVersion::new(version), BlsCtx::from_legacy(legacy));
        let read = read::decode(&CommandString::QFCOMMIT, &raw(&case["raw"]), &ctx);
        let core = &case["core"];
        if core["ok"] == false {
            refused += 1;
            if let Ok(d) = read {
                wrong.push(format!("{name}: Core refuses ({}), read {:?}", core["class"], d.msg));
            }
            continue;
        }
        reencoded += 1;
        let qc = match read {
            Ok(read::Decoded {
                msg: Msg::Known(P2pMsg::QfCommit(qc)),
                ..
            }) => *qc,
            other => {
                wrong.push(format!("{name}: Core reads it, read {other:?}"));
                continue;
            }
        };
        let mut written = Vec::new();
        P2pCommitment::new(qc).write_core(&mut written);
        let reenc = hex(core["reenc"].as_str().expect("reenc"));
        if written != reenc {
            wrong.push(format!("{name}: write_core differs from Core's write-back"));
        }
    }
    assert!(wrong.is_empty(), "{} wrong cases:\n{}", wrong.len(), wrong.join("\n"));
    assert_eq!((cases.len(), refused, reencoded), (136, 72, 64));
}
