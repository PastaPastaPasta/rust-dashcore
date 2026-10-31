//! Rotation snapshots (Core `CQuorumSnapshot`, `llmq/snapshot.h`).

/// `SnapshotSkipMode` (`llmq/snapshot.h` v24:31-36).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotSkipMode {
    /// `MODE_NO_SKIPPING` (0).
    NoSkipping,
    /// `MODE_SKIPPING_ENTRIES` (1).
    SkippingEntries,
    /// `MODE_NO_SKIPPING_ENTRIES` (2).
    NoSkippingEntries,
    /// `MODE_ALL_SKIPPED` (3).
    AllSkipped,
}

/// Core's `CQuorumSnapshot`: the members of a rotation cycle that were active, and the skip list.
///
/// The value of an `llmq_S` row and of [`crate::effect::Effect::StoreQuorumSnapshot`]. The
/// mode is held as Core's raw `int`, because Core decodes any value (`is_serializable_enum`) and
/// a stored row must re-serialize as it was read; [`QuorumSnapshot::skip_mode`] names the four
/// defined ones. BS8 owns the wire codec (LQ1 §2.4); when it lands, this becomes its type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuorumSnapshot {
    /// `activeQuorumMembers`.
    pub active_quorum_members: Vec<bool>,
    /// `mnSkipListMode`, raw.
    pub skip_list_mode: i32,
    /// `mnSkipList`.
    pub skip_list: Vec<i32>,
}

impl QuorumSnapshot {
    /// The skip mode, if it is one of Core's four.
    pub fn skip_mode(&self) -> Option<SnapshotSkipMode> {
        match self.skip_list_mode {
            0 => Some(SnapshotSkipMode::NoSkipping),
            1 => Some(SnapshotSkipMode::SkippingEntries),
            2 => Some(SnapshotSkipMode::NoSkippingEntries),
            3 => Some(SnapshotSkipMode::AllSkipped),
            _ => None,
        }
    }
}
