//! The [`MempoolHandle`] on the [`Mempool`], with stage B's production seams: [`AcceptPool`] (the
//! pool write guard) and [`AcceptSignals`] (the pool's clone of V's queue).
//!
//! Admissions are serial, as Core's `ProcessTransaction` runs under `cs_main`: one worker thread
//! takes requests from a FIFO in arrival order and submits each as one class-M V turn, then waits
//! until V has run (or dropped) that turn before it takes the next. `prioritise` and
//! `post_prioritise` join the same FIFO and run on the worker between turns, under the pool write
//! lock alone; only `prioritise`'s caller waits for it.
//!
//! The worker exists from [`Mempool::attach_chain`], which the node calls once V's evo state is
//! attached, to [`Mempool::detach_chain`], which it calls once V has stopped; outside that span
//! an admission replies [`WaitError::Disconnected`] without reaching V.
//!
//! Lock order (§3: V turn → pool write → IS → coins): the worker holds no lock while it waits for
//! V, and takes the pool lock only for `prioritise`, never across a wait; V never waits for the
//! worker. The IS path (`remove_conflicts_for_lock`) and the relay reads take the pool lock alone,
//! off V. A turn takes the pool write guard under V, as Core takes `m_pool.cs` under `cs_main`.

use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Weak, mpsc};

use dash_consensus::{Amount, TxRef};
use dash_primitives::TxHash;
use dashd_chainstate::chain::mempool_turn::{LockOrderGuard, LockRank};
use dashd_chainstate::chain::{
    ActorReply, ChainCommand, ChainHandle, MempoolCommand, NodeJoinHandle, Queued, ReplySender,
    ValidationSignals, WaitError, reply, spawn_thread,
};
use parking_lot::Mutex;

use crate::accept::{
    AcceptArgs, AcceptPool, AcceptSignals, StatelessRecord, accept_to_memory_pool,
    process_new_package,
};
use crate::api::{MempoolHandle, RelayRead};
use crate::events;
use crate::pool::{Mempool, PendingEvent, PoolState};
use crate::sink::sampled;
use crate::types::{
    MempoolAcceptResult, Package, PackageMempoolAcceptResult, PackageValidationResult,
};

/// One request in the admission FIFO.
pub(crate) enum Request {
    Accept {
        tx: TxRef,
        bypass_limits: bool,
        test_accept: bool,
        reply: ReplySender<MempoolAcceptResult>,
    },
    Package {
        txns: Package,
        test_accept: bool,
        reply: ReplySender<PackageMempoolAcceptResult>,
    },
    Prioritise {
        txid: TxHash,
        fee_delta: Amount,
        done: ReplySender<()>,
    },
    /// A test's fault: the worker panics as it takes this.
    #[cfg(feature = "test-support")]
    Panic,
}

/// The admission worker: none until [`Mempool::attach_chain`], then its FIFO and thread until
/// [`Mempool::detach_chain`] takes both for good.
#[derive(Default)]
pub(crate) struct Admissions(Mutex<Attachment>);

#[derive(Default)]
enum Attachment {
    #[default]
    Unattached,
    Attached {
        queue: mpsc::Sender<Request>,
        worker: NodeJoinHandle<()>,
    },
    Detached,
}

/// Stage B's events, queued onto the pool's queue as they occur, under the guard; listeners run
/// later on the signals thread, never under the guard.
struct Enqueue<'a>(&'a ValidationSignals);

impl AcceptSignals for Enqueue<'_> {
    // Core: txmempool.cpp:710-717 at v24.0.0
    fn removed(&mut self, event: PendingEvent) {
        self.0.enqueue(events::queued(event));
    }

    // Core: validation.cpp:1423, :1455 at v24.0.0
    fn added(&mut self, tx: TxRef, accept_time: i64, sequence: u64) {
        self.0.enqueue(Queued::TransactionAddedToMempool {
            tx,
            accept_time,
            sequence,
        });
    }

    // Core: validation.cpp:902-907 at v24.0.0
    fn double_spend_attempt(&mut self, tx: TxRef, conflict: TxRef) {
        self.0.enqueue(Queued::NotifyInstantSendDoubleSpendAttempt {
            tx,
            conflict,
        });
    }

    fn flushed(&mut self, event: Queued) {
        self.0.enqueue(event);
    }
}

