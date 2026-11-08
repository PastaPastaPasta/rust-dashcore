//! `randomenv.cpp`: the dynamic environment as Core writes it on Linux (each C struct as its
//! raw bytes), and a reduced static environment.

#[cfg(unix)]
use std::mem::MaybeUninit;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::EnvSink;

/// The paths `RandAddDynamicEnv` reads on Linux, in its order.
// Core: randomenv.cpp:270-278
#[cfg(target_os = "linux")]
pub(crate) const DYNAMIC_FILES: [&std::ffi::CStr; 9] = [
    c"/proc/diskstats",
    c"/proc/vmstat",
    c"/proc/schedstat",
    c"/proc/zoneinfo",
    c"/proc/meminfo",
    c"/proc/softirqs",
    c"/proc/stat",
    c"/proc/self/schedstat",
    c"/proc/self/status",
];

/// `hasher << value` for a C struct: all of its bytes, padding included.
///
/// # Safety
///
/// Every byte of `value` is initialized: it was made with [`MaybeUninit::zeroed`] and written
/// since only through raw pointers.
#[cfg(unix)]
unsafe fn write_raw<T>(sink: &mut dyn EnvSink, value: &MaybeUninit<T>) {
    // SAFETY: the caller's contract; the slice covers exactly `value`'s bytes.
    sink.write(unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), size_of::<T>()) });
}

/// Nanoseconds since the Unix epoch: libstdc++'s `system_clock` (and `high_resolution_clock`)
/// count.
fn system_clock_count() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i64,
        Err(e) => -(e.duration().as_nanos() as i64),
    }
}

/// `AddFile`'s read loop: 4096-byte reads, each written when positive, stopping at the first
/// read that is not a full 4096 bytes or once 1 MiB is counted. A failed read (−1) still adds
/// to the count, as Core's `size_t` sum does.
// Core: randomenv.cpp:148-154
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn add_file_contents(sink: &mut dyn EnvSink, mut read: impl FnMut(&mut [u8]) -> isize) {
    let mut fbuf = [0u8; 4096];
    let mut total: usize = 0;
    loop {
        let n = read(&mut fbuf);
        if n > 0 {
            sink.write(&fbuf[..n as usize]);
        }
        total = total.wrapping_add(n as usize);
        if !(n == fbuf.len() as isize && total < 1_048_576) {
            break;
        }
    }
}

/// `AddFile`: the descriptor number, the file's `stat`, then its contents as
/// [`add_file_contents`] reads them; nothing when it does not open.
// Core: randomenv.cpp:139-157
#[cfg(target_os = "linux")]
fn add_file(sink: &mut dyn EnvSink, path: &'static std::ffi::CStr) {
    sink.label(path.to_str().unwrap_or("?"));
    let mut sb = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `path` is a NUL-terminated string.
    let f = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if f == -1 {
        return;
    }
    sink.write(&f.to_ne_bytes());
    // SAFETY: `sb` is a writable `stat` and `f` an open descriptor.
    if unsafe { libc::fstat(f, sb.as_mut_ptr()) } == 0 {
        // SAFETY: `sb` was zeroed, then written by fstat through its pointer.
        unsafe { write_raw(sink, &sb) };
    }
    add_file_contents(sink, |buf| {
        // SAFETY: `buf` is writable for its length, and `f` is open until the close below.
        unsafe { libc::read(f, buf.as_mut_ptr().cast(), buf.len()) }
    });
    // SAFETY: `f` is open and closed once.
    unsafe { libc::close(f) };
}

/// The clocks: `clock_gettime` (one `timespec`, reused), `gettimeofday`, and the three standard
/// library clocks.
// Core: randomenv.cpp:234-261
fn add_clocks(sink: &mut dyn EnvSink) {
    #[cfg(unix)]
    {
        let mut ts = MaybeUninit::<libc::timespec>::zeroed();
        #[cfg(target_os = "linux")]
        let clocks = [
            ("CLOCK_MONOTONIC", libc::CLOCK_MONOTONIC),
            ("CLOCK_REALTIME", libc::CLOCK_REALTIME),
            ("CLOCK_BOOTTIME", libc::CLOCK_BOOTTIME),
        ];
        #[cfg(not(target_os = "linux"))]
        let clocks =
            [("CLOCK_MONOTONIC", libc::CLOCK_MONOTONIC), ("CLOCK_REALTIME", libc::CLOCK_REALTIME)];
        for (label, clock) in clocks {
            sink.label(label);
            // SAFETY: `ts` is a writable `timespec`.
            unsafe { libc::clock_gettime(clock, ts.as_mut_ptr()) };
            // SAFETY: `ts` was zeroed, then written by clock_gettime through its pointer.
            unsafe { write_raw(sink, &ts) };
        }
        sink.label("gettimeofday");
        let mut tv = MaybeUninit::<libc::timeval>::zeroed();
        // SAFETY: `tv` is a writable `timeval`; no time zone is asked for.
        unsafe { libc::gettimeofday(tv.as_mut_ptr(), std::ptr::null_mut()) };
        // SAFETY: `tv` was zeroed, then written by gettimeofday through its pointer.
        unsafe { write_raw(sink, &tv) };
    }
    sink.label("system_clock");
    sink.write(&system_clock_count().to_ne_bytes());
    sink.label("steady_clock");
    sink.write(&steady_clock_count().to_ne_bytes());
    sink.label("high_resolution_clock");
    sink.write(&system_clock_count().to_ne_bytes());
}

