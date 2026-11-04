//! CH7a-5b C3: T-RD's state-hash mode. A [`DigestRun`] is a `DigestLog` the SUT's connector
//! writes to a file from genesis (X5's pending sink, [`NodeSut::new_with_digest`]); and
//! [`linear_clean`] compares it with a golden in linear mode under I4's contract.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use dash_consensus::coins::stats::CoinStatsAccumulator;
use dash_consensus::primitives::BlockHash;
use dash_statedigest::compare::{FieldId, Options, Projector, Report, Verdict, compare_linear};
use dash_statedigest::{Chain, Class, FileKind, Header, Kind, Lines, Outcome, Record, Writer};
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

    fn open(dir: &Path, muhash: MuHashAt, chain: Chain) -> (DigestRun, DigestLog<File>) {
        let path = dir.join("log.jsonl");
        let file = File::create(&path).expect("the log file");
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

/// I4: `run`'s log (left) against `golden` (right) in linear mode over `from..=to`.
/// - Every key but `muhash` is required at every height in range, present on both sides and
///   equal, with no gap and no skipped height: exactly `to − from + 1` pairs.
/// - Under `MuHashAt::Every`, `muhash` is required too, so only exit 0 passes.
/// - Under `MuHashAt::Only(selected)`, `muhash` is present on both sides and equal at every
///   selected height in range, and absent on the left at every other height (the golden's
///   presence there is ignored). `require` names the other 21 keys (never `Options::exclude`),
///   and exit 3 passes only with `muhash` the sole uncovered key.
///
/// A failure panics with the first mismatch's height and key names only, never values.
pub(crate) fn linear_clean(run: &DigestRun, golden: &Path, from: i64, to: i64) -> Clean {
    linear_clean_noting(run, golden, from, to, |_| {})
}

/// [`linear_clean`], `note` seeing the whole comparison report, values included, before any
/// check: T-TN's owner report (CH7a-5b C4) keeps what the value-free failure leaves out.
pub(crate) fn linear_clean_noting(
    run: &DigestRun,
    golden: &Path,
    from: i64,
    to: i64,
    note: impl FnOnce(&Report),
) -> Clean {
    assert!(from <= to, "an empty range");
    let left = projected("rdashd", &run.records());
    let right = projected("golden", &read_records(golden));
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
    let ok = |o: (usize, Outcome)| Ok::<_, std::convert::Infallible>(o);
    let report = compare_linear(left.iter().cloned().map(ok), right.into_iter().map(ok), opts)
        .expect("projected inputs");
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
            let left_has: BTreeSet<i64> = left
                .iter()
                .filter(|(_, o)| (from..=to).contains(&o.id.h) && o.s().get("muhash").is_some())
                .map(|(_, o)| o.id.h)
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
}
