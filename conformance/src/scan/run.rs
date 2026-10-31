//! The scan driver: indexes a `blocks/` directory, runs the classifiers over the best chain and
//! the stale blocks, adds scan (c)'s oracle step, and writes the results.
//!
//! Files are only ever opened for reading (`File::open`, `read_exact_at`); nothing is created in
//! the blocks directory. The caller runs the scan under `ReadOnlyPaths=` as well.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use dashcore::hashes::{Hash, sha256};
use rayon::prelude::*;
use serde_json::{Value, json};

use super::classify::{CClass, c_hit, cl_sequence_hits, classify_c, scan_block};
use super::{
    BlockInput, CRow, CRowKind, Cfg, ClFact, Hit, Net, Scan, ScanSet, V24, bump, merge_counters,
    sort_hits,
};
use crate::blkfile::{self, Chain, Record};
use crate::check::{H256, rev_hex};
use crate::rpc::Rpc;

/// Examples of each hit class kept in `summary.json`.
const EXAMPLES_PER_CLASS: usize = 50;

pub struct ScanOptions<'a> {
    pub net: Net,
    pub blocks_dir: PathBuf,
    pub scans: ScanSet,
    pub magic: [u8; 4],
    pub genesis: H256,
    pub from: u32,
    pub to: Option<u32>,
    /// The oracle's best block, pinning the chain (`blk` mode's anchor).
    pub anchor: Option<H256>,
    /// The oracle, for scan (c)'s block rewards. Without it scan (c) lists its candidates
    /// unclassified.
    pub oracle: Option<&'a Rpc>,
    pub out: PathBuf,
    /// The code identity recorded in `summary.json`.
    pub code_id: String,
}

/// What a scan wrote.
#[derive(Debug)]
pub struct ScanSummary {
    pub summary: Value,
    /// A finding the exit status should reflect: an unframed block, a block file that changed
    /// during the run, or an unexplained subsidy disagreement.
    pub incomplete: Vec<String>,
}

#[derive(Default)]
struct Partial {
    hits: Vec<Hit>,
    counters: BTreeMap<String, u64>,
    cl: Vec<ClFact>,
    c_rows: Vec<CRow>,
    unparsed: Vec<(u32, H256, String)>,
}

impl Partial {
    fn merge(mut self, other: Partial) -> Partial {
        self.hits.extend(other.hits);
        merge_counters(&mut self.counters, other.counters);
        self.cl.extend(other.cl);
        self.c_rows.extend(other.c_rows);
        self.unparsed.extend(other.unparsed);
        self
    }
}

/// One selected record: its index into the record list, height and whether it is stale.
struct Pick {
    record: usize,
    height: u32,
    stale: bool,
}

/// The `v24` deployment state at the anchor tip, from the oracle's `getdeploymentinfo`.
fn v24_from_oracle(rpc: &Rpc, tip: &H256) -> Result<V24> {
    let info = rpc.call("getdeploymentinfo", json!([rev_hex(tip)]))?;
    V24::from_deployment_info(&info).map_err(|e| anyhow::anyhow!("getdeploymentinfo: {e}"))
}

/// `[size, mtime, inode]` of each file.
fn generations(files: &[(u32, PathBuf)]) -> Result<Vec<[u64; 3]>> {
    files.iter().map(|(_, path)| blkfile::generation(path)).collect()
}

/// The digest of the files' generations and their total size.
fn files_digest(files: &[(u32, PathBuf)], generations: &[[u64; 3]]) -> (String, u64) {
    let mut text = String::new();
    let mut total = 0u64;
    for ((n, _), g) in files.iter().zip(generations) {
        total += g[0];
        text.push_str(&format!("{n}:{}:{}:{}\n", g[0], g[1], g[2]));
    }
    (sha256::Hash::hash(text.as_bytes()).to_string(), total)
}

