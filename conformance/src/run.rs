//! Drivers: stream blocks from the RPC oracle or from blk files and check them in parallel.
//!
//! Both drivers work in units: RPC mode in chunks of heights aligned to the chunk size, blk
//! mode in whole files. `--from`/`--to` select the units that contain the requested heights,
//! and those units are always checked whole. A finished unit is a checkpoint bound to the
//! run's [`Identity`](crate::report::Identity) and to the blocks it covered
//! ([`Coverage`]). Before checking, each driver invalidates earlier checkpoints that no longer
//! hold: in RPC mode, a chunk whose last block is no longer on the oracle's chain (a reorg);
//! in blk mode, a file whose selected blocks, reference hashes or generation changed. Each
//! driver returns the gate's scope: the units the requested range needs, whether this run
//! checked them or an earlier one did.
//!
//! The oracle can reorganise at any moment of an RPC run, also between the separate calls
//! that read the scope's boundaries. RPC mode therefore pins one block at the start, the top
//! of the scope, and a gate only passes when the results it counts form one chain of linked
//! blocks ending in it, and the pinned block is still on the oracle's chain at the end
//! ([`Snapshot`]).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context as _, Result, anyhow, bail};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::blkfile::{self, Chain, Record};
use crate::check::{BlockJob, Context, H256, OracleBlock, check_block, parse_rev_hex, rev_hex};
use crate::report::{Coverage, Fnv128, Mismatch, Reporter, Requirement, Side, Stats, UnitDone};
use crate::rpc::Rpc;
use crate::split;

/// Live progress shared by worker threads.
struct Progress {
    start: Instant,
    blocks: AtomicU64,
    bytes: AtomicU64,
    mismatches: AtomicU64,
    units_done: AtomicUsize,
    units_total: usize,
    failed: AtomicUsize,
}

impl Progress {
    fn new(units_total: usize) -> Self {
        Self {
            start: Instant::now(),
            blocks: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            mismatches: AtomicU64::new(0),
            units_done: AtomicUsize::new(0),
            units_total,
            failed: AtomicUsize::new(0),
        }
    }

    fn unit_done(&self, unit: &str, stats: &Stats) {
        self.blocks.fetch_add(stats.blocks, Ordering::Relaxed);
        self.bytes.fetch_add(stats.bytes, Ordering::Relaxed);
        self.mismatches.fetch_add(stats.mismatches, Ordering::Relaxed);
        let done = self.units_done.fetch_add(1, Ordering::Relaxed) + 1;
        let every = (self.units_total / 200).max(1);
        if done.is_multiple_of(every) || done == self.units_total {
            let secs = self.start.elapsed().as_secs_f64().max(1e-3);
            let blocks = self.blocks.load(Ordering::Relaxed);
            eprintln!(
                "[{done}/{}] {unit}: {blocks} blocks, {:.0} blocks/s, {:.1} MB/s, {} mismatches",
                self.units_total,
                blocks as f64 / secs,
                self.bytes.load(Ordering::Relaxed) as f64 / secs / 1e6,
                self.mismatches.load(Ordering::Relaxed),
            );
        }
    }
}

fn harness_error(
    ctx: &Context,
    height: Option<u32>,
    block_hash: Option<String>,
    check: &str,
    detail: String,
) -> Mismatch {
    Mismatch {
        net: ctx.net.clone(),
        source: ctx.source.to_string(),
        height,
        block_hash,
        tx_index: None,
        txid: None,
        check: check.to_string(),
        side: Side::Harness,
        field: None,
        expected: None,
        actual: None,
        detail,
        tx_hex: None,
        unit: None,
        run: None,
    }
}

/// Outcome of a run: totals for the units checked in this run, how many failed, the
/// requirements of the gate (see [`Reporter::gate`]), and in RPC mode the block the run
/// pinned.
#[derive(Debug, Default)]
pub struct RunResult {
    pub stats: Stats,
    pub failed_units: usize,
    pub scope: Vec<Requirement>,
    /// RPC mode: the gate must also pass [`Snapshot::problems`].
    pub snapshot: Option<Snapshot>,
}

/// The chain an RPC run is judged on: the chain of blocks ending in `hash`, the oracle's block
/// at `height` (the scope's last height) when the run started. With the default `--to`, that
/// is the oracle's tip. The scope's last unit must end in it.
///
/// Block hashes commit to their parents, so the chain ending in `hash` never changes. See
/// [`Snapshot::problems`] for how a gate's results are held to it.
#[derive(Debug)]
pub struct Snapshot {
    pub height: u32,
    pub hash: String,
}

