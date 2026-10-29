//! The comparator (spec §10; node program task O9a): the linear projection, the field-class
//! rules, per-key coverage and the first mismatch.
//!
//! * **Classes** (§10.1) come from [`FIELDS`], never from key names: `s` is compared in both
//!   modes, `o` in [`Mode::Stream`] only, `x` never. The identity (`h`, `b`, `org`) is compared
//!   in both modes and the kind in stream mode.
//! * **Linear mode** (§10.2) compares the two inputs' final active chains. A [`Projector`] finds
//!   that chain in one pass over a file, keeping only `(height, hash, record index)` per block;
//!   the caller then streams the file again and hands [`compare_linear`] the records the
//!   [`Projection`] picked. Their record indexes rise with their heights (a block is pushed onto
//!   the tip stack after its parent), so the second pass is one forward scan.
//! * **Stream mode** (§10.2) compares the `C`, `R` and `D` records of the selected origins
//!   record for record ([`compare_stream`]).
//! * **Coverage** (§10.3): a key that is absent on either side is counted, never passed: any
//!   uncovered key makes the run [`Verdict::Incomplete`]. With [`Options::require`], an absence of
//!   a required key is a mismatch. Heights a linear input skips ([`Skipped`], a projection hole)
//!   are uncovered too: missing on one side is also a gap mismatch, and missing on both still
//!   leaves the run incomplete.
//!
//! [`Options::exclude`] drops keys from the comparison and from coverage altogether (the golden
//! keys a caller declares, O2b D16); excluding a required key panics in [`Comparator::new`].
//! [`Report::excluded`] still counts, per side, the compared pairs whose record holds each
//! excluded key, so that a caller can name what it left out (CH3a D167).
//! [`compare_attempts`] compares two `A` (attempt) streams in strict sequence (O2b §4.3): identity,
//! `t` and the queries, never the schedule-class `x`.
//!
//! The module is pure: the caller reads the files and hands in records.

use std::collections::BTreeMap;

use core::fmt;

use crate::fields::{Class, FIELDS, FieldSpec, Kind};
use crate::hex::Hash256;
use crate::record::{Attempt, Ident, Origin, Outcome, Record};
use crate::value::Value;

/// The comparison mode (spec §10.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Both inputs projected onto their final active chains; `s` compared.
    Linear,
    /// Record for record over the selected origins; `s` and `o` compared.
    Stream,
}

impl Mode {
    /// The classes this mode compares.
    pub fn classes(self) -> &'static [Class] {
        match self {
            Mode::Linear => &[Class::Semantic],
            Mode::Stream => &[Class::Semantic, Class::Observation],
        }
    }
}

/// One key of the field table: an index into [`FIELDS`]. Ordered as the spec lists the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FieldId(usize);

impl FieldId {
    /// The key `key` of the `(kind, class)` sub-object.
    pub fn find(kind: Kind, class: Class, key: &str) -> Option<FieldId> {
        FIELDS.iter().position(|f| f.kind == kind && f.class == class && f.key == key).map(FieldId)
    }

    /// Every compared key a name stands for: `C.s.prev` names one key; a bare key (`res`, `fx`)
    /// names it in every outcome kind that defines it. `x` keys and the `A` record's are never
    /// compared, so they resolve to nothing.
    pub fn resolve(name: &str) -> Vec<FieldId> {
        let compared = |f: &FieldSpec| {
            matches!(f.kind, Kind::Connect | Kind::Reject | Kind::Disconnect)
                && f.class != Class::Schedule
        };
        FIELDS
            .iter()
            .enumerate()
            .filter(|(_, f)| compared(f))
            .filter(|(i, f)| f.key == name || FieldId(*i).to_string() == name)
            .map(|(i, _)| FieldId(i))
            .collect()
    }

    /// The table entry.
    pub fn spec(self) -> &'static FieldSpec {
        &FIELDS[self.0]
    }
}

impl fmt::Display for FieldId {
    /// `C.s.prev`: the kind letter, the class's sub-object and the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spec = self.spec();
        write!(f, "{}.{}.{}", spec.kind.letter() as char, spec.class.key(), spec.key)
    }
}

