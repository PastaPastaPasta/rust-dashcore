//! `rdash-cli`: a byte-compatible port of Dash Core v24's `dash-cli` (src/bitcoin-cli.cpp at
//! v24.0.0-rc.3).
//!
//! Options and configuration come from `dash-args`; the RPC parameter conversion table from
//! `dash-rpc-spec`. See `PORTING.md` for the mapping to Core's code and what is not ported.
//!
//! Byte-for-byte parity with the stock `dash-cli` is shown by the XCLI differential test
//! (`tests/xcli`). It is proven on Linux only: other Unix systems run a portable subset and list
//! the rest as an ignored test with the reason, and the test is not built on other platforms.

pub mod cli;
pub mod convert;
pub mod handlers;
pub mod http;
pub mod introsort;
pub mod json;
pub mod stdin;

pub use cli::{CliError, main_with, non_utf8_argument_message};

/// Core: `PACKAGE_NAME`.
pub const PACKAGE_NAME: &str = "Dash Core";
/// Core: `FormatFullVersion()` of the release whose dash-cli this is.
pub const CLIENT_VERSION: &str = dash_rpc_spec::CORE_VERSION;
/// Core: `COPYRIGHT_YEAR` of that release.
pub const COPYRIGHT_YEAR: u32 = 2026;
