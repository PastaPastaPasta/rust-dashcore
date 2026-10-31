//! MNHF signals (EV3-3): Core's `CMNHFManager` (`evo/mnhftx.cpp`) over the evo store.
//!
//! - [`process_signals`] is `ProcessBlock`'s checks and result for a block, given the stage it is
//!   checked against; [`is_valid_mn_activation`] is `CChainParams::IsValidMNActivation`.
//! - [`MnhfManager`] is the node's one manager (D16): Core's 1000-entry cache, which lives as long
//!   as the process. [`MnhfView`] puts it over a block index and an evo store: `ProcessBlock`,
//!   `UndoBlock`, and `GetForBlock` as the node's [`EhfSignalSource`], which
//!   [`Deployments::ehf_signals_stage`] (`GetSignalsStage`) reads.
//!
//! The row a block stores is [`MnhfProcessed::row`]: its signals, for every block where V20 is
//! active (D15). The store keeps it in `EvoDelta::mnhf_signals`, keyed by hash. A row missing
//! where Core would read one is a store fault, never a rebuild from block files (D17), and never
//! a verdict: [`MnhfView::process_block`] returns it as [`MnhfError::Store`] (D21).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

use dash_consensus::ctx::{BlockId, ChainAncestry, HeaderCtx};
pub use dash_consensus::deployments::EhfSignals;
use dash_consensus::deployments::{
    Deployments, EhfSignalSource, VERSIONBITS_NUM_BITS, VersionBitsCache,
};
use dash_consensus::{Block, BlockValidationError, BlockValidationResult, Tx};
use dash_params::{BuriedDeployment, ChainParams};
use dash_primitives::{BlockHash, SpecialPayload, TxType};

use crate::store::{EvoStore, StoreError};

fn reject(reason: &'static str) -> BlockValidationError {
    BlockValidationError::new(BlockValidationResult::Consensus, reason)
}

/// `IsValidMNActivation(nBit, timePast)`: the first deployment on the bit whose window holds
/// `time_past` decides by `useEHF`; a bit no such deployment uses is valid (quirk 31).
///
/// # Panics
/// If `bit` is not below `VERSIONBITS_NUM_BITS` (Core's `assert`; `IsTriviallyValid` rejects
/// such a signal first, D18).
// Core: chainparams.cpp:109-130 CChainParams::IsValidMNActivation
pub fn is_valid_mn_activation(params: &ChainParams, bit: u8, time_past: i64) -> bool {
    assert!(i32::from(bit) < VERSIONBITS_NUM_BITS, "IsValidMNActivation: bit {bit}");

    for deployment in &params.consensus.deployments {
        if deployment.bit == i32::from(bit) {
            if time_past > deployment.timeout || time_past < deployment.start_time {
                // Core logs "activation by bit=%d deployment='%s' is out of time range"
                continue;
            }
            // Core logs "trying to set MnEHF for non-masternode activation fork" when false
            return deployment.use_ehf;
        }
    }
    // Core logs "WARNING: unknown MnEHF fork bit=%d"
    true
}

/// `extractEHFSignal(tx)`: the version bit of an MNHF signal whose payload decodes completely.
// Core: evo/mnhftx.cpp:176-188 extractEHFSignal
fn ehf_signal(tx: &Tx) -> Option<u8> {
    if !tx.is_special_tx_version() || tx.tx_type() != TxType::MnhfSignal {
        // only interested in special TXs 'TRANSACTION_MNHF_SIGNAL'
        return None;
    }
    match tx.payload() {
        Some(Ok(decoded)) if decoded.is_complete() => match &decoded.payload {
            SpecialPayload::MnhfSignal(payload) => Some(payload.version_bit),
            _ => None,
        },
        _ => None,
    }
}

/// `extractSignals(block)`: the version bits of the block's MNHF signals after the coinbase, in
/// block order; `bad-mnhf-duplicates-in-block` for a bit twice.
// Core: evo/mnhftx.cpp:190-208 extractSignals
pub fn extract_signals(block: &Block) -> Result<Vec<u8>, BlockValidationError> {
    let mut new_signals = Vec::new();
    // we skip the coinbase
    for tx in block.txs().iter().skip(1) {
        let Some(bit) = ehf_signal(tx) else {
            continue;
        };
        if new_signals.contains(&bit) {
            return Err(reject("bad-mnhf-duplicates-in-block"));
        }
        new_signals.push(bit);
    }
    Ok(new_signals)
}

