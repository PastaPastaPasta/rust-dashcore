//! The server's mempool (D17): MP1a's pool for admission and prioritisation, Core's
//! `CCoinsViewMemPool` over the pool and V's tip coins, and LK1b's InstantSend locks.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_consensus::Tx;
use dash_consensus::coins::Coin;
use dash_primitives::{OutPoint, Transaction, TxHash, TxOut};
use dashd_chainstate::chain::{
    ActorReply, ChainHandle, ReplySender, WaitError, reply, spawn_thread,
};
use dashd_coinjoin::env::{AsyncMempool, Mempool, Pending};
use dashd_locks::is::manager::IsManager;
use dashd_mempool::accept::view::{MEMPOOL_HEIGHT, pool_coin};
use dashd_mempool::api::MempoolHandle;
use dashd_mempool::types::{MempoolAcceptResult, MempoolAcceptResultType};
use dashd_store::KvEngine;
use dashd_store::flatfile::FlatFs;

use crate::rpc::CoinsRead;
use crate::steps::ChainSteps;

/// The node's pool, V (for the tip's coins) and the InstantSend manager.
struct Parts {
    pool: Arc<dyn MempoolHandle>,
    v: ChainHandle,
    coins: CoinsRead,
    is: Option<Arc<IsManager>>,
    accepts: Arc<Accepts>,
}

/// The most test-accepts waiting for [`Accepts`]' thread. A caller past them waits for a slot.
const ACCEPT_QUEUE: usize = 8;

/// The server's test-accepts, run in order on one `cj-accept` thread, started at the first. At
/// most [`ACCEPT_QUEUE`] wait; another caller waits for one of them to start, as Core's message
/// thread waits for its inline test-accept (`IsCollateralValid` under `cs_main`). It stops when
/// its [`Parts`] are dropped.
// Core: coinjoin/coinjoin.cpp:477, :498
#[derive(Default)]
struct Accepts {
    queue: Mutex<Queue>,
    ready: Condvar,
}

type Job = (Transaction, ReplySender<Option<i64>>);

/// Where [`Accepts::push`] left a test-accept.
enum Pushed {
    /// Queued: its answer.
    Queued(ActorReply<Option<i64>>),
    /// The queue was full: the transaction back, and a reply once a job starts.
    Full(Transaction, ActorReply<()>),
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// Callers waiting for a slot, each woken to try again when a job starts.
    waiting: VecDeque<ReplySender<()>>,
    started: bool,
    stopped: bool,
}

impl Accepts {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues `tx`'s test-accept on `pool`, unless the queue is full. `None` when the thread
    /// could not start or stopped.
    fn push(self: &Arc<Self>, pool: &Arc<dyn MempoolHandle>, tx: Transaction) -> Option<Pushed> {
        let mut queue = self.queue();
        if queue.stopped {
            return None;
        }
        if queue.jobs.len() >= ACCEPT_QUEUE {
            let (send, slot) = reply();
            queue.waiting.push_back(send);
            return Some(Pushed::Full(tx, slot));
        }
        if !queue.started {
            let (this, pool) = (Arc::clone(self), Arc::clone(pool));
            spawn_thread("cj-accept", move || this.work(&*pool)).ok()?;
            queue.started = true;
        }
        let (send, answer) = reply();
        queue.jobs.push_back((tx, send));
        self.ready.notify_one();
        Some(Pushed::Queued(answer))
    }

    fn work(&self, pool: &dyn MempoolHandle) {
        loop {
            let (tx, send) = {
                let mut queue = self.queue();
                loop {
                    if queue.stopped {
                        return;
                    }
                    if let Some(job) = queue.jobs.pop_front() {
                        queue.waiting.drain(..).for_each(|slot| slot.send(()));
                        break job;
                    }
                    queue = self.ready.wait(queue).unwrap_or_else(PoisonError::into_inner);
                }
            };
            send.send(base_fee(pool.test_accept(Arc::new(Tx::new(tx)), false)));
        }
    }
}

impl Drop for Parts {
    fn drop(&mut self) {
        let mut queue = self.accepts.queue();
        queue.stopped = true;
        queue.jobs.clear();
        queue.waiting.clear();
        drop(queue);
        self.accepts.ready.notify_all();
    }
}

/// The server's [`Mempool`] over the node's pool. Its [`AsyncMempool`] is what the server's
/// message continuation awaits; the blocking calls are for threads that may wait on V. Without a
/// pool (the default) every admission and test-accept is refused and no coin is found.
#[derive(Clone, Default)]
pub struct NodeMempool(Option<Arc<Parts>>);

impl NodeMempool {
    pub fn new(
        pool: Arc<dyn MempoolHandle>,
        v: ChainHandle,
        coins: CoinsRead,
        is: Option<Arc<IsManager>>,
    ) -> Self {
        Self(Some(Arc::new(Parts {
            pool,
            v,
            coins,
            is,
            accepts: Arc::default(),
        })))
    }

    /// Over `steps`' pool, tip coins and InstantSend manager and `v`, once step 7 loaded them.
    pub(crate) fn of<E: KvEngine + 'static, F: FlatFs>(
        steps: &ChainSteps<E, F>,
        v: &ChainHandle,
    ) -> Option<Self> {
        let (pool, coins) = (steps.mempool()?, steps.coins_cell()?);
        Some(Self::new(
            pool.handle(),
            v.clone(),
            crate::rpc::coins_read(Arc::clone(coins)),
            steps.instantsend().map(|is| Arc::clone(&is.manager)),
        ))
    }
}