impl Snapshot {
    /// Why the unit results a gate counts (`counted`, from [`Reporter::gate`]) are not one
    /// chain ending in the pinned block, or the pinned block is no longer the oracle's; empty
    /// when neither. The gate may only pass when this is empty.
    ///
    /// Every counted result of a unit in `scope` must check exactly the requirement's heights
    /// and end in its end hash (the last requirement's is the pinned hash), and, in height
    /// order, the parent of each unit's first block must be the previous unit's end hash. The
    /// counted results are then all on the chain ending in the pinned block, whatever the
    /// oracle answered between its calls: blocks link to each other within a unit, and a
    /// result whose oracle hashes or parents disagree with its block bytes has failing
    /// mismatches of its own. Last, `getblockhash` at the pinned height must still return the
    /// pinned hash, so that chain is the oracle's when the gate is decided. The oracle's tip
    /// growing above the pinned height changes none of this.
    pub fn problems(
        &self,
        rpc: &Rpc,
        scope: &[Requirement],
        counted: &[UnitDone],
    ) -> Result<Vec<String>> {
        let mut by_unit: HashMap<&str, Vec<&UnitDone>> = HashMap::new();
        for u in counted {
            by_unit.entry(u.unit.as_str()).or_default().push(u);
        }
        let mut problems = Vec::new();
        let mut below: Option<(&str, &str)> = None;
        for req in scope {
            let Coverage::Heights {
                lo,
                hi,
                end_hash,
                ..
            } = &req.coverage
            else {
                unreachable!("RPC requirements are heights");
            };
            let results = by_unit.get(req.unit.as_str()).map_or(&[][..], Vec::as_slice);
            if results.is_empty() {
                problems.push(format!("{}: no result for heights {lo}..={hi}", req.unit));
            }
            for r in results {
                match &r.coverage {
                    Some(Coverage::Heights {
                        lo: l,
                        hi: h,
                        prev_hash,
                        end_hash: e,
                    }) if l == lo && h == hi && e == end_hash => {
                        if let Some((unit, end)) = below
                            && prev_hash.as_deref() != Some(end)
                        {
                            let parent = prev_hash.as_deref().unwrap_or("none");
                            problems.push(format!(
                                "{}: the parent of block {lo} is {parent}, but {unit} ends in {end}",
                                req.unit
                            ));
                        }
                    }
                    other => problems.push(format!(
                        "{}: a result of run {} checked {other:?}, not heights {lo}..={hi} \
                         ending in {end_hash}",
                        req.unit, r.run
                    )),
                }
            }
            below = Some((&req.unit, end_hash));
        }
        let now = block_hash(rpc, self.height)?;
        if now != self.hash {
            problems.push(format!(
                "the oracle's block at height {} was {} when the run started and is now {now}",
                self.height, self.hash
            ));
        }
        Ok(problems)
    }
}

/// Records a finished unit (or counts it as failed) and updates the totals.
fn finish_unit(
    reporter: &Reporter,
    progress: &Progress,
    totals: &Mutex<Stats>,
    unit: &str,
    done: Result<(Coverage, Stats)>,
    started: Instant,
) {
    let stats = match done {
        Ok((coverage, stats)) => {
            let secs = started.elapsed().as_secs_f64();
            match reporter.record_unit(unit, coverage, stats.clone(), secs) {
                Ok(()) => totals.lock().expect("totals poisoned").merge(&stats),
                Err(err) => {
                    eprintln!("{unit}: cannot record progress: {err:#}");
                    progress.failed.fetch_add(1, Ordering::Relaxed);
                }
            }
            stats
        }
        Err(err) => {
            eprintln!("{unit}: failed: {err:#}");
            progress.failed.fetch_add(1, Ordering::Relaxed);
            Stats::default()
        }
    };
    progress.unit_done(unit, &stats);
}

fn finish_run(progress: Progress, totals: Mutex<Stats>, scope: Vec<Requirement>) -> RunResult {
    let failed_units = progress.failed.load(Ordering::Relaxed);
    if failed_units > 0 {
        eprintln!("{failed_units} units failed; re-run with --resume to retry them");
    }
    RunResult {
        stats: totals.into_inner().expect("totals poisoned"),
        failed_units,
        scope,
        snapshot: None,
    }
}

/// Indices of the requirements of `scope` that still need checking: all of them, or with
/// `resume` those no valid checkpoint covers.
fn to_do(scope: &[Requirement], checkpoints: &[UnitDone], resume: bool) -> Vec<usize> {
    let todo: Vec<usize> = (0..scope.len())
        .filter(|&i| {
            !resume || !Reporter::is_covered(checkpoints, &scope[i].unit, &scope[i].coverage)
        })
        .collect();
    eprintln!("{} units in scope, {} to check", scope.len(), todo.len());
    todo
}

/// Options for RPC mode.
#[derive(Debug, Clone)]
pub struct RpcOptions {
    pub from: u32,
    pub to: Option<u32>,
    pub chunk: u32,
    pub batch: usize,
    /// Cross-check every Nth block against `getblock 2` (0 disables).
    pub verbose_every: u32,
    /// Also cross-check every block containing one of these special tx types.
    pub verbose_types: BTreeSet<u16>,
    pub resume: bool,
}

impl RpcOptions {
    /// The comparison policy, part of the checkpoint identity: which blocks get the
    /// `getblock 2` cross-check.
    pub fn policy(&self) -> String {
        format!("rpc verbose_every={} verbose_types={:?}", self.verbose_every, self.verbose_types)
    }
}

