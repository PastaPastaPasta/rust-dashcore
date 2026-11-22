use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

use dashcore::{OutPoint, Transaction, Txid};

use crate::transaction_checking::BlockInfo;

/// The most dismissed transactions kept; the oldest goes first.
const MAX_DISMISSED: usize = 4_096;

/// The most transactions kept dismissed for what they spend; the oldest goes first.
const MAX_INHERITED: usize = 4_096;

/// The most outpoints spent by locked transactions kept; the oldest goes first.
const MAX_LOCKED_SPENDS: usize = 16_384;

/// The most outpoints spent in blocks kept with their spender; the oldest goes first.
pub(crate) const MAX_OBSERVED_SPENDERS: usize = 16_384;

/// Unconfirmed transactions the wallet dropped as dead: losers of a conflict
/// a final transaction won, and transactions the user abandoned. A peer, or
/// the mempool replay, delivering one again unconfirmed and unlocked brings
/// back nothing.
#[derive(Debug, Clone, Default)]
pub(crate) struct DismissedTxids {
    order: VecDeque<Txid>,
    set: HashSet<Txid>,
}

impl DismissedTxids {
    pub(crate) fn insert(&mut self, txid: Txid) {
        if !self.set.insert(txid) {
            return;
        }
        self.order.push_back(txid);
        if self.order.len() > MAX_DISMISSED {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }

    pub(crate) fn contains(&self, txid: &Txid) -> bool {
        self.set.contains(txid)
    }

    pub(crate) fn remove(&mut self, txid: &Txid) {
        if self.set.remove(txid) {
            self.order.retain(|t| t != txid);
        }
    }
}

/// Unconfirmed transactions dropped as dead for what they spend, as a peer
/// delivered them: a spend of a dismissed transaction, or of what a locked
/// transaction spends. Kept apart from [`DismissedTxids`], so that however
/// many a peer sends, none displaces a transaction the wallet itself dropped.
/// One is no longer dismissed once every dismissed transaction it spends is
/// mined, or no longer dismissed for that reason either.
#[derive(Debug, Clone, Default)]
pub(crate) struct InheritedDismissals {
    order: VecDeque<(Txid, u64)>,
    parents: HashMap<Txid, (u64, Vec<Txid>)>,
    spends: HashMap<Txid, HashSet<Txid>>,
    next: u64,
}

impl InheritedDismissals {
    /// Dismiss `txid` for spending the dismissed `parents` (none for a spend
    /// of what a locked transaction spends).
    pub(crate) fn insert(&mut self, txid: Txid, parents: Vec<Txid>) {
        if self.parents.contains_key(&txid) {
            return;
        }
        for parent in &parents {
            self.spends.entry(*parent).or_default().insert(txid);
        }
        let seq = self.next;
        self.next += 1;
        self.parents.insert(txid, (seq, parents));
        self.order.push_back((txid, seq));
        while self.parents.len() > MAX_INHERITED {
            if let Some((oldest, seq)) = self.order.pop_front() {
                if self.is_live(&oldest, seq) {
                    self.forget(&oldest);
                }
            }
        }
        if self.order.len() > 2 * MAX_INHERITED {
            let parents = &self.parents;
            self.order.retain(|(t, seq)| parents.get(t).is_some_and(|(s, _)| s == seq));
        }
    }

    fn is_live(&self, txid: &Txid, seq: u64) -> bool {
        self.parents.get(txid).is_some_and(|(s, _)| *s == seq)
    }

    pub(crate) fn contains(&self, txid: &Txid) -> bool {
        self.parents.contains_key(txid)
    }

    pub(crate) fn remove(&mut self, txid: &Txid) {
        self.forget(txid);
    }

    fn forget(&mut self, txid: &Txid) -> bool {
        let Some((_, parents)) = self.parents.remove(txid) else {
            return false;
        };
        for parent in parents {
            if let Some(spends) = self.spends.get_mut(&parent) {
                spends.remove(txid);
                if spends.is_empty() {
                    self.spends.remove(&parent);
                }
            }
        }
        true
    }

