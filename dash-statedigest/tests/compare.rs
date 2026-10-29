//! The comparator's rules (spec §10) on records built through the crate's API.
//!
//! These records are logic inputs, not Core fixtures: no value in them comes from Dash Core, and
//! no test here claims anything about Core's output. The tests over Core's own recordings
//! (projection = golden, Core-vs-Core, planted mutations) are in dash-conformance, which can
//! read the zstd vectors.

use std::convert::Infallible;

use dash_statedigest::compare::{
    AttemptDiff, Comparator, FieldId, Mode, Options, ProjectError, Projector, Side, Verdict, What,
    compare_attempts, compare_linear, compare_stream,
};
use dash_statedigest::{
    Attempt, Chain, Class, Effect, FileKind, Hash256, Header, Ident, Kind, Origin, Outcome, Query,
    Record, Value, Writer, group,
};

fn hx(n: u64) -> Hash256 {
    let mut bytes = [0u8; 32];
    bytes[24..].copy_from_slice(&n.to_be_bytes());
    Hash256::from_display_bytes(bytes)
}

/// The block at height `h` on branch `fork` (0 is the main branch).
fn block(h: i64, fork: u64) -> Hash256 {
    hx(1_000 * fork + h as u64 + 1)
}

fn id(h: i64, b: Hash256, org: Origin) -> Ident {
    Ident {
        h,
        b,
        org,
    }
}

fn header(run: &str) -> Record {
    Record::Header(Header {
        file: FileKind::Digest,
        src: Writer::Dashd,
        ver: b"test".to_vec(),
        chain: Chain::Regtest,
        run: run.as_bytes().to_vec(),
    })
}

/// A `C` with every `s` key set, `fx` empty and `seq` 1.
fn connect(h: i64, b: Hash256, prev: Option<Hash256>, org: Origin) -> Outcome {
    let mut c = Outcome::connect(id(h, b, org));
    let mut s = c.s_mut();
    let hash = |n| Value::hash(&hx(n));
    for (key, value) in [
        ("prev", prev.map_or(Value::Null, |p| Value::hash(&p))),
        ("ver", Value::Int(0x2000_0000)),
        ("mrkl", hash(7)),
        ("time", Value::Int(1_700_000_000 + h)),
        ("bits", Value::Int(0x207f_ffff)),
        ("nonce", Value::Int(h)),
        ("work", hash(h as u64 * 2 + 2)),
        ("ntx", Value::Int(1)),
        ("undo", hash(9)),
        ("mnl", Value::Null),
        ("qc", Value::Null),
        ("cp", Value::Null),
        ("ehf", Value::Null),
        ("bls", Value::Bool(true)),
        ("bur", Value::Int(3)),
        (
            "dep",
            Value::Arr(vec![Value::Arr(vec![
                Value::string("testdummy"),
                Value::string("defined"),
                Value::Int(0),
            ])]),
        ),
        ("muhash", Value::string("ab")),
        ("txouts", Value::Int(h)),
        ("amount", Value::Int(50 * h)),
        ("unspendable", Value::Int(0)),
        ("bi", Value::Arr(vec![Value::Int(0); 7])),
        ("stats", Value::Null),
    ] {
        s.set(key, value).unwrap();
    }
    c.o_mut().set_effects(&[]).unwrap();
    c.x_mut().set("seq", Value::Int(1)).unwrap();
    c
}

/// The tip connect at `h` on branch `fork`, which forks off the main branch at height 10.
fn tip(h: i64, fork: u64) -> Outcome {
    let on = |h: i64| {
        block(
            h,
            if h >= 10 {
                fork
            } else {
                0
            },
        )
    };
    connect(h, on(h), (h > 0).then(|| on(h - 1)), Origin::Tip)
}

fn disconnect(h: i64, b: Hash256, org: Origin, res: &str) -> Outcome {
    let mut d = Outcome::disconnect(id(h, b, org));
    d.s_mut().set("res", Value::string(res)).unwrap();
    d.o_mut().set_effects(&[]).unwrap();
    d.x_mut().set("seq", Value::Int(1)).unwrap();
    d
}

fn reject(h: i64, b: Hash256, org: Origin, why: &str) -> Outcome {
    let mut r = Outcome::reject(id(h, b, org));
    let mut s = r.s_mut();
    s.set("res", Value::string("invalid")).unwrap();
    s.set("cls", Value::string("consensus")).unwrap();
    s.set("why", Value::string(why)).unwrap();
    r.o_mut().set_effects(&[]).unwrap();
    r.x_mut().set("seq", Value::Int(1)).unwrap();
    r
}

/// The main chain 0..=n as tip connects.
fn chain(n: i64) -> Vec<Outcome> {
    (0..=n).map(|h| tip(h, 0)).collect()
}

fn items(records: &[Outcome]) -> Vec<Result<(usize, Outcome), Infallible>> {
    records.iter().cloned().enumerate().map(|(i, r)| Ok((i + 1, r))).collect()
}

