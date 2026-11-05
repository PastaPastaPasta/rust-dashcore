// EMBARGO-1
//! The private sink for differential-test and fuzz artefacts (CS0 §9.3).
//!
//! Every L1 target and its proptest twin record a divergence here instead of printing or
//! panicking with its input. A run directory is `$DASH_EMBARGO_DIR/fuzz/<target>/runs/<run>/`
//! with `divergences/`, `crashes/`, `logs/` and `tmp/`.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::{
    PRIVATE_FILES_SUPPORTED, create_private_dir, embargo_dir, is_private_dir, report, sha256_hex,
    write_private_file,
};

/// The run id the runner passes to a fuzz target.
pub const RUN_VAR: &str = "NODE_FUZZ_RUN";
/// The pin the runner passes to a fuzz target (`v24.0`).
pub const PIN_VAR: &str = "NODE_FUZZ_PIN";

/// Why the sink could not be used. The messages name no input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SinkError {
    /// `DASH_EMBARGO_DIR` is unset.
    Unset,
    /// This platform cannot keep private files ([`PRIVATE_FILES_SUPPORTED`]).
    Unsupported,
    /// The private tree's `fuzz/` directory is missing or not private.
    NotPrivate,
    /// A run id that is not a plain name.
    BadRunId,
    /// A write failed.
    Write,
}

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unset => "DASH_EMBARGO_DIR is unset",
            Self::Unsupported => "this platform cannot keep private files (Unix only)",
            Self::NotPrivate => "the private fuzz directory is missing or not mode 0700",
            Self::BadRunId => "the run id is not a plain name",
            Self::Write => "a write to the private sink failed",
        })
    }
}

impl std::error::Error for SinkError {}

/// `$DASH_EMBARGO_DIR/fuzz/<target>`.
pub fn target_dir(root: &Path, target: &str) -> PathBuf {
    root.join("fuzz").join(target)
}

fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && name != "."
        && name != ".."
}

/// One run's private directory for one target.
#[derive(Debug)]
pub struct Sink {
    target: String,
    run_dir: PathBuf,
}

impl Sink {
    /// The run directory of `target`'s run `run_id`, checked but not created.
    fn run_dir_for(target: &str, run_id: &str) -> Result<(PathBuf, PathBuf), SinkError> {
        let root = embargo_dir().ok_or(SinkError::Unset)?;
        if !PRIVATE_FILES_SUPPORTED {
            return Err(SinkError::Unsupported);
        }
        if !plain_name(target) || !plain_name(run_id) {
            return Err(SinkError::BadRunId);
        }
        if !is_private_dir(&root.join("fuzz")) {
            return Err(SinkError::NotPrivate);
        }
        let target_dir = target_dir(&root, target);
        let run_dir = target_dir.join("runs").join(run_id);
        Ok((target_dir, run_dir))
    }

    /// Opens (creating it if needed) the run directory of `target`'s run `run_id`.
    pub fn open(target: &str, run_id: &str) -> Result<Sink, SinkError> {
        let (_, run_dir) = Sink::run_dir_for(target, run_id)?;
        for sub in ["divergences", "crashes", "logs", "tmp"] {
            create_private_dir(&run_dir.join(sub)).map_err(|_| SinkError::Write)?;
        }
        Ok(Sink {
            target: target.to_owned(),
            run_dir,
        })
    }

    pub fn run_dir(&self) -> &Path {
        &self.run_dir
    }

    /// Records a divergence: the input goes to `divergences/<sha256>` with a sidecar naming the
    /// target, pin and field, and one line (target, the first 12 hex digits, field) goes to
    /// stderr. Returns that 12-digit id.
    pub fn record(&self, input: &[u8], pin: &str, field: &str) -> Result<String, SinkError> {
        self.record_with_cmd(input, pin, field, None)
    }

    /// As [`Sink::record`], with the message command a model target's divergence happened on (a
    /// `cmd` line in the sidecar, which keys fuzzctl's clusters). The command is a name from a
    /// closed set (`[a-z0-9_.-]`, at most 24 bytes), never input bytes; anything else is
    /// refused as a write error.
    pub fn record_with_cmd(
        &self,
        input: &[u8],
        pin: &str,
        field: &str,
        cmd: Option<&str>,
    ) -> Result<String, SinkError> {
        if cmd.is_some_and(|c| !is_command_name(c)) {
            return Err(SinkError::Write);
        }
        let digest = sha256_hex(input);
        let dir = self.run_dir.join("divergences");
        write_private_file(&dir.join(&digest), input).map_err(|_| SinkError::Write)?;
        let mut sidecar = format!("target {}\npin {pin}\nfield {field}\n", self.target);
        if let Some(cmd) = cmd {
            sidecar.push_str(&format!("cmd {cmd}\n"));
        }
        write_private_file(&dir.join(format!("{digest}.txt")), sidecar.as_bytes())
            .map_err(|_| SinkError::Write)?;
        let id = digest[..12].to_owned();
        report(&format!("{}: divergence {id} in {field} ({pin})", self.target));
        Ok(id)
    }
}

