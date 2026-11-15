//! The warn leg (plan §7.2, D100): a regtest dashd and V on D49's arguments, `-par=1`, building
//! [`LAST`] blocks whose versions signal the unknown bit [`BIT`] in two periods: 107 of 144 (no
//! lock-in), then 108 (lock-in at the threshold). Its planes:
//! - the log plane: UpdateTip's line per connected block on each side, reduced to (height, hash,
//!   warning): exactly one per tip 1..=[`LAST`], equal on both sides, the warning LOCKED_IN's
//!   from tip 431 to 574 and none elsewhere (validation.cpp:3230-3246, UpdateTipLog :3178-3199);
//! - the concise plane at [`CONCISE_AT`]: `getblockchaininfo.warnings` against
//!   `NodeSide::warnings()`, ACTIVE's DoWarning from tip 575 (warnings.cpp's `GetWarnings(false)`).
//!
//! The expected tips come from period arithmetic over the signalling heights ([`state_after`]),
//! not from either side. It is a separate leg because ACTIVE's misc warning persists. Every
//! block passes §3.6's gates on both sides (it is a census leg) and `ibd` is false at every
//! observation. V's UpdateTip lines come from a global `tracing` capture in its own test binary
//! (`tests/l3_warn.rs`: V logs on its own thread).

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::Path;

use dash_oracle_sys::{ChainOp, RpcError};
use serde_json::{Value, json};

use super::dashd::Process;
use super::evo;
use super::leg::{CHAIN_ARGS, T0};
use super::planes::{self, block_at, rpc_hash};
use super::trial::{from_hex, hex, to_hex};
use super::version::{Dashd, private_dir};
use crate::chain::build::BlockPlan;
use crate::chain::mutate::Par;
use crate::chain::node::NodeSide;

/// The unknown bit: regtest's deployments use 28, 13 and 14 (chainparams.cpp CRegTestParams).
pub const BIT: u32 = 27;
/// The leg's last block: two blocks into ACTIVE's period.
pub const LAST: u32 = 577;
/// Regtest's `nMinerConfirmationWindow` and `nRuleChangeActivationThreshold`.
pub const WINDOW: u32 = 144;
pub const THRESHOLD: u32 = 108;
/// The signalling heights: 107 in 144..287, 108 in 288..431.
pub const SIGNALS: [RangeInclusive<u32>; 2] = [144..=250, 288..=395];
/// The tips the concise plane reads.
pub const CONCISE_AT: RangeInclusive<u32> = 573..=576;
/// The log plane's named edges (rev1 §7.2), reported with their lines.
pub const EDGES: [u32; 10] = [286, 287, 288, 430, 431, 432, 573, 574, 575, 576];
/// The pre-release concise warning (warnings.cpp), the one value normalized to "".
pub const PRE_RELEASE: &str = "This is a pre-release test build - use at your own risk - do not use \
                               for mining or merchant applications";

/// Whether the block at `h` signals [`BIT`].
pub fn signals(h: u32) -> bool {
    SIGNALS.iter().any(|r| r.contains(&h))
}

/// A BIP9 state of the warning checker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Defined,
    Started,
    LockedIn,
    Active,
}

/// The warning checker's state for the block after tip `tip` (`GetStateFor(tip)`), walked period
/// by period from genesis (versionbits.cpp:51-119): a period end below `min_height`
/// (`MinBIP9WarningHeight`) stays DEFINED uncounted (:64-67 cut the walk there); the first
/// computed end moves DEFINED to STARTED (start time 0), uncounted; a period with `threshold`
/// signalling blocks makes the next LOCKED_IN, and the one after ACTIVE.
pub fn walk_state(
    tip: u32,
    (period, threshold, min_height): (u32, u32, u32),
    signals: &dyn Fn(u32) -> bool,
) -> State {
    let mut state = State::Defined;
    // The period ends at or below `tip`, in order.
    for end in (period - 1..=tip).step_by(period as usize) {
        if end < min_height {
            continue;
        }
        state = match state {
            State::Defined => State::Started,
            State::Started => {
                let count = (end + 1 - period..=end).filter(|h| signals(*h)).count() as u32;
                if count >= threshold {
                    State::LockedIn
                } else {
                    State::Started
                }
            }
            State::LockedIn | State::Active => State::Active,
        };
    }
    state
}

