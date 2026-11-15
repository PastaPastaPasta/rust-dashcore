//! One process per measured pass (rev5 N4). The supervisor (an unfiltered test, which never
//! masks or guards) re-executes its own test binary with the pass's child test, `AMB_PASS`,
//! one inherited pipe as `AMB_RECORD_FD`, `LD_BIND_NOW=1` and the revision; it drains the pipe
//! while the child runs and waits for it. The child builds its pass, measures, writes one record
//! with a single `write(2)` ([`finish`]) and exits 0 without unwinding. A child that dies (the
//! guard's SIGSYS included), exits non-zero, or writes nothing, a short, long or malformed record
//! is a refusal with no number.

use std::io::Read as _;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::record::MAX_RECORD;
use crate::{Refusal, Result, refuse};

pub const PASS_ENV: &str = "AMB_PASS";
pub const RECORD_FD_ENV: &str = "AMB_RECORD_FD";
pub const REVISION_ENV: &str = "AMB_REVISION";
/// Set to make a test without a PMU fail instead of skipping.
pub const REQUIRE_PMU_ENV: &str = "AMB_REQUIRE_PMU";

/// This process's pass, when it is a child.
pub fn pass() -> Option<String> {
    std::env::var(PASS_ENV).ok()
}

/// The child's revision, from its supervisor.
pub fn child_revision() -> String {
    std::env::var(REVISION_ENV).unwrap_or_else(|_| "unknown".into())
}

/// `git rev-parse HEAD` of this checkout, with `-dirty` for tracked changes or an untracked
/// Cargo configuration file in the checkout, or `-stale` when the harness was built from
/// another commit or a tracked file changed after this binary was built (relink to clear it,
/// e.g. by touching `node-amb/build.rs`).
pub fn revision() -> String {
    let dir = env!("CARGO_MANIFEST_DIR");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
    };
    let Some(head) = git(&["rev-parse", "HEAD"]) else {
        return "unknown".into();
    };
    if head != env!("AMB_BUILD_REVISION") || newer_than_binary(&git) {
        return format!("{head}-stale");
    }
    match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(s) if s.is_empty() && !untracked_cargo_config(&git) => head,
        _ => format!("{head}-dirty"),
    }
}

/// Whether a `.cargo/config{,.toml}` that git does not track sits in the checkout, in a
/// directory from this crate up to the top: Cargo reads it, so it can patch or replace the
/// code a clean revision names.
fn untracked_cargo_config(git: &dyn Fn(&[&str]) -> Option<String>) -> bool {
    let Some(root) = git(&["rev-parse", "--show-toplevel"]) else {
        return true;
    };
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    dir.ancestors().take_while(|d| d.starts_with(&root)).any(|d| {
        ["config", "config.toml"].iter().any(|name| {
            let path = d.join(".cargo").join(name);
            path.exists()
                && git(&["ls-files", "--error-unmatch", &path.display().to_string()]).is_none()
        })
    })
}

/// Whether a tracked file was modified after the running binary (or that cannot be told).
fn newer_than_binary(git: &dyn Fn(&[&str]) -> Option<String>) -> bool {
    let built = std::env::current_exe().and_then(|p| p.metadata()?.modified());
    let (Ok(built), Some(root), Some(files)) =
        (built, git(&["rev-parse", "--show-toplevel"]), git(&["ls-files"]))
    else {
        return true;
    };
    files.lines().any(|f| {
        std::fs::metadata(Path::new(&root).join(f))
            .and_then(|m| m.modified())
            .is_ok_and(|m| m > built)
    })
}

/// The child's end: one `write(2)` of `record` to the record pipe, then `_exit(0)`. A failed or
/// short write exits 3.
pub fn finish(record: &[u8]) -> ! {
    let fd: i32 = match std::env::var(RECORD_FD_ENV).ok().and_then(|v| v.parse().ok()) {
        Some(fd) => fd,
        None => die("no record descriptor"),
    };
    // SAFETY: writes `record`'s bytes to the inherited pipe.
    let n = unsafe { libc::write(fd, record.as_ptr().cast(), record.len()) };
    // SAFETY: _exit ends the process without running destructors (no teardown under the guard).
    unsafe {
        libc::_exit(if n == record.len() as isize {
            0
        } else {
            3
        })
    }
}

/// The child refuses: `why` to stderr, then `_exit(2)`.
pub fn die(why: &str) -> ! {
    eprintln!("AMB CHILD REFUSED: {why}");
    // SAFETY: as in `finish`.
    unsafe { libc::_exit(2) }
}

/// A test that needs a PMU and has none: skipped, unless [`REQUIRE_PMU_ENV`] is set.
pub fn no_pmu(test: &str) {
    assert!(
        std::env::var_os(REQUIRE_PMU_ENV).is_none(),
        "{test}: no PMU and {REQUIRE_PMU_ENV} set"
    );
    eprintln!(
        "SKIP {test}: no perf_event_open for this process (perf_event_paranoid > 2 or no PMU)"
    );
}

/// A running child.
pub struct Child {
    proc: std::process::Child,
    reader: JoinHandle<(Vec<u8>, usize)>,
    log: PathBuf,
}