/// `ProcessBlock`'s checks and result for the block at `height` with median time past `mtp`:
/// the stage (`GetSignalsStage(pprev)`, read after the block's signals are extracted, as Core
/// reads it), plus the block's new signals at `height` unless `just_check`. The caching and the
/// row are [`MnhfView::process_block`]'s.
///
/// A signal bit at or above `VERSIONBITS_NUM_BITS` never reaches Core's `ProcessBlock`:
/// `ProcessSpecialTxsInBlock` runs `CheckSpecialTxInner`, so `CheckMNHFTx`, on every transaction
/// first (`evo/specialtxman.cpp:853-871`), whose `IsTriviallyValid` rejects it. Here it gets that
/// reject reason, before any other check, rather than `IsValidMNActivation`'s `assert`. The
/// caller still runs EV1's `check_mnhf_tx` first (A-2), so the verdict is the same either way.
// Core: evo/mnhftx.cpp:210-251 CMNHFManager::ProcessBlock
pub fn process_signals<E>(
    block: &Block,
    height: i32,
    mtp: i64,
    params: &ChainParams,
    just_check: bool,
    stage: impl FnOnce() -> Result<EhfSignals, E>,
) -> Result<EhfSignals, MnhfError<E>> {
    let out_of_bounds = |tx: &Tx| ehf_signal(tx).is_some_and(|bit| bit >= NUM_BITS);
    if block.txs().iter().skip(1).any(|tx| out_of_bounds(tx)) {
        // Core: evo/mnhftx.cpp:99-101 MNHFTxPayload::IsTriviallyValid
        return Err(MnhfError::Invalid(reject("bad-mnhf-nbit-out-of-bounds")));
    }
    let new_signals = extract_signals(block).map_err(MnhfError::Invalid)?;
    let mut signals = stage().map_err(MnhfError::Store)?;
    if new_signals.is_empty() {
        return Ok(signals);
    }

    // Extra validation of signals to be sure that it can succeed
    for &version_bit in &new_signals {
        if signals.get(version_bit).is_some() {
            return Err(MnhfError::Invalid(reject("bad-mnhf-duplicate")));
        }
        if !is_valid_mn_activation(params, version_bit, mtp) {
            return Err(MnhfError::Invalid(reject("bad-mnhf-non-mn-fork")));
        }
    }
    if just_check {
        // We are done, no need actually update any params
        return Ok(signals);
    }
    for &version_bit in &new_signals {
        signals.insert(version_bit, height);
    }
    Ok(signals)
}

/// `VERSIONBITS_NUM_BITS`, as a signal's bit.
const NUM_BITS: u8 = VERSIONBITS_NUM_BITS as u8;

/// What stops [`MnhfView::process_block`] (D21): Core's verdict on the block, or a fault of the
/// store it reads, which is never a verdict (Core's `EvoDbInconsistencyError`, `state.Error`,
/// `evo/mnhftx.cpp:252-256`). `E` is the stage read's error: [`StoreError`] for the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MnhfError<E = StoreError> {
    /// Core's `state.Invalid(BLOCK_CONSENSUS, reason)`.
    Invalid(BlockValidationError),
    /// A local store fault: a node error, never a block verdict.
    Store(E),
}

impl<E> MnhfError<E> {
    /// The block's reject reason; `None` for a store fault.
    pub fn reject_reason(&self) -> Option<&str> {
        match self {
            MnhfError::Invalid(e) => Some(e.reject_reason()),
            MnhfError::Store(_) => None,
        }
    }
}

impl<E: fmt::Display> fmt::Display for MnhfError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MnhfError::Invalid(e) => write!(f, "{e}"),
            MnhfError::Store(e) => write!(f, "{e}"),
        }
    }
}

impl<E: fmt::Debug + fmt::Display> std::error::Error for MnhfError<E> {}

/// `MNHFCacheSize`.
// Core: evo/mnhftx.h:104
const CACHE_SIZE: usize = 1000;

/// Core's `unordered_lru_cache<uint256, Signals>` of [`CACHE_SIZE`]: every `get` and `insert`
/// stamps the entry with the next access count; past twice the size, only the [`CACHE_SIZE`]
/// most recently stamped entries stay. Stamps are unique, so which entries stay is Core's.
// Core: unordered_lru_cache.h:15-118
#[derive(Debug, Default)]
struct SignalsCache {
    map: BTreeMap<BlockHash, (EhfSignals, i64)>,
    access_counter: i64,
}

