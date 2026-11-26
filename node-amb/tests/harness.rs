//! The guard, masking, calibration and child-per-pass tests (rev4–rev8, conditions 1, 3, 4, 5,
//! 6): each runs a child of this binary through the supervisor and asserts its verdict. The
//! child (`harness_child`) starts two worker threads, opens a window, then does what its
//! `AMB_PASS` names inside it.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use node_amb::account::Completion;
use node_amb::sys::guard::nr;
use node_amb::sys::supervisor::{self, Child, ChildRun};
use node_amb::sys::window::{self, Setup, Window};
use node_amb::sys::{self, counters, gettid, mask};

const CHILD: &str = "harness_child";
/// The `counted` child's busy counted calls, and the `counted-empty` child's empty ones.
const BUSY_CALLS: usize = 8;
const EMPTY_CALLS: usize = 16;
/// The `idle-woken` child's exit when its held worker ran before the close (a discarded trial).
const UNQUALIFIED: &str = "trial: the held worker ran before the close";

static HANDLED: AtomicBool = AtomicBool::new(false);

extern "C" fn flag(_: libc::c_int) {
    HANDLED.store(true, Ordering::SeqCst);
}

fn done() -> Completion {
    Completion {
        tip_is_last: true,
        signals_barrier: true,
        actor_barrier: None,
        hb_last_cmpct: None,
        quiesced: None,
    }
}

/// A worker that runs the jobs it is sent (so a pre-existing thread acts inside the window).
struct Worker {
    tx: mpsc::Sender<Box<dyn FnOnce() + Send>>,
    tid: i32,
    handle: std::thread::JoinHandle<()>,
}

fn worker() -> Worker {
    let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
    let (tid_tx, tid_rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        tid_tx.send(gettid()).unwrap();
        while let Ok(job) = rx.recv() {
            job();
        }
    });
    Worker {
        tx,
        tid: tid_rx.recv().unwrap(),
        handle,
    }
}

fn raw(n: u32, args: [libc::c_long; 6]) -> libc::c_long {
    // SAFETY: the adversaries' syscalls; the guard kills each before it runs.
    unsafe {
        libc::syscall(n as libc::c_long, args[0], args[1], args[2], args[3], args[4], args[5])
    }
}

