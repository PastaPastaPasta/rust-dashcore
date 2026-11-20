//! The devnet leg's period walk (plan §7.2, M-8 / OD-L3-14): the witness that Core's warning
//! walk never counts a period ending below `MinBIP9WarningHeight` (2018 on devnets,
//! versionbits.cpp:64-67). A throwaway devnet dashd on [`args`] builds the prefix 2..=[`PREFIX`]
//! (block 1 is Core's own, [`devnet::BLOCK_1`]); three fresh datadirs are fed those bytes and
//! then build their own [`Chain`] in parallel, signalling [`warn::BIT`] in Core templates
//! re-ground with `nVersion |= 1 << 27`. Its planes, per chain:
//! - the log plane: UpdateTip's one line per tip 1..=[`Chain::last`] in debug.log, its warning
//!   LOCKED_IN's exactly where the walk ([`Chain::state_after`]) says;
//! - the concise plane at [`Chain::observations`]: `getblockchaininfo.warnings`, ACTIVE's
//!   DoWarning, with `ibd` false.
//!
//! V runs no devnet node past op 0 (OD-L3-23): V's side is [`V_ROUTE`] (OD-L3-25 (b)),
//! `unknown_rules`' copy of V's `pub(crate)` loop with one WarningCache across the walk,
//! over Core's headers (`getblockheader <hash> false`, genesis to tip) with rdashd's devnet
//! parameters for the same arguments, compared tip by tip with Core's log and concise warnings.
//! It is not a census leg (M-12): DIP3 is active from height 2 and its reports stay out of the
//! union. Core's per-header Condition guard (v:2422) is redundant under this walk (every counted
//! height is ≥ 4032) and stays covered by the in-process boundary tests only.

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::Path;

use dash_consensus::BlockId;
use dash_consensus::deployments::{
    Deployments, NoEhfSignals, ThresholdState, VERSIONBITS_NUM_BITS, VersionBitsCache, WarningCache,
};
use serde_json::{Value, json};

use super::dashd::Process;
use super::devnet;
use super::trial::{from_hex, to_hex};
use super::version::{Dashd, private_dir};
use super::warn::{self, State, walk_state};
use crate::chain::build::BlockPlan;
use crate::chain::mutate::Par;
use crate::chain::params::devnet_params;
use crate::mnhf::Line;

/// Devnet's `nMinerConfirmationWindow`, `nRuleChangeActivationThreshold` and
/// `MinBIP9WarningHeight` (chainparams.cpp CDevNetParams).
pub const PERIOD: u32 = 2016;
pub const THRESHOLD: u32 = 1512;
pub const MIN_HEIGHT: u32 = 2018;
/// The shared prefix's last height: the first period, no signals.
pub const PREFIX: u32 = 2015;
/// Keeps the minimum difficulty past DGW (4001) under 150 s spacing (the feasibility probe).
pub const MIN_DIFFICULTY: &str = "-minimumdifficultyblocks=9000";

/// The leg's devnet arguments: OD-L3-22's, [`MIN_DIFFICULTY`] and `-par=1`.
pub fn args() -> Vec<String> {
    devnet::ARGS
        .iter()
        .copied()
        .chain([MIN_DIFFICULTY, Par::One.arg()])
        .map(str::to_owned)
        .collect()
}

/// Plan §7.2's three chains (period ends 2015 / 4031 / 6047 / 8063).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    /// 1,512 signals in 4032..6047: LOCKED_IN at tips 6047..8062, ACTIVE from 8063.
    X,
    /// 1,511 there: never locks in.
    Y,
    /// Every block of 2016..4031, the period the walk never counts: no warning anywhere.
    Z,
}

impl Chain {
    pub const ALL: [Chain; 3] = [Chain::X, Chain::Y, Chain::Z];

