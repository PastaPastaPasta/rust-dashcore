//! The coins layer's density and hit cost at a full budget (CH2 §2.3, §7.1 a1; PF1). CH2a's layout
//! gate ran an earlier version of this harness against both candidate layouts and kept this one
//! (`docs/node-program/research/PF1-CH2a-layout-gate.md`).
//!
//! The real [`ShardedLayer`] is driven block by block with a fixed salt, so every run is identical.
//! The budget is checked where DEC-95 checks it, at block boundaries, against native
//! `Σ = F + active + R` with `R = S + 64 KiB` while F is in flight (CH2 §2.5). Inside a block the
//! layers grow as they must. At every boundary the tagging allocator's coins bytes must equal the
//! layers' allocated bytes.
//!
//! Each budget is filled as two layers, presized at `f · (B − R − B/16)` (CH2 §2.3, f = 1/2), the
//! second at most what is left of `B − R − B/16` beside the first (rev5), under §2.5's policy with
//! rev4's shard trigger. With no F in flight, active freezes into F at the first boundary where
//! THRESHOLD holds: its allocated bytes reach f·B, or a shard of it reaches its trigger, 55/64 of
//! its buckets; at every other boundary with no F in flight it gets the boundary rebuild (DEC-108).
//! A new active then fills with F in flight until a boundary takes Σ past B, where V
//! waits for the release (Core's CRITICAL); that boundary is the measurement point. Every mode
//! that fills both layers counts the shard doublings with and without F in flight; at B_DEC43 none
//! may happen without (CH2 §2.3, rev4; r6 m4: at small B the trigger's margin is a few entries, so
//! there it is reported, not gated).
//!
//! Modes:
//! - `sweep`: synthetic blocks of one coin each, creates only, lengths from the mainnet coin-length
//!   histogram (heights 1..453,371) and random keys. N_res and r = B / N_res at B_DEC43 and 16
//!   log-uniform budgets from 4 MiB to 1 GiB, the coins bytes' peak, and Core's floor
//!   `N_res ≥ N_core`. Here `N_res` is the coins at the last boundary within B, one coin short of
//!   the trace mode's point.
//! - `trace <path>`: the gate's method (CH2 §2.3): a DSTRACE1 UTXO churn trace (ST1's
//!   `mainnet-453371`) replayed block by block, spends included, over the same budgets, with
//!   `N_res` (entries, tombstones included) at the measurement point, the shard rebuilds' count,
//!   entries moved and time (in all, and per block that rebuilt: median, p99, max), the floor,
//!   F's allocated and table bytes with the presize they leave the second layer, and the boundary
//!   rebuilds (DEC-108): shards rebuilt, and per boundary that rebuilt its time (median, p99, max).
//!   Past the trace's end the replay repeats it with fresh keys.
//! - `worst`: the worst single block (CH2 §2.3, rev4) at each budget: active, with no F in flight,
//!   one coin short of its THRESHOLD freeze, takes a block of `MAX_OUTPUTS_PER_BLOCK`
//!   minimum-size outputs; the growth it keeps, its transient above that, and Σ once it freezes
//!   beside a new active presized either way.
//! - `stalls <path> [passes]`: V's freezes, stalls, double stalls, time waiting and shard
//!   doublings with no F in flight over `passes` passes of the trace (one by default) at every
//!   budget, on a clock of events, with W writing each F in ρ times the events V applied to it, for
//!   a new active presized to what is left beside F (rev5, O-R6-m6) and at the full presize.
//! - `latency <get|path|base> <n>`: builds the B_DEC43 state, then runs n lookups between
//!   `perf stat --control` markers (`PERF_CTL_FIFO`, `PERF_ACK_FIFO`): hits in the frozen layer
//!   (`get`), resident keys through active then F (`path`), or the key generation alone (`base`).
//! - `timing <n>`: ns per hit in the frozen layer at the B_DEC43 state, and `iter_dirty` throughput.
//!
//! `cargo test` runs this target with no mode, which does nothing.

// PROCESS.md: benches measure production only, so a test feature turned on here (by a self
// dev-dependency or `--all-features`) fails the build. Clippy's `--all-features` run is exempt.
#[cfg(all(not(clippy), any(feature = "crash", feature = "planted-bugs")))]
compile_error!("benches measure production only: build them without `crash` and `planted-bugs`");

use std::fs::File;
use std::hint::black_box;
use std::io::{Read as _, Write as _};
use std::time::Instant;

