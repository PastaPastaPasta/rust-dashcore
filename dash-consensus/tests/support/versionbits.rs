//! CS4's versionbits scenarios and the node's side of their queries, shared by `node-l1` (which
//! runs Core's side through `oracle_versionbits_state`) and this crate's tests (which replay
//! Core's recorded answers).
//!
//! A [`Scenario`] is what the oracle call takes: a chain selection (with Core's arguments), a
//! window of headers at `first_height..` with placeholder entries below it (zero time and
//! version, as the oracle's context), the EHF signals each block holds from a height on
//! (`CMNHFManager::GetForBlock`), and a list of queries run in order against one cache that
//! starts empty. The node's answers come from [`Scenario::ours`].
//!
//! The fuzz input format ([`Scenario::from_bytes`]) is total: a short input is [`None`], never a
//! panic, and every other input is a scenario.

#![allow(dead_code, reason = "each includer uses a subset")]

use std::sync::atomic::{AtomicBool, Ordering};

use dash_consensus::deployments::{
    Deployments, EhfSignalSource, EhfSignals, VERSIONBITS_TOP_BITS, VersionBitsCache,
};
use dash_consensus::{BlockId, ChainAncestry, HeaderCtx};
use dash_num::{Arith256, CompactTarget};
use dash_params::{
    Bip9Deployment, BuriedDeployment, ChainOption, ChainParamsBuilder, ChainSetup, DeploymentPos,
    Network as CoreNetwork, Release,
};
use dash_primitives::BlockHash;
use dash_types::Numeric as _;

/// The networks a scenario selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Network {
    Main,
    Test,
    Regtest,
    /// `-devnet=cs4`.
    Devnet,
}

pub const DEVNET_NAME: &str = "cs4";

impl Network {
    pub const ALL: [Network; 4] = [Network::Main, Network::Test, Network::Regtest, Network::Devnet];

    /// Core's chain name.
    pub fn core_name(self) -> &'static str {
        match self {
            Network::Main => "main",
            Network::Test => "test",
            Network::Regtest => "regtest",
            Network::Devnet => "devnet",
        }
    }

    pub fn from_core_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|n| n.core_name() == name)
    }

    fn kind(self) -> CoreNetwork {
        match self {
            Network::Main => CoreNetwork::Main,
            Network::Test => CoreNetwork::Testnet3,
            Network::Regtest => CoreNetwork::Regtest,
            Network::Devnet => CoreNetwork::Devnet,
        }
    }
}

/// `count` headers of `version`, the first at `time`, each `step` seconds after the previous
/// (wrapping, as a `uint32`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub count: u32,
    pub version: i32,
    pub time: u32,
    pub step: i32,
}

/// One query; heights are `None` for `nullptr` (genesis's parent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    State(DeploymentPos, Option<u32>),
    StateSinceHeight(DeploymentPos, Option<u32>),
    Statistics(DeploymentPos, Option<u32>),
    ComputeBlockVersion(Option<u32>),
    SignalsStage(Option<u32>),
}

impl Query {
    pub fn height(self) -> Option<u32> {
        match self {
            Query::State(_, h)
            | Query::StateSinceHeight(_, h)
            | Query::Statistics(_, h)
            | Query::ComputeBlockVersion(h)
            | Query::SignalsStage(h) => h,
        }
    }
}

/// The answer to a [`Query`] of the same kind, in Core's types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `ThresholdState` as Core's enum value.
    State(u8),
    StateSinceHeight(i32),
    Statistics {
        period: i32,
        threshold: i32,
        elapsed: i32,
        count: i32,
        possible: bool,
        blocks: Vec<bool>,
    },
    ComputeBlockVersion(i32),
    SignalsStage(Vec<(u8, i32)>),
}

/// A versionbits scenario (see the module documentation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scenario {
    pub network: Network,
    /// Core's chain arguments, `-name=value` (`-devnet=cs4` is implied on devnet).
    pub args: Vec<String>,
    pub first_height: u32,
    pub headers: Vec<Run>,
    /// `(height, signals)`: from `height` on, every header at which V20 is active holds these
    /// `(bit, signal height)` pairs, until the next change. Ascending header heights.
    pub signals: Vec<(u32, Vec<(u8, i32)>)>,
    pub queries: Vec<Query>,
}