/// Which blocks of a batch get the `getblock 2` cross-check.
fn wants_verbose(opts: &RpcOptions, height: u32, raw: &[u8]) -> bool {
    if opts.verbose_every > 0 && height.is_multiple_of(opts.verbose_every) {
        return true;
    }
    if opts.verbose_types.is_empty() {
        return false;
    }
    match split::split_block(raw) {
        Ok(s) => {
            s.txs.iter().any(|t| t.payload.is_some() && opts.verbose_types.contains(&t.tx_type))
        }
        Err(_) => true,
    }
}

/// The ends of the linked blocks a unit has checked so far: the parent of its first block
/// (`None` for genesis) and the hash of its last.
type Link = Option<(Option<H256>, H256)>;

/// Checks the blocks at `heights`, which must link to each other and to the blocks `link`
/// ends in, and extends `link` by them. A broken link means the oracle's chain changed during
/// the unit: the unit fails and is retried by the next run.
fn check_rpc_batch(
    rpc: &Rpc,
    ctx: &Context,
    opts: &RpcOptions,
    heights: &[u32],
    link: &mut Link,
    stats: &mut Stats,
) -> Result<Vec<Mismatch>> {
    let hash_params: Vec<Value> = heights.iter().map(|h| json!([h])).collect();
    let hashes = rpc.batch("getblockhash", &hash_params)?;
    let raw_params: Vec<Value> = hashes.iter().map(|h| json!([h, 0])).collect();
    let info_params: Vec<Value> = hashes.iter().map(|h| json!([h, 1])).collect();
    let raws = rpc.batch("getblock", &raw_params)?;
    let infos = rpc.batch("getblock", &info_params)?;

    let mut blocks = Vec::with_capacity(heights.len());
    for (((h, hash), raw), info) in heights.iter().zip(&hashes).zip(&raws).zip(&infos) {
        let hex = raw.as_str().ok_or_else(|| anyhow!("getblock {h} 0: not a string"))?;
        let bytes = hex_conservative::decode_to_vec(hex)
            .map_err(|e| anyhow!("getblock {h} 0: bad hex: {e}"))?;
        let oracle = OracleBlock::from_json(info)
            .ok_or_else(|| anyhow!("getblock {h} 1: unexpected shape"))?;
        if hash.as_str().and_then(parse_rev_hex) != Some(oracle.hash) {
            bail!("getblock {h} 1 returned block {} instead of {hash}", rev_hex(&oracle.hash));
        }
        match link {
            Some((_, last)) if oracle.prev != Some(*last) => {
                bail!("block {h} does not link to the block before it: the chain changed (reorg?)")
            }
            Some((_, last)) => *last = oracle.hash,
            None => *link = Some((oracle.prev, oracle.hash)),
        }
        blocks.push((*h, bytes, oracle));
    }

    let verbose_idx: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, (h, b, _))| wants_verbose(opts, *h, b))
        .map(|(i, _)| i)
        .collect();
    let verbose_params: Vec<Value> =
        verbose_idx.iter().map(|&i| json!([rev_hex(&blocks[i].2.hash), 2])).collect();
    let verbose = rpc.batch("getblock", &verbose_params)?;
    let mut verbose_for = vec![None; blocks.len()];
    for (&i, v) in verbose_idx.iter().zip(&verbose) {
        verbose_for[i] = Some(v);
    }

    let mut out = Vec::new();
    for (i, (h, bytes, oracle)) in blocks.iter().enumerate() {
        if oracle.height != *h {
            let detail = format!("getblockhash {h} returned a block at height {}", oracle.height);
            let m = harness_error(ctx, Some(*h), None, "oracle.height", detail);
            stats.count(std::slice::from_ref(&m));
            out.push(m);
        }
        let job = BlockJob {
            bytes,
            height: Some(*h),
            stale: false,
            expected_hash: None,
            oracle: Some(oracle),
            verbose: verbose_for[i],
        };
        out.extend(check_block(ctx, &job, stats));
    }
    Ok(out)
}

/// Block hashes (display hex) of the oracle's chain at `heights`.
fn block_hashes(rpc: &Rpc, heights: &BTreeSet<u32>) -> Result<HashMap<u32, String>> {
    let heights: Vec<u32> = heights.iter().copied().collect();
    let mut out = HashMap::with_capacity(heights.len());
    for part in heights.chunks(1000) {
        let params: Vec<Value> = part.iter().map(|h| json!([h])).collect();
        for (h, v) in part.iter().zip(rpc.batch("getblockhash", &params)?) {
            let hash = v.as_str().ok_or_else(|| anyhow!("getblockhash {h}: {v}"))?;
            out.insert(*h, hash.to_string());
        }
    }
    Ok(out)
}

/// The hash at `height` of the oracle's chain.
fn block_hash(rpc: &Rpc, height: u32) -> Result<String> {
    let v = rpc.call("getblockhash", json!([height]))?;
    v.as_str().map(str::to_string).ok_or_else(|| anyhow!("getblockhash {height}: {v}"))
}

