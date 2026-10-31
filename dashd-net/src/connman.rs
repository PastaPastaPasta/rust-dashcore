//! `CConnman` for the node (P2a §3.8): `Start` (binds, dial threads, the `DumpAddresses`
//! schedule), the thread half of `Stop`, the RPC back ends `addconnection`, `addnode … onetry`
//! and `setnetworkactive`, and `GetNodeStats`.
//!
//! SB6's [`NetCore`] owns everything stateful: the table, AddrMan, the dial state, anchors and
//! seeds. The host builds the core (with P2a-core's processor as its events), starts the
//! processor, and calls [`ConnMan::start`]; at shutdown it calls [`ConnMan::interrupt`] and
//! [`ConnMan::stop_threads`], stops the processor, then [`NetCore::shutdown`] (Core's
//! `StopNodes`). `stop_threads` returns only once the dial threads, the accept loops and the
//! dump schedule have ended (a dump already running completes first), so the final dump in
//! `shutdown` is the last write.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use dash_p2p::{
    ConnectionType, DialLoop, DialPolicy, NetCore, OpenError, OpenRequest, PeerHandle, Target,
};
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle as TaskHandle;

use crate::binds::{BindError, BindOptions, PACKAGE_NAME, accept_loop, init_binds};
use crate::driver::{self, OpenFilter};
use crate::stats::{NodeStats, node_stats};

/// Core's `DUMP_PEERS_INTERVAL`: 15 minutes.
pub const DUMP_PEERS_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// What [`ConnMan::start`] needs besides the core.
pub struct ConnManOptions {
    /// What to bind when the core's config listens (`-listen`).
    pub binds: BindOptions,
    /// Core's `m_active_masternode && !IsBlockchainSynced()`, read per accepted connection.
    pub refuse_unsynced: Arc<dyn Fn() -> bool + Send + Sync>,
    /// The `DumpAddresses` period ([`DUMP_PEERS_INTERVAL`]), counted from the end of the
    /// previous dump.
    pub dump_interval: Duration,
    /// The destinations the dial threads and the RPC opens may dial (`None`: any). A host
    /// leaves it unset; a test harness uses it to keep a node on loopback.
    pub open_filter: Option<OpenFilter>,
    /// The product name in the bind errors (Core's `PACKAGE_NAME`).
    pub package_name: &'static str,
}

impl Default for ConnManOptions {
    fn default() -> Self {
        Self {
            binds: BindOptions {
                bind_on_any: true,
                ..BindOptions::default()
            },
            refuse_unsynced: Arc::new(|| false),
            dump_interval: DUMP_PEERS_INTERVAL,
            open_filter: None,
            package_name: PACKAGE_NAME,
        }
    }
}

/// Why [`ConnMan::start`] failed.
#[derive(Debug)]
#[non_exhaustive]
pub enum StartError {
    /// `InitBinds` failed.
    Bind(BindError),
    /// A thread could not be spawned.
    Thread(std::io::Error),
}

