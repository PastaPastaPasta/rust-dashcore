//! Recorded decisions as a replay source (spec §7.2, §7.3, §10.5; node program task O9a, for
//! CH7b's `ReplayChainHelper`).
//!
//! [`RecordedDecisions::load`] joins a node's `decisions.jsonl` with its `digest.jsonl`: an `A`
//! belongs to the `C` or `R` with the same `seq` in the run of the same `run` string (§7.3), and
//! the two must name the same block and origin. Every `A` without its attempt, and every `C` or
//! `R` without its `A` in a run the decisions file has, is an orphan, and the load fails listing
//! them (§10.5).
//!
//! A replay looks attempts up by `(b, org, n)`, where `n` counts the earlier attempts with the
//! same `(b, org)` in the recording ([`ReplaySession::begin`]), or takes the attempt that made a
//! block's last tip connect ([`RecordedDecisions::final_connect`]): a replay of the final chain
//! connects each block once, while Core may have tried it several times. Within an attempt the
//! queries are answered by an [`AttemptCursor`] under a [`Policy`]. A query the attempt never
//! recorded is a [`Miss`]; the cursor never guesses.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use core::fmt;

use crate::fields::{Class, Kind};
use crate::hex::Hash256;
use crate::parse::Lines;
use crate::record::{Ident, Origin, Record};
use crate::typed::{BestChainLock, IsConflictPair, Query, SbPayment};
use crate::value::Value;

/// A chain-helper query without its answer: what a replay asks (spec §7.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    ClConflict {
        height: i64,
        hash: Hash256,
    },
    ClBest,
    Ibd,
    ClHas {
        height: i64,
        hash: Hash256,
    },
    IsConflict {
        txid: Hash256,
    },
    SbRequired {
        height: i64,
    },
    GovValid,
    SbTriggered {
        height: i64,
    },
    SbPayments {
        height: i64,
    },
}

/// A recorded answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `cl_conflict`, `ibd`, `cl_has`, `sb_required`, `gov_valid`, `sb_triggered`.
    Bool(bool),
    /// `cl_best`.
    BestChainLock(Option<BestChainLock>),
    /// `is_conflict`.
    IsConflict(Option<IsConflictPair>),
    /// `sb_payments`.
    SbPayments(Option<Vec<SbPayment>>),
}

/// Splits a recorded query into what was asked and what Core answered.
pub fn split(query: &Query) -> (Ask, Answer) {
    match query {
        Query::ClConflict {
            height,
            hash,
            answer,
        } => (
            Ask::ClConflict {
                height: *height,
                hash: *hash,
            },
            Answer::Bool(*answer),
        ),
        Query::ClBest(best) => (Ask::ClBest, Answer::BestChainLock(best.clone())),
        Query::Ibd(answer) => (Ask::Ibd, Answer::Bool(*answer)),
        Query::ClHas {
            height,
            hash,
            answer,
        } => (
            Ask::ClHas {
                height: *height,
                hash: *hash,
            },
            Answer::Bool(*answer),
        ),
        Query::IsConflict {
            txid,
            conflict,
        } => (
            Ask::IsConflict {
                txid: *txid,
            },
            Answer::IsConflict(conflict.clone()),
        ),
        Query::SbRequired {
            height,
            answer,
        } => (
            Ask::SbRequired {
                height: *height,
            },
            Answer::Bool(*answer),
        ),
        Query::GovValid(answer) => (Ask::GovValid, Answer::Bool(*answer)),
        Query::SbTriggered {
            height,
            answer,
        } => (
            Ask::SbTriggered {
                height: *height,
            },
            Answer::Bool(*answer),
        ),
        Query::SbPayments {
            height,
            payments,
        } => (
            Ask::SbPayments {
                height: *height,
            },
            Answer::SbPayments(payments.clone()),
        ),
    }
}

/// One recorded attempt: its `A` record, joined with its digest record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedAttempt {
    /// The run, counting the digest's `H` records from 1.
    pub run: usize,
    /// `x.seq`.
    pub seq: i64,
    pub id: Ident,
    /// `GetAdjustedTime()` when the attempt opened (the replay clock).
    pub t: i64,
    /// The queries in call order.
    pub queries: Vec<Query>,
    /// The attempt produced a `C` (else an `R`).
    pub connected: bool,
    /// The `A` record's line in the decisions file.
    pub line: usize,
}

