//! Standard input handling.
//!
//! Core: src/compat/stdin.cpp at v24.0.0-rc.3 (`StdinTerminal`, `StdinReady`, `NoechoInst`) and
//! the `std::getline(std::cin, ...)` calls of `CommandLineRPC`.

use std::io::{BufRead, IsTerminal};

/// Core: `StdinTerminal`.
pub fn stdin_terminal() -> bool {
    std::io::stdin().is_terminal()
}

/// Core: `StdinReady`: whether reading standard input would not block. Anything but a
/// terminal is always ready.
pub fn stdin_ready() -> bool {
    if !stdin_terminal() {
        return true;
    }
    #[cfg(unix)]
    {
        let mut fds = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `fds` is a valid, initialised `pollfd` and the count is 1.
        unsafe { libc::poll(&mut fds, 1, 0) == 1 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Core: `NoechoInst` (`NO_STDIN_ECHO()`): terminal echo is off while the value is alive.
///
/// Departure from Core, which leaves echo off when a signal ends the program at the prompt (the
/// user's shell then shows nothing they type): while a terminal's echo is off, SIGINT, SIGTERM,
/// SIGHUP and SIGQUIT restore the terminal settings saved at the start and then take their
/// default action, so the exit status is the one the stock binary has. A signal the process
/// ignores stays ignored, as it does for Core.
///
/// Values nest: the settings are saved and the handlers installed when the first one is created,
/// and the terminal and the handlers are put back when the last one is dropped, in whatever
/// order they are dropped.
pub struct NoEcho {
    #[cfg(unix)]
    fd: libc::c_int,
}

/// The values alive and what the first of them replaced.
#[cfg(unix)]
struct Guards {
    live: usize,
    /// The handlers the first value replaced (`None` when the input is not a terminal).
    actions: Option<Vec<(libc::c_int, libc::sigaction)>>,
}

#[cfg(unix)]
static GUARDS: std::sync::Mutex<Guards> = std::sync::Mutex::new(Guards {
    live: 0,
    actions: None,
});

/// The terminal settings to restore from the signal handler (null when there are none).
#[cfg(unix)]
static SAVED_TERMIOS: std::sync::atomic::AtomicPtr<libc::termios> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// The descriptor those settings belong to.
#[cfg(unix)]
static SAVED_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

#[cfg(unix)]
const RESTORED_SIGNALS: [libc::c_int; 4] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

/// Restore the terminal, then die of the signal. Only async-signal-safe calls: the handler is
/// installed with `SA_RESETHAND | SA_NODEFER`, so `raise` delivers the default action at once.
#[cfg(unix)]
extern "C" fn restore_terminal_and_die(signal: libc::c_int) {
    use std::sync::atomic::Ordering;
    let saved = SAVED_TERMIOS.load(Ordering::SeqCst);
    if !saved.is_null() {
        // SAFETY: `saved` points to a leaked, initialised `termios` until the last `NoEcho` is
        // dropped.
        unsafe {
            libc::tcsetattr(SAVED_FD.load(Ordering::SeqCst), libc::TCSANOW, saved);
        }
    }
    // SAFETY: `raise` is async-signal-safe; the disposition is already the default.
    unsafe {
        libc::raise(signal);
    }
}

impl NoEcho {
    pub fn new() -> Self {
        #[cfg(unix)]
        {
            Self::on(libc::STDIN_FILENO)
        }
        #[cfg(not(unix))]
        {
            set_echo(false);
            NoEcho {}
        }
    }

    /// Echo off on the terminal `fd`.
    #[cfg(unix)]
    fn on(fd: libc::c_int) -> Self {
        let mut guards = GUARDS.lock().unwrap_or_else(|e| e.into_inner());
        if guards.live == 0 {
            guards.actions = install_restore_handlers(fd);
        }
        guards.live += 1;
        set_echo(fd, false);
        NoEcho {
            fd,
        }
    }
}

/// Save the terminal settings and catch the signals that end a prompt. Nothing happens when `fd`
/// is not a terminal.
#[cfg(unix)]
fn install_restore_handlers(fd: libc::c_int) -> Option<Vec<(libc::c_int, libc::sigaction)>> {
    use std::sync::atomic::Ordering;
    // SAFETY: `tcgetattr` fills a zeroed `termios`; `sigaction` gets initialised structs.
    unsafe {
        let mut tty: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut tty) != 0 {
            return None;
        }
        SAVED_FD.store(fd, Ordering::SeqCst);
        SAVED_TERMIOS.store(Box::into_raw(Box::new(tty)), Ordering::SeqCst);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = restore_terminal_and_die as *const () as usize;
        action.sa_flags = libc::SA_RESETHAND | libc::SA_NODEFER;
        libc::sigemptyset(&mut action.sa_mask);
        // Writing the settings back from a background process group would stop the process with
        // SIGTTOU instead of letting it die; blocked, the write goes through.
        libc::sigaddset(&mut action.sa_mask, libc::SIGTTOU);
        let mut old_actions = Vec::new();
        for signal in RESTORED_SIGNALS {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut old) != 0
                || old.sa_sigaction == libc::SIG_IGN
            {
                continue;
            }
            if libc::sigaction(signal, &action, std::ptr::null_mut()) == 0 {
                old_actions.push((signal, old));
            }
        }
        Some(old_actions)
    }
}

impl Default for NoEcho {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for NoEcho {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;
            let mut guards = GUARDS.lock().unwrap_or_else(|e| e.into_inner());
            guards.live = guards.live.saturating_sub(1);
            if guards.live > 0 {
                return;
            }
            set_echo(self.fd, true);
            // SAFETY: the saved actions come from `sigaction`; the pointer was leaked by the
            // first value, whose handlers are gone before it is freed.
            unsafe {
                for (signal, old) in guards.actions.take().into_iter().flatten() {
                    libc::sigaction(signal, &old, std::ptr::null_mut());
                }
                let saved = SAVED_TERMIOS.swap(std::ptr::null_mut(), Ordering::SeqCst);
                if !saved.is_null() {
                    drop(Box::from_raw(saved));
                }
            }
        }
        #[cfg(not(unix))]
        set_echo(true);
    }
}

