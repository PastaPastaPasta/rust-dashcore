//! Core's `indexed_modified_transaction_set`: the packages whose ancestors are partly in the block.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use dash_consensus::primitives::core_hash_cmp;

use crate::view::{EntryId, MiningEntry, PackageView};

/// What `CompareTxMemPoolEntryByAncestorFee` reads of a mempool or modified entry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AncestorScore {
    pub modified_fee: i64,
    pub tx_size: u64,
    pub size_with_ancestors: u64,
    pub mod_fees_with_ancestors: i64,
    pub txid: [u8; 32],
}

impl AncestorScore {
    /// `CTxMemPoolModifiedEntry(iter)`'s view, or the mempool entry's own.
    pub(crate) fn of(e: &MiningEntry<'_>) -> Self {
        Self {
            modified_fee: e.modified_fee,
            tx_size: e.tx_size,
            size_with_ancestors: e.size_with_ancestors,
            mod_fees_with_ancestors: e.mod_fees_with_ancestors,
            txid: *e.txid.as_bytes(),
        }
    }

    // Core v24.0.2 src/txmempool.h:181-195 GetModFeeAndSize
    fn mod_fee_and_size(&self) -> (f64, f64) {
        let f1 = self.modified_fee as f64 * self.size_with_ancestors as f64;
        let f2 = self.mod_fees_with_ancestors as f64 * self.tx_size as f64;
        if f1 > f2 {
            (self.mod_fees_with_ancestors as f64, self.size_with_ancestors as f64)
        } else {
            (self.modified_fee as f64, self.tx_size as f64)
        }
    }
}

/// `Less` means a sorts first (the better package).
// Core v24.0.2 src/txmempool.h:158-177 CompareTxMemPoolEntryByAncestorFee
impl Ord for AncestorScore {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a_fee, a_size) = self.mod_fee_and_size();
        let (b_fee, b_size) = other.mod_fee_and_size();
        let f1 = a_fee * b_size;
        let f2 = a_size * b_fee;
        if f1 == f2 {
            return core_hash_cmp(&self.txid, &other.txid);
        }
        if f1 > f2 {
            Ordering::Less
        } else {
            Ordering::Greater
        }
    }
}

impl PartialOrd for AncestorScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for AncestorScore {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for AncestorScore {}

/// `CTxMemPoolModifiedEntry` (Core v24.0.2 src/node/miner.h:60-79).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ModifiedEntry {
    pub score: AncestorScore,
    pub sigops_with_ancestors: u32,
}

/// Core v24.0.2 src/node/miner.h:114-129: by id, and by ancestor score. Rounded doubles can make
/// the score order cyclic, where a keyed `BTreeSet` search can miss; Core erases by iterator, so
/// removal here falls back to a scan by id.
#[derive(Default)]
pub(crate) struct ModifiedSet {
    by_id: HashMap<EntryId, ModifiedEntry>,
    by_score: BTreeSet<(AncestorScore, EntryId)>,
}

impl ModifiedSet {
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn contains(&self, id: EntryId) -> bool {
        self.by_id.contains_key(&id)
    }

    pub fn best(&mut self) -> Option<(EntryId, ModifiedEntry)> {
        debug_assert_eq!(self.by_score.len(), self.by_id.len());
        while let Some(&(_, id)) = self.by_score.first() {
            if let Some(entry) = self.by_id.get(&id) {
                return Some((id, *entry));
            }
            self.by_score.pop_first();
        }
        None
    }

    pub fn remove(&mut self, id: EntryId) {
        if let Some(entry) = self.by_id.remove(&id) {
            self.unindex(entry.score, id);
        }
    }

    fn unindex(&mut self, score: AncestorScore, id: EntryId) {
        if !self.by_score.remove(&(score, id)) {
            self.by_score.retain(|(_, other)| *other != id);
        }
    }

    /// Core v24.0.2 src/node/miner.cpp:479-484 with `update_for_parent_inclusion`
    /// (src/node/miner.h:134-146).
    pub fn update_for_parent_inclusion<V: PackageView + ?Sized>(
        &mut self,
        view: &V,
        desc: EntryId,
        parent: EntryId,
    ) {
        let mut entry = match self.by_id.remove(&desc) {
            Some(entry) => {
                self.unindex(entry.score, desc);
                entry
            }
            None => {
                let e = view.mining_entry(desc);
                ModifiedEntry {
                    score: AncestorScore::of(&e),
                    sigops_with_ancestors: e.sigops_with_ancestors,
                }
            }
        };
        let p = view.mining_entry(parent);
        entry.score.mod_fees_with_ancestors =
            entry.score.mod_fees_with_ancestors.wrapping_sub(p.modified_fee);
        entry.score.size_with_ancestors = entry.score.size_with_ancestors.wrapping_sub(p.tx_size);
        entry.sigops_with_ancestors = entry.sigops_with_ancestors.wrapping_sub(p.sigops);
        self.by_score.insert((entry.score, desc));
        self.by_id.insert(desc, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn score(fee: i64, size: u64, txid_first_byte: u8) -> AncestorScore {
        let mut txid = [0; 32];
        txid[0] = txid_first_byte;
        AncestorScore {
            modified_fee: fee,
            tx_size: size,
            size_with_ancestors: size,
            mod_fees_with_ancestors: fee,
            txid,
        }
    }

    /// `a` pays exactly one duff·byte more than `b` (`F1·10000 − F2·9999 = 1`), but both products
    /// are near 1e19, where a double's spacing is 2048: Core's doubles call it a tie and the
    /// txid decides, so `b`, with the smaller txid, sorts first.
    #[test]
    fn double_rounding_ties_fall_to_the_txid() {
        let m = 100_000_000_000i64;
        let (f1, s1, f2, s2) = (1 + 9_999 * m, 9_999u64, 1 + 10_000 * m, 10_000u64);
        assert!(f1 <= dash_consensus::MAX_MONEY && f2 <= dash_consensus::MAX_MONEY);
        assert_eq!(i128::from(f1) * i128::from(s2) - i128::from(f2) * i128::from(s1), 1);
        assert_eq!(f1 as f64 * s2 as f64, s1 as f64 * f2 as f64);
        let a = score(f1, s1, 2);
        let b = score(f2, s2, 1);
        assert_eq!(b.cmp(&a), Ordering::Less);
        assert_eq!(a.cmp(&b), Ordering::Greater);
    }

    /// Equal rates at different sizes are an exact tie: the raw txid bytes decide, byte 0 first.
    #[test]
    fn exact_ties_order_by_raw_txid_bytes() {
        let mut low = score(200, 200, 0);
        low.txid[31] = 1;
        let high = score(100, 100, 1);
        assert_eq!(low.cmp(&high), Ordering::Less);
        assert_eq!(high.cmp(&low), Ordering::Greater);
        assert_eq!(score(300, 100, 9).cmp(&score(200, 100, 1)), Ordering::Less);
    }

    /// The min of the entry's own rate and its package rate is what is compared.
    #[test]
    fn package_rate_caps_a_rich_child() {
        let mut child = score(300, 100, 0);
        child.size_with_ancestors = 200;
        child.mod_fees_with_ancestors = 400;
        let between = score(250, 100, 1);
        assert_eq!(between.cmp(&child), Ordering::Less);
    }
}
