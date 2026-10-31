//! A device-level crash campaign (S4): a fixture's schedule recorded through `dm-log-writes`, then
//! its flush, FUA, random and torn points replayed onto image copies, each checked by C0–C4, and by
//! C5 on a sample: every point with a marker, and one in `c5_every` of the others.
//!
//! The recording runs rdashd from a fresh datadir on the device under test, the peer dashd on
//! plain disk, and must split at least one coins flush. It marks `start` once the filesystem is
//! up, `a<k>` right after acknowledgement `k` returns, and `stop` after rdashd's clean stop, so a
//! point's freshness floor (C3) is the last of those its state must hold.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io;
use std::ops::RangeInclusive;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::runtime::Runtime;

use super::baseline;
use super::checks::{self, Floor};
use super::devices::{Fs, Priv, Run, Stack, remove_own_file, sectors, sparse_file};
use super::fixture::Fixture;
use super::image::{Replayer, reflink};
use super::inspect::{self, Keys};
use super::logwrites::{Draw, FLUSH, Log, Point, PointKind, points, replay_order, window_end};

/// The device under test, and its log (sparse: a W-small run writes far less).
const DATA_LEN: u64 = 2 << 30;
const LOG_LEN: u64 = 16 << 30;

/// rdashd's datadir on the device.
const DATADIR: &str = "rdashd";

#[derive(Clone, Debug)]
pub struct Config {
    pub fs: Fs,
    pub draw: Draw,
    pub rdashd: PathBuf,
    pub dashd: PathBuf,
    /// rdashd's arguments beyond the fixture's chain (D-IBD-S: `-dbcache=4 -dbbatchsize=20000`).
    pub extra: Vec<String>,
    /// An acknowledgement every this many submitted blocks.
    pub every: usize,
    /// C5 on every point with a marker, and on one in this many of the others.
    pub c5_every: usize,
    /// C6 on every point with a marker, and on one in this many of the points whose index has
    /// stored blocks above the tip (rdashd's recovery connects them: activation flushes).
    pub c6_every: usize,
    /// Check only the points whose index is `shard.0` modulo `shard.1` ((0, 1): all). A shard's
    /// `pre`/`one`/`post` are relative to its previous checked point; `mid` is exact.
    pub shard: (usize, usize),
    /// Check only the points whose index is in this range (a rerun of one window of a campaign:
    /// the same seed on a fresh recording draws its points from a new log, so an index names a
    /// point of the same kind near the same place, not the same state).
    pub points: RangeInclusive<usize>,
    /// The chainstate on a filesystem of its own, mounted on the datadir's `regtest/chainstate`
    /// (BL-509 M-1: the store's directory is a mount root, `lost+found` in it before rdashd
    /// starts): one disk of two partitions, so one log orders both filesystems' writes. No C6.
    pub split_chainstate: bool,
    /// The planted bug the recording arms (`RDASHD_ST3B_PLANT`; `rdashd` a plant build). Every
    /// check runs the same binary with nothing armed: the recording writes the broken order, and
    /// an honest recovery has to cope with what it left (ST3b plan §3.7).
    pub plant: Option<String>,
    /// Check only the torn and split points (a planted bug that only a torn state shows, T18: S5).
    pub torn_only: bool,
    /// Check C0 alone at each point: no node runs, so no C1–C6 (a pass of C0's own over a
    /// recording, ST3b r1).
    pub c0_only: bool,
}

impl Config {
    /// Whether point `i` of the recording, `pt`, is one this campaign checks.
    fn checks(&self, i: usize, pt: &Point) -> bool {
        let (shard, shards) = self.shard;
        i % shards == shard
            && self.points.contains(&i)
            && (!self.torn_only || matches!(pt.kind, PointKind::Torn | PointKind::Split))
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub entries: usize,
    pub points: usize,
    pub by_kind: BTreeMap<String, usize>,
    /// The coverage report: points by flush phase (`pre`, `mid`, `one`, `post`); a double
    /// crash's points as `c6-<phase>`.
    pub phases: BTreeMap<String, usize>,
    pub c5_runs: usize,
    /// Double crashes, and the points their recoveries' logs gave.
    pub c6_runs: usize,
    pub c6_points: usize,
    pub failures: Vec<String>,
}

/// A point's flush phase, from C0's decision keys and the previous point's (PLAN §3.4): `mid`
/// with a marker; else `pre` with the same tip as before, `post` with a new tip after a marker,
/// and `one` with a new tip and no marker since (a flush that fit one batch).
#[derive(Debug, Default)]
pub struct Phases {
    tip: Option<String>,
    marker_since: bool,
}

impl Phases {
    pub fn classify(&mut self, keys: &Keys) -> &'static str {
        let tip = match keys {
            Keys::Marker {
                ..
            } => {
                self.marker_since = true;
                return "mid";
            }
            Keys::Tip(t) => Some(t.clone()),
            Keys::Empty => None,
        };
        let phase = if tip == self.tip {
            "pre"
        } else if self.marker_since {
            "post"
        } else {
            "one"
        };
        self.tip = tip;
        self.marker_since = false;
        phase
    }
}

