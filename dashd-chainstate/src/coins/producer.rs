//! [`PrefetchProducer`]: S2's worker threads (CH4-1; A1 §3.2, §3.4; CH2 §2.4.3). They read V's
//! [`PrefetchRequest`]s from database snapshots and hand the batches back for V to admit behind
//! the fence.
//!
//! - V stamps each request's generation as it dispatches it, so a worker's snapshot comes after
//!   the stamp (DEC-142, O5).
//! - V never waits on a worker: it dispatches with `try_send` and takes results with `try_recv`.
//!   It keeps at most `depth` requests in flight and the results channel holds `depth`, so a
//!   worker's send never blocks either. The workers carry LO-9's no-wait marker, as V's helper
//!   threads do.
//! - A read error has tripped the latch and told the abort sink at the read (CH2 §2.4.4 rule 4).
//!   The worker hands it back and stops; [`PrefetchProducer::drain`] returns it. Once every worker
//!   has stopped, `drain` joins them (raising a worker's panic) and the producer refuses requests.
//! - Dropping the producer cancels the queued requests, closes the channel and joins the workers,
//!   each after its current batch: a snapshot never outlives the producer, so the engine's owner
//!   drops the producer before it closes the engine. The owner is unmarked, so the joins wait.

use std::sync::Arc;

use crossbeam_channel::{Sender, TryRecvError, bounded};
use dashd_store::KvEngine;

use super::cache::CoinsCache;
use super::prefetch::{PrefetchBatch, PrefetchRequest, read_from};
use super::table::CoinsTable;
use crate::chain::reply::{NoWaitGuard, NodeJoinHandle, no_wait_marked, spawn_thread};
use crate::store::{CoinsReadError, GuardedEngine};

type Read = Result<PrefetchBatch, CoinsReadError>;

/// A worker's stage boundaries, where the interleaving tests park it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    /// It took a request.
    Took,
    /// It took the request's snapshot.
    Snapshot,
    /// It read the request's keys.
    Read,
    /// It sent the batch.
    Sent,
}

/// What a worker calls at each [`Stage`] (the interleaving tests' scheduler).
#[cfg(test)]
pub(crate) type Gate = Arc<dyn Fn(Stage) + Send + Sync>;

/// S2's workers: see the module docs.
pub struct PrefetchProducer {
    requests: Option<Sender<PrefetchRequest>>,
    /// The request channel's own receiving end, which the drop empties.
    #[expect(clippy::disallowed_types, reason = "only emptied with try_recv, never waited on")]
    queued: crossbeam_channel::Receiver<PrefetchRequest>,
    #[expect(clippy::disallowed_types, reason = "V's own results, which it only polls")]
    results: crossbeam_channel::Receiver<Read>,
    depth: usize,
    in_flight: usize,
    workers: Vec<NodeJoinHandle<()>>,
}

impl PrefetchProducer {
    /// `workers` threads reading from `db`, with at most `depth` requests in flight.
    ///
    /// # Errors
    ///
    /// The OS refused a thread; the ones already started stop as the producer drops.
    pub fn spawn<E>(
        db: Arc<GuardedEngine<E>>,
        workers: usize,
        depth: usize,
    ) -> std::io::Result<Self>
    where
        E: KvEngine + Send + Sync + 'static,
    {
        Self::start(db, workers, depth, None)
    }

    /// [`PrefetchProducer::spawn`], with each worker calling `gate` at its stage boundaries.
    #[cfg(test)]
    pub(crate) fn gated<E>(
        db: Arc<GuardedEngine<E>>,
        workers: usize,
        depth: usize,
        gate: Gate,
    ) -> std::io::Result<Self>
    where
        E: KvEngine + Send + Sync + 'static,
    {
        Self::start(db, workers, depth, Some(gate))
    }

    fn start<E>(
        db: Arc<GuardedEngine<E>>,
        workers: usize,
        depth: usize,
        #[cfg(test)] gate: Option<Gate>,
        #[cfg(not(test))] _gate: Option<std::convert::Infallible>,
    ) -> std::io::Result<Self>
    where
        E: KvEngine + Send + Sync + 'static,
    {
        assert!(workers > 0 && depth > 0, "a producer needs a worker and room for a request");
        let (requests, take) = bounded(depth);
        let (send, results) = bounded(depth);
        let mut producer = Self {
            requests: Some(requests),
            queued: take.clone(),
            results,
            depth,
            in_flight: 0,
            workers: Vec::with_capacity(workers),
        };
        for i in 0..workers {
            let worker = Worker {
                db: db.clone(),
                requests: take.clone(),
                results: send.clone(),
                #[cfg(test)]
                gate: gate.clone(),
            };
            producer.workers.push(spawn_thread(&format!("d-prefetch.{i}"), move || worker.run())?);
        }
        Ok(producer)
    }

