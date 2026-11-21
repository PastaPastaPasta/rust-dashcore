use std::collections::{HashMap, HashSet, VecDeque};

use dashcore::{OutPoint, Transaction, Txid};

/// The most dismissed transactions kept; the oldest goes first.
const MAX_DISMISSED: usize = 4_096;

/// The most transactions kept dismissed for what they spend; the oldest goes first.
const MAX_INHERITED: usize = 4_096;

/// The most outpoints spent by locked transactions kept; the oldest goes first.
const MAX_LOCKED_SPENDS: usize = 16_384;

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
    order: VecDeque<Txid>,
    parents: HashMap<Txid, Vec<Txid>>,
    spends: HashMap<Txid, Vec<Txid>>,
}

impl InheritedDismissals {
    /// Dismiss `txid` for spending the dismissed `parents` (none for a spend
    /// of what a locked transaction spends).
    pub(crate) fn insert(&mut self, txid: Txid, parents: Vec<Txid>) {
        if self.parents.contains_key(&txid) {
            return;
        }
        for parent in &parents {
            self.spends.entry(*parent).or_default().push(txid);
        }
        self.parents.insert(txid, parents);
        self.order.push_back(txid);
        if self.order.len() > MAX_INHERITED {
            if let Some(oldest) = self.order.pop_front() {
                self.forget(&oldest);
            }
        }
    }

    pub(crate) fn contains(&self, txid: &Txid) -> bool {
        self.parents.contains_key(txid)
    }

    pub(crate) fn remove(&mut self, txid: &Txid) {
        if self.forget(txid) {
            self.order.retain(|t| t != txid);
        }
    }

    fn forget(&mut self, txid: &Txid) -> bool {
        let Some(parents) = self.parents.remove(txid) else {
            return false;
        };
        for parent in parents {
            if let Some(spends) = self.spends.get_mut(&parent) {
                spends.retain(|t| t != txid);
                if spends.is_empty() {
                    self.spends.remove(&parent);
                }
            }
        }
        true
    }

    /// `txid` was mined: what is dismissed only for spending it, or for
    /// spending one of those in turn, is not any more.
    pub(crate) fn release_spends_of(&mut self, txid: &Txid) {
        let mut released = vec![*txid];
        while let Some(parent) = released.pop() {
            for spend in self.spends.remove(&parent).unwrap_or_default() {
                let Some(parents) = self.parents.get_mut(&spend) else {
                    continue;
                };
                parents.retain(|p| *p != parent);
                if parents.is_empty() {
                    self.remove(&spend);
                    released.push(spend);
                }
            }
        }
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
        assert_eq!(inherited.order, VecDeque::from([txid(5)]));
        assert!(inherited.spends.is_empty());

        for n in 10..=10 + MAX_INHERITED as u32 {
            inherited.insert(txid(n), vec![txid(1)]);
        }
        assert!(!inherited.contains(&txid(5)), "the oldest goes first");
        assert_eq!(inherited.spends[&txid(1)].len(), MAX_INHERITED);
    }
}
