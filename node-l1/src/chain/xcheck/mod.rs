//! The X-check (O2b-4 PLAN §5, O2b §8): does the shim session behave as a real dashd? Sampled
//! programs run on the session (with `NodeSut` beside it, for the coins' MuHash) and then, op by
//! op over RPC, on two dashds: stock v24.0.0, the reference for replies and state, and the
//! published hooked build, whose O8 logs give the C/R/D and A records. Every disagreement is
//! kept with its disposition; none is resolved by choosing the binary that agrees.
//!
//! Each dashd runs on a disposable copy of its schedule's base datadir (the image's blocks, spork
//! 19 on, then `stop`), started with the schedule's arguments and `-mocktime` at the program's
//! clock, its `mnsync` asserted not synced and its startup import awaited before the first op.

pub mod compare;
pub mod evidence;
pub mod opmap;
pub mod sample;
pub mod spork;

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use dash_oracle_sys::{ChainOp, ChainReport, Pin, RestartKind};
use dash_statedigest::{Lines, Record};
use serde_json::{Value, json};

use self::compare::{
    Answer, Planes, UtxoPlanes, dashd_answer, dashd_answers_equal, refused, replies_equal,
    restart_answer, session_answer,
};
use self::opmap::Mapped;
use self::sample::{Executed, Sampled, Tally};
use crate::chain::core::{SPORK_19, Schedule, T_OLD, T_RECENT, image_op};
use crate::chain::intent::{Encoding, Program};
use crate::chain::run::{End, Mode, Runner, Traced};
use crate::chain::step::Step;
use crate::l3::dashd::{Process, Refusal, Rpc, Started, is_synced, log_messages};

/// Which dashd a binary is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// Stock v24.0.0: the reference for replies, state, load refusals and the spork experiment.
    Stock,
    /// The published O8-patched, hooked build at the pinned shim: also the C/R/D and A records.
    Hooked,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Stock => "stock",
            Kind::Hooked => "hooked",
        }
    }
}

/// A dashd binary and its provenance.
#[derive(Clone, Debug)]
pub struct Binary {
    pub path: PathBuf,
    pub kind: Kind,
    pub sha256: String,
    /// `--version`'s first line.
    pub version: String,
    /// The hooked build's `build-info` beside it, by sha256.
    pub build_info_sha256: Option<String>,
    /// Its `pin`, `branch` and `commit` lines.
    pub provenance: Vec<String>,
}

impl Binary {
    /// `path`, hashed and asked its version. The hooked build must have its `build-info`.
    pub fn new(path: &Path, kind: Kind) -> Result<Binary, String> {
        let output = Command::new(path)
            .arg("--version")
            .output()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let version =
            String::from_utf8_lossy(&output.stdout).lines().next().unwrap_or_default().to_owned();
        let build_info = path.with_file_name("build-info");
        let (build_info_sha256, provenance) = match kind {
            Kind::Stock => (None, Vec::new()),
            Kind::Hooked => {
                let text = std::fs::read_to_string(&build_info)
                    .map_err(|e| format!("{}: {e}", build_info.display()))?;
                let keep =
                    |l: &&str| ["pin ", "branch ", "commit "].iter().any(|k| l.starts_with(k));
                let provenance = text.lines().filter(keep).map(str::to_owned).collect();
                (Some(sha256_file(&build_info)?), provenance)
            }
        };
        Ok(Binary {
            path: path.to_owned(),
            kind,
            sha256: sha256_file(path)?,
            version,
            build_info_sha256,
            provenance,
        })
    }
}

/// A file's sha256, lowercase hex.
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use bitcoin_hashes::{HashEngine, sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut engine = sha256::HashEngine::default();
    let mut buf = vec![0; 1 << 20];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        engine.input(&buf[..n]);
    }
    Ok(opmap::hex(&sha256::Hash::from_engine(engine).to_byte_array()))
}

/// The X-check's settings.
pub struct Config {
    /// The run directory's parent (Q10: `/work/scratch/l3/o2b4/xcheck/` by default).
    pub root: PathBuf,
    pub stock: Binary,
    pub hooked: Binary,
    pub schedules: Vec<Schedule>,
    /// dashds running at once (at most 4).
    pub concurrency: usize,
    pub minimums: sample::Minimums,
    /// Generated draws per schedule before the sample gives up.
    pub max_draws: u64,
}

/// The most dashds at once (PLAN §5.2).
pub const MAX_CONCURRENCY: usize = 4;

/// A run directory under `root`, 0700 whatever the umask: `<root>/<name>/`.
pub fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// `from` copied into `to` (a new directory), files and directories only.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    private_dir(to)?;
    for entry in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", from.display()))?;
        let kind = entry.file_type().map_err(|e| format!("{}: {e}", entry.path().display()))?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Where a datadir's O8 logs go, outside the network's directory.
