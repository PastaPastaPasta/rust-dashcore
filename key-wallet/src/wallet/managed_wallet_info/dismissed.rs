use std::collections::{HashSet, VecDeque};

use dashcore::Txid;

/// The most dismissed transactions kept; the oldest goes first.
const MAX_DISMISSED: usize = 4_096;

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
}