/// Why the node's side has no answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The chain arguments do not build (Core's chain selection fails too).
    Params,
    /// A deployment's period, as `int`, is below 1: the oracle refuses it (Core divides by it).
    Period,
    /// The scenario breaks the oracle's own rules (signal changes, query heights, no headers).
    Malformed,
    /// Core read the EHF signals of a placeholder at which V20 is active (`ORACLE_E_CONTEXT`).
    Context,
}

impl Scenario {
    /// Core's chain arguments, with the devnet name.
    pub fn core_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.network == Network::Devnet {
            args.push(format!("-devnet={DEVNET_NAME}"));
        }
        args.extend(self.args.iter().cloned());
        args
    }

    /// v24's chain parameters for the selection; `None` if Core's arguments do not build.
    pub fn params(&self) -> Option<ChainSetup> {
        let mut builder = ChainParamsBuilder::new(self.network.kind(), Release::V24_0);
        for arg in self.core_args() {
            let (name, value) = arg.split_once('=').unwrap_or((&arg, ""));
            builder = builder.arg(ChainOption::from_name(name)?, value);
        }
        builder.build().ok()
    }

    /// `(nVersion, nTime)` of each header, from `first_height`.
    pub fn header_fields(&self) -> Vec<(i32, u32)> {
        let mut out = Vec::new();
        for run in &self.headers {
            out.extend(
                (0..run.count).map(|i| {
                    (run.version, run.time.wrapping_add((run.step as u32).wrapping_mul(i)))
                }),
            );
        }
        out
    }

    /// The tip's height, if the scenario has headers.
    pub fn tip(&self) -> Option<u32> {
        let n: u64 = self.headers.iter().map(|r| u64::from(r.count)).sum();
        u32::try_from(u64::from(self.first_height) + n.checked_sub(1)?).ok()
    }

    /// The oracle's checks of the scenario's own shape (`oracle_versionbits_state`'s
    /// `ORACLE_E_ARG` cases other than the period).
    fn well_formed(&self) -> bool {
        let Some(tip) = self.tip() else {
            return false;
        };
        if i64::from(tip) >= i64::from(i32::MAX) {
            return false;
        }
        let mut last = None;
        for (height, signals) in &self.signals {
            if *height < self.first_height || *height > tip || last.is_some_and(|l| *height <= l) {
                return false;
            }
            last = Some(*height);
            let mut seen = 0u32;
            for &(bit, h) in signals {
                if bit >= 29 || h < 0 || i64::from(h) > i64::from(*height) || seen & (1 << bit) != 0
                {
                    return false;
                }
                seen |= 1 << bit;
            }
        }
        self.queries.iter().all(|q| q.height().is_none_or(|h| h <= tip))
    }

    /// The node's answers: [`Deployments`] over the scenario's chain, one cache, the queries in
    /// order.
    pub fn ours(&self) -> Result<Vec<Answer>, Refused> {
        if !self.well_formed() {
            return Err(Refused::Malformed);
        }
        let params = self.params().ok_or(Refused::Params)?;
        let c = &params.consensus;
        for (_, d) in c.deployments.iter() {
            let period = if d.window_size != 0 {
                d.window_size
            } else {
                i64::from(c.miner_confirmation_window)
            };
            if (period as i32) < 1 {
                return Err(Refused::Period);
            }
        }
        let chain = Chain::new(self.first_height, &self.header_fields());
        let ehf = Signals {
            chain: &chain,
            params: &params,
            changes: &self.signals,
            missing: AtomicBool::new(false),
        };
        let cache = VersionBitsCache::new();
        let d = Deployments::new(&params, &chain, &ehf, &cache);
        let id = |h: Option<u32>| h.map(BlockId);
        let mut answers = Vec::with_capacity(self.queries.len());
        for &query in &self.queries {
            answers.push(match query {
                Query::State(pos, h) => Answer::State(d.state(id(h), pos) as u8),
                Query::StateSinceHeight(pos, h) => {
                    Answer::StateSinceHeight(d.state_since_height(id(h), pos))
                }
                Query::Statistics(pos, h) => {
                    let mut blocks = Vec::new();
                    let s = d.statistics(id(h), pos, Some(&mut blocks));
                    Answer::Statistics {
                        period: s.period,
                        threshold: s.threshold,
                        elapsed: s.elapsed,
                        count: s.count,
                        possible: s.possible,
                        blocks,
                    }
                }
                Query::ComputeBlockVersion(h) => {
                    Answer::ComputeBlockVersion(d.compute_block_version(id(h)))
                }
                Query::SignalsStage(h) => {
                    Answer::SignalsStage(d.ehf_signals_stage(id(h)).iter().collect())
                }
            });
            if ehf.missing.load(Ordering::Relaxed) {
                return Err(Refused::Context);
            }
        }
        Ok(answers)
    }
}