fn o8_dir(datadir: &Path) -> PathBuf {
    datadir.join("o8")
}

/// The hooked build's O8 arguments (O8 §2), into [`o8_dir`].
fn o8_args(datadir: &Path) -> Vec<String> {
    let dir = o8_dir(datadir);
    vec![
        format!("-statedigestlog={}", dir.join("digest.jsonl").display()),
        format!("-recordstatedecisions={}", dir.join("decisions.jsonl").display()),
    ]
}

/// The O8 logs of one dashd and how far they were read.
struct O8Logs {
    files: [(PathBuf, u64); 2],
}

impl O8Logs {
    fn new(datadir: &Path) -> O8Logs {
        let dir = o8_dir(datadir);
        O8Logs {
            files: [(dir.join("digest.jsonl"), 0), (dir.join("decisions.jsonl"), 0)],
        }
    }

    /// The records both logs gained since the last read, through their last complete line.
    fn take(&mut self) -> Result<Vec<Record>, String> {
        let mut out = Vec::new();
        for (path, offset) in &mut self.files {
            let bytes = match std::fs::read(&*path) {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            };
            let start = usize::try_from(*offset).expect("a file offset");
            let new = bytes.get(start..).unwrap_or_default();
            let end = new.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
            for (line, record) in Lines::new(&new[..end]) {
                out.push(record.map_err(|e| format!("{} line +{line}: {e:?}", path.display()))?);
            }
            *offset += end as u64;
        }
        Ok(out)
    }
}

/// One dashd under the X-check.
pub struct Dashd<'a> {
    binary: &'a Binary,
    datadir: PathBuf,
    args: Vec<String>,
    process: Option<Process>,
    /// The program's clock: each start's `-mocktime`.
    clock: i64,
    logs: Option<O8Logs>,
    /// The last start's arguments.
    argv: Vec<String>,
}

/// Governance validation off on every X-check dashd (OD-L3-113).
pub const DISABLE_GOVERNANCE: &str = "-disablegovernance=1";

/// `debug.log` never shrunk at a start: the start guards and refusals read it from the offset
/// it had before the start, which a shrink would leave past its end.
// Core: src/logging.cpp:514-530 at v24.0.0
pub const KEEP_DEBUG_LOG: &str = "-shrinkdebugfile=0";

/// The `InitWarning` a node started with [`DISABLE_GOVERNANCE`] logs.
// Core: src/init.cpp:1440-1445, src/node/interface_ui.cpp:93-96, src/noui.cpp:31-33 at v24.0.0
const GOVERNANCE_DISABLED: &str = "Warning: You are starting with governance validation disabled.";

/// The guard on every start: the lines `log` gained from `from` on hold `GOVERNANCE_DISABLED`,
/// else an error naming the binary (a renamed or ignored flag would compare `gov_valid` wrong).
pub fn governance_disabled(kind: Kind, log: &Path, from: u64) -> Result<(), String> {
    if log_messages(log, from).iter().any(|l| l == GOVERNANCE_DISABLED) {
        Ok(())
    } else {
        Err(format!(
            "{} dashd did not warn that governance validation is disabled ({DISABLE_GOVERNANCE} \
             ignored?)",
            kind.name()
        ))
    }
}

impl<'a> Dashd<'a> {
    pub fn new(binary: &'a Binary, datadir: &Path, schedule: Schedule, clock: i64) -> Dashd<'a> {
        let mut args: Vec<String> = schedule.args().into_iter().map(str::to_owned).collect();
        // The session builds no governance manager, so its `gov_valid` is false (OD-L3-113).
        args.push(DISABLE_GOVERNANCE.to_owned());
        args.push(KEEP_DEBUG_LOG.to_owned());
        let logs = match binary.kind {
            Kind::Stock => None,
            Kind::Hooked => {
                args.extend(o8_args(datadir));
                Some(O8Logs::new(datadir))
            }
        };
        Dashd {
            binary,
            datadir: datadir.to_owned(),
            args,
            process: None,
            clock,
            logs,
            argv: Vec::new(),
        }
    }

    /// This dashd without the start argument `arg`: for tests that check a start guard fails
    /// closed.
    #[doc(hidden)]
    pub fn without_arg(mut self, arg: &str) -> Dashd<'a> {
        self.args.retain(|a| a != arg);
        self
    }

    /// The last start's arguments, its datadir, ports and clock as placeholders: what the
    /// evidence records.
    pub fn arguments(&self) -> Vec<String> {
        let datadir = self.datadir.display().to_string();
        self.argv
            .iter()
            .map(|a| {
                let a = a.replace(&datadir, "<datadir>");
                match a.split_once('=') {
                    Some(("-port" | "-rpcport", _)) => {
                        format!("{}=<port>", a.split('=').next().unwrap_or_default())
                    }
                    Some(("-mocktime", _)) => "-mocktime=<clock>".to_owned(),
                    _ => a,
                }
            })
            .collect()
    }