/// [`walk_state`] on regtest (MinBIP9WarningHeight 0) for the warn leg's signals.
pub fn state_after(tip: u32) -> State {
    walk_state(tip, (WINDOW, THRESHOLD, 0), &signals)
}

/// Core's text for the bit (validation.cpp:3238).
pub fn unknown_rules() -> String {
    format!("Unknown new rules activated (versionbit {BIT})")
}

/// The warning tip `tip`'s UpdateTip line carries: LOCKED_IN's.
pub fn log_warning(tip: u32) -> Option<String> {
    (state_after(tip) == State::LockedIn).then(unknown_rules)
}

/// The concise warning at tip `tip`, normalized: ACTIVE's DoWarning, which persists.
pub fn concise(tip: u32) -> String {
    match (1..=tip).any(|t| state_after(t) == State::Active) {
        true => unknown_rules(),
        false => String::new(),
    }
}

/// The concise warning as compared: the pre-release text alone is "".
pub fn normalize(warning: &str) -> &str {
    if warning == PRE_RELEASE {
        ""
    } else {
        warning
    }
}

/// The leg's chain arguments: D49's and `-par=1`.
pub fn args() -> Vec<String> {
    CHAIN_ARGS[..3].iter().copied().chain([Par::One.arg()]).map(str::to_owned).collect()
}

/// An UpdateTip line reduced to its height, hash and warning, or `None` for another line.
pub fn update_tip(line: &str) -> Option<(u32, String, Option<String>)> {
    let rest = &line[line.find("UpdateTip: new best=")? + "UpdateTip: new best=".len()..];
    let hash = rest.get(..64)?.to_owned();
    let height = rest[64..].strip_prefix(" height=")?;
    let height = height[..height.find(' ').unwrap_or(height.len())].parse().ok()?;
    let warning = line.find(" warning='").and_then(|at| {
        line[at + " warning='".len()..].trim_end().strip_suffix('\'').map(str::to_owned)
    });
    Some((height, hash, warning))
}

/// Each tip's one UpdateTip line (height 0 aside): a missing or repeated tip in 1..=`last`
/// fails, as does a tip past it.
pub fn per_tip<'l>(
    side: &str,
    lines: impl IntoIterator<Item = &'l str>,
    last: u32,
) -> Result<BTreeMap<u32, (String, Option<String>)>, String> {
    let mut tips = BTreeMap::new();
    for (height, hash, warning) in lines.into_iter().filter_map(update_tip) {
        if height == 0 {
            continue;
        }
        if height > last || tips.insert(height, (hash, warning)).is_some() {
            return Err(format!("{side}: UpdateTip for {height} is repeated or past {last}"));
        }
    }
    match (1..=last).find(|h| !tips.contains_key(h)) {
        Some(h) => Err(format!("{side}: no UpdateTip line for tip {h}")),
        None => Ok(tips),
    }
}

/// The leg's record.
#[derive(Debug, Default)]
pub struct WarnLog {
    pub dashd_argv: Vec<String>,
    /// V's E-evo counters after the last block (§3.6).
    pub evo_counts: BTreeMap<&'static str, u64>,
    /// Each tip's hash and its log warning (both sides equal).
    pub tips: BTreeMap<u32, (String, Option<String>)>,
    /// The concise plane: tip, dashd's and V's warnings, as returned.
    pub concise: Vec<(u32, String, String)>,
    pub seconds: f64,
    pub error: Option<String>,
}

impl WarnLog {
    /// Its report, in the census's shape (set `warn`, one leg, a one-row manifest; plan §6.3's
    /// union): the edges' lines, the concise reads, the counts, and the status, with the rdc
    /// head it ran at, as the full set's and AV's reports carry it (OD-L3-72 m5).
    pub fn report(&self, dashd: &Dashd, rdc_head: &str) -> Value {
        let done = self.error.is_none();
        let leg = self.leg();
        json!({
            "set": "warn",
            "rdc_head": rdc_head,
            "dashd": {
                "version": dashd.version,
                "binary": dashd.binary,
                "line": dashd.line,
                "sha256": dashd.sha256,
                "tag": dashd.tag,
                "commit": dashd.commit,
            },
            "manifest": [{"trial_id": "warn", "status": if done { "ran" } else { "not run" }}],
            "legs": [leg],
        })
    }

