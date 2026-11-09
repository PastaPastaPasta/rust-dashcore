//! What LQ1 reads from a masternode list (LQ1 §2.2.1-§2.2.2, text unchanged; EV addendum §1.3).
//!
//! EV0 lands the views so the list types can implement them before LQ1a: the node's `MnList`
//! (EV0, then EV2a) and SPV's SML list (SB8). Both are materialized values, so the views are
//! infallible (DEC-109 (3)); every fallible read happens in the view that produces a list.

use std::sync::Arc;

use dash_primitives::OutPoint;

use crate::types::ProTxHash;

/// One masternode as Core's member selection sees it (a `CDeterministicMNCPtr`). Cheap to clone.
pub trait MnEntryView: Clone {
    /// `proTxHash`.
    fn pro_tx_hash(&self) -> &ProTxHash;
    /// Tie-break key. Core's `COutPoint` `operator<` (hash memcmp, then n):
    /// `primitives/transaction.h` v24:75.
    fn collateral(&self) -> &OutPoint;
    /// `MnType::Evo`. Only the Platform type filters on it, non-rotated, from V19 (LQ1 §3.4).
    fn is_evo(&self) -> bool;
    /// `pdmnState->IsBanned()`: `pose_ban_height != -1` (`evo/dmnstate.h` v24:146). The entry's
    /// **own** state: rotation inserts entries taken from older lists, whose state is the older
    /// one.
    fn is_banned(&self) -> bool;
    /// Only null-tested: unconfirmed masternodes are never scored (`llmq/utils.cpp` v24:144).
    fn confirmed_hash_is_null(&self) -> bool;
    /// The **stored** field `confirmedHashWithProRegTxHash` (single SHA256(proTx ‖ confirmed),
    /// `evo/dmnstate.h` v24:162-169), read, never recomputed (`llmq/utils.cpp` v24:72-85).
    fn confirmed_hash_with_protx(&self) -> &[u8; 32];
}

/// Forwards every method to `T` (EV addendum M1). A crate that owns its entry type `T` writes
/// `impl MnEntryView for T` and uses `Entry = Arc<T>`, which it could not implement itself: the
/// trait is this crate's and `Arc` is `std`'s (E0117).
impl<T: MnEntryView> MnEntryView for Arc<T> {
    fn pro_tx_hash(&self) -> &ProTxHash {
        (**self).pro_tx_hash()
    }
    fn collateral(&self) -> &OutPoint {
        (**self).collateral()
    }
    fn is_evo(&self) -> bool {
        (**self).is_evo()
    }
    fn is_banned(&self) -> bool {
        (**self).is_banned()
    }
    fn confirmed_hash_is_null(&self) -> bool {
        (**self).confirmed_hash_is_null()
    }
    fn confirmed_hash_with_protx(&self) -> &[u8; 32] {
        (**self).confirmed_hash_with_protx()
    }
}

/// `GetCounts().total()` and `.enabled()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListCounts {
    /// Every masternode.
    pub total: usize,
    /// The non-banned ones (`evo/deterministicmns.h` v24:146-155, 269-293).
    pub enabled: usize,
}

/// A masternode list as LQ1 reads it.
pub trait MnListView {
    /// The list's entry.
    type Entry: MnEntryView;
    /// The list type `MnsUsedAtH` sets are built in.
    type Scratch: ScratchMnList<Entry = Self::Entry>;
    /// `GetCounts()`: enabled = non-banned (`evo/deterministicmns.h` v24:146-155, 269-293).
    fn counts(&self) -> ListCounts;
    /// `HasMN`.
    fn contains(&self, p: &ProTxHash) -> bool;
    /// `IsMNPoSeBanned`: false when absent (`evo/deterministicmns.cpp` v24:66-73).
    fn is_pose_banned(&self, p: &ProTxHash) -> bool;
    /// `ForEachMNShared(onlyValid, …)`. Order unspecified: LQ1 never depends on it (LQ1 §3.4,
    /// tested in §6.5).
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Self::Entry));
    /// `CDeterministicMNList()`: an empty list.
    fn new_scratch(&self) -> Self::Scratch;
}

/// `MnsUsedAtH` / `MnsUsedAtHIndexed[i]`: Core builds them with `AddMN` and swallows its
/// exceptions (quirk 28b), so `AddMN`'s exact acceptance rule (EV2 §2.5 "try_add order and
/// failures") is consensus here.
pub trait ScratchMnList {
    /// The list's entry.
    type Entry: MnEntryView;
    /// `CDeterministicMNList::AddMN(dmn, fBumpTotalCount = true)` (`evo/deterministicmns.cpp`
    /// v24:439-517), all-or-nothing, judged on the entry's **own** state.
    fn try_add(&mut self, e: &Self::Entry) -> Result<(), AddMnRejected>;
    /// `HasMN`.
    fn contains(&self, p: &ProTxHash) -> bool;
    /// `GetCounts().total()`.
    fn total(&self) -> usize;
    /// `ForEachMNShared(onlyValid, …)`.
    fn for_each(&self, only_valid: bool, f: &mut dyn FnMut(&Self::Entry));
}

/// `AddMN` rejected the entry. The reason is never observable (Core swallows it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddMnRejected;