fn stream_items(records: &[Record]) -> Vec<Result<(usize, Record), Infallible>> {
    records.iter().cloned().enumerate().map(|(i, r)| Ok((i + 1, r))).collect()
}

fn linear(left: &[Outcome], right: &[Outcome], opts: Options) -> dash_statedigest::compare::Report {
    compare_linear(items(left), items(right), opts).unwrap()
}

fn stream(left: &[Record], right: &[Record], opts: Options) -> dash_statedigest::compare::Report {
    compare_stream(stream_items(left), stream_items(right), opts).unwrap()
}

fn as_records(outcomes: &[Outcome]) -> Vec<Record> {
    std::iter::once(header("1-1")).chain(outcomes.iter().cloned().map(Record::Outcome)).collect()
}

fn field(name: &str) -> FieldId {
    let ids = FieldId::resolve(name);
    assert_eq!(ids.len(), 1, "{name}");
    ids[0]
}

fn keys(report: &dash_statedigest::compare::Report) -> Vec<String> {
    match &report.first().expect("a mismatch").what {
        What::Keys(diffs) => diffs.iter().map(|d| d.field.to_string()).collect(),
        other => panic!("not a key mismatch: {other:?}"),
    }
}

#[test]
fn identical_chains_are_clean_in_both_modes() {
    let a = chain(20);
    let report = linear(&a, &a, Options::new(Mode::Linear));
    assert_eq!(report.verdict, Verdict::Clean);
    assert_eq!(report.pairs, 21);
    assert_eq!(report.range, Some((0, 20)));
    assert!(report.uncovered.is_empty());
    // Every C.s key is covered in linear mode, and nothing else.
    let covered: Vec<FieldId> = report.coverage.keys().copied().collect();
    let all: Vec<FieldId> = group(Kind::Connect, Class::Semantic)
        .iter()
        .map(|f| FieldId::find(f.kind, f.class, f.key).unwrap())
        .collect();
    assert_eq!(covered, all);
    let report = stream(&as_records(&a), &as_records(&a), Options::new(Mode::Stream));
    assert_eq!(report.verdict, Verdict::Clean);
    assert!(report.coverage.contains_key(&field("C.o.fx")));
    assert_eq!(Verdict::Clean.exit_code(), 0);
}

/// Every `s` key of `C`, changed at height 7, is reported at 7 with that key, in both modes.
#[test]
fn a_semantic_key_is_reported_in_both_modes() {
    let a = chain(12);
    for spec in group(Kind::Connect, Class::Semantic) {
        let mut b = a.clone();
        let changed = match b[7].s().get(spec.key).unwrap() {
            Value::Int(n) => Value::Int(n + 1),
            Value::Null => Value::Null,
            _ => Value::hash(&hx(999)),
        };
        // `null` keys have a non-null value of their own shape.
        let changed = match (spec.key, changed) {
            ("mnl", _) => Value::hash(&hx(5)),
            ("qc", _) => Value::Arr(vec![]),
            ("cp", _) => {
                Value::Arr(vec![Value::Int(1), Value::Int(2), Value::Int(3), Value::hash(&hx(4))])
            }
            ("ehf", _) => Value::Arr(vec![]),
            ("stats", _) => Value::Arr(vec![Value::Int(1); 7]),
            ("bls", _) => Value::Bool(false),
            ("dep", _) => Value::Arr(vec![]),
            ("muhash", _) => Value::string("cd"),
            ("bi", _) => Value::Arr(vec![Value::Int(1); 7]),
            (_, v) => v,
        };
        b[7].s_mut().set(spec.key, changed).unwrap();
        let name = format!("C.s.{}", spec.key);
        for report in [
            linear(&a, &b, Options::new(Mode::Linear)),
            stream(&as_records(&a), &as_records(&b), Options::new(Mode::Stream)),
        ] {
            assert_eq!(report.verdict, Verdict::Diverged, "{name}");
            let first = report.first().unwrap();
            assert_eq!((first.h, *first.b()), (7, block(7, 0)), "{name}");
            assert_eq!(keys(&report), std::slice::from_ref(&name));
            assert!(report.stopped);
            assert_eq!(report.mismatches.len(), 1);
            assert_eq!(first.before[0].as_ref().unwrap().id.h, 6);
        }
    }
}

#[test]
fn an_observation_key_is_compared_in_stream_mode_only() {
    let a = chain(10);
    let mut b = a.clone();
    b[4].o_mut()
        .set_effects(&[Effect::MnChanged {
            undo: false,
        }])
        .unwrap();
    assert_eq!(linear(&a, &b, Options::new(Mode::Linear)).verdict, Verdict::Clean);
    let report = stream(&as_records(&a), &as_records(&b), Options::new(Mode::Stream));
    assert_eq!(report.verdict, Verdict::Diverged);
    assert_eq!(report.first().unwrap().h, 4);
    assert_eq!(keys(&report), ["C.o.fx"]);
}

