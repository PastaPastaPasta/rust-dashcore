//! The advisory tie model (O2b-3 P3; the O2b-2 D6/D8 constraint): which ops could reach
//! Core's address tie-break, so that the generator defers, retargets or drops them. Core's
//! POST `tie` stays authoritative (D30); the census counts the steps this model let through
//! that Core then tainted, by site.
//!
//! Every block the generator indexes has regtest's bits, so equal work is equal height. Core
//! compares two distinct entries by address only when their work and sequence ids are equal,
//! and only loaded, header-only and unlinked (stored under a chain without data) entries share
//! an id (0).

use std::collections::BTreeMap;

use super::world::{Held, World};

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
    let tip = world.blocks[world.tip].height;
    let (mut lead, mut failed) = (None, Vec::new());
    for (i, b) in world.blocks.iter().enumerate() {
        if !b.indexed() || world.is_ancestor(i, world.tip) {
            continue;
        }
        let undone = !world.consensus_failure
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

/// G-T1 for a KILL: [`restart_deficit`] of what the load reads, the entries as the last write
/// left them, with the active chain's highest stored one as the tip (blocks since are lost).
pub fn kill_deficit(world: &World) -> u32 {
    let mut lost = world.clone();
    lost.lose_dirty();
    lost.tip = world
        .ancestry(world.tip)
        .find(|&a| lost.blocks[a].held == Held::Data && !lost.blocks[a].invalid)
        .unwrap_or(0);
    restart_deficit(&lost)
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
    use crate::chain::census::Census;
    use crate::chain::core::image_world;
    use crate::chain::generate::due_enforce;
    use crate::chain::world::{Block, Seq};

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
