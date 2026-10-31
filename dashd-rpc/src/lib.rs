//! Dash Core's RPC server for the Rust node (A1 §2.17; PLAN RP0).
//!
//! - [`http`]: the HTTP/1.1 server of `httpserver.cpp`, with libevent's framing: bind and allow
//!   rules, the work queue (and Dash's queue for external users) and its worker threads.
//! - [`httprpc`]: the JSON-RPC endpoint of `httprpc.cpp`: authentication (`.cookie`,
//!   `-rpcuser`/`-rpcpassword`, `-rpcauth`), whitelists, batches, `/wallet/<name>`.
//! - [`server`]: the command table of `rpc/server.cpp`: warmup, sub-command squashing, named
//!   arguments, and the arity and type checks `RPCHelpMan` makes.
//! - [`commands`]: the commands that need no chain.
//! - [`blockchain`]: `rpc/blockchain.cpp`'s commands over the node's chain
//!   ([`blockchain::NodeChain`]).
//! - [`mining`]: `rpc/mining.cpp`'s commands over the same chain and the node's block
//!   assembler ([`mining::Miner`]), with [`descriptor`] and [`key_io`] for their outputs.
//! - [`peers`]: `rpc/net.cpp`'s commands over the node's network ([`peers::NodeNet`]).
//! - [`dash`]: `rpc/node.cpp`'s Dash commands over the node's sync ([`dash::NodeSync`]).
//! - [`warnings`]: the node's warnings the chain and network commands report.
//!
//! JSON values are Core's `UniValue` ([`UniValue`]): numbers keep their text, so a reply writes
//! a number exactly as Core does.

#![forbid(unsafe_code)]

pub mod auth;
pub mod bech32;
pub mod blockchain;
pub mod commands;
pub mod core_write;
pub mod dash;
pub mod descriptor;
pub mod http;
pub mod httprpc;
pub mod key_io;
pub mod message;
pub mod mining;
pub mod net;
pub mod peers;
pub mod protocol;
pub mod request;
pub mod server;
pub mod util;
pub mod warnings;

#[cfg(test)]
mod testutil;

pub use dash_args::univalue::SettingsValue as UniValue;

/// Where an internal bug is reported (Core's `PACKAGE_BUGREPORT`).
pub const BUG_REPORT_URL: &str = "https://github.com/dashpay/rust-dashcore/issues";