/// Which input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    fn index(self) -> usize {
        match self {
            Side::Left => 0,
            Side::Right => 1,
        }
    }
}

/// What the caller asks for.
#[derive(Clone, Debug)]
pub struct Options {
    pub mode: Mode,
    /// Stream mode: the origins kept (`None`: all). Linear mode compares `tip` connects only.
    pub origins: Option<Vec<Origin>>,
    /// Records below this height are skipped.
    pub from: Option<i64>,
    /// Records above this height are skipped.
    pub to: Option<i64>,
    /// Keys whose absence is a mismatch (§10.3). A required key that is never compared is
    /// uncovered; other keys' absences stay uncovered, as without `require`.
    pub require: Vec<FieldId>,
    /// Keys left out of the comparison and of coverage: the golden keys a caller declares
    /// (O2b D16). Excluding a required key is a caller error: `Comparator::new` panics.
    pub exclude: Vec<FieldId>,
    /// Go on after a field mismatch or a linear gap. An identity mismatch, or a gap in stream
    /// mode, always stops: the records after it no longer pair up.
    pub keep_going: bool,
}

impl Options {
    /// The defaults of a mode: every origin, the whole range, nothing required, stop at the
    /// first mismatch.
    pub fn new(mode: Mode) -> Options {
        Options {
            mode,
            origins: None,
            from: None,
            to: None,
            require: Vec::new(),
            exclude: Vec::new(),
            keep_going: false,
        }
    }

    fn in_range(&self, h: i64) -> bool {
        self.from.is_none_or(|from| h >= from) && self.to.is_none_or(|to| h <= to)
    }
}

/// A record's place in its input: the caller's record index (a line number), kind and identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct At {
    pub at: usize,
    pub kind: Kind,
    pub id: Ident,
}

impl At {
    fn new(at: usize, record: &Outcome) -> At {
        At {
            at,
            kind: record.kind(),
            id: record.id.clone(),
        }
    }
}

/// One key that differs, or that is absent where it is required. `None` is absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDiff {
    pub field: FieldId,
    pub left: Option<Value>,
    pub right: Option<Value>,
}

/// What went wrong at a [`Mismatch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum What {
    /// The record is on one side only; the side named lacks it.
    Gap(Side),
    /// The two records are of different blocks, kinds or origins.
    Identity,
    /// Keys differ, in spec order.
    Keys(Vec<KeyDiff>),
}

/// A difference between the inputs (spec §10.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    /// The height: of the left record if there is one, else of the right.
    pub h: i64,
    pub left: Option<At>,
    pub right: Option<At>,
    /// The record compared before this one, per side (left, right).
    pub before: [Option<At>; 2],
    pub what: What,
}

impl Mismatch {
    /// The block hash: the left record's if there is one, else the right's.
    pub fn b(&self) -> &Hash256 {
        &self.left.as_ref().or(self.right.as_ref()).expect("a mismatch has a record").id.b
    }
}

/// Per-key counts over the compared record pairs (spec §10.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub equal: u64,
    pub differ: u64,
    pub left_absent: u64,
    pub right_absent: u64,
    pub both_absent: u64,
}

impl Counts {
    /// Pairs where the key was absent on at least one side.
    pub fn absent(&self) -> u64 {
        self.left_absent + self.right_absent + self.both_absent
    }
}

/// The comparison's outcome. Exit codes: 0 clean, 1 diverged, 3 incomplete (the CLI's 2 is an
/// error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every key of every compared record present on both sides and equal, and every required
    /// key compared.
    Clean,
    /// Nothing differs, but a key went uncovered, nothing was compared, or the compared range
    /// stops short of the requested one. Never a pass.
    Incomplete,
    /// At least one mismatch.
    Diverged,
}

impl Verdict {
    /// The process exit code for this verdict.
    pub fn exit_code(self) -> u8 {
        match self {
            Verdict::Clean => 0,
            Verdict::Diverged => 1,
            Verdict::Incomplete => 3,
        }
    }
}

