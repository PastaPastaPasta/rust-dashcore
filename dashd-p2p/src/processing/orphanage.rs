//! Core v24's transaction orphanage, kept independent of networking and storage.
//!
//! Transactions are indexed by their missing inputs and by the peer that supplied them.  The
//! indexes make block reconsideration and random byte-bounded eviction deterministic when the
//! caller supplies the clock and random stream.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use dash_consensus::{Block, Tx, TxRef};
use dash_p2p::PeerId;
use dash_p2p::rng::FastRandom;
use dash_primitives::{OutPoint, TxHash};
use dash_types::Numeric;

use dash_consensus::tx::MAX_STANDARD_TX_SIZE;
const ORPHAN_TX_EXPIRE_TIME: i64 = 20 * 60;
const ORPHAN_TX_EXPIRE_INTERVAL: i64 = 5 * 60;
const BLOCK_WORK_PEER: PeerId = -1;

/// Random source used by [`TxOrphanage::limit_orphans`].
///
/// The bound is exclusive, matching Core's `FastRandomContext::randrange`.
pub trait OrphanRng {
    /// Returns a position strictly below `upper`; `upper` is always positive.
    fn rand_range(&mut self, upper: usize) -> usize;
}

impl<F> OrphanRng for F
where
    F: FnMut(usize) -> usize,
{
    fn rand_range(&mut self, upper: usize) -> usize {
        self(upper)
    }
}

impl OrphanRng for FastRandom {
    fn rand_range(&mut self, upper: usize) -> usize {
        self.randrange(upper as u64) as usize
    }
}

struct Orphan {
    tx: TxRef,
    peer: PeerId,
    expires: i64,
    list_pos: usize,
    size: usize,
}

/// Transactions rejected for missing inputs, following Core's `TxOrphanage`.
#[derive(Default)]
pub struct TxOrphanage {
    orphans: BTreeMap<[u8; 32], Orphan>,
    by_prevout: BTreeMap<OutPoint, BTreeSet<[u8; 32]>>,
    peer_work: BTreeMap<PeerId, BTreeSet<[u8; 32]>>,
    orphan_list: Vec<[u8; 32]>,
    orphan_bytes: usize,
    next_sweep: i64,
}

impl TxOrphanage {
    /// Creates an empty orphanage. The first `limit_orphans` call performs an expiry sweep.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an orphan at `now`, rejecting duplicate hashes and transactions over Core's standard
    /// transaction size limit.
    /// Core v24.0.0: txorphanage.cpp:20-56 (`TxOrphanage::AddTx`)
    pub fn add_tx(&mut self, tx: TxRef, peer: PeerId, now: i64) -> bool {
        let hash = tx.txid().to_lendian();
        if self.orphans.contains_key(&hash) || tx.size() > MAX_STANDARD_TX_SIZE {
            return false;
        }
        let size = tx.size() as usize;
        let list_pos = self.orphan_list.len();
        self.orphans.insert(
            hash,
            Orphan {
                tx: Arc::clone(&tx),
                peer,
                expires: now + ORPHAN_TX_EXPIRE_TIME,
                list_pos,
                size,
            },
        );
        self.orphan_list.push(hash);
        for input in tx.inputs() {
            self.by_prevout.entry(input.prevout()).or_default().insert(hash);
        }
        self.orphan_bytes += size;
        true
    }

    /// Returns whether an orphan with `txid` is stored.
    pub fn have_tx(&self, txid: &TxHash) -> bool {
        self.orphans.contains_key(&txid.to_lendian())
    }

    /// Returns and removes the next queued orphan for `peer`, skipping stale queue entries.
    /// Core v24.0.0: txorphanage.cpp:180-198 (`TxOrphanage::GetTxToReconsider`)
    pub fn get_tx_to_reconsider(&mut self, peer: PeerId) -> Option<TxRef> {
        let work = self.peer_work.get_mut(&peer)?;
        while let Some(txid) = work.pop_first() {
            if let Some(orphan) = self.orphans.get(&txid) {
                return Some(Arc::clone(&orphan.tx));
            }
        }
        None
    }

