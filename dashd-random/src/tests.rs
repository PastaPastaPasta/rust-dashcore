//! BL-502 tests 3–6, 10 and 12. Every test that counts builds its own [`RngState`] over a fake
//! [`Entropy`], so no count is shared between test threads.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use dash_p2p::rng::{FastRandom, RandSource, get_rand};
use sha2::{Digest as _, Sha512};

use crate::state::{Chunk, Hasher, add_event, extract, init_chunk, mix, seed_events, strengthen};
use crate::{Entropy, EnvSink, OsRandError, RngState, SystemEntropy, env};

/// Deterministic sources that count their calls. The OS bytes are `os_seed` and the call
/// number; the clock advances 1 ms per call.
#[derive(Default)]
struct Counting {
    os_seed: u8,
    os_fails: bool,
    os: AtomicUsize,
    hw_fast: AtomicUsize,
    hw_slow: AtomicUsize,
    perf: AtomicI64,
    steady_ms: AtomicU64,
    dynamic: AtomicUsize,
    statics: AtomicUsize,
}

impl Counting {
    fn with_os(os_seed: u8) -> Self {
        Self {
            os_seed,
            ..Self::default()
        }
    }

    fn os(&self) -> usize {
        self.os.load(Ordering::SeqCst)
    }

    fn statics(&self) -> usize {
        self.statics.load(Ordering::SeqCst)
    }

    fn steady_calls(&self) -> u64 {
        self.steady_ms.load(Ordering::SeqCst)
    }
}

impl Entropy for Counting {
    fn os_rand(&self, out: &mut [u8; 32]) -> Result<(), OsRandError> {
        let n = self.os.fetch_add(1, Ordering::SeqCst);
        if self.os_fails {
            return Err(OsRandError);
        }
        for (i, b) in out.iter_mut().enumerate() {
            *b = self.os_seed ^ (n as u8) ^ (i as u8 + 1);
        }
        Ok(())
    }

    fn hw_fast(&self) -> Option<u64> {
        self.hw_fast.fetch_add(1, Ordering::SeqCst);
        Some(0x0123_4567_89ab_cdef)
    }

    fn hw_slow(&self, out: &mut [u8; 32]) -> bool {
        self.hw_slow.fetch_add(1, Ordering::SeqCst);
        out.fill(0x5a);
        true
    }

    fn perf_counter(&self) -> i64 {
        self.perf.fetch_add(1, Ordering::SeqCst)
    }

    fn steady_now(&self) -> Duration {
        Duration::from_millis(self.steady_ms.fetch_add(1, Ordering::SeqCst))
    }

    fn dynamic_env(&self, sink: &mut dyn EnvSink) {
        self.dynamic.fetch_add(1, Ordering::SeqCst);
        sink.write(b"dynamic");
    }

    fn static_env(&self, sink: &mut dyn EnvSink) {
        self.statics.fetch_add(1, Ordering::SeqCst);
        sink.write(b"static");
    }

    fn hardware_report(&self) -> &'static [&'static str] {
        &[]
    }
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// Test 3: Core's own `RNGState` over a chain of `AddEvent`, `SeedEvents` and `MixExtract`
/// calls (tests/data/PROVENANCE.md), replayed on a zeroed state.
#[test]
fn mix_extract_and_events_match_cores_vectors() {
    let text = include_str!("../tests/data/rng_state_vectors.json");
    let steps: serde_json::Value = serde_json::from_str(text).unwrap();
    let steps = steps.as_array().unwrap();
    assert_eq!(steps.len(), 54);
    let mut c: Chunk = [0xee; crate::RNG_STATE_SIZE];
    init_chunk(&mut c);
    let (mut mixes, mut strong_seen) = (0, false);
    for (i, step) in steps.iter().enumerate() {
        match step["op"].as_str().unwrap() {
            "event" => {
                let info = step["info"].as_u64().unwrap() as u32;
                let perf = step["perf"].as_i64().unwrap();
                add_event(&mut c, info, perf);
            }
            "mix" => {
                let mut h = Hasher::new();
                if step["events"].as_bool().unwrap() {
                    seed_events(&mut c, &mut h);
                }
                h.write(&hex(step["input"].as_str().unwrap()));
                let num = step["num"].as_u64().unwrap() as usize;
                let mut buf = [0; 64];
                let ret = mix(&mut c, &mut h, step["strong"].as_bool().unwrap(), &mut buf);
                let mut out = vec![0; num];
                extract(&mut out, &mut buf);
                assert_eq!(ret, step["ret"].as_bool().unwrap(), "step {i}: ret");
                assert_eq!(out, hex(step["out"].as_str().unwrap()), "step {i}: out");
                assert_eq!(c[..32], hex(step["state"].as_str().unwrap()), "step {i}: state");
                let counter = u64::from_ne_bytes(c[32..40].try_into().unwrap());
                assert_eq!(counter, step["counter"].as_u64().unwrap(), "step {i}: counter");
                mixes += 1;
                strong_seen |= ret;
            }
            op => panic!("step {i}: unknown op {op}"),
        }
    }
    assert!(mixes == 25 && strong_seen);
}

