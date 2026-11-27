//! Collect a mixed-network observation log, resuming an existing file.
//!
//! ```text
//! mixnet-watch --nodes <nodes.json> --out <obs.jsonl> [--events <path|->] [--poll-ms <n>] [--page-cmd <cmd>]
//! ```
//!
//! EOF on the event stream triggers one final pass and exit 0. Without a stream, collection
//! continues until killed. Divergences write `PAGE-<height>-<kind>.json` beside `--out` once and,
//! when `--page-cmd` is set, invoke it with the page path. Output write failures exit 1. Invalid
//! usage, input or logs exit 2.

use std::io::{self, Write};
use std::process::ExitCode;

#[cfg(unix)]
fn main() -> ExitCode {
    use dash_testkit::mixnet::watch::{self, Error};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn usage(message: &str) -> ! {
        let _ = writeln!(
            io::stderr().lock(),
            "mixnet-watch: {message}\nusage: mixnet-watch --nodes <nodes.json> --out <obs.jsonl> [--events <path|->] [--poll-ms <n>] [--page-cmd <cmd>]"
        );
        std::process::exit(2)
    }

    let (mut nodes, mut out, mut events, mut poll, mut page_cmd) =
        (None, None, None, None::<OsString>, None);
    let mut args = std::env::args_os().skip(1);
    while let Some(flag) = args.next() {
        let slot = match flag.to_str() {
            Some("--nodes") => &mut nodes,
            Some("--out") => &mut out,
            Some("--events") => &mut events,
            Some("--poll-ms") => &mut poll,
            Some("--page-cmd") => &mut page_cmd,
            _ => usage(&format!("unknown argument {flag:?}")),
        };
        let value = args
            .next()
            .filter(|v| !v.as_encoded_bytes().starts_with(b"-") || (flag == "--events" && v == "-"))
            .unwrap_or_else(|| usage(&format!("{flag:?} needs a value")));
        if slot.replace(value).is_some() {
            usage(&format!("{flag:?} given twice"));
        }
    }
    let nodes = nodes.unwrap_or_else(|| usage("--nodes is required"));
    let out = out.unwrap_or_else(|| usage("--out is required"));
    let ms = poll.map_or(500, |s| {
        s.to_str()
            .and_then(|s| s.parse().ok())
            .filter(|ms| *ms > 0)
            .unwrap_or_else(|| usage("invalid --poll-ms"))
    });
    match watch::run(
        Path::new(&nodes),
        Path::new(&out),
        events.map(PathBuf::from),
        Duration::from_millis(ms),
        page_cmd.as_deref(),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr().lock(), "mixnet-watch: {e}");
            ExitCode::from(if matches!(e, Error::Output(_)) {
                1
            } else {
                2
            })
        }
    }
}

#[cfg(not(unix))]
fn main() -> ExitCode {
    let _ = writeln!(io::stderr().lock(), "mixnet-watch requires Unix");
    ExitCode::from(2)
}