#[test]
fn a_schedule_key_is_never_compared() {
    let a = chain(10);
    let mut b = a.clone();
    b[5].x_mut().set("seq", Value::Int(77)).unwrap();
    assert_eq!(linear(&a, &b, Options::new(Mode::Linear)).verdict, Verdict::Clean);
    let report = stream(&as_records(&a), &as_records(&b), Options::new(Mode::Stream));
    assert_eq!(report.verdict, Verdict::Clean);
    assert!(
        !report
            .coverage
            .contains_key(&FieldId::find(Kind::Connect, Class::Schedule, "seq").unwrap())
    );
    assert!(FieldId::resolve("seq").is_empty());
}

#[test]
fn an_absent_key_is_uncovered_never_passed() {
    let a = chain(10);
    let mut b = a.clone();
    for r in &mut b {
        r.s_mut().remove("muhash");
    }
    b[3].s_mut().remove("undo");
    let report = linear(&a, &b, Options::new(Mode::Linear));
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.verdict.exit_code(), 3);
    assert!(report.mismatches.is_empty());
    let uncovered: Vec<String> = report.uncovered.iter().map(ToString::to_string).collect();
    assert_eq!(uncovered, ["C.s.undo", "C.s.muhash"]);
    let undo = report.coverage[&field("undo")];
    assert_eq!((undo.equal, undo.right_absent), (10, 1));
    let undo = linear(&b, &a, Options::new(Mode::Linear)).coverage[&field("undo")];
    assert_eq!((undo.equal, undo.left_absent), (10, 1));

    // Requiring other keys does not pass the uncovered ones (§10.3): still incomplete.
    let mut opts = Options::new(Mode::Linear);
    opts.require = vec![field("prev"), field("time")];
    let report = linear(&a, &b, opts);
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.uncovered.len(), 2, "still listed");

    // Requiring an absent key makes its absence a mismatch, at the first record without it.
    let mut opts = Options::new(Mode::Linear);
    opts.require = vec![field("undo")];
    let report = linear(&a, &b, opts);
    assert_eq!(report.verdict, Verdict::Diverged);
    let first = report.first().unwrap();
    assert_eq!(first.h, 3);
    let What::Keys(diffs) = &first.what else {
        panic!()
    };
    assert_eq!(diffs[0].field, field("undo"));
    assert!(diffs[0].left.is_some() && diffs[0].right.is_none());

    // Absent on both sides is uncovered too.
    let report = linear(&b, &b, Options::new(Mode::Linear));
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.coverage[&field("muhash")].both_absent, 11);
}

#[test]
fn a_required_key_that_is_never_compared_is_incomplete() {
    let a = as_records(&chain(3));
    let mut opts = Options::new(Mode::Stream);
    opts.require = FieldId::resolve("why");
    let report = stream(&a, &a, opts);
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.uncovered, FieldId::resolve("why"));
}

#[test]
fn a_different_block_is_an_identity_mismatch_and_stops() {
    let a = chain(15);
    let b: Vec<Outcome> = (0..=15).map(|h| tip(h, 1)).collect();
    let mut opts = Options::new(Mode::Linear);
    opts.keep_going = true;
    let report = linear(&a, &b, opts);
    assert_eq!(report.verdict, Verdict::Diverged);
    let first = report.first().unwrap();
    assert_eq!(first.h, 10);
    assert_eq!(first.what, What::Identity);
    assert_eq!(first.right.as_ref().unwrap().id.b, block(10, 1));
    assert_eq!(report.mismatches.len(), 1, "even with keep_going");
}

#[test]
fn heights_on_one_side_only_are_gaps() {
    let a = chain(10);
    let b = chain(8);
    let report = linear(&a, &b, Options::new(Mode::Linear));
    assert_eq!(report.verdict, Verdict::Diverged);
    let first = report.first().unwrap();
    assert_eq!((first.h, &first.what), (9, &What::Gap(Side::Right)));
    assert_eq!(report.mismatches.len(), 1);

    let mut opts = Options::new(Mode::Linear);
    opts.keep_going = true;
    assert_eq!(linear(&a, &b, opts).mismatches.len(), 2);

    // The left starts later (a digest that begins after genesis): gaps at 0..=4 on the left.
    let late = &a[5..];
    let report = linear(late, &a, Options::new(Mode::Linear));
    assert_eq!(report.first().unwrap().what, What::Gap(Side::Left));
    assert_eq!(report.first().unwrap().h, 0);
    // With --from 5 the common range is clean.
    let mut opts = Options::new(Mode::Linear);
    opts.from = Some(5);
    let report = linear(late, &a, opts);
    assert_eq!(report.verdict, Verdict::Clean);
    assert_eq!(report.range, Some((5, 10)));
}

#[test]
fn a_range_short_of_the_request_is_incomplete() {
    let a = chain(10);
    let mut opts = Options::new(Mode::Linear);
    opts.to = Some(20);
    let report = linear(&a, &a, opts);
    assert!(report.short_range);
    assert_eq!(report.verdict, Verdict::Incomplete);
    let report = linear(&[], &[], Options::new(Mode::Linear));
    assert_eq!((report.pairs, report.verdict), (0, Verdict::Incomplete));
    // --to is inclusive, and a range that reaches it exactly is not short.
    let mut opts = Options::new(Mode::Linear);
    opts.to = Some(5);
    let report = linear(&a, &a, opts);
    assert_eq!((report.pairs, report.range, report.short_range), (6, Some((0, 5)), false));
    assert_eq!(report.verdict, Verdict::Clean);
}