/// The oracle's tip: its best block's height and hash. The height comes from that block's
/// header, so the two agree even if the tip moves between the calls.
pub fn oracle_tip(rpc: &Rpc) -> Result<(u32, String)> {
    let hash = rev_hex(&oracle_best_hash(rpc)?);
    let header = rpc.call("getblockheader", json!([hash]))?;
    let height = header.get("height").and_then(Value::as_u64).context("getblockheader height")?;
    Ok((u32::try_from(height)?, hash))
}

/// The oracle's version (`getnetworkinfo`), part of the identity of RPC-mode results.
pub fn oracle_version(rpc: &Rpc) -> Result<String> {
    let v = rpc.call("getnetworkinfo", json!([]))?;
    let version = v.get("version").and_then(Value::as_u64).context("getnetworkinfo version")?;
    let sub = v.get("subversion").and_then(Value::as_str).unwrap_or_default();
    Ok(format!("{sub} {version}"))
}

/// The oracle's tip ([`oracle_tip`]) and the last height of `[from, to]`, rejecting a range
/// that is inverted or reaches above the tip.
pub fn rpc_range(rpc: &Rpc, from: u32, to: Option<u32>) -> Result<((u32, String), u32)> {
    let (tip, tip_hash) = oracle_tip(rpc)?;
    let to = to.unwrap_or(tip);
    if from > to {
        bail!("--from {from} is above --to {to}");
    }
    if to > tip {
        bail!("--to {to} is above the oracle's tip {tip}");
    }
    Ok(((tip, tip_hash), to))
}

/// Streams the chunks containing `[from, to]` from the oracle over RPC.
pub fn run_rpc(
    rpc: &Rpc,
    ctx: &Context,
    opts: &RpcOptions,
    reporter: &Reporter,
) -> Result<RunResult> {
    let ((tip, tip_hash), to) = rpc_range(rpc, opts.from, opts.to)?;
    let chunk = opts.chunk;
    let units: Vec<(u32, u32)> = (opts.from / chunk..=to / chunk)
        .map(|k| (k * chunk, (k * chunk).saturating_add(chunk - 1).min(tip)))
        .collect();
    // Pin the top of the scope before reading anything else.
    let top = units.last().expect("a range has a unit").1;
    let pin = Snapshot {
        height: top,
        hash: if top == tip {
            tip_hash
        } else {
            block_hash(rpc, top)?
        },
    };

    // Earlier checkpoints whose last block is no longer on the oracle's chain are invalid.
    let checkpoints = reporter.checkpoints()?;
    let mut heights: BTreeSet<u32> = units.iter().map(|u| u.1).collect();
    for c in &checkpoints {
        if let Some(Coverage::Heights {
            hi,
            ..
        }) = &c.coverage
            && *hi <= tip
        {
            heights.insert(*hi);
        }
    }
    heights.remove(&top);
    let mut hashes = block_hashes(rpc, &heights)?;
    hashes.insert(top, pin.hash.clone());
    let mut valid = Vec::with_capacity(checkpoints.len());
    for c in checkpoints {
        match &c.coverage {
            Some(Coverage::Heights {
                hi,
                end_hash,
                ..
            }) if hashes.get(hi) != Some(end_hash) => {
                let now = hashes.get(hi).map_or("above the tip", String::as_str);
                let reason = format!("block {hi} was {end_hash}, is now {now}");
                eprintln!("invalidating {} of run {}: {reason}", c.unit, c.run);
                reporter.invalidate(&c, &reason)?;
            }
            _ => valid.push(c),
        }
    }

    let scope: Vec<Requirement> = units
        .iter()
        .map(|&(lo, hi)| Requirement {
            unit: format!("rpc:{lo}"),
            coverage: Coverage::Heights {
                lo,
                hi,
                prev_hash: None,
                end_hash: hashes[&hi].clone(),
            },
        })
        .collect();
    eprintln!(
        "rpc mode: {} heights {}..={to} (tip {tip}), pinned block {top} {}",
        ctx.net, opts.from, pin.hash
    );
    let todo = to_do(&scope, &valid, opts.resume);
    let progress = Progress::new(todo.len());
    let totals = Mutex::new(Stats::default());
    todo.par_iter().for_each(|&u| {
        let (lo, hi) = units[u];
        let unit = &scope[u].unit;
        let started = Instant::now();
        let done = (|| {
            let mut stats = Stats::default();
            let mut link = None;
            let heights: Vec<u32> = (lo..=hi).collect();
            for batch in heights.chunks(opts.batch) {
                let m = check_rpc_batch(rpc, ctx, opts, batch, &mut link, &mut stats)
                    .with_context(|| format!("batch at {}", batch[0]))?;
                reporter.record_mismatches(unit, &m, &mut stats)?;
            }
            let (prev, end) = link.expect("a unit has blocks");
            Ok((
                Coverage::Heights {
                    lo,
                    hi,
                    prev_hash: prev.as_ref().map(rev_hex),
                    end_hash: rev_hex(&end),
                },
                stats,
            ))
        })();
        finish_unit(reporter, &progress, &totals, unit, done, started);
    });
    Ok(RunResult {
        snapshot: Some(pin),
        ..finish_run(progress, totals, scope)
    })
}

