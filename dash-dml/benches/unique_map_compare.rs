//! A one-time comparison of the unique map's container and hasher (EV2 §5.5 row
//! `crit:mnlist_unique_probe`, DEC-50(3), DEC-182). Recorded in `ARCHITECTURE.md`, not a PF0
//! row: it is in no `workloads.toml` line and it does not use criterion.
//!
//! It compares three maps over `UniqueKey -> UniqueSlot`, the types `UniqueMap` holds:
//!
//! - `sip`: `imbl::HashMap` with std `RandomState` (keyed SipHash-1-3, the shipped choice);
//! - `fx`: `imbl::GenericHashMap` with `BuildHasherDefault<FxHasher>`, rustc-hash 2.1.3, the
//!   hasher the reviewer's collision fixture (EV2 rev1, GPT F3) was built against;
//! - `ord`: `imbl::OrdMap`.
//!
//! The key sets all start from the 4,000 masternodes of `testkit::numbered` (collateral,
//! owner key id, operator key, legacy service, and the platform node id of each Evo node:
//! about 16,500 keys) and differ in the 512 keys added to them:
//!
//! - `normal` (a): none; the probed and removed keys are the listed ones, the added keys and
//!   the probe misses are those of 4,000 further masternodes;
//! - `collide` (b): 512 IPv6 services that collide under rustc-hash 2.1.3 (`2600:` + 8 colliding
//!   bytes + 6 index bytes, port 19999), added to the list; the operations touch only them (and
//!   512 more of the same family that are not listed);
//! - `benign` (c): the same 18-byte shape with the colliding bytes replaced by the index, so the
//!   hash does not collide. It separates the key shape from the collisions.
//!
//! Each operation runs against a map holding the list: a probe is `get`; an add clones the map
//! (as a block's list clone does), `get`s and then `insert`s, as `UniqueMap::add` does; a
//! remove clones, `get`s and `remove`s, as `UniqueMap::delete` does. The clone and the drop of
//! the path-copied nodes are inside the measurement, in every map.
//!
//! ```text
//! RUSTC_WRAPPER= cargo bench -p dash-dml --bench unique_map_compare -- --compare
//! ```
//!
//! The driver re-runs this binary as a worker under `perf stat -e instructions:u` for every
//! map, key set and operation, once with no operations and once with [`OPS`], and divides the
//! difference by [`OPS`]: the setup (the 8,000 masternodes, the map's build) cancels. That is
//! repeated [`REPS`] times, and each cell is the median with the range. The instruction count
//! varies between runs only through `RandomState`'s random key. The driver then prints the
//! table, the largest number of keys sharing one hash value (the 32 bits imbl uses), the most
//! equality (or ordering) comparisons one `get` made, and the two design criteria.

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasher, BuildHasherDefault, Hash};
use std::hint::black_box;
use std::process::Command;

use dash_dml::testkit::numbered;
use dash_dml::{ProTxHash, UniqueKey, UniqueSlot};
use dash_primitives::{AddrV2, NetInfo, ServiceV2};
use rustc_hash::FxHasher;

/// About mainnet's size (≈ 3,900 masternodes at 2.3M), as `benches/mnlist.rs`.
const MASTERNODES: u32 = 4_000;
/// The operations per measured run (EV2 §5.5: 10⁵).
const OPS: u64 = 100_000;
/// The repetitions of every measurement.
const REPS: usize = 5;
/// The colliding services of the reviewer's fixture.
const COLLIDERS: u64 = 512;
/// The port of the fixture's services.
const FIXTURE_PORT: u16 = 19_999;
/// The fixture's colliding bytes (the second seed word of rustc-hash 2.1.3's `hash_bytes`).
const COLLIDING_BYTES: u64 = 0x1319_8a2e_0370_7344;

type FxMap<K> = imbl::GenericHashMap<
    K,
    UniqueSlot,
    BuildHasherDefault<FxHasher>,
    imbl::shared_ptr::DefaultSharedPtr,
>;

/// The few operations `UniqueMap` needs of a container.
trait Table<K>: Clone + Default {
    fn find(&self, key: &K) -> bool;
    fn put(&mut self, key: K, slot: UniqueSlot);
    fn take(&mut self, key: &K);
}