/// Whether `name` is a command name a sidecar may carry: lower-case letters, digits, `_`, `.`
/// and `-`, 1 to 24 bytes (what run.sh and fuzzctl accept).
pub fn is_command_name(name: &str) -> bool {
    (1..=24).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}

/// Records a failing test input (a property, a twin or a boundary row) in a sink run of its own
/// when `DASH_EMBARGO_DIR` is set. Returns what the failure message may say about it: the sink
/// id, the sink error, or that it was not recorded.
pub fn record_for_test(test: &str, input: &[u8], pin: &str, field: &str) -> String {
    if embargo_dir().is_none() {
        return "not recorded: DASH_EMBARGO_DIR is unset".to_owned();
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    match Sink::open(test, &format!("test-{}-{nanos}", std::process::id())) {
        Ok(sink) => sink.record(input, pin, field).unwrap_or_else(|e| e.to_string()),
        Err(e) => e.to_string(),
    }
}

/// libFuzzer flags the runner passes (`node-fuzz/run.sh`) and the flags libFuzzer itself adds
/// when it re-runs the target for `-merge` and `-minimize_crash`. Each may appear at most once.
/// `stop_file` ends a certify window at libFuzzer's loop boundary (DEC-186 amendment 3); fork
/// mode passes it to its children too, but their other flags (`fork`, `reload`, `features_dir`)
/// and temporary corpus stay refused. `shuffle` and `prefer_small` are off in a fixed-order
/// replay.
const ALLOWED_FLAGS: &[&str] = &[
    "artifact_prefix",
    "exact_artifact_path",
    "max_total_time",
    "max_len",
    "runs",
    "jobs",
    "workers",
    "timeout",
    "rss_limit_mb",
    "print_final_stats",
    "stop_file",
    "seed",
    "merge",
    "merge_control_file",
    "merge_inner",
    "minimize_crash",
    "minimize_crash_internal_step",
    "shuffle",
    "prefer_small",
];

/// The flags whose value is a path.
const PATH_FLAGS: &[&str] =
    &["artifact_prefix", "exact_artifact_path", "merge_control_file", "stop_file"];

/// Flags whose only accepted value is `1`.
const ONE_ONLY: &[&str] =
    &["merge", "merge_inner", "minimize_crash", "minimize_crash_internal_step"];

/// The flags of a merge and of libFuzzer's merge child (`merge_control_file` alone starts no
/// merge; it is listed defensively). libFuzzer's other merge form, `-set_cover_merge`, is not
/// allowlisted.
const MERGE_FLAGS: &[&str] = &["merge", "merge_inner", "merge_control_file"];

/// Flags whose only accepted value is `0`.
const ZERO_ONLY: &[&str] = &["shuffle", "prefer_small"];

/// Checks a fuzz process's arguments against the runner's shapes (CS0 §9.3). `args` excludes
/// the program name.
///
/// - Only allowlisted flags, each at most once (libFuzzer takes the last occurrence of a repeated
///   flag); `-artifact_prefix` must be the run's `crashes/`, `-exact_artifact_path` a path below
///   it and `-merge_control_file` a path below the run's `tmp/` (run.sh's `TMPDIR`, BL-350);
///   `-stop_file` and every positional path lie inside `target_dir` or `run_dir`.
/// - Merging (`-merge`, `-merge_inner` or `-merge_control_file`): the first positional, where
///   the merge writes, is `target_dir/corpus`; no other shape below applies.
/// - Fuzzing: the first positional, where libFuzzer writes new inputs, is `target_dir/corpus`,
///   or for the smoke's fuzzing runs the run's own sample of it, `run_dir/sample` (BL-333), so
///   that they never write the working corpus.
/// - Replaying (crash minimisation and its child runs, a replay job): every positional lies under
///   `target_dir/runs/`, and either `-runs=0` is given, which executes them once and mutates
///   nothing (a replay job's `input/`, a fixed-order replay's one directory per input), or there
///   is one positional and `is_file` says it is a regular file (the staged input a minimisation
///   starts from, the crash file its children replay), which libFuzzer executes rather than
///   fuzzes into (BL-350).
pub fn check_fuzz_args(
    args: &[String],
    target_dir: &Path,
    run_dir: &Path,
    is_file: impl Fn(&Path) -> bool,
) -> Result<(), &'static str> {
    let inside = |path: &str| {
        let path = Path::new(path);
        path.is_absolute()
            && path.components().all(|c| !matches!(c, std::path::Component::ParentDir))
            && (path.starts_with(target_dir) || path.starts_with(run_dir))
    };
    let below = |path: &str, dir: &str| {
        let (path, dir) = (Path::new(path), run_dir.join(dir));
        path != dir && path.starts_with(&dir)
    };
    let mut seen: Vec<&str> = Vec::new();
    let mut positional = Vec::new();
    let mut artifact_prefix = None;
    let mut runs = None;
    for arg in args {
        let Some(flag) = arg.strip_prefix('-') else {
            positional.push(arg.as_str());
            continue;
        };
        let (name, value) = flag.split_once('=').ok_or("a flag without a value")?;
        if !ALLOWED_FLAGS.contains(&name) {
            return Err("a flag the runner does not use");
        }
        if seen.contains(&name) {
            return Err("a repeated flag");
        }
        seen.push(name);
        if ONE_ONLY.contains(&name) && value != "1" {
            return Err("an internal flag with an unexpected value");
        }
        if ZERO_ONLY.contains(&name) && value != "0" {
            return Err("an order flag with an unexpected value");
        }
        if PATH_FLAGS.contains(&name) && !inside(value) {
            return Err("a path flag outside the private tree");
        }
        if name == "exact_artifact_path" && !below(value, "crashes") {
            return Err("the exact artifact path is not in the run's crashes directory");
        }
        if name == "merge_control_file" && !below(value, "tmp") {
            return Err("the merge control file is not in the run's tmp directory");
        }
        if name == "artifact_prefix" {
            artifact_prefix = Some(value);
        }
        if name == "runs" {
            runs = Some(value);
        }
    }
    let crashes = format!("{}/crashes/", run_dir.display());
    if artifact_prefix != Some(crashes.as_str()) {
        return Err("the artifact prefix is not the run's crashes directory");
    }
    if positional.iter().any(|p| !inside(p)) {
        return Err("a corpus path outside the private tree");
    }
    let corpus = target_dir.join("corpus");
    let first = positional.first().map(Path::new);
    if seen.iter().any(|f| MERGE_FLAGS.contains(f)) {
        return match first {
            Some(first) if first == corpus => Ok(()),
            _ => Err("a merge into a directory other than the private working corpus"),
        };
    }
    let replay = positional.iter().all(|p| Path::new(p).starts_with(target_dir.join("runs")))
        && (runs == Some("0") || matches!(positional[..], [one] if is_file(Path::new(one))));
    match first {
        Some(first) if first == corpus || first == run_dir.join("sample") => Ok(()),
        Some(_) if replay => Ok(()),
        _ => Err("the first corpus directory is not the private working corpus"),
    }
}

