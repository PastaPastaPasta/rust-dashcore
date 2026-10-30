//! The host's threads and bounded services (SB0 §7): Core's `TraceThread`, a bounded pool for
//! blocking work, and `getaddrinfo` on that pool. Shared crates never spawn or resolve
//! themselves (SB0 L5); the node passes these in.

use std::collections::VecDeque;
use std::future::Future;
use std::net::IpAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;

use crate::logging::set_thread_name;

/// Core's `util::TraceThread` on a new thread: the OS name `d-<name>` (`ThreadRename`), the
/// internal name `<name>`, and `<name> thread start`/`exit` in the log. A panic is Core's
/// uncaught exception: `PrintExceptionContinue`, then the process aborts.
// Core: util/thread.cpp:13-25, util/threadnames.cpp:59-63
pub fn trace_thread<T: Send + 'static>(
    name: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<JoinHandle<T>> {
    let name = name.to_owned();
    std::thread::Builder::new().name(format!("d-{name}")).spawn(move || {
        set_thread_name(&name);
        tracing::info!("{name} thread start");
        match catch_unwind(AssertUnwindSafe(f)) {
            Ok(out) => {
                tracing::info!("{name} thread exit");
                out
            }
            Err(panic) => {
                print_exception_continue(&name, &panic_message(&*panic));
                std::process::abort();
            }
        }
    })
}

/// Core's `PrintExceptionContinue`: the banner and the message to the log and to stderr.
// Core: util/system.cpp:783-788
pub fn print_exception_continue(thread_name: &str, what: &str) {
    let message = format!("\"{thread_name}\" raised an exception\n{what}");
    tracing::info!("\n\n************************\n{message}");
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "\n\n************************\n{message}");
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    match (panic.downcast_ref::<&str>(), panic.downcast_ref::<String>()) {
        (Some(s), _) => format!("panic: {s}"),
        (_, Some(s)) => format!("panic: {s}"),
        _ => "panic".to_owned(),
    }
}

type Job = Box<dyn FnOnce() + Send>;

struct PoolState {
    jobs: VecDeque<Job>,
    threads: usize,
    idle: usize,
    /// Idle threads already woken for a queued job; the rest can take a new one.
    woken: usize,
    stopping: bool,
    handles: Vec<JoinHandle<()>>,
}

struct PoolShared {
    name: String,
    max_threads: usize,
    state: Mutex<PoolState>,
    work: Condvar,
}

impl PoolShared {
    fn lock(&self) -> MutexGuard<'_, PoolState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A bounded pool for blocking work (storage calls, DNS): at most `max_threads` threads, started
/// as work arrives and kept until [`shutdown`](Self::shutdown). Work beyond them queues.
#[derive(Clone)]
pub struct BlockingPool {
    shared: Arc<PoolShared>,
}

/// Why a [`BlockingTask`] has no result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskError {
    /// The pool shut down before the job ran.
    Cancelled,
    /// The job panicked.
    Panicked,
}

impl BlockingPool {
    /// A pool of at most `max_threads` (≥ 1) threads named `d-<name>.<n>`.
    pub fn new(name: &str, max_threads: usize) -> BlockingPool {
        assert!(max_threads >= 1);
        BlockingPool {
            shared: Arc::new(PoolShared {
                name: name.to_owned(),
                max_threads,
                state: Mutex::new(PoolState {
                    jobs: VecDeque::new(),
                    threads: 0,
                    idle: 0,
                    woken: 0,
                    stopping: false,
                    handles: Vec::new(),
                }),
                work: Condvar::new(),
            }),
        }
    }