/// A finished comparison.
#[derive(Clone, Debug)]
pub struct Report {
    pub mode: Mode,
    pub verdict: Verdict,
    /// In order; the first is the first mismatch.
    pub mismatches: Vec<Mismatch>,
    /// Every key of the compared classes of the compared kinds, in spec order.
    pub coverage: BTreeMap<FieldId, Counts>,
    /// Keys that were absent somewhere, or required and never compared.
    pub uncovered: Vec<FieldId>,
    /// Record pairs compared.
    pub pairs: u64,
    /// The lowest and highest height compared.
    pub range: Option<(i64, i64)>,
    /// The compared range stops short of [`Options::from`] or [`Options::to`].
    pub short_range: bool,
    /// The comparison stopped at a mismatch before the inputs ended.
    pub stopped: bool,
    /// Linear mode: the heights in the compared range that an input skips, by height and side;
    /// after a stop at the first mismatch, those read up to it.
    pub skipped: Vec<Skipped>,
    /// Each [`Options::exclude`] key, with the compared pairs whose record held it, per side
    /// (left, right).
    pub excluded: BTreeMap<FieldId, [u64; 2]>,
}

/// Heights `from..=to` (within the compared range) that a linear input skips between two of its
/// records: a [`Hole`] in its projection. Unrecorded state is uncovered, never passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skipped {
    pub side: Side,
    pub from: i64,
    pub to: i64,
}

impl Report {
    /// The first mismatch.
    pub fn first(&self) -> Option<&Mismatch> {
        self.mismatches.first()
    }
}

/// Where two `A` (attempt) streams first differ (O2b §4.3: attempts are compared in strict
/// sequence; `x` is schedule and never compared).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptDiff {
    /// The streams have different lengths, and every common pair is equal.
    Count {
        left: usize,
        right: usize,
    },
    /// The `at`-th attempts differ in identity (`h`, `b`, `org`).
    Identity {
        at: usize,
    },
    /// ... in `t`.
    Time {
        at: usize,
    },
    /// ... in their queries: the first differing query index (or the shorter list's length).
    Query {
        at: usize,
        query: usize,
    },
}

/// The first difference between two attempt streams, or `None` if they agree. Pairs are compared
/// in order (identity, then `t`, then the queries), so a difference in a common pair wins over a
/// [`AttemptDiff::Count`], which is reported only when all common pairs are equal.
pub fn compare_attempts(left: &[Attempt], right: &[Attempt]) -> Option<AttemptDiff> {
    for (at, (l, r)) in left.iter().zip(right).enumerate() {
        if l.id != r.id {
            return Some(AttemptDiff::Identity {
                at,
            });
        }
        if l.t != r.t {
            return Some(AttemptDiff::Time {
                at,
            });
        }
        if l.q != r.q {
            let query = l.q.iter().zip(&r.q).take_while(|(a, b)| a == b).count();
            return Some(AttemptDiff::Query {
                at,
                query,
            });
        }
    }
    (left.len() != right.len()).then_some(AttemptDiff::Count {
        left: left.len(),
        right: right.len(),
    })
}

/// One compared key of a record kind, resolved when the comparison starts.
#[derive(Debug)]
struct Compared {
    class: Class,
    key: &'static str,
    field: FieldId,
    required: bool,
}

/// The comparison state: aligned record pairs go in, a [`Report`] comes out.
/// [`compare_linear`] and [`compare_stream`] do the aligning.
#[derive(Debug)]
pub struct Comparator {
    opts: Options,
    /// Per outcome kind, the keys of the mode's classes in spec order: built once, so a record
    /// pair costs no field-table scan.
    compared: Vec<(Kind, Vec<Compared>)>,
    coverage: BTreeMap<FieldId, Counts>,
    mismatches: Vec<Mismatch>,
    pairs: u64,
    range: Option<(i64, i64)>,
    before: [Option<At>; 2],
    stopped: bool,
    skipped: Vec<Skipped>,
    excluded: BTreeMap<FieldId, [u64; 2]>,
}

