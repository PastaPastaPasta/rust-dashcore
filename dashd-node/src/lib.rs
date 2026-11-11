//! The Rust Dash node's process shell: Core's init and shutdown order, Core-format
//! logging, the scheduler, and the host services the node's crates share. `rdashd` is a thin
//! `main` over this crate.
//!
//! Each item names the Dash Core v24.0.0-rc.3 code it ports.

// A build without debug assertions (`cargo bench`, any release build) refuses the test seams,
// so no bench measures them and no product rdashd carries them. Clippy is exempt: the pre-push
// hook lints `--release --all-features`.
#[cfg(all(feature = "test-hooks", not(debug_assertions), not(clippy)))]
compile_error!("the `test-hooks` feature is for tests only; build without it");
#[cfg(all(feature = "test-support", not(debug_assertions), not(clippy)))]
compile_error!("the `test-support` feature is for tests only; build without it");

pub mod abort;
pub mod chain;
pub mod chainstate;
pub mod datadir;
pub mod error;
pub mod host;
pub mod init;
#[cfg(feature = "test-hooks")]
pub mod livecopy;
pub mod logging;
pub mod mnsync;
pub mod node;
pub mod notify;
pub mod p2p;
pub mod rpc;
pub mod scheduler;
pub mod shutdown;
pub mod sporks;
pub mod steps;
#[cfg(feature = "test-support")]
pub mod sut;
#[cfg(test)]
mod test_log;
pub mod ui;
pub mod warnings;

pub use error::InitFailure;
pub use node::Node;

/// The program name in the version line and the messages Core words with `PACKAGE_NAME`.
/// rdashd names itself (DEC-201): its logs and errors never claim to be Dash Core's.
pub const PACKAGE_NAME: &str = "rdashd";

/// Core's `FormatFullVersion()` for this build.
pub const FULL_VERSION: &str = concat!("v", env!("CARGO_PKG_VERSION"));

/// Core's `CLIENT_VERSION` for this build's version, by Core's formula (`clientversion.h:33-36`):
/// `10000·major + 100·minor + patch`.
pub const CLIENT_VERSION: i64 = 10_000 * version_part(env!("CARGO_PKG_VERSION_MAJOR"))
    + 100 * version_part(env!("CARGO_PKG_VERSION_MINOR"))
    + version_part(env!("CARGO_PKG_VERSION_PATCH"));

/// A version component's digits.
const fn version_part(digits: &str) -> i64 {
    match i64::from_str_radix(digits, 10) {
        Ok(value) => value,
        Err(_) => panic!("a numeric version component"),
    }
}

/// BL-598: the unit tests run many nodes at once and peak near 1,000 descriptors, past macOS's
/// default soft `RLIMIT_NOFILE` of 256. libtest has no setup hook, so the binary raises its soft
/// limit as init step 2 does (`init::checks::parameter_interaction`), from the loader's
/// constructor list before `main`.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[used]
#[cfg_attr(target_os = "linux", unsafe(link_section = ".init_array"))]
#[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__mod_init_func"))]
static RAISE_TEST_FD_LIMIT: extern "C" fn() = {
    extern "C" fn raise() {
        init::checks::raise_file_descriptor_limit(TEST_FD_LIMIT);
    }
    raise
};

// Under `OPEN_MAX` (10240), the most macOS's `setrlimit(2)` page says to ask for.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
const TEST_FD_LIMIT: i32 = 4096;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::TEST_FD_LIMIT;

    #[test]
    #[ignore = "the child of the test below"]
    fn the_fd_limit_was_raised() {
        // SAFETY: getrlimit writes only the struct we pass.
        let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) }, 0);
        let want = (TEST_FD_LIMIT as libc::rlim_t).min(limit.rlim_max);
        assert!(limit.rlim_cur >= want, "soft {} < {want}", limit.rlim_cur);
    }

    /// A run started at macOS's soft limit gets the raise (the gate starts at the hard limit).
    #[test]
    fn a_binary_started_at_a_low_fd_limit_raises_it() {
        let out = std::process::Command::new("sh")
            .args([
                "-c",
                r#"ulimit -Sn 256 && exec "$0" --exact tests::the_fd_limit_was_raised --ignored"#,
            ])
            .arg(std::env::current_exe().unwrap())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{}\n{stdout}{stderr}",
            out.status
        );
    }
}
