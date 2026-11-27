//! Core's `NetSigning` share threads around [`SigSharesRuntime`]: `sigsh-maint`,
//! `sigsh-dispat` and the `sigsh-work` pool.

use std::io;
use std::num::NonZero;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dash_llmq::sigenv::{QuorumLookup, RecSigReads, ShareRecovery, ShareVerifier, SigSharesNet};
use dash_llmq::signing::api::{RecoveredSigIntake, SigningFault};
use dash_llmq::sigshares::ticks::{CLEANING_SLEEP_MS, DISPATCHER_SLEEP_MS, worker_count};
use dashd_chainstate::chain::reply::no_wait_marked;

use super::facts::{SharesFacts, SharesOut};
use super::pool::{OnPanic, Pool, spawn};
use super::runtime::{SigSharesRuntime, Work};
use crate::signing::SigningInterrupt;

// Core: v24.0.0 src/llmq/net_signing.cpp:332, :389
const MAINT_SLEEP: Duration = Duration::from_millis(CLEANING_SLEEP_MS.unsigned_abs());
const DISPATCH_SLEEP: Duration = Duration::from_millis(DISPATCHER_SLEEP_MS.unsigned_abs());

/// What the thread loops read of Core's `workInterrupt`.
pub(super) trait Pause {
    fn is_set(&self) -> bool;
    fn sleep_for(&self, d: Duration) -> bool;
}

impl Pause for SigningInterrupt {
    fn is_set(&self) -> bool {
        SigningInterrupt::is_set(self)
    }

    fn sleep_for(&self, d: Duration) -> bool {
        SigningInterrupt::sleep_for(self, d)
    }
}

/// The running share threads, stopped by [`SigSharesThreads::join`] once the interrupt is set.
pub struct SigSharesThreads {
    interrupt: SigningInterrupt,
    maint: JoinHandle<()>,
    dispat: JoinHandle<()>,
    pool: Pool,
}

impl SigSharesThreads {
    /// Joins `sigsh-maint`, then `sigsh-dispat`, then drops the pool's queued tasks and joins
    /// its workers.
    pub fn join(self) {
        self.join_with(|_| {});
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:171-195 NetSigning::Stop
    pub(super) fn join_with(self, mut joined: impl FnMut(&'static str)) {
        assert!(self.interrupt.is_set(), "the share threads join after the interrupt");
        for (name, thread) in [("sigsh-maint", self.maint), ("sigsh-dispat", self.dispat)] {
            let _ = thread.join();
            joined(name);
        }
        self.pool.stop();
        joined("sigsh-work");
    }
}

impl<L, I, N, X, F, S> SigSharesRuntime<L, I, N, X, F, S>
where
    L: QuorumLookup,
    L::Quorum: Send + Sync,
    I: RecoveredSigIntake + RecSigReads<Error = SigningFault>,
    N: SigSharesNet,
    X: SharesOut,
    F: SharesFacts,
    S: ShareRecovery,
    Self: Send + Sync + 'static,
{
    /// Starts the `sigsh-work` pool, then `sigsh-maint`, then `sigsh-dispat`. Each verify task
    /// builds its own verifier with `verifier`, which must draw fresh weights from a strong RNG
    /// on every call. A panic on any share thread takes the abort path. On an `Err` no share
    /// thread is left running; a failed `sigsh-dispat` leaves the interrupt set.
    // Core: v24.0.0 src/llmq/net_signing.cpp:159-169 NetSigning::Start
    pub fn start<V: ShareVerifier>(
        self: &Arc<Self>,
        interrupt: SigningInterrupt,
        verifier: impl Fn() -> V + Send + Sync + 'static,
    ) -> io::Result<SigSharesThreads> {
        let rt = Arc::clone(self);
        let on_panic: OnPanic = Arc::new(move |name: &str| rt.panicked(name));
        let hw = thread::available_parallelism().map_or(0, NonZero::get);
        let pool = Pool::start(worker_count(hw), &on_panic)?;
        let (rt, pause) = (Arc::clone(self), interrupt.clone());
        let maint =
            match spawn("sigsh-maint".into(), on_panic.clone(), move || rt.maint_loop(&pause)) {
                Ok(maint) => maint,
                Err(e) => {
                    pool.stop();
                    return Err(e);
                }
            };
        let (rt, pause, queue) = (Arc::clone(self), interrupt.clone(), pool.queue());
        let verifier = Arc::new(verifier);
        let dispat = spawn("sigsh-dispat".into(), on_panic, move || {
            rt.dispatch_loop(&pause, |work| {
                let (rt, verifier) = (Arc::clone(&rt), Arc::clone(&verifier));
                queue.push(Box::new(move || match work {
                    Work::Sign(w) => rt.sign_task(w),
                    Work::Verify(batch, _in_flight) => rt.verify_task(&batch, &verifier()),
                }));
            });
        });
        match dispat {
            Ok(dispat) => Ok(SigSharesThreads {
                interrupt,
                maint,
                dispat,
                pool,
            }),
            Err(e) => {
                interrupt.trigger();
                let _ = maint.join();
                pool.stop();
                Err(e)
            }
        }
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:322-334 NetSigning::WorkThreadCleaning
    pub(super) fn maint_loop(&self, interrupt: &impl Pause) {
        debug_assert!(!no_wait_marked(), "sigsh-maint runs unmarked");
        while !interrupt.is_set() {
            self.maint_pass();
            interrupt.sleep_for(MAINT_SLEEP);
        }
    }

    // Core: v24.0.0 src/llmq/net_signing.cpp:336-391 NetSigning::WorkThreadDispatcher
    pub(super) fn dispatch_loop(
        &self,
        interrupt: &impl Pause,
        mut spawn: impl FnMut(Work<L::Quorum>),
    ) {
        debug_assert!(!no_wait_marked(), "sigsh-dispat runs unmarked");
        while !interrupt.is_set() {
            self.dispatcher_pass(|| interrupt.is_set(), &mut spawn);
            interrupt.sleep_for(DISPATCH_SLEEP);
        }
    }
}