/// Test 4, Core's `osrandom_tests`.
#[test]
fn random_sanity_check_passes() {
    assert!(crate::random_sanity_check().is_some());
}

/// Test 4, the non-deterministic half of Core's `fastrandom_tests`: draws from the process
/// state are not the deterministic context's values, and two fresh contexts differ.
#[test]
fn draws_are_not_deterministic() {
    let src: Arc<dyn RandSource> = Arc::new(RngState::new(SystemEntropy));
    for _ in 0..10 {
        assert_ne!(get_rand(&src, u64::MAX), 10393729187455219830);
        assert_ne!(get_rand(&src, i32::MAX as u64), 769702006);
        assert_ne!(get_rand(&src, 3_600_000_000), 2917185654);
        assert_ne!(get_rand(&src, 3_600_000), 2144374);
    }
    let (mut a, mut b) = (FastRandom::lazy(src.clone()), FastRandom::lazy(src.clone()));
    assert_ne!(a.rand64(), b.rand64());
    let (mut a, mut b) = (FastRandom::lazy(src.clone()), FastRandom::lazy(src.clone()));
    assert_ne!(a.rand256(), b.rand256());
    let (mut a, mut b) = (FastRandom::lazy(src.clone()), FastRandom::lazy(src));
    let (mut x, mut y) = ([0u8; 7], [0u8; 7]);
    a.fill(&mut x);
    b.fill(&mut y);
    assert_ne!(x, y);
    assert_ne!(crate::get_rand_hash(), crate::get_rand_hash());
}

/// Test 5: FAST draws read no OS bytes; STARTUP runs once, at the first draw.
#[test]
fn fast_draws_read_no_os_and_startup_runs_once() {
    let s = RngState::new(Counting::default());
    s.rand_bytes(&mut [0u8; 32]);
    // STARTUP: SLOW's OS read, hardware, both environments.
    assert_eq!((s.entropy().os(), s.entropy().statics()), (1, 1));
    assert_eq!(s.entropy().hw_slow.load(Ordering::SeqCst), 1);
    for _ in 0..99 {
        s.rand_bytes(&mut [0u8; 16]);
    }
    assert_eq!((s.entropy().os(), s.entropy().statics()), (1, 1));
    assert_eq!(s.entropy().hw_slow.load(Ordering::SeqCst), 1);
}

/// Test 5: each strong draw reads the OS once, through the typed API and through
/// `RandSource` with `strong`; `RandSource` without `strong` reads nothing.
#[test]
fn strong_draws_read_the_os_once() {
    let s = RngState::new(Counting::default());
    s.strong_rand_bytes(&mut [0u8; 32]);
    // STARTUP's read, then the draw's.
    assert_eq!(s.entropy().os(), 2);
    for i in 0..10 {
        s.strong_rand_bytes(&mut [0u8; 8]);
        assert_eq!(s.entropy().os(), 3 + i);
    }
    RandSource::rand_bytes(&s, &mut [0u8; 32], true);
    assert_eq!(s.entropy().os(), 13);
    RandSource::rand_bytes(&s, &mut [0u8; 32], false);
    assert_eq!(s.entropy().os(), 13);
    assert_eq!(s.entropy().statics(), 1);
}