#[test]
fn linear_inputs_must_be_projections() {
    let mut a = chain(3);
    a.push(disconnect(3, block(3, 0), Origin::Tip, "ok"));
    let err = compare_linear(items(&a), items(&chain(3)), Options::new(Mode::Linear)).unwrap_err();
    assert!(err.to_string().contains("Left record 5"));
}

#[test]
fn stream_mode_compares_rejects_and_disconnects() {
    let base: Vec<Outcome> = vec![
        tip(0, 0),
        tip(1, 0),
        reject(2, hx(500), Origin::Tip, "bad-cb-amount"),
        tip(2, 0),
        disconnect(2, block(2, 0), Origin::Tip, "ok"),
        tip(2, 0),
    ];
    let left = as_records(&base);

    let mut why = base.clone();
    why[2].s_mut().set("why", Value::string("bad-cb-payee")).unwrap();
    let report = stream(&left, &as_records(&why), Options::new(Mode::Stream));
    assert_eq!((report.first().unwrap().h, keys(&report)), (2, vec!["R.s.why".to_string()]));

    let mut dbg = base.clone();
    dbg[2].x_mut().set("dbg", Value::string("debug text")).unwrap();
    assert_eq!(
        stream(&left, &as_records(&dbg), Options::new(Mode::Stream)).verdict,
        Verdict::Clean
    );

    let mut res = base.clone();
    res[4].s_mut().set("res", Value::string("unclean")).unwrap();
    let report = stream(&left, &as_records(&res), Options::new(Mode::Stream));
    assert_eq!(keys(&report), ["D.s.res"]);

    // A missing D: the next records no longer pair up, so the kinds differ.
    let mut missing = base.clone();
    missing.remove(4);
    let report = stream(&left, &as_records(&missing), Options::new(Mode::Stream));
    let first = report.first().unwrap();
    assert_eq!(first.what, What::Identity);
    assert_eq!(first.left.as_ref().unwrap().kind, Kind::Disconnect);

    // A record on one side only, at the end, is a gap.
    let report = stream(&left, &as_records(&base[..5]), Options::new(Mode::Stream));
    assert_eq!(report.first().unwrap().what, What::Gap(Side::Right));

    // Linear mode does not see the R or the reorg: both projections are 0..=2.
    let project = |records: &[Record]| {
        let mut p = Projector::new();
        for (i, r) in records.iter().enumerate() {
            p.feed(i + 1, r).unwrap();
        }
        p.finish()
    };
    assert_eq!(project(&left).range(), Some((0, 2)));
}

#[test]
fn stream_origins_filter_the_records() {
    let mut with_proposal = chain(4);
    with_proposal.insert(3, reject(3, block(3, 0), Origin::Proposal, "ok"));
    let a = as_records(&with_proposal);
    let b = as_records(&chain(4));
    assert_eq!(stream(&a, &b, Options::new(Mode::Stream)).verdict, Verdict::Diverged);
    let mut opts = Options::new(Mode::Stream);
    opts.origins = Some(vec![Origin::Tip]);
    assert_eq!(stream(&a, &b, opts).verdict, Verdict::Clean);
}

fn project(records: &[Record]) -> Result<dash_statedigest::compare::Projection, ProjectError> {
    let mut p = Projector::new();
    for (i, r) in records.iter().enumerate() {
        p.feed(i + 1, r)?;
    }
    Ok(p.finish())
}

#[test]
fn the_projection_follows_reorgs_and_ignores_other_origins() {
    // 0..=12 on the main branch, a reorg of 11 and 12 onto branch 1 with a failed attempt, a
    // proposal, VerifyDB's disconnect and reconnect after a restart, then 13 on branch 1.
    let mut records = as_records(&chain(12));
    for r in [
        disconnect(12, block(12, 0), Origin::Tip, "ok"),
        disconnect(11, block(11, 0), Origin::Tip, "ok"),
        disconnect(10, block(10, 0), Origin::Tip, "ok"),
        reject(10, block(10, 1), Origin::Tip, "bad-txns"),
        tip(10, 1),
        tip(11, 1),
        reject(12, block(12, 1), Origin::Proposal, "ok"),
        tip(12, 1),
    ] {
        records.push(Record::Outcome(r));
    }
    records.push(header("2-2"));
    records.push(Record::Outcome(disconnect(12, block(12, 1), Origin::Verifydb, "ok")));
    records.push(Record::Outcome(connect(12, block(12, 1), Some(block(11, 1)), Origin::Verifydb)));
    records.push(Record::Outcome(tip(13, 1)));
    let p = project(&records).unwrap();
    let hashes: Vec<Hash256> = p.chain.iter().map(|s| s.b).collect();
    let want: Vec<Hash256> = (0..=13)
        .map(|h| {
            block(
                h,
                if h >= 10 {
                    1
                } else {
                    0
                },
            )
        })
        .collect();
    assert_eq!(hashes, want);
    assert!(p.picks().collect::<Vec<_>>().windows(2).all(|w| w[0] < w[1]));
    assert_eq!(p.runs, 2);
    assert!(p.rewinds.is_empty());
}