impl SignalsCache {
    fn stamp(&mut self) -> i64 {
        let stamp = self.access_counter;
        self.access_counter += 1;
        stamp
    }

    /// A miss takes no stamp, as in Core.
    fn get(&mut self, key: &BlockHash) -> Option<EhfSignals> {
        let stamp = self.access_counter;
        let entry = self.map.get_mut(key)?;
        entry.1 = stamp;
        self.access_counter += 1;
        Some(entry.0)
    }

    fn insert(&mut self, key: BlockHash, signals: EhfSignals) {
        let stamp = self.stamp();
        self.map.insert(key, (signals, stamp));
        self.truncate_if_needed();
    }

    fn truncate_if_needed(&mut self) {
        if self.map.len() <= 2 * CACHE_SIZE {
            return;
        }
        // Core's nth_element: the CACHE_SIZE-th most recent stamp
        let mut stamps: Vec<i64> = self.map.values().map(|(_, stamp)| *stamp).collect();
        let (_, nth, _) = stamps.select_nth_unstable_by(CACHE_SIZE - 1, |a, b| b.cmp(a));
        let oldest_kept = *nth;
        self.map.retain(|_, (_, stamp)| *stamp >= oldest_kept);
    }
}

/// The node's MNHF manager: Core's cache of each block's signals (D16). One instance lives as
/// long as the node process and is never persisted; [`MnhfView`] reads and fills it.
#[derive(Debug, Default)]
pub struct MnhfManager {
    cache: Mutex<SignalsCache>,
}

impl MnhfManager {
    pub fn new() -> Self {
        Self::default()
    }

    fn cache(&self) -> MutexGuard<'_, SignalsCache> {
        // A panic while the lock was held cannot leave an entry half-written.
        self.cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The number of cached blocks.
    #[cfg(test)]
    fn cached(&self) -> usize {
        self.cache().map.len()
    }
}

/// What [`MnhfView::process_block`] accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MnhfProcessed {
    /// `ProcessBlock`'s result: the block's signals, or with `just_check` the stage alone.
    pub signals: EhfSignals,
    /// The row to store for the block (`EvoDelta::mnhf_signals`): its signals where V20 is
    /// active at it and the block is connected, not just checked (D15).
    pub row: Option<EhfSignals>,
}

/// [`MnhfManager`] over a block index and an evo store: the node's `CMNHFManager`.
#[derive(Clone, Copy)]
pub struct MnhfView<'a> {
    pub manager: &'a MnhfManager,
    pub params: &'a ChainParams,
    pub chain: &'a dyn ChainAncestry,
    pub store: &'a (dyn EvoStore + Sync),
}

