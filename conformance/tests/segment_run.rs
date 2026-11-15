//! `conformance/scripts/segment-run.sh` (O9b S4) over a stub `dash-conformance`: the input must
//! be a whole selection (its `BUDGET` line last), on one chain; every bundle is opened before any
//! window runs (exit 2 on one that fails or is not the RUN line's); each window runs `digest l2`
//! from its bundle's snapshot with a reverse leg to the base, then `digest compare` over
//! base+1..=to, and passes only when l2 exits 0, the compare 4 and the compare lists every evo
//! key at 0 pairs on the log's side. A log that exists is refused.
//!
//! One test: the stub is executed while it is a new file, which a concurrent test's fork could
//! hold open for writing (ETXTBSY).
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

const SCRIPT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/segment-run.sh");

/// Logs its arguments and answers as the case says: `segment open` prints the bundle's `open`
/// file, the compare lists the excluded keys with `mnl` at `$STUB_MNL` pairs.
const STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_ARGS"
case "$1 $2" in
"segment open") cat "$3/open" 2>/dev/null || { echo "no bundle"; exit 1; } ;;
"digest l2") exit "$STUB_L2" ;;
"digest compare")
    echo "excluded, not compared (pairs holding the key: left, right):"
    for k in muhash mnl qc cp ehf; do
        n=0; [ "$k" = mnl ] && n=$STUB_MNL
        printf '  %-14s %s, 7\n' "C.s.$k" "$n"
    done
    exit "$STUB_COMPARE" ;;
*) exit 99 ;;
esac
"#;

/// A bundle directory `id` under `root` that `segment open` reports as `opened`.
fn bundle(root: &Path, id: &str, opened: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("open"), format!("{opened}\n")).unwrap();
}

/// How the stub answers.
#[derive(Clone, Copy)]
struct Stub {
    l2: i32,
    compare: i32,
    mnl: u32,
}

const PASS: Stub = Stub {
    l2: 0,
    compare: 4,
    mnl: 0,
};

