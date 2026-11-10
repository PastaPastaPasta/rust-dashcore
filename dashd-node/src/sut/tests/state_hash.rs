//! CH7a-5b C3: T-RD's state-hash mode. A [`DigestRun`] is a `DigestLog` the SUT's connector
//! writes to a file from genesis (X5's pending sink, [`NodeSut::new_with_digest`]); and
//! [`linear_clean`] compares it with a golden in linear mode under I4's contract.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::stats::CoinStatsAccumulator;
use dash_consensus::primitives::BlockHash;
use dash_statedigest::compare::{
    FieldId, Options, Projector, Report, Slot, Verdict, compare_linear,
};
use dash_statedigest::{
    Chain, Class, FileKind, Header, Kind, Lines, Origin, Outcome, ParseError, Record, Writer,
    parse_line,
};
use dashd_chainstate::chain::{ConnectCx, Fatal};
use dashd_chainstate::connect::{DigestAttempt, DigestLog, DigestSink, MuHashAt};

/// A `C` record's `s` keys, all 22 (O8 §5.1–§5.3), in the field table's order.
pub(crate) const ALL_C_S: [&str; 22] = [
    "prev",
    "ver",
    "mrkl",
    "time",
    "bits",
    "nonce",
    "work",
    "ntx",
    "undo",
    "mnl",
    "qc",
    "cp",
    "ehf",
    "bls",
    "bur",
    "dep",
    "muhash",
    "txouts",
    "amount",
    "unspendable",
    "bi",
    "stats",
];

/// O8 §5.2's evo keys, which hook H11 writes.
pub(crate) const EVO_KEYS: [&str; 4] = ["mnl", "qc", "cp", "ehf"];

/// A state-hash log the SUT writes: its file and the heights whose `muhash` it writes.
pub(crate) struct DigestRun {
    path: PathBuf,
    muhash: MuHashAt,
}

impl DigestRun {
    /// A regtest log at `dir/log.jsonl` writing `muhash` at `muhash`'s heights, and its sink,
    /// for [`NodeSut::new_with_digest`].
    pub(crate) fn new(dir: &Path, muhash: MuHashAt) -> (DigestRun, Box<dyn DigestSink>) {
        Self::on(dir, muhash, Chain::Regtest)
    }

    /// [`new`](Self::new) on `chain`.
    pub(crate) fn on(
        dir: &Path,
        muhash: MuHashAt,
        chain: Chain,
    ) -> (DigestRun, Box<dyn DigestSink>) {
        let (run, log) = Self::open(dir, muhash, chain);
        (run, Box::new(log))
    }

    /// [`on`](Self::on) writing to `file`, already open at `path` (CH7b-3b C14 creates it fresh).
    pub(crate) fn on_file(
        path: PathBuf,
        file: File,
        muhash: MuHashAt,
        chain: Chain,
    ) -> (DigestRun, Box<dyn DigestSink>) {
        let (run, log) = Self::writing(path, file, muhash, chain);
        (run, Box::new(log))
    }

    fn open(dir: &Path, muhash: MuHashAt, chain: Chain) -> (DigestRun, DigestLog<File>) {
        let path = dir.join("log.jsonl");
        let file = File::create(&path).expect("the log file");
        Self::writing(path, file, muhash, chain)
    }

    fn writing(
        path: PathBuf,
        file: File,
        muhash: MuHashAt,
        chain: Chain,
    ) -> (DigestRun, DigestLog<File>) {
        let log = DigestLog::new(file, start_header(chain, 1)).expect("the H is written");
        let log = log.with_muhash_at(muhash.clone());
        (
            DigestRun {
                path,
                muhash,
            },
            log,
        )
    }

    /// [`new`](Self::new) for a run of several starts (OD-CP-44's segment-wise replay), one
    /// file across them as dashd's: each start's log opens with its own `H`, its accumulator the
    /// last start's at that start's tip, as Core's coinstats index keeps it across a restart.
    /// [`SharedSink::arm`] arms the first open, [`SharedSink::restart`] each later one.
    pub(crate) fn shared(dir: &Path, muhash: MuHashAt) -> (DigestRun, SharedSink) {
        let (run, log) = Self::open(dir, muhash.clone(), Chain::Regtest);
        let state = Start::Live(log);
        (
            run,
            SharedSink {
                state: Arc::new(Mutex::new(state)),
                muhash,
                starts: Arc::new(AtomicU32::new(1)),
            },
        )
    }

    /// The log's path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The heights whose `muhash` the log writes: `None` for every height.
    pub(crate) fn selected(&self) -> Option<&BTreeSet<u32>> {
        match &self.muhash {
            MuHashAt::Every => None,
            MuHashAt::Only(heights) => Some(heights),
        }
    }

