use std::collections::{HashMap, HashSet, VecDeque};

use dashcore::Txid;

/// The most dismissed transactions kept; the oldest goes first.
const MAX_DISMISSED: usize = 4_096;

/// The most spends of dismissed transactions kept; the oldest goes first.
const MAX_SPENDS: usize = 4_096;

/// The most dismissed transactions one spend is released by.
const MAX_PARENTS: usize = 16;

/// Unconfirmed transactions the wallet dropped as dead: losers of a conflict
/// a final transaction won, and transactions the user abandoned. A peer, or
/// the mempool replay, delivering one again unconfirmed and unlocked brings
/// back nothing.
///
/// A spend of a dismissed transaction, seen unlocked, is held back with it.
/// Anyone can send such spends, so they are kept apart from the wallet's own
/// dismissals and cannot push one out. A spend is held back only while a
/// transaction it spends is: a lock or block settling that transaction
/// releases it. A released spend is still remembered, so its own lock has it
/// fetched.
#[derive(Debug, Clone, Default)]
pub(crate) struct DismissedTxids {
    own: Fifo<MAX_DISMISSED>,
    spends: Fifo<MAX_SPENDS>,
    /// For each remembered spend, the dismissed transactions it spends that
    /// are not yet settled. Empty once released.
    held_by: HashMap<Txid, HashSet<Txid>>,
    /// Spends of more dismissed transactions than [`MAX_PARENTS`]: held back
    /// until they are locked or mined themselves, or pushed out.
    pinned: HashSet<Txid>,
    /// For each dismissed transaction, the remembered spends it holds back.
    holds: HashMap<Txid, HashSet<Txid>>,
}

impl DismissedTxids {
    /// Dismiss `txid` as the wallet's own decision.
    pub(crate) fn insert(&mut self, txid: Txid) {
        self.own.insert(txid);
    }

    /// Hold `txid` back as a spend of the dismissed transactions among
    /// `parents`. Returns whether any of them is dismissed.
    pub(crate) fn hold_spend(
        &mut self,
        txid: Txid,
        parents: impl IntoIterator<Item = Txid>,
    ) -> bool {
        let parents: HashSet<Txid> = parents.into_iter().filter(|p| self.contains(p)).collect();
        if parents.is_empty() {
            return false;
        }
        if let Some(evicted) = self.spends.insert(txid) {
            self.forget_spend(&evicted);
        }
        let held =
            self.held_by.get(&txid).map_or(parents.len(), |held| held.union(&parents).count());
        if self.pinned.contains(&txid) || held > MAX_PARENTS {
            self.forget_spend(&txid);
            self.pinned.insert(txid);
            return true;
        }
        for parent in &parents {
            self.holds.entry(*parent).or_default().insert(txid);
        }
        self.held_by.entry(txid).or_default().extend(parents);
        true
    }

    /// Whether `txid` is dismissed, or held back as a spend of one.
    pub(crate) fn contains(&self, txid: &Txid) -> bool {
        self.own.contains(txid)
            || self.pinned.contains(txid)
            || self.held_by.get(txid).is_some_and(|parents| !parents.is_empty())
    }

    /// Whether a lock for `txid` should have it fetched again: it is
    /// dismissed, or a spend of a dismissed transaction, released or not.
    pub(crate) fn refetch_on_lock(&self, txid: &Txid) -> bool {
        self.own.contains(txid) || self.spends.contains(txid)
    }

    /// `txid` is locked or mined: it is not dead, and spends it held back are
    /// released, with spends they in turn held back. An explicit dismissal of
    /// one of those spends stays.
    pub(crate) fn settled(&mut self, txid: &Txid) {
        self.own.remove(txid);
        if self.spends.remove(txid) {
            self.forget_spend(txid);
        }
        self.release_spends_of(*txid);
    }

    fn release_spends_of(&mut self, parent: Txid) {
        let mut released = vec![parent];
        while let Some(parent) = released.pop() {
            for spend in self.holds.remove(&parent).unwrap_or_default() {
                let Some(parents) = self.held_by.get_mut(&spend) else {
                    continue;
                };
                parents.remove(&parent);
                if parents.is_empty() && !self.own.contains(&spend) && !self.pinned.contains(&spend)
                {
                    released.push(spend);
                }
            }
        }
    }

    fn forget_spend(&mut self, txid: &Txid) {
        self.pinned.remove(txid);
        for parent in self.held_by.remove(txid).unwrap_or_default() {
            if let Some(spends) = self.holds.get_mut(&parent) {
                spends.remove(txid);
                if spends.is_empty() {
                    self.holds.remove(&parent);
                }
            }
        }
    }
}

/// A set of at most `CAP` txids; the oldest goes first.
#[derive(Debug, Clone, Default)]
struct Fifo<const CAP: usize> {
    order: VecDeque<Txid>,
    set: HashSet<Txid>,
}

