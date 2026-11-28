//! The fault observed (S6 plan §3b): rdashd's storage calls from `strace -f -ttt -y`, each with
//! its thread, its path, its result and the time it **completed**, and the run's first fatal
//! failure among them.
//!
//! An `<unfinished ...>` call completes on its thread's `<... resumed>` line, so its time is that
//! line's, never the entry stamp. Only failures with a fault's errno (EIO, ENOSPC, EDQUOT, EROFS)
//! on the faulted mount count (so never a sync's EINVAL, which Core's `FileCommit` tolerates), and
//! not the ones rdashd's contract tolerates: an advisory `fallocate` (`OsFs::allocate` falls back
//! to writes), a deletion (RocksDB logs a failed obsolete-file deletion and goes on), a directory's
//! sync (flat-file directory syncs are warnings, as Core's `DirectoryCommit`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::faults::{Case, EXACT};

/// The calls traced: storage writes, syncs, creates and metadata changes (x86_64 glibc's
/// `rename`, `unlink` and the like call the plain syscalls, not their `*at` forms: Opus r1 m3).
pub const TRACED: &str = "openat,openat2,creat,mkdir,mkdirat,unlinkat,renameat,renameat2,write,\
                          pwrite64,writev,pwritev,pwritev2,fsync,fdatasync,sync_file_range,\
                          fallocate,ftruncate,rename,link,linkat,truncate,unlink,rmdir";

/// Microseconds since the Unix epoch.
pub type Micros = i64;

/// A call's result: its return value, or `-1` and the errno strace names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ret {
    Ok(i64),
    Err(String),
}

/// One completed call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub tid: u32,
    /// When it returned.
    pub done: Micros,
    pub name: String,
    /// The file it acted on (for a rename, the new name).
    pub path: Option<PathBuf>,
    pub ret: Ret,
}

impl Call {
    pub fn errno(&self) -> Option<&str> {
        match &self.ret {
            Ret::Err(e) => Some(e),
            Ret::Ok(_) => None,
        }
    }

    pub fn is_write(&self) -> bool {
        matches!(self.name.as_str(), "write" | "pwrite64" | "writev" | "pwritev" | "pwritev2")
    }

    /// A call that creates a file or directory when it fails for space: an open fails with
    /// ENOSPC only when it creates.
    pub fn creates(&self) -> bool {
        matches!(self.name.as_str(), "openat" | "openat2" | "creat" | "mkdir" | "mkdirat")
    }
}

/// The errnos a device fault or a full disk gives, by strace's name and number.
pub const FAULT_ERRNOS: [(&str, i32); 4] = [
    ("EIO", libc::EIO),
    ("ENOSPC", libc::ENOSPC),
    ("EDQUOT", libc::EDQUOT),
    ("EROFS", libc::EROFS),
];

pub fn errno_number(name: &str) -> Option<i32> {
    FAULT_ERRNOS.iter().find(|(n, _)| *n == name).map(|&(_, e)| e)
}

/// `secs` and a decimal fraction of a second (padded or cut to 6 digits) as microseconds.
fn micros(secs: i64, frac: &str) -> Option<Micros> {
    Some(secs * 1_000_000 + format!("{frac:0<6}").get(..6)?.parse::<i64>().ok()?)
}

/// `"12.345678"` (strace `-ttt`) as microseconds.
fn parse_ttt(s: &str) -> Option<Micros> {
    let (secs, frac) = s.split_once('.')?;
    micros(secs.parse().ok()?, frac)
}

/// The days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 {
        y - 1
    } else {
        y
    };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A debug.log line's `-logtimemicros` stamp (`2026-10-31T03:52:01.123456Z`) as microseconds.
pub fn log_time(line: &str) -> Option<Micros> {
    let stamp = line.split_whitespace().next()?.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut d = date.splitn(3, '-').map(str::parse::<i64>);
    let (y, mo, da) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let (hms, frac) = time.split_once('.')?;
    let mut t = hms.splitn(3, ':').map(str::parse::<i64>);
    let (h, mi, s) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let secs = days_from_civil(y, mo, da) * 86_400 + h * 3600 + mi * 60 + s;
    micros(secs, frac)
}

/// The quoted string starting at `s` (after its opening quote): its text and the rest.
fn quoted(s: &str) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => out.push(chars.next()?.1),
            c => out.push(c),
        }
    }
    None
}

