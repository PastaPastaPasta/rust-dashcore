//! Regtest harness for the Rust node: start `rdashd` beside Dash Core's `dashd`, wire them into
//! one network and drive their clocks (US-DEV-09, REL-5.7).
//!
#![cfg_attr(
    feature = "harness",
    doc = r#"
It wraps dash-spv's `test-utils` rather than moving them: [`DashCoreNode`],
[`DashdTestContext`] and the `contrib/setup-dashd.py` environment (`DASHD_PATH`,
`DASHD_TEST_DATA`) are dash-spv's, re-exported here. On top of them:
- [`RustNode`] runs `rdashd` in its own data directory, with mock time and log capture, and
  stops it the way Core stops (SIGTERM, then `Shutdown: done`).
- [`MixedNetwork`] runs N `dashd` and M `rdashd` regtest nodes with ports from the shared
  per-process allocator, peer edges between them, and the `DASHD_TEST_RETAIN_DIR` /
  `DASHD_TEST_LOG` conventions.
- [`MockTime`] gives every node the same `-mocktime` and moves them together.
- [`NodeImpl`] is the `DASHD_IMPL=rdashd` switch: dash-spv's harness then starts rdashd
  where it would start dashd.

rdashd grows feature by feature. What it does not support yet it refuses at start, naming
the task that adds it, and the harness reports that as [`StartError::Refused`]. Tests that
need such a feature are `#[ignore]`d with that task's name; `README.md` keeps the matrix.
"#
)]
#![cfg_attr(
    not(feature = "harness"),
    doc = r#"
This build leaves out the default `harness` feature, and with it the regtest harness
(`RustNode`, `MixedNetwork`, `MockTime`) and its dash-spv/tokio dependencies.
"#
)]
//!
//! [`keys`] derives the fixtures' basic-scheme BLS keys (the `testkit-keys` CLI prints them). It
//! is the one part built without the default `harness` feature, which gates the harness and its
//! dash-spv/tokio dependencies: `default-features = false` gives just the keys.
//!
//! [`mixnet`] is O12b's mixed-network contract (observation-log records and role plans). It is
//! pure and also builds without `harness`.
//!
//! This is a test-only crate: depend on it only as a dev-dependency.

pub mod keys;
pub mod mixnet;

#[cfg(all(feature = "crash", target_os = "linux"))]
pub mod crash;
#[cfg(feature = "harness")]
mod dir;
/// CH5-5's reopen oracle rows (§6.3.2 step 1), over C0's inspector.
#[cfg(all(feature = "crash", target_os = "linux"))]
pub mod livecopy;
#[cfg(feature = "harness")]
mod mocktime;
#[cfg(feature = "harness")]
mod network;
#[cfg(feature = "harness")]
mod rpc;
#[cfg(feature = "harness")]
mod rust_node;

#[cfg(feature = "harness")]
pub use dash_spv::test_utils::{
    DashCoreConfig, DashCoreNode, DashdTestContext, NodeImpl, TestChain, find_available_port,
    retain_test_dir, retain_test_dir_now,
};
#[cfg(feature = "harness")]
pub use dir::TestDir;
#[cfg(feature = "harness")]
pub use mocktime::MockTime;
#[cfg(feature = "harness")]
pub use network::{MixedNetwork, MixedNetworkBuilder, NetworkStartError, NodeId};
#[cfg(feature = "harness")]
pub use rpc::{NodeRpc, PeerInfo, RpcEndpoint, RpcError};
#[cfg(feature = "harness")]
pub use rust_node::{RustNode, RustNodeConfig, StartError, StopError, Stopped};

#[cfg(feature = "harness")]
use std::net::SocketAddr;
#[cfg(feature = "harness")]
use std::time::Duration;

/// How long a node may take to start or stop. Generous: on a loaded box a regtest node has taken
/// minutes to start.
#[cfg(feature = "harness")]
pub const PATIENCE: Duration = Duration::from_secs(300);

/// `127.0.0.1:<port>`: every test node binds and connects on loopback.
#[cfg(feature = "harness")]
pub(crate) fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}
