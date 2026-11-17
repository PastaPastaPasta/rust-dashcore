//! The advisory tie model (O2b-3 P3; the O2b-2 D6/D8 constraint): which ops could reach
//! Core's address tie-break, so that the generator defers, retargets or drops them. Core's
//! POST `tie` stays authoritative (D30); the census counts the steps this model let through
//! that Core then tainted, by site.
//!
//! Every block the generator indexes has regtest's bits, so equal work is equal height. Core
//! compares two distinct entries by address only when their work and sequence ids are equal,
//! and only loaded, header-only and unlinked (stored under a chain without data) entries share
//! an id (0).

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use super::world::{Block, Held, World};

/// A tie site whose order decided an outcome (D30): Core's oracle hook's number, or one the
/// harness reads itself. The two never share a key: Core's numbers are its hook's to assign (1-14
/// at the pin, and a later pin may add more), so a harness site is named, not numbered. Core's
/// come first in the census's order. Debug prints as Display, so a list reads `[13, h1]`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TieSite {
    /// The number the oracle's POST `tie` reports (oracle.h).
    Core(u32),
    /// A harness-side site, by its name (`h1`, ...).
    Harness(&'static str),
}

impl fmt::Display for TieSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TieSite::Core(n) => write!(f, "{n}"),
            TieSite::Harness(name) => f.write_str(name),
        }
    }
}

impl fmt::Debug for TieSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// The harness-side tie sites, each read by the harness, never reported by Core's hook:
///
/// - `h1`, "load best_invalid" ([`LOAD_BEST_INVALID`]).
pub const HARNESS_SITES: [TieSite; 1] = [LOAD_BEST_INVALID];

/// Harness site `h1`, "load best_invalid" (OD-L3-97, D58): a load that leaves two or more failed
/// entries at the most work a failed entry has. Core keeps the first of them in its block map's
/// order as `m_best_invalid`, which no port reproduces; the oracle reports no tie for it unless
/// the load's reset could reach one (Core's site 12). The Diff judge reads it from the node's
/// index once every other plane, Core's `index` digest included, is the same
/// (`diff::judge`). It relies on (a) that digest covering each entry's hash, height and
/// failure bits (`node::index_digest`, `node::INDEX_STATUS_MASK`), and (b) the
/// constant bits this module's doc states, so the most failed work is the greatest failed
/// height, counted by height alone. A builder whose bits vary breaks (b), and a debug build's
/// assert in the count then trips.
// Core: src/validation.cpp:5376-5377 at v24.0.0
pub const LOAD_BEST_INVALID: TieSite = TieSite::Harness("h1");

/// Entries the node holds that tie-break by address against each other, by height (≥ 2 at a
/// height).
fn seq_zero_heights(world: &World) -> BTreeMap<u32, usize> {
    let mut by_height = BTreeMap::new();
    for b in &world.blocks {
        if b.indexed() && b.seq_zero() {
            *by_height.entry(b.height).or_insert(0usize) += 1;
        }
    }
    by_height.retain(|_, n| *n >= 2);
    by_height
}

/// G-T2: whether an op whose comparisons reach heights from `from` up could meet a seq-0
/// pair (sites 3–9).
pub fn risky_from(world: &World, from: u32) -> bool {
    seq_zero_heights(world).range(from..).next().is_some()
}

/// G-T1: how many blocks the active chain must gain before a load, so that its tip strictly
/// leads every other entry (sites 1 and 2: the load's best chain and best header; site
/// 12: the load reconsiders the best failed entry when it is ahead of the tip, so failed and
/// ChainLock-conflicting entries count too). The one exception is the input's own
/// INVALIDATE with no consensus failure (rule 2's case): a single such top may be
/// reconsidered by the load, which is a row (`invalidate_undone_by_restart`); two level at the
/// top, or one level with another entry, still count.
pub fn restart_deficit(world: &World) -> u32 {
    deficit(world, world.tip, |b| (b.held, b.invalid))
}

/// G-T1 for a KILL: [`restart_deficit`] of what the load reads, the entries as the last write
/// left them ([`World::lose_dirty`]), with the active chain's highest stored one as the tip
/// (blocks since are lost).
pub fn kill_deficit(world: &World) -> u32 {
    let lost = |b: &Block| {
        if b.dirty() {
            b.durable.unwrap_or((Held::Nothing, false))
        } else {
            (b.held, b.invalid)
        }
    };
    let tip = world
        .ancestry(world.tip)
        .find(|&a| lost(&world.blocks[a]) == (Held::Data, false))
        .unwrap_or(0);
    deficit(world, tip, lost)
}

