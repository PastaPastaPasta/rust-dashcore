//! Node ownership and notifications for Dash Core v24.0.0's `CDSTXManager`.

use std::borrow::BorrowMut;
use std::sync::{Mutex, MutexGuard, PoisonError};

use dash_coinjoin::dstx::{
    DstxChain, DstxManager, DstxValidation, PoolLimits, StoredDstx, txid, validate_dstx,
};
use dash_coinjoin::queue;
use dash_coinjoin::relay::{TxInvKind, announce_inv_kind};
use dash_p2p_core::{DsTx, ProtocolVersion};
use dash_primitives::{BlockHash, Transaction, TxHash};

use crate::env::{ChainView, Mempool, MempoolListener};

/// Owns accepted mixing transactions under Core's `cs_mapdstx` lock.
/// Core: `src/coinjoin/coinjoin.cpp:564-650`.
#[derive(Debug)]
pub struct DstxRuntime {
    dstxes: Mutex<DstxManager>,
    limits: PoolLimits,
}

impl DstxRuntime {
    /// Creates an empty manager using the network's pool limits.
    pub fn new(limits: PoolLimits) -> Self {
        Self {
            dstxes: Mutex::default(),
            limits,
        }
    }

    fn manager(&self) -> MutexGuard<'_, DstxManager> {
        self.dstxes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Checks a received DSTX and prioritises its transaction only when processing continues.
    /// `Ok(())` continues to admission; `Err(score)` stops, including a zero-penalty stop.
    /// Core: `src/net_processing.cpp:3642-3720,4930-4938`.
    pub fn validate<C, M, T>(
        &self,
        dstx: &DsTx,
        txid: &TxHash,
        counters: &mut T,
        chain: &C,
        mempool: &mut M,
    ) -> Result<(), i32>
    where
        C: DstxChain + ?Sized,
        M: Mempool + ?Sized,
        T: BorrowMut<queue::MixingCounters>,
    {
        if self.contains(txid) {
            return Err(0);
        }
        // The pure validator only uses its manager for the duplicate check completed above.
        let result = validate_dstx(
            &DstxManager::default(),
            counters.borrow_mut(),
            chain,
            self.limits,
            dstx,
            txid,
        );
        match result {
            DstxValidation::Stop(score) => Err(score.score()),
            DstxValidation::Continue {
                priority_delta,
                ..
            } => {
                mempool.prioritise(txid, priority_delta);
                Ok(())
            }
        }
    }

    /// Keeps a DSTX after transaction admission, preserving an existing entry.
    /// Core: `src/net_processing.cpp:4964-4967`; `src/coinjoin/coinjoin.cpp:564-569`.
    pub fn on_accepted(&self, dstx: DsTx) {
        self.manager().add(dstx);
    }

    /// Returns an owned snapshot of a kept DSTX and its confirmation height.
    /// Core: `src/coinjoin/coinjoin.cpp:571-577`.
    pub fn get(&self, txid: &TxHash) -> Option<StoredDstx> {
        self.manager().get(txid).cloned()
    }

    /// Whether a DSTX is kept for this transaction.
    pub fn contains(&self, txid: &TxHash) -> bool {
        self.manager().contains(txid)
    }

    /// Returns the inventory kind the negotiated peer version can read.
    /// Core: `src/net_processing.cpp:3617-3622,6649-6655,6717-6722`.
    pub fn announce_kind(&self, txid: &TxHash, peer_version: ProtocolVersion) -> TxInvKind {
        announce_inv_kind(&self.manager(), txid, self.limits, peer_version)
    }

    /// Expires DSTXes after the masternode-sync listener has processed the new tip.
    /// Core: `src/dsnotificationinterface.cpp:55-67`; `src/coinjoin/coinjoin.cpp:584-605`.
    pub fn updated_block_tip<C: ChainView + ?Sized>(
        &self,
        new: Option<(i32, BlockHash)>,
        fork: Option<(i32, BlockHash)>,
        initial_download: bool,
        chain: &C,
    ) {
        if new == fork || initial_download || !chain.blockchain_synced() {
            return;
        }
        self.check(new, chain);
    }

    /// Expires DSTXes against the notified ChainLock block when blockchain sync permits.
    /// Core: `src/dsnotificationinterface.cpp:98-104`; `src/coinjoin/coinjoin.cpp:606-611`.
    pub fn notify_chain_lock<C: ChainView + ?Sized>(
        &self,
        block: Option<(i32, BlockHash)>,
        chain: &C,
    ) {
        if chain.blockchain_synced() {
            self.check(block, chain);
        }
    }

    fn check<C: ChainView + ?Sized>(&self, block: Option<(i32, BlockHash)>, chain: &C) {
        if let Some((height, hash)) = block {
            let chainlocked = chain.has_chain_lock(height, &hash);
            self.manager().check(height, chainlocked);
        }
    }

    /// Marks each tracked transaction as confirmed at the connected block's height.
    /// Core: `src/coinjoin/coinjoin.cpp:613-624,633-641`.
    pub fn block_connected(&self, transactions: &[Transaction], height: i32) {
        self.update_confirmed_heights(transactions, Some(height));
    }

    /// Clears each tracked transaction's confirmation height in a disconnected block.
    /// Core: `src/coinjoin/coinjoin.cpp:643-650`.
    pub fn block_disconnected(&self, transactions: &[Transaction]) {
        self.update_confirmed_heights(transactions, None);
    }

    fn update_confirmed_heights(&self, transactions: &[Transaction], height: Option<i32>) {
        let mut manager = self.manager();
        for tx in transactions {
            manager.update_confirmed_height(&txid(tx), height);
        }
    }
}

impl MempoolListener for DstxRuntime {
    /// Clears the confirmation height when a tracked transaction enters the mempool.
    /// Core: `src/dsnotificationinterface.cpp:74-78`; `src/coinjoin/coinjoin.cpp:626-631`.
    fn transaction_added_to_mempool(&self, tx: &Transaction) {
        self.manager().update_confirmed_height(&txid(tx), None);
    }
}