/// Options for blk-file mode.
#[derive(Debug, Clone)]
pub struct BlkOptions {
    pub blocks_dir: PathBuf,
    pub magic: [u8; 4],
    /// The network's genesis block hash, to pick the right root.
    pub genesis: Option<H256>,
    pub from: u32,
    pub to: Option<u32>,
    pub include_stale: bool,
    pub resume: bool,
    /// Best block hash from the oracle, to anchor the chain and check the tip hash.
    pub anchor: Option<H256>,
}

impl BlkOptions {
    /// The comparison policy, part of the checkpoint identity.
    pub fn policy(&self) -> String {
        format!("blk include_stale={} anchored={}", self.include_stale, self.anchor.is_some())
    }
}

/// Signature of what is checked in one file: its generation, and for every selected record
/// its location, both libraries' header hashes, its `prev_hash`, height, best-chain
/// membership and the reference hash it is checked against. A resumed run re-checks a file
/// whose signature changed.
fn selection_signature(
    generation: [u64; 3],
    records: &[Record],
    selected: &[usize],
    height: &[Option<u32>],
    on_best: &[bool],
    expected: &[Option<H256>],
) -> String {
    let mut h = Fnv128::default();
    for g in generation {
        h.eat(&g.to_le_bytes());
    }
    for &i in selected {
        let r = &records[i];
        h.eat(&r.offset.to_le_bytes()).eat(&r.len.to_le_bytes()).eat(&r.prev);
        h.eat(r.dc_hash.as_ref().map_or(&[][..], |x| x))
            .eat(r.bs_hash.as_ref().map_or(&[][..], |x| x));
        h.eat(&height[i].unwrap_or(u32::MAX).to_le_bytes()).eat(&[u8::from(on_best[i])]);
        h.eat(expected[i].as_ref().map_or(&[][..], |x| x));
    }
    format!("{}-{}", selected.len(), h.hex())
}

/// What dashd's block index says about one block (`getblockheader`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedHeader {
    pub height: u32,
    pub prev: H256,
    /// On dashd's active chain (`confirmations` >= 1).
    pub active: bool,
    /// dashd has the block's data, not just its header (`nTx` > 0).
    pub has_data: bool,
}

/// Read-only view of dashd's block index. blk mode asks it what a record that is not linked
/// to genesis is (see [`Unlinked`]).
pub trait BlockIndex {
    /// The block's index entry; `None` if dashd does not know the hash.
    fn header(&self, hash: &H256) -> Result<Option<IndexedHeader>>;
    /// The active chain's block at `height`; `None` above dashd's tip.
    fn active_at(&self, height: u32) -> Result<Option<H256>>;
}

/// dashd's JSON-RPC error codes the [`BlockIndex`] answers rely on.
const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;
const RPC_INVALID_PARAMETER: i64 = -8;

impl BlockIndex for Rpc {
    fn header(&self, hash: &H256) -> Result<Option<IndexedHeader>> {
        let v = match self.try_call("getblockheader", json!([rev_hex(hash), true]))? {
            Ok(v) => v,
            Err(RPC_INVALID_ADDRESS_OR_KEY) => return Ok(None),
            Err(code) => bail!("getblockheader {}: error {code}", rev_hex(hash)),
        };
        let height = v["height"].as_u64().and_then(|h| u32::try_from(h).ok());
        // Genesis has no previousblockhash.
        let prev = match v.get("previousblockhash") {
            None => Some([0; 32]),
            Some(p) => p.as_str().and_then(parse_rev_hex),
        };
        match (height, prev, v["confirmations"].as_i64(), v["nTx"].as_u64()) {
            (Some(height), Some(prev), Some(c), Some(n_tx)) => Ok(Some(IndexedHeader {
                height,
                prev,
                active: c >= 1,
                has_data: n_tx > 0,
            })),
            _ => bail!("getblockheader {}: unexpected reply {v}", rev_hex(hash)),
        }
    }

    fn active_at(&self, height: u32) -> Result<Option<H256>> {
        match self.try_call("getblockhash", json!([height]))? {
            Ok(v) => v
                .as_str()
                .and_then(parse_rev_hex)
                .map(Some)
                .ok_or_else(|| anyhow!("getblockhash {height}: {v}")),
            Err(RPC_INVALID_PARAMETER) => Ok(None),
            Err(code) => bail!("getblockhash {height}: error {code}"),
        }
    }
}