    /// Runs `f` on the pool. After [`shutdown`](Self::shutdown) the task fails with
    /// [`TaskError::Cancelled`] at once.
    pub fn spawn<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> BlockingTask<T> {
        let slot = Arc::new(Slot::new());
        let done = Completer(Arc::clone(&slot));
        let job: Job = Box::new(move || {
            let out = catch_unwind(AssertUnwindSafe(f)).map_err(|_| TaskError::Panicked);
            done.0.complete(out);
        });
        let mut state = self.shared.lock();
        if state.stopping {
            drop(state);
            drop(job);
            return BlockingTask {
                slot,
            };
        }
        state.jobs.push_back(job);
        if state.idle > state.woken {
            state.woken += 1;
            self.shared.work.notify_one();
        } else if state.threads < self.shared.max_threads {
            let n = state.threads;
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name(format!("d-{}.{n}", self.shared.name))
                .spawn(move || worker(&shared, n));
            match spawned {
                Ok(handle) => {
                    state.threads += 1;
                    state.handles.push(handle);
                }
                // The job waits for a running thread; with none, the pool cannot run it.
                Err(_) if state.threads == 0 => {
                    let job = state.jobs.pop_back();
                    drop(state);
                    drop(job);
                    return BlockingTask {
                        slot,
                    };
                }
                Err(_) => {}
            }
        }
        BlockingTask {
            slot,
        }
    }

    /// Stops the pool: queued jobs are cancelled, running ones finish, and every thread is
    /// joined.
    pub fn shutdown(&self) {
        let (jobs, handles) = {
            let mut state = self.shared.lock();
            state.stopping = true;
            (std::mem::take(&mut state.jobs), std::mem::take(&mut state.handles))
        };
        self.shared.work.notify_all();
        // A job dropped unrun completes its task as `Cancelled`.
        drop(jobs);
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// The pool's threads now (for the thread-count gates).
    pub fn thread_count(&self) -> usize {
        self.shared.lock().threads
    }
}

/// The shared crates' blocking service (SB0 §7). A job the pool drops unrun at shutdown resolves
/// its `dash_kv::Pending` to `Cancelled`.
impl dash_kv::BlockingIo for BlockingPool {
    fn spawn(&self, job: dash_kv::Job) {
        drop(BlockingPool::spawn(self, job));
    }
}

fn worker(shared: &PoolShared, n: usize) {
    set_thread_name(&format!("{}.{n}", shared.name));
    let mut state = shared.lock();
    loop {
        if let Some(job) = state.jobs.pop_front() {
            drop(state);
            job();
            state = shared.lock();
            continue;
        }
        if state.stopping {
            state.threads -= 1;
            return;
        }
        state.idle += 1;
        state = shared.work.wait(state).unwrap_or_else(PoisonError::into_inner);
        state.idle -= 1;
        state.woken = state.woken.saturating_sub(1);
    }
}

/// A one-shot result slot.
struct Slot<T> {
    state: Mutex<SlotState<T>>,
    ready: Condvar,
}

struct SlotState<T> {
    out: Option<Result<T, TaskError>>,
    /// The awaiting task's waker.
    waker: Option<Waker>,
}

/// A job's hold on its slot: a job dropped unrun (at shutdown) completes it as cancelled.
struct Completer<T>(Arc<Slot<T>>);

impl<T> Drop for Completer<T> {
    fn drop(&mut self) {
        self.0.complete(Err(TaskError::Cancelled));
    }
}

impl<T> Slot<T> {
    fn new() -> Slot<T> {
        Slot {
            state: Mutex::new(SlotState {
                out: None,
                waker: None,
            }),
            ready: Condvar::new(),
        }
    }