/// The first strong draw of a fresh state, from one call site, so the stack address SeedFast
/// writes is the same for every state.
#[inline(never)]
fn first_strong(s: &RngState<Counting>) -> [u8; 32] {
    let mut out = [0u8; 32];
    s.strong_rand_bytes(&mut out);
    s.strong_rand_bytes(&mut out);
    out
}

/// Test 5: the OS bytes reach the output. Two states over equal sources draw equal bytes;
/// one whose OS bytes differ draws others.
#[test]
fn os_bytes_reach_the_output() {
    let draws: Vec<_> = [1u8, 1, 2]
        .into_iter()
        .map(|seed| first_strong(&RngState::new(Counting::with_os(seed))))
        .collect();
    assert_eq!(draws[0], draws[1], "the fake sources are not deterministic");
    assert_ne!(draws[0], draws[2]);
}

/// `RandSource::add_event` on the state is `RandAddEvent`: through the trait, an event changes
/// the next draw exactly as the state's own `add_event` does.
#[test]
fn add_event_through_rand_source_reaches_the_state() {
    let draws: Vec<_> = [(true, 7), (false, 7), (false, 8)]
        .into_iter()
        .map(|(via_source, info)| {
            let s = RngState::new(Counting::with_os(1));
            if via_source {
                (&s as &dyn RandSource).add_event(info);
            } else {
                s.add_event(info);
            }
            first_strong(&s)
        })
        .collect();
    assert_eq!(draws[0], draws[1]);
    assert_ne!(draws[1], draws[2]);
}

/// Test 5: PERIODIC adds the dynamic environment once and strengthens for exactly the blocks
/// 10 ms gives on a clock that advances 1 ms per read: one read for the deadline, then one per
/// block until the deadline (10 blocks).
#[test]
fn periodic_strengthens_for_its_budget() {
    let s = RngState::new(Counting::default());
    s.rand_bytes(&mut [0u8; 32]);
    let (dynamic, steady) =
        (s.entropy().dynamic.load(Ordering::SeqCst), s.entropy().steady_calls());
    // STARTUP's 100 ms: 101 clock reads.
    assert_eq!(steady, 101);
    s.add_periodic();
    assert_eq!(s.entropy().dynamic.load(Ordering::SeqCst), dynamic + 1);
    assert_eq!(s.entropy().steady_calls(), steady + 11);
    assert_eq!(s.entropy().os(), 1);
}

/// Records each write.
#[derive(Default)]
struct Recorder {
    writes: Vec<Vec<u8>>,
    items: Vec<(&'static str, Vec<usize>)>,
}

impl EnvSink for Recorder {
    fn write(&mut self, bytes: &[u8]) {
        self.writes.push(bytes.to_vec());
        if let Some((_, sizes)) = self.items.last_mut() {
            sizes.push(bytes.len());
        }
    }

