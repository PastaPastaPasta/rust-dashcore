//! The kill smoke (PLAN-ST3b §1 item 3, S8): a cheap SIGKILL loop on plain disk, with no devices
//! and no sudo, labelled smoke and run before the device pass.
//!
//! A lap is a fresh data directory and a fresh peer (the fixture's dashd: a peer that saw a lap's
//! submitted blocks would serve them to the next). It runs W-small's schedule with a kill planned
//! at a seeded time after each start. SIGKILL goes to rdashd, the harness's own unreaped child,
//! so its pid cannot have been reused. Each restart is a C1 start with no peers; then C2 at the
//! recovered tip, C3 at the floor (the last acknowledgement that returned before the kill), the
//! acknowledged tip's block still stored, and C4. The run then continues from there (the peer
//! added back while the node is still in the P2P part). Once the kills are made, the lap runs to
//! the fixture's tip, whose acknowledgement is C2 at the tip, and stops cleanly. Laps go on until
//! the kills are made.
//!
//! A process kill leaves the page cache standing, so this checks rdashd's own recovery from a
//! crash at any instruction, not the disk's ordering: that is the device and VM levels'.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dashcore_rpc::Client;
use tokio::runtime::Runtime;

use super::baseline::{Peer, UntilDeath, resume_until_death};
use super::checks::{Floor, RPC_PATIENCE, c1, c2, c3, c4, c4_read};
use super::devices::{Run, private_dir};
use super::fixture::Fixture;
use super::logwrites::Rng;
use super::vm::{active_chain, floor_after};
use crate::NodeRpc;

/// A kill's time after the workload starts: a W-small lap on the host runs about 10 s, so most
/// kills land in its IBD and some after the P2P part, in the submitted blocks.
pub const KILL_MS: std::ops::RangeInclusive<u64> = 100..=6_000;
/// How long a killed rdashd may take to be reaped.
const REAP_PATIENCE: Duration = Duration::from_secs(30);
/// Laps in a row without a kill end the run.
const IDLE_LAPS: usize = 3;

#[derive(Clone, Debug)]
pub struct Config {
    pub rdashd: PathBuf,
    /// The fixture's dashd, the peer.
    pub dashd: PathBuf,
    pub kills: usize,
    pub seed: u64,
    /// rdashd's arguments beyond the fixture's chain (`-dbcache=4 -dbbatchsize=20000`).
    pub extra: Vec<String>,
    /// An acknowledgement every this many submitted blocks.
    pub every: usize,
}

/// One kill.
#[derive(Clone, Debug)]
pub struct Kill {
    pub lap: usize,
    pub boot: usize,
    /// From the workload's start to the signal.
    pub after: Duration,
    /// The floor the next start's C3 holds the node to.
    pub floor: Floor,
    /// Where the next start's checks found the node (C3's position).
    pub recovered: Option<usize>,
    /// From the killed start to the end of the next start's checks.
    pub cycle: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub kills: Vec<Kill>,
    pub laps: usize,
    pub boots: usize,
    /// Laps that ran to the fixture's tip with C2 there and a clean stop.
    pub tips: usize,
    /// From the start to RPC, per start.
    pub start_times: Vec<Duration>,
    pub failures: Vec<String>,
}

/// SIGKILL to rdashd `pid` after `after`, unless cancelled first; the time `armed` was set.
struct Killer {
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Option<Instant>>,
}

