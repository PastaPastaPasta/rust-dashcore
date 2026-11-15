//! Core's walks over `pprev`/`pskip` (SB4 §3.3, §10.1): skip heights, `GetAncestor`,
//! `LastCommonAncestor` and `GetMedianTimePast`, over any source of [`Link`]s, so the resident
//! arena and (in SPV) the cold tier share one port.

use dash_consensus::BlockId;

/// The pointers of one entry that the walks read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) height: u32,
    pub(crate) prev: Option<BlockId>,
    pub(crate) skip: Option<BlockId>,
}

/// Core's `InvertLowestOne`: clears the lowest set bit.
// Core: chain.cpp:83 InvertLowestOne
fn invert_lowest_one(n: u32) -> u32 {
    n & n.wrapping_sub(1)
}

/// Core's `GetSkipHeight`: the height `pskip` jumps back to.
// Core: chain.cpp:86-95 GetSkipHeight
pub(crate) fn skip_height(height: u32) -> u32 {
    if height < 2 {
        return 0;
    }
    match height & 1 {
        1 => invert_lowest_one(invert_lowest_one(height - 1)) + 1,
        _ => invert_lowest_one(height),
    }
}

/// Core's `CBlockIndex::GetAncestor`: `None` when `height` is above `id`'s height. Follows
/// `pskip` unless `pprev`'s skip is the better jump, exactly as Core does, so it visits the same
/// entries. `link` fails only when an entry cannot be read. Each link must land where Core's
/// `BuildSkip` and `pprev` put it, `GetSkipHeight` or one lower; `off_height(x)` is the error
/// for an `x` that does not (a malformed record), so no stored pointer can loop the walk. A skip
/// to the right height on another branch is not detectable here (heights are all a link shows).
/// Stored `prev`s are also checked at the source (`Shared::cold_parent`); this keeps the walk
/// safe on its own.
// Core: chain.cpp:97-119 CBlockIndex::GetAncestor
pub(crate) fn ancestor<E>(
    id: BlockId,
    height: u32,
    mut link: impl FnMut(BlockId) -> Result<Link, E>,
    mut off_height: impl FnMut(BlockId) -> E,
) -> Result<Option<BlockId>, E> {
    let mut cur = link(id)?;
    if height > cur.height {
        return Ok(None);
    }
    let mut walk = id;
    // Core's arithmetic is on `int`; `height_skip - 2` can be negative.
    let target = i64::from(height);
    while cur.height > height {
        let h_skip = i64::from(skip_height(cur.height));
        let h_skip_prev = i64::from(skip_height(cur.height - 1));
        let (next, at) = match cur.skip {
            Some(skip)
                if h_skip == target
                    || (h_skip > target
                        && !(h_skip_prev < h_skip - 2 && h_skip_prev >= target)) =>
            {
                (skip, skip_height(cur.height))
            }
            _ => (cur.prev.expect("Core asserts pprev below an entry's height"), cur.height - 1),
        };
        cur = link_at(next, at, &mut link, &mut off_height)?;
        walk = next;
    }
    Ok(Some(walk))
}

/// Core's `BuildSkip`: `pskip = pprev->GetAncestor(GetSkipHeight(nHeight))`.
// Core: chain.cpp:126-130 CBlockIndex::BuildSkip
pub(crate) fn build_skip<E>(
    prev: Option<BlockId>,
    height: u32,
    link: impl FnMut(BlockId) -> Result<Link, E>,
    off_height: impl FnMut(BlockId) -> E,
) -> Result<Option<BlockId>, E> {
    match prev {
        Some(prev) => ancestor(prev, skip_height(height), link, off_height),
        None => Ok(None),
    }
}

/// Core's `LastCommonAncestor`. Core asserts that the branches meet; `None` here means they
/// share no ancestor `link` can read (SPV's cold tier ends at the anchor). Each `pprev` must
/// step one height down, `off_height` as for [`ancestor`].
// Core: chain.cpp:166-181 LastCommonAncestor
pub(crate) fn last_common_ancestor<E>(
    a: BlockId,
    b: BlockId,
    mut link: impl FnMut(BlockId) -> Result<Link, E>,
    mut off_height: impl FnMut(BlockId) -> E,
) -> Result<Option<BlockId>, E> {
    let (ha, hb) = (link(a)?.height, link(b)?.height);
    let mut a = match ha > hb {
        true => ancestor(a, hb, &mut link, &mut off_height)?,
        false => Some(a),
    };
    let mut b = match hb > ha {
        true => ancestor(b, ha, &mut link, &mut off_height)?,
        false => Some(b),
    };
    let mut h = ha.min(hb);
    while let (Some(x), Some(y)) = (a, b) {
        if x == y {
            return Ok(Some(x));
        }
        let lx = link_at(x, h, &mut link, &mut off_height)?;
        let ly = link_at(y, h, &mut link, &mut off_height)?;
        (a, b, h) = (lx.prev, ly.prev, h.wrapping_sub(1));
    }
    Ok(None)
}

