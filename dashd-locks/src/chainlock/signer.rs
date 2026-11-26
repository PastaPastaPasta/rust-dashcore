//! Core's `ChainLockSigner`: signs the active tip once its recent transactions are safe, and
//! turns the recovered signature into a local ChainLock.
//!
//! Lock order: `try_sign`, then `state`. `try_sign` is held across the walk and the sign, as
//! Core's `cs_try_sign` is, and every acquirer uses `try_lock`. `state` is never held across a
//! [`QuorumSigning`], [`ClProcess`], [`TxSource`](crate::signing::TxSource) or IS store call.
//! [`QuorumSigning`] may wait on V, so tip signing runs only on the signer's scheduler, never on
//! the signals thread.
//! [`ClProcess`] waits on V, so it runs only on recovered-signature callers.
//!
//! Deviations from Core v24.0.2:
//! 1. The first-seen cleanup keeps a mined transaction whose block the chain does not know,
//!    where Core asserts (`handler.cpp:372`).
//! 2. The first-seen cleanup reads [`TxSource`](crate::signing::TxSource) from a snapshot of
//!    the txids with no lock held, where Core holds `cs_main`, `mempool.cs` and `cs`: an entry
//!    re-added between its read and its erase is erased, dropping the newer first-seen time,
//!    which delays signing over it.
//! 3. [`has_conflicting_chainlock`] reads a missing ancestor as no conflict, where Core asserts
//!    (`chainlock.cpp:93-94`).
//! 4. `GetBlockTxs`'s index lookup and disk read are one [`ChainReads::read_block`].
//! 5. [`ClProcess`]'s implementation reads and hashes the signature under the current BLS
//!    scheme.
//! 6. A `Storage` or `Lookup` signing fault, an IS store error in the walk and a tx-source error
//!    in the first-seen cleanup go to [`Abort`]; `Stopped` is silent.
//! 7. The scheduler thread is the [`ClScheduler`] implementation's, not started by the signer,
//!    and no `Drop` stops it: the node calls [`ClSigner::stop`]. Scheduled tasks hold the signer
//!    weakly.
//! 8. `BlockConnected` extends a block's txid set copy-on-write, so a running walk keeps the set
//!    it read.
//! 9. `TxFirstSeen` is its own listener, the signer holds it directly, and the IS reads come
//!    from [`IsLockView`].
//! 10. `UpdatedBlockTip` schedules `TrySignChainTip` on the signer's scheduler, where Core calls
//!     it inline (`signing.cpp:71-74`).
//! 11. The walk refuses a tip when a parent inside its window is missing from the chain, which
//!     Core's `pprev` never is.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use dash_clock::Clock;
use dash_consensus::primitives::TxHash;
use dash_consensus::{Block, BlockId};
use dash_evo::locks::chainlock_request_id;
use dash_evo::locks::wire::RecoveredSig;
use dash_llmq::signing::api::{
    QuorumSigning, RecoveredSigListener, RecoveredSigResult, SignRequest, SigningFault,
};
use dash_llmq::sigshares::manager::CleanupThrottler;
use dash_p2p_core::{ClSig, Inventory};
use dash_params::{ChainSetup, LlmqType};
use dash_pkc::bls::BlsSigBytes;
use dash_primitives::BlockHash;
use dashd_chainstate::chain::{Cont, ValidationInterface};
use parking_lot::Mutex;

use super::first_seen::{Synced, TxFirstSeen, lockable_txids};
use super::reads::{Abort, ChainEntry, ChainReads, TX_CONFIRM_THRESHOLD};
use crate::signing::IsLockView;

/// The period of the signer's scheduled retry and cleanup.
// Core: v24.0.2 src/chainlock/signing.cpp:52 Start
pub const SIGN_INTERVAL: Duration = Duration::from_secs(5);

// Core: v24.0.2 src/chainlock/signing.cpp:295 Cleanup
const CLEANUP_INTERVAL_MS: i64 = 30_000;

