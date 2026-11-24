//! `NetInstantSend::ProcessInstantSendLock` (PLAN-LK1b §6, slice S4): a verified lock is stored or
//! parked, its transaction's non-locked tracking and recovered signatures are dropped, its block
//! conflicts are resolved on V, and, for a known transaction, the mempool's conflicts go and the
//! lock is announced; then it is relayed.
//!
//! The processor runs on NetInstantSend's worker, never on V: [`IsChain::resolve_block_conflicts`]
//! waits for V's `IsConflictInvalidate`, whose InstantSend half ([`super::conflicts`]) runs inline
//! in V's turn and reads the lock from [`ResolvingLocks`]. No manager or store lock is held across
//! that wait.
//!
//! The mempool, signal and relay calls are seams with no-op defaults until their slices wire them:
//! the mempool through `dashd_mempool::is::IsMempool` ([`super::mempool`]; the node passes the
//! pool once MP1a-9 builds it), `NotifyTransactionLock` through
//! [`IsNotify`] (S7), relay and the transaction request through [`IsPeers`] (S6).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::Tx;
use dash_consensus::primitives::{OutPoint, TxHash};
use dash_kv::Result;
use dash_num::Hash256;
use dash_params::LlmqType;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{ChainHandle, WaitError};
use dashd_mempool::is::IsMempool;
use parking_lot::Mutex;

use super::listener::input_lock_request_id;
use super::manager::{IsManager, NodeId};
use super::mempool::remove_mempool_conflicts_for_lock;
use super::seams::{
    ChainLockReads, IsLockProcessor, IsPeers, IsSignerSeam, RecoveredSigs, SignRequestId, TxLookup,
};
use super::{IsLockHash, LockedTx, core_height, lock_inputs};

/// What `ProcessInstantSendLock` asks of V: dashd-node answers it over its chain handle.
pub trait IsChain: Send + Sync {
    /// `GetBlockHeight(hash)`'s `LookupBlockIndex`: the height of any block V knows, on the
    /// active chain or not. Core's block-height cache is not ported (`listener.rs`).
    /// Core: `instantsend/net_instantsend.cpp:40-55`.
    fn block_height(&self, hash: &BlockHash) -> Option<u32>;

    /// `ResolveBlockConflicts(islockHash, islock)`: V's `IsConflictInvalidate` for the lock,
    /// awaited. V answers InstantSend's half through [`super::conflicts::IsBlockConflicts`].
    /// Core: `instantsend/net_instantsend.cpp:584-643`.
    fn resolve_block_conflicts(&self, lock: &IsLockHash, txid: &TxHash);
}

/// [`IsChain`] over V's handle: the block tree read past V, and the resolution sent as
/// `IsConflictInvalidate` and awaited. A V that stops first has reported its own fault.
pub struct VChain(pub ChainHandle);

impl IsChain for VChain {
    fn block_height(&self, hash: &BlockHash) -> Option<u32> {
        let reader = self.0.reader();
        reader.lookup(hash).and_then(|id| reader.entry(id)).map(|e| e.height)
    }

    fn resolve_block_conflicts(&self, lock: &IsLockHash, txid: &TxHash) {
        let reply =
            self.0.is_conflict_invalidate(Hash256::from(*lock), Hash256::from(*txid.as_bytes()));
        match reply.recv() {
            Ok(_) => {}
            Err(WaitError::Disconnected) => tracing::debug!(
                target: "instantsend",
                "NetInstantSend::ResolveBlockConflicts -- islock={}: V stopped",
                Hash256::from(*lock),
            ),
            Err(e) => {
                tracing::error!(
                    "NetInstantSend::ResolveBlockConflicts -- islock={}: {e}",
                    Hash256::from(*lock),
                );
                if cfg!(debug_assertions) {
                    panic!("the processor must run on a thread that may wait on V: {e}");
                }
            }
        }
    }
}