/// The adversary named by `what`, inside an open window.
fn adversary(what: &str, w: &mut Window, workers: &[Worker]) {
    let all: u64 = !0;
    let empty: u64 = 0;
    let mask_ptr = &empty as *const u64 as libc::c_long;
    let mut pfd = libc::pollfd {
        fd: 0,
        events: 0,
        revents: 0,
    };
    let pfd_ptr = &mut pfd as *mut libc::pollfd as libc::c_long;
    let zero_ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let ts = &zero_ts as *const libc::timespec as libc::c_long;
    let epfd = || {
        // SAFETY: a new epoll descriptor.
        unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) as libc::c_long }
    };
    let mut ev = [0u8; 64];
    let evp = ev.as_mut_ptr() as libc::c_long;
    let pid = std::process::id() as libc::c_long;
    let me = gettid() as libc::c_long;
    match what {
        // Survivors: what the workloads do, allowed by the guard.
        "allowed" => {
            raw(nr::PPOLL, [pfd_ptr, 0, ts, 0, 8, 0]);
            let e = epfd();
            raw(nr::EPOLL_PWAIT, [e, evp, 1, 0, 0, 8]);
            raw(nr::EPOLL_PWAIT2, [e, evp, 1, ts, 0, 8]);
            // glibc's select: pselect6 with a NULL sixth argument.
            raw(nr::PSELECT6, [0, 0, 0, 0, ts, 0]);
            raw(nr::RT_SIGPROCMASK, [libc::SIG_BLOCK as _, &all as *const u64 as _, 0, 8, 0, 0]);
            std::thread::sleep(Duration::from_millis(1));
            let _ = std::fs::read_to_string("/proc/self/status");
            drop(Vec::<u8>::with_capacity(1 << 20));
        }
        "thread" => {
            let _ = std::thread::spawn(|| {});
        }
        "fork" => {
            // SAFETY: the guard kills the process at the call.
            unsafe { libc::fork() };
        }
        "raw-fork" => {
            raw(nr::FORK, [0; 6]);
        }
        "vfork" => {
            raw(nr::VFORK, [0; 6]);
        }
        "clone3" => {
            raw(nr::CLONE3, [0; 6]);
        }
        "execve" => {
            raw(nr::EXECVE, [0; 6]);
        }
        "execveat" => {
            raw(nr::EXECVEAT, [0; 6]);
        }
        "setmask" => {
            raw(nr::RT_SIGPROCMASK, [libc::SIG_SETMASK as _, mask_ptr, 0, 8, 0, 0]);
        }
        "unblock" => {
            raw(nr::RT_SIGPROCMASK, [libc::SIG_UNBLOCK as _, &all as *const u64 as _, 0, 8, 0, 0]);
        }
        "rt_sigreturn" => {
            // r6's saved-context path: the guard kills rt_sigreturn at entry, whatever the frame.
            raw(nr::RT_SIGRETURN, [0; 6]);
        }
        "rt_sigsuspend" => {
            // A pending SIGUSR2 (no handler: its default ends the process), so a guard that let
            // the call through dies by SIGUSR2 at once instead of waiting forever.
            raw(nr::TGKILL, [pid, me, libc::SIGUSR2 as _, 0, 0, 0]);
            raw(nr::RT_SIGSUSPEND, [mask_ptr, 8, 0, 0, 0, 0]);
        }
        "rt_sigtimedwait" => {
            raw(nr::RT_SIGTIMEDWAIT, [&all as *const u64 as _, 0, ts, 8, 0, 0]);
        }
        "ppoll-mask" => {
            raw(nr::PPOLL, [pfd_ptr, 0, ts, mask_ptr, 8, 0]);
        }
        "ppoll-mask-high" => {
            // A pointer whose low half is zero: both halves are checked.
            raw(nr::PPOLL, [pfd_ptr, 0, ts, 1 << 32, 8, 0]);
        }
        "epoll_pwait-mask" => {
            raw(nr::EPOLL_PWAIT, [epfd(), evp, 1, 0, mask_ptr, 8]);
        }
        "epoll_pwait2-mask" => {
            raw(nr::EPOLL_PWAIT2, [epfd(), evp, 1, ts, mask_ptr, 8]);
        }
        "pselect6-desc" => {
            // A non-NULL descriptor is killed even when its nested mask is NULL (rev8).
            let desc: [libc::c_long; 2] = [0, 8];
            raw(nr::PSELECT6, [0, 0, 0, 0, ts, &desc as *const _ as libc::c_long]);
        }
        "io_pgetevents" => {
            // r7's adversary: a pending SIGUSR1 and a temporary mask that unblocks it. The guard
            // kills the call at entry, so neither the handler nor its siglongjmp exit runs.
            raw(nr::TGKILL, [pid, me, libc::SIGUSR1 as _, 0, 0, 0]);
            let sig = [mask_ptr, 8];
            raw(nr::IO_PGETEVENTS, [0, 0, 1, evp, ts, &sig as *const _ as libc::c_long]);
        }
        "io_uring_setup" => {
            raw(nr::IO_URING_SETUP, [1, evp, 0, 0, 0, 0]);
        }
        "io_uring_enter" => {
            raw(nr::IO_URING_ENTER, [-1, 0, 0, 0, mask_ptr, 8]);
        }
        "io_uring_register" => {
            raw(nr::IO_URING_REGISTER, [-1, 0, 0, 0, 0, 0]);
        }
        "signalfd" => {
            raw(nr::SIGNALFD, [-1, mask_ptr, 8, 0, 0, 0]);
        }
        "signalfd4" => {
            raw(nr::SIGNALFD4, [-1, mask_ptr, 8, 0, 0, 0]);
        }
        "rt_sigqueueinfo" => {
            raw(nr::RT_SIGQUEUEINFO, [pid, libc::SIGUSR1 as _, evp, 0, 0, 0]);
        }
        "rt_tgsigqueueinfo" => {
            // r8's combined adversary: a self-queued synchronous signal with a crafted si_code.
            let mut info = [0i32; 32];
            info[0] = libc::SIGSEGV;
            info[2] = 1; // si_code > 0: a kernel-looking SEGV_MAPERR
            raw(nr::RT_TGSIGQUEUEINFO, [pid, me, libc::SIGSEGV as _, info.as_ptr() as _, 0, 0]);
        }
        "pidfd_send_signal" => {
            raw(nr::PIDFD_SEND_SIGNAL, [0, libc::SIGUSR1 as _, 0, 0, 0, 0]);
        }
        "tkill-33" => {
            raw(nr::TKILL, [workers[0].tid as _, 33, 0, 0, 0, 0]);
        }
        "tgkill-32" => {
            raw(nr::TGKILL, [pid, workers[0].tid as _, 32, 0, 0, 0]);
        }
        "x32" => {
            // getpid's number with __X32_SYSCALL_BIT.
            raw(libc::SYS_getpid as u32 | 0x4000_0000, [0; 6]);
        }
        "i386" => {
            // SAFETY: int 0x80 getpid; the guard kills the non-native arch at entry.
            unsafe {
                std::arch::asm!(
                    "int 0x80",
                    inout("eax") 20 => _,
                    out("r8") _,
                    out("r9") _,
                    out("r10") _,
                    out("r11") _,
                )
            };
        }
        "setresuid" => {
            // glibc's setxid broadcast sends SIGSETXID (33) to every other thread.
            let (tx, rx) = mpsc::channel();
            workers[0]
                .tx
                .send(Box::new(move || {
                    // SAFETY: the current IDs.
                    let r =
                        unsafe { libc::setresuid(libc::getuid(), libc::geteuid(), libc::getuid()) };
                    tx.send(r).unwrap();
                }))
                .unwrap();
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
        "pthread_cancel" => {
            use std::os::unix::thread::JoinHandleExt as _;
            // SAFETY: a live thread's handle. Its cancel type is asynchronous (set before the
            // window), so glibc sends SIGCANCEL (32). With the default deferred type, glibc 2.39
            // signals only a target inside a cancellation point, and the worker waits in a raw
            // futex (mpsc `recv`), which is none: the pass had no reserved send to deny.
            unsafe { libc::pthread_cancel(workers[1].handle.as_pthread_t()) };
        }
        "external-signals" => {
            // The supervisor sends 32, 33 and SIGUSR1 to this TID, then writes the done file.
            let dir = std::env::var("AMB_SIGNAL_DIR").expect("the signal dir");
            std::fs::write(format!("{dir}/tid"), me.to_string()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(60);
            while !std::path::Path::new(&format!("{dir}/sent")).exists() {
                assert!(Instant::now() < deadline, "no signals");
                std::thread::sleep(Duration::from_millis(5));
            }
            let blk = mask::sigblk(gettid()).unwrap();
            assert_eq!(blk, mask::FULL_SIGBLK, "the caller's mask held");
        }
        "counted" => {
            for i in 0..BUSY_CALLS as u64 {
                w.counted(&mut || {
                    let mut x = 0u64;
                    for j in 0..1000 * (i + 1) {
                        x = std::hint::black_box(x.wrapping_add(j));
                    }
                })
                .unwrap();
            }
        }
        "counted-empty" => {
            // Empty counted calls after the calibration, through the counted path itself: pairs
            // the calibration's maximum was not taken from.
            for _ in 0..EMPTY_CALLS {
                w.counted(&mut || {}).unwrap();
            }
        }
        "idle-woken" => {
            // A job for the held worker just before the close: it stays runnable behind the
            // hog (R2-F1), so without the close's wait it would start only after the end reads.
            let held = workers[0].tid;
            let ran = sys::run_ns(held).unwrap();
            workers[0]
                .tx
                .send(Box::new(|| {
                    let mut x = 0u64;
                    for j in 0..10_000u64 {
                        x = std::hint::black_box(x.wrapping_add(j));
                    }
                }))
                .unwrap();
            // SOL-R3-N1: a trial counts only if the worker is still runnable and has not run
            // since the job, so the close's wait is what the refusal depends on.
            if sys::state(held).unwrap() != 'R' || sys::run_ns(held).unwrap() != ran {
                supervisor::die(UNQUALIFIED);
            }
        }
        other => panic!("no adversary {other}"),
    }
}

/// Before the window: worker 1 spins (counted) until the child exits, and worker 0, the held thread, runs at
/// `SCHED_IDLE` on the same single CPU, so once woken it waits behind the spin.
fn hold_behind_a_hog(workers: &[Worker]) {
    // SAFETY: cpu_set_t is plain data; the calls read this process's mask and set the workers'.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        let size = std::mem::size_of::<libc::cpu_set_t>();
        assert_eq!(libc::sched_getaffinity(0, size, &mut set), 0);
        let cpu = (0..libc::CPU_SETSIZE as usize).find(|&c| libc::CPU_ISSET(c, &set)).unwrap();
        let mut one: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut one);
        for w in workers {
            assert_eq!(libc::sched_setaffinity(w.tid, size, &one), 0, "pin {}", w.tid);
        }
        let param = libc::sched_param {
            sched_priority: 0,
        };
        assert_eq!(libc::sched_setscheduler(workers[0].tid, libc::SCHED_IDLE, &param), 0);
    }
    workers[1]
        .tx
        .send(Box::new(|| {
            loop {
                std::hint::spin_loop();
            }
        }))
        .unwrap();
}