/// Core's `chainlock::Chainlocks` as the signer reads it. `has_chainlock` and
/// `has_conflicting_chainlock` return false while `enabled()` is false, as Core's do. Every method
/// returns without waiting on V or the signals queue: the signer calls them under `state`, which
/// the signals thread also takes.
pub trait ClReads: Send + Sync {
    /// `IsEnabled`: SPORK_19 is active.
    fn enabled(&self) -> bool;
    /// `IsSigningEnabled`: SPORK_19's value is 0.
    fn signing_enabled(&self) -> bool;
    /// `GetBestChainLockHeight`: -1 without one.
    fn best_height(&self) -> i32;
    /// `HasChainLock(height, hash)`; see [`has_chainlock`].
    fn has_chainlock(&self, height: i32, hash: &BlockHash) -> bool;
    /// `HasConflictingChainLock(height, hash)`; see [`has_conflicting_chainlock`].
    fn has_conflicting_chainlock(&self, height: i32, hash: &BlockHash) -> bool;
}

/// `ProcessNewChainLock(-1, clsig, qman, ::SerializeHash(clsig))`, returning its first inventory.
/// The implementation reads the signature and hashes it under the current BLS scheme, as Core's
/// `recoveredSig.sig.Get()` does. It waits on V.
pub trait ClProcess: Send + Sync {
    fn process_local(&self, clsig: ClSig) -> Option<Inventory>;
}

/// The signer's own `cls-schdlr` scheduler.
pub trait ClScheduler: Send + Sync {
    /// `scheduleEvery(task, every)`.
    fn schedule_every(&self, task: Box<dyn Fn() + Send + Sync>, every: Duration);
    /// `scheduleFromNow(task, 0)`. A task scheduled after `stop` is dropped.
    fn schedule(&self, task: Box<dyn FnOnce() + Send>);
    /// `stop()`, then the thread's join: no task runs after it returns.
    fn stop(&self);
}

/// Whether the best ChainLock's block, or its ancestor at `height`, is `hash`. The caller applies
/// `IsEnabled`.
// Core: v24.0.2 src/chainlock/chainlock.cpp:49-71 HasChainLock
pub fn has_chainlock(
    best: Option<&ChainEntry>,
    height: i32,
    hash: &BlockHash,
    chain: &dyn ChainReads,
) -> bool {
    best.filter(|b| height <= b.height)
        .is_some_and(|b| chain.ancestor_hash(&b.hash, height) == Some(*hash))
}

/// Whether the best ChainLock's block, or its ancestor at `height`, is another block. A missing
/// ancestor is no conflict, where Core asserts. The caller applies `IsEnabled`.
// Core: v24.0.2 src/chainlock/chainlock.cpp:73-96 HasConflictingChainLock
pub fn has_conflicting_chainlock(
    best: Option<&ChainEntry>,
    height: i32,
    hash: &BlockHash,
    chain: &dyn ChainReads,
) -> bool {
    best.filter(|b| height <= b.height)
        .and_then(|b| chain.ancestor_hash(&b.hash, height))
        .is_some_and(|ancestor| ancestor != *hash)
}

pub struct ClSignerParts {
    pub chain: Arc<dyn ChainReads>,
    pub cl: Arc<dyn ClReads>,
    pub is: Arc<dyn IsLockView>,
    pub signing: Arc<dyn QuorumSigning>,
    pub process: Arc<dyn ClProcess>,
    pub first_seen: Arc<TxFirstSeen>,
    pub synced: Synced,
    pub clock: Arc<dyn Clock>,
    pub scheduler: Arc<dyn ClScheduler>,
    pub abort: Abort,
}

type BlockTxs = Arc<HashSet<TxHash>>;

struct State {
    block_txs: HashMap<BlockHash, BlockTxs>,
    last_signed_height: i32,
    last_signed_request_id: [u8; 32],
    last_signed_msg_hash: [u8; 32],
}

/// Core's `chainlock::ChainLockSigner`. The node calls [`ClSigner::stop`]; dropping the signer
/// does not stop its scheduler.
pub struct ClSigner {
    me: Weak<ClSigner>,
    parts: ClSignerParts,
    llmq_type: LlmqType,
    try_sign: Mutex<()>,
    state: Mutex<State>,
    throttle: Mutex<CleanupThrottler>,
}