    fn label(&mut self, item: &'static str) {
        self.items.push((item, Vec::new()));
    }
}

/// Test 5: `Strengthen` runs 1000 SHA-512 iterations per block, writes one counter sample per
/// block, then the inner hash.
#[test]
fn strengthen_iterates_in_blocks_of_1000() {
    let e = Counting::default();
    let seed = [7u8; 32];
    let mut rec = Recorder::default();
    strengthen(&seed, Duration::from_millis(3), &mut rec, &e);
    // Three blocks: the counter samples 0, 1, 2, then the inner hash.
    assert_eq!(rec.writes.len(), 4);
    for (i, w) in rec.writes[..3].iter().enumerate() {
        assert_eq!(w[..], (i as i64).to_ne_bytes());
    }
    let mut inner = Sha512::new();
    inner.update(seed);
    for _ in 0..3000 {
        let out = inner.finalize_reset();
        inner.update(out);
    }
    assert_eq!(rec.writes[3][..], inner.finalize()[..]);
}

/// Test 6: 16 threads drawing FAST and strong from one state get distinct outputs.
#[test]
fn concurrent_draws_are_distinct() {
    const THREADS: usize = 16;
    const DRAWS: usize = 100_000;
    let s = Arc::new(RngState::new(SystemEntropy));
    let outs: Vec<Vec<[u8; 32]>> = (0..THREADS)
        .map(|_| {
            let s = Arc::clone(&s);
            std::thread::spawn(move || {
                (0..DRAWS)
                    .map(|i| {
                        let mut out = [0u8; 32];
                        if i % 2 == 0 {
                            s.rand_bytes(&mut out);
                        } else {
                            s.strong_rand_bytes(&mut out);
                        }
                        out
                    })
                    .collect()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap())
        .collect();
    let all: HashSet<[u8; 32]> = outs.iter().flatten().copied().collect();
    assert_eq!(all.len(), THREADS * DRAWS);
}

/// Test 6's chunk view: the state's operations on a plain chunk.
#[test]
fn chunk_view_mixes_and_hashes_events() {
    let mut c: Chunk = [0; crate::RNG_STATE_SIZE];
    init_chunk(&mut c);
    for i in 0..20 {
        add_event(&mut c, i, i64::from(i) * 3);
    }
    let mut h = Hasher::new();
    seed_events(&mut c, &mut h);
    let mut out = [0; 64];
    let seeded = mix(&mut c, &mut h, true, &mut out);
    assert!(seeded);
    assert_ne!(out[..32], [0; 32]);
    assert_eq!(c[32..40], 1u64.to_ne_bytes());
    assert_eq!(c[32 + 8 + 1..48], [0; 7]);
    assert!(c[crate::events::END..].iter().all(|&b| b == 0));
}

/// `mix` and `extract` leave no copy of `H` behind: the caller's buffer is wiped by `extract`,
/// the hasher is reset, and the hasher wipes itself on drop.
#[test]
fn mix_extract_wipes_its_buffer_and_resets_the_hasher() {
    const fn wipes_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    wipes_on_drop::<Sha512>();

    let mut c: Chunk = [0; crate::RNG_STATE_SIZE];
    init_chunk(&mut c);
    let mut h = Hasher::new();
    h.write(b"input");
    let mut buf = [0; 64];
    mix(&mut c, &mut h, false, &mut buf);
    assert_ne!(buf, [0; 64]);
    assert_eq!(c[..32], buf[32..]);
    assert_eq!(h.size(), 0);
    let mut empty = [0; 64];
    h.finalize_reset_into(&mut empty);
    assert_eq!(empty[..], Sha512::digest([])[..]);

    let mut out = [0; 32];
    let first = buf;
    extract(&mut out, &mut buf);
    assert_eq!(out[..], first[..32]);
    assert_eq!(buf, [0; 64]);
}

/// Test 12: `AddFile` stops at the first read shorter than 4096 bytes.
#[test]
fn add_file_stops_at_a_short_read() {
    let mut reads = [4096isize, 100, 4096, 0].into_iter();
    let mut calls = 0;
    let mut rec = Recorder::default();
    env::add_file_contents(&mut rec, |buf| {
        calls += 1;
        let n = reads.next().unwrap();
        buf[..n.max(0) as usize].fill(calls);
        n
    });
    assert_eq!(calls, 2);
    assert_eq!(rec.writes.iter().map(Vec::len).collect::<Vec<_>>(), [4096, 100]);
    // A failed read ends the loop and writes nothing.
    let mut rec = Recorder::default();
    env::add_file_contents(&mut rec, |_| -1);
    assert!(rec.writes.is_empty());
    // Full reads stop once 1 MiB is counted.
    let mut calls = 0;
    env::add_file_contents(&mut Recorder::default(), |_| {
        calls += 1;
        4096
    });
    assert_eq!(calls, 256);
}

/// Test 12: the dynamic environment writes Core's Linux sequence: three `timespec`s, a
/// `timeval`, three clock counts, `rusage`, then each of the nine files as its descriptor, its
/// `stat` and its contents in 4096-byte reads up to the first short one, then the stack and
/// heap addresses. The fixed part is 1580 bytes.
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
#[test]
fn dynamic_env_writes_cores_linux_layout() {
    let mut rec = Recorder::default();
    SystemEntropy.dynamic_env(&mut rec);
    let mut want: Vec<&str> = vec![
        "CLOCK_MONOTONIC",
        "CLOCK_REALTIME",
        "CLOCK_BOOTTIME",
        "gettimeofday",
        "system_clock",
        "steady_clock",
        "high_resolution_clock",
        "rusage",
    ];
    want.extend(env::DYNAMIC_FILES.iter().map(|p| p.to_str().unwrap()));
    want.extend(["stack", "heap"]);
    assert_eq!(rec.items.iter().map(|(l, _)| *l).collect::<Vec<_>>(), want);
    let mut fixed = 0;
    for (label, sizes) in &rec.items {
        if label.starts_with("/proc/") {
            assert_eq!(sizes[..2], [4, 144], "{label}: the descriptor, then the stat");
            let contents = &sizes[2..];
            assert!(!contents.is_empty(), "{label}: no contents");
            let (last, full) = contents.split_last().unwrap();
            assert!(full.iter().all(|&n| n == 4096), "{label}: {contents:?}");
            assert!(*last <= 4096, "{label}: {contents:?}");
            fixed += 4 + 144;
        } else {
            let want = match *label {
                "CLOCK_MONOTONIC" | "CLOCK_REALTIME" | "CLOCK_BOOTTIME" | "gettimeofday" => 16,
                "rusage" => 144,
                _ => 8,
            };
            assert_eq!(sizes[..], [want], "{label}");
            fixed += want;
        }
    }
    assert_eq!(fixed, 1580);
}

mod abort {
    //! Test 10: the abort paths, each in a child process (this test binary run again with
    //! `CHILD` naming the path). The child must die by SIGABRT.

    use std::fs;
    use std::io::Write as _;
    use std::process::Command;
    use std::sync::Mutex;

    use dash_p2p::rng::RandSource;
    use tracing::field::{Field, Visit};
    use tracing::{Event, Metadata, Subscriber};

    use super::Counting;
    use crate::{RngState, SystemEntropy};

    const CHILD: &str = "DASHD_RANDOM_TEST_ABORT_CHILD";
    const LOG: &str = "DASHD_RANDOM_TEST_ABORT_LOG";

    /// Writes each event's message to a file at once.
    struct FileLog(Mutex<fs::File>);

    struct Message(String);

    impl Visit for Message {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0 = format!("{value:?}");
            }
        }
    }

    impl Subscriber for FileLog {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut m = Message(String::new());
            event.record(&mut m);
            let _ = writeln!(self.0.lock().unwrap(), "{}", m.0);
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    /// In the child: the path `which` names, which must not return.
    fn child(which: &str) {
        let log = fs::File::create(std::env::var(LOG).unwrap()).unwrap();
        tracing::subscriber::set_global_default(FileLog(Mutex::new(log))).unwrap();
        let failing = || {
            RngState::new(Counting {
                os_fails: true,
                ..Counting::default()
            })
        };
        match which {
            "draw" => failing().strong_rand_bytes(&mut [0u8; 32]),
            "sanity" => {
                let _ = failing().sanity_check();
            }
            "oversize" => {
                RandSource::rand_bytes(&RngState::new(SystemEntropy), &mut [0u8; 33], false)
            }
            _ => unreachable!(),
        }
        println!("returned");
    }

    /// Runs `test` in a child on path `which`: its stderr and the log it wrote.
    fn run(test: &str, which: &str) -> (String, String) {
        let dir = std::env::temp_dir();
        let log = dir.join(format!("dashd-random-abort-{which}-{}.log", std::process::id()));
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(CHILD, which)
            .env(LOG, &log)
            .output()
            .unwrap();
        let logged = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_file(&log);
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(
                out.status.signal(),
                Some(libc::SIGABRT),
                "{which}: {:?}, stdout {}, stderr {stderr}",
                out.status,
                String::from_utf8_lossy(&out.stdout)
            );
        }
        (stderr, logged)
    }

    #[test]
    fn os_read_failure_logs_then_aborts() {
        if let Ok(which) = std::env::var(CHILD) {
            return child(&which);
        }
        let test = "tests::abort::os_read_failure_logs_then_aborts";
        for which in ["draw", "sanity"] {
            let (_, logged) = run(test, which);
            assert_eq!(logged, "Failed to read randomness, aborting\n", "{which}");
        }
    }

    #[test]
    fn draw_over_32_bytes_aborts() {
        if let Ok(which) = std::env::var(CHILD) {
            return child(&which);
        }
        let (stderr, logged) = run("tests::abort::draw_over_32_bytes_aborts", "oversize");
        assert!(stderr.contains("Assertion `num <= 32' failed."), "{stderr}");
        assert_eq!(logged, "");
    }
}