/// The scenario's chain: the headers at `first_height..` and the oracle's placeholders below,
/// with `BlockId(h)` the entry at height `h`.
pub struct Chain {
    first_height: u32,
    entries: Vec<HeaderCtx>,
}

impl Chain {
    pub fn new(first_height: u32, fields: &[(i32, u32)]) -> Self {
        let mut chain = Self {
            first_height,
            entries: Vec::with_capacity(fields.len()),
        };
        for (&(version, time), height) in fields.iter().zip(first_height..) {
            chain.entries.push(HeaderCtx {
                height,
                hash: BlockHash::from([0; 32]),
                time,
                bits: CompactTarget::new(0x207f_ffff),
                version,
                mtp: 0,
                chain_work: Arith256::ZERO,
            });
            let mtp = chain.median_time_past(height);
            chain.entries.last_mut().expect("pushed").mtp = mtp;
        }
        chain
    }

    pub fn tip(&self) -> u32 {
        self.first_height + self.entries.len() as u32 - 1
    }

    fn time(&self, height: u32) -> u32 {
        height.checked_sub(self.first_height).map_or(0, |i| self.entries[i as usize].time)
    }

    /// `GetMedianTimePast()`: the median of the times of `height` and its ten predecessors.
    // Core: chain.h:283 CBlockIndex::GetMedianTimePast
    fn median_time_past(&self, height: u32) -> i64 {
        let mut times: Vec<i64> =
            (height.saturating_sub(10)..=height).map(|h| i64::from(self.time(h))).collect();
        times.sort_unstable();
        times[times.len() / 2]
    }
}

impl ChainAncestry for Chain {
    fn header(&self, id: BlockId) -> HeaderCtx {
        assert!(id.0 <= self.tip(), "an id above the tip");
        match id.0.checked_sub(self.first_height) {
            Some(i) => self.entries[i as usize],
            None => HeaderCtx {
                height: id.0,
                hash: BlockHash::from([0; 32]),
                time: 0,
                bits: CompactTarget::new(0),
                version: 0,
                mtp: self.median_time_past(id.0),
                chain_work: Arith256::ZERO,
            },
        }
    }

    fn ancestor(&self, id: BlockId, height: u32) -> Option<BlockId> {
        (height <= id.0).then_some(BlockId(height))
    }

    fn prev(&self, id: BlockId) -> Option<BlockId> {
        id.0.checked_sub(1).map(BlockId)
    }
}

/// The oracle's EHF manager: each header at which V20 is active holds the change in force at its
/// height; a placeholder holds nothing Core may read.
struct Signals<'a> {
    chain: &'a Chain,
    params: &'a ChainSetup,
    changes: &'a [(u32, Vec<(u8, i32)>)],
    missing: AtomicBool,
}