/// What a blk record that is not linked to genesis is, by dashd's block index.
///
/// dashd stores blocks in arrival order. During initial block download it fetches up to
/// `BLOCK_DOWNLOAD_WINDOW` (1024) blocks ahead of its tip from several peers, so a block is
/// often written before its parent; it waits in `m_blocks_unlinked` until the parent's data
/// arrives. A block of a fork can also be stored while its parent's data never is. dashd's
/// own `-reindex` (`LoadExternalBlockFile`) defers a record whose parent is unknown and skips
/// it if the parent never shows up. Those two cases are expected data; anything else is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unlinked {
    /// Above the files' best tip and, by dashd's index, a descendant of it (connected by
    /// dashd since, or still waiting for a parent's data): downloaded ahead of its parent,
    /// which was not written when the files were indexed. Blocks above the tip are never
    /// checked; a later run, with a higher tip, links (or reclassifies) it.
    Ahead {
        height: u32,
    },
    /// Not [`Unlinked::Ahead`], off dashd's active chain, and its parent is another unlinked
    /// record or has no block data in dashd: a fork block that dashd cannot connect either.
    Stale {
        height: u32,
        hash: H256,
    },
    /// Anything else (a failing finding): a hash dashd does not know or the libraries
    /// disagree on, another parent than dashd's, a block on dashd's active chain that is not
    /// a descendant of the tip above it, or a parent dashd has data for that is missing from
    /// the files.
    Unexplained(String),
}

/// Classifies unlinked records ([`Unlinked`]) against dashd's block index, caching the
/// headers it reads.
pub struct Classifier<'a> {
    index: &'a dyn BlockIndex,
    tip: (u32, H256),
    unlinked: HashSet<H256>,
    headers: HashMap<H256, Option<IndexedHeader>>,
}

impl<'a> Classifier<'a> {
    /// `tip` is the files' best tip (height and hash); `unlinked` holds the hashes of every
    /// unlinked record, so a block whose parent is one of them belongs to the same branch.
    pub fn new(index: &'a dyn BlockIndex, tip: (u32, H256), unlinked: HashSet<H256>) -> Self {
        Self {
            index,
            tip,
            unlinked,
            headers: HashMap::new(),
        }
    }

    fn header(&mut self, hash: &H256) -> Result<Option<IndexedHeader>> {
        if let Some(h) = self.headers.get(hash) {
            return Ok(h.clone());
        }
        let h = self.index.header(hash)?;
        self.headers.insert(*hash, h.clone());
        Ok(h)
    }

    /// The parent of a block dashd knows, which dashd's index always holds.
    fn parent(&mut self, prev: &H256) -> Result<IndexedHeader> {
        self.header(prev)?.ok_or_else(|| {
            anyhow!("dashd's index has no parent {} of a block it knows", rev_hex(prev))
        })
    }

    /// Whether the block `hash` (known to dashd, at `h`) descends from the files' tip, by
    /// dashd's index: from the first block on the way down that is on the active chain, by
    /// `getblockhash`; below that, by walking parents. A block dashd has not connected yet
    /// is at most `BLOCK_DOWNLOAD_WINDOW` blocks above dashd's tip, which is at or above the
    /// files' tip, so the walk is short.
    fn descends_from_tip(&mut self, mut hash: H256, mut h: IndexedHeader) -> Result<bool> {
        let (tip_height, tip_hash) = self.tip;
        loop {
            if h.height < tip_height {
                return Ok(false);
            }
            if h.height == tip_height {
                return Ok(hash == tip_hash);
            }
            if h.active {
                return Ok(self.index.active_at(tip_height)? == Some(tip_hash));
            }
            hash = h.prev;
            h = self.parent(&hash)?;
        }
    }

    /// Classifies record `r`.
    pub fn classify(&mut self, r: &Record) -> Result<Unlinked> {
        let unexplained = |why: String| Ok(Unlinked::Unexplained(why));
        let hash = match (r.dc_hash, r.bs_hash) {
            (Some(a), Some(b)) if a == b => a,
            _ => return unexplained("the libraries do not agree on its hash".into()),
        };
        let Some(h) = self.header(&hash)? else {
            return unexplained("dashd does not know this block".into());
        };
        if h.prev != r.prev {
            return unexplained(format!("dashd's parent is {}", rev_hex(&h.prev)));
        }
        let (tip_height, tip_hash) = self.tip;
        if h.height > tip_height && self.descends_from_tip(hash, h.clone())? {
            return Ok(Unlinked::Ahead {
                height: h.height,
            });
        }
        if h.active {
            let why = if h.height <= tip_height {
                format!(
                    "dashd's active chain holds it at height {}, at or below the best tip {}",
                    h.height, tip_height
                )
            } else {
                format!(
                    "dashd's active chain no longer holds the best tip {} {} (reorganised; \
                     re-run)",
                    tip_height,
                    rev_hex(&tip_hash)
                )
            };
            return unexplained(why);
        }
        if !self.unlinked.contains(&r.prev) && self.parent(&r.prev)?.has_data {
            return unexplained(
                "dashd has its parent's data, which no record in the files hashes to (written \
                 after the files were indexed, or hashed wrongly by both libraries; re-run)"
                    .into(),
            );
        }
        Ok(Unlinked::Stale {
            height: h.height,
            hash,
        })
    }
}

