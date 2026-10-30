//! `DisconnectedBlockTransactions` (`kernel/disconnected_transactions.h:39-136`): the production
//! [`DisconnectPool`].

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use dash_consensus::TxRef;
use dash_primitives::TxHash;

use super::buckets::Buckets;
use super::memusage::{list_usage, recursive_dynamic_usage, unordered_map_usage};
use crate::chain::DisconnectPool;

/// `MAX_DISCONNECTED_TX_POOL_SIZE` (`:18`), in kB: Core's pools hold a thousand times it in bytes
/// (`validation.cpp:3611`, `:3958`, `:4095`).
pub const MAX_DISCONNECTED_TX_POOL_SIZE: usize = 20_000;

/// Core's disconnect pool. The front of the queue is the earliest added: the transactions at the
/// end of the first disconnected block, nearest the old tip. The pool evicts from the front, and
/// the mempool re-adds from the back (`:33-37`).
///
/// Its budget is Core's [`DynamicMemoryUsage`](DisconnectPool::dynamic_memory_usage), a figure
/// computed from Core's containers ([`super::memusage`], [`super::buckets`]) and not from this
/// type's own allocations.
pub struct DisconnectedBlockTransactions {
    /// `cachedInnerUsage`: the queued transactions' `RecursiveDynamicUsage`.
    inner_usage: usize,
    /// `m_max_mem_usage`.
    max_mem_usage: usize,
    /// `queuedTx` by position: an entry keeps its place as others leave, as in Core's list.
    queue: BTreeMap<u64, TxRef>,
    /// The position of the next entry at the back.
    next_position: u64,
    /// `iters_by_txid`: the queue position of each txid (`GetHash()`, [`Tx::txid`]).
    ///
    /// [`Tx::txid`]: dash_consensus::Tx::txid
    by_txid: HashMap<TxHash, u64>,
    /// `iters_by_txid.bucket_count()`, as libstdc++'s map would have it.
    buckets: Buckets,
}

impl DisconnectedBlockTransactions {
    /// `DisconnectedBlockTransactions(max_mem_usage)` (`:64`).
    pub fn new(max_mem_usage: usize) -> Self {
        DisconnectedBlockTransactions {
            inner_usage: 0,
            max_mem_usage,
            queue: BTreeMap::new(),
            next_position: 0,
            by_txid: HashMap::new(),
            buckets: Buckets::default(),
        }
    }

    /// `size()` (`:120`).
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// The queued transactions, front first.
    pub fn queued(&self) -> impl Iterator<Item = &TxRef> {
        self.queue.values()
    }

    /// `iters_by_txid.bucket_count()`.
    pub fn bucket_count(&self) -> u64 {
        self.buckets.count()
    }

    /// `cachedInnerUsage`.
    pub fn inner_usage(&self) -> usize {
        self.inner_usage
    }

    /// `LimitMemoryUsage()` (`:50-61`): evicts from the front while over the budget, returning
    /// the evicted in order. It erases the front's txid from the map whichever entry holds it.
    fn limit_memory_usage(&mut self) -> Vec<TxRef> {
        let mut evicted = Vec::new();
        while self.dynamic_memory_usage() > self.max_mem_usage {
            let Some((_, tx)) = self.queue.pop_first() else {
                break;
            };
            self.inner_usage -= recursive_dynamic_usage(&tx);
            self.by_txid.remove(&tx.txid());
            evicted.push(tx);
        }
        evicted
    }
}

/// Core's pool for every reorg: `MAX_DISCONNECTED_TX_POOL_SIZE * 1000` bytes.
impl Default for DisconnectedBlockTransactions {
    fn default() -> Self {
        DisconnectedBlockTransactions::new(MAX_DISCONNECTED_TX_POOL_SIZE * 1000)
    }
}

impl DisconnectPool for DisconnectedBlockTransactions {
    /// `AddTransactionsFromBlock(vtx)` (`:91-100`): reserves the map for `size + vtx.len()`
    /// entries (which can shrink it), queues `vtx` in reverse at the back, then evicts to the
    /// budget. A txid already queued gets a second queue entry but keeps its map entry, as Core's
    /// `emplace` does; Core assumes it never happens.
    fn add_from_block(&mut self, vtx: &[TxRef]) -> Vec<TxRef> {
        let len = self.by_txid.len() as u64;
        self.buckets.reserve(len, len + vtx.len() as u64);
        for tx in vtx.iter().rev() {
            let position = self.next_position;
            self.next_position += 1;
            self.queue.insert(position, Arc::clone(tx));
            let len = self.by_txid.len() as u64;
            if let Entry::Vacant(entry) = self.by_txid.entry(tx.txid()) {
                // Never grows the table: the `reserve` above already holds `len + vtx.len()`
                // entries, as in libstdc++. `container.txt` pins the growth rule itself.
                self.buckets.insert(len);
                entry.insert(position);
            }
            self.inner_usage += recursive_dynamic_usage(tx);
        }
        self.limit_memory_usage()
    }

    /// `removeForBlock(vtx)` (`:103-118`): each of `vtx`'s txids leaves the pool, the entry the
    /// map holds for it. An empty pool returns at once.
    fn remove_for_block(&mut self, vtx: &[TxRef]) {
        if self.queue.is_empty() {
            return;
        }
        for tx in vtx {
            if let Some(position) = self.by_txid.remove(&tx.txid()) {
                // The map's entries are queued: only `LimitMemoryUsage` takes entries off the
                // queue other than through the map, and it erases their txid's entry.
                let queued = self.queue.remove(&position).expect("the map's entries are queued");
                self.inner_usage -= recursive_dynamic_usage(&queued);
            }
        }
    }

    /// `take()` (`:130-135`): the queue, front first, leaving the pool empty. Like `clear()`, it
    /// keeps the map's buckets.
    fn take(&mut self) -> Vec<TxRef> {
        self.inner_usage = 0;
        self.by_txid.clear();
        std::mem::take(&mut self.queue).into_values().collect()
    }

    /// `DynamicMemoryUsage()` (`:80-82`).
    fn dynamic_memory_usage(&self) -> usize {
        self.inner_usage
            + unordered_map_usage(self.by_txid.len(), self.buckets.count() as usize)
            + list_usage(self.queue.len())
    }
}

/// Core asserts that a pool is empty when it is destroyed (`:74-78`): every reorg drains its
/// pool through `MaybeUpdateMempoolForReorg`. Builds with debug assertions check the same, unless
/// already unwinding; a release build drops the transactions where Core aborts.
impl Drop for DisconnectedBlockTransactions {
    fn drop(&mut self) {
        debug_assert!(
            std::thread::panicking()
                || (self.queue.is_empty() && self.by_txid.is_empty() && self.inner_usage == 0),
            "a disconnect pool is dropped with {} transactions queued",
            self.queue.len()
        );
    }
}
