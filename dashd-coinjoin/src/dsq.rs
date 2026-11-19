//! Queue intake and inventory service for a wallet build with no loaded wallets.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use dash_coinjoin::queue::{
    ClientQueueSessions, DsqOutcome, MixingCounters, QueueManager, client_process_dsq,
};
use dash_num::Hash256;
use dash_p2p::CommandString;
use dash_p2p::policy::Throw;
use dash_p2p_core::{DsQueue, InvType, Inventory};
use dash_primitives::{OutPoint, TxHash};
use dash_types::Hashable;
use dashd_p2p::processing::logic::out_msg;
use dashd_p2p::processing::registry::Extension;
use dashd_p2p::{HandlerCx, NetHandler};

use crate::env::{ChainView, MnLists};

const DSQ: CommandString = CommandString::from_static("dsq");
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(1);

struct NoSessions;

impl ClientQueueSessions for NoSessions {
    fn try_submit_denominate(&mut self, _pro_tx_hash: &TxHash) -> bool {
        false
    }

    fn is_mixing_on(&self, _collateral: &OutPoint) -> bool {
        false
    }
}

/// Tests whether a DSQ inventory names a kept queue, for either node role.
// Core v24.0.0: net_processing.cpp:2455-2456; coinjoin/server.cpp:1445-1447.
pub fn already_have_queue(queues: &QueueManager, inv: &Inventory) -> bool {
    inv.inv_type() == InvType::Dsq && queues.has_queue(&inv.inv_hash())
}

/// Serves a kept DSQ; false lets the processor return `notfound`.
// Core v24.0.0: net_processing.cpp:3061-3066; coinjoin/server.cpp:1450-1458.
pub fn process_queue_getdata(queues: &QueueManager, cx: &HandlerCx<'_>, inv: &Inventory) -> bool {
    if inv.inv_type() != InvType::Dsq {
        return false;
    }
    let Some(queue) = queues.queue_from_hash(&inv.inv_hash()) else {
        return false;
    };
    cx.push(out_msg(DSQ, &queue.dsq));
    true
}

/// Owns the non-masternode queue manager and shares it with its maintenance task.
pub struct DsqRuntime<C, M> {
    chain: Arc<C>,
    mns: Arc<M>,
    counters: Arc<Mutex<MixingCounters>>,
    queues: Option<Arc<Mutex<QueueManager>>>,
}

impl<C, M> Clone for DsqRuntime<C, M> {
    fn clone(&self) -> Self {
        Self {
            chain: Arc::clone(&self.chain),
            mns: Arc::clone(&self.mns),
            counters: Arc::clone(&self.counters),
            queues: self.queues.as_ref().map(Arc::clone),
        }
    }
}

impl<C: ChainView, M: MnLists> DsqRuntime<C, M> {
    /// Creates queues only when the node relays transactions.
    // Core v24.0.0: coinjoin/walletman.cpp:98-110.
    pub fn new(
        relay_txs: bool,
        chain: Arc<C>,
        mns: Arc<M>,
        counters: Arc<Mutex<MixingCounters>>,
    ) -> Self {
        Self {
            chain,
            mns,
            counters,
            queues: relay_txs.then(|| Arc::new(Mutex::new(QueueManager::default()))),
        }
    }

    /// Returns a queue by its inventory hash.
    pub fn get(&self, hash: &Hash256) -> Option<DsQueue> {
        self.queues
            .as_ref()?
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queue_from_hash(hash)
            .map(|entry| entry.dsq.clone())
    }

    /// Returns the number of retained queues.
    pub fn len(&self) -> usize {
        self.queues
            .as_ref()
            .map_or(0, |queues| queues.lock().unwrap_or_else(PoisonError::into_inner).len())
    }

    /// Returns whether no queues are retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Arms the one-second maintenance task when transaction relay is enabled.
    // Core v24.0.0: coinjoin/walletman.cpp:133-137.
    pub fn schedule(&self, now: Instant) -> Option<DsqMaintenance<C>> {
        Some(DsqMaintenance {
            chain: Arc::clone(&self.chain),
            queues: Arc::clone(self.queues.as_ref()?),
            next: now + MAINTENANCE_INTERVAL,
        })
    }
}

impl<C: ChainView + Send + Sync, M: MnLists + Send + Sync> NetHandler for DsqRuntime<C, M> {
    fn commands(&self) -> &'static [CommandString] {
        &[DSQ]
    }

    fn extension(&self) -> Option<Extension> {
        Some(Extension::CoinJoinWallet)
    }

    // Core v24.0.0: coinjoin/walletman.cpp:230-259,261-326.
    fn process_message(
        &mut self,
        cx: &mut HandlerCx<'_>,
        cmd: &CommandString,
        mut payload: &[u8],
    ) -> Result<(), Throw> {
        let Some(queues) = self.queues.as_ref().filter(|_| *cmd == DSQ) else {
            return Ok(());
        };
        if !self.chain.blockchain_synced() {
            return Ok(());
        }
        let dsq: DsQueue = cx.read(&mut payload)?;
        let inv = Inventory::new(InvType::Dsq, dsq.hash());
        cx.received_response(inv);
        let mut queues = queues.lock().unwrap_or_else(PoisonError::into_inner);
        let mut counters = self.counters.lock().unwrap_or_else(PoisonError::into_inner);
        let outcome = client_process_dsq(
            &mut queues,
            &mut *counters,
            &*self.mns,
            &mut NoSessions,
            dsq,
            cx.core.adjusted_time(),
        );
        if let DsqOutcome::Misbehaving(score) = outcome {
            cx.misbehaving(score, "invalid CoinJoin queue");
        }
        Ok(())
    }

    fn already_have(&self, inv: &Inventory) -> bool {
        self.queues.as_ref().is_some_and(|queues| {
            already_have_queue(&queues.lock().unwrap_or_else(PoisonError::into_inner), inv)
        })
    }

    fn process_getdata(&mut self, cx: &mut HandlerCx<'_>, inv: &Inventory) -> bool {
        self.queues.as_ref().is_some_and(|queues| {
            process_queue_getdata(&queues.lock().unwrap_or_else(PoisonError::into_inner), cx, inv)
        })
    }
}

/// A scheduler task sharing the wallet queue manager.
pub struct DsqMaintenance<C> {
    chain: Arc<C>,
    queues: Arc<Mutex<QueueManager>>,
    next: Instant,
}

impl<C: ChainView> DsqMaintenance<C> {
    /// Runs a due tick and rearms from its end. `now` is steady time; `adjusted_now` is seconds.
    // Core v24.0.0: coinjoin/walletman.cpp:133-137,210-214.
    pub fn tick(&mut self, now: Instant, adjusted_now: i64, shutdown_requested: bool) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + MAINTENANCE_INTERVAL;
        if self.chain.blockchain_synced() && !shutdown_requested {
            // Core v24.0.0 coinjoin/coinjoin.cpp:176-179: maintenance skips a busy queue lock.
            if let Ok(mut queues) = self.queues.try_lock() {
                queues.check_queue(adjusted_now);
            }
        }
        true
    }
}
