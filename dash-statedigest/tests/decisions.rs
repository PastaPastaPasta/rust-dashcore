//! `RecordedDecisions` (spec §7.2, §7.3, §10.5) on records built through the crate's API.
//!
//! Logic inputs, not Core fixtures: no value here comes from Dash Core. The load over Core's
//! recordings is tested in dash-conformance.

use dash_statedigest::decisions::{
    Answer, Ask, AttemptKey, LoadError, Miss, Policy, RecordedDecisions,
};
use dash_statedigest::{
    Attempt, Chain, FileKind, Hash256, Header, Ident, Origin, Outcome, Query, Record, Value, Writer,
};

fn hx(n: u64) -> Hash256 {
    let mut bytes = [0u8; 32];
    bytes[24..].copy_from_slice(&n.to_be_bytes());
    Hash256::from_display_bytes(bytes)
}

fn id(h: i64, b: u64, org: Origin) -> Ident {
    Ident {
        h,
        b: hx(b),
        org,
    }
}

fn header(file: FileKind, run: &str) -> Record {
    Record::Header(Header {
        file,
        src: Writer::Dashd,
        ver: b"test".to_vec(),
        chain: Chain::Regtest,
        run: run.as_bytes().to_vec(),
    })
}

fn outcome(mut o: Outcome, seq: i64) -> Record {
    o.x_mut().set("seq", Value::Int(seq)).unwrap();
    Record::Outcome(o)
}

fn connect(h: i64, b: u64, org: Origin, seq: i64) -> Record {
    outcome(Outcome::connect(id(h, b, org)), seq)
}

fn reject(h: i64, b: u64, org: Origin, seq: i64) -> Record {
    let mut r = Outcome::reject(id(h, b, org));
    r.s_mut().set("res", Value::string("invalid")).unwrap();
    outcome(r, seq)
}

fn disconnect(h: i64, b: u64, seq: i64) -> Record {
    outcome(Outcome::disconnect(id(h, b, Origin::Tip)), seq)
}

fn attempt(h: i64, b: u64, org: Origin, seq: i64, t: i64, q: Vec<Query>) -> Record {
    let mut a = Attempt::new(id(h, b, org), t);
    a.q = q;
    a.x_mut().set("seq", Value::Int(seq)).unwrap();
    Record::Attempt(a)
}

fn bytes(records: &[Record]) -> Vec<u8> {
    records.iter().flat_map(Record::to_line).collect()
}

fn ibd(answer: bool) -> Query {
    Query::Ibd(answer)
}

fn cl_has(height: i64, answer: bool) -> Query {
    Query::ClHas {
        height,
        hash: hx(height as u64),
        answer,
    }
}

/// Block 7 at height 1 fails once, connects, is disconnected and connects again; block 8 is a
/// proposal; the second run (a restart) connects block 9.
fn recording() -> (Vec<u8>, Vec<u8>) {
    let digest = bytes(&[
        header(FileKind::Digest, "1-1"),
        reject(1, 7, Origin::Tip, 1),
        connect(1, 7, Origin::Tip, 2),
        reject(2, 8, Origin::Proposal, 3),
        disconnect(1, 7, 4),
        connect(1, 7, Origin::Tip, 5),
        header(FileKind::Digest, "2-2"),
        connect(2, 9, Origin::Tip, 1),
    ]);
    let decisions = bytes(&[
        header(FileKind::Decisions, "1-1"),
        attempt(1, 7, Origin::Tip, 1, 100, vec![ibd(true)]),
        attempt(1, 7, Origin::Tip, 2, 101, vec![ibd(true), cl_has(1, false)]),
        attempt(2, 8, Origin::Proposal, 3, 102, vec![]),
        attempt(1, 7, Origin::Tip, 5, 103, vec![ibd(false), cl_has(1, true), cl_has(1, false)]),
        header(FileKind::Decisions, "2-2"),
        attempt(2, 9, Origin::Tip, 1, 200, vec![ibd(false)]),
    ]);
    (digest, decisions)
}

fn key(b: u64, org: Origin, n: u32) -> AttemptKey {
    AttemptKey {
        b: hx(b),
        org,
        n,
    }
}

#[test]
fn loads_and_numbers_attempts_per_block_and_origin() {
    let (digest, decisions) = recording();
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    assert_eq!(rec.attempts().len(), 5);
    assert_eq!(rec.get(&key(7, Origin::Tip, 0)).unwrap().seq, 1);
    assert!(!rec.get(&key(7, Origin::Tip, 0)).unwrap().connected);
    assert_eq!(rec.get(&key(7, Origin::Tip, 2)).unwrap().t, 103);
    assert!(rec.get(&key(7, Origin::Tip, 3)).is_none());
    assert_eq!(rec.get(&key(8, Origin::Proposal, 0)).unwrap().run, 1);
    assert_eq!(rec.get(&key(9, Origin::Tip, 0)).unwrap().run, 2);
    // The final connect of block 7 is its third tip attempt (n = 2), not the first.
    assert_eq!(rec.final_connect(&hx(7)), Some(key(7, Origin::Tip, 2)));
    assert_eq!(rec.final_connect(&hx(8)), None, "a proposal is not a tip connect");
}