impl core::fmt::Display for StartError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bind(e) => e.fmt(f),
            Self::Thread(e) => write!(f, "failed to start a network thread: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// The node's connection manager: the dial threads, the listeners and the dump schedule around
/// SB6's network core.
pub struct ConnMan {
    core: Arc<NetCore>,
    rt: Handle,
    listen_addrs: Vec<std::net::SocketAddr>,
    open_filter: Option<OpenFilter>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    tasks: Mutex<Vec<TaskHandle<()>>>,
    /// Set by [`ConnMan::interrupt`]: the accept loops and the dump schedule end at their next
    /// await.
    stop: watch::Sender<bool>,
}

impl ConnMan {
    /// Core's `CConnman::Start` after `Init`: `InitBinds` when listening, then the `dnsseed`,
    /// `addcon` and `opencon` threads in Core's order, then `DumpAddresses` every
    /// `opts.dump_interval`. SB6's `NetCore::new` has already queued `-seednode` and started the
    /// socket sweep; `NetCore::start_addresses` reads the anchors once the binds succeed.
    /// `policy` is P2a-core's `Shared::dial_policy()`.
    /// Call it from a plain thread, not a runtime worker: a failed start joins the threads it
    /// spawned.
    ///
    /// # Errors
    ///
    /// [`StartError::Bind`] as Core's `"Failed to listen on any port…"`; the core is
    /// interrupted and nothing keeps running. As in Core, `anchors.dat` is left as it was and
    /// `NetCore::shutdown` dumps nothing.
    // Core: net.cpp:3991-4120 CConnman::Start
    pub fn start(
        core: Arc<NetCore>,
        rt: Handle,
        policy: Arc<dyn DialPolicy>,
        opts: ConnManOptions,
    ) -> Result<Self, StartError> {
        let mut cm = Self {
            core,
            rt,
            listen_addrs: Vec::new(),
            open_filter: opts.open_filter.clone(),
            threads: Mutex::new(Vec::new()),
            tasks: Mutex::new(Vec::new()),
            stop: watch::Sender::new(false),
        };
        if cm.core.config().listen {
            let listeners = {
                let _guard = cm.rt.enter();
                init_binds(&cm.core, &opts.binds, opts.package_name)
            };
            let listeners = match listeners {
                Ok(l) => l,
                Err(e) => {
                    // `BindListenPort` has logged `e.reported`; the host shows it as Core's
                    // `BF_REPORT_ERROR` message box, then the `Display` text (BL-327 D3).
                    cm.core.interrupt();
                    return Err(StartError::Bind(e));
                }
            };
            cm.listen_addrs = listeners.iter().filter_map(|l| l.socket.local_addr().ok()).collect();
            // Core reads the anchors before `ThreadSocketHandler` accepts.
            cm.core.start_addresses();
            let mut tasks = cm.tasks.lock();
            for l in listeners {
                let refuse = Arc::clone(&opts.refuse_unsynced);
                let stop = cm.stop.subscribe();
                tasks.push(cm.rt.spawn(accept_loop(Arc::clone(&cm.core), l, refuse, stop)));
            }
        } else {
            cm.core.start_addresses();
        }
        // `NetCore::dial_tick` returns `Done` at once for the loops Core does not start
        // (`-dnsseed=0`, `-connect=0`), so every loop gets its thread.
        for lp in [DialLoop::DnsSeed, DialLoop::AddCon, DialLoop::OpenCon] {
            let spawned = driver::spawn(
                Arc::clone(&cm.core),
                lp,
                Arc::clone(&policy),
                cm.rt.clone(),
                cm.open_filter.clone(),
            );
            match spawned {
                Ok(h) => cm.threads.lock().push(h),
                Err(e) => {
                    cm.interrupt();
                    cm.stop_threads();
                    return Err(StartError::Thread(e));
                }
            }
        }
        let core = Arc::clone(&cm.core);
        let every = opts.dump_interval;
        let mut stop = cm.stop.subscribe();
        // Core: scheduler.cpp:102-111 Repeat (the next run is scheduled after `f()` returns)
        cm.tasks.lock().push(cm.rt.spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = stop.wait_for(|stopped| *stopped) => return,
                    () = tokio::time::sleep(every) => {}
                }
                let core = Arc::clone(&core);
                // The store blocks; Core runs this on its scheduler thread. Awaited, never
                // cancelled: `stop_threads` waits for it.
                let _ = tokio::task::spawn_blocking(move || core.dump()).await;
            }
        }));
        Ok(cm)
    }

    /// The addresses the listeners are bound to (a `-bind` port 0 shows the port the system
    /// chose).
    pub fn listen_addrs(&self) -> &[std::net::SocketAddr] {
        &self.listen_addrs
    }

    /// The network core.
    pub fn core(&self) -> &Arc<NetCore> {
        &self.core
    }

    /// Core's `CConnman::Interrupt`: wakes and stops the dial loops and new connections, and
    /// tells the accept loops and the dump schedule to end.
    pub fn interrupt(&self) {
        self.core.interrupt();
        self.stop.send_replace(true);
    }

    /// Core's `StopThreads`, after [`ConnMan::interrupt`]: joins the dial threads, then waits
    /// for the accept loops (their listeners closed) and the dump schedule, including a dump
    /// already running. The host must call it before `NetCore::shutdown` (dropping `ConnMan`
    /// only signals). Call it outside any async context, with the runtime multi-threaded.
    // Core: net.cpp:4168-4185 CConnman::StopThreads
    pub fn stop_threads(&self) {
        let threads = std::mem::take(&mut *self.threads.lock());
        for t in threads {
            if t.join().is_err() {
                tracing::error!(target: "net", "a network thread panicked");
            }
        }
        let tasks = std::mem::take(&mut *self.tasks.lock());
        for t in tasks {
            if let Err(e) = self.rt.block_on(t)
                && e.is_panic()
            {
                tracing::error!(target: "net", "a network task panicked");
            }
        }
    }

    /// Whether the open filter ([`ConnManOptions::open_filter`]) lets `req` through.
    fn allowed(&self, req: &OpenRequest) -> bool {
        driver::allowed(self.open_filter.as_ref(), req)
    }

    /// Core's `addnode <node> onetry`: one MANUAL connection attempt to `node` without a slot,
    /// waiting for the connect as Core's RPC thread does.
    ///
    /// # Errors
    ///
    /// Why no connection was added (Core's RPC reports nothing).
    // Core: rpc/net.cpp addnode ("onetry"): OpenNetworkConnection(CAddress(), false, {}, …, MANUAL)
    pub async fn open_one_try(&self, node: &str, use_v2: bool) -> Result<PeerHandle, OpenError> {
        let req = dest_request(node, ConnectionType::Manual, use_v2);
        if !self.allowed(&req) {
            return Err(OpenError::Connect(format!("{node}: refused by the open filter")));
        }
        self.core.open(req).await
    }

    /// [`ConnMan::open_one_try`] from a thread outside the runtime, as Core's RPC thread makes
    /// the attempt: it blocks until the attempt ends.
    ///
    /// # Errors
    ///
    /// As [`ConnMan::open_one_try`].
    pub fn open_one_try_blocking(&self, node: &str, use_v2: bool) -> Result<PeerHandle, OpenError> {
        self.rt.block_on(self.open_one_try(node, use_v2))
    }

    /// Core's `AddConnection` (the test-only `addconnection` RPC): `false` for inbound or manual
    /// types, when the type's limit is reached, or when no outbound slot is free; otherwise one
    /// attempt holding a slot, and `true` whatever its outcome.
    // Core: net.cpp:2030-2065 CConnman::AddConnection
    pub async fn add_connection(
        &self,
        address: &str,
        conn_type: ConnectionType,
        use_v2: bool,
    ) -> bool {
        let cfg = self.core.config();
        let max = match conn_type {
            ConnectionType::Inbound | ConnectionType::Manual => return false,
            ConnectionType::OutboundFullRelay => Some(cfg.max_outbound_full_relay),
            ConnectionType::BlockRelay => Some(cfg.max_outbound_block_relay),
            // No limit for ADDR_FETCH because -seednode has no limit either, and none for
            // FEELER connections since they're short-lived.
            ConnectionType::AddrFetch | ConnectionType::Feeler => None,
        };
        let existing = self.core.peers().iter().filter(|p| p.conn_type() == conn_type).count();
        if max.is_some_and(|m| existing >= m) {
            return false;
        }
        // Max total outbound connections already exist.
        let Some(grant) = self.core.try_grant(DialLoop::OpenCon) else {
            return false;
        };
        let mut req = dest_request(address, conn_type, use_v2);
        req.grant = Some(grant);
        if !self.allowed(&req) {
            return true;
        }
        if let Err(e) = self.core.open(req).await {
            tracing::debug!(target: "net", "addconnection to {address} not opened: {e:?}");
        }
        true
    }

    /// Core's `SetNetworkActive`: SB6's sweep drops every connection when inactive, and the dial
    /// loops stop opening new ones.
    // Core: net.cpp CConnman::SetNetworkActive
    pub fn set_network_active(&self, active: bool) {
        self.core.set_network_active(active);
    }

    /// Core's `GetNodeStats`.
    pub fn node_stats(&self) -> Vec<NodeStats> {
        node_stats(&self.core)
    }
}

/// An RPC-requested open of `dest` by name, without a grant, not counted as a failure.
fn dest_request(dest: &str, conn_type: ConnectionType, use_v2: bool) -> OpenRequest {
    OpenRequest {
        target: Target::Dest(dest.to_owned()),
        conn_type,
        count_failure: false,
        grant: None,
        use_v2,
        masternode: false,
        masternode_probe: false,
    }
}

impl Drop for ConnMan {
    fn drop(&mut self) {
        self.interrupt();
    }
}