impl<K: Hash + Eq + Clone> Table<K> for imbl::HashMap<K, UniqueSlot> {
    fn find(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    fn put(&mut self, key: K, slot: UniqueSlot) {
        self.insert(key, slot);
    }
    fn take(&mut self, key: &K) {
        self.remove(key);
    }
}

impl<K: Hash + Eq + Clone> Table<K> for FxMap<K> {
    fn find(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    fn put(&mut self, key: K, slot: UniqueSlot) {
        self.insert(key, slot);
    }
    fn take(&mut self, key: &K) {
        self.remove(key);
    }
}

impl<K: Ord + Clone> Table<K> for imbl::OrdMap<K, UniqueSlot> {
    fn find(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    fn put(&mut self, key: K, slot: UniqueSlot) {
        self.insert(key, slot);
    }
    fn take(&mut self, key: &K) {
        self.remove(key);
    }
}

fn slot() -> UniqueSlot {
    UniqueSlot {
        owner: ProTxHash::from([1; 32]),
        refs: 1,
    }
}

/// The keys of masternodes `range`, as the list holds them: collateral, owner key id, operator
/// key, service, and the platformNodeID of an Evo node.
fn normal(range: std::ops::Range<u32>) -> Vec<UniqueKey> {
    let mut keys = Vec::new();
    for i in range {
        let dmn = numbered(i);
        keys.push(UniqueKey::collateral(&dmn.collateral));
        keys.push(UniqueKey::key_id(&dmn.cold.key_id_owner));
        keys.push(UniqueKey::operator_key(&dmn.cold.pub_key_operator));
        match &*dmn.cold.net_info {
            NetInfo::Legacy(v1) => keys.push(UniqueKey::service_v1(&v1.0)),
            NetInfo::Extended(_) => unreachable!("`numbered` builds legacy services"),
        }
        if dmn.is_evo() {
            keys.push(UniqueKey::platform_node_id(&dmn.cold.platform_node_id));
        }
    }
    keys
}

/// Service `i` of the fixture family: `2600:` + 8 bytes + the low 6 bytes of `i`, port 19999.
/// The 8 bytes are the colliding constant, or `i` when `collide` is false.
fn fixture(i: u64, collide: bool) -> UniqueKey {
    let mut ip = [0u8; 16];
    ip[..2].copy_from_slice(&[0x26, 0x00]);
    ip[2..10].copy_from_slice(
        &(if collide {
            COLLIDING_BYTES
        } else {
            i
        })
        .to_le_bytes(),
    );
    ip[10..16].copy_from_slice(&i.to_le_bytes()[..6]);
    UniqueKey::service(&ServiceV2 {
        addr: AddrV2::Ipv6(ip),
        port: FIXTURE_PORT,
    })
}

/// The keys listed, the ones an operation can hit (listed) and the ones it cannot (not listed).
struct Keys {
    base: Vec<UniqueKey>,
    present: Vec<UniqueKey>,
    absent: Vec<UniqueKey>,
}

fn key_set(name: &str) -> Keys {
    let listed = normal(0..MASTERNODES);
    match name {
        "normal" => Keys {
            present: listed.clone(),
            absent: normal(MASTERNODES..2 * MASTERNODES),
            base: listed,
        },
        "collide" | "benign" => {
            let collide = name == "collide";
            let present: Vec<_> = (0..COLLIDERS).map(|i| fixture(i, collide)).collect();
            let absent = (COLLIDERS..2 * COLLIDERS).map(|i| fixture(i, collide)).collect();
            let mut base = listed;
            base.extend(present.iter().cloned());
            Keys {
                base,
                present,
                absent,
            }
        }
        other => panic!("unknown key set {other}"),
    }
}

/// A fixed sequence (xorshift), the same in every process.
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 as usize
    }
}

/// The worker: builds the map, runs `ops` operations and returns how many did their work (a
/// hit, an insert, a removal), which the driver checks.
fn run<T: Table<UniqueKey>>(keys: &Keys, op: &str, ops: u64) -> u64 {
    let mut base = T::default();
    for key in &keys.base {
        base.put(key.clone(), slot());
    }
    let mut rng = Xorshift(0x2545_f491_4f6c_dd1d);
    let mut done = 0u64;
    for j in 0..ops {
        let r = rng.next();
        match op {
            // Half hits, half misses.
            "probe" => {
                let key = if j % 2 == 0 {
                    &keys.present[r % keys.present.len()]
                } else {
                    &keys.absent[r % keys.absent.len()]
                };
                done += u64::from(base.find(black_box(key)));
            }
            "add" => {
                let key = &keys.absent[r % keys.absent.len()];
                let mut next = base.clone();
                if !next.find(key) {
                    next.put(key.clone(), slot());
                    done += 1;
                }
                black_box(next);
            }
            "remove" => {
                let key = &keys.present[r % keys.present.len()];
                let mut next = base.clone();
                if next.find(key) {
                    next.take(key);
                    done += 1;
                }
                black_box(next);
            }
            other => panic!("unknown operation {other}"),
        }
    }
    done
}

fn worker(map: &str, set: &str, op: &str, ops: u64) -> u64 {
    let keys = key_set(set);
    match map {
        "sip" => run::<imbl::HashMap<UniqueKey, UniqueSlot>>(&keys, op, ops),
        "fx" => run::<FxMap<UniqueKey>>(&keys, op, ops),
        "ord" => run::<imbl::OrdMap<UniqueKey, UniqueSlot>>(&keys, op, ops),
        other => panic!("unknown map {other}"),
    }
}

thread_local! {
    static COMPARES: Cell<u64> = const { Cell::new(0) };
}

/// A key that counts its equality and ordering comparisons.
#[derive(Clone)]
struct Counted(UniqueKey);

impl PartialEq for Counted {
    fn eq(&self, other: &Self) -> bool {
        COMPARES.with(|c| c.set(c.get() + 1));
        self.0 == other.0
    }
}
impl Eq for Counted {}
impl Hash for Counted {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state)
    }
}
impl PartialOrd for Counted {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Counted {
    fn cmp(&self, other: &Self) -> Ordering {
        COMPARES.with(|c| c.set(c.get() + 1));
        self.0.cmp(&other.0)
    }
}

/// The most comparisons one `get` made, and the mean, over every listed and unlisted key.
fn compares<T: Table<Counted>>(keys: &Keys) -> (u64, f64) {
    let mut map = T::default();
    for key in &keys.base {
        map.put(Counted(key.clone()), slot());
    }
    let (mut max, mut total, mut n) = (0, 0, 0u64);
    for key in keys.present.iter().chain(&keys.absent) {
        let key = Counted(key.clone());
        let before = COMPARES.with(Cell::get);
        black_box(map.find(&key));
        let used = COMPARES.with(Cell::get) - before;
        max = max.max(used);
        total += used;
        n += 1;
    }
    (max, total as f64 / n as f64)
}

/// The largest number of listed keys sharing one hash value, at the 32 bits imbl uses.
fn max_bucket<S: BuildHasher>(hasher: &S, keys: &[UniqueKey]) -> usize {
    let mut buckets: BTreeMap<u32, usize> = BTreeMap::new();
    for key in keys {
        *buckets.entry(hasher.hash_one(key) as u32).or_default() += 1;
    }
    buckets.values().copied().max().unwrap_or(0)
}

fn distinct_hashes<S: BuildHasher>(hasher: &S, keys: &[UniqueKey]) -> usize {
    keys.iter().map(|k| hasher.hash_one(k)).collect::<BTreeSet<_>>().len()
}

/// `perf stat`'s user-space instruction count of one worker run, and the worker's output.
fn measure(map: &str, set: &str, op: &str, ops: u64) -> (u64, u64) {
    let exe = std::env::current_exe().expect("this binary's path");
    let out = Command::new("perf")
        .args(["stat", "-x,", "-e", "instructions:u", "--"])
        .arg(exe)
        .args(["--worker", map, set, op, &ops.to_string()])
        .output()
        .expect("perf stat runs");
    assert!(out.status.success(), "{map} {set} {op}: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let instructions = stderr
        .lines()
        .find(|line| line.contains("instructions"))
        .and_then(|line| line.split(',').next())
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or_else(|| panic!("no instruction count in perf's output: {stderr}"));
    let done = String::from_utf8_lossy(&out.stdout).trim().parse().expect("the worker's count");
    (instructions, done)
}

/// The instructions per operation of `REPS` runs.
fn per_op(map: &str, set: &str, op: &str) -> Vec<f64> {
    let expected = match op {
        "probe" => OPS / 2,
        _ => OPS,
    };
    (0..REPS)
        .map(|_| {
            let (setup, none) = measure(map, set, op, 0);
            let (total, done) = measure(map, set, op, OPS);
            assert_eq!(none, 0);
            assert_eq!(done, expected, "{map} {set} {op}: operations that did their work");
            (total as f64 - setup as f64) / OPS as f64
        })
        .collect()
}

fn median(values: &[f64]) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[sorted.len() / 2]
}

fn spread(values: &[f64]) -> String {
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    format!("{:.0} ({:.0}-{:.0})", median(values), min, max)
}

fn compare() {
    const MAPS: [(&str, &str); 3] = [
        ("sip", "imbl::HashMap, SipHash-1-3 (shipped)"),
        ("fx", "GenericHashMap, FxHasher 2.1.3"),
        ("ord", "imbl::OrdMap"),
    ];
    const SETS: [(&str, &str); 3] = [
        ("normal", "(a) normal"),
        ("collide", "(b) 512 Fx-colliding services"),
        ("benign", "(c) 512 same-shape, non-colliding"),
    ];
    const OPERATIONS: [&str; 3] = ["probe", "add", "remove"];

    // The fixture really collides under this crate's `UniqueKey` layout and `Hash` impl.
    let fx = BuildHasherDefault::<FxHasher>::default();
    let sip = RandomState::new();
    for (name, collide) in [("colliding", true), ("benign", false)] {
        let keys: Vec<_> = (0..COLLIDERS).map(|i| fixture(i, collide)).collect();
        let distinct: BTreeSet<_> = keys.iter().map(UniqueKey::as_bytes).collect();
        println!(
            "fixture check, {name}: {} distinct keys of {} bytes; {} distinct Fx hashes, {} SipHash hashes",
            distinct.len(),
            keys[0].as_bytes().len(),
            distinct_hashes(&fx, &keys),
            distinct_hashes(&sip, &keys),
        );
        if collide {
            assert_eq!(distinct_hashes(&fx, &keys), 1, "the fixture does not collide under Fx");
        }
    }

    println!(
        "\n{OPS} operations per run, {REPS} runs per cell; instructions per operation, median (min-max)\n"
    );
    println!("| keys | map | probe | add | remove |");
    println!("|---|---|---|---|---|");
    let mut medians: BTreeMap<(&str, &str, &str), f64> = BTreeMap::new();
    for (set, set_label) in SETS {
        for (map, map_label) in MAPS {
            let mut cells = Vec::new();
            for op in OPERATIONS {
                let runs = per_op(map, set, op);
                medians.insert((set, map, op), median(&runs));
                cells.push(spread(&runs));
            }
            println!("| {set_label} | {map_label} | {} |", cells.join(" | "));
        }
    }

    println!(
        "\n| keys | map | max keys sharing a hash | max comparisons per get | mean comparisons per get |"
    );
    println!("|---|---|---|---|---|");
    for (set, set_label) in SETS {
        let keys = key_set(set);
        let (sip_max, sip_cmp) = compares::<imbl::HashMap<Counted, UniqueSlot>>(&keys);
        let (fx_max, fx_cmp) = compares::<FxMap<Counted>>(&keys);
        let (ord_max, ord_cmp) = compares::<imbl::OrdMap<Counted, UniqueSlot>>(&keys);
        // The SipHash key is random per process: the worst of 20 keys.
        let sip_bucket = (0..20).map(|_| max_bucket(&RandomState::new(), &keys.base)).max();
        println!(
            "| {set_label} | SipHash | {} (worst of 20 keys) | {sip_max} | {sip_cmp:.2} |",
            sip_bucket.unwrap_or(0)
        );
        println!("| {set_label} | Fx | {} | {fx_max} | {fx_cmp:.2} |", max_bucket(&fx, &keys.base));
        println!("| {set_label} | OrdMap | n/a | {ord_max} | {ord_cmp:.2} |");
    }

    println!("\ncriteria (EV2 §5.5 as DEC-182 amends it):");
    for op in OPERATIONS {
        let ratio = medians[&("collide", "sip", op)] / medians[&("normal", "sip", op)];
        println!(
            "- SipHash (b) within 2x of SipHash (a), {op}: {ratio:.2}x, {}",
            if ratio <= 2.0 {
                "PASS"
            } else {
                "FAIL"
            }
        );
    }
    for op in OPERATIONS {
        let ratio = medians[&("collide", "sip", op)] / medians[&("benign", "sip", op)];
        println!("- (info) SipHash (b) over (c), {op}: {ratio:.2}x");
    }
    // DEC-182 withdrew §5.5's SipHash <= 1.2x Fx bound; the ratio stays on record.
    for op in OPERATIONS {
        let ratio = medians[&("normal", "sip", op)] / medians[&("normal", "fx", op)];
        println!("- (info) SipHash over Fx on (a), {op}: {ratio:.2}x");
    }
    for op in OPERATIONS {
        let ratio = medians[&("collide", "fx", op)] / medians[&("normal", "fx", op)];
        println!("- (info) Fx (b) over Fx (a), {op}: {ratio:.2}x");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--worker", map, set, op, ops] => {
            println!("{}", worker(map, set, op, ops.parse().expect("an operation count")))
        }
        a if a.contains(&"--compare") => compare(),
        _ => println!(
            "a one-time comparison, not a benchmark: run it with `-- --compare` (see the module doc)"
        ),
    }
}