#[test]
fn keyed_lookup_answers_in_recorded_order_and_never_guesses() {
    let (digest, decisions) = recording();
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    let mut session = rec.session(Policy::Keyed);
    let first = session.begin(&hx(7), Origin::Tip).unwrap();
    assert_eq!(first.adjusted_time(), 100);
    drop(first);
    let _second = session.begin(&hx(7), Origin::Tip).unwrap();
    let mut third = session.begin(&hx(7), Origin::Tip).unwrap();
    assert_eq!(third.attempt().seq, 5);
    let has = Ask::ClHas {
        height: 1,
        hash: hx(1),
    };
    // Asked before `ibd` (keyed allows reordering); two answers, returned in order.
    assert_eq!(third.ask(&has), Ok(Answer::Bool(true)));
    assert_eq!(third.ask(&has), Ok(Answer::Bool(false)));
    // A third ask is ambiguous (the answers differed): a miss.
    assert!(matches!(third.ask(&has), Err(Miss::NotRecorded { .. })));
    assert!(matches!(third.ask(&Ask::GovValid), Err(Miss::NotRecorded { .. })));
    // `ibd` was recorded once; asking it twice repeats the one answer.
    assert_eq!(third.ask(&Ask::Ibd), Ok(Answer::Bool(false)));
    assert_eq!(third.ask(&Ask::Ibd), Ok(Answer::Bool(false)));
    assert_eq!(third.finish(), Ok(vec![]));

    assert!(matches!(session.begin(&hx(7), Origin::Tip), Err(Miss::NoAttempt(k)) if k.n == 3));

    // Unasked queries are reported, not failed.
    let s = rec.session(Policy::Keyed);
    let cursor = s.begin_at(key(7, Origin::Tip, 1)).unwrap();
    assert_eq!(cursor.finish().unwrap().len(), 2);
}

#[test]
fn strict_lookup_requires_the_recorded_order() {
    let (digest, decisions) = recording();
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    let session = rec.session(Policy::Strict);
    let mut c = session.begin_at(rec.final_connect(&hx(7)).unwrap()).unwrap();
    let has = Ask::ClHas {
        height: 1,
        hash: hx(1),
    };
    let err = c.ask(&has).unwrap_err();
    assert!(matches!(
        err,
        Miss::OutOfOrder {
            index: 0,
            expected: Some(ref q),
            ..
        } if **q == Query::Ibd(false)
    ));
    assert_eq!(c.ask(&Ask::Ibd), Ok(Answer::Bool(false)));
    assert_eq!(c.ask(&has), Ok(Answer::Bool(true)));
    assert!(matches!(
        c.finish(),
        Err(Miss::Unasked {
            remaining: 1,
            ..
        })
    ));
}

/// An owned cursor goes on where the borrowed one stood, after the recording is gone.
#[test]
fn an_owned_cursor_keeps_its_place() {
    let (digest, decisions) = recording();
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    let has = Ask::ClHas {
        height: 1,
        hash: hx(1),
    };
    let mut c = rec.session(Policy::Strict).begin_at(key(7, Origin::Tip, 2)).unwrap();
    assert_eq!(c.ask(&Ask::Ibd), Ok(Answer::Bool(false)));
    let mut owned = c.into_owned();
    drop(rec);
    assert_eq!(owned.attempt().seq, 5);
    assert_eq!(owned.ask(&has), Ok(Answer::Bool(true)));
    assert_eq!(owned.ask(&has), Ok(Answer::Bool(false)));
    assert_eq!(owned.finish(), Ok(vec![]));
}