    pub fn name(self) -> &'static str {
        match self {
            Chain::X => "X",
            Chain::Y => "Y",
            Chain::Z => "Z",
        }
    }

    /// The heights whose blocks signal the bit.
    pub fn signalling(self) -> RangeInclusive<u32> {
        match self {
            Chain::X => 4032..=5543,
            Chain::Y => 4032..=5542,
            Chain::Z => 2016..=4031,
        }
    }

    pub fn last(self) -> u32 {
        match self {
            Chain::X => 8064,
            Chain::Y | Chain::Z => 6060,
        }
    }

    /// The tips whose concise warning and `ibd` are read.
    pub fn observations(self) -> &'static [u32] {
        match self {
            Chain::X => &[4031, 4032, 6046, 6047, 6048, 8062, 8063, 8064],
            Chain::Y => &[4031, 6046, 6047, 6048],
            Chain::Z => &[4030, 4031, 4032, 6046, 6047, 6048],
        }
    }

    /// The warning checker's state for the block after `tip`, on devnet's walk.
    pub fn state_after(self, tip: u32) -> State {
        walk_state(tip, (PERIOD, THRESHOLD, MIN_HEIGHT), &|h| self.signalling().contains(&h))
    }

    /// The warning tip `tip`'s UpdateTip line carries: LOCKED_IN's.
    pub fn log_warning(self, tip: u32) -> Option<String> {
        (self.state_after(tip) == State::LockedIn).then(warn::unknown_rules)
    }

    /// The concise warning at `tip`, normalized: ACTIVE's DoWarning (ACTIVE never ends).
    pub fn concise(self, tip: u32) -> String {
        match self.state_after(tip) {
            State::Active => warn::unknown_rules(),
            _ => String::new(),
        }
    }
}

/// One chain's record.
#[derive(Debug, Default)]
pub struct ChainLog {
    pub name: &'static str,
    pub dashd_argv: Vec<String>,
    /// Each tip's hash and its log warning (dashd's, as the walk expects).
    pub tips: BTreeMap<u32, (String, Option<String>)>,
    /// The concise plane: tip, dashd's warning as returned, V's.
    pub concise: Vec<(u32, String, String)>,
    pub error: Option<String>,
}

/// The leg's record.
#[derive(Debug, Default)]
pub struct WalkLog {
    pub prefix_argv: Vec<String>,
    pub chains: Vec<ChainLog>,
    pub seconds: f64,
    pub error: Option<String>,
}

impl WalkLog {
    /// The first failure: the prefix's, or a chain's.
    pub fn failure(&self) -> Option<String> {
        self.error.clone().or_else(|| {
            self.chains.iter().find_map(|c| c.error.as_ref().map(|e| format!("{}: {e}", c.name)))
        })
    }

    /// Its report: per chain the observation tips' lines, the concise reads and the counts.
    pub fn report(&self) -> Value {
        let chain = |c: &ChainLog| {
            let spec = Chain::ALL.into_iter().find(|s| s.name() == c.name);
            let observed = spec.map(Chain::observations).unwrap_or_default();
            let line = |h: &u32| {
                c.tips.get(h).map(|(hash, w)| json!({"tip": h, "hash": hash, "warning": w}))
            };
            json!({
                "chain": c.name,
                "status": if c.error.is_none() { "done" } else { "failed" },
                "error": c.error,
                "dashd_argv": c.dashd_argv,
                "signalling": spec.map(|s| [*s.signalling().start(), *s.signalling().end()]),
                "tips": c.tips.len(),
                "locked_in_tips": c.tips.values().filter(|t| t.1.is_some()).count(),
                "observations": observed.iter().filter_map(line).collect::<Vec<_>>(),
                "concise": c.concise.iter().map(|(h, d, v)| json!({"tip": h, "dashd": d, "v": v})).collect::<Vec<_>>(),
            })
        };
        json!({
            "name": "devnet-walk",
            "status": if self.failure().is_none() { "done" } else { "failed" },
            "error": self.failure(),
            "args": args(),
            "prefix_argv": self.prefix_argv,
            "v_route": V_ROUTE,
            "bit": warn::BIT,
            "period": PERIOD,
            "threshold": THRESHOLD,
            "min_bip9_warning_height": MIN_HEIGHT,
            "chains": self.chains.iter().map(chain).collect::<Vec<_>>(),
            "seconds": self.seconds,
        })
    }
}