use dash_consensus::coins::MAX_OUTPUTS_PER_BLOCK;
use dash_consensus::primitives::TxOut;
use dashd_chainstate::alloc_tag::{self, Tag, TaggingAllocator};
use dashd_chainstate::coins::layer::{ShardSizes, ShardedLayer};
use dashd_chainstate::coins::{CoinLayerOps, Flags, OutPointKey, PackedCoin, Salt};

#[global_allocator]
static ALLOC: TaggingAllocator = TaggingAllocator;

const MIB: u64 = 1 << 20;
/// Core's coins share at `-dbcache=300` with txindex on (CH2 §2.5).
const B_DEC43: u64 = 252 * MIB + 3 * MIB / 4;
/// Core's bytes per cached P2PKH coin (CH2 §2.3).
const CORE_BYTES_PER_COIN: f64 = 115.67;
const SALT: Salt = Salt::fixed(0xC0FF_EE00_0CA5_4E00);
const HISTOGRAM: &str = include_str!("data/mainnet-453371-coin-lengths.txt");

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Coin `i` of the stream: a random txid, a small vout, a length from the histogram.
struct Stream {
    /// Cumulative counts and their lengths.
    cdf: Vec<(u64, usize)>,
    total: u64,
}

impl Stream {
    fn new() -> Self {
        let mut cdf = Vec::new();
        let mut total = 0;
        for line in HISTOGRAM.lines().filter(|l| !l.starts_with('#')) {
            let (len, count) = line.split_once(' ').expect("`length count`");
            total += count.parse::<u64>().unwrap();
            cdf.push((total, len.parse().unwrap()));
        }
        Self {
            cdf,
            total,
        }
    }

    fn key(i: u64) -> OutPointKey {
        let mut k = [0; 36];
        for w in 0..4 {
            k[w * 8..w * 8 + 8].copy_from_slice(&mix(i << 2 | w as u64).to_le_bytes());
        }
        k[32..].copy_from_slice(&((mix(!i) % 4) as u32).to_le_bytes());
        OutPointKey(k)
    }

    /// Coin `i`'s bytes, into `buf`.
    fn coin<'a>(&self, i: u64, buf: &'a mut Vec<u8>) -> &'a [u8] {
        let x = mix(i ^ 0x005e_ed0f_c01e) % self.total;
        let len = self.cdf[self.cdf.partition_point(|&(c, _)| c <= x)].1;
        let fill = mix(i).to_le_bytes();
        buf.clear();
        buf.extend((0..len).map(|j| fill[j % 8]));
        buf
    }
}

/// What ended a layer's fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// The boundary took Σ past B: with F in flight V waits (Core's CRITICAL).
    Critical,
    /// THRESHOLD: active's allocated bytes reached f·B.
    Threshold,
    /// THRESHOLD: a shard of active reached its trigger (CH2 §2.3, rev4).
    Trigger,
}

/// §2.5's THRESHOLD at a boundary within B with no F in flight: active's charged bytes (its
/// ceilings at least, CH2c D29) reach f·B, or a shard of active its trigger. With F in flight
/// nothing freezes.
fn threshold(active: &ShardedLayer, b: u64) -> Option<Stop> {
    if active.charged_bytes() >= b / 2 {
        Some(Stop::Threshold)
    } else if active.at_shard_trigger() {
        Some(Stop::Trigger)
    } else {
        None
    }
}

/// Each shard's buckets, or its ceiling's while it is below it (CH2c D29).
fn buckets(layer: &ShardedLayer) -> [usize; 64] {
    let mut out = [0; 64];
    let ceiling = |i| layer.sizes().buckets(i);
    out.iter_mut()
        .zip(layer.shard_loads().enumerate())
        .for_each(|(o, (i, (_, b)))| *o = b.max(ceiling(i)));
    out
}

/// The shard doublings past their ceilings from `was` to `now`.
fn doublings(was: &[usize; 64], now: &[usize; 64]) -> u32 {
    was.iter().zip(now).map(|(w, n)| (n / w).ilog2()).sum()
}

/// A filled layer.
struct Filled {
    layer: ShardedLayer,
    /// Boundaries within B (in the sweep, the coins at the last one).
    n: u64,
    stop: Stop,
    /// Shard doublings at its boundaries.
    doublings: u32,
}

/// `S + 64 KiB` with `S = min(-dbbatchsize, max(coins_share / 8, 64 KiB))` at the default 16 MiB
/// batch size and a coins share of B (CH2 §2.5).
fn sub_batch_reserve(b: u64) -> u64 {
    (16 * MIB).min((b / 8).max(64 << 10)) + (64 << 10)
}

/// The presize, `f · (B − R − B/16)` at f = 1/2 (CH2 §2.3).
fn presize(b: u64) -> u64 {
    (b - sub_batch_reserve(b) - b / 16) / 2
}

