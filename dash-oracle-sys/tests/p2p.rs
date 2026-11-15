//! Smoke tests of `oracle_p2p_decode` (ABI 7): BS8's judge verdicts reproduced in process, the
//! O4 mnlistdiff and qrinfo fixtures round-tripped, and the entry point's refusals.

mod common;

use dash_oracle_sys::{ErrorKind, Oracle, P2P_COMMANDS, P2pStream, P2pVerdict, Pin};
use serde_json::Value;

use common::hex;

const CURRENT: i32 = 70242;

fn stream(version: i32, bls_legacy: bool, max: [u32; 2]) -> P2pStream {
    P2pStream {
        version,
        bls_legacy,
        qdata_max_vvec: max[0],
        qdata_max_contrib: max[1],
    }
}

fn v24() -> Oracle {
    Oracle::new(Pin::V24_0)
}

/// A payload in the corpus: hex, or over 4 KiB a list of hex and `{"b": byte, "n": count}` runs.
fn payload(raw: &Value) -> Vec<u8> {
    match raw {
        Value::String(s) => hex(s),
        Value::Array(parts) => parts
            .iter()
            .flat_map(|part| match part {
                Value::String(s) => hex(s),
                run => {
                    vec![hex(run["b"].as_str().unwrap())[0]; run["n"].as_u64().unwrap() as usize]
                }
            })
            .collect(),
        other => panic!("payload {other}"),
    }
}

/// tests/data/bs8-core-overlay.json5 (PROVENANCE.md): the oracle returns bs8-judge's read of
/// every case, so it can stand for the judge.
#[test]
fn reproduces_the_bs8_judge() {
    let path = format!("{}/tests/data/bs8-core-overlay.json5", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap();
    let json: String =
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    let corpus: Value = serde_json::from_str(&json).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 445);
    let oracle = v24();
    let mut commands = std::collections::BTreeSet::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let command = case["command"].as_str().unwrap();
        commands.insert(command);
        let raw = payload(&case["raw"]);
        let max = [0, 1].map(|i| case["max"][i].as_u64().unwrap() as u32);
        let s = stream(
            case["version"].as_i64().unwrap() as i32,
            case["legacy"].as_bool().unwrap(),
            max,
        );
        let judge = &case["core"];
        let ours = oracle.p2p_decode(command, &raw, &s);
        if judge["ok"] == false {
            match ours {
                Err(e) => assert_eq!(e.kind, ErrorKind::Decode, "{name}"),
                Ok(P2pVerdict::Refused(_)) => {}
                Ok(P2pVerdict::Read(_)) => panic!("{name}: the judge refused, the oracle read it"),
            }
            continue;
        }
        let Ok(P2pVerdict::Read(read)) = ours else {
            panic!("{name}: the judge read it, the oracle did not: {ours:?}");
        };
        let consumed = judge["consumed"].as_u64().unwrap() as usize;
        assert_eq!(read.consumed, consumed, "{name}");
        // The judge records the write-back only where it is not the bytes read.
        let reenc = judge["reenc"].as_str().map_or_else(|| raw[..consumed].to_vec(), hex);
        assert!(read.reencoded == reenc, "{name}: write-back");
        if command == "qdata" {
            let q = read.qdata.unwrap();
            assert_eq!(u64::from(q.error), judge["errorCode"].as_u64().unwrap(), "{name}");
            // The judge records the vector's size only where it is not the threshold.
            let other_size = q.vvec_size.filter(|&n| n != max[0] as usize);
            assert_eq!(other_size, judge["vvecSize"].as_u64().map(|n| n as usize), "{name}");
        } else {
            assert_eq!(read.qdata, None, "{name}");
        }
    }
    assert_eq!(commands.len(), P2P_COMMANDS.len(), "every command has cases");
}

/// The O4 fixtures (test-vectors/golden): Core round-trips each mnlistdiff and qrinfo exactly.
#[test]
fn round_trips_the_o4_fixtures() {
    let root = format!("{}/..", env!("CARGO_MANIFEST_DIR"));
    let oracle = v24();
    let current = stream(CURRENT, false, [0, 0]);
    let mut n = 0;
    for (file, command) in [("mnlistdiff", "mnlistdiff"), ("qrinfo", "qrinfo")] {
        let list = std::fs::read_to_string(format!(
            "{root}/test-vectors/golden/{file}-core-ground-truth.json"
        ))
        .unwrap();
        let records: Value = serde_json::from_str(&list).unwrap();
        for r in records.as_array().unwrap() {
            let raw = std::fs::read(format!("{root}/{}", r["repoPath"].as_str().unwrap())).unwrap();
            let Ok(P2pVerdict::Read(read)) = oracle.p2p_decode(command, &raw, &current) else {
                panic!("{}: not read", r["name"]);
            };
            assert_eq!(read.consumed, raw.len(), "{}", r["name"]);
            assert!(read.reencoded == raw, "{}: write-back", r["name"]);
            // One byte short: Core's read throws.
            let err = oracle.p2p_decode(command, &raw[..raw.len() - 1], &current);
            assert_eq!(err.unwrap_err().kind, ErrorKind::Decode, "{}", r["name"]);
            n += 1;
        }
    }
    assert_eq!(n, 37);
}

#[test]
fn refusals_and_arguments() {
    let oracle = v24();
    let s = stream(CURRENT, false, [0, 0]);
    // A request is 67 bytes; NetQuorum refuses a longer QGETDATA before reading it.
    let request = [0u8; 67];
    assert!(matches!(oracle.p2p_decode("qgetdata", &request, &s), Ok(P2pVerdict::Read(_))));
    let oversized = [0u8; 68];
    assert_eq!(
        oracle.p2p_decode("qgetdata", &oversized, &s).unwrap(),
        P2pVerdict::Refused("oversized qgetdata".into())
    );
    // Not a command of this entry point.
    assert_eq!(oracle.p2p_decode("tx", &[], &s).unwrap_err().kind, ErrorKind::Argument);
    assert_eq!(oracle.p2p_decode("clsig\0", &[], &s).unwrap_err().kind, ErrorKind::Argument);
    // Every command reads an empty payload as Core does: it throws.
    for command in P2P_COMMANDS {
        assert_eq!(
            oracle.p2p_decode(command, &[], &s).unwrap_err().kind,
            ErrorKind::Decode,
            "{command}"
        );
    }
}