/// [`restart_deficit`] with the active tip `tip` and each entry's `(held, invalid)` read through
/// `entry`, in one pass over the blocks: the active chain is marked once, and whether an own
/// INVALIDATE is at or below a block is kept per block, so no block walks its ancestry.
fn deficit(world: &World, tip: usize, entry: impl Fn(&Block) -> (Held, bool)) -> u32 {
    // `World::is_ancestor(i, tip)`: `i` is the first block at its height on `tip`'s ancestry.
    let mut active: HashMap<u32, usize> = HashMap::new();
    for a in world.ancestry(tip) {
        active.entry(world.blocks[a].height).or_insert(a);
    }
    let marks = !world.consensus_failure && !world.own_invalidates.is_empty();
    let mut undone = vec![None; world.blocks.len()];
    let tip_height = world.blocks[tip].height;
    let (mut lead, mut failed) = (None, Vec::new());
    for (i, b) in world.blocks.iter().enumerate() {
        let (held, invalid) = entry(b);
        if !matches!(held, Held::Header | Held::Data) || active.get(&b.height) == Some(&i) {
            continue;
        }
        if invalid && marks && own_invalidate_at_or_below(world, i, &mut undone) {
            failed.push(b.height);
        } else {
            lead = lead.max(Some(b.height));
        }
    }
    if let Some(&top) = failed.iter().max() {
        let level = failed.iter().filter(|&&h| h == top).count() >= 2 || lead == Some(top);
        if level {
            lead = lead.max(Some(top));
        }
    }
    match lead {
        Some(h) if h >= tip_height => h - tip_height + 1,
        _ => 0,
    }
}

/// Whether `id` or an ancestor is one of the input's own INVALIDATEs, memoised in `known` along
/// the walk, so every block is walked once whatever order the blocks come in.
fn own_invalidate_at_or_below(world: &World, id: usize, known: &mut [Option<bool>]) -> bool {
    let mut path = Vec::new();
    let mut at = Some(id);
    let found = loop {
        let Some(a) = at else {
            break false;
        };
        if let Some(k) = known[a] {
            break k;
        }
        if world.own_invalidates.contains(&a) {
            known[a] = Some(true);
            break true;
        }
        path.push(a);
        at = world.blocks[a].parent;
    };
    for a in path {
        known[a] = Some(found);
    }
    found
}

/// G-T3: whether enforcing the ChainLock on `cl` walks a level with two or more siblings
/// (site 13, structural: `EnforceBlock`'s walk from the locked block to the active chain).
pub fn enforce_walks_siblings(world: &World, cl: Option<usize>) -> bool {
    let Some(cl) = cl else {
        return false;
    };
    for w in world.ancestry(cl) {
        if world.is_ancestor(w, world.tip) {
            return false;
        }
        let Some(parent) = world.blocks[w].parent else {
            return false;
        };
        let siblings =
            world.children(parent).filter(|&c| c != w && world.blocks[c].indexed()).count();
        if siblings >= 2 {
            return true;
        }
    }
    false
}