#[test]
fn a_run_may_rewind_at_its_first_tip_connect() {
    // A -reindex-chainstate restart connects from genesis again.
    let mut records = as_records(&chain(5));
    records.push(header("2-2"));
    records.extend(chain(3).into_iter().map(Record::Outcome));
    let p = project(&records).unwrap();
    assert_eq!(p.range(), Some((0, 3)));
    assert_eq!(p.chain[0].at, 9, "the reconnect, not the first connect");
    assert_eq!(p.rewinds.len(), 1);
    assert_eq!((p.rewinds[0].run, p.rewinds[0].from, p.rewinds[0].to), (2, 5, 0));

    // Inside a run, a connect at or below the tip is an error.
    let mut records = as_records(&chain(5));
    records.push(Record::Outcome(tip(3, 0)));
    assert!(matches!(
        project(&records),
        Err(ProjectError::Overlap {
            h: 3,
            tip: 5,
            ..
        })
    ));
}

#[test]
fn a_chain_popped_empty_must_be_rebuilt_from_its_last_block() {
    // The file starts at 3 (no parent known), both blocks are disconnected, then a connect far
    // above: the chain popped empty still has a tip's child, height 3.
    let late = &chain(4)[3..];
    let mut records = as_records(late);
    records.push(Record::Outcome(disconnect(4, block(4, 0), Origin::Tip, "ok")));
    records.push(Record::Outcome(disconnect(3, block(3, 0), Origin::Tip, "ok")));
    let mut far = records.clone();
    far.push(Record::Outcome(tip(12, 0)));
    assert!(matches!(
        project(&far),
        Err(ProjectError::Gap {
            h: 12,
            tip: 2,
            ..
        })
    ));
    // A replacement for block 3 must have block 3's parent.
    let mut wrong_parent = records.clone();
    wrong_parent.push(Record::Outcome(connect(3, block(3, 1), Some(hx(77)), Origin::Tip)));
    assert!(matches!(
        project(&wrong_parent),
        Err(ProjectError::Unlinked {
            h: 3,
            ..
        })
    ));
    records.push(Record::Outcome(connect(3, block(3, 1), Some(block(2, 0)), Origin::Tip)));
    let p = project(&records).unwrap();
    assert_eq!(p.chain.iter().map(|s| s.b).collect::<Vec<_>>(), [block(3, 1)]);
}