impl Comparator {
    /// # Panics
    ///
    /// If a key is both in [`Options::require`] and in [`Options::exclude`].
    pub fn new(opts: Options) -> Comparator {
        if let Some(f) = opts.require.iter().find(|f| opts.exclude.contains(f)) {
            panic!("key {f} is both required and excluded");
        }
        let compared = [Kind::Connect, Kind::Reject, Kind::Disconnect]
            .into_iter()
            .map(|kind| {
                let keys = FIELDS
                    .iter()
                    .enumerate()
                    .filter(|(i, f)| {
                        f.kind == kind
                            && opts.mode.classes().contains(&f.class)
                            && !opts.exclude.contains(&FieldId(*i))
                    })
                    .map(|(i, f)| Compared {
                        class: f.class,
                        key: f.key,
                        field: FieldId(i),
                        required: opts.require.contains(&FieldId(i)),
                    })
                    .collect();
                (kind, keys)
            })
            .collect();
        let excluded = opts.exclude.iter().map(|f| (*f, [0; 2])).collect();
        Comparator {
            opts,
            compared,
            coverage: BTreeMap::new(),
            mismatches: Vec::new(),
            pairs: 0,
            range: None,
            before: [None, None],
            stopped: false,
            skipped: Vec::new(),
            excluded,
        }
    }

    /// One aligned pair; `None` is a record missing on that side. Returns `false` when the
    /// comparison must stop (the caller then calls [`Comparator::finish`]).
    pub fn pair(
        &mut self,
        left: Option<&(usize, Outcome)>,
        right: Option<&(usize, Outcome)>,
    ) -> bool {
        let go_on = match (left, right) {
            (Some(l), Some(r)) => self.both(l, r),
            (Some(l), None) => self.gap(Side::Left, l),
            (None, Some(r)) => self.gap(Side::Right, r),
            (None, None) => true,
        };
        self.stopped |= !go_on;
        go_on
    }

    fn see(&mut self, h: i64) {
        self.range = Some(match self.range {
            None => (h, h),
            Some((lo, hi)) => (lo.min(h), hi.max(h)),
        });
    }

    fn mismatch(&mut self, h: i64, left: Option<At>, right: Option<At>, what: What) {
        self.mismatches.push(Mismatch {
            h,
            left,
            right,
            before: self.before.clone(),
            what,
        });
    }

    /// A record on the `present` side only.
    fn gap(&mut self, present: Side, (at, record): &(usize, Outcome)) -> bool {
        self.see(record.id.h);
        let here = At::new(*at, record);
        let (left, right, missing) = match present {
            Side::Left => (Some(here.clone()), None, Side::Right),
            Side::Right => (None, Some(here.clone()), Side::Left),
        };
        self.mismatch(record.id.h, left, right, What::Gap(missing));
        self.before[present.index()] = Some(here);
        self.opts.keep_going && self.opts.mode == Mode::Linear
    }

    fn both(&mut self, (la, left): &(usize, Outcome), (ra, right): &(usize, Outcome)) -> bool {
        self.pairs += 1;
        self.see(left.id.h);
        let here = [Some(At::new(*la, left)), Some(At::new(*ra, right))];
        if left.kind() != right.kind() || left.id != right.id {
            let [l, r] = here;
            self.mismatch(left.id.h, l, r, What::Identity);
            return false;
        }
        for (field, held) in &mut self.excluded {
            let spec = field.spec();
            if spec.kind == left.kind() {
                for (n, record) in held.iter_mut().zip([left, right]) {
                    *n += u64::from(record.fields(spec.class).get(spec.key).is_some());
                }
            }
        }
        let mut diffs = Vec::new();
        let keys = self.compared.iter().find(|(kind, _)| *kind == left.kind());
        for c in keys.map_or(&[][..], |(_, keys)| keys) {
            let lv = left.fields(c.class).get(c.key);
            let rv = right.fields(c.class).get(c.key);
            let counts = self.coverage.entry(c.field).or_default();
            let differs = match (lv, rv) {
                (Some(a), Some(b)) if a == b => {
                    counts.equal += 1;
                    false
                }
                (Some(_), Some(_)) => {
                    counts.differ += 1;
                    true
                }
                (None, Some(_)) => {
                    counts.left_absent += 1;
                    c.required
                }
                (Some(_), None) => {
                    counts.right_absent += 1;
                    c.required
                }
                (None, None) => {
                    counts.both_absent += 1;
                    c.required
                }
            };
            if differs {
                diffs.push(KeyDiff {
                    field: c.field,
                    left: lv.cloned(),
                    right: rv.cloned(),
                });
            }
        }
        let go_on = diffs.is_empty() || self.opts.keep_going;
        if !diffs.is_empty() {
            let [l, r] = here.clone();
            self.mismatch(left.id.h, l, r, What::Keys(diffs));
        }
        self.before = here;
        go_on
    }

