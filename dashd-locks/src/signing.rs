//! The reads the ChainLock and InstantSend signers share.

use dash_consensus::primitives::TxHash;
use dash_kv::Result;
use dash_primitives::BlockHash;

/// Core's `CInstantSendManager::IsInstantSendEnabled` and `IsLocked`, as the signers read them.
/// Never waits on V or the signals queue.
pub trait IsLockView: Send + Sync {
    // Core: v24.0.2 src/instantsend/instantsend.cpp:513-516 IsInstantSendEnabled
    fn is_enabled(&self) -> bool;
    /// `false` whenever [`Self::is_enabled`] is.
    // Core: v24.0.2 src/instantsend/instantsend.cpp:400-407 IsLocked
    fn is_locked(&self, txid: &TxHash) -> Result<bool>;
}

/// Core's `node::GetTransaction(nullptr, &mempool, txid, ..)` as the signers read it:
/// `Some(None)` in the mempool, `Some(Some(block))` mined in `block`, `None` unknown. Never waits
/// on V, the signals queue or txindex catch-up. On the signals thread it reflects every block
/// whose `BlockConnected` came earlier on that queue, as Core's txindex listener does.
pub trait TxSource: Send + Sync {
    fn get_transaction(&self, txid: &TxHash) -> Result<Option<Option<BlockHash>>>;
}
