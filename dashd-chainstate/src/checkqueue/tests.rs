use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

/// `init.cpp:1554-1565`, case by case.
#[test]
fn par_follows_cores_arithmetic() {
    let rows: &[(i64, usize, usize)] = &[
        // -par=0 (auto): one fewer than the cores, at most 15.
        (0, 1, 0),
        (0, 4, 3),
        (0, 32, 15),
        // -par=1: serial; -par=2: one additional thread (the functional framework's).
        (1, 8, 0),
        (2, 8, 1),
        (16, 8, 15),
        (1000, 1, 15),
        (15, 8, 14),
        (17, 64, 15),
        (i64::from(i32::MAX), 1, 15),
        // -par=-n leaves n cores free.
        (-1, 4, 2),
        (-3, 4, 0),
        (-100, 4, 0),
        (-1, 32, 15),
        (-16, 32, 15),
        (-17, 32, 14),
        (-20, 32, 11),
        (-30, 32, 1),
        (-31, 32, 0),
        (-32, 32, 0),
        // No cores known (hardware_concurrency() returns 0).
        (0, 0, 0),
        (2, 0, 1),
        // Core reads the option into an int: 2^32 + 2 is -par=2, 2^32 is -par=0, 2^32 - 1 and
        // i64::MAX (where GetIntArg saturates) are -par=-1.
        ((1 << 32) + 2, 8, 1),
        (1 << 32, 8, 7),
        ((1 << 32) - 1, 8, 6),
        (i64::MAX, 8, 6),
        (i64::MIN, 4, 3),
        (i64::from(i32::MIN), 4, 0),
        (i64::from(i32::MIN) - 1, 4, 15),
    ];
    for &(par, cores, want) in rows {
        assert_eq!(script_check_threads(par, cores), want, "-par={par} on {cores} cores");
    }
    assert_eq!(DEFAULT_SCRIPTCHECK_THREADS, 0);
}

struct Probe<'a> {
    ok: bool,
    ran: &'a AtomicUsize,
    done: &'a AtomicBool,
}

impl Check for Probe<'_> {
    fn check(self) -> bool {
        assert!(!self.done.load(Ordering::SeqCst), "a check ran after control returned");
        self.ran.fetch_add(1, Ordering::Relaxed);
        self.ok
    }
}

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut state = seed | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

/// The model test against serial (PLAN CS6): for random check lists, failure positions, add
/// patterns and thread counts, the verdict is the AND of the checks, every check runs exactly
/// once when all pass, and none runs twice or after the control returns.
#[test]
fn queue_verdict_is_the_and_of_its_checks() {
    let mut next = xorshift(0x9e37_79b9_7f4a_7c15);
    for threads in [0, 1, 2, 3, 8] {
        let queue = CheckQueue::new(threads).unwrap();
        assert_eq!(queue.has_threads(), threads > 0);
        assert_eq!(queue.threads(), threads);
        for _ in 0..60 {
            let n = match next() % 4 {
                0 => (next() % 4) as usize,
                1 => 127 + (next() % 3) as usize,
                _ => (next() % 700) as usize,
            };
            let failing: Vec<bool> = (0..n).map(|_| next().is_multiple_of(50)).collect();
            let serial = failing.iter().all(|f| !f);
            let (ran, done) = (AtomicUsize::new(0), AtomicBool::new(false));
            let (added, ok) = queue.control(|control| {
                assert_eq!(control.is_parallel(), threads > 0);
                let mut checks = failing.iter().map(|&f| Probe {
                    ok: !f,
                    ran: &ran,
                    done: &done,
                });
                let mut added = 0;
                while added < n {
                    let take = (1 + next() % 40) as usize;
                    let batch: Vec<_> = checks.by_ref().take(take).collect();
                    added += batch.len();
                    control.add(batch);
                }
                control.add(Vec::new());
                added
            });
            done.store(true, Ordering::SeqCst);
            assert_eq!(added, n);
            assert_eq!(ok, serial, "{threads} threads, {n} checks");
            let ran = ran.load(Ordering::Relaxed);
            assert!(ran <= n);
            if serial {
                assert_eq!(ran, n, "every check of a passing block runs");
            }
        }
    }
}

/// Core's control destructor waits for the queue when `ConnectBlock` returns early; so does
/// `control`, whose caller then ignores the verdict.
#[test]
fn an_early_return_still_waits_for_queued_checks() {
    let queue = CheckQueue::new(4).unwrap();
    let (ran, done) = (AtomicUsize::new(0), AtomicBool::new(false));
    let (result, ok) = queue.control(|control| -> Result<(), &str> {
        control.add(
            (0..500)
                .map(|_| Probe {
                    ok: true,
                    ran: &ran,
                    done: &done,
                })
                .collect(),
        );
        Err("bad-txns-inputs-missingorspent")
    });
    done.store(true, Ordering::SeqCst);
    assert_eq!(result, Err("bad-txns-inputs-missingorspent"));
    assert!(ok);
    assert_eq!(ran.load(Ordering::Relaxed), 500);
}