/// A new active's presize beside a frozen layer charged `frozen` bytes (CH2 §2.3, rev5, O-R6-m6): the
/// presize, or what is left of `B − R − B/16` if that is less, but at least 256 buckets a shard.
fn presize_after(b: u64, frozen: u64) -> u64 {
    let left = (b - sub_batch_reserve(b) - b / 16).saturating_sub(frozen);
    presize(b).min(left).max(
        ShardSizes {
            k: 8,
            s: 0,
        }
        .table_bytes(),
    )
}

fn n_core(b: u64) -> f64 {
    (0.9 * b as f64).max(b as f64 - (10 * MIB) as f64) / CORE_BYTES_PER_COIN
}

/// F and active at the moment V must wait.
struct Point {
    filled: [Filled; 2],
    /// The largest Σ at any boundary.
    sigma_max: u64,
    /// The coins bytes' peak over the build, inside blocks included (R excluded).
    peak: u64,
    /// Boundaries over the build (blocks in the trace).
    boundaries: u64,
    /// The time of each boundary rebuild (DEC-108) that rebuilt a shard, and the shards rebuilt.
    boundary_rebuild_secs: Vec<f64>,
    boundary_rebuilt_shards: u64,
    /// The allocator domain the build was counted in.
    domain: u8,
}

/// Fills F, `step` making each boundary's changes, until THRESHOLD freezes it, then a new active
/// with F in flight until V waits, for budget `b`, counted in a fresh allocator domain. At every
/// boundary the tagged coins bytes equal the layers' allocated bytes.
fn build_with(b: u64, mut step: impl FnMut(&mut ShardedLayer)) -> Point {
    let domain = alloc_tag::new_domain();
    alloc_tag::set_thread_domain(domain);
    alloc_tag::reset_peak(domain, Tag::Coins);
    let mut sigma_max = 0;
    let mut boundaries = 0;
    let (mut boundary_rebuild_secs, mut boundary_rebuilt_shards) = (Vec::new(), 0);
    // `in_flight`: what F allocates and is charged, once it is frozen.
    let mut fill = |in_flight: Option<(u64, u64)>| {
        let (other, charged, reserve) =
            in_flight.map_or((0, 0, 0), |(f, c)| (f, c, sub_batch_reserve(b)));
        let mut layer =
            ShardedLayer::new(in_flight.map_or(presize(b), |_| presize_after(b, charged)), SALT);
        let (mut n, mut grew) = (0, 0);
        let mut was = buckets(&layer);
        let stop = loop {
            step(&mut layer);
            boundaries += 1;
            let now = buckets(&layer);
            grew += doublings(&was, &now);
            was = now;
            let mine = layer.allocated_bytes();
            let tagged = alloc_tag::tagged_bytes(domain, Tag::Coins) as u64;
            assert_eq!(tagged, other + mine, "tagged bytes differ from allocated");
            let sigma = charged + reserve + layer.charged_bytes();
            sigma_max = sigma_max.max(sigma);
            if sigma > b {
                break Stop::Critical;
            }
            n += 1;
            if in_flight.is_none() {
                if let Some(stop) = threshold(&layer, b) {
                    break stop;
                }
                // CH2 §2.3, DEC-108.
                let t = Instant::now();
                let shards = layer.boundary_rebuild();
                if shards > 0 {
                    boundary_rebuild_secs.push(t.elapsed().as_secs_f64());
                    boundary_rebuilt_shards += shards as u64;
                }
            }
        };
        Filled {
            layer,
            n,
            stop,
            doublings: grew,
        }
    };
    let f = fill(None);
    let a = fill(Some((f.layer.allocated_bytes(), f.layer.charged_bytes())));
    Point {
        filled: [f, a],
        sigma_max,
        peak: alloc_tag::peak_bytes(domain, Tag::Coins) as u64,
        boundaries,
        boundary_rebuild_secs,
        boundary_rebuilt_shards,
        domain,
    }
}

/// [`build_with`] over the stream's coins `0..`, inserted as DIRTY | FRESH with a boundary after
/// each.
fn build(s: &Stream, b: u64) -> Point {
    let mut next = 0;
    let mut buf = Vec::new();
    build_with(b, |active| {
        active.upsert(&Stream::key(next), Flags::DIRTY | Flags::FRESH, s.coin(next, &mut buf));
        next += 1;
    })
}

fn sweep_points() -> Vec<u64> {
    let mut pts: Vec<u64> =
        (0..16).map(|i| (4.0 * MIB as f64 * 256f64.powf(f64::from(i) / 15.0)) as u64).collect();
    pts.push(B_DEC43);
    pts.sort_unstable();
    pts
}

