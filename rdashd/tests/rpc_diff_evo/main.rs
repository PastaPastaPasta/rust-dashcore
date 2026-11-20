#![cfg(unix)]

#[path = "../common/mod.rs"]
mod common;
mod diff;
mod golden;
mod protx;
mod quorum;
mod sample;

use common::pair::Pair;

#[test]
fn family_usage_matches_dashd_v24() {
    let _dashd = common::require_dashd();
    let mut pair = Pair::start(&["-txindex=0"]);
    pair.both("protx_diff_bool_type", "protx diff", "[true,1]");
    pair.both("protx_diff_block_bool_type", "protx diff", "[1,true]");
    pair.both("protx_diff_block_null", "protx diff", "[1,null]");
    for family in ["protx", "quorum"] {
        pair.both(&format!("help_{family}"), "help", &format!(r#"["{family}"]"#));
        for (name, params) in [
            ("bare", "[]"),
            ("unknown", r#"["unknown"]"#),
            ("unknown_extra", r#"["unknown",1]"#),
            ("type", "[1]"),
            ("null", "[null]"),
            ("named", r#"{"command":"unknown"}"#),
        ] {
            pair.both(&format!("{family}_{name}"), family, params);
        }
    }
    assert_eq!(pair.dashd_replies().len(), pair.count);
    assert_eq!(pair.dashd_replies()["protx_unknown"], pair.dashd_replies()["quorum_unknown"]);
    pair.finish();
}
