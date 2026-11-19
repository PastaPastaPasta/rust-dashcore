//! The SIGPIPE disposition the binary inherited at exec. Rust's runtime ignores SIGPIPE before
//! `main`, where Core's programs keep the inherited disposition: dashd until `AppInitBasicSetup`
//! ignores it, dash-cli throughout. So from a shell a write to a closed pipe kills them, and
//! under a parent that ignores SIGPIPE (systemd's default) the write fails and they go on. A
//! constructor reads the disposition before the runtime starts and `main` puts it back. On
//! targets without a constructor arm nothing is read.
//!
//! rdashd and rdash-cli each carry this file, and a test in rdash-cli keeps the two identical.
// Core: init.cpp:1177-1178 (dashd); bitcoin-cli.cpp sets no signal disposition (dash-cli)

use std::sync::atomic::{AtomicU8, Ordering};

/// Not read: no constructor ran.
const UNKNOWN: u8 = 0;
const DEFAULT: u8 = 1;
const IGNORED: u8 = 2;

static INHERITED: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Reads the disposition. It runs before Rust's runtime is set up, so it touches no runtime
/// state: one `sigaction` read and an atomic store, no allocation, no panic.
#[cfg(any(target_os = "linux", target_os = "macos"))]
extern "C" fn read_inherited() {
    // SAFETY: an all-zero `sigaction` is a valid output buffer, and a null `act` only reads the
    // disposition.
    let disposition = unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut old) != 0 {
            return;
        }
        old.sa_sigaction
    };
    // A caught signal is reset to the default at exec, so only these two can be inherited.
    let inherited = if disposition == libc::SIG_IGN {
        IGNORED
    } else {
        DEFAULT
    };
    INHERITED.store(inherited, Ordering::Relaxed);
}

// Nothing names the constructors, so without `#[used]` a release build drops them.
#[cfg(target_os = "linux")]
#[used]
#[unsafe(link_section = ".init_array")]
static READ_INHERITED: extern "C" fn() = read_inherited;

#[cfg(target_os = "macos")]
#[used]
#[unsafe(link_section = "__DATA,__mod_init_func,mod_init_funcs")]
static READ_INHERITED: extern "C" fn() = read_inherited;

/// Puts back the disposition the constructor read. Call it before any output. Returns whether
/// there was one to put back: false where no constructor ran.
pub fn restore_inherited() -> bool {
    let handler = match INHERITED.load(Ordering::Relaxed) {
        DEFAULT => libc::SIG_DFL,
        IGNORED => libc::SIG_IGN,
        _ => return false,
    };
    // SAFETY: setting SIGPIPE to the default or to ignore installs no handler.
    unsafe {
        libc::signal(libc::SIGPIPE, handler);
    }
    true
}