#[test]
fn a_run_may_jump_over_unrecorded_blocks_and_linear_mode_reports_them() {
    // A crash after block 5: the restart rolls 6 and 7 forward without records (spec §1).
    let mut records = as_records(&chain(5));
    records.push(header("2-2"));
    records.extend(chain(10)[8..].iter().cloned().map(Record::Outcome));
    let p = project(&records).unwrap();
    assert_eq!(p.holes.len(), 1);
    assert_eq!((p.holes[0].run, p.holes[0].from, p.holes[0].to), (2, 6, 7));
    let heights: Vec<i64> = p.chain.iter().map(|s| s.h).collect();
    assert_eq!(heights, [0, 1, 2, 3, 4, 5, 8, 9, 10]);
    // The second pass keeps the picked records: the H of each run is not one.
    assert_eq!(p.picks().collect::<Vec<_>>(), [2, 3, 4, 5, 6, 7, 9, 10, 11]);
    // Inside a run the same jump is an error.
    let mut inside = as_records(&chain(5));
    inside.extend(chain(10)[8..].iter().cloned().map(Record::Outcome));
    assert!(matches!(
        project(&inside),
        Err(ProjectError::Gap {
            h: 8,
            ..
        })
    ));
    // Compared with a full chain, the hole is the first mismatch: a gap on the left.
    let picked: Vec<Outcome> = p
        .chain
        .iter()
        .map(|slot| match &records[slot.at - 1] {
            Record::Outcome(o) => o.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    let report = linear(&picked, &chain(10), Options::new(Mode::Linear));
    let first = report.first().unwrap();
    assert_eq!((first.h, &first.what), (6, &What::Gap(Side::Left)));
}

/// The main chain 0..=10 without heights 6 and 7, as a projection with a hole picks it.
fn holed() -> Vec<Outcome> {
    let mut c = chain(10);
    c.drain(6..8);
    c
}

fn skipped(report: &dash_statedigest::compare::Report) -> Vec<(Side, i64, i64)> {
    report.skipped.iter().map(|s| (s.side, s.from, s.to)).collect()
}

#[test]
fn a_hole_both_inputs_share_is_incomplete_never_clean() {
    let report = linear(&holed(), &holed(), Options::new(Mode::Linear));
    assert!(report.mismatches.is_empty());
    assert!(report.uncovered.is_empty());
    assert_eq!(report.pairs, 9);
    assert_eq!(skipped(&report), [(Side::Left, 6, 7), (Side::Right, 6, 7)]);
    assert_eq!(report.verdict, Verdict::Incomplete);
    assert_eq!(report.verdict.exit_code(), 3);
}

#[test]
fn a_hole_on_one_side_is_a_gap_and_is_reported_skipped() {
    let report = linear(
        &chain(10),
        &holed(),
        Options {
            keep_going: true,
            ..Options::new(Mode::Linear)
        },
    );
    let gaps: Vec<(i64, &What)> = report.mismatches.iter().map(|m| (m.h, &m.what)).collect();
    assert_eq!(gaps, [(6, &What::Gap(Side::Right)), (7, &What::Gap(Side::Right))]);
    assert_eq!(skipped(&report), [(Side::Right, 6, 7)]);
    assert_eq!(report.verdict, Verdict::Diverged);
    // Without the hole the same inputs are clean.
    assert_eq!(linear(&chain(10), &chain(10), Options::new(Mode::Linear)).verdict, Verdict::Clean);
}

#[test]
fn skipped_heights_are_clipped_to_the_compared_range() {
    let range = |from, to| Options {
        from,
        to,
        ..Options::new(Mode::Linear)
    };
    let both = |opts| linear(&holed(), &holed(), opts);
    assert_eq!(skipped(&both(range(Some(7), None))), [(Side::Left, 7, 7), (Side::Right, 7, 7)]);
    assert_eq!(skipped(&both(range(None, Some(6)))), [(Side::Left, 6, 6), (Side::Right, 6, 6)]);
    assert_eq!(skipped(&both(range(Some(1), Some(9)))), [(Side::Left, 6, 7), (Side::Right, 6, 7)]);
    // A range that misses the hole is clean on both sides of it.
    for opts in [range(Some(8), None), range(None, Some(5))] {
        let report = both(opts);
        assert!(report.skipped.is_empty());
        assert_eq!(report.verdict, Verdict::Clean);
    }
}

#[test]
fn projection_errors() {
    let mut gap = chain(5);
    gap.remove(3);
    let err = project(&as_records(&gap)).unwrap_err();
    assert!(matches!(
        err,
        ProjectError::Gap {
            h: 4,
            tip: 2,
            ..
        }
    ));
    assert_eq!(err.to_string(), "record 5: a tip connect at height 4 above the tip at 2 (gap)");

    let mut unlinked = chain(5);
    unlinked[4].s_mut().set("prev", Value::hash(&hx(4242))).unwrap();
    assert!(matches!(
        project(&as_records(&unlinked)),
        Err(ProjectError::Unlinked {
            h: 4,
            ..
        })
    ));

    let mut wrong = chain(5);
    wrong.push(disconnect(4, block(4, 0), Origin::Tip, "ok"));
    assert!(matches!(
        project(&as_records(&wrong)),
        Err(ProjectError::Disconnect {
            h: 4,
            ..
        })
    ));

    // A failed disconnect leaves the tip; a ReplayBlocks rollback that is unclean pops it.
    let mut failed = chain(5);
    failed.push(disconnect(5, block(5, 0), Origin::Tip, "failed"));
    assert_eq!(project(&as_records(&failed)).unwrap().range(), Some((0, 5)));
    let mut replay = as_records(&chain(5));
    replay.push(header("2-2"));
    replay.push(Record::Outcome(disconnect(5, block(5, 0), Origin::Replay, "unclean")));
    replay.push(Record::Outcome(tip(5, 0)));
    assert_eq!(project(&replay).unwrap().chain.last().unwrap().at, 10);

    // A digest that begins after genesis starts the chain where it begins.
    let late = as_records(&chain(9)[4..]);
    assert_eq!(project(&late).unwrap().range(), Some((4, 9)));
}

#[test]
fn field_names_resolve() {
    assert_eq!(field("C.s.prev").to_string(), "C.s.prev");
    let res: Vec<String> = FieldId::resolve("res").iter().map(ToString::to_string).collect();
    assert_eq!(res, ["R.s.res", "D.s.res"]);
    assert_eq!(FieldId::resolve("fx").len(), 3);
    assert!(FieldId::resolve("nope").is_empty());
}

#[test]
fn a_gap_is_the_present_sides_record_compared_before() {
    // Each side lacks height 5 in turn; the next pair differs in mrkl and names, per side, the
    // record compared before it: the gap's record on the side that had it.
    let full = chain(10);
    let mut changed = chain(10);
    changed[6].s_mut().set("mrkl", Value::hash(&hx(4242))).unwrap();
    let mut short = changed.clone();
    short.remove(5);
    let mut opts = Options::new(Mode::Linear);
    opts.keep_going = true;
    for (left, right, missing) in [(&short, &full, Side::Left), (&full, &short, Side::Right)] {
        let report = linear(left, right, opts.clone());
        assert!(!report.stopped);
        let [gap, keys] = &report.mismatches[..] else {
            panic!("{:?}", report.mismatches);
        };
        assert_eq!((gap.h, &gap.what), (5, &What::Gap(missing)));
        assert_eq!(keys.h, 6);
        assert_eq!(report.coverage[&field("C.s.mrkl")].differ, 1);
        let present = 1 - missing as usize;
        let h = |i: usize| keys.before[i].as_ref().map(|at| at.id.h);
        assert_eq!(h(present), Some(5), "{missing:?}");
        assert_eq!(h(missing as usize), Some(4), "{missing:?}");
    }
    assert!(linear(&short, &full, Options::new(Mode::Linear)).stopped);
}

#[test]
fn equal_keys_are_counted_and_each_entry_point_sets_its_mode() {
    let a = chain(5);
    let report = linear(&a, &a, Options::new(Mode::Stream));
    assert_eq!(report.mode, Mode::Linear);
    assert!(!report.stopped);
    assert_eq!(report.coverage[&field("C.s.mrkl")].equal, 6);
    assert!(!report.coverage.contains_key(&field("C.o.fx")));
    let records = as_records(&a);
    let report = stream(&records, &records, Options::new(Mode::Linear));
    assert_eq!(report.mode, Mode::Stream);
    assert_eq!(report.coverage[&field("C.o.fx")].equal, 6);
}

#[test]
fn a_range_starting_above_from_is_short() {
    let late = &chain(10)[5..];
    let mut opts = Options::new(Mode::Linear);
    opts.from = Some(2);
    let report = linear(late, late, opts);
    assert_eq!((report.short_range, report.verdict), (true, Verdict::Incomplete));
    // Nothing compared with only --from or only --to: short as well.
    for (from, to) in [(Some(1), None), (None, Some(1))] {
        let mut opts = Options::new(Mode::Linear);
        (opts.from, opts.to) = (from, to);
        assert!(linear(&[], &[], opts).short_range, "{from:?} {to:?}");
    }
}

#[test]
fn a_linear_input_holds_rising_tip_connects_only() {
    let base = chain(3);
    let proposal = connect(4, block(4, 0), Some(block(3, 0)), Origin::Proposal);
    let mut repeated = tip(4, 0);
    repeated.id.h = 3;
    for extra in [disconnect(4, block(4, 0), Origin::Tip, "ok"), proposal, repeated] {
        let mut left = base.clone();
        left.push(extra.clone());
        let err =
            compare_linear(items(&left), items(&chain(4)), Options::new(Mode::Linear)).unwrap_err();
        assert!(err.to_string().contains("Left record 5"), "{extra:?}: {err}");
    }
}

#[test]
fn a_rewind_into_a_hole_or_below_a_floor_has_no_recorded_parent() {
    // Run 2 jumps from 5 to 8; run 3 rewinds to 7, whose parent 6 no run recorded.
    let mut records = as_records(&chain(5));
    records.push(header("2-2"));
    records.extend(chain(10)[8..].iter().cloned().map(Record::Outcome));
    records.push(header("3-3"));
    records.push(Record::Outcome(tip(7, 0)));
    let p = project(&records).unwrap();
    assert_eq!(p.chain.iter().map(|s| s.h).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 7]);

    // A chain from 3 popped empty keeps 3 as its floor; run 2 rewinds below it, to 1.
    let mut records = as_records(&chain(4)[3..]);
    records.push(Record::Outcome(disconnect(4, block(4, 0), Origin::Tip, "ok")));
    records.push(Record::Outcome(disconnect(3, block(3, 0), Origin::Tip, "ok")));
    records.push(header("2-2"));
    records.push(Record::Outcome(connect(1, block(1, 1), Some(hx(77)), Origin::Tip)));
    let p = project(&records).unwrap();
    assert_eq!(p.chain.iter().map(|s| s.b).collect::<Vec<_>>(), [block(1, 1)]);
}

#[test]
fn a_disconnect_moves_the_run_and_a_replay_rollback_pops_without_a_rewind() {
    // A run that disconnected its tip may not rewind afterwards.
    let mut records = as_records(&chain(5));
    records.push(header("2-2"));
    records.push(Record::Outcome(disconnect(5, block(5, 0), Origin::Tip, "ok")));
    records.push(Record::Outcome(tip(3, 0)));
    assert!(matches!(
        project(&records),
        Err(ProjectError::Overlap {
            h: 3,
            ..
        })
    ));
    // An unclean ReplayBlocks rollback pops the tip, so reconnecting it is no rewind.
    let mut replay = as_records(&chain(5));
    replay.push(header("2-2"));
    replay.push(Record::Outcome(disconnect(5, block(5, 0), Origin::Replay, "unclean")));
    replay.push(Record::Outcome(tip(5, 0)));
    let p = project(&replay).unwrap();
    assert!(p.rewinds.is_empty(), "{:?}", p.rewinds);
}

#[test]
fn an_excluded_key_is_neither_compared_nor_covered() {
    let a = chain(5);
    let mut b = a.clone();
    b[3].s_mut().set("nonce", Value::Int(999)).unwrap();
    let nonce = field("C.s.nonce");

    // Without the exclusion the run diverges on that key.
    let report = linear(&a, &b, Options::new(Mode::Linear));
    assert_eq!(report.verdict, Verdict::Diverged);
    assert_eq!(keys(&report), ["C.s.nonce"]);
    assert!(report.coverage.contains_key(&nonce));

    let mut opts = Options::new(Mode::Linear);
    opts.exclude = vec![nonce];
    let report = linear(&a, &b, opts.clone());
    assert_eq!(report.verdict, Verdict::Clean);
    assert!(report.mismatches.is_empty());
    assert!(!report.coverage.contains_key(&nonce));
    assert!(report.uncovered.is_empty());
    // Other keys are still covered.
    assert!(report.coverage.contains_key(&field("C.s.ntx")));

    // The same in stream mode.
    let report = stream(&as_records(&a), &as_records(&b), opts);
    assert_eq!(report.verdict, Verdict::Clean);
    assert!(!report.coverage.contains_key(&nonce));
}

/// CH3a D167: the report counts, per side, the compared pairs whose record holds each excluded
/// key, in both modes: here one held by the right side only, one by both sides at one height
/// only, and one by neither.
#[test]
fn the_report_counts_where_each_excluded_key_is_held() {
    let (mut a, mut b) = (chain(5), chain(5));
    for c in &mut a {
        c.s_mut().remove("txouts");
    }
    for side in [&mut a, &mut b] {
        for c in side.iter_mut() {
            c.s_mut().remove("unspendable");
            if c.id.h != 5 {
                c.s_mut().remove("amount");
            }
        }
    }
    let (txouts, amount, unspendable) =
        (field("C.s.txouts"), field("C.s.amount"), field("C.s.unspendable"));
    let mut opts = Options::new(Mode::Linear);
    opts.exclude = vec![txouts, amount, unspendable];
    let want = [(txouts, [0, 6]), (amount, [1, 1]), (unspendable, [0, 0])];
    let report = linear(&a, &b, opts.clone());
    assert_eq!(report.verdict, Verdict::Clean);
    assert_eq!(report.excluded, want.into());
    let report = stream(&as_records(&a), &as_records(&b), opts);
    assert_eq!(report.excluded, want.into());
    // Nothing excluded, nothing counted.
    assert!(linear(&a, &a, Options::new(Mode::Linear)).excluded.is_empty());
}

#[test]
#[should_panic(expected = "both required and excluded")]
fn excluding_a_required_key_panics() {
    let mut opts = Options::new(Mode::Linear);
    opts.require = vec![field("C.s.nonce")];
    opts.exclude = vec![field("C.s.nonce")];
    let _ = Comparator::new(opts);
}

fn attempt(h: i64, fork: u64, t: i64, q: &[bool]) -> Attempt {
    let mut a = Attempt::new(id(h, block(h, fork), Origin::Tip), t);
    a.q = q.iter().map(|&answer| Query::Ibd(answer)).collect();
    a
}

#[test]
fn attempts_are_compared_in_strict_sequence() {
    let base = vec![
        attempt(1, 0, 100, &[true]),
        attempt(2, 0, 101, &[true, false]),
        attempt(3, 0, 102, &[]),
    ];
    assert_eq!(compare_attempts(&base, &base.clone()), None);
    assert_eq!(compare_attempts(&[], &[]), None);

    // `x` is schedule: never compared.
    let mut x = base.clone();
    x[1].x_mut().set("seq", Value::Int(42)).unwrap();
    assert_eq!(compare_attempts(&base, &x), None);

    let mut b = base.clone();
    b[2] = attempt(3, 1, 102, &[]);
    assert_eq!(
        compare_attempts(&base, &b),
        Some(AttemptDiff::Identity {
            at: 2
        })
    );

    let mut b = base.clone();
    b[1].t += 1;
    assert_eq!(
        compare_attempts(&base, &b),
        Some(AttemptDiff::Time {
            at: 1
        })
    );

    let mut b = base.clone();
    b[1].q[1] = Query::Ibd(true);
    assert_eq!(
        compare_attempts(&base, &b),
        Some(AttemptDiff::Query {
            at: 1,
            query: 1
        })
    );

    // One query list is a prefix of the other: the shorter list's length.
    let mut b = base.clone();
    b[1].q.pop();
    assert_eq!(
        compare_attempts(&base, &b),
        Some(AttemptDiff::Query {
            at: 1,
            query: 1
        })
    );

    // Lengths differ and all common pairs are equal.
    assert_eq!(
        compare_attempts(&base, &base[..2]),
        Some(AttemptDiff::Count {
            left: 3,
            right: 2
        })
    );
    assert_eq!(
        compare_attempts(&base[..1], &base),
        Some(AttemptDiff::Count {
            left: 1,
            right: 3
        })
    );

    // A difference in a common pair wins over the count.
    let mut b = base[..2].to_vec();
    b[0].t = 7;
    assert_eq!(
        compare_attempts(&base, &b),
        Some(AttemptDiff::Time {
            at: 0
        })
    );
}