    /// The log's records, `H` first.
    pub(crate) fn records(&self) -> Vec<Record> {
        read_records(&self.path)
    }
}

/// A start's `H`: rdashd's, its `run` the start's number.
fn start_header(chain: Chain, start: u32) -> Header {
    Header {
        file: FileKind::Digest,
        src: Writer::Rdashd,
        ver: b"sut".to_vec(),
        chain,
        run: format!("1-{start}").into_bytes(),
    }
}

/// One state-hash file that several starts' connectors write in turn ([`DigestRun::shared`]).
pub(crate) struct SharedSink {
    state: Arc<Mutex<Start>>,
    muhash: MuHashAt,
    starts: Arc<AtomicU32>,
}

/// The shared file's log: the current start's, or the next start's parts until its first
/// record, which looks up the block the accumulator is at in that start's index.
enum Start {
    Live(DigestLog<File>),
    Next {
        out: File,
        stats: CoinStatsAccumulator,
        tip: BlockHash,
        header: Header,
    },
    Taken,
}

impl SharedSink {
    /// The first open's sink ([`NodeSut::new_with_digest`]'s slot).
    pub(crate) fn arm(&self) -> Box<dyn DigestSink> {
        Box::new(Armed {
            state: Arc::clone(&self.state),
            muhash: self.muhash.clone(),
        })
    }

    /// The sink of the open after a clean restart at `tip`, the block the last start's
    /// accumulator is at: the next start's log, opened with its `H` at its first record, seeded
    /// with that accumulator at `tip` ([`DigestLog::seeded`]).
    pub(crate) fn restart(&self, tip: BlockHash) -> Box<dyn DigestSink> {
        let mut state = self.state.lock().expect("the shared log");
        let Start::Live(log) = std::mem::replace(&mut *state, Start::Taken) else {
            panic!("a restart before the last start's first record");
        };
        let (out, stats) = log.into_parts();
        let start = self.starts.fetch_add(1, Ordering::Relaxed) + 1;
        *state = Start::Next {
            out,
            stats,
            tip,
            header: start_header(Chain::Regtest, start),
        };
        drop(state);
        self.arm()
    }
}

/// [`SharedSink`]'s sink for one open.
struct Armed {
    state: Arc<Mutex<Start>>,
    muhash: MuHashAt,
}

impl DigestSink for Armed {
    fn record(&mut self, cx: &ConnectCx<'_>, attempt: &DigestAttempt<'_>) -> Result<(), Fatal> {
        let mut state = self.state.lock().expect("the shared log");
        if matches!(*state, Start::Next { .. }) {
            let Start::Next {
                out,
                stats,
                tip,
                header,
            } = std::mem::replace(&mut *state, Start::Taken)
            else {
                unreachable!()
            };
            let at = cx.reader.lookup(&tip).ok_or_else(|| {
                Fatal::new("statedigest: the last start's tip is not in this start's index")
            })?;
            let log = DigestLog::seeded(out, header, stats, at)?;
            *state = Start::Live(log.with_muhash_at(self.muhash.clone()));
        }
        match &mut *state {
            Start::Live(log) => log.record(cx, attempt),
            _ => Err(Fatal::new("statedigest: no live log")),
        }
    }
}

/// `path`'s records, zstd-decoded if it ends in `.zst`.
pub(crate) fn read_records(path: &Path) -> Vec<Record> {
    let bytes = read(path);
    Lines::new(&bytes)
        .map(|(line, r)| r.unwrap_or_else(|e| panic!("{}:{line}: {e}", path.display())))
        .collect()
}

fn read(path: &Path) -> Vec<u8> {
    let file = std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if path.extension().is_some_and(|e| e == "zst") {
        zstd::decode_all(file).expect("zstd")
    } else {
        std::io::Read::bytes(file).collect::<Result<_, _>>().expect("read")
    }
}

/// `records`' final active chain, as linear mode pairs it: the projected `tip` `C`s, with their
/// record indexes.
pub(crate) fn projected(name: &str, records: &[Record]) -> Vec<(usize, Outcome)> {
    let mut p = Projector::new();
    for (at, r) in records.iter().enumerate() {
        p.feed(at, r).unwrap_or_else(|e| panic!("{name}: {e}"));
    }
    let picks: BTreeSet<usize> = p.finish().picks().collect();
    records
        .iter()
        .enumerate()
        .filter(|(at, _)| picks.contains(at))
        .map(|(at, r)| match r {
            Record::Outcome(o) => (at, o.clone()),
            other => panic!("{name}: {at} is not an outcome: {other:?}"),
        })
        .collect()
}

