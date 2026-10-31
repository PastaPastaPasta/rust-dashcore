//! Anchors and the initial-sync candidate rules (SB0 §4.3, §4.3.1 steps 1–2; SB4 §11).

use dash_clock::NodeSeconds;
use dash_params::ChainSetup;
use dash_primitives::{BlockHash, BlockHeader};

use crate::Work;

/// Where a chain starts. Set at construction and never moved by any `HeaderChain` method
/// (§11.3): the driver clears the store and constructs a new chain instead.
///
/// No setter and no public field (A-13), in the node's and SPV's residency:
///
/// ```compile_fail,E0599
/// # use dash_chain::{Anchor, HeaderChain};
/// fn move_anchor(chain: &mut HeaderChain) {
///     chain.set_anchor(Anchor::Genesis);
/// }
/// ```
///
/// ```compile_fail,E0599
/// # use dash_chain::{Anchor, Evicting, HeaderChain, NoStore};
/// fn move_anchor(chain: &mut HeaderChain<NoStore, Evicting>) {
///     chain.set_anchor(Anchor::Genesis);
/// }
/// ```
///
/// ```compile_fail
/// # use dash_chain::{Anchor, HeaderChain};
/// fn move_anchor(chain: &mut HeaderChain) {
///     chain.anchor = Anchor::Genesis;
/// }
/// ```
///
/// ```compile_fail
/// # use dash_chain::{Anchor, Evicting, HeaderChain, NoStore};
/// fn move_anchor(chain: &mut HeaderChain<NoStore, Evicting>) {
///     chain.anchor = Anchor::Genesis;
/// }
/// ```
///
/// The same imports and signatures compile, so the four above fail only on the missing API:
///
/// ```
/// # use dash_chain::{Anchor, Evicting, HeaderChain, NoStore};
/// fn keep_anchor(_node: &mut HeaderChain, _spv: &mut HeaderChain<NoStore, Evicting>, _a: Anchor) {}
/// ```
///
/// A-13's other half is the public-API audit: `tests/public-api.txt` lists the crate's public API
/// and `contrib/node/check-public-api.sh` fails when it changes, so a new path to the anchor
/// shows up in review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    Genesis,
    Checkpoint {
        height: u32,
        hash: BlockHash,
        /// Absolute chain work at the anchor.
        chain_work: Work,
        /// `[0]` is anchor − 24; `[24]` is the anchor.
        window: Box<[BlockHeader; 25]>,
    },
}

/// The context entries below an anchor (§8.4): DGW's window, held for the rules but never
/// accepted.
pub(crate) const CONTEXT_ENTRIES: u32 = 24;

/// The floor, the lowest height held, of a chain whose lowest accepted height is `anchor`:
/// `anchor − 24`, or genesis (0) from genesis (§8.4).
pub(crate) fn floor_below(anchor: u32) -> u32 {
    // `check_pack` refuses a pack below DGW's window, so an anchor is genesis or above it.
    debug_assert!(anchor == 0 || anchor >= CONTEXT_ENTRIES, "an anchor at {anchor}");
    anchor.saturating_sub(CONTEXT_ENTRIES)
}

/// A shipped anchor pack: a post-DGW checkpoint with its absolute chain work and the 24 headers
/// below it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorPack {
    pub height: u32,
    pub hash: BlockHash,
    pub chain_work: Work,
    /// `[0]` is anchor − 24; `[24]` is the anchor.
    pub headers: [BlockHeader; 25],
}

impl AnchorPack {
    /// The pack as a checkpoint anchor.
    fn anchor(&self) -> Anchor {
        Anchor::Checkpoint {
            height: self.height,
            hash: self.hash,
            chain_work: self.chain_work,
            window: Box::new(self.headers),
        }
    }
}

/// Why the initial sync needs an earlier anchor (SB0 §4.3.1 step 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoverageMiss {
    /// The lowest height the active quorums need that the anchor does not cover.
    LowestUncovered(u32),
    /// A quorum base below the anchor is unknown.
    UnknownBase,
}