/// `link(id)`, which must sit at `height`; `off_height(id)` if it does not.
fn link_at<E>(
    id: BlockId,
    height: u32,
    link: &mut impl FnMut(BlockId) -> Result<Link, E>,
    off_height: &mut impl FnMut(BlockId) -> E,
) -> Result<Link, E> {
    let l = link(id)?;
    match l.height == height {
        true => Ok(l),
        false => Err(off_height(id)),
    }
}

/// For walks over resident entries only: `BuildSkip` set their pointers, so none is off its
/// height (Core asserts as much).
pub(crate) fn resident_off_height<E>(id: BlockId) -> E {
    panic!("resident entry {} is off the height its link names", id.0)
}

/// The heights `LocatorEntries` visits from an entry at `height`: ten single steps back, then
/// steps that double, then genesis. Each is an ancestor of the start, so `GetAncestor` from the
/// start reaches the same entries as Core's walk from the previous one.
// Core: chain.cpp:32-49 LocatorEntries
pub(crate) fn locator_heights(height: u32) -> Vec<u32> {
    let mut have = Vec::with_capacity(32);
    let mut step = 1u32;
    let mut h = height;
    loop {
        have.push(h);
        if h == 0 {
            return have;
        }
        h = h.saturating_sub(step);
        if have.len() > 10 {
            step = step.saturating_mul(2);
        }
    }
}

/// Core's `CBlockIndex::nMedianTimeSpan`.
pub(crate) const MEDIAN_TIME_SPAN: usize = 11;