    /// `txid` was mined or locked: what is dismissed only for spending it,
    /// or for spending one of those in turn, is not any more. Returns those.
    pub(crate) fn release_spends_of(&mut self, txid: &Txid) -> Vec<Txid> {
        let mut freed = Vec::new();
        let mut released = vec![*txid];
        while let Some(parent) = released.pop() {
            for spend in self.spends.remove(&parent).unwrap_or_default() {
                let Some((_, parents)) = self.parents.get_mut(&spend) else {
                    continue;
                };
                parents.retain(|p| *p != parent);
                if parents.is_empty() {
                    self.remove(&spend);
                    released.push(spend);
                    freed.push(spend);
                }
            }
        }
        freed
    }
}

/// The outpoints InstantSend-locked transactions spend, each with its
/// spender. A lock is final (DIP-0010), so another transaction spending one of
/// them can never confirm.
#[derive(Debug, Clone, Default)]
pub(crate) struct LockedSpends {
    order: VecDeque<OutPoint>,
    spenders: HashMap<OutPoint, Txid>,
}

impl LockedSpends {
    pub(crate) fn insert(&mut self, tx: &Transaction, txid: Txid) {
        for input in &tx.input {
            if self.spenders.insert(input.previous_output, txid).is_none() {
                self.order.push_back(input.previous_output);
            }
        }
        while self.order.len() > MAX_LOCKED_SPENDS {
            if let Some(oldest) = self.order.pop_front() {
                self.spenders.remove(&oldest);
            }
        }
    }

    /// Whether `tx` spends an outpoint a locked transaction other than itself spends.
    pub(crate) fn conflicts(&self, tx: &Transaction, txid: &Txid) -> bool {
        tx.input.iter().any(|input| {
            self.spenders.get(&input.previous_output).is_some_and(|spender| spender != txid)
        })
    }
}

/// The outpoints spent in processed blocks that an InstantSend-locked
/// transaction of the wallet spends too, each with its spender, its block and
/// the claimant it beat, if any, until a ChainLock covers that block: the
/// spend is then final, and what it beat is swept. One that goes first at
/// capacity is only forgotten.
#[derive(Debug, Clone, Default)]
pub(crate) struct ObservedSpenders {
    order: VecDeque<OutPoint>,
    spenders: HashMap<OutPoint, (Txid, BlockInfo, Option<Txid>)>,
}

/// The spends of one spender taken out of [`ObservedSpenders`]: its block,
/// the outpoints, and the claimants they beat.
pub(crate) type TakenSpends = (BlockInfo, BTreeSet<OutPoint>, BTreeSet<Txid>);

impl ObservedSpenders {
    pub(crate) fn insert(
        &mut self,
        outpoint: OutPoint,
        spender: Txid,
        block: BlockInfo,
        claimant: Option<Txid>,
    ) {
        match self.spenders.entry(outpoint) {
            Entry::Occupied(mut kept) => {
                let beaten = kept.get().2.filter(|_| kept.get().0 == spender);
                kept.insert((spender, block, claimant.or(beaten)));
            }
            Entry::Vacant(slot) => {
                slot.insert((spender, block, claimant));
                self.order.push_back(outpoint);
            }
        }
        while self.order.len() > MAX_OBSERVED_SPENDERS {
            if let Some(oldest) = self.order.pop_front() {
                self.spenders.remove(&oldest);
            }
        }
    }

    /// Takes out the spends in blocks at or below `height`, by spender.
    pub(crate) fn take_up_to(&mut self, height: u32) -> BTreeMap<Txid, TakenSpends> {
        self.take(|block| block.height() <= height)
    }

    /// Forgets the spends in blocks above `height`.
    pub(crate) fn truncate_above(&mut self, height: u32) {
        self.take(|block| block.height() > height);
    }

