//! The decision seam (CH3 §3.3, D18): what `ConnectBlock` asks Core's `m_chain_helper`, and
//! readiness guard G2's predicate (CH3 §2.17).

use dash_consensus::Tx;
use dash_evo::payments::GovernanceView;
use dash_evo::{Effect, EffectSink};
use dash_num::Hash256;
use dash_params::ChainParams;
use dash_primitives::{BlockHash, TxHash, TxOut};

use super::evo::BlockAt;
use crate::chain::{ConnectCx, Fatal, GuardScope};

/// An InstantSend lock a transaction conflicts with: Core's `(islock hash, txid)`
/// (`evo/chainhelper.cpp:79-85`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IsLockRef {
    /// `::SerializeHash(*islock)`.
    pub hash: Hash256,
    /// The locked transaction.
    pub txid: TxHash,
}

/// O8 §7.1's queries, exactly, as `ConnectBlock` asks them (CH3 §3.3, D18): a recorded helper
/// answers from O8's decision stream, and a decision miss is a [`Fatal`] that stops the replay.
/// The live answers read V's state through the call's [`ConnectCx`].
pub trait ChainHelper: Send {
    /// `HasConflictingChainLock(pindex->nHeight, hash)` (`validation.cpp:2516`).
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal>;

    /// `IsInitialBlockDownload()` (`:2801`).
    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal>;

    /// `HasChainLock(pindex->nHeight, hash)` (`:2803`).
    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal>;

    /// `ConflictingISLockIfAny(tx)` (`:2806`), the loop condition: the only query that advances a
    /// recorded cursor (D35). It excludes every lock in this attempt's removed view.
    fn is_conflict(&mut self, tx: &Tx) -> Result<Option<IsLockRef>, Fatal>;

    /// `RemoveConflictingISLockByTx(tx)` (`:2810`) on the lock `is_conflict` just returned: emits
    /// `Effect::RemoveConflictingIsLock`, and adds Core's removal closure (the lock and its chained
    /// descendants, `instantsend.cpp:339-350`) to the attempt's removed view. It answers no query
    /// and consumes no recorded entry.
    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink);

    /// A `ConnectBlock` begins for the block `at` (CH3a D125): the helper's attempt scope opens,
    /// so a proposal's ChainLock and pre-check rejections open none. Its O8 key is `at.hash` and
    /// `cx.origin` (O8 §7.2); [`Self::end_attempt`] closes it.
    ///
    /// # Errors
    ///
    /// A recorded helper's decision miss (no such attempt): the replay stops.
    fn begin_attempt(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<(), Fatal> {
        let _ = (cx, at);
        Ok(())
    }

    /// The attempt is over (its log applied or failed): the removed view ends here, on every
    /// path (CH3a D76). The connector calls it right after `finish_attempt`, also after an attempt
    /// whose `ConnectBlock` never began.
    fn end_attempt(&mut self) {}

    /// `IsSuperblockValidationRequired(pindex)` (`:2841`; `evo/chainhelper.cpp:41-52`).
    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal>;

    /// `m_superblocks.IsValid()`: governance has its data (`masternode/payments.cpp:355`, `:404`).
    fn gov_valid(&mut self) -> Result<bool, Fatal>;

    /// `IsSuperblockTriggered(nBlockHeight)` (`governance/superblock.cpp:578`).
    fn sb_triggered(&mut self, h: u32) -> Result<bool, Fatal>;

    /// The best superblock's payments at `h` (`GetSuperblockPayments`,
    /// `governance/superblock.cpp:632-650`), which EV3's `IsValidSuperblock` checks the coinbase
    /// against; `None` with no best superblock.
    fn sb_payments(&mut self, h: u32) -> Result<Option<Vec<TxOut>>, Fatal>;

    /// Readiness guard G2 (CH3 §2.17, D31): the capability this build lacks to judge the block at
    /// `h`, which the connector refuses. Not an O8 query and never recorded.
    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        let _ = (cx, h);
        None
    }
}

/// [`ChainHelper`]'s governance answers as EV3's [`GovernanceView`] (CH3a D62): a query that cannot
/// be answered (a recorded replay's decision miss) stops the connect as a [`Fatal`].
pub(crate) struct HelperGov<'h>(pub &'h mut dyn ChainHelper);

impl GovernanceView for HelperGov<'_> {
    type Error = Fatal;

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        self.0.gov_valid()
    }

    fn sb_triggered(&mut self, height: u32) -> Result<bool, Fatal> {
        self.0.sb_triggered(height)
    }

    fn sb_payments(&mut self, height: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        self.0.sb_payments(height)
    }
}

/// What G2 names as missing (CH3 §2.17): the connector's refusal text carries it.
pub const INTERIM_REFUSAL: &str = "superblock and budget validation (SP1, GV1b)";