    /// A start at the program's clock: `None` once the node answers, has warned that governance
    /// validation is disabled, reports `mnsync` not synced and has finished its startup import;
    /// or what refused it.
    pub fn start(&mut self) -> Result<Option<Refusal>, String> {
        if self.logs.is_some() {
            private_dir(&o8_dir(&self.datadir))?;
        }
        let log = self.debug_log();
        let from = std::fs::metadata(&log).map_or(0, |m| m.len());
        match Process::start_once(&self.binary.path, &self.datadir, &self.args, self.clock)? {
            Started::Refused(refusal) => Ok(Some(refusal)),
            Started::Up(process) => {
                governance_disabled(self.binary.kind, &log, from)?;
                self.argv = process.argv().to_vec();
                let status = process.rpc().ok("mnsync", json!(["status"]))?;
                if is_synced(&status)? {
                    return Err("mnsync reports synced on a node with no peers".into());
                }
                process.wait_loaded()?;
                self.process = Some(process);
                Ok(None)
            }
        }
    }

    pub fn rpc(&self) -> Result<&Rpc, String> {
        self.process.as_ref().map(Process::rpc).ok_or_else(|| "dashd is not running".into())
    }

    pub fn up(&self) -> bool {
        self.process.is_some()
    }

    /// The state planes ([`Planes::dashd`]).
    pub fn planes(&self) -> Result<Planes, String> {
        let rpc = self.rpc()?;
        Planes::dashd(
            &rpc.ok("getblockchaininfo", json!([]))?,
            &rpc.ok("getchaintips", json!([]))?,
            &rpc.ok("spork", json!(["show"]))?,
            &rpc.ok("spork", json!(["active"]))?,
        )
    }

    /// `op` as `mapped` says: its answer. A restart that refuses the load leaves dashd down.
    pub fn run(&mut self, op: &ChainOp, mapped: &Mapped) -> Result<Answer, String> {
        match mapped {
            Mapped::Rpc {
                method,
                params,
            } => {
                let answer = dashd_answer(self.rpc()?.call(method, params.clone())?);
                if let ChainOp::SetTime(time) = op {
                    self.clock = *time;
                }
                Ok(answer)
            }
            Mapped::Flush => {
                Ok(dashd_answer(self.rpc()?.call("gettxoutsetinfo", json!(["muhash"]))?))
            }
            Mapped::Spork {
                name,
                value,
            } => Ok(dashd_answer(self.rpc()?.call("sporkupdate", json!([name, value]))?)),
            Mapped::Restart(kind) => {
                let process = self.process.take().ok_or("dashd is not running")?;
                match kind {
                    RestartKind::Clean => process.stop()?,
                    RestartKind::Kill => process.kill()?,
                }
                Ok(restart_answer(self.start()?.as_ref()))
            }
            Mapped::Excluded(why) => Err(format!("an excluded op ({why}) reached dashd")),
        }
    }

    /// The O8 records since the last read; `None` for the stock build.
    pub fn records(&mut self) -> Result<Option<Vec<Record>>, String> {
        self.logs.as_mut().map(O8Logs::take).transpose()
    }

    /// `stop`, if running.
    pub fn stop(&mut self) -> Result<(), String> {
        match self.process.take() {
            Some(process) => process.stop(),
            None => Ok(()),
        }
    }

    /// The node's `debug.log`.
    pub fn debug_log(&self) -> PathBuf {
        self.datadir.join("regtest").join("debug.log")
    }
}

/// A schedule's base datadir for `binary`: the image's blocks as the session's image holds them
/// ([`image_op`]), spork 19 turned on, then `stop` (PLAN §5.2). Its planes, before the stop, and
/// its dashd's arguments ([`Dashd::arguments`]).
pub fn build_base(
    binary: &Binary,
    dir: &Path,
    schedule: Schedule,
) -> Result<(Planes, Vec<String>), String> {
    private_dir(dir)?;
    let mut node = Dashd::new(binary, dir, schedule, T_RECENT);
    if let Some(error) = node.start()? {
        return Err(format!("the base's first start was refused: {error:?}"));
    }
    for b in &schedule.world().blocks[1..] {
        let op = image_op(b);
        let answer = node.run(&op, &opmap::map(&op, true))?;
        if answer != Ok(Value::Null) {
            return Err(format!("image block {} on {}: {answer:?}", b.height, binary.kind.name()));
        }
    }
    let spork = ChainOp::Spork {
        id: SPORK_19,
        value: 0,
    };
    let answer = node.run(&spork, &opmap::map(&spork, true))?;
    if answer != Ok(Value::String("success".into())) {
        return Err(format!("spork 19 on: {answer:?}"));
    }
    let planes = node.planes()?;
    let arguments = node.arguments();
    node.stop()?;
    let _ = std::fs::remove_dir_all(o8_dir(dir));
    Ok((planes, arguments))
}