impl ClSigner {
    pub fn new(params: &ChainSetup, parts: ClSignerParts) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: Weak::clone(me),
            parts,
            llmq_type: params.consensus.llmq_type_chain_locks,
            try_sign: Mutex::new(()),
            state: Mutex::new(State {
                block_txs: HashMap::new(),
                last_signed_height: -1,
                last_signed_request_id: [0; 32],
                last_signed_msg_hash: [0; 32],
            }),
            throttle: Mutex::default(),
        })
    }

    /// Every [`SIGN_INTERVAL`] while signing is enabled: retries the tip, as a missing islock may
    /// have refused it, then cleans up.
    // Core: v24.0.2 src/chainlock/signing.cpp:43-53 Start
    pub fn start(&self) {
        let this = Weak::clone(&self.me);
        let task = move || {
            if let Some(this) = this.upgrade()
                && this.parts.cl.signing_enabled()
            {
                this.try_sign_chain_tip();
                this.cleanup();
            }
        };
        self.parts.scheduler.schedule_every(Box::new(task), SIGN_INTERVAL);
    }

    // Core: v24.0.2 src/chainlock/signing.cpp:55-59 Stop
    pub fn stop(&self) {
        self.parts.scheduler.stop();
    }

    /// Runs on the signer's scheduler only: the sign call may wait on V.
    // Core: v24.0.2 src/chainlock/signing.cpp:76-178 TrySignChainTip
    pub fn try_sign_chain_tip(&self) {
        let Some(_guard) = self.try_sign.try_lock() else {
            return;
        };
        let cl = &*self.parts.cl;
        if !(self.parts.synced)() || !cl.enabled() || !cl.signing_enabled() {
            return;
        }
        let Some(tip) = self.parts.chain.tip().filter(|t| t.prev.is_some()) else {
            return;
        };
        if self.state.lock().last_signed_height == tip.height
            || cl.best_height() >= tip.height
            || cl.has_conflicting_chainlock(tip.height, &tip.hash)
        {
            return;
        }
        tracing::debug!(target: "chainlocks", "TrySignChainTip -- trying to sign {}, height={}", tip.hash, tip.height);
        if self.parts.is.is_enabled() {
            match self.tip_txs_safe(&tip) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => return (self.parts.abort)(&e.to_string()),
            }
        }
        let request_id = chainlock_request_id(tip.height);
        let msg_hash = *tip.hash.as_bytes();
        if cl.best_height() >= tip.height {
            return;
        }
        {
            let mut state = self.state.lock();
            state.last_signed_height = tip.height;
            state.last_signed_request_id = request_id;
            state.last_signed_msg_hash = msg_hash;
        }
        let req = SignRequest {
            llmq_type: self.llmq_type,
            id: request_id,
            msg_hash,
            quorum_hash: None,
            allow_resign: false,
            allow_diff_msg: false,
        };
        match self.parts.signing.async_sign_if_member(&req) {
            Ok(_) | Err(SigningFault::Stopped) => {}
            Err(e) => (self.parts.abort)(&e.to_string()),
        }
    }

    /// The walk from the tip down: every transaction is old enough or islocked, until the depth
    /// passes [`TX_CONFIRM_THRESHOLD`] or a block is ChainLocked.
    // Core: v24.0.2 src/chainlock/signing.cpp:129-161 TrySignChainTip
    fn tip_txs_safe(&self, tip: &ChainEntry) -> dash_kv::Result<bool> {
        let mut entry = *tip;
        loop {
            if tip.height - entry.height > TX_CONFIRM_THRESHOLD {
                tracing::debug!(target: "chainlocks", "TrySignChainTip -- tip and previous {TX_CONFIRM_THRESHOLD} blocks all safe");
                return Ok(true);
            }
            if self.parts.cl.has_chainlock(entry.height, &entry.hash) {
                tracing::debug!(target: "chainlocks", "TrySignChainTip -- chainlock at height {}", entry.height);
                return Ok(true);
            }
            for txid in self.block_txs(&entry.hash).iter().flat_map(|txs| txs.iter()) {
                if !self.parts.first_seen.is_tx_safe_for_mining(txid)
                    && !self.parts.is.is_locked(txid)?
                {
                    tracing::debug!(target: "chainlocks", "TrySignChainTip -- not signing block {} due to TX {txid} not being islocked and not old enough.", entry.hash);
                    return Ok(false);
                }
            }
            let Some(prev) = entry.prev else {
                return Ok(true);
            };
            match self.parts.chain.lookup(&prev) {
                Some(parent) => entry = parent,
                None if tip.height - entry.height >= TX_CONFIRM_THRESHOLD => return Ok(true),
                None => {
                    tracing::debug!(target: "chainlocks", "TrySignChainTip -- not signing: parent {prev} of block {} not found", entry.hash);
                    return Ok(false);
                }
            }
        }
    }

    /// A block's lockable txids, read from disk on a miss, which seeds their first-seen times
    /// with the block's time.
    // Core: v24.0.2 src/chainlock/signing.cpp:212-261 GetBlockTxs
    fn block_txs(&self, hash: &BlockHash) -> Option<BlockTxs> {
        if let Some(txs) = self.state.lock().block_txs.get(hash) {
            return Some(Arc::clone(txs));
        }
        tracing::debug!(target: "chainlocks", "GetBlockTxs -- blockTxs for {hash} not found. Trying ReadBlockFromDisk");
        let block = self.parts.chain.read_block(hash)?;
        let txs: BlockTxs = Arc::new(lockable_txids(&block).collect());
        self.state.lock().block_txs.entry(*hash).or_insert_with(|| Arc::clone(&txs));
        self.parts.first_seen.update(txs.iter().copied(), i64::from(block.header().time()));
        Some(txs)
    }

    /// Every 30 s while synced: drops the txids of unknown, ChainLocked and conflicting blocks,
    /// and the ChainLocked ones' first-seen times.
    // Core: v24.0.2 src/chainlock/signing.cpp:293-321 Cleanup
    pub fn cleanup(&self) {
        if !(self.parts.synced)() {
            return;
        }
        let now_ms = self.parts.clock.node_now_micros() / 1000;
        if !self.throttle.lock().try_cleanup(now_ms, CLEANUP_INTERVAL_MS) {
            return;
        }
        let (chain, cl) = (&*self.parts.chain, &*self.parts.cl);
        let mut removed = Vec::new();
        self.state.lock().block_txs.retain(|hash, txs| match chain.lookup(hash) {
            None => false,
            Some(entry) if cl.has_chainlock(entry.height, hash) => {
                removed.push(Arc::clone(txs));
                false
            }
            Some(entry) => !cl.has_conflicting_chainlock(entry.height, hash),
        });
        self.parts.first_seen.cleanup_from_signer(removed.iter().flat_map(|txs| txs.iter()));
    }
}

