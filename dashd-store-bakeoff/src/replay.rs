//! The trace replay (protocol §3) and the per-run metrics (§6).

use std::path::Path;
use std::time::Instant;

use dash_coreimport::trace::{Event, OutKey, TraceBlock, TraceReader};
use serde_json::{Value, json};

use crate::cache::{Cache, DIRTY, Entry, SPENT, SpendResult, engine_key};
use crate::stats::Histogram;
use crate::store::{Res, Store};

pub struct ReplayConfig {
    pub coins_budget: usize,
    pub max_entries: usize,
}

#[derive(Default)]
pub struct ReplayResult {
    pub blocks: u64,
    pub spends: u64,
    pub creates: u64,
    pub misses: u64,
    pub flushes: u64,
    pub replay_s: f64,
    pub connect_s: f64,
    pub flush_s: f64,
    pub pipelined_est_s: f64,
    /// Σ key + value of puts and Σ key of deletes, over all flushes.
    pub logical_write_bytes: u64,
    pub puts: u64,
    pub deletes: u64,
    /// The largest flush, in logical bytes (D3 uses it).
    pub largest_flush_bytes: u64,
    pub miss_read: Histogram,
    pub flush_stall: Histogram,
    pub block: Histogram,
    pub table_bytes: usize,
    pub last_height: u32,
}

impl ReplayResult {
    pub fn json(&self) -> Value {
        json!({
            "blocks": self.blocks,
            "spends": self.spends,
            "creates": self.creates,
            "misses": self.misses,
            "flushes": self.flushes,
            "time_replay_s": self.replay_s,
            "connect_s": self.connect_s,
            "flush_s": self.flush_s,
            "time_pipelined_est_s": self.pipelined_est_s,
            "logical_write_bytes": self.logical_write_bytes,
            "puts": self.puts,
            "deletes": self.deletes,
            "largest_flush_bytes": self.largest_flush_bytes,
            "miss_read_us": self.miss_read.summary_us(),
            "flush_stall_ms": {
                "count": self.flush_stall.count(),
                "p50": self.flush_stall.quantile(0.5) as f64 / 1e6,
                "p99": self.flush_stall.quantile(0.99) as f64 / 1e6,
                "max": self.flush_stall.quantile(1.0) as f64 / 1e6,
            },
            "block_us": self.block.summary_us(),
            "harness_table_bytes": self.table_bytes,
            "last_height": self.last_height,
        })
    }
}

/// Writes every dirty entry, sorted by engine key, as one flush, and records its stall. The sort
/// index holds one pointer per entry and keys are encoded on the fly, so a flush needs no copy of
/// the cache. Returns the stall in seconds.
fn timed_flush(
    cache: &mut Cache,
    store: &mut dyn Store,
    tip: u32,
    old_tip: u32,
    r: &mut ReplayResult,
) -> Res<f64> {
    let t = Instant::now();
    let mut entries: Vec<(&OutKey, &Entry)> =
        cache.map.iter().filter(|(_, e)| e.flags & DIRTY != 0).collect();
    // Engine-key order: txid, then the VARINT bytes of the vout (VARINT is not order-preserving
    // across lengths, so compare the encodings).
    entries.sort_unstable_by(|a, b| {
        a.0[..32]
            .cmp(&b.0[..32])
            .then_with(|| engine_key(a.0).as_slice().cmp(engine_key(b.0).as_slice()))
    });
    let (mut logical, mut puts, mut deletes) = (0u64, 0u64, 0u64);
    let mut ops = entries.iter().map(|(k, e)| {
        let key = engine_key(k);
        if e.flags & SPENT != 0 {
            deletes += 1;
            logical += key.as_slice().len() as u64;
            (key, None)
        } else {
            puts += 1;
            logical += (key.as_slice().len() + e.coin.bytes().len()) as u64;
            (key, Some(e.coin.bytes()))
        }
    });
    store.flush(&mut ops, tip, old_tip)?;
    drop(ops);
    drop(entries);
    cache.clear();
    let f = t.elapsed().as_secs_f64();
    r.puts += puts;
    r.deletes += deletes;
    r.logical_write_bytes += logical;
    r.largest_flush_bytes = r.largest_flush_bytes.max(logical);
    r.flush_stall.record((f * 1e9) as u64);
    r.flush_s += f;
    r.flushes += 1;
    Ok(f)
}

pub fn replay(
    trace: &Path,
    store: &mut dyn Store,
    config: &ReplayConfig,
    mut progress: impl FnMut(u32),
) -> Res<ReplayResult> {
    let mut reader = TraceReader::open(trace).map_err(|e| e.to_string())?;
    let mut cache = Cache::new(config.coins_budget, config.max_entries);
    let mut r = ReplayResult {
        table_bytes: cache.table_bytes(),
        ..Default::default()
    };
    let mut block = TraceBlock::default();
    let mut value = Vec::with_capacity(64);
    let mut last_flush_tip = 0u32;
    // Pipelined estimate: connect time of the current interval and the previous flush.
    let mut interval_connect = 0f64;
    let mut prev_flush: Option<f64> = None;
    let started = Instant::now();
    while reader.next_block(&mut block).map_err(|e| e.to_string())? {
        let t0 = Instant::now();
        for e in &block.events {
            match e {
                Event::Spend(k) => {
                    r.spends += 1;
                    if let SpendResult::Miss = cache.spend(k)? {
                        r.misses += 1;
                        let key = engine_key(k);
                        let t = Instant::now();
                        let found = store.get(key.as_slice(), &mut value)?;
                        r.miss_read.record(t.elapsed().as_nanos() as u64);
                        if !found {
                            return Err(format!(
                                "height {}: coin missing from the engine",
                                block.height
                            ));
                        }
                        cache.insert_spent(*k);
                    }
                }
                Event::Create {
                    key: k,
                    coin_start,
                    coin_len,
                } => {
                    r.creates += 1;
                    cache.create(*k, block.coin(*coin_start, *coin_len))?;
                }
            }
        }
        let dt = t0.elapsed().as_secs_f64();
        r.block.record((dt * 1e9) as u64);
        r.connect_s += dt;
        interval_connect += dt;
        r.blocks += 1;
        r.last_height = block.height;
        if cache.needs_flush() {
            let f = timed_flush(&mut cache, store, block.height, last_flush_tip, &mut r)?;
            r.pipelined_est_s += prev_flush.map_or(interval_connect, |p| interval_connect.max(p));
            prev_flush = Some(f);
            interval_connect = 0.0;
            last_flush_tip = block.height;
        }
        if block.height.is_multiple_of(10_000) {
            progress(block.height);
        }
    }
    // The final flush.
    let f = timed_flush(&mut cache, store, r.last_height, last_flush_tip, &mut r)?;
    r.pipelined_est_s += prev_flush.map_or(interval_connect, |p| interval_connect.max(p)) + f;
    r.replay_s = started.elapsed().as_secs_f64();
    Ok(r)
}
