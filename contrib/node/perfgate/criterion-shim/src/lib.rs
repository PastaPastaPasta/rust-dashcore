//! A deterministic stand-in for `criterion`, used only by the PF0 counter gate.
//!
//! `contrib/node/perfgate/run` builds the workspace's criterion benches with this crate patched in
//! (`cargo --config patch.crates-io.criterion…`), so no bench source changes. Instead of timing a
//! wall-clock-sized number of iterations, it runs exactly one benchmark for exactly
//! `PERFGATE_ITERS` iterations and counts what happens inside the measured region only:
//!
//! - `PERFGATE_MODE=counters`: user-space instructions retired (`perf_event_open`, inherited by
//!   threads spawned after `main` starts), heap allocations and bytes (a counting global
//!   allocator), page faults (`getrusage`), `/proc/self/io` and the region's peak RSS (`VmHWM`
//!   after a `clear_refs` reset), plus `instructions_total` (from `main` to the report, so work moved
//!   out of the region still counts). One JSON object is written to `PERFGATE_OUT`.
//! - `PERFGATE_MODE=syscalls`: no counters; the region is bracketed by marker syscalls
//!   (`access("/perfgate/<marker>")`) so that `strace -f` output can be cut to the region.
//! - `PERFGATE_MODE=list` (or unset): print every benchmark ID, run nothing.
//!
//! `PERFGATE_BENCH` selects the benchmark by its full criterion ID (`group/function/parameter`).
//! The process exits as soon as that benchmark has been measured.
//!
//! Only the API surface the workspace's benches use is provided; a bench that needs more fails to
//! compile under the gate, which names the missing item.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::{Display, Write as _};
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

pub use std::hint::black_box;

// ---------------------------------------------------------------------------------------------
// Counting allocator

struct CountingAlloc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

