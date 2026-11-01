//! The P7 harness model (plan §3.7): each leg's index as Core's rules move it, for the best
//! header, `best_invalid` and the candidate set after every op. V is held to it always; Core is,
//! through the shim replay, for the best header now and for the other two at the ABI 33 pin.
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
        // Core: node/blockstorage.cpp:85-119 (strictly more work)
        if height > self.work(&self.best_header) {
            self.best_header = hash;
        }
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
    /// else (v:4824-4832).
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

    /// InvalidChainFound (v:2040-2050).
    fn invalid_chain_found(&mut self, hash: Hash) {
        if self.best_invalid.is_none_or(|b| self.work(&hash) > self.work(&b)) {
            self.best_invalid = Some(hash);
        }
        if self.ancestor(&self.best_header, self.entry(&hash).height) == Some(hash) {
            self.best_header = self.tip();
        }
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

    /// `invalidateblock` of the tip (L3's rollback): InvalidateBlock (v:3895-4052), then the
    /// RPC's ActivateBestChain, which finds nothing better than the parent in L3's legs.
    pub fn invalidate_tip(&mut self, hash: Hash) {
        assert_eq!(hash, self.tip(), "L3 rolls back its tip only");
        // Core: v:3957-3960 (the disconnected block is the best header)
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

    /// The candidate set after an op, by InvalidateBlock's rescan predicate (v:4035-4039), which
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
    /// "bad-prevblk" (v:4646), or its walk of the failed set from a parent short of SCRIPTS
    /// (v:4679-4694).
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

    /// The rollback: the accepted best header is invalidated, `best_invalid` takes it
    /// unconditionally (v:3957-3960), and a second rollback at equal work leaves it.
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
