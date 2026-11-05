//! The reason constants against the census at the pin (EV1 §3.13), and the census closure of the
//! rows this slice ports.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::specialtx::{network, payload, trivial};

/// `tests/data/ev1_census.tsv`, written by `contrib/node/ev1-census.py`.
const CENSUS: &str = include_str!("../../../tests/data/ev1_census.tsv");

struct Row<'a> {
    file: &'a str,
    line: u32,
    scope: &'a str,
    class: &'a str,
    reason: &'a str,
    form: &'a str,
    note: &'a str,
}

fn census() -> Vec<Row<'static>> {
    CENSUS
        .lines()
        .filter(|l| !l.starts_with('#'))
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 6, "{l}");
            let (file, line) = f[0].rsplit_once(':').expect("file:line");
            Row {
                file,
                line: line.parse().expect("line"),
                scope: f[1],
                class: f[2],
                reason: f[3],
                form: f[4],
                note: f[5],
            }
        })
        .collect()
}

fn class(c: TxValidationResult) -> &'static str {
    match c {
        TxValidationResult::BadSpecial => "TX_BAD_SPECIAL",
        TxValidationResult::Consensus => "TX_CONSENSUS",
        other => panic!("not a class EV1 uses: {other:?}"),
    }
}

/// The `pub const NAME: &str = "reason";` lines of `reasons.rs`.
fn constants() -> Vec<(&'static str, &'static str)> {
    include_str!("../reasons.rs")
        .lines()
        .filter_map(|l| l.strip_prefix("pub const ")?.split_once(": &str = \""))
        .map(|(name, rest)| (name, rest.strip_suffix("\";").expect("one-line constant")))
        .collect()
}

#[test]
fn census_matches_pin() {
    let rows = census();
    let ev1: Vec<&Row> = rows.iter().filter(|r| r.scope == "ev1").collect();

    // Every constant is one distinct EV1 census reason, named after it, and every EV1 census
    // reason (literal or forwarded) has its constant.
    let consts = constants();
    let strings: BTreeSet<&str> = consts.iter().map(|c| c.1).collect();
    assert_eq!(strings.len(), consts.len(), "one constant per reason");
    for (name, reason) in &consts {
        assert_eq!(*name, reason.to_uppercase().replace('-', "_"), "{reason}");
    }
    let census_reasons: BTreeSet<&str> = ev1.iter().map(|r| r.reason).collect();
    assert_eq!(strings, census_reasons);

    // CLASSES is exactly the census's (reason, class) pairs, once each.
    let pairs: BTreeSet<(&str, &str)> = ev1.iter().map(|r| (r.reason, r.class)).collect();
    let ours: Vec<(&str, &str)> = CLASSES.iter().map(|(r, c)| (*r, class(*c))).collect();
    assert_eq!(ours.iter().copied().collect::<BTreeSet<_>>(), pairs);
    assert_eq!(ours.len(), pairs.len(), "each pair once");

    // The totals at the pin: the design's (EV1 §3.13, rc.3) less rc.4's two entry loops (CE4-03).
    let literal: Vec<&&Row> = ev1.iter().filter(|r| r.form == "literal").collect();
    let by_class = |c| literal.iter().filter(|r| r.class == c).count();
    assert_eq!(literal.len(), 211);
    assert_eq!((by_class("TX_BAD_SPECIAL"), by_class("TX_CONSENSUS")), (142, 69));
    assert_eq!(literal.iter().map(|r| r.reason).collect::<BTreeSet<_>>().len(), 146);
    // The two forwarding sites forward only the migration reason.
    let forwards: Vec<(u32, &str)> =
        ev1.iter().filter(|r| r.form == "forward").map(|r| (r.line, r.reason)).collect();
    assert_eq!(forwards, [(1427, BAD_PROTX_NETINFO_VERSION), (1536, BAD_PROTX_NETINFO_VERSION)]);
}

/// Census closure for the rows EV1-1 and EV1-2 port: each has a vector with its exact class and
/// reason (`network::tests`, `trivial::tests`, `payload::tests`), or an `unreachable` mark. Later
/// slices extend [`CLOSED`] with the ranges they port.
const CLOSED: &[(&str, u32, u32)] = &[
    // EV1-1: the provider network fields; EV1-2: the rest of the file.
    ("src/evo/providertx.cpp", 1, 586),
    // EV1-2: `GetValidatedPayload`.
    ("src/evo/specialtxman.cpp", 1157, 1183),
];

#[test]
fn closed_rows_are_reached_or_unreachable() {
    let providertx = network::tests::REACHED.iter().chain(trivial::tests::REACHED);
    let reached: BTreeMap<(&str, u32), (&str, &str)> = providertx
        .map(|r| ("src/evo/providertx.cpp", r))
        .chain(payload::tests::REACHED.iter().map(|r| ("src/evo/specialtxman.cpp", r)))
        .map(|(f, (l, c, r))| ((f, *l), (class(*c), *r)))
        .collect();
    let (mut closed, mut unreachable) = (0, 0);
    for row in census().iter().filter(|r| r.scope == "ev1") {
        if !CLOSED.iter().any(|(f, a, b)| *f == row.file && (*a..=*b).contains(&row.line)) {
            continue;
        }
        closed += 1;
        let at = (row.file, row.line);
        if row.note.starts_with("unreachable: ") {
            unreachable += 1;
            assert!(!reached.contains_key(&at), "{}:{} is marked unreachable", row.file, row.line);
            continue;
        }
        assert_eq!(
            reached.get(&at),
            Some(&(row.class, row.reason)),
            "{}:{} {} {} has no vector",
            row.file,
            row.line,
            row.class,
            row.reason
        );
    }
    assert_eq!(closed, reached.len() + unreachable, "every REACHED line is a census row");
}

/// The initial `unreachable` rows of the design (§3.13), the one EV1-1 adds and the three EV1-2
/// adds.
#[test]
fn unreachable_rows() {
    let marked: Vec<(u32, &str)> = census()
        .iter()
        .filter(|r| r.note.starts_with("unreachable: "))
        .map(|r| (r.line, r.reason))
        .collect();
    assert_eq!(
        marked,
        [
            (49, BAD_PROTX_PAYEE_DEST),
            (105, BAD_PROTX_SHARES_PAYEE_DEST),
            (191, BAD_PROTX_NETINFO_ADDR),
            (304, BAD_PROTX_OPERATOR_PUBKEY),
            (462, BAD_PROTX_OPERATOR_PUBKEY),
            (573, BAD_PROTX_OPERATOR_PUBKEY)
        ]
    );
}