/// Core: `SetStdinEcho`. Without a terminal, `tcgetattr` fails and nothing changes.
#[cfg(unix)]
fn set_echo(fd: libc::c_int, enable: bool) {
    // SAFETY: `tty` is only read after `tcgetattr` filled it in; both calls take a valid file
    // descriptor and a pointer to a live `termios`.
    unsafe {
        let mut tty: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut tty) != 0 {
            return;
        }
        if enable {
            tty.c_lflag |= libc::ECHO;
        } else {
            tty.c_lflag &= !libc::ECHO;
        }
        let _ = libc::tcsetattr(fd, libc::TCSANOW, &tty);
    }
}

/// Without termios there is no echo to change.
#[cfg(not(unix))]
fn set_echo(_enable: bool) {}

/// `std::getline(std::cin, line)`: the bytes of the next line without its `\n`, or `None` if
/// nothing could be read. The last line may lack its newline.
pub fn getline(input: &mut impl BufRead) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    match input.read_until(b'\n', &mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            Some(line)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getline_matches_std_getline() {
        let mut input: &[u8] = b"one\ntwo\r\n\nlast";
        assert_eq!(getline(&mut input).as_deref(), Some(&b"one"[..]));
        assert_eq!(getline(&mut input).as_deref(), Some(&b"two\r"[..]));
        assert_eq!(getline(&mut input).as_deref(), Some(&b""[..]));
        assert_eq!(getline(&mut input).as_deref(), Some(&b"last"[..]));
        assert_eq!(getline(&mut input), None);
    }

    /// Both orders of dropping two live values: the terminal and the handlers stay as the second
    /// value needs them until the last one is gone.
    #[cfg(unix)]
    #[test]
    fn nested_guards_restore_only_when_the_last_drops() {
        let (mut master, mut slave) = (0, 0);
        // SAFETY: `openpty` writes both descriptors; they are closed below.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(rc, 0);
        let echo_on = || {
            // SAFETY: `slave` is open; `termios` is plain data that `tcgetattr` fills.
            let mut tty: libc::termios = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::tcgetattr(slave, &mut tty) }, 0);
            tty.c_lflag & libc::ECHO != 0
        };
        let handler_installed = || {
            // SAFETY: querying the action of a signal; `old` is plain data.
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            assert_eq!(unsafe { libc::sigaction(libc::SIGTERM, std::ptr::null(), &mut old) }, 0);
            old.sa_sigaction == restore_terminal_and_die as *const () as usize
        };
        assert!(echo_on() && !handler_installed());
        for first_out in [true, false] {
            let a = NoEcho::on(slave);
            let b = NoEcho::on(slave);
            assert!(!echo_on() && handler_installed());
            let (first, last) = if first_out {
                (a, b)
            } else {
                (b, a)
            };
            drop(first);
            // one is still alive: echo off, handlers in place
            assert!(!echo_on(), "echo came back with a guard alive (first_out={first_out})");
            assert!(
                handler_installed(),
                "handlers went with a guard alive (first_out={first_out})"
            );
            drop(last);
            assert!(echo_on(), "echo not restored (first_out={first_out})");
            assert!(!handler_installed(), "handlers not restored (first_out={first_out})");
        }
        // SAFETY: both descriptors came from `openpty` above.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}
