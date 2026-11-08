//! The fault cases (S6, PLAN-ST3b §3.5–3.6): write errors from a dm-flakey window and a full
//! filesystem, each while rdashd syncs a fixture. rdashd must fail-stop (exit 1, one `AbortNode`
//! with a Core text for an I/O or space failure, no tip after it); then the state a power cut would
//! have left and the repaired device must both recover.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::runtime::Runtime;

use super::baseline::{self, Peer, UntilDeath, node_args};
use super::campaign::{self, DATA_LEN, DATADIR, LOG_LEN, Phases, check_image};
use super::checks::{Floor, RPC_PATIENCE};
use super::devices::{Fs, Priv, Run, Stack, private_dir, remove_own_file, sectors, sparse_file};
use super::fixture::Fixture;
use super::image::{Replayer, reflink};
use super::logwrites::{Draw, FLUSH, FUA, Log, Point, PointKind, Rng};
use super::trace::{self, Micros, Scope};
use crate::{MockTime, PATIENCE, RustNode, RustNodeConfig, find_available_port};

/// The `AbortNode` texts an I/O error or a full disk gives that are fixed in Core v24
/// (`node/blockstorage.cpp` `FlushUndoFile`, `FlushBlockFile`, `FindBlockPos`, `FindUndoPos`,
/// `WriteUndoDataForBlock`, `SaveBlockToDisk`; `validation.cpp` `FlushStateToDisk`), matched
/// whole. Core's false-return branches (`Failed to write to block index database`, `… coin
/// database`, `Failed to commit EvoDB`) are outside S6's non-evo scope and match nothing, so a run
/// that meets one is looked at (S6 plan §3a).
pub const EXACT: [&str; 5] = [
    "Disk space is too low!",
    "Flushing block file to disk failed. This is likely the result of an I/O error.",
    "Flushing undo file to disk failed. This is likely the result of an I/O error.",
    "Failed to write block",
    "Failed to write undo data",
];

/// The texts Core builds from a caught system error (`FlushStateToDisk`, `AcceptBlock`); the rest
/// is the error's own text, which must name the failure the trace saw.
pub const PREFIXED: [&str; 2] = ["System error while flushing: ", "System error: "];

/// A shutdown flush's repeat of a low-space abort ([`EXACT`]`[0]`): rdashd latches it and reports
/// the latch in DEC-253's form, where Core checks the space again and prints `EXACT[0]`. A known
/// deviation, BL-213 (DEC-268); remove this when BL-213 lands (OD-ST3b-8).
const LOW_SPACE_REPEAT: &str = "System error while flushing: Disk space is too low!";

/// The texts of [`EXACT`] that name a flat file's failure: rdashd's flush passes the whole
/// `FlatFileError::Io` display after one, `<context>: <path>: <OS error>` (`flatfile/error.rs`).
const FLAT_FILE: [&str; 4] = [EXACT[1], EXACT[2], EXACT[3], EXACT[4]];

/// The run's fatal failures up to its causal abort (S6 plan §3b): their errnos, and the mount the
/// faulted filesystem is on, which a flat-file abort's path must lie under. Not the first failure
/// alone: RocksDB reports the error of a later job, as when a failed table sync aborts ext4's
/// journal and the next table's open meets EROFS (OD-ST3b-8).
#[derive(Clone, Copy, Debug)]
pub struct Failure<'a> {
    pub errnos: &'a [i32],
    pub mount: &'a Path,
}

/// The C library's text for `errno`, as Rust's `io::Error` shows it before ` (os error N)`.
fn strerror(errno: i32) -> String {
    let full = io::Error::from_raw_os_error(errno).to_string();
    full.strip_suffix(&format!(" (os error {errno})")).unwrap_or(&full).to_owned()
}

/// Whether `msg` is an abort text the run's fatal failures may give (S6 plan §3a): one of
/// [`EXACT`]; a flat-file context, then a path under the faulted mount and one of their errnos as
/// `io::Error` shows it; or a [`PREFIXED`] text whose rest names one of their errnos. With no
/// failure observed only the exact texts match.
pub fn sanctioned(msg: &str, failure: Option<Failure<'_>>) -> bool {
    if EXACT.contains(&msg) {
        return true;
    }
    let Some(f) = failure else {
        return false;
    };
    f.errnos.iter().any(|&errno| {
        let os = io::Error::from_raw_os_error(errno).to_string();
        let flat_file =
            FLAT_FILE.iter().filter_map(|c| msg.strip_prefix(c)?.strip_prefix(": ")).any(|rest| {
                rest.strip_suffix(&os)
                    .and_then(|r| r.strip_suffix(": "))
                    .is_some_and(|path| Path::new(path).starts_with(f.mount))
            });
        let name = strerror(errno);
        flat_file || PREFIXED.iter().filter_map(|p| msg.strip_prefix(p)).any(|r| r.contains(&name))
    })
}

/// The line rdashd logs for each new tip (`chain/abc.rs`, Core's `UpdateTipLog`).
pub const UPDATE_TIP: &str = "UpdateTip: ";

/// One fault case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    /// Write errors from a random moment of the sync on.
    FlIbd,
    /// Write errors from the first coins-flush line on, rdashd frozen (`SIGSTOP`) at that line for
    /// the swap and continued after it: a W-small flush lasts 5–110 ms, less than the swap, and
    /// a fault applied while it ran on met only background syncs completing after it (S6 smoke at
    /// 2193e6422, by completion time, OD-ST3b-15).
    FlFlush,
    /// Write errors on the chainstate's live WAL alone, from an acknowledgement on
    /// ([`wal_fault_table`]): the next flush's first chainstate write, B1's synced one, meets them.
    /// A fault on the whole device met the block file's sync first, or a background memtable
    /// flush's table sync (S6 smoke, OD-ST3b-7 and OD-ST3b-11).
    FlWal,
    /// The filesystem left under Core's 50 MiB minimum.
    DfPre,
    /// The filesystem filled at the first coins-flush line, after a pre-fill at the
    /// acknowledgement that leaves [`DF_MID_PREFILL_LEAVE`].
    DfMid,
}

impl Case {
    pub const ALL: [Case; 5] = [Case::FlIbd, Case::FlFlush, Case::FlWal, Case::DfPre, Case::DfMid];

    pub fn name(self) -> &'static str {
        match self {
            Case::FlIbd => "FL-IBD",
            Case::FlFlush => "FL-FLUSH",
            Case::FlWal => "FL-WAL",
            Case::DfPre => "DF-PRE",
            Case::DfMid => "DF-MID",
        }
    }

    pub fn parse(s: &str) -> Option<Case> {
        Case::ALL.into_iter().find(|c| c.name() == s)
    }

    /// A dm-flakey case (else a disk-full one).
    pub fn flakey(self) -> bool {
        matches!(self, Case::FlIbd | Case::FlFlush | Case::FlWal)
    }

    /// Armed on a flush log line ([`FLUSH_LINES`]) rather than at an acknowledgement.
    pub fn on_flush_line(self) -> bool {
        matches!(self, Case::FlFlush | Case::DfMid)
    }
}

impl fmt::Display for Case {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The debug.log lines that start a coins flush's writes (CH2's split flush, ST3b-D9).
pub const FLUSH_LINES: [&str; 2] = ["Writing partial batch", "Writing final batch"];

/// Whether `line` is one [`Case::on_flush_line`] cases fire on.
pub fn is_flush_line(line: &str) -> bool {
    FLUSH_LINES.iter().any(|l| line.contains(l))
}

/// What a fail-stop left in the node's log: the causal abort's text (after `*** `), and how many
/// repeat reports shutdown made after it.
#[derive(Debug, PartialEq, Eq)]
pub struct FailStop {
    pub abort: String,
    pub repeats: usize,
}

/// Lists the first panic line of the debug.log `log` and of `stderr`, if either has one.
fn panics(log: &str, stderr: &str, failures: &mut Vec<String>) {
    for (what, text) in [("debug.log", log), ("stderr", stderr)] {
        if let Some(l) = text.lines().find(|l| l.contains("panicked at")) {
            failures.push(format!("a panic in {what}: {l}"));
        }
    }
}

/// PLAN §3.5's fail-stop, judged from this run's debug.log `log`, its stderr, its exit code and its
/// fatal failures up to the abort (S6 plan §3.1–3.2, §3a): exit code 1; no panic; a first `*** `
/// line, the causal abort, [`sanctioned`] for `failure`; no `UpdateTip` after it; and every later
/// `*** ` line (shutdown's forced flushes report the latched failure again, DEC-241/253, BL-482)
/// sanctioned too. Every failure is listed.
pub fn fail_stop(
    log: &str,
    stderr: &str,
    code: Option<i32>,
    failure: Option<Failure<'_>>,
) -> Result<FailStop, Vec<String>> {
    let mut failures = Vec::new();
    if code != Some(1) {
        failures.push(format!("exit code {code:?}, not 1"));
    }
    panics(log, stderr, &mut failures);
    let aborts: Vec<(usize, &str)> = log
        .lines()
        .enumerate()
        .filter_map(|(n, l)| l.split_once("*** ").map(|(_, msg)| (n, msg)))
        .collect();
    let Some(&(first, abort)) = aborts.first() else {
        failures.push("no AbortNode line (`*** `): the fault did not stop rdashd".to_owned());
        return Err(failures);
    };
    for (k, (_, msg)) in aborts.iter().enumerate() {
        let latched_low_space = k > 0 && abort == EXACT[0] && *msg == LOW_SPACE_REPEAT;
        if !sanctioned(msg, failure) && !latched_low_space {
            let which = if k == 0 {
                "the abort".to_owned()
            } else {
                format!("repeat report {k}")
            };
            failures.push(format!("{which} {msg:?} is not an I/O or space failure's text"));
        }
    }
    if let Some(l) = log.lines().skip(first + 1).find(|l| l.contains(UPDATE_TIP)) {
        failures.push(format!("a new tip after the abort: {l}"));
    }
    if failures.is_empty() {
        Ok(FailStop {
            abort: abort.to_owned(),
            repeats: aborts.len() - 1,
        })
    } else {
        Err(failures)
    }
}