fn c_s(key: &str) -> FieldId {
    FieldId::find(Kind::Connect, Class::Semantic, key).expect("a C.s key")
}

/// What [`linear_clean`] compared: the pairs, and the heights where both sides' `muhash` was
/// compared equal.
#[derive(Debug)]
pub(crate) struct Clean {
    pub pairs: u64,
    pub muhash_equal: u64,
}

/// One input of [`linear_clean`]: a file read in two passes from its start. Its stream is the
/// file's bytes, or the zstd decoder's output for a `.zst` file. Pass 1 projects the stream and
/// keeps only the projection's slots; pass 2 yields the slots' records to linear mode. Neither
/// pass holds the stream or its records.
struct Side<'a> {
    name: &'static str,
    path: &'a Path,
}

/// What one pass read through the end of a side's stream: its records, counted, and its bytes'
/// sha256.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Seen {
    records: u64,
    sha256: [u8; 32],
}

/// One pass over a side's stream from its start: each line read through its newline, hashed and
/// counted, then parsed. `parse_line` is strict: it refuses a blank line, a final fragment without
/// its newline and a non-canonical line.
struct Pass {
    lines: Box<dyn BufRead>,
    line: Vec<u8>,
    sha256: dash_sha256::Sha256,
    records: u64,
}

impl Pass {
    /// The next line's index and record, `None` at the stream's end, an error on a failed read or
    /// decompress.
    fn next_record(&mut self) -> std::io::Result<Option<(usize, Result<Record, ParseError>)>> {
        self.line.clear();
        if self.lines.read_until(b'\n', &mut self.line)? == 0 {
            return Ok(None);
        }
        self.sha256.update(&self.line);
        let at = usize::try_from(self.records).expect("an index");
        self.records += 1;
        Ok(Some((at, parse_line(&self.line))))
    }

    fn seen(self) -> Seen {
        Seen {
            records: self.records,
            sha256: self.sha256.finalize(),
        }
    }
}

impl<'a> Side<'a> {
    /// A pass from the start of the stream.
    fn pass(&self) -> std::io::Result<Pass> {
        let file = File::open(self.path)?;
        let lines: Box<dyn BufRead> = if self.path.extension().is_some_and(|e| e == "zst") {
            Box::new(BufReader::new(zstd::stream::read::Decoder::new(file)?))
        } else {
            Box::new(BufReader::new(file))
        };
        Ok(Pass {
            lines,
            line: Vec::new(),
            sha256: dash_sha256::Sha256::new(),
            records: 0,
        })
    }

    /// Pass 1, through the end of the stream: the projection's slots, as [`projected`] keeps them,
    /// `each` seeing every raw record before it is projected; and what the pass read. A failed
    /// read, decompress or parse panics with the file and the line, as [`read_records`] does.
    fn project(&self, mut each: impl FnMut(usize, &Record)) -> (Vec<Slot>, Seen) {
        let path = self.path.display();
        let mut pass = self.pass().unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut p = Projector::new();
        while let Some((at, r)) = pass.next_record().unwrap_or_else(|e| panic!("{path}: {e}")) {
            let r = r.unwrap_or_else(|e| panic!("{path}:{}: {e}", at + 1));
            each(at, &r);
            p.feed(at, &r).unwrap_or_else(|e| panic!("{}: {e}", self.name));
        }
        (p.finish().chain, pass.seen())
    }

    /// Pass 2: `slots`' records, in order, as linear mode takes them, each checked against pass
    /// 1 ([`Picked`]).
    fn picked(&self, slots: Vec<Slot>, first: Seen) -> Picked<'_> {
        let pass = self.pass().unwrap_or_else(|e| self.changed(&format!("an open failed: {e}")));
        Picked {
            side: self,
            pass,
            slots: slots.into_iter(),
            first,
        }
    }

    /// The refusal when pass 2 does not read pass 1's input, saying `what` differs and no value.
    fn changed(&self, what: &str) -> ! {
        panic!(
            "{}: {}: the input changed between the two passes ({what}); the comparison is refused",
            self.name,
            self.path.display()
        )
    }
}

/// [`Side::picked`]'s records. Pass 1 read these bytes into these slots, so the comparison is
/// refused when pass 2 reads otherwise: a failed read, decompress or parse, a slot whose record is
/// gone or is not the slot's tip connect (its height and block), or, at the end of the stream, a
/// record count or a sha256 that is not pass 1's.
struct Picked<'a> {
    side: &'a Side<'a>,
    pass: Pass,
    slots: std::vec::IntoIter<Slot>,
    first: Seen,
}