/// Run the leg in new directories under `dir`.
pub fn run(dashd: &Dashd, dir: &Path) -> WalkLog {
    let start = std::time::Instant::now();
    let mut log = WalkLog::default();
    match prefix(dashd, dir, &mut log) {
        Ok(prefix) => {
            let prefix = prefix.as_slice();
            log.chains = std::thread::scope(|s| {
                let runs: Vec<_> = Chain::ALL
                    .map(|c| s.spawn(move || run_chain(dashd, dir, c, prefix)))
                    .into_iter()
                    .collect();
                runs.into_iter()
                    .zip(Chain::ALL)
                    .map(|(run, c)| {
                        run.join().unwrap_or_else(|_| ChainLog {
                            name: c.name(),
                            error: Some("its thread panicked".to_owned()),
                            ..ChainLog::default()
                        })
                    })
                    .collect()
            });
        }
        Err(e) => log.error = Some(e),
    }
    log.seconds = start.elapsed().as_secs_f64();
    log
}

/// The prefix 2..=[`PREFIX`] from a throwaway dashd: Core's blocks, as built. Also written to
/// `prefix.hex` under `dir`, one block per line.
fn prefix(dashd: &Dashd, dir: &Path, log: &mut WalkLog) -> Result<Vec<Vec<u8>>, String> {
    let datadir = private_dir(dir, "dashd-prefix")?;
    let node = Process::start(&dashd.binary, &datadir, &args(), REAL_CLOCK)?;
    log.prefix_argv = node.argv().to_vec();
    devnet::dashd_tips(&node)?;
    let blocks =
        (2..=PREFIX).map(|h| next_block(&node, h, false)).collect::<Result<Vec<_>, _>>()?;
    node.stop()?;
    let lines: Vec<String> = blocks.iter().map(|b| to_hex(b)).collect();
    std::fs::write(dir.join("prefix.hex"), lines.join("\n") + "\n")
        .map_err(|e| format!("prefix.hex: {e}"))?;
    Ok(blocks)
}

/// The devnet nodes' clock: `-mocktime=0`, the real one. Core refuses `setmocktime` off regtest
/// (`IsMockableChain`), and a clock fixed by `-mocktime` would leave the tip `time-too-new` long
/// before [`Chain::last`]; Core's templates take `max(MTP + 1, now)`.
const REAL_CLOCK: i64 = 0;

/// The block at `h` on dashd's tip: Core's template, re-ground with the bit when `signal`,
/// submitted and accepted.
fn next_block(node: &Process, h: u32, signal: bool) -> Result<Vec<u8>, String> {
    let template = node.rpc().ok("generateblock", json!(["raw(51)", [], false]))?;
    let mut bytes = from_hex(template["hex"].as_str().ok_or("no template hex")?)?;
    if signal {
        let mut plan = BlockPlan::from_block(&bytes)?;
        plan.version |= 1 << warn::BIT;
        bytes = plan.build().bytes;
    }
    submit(node, h, &bytes)?;
    Ok(bytes)
}

fn submit(node: &Process, h: u32, bytes: &[u8]) -> Result<(), String> {
    match node.rpc().ok("submitblock", json!([to_hex(bytes)]))? {
        Value::Null => Ok(()),
        refused => Err(format!("block {h} was refused: {refused}")),
    }
}

fn run_chain(dashd: &Dashd, dir: &Path, chain: Chain, prefix: &[Vec<u8>]) -> ChainLog {
    let mut log = ChainLog {
        name: chain.name(),
        ..ChainLog::default()
    };
    if let Err(e) = build_chain(dashd, dir, chain, prefix, &mut log) {
        log.error = Some(e);
    }
    log
}