/// `path` with symlinks resolved. A path that does not exist yet resolves through its nearest
/// existing ancestor, which is where creating it would land; a dangling symlink is refused,
/// since writing through it would land wherever it points.
fn resolve_for_guard(path: &Path) -> Result<PathBuf> {
    let mut current = std::path::absolute(path)?;
    let mut tail = Vec::new();
    loop {
        match current.canonicalize() {
            Ok(resolved) => return Ok(tail.iter().rev().fold(resolved, |p, name| p.join(name))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if current.symlink_metadata().is_ok() {
                    bail!("{} is a dangling symlink", current.display());
                }
                let Some(name) = current.file_name().map(ToOwned::to_owned) else {
                    bail!("no existing ancestor of {}", path.display());
                };
                tail.push(name);
                current.pop();
            }
            Err(e) => return Err(e).with_context(|| format!("resolving {}", current.display())),
        }
    }
}

/// `blocks` and, for the usual `<datadir>[/<chain>]/blocks` layout, the directories around it
/// that belong to the node.
fn node_dirs(blocks: &Path, protected: &mut Vec<PathBuf>) {
    protected.push(blocks.to_path_buf());
    if blocks.file_name().is_some_and(|n| n == "blocks")
        && let Some(chain_dir) = blocks.parent()
    {
        protected.push(chain_dir.to_path_buf());
        let is_chain_subdir = chain_dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.starts_with("testnet") || n.starts_with("regtest") || n.starts_with("devnet")
        });
        if is_chain_subdir && let Some(datadir) = chain_dir.parent() {
            protected.push(datadir.to_path_buf());
        }
    }
}

/// The scan only reads the node's data: refuse an output target that resolves, symlinks
/// followed, inside the blocks directory or, for the usual `<datadir>[/<chain>]/blocks`
/// layout, inside the whole datadir. The datadir is taken both from where `blocks_dir` resolves
/// and from the path as given with only its parent chain resolved, since `blocks` itself may be a
/// symlink to a directory elsewhere.
fn refuse_output_inside_datadir(blocks_dir: &Path, out: &Path) -> Result<()> {
    let blocks =
        blocks_dir.canonicalize().with_context(|| format!("resolving {}", blocks_dir.display()))?;
    let mut protected = Vec::new();
    node_dirs(&blocks, &mut protected);
    let given = std::path::absolute(blocks_dir)?;
    if let (Some(parent), Some(name)) = (given.parent(), given.file_name()) {
        let parent =
            parent.canonicalize().with_context(|| format!("resolving {}", parent.display()))?;
        node_dirs(&parent.join(name), &mut protected);
    }
    let target = resolve_for_guard(out)?;
    if let Some(root) = protected.iter().find(|root| target.starts_with(root)) {
        bail!(
            "--out {} resolves to {}, inside {}, which is only read",
            out.display(),
            target.display(),
            root.display()
        );
    }
    Ok(())
}