/// Core's `GetMedianTimePast` over the block's time and its ancestors' times, newest first: the
/// middle of up to eleven sorted times (the upper middle of an even count).
// Core: chain.h:283-295 CBlockIndex::GetMedianTimePast
pub(crate) fn median_time_past(times: impl IntoIterator<Item = u32>) -> i64 {
    let mut buf = [0i64; MEDIAN_TIME_SPAN];
    let mut n = 0;
    for t in times.into_iter().take(MEDIAN_TIME_SPAN) {
        buf[n] = i64::from(t);
        n += 1;
    }
    let sorted = &mut buf[..n];
    sorted.sort_unstable();
    sorted[n / 2]
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use super::*;

    /// A chain of `n` entries, ids = heights, with Core's skip pointers.
    fn chain(n: u32) -> Vec<Link> {
        let mut links: Vec<Link> = Vec::with_capacity(n as usize);
        for h in 0..n {
            let prev = h.checked_sub(1).map(BlockId);
            let skip = build_skip(
                prev,
                h,
                |id| Ok::<_, Infallible>(links[id.0 as usize]),
                resident_off_height,
            )
            .unwrap();
            links.push(Link {
                height: h,
                prev,
                skip,
            });
        }
        links
    }

    /// Core's `skiplist_tests` `skiplist_test` invariants: `pskip` is the entry at
    /// `GetSkipHeight`, strictly lower, and `GetAncestor` lands on the requested height.
    #[test]
    fn skip_pointers_and_ancestors_match_core_invariants() {
        let links = chain(300_000);
        for (h, l) in links.iter().enumerate().skip(1) {
            let skip = l.skip.expect("every entry above genesis has a skip");
            assert_eq!(skip.0, skip_height(h as u32));
            assert!(skip.0 < h as u32);
        }
        let mut state = 0x0005_b4c2_u64;
        for _ in 0..10_000 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let from = (state >> 33) as u32 % 300_000;
            let to = (state >> 13) as u32 % (from + 1);
            let link = |id: BlockId| Ok::<_, Infallible>(links[id.0 as usize]);
            let got = ancestor(BlockId(from), to, link, resident_off_height);
            assert_eq!(got, Ok(Some(BlockId(to))));
        }
        assert_eq!(
            ancestor(
                BlockId(5),
                6,
                |id| Ok::<_, Infallible>(links[id.0 as usize]),
                resident_off_height
            ),
            Ok(None)
        );
    }

    /// Core's comment on `GetSkipHeight`: at most 110 steps back over 2^18 blocks.
    #[test]
    fn ancestor_takes_at_most_110_steps_over_2_pow_18() {
        let links = chain(1 << 18);
        let mut worst = 0;
        for from in (0..1u32 << 18).step_by(97) {
            for to in [0, from / 3, from / 2, from.saturating_sub(1)] {
                let mut steps = 0;
                let link = |id: BlockId| {
                    steps += 1;
                    Ok::<_, Infallible>(links[id.0 as usize])
                };
                ancestor(BlockId(from), to, link, resident_off_height).unwrap();
                worst = worst.max(steps - 1);
            }
        }
        assert!(worst <= 110, "{worst} steps");
    }

    #[test]
    fn skip_heights_of_small_values() {
        // Core: 0 and 1 jump to 0; even heights clear their lowest bit; odd heights clear the
        // two lowest bits of height - 1 and add one.
        let got: Vec<u32> = (0..12).map(skip_height).collect();
        assert_eq!(got, [0, 0, 0, 1, 0, 1, 4, 1, 0, 1, 8, 1]);
    }

    #[test]
    fn last_common_ancestor_of_two_branches() {
        // 0..=9 is the trunk; 10..=14 branch from 5 at heights 6..=10.
        let mut links = chain(10);
        for k in 0..5u32 {
            let prev = BlockId(if k == 0 {
                5
            } else {
                9 + k
            });
            let h = 6 + k;
            let skip = build_skip(
                Some(prev),
                h,
                |id| Ok::<_, Infallible>(links[id.0 as usize]),
                resident_off_height,
            )
            .unwrap();
            links.push(Link {
                height: h,
                prev: Some(prev),
                skip,
            });
        }
        let link = |id: BlockId| Ok::<_, Infallible>(links[id.0 as usize]);
        let lca = |a, b| last_common_ancestor(BlockId(a), BlockId(b), link, resident_off_height);
        assert_eq!(lca(14, 9), Ok(Some(BlockId(5))));
        assert_eq!(lca(3, 14), Ok(Some(BlockId(3))));
        assert_eq!(lca(12, 12), Ok(Some(BlockId(12))));
    }

    /// Sol r1 m1: a stored pointer that does not step down is corruption, not a loop: a skip
    /// naming its own entry (`Corrupt::BadSkip`), a skip at the wrong height, and a `pprev`
    /// that climbs back up, in `GetAncestor` and in `LastCommonAncestor`'s single steps.
    #[test]
    fn links_off_their_height_are_refused() {
        let base = chain(64);
        let walk = |links: &[Link], from, to| {
            let mut reads = 0;
            let link = |id: BlockId| {
                reads += 1;
                assert!(reads < 100, "the walk loops");
                Ok(links[id.0 as usize])
            };
            ancestor(BlockId(from), to, link, |id| id)
        };
        assert_eq!(walk(&base, 40, 32), Ok(Some(BlockId(32))));
        let mut links = base.clone();
        links[2].skip = Some(BlockId(2));
        assert_eq!(walk(&links, 2, 0), Err(BlockId(2)), "a self skip");
        let mut links = base.clone();
        links[40].skip = Some(BlockId(33));
        assert_eq!(walk(&links, 40, 32), Err(BlockId(33)), "GetSkipHeight(40) is 32");
        let mut links = base.clone();
        links[20].prev = Some(BlockId(21));
        assert_eq!(walk(&links, 21, 19), Err(BlockId(21)), "a pprev one up");
        // A second entry at height 20 (id 64): the walks meet only below 20's bad pprev.
        links.push(Link {
            prev: Some(BlockId(19)),
            ..base[20]
        });
        let link = |id: BlockId| Ok(links[id.0 as usize]);
        let lca = last_common_ancestor(BlockId(20), BlockId(64), link, |id| id);
        assert_eq!(lca, Err(BlockId(21)), "LastCommonAncestor's pprev steps");
    }

    #[test]
    fn locator_heights_step_like_core() {
        let mut want: Vec<u32> = (290..=300).rev().collect();
        want.extend([289, 287, 283, 275, 259, 227, 163, 35, 0]);
        assert_eq!(locator_heights(300), want);
        assert_eq!(locator_heights(0), [0]);
        assert_eq!(locator_heights(10), (0..=10).rev().collect::<Vec<_>>());
        assert_eq!(locator_heights(11), [11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(locator_heights(u32::MAX).last(), Some(&0));
    }

    #[test]
    fn median_time_past_is_cores_middle_element() {
        assert_eq!(median_time_past([7]), 7);
        assert_eq!(median_time_past([3, 1]), 3); // sorted [1, 3], index 1
        assert_eq!(median_time_past([5, 1, 9, 3]), 5); // sorted [1, 3, 5, 9], index 2
        // Only the newest eleven count.
        assert_eq!(median_time_past((1..=20).rev()), 15); // 20..=10 sorted, index 5
    }
}
