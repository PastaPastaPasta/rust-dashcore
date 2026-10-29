//! `ActiveChain` (SB4 §3.6): a persistent radix vector of ids by height. Push and pop copy one
//! root-to-leaf path (`Arc::make_mut`), so a published view keeps its own root (R8).

use std::sync::Arc;

use dash_consensus::BlockId;

use crate::anchor::floor_below;

/// Ids per leaf (4 KiB).
pub(crate) const LEAF: u32 = 1 << 10;
/// Children per inner node.
const FAN: u32 = 64;
/// The deepest root, whose span covers every `u32` height.
pub(crate) const MAX_DEPTH: u32 = 4;
const _: () = assert!(LEAF as u64 * (FAN as u64).pow(MAX_DEPTH) >= 1 << 32);

/// A leaf in its `Arc`, at full capacity.
pub(crate) const LEAF_NODE_BYTES: usize =
    2 * size_of::<usize>() + size_of::<Node>() + LEAF as usize * size_of::<BlockId>();
/// An inner node in its `Arc`, at full capacity.
pub(crate) const INNER_NODE_BYTES: usize =
    2 * size_of::<usize>() + size_of::<Node>() + FAN as usize * size_of::<Option<Arc<Node>>>();

/// One root-to-leaf path copied out of a published view: a leaf and an inner node per level
/// to [`MAX_DEPTH`] (SB5 §6.2's path term). A node a push or a re-extension allocates fresh is
/// residency instead (§6.3), and so are the published view's originals a reorg rebuilds, which
/// that view, one retained snapshot, holds until the release.
pub(crate) const PATH_COPY_BYTES: usize = LEAF_NODE_BYTES + MAX_DEPTH as usize * INNER_NODE_BYTES;

enum Node {
    /// The ids of heights `k × LEAF ..`, in order.
    Leaf(Vec<BlockId>),
    /// Children by position; `None` for a subtree below the window (SPV).
    Inner(Vec<Option<Arc<Node>>>),
}

/// A copy has the node's full capacity, which is final: a leaf never holds more than LEAF ids,
/// an inner node never more than FAN children. `Arc::make_mut` copies the published path on
/// every push; a copy at its length would reallocate at the next push and keep up to twice its
/// ids (A-31).
impl Clone for Node {
    fn clone(&self) -> Node {
        match self {
            Node::Leaf(ids) => Node::Leaf(with_room(ids, LEAF)),
            Node::Inner(children) => Node::Inner(with_room(children, FAN)),
        }
    }
}

impl Node {
    /// An empty node at `level`, with its full capacity.
    fn empty(level: u32) -> Node {
        match level {
            0 => Node::Leaf(Vec::with_capacity(LEAF as usize)),
            _ => Node::Inner(Vec::with_capacity(FAN as usize)),
        }
    }
}

/// A fresh node at `level`: the chain's residency, not a step's copy (SB5 §6.3).
fn fresh(level: u32) -> Arc<Node> {
    crate::fault::residency(|| Arc::new(Node::empty(level)))
}

fn with_room<T: Clone>(items: &[T], cap: u32) -> Vec<T> {
    debug_assert!(items.len() <= cap as usize, "a node over its capacity");
    let mut v = Vec::with_capacity(cap as usize);
    v.extend_from_slice(items);
    v
}

/// The heights a node at `level` covers (level 0 is a leaf).
fn span(level: u32) -> u64 {
    u64::from(LEAF) * u64::from(FAN).pow(level)
}

/// The active chain: `len` heights from 0, of which `base..len` are resident.
#[derive(Clone, Default)]
pub(crate) struct ActiveChain {
    root: Option<Arc<Node>>,
    /// The root's level.
    depth: u32,
    /// `CChain::Height() + 1`.
    len: u32,
    /// The lowest resident height: 0 in the node; SPV's window moves it (DEC-174).
    base: u32,
    /// The lowest height held, which the base never goes below: 0 from genesis, an anchored
    /// chain's first height.
    floor: u32,
    /// The lowest accepted height, at or above `floor`: genesis, or an anchored chain's anchor.
    anchor: u32,
    /// The id at `anchor`, which outlives the window: `FindForkInGlobalIndex`'s fallback
    /// (§3.6, §10.1 n6).
    anchor_id: Option<BlockId>,
}

