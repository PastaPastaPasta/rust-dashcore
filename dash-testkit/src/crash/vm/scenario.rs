//! The VM scenarios (PLAN-ST3b-S7 §2.3): kill cycles on one guest disk.
//!
//! A run is laps. A lap starts from a fresh copy of an empty formatted disk and a fresh peer (the
//! fixture's dashd on the host: a peer that saw a lap's submitted blocks would serve them to the
//! next), and runs the fixture's schedule to its tip. Each kill cycle in it:
//!
//! 1. boots the guest (no peers) and waits for RPC;
//! 2. after a kill, checks the recovered node: C1 on this boot's console, C2 through the guest's
//!    RPC, C3 against the floor (the last acknowledgement that returned before the kill), C4;
//! 3. adds the peer while the node is still in the schedule's P2P part, and continues the
//!    schedule with a kill planned (random or triggered); the kill is recorded, then carried out;
//! 4. on a kill, goes back to 1.
//!
//! A lap whose schedule reaches the tip ends with `stop`, the guest's `rdashd exited 0`, a last
//! boot with C1–C4 at the stopped tip (no replay) and a clean stop again, then a read-only fsck of
//! the disk. Laps go on until the scenario's kills are made. V-RECOVERY's kills are double
//! crashes: a first kill on a split flush's partial batch, then on the next boot's replay lines,
//! before RPC answers.
//!
//! A triggered kill lands some time after its line (the console's lag, the jitter, the signal),
//! so it counts only when the evidence shows it landed in its window (S7 r1 M1): V-FLUSH's and
//! V-RECOVERY's when the next boot replays blocks (the flush, or the replay, never committed);
//! V-REORG's when the `submitblock` that set the reorg off had not returned when the kill was
//! armed and the killed boot's console shows the reorg cut mid-way (see [`cut_mid_reorg`];
//! nothing is flushed inside a reorg here, so the node comes back at the last flushed state, and
//! the test is that it does so with no half-applied reorg; S7 r1 ruling 1, r2 N3). Each V-REORG
//! boot aims at one reorg still ahead of it whose quota (its even share of the kills) is not met,
//! drawn from the seed (S7 r2 N6, r3); with none ahead the lap stops killing and runs to its
//! end, and the next lap starts before every reorg again. C3 finding the node
//! strictly inside a reorg's positions is reported apart, the stronger case, not required, and
//! holds only with a flush inside that reorg on the killed boot's console (S7 r2 N4). V-IBD's
//! random kills always count. A kill out of its window still gets every check (C1–C4 at its
//! floor); it is reported, not counted.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dashcore_rpc::Client;
use serde_json::json;
use tokio::runtime::Runtime;

use super::{
    Booted, Disk, Guest, Kill, Killer, Plan, REPLAYING, Tail, Trigger, Vm, add_peer, cut_mid_reorg,
    exit_code, floor_after, flushed_inside_reorg, height_at, reorg_at, reorg_trigger,
    reorg_windows, vm_c1_lines,
};
use crate::NodeRpc;
use crate::crash::baseline::{Peer, UntilDeath, resume_until_death};
use crate::crash::checks::{Floor, c2, c3, c4, c4_read};
use crate::crash::devices::{Fs, Run, private_dir};
use crate::crash::fixture::Fixture;
use crate::crash::logwrites::Rng;

/// How long a stopped guest may take to power off.
const STOP_PATIENCE: Duration = Duration::from_secs(300);
/// Laps in a row without a kill end the run: a trigger that never fires would loop forever.
const IDLE_LAPS: usize = 3;
/// Kills in a row that neither counted nor left the node past where their workload started end
/// the lap's kills: each boot would redo the same stretch and be killed in it again, and the lap
/// would never end (V-REORG: a reorg cut before any flush comes back before it, every time). The
/// lap then runs to its end, and [`IDLE_LAPS`] decides.
const STUCK_KILLS: usize = 5;