    /// The leg's part of [`WarnLog::report`].
    fn leg(&self) -> Value {
        let line = |h: &u32| {
            self.tips.get(h).map(|(hash, w)| json!({"tip": h, "hash": hash, "warning": w}))
        };
        json!({
            "name": "warn",
            "status": if self.error.is_none() { "done" } else { "failed" },
            "error": self.error,
            "chain_args": args(),
            "dashd_argv": self.dashd_argv,
            "bit": BIT,
            "signals": SIGNALS.iter().map(|r| [r.start(), r.end()]).collect::<Vec<_>>(),
            "tips": self.tips.len(),
            "locked_in_tips": self.tips.values().filter(|t| t.1.is_some()).count(),
            "edges": EDGES.iter().filter_map(line).collect::<Vec<_>>(),
            "concise": self.concise.iter().map(|(h, d, v)| json!({"tip": h, "dashd": d, "v": v})).collect::<Vec<_>>(),
            "evo_keys": evo::report(evo::EVO_KEYS, &self.evo_counts),
            "trials": [],
            "seconds": self.seconds,
        })
    }
}

/// Run the leg in a new directory under `dir`; V's UpdateTip lines are `v_lines()`'s, read once
/// the chain is built.
pub fn run(dashd: &Dashd, dir: &Path, v_lines: &dyn Fn() -> Vec<String>) -> WarnLog {
    let start = std::time::Instant::now();
    let mut log = WarnLog::default();
    if let Err(e) = run_on(dashd, dir, v_lines, &mut log) {
        log.error = Some(e);
    }
    log.seconds = start.elapsed().as_secs_f64();
    log
}

