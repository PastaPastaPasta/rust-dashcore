//! `NetDKG`'s phase threads (DK1 plan §4 L2; DEC-1865 Q2): one OS thread per handler of an
//! active masternode, each running rounds until it is asked to stop.

use std::any::Any;
use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use dash_llmq::dkg::rng::{CoreRng, DkgRng};
use dash_llmq::dkg::session::{DkgFault, DkgStore, FinalVerifier, OperatorKey, RelayTopology};
use dash_llmq::dkg::sim::{InvalidRate, SimError, SimErrorRates};
use dash_types::Numeric;
use getrandom::SysRng;
use rand_core::UnwrapErr;
use tracing::{debug, info};

use super::DkgSessions;
use super::debug::DebugManager;
use super::handler::Handler;
use super::round::handle_round;
use super::seams::{
    ActiveMn, BatchProcessor, ConnFactsSource, DkgChain, DkgConnections, MembersAccess,
    MineableSink, PendingQueues, PhaseClock,
};
use super::types::SessionKey;
use super::wait::Abort;

/// The session's environment objects a round builds per call: the relay topology, dkgdb, the
/// final commitments' `Verify`, the operator key and the session's randomness.
pub trait DkgPorts: Send + Sync {
    /// `GetQuorumRelayMembers` over CH7b's facade (access #2).
    fn topology(&self) -> Box<dyn RelayTopology + '_>;
    /// dkgdb: DK1-db's `&DkgDb` (accesses #3 and #4).
    fn store(&self) -> Box<dyn DkgStore + '_>;
    /// `CFinalCommitment::Verify(…, checkSigs = true)` (accesses #5 and #6).
    fn verifier(&self) -> Box<dyn FinalVerifier + '_>;
    /// The active masternode's operator key.
    fn operator_key(&self) -> &dyn OperatorKey;
    /// The randomness of one session of handler `key`: Core's draws over the OS RNG.
    fn session_rng(&self, key: SessionKey) -> Box<dyn DkgRng + Send> {
        let _ = key;
        Box::new(CoreRng(UnwrapErr(SysRng)))
    }
}

/// Where a phase thread reports a fault Core's `TraceThread` would end the process on: a
/// [`DkgFault`] or any other panic on the thread (only [`Abort`] is caught). It runs on the
/// faulting phase thread, after Core's `PrintExceptionContinue` banner.
pub trait DkgFailStop: Send + Sync {
    /// Stops the node; `why` describes the fault. It must not wait for the phase threads to
    /// end; [`DkgRuntime::stop`] called from a phase thread only interrupts them.
    fn fail_stop(&self, key: SessionKey, why: &str);
}

/// The simulated error rates `quorum dkgsimerror` sets (`simDkgErrorMap`). Each session call
/// reads a snapshot.
#[derive(Debug, Default)]
pub struct SimRates(Mutex<SimErrorRates>);

impl SimRates {
    fn lock(&self) -> MutexGuard<'_, SimErrorRates> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The rates now.
    pub fn snapshot(&self) -> SimErrorRates {
        *self.lock()
    }

    /// `SetSimulatedDKGErrorRate(kind, rate / 100)`.
    ///
    /// # Errors
    ///
    /// The RPC's refusal outside `0..=100`.
    pub fn set(&self, kind: SimError, rate: i32) -> Result<(), InvalidRate> {
        self.lock().set(kind, rate)
    }
}

/// Everything a phase thread reaches outside its handler.
#[derive(Clone)]
pub struct RoundSeams {
    /// `m_chainman`.
    pub chain: Arc<dyn DkgChain>,
    /// Access #1, through CH7b's facade.
    pub members: Arc<dyn MembersAccess>,
    /// `m_mn_activeman`.
    pub active: Arc<dyn ActiveMn>,
    /// The session's per-call environment objects.
    pub ports: Arc<dyn DkgPorts>,
    /// ND1's pending messages.
    pub queues: Arc<dyn PendingQueues>,
    /// ND1's `ProcessPendingMessageBatch`.
    pub batches: Arc<dyn BatchProcessor>,
    /// `EnsureQuorumConnections` and `AddQuorumProbeConnections`.
    pub connections: Arc<dyn DkgConnections>,
    /// The connection snapshot `VerifyAndComplain` reads.
    pub conn_facts: Arc<dyn ConnFactsSource>,
    /// `AddMineableCommitment` and the relay of its inv.
    pub mineable: Arc<dyn MineableSink>,
    /// The waits' clock.
    pub clock: Arc<dyn PhaseClock>,
    /// `simDkgErrorMap`.
    pub sim: Arc<SimRates>,
    /// `m_dkgdbgman`.
    pub debug: Arc<DebugManager>,
    /// Where a fault goes.
    pub fail_stop: Arc<dyn DkgFailStop>,
    /// `-watchquorums`.
    pub watch: bool,
}

/// `Start` was called while the phase threads run.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    /// "Tried to start PhaseHandlerThreads again."
    #[error("Tried to start PhaseHandlerThreads again.")]
    AlreadyStarted,
    /// A thread could not be spawned.
    #[error("failed to spawn a DKG phase thread: {0}")]
    Spawn(String),
}