/// How rdashd's run ended (S6 plan §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Termination {
    /// It exited by itself, seen by the exit watcher (`watch_exit`).
    Own,
    /// Still running [`BOUND`] after the injection: the watcher killed it.
    Deadline,
    /// The harness stopped it, with no fault in place (FL-WAL untouched).
    Stopped,
}

/// The fail-stop verdict on a run's end `(termination, whether any fatal failure was traced)`
/// (S6 plan §3.1): only an exit of rdashd's own can be a fail-stop ([`fail_stop`]); a kill at the
/// bound never is; a harness stop is a clean stop when nothing failed ([`clean_stop`], the case a
/// miss) and a failure when something did: rdashd went on (Sol r2 M1). `Ok(None)`: no fail-stop
/// was due.
pub fn judge_end(
    (termination, failed): (Termination, bool),
    log: &str,
    stderr: &str,
    code: Option<i32>,
    failure: Option<Failure<'_>>,
) -> Result<Option<FailStop>, Vec<String>> {
    match termination {
        Termination::Own => fail_stop(log, stderr, code, failure).map(Some),
        Termination::Deadline => {
            Err(vec![format!("no fail-stop within {BOUND:?} of the injection: killed")])
        }
        Termination::Stopped if failed => Err(vec![
            "rdashd went on after a fatal failure until the harness stopped it".to_owned(),
        ]),
        Termination::Stopped => {
            let failures = clean_stop(log, stderr, code);
            if failures.is_empty() {
                Ok(None)
            } else {
                Err(failures)
            }
        }
    }
}

/// The exit mark: the log-writes `exit` mark, from whichever thread sees rdashd gone first.
type MarkExit<'a> = dyn Fn() -> io::Result<()> + Sync + 'a;

/// Whether child `pid` has exited, without reaping it (`waitid` with `WNOWAIT`: the node's own
/// wait reaps it later). ECHILD (no such child: reaped) counts as exited.
fn exited(pid: u32) -> io::Result<bool> {
    // SAFETY: all-zero is a valid siginfo_t.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    // SAFETY: a child of this process and a siginfo the call fills; WNOWAIT leaves it unreaped.
    if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, flags) } != 0 {
        let e = io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::ECHILD) {
            Ok(true)
        } else {
            Err(e)
        };
    }
    // SAFETY: filled by the call; its pid stays zero while the child runs.
    Ok(unsafe { info.si_pid() } != 0)
}

/// One look at the child for the watcher (S6 plan §3.1, Sol r3/r4 M5): past `bound` from the
/// injection (`started`) it is killed first (`kill`); then whether it is `gone`. An exit is
/// rdashd's own only if the bound still holds after it was seen: a look delayed past the bound,
/// before or during the observation, can't tell a late exit from an early one, so it counts as the
/// bound's. `Some` once it is gone.
fn look(
    started: &dyn Fn() -> Option<Instant>,
    bound: Duration,
    killed: &mut bool,
    (kill, gone): (&dyn Fn(), &dyn Fn() -> io::Result<bool>),
) -> io::Result<Option<Termination>> {
    let late = || started().is_some_and(|t| t.elapsed() >= bound);
    if !*killed && late() {
        kill();
        *killed = true;
    }
    if !gone()? {
        return Ok(None);
    }
    Ok(Some(if *killed || late() {
        Termination::Deadline
    } else {
        Termination::Own
    }))
}