/// The lookup key of spec §7.2: the block, the origin and how many earlier attempts with the
/// same `(b, org)` the recording holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AttemptKey {
    pub b: Hash256,
    pub org: Origin,
    pub n: u32,
}

impl fmt::Display for AttemptKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "attempt {} {} #{}", self.b, self.org.as_str(), self.n)
    }
}

/// A record one file has and the other lacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Orphan {
    /// `"decisions"` for an `A` without its attempt, `"digest"` for an attempt without its `A`.
    pub file: &'static str,
    pub line: usize,
    pub run: Vec<u8>,
    pub seq: Option<i64>,
    pub id: Ident,
}

/// Why the two files do not load as one recording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// A line does not parse, a file holds a record kind it must not, a record comes before the
    /// first `H`, or a record lacks `seq`.
    Malformed {
        file: &'static str,
        line: usize,
        why: String,
    },
    /// Two records of one run share a `seq`.
    DuplicateSeq {
        file: &'static str,
        line: usize,
        seq: i64,
    },
    /// An `A` and its attempt name different blocks or origins.
    Mismatched {
        line: usize,
        decisions: Ident,
        digest: Ident,
    },
    /// Records without their partner (spec §10.5), in file order.
    Orphans(Vec<Orphan>),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Malformed {
                file,
                line,
                why,
            } => write!(f, "{file}:{line}: {why}"),
            LoadError::DuplicateSeq {
                file,
                line,
                seq,
            } => write!(f, "{file}:{line}: seq {seq} appears twice in its run"),
            LoadError::Mismatched {
                line,
                decisions,
                digest,
            } => write!(
                f,
                "decisions:{line}: the A names {} at {} ({}), its attempt {} at {} ({})",
                decisions.b,
                decisions.h,
                decisions.org.as_str(),
                digest.b,
                digest.h,
                digest.org.as_str()
            ),
            LoadError::Orphans(orphans) => {
                write!(f, "{} orphan records", orphans.len())?;
                for o in orphans.iter().take(10) {
                    write!(f, "; {}:{} seq {:?} {} at {}", o.file, o.line, o.seq, o.id.b, o.id.h)?;
                }
                Ok(())
            }
        }
    }
}

impl core::error::Error for LoadError {}

/// The attempts of one run of one file: `seq` → (line, identity, payload).
struct RunRecords<T> {
    run: Vec<u8>,
    by_seq: BTreeMap<i64, (usize, Ident, T)>,
    order: Vec<i64>,
    /// Every `seq` of the run, kept records or not (a `D` shares the numbering).
    seqs: BTreeSet<i64>,
}

impl<T> RunRecords<T> {
    /// The record of `seq` as an orphan of `file`.
    fn orphan(&self, file: &'static str, seq: i64) -> Orphan {
        let (line, id, _) = &self.by_seq[&seq];
        Orphan {
            file,
            line: *line,
            run: self.run.clone(),
            seq: Some(seq),
            id: id.clone(),
        }
    }
}

fn seq_of(record: &Record) -> Option<i64> {
    record.fields(Class::Schedule)?.get("seq").and_then(Value::as_int)
}

fn runs<T>(
    file: &'static str,
    bytes: &[u8],
    mut pick: impl FnMut(Record) -> Result<Option<(Ident, T)>, String>,
) -> Result<Vec<RunRecords<T>>, LoadError> {
    let mut runs: Vec<RunRecords<T>> = Vec::new();
    for (line, parsed) in Lines::new(bytes) {
        let malformed = |why: String| LoadError::Malformed {
            file,
            line,
            why,
        };
        let record = parsed.map_err(|e| malformed(e.to_string()))?;
        if let Record::Header(h) = &record {
            runs.push(RunRecords {
                run: h.run.clone(),
                by_seq: BTreeMap::new(),
                order: Vec::new(),
                seqs: BTreeSet::new(),
            });
            continue;
        }
        let seq = seq_of(&record);
        let Some(current) = runs.last_mut() else {
            return Err(malformed("a record before the first H".into()));
        };
        if let Some(seq) = seq
            && !current.seqs.insert(seq)
        {
            return Err(LoadError::DuplicateSeq {
                file,
                line,
                seq,
            });
        }
        let Some((id, payload)) = pick(record).map_err(malformed)? else {
            continue;
        };
        let seq = seq.ok_or_else(|| malformed("a record without x.seq".into()))?;
        current.by_seq.insert(seq, (line, id, payload));
        current.order.push(seq);
    }
    Ok(runs)
}

