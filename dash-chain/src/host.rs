//! The enforcement view (SB4 §4, §7.3, D32, D38, D39).

use dash_consensus::BlockId;

use crate::chain::HeaderChain;
use crate::error::ChainError;
use crate::residency::Residency;
use crate::store::HeaderStore;

/// The chain as Core's enforcement code sees it: `m_chain.Tip()`, `m_chain.Contains()` and
/// `DisconnectTip` (`validation.cpp:4057`, `:4087`, `:4096-4110`). Every read in §7.2/§7.3 goes
/// through it (Sol r1 M2).
///
/// Node: V implements it. `tip`/`contains` read the writer's active chain; `disconnect_tip` is
/// V's `DisconnectTip`, which ends in `chain.disconnect_tip()`. Inside `enforce_block` that step
/// is staged under the tip mutex `enforce_block` holds throughout, and published once when it
/// returns (§9.1). Callbacks run with that mutex held and must not call a mutex-taking `TipCell`
/// API (§13.4, D42).
///
/// SPV: [`VirtualTip`].
pub trait TipHost<S: HeaderStore, R: Residency> {
    type Error;

    fn tip(&self, chain: &HeaderChain<S, R>) -> BlockId;

    fn contains(&self, chain: &HeaderChain<S, R>, id: BlockId) -> Result<bool, ChainError>;

    fn disconnect_tip(&mut self, chain: &mut HeaderChain<S, R>) -> Result<(), Self::Error>;

    /// Called once at the end of every `mark_conflicting_block`, after its status writes (Core
    /// :4121) and before `enforce_block` moves to the next sibling (O-M2, DEC-136). The node does
    /// Core's :4122-4148 here, in Core's order (§7.3); `self.tip(chain)` is the tip Core passes to
    /// `UpdatedBlockTip(tip, nullptr)`.
    fn conflicting_marked(
        &mut self,
        chain: &mut HeaderChain<S, R>,
        marked: BlockId,
        was_in_chain: bool,
    ) -> Result<(), Self::Error>;
}

/// SPV's enforcement view, one per step (O-m4): a cursor that starts at the active tip.
///
/// `contains(x)` is `height(x) <= height(cursor)` and `ancestor(cursor, height(x)) == x`, over
/// arena, overlay and cold records; `disconnect_tip` sets `cursor = prev(cursor)` and publishes
/// nothing; `conflicting_marked` records the cursor as the tip of one
/// `UpdatedBlockTip(tip, nullptr)` notification. The real tip moves at stage 4 of §9.1's recipe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualTip {
    pub(crate) cursor: BlockId,
    pub(crate) disconnected: Vec<BlockId>,
    pub(crate) notified_tips: Vec<BlockId>,
}

impl VirtualTip {
    /// The enforcement tip: the active tip less the step's disconnects.
    pub fn cursor(&self) -> BlockId {
        self.cursor
    }

    /// The tip of each `UpdatedBlockTip(tip, nullptr)` the step's marks issued, in order.
    pub fn notified_tips(&self) -> &[BlockId] {
        &self.notified_tips
    }

    /// The tips the step disconnected, tip down.
    pub fn disconnected(&self) -> &[BlockId] {
        &self.disconnected
    }
}

impl<S: HeaderStore, R: Residency> TipHost<S, R> for VirtualTip {
    type Error = ChainError;

    fn tip(&self, _chain: &HeaderChain<S, R>) -> BlockId {
        self.cursor
    }

    fn contains(&self, chain: &HeaderChain<S, R>, id: BlockId) -> Result<bool, ChainError> {
        chain.on_cursor_chain(self.cursor, id)
    }

    fn disconnect_tip(&mut self, chain: &mut HeaderChain<S, R>) -> Result<(), ChainError> {
        let prev = chain.cursor_prev(self.cursor)?;
        self.disconnected.push(self.cursor);
        self.cursor = prev;
        Ok(())
    }

    fn conflicting_marked(
        &mut self,
        _chain: &mut HeaderChain<S, R>,
        _marked: BlockId,
        _was_in_chain: bool,
    ) -> Result<(), ChainError> {
        self.notified_tips.push(self.cursor);
        Ok(())
    }
}