/// rdashd's exit watcher (S6 plan §3.1, Sol r1–r3 M5), beside the schedule from its start: marks
/// `exit` the moment the child `pid` is gone (§3.3) and reports how it ended ([`look`]); kills it
/// `bound` after the injection (`started`), whatever the schedule is waiting for. `None`:
/// `release` came first, with the child alive at that look (the harness stops it itself). An
/// error looking at the child ends the watch as the bound's: never a pass.
fn watch_exit(
    pid: u32,
    started: &dyn Fn() -> Option<Instant>,
    bound: Duration,
    release: &AtomicBool,
    mark_exit: &MarkExit<'_>,
) -> Option<(Termination, io::Result<()>)> {
    // A zombie ignores the signal.
    let kill = || signal(pid, libc::SIGKILL);
    let gone = || exited(pid);
    let mut killed = false;
    loop {
        match look(started, bound, &mut killed, (&kill, &gone)) {
            Ok(Some(how)) => return Some((how, mark_exit())),
            Ok(None) => {}
            Err(e) => return Some((Termination::Deadline, mark_exit().and(Err(e)))),
        }
        if release.load(Ordering::SeqCst) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// What a harness stop must leave when no fault was met (FL-WAL untouched, Sol r1 M1): exit code 0,
/// no panic and no abort line. Every failure is listed.
pub fn clean_stop(log: &str, stderr: &str, code: Option<i32>) -> Vec<String> {
    let mut failures = Vec::new();
    if code != Some(0) {
        failures.push(format!("exit code {code:?} after a stop with no fault met, not 0"));
    }
    panics(log, stderr, &mut failures);
    if let Some(l) = log.lines().find(|l| l.contains("*** ")) {
        failures.push(format!("an abort with no fault met: {l}"));
    }
    failures
}

/// Core's `CheckDiskSpace` minimum (`util/fs_helpers.h` `MIN_DISK_SPACE`, 50 MiB), which DF-PRE
/// leaves the filesystem under.
pub const MIN_DISK_SPACE: u64 = 50 << 20;

/// What DF-PRE leaves available: under [`MIN_DISK_SPACE`], with room for the log lines and the
/// harness's own files.
pub const DF_PRE_LEAVE: u64 = 40 << 20;
const _: () = assert!(DF_PRE_LEAVE < MIN_DISK_SPACE);

/// What DF-MID's pre-fill leaves when it arms (OD-ST3b-9): over [`MIN_DISK_SPACE`] by a new block
/// file chunk (16 MiB, which `FindBlockPos` checks for) and room, so the flush's space checks pass
/// and the fill at the flush line has little left to take. A full fill there took longer than the
/// flush's 0.02 MiB batches, which ext4 had already reserved (acceptance run, 10-31).
pub const DF_MID_PREFILL_LEAVE: u64 = MIN_DISK_SPACE + (24 << 20);

/// The filler's length that leaves `leave` of `available` bytes, or `None` when less than that is
/// available already.
pub fn filler_len(available: u64, leave: u64) -> Option<u64> {
    available.checked_sub(leave).filter(|&n| n > 0)
}

/// The table of the fault device `fl` over `under` (`len` sectors): `linear`, or with `down` a
/// flakey table that is always down (0 s up, 1 s down) and fails every write, empty flush bios
/// included.
pub fn fault_table(under: &str, len: u64, down: bool) -> String {
    if down {
        format!("0 {len} flakey {under} 0 0 1 1 error_writes")
    } else {
        format!("0 {len} linear {under} 0")
    }
}

/// FL-WAL's table over `under` (`len` sectors), as `dm-swap` segments: always down (as
/// [`fault_table`]) on the WAL's physical `extents` (bytes: start, length) and linear elsewhere,
/// each segment an identity map.
pub fn wal_fault_table(
    under: &str,
    len: u64,
    extents: &[(u64, u64)],
) -> Result<Vec<String>, String> {
    let mut spans = Vec::with_capacity(extents.len());
    for &(at, n) in extents {
        if at % 512 != 0 || n % 512 != 0 || n == 0 {
            return Err(format!("extent ({at}, {n}) is not in whole sectors"));
        }
        spans.push((at / 512, (at + n) / 512));
    }
    spans.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    match merged.last() {
        None => return Err("no extents".to_owned()),
        Some(&(_, end)) if end > len => {
            return Err(format!("an extent ends at sector {end}, past the device's {len}"));
        }
        Some(_) => {}
    }
    let mut table = Vec::new();
    let mut at = 0;
    for (a, b) in merged {
        if a > at {
            table.push(format!("{at} {} linear {under} {at}", a - at));
        }
        table.push(format!("{a} {} flakey {under} {a} 0 1 1 error_writes", b - a));
        at = b;
    }
    if at < len {
        table.push(format!("{at} {} linear {under} {at}", len - at));
    }
    Ok(table)
}

/// What FL-WAL preallocates of the live WAL before mapping it: more than a flush writes there
/// (B1's 36 bytes, then the coin batches' up to ~0.1 MiB at W-small), with RocksDB's own
/// preallocation (1,064,960 bytes) inside it, so every write the flush makes to it lands on a
/// mapped sector.
const WAL_PREALLOC: i64 = 4 << 20;

/// How long FL-WAL waits for the flush that acknowledged to open the next WAL.
const WAL_SWITCH_WAIT: Duration = Duration::from_secs(2);

/// The chainstate's live WAL under `datadir`: its highest-numbered `*.log`, and its length.
fn live_wal(datadir: &Path) -> io::Result<(PathBuf, u64)> {
    let mut best: Option<(u64, PathBuf)> = None;
    for e in fs::read_dir(datadir.join(campaign::CHAINSTATE))? {
        let path = e?.path();
        let number =
            path.file_name().and_then(|n| n.to_str()?.strip_suffix(".log")?.parse::<u64>().ok());
        if let Some(n) = number.filter(|&n| best.as_ref().is_none_or(|(b, _)| n > *b)) {
            best = Some((n, path));
        }
    }
    let (_, path) = best.ok_or_else(|| io::Error::other("no chainstate WAL"))?;
    let len = fs::metadata(&path)?.len();
    Ok((path, len))
}

/// `FS_IOC_FIEMAP`'s request (`linux/fiemap.h`) with room for [`FIEMAP_EXTENTS`] extents.
#[repr(C)]
struct Fiemap {
    start: u64,
    length: u64,
    flags: u32,
    mapped_extents: u32,
    extent_count: u32,
    reserved: u32,
    extents: [FiemapExtent; FIEMAP_EXTENTS],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FiemapExtent {
    logical: u64,
    physical: u64,
    length: u64,
    reserved64: [u64; 2],
    flags: u32,
    reserved: [u32; 3],
}

const FIEMAP_EXTENTS: usize = 256;
/// `_IOWR('f', 11, struct fiemap)`.
const FS_IOC_FIEMAP: libc::c_ulong = 0xC020_660B;
const FIEMAP_EXTENT_LAST: u32 = 0x1;
/// Extents a write may not land on where they say: unknown or delayed placement, encoded,
/// unaligned, inline or tail-packed data, or shared (a write copies it elsewhere).
const FIEMAP_EXTENT_UNMAPPABLE: u32 = 0x2 | 0x4 | 0x8 | 0x80 | 0x100 | 0x200 | 0x400 | 0x2000;

/// The physical extents (bytes: start, length) of `file`, preallocated ones included.
fn physical_extents(file: &File) -> io::Result<Vec<(u64, u64)>> {
    let mut map = Fiemap {
        start: 0,
        length: u64::MAX,
        flags: 0,
        mapped_extents: 0,
        extent_count: FIEMAP_EXTENTS as u32,
        reserved: 0,
        extents: [FiemapExtent::default(); FIEMAP_EXTENTS],
    };
    // SAFETY: an open descriptor and a request with room for the extents it counts.
    if unsafe { libc::ioctl(file.as_raw_fd(), FS_IOC_FIEMAP as _, &mut map) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let extents = &map.extents[..map.mapped_extents as usize];
    if extents.last().is_none_or(|e| e.flags & FIEMAP_EXTENT_LAST == 0) {
        return Err(io::Error::other(format!("{} extents and no last one", extents.len())));
    }
    if let Some(e) = extents.iter().find(|e| e.flags & FIEMAP_EXTENT_UNMAPPABLE != 0) {
        return Err(io::Error::other(format!("an extent with flags {:#x}", e.flags)));
    }
    Ok(extents.iter().map(|e| (e.physical, e.length)).collect())
}

/// How long the fault stays open for rdashd to exit by itself; past it, no fail-stop (S6 plan §1).
pub const BOUND: Duration = Duration::from_secs(600);

/// The disk-full cases' filler, at the faulted filesystem's root, outside the datadir.
const FILLER: &str = "st3b-filler";

/// What a writable copy must have available once the filler is gone (S6 plan §3.6).
const FREE_FOR_CHECKS: u64 = 64 << 20;

#[derive(Clone, Debug)]
pub struct Config {
    pub case: Case,
    pub fs: Fs,
    pub rdashd: PathBuf,
    pub dashd: PathBuf,
    /// rdashd's arguments beyond the fixture's chain and `-logtimemicros`.
    pub extra: Vec<String>,
    /// An acknowledgement every this many submitted blocks.
    pub every: usize,
    pub seed: u64,
}

/// What one attempt of a case found.
#[derive(Debug, Default)]
pub struct Report {
    /// The trigger the seed drew.
    pub trigger: String,
    /// The case's condition missed (S6 plan §3b): the case retries with the next seed.
    pub miss: Option<String>,
    pub abort: Option<String>,
    pub repeats: usize,
    /// The run's first fatal failure, as traced.
    pub first_failure: Option<String>,
    pub floor: Option<Floor>,
    /// Behaviour failures: never retried away.
    pub failures: Vec<String>,
    /// The power-loss image's findings when the capture is not the device's history (a miss):
    /// printed, not judged (OD-ST3b-8).
    pub set_aside: Vec<String>,
}

/// The trigger seed `seed` draws (S6 plan §1) for a schedule with `stored_acks` acknowledgements
/// of stored blocks: the one it arms after, `k` in `[1, stored_acks - 2]` (from 0), and a delay in
/// microseconds (FL-IBD up to 2 s after it, else none).
pub fn draw(case: Case, seed: u64, stored_acks: usize) -> Option<(usize, u64)> {
    let span = (stored_acks as u64).checked_sub(2).filter(|&n| n > 0)?;
    let mut rng = Rng::new(seed);
    let k = 1 + rng.below(span) as usize;
    let delay = match case {
        Case::FlIbd => rng.below(2_000_001),
        _ => 0,
    };
    Some((k, delay))
}

fn now() -> Micros {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as Micros)
}

/// `statvfs`'s `f_bavail` in bytes: what Core's `CheckDiskSpace` reads.
fn available(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a NUL-terminated path and a statvfs the call fills.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(available_bytes(st.f_bavail as u64, st.f_blocks as u64, st.f_frsize as u64))
}

/// `available` blocks of `frsize` bytes on a filesystem of `blocks`: none when it reports more
/// than it has (a full xfs drawing on its reserve gave a count whose bytes overflowed u64,
/// DF-MID's keep-full loop, 10-31).
fn available_bytes(available: u64, blocks: u64, frsize: u64) -> u64 {
    if available > blocks {
        0
    } else {
        available.saturating_mul(frsize)
    }
}

/// Grows the filler at `path` with `fallocate(2)` until `leave` bytes or less are available,
/// halving the request on each ENOSPC (metadata takes its share) until it is under 4 KiB.
fn fill(path: &Path, leave: u64) -> io::Result<()> {
    let file = OpenOptions::new().create(true).write(true).truncate(false).open(path)?;
    let dir = path.parent().expect("the filler is in a directory");
    let (mut len, mut cap) = (file.metadata()?.len(), u64::MAX);
    while let Some(want) = filler_len(available(dir)?, leave).map(|n| n.min(cap)) {
        if want < 4096 {
            break;
        }
        // SAFETY: an open file and a range in bytes.
        if unsafe { libc::fallocate(file.as_raw_fd(), 0, len as i64, want as i64) } == 0 {
            len += want;
            continue;
        }
        match io::Error::last_os_error() {
            e if e.raw_os_error() == Some(libc::ENOSPC) => cap = want / 2,
            e => return Err(e),
        }
    }
    Ok(())
}

/// The fault, applied once by whichever trigger fires first, and never after [`Gate::close`].
struct Gate<'a> {
    p: &'a Priv,
    case: Case,
    /// The fault device's name, the device under it and its length in sectors.
    fl: &'a str,
    under: &'a str,
    len: u64,
    mnt: &'a Path,
    armed: &'a AtomicBool,
    /// Closed, and when the fault was applied (just before: nothing else fails a write) or why
    /// applying it failed.
    state: Mutex<(bool, Option<Result<Micros, String>>)>,
    /// The injection's monotonic moment, for the bound, read without the state's lock (which
    /// injection holds while it prepares and applies the fault, Sol r3).
    started: OnceLock<Instant>,
}

impl Gate<'_> {
    /// Swaps the fault device's table: always down, or linear again.
    fn swap(&self, down: bool) -> io::Result<()> {
        self.swap_to(&[fault_table(self.under, self.len, down)])
    }

    fn swap_to(&self, table: &[String]) -> io::Result<()> {
        let mut args = vec!["dm-swap", self.fl];
        args.extend(table.iter().map(String::as_str));
        self.p.run(&args).map(drop)
    }

    /// FL-WAL's table ([`wal_fault_table`]) over the live WAL, once the flush that acknowledged
    /// has opened its next one (RocksDB opens it at the flush's end, empty: no sector of it is
    /// placed until written, S6 smoke trace). Preallocating it places the sectors the next flush
    /// writes, as RocksDB's own preallocation would at that write; its length and bytes stay.
    fn wal_table(&self) -> io::Result<Vec<String>> {
        let datadir = self.mnt.join(DATADIR);
        let deadline = Instant::now() + WAL_SWITCH_WAIT;
        let mut wal = live_wal(&datadir)?;
        while wal.1 != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
            wal = live_wal(&datadir)?;
        }
        let file = OpenOptions::new().write(true).open(&wal.0)?;
        let kept = libc::FALLOC_FL_KEEP_SIZE;
        // SAFETY: an open descriptor and a range in bytes.
        if unsafe { libc::fallocate(file.as_raw_fd(), kept, 0, WAL_PREALLOC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let extents = physical_extents(&file)?;
        wal_fault_table(self.under, self.len, &extents)
            .map_err(|e| io::Error::other(format!("{}: {e}", wal.0.display())))
    }

    fn inject(&self) {
        let mut state = self.state.lock().expect("not poisoned");
        if state.0 || state.1.is_some() {
            return;
        }
        // FL-WAL's table comes first, while nothing fails.
        let wal = (self.case == Case::FlWal).then(|| self.wal_table());
        self.armed.store(true, Ordering::SeqCst);
        let at = now();
        let _ = self.started.set(Instant::now());
        let applied = match (self.case, wal) {
            (_, Some(table)) => table.and_then(|t| self.swap_to(&t)),
            (c, None) if c.flakey() => self.swap(true),
            (Case::DfPre, None) => fill(&self.mnt.join(FILLER), DF_PRE_LEAVE),
            _ => fill(&self.mnt.join(FILLER), 0),
        };
        state.1 = Some(applied.map(|()| at).map_err(|e| e.to_string()));
    }

    /// DF-MID's pre-fill: most of the space, before the flush line, without arming the window.
    fn prefill(&self) -> io::Result<()> {
        fill(&self.mnt.join(FILLER), DF_MID_PREFILL_LEAVE)
    }

    /// DF-MID's disk stays full until `stop`: the filler takes whatever the node frees (RocksDB
    /// deletes obsolete files during a flush, and on ext4 the flush's batches fit in that room,
    /// OD-ST3b-9). Stops at its first error, or once the gate is closed.
    fn keep_full(&self, stop: &AtomicBool) {
        while !stop.load(Ordering::SeqCst) && !self.state.lock().expect("not poisoned").0 {
            if fill(&self.mnt.join(FILLER), 0).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// No fault from now on; whether one was applied (or tried).
    fn close(&self) -> Option<Result<Micros, String>> {
        let mut state = self.state.lock().expect("not poisoned");
        state.0 = true;
        state.1.clone()
    }
}

/// Sends `signal` to the child `pid`.
fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: signalling the child this process started and has not reaped.
    unsafe { libc::kill(pid as libc::pid_t, signal) };
}

/// Whether every live thread of `pid` is stopped (`T`, or `t` while the tracer holds it), by
/// `/proc/<pid>/task/*/stat`; `None` when no thread is live (it is gone).
fn stopped(pid: u32) -> Option<bool> {
    let states: Vec<u8> = fs::read_dir(format!("/proc/{pid}/task"))
        .ok()?
        .filter_map(|t| {
            let stat = fs::read_to_string(t.ok()?.path().join("stat")).ok()?;
            stat.rsplit_once(") ")?.1.bytes().next()
        })
        .filter(|s| !matches!(s, b'Z' | b'X'))
        .collect();
    (!states.is_empty()).then(|| states.iter().all(|s| matches!(s, b'T' | b't')))
}

/// How long a freeze waits for rdashd's threads to stop, or to run again.
const FREEZE_PATIENCE: Duration = Duration::from_secs(5);

/// How long rdashd's threads must be seen running before a thaw counts.
const THAW_SETTLE: Duration = Duration::from_millis(250);

/// Waits up to [`FREEZE_PATIENCE`] for the threads of `pid` still running to stop.
fn await_stopped(pid: u32) {
    let until = Instant::now() + FREEZE_PATIENCE;
    while stopped(pid) == Some(false) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// FL-FLUSH's freeze (S6 plan §1 rev7): rdashd held by `SIGSTOP` until [`Freeze::thaw`], or until
/// dropped, so that a panic can't leave it stopped (Opus r1 m1). A stop only pauses it (a process
/// may be descheduled for as long), and the exit watcher counts only exits.
struct Freeze(u32);

impl Freeze {
    /// Stops `pid` and waits up to [`FREEZE_PATIENCE`] until every thread is stopped: under strace
    /// a stop takes hold only once the tracer passes the signal on (Opus r1 m1).
    fn new(pid: u32) -> Freeze {
        signal(pid, libc::SIGSTOP);
        await_stopped(pid);
        Freeze(pid)
    }

    /// Continues it. A `SIGCONT` sent while the tracer still holds the `SIGSTOP` cancels nothing,
    /// and the stop then comes after it (Opus r1 m1): so the stop is let take hold first, then
    /// `SIGCONT` is sent until the threads have run for [`THAW_SETTLE`]. An error when it never
    /// stopped, or never ran again.
    fn thaw(self) -> Result<(), String> {
        let pid = self.0;
        std::mem::forget(self);
        await_stopped(pid);
        let took = stopped(pid) != Some(false);
        resume(pid)?;
        if took {
            Ok(())
        } else {
            Err(format!("rdashd did not stop within {:?} of the SIGSTOP", FREEZE_PATIENCE * 2))
        }
    }
}

impl Drop for Freeze {
    fn drop(&mut self) {
        let _ = resume(self.0);
    }
}

/// Sends `SIGCONT` to `pid`, again whenever it is seen stopped (only then: under strace each one is
/// a signal stop of its own, Opus r2 n3), until its threads have run for [`THAW_SETTLE`] (or it is
/// gone), for up to [`FREEZE_PATIENCE`].
fn resume(pid: u32) -> Result<(), String> {
    let until = Instant::now() + FREEZE_PATIENCE;
    let mut running = None;
    signal(pid, libc::SIGCONT);
    loop {
        std::thread::sleep(Duration::from_millis(10));
        match stopped(pid) {
            None => return Ok(()),
            Some(true) => {
                running = None;
                signal(pid, libc::SIGCONT);
            }
            Some(false) => {
                if running.get_or_insert_with(Instant::now).elapsed() >= THAW_SETTLE {
                    return Ok(());
                }
            }
        }
        if Instant::now() >= until {
            return Err(format!("rdashd still stopped {FREEZE_PATIENCE:?} after the SIGCONT"));
        }
    }
}

/// Raises its flags when dropped: a scope's threads waiting on them end however its body ends, a
/// panic included (Opus r1 m4).
struct Raise<'a>([&'a AtomicBool; 2]);

impl Drop for Raise<'_> {
    fn drop(&mut self) {
        for flag in self.0 {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Sleeps `micros`, or less if `stop` is set; whether it slept the whole time.
fn sleep_unless(stop: &AtomicBool, micros: u64) -> bool {
    let until = Instant::now() + Duration::from_micros(micros);
    while Instant::now() < until {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1).min(until - Instant::now()));
    }
    !stop.load(Ordering::SeqCst)
}

/// Tails the debug.log at `path` from byte `from` until a coins-flush line comes (its time), or an
/// abort line (`*** `: no trigger on the flushes of a shutdown rdashd began itself, Opus r1 M1),
/// or `stop` is set.
fn watch_flush_line(path: &Path, from: u64, stop: &AtomicBool) -> Option<Micros> {
    let (mut pos, mut pending) = (from, Vec::new());
    while !stop.load(Ordering::SeqCst) {
        if let Ok(mut f) = File::open(path) {
            let read = f.seek(SeekFrom::Start(pos)).and_then(|_| f.read_to_end(&mut pending));
            pos += read.unwrap_or(0) as u64;
        }
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line);
            if line.contains("*** ") {
                return None;
            }
            if is_flush_line(&line) {
                return Some(trace::log_time(&line).unwrap_or_else(now));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

/// `strace` attached to every thread of `pid` (S6 plan §3b), writing to `out`; returns once it
/// reports the attach.
fn attach_tracer(pid: u32, out: &Path, err: &Path) -> io::Result<Child> {
    let mut command = Command::new("strace");
    crate::rust_node::kill_with_parent(&mut command);
    let mut child = command
        .args(["-f", "-ttt", "-T", "-y", "-s", "0", "-e"])
        .arg(format!("trace={}", trace::TRACED))
        .arg("-o")
        .arg(out)
        .arg("-p")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(File::create(err)?)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let attached = format!("Process {pid} attached");
    loop {
        if fs::read_to_string(err)?.contains(&attached) {
            return Ok(child);
        }
        if child.try_wait()?.is_some() || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::other(format!(
                "strace did not attach to {pid}: {}",
                fs::read_to_string(err).unwrap_or_default().trim()
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// What the faulted run left, before any check.
struct Observed {
    schedule: baseline::Report,
    /// The RPC error that ended the schedule, if one did.
    ended: Option<io::Error>,
    /// When the fault was applied, if it was.
    injected: Option<Result<Micros, String>>,
    /// The flush line a flush-line trigger fired on.
    trigger_line: Option<Micros>,
    /// What went wrong with FL-FLUSH's freeze, if anything did.
    thaw: Option<String>,
    /// rdashd's process.
    pid: u32,
    /// How rdashd's run ended.
    termination: Termination,
    exit: Result<crate::Stopped, crate::StopError>,
    /// strace's exit status; `None` if it had to be killed.
    tracer: Option<ExitStatus>,
    log: String,
    stderr: String,
}

/// The node part of a fault run: rdashd on the mounted faulted filesystem `mnt` (its logs in
/// `<dir>/node`), traced, through the schedule until it dies; the fault applied by `gate` after
/// the stored blocks' acknowledgement `k` plus `delay`. `mark` marks the log for the schedule,
/// `mark_exit` its `exit` mark, at once when rdashd is gone.
#[allow(clippy::too_many_arguments)]
fn observe(
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    dir: &Path,
    mnt: &Path,
    (mark, mark_exit): (baseline::Marker<'_>, &MarkExit<'_>),
    gate: &Gate<'_>,
    (k, delay): (usize, u64),
) -> io::Result<Observed> {
    let work = dir.join("peer");
    private_dir(&work)?;
    let peer = Peer::start(&cfg.dashd, fixture, &work)?;
    let mut extra = cfg.extra.clone();
    extra.push("-logtimemicros".to_owned());
    let config = RustNodeConfig::with_binary(&cfg.rdashd, mnt.join(DATADIR))
        .with_mocktime(Some(MockTime::at(fixture.mocktime)))
        .with_connect(peer.p2p)
        .with_rpc(find_available_port())
        .with_args(node_args(fixture, &extra))
        .with_log_dir(dir.join("node"));
    let log_path = config.debug_log_path();
    let stderr_path = dir.join("node").join("rdashd.stderr");
    let node = rt.block_on(RustNode::start(config)).map_err(io::Error::other)?;
    if let Some(why) = baseline::profile_failure(&node.log(), &node_args(fixture, &extra), true) {
        return Err(io::Error::other(why));
    }
    let mut tracer = attach_tracer(node.pid(), &dir.join("strace.txt"), &dir.join("strace.err"))?;
    let rpc = node.rpc_with_timeout(RPC_PATIENCE).map_err(io::Error::other)?;
    let (stop, trigger_line, thaw) = (AtomicBool::new(false), Mutex::new(None), Mutex::new(None));
    let (pid, release) = (node.pid(), AtomicBool::new(false));
    let started = || gate.started.get().copied();
    let (schedule, ended, injected, termination, exit, marked) = std::thread::scope(|s| {
        let _ends = Raise([&stop, &release]);
        let watcher = s.spawn(|| watch_exit(pid, &started, BOUND, &release, mark_exit));
        let (stop, trigger_line, thaw, log_path) = (&stop, &trigger_line, &thaw, &log_path);
        let mut hook = |i: usize| -> io::Result<()> {
            if i == k {
                match cfg.case {
                    Case::DfPre | Case::FlWal => gate.inject(),
                    Case::FlIbd => {
                        s.spawn(move || sleep_unless(stop, delay).then(|| gate.inject()));
                    }
                    Case::FlFlush | Case::DfMid => {
                        if cfg.case == Case::DfMid {
                            gate.prefill()?;
                        }
                        let from = fs::metadata(log_path).map_or(0, |m| m.len());
                        s.spawn(move || {
                            let line = watch_flush_line(log_path, from, stop)?;
                            *trigger_line.lock().expect("not poisoned") = Some(line);
                            let frozen = (cfg.case == Case::FlFlush).then(|| Freeze::new(pid));
                            let injected = sleep_unless(stop, delay).then(|| gate.inject());
                            if let Some(Err(e)) = frozen.map(Freeze::thaw) {
                                *thaw.lock().expect("not poisoned") = Some(e);
                            }
                            injected?;
                            if cfg.case == Case::DfMid {
                                gate.keep_full(stop);
                            }
                            Some(())
                        });
                    }
                }
            }
            Ok(())
        };
        let until = UntilDeath {
            armed: gate.armed,
            hook: &mut hook,
        };
        let (schedule, ended) =
            rt.block_on(baseline::until_death(&rpc, fixture, cfg.every, &mut *mark, until));
        let injected = gate.close();
        stop.store(true, Ordering::SeqCst);
        // FL-WAL's schedule ran to its end only if no flush met the fault (the WAL switched
        // first). With no fault in place (never armed, not applied, or FL-WAL untouched) the
        // harness stops rdashd itself; otherwise the watcher waits out its exit or the bound.
        let untouched = cfg.case == Case::FlWal && ended.is_none();
        // The fault is lifted before the stop, so that shutdown's own flush can't meet it (Sol r1
        // M1), and while the watcher still watches, so that an exit meanwhile stays rdashd's own
        // (Sol r3 N1). A stop is never a fail-stop (`judge_end`).
        let lifted = if untouched && injected.is_some() {
            gate.swap(false)
        } else {
            Ok(())
        };
        if untouched || !matches!(injected, Some(Ok(_))) {
            release.store(true, Ordering::SeqCst);
        }
        let watched = watcher.join().expect("the exit watcher does not panic");
        let (termination, exit, marked) = match watched {
            Some((how, marked)) => (how, rt.block_on(node.wait_exit(PATIENCE)), marked.and(lifted)),
            None => {
                let exit = rt.block_on(node.stop());
                (Termination::Stopped, exit, mark_exit().and(lifted))
            }
        };
        (schedule, ended, injected, termination, exit, marked)
    });
    // strace exits by itself after its tracee; how is judged with the trace (Opus r1 m2).
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut tracer_end = tracer.try_wait()?;
    while tracer_end.is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        tracer_end = tracer.try_wait()?;
    }
    if tracer_end.is_none() {
        let _ = tracer.kill();
        let _ = tracer.wait();
    }
    peer.stop()?;
    marked?;
    let log = match &exit {
        Ok(stopped) => stopped.log.clone(),
        Err(_) => fs::read_to_string(&log_path).unwrap_or_default(),
    };
    Ok(Observed {
        schedule,
        ended,
        injected,
        trigger_line: trigger_line.into_inner().expect("not poisoned"),
        thaw: thaw.into_inner().expect("not poisoned"),
        pid,
        termination,
        exit,
        tracer: tracer_end,
        log,
        stderr: fs::read_to_string(&stderr_path).unwrap_or_default(),
    })
}

/// The state a power cut at rdashd's exit can leave at least (S6 plan §3.3): the entries before
/// the last FLUSH before the mark at `exit` (an index), then every FUA write from that FLUSH up to
/// the mark (a FUA write is durable by itself; a PREFLUSH write's data is not).
pub fn power_loss_point(log: &Log, exit: usize) -> Point {
    let flush = (0..exit).rev().find(|&i| log.entries[i].is(FLUSH)).unwrap_or(0);
    let extra =
        (flush..exit).filter(|&i| log.entries[i].is(FUA) && log.entries[i].has_data()).collect();
    Point {
        kind: PointKind::Torn,
        prefix: flush,
        extra,
        part: None,
    }
}

/// The floor of a fault run (S6 plan §3.4): the trajectory position of its last successful
/// durability reply, whatever came after it; none, the start.
pub fn fault_floor(acks: &[usize]) -> Floor {
    acks.last().map_or(Floor::Start, |&p| Floor::Ack(p))
}

/// The campaign settings [`check_image`] reads, for a fault run.
fn check_config(cfg: &Config) -> campaign::Config {
    campaign::Config {
        fs: cfg.fs,
        draw: Draw::default(),
        rdashd: cfg.rdashd.clone(),
        dashd: cfg.dashd.clone(),
        extra: cfg.extra.clone(),
        every: cfg.every,
        c5_every: 1,
        c6_every: usize::MAX,
        c6_mid_every: 1,
        c6_max: usize::MAX,
        shard: (0, 1),
        points: 0..=usize::MAX,
        split_chainstate: false,
        plant: None,
        torn_only: false,
        c0_only: false,
        sample: 1,
    }
}

/// Removes the filler from the image `img` (absent is fine) and checks that at least
/// [`FREE_FOR_CHECKS`] is then available (S6 plan §3.6), before a check writes to it.
fn free_filler(
    p: &Priv,
    cc: &campaign::Config,
    img: &Path,
    mnt: &Path,
) -> io::Result<Result<(), String>> {
    campaign::mounted(p, cc, img, mnt, |datadir| {
        let root = datadir.parent().expect("the datadir is in the mount");
        match fs::remove_file(root.join(FILLER)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(format!("the filler: {e}"));
            }
            _ => {}
        }
        // xfs frees an unlinked file's blocks in the background (inode inactivation); syncfs
        // waits for that, and the space may still take a moment to show.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            syncfs(root).map_err(|e| format!("syncfs: {e}"))?;
            match available(root) {
                Ok(n) if n >= FREE_FOR_CHECKS => return Ok(()),
                Ok(n) if Instant::now() >= deadline => {
                    return Err(format!("{n} bytes available without the filler"));
                }
                Ok(_) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => return Err(format!("statvfs: {e}")),
            }
        }
    })
    .map(|r| r.and_then(|r| r))
}

/// `syncfs(2)` on the filesystem holding `path`.
fn syncfs(path: &Path) -> io::Result<()> {
    let dir = File::open(path)?;
    // SAFETY: an open descriptor.
    if unsafe { libc::syncfs(dir.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// C0–C4 on a copy of `src` (and C5 when `c5` is given), the filler removed first.
#[allow(clippy::too_many_arguments)]
fn check_copy(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cc: &campaign::Config,
    (src, name): (&File, &str),
    floor: Floor,
    seed: u64,
    c5: Option<PathBuf>,
) -> io::Result<Vec<String>> {
    let img = run.dir().join(format!("{name}.img"));
    let img_c0 = run.dir().join(format!("{name}.c0.img"));
    drop(reflink(src, &img)?);
    drop(reflink(src, &img_c0)?);
    let (mnt_c0, mnt_node) = (run.mountpoint("c0")?, run.mountpoint("pt")?);
    let mut failures = Vec::new();
    for copy in [&img, &img_c0] {
        if let Err(e) = free_filler(p, cc, copy, &mnt_c0)? {
            failures.push(format!("{name}: {e}"));
        }
    }
    if failures.is_empty() {
        let mnts = (mnt_c0.as_path(), mnt_node.as_path());
        let mut phases = Phases::default();
        let checked = check_image(
            p,
            rt,
            fixture,
            cc,
            (&img, &img_c0),
            mnts,
            floor,
            seed,
            &mut phases,
            false,
            |_| c5,
        )?;
        failures.extend(checked.failures.into_iter().map(|f| format!("{name}: {f}")));
    }
    if failures.is_empty() {
        remove_own_file(&img)?;
        remove_own_file(&img_c0)?;
    }
    Ok(failures)
}

/// The failures of a run the fault never reached (Opus r1 M2): a schedule that ended in an error
/// (the node's, with no fault in place), the schedule's own failures, rdashd's exit, and its end:
/// only a clean harness stop is none.
fn unfaulted(obs: &Observed) -> Vec<String> {
    let mut failures = Vec::new();
    if let Some(e) = &obs.ended {
        failures.push(format!("the schedule failed before the fault: {e}"));
    }
    failures.extend(obs.schedule.failures.iter().map(|f| format!("schedule: {f}")));
    match &obs.exit {
        Ok(stopped) => {
            if obs.termination != Termination::Stopped {
                failures.push("rdashd ended by itself with no fault applied".to_owned());
            }
            failures.extend(clean_stop(&obs.log, &obs.stderr, stopped.status.code()));
        }
        Err(e) => failures.push(format!("rdashd's exit: {e}")),
    }
    failures
}

/// A causal abort stamped before the injection `injected` (Opus r1 M1): rdashd's own doing,
/// whatever its text and whatever failed after it, so never a fault run's fail-stop. The stamp is
/// taken before the fault is applied, so nothing the fault caused can come before it.
pub fn abort_before_fault(log: &str, injected: Micros) -> Option<String> {
    let (t, msg) = trace::first_abort(log)?;
    (t < injected).then(|| format!("the abort {msg:?} at {t} came before the fault at {injected}"))
}

/// Why the trace `traced` can't be the whole run's, if it can't (Opus r1 m2): strace must have
/// exited by itself (`tracer`) with status 0, and its last word on rdashd (`pid`) must be
/// rdashd's own end, its exit `code` or a kill. A tracer that died early leaves failures out.
fn trace_gap(
    traced: &str,
    pid: u32,
    tracer: Option<ExitStatus>,
    code: Option<i32>,
) -> Option<String> {
    let end = trace::tracee_end(traced, pid);
    let whole = match (end, code) {
        (Some(e), Some(c)) => e == format!("exited with {c}"),
        (Some(e), None) => e.starts_with("killed by "),
        (None, _) => false,
    };
    match tracer {
        Some(s) if !s.success() => Some(format!("unjudgeable: strace {s}")),
        Some(_) if whole => None,
        Some(_) => Some(format!("unjudgeable: the trace ends {end:?}, not with rdashd's {code:?}")),
        None => Some("unjudgeable: strace outlived rdashd by 60 s and was killed".to_owned()),
    }
}

/// One attempt of a case (S6 plan §1–§3) in `run`: the faulted stack, the traced node until it
/// dies, then its fail-stop, the case's coverage, the power-loss state and the repaired device.
pub fn run(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
) -> io::Result<Report> {
    let stored_acks = fixture.submit.len() / cfg.every + 1;
    let (k, delay) = draw(cfg.case, cfg.seed, stored_acks).ok_or_else(|| {
        io::Error::other(format!("{stored_acks} acknowledgements of stored blocks: lower --every"))
    })?;
    let mut report = Report {
        trigger: format!("stored-block acknowledgement {k}, then {delay} µs"),
        ..Report::default()
    };
    let dir = run.dir();
    let (data, log_img) = (dir.join("data.img"), dir.join("log.img"));
    sparse_file(&data, DATA_LEN)?;
    sparse_file(&log_img, LOG_LEN)?;
    let mnt = run.mountpoint("flt")?;
    let (lw, fl, len) = (run.dm("lw"), run.dm("fl"), sectors(DATA_LEN));
    let mut stack = Stack::new(p);
    let data_dev = stack.attach(&data)?;
    let log_dev = stack.attach(&log_img)?;
    let lw_dev = stack.dm_create(&lw, &format!("0 {len} log-writes {data_dev} {log_dev}"))?;
    let lw_dev = lw_dev.to_string_lossy().into_owned();
    let fl_dev = stack.dm_create(&fl, &fault_table(&lw_dev, len, false))?;
    // dm-log-writes copies a write's data when it maps the bio, and without stable pages a page
    // can change before the device reads it. The page's next write logs the change, unless the
    // fault fails that write: then the device holds bytes the log never saw (S6 smoke, 10-31).
    p.run(&["stable-writes", &fl])?;
    stack.mkfs(&fl_dev, cfg.fs)?;
    stack.mount(&fl_dev, &mnt, cfg.fs, false)?;
    stack.chown_mnt(&mnt)?;
    File::open(&mnt)?.sync_all()?;
    stack.message(&lw, &["mark", "start"])?;
    let armed = AtomicBool::new(false);
    let gate = Gate {
        p,
        case: cfg.case,
        fl: &fl,
        under: &lw_dev,
        len,
        mnt: &mnt,
        armed: &armed,
        state: Mutex::new((false, None)),
        started: OnceLock::new(),
    };
    let observed = {
        let mut mark = |name: &str| stack.message(&lw, &["mark", name]);
        let mark_exit = || stack.message(&lw, &["mark", "exit"]);
        observe(rt, fixture, cfg, dir, &mnt, (&mut mark, &mark_exit), &gate, (k, delay))
    };
    let restored = if cfg.case.flakey() {
        gate.swap(false)
    } else {
        Ok(())
    };
    stack.teardown().map_err(io::Error::other)?;
    let obs = observed?;
    restored?;

    report.failures.extend(obs.thaw.iter().map(|e| format!("the freeze: {e}")));
    let injected = match &obs.injected {
        Some(Ok(t)) => *t,
        Some(Err(e)) => return Err(io::Error::other(format!("applying the fault: {e}"))),
        None => {
            report.failures.extend(unfaulted(&obs));
            if report.failures.is_empty() {
                report.miss = Some(format!("{} missed: the trigger never fired", cfg.case));
                clean(dir)?;
            }
            return Ok(report);
        }
    };
    report.failures.extend(obs.schedule.failures.iter().map(|f| format!("schedule: {f}")));
    let code = match &obs.exit {
        Ok(stopped) => stopped.status.code(),
        Err(e) => {
            report.failures.push(format!("rdashd's exit: {e}"));
            None
        }
    };
    let traced = fs::read_to_string(dir.join("strace.txt"))?;
    report.failures.extend(trace_gap(&traced, obs.pid, obs.tracer, code));
    let calls = trace::parse(&traced).map_err(io::Error::other)?;
    let scope = Scope {
        mount: &mnt,
        injected,
    };
    let fatal = trace::fatal(&calls, &scope);
    let first = fatal.first().copied();
    report.first_failure = first.map(|c| format!("{c:?}"));
    report.failures.extend(abort_before_fault(&obs.log, injected));
    let abort_at = trace::first_abort(&obs.log).map(|(t, _)| t);
    let mut errnos: Vec<i32> = fatal
        .iter()
        .filter(|c| abort_at.is_none_or(|t| c.done <= t))
        .filter_map(|c| trace::errno_number(c.errno()?))
        .collect();
    errnos.sort_unstable();
    errnos.dedup();
    let failure = (!errnos.is_empty()).then_some(Failure {
        errnos: &errnos,
        mount: &mnt,
    });
    let end = (obs.termination, !fatal.is_empty());
    match judge_end(end, &obs.log, &obs.stderr, code, failure) {
        Ok(Some(stopped)) => {
            report.abort = Some(stopped.abort);
            report.repeats = stopped.repeats;
        }
        Ok(None) => {}
        Err(f) => report.failures.extend(f),
    }
    let interval = obs.trigger_line.and_then(|t| trace::flush_interval(&obs.log, t));
    report.miss = trace::coverage(cfg.case, &fatal, trace::first_abort(&obs.log), interval).err();

    // The power-loss state, then the repaired device, which the whole log replays to.
    let floor = fault_floor(&obs.schedule.acks);
    report.floor = Some(floor);
    let log = Log::open(&log_img).map_err(io::Error::other)?;
    let exit = log.mark("exit").ok_or_else(|| io::Error::other("no exit mark"))? - 1;
    let pt = power_loss_point(&log, exit);
    let base = sparse_file(&dir.join("replay.img"), DATA_LEN)?;
    let mut replayer = Replayer::new(&log, File::open(&log_img)?, base);
    let cc = check_config(cfg);
    let pl = replayer.snapshot(&pt, &dir.join("pl-snap.img"))?;
    let failed = check_copy(p, run, rt, fixture, &cc, (&pl, "power-loss"), floor, cfg.seed, None)?;
    drop(pl);
    // The pristine power-loss image stays with a failing check (S6 plan §3.6, Sol r1 m1).
    if failed.is_empty() {
        remove_own_file(&dir.join("pl-snap.img"))?;
    }
    // dm-log-writes logs the writes a flush found completed when that flush completes, and the
    // loop device under it serves each cgroup's flushes from its own worker, out of order: a
    // block's older copy can then follow its newer one, and the power-loss image is no state the
    // device passed through. The device decides: a capture it contradicts is a miss, retried, and
    // its power-loss findings are set aside (OD-ST3b-8).
    if let Some(at) = replayer.whole_log_differs(&data)? {
        let capture = format!("capture: the whole log replayed differs from data.img at byte {at}");
        report.miss = Some(match report.miss.take() {
            Some(m) => format!("{capture}; {m}"),
            None => capture,
        });
        report.set_aside = failed;
    } else {
        report.failures.extend(failed);
    }
    drop(replayer);
    remove_own_file(&dir.join("replay.img"))?;
    let c5 = Some(dir.join("c5"));
    let repaired = File::open(&data)?;
    let failed =
        check_copy(p, run, rt, fixture, &cc, (&repaired, "repaired"), floor, cfg.seed, c5)?;
    report.failures.extend(failed);
    // `ST3B_KEEP_RUNS` keeps a passing or missed attempt's files too, for a look at its trace.
    if report.failures.is_empty() && std::env::var_os("ST3B_KEEP_RUNS").is_none() {
        clean(dir)?;
    }
    Ok(report)
}

/// Removes a passing attempt's files from its run directory `dir`; a failing one keeps them.
fn clean(dir: &Path) -> io::Result<()> {
    for f in ["data.img", "log.img", "strace.txt", "strace.err"] {
        let path = dir.join(f);
        if path.exists() {
            remove_own_file(&path)?;
        }
    }
    for d in ["peer", "node"] {
        let path = dir.join(d);
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIP: &str = "2026-10-31T00:00:01Z UpdateTip: new best=ab height=7";

    fn log(lines: &[&str]) -> String {
        lines.iter().map(|l| format!("{l}\n")).collect()
    }

    #[test]
    fn a_sanctioned_abort_with_exit_1_and_no_later_tip_is_a_fail_stop() {
        let msg = "System error while flushing: IO error: While appending: x: Input/output error";
        let text = log(&[TIP, &format!("2026-10-31T00:00:02.000001Z *** {msg}")]);
        assert_eq!(
            fail_stop(&text, "", Some(1), Some(eio())),
            Ok(FailStop {
                abort: msg.to_owned(),
                repeats: 0,
            })
        );
        for s in EXACT {
            assert!(fail_stop(&log(&[&format!("*** {s}")]), "", Some(1), None).is_ok(), "{s}");
        }
    }

    #[test]
    fn shutdown_may_report_the_failure_again() {
        let low = "*** Disk space is too low!";
        let text = log(&[TIP, low, "Error: Disk space is too low!", low]);
        assert_eq!(fail_stop(&text, "", Some(1), None).map(|f| f.repeats), Ok(1));
        let latched = format!("*** {LOW_SPACE_REPEAT}");
        assert_eq!(fail_stop(&log(&[low, &latched]), "", Some(1), None).map(|f| f.repeats), Ok(1));
        let first = fail_stop(&log(&[&latched]), "", Some(1), None).unwrap_err();
        assert!(first[0].contains("the abort"), "{first:?}");
        let repeat = "*** Corrupt block found indicating potential hardware failure";
        let fails = fail_stop(&log(&[low, repeat]), "", Some(1), None).unwrap_err();
        assert!(fails[0].contains("repeat report 1"), "{fails:?}");
    }

    fn eio() -> Failure<'static> {
        Failure {
            errnos: &[libc::EIO],
            mount: Path::new("/mnt/r"),
        }
    }

    #[test]
    fn a_system_error_must_name_the_observed_errno() {
        let text = "System error while flushing: IO error: Input/output error";
        let enospc = Failure {
            errnos: &[libc::ENOSPC],
            ..eio()
        };
        assert!(sanctioned(text, Some(eio())));
        assert!(!sanctioned(text, Some(enospc)));
        assert!(!sanctioned(text, None));
        assert!(sanctioned("System error: No space left on device", Some(enospc)));
        // A later job's error: ext4 went read-only after the failed sync (OD-ST3b-8).
        let erofs = "System error while flushing: rocksdb error (Io): IO error: While open a file \
                     for appending: /mnt/r/c/000239.sst: Read-only file system";
        let both = Failure {
            errnos: &[libc::EIO, libc::EROFS],
            ..eio()
        };
        assert!(sanctioned(erofs, Some(both)));
        assert!(!sanctioned(erofs, Some(eio())));
        assert!(!sanctioned("Failed to write block extra", Some(eio())));
        assert!(!sanctioned("Failed to write to coin database", Some(eio())));
    }

    #[test]
    fn a_flat_file_abort_names_a_file_on_the_faulted_mount_and_the_errno() {
        // As rdashd's flush renders a failed files barrier (`FlatFileError::Io`).
        let err = dashd_store::flatfile::FlatFileError::Io {
            context: EXACT[1],
            path: "/mnt/r/rdashd/regtest/blocks/blk00000.dat".into(),
            source: io::Error::from_raw_os_error(libc::EIO),
        };
        let msg = err.to_string();
        assert!(sanctioned(&msg, Some(eio())), "{msg}");
        let elsewhere = Failure {
            mount: Path::new("/mnt/other"),
            ..eio()
        };
        assert!(!sanctioned(&msg, Some(elsewhere)));
        let enospc = Failure {
            errnos: &[libc::ENOSPC],
            ..eio()
        };
        assert!(!sanctioned(&msg, Some(enospc)));
        assert!(!sanctioned(&msg, None));
    }

    #[test]
    fn each_broken_fail_stop_is_named() {
        let abort = "*** Disk space is too low!";
        let fails =
            |text: &str, stderr: &str, code| fail_stop(text, stderr, code, None).unwrap_err();
        assert!(fails(&log(&[abort]), "", Some(0))[0].contains("exit code"));
        assert!(fails(&log(&[abort]), "", None)[0].contains("exit code"));
        assert!(fails(&log(&[TIP]), "", Some(1))[0].contains("no AbortNode"));
        assert!(fails(&log(&[abort, TIP]), "", Some(1))[0].contains("a new tip after"));
        let corrupt = "*** Corrupt block found indicating potential hardware failure";
        assert!(fails(&log(&[corrupt]), "", Some(1))[0].contains("not an I/O"));
        let panic = "thread 'main' panicked at src/x.rs:1:1";
        assert!(fails(&log(&[abort]), panic, Some(1))[0].contains("a panic in stderr"));
        // A tip before the abort is the sync's own.
        assert!(fail_stop(&log(&[TIP, abort]), "", Some(1), None).is_ok());
    }

    #[test]
    fn only_an_exit_of_its_own_can_be_a_fail_stop() {
        let msg = "System error while flushing: IO error: While fdatasync: x: Input/output error";
        let aborted = log(&[TIP, &format!("2026-10-31T00:00:02.000001Z *** {msg}")]);
        let own = judge_end((Termination::Own, true), &aborted, "", Some(1), Some(eio()));
        assert_eq!(own.unwrap().unwrap().abort, msg);
        // A fatal WAL failure, the schedule run to its end, then a sanctioned abort in the
        // shutdown the harness asked for: not a fail-stop (Sol r2 M1).
        let stopped = judge_end((Termination::Stopped, true), &aborted, "", Some(1), Some(eio()));
        assert!(stopped.unwrap_err()[0].contains("went on after a fatal failure"));
        // The same abort after the bound's kill (Sol r2 M5).
        let killed = judge_end((Termination::Deadline, true), &aborted, "", Some(1), Some(eio()));
        assert!(killed.unwrap_err()[0].contains("no fail-stop within"));
        // Nothing failed and nothing met the fault: a clean stop is due, no fail-stop.
        assert_eq!(
            judge_end((Termination::Stopped, false), &log(&[TIP]), "", Some(0), None),
            Ok(None)
        );
        assert!(judge_end((Termination::Stopped, false), &aborted, "", Some(1), None).is_err());
    }

    fn child(script: &str) -> std::process::Child {
        Command::new("sh").args(["-c", script]).spawn().expect("sh starts")
    }

    /// `script`'s child, gone (unreaped) before this returns.
    fn exited_child(script: &str) -> std::process::Child {
        let c = child(script);
        while !exited(c.id()).unwrap() {
            std::thread::sleep(Duration::from_millis(1));
        }
        c
    }

    #[test]
    fn the_exit_watcher_marks_an_exit_and_kills_at_the_bound() {
        let marks = std::sync::atomic::AtomicUsize::new(0);
        let mark = || {
            marks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let (held, released) = (AtomicBool::new(false), AtomicBool::new(true));
        let at = Instant::now();
        let armed = move || Some(at);
        let watch = |c: &std::process::Child, bound, release| {
            watch_exit(c.id(), &armed, bound, release, &mark).map(|(t, m)| (t, m.is_ok()))
        };
        // An exit of its own before the bound.
        let mut c = child("exit 1");
        assert_eq!(watch(&c, Duration::from_secs(60), &held), Some((Termination::Own, true)));
        assert_eq!(c.wait().unwrap().code(), Some(1), "left for the node's own wait to reap");
        // Still running at the bound, as while the schedule waits on an outstanding call: killed.
        let mut c = child("sleep 30");
        let begun = Instant::now();
        let seen = watch(&c, Duration::from_millis(200), &held);
        assert_eq!(seen, Some((Termination::Deadline, true)));
        assert!(begun.elapsed() < Duration::from_secs(10));
        assert_eq!(c.wait().unwrap().code(), None, "killed by a signal");
        // A watcher delayed past the bound finds an exit of status 1 already there: its time is
        // unknown, so it is the bound's, never a pass (Sol r3 M5).
        std::thread::sleep(Duration::from_millis(20));
        let mut c = exited_child("exit 1");
        let seen = watch(&c, Duration::from_millis(10), &held);
        assert_eq!(seen, Some((Termination::Deadline, true)));
        assert_eq!(c.wait().unwrap().code(), Some(1));
        // Gone during the handoff: rdashd's own, though the harness released the watch (Sol r3
        // N1).
        let mut c = exited_child("exit 1");
        assert_eq!(watch(&c, Duration::from_secs(60), &released), Some((Termination::Own, true)));
        c.wait().unwrap();
        // Released while it runs: the harness stops it.
        let mut c = child("sleep 30");
        assert_eq!(watch(&c, Duration::from_secs(60), &released), None);
        c.kill().unwrap();
        c.wait().unwrap();
        // Never armed: no bound.
        let mut c = exited_child("exit 0");
        let none = watch_exit(c.id(), &|| None, Duration::ZERO, &held, &mark);
        assert_eq!(none.map(|(t, _)| t), Some(Termination::Own));
        c.wait().unwrap();
        assert_eq!(marks.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn an_exit_seen_after_the_bound_is_the_bounds() {
        let bound = Duration::from_secs(60);
        let expired_at = Instant::now().checked_sub(bound * 2).expect("the box has been up 2 min");
        let expired = move || Some(expired_at);
        let fresh = || Some(Instant::now());
        let kills = std::sync::atomic::AtomicUsize::new(0);
        let kill = || {
            kills.fetch_add(1, Ordering::SeqCst);
        };
        // In time at the bound check; the observation then finds a status-1 exit with the bound
        // gone by (the test's clock moves past it inside the look): the bound's, though nothing
        // was killed (Sol r4 M5). Driven by `started`, not by real time (Sol r5 N2).
        let passed = AtomicBool::new(false);
        let started = || {
            if passed.load(Ordering::SeqCst) {
                expired()
            } else {
                fresh()
            }
        };
        let paused = || {
            passed.store(true, Ordering::SeqCst);
            Ok(true)
        };
        let mut killed = false;
        let seen = look(&started, bound, &mut killed, (&kill, &paused)).unwrap();
        assert_eq!((seen, kills.load(Ordering::SeqCst)), (Some(Termination::Deadline), 0));
        // Seen gone with the bound still holding after the look: its own.
        let mut killed = false;
        let seen = look(&fresh, bound, &mut killed, (&kill, &|| Ok(true))).unwrap();
        assert_eq!(seen, Some(Termination::Own));
        // Alive: nothing yet; alive past the bound: killed, and its exit the bound's after that.
        let mut killed = false;
        assert_eq!(look(&fresh, bound, &mut killed, (&kill, &|| Ok(false))).unwrap(), None);
        assert!(!killed && kills.load(Ordering::SeqCst) == 0);
        assert_eq!(look(&expired, bound, &mut killed, (&kill, &|| Ok(false))).unwrap(), None);
        assert!(killed && kills.load(Ordering::SeqCst) == 1);
        let seen = look(&fresh, bound, &mut killed, (&kill, &|| Ok(true))).unwrap();
        assert_eq!(seen, Some(Termination::Deadline), "never Own once killed");
        // An observation error is passed on (the watcher ends as the bound's).
        let mut killed = false;
        let failed = || Err(io::Error::other("waitid"));
        assert!(look(&fresh, bound, &mut killed, (&kill, &failed)).is_err());
    }

    /// A failed call of thread 1 at `done` on `path` under `/m`, with `errno`.
    fn failed(done: Micros, path: &str, name: &str, errno: &str) -> trace::Call {
        trace::Call {
            tid: 1,
            done,
            name: name.to_owned(),
            path: Some(PathBuf::from(format!("/m/rdashd/regtest/{path}"))),
            ret: trace::Ret::Err(errno.to_owned()),
        }
    }

    #[test]
    fn an_abort_before_the_fault_is_rdashds_own() {
        let at = |secs: &str| trace::log_time(&format!("2026-10-31T00:00:{secs}Z x")).unwrap();
        let injected = at("10.000000");
        // FL-IBD: an exact-text abort of its own, then the late injection, then shutdown's chainstate
        // write meets the fault. Coverage alone passes it; the order refuses it.
        let msg = EXACT[1];
        let text = log(&[&format!("2026-10-31T00:00:09.999999Z *** {msg}")]);
        let eio = failed(at("10.200000"), "chainstate/000009.log", "write", "EIO");
        let fatal = [&eio];
        let abort = trace::first_abort(&text);
        assert_eq!(trace::coverage(Case::FlIbd, &fatal, abort, None), Ok(()));
        assert!(abort_before_fault(&text, injected).is_some_and(|f| f.contains(msg)));
        // DF-PRE: a low-space refusal of its own just before the fill.
        let text = log(&[&format!("2026-10-31T00:00:09.500000Z *** {}", EXACT[0])]);
        let abort = trace::first_abort(&text);
        assert_eq!(trace::coverage(Case::DfPre, &[], abort, None), Ok(()));
        assert!(abort_before_fault(&text, injected).is_some());
        // At or after the injection: the fault's run to judge.
        for t in ["10.000000", "10.300000"] {
            let text = log(&[&format!("2026-10-31T00:00:{t}Z *** {}", EXACT[0])]);
            assert_eq!(abort_before_fault(&text, injected), None, "{t}");
        }
        assert_eq!(abort_before_fault(&log(&[TIP]), injected), None);
    }

    #[test]
    fn the_flush_trigger_never_arms_after_an_abort() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("debug.log");
        let flush = "2026-10-31T00:00:03.000000Z Writing final batch of 7 coins";
        let abort = format!("2026-10-31T00:00:02.000000Z *** {}", EXACT[1]);
        let never = AtomicBool::new(false);
        fs::write(&path, log(&[TIP, flush])).unwrap();
        assert_eq!(watch_flush_line(&path, 0, &never), trace::log_time(flush));
        // Shutdown's forced flush after an abort of its own: no trigger, no fault.
        fs::write(&path, log(&[TIP, &abort, flush])).unwrap();
        assert_eq!(watch_flush_line(&path, 0, &never), None);
    }

    fn observed(injected: Option<Result<Micros, String>>, termination: Termination) -> Observed {
        use std::os::unix::process::ExitStatusExt;
        Observed {
            schedule: baseline::Report::default(),
            ended: None,
            injected,
            trigger_line: None,
            thaw: None,
            pid: 7,
            termination,
            exit: Ok(crate::Stopped {
                pid: 7,
                status: ExitStatus::from_raw(0),
                log: String::new(),
            }),
            tracer: Some(ExitStatus::from_raw(0)),
            log: String::new(),
            stderr: String::new(),
        }
    }

    #[test]
    fn a_run_the_fault_never_reached_keeps_its_failures() {
        use std::os::unix::process::ExitStatusExt;
        // A clean harness stop and nothing else: the miss.
        assert!(unfaulted(&observed(None, Termination::Stopped)).is_empty());
        // The schedule's own failures stay (a tip off the trajectory, say).
        let mut obs = observed(None, Termination::Stopped);
        obs.schedule.failures.push("tip ab is not on the trajectory".to_owned());
        assert_eq!(unfaulted(&obs), ["schedule: tip ab is not on the trajectory"]);
        // The stop is judged: a panic, a bad exit.
        let mut obs = observed(None, Termination::Stopped);
        obs.stderr = "thread 'main' panicked at x".to_owned();
        assert_eq!(unfaulted(&obs).len(), 1);
        let mut obs = observed(None, Termination::Stopped);
        obs.exit.as_mut().unwrap().status = ExitStatus::from_raw(1 << 8);
        assert_eq!(unfaulted(&obs).len(), 1);
        // An end of its own with no fault, even with a sanctioned text, and a schedule cut short.
        let mut obs = observed(None, Termination::Own);
        obs.ended = Some(io::Error::other("connection refused"));
        obs.exit.as_mut().unwrap().status = ExitStatus::from_raw(1 << 8);
        obs.log = log(&[&format!("*** {}", EXACT[0])]);
        let failures = unfaulted(&obs);
        assert!(failures[0].starts_with("the schedule failed before the fault"), "{failures:?}");
        assert!(failures.iter().any(|f| f.contains("ended by itself")), "{failures:?}");
    }

    #[test]
    fn a_trace_must_reach_rdashds_own_end() {
        use std::os::unix::process::ExitStatusExt;
        let ok = Some(ExitStatus::from_raw(0));
        let exited = "7 10.0 +++ exited with 1 +++\n";
        assert_eq!(trace_gap(exited, 7, ok, Some(1)), None);
        assert_eq!(trace_gap("7 10.0 +++ killed by SIGKILL +++\n", 7, ok, None), None);
        // Stopped short, another code, another thread's end, a failed or killed tracer.
        assert!(trace_gap("7 10.0 write(1</m/x>, \"\", 1) = 1 <0.1>\n", 7, ok, Some(1)).is_some());
        assert!(trace_gap(exited, 7, ok, Some(0)).is_some());
        assert!(trace_gap(exited, 8, ok, Some(1)).is_some());
        assert!(trace_gap(exited, 7, Some(ExitStatus::from_raw(1 << 8)), Some(1)).is_some());
        assert!(trace_gap(exited, 7, None, Some(1)).is_some());
    }

    #[test]
    fn a_freeze_stops_every_thread_and_a_thaw_or_its_drop_ends_it() {
        let mut c = child("sleep 30");
        let pid = c.id();
        let frozen = Freeze::new(pid);
        assert_eq!(stopped(pid), Some(true));
        assert_eq!(frozen.thaw(), Ok(()));
        assert_eq!(stopped(pid), Some(false));
        // Dropped without a thaw, as on a panic: continued all the same.
        drop(Freeze::new(pid));
        assert_eq!(stopped(pid), Some(false));
        c.kill().unwrap();
        c.wait().unwrap();
        assert_eq!(stopped(pid), None, "gone");
    }

    #[test]
    fn a_raise_sets_its_flags_however_the_scope_ends() {
        let flags = [AtomicBool::new(false), AtomicBool::new(false)];
        let unwound = std::panic::catch_unwind(|| {
            let _ends = Raise([&flags[0], &flags[1]]);
            panic!("in the scope's body");
        });
        assert!(unwound.is_err());
        assert!(flags.iter().all(|f| f.load(Ordering::SeqCst)));
    }

    #[test]
    fn a_stop_with_no_fault_met_must_be_clean() {
        assert!(clean_stop(&log(&[TIP]), "", Some(0)).is_empty());
        assert!(clean_stop("", "", Some(1))[0].contains("exit code Some(1)"));
        let panic = "thread 'main' panicked at src/x.rs:1:1";
        assert!(clean_stop("", panic, Some(0))[0].contains("a panic in stderr"));
        let abort = "2026-10-31T00:00:02Z *** System error while flushing: x";
        assert!(clean_stop(&log(&[abort]), "", Some(0))[0].contains("an abort"));
    }

    #[test]
    fn cases_parse_from_their_names() {
        for c in Case::ALL {
            assert_eq!(Case::parse(c.name()), Some(c));
        }
        assert_eq!(Case::parse("FL-X"), None);
        assert!(Case::FlWal.flakey() && !Case::DfMid.flakey());
        assert!([Case::FlFlush, Case::DfMid].iter().all(|c| c.on_flush_line()));
        assert!([Case::FlIbd, Case::FlWal, Case::DfPre].iter().all(|c| !c.on_flush_line()));
        assert!(is_flush_line("t Writing partial batch of 1.00 MiB"));
        assert!(!is_flush_line("t UpdateTip: new best"));
    }

    #[test]
    fn the_wal_fault_table_is_down_on_the_extents_alone() {
        let under = "/dev/mapper/st3b-1-lw";
        let down = |a: u64, n: u64| format!("{a} {n} flakey {under} {a} 0 1 1 error_writes");
        let up = |a: u64, n: u64| format!("{a} {n} linear {under} {a}");
        // Two extents, given out of order, the second touching a third: merged, linear around.
        let extents = [(8192 << 9, 4096), (4096, 4096), (8200 << 9, 8192)];
        assert_eq!(
            wal_fault_table(under, 1 << 20, &extents),
            Ok(vec![up(0, 8), down(8, 8), up(16, 8176), down(8192, 24), up(8216, 1_040_360)])
        );
        // At the device's ends: no empty linear segment.
        assert_eq!(
            wal_fault_table(under, 64, &[(0, 4096), (56 << 9, 4096)]),
            Ok(vec![down(0, 8), up(8, 48), down(56, 8)])
        );
        // Overlapping extents merge.
        assert_eq!(
            wal_fault_table(under, 64, &[(0, 8192), (4096, 8192)]),
            Ok(vec![down(0, 24), up(24, 40)])
        );
        assert!(wal_fault_table(under, 64, &[]).is_err());
        assert!(wal_fault_table(under, 64, &[(4096, 100)]).is_err());
        assert!(wal_fault_table(under, 64, &[(4096, 0)]).is_err());
        assert!(wal_fault_table(under, 64, &[(60 << 9, 4096)]).is_err());
    }

    #[test]
    fn a_count_past_the_filesystem_is_none_available() {
        assert_eq!(available_bytes(10, 100, 4096), 40960);
        assert_eq!(available_bytes(u64::MAX - 5, 100, 4096), 0);
        assert_eq!(available_bytes(100, 100, u64::MAX), u64::MAX);
    }

    #[test]
    fn the_filler_leaves_what_is_asked() {
        assert_eq!(filler_len(100 << 20, DF_PRE_LEAVE), Some(60 << 20));
        assert_eq!(filler_len(DF_PRE_LEAVE, DF_PRE_LEAVE), None);
        assert_eq!(filler_len(1, DF_PRE_LEAVE), None);
    }

    #[test]
    fn the_fault_table_is_linear_or_always_down() {
        assert_eq!(
            fault_table("/dev/mapper/st3b-1-lw", 8, false),
            "0 8 linear /dev/mapper/st3b-1-lw 0"
        );
        assert_eq!(
            fault_table("/dev/mapper/st3b-1-lw", 8, true),
            "0 8 flakey /dev/mapper/st3b-1-lw 0 0 1 1 error_writes"
        );
    }

    #[test]
    fn a_seed_draws_an_acknowledgement_inside_the_schedule() {
        for seed in 0..200 {
            let (k, delay) = draw(Case::FlIbd, seed, 6).unwrap();
            assert!((1..=4).contains(&k) && delay <= 2_000_000, "{k} {delay}");
            assert_eq!(draw(Case::FlFlush, seed, 6).unwrap().1, 0, "frozen at the line");
            assert_eq!(draw(Case::FlWal, seed, 6).unwrap().1, 0);
        }
        assert_eq!(draw(Case::FlIbd, 1, 3).unwrap().0, 1);
        assert_eq!(draw(Case::FlIbd, 1, 2), None);
    }

    #[test]
    fn the_power_loss_state_keeps_the_last_flush_and_later_fua_writes() {
        use crate::crash::logwrites::{Entry, MARK};
        let entry = |flags, nr_sectors| Entry {
            sector: 0,
            nr_sectors,
            flags,
            data_offset: 0,
            mark: (flags == MARK).then(|| "m".to_owned()),
        };
        // write, FLUSH, a0, write, FUA write, write, FLUSH+FUA write, write, FUA write, exit.
        let log = Log {
            sector_size: 512,
            entries: vec![
                entry(0, 8),
                entry(FLUSH, 0),
                entry(MARK, 0),
                entry(0, 8),
                entry(FUA, 8),
                entry(0, 8),
                entry(FLUSH | FUA, 8),
                entry(0, 8),
                entry(FUA, 8),
                entry(MARK, 0),
            ],
        };
        let pt = power_loss_point(&log, 9);
        assert_eq!((pt.prefix, pt.extra), (6, vec![6, 8]));
        // No FLUSH after the start: from the first entry.
        assert_eq!(power_loss_point(&log, 1).prefix, 0);
    }

    #[test]
    fn the_floor_is_the_last_successful_reply() {
        assert_eq!(fault_floor(&[]), Floor::Start);
        assert_eq!(fault_floor(&[3, 9, 12]), Floor::Ack(12));
    }
}