/// A summary: the ids would print millions of lines.
impl std::fmt::Debug for ActiveChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActiveChain")
            .field("len", &self.len)
            .field("base", &self.base)
            .field("floor", &self.floor)
            .field("anchor", &self.anchor)
            .field("depth", &self.depth)
            .finish_non_exhaustive()
    }
}

impl ActiveChain {
    /// An empty chain whose lowest accepted height is `anchor` (0 from genesis), and whose first
    /// height is its floor, `anchor − 24` when anchored (§8.4, §11.1).
    pub(crate) fn starting_at(anchor: u32) -> ActiveChain {
        let base = floor_below(anchor);
        let mut depth = 0;
        while span(depth) <= u64::from(base) {
            depth += 1;
        }
        ActiveChain {
            root: None,
            depth,
            len: base,
            base,
            floor: base,
            anchor,
            anchor_id: None,
        }
    }

    /// The number of heights, `Height() + 1`; 0 for an empty chain.
    pub(crate) fn len(&self) -> u32 {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The lowest height [`ActiveChain::get`] answers.
    pub(crate) fn base(&self) -> u32 {
        self.base
    }

    /// The id at the lowest accepted height (genesis, or an anchored chain's anchor), resident
    /// or not.
    pub(crate) fn anchor_id(&self) -> Option<BlockId> {
        self.anchor_id
    }

    /// The id at height `h`, when `base <= h < len`.
    pub(crate) fn get(&self, h: u32) -> Option<BlockId> {
        if h >= self.len || h < self.base {
            return None;
        }
        let mut node = self.root.as_deref()?;
        for level in (1..=self.depth).rev() {
            let Node::Inner(children) = node else {
                unreachable!("inner levels hold inner nodes")
            };
            let idx = (u64::from(h) / span(level - 1) % u64::from(FAN)) as usize;
            node = children.get(idx)?.as_deref()?;
        }
        let Node::Leaf(ids) = node else {
            unreachable!("level 0 holds leaves")
        };
        ids.get((h % LEAF) as usize).copied()
    }

    /// Appends `id` at height `len`.
    pub(crate) fn push(&mut self, id: BlockId) {
        let h = self.len;
        assert!(h < u32::MAX, "the active chain is full");
        let depth = self.depth;
        let root = self.root.get_or_insert_with(|| fresh(depth));
        if u64::from(h) == span(self.depth) {
            let old = std::mem::replace(root, fresh(self.depth + 1));
            Arc::make_mut(root).inner_mut().push(Some(old));
            self.depth += 1;
        }
        let ids = leaf_at(root, self.depth, h);
        // An anchored chain's first leaf starts at its base: the heights below it are padding,
        // which `get` never reads.
        debug_assert!(ids.len() as u32 == h % LEAF || h == self.base, "heights in order");
        ids.resize((h % LEAF) as usize, BlockId(u32::MAX));
        ids.push(id);
        if h == self.anchor {
            self.anchor_id = Some(id);
        }
        self.len += 1;
    }

    /// Removes the id at height `len - 1`.
    pub(crate) fn pop(&mut self) -> Option<BlockId> {
        let h = self.len.checked_sub(1)?;
        assert!(h >= self.base, "pop below the window's base");
        let root = self.root.as_mut().expect("a non-empty chain has a root");
        let (id, _) = pop_at(root, self.depth, h);
        self.len = h;
        if h == self.anchor {
            self.anchor_id = None;
        }
        // Only an empty chain restarts at a leaf root. Above a base, the trimmed tree keeps its
        // depth: `push` deepens only when `len` reaches the root's span.
        if self.len == 0 {
            self.root = None;
            self.depth = 0;
            self.base = 0;
        }
        Some(id)
    }

    /// Drops the leaves wholly below `base` (SPV's window, §3.6): a retained view keeps them,
    /// the chain no longer does. `base` only moves up; [`ActiveChain::extend_down`] lowers it.
    pub(crate) fn set_base(&mut self, base: u32) {
        let base = base.min(self.len);
        if base <= self.base {
            return;
        }
        // Leaves below this height hold no resident id; only a whole leaf leaving changes the tree.
        let drop_below = u64::from(base / LEAF) * u64::from(LEAF);
        if let Some(root) = self.root.as_mut().filter(|_| base / LEAF > self.base / LEAF) {
            drop_leaves(root, self.depth, 0, drop_below);
        }
        self.base = base;
    }

    /// Restores `id` at height `base − 1`, which becomes the base: a disconnect below SPV's
    /// window re-extends the chain from ids read by ancestry before it pops (DEC-174).
    pub(crate) fn extend_down(&mut self, id: BlockId) {
        assert!(self.base > self.floor && self.base < self.len, "re-extended inside the chain");
        let h = self.base - 1;
        let root = self.root.as_mut().expect("a non-empty chain has a root");
        let ids = leaf_at(root, self.depth, h);
        // A dropped leaf comes back from its top: its lower heights are padding until restored.
        if ids.is_empty() {
            ids.resize(LEAF as usize, BlockId(u32::MAX));
        }
        ids[(h % LEAF) as usize] = id;
        self.base = h;
    }

    /// The ids at heights `from..len`, lowest first.
    #[cfg(test)]
    pub(crate) fn ids_from(&self, from: u32) -> Vec<Option<BlockId>> {
        (from..self.len).map(|h| self.get(h)).collect()
    }
}

impl Node {
    fn inner_mut(&mut self) -> &mut Vec<Option<Arc<Node>>> {
        match self {
            Node::Inner(children) => children,
            Node::Leaf(_) => unreachable!("inner levels hold inner nodes"),
        }
    }