// Core: governance/superblock.cpp:119-124 CSuperblock::IsValidBlockHeight;
// masternode/payments.cpp:262-295 IsOldBudgetBlockValueValid's window
/// Whether `h` is where Core with a finished masternode sync could judge the coinbase's value
/// differently (CH3 §2.17): a superblock height, or a height inside an old-budget payment window.
pub fn is_governance_height(params: &ChainParams, h: u32) -> bool {
    let c = &params.consensus;
    let h = i64::from(h);
    let (sb_start, sb_cycle) =
        (i64::from(c.superblock_start.0.to_u32()), i64::from(c.superblock_cycle));
    let (budget_start, budget_cycle, budget_window) = (
        i64::from(c.budget_payments_start_block.to_u32()),
        i64::from(c.budget_payments_cycle_blocks),
        i64::from(c.budget_payments_window_blocks),
    );
    let superblock = h >= sb_start && sb_cycle > 0 && h % sb_cycle == 0;
    let budget =
        h >= budget_start && h < sb_start && budget_cycle > 0 && h % budget_cycle < budget_window;
    superblock || budget
}

// Core: evo/chainhelper.cpp:47-49
/// `IsSuperblockValidationRequired`'s line for a node whose masternode sync has not finished.
fn log_not_synced(at: &BlockAt) {
    tracing::debug!(
        target: "mnpayments",
        "IsSuperblockValidationRequired -- WARNING! Node is not fully synced, checked superblock \
         for block={} max bounds only",
        at.hash
    );
}

/// The production helper (M3; CH3 §3.3). ChainLocks come from V's state through the call's
/// [`ConnectCx`] (LK1a feeds it; a node without LK1a has none), `ibd` from V's latch there, and
/// no InstantSend lock conflicts until the IS owner lands. Masternode sync (SP1) and governance
/// (GV1b) are not built: `mn_synced` and `gov_valid` are false, which is Core with
/// `-disablegovernance` and an unfinished sync (n1, D18), and G2 refuses where that differs.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeHelper;

impl NodeHelper {
    /// `mn_sync.IsSynced()`, false until SP1 lands.
    const MN_SYNCED: bool = false;

    /// SP1 and GV1b, which lift G2, are built.
    const GOVERNANCE_BUILT: bool = false;
}

impl ChainHelper for NodeHelper {
    fn cl_conflict(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        cx.chainlocks.has_conflicting_chainlock(h, hash)
    }

    fn ibd(&mut self, cx: &ConnectCx<'_>) -> Result<bool, Fatal> {
        Ok(cx.is_ibd)
    }

    fn cl_has(&mut self, cx: &ConnectCx<'_>, h: u32, hash: &BlockHash) -> Result<bool, Fatal> {
        cx.chainlocks.has_chainlock(h, hash)
    }

    fn is_conflict(&mut self, _: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        Ok(None)
    }

    fn remove_conflicting_is_lock(&mut self, lock: &IsLockRef, sink: &mut dyn EffectSink) {
        sink.emit(Effect::RemoveConflictingIsLock {
            lock_hash: lock.hash,
        });
    }

    // Core: evo/chainhelper.cpp:41-52 CChainstateHelper::IsSuperblockValidationRequired
    fn sb_required(&mut self, cx: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        if i64::from(cx.chainlocks.best_height()) >= i64::from(at.height) {
            tracing::debug!(
                target: "mnpayments",
                "IsSuperblockValidationRequired -- validation of chainlocked block={} is skipped",
                at.hash
            );
            return Ok(false);
        }
        if !Self::MN_SYNCED {
            log_not_synced(at);
            return Ok(false);
        }
        Ok(true)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        Ok(None)
    }