#[test]
fn orphans_are_reported() {
    let (digest, decisions) = recording();
    // An A whose attempt is missing, and an attempt whose A is missing.
    let mut lines: Vec<&[u8]> = decisions.split_inclusive(|&b| b == b'\n').collect();
    lines.remove(2);
    let cut_decisions: Vec<u8> = lines.concat();
    match RecordedDecisions::load(&digest, &cut_decisions) {
        Err(LoadError::Orphans(orphans)) => {
            assert_eq!(orphans.len(), 1);
            assert_eq!((orphans[0].file, orphans[0].seq), ("digest", Some(2)));
        }
        other => panic!("{other:?}"),
    }
    let mut lines: Vec<&[u8]> = digest.split_inclusive(|&b| b == b'\n').collect();
    lines.remove(3);
    let cut_digest: Vec<u8> = lines.concat();
    match RecordedDecisions::load(&cut_digest, &decisions) {
        Err(LoadError::Orphans(orphans)) => {
            assert_eq!(orphans.len(), 1);
            assert_eq!((orphans[0].file, orphans[0].line), ("decisions", 4));
        }
        other => panic!("{other:?}"),
    }
    // A decisions run the digest does not have: all its A records are orphans.
    let other_run =
        bytes(&[header(FileKind::Decisions, "9-9"), attempt(1, 7, Origin::Tip, 1, 100, vec![])]);
    assert!(
        matches!(RecordedDecisions::load(&digest, &other_run), Err(LoadError::Orphans(o)) if o.len() == 1)
    );
    // A digest run without decisions (only -statedigestlog set) is not an orphan.
    let only_second =
        bytes(&[header(FileKind::Decisions, "2-2"), attempt(2, 9, Origin::Tip, 1, 200, vec![])]);
    assert_eq!(RecordedDecisions::load(&digest, &only_second).unwrap().attempts().len(), 1);
}

#[test]
fn the_final_connect_may_be_in_a_run_without_decisions() {
    // Run 2 (digest only) connects block 7 again: the golden merges that C, which has no A.
    let digest = bytes(&[
        header(FileKind::Digest, "1-1"),
        connect(1, 7, Origin::Tip, 1),
        header(FileKind::Digest, "2-2"),
        connect(1, 7, Origin::Tip, 1),
    ]);
    let decisions =
        bytes(&[header(FileKind::Decisions, "1-1"), attempt(1, 7, Origin::Tip, 1, 100, vec![])]);
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    assert!(rec.get(&key(7, Origin::Tip, 0)).is_some());
    assert_eq!(rec.final_connect(&hx(7)), None);
    assert!(rec.has_tip_connect(&hx(7)), "connected, in a run without decisions");
    assert!(!rec.has_tip_connect(&hx(8)));
}

#[test]
fn a_disconnect_shares_the_seq_numbering() {
    let digest = bytes(&[
        header(FileKind::Digest, "1-1"),
        connect(1, 7, Origin::Tip, 1),
        disconnect(1, 7, 1),
    ]);
    let decisions =
        bytes(&[header(FileKind::Decisions, "1-1"), attempt(1, 7, Origin::Tip, 1, 100, vec![])]);
    assert!(matches!(
        RecordedDecisions::load(&digest, &decisions),
        Err(LoadError::DuplicateSeq {
            file: "digest",
            line: 3,
            seq: 1
        })
    ));
}

#[test]
fn a_joined_pair_must_name_the_same_attempt() {
    let digest = bytes(&[header(FileKind::Digest, "1-1"), connect(1, 7, Origin::Tip, 1)]);
    let decisions =
        bytes(&[header(FileKind::Decisions, "1-1"), attempt(1, 8, Origin::Tip, 1, 0, vec![])]);
    assert!(matches!(
        RecordedDecisions::load(&digest, &decisions),
        Err(LoadError::Mismatched {
            line: 2,
            ..
        })
    ));
    let wrong_kind = bytes(&[header(FileKind::Decisions, "1-1"), connect(1, 7, Origin::Tip, 1)]);
    assert!(matches!(
        RecordedDecisions::load(&digest, &wrong_kind),
        Err(LoadError::Malformed {
            file: "decisions",
            line: 2,
            ..
        })
    ));
}

#[test]
fn final_connect_counts_tip_connects_only() {
    // Block 7 connects at the tip twice, then as a proposal: the golden's C is the second tip
    // one, which a later C of another origin does not replace.
    let digest = bytes(&[
        header(FileKind::Digest, "1-1"),
        connect(1, 7, Origin::Tip, 1),
        disconnect(1, 7, 2),
        connect(1, 7, Origin::Tip, 3),
        connect(1, 7, Origin::Proposal, 4),
    ]);
    let decisions = bytes(&[
        header(FileKind::Decisions, "1-1"),
        attempt(1, 7, Origin::Tip, 1, 100, vec![]),
        attempt(1, 7, Origin::Tip, 3, 100, vec![]),
        attempt(1, 7, Origin::Proposal, 4, 100, vec![]),
    ]);
    let rec = RecordedDecisions::load(&digest, &decisions).unwrap();
    assert_eq!(rec.final_connect(&hx(7)), Some(key(7, Origin::Tip, 1)));
}

#[test]
fn errors_name_the_attempt_and_the_record() {
    let k = key(7, Origin::Tip, 2);
    assert_eq!(k.to_string(), format!("attempt {} tip #2", hx(7)));
    assert_eq!(
        Miss::NoAttempt(k).to_string(),
        format!("attempt {} tip #2: not in the recording", hx(7))
    );
    let err = LoadError::DuplicateSeq {
        file: "digest",
        line: 3,
        seq: 1,
    };
    assert_eq!(err.to_string(), "digest:3: seq 1 appears twice in its run");
}