unsafe extern "C" {
    fn pthread_setcanceltype(kind: libc::c_int, old: *mut libc::c_int) -> libc::c_int;
}

/// glibc's `PTHREAD_CANCEL_ASYNCHRONOUS`.
const PTHREAD_CANCEL_ASYNCHRONOUS: libc::c_int = 1;

/// Before the window: `w` takes an asynchronous cancel type, so a `pthread_cancel` of it is a
/// SIGCANCEL send on every glibc.
fn cancel_asynchronously(w: &Worker) {
    let (tx, rx) = mpsc::channel();
    w.tx.send(Box::new(move || {
        let mut old = 0;
        // SAFETY: the calling thread's own cancel type.
        tx.send(unsafe { pthread_setcanceltype(PTHREAD_CANCEL_ASYNCHRONOUS, &mut old) }).unwrap();
    }))
    .unwrap();
    assert_eq!(rx.recv().unwrap(), 0, "pthread_setcanceltype");
}

/// The child's entry: no-op unless it is a supervised pass.
#[test]
fn harness_child() {
    let Some(pass) = supervisor::pass() else {
        return;
    };
    let workers = [worker(), worker()];
    // SAFETY: a plain handler that sets a flag.
    unsafe { libc::signal(libc::SIGUSR1, flag as extern "C" fn(libc::c_int) as usize) };
    if pass == "pthread_cancel" {
        cancel_asynchronously(&workers[1]);
    }
    let segment = pass.starts_with("counted");
    let idle_woken = pass == "idle-woken";
    if idle_woken {
        hold_behind_a_hog(&workers);
    }
    let setup = Setup {
        count: segment || idle_woken || pass == "contention",
        segment_caller: segment,
        skip_mask: (pass == "mask-gap").then_some(workers[1].tid),
        idle: idle_woken.then_some(workers[0].tid).into_iter().collect(),
        ..Setup::default()
    };
    if pass == "contention" {
        // Fill the PMU with pinned events on the caller before the gate counters attach.
        let extra: Vec<_> = (0..32)
            .filter_map(|_| counters::Counter::open_with(gettid(), counters::FLAGS).ok())
            .collect();
        std::mem::forget(extra);
    }
    let mut w = match window::open(setup) {
        Ok(w) => w,
        Err(e) => supervisor::die(&e.0),
    };
    if pass != "contention" {
        adversary(&pass, &mut w, &workers);
    }
    let segments = w.segments().map(|(c, calls)| {
        let max = c.max().unwrap();
        let deltas: Vec<u64> = calls.iter().map(|p| p.after.value - p.before.value).collect();
        let same = calls.iter().all(|p| p.wrapper == c.wrapper);
        format!("{max} {} {same} {:?} {:?}", c.wrapper, c.pairs, deltas)
    });
    match w.close(&done(), false) {
        Ok(count) => {
            let out = format!(
                "handled={} count={:?} segments={:?}",
                HANDLED.load(Ordering::SeqCst),
                count.map(|c| c.total),
                segments
            );
            supervisor::finish(out.as_bytes())
        }
        // The empty calls' accounting may refuse one cheaper than the maximum (a counted call
        // that retired less than an empty pair): the record still carries their counts.
        Err(e) if pass == "counted-empty" => {
            supervisor::finish(format!("refused={:?} segments={segments:?}", e.0).as_bytes())
        }
        Err(e) => supervisor::die(&format!("{} (handled={})", e.0, HANDLED.load(Ordering::SeqCst))),
    }
}

