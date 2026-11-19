//! Durability and failure experiments D1–D4 (protocol §6a).

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use serde_json::{Value, json};

use crate::stats::{Histogram, Sampler, malloc_trim, proc_status, read_io_stat};
use crate::store::{KeyBuf, Op, Res, Store};

/// Length of the synthetic coin values (a typical compressed P2PKH coin is 24–30 bytes).
const VALUE: [u8; 30] = [0x5a; 30];

/// Fresh random outpoint keys: `random txid ‖ VARINT(0)`.
pub struct KeyGen(u64);

impl KeyGen {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn key(&mut self) -> KeyBuf {
        let mut k = [0u8; 33];
        for chunk in k[..32].chunks_mut(8) {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        KeyBuf::new(&k)
    }

    /// Puts of fresh keys with `VALUE`, `n` of them.
    pub fn puts(&mut self, n: usize) -> impl Iterator<Item = Op<'static>> + '_ {
        (0..n).map(|_| (self.key(), Some(&VALUE[..])))
    }
}

/// Logical bytes of one synthetic put.
const OP_BYTES: usize = 33 + VALUE.len();

const D1_PHASE_LIMIT: std::time::Duration = std::time::Duration::from_secs(120);

/// D1: commit latency, buffered vs durable, for 1-op and 100-op batches (at most 2 minutes per
/// phase; `count` says how many commits ran).
pub fn d1(store: &mut dyn Store, keys: &mut KeyGen) -> Res<Value> {
    let mut out = serde_json::Map::new();
    for (ops_per_commit, commits) in [(1usize, 20_000usize), (100, 2_000)] {
        for durable in [false, true] {
            let mut h = Histogram::default();
            let started = Instant::now();
            for _ in 0..commits {
                let t = Instant::now();
                store.commit(&mut keys.puts(ops_per_commit), durable)?;
                h.record(t.elapsed().as_nanos() as u64);
                // Each phase is capped at two minutes: the rates are what matter.
                if started.elapsed() > D1_PHASE_LIMIT {
                    break;
                }
            }
            let secs = started.elapsed().as_secs_f64();
            let mut v = h.summary_us();
            v["commits_per_s"] = json!(h.count() as f64 / secs);
            let name = format!(
                "{}x{}_{}",
                commits,
                ops_per_commit,
                if durable {
                    "durable"
                } else {
                    "buffered"
                }
            );
            out.insert(name, v);
        }
    }
    Ok(Value::Object(out))
}

/// D2: barrier latency after `mib` MiB of buffered puts, plus the flat-file fsync reference.
pub fn d2(
    store: &mut dyn Store,
    keys: &mut KeyGen,
    cgroup: Option<&std::path::Path>,
    dir: &Path,
) -> Res<Value> {
    let io = || cgroup.map_or([0; 4], read_io_stat);
    let mut out = serde_json::Map::new();
    for mib in [16usize, 256] {
        // The fill is capped at two minutes too: on a B+tree each 4 MiB random-key commit can
        // rewrite most leaf pages. What was written, and at what cost, is reported.
        let per_commit = (4 << 20) / OP_BYTES;
        let (before, t) = (io(), Instant::now());
        let mut commits = 0;
        for _ in 0..(mib << 20) / (4 << 20) {
            store.commit(&mut keys.puts(per_commit), false)?;
            commits += 1;
            if t.elapsed() > D1_PHASE_LIMIT {
                break;
            }
        }
        let after = io();
        out.insert(
            format!("{mib}MiB_fill"),
            json!({ "seconds": t.elapsed().as_secs_f64(), "commits": commits,
                    "logical_bytes": (commits * per_commit * OP_BYTES) as u64,
                    "written_bytes": after[1] - before[1] }),
        );
        for &b in store.barriers() {
            let before = io();
            let t = Instant::now();
            store.barrier(b)?;
            let secs = t.elapsed().as_secs_f64();
            let after = io();
            out.insert(
                format!("{mib}MiB_{b}"),
                json!({ "seconds": secs, "written_bytes": after[1] - before[1], "write_ios": after[3] - before[3] }),
            );
        }
    }
    // Reference: the block-file barrier in Core's order (fsync the file, then its directory).
    let path = dir.join("flatfile-reference.dat");
    let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let chunk = vec![0xa5u8; 1 << 20];
    for _ in 0..128 {
        f.write_all(&chunk).map_err(|e| e.to_string())?;
    }
    let before = io();
    let t = Instant::now();
    f.sync_all().map_err(|e| e.to_string())?;
    let file_s = t.elapsed().as_secs_f64();
    let t = Instant::now();
    std::fs::File::open(dir).and_then(|d| d.sync_all()).map_err(|e| e.to_string())?;
    let dir_s = t.elapsed().as_secs_f64();
    let after = io();
    drop(f);
    let _ = std::fs::remove_file(&path);
    out.insert(
        "flatfile_128MiB".into(),
        json!({ "file_fsync_s": file_s, "dir_fsync_s": dir_s, "written_bytes": after[1] - before[1] }),
    );
    Ok(Value::Object(out))
}

/// D3: anonymous memory held by one pending batch of `logical_bytes`, and the peak while it is
/// committed.
pub fn d3(
    store: &mut dyn Store,
    keys: &mut KeyGen,
    sampler: &Sampler,
    logical_bytes: u64,
) -> Res<Value> {
    let limit = (logical_bytes as usize / OP_BYTES).max(1);
    malloc_trim();
    let base = proc_status()[0];
    let mut pending_anon = 0;
    sampler.peaks.reset();
    let t = Instant::now();
    // Building the batch stops after two minutes (an MDBX transaction spills to the file).
    let mut n = 0;
    let mut ops = keys.puts(limit).take_while(|_| t.elapsed() < D1_PHASE_LIMIT).inspect(|_| n += 1);
    store.pending_batch(&mut ops, &mut || pending_anon = proc_status()[0])?;
    drop(ops);
    let secs = t.elapsed().as_secs_f64();
    Sampler::sample(&sampler.peaks, None);
    let peak = sampler.peaks.rss_anon.load(std::sync::atomic::Ordering::Relaxed);
    Ok(json!({
        "logical_bytes": (n * OP_BYTES) as u64,
        "ops": n,
        "baseline_anon": base,
        "pending_anon_delta": pending_anon.saturating_sub(base),
        "peak_anon_delta": peak.saturating_sub(base),
        "pending_per_logical_byte": pending_anon.saturating_sub(base) as f64 / (n * OP_BYTES) as f64,
        "build_and_commit_s": secs,
    }))
}

/// How long D4 waits after freeing space before it retries a write without reopening.
const D4_RETRY_WAIT: std::time::Duration = std::time::Duration::from_secs(12);

/// Free bytes of the filesystem holding `path` (`df`), or `None` if it cannot be read.
fn free_bytes(path: &Path) -> Option<u64> {
    let out =
        std::process::Command::new("df").args(["--output=avail", "-B1"]).arg(path).output().ok()?;
    String::from_utf8(out.stdout).ok()?.lines().nth(1)?.trim().parse().ok()
}

/// D4: fill the filesystem under `dir` until a write fails, then check the error path.
///
/// Writes 4 MiB batches buffered with a durable commit every 8 batches. `ballast` is a file on
/// the same filesystem deleted after the failure to free space. The retry without a reopen waits
/// [`D4_RETRY_WAIT`] after that: RocksDB re-checks free space every 5 s and, once its reserve
/// (the largest column family write buffer: 64 MiB, from RocksDB's unconfigured `default` column
/// family) is free, resumes writes by itself, so an immediate retry
/// cannot tell an engine that stays stopped from one that is about to resume (ST1 review r3, M1).
/// The ballast must be larger than that reserve.
pub fn d4(
    engine: &str,
    dir: &Path,
    ballast: &Path,
    budget: crate::store::EngineBudget,
) -> Res<Value> {
    let per_batch = (4 << 20) / OP_BYTES;
    let mut keys = KeyGen::new(0xd4);
    let mut store = crate::store::open(engine, dir, budget)?;
    // The first and last key of each batch, probed after reopening.
    let mut probes: Vec<(KeyBuf, KeyBuf)> = Vec::new();
    let mut durable_batches = 0usize;
    let mut failure = None;
    let mut written = 0usize;
    for i in 0.. {
        let mut batch: Vec<Op> = keys.puts(per_batch).collect();
        probes.push((batch[0].0, batch[batch.len() - 1].0));
        let durable = i % 8 == 7;
        match store.commit(&mut batch.drain(..), durable) {
            Ok(()) => {
                written += 1;
                if durable {
                    durable_batches = written;
                }
            }
            Err(e) => {
                failure = Some((i, e));
                break;
            }
        }
        if i > 100_000 {
            return Err("the filesystem never filled".into());
        }
    }
    let (failed_batch, error) = failure.expect("loop ends on failure");
    std::fs::remove_file(ballast).map_err(|e| format!("removing ballast: {e}"))?;
    std::thread::sleep(D4_RETRY_WAIT);
    let free_at_retry = free_bytes(dir);
    let retry = store.commit(&mut keys.puts(16), true).map_err(|e| e.to_string());
    drop(store);
    let reopen = crate::store::open(engine, dir, budget);
    let mut result = json!({
        "failed_batch": failed_batch,
        "acknowledged_batches": written,
        "acknowledged_durable_batches": durable_batches,
        "error": error,
        "write_after_freeing_space_without_reopen": match &retry { Ok(()) => "ok".to_string(), Err(e) => e.clone() },
        "retry_after_s": D4_RETRY_WAIT.as_secs(),
        "free_bytes_at_retry": free_at_retry,
    });
    match reopen {
        Err(e) => result["reopen"] = json!(format!("failed: {e}")),
        Ok(mut store) => {
            let mut buf = Vec::new();
            let mut has = |k: &KeyBuf| store.get(k.as_slice(), &mut buf).unwrap_or(false);
            let present: Vec<(bool, bool)> = probes.iter().map(|(a, b)| (has(a), has(b))).collect();
            let durable_ok = present[..durable_batches].iter().all(|&(a, b)| a && b);
            let (first, last) = present[failed_batch];
            result["reopen"] = json!("ok");
            result["all_durable_batches_present"] = json!(durable_ok);
            result["failed_batch_visible"] = json!(first && last);
            result["failed_batch_atomic"] = json!(first == last);
            result["torn_batches"] = json!(present.iter().filter(|(a, b)| a != b).count());
            result["present_batches"] = json!(present.iter().filter(|(a, b)| *a && *b).count());
        }
    }
    Ok(result)
}