/// The session's `A` records in a sampled run that answered `sb_required` true. This is 0 by
/// construction, not a measurement: superblock validation is required only on a synced node,
/// and no X-check dashd is ever synced ([`Dashd::start`] fails when `mnsync` says so). The
/// superblock payment checks are uncovered with governance on or off; `-disablegovernance=1`
/// moves only `gov_valid`, which is still compared.
// Core: src/masternode/payments.cpp:355-361, 404-422 at v24.0.0; shim src/evo/chainhelper.cpp:48-51
pub fn sb_required(run: &SessionRun) -> u64 {
    let records = run.restore.records.iter().chain(run.trace.iter().flat_map(|t| &t.step.records));
    let required = |r: &&Record| match r {
        Record::Attempt(a) => a.q.iter().any(|q| {
            matches!(
                q,
                dash_statedigest::Query::SbRequired {
                    answer: true,
                    ..
                }
            )
        }),
        _ => false,
    };
    records.filter(required).count() as u64
}

/// What the session ran for one sampled program.
#[derive(Clone, Debug)]
pub struct SessionRun {
    pub old: bool,
    /// Step 0: the image restored at the program's clock.
    pub restore: Step,
    pub start_muhash: Option<[u8; 32]>,
    pub trace: Vec<Traced>,
    /// How the run ended (one of [`sample::END_KINDS`]).
    pub end: &'static str,
}

/// What a dashd showed at one step, for the stock-against-hooked comparison.
#[derive(Clone, Debug, PartialEq)]
pub struct Observed {
    pub op: &'static str,
    pub answer: Option<Answer>,
    pub planes: Option<Planes>,
    pub utxo: Option<UtxoPlanes>,
}

/// A difference the X-check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disagreement {
    pub schedule: &'static str,
    pub program: String,
    /// `session-stock`, `session-hooked` or `stock-hooked`.
    pub pair: &'static str,
    /// The step: 0 the restore, then one per op; `end` past the last.
    pub step: String,
    pub op: &'static str,
    pub plane: &'static str,
    /// Why it is accepted, from [`DISPOSITIONS`]; none fails the run.
    pub disposition: Option<&'static str>,
    /// What differed, for triage: test output only, never the evidence (DEC-1368).
    pub detail: String,
}

/// One plane's comparison: its name, and `None` when equal or what differed.
type Check = (&'static str, Option<String>);

fn check(plane: &'static str, equal: bool) -> Check {
    (plane, (!equal).then(String::new))
}

/// Declared differences: `(pair, op, plane, disposition)`. A disagreement matching none is
/// undisposed, and the run fails. Each row needs L3's ruling (a declared session difference
/// with Sol's sign-off, O2b §8).
pub const DISPOSITIONS: &[(&str, &str, &str, &str)] = &[];

fn disposition(pair: &str, op: &str, plane: &str) -> Option<&'static str> {
    DISPOSITIONS
        .iter()
        .find(|(p, o, pl, _)| *p == pair && *o == op && *pl == plane)
        .map(|(_, _, _, why)| *why)
}

/// Equal and differing comparisons, per pair and plane.
pub type Agreement = BTreeMap<(&'static str, &'static str), (u64, u64)>;

/// One program on one dashd.
pub struct Replay {
    pub observed: Vec<Observed>,
    pub agreement: Agreement,
    pub disagreements: Vec<Disagreement>,
}

/// Runs a session program's ops on `binary` from a copy of `base` at `work`, comparing each with
/// the session's step; `work` is removed after, its `debug.log` kept in `log` on a
/// disagreement.
pub fn replay(
    binary: &Binary,
    base: &Path,
    work: &Path,
    log: &Path,
    schedule: Schedule,
    label: &str,
    run: &SessionRun,
) -> Result<Replay, String> {
    copy_tree(base, work)?;
    let pair = match binary.kind {
        Kind::Stock => "session-stock",
        Kind::Hooked => "session-hooked",
    };
    let mut out = Replay {
        observed: Vec::new(),
        agreement: Agreement::new(),
        disagreements: Vec::new(),
    };
    let clock = if run.old {
        T_OLD
    } else {
        T_RECENT
    };
    let mut node = Dashd::new(binary, work, schedule, clock);
    let result = replay_ops(&mut node, schedule, label, pair, run, &mut out);
    let stopped = node.stop();
    if !out.disagreements.is_empty() || result.is_err() {
        private_dir(log)?;
        let name = log_name(schedule, label, &format!("{}.log", binary.kind.name()));
        let _ = std::fs::copy(node.debug_log(), log.join(name));
    }
    std::fs::remove_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))?;
    result?;
    stopped?;
    Ok(out)
}