/// The freshness floor of a point whose state holds every entry before `end`: the last mark it
/// holds. dm-log-writes logs a mark at once, ahead of plain writes not yet flushed, so a mark
/// alone proves nothing; but every acknowledgement is a `gettxoutsetinfo`, whose synced flush puts
/// its FLUSH entry (and the FUA writes before it) ahead of the mark, so a state with the entries
/// before mark `m` holds `m`'s acknowledgement. [`acks_follow_flushes`] checks the FLUSH is there.
/// `acks[k]` is the log index of mark `a<k>`, `positions[k]` the trajectory position that
/// acknowledgement saw.
pub fn floor(
    end: usize,
    acks: &[usize],
    positions: &[usize],
    stop: Option<usize>,
    stopped_at: usize,
) -> Floor {
    if stop.is_some_and(|s| s <= end) {
        return Floor::Stopped(stopped_at);
    }
    match acks.iter().rposition(|&a| a <= end) {
        Some(k) => Floor::Ack(positions[k]),
        None => Floor::Start,
    }
}

/// What the recording left: the log, every acknowledgement's trajectory position and the final
/// one.
struct Recording {
    log: PathBuf,
    positions: Vec<usize>,
    stopped_at: usize,
}

/// The recording's schedule run: rdashd in `<root>/rdashd`, its peer's files in `work`. It must
/// pass its own checks (the baseline's) and split at least one coins flush (ST3b-D9).
fn recorded_run(
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    work: &Path,
    root: &Path,
    mark: baseline::Marker<'_>,
) -> io::Result<baseline::Report> {
    super::devices::private_dir(work)?;
    let report = rt.block_on(baseline::run(
        &cfg.rdashd,
        &cfg.dashd,
        fixture,
        work,
        &root.join(DATADIR),
        0,
        &cfg.extra,
        cfg.plant.as_deref(),
        cfg.every,
        mark,
    ))?;
    if !report.failures.is_empty() {
        return Err(io::Error::other(format!(
            "the recorded run failed its own checks: {}",
            report.failures.join("; ")
        )));
    }
    // A recording whose flushes never split would leave every crash check passing on unsplit
    // flushes only (S0 r1 m-2).
    if report.partial_batches == 0 {
        return Err(io::Error::other(
            "the recorded run wrote no partial coins batch: no flush split (see -dbbatchsize)",
        ));
    }
    Ok(report)
}

/// Records the schedule on `cfg.fs` over log-writes.
fn record(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
) -> io::Result<Recording> {
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
    mount_disk(&mut stack, &dev, &mnt, cfg.fs, cfg.split_chainstate, true)?;
    File::open(&mnt)?.sync_all()?;
    if cfg.split_chainstate {
        // The store's filesystem has a journal of its own: its root's owner must be durable before
        // `start` too, or a point before its first commit replays a store rdashd cannot write.
        File::open(mnt.join(DATADIR).join(CHAINSTATE))?.sync_all()?;
    }
    stack.message(&lw, &["mark", "start"])?;
    let mut mark = |name: &str| stack.message(&lw, &["mark", name]);
    let report = recorded_run(rt, fixture, cfg, &run.dir().join("record"), &mnt, &mut mark)?;
    stack.message(&lw, &["mark", "stop"])?;
    stack.teardown().map_err(io::Error::other)?;
    let stopped_at = *report.acks.last().expect("a final acknowledgement");
    Ok(Recording {
        log,
        positions: report.acks,
        stopped_at,
    })
}

/// The datadir directory [`Config::split_chainstate`] mounts the chainstate's filesystem on.
const CHAINSTATE: &str = "regtest/chainstate";

/// The device-mapper name of partition `n` of the disk mounted at `mnt`, a run's mount point.
fn partition(mnt: &Path, n: u8) -> String {
    let name = |p: &Path| p.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let run = mnt.parent().map(name).unwrap_or_default();
    format!("st3b-{run}-{}p{n}", name(mnt))
}