/// `GetMainSignals().NotifyTransactionLock(tx, islock)`, which S7 wires to the node's signals.
/// Core: `instantsend/net_instantsend.cpp:411`.
pub trait IsNotify: Send + Sync {
    /// Announce a lock for a known transaction.
    fn notify_transaction_lock(&self, _tx: &Arc<Tx>, _islock: &Arc<LockedTx>) {}
}

/// No signal listeners (S7 wires them).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoNotify;

impl IsNotify for NoNotify {}

/// The locks whose block conflicts V is resolving, by hash: Core passes `*islock` into
/// `ResolveBlockConflicts`, while V's command carries only the hash.
#[derive(Debug, Default)]
pub struct ResolvingLocks(pub(super) Mutex<BTreeMap<IsLockHash, Arc<LockedTx>>>);

impl ResolvingLocks {
    /// The lock V is resolving under `hash`.
    pub fn get(&self, hash: &IsLockHash) -> Option<Arc<LockedTx>> {
        self.0.lock().get(hash).cloned()
    }
}

/// What [`IsProcessor`] is built from.
pub struct IsProcessorParts {
    pub manager: Arc<IsManager>,
    pub txs: Arc<dyn TxLookup>,
    pub chain: Arc<dyn IsChain>,
    pub chainlocks: Arc<dyn ChainLockReads>,
    pub recsigs: Arc<dyn RecoveredSigs>,
    pub peers: Arc<dyn IsPeers>,
    /// `m_signer`: none on a node that is not a masternode.
    pub signer: Option<Arc<dyn IsSignerSeam>>,
    /// `m_mempool`'s InstantSend surface: none until MP1a-9 wires the pool, which is Core with
    /// an empty mempool.
    pub mempool: Option<Arc<dyn IsMempool>>,
    pub notify: Arc<dyn IsNotify>,
    pub resolving: Arc<ResolvingLocks>,
    /// `llmqTypeDIP0024InstantSend`, the recovered signatures' type.
    pub is_llmq: LlmqType,
}

/// NetInstantSend's [`IsLockProcessor`]: `ProcessInstantSendLock` over S2's manager.
pub struct IsProcessor {
    parts: IsProcessorParts,
}

impl IsProcessor {
    /// The processor over `parts`.
    pub fn new(parts: IsProcessorParts) -> Self {
        IsProcessor {
            parts,
        }
    }

    /// `ResolveBlockConflicts(hash, *islock)`, with the lock readable by V's half for its turn.
    fn resolve_block_conflicts(&self, hash: &IsLockHash, islock: &Arc<LockedTx>) {
        let resolving = &self.parts.resolving;
        resolving.0.lock().insert(*hash, Arc::clone(islock));
        self.parts.chain.resolve_block_conflicts(hash, &islock.txid());
        resolving.0.lock().remove(hash);
    }
}

/// `ClearConflicting(to_delete)`: each conflicting transaction leaves the non-locked graph
/// without retrying its children, and the signer forgets its inputs' requests.
/// Core: `instantsend/net_instantsend.cpp:465-473`.
pub(super) fn clear_conflicting<'a>(
    manager: &IsManager,
    signer: Option<&Arc<dyn IsSignerSeam>>,
    txs: impl Iterator<Item = &'a Arc<Tx>>,
) {
    for tx in txs {
        manager.remove_non_locked_tx(&tx.txid(), false, false);
        if let Some(signer) = signer {
            signer.clear_inputs(ids_from_lockable(&lock_inputs(tx)).into_iter().collect());
        }
    }
}

/// `GetIdsFromLockable(inputs)`: each input's request id, once.
/// Core: `instantsend/net_instantsend.cpp:227-235`.
pub(super) fn ids_from_lockable(inputs: &[OutPoint]) -> BTreeSet<SignRequestId> {
    inputs.iter().map(input_lock_request_id).collect()
}