fn run_on(
    dashd: &Dashd,
    dir: &Path,
    v_lines: &dyn Fn() -> Vec<String>,
    log: &mut WarnLog,
) -> Result<(), String> {
    let args = args();
    let datadir = private_dir(dir, "dashd-warn")?;
    let node = Process::start(&dashd.binary, &datadir, &args, T0)?;
    log.dashd_argv = node.argv().to_vec();
    let rpc = |method: &str, params: Value| node.rpc().ok(method, params);
    let mut v = NodeSide::with_argv(&args, T0, &private_dir(dir, "v-warn")?, None)?;
    if let Some(message) = v.down() {
        return Err(format!("V's load was refused: {message}"));
    }
    let mut hashes = BTreeMap::new();
    let mut ops = 0;
    for h in 1..=LAST {
        let mocktime = T0 + 150 * i64::from(h);
        rpc("setmocktime", json!([mocktime]))?;
        let clock = v
            .step(&ChainOp::SetTime(mocktime))
            .map_err(|u| format!("V refused the clock: {u:?}"))?;
        let template = rpc("generateblock", json!(["raw(51)", [], false]))?;
        let mut plan =
            BlockPlan::from_block(&from_hex(template["hex"].as_str().ok_or("no template hex")?)?)?;
        if signals(h) {
            plan.version |= 1 << BIT;
        }
        let built = plan.build();
        // §3.6: DIP3 and V24 inactive for the body on both sides, no decoded CbTx.
        let parent = plan.prev;
        let deployments = rpc("getdeploymentinfo", json!([hex(&parent)]))?;
        let first = to_hex(&dash_consensus::core_serialize(&*plan.txs[0].tx()));
        let decoded = rpc("decoderawtransaction", json!([first]))?;
        let v_deployments = clock.step.post.deployments.clone().unwrap_or_default();
        planes::body_gates(&parent, &deployments, &v_deployments, &decoded)?;
        let reply =
            node.rpc().call("submitblock", json!([to_hex(&built.bytes)]))?.map_err(|e| RpcError {
                code: Some(e.code),
                message: e.message,
            });
        let step = v
            .step(&ChainOp::Block(built.bytes.clone()))
            .map_err(|u| format!("V refused {h}: {u:?}"))?;
        planes::p1(ops, &reply, &step.step.reply.outcome)?;
        ops += 1;
        if reply != Ok(Value::Null) {
            return Err(format!("block {h} was refused: {reply:?}"));
        }
        let counts = v.evo_gates().ok_or("V's E-evo counters are not registered")?;
        evo::gate_check(&counts, ops)?;
        log.evo_counts = counts;
        let info = rpc("getblockchaininfo", json!([]))?;
        let post = &step.step.post;
        if rpc_hash(&rpc("getbestblockhash", json!([]))?)? != built.hash
            || post.tip != Some(block_at(h, built.hash))
        {
            return Err(format!("block {h} is not the tip on both sides"));
        }
        if info["initialblockdownload"] != false || post.ibd != Some(false) {
            return Err(format!("tip {h}: a side is in IBD"));
        }
        if CONCISE_AT.contains(&h) {
            let dashd_warning = info["warnings"].as_str().ok_or("no warnings string")?.to_owned();
            let v_warning = v.warnings().ok_or("V does not serve")?;
            let want = concise(h);
            if normalize(&dashd_warning) != want || normalize(&v_warning) != want {
                return Err(format!(
                    "concise at {h}: dashd {dashd_warning:?}, V {v_warning:?}, expected {want:?}"
                ));
            }
            log.concise.push((h, dashd_warning, v_warning));
        }
        hashes.insert(h, hex(&built.hash));
    }
    node.stop()?;
    let debug = std::fs::read_to_string(datadir.join("regtest/debug.log"))
        .map_err(|e| format!("dashd's debug.log: {e}"))?;
    let dashd_tips = per_tip("dashd", debug.lines(), LAST)?;
    let v_lines = v_lines();
    let v_tips = per_tip("V", v_lines.iter().map(String::as_str), LAST)?;
    for h in 1..=LAST {
        let want = (hashes[&h].clone(), log_warning(h));
        if dashd_tips[&h] != want || v_tips[&h] != want {
            return Err(format!(
                "UpdateTip at {h}: dashd {:?}, V {:?}, expected {want:?}",
                dashd_tips[&h], v_tips[&h]
            ));
        }
    }
    log.tips = dashd_tips;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The period arithmetic gives rev1 §7.2's tips: no warning through 430, LOCKED_IN at
    /// 431..574, ACTIVE from 575; and the signal counts are 107 and 108.
    #[test]
    fn the_periods_give_the_planned_tips() {
        assert_eq!((144..288).filter(|h| signals(*h)).count(), 107);
        assert_eq!((288..432).filter(|h| signals(*h)).count(), 108);
        assert_eq!(state_after(142), State::Defined);
        assert_eq!(state_after(143), State::Started);
        assert_eq!(state_after(287), State::Started);
        let locked: Vec<u32> = (1..=LAST).filter(|t| log_warning(*t).is_some()).collect();
        assert_eq!(locked, (431..=574).collect::<Vec<_>>());
        assert_eq!(state_after(575), State::Active);
        assert_eq!(concise(574), "");
        assert_eq!(concise(575), "Unknown new rules activated (versionbit 27)");
        assert_eq!(concise(LAST), concise(575), "DoWarning persists");
    }

    /// Core's UpdateTip line (and V's, which formats it the same) reduces to height, hash and
    /// warning; the applicability manifest wants each tip exactly once.
    #[test]
    fn update_tip_lines_reduce_and_are_counted() {
        let hash = "ab".repeat(32);
        let plain = format!(
            "2026-10-31T00:00:00Z UpdateTip: new best={hash} height=7 version=0x20000000 \
             log2_work=3.000000 tx=8 date='2023-11-14T22:30:50Z' progress=1.000000 \
             cache=0.0MiB(7utxo) evodb_cache=0.0MiB"
        );
        assert_eq!(update_tip(&plain), Some((7, hash.clone(), None)));
        let warned = format!("{plain} warning='{}'", unknown_rules());
        assert_eq!(update_tip(&warned), Some((7, hash.clone(), Some(unknown_rules()))));
        assert_eq!(update_tip("Leaving InitialBlockDownload"), None);
        let lines: Vec<String> =
            (0..=LAST).map(|h| plain.replace("height=7", &format!("height={h}"))).collect();
        assert_eq!(
            per_tip("x", lines.iter().map(String::as_str), LAST).map(|t| t.len()),
            Ok(LAST as usize)
        );
        assert!(per_tip("x", lines[..LAST as usize].iter().map(String::as_str), LAST).is_err());
        let twice = [lines.clone(), vec![lines[5].clone()]].concat();
        assert!(per_tip("x", twice.iter().map(String::as_str), LAST).is_err());
        assert_eq!(normalize(PRE_RELEASE), "");
        assert_eq!(args().last().map(String::as_str), Some("-par=1"));
    }
}