/// A node's decisions, joined with its digest and indexed for replay.
#[derive(Clone, Debug, Default)]
pub struct RecordedDecisions {
    /// In decisions-file order.
    attempts: Vec<RecordedAttempt>,
    /// `(b, org)` → indexes into `attempts`, in file order: `n` is the position.
    by_block: BTreeMap<(Hash256, Origin), Vec<usize>>,
    /// `b` → the `n` of the attempt behind the digest's last tip `C` of `b` (§9.2), or `None`
    /// when that `C` is in a run without decisions.
    final_tip: BTreeMap<Hash256, Option<u32>>,
}

impl RecordedDecisions {
    /// Joins `decisions.jsonl` with `digest.jsonl` (the bytes of each). Runs are matched by their
    /// `run` string; a digest run the decisions file does not have (only `-statedigestlog` was
    /// set) has no decisions and is not an orphan.
    pub fn load(digest: &[u8], decisions: &[u8]) -> Result<RecordedDecisions, LoadError> {
        let digest_runs = runs("digest", digest, |record| match record {
            Record::Outcome(o) => Ok(match o.kind() {
                Kind::Connect => Some((o.id, true)),
                Kind::Reject => Some((o.id, false)),
                _ => None,
            }),
            other => Err(format!("a {:?} record in a digest", other.kind())),
        })?;
        let decision_runs = runs("decisions", decisions, |record| match record {
            Record::Attempt(a) => Ok(Some((a.id, (a.t, a.q)))),
            other => Err(format!("a {:?} record in a decisions file", other.kind())),
        })?;

        let mut out = RecordedDecisions::default();
        // (run, seq) → n, for the joined attempts.
        let mut numbered: BTreeMap<(usize, i64), u32> = BTreeMap::new();
        let mut orphans = Vec::new();
        let mut matched_digest_runs = vec![false; digest_runs.len()];
        for dr in &decision_runs {
            let found = digest_runs
                .iter()
                .enumerate()
                .find(|(i, r)| r.run == dr.run && !matched_digest_runs[*i]);
            if let Some((i, _)) = found {
                matched_digest_runs[i] = true;
            }
            for seq in &dr.order {
                let (line, id, (t, queries)) = &dr.by_seq[seq];
                let Some((run_index, (_, digest_id, connected))) =
                    found.and_then(|(i, attempts)| Some((i, attempts.by_seq.get(seq)?)))
                else {
                    orphans.push(dr.orphan("decisions", *seq));
                    continue;
                };
                if digest_id != id {
                    return Err(LoadError::Mismatched {
                        line: *line,
                        decisions: id.clone(),
                        digest: digest_id.clone(),
                    });
                }
                let n = out.push(RecordedAttempt {
                    run: run_index + 1,
                    seq: *seq,
                    id: id.clone(),
                    t: *t,
                    queries: queries.clone(),
                    connected: *connected,
                    line: *line,
                });
                numbered.insert((run_index + 1, *seq), n);
            }
            if let Some((_, attempts)) = found {
                let unmatched = attempts.order.iter().filter(|seq| !dr.by_seq.contains_key(seq));
                orphans.extend(unmatched.map(|seq| attempts.orphan("digest", *seq)));
            }
        }
        if !orphans.is_empty() {
            orphans.sort_by_key(|o| (o.file, o.line));
            return Err(LoadError::Orphans(orphans));
        }
        // The golden's C of a block is the digest's last tip C of it, over every run (§9.2),
        // joined or not.
        for (i, run) in digest_runs.iter().enumerate() {
            for seq in &run.order {
                let (_, id, connected) = &run.by_seq[seq];
                if *connected && id.org == Origin::Tip {
                    out.final_tip.insert(id.b, numbered.get(&(i + 1, *seq)).copied());
                }
            }
        }
        Ok(out)
    }

