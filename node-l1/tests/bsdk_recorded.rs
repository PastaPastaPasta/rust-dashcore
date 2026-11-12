//! The BS4b rows replayed through base-sdk against Dash Core's recorded verdicts
//! (`data/bsdk_decode_recorded.json`), without the oracle. Each row's bytes are rebuilt from the
//! seeds and builders, their SHA-256 must be the one Core judged, and the judge must reach the
//! recorded agreement: the same comparison the fuzz targets make, with Core's side read from the
//! file. Regenerate with `NODE_L1_REGENERATE=1 cargo test -p node-l1 --features oracle --test
//! l1_bsdk_decode`.

#![deny(clippy::use_debug, clippy::dbg_macro, clippy::print_stdout)]

#[path = "support/bsdk_recorded.rs"]
mod bsdk_recorded;
#[path = "support/bsdk_rows.rs"]
mod bsdk_rows;
// Compiled here by path: the library is empty without the `oracle` feature.
#[allow(dead_code)]
#[path = "../src/bsdk_decode.rs"]
mod bsdk_decode;
#[path = "support/spec.rs"]
mod spec;

use std::collections::BTreeSet;

use bsdk_decode::{
    Agreement, Judged, judge_block, judge_header, judge_payload, judge_tx, split_payload_input,
};
use bsdk_recorded::{RecordedRow, input_digest, load};
use bsdk_rows::{
    BLOCK, BLS_FIELDS, Class, Expect, FieldKind, HEADER, PAYLOAD, Row, TX, floor_problems, rows,
};

fn agreement(judged: &[Judged]) -> Result<Agreement, &'static str> {
    let mut accepted = false;
    for j in judged {
        match j {
            Ok(Agreement::BothAccept) => accepted = true,
            Ok(Agreement::BothReject) => {}
            Err(field) => return Err(field),
        }
    }
    Ok(if accepted {
        Agreement::BothAccept
    } else {
        Agreement::BothReject
    })
}

/// The judge's result for a payload row under the basic, then the legacy BLS flag, against what
/// Core recorded.
fn payload_flags(row: &Row, recorded: &RecordedRow) -> [Judged; 2] {
    let (tx_type, payload) = split_payload_input(&row.input, bsdk_decode::PAYLOAD_TYPES)
        .expect("a payload row names its type");
    assert_eq!(recorded.core.len(), 2, "{}", row.name);
    let flag = |legacy: bool| {
        judge_payload(
            tx_type,
            legacy,
            payload,
            recorded.core[usize::from(legacy)].as_ref().map(|c| c.payload()).as_ref(),
        )
    };
    [flag(false), flag(true)]
}