fn sweep() {
    let s = Stream::new();
    println!(
        "# coins_layer_density sweep (rev4): f = 1/2, R = S + 64 KiB, a boundary per coin, salt fixed"
    );
    println!(
        "B_bytes\tN_res\tr\tN1\tstop1\talloc1\tN2\tstop2\talloc2\tpeak\tgrew_no_F\tgrew_F\tN_core"
    );
    sweep_table(|b| {
        let p = build(&s, b);
        let [f, a] = &p.filled;
        let n = f.n + a.n;
        println!(
            "{b}\t{n}\t{:.2}\t{}\t{:?}\t{}\t{}\t{:?}\t{}\t{}\t{}\t{}\t{:.0}",
            b as f64 / n as f64,
            f.n,
            f.stop,
            f.layer.allocated_bytes(),
            a.n,
            a.stop,
            a.layer.allocated_bytes(),
            p.peak,
            f.doublings,
            a.doublings,
            n_core(b),
        );
        (n as usize, p)
    });
}

/// Runs `row` at every budget: it prints a table row and returns N_res and the build, whose layers
/// must free every tagged byte. Then prints Core's floor `N_res ≥ N_core` (gated at every budget)
/// and the budgets where a shard grew with no F in flight, in F's fill (gated at B_DEC43 only),
/// each `ok` or the budgets (MiB) it fails at, and fails if either gate does.
fn sweep_table(mut row: impl FnMut(u64) -> (usize, Point)) {
    let (mut floor, mut grew) = (Vec::new(), Vec::new());
    for b in sweep_points() {
        let (n, p) = row(b);
        let mib = format!("{:.2}", b as f64 / MIB as f64);
        if (n as f64) < n_core(b) {
            floor.push(mib.clone());
        }
        if p.filled[0].doublings > 0 {
            grew.push(mib);
        }
        let domain = p.domain;
        drop(p);
        assert_eq!(alloc_tag::tagged_bytes(domain, Tag::Coins), 0, "a layer leaked");
    }
    let verdict = |f: &[String]| {
        if f.is_empty() {
            "ok".into()
        } else {
            f.join(",")
        }
    };
    println!();
    println!("floor\t{}\t# N_res >= N_core at every point", verdict(&floor));
    println!(
        "growth_without_F\t{}\t# budgets where a shard doubled while no F was in flight",
        verdict(&grew)
    );
    let dec43 = format!("{:.2}", B_DEC43 as f64 / MIB as f64);
    let gated = !grew.contains(&dec43);
    println!(
        "no_growth_without_F_at_B_DEC43\t{}",
        if gated {
            "ok"
        } else {
            "FAIL"
        }
    );
    assert!(floor.is_empty() && gated, "the floor or the no-growth check failed");
}

/// `perf stat --control` markers, when the run is under one.
struct PerfCtl(Option<(File, File)>);

impl PerfCtl {
    fn open() -> Self {
        let (Some(ctl), Some(ack)) =
            (std::env::var_os("PERF_CTL_FIFO"), std::env::var_os("PERF_ACK_FIFO"))
        else {
            return Self(None);
        };
        let ctl = File::options().write(true).open(ctl).expect("ctl fifo");
        let ack = File::open(ack).expect("ack fifo");
        Self(Some((ctl, ack)))
    }

