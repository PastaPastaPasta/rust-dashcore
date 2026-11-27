//! `mixnet-judge`: judges an O12b observation log and prints its report and metrics.
//!
//! ```text
//! mixnet-judge <obs.jsonl> --plan <plan.json> --gate <stage-a|rehearsal>
//! ```
//!
//! It prints one pretty JSON object `{"report": ..., "metrics": ..., "metrics_error": ...}` on
//! stdout. Exit codes:
//! - 0: `report.passed`.
//! - 1: not passed and some criterion's verdict is `Fail`.
//! - 2: not passed and no `Fail` verdict, any usage, read, parse or plan-mismatch error
//!   (printed on stderr, nothing on stdout), or a failed stdout write.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::exit;

use dash_testkit::mixnet::contract::{Envelope, Record, RolePlan};
use dash_testkit::mixnet::criteria::{Config, Gate, Metrics, Report, Verdict, judge, metrics};
use serde::Serialize;

const USAGE: &str = "usage: mixnet-judge <obs.jsonl> --plan <plan.json> --gate <stage-a|rehearsal>";

fn say(message: &str) {
    let _ = writeln!(io::stderr().lock(), "mixnet-judge: {message}");
}

fn fail(message: &str) -> ! {
    say(&format!("{message}\n{USAGE}"));
    exit(2);
}

fn error(message: &str) -> ! {
    say(message);
    exit(2);
}

fn is_flag(arg: &OsStr) -> bool {
    arg.as_encoded_bytes().starts_with(b"-")
}

#[derive(Serialize)]
struct Output {
    report: Report,
    metrics: Option<Metrics>,
    metrics_error: Option<String>,
}

fn main() {
    let (mut obs, mut plan_path, mut gate) = (None, None, None::<OsString>);
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        if !is_flag(&arg) {
            if obs.replace(PathBuf::from(arg)).is_some() {
                fail("more than one log path given");
            }
            continue;
        }
        let (slot, flag) = match arg.to_str() {
            Some("--plan") => (&mut plan_path, "--plan"),
            Some("--gate") => (&mut gate, "--gate"),
            _ => fail(&format!("unknown argument {arg:?}")),
        };
        let value = args
            .next()
            .filter(|v| !is_flag(v))
            .unwrap_or_else(|| fail(&format!("{flag} needs a value")));
        if slot.replace(value).is_some() {
            fail(&format!("{flag} is given twice"));
        }
    }
    let obs_path = obs.unwrap_or_else(|| fail("<obs.jsonl> is required"));
    let plan_path = PathBuf::from(plan_path.unwrap_or_else(|| fail("--plan is required")));
    let gate = match gate.unwrap_or_else(|| fail("--gate is required")).to_str() {
        Some("stage-a") => Gate::StageA,
        Some("rehearsal") => Gate::Rehearsal,
        other => fail(&format!("unknown gate {other:?} (expected stage-a or rehearsal)")),
    };

    let plan_text = fs::read_to_string(&plan_path)
        .unwrap_or_else(|e| error(&format!("cannot read {}: {e}", plan_path.display())));
    let plan: RolePlan = serde_json::from_str(&plan_text)
        .unwrap_or_else(|e| error(&format!("cannot parse {}: {e}", plan_path.display())));

    let file = File::open(&obs_path)
        .unwrap_or_else(|e| error(&format!("cannot open {}: {e}", obs_path.display())));
    let mut reader = BufReader::new(file);
    let (mut log, mut line, mut line_no) = (Vec::new(), Vec::new(), 0_u64);
    loop {
        line.clear();
        line_no += 1;
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => error(&format!("cannot read {}: line {line_no}: {e}", obs_path.display())),
        }
        if std::str::from_utf8(&line).is_ok_and(|s| s.trim().is_empty()) {
            continue;
        }
        let envelope: Envelope = serde_json::from_slice(&line)
            .unwrap_or_else(|e| error(&format!("line {line_no}: {e}")));
        log.push(envelope);
    }

    if let Some(Record::Run(run)) = log.first().map(|e| &e.record)
        && let Ok(log_plan) = RolePlan::new(run.preset, run.caps.clone(), run.context)
        && plan != log_plan
    {
        error("plan.json does not match the log's run");
    }

    let config = Config::default();
    let report = judge(&log, gate, &config);
    let (metrics, metrics_error) = match metrics(&log, &config) {
        Ok(m) => (Some(m), None),
        Err(e) => (None, Some(e)),
    };
    let code = if report.passed {
        0
    } else if report.criteria.iter().any(|c| matches!(c.verdict, Verdict::Fail(_))) {
        1
    } else {
        2
    };
    let output = Output {
        report,
        metrics,
        metrics_error,
    };
    let json = serde_json::to_string_pretty(&output).expect("a judge output serializes");
    let mut stdout = io::stdout().lock();
    if let Err(e) = writeln!(stdout, "{json}").and_then(|()| stdout.flush()) {
        if e.kind() != io::ErrorKind::BrokenPipe {
            say(&format!("cannot write to stdout: {e}"));
        }
        exit(2);
    }
    exit(code);
}