    fn take(&mut self, taken: impl Fn(&BlockInfo) -> bool) -> BTreeMap<Txid, TakenSpends> {
        let mut out: BTreeMap<Txid, TakenSpends> = BTreeMap::new();
        self.spenders.retain(|outpoint, (txid, block, spared)| {
            if !taken(block) {
                return true;
            }
            let entry =
                out.entry(*txid).or_insert_with(|| (*block, BTreeSet::new(), BTreeSet::new()));
            entry.1.insert(*outpoint);
            entry.2.extend(*spared);
            false
        });
        if !out.is_empty() {
            self.order.retain(|outpoint| self.spenders.contains_key(outpoint));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::hashes::Hash;

    #[test]
    fn the_oldest_dismissal_goes_first_at_capacity() {
        let txid = |n: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            Txid::from_byte_array(bytes)
        };
        let mut dismissed = DismissedTxids::default();
        for n in 0..=MAX_DISMISSED as u32 {
            dismissed.insert(txid(n));
        }
        assert!(!dismissed.contains(&txid(0)));
        assert!(dismissed.contains(&txid(1)));
        assert!(dismissed.contains(&txid(MAX_DISMISSED as u32)));
        dismissed.remove(&txid(1));
        assert!(!dismissed.contains(&txid(1)));
        assert_eq!(dismissed.order.len(), MAX_DISMISSED - 1);
    }

    #[test]
    fn observed_spenders_are_bounded_and_taken_by_height() {
        let outpoint = |n: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            OutPoint::new(Txid::from_byte_array(bytes), 0)
        };
        let block =
            |height: u32| BlockInfo::new(height, dashcore::BlockHash::from_byte_array([0; 32]), 0);
        let mut spenders = ObservedSpenders::default();
        for n in 0..=MAX_OBSERVED_SPENDERS as u32 {
            spenders.insert(outpoint(n), outpoint(n + 1).txid, block(100 + n % 2), None);
        }
        assert_eq!(spenders.spenders.len(), MAX_OBSERVED_SPENDERS);
        assert!(!spenders.spenders.contains_key(&outpoint(0)), "the oldest goes first");

        spenders.truncate_above(100);
        assert_eq!(spenders.spenders.len(), MAX_OBSERVED_SPENDERS / 2);
        assert!(spenders.take_up_to(99).is_empty());
        let taken = spenders.take_up_to(100);
        assert_eq!(taken.len(), MAX_OBSERVED_SPENDERS / 2);
        assert!(taken.values().all(|(block, spent, spared)| {
            block.height() == 100 && spent.len() == 1 && spared.is_empty()
        }));
        assert!(spenders.spenders.is_empty() && spenders.order.is_empty());
    }

    #[test]
    fn a_mined_parent_releases_what_is_dismissed_only_for_spending_it() {
        let txid = |n: u32| {
            let mut bytes = [0u8; 32];
            bytes[..4].copy_from_slice(&n.to_le_bytes());
            Txid::from_byte_array(bytes)
        };
        let mut inherited = InheritedDismissals::default();
        inherited.insert(txid(2), vec![txid(1)]);
        inherited.insert(txid(3), vec![txid(2)]);
        inherited.insert(txid(4), vec![txid(1), txid(9)]);
        inherited.insert(txid(5), Vec::new());
        inherited.release_spends_of(&txid(1));
        assert!(!inherited.contains(&txid(2)));
        assert!(!inherited.contains(&txid(3)), "its spend goes with it");
        assert!(inherited.contains(&txid(4)), "it spends another dismissed one");
        assert!(inherited.contains(&txid(5)));
        inherited.release_spends_of(&txid(9));
        assert!(!inherited.contains(&txid(4)));
        assert_eq!(inherited.parents.keys().collect::<Vec<_>>(), vec![&txid(5)]);
        assert!(inherited.spends.is_empty());

        for n in 10..=10 + MAX_INHERITED as u32 {
            inherited.insert(txid(n), vec![txid(1)]);
        }
        assert!(!inherited.contains(&txid(5)), "the oldest goes first");
        assert_eq!(inherited.spends[&txid(1)].len(), MAX_INHERITED);

        inherited.remove(&txid(11));
        inherited.insert(txid(11), vec![txid(1)]);
        inherited.insert(txid(20_000), vec![txid(1)]);
        assert!(inherited.contains(&txid(11)), "one dismissed again takes its new place");
        assert!(!inherited.contains(&txid(12)));
        for _ in 0..3 * MAX_INHERITED {
            inherited.remove(&txid(11));
            inherited.insert(txid(11), vec![txid(1)]);
        }
        assert!(inherited.order.len() <= 2 * MAX_INHERITED);
        assert_eq!(inherited.parents.len(), MAX_INHERITED);
    }
}