pub fn run_scan(opts: &ScanOptions<'_>) -> Result<ScanSummary> {
    if let Some(to) = opts.to
        && opts.from > to
    {
        bail!("--from {} is above --to {to}", opts.from);
    }
    refuse_output_inside_datadir(&opts.blocks_dir, &opts.out)?;
    let started = Instant::now();
    let key = blkfile::xor_key(&opts.blocks_dir)?;
    let files = blkfile::list_files(&opts.blocks_dir)?;
    let generations_before = generations(&files)?;
    let (digest, blk_bytes) = files_digest(&files, &generations_before);
    eprintln!(
        "scan {}: {} blk files in {}",
        opts.net.name(),
        files.len(),
        opts.blocks_dir.display()
    );
    let (records, issues) = blkfile::index(&files, key, opts.magic)?;
    let chain = Chain::build(&records, opts.anchor.as_ref(), Some(&opts.genesis))?;
    let tip_height = chain.best.len() as u32 - 1;
    let tip_hash = records[*chain.best.last().expect("genesis at least")]
        .any_hash()
        .context("the best tip has no hash")?;
    eprintln!(
        "indexed {} records in {:.1}s: best height {tip_height}",
        records.len(),
        started.elapsed().as_secs_f64()
    );
    let to = opts.to.unwrap_or(tip_height).min(tip_height);
    if opts.from > to {
        bail!("--from {} is above the chain tip {tip_height}: nothing to scan", opts.from);
    }
    let mut incomplete = Vec::new();
    if let Some(requested) = opts.to.filter(|t| *t > tip_height) {
        incomplete.push(format!("--to {requested} is above the chain tip {tip_height}"));
    }
    let mut on_best = vec![false; records.len()];
    for &i in &chain.best {
        on_best[i] = true;
    }
    let mut unlinked = 0u64;
    let mut ahead = 0u64;
    let mut by_file: BTreeMap<u32, Vec<Pick>> = BTreeMap::new();
    for (i, r) in records.iter().enumerate() {
        if !chain.primary[i] {
            continue;
        }
        match chain.height[i] {
            None => unlinked += 1,
            Some(h) if h > tip_height => ahead += 1,
            Some(h) if h >= opts.from && h <= to => by_file.entry(r.file).or_default().push(Pick {
                record: i,
                height: h,
                stale: !on_best[i],
            }),
            Some(_) => {}
        }
    }

    let mut cfg = Cfg::new(opts.net, opts.scans);
    if let (Some(rpc), true) = (opts.oracle, opts.scans.contains(Scan::D)) {
        match v24_from_oracle(rpc, &tip_hash) {
            Ok(v24) => cfg.v24 = v24,
            Err(e) => incomplete.push(format!("scan (d): the v24 deployment state: {e:#}")),
        }
    }
    let file_paths: BTreeMap<u32, &Path> = files.iter().map(|(n, p)| (*n, p.as_path())).collect();
    let best = BestChain {
        records: &records,
        best: &chain.best,
        paths: &file_paths,
    };
    let work: Vec<(&u32, &Vec<Pick>)> = by_file.iter().collect();
    let parts: Vec<Result<Partial>> = work
        .par_iter()
        .map(|(file_no, picks)| {
            scan_file(&cfg, &best, picks, file_paths[*file_no], key, opts.magic)
        })
        .collect();
    let mut total = Partial::default();
    for p in parts {
        total = total.merge(p?);
    }
    eprintln!("classified in {:.1}s", started.elapsed().as_secs_f64());

    if cfg.v24 == V24::Unknown
        && let Some(n) = total.counters.get("d.v24_undecided").filter(|n| **n > 0)
    {
        incomplete.push(format!(
            "scan (d): the v24 deployment state is unknown, so {n} CbTx at or after V20 were not \
             judged against the unlock-root version rule"
        ));
    }

    // Scan (d)'s sequential rule, over the best chain by height.
    total.cl.sort_by_key(|f| f.height);
    if opts.scans.contains(Scan::D) {
        total.hits.extend(cl_sequence_hits(&cfg, &total.cl));
    }

    // Scan (c): the candidates against the oracle's block rewards.
    total.c_rows.sort_by_key(|r| r.height);
    if opts.scans.contains(Scan::C) {
        total.counters.insert("c.candidates".into(), total.c_rows.len() as u64);
        match opts.oracle {
            Some(rpc) => {
                let prev_bits =
                    prev_bits_of(&total.c_rows, &records, &chain, &file_paths, key, opts.magic)?;
                let (hits, counters, problems) =
                    oracle_step(&cfg, rpc, &total.c_rows, &prev_bits, opts.net)?;
                total.hits.extend(hits);
                merge_counters(&mut total.counters, counters);
                incomplete.extend(problems);
            }
            None => {
                for row in &total.c_rows {
                    total.hits.push(Hit::on_coinbase(
                        Scan::C,
                        "c.candidate_unclassified",
                        row.height,
                        row.block,
                        row.coinbase_txid,
                        json!({"coinbase_value": row.coinbase_value}),
                    ));
                }
                incomplete
                    .push("scan (c) ran without an oracle: candidates are unclassified".into());
            }
        }
    }

    sort_hits(&mut total.hits);
    total.unparsed.sort_by_key(|(h, b, _)| (*h, *b));
    if !total.unparsed.is_empty() {
        incomplete.push(format!(
            "{} blocks could not be framed or do not match their header's merkle root",
            total.unparsed.len()
        ));
    }
    if opts.anchor.is_none() && unlinked > 0 {
        incomplete
            .push(format!("no anchor, and {unlinked} records are not linked to the chain scanned"));
    }
    // A file rewritten under the scan means the pass may have mixed two states. A live node
    // appends to its newest file, so that one may grow; records are never moved.
    let generations_after = generations(&files)?;
    let last = files.len().saturating_sub(1);
    for (i, (b, a)) in generations_before.iter().zip(&generations_after).enumerate() {
        if b != a && !(i == last && a[2] == b[2] && a[0] >= b[0]) {
            incomplete.push(format!(
                "blk file {} changed during the scan ({b:?} -> {a:?}); re-run",
                files[i].0
            ));
        }
    }
    if !issues.is_empty() {
        incomplete.push(format!("{} blk scan issues (bytes skipped)", issues.len()));
        for i in issues.iter().take(20) {
            incomplete.push(format!(
                "blk{:05}.dat@{}: {}{}",
                i.file,
                i.offset,
                i.reason,
                if i.at_end {
                    " (to the end of the file)"
                } else {
                    ""
                }
            ));
        }
    }

    let summary = write_outputs(
        opts,
        &total,
        &SummaryInputs {
            tip: (tip_height, tip_hash),
            to,
            files: files.len(),
            blk_bytes,
            digest,
            unlinked,
            ahead,
            duplicates: chain.duplicates as u64,
            scan_issues: issues.len() as u64,
            v24: cfg.v24.to_json(),
            incomplete: &incomplete,
        },
    )?;
    eprintln!("done in {:.1}s", started.elapsed().as_secs_f64());
    Ok(ScanSummary {
        summary,
        incomplete,
    })
}