    /// Adds an attempt and returns its `n`.
    fn push(&mut self, attempt: RecordedAttempt) -> u32 {
        let list = self.by_block.entry((attempt.id.b, attempt.id.org)).or_default();
        let n = list.len() as u32;
        list.push(self.attempts.len());
        self.attempts.push(attempt);
        n
    }

    /// Every attempt, in decisions-file order.
    pub fn attempts(&self) -> &[RecordedAttempt] {
        &self.attempts
    }

    /// The attempt of a key.
    pub fn get(&self, key: &AttemptKey) -> Option<&RecordedAttempt> {
        let list = self.by_block.get(&(key.b, key.org))?;
        list.get(key.n as usize).map(|&i| &self.attempts[i])
    }

    /// The attempt behind the digest's last tip `C` of `b`, over every run: the `C` the golden
    /// merges (§9.2) when `b` is on the final chain. A replay of the final chain uses it, since
    /// Core may have tried the block more than once (a reorg, a reindex). `None` when `b` has no
    /// tip `C`, or when that `C` is in a run that recorded no decisions. `n` counts the joined
    /// attempts only.
    pub fn final_connect(&self, b: &Hash256) -> Option<AttemptKey> {
        self.final_tip.get(b).copied().flatten().map(|n| AttemptKey {
            b: *b,
            org: Origin::Tip,
            n,
        })
    }

    /// Whether the digest has a tip `C` of `b` in any run, joined with decisions or not. Without
    /// one, `b` was never connected at the tip; with one, [`Self::final_connect`] answers `None`
    /// only when the last such `C` is in a run without decisions.
    pub fn has_tip_connect(&self, b: &Hash256) -> bool {
        self.final_tip.contains_key(b)
    }

    /// A replay that looks attempts up in the order it makes them.
    pub fn session(&self, policy: Policy) -> ReplaySession<'_> {
        ReplaySession {
            decisions: self,
            policy,
            seen: BTreeMap::new(),
        }
    }
}

/// How an attempt's queries are matched (spec §7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// A query returns the recorded answer for its `(name, args)`; a key recorded more than once
    /// returns its answers in order. The default.
    Keyed,
    /// The replay's queries must be the recorded ones, in order.
    Strict,
}

/// Why a replay's lookup failed. It never guesses an answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    /// The recording has no such attempt.
    NoAttempt(AttemptKey),
    /// The attempt never recorded this query (keyed), or recorded it fewer times with
    /// different answers.
    NotRecorded {
        key: AttemptKey,
        ask: Ask,
    },
    /// Strict: the query is not the next recorded one (`expected` is `None` past the end).
    OutOfOrder {
        key: AttemptKey,
        index: usize,
        expected: Option<Box<Query>>,
        asked: Ask,
    },
    /// Strict: the attempt ended with recorded queries left unasked.
    Unasked {
        key: AttemptKey,
        remaining: usize,
    },
}

impl fmt::Display for Miss {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Miss::NoAttempt(k) => write!(f, "{k}: not in the recording"),
            Miss::NotRecorded {
                key,
                ask,
            } => write!(f, "{key}: {ask:?} was not recorded"),
            Miss::OutOfOrder {
                key,
                index,
                expected,
                asked,
            } => write!(f, "{key}: query {index} is {asked:?}, the recording has {expected:?}"),
            Miss::Unasked {
                key,
                remaining,
            } => write!(f, "{key}: {remaining} recorded queries were not asked"),
        }
    }
}

impl core::error::Error for Miss {}

/// A replay over one recording: it numbers the attempts per `(b, org)` as it begins them.
#[derive(Debug)]
pub struct ReplaySession<'a> {
    decisions: &'a RecordedDecisions,
    policy: Policy,
    seen: BTreeMap<(Hash256, Origin), u32>,
}