impl AcceptPool for Mempool {
    /// `LOCK(m_pool.cs)` for one `AcceptSingleTransaction`; the usage telemetry is published
    /// before the guard is released.
    // Core: validation.cpp:1432 at v24.0.0
    fn with_write(&self, f: &mut dyn FnMut(&mut PoolState, &mut dyn AcceptSignals)) {
        let _order = LockOrderGuard::enter(LockRank::Mempool);
        let mut state = self.write();
        f(&mut state, &mut Enqueue(self.signals()));
        events::enqueue(&mut state, self.signals());
        self.publish_usage(&state);
    }
}

impl Mempool {
    /// Starts the admission worker, which submits this pool's admissions to `chain` (V) one
    /// turn at a time. Call it only once V's evo state is attached (DEC-1867): a turn's readiness
    /// refusals are `Fatal`. Until then an admission replies [`WaitError::Disconnected`] and never
    /// reaches V.
    /// Core v24.0.0 `src/validation.cpp:4920-4932`, `ProcessTransaction` under `cs_main`.
    ///
    /// # Errors
    ///
    /// The worker thread did not start, or this pool already had one.
    pub fn attach_chain(self: &Arc<Self>, chain: ChainHandle) -> std::io::Result<()> {
        let mut attachment = self.admissions.0.lock();
        if !matches!(*attachment, Attachment::Unattached) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "the mempool's admission worker is already attached",
            ));
        }
        let (queue, requests) = mpsc::channel();
        let pool = Arc::downgrade(self);
        let worker =
            spawn_thread("mempool-accept", move || serve_logged(&pool, &chain, &requests))?;
        *attachment = Attachment::Attached {
            queue,
            worker,
        };
        Ok(())
    }

    /// Stops the admission worker for good, once V has stopped: the FIFO closes, the worker
    /// hands V's stopped handle what it already took (each such admission replies
    /// [`WaitError::Disconnected`]), ends, dropping its chain handle, and is joined. Later
    /// admissions reply `Disconnected`; a pool never attached has nothing to stop.
    /// Core v24.0.0 `src/init.cpp:460`, `node.mempool.reset()` after the chainstate stops.
    ///
    /// # Errors
    ///
    /// The worker's panic, which it logged as it unwound, or a caller marked no-wait (LO-9),
    /// which cannot join it.
    pub fn detach_chain(&self) -> Result<(), String> {
        let attachment = std::mem::replace(&mut *self.admissions.0.lock(), Attachment::Detached);
        let Attachment::Attached {
            queue,
            worker,
        } = attachment
        else {
            return Ok(());
        };
        drop(queue);
        match worker.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(panic)) => {
                Err(format!("the mempool admission worker panicked: {}", panic_message(&*panic)))
            }
            Err(e) => Err(format!("the mempool admission worker was not joined: {e}")),
        }
    }

    /// A test's fault: the worker panics at this request, behind every earlier one.
    #[cfg(feature = "test-support")]
    pub fn panic_admission_worker(&self) {
        self.request(Request::Panic);
    }

    /// Queues `request` behind every earlier one. Without a worker an admission is dropped
    /// (`Disconnected`) and a prioritisation applies at once: no turn can precede it.
    fn request(&self, request: Request) {
        let unsent = match &*self.admissions.0.lock() {
            Attachment::Attached {
                queue,
                ..
            } => queue.send(request).err().map(|e| e.0),
            Attachment::Unattached | Attachment::Detached => Some(request),
        };
        if let Some(Request::Prioritise {
            txid,
            fee_delta,
            done,
        }) = unsent
        {
            self.prioritise_now(&txid, fee_delta);
            done.send(());
        }
    }

    fn queue_prioritise(&self, txid: &TxHash, fee_delta: Amount) -> ActorReply<()> {
        let (done, applied) = reply();
        self.request(Request::Prioritise {
            txid: *txid,
            fee_delta,
            done,
        });
        applied
    }

    fn admit(
        &self,
        tx: TxRef,
        bypass_limits: bool,
        test_accept: bool,
    ) -> ActorReply<MempoolAcceptResult> {
        let (send, reply) = reply();
        self.request(Request::Accept {
            tx,
            bypass_limits,
            test_accept,
            reply: send,
        });
        reply
    }

    // Core: txmempool.cpp:1878-1903 at v24.0.0
    fn prioritise_now(&self, txid: &TxHash, fee_delta: Amount) {
        {
            let _order = LockOrderGuard::enter(LockRank::Mempool);
            let mut state = self.write();
            state.prioritise_transaction(*txid, fee_delta);
            self.publish_usage(&state);
        }
        tracing::debug!(target: "mempool", "PrioritiseTransaction: {txid} fee += {fee_delta}");
    }

    fn with_short_write(&self, f: impl FnOnce(&mut PoolState)) {
        let _order = LockOrderGuard::enter(LockRank::Mempool);
        f(&mut self.write());
    }

    /// `ProcessTransaction`'s `check(CoinsTip(), height + 1)`, sampled by `-checkmempool`; the
    /// coins half is not run (as in the sink's `check`).
    // Core: validation.cpp:4930, txmempool.cpp:1371-1380 at v24.0.0
    fn check_sampled(&self) {
        self.short_read(|state| {
            if !sampled(state.options().check_ratio) {
                return;
            }
            tracing::debug!(
                target: "mempool",
                "Checking mempool with {} transactions and {} inputs",
                state.size(),
                state.next_tx().len()
            );
            state.check();
        });
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|m| (*m).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a non-string payload".to_owned())
}

