//! `stdsort::sort_by` against libstdc++'s `std::sort` from GCC 13.3.0
//! (`tests/data/libstdcxx_sort_trace.txt`). For each case the trace holds the number of
//! comparator calls, an FNV-1a-64 hash of the whole call sequence (the ids compared, in order)
//! and a hash of the final order, so equal elements must end where libstdc++ leaves them.
//!
//! To regenerate the trace, build `tests/data/libstdcxx_sort_ref.cpp` with GCC 13.3.0
//! (`docker run --rm -v "$PWD":/w -w /w gcc:13.3.0 g++ -O2 -std=c++20 -static -o sort_ref
//! libstdcxx_sort_ref.cpp`) and run this test with `DASH_P2P_SORT_REF=<that binary>`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command, Stdio};

use dash_p2p::stdsort::sort_by_counting_heap_fallbacks;

const TRACE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/libstdcxx_sort_trace.txt");

enum Case {
    /// (key, id) elements; `comp` compares the key.
    Single(Vec<i64>),
    /// (a, b, id) elements; `comp` is a ascending, then b descending (the shape of Core's
    /// eviction comparators).
    Multi(Vec<(i64, i64)>),
}

struct Named {
    name: String,
    case: Case,
    /// Whether the introsort depth limit must be hit.
    heap_fallback: bool,
}

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, m: u64) -> i64 {
        i64::try_from(self.next() % m).unwrap()
    }
}

/// Distinct key values; 0 is the whole `i64` range.
const RANGES: [u64; 6] = [1, 2, 3, 8, 1_000_000, 0];

fn random_keys(rng: &mut Rng, n: usize, range: u64) -> Vec<i64> {
    (0..n)
        .map(|_| {
            if range == 0 {
                rng.next() as i64
            } else {
                rng.below(range) - i64::try_from(range / 2).unwrap()
            }
        })
        .collect()
}

/// McIlroy's "A Killer Adversary for Quicksort" run against the port: the keys drive the same
/// degenerate partitions in libstdc++ (the trace proves the call sequences equal) and exhaust
/// the depth limit.
fn antiqsort_keys(n: usize) -> Vec<i64> {
    let gas = i64::try_from(n).unwrap();
    let mut val = vec![gas; n];
    let mut solid = 0;
    let mut candidate = 0;
    let mut idx: Vec<usize> = (0..n).collect();
    dash_p2p::stdsort::sort_by(&mut idx, |&x, &y| {
        if val[x] == gas && val[y] == gas {
            if x == candidate {
                val[x] = solid;
            } else {
                val[y] = solid;
            }
            solid += 1;
        }
        if val[x] == gas {
            candidate = x;
        } else if val[y] == gas {
            candidate = y;
        }
        val[x] < val[y]
    });
    val
}

fn cases() -> Vec<Named> {
    let mut cases = Vec::new();
    let mut push = |name: String, case, heap_fallback| {
        cases.push(Named {
            name,
            case,
            heap_fallback,
        })
    };
    for n in 0..=100usize {
        for range in RANGES {
            for seed in 0..2u64 {
                let mut rng = Rng(seed * 1_000_003 + n as u64 * 7919 + range);
                let keys = random_keys(&mut rng, n, range);
                push(format!("rand/{n}/{range}/{seed}"), Case::Single(keys), false);
            }
        }
    }
    for n in [1000usize, 5000] {
        for range in RANGES {
            let mut rng = Rng(0xabcdef ^ (n as u64) << 20 ^ range);
            push(
                format!("rand/{n}/{range}/0"),
                Case::Single(random_keys(&mut rng, n, range)),
                false,
            );
        }
    }
    let sizes = (0..=40usize).chain([64, 100, 127, 128, 129, 256, 257, 1000, 5000]);
    for n in sizes {
        let up = || 0..i64::try_from(n).unwrap();
        push(format!("equal/{n}"), Case::Single(vec![42; n]), false);
        push(format!("sorted/{n}"), Case::Single(up().collect()), false);
        push(format!("reverse/{n}"), Case::Single(up().rev().collect()), false);
        push(format!("sorted-dup/{n}"), Case::Single(up().map(|i| i / 3).collect()), false);
        push(format!("reverse-dup/{n}"), Case::Single(up().rev().map(|i| i / 3).collect()), false);
        push(format!("sawtooth/{n}"), Case::Single(up().map(|i| i % 5).collect()), false);
    }
    for n in (40..=200).step_by(16).chain([1000, 5000]) {
        push(format!("antiqsort/{n}"), Case::Single(antiqsort_keys(n)), true);
    }
    for n in [200usize, 1000] {
        let keys = antiqsort_keys(n).into_iter().map(|k| k / 2).collect();
        push(format!("antiqsort-ties/{n}"), Case::Single(keys), false);
    }
    for n in (0..=100usize).step_by(3).chain([1000]) {
        for (ra, rb) in [(1u64, 1u64), (2, 2), (3, 2), (4, 8), (50, 3), (1_000_000, 1_000_000)] {
            let mut rng = Rng(n as u64 * 131 + ra * 7 + rb);
            let v = (0..n).map(|_| (rng.below(ra), rng.below(rb))).collect();
            push(format!("multi/{n}/{ra}/{rb}"), Case::Multi(v), false);
        }
    }
    cases
}