/// `NetDKG`'s `Start`, `Interrupt` and `Stop` over [`DkgSessions`]: on an active masternode,
/// one thread named `llmq-<type>-<index>` per handler; none for an observer.
pub struct DkgRuntime {
    sessions: Arc<DkgSessions>,
    seams: Arc<RoundSeams>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl DkgRuntime {
    /// A runtime over `sessions` that has started no thread.
    ///
    /// # Panics
    ///
    /// When `seams.debug` was built without a type a handler runs: its sessions would have no
    /// member entries.
    pub fn new(sessions: Arc<DkgSessions>, seams: RoundSeams) -> Self {
        for handler in sessions.handlers() {
            let t = handler.key().llmq_type;
            assert!(seams.debug.knows(t), "no debug status size for {t}");
        }
        Self {
            sessions,
            seams: Arc::new(seams),
            threads: Mutex::new(Vec::new()),
        }
    }

    /// The handlers and their lookups.
    pub fn sessions(&self) -> &Arc<DkgSessions> {
        &self.sessions
    }

    fn lock_threads(&self) -> MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.threads.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Starts one phase thread per handler under Core's `TraceThread`; nothing for an observer.
    /// Each thread inherits the caller's tracing dispatcher.
    ///
    /// # Errors
    ///
    /// [`StartError::AlreadyStarted`] while threads run; [`StartError::Spawn`] when the OS
    /// refuses a thread.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:750-764 NetDKG::Start
    pub fn start(&self) -> Result<(), StartError> {
        if !self.sessions.is_active() {
            return Ok(());
        }
        let mut threads = self.lock_threads();
        if !threads.is_empty() {
            return Err(StartError::AlreadyStarted);
        }
        let dispatch = tracing::dispatcher::get_default(Clone::clone);
        for handler in self.sessions.handlers() {
            let key = handler.key();
            let name = format!("llmq-{}-{}", key.llmq_type.to_base(), key.quorum_index);
            let (seams, handler, dispatch) =
                (self.seams.clone(), handler.clone(), dispatch.clone());
            let spawned = std::thread::Builder::new().name(format!("d-{name}")).spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    trace_thread(
                        &name,
                        || phase_thread(&seams, &handler),
                        |why| {
                            seams.fail_stop.fail_stop(key, why);
                        },
                    );
                });
            });
            threads.push(spawned.map_err(|e| StartError::Spawn(e.to_string()))?);
        }
        Ok(())
    }

    /// `RequestStop()` on every handler: each thread ends at its next wait check.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:775-783 NetDKG::Interrupt
    pub fn interrupt(&self) {
        if !self.sessions.is_active() {
            return;
        }
        for handler in self.sessions.handlers() {
            handler.request_stop();
        }
    }

    /// [`Self::interrupt`], then joins every thread. On a phase thread it only interrupts,
    /// and a later call from another thread joins them all.
    // Core: v24.0.0 src/llmq/net_dkg.cpp:766-773 NetDKG::Stop
    pub fn stop(&self) {
        self.interrupt();
        let me = std::thread::current().id();
        let threads = {
            let mut threads = self.lock_threads();
            if threads.iter().any(|t| t.thread().id() == me) {
                return;
            }
            std::mem::take(&mut *threads)
        };
        for thread in threads {
            let _ = thread.join();
        }
    }
}

fn describe(payload: &(dyn Any + Send)) -> String {
    if let Some(fault) = payload.downcast_ref::<DkgFault>() {
        format!("{fault:?}")
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

// Core: v24.0.0 src/util/thread.cpp:16-27 util::TraceThread, src/util/system.cpp:783-788 PrintExceptionContinue
fn trace_thread(name: &str, f: impl FnOnce() -> Result<(), String>, fail_stop: impl FnOnce(&str)) {
    info!("{name} thread start");
    match f() {
        Ok(()) => info!("{name} thread exit"),
        Err(what) => {
            let message = format!("\"{name}\" raised an exception\n{what}");
            info!("\n\n************************\n{message}");
            let _ = writeln!(std::io::stderr(), "\n\n************************\n{message}");
            fail_stop(&what);
        }
    }
}

// Core: v24.0.0 src/llmq/net_dkg.cpp:785-798 NetDKG::PhaseHandlerThread
fn phase_thread(seams: &RoundSeams, handler: &Handler) -> Result<(), String> {
    let key = handler.key();
    let (t, qi) = (key.llmq_type, key.quorum_index);
    let run = catch_unwind(AssertUnwindSafe(|| {
        while !handler.stop_requested() {
            debug!(
                target: "llmq-dkg",
                "NetDKG::PhaseHandlerThread -- {t} qi[{qi}] - starting HandleDKGRound"
            );
            if let Err(Abort) = handle_round(seams, handler) {
                seams.debug.mark_aborted(t, qi);
                debug!(
                    target: "llmq-dkg",
                    "NetDKG::PhaseHandlerThread -- {t} qi[{qi}] - aborted current DKG session"
                );
            }
        }
    }));
    run.map_err(|payload| describe(payload.as_ref()))
}

#[cfg(test)]
mod tests;