/// Without threads every check runs as it is added, and the verdict is the same AND.
#[test]
fn a_serial_queue_runs_checks_inline() {
    let queue = CheckQueue::serial();
    let (ran, done) = (AtomicUsize::new(0), AtomicBool::new(false));
    let ((), ok) = queue.control(|control| {
        let probe = |ok| Probe {
            ok,
            ran: &ran,
            done: &done,
        };
        control.add(vec![probe(true), probe(true)]);
        assert_eq!(ran.load(Ordering::Relaxed), 2);
        control.add(vec![probe(false), probe(true)]);
        // Skipped once one has failed, as Core's workers skip.
        assert_eq!(ran.load(Ordering::Relaxed), 3);
    });
    assert!(!ok);
}

/// Records the order checks run in. A batch runs in order from the top of the stack, so a run of
/// consecutive indices is one batch.
struct Rec<'a>(usize, &'a std::sync::Mutex<Vec<usize>>);

impl Check for Rec<'_> {
    fn check(self) -> bool {
        self.1.lock().unwrap().push(self.0);
        true
    }
}

fn batch_sizes(order: &[usize]) -> Vec<usize> {
    let mut sizes = vec![1];
    for w in order.windows(2) {
        if w[1] == w[0] + 1 {
            *sizes.last_mut().unwrap() += 1;
        } else {
            sizes.push(1);
        }
    }
    sizes
}

/// Core's `max(1, min(128, queue / divisor))` over a stack of `n` drained by one thread.
fn core_batches(mut n: usize, divisor: usize) -> Vec<usize> {
    let mut sizes = Vec::new();
    while n > 0 {
        let now = (n / divisor).clamp(1, BATCH_SIZE);
        sizes.push(now);
        n -= now;
    }
    sizes
}

/// Shared's batch sizes are Core's `max(1, min(128, queue / (nTotal + nIdle + 1)))`: nTotal is
/// every worker, plus the master once it waits (`checkqueue.h:88`, `:113`).
#[test]
fn batches_are_cores() {
    let order = std::sync::Mutex::new(Vec::new());
    // A helper while the master is still adding: 3 workers, none idle, the master not counted.
    let shared = Shared::new(3);
    assert_eq!(shared.add((0..400).map(|i| Rec(i, &order)).collect()), 3);
    shared.drain(true);
    assert_eq!(batch_sizes(&order.lock().unwrap()), core_batches(400, 4)); // 3 + 0 idle + 1
    assert_eq!(shared.lock().active, 2);

    // The master at `Wait`, alone: 3 workers and the master, none idle (with the 128 cap).
    order.lock().unwrap().clear();
    let shared = Shared::new(3);
    assert_eq!(shared.add((0..1000).map(|i| Rec(i, &order)).collect()), 3);
    // Three helpers are counted as started; none takes another.
    assert_eq!(shared.add(Vec::new()), 0);
    shared.drain(false);
    let sizes = batch_sizes(&order.lock().unwrap());
    assert_eq!(sizes, core_batches(1000, 5)); // 4 + 0 idle + 1
    assert_eq!(sizes[..3], [BATCH_SIZE; 3]);
    assert!(shared.all_ok());
    // A helper that finds the stack empty leaves the active count, and the next add starts it.
    for _ in 0..3 {
        shared.drain(true);
    }
    assert_eq!(shared.lock().active, 0);
    assert_eq!(shared.add(vec![Rec(0, &order), Rec(1, &order)]), 2);
}

/// A panicking check surfaces from `control` once the jobs have returned (the rest of its batch
/// is dropped unrun), no verdict is reported, and the queue still works.
#[test]
fn a_panicking_check_surfaces_and_the_queue_survives() {
    struct Boom(bool);
    impl Check for Boom {
        fn check(self) -> bool {
            assert!(!self.0, "planted panic");
            true
        }
    }
    let queue = CheckQueue::new(2).unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        queue.control(|control| control.add((0..500).map(|i| Boom(i == 250)).collect()))
    }));
    assert!(r.is_err());
    let ((), ok) = queue.control(|control| control.add((0..500).map(|_| Boom(false)).collect()));
    assert!(ok);
}

/// D41: `control_inline` is the inline mode on any queue. On one with threads, the checks run on
/// the caller as they are added, skipped once one has failed, and the verdict is their AND; it
/// takes no control lock, so it runs even inside another control on the same queue.
#[test]
fn control_inline_runs_on_the_caller_whatever_the_threads() {
    let queue = CheckQueue::new(3).unwrap();
    let (ran, done) = (AtomicUsize::new(0), AtomicBool::new(false));
    let probe = |ok| Probe {
        ok,
        ran: &ran,
        done: &done,
    };
    let ((), ok) = queue.control_inline(|control| {
        assert!(!control.is_parallel());
        control.add(vec![probe(true), probe(true)]);
        assert_eq!(ran.load(Ordering::Relaxed), 2, "run as they are added");
        control.add(vec![probe(false), probe(true)]);
        assert_eq!(ran.load(Ordering::Relaxed), 3);
    });
    assert!(!ok);
    let ((), ok) = queue.control_inline(|control| control.add(vec![probe(true)]));
    assert!(ok);
    // Inside a parallel control on the same queue: no lock, so no deadlock.
    let (inner, outer) = queue.control::<Probe<'_>, _, _>(|control| {
        assert!(control.is_parallel());
        queue.control_inline(|inline| inline.add(vec![probe(true)])).1
    });
    assert!(inner && outer);
}
