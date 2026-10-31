//! Regtest harness for the Rust node: start `rdashd` beside Dash Core's `dashd`, wire them into
//! one network and drive their clocks (US-DEV-09, REL-5.7).
//!
//! It wraps dash-spv's `test-utils` rather than moving them: [`DashCoreNode`],
//! [`DashdTestContext`] and the `contrib/setup-dashd.py` environment (`DASHD_PATH`,
//! `DASHD_TEST_DATA`) are dash-spv's, re-exported here. On top of them:
//! - [`RustNode`] runs `rdashd` in its own data directory, with mock time and log capture, and
//!   stops it the way Core stops (SIGTERM, then `Shutdown: done`).
//! - [`MixedNetwork`] runs N `dashd` and M `rdashd` regtest nodes with ports from the shared
//!   per-process allocator, peer edges between them, and the `DASHD_TEST_RETAIN_DIR` /
//!   `DASHD_TEST_LOG` conventions.
//! - [`MockTime`] gives every node the same `-mocktime` and moves them together.
//! - [`NodeImpl`] is the `DASHD_IMPL=rdashd` switch: dash-spv's harness then starts rdashd
//!   where it would start dashd.
//!
//! rdashd grows feature by feature. What it does not support yet it refuses at start, naming
//! the task that adds it, and the harness reports that as [`StartError::Refused`]. Tests that
//! need such a feature are `#[ignore]`d with that task's name; `README.md` keeps the matrix.
//!
//! This is a test-only crate: depend on it only as a dev-dependency.

#[cfg(all(feature = "crash", target_os = "linux"))]
pub mod crash;
mod dir;
mod mocktime;
mod network;
mod rpc;
mod rust_node;

pub use dash_spv::test_utils::{
    DashCoreConfig, DashCoreNode, DashdTestContext, NodeImpl, TestChain, find_available_port,
    retain_test_dir, retain_test_dir_now,
};
pub use dir::TestDir;
pub use mocktime::MockTime;
pub use network::{MixedNetwork, MixedNetworkBuilder, NetworkStartError, NodeId};
pub use rpc::{NodeRpc, PeerInfo, RpcEndpoint, RpcError};
pub use rust_node::{RustNode, RustNodeConfig, StartError, StopError, Stopped};

use std::net::SocketAddr;
use std::time::Duration;

/// How long a node may take to start or stop. Generous: on a loaded box a regtest node has taken
/// minutes to start.
pub const PATIENCE: Duration = Duration::from_secs(300);

/// `127.0.0.1:<port>`: every test node binds and connects on loopback.
pub(crate) fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}
