//! The node's simplified masternode list (EV4-serve; EV addendum §1.5): each masternode's
//! [`SmlEntry`] (Core's `CDeterministicMN::to_sml_entry`), the list's projection, and
//! `merkleRootMNList` kept incrementally across blocks ([`SmlRootCache`]) next to the full
//! reference ([`merkle_root_mn_list_full`]).
//!
//! The entry, its hash and the merkle tree are SB7's (`dash_evo::sml`); this module only maps the
//! list onto them. Decisions: `docs/node-program/designs/EV4-serve-notes.md` D1–D4.

use dash_consensus::validation::BlockValidationError;
use dash_evo::sml::{SmlEntry, SmlList, merkle_root_mn_list};
use dash_primitives::{BlockHash, MerkleRoot};
use imbl::ordmap::DiffItem;

use crate::{Dmn, MnList};

impl Dmn {
    /// The masternode's simplified-list entry (Core `CDeterministicMN::to_sml_entry`), with the
    /// operator key in its stored encoding, which the state's version selects (EV2a D17), and
    /// `isValid` = not PoSe-banned. Core narrows the state's `int` version to the entry's
    /// `uint16_t`, as `as u16` does.
    // Core: evo/deterministicmns.cpp:43 CDeterministicMN::to_sml_entry
    pub fn to_sml_entry(&self) -> SmlEntry {
        let cold = &*self.cold;
        SmlEntry {
            version: cold.version as u16,
            pro_reg_tx_hash: self.pro_tx_hash,
            confirmed_hash: BlockHash::from(*cold.confirmed_hash.as_bytes()),
            net_info: (*cold.net_info).clone(),
            operator_key: cold.operator_key_bytes(),
            voting_key_id: *cold.key_id_voting.as_bytes(),
            is_valid: self.is_valid(),
            mn_type: self.mn_type,
            platform_http_port: cold.platform_http_port,
            platform_node_id: cold.platform_node_id,
        }
    }
}

impl MnList {
    /// The list's simplified masternode list (Core `CDeterministicMNList::to_sml`): every
    /// masternode, banned ones included, in proTxHash memcmp order, which is the order
    /// `CSimplifiedMNList`'s constructor sorts into.
    // Core: evo/deterministicmns.cpp:251 CDeterministicMNList::to_sml; evo/simplifiedmns.cpp:73
    pub fn project_sml(&self) -> SmlList {
        SmlList::from_entries(self.iter().map(|dmn| dmn.to_sml_entry()))
    }
}

/// `merkleRootMNList` of `list` computed in full (`CalcCbTxMerkleRootMNList(list.to_sml())`
/// without Core's cache): every entry projected and hashed, then the tree. The reference for
/// [`SmlRootCache`].
// Core: evo/simplifiedmns.cpp:102 CalcCbTxMerkleRootMNList, :81 CalcMerkleRoot
pub fn merkle_root_mn_list_full(list: &MnList) -> Result<MerkleRoot, BlockValidationError> {
    let entries: Vec<SmlEntry> = list.iter().map(|dmn| dmn.to_sml_entry()).collect();
    merkle_root_mn_list(&entries)
}

/// `merkleRootMNList` kept between lists (EV4-serve D3).
///
/// The cache holds the last list it was given and that list's [`SmlList`]. For the next list it
/// walks `imbl`'s diff of the two lists, which skips the subtrees they share, re-projects each
/// added or replaced masternode, and updates the [`SmlList`] only where the entry changed; SB7's
/// tree then rehashes only those leaves' paths. The result never depends on which list came
/// before: any list (a block's child, a reorg target, a decoded list) gives the root
/// [`merkle_root_mn_list_full`] gives; only the cost grows with the difference.
#[derive(Clone)]
pub struct SmlRootCache {
    list: MnList,
    sml: SmlList,
}

impl Default for SmlRootCache {
    fn default() -> Self {
        SmlRootCache::new()
    }
}

impl SmlRootCache {
    /// A cache holding the empty pre-DIP3 list.
    pub fn new() -> Self {
        SmlRootCache {
            list: MnList::default_pre_dip3(),
            sml: SmlList::new(),
        }
    }

    /// `merkleRootMNList` of `list`; [`merkle_root_mn_list_full`]'s value.
    // Core: evo/simplifiedmns.cpp:102 CalcCbTxMerkleRootMNList
    pub fn merkle_root(&mut self, list: &MnList) -> Result<MerkleRoot, BlockValidationError> {
        self.advance(list);
        self.sml.merkle_root()
    }

    /// The simplified list of the last list given to [`SmlRootCache::merkle_root`] (or
    /// [`SmlRootCache::advance`]).
    pub fn sml(&self) -> &SmlList {
        &self.sml
    }