/// Runs the script with `input` on stdin, the stub answering as `stub`, in a new out dir `name`:
/// its status, stdout and the stub's calls.
fn run(root: &Path, name: &str, input: &str, stub: Stub) -> (i32, String, String) {
    let out = root.join(name);
    fs::create_dir_all(&out).unwrap();
    let args = root.join(format!("{name}.args"));
    let mut child = Command::new(SCRIPT)
        .args([
            root.join("stub").to_str().unwrap(),
            out.to_str().unwrap(),
            "/blocks",
            "/headers.bin",
            "/golden.jsonl.zst",
            root.join("bundles").to_str().unwrap(),
        ])
        .env("DASH_EMBARGO_DIR", root)
        .env("STUB_ARGS", &args)
        .env("STUB_L2", stub.l2.to_string())
        .env("STUB_COMPARE", stub.compare.to_string())
        .env("STUB_MNL", stub.mnl.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write as _;
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    let calls = fs::read_to_string(&args).unwrap_or_default();
    (output.status.code().unwrap(), String::from_utf8(output.stdout).unwrap(), calls)
}

#[test]
fn segment_run_runs_each_window_from_its_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let stub = root.join("stub");
    fs::write(&stub, STUB).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let bundles = root.join("bundles");
    bundle(&bundles, "main-10", "BUNDLE main 10");
    bundle(&bundles, "main-50", "BUNDLE main 50");
    let sel =
        "RUN main-10 main 10 30\nNIGHTLY main-99 (over)\nRUN main-50 main 50 70\nBUDGET 2 of 3 s\n";

    let (status, stdout, calls) = run(root, "ok", sel, PASS);
    assert_eq!(status, 0, "{stdout}");
    let windows: Vec<_> =
        stdout.lines().map(|l| l.split(' ').take(3).collect::<Vec<_>>()).collect();
    assert_eq!(windows, [["WINDOW", "main-10", "0"], ["WINDOW", "main-50", "0"]]);
    let calls: Vec<_> = calls.lines().collect();
    assert_eq!(calls.len(), 6, "{calls:?}");
    assert_eq!(calls[0], format!("segment open {}/main-10", bundles.display()));
    assert_eq!(calls[1], format!("segment open {}/main-50", bundles.display()));
    assert!(calls[2].starts_with("digest l2 --chain main"), "{}", calls[2]);
    assert!(calls[2].contains("--to 30 "), "{}", calls[2]);
    assert!(calls[2].contains("--reverse-to 10 "), "{}", calls[2]);
    let start = format!("--start-utxo {}/main-10/utxo.dat", bundles.display());
    assert!(calls[2].ends_with(&start), "{}", calls[2]);
    assert!(calls[3].starts_with("digest compare --mode linear --no-dump"), "{}", calls[3]);
    assert!(calls[3].contains("--exclude muhash,mnl,qc,cp,ehf --from 11 --to 30 "), "{}", calls[3]);
    assert!(calls[5].contains("--from 51 --to 70 "), "{}", calls[5]);
    for f in ["main-10.log", "main-10.compare.log", "main-50.log", "main-50.compare.log"] {
        let text = fs::read_to_string(root.join("ok").join(f)).unwrap();
        assert!(text.trim_end().ends_with("EXIT 0") || text.trim_end().ends_with("EXIT 4"), "{f}");
    }

    // Logs that exist are refused, never read.
    let (status, stdout, _) = run(root, "ok", sel, PASS);
    assert_eq!(status, 1);
    assert!(stdout.contains("exists"), "{stdout}");

    // A compare that is not 4, an l2 that fails, or an evo key the log holds fails its window
    // and the run.
    for (name, stub, want) in [
        (
            "clean",
            Stub {
                compare: 0,
                ..PASS
            },
            "WINDOW main-10 compare",
        ),
        (
            "three",
            Stub {
                compare: 3,
                ..PASS
            },
            "WINDOW main-10 compare",
        ),
        (
            "l2",
            Stub {
                l2: 1,
                ..PASS
            },
            "WINDOW main-10 1",
        ),
        (
            "evo",
            Stub {
                mnl: 3,
                ..PASS
            },
            "WINDOW main-10 evo",
        ),
    ] {
        let (status, stdout, calls) = run(root, name, sel, stub);
        assert_eq!(status, 1, "{name}");
        assert!(stdout.contains(want), "{name}: {stdout}");
        if name == "l2" {
            assert!(!calls.contains("compare"), "no compare after a failed l2");
        }
        if name == "evo" {
            assert!(stdout.contains("holds mnl at 3 pairs"), "{stdout}");
        }
    }

    // Not a whole selection, mixed chains, a malformed line: refused before anything runs.
    for (name, input) in [
        ("cut", "RUN main-10 main 10 30\n"),
        ("empty", ""),
        ("chains", "RUN main-10 main 10 30\nRUN test-5 test 5 9\nBUDGET 2 of 3 s\n"),
        ("malformed", "RUN main-10 main 10\nBUDGET 1 of 3 s\n"),
    ] {
        let (status, _, calls) = run(root, name, input, PASS);
        assert_eq!(status, 1, "{name}");
        assert_eq!(calls, "", "{name}");
    }
    // A selection with nothing to run passes.
    let (status, stdout, calls) =
        run(root, "none", "DEFERRED x (phase B)\nBUDGET 0 of 3 s\n", PASS);
    assert_eq!((status, stdout.as_str(), calls.as_str()), (0, "", ""));

    // A bundle that does not open, or is not the RUN line's: exit 2 before any window runs.
    bundle(&bundles, "main-60", "BUNDLE main 61");
    for (name, input) in [
        ("missing", "RUN main-10 main 10 30\nRUN main-40 main 40 60\nBUDGET 2 of 3 s\n"),
        ("base", "RUN main-10 main 10 30\nRUN main-60 main 60 80\nBUDGET 2 of 3 s\n"),
    ] {
        let (status, stdout, calls) = run(root, name, input, PASS);
        assert_eq!(status, 2, "{name}: {stdout}");
        assert!(!calls.contains("digest"), "{name}: no window runs: {calls}");
    }
}