fn fnv(hash: &mut u64, x: u32) {
    for b in x.to_le_bytes() {
        *hash ^= u64::from(b);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// Runs the port: (comparator calls, call-sequence hash, final ids, heap fallbacks).
fn run(case: &Case) -> (u64, u64, Vec<u32>, usize) {
    let mut hash = FNV_OFFSET;
    let mut calls = 0u64;
    let mut trace = |x: u32, y: u32| {
        calls += 1;
        fnv(&mut hash, x);
        fnv(&mut hash, y);
    };
    let id = |i: usize| u32::try_from(i).unwrap();
    let (ids, fallbacks) = match case {
        Case::Single(keys) => {
            let mut v: Vec<(i64, u32)> =
                keys.iter().enumerate().map(|(i, &k)| (k, id(i))).collect();
            let fb = sort_by_counting_heap_fallbacks(&mut v, |x, y| {
                trace(x.1, y.1);
                x.0 < y.0
            });
            (v.iter().map(|e| e.1).collect(), fb)
        }
        Case::Multi(keys) => {
            let mut v: Vec<(i64, i64, u32)> =
                keys.iter().enumerate().map(|(i, &(a, b))| (a, b, id(i))).collect();
            let fb = sort_by_counting_heap_fallbacks(&mut v, |x, y| {
                trace(x.2, y.2);
                if x.0 != y.0 {
                    return x.0 < y.0;
                }
                x.1 > y.1
            });
            (v.iter().map(|e| e.2).collect(), fb)
        }
    };
    (calls, hash, ids, fallbacks)
}

fn line(name: &str, calls: u64, hash: u64, ids: &[u32]) -> String {
    let mut order = FNV_OFFSET;
    for &i in ids {
        fnv(&mut order, i);
    }
    format!("{name} {calls} {hash:016x} {order:016x}")
}

/// The reference harness's input (see `libstdcxx_sort_ref.cpp`).
fn encode(cases: &[Named]) -> String {
    let mut s = String::new();
    for c in cases {
        match &c.case {
            Case::Single(keys) => {
                write!(s, "S {}", keys.len()).unwrap();
                for k in keys {
                    write!(s, " {k}").unwrap();
                }
            }
            Case::Multi(keys) => {
                write!(s, "M {}", keys.len()).unwrap();
                for (a, b) in keys {
                    write!(s, " {a} {b}").unwrap();
                }
            }
        }
        s.push('\n');
    }
    s
}

/// Runs the reference binary and rewrites the trace.
fn record(bin: &str, cases: &[Named]) {
    let mut child = Command::new(bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("run the reference harness");
    let input = encode(cases);
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()).unwrap());
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(out.status.success());
    let mut trace = String::new();
    for (c, l) in cases.iter().zip(String::from_utf8(out.stdout).unwrap().lines()) {
        let mut it = l.split(' ');
        let calls = it.next().unwrap().parse().unwrap();
        let hash = it.next().unwrap().parse().unwrap();
        let ids: Vec<u32> = it.map(|x| x.parse().unwrap()).collect();
        trace.push_str(&line(&c.name, calls, hash, &ids));
        trace.push('\n');
    }
    std::fs::write(TRACE, trace).unwrap();
}

#[test]
fn matches_libstdcxx_std_sort() {
    let cases = cases();
    if let Ok(bin) = std::env::var("DASH_P2P_SORT_REF") {
        record(&bin, &cases);
    }
    let trace = std::fs::read_to_string(TRACE).expect(TRACE);
    let want: Vec<&str> = trace.lines().collect();
    assert_eq!(want.len(), cases.len(), "{TRACE}: case count");
    let mut fallbacks = 0;
    for (c, want) in cases.iter().zip(want) {
        let (calls, hash, ids, fb) = run(&c.case);
        assert_eq!(line(&c.name, calls, hash, &ids), want);
        assert!(!c.heap_fallback || fb > 0, "{}: no heap fallback", c.name);
        fallbacks += usize::from(fb > 0);
    }
    assert!(fallbacks >= 13);
}