fn replay_ops(
    node: &mut Dashd<'_>,
    schedule: Schedule,
    label: &str,
    pair: &'static str,
    run: &SessionRun,
    out: &mut Replay,
) -> Result<(), String> {
    let mut see = |step: String, op: &'static str, planes: Vec<Check>| {
        for (plane, differ) in planes {
            let e = out.agreement.entry((pair, plane)).or_default();
            if let Some(detail) = differ {
                e.1 += 1;
                out.disagreements.push(Disagreement {
                    schedule: schedule.name(),
                    program: label.to_owned(),
                    pair,
                    step: step.clone(),
                    op,
                    plane,
                    disposition: disposition(pair, op, plane),
                    detail,
                });
            } else {
                e.0 += 1;
            }
        }
    };
    if let Some(error) = node.start()? {
        see("0".into(), "restore", vec![("load", Some(format!("{error:?}")))]);
        out.observed.push(Observed {
            op: "restore",
            answer: Some(restart_answer(Some(&error))),
            planes: None,
            utxo: None,
        });
        return Ok(());
    }
    let planes = node.planes()?;
    let mut checks = plane_checks(Planes::session(&run.restore.post).as_ref(), Some(&planes));
    if let Some(records) = node.records()? {
        checks.extend(records_checks(&run.restore.records, &records));
    }
    see("0".into(), "restore", checks);
    out.observed.push(Observed {
        op: "restore",
        answer: None,
        planes: Some(planes),
        utxo: None,
    });
    let mut last_post = &run.restore.post;
    let mut last_muhash = run.start_muhash;
    for (i, t) in run.trace.iter().enumerate() {
        let kind = crate::chain::run::op_kind(&t.op);
        let mapped = opmap::map(&t.op, crate::chain::generate::SPORKS_ADMITTED);
        let answer = node.run(&t.op, &mapped)?;
        let session = session_answer(&t.step.reply.outcome);
        let reply = replies_equal(&mapped, &session, &answer);
        let mut checks = vec![("reply", (!reply).then(|| format!("{session:?} / {answer:?}")))];
        let ended = matches!(mapped, Mapped::Restart(_)) && (refused(&session) || refused(&answer));
        let planes = if node.up() {
            Some(node.planes()?)
        } else {
            None
        };
        if !ended {
            checks.extend(plane_checks(Planes::session(&t.step.post).as_ref(), planes.as_ref()));
        }
        let mut utxo = None;
        if mapped == Mapped::Flush {
            let dashd = answer.as_ref().ok().map(UtxoPlanes::dashd).transpose()?;
            let session = UtxoPlanes::session(&t.step.post, t.muhash);
            checks.extend(utxo_checks(session.as_ref(), dashd.as_ref()));
            utxo = dashd;
        }
        if let Some(records) = node.records()? {
            checks.extend(records_checks(&t.step.records, &records));
        }
        see((i + 1).to_string(), kind, checks);
        out.observed.push(Observed {
            op: kind,
            answer: Some(answer),
            planes,
            utxo,
        });
        if ended {
            return Ok(());
        }
        last_post = &t.step.post;
        last_muhash = t.muhash;
    }
    // The end: `gettxoutsetinfo "muhash"` (its flush is acceptable after the last op).
    let info = node.rpc()?.ok("gettxoutsetinfo", json!(["muhash"]))?;
    let dashd = UtxoPlanes::dashd(&info)?;
    let session = UtxoPlanes::session(last_post, last_muhash);
    see("end".into(), "end", utxo_checks(session.as_ref(), Some(&dashd)));
    out.observed.push(Observed {
        op: "end",
        answer: None,
        planes: None,
        utxo: Some(dashd),
    });
    Ok(())
}

/// The state planes, each equal or not; a missing side (a down session against a running dashd,
/// or the other way) differs in every plane.
fn plane_checks(session: Option<&Planes>, dashd: Option<&Planes>) -> Vec<Check> {
    const PLANES: [&str; 5] = ["tip", "best_header", "ibd", "tips", "sporks"];
    match (session, dashd) {
        (Some(s), Some(d)) => {
            let differ = d.differences(s);
            PLANES.iter().map(|p| check(p, !differ.contains(p))).collect()
        }
        (None, None) => Vec::new(),
        _ => vec![check("down", false)],
    }
}

fn utxo_checks(session: Option<&UtxoPlanes>, dashd: Option<&UtxoPlanes>) -> Vec<Check> {
    const PLANES: [&str; 4] = ["utxo.txouts", "utxo.total_amount", "utxo.bestblock", "utxo.muhash"];
    match (session, dashd) {
        (Some(s), Some(d)) => {
            let differ = d.differences(s);
            PLANES
                .iter()
                .filter(|p| **p != "utxo.muhash" || s.muhash.is_some())
                .map(|p| check(p, !differ.contains(p)))
                .collect()
        }
        _ => vec![check("utxo", false)],
    }
}