/// How long a child may run before it is killed and refused (a regression that blocks fails
/// instead of hanging the test).
const CHILD_LIMIT: Duration = Duration::from_secs(300);

fn child(pass: &str) -> ChildRun {
    Child::spawn(CHILD, pass, &[]).expect("spawn").wait_within(CHILD_LIMIT)
}

fn killed(pass: &str) {
    let run = child(pass);
    let log = run.log.clone();
    let e = run.record().expect_err(pass);
    assert!(e.0.contains("SIGSYS"), "{pass}: {e}\n{log}");
}

fn refused(pass: &str, why: &str) {
    let e = child(pass).record().expect_err(pass);
    assert!(e.0.contains(why), "{pass}: {e}");
}

/// The workloads' own calls survive the guard and the window gives its record.
#[test]
fn the_guard_allows_the_workloads_calls() {
    let r = String::from_utf8(child("allowed").record().unwrap()).unwrap();
    assert!(r.starts_with("handled=false"), "{r}");
}

/// rev4: thread and process creation and exec inside the window kill the child.
#[test]
fn creation_and_exec_in_the_window_kill_the_child() {
    for pass in ["thread", "fork", "raw-fork", "vfork", "clone3", "execve", "execveat"] {
        killed(pass);
    }
}

/// rev5/rev6/rev7/rev8: every unmask, `rt_sigreturn`, temporary-mask syscall in the audit
/// list, signalfd and self-queue kills the child (one per entry).
#[test]
fn every_audited_mask_route_kills_the_child() {
    for pass in [
        "setmask",
        "unblock",
        "rt_sigreturn",
        "rt_sigsuspend",
        "rt_sigtimedwait",
        "ppoll-mask",
        "ppoll-mask-high",
        "epoll_pwait-mask",
        "epoll_pwait2-mask",
        "pselect6-desc",
        "io_pgetevents",
        "io_uring_setup",
        "io_uring_enter",
        "io_uring_register",
        "signalfd",
        "signalfd4",
        "rt_sigqueueinfo",
        "rt_tgsigqueueinfo",
        "pidfd_send_signal",
    ] {
        killed(pass);
    }
}

