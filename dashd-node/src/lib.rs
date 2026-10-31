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
