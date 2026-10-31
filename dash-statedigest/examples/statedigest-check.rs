//! Checks that one node's three StateDigest files belong together (O8 spec §7.3, §9.2, §13.1).
//!
//! Usage: `statedigest-check <digest.jsonl> <decisions.jsonl> <golden.jsonl>`. Each file must parse
//! strictly, start with `H` and hold only the record kinds of its file; the digest and decisions
//! must join run by run and attempt by attempt; and the golden must be the digest's last tip
//! connects, block by block, plus the golden-only keys (see `dash_statedigest::recording`).
//! On success one JSON object is printed with the record counts per file and kind, for example
//! `{"decisions":{"A":430,"H":4},"digest":{"C":213,"D":18,"H":4,"R":217},"golden":{...}}`, and
//! the exit code is 0. On the first violation it is printed with its file and line and the exit
//! code is 1.

use std::collections::BTreeMap;
use std::process::ExitCode;

use dash_statedigest::recording::{Tally, check_recording};

fn json(tally: &Tally) -> String {
    let counts: Vec<String> = tally.iter().map(|(k, n)| format!("\"{k}\":{n}")).collect();
    format!("{{{}}}", counts.join(","))
}

fn run(paths: &[String]) -> Result<String, String> {
    let [digest, decisions, golden] = paths else {
        return Err(
            "usage: statedigest-check <digest.jsonl> <decisions.jsonl> <golden.jsonl>".into()
        );
    };
    let read = |path: &String| std::fs::read(path).map_err(|e| format!("{path}: {e}"));
    let counts = check_recording(&read(digest)?, &read(decisions)?, &read(golden)?)?;
    let files: BTreeMap<&str, &Tally> =
        [("decisions", &counts.decisions), ("digest", &counts.digest), ("golden", &counts.golden)]
            .into_iter()
            .collect();
    let files: Vec<String> = files.iter().map(|(f, t)| format!("\"{f}\":{}", json(t))).collect();
    Ok(format!("{{{}}}", files.join(",")))
}

fn main() -> ExitCode {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    match run(&paths) {
        Ok(counts) => {
            println!("{counts}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