    /// G2: a live network, past IBD, at a governance height not ChainLocked yet, without SP1 and
    /// GV1b. IBD and reindex pass, as Core is in the same not-synced regime there; regtest and
    /// devnet keep D18's declared regime.
    fn interim_refusal(&mut self, cx: &ConnectCx<'_>, h: u32) -> Option<&'static str> {
        let refused = cx.guard == GuardScope::Live
            && !cx.is_ibd
            && is_governance_height(cx.params, h)
            && i64::from(cx.chainlocks.best_height()) < i64::from(h)
            && !Self::GOVERNANCE_BUILT;
        refused.then_some(INTERIM_REFUSAL)
    }
}

/// The helper for L2 mainnet and testnet replays (CH3 §3.3): Core's answers during
/// `-reindex-chainstate` of a node with no ChainLocks and an unfinished masternode sync. They
/// hold for the pre-DIP3 windows, where no ChainLock exists; `gov_valid`'s answer cannot change a
/// verdict on the `NoCheck` path, the only one this helper yields.
#[derive(Clone, Copy, Debug, Default)]
pub struct IbdHelper;

impl ChainHelper for IbdHelper {
    fn cl_conflict(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn ibd(&mut self, _: &ConnectCx<'_>) -> Result<bool, Fatal> {
        Ok(true)
    }

    fn cl_has(&mut self, _: &ConnectCx<'_>, _: u32, _: &BlockHash) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn is_conflict(&mut self, _: &Tx) -> Result<Option<IsLockRef>, Fatal> {
        Ok(None)
    }

    /// Never asked: `is_conflict` reports no lock.
    fn remove_conflicting_is_lock(&mut self, _: &IsLockRef, _: &mut dyn EffectSink) {
        debug_assert!(false, "IbdHelper reports no InstantSend conflict");
    }

    /// Core's unsynced answer with its line: no ChainLock covers the block (there are none).
    fn sb_required(&mut self, _: &ConnectCx<'_>, at: &BlockAt) -> Result<bool, Fatal> {
        log_not_synced(at);
        Ok(false)
    }

    fn gov_valid(&mut self) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_triggered(&mut self, _: u32) -> Result<bool, Fatal> {
        Ok(false)
    }

    fn sb_payments(&mut self, _: u32) -> Result<Option<Vec<TxOut>>, Fatal> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use dash_params::{ChainParamsBuilder, NetworkKind, Release};

    use super::*;

    fn params(network: NetworkKind) -> ChainParams {
        ChainParamsBuilder::new(network, Release::V24_0).build().expect("Core's parameters")
    }

    /// Core's two sets at their boundaries, from each network's own parameters.
    #[test]
    fn governance_heights_are_superblocks_and_old_budget_windows() {
        for network in [NetworkKind::Mainnet, NetworkKind::Testnet, NetworkKind::Regtest] {
            let p = params(network);
            let c = &p.consensus;
            let (start, cycle) = (c.superblock_start.0.to_u32(), c.superblock_cycle as u32);
            let first = start.div_ceil(cycle) * cycle;
            assert!(is_governance_height(&p, first), "{network:?}: the first superblock");
            assert!(!is_governance_height(&p, first + 1), "{network:?}");
            assert!(is_governance_height(&p, first + cycle), "{network:?}");
            let (b_start, b_cycle, b_window) = (
                c.budget_payments_start_block.to_u32(),
                c.budget_payments_cycle_blocks as u32,
                c.budget_payments_window_blocks as u32,
            );
            // A cycle multiple below the start is no superblock: only a budget window holds it.
            if first >= cycle && first - cycle < start {
                let h = first - cycle;
                let window = h >= b_start && h % b_cycle < b_window;
                assert_eq!(is_governance_height(&p, h), window, "{network:?}: below the start");
            }
            // The first whole budget window at or above the start that ends below the superblocks.
            let w = b_start.div_ceil(b_cycle) * b_cycle;
            if w + b_cycle < start {
                assert!(is_governance_height(&p, w), "{network:?}: a window's first height");
                assert!(is_governance_height(&p, w + b_window - 1), "{network:?}: its last");
                assert!(!is_governance_height(&p, w + b_window), "{network:?}: past it");
            }
            if b_start > 0 {
                assert!(!is_governance_height(&p, b_start - 1), "{network:?}: before budgets");
            }
        }
    }

    /// Mainnet's literal values (`chainparams.cpp`): superblocks every 16616 blocks from 614820;
    /// budget windows of 100 blocks every 16616 from 328008.
    #[test]
    fn mainnet_governance_heights() {
        let p = params(NetworkKind::Mainnet);
        assert!(is_governance_height(&p, 38 * 16_616));
        assert!(!is_governance_height(&p, 38 * 16_616 + 1));
        assert!(is_governance_height(&p, 36 * 16_616), "below 614820: an old-budget window");
        assert!(!is_governance_height(&p, 36 * 16_616 + 100), "past that window");
        assert!(is_governance_height(&p, 20 * 16_616 + 99), "inside 332320's window");
        assert!(!is_governance_height(&p, 20 * 16_616 + 100));
        assert!(!is_governance_height(&p, 19 * 16_616), "below 328008: no budgets yet");
    }

    #[test]
    fn ibd_helper_answers_reindex() {
        let mut h = IbdHelper;
        assert_eq!(h.gov_valid(), Ok(false));
        assert_eq!(h.sb_triggered(0), Ok(false));
        assert_eq!(h.sb_payments(0), Ok(None));
    }

    #[test]
    fn node_helper_emits_the_removal() {
        let lock = IsLockRef {
            hash: Hash256::from([7; 32]),
            txid: TxHash::from([9; 32]),
        };
        let mut log = Vec::new();
        NodeHelper.remove_conflicting_is_lock(&lock, &mut log);
        assert_eq!(
            log,
            [Effect::RemoveConflictingIsLock {
                lock_hash: lock.hash
            }]
        );
    }
}