impl EhfSignalSource for Signals<'_> {
    fn signals_for_block(&self, block: BlockId) -> EhfSignals {
        let v20 = self.params.consensus.deployment_height(BuriedDeployment::V20).to_u32();
        if block.0 < v20 {
            return EhfSignals::new();
        }
        if block.0 < self.chain.first_height {
            self.missing.store(true, Ordering::Relaxed);
            return EhfSignals::new();
        }
        self.changes
            .iter()
            .rev()
            .find(|(height, _)| *height <= block.0)
            .map(|(_, signals)| signals.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// Reads a fuzz input front to back; past the end every read is 0.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn u8(&mut self) -> u8 {
        let (&b, rest) = self.0.split_first().unwrap_or((&0, &[]));
        self.0 = rest;
        b
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    fn u32(&mut self) -> u32 {
        u32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// The versionbits deployments in Core's order (`Consensus::DeploymentPos`).
pub const POSITIONS: [DeploymentPos; 3] =
    [DeploymentPos::TestDummy, DeploymentPos::V24, DeploymentPos::EvoShares];

/// The base time of the fuzz and boundary scenarios: regtest's genesis time.
pub const BASE_TIME: u32 = 1_417_713_337;

/// The smallest input [`Scenario::from_bytes`] reads as a scenario.
pub const MIN_INPUT: usize = 8;

impl Scenario {
    /// The fuzz input format: a header byte (network and which parameter sets follow), the
    /// parameters, header runs, signal changes, then queries until the input ends. Values are
    /// drawn from the domains where the state machine branches: start and timeout times around
    /// the headers' times and the special values, small windows and thresholds, falloff
    /// coefficients that reach the threshold's int overflow, versions with and without the top
    /// bits and each deployment's bit, and query heights at and around period boundaries.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < MIN_INPUT {
            return None;
        }
        let mut r = Reader(bytes);
        let head = r.u8();
        // regtest has -vbparams and -testactivationheight, so most inputs select it
        let network = match head % 8 {
            0 => Network::Main,
            1 => Network::Test,
            2 => Network::Devnet,
            _ => Network::Regtest,
        };
        let mut args = Vec::new();
        let mut v20 = 0u32;
        if network == Network::Regtest {
            // V20 (and DIP3 and MN_RR below it, as Core requires) low enough for EHF signals
            v20 = u32::from(r.u8());
            args.push(format!("-dip3params={v20}:{v20}"));
            args.push(format!("-testactivationheight=v20@{v20}"));
            args.push(format!("-testactivationheight=mn_rr@{v20}"));
            for (i, pos) in POSITIONS.into_iter().enumerate() {
                if head & (0x08 << i) != 0 {
                    args.push(format!("-vbparams={}:{}", pos.name(), vbparams(&mut r)));
                }
            }
        }
        let first_height = match network {
            // near the heights where testnet's and mainnet's deployments can move
            Network::Main | Network::Test if head & 0x40 != 0 => {
                let floor = if network == Network::Test {
                    1_150_000
                } else {
                    0
                };
                floor + u32::from(r.u16()) * 16
            }
            _ => 0,
        };
        let mut headers = Vec::new();
        let mut time = match network {
            // around v24's start times (testnet 1790208000, devnet 1751328000)
            Network::Test => 1_790_000_000 + u32::from(r.u16()) * 600,
            Network::Devnet => 1_751_000_000 + u32::from(r.u16()) * 600,
            _ => BASE_TIME,
        };
        let runs = 1 + usize::from(r.u8() % 24);
        let mut total = 0u32;
        for _ in 0..runs {
            let count = 1 + u32::from(r.u8()) % 64 * (1 + u32::from(r.u8() % 4));
            let version = version(&mut r);
            let step = match r.u8() % 8 {
                0 => 0,
                1 => -600,
                2 => 1,
                3 => i32::from(r.u16() as i16),
                4 => 7_200,
                _ => 150,
            };
            headers.push(Run {
                count,
                version,
                time,
                step,
            });
            time = time.wrapping_add((step as u32).wrapping_mul(count));
            total += count;
        }
        let tip = first_height + total - 1;
        let mut signals = Vec::new();
        let changes = r.u8() % 4;
        let mut at = first_height.max(v20);
        for _ in 0..changes {
            at += u32::from(r.u8());
            if at > tip {
                break;
            }
            let n = r.u8() % 4;
            let mut set = Vec::new();
            for _ in 0..n {
                let bit = [28, 13, 14, 10, 11, 12, 0][usize::from(r.u8() % 7)];
                let height = at.saturating_sub(u32::from(r.u8()));
                if !set.iter().any(|&(b, _)| b == bit) {
                    set.push((bit, height as i32));
                }
            }
            signals.push((at, set));
            at += 1;
        }
        let mut queries = Vec::new();
        while !r.is_empty() && queries.len() < 64 {
            let op = r.u8();
            let pos = POSITIONS[usize::from(op >> 4) % 3];
            let height = match r.u8() % 4 {
                0 => None,
                1 => Some(tip),
                _ => {
                    let back = u32::from(r.u16());
                    Some(tip.saturating_sub(back % (total + 1)))
                }
            };
            queries.push(match op % 5 {
                0 => Query::State(pos, height),
                1 => Query::StateSinceHeight(pos, height),
                2 => Query::Statistics(pos, height),
                3 => Query::ComputeBlockVersion(height),
                _ => Query::SignalsStage(height),
            });
        }
        Some(Self {
            network,
            args,
            first_height,
            headers,
            signals,
            queries,
        })
    }
}

/// `-vbparams` after the name: `start:timeout:min_activation_height:window:threshold_start` and
/// on most inputs `:threshold_min:falloff:use_ehf`.
fn vbparams(r: &mut Reader<'_>) -> String {
    let time = |r: &mut Reader<'_>| -> i64 {
        match r.u8() % 6 {
            0 => Bip9Deployment::ALWAYS_ACTIVE,
            1 => Bip9Deployment::NEVER_ACTIVE,
            2 => Bip9Deployment::NO_TIMEOUT,
            3 => 0,
            _ => i64::from(BASE_TIME) + i64::from(r.u16()) * 150,
        }
    };
    let start = time(r);
    let timeout = time(r);
    let min_height = u32::from(r.u16() % 2048);
    let window = 1 + i64::from(r.u8() % 48);
    let threshold_start = i64::from(r.u8()) % (window + 2);
    let mut out = format!("{start}:{timeout}:{min_height}:{window}:{threshold_start}");
    if !r.u8().is_multiple_of(4) {
        let threshold_min = i64::from(r.u8()) % (window + 2);
        let falloff = match r.u8() % 4 {
            0 => 0,
            1 => 1,
            _ => i64::from(r.u8() % 16),
        };
        let use_ehf = r.u8() % 2;
        out.push_str(&format!(":{threshold_min}:{falloff}:{use_ehf}"));
    }
    out
}

