//! `digest compare` (spec O8 §10; node program task O9a): runs `dash_statedigest::compare` over
//! two files and renders the report.
//!
//! Linear mode reads each file twice: a [`Projector`] pass finds the final chain, keeping only
//! `(height, hash, line)` per block, and a second pass hands the comparator the lines it picked.
//! Stream mode reads each file once. Memory stays O(chain length) either way.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use dash_statedigest::compare::{
    At, CompareError, Mode, Options, Projection, Projector, Report, Side, Verdict, What,
    compare_linear, compare_stream,
};
use dash_statedigest::{Chain, FileKind, Outcome, Record, Value, parse_line};

use super::dump::{mode_name, side_name};
use super::store::Records;

/// One input file, as its first `H` record describes it.
#[derive(Clone, Debug)]
pub struct Input {
    pub path: PathBuf,
    pub file: FileKind,
    pub chain: Chain,
    /// Linear mode: the input's final chain.
    pub projection: Option<Projection>,
}

impl Input {
    /// Reads the first record, which must be an `H`.
    pub fn open(path: &Path) -> Result<Input> {
        let first = Records::open(path)?
            .next()
            .with_context(|| format!("{} is empty", path.display()))??;
        let Record::Header(h) = first.1 else {
            bail!("{}: the first record is not an H", path.display());
        };
        Ok(Input {
            path: path.to_owned(),
            file: h.file,
            chain: h.chain,
            projection: None,
        })
    }
}

/// The chain both inputs are of; they must agree.
pub fn chain_of(inputs: &[Input; 2]) -> Result<Chain> {
    let [l, r] = inputs;
    if l.chain != r.chain {
        bail!(
            "the inputs are of different chains: {} is {}, {} is {}",
            l.path.display(),
            l.chain.as_str(),
            r.path.display(),
            r.chain.as_str()
        );
    }
    Ok(l.chain)
}

/// The input's records; an `H` record of another chain than the first one's is an error, so a
/// file cannot switch chains under the value rules.
fn records(input: &Input) -> Result<impl Iterator<Item = Result<(usize, Record)>> + use<>> {
    let (path, chain) = (input.path.clone(), input.chain);
    Ok(Records::open(&input.path)?.map(move |item| match item? {
        (line, Record::Header(h)) if h.chain != chain => bail!(
            "{}:{line}: an H record of chain {}, after {}",
            path.display(),
            h.chain.as_str(),
            chain.as_str()
        ),
        record => Ok(record),
    }))
}

fn project_records(
    path: &Path,
    records: impl Iterator<Item = Result<(usize, Record)>>,
) -> Result<Projection> {
    let mut projector = Projector::new();
    for item in records {
        let (line, record) = item?;
        projector.feed(line, &record).with_context(|| format!("projecting {}", path.display()))?;
    }
    Ok(projector.finish())
}

/// Projects a file onto its final chain (the first pass).
pub fn project(path: &Path) -> Result<Projection> {
    project_records(path, Records::open(path)?)
}

/// The records a projection picked (the second pass), in height order. Only the picked lines
/// are parsed, and reading stops after the last.
fn picked(
    path: &Path,
    projection: &Projection,
) -> Result<impl Iterator<Item = Result<(usize, Outcome)>> + use<>> {
    let mut picks = projection.picks().collect::<Vec<_>>().into_iter();
    let mut lines = Records::raw(path)?;
    let name = path.display().to_string();
    Ok(std::iter::from_fn(move || {
        let wanted = picks.next()?;
        loop {
            let (line, bytes) = match lines.next()? {
                Ok(item) => item,
                Err(e) => return Some(Err(e)),
            };
            if line != wanted {
                continue;
            }
            return Some(match parse_line(&bytes) {
                Ok(Record::Outcome(o)) => Ok((line, o)),
                Ok(_) => Err(anyhow::anyhow!("{name}:{line}: not an outcome")),
                Err(e) => Err(anyhow::anyhow!("{name}:{line}: {e}")),
            });
        }
    }))
}

/// Projects a linear input (keeping its projection for the report) and returns its picks.
fn linear_input(
    input: &mut Input,
) -> Result<impl Iterator<Item = Result<(usize, Outcome)>> + use<>> {
    let projection = project_records(&input.path, records(input)?)?;
    let picks = picked(&input.path, &projection)?;
    input.projection = Some(projection);
    Ok(picks)
}

fn input_error(e: CompareError<anyhow::Error>) -> anyhow::Error {
    match e {
        CompareError::Input(e) => e,
        other => anyhow::anyhow!("{other}"),
    }
}

/// Compares the two inputs (projecting them first in linear mode).
pub fn compare(inputs: &mut [Input; 2], opts: Options) -> Result<Report> {
    match opts.mode {
        Mode::Linear => {
            let [l, r] = inputs;
            let (left, right) = (linear_input(l)?, linear_input(r)?);
            compare_linear(left, right, opts).map_err(input_error)
        }
        Mode::Stream => {
            let (left, right) = (records(&inputs[0])?, records(&inputs[1])?);
            compare_stream(left, right, opts).map_err(input_error)
        }
    }
}

/// Whether reports on this chain may show values on stdout: only regtest and devnet. Mainnet
/// and testnet values go to the private dump only (PLAN §7.5, manager ruling 6).
pub fn shows_values(chain: Chain) -> bool {
    matches!(chain, Chain::Regtest | Chain::Devnet)
}

