//! The effect channel (A1 §2.10.1, with the EV2 §3.8 and LQ1 §3.14 errata).
//!
//! Core performs some actions inline during a connect attempt, and they persist even when the
//! attempt later fails, runs with `fJustCheck`, is a GBT proposal or a VerifyDB pass. They are
//! **effects**, kept apart from the transactional `dash_dml::store::EvoDelta`:
//!
//! - **Emission.** A pure function that can produce one takes `&mut dyn EffectSink` and never
//!   returns effects inside its `Result`, so an effect emitted before an `Err` is not lost.
//! - **Application** (CH7a, CH7b): the chainstate applies an attempt's log **in emission order and
//!   regardless of the result**, before acting on the result and before the next attempt takes its
//!   read view. Each effect is persisted as Core persists it, no more durably and no less.
//! - **Durability classes** (ST3a §2.4.4, §5.6) are CH7a's runtime policy: CH7a classifies each
//!   [`EffectKind`] *presumed* (a later consensus computation reads it and coins-only recovery
//!   cannot recreate it, so a pending one needs a `sync_wal` before B1) or *independent*, in an
//!   exhaustive `match` (EV0 API review Q8). As guidance, rotation snapshots are the presumed
//!   class ST3a names; the others are independent.

use std::sync::Arc;

use dash_num::Hash256;
use dash_primitives::{BlockHash, LlmqType, QuorumHash};

use crate::llmq::{LlmqCommitment, QuorumSnapshot};
use crate::types::ProTxHash;

/// One of Core's non-transactional actions (A1 §2.10.1's inventory, verified at v24.0.0-rc.3).
///
/// Not `#[non_exhaustive]`: the chainstate's router matches every variant, so a new effect is a
/// compile error there rather than a silently dropped action.
///
/// `PartialEq` is safe: [`LlmqCommitment`]'s BLS fields are eager and compare by their decode, so
/// equality of decoded commitments is Core's `SerializedEqual` (LQ2 D1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    /// `StoreSnapshotForBlock` from the member computation (`PreComputeQuorumMembers`, a
    /// commitment check, PoSe, a quorum build): an unconditional overwrite of the `llmq_S` row
    /// keyed by (type, cycle-base hash), never erased (LQ1 §3.7). Applied as an immediate
    /// single-row Buffered batch (A1 §3.5.7; LQ1 §4.2).
    StoreQuorumSnapshot {
        llmq_type: LlmqType,
        cycle_base: BlockHash,
        snapshot: QuorumSnapshot,
    },
    /// A ChainLocked block overrides a conflicting InstantSend lock (`validation.cpp`
    /// v24:2800-2817); `dashd-locks` deletes it from `llmq/` immediately.
    RemoveConflictingIsLock {
        lock_hash: Hash256,
    },
    /// Quorum `ProcessCommitment` with `fJustCheck = false` erases the mineable commitment
    /// (`llmq/blockprocessor.cpp` v24:448-453); it is not restored when the block later fails.
    /// Core erases the pool's entry for (type, quorum hash) and the pool's commitment whose
    /// `SerializeHash` is the mined commitment's, `commitment_hash`, which need not be the one
    /// the quorum's entry names (LQ2 D7, D8).
    EraseMineableCommitment {
        llmq_type: LlmqType,
        quorum_hash: QuorumHash,
        commitment_hash: [u8; 32],
    },
    /// Undo and VerifyDB level 3 re-insert the block's commitments into the mineable pool
    /// (`llmq/blockprocessor.cpp` v24:556).
    ReinsertMineableCommitment {
        commitment: Arc<LlmqCommitment>,
    },
    /// A ProUpServTx revive resets the platform ban: `ResetPlatformBan(proTxHash, nHeight)` →
    /// `SetPlatformBan(false, height)`, which refuses a stale or equal height
    /// (`masternode/meta.cpp:187-194`; EV2 §3.8 erratum: the height is part of the effect).
    /// Emitted by EV2b's `process_block` before the `bad-dip3-enf-block` check.
    ResetPlatformBan {
        pro_tx_hash: ProTxHash,
        height: i32,
    },
    /// VerifyDB notifies the MN-list listeners, which act on the direction and the diff
    /// (`CMNAuth` disconnects MN-authenticated peers whose masternode the diff removes or whose
    /// operator key it changes, `evo/mnauth.cpp:179-210`; governance runs `CheckAndRemove`), and
    /// those actions survive VerifyDB's rolled-back evodb transaction:
    /// - level 3 disconnects block `at` through `DisconnectBlock`, which notifies with
    ///   `undo = true` (`validation.cpp` v24:2368-2370);
    /// - level 4 reconnects it through `ConnectBlock(fJustCheck = false)` (v24:5160), which
    ///   notifies with `undo = false` (v24:2918-2921).
    ///
    /// Core notifies only for the active chainstate and only when the diff has changes. The rows
    /// are never erased, so the router re-resolves the old list and the diff from `at`.
    NotifyMnListChanged {
        at: BlockHash,
        undo: bool,
    },
}