    /// Makes `list` the cached list: its projection replaces the last one's, entry by entry.
    pub fn advance(&mut self, list: &MnList) {
        for item in self.list.mns.diff(&list.mns) {
            match item {
                DiffItem::Add(_, new)
                | DiffItem::Update {
                    new: (_, new),
                    ..
                } => {
                    let entry = new.0.to_sml_entry();
                    if self.sml.get(&entry.pro_reg_tx_hash) != Some(&entry) {
                        self.sml.upsert(entry);
                    }
                }
                DiffItem::Remove(pro_tx, _) => {
                    self.sml.remove(pro_tx);
                }
            }
        }
        self.list = list.clone();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dash_num::Hash256;
    use dash_primitives::BlockHash;

    use super::*;
    use crate::testkit::{fresh, numbered};

    fn xorshift(x: &mut u64) -> u64 {
        *x ^= *x << 13;
        *x ^= *x >> 7;
        *x ^= *x << 17;
        *x
    }

    /// The cache against the full reference over random block-to-block changes (EV4-serve D4):
    /// additions, removals, hot updates that leave the entry alone, bans and revivals, cold
    /// updates inside and outside the entry, a new block with no change, and jumps to an earlier
    /// list and back.
    #[test]
    fn the_cache_gives_the_full_root_after_any_change() {
        let mut list = MnList::new(BlockHash::from([1; 32]), 1_000, 0);
        for i in 0..48 {
            list.try_add(numbered(i), true).unwrap();
        }
        let mut cache = SmlRootCache::new();
        let mut history = vec![list.clone()];
        let (mut x, mut next) = (0x2545_f491_4f6c_dd1d_u64, 48);
        let mut seen = [0usize; 8];
        for step in 0..600 {
            let keys: Vec<_> = list.iter().map(|dmn| dmn.pro_tx_hash).collect();
            let key = keys[(xorshift(&mut x) % keys.len() as u64) as usize];
            let dmn = Arc::clone(list.get(&key).unwrap());
            let op = (xorshift(&mut x) % 8) as usize;
            seen[op] += 1;
            match op {
                0 => {
                    list.try_add(numbered(next), true).unwrap();
                    next += 1;
                }
                1 if keys.len() > 1 => list.remove(&key).unwrap(),
                2 => {
                    let mut hot = dmn.hot;
                    hot.last_paid_height = 1_000 + step;
                    list.update_hot(&key, hot).unwrap();
                }
                3 => {
                    let mut hot = dmn.hot;
                    hot.ban_if_not_banned(1_000 + step);
                    list.update_hot(&key, hot).unwrap();
                }
                4 => {
                    let mut hot = dmn.hot;
                    hot.revive(1_000 + step);
                    list.update_hot(&key, hot).unwrap();
                }
                5 => {
                    let mut cold = (*dmn.cold).clone();
                    cold.confirmed_hash = Hash256::from([step as u8; 32]);
                    list.update(&key, dmn.hot, Arc::new(fresh(cold))).unwrap();
                }
                6 => {
                    let mut cold = (*dmn.cold).clone();
                    cold.revocation_reason = cold.revocation_reason.wrapping_add(1);
                    list.update(&key, dmn.hot, Arc::new(fresh(cold))).unwrap();
                }
                _ => {
                    let back = &history[(xorshift(&mut x) % history.len() as u64) as usize];
                    assert_eq!(
                        cache.merkle_root(back).unwrap(),
                        merkle_root_mn_list_full(back).unwrap()
                    );
                    assert_eq!(cache.sml(), &back.project_sml());
                }
            }
            assert_eq!(
                cache.merkle_root(&list).unwrap(),
                merkle_root_mn_list_full(&list).unwrap(),
                "step {step}, op {op}"
            );
            assert_eq!(cache.sml(), &list.project_sml(), "step {step}, op {op}");
            history.push(list.clone());
        }
        assert!(seen.iter().all(|&n| n > 40), "{seen:?}");
    }

    /// A banned masternode stays in the projection, with `isValid` false.
    // Core: evo/deterministicmns.cpp:45, :255-259
    #[test]
    fn the_projection_keeps_banned_masternodes() {
        let mut list = MnList::new(BlockHash::from([1; 32]), 1_000, 0);
        let (a, b) = (numbered(1), numbered(2));
        list.try_add(Arc::clone(&a), true).unwrap();
        list.try_add(Arc::clone(&b), true).unwrap();
        let mut hot = b.hot;
        hot.ban_if_not_banned(1_001);
        list.update_hot(&b.pro_tx_hash, hot).unwrap();

        let sml = list.project_sml();
        assert_eq!(sml.len(), 2);
        assert!(sml.get(&a.pro_tx_hash).unwrap().is_valid);
        assert!(!sml.get(&b.pro_tx_hash).unwrap().is_valid);
        let entry = list.get(&b.pro_tx_hash).unwrap().to_sml_entry();
        assert_eq!(entry.version, b.cold.version as u16);
        assert_eq!(entry.platform_http_port, b.cold.platform_http_port);
    }
}