impl ValidationInterface for ClSigner {
    /// Schedules the tip's signing on the signer's scheduler, where Core calls it inline: the
    /// signals thread never waits, and the sign call may wait on V.
    // Core: v24.0.2 src/chainlock/signing.cpp:71-74 UpdatedBlockTip
    fn updated_block_tip(&self, _new: BlockId, _fork: Option<BlockId>, _ibd: bool) {
        let this = Weak::clone(&self.me);
        self.parts.scheduler.schedule(Box::new(move || {
            if let Some(this) = this.upgrade() {
                this.try_sign_chain_tip();
            }
        }));
    }

    /// The entry is made even for a block without lockable transactions, so the walk knows it.
    // Core: v24.0.2 src/chainlock/signing.cpp:188-211 BlockConnected
    fn block_connected(&self, block: &Arc<Block>, _id: BlockId) -> Cont {
        if (self.parts.synced)() {
            let mut state = self.state.lock();
            let txs = state.block_txs.entry(block.hash()).or_default();
            Arc::make_mut(txs).extend(lockable_txids(block));
        }
        Cont::Done
    }

    // Core: v24.0.2 src/chainlock/signing.cpp:180-185 BlockDisconnected
    fn block_disconnected(&self, block: &Arc<Block>, _id: BlockId) {
        self.state.lock().block_txs.remove(&block.hash());
    }
}

impl RecoveredSigListener for ClSigner {
    /// The ChainLock is built under `state` and processed after its release.
    // Core: v24.0.2 src/chainlock/signing.cpp:263-291 HandleNewRecoveredSig
    fn handle_new_recovered_sig(&self, sig: &RecoveredSig) -> RecoveredSigResult {
        if !self.parts.cl.enabled() {
            return RecoveredSigResult::Nothing;
        }
        let clsig = {
            let state = self.state.lock();
            if sig.id != state.last_signed_request_id
                || sig.msg_hash != state.last_signed_msg_hash
                || self.parts.cl.best_height() >= state.last_signed_height
            {
                return RecoveredSigResult::Nothing;
            }
            ClSig::new(
                state.last_signed_height,
                BlockHash::from(state.last_signed_msg_hash),
                BlsSigBytes::from(sig.sig),
            )
        };
        self.parts
            .process
            .process_local(clsig)
            .map_or(RecoveredSigResult::Nothing, RecoveredSigResult::RelayInv)
    }
}

#[cfg(test)]
#[path = "signer_tests.rs"]
mod tests;
