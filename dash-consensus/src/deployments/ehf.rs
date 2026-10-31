//! The EHF signals version bits read: Core's `AbstractEHFManager::Signals`, the source that
//! holds them per block (`CMNHFManager::GetForBlock`, implemented by the node's MNHF manager), and
//! `CMNHFManager::GetSignalsStage` over it.

use core::fmt;

use super::Deployments;
use super::versionbits::VERSIONBITS_NUM_BITS;
use crate::ctx::BlockId;

const SLOTS: usize = VERSIONBITS_NUM_BITS as usize;

/// Core's `AbstractEHFManager::Signals` (`std::map<uint8_t, int>`): for each version bit with a
/// signal, the height of the block that mined it.
///
/// Only bits below `VERSIONBITS_NUM_BITS` can hold a signal (`MNHFTxPayload::IsTriviallyValid`
/// rejects the others), so the map is a fixed array and building one allocates nothing.
/// Iteration is in bit order, as the `std::map`'s, and visits only the bits with a signal: a
/// scan of every slot cost 5 to 8 instructions per empty one, whichever the codegen unit split
/// (T30 PF0 deployments-per-block-warm).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EhfSignals {
    /// Bit `b` is set when `heights[b]` holds a signal.
    bits: u32,
    /// A slot without a signal stays 0 (nothing removes one), so the derives compare the maps.
    heights: [i32; SLOTS],
}

const _: () = assert!(SLOTS <= u32::BITS as usize);

impl EhfSignals {
    pub const fn new() -> Self {
        Self {
            bits: 0,
            heights: [0; SLOTS],
        }
    }

    /// The height of `bit`'s signal (`find`).
    pub fn get(&self, bit: u8) -> Option<i32> {
        let slot = usize::from(bit);
        (slot < SLOTS && self.bits & (1 << slot) != 0).then(|| self.heights[slot])
    }

    /// `insert({bit, height})`: keeps an existing signal for the bit, as `std::map::insert` does.
    ///
    /// # Panics
    /// If `bit` is not below `VERSIONBITS_NUM_BITS`.
    pub fn insert(&mut self, bit: u8, height: i32) {
        let slot = usize::from(bit);
        assert!(slot < SLOTS, "an EHF signal's bit is below VERSIONBITS_NUM_BITS");
        if self.bits & (1 << slot) == 0 {
            self.bits |= 1 << slot;
            self.heights[slot] = height;
        }
    }

    /// `(bit, height)` in bit order.
    pub fn iter(&self) -> impl Iterator<Item = (u8, i32)> + '_ {
        let mut bits = self.bits;
        core::iter::from_fn(move || {
            if bits == 0 {
                return None;
            }
            let bit = bits.trailing_zeros();
            bits &= bits - 1;
            // `insert` sets bits below SLOTS (29) only, so both casts are lossless.
            Some((bit as u8, self.heights[bit as usize]))
        })
    }

    pub fn len(&self) -> usize {
        self.bits.count_ones() as usize
    }

    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }
}

impl fmt::Debug for EhfSignals {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl FromIterator<(u8, i32)> for EhfSignals {
    fn from_iter<I: IntoIterator<Item = (u8, i32)>>(iter: I) -> Self {
        let mut signals = Self::new();
        for (bit, height) in iter {
            signals.insert(bit, height);
        }
        signals
    }
}

/// The signals each block's state holds: Core's `CMNHFManager::GetForBlock(pindex)`, the map
/// `ProcessBlock` stored for the block. That is not every signal mined in the block's ancestry:
/// `ProcessBlock` stores `GetSignalsStage(pprev)` (the previous block's map with expired signals
/// dropped, empty before V20) plus the block's own new signals at its height. So:
///
/// - a signal whose block's MTP is below the start time of a deployment on its bit (one mined
///   before that start, which `IsValidMNActivation` accepts as an unknown fork bit) is in its own
///   block's map and drops out of the next block's;
/// - its bit can then be signalled again, and the map from that block on holds the new height,
///   not the first one. A source that collects every mined signal with [`EhfSignals::insert`]
///   (which keeps the first height) would keep the expired one, and the stage filter would then
///   drop the bit: the deployment would never start.
///
/// Contract: the recurrence above, every height at most the block's own, and a result that is a
/// function of the block alone. A source that cannot read a block's signals panics, as Core's
/// `GetForBlock` throws out of its caller: a fallback answer would stay in the
/// [`VersionBitsCache`](super::VersionBitsCache) for good. [`Deployments`](super::Deployments)
/// calls it with that cache's lock held, so it must not query versionbits itself.
// Core: evo/mnhftx.cpp:210-247 CMNHFManager::ProcessBlock
pub trait EhfSignalSource: Sync {
    // Core: evo/mnhftx.cpp:285 CMNHFManager::GetForBlock
    fn signals_for_block(&self, block: BlockId) -> EhfSignals;
}

/// A source with no signals: every chain before its first EHF signal.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEhfSignals;

impl EhfSignalSource for NoEhfSignals {
    fn signals_for_block(&self, _block: BlockId) -> EhfSignals {
        EhfSignals::new()
    }
}

impl Deployments<'_> {
    // Core: evo/mnhftx.cpp:61-92 CMNHFManager::GetSignalsStage
    pub fn ehf_signals_stage(&self, prev: Option<BlockId>) -> EhfSignals {
        let Some(prev) = self.ehf_stage_parent(prev) else {
            return EhfSignals::new();
        };
        self.ehf_unexpired(prev, self.ehf.signals_for_block(prev))
    }

