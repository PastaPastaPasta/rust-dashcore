//! `NetInstantSend`'s worker (`isman`, PLAN-LK1b §6 S3): each tick verifies at most 32 received
//! locks and retries the signer's queued transactions, and the loop sleeps 100 ms after a tick
//! that left no work.

use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use rand_core::CryptoRng;

use super::batch::IsPassView;
use super::manager::PendingState;
use super::net::{IsNetError, NetInstantSend};

/// The worker's idle sleep. Core: `instantsend/net_instantsend.cpp:37`.
pub const WORK_THREAD_SLEEP_INTERVAL: Duration = Duration::from_millis(100);

/// Core's `CThreadInterrupt` as the worker reads it.
pub trait WorkInterrupt: Send + Sync {
    /// `!workInterrupt`'s negation: the worker was asked to stop.
    fn interrupted(&self) -> bool;

    /// `sleep_for(d)`: waits `d` or until interrupted; false when interrupted.
    fn sleep_for(&self, d: Duration) -> bool;
}

/// Core's `CThreadInterrupt` (`util/threadinterrupt.cpp`): a flag and a condition variable.
#[derive(Debug, Default)]
pub struct ThreadInterrupt {
    flag: Mutex<bool>,
    cond: Condvar,
}

impl ThreadInterrupt {
    pub fn new() -> Self {
        Self::default()
    }

    /// `operator()()`: interrupt every sleeper and every later sleep.
    pub fn interrupt(&self) {
        *self.flag.lock() = true;
        self.cond.notify_all();
    }

    /// `reset()`.
    pub fn reset(&self) {
        *self.flag.lock() = false;
    }
}

impl WorkInterrupt for ThreadInterrupt {
    fn interrupted(&self) -> bool {
        *self.flag.lock()
    }

    fn sleep_for(&self, d: Duration) -> bool {
        let mut flag = self.flag.lock();
        if !*flag {
            let _ = self.cond.wait_while_for(&mut flag, |interrupted| !*interrupted, d);
        }
        !*flag
    }
}

impl NetInstantSend {
    /// One pass of `WorkThreadMain`'s loop body: nothing while InstantSend is disabled; else
    /// fetch at most 32 received locks and verify them ([`Self::process_pending_is_locks`]), then,
    /// with a signer, hand it the transactions to retry. Returns whether locks were left queued.
    ///
    /// Core calls `ProcessPendingRetryLockTxs` with an empty list too, where it does nothing
    /// (`instantsend/signing.cpp:150-187`); the empty list is not handed on.
    ///
    /// # Errors
    ///
    /// As [`Self::process_pending_is_locks`]; the fetched locks are dropped.
    // Core: instantsend/net_instantsend.cpp:428-439
    pub fn worker_tick<Q: IsPassView>(
        &self,
        views: &Q,
        rng: &mut impl CryptoRng,
    ) -> Result<bool, IsNetError<Q::Error>> {
        if !self.manager.is_instant_send_enabled() {
            return Ok(false);
        }
        let PendingState {
            pending_work,
            pending_is,
        } = self.manager.fetch_pending_locks();
        if !pending_is.is_empty() {
            self.process_pending_is_locks(views, pending_is, rng)?;
        }
        if let Some(signer) = &self.signer {
            let txs = self.manager.prepare_tx_to_retry();
            if !txs.is_empty() {
                signer.retry(txs);
            }
        }
        Ok(pending_work)
    }

    /// `WorkThreadMain`: ticks until `interrupt`, sleeping [`WORK_THREAD_SLEEP_INTERVAL`] after a
    /// tick that left no work and not after one that did.
    ///
    /// # Errors
    ///
    /// A tick's error, which ends the loop; the caller stops the node.
    // Core: instantsend/net_instantsend.cpp:425-445
    pub fn work_thread_main<Q: IsPassView>(
        &self,
        views: &Q,
        interrupt: &dyn WorkInterrupt,
        rng: &mut impl CryptoRng,
    ) -> Result<(), IsNetError<Q::Error>> {
        while !interrupt.interrupted() {
            let more_work = self.worker_tick(views, &mut *rng)?;
            if !more_work && !interrupt.sleep_for(WORK_THREAD_SLEEP_INTERVAL) {
                return Ok(());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "worker_tests.rs"]
mod tests;
