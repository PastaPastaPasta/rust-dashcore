//! SPV mode's seam: the binary's SPV client behind [`SpvHost`], which [`Node`](crate::Node)
//! starts in place of the full node's steps 6 to 12 and stops in its shutdown. dashd-node itself
//! has no SPV client.

use std::path::PathBuf;
use std::sync::Arc;

use dash_args::ArgsManager;
use dash_clock::MockClock;
use dash_params::ChainSetup;
use dashd_rpc::server::RpcServer;

use crate::InitFailure;
use crate::abort::NodeAbort;
use crate::scheduler::Scheduler;
use crate::shutdown::ShutdownState;
use crate::ui::Ui;

/// The SPV store's directory under the network data directory.
pub const SPV_DIR: &str = "spv";

/// The OS names of the threads the node itself runs in SPV mode once init is done, with
/// `-server` and `-rpcthreads=2`: the scheduler and the HTTP server's. The host's threads come on
/// top.
pub const SPV_FIXED_THREADS: &[&str] = &["d-scheduler", "http", "httpworker.0", "httpworker.1"];

/// The SPV client as the node runs it.
pub trait SpvHost: Send {
    /// Starts the client as step 12. An `Err` is init's error.
    fn start(&mut self, ctx: SpvStart<'_>) -> Result<(), InitFailure>;
    /// Adds the client's commands to SPV mode's table before the RPC server starts in warmup.
    /// They read what [`start`](Self::start) provides; until then the warmup answers them.
    fn register_rpc(&self, table: &Arc<RpcServer>);
    /// Stops the client's network intake, as `Interrupt` does; returns at once. The node calls
    /// it even after a failed or skipped [`start`](Self::start), and when
    /// [`register_rpc`](Self::register_rpc) never ran.
    fn interrupt(&self);
    /// Stops the client, drops every handle to it and closes its store, as `Shutdown` does. An
    /// `Err` is a failed flush of the store, which the node aborts on. The node calls it even
    /// after a failed or skipped [`start`](Self::start).
    fn shutdown(&mut self) -> Result<(), String>;
}

/// What [`SpvHost::start`] takes from the node.
pub struct SpvStart<'a> {
    pub args: &'a ArgsManager,
    pub chain: &'a ChainSetup,
    /// `<datadir>/<net>/spv`, which may not exist yet.
    pub spv_dir: PathBuf,
    pub abort: Arc<NodeAbort>,
    pub shutdown: Arc<ShutdownState>,
    pub clock: Arc<MockClock>,
    /// The node's scheduler, already running; it stops after [`SpvHost::shutdown`].
    pub scheduler: Scheduler,
    pub ui: Arc<Ui>,
}
