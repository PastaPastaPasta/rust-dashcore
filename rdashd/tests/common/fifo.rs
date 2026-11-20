//! A reindex's pause between two blk files, on both nodes: a FIFO `blkNNNNN.dat` passes the
//! scan's exists check, and its open blocks until a writer opens it. A release gives EOF, an
//! empty file. Only between files: a FIFO fed part-way through a file would stop the nodes'
//! reads at different records.

use std::ffi::CString;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::node::Node;
use super::{Running, net};

/// Every wait of these tests: one that ends later fails the test.
pub const BOUND: Duration = Duration::from_secs(60);

/// `node`'s `blkNNNNN.dat`.
pub fn blk_path(node: &Node, n: u32) -> PathBuf {
    net(&node.datadir).join("blocks").join(format!("blk{n:05}.dat"))
}

/// A FIFO at a node's `blkNNNNN.dat`, and the nodes that may block opening it. Dropped, it
/// releases the FIFO (a short wait, its failure ignored), SIGKILLs every node it watches that is
/// still running, then unlinks the path: an unlink alone releases no reader blocked in `open`.
pub struct FifoGuard {
    path: PathBuf,
    nodes: Vec<OwnedFd>,
    unlinked: bool,
}

/// `mkfifo` at `node`'s `blk{n:05}.dat`.
pub fn make_fifo(node: &Node, n: u32) -> FifoGuard {
    fifo_at(blk_path(node, n))
}

/// `mkfifo` at `path`.
pub fn fifo_at(path: PathBuf) -> FifoGuard {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path.
    let made = unsafe { libc::mkfifo(c.as_ptr(), 0o600) };
    assert_eq!(made, 0, "mkfifo {}: {}", path.display(), std::io::Error::last_os_error());
    FifoGuard {
        path,
        nodes: Vec::new(),
        unlinked: false,
    }
}

impl FifoGuard {
    /// Holds `node`'s process (a pidfd, so a reused pid is never signalled) for the drop's kill.
    pub fn watch(&mut self, node: &Node) {
        let (running, _) = node.run.as_ref().expect("a running node");
        self.watch_process(running);
    }

    /// [`watch`](Self::watch) for a node's process.
    pub fn watch_process(&mut self, running: &Running) {
        // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1; the
        // descriptor is close-on-exec, as pidfd_open always makes it.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, running.id(), 0) };
        assert!(fd >= 0, "pidfd_open: {}", std::io::Error::last_os_error());
        // SAFETY: the descriptor was just made and is owned here alone.
        self.nodes.push(unsafe { OwnedFd::from_raw_fd(i32::try_from(fd).unwrap()) });
    }

    /// Opens the FIFO for writing without blocking, retrying while no reader has it open
    /// (`ENXIO`) every 10 ms until `deadline`, then closes it: the reader blocked in `open` goes
    /// on and reads EOF.
    pub fn try_release(&self, deadline: Duration) -> Result<(), String> {
        let c = CString::new(self.path.as_os_str().as_bytes()).unwrap();
        let end = Instant::now() + deadline;
        loop {
            // SAFETY: a NUL-terminated path; the descriptor is closed below.
            let fd = unsafe {
                libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC)
            };
            if fd >= 0 {
                // SAFETY: the descriptor this call opened.
                unsafe { libc::close(fd) };
                return Ok(());
            }
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::ENXIO) {
                return Err(format!("{}: {e}", self.path.display()));
            }
            if Instant::now() >= end {
                return Err(format!("{}: no reader within {deadline:?}", self.path.display()));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Unlinks the FIFO: after a kill, before any restart, or after "Reindexing finished",
    /// before any block write.
    pub fn unlink(&mut self) {
        std::fs::remove_file(&self.path).unwrap();
        self.unlinked = true;
    }
}

/// Releases `guard`'s FIFO within [`BOUND`]; a missed deadline fails the test.
pub fn release_fifo(guard: &FifoGuard) {
    guard.try_release(BOUND).unwrap();
}

impl Drop for FifoGuard {
    fn drop(&mut self) {
        if self.unlinked {
            return;
        }
        let _ = self.try_release(Duration::from_secs(2));
        for fd in &self.nodes {
            // SAFETY: a pidfd this guard owns; a process that has exited is not signalled.
            unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                );
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// `what`, waited for within [`BOUND`].
pub fn within(what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + BOUND;
    while !done() {
        assert!(Instant::now() < end, "{what} within {BOUND:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The lines of `log` without their time.
pub fn texts(log: &str) -> Vec<&str> {
    log.lines().map(|l| l.split_once(' ').map_or(l, |(_, text)| text)).collect()
}

/// A line of [`texts`] without its log category (`[reindex] `, `[error] `), if it has one.
pub fn body(line: &str) -> &str {
    line.strip_prefix('[').and_then(|l| l.split_once("] ")).map_or(line, |(_, rest)| rest)
}

/// `file`'s "Reindexing block file" line.
pub fn reindexing_line(file: u32) -> String {
    format!("Reindexing block file blk{file:05}.dat...")
}

/// Whether `line` is a file's "Loaded … blocks from external file in …ms" line.
pub fn is_loaded(line: &str) -> bool {
    line.starts_with("Loaded ") && line.contains(" blocks from external file in ")
}

/// `line`, a "Loaded" line, without its duration.
pub fn loaded_count(line: &str) -> &str {
    line.split(" in ").next().unwrap()
}

/// The pause barrier at `paused`: `node`'s scan has logged the "Loaded" line of the file before
/// it, and not `paused`'s own "Reindexing block file" line, which comes only once its open
/// returns.
pub fn wait_paused(node: &Node, paused: u32) {
    within(&format!("{} paused at blk{paused:05}.dat", node.name), || {
        let log = node.new_log();
        let lines = texts(&log);
        let before = reindexing_line(paused - 1);
        lines
            .iter()
            .position(|l| *l == before)
            .is_some_and(|at| lines[at..].iter().any(|l| is_loaded(l)))
    });
    let log = node.new_log();
    let paused_line = reindexing_line(paused);
    assert!(!texts(&log).contains(&paused_line.as_str()), "{}: {log}", node.name);
}

/// The records (`magic ‖ size ‖ block`) of `node`'s `blk{file:05}.dat`, in file order, up to its
/// zero-filled tail.
pub fn blk_records(node: &Node, file: u32) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(blk_path(node, file)).unwrap();
    let mut records = Vec::new();
    let mut at = 0;
    while at + 8 <= bytes.len() && bytes[at..at + 4] != [0; 4] {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        records.push(bytes[at..at + 8 + size].to_vec());
        at += 8 + size;
    }
    records
}

/// Writes `records` as `node`'s `blk{file:05}.dat`, replacing it.
pub fn write_blk(node: &Node, file: u32, records: &[Vec<u8>]) {
    std::fs::write(blk_path(node, file), records.concat()).unwrap();
}
