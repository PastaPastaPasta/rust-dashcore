//! What both legs' tests share around their passes: the child entry, running A then B in fresh
//! children, and saving the records with the leg's report.

use super::meter::Guarded;
use super::supervisor;
use crate::Result;
use crate::compare::LegReport;
use crate::fixture::Fixture;
use crate::record::Record;

/// A child's pass, from `AMB_PASS=<leg>:<a|b>:<rows>:<count 0|1>`.
pub struct ChildPass {
    pub pass_a: bool,
    /// The fixture's first `rows` workload blocks.
    pub fixture: Fixture,
    pub meter: Guarded,
    pub revision: String,
}

/// In a child of `leg` (its test is `child`'s), runs `pass` and exits with its record; a no-op
/// outside a child. `exclude_caller`: the caller is a driver with no counter.
pub fn child_main(leg: &str, exclude_caller: bool, pass: impl FnOnce(ChildPass) -> Result<Record>) {
    let Some(spec) = supervisor::pass() else {
        return;
    };
    let parts: Vec<&str> = spec.split(':').collect();
    let (pass_a, rows, count) = match parts[..] {
        [l, p @ ("a" | "b"), rows, count @ ("0" | "1")] if l == leg => match rows.parse() {
            Ok(rows) => (p == "a", rows, count == "1"),
            Err(_) => supervisor::die(&format!("pass {spec:?}")),
        },
        _ => supervisor::die(&format!("pass {spec:?}")),
    };
    let fixture = match Fixture::load() {
        Ok(f) => f.truncated(rows),
        Err(e) => supervisor::die(&e.0),
    };
    let run = pass(ChildPass {
        pass_a,
        fixture,
        meter: Guarded::new(count, exclude_caller),
        revision: supervisor::child_revision(),
    });
    match run.and_then(|r| r.encode()) {
        Ok(bytes) => supervisor::finish(&bytes),
        Err(e) => supervisor::die(&e.0),
    }
}

/// Runs `leg`'s A then B, each in a fresh child of the test `child`, on `rows` rows; refuses a
/// record that names another process than the child that wrote it.
pub fn run(child: &str, leg: &str, rows: usize, count: bool) -> Result<(Record, Record)> {
    let pass = |p: &str| -> Result<Record> {
        let spec = format!("{leg}:{p}:{rows}:{}", u8::from(count));
        let (pid, bytes) = supervisor::run(child, &spec, &[])?;
        let r = Record::decode(&bytes)?;
        if r.pid != pid {
            crate::refuse!("child {pid}'s record names process {}", r.pid);
        }
        Ok(r)
    };
    Ok((pass("a")?, pass("b")?))
}

/// The measurement's rows: `AMB_ROWS` (a mutant run on a short workload, with its baseline on
/// the same rows), else the full fixture. The gate takes only the full fixture's legs.
pub fn measured_rows() -> usize {
    std::env::var("AMB_ROWS").map_or(usize::MAX, |r| r.parse().expect("AMB_ROWS is a row count"))
}

/// Whether `rows` covers the full fixture (the gate's legs).
pub fn is_full(rows: usize) -> bool {
    rows >= crate::fixture::WORKLOAD_BLOCKS
}

/// Saves `<leg>-a.json`, `<leg>-b.json` and `<leg>-leg.json` under `AMB_OUT`; the report's json.
pub fn save(leg: &str, a: &Record, b: &Record, report: &LegReport) -> Result<String> {
    let json = match serde_json::to_string(report) {
        Ok(j) => j,
        Err(e) => crate::refuse!("the report: {e}"),
    };
    for (name, bytes) in [
        (format!("{leg}-a.json"), a.encode()?),
        (format!("{leg}-b.json"), b.encode()?),
        (format!("{leg}-leg.json"), json.clone().into_bytes()),
    ] {
        supervisor::save(&name, &bytes);
    }
    Ok(json)
}