    /// The report: the verdict, mismatches and coverage.
    pub fn finish(self) -> Report {
        let mut uncovered: Vec<FieldId> =
            self.coverage.iter().filter(|(_, c)| c.absent() > 0).map(|(f, _)| *f).collect();
        uncovered.extend(self.opts.require.iter().filter(|f| !self.coverage.contains_key(f)));
        uncovered.sort();
        uncovered.dedup();
        let short_range = match self.range {
            None => self.opts.from.is_some() || self.opts.to.is_some(),
            Some((lo, hi)) => {
                self.opts.from.is_some_and(|from| lo > from)
                    || self.opts.to.is_some_and(|to| hi < to)
            }
        };
        let verdict = if !self.mismatches.is_empty() {
            Verdict::Diverged
        } else if self.pairs == 0
            || short_range
            || !uncovered.is_empty()
            || !self.skipped.is_empty()
        {
            Verdict::Incomplete
        } else {
            Verdict::Clean
        };
        Report {
            mode: self.opts.mode,
            verdict,
            mismatches: self.mismatches,
            coverage: self.coverage,
            uncovered,
            pairs: self.pairs,
            range: self.range,
            short_range,
            stopped: self.stopped,
            skipped: self.skipped,
            excluded: self.excluded,
        }
    }
}

/// Why a comparison could not run.
#[derive(Debug)]
pub enum CompareError<E> {
    /// The caller's input failed (a read or parse error).
    Input(E),
    /// A linear input is not a projection: a record other than a `tip` `C`, or heights that do
    /// not rise.
    NotProjected {
        side: Side,
        at: usize,
    },
}

impl<E: fmt::Display> fmt::Display for CompareError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareError::Input(e) => e.fmt(f),
            CompareError::NotProjected {
                side,
                at,
            } => {
                write!(f, "{side:?} record {at} is not part of a linear projection")
            }
        }
    }
}

impl<E: fmt::Debug + fmt::Display> core::error::Error for CompareError<E> {}

/// Linear mode: both inputs are projections (each record a `tip` `C`, heights rising), as
/// [`Projection::picks`] selects them. Records are paired by height; a height on one side only
/// is a gap. Heights an input skips between two records are [`Report::skipped`], so even a hole
/// both inputs share leaves the run incomplete. `opts.mode` is set to [`Mode::Linear`];
/// `opts.origins` does not apply.
pub fn compare_linear<E>(
    left: impl IntoIterator<Item = Result<(usize, Outcome), E>>,
    right: impl IntoIterator<Item = Result<(usize, Outcome), E>>,
    opts: Options,
) -> Result<Report, CompareError<E>> {
    let opts = Options {
        mode: Mode::Linear,
        ..opts
    };
    let mut l = Linear::new(left, Side::Left);
    let mut r = Linear::new(right, Side::Right);
    let mut cmp = Comparator::new(opts);
    loop {
        let go_on = match (l.peek(&cmp.opts)?, r.peek(&cmp.opts)?) {
            (None, None) => break,
            (Some(lh), Some(rh)) if lh == rh => cmp.pair(Some(&l.take()), Some(&r.take())),
            (Some(lh), rh) if rh.is_none_or(|rh| lh < rh) => cmp.pair(Some(&l.take()), None),
            _ => cmp.pair(None, Some(&r.take())),
        };
        if !go_on {
            break;
        }
    }
    cmp.skipped = l.skipped;
    cmp.skipped.append(&mut r.skipped);
    cmp.skipped.sort_by_key(|s| (s.from, s.side.index()));
    Ok(cmp.finish())
}