    fn send(&mut self, cmd: &str) {
        if let Some((ctl, ack)) = &mut self.0 {
            ctl.write_all(cmd.as_bytes()).unwrap();
            ctl.write_all(b"\n").unwrap();
            let mut buf = [0; 5];
            ack.read_exact(&mut buf).unwrap();
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// A hit in the frozen layer.
    Get,
    /// A resident key through active, then F.
    Path,
    /// The key alone.
    Base,
}

/// `n` lookups of resident keys chosen by `seed`; returns the hits.
fn lookups(p: &Point, what: Lookup, n: u64, seed: u64) -> u64 {
    let [f, a] = &p.filled;
    let (n0, n_all) = (f.n, f.n + a.n);
    let (f, a) = (&f.layer, &a.layer);
    let mut hits = 0;
    for j in 0..n {
        let x = mix(j ^ seed.rotate_left(32));
        match what {
            Lookup::Get => {
                let k = Stream::key(x % n0);
                hits += u64::from(black_box(f.get(&k)).is_some());
            }
            Lookup::Path => {
                let k = Stream::key(x % n_all);
                let e = a.get(&k).or_else(|| f.get(&k));
                hits += u64::from(black_box(e).is_some());
            }
            Lookup::Base => {
                black_box(Stream::key(x % n0));
                hits += 1;
            }
        }
    }
    hits
}

fn latency(what: &str, n: u64) {
    let what = match what {
        "get" => Lookup::Get,
        "path" => Lookup::Path,
        "base" => Lookup::Base,
        _ => panic!("lookup: get | path | base"),
    };
    let s = Stream::new();
    let p = build(&s, B_DEC43);
    let mut ctl = PerfCtl::open();
    ctl.send("enable");
    let hits = lookups(&p, what, n, 1);
    ctl.send("disable");
    assert_eq!(hits, n, "every lookup is a hit");
    println!("latency\t{n}\tresident {}", p.filled[0].n + p.filled[1].n);
}

fn timing(n: u64) {
    let s = Stream::new();
    let p = build(&s, B_DEC43);
    let t = Instant::now();
    assert_eq!(lookups(&p, Lookup::Get, n, 100), n);
    println!("get\t{:.2} ns per hit", t.elapsed().as_secs_f64() * 1e9 / n as f64);
    let mut out = Vec::with_capacity(64 << 20);
    let t = Instant::now();
    let (entries, bytes) = copy_dirty(&p.filled[0].layer, &mut out);
    let secs = t.elapsed().as_secs_f64();
    println!(
        "iter_dirty\t{entries} entries\t{:.1} M entries/s\t{:.0} MiB/s",
        entries as f64 / secs / 1e6,
        bytes as f64 / secs / MIB as f64
    );
}

/// Copies the layer's dirty entries as a flush batch would (key, then coin bytes).
fn copy_dirty(layer: &ShardedLayer, out: &mut Vec<u8>) -> (u64, u64) {
    let mut entries = 0;
    out.clear();
    for (k, _, coin) in layer.iter_dirty() {
        if out.len() + 36 + coin.len() > out.capacity() {
            out.clear();
        }
        out.extend_from_slice(&k.0);
        out.extend_from_slice(coin);
        entries += 1;
    }
    black_box(&out);
    (entries, layer.dirty_bytes())
}

/// The DSTRACE1 trace (ST1's UTXO churn format): per block, its spends and creates in order.
/// rev3's gate (CH2 §2.3) replays it block by block.
struct Trace {
    data: Vec<u8>,
}

/// Where the events start, after the magic and the header.
const TRACE_EVENTS: usize = 52;

impl Trace {
    fn load(path: &str) -> Self {
        let data = std::fs::read(path).expect("trace");
        assert_eq!(&data[..8], b"DSTRACE1", "trace magic");
        Self {
            data,
        }
    }

    fn u32_at(&self, pos: usize) -> u32 {
        u32::from_le_bytes(self.data[pos..pos + 4].try_into().unwrap())
    }
}

enum Event<'a> {
    Spend(OutPointKey),
    Create(OutPointKey, &'a [u8]),
}

/// A replay position. Past the trace's end the replay starts over as a new epoch whose keys have
/// their first word mixed by the epoch, so every epoch's coins are fresh.
struct Cursor<'a> {
    t: &'a Trace,
    pos: usize,
    epoch: u64,
}

impl<'a> Cursor<'a> {
    fn new(t: &'a Trace) -> Self {
        Self {
            t,
            pos: TRACE_EVENTS,
            epoch: 0,
        }
    }

    /// The passes that are over.
    fn passes(&self) -> u64 {
        self.epoch + u64::from(self.t.u32_at(self.pos) == u32::MAX)
    }