    /// Removes an orphan by transaction hash, returning whether it was present.
    /// Core v24.0.0: txorphanage.cpp:59-98 (`TxOrphanage::EraseTx`)
    pub fn erase_tx(&mut self, txid: &TxHash) -> bool {
        let key = txid.to_lendian();
        let Some(orphan) = self.orphans.remove(&key) else {
            return false;
        };
        for input in orphan.tx.inputs() {
            if let Some(set) = self.by_prevout.get_mut(&input.prevout()) {
                set.remove(&key);
                if set.is_empty() {
                    self.by_prevout.remove(&input.prevout());
                }
            }
        }
        let last = self.orphan_list.pop().expect("orphan list follows map");
        if last != key {
            self.orphan_list[orphan.list_pos] = last;
            self.orphans.get_mut(&last).expect("orphan list follows map").list_pos =
                orphan.list_pos;
        }
        self.orphan_bytes -= orphan.size;
        true
    }

    /// Removes every orphan supplied by `peer` and clears that peer's reconsideration queue.
    /// Core v24.0.0: txorphanage.cpp:100-117 (`TxOrphanage::EraseForPeer`)
    pub fn erase_for_peer(&mut self, peer: PeerId) {
        self.peer_work.remove(&peer);
        let ids: Vec<_> = self
            .orphans
            .iter()
            .filter_map(|(id, orphan)| (orphan.peer == peer).then_some(*id))
            .collect();
        for id in ids {
            self.erase_tx(&TxHash::from_lendian(id));
        }
    }

    /// Removes orphans that conflict with an input spent by any transaction in `block`.
    /// Core v24.0.0: txorphanage.cpp:235-264 (`TxOrphanage::EraseForBlock`)
    pub fn erase_for_block(&mut self, block: &Block) {
        let mut ids = Vec::new();
        for tx in block.txs() {
            for input in tx.inputs() {
                if let Some(children) = self.by_prevout.get(&input.prevout()) {
                    ids.extend(children.iter().copied());
                }
            }
        }
        for id in ids {
            self.erase_tx(&TxHash::from_lendian(id));
        }
    }

    /// Queues orphans spending outputs created by transactions in `block` for peer `-1`.
    /// Core v24.0.0: txorphanage.cpp:200-221 (`TxOrphanage::SetCandidatesByBlock`)
    pub fn set_candidates_by_block(&mut self, block: &Block) {
        self.peer_work.entry(BLOCK_WORK_PEER).or_default().clear();
        for tx in block.txs() {
            self.add_children_to_work_set_for_peer(tx, BLOCK_WORK_PEER);
        }
    }

    /// Queues children of `tx` for reconsideration by the peers that announced them.
    /// Core v24.0.0: txorphanage.cpp:155-172 (`TxOrphanage::AddChildrenToWorkSet`)
    pub fn add_children_to_work_set(&mut self, tx: &Tx) {
        let hash = tx.txid();
        for index in 0..tx.outputs().len() {
            let outpoint = OutPoint::new(hash, index as u32);
            let Some(children) = self.by_prevout.get(&outpoint).cloned() else {
                continue;
            };
            for child in children {
                if let Some(orphan) = self.orphans.get(&child) {
                    self.peer_work.entry(orphan.peer).or_default().insert(child);
                }
            }
        }
    }

    /// Whether `peer` has queued reconsideration work, including stale entries.
    ///
    /// Core v24.0.0: txorphanage.cpp:266 (`TxOrphanage::HaveMoreWork`).
    pub fn have_more_work(&self, peer: PeerId) -> bool {
        self.peer_work.get(&peer).is_some_and(|work| !work.is_empty())
    }