/// [`serve`], whose panic is logged as it unwinds, so it is never only a `Disconnected` reply;
/// [`Mempool::detach_chain`] returns it too.
fn serve_logged(pool: &Weak<Mempool>, chain: &ChainHandle, requests: &mpsc::Receiver<Request>) {
    if let Err(panic) = panic::catch_unwind(AssertUnwindSafe(|| serve(pool, chain, requests))) {
        tracing::error!(
            target: "mempool",
            "the mempool admission worker panicked: {}",
            panic_message(&*panic)
        );
        panic::resume_unwind(panic);
    }
}

/// The worker: each request in arrival order, an admission's turn finished before the next.
fn serve(pool: &Weak<Mempool>, chain: &ChainHandle, requests: &mpsc::Receiver<Request>) {
    while let Ok(request) = requests.recv() {
        let Some(pool) = pool.upgrade() else {
            return;
        };
        match request {
            Request::Prioritise {
                txid,
                fee_delta,
                done,
            } => {
                pool.prioritise_now(&txid, fee_delta);
                done.send(());
            }
            #[cfg(feature = "test-support")]
            Request::Panic => panic!("injected mempool admission worker panic"),
            Request::Accept {
                tx,
                bypass_limits,
                test_accept,
                reply,
            } => {
                let (ran, turned) = mpsc::channel::<()>();
                let command = process_transaction(pool, tx, bypass_limits, test_accept, reply, ran);
                submit_turn(chain, command, &turned);
            }
            Request::Package {
                txns,
                test_accept,
                reply,
            } => {
                let (ran, turned) = mpsc::channel::<()>();
                let command = process_package(pool, txns, test_accept, reply, ran);
                submit_turn(chain, command, &turned);
            }
        }
    }
}

/// Submits `command` as V's turn and waits until V ran or dropped it. A stopped V drops it here
/// and its reply disconnects.
fn submit_turn(chain: &ChainHandle, command: MempoolCommand, turned: &mpsc::Receiver<()>) {
    if chain.send(ChainCommand::Mempool(command)).is_ok() {
        let _ = turned.recv();
    }
}