/// A finished child.
#[derive(Debug)]
pub struct ChildRun {
    pub status: ExitStatus,
    /// The child outlived [`Child::wait_within`]'s limit and was killed.
    pub timed_out: bool,
    /// The record's bytes, at most [`MAX_RECORD`] + 1 kept.
    pub record: Vec<u8>,
    /// Bytes the child wrote to the pipe in all.
    pub written: usize,
    /// The child's stdout and stderr.
    pub log: String,
}

fn log_path() -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "amb-child-{}-{}.log",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ))
}

impl Child {
    /// Starts `test` (a test name of this binary, run exactly) as pass `pass`, with `env`.
    pub fn spawn(test: &str, pass: &str, env: &[(&str, String)]) -> std::io::Result<Child> {
        let mut fds = [0i32; 2];
        // SAFETY: fills two descriptors, close-on-exec.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: pipe2 returned two new descriptors we own.
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        let log = log_path();
        let out = std::fs::File::create(&log)?;
        let wfd = fds[1];
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.args([test, "--exact", "--nocapture", "--test-threads=1", "--include-ignored"])
            .env(PASS_ENV, pass)
            .env(RECORD_FD_ENV, wfd.to_string())
            .env(REVISION_ENV, revision())
            .env("LD_BIND_NOW", "1")
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out);
        for (k, v) in env {
            cmd.env(k, v);
        }
        // SAFETY: fcntl is async-signal-safe; the child keeps only the write end across exec
        // (the flag is per descriptor table, so no other child inherits it).
        unsafe {
            cmd.pre_exec(move || {
                if libc::fcntl(wfd, libc::F_SETFD, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let proc = cmd.spawn()?;
        drop(w);
        let reader = std::thread::spawn(move || {
            let mut pipe = std::fs::File::from(r);
            let (mut kept, mut total, mut chunk) = (Vec::new(), 0usize, [0u8; 4096]);
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        total += n;
                        let room = (MAX_RECORD + 1).saturating_sub(kept.len());
                        kept.extend_from_slice(&chunk[..n.min(room)]);
                    }
                }
            }
            (kept, total)
        });
        Ok(Child {
            proc,
            reader,
            log,
        })
    }

    pub fn pid(&self) -> u32 {
        self.proc.id()
    }

    pub fn wait(mut self) -> ChildRun {
        let status = self.proc.wait().expect("waiting for the child");
        self.finish(status, false)
    }

    /// [`Child::wait`], but a child still running after `limit` is killed (SIGKILL to its
    /// pid) and its run is a refusal.
    pub fn wait_within(mut self, limit: Duration) -> ChildRun {
        let deadline = Instant::now().checked_add(limit);
        loop {
            if let Some(status) = self.proc.try_wait().expect("waiting for the child") {
                return self.finish(status, false);
            }
            if deadline.is_some_and(|d| Instant::now() >= d) {
                let _ = self.proc.kill();
                let status = self.proc.wait().expect("waiting for the killed child");
                return self.finish(status, true);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn finish(self, status: ExitStatus, timed_out: bool) -> ChildRun {
        let (record, written) = self.reader.join().expect("the record reader");
        let log = std::fs::read_to_string(&self.log).unwrap_or_default();
        let _ = std::fs::remove_file(&self.log);
        ChildRun {
            status,
            timed_out,
            record,
            written,
            log,
        }
    }
}

/// The last lines of a child's log.
fn tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

impl ChildRun {
    /// The record, or the refusal the run is.
    pub fn record(self) -> Result<Vec<u8>> {
        let why = if self.timed_out {
            "the child timed out and was killed".into()
        } else if let Some(sig) = self.status.signal() {
            let name = if sig == libc::SIGSYS {
                " (SIGSYS: the guard)"
            } else {
                ""
            };
            format!("the child died by signal {sig}{name}")
        } else if !self.status.success() {
            format!("the child exited {}", self.status.code().unwrap_or(-1))
        } else if self.written == 0 {
            "the child wrote no record".into()
        } else if self.written > MAX_RECORD {
            format!("the child wrote {} record bytes, more than {MAX_RECORD}", self.written)
        } else {
            return Ok(self.record);
        };
        Err(Refusal(format!("{why}\n{}", tail(&self.log))))
    }
}

/// Runs `test` as pass `pass` in a fresh child; its pid and record.
pub fn run(test: &str, pass: &str, env: &[(&str, String)]) -> Result<(u32, Vec<u8>)> {
    match Child::spawn(test, pass, env) {
        Ok(c) => {
            let pid = c.pid();
            Ok((pid, c.wait().record()?))
        }
        Err(e) => refuse!("starting the child: {e}"),
    }
}

/// Writes `bytes` to `dir/name` when `AMB_OUT` names a directory; returns the path.
pub fn save(name: &str, bytes: &[u8]) -> Option<PathBuf> {
    let dir = std::env::var_os("AMB_OUT")?;
    let path = Path::new(&dir).join(name);
    std::fs::create_dir_all(&dir).expect("AMB_OUT");
    std::fs::write(&path, bytes).expect("writing a report");
    Some(path)
}
