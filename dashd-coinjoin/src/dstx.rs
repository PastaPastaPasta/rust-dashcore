//! Node ownership and notifications for Dash Core v24.0.0's `CDSTXManager`.

use std::cell::Cell;
use std::sync::{Mutex, MutexGuard, PoisonError};

use dash_coinjoin::dstx::{
    DstxChain, DstxManager, DstxScore, DstxValidation, PoolLimits, StoredDstx, txid, validate_dstx,
};
use dash_coinjoin::queue::{MixingMasternode, MixingMeta};
use dash_coinjoin::relay::{TxInvKind, announce_inv_kind};
use dash_p2p_core::{DsTx, ProtocolVersion};
use dash_primitives::{BlockHash, OutPoint, Transaction, TxHash};

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

    /// Validates a received DSTX with no mempool side effect; the caller applies the
    /// `Continue` priority delta. Core: `src/net_processing.cpp:3642-3720,4930-4938`.
    pub fn check<C, Meta>(
        &self,
        dstx: &DsTx,
        txid: &TxHash,
        meta: &mut Meta,
        chain: &C,
    ) -> DstxValidation
    where
        C: DstxChain + ?Sized,
        Meta: MixingMeta + ?Sized,
    {
        if self.contains(txid) {
            return DstxValidation::Stop(DstxScore::None);
        }
        let walk = Walk {
            chain,
            walked: Cell::new(false),
        };
        let validation =
            validate_dstx(&DstxManager::default(), meta, &walk, self.limits, dstx, txid);
        match validation {
            // Core: net_processing.cpp:3661 at v24.0.2, before the masternode walk.
            DstxValidation::Stop(DstxScore::Invalid | DstxScore::Premature)
                if !walk.walked.get() =>
            {
                tracing::debug!(target: "coinjoin", "DSTX -- Invalid DSTX structure: {txid}");
            }
            // Core: net_processing.cpp:3696 at v24.0.2. A received DSTX names its masternode by
            // proTxHash only, so Core's `masternodeOutpoint` is still the null outpoint here.
            DstxValidation::Stop(DstxScore::UnknownMasternode) => tracing::debug!(
                target: "coinjoin",
                "DSTX -- Can't find masternode {} to verify {txid}",
                to_string_short(&OutPoint::new(TxHash::from([0; 32]), u32::MAX))
            ),
            _ => {}
        }
        validation
    }

    /// [`check`](Self::check), then prioritises the transaction when processing continues.
    /// `Ok(())` continues to admission; `Err(score)` stops, including a zero-penalty stop.
    pub fn validate<C, M, Meta>(
        &self,
        dstx: &DsTx,
        txid: &TxHash,
        meta: &mut Meta,
        chain: &C,
        mempool: &mut M,
    ) -> Result<(), i32>
    where
        C: DstxChain + ?Sized,
        M: Mempool + ?Sized,
        Meta: MixingMeta + ?Sized,
    {
        match self.check(dstx, txid, meta, chain) {
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
        self.expire(new, chain);
    }

    /// Expires DSTXes against the notified ChainLock block when blockchain sync permits.
    /// Core: `src/dsnotificationinterface.cpp:98-104`; `src/coinjoin/coinjoin.cpp:606-611`.
    pub fn notify_chain_lock<C: ChainView + ?Sized>(
        &self,
        block: Option<(i32, BlockHash)>,
        chain: &C,
    ) {
        if chain.blockchain_synced() {
            self.expire(block, chain);
        }
    }

    fn expire<C: ChainView + ?Sized>(&self, block: Option<(i32, BlockHash)>, chain: &C) {
        if let Some((height, hash)) = block {
            let chainlocked = chain.has_chain_lock(height, &hash);
            self.manager().check(height, chainlocked);
        }
    }

    /// Marks each tracked transaction as confirmed at the connected block's height.
    /// Core: `src/coinjoin/coinjoin.cpp:613-624,633-641`.
    pub fn block_connected<'a>(
        &self,
        transactions: impl IntoIterator<Item = &'a Transaction>,
        height: i32,
    ) {
        self.update_confirmed_heights(transactions, Some(height));
    }

    /// Clears each tracked transaction's confirmation height in a disconnected block.
    /// Core: `src/coinjoin/coinjoin.cpp:643-650`.
    pub fn block_disconnected<'a>(&self, transactions: impl IntoIterator<Item = &'a Transaction>) {
        self.update_confirmed_heights(transactions, None);
    }

    fn update_confirmed_heights<'a>(
        &self,
        transactions: impl IntoIterator<Item = &'a Transaction>,
        height: Option<i32>,
    ) {
        let mut manager = self.manager();
        for tx in transactions {
            manager.update_confirmed_height(&txid(tx), height);
        }
    }
}

/// `COutPoint::ToStringShort`: the hash, a dash, the index.
// Core: primitives/transaction.cpp:26-29
fn to_string_short(outpoint: &OutPoint) -> String {
    format!("{}-{}", outpoint.txid(), outpoint.index())
}

/// A [`DstxChain`] that records whether `ValidateDSTX` reached its masternode walk, which follows
/// the structure check.
struct Walk<'c, C: ?Sized> {
    chain: &'c C,
    walked: Cell<bool>,
}

impl<C: DstxChain + ?Sized> DstxChain for Walk<'_, C> {
    fn tip_height(&self) -> Option<i32> {
        self.chain.tip_height()
    }

    fn v24_active_at_tip(&self) -> bool {
        self.chain.v24_active_at_tip()
    }

    fn v24_active_after_tip(&self) -> bool {
        self.chain.v24_active_after_tip()
    }

    fn mn_at_depth(&self, depth: i32, pro_tx_hash: &TxHash) -> Option<MixingMasternode> {
        self.walked.set(true);
        self.chain.mn_at_depth(depth, pro_tx_hash)
    }
}

impl MempoolListener for DstxRuntime {
    /// Clears the confirmation height when a tracked transaction enters the mempool.
    /// Core: `src/dsnotificationinterface.cpp:74-78`; `src/coinjoin/coinjoin.cpp:626-631`.
    fn transaction_added_to_mempool(&self, tx: &Transaction) {
        self.manager().update_confirmed_height(&txid(tx), None);
    }
}