/// `ProcessNewPackage(txns, test_accept)` as one class-M turn at `GetTime()`, each transaction's
/// stateless record computed in it. Dropping the turn, run or not, wakes the worker.
// Core: validation.cpp:1763-1790 at v24.0.2
fn process_package(
    pool: Arc<Mempool>,
    txns: Package,
    test_accept: bool,
    reply: ReplySender<PackageMempoolAcceptResult>,
    ran: mpsc::Sender<()>,
) -> MempoolCommand {
    Box::new(move |turn| {
        let _ran = ran;
        let records: Vec<StatelessRecord> = pool.short_read(|state| {
            txns.iter().map(|tx| StatelessRecord::new(tx, state.options())).collect()
        });
        let accept_time = turn.clock().node_now().secs();
        process_new_package(
            &*pool,
            turn,
            &txns,
            &records,
            accept_time,
            test_accept,
            Box::new(move |r| reply.send(r)),
        )
    })
}

/// `ProcessTransaction(tx, test_accept, bypass_limits)` as one class-M turn: `AcceptToMemoryPool`
/// at `GetTime()`, then the sampled `check`. Dropping the turn, run or not, wakes the worker.
// Core: validation.cpp:4920-4932 at v24.0.0
fn process_transaction(
    pool: Arc<Mempool>,
    tx: TxRef,
    bypass_limits: bool,
    test_accept: bool,
    reply: ReplySender<MempoolAcceptResult>,
    ran: mpsc::Sender<()>,
) -> MempoolCommand {
    Box::new(move |turn| {
        let _ran = ran;
        let record = pool.short_read(|state| StatelessRecord::new(&tx, state.options()));
        let args =
            AcceptArgs::single_accept(turn.clock().node_now().secs(), bypass_limits, test_accept);
        let outcome = accept_to_memory_pool(
            &*pool,
            turn,
            &tx,
            &record,
            args,
            Box::new(move |r| reply.send(r)),
        );
        if outcome.is_ok() {
            pool.check_sampled();
        }
        outcome
    })
}

impl MempoolHandle for Mempool {
    fn accept_async(&self, tx: TxRef, _from: Option<u64>) -> ActorReply<MempoolAcceptResult> {
        self.admit(tx, false, false)
    }

    fn accept(&self, tx: TxRef, bypass_limits: bool) -> Result<MempoolAcceptResult, WaitError> {
        self.admit(tx, bypass_limits, false).recv()
    }

    fn test_accept(
        &self,
        tx: TxRef,
        bypass_limits: bool,
    ) -> Result<MempoolAcceptResult, WaitError> {
        self.admit(tx, bypass_limits, true).recv()
    }

    /// An empty package is a `PCKG_POLICY` refusal before it is queued: it never reaches Core's
    /// `assert(!package.empty())` (`validation.cpp:1767`), which stops V.
    fn process_new_package(
        &self,
        txns: Package,
        test_accept: bool,
    ) -> Result<PackageMempoolAcceptResult, WaitError> {
        if txns.is_empty() {
            let mut refused = PackageMempoolAcceptResult::default();
            refused.state.invalid(PackageValidationResult::Policy, "package-empty", "");
            return Ok(refused);
        }
        let (send, reply) = reply();
        self.request(Request::Package {
            txns,
            test_accept,
            reply: send,
        });
        reply.recv()
    }

    /// Queued behind earlier admissions; the caller waits until it applied. A caller marked
    /// no-wait (LO-9) is refused the wait and returns at once, before the delta applies, as with
    /// [`MempoolHandle::post_prioritise`], which it should use. Never from V: see
    /// [`MempoolHandle::prioritise`].
    fn prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        let _ = self.queue_prioritise(txid, fee_delta).recv();
    }

    /// Queued behind earlier admissions as [`MempoolHandle::prioritise`] is; nobody waits for it.
    fn post_prioritise(&self, txid: &TxHash, fee_delta: Amount) {
        drop(self.queue_prioritise(txid, fee_delta));
    }

    fn add_unbroadcast(&self, txid: &TxHash) {
        self.with_short_write(|state| state.add_unbroadcast_tx(*txid));
    }

    fn remove_unbroadcast(&self, txid: &TxHash, unchecked: bool) {
        self.with_short_write(|state| state.remove_unbroadcast_tx(txid, unchecked));
    }

    fn with_relay_read(&self, f: &mut dyn FnMut(&dyn RelayRead)) {
        self.short_read(|state| f(state));
    }
}