/// Checks blocks straight from `blk*.dat`. With `index` (dashd's block index, normally the
/// oracle), records not linked to genesis are classified ([`Unlinked`]); without it they all
/// fail the gate.
pub fn run_blk(
    ctx: &Context,
    opts: &BlkOptions,
    index: Option<&dyn BlockIndex>,
    reporter: &Reporter,
) -> Result<RunResult> {
    if let Some(to) = opts.to
        && opts.from > to
    {
        bail!("--from {} is above --to {to}", opts.from);
    }
    let started = Instant::now();
    let key = blkfile::xor_key(&opts.blocks_dir)?;
    let files = blkfile::list_files(&opts.blocks_dir)?;
    eprintln!(
        "blk mode: {} files in {} (xor key: {})",
        files.len(),
        opts.blocks_dir.display(),
        key.map_or("none", |_| "yes")
    );
    let (records, issues) = blkfile::index(&files, key, opts.magic)?;
    let chain = Chain::build(&records, opts.anchor.as_ref(), opts.genesis.as_ref())?;
    let tip_height = chain.best.len() as u32 - 1;
    // Second copies of a block never get a height; they are duplicates, not unlinked.
    let orphans: Vec<usize> =
        (0..records.len()).filter(|&i| chain.height[i].is_none() && chain.primary[i]).collect();
    let ahead: Vec<usize> =
        (0..records.len()).filter(|&i| chain.height[i].is_some_and(|h| h > tip_height)).collect();
    eprintln!(
        "indexed {} records in {:.1}s: best height {tip_height}, {} unlinked, {} beyond the anchor, \
         {} duplicates, {} scan issues",
        records.len(),
        started.elapsed().as_secs_f64(),
        orphans.len(),
        ahead.len(),
        chain.duplicates,
        issues.len()
    );

    // Index-level findings go into their own unit, re-done (and replaced) by every run.
    let newest_file = files.last().map(|f| f.0);
    let mut index_mismatches: Vec<Mismatch> = issues
        .iter()
        .map(|i| {
            // Discarded bytes at the end of the newest file may be a block being written; a
            // later run re-scans them. Anything else is damage. Both fail the gate: blocks
            // may have been skipped.
            let (check, note) = if i.at_end && Some(i.file) == newest_file {
                ("blk.scan_tail", " (possibly still being written; re-run)")
            } else {
                ("blk.scan", "")
            };
            let detail = format!("blk{:05}.dat@{}: {}{note}", i.file, i.offset, i.reason);
            harness_error(ctx, None, None, check, detail)
        })
        .collect();
    // Unlinked records dashd also leaves off its active chain are informational. Stale ones
    // get dashd's height and hash, and are checked like any stale block.
    let mut height = chain.height.clone();
    let mut stale_refs: Vec<(usize, H256)> = Vec::new();
    let tip_hash = opts.anchor.or_else(|| chain.best.last().and_then(|&t| records[t].any_hash()));
    let tip = tip_hash.map(|h| (tip_height, h));
    let unlinked: HashSet<H256> = orphans.iter().flat_map(|&i| records[i].hashes()).collect();
    let mut classifier = index.zip(tip).map(|(index, tip)| Classifier::new(index, tip, unlinked));
    for &i in &orphans {
        let r = &records[i];
        let class = match &mut classifier {
            Some(c) => c.classify(r)?,
            None => Unlinked::Unexplained("no oracle to classify it".into()),
        };
        let at = format!("blk{:05}.dat@{}", r.file, r.offset);
        let parent = rev_hex(&r.prev);
        let block_hash = r.any_hash().as_ref().map(rev_hex);
        let m = match class {
            Unlinked::Ahead {
                height: h,
            } => {
                let detail = format!(
                    "{at}: parent {parent} not linked in the blk files; by dashd's index a \
                     descendant of the best tip {tip_height}, at height {h} (downloaded ahead \
                     of its parent; checked once linked)"
                );
                harness_error(ctx, Some(h), block_hash, "blk.unlinked_ahead", detail)
            }
            Unlinked::Stale {
                height: h,
                hash,
            } => {
                let checked = opts.include_stale && h <= tip_height;
                if checked {
                    height[i] = Some(h);
                    stale_refs.push((i, hash));
                }
                let detail = format!(
                    "{at}: parent {parent} is another unlinked record or has no data in \
                     dashd; off dashd's active chain at height {h} ({})",
                    if checked {
                        "checked as a stale block at that height"
                    } else {
                        "not checked: needs --include-stale and a height at or below the tip"
                    }
                );
                harness_error(ctx, Some(h), block_hash, "blk.unlinked_stale", detail)
            }
            Unlinked::Unexplained(why) => {
                let detail = format!("{at}: not linked to genesis (parent {parent}): {why}");
                harness_error(ctx, None, block_hash, "blk.orphan", detail)
            }
        };
        index_mismatches.push(m);
    }
    // A requested range the chain does not reach means blocks went unchecked.
    if opts.from > tip_height || opts.to.is_some_and(|t| t > tip_height) {
        let detail = format!(
            "requested heights {}..={} but the best chain in the blk files ends at {tip_height}",
            opts.from,
            opts.to.map_or_else(|| "tip".to_string(), |t| t.to_string())
        );
        index_mismatches.push(harness_error(ctx, None, None, "blk.range", detail));
    }
    if opts.anchor.is_none() {
        eprintln!("warning: no anchor; the best tip's hash is not checked against an oracle");
    }
    let mut index_stats = Stats::default();
    index_stats.count(&index_mismatches);
    reporter.record_mismatches("blk:index", &index_mismatches, &mut index_stats)?;
    let secs = started.elapsed().as_secs_f64();
    reporter.record_unit("blk:index", Coverage::Index, index_stats.clone(), secs)?;

    let to = opts.to.unwrap_or(tip_height).min(tip_height);
    let mut on_best = vec![false; records.len()];
    for &i in &chain.best {
        on_best[i] = true;
    }
    // Expected hash of best-chain block h = raw prev_hash of best-chain block h+1; neither
    // library computed it. The tip's comes from the oracle when anchored.
    let mut expected: Vec<Option<H256>> = vec![None; records.len()];
    for w in chain.best.windows(2) {
        expected[w[0]] = Some(records[w[1]].prev);
    }
    if let (Some(a), Some(&tip)) = (opts.anchor, chain.best.last()) {
        expected[tip] = Some(a);
    }
    for &(i, hash) in &stale_refs {
        expected[i] = Some(hash);
    }

    // Each file's selection: every block up to the best tip, on the best chain (or any, with
    // --include-stale). Blocks above the tip (downloaded but not yet connected, possibly
    // still being written) are never selected. A file is in scope when it holds a selected
    // block in [from, to], and is then checked whole.
    let mut by_file: Vec<Vec<usize>> = vec![Vec::new(); files.len()];
    let mut in_range = vec![false; files.len()];
    let file_pos: HashMap<u32, usize> =
        files.iter().enumerate().map(|(i, (n, _))| (*n, i)).collect();
    for (i, r) in records.iter().enumerate() {
        if let Some(h) = height[i]
            && h <= tip_height
            && (on_best[i] || opts.include_stale)
        {
            let f = file_pos[&r.file];
            by_file[f].push(i);
            in_range[f] |= h >= opts.from && h <= to;
        }
    }
    let mut sigs: HashMap<String, String> = HashMap::new();
    for (f, sel) in by_file.iter().enumerate() {
        if !sel.is_empty() {
            let generation = blkfile::generation(&files[f].1)?;
            let sig = selection_signature(generation, &records, sel, &height, &on_best, &expected);
            sigs.insert(format!("blk:{}", files[f].0), sig);
        }
    }

    // Earlier checkpoints of files whose selection changed are invalid.
    let mut valid = Vec::new();
    for c in reporter.checkpoints()? {
        match &c.coverage {
            Some(Coverage::Selection {
                sig,
            }) if sigs.get(&c.unit) != Some(sig) => {
                reporter.invalidate(&c, "the file's selection or generation changed")?;
            }
            _ => valid.push(c),
        }
    }

    let files_in_scope: Vec<usize> = (0..files.len()).filter(|&f| in_range[f]).collect();
    let file_reqs: Vec<Requirement> = files_in_scope
        .iter()
        .map(|&f| {
            let unit = format!("blk:{}", files[f].0);
            let sig = sigs[&unit].clone();
            Requirement {
                unit,
                coverage: Coverage::Selection {
                    sig,
                },
            }
        })
        .collect();
    let todo = to_do(&file_reqs, &valid, opts.resume);

    let progress = Progress::new(todo.len());
    let totals = Mutex::new(index_stats);
    // Files are checked one block at a time, and each block's mismatches are written as soon
    // as it is checked, so memory stays bounded by the worker count.
    todo.par_iter().for_each(|&u| {
        let (f, req) = (files_in_scope[u], &file_reqs[u]);
        let unit_start = Instant::now();
        let done = File::open(&files[f].1)
            .with_context(|| format!("opening {}", files[f].1.display()))
            .and_then(|file| {
                by_file[f]
                    .par_iter()
                    .map(|&i| {
                        let bytes = blkfile::read_block(&file, &records[i], key, opts.magic)?;
                        let job = BlockJob {
                            bytes: &bytes,
                            height: height[i],
                            stale: !on_best[i],
                            expected_hash: expected[i],
                            oracle: None,
                            verbose: None,
                        };
                        let mut s = Stats::default();
                        let m = check_block(ctx, &job, &mut s);
                        reporter.record_mismatches(&req.unit, &m, &mut s)?;
                        Ok(s)
                    })
                    .try_reduce(Stats::default, |mut a, b| {
                        a.merge(&b);
                        Ok(a)
                    })
            })
            .map(|stats| (req.coverage.clone(), stats));
        finish_unit(reporter, &progress, &totals, &req.unit, done, unit_start);
    });
    let index_req = Requirement {
        unit: "blk:index".to_string(),
        coverage: Coverage::Index,
    };
    let scope = std::iter::once(index_req).chain(file_reqs).collect();
    Ok(finish_run(progress, totals, scope))
}

/// Parses a display-hex block hash from the oracle.
pub fn oracle_best_hash(rpc: &Rpc) -> Result<H256> {
    let v = rpc.call("getbestblockhash", json!([]))?;
    v.as_str().and_then(parse_rev_hex).ok_or_else(|| anyhow!("getbestblockhash: {v}"))
}