impl Iterator for Picked<'_> {
    type Item = Result<(usize, Outcome), Infallible>;

    fn next(&mut self) -> Option<Self::Item> {
        let slot = self.slots.next()?;
        loop {
            let Some((at, r)) = self.read() else {
                self.side.changed(&format!("record {}, height {}, is gone", slot.at, slot.h))
            };
            if at < slot.at {
                continue;
            }
            return match r {
                Record::Outcome(o)
                    if o.kind() == Kind::Connect
                        && o.id.org == Origin::Tip
                        && (o.id.h, o.id.b) == (slot.h, slot.b) =>
                {
                    Some(Ok((at, o)))
                }
                _ => self.side.changed(&format!("record {at} is not height {}'s pick", slot.h)),
            };
        }
    }
}

impl Picked<'_> {
    /// Pass 2's next record, `None` at the end of the stream; a failed read, decompress or parse is
    /// refused.
    fn read(&mut self) -> Option<(usize, Record)> {
        match self.pass.next_record() {
            Ok(Some((at, Ok(r)))) => Some((at, r)),
            Ok(Some((at, Err(_)))) => self.side.changed(&format!("line {} does not parse", at + 1)),
            Ok(None) => None,
            Err(e) => self.side.changed(&format!("a read failed: {e}")),
        }
    }

    /// The rest of the slots taken, each seen by `each`, then the rest of the stream read through
    /// its end: its record count and sha256 must be pass 1's.
    fn finish(mut self, mut each: impl FnMut(&Outcome)) {
        for item in self.by_ref() {
            let Ok((_, o)) = item;
            each(&o);
        }
        while self.read().is_some() {}
        let Picked {
            side,
            pass,
            first,
            ..
        } = self;
        let again = pass.seen();
        if again.records != first.records {
            side.changed(&format!("{} records, pass 1 read {}", again.records, first.records));
        }
        if again.sha256 != first.sha256 {
            side.changed("the stream's sha256");
        }
    }
}

/// I3's genesis check, for each raw record of a SUT's log in turn, before it is projected: the log
/// opens with its `H`, then genesis's `C`, the log's only `C` at height 0; `seen` says it came.
fn genesis_once(at: usize, record: &Record, seen: &mut bool) {
    let genesis = matches!(record, Record::Outcome(o) if o.kind() == Kind::Connect && o.id.h == 0);
    match at {
        0 => {
            assert!(matches!(record, Record::Header(_)), "rdashd: the log does not open with an H")
        }
        1 => {
            assert!(genesis, "rdashd: the record after the H is not genesis's C");
            *seen = true;
        }
        _ => assert!(!genesis, "rdashd: a second genesis C, at record {at}"),
    }
}

/// I4: `run`'s log (left) against `golden` (right) in linear mode over `from..=to`.
/// - Every key but `muhash` is required at every height in range, present on both sides and
///   equal, with no gap and no skipped height: exactly `to − from + 1` pairs.
/// - Under `MuHashAt::Every`, `muhash` is required too, so only exit 0 passes.
/// - Under `MuHashAt::Only(selected)`, `muhash` is present on both sides and equal at every
///   selected height in range, and absent on the left at every other height (the golden's
///   presence there is ignored). `require` names the other 21 keys (never `Options::exclude`),
///   and exit 3 passes only with `muhash` the sole uncovered key.
/// - I3: the log opens with its `H`, then genesis's `C`, its only `C` at height 0, checked on
///   every raw record.
///
/// Each side is read in two passes ([`Side`]), so the memory is the two projections' slots, not
/// the streams. Pass 2 must read pass 1's input ([`Picked`]), or the comparison is refused before
/// any check.
///
/// A failure panics with the first mismatch's height and key names only, never values.
pub(crate) fn linear_clean(run: &DigestRun, golden: &Path, from: i64, to: i64) -> Clean {
    compared_twice(run, golden, None, from..=to, |_| {}, |_| {}, || {})
}

/// [`linear_clean`] against a golden whose decompressed stream's sha256 is `plain_sha256` (pass 1
/// refuses any other before pass 2), `note` seeing the whole comparison report, values included,
/// before any check: T-TN's owner report (CH7a-5b C4) keeps what the value-free failure leaves
/// out. `each_left` sees each of the left's projected records in pass 2, in range or not, before
/// any check (C14's window scan).
pub(crate) fn linear_clean_noting(
    run: &DigestRun,
    golden: &Path,
    plain_sha256: &str,
    from: i64,
    to: i64,
    note: impl FnOnce(&Report),
    each_left: impl FnMut(&Outcome),
) -> Clean {
    compared_twice(run, golden, Some(plain_sha256), from..=to, note, each_left, || {})
}

