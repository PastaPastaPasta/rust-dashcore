//! The checks that bind the three files of one recorded node to each other (spec §2, §7.3, §9.2).
//!
//! A recording is a node's `digest.jsonl`, `decisions.jsonl` and `golden.jsonl`. Each file is
//! valid on its own when it parses; [`check_recording`] checks what only the three together can
//! show: that no file was swapped, cut short or rewritten after the others were made.
//!
//! * **Grammar and kinds** (§3, §4): every line parses strictly, the file starts with `H`, every
//!   `H` names the file's [`FileKind`], and the records are kinds the file holds (digest `H C R D`,
//!   decisions `H A`, golden `H C S`).
//! * **The `A` join** (§7.3): records belong to the run of the `H` before them. The two logs name
//!   the same runs in the same order. Within a run the `seq` of the `C` and `R` records strictly
//!   increases, each has exactly one `A` with the same `seq`, `h`, `b` and `org`, no `A` is left
//!   over, and a `D` has none.
//! * **The tip-connect suffix** (§9.2): the golden's `C` records run through consecutive heights,
//!   each links to the one before it, and each is the digest's *last* `org:"tip"` `C` for its block
//!   with the golden-only keys of §5.3 added to `s`. A digest that is empty, cut short or replaced
//!   fails here.
//!
//! The crate is pure, so the caller reads the files; this module sees only their bytes.

use std::collections::{BTreeMap, BTreeSet};

use crate::fields::{FIELDS, FieldSource, Kind};
use crate::hex::Hash256;
use crate::parse::Lines;
use crate::record::{FileKind, Ident, Origin, Outcome, Record};
use crate::value::Value;

/// How many records of each kind a file holds, keyed by the `"k"` letter.
pub type Tally = BTreeMap<char, u64>;

/// Per-file record counts of a checked recording.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// `H C R D` of `digest.jsonl`.
    pub digest: Tally,
    /// `H A` of `decisions.jsonl`.
    pub decisions: Tally,
    /// `H C S` of `golden.jsonl`.
    pub golden: Tally,
}

/// Whether a file of this kind holds records of `kind` (spec §3).
fn holds(file: FileKind, kind: Kind) -> bool {
    match file {
        FileKind::Digest => {
            matches!(kind, Kind::Header | Kind::Connect | Kind::Reject | Kind::Disconnect)
        }
        FileKind::Decisions => matches!(kind, Kind::Header | Kind::Attempt),
        FileKind::Golden => matches!(kind, Kind::Header | Kind::Connect | Kind::Sparse),
        FileKind::Memory => matches!(
            kind,
            Kind::Header | Kind::Connect | Kind::Reject | Kind::Disconnect | Kind::Attempt
        ),
    }
}

/// One parsed file: its tally and its records with their line numbers. `S` records are tallied
/// but not kept, as nothing here reads them.
struct Scanned {
    counts: Tally,
    records: Vec<(usize, Record)>,
}

fn scan(name: &str, bytes: &[u8], file: FileKind) -> Result<Scanned, String> {
    let mut scanned = Scanned {
        counts: Tally::new(),
        records: Vec::new(),
    };
    for (line, parsed) in Lines::new(bytes) {
        let record = parsed.map_err(|e| format!("{name}:{line}: {e}"))?;
        match &record {
            Record::Header(h) if h.file != file => {
                return Err(format!(
                    "{name}:{line}: a {} header in the {name} file",
                    h.file.as_str()
                ));
            }
            Record::Header(_) => {}
            _ if scanned.counts.is_empty() => {
                return Err(format!("{name}:{line}: the file does not start with H"));
            }
            _ => {}
        }
        if !holds(file, record.kind()) {
            let letter = char::from(record.kind().letter());
            return Err(format!("{name}:{line}: record kind {letter} in the {name} file"));
        }
        *scanned.counts.entry(char::from(record.kind().letter())).or_default() += 1;
        if !matches!(record, Record::Sparse(_)) {
            scanned.records.push((line, record));
        }
    }
    if scanned.counts.is_empty() {
        return Err(format!("{name}: the file is empty"));
    }
    Ok(scanned)
}