    /// Whether `peer` has queued reconsideration work, including stale entries.
    ///
    /// Core v24.0.0: txorphanage.cpp:223 (`TxOrphanage::HaveTxToReconsider`).
    pub fn have_tx_to_reconsider(&self, peer: PeerId) -> bool {
        self.have_more_work(peer)
    }

    /// Number of stored orphans.
    pub fn size(&self) -> usize {
        self.orphans.len()
    }

    /// Sum of serialized orphan sizes.
    pub fn bytes(&self) -> usize {
        self.orphan_bytes
    }

    /// Expires old entries and randomly evicts until the byte limit is met.
    /// Core v24.0.0: txorphanage.cpp:119-153 (`TxOrphanage::LimitOrphans`)
    pub fn limit_orphans<R: OrphanRng + ?Sized>(
        &mut self,
        max_bytes: usize,
        rng: &mut R,
        now: i64,
    ) {
        if self.next_sweep <= now {
            let mut next_expiry = now + ORPHAN_TX_EXPIRE_TIME - ORPHAN_TX_EXPIRE_INTERVAL;
            let ids: Vec<_> =
                self.orphans.iter().map(|(id, orphan)| (*id, orphan.expires)).collect();
            for (id, expires) in ids {
                if expires <= now {
                    self.erase_tx(&TxHash::from_lendian(id));
                } else {
                    next_expiry = next_expiry.min(expires);
                }
            }
            self.next_sweep = next_expiry + ORPHAN_TX_EXPIRE_INTERVAL;
        }
        while self.orphan_bytes > max_bytes {
            let pos = rng.rand_range(self.orphan_list.len());
            let id = self.orphan_list[pos];
            self.erase_tx(&TxHash::from_lendian(id));
        }
    }