/// SB0 §4.3.1 step 1: the newest pack with `height >= dgw_height + 24`, `height <= birth` (when
/// set) and `time(anchor) <= now - 2 · history_blocks · pow_target_spacing`; else `Genesis`.
/// `history_blocks` = `L_est + H_keep`, from the host (D26).
pub fn anchor_candidate(
    packs: &[AnchorPack],
    params: &ChainSetup,
    birth: Option<u32>,
    now: NodeSeconds,
    history_blocks: u32,
) -> Anchor {
    let spacing = params.consensus.pow_target_spacing;
    let latest_time =
        now.secs().saturating_sub(spacing.saturating_mul(2 * i64::from(history_blocks)));
    newest(packs, params, |p| {
        birth.is_none_or(|b| p.height <= b) && i64::from(p.headers[24].time) <= latest_time
    })
}

/// SB0 §4.3.1 step 2: `LowestUncovered(h)` → the newest pack at or below `h`; `UnknownBase` →
/// the newest pack strictly below `current`; `Genesis` when none. Never a pack at or above
/// `current`, so each fallback moves the anchor down.
pub fn anchor_fallback(
    packs: &[AnchorPack],
    params: &ChainSetup,
    current: &Anchor,
    miss: CoverageMiss,
) -> Anchor {
    let Anchor::Checkpoint {
        height: current,
        ..
    } = *current
    else {
        return Anchor::Genesis;
    };
    newest(packs, params, |p| {
        p.height < current
            && match miss {
                CoverageMiss::LowestUncovered(h) => p.height <= h,
                CoverageMiss::UnknownBase => true,
            }
    })
}