/// Mounts the disk `dev` at `mnt`: one filesystem, or with `split` two partitions of it, the
/// second on the datadir's [`CHAINSTATE`]. `fresh` formats them first, makes the chainstate's
/// mount point, and gives each root to this user.
fn mount_disk(
    stack: &mut Stack,
    dev: &Path,
    mnt: &Path,
    fs: Fs,
    split: bool,
    fresh: bool,
) -> io::Result<()> {
    let parts = if split {
        let half = sectors(DATA_LEN) / 2;
        let mut parts = Vec::new();
        for (n, offset) in [(1, 0), (2, half)] {
            let table = format!("0 {half} linear {} {offset}", dev.display());
            parts.push(stack.dm_create(&partition(mnt, n), &table)?);
        }
        parts
    } else {
        vec![dev.to_owned()]
    };
    if fresh {
        for part in &parts {
            stack.mkfs(part, fs)?;
        }
    }
    stack.mount(&parts[0], mnt, fs, false)?;
    if fresh {
        stack.chown_mnt(mnt)?;
    }
    if let Some(store_fs) = parts.get(1) {
        let store = mnt.join(DATADIR).join(CHAINSTATE);
        if fresh {
            std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&store)?;
        }
        stack.mount(store_fs, &store, fs, false)?;
        if fresh {
            stack.chown_mnt(&store)?;
        }
    }
    Ok(())
}

/// Mounts `img` at `mnt`, runs `f` on the datadir in it, and unmounts.
fn mounted<T>(
    p: &Priv,
    cfg: &Config,
    img: &Path,
    mnt: &Path,
    f: impl FnOnce(&Path) -> T,
) -> Result<T, String> {
    let mut stack = Stack::new(p);
    let dev = stack.attach(img).map_err(|e| e.to_string())?;
    mount_disk(&mut stack, Path::new(&dev), mnt, cfg.fs, cfg.split_chainstate, false)
        .map_err(|e| format!("mount: {e}"))?;
    let out = f(&mnt.join(DATADIR));
    stack.teardown()?;
    Ok(out)
}

/// Each acknowledgement's durability in the log: a FLUSH entry between mark `a<k>` and the one
/// before it (`start` for `a0`), and between the last and `stop` (the clean stop's own sync, which
/// [`Floor::Stopped`] relies on; ST3b r2 n-2). Without one, [`floor`] would demand state the
/// device may not hold.
pub fn acks_follow_flushes(
    log: &Log,
    start: usize,
    acks: &[usize],
    stop: Option<usize>,
) -> Result<(), String> {
    let mut from = start;
    let marks = acks.iter().enumerate().map(|(k, &a)| (format!("a{k}"), a));
    for (name, a) in marks.chain(stop.map(|s| ("stop".to_owned(), s))) {
        if !log.entries[from..a].iter().any(|e| e.is(FLUSH)) {
            return Err(format!("no FLUSH entry before mark {name} (entries {from}..{a})"));
        }
        from = a;
    }
    Ok(())
}