fn records_checks(session: &[Record], dashd: &[Record]) -> Vec<Check> {
    let differ = crate::chain::compare::records_planes(session, dashd, &mut Default::default());
    let mut checks: Vec<Check> = ["records", "attempts", "sparse records"]
        .iter()
        .map(|p| check(p, !differ.contains(p)))
        .collect();
    if let Some(diff) = crate::chain::compare::attempts_diff(session, dashd) {
        checks[1].1 = Some(format!("{diff:?}: {} / {}", queries(session), queries(dashd)));
    }
    checks
}

/// The attempts' queries by kind and answer, without hashes or txids: for a disagreement's
/// detail.
fn queries(records: &[Record]) -> String {
    use dash_statedigest::Query;
    let attempts = records.iter().filter_map(|r| match r {
        Record::Attempt(a) => Some(a),
        _ => None,
    });
    let shape = |q: &Query| match q {
        Query::ClConflict {
            answer,
            ..
        } => format!("cl_conflict={answer}"),
        Query::ClBest(best) => format!("cl_best={}", best.is_some()),
        Query::Ibd(ibd) => format!("ibd={ibd}"),
        Query::ClHas {
            answer,
            ..
        } => format!("cl_has={answer}"),
        Query::IsConflict {
            conflict,
            ..
        } => format!("is_conflict={}", conflict.is_some()),
        Query::SbRequired {
            height,
            answer,
        } => format!("sb_required@{height}={answer}"),
        Query::GovValid(valid) => format!("gov_valid={valid}"),
        Query::SbTriggered {
            height,
            answer,
        } => format!("sb_triggered@{height}={answer}"),
        Query::SbPayments {
            height,
            payments,
        } => format!("sb_payments@{height}={:?}", payments.as_ref().map(Vec::len)),
    };
    let lists: Vec<String> =
        attempts.map(|a| a.q.iter().map(shape).collect::<Vec<_>>().join(",")).collect();
    format!("[{}]", lists.join("] ["))
}

/// The stock build's steps against the hooked build's, on their shared program: the outcome
/// planes (replies, state and UTXO planes), never the records.
pub fn compare_binaries(
    schedule: Schedule,
    label: &str,
    stock: &[Observed],
    hooked: &[Observed],
    agreement: &mut Agreement,
) -> Vec<Disagreement> {
    let pair = "stock-hooked";
    let mut out = Vec::new();
    let n = stock.len().max(hooked.len());
    for i in 0..n {
        let (s, h) = (stock.get(i), hooked.get(i));
        let op = s.or(h).map_or("?", |o| o.op);
        let checks = match (s, h) {
            (Some(s), Some(h)) => vec![
                ("reply", dashd_answers_equal(op, s.answer.as_ref(), h.answer.as_ref())),
                ("state", s.planes == h.planes),
                ("utxo", s.utxo == h.utxo),
            ],
            _ => vec![("steps", false)],
        };
        for (plane, equal) in checks {
            let detail = || format!("stock {:?}\nhooked {:?}", s, h);
            let e = agreement.entry((pair, plane)).or_default();
            if equal {
                e.0 += 1;
            } else {
                e.1 += 1;
                out.push(Disagreement {
                    schedule: schedule.name(),
                    program: label.to_owned(),
                    pair,
                    step: if op == "end" {
                        "end".into()
                    } else {
                        i.to_string()
                    },
                    op,
                    plane,
                    disposition: disposition(pair, op, plane),
                    detail: detail(),
                });
            }
        }
    }
    out
}

/// A session program for the sampler: `Ran` with its trace and contexts, unless a tie taint
/// fired or an op it ran has no dashd counterpart.
pub fn execute(runner: &mut Runner, program: &Program) -> Result<Executed<SessionRun>, String> {
    let run = runner.run(program).map_err(|e| format!("the session: {}", e.0))?;
    if matches!(run.end, End::Tainted(..)) {
        return Ok(Executed::Tainted);
    }
    for t in &run.trace {
        if let Mapped::Excluded(why) = opmap::map(&t.op, crate::chain::generate::SPORKS_ADMITTED) {
            return Ok(Executed::Excluded(why));
        }
    }
    let restore: Step = ChainReport::try_decode(run.raws[0].clone())?.into();
    let contexts = sample::contexts(&restore, &run.trace);
    let end = match run.end {
        End::Done => "done",
        End::Unexpected(_) => "unexpected",
        End::Diverged(_) => "diverged",
        End::Unsupported(_) => "unsupported",
        End::Tainted(..) => unreachable!("a tainted run returned above"),
    };
    Ok(Executed::Ran(
        SessionRun {
            old: program.old,
            restore,
            start_muhash: run.start_muhash,
            trace: run.trace,
            end,
        },
        contexts,
    ))
}

