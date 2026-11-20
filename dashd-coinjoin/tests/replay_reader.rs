mod common;

use std::collections::BTreeSet;
use std::path::Path;

use common::transcript::{Direction, Record, parse};

const FIXTURES: &[&str] = &[
    "dsq-blocksonly",
    "dsq-modes",
    "dsq-server-intake",
    "dsq-wallet-intake",
    "dsq-wallet-threshold",
    "dstx-intake",
    "dstx-lifecycle-reorg",
    "dstx-removed-mn",
    "server-empty-entry",
    "server-mn-list",
    "server-recent",
    "server-session",
    "server-timeout-charge",
    "server-timeout-no-charge",
];

#[test]
fn every_dashd_v24_fixture_parses_without_unknown_records() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dashd-v24");
    let mut kinds = BTreeSet::new();
    for name in FIXTURES {
        let path = root.join(format!("{name}.txt"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let fixture = parse(*name, &text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        assert_eq!(fixture.format, "cj1-v1");
        assert!(fixture.mocktime > 0);
        assert!(!fixture.mask.is_null());
        for record in fixture.records {
            let kind = match record {
                Record::Accept {
                    ..
                } => "accept",
                Record::Message(message) => match message.direction {
                    Direction::ToNode => ">",
                    Direction::FromNode => "<",
                },
                Record::Expect(_) => "expect",
                Record::Log(_) => "log",
                Record::Observe(_) => "observe",
                Record::Rpc(_) => "rpc",
                Record::Time(_) => "time",
            };
            kinds.insert(kind);
        }
    }
    assert_eq!(
        kinds,
        ["<", ">", "accept", "expect", "log", "observe", "rpc", "time"].into_iter().collect()
    );
}

#[test]
fn unknown_record_kind_is_rejected() {
    let error = parse("unknown", "format cj1-v1\nmocktime 1\nibd 0\nmask {}\nfuture value\n")
        .expect_err("unknown records must not be silently ignored");
    assert!(error.message.contains("unknown record kind"));
}

#[test]
fn duplicate_header_records_are_rejected() {
    for (header, duplicate) in
        [("mocktime", "mocktime 2"), ("ibd", "ibd 1"), ("mask", "mask {\"x\":1}")]
    {
        let text = format!("format cj1-v1\nmocktime 1\nibd 0\nmask {{}}\n{duplicate}\n");
        let error = parse("duplicate", &text)
            .expect_err("a second header record must not silently replace the first");
        assert!(error.message.contains(&format!("duplicate {header}")), "{header}: {error}");
    }
}