impl Killer {
    fn spawn(pid: u32, after: Duration, armed: Arc<AtomicBool>) -> Killer {
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let handle = std::thread::spawn(move || {
            let start = Instant::now();
            while start.elapsed() < after {
                if stop.load(Ordering::SeqCst) {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            if stop.load(Ordering::SeqCst) {
                return None;
            }
            armed.store(true, Ordering::SeqCst);
            let at = Instant::now();
            // SAFETY: kill(2) on the harness's own child, which is not reaped before `finish`
            // joins this thread, so the pid is still that process (a zombie at worst).
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            Some(at)
        });
        Killer {
            cancel,
            handle,
        }
    }

    /// Stops the timer; when the kill was armed, if it was.
    fn finish(self) -> Option<Instant> {
        self.cancel.store(true, Ordering::SeqCst);
        self.handle.join().unwrap_or(None)
    }
}

/// A seeded kill time in [`KILL_MS`].
pub fn kill_after(rng: &mut Rng) -> Duration {
    let (lo, hi) = (*KILL_MS.start(), *KILL_MS.end());
    Duration::from_millis(lo + rng.below(hi - lo + 1))
}

/// The checks after a kill, on the restarted node: C2 at its tip, C3 at `floor`, the
/// acknowledged tip's block still stored, and C4 over the active chain and the trajectory up to
/// C3's position (the reorged-out branch included). The node's position, if C3 found one.
fn checks(
    rpc: &Client,
    fixture: &Fixture,
    log: &str,
    floor: Floor,
    seed: u64,
) -> (Option<usize>, Vec<String>) {
    let mut failures = Vec::new();
    let tip = match c2(rpc, fixture, &mut failures) {
        Ok(tip) => tip,
        Err(e) => {
            failures.push(format!("C2: gettxoutsetinfo: {e}"));
            return (None, failures);
        }
    };
    let pos = c3(fixture, &tip, floor, log).map_err(|e| failures.push(e)).ok();
    if let Floor::Ack(p) = floor {
        let acked = &fixture.trajectory[p];
        if let Err(e) = c4_read(rpc, fixture, acked) {
            failures.push(format!("C3: the acknowledged tip {acked} (position {p}): {e}"));
        }
    }
    let mut stored = active_chain(fixture, &tip);
    let mut seen: HashSet<String> = stored.iter().cloned().collect();
    for hash in pos.map_or(&[][..], |p| &fixture.trajectory[..=p]) {
        if seen.insert(hash.clone()) {
            stored.push(hash.clone());
        }
    }
    c4(rpc, fixture, &tip, &stored, seed, &mut failures);
    (pos, failures)
}

/// The kill cycles in `run`'s directory.
pub fn run(rt: &Runtime, run: &Run, fixture: &Fixture, cfg: &Config) -> io::Result<Report> {
    let p2p_pos = fixture.on_trajectory(&fixture.p2p_tip, 0).ok_or_else(|| {
        io::Error::other(format!("the P2P tip {} is not on the trajectory", fixture.p2p_tip))
    })?;
    let mut rng = Rng::new(cfg.seed);
    let mut report = Report::default();
    let mut idle = 0;
    while report.failures.is_empty() && (report.laps == 0 || report.kills.len() < cfg.kills) {
        let lap = report.laps;
        let before = report.kills.len();
        let dir = run.dir().join(format!("lap{lap}"));
        private_dir(&dir)?;
        let passed = match run_lap(rt, fixture, cfg, &mut rng, &mut report, lap, p2p_pos, &dir) {
            Ok(passed) => passed,
            Err(e) => {
                report.failures.push(format!("lap {lap}: {e}"));
                false
            }
        };
        report.laps += 1;
        println!(
            "lap {lap}: {} kills ({} so far), {}",
            report.kills.len() - before,
            report.kills.len(),
            if passed {
                "passed"
            } else {
                "FAILED"
            }
        );
        // The lap directory is the harness's own (`remove_dir_all` follows no symlink).
        if passed && std::env::var_os("ST3B_KEEP_RUNS").is_none() {
            fs::remove_dir_all(&dir)?;
        }
        idle = if report.kills.len() == before {
            idle + 1
        } else {
            0
        };
        if idle >= IDLE_LAPS && report.kills.len() < cfg.kills {
            report.failures.push(format!("{IDLE_LAPS} laps in a row made no kill"));
        }
    }
    Ok(report)
}

/// One lap; whether it passed (its failures are in the report).
#[allow(clippy::too_many_arguments)]
fn run_lap(
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    rng: &mut Rng,
    report: &mut Report,
    lap: usize,
    p2p_pos: usize,
    dir: &Path,
) -> io::Result<bool> {
    let failed_before = report.failures.len();
    let peer = Peer::start(&cfg.dashd, fixture, dir)?;
    let datadir = dir.join("node");
    let (mut floor, mut pos) = (Floor::Start, 0);
    // The kill not yet checked: its index in the report.
    let mut unchecked: Option<usize> = None;
    let mut cycle = Instant::now();
    loop {
        let boot = report.boots;
        report.boots += 1;
        let started = Instant::now();
        let mut c1_failures = Vec::new();
        let node = rt.block_on(c1(&cfg.rdashd, fixture, &datadir, &cfg.extra, &mut c1_failures));
        report.failures.extend(c1_failures.into_iter().map(|f| format!("boot {boot}: {f}")));
        let Some(node) = node else {
            break;
        };
        report.start_times.push(started.elapsed());
        let rpc = node.rpc_with_timeout(RPC_PATIENCE).map_err(io::Error::other)?;
        if let Some(k) = unchecked.take() {
            let seed = cfg.seed ^ boot as u64;
            let (at, failed) = checks(&rpc, fixture, &node.log(), floor, seed);
            report.kills[k].recovered = at;
            report.kills[k].cycle = Some(cycle.elapsed());
            report.failures.extend(failed.into_iter().map(|f| format!("boot {boot}: {f}")));
            if let Some(p) = at {
                (floor, pos) = (Floor::Ack(p), p);
            }
        }
        if report.failures.len() > failed_before {
            break;
        }
        cycle = Instant::now();
        // Past the P2P part the peer stays away: it would serve back blocks the node lost.
        if pos < p2p_pos {
            rpc.add_node(peer.p2p, "add").map_err(io::Error::other)?;
        }
        let armed = Arc::new(AtomicBool::new(false));
        let killer = (report.kills.len() < cfg.kills).then(|| {
            let after = kill_after(rng);
            Killer::spawn(node.pid(), after, Arc::clone(&armed))
        });
        let workload_start = Instant::now();
        let (done, ended) = rt.block_on(resume_until_death(
            &rpc,
            fixture,
            pos,
            cfg.every,
            &mut |_| Ok(()),
            UntilDeath {
                armed: &armed,
                hook: &mut |_| Ok(()),
            },
        ));
        let ended_at = Instant::now();
        let armed_at = killer.and_then(Killer::finish);
        report.failures.extend(done.failures.iter().map(|f| format!("boot {boot}: {f}")));
        floor = floor_after(floor, &done.acks);
        pos = done.acks.last().copied().unwrap_or(pos);
        if let Some(at) = armed_at {
            // rdashd died of the signal, and not before it.
            match rt.block_on(node.wait_exit(REAP_PATIENCE)) {
                Ok(s) if s.status.signal() == Some(libc::SIGKILL) => {}
                Ok(s) => report
                    .failures
                    .push(format!("boot {boot}: rdashd exited {} before the kill", s.status)),
                Err(e) => report.failures.push(format!("boot {boot}: reaping rdashd: {e}")),
            }
            if let Some(e) = ended.as_ref().filter(|_| ended_at < at) {
                report.failures.push(format!(
                    "boot {boot}: the workload ended {:.2} s before the kill: {e}",
                    (at - ended_at).as_secs_f64()
                ));
            }
            report.kills.push(Kill {
                lap,
                boot,
                after: at - workload_start,
                floor,
                recovered: None,
                cycle: None,
            });
            unchecked = Some(report.kills.len() - 1);
            snapshot_killed(&datadir, dir)?;
            if report.failures.len() > failed_before {
                break;
            }
            continue;
        }
        if let Some(e) = ended {
            report.failures.push(format!("boot {boot}: the workload ended: {e}"));
            break;
        }
        // The schedule's last acknowledgement was C2 at the fixture's tip.
        match rt.block_on(node.stop()) {
            Ok(s) if s.is_clean() => report.tips += 1,
            Ok(s) => {
                report.failures.push(format!("boot {boot}: rdashd did not stop cleanly: {s:?}"))
            }
            Err(e) => report.failures.push(format!("boot {boot}: stopping rdashd: {e}")),
        }
        break;
    }
    let passed = report.failures.len() == failed_before;
    peer.stop()?;
    Ok(passed)
}

/// Keeps the datadir as the last kill left it, `<lap dir>/killed`, before the restart recovers it:
/// a failed check's repro (`st3b-crash check` on it replays that recovery). A reflinked copy
/// where the filesystem allows; it replaces the previous kill's.
fn snapshot_killed(datadir: &Path, dir: &Path) -> io::Result<()> {
    let (killed, new) = (dir.join("killed"), dir.join("killed.new"));
    let copied = std::process::Command::new("cp")
        .args(["-a", "--reflink=auto", "--"])
        .arg(datadir)
        .arg(&new)
        .status()?;
    if !copied.success() {
        return Err(io::Error::other(format!("copying {} for its repro", datadir.display())));
    }
    // Both are the harness's own (`remove_dir_all` follows no symlink).
    if killed.exists() {
        fs::remove_dir_all(&killed)?;
    }
    fs::rename(&new, &killed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_times_are_drawn_from_the_seed_within_their_bounds() {
        let (mut a, mut b) = (Rng::new(5), Rng::new(5));
        for _ in 0..1000 {
            let t = kill_after(&mut a);
            assert_eq!(t, kill_after(&mut b));
            assert!(KILL_MS.contains(&(t.as_millis() as u64)), "{t:?}");
        }
        assert_ne!(kill_after(&mut Rng::new(1)), kill_after(&mut Rng::new(2)));
    }

    #[test]
    fn a_cancelled_killer_never_signals() {
        let armed = Arc::new(AtomicBool::new(false));
        // A pid that cannot exist (above pid_max): a stray signal could not land anyway.
        let killer = Killer::spawn(i32::MAX as u32, Duration::from_secs(60), Arc::clone(&armed));
        assert_eq!(killer.finish(), None);
        assert!(!armed.load(Ordering::SeqCst));
        let killer = Killer::spawn(i32::MAX as u32, Duration::ZERO, Arc::clone(&armed));
        std::thread::sleep(Duration::from_millis(50));
        assert!(killer.finish().is_some());
        assert!(armed.load(Ordering::SeqCst));
    }
}
