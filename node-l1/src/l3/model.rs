//! The P7 harness model (plan §3.7): each leg's index as Core's rules move it, for the best
//! header, `best_invalid` and the candidate set after every op. V and Core (through the shim
//! replay) are held to its best header; `best_invalid` and the candidates are Core's against V's
//! in the replay (OD-L3-15), and the model's are their cross-check there: where Core = V, the
//! model differing is a harness error (OD-L3-35).
//!
//! The rules are rc.4's: AddToBlockIndex moves the best header to strictly more work only;
//! AcceptBlock's contextual reject marks the entry failed and calls nothing else (v:4824-4832);
//! a ConnectBlock failure is InvalidBlockFound (v:2083-2094), then InvalidChainFound
//! (v:2040-2050); `invalidateblock` is InvalidateBlock (v:3895-4052). No leg restarts, so Core's
//! load-time recomputation of `best_invalid` (v:5362-5363) never applies.
//!
//! Every indexed block on L3's regtest legs has regtest's bits (a block with other bits fails
//! before the index), so chain work is the height's.

use std::collections::{BTreeMap, BTreeSet};

use super::inventory::ParentState;

/// A block hash, internal byte order.
pub type Hash = [u8; 32];

#[derive(Clone, Debug)]
struct Entry {
    height: u32,
    prev: Option<Hash>,
    /// `nSequenceId`: set when the block's data arrives (0 for a header only).
    seq: u64,
    /// BLOCK_HAVE_DATA, and with it BLOCK_VALID_TRANSACTIONS.
    data: bool,
    failed_valid: bool,
    failed_child: bool,
    /// BLOCK_VALID_SCRIPTS: connected once.
    scripts: bool,
}

/// One leg's model.
#[derive(Clone, Debug)]
pub struct Model {
    entries: BTreeMap<Hash, Entry>,
    /// The active chain, genesis first.
    chain: Vec<Hash>,
    best_header: Hash,
    best_invalid: Option<Hash>,
    /// `m_failed_blocks`.
    failed: BTreeSet<Hash>,
    next_seq: u64,
}

/// A candidate as the post reports it: height, hash, sequence id.
pub type Candidate = (u32, Hash, u64);

/// The model's P7 fields after an op, which the replay holds Core to once Core = V (a harness
/// cross-check, OD-L3-35): the best header, `best_invalid` and the candidates' (height, hash)
/// in Core's comparator order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct P7 {
    pub best_header: (u32, Hash),
    pub best_invalid: Option<(u32, Hash)>,
    pub candidates: Vec<(u32, Hash)>,
}

impl Model {
    pub fn new(genesis: Hash) -> Model {
        let mut entries = BTreeMap::new();
        entries.insert(
            genesis,
            Entry {
                height: 0,
                prev: None,
                seq: 0,
                data: true,
                failed_valid: false,
                failed_child: false,
                scripts: true,
            },
        );
        Model {
            entries,
            chain: vec![genesis],
            best_header: genesis,
            best_invalid: None,
            failed: BTreeSet::new(),
            next_seq: 1,
        }
    }

    fn entry(&self, hash: &Hash) -> &Entry {
        self.entries.get(hash).expect("a modelled entry")
    }

    fn work(&self, hash: &Hash) -> u32 {
        self.entry(hash).height
    }

    pub fn tip(&self) -> Hash {
        *self.chain.last().expect("genesis")
    }

    pub fn height_of(&self, hash: &Hash) -> Option<u32> {
        self.entries.get(hash).map(|e| e.height)
    }

    pub fn parent_of(&self, hash: &Hash) -> Option<Hash> {
        self.entries.get(hash).and_then(|e| e.prev)
    }

    pub fn knows(&self, hash: &Hash) -> bool {
        self.entries.contains_key(hash)
    }