/// The public seeds a schedule's sample starts from: the 15 shared ones, and on S2 its own.
pub fn seeds(schedule: Schedule) -> Result<Vec<(String, Program)>, String> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/chain_seeds");
    let mut dirs = vec![root.clone()];
    if schedule == Schedule::S2 {
        dirs.push(root.join("diff_chain_av"));
    }
    let mut out = Vec::new();
    for dir in dirs {
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "prog"))
            .collect();
        names.sort();
        for path in names {
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let program = Program::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let name = path.file_stem().expect("a name").to_string_lossy().into_owned();
            out.push((name, program));
        }
    }
    Ok(out)
}

/// The session's sample on `schedule`, from a Diff runner tracing in `chain-v2`.
pub fn session_sample(
    schedule: Schedule,
    minimums: sample::Minimums,
    max_draws: u64,
) -> Result<(Vec<Sampled<SessionRun>>, Tally), String> {
    let mut runner = Runner::with_mode(Pin::V24_0, schedule, Mode::Diff)
        .map_err(|e| format!("the session: {}", e.0))?;
    runner.encoding = Encoding::V2;
    runner.trace(true);
    let (sampled, mut tally) =
        sample::sample(schedule, &seeds(schedule)?, minimums, max_draws, |p| {
            execute(&mut runner, p)
        })?;
    for s in &sampled {
        *tally.ended.entry(s.run.end).or_default() += 1;
    }
    Ok((sampled, tally))
}