#[test]
fn rows_replay_against_the_recorded_verdicts() {
    let recorded = load().rows;
    let rows = rows();
    let names = |names: Vec<&str>| names.join("\n");
    assert_eq!(
        names(recorded.iter().map(|r| r.name.as_str()).collect()),
        names(rows.iter().map(|r| r.name.as_str()).collect()),
        "the recorded rows are not rows(); regenerate with l1_bsdk_decode"
    );
    let mut seen = BTreeSet::new();
    let mut failures = Vec::new();
    for (row, recorded) in rows.iter().zip(&recorded) {
        assert!(seen.insert((row.target, row.name.as_str())), "{}: a duplicate row", row.name);
        assert_eq!(row.target, recorded.target, "{}", row.name);
        assert_eq!(recorded.tag, row.tag, "{}: the recorded tag differs", row.name);
        assert_eq!(
            input_digest(&row.input),
            recorded.input_sha256,
            "{}: the row's bytes are not the ones Core judged; regenerate with l1_bsdk_decode",
            row.name
        );
        let judged: Vec<Judged> = match row.target {
            PAYLOAD => payload_flags(row, recorded).to_vec(),
            target => {
                assert_eq!(recorded.core.len(), 1, "{}", row.name);
                let core = recorded.core[0].as_ref().map(|c| c.object());
                vec![match target {
                    TX => judge_tx(&row.input, core.as_ref()),
                    BLOCK => judge_block(&row.input, core.as_ref()),
                    HEADER => judge_header(&row.input, core.as_ref()),
                    other => panic!("{}: unknown target {other}", row.name),
                }]
            }
        };
        match agreement(&judged) {
            Err(field) => failures.push(format!("{}: {field}", row.name)),
            Ok(got) => {
                let want = match recorded.agreement.as_str() {
                    "accept" => Agreement::BothAccept,
                    "reject" => Agreement::BothReject,
                    other => panic!("{}: the recorded agreement {other:?} is neither", row.name),
                };
                if got != want {
                    failures.push(format!("{}: recorded {}", row.name, recorded.agreement));
                }
                if (row.expect == Expect::Accept && got != Agreement::BothAccept)
                    || (row.expect == Expect::Reject && got != Agreement::BothReject)
                {
                    failures.push(format!("{}: not the expected agreement", row.name));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

// ---------------------------------------------------------------------------------------------
// The DDE #10 floor.

/// Every payload row with its recorded comparison under both flags.
fn payload_judgements(rows: &[Row], recorded: &[RecordedRow]) -> Vec<(usize, [Judged; 2])> {
    rows.iter()
        .zip(recorded)
        .enumerate()
        .filter(|(_, (row, _))| row.target == PAYLOAD)
        .map(|(i, (row, recorded))| (i, payload_flags(row, recorded)))
        .collect()
}

fn problems_of(
    rows: &[Row],
    judged: &[(usize, [Judged; 2])],
    mut keep: impl FnMut(&Row) -> bool,
) -> Vec<String> {
    let kept: Vec<(&Row, [Judged; 2])> =
        judged.iter().filter(|(i, _)| keep(&rows[*i])).map(|(i, j)| (&rows[*i], *j)).collect();
    floor_problems(&kept)
}

#[test]
fn every_bls_cell_has_a_row_with_its_outcome() {
    let rows = rows();
    let recorded = load().rows;
    let judged = payload_judgements(&rows, &recorded);
    let problems = problems_of(&rows, &judged, |_| true);
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    // Every DDE #10 row stands for cells of the table, and only for those.
    for row in rows.iter().filter(|row| row.tag == "DDE#10") {
        assert!(!row.covers.is_empty(), "{}: covers no cell", row.name);
        for c in &row.covers {
            assert!(
                BLS_FIELDS.iter().any(|f| f.tx_type == c.tx_type
                    && f.field == c.field
                    && f.versions.contains(&c.version)),
                "{}: covers a cell outside the table",
                row.name
            );
        }
    }
    // 45 (type, version, field) triples of four classes, each compared under two flags.
    let cells: usize = BLS_FIELDS.iter().map(|f| f.versions.len() * Class::ALL.len()).sum();
    assert_eq!(cells, 180);
}

/// Negative control: with one `DDE#10` row per payload type (the deletion Sol's review made) the
/// floor reports the cells that are gone.
#[test]
fn floor_rejects_one_row_per_type() {
    let rows = rows();
    let recorded = load().rows;
    let judged = payload_judgements(&rows, &recorded);
    let mut types = BTreeSet::new();
    let problems = problems_of(&rows, &judged, |row| {
        row.covers.first().is_none_or(|c| types.insert(c.tx_type))
    });
    assert!(!problems.is_empty());
    // Each field kind and each class lost cells.
    for spec in BLS_FIELDS {
        assert!(
            problems.iter().any(|p| p.starts_with(&format!("type {} ", spec.tx_type))),
            "type {} lost no cell",
            spec.tx_type
        );
    }
}

/// Negative control: dropping any one class of a field, or any one version, is reported.
#[test]
fn floor_rejects_a_missing_class_or_version() {
    let rows = rows();
    let recorded = load().rows;
    let judged = payload_judgements(&rows, &recorded);
    for class in Class::ALL {
        let problems =
            problems_of(&rows, &judged, |row| !row.covers.iter().any(|c| c.class == class));
        assert!(!problems.is_empty(), "no problem without the {class:?} rows");
    }
    for spec in BLS_FIELDS {
        for &version in spec.versions {
            let problems = problems_of(&rows, &judged, |row| {
                !row.covers
                    .iter()
                    .any(|c| (c.tx_type, c.version, c.field) == (spec.tx_type, version, spec.field))
            });
            assert!(!problems.is_empty(), "type {} v{version} field {}", spec.tx_type, spec.field);
        }
    }
}

/// Negative control: a row whose comparison has the other outcome, or that diverged, is reported
/// for every field kind.
#[test]
fn floor_rejects_a_wrong_outcome() {
    let rows = rows();
    let recorded = load().rows;
    let judged = payload_judgements(&rows, &recorded);
    for kind in [FieldKind::Lazy, FieldKind::Global, FieldKind::Eager] {
        for class in [Class::Zero, Class::Neither] {
            for wrong in
                [Ok(Agreement::BothAccept), Ok(Agreement::BothReject), Err("payload fields")]
            {
                let mut flipped = judged.clone();
                let target = flipped
                    .iter_mut()
                    .find(|(i, _)| {
                        let covers = &rows[*i].covers;
                        covers.iter().any(|c| c.class == class)
                            && covers.iter().all(|c| {
                                BLS_FIELDS.iter().any(|f| {
                                    f.tx_type == c.tx_type && f.field == c.field && f.kind == kind
                                })
                            })
                            && !covers.is_empty()
                    })
                    .expect("a row of the kind and class");
                if target.1[0] == wrong {
                    continue;
                }
                target.1[0] = wrong;
                let problems = problems_of(&rows, &flipped, |_| true);
                assert!(!problems.is_empty(), "{kind:?} {class:?} {wrong:?} went unnoticed");
            }
        }
    }
}