    /// AcceptBlockHeader's AddToBlockIndex for a new header on a known `prev`.
    fn add_header(&mut self, hash: Hash, prev: Hash) {
        if self.entries.contains_key(&hash) {
            return;
        }
        let height = self.entry(&prev).height + 1;
        self.entries.insert(
            hash,
            Entry {
                height,
                prev: Some(prev),
                seq: 0,
                data: false,
                failed_valid: false,
                failed_child: false,
                scripts: false,
            },
        );
        // Core: node/blockstorage.cpp:86-120 (strictly more work)
        if height > self.work(&self.best_header) {
            self.best_header = hash;
        }
    }

    /// A header `submitheader` accepted: indexed headers-only.
    pub fn header(&mut self, hash: Hash, prev: Hash) {
        self.add_header(hash, prev);
    }

    /// ReceivedBlockTransactions: the data and its sequence id.
    fn add_data(&mut self, hash: &Hash) {
        let seq = self.next_seq;
        self.next_seq += 1;
        let e = self.entries.get_mut(hash).expect("indexed");
        e.data = true;
        e.seq = seq;
    }

    /// A block `submitblock` accepted onto the tip.
    pub fn connect(&mut self, hash: Hash, prev: Hash) {
        assert_eq!(prev, self.tip(), "an accepted block extends the tip in L3's legs");
        self.add_header(hash, prev);
        self.add_data(&hash);
        self.entries.get_mut(&hash).expect("indexed").scripts = true;
        self.chain.push(hash);
    }

    /// AcceptBlock's ContextualCheckBlock reject: indexed, marked failed unless MUTATED, nothing
    /// else (v:4829-4837).
    pub fn contextual_reject(&mut self, hash: Hash, prev: Hash, mutated: bool) {
        self.add_header(hash, prev);
        if !mutated {
            self.entries.get_mut(&hash).expect("indexed").failed_valid = true;
        }
    }

    /// A ConnectBlock failure on a stored block: InvalidBlockFound unless MUTATED.
    pub fn connect_reject(&mut self, hash: Hash, prev: Hash, mutated: bool) {
        self.add_header(hash, prev);
        self.add_data(&hash);
        if !mutated {
            self.entries.get_mut(&hash).expect("indexed").failed_valid = true;
            self.failed.insert(hash);
            self.invalid_chain_found(hash);
        }
    }

    /// InvalidChainFound (v:2045-2055).
    fn invalid_chain_found(&mut self, hash: Hash) {
        if self.best_invalid.is_none_or(|b| self.work(&hash) > self.work(&b)) {
            self.best_invalid = Some(hash);
        }
        if self.ancestor(&self.best_header, self.entry(&hash).height) == Some(hash) {
            self.best_header = self.tip();
        }
    }

    /// AcceptBlockHeader's walk for a header on `prev` (v:4684-4699): when `prev` is short of
    /// SCRIPTS and descends from a block in the failed set, it and every block up to that one
    /// are marked BLOCK_FAILED_CHILD and the header is refused; nothing else moves. Core takes
    /// the first such block in `m_failed_blocks`; on L3's legs a header has at most one (every
    /// other failed block is a trial's, off the chain). Whether it marked anything.
    pub fn failed_child_walk(&mut self, prev: &Hash) -> bool {
        if self.entry(prev).scripts {
            return false;
        }
        let Some(failed) =
            self.failed.iter().copied().find(|f| self.is_ancestor(f, prev) && f != prev)
        else {
            return false;
        };
        let mut at = *prev;
        while at != failed {
            let e = self.entries.get_mut(&at).expect("indexed");
            e.failed_child = true;
            at = e.prev.expect("a descendant of the failed block");
        }
        true
    }

    /// Whether `a` is `of` or an ancestor of it, by the blocks' own parent links.
    pub fn is_ancestor(&self, a: &Hash, of: &Hash) -> bool {
        self.height_of(a).and_then(|h| self.ancestor(of, h)) == Some(*a)
    }

    fn ancestor(&self, hash: &Hash, height: u32) -> Option<Hash> {
        let mut at = *hash;
        loop {
            let e = self.entries.get(&at)?;
            if e.height == height {
                return Some(at);
            }
            if e.height < height {
                return None;
            }
            at = e.prev?;
        }
    }