fn build_chain(
    dashd: &Dashd,
    dir: &Path,
    chain: Chain,
    prefix: &[Vec<u8>],
    log: &mut ChainLog,
) -> Result<(), String> {
    let datadir = private_dir(dir, &format!("dashd-{}", chain.name()))?;
    let node = Process::start(&dashd.binary, &datadir, &args(), REAL_CLOCK)?;
    log.dashd_argv = node.argv().to_vec();
    let rpc = |method: &str, params: Value| node.rpc().ok(method, params);
    devnet::dashd_tips(&node)?;
    for (h, bytes) in (2..).zip(prefix) {
        submit(&node, h, bytes)?;
    }
    let mut concise = BTreeMap::new();
    for h in PREFIX + 1..=chain.last() {
        next_block(&node, h, chain.signalling().contains(&h))?;
        if chain.observations().contains(&h) {
            let info = rpc("getblockchaininfo", json!([]))?;
            if info["blocks"] != h || info["initialblockdownload"] != false {
                return Err(format!(
                    "tip {h}: blocks {}, ibd {}",
                    info["blocks"], info["initialblockdownload"]
                ));
            }
            let warning = info["warnings"].as_str().ok_or("no warnings string")?.to_owned();
            if warn::normalize(&warning) != chain.concise(h) {
                return Err(format!(
                    "concise at {h}: dashd {warning:?}, expected {:?}",
                    chain.concise(h)
                ));
            }
            concise.insert(h, warning);
        }
    }
    let headers = (0..=chain.last())
        .map(|h| {
            let hash = rpc("getblockhash", json!([h]))?;
            let header =
                from_hex(rpc("getblockheader", json!([hash, false]))?.as_str().unwrap_or(""))?;
            header.try_into().map_err(|_| format!("header {h} is not 80 bytes"))
        })
        .collect::<Result<Vec<[u8; 80]>, String>>()?;
    node.stop()?;
    let debug = std::fs::read_to_string(datadir.join("devnet-l3/debug.log"))
        .map_err(|e| format!("dashd's debug.log: {e}"))?;
    let tips = warn::per_tip("dashd", debug.lines(), chain.last())?;
    for (h, (hash, warning)) in &tips {
        let want = chain.log_warning(*h);
        let core_hash = super::trial::hex(&crate::chain::world::header_hash(&headers[*h as usize]));
        if *warning != want || *hash != core_hash {
            return Err(format!(
                "UpdateTip at {h}: {hash} {warning:?}, expected {core_hash} {want:?}"
            ));
        }
    }
    log.tips = tips;
    let v = v_warnings(&headers)?;
    for (h, (v_log, _)) in v.iter().enumerate().skip(1) {
        let h = h as u32;
        if *v_log != chain.log_warning(h) {
            return Err(format!(
                "V's log warning at {h}: {v_log:?}, Core {:?}",
                chain.log_warning(h)
            ));
        }
    }
    for (h, warning) in concise {
        let v_concise = &v[h as usize].1;
        if v_concise != warn::normalize(&warning) {
            return Err(format!("V's concise warning at {h}: {v_concise:?}, dashd {warning:?}"));
        }
        log.concise.push((h, warning, v_concise.clone()));
    }
    Ok(())
}

/// V's warnings at each tip of `headers` (genesis first): UpdateTip's log warning (the
/// LOCKED_IN bits' texts, joined as Core's `AppendWarning`) and the concise one (the last
/// ACTIVE bit's DoWarning so far, which persists), through `unknown_rules` with rdashd's
/// devnet parameters for [`args`].
pub fn v_warnings(headers: &[[u8; 80]]) -> Result<Vec<(Option<String>, String)>, String> {
    let args = args();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let params = devnet_params(&args)?;
    let line = Line::new(headers);
    let cache = VersionBitsCache::new();
    let deployments = Deployments::new(&params, &line, &NoEhfSignals, &cache);
    // One cache across the walk, as V keeps its own (`ChainDeployments::warning`).
    let mut warning = WarningCache::new();
    let mut misc = String::new();
    let mut out = vec![(None, String::new())];
    for tip in 1..headers.len() as u32 {
        let mut locked = Vec::new();
        for (bit, state) in unknown_rules(&deployments, BlockId(tip), &mut warning) {
            let text = format!("Unknown new rules activated (versionbit {bit})");
            match state {
                ThresholdState::Active => misc = text,
                _ => locked.push(text),
            }
        }
        out.push(((!locked.is_empty()).then(|| locked.join(", ")), misc.clone()));
    }
    Ok(out)
}

/// How the report names V's side of the walk (O10 P-4's naming rule).
pub const V_ROUTE: &str = "`warning_state` per bit, V's `unknown_rules` loop";

