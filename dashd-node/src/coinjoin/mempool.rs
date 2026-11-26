//! The server's mempool (D17): MP1a's pool for admission and prioritisation, Core's
//! `CCoinsViewMemPool` over the pool and V's tip coins, and LK1b's InstantSend locks.

use std::sync::Arc;

use dash_coinjoin::collateral::MempoolAccept;
use dash_coinjoin::inouts::{CoinView, ViewCoin};
use dash_consensus::Tx;
use dash_consensus::coins::Coin;
use dash_primitives::{OutPoint, Transaction, TxHash, TxOut};
use dashd_chainstate::chain::{ActorReply, ChainHandle, WaitError, reply, spawn_thread};
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

    /// The pool's test-accept blocks, so it runs on its own thread.
    fn test_accept(&self, tx: Transaction) -> Pending<Option<i64>> {
        let (send, answer) = reply();
        let this = self.clone();
        if spawn_thread("cj-accept", move || send.send(MempoolAccept::test_accept(&this, &tx)))
            .is_err()
        {
            return Box::pin(std::future::ready(None));
        }
        mapped(answer, |fee| fee)
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