/// rev5's reserved-signal adversaries: a worker's `setresuid` (SIGSETXID) and a
/// `pthread_cancel` (SIGCANCEL), and raw sends of 32 and 33.
#[test]
fn reserved_signal_sends_kill_the_child() {
    for pass in ["setresuid", "pthread_cancel", "tkill-33", "tgkill-32"] {
        killed(pass);
    }
}

/// rev6: other syscall ABIs (x32 numbers, i386's `int 0x80`) kill the child.
#[test]
fn other_abis_kill_the_child() {
    killed("x32");
    killed("i386");
}

/// rev5: an external process sends 32, 33 and SIGUSR1 to the caller inside the window: each
/// stays blocked, the flag-setting handler never runs, and the end check refuses them pending.
#[test]
fn external_signals_stay_pending_and_refuse() {
    let dir = tempfile_dir();
    let c = Child::spawn(CHILD, "external-signals", &[("AMB_SIGNAL_DIR", dir.clone())]).unwrap();
    let pid = c.pid() as libc::c_long;
    let tid_file = format!("{dir}/tid");
    let deadline = Instant::now() + Duration::from_secs(120);
    let tid: libc::c_long = loop {
        if let Some(t) = std::fs::read_to_string(&tid_file).ok().and_then(|t| t.parse().ok()) {
            break t;
        }
        assert!(Instant::now() < deadline, "the child never opened its window");
        std::thread::sleep(Duration::from_millis(5));
    };
    for sig in [32, 33, libc::SIGUSR1] {
        // SAFETY: a signal to the child's thread, from this unfiltered process.
        let r = unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, sig) };
        assert_eq!(r, 0, "tgkill {sig}");
    }
    std::fs::write(format!("{dir}/sent"), "").unwrap();
    let run = c.wait_within(CHILD_LIMIT);
    let log = run.log.clone();
    let e = run.record().expect_err("refused");
    assert!(e.0.contains("exited 2") && log.contains("pending"), "{e}");
    assert!(log.contains("handled=false"), "{log}");
    let _ = std::fs::remove_dir_all(&dir);
}

