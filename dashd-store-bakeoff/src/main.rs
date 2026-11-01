//! ST1 storage-engine bake-off harness (`docs/node-program/designs/ST1-bakeoff.md`).
//!
//! Subcommands:
//! - `extract`: build the UTXO churn trace from a blocks directory (protocol §2);
//! - `table-bytes`: the coins-cache table allocation for a budget (for the cgroup cap, §5);
//! - `replay`: replay a trace through one engine and report the §6 metrics;
//! - `micro`: experiments D1–D3 on an existing database (§6a);
//! - `diskfull`: experiment D4;
//! - `proxy`: one arm of CH2c's DEC-43 proxy (`CH2c-notes.md` D6–D11).
//!
//! The runner (`run.py`) owns isolation, ordering and noise records.

mod cache;
mod core_map;
// CH2c D4: node-l1 checks this model against the oracle op by op.
#[allow(dead_code)]
#[path = "../../node-l1/src/core_usage.rs"]
mod core_usage;
mod micro;
mod proxy;
mod replay;
mod stats;
mod store;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use dash_coreimport::trace::{ExtractConfig, extract};
use dash_coreimport::{hash_from_hex, hash_to_hex};
use dashd_chainstate::tuning::RocksTuning;
use serde_json::{Value, json};

use crate::stats::{Sampler, dir_bytes, own_cgroup, read_io_stat, read_kv, rusage};
use crate::store::EngineBudget;

struct Args(HashMap<String, String>);

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut map = HashMap::new();
        let mut it = raw.iter();
        while let Some(k) = it.next() {
            let k = k.strip_prefix("--").ok_or_else(|| format!("unexpected argument {k}"))?;
            let v = it.next().ok_or_else(|| format!("--{k} needs a value"))?;
            map.insert(k.to_string(), v.clone());
        }
        Ok(Self(map))
    }

    fn str(&self, k: &str) -> Result<&str, String> {
        self.0.get(k).map(String::as_str).ok_or_else(|| format!("missing --{k}"))
    }

    fn path(&self, k: &str) -> Result<PathBuf, String> {
        self.str(k).map(PathBuf::from)
    }

    fn num(&self, k: &str) -> Result<usize, String> {
        self.str(k)?.parse().map_err(|e| format!("--{k}: {e}"))
    }

    fn budget(&self) -> Result<EngineBudget, String> {
        Ok(EngineBudget {
            block_cache: self.num("block-cache")?,
            total: self.num("engine-budget")?,
        })
    }
}

fn write_json(path: &std::path::Path, v: &Value) -> Result<(), String> {
    std::fs::write(path, serde_json::to_string_pretty(v).expect("json")).map_err(|e| e.to_string())
}

fn cmd_extract(a: &Args) -> Result<(), String> {
    let network = match a.str("net")? {
        "mainnet" => dash_network::Network::Mainnet,
        "testnet" => dash_network::Network::Testnet,
        n => return Err(format!("unknown network {n}")),
    };
    let anchor = hash_from_hex(a.str("anchor")?).ok_or("bad --anchor")?;
    let out = a.path("out")?;
    let config = ExtractConfig {
        blocks_dir: a.path("blocks")?,
        network_magic: network.magic().to_le_bytes(),
        anchor,
        output: out.clone(),
        rev_cache_files: 4,
    };
    let r = extract(&config, |h, r| {
        eprintln!("extract: height {h}, live {} coins / {} B", r.live_coins, r.live_bytes)
    })
    .map_err(|e| e.to_string())?;
    let kinds = |m: &std::collections::BTreeMap<_, u64>| {
        m.iter().map(|(k, v)| (format!("{k:?}"), json!(v))).collect::<serde_json::Map<_, _>>()
    };
    let report = json!({
        "network": a.str("net")?,
        "blocks_dir": config.blocks_dir,
        "anchor": hash_to_hex(&anchor),
        "last_height": r.last_height,
        "blk_files": r.blk_files,
        "blocks_indexed": r.blocks_indexed,
        "txs": r.txs,
        "spends": r.spends,
        "creates": r.creates,
        "unspendable_outputs": r.unspendable_outputs,
        "stale_undo_skipped": r.stale_undo_skipped,
        "blk_resync_bytes": r.blk_resync_bytes,
        "rev_resync_bytes": r.rev_resync_bytes,
        "final_live_coins": r.live_coins,
        "final_live_bytes": r.live_bytes,
        "peak_live_coins": r.peak_live_coins,
        "peak_live_coins_height": r.peak_live_coins_height,
        "peak_live_bytes": r.peak_live_bytes,
        "checked_spent_kinds": kinds(&r.checked_spent_kinds),
        "created_kinds": kinds(&r.created_kinds),
        "live_curve": r.live_curve,
        "trace_bytes": std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0),
        "seconds": r.seconds,
    });
    let mut json_path = out.into_os_string();
    json_path.push(".json");
    write_json(&PathBuf::from(json_path), &report)
}