impl<'a> ReplaySession<'a> {
    /// The next attempt for `(b, org)`: the first call gets `n = 0`, the next `n = 1`, and so on.
    pub fn begin(&mut self, b: &Hash256, org: Origin) -> Result<AttemptCursor<'a>, Miss> {
        let n = self.seen.entry((*b, org)).or_insert(0);
        let key = AttemptKey {
            b: *b,
            org,
            n: *n,
        };
        *n += 1;
        self.begin_at(key)
    }

    /// The attempt of `key`, outside the numbering (e.g. [`RecordedDecisions::final_connect`]).
    pub fn begin_at(&self, key: AttemptKey) -> Result<AttemptCursor<'a>, Miss> {
        let attempt = self.decisions.get(&key).ok_or_else(|| Miss::NoAttempt(key.clone()))?;
        Ok(AttemptCursor {
            entries: attempt.queries.iter().map(split).collect(),
            attempt: Cow::Borrowed(attempt),
            key,
            policy: self.policy,
            used: vec![false; attempt.queries.len()],
            next: 0,
        })
    }
}

/// The queries of one attempt, answered as a replay asks them.
#[derive(Debug)]
pub struct AttemptCursor<'a> {
    attempt: Cow<'a, RecordedAttempt>,
    key: AttemptKey,
    policy: Policy,
    entries: Vec<(Ask, Answer)>,
    used: Vec<bool>,
    next: usize,
}

impl AttemptCursor<'_> {
    /// The attempt.
    pub fn attempt(&self) -> &RecordedAttempt {
        &self.attempt
    }

    /// The cursor with its own copy of the attempt, as it stands: for a replay that keeps it
    /// beside the recording rather than borrowing it (CH3a's `RecordedHelper`).
    pub fn into_owned(self) -> AttemptCursor<'static> {
        AttemptCursor {
            attempt: Cow::Owned(self.attempt.into_owned()),
            key: self.key,
            policy: self.policy,
            entries: self.entries,
            used: self.used,
            next: self.next,
        }
    }

    /// The replay clock for this attempt (`A.t`).
    pub fn adjusted_time(&self) -> i64 {
        self.attempt.t
    }

    /// The recorded answer to `ask`.
    ///
    /// Keyed: the first unused entry with this `(name, args)`. Once all of them are used, a
    /// repeated query gets the same answer again if every entry for it had the same answer
    /// (a pure query asked once more), and a [`Miss`] otherwise.
    pub fn ask(&mut self, ask: &Ask) -> Result<Answer, Miss> {
        match self.policy {
            Policy::Strict => {
                let index = self.next;
                match self.entries.get(index) {
                    Some((recorded, answer)) if recorded == ask => {
                        self.used[index] = true;
                        self.next += 1;
                        Ok(answer.clone())
                    }
                    _ => Err(Miss::OutOfOrder {
                        key: self.key.clone(),
                        index,
                        expected: self.attempt.queries.get(index).cloned().map(Box::new),
                        asked: ask.clone(),
                    }),
                }
            }
            Policy::Keyed => {
                let mut same = self.entries.iter().enumerate().filter(|(_, (a, _))| a == ask);
                if let Some((index, (_, answer))) = same.clone().find(|(i, _)| !self.used[*i]) {
                    self.used[index] = true;
                    return Ok(answer.clone());
                }
                let first = same.next().map(|(_, (_, answer))| answer);
                match first {
                    Some(answer) if same.all(|(_, (_, other))| other == answer) => {
                        Ok(answer.clone())
                    }
                    _ => Err(Miss::NotRecorded {
                        key: self.key.clone(),
                        ask: ask.clone(),
                    }),
                }
            }
        }
    }

    /// Ends the attempt. Strict: every recorded query must have been asked. Keyed: returns the
    /// recorded queries nobody asked (a replay may cache a pure query), for the report.
    pub fn finish(self) -> Result<Vec<Query>, Miss> {
        let unused: Vec<Query> = self
            .attempt
            .queries
            .iter()
            .zip(&self.used)
            .filter(|(_, used)| !**used)
            .map(|(q, _)| q.clone())
            .collect();
        if self.policy == Policy::Strict && !unused.is_empty() {
            return Err(Miss::Unasked {
                key: self.key,
                remaining: unused.len(),
            });
        }
        Ok(unused)
    }
}