/// The height from which enforcing the ChainLock on `cl` compares chains: its fork point
/// with the active chain.
pub fn enforce_from(world: &World, cl: Option<usize>) -> Option<u32> {
    let cl = cl?;
    world.ancestry(cl).find(|&a| world.is_ancestor(a, world.tip)).map(|a| world.blocks[a].height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::av;
    use crate::chain::census::Census;
    use crate::chain::core::image_world;
    use crate::chain::generate::due_enforce;
    use crate::chain::world::Seq;
    use proptest::prelude::*;
    use std::sync::OnceLock;

    /// A block the model knows, not built (the tie model reads the tree only).
    fn add(w: &mut World, parent: usize, held: Held, seq: Seq) -> usize {
        let mut hash = [0xa5; 32];
        hash[..8].copy_from_slice(&(w.blocks.len() as u64).to_le_bytes());
        let height = w.blocks[parent].height + 1;
        w.add(Block {
            hash,
            parent: Some(parent),
            height,
            time: w.blocks[parent].time + 150,
            built: None,
            creates: Vec::new(),
            spends: Vec::new(),
            held,
            invalid: false,
            mutation: None,
            seq,
            durable: None,
        })
    }

    /// [`restart_deficit`] as each block's own ancestry walk computed it before the one pass:
    /// the reference the property tests hold it to.
    fn walked_restart_deficit(world: &World) -> u32 {
        let tip = world.blocks[world.tip].height;
        let (mut lead, mut failed) = (None, Vec::new());
        for (i, b) in world.blocks.iter().enumerate() {
            if !b.indexed() || world.is_ancestor(i, world.tip) {
                continue;
            }
            let undone = !world.consensus_failure
                && !world.own_invalidates.is_empty()
                && world.ancestry(i).any(|a| world.own_invalidates.contains(&a));
            if b.invalid && undone {
                failed.push(b.height);
            } else {
                lead = lead.max(Some(b.height));
            }
        }
        if let Some(&top) = failed.iter().max() {
            let level = failed.iter().filter(|&&h| h == top).count() >= 2 || lead == Some(top);
            if level {
                lead = lead.max(Some(top));
            }
        }
        match lead {
            Some(h) if h >= tip => h - tip + 1,
            _ => 0,
        }
    }

    /// [`kill_deficit`] as a clone of the world with its dirty entries lost computed it.
    fn walked_kill_deficit(world: &World) -> u32 {
        let mut lost = world.clone();
        lost.lose_dirty();
        lost.tip = world
            .ancestry(world.tip)
            .find(|&a| lost.blocks[a].held == Held::Data && !lost.blocks[a].invalid)
            .unwrap_or(0);
        walked_restart_deficit(&lost)
    }

    fn held(n: u8) -> Held {
        match n % 4 {
            0 => Held::Nothing,
            1 => Held::Header,
            2 => Held::Data,
            _ => Held::Refused,
        }
    }

    /// One block a generated world gains: its parent counted back from the newest block, what
    /// is held of it, its failed mark, and its last written entry (none, its own, or another).
    type Spec = (usize, u8, bool, u8);

    fn spec() -> impl Strategy<Value = Spec> {
        (prop_oneof![3 => Just(0usize), 1 => 0usize..64], 0u8..4, any::<bool>(), 0u8..9)
    }

    /// `w` with `specs`' blocks added, newest last.
    fn grow(w: &mut World, specs: &[Spec]) {
        for &(back, h, invalid, durable) in specs {
            let len = w.blocks.len();
            let id = add(w, len - 1 - back % len, held(h), Seq::Arrived);
            let b = &mut w.blocks[id];
            b.invalid = invalid;
            b.durable = match durable % 3 {
                0 => None,
                1 => Some((b.held, b.invalid)),
                _ => Some((held(durable + h), !invalid)),
            };
        }
    }

    /// The block `back` before the newest.
    fn back(w: &World, back: usize) -> usize {
        let len = w.blocks.len();
        len - 1 - back % len
    }

    fn image() -> World {
        static IMAGE: OnceLock<World> = OnceLock::new();
        IMAGE.get_or_init(image_world).clone()
    }

    proptest! {
        /// The one pass gives the walks' deficits on S0's image with up to 96 generated blocks:
        /// forks, header-only and refused entries, failed marks, own INVALIDATEs, written and
        /// unwritten entries, and tips off the newest chain.
        #[test]
        fn the_one_pass_deficits_equal_the_walks(
            specs in prop::collection::vec(spec(), 0..96),
            tip in 0usize..64,
            own in prop::collection::vec(0usize..64, 0..4),
            failure in any::<bool>(),
        ) {
            let mut w = image();
            grow(&mut w, &specs);
            w.tip = back(&w, tip);
            w.own_invalidates = own.iter().map(|&o| back(&w, o)).collect();
            w.consensus_failure = failure;
            prop_assert_eq!(restart_deficit(&w), walked_restart_deficit(&w));
            prop_assert_eq!(kill_deficit(&w), walked_kill_deficit(&w));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(12))]

        /// The same on S2's world, 8,100 header-only line blocks above the tip: own INVALIDATEs
        /// and failed marks on the line, as S2's best-header resets make them.
        #[test]
        fn the_one_pass_deficits_equal_the_walks_on_s2(
            specs in prop::collection::vec(spec(), 0..24),
            marks in prop::collection::vec((0u32..av::LINE, any::<bool>()), 1..6),
            failure in any::<bool>(),
        ) {
            let mut w = av::world();
            let line = w.line.expect("S2's world has its line");
            grow(&mut w, &specs);
            for &(place, own) in &marks {
                let id = line.first + place as usize;
                w.blocks[id].invalid = true;
                if own {
                    w.own_invalidates.insert(id);
                }
            }
            w.consensus_failure = failure;
            prop_assert_eq!(restart_deficit(&w), walked_restart_deficit(&w));
            prop_assert_eq!(kill_deficit(&w), walked_kill_deficit(&w));
        }
    }

    #[test]
    fn g_t1_a_restart_waits_for_a_strict_lead() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        assert_eq!(restart_deficit(&w), 0, "one chain");
        // An equal-height sibling of the tip: one block more and the tip leads.
        let rival = add(&mut w, below, Held::Data, Seq::Arrived);
        assert_eq!(restart_deficit(&w), 1);
        // Two above the tip, header only: three more.
        let top = add(&mut w, rival, Held::Header, Seq::Arrived);
        add(&mut w, top, Held::Header, Seq::Arrived);
        assert_eq!(restart_deficit(&w), 3);
        // Known-failed entries compete too (a load may reconsider them).
        for b in &mut w.blocks[rival..] {
            b.invalid = true;
        }
        assert_eq!(restart_deficit(&w), 3);
        // Unless the input's own INVALIDATE marked them and the top is single.
        w.own_invalidates.insert(rival);
        assert_eq!(restart_deficit(&w), 0);
        // Two level at the top: the load's reset picks one (site 12).
        let level = add(&mut w, top, Held::Header, Seq::Arrived);
        w.blocks[level].invalid = true;
        assert_eq!(restart_deficit(&w), 3);
    }

    #[test]
    fn g_t1_a_kill_loads_only_what_was_written() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let rival = add(&mut w, below, Held::Data, Seq::Arrived);
        assert_eq!(restart_deficit(&w), 1);
        assert_eq!(kill_deficit(&w), 0, "a KILL loses the unwritten rival");
        w.blocks[rival].durable = Some((Held::Data, false));
        assert_eq!(kill_deficit(&w), 1);
        // A mark since the write is lost too.
        w.blocks[rival].invalid = true;
        assert!(w.blocks[rival].dirty());
        w.lose_dirty();
        assert!(!w.blocks[rival].invalid && w.blocks[rival].held == Held::Data);
    }

    #[test]
    fn rule_3_a_known_lock_is_due_until_enforced() {
        let mut w = image_world();
        // A held lock means a ChainLock op ran `CheckActiveState`: no reset is due.
        w.cl_handler_enabled = true;
        let mut census = Census::default();
        assert!(due_enforce(&w, &mut census).is_none(), "no lock");
        let tip = w.tip;
        w.best_cl = Some(tip);
        assert!(matches!(due_enforce(&w, &mut census), Some(Some(_))));
        w.cl_enforced = w.best_cl;
        assert!(due_enforce(&w, &mut census).is_none(), "enforced since it resolved");
        // A header-only lock above a failed entry: enforcing would clear the failure (rule 2).
        let header = add(&mut w, tip, Held::Header, Seq::Arrived);
        let failed = add(&mut w, header, Held::Header, Seq::Arrived);
        w.blocks[failed].invalid = true;
        w.best_cl = Some(header);
        assert!(matches!(due_enforce(&w, &mut census), Some(None)));
        assert_eq!(census.avoided.get("cl_on_invalid"), Some(&1));
    }

    #[test]
    fn g_t2_seq_zero_pairs_after_a_load_are_risky() {
        let mut w = image_world();
        let tip = w.tip;
        let below = w.blocks[tip].parent.expect("parent");
        let h = w.blocks[tip].height;
        // A sibling whose data arrived after the load has its own sequence id: no tie.
        let sibling = add(&mut w, below, Held::Data, Seq::Arrived);
        assert!(!risky_from(&w, 0));
        // After a load both are seq 0 at the tip's height (site 5's invalidate walk).
        w.loaded();
        assert!(risky_from(&w, h - 1));
        assert!(risky_from(&w, h));
        assert!(!risky_from(&w, h + 1));
        // A header-only entry is always seq 0.
        w.blocks[sibling].seq = Seq::Precious;
        assert!(!risky_from(&w, 0));
        add(&mut w, below, Held::Header, Seq::Arrived);
        assert!(risky_from(&w, h));
    }

    #[test]
    fn g_t2_data_stored_under_a_parent_without_data_stays_seq_zero() {
        let mut w = image_world();
        let tip = w.tip;
        let h = w.blocks[tip].height;
        let below = w.blocks[tip].parent.expect("parent");
        let fork_parent = w.blocks[below].parent.expect("parent");
        // A fork sent headers first; the data of its second block alone arrives.
        let first = add(&mut w, fork_parent, Held::Header, Seq::Arrived);
        let second = add(&mut w, first, Held::Header, Seq::Arrived);
        w.store_data(second);
        assert_eq!(w.blocks[second].seq, Seq::Unlinked);
        // It ties with the loaded tip at the tip's height.
        assert!(risky_from(&w, h));
        // The first block's data numbers both: no pair is left.
        w.store_data(first);
        assert_eq!(w.blocks[first].seq, Seq::Arrived);
        assert_eq!(w.blocks[second].seq, Seq::Arrived);
        assert!(!risky_from(&w, 0));
    }

    #[test]
    fn g_t2_data_stored_under_a_failed_parent_stays_seq_zero_after_the_link() {
        let mut w = image_world();
        let tip = w.tip;
        let h = w.blocks[tip].height;
        let below = w.blocks[tip].parent.expect("parent");
        let fork_parent = w.blocks[below].parent.expect("parent");
        let first = add(&mut w, fork_parent, Held::Header, Seq::Arrived);
        let second = add(&mut w, first, Held::Header, Seq::Arrived);
        // The first header is marked failed when the second block's data arrives: Core never
        // queues the second for the walk.
        w.blocks[first].invalid = true;
        w.store_data(second);
        assert_eq!(w.blocks[second].seq, Seq::Stranded);
        // Cleared and then stored, the first block is numbered; the second is not.
        w.blocks[first].invalid = false;
        w.store_data(first);
        assert_eq!(w.blocks[first].seq, Seq::Arrived);
        assert_eq!(w.blocks[second].seq, Seq::Stranded);
        assert!(risky_from(&w, h));
        // A load queues it like any other stored entry.
        w.loaded();
        assert_eq!(w.blocks[second].seq, Seq::Loaded);
    }

    #[test]
    fn rule_2_scope_reaches_the_subtrees_of_the_target_and_its_failed_ancestors() {
        let mut w = image_world();
        let tip = w.tip;
        let below = w.blocks[tip].parent.expect("parent");
        let fork_parent = w.blocks[below].parent.expect("parent");
        let failed = add(&mut w, fork_parent, Held::Header, Seq::Arrived);
        let under = add(&mut w, failed, Held::Data, Seq::Stranded);
        let sibling = add(&mut w, failed, Held::Header, Seq::Arrived);
        w.blocks[failed].invalid = true;
        w.blocks[sibling].invalid = true;
        assert!(!w.reconsider_reaches_stored_under_failed(sibling));
        w.stored_under_failed.insert(under);
        // Its own subtree, and a failed ancestor's.
        assert!(w.reconsider_reaches_stored_under_failed(failed));
        assert!(w.reconsider_reaches_stored_under_failed(sibling));
        // Not a block whose ancestors are all good.
        assert!(!w.reconsider_reaches_stored_under_failed(tip));
    }

    #[test]
    fn g_t3_an_enforce_walk_meeting_two_siblings_is_structural() {
        let mut w = image_world();
        let below = w.blocks[w.tip].parent.expect("parent");
        let locked = add(&mut w, below, Held::Data, Seq::Arrived);
        w.best_cl = Some(locked);
        // The locked block's level holds the tip and itself: one other sibling.
        assert!(!enforce_walks_siblings(&w, w.best_cl));
        assert_eq!(enforce_from(&w, w.best_cl), Some(w.blocks[below].height));
        add(&mut w, below, Held::Header, Seq::Arrived);
        assert!(enforce_walks_siblings(&w, w.best_cl), "site 13");
        // On the active chain there is no walk.
        w.best_cl = Some(w.tip);
        assert!(!enforce_walks_siblings(&w, w.best_cl));
    }
}