    fn add_children_to_work_set_for_peer(&mut self, tx: &Tx, peer: PeerId) {
        let hash = tx.txid();
        for index in 0..tx.outputs().len() {
            let outpoint = OutPoint::new(hash, index as u32);
            if let Some(children) = self.by_prevout.get(&outpoint) {
                self.peer_work.entry(peer).or_default().extend(children.iter().copied());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash_consensus::{Header, decode_tx};
    use dash_primitives::{BlockHash, BlockHeader, MerkleRoot};
    use proptest::prelude::*;

    fn transaction(inputs: &[OutPoint], outputs: usize, script_size: usize, tag: u32) -> TxRef {
        let mut raw = Vec::new();
        raw.extend_from_slice(&1u32.to_le_bytes());
        dash_consensus::primitives::write_compact_size(inputs.len() as u64, &mut raw);
        for input in inputs {
            raw.extend_from_slice(&input.txid().to_lendian());
            raw.extend_from_slice(&input.index().to_le_bytes());
            dash_consensus::primitives::write_compact_size(script_size as u64, &mut raw);
            raw.extend(std::iter::repeat_n(0x51, script_size));
            raw.extend_from_slice(&u32::MAX.to_le_bytes());
        }
        dash_consensus::primitives::write_compact_size(outputs as u64, &mut raw);
        for _ in 0..outputs {
            raw.extend_from_slice(&1i64.to_le_bytes());
            raw.extend_from_slice(&[1, 0x51]);
        }
        raw.extend_from_slice(&tag.to_le_bytes());
        Arc::new(decode_tx(&mut raw.as_slice()).unwrap().into_value())
    }

    fn outpoint(seed: u8, index: u32) -> OutPoint {
        OutPoint::new(TxHash::from_lendian([seed; 32]), index)
    }

    fn tx(seed: u8) -> TxRef {
        transaction(&[outpoint(seed, 0)], 1, 0, u32::from(seed))
    }

    fn child(parent: &Tx, index: u32, tag: u32) -> TxRef {
        transaction(&[OutPoint::new(parent.txid(), index)], 1, 0, tag)
    }

    fn block(txs: Vec<TxRef>) -> Block {
        Block::new(
            Header::new(BlockHeader::new(1, BlockHash::ZERO, MerkleRoot::ZERO, 0, 0, 0)),
            txs,
        )
    }

    fn no_draw(_: usize) -> usize {
        panic!("unexpected eviction draw")
    }

    fn check_indexes(o: &TxOrphanage) {
        assert_eq!(o.orphan_list.len(), o.size());
        assert_eq!(
            o.bytes(),
            o.orphans.values().map(|entry| entry.tx.size() as usize).sum::<usize>()
        );
        let mut by_prevout = BTreeMap::<_, BTreeSet<_>>::new();
        for (pos, id) in o.orphan_list.iter().enumerate() {
            assert_eq!(o.orphans[id].list_pos, pos);
            for input in o.orphans[id].tx.inputs() {
                by_prevout.entry(input.prevout()).or_default().insert(*id);
            }
        }
        assert_eq!(o.by_prevout, by_prevout);
    }

    #[test]
    fn core_dos_map_orphans() {
        let mut o = TxOrphanage::new();
        let mut rng = FastRandom::with_seed([33; 32]);
        let mut added = Vec::new();
        for i in 0..50 {
            let t = tx(i);
            assert!(o.add_tx(Arc::clone(&t), PeerId::from(i), 0));
            added.push(t);
        }
        for i in 0..50 {
            let parent = &added[rng.randrange(added.len() as u64) as usize];
            let t = child(parent, 0, 100 + i);
            assert!(o.add_tx(Arc::clone(&t), PeerId::from(i), 0));
            added.push(t);
        }
        for i in 0..10 {
            let large =
                transaction(&(0..2777).map(|n| outpoint(i, n)).collect::<Vec<_>>(), 1, 73, 0);
            assert!(!o.add_tx(large, PeerId::from(i), 0));
        }
        for peer in 0..3 {
            let before = o.size();
            o.erase_for_peer(peer);
            assert_eq!(o.size(), before - 2);
            check_indexes(&o);
        }
        for limit in [40, 10, 0] {
            o.limit_orphans(limit, &mut rng, 0);
            assert!(o.bytes() <= limit);
            assert!(o.size() <= limit);
            check_indexes(&o);
        }
        assert_eq!(o.size(), 0);
    }

    #[test]
    fn size_boundary_duplicate_and_exact_byte_budget() {
        let base = transaction(&[outpoint(1, 0)], 1, 99_935, 0);
        assert_eq!(base.size(), MAX_STANDARD_TX_SIZE);
        let over = transaction(&[outpoint(1, 0)], 1, 99_936, 0);
        let mut o = TxOrphanage::new();
        assert!(!o.add_tx(over, 1, 0));
        assert!(o.add_tx(Arc::clone(&base), 1, 0));
        assert!(!o.add_tx(Arc::clone(&base), 2, 10_000));
        o.limit_orphans(base.size() as usize, &mut no_draw, 0);
        assert_eq!(o.bytes(), 100_000);
        assert!(o.erase_tx(&base.txid()));
        assert!(!o.erase_tx(&base.txid()));
        assert!(o.add_tx(tx(2), 2, 0));
        assert!(o.add_tx(transaction(&[outpoint(3, 0)], 1, 50, 0), 3, 0));
        let mut bounds = Vec::new();
        o.limit_orphans(
            61,
            &mut |upper| {
                bounds.push(upper);
                1
            },
            0,
        );
        assert_eq!(bounds, [2]);
        assert_eq!(o.bytes(), 61);
        check_indexes(&o);
    }

    #[test]
    fn core_set_candidates_by_block_uses_outputs() {
        let parent = transaction(&[outpoint(1, 0)], 3, 1, 0);
        let orphan = child(&parent, 2, 0);
        let mut o = TxOrphanage::new();
        assert!(o.add_tx(Arc::clone(&orphan), 128, 0));
        o.set_candidates_by_block(&block(vec![parent]));
        assert!(!o.have_more_work(128));
        assert!(o.have_tx_to_reconsider(-1));
        let reconsider = o.get_tx_to_reconsider(-1).unwrap();
        assert!(o.have_tx(&reconsider.txid()));
        assert_eq!(reconsider.txid(), orphan.txid());
        assert!(o.get_tx_to_reconsider(-1).is_none());
    }

    #[test]
    fn sender_queues_deduplicate_sort_and_keep_stale_entries() {
        let parent = transaction(&[outpoint(1, 0)], 2, 0, 0);
        let a = child(&parent, 0, 1);
        let b = child(&parent, 1, 2);
        let both = transaction(
            &[OutPoint::new(parent.txid(), 0), OutPoint::new(parent.txid(), 1)],
            1,
            0,
            3,
        );
        let invalid_index = child(&parent, 2, 4);
        let mut o = TxOrphanage::new();
        for (t, peer) in [(&a, 7), (&b, 8), (&both, 7), (&invalid_index, 7)] {
            assert!(o.add_tx(Arc::clone(t), peer, 0));
        }
        o.add_children_to_work_set(&parent);
        o.add_children_to_work_set(&parent);
        assert!(o.have_more_work(7));
        assert!(o.have_more_work(8));
        assert!(!o.have_more_work(-1));
        let mut actual = Vec::new();
        while let Some(t) = o.get_tx_to_reconsider(7) {
            actual.push(t.txid().to_lendian());
        }
        let mut expected = vec![a.txid().to_lendian(), both.txid().to_lendian()];
        expected.sort();
        assert_eq!(actual, expected);
        assert!(o.erase_tx(&b.txid()));
        assert!(o.have_tx_to_reconsider(8));
        assert!(o.have_more_work(8));
        assert!(o.get_tx_to_reconsider(8).is_none());
        assert!(!o.have_more_work(8));
        assert_eq!(o.size(), 3);
        check_indexes(&o);
    }

    #[test]
    fn block_candidates_replace_only_block_queue_and_peer_disconnect_keeps_stale_block_work() {
        let parent = tx(1);
        let other = tx(2);
        let a = child(&parent, 0, 1);
        let b = child(&other, 0, 2);
        let mut o = TxOrphanage::new();
        assert!(o.add_tx(Arc::clone(&a), 7, 0));
        assert!(o.add_tx(Arc::clone(&b), 8, 0));
        o.add_children_to_work_set(&parent);
        o.set_candidates_by_block(&block(vec![Arc::clone(&parent)]));
        o.set_candidates_by_block(&block(vec![Arc::clone(&other)]));
        assert_eq!(o.get_tx_to_reconsider(-1).unwrap().txid(), b.txid());
        assert!(o.get_tx_to_reconsider(-1).is_none());
        assert_eq!(o.get_tx_to_reconsider(7).unwrap().txid(), a.txid());
        o.set_candidates_by_block(&block(vec![parent, other]));
        o.erase_for_peer(7);
        assert!(!o.have_more_work(7));
        assert!(!o.have_tx(&a.txid()));
        assert!(o.have_tx(&b.txid()));
        assert_eq!(o.get_tx_to_reconsider(-1).unwrap().txid(), b.txid());
        assert!(o.get_tx_to_reconsider(-1).is_none());
        o.set_candidates_by_block(&block(Vec::new()));
        assert!(!o.have_more_work(-1));
        check_indexes(&o);
    }

    #[test]
    fn erase_for_block_removes_included_and_conflicting_but_not_children() {
        let mined = tx(1);
        let conflict = transaction(&[outpoint(1, 0), outpoint(1, 0)], 1, 0, 9);
        let dependent = child(&mined, 0, 2);
        let unrelated = tx(3);
        let mut o = TxOrphanage::new();
        for t in [&mined, &conflict, &dependent, &unrelated] {
            assert!(o.add_tx(Arc::clone(t), 7, 0));
        }
        o.erase_for_block(&block(vec![mined, tx(1)]));
        assert!(!o.have_tx(&conflict.txid()));
        assert!(o.have_tx(&dependent.txid()));
        assert!(o.have_tx(&unrelated.txid()));
        assert_eq!(o.size(), 2);
        check_indexes(&o);
    }

    #[test]
    fn expiry_is_batched_and_sweep_removals_use_core_hash_order() {
        let mut o = TxOrphanage::new();
        let a = tx(1);
        let b = tx(2);
        assert!(o.add_tx(Arc::clone(&a), 1, 0));
        o.limit_orphans(usize::MAX, &mut no_draw, 0);
        assert_eq!(o.next_sweep, 1200);
        assert!(!o.add_tx(Arc::clone(&a), 2, 500));
        assert!(o.add_tx(Arc::clone(&b), 2, 100));
        o.limit_orphans(usize::MAX, &mut no_draw, 1199);
        assert_eq!(o.size(), 2);
        o.limit_orphans(usize::MAX, &mut no_draw, 1200);
        assert!(!o.have_tx(&a.txid()));
        assert_eq!(o.next_sweep, 1600);
        o.limit_orphans(usize::MAX, &mut no_draw, 1300);
        assert!(o.have_tx(&b.txid()));
        o.limit_orphans(usize::MAX, &mut no_draw, 1599);
        assert_eq!(o.size(), 1);
        o.limit_orphans(usize::MAX, &mut no_draw, 1600);
        assert_eq!(o.size(), 0);
        check_indexes(&o);
    }

    #[test]
    fn random_eviction_uses_swap_with_last_positions() {
        let mut o = TxOrphanage::new();
        let txs: Vec<_> = (0..5).map(tx).collect();
        for t in &txs {
            assert!(o.add_tx(Arc::clone(t), 1, 0));
        }
        assert!(o.erase_tx(&txs[1].txid()));
        let mut bounds = Vec::new();
        let mut draws = [1, 0].into_iter();
        o.limit_orphans(
            2 * txs[0].size() as usize,
            &mut |upper| {
                bounds.push(upper);
                draws.next().unwrap()
            },
            0,
        );
        assert_eq!(bounds, [4, 3]);
        assert!(!o.have_tx(&txs[4].txid()));
        assert!(!o.have_tx(&txs[0].txid()));
        assert!(o.have_tx(&txs[2].txid()));
        assert!(o.have_tx(&txs[3].txid()));
        check_indexes(&o);
    }

    struct NaiveEntry {
        tx: TxRef,
        peer: PeerId,
        expires: i64,
    }

    #[derive(Default)]
    struct Naive {
        entries: Vec<NaiveEntry>,
        work: BTreeMap<PeerId, BTreeSet<[u8; 32]>>,
        next_sweep: i64,
    }

    impl Naive {
        fn erase(&mut self, id: TxHash) -> bool {
            if let Some(pos) = self.entries.iter().position(|e| e.tx.txid() == id) {
                self.entries.swap_remove(pos);
                true
            } else {
                false
            }
        }

        fn bytes(&self) -> usize {
            self.entries.iter().map(|e| e.tx.size() as usize).sum()
        }

        fn add_children(&mut self, parent: &Tx, peer: Option<PeerId>) {
            for e in &self.entries {
                if e.tx.inputs().iter().any(|i| {
                    i.prevout().txid() == parent.txid()
                        && (i.prevout().index() as usize) < parent.outputs().len()
                }) {
                    self.work
                        .entry(peer.unwrap_or(e.peer))
                        .or_default()
                        .insert(e.tx.txid().to_lendian());
                }
            }
        }

        fn take(&mut self, peer: PeerId) -> Option<TxHash> {
            let work = self.work.get_mut(&peer)?;
            while let Some(id) = work.pop_first() {
                if self.entries.iter().any(|e| e.tx.txid().to_lendian() == id) {
                    return Some(TxHash::from_lendian(id));
                }
            }
            None
        }

        fn limit(&mut self, max: usize, rng: &mut FastRandom, now: i64) {
            if self.next_sweep <= now {
                let mut expired: Vec<_> = self
                    .entries
                    .iter()
                    .filter(|e| e.expires <= now)
                    .map(|e| e.tx.txid().to_lendian())
                    .collect();
                expired.sort();
                for id in expired {
                    self.erase(TxHash::from_lendian(id));
                }
                self.next_sweep =
                    self.entries.iter().map(|e| e.expires).fold(now + 900, i64::min) + 300;
            }
            while self.bytes() > max {
                let pos = rng.randrange(self.entries.len() as u64) as usize;
                self.entries.swap_remove(pos);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]
        #[test]
        fn naive_model_same_rng_same_survivors(
            seed in any::<[u8; 32]>(),
            ops in prop::collection::vec((0u8..9, 0u8..32, 0u8..5, 0u16..2500), 1..150),
        ) {
            let mut indexed = TxOrphanage::new();
            let mut naive = Naive::default();
            let mut rng = FastRandom::with_seed(seed);
            let mut model_rng = FastRandom::with_seed(seed);
            let parent = transaction(&[outpoint(254, 0)], 3, 0, 0);
            let txs: Vec<_> = (0..32).map(|i| child(&parent, i % 4, i)).collect();
            for (op, n, p, time) in ops {
                let t = &txs[n as usize];
                let peer = PeerId::from(p);
                let now = i64::from(time);
                match op {
                    0 | 1 => {
                        let exists = naive.entries.iter().any(|e| e.tx.txid() == t.txid());
                        prop_assert_eq!(indexed.add_tx(Arc::clone(t), peer, now), !exists);
                        if !exists { naive.entries.push(NaiveEntry { tx: Arc::clone(t), peer, expires: now + 1200 }); }
                    }
                    2 => { prop_assert_eq!(indexed.erase_tx(&t.txid()), naive.erase(t.txid())); }
                    3 => {
                        indexed.erase_for_peer(peer);
                        naive.work.remove(&peer);
                        let mut ids: Vec<_> = naive.entries.iter().filter(|e| e.peer == peer)
                            .map(|e| e.tx.txid().to_lendian()).collect();
                        ids.sort();
                        for id in ids { naive.erase(TxHash::from_lendian(id)); }
                    }
                    4 => {
                        let max = usize::from(time) % 800;
                        indexed.limit_orphans(max, &mut rng, now);
                        naive.limit(max, &mut model_rng, now);
                    }
                    5 => { indexed.add_children_to_work_set(&parent); naive.add_children(&parent, None); }
                    6 => {
                        indexed.set_candidates_by_block(&block(vec![Arc::clone(&parent)]));
                        naive.work.entry(-1).or_default().clear();
                        naive.add_children(&parent, Some(-1));
                    }
                    7 => { prop_assert_eq!(indexed.get_tx_to_reconsider(peer).map(|t| t.txid()), naive.take(peer)); }
                    _ => { prop_assert_eq!(indexed.get_tx_to_reconsider(-1).map(|t| t.txid()), naive.take(-1)); }
                }
                prop_assert_eq!(indexed.size(), naive.entries.len());
                prop_assert_eq!(indexed.bytes(), naive.bytes());
                for t in &txs {
                    prop_assert_eq!(indexed.have_tx(&t.txid()), naive.entries.iter().any(|e| e.tx.txid() == t.txid()));
                }
                for peer in -1..5 {
                    let expected = naive.work.get(&peer).is_some_and(|s| !s.is_empty());
                    prop_assert_eq!(indexed.have_more_work(peer), expected);
                    prop_assert_eq!(indexed.have_tx_to_reconsider(peer), expected);
                }
                prop_assert_eq!(rng.keystream_bytes(), model_rng.keystream_bytes());
                prop_assert_eq!(rng.bitbuf(), model_rng.bitbuf());
                check_indexes(&indexed);
            }
        }
    }
}