/// The start-up check of every L1 fuzz target, run in libFuzzer's initialisation hook
/// (`fuzz_target!(init: …)`) before any input executes, so refusing never dumps an input. It
/// exits the process with one fixed line unless the runner started the target: the private tree
/// set and private, a run id and a pin, arguments of the runner's shapes, the working directory
/// inside the run's `logs/`, stderr redirected, and no `RUST_LIBFUZZER_DEBUG_PATH` (which makes
/// libfuzzer-sys write each input's `Debug` to a file and skip the target). Returns the run's
/// sink and the pin.
pub fn fuzz_init(target: &str) -> (Sink, String) {
    match fuzz_init_checked(target) {
        Ok(started) => started,
        Err(reason) => {
            report(&format!(
                "{target}: refusing to run: {reason}. Run L1 targets only through node-fuzz/run.sh (CS0 §9.3)."
            ));
            std::process::exit(2);
        }
    }
}

fn fuzz_init_checked(target: &str) -> Result<(Sink, String), String> {
    use std::io::IsTerminal as _;

    if std::env::var_os("RUST_LIBFUZZER_DEBUG_PATH").is_some() {
        return Err("RUST_LIBFUZZER_DEBUG_PATH is set".into());
    }
    if std::io::stderr().is_terminal() || std::io::stdout().is_terminal() {
        return Err("stdout or stderr is a terminal".into());
    }
    let run_id = std::env::var(RUN_VAR).map_err(|_| "NODE_FUZZ_RUN is unset")?;
    let pin = std::env::var(PIN_VAR).map_err(|_| "NODE_FUZZ_PIN is unset")?;
    if pin != "v24.0" {
        return Err("NODE_FUZZ_PIN is not v24.0".into());
    }
    let (target_dir, run_dir) = Sink::run_dir_for(target, &run_id).map_err(|e| e.to_string())?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    check_fuzz_args(&args, &target_dir, &run_dir, |path| {
        path.metadata().is_ok_and(|m| m.is_file())
    })?;
    let cwd = std::env::current_dir().map_err(|_| "the working directory is unreadable")?;
    if !cwd.starts_with(run_dir.join("logs")) {
        return Err("the working directory is not the run's logs directory".into());
    }
    let sink = Sink::open(target, &run_id).map_err(|e| e.to_string())?;
    Ok((sink, pin))
}