/// A header version: the top bits with deployments' bits, the old versions, or arbitrary.
fn version(r: &mut Reader<'_>) -> i32 {
    match r.u8() % 8 {
        0 => 4,
        1 => r.u32() as i32,
        2 => VERSIONBITS_TOP_BITS,
        3 => 0x6000_0000,
        _ => {
            let bits = r.u8();
            let mut v = VERSIONBITS_TOP_BITS;
            for (i, bit) in [28, 13, 14].into_iter().enumerate() {
                if bits & (1 << i) != 0 {
                    v |= 1 << bit;
                }
            }
            v
        }
    }
}

/// The scenario's headers as 80-byte headers, each linked to the previous one's hash by `hash`
/// (X11), the zero hash before the first. `bits` is regtest's powLimit; Core's version-bits code
/// reads only the version and time.
pub fn headers_80(scenario: &Scenario, hash: impl Fn(&[u8; 80]) -> [u8; 32]) -> Vec<[u8; 80]> {
    let mut prev = [0u8; 32];
    let mut out = Vec::new();
    for (version, time) in scenario.header_fields() {
        let mut h = [0u8; 80];
        h[0..4].copy_from_slice(&version.to_le_bytes());
        h[4..36].copy_from_slice(&prev);
        h[68..72].copy_from_slice(&time.to_le_bytes());
        h[72..76].copy_from_slice(&0x207f_ffffu32.to_le_bytes());
        prev = hash(&h);
        out.push(h);
    }
    out
}