/// A `-y` descriptor `N</path>` or `AT_FDCWD</path>` at the start of `s`: its path.
fn fd_path(s: &str) -> Option<PathBuf> {
    let open = s.find('<')?;
    let close = s[open..].find('>')? + open;
    (!s[..open].contains([',', ')'])).then(|| PathBuf::from(&s[open + 1..close]))
}

/// `path` as the call named it: relative to the directory descriptor `dir`, if it gave one.
fn under(dir: Option<PathBuf>, path: String) -> PathBuf {
    match dir {
        Some(d) => d.join(path),
        None => PathBuf::from(path),
    }
}

/// The path a call acted on, from its argument text.
fn call_path(name: &str, args: &str) -> Option<PathBuf> {
    let first_quoted = |s: &str| quoted(s.split_once('"')?.1).map(|(p, _)| p);
    match name {
        "creat" | "mkdir" | "unlink" | "rmdir" | "truncate" => {
            first_quoted(args).map(PathBuf::from)
        }
        "openat" | "openat2" | "mkdirat" | "unlinkat" => {
            Some(under(fd_path(args), first_quoted(args)?))
        }
        // ("old", "new"): the new name.
        "rename" | "link" => first_quoted(quoted(args.split_once('"')?.1)?.1).map(PathBuf::from),
        "renameat" | "renameat2" | "linkat" => {
            // (olddirfd, "old", newdirfd, "new"): the new name.
            let after_old = quoted(args.split_once('"')?.1)?.1;
            let new_dir = fd_path(after_old.trim_start_matches([',', ' ']));
            Some(under(new_dir, first_quoted(after_old)?))
        }
        _ => fd_path(args),
    }
}

/// A result text after `= `: `0`, `-1 EIO (Input/output error)`, `? <unavailable>`.
fn parse_ret(s: &str) -> Ret {
    let s = s.trim();
    match s.strip_prefix("-1 ") {
        Some(rest) => Ret::Err(rest.split_whitespace().next().unwrap_or("").to_owned()),
        None => Ret::Ok(s.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0)),
    }
}

/// The calls of an strace `-f -ttt -T -y` output, in completion order; lines it can't read
/// (signals, exits, `+++`) and calls that never returned (`= ?`) are skipped. A call's completion
/// time is its entry stamp (`-ttt` stamps a line, or an `<unfinished ...>` part, at entry) plus its
/// `-T` duration: a call with a result and no duration is an error, not a guess.
pub fn parse(text: &str) -> Result<Vec<Call>, String> {
    let mut pending = HashMap::new();
    let mut calls = Vec::new();
    for line in text.lines() {
        calls.extend(parse_line(line, &mut pending)?);
    }
    calls.sort_by_key(|c| c.done);
    Ok(calls)
}

/// A result text's `-T` duration (` <0.000123>`, last), split off: the result and the duration.
fn split_duration(ret: &str) -> Option<(&str, Micros)> {
    let (ret, d) = ret.trim_end().strip_suffix('>')?.rsplit_once(" <")?;
    Some((ret, parse_ttt(d)?))
}

/// The call `line` completes, if any. An `<unfinished ...>` entry is held in `pending` (by thread:
/// its name, argument text and entry stamp) until the thread's `<... resumed>` line completes it.
fn parse_line(
    line: &str,
    pending: &mut HashMap<u32, (String, String, Micros)>,
) -> Result<Option<Call>, String> {
    let mut words = line.trim_start().splitn(3, ' ');
    let (Some(tid), Some(time), Some(rest)) = (words.next(), words.next(), words.next()) else {
        return Ok(None);
    };
    let (Some(tid), Some(stamp)) = (tid.parse::<u32>().ok(), parse_ttt(time)) else {
        return Ok(None);
    };
    let rest = rest.trim_start();
    let (name, args, entry, ret) = if let Some(resumed) = rest.strip_prefix("<... ") {
        let Some((name, tail)) = resumed.split_once(" resumed>") else {
            return Ok(None);
        };
        let Some((entry_name, args, entry)) = pending.remove(&tid) else {
            return Ok(None);
        };
        let Some((more, ret)) = tail.rsplit_once(" = ").filter(|_| entry_name == name) else {
            return Ok(None);
        };
        (entry_name, format!("{args}{more}"), entry, ret)
    } else {
        let Some((name, args)) = rest.split_once('(') else {
            return Ok(None);
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Ok(None);
        }
        if let Some(args) = args.strip_suffix(" <unfinished ...>") {
            pending.insert(tid, (name.to_owned(), args.to_owned(), stamp));
            return Ok(None);
        }
        let Some((args, ret)) = args.rsplit_once(" = ") else {
            return Ok(None);
        };
        (name.to_owned(), args.to_owned(), stamp, ret)
    };
    if ret.trim_start().starts_with('?') {
        return Ok(None);
    }
    let (ret, took) = split_duration(ret).ok_or_else(|| format!("no -T duration: {line}"))?;
    Ok(Some(Call {
        tid,
        done: entry + took,
        path: call_path(&name, &args),
        name,
        ret: parse_ret(ret),
    }))
}

