//! The privileged self-test: a known file workload recorded through `dm-log-writes`, then every
//! flush and FUA point (and seeded random and torn ones) replayed, mounted and checked. It proves
//! the device stack, the log reader and the replayer on a log the kernel wrote, before any node
//! runs on them.
//!
//! The workload makes files durably (write a temporary, fsync it, rename it into place, fsync the
//! directory) and marks `a<k>` after step `k` returns. At a point past `a<k>`, every file as of
//! step `k` must hold that content or a later version of it; a later file may be missing or hold
//! one of its versions. Anything else (a file that fails to mount, torn content, an unknown file)
//! is a failure.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use super::devices::{Fs, Priv, Run, Stack, remove_own_file, sectors, sparse_file};
use super::image::Replayer;
use super::logwrites::{Draw, Log, Point, PointKind, Rng, points, replay_order, window_end};

const DATA_LEN: u64 = 512 << 20;
const LOG_LEN: u64 = 2 << 30;
const STEPS: usize = 24;

/// A fault to inject, for the teardown tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    /// Panic with the recording stack up (Drop must tear it down).
    Panic,
    /// Write `<run dir>/paused` and wait with the recording stack up: until killed (the sweep must
    /// tear it down), or until the file is removed, then carry on (the pin test's sandbox starts
    /// meanwhile).
    Pause,
}

#[derive(Debug, Default)]
pub struct Report {
    pub entries: usize,
    pub points: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub failures: Vec<String>,
}

/// Step `k`'s file and version: mostly new files, every fourth step rewriting an older one.
fn step(k: usize) -> (String, u32) {
    if k % 4 == 3 {
        (format!("f{}", k - 2), 2)
    } else {
        (format!("f{k}"), 1)
    }
}

fn content(name: &str, version: u32) -> Vec<u8> {
    let n: u64 = name[1..].parse().expect("f<k>");
    let len = 4096 * (1 + (n as usize + version as usize) % 7) + 123;
    let mut rng = Rng::new(n << 8 | u64::from(version));
    (0..len).map(|_| rng.next_u64() as u8).collect()
}

fn fsync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