/// A value as its canonical JSON text; `absent` when the key is absent.
pub fn show(value: Option<&Value>) -> String {
    match value {
        Some(v) => String::from_utf8_lossy(&v.to_vec()).into_owned(),
        None => "absent".into(),
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Clean => "CLEAN",
        Verdict::Incomplete => "INCOMPLETE",
        Verdict::Diverged => "DIVERGED",
    }
}

/// The exit code of a clean comparison that left keys out (CH3a D167): never 0, which is clean
/// on every key the mode compares.
pub const CLEAN_EXCEPT_EXCLUDED: u8 = 4;

/// The CLI's exit code for `report`: [`Verdict::exit_code`], but [`CLEAN_EXCEPT_EXCLUDED`] for a
/// clean comparison with excluded keys.
pub fn exit_code(report: &Report) -> u8 {
    match report.verdict {
        Verdict::Clean if !report.excluded.is_empty() => CLEAN_EXCEPT_EXCLUDED,
        v => v.exit_code(),
    }
}

/// The verdict as the first line names it, the excluded keys named after a clean one.
fn verdict_line(report: &Report) -> String {
    match report.verdict {
        Verdict::Clean if !report.excluded.is_empty() => {
            let names: Vec<String> = report.excluded.keys().map(ToString::to_string).collect();
            format!("CLEAN EXCEPT THE EXCLUDED KEYS {}", names.join(", "))
        }
        v => verdict_name(v).to_owned(),
    }
}

/// `lo..=hi`, or `none`.
fn span(range: Option<(i64, i64)>, none: &str) -> String {
    range.map_or(none.to_owned(), |(lo, hi)| format!("{lo}..={hi}"))
}

/// One record's place: line, kind, height, block and origin.
fn place(at: &At) -> String {
    format!(
        "line {} {} h {} {} org {}",
        at.at,
        at.kind.letter() as char,
        at.id.h,
        at.id.b,
        at.id.org.as_str()
    )
}

/// The text report: the verdict, each mismatch, the coverage table and the uncovered keys.
/// With `values` false only heights, hashes, kinds, origins and key names are printed.
pub fn render(report: &Report, inputs: &[Input; 2], values: bool) -> String {
    let mut out = String::new();
    let mode = mode_name(report.mode);
    let range = span(report.range, "none");
    let _ = writeln!(
        out,
        "digest compare ({mode}): {} pairs, heights {range}: {} (exit {})",
        report.pairs,
        verdict_line(report),
        exit_code(report)
    );
    for (side, input) in ["left", "right"].iter().zip(inputs) {
        let _ = write!(out, "  {side}: {} ({})", input.path.display(), input.file.as_str());
        if let Some(p) = &input.projection {
            let _ = write!(
                out,
                ", final chain {}, {} runs, {} rewinds, {} holes",
                span(p.range(), "empty"),
                p.runs,
                p.rewinds.len(),
                p.holes.len()
            );
        }
        out.push('\n');
    }
    for (n, m) in report.mismatches.iter().enumerate() {
        let what = match &m.what {
            What::Gap(Side::Left) => "gap: the left lacks this record".to_owned(),
            What::Gap(Side::Right) => "gap: the right lacks this record".to_owned(),
            What::Identity => "identity differs".to_owned(),
            What::Keys(diffs) => {
                let names: Vec<String> = diffs.iter().map(|d| d.field.to_string()).collect();
                format!("keys {}", names.join(", "))
            }
        };
        let _ = writeln!(
            out,
            "{} at height {} block {}: {what}",
            if n == 0 {
                "first mismatch"
            } else {
                "mismatch"
            },
            m.h,
            m.b()
        );
        for (i, (side, at)) in [("left", &m.left), ("right", &m.right)].into_iter().enumerate() {
            if let Some(at) = at {
                let _ = writeln!(out, "  {side}: {}", place(at));
            }
            if let Some(before) = &m.before[i] {
                let _ = writeln!(out, "  {side}, compared before: {}", place(before));
            }
        }
        if let What::Keys(diffs) = &m.what {
            for d in diffs {
                if values {
                    let _ = writeln!(
                        out,
                        "  {}: left {} right {}",
                        d.field,
                        show(d.left.as_ref()),
                        show(d.right.as_ref())
                    );
                } else {
                    let _ = writeln!(out, "  {}: values in the private dump", d.field);
                }
            }
        }
    }
    if report.stopped {
        let _ = writeln!(out, "stopped at the first mismatch (--keep-going to go on)");
    }
    if report.short_range {
        let _ = writeln!(out, "the compared range stops short of --from/--to");
    }
    if !report.skipped.is_empty() {
        let spans: Vec<String> = report
            .skipped
            .iter()
            .map(|s| format!("{} {}..={}", side_name(s.side), s.from, s.to))
            .collect();
        let _ = writeln!(out, "unrecorded heights (uncovered): {}", spans.join(", "));
    }
    let _ = writeln!(out, "coverage (equal / differ / absent left, right, both):");
    for (field, c) in &report.coverage {
        let _ = writeln!(
            out,
            "  {:<14} {} / {} / {}, {}, {}",
            field.to_string(),
            c.equal,
            c.differ,
            c.left_absent,
            c.right_absent,
            c.both_absent
        );
    }
    if !report.uncovered.is_empty() {
        let names: Vec<String> = report.uncovered.iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "uncovered (never passed): {}", names.join(", "));
    }
    if !report.excluded.is_empty() {
        let _ = writeln!(out, "excluded, not compared (pairs holding the key: left, right):");
        for (field, [left, right]) in &report.excluded {
            let _ = writeln!(out, "  {:<14} {left}, {right}", field.to_string());
        }
    }
    out
}