    /// [`Self::ehf_signals_stage`] with `prev`'s stored map from `read` rather than the
    /// [`EhfSignalSource`], whose contract is to panic on a fault: a caller with a fallible
    /// store (the MNHF manager's `ProcessBlock`) gets the read's error instead.
    // Core: evo/mnhftx.cpp:61-92 CMNHFManager::GetSignalsStage
    pub fn ehf_signals_stage_from<E>(
        &self,
        prev: Option<BlockId>,
        read: impl FnOnce(BlockId) -> Result<EhfSignals, E>,
    ) -> Result<EhfSignals, E> {
        let Some(prev) = self.ehf_stage_parent(prev) else {
            return Ok(EhfSignals::new());
        };
        Ok(self.ehf_unexpired(prev, read(prev)?))
    }

    /// The block whose stored map the stage reads: `prev`, where V20 is active after it.
    #[inline]
    fn ehf_stage_parent(&self, prev: Option<BlockId>) -> Option<BlockId> {
        if !self.buried_active_after(prev, dash_params::BuriedDeployment::V20) {
            return None;
        }
        prev
    }

    /// `prev`'s stored map without the signals that expired: one may be mined in `prev` itself
    /// and already expired.
    #[inline]
    fn ehf_unexpired(&self, prev: BlockId, signals_tmp: EhfSignals) -> EhfSignals {
        let mut signals_ret = EhfSignals::new();

        for (bit, signal_height) in signals_tmp.iter() {
            let mut expired = false;
            let signal_index = self
                .ancestor(prev, signal_height)
                .expect("GetSignalsStage: assert(signal_pindex != nullptr)");
            let signal_time = self.mtp(signal_index);
            for deployment in &self.params.consensus.deployments {
                if deployment.bit != i32::from(bit) {
                    continue;
                }
                if signal_time < deployment.start_time {
                    expired = true;
                }
            }
            if !expired {
                signals_ret.insert(bit, signal_height);
            }
        }
        signals_ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `std::map<uint8_t, int>` semantics the bitmask keeps: bit order, the first height of
    /// a bit, `find` on any bit, and equality of the maps whatever the insertion order.
    #[test]
    fn signals_are_a_map_in_bit_order() {
        let mut s = EhfSignals::new();
        assert!(s.is_empty());
        s.insert(28, 7);
        s.insert(3, 5);
        s.insert(0, 9);
        s.insert(3, 6);
        assert_eq!(s.iter().collect::<Vec<_>>(), [(0, 9), (3, 5), (28, 7)]);
        assert_eq!(
            (s.len(), s.get(3), s.get(1), s.get(29), s.get(u8::MAX)),
            (3, Some(5), None, None, None)
        );
        assert_eq!(s, [(28, 7), (0, 9), (3, 5)].into_iter().collect());
        assert_ne!(s, [(28, 7), (0, 9), (3, 6)].into_iter().collect());
        // Presence is the bit, not the height: a signal at height 0 is not an empty map.
        let mut zero = EhfSignals::new();
        zero.insert(5, 0);
        assert_eq!((zero.get(5), zero.len()), (Some(0), 1));
        assert_ne!(zero, EhfSignals::new());
    }

    #[test]
    #[should_panic(expected = "below VERSIONBITS_NUM_BITS")]
    fn insert_refuses_a_bit_past_the_map() {
        EhfSignals::new().insert(29, 1);
    }
}