fn syncfs(dir: &Path) -> io::Result<()> {
    let f = File::open(dir)?;
    // SAFETY: an open descriptor.
    if unsafe { libc::syncfs(f.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_durably(dir: &Path, name: &str, data: &[u8]) -> io::Result<()> {
    let tmp = dir.join(format!(".t-{name}"));
    let mut f = File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(name))?;
    fsync_dir(dir)
}

/// Checks a mounted point against the workload: `acked` is how many steps were acknowledged.
fn check_tree(mnt: &Path, acked: usize) -> Result<(), String> {
    let mut versions: BTreeMap<String, Vec<(usize, u32)>> = BTreeMap::new();
    for k in 0..STEPS {
        let (name, v) = step(k);
        versions.entry(name).or_default().push((k, v));
    }
    let mut seen = Vec::new();
    for e in fs::read_dir(mnt).map_err(|e| format!("listing: {e}"))? {
        let e = e.map_err(|e| format!("listing: {e}"))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(".t-") || name == "lost+found" {
            continue;
        }
        let Some(vs) = versions.get(&name) else {
            return Err(format!("unknown file {name}"));
        };
        let data = fs::read(e.path()).map_err(|e| format!("reading {name}: {e}"))?;
        // The acknowledged version, if any, or any later one.
        let floor = vs.iter().rposition(|&(k, _)| k < acked).unwrap_or(0);
        if !vs[floor..].iter().any(|&(_, v)| data == content(&name, v)) {
            return Err(format!("{name}: {} bytes match no allowed version", data.len()));
        }
        seen.push(name);
    }
    for (name, vs) in &versions {
        if vs[0].0 < acked && !seen.contains(name) {
            return Err(format!("{name} missing (made at step {}, {acked} acknowledged)", vs[0].0));
        }
    }
    Ok(())
}

/// Records the workload on `fs` over log-writes; returns the log's path.
fn record(p: &Priv, run: &Run, fs: Fs, fault: Fault) -> io::Result<PathBuf> {
    let (data, log) = (run.dir().join("data.img"), run.dir().join("log.img"));
    sparse_file(&data, DATA_LEN)?;
    sparse_file(&log, LOG_LEN)?;
    let mnt = run.mountpoint("rec")?;
    let lw = run.dm("lw");
    let mut stack = Stack::new(p);
    let data_dev = stack.attach(&data)?;
    let log_dev = stack.attach(&log)?;
    let dev = stack
        .dm_create(&lw, &format!("0 {} log-writes {data_dev} {log_dev}", sectors(DATA_LEN)))?;
    stack.mkfs(&dev, fs)?;
    stack.mount(&dev, &mnt, fs, false)?;
    stack.chown_mnt(&mnt)?;
    syncfs(&mnt)?;
    stack.message(&lw, &["mark", "start"])?;
    match fault {
        Fault::None => {}
        Fault::Panic => panic!("st3b selftest: injected panic with the recording stack up"),
        Fault::Pause => {
            let paused = run.dir().join("paused");
            File::create(&paused)?;
            while paused.exists() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    for k in 0..STEPS {
        let (name, v) = step(k);
        write_durably(&mnt, &name, &content(&name, v))?;
        stack.message(&lw, &["mark", &format!("a{k}")])?;
    }
    stack.teardown().map_err(io::Error::other)?;
    Ok(log)
}

/// Replays and checks every point; a failing point's image is kept as its repro.
fn check(p: &Priv, run: &Run, fs: Fs, log_path: &Path, draw: Draw) -> io::Result<Report> {
    let log = Log::open(log_path).map_err(io::Error::other)?;
    let start = log.mark("start").ok_or_else(|| io::Error::other("no start mark"))?;
    let acks: Vec<usize> =
        (0..STEPS).map(|k| log.mark(&format!("a{k}")).expect("every ack mark")).collect();
    let pts = points(&log, start, draw);
    let base = sparse_file(&run.dir().join("replay.img"), DATA_LEN)?;
    let mut replayer = Replayer::new(&log, File::open(log_path)?, base);
    let mnt = run.mountpoint("pt")?;
    let mut report = Report {
        entries: log.entries.len(),
        points: pts.len(),
        ..Report::default()
    };
    // The control: at the start mark nothing is on disk yet, so claiming step 0 must fail. A check
    // that passes it proves nothing.
    let control = Point {
        kind: PointKind::Flush,
        prefix: start,
        extra: vec![],
        part: None,
    };
    let img = run.dir().join("control.img");
    replayer.snapshot(&control, &img)?;
    match (check_point(p, fs, &img, &mnt, 0, false), check_point(p, fs, &img, &mnt, 1, false)) {
        (Ok(()), Err(_)) => remove_own_file(&img)?,
        (empty, claimed) => report.failures.push(format!(
            "control at the start mark: nothing acknowledged gave {empty:?}, step 0 claimed gave \
             {claimed:?} (repro {})",
            img.display()
        )),
    }
    for (i, pt) in replay_order(&pts).into_iter().map(|i| (i, &pts[i])) {
        *report.by_kind.entry(pt.kind.to_string()).or_default() += 1;
        let img = run.dir().join(format!("pt{i}.img"));
        replayer.snapshot(pt, &img)?;
        let end = window_end(&log, pt);
        let acked = acks.iter().filter(|&&a| a <= end).count();
        match check_point(p, fs, &img, &mnt, acked, false) {
            Ok(()) => remove_own_file(&img)?,
            Err(why) => {
                report.failures.push(format!("{}: {why} (repro {})", describe(pt), img.display()))
            }
        }
    }
    // Every fsync flushes, so a log with fewer flush points than steps was not this workload's.
    let flushes = report.by_kind.get("flush").copied().unwrap_or(0);
    if flushes < STEPS {
        report.failures.push(format!("only {flushes} flush points for {STEPS} durable steps"));
    }
    // The second control: with everything on disk the tree passes, and one changed byte fails it.
    let end = Point {
        kind: PointKind::Rand,
        prefix: log.entries.len(),
        extra: vec![],
        part: None,
    };
    let img = run.dir().join("control-end.img");
    replayer.snapshot(&end, &img)?;
    match (
        check_point(p, fs, &img, &mnt, STEPS, false),
        check_point(p, fs, &img, &mnt, STEPS, true),
    ) {
        (Ok(()), Err(_)) => remove_own_file(&img)?,
        (clean, tampered) => report.failures.push(format!(
            "control at the log's end: as written gave {clean:?}, with f0 changed gave \
             {tampered:?} (repro {})",
            img.display()
        )),
    }
    // The reader's replay of the whole log is the recorded device, byte for byte.
    // Kept as the repro if not.
    let data = run.dir().join("data.img");
    match replayer.whole_log_differs(&data)? {
        None => remove_own_file(&data)?,
        Some(at) => report.failures.push(format!(
            "the whole log replayed differs from the recorded device at byte {at} (repro {})",
            data.display()
        )),
    }
    drop(replayer);
    remove_own_file(&run.dir().join("replay.img"))?;
    Ok(report)
}

fn describe(pt: &Point) -> String {
    let mut out = format!("{} point at prefix {}", pt.kind, pt.prefix);
    if !pt.extra.is_empty() {
        let _ = write!(out, " + {:?}", pt.extra);
    }
    if let Some(p) = &pt.part {
        let _ = write!(out, " + blocks {:?} of {}", p.blocks, p.entry);
    }
    out
}

/// Mounts a point's image and checks it; with `tamper`, first changes a byte of `f0` (made at step
/// 0, never rewritten) in the mounted copy.
fn check_point(
    p: &Priv,
    fs: Fs,
    img: &Path,
    mnt: &Path,
    acked: usize,
    tamper: bool,
) -> Result<(), String> {
    let mut stack = Stack::new(p);
    let dev = stack.attach(img).map_err(|e| e.to_string())?;
    stack.mount(Path::new(&dev), mnt, fs, false).map_err(|e| format!("mount: {e}"))?;
    let checked = if tamper {
        flip_first_byte(&mnt.join("f0"))
    } else {
        Ok(())
    }
    .map_err(|e| format!("tampering: {e}"))
    .and_then(|()| check_tree(mnt, acked));
    stack.teardown()?;
    checked
}

fn flip_first_byte(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    let f = fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut b = [0u8];
    f.read_exact_at(&mut b, 0)?;
    f.write_all_at(&[!b[0]], 0)?;
    f.sync_all()
}

/// Records, replays and checks; the recording's devices are torn down before the replay.
pub fn run(p: &Priv, run: &Run, fs: Fs, fault: Fault, draw: Draw) -> io::Result<Report> {
    let log = record(p, run, fs, fault)?;
    let report = check(p, run, fs, &log, draw)?;
    if report.failures.is_empty() {
        remove_own_file(&log)?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workload_rewrites_files_it_made() {
        let names: Vec<_> = (0..STEPS).map(step).collect();
        for (k, (name, v)) in names.iter().enumerate() {
            if *v == 2 {
                assert!(names[..k].contains(&(name.clone(), 1)), "step {k} rewrites {name}");
            }
        }
        assert_ne!(content("f1", 1), content("f1", 2));
    }
}
