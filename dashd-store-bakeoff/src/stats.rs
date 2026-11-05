//! Latency histograms, process/cgroup memory sampling and cgroup counters.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

/// A log-linear histogram of nanosecond values: 64 sub-buckets per power of two (≤ 1.6 % error).
pub struct Histogram {
    counts: Vec<u64>,
    total: u64,
    sum: u128,
    max: u64,
}

const SUB: u64 = 64;

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: vec![0; 64 * SUB as usize],
            total: 0,
            sum: 0,
            max: 0,
        }
    }
}

impl Histogram {
    fn index(v: u64) -> usize {
        if v < SUB {
            return v as usize;
        }
        let exp = 63 - v.leading_zeros() as u64; // ≥ 6
        let shift = exp - 6;
        let mantissa = (v >> shift) - SUB; // 0..64
        ((exp - 5) * SUB + mantissa) as usize
    }

    fn lower_bound(i: usize) -> u64 {
        let i = i as u64;
        if i < SUB {
            return i;
        }
        let exp = i / SUB + 5;
        let mantissa = i % SUB;
        (SUB + mantissa) << (exp - 6)
    }

    pub fn record(&mut self, ns: u64) {
        self.counts[Self::index(ns)] += 1;
        self.total += 1;
        self.sum += u128::from(ns);
        self.max = self.max.max(ns);
    }

    pub fn count(&self) -> u64 {
        self.total
    }

    pub fn quantile(&self, q: f64) -> u64 {
        if self.total == 0 {
            return 0;
        }
        let rank = ((q * self.total as f64).ceil() as u64).clamp(1, self.total);
        if rank == self.total {
            return self.max;
        }
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= rank {
                return Self::lower_bound(i).min(self.max);
            }
        }
        self.max
    }

    /// Summary in microseconds.
    pub fn summary_us(&self) -> Value {
        let us = |ns: u64| ns as f64 / 1000.0;
        json!({
            "count": self.total,
            "mean": if self.total == 0 { 0.0 } else { self.sum as f64 / self.total as f64 / 1000.0 },
            "p50": us(self.quantile(0.5)),
            "p90": us(self.quantile(0.9)),
            "p99": us(self.quantile(0.99)),
            "p999": us(self.quantile(0.999)),
            "max": us(self.max),
        })
    }
}

/// This process's cgroup v2 directory.
pub fn own_cgroup() -> Option<PathBuf> {
    let s = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let rel = s.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(PathBuf::from("/sys/fs/cgroup").join(rel.trim_start_matches('/')))
}

/// `key value` lines (`memory.stat`, `cpu.stat`) as a map.
pub fn read_kv(path: &Path) -> std::collections::BTreeMap<String, u64> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(' ')?;
            Some((k.to_string(), v.trim().parse().ok()?))
        })
        .collect()
}

/// cgroup `io.stat` summed over devices: rbytes, wbytes, rios, wios.
pub fn read_io_stat(cg: &Path) -> [u64; 4] {
    let mut out = [0u64; 4];
    for line in std::fs::read_to_string(cg.join("io.stat")).unwrap_or_default().lines() {
        for field in line.split_whitespace().skip(1) {
            if let Some((k, v)) = field.split_once('=') {
                let i = match k {
                    "rbytes" => 0,
                    "wbytes" => 1,
                    "rios" => 2,
                    "wios" => 3,
                    _ => continue,
                };
                out[i] += v.parse::<u64>().unwrap_or(0);
            }
        }
    }
    out
}

/// `RssAnon`, `RssFile`, `VmHWM`, `RssShmem` and `VmRSS` of this process in bytes. `VmRSS` is the
/// other three summed at one read, the total resident set a user sees.
pub fn proc_status() -> [u64; 5] {
    let mut out = [0u64; 5];
    for line in std::fs::read_to_string("/proc/self/status").unwrap_or_default().lines() {
        let i = if line.starts_with("RssAnon:") {
            0
        } else if line.starts_with("RssFile:") {
            1
        } else if line.starts_with("VmHWM:") {
            2
        } else if line.starts_with("RssShmem:") {
            3
        } else if line.starts_with("VmRSS:") {
            4
        } else {
            continue;
        };
        out[i] =
            line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
    }
    out
}