#[inline]
fn count_alloc(bytes: usize) {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    ALLOC_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` with the caller's arguments unchanged.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_alloc(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_alloc(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_alloc(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

// ---------------------------------------------------------------------------------------------
// Configuration

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    List,
    Counters,
    Syscalls,
}

struct Config {
    mode: Mode,
    bench: String,
    iters: u64,
    out: Option<String>,
    /// `instructions:u` counter fd (counters mode).
    perf_fd: i32,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

fn fail(msg: &str) -> ! {
    eprintln!("perfgate criterion shim: {msg}");
    std::process::exit(3)
}

fn config() -> &'static Config {
    CONFIG.get_or_init(|| {
        let mode = match std::env::var("PERFGATE_MODE").as_deref() {
            Err(_) | Ok("list") => Mode::List,
            Ok("counters") => Mode::Counters,
            Ok("syscalls") => Mode::Syscalls,
            Ok(other) => fail(&format!("unknown PERFGATE_MODE {other:?}")),
        };
        let bench = std::env::var("PERFGATE_BENCH").unwrap_or_default();
        let iters = match std::env::var("PERFGATE_ITERS") {
            Ok(v) => v.parse().unwrap_or_else(|_| fail(&format!("bad PERFGATE_ITERS {v:?}"))),
            Err(_) => 1,
        };
        if mode != Mode::List && bench.is_empty() {
            fail("PERFGATE_BENCH is required in counters and syscalls modes");
        }
        let perf_fd = if mode == Mode::Counters {
            perf::open_instructions()
        } else {
            -1
        };
        Config {
            mode,
            bench,
            iters,
            out: std::env::var("PERFGATE_OUT").ok(),
            perf_fd,
        }
    })
}

/// Called first thing in `main` by [`criterion_main!`], before any bench code can spawn threads,
/// so that the inherited instruction counter covers every thread the benchmark creates.
#[doc(hidden)]
pub fn __init() {
    config();
}

// ---------------------------------------------------------------------------------------------
// Counters

mod perf {
    /// `struct perf_event_attr` up to `PERF_ATTR_SIZE_VER0` (64 bytes).
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup_events: u32,
        bp_type: u32,
        config1: u64,
    }

    const PERF_TYPE_HARDWARE: u32 = 0;
    const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
    const PERF_FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
    const PERF_FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;
    const FLAG_INHERIT: u64 = 1 << 1;
    const FLAG_EXCLUDE_KERNEL: u64 = 1 << 5;
    const FLAG_EXCLUDE_HV: u64 = 1 << 6;
    const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 1 << 3;

    /// Open an enabled, inherited `instructions:u` counter on this process.
    pub fn open_instructions() -> i32 {
        let attr = Attr {
            type_: PERF_TYPE_HARDWARE,
            size: std::mem::size_of::<Attr>() as u32,
            config: PERF_COUNT_HW_INSTRUCTIONS,
            read_format: PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING,
            flags: FLAG_INHERIT | FLAG_EXCLUDE_KERNEL | FLAG_EXCLUDE_HV,
            ..Default::default()
        };
        // SAFETY: `attr` is a valid, initialised perf_event_attr of the declared size.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                &attr as *const Attr,
                0 as libc::pid_t,
                -1 as libc::c_int,
                -1 as libc::c_int,
                PERF_FLAG_FD_CLOEXEC,
            )
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            super::fail(&format!(
                "perf_event_open(instructions:u) failed: {err}; the gate needs a PMU and \
                 kernel.perf_event_paranoid <= 2"
            ));
        }
        fd as i32
    }

    /// `(count, time_enabled, time_running)`; the count includes live and exited child threads.
    pub fn read(fd: i32) -> [u64; 3] {
        let mut v = [0u64; 3];
        // SAFETY: reads at most 24 bytes into a 24-byte buffer.
        let n = unsafe { libc::read(fd, v.as_mut_ptr().cast(), std::mem::size_of_val(&v)) };
        if n != std::mem::size_of_val(&v) as isize {
            super::fail("reading the instruction counter failed");
        }
        v
    }
}

/// Read a small procfs file into a stack buffer, without allocating.
fn read_proc(path: &std::ffi::CStr, buf: &mut [u8]) -> usize {
    // SAFETY: `path` is NUL-terminated; reads stay within `buf`.
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return 0;
        }
        let mut len = 0;
        while len < buf.len() {
            let n = libc::read(fd, buf[len..].as_mut_ptr().cast(), buf.len() - len);
            if n <= 0 {
                break;
            }
            len += n as usize;
        }
        libc::close(fd);
        len
    }
}

/// The value of `key` in a `key: value [unit]` procfs listing.
fn proc_field(text: &[u8], key: &[u8]) -> u64 {
    for line in text.split(|&b| b == b'\n') {
        if let Some(rest) = line.strip_prefix(key) {
            let digits = rest.iter().skip_while(|b| !b.is_ascii_digit());
            return digits
                .take_while(|b| b.is_ascii_digit())
                .fold(0, |acc, &b| acc * 10 + u64::from(b - b'0'));
        }
    }
    0
}

/// Counter values at one instant. Taking a snapshot never allocates.
#[derive(Clone, Copy, Default)]
struct Snapshot {
    instructions: u64,
    enabled: u64,
    running: u64,
    allocs: u64,
    alloc_bytes: u64,
    page_faults: u64,
    rchar: u64,
    wchar: u64,
    syscr: u64,
    syscw: u64,
}

impl Snapshot {
    fn page_faults() -> u64 {
        // SAFETY: getrusage fills the zeroed struct.
        unsafe {
            let mut ru: libc::rusage = std::mem::zeroed();
            libc::getrusage(libc::RUSAGE_SELF, &mut ru);
            (ru.ru_minflt + ru.ru_majflt) as u64
        }
    }

    /// Taken in this order so that the instruction counter is read closest to the region.
    fn start(fd: i32) -> Self {
        let mut s = Snapshot::default();
        s.read_io();
        s.page_faults = Self::page_faults();
        s.allocs = ALLOCS.load(Ordering::SeqCst);
        s.alloc_bytes = ALLOC_BYTES.load(Ordering::SeqCst);
        [s.instructions, s.enabled, s.running] = perf::read(fd);
        s
    }

    fn end(fd: i32) -> Self {
        let mut s = Snapshot::default();
        [s.instructions, s.enabled, s.running] = perf::read(fd);
        s.allocs = ALLOCS.load(Ordering::SeqCst);
        s.alloc_bytes = ALLOC_BYTES.load(Ordering::SeqCst);
        s.page_faults = Self::page_faults();
        s.read_io();
        s
    }

    fn read_io(&mut self) {
        let mut buf = [0u8; 512];
        let n = read_proc(c"/proc/self/io", &mut buf);
        let text = &buf[..n];
        self.rchar = proc_field(text, b"rchar:");
        self.wchar = proc_field(text, b"wchar:");
        self.syscr = proc_field(text, b"syscr:");
        self.syscw = proc_field(text, b"syscw:");
    }
}

/// Accumulated counter deltas over every measured span of one benchmark.
#[derive(Default)]
struct Region {
    total: Snapshot,
    spans: u64,
    open: Option<Snapshot>,
}

impl Region {
    fn begin(&mut self) {
        let cfg = config();
        match cfg.mode {
            Mode::Counters => self.open = Some(Snapshot::start(cfg.perf_fd)),
            Mode::Syscalls => marker(c"/perfgate/begin"),
            Mode::List => {}
        }
    }

    fn end(&mut self) {
        let cfg = config();
        match cfg.mode {
            Mode::Counters => {
                let end = Snapshot::end(cfg.perf_fd);
                let start = self.open.take().expect("region end without begin");
                let t = &mut self.total;
                t.instructions += end.instructions - start.instructions;
                t.enabled += end.enabled - start.enabled;
                t.running += end.running - start.running;
                t.allocs += end.allocs - start.allocs;
                t.alloc_bytes += end.alloc_bytes - start.alloc_bytes;
                t.page_faults += end.page_faults - start.page_faults;
                t.rchar += end.rchar - start.rchar;
                t.wchar += end.wchar - start.wchar;
                t.syscr += end.syscr - start.syscr;
                t.syscw += end.syscw - start.syscw;
            }
            Mode::Syscalls => marker(c"/perfgate/end"),
            Mode::List => {}
        }
        self.spans += 1;
    }
}

/// A syscall that strace shows verbatim, so the gate can cut its log to the measured region.
fn marker(path: &std::ffi::CStr) {
    // SAFETY: `path` is NUL-terminated; the call only probes for a non-existent file.
    unsafe {
        libc::access(path.as_ptr(), libc::F_OK);
    }
}

/// Reset the process's peak RSS so that `VmHWM` afterwards covers only the region.
fn reset_peak_rss() {
    // SAFETY: writes one byte from a static buffer to our own clear_refs.
    unsafe {
        let fd = libc::open(c"/proc/self/clear_refs".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd >= 0 {
            libc::write(fd, b"5".as_ptr().cast(), 1);
            libc::close(fd);
        }
    }
}

fn peak_rss_kib() -> u64 {
    let mut buf = [0u8; 4096];
    let n = read_proc(c"/proc/self/status", &mut buf);
    proc_field(&buf[..n], b"VmHWM:")
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write the result for the selected benchmark and end the process.
fn report(id: &str, region: &Region, peak_rss_kib: u64) -> ! {
    let cfg = config();
    let t = &region.total;
    let mut line = format!(
        "{{\"id\":{},\"mode\":\"{}\",\"iters\":{},\"spans\":{}",
        json_str(id),
        if cfg.mode == Mode::Counters {
            "counters"
        } else {
            "syscalls"
        },
        cfg.iters,
        region.spans
    );
    if cfg.mode == Mode::Counters {
        // Everything since `main` started, setup and the drops after the last span included: work a
        // bench moves out of the measured region still shows here.
        let [total, ..] = perf::read(cfg.perf_fd);
        let _ = write!(line, ",\"instructions_total\":{total}");
        let _ = write!(
            line,
            ",\"instructions\":{},\"time_enabled\":{},\"time_running\":{},\"allocs\":{},\
             \"alloc_bytes\":{},\"page_faults\":{},\"read_bytes\":{},\"write_bytes\":{},\
             \"read_syscalls\":{},\"write_syscalls\":{},\"peak_rss_kib\":{}",
            t.instructions,
            t.enabled,
            t.running,
            t.allocs,
            t.alloc_bytes,
            t.page_faults,
            t.rchar,
            t.wchar,
            t.syscr,
            t.syscw,
            peak_rss_kib
        );
    }
    line.push_str("}\n");
    let written = match &cfg.out {
        Some(path) => std::fs::write(path, &line),
        None => std::io::stdout().write_all(line.as_bytes()),
    };
    if let Err(e) = written {
        fail(&format!("writing the result failed: {e}"));
    }
    std::process::exit(0)
}

// ---------------------------------------------------------------------------------------------
// Criterion API

/// Ignored; kept for source compatibility.
#[derive(Debug, Clone, Copy)]
pub enum Throughput {
    Bytes(u64),
    BytesDecimal(u64),
    Elements(u64),
}

/// How [`Bencher::iter_batched`] groups inputs, mirroring criterion's batch sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchSize {
    SmallInput,
    LargeInput,
    PerIteration,
    NumBatches(u64),
    NumIterations(u64),
}

impl BatchSize {
    fn iters_per_batch(self, iters: u64) -> u64 {
        let n = match self {
            BatchSize::SmallInput => iters.div_ceil(10),
            BatchSize::LargeInput => iters.div_ceil(1000),
            BatchSize::PerIteration => 1,
            BatchSize::NumBatches(b) => iters.div_ceil(b.max(1)),
            BatchSize::NumIterations(n) => n,
        };
        n.max(1)
    }

    /// The batch lengths that make up `iters` iterations: full batches, then any remainder.
    fn batches(self, iters: u64) -> impl Iterator<Item = u64> {
        let batch = self.iters_per_batch(iters);
        let mut remaining = iters;
        std::iter::from_fn(move || {
            let n = batch.min(remaining);
            remaining -= n;
            (n > 0).then_some(n)
        })
    }
}

#[derive(Debug, Clone)]
pub struct BenchmarkId {
    function: Option<String>,
    parameter: Option<String>,
}

impl BenchmarkId {
    pub fn new<S: Into<String>, P: Display>(function_name: S, parameter: P) -> Self {
        BenchmarkId {
            function: Some(function_name.into()),
            parameter: Some(parameter.to_string()),
        }
    }

    pub fn from_parameter<P: Display>(parameter: P) -> Self {
        BenchmarkId {
            function: None,
            parameter: Some(parameter.to_string()),
        }
    }
}

pub trait IntoBenchmarkId {
    fn into_benchmark_id(self) -> BenchmarkId;
}

impl IntoBenchmarkId for BenchmarkId {
    fn into_benchmark_id(self) -> BenchmarkId {
        self
    }
}

impl<S: Into<String>> IntoBenchmarkId for S {
    fn into_benchmark_id(self) -> BenchmarkId {
        BenchmarkId {
            function: Some(self.into()),
            parameter: None,
        }
    }
}

/// Runs one benchmark if it is the selected one.
fn run_bench(id: String, f: impl FnOnce(&mut Bencher)) {
    let cfg = config();
    match cfg.mode {
        Mode::List => println!("{id}"),
        _ if id != cfg.bench => {}
        _ => {
            let mut b = Bencher {
                iters: cfg.iters,
                region: Region::default(),
            };
            if cfg.mode == Mode::Counters {
                reset_peak_rss();
            }
            f(&mut b);
            if b.region.spans == 0 {
                fail(&format!("benchmark {id:?} never called an iter method"));
            }
            let peak = if cfg.mode == Mode::Counters {
                peak_rss_kib()
            } else {
                0
            };
            report(&id, &b.region, peak);
        }
    }
}

#[derive(Default)]
pub struct Criterion {
    _private: (),
}

impl Criterion {
    pub fn configure_from_args(self) -> Self {
        self
    }

    pub fn sample_size(self, _n: usize) -> Self {
        self
    }

    pub fn warm_up_time(self, _d: Duration) -> Self {
        self
    }

    pub fn measurement_time(self, _d: Duration) -> Self {
        self
    }

    pub fn noise_threshold(self, _t: f64) -> Self {
        self
    }

    pub fn significance_level(self, _s: f64) -> Self {
        self
    }

    pub fn confidence_level(self, _c: f64) -> Self {
        self
    }

    pub fn nresamples(self, _n: usize) -> Self {
        self
    }

    pub fn without_plots(self) -> Self {
        self
    }

    pub fn bench_function<F: FnMut(&mut Bencher)>(&mut self, id: &str, f: F) -> &mut Self {
        run_bench(id.to_owned(), f);
        self
    }

    pub fn bench_with_input<I: ?Sized, F: FnMut(&mut Bencher, &I)>(
        &mut self,
        id: BenchmarkId,
        input: &I,
        mut f: F,
    ) -> &mut Self {
        run_bench(full_id(None, id), |b| f(b, input));
        self
    }

    pub fn benchmark_group<S: Into<String>>(&mut self, group_name: S) -> BenchmarkGroup<'_> {
        BenchmarkGroup {
            name: group_name.into(),
            _criterion: self,
        }
    }

    /// Reached only when the selected benchmark does not exist (a found one exits the process).
    pub fn final_summary(&self) {
        let cfg = config();
        if cfg.mode != Mode::List {
            fail(&format!("no benchmark with id {:?} in this binary", cfg.bench));
        }
    }
}

fn full_id(group: Option<&str>, id: BenchmarkId) -> String {
    [group, id.function.as_deref(), id.parameter.as_deref()]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("/")
}

pub struct BenchmarkGroup<'a> {
    name: String,
    _criterion: &'a mut Criterion,
}

impl BenchmarkGroup<'_> {
    pub fn sample_size(&mut self, _n: usize) -> &mut Self {
        self
    }

    pub fn warm_up_time(&mut self, _d: Duration) -> &mut Self {
        self
    }

    pub fn measurement_time(&mut self, _d: Duration) -> &mut Self {
        self
    }

    pub fn throughput(&mut self, _t: Throughput) -> &mut Self {
        self
    }

    pub fn bench_function<ID: IntoBenchmarkId, F: FnMut(&mut Bencher)>(
        &mut self,
        id: ID,
        f: F,
    ) -> &mut Self {
        run_bench(full_id(Some(&self.name), id.into_benchmark_id()), f);
        self
    }

    pub fn bench_with_input<ID: IntoBenchmarkId, I: ?Sized, F: FnMut(&mut Bencher, &I)>(
        &mut self,
        id: ID,
        input: &I,
        mut f: F,
    ) -> &mut Self {
        run_bench(full_id(Some(&self.name), id.into_benchmark_id()), |b| f(b, input));
        self
    }

    pub fn finish(self) {}
}

pub struct Bencher {
    iters: u64,
    region: Region,
}

impl Bencher {
    pub fn iter<O, R: FnMut() -> O>(&mut self, mut routine: R) {
        self.region.begin();
        for _ in 0..self.iters {
            black_box(routine());
        }
        self.region.end();
    }

    pub fn iter_batched<I, O, S: FnMut() -> I, R: FnMut(I) -> O>(
        &mut self,
        mut setup: S,
        mut routine: R,
        size: BatchSize,
    ) {
        for n in size.batches(self.iters) {
            let inputs: Vec<I> = (0..n).map(|_| setup()).collect();
            let mut outputs = Vec::with_capacity(n as usize);
            self.region.begin();
            for input in inputs {
                outputs.push(black_box(routine(input)));
            }
            self.region.end();
            drop(outputs);
        }
    }

    pub fn iter_batched_ref<I, O, S: FnMut() -> I, R: FnMut(&mut I) -> O>(
        &mut self,
        mut setup: S,
        mut routine: R,
        size: BatchSize,
    ) {
        for n in size.batches(self.iters) {
            let mut inputs: Vec<I> = (0..n).map(|_| setup()).collect();
            let mut outputs = Vec::with_capacity(n as usize);
            self.region.begin();
            for input in inputs.iter_mut() {
                outputs.push(black_box(routine(input)));
            }
            self.region.end();
            drop(outputs);
            drop(inputs);
        }
    }

    pub fn iter_with_setup<I, O, S: FnMut() -> I, R: FnMut(I) -> O>(
        &mut self,
        setup: S,
        routine: R,
    ) {
        self.iter_batched(setup, routine, BatchSize::PerIteration);
    }

    pub fn iter_with_large_drop<O, R: FnMut() -> O>(&mut self, mut routine: R) {
        self.iter_batched(|| (), |()| routine(), BatchSize::SmallInput);
    }

    #[cfg(feature = "async_tokio")]
    pub fn to_async<A: AsyncExecutor>(&mut self, runner: A) -> AsyncBencher<'_, A> {
        AsyncBencher {
            b: self,
            runner,
        }
    }
}

/// Runs a future to completion; implemented for tokio runtimes.
#[cfg(feature = "async_tokio")]
pub trait AsyncExecutor {
    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T;
}

#[cfg(feature = "async_tokio")]
impl AsyncExecutor for tokio::runtime::Runtime {
    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Runtime::block_on(self, future)
    }
}

#[cfg(feature = "async_tokio")]
impl AsyncExecutor for &tokio::runtime::Runtime {
    fn block_on<T>(&self, future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Runtime::block_on(self, future)
    }
}

#[cfg(feature = "async_tokio")]
pub struct AsyncBencher<'b, A: AsyncExecutor> {
    b: &'b mut Bencher,
    runner: A,
}

#[cfg(feature = "async_tokio")]
impl<A: AsyncExecutor> AsyncBencher<'_, A> {
    pub fn iter<O, R, F>(&mut self, mut routine: R)
    where
        R: FnMut() -> F,
        F: std::future::Future<Output = O>,
    {
        let (iters, region) = (self.b.iters, &mut self.b.region);
        self.runner.block_on(async {
            region.begin();
            for _ in 0..iters {
                black_box(routine().await);
            }
            region.end();
        });
    }

    pub fn iter_batched<I, O, S, R, F>(&mut self, mut setup: S, mut routine: R, size: BatchSize)
    where
        S: FnMut() -> I,
        R: FnMut(I) -> F,
        F: std::future::Future<Output = O>,
    {
        for n in size.batches(self.b.iters) {
            let inputs: Vec<I> = (0..n).map(|_| setup()).collect();
            let mut outputs = Vec::with_capacity(n as usize);
            let region = &mut self.b.region;
            self.runner.block_on(async {
                region.begin();
                for input in inputs {
                    outputs.push(black_box(routine(input).await));
                }
                region.end();
            });
            drop(outputs);
        }
    }
}

#[macro_export]
macro_rules! criterion_group {
    (name = $name:ident; config = $config:expr; targets = $( $target:path ),+ $(,)*) => {
        pub fn $name() {
            let mut criterion: $crate::Criterion = $config.configure_from_args();
            $( $target(&mut criterion); )+
        }
    };
    ($name:ident, $( $target:path ),+ $(,)*) => {
        $crate::criterion_group! {
            name = $name;
            config = $crate::Criterion::default();
            targets = $( $target ),+
        }
    };
}

#[macro_export]
macro_rules! criterion_main {
    ( $( $group:path ),+ $(,)* ) => {
        fn main() {
            $crate::__init();
            $( $group(); )+
            $crate::Criterion::default().final_summary();
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_sizes_match_criterion() {
        assert_eq!(BatchSize::SmallInput.iters_per_batch(100), 10);
        assert_eq!(BatchSize::SmallInput.iters_per_batch(3), 1);
        assert_eq!(BatchSize::LargeInput.iters_per_batch(5000), 5);
        assert_eq!(BatchSize::PerIteration.iters_per_batch(5000), 1);
        assert_eq!(BatchSize::NumBatches(4).iters_per_batch(10), 3);
        assert_eq!(BatchSize::NumIterations(7).iters_per_batch(10), 7);
        assert_eq!(BatchSize::NumBatches(4).batches(10).collect::<Vec<_>>(), [3, 3, 3, 1]);
        assert_eq!(BatchSize::PerIteration.batches(0).count(), 0);
    }

    #[test]
    fn ids_join_like_criterion() {
        assert_eq!(full_id(Some("sha256"), BenchmarkId::from_parameter("1k")), "sha256/1k");
        assert_eq!(
            full_id(Some("siphash24"), BenchmarkId::new("batch", 1024)),
            "siphash24/batch/1024"
        );
        assert_eq!(full_id(Some("cmp"), "32b_eq".into_benchmark_id()), "cmp/32b_eq");
        assert_eq!(full_id(None, "storage/disk/get".into_benchmark_id()), "storage/disk/get");
    }

    #[test]
    fn proc_fields_parse() {
        let io = b"rchar: 123\nwchar: 45\nsyscr: 6\nsyscw: 7\n";
        assert_eq!(proc_field(io, b"rchar:"), 123);
        assert_eq!(proc_field(io, b"syscw:"), 7);
        assert_eq!(proc_field(b"VmHWM:\t  2048 kB\n", b"VmHWM:"), 2048);
        assert_eq!(proc_field(io, b"missing:"), 0);
    }

    #[test]
    fn json_strings_escape() {
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\u000a\"");
    }
}