/// The best chain, for the median time past of a block's parent.
struct BestChain<'a> {
    records: &'a [Record],
    best: &'a [usize],
    paths: &'a BTreeMap<u32, &'a Path>,
}

impl BestChain<'_> {
    /// `GetMedianTimePast()` of the best-chain block at `height`: the median of the times of it
    /// and the 10 before it (`chain.h`), `None` if a header cannot be read.
    fn median_time_past(&self, height: u32, key: Option<[u8; 8]>, magic: [u8; 4]) -> Option<i64> {
        let from = height.saturating_sub(10);
        let mut times = Vec::new();
        for h in from..=height {
            let r = &self.records[*self.best.get(h as usize)?];
            let file = File::open(self.paths.get(&r.file)?).ok()?;
            let header = blkfile::read_header(&file, r, key, magic).ok()?;
            times.push(i64::from(u32::from_le_bytes(header[68..72].try_into().expect("4 bytes"))));
        }
        times.sort_unstable();
        Some(times[times.len() / 2])
    }
}

fn scan_file(
    cfg: &Cfg,
    chain: &BestChain<'_>,
    picks: &[Pick],
    path: &Path,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<Partial> {
    let records = chain.records;
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut part = Partial::default();
    for p in picks {
        let r = &records[p.record];
        let bytes = blkfile::read_block(&file, r, key, magic)?;
        let hash = r.any_hash().unwrap_or([0; 32]);
        let mtp = || chain.median_time_past(p.height.checked_sub(1)?, key, magic);
        let res = scan_block(
            cfg,
            &BlockInput {
                bytes: &bytes,
                height: p.height,
                hash,
                stale: p.stale,
                mtp_prev: if p.stale {
                    None
                } else {
                    Some(&mtp)
                },
            },
        );
        if let Some(why) = res.unparsed {
            part.unparsed.push((p.height, hash, why));
        }
        // The record's magic sits 8 bytes before the block.
        let record_offset = r.offset - 8;
        let blk = json!({"file": r.file, "record_offset": record_offset});
        part.hits.extend(res.hits.into_iter().map(|mut h| {
            if let Some(d) = h.detail.as_object_mut() {
                d.insert("blk".into(), blk.clone());
            }
            h
        }));
        merge_counters(&mut part.counters, res.counters);
        part.cl.extend(res.cl);
        part.c_rows.extend(res.c_row.map(|mut row| {
            row.blk = Some((r.file, record_offset));
            row
        }));
    }
    Ok(part)
}

/// Scan (c)'s oracle step: the hits, the counters, and the problems that make the run incomplete.
type OracleStep = (Vec<Hit>, BTreeMap<String, u64>, Vec<String>);

/// The reward `R` of each candidate from the oracle: `getblockstats` for `subsidy` and
/// `totalfee`, and `getsuperblockbudget` where a superblock pays more than `R`. One connection,
/// duty-capped at half so it does not load the shared oracle.
fn oracle_step(
    cfg: &Cfg,
    rpc: &Rpc,
    rows: &[CRow],
    prev_bits: &BTreeMap<u32, u32>,
    net: Net,
) -> Result<OracleStep> {
    const BATCH: usize = 100;
    let mut hits = Vec::new();
    let mut counters: BTreeMap<String, u64> = BTreeMap::new();
    let mut problems = Vec::new();
    let subsidy_params = subsidy_params(net)?;
    for chunk in rows.chunks(BATCH) {
        let t = Instant::now();
        let params: Vec<Value> =
            chunk.iter().map(|r| json!([rev_hex(&r.block), ["subsidy", "totalfee"]])).collect();
        let replies = rpc.batch("getblockstats", &params)?;
        for (row, stats) in chunk.iter().zip(&replies) {
            let subsidy = stats["subsidy"].as_i64().context("getblockstats: subsidy")?;
            let fee = stats["totalfee"].as_i64().context("getblockstats: totalfee")?;
            let reward = subsidy + fee;
            // The independent leg: `dash_consensus`'s port of `GetBlockSubsidyInner`.
            if let Some(&bits) = prev_bits.get(&row.height) {
                let v20 = row.height >= cfg.net.v20;
                let ours = dash_consensus::subsidy::block_subsidy_inner(
                    bits,
                    row.height as i32 - 1,
                    v20,
                    &subsidy_params,
                );
                bump(&mut counters, "c.subsidy_checked", 1);
                if ours != Ok(subsidy) {
                    bump(&mut counters, "c.subsidy_mismatch", 1);
                    problems.push(format!(
                        "scan (c): subsidy at {}: oracle {subsidy}, dash_consensus {ours:?}",
                        row.height
                    ));
                    hits.push(Hit {
                        scan: Scan::C,
                        class: "c.subsidy_mismatch",
                        height: row.height,
                        block: row.block,
                        tx_index: None,
                        txid: None,
                        stale: false,
                        detail: json!({"oracle": subsidy, "dash_consensus": format!("{ours:?}")}),
                    });
                }
            }
            let mut class = classify_c(cfg, row, reward, None);
            let mut limit = None;
            if row.kind == CRowKind::Superblock && class != CClass::Within {
                // Superblocks may exceed R by the payments limit.
                let v = rpc.call("getsuperblockbudget", json!([row.height]))?;
                let dash = v.as_f64().context("getsuperblockbudget: number")?;
                let l = (dash * 1e8).round() as i64;
                limit = Some(l);
                class = classify_c(cfg, row, reward, limit);
            }
            let hit = c_hit(row, class, reward, limit);
            bump(&mut counters, hit.class, 1);
            // Within-bound superblocks are only counted; every window block is listed, and so
            // is every overpay.
            if !(class == CClass::Within && row.kind == CRowKind::Superblock) {
                hits.push(hit);
            }
        }
        std::thread::sleep(t.elapsed().min(Duration::from_secs(5)));
    }
    Ok((hits, counters, problems))
}

/// `SubsidyParams` for the network's v24 chain parameters.
fn subsidy_params(net: Net) -> Result<dash_consensus::subsidy::SubsidyParams> {
    use dash_params::{ChainParamsBuilder, Network, Release};
    let kind = match net {
        Net::Mainnet => Network::Main,
        Net::Testnet => Network::Testnet3,
    };
    let params = ChainParamsBuilder::new(kind, Release::V24_0)
        .build()
        .map_err(|e| anyhow::anyhow!("the {} params: {e:?}", net.name()))?;
    let genesis_reward =
        dash_consensus::subsidy::genesis_reward(&dash_consensus::params::genesis_block(&params))
            .map_err(|e| anyhow::anyhow!("genesis reward: {e:?}"))?;
    dash_consensus::subsidy::SubsidyParams::from_chain_params(&params, genesis_reward)
        .map_err(|e| anyhow::anyhow!("subsidy params: {e:?}"))
}

/// `nBits` of the best-chain block below each candidate, read from the blk files' headers.
fn prev_bits_of(
    rows: &[CRow],
    records: &[Record],
    chain: &Chain,
    paths: &BTreeMap<u32, &Path>,
    key: Option<[u8; 8]>,
    magic: [u8; 4],
) -> Result<BTreeMap<u32, u32>> {
    let mut out = BTreeMap::new();
    for row in rows {
        let Some(prev) = row.height.checked_sub(1) else {
            continue;
        };
        let r = &records[chain.best[prev as usize]];
        let file = File::open(paths[&r.file])?;
        let header = blkfile::read_header(&file, r, key, magic)?;
        out.insert(row.height, u32::from_le_bytes(header[72..76].try_into().expect("4 bytes")));
    }
    Ok(out)
}

struct SummaryInputs<'a> {
    tip: (u32, H256),
    to: u32,
    files: usize,
    blk_bytes: u64,
    digest: String,
    unlinked: u64,
    ahead: u64,
    duplicates: u64,
    scan_issues: u64,
    v24: Value,
    incomplete: &'a [String],
}

