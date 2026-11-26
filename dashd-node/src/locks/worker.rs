//! The coinbase route's `cl-cb` worker (LK1a F): Core's `ProcessCoinbaseChainLock`, run inside
//! `PeerManagerImpl::BlockConnected` (`net_processing.cpp:2167-2169` at v24.0.0), as the
//! asynchronous completion of the block's held `BlockConnected`. The P2P listener posts a
//! [`CoinbaseJob`] and answers `Pending`; this one unmarked thread runs the job, which waits for
//! V's class M commands (`ClIntake`, the verifier's `OnV`, `RecordChainLock`), and completes
//! the job's ticket: [`Outcome::Done`] once the job ran to its end, Core's early returns
//! included, [`Outcome::Stopped`] when V had stopped, at intake, verification or record. V
//! serves those commands inside every queue wait (`ServiceWait`), so V waiting behind the held
//! event cannot deadlock it.

use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};
use dash_consensus::{Block, BlockId};
use dash_evo::cbtx::CoinbaseFacts;
use dash_evo::locks::ClsigHashSchemes;
use dashd_chainstate::chain::{ChainHandle, Outcome, Ticket};
use dashd_locks::chainlock::coinbase::coinbase_clsig;

use super::chain::{SharedHandler, SharedRelay};

/// A connected block's coinbase ChainLock, held on the block's `BlockConnected`.
pub struct CoinbaseJob {
    pub block: Arc<Block>,
    pub id: BlockId,
    pub height: u32,
    pub ticket: Ticket,
}

enum Msg {
    Job(CoinbaseJob),
    Stop,
}

/// What a job reads besides V: `IsBlockchainSynced()` and `IsEnabled()`, live.
pub struct CoinbaseGates {
    pub synced: Box<dyn Fn() -> bool + Send>,
    pub enabled: Box<dyn Fn() -> bool + Send>,
    /// V20's activation height (a buried deployment).
    pub v20_height: u32,
}

/// The route: V, the gates, and the handler shared with the P2P seam.
pub struct CoinbaseRoute {
    pub v: ChainHandle,
    pub gates: CoinbaseGates,
    pub handler: SharedHandler,
    pub relay: SharedRelay,
}

impl CoinbaseRoute {
    /// One job: the guards, the BLS context and the best height sampled now, at processing
    /// time, then `ProcessNewChainLock(-1, ..)`.
    // Core: chainlock/handler.cpp:104-131 at v24.0.0
    pub fn run(&self, block: &Block, id: BlockId, height: u32) -> Outcome {
        let synced = (self.gates.synced)();
        let enabled = (self.gates.enabled)();
        let view = self.v.view();
        let reader = self.v.reader();
        let found = coinbase_clsig(
            &CoinbaseFacts::of(Some(block)),
            height,
            height >= self.gates.v20_height,
            synced,
            enabled,
            view.cl.best_height(),
            |at| reader.ancestor(id, at).and_then(|a| reader.entry(a)).map(|e| e.hash),
            ClsigHashSchemes::both(view.bls),
        );
        let Some((read, hash)) = found else {
            return Outcome::Done;
        };
        let mut handler = self.handler.lock();
        handler.verifier().take_v_stopped();
        match handler.try_process_external(read, &hash, &self.relay) {
            Ok(()) if handler.verifier().take_v_stopped() => Outcome::Stopped,
            Ok(()) => Outcome::Done,
            Err(_) => Outcome::Stopped,
        }
    }
}

/// The posting end, which the P2P listener holds: a post never waits (LO-9).
#[derive(Clone)]
pub struct CoinbasePost(Sender<Msg>);

impl CoinbasePost {
    /// Hands `job` to the worker. A worker that has exited drops it, which completes its ticket
    /// [`Outcome::Aborted`].
    pub fn post(&self, job: CoinbaseJob) {
        let _ = self.0.send(Msg::Job(job));
    }
}

/// The running worker. [`CoinbaseWorker::stop`] ends it once every earlier job is done.
pub struct CoinbaseWorker {
    post: Sender<Msg>,
    join: Option<JoinHandle<()>>,
}

impl CoinbaseWorker {
    /// Spawns `cl-cb` over `route`.
    pub fn spawn(route: CoinbaseRoute) -> std::io::Result<(CoinbaseWorker, CoinbasePost)> {
        let mut run = move |job: &CoinbaseJob| route.run(&job.block, job.id, job.height);
        let (post, jobs) = crossbeam_channel::unbounded();
        let join = std::thread::Builder::new()
            .name("cl-cb".into())
            .spawn(move || serve(&jobs, &mut run))?;
        Ok((
            CoinbaseWorker {
                post: post.clone(),
                join: Some(join),
            },
            CoinbasePost(post),
        ))
    }

    /// Tells the worker to stop after the jobs posted before, and joins it.
    pub fn stop(mut self) {
        self.stop_and_join();
    }

    fn stop_and_join(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = self.post.send(Msg::Stop);
            let _ = join.join();
        }
    }
}

impl Drop for CoinbaseWorker {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

fn serve(jobs: &Receiver<Msg>, run: &mut impl FnMut(&CoinbaseJob) -> Outcome) {
    while let Ok(Msg::Job(job)) = jobs.recv() {
        let outcome = run(&job);
        job.ticket.complete(outcome);
    }
}