/// One linear input with one record of look-ahead, filtered to the requested range.
struct Linear<I> {
    iter: I,
    side: Side,
    head: Option<(usize, Outcome)>,
    last_h: Option<i64>,
    /// The heights in range it skipped so far.
    skipped: Vec<Skipped>,
}

impl<E, I: Iterator<Item = Result<(usize, Outcome), E>>> Linear<I> {
    fn new(iter: impl IntoIterator<IntoIter = I>, side: Side) -> Self {
        Linear {
            iter: iter.into_iter(),
            side,
            head: None,
            last_h: None,
            skipped: Vec::new(),
        }
    }

    fn peek(&mut self, opts: &Options) -> Result<Option<i64>, CompareError<E>> {
        while self.head.is_none() {
            let Some(next) = self.iter.next() else {
                return Ok(None);
            };
            let (at, record) = next.map_err(CompareError::Input)?;
            let projected = record.kind() == Kind::Connect
                && record.id.org == Origin::Tip
                && self.last_h.is_none_or(|h| record.id.h > h);
            if !projected {
                return Err(CompareError::NotProjected {
                    side: self.side,
                    at,
                });
            }
            let h = record.id.h;
            if let Some(last) = self.last_h {
                let from = opts.from.map_or(last + 1, |f| f.max(last + 1));
                let to = opts.to.map_or(h - 1, |t| t.min(h - 1));
                if from <= to {
                    self.skipped.push(Skipped {
                        side: self.side,
                        from,
                        to,
                    });
                }
            }
            self.last_h = Some(h);
            if opts.in_range(h) {
                self.head = Some((at, record));
            }
        }
        Ok(self.head.as_ref().map(|(_, r)| r.id.h))
    }

    fn take(&mut self) -> (usize, Outcome) {
        self.head.take().expect("peeked")
    }
}

/// Stream mode: the `C`, `R` and `D` records of the selected origins and range, compared
/// record for record; other kinds are skipped. `opts.mode` is set to [`Mode::Stream`].
pub fn compare_stream<E>(
    left: impl IntoIterator<Item = Result<(usize, Record), E>>,
    right: impl IntoIterator<Item = Result<(usize, Record), E>>,
    opts: Options,
) -> Result<Report, CompareError<E>> {
    let opts = Options {
        mode: Mode::Stream,
        ..opts
    };
    let mut left = left.into_iter();
    let mut right = right.into_iter();
    let mut cmp = Comparator::new(opts.clone());
    loop {
        let l = next_kept(&mut left, &opts)?;
        let r = next_kept(&mut right, &opts)?;
        if l.is_none() && r.is_none() {
            break;
        }
        let go_on = cmp.pair(l.as_ref(), r.as_ref());
        if !go_on {
            break;
        }
    }
    Ok(cmp.finish())
}

/// The next `C`, `R` or `D` of the selected origins and range.
fn next_kept<E>(
    iter: &mut impl Iterator<Item = Result<(usize, Record), E>>,
    opts: &Options,
) -> Result<Option<(usize, Outcome)>, CompareError<E>> {
    for item in iter {
        let (at, record) = item.map_err(CompareError::Input)?;
        if let Record::Outcome(o) = record
            && opts.origins.as_ref().is_none_or(|keep| keep.contains(&o.id.org))
            && opts.in_range(o.id.h)
        {
            return Ok(Some((at, o)));
        }
    }
    Ok(None)
}

/// One block of a projection: its height, its hash and the index of the record that put it on
/// the final chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slot {
    pub h: i64,
    pub b: Hash256,
    pub at: usize,
    /// The record's `prev`, when present and not `null`.
    pub prev: Option<Hash256>,
}

/// Heights a run's first tip connect jumped over: blocks connected without a record (a crash's
/// roll-forward, spec §1). The projection has no slots there, so a linear comparison reports
/// them as gaps (§10.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hole {
    pub run: usize,
    /// The record after the hole.
    pub at: usize,
    /// The first and last missing height.
    pub from: i64,
    pub to: i64,
}