fn cg_snapshot(cg: Option<&std::path::Path>) -> Value {
    let Some(cg) = cg else {
        return Value::Null;
    };
    let cpu = read_kv(&cg.join("cpu.stat"));
    let io = read_io_stat(cg);
    json!({
        "cpu_usage_usec": cpu.get("usage_usec"),
        "cpu_user_usec": cpu.get("user_usec"),
        "cpu_system_usec": cpu.get("system_usec"),
        "io_rbytes": io[0], "io_wbytes": io[1], "io_rios": io[2], "io_wios": io[3],
        "memory_peak": std::fs::read_to_string(cg.join("memory.peak")).ok().and_then(|s| s.trim().parse::<u64>().ok()),
    })
}

fn cmd_replay(a: &Args) -> Result<(), String> {
    let engine = a.str("engine")?;
    let db = a.path("db")?;
    let budget = a.budget()?;
    let config = replay::ReplayConfig {
        coins_budget: a.num("coins-budget")?,
        max_entries: a.num("max-entries")?,
    };
    let cg = own_cgroup();
    let sampler = Sampler::start(cg.clone());
    let cg_start = cg_snapshot(cg.as_deref());
    let ru_start = rusage();
    let mut store = store::open(engine, &db, budget)?;
    let r = replay::replay(&a.path("trace")?, store.as_mut(), &config, |h| {
        eprintln!("{engine}: height {h}")
    })?;
    // Background work is paid inside the measured time: wait for it, then close (LevelDB's close
    // also waits for a running compaction).
    let t = Instant::now();
    store.quiesce()?;
    let quiesce_s = t.elapsed().as_secs_f64();
    let extra = store.extra();
    let t = Instant::now();
    drop(store);
    let close_s = t.elapsed().as_secs_f64();
    let ru_end = rusage();
    let cg_end = cg_snapshot(cg.as_deref());
    let peaks = sampler.peaks.json();
    let db_bytes = dir_bytes(&db);
    let compacted = {
        let mut s = store::open(engine, &db, budget)?;
        s.compact()?;
        drop(s);
        dir_bytes(&db)
    };
    let events = (r.spends + r.creates).max(1);
    let cpu_s = ru_end.0 - ru_start.0 + ru_end.1 - ru_start.1;
    let out = json!({
        "engine": engine,
        "coins_budget": config.coins_budget,
        "max_entries": config.max_entries,
        "budget": { "block_cache": budget.block_cache, "engine_budget": budget.total },
        "replay": r.json(),
        "quiesce_s": quiesce_s,
        "close_s": close_s,
        "time_total_s": r.replay_s + quiesce_s + close_s,
        "db_bytes": db_bytes,
        "db_bytes_compacted": compacted,
        "memory": peaks,
        "rusage": { "user_s": ru_end.0 - ru_start.0, "sys_s": ru_end.1 - ru_start.1, "maj_faults": ru_end.2 - ru_start.2 },
        "cpu_per_event_ns": cpu_s * 1e9 / events as f64,
        "cgroup_start": cg_start,
        "cgroup_end": cg_end,
        "engine_extra": extra,
    });
    write_json(&a.path("out")?, &out)
}

fn cmd_micro(a: &Args) -> Result<(), String> {
    let engine = a.str("engine")?;
    let db = a.path("db")?;
    let cg = own_cgroup();
    let sampler = Sampler::start(cg.clone());
    let mut store = store::open(engine, &db, a.budget()?)?;
    let mut keys = micro::KeyGen::new(0x5eed);
    let out = a.path("out")?;
    let largest = a.num("largest-flush")? as u64;
    // Written after every experiment, so a stopped run keeps what it measured.
    let mut result = json!({ "engine": engine });
    result["d1"] = micro::d1(store.as_mut(), &mut keys)?;
    write_json(&out, &result)?;
    result["d2"] = micro::d2(store.as_mut(), &mut keys, cg.as_deref(), &db)?;
    write_json(&out, &result)?;
    result["d3_largest_flush"] = micro::d3(store.as_mut(), &mut keys, &sampler, largest)?;
    write_json(&out, &result)?;
    result["d3_256MiB"] = micro::d3(store.as_mut(), &mut keys, &sampler, 256 << 20)?;
    write_json(&out, &result)
}

fn cmd_diskfull(a: &Args) -> Result<(), String> {
    let r = micro::d4(a.str("engine")?, &a.path("dir")?, &a.path("ballast")?, a.budget()?)?;
    write_json(&a.path("out")?, &json!({ "engine": a.str("engine")?, "d4": r }))
}