/// The base fee of a valid test-accept or admission.
fn base_fee(result: Result<MempoolAcceptResult, WaitError>) -> Option<i64> {
    let result = result.ok()?;
    (result.result_type == MempoolAcceptResultType::Valid).then_some(result.base_fee).flatten()
}

fn view_coin(coin: Coin) -> ViewCoin {
    ViewCoin {
        out: TxOut::from_duffs(coin.value(), coin.script_pubkey().to_vec().into()),
        in_mempool: coin.height() == MEMPOOL_HEIGHT,
    }
}

/// `reply` mapped by `f`, `None` when V or the pool dropped it.
fn mapped<T: Send + 'static, U>(
    reply: ActorReply<T>,
    f: impl FnOnce(T) -> U + Send + 'static,
) -> Pending<U> {
    Box::pin(async move { reply.await.ok().map(f) })
}

impl Parts {
    /// `CCoinsViewMemPool::GetCoin` of each outpoint: the pool's answer under one read lock,
    /// then V's tip coins for the rest in one query turn. A spent or unreadable coin is missing.
    // Core: txmempool.cpp:1954-1975; coinjoin/coinjoin.cpp:478-483
    fn coins(&self, outpoints: Vec<OutPoint>) -> ActorReply<Vec<Option<ViewCoin>>> {
        let pooled: Vec<Option<Option<Coin>>> =
            self.pool.read(|r| outpoints.iter().map(|o| pool_coin(r, o)).collect());
        let coins = Arc::clone(&self.coins);
        self.v.query(move |_| {
            outpoints
                .iter()
                .zip(pooled)
                .map(|(o, pooled)| {
                    pooled
                        .unwrap_or_else(|| coins(o).ok().and_then(|(coin, _)| coin))
                        .filter(|coin| !coin.is_spent())
                        .map(view_coin)
                })
                .collect()
        })
    }
}

impl MempoolAccept for NodeMempool {
    // Core: coinjoin/coinjoin.cpp:444
    fn test_accept(&self, tx: &Transaction) -> Option<i64> {
        let parts = self.0.as_ref()?;
        base_fee(parts.pool.test_accept(Arc::new(Tx::new(tx.clone())), false))
    }
}

impl CoinView for NodeMempool {
    fn coin(&self, outpoint: &OutPoint) -> Option<ViewCoin> {
        self.0.as_ref()?.coins(vec![*outpoint]).recv().ok()?.pop().flatten()
    }

    /// `IsLocked`, false without the manager or on a failed read.
    // Core: coinjoin/coinjoin.cpp:483; instantsend/instantsend.cpp:400-407
    fn is_instantsend_locked(&self, txid: &TxHash) -> bool {
        let is = self.0.as_ref().and_then(|parts| parts.is.as_ref());
        is.is_some_and(|is| is.is_locked(txid).unwrap_or(false))
    }
}

impl Mempool for NodeMempool {
    // Core: coinjoin/coinjoin.cpp:455
    fn submit(&mut self, tx: &Transaction) -> bool {
        let Some(parts) = &self.0 else {
            return false;
        };
        base_fee(parts.pool.accept(Arc::new(Tx::new(tx.clone())), false)).is_some()
    }

    // Core: coinjoin/server.cpp:481; net_processing.cpp:3716
    fn prioritise(&mut self, txid: &TxHash, fee_delta: i64) {
        if let Some(parts) = &self.0 {
            parts.pool.prioritise(txid, fee_delta);
        }
    }

    fn awaitable(&self) -> Option<Arc<dyn AsyncMempool>> {
        Some(Arc::new(self.clone()))
    }
}

impl AsyncMempool for NodeMempool {
    fn coins(&self, outpoints: Vec<OutPoint>) -> Pending<Vec<Option<ViewCoin>>> {
        match &self.0 {
            Some(parts) => mapped(parts.coins(outpoints), |coins| coins),
            None => Box::pin(std::future::ready(Some(vec![None; outpoints.len()]))),
        }
    }

    /// Read when asked, outside the server's lock: a store read, not a V command.
    fn instantsend_locked(&self, txids: Vec<TxHash>) -> Pending<Vec<bool>> {
        let locked = txids.iter().map(|txid| self.is_instantsend_locked(txid)).collect();
        Box::pin(std::future::ready(Some(locked)))
    }

    /// The pool's test-accept blocks, so it runs on one `cj-accept` thread behind a bounded queue;
    /// a caller past the queue waits for a slot.
    fn test_accept(&self, tx: Transaction) -> Pending<Option<i64>> {
        let Some(parts) = &self.0 else {
            return Box::pin(std::future::ready(Some(None)));
        };
        let (accepts, pool) = (Arc::clone(&parts.accepts), Arc::clone(&parts.pool));
        Box::pin(async move {
            let mut tx = tx;
            loop {
                match accepts.push(&pool, tx)? {
                    Pushed::Queued(answer) => return answer.await.ok(),
                    Pushed::Full(back, slot) => {
                        tx = back;
                        slot.await.ok()?;
                    }
                }
            }
        })
    }

    fn submit(&self, tx: Transaction) -> Pending<bool> {
        match &self.0 {
            Some(parts) => mapped(parts.pool.accept_async(Arc::new(Tx::new(tx)), None), |r| {
                base_fee(Ok(r)).is_some()
            }),
            None => Box::pin(std::future::ready(Some(false))),
        }
    }

    /// Posted: it applies ahead of every admission asked for after it.
    fn prioritise(&self, txid: TxHash, fee_delta: i64) -> Pending<()> {
        if let Some(parts) = &self.0 {
            parts.pool.post_prioritise(&txid, fee_delta);
        }
        Box::pin(std::future::ready(Some(())))
    }
}
