//! The node's mempool (Core's `NodeContext::mempool`): one pool, built at step 7 before the
//! chainstate loads, which V's tip steps run through, whose admission worker attaches once V's
//! evo state is and detaches once V stops, and `LoadMempool`'s load-tried flag.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use dash_clock::Clock;
use dashd_chainstate::chain::ValidationSignals;
use dashd_mempool::Mempool;
use dashd_mempool::api::MempoolHandle;
use dashd_mempool::options::MemPoolOptions;

/// The node's one pool, beside `CTxMemPool::m_load_tried`.
pub struct NodeMempool {
    pool: Arc<Mempool>,
    load_tried: AtomicBool,
}

impl NodeMempool {
    /// `node.mempool = std::make_unique<CTxMemPool>(mempool_opts)` on V's signals queue, so the
    /// pool's off-V removals (InstantSend's) are queued with V's own events, and on the node
    /// clock (`GetTime()`, mockable), which its expiry and rolling fee read.
    // Core: init.cpp:2035-2037 at v24.0.0
    pub fn new(
        options: MemPoolOptions,
        signals: ValidationSignals,
        clock: Arc<dyn Clock>,
    ) -> NodeMempool {
        let pool = Mempool::with_signals(options, signals);
        pool.set_clock(clock);
        NodeMempool {
            pool: Arc::new(pool),
            load_tried: AtomicBool::new(false),
        }
    }

    /// The pool itself. Never call `prioritise` on it from V: it waits behind the queued
    /// admissions, which wait on V. Core v24.0.0 `src/node/context.h:83`, `NodeContext::mempool`.
    pub fn pool(&self) -> &Arc<Mempool> {
        &self.pool
    }

    /// The pool as its consumers take it (P2P's attach, RPC), under the same rule as
    /// [`pool`](Self::pool). Core v24.0.0 `src/node/context.h:83`, `NodeContext::mempool`.
    pub fn handle(&self) -> Arc<dyn MempoolHandle> {
        Arc::clone(&self.pool) as _
    }

    /// `GetLoadTried()`: the import reached `LoadMempool` with no shutdown requested, which
    /// `getmempoolinfo`'s `loaded` reports. Core v24.0.0 `src/txmempool.cpp:2166-2170`,
    /// `src/rpc/mempool.cpp:664`.
    pub fn load_tried(&self) -> bool {
        self.load_tried.load(Ordering::SeqCst)
    }

    /// `SetLoadTried(!ShutdownRequested())` at `LoadMempool`'s slot; no file is read (MP1b).
    // Core: validation.cpp:4995-5000, txmempool.cpp:2172-2176 at v24.0.0
    pub(crate) fn set_load_tried(&self, tried: bool) {
        self.load_tried.store(tried, Ordering::SeqCst);
    }
}