fn tempfile_dir() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
    let dir =
        std::env::temp_dir().join(format!("amb-sig-{}-{}", std::process::id(), nanos.as_nanos()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.display().to_string()
}

/// R2-F1: a held thread handed a job inside the window that has not run by the close is waited
/// for, so its work is counted and the idle hold refuses the pass.
#[test]
fn a_held_thread_woken_before_the_close_refuses() {
    if !counters::available() {
        supervisor::no_pmu("a_held_thread_woken_before_the_close_refuses");
        return;
    }
    for _ in 0..20 {
        let run = child("idle-woken");
        let log = run.log.clone();
        let e = run.record().expect_err("refused");
        if log.contains(UNQUALIFIED) {
            continue;
        }
        assert!(log.contains("idle thread") && log.contains("inside the window"), "{e}\n{log}");
        return;
    }
    panic!("no trial had the held worker unrun at the close");
}

/// rev5: a worker that never took its mask refuses at step 3, before the window.
#[test]
fn a_mask_gap_refuses_before_the_window() {
    refused("mask-gap", "SigBlk");
}

/// rev5 calibration: the calibrated maximum ([`Calibration::max`], as the child's accounting
/// subtracts it) is at least every empty pair; empty counted calls taken through the counted
/// path after the calibration, pairs the maximum was not taken from, stay within it; and each
/// counted call names the wrapper that took it, which is the calibration's.
///
/// [`Calibration::max`]: node_amb::account::Calibration::max
#[test]
fn the_calibration_bounds_every_empty_pair_through_one_wrapper() {
    if !counters::available() {
        supervisor::no_pmu("the_calibration_bounds_every_empty_pair_through_one_wrapper");
        return;
    }
    // (max, wrapper, every call took it, the calibration's pairs, the calls' counts)
    let segments = |pass: &str| {
        let r = String::from_utf8(child(pass).record().unwrap()).unwrap();
        let seg = r.split("segments=Some(\"").nth(1).unwrap_or_else(|| panic!("segments: {r}"));
        let mut it = seg.splitn(4, ' ');
        let max: u64 = it.next().unwrap().parse().unwrap();
        let wrapper: usize = it.next().unwrap().parse().unwrap();
        assert_eq!(it.next(), Some("true"), "every call took the calibration's wrapper: {r}");
        let list = |s: &str| -> Vec<u64> {
            s[1..s.find(']').unwrap()].split(", ").map(|p| p.parse().unwrap()).collect()
        };
        let rest = it.next().unwrap();
        let (pairs, calls) = (list(rest), list(&rest[rest.find("] [").unwrap() + 2..]));
        assert_eq!(pairs.len(), window::CALIBRATION_PAIRS);
        assert!(pairs.iter().all(|&p| p <= max) && max > 0, "{r}");
        assert_ne!(wrapper, 0, "the wrapper's address (ASLR moves it per process)");
        (r, max, calls)
    };
    let (r, max, busy) = segments("counted");
    assert!(r.contains("count=Some("), "{r}");
    assert_eq!(busy.len(), BUSY_CALLS, "{r}");
    assert!(busy.iter().all(|&d| d > max), "{r}");
    let (r, max, empty) = segments("counted-empty");
    assert_eq!(empty.len(), EMPTY_CALLS, "{r}");
    assert!(empty.iter().all(|&d| d <= max), "an empty counted call exceeds the max {max}: {r}");
}

/// rev3 N3: extra pinned events on the caller crowd its gate counter off the PMU: the window
/// refuses (the counter's short read or time check), with no number.
#[test]
fn forced_contention_against_a_gate_counter_refuses() {
    if !counters::available() {
        supervisor::no_pmu("forced_contention_against_a_gate_counter_refuses");
        return;
    }
    let e = child("contention").record().expect_err("refused");
    assert!(e.0.contains("exited 2"), "{e}");
    assert!(e.0.contains("read of thread") || e.0.contains("lost coverage"), "{e}");
}
