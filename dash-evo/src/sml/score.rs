//! Masternode scores for a quorum over the simplified list, moved from dashcore's
//! `sml::masternode_list::scores_for_quorum` (SB0 §5.5).
//!
//! The node's member selection is LQ1's, over `MnListView`; these serve dashcore's SPV callers,
//! in Core's order.

use std::cmp::{Ordering, Reverse};

use dash_consensus::hash::sha256;
use dash_primitives::OutPoint;

use crate::types::{ProTxHash, RawMnType};

/// A masternode's score for a quorum: Core's `arith_uint256` of the single SHA-256, so ordered as
/// a number, little-endian bytes compared from the last.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct QuorumScore(pub [u8; 32]);

impl Ord for QuorumScore {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }
}

impl PartialOrd for QuorumScore {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// `confirmedHashWithProRegTxHash`: the single SHA-256 of proTxHash and confirmedHash.
// Core: evo/dmnstate.h:162 CDeterministicMNState::UpdateConfirmedHash
pub fn confirmed_hash_with_pro_reg_tx(
    pro_reg_tx_hash: &ProTxHash,
    confirmed_hash: &[u8; 32],
) -> [u8; 32] {
    sha256_pair(pro_reg_tx_hash.as_bytes(), confirmed_hash)
}

/// `calculateQuorumScore`: the single SHA-256 of `confirmedHashWithProRegTxHash` and the quorum
/// modifier.
// Core: llmq/utils.cpp:72 calculateQuorumScore
pub fn quorum_score(confirmed_with_pro_reg_tx: &[u8; 32], modifier: &[u8; 32]) -> QuorumScore {
    QuorumScore(sha256_pair(confirmed_with_pro_reg_tx, modifier))
}

fn sha256_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut both = [0; 64];
    both[..32].copy_from_slice(a);
    both[32..].copy_from_slice(b);
    sha256(&both)
}

/// What scoring reads from a list entry.
pub trait ScoredEntry {
    /// `isValid`: not PoSe-banned.
    fn is_valid(&self) -> bool;
    /// `nType`.
    fn mn_type(&self) -> RawMnType;
    /// `confirmedHashWithProRegTxHash`, or `None` while the entry is unconfirmed.
    fn confirmed_hash_with_pro_reg_tx(&self) -> Option<[u8; 32]>;
    /// `collateralOutpoint`, Core's tie-break between equal scores, or `None` where the list does
    /// not carry it: Core's `CSimplifiedMNListEntry` has no collateral. A list gives it for all its
    /// entries or for none.
    fn collateral_outpoint(&self) -> Option<OutPoint>;
}

/// Each valid, confirmed entry (Evo only when `evo_only`) with its score for `modifier`, in input
/// order: Core's `CalculateScoresForQuorum`.
// Core: llmq/utils.cpp:117,137 CalculateScoresForQuorum
pub fn scores_for_quorum<'a, E: ScoredEntry>(
    entries: impl IntoIterator<Item = &'a E>,
    modifier: &[u8; 32],
    evo_only: bool,
) -> Vec<(QuorumScore, &'a E)> {
    entries
        .into_iter()
        .filter(|entry| entry.is_valid() && (!evo_only || entry.mn_type().is_evo()))
        .filter_map(|entry| {
            let confirmed = entry.confirmed_hash_with_pro_reg_tx()?;
            Some((quorum_score(&confirmed, modifier), entry))
        })
        .collect()
}