/// libstdc++'s `steady_clock` count: `CLOCK_MONOTONIC` in nanoseconds.
#[cfg(unix)]
// `time_t` and `long` are 32 bits on some targets.
#[allow(clippy::useless_conversion)]
fn steady_clock_count() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a writable `timespec`.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    i64::from(ts.tv_sec).wrapping_mul(1_000_000_000).wrapping_add(i64::from(ts.tv_nsec))
}

#[cfg(not(unix))]
fn steady_clock_count() -> i64 {
    crate::entropy::epoch().elapsed().as_nanos() as i64
}

/// `RandAddDynamicEnv`.
// Core: randomenv.cpp:230-309
pub(crate) fn dynamic_env(sink: &mut dyn EnvSink) {
    add_clocks(sink);
    #[cfg(unix)]
    {
        sink.label("rusage");
        let mut usage = MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `usage` is a writable `rusage`.
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } == 0 {
            // SAFETY: `usage` was zeroed, then written by getrusage through its pointer.
            unsafe { write_raw(sink, &usage) };
        }
    }
    #[cfg(target_os = "linux")]
    for path in DYNAMIC_FILES {
        add_file(sink, path);
    }
    // Stack and heap location: the address of a stack slot holding a fresh 4097-byte
    // allocation's address, then that address.
    let heap = Vec::<u8>::with_capacity(4097);
    let addr = std::hint::black_box(heap.as_ptr() as usize);
    sink.label("stack");
    sink.write(&(&raw const addr as usize).to_ne_bytes());
    sink.label("heap");
    sink.write(&addr.to_ne_bytes());
}

/// `RandAddStaticEnv`, reduced: `uname`, the host name, the process, parent, user and group
/// ids, the thread id, the auxiliary vector's `AT_HWCAP`, `AT_HWCAP2` and `AT_RANDOM`, and
/// `/proc/self/stat`.
// Core: randomenv.cpp:311-513
pub(crate) fn static_env(sink: &mut dyn EnvSink) {
    #[cfg(unix)]
    {
        let cstr_with_nul = |field: &[libc::c_char]| -> Vec<u8> {
            let len = field.iter().position(|&c| c == 0).unwrap_or(field.len());
            let mut v: Vec<u8> = field[..len].iter().map(|&c| c.to_ne_bytes()[0]).collect();
            v.push(0);
            v
        };
        let mut name = MaybeUninit::<libc::utsname>::zeroed();
        // SAFETY: `name` is a writable `utsname`.
        if unsafe { libc::uname(name.as_mut_ptr()) } != -1 {
            // SAFETY: zeroed, then written by uname through its pointer.
            let name = unsafe { name.assume_init_ref() };
            for field in
                [&name.sysname, &name.nodename, &name.release, &name.version, &name.machine]
            {
                sink.write(&cstr_with_nul(field));
            }
        }
        let mut hname = [0 as libc::c_char; 256];
        // SAFETY: `hname` is writable for its 256 bytes.
        if unsafe { libc::gethostname(hname.as_mut_ptr(), hname.len()) } == 0 {
            let bytes = cstr_with_nul(&hname);
            sink.write(&bytes[..bytes.len() - 1]);
        }
        // SAFETY: these calls take no arguments and cannot fail.
        let ids = unsafe {
            [libc::getpid() as u32, libc::getppid() as u32, libc::getuid(), libc::getgid()]
        };
        for id in ids {
            sink.write(&id.to_ne_bytes());
        }
        // SAFETY: as above.
        sink.write(&(unsafe { libc::pthread_self() } as u64).to_ne_bytes());
    }
    #[cfg(not(unix))]
    sink.write(&std::process::id().to_ne_bytes());
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getauxval reads the auxiliary vector and returns 0 for an absent entry.
        let (hwcap, hwcap2, random) = unsafe {
            (
                libc::getauxval(libc::AT_HWCAP),
                libc::getauxval(libc::AT_HWCAP2),
                libc::getauxval(libc::AT_RANDOM),
            )
        };
        sink.write(&hwcap.to_ne_bytes());
        sink.write(&hwcap2.to_ne_bytes());
        if random != 0 {
            // SAFETY: AT_RANDOM is the address of 16 random bytes the kernel placed on the
            // initial stack, which live for the whole process.
            sink.write(unsafe { std::slice::from_raw_parts(random as *const u8, 16) });
        }
        add_file(sink, c"/proc/self/stat");
    }
}
