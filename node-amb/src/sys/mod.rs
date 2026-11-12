//! The kernel side (Linux, x86-64 only: the guard's filter checks that ABI).

pub mod calib;
pub mod counters;
pub mod guard;
pub mod leg;
pub mod mask;
pub mod meter;
pub mod supervisor;
pub mod window;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::fnv::fnv64;
use crate::{Result, refuse};

/// The calling thread's id.
pub fn gettid() -> i32 {
    // SAFETY: gettid has no preconditions.
    unsafe { libc::gettid() }
}

/// `/proc/self/task`: the process's threads.
pub fn census() -> Result<BTreeSet<i32>> {
    let dir = match std::fs::read_dir("/proc/self/task") {
        Ok(d) => d,
        Err(e) => refuse!("/proc/self/task: {e}"),
    };
    let mut tids = BTreeSet::new();
    for e in dir {
        let Ok(e) = e else {
            refuse!("reading /proc/self/task");
        };
        match e.file_name().to_string_lossy().parse() {
            Ok(t) => tids.insert(t),
            Err(_) => refuse!("a task entry {:?}", e.file_name()),
        };
    }
    Ok(tids)
}

/// The threads whose name `matches`. A thread that exits between the census and its name read
/// is not among them.
pub fn threads_named(matches: impl Fn(&str) -> bool) -> Result<BTreeSet<i32>> {
    let mut tids = BTreeSet::new();
    for tid in census()? {
        let name = match std::fs::read_to_string(format!("/proc/self/task/{tid}/comm")) {
            Ok(name) => name,
            // ENOENT at the open, or ESRCH from a read after the open (comm_show's task is gone).
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ESRCH) =>
            {
                continue;
            }
            Err(e) => refuse!("thread {tid}'s name: {e}"),
        };
        if matches(name.trim_end_matches('\n')) {
            tids.insert(tid);
        }
    }
    Ok(tids)
}

/// Core's and the node's script-check workers (`d-scriptch.<n>`) in this process.
pub fn script_check_threads() -> Result<usize> {
    Ok(threads_named(|n| n.starts_with("d-scriptch."))?.len())
}

/// The nanoseconds `tid` has run on a CPU (`/proc/self/task/<tid>/schedstat`'s first field).
pub fn run_ns(tid: i32) -> Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/schedstat"));
    match stat.ok().and_then(|s| s.split_whitespace().next()?.parse().ok()) {
        Some(ns) => Ok(ns),
        None => refuse!("thread {tid}'s schedstat"),
    }
}

/// A thread's scheduler state (`/proc/self/task/<tid>/stat`'s third field: `S` sleeping, `D`
/// in uninterruptible wait, `R` running, …).
pub fn state(tid: i32) -> Result<char> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"));
    match stat.ok().and_then(|s| s.rsplit_once(") ")?.1.chars().next()) {
        Some(c) => Ok(c),
        None => refuse!("thread {tid}'s stat"),
    }
}

/// `uname -r`.
pub fn kernel_release() -> String {
    // SAFETY: utsname is plain data; uname fills it.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid utsname.
    if unsafe { libc::uname(&mut u) } != 0 {
        return "unknown".into();
    }
    // SAFETY: uname NUL-terminates its fields.
    unsafe { std::ffi::CStr::from_ptr(u.release.as_ptr()) }.to_string_lossy().into_owned()
}

/// The first CPU's `model name`, `microcode` and the instruction-set flags hashing and
/// arithmetic dispatch on at run time (SHA-NI, AVX2, AVX-512, …), from `/proc/cpuinfo`.
pub fn cpu() -> String {
    const FLAGS: &[&str] = &["sha_ni", "avx", "avx2", "aes", "bmi2", "adx", "sse4_1", "sse4_2"];
    let info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let field = |name: &str| {
        info.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                (k.trim() == name).then(|| v.trim().to_owned())
            })
            .unwrap_or_else(|| "unknown".into())
    };
    let flags = field("flags");
    let mut on: Vec<&str> =
        flags.split_whitespace().filter(|f| FLAGS.contains(f) || f.starts_with("avx512")).collect();
    on.sort_unstable();
    format!("{}; microcode {}; {}", field("model name"), field("microcode"), on.join(" "))
}

/// The build's compile-time flags and environment (`node-amb`'s build script: rustflags, opt
/// level, debug, target features, rustc, `CARGO_PROFILE_*`), and a digest of every Cargo
/// configuration file a build here reads: `.cargo/config{,.toml}` in each directory from the
/// crate up, and in `CARGO_HOME`.
pub fn build_env() -> String {
    let mut files = Vec::new();
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cargo")));
    let mut dirs: Vec<PathBuf> =
        Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().map(|d| d.join(".cargo")).collect();
    dirs.extend(home);
    for dir in dirs {
        for name in ["config", "config.toml"] {
            let path = dir.join(name);
            if let Ok(bytes) = std::fs::read(&path) {
                files.push(format!("{} {}", path.display(), fnv64(&bytes)));
            }
        }
    }
    files.dedup();
    format!("{}; config {}", env!("AMB_BUILD_ENV"), fnv64(files.join("\n").as_bytes()))
}