impl<const CAP: usize> Fifo<CAP> {
    /// Insert `txid`, returning the entry pushed out at capacity, if any.
    fn insert(&mut self, txid: Txid) -> Option<Txid> {
        if !self.set.insert(txid) {
            return None;
        }
        self.order.push_back(txid);
        if self.order.len() <= CAP {
            return None;
        }
        let oldest = self.order.pop_front()?;
        self.set.remove(&oldest);
        Some(oldest)
    }

    fn contains(&self, txid: &Txid) -> bool {
        self.set.contains(txid)
    }

    fn remove(&mut self, txid: &Txid) -> bool {
        let removed = self.set.remove(txid);
        if removed {
            self.order.retain(|t| t != txid);
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashcore::hashes::Hash;

    fn txid(n: u32) -> Txid {
        let mut bytes = [0u8; 32];
        bytes[..4].copy_from_slice(&n.to_le_bytes());
        Txid::from_byte_array(bytes)
    }

    #[test]
    fn the_oldest_dismissal_goes_first_at_capacity() {
        let mut dismissed = DismissedTxids::default();
        for n in 0..=MAX_DISMISSED as u32 {
            dismissed.insert(txid(n));
        }
        assert!(!dismissed.contains(&txid(0)));
        assert!(dismissed.contains(&txid(1)));
        assert!(dismissed.contains(&txid(MAX_DISMISSED as u32)));
        dismissed.settled(&txid(1));
        assert!(!dismissed.contains(&txid(1)));
        assert_eq!(dismissed.own.order.len(), MAX_DISMISSED - 1);
    }

    #[test]
    fn spends_do_not_push_out_the_wallets_own_dismissals() {
        let mut dismissed = DismissedTxids::default();
        let (own, parent) = (txid(u32::MAX), txid(u32::MAX - 1));
        dismissed.insert(own);
        dismissed.insert(parent);
        for n in 0..=2 * MAX_SPENDS as u32 {
            assert!(dismissed.hold_spend(txid(n), [parent]));
        }
        assert!(dismissed.contains(&own));
        assert!(dismissed.contains(&parent));
        assert!(!dismissed.contains(&txid(0)), "the oldest spend went");
        assert!(dismissed.contains(&txid(2 * MAX_SPENDS as u32)));
        assert_eq!(dismissed.spends.order.len(), MAX_SPENDS);
        assert_eq!(dismissed.held_by.len(), MAX_SPENDS);
        assert_eq!(dismissed.holds[&parent].len(), MAX_SPENDS);
    }

    #[test]
    fn settling_a_dismissed_transaction_releases_its_spends_but_not_an_abandon() {
        let mut dismissed = DismissedTxids::default();
        let (parent, other) = (txid(1), txid(2));
        let (child, abandoned_child, two_parents, grandchild) =
            (txid(3), txid(4), txid(5), txid(6));
        dismissed.insert(parent);
        dismissed.insert(other);
        assert!(!dismissed.hold_spend(txid(9), [txid(8)]), "it spends nothing dismissed");
        assert!(dismissed.hold_spend(child, [parent]));
        assert!(dismissed.hold_spend(abandoned_child, [parent]));
        dismissed.insert(abandoned_child);
        assert!(dismissed.hold_spend(two_parents, [parent, other]));
        assert!(dismissed.hold_spend(grandchild, [child]));

        dismissed.settled(&parent);
        assert!(!dismissed.contains(&parent));
        assert!(!dismissed.contains(&child));
        assert!(!dismissed.contains(&grandchild), "released with its parent");
        assert!(dismissed.contains(&abandoned_child), "abandoned in its own right");
        assert!(dismissed.contains(&two_parents), "it still spends a dismissed one");
        assert!(dismissed.refetch_on_lock(&child), "its lock still fetches it");
        assert!(!dismissed.refetch_on_lock(&parent));

        dismissed.settled(&child);
        assert!(!dismissed.refetch_on_lock(&child));
        assert!(!dismissed.held_by.contains_key(&child));
    }

    #[test]
    fn a_spend_of_many_dismissed_transactions_is_held_until_it_settles() {
        let mut dismissed = DismissedTxids::default();
        let parents: Vec<Txid> = (0..=MAX_PARENTS as u32).map(txid).collect();
        for parent in &parents {
            dismissed.insert(*parent);
        }
        let again = txid(u32::MAX - 1);
        for _ in 0..2 {
            assert!(dismissed.hold_spend(again, parents[..MAX_PARENTS].iter().copied()));
        }
        assert!(!dismissed.pinned.contains(&again), "parents counted once each");
        dismissed.settled(&again);
        let spend = txid(u32::MAX);
        assert!(dismissed.hold_spend(spend, parents.iter().copied()));
        assert!(dismissed.holds.is_empty(), "no parent index is kept for it");
        for parent in &parents {
            dismissed.settled(parent);
        }
        assert!(dismissed.contains(&spend));
        dismissed.settled(&spend);
        assert!(!dismissed.contains(&spend));
        assert!(dismissed.pinned.is_empty());
    }
}
