//! The capability probes (CH7b-1 PLAN F2): the forbidden operations inside the crate, behind
//! `cap_probe` cfgs in `src/llmq/cap_probes.rs` and `src/chain/actor.rs`. Each probe is checked
//! with `cargo rustc --profile check` into a target directory of this test's own, and must fail
//! with its error code on its marked line and nowhere else; the twins, checked together, compile
//! cleanly. Outside the crate the same rules are `src/llmq/capability_rules.md`'s doctests.
#![cfg(not(any(miri, loom)))]

use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// A probe's cfg value, its file, and the error code it must fail with.
const PROBES: [(&str, &str, &str); 10] = [
    ("onv_fields", "src/llmq/cap_probes.rs", "E0451"),
    ("on_v", "src/llmq/cap_probes.rs", "E0624"),
    ("vcontext_fields", "src/llmq/cap_probes.rs", "E0451"),
    ("vcontext_new", "src/llmq/cap_probes.rs", "E0624"),
    ("handle_fields", "src/llmq/cap_probes.rs", "E0451"),
    ("applied_fields", "src/llmq/cap_probes.rs", "E0451"),
    ("sealed", "src/llmq/cap_probes.rs", "E0603"),
    ("binding_fields", "src/llmq/cap_probes.rs", "E0451"),
    ("on_v_then_attempt", "src/chain/actor.rs", "E0499"),
    ("attempt_then_on_v", "src/chain/actor.rs", "E0499"),
];

/// An error rustc reported: where, and its code (empty without one).
#[derive(Debug, PartialEq, Eq)]
struct Error {
    file: String,
    line: usize,
    code: String,
}

/// The lib checked with `cfgs` set: whether it compiled, its errors, and the whole of stderr.
fn check(cfgs: &[String]) -> (bool, Vec<Error>, String) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(["rustc", "-p", "dashd-chainstate", "--lib", "--profile", "check", "--offline"])
        .args(["--message-format", "short", "--"])
        .env("CARGO_TARGET_DIR", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cap-probes"))
        // The outer build's flags (`-D warnings` on CI) would make a dead probe an error.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS");
    for cfg in cfgs {
        cmd.arg("--cfg").arg(format!("cap_probe=\"{cfg}\""));
    }
    let out = cmd.output().expect("cargo runs");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let errors = stderr.lines().filter_map(error).collect();
    (out.status.success(), errors, stderr)
}

/// A line of stderr as an error: `path:line:col: error[CODE]: …` from rustc, or any other
/// `error` line but cargo's and rustc's closing summaries.
fn error(line: &str) -> Option<Error> {
    if let Some((at, rest)) = line.split_once(": error") {
        let mut parts = at.rsplitn(3, ':');
        let (_, row, file) = (parts.next(), parts.next(), parts.next());
        if let (Some(row), Some(file)) = (row.and_then(|r| r.parse().ok()), file) {
            let code = rest.strip_prefix('[').and_then(|c| c.split_once(']')).map(|(c, _)| c);
            return Some(Error {
                file: file.to_owned(),
                line: row,
                code: code.unwrap_or_default().to_owned(),
            });
        }
    }
    let summary = ["error: could not compile", "error: aborting due to"];
    (line.starts_with("error") && !summary.iter().any(|s| line.starts_with(s))).then(|| Error {
        file: String::new(),
        line: 0,
        code: line.to_owned(),
    })
}

/// The line of `file` marked `marker`.
fn marked(file: &str, marker: &str) -> usize {
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
        .expect("the probe's file");
    let mut lines = source.lines().enumerate().filter(|(_, l)| l.ends_with(marker));
    let (i, _) = lines.next().unwrap_or_else(|| panic!("{file} marks no line `{marker}`"));
    assert!(lines.next().is_none(), "{file} marks two lines `{marker}`");
    i + 1
}

/// Each probe fails on its marked line with its code and nothing else, and the twins compile.
#[test]
fn each_probe_fails_on_its_line_and_each_twin_compiles() {
    let start = Instant::now();
    for (name, file, code) in PROBES {
        let line = marked(file, &format!("// probe: {name}"));
        let (compiled, errors, stderr) = check(&[name.to_owned()]);
        assert!(!compiled, "probe {name} compiled");
        assert!(!errors.is_empty(), "probe {name} failed with no error:\n{stderr}");
        for e in &errors {
            let at = e.file.ends_with(file) && e.line == line && e.code == code;
            assert!(at, "probe {name}: {e:?}, not {code} at {file}:{line}:\n{stderr}");
        }
    }
    let twins: Vec<String> = PROBES.iter().map(|(name, ..)| format!("{name}_twin")).collect();
    let (compiled, errors, stderr) = check(&twins);
    assert!(compiled && errors.is_empty(), "the twins:\n{stderr}");
    eprintln!("{} probes and their twins checked in {:?}", PROBES.len(), start.elapsed());
}