/// What the join needs of one attempt: where it is, who it is and its `seq`.
struct Attempt {
    line: usize,
    id: Ident,
    seq: i64,
}

/// The attempts of one run: the `C` and `R` records of the digest, or the `A` records of the
/// decisions, in file order.
struct Run<'a> {
    name: &'a [u8],
    attempts: Vec<Attempt>,
}

/// Groups a file's attempts by the `H` before them. `D` records are not attempts here.
fn runs<'a>(name: &str, records: &'a [(usize, Record)]) -> Result<Vec<Run<'a>>, String> {
    let mut runs: Vec<Run> = Vec::new();
    for (line, record) in records {
        let (id, seq) = match record {
            Record::Header(h) => {
                runs.push(Run {
                    name: &h.run,
                    attempts: Vec::new(),
                });
                continue;
            }
            Record::Outcome(o) if o.kind() != Kind::Disconnect => (&o.id, o.x().get("seq")),
            Record::Attempt(a) => (&a.id, a.x().get("seq")),
            _ => continue,
        };
        let seq = seq
            .and_then(Value::as_int)
            .ok_or_else(|| format!("{name}:{line}: the record has no x.seq"))?;
        let run = runs.last_mut().ok_or_else(|| format!("{name}:{line}: no H before it"))?;
        run.attempts.push(Attempt {
            line: *line,
            id: id.clone(),
            seq,
        });
    }
    Ok(runs)
}

fn show_id(id: &Ident) -> String {
    format!("h={} b={} org={}", id.h, id.b, id.org.as_str())
}

fn show_run(run: &[u8]) -> String {
    String::from_utf8_lossy(run).into_owned()
}

/// Joins the digest's runs with the decisions' runs (spec §7.3).
fn join(digest: &[(usize, Record)], decisions: &[(usize, Record)]) -> Result<(), String> {
    let digest_runs = runs("digest", digest)?;
    let decision_runs = runs("decisions", decisions)?;
    if digest_runs.len() != decision_runs.len() {
        return Err(format!(
            "the digest has {} runs and the decisions {}",
            digest_runs.len(),
            decision_runs.len()
        ));
    }
    for (n, (d, a)) in digest_runs.iter().zip(&decision_runs).enumerate() {
        if d.name != a.name {
            return Err(format!(
                "run {}: the digest has {:?} and the decisions {:?}",
                n + 1,
                show_run(d.name),
                show_run(a.name)
            ));
        }
        let run = show_run(d.name);
        let mut decided: BTreeMap<i64, &Attempt> = BTreeMap::new();
        for attempt in &a.attempts {
            if let Some(first) = decided.insert(attempt.seq, attempt) {
                return Err(format!(
                    "decisions:{}: run {run}: A seq {} again (first at line {})",
                    attempt.line, attempt.seq, first.line
                ));
            }
        }
        let mut last = None;
        for c in &d.attempts {
            if last.is_some_and(|last| last >= c.seq) {
                return Err(format!(
                    "digest:{}: run {run}: seq {} does not exceed the one before it",
                    c.line, c.seq
                ));
            }
            last = Some(c.seq);
            let Some(a) = decided.remove(&c.seq) else {
                return Err(format!(
                    "digest:{}: run {run}: seq {} ({}) has no A",
                    c.line,
                    c.seq,
                    show_id(&c.id)
                ));
            };
            if a.id != c.id {
                return Err(format!(
                    "digest:{}: run {run}: seq {} is {} but its A (decisions:{}) is {}",
                    c.line,
                    c.seq,
                    show_id(&c.id),
                    a.line,
                    show_id(&a.id)
                ));
            }
        }
        if let Some((seq, a)) = decided.first_key_value() {
            return Err(format!(
                "decisions:{}: run {run}: A seq {seq} ({}) has no C or R",
                a.line,
                show_id(&a.id)
            ));
        }
    }
    Ok(())
}