    /// Sets the result unless it is set already, and wakes the waiter.
    fn complete(&self, out: Result<T, TaskError>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.out.is_none() {
            state.out = Some(out);
        }
        let waker = state.waker.take();
        drop(state);
        self.ready.notify_all();
        if let Some(w) = waker {
            w.wake();
        }
    }
}

/// The result of a [`BlockingPool::spawn`]: wait for it, or await it.
pub struct BlockingTask<T> {
    slot: Arc<Slot<T>>,
}

impl<T> BlockingTask<T> {
    /// Blocks until the job is done.
    pub fn wait(self) -> Result<T, TaskError> {
        let mut state = self.slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(out) = state.out.take() {
                return out;
            }
            state = self.slot.ready.wait(state).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl<T> Future for BlockingTask<T> {
    type Output = Result<T, TaskError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.slot.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(out) = state.out.take() {
            return Poll::Ready(out);
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

/// Core's `WrappedGetAddrInfo`: the IPv4 and IPv6 addresses `getaddrinfo` gives for `name`
/// (with their IPv6 scope ids), in its order. Without `allow_lookup` only a numeric `name`
/// resolves; with it, a failed `AI_ADDRCONFIG` lookup is retried without that flag. Any error
/// gives no address.
// Core: netbase.cpp:44-95
#[cfg(unix)]
pub fn get_addr_info(name: &str, allow_lookup: bool) -> Vec<(IpAddr, u32)> {
    use std::ffi::CString;
    use std::net::{Ipv4Addr, Ipv6Addr};

    let Ok(cname) = CString::new(name) else {
        return Vec::new();
    };
    // SAFETY: an all-zero `addrinfo` is a valid hint (null pointers, zero fields).
    let mut hint: libc::addrinfo = unsafe { std::mem::zeroed() };
    hint.ai_socktype = libc::SOCK_STREAM;
    hint.ai_protocol = libc::IPPROTO_TCP;
    hint.ai_family = libc::AF_UNSPEC;
    hint.ai_flags = if allow_lookup {
        libc::AI_ADDRCONFIG
    } else {
        libc::AI_NUMERICHOST
    };
    let mut res: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: `cname` is NUL-terminated, `hint` is initialized, and `res` receives the list.
    let mut err = unsafe { libc::getaddrinfo(cname.as_ptr(), std::ptr::null(), &hint, &mut res) };
    if err != 0 && hint.ai_flags & libc::AI_ADDRCONFIG != 0 {
        // AI_ADDRCONFIG can exclude loopback-only addresses.
        hint.ai_flags &= !libc::AI_ADDRCONFIG;
        // SAFETY: as above; `res` was not set by the failed call.
        err = unsafe { libc::getaddrinfo(cname.as_ptr(), std::ptr::null(), &hint, &mut res) };
    }
    if err != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = res;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getaddrinfo returned, not yet freed.
        let ai = unsafe { &*cur };
        if ai.ai_family == libc::AF_INET {
            assert!(ai.ai_addrlen as usize >= std::mem::size_of::<libc::sockaddr_in>());
            // SAFETY: an AF_INET entry's address is a `sockaddr_in` of the asserted length.
            let sin = unsafe { &*(ai.ai_addr as *const libc::sockaddr_in) };
            out.push((IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr))), 0));
        }
        if ai.ai_family == libc::AF_INET6 {
            assert!(ai.ai_addrlen as usize >= std::mem::size_of::<libc::sockaddr_in6>());
            // SAFETY: an AF_INET6 entry's address is a `sockaddr_in6` of the asserted length.
            let sin6 = unsafe { &*(ai.ai_addr as *const libc::sockaddr_in6) };
            out.push((IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr)), sin6.sin6_scope_id));
        }
        cur = ai.ai_next;
    }
    // SAFETY: `res` is the list getaddrinfo returned, freed once.
    unsafe { libc::freeaddrinfo(res) };
    out
}