    /// The next block's events, into `out`.
    fn block(&mut self, out: &mut Vec<Event<'a>>) {
        out.clear();
        if self.t.u32_at(self.pos) == u32::MAX {
            self.pos = TRACE_EVENTS;
            self.epoch += 1;
        }
        let n = self.t.u32_at(self.pos + 4);
        self.pos += 8;
        let d = &self.t.data;
        for _ in 0..n {
            let tag = d[self.pos];
            let mut k = OutPointKey(d[self.pos + 1..self.pos + 37].try_into().unwrap());
            if self.epoch > 0 {
                let w = u64::from_le_bytes(k.0[..8].try_into().unwrap()) ^ mix(self.epoch);
                k.0[..8].copy_from_slice(&w.to_le_bytes());
            }
            self.pos += 37;
            if tag == 0 {
                out.push(Event::Spend(k));
            } else {
                let len =
                    usize::from(u16::from_le_bytes(d[self.pos..self.pos + 2].try_into().unwrap()));
                out.push(Event::Create(k, &d[self.pos + 2..self.pos + 2 + len]));
                self.pos += 2 + len;
            }
        }
    }
}

/// One event into active, as Core's cache leaves it: a create is DIRTY | FRESH (DIRTY alone over
/// a DIRTY tombstone); a spend erases a FRESH entry and otherwise leaves a DIRTY tombstone (a coin
/// in F or on disk is fetched, then spent).
fn apply(a: &mut ShardedLayer, ev: &Event<'_>) {
    match *ev {
        Event::Create(k, coin) => {
            let fresh = !a.get(&k).is_some_and(|e| e.flags.is_dirty());
            let flags = if fresh {
                Flags::DIRTY | Flags::FRESH
            } else {
                Flags::DIRTY
            };
            a.upsert(&k, flags, coin);
        }
        Event::Spend(k) => {
            if a.get(&k).is_some_and(|e| e.flags.contains(Flags::FRESH)) {
                a.remove(&k);
            } else {
                a.upsert(&k, Flags::DIRTY | Flags::SPENT, &[]);
            }
        }
    }
}

fn tombstones(layer: &ShardedLayer) -> usize {
    layer.iter().filter(|(_, e)| e.flags.is_spent()).count()
}

/// Replays `t` block by block for budget `b` until V must wait, with the time spent in ops that
/// rebuilt a shard, per block that had one.
fn trace_build(t: &Trace, b: u64) -> (Point, Vec<f64>) {
    let mut cur = Cursor::new(t);
    let mut evs = Vec::new();
    let mut rebuild_secs = Vec::new();
    let p = build_with(b, |active| {
        cur.block(&mut evs);
        let mut secs = 0.0;
        for e in &evs {
            let before = active.rebuilds().count;
            let t = Instant::now();
            apply(active, e);
            if active.rebuilds().count != before {
                secs += t.elapsed().as_secs_f64();
            }
        }
        if secs > 0.0 {
            rebuild_secs.push(secs);
        }
    });
    (p, rebuild_secs)
}

/// The `q` quantile of `xs` (nearest rank), 0 if empty.
fn quantile(xs: &mut [f64], q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_unstable_by(f64::total_cmp);
    xs[((q * xs.len() as f64).ceil() as usize).clamp(1, xs.len()) - 1]
}

/// The gate's measurement: the mainnet trace block by block, every budget.
fn trace_sweep(path: &str) {
    let t = Trace::load(path);
    println!(
        "# coins_layer_density trace sweep (rev4): {path}, f = 1/2, R = S + 64 KiB, salt fixed"
    );
    println!(
        "B_bytes\tN_res\tr\tN1\tstop1\tN2\ttombstones\tsigma_wait\tsigma_max\tpeak\tgrew_no_F\tgrew_F\tblocks\trebuilds\trebuilt_entries\trebuild_ms\trebuild_blocks\tblock_ms_p50\tblock_ms_p99\tblock_ms_max\tN_core\talloc1\ttables1\tpresize2\tbnd_shards\tbnd_blocks\tbnd_ms_p50\tbnd_ms_p99\tbnd_ms_max"
    );
    sweep_table(|b| {
        let (mut x, mut per_block) = trace_build(&t, b);
        let mut ms = |q| quantile(&mut per_block, q) * 1e3;
        let (p50, p99, max) = (ms(0.5), ms(0.99), ms(1.0));
        let bnd = &mut x.boundary_rebuild_secs;
        let (b50, b99, bmax) =
            (quantile(bnd, 0.5) * 1e3, quantile(bnd, 0.99) * 1e3, quantile(bnd, 1.0) * 1e3);
        let [f, a] = &x.filled;
        let (n1, n2) = (f.layer.len(), a.layer.len());
        let n = n1 + n2;
        let (r1, r2) = (f.layer.rebuilds(), a.layer.rebuilds());
        // F is as it froze, and both layers as they were where V waited.
        println!(
            "{b}\t{n}\t{:.2}\t{n1}\t{:?}\t{n2}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.1}\t{}\t{p50:.3}\t{p99:.3}\t{max:.3}\t{:.0}\t{}\t{}\t{}\t{}\t{}\t{b50:.3}\t{b99:.3}\t{bmax:.3}",
            b as f64 / n as f64,
            f.stop,
            tombstones(&f.layer) + tombstones(&a.layer),
            f.layer.charged_bytes() + a.layer.charged_bytes() + sub_batch_reserve(b),
            x.sigma_max,
            x.peak,
            f.doublings,
            a.doublings,
            x.boundaries,
            r1.count + r2.count,
            r1.entries + r2.entries,
            per_block.iter().sum::<f64>() * 1e3,
            per_block.len(),
            n_core(b),
            f.layer.allocated_bytes(),
            f.layer.table_bytes(),
            presize_after(b, f.layer.charged_bytes()),
            x.boundary_rebuilt_shards,
            x.boundary_rebuild_secs.len(),
        );
        (n, x)
    });
}

/// One [`stall_run`]'s counts.
#[derive(Default)]
struct Stalls {
    freezes: u32,
    stalls: u32,
    /// Stalls at a boundary where V has already stalled.
    doubles: u32,
    /// V's time waiting for W, in events.
    wait: u64,
    /// V's time applying events.
    events: u64,
    /// Shard doublings in blocks applied with no F in flight.
    grew: u32,
}

/// V's stalls over `passes` passes of `t` for budget `b` (CH2 §2.3, rev5, O-R6-m6), on a clock
/// that counts events: V takes one unit per event, and W writes each F in `rho` times the events V
/// applied while F was active. V never waits for W otherwise. At a boundary: F is released if W is
/// done; with no F in flight THRESHOLD freezes active, and the new active is presized to what is
/// left beside F (`left`) or in full; then while Σ > B, V stalls until W releases F, the clock
/// moving on to the release, and the freeze rule runs again.
fn stall_run(t: &Trace, b: u64, passes: u64, rho: f64, left: bool) -> Stalls {
    let fresh = |frozen| {
        ShardedLayer::new(
            if left {
                presize_after(b, frozen)
            } else {
                presize(b)
            },
            SALT,
        )
    };
    let mut cur = Cursor::new(t);
    let mut evs = Vec::new();
    let mut active = ShardedLayer::new(presize(b), SALT);
    // F, and the time at which W has written it.
    let mut f: Option<(ShardedLayer, u64)> = None;
    let mut s = Stalls::default();
    // The clock, and the events applied to active.
    let (mut now, mut filled) = (0, 0);
    while cur.passes() < passes {
        cur.block(&mut evs);
        let was = f.is_none().then(|| buckets(&active));
        evs.iter().for_each(|e| apply(&mut active, e));
        if let Some(was) = was {
            s.grew += doublings(&was, &buckets(&active));
        }
        let n = evs.len() as u64;
        (now, filled, s.events) = (now + n, filled + n, s.events + n);
        if f.as_ref().is_some_and(|&(_, done)| now >= done) {
            f = None;
        }
        let mut here = 0;
        loop {
            if f.is_none() && threshold(&active, b).is_some() {
                let next = fresh(active.charged_bytes());
                let frozen = std::mem::replace(&mut active, next);
                f = Some((frozen, now + (rho * filled as f64).ceil() as u64));
                (filled, s.freezes) = (0, s.freezes + 1);
            }
            let sigma = active.charged_bytes()
                + f.as_ref().map_or(0, |(l, _)| l.charged_bytes() + sub_batch_reserve(b));
            if sigma <= b {
                break;
            }
            // Active alone past B has frozen above, so F is in flight: V waits for its release.
            let (_, done) = f.take().expect("Σ past B with no F in flight");
            s.wait += done.saturating_sub(now);
            now = now.max(done);
            s.stalls += 1;
            s.doubles += u32::from(here > 0);
            here += 1;
        }
        if f.is_none() {
            active.boundary_rebuild();
        }
    }
    s
}

/// [`stall_run`] at every budget, ρ ∈ {0.5, 1, 2, 4}, both presize rules, a thread per budget.
fn stalls(path: &str, passes: u64) {
    const RHOS: [f64; 4] = [0.5, 1.0, 2.0, 4.0];
    let t = Trace::load(path);
    println!(
        "# coins_layer_density stalls (rev5, O-R6-m6): {path}, passes = {passes}, f = 1/2, salt fixed"
    );
    println!(
        "B_bytes\trho\tevents\tfreezes_left\tstalls_left\tdoubles_left\twait_left\tgrew_left\tfreezes_full\tstalls_full\tdoubles_full\twait_full\tgrew_full"
    );
    let rows: Vec<Vec<String>> = std::thread::scope(|sc| {
        let runs: Vec<_> = sweep_points()
            .into_iter()
            .map(|b| {
                let t = &t;
                sc.spawn(move || {
                    alloc_tag::set_thread_domain(alloc_tag::new_domain());
                    RHOS.map(|rho| {
                        let [l, f] = [true, false].map(|left| stall_run(t, b, passes, rho, left));
                        format!(
                            "{b}\t{rho}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                            l.events,
                            l.freezes,
                            l.stalls,
                            l.doubles,
                            l.wait,
                            l.grew,
                            f.freezes,
                            f.stalls,
                            f.doubles,
                            f.wait,
                            f.grew
                        )
                    })
                    .to_vec()
                })
            })
            .collect();
        runs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    rows.iter().flatten().for_each(|r| println!("{r}"));
}

/// The worst single block (CH2 §2.3, rev4) at every budget: active, with no F in flight, filled one
/// coin a boundary to one coin short of its THRESHOLD freeze, then one transaction's
/// `MAX_OUTPUTS_PER_BLOCK` outputs of value 0 with an empty script (9 serialized bytes each, the
/// most a 2 MB block holds). Reports the growth the block keeps, the transient above that, and
/// Σ once the layer freezes with R and a new active beside it, presized in full or to what is left.
fn worst() {
    let s = Stream::new();
    let empty = TxOut {
        value: 0,
        script_pubkey: Vec::new().into(),
    };
    let coin = PackedCoin::from_output(&empty, 453_372, false).unwrap();
    println!(
        "# coins_layer_density worst block (rev4): {MAX_OUTPUTS_PER_BLOCK} outputs of {} coin bytes, f = 1/2, salt fixed",
        coin.as_bytes().len()
    );
    println!(
        "B_bytes\tN_before\tnext_stop\tbefore\tpeak\tafter\tkept\ttransient\tdoublings\tmost_per_shard\tblock_ms\tsigma_frozen\tabove_B\tsigma_frozen_left\tabove_B_left"
    );
    let mut buf = Vec::new();
    for b in sweep_points() {
        let domain = alloc_tag::new_domain();
        alloc_tag::set_thread_domain(domain);
        let fill = |n: u64, buf: &mut Vec<u8>| {
            let mut l = ShardedLayer::new(presize(b), SALT);
            let mut i = 0;
            while i < n && threshold(&l, b).is_none() {
                l.upsert(&Stream::key(i), Flags::DIRTY | Flags::FRESH, s.coin(i, buf));
                i += 1;
            }
            (l, i)
        };
        // The coins at which THRESHOLD holds, then a layer one short of them.
        let (dry, at) = fill(u64::MAX, &mut buf);
        let next_stop = threshold(&dry, b).unwrap();
        drop(dry);
        let (mut l, n) = fill(at - 1, &mut buf);
        assert!(threshold(&l, b).is_none());
        let before = l.allocated_bytes();
        let was = buckets(&l);
        alloc_tag::reset_peak(domain, Tag::Coins);
        let txid = Stream::key(u64::MAX).0;
        let t = Instant::now();
        for vout in 0..MAX_OUTPUTS_PER_BLOCK {
            let mut k = OutPointKey(txid);
            k.0[32..].copy_from_slice(&vout.to_le_bytes());
            l.upsert(&k, Flags::DIRTY | Flags::FRESH, coin.as_bytes());
        }
        let block_ms = t.elapsed().as_secs_f64() * 1e3;
        let after = l.allocated_bytes();
        assert_eq!(alloc_tag::tagged_bytes(domain, Tag::Coins) as u64, after);
        let peak = alloc_tag::peak_bytes(domain, Tag::Coins) as u64;
        let now = buckets(&l);
        let most = was.iter().zip(&now).map(|(w, n)| (n / w).ilog2()).max().unwrap();
        let charged = l.charged_bytes();
        let frozen =
            |cap| charged + ShardedLayer::new(cap, SALT).charged_bytes() + sub_batch_reserve(b);
        let (full, left) = (frozen(presize(b)), frozen(presize_after(b, charged)));
        println!(
            "{b}\t{n}\t{next_stop:?}\t{before}\t{peak}\t{after}\t{}\t{}\t{}\t{most}\t{block_ms:.1}\t{full}\t{}\t{left}\t{}",
            after - before,
            peak - after,
            doublings(&was, &now),
            full > b,
            left > b,
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|a| a == "--bench");
    let args: Vec<&str> =
        args.iter().map(String::as_str).filter(|a| !a.starts_with("--")).collect();
    let arg = |i: usize| args.get(i).copied();
    match arg(0) {
        None if !bench => {}
        None | Some("sweep") => sweep(),
        Some("worst") => worst(),
        Some("stalls") => {
            stalls(arg(1).expect("trace path"), arg(2).unwrap_or("1").parse().unwrap())
        }
        Some("latency") => latency(arg(1).expect("lookup"), arg(2).expect("n").parse().unwrap()),
        Some("timing") => timing(arg(1).unwrap_or("20000000").parse().unwrap()),
        Some("trace") => trace_sweep(arg(1).expect("trace path")),
        Some(m) => panic!("unknown mode {m}: sweep | trace | worst | stalls | latency | timing"),
    }
}