    /// Dispatches `req` unless `depth` requests are in flight; returns whether it went. A refusal
    /// loses nothing: V reads those coins from the database when it connects.
    pub fn request(&mut self, req: PrefetchRequest) -> bool {
        if self.in_flight == self.depth {
            return false;
        }
        let sent = self.requests.as_ref().is_some_and(|r| r.try_send(req).is_ok());
        self.in_flight += usize::from(sent);
        sent
    }

    /// Admits every batch that has arrived into `cache`, behind its fence; returns the entries
    /// admitted. Once every worker has stopped, it joins them, and the producer refuses requests
    /// and has none in flight.
    ///
    /// # Errors
    ///
    /// The read error a worker handed back (it stopped there).
    ///
    /// # Panics
    ///
    /// With a worker's panic, once every worker has stopped.
    pub fn drain<E: KvEngine>(
        &mut self,
        cache: &mut CoinsCache<E>,
        mempool_usage: u64,
    ) -> Result<usize, CoinsReadError> {
        let mut admitted = 0;
        loop {
            match self.results.try_recv() {
                Ok(read) => {
                    self.in_flight -= 1;
                    admitted += cache.admit_prefetch(read?, mempool_usage);
                }
                Err(TryRecvError::Empty) => return Ok(admitted),
                Err(TryRecvError::Disconnected) => {
                    // After a read error an earlier drain returned, or a panic the join raises.
                    self.requests = None;
                    self.in_flight = 0;
                    self.join_workers();
                    return Ok(admitted);
                }
            }
        }
    }

    /// Requests dispatched and not yet drained.
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// The request channel's receiving end, for a test to see the drop empty it.
    #[cfg(test)]
    #[expect(clippy::disallowed_types, reason = "a test only reads its length")]
    pub(crate) fn queue(&self) -> crossbeam_channel::Receiver<PrefetchRequest> {
        self.queued.clone()
    }

    /// Joins every worker, then raises the first one's panic (unless this thread is panicking).
    fn join_workers(&mut self) {
        debug_assert!(
            !no_wait_marked() || std::thread::panicking(),
            "a marked thread's joins would leave the prefetch workers detached (LO-9)"
        );
        let mut panicked = None;
        for worker in self.workers.drain(..) {
            if let Ok(Err(panic)) = worker.join() {
                panicked.get_or_insert(panic);
            }
        }
        if let Some(panic) = panicked
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(panic);
        }
    }
}

impl Drop for PrefetchProducer {
    fn drop(&mut self) {
        // Cancelled, then closed: each worker stops after its current batch, and none is blocked
        // on a send (the credit).
        while self.queued.try_recv().is_ok() {}
        self.requests = None;
        self.join_workers();
    }
}

struct Worker<E: KvEngine> {
    db: Arc<GuardedEngine<E>>,
    #[expect(clippy::disallowed_types, reason = "a worker's requests, from V")]
    requests: crossbeam_channel::Receiver<PrefetchRequest>,
    results: Sender<Read>,
    #[cfg(test)]
    gate: Option<Gate>,
}

impl<E: KvEngine> Worker<E> {
    fn run(self) {
        let _marked = NoWaitGuard::set();
        let table = CoinsTable::default();
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "it waits only for V's next request; the producer's drop closes the channel \
                          before it joins"
            )]
            let Ok(req) = self.requests.recv() else {
                return;
            };
            self.pass(Stage::Took);
            let snap = self.db.snapshot();
            self.pass(Stage::Snapshot);
            let read = read_from(&snap, &table, req);
            drop(snap);
            self.pass(Stage::Read);
            let failed = read.is_err();
            if self.results.send(read).is_err() || failed {
                return;
            }
            self.pass(Stage::Sent);
        }
    }

    fn pass(&self, _stage: Stage) {
        #[cfg(test)]
        if let Some(gate) = &self.gate {
            gate(_stage);
        }
    }
}
