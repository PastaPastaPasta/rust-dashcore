//! The policy table's throw and read columns, row by row (SB6 r5 m12): every handler catch with
//! Core's score and reason (v24.0.0-rc.3), and exactly Core's 15 commands that read nothing.

use dash_p2p::policy::{self, ReadPlan, ThrowOutcome};

/// `Misbehaving(100, ...)` in the handler's catch: filterload, filteradd, spork and getqrinfo
/// (`net_processing.cpp:5540`, `:5573`, `:5695`, `:5667`), govobjvote
/// (`governance/net_governance.cpp:210`) and the DKG framing walk (`llmq/net_dkg.cpp:601`).
const fn handler(msg: &'static str) -> ThrowOutcome {
    ThrowOutcome::Handler {
        score: 100,
        msg,
    }
}

/// The signing shares' `BanNode` (+100, no reason) and rethrow (`llmq/net_signing.cpp:66-146`,
/// `:307-311`).
const SIGSHARE: ThrowOutcome = ThrowOutcome::HandlerThenCaught {
    score: 100,
    msg: "",
};

/// Every row whose handler catches the read's throw, in table order, with its outcome.
const HANDLER_ROWS: [(&str, ThrowOutcome); 15] = [
    ("filterload", handler("misformatted bloom filter")),
    ("filteradd", handler("bad filteradd message")),
    ("spork", handler("malformed spork received")),
    // governance/net_governance.cpp:93-100: the hash is read outside the catch.
    (
        "govsync",
        ThrowOutcome::HandlerAfterHead {
            head_bytes: 32,
            score: 100,
            msg: "misformatted govsync bloom filter",
        },
    ),
    ("govobjvote", handler("malformed governance vote")),
    ("qcontrib", handler("malformed DKG message")),
    ("qcomplaint", handler("malformed DKG message")),
    ("qjustify", handler("malformed DKG message")),
    ("qpcommit", handler("malformed DKG message")),
    ("qsigsesann", SIGSHARE),
    ("qsigsinv", SIGSHARE),
    ("qgetsigs", SIGSHARE),
    ("qbsigs", SIGSHARE),
    ("qsigshare", SIGSHARE),
    ("getqrinfo", handler("malformed getqrinfo received")),
];

/// The commands Core's branch reads nothing of, in table order: no branch of their own
/// (`merkleblock`, `cfilter`, `cfheaders`, `cfcheckpt`, the extension dispatch at
/// `net_processing.cpp:5769-5825`), or a branch that reads no payload.
const NOTHING_ROWS: [&str; 15] = [
    "verack",
    "sendaddrv2",
    "merkleblock",
    "getaddr",
    "mempool",
    "filterclear",
    "sendheaders",
    "cfilter",
    "cfheaders",
    "cfcheckpt",
    "getsporks",
    "mnlistdiff",
    "qwatch",
    "sendheaders2",
    "qrinfo",
];

#[test]
fn every_handler_catch_is_cores_score_and_reason() {
    let got: Vec<(&str, ThrowOutcome)> = policy::table()
        .iter()
        .filter(|r| {
            matches!(
                r.on_throw,
                ThrowOutcome::Handler { .. }
                    | ThrowOutcome::HandlerAfterHead { .. }
                    | ThrowOutcome::HandlerThenCaught { .. }
            )
        })
        .map(|r| (r.cmd.as_str(), r.on_throw))
        .collect();
    assert_eq!(got, HANDLER_ROWS);
}

/// Every other row: nothing read never throws, `pong` reads only when 8 bytes remain, `version`'s
/// tail is its own outcome, and the rest is caught by `ProcessMessages`.
#[test]
fn every_other_row_throws_to_process_messages() {
    for r in policy::table() {
        let name = r.cmd.as_str();
        if HANDLER_ROWS.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let want = match name {
            "version" => ThrowOutcome::VersionTail,
            "pong" => ThrowOutcome::NeverThrows,
            _ if NOTHING_ROWS.contains(&name) => ThrowOutcome::NeverThrows,
            _ => ThrowOutcome::CaughtByProcessMessages,
        };
        assert_eq!(r.on_throw, want, "{name}");
    }
}

/// Exactly 15 of Core's 74 commands read nothing; the other 59 read their payload.
#[test]
fn exactly_fifteen_rows_read_nothing() {
    let nothing: Vec<&str> = policy::table()
        .iter()
        .filter(|r| r.read == ReadPlan::Nothing)
        .map(|r| r.cmd.as_str())
        .collect();
    assert_eq!(nothing, NOTHING_ROWS);
    assert_eq!(policy::table().len() - nothing.len(), 59);
}
