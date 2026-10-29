//! The drivers of SB6's sans-io dial loops (P2a §3.8.1–3.8.5): one OS thread per loop, as Core
//! runs `ThreadOpenConnections`, `ThreadOpenAddedConnections` and `ThreadDNSAddressSeed`.
//!
//! A driver ticks the loop through [`NetCore::dial_tick`] and performs each [`DialPlan`] with a
//! [`DialExec`]. [`RuntimeExec`] performs them on the host's tokio runtime, blocking the loop's
//! thread as Core's blocking connect, sleep, semaphore wait and `getaddrinfo` do. The dial `sim`
//! ([`crate::sim`]) performs the same plans with in-memory connections and a virtual clock.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use dash_p2p::{DialLoop, DialPlan, DialPolicy, NetCore, OpenRequest};
use tokio::runtime::Handle;

/// A boundary on the destinations a node may dial: `false` drops the open, releasing its grant.
pub type OpenFilter = Arc<dyn Fn(&OpenRequest) -> bool + Send + Sync>;

/// Whether `filter` lets `req` through (logging a refusal).
pub(crate) fn allowed(filter: Option<&OpenFilter>, req: &OpenRequest) -> bool {
    let ok = filter.is_none_or(|f| f(req));
    if !ok {
        tracing::debug!(target: "net", "{} connection to {:?} refused by the open filter", req.conn_type.as_str(), req.target);
    }
    ok
}

/// What a driver does with each [`DialPlan`].
pub trait DialExec {
    /// [`DialPlan::Open`]: Core's `OpenNetworkConnection` (the outcome is not reported back to
    /// the loop, as in Core).
    fn open(&mut self, core: &NetCore, req: OpenRequest);

    /// [`DialPlan::Sleep`] (`Some`: the full duration unless interrupted) or
    /// [`DialPlan::WaitGrant`] (`None`); `false` once the core is interrupted.
    fn wait(&mut self, core: &NetCore, lp: DialLoop, max: Option<Duration>) -> bool;

    /// [`DialPlan::Resolve`]: Core's per-seed body of `ThreadDNSAddressSeed`.
    fn resolve(&mut self, core: &NetCore, seed: &str, max: usize);
}

/// Advances `lp` by one plan. `false` when the loop has finished or was interrupted.
pub fn step(
    core: &NetCore,
    lp: DialLoop,
    policy: &dyn DialPolicy,
    exec: &mut dyn DialExec,
) -> bool {
    match core.dial_tick(lp, policy) {
        DialPlan::Open(req) => {
            exec.open(core, req);
            true
        }
        DialPlan::Sleep(d) => exec.wait(core, lp, Some(d)),
        DialPlan::WaitGrant => exec.wait(core, lp, None),
        DialPlan::Resolve {
            seed,
            max,
        } => {
            exec.resolve(core, &seed, max);
            true
        }
        DialPlan::Done => false,
    }
}

/// Runs `lp` until it finishes or the core is interrupted.
pub fn run(core: &NetCore, lp: DialLoop, policy: &dyn DialPolicy, exec: &mut dyn DialExec) {
    while step(core, lp, policy, exec) {}
}

/// Performs dial plans on the host's runtime, blocking the calling (loop) thread.
#[derive(Clone)]
pub struct RuntimeExec {
    rt: Handle,
    filter: Option<OpenFilter>,
}

impl RuntimeExec {
    /// An executor on `rt` that opens what `filter` lets through (`None`: everything). The
    /// caller must not be a runtime worker thread.
    pub fn new(rt: Handle, filter: Option<OpenFilter>) -> Self {
        Self {
            rt,
            filter,
        }
    }
}

impl DialExec for RuntimeExec {
    fn open(&mut self, core: &NetCore, req: OpenRequest) {
        if !allowed(self.filter.as_ref(), &req) {
            return;
        }
        let conn_type = req.conn_type;
        if let Err(e) = self.rt.block_on(core.open(req)) {
            tracing::trace!(target: "net", "{} connection not opened: {e:?}", conn_type.as_str());
        }
    }

    // Core: util/threadinterrupt.cpp:32-35 CThreadInterrupt::sleep_for (SB6's `dial_wait`
    // keeps the full duration unless interrupted)
    fn wait(&mut self, core: &NetCore, lp: DialLoop, max: Option<Duration>) -> bool {
        self.rt.block_on(core.dial_wait(lp, max))
    }

    fn resolve(&mut self, core: &NetCore, seed: &str, max: usize) {
        self.rt.block_on(core.resolve_seed(seed, max));
    }
}

/// Core's thread name for a loop (`util::TraceThread`).
pub const fn thread_name(lp: DialLoop) -> &'static str {
    match lp {
        DialLoop::OpenCon => "opencon",
        DialLoop::AddCon => "addcon",
        DialLoop::DnsSeed => "dnsseed",
        _ => "dial",
    }
}

/// Spawns `lp`'s OS thread, driving it with a [`RuntimeExec`] on `rt` behind `filter`.
///
/// # Errors
///
/// When the OS refuses the thread.
pub fn spawn(
    core: Arc<NetCore>,
    lp: DialLoop,
    policy: Arc<dyn DialPolicy>,
    rt: Handle,
    filter: Option<OpenFilter>,
) -> std::io::Result<JoinHandle<()>> {
    let name = thread_name(lp);
    std::thread::Builder::new().name(name.to_owned()).spawn(move || {
        tracing::info!(target: "net", "{name} thread start");
        run(&core, lp, &*policy, &mut RuntimeExec::new(rt, filter));
        tracing::info!(target: "net", "{name} thread exit");
    })
}