/// Replays and checks every point; a failing point's images are kept, with `repro.txt`.
fn check(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    rec: &Recording,
) -> io::Result<Report> {
    let log = Log::open(&rec.log).map_err(io::Error::other)?;
    let start = log.mark("start").ok_or_else(|| io::Error::other("no start mark"))?;
    let acks: Vec<usize> =
        (0..rec.positions.len()).map_while(|k| log.mark(&format!("a{k}"))).collect();
    if acks.len() != rec.positions.len() {
        return Err(io::Error::other(format!(
            "{} acknowledgements, {} marks in the log",
            rec.positions.len(),
            acks.len()
        )));
    }
    let stop = log.mark("stop");
    acks_follow_flushes(&log, start, &acks, stop).map_err(io::Error::other)?;
    let pts = points(&log, start, cfg.draw);
    let base = sparse_file(&run.dir().join("replay.img"), DATA_LEN)?;
    let mut replayer = Replayer::new(&log, File::open(&rec.log)?, base);
    let (mnt_c0, mnt_node) = (run.mountpoint("c0")?, run.mountpoint("pt")?);
    let (shard, shards) = cfg.shard;
    let mut report = Report {
        entries: log.entries.len(),
        points: pts.iter().enumerate().filter(|&(i, pt)| cfg.checks(i, pt)).count(),
        ..Report::default()
    };
    let mut phases = Phases::default();
    println!(
        "recorded {} log entries, {} points, {} to check (shard {shard}/{shards}, points {:?})",
        log.entries.len(),
        pts.len(),
        report.points,
        cfg.points
    );
    let started = std::time::Instant::now();
    let mnts = (mnt_c0.as_path(), mnt_node.as_path());
    for (i, pt) in
        replay_order(&pts).into_iter().map(|i| (i, &pts[i])).filter(|&(i, pt)| cfg.checks(i, pt))
    {
        *report.by_kind.entry(pt.kind.to_string()).or_default() += 1;
        let floor = floor(window_end(&log, pt), &acks, &rec.positions, stop, rec.stopped_at);
        let img = run.dir().join(format!("pt{i}.img"));
        let (img_c0, img_c6) = (img.with_extension("c0.img"), img.with_extension("c6.img"));
        let snap = replayer.snapshot(pt, &img)?;
        drop(reflink(&snap, &img_c0)?);
        // Pristine for C6: C0 and C1 write to theirs.
        drop(reflink(&snap, &img_c6)?);
        drop(snap);
        let seed = cfg.draw.seed ^ i as u64;
        let c5 = |phase: &str| {
            (phase == "mid" || i % cfg.c5_every == 0).then(|| run.dir().join(format!("c5-{i}")))
        };
        let mut checked =
            check_image(p, rt, fixture, cfg, (&img, &img_c0), mnts, floor, seed, &mut phases, c5);
        *report.phases.entry(checked.phase.to_owned()).or_default() += 1;
        report.c5_runs += usize::from(checked.c5_ran);
        let c6 = checked.failures.is_empty()
            && !cfg.split_chainstate
            && !cfg.c0_only
            && (checked.phase == "mid" || (checked.above_tip && i % cfg.c6_every == 0));
        if let (true, Some(tip)) = (c6, checked.tip.clone()) {
            let dc = double_crash(p, run, rt, fixture, cfg, i, &img_c6, mnts, floor, &tip)?;
            report.c6_runs += 1;
            report.c6_points += dc.points;
            for (phase, n) in dc.phases {
                *report.phases.entry(format!("c6-{phase}")).or_default() += n;
            }
            checked.failures.extend(dc.failures.into_iter().map(|f| format!("C6: {f}")));
        }
        let failures = checked.failures;
        // Progress, for the journal: one line per point.
        println!(
            "point {i}/{} {} {} {floor:?}{}: {} ({:.0} s)",
            pts.len(),
            describe(pt),
            checked.phase,
            if c6 {
                " +C6"
            } else {
                ""
            },
            if failures.is_empty() {
                "ok"
            } else {
                "FAILED"
            },
            started.elapsed().as_secs_f64()
        );
        if failures.is_empty() {
            remove_own_file(&img)?;
            remove_own_file(&img_c0)?;
            remove_own_file(&img_c6)?;
        } else {
            let what = describe(pt);
            repro(run, &rec.log, cfg, i, &what, floor, &failures)?;
            report.failures.extend(failures.iter().map(|f| format!("point {i} ({what}): {f}")));
        }
    }
    // The reader's replay of the whole log is the recorded device, byte for byte (ST3b r1 m-4).
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

/// What [`check_image`] found on one point's image.
struct Checked {
    failures: Vec<String>,
    phase: &'static str,
    /// The tip C1 recovered to.
    tip: Option<String>,
    /// C0's index has the data of a block above the tip's height.
    above_tip: bool,
    c5_ran: bool,
}

/// One point's image `img` (C1–C5 write to it) and its copy for C0 `img_c0`, checked with the
/// mount points `mnts` (C0's, the node's). `c5` gives C5's work directory for the point's phase,
/// or none.
#[allow(clippy::too_many_arguments)]
fn check_image(
    p: &Priv,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    (img, img_c0): (&Path, &Path),
    (mnt_c0, mnt_node): (&Path, &Path),
    floor: Floor,
    seed: u64,
    phases: &mut Phases,
    c5: impl FnOnce(&str) -> Option<PathBuf>,
) -> Checked {
    let mut checked = Checked {
        failures: Vec::new(),
        phase: "-",
        tip: None,
        above_tip: false,
        c5_ran: false,
    };
    let c0 = match mounted(p, cfg, img_c0, mnt_c0, |d| inspect::inspect(d, Some(fixture)))
        .and_then(|r| r)
    {
        Ok(c0) => c0,
        Err(e) => {
            checked.failures.push(format!("C0: {e}"));
            return checked;
        }
    };
    checked.phase = phases.classify(&c0.keys);
    checked.failures.extend(c0.failures.iter().map(|f| format!("C0: {f}")));
    if !checked.failures.is_empty() || cfg.c0_only {
        return checked;
    }
    let height = |h: &str| fixture.rows.get(h).map(|r| r.height);
    let tip_height = match &c0.keys {
        Keys::Tip(t) => height(t),
        Keys::Marker {
            old,
            ..
        } => old.as_deref().and_then(height),
        Keys::Empty => None,
    };
    checked.above_tip = c0.with_data.iter().any(|h| height(h) > tip_height);
    let c5_work = c5(checked.phase);
    let node = mounted(p, cfg, img, mnt_node, |datadir| {
        node_checks(rt, fixture, cfg, datadir, floor, &c0.with_data, seed, c5_work)
    });
    match node {
        Ok(n) => {
            checked.failures.extend(n.failures);
            checked.tip = n.tip;
            checked.c5_ran = n.c5_ran;
        }
        Err(e) => checked.failures.push(e),
    }
    checked
}

/// What a double crash found.
#[derive(Default)]
struct DoubleCrash {
    points: usize,
    phases: BTreeMap<&'static str, usize>,
    failures: Vec<String>,
}

/// C6 at point `i` (PLAN §3.4): the point's pristine image `img` on a second log-writes stack,
/// rdashd's recovery on it with no peers (C1) to a clean stop, then every flush and FUA point of
/// that recovery's log replayed onto copies of `img`. Each must pass C0–C4 with the first crash's
/// `floor`, and recover to the first recovery's tip `tip`, so to its statistics (I4: C2 holds
/// both to the oracle row of the tip). The recovery's directory is kept when a point fails.
#[allow(clippy::too_many_arguments)]
fn double_crash(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    i: usize,
    img: &Path,
    mnts: (&Path, &Path),
    floor: Floor,
    tip: &str,
) -> io::Result<DoubleCrash> {
    let dir = run.dir().join(format!("c6-{i}"));
    super::devices::private_dir(&dir)?;
    let (data, log_path) = (dir.join("data.img"), dir.join("log.img"));
    drop(reflink(&File::open(img)?, &data)?);
    sparse_file(&log_path, LOG_LEN)?;
    let mut out = DoubleCrash::default();
    {
        let mnt = run.mountpoint("dc")?;
        let lw = run.dm("lw2");
        let mut stack = Stack::new(p);
        let data_dev = stack.attach(&data)?;
        let log_dev = stack.attach(&log_path)?;
        let table = format!("0 {} log-writes {data_dev} {log_dev}", sectors(DATA_LEN));
        let dev = stack.dm_create(&lw, &table)?;
        // Mounting replays the filesystem's journal: before `start`, so no point of its own.
        stack.mount(&dev, &mnt, cfg.fs, false)?;
        stack.message(&lw, &["mark", "start"])?;
        let datadir = mnt.join(DATADIR);
        if let Some(node) =
            rt.block_on(checks::c1(&cfg.rdashd, fixture, &datadir, &cfg.extra, &mut out.failures))
        {
            let reached = node
                .rpc_with_timeout(checks::RPC_PATIENCE)
                .map_err(|e| e.to_string())
                .and_then(|rpc| checks::c2(&rpc, fixture, &mut out.failures));
            match reached {
                Ok(t) if t == tip => {}
                Ok(t) => out.failures.push(format!("the recorded recovery reached {t}, not {tip}")),
                Err(e) => out.failures.push(format!("the recorded recovery: {e}")),
            }
            match rt.block_on(node.stop()) {
                Ok(s) if s.is_clean() => {}
                other => out.failures.push(format!("the recorded recovery's stop: {other:?}")),
            }
        }
        stack.teardown().map_err(io::Error::other)?;
    }
    if !out.failures.is_empty() {
        return Ok(out);
    }
    let log = Log::open(&log_path).map_err(io::Error::other)?;
    let start = log.mark("start").ok_or_else(|| io::Error::other("no start mark"))?;
    let draw = Draw {
        seed: cfg.draw.seed,
        ..Draw::default()
    };
    let pts = points(&log, start, draw);
    // The replay advances its base in place: a copy, so `img` stays the pristine repro.
    let base_path = dir.join("base.img");
    let base = reflink(&File::open(img)?, &base_path)?;
    let mut replayer = Replayer::new(&log, File::open(&log_path)?, base);
    let mut phases = Phases::default();
    for (j, pt) in replay_order(&pts).into_iter().map(|j| (j, &pts[j])) {
        let img2 = dir.join(format!("pt{j}.img"));
        let img2_c0 = img2.with_extension("c0.img");
        let snap = replayer.snapshot(pt, &img2)?;
        drop(reflink(&snap, &img2_c0)?);
        drop(snap);
        let seed = cfg.draw.seed ^ (i as u64) << 20 ^ j as u64;
        let checked = check_image(
            p,
            rt,
            fixture,
            cfg,
            (&img2, &img2_c0),
            mnts,
            floor,
            seed,
            &mut phases,
            |_| None,
        );
        *out.phases.entry(checked.phase).or_default() += 1;
        out.points += 1;
        let mut failures = checked.failures;
        if failures.is_empty() && checked.tip.as_deref() != Some(tip) {
            failures
                .push(format!("I4: recovered to {:?}, the first recovery to {tip}", checked.tip));
        }
        if failures.is_empty() {
            remove_own_file(&img2)?;
            remove_own_file(&img2_c0)?;
        } else {
            let what = describe(pt);
            out.failures.extend(failures.iter().map(|f| format!("{j} ({what}): {f}")));
        }
    }
    if let Some(at) = replayer.whole_log_differs(&data)? {
        out.failures.push(format!("the whole log replayed differs from data.img at byte {at}"));
    }
    drop(replayer);
    remove_own_file(&base_path)?;
    if out.failures.is_empty() {
        remove_own_file(&data)?;
        remove_own_file(&log_path)?;
        fs::remove_dir(&dir)?;
    }
    Ok(out)
}

/// C6 alone on a kept first-crash image `img`, point `i` of its campaign with the floor `floor`:
/// C0–C4 on copies for the tip, then [`double_crash`] from a third, `img` itself untouched. A kept
/// C6 failure rerun on a new binary without recording the campaign again (`img` rebuilt from the
/// run's kept log). The seeds are the campaign's for point `i`.
#[allow(clippy::too_many_arguments)]
pub fn double_crash_image(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    img: &Path,
    i: usize,
    floor: Floor,
) -> io::Result<Report> {
    let pt = run.dir().join(format!("pt{i}.img"));
    let (pt_c0, pt_c6) = (pt.with_extension("c0.img"), pt.with_extension("c6.img"));
    let src = File::open(img)?;
    for copy in [&pt, &pt_c0, &pt_c6] {
        drop(reflink(&src, copy)?);
    }
    let (mnt_c0, mnt_node) = (run.mountpoint("c0")?, run.mountpoint("pt")?);
    let mnts = (mnt_c0.as_path(), mnt_node.as_path());
    let seed = cfg.draw.seed ^ i as u64;
    let mut phases = Phases::default();
    let checked =
        check_image(p, rt, fixture, cfg, (&pt, &pt_c0), mnts, floor, seed, &mut phases, |_| None);
    let mut report = Report {
        points: 1,
        failures: checked.failures,
        ..Report::default()
    };
    *report.phases.entry(checked.phase.to_owned()).or_default() += 1;
    if let (true, Some(tip)) = (report.failures.is_empty(), checked.tip) {
        let dc = double_crash(p, run, rt, fixture, cfg, i, &pt_c6, mnts, floor, &tip)?;
        report.c6_runs = 1;
        report.c6_points = dc.points;
        for (phase, n) in dc.phases {
            *report.phases.entry(format!("c6-{phase}")).or_default() += n;
        }
        report.failures.extend(dc.failures.into_iter().map(|f| format!("C6: {f}")));
    }
    if report.failures.is_empty() {
        for copy in [&pt, &pt_c0, &pt_c6] {
            remove_own_file(copy)?;
        }
    }
    Ok(report)
}

/// What [`node_checks`] found.
struct NodeChecks {
    failures: Vec<String>,
    tip: Option<String>,
    c5_ran: bool,
}

/// C1–C4 on the node in `datadir`, then C5 in `c5_work` when it is given and C1–C4 passed.
#[allow(clippy::too_many_arguments)]
fn node_checks(
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
    datadir: &Path,
    floor: Floor,
    stored: &[String],
    seed: u64,
    c5_work: Option<PathBuf>,
) -> NodeChecks {
    let point = rt.block_on(checks::check_point(
        &cfg.rdashd,
        fixture,
        datadir,
        &cfg.extra,
        floor,
        stored,
        seed,
    ));
    let mut failures = point.failures;
    let tip = (!point.tip.is_empty()).then_some(point.tip);
    let (Some(work), Some(pos), true) = (c5_work, point.pos, failures.is_empty()) else {
        return NodeChecks {
            failures,
            tip,
            c5_ran: false,
        };
    };
    let continued = super::devices::private_dir(&work).and_then(|()| {
        rt.block_on(baseline::run(
            &cfg.rdashd,
            &cfg.dashd,
            fixture,
            &work,
            datadir,
            pos,
            &cfg.extra,
            None,
            cfg.every,
            &mut |_| Ok(()),
        ))
    });
    match continued {
        Ok(r) => failures.extend(r.failures.iter().map(|f| format!("C5: {f}"))),
        Err(e) => failures.push(format!("C5: {e}")),
    }
    if failures.is_empty() {
        let _ = fs::remove_dir_all(&work);
    }
    NodeChecks {
        failures,
        tip,
        c5_ran: true,
    }
}

/// The campaign without devices or privileges, in `dir`: the recording's schedule run on plain
/// disk, with its own checks and the partial-batch demand, then its clean stop checked as a point
/// (C0 on a reflinked copy, C1–C4 with the `Stopped` floor, C5). A scenario's arguments and the
/// check path are proved this way before a privileged run, and on hosts without dm targets.
pub fn dry_run(dir: &Path, rt: &Runtime, fixture: &Fixture, cfg: &Config) -> io::Result<Report> {
    let mut marks = Vec::new();
    let mut mark = |name: &str| {
        marks.push(name.to_owned());
        Ok(())
    };
    let rec = recorded_run(rt, fixture, cfg, &dir.join("record"), dir, &mut mark)?;
    let stopped_at = *rec.acks.last().expect("a final acknowledgement");
    let mut report = Report {
        points: 1,
        ..Report::default()
    };
    *report.by_kind.entry("stop".to_owned()).or_default() += 1;
    let want: Vec<String> = (0..rec.acks.len()).map(|k| format!("a{k}")).collect();
    if marks != want {
        report.failures.push(format!("marks {marks:?}, one per acknowledgement expected"));
    }
    // C0 opens a copy: the open replays the WAL, which writes.
    let copy = dir.join("c0");
    super::devices::private_dir(&copy)?;
    let copied = Command::new("cp")
        .args(["-a", "--reflink=always"])
        .arg(dir.join(DATADIR))
        .arg(copy.join(DATADIR))
        .status()?;
    if !copied.success() {
        return Err(io::Error::other("copying the stopped datadir for C0"));
    }
    let c0 = inspect::inspect(&copy.join(DATADIR), Some(fixture)).map_err(io::Error::other)?;
    // One point, so no phase: `classify` needs the point before it.
    report.failures.extend(c0.failures.iter().map(|f| format!("C0: {f}")));
    if report.failures.is_empty() {
        let checked = node_checks(
            rt,
            fixture,
            cfg,
            &dir.join(DATADIR),
            Floor::Stopped(stopped_at),
            &c0.with_data,
            cfg.draw.seed,
            Some(dir.join("c5")),
        );
        report.failures.extend(checked.failures);
        report.c5_runs += usize::from(checked.c5_ran);
    }
    Ok(report)
}

fn describe(pt: &Point) -> String {
    let mut out = format!("{} at prefix {}", pt.kind, pt.prefix);
    if !pt.extra.is_empty() {
        let _ = write!(out, " + {} entries", pt.extra.len());
    }
    if let Some(p) = &pt.part {
        let _ = write!(out, " + {} of entry {}'s blocks", p.blocks.len(), p.entry);
    }
    out
}

/// `pt<i>.repro.txt` beside the kept images (PLAN §3.4 "Pass").
fn repro(
    run: &Run,
    log: &Path,
    cfg: &Config,
    i: usize,
    what: &str,
    floor: Floor,
    failures: &[String],
) -> io::Result<()> {
    let sha = Command::new("sha256sum").arg(log).output()?;
    let sha = String::from_utf8_lossy(&sha.stdout);
    let mut text = String::new();
    let _ = writeln!(text, "run {}", run.id());
    let _ = writeln!(text, "log {} sha256 {}", log.display(), sha.split(' ').next().unwrap_or(""));
    let _ = writeln!(text, "point {i}: {what}, floor {floor:?}");
    let _ = writeln!(text, "fs {} draw {:?} extra {:?}", cfg.fs.name(), cfg.draw, cfg.extra);
    let _ = writeln!(
        text,
        "images pt{i}.img pt{i}.c0.img pt{i}.c6.img (pristine), c6-{i}/ (mount, then st3b-crash \
         check)"
    );
    for f in failures {
        let _ = writeln!(text, "FAIL {f}");
    }
    fs::write(run.dir().join(format!("pt{i}.repro.txt")), text)
}

/// Records, replays and checks; the recording's devices are torn down before the replay. The log
/// is kept when a point fails.
pub fn run(
    p: &Priv,
    run: &Run,
    rt: &Runtime,
    fixture: &Fixture,
    cfg: &Config,
) -> io::Result<Report> {
    let rec = record(p, run, rt, fixture, cfg)?;
    let report = check(p, run, rt, fixture, cfg, &rec)?;
    if report.failures.is_empty() {
        remove_own_file(&rec.log)?;
        let _ = fs::remove_dir_all(run.dir().join("record"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_acknowledgement_needs_a_flush_since_the_last_mark() {
        use crate::crash::logwrites::{Entry, MARK};
        let entry = |flags, mark: Option<&str>| Entry {
            sector: 0,
            nr_sectors: 0,
            flags,
            data_offset: 0,
            mark: mark.map(str::to_owned),
        };
        // start, flush, a0, write, flush, a1, flush, stop.
        let mut log = Log {
            sector_size: 512,
            entries: vec![
                entry(MARK, Some("start")),
                entry(FLUSH, None),
                entry(MARK, Some("a0")),
                entry(0, None),
                entry(FLUSH, None),
                entry(MARK, Some("a1")),
                entry(FLUSH, None),
                entry(MARK, Some("stop")),
            ],
        };
        let (acks, stop) = ([3, 6], Some(8));
        assert_eq!(acks_follow_flushes(&log, 1, &acks, stop), Ok(()));
        log.entries[6].flags = 0;
        assert!(acks_follow_flushes(&log, 1, &acks, stop).unwrap_err().contains("stop"));
        // A recording that never stopped has no stop to check.
        assert_eq!(acks_follow_flushes(&log, 1, &acks, None), Ok(()));
        log.entries[6].flags = FLUSH;
        log.entries[4].flags = 0;
        assert!(acks_follow_flushes(&log, 1, &acks, stop).unwrap_err().contains("a1"));
        log.entries[4].flags = FLUSH;
        log.entries[1].flags = 0;
        assert!(acks_follow_flushes(&log, 1, &acks, stop).unwrap_err().contains("a0"));
    }

    #[test]
    fn torn_only_checks_the_torn_points_of_the_shard_and_window() {
        let mut cfg = Config {
            fs: Fs::Ext4,
            draw: Draw::default(),
            rdashd: PathBuf::new(),
            dashd: PathBuf::new(),
            extra: vec![],
            every: 100,
            c5_every: 1,
            c6_every: 1,
            shard: (0, 2),
            points: 0..=4,
            split_chainstate: false,
            plant: None,
            torn_only: false,
            c0_only: false,
        };
        let pt = |kind| Point {
            kind,
            prefix: 0,
            extra: vec![],
            part: None,
        };
        let (flush, torn, split) =
            (pt(PointKind::Flush), pt(PointKind::Torn), pt(PointKind::Split));
        assert!(cfg.checks(0, &flush) && cfg.checks(0, &torn));
        cfg.torn_only = true;
        assert!(!cfg.checks(0, &flush));
        assert!(cfg.checks(2, &torn) && cfg.checks(2, &split));
        // The shard and the window still apply.
        assert!(!cfg.checks(1, &torn) && !cfg.checks(6, &torn));
    }

    #[test]
    fn a_partition_is_named_for_its_run_and_mount_point() {
        let mnt = Path::new("/work/scratch/st3b/mnt/1793280000-42/c0");
        let name = partition(mnt, 2);
        assert_eq!(name, "st3b-1793280000-42-c0p2");
        // The leak sweep finds its run from the name.
        let line = format!("dm {name}");
        assert_eq!(super::super::devices::leak_run(&line), Some("1793280000-42"));
    }

    #[test]
    fn phases_follow_the_decision_keys() {
        let tip = |t: &str| Keys::Tip(t.to_owned());
        let marker = Keys::Marker {
            new: "b".to_owned(),
            old: Some("a".to_owned()),
        };
        let mut p = Phases::default();
        let seen: Vec<_> =
            [Keys::Empty, Keys::Empty, tip("a"), tip("a"), marker, tip("b"), tip("c")]
                .iter()
                .map(|k| p.classify(k))
                .collect();
        assert_eq!(seen, ["pre", "pre", "one", "pre", "mid", "post", "one"]);
    }

    #[test]
    fn the_floor_is_the_last_mark_the_state_holds() {
        // a0 at entry 10 saw position 3, a1 at 20 saw 7; stop at 30, after the tip at 9.
        let (acks, positions) = ([10, 20], [3, 7]);
        assert_eq!(floor(9, &acks, &positions, Some(30), 9), Floor::Start);
        assert_eq!(floor(10, &acks, &positions, Some(30), 9), Floor::Ack(3));
        assert_eq!(floor(29, &acks, &positions, Some(30), 9), Floor::Ack(7));
        assert_eq!(floor(30, &acks, &positions, Some(30), 9), Floor::Stopped(9));
        assert_eq!(floor(30, &acks, &positions, None, 9), Floor::Ack(7));
    }
}