/// [`get_addr_info`] off Unix: std's resolver (`getaddrinfo` too, without Core's hints).
#[cfg(not(unix))]
pub fn get_addr_info(name: &str, allow_lookup: bool) -> Vec<(IpAddr, u32)> {
    use std::net::{SocketAddr, ToSocketAddrs};
    if !allow_lookup || name.parse::<IpAddr>().is_ok() {
        return name.parse::<IpAddr>().map(|ip| vec![(ip, 0)]).unwrap_or_default();
    }
    (name, 0)
        .to_socket_addrs()
        .map(|addrs| {
            addrs
                .map(|a| match a {
                    SocketAddr::V4(a) => (IpAddr::V4(*a.ip()), 0),
                    SocketAddr::V6(a) => (IpAddr::V6(*a.ip()), a.scope_id()),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `getaddrinfo` on a [`BlockingPool`]: the node's DNS service (the `Resolver` dash-p2p takes,
/// once SB6 lands, resolves through it).
#[derive(Clone)]
pub struct HostResolver {
    pool: BlockingPool,
}

impl HostResolver {
    pub fn new(pool: BlockingPool) -> HostResolver {
        HostResolver {
            pool,
        }
    }

    /// [`get_addr_info`] on the pool.
    pub fn lookup(&self, name: &str, allow_lookup: bool) -> BlockingTask<Vec<(IpAddr, u32)>> {
        let name = name.to_owned();
        self.pool.spawn(move || get_addr_info(&name, allow_lookup))
    }
}

/// The interfaces `Discover` walks: the name and IPv4 or IPv6 address of each `getifaddrs` entry
/// whose interface is up and not loopback, in its order.
// Core: net.cpp:3860-3886
#[cfg(unix)]
pub fn interface_addrs() -> Vec<(String, IpAddr)> {
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// The list `getifaddrs` returned, freed once when dropped, even by a panic.
    struct IfAddrs(*mut libc::ifaddrs);
    impl Drop for IfAddrs {
        fn drop(&mut self) {
            // SAFETY: the list getifaddrs returned, freed only here.
            unsafe { libc::freeifaddrs(self.0) };
        }
    }

    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `list` receives the list, which `IfAddrs` frees.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Vec::new();
    }
    let list = IfAddrs(list);
    let mut out = Vec::new();
    let mut cur = list.0;
    while !cur.is_null() {
        // SAFETY: `cur` is a node of the list getifaddrs returned, not yet freed.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        let up = ifa.ifa_flags & libc::IFF_UP as libc::c_uint != 0;
        let loopback = ifa.ifa_flags & libc::IFF_LOOPBACK as libc::c_uint != 0;
        if ifa.ifa_addr.is_null() || !up || loopback {
            continue;
        }
        // SAFETY: a non-null `ifa_addr` points to a `sockaddr`, whose family says its type.
        let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
        let ip = if family == libc::AF_INET {
            // SAFETY: an AF_INET entry's address is a `sockaddr_in`; read unaligned so nothing is
            // assumed of the allocation.
            let sin = unsafe { ifa.ifa_addr.cast::<libc::sockaddr_in>().read_unaligned() };
            IpAddr::V4(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)))
        } else if family == libc::AF_INET6 {
            // SAFETY: an AF_INET6 entry's address is a `sockaddr_in6`.
            let sin6 = unsafe { ifa.ifa_addr.cast::<libc::sockaddr_in6>().read_unaligned() };
            IpAddr::V6(Ipv6Addr::from(sin6.sin6_addr.s6_addr))
        } else {
            continue;
        };
        let name = if ifa.ifa_name.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null `ifa_name` is the interface's NUL-terminated name.
            unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }.to_string_lossy().into_owned()
        };
        out.push((name, ip));
    }
    out
}