/// The quorum for `modifier`: Core's `CalculateQuorum`, the scored entries highest score first,
/// the first `max_size` of them when given (Core's `maxSize` 0, no limit, is `None`).
///
/// Equal scores go higher collateral outpoint first, as Core's ascending comparator sorted through
/// reverse iterators puts them (`COutPoint`: the txid's bytes by memcmp, then the index). A tie
/// takes a SHA-256 collision: the scores hash `confirmedHashWithProRegTxHash`, itself a hash over
/// the proTxHash, which no two entries of a list share. Tied entries without a collateral (the
/// simplified list) keep their input order.
// Core: llmq/utils.cpp:160-186 CalculateQuorum
pub fn calculate_quorum<'a, E: ScoredEntry>(
    entries: impl IntoIterator<Item = &'a E>,
    modifier: &[u8; 32],
    evo_only: bool,
    max_size: Option<usize>,
) -> Vec<&'a E> {
    let mut scores = scores_for_quorum(entries, modifier, evo_only);
    // A stable sort: tied entries without a collateral keep their order.
    scores.sort_by_key(|(score, entry)| {
        Reverse((*score, entry.collateral_outpoint().map(|o| (*o.hash.as_bytes(), o.index))))
    });
    scores.into_iter().take(max_size.unwrap_or(usize::MAX)).map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use dash_primitives::TxHash;
    use dash_types::Numeric;

    use super::*;

    #[derive(Debug, PartialEq)]
    struct Mn {
        confirmed: Option<[u8; 32]>,
        valid: bool,
        evo: bool,
        collateral: Option<OutPoint>,
    }

    impl ScoredEntry for Mn {
        fn is_valid(&self) -> bool {
            self.valid
        }

        fn mn_type(&self) -> RawMnType {
            if self.evo {
                RawMnType::EVO
            } else {
                RawMnType::REGULAR
            }
        }

        fn confirmed_hash_with_pro_reg_tx(&self) -> Option<[u8; 32]> {
            self.confirmed
        }

        fn collateral_outpoint(&self) -> Option<OutPoint> {
            self.collateral
        }
    }

    fn mn(confirmed: u8, collateral: Option<OutPoint>) -> Mn {
        Mn {
            confirmed: Some([confirmed; 32]),
            valid: true,
            evo: false,
            collateral,
        }
    }

    /// A txid whose first and last bytes differ, so memcmp and numeric order disagree.
    fn outpoint(first: u8, last: u8, index: u32) -> OutPoint {
        let mut hash = [0x80; 32];
        hash[0] = first;
        hash[31] = last;
        OutPoint {
            hash: TxHash::from_lendian(hash),
            index,
        }
    }

    /// Core's `CalculateQuorum` order as its code states it: `std::sort(scores.rbegin(),
    /// scores.rend(), comp)` with `comp` ascending by score, then `COutPoint::operator<` (memcmp of
    /// the txid, then `n`), which reads back as the reverse of the ascending order. A C++ run of
    /// that sort on this test's entries gave the order the test expects.
    fn core_order<'a>(entries: &'a [Mn], modifier: &[u8; 32]) -> Vec<&'a Mn> {
        let mut scores = scores_for_quorum(entries, modifier, false);
        scores.sort_by_key(|(score, mn)| {
            let o = mn.collateral.unwrap();
            (*score, *o.hash.as_bytes(), o.index)
        });
        scores.into_iter().rev().map(|(_, entry)| entry).collect()
    }

    #[test]
    fn score_orders_as_arith_uint256() {
        let (mut high, mut low) = ([0; 32], [0; 32]);
        high[31] = 1;
        low[0] = 0xff;
        assert!(QuorumScore(high) > QuorumScore(low), "the last byte is the most significant");
    }

    // A tie needs one `confirmedHashWithProRegTxHash` on two entries, a SHA-256 collision on a
    // real list; the test sets it directly.
    #[test]
    fn ties_go_by_collateral_as_core_sorts_them() {
        let modifier = [7; 32];
        let entries = [
            mn(1, Some(outpoint(0x01, 0xff, 0))),
            mn(2, Some(outpoint(0x05, 0x00, 0))),
            mn(1, Some(outpoint(0x02, 0x00, 1))),
            mn(3, Some(outpoint(0x03, 0x00, 0))),
            mn(1, Some(outpoint(0x02, 0x00, 0))),
            mn(2, Some(outpoint(0x04, 0x00, 9))),
        ];
        let quorum = calculate_quorum(&entries, &modifier, false, None);
        assert_eq!(quorum.len(), entries.len(), "a tie drops no entry");
        let order = |list: &[&Mn]| list.iter().map(|mn| mn.collateral.unwrap()).collect::<Vec<_>>();
        assert_eq!(order(&quorum), order(&core_order(&entries, &modifier)));
        let tied: Vec<_> =
            quorum.iter().filter(|mn| mn.confirmed == Some([1; 32])).copied().collect();
        assert_eq!(
            order(&tied),
            [outpoint(0x02, 0x00, 1), outpoint(0x02, 0x00, 0), outpoint(0x01, 0xff, 0)],
            "higher collateral first, the txid by memcmp, then the index"
        );
        let scores: Vec<_> =
            quorum.iter().map(|mn| quorum_score(&mn.confirmed.unwrap(), &modifier)).collect();
        assert!(scores.is_sorted_by(|a, b| a >= b), "highest score first");
    }

    #[test]
    fn quorum_filters_and_cuts_like_core() {
        let modifier = [9; 32];
        let mut entries: Vec<Mn> = (0..8).map(|i| mn(i, Some(outpoint(i, 0, 0)))).collect();
        entries[1].valid = false;
        entries[2].confirmed = None;
        entries[3].evo = true;
        entries[4].evo = true;
        let all = calculate_quorum(&entries, &modifier, false, None);
        assert_eq!(all.len(), 6);
        let cut = calculate_quorum(&entries, &modifier, false, Some(4));
        assert_eq!(cut, all[..4]);
        let evo = calculate_quorum(&entries, &modifier, true, None);
        assert_eq!(evo.len(), 2);
        assert!(evo.iter().all(|mn| mn.evo));
    }

    #[test]
    fn ties_without_a_collateral_keep_input_order() {
        // Enough entries that an unstable sort would reorder them.
        let entries: Vec<Mn> = (0..64).map(|i| mn(i % 2, None)).collect();
        let quorum = calculate_quorum(&entries, &[3; 32], false, None);
        let position =
            |i: usize| quorum.iter().position(|mn| std::ptr::eq(*mn, &entries[i])).unwrap();
        for i in 2..entries.len() {
            assert!(position(i - 2) < position(i), "tied entries {} and {i} swapped", i - 2);
        }
    }
}
