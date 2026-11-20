//! `isdump DB`: every row of a stopped copy of Core's `llmq/isdb`, as stable JSON on stdout.
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("usage: isdump DB");
        return ExitCode::from(2);
    };
    match isdump::dump(&PathBuf::from(path)) {
        Ok(dump) => {
            let mut out = std::io::stdout().lock();
            if out.write_all(dump.to_json().as_bytes()).and_then(|()| out.flush()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("isdump: {e}");
            ExitCode::FAILURE
        }
    }
}