    /// `invalidateblock` of the tip (L3's rollback): InvalidateBlock (v:3900-4057), then the
    /// RPC's ActivateBestChain, which finds nothing better than the parent in L3's legs.
    pub fn invalidate_tip(&mut self, hash: Hash) {
        assert_eq!(hash, self.tip(), "L3 rolls back its tip only");
        // Core: v:3962-3965 (the disconnected block is the best header)
        if self.best_header == hash {
            self.best_invalid = Some(self.best_header);
            self.best_header = self.entry(&hash).prev.expect("not genesis");
        }
        self.chain.pop();
        let e = self.entries.get_mut(&hash).expect("indexed");
        e.failed_valid = true;
        self.failed.insert(hash);
        self.invalid_chain_found(hash);
    }

    pub fn best_header(&self) -> (u32, Hash) {
        (self.work(&self.best_header), self.best_header)
    }

    pub fn best_invalid(&self) -> Option<(u32, Hash)> {
        self.best_invalid.map(|h| (self.work(&h), h))
    }

    /// The candidate set after an op, by InvalidateBlock's rescan predicate (v:4040-4044), which
    /// every op leaves true in L3's legs: valid to TRANSACTIONS (data, no failure flag), every
    /// ancestor's data too (HaveTxsDownloaded), not worse than the tip. In Core's comparator order
    /// (ascending work, then descending sequence id); equal (work, sequence id) runs are an
    /// unordered group (Q-H), listed here by hash.
    pub fn candidates(&self) -> Vec<Candidate> {
        let tip = self.tip();
        let (tip_work, tip_seq) = (self.work(&tip), self.entry(&tip).seq);
        let mut out: Vec<Candidate> = self
            .entries
            .iter()
            .filter(|(_, e)| e.data && !e.failed_valid && !e.failed_child)
            .filter(|(h, _)| self.txs_downloaded(h))
            .filter(|(_, e)| e.height > tip_work || (e.height == tip_work && e.seq <= tip_seq))
            .map(|(h, e)| (e.height, *h, e.seq))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)).then(a.1.cmp(&b.1)));
        out
    }

    /// [`P7`] after the last op.
    pub fn p7(&self) -> P7 {
        P7 {
            best_header: self.best_header(),
            best_invalid: self.best_invalid(),
            candidates: self.candidates().into_iter().map(|(h, hash, _)| (h, hash)).collect(),
        }
    }

    fn txs_downloaded(&self, hash: &Hash) -> bool {
        let mut at = Some(*hash);
        while let Some(h) = at {
            let e = self.entry(&h);
            if !e.data {
                return false;
            }
            at = e.prev;
        }
        true
    }

    /// The parent's state for the binder's guards (plan §3.5 step 3): AcceptBlockHeader's
    /// "bad-prevblk" (v:4651), or its walk of the failed set from a parent short of SCRIPTS
    /// (v:4684-4699).
    pub fn parent_state(&self, prev: &Hash) -> ParentState {
        let e = self.entry(prev);
        if e.failed_valid || e.failed_child {
            return ParentState::Failed;
        }
        let failed_ancestor =
            self.failed.iter().any(|f| self.ancestor(prev, self.entry(f).height) == Some(*f));
        if !e.scripts && failed_ancestor {
            return ParentState::UnmarkedFailedAncestor;
        }
        ParentState::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Hash {
        [n; 32]
    }

    /// A chain 0-1-2 on genesis 0.
    fn chain() -> Model {
        let mut m = Model::new(h(0));
        m.connect(h(1), h(0));
        m.connect(h(2), h(1));
        m
    }

    #[test]
    fn accepted_blocks_move_tip_and_best_header() {
        let m = chain();
        assert_eq!(m.tip(), h(2));
        assert_eq!(m.best_header(), (2, h(2)));
        assert_eq!(m.best_invalid(), None);
        assert_eq!(m.candidates().iter().map(|c| c.1).collect::<Vec<_>>(), [h(2)]);
    }

    /// A contextual reject at tip+1 takes the best header and keeps it on a failed block; a
    /// later equal-work sibling does not move it (only the hash shows that, plan §1).
    #[test]
    fn a_contextual_reject_keeps_the_best_header() {
        let mut m = chain();
        m.contextual_reject(h(10), h(2), false);
        assert_eq!(m.best_header(), (3, h(10)));
        assert_eq!(m.best_invalid(), None, "no InvalidChainFound");
        m.connect_reject(h(11), h(2), false);
        assert_eq!(m.best_header(), (3, h(10)), "equal work: not moved, not an ancestor");
        assert_eq!(m.best_invalid(), Some((3, h(11))));
        assert_eq!(m.candidates().iter().map(|c| c.1).collect::<Vec<_>>(), [h(2)]);
        assert_eq!(m.parent_state(&h(10)), ParentState::Failed);
    }

    /// A ConnectBlock failure that holds the best header sends it back to the tip.
    #[test]
    fn a_connect_reject_falls_back_to_the_tip() {
        let mut m = chain();
        m.connect_reject(h(11), h(2), false);
        assert_eq!(m.best_header(), (2, h(2)));
        assert_eq!(m.best_invalid(), Some((3, h(11))));
        m.connect_reject(h(12), h(2), true);
        assert_eq!(m.best_header(), (3, h(12)), "MUTATED: indexed, not marked");
        assert_eq!(m.best_invalid(), Some((3, h(11))));
    }

    /// S6 (plan rev5 §4): F′ and C′ headers-only, F′'s block fails ConnectBlock, then a header
    /// on C′ walks to F′ and marks C′ (v:4699); F′'s failure sends the best header back.
    #[test]
    fn a_header_on_an_unmarked_descendant_marks_it() {
        let mut m = chain();
        m.header(h(10), h(2));
        m.header(h(11), h(10));
        assert_eq!(m.best_header(), (4, h(11)));
        assert_eq!(m.parent_state(&h(11)), ParentState::Clean);
        m.connect_reject(h(10), h(2), false);
        assert_eq!(m.best_header(), (2, h(2)), "C′ descends from F′: back to the tip");
        assert_eq!(m.best_invalid(), Some((3, h(10))));
        assert_eq!(m.parent_state(&h(11)), ParentState::UnmarkedFailedAncestor);
        assert!(m.failed_child_walk(&h(11)));
        assert_eq!(m.parent_state(&h(11)), ParentState::Failed);
        assert!(!m.failed_child_walk(&h(2)), "the tip is VALID_SCRIPTS");
        assert_eq!(m.best_header(), (2, h(2)));
        assert_eq!(m.candidates().iter().map(|c| c.1).collect::<Vec<_>>(), [h(2)]);
    }

    /// The rollback: the accepted best header is invalidated, `best_invalid` takes it
    /// unconditionally (v:3962-3965), and a second rollback at equal work leaves it.
    #[test]
    fn invalidating_the_tip_rolls_back() {
        let mut m = chain();
        m.connect(h(20), h(2));
        m.invalidate_tip(h(20));
        assert_eq!(m.tip(), h(2));
        assert_eq!(m.best_header(), (2, h(2)));
        assert_eq!(m.best_invalid(), Some((3, h(20))));
        m.connect(h(21), h(2));
        m.invalidate_tip(h(21));
        assert_eq!(m.best_invalid(), Some((3, h(21))), "the best header: assigned unconditionally");
        m.contextual_reject(h(22), h(2), false);
        m.connect(h(23), h(2));
        assert_eq!(m.best_header(), (3, h(22)), "an equal-work accept does not take it");
        m.invalidate_tip(h(23));
        assert_eq!(
            m.best_invalid(),
            Some((3, h(21))),
            "not the best header: strictly more work only"
        );
        assert_eq!(m.best_header(), (3, h(22)));
        assert_eq!(m.candidates().iter().map(|c| c.1).collect::<Vec<_>>(), [h(2)]);
        assert_eq!(m.parent_state(&h(2)), ParentState::Clean);
    }
}