/// A run's first tip connect that lowered the tip (a `-reindex-chainstate`, or a restart after
/// `ReplayBlocks`): the run (counting `H` records from 1), the record and the heights.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewind {
    pub run: usize,
    pub at: usize,
    /// The tip height before the record.
    pub from: i64,
    /// The record's height.
    pub to: i64,
}

/// Why an input has no linear projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectError {
    /// A tip connect above the tip's child inside a run (only a run's first tip connect may
    /// jump: a [`Hole`]).
    Gap {
        at: usize,
        h: i64,
        tip: i64,
    },
    /// A tip connect at or below the tip inside a run: only a run's first tip connect may rewind.
    Overlap {
        at: usize,
        h: i64,
        tip: i64,
    },
    /// A tip connect whose `prev` is not the block below it.
    Unlinked {
        at: usize,
        h: i64,
        prev: Hash256,
        below: Hash256,
    },
    /// A disconnect of a block that is not the tip.
    Disconnect {
        at: usize,
        h: i64,
        b: Hash256,
        tip: Option<(i64, Hash256)>,
    },
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProjectError::Gap {
                at,
                h,
                tip,
            } => {
                write!(f, "record {at}: a tip connect at height {h} above the tip at {tip} (gap)")
            }
            ProjectError::Overlap {
                at,
                h,
                tip,
            } => {
                write!(f, "record {at}: a tip connect at height {h} at or below the tip at {tip}")
            }
            ProjectError::Unlinked {
                at,
                h,
                prev,
                below,
            } => write!(
                f,
                "record {at}: the tip connect at height {h} has prev {prev}, the block below is {below}"
            ),
            ProjectError::Disconnect {
                at,
                h,
                b,
                tip,
            } => match tip {
                Some((th, tb)) => write!(
                    f,
                    "record {at}: disconnect of {b} at height {h}, but the tip is {tb} at {th}"
                ),
                None => write!(f, "record {at}: disconnect of {b} at height {h} with no tip"),
            },
        }
    }
}

impl core::error::Error for ProjectError {}

/// Finds an input's final active chain (spec §10.2) in one pass: for each block of that chain,
/// in height order, its last `C` with `org:"tip"`.
///
/// * a tip `C` at height `h` must be the tip's child (`h` = tip + 1; its `prev`, when present
///   and not `null`, the tip's hash). A run's first tip connect may also rewind: it lowers the
///   tip to `h - 1` and is recorded as a [`Rewind`]; or jump forward over blocks connected
///   without a record, recorded as a [`Hole`]. The first record of all starts the chain at its
///   height (a digest that begins after genesis, §9.2);
/// * a `D` of origin `tip` with `res:"ok"`, or of origin `replay` with `res` `ok` or `unclean`
///   (what `DisconnectTip` and `ReplayBlocks` accept), must be the tip's and pops it; any other
///   `D` of those origins must be the tip's and leaves it. Once the chain is popped empty, the
///   last popped block stands for the tip's child: the next tip connect replaces it (same height
///   and `prev`);
/// * `R` records, `C` and `D` of origins `proposal` and `verifydb`, and `A` and `S` records do
///   not touch the chain. `H` starts a run.
#[derive(Debug, Default)]
pub struct Projector {
    chain: Vec<Slot>,
    rewinds: Vec<Rewind>,
    holes: Vec<Hole>,
    /// The last block popped off an emptied chain.
    floor: Option<Slot>,
    run: usize,
    run_moved: bool,
}

impl Projector {
    pub fn new() -> Projector {
        Projector::default()
    }

    /// Feeds the record with index `at` (the caller's numbering, e.g. a line number).
    pub fn feed(&mut self, at: usize, record: &Record) -> Result<(), ProjectError> {
        let o = match record {
            Record::Header(_) => {
                self.run += 1;
                self.run_moved = false;
                return Ok(());
            }
            Record::Outcome(o) => o,
            Record::Attempt(_) | Record::Sparse(_) => return Ok(()),
        };
        match (o.kind(), o.id.org) {
            (Kind::Connect, Origin::Tip) => self.connect(at, o),
            (Kind::Disconnect, org @ (Origin::Tip | Origin::Replay)) => self.disconnect(at, o, org),
            _ => Ok(()),
        }
    }