fn write_outputs(opts: &ScanOptions<'_>, total: &Partial, s: &SummaryInputs<'_>) -> Result<Value> {
    std::fs::create_dir_all(&opts.out)
        .with_context(|| format!("creating {}", opts.out.display()))?;
    let mut hit_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut stale_hit_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut examples: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for scan in Scan::ALL.into_iter().filter(|s| opts.scans.contains(*s)) {
        let mut lines = String::new();
        for h in total.hits.iter().filter(|h| h.scan == scan) {
            let counts = if h.stale {
                &mut stale_hit_counts
            } else {
                &mut hit_counts
            };
            bump(counts, h.class, 1);
            let example_key = if h.stale {
                format!("stale.{}", h.class)
            } else {
                h.class.to_string()
            };
            let ex = examples.entry(example_key).or_default();
            let json = h.to_json();
            lines.push_str(&serde_json::to_string(&json)?);
            lines.push('\n');
            if ex.len() < EXAMPLES_PER_CLASS {
                ex.push(json);
            }
        }
        let path = opts.out.join(format!("scan-{}.ndjson", scan.letter()));
        std::fs::write(&path, lines).with_context(|| format!("writing {}", path.display()))?;
    }
    let unparsed: Vec<Value> = total
        .unparsed
        .iter()
        .take(EXAMPLES_PER_CLASS)
        .map(|(h, b, why)| json!({"height": h, "block": rev_hex(b), "reason": why}))
        .collect();
    let summary = json!({
        "schema_version": 1,
        "net": opts.net.name(),
        "scans": opts.scans.letters(),
        "tip": {"height": s.tip.0, "hash": rev_hex(&s.tip.1)},
        "anchored": opts.anchor.is_some(),
        "range": {"from": opts.from, "to": s.to},
        "inputs": {
            "blk_files": s.files,
            "blk_bytes": s.blk_bytes,
            "files_digest": s.digest,
            "code": opts.code_id,
            "unlinked_records": s.unlinked,
            "records_above_tip": s.ahead,
            "duplicate_records": s.duplicates,
            "scan_issues": s.scan_issues,
            "v24": s.v24,
        },
        "counters": total.counters,
        "hit_counts": hit_counts,
        "stale_hit_counts": stale_hit_counts,
        "examples": examples,
        "unparsed": {"count": total.unparsed.len(), "examples": unparsed},
        "incomplete": s.incomplete,
    });
    let path = opts.out.join("summary.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary)? + "\n")
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(summary)
}