/// Peaks seen by the [`Sampler`]: process RssAnon/RssFile/RssShmem, its total VmRSS, and cgroup
/// anon/file.
#[derive(Default)]
pub struct Peaks {
    pub rss_anon: AtomicU64,
    pub rss_file: AtomicU64,
    pub rss_shmem: AtomicU64,
    pub rss_total: AtomicU64,
    pub cg_anon: AtomicU64,
    pub cg_file: AtomicU64,
}

impl Peaks {
    pub fn reset(&self) {
        for a in [
            &self.rss_anon,
            &self.rss_file,
            &self.rss_shmem,
            &self.rss_total,
            &self.cg_anon,
            &self.cg_file,
        ] {
            a.store(0, Ordering::Relaxed);
        }
    }

    pub fn json(&self) -> Value {
        json!({
            "rss_anon_peak": self.rss_anon.load(Ordering::Relaxed),
            "rss_file_peak": self.rss_file.load(Ordering::Relaxed),
            "rss_shmem_peak": self.rss_shmem.load(Ordering::Relaxed),
            "rss_total_peak": self.rss_total.load(Ordering::Relaxed),
            "cg_anon_peak": self.cg_anon.load(Ordering::Relaxed),
            "cg_file_peak": self.cg_file.load(Ordering::Relaxed),
        })
    }
}

/// Samples memory every 50 ms on a background thread.
pub struct Sampler {
    pub peaks: Arc<Peaks>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Sampler {
    pub fn start(cgroup: Option<PathBuf>) -> Self {
        Self::start_every(cgroup, 50)
    }

    /// A sampler reading every `ms` milliseconds.
    pub fn start_every(cgroup: Option<PathBuf>, ms: u64) -> Self {
        let peaks = Arc::new(Peaks::default());
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (peaks.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            while !s.load(Ordering::Relaxed) {
                Self::sample(&p, cgroup.as_ref());
                std::thread::sleep(Duration::from_millis(ms));
            }
        });
        Self {
            peaks,
            stop,
            handle: Some(handle),
        }
    }

    pub fn sample(p: &Peaks, cgroup: Option<&PathBuf>) {
        let [anon, file, _, shmem, total] = proc_status();
        p.rss_anon.fetch_max(anon, Ordering::Relaxed);
        p.rss_file.fetch_max(file, Ordering::Relaxed);
        p.rss_shmem.fetch_max(shmem, Ordering::Relaxed);
        p.rss_total.fetch_max(total, Ordering::Relaxed);
        if let Some(cg) = cgroup {
            let m = read_kv(&cg.join("memory.stat"));
            p.cg_anon.fetch_max(m.get("anon").copied().unwrap_or(0), Ordering::Relaxed);
            p.cg_file.fetch_max(m.get("file").copied().unwrap_or(0), Ordering::Relaxed);
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Allocated bytes (`st_blocks × 512`) of every file under `dir`.
pub fn dir_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if let Ok(m) = e.metadata() {
                total += if m.is_dir() {
                    dir_bytes(&e.path())
                } else {
                    m.blocks() * 512
                };
            }
        }
    }
    total
}

/// `getrusage(RUSAGE_SELF)`: user and system CPU seconds and major faults.
pub fn rusage() -> (f64, f64, u64) {
    // SAFETY: getrusage fills a caller-provided struct.
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    (secs(r.ru_utime), secs(r.ru_stime), r.ru_majflt as u64)
}

/// Returns freed heap memory to the OS so a following RssAnon baseline is clean.
pub fn malloc_trim() {
    // SAFETY: plain glibc call.
    unsafe { libc::malloc_trim(0) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_quantiles() {
        let mut h = Histogram::default();
        for v in 1..=10_000u64 {
            h.record(v * 1000);
        }
        let p50 = h.quantile(0.5) as f64;
        assert!((p50 / 5_000_000.0 - 1.0).abs() < 0.02, "{p50}");
        let p99 = h.quantile(0.99) as f64;
        assert!((p99 / 9_900_000.0 - 1.0).abs() < 0.02, "{p99}");
        assert_eq!(h.quantile(1.0), 10_000_000);
        for v in [0, 1, 63, 64, 65, 1000, u64::MAX / 2] {
            assert!(Histogram::lower_bound(Histogram::index(v)) <= v);
        }
    }
}