    fn leaf_mut(&mut self) -> &mut Vec<BlockId> {
        match self {
            Node::Leaf(ids) => ids,
            Node::Inner(_) => unreachable!("level 0 holds leaves"),
        }
    }
}

/// The leaf that covers height `h`, its path copied out of any retained view; a missing node on
/// the path (past the end, or dropped below the window) comes back empty.
fn leaf_at(root: &mut Arc<Node>, depth: u32, h: u32) -> &mut Vec<BlockId> {
    let mut node = Arc::make_mut(root);
    for level in (1..=depth).rev() {
        let idx = (u64::from(h) / span(level - 1) % u64::from(FAN)) as usize;
        let children = node.inner_mut();
        if children.len() <= idx {
            children.resize(idx + 1, None);
        }
        let child = children[idx].get_or_insert_with(|| fresh(level - 1));
        node = Arc::make_mut(child);
    }
    node.leaf_mut()
}

/// Pops height `h`, the last one; returns the id and whether `node` is now empty.
fn pop_at(node: &mut Arc<Node>, level: u32, h: u32) -> (BlockId, bool) {
    match Arc::make_mut(node) {
        Node::Leaf(ids) => {
            let id = ids.pop().expect("the last height is in the last leaf");
            (id, ids.is_empty())
        }
        Node::Inner(children) => {
            let idx = (u64::from(h) / span(level - 1) % u64::from(FAN)) as usize;
            let child = children[idx].as_mut().expect("the last height's path is resident");
            let (id, empty) = pop_at(child, level - 1, h);
            if empty {
                children.truncate(idx);
                while children.last().is_some_and(Option::is_none) {
                    children.pop();
                }
            }
            (id, children.is_empty())
        }
    }
}

/// Replaces with `None` every subtree of `node` (covering heights from `start`) that lies wholly
/// below `below`.
fn drop_leaves(node: &mut Arc<Node>, level: u32, start: u64, below: u64) {
    if level == 0 || start + span(level) <= below || start >= below {
        return;
    }
    let child_span = span(level - 1);
    let children = Arc::make_mut(node).inner_mut();
    for (k, child) in children.iter_mut().enumerate() {
        let child_start = start + k as u64 * child_span;
        if child_start + child_span <= below {
            *child = None;
        } else if let Some(c) = child.as_mut() {
            drop_leaves(c, level - 1, child_start, below);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u32) -> ActiveChain {
        let mut c = ActiveChain::default();
        for h in 0..n {
            c.push(BlockId(h + 1_000_000));
        }
        c
    }

    /// Every node's capacity, leaves and inner nodes apart.
    fn capacities(node: &Node, leaves: &mut Vec<usize>, inner: &mut Vec<usize>) {
        match node {
            Node::Leaf(ids) => leaves.push(ids.capacity()),
            Node::Inner(children) => {
                inner.push(children.capacity());
                children.iter().flatten().for_each(|c| capacities(c, leaves, inner));
            }
        }
    }

    /// A-31: with a published root retained across every push and pop, as the chain's snapshot
    /// does, each copied node keeps exactly its full room.
    #[test]
    fn copied_nodes_keep_their_capacity() {
        let mut c = ActiveChain::default();
        // Held across each step, so every step copies the path it changes.
        let mut published = c.clone();
        for h in 0..3 * LEAF + 5 {
            c.push(BlockId(h));
            published = c.clone();
        }
        for _ in 0..10 {
            c.pop();
            published = c.clone();
        }
        drop(published);
        let (mut leaves, mut inner) = (Vec::new(), Vec::new());
        capacities(c.root.as_ref().expect("a root"), &mut leaves, &mut inner);
        assert_eq!(leaves, [LEAF as usize; 3]);
        assert_eq!(inner, [FAN as usize]);
    }

    #[test]
    fn push_get_pop_across_leaf_and_level_boundaries() {
        // Past one leaf, past one level-1 node (64 leaves), into level 2.
        let n = LEAF * FAN + LEAF + 3;
        let mut c = ids(n);
        assert_eq!(c.len(), n);
        for h in [0, 1, LEAF - 1, LEAF, LEAF * FAN - 1, LEAF * FAN, n - 1] {
            assert_eq!(c.get(h), Some(BlockId(h + 1_000_000)), "height {h}");
        }
        assert_eq!(c.get(n), None);
        for h in (0..n).rev() {
            assert_eq!(c.pop(), Some(BlockId(h + 1_000_000)));
            assert_eq!(c.get(h), None);
            if h > 0 {
                assert_eq!(c.get(h - 1), Some(BlockId(h - 1 + 1_000_000)));
            }
        }
        assert_eq!(c.pop(), None);
        // Reusable after emptying.
        c.push(BlockId(7));
        assert_eq!((c.len(), c.get(0)), (1, Some(BlockId(7))));
    }

    #[test]
    fn a_retained_view_keeps_its_own_root() {
        let mut c = ids(3000);
        let view = c.clone();
        for _ in 0..1500 {
            c.pop();
        }
        for h in 1500..1600 {
            c.push(BlockId(h));
        }
        assert_eq!(view.len(), 3000);
        assert_eq!(view.get(2999), Some(BlockId(2999 + 1_000_000)));
        assert_eq!(view.get(1500), Some(BlockId(1500 + 1_000_000)));
        assert_eq!(c.get(1500), Some(BlockId(1500)));
        assert_eq!(c.get(1499), Some(BlockId(1499 + 1_000_000)));
    }

    #[test]
    fn reorg_round_trip_matches_a_plain_vector() {
        let mut c = ActiveChain::default();
        let mut model: Vec<BlockId> = Vec::new();
        let mut state = 7u64;
        for step in 0..20_000u32 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            if (state >> 60) < 4 && !model.is_empty() {
                let k = ((state >> 20) % 1500) as usize;
                for _ in 0..k.min(model.len()) {
                    assert_eq!(c.pop(), model.pop());
                }
            } else {
                c.push(BlockId(step));
                model.push(BlockId(step));
            }
        }
        assert_eq!(c.len() as usize, model.len());
        assert_eq!(c.ids_from(0), model.iter().copied().map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn set_base_drops_whole_leaves_below_the_window_only() {
        let mut c = ids(5 * LEAF);
        let view = c.clone();
        c.set_base(2 * LEAF + 10);
        assert_eq!(c.base(), 2 * LEAF + 10);
        assert_eq!(c.get(2 * LEAF + 9), None, "below the base");
        assert_eq!(c.get(2 * LEAF + 10), Some(BlockId(2 * LEAF + 10 + 1_000_000)));
        // The view still answers every height.
        assert_eq!(view.get(0), Some(BlockId(1_000_000)));
        // The chain still grows and shrinks above the base.
        c.push(BlockId(1));
        assert_eq!(c.pop(), Some(BlockId(1)));
        assert_eq!(c.pop(), Some(BlockId(5 * LEAF - 1 + 1_000_000)));
        // Only whole leaves left the tree.
        let Some(Node::Inner(children)) = c.root.as_deref() else {
            panic!("five leaves need an inner root")
        };
        assert!(children[0].is_none() && children[1].is_none() && children[2].is_some());
    }

    /// SPV raises the base a few heights per step (§3.6): a leaf goes as soon as the base
    /// crosses its top, not only when one call crosses several leaves (A-31's bound).
    #[test]
    fn set_base_in_small_steps_drops_each_crossed_leaf() {
        let mut c = ids(5 * LEAF);
        let live = |c: &ActiveChain| {
            let Some(Node::Inner(children)) = c.root.as_deref() else {
                panic!("five leaves need an inner root")
            };
            children.iter().map(Option::is_some).collect::<Vec<_>>()
        };
        // Every 7th height misses the third leaf's top, so its first heights follow.
        for base in (1..=3 * LEAF + 1).step_by(7).chain([3 * LEAF, 3 * LEAF + 1]) {
            c.set_base(base);
            let dropped = (base / LEAF) as usize;
            let want: Vec<bool> = (0..5).map(|l| l >= dropped).collect();
            assert_eq!(live(&c), want, "base {base}");
        }
    }

    /// DEC-174: re-extending below the base restores dropped leaves and a dropped inner subtree,
    /// heights pop through them, and a retained view never sees the restored ids.
    #[test]
    fn extend_down_restores_dropped_subtrees_then_pops() {
        let (n, split, low) = (LEAF * FAN + 2 * LEAF, LEAF * FAN + LEAF + 3, LEAF * FAN - 10);
        // Restored ids are 2M-based, the chain's own 1M-based.
        let id_at = |h: u32| {
            BlockId(
                h + if h >= split {
                    1_000_000
                } else {
                    2_000_000
                },
            )
        };
        let first_dropped =
            |c: &ActiveChain| matches!(c.root.as_deref(), Some(Node::Inner(ch)) if ch[0].is_none());
        let mut c = ids(n);
        c.set_base(split);
        assert!(first_dropped(&c), "the first level-1 subtree left");
        let view = c.clone();
        for h in (low..split).rev() {
            c.extend_down(id_at(h));
        }
        assert_eq!((c.base(), c.get(low - 1)), (low, None));
        for h in [low, LEAF * FAN - 1, LEAF * FAN, split - 1, split] {
            assert_eq!(c.get(h), Some(id_at(h)), "height {h}");
        }
        assert!(first_dropped(&view), "the view keeps its own root");
        for h in (low + 1..n).rev() {
            assert_eq!(c.pop(), Some(id_at(h)), "height {h}");
        }
        assert_eq!((c.len(), c.get(low)), (low + 1, Some(id_at(low))));
        c.push(BlockId(1));
        assert_eq!(c.get(low + 1), Some(BlockId(1)));
        assert_eq!(c.anchor_id(), Some(BlockId(1_000_000)), "the anchor outlives the window");
    }

    /// Popping down to a base on a leaf boundary empties the tree's resident part but keeps
    /// its depth, so later pushes land at their heights.
    #[test]
    fn pop_to_a_leaf_aligned_base_then_push() {
        for (n, base) in [(3000, 2 * LEAF), (LEAF * FAN + 5, LEAF * FAN), (2 * LEAF, LEAF)] {
            let mut c = ids(n);
            c.set_base(base);
            while c.len() > base {
                c.pop();
            }
            assert_eq!((c.len(), c.get(base)), (base, None));
            for h in base..n + 2000 {
                c.push(BlockId(h));
            }
            for h in [base, base + 1, base + LEAF, n + 1999] {
                assert_eq!(c.get(h), Some(BlockId(h)), "n {n}, base {base}, height {h}");
            }
            assert_eq!(c.get(base - 1), None);
        }
    }
}
