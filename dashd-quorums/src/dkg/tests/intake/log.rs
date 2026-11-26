//! `dkg::log` and `round::log_events` (`batchedlogger.{h,cpp}`, v24.0.0): a record's lines are one write, each indented;
//! a line of its own is `<func> -- <text>`.

use dash_llmq::dkg::session::DkgEvent;

use super::super::fakes::{dkg_lines, logged};
use crate::dkg::log::{log_record, uint256_hex};
use crate::dkg::round::log_events;

fn event(record: Option<u32>, text: &str) -> DkgEvent {
    DkgEvent {
        record,
        func: "ReceiveMessage",
        text: text.into(),
    }
}

#[test]
fn records_are_one_write_and_other_lines_stand_alone() {
    let events = [
        event(Some(0), "a"),
        event(Some(0), "b"),
        event(None, "c"),
        event(None, "d"),
        event(Some(1), "e"),
        event(Some(2), "f"),
    ];
    let ((), lines) = logged(|| log_events(&events));
    assert_eq!(
        dkg_lines(&lines),
        ["    a\n    b", "ReceiveMessage -- c", "ReceiveMessage -- d", "    e", "    f"]
    );
    let ((), lines) = logged(|| log_record(Vec::<String>::new()));
    assert!(lines.is_empty(), "an empty record writes nothing");
}

// Core: v24.0.0 src/llmq/dkgsessionmgr.cpp:166 GetVerifiedContributions
#[test]
fn a_verified_contributions_line_is_logged_under_llmq() {
    let events = [
        DkgEvent {
            record: None,
            func: "GetVerifiedContributions",
            text: "c".into(),
        },
        event(None, "d"),
    ];
    let ((), lines) = logged(|| log_events(&events));
    let got: Vec<_> = lines.iter().map(|l| (l.target.as_str(), l.text.as_str())).collect();
    assert_eq!(
        got,
        [("llmq", "GetVerifiedContributions -- c"), ("llmq-dkg", "ReceiveMessage -- d")]
    );
}

#[test]
fn a_uint256_prints_reversed() {
    let mut bytes = [0u8; 32];
    bytes[0] = 0xab;
    bytes[31] = 0x01;
    let hex = uint256_hex(&bytes);
    assert!(hex.starts_with("01") && hex.ends_with("ab"), "{hex}");
}