/// The highest post-DGW pack (`height >= dgw_height + 24`, so every header from anchor + 1 is in
/// the DGW era, §11.1) that `keep` accepts, as an anchor; else `Genesis`.
fn newest(packs: &[AnchorPack], params: &ChainSetup, keep: impl Fn(&AnchorPack) -> bool) -> Anchor {
    let dgw_era = params.consensus.pow_dgw_height.to_u32().saturating_add(24);
    packs
        .iter()
        .filter(|p| p.height >= dgw_era && keep(p))
        .max_by_key(|p| p.height)
        .map_or(Anchor::Genesis, AnchorPack::anchor)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use dash_params::Network;

    use super::*;
    use crate::testing::{fake_hash, params};

    /// Mainnet's DGW era starts at 34,140; one block is 150 s.
    const DGW_ERA: u32 = 34_140 + 24;
    const SPACING: i64 = 150;
    /// `L_est + H_keep` for the cases (D26).
    const HISTORY: u32 = 4_352;
    /// `2 · history_blocks · pow_target_spacing`.
    const AGE: i64 = 2 * HISTORY as i64 * SPACING;

    /// A pack at `height` whose anchor header has `time`.
    fn pack(height: u32, time: u32) -> AnchorPack {
        let mut headers = [BlockHeader::default(); 25];
        for (k, h) in headers.iter_mut().enumerate() {
            h.time = time - (24 - k as u32) * SPACING as u32;
        }
        AnchorPack {
            height,
            hash: fake_hash(u64::from(height)),
            chain_work: Work::from(u64::from(height)),
            headers,
        }
    }

    fn height(a: &Anchor) -> Option<u32> {
        match a {
            Anchor::Genesis => None,
            Anchor::Checkpoint {
                height,
                ..
            } => Some(*height),
        }
    }

    /// Three packs 500,000 blocks apart, given out of order.
    fn packs() -> (Vec<AnchorPack>, i64) {
        let t3: u32 = 1_700_000_000;
        let gap = 500_000 * SPACING as u32;
        let ps =
            vec![pack(1_500_000, t3 - gap), pack(2_000_000, t3), pack(1_000_000, t3 - 2 * gap)];
        (ps, i64::from(t3))
    }

    #[test]
    fn anchor_candidate_cases() {
        let p = params(Network::Main);
        assert_eq!(p.consensus.pow_dgw_height.to_u32() + 24, DGW_ERA);
        assert_eq!(p.consensus.pow_target_spacing, SPACING);
        let (ps, t3) = packs();
        let pick =
            |birth, now| height(&anchor_candidate(&ps, &p, birth, NodeSeconds(now), HISTORY));

        // The tip at exactly the newest checkpoint, and 100 blocks past it: the time rule
        // rejects the newest pack, so the previous one is the candidate.
        assert_eq!(pick(None, t3), Some(1_500_000));
        assert_eq!(pick(None, t3 + 100 * SPACING), Some(1_500_000));
        // The newest pack once it is old enough, and not a second before.
        assert_eq!(pick(None, t3 + AGE), Some(2_000_000));
        assert_eq!(pick(None, t3 + AGE - 1), Some(1_500_000));
        assert_eq!(
            anchor_candidate(&ps, &p, None, NodeSeconds(t3 + AGE), HISTORY),
            ps[1].anchor(),
            "the anchor carries the pack's hash, work and window"
        );

        // The birth-height cut is inclusive.
        let late = t3 + AGE;
        assert_eq!(pick(Some(2_000_000), late), Some(2_000_000));
        assert_eq!(pick(Some(1_999_999), late), Some(1_500_000));
        assert_eq!(pick(Some(1_000_000), late), Some(1_000_000));
        assert_eq!(pick(Some(999_999), late), None);

        // No pack, or none old enough → Genesis.
        assert_eq!(anchor_candidate(&[], &p, None, NodeSeconds(late), HISTORY), Anchor::Genesis);
        assert_eq!(pick(None, t3 - 2 * 500_000 * SPACING), None);
        // `now` far in the past saturates instead of overflowing.
        assert_eq!(pick(None, i64::MIN), None);

        // A pre-DGW pack is never chosen; the first DGW-era height is.
        let old = [pack(DGW_ERA - 1, 1_400_000_000)];
        assert_eq!(anchor_candidate(&old, &p, None, NodeSeconds(late), HISTORY), Anchor::Genesis);
        let first = [pack(DGW_ERA, 1_400_000_000)];
        assert_eq!(
            height(&anchor_candidate(&first, &p, None, NodeSeconds(late), HISTORY)),
            Some(DGW_ERA)
        );

        // Fallback from the newest pack.
        let fall = |current: &Anchor, miss| height(&anchor_fallback(&ps, &p, current, miss));
        let newest = ps[1].anchor();
        assert_eq!(fall(&newest, CoverageMiss::LowestUncovered(1_700_000)), Some(1_500_000));
        assert_eq!(fall(&newest, CoverageMiss::LowestUncovered(1_500_000)), Some(1_500_000));
        assert_eq!(fall(&newest, CoverageMiss::LowestUncovered(1_499_999)), Some(1_000_000));
        assert_eq!(fall(&newest, CoverageMiss::LowestUncovered(10)), None);
        // Never the current pack or above it, even for a height the anchor covers.
        assert_eq!(fall(&newest, CoverageMiss::LowestUncovered(2_000_000)), Some(1_500_000));
        assert_eq!(fall(&newest, CoverageMiss::UnknownBase), Some(1_500_000));
        assert_eq!(fall(&ps[0].anchor(), CoverageMiss::UnknownBase), Some(1_000_000));
        assert_eq!(fall(&ps[2].anchor(), CoverageMiss::UnknownBase), None);
        // From Genesis there is nowhere lower; pre-DGW packs stay out.
        assert_eq!(fall(&Anchor::Genesis, CoverageMiss::UnknownBase), None);
        let low = [pack(DGW_ERA - 1, 1_400_000_000), ps[1].clone()];
        assert_eq!(anchor_fallback(&low, &p, &newest, CoverageMiss::UnknownBase), Anchor::Genesis);
    }
}