impl MnhfView<'_> {
    /// `DeploymentActiveAt(block, V20)`.
    fn v20_active_at(&self, header: &HeaderCtx) -> bool {
        header.height >= self.params.consensus.deployment_height(BuriedDeployment::V20).to_u32()
    }

    /// `GetSignalsStage(prev)`: the signals a block on `prev` is checked against; a fault reading
    /// `prev`'s row is the store's (D21). The versionbits cache is a throwaway: the stage reads
    /// no deployment state.
    // Core: evo/mnhftx.cpp:61-92 CMNHFManager::GetSignalsStage
    pub fn signals_stage(&self, prev: Option<BlockId>) -> Result<EhfSignals, StoreError> {
        let cache = VersionBitsCache::new();
        Deployments::new(self.params, self.chain, self, &cache)
            .ehf_signals_stage_from(prev, |block| self.get_for_block(block))
    }

    /// `GetFromCache(block)`: the cache, else the empty map where V20 is not active at the block
    /// (cached), else the block's row (cached); `None` where the row is missing. The lock is not
    /// held across the store read, as Core drops `cs_cache` around it. Core's legacy `mnhf_s`
    /// key is the evodb importer's (D17).
    // Core: evo/mnhftx.cpp:323-362 CMNHFManager::GetFromCache
    fn get_from_cache(&self, block: BlockId) -> Result<Option<EhfSignals>, StoreError> {
        let header = self.chain.header(block);
        {
            let mut cache = self.manager.cache();
            if let Some(signals) = cache.get(&header.hash) {
                return Ok(Some(signals));
            }
            if !self.v20_active_at(&header) {
                cache.insert(header.hash, EhfSignals::new());
                return Ok(Some(EhfSignals::new()));
            }
        }
        let Some(signals) = self.store.mnhf_signals(&header.hash)? else {
            return Ok(None);
        };
        self.manager.cache().insert(header.hash, signals);
        Ok(Some(signals))
    }

    /// `GetForBlock(block)`: the signals for `block` as the next block's stage reads them: the
    /// cached entry, else the empty map where V20 is not active at it, else its row. Where Core
    /// would re-process the block from disk, the row is missing: a store fault (D17).
    // Core: evo/mnhftx.cpp:285-321 CMNHFManager::GetForBlock
    pub fn get_for_block(&self, block: BlockId) -> Result<EhfSignals, StoreError> {
        self.get_from_cache(block)?
            .ok_or_else(|| StoreError::inconsistent("no MNHF row for a block where V20 is active"))
    }

    /// `ProcessBlock(block, pindex, fJustCheck)` for `block` at `id`, whose parent's row the store
    /// or the cache holds. Unless `just_check`, caches the block's signals (`AddToCache`) and
    /// returns its row for the store where V20 is active at it (D15, D16). A fault reading the
    /// parent's row is [`MnhfError::Store`], never a verdict (D21); a signal bit out of range is
    /// rejected (see [`process_signals`]), but EV1's `check_mnhf_tx` runs first (A-2).
    // Core: evo/mnhftx.cpp:210-262 CMNHFManager::ProcessBlock
    pub fn process_block(
        &self,
        block: &Block,
        id: BlockId,
        just_check: bool,
    ) -> Result<MnhfProcessed, MnhfError> {
        let header = self.chain.header(id);
        let height = i32::try_from(header.height).expect("block heights are Core's int");
        let prev = self.chain.prev(id);
        let signals = process_signals(block, height, header.mtp, self.params, just_check, || {
            self.signals_stage(prev)
        })?;
        if just_check {
            return Ok(MnhfProcessed {
                signals,
                row: None,
            });
        }
        Ok(MnhfProcessed {
            signals,
            row: self.add_to_cache(signals, &header),
        })
    }

    /// `AddToCache(signals, block)`: caches them, and gives the row where V20 is active at the
    /// block (Core's `WriteDerived`; the store refuses a different row, D15).
    // Core: evo/mnhftx.cpp:364-383 CMNHFManager::AddToCache
    fn add_to_cache(&self, signals: EhfSignals, header: &HeaderCtx) -> Option<EhfSignals> {
        self.manager.cache().insert(header.hash, signals);
        self.v20_active_at(header).then_some(signals)
    }

    /// `AddToCache(signals, block)` alone, for differential harnesses that seed Core's manager
    /// the same way (the oracle's `seed_tip_cache`). The node never calls it.
    #[doc(hidden)]
    pub fn seed_cache(&self, block: BlockId, signals: EhfSignals) {
        self.add_to_cache(signals, &self.chain.header(block));
    }

    /// `UndoBlock(block, pindex)`: `false` where the block's signals do not extract (a bit
    /// twice); else each signal the block mined is in its row, or Core's `assert` fails, a store
    /// fault (D15).
    // Core: evo/mnhftx.cpp:264-283 CMNHFManager::UndoBlock
    pub fn undo_block(&self, block: &Block, id: BlockId) -> Result<bool, StoreError> {
        let Ok(excluded_signals) = extract_signals(block) else {
            // Core logs "failed to extract signals"
            return Ok(false);
        };
        if excluded_signals.is_empty() {
            return Ok(true);
        }

        let signals = self.get_for_block(id)?;
        for version_bit in excluded_signals {
            if signals.get(version_bit).is_none() {
                return Err(StoreError::inconsistent(
                    "UndoBlock: a mined signal is not in its row",
                ));
            }
        }
        Ok(true)
    }
}

/// `GetForBlock`, as versionbits reads it (D20). A store fault panics: CS4's contract, since a
/// fallback would stay in the versionbits cache. `ProcessBlock`'s own stage read does not come
/// through here: it returns the fault ([`MnhfView::signals_stage`], D21).
impl EhfSignalSource for MnhfView<'_> {
    fn signals_for_block(&self, block: BlockId) -> EhfSignals {
        self.get_for_block(block).unwrap_or_else(|e| panic!("GetForBlock: {e}"))
    }
}

#[cfg(test)]
mod tests;