/// A proptest seed whose `Debug` prints a fixed placeholder, so a failing property or twin never
/// prints a value that reproduces its input (CS0 §9.5).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OpaqueSeed(pub u64);

impl fmt::Debug for OpaqueSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<opaque seed>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::expect_used)]
    fn a_sidecar_names_the_command_only_when_there_is_one() {
        let dir = std::env::temp_dir().join(format!("node-sink-cmd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        create_private_dir(&dir.join("divergences")).expect("test dir");
        let sink = Sink {
            target: "p2p_decode_model".to_owned(),
            run_dir: dir.clone(),
        };
        let bare = sink.record(b"one", "v24.0", "log line").expect("records");
        let with =
            sink.record_with_cmd(b"two", "v24.0", "log line", Some("qgetdata")).expect("records");
        let read = |input: &[u8]| {
            std::fs::read_to_string(
                dir.join("divergences").join(format!("{}.txt", sha256_hex(input))),
            )
            .expect("sidecar")
        };
        assert_eq!(read(b"one"), "target p2p_decode_model\npin v24.0\nfield log line\n");
        assert_eq!(
            read(b"two"),
            "target p2p_decode_model\npin v24.0\nfield log line\ncmd qgetdata\n"
        );
        assert_ne!(bare, with);
        for bad in ["", "Ping", "a b", "x\ny", "a/b", &"a".repeat(25)] {
            assert!(sink.record_with_cmd(b"three", "v24.0", "f", Some(bad)).is_err(), "{bad:?}");
        }
        assert!(!dir.join("divergences").join(sha256_hex(b"three")).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    const T: &str = "/p/fuzz/t";
    const R: &str = "/p/fuzz/t/runs/1";

    /// The regular files of the tests' tree: an earlier run's crash, the input a minimisation
    /// stages and the crash file its children replay.
    const FILES: &[&str] = &[
        "/p/fuzz/t/runs/0/crashes/crash-abc",
        "/p/fuzz/t/runs/1/input/input-1",
        "/p/fuzz/t/runs/1/crashes/minimized",
    ];

    fn check(list: &[&str]) -> Result<(), &'static str> {
        check_fuzz_args(&args(list), Path::new(T), Path::new(R), |path| {
            FILES.iter().any(|f| Path::new(f) == path)
        })
    }

    #[test]
    fn the_runner_shapes_pass() {
        let prefix = "-artifact_prefix=/p/fuzz/t/runs/1/crashes/";
        // Fuzzing, with workers.
        assert_eq!(
            check(&[
                prefix,
                "-max_total_time=60",
                "-jobs=4",
                "-workers=4",
                "-print_final_stats=1",
                "/p/fuzz/t/corpus",
                "/p/fuzz/t/seeds",
                "/p/fuzz/t/public-seeds",
            ]),
            Ok(())
        );
        // A smoke's fuzzing run, from its own sample of the working corpus (BL-333).
        assert_eq!(
            check(&[
                prefix,
                "-max_total_time=86400",
                "-print_final_stats=1",
                "-stop_file=/p/fuzz/t/runs/1/logs/segment-1/stop",
                "/p/fuzz/t/runs/1/sample",
                "/p/fuzz/t/seeds",
                "/p/fuzz/t/regress/fixed",
            ]),
            Ok(())
        );
        // A worker of -jobs: the parent's flags without jobs and workers.
        assert_eq!(
            check(&[prefix, "-max_total_time=60", "/p/fuzz/t/corpus", "/p/fuzz/t/seeds"]),
            Ok(())
        );
        // A certify window, which its stop file ends.
        assert_eq!(
            check(&[
                prefix,
                "-max_total_time=46800",
                "-print_final_stats=1",
                "-stop_file=/p/fuzz/t/runs/1/logs/segment-3/stop",
                "/p/fuzz/t/corpus",
            ]),
            Ok(())
        );
        // Merging, and libFuzzer's merge child.
        assert_eq!(check(&[prefix, "-merge=1", "/p/fuzz/t/corpus", "/p/fuzz/t/seeds"]), Ok(()));
        assert_eq!(
            check(&[
                prefix,
                "-merge_control_file=/p/fuzz/t/runs/1/tmp/libFuzzerTemp.Merge.txt",
                "-merge_inner=1",
                "/p/fuzz/t/corpus",
                "/p/fuzz/t/seeds",
            ]),
            Ok(())
        );
        // Minimising a crash of an earlier run, and libFuzzer's two child shapes.
        let crash = "/p/fuzz/t/runs/0/crashes/crash-abc";
        let exact = "-exact_artifact_path=/p/fuzz/t/runs/1/crashes/minimized";
        assert_eq!(check(&[prefix, "-minimize_crash=1", "-runs=10000", exact, crash]), Ok(()));
        assert_eq!(check(&[prefix, "-runs=10000", crash]), Ok(()));
        assert_eq!(
            check(&[prefix, "-runs=10000", "-minimize_crash_internal_step=1", exact, crash]),
            Ok(())
        );
        // run.sh's minimisation, from the input it stages, and a child replaying its result.
        let staged = "/p/fuzz/t/runs/1/input/input-1";
        assert_eq!(check(&[prefix, "-minimize_crash=1", "-runs=10000", exact, staged]), Ok(()));
        let minimized = "/p/fuzz/t/runs/1/crashes/minimized";
        assert_eq!(
            check(&[prefix, "-runs=10000", "-minimize_crash_internal_step=1", exact, minimized]),
            Ok(())
        );
        // A replay job, and a fixed-order one.
        assert_eq!(
            check(&[prefix, "-runs=0", "-print_final_stats=1", "/p/fuzz/t/runs/1/input"]),
            Ok(())
        );
        assert_eq!(
            check(&[
                prefix,
                "-runs=0",
                "-print_final_stats=1",
                "-shuffle=0",
                "-prefer_small=0",
                "/p/fuzz/t/runs/1/input/1",
                "/p/fuzz/t/runs/1/input/2",
            ]),
            Ok(())
        );
    }

    #[test]
    fn other_shapes_fail() {
        let prefix = "-artifact_prefix=/p/fuzz/t/runs/1/crashes/";
        for bad in [
            &["/p/fuzz/t/corpus"][..],
            &["-artifact_prefix=./", "/p/fuzz/t/corpus"],
            &["-artifact_prefix=/p/fuzz/t/runs/1/", "/p/fuzz/t/corpus"],
            &[prefix, "-artifact_prefix=./", "/p/fuzz/t/corpus"],
            &[prefix, "-fork=4", "/p/fuzz/t/corpus"],
            &[prefix, "-stop_file=/tmp/stop", "/p/fuzz/t/corpus"],
            &[prefix, "-reload=0", "-stop_file=/p/fuzz/t/runs/1/stop", "/p/fuzz/t/corpus"],
            &[prefix, "-merge_inner=2", "/p/fuzz/t/corpus"],
            &[prefix, "-exact_artifact_path=/home/x/crash", "/p/fuzz/t/corpus"],
            &[prefix, "-merge_control_file=/tmp/m.txt", "-merge_inner=1", "/p/fuzz/t/corpus"],
            &[prefix, "/p/fuzz/t/seeds", "/p/fuzz/t/corpus"],
            &[prefix, "/work/elsewhere/corpus"],
            &[prefix, "/p/fuzz/t/corpus", "/p/fuzz/t/../../other"],
            &[prefix, "/p/fuzz/t/corpus", "relative/dir"],
            &[prefix, "/p/fuzz/t/runs/0/crashes/crash-abc", "/p/fuzz/t/corpus"],
            &[prefix, "/p/fuzz/t/runs/1/input/1", "/p/fuzz/t/seeds"],
            &[prefix, "/p/fuzz/t/runs/0/sample", "/p/fuzz/t/seeds"],
            &[prefix, "/p/fuzz/t/runs/1/sample/x", "/p/fuzz/t/seeds"],
            &[prefix, "-merge=1", "/p/fuzz/t/runs/1/sample", "/p/fuzz/t/seeds"],
            // A merge never writes anywhere but the working corpus, whatever its other shapes
            // allow: not the run's sample/ alone, nor run directories with -runs=0 (BL-333).
            &[prefix, "-merge=1", "/p/fuzz/t/runs/1/sample"],
            &[prefix, "-merge_inner=1", "/p/fuzz/t/runs/1/sample"],
            &[
                prefix,
                "-merge_control_file=/p/fuzz/t/runs/1/tmp/libFuzzerTemp.Merge.txt",
                "-merge_inner=1",
                "/p/fuzz/t/runs/1/sample",
            ],
            &[prefix, "-merge_control_file=/p/fuzz/t/runs/1/tmp/m.txt", "/p/fuzz/t/runs/1/sample"],
            &[prefix, "-merge=1", "-runs=0", "/p/fuzz/t/runs/1/sample", "/p/fuzz/t/runs/0/input"],
            &[prefix, "-merge=1", "-runs=0", "/p/fuzz/t/runs/0/input", "/p/fuzz/t/runs/1/input"],
            &[prefix, "-merge=1", "/p/fuzz/t/runs/0/crashes/crash-abc"],
            &[prefix, "-set_cover_merge=1", "/p/fuzz/t/corpus", "/p/fuzz/t/seeds"],
            &[prefix, "-shuffle=1", "/p/fuzz/t/runs/1/input"],
            &[prefix, "-prefer_small=1", "/p/fuzz/t/runs/1/input"],
            &[prefix, "-shuffle=0", "-shuffle=0", "/p/fuzz/t/runs/1/input"],
            &[prefix, "/p/fuzz/t/runs/1/input/1", "/p/fuzz/t/runs/1/input/2"],
            &[prefix, "-runs=10", "/p/fuzz/t/runs/1/input/1", "/p/fuzz/t/runs/1/input/2"],
            // One path under runs/ without -runs=0 is a file libFuzzer executes, never a
            // directory it would fuzz into, and never a file that is missing (BL-350).
            &[prefix, "/p/fuzz/t/runs/1/input"],
            &[prefix, "-runs=10000", "/p/fuzz/t/runs/1/input"],
            &[prefix, "-max_total_time=60", "/p/fuzz/t/runs/0/crashes"],
            &[prefix, "-minimize_crash=1", "-runs=10000", "/p/fuzz/t/runs/1/input/input-2"],
            // The exact artifact path is below the run's crashes/, the merge control file below
            // its tmp/ (BL-350).
            &[
                prefix,
                "-exact_artifact_path=/p/fuzz/t/corpus/x",
                "/p/fuzz/t/runs/0/crashes/crash-abc",
            ],
            &[
                prefix,
                "-minimize_crash=1",
                "-runs=10000",
                "-exact_artifact_path=/p/fuzz/t/runs/0/crashes/minimized",
                "/p/fuzz/t/runs/0/crashes/crash-abc",
            ],
            &[
                prefix,
                "-minimize_crash=1",
                "-runs=10000",
                "-exact_artifact_path=/p/fuzz/t/runs/1/crashes",
                "/p/fuzz/t/runs/0/crashes/crash-abc",
            ],
            &[
                prefix,
                "-merge_control_file=/p/fuzz/t/corpus/m.txt",
                "-merge_inner=1",
                "/p/fuzz/t/corpus",
            ],
            &[
                prefix,
                "-merge_control_file=/p/fuzz/t/runs/0/tmp/m.txt",
                "-merge=1",
                "/p/fuzz/t/corpus",
            ],
            &[prefix, "-merge_control_file=/p/fuzz/t/runs/1/tmp", "-merge=1", "/p/fuzz/t/corpus"],
            &[
                prefix,
                "-merge_control_file=/p/fuzz/t/runs/1/logs/m.txt",
                "-merge=1",
                "/p/fuzz/t/corpus",
            ],
            &[prefix],
        ] {
            assert!(check(bad).is_err(), "{}", bad.join(" "));
        }
    }

    #[test]
    #[allow(clippy::use_debug)]
    fn opaque_seeds_print_a_placeholder() {
        assert_eq!(format!("{:?}", OpaqueSeed(42)), "<opaque seed>");
    }
}