/// [`interface_addrs`] off Unix: none. Core's Windows `Discover` adds its host name's
/// addresses instead, which rdashd does not until BL-338 (BL-327 D5).
#[cfg(not(unix))]
pub fn interface_addrs() -> Vec<(String, IpAddr)> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn interface_addrs_skip_loopback() {
        let found = interface_addrs();
        assert!(found.iter().all(|(_, ip)| !ip.is_loopback()), "{found:?}");
    }

    #[test]
    fn trace_thread_names_the_thread() {
        let handle = trace_thread("loadblk", || {
            (std::thread::current().name().map(str::to_owned), crate::logging::thread_name())
        })
        .unwrap();
        let (os, internal) = handle.join().unwrap();
        assert_eq!(os.as_deref(), Some("d-loadblk"));
        assert_eq!(internal, "loadblk");
    }

    #[test]
    fn the_pool_never_exceeds_its_bound() {
        let pool = BlockingPool::new("io", 2);
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let tasks: Vec<_> = (0..8)
            .map(|i| {
                let (running, peak) = (Arc::clone(&running), Arc::clone(&peak));
                pool.spawn(move || {
                    let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(5));
                    running.fetch_sub(1, Ordering::SeqCst);
                    i
                })
            })
            .collect();
        let out: Vec<_> = tasks.into_iter().map(|t| t.wait().unwrap()).collect();
        assert_eq!(out, (0..8).collect::<Vec<_>>());
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert!(pool.thread_count() <= 2);
        pool.shutdown();
        assert_eq!(pool.thread_count(), 0);
        assert_eq!(pool.spawn(|| 1).wait(), Err(TaskError::Cancelled));
    }

    /// Two jobs queued while one thread is idle get a thread each: they meet.
    #[test]
    fn an_idle_thread_is_woken_for_one_job_only() {
        let pool = BlockingPool::new("io", 2);
        pool.spawn(|| ()).wait().unwrap();
        while pool.shared.lock().idle == 0 {
            std::thread::yield_now();
        }
        let met = Arc::new((Mutex::new(0), Condvar::new()));
        let meet = |met: Arc<(Mutex<u32>, Condvar)>| {
            move || {
                let (count, cv) = &*met;
                let mut n = count.lock().unwrap();
                *n += 1;
                cv.notify_all();
                !cv.wait_timeout_while(n, Duration::from_secs(10), |n| *n < 2)
                    .unwrap()
                    .1
                    .timed_out()
            }
        };
        let a = pool.spawn(meet(Arc::clone(&met)));
        let b = pool.spawn(meet(met));
        assert!(a.wait().unwrap() && b.wait().unwrap());
        assert_eq!(pool.thread_count(), 2);
        pool.shutdown();
    }

    #[test]
    fn shutdown_cancels_queued_jobs_and_finishes_running_ones() {
        let pool = BlockingPool::new("io", 1);
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel::<()>();
        let running = pool.spawn(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            7
        });
        started.recv().unwrap();
        let queued = pool.spawn(|| 8);
        let stopper = std::thread::spawn({
            let pool = pool.clone();
            move || pool.shutdown()
        });
        // The queued job is cancelled while the running one still runs.
        assert_eq!(queued.wait(), Err(TaskError::Cancelled));
        release.send(()).unwrap();
        stopper.join().unwrap();
        assert_eq!(running.wait(), Ok(7));
    }

    #[test]
    fn a_panicking_job_reports_it_and_the_pool_goes_on() {
        let pool = BlockingPool::new("io", 1);
        assert_eq!(pool.spawn(|| panic!("boom")).wait(), Err::<(), _>(TaskError::Panicked));
        assert_eq!(pool.spawn(|| 3).wait(), Ok(3));
        pool.shutdown();
    }

    /// The pool serves dash-kv's `run_blocking`: results, caught panics, and `Cancelled` after
    /// shutdown.
    #[test]
    fn the_pool_is_the_shared_crates_blocking_io() {
        use dash_kv::{BlockingError, run_blocking};
        let pool = BlockingPool::new("io", 1);
        assert_eq!(run_blocking(&pool, || 4).wait(), Ok(4));
        assert_eq!(
            run_blocking(&pool, || panic!("boom")).wait(),
            Err::<(), _>(BlockingError::Panicked)
        );
        pool.shutdown();
        assert_eq!(run_blocking(&pool, || 5).wait(), Err(BlockingError::Cancelled));
    }

    #[test]
    fn a_task_can_be_awaited() {
        let pool = BlockingPool::new("io", 1);
        let task = pool.spawn(|| 5);
        // A minimal executor: poll, and park until the waker unparks this thread.
        struct Unpark(std::thread::Thread);
        impl std::task::Wake for Unpark {
            fn wake(self: Arc<Self>) {
                self.0.unpark();
            }
        }
        let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
        let mut cx = Context::from_waker(&waker);
        let mut task = std::pin::pin!(task);
        let out = loop {
            match task.as_mut().poll(&mut cx) {
                Poll::Ready(out) => break out,
                Poll::Pending => std::thread::park(),
            }
        };
        assert_eq!(out, Ok(5));
        pool.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn get_addr_info_resolves_numeric_names_without_lookups() {
        assert_eq!(get_addr_info("127.0.0.1", false), [("127.0.0.1".parse().unwrap(), 0)]);
        assert_eq!(get_addr_info("::1", false), [("::1".parse().unwrap(), 0)]);
        assert_eq!(get_addr_info("fe80::1%1", false), [("fe80::1".parse().unwrap(), 1)]);
        // AI_NUMERICHOST: a name is not looked up.
        assert!(get_addr_info("localhost", false).is_empty());
        assert!(get_addr_info("bad\0name", true).is_empty());
        let resolver = HostResolver::new(BlockingPool::new("dns", 1));
        assert_eq!(
            resolver.lookup("10.0.0.1", true).wait(),
            Ok(vec![("10.0.0.1".parse().unwrap(), 0)])
        );
    }
}