    fn connect(&mut self, at: usize, o: &Outcome) -> Result<(), ProjectError> {
        let h = o.id.h;
        let prev = o.s().get("prev").and_then(Value::as_hash);
        let first_of_run = !self.run_moved;
        self.run_moved = true;
        // The block below the tip's child: the tip, or below an emptied chain's last block.
        let below = match (self.chain.last(), &self.floor) {
            (Some(tip), _) => Some((tip.h, Some(tip.b))),
            (None, Some(floor)) => Some((floor.h - 1, floor.prev)),
            (None, None) => None,
        };
        let mut linked = true;
        if let Some((tip_h, _)) = below {
            if h > tip_h + 1 {
                if !first_of_run {
                    return Err(ProjectError::Gap {
                        at,
                        h,
                        tip: tip_h,
                    });
                }
                self.holes.push(Hole {
                    run: self.run,
                    at,
                    from: tip_h + 1,
                    to: h - 1,
                });
                linked = false;
            }
            if h <= tip_h {
                if !first_of_run {
                    return Err(ProjectError::Overlap {
                        at,
                        h,
                        tip: tip_h,
                    });
                }
                self.chain.retain(|slot| slot.h < h);
                self.rewinds.push(Rewind {
                    run: self.run,
                    at,
                    from: tip_h,
                    to: h,
                });
            }
        }
        // After a rewind the block below is the retained chain's tip, if it is the parent's
        // height (a rewind into a hole has no recorded parent).
        let below = match (self.chain.last(), &self.floor) {
            (Some(tip), _) if tip.h == h - 1 => Some(tip.b),
            (None, Some(floor)) if floor.h == h => floor.prev,
            _ => None,
        };
        if let (true, Some(below), Some(prev)) = (linked, below, prev)
            && prev != below
        {
            return Err(ProjectError::Unlinked {
                at,
                h,
                prev,
                below,
            });
        }
        self.chain.push(Slot {
            h,
            b: o.id.b,
            at,
            prev,
        });
        Ok(())
    }

    fn disconnect(&mut self, at: usize, o: &Outcome, org: Origin) -> Result<(), ProjectError> {
        if org == Origin::Tip {
            self.run_moved = true;
        }
        let tip = self.chain.last().map(|t| (t.h, t.b));
        if tip != Some((o.id.h, o.id.b)) {
            return Err(ProjectError::Disconnect {
                at,
                h: o.id.h,
                b: o.id.b,
                tip,
            });
        }
        let res = o.s().get("res").and_then(Value::as_str);
        let pops = match org {
            Origin::Replay => matches!(res, Some("ok" | "unclean")),
            _ => res == Some("ok"),
        };
        if pops {
            let popped = self.chain.pop();
            if self.chain.is_empty() {
                self.floor = popped;
            }
        }
        Ok(())
    }

    /// The final chain.
    pub fn finish(self) -> Projection {
        Projection {
            chain: self.chain,
            rewinds: self.rewinds,
            holes: self.holes,
            runs: self.run,
        }
    }
}

/// An input's final active chain: one [`Slot`] per recorded block, heights rising (consecutive
/// but for [`Hole`]s), record indexes rising.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projection {
    pub chain: Vec<Slot>,
    pub rewinds: Vec<Rewind>,
    pub holes: Vec<Hole>,
    /// The `H` records seen.
    pub runs: usize,
}

impl Projection {
    /// The record indexes of the chain, rising: what the second pass keeps.
    pub fn picks(&self) -> impl Iterator<Item = usize> + '_ {
        self.chain.iter().map(|s| s.at)
    }

    /// The lowest and highest height.
    pub fn range(&self) -> Option<(i64, i64)> {
        Some((self.chain.first()?.h, self.chain.last()?.h))
    }
}