/// How the trace `text` saw thread `tid` end, from strace's `+++` line: `exited with N` or
/// `killed by SIG…`. None: the trace stopped before it ended (Opus r1 m2).
pub fn tracee_end(text: &str, tid: u32) -> Option<&str> {
    text.lines().rev().find_map(|l| {
        let mut words = l.trim_start().splitn(3, ' ');
        (words.next()?.parse::<u32>().ok()? == tid).then_some(())?;
        words.next()?;
        words.next()?.strip_prefix("+++ ")?.strip_suffix(" +++")
    })
}

/// What makes a failed call the fault's: completing after `injected`, under `mount`.
#[derive(Clone, Debug)]
pub struct Scope<'a> {
    pub mount: &'a Path,
    pub injected: Micros,
}

/// Whether `call` is a fatal relevant failure (S6 plan §3b).
pub fn is_fatal(call: &Call, scope: &Scope<'_>) -> bool {
    let (Some(errno), Some(path)) = (call.errno(), call.path.as_deref()) else {
        return false;
    };
    errno_number(errno).is_some()
        && call.done >= scope.injected
        && path.starts_with(scope.mount)
        && !advisory(&call.name)
        && fatal_store(path)
        && !info_log(path)
}

/// A call whose failure rdashd's contract tolerates whatever its path: `fallocate` (`OsFs::allocate`
/// falls back to writes) and a deletion (RocksDB logs a failed obsolete-file deletion and goes on,
/// `DeleteObsoleteFileImpl`).
fn advisory(name: &str) -> bool {
    matches!(name, "fallocate" | "unlink" | "unlinkat" | "rmdir")
}

/// Whether `path` belongs to a store whose failures are fatal to rdashd (S6 plan §3b, rev7): a
/// file in `blocks/` (the block and undo files) or in `chainstate/` (RocksDB), or the
/// `chainstate` directory itself (RocksDB's directory syncs are fatal to it: a failed one aborted
/// FL-WAL xfs with no other failure before it, OD-ST3b-11). Not the `blocks` directory (its sync
/// failure is a warning, `seq.rs`, Core's `DirectoryCommit`), nor the chain directory's flat
/// caches (`peers.dat`, `mncache.dat`, `netfulfilled.dat`, `sporks.dat`, their temporaries) and
/// that directory's syncs, whose failures rdashd logs and goes on from, as Core does (Sol r1 M4).
fn fatal_store(path: &Path) -> bool {
    let parent = path.parent().and_then(Path::file_name);
    path.file_name().is_some_and(|n| n == "chainstate")
        || parent.is_some_and(|d| d == "blocks" || d == "chainstate")
}

/// RocksDB's info log (`LOG`, `LOG.old.*`): advisory, its write failures ignored (OD-ST3b-9).
fn info_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "LOG" || n.starts_with("LOG.old"))
}

/// The run's fatal relevant failures, in completion order; the first is the run's first fatal
/// failure.
pub fn fatal<'c>(calls: &'c [Call], scope: &Scope<'_>) -> Vec<&'c Call> {
    calls.iter().filter(|c| is_fatal(c, scope)).collect()
}

/// The line that ends a coins flush (`flush/machine.rs`, after its last durable write).
const COMMITTED: (&str, &str) = ("Committed ", " to coin database");

/// The log's causal abort (S6 plan §3.1): the first `*** ` line's time and text.
pub fn first_abort(log: &str) -> Option<(Micros, &str)> {
    log.lines().find_map(|l| Some((log_time(l)?, l.split_once("*** ")?.1)))
}