/// `TruncateRecoveredSigsForInputs(islock)`: the signer forgets the inputs' requests, then each
/// input's recovered signature is truncated. Core: `instantsend/net_instantsend.cpp:645-654`.
pub(super) fn truncate_recovered_sigs_for_inputs(
    signer: Option<&dyn IsSignerSeam>,
    recsigs: &dyn RecoveredSigs,
    llmq: LlmqType,
    inputs: &[OutPoint],
) -> Result<()> {
    let ids = ids_from_lockable(inputs);
    if let Some(signer) = signer {
        signer.clear_inputs(ids.iter().copied().collect());
    }
    for id in &ids {
        recsigs.truncate(llmq, id)?;
    }
    Ok(())
}

impl IsLockProcessor for IsProcessor {
    // Core: instantsend/net_instantsend.cpp:359-423
    fn process_instant_send_lock(
        &self,
        from: NodeId,
        hash: &IsLockHash,
        islock: &Arc<LockedTx>,
        legacy: bool,
    ) -> Result<()> {
        let p = &self.parts;
        let txid = islock.txid();
        tracing::debug!(
            target: "instantsend",
            "NetInstantSend::ProcessInstantSendLock -- txid={}, islock={}: processing islock, \
             peer={}",
            txid,
            Hash256::from(*hash),
            from,
        );
        if let Some(signer) = &p.signer {
            signer.clear_lock(Arc::clone(islock));
        }
        if !p.manager.pre_verify_is_lock(hash, islock, from)? {
            return Ok(());
        }
        let found = p.txs.get_transaction(&txid);
        if let Some((tx, _)) = &found
            && islock.inputs() != lock_inputs(tx).as_slice()
        {
            tracing::info!(
                "NetInstantSend::ProcessInstantSendLock -- txid={}, islock={}: lock inputs do \
                 not match the transaction, peer={}",
                txid,
                Hash256::from(*hash),
                from,
            );
            return Ok(());
        }
        // A mempool hit has the null `hashBlock`, whose height is `nullopt`.
        let block = found.as_ref().and_then(|(_, block)| *block);
        let mined_height = block.and_then(|b| p.chain.block_height(&b)).map(core_height).zip(block);
        match &found {
            Some(_) => {
                if let Some((height, block)) = mined_height
                    && p.chainlocks.has_chain_lock(height, &block)
                {
                    tracing::debug!(
                        target: "instantsend",
                        "NetInstantSend::ProcessInstantSendLock -- txlock={}, islock={}: \
                         dropping islock as it already got a ChainLock in block {}, peer={}",
                        txid,
                        Hash256::from(*hash),
                        block,
                        from,
                    );
                    return Ok(());
                }
                p.manager.write_new_is_lock(hash, islock, mined_height.map(|(h, _)| h))?;
            }
            None => p.manager.add_pending_is_lock(*hash, Arc::clone(islock), from, legacy),
        }
        p.manager.remove_non_locked_tx(&txid, true, false);
        truncate_recovered_sigs_for_inputs(
            p.signer.as_deref(),
            &*p.recsigs,
            p.is_llmq,
            islock.inputs(),
        )?;
        self.resolve_block_conflicts(hash, islock);
        match &found {
            Some((tx, _)) => {
                if let Some(mempool) = &p.mempool {
                    remove_mempool_conflicts_for_lock(
                        &**mempool,
                        &p.manager,
                        p.signer.as_ref(),
                        hash,
                        islock,
                    );
                }
                tracing::debug!(
                    target: "instantsend",
                    "NetInstantSend::ProcessInstantSendLock -- notify about lock {} for tx {}",
                    Hash256::from(*hash),
                    tx.txid(),
                );
                p.notify.notify_transaction_lock(tx, islock);
                if let Some(mempool) = &p.mempool {
                    mempool.add_transactions_updated(1);
                }
                p.peers.relay_inv_filtered_tx(hash, tx);
            }
            None => {
                p.peers.relay_inv_filtered_txid(hash, &txid);
                p.peers.ask_peers_for_transaction(&txid);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