/// V's `ChainDeployments::unknown_rules` (dashd-chainstate/src/chain/deployments.rs:75-85,
/// `pub(crate)`, so copied, not called): every bit in `0..VERSIONBITS_NUM_BITS`, keeping
/// `LOCKED_IN` and `ACTIVE`, in bit order.
fn unknown_rules(
    deployments: &Deployments<'_>,
    tip: BlockId,
    warning: &mut WarningCache,
) -> Vec<(i32, ThresholdState)> {
    let states = (0..VERSIONBITS_NUM_BITS)
        .map(|bit| deployments.warning_state(bit, Some(tip), warning).map(|state| (bit, state)))
        .collect::<Result<Vec<_>, _>>()
        .expect("NoEhfSignals cannot fault");
    states
        .into_iter()
        .filter(|(_, state)| matches!(state, ThresholdState::Active | ThresholdState::LockedIn))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rdashd parses the leg's arguments into devnet's walk parameters, OD-L3-22's subsidy and
    /// the minimum difficulty the chains rely on.
    #[test]
    fn rdashd_reads_the_legs_devnet_args() {
        let args = args();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let p = devnet_params(&args).expect("devnet params");
        let c = &p.consensus;
        assert_eq!(c.min_bip9_warning_height.to_u32(), MIN_HEIGHT);
        assert_eq!(c.miner_confirmation_window, PERIOD);
        assert_eq!(c.rule_change_activation_threshold, THRESHOLD);
        assert_eq!((c.high_subsidy_blocks, c.high_subsidy_factor), (1, 1));
        assert_eq!(c.minimum_difficulty_blocks, 9000);
    }

    /// Plan §7.2's table: X locks in at 6047 and activates at 8063, Y never locks in, Z's
    /// period is never counted; a walk counting from 0 would lock Z in at 4031.
    #[test]
    fn the_chains_give_the_planned_tips() {
        assert_eq!(Chain::X.signalling().count() as u32, THRESHOLD);
        assert_eq!(Chain::Y.signalling().count() as u32, THRESHOLD - 1);
        assert_eq!(Chain::Z.signalling().count() as u32, PERIOD);
        for c in Chain::ALL {
            assert_eq!(c.state_after(PREFIX), State::Defined, "{c:?}");
            assert_eq!(c.state_after(4031), State::Started, "{c:?}");
            assert!(c.observations().iter().all(|t| *t <= c.last() && *t > PREFIX));
        }
        let locked: Vec<u32> =
            (1..=Chain::X.last()).filter(|t| Chain::X.log_warning(*t).is_some()).collect();
        assert_eq!(locked, (6047..=8062).collect::<Vec<_>>());
        assert_eq!(Chain::X.concise(8062), "");
        assert_eq!(Chain::X.concise(8063), warn::unknown_rules());
        for c in [Chain::Y, Chain::Z] {
            assert!((1..=c.last()).all(|t| c.log_warning(t).is_none() && c.concise(t).is_empty()));
        }
        let counted =
            walk_state(4031, (PERIOD, THRESHOLD, 0), &|h| Chain::Z.signalling().contains(&h));
        assert_eq!(counted, State::LockedIn, "the suppression the walk witnesses");
    }

    /// V's evaluation over a line of headers: X's signals give LOCKED_IN then ACTIVE where the
    /// walk says. Headers carry no PoW; the checker reads only heights, versions and times.
    #[test]
    fn v_warns_on_a_synthetic_x_line() {
        let headers: Vec<[u8; 80]> = (0..=Chain::X.last())
            .map(|h| {
                let mut header = [0u8; 80];
                let version = 0x2000_0000u32
                    | if Chain::X.signalling().contains(&h) {
                        1 << warn::BIT
                    } else {
                        0
                    };
                header[..4].copy_from_slice(&version.to_le_bytes());
                header[68..72]
                    .copy_from_slice(&(crate::l3::leg::T0 as u32 + 150 * h).to_le_bytes());
                header[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
                header
            })
            .collect();
        let v = v_warnings(&headers).expect("V's warnings");
        for tip in 1..=Chain::X.last() {
            assert_eq!(v[tip as usize].0, Chain::X.log_warning(tip), "log at {tip}");
            assert_eq!(v[tip as usize].1, Chain::X.concise(tip), "concise at {tip}");
        }
    }
}