/// [`linear_clean_noting`] over `range`, the pin optional, `between` run after both sides' first
/// passes.
fn compared_twice(
    run: &DigestRun,
    golden: &Path,
    plain_sha256: Option<&str>,
    range: std::ops::RangeInclusive<i64>,
    note: impl FnOnce(&Report),
    mut each_left: impl FnMut(&Outcome),
    between: impl FnOnce(),
) -> Clean {
    let (from, to) = range.into_inner();
    assert!(from <= to, "an empty range");
    let left = Side {
        name: "rdashd",
        path: run.path(),
    };
    let right = Side {
        name: "golden",
        path: golden,
    };
    let mut genesis = false;
    let (left_slots, left_seen) = left.project(|at, r| genesis_once(at, r, &mut genesis));
    assert!(genesis, "rdashd: no genesis C");
    let (right_slots, right_seen) = right.project(|_, _| {});
    if let Some(pin) = plain_sha256 {
        let got: String = right_seen.sha256.iter().map(|b| format!("{b:02x}")).collect();
        assert!(
            got == pin,
            "golden: {}: the stream is not the pinned one; the comparison is refused",
            golden.display()
        );
    }
    between();
    let muhash = c_s("muhash");
    let selected = run.selected();
    let require: Vec<FieldId> =
        ALL_C_S.iter().map(|k| c_s(k)).filter(|&f| selected.is_none() || f != muhash).collect();
    let opts = Options {
        from: Some(from),
        to: Some(to),
        require,
        ..Options::new(dash_statedigest::compare::Mode::Linear)
    };
    let mut left_has = BTreeSet::new();
    let mut scan = |o: &Outcome| {
        if (from..=to).contains(&o.id.h) && o.s().get("muhash").is_some() {
            left_has.insert(o.id.h);
        }
        each_left(o);
    };
    let (mut l, mut r) =
        (left.picked(left_slots, left_seen), right.picked(right_slots, right_seen));
    let scanned = l.by_ref().inspect(|item| {
        if let Ok((_, o)) = item {
            scan(o);
        }
    });
    let report = compare_linear(scanned, r.by_ref(), opts).expect("projected inputs");
    l.finish(scan);
    r.finish(|_| {});
    note(&report);
    let want_pairs = u64::try_from(to - from + 1).expect("a range");
    let summary = summary(&report);
    assert!(report.mismatches.is_empty(), "diverged: {summary}");
    assert!(report.skipped.is_empty(), "skipped heights: {summary}");
    assert!(!report.short_range, "short range: {summary}");
    assert_eq!(report.pairs, want_pairs, "compared pairs: {summary}");
    assert_eq!(report.range, Some((from, to)), "compared range: {summary}");
    for key in EVO_KEYS {
        let evo = report.coverage.get(&c_s(key)).copied().unwrap_or_default();
        assert_eq!(evo.equal, want_pairs, "{key} equal at every height: {summary}");
    }
    let counts = report.coverage.get(&muhash).copied().unwrap_or_default();
    match selected {
        None => assert_eq!(report.verdict, Verdict::Clean, "not clean: {summary}"),
        Some(selected) => {
            match report.verdict {
                Verdict::Clean => {}
                Verdict::Incomplete => {
                    assert_eq!(report.uncovered, [muhash], "uncovered: {summary}")
                }
                Verdict::Diverged => panic!("diverged: {summary}"),
            }
            let in_range: BTreeSet<i64> = selected
                .iter()
                .map(|&h| i64::from(h))
                .filter(|h| (from..=to).contains(h))
                .collect();
            assert_eq!(left_has, in_range, "rdashd's muhash heights");
            assert_eq!(counts.differ, 0, "muhash differs: {summary}");
            let n = u64::try_from(in_range.len()).expect("a count");
            assert_eq!(counts.equal, n, "muhash equal at the selected heights: {summary}");
        }
    }
    Clean {
        pairs: report.pairs,
        muhash_equal: counts.equal,
    }
}