/// Every sampled program on both binaries, at most `concurrency` dashds at once, under `dir`.
pub fn replay_all(
    config: &Config,
    schedule: Schedule,
    dir: &Path,
    bases: &BTreeMap<Kind, PathBuf>,
    sampled: &[Sampled<SessionRun>],
) -> Result<(Agreement, Vec<Disagreement>), String> {
    let jobs: Vec<(usize, &Binary)> =
        (0..sampled.len()).flat_map(|i| [(i, &config.stock), (i, &config.hooked)]).collect();
    let next = AtomicUsize::new(0);
    let results: Mutex<BTreeMap<(usize, Kind), Result<Replay, String>>> = Mutex::default();
    let log = dir.join("log");
    std::thread::scope(|scope| {
        for _ in 0..config.concurrency.clamp(1, MAX_CONCURRENCY) {
            scope.spawn(|| {
                loop {
                    let n = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(i, binary)) = jobs.get(n) else {
                        break;
                    };
                    let s = &sampled[i];
                    let work = dir.join(format!("p-{i}-{}", binary.kind.name()));
                    let result = replay(
                        binary,
                        &bases[&binary.kind],
                        &work,
                        &log,
                        schedule,
                        &s.label,
                        &s.run,
                    );
                    results
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert((i, binary.kind), result);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|p| p.into_inner());
    let mut agreement = Agreement::new();
    let mut disagreements = Vec::new();
    for (i, s) in sampled.iter().enumerate() {
        let mut take = |kind| {
            results
                .remove(&(i, kind))
                .expect("every job ran")
                .map_err(|e| format!("{} on {}: {e}", s.label, Kind::name(kind)))
        };
        let (stock, hooked) = (take(Kind::Stock)?, take(Kind::Hooked)?);
        for r in [&stock, &hooked] {
            for (key, (eq, ne)) in &r.agreement {
                let e = agreement.entry(*key).or_default();
                e.0 += eq;
                e.1 += ne;
            }
            disagreements.extend(r.disagreements.iter().cloned());
        }
        let between =
            compare_binaries(schedule, &s.label, &stock.observed, &hooked.observed, &mut agreement);
        if !between.is_empty() {
            log_binary_differences(&log, schedule, &s.label, &between)?;
        }
        disagreements.extend(between);
    }
    Ok((agreement, disagreements))
}

/// A file name in the run's `log/` for `label` on `schedule`: the schedule first, since seed
/// labels repeat across schedules, then the label with every character but an ASCII letter,
/// digit, `-` or `_` as `_`, then `suffix`.
fn log_name(schedule: Schedule, label: &str, suffix: &str) -> String {
    let safe: String = label
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}-{safe}-{suffix}", schedule.name())
}

/// Both binaries' full observations at each stock-hooked difference of `label`, in the run's
/// `log/` (OD-L3-124): read there, never in the evidence, which keeps no `detail`.
fn log_binary_differences(
    log: &Path,
    schedule: Schedule,
    label: &str,
    found: &[Disagreement],
) -> Result<(), String> {
    private_dir(log)?;
    let path = log.join(log_name(schedule, label, "stock-hooked.txt"));
    let text: String = found
        .iter()
        .map(|d| format!("step {} op {} plane {}\n{}\n\n", d.step, d.op, d.plane, d.detail))
        .collect();
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The run's name: the UTC time and the process id.
pub fn run_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    format!("{secs}-{}", std::process::id())
}

/// The whole X-check under `config.root/<run>/`: the bases, the spork experiment, then each
/// schedule's sample replayed on both binaries. The datadirs are removed after; `log/` stays
/// where a program disagreed. The evidence, whatever it says, and the run directory.
pub fn run(config: &Config) -> Result<(evidence::Evidence, PathBuf), String> {
    let dir = config.root.join(run_name());
    private_dir(&dir)?;
    let provenance = evidence::Provenance::here()?;
    let mut bases: Vec<(Schedule, BTreeMap<Kind, PathBuf>)> = Vec::new();
    let mut dashd_args: Vec<(Schedule, BTreeMap<&'static str, Vec<String>>)> = Vec::new();
    let mut schedules = config.schedules.clone();
    if !schedules.contains(&Schedule::S0) {
        schedules.insert(0, Schedule::S0);
    }
    for &schedule in &schedules {
        let mut by_kind = BTreeMap::new();
        let mut planes = Vec::new();
        let mut arguments = BTreeMap::new();
        for binary in [&config.stock, &config.hooked] {
            let base = dir.join(format!("base-{}-{}", schedule.name(), binary.kind.name()));
            let (p, a) = build_base(binary, &base, schedule)?;
            planes.push(p);
            arguments.insert(binary.kind.name(), a);
            by_kind.insert(binary.kind, base);
        }
        if planes[0] != planes[1] {
            return Err(format!("{}: the bases' planes differ: {planes:?}", schedule.name()));
        }
        bases.push((schedule, by_kind));
        dashd_args.push((schedule, arguments));
    }
    let base = |s: Schedule| &bases.iter().find(|(b, _)| *b == s).expect("built").1;
    let mut spork = spork::session_rows(&dir.join("spork-session"))?;
    spork.extend(spork::dashd_rows(&config.stock, &base(Schedule::S0)[&Kind::Stock], &dir)?);
    let mut counts = Vec::new();
    for &schedule in &config.schedules {
        let (sampled, tally) = session_sample(schedule, config.minimums, config.max_draws)?;
        let (agreement, disagreements) =
            replay_all(config, schedule, &dir, base(schedule), &sampled)?;
        let mut c = evidence::ScheduleCounts {
            schedule: schedule.name(),
            tally,
            agreement,
            disagreements,
            dashd_args: dashd_args
                .iter()
                .find(|(s, _)| *s == schedule)
                .map(|(_, a)| a.clone())
                .unwrap_or_default(),
            sb_required: sampled.iter().map(|s| sb_required(&s.run)).sum(),
            ..Default::default()
        };
        for s in &sampled {
            *c.categories.entry(s.category).or_default() += 1;
            for x in &s.contexts {
                *c.contexts.entry(*x).or_default() += 1;
            }
        }
        c.with_proposal = sampled.iter().filter(|s| !s.contexts.is_empty()).count();
        counts.push(c);
    }
    for (_, by_kind) in &bases {
        for path in by_kind.values() {
            std::fs::remove_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    let session = dir.join("spork-session");
    std::fs::remove_dir_all(&session).map_err(|e| format!("{}: {e}", session.display()))?;
    Ok((
        evidence::Evidence {
            provenance,
            binaries: vec![config.stock.clone(), config.hooked.clone()],
            minimums: config.minimums,
            schedules: counts,
            spork,
        },
        dir,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::{Disagreement, log_binary_differences, log_name};
    use crate::chain::core::Schedule;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// OD-L3-124's recording: a stock-hooked difference's full text goes to the run's `log/`,
    /// one file per schedule and label, so the same label on two schedules keeps both; nothing
    /// is written beside `log/`, so none of it reaches the evidence.
    #[test]
    fn binary_differences_are_logged_per_schedule_outside_the_evidence() {
        let run = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let log = run.path().join("log");
        let label = "basic/augmented";
        for schedule in [Schedule::S0, Schedule::S1] {
            let found = Disagreement {
                schedule: schedule.name(),
                program: label.into(),
                pair: "stock-hooked",
                step: "3".into(),
                op: "flush",
                plane: "reply",
                disposition: None,
                detail: format!("stock {}\nhooked", schedule.name()),
            };
            log_binary_differences(&log, schedule, label, &[found]).unwrap();
        }
        assert_eq!(names(run.path()), ["log"]);
        assert_eq!(
            names(&log),
            ["S0-basic_augmented-stock-hooked.txt", "S1-basic_augmented-stock-hooked.txt"]
        );
        for schedule in ["S0", "S1"] {
            let text = std::fs::read_to_string(
                log.join(format!("{schedule}-basic_augmented-stock-hooked.txt")),
            )
            .unwrap();
            assert_eq!(text, format!("step 3 op flush plane reply\nstock {schedule}\nhooked\n\n"));
        }
        assert_eq!(
            log_name(Schedule::S2, "xcheck/S2/7/augmented", "hooked.log"),
            "S2-xcheck_S2_7_augmented-hooked.log"
        );
        assert_eq!(log_name(Schedule::S0, "../x y", "stock.log"), "S0-___x_y-stock.log");
    }
}