/// The `C` records of a file, with their line numbers.
fn connects(records: &[(usize, Record)]) -> impl Iterator<Item = (usize, &Outcome)> {
    records.iter().filter_map(|(line, r)| match r {
        Record::Outcome(o) if o.kind() == Kind::Connect => Some((*line, o)),
        _ => None,
    })
}

/// A value as short text for a message.
fn show(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "absent".into();
    };
    let mut text = String::from_utf8_lossy(&value.to_vec()).into_owned();
    if text.len() > 80 {
        let mut end = 80;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

/// Checks the golden's `C` records against the digest's last tip `C` for each block (spec §9.2).
fn suffix(digest: &[(usize, Record)], golden: &[(usize, Record)]) -> Result<(), String> {
    let golden: Vec<(usize, &Outcome)> = connects(golden).collect();
    if golden.is_empty() {
        return Err("golden: no C record".into());
    }
    let wanted: BTreeSet<Hash256> = golden.iter().map(|(_, g)| g.id.b).collect();
    let mut last_tip: BTreeMap<Hash256, (usize, &Outcome)> = BTreeMap::new();
    for (line, c) in connects(digest).filter(|(_, c)| c.id.org == Origin::Tip) {
        if wanted.contains(&c.id.b) {
            last_tip.insert(c.id.b, (line, c));
        }
    }

    let golden_only: BTreeSet<&str> = FIELDS
        .iter()
        .filter(|f| f.kind == Kind::Connect && f.source == FieldSource::Golden)
        .map(|f| f.key)
        .collect();
    for (n, (line, g)) in golden.iter().enumerate() {
        let at = format!("golden:{line}: h={} b={}", g.id.h, g.id.b);
        match n.checked_sub(1).map(|p| golden[p].1) {
            None if g.id.h == 0 && g.s().get("prev").is_none_or(|v| !v.is_null()) => {
                return Err(format!("{at}: genesis does not have prev null"));
            }
            None => {}
            Some(before) => {
                if g.id.h != before.id.h + 1 {
                    return Err(format!(
                        "{at}: follows height {} (the golden's heights are not contiguous)",
                        before.id.h
                    ));
                }
                if g.s().get("prev") != Some(&Value::hash(&before.id.b)) {
                    return Err(format!(
                        "{at}: prev is {}, not the block before it ({})",
                        show(g.s().get("prev")),
                        before.id.b
                    ));
                }
            }
        }
        let Some(&(dline, d)) = last_tip.get(&g.id.b) else {
            return Err(format!("{at}: the digest has no tip C for this block"));
        };
        let at = format!("{at} (digest:{dline})");
        if g.id != d.id {
            return Err(format!(
                "{at}: the golden is {}, the digest {}",
                show_id(&g.id),
                show_id(&d.id)
            ));
        }
        for (spec, value) in d.s().iter() {
            if g.s().get(spec.key) != Some(value) {
                return Err(format!(
                    "{at}: s.{} is {} in the golden, {} in the digest",
                    spec.key,
                    show(g.s().get(spec.key)),
                    show(Some(value))
                ));
            }
        }
        let want: BTreeSet<&str> =
            d.s().iter().map(|(spec, _)| spec.key).chain(golden_only.iter().copied()).collect();
        let have: BTreeSet<&str> = g.s().iter().map(|(spec, _)| spec.key).collect();
        if want != have {
            let missing: Vec<&str> = want.difference(&have).copied().collect();
            let extra: Vec<&str> = have.difference(&want).copied().collect();
            return Err(format!("{at}: golden s keys: missing {missing:?}, unexpected {extra:?}"));
        }
        if g.o() != d.o() {
            return Err(format!("{at}: o differs between the golden and the digest"));
        }
        if g.x() != d.x() {
            return Err(format!("{at}: x differs between the golden and the digest"));
        }
    }
    Ok(())
}

/// Checks one node's three files (see the module docs) and counts their records.
///
/// The error names the file and line of the first violation. The files are the bytes of
/// `digest.jsonl`, `decisions.jsonl` and `golden.jsonl`.
pub fn check_recording(digest: &[u8], decisions: &[u8], golden: &[u8]) -> Result<Counts, String> {
    let digest = scan("digest", digest, FileKind::Digest)?;
    let decisions = scan("decisions", decisions, FileKind::Decisions)?;
    let golden = scan("golden", golden, FileKind::Golden)?;
    join(&digest.records, &decisions.records)?;
    suffix(&digest.records, &golden.records)?;
    Ok(Counts {
        digest: digest.counts,
        decisions: decisions.counts,
        golden: golden.counts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{Attempt as A, Chain, Header, Sparse, Writer};

    fn hash(h: i64) -> Hash256 {
        let mut b = [0x11u8; 32];
        b[0] = u8::try_from(h).unwrap();
        Hash256::from_display_bytes(b)
    }

    fn id(h: i64, org: Origin) -> Ident {
        Ident {
            h,
            b: hash(h),
            org,
        }
    }

    fn header(file: FileKind, run: &str) -> Record {
        Header {
            file,
            src: Writer::Dashd,
            ver: b"test".to_vec(),
            chain: Chain::Regtest,
            run: run.as_bytes().to_vec(),
        }
        .into()
    }

    fn fill(c: &mut Outcome, h: i64, seq: i64) {
        let prev = if h == 0 {
            Value::Null
        } else {
            Value::hash(&hash(h - 1))
        };
        c.s_mut().set("prev", prev).unwrap();
        c.s_mut().set("ver", Value::Int(1)).unwrap();
        c.s_mut().set("time", Value::Int(1000 + h)).unwrap();
        c.o_mut().set_effects(&[]).unwrap();
        c.x_mut().set("seq", Value::Int(seq)).unwrap();
    }

    /// A tip `C` of the digest.
    fn tip(h: i64, seq: i64) -> Outcome {
        let mut c = Outcome::connect(id(h, Origin::Tip));
        fill(&mut c, h, seq);
        c
    }

    /// The golden's `C` for a digest `C`: the same record plus the golden-only keys.
    fn merged(c: &Outcome) -> Outcome {
        let mut g = c.clone();
        g.s_mut().set("muhash", Value::string("ab")).unwrap();
        g.s_mut().set("txouts", Value::Int(5)).unwrap();
        g.s_mut().set("amount", Value::Int(6)).unwrap();
        g.s_mut().set("unspendable", Value::Int(0)).unwrap();
        g.s_mut().set("bi", Value::Arr(vec![Value::Int(0); 7])).unwrap();
        g.s_mut().set("stats", Value::Null).unwrap();
        g
    }

    fn reject(h: i64, org: Origin, seq: i64) -> Outcome {
        let mut r = Outcome::reject(id(h, org));
        r.s_mut().set("res", Value::string("ok")).unwrap();
        r.o_mut().set_effects(&[]).unwrap();
        r.x_mut().set("seq", Value::Int(seq)).unwrap();
        r
    }

    fn disconnect(h: i64, seq: i64) -> Outcome {
        let mut d = Outcome::disconnect(id(h, Origin::Tip));
        d.s_mut().set("res", Value::string("ok")).unwrap();
        d.o_mut().set_effects(&[]).unwrap();
        d.x_mut().set("seq", Value::Int(seq)).unwrap();
        d
    }

    fn decision(h: i64, org: Origin, seq: i64) -> A {
        let mut a = A::new(id(h, org), 5);
        a.x_mut().set("seq", Value::Int(seq)).unwrap();
        a
    }

    fn bytes(records: Vec<Record>) -> Vec<u8> {
        records.iter().flat_map(Record::to_line).collect()
    }

    /// A recording of blocks 0..=3 in one run: a proposal for block 1, then each block's
    /// connect, then a disconnect of block 3 and its reconnect.
    struct Rec {
        digest: Vec<Record>,
        decisions: Vec<Record>,
        golden: Vec<Record>,
    }

    impl Rec {
        fn new() -> Rec {
            let mut digest = vec![header(FileKind::Digest, "1-2")];
            let mut decisions = vec![header(FileKind::Decisions, "1-2")];
            let mut golden = vec![header(FileKind::Golden, "x")];
            let mut seq = 0;
            let mut next = || {
                seq += 1;
                seq
            };
            let s = next();
            digest.push(tip(0, s).into());
            decisions.push(decision(0, Origin::Tip, s).into());
            let s = next();
            digest.push(reject(1, Origin::Proposal, s).into());
            decisions.push(decision(1, Origin::Proposal, s).into());
            for h in 1..=3 {
                let s = next();
                digest.push(tip(h, s).into());
                decisions.push(decision(h, Origin::Tip, s).into());
            }
            // Block 3 is disconnected and connected again: the last tip C counts.
            let s = next();
            digest.push(disconnect(3, s).into());
            let s = next();
            digest.push(tip(3, s).into());
            decisions.push(decision(3, Origin::Tip, s).into());
            for h in 0..=3 {
                let last = digest
                    .iter()
                    .rev()
                    .find_map(|r| match r {
                        Record::Outcome(o) if o.kind() == Kind::Connect && o.id.h == h => Some(o),
                        _ => None,
                    })
                    .unwrap();
                golden.push(merged(last).into());
            }
            golden.push(
                Sparse::new(3, hash(3), "getblockcount", vec![], Value::Num("3".into()))
                    .unwrap()
                    .into(),
            );
            Rec {
                digest,
                decisions,
                golden,
            }
        }

        fn check(&self) -> Result<Counts, String> {
            let (d, a, g) = (
                bytes(self.digest.clone()),
                bytes(self.decisions.clone()),
                bytes(self.golden.clone()),
            );
            check_recording(&d, &a, &g)
        }
    }

    fn fails(rec: &Rec, needle: &str) {
        match rec.check() {
            Ok(counts) => panic!("accepted: {counts:?}"),
            Err(e) => assert!(e.contains(needle), "{e:?} lacks {needle:?}"),
        }
    }

    fn tally(pairs: &[(char, u64)]) -> Tally {
        pairs.iter().copied().collect()
    }

    #[test]
    fn a_well_formed_recording_passes_and_is_counted() {
        let counts = Rec::new().check().unwrap();
        assert_eq!(counts.digest, tally(&[('H', 1), ('C', 5), ('R', 1), ('D', 1)]));
        assert_eq!(counts.decisions, tally(&[('H', 1), ('A', 6)]));
        assert_eq!(counts.golden, tally(&[('H', 1), ('C', 4), ('S', 1)]));
    }

    #[test]
    fn the_golden_only_keys_are_the_six_of_the_spec() {
        let keys: Vec<&str> = FIELDS
            .iter()
            .filter(|f| f.kind == Kind::Connect && f.source == FieldSource::Golden)
            .map(|f| f.key)
            .collect();
        assert_eq!(keys, ["muhash", "txouts", "amount", "unspendable", "bi", "stats"]);
    }

    #[test]
    fn a_digest_of_its_header_alone_fails_beside_a_full_golden() {
        let mut rec = Rec::new();
        rec.digest.truncate(1);
        fails(&rec, "decisions:2: run 1-2: A seq 1");
        rec.decisions.truncate(1);
        fails(&rec, "no tip C for this block");
    }

    #[test]
    fn a_truncated_digest_fails() {
        let mut rec = Rec::new();
        // Drop the digest's last C and the D before it; the golden's block 3 still has the C
        // before the disconnect, so cut deeper: drop block 3's connects altogether.
        rec.digest.retain(|r| !matches!(r, Record::Outcome(o) if o.id.h == 3));
        rec.decisions.retain(|r| !matches!(r, Record::Attempt(a) if a.id.h == 3));
        fails(&rec, "the digest has no tip C for this block");
    }

    #[test]
    fn a_missing_a_fails() {
        let mut rec = Rec::new();
        rec.decisions.remove(2);
        fails(&rec, "has no A");
    }

    #[test]
    fn an_a_that_names_another_origin_fails() {
        let mut rec = Rec::new();
        rec.decisions[2] = decision(1, Origin::Tip, 2).into();
        fails(&rec, "but its A");
    }

    #[test]
    fn an_a_that_names_another_block_fails() {
        let mut rec = Rec::new();
        rec.decisions[3] = decision(2, Origin::Tip, 3).into();
        fails(&rec, "but its A");
    }

    #[test]
    fn an_extra_a_fails() {
        let mut rec = Rec::new();
        rec.decisions.push(decision(3, Origin::Tip, 99).into());
        fails(&rec, "A seq 99");
        let mut rec = Rec::new();
        // An A for the disconnect's seq: a D has none.
        rec.decisions.push(decision(3, Origin::Tip, 6).into());
        fails(&rec, "A seq 6");
    }

    #[test]
    fn a_repeated_a_fails() {
        let mut rec = Rec::new();
        let again = rec.decisions[2].clone();
        rec.decisions.insert(3, again);
        fails(&rec, "again");
    }

    #[test]
    fn seq_must_increase_within_a_run() {
        let mut rec = Rec::new();
        rec.digest.swap(2, 3);
        rec.decisions.swap(2, 3);
        fails(&rec, "does not exceed");
    }

    #[test]
    fn the_runs_of_the_two_logs_must_match() {
        let mut rec = Rec::new();
        rec.decisions[0] = header(FileKind::Decisions, "1-3");
        fails(&rec, "the decisions \"1-3\"");
        let mut rec = Rec::new();
        rec.decisions.push(header(FileKind::Decisions, "2-4"));
        fails(&rec, "1 runs and the decisions 2");
    }

    #[test]
    fn two_runs_join_run_by_run() {
        let mut rec = Rec::new();
        // A second start reuses the seq numbers: legal, as seq restarts per run.
        rec.digest.push(header(FileKind::Digest, "3-4"));
        rec.digest.push(tip(4, 1).into());
        rec.decisions.push(header(FileKind::Decisions, "3-4"));
        rec.decisions.push(decision(4, Origin::Tip, 1).into());
        rec.golden.insert(5, merged(&tip(4, 1)).into());
        let counts = rec.check().unwrap();
        assert_eq!(counts.digest[&'H'], 2);
        // The first run's A for seq 1 may not stand in for the second run's.
        rec.decisions.pop();
        fails(&rec, "has no A");
    }

    #[test]
    fn the_files_must_start_with_their_header() {
        let rec = Rec::new();
        let (d, a, g) =
            (bytes(rec.digest.clone()), bytes(rec.decisions.clone()), bytes(rec.golden.clone()));
        let headless = bytes(rec.digest[1..].to_vec());
        let e = check_recording(&headless, &a, &g).unwrap_err();
        assert!(e.starts_with("digest:1: the file does not start with H"), "{e}");
        let e = check_recording(&d, &d, &g).unwrap_err();
        assert!(e.starts_with("decisions:1: a digest header in the decisions file"), "{e}");
        let e = check_recording(b"", &a, &g).unwrap_err();
        assert_eq!(e, "digest: the file is empty");
        let mut cut = d.clone();
        cut.pop();
        let e = check_recording(&cut, &a, &g).unwrap_err();
        assert!(e.starts_with(&format!("digest:{}:", rec.digest.len())), "{e}");
    }

    #[test]
    fn a_record_kind_the_file_does_not_hold_fails() {
        let mut rec = Rec::new();
        rec.digest.push(decision(3, Origin::Tip, 9).into());
        fails(&rec, "record kind A in the digest file");
        let mut rec = Rec::new();
        rec.decisions.push(tip(3, 9).into());
        fails(&rec, "record kind C in the decisions file");
        let mut rec = Rec::new();
        rec.golden.push(reject(3, Origin::Tip, 9).into());
        fails(&rec, "record kind R in the golden file");
    }

    #[test]
    fn a_golden_c_that_differs_from_the_digests_last_tip_c_fails() {
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[2] else {
            panic!()
        };
        g.s_mut().set("time", Value::Int(7)).unwrap();
        fails(&rec, "s.time is 7 in the golden");
        // The golden is made from the digest's last tip C of block 3, not an earlier one.
        let mut rec = Rec::new();
        let Record::Outcome(first) = &mut rec.digest[5] else {
            panic!()
        };
        first.s_mut().set("time", Value::Int(1)).unwrap();
        rec.check().unwrap();
        let Record::Outcome(g) = &mut rec.golden[4] else {
            panic!()
        };
        g.s_mut().set("time", Value::Int(1)).unwrap();
        fails(&rec, "s.time is 1 in the golden");
    }

    #[test]
    fn the_golden_adds_exactly_the_golden_only_keys() {
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.s_mut().remove("muhash");
        fails(&rec, "missing [\"muhash\"]");
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.s_mut().set("mrkl", Value::hash(&hash(9))).unwrap();
        fails(&rec, "unexpected [\"mrkl\"]");
        // A key the digest has and the golden dropped.
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.s_mut().remove("ver");
        fails(&rec, "s.ver is absent in the golden");
    }

    #[test]
    fn the_golden_must_agree_on_o_and_x() {
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.x_mut().set("seq", Value::Int(77)).unwrap();
        fails(&rec, "x differs");
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.o_mut().remove("fx");
        fails(&rec, "o differs");
    }

    #[test]
    fn a_gap_in_the_golden_heights_fails() {
        let mut rec = Rec::new();
        rec.golden.remove(2);
        fails(&rec, "follows height 0");
        // A repeat is as bad as a gap.
        let mut rec = Rec::new();
        let again = rec.golden[2].clone();
        rec.golden.insert(3, again);
        fails(&rec, "follows height 1");
    }

    #[test]
    fn a_golden_c_must_link_to_the_one_before() {
        let mut rec = Rec::new();
        let (Record::Outcome(g), Record::Outcome(d)) = (&mut rec.golden[3], &mut rec.digest[4])
        else {
            panic!()
        };
        // Both files say the block's prev is wrong: they agree with each other, not the chain.
        g.s_mut().set("prev", Value::hash(&hash(9))).unwrap();
        d.s_mut().set("prev", Value::hash(&hash(9))).unwrap();
        fails(&rec, "not the block before it");
    }

    #[test]
    fn the_suffix_may_start_above_genesis_but_genesis_has_no_prev() {
        // A node that began from a copied datadir: the golden starts at height 2, prev unchecked.
        let mut rec = Rec::new();
        rec.golden.drain(1..3);
        rec.check().unwrap();
        let mut rec = Rec::new();
        let Record::Outcome(g) = &mut rec.golden[1] else {
            panic!()
        };
        g.s_mut().set("prev", Value::hash(&hash(9))).unwrap();
        let Record::Outcome(d) = &mut rec.digest[1] else {
            panic!()
        };
        d.s_mut().set("prev", Value::hash(&hash(9))).unwrap();
        fails(&rec, "genesis does not have prev null");
    }

    #[test]
    fn a_golden_without_a_c_fails() {
        let mut rec = Rec::new();
        rec.golden.truncate(1);
        fails(&rec, "golden: no C record");
    }

    #[test]
    fn only_tip_connects_count_for_the_golden() {
        let mut rec = Rec::new();
        // A replay C for block 2 with another time must not be taken as block 2's.
        let mut other = Outcome::connect(id(2, Origin::Replay));
        fill(&mut other, 2, 50);
        other.s_mut().set("time", Value::Int(1)).unwrap();
        rec.digest.push(other.into());
        rec.decisions.push(decision(2, Origin::Replay, 50).into());
        rec.check().unwrap();
    }
}