/// Writes a test fixture for `dash-coreimport`: the chain up to `--anchor` as one blk file in
/// height order and its undo records as one rev file, copied byte for byte from a Core blocks
/// directory. Height 2's undo record is also placed once before height 1's, so the extractor
/// has a non-matching record to skip.
fn cmd_fixture(a: &Args) -> Result<(), String> {
    use dash_coreimport::blockfiles::{BlockReader, HeaderIndex, RevCursors};
    let magic = dash_network::Network::Mainnet.magic().to_le_bytes();
    let blocks = a.path("blocks")?;
    let anchor = hash_from_hex(a.str("anchor")?).ok_or("bad --anchor")?;
    let out = a.path("out")?;
    let index = HeaderIndex::build(&blocks, magic).map_err(|e| e.to_string())?;
    let chain = index.chain_to(&anchor).map_err(|e| e.to_string())?;
    let mut reader = BlockReader::new(&index.files);
    let mut revs = RevCursors::new(&blocks, magic, 2);
    let (mut blk, mut undos) = (Vec::new(), Vec::new());
    let mut buf = Vec::new();
    for (height, (_, loc)) in chain.iter().enumerate() {
        reader.read(loc, &mut buf).map_err(|e| e.to_string())?;
        blk.extend(magic);
        blk.extend((buf.len() as u32).to_le_bytes());
        blk.extend(&buf);
        if height > 0 {
            let mut undo = None;
            revs.take(loc.file, &loc.prev, |u| {
                undo = Some(u.to_vec());
                true
            })
            .map_err(|e| e.to_string())?;
            let undo = undo.ok_or(format!("no undo for height {height}"))?;
            let mut rec = magic.to_vec();
            rec.extend((undo.len() as u32).to_le_bytes());
            rec.extend(&undo);
            rec.extend(dash_coreimport::blockfiles::undo_checksum(&loc.prev, &undo));
            undos.push(rec);
        }
    }
    if undos.len() < 2 {
        return Err("the fixture needs at least 3 blocks".into());
    }
    let mut rev = undos[1].clone();
    for rec in &undos {
        rev.extend(rec);
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::write(out.join("blk00000.dat"), blk).map_err(|e| e.to_string())?;
    std::fs::write(out.join("rev00000.dat"), rev).map_err(|e| e.to_string())
}

fn cmd_proxy(a: &Args) -> Result<(), String> {
    let arm = a.str("arm")?;
    let db = a.path("db")?;
    std::fs::create_dir_all(&db).map_err(|e| e.to_string())?;
    let cfg = proxy::ProxyConfig {
        coins_share: a.num("coins-share")? as u64,
        blocks: a
            .0
            .get("blocks")
            .map(|b| b.parse())
            .transpose()
            .map_err(|e| format!("--blocks: {e}"))?,
        periodic_flush_height: a
            .0
            .get("periodic-flush-height")
            .map(|h| h.parse())
            .transpose()
            .map_err(|e| format!("--periodic-flush-height: {e}"))?,
    };
    let cg = own_cgroup();
    let sampler = Sampler::start_every(cg.clone(), 5);
    let cg_start = cg_snapshot(cg.as_deref());
    let ru_start = rusage();
    let trace = a.path("trace")?;
    let mut out = match arm {
        "core" => proxy::run_core(&trace, &db, &cfg)?,
        "rdashd" => proxy::run_rdashd(&trace, &db, &cfg, Arc::new(RocksTuning::default()), arm)?,
        other => return Err(format!("unknown arm {other}")),
    };
    let ru_end = rusage();
    Sampler::sample(&sampler.peaks, cg.as_ref());
    out["memory"] = sampler.peaks.json();
    out["cgroup_start"] = cg_start;
    out["cgroup_end"] = cg_snapshot(cg.as_deref());
    out["rusage"] = json!({ "user_s": ru_end.0 - ru_start.0, "sys_s": ru_end.1 - ru_start.1, "maj_faults": ru_end.2 - ru_start.2 });
    out["db_bytes"] = json!(dir_bytes(&db));
    write_json(&a.path("out")?, &out)
}

fn cmd_table_bytes(a: &Args) -> Result<(), String> {
    let c = cache::Cache::new(a.num("coins-budget")?, a.num("max-entries")?);
    println!("{}", c.table_bytes());
    Ok(())
}

fn main() {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let Some((cmd, rest)) = raw.split_first() else {
        eprintln!(
            "usage: dashd-store-bakeoff <extract|fixture|table-bytes|replay|micro|diskfull|proxy> --key value ..."
        );
        std::process::exit(2);
    };
    let result = Args::parse(rest).and_then(|a| match cmd.as_str() {
        "extract" => cmd_extract(&a),
        "table-bytes" => cmd_table_bytes(&a),
        "fixture" => cmd_fixture(&a),
        "replay" => cmd_replay(&a),
        "micro" => cmd_micro(&a),
        "diskfull" => cmd_diskfull(&a),
        "proxy" => cmd_proxy(&a),
        other => Err(format!("unknown command {other}")),
    });
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