/// A value-free account of `report`: the verdict, the pairs, the uncovered keys, and the first
/// mismatch's height and keys.
fn summary(report: &Report) -> String {
    use dash_statedigest::compare::What;
    let first = report.first().map(|m| match &m.what {
        What::Keys(diffs) => {
            let keys: Vec<String> = diffs.iter().map(|d| d.field.to_string()).collect();
            format!("first at {}: {}", m.h, keys.join(","))
        }
        What::Gap(side) => format!("first at {}: gap on {side:?}", m.h),
        What::Identity => format!("first at {}: identity", m.h),
    });
    let uncovered: Vec<String> = report.uncovered.iter().map(ToString::to_string).collect();
    format!(
        "{:?}, {} pairs, range {:?}, uncovered [{}], {}",
        report.verdict,
        report.pairs,
        report.range,
        uncovered.join(","),
        first.unwrap_or_else(|| "no mismatch".to_owned())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `feature_dip3_deterministicmns` node0's golden: 246 `C`s from genesis, each with `muhash`.
    fn golden() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../test-vectors/statedigest/regtest-v24/feature_dip3_deterministicmns/node0")
            .join("golden.jsonl.zst")
    }

    /// What a synthetic left side does with a `C`'s `muhash`.
    #[derive(Clone, Copy)]
    enum MuHash {
        Keep,
        Drop,
        /// The golden's value with its first hex digit changed.
        Change,
    }

    /// A left side in `dir`: the golden's `H` and `C` lines, each `C`'s `muhash` edited as `at`
    /// says for its height, every other key's bytes as the golden wrote them.
    fn left(dir: &Path, muhash: MuHashAt, at: impl Fn(i64) -> MuHash) -> DigestRun {
        let text = String::from_utf8(read(&golden())).expect("utf-8");
        let mut out = String::new();
        for line in text.lines() {
            if line.starts_with(r#"{"k":"H""#) {
                out.push_str(line);
            } else if line.starts_with(r#"{"k":"C""#) {
                let h: i64 = line[r#"{"k":"C","h":"#.len()..]
                    .split(',')
                    .next()
                    .and_then(|h| h.parse().ok())
                    .expect("a height");
                let key = r#","muhash":""#;
                let start = line.find(key).expect("muhash");
                let end = start + key.len() + 64 + 1;
                match at(h) {
                    MuHash::Keep => out.push_str(line),
                    MuHash::Drop => {
                        out.push_str(&line[..start]);
                        out.push_str(&line[end..]);
                    }
                    MuHash::Change => {
                        let first = start + key.len();
                        let digit = if &line[first..=first] == "0" {
                            "1"
                        } else {
                            "0"
                        };
                        out.push_str(&line[..first]);
                        out.push_str(digit);
                        out.push_str(&line[first + 1..]);
                    }
                }
            } else {
                continue;
            }
            out.push('\n');
        }
        let path = dir.join("log.jsonl");
        std::fs::write(&path, out).expect("write");
        DigestRun {
            path,
            muhash,
        }
    }

    /// I3 (Sol r1 #2): a fresh `new_with_digest`'s log is its `H` and exactly one `C`, genesis's,
    /// before any submit; the first load took the sink. A clean restart, not re-armed, writes
    /// nothing more and leaves the slot empty.
    #[test]
    fn a_pending_sink_records_genesis_exactly_once() {
        use super::super::{Teardown, try_sut_with_digest, ui};

        let dir = tempfile::tempdir().unwrap();
        let logs = tempfile::tempdir().unwrap();
        let (run, sink) = DigestRun::new(logs.path(), MuHashAt::Every);
        let mut sut = try_sut_with_digest(dir.path(), &[], ui(), sink).expect("accepted");
        assert!(!sut.digest_pending(), "the first load took the sink");
        let genesis = dashd_chainstate::chain::genesis_block(&sut.params.chain).hash();
        let records = run.records();
        assert!(
            matches!(records.as_slice(), [Record::Header(_), Record::Outcome(_)]),
            "{records:?}"
        );
        let Record::Outcome(c) = &records[1] else {
            unreachable!()
        };
        assert_eq!((c.kind(), c.id.h), (Kind::Connect, 0));
        assert_eq!(c.id.b.to_string(), genesis.to_string());
        let before = std::fs::read(run.path()).unwrap();
        sut.restart(Teardown::Clean);
        assert!(!sut.digest_pending(), "a restart does not re-arm");
        assert_eq!(std::fs::read(run.path()).unwrap(), before, "nothing written after the restart");
    }

    fn only(heights: &[u32]) -> MuHashAt {
        MuHashAt::Only(heights.iter().copied().collect())
    }

    /// `f`'s panic message, which it must raise.
    fn panics(f: impl FnOnce() -> Clean) -> String {
        let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("a failure");
        err.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    /// I4 under `MuHashAt::Every`: the golden against its own lines is clean, every pair
    /// compared, `muhash` equal at every height.
    #[test]
    fn every_key_present_and_equal_passes() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        let clean = linear_clean(&run, &golden(), 0, 245);
        assert_eq!((clean.pairs, clean.muhash_equal), (246, 246));
    }

    /// I4 (M18): a `muhash` that differs at a selected height is a divergence there, named by
    /// key: the comparison covers `muhash`, it does not exclude it.
    #[test]
    fn muhash_changed_at_a_selected_height_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), only(&[5, 10]), |h| match h {
            5 => MuHash::Keep,
            10 => MuHash::Change,
            _ => MuHash::Drop,
        });
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.contains("diverged"), "{message}");
        assert!(message.contains("first at 10: C.s.muhash"), "{message}");
    }

    /// I4: a selected height whose `muhash` the left side lacks fails, though the golden has it.
    #[test]
    fn muhash_missing_at_a_selected_height_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), only(&[5, 10]), |h| match h {
            5 => MuHash::Keep,
            _ => MuHash::Drop,
        });
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.contains("rdashd's muhash heights"), "{message}");
    }

    /// I4: `muhash` absent on the left at every unselected height, present at the golden's, passes
    /// with exit 3 (`muhash` the sole uncovered key) and both selected heights compared equal.
    #[test]
    fn muhash_absent_at_an_unselected_height_passes() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), only(&[5, 10]), |h| match h {
            5 | 10 => MuHash::Keep,
            _ => MuHash::Drop,
        });
        let clean = linear_clean(&run, &golden(), 0, 245);
        assert_eq!((clean.pairs, clean.muhash_equal), (246, 2));
    }

    /// `path`'s lines rewritten in place, in the same file, by `edit`.
    fn rewrite(path: &Path, edit: impl FnOnce(&mut Vec<String>)) {
        let text = std::fs::read_to_string(path).unwrap();
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        edit(&mut lines);
        let out: String = lines.iter().map(|line| format!("{line}\n")).collect();
        std::fs::write(path, out).unwrap();
    }

    /// `line` with the character after `key` changed, to `0` or else to `1`: the same length and
    /// still a canonical line.
    fn flipped(line: &str, key: &str) -> String {
        let at = line.find(key).expect("the key") + key.len();
        let to = if &line[at..=at] == "0" {
            "1"
        } else {
            "0"
        };
        format!("{}{to}{}", &line[..at], &line[at + 1..])
    }

    /// A plain right side in `dir`: the golden's `H` and `C` lines as [`left`] writes them,
    /// edited by `edit`, its bytes the test's.
    fn plain_golden(dir: &Path, edit: impl FnOnce(&mut Vec<String>)) -> PathBuf {
        let path = left(dir, MuHashAt::Every, |_| MuHash::Keep).path().to_owned();
        rewrite(&path, edit);
        path
    }

    /// I3: a log with a second genesis `C` fails at that record, before it is projected: in the
    /// same run (H, C0, C0, …), which the projection would refuse as an overlap anyway, and in a
    /// second run (H, C0, H, C0, …), which it accepts as a rewind to genesis.
    #[test]
    fn a_second_genesis_c_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        let original = std::fs::read(run.path()).unwrap();
        rewrite(run.path(), |lines| lines.insert(2, lines[1].clone()));
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.contains("rdashd: a second genesis C, at record 2"), "{message}");

        std::fs::write(run.path(), &original).unwrap();
        rewrite(run.path(), |lines| {
            let (h, c0) = (lines[0].clone(), lines[1].clone());
            lines.splice(2..2, [h, c0]);
        });
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.contains("rdashd: a second genesis C, at record 3"), "{message}");
        let projection = projected("rdashd", &read_records(run.path()));
        assert_eq!(projection.len(), 246, "the projection keeps one genesis");
    }

    /// Pass 2 must read pass 1's input. Each case rewrites a side in place between the passes and
    /// is refused, before any check, for the reason it breaks; unchanged, the same comparison
    /// passes.
    #[test]
    fn a_side_changed_between_its_passes_is_refused() {
        let refused = |message: String, side: &str, why: &str| {
            assert!(message.starts_with(side), "{message}");
            assert!(message.contains("the input changed between the two passes"), "{message}");
            assert!(message.contains(why), "{why}: {message}");
        };
        let dir = tempfile::tempdir().unwrap();
        let fresh = || left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        let compare = |run: &DigestRun, golden: &Path, between: &dyn Fn()| {
            panics(|| compared_twice(run, golden, None, 0..=245, |_| {}, |_| {}, between))
        };
        let golden = golden();

        // A reorg appended: a new run whose first connect replaces the tip with another block.
        // Every pick still parses and is its slot's record; only the count and the hash differ.
        let run = fresh();
        let reorg = || {
            rewrite(run.path(), |lines| {
                let tip = flipped(lines.last().unwrap(), r#""b":""#);
                lines.extend([lines[0].clone(), tip]);
            })
        };
        refused(compare(&run, &golden, &reorg), "rdashd: ", "249 records, pass 1 read 247");

        // An unpicked record, the H, edited, its length kept.
        let run = fresh();
        let header = || rewrite(run.path(), |lines| lines[0] = flipped(&lines[0], r#""run":""#));
        refused(compare(&run, &golden, &header), "rdashd: ", "the stream's sha256");

        // A picked record's value edited, its length kept.
        let run = fresh();
        let value =
            || rewrite(run.path(), |lines| lines[21] = flipped(&lines[21], r#""muhash":""#));
        refused(compare(&run, &golden, &value), "rdashd: ", "the stream's sha256");

        // A picked record's block edited, its length kept: not its slot's identity.
        let run = fresh();
        let block = || rewrite(run.path(), |lines| lines[21] = flipped(&lines[21], r#""b":""#));
        refused(compare(&run, &golden, &block), "rdashd: ", "record 21 is not height 20's pick");

        // The last record dropped: a pick is gone.
        let run = fresh();
        let dropped = || {
            rewrite(run.path(), |lines| {
                lines.pop();
            })
        };
        refused(compare(&run, &golden, &dropped), "rdashd: ", "record 246, height 245, is gone");

        // The file cut inside its last line: that line no longer parses.
        let run = fresh();
        let cut = || {
            let bytes = std::fs::read(run.path()).unwrap();
            std::fs::write(run.path(), &bytes[..bytes.len() - 10]).unwrap();
        };
        refused(compare(&run, &golden, &cut), "rdashd: ", "line 247 does not parse");

        // The golden side's value edited, its length kept.
        let run = fresh();
        let right = tempfile::tempdir().unwrap();
        let plain = plain_golden(right.path(), |_| {});
        let edit = || rewrite(&plain, |lines| lines[21] = flipped(&lines[21], r#""muhash":""#));
        refused(compare(&run, &plain, &edit), "golden: ", "the stream's sha256");

        let plain = plain_golden(right.path(), |_| {});
        let clean = compared_twice(&run, &plain, None, 0..=245, |_| {}, |_| {}, || {});
        assert_eq!((clean.pairs, clean.muhash_equal), (246, 246));
    }

    /// `linear_clean_noting`'s pin is the golden's decompressed stream's sha256: the zstd file's
    /// own sha256 is refused, the stream's passes.
    #[test]
    fn a_golden_off_its_pin_is_refused() {
        let hex =
            |bytes: [u8; 32]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        let file = hex(dash_sha256::sha256(&std::fs::read(golden()).unwrap()));
        let message =
            panics(|| linear_clean_noting(&run, &golden(), &file, 0, 245, |_| {}, |_| {}));
        assert!(message.contains("the stream is not the pinned one"), "{message}");
        let stream = hex(dash_sha256::sha256(&read(&golden())));
        let clean = linear_clean_noting(&run, &golden(), &stream, 0, 245, |_| {}, |_| {});
        assert_eq!(clean.pairs, 246);
    }

    /// `lines` with a new run, the `H` again, starting at height 100: the record of 100 dropped,
    /// so the projection has a hole there (spec §1's roll-forward).
    fn hole_at_100(lines: &mut [String]) {
        let h = lines[0].clone();
        lines[101] = h;
    }

    /// I4: a hole both sides share is skipped heights, not a pass.
    #[test]
    fn a_hole_both_sides_share_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        rewrite(run.path(), |lines| hole_at_100(lines));
        let right = tempfile::tempdir().unwrap();
        let plain = plain_golden(right.path(), |lines| hole_at_100(lines));
        let message = panics(|| linear_clean(&run, &plain, 0, 245));
        assert!(message.starts_with("skipped heights: "), "{message}");
    }

    /// I4: a height one side lacks is a gap there.
    #[test]
    fn a_one_sided_gap_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        rewrite(run.path(), |lines| hole_at_100(lines));
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.starts_with("diverged: "), "{message}");
        assert!(message.contains("first at 100: gap on "), "{message}");
    }

    /// I4: both sides ending below `to` is a short range.
    #[test]
    fn a_short_tail_fails() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        let message = panics(|| linear_clean(&run, &golden(), 0, 250));
        assert!(message.starts_with("short range: "), "{message}");
    }

    /// Records out of order in one run: the projection refuses the first one above the tip.
    #[test]
    fn records_out_of_order_fail() {
        let dir = tempfile::tempdir().unwrap();
        let run = left(dir.path(), MuHashAt::Every, |_| MuHash::Keep);
        rewrite(run.path(), |lines| lines.swap(11, 12));
        let message = panics(|| linear_clean(&run, &golden(), 0, 245));
        assert!(message.starts_with("rdashd: record 11: a tip connect at height 11"), "{message}");
    }
}