/// The count of stuck kills (see [`STUCK_KILLS`]) after one is checked: whether it `counted`, where
/// the next boot found the node (`at`), and where its workload had started (`from`).
fn stuck_after(stuck: usize, counted: bool, at: Option<usize>, from: usize) -> usize {
    if counted || at.is_some_and(|p| p > from) {
        0
    } else {
        stuck + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scenario {
    Ibd,
    Reorg,
    Flush,
    Recovery,
    /// A stub until CH5 (Q3, BL-507): reported `SKIPPED (CH5)`, never run or counted.
    Reindex,
}

impl Scenario {
    pub fn parse(s: &str) -> Option<Scenario> {
        Some(match s {
            "V-IBD" => Scenario::Ibd,
            "V-REORG" => Scenario::Reorg,
            "V-FLUSH" => Scenario::Flush,
            "V-RECOVERY" => Scenario::Recovery,
            "V-REINDEX" => Scenario::Reindex,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Scenario::Ibd => "V-IBD",
            Scenario::Reorg => "V-REORG",
            Scenario::Flush => "V-FLUSH",
            Scenario::Recovery => "V-RECOVERY",
            Scenario::Reindex => "V-REINDEX",
        }
    }

    /// The kind of kill that counts toward the scenario's kills.
    pub fn kind(self) -> &'static str {
        match self {
            Scenario::Ibd => "random",
            Scenario::Reorg => "reorg",
            Scenario::Flush => "flush",
            Scenario::Recovery => "recovery",
            Scenario::Reindex => "-",
        }
    }

    /// The kind of a workload's kill: V-RECOVERY's is the first crash of a pair.
    fn workload_kind(self) -> &'static str {
        match self {
            Scenario::Recovery => "partial",
            s => s.kind(),
        }
    }

    /// The plan of a workload's kill (V-RECOVERY's: the first crash of a pair); V-REORG's on its
    /// target reorg's trigger (see [`reorg_trigger`]), none without one.
    fn plan(self, rng: &mut Rng, reorg: Option<Trigger>) -> Option<Plan> {
        Some(match self {
            Scenario::Ibd => Plan::random(rng),
            Scenario::Reorg => Plan::on(reorg?, rng),
            Scenario::Flush => Plan::on(Trigger::Flush, rng),
            Scenario::Recovery => Plan::on(Trigger::Partial, rng),
            Scenario::Reindex => unreachable!("V-REINDEX never runs"),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub scenario: Scenario,
    pub fs: Fs,
    /// The scenario's own kills to make (V-RECOVERY: double crashes).
    pub kills: usize,
    pub seed: u64,
    /// The fixture's dashd, the peer.
    pub dashd: PathBuf,
    /// `mkguest.sh`'s out dir.
    pub guest: PathBuf,
    /// rdashd's arguments beyond the fixture's chain (weekly-vm.sh: `-dbcache=4`, with
    /// `-dbbatchsize=20000` for V-IBD and V-REORG, 2000 for V-FLUSH and V-RECOVERY, S7 r4).
    pub extra: Vec<String>,
    /// An acknowledgement every this many submitted blocks.
    pub every: usize,
}

/// One kill.
#[derive(Clone, Debug)]
pub struct KillRecord {
    pub lap: usize,
    pub boot: usize,
    /// `random`, or the trigger's name; V-RECOVERY's first crash is `partial`, its second
    /// `recovery`.
    pub kind: &'static str,
    /// Whether the evidence shows the kill landed in its window (see [`in_window`]); `None`
    /// until the next boot's evidence is in.
    pub in_window: Option<bool>,
    /// A reorg kill: its boot's target, an index into the trajectory's reorgs (see
    /// [`reorg_windows`]).
    pub reorg: Option<usize>,
    /// A reorg kill: whether the `submitblock` of the block that sets the target reorg off had
    /// not returned when the kill was armed (S7 r2 N3): the host's bound, which no console lag
    /// can stretch.
    pub pending: Option<bool>,
    /// A reorg kill: whether the killed boot's console shows the reorg cut mid-way (see
    /// [`cut_mid_reorg`]).
    pub mid_reorg: Option<bool>,
    /// A reorg kill: whether C3 found the node strictly inside a reorg's positions (the stronger
    /// case: a durable state inside the reorg), once the next boot's checks ran.
    pub inside: Option<bool>,
    /// Counted toward the scenario's kills: the scenario's own kind, in its window.
    pub counted: bool,
    pub kill: Kill,
    /// The floor the next boot's C3 holds the node to.
    pub floor: Floor,
    /// The trajectory position the killed boot's workload started from, and that boot's console
    /// (a double crash: the first kill's), which C3 reads for a flush inside a reorg (S7 r2 N4).
    pub from: usize,
    pub console: PathBuf,
    /// Where the next boot's checks found the node (C3's position), once they ran.
    pub recovered: Option<usize>,
    /// From the start of the killed boot to the end of the next boot's checks (a double crash:
    /// of the boot after the second kill).
    pub cycle: Option<Duration>,
}

impl KillRecord {
    /// A kill not yet checked: no window, no recovery position, no cycle time; it came on a boot
    /// that started at position `from`, with its console at `console`.
    fn new(
        lap: usize,
        boot: usize,
        kind: &'static str,
        kill: Kill,
        floor: Floor,
        (from, console): (usize, PathBuf),
    ) -> KillRecord {
        KillRecord {
            lap,
            boot,
            kind,
            in_window: None,
            reorg: None,
            pending: None,
            mid_reorg: None,
            inside: None,
            counted: false,
            kill,
            floor,
            from,
            console,
            recovered: None,
            cycle: None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    /// The scenario's own kind of kill (see [`Scenario::kind`]).
    pub kind: &'static str,
    pub kills: Vec<KillRecord>,
    /// The scenario's own kills in their window.
    pub counted: usize,
    pub laps: usize,
    pub boots: usize,
    /// From `docker run` to the first RPC answer, per boot that answered.
    pub boot_times: Vec<Duration>,
    /// The trajectory's reorgs (see [`reorg_windows`]): [`KillRecord::reorg`] indexes it.
    pub windows: Vec<(usize, usize)>,
    /// V-REORG: each reorg's quota of counted kills (see [`reorg_quotas`]); empty otherwise.
    pub quotas: Vec<usize>,
    pub failures: Vec<String>,
}

impl Report {
    /// Records a kill; returns its index.
    fn record(&mut self, kill: KillRecord) -> usize {
        self.kills.push(kill);
        self.kills.len() - 1
    }

    /// Settles kill `k`'s window, counting it if it is the scenario's own kind and in it, and
    /// prints the totals so far (see [`Report::so_far`]).
    fn classify(&mut self, k: usize, in_window: bool) {
        let kill = &mut self.kills[k];
        kill.in_window = Some(in_window);
        kill.counted = in_window && kill.kind == self.kind;
        self.counted += usize::from(kill.counted);
        println!("{}", self.so_far());
    }

    /// The running totals after a checked kill, which a weekly component cut by its timeout
    /// records (S7 r2 N-c): all kills, the scenario's own, those counted, and those C3 found
    /// strictly inside a reorg.
    pub fn so_far(&self) -> String {
        format!(
            "so far: kills {}, own {}, counted {}, inside {}",
            self.kills.len(),
            self.own(),
            self.counted,
            self.inside()
        )
    }

    /// The counted kills aimed at reorg `w`.
    pub fn counted_on(&self, w: usize) -> usize {
        self.kills.iter().filter(|k| k.counted && k.reorg == Some(w)).count()
    }

    /// The scenario's own kills, in their window or not.
    pub fn own(&self) -> usize {
        self.kills.iter().filter(|k| k.kind == self.kind).count()
    }

    /// The reorg kills C3 found strictly inside a reorg's positions.
    pub fn inside(&self) -> usize {
        self.kills.iter().filter(|k| k.inside == Some(true)).count()
    }
}

/// Whether a kill of `kind` landed in its window (S7 r1 M1, ruling 1, r2 N3): whether the next
/// boot's console `replayed` blocks, and whether the power went mid-reorg (`mid_reorg`: the
/// submit that set the reorg off had not returned when the kill was armed, and the killed boot's
/// console shows the reorg cut mid-way, see [`cut_mid_reorg`]).
/// - `flush` and `partial`: the next boot replays, so the flush's last batch never committed.
/// - `recovery`: the next boot replays again, so the first replay's result never committed.
/// - `reorg`: the power went mid-reorg. Nothing is flushed inside a reorg at W-small's heights
///   and `-dbcache=4` (Core's minimum), so the recovery is to the last flushed state, which
///   C1–C4 check: no half-applied reorg. A durable state inside a reorg is the device level's
///   (D-REORG-S); C3 finding one is reported apart (`KillRecord::inside`), not required, and
///   needs a flush inside the reorg (S7 r2 N4).
/// - `random`: always.
pub fn in_window(kind: &str, replayed: bool, mid_reorg: bool) -> bool {
    match kind {
        "flush" | "partial" | "recovery" => replayed,
        "reorg" => mid_reorg,
        _ => true,
    }
}

/// V-REORG's per-reorg quotas (S7 r3): `kills` split evenly over `reorgs`, the remainder to the
/// last, so a run that makes its kills has cut every reorg (a counted kill past a reorg recovers
/// past it, so without quotas the later reorgs take every kill after a lap's first boot).
pub fn reorg_quotas(kills: usize, reorgs: usize) -> Vec<usize> {
    if reorgs == 0 {
        return Vec::new();
    }
    let mut quotas = vec![kills / reorgs; reorgs];
    quotas[reorgs - 1] += kills % reorgs;
    quotas
}

/// A V-REORG boot's target (S7 r2 N6, r3): drawn from the seed among the reorgs still ahead of
/// position `pos` (their top at or after it) whose `counted` kills are below their `quotas`; none
/// when no such reorg is left.
pub fn draw_reorg(
    rng: &mut Rng,
    windows: &[(usize, usize)],
    pos: usize,
    counted: impl Fn(usize) -> usize,
    quotas: &[usize],
) -> Option<usize> {
    let open: Vec<usize> = (0..windows.len())
        .filter(|&w| windows[w].0 >= pos && counted(w) < quotas.get(w).copied().unwrap_or(0))
        .collect();
    (!open.is_empty()).then(|| open[rng.below(open.len() as u64) as usize])
}

/// A run's fixed parts.
struct Ctx<'a> {
    rt: &'a Runtime,
    fixture: &'a Fixture,
    cfg: &'a Config,
    vm: Vm,
    args: Vec<String>,
    p2p_pos: usize,
    /// The trajectory's reorgs (see [`reorg_windows`]).
    windows: Vec<(usize, usize)>,
}

/// The kill cycles of one scenario in `run`'s directory. `Reindex` is the caller's to skip.
pub fn run(rt: &Runtime, run: &Run, fixture: &Fixture, cfg: &Config) -> io::Result<Report> {
    assert_ne!(cfg.scenario, Scenario::Reindex, "V-REINDEX is SKIPPED (CH5)");
    let vm = Vm::new(&cfg.guest, run.id())?;
    let args = super::guest_args(fixture, &cfg.extra, &vm.rpcauth()).map_err(io::Error::other)?;
    super::cmdline(cfg.fs, &args).map_err(io::Error::other)?;
    let p2p_pos = fixture.on_trajectory(&fixture.p2p_tip, 0).ok_or_else(|| {
        io::Error::other(format!("the P2P tip {} is not on the trajectory", fixture.p2p_tip))
    })?;
    println!(
        "image {} aio={} kernel {} initramfs {}\nrdashd args {args:?}",
        vm.image,
        super::AIO,
        vm.kernel.display(),
        vm.initramfs.display(),
    );
    for line in vm.manifest.lines().filter(|l| !l.starts_with(|c: char| c.is_ascii_hexdigit())) {
        println!("manifest {line}");
    }
    let ctx = Ctx {
        rt,
        fixture,
        cfg,
        vm,
        args,
        p2p_pos,
        windows: reorg_windows(fixture),
    };
    let empty = Disk::create(&run.dir().join("empty.raw"), cfg.fs)?;
    let mut rng = Rng::new(cfg.seed);
    let mut report = Report {
        kind: cfg.scenario.kind(),
        windows: ctx.windows.clone(),
        quotas: if cfg.scenario == Scenario::Reorg {
            reorg_quotas(cfg.kills, ctx.windows.len())
        } else {
            Vec::new()
        },
        ..Report::default()
    };
    let mut idle = 0;
    while report.failures.is_empty() && (report.laps == 0 || report.counted < cfg.kills) {
        let lap = report.laps;
        let (before, counted_before) = (report.kills.len(), report.counted);
        let dir = run.dir().join(format!("lap{lap}"));
        private_dir(&dir)?;
        // An I/O error ends the run but keeps what the report holds (S7 r1 #6).
        let passed = match run_lap(&ctx, &mut rng, &mut report, lap, &empty, &dir) {
            Ok(passed) => passed,
            Err(e) => {
                report.failures.push(format!("lap {lap}: {e}"));
                false
            }
        };
        report.laps += 1;
        println!(
            "lap {lap}: {} kills ({} counted so far), {}",
            report.kills.len() - before,
            report.counted,
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
        // Counted kills: V-RECOVERY's first crashes come every lap even when no replay follows.
        idle = if report.counted == counted_before {
            idle + 1
        } else {
            0
        };
        if idle >= IDLE_LAPS && report.counted < cfg.kills {
            report.failures.push(format!("{IDLE_LAPS} laps in a row made no counted kill"));
        }
    }
    // V-REORG passes only with every reorg's quota met (S7 r3).
    for (w, &quota) in report.quotas.iter().enumerate() {
        let counted = report.counted_on(w);
        if counted < quota {
            report
                .failures
                .push(format!("reorg {}: {counted} of its quota {quota} counted", w + 1));
        }
    }
    empty.remove()?;
    Ok(report)
}

/// The checks after a kill (C2–C4; C1 reads the console when the boot ends): the node's
/// position on the trajectory, if C3 found one.
/// - C3 also holds the node to the floor's block: at `Floor::Ack(p)`, the acknowledged tip's
///   block must still be stored with the fixture's bytes (the device level's C0 does this). A tip
///   inside a reorg's positions must come with the block that set the reorg off: W-small's
///   trajectory repeats the disconnected hashes, so a node that lost its last flush could resolve
///   to a disconnect position after its floor (S7 r1 m1). After a kill (`killed`), it must also
///   come with a flush inside that reorg on the killed boot's console, unless the node is where
///   that boot started (S7 r2 N4, see [`flushed_inside_reorg`]).
/// - C4 reads, beside the active chain, every trajectory block up to the node's position that is
///   off it (the reorged-out branch), and F01's side blocks once the node knows their headers
///   (S7 r1 m7, r2 N5).
fn checks(
    ctx: &Ctx,
    rpc: &Client,
    console: &str,
    floor: Floor,
    seed: u64,
    killed: Option<&KillRecord>,
) -> (Option<usize>, Vec<String>) {
    let mut failures = Vec::new();
    let tip = match c2(rpc, ctx.fixture, &mut failures) {
        Ok(tip) => tip,
        Err(e) => {
            failures.push(format!("C2: gettxoutsetinfo: {e}"));
            return (None, failures);
        }
    };
    let pos = match c3(ctx.fixture, &tip, floor, console) {
        Ok(p) => Some(p),
        Err(e) => {
            failures.push(e);
            None
        }
    };
    if let Floor::Ack(p) = floor {
        let acked = &ctx.fixture.trajectory[p];
        if let Err(e) = c4_read(rpc, ctx.fixture, acked) {
            failures.push(format!("C3: the acknowledged tip {acked} (position {p}): {e}"));
        }
    }
    if let Some((p, (top, end))) = pos.and_then(|p| Some((p, reorg_at(&ctx.windows, p)?))) {
        let trigger = ctx.fixture.trajectory.get(end).map_or("-", String::as_str);
        if let Err(e) = rpc.call("getblockheader", &[json!(trigger)]) {
            failures.push(format!(
                "C3: tip {tip} is inside the reorg that {trigger} (position {end}) sets off, \
                 but the node does not know that block: {e}"
            ));
        }
        if let Some(k) = killed {
            let started_inside = top < k.from && k.from < end;
            let flushed = match fs::read(&k.console) {
                Ok(bytes) => height_at(ctx.fixture, top).is_some_and(|t| {
                    let console = String::from_utf8_lossy(&bytes);
                    flushed_inside_reorg(
                        &console,
                        height_at(ctx.fixture, k.from),
                        t,
                        started_inside,
                    )
                }),
                Err(e) => {
                    failures.push(format!("C3: reading {}: {e}", k.console.display()));
                    false
                }
            };
            if !flushed && !(started_inside && p == k.from) {
                failures.push(format!(
                    "C3: tip {tip} (position {p}) is strictly inside the reorg at positions \
                     {top}..{end}, but the killed boot's console shows no flush inside it: \
                     acknowledged state was lost, or appeared without a flush"
                ));
            }
        }
    }
    let mut stored = super::active_chain(ctx.fixture, &tip);
    let mut seen: HashSet<String> = stored.iter().cloned().collect();
    if let Some(p) = pos {
        for hash in &ctx.fixture.trajectory[..=p] {
            if seen.insert(hash.clone()) {
                stored.push(hash.clone());
            }
        }
    }
    c4(rpc, ctx.fixture, &tip, &stored, seed, &mut failures);
    // F01's F and C: only once the node knows them (C may come after the point). F and C come by
    // `submitblock`, so a known header means their data was submitted, and it must read back
    // (S7 r2 N5, the device level's C0 rule).
    if let Some(f) = &ctx.fixture.f01 {
        for hash in [&f.f_hash, &f.c_hash] {
            if rpc.call("getblockheader", &[json!(hash)]).is_err() {
                continue;
            }
            match rpc.call("getblock", &[json!(hash), json!(0)]) {
                Ok(got) if got.as_str() == ctx.fixture.block_hex(hash) => {}
                Ok(_) => failures.push(format!("C4: getblock {hash} differs from the fixture")),
                Err(e) => failures.push(format!(
                    "C4: the node knows {hash}'s header but cannot read the block: {e}"
                )),
            }
        }
    }
    (pos, failures)
}

/// Ends a boot's guest (its container killed and removed, if it still runs) and runs C1 on its
/// whole console. A boot the harness `killed` must not have seen rdashd exit first: that is a
/// crash of rdashd's own, not the power cut (S7 r1 #7).
fn end_boot(guest: &mut Guest, boot: usize, killed: bool, report: &mut Report) -> io::Result<()> {
    guest.kill()?;
    let console = guest.console();
    report.failures.extend(vm_c1_lines(&console).into_iter().map(|l| format!("boot {boot}: {l}")));
    // A boot killed before rdashd's init logged its profile has no line; one that got through
    // init must show the profile it was asked for (BL-634).
    let started = console.contains("init message: Done loading");
    if let Some(why) = crate::crash::baseline::profile_failure(&console, guest.args(), started) {
        report.failures.push(format!("boot {boot}: C1: {why}"));
    }
    if let Some(code) = exit_code(&console).filter(|_| killed) {
        report.failures.push(format!("boot {boot}: rdashd exited {code} before the kill"));
    }
    Ok(())
}

/// The failures of a kill itself (S7 r1 m3): the guest had stopped before the signal, or the
/// workload had ended (`ended`, at `ended_at`) before the kill was armed: rdashd died, or stopped
/// answering, of its own accord.
fn kill_failures(boot: usize, kill: &Kill, ended: Option<(&io::Error, Instant)>) -> Vec<String> {
    let mut failures = Vec::new();
    if !kill.landed {
        failures.push(format!("boot {boot}: the guest had stopped before the kill"));
    }
    if let Some((e, at)) = ended.filter(|&(_, at)| at < kill.armed_at) {
        let early = kill.armed_at - at;
        failures.push(format!(
            "boot {boot}: the workload ended {:.2} s before the kill: {e}",
            early.as_secs_f64()
        ));
    }
    failures
}

/// One lap; whether it passed (its failures are in the report).
fn run_lap(
    ctx: &Ctx,
    rng: &mut Rng,
    report: &mut Report,
    lap: usize,
    empty: &Disk,
    dir: &Path,
) -> io::Result<bool> {
    let failed_before = report.failures.len();
    let peer = Peer::start(&ctx.cfg.dashd, ctx.fixture, dir)?;
    let disk = empty.copy_to(&dir.join("data.raw"))?;
    let (mut floor, mut pos) = (Floor::Start, 0);
    // A kill made and not yet checked: its record's index.
    let mut unchecked: Option<usize> = None;
    // C's expected status after F01's pair, from the last boot that ran the pair.
    let mut c_expected = None;
    // V-RECOVERY: the next boot's init is watched for its replay.
    let mut watch_init = false;
    let mut cycle = Instant::now();
    let mut clean = false;
    // Where the last killed workload started, and the kills in a row that got nowhere.
    let (mut kill_from, mut stuck) = (0, 0);
    loop {
        let boot = report.boots;
        report.boots += 1;
        let init = watch_init.then(|| Plan::on(Trigger::Recovery, rng));
        watch_init = false;
        let Booted {
            mut guest,
            kill: init_kill,
            ready,
        } = ctx.vm.boot_ready(boot, dir, &disk, ctx.cfg.fs, &ctx.args, init)?;
        if let Some(kill) = init_kill {
            // The partial kill before it left blocks to replay: its window's evidence. Its boot
            // ran the workload, so its console is the one C3 reads after the pair.
            let mut workload = (pos, guest.console_path().to_owned());
            if let Some(k) = unchecked.take() {
                let replayed = guest.console().contains(REPLAYING);
                report.classify(k, in_window(report.kills[k].kind, replayed, false));
                workload = (report.kills[k].from, report.kills[k].console.clone());
            }
            report.failures.extend(kill_failures(boot, &kill, None));
            end_boot(&mut guest, boot, true, report)?;
            let record = KillRecord::new(lap, boot, "recovery", kill, floor, workload);
            unchecked = Some(report.record(record));
            if report.failures.len() > failed_before {
                break;
            }
            continue;
        }
        match ready {
            Ok(t) => report.boot_times.push(t),
            Err(e) => {
                report.failures.push(format!("C1: boot {boot}: {e}"));
                end_boot(&mut guest, boot, false, report)?;
                break;
            }
        }
        let rpc = guest.rpc().map_err(io::Error::other)?;
        if let Some(k) = unchecked.take() {
            let console = guest.console();
            let seed = ctx.cfg.seed ^ boot as u64;
            let (at, failed) = checks(ctx, &rpc, &console, floor, seed, Some(&report.kills[k]));
            let record = &mut report.kills[k];
            if record.kind == "reorg" {
                record.inside = Some(at.is_some_and(|p| reorg_at(&ctx.windows, p).is_some()));
            }
            let mid = record.mid_reorg.unwrap_or(false) && record.pending.unwrap_or(false);
            let window = in_window(record.kind, console.contains(REPLAYING), mid);
            report.classify(k, window);
            stuck = stuck_after(stuck, report.kills[k].counted, at, kill_from);
            if stuck == STUCK_KILLS {
                println!(
                    "lap {lap}: {STUCK_KILLS} kills in a row neither counted nor got past \
                     position {kill_from}; no more kills this lap"
                );
            }
            let record = &mut report.kills[k];
            record.recovered = at;
            record.cycle = Some(cycle.elapsed());
            if !failed.is_empty() {
                report.failures.extend(failed.into_iter().map(|f| format!("boot {boot}: {f}")));
                end_boot(&mut guest, boot, false, report)?;
                break;
            }
            if let Some(p) = at {
                (floor, pos) = (Floor::Ack(p), p);
            }
        }
        cycle = Instant::now();
        // Past the P2P part the peer stays away: it would serve back blocks the node lost.
        if pos < ctx.p2p_pos {
            add_peer(&rpc, peer.p2p.port()).map_err(io::Error::other)?;
        }
        let armed = Arc::new(AtomicBool::new(false));
        kill_from = pos;
        let start = height_at(ctx.fixture, pos);
        // V-REORG: this boot's target, one of the reorgs still ahead with quota left, drawn from
        // the seed (S7 r2 N6, r3). None left: no kill, and the lap runs to its end.
        let mut target = None;
        let killer = (report.counted < ctx.cfg.kills && stuck < STUCK_KILLS).then(|| {
            if ctx.cfg.scenario == Scenario::Reorg {
                let counted = |w| report.counted_on(w);
                target = draw_reorg(rng, &ctx.windows, pos, counted, &report.quotas);
                if target.is_none() {
                    println!(
                        "lap {lap}: no reorg at or after position {pos} has quota left; no more \
                         kills this lap"
                    );
                }
            }
            let trigger = target.and_then(|w| reorg_trigger(ctx.fixture, ctx.windows[w]));
            let plan = ctx.cfg.scenario.plan(rng, trigger)?;
            if let Some(w) = target {
                let (top, end) = ctx.windows[w];
                println!(
                    "boot {boot}: aiming at reorg {} (positions {top}..{end}), {plan:?}",
                    w + 1
                );
            }
            Some(
                Tail::at_end(guest.console_path())
                    .map(|t| Killer::spawn(&guest, t, plan, start, Arc::clone(&armed))),
            )
        });
        let killer = killer.flatten().transpose()?;
        let (done, ended) = ctx.rt.block_on(resume_until_death(
            &rpc,
            ctx.fixture,
            pos,
            ctx.cfg.every,
            &mut |_| Ok(()),
            UntilDeath {
                armed: &armed,
                hook: &mut |_| Ok(()),
            },
            &mut c_expected,
        ));
        let ended_at = Instant::now();
        let kill = killer.map(Killer::finish).transpose()?.flatten();
        report.failures.extend(done.failures.iter().map(|f| format!("boot {boot}: {f}")));
        floor = floor_after(floor, &done.acks);
        pos = done.acks.last().copied().unwrap_or(pos);
        if let Some(kill) = kill {
            report.failures.extend(kill_failures(
                boot,
                &kill,
                ended.as_ref().map(|e| (e, ended_at)),
            ));
            end_boot(&mut guest, boot, true, report)?;
            let kind = ctx.cfg.scenario.workload_kind();
            let workload = (kill_from, guest.console_path().to_owned());
            let k = report.record(KillRecord::new(lap, boot, kind, kill, floor, workload));
            if kind == "reorg" {
                let record = &mut report.kills[k];
                record.reorg = target;
                // The host's bound (S7 r2 N3): the block that sets the reorg off is delivered by
                // one `submitblock`, inside which the reorg runs; once that call returned, the
                // reorg was over, whatever the console had shown yet.
                let trigger = target.and_then(|w| ctx.fixture.trajectory.get(ctx.windows[w].1));
                record.pending = Some(trigger.is_some_and(|hash| {
                    done.returned.get(hash).is_none_or(|&t| t > record.kill.armed_at)
                }));
                record.mid_reorg = Some(cut_mid_reorg(&guest.console(), start));
            }
            unchecked = Some(k);
            watch_init = ctx.cfg.scenario == Scenario::Recovery;
            if report.failures.len() > failed_before {
                break;
            }
            continue;
        }
        if let Some(e) = ended {
            report.failures.push(format!("boot {boot}: the workload ended: {e}"));
            end_boot(&mut guest, boot, false, report)?;
            break;
        }
        if report.failures.len() > failed_before {
            end_boot(&mut guest, boot, false, report)?;
            break;
        }
        // The tip: a clean stop, then a last boot on the stopped disk.
        stop(&rpc, &mut guest, boot, report)?;
        let last = report.boots;
        report.boots += 1;
        let Booted {
            mut guest,
            ready,
            ..
        } = ctx.vm.boot_ready(last, dir, &disk, ctx.cfg.fs, &ctx.args, None)?;
        match ready {
            Ok(t) => {
                report.boot_times.push(t);
                let rpc = guest.rpc().map_err(io::Error::other)?;
                let stopped = Floor::Stopped(pos);
                let seed = ctx.cfg.seed ^ last as u64;
                let (_, failed) = checks(ctx, &rpc, &guest.console(), stopped, seed, None);
                report.failures.extend(failed.into_iter().map(|f| format!("boot {last}: {f}")));
                stop(&rpc, &mut guest, last, report)?;
                clean = true;
            }
            Err(e) => {
                report.failures.push(format!("C1: boot {last}: {e}"));
                end_boot(&mut guest, last, false, report)?;
            }
        }
        break;
    }
    // The guest's own filesystem check on the disk the last boot left cleanly unmounted (S7 r1 m2).
    if clean
        && report.failures.len() == failed_before
        && let Some(damage) = disk.fsck(ctx.cfg.fs)?
    {
        report.failures.push(format!("lap {lap}: fsck: {damage}"));
    }
    let passed = report.failures.len() == failed_before;
    peer.stop()?;
    if passed {
        disk.remove()?;
    }
    Ok(passed)
}

/// `stop`, then the guest's own power-off with `rdashd exited 0`, and C1 on its console.
fn stop(rpc: &Client, guest: &mut Guest, boot: usize, report: &mut Report) -> io::Result<()> {
    if let Err(e) = rpc.call("stop", &[]) {
        report.failures.push(format!("boot {boot}: stop: {e}"));
    }
    match guest.wait_exit(STOP_PATIENCE) {
        Ok(Some(0)) => {}
        Ok(code) => report.failures.push(format!("boot {boot}: rdashd exited {code:?}, not 0")),
        Err(e) => report.failures.push(format!("boot {boot}: {e}")),
    }
    end_boot(guest, boot, false, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_parse_by_name() {
        for s in
            [Scenario::Ibd, Scenario::Reorg, Scenario::Flush, Scenario::Recovery, Scenario::Reindex]
        {
            assert_eq!(Scenario::parse(s.name()), Some(s));
        }
        assert_eq!(Scenario::parse("V-X"), None);
    }

    #[test]
    fn each_scenario_plans_its_own_kill() {
        let mut rng = Rng::new(3);
        assert!(matches!(Scenario::Ibd.plan(&mut rng, None), Some(Plan::At(_))));
        let reorg = Trigger::Reorg {
            top: 1100,
            lines: 5,
        };
        let on = |s: Scenario, rng: &mut Rng, r| match s.plan(rng, r) {
            Some(Plan::OnMatch {
                trigger,
                ..
            }) => Some(trigger),
            _ => None,
        };
        assert_eq!(on(Scenario::Reorg, &mut rng, Some(reorg)), Some(reorg));
        // No reorg ahead: no kill.
        assert_eq!(Scenario::Reorg.plan(&mut rng, None), None);
        assert_eq!(on(Scenario::Flush, &mut rng, Some(reorg)), Some(Trigger::Flush));
        assert_eq!(on(Scenario::Recovery, &mut rng, None), Some(Trigger::Partial));
        assert_eq!(Scenario::Recovery.workload_kind(), "partial");
        assert_eq!(Scenario::Flush.workload_kind(), Scenario::Flush.kind());
    }

    #[test]
    fn kills_that_get_nowhere_stop_the_laps_kills() {
        let mut stuck = 0;
        for _ in 0..STUCK_KILLS {
            stuck = stuck_after(stuck, false, Some(1100), 1100);
        }
        assert_eq!(stuck, STUCK_KILLS);
        // A counted kill, or one the node got past its start from, starts the count again.
        assert_eq!(stuck_after(3, true, Some(1100), 1100), 0);
        assert_eq!(stuck_after(3, false, Some(1101), 1100), 0);
        assert_eq!(stuck_after(3, false, None, 1100), 4);
    }

    #[test]
    fn a_kill_counts_only_in_its_window() {
        for kind in ["flush", "partial", "recovery"] {
            assert!(in_window(kind, true, false));
            assert!(!in_window(kind, false, true));
        }
        // A reorg kill: by the killed boot's console, whatever the next boot replays.
        assert!(in_window("reorg", false, true));
        assert!(!in_window("reorg", true, false));
        assert!(in_window("random", false, false));
    }

    fn a_kill(landed: bool) -> Kill {
        Kill {
            after: Duration::from_secs(1),
            line: None,
            armed_at: Instant::now(),
            landed,
        }
    }

    #[test]
    fn reorg_kills_are_drawn_within_each_reorgs_quota() {
        // An even split, the remainder to the last.
        assert_eq!(reorg_quotas(4, 2), [2, 2]);
        assert_eq!(reorg_quotas(5, 2), [2, 3]);
        assert_eq!(reorg_quotas(50, 2), [25, 25]);
        assert_eq!(reorg_quotas(1, 2), [0, 1]);
        assert_eq!(reorg_quotas(3, 0), Vec::<usize>::new());
        // W-small's reorgs.
        let windows = [(1100, 1107), (1356, 1373)];
        let quotas = reorg_quotas(4, 2);
        let mut rng = Rng::new(9);
        let mut seen = [0; 2];
        for _ in 0..200 {
            let w = draw_reorg(&mut rng, &windows, 0, |_| 0, &quotas).expect("both open");
            seen[w] += 1;
        }
        assert!(seen.iter().all(|&n| n > 0), "both drawn: {seen:?}");
        // A reorg whose quota is met is never drawn; at its top, reorg 1 is still ahead.
        for _ in 0..50 {
            let met_1 = |w| [2, 0][w];
            assert_eq!(draw_reorg(&mut rng, &windows, 1100, met_1, &quotas), Some(1));
            let met_2 = |w| [1, 2][w];
            assert_eq!(draw_reorg(&mut rng, &windows, 0, met_2, &quotas), Some(0));
        }
        // Past reorg 1 with reorg 2's quota met, or a quota of 0: nothing left, no kill.
        assert_eq!(draw_reorg(&mut rng, &windows, 1303, |w| [0, 2][w], &quotas), None);
        assert_eq!(draw_reorg(&mut rng, &windows, 0, |_| 0, &reorg_quotas(1, 2)), Some(1));
        assert_eq!(draw_reorg(&mut rng, &windows, 1400, |_| 0, &quotas), None);
        // The same seed draws the same targets.
        let draws = |seed| {
            let mut rng = Rng::new(seed);
            (0..20).map(|_| draw_reorg(&mut rng, &windows, 0, |_| 0, &quotas)).collect::<Vec<_>>()
        };
        assert_eq!(draws(4), draws(4));
    }

    #[test]
    fn only_the_scenarios_own_kills_in_their_window_count() {
        let mut report = Report {
            kind: "recovery",
            ..Report::default()
        };
        let mut record = |boot, kind| {
            let workload = (0, PathBuf::from("console-0.log"));
            report.record(KillRecord::new(0, boot, kind, a_kill(true), Floor::Start, workload))
        };
        let partial = record(0, "partial");
        let first = record(1, "recovery");
        let second = record(3, "recovery");
        report.classify(partial, true);
        report.classify(first, false);
        report.classify(second, true);
        assert_eq!((report.counted, report.own()), (1, 2));
        assert_eq!(report.so_far(), "so far: kills 3, own 2, counted 1, inside 0");
        assert!(!report.kills[partial].counted && report.kills[partial].in_window == Some(true));
        assert!(!report.kills[first].counted && report.kills[second].counted);
    }

    #[test]
    fn a_kill_after_the_guest_or_the_workload_died_fails() {
        assert!(kill_failures(3, &a_kill(true), None).is_empty());
        let gone = kill_failures(3, &a_kill(false), None);
        assert_eq!(gone, ["boot 3: the guest had stopped before the kill"]);
        let kill = a_kill(true);
        let e = io::Error::other("connection refused");
        // The workload's death after the kill (the kill's own effect) is no failure; before, it is.
        assert!(
            kill_failures(3, &kill, Some((&e, kill.armed_at + Duration::from_millis(5))))
                .is_empty()
        );
        let early = kill_failures(3, &kill, Some((&e, kill.armed_at - Duration::from_millis(500))));
        assert_eq!(early.len(), 1);
        assert!(early[0].contains("ended 0.50 s before the kill: connection refused"), "{early:?}");
    }
}
