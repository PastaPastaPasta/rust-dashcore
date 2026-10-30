//! Parses StateDigest files strictly and tallies what they cover (O8 spec §13.1).
//!
//! Usage: `statedigest-stats <file.jsonl>...`. Every line must parse, every file must start with an
//! `H` record, and every record must be a kind its file holds (spec §3: digest `H C R D`, decisions
//! `H A`, golden `H C S`). The first error is printed with its file and line and the exit code is 1.
//! On success one JSON object is printed with counts per record kind and origin, per effect tag,
//! per query tag and answer, per `R`/`D` result and per `R` class and reason.

use std::collections::BTreeMap;
use std::process::ExitCode;

use dash_statedigest::{FileKind, Kind, Lines, Record, Value};

#[derive(Default)]
struct Tally(BTreeMap<String, BTreeMap<String, u64>>);

impl Tally {
    fn add(&mut self, table: &str, key: impl Into<String>) {
        *self.0.entry(table.into()).or_default().entry(key.into()).or_default() += 1;
    }

    fn json(&self) -> String {
        let tables: Vec<String> = self
            .0
            .iter()
            .map(|(table, counts)| {
                let counts: Vec<String> =
                    counts.iter().map(|(k, n)| format!("{}:{}", quote(k), n)).collect();
                format!("{}:{{{}}}", quote(table), counts.join(","))
            })
            .collect();
        format!("{{{}}}", tables.join(","))
    }
}

/// A JSON string; the keys are ASCII names and reasons, so only `"` and `\` need escaping.
fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn text(v: &Value) -> String {
    v.as_bytes().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_else(|| "?".into())
}

/// The tag of an `fx` or `q` entry: the first element of its array.
fn tag(entry: &Value) -> String {
    entry.as_arr().and_then(|a| a.first()).map(text).unwrap_or_else(|| "?".into())
}

/// A query's tag and the shape of its answer, the last element: `true`, `false`, `null` or `set`.
fn answer(entry: &Value) -> String {
    let shape = match entry.as_arr().and_then(|a| a.last()) {
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Null) => "null".into(),
        _ => "set".into(),
    };
    format!("{}/{shape}", tag(entry))
}

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

fn tally_file(path: &str, tally: &mut Tally) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut file = None;
    for (line, record) in Lines::new(&bytes) {
        let record = record.map_err(|e| format!("{path}:{line}: {e}"))?;
        let this_file = match (&record, file) {
            (Record::Header(h), None) => {
                file = Some(h.file);
                h.file
            }
            (Record::Header(h), Some(f)) if h.file != f => {
                return Err(format!(
                    "{path}:{line}: a {} header in a {} file",
                    h.file.as_str(),
                    f.as_str()
                ));
            }
            (_, None) => return Err(format!("{path}:{line}: the file does not start with H")),
            (_, Some(f)) => f,
        };
        let kind = char::from(record.kind().letter());
        if !holds(this_file, record.kind()) {
            return Err(format!(
                "{path}:{line}: record kind {kind} in a {} file",
                this_file.as_str()
            ));
        }
        match &record {
            Record::Header(_) | Record::Sparse(_) => tally.add("kind", kind),
            Record::Outcome(o) => {
                tally.add("kind", format!("{kind}/{}", o.id.org.as_str()));
                if let Some(res) = o.s().get("res") {
                    tally.add("result", format!("{kind}/{}", text(res)));
                }
                if let Some(cls) = o.s().get("cls") {
                    let why = o.s().get("why").map(text).unwrap_or_default();
                    tally.add("reason", format!("{}/{why}", text(cls)));
                }
                for fx in o.o().get("fx").and_then(Value::as_arr).into_iter().flatten() {
                    tally.add("effect", tag(fx));
                }
            }
            Record::Attempt(a) => {
                tally.add("kind", format!("{kind}/{}", a.id.org.as_str()));
                for q in &a.q {
                    tally.add("query", answer(&q.to_value()));
                }
            }
        }
    }
    let file = file.ok_or_else(|| format!("{path}: empty"))?;
    tally.add("files", file.as_str());
    Ok(())
}

fn main() -> ExitCode {
    let mut tally = Tally::default();
    for path in std::env::args().skip(1) {
        if let Err(e) = tally_file(&path, &mut tally) {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    }
    println!("{}", tally.json());
    ExitCode::SUCCESS
}