/// The kind of an [`Effect`], for CH7a's durability classification and routing tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectKind {
    StoreQuorumSnapshot,
    RemoveConflictingIsLock,
    EraseMineableCommitment,
    ReinsertMineableCommitment,
    ResetPlatformBan,
    NotifyMnListChanged,
}

impl EffectKind {
    /// Every kind, in declaration order.
    pub const ALL: [EffectKind; 6] = [
        EffectKind::StoreQuorumSnapshot,
        EffectKind::RemoveConflictingIsLock,
        EffectKind::EraseMineableCommitment,
        EffectKind::ReinsertMineableCommitment,
        EffectKind::ResetPlatformBan,
        EffectKind::NotifyMnListChanged,
    ];
}

impl Effect {
    /// The effect's kind.
    pub fn kind(&self) -> EffectKind {
        match self {
            Effect::StoreQuorumSnapshot {
                ..
            } => EffectKind::StoreQuorumSnapshot,
            Effect::RemoveConflictingIsLock {
                ..
            } => EffectKind::RemoveConflictingIsLock,
            Effect::EraseMineableCommitment {
                ..
            } => EffectKind::EraseMineableCommitment,
            Effect::ReinsertMineableCommitment {
                ..
            } => EffectKind::ReinsertMineableCommitment,
            Effect::ResetPlatformBan {
                ..
            } => EffectKind::ResetPlatformBan,
            Effect::NotifyMnListChanged {
                ..
            } => EffectKind::NotifyMnListChanged,
        }
    }
}

/// Where a pure function records the effects it produces, in Core's order (A1 §2.10.1).
///
/// An implementation only records: it never fails and never reorders. The chainstate's
/// attempt-scoped sinks may also update an overlay that later reads in the same attempt see
/// (LQ1 §3.7.2's `AttemptRows`), as Core's raw writes are visible to the next read.
pub trait EffectSink {
    /// Records `effect` after every effect recorded before it.
    fn emit(&mut self, effect: Effect);
}

/// The plain effect log: an attempt's effects in emission order.
impl EffectSink for Vec<Effect> {
    fn emit(&mut self, effect: Effect) {
        self.push(effect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_keeps_emission_order() {
        let mut log: Vec<Effect> = Vec::new();
        let sink: &mut dyn EffectSink = &mut log;
        sink.emit(Effect::NotifyMnListChanged {
            at: BlockHash::from([2u8; 32]),
            undo: false,
        });
        sink.emit(Effect::ResetPlatformBan {
            pro_tx_hash: ProTxHash::from([1u8; 32]),
            height: 7,
        });
        let kinds: Vec<_> = log.iter().map(Effect::kind).collect();
        assert_eq!(kinds, [EffectKind::NotifyMnListChanged, EffectKind::ResetPlatformBan]);
    }

    #[test]
    fn every_kind_is_listed_once() {
        // The match is exhaustive, so a new kind must be given the next position here, and the
        // positions must then be exactly ALL's.
        let position = |kind: EffectKind| match kind {
            EffectKind::StoreQuorumSnapshot => 0,
            EffectKind::RemoveConflictingIsLock => 1,
            EffectKind::EraseMineableCommitment => 2,
            EffectKind::ReinsertMineableCommitment => 3,
            EffectKind::ResetPlatformBan => 4,
            EffectKind::NotifyMnListChanged => 5,
        };
        let positions: Vec<_> = EffectKind::ALL.iter().map(|k| position(*k)).collect();
        assert_eq!(positions, (0..6).collect::<Vec<_>>());
    }
}
