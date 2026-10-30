//! Core's signal handling (`AppInitBasicSetup`): SIGTERM and SIGINT request a shutdown, SIGHUP
//! reopens the debug log, SIGPIPE is ignored; on Windows a console control event requests a
//! shutdown.
//!
//! A handler sets a latch (an atomic, as Core's flags) and then writes one wake byte to a
//! self-pipe; both are async-signal-safe. A thread wakes on the pipe, drains it and consumes the
//! latches. A full pipe drops the byte but never the request: the bytes already in the pipe wake
//! the thread, which reads the latch after them (Core: flag before token, shutdown.cpp:66-84).
//! The thread is started after the daemon fork, as the fork must happen before any thread
//! exists; signals that arrive before then wait in the latches.

use std::io;
use std::sync::Arc;

use dashd_node::logging::Logger;
use dashd_node::shutdown::ShutdownState;

/// The installed handlers, until [`Signals::start`] runs their thread.
pub struct Signals {
    #[cfg(unix)]
    read: std::os::fd::OwnedFd,
}

#[cfg(unix)]
mod imp {
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd as _, OwnedFd, RawFd};
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    use super::*;

    /// The self-pipe's write end, or -1 once this process stops listening (the daemon parent).
    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    /// SIGTERM or SIGINT arrived (Core's `fRequestShutdown`).
    static SHUTDOWN: AtomicBool = AtomicBool::new(false);
    /// SIGHUP arrived (Core's `fReopenDebugLog`).
    static REOPEN: AtomicBool = AtomicBool::new(false);

    #[cfg(any(target_os = "linux", target_os = "android"))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: always valid for the calling thread.
        unsafe { libc::__errno_location() }
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    unsafe fn errno_location() -> *mut libc::c_int {
        // SAFETY: always valid for the calling thread.
        unsafe { libc::__error() }
    }

    // Core: init.cpp:528-536 (HandleSIGTERM, HandleSIGHUP)
    extern "C" fn handle(sig: libc::c_int) {
        let fd = WRITE_FD.load(Ordering::SeqCst);
        if fd < 0 {
            return;
        }
        // The latch first, then the wake: whoever drains the byte sees the latch.
        if sig == libc::SIGHUP {
            REOPEN.store(true, Ordering::SeqCst);
        } else {
            SHUTDOWN.store(true, Ordering::SeqCst);
        }
        let byte = 0u8;
        // SAFETY: `write` is async-signal-safe; errno is restored for the interrupted code. A
        // full pipe (EAGAIN) drops the byte: the bytes in it already wake the thread.
        unsafe {
            let saved = *errno_location();
            libc::write(fd, (&raw const byte).cast(), 1);
            *errno_location() = saved;
        }
    }

    fn set_flags(fd: RawFd, nonblocking: bool) -> io::Result<()> {
        // SAFETY: `fd` is open; the calls only change its descriptor and status flags.
        unsafe {
            if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                return Err(io::Error::last_os_error());
            }
            if nonblocking {
                let flags = libc::fcntl(fd, libc::F_GETFL);
                if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }

    /// Installs the handlers. Unlike Core (`sa_flags = 0`) they restart interrupted system calls,
    /// so no blocking call elsewhere sees `EINTR`.
    // Core: init.cpp:547-554, 1164-1175
    pub fn install() -> io::Result<Signals> {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for both ends.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe` opened both descriptors; each is owned once.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        set_flags(read.as_raw_fd(), false)?;
        // A handler never blocks.
        set_flags(write.as_raw_fd(), true)?;
        // The write end lives as long as the process: a handler may run at any time.
        WRITE_FD.store(write.into_raw_fd(), Ordering::SeqCst);
        // SAFETY: an all-zero `sigaction` is valid; the handler is async-signal-safe.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handle as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = libc::SA_RESTART;
            for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        Ok(Signals {
            read,
        })
    }

    /// In the daemon's parent: its signals no longer reach the child's pipe (Core's parent sets
    /// a flag nothing reads).
    pub fn forget() {
        WRITE_FD.store(-1, Ordering::SeqCst);
    }

    impl Signals {
        /// Starts the thread that acts on the signals.
        pub fn start(self, shutdown: Arc<ShutdownState>, logger: Arc<Logger>) -> io::Result<()> {
            let read = self.read;
            std::thread::Builder::new().name("signals".into()).spawn(move || {
                let mut buf = [0u8; 256];
                loop {
                    // Latches set before the thread started, or before the bytes just drained.
                    if REOPEN.swap(false, Ordering::SeqCst) {
                        logger.request_reopen();
                    }
                    if SHUTDOWN.swap(false, Ordering::SeqCst) {
                        shutdown.start_shutdown();
                    }
                    // SAFETY: `read` is open for the thread's lifetime; `buf` is writable.
                    let n =
                        unsafe { libc::read(read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                    match n {
                        1.. => {}
                        -1 if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted => {}
                        _ => return,
                    }
                }
            })?;
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        use std::path::Path;
        use std::time::{Duration, Instant};

        use dashd_node::logging::LogOptions;

        use super::*;

        /// Lines go to `path` once a reopen was requested, to the renamed file before.
        fn reopened(logger: &Logger, path: &Path) -> bool {
            logger.log_printf("line\n");
            path.exists()
        }

        fn wait(what: &str, mut done: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(30);
            while !done() {
                assert!(Instant::now() < deadline, "{what}");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        /// A pipe full of SIGHUP wakes cannot swallow a SIGTERM (Sol r1 P1): the handler's write
        /// fails, the latch does not. Then, with the thread running, a SIGHUP still reopens.
        #[test]
        fn a_full_pipe_keeps_the_shutdown() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("debug.log");
            let logger = Arc::new(Logger::new());
            assert!(logger.start_logging());
            logger.set_options(LogOptions {
                print_to_file: true,
                file_path: path.clone(),
                ..LogOptions::default()
            });
            logger.request_reopen();
            assert!(reopened(&logger, &path));
            std::fs::rename(&path, dir.path().join("rotated.log")).unwrap();

            let signals = install().unwrap();
            // With the thread not started, SIGHUPs fill the pipe until a write would block.
            for _ in 0..1 << 20 {
                handle(libc::SIGHUP);
            }
            let fd = WRITE_FD.load(Ordering::SeqCst);
            let byte = 0u8;
            // SAFETY: writing to the nonblocking write end `install` opened.
            assert_eq!(unsafe { libc::write(fd, (&raw const byte).cast(), 1) }, -1);
            assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
            handle(libc::SIGTERM);

            let shutdown = Arc::new(ShutdownState::new());
            signals.start(Arc::clone(&shutdown), Arc::clone(&logger)).unwrap();
            wait("the SIGTERM was lost", || shutdown.shutdown_requested());
            wait("the SIGHUP was lost", || reopened(&logger, &path));

            // The running thread, its pipe drained, still acts on a latch.
            std::fs::rename(&path, dir.path().join("rotated2.log")).unwrap();
            handle(libc::SIGHUP);
            wait("the later SIGHUP was lost", || reopened(&logger, &path));

            forget();
            // SAFETY: back to the default dispositions for the rest of the test process.
            unsafe {
                for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                    libc::signal(sig, libc::SIG_DFL);
                }
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    use super::*;

    static SHUTDOWN: OnceLock<Arc<ShutdownState>> = OnceLock::new();
    static PENDING: AtomicBool = AtomicBool::new(false);

    /// Core's `consoleCtrlHandler`: requests a shutdown and never returns, so the process lives
    /// until `main` exits.
    // Core: init.cpp:538-545
    unsafe extern "system" fn handle(_ctrl_type: u32) -> i32 {
        // Before `start` connects the state, `start` sees the flag.
        PENDING.store(true, Ordering::SeqCst);
        if let Some(shutdown) = SHUTDOWN.get() {
            shutdown.start_shutdown();
        }
        loop {
            std::thread::park();
        }
    }

    // Core: init.cpp:1176-1178
    pub fn install() -> io::Result<Signals> {
        // SAFETY: `handle` has the handler signature and stays valid for the process.
        if unsafe { SetConsoleCtrlHandler(Some(handle), 1) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Signals {})
    }

    impl Signals {
        /// Connects the handler to the node's shutdown state.
        pub fn start(self, shutdown: Arc<ShutdownState>, _logger: Arc<Logger>) -> io::Result<()> {
            let shutdown = SHUTDOWN.get_or_init(|| shutdown);
            if PENDING.load(Ordering::SeqCst) {
                shutdown.start_shutdown();
            }
            Ok(())
        }
    }
}

pub use imp::*;