/// The targeted flush's interval (S6 plan §3b), from the first flush line at or after `from` to
/// that flush's `Committed … to coin database` line or the causal abort, whichever comes first
/// (open-ended if the log has neither).
pub fn flush_interval(log: &str, from: Micros) -> Option<(Micros, Micros)> {
    let mut lines = log.lines().filter_map(|l| Some((log_time(l)?, l)));
    let (start, _) = lines.find(|&(t, l)| t >= from && super::faults::is_flush_line(l))?;
    let end = lines
        .find(|(_, l)| l.contains("*** ") || (l.contains(COMMITTED.0) && l.contains(COMMITTED.1)))
        .map_or(Micros::MAX, |(t, _)| t);
    Some((start, end))
}

/// Whether `path` is a chainstate file (`<datadir>/<chain>/chainstate/…`).
fn chainstate(path: &Path) -> bool {
    path.parent().and_then(Path::file_name).is_some_and(|d| d == "chainstate")
}

/// Whether a run met its case's condition (S6 plan §3b coverage), from its fatal failures
/// ([`fatal`]), its causal abort ([`first_abort`]) and, for a flush-line case, the targeted
/// flush's interval ([`flush_interval`]). `Err` names the miss; a miss is retried with the next
/// seed, a behaviour failure never is.
pub fn coverage(
    case: Case,
    fatal: &[&Call],
    abort: Option<(Micros, &str)>,
    interval: Option<(Micros, Micros)>,
) -> Result<(), String> {
    let before_abort = |c: &Call| abort.is_none_or(|(t, _)| c.done <= t);
    let inside = |c: &Call| interval.is_some_and(|(a, b)| (a..=b).contains(&c.done));
    let first = fatal.first().copied();
    let miss = |what: &str| Err(format!("{case} missed: {what}"));
    match case {
        Case::FlIbd => match first {
            Some(c) if c.errno() == Some("EIO") => Ok(()),
            Some(c) => miss(&format!("the first failure is {c:?}, not EIO")),
            None => miss("no fatal failure"),
        },
        Case::FlFlush => match first {
            _ if interval.is_none() => miss("no flush line after the trigger"),
            Some(c)
                if c.errno() == Some("EIO")
                    && inside(c)
                    && c.path.as_deref().is_some_and(chainstate) =>
            {
                Ok(())
            }
            Some(c) => miss(&format!(
                "the first failure {c:?} is not an EIO on chainstate in {interval:?}"
            )),
            None => miss("no fatal failure"),
        },
        Case::FlWal => {
            let on_wal = |c: &Call| {
                c.path
                    .as_deref()
                    .is_some_and(|p| chainstate(p) && p.extension().is_some_and(|e| e == "log"))
            };
            let mut wal = fatal.iter().copied().filter(|c| before_abort(c) && on_wal(c)).peekable();
            if wal.clone().any(|c| matches!(c.name.as_str(), "fsync" | "fdatasync")) {
                Ok(())
            } else if wal.peek().is_some() {
                miss("append only (no failed WAL sync)")
            } else {
                miss("no WAL failure")
            }
        }
        Case::DfPre => match abort {
            Some((t, msg)) if msg == EXACT[0] && fatal.iter().all(|c| c.done > t) => Ok(()),
            Some((_, msg)) if msg == EXACT[0] => {
                miss("a fatal failure precedes the low-space abort")
            }
            _ => miss("the abort is not the low-space refusal"),
        },
        Case::DfMid => match (first, abort) {
            (_, Some((_, msg))) if msg == EXACT[0] => {
                miss("a low-space refusal (DF-PRE's condition)")
            }
            // A flush that switches memtables creates its next WAL file (OD-ST3b-9).
            (Some(c), _)
                if c.errno() == Some("ENOSPC")
                    && (c.is_write() || c.creates())
                    && inside(c)
                    && before_abort(c) =>
            {
                Ok(())
            }
            (Some(c), _) => miss(&format!(
                "the first failure {c:?} is not a write's or a create's ENOSPC in {interval:?}"
            )),
            (None, _) => miss("no fatal failure"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: &str = "/m/r";

    fn scope() -> Scope<'static> {
        Scope {
            mount: Path::new(M),
            injected: 10_000_000,
        }
    }

    /// [`super::parse`] of `text` with a zero `-T` duration on each returned call that has none:
    /// completion at entry.
    fn parse(text: &str) -> Vec<Call> {
        let timed: String = text
            .lines()
            .map(|l| {
                let returned = l.contains(" = ") && !l.contains(" = ?");
                if returned && split_duration(l.rsplit_once(" = ").unwrap().1).is_none() {
                    format!("{l} <0.000000>\n")
                } else {
                    format!("{l}\n")
                }
            })
            .collect();
        super::parse(&timed).unwrap()
    }

    #[test]
    fn a_call_completes_at_its_entry_plus_its_duration() {
        // An unsplit sync that entered before injection (10.0) and returned EIO after it.
        let text = "1 9.900000 fdatasync(5</m/r/rdashd/regtest/chainstate/000004.log>) = -1 EIO (Input/output error) <0.200000>\n";
        let calls = super::parse(text).unwrap();
        assert_eq!(calls[0].done, 10_100_000);
        assert_eq!(fatal(&calls, &scope()).len(), 1);
        // A split one: the entry stamp plus the duration, whenever the resumed line was printed.
        let text = "\
2 9.800000 fdatasync(5</m/r/rdashd/regtest/blocks/blk00000.dat> <unfinished ...>
3 9.900000 write(6</m/r/x>, \"\"..., 1) = 1 <0.000010>
2 10.400000 <... fdatasync resumed>) = -1 EIO (Input/output error) <0.300000>
";
        let calls = super::parse(text).unwrap();
        assert_eq!((calls[1].name.as_str(), calls[1].done), ("fdatasync", 10_100_000));
        // No duration on a returned call: refused. A call that never returned: skipped.
        let bare = "1 9.9 fdatasync(5</m/r/a.log>) = -1 EIO (Input/output error)\n";
        assert!(super::parse(bare).unwrap_err().contains("no -T duration"));
        assert!(super::parse("1 9.9 exit_group(1) = ?\n").unwrap().is_empty());
    }

    #[test]
    fn calls_carry_their_thread_path_result_and_completion_time() {
        let text = "\
101 10.000001 openat(AT_FDCWD</m/r/rdashd>, \"regtest/blocks/blk00000.dat\", O_RDWR|O_CREAT, 0666) = 7</m/r/rdashd/regtest/blocks/blk00000.dat>
102 10.000002 fdatasync(9</m/r/rdashd/regtest/chainstate/000004.log> <unfinished ...>
101 10.000003 pwrite64(7</m/r/rdashd/regtest/blocks/blk00000.dat>, \"\"..., 4096, 0) = -1 EIO (Input/output error)
102 10.500000 <... fdatasync resumed>) = -1 EIO (Input/output error) <0.499998>
101 10.6 renameat2(AT_FDCWD</m/r/rdashd>, \"settings.json.tmp\", AT_FDCWD</m/r/rdashd>, \"settings.json\", 0) = 0
101 10.7 +++ exited with 1 +++
";
        let calls = parse(text);
        let names: Vec<_> = calls.iter().map(|c| (c.tid, c.name.as_str(), c.done)).collect();
        assert_eq!(
            names,
            [
                (101, "openat", 10_000_001),
                (101, "pwrite64", 10_000_003),
                (102, "fdatasync", 10_500_000),
                (101, "renameat2", 10_600_000)
            ]
        );
        assert_eq!(
            calls[0].path.as_deref(),
            Some(Path::new("/m/r/rdashd/regtest/blocks/blk00000.dat"))
        );
        assert_eq!(calls[1].errno(), Some("EIO"));
        assert_eq!(
            calls[2].path.as_deref(),
            Some(Path::new("/m/r/rdashd/regtest/chainstate/000004.log"))
        );
        assert_eq!(calls[3].path.as_deref(), Some(Path::new("/m/r/rdashd/settings.json")));
    }

    #[test]
    fn an_unfinished_call_completes_after_a_later_entry() {
        // The sync entered first but returned last: the write's failure is the first.
        let text = "\
7 10.1 fdatasync(4</m/r/rdashd/regtest/chainstate/000004.log> <unfinished ...>
8 10.2 write(5</m/r/rdashd/regtest/blocks/blk00000.dat>, \"\"..., 10) = -1 EIO (Input/output error)
7 10.3 <... fdatasync resumed>) = -1 EIO (Input/output error) <0.200000>
";
        let calls = parse(text);
        let first = fatal(&calls, &scope())[0];
        assert_eq!((first.name.as_str(), first.done), ("write", 10_200_000));
    }

    #[test]
    fn tolerated_and_unrelated_failures_are_not_fatal() {
        let text = "\
1 9.9 write(5</m/r/rdashd/regtest/blocks/blk00000.dat>, \"\"..., 1) = -1 EIO (Input/output error)
1 10.1 fallocate(5</m/r/rdashd/regtest/blocks/blk00000.dat>, 0, 0, 16777216) = -1 EIO (Input/output error)
1 10.2 fsync(6</m/r/rdashd/regtest/blocks>) = -1 EIO (Input/output error)
1 10.3 fdatasync(5</m/r/rdashd/regtest/blocks/blk00000.dat>) = -1 EINVAL (Invalid argument)
1 10.4 openat(AT_FDCWD</m/r>, \"rdashd/x\", O_RDONLY) = -1 ENOENT (No such file or directory)
1 10.5 write(3</work/logs/debug.log>, \"\"..., 9) = -1 EIO (Input/output error)
1 10.6 mkdir(\"/m/r/rdashd/new/blocks\", 0700) = 0
1 10.7 fsync(8</m/r/rdashd/new/blocks>) = -1 EIO (Input/output error)
1 10.8 openat(AT_FDCWD</m/r/rdashd>, \"chainstate/000009.log\", O_RDWR|O_CREAT, 0644) = -1 EIO (Input/output error)
";
        let calls = parse(text);
        let f = fatal(&calls, &scope());
        // Before injection, advisory, the blocks directory, EINVAL, ENOENT, off the mount, another
        // blocks directory: none; the failed create is the first.
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].name, "openat");
        assert_eq!(f[0].path.as_deref(), Some(Path::new("/m/r/rdashd/chainstate/000009.log")));
        // RocksDB's directory sync is fatal to it.
        let text =
            "1 10.2 fsync(6</m/r/rdashd/regtest/chainstate>) = -1 EIO (Input/output error)\n";
        assert_eq!(fatal(&parse(text), &scope()).len(), 1);
    }

    #[test]
    fn a_trace_ends_with_its_tracees_own_exit() {
        let text = "\
7 10.1 fdatasync(5</m/r/x>) = 0 <0.000010>
8 10.2 +++ exited with 1 +++
7 10.3 +++ killed by SIGKILL +++
";
        assert_eq!(tracee_end(text, 8), Some("exited with 1"));
        assert_eq!(tracee_end(text, 7), Some("killed by SIGKILL"));
        assert_eq!(tracee_end(text, 9), None);
        assert_eq!(tracee_end("7 10.1 fdatasync(5</m/r/x>) = 0 <0.000010>\n", 7), None);
    }

    #[test]
    fn plain_metadata_calls_are_traced_and_deletions_are_advisory() {
        let text = "\
1 10.1 rename(\"/m/r/rdashd/regtest/chainstate/000004.dbtmp\", \"/m/r/rdashd/regtest/chainstate/CURRENT\") = -1 EIO (Input/output error)
1 10.2 unlink(\"/m/r/rdashd/regtest/chainstate/000003.log\") = -1 EIO (Input/output error)
1 10.3 rmdir(\"/m/r/rdashd/regtest/chainstate/x\") = -1 EIO (Input/output error)
1 10.4 unlinkat(AT_FDCWD</m/r>, \"rdashd/regtest/chainstate/000005.sst\", 0) = -1 EIO (Input/output error)
1 10.5 truncate(\"/m/r/rdashd/regtest/blocks/rev00000.dat\", 0) = -1 EIO (Input/output error)
1 10.6 link(\"/m/r/a\", \"/m/r/rdashd/regtest/chainstate/b\") = -1 ENOSPC (No space left on device)
1 10.7 linkat(AT_FDCWD</m/r/rdashd>, \"c\", AT_FDCWD</m/r/rdashd/regtest/chainstate>, \"d\", 0) = -1 EIO (Input/output error)
";
        let calls = parse(text);
        assert!(TRACED.split(',').all(|n| n.trim() == n));
        for name in ["rename", "unlink", "rmdir", "truncate", "link", "linkat"] {
            assert!(TRACED.split(',').any(|n| n == name), "{name} not traced");
        }
        let fatal: Vec<_> = fatal(&calls, &scope())
            .iter()
            .map(|c| (c.name.as_str(), c.path.clone().unwrap_or_default()))
            .collect();
        let at = |p: &str| PathBuf::from(format!("/m/r/rdashd/{p}"));
        assert_eq!(
            fatal,
            [
                ("rename", at("regtest/chainstate/CURRENT")),
                ("truncate", at("regtest/blocks/rev00000.dat")),
                ("link", at("regtest/chainstate/b")),
                ("linkat", at("regtest/chainstate/d")),
            ]
        );
        // The deletions carry their paths, and are advisory.
        assert_eq!(calls[1].path, Some(at("regtest/chainstate/000003.log")));
        assert_eq!(calls[3].path, Some(at("regtest/chainstate/000005.sst")));
    }

    #[test]
    fn the_flat_caches_and_their_directory_are_advisory() {
        // peers.dat's temporary, a flat cache's new file and the chain directory's sync warn (Sol
        // r1 M4); the chainstate write after them is the first fatal failure.
        let text = "\
1 10.1 openat(AT_FDCWD</m/r/rdashd>, \"regtest/peers.8a0b\", O_WRONLY|O_CREAT|O_EXCL, 0600) = -1 ENOSPC (No space left on device)
1 10.2 write(7</m/r/rdashd/regtest/mncache.dat.new>, \"\"..., 9) = -1 ENOSPC (No space left on device)
1 10.3 fsync(8</m/r/rdashd/regtest>) = -1 EIO (Input/output error)
1 10.4 pwrite64(9</m/r/rdashd/regtest/chainstate/000012.sst>, \"\"..., 9, 0) = -1 ENOSPC (No space left on device)
";
        let calls = parse(text);
        let f = fatal(&calls, &scope());
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].done, 10_400_000);
        // DF-MID: an advisory create inside the flush, the fatal write after it: not covered.
        let abort = (11_000_000, "System error while flushing: x: No space left on device");
        assert!(coverage(Case::DfMid, &f, Some(abort), Some((10_000_000, 10_300_000))).is_err());
    }

    #[test]
    fn log_stamps_and_trace_stamps_share_a_clock() {
        // 2026-10-31T03:52:01.123456Z
        let t = log_time("2026-10-31T03:52:01.123456Z UpdateTip: new best=ab").unwrap();
        assert_eq!(t, 1_793_418_721_123_456);
        assert_eq!(parse_ttt("1793418721.123456"), Some(t));
        assert_eq!(log_time("1970-01-01T00:00:00.5Z x"), Some(500_000));
        assert_eq!(log_time("no stamp"), None);
    }

    fn stamp(secs: &str) -> String {
        format!("2026-10-31T00:00:{secs}Z")
    }

    /// 2026-10-31T00:00:00Z.
    const T0: i64 = 1_793_404_800;

    /// Trace lines at `T0 + s` seconds (`s` as `"S.UUUUUU"`).
    fn trace(lines: &[(&str, &str)]) -> Vec<Call> {
        let at = |s: &str| {
            let (secs, micros) = s.split_once('.').unwrap();
            format!("{}.{micros}", T0 + secs.parse::<i64>().unwrap())
        };
        let text: String = lines.iter().map(|(t, l)| format!("1 {} {l}\n", at(t))).collect();
        parse(&text)
    }

    fn log(lines: &[(&str, &str)]) -> String {
        lines.iter().map(|(t, l)| format!("{} {l}\n", stamp(t))).collect()
    }

    const EIO: &str = "= -1 EIO (Input/output error)";

    #[test]
    fn a_final_batch_failure_is_inside_the_flush_and_a_later_flush_is_not() {
        let l = log(&[
            ("01.000000", "Writing partial batch of 1.00 MiB"),
            ("01.500000", "Writing final batch of 0.10 MiB"),
            ("02.000000", "Committed 9 changed transaction outputs (out of 9) to coin database..."),
            ("03.000000", "Writing final batch of 0.10 MiB"),
            ("04.000000", "*** System error while flushing: IO error: x: Input/output error"),
        ]);
        let from = log_time(&format!("{} x", stamp("00.500000"))).unwrap();
        let interval = flush_interval(&l, from);
        assert_eq!(interval, Some((T0 * 1_000_000 + 1_000_000, T0 * 1_000_000 + 2_000_000)));
        let sc = Scope {
            mount: Path::new(M),
            injected: 0,
        };
        let inside = trace(&[(
            "1.600000",
            &format!("pwrite64(5</m/r/rdashd/regtest/chainstate/000012.sst>, \"\"..., 9, 0) {EIO}"),
        )]);
        let f = fatal(&inside, &sc);
        assert_eq!(coverage(Case::FlFlush, &f, first_abort(&l), interval), Ok(()));
        // The next flush's ENOSPC, at 03.1, is outside the targeted flush.
        let later = trace(&[(
            "3.100000",
            "pwrite64(5</m/r/rdashd/regtest/chainstate/000013.sst>, \"\"..., 9, 0) = -1 ENOSPC (No space left on device)",
        )]);
        let f = fatal(&later, &sc);
        assert!(coverage(Case::DfMid, &f, first_abort(&l), interval).is_err());
    }

    #[test]
    fn a_wal_run_needs_a_failed_wal_sync() {
        let sc = Scope {
            mount: Path::new(M),
            injected: 0,
        };
        let append = trace(&[(
            "1.000000",
            &format!("write(5</m/r/rdashd/regtest/chainstate/000004.log>, \"\"..., 9) {EIO}"),
        )]);
        let f = fatal(&append, &sc);
        assert_eq!(
            coverage(Case::FlWal, &f, None, None),
            Err("FL-WAL missed: append only (no failed WAL sync)".into())
        );
        let synced = trace(&[
            (
                "1.000000",
                &format!("write(5</m/r/rdashd/regtest/chainstate/000004.log>, \"\"..., 9) {EIO}"),
            ),
            ("2.000000", &format!("fdatasync(5</m/r/rdashd/regtest/chainstate/000004.log>) {EIO}")),
        ]);
        let f = fatal(&synced, &sc);
        assert_eq!(coverage(Case::FlWal, &f, None, None), Ok(()));
        assert_eq!(coverage(Case::FlIbd, &f, None, None), Ok(()));
    }

    #[test]
    fn disk_full_cases_split_on_the_low_space_refusal() {
        let sc = Scope {
            mount: Path::new(M),
            injected: 0,
        };
        let refusal = log(&[("05.000000", "*** Disk space is too low!")]);
        assert_eq!(coverage(Case::DfPre, &[], first_abort(&refusal), None), Ok(()));
        assert!(coverage(Case::DfMid, &[], first_abort(&refusal), Some((0, i64::MAX))).is_err());
        let enospc = trace(&[(
            "1.000000",
            "pwrite64(5</m/r/rdashd/regtest/chainstate/000012.sst>, \"\"..., 9, 0) = -1 ENOSPC (No space left on device)",
        )]);
        let f = fatal(&enospc, &sc);
        assert!(
            coverage(Case::DfPre, &f, first_abort(&refusal), None).is_err(),
            "a failure before it"
        );
        let flushing = log(&[(
            "05.000000",
            "*** System error while flushing: IO error: No space left on device",
        )]);
        assert_eq!(coverage(Case::DfMid, &f, first_abort(&flushing), Some((0, i64::MAX))), Ok(()));
        assert!(coverage(Case::DfMid, &f, first_abort(&flushing), None).is_err(), "no flush line");
        let create = trace(&[(
            "1.000000",
            "openat(AT_FDCWD</m>, \"/m/r/rdashd/regtest/chainstate/000184.log\", O_WRONLY|O_CREAT|O_TRUNC|O_CLOEXEC, 0644) = -1 ENOSPC (No space left on device)",
        )]);
        let f = fatal(&create, &sc);
        assert_eq!(f.len(), 1, "a failed create is fatal");
        let info = trace(&[(
            "1.000000",
            "write(13</m/r/rdashd/regtest/chainstate/LOG>, \"\"..., 5624) = -1 ENOSPC (No space left on device)",
        )]);
        assert!(fatal(&info, &sc).is_empty(), "the info log is advisory");
        assert_eq!(coverage(Case::DfMid, &f, first_abort(&flushing), Some((0, i64::MAX))), Ok(()));
    }
}
