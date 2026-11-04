use dash_types::Numeric as _;
use hashes::{Hash, sha256d};

use crate::Transaction;
use crate::hash_types::{MerkleRootMasternodeList, MerkleRootQuorums};
use crate::sml::error::SmlError;
use crate::sml::masternode_list::MasternodeList;
use crate::transaction::special_transaction::TransactionPayload;

impl MasternodeList {
    /// The quorum root is in the coinbase payload from its version 2. An empty
    /// list hashes to the all-zero root, as in Dash Core.
    pub fn verify_coinbase_roots(
        &self,
        coinbase_transaction: &Transaction,
    ) -> Result<(), SmlError> {
        let Some(TransactionPayload::CoinbasePayloadType(coinbase_payload)) =
            &coinbase_transaction.special_transaction_payload
        else {
            return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash));
        };

        let calculated = self
            .calculate_masternodes_merkle_root(self.known_height)
            .unwrap_or_else(MerkleRootMasternodeList::all_zeros);
        if calculated != coinbase_payload.merkle_root_masternode_list {
            return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash));
        }

        if coinbase_payload.version >= 2 {
            let calculated =
                self.calculate_llmq_merkle_root().unwrap_or_else(MerkleRootQuorums::all_zeros);
            if calculated != coinbase_payload.merkle_root_quorums {
                return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash));
            }
        }

        Ok(())
    }

    /// Computes the Merkle root for the masternode list at a given block height.
    ///
    /// This function generates a Merkle root for the masternode list based on the
    /// masternode entries at the specified block height.
    ///
    /// # Parameters
    ///
    /// - `block_height`: The block height at which to compute the Merkle root.
    ///
    /// # Returns
    ///
    /// - `Some(MerkleRootMasternodeList)`: The calculated Merkle root.
    /// - `None`: If no hashes are available for the given block height, or the tree is mutated.
    pub fn calculate_masternodes_merkle_root(
        &self,
        block_height: u32,
    ) -> Option<MerkleRootMasternodeList> {
        let hashes = self.hashes_for_merkle_root(block_height)?;
        if hashes.is_empty() {
            return None;
        }
        // dash-evo's `merkleRootMNList` over the cached entry hashes, in proTxHash order (SB7).
        let leaves = hashes.iter().map(|hash| hash.to_byte_array()).collect();
        let root = dash_evo::sml::merkle_root_mn_list_from_leaves(leaves).ok()?;
        Some(MerkleRootMasternodeList::from_byte_array(root.to_lendian()))
    }

    /// Computes the Merkle root for the LLMQ (Long-Living Masternode Quorum) list.
    ///
    /// This function constructs a Merkle tree using the commitment hashes of all known LLMQs
    /// and returns the root hash.
    ///
    /// # Returns
    ///
    /// - `Some(MerkleRootQuorums)`: The calculated Merkle root.
    /// - `None`: If no quorum commitment hashes are available.
    pub fn calculate_llmq_merkle_root(&self) -> Option<MerkleRootQuorums> {
        let hashes = self.hashes_for_quorum_merkle_root();
        if hashes.is_empty() {
            return None;
        }
        // dash-evo's `merkleRootQuorums` over the held set (SB7); a mutated tree has no root.
        let leaves = hashes.iter().map(|hash| hash.to_byte_array()).collect();
        let root = dash_evo::sml::merkle_root_quorums_from_hashes(leaves).ok()?;
        Some(MerkleRootQuorums::from_byte_array(root.to_lendian()))
    }

    /// Retrieves the list of hashes required to compute the masternode list Merkle root.
    ///
    /// This function sorts the masternode list by pro-reg transaction hash and extracts
    /// the entry hashes for the given block height.
    ///
    /// # Parameters
    ///
    /// - `block_height`: The block height for which to retrieve the hashes.
    ///
    /// # Returns
    ///
    /// - `Some(Vec<sha256d::Hash>)`: A sorted list of masternode entry hashes.
    /// - `None`: If the block height is invalid (`u32::MAX`).
    pub fn hashes_for_merkle_root(&self, block_height: u32) -> Option<Vec<sha256d::Hash>> {
        (block_height != u32::MAX).then_some({
            let mut pro_tx_hashes = self.reversed_pro_reg_tx_hashes();
            pro_tx_hashes.sort_by_key(|&s| s.reverse());
            pro_tx_hashes
                .into_iter()
                .map(|hash| self.masternodes[hash].entry_hash.to_raw_hash())
                .collect::<Vec<_>>()
        })
    }

    /// Retrieves the list of hashes required to compute the quorum Merkle root.
    ///
    /// This function collects and sorts the entry hashes of all known quorums
    /// to construct a Merkle tree.
    ///
    /// # Returns
    ///
    /// - `Vec<[u8; 32]>`: A sorted list of quorum commitment hashes.
    pub fn hashes_for_quorum_merkle_root(&self) -> Vec<sha256d::Hash> {
        let mut llmq_commitment_hashes = self
            .quorums
            .values()
            .flat_map(|q_map| q_map.values().map(|entry| entry.entry_hash.to_raw_hash()))
            .collect::<Vec<_>>();
        llmq_commitment_hashes.sort();
        llmq_commitment_hashes
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hashes::Hash;
    use test_case::test_case;

    use super::*;
    use crate::consensus::deserialize;
    use crate::hash_types::{MerkleRootMasternodeList, MerkleRootQuorums};
    use crate::network::message_sml::MnListDiff;
    use crate::sml::error::SmlError;
    use crate::transaction::special_transaction::TransactionPayload;
    use crate::transaction::special_transaction::coinbase::CoinbasePayload;
    use crate::{BlockHash, Network, QuorumHash};

    fn mainnet_list() -> MasternodeList {
        let bytes: &[u8] =
            include_bytes!("../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin");
        let diff: MnListDiff = deserialize(bytes).expect("expected to deserialize");
        MasternodeList::from_diff(diff, 2_227_096, Network::Mainnet).expect("expected a list")
    }

    /// Two equal sibling leaves make the tree mutated, a root Core rejects: the adapters give
    /// none (SB7).
    #[test]
    fn mutated_roots_are_none() {
        let mut list = mainnet_list();
        assert!(list.calculate_masternodes_merkle_root(2_227_096).is_some());
        assert!(list.calculate_llmq_merkle_root().is_some());

        // Two masternodes left, the second's cached entry hash set to the first's.
        let masternodes = Arc::make_mut(&mut list.masternodes);
        let keys: Vec<_> = masternodes.keys().take(2).copied().collect();
        masternodes.retain(|key, _| keys.contains(key));
        let first = masternodes[&keys[0]].entry_hash;
        Arc::make_mut(masternodes.get_mut(&keys[1]).unwrap()).entry_hash = first;
        assert_eq!(list.calculate_masternodes_merkle_root(2_227_096), None);

        // One quorum held under two quorum hashes.
        let quorums = Arc::make_mut(&mut list.quorums);
        let (llmq_type, quorum) = quorums
            .iter()
            .find_map(|(llmq_type, by_hash)| Some((*llmq_type, by_hash.values().next()?.clone())))
            .unwrap();
        quorums.clear();
        let by_hash = quorums.entry(llmq_type).or_default();
        by_hash.insert(QuorumHash::from_byte_array([1; 32]), quorum.clone());
        assert!(list.calculate_llmq_merkle_root().is_some());
        let quorums = Arc::make_mut(&mut list.quorums);
        quorums.get_mut(&llmq_type).unwrap().insert(QuorumHash::from_byte_array([2; 32]), quorum);
        assert_eq!(list.calculate_llmq_merkle_root(), None);
    }

    #[test_case(|_| {}, None; "matching")]
    #[test_case(
        |payload| payload.merkle_root_masternode_list = MerkleRootMasternodeList::all_zeros(),
        Some(SmlError::CoinbaseCommitmentMismatch);
        "off the masternode list root"
    )]
    #[test_case(
        |payload| payload.merkle_root_quorums = MerkleRootQuorums::all_zeros(),
        Some(SmlError::CoinbaseCommitmentMismatch);
        "off the quorum list root"
    )]
    #[test_case(
        |payload| {
            payload.version = 1;
            payload.merkle_root_quorums = MerkleRootQuorums::all_zeros();
        },
        None;
        "a version 1 coinbase committing to no quorums"
    )]
    fn verify_coinbase_roots(
        tamper: fn(&mut CoinbasePayload),
        error: Option<fn(BlockHash) -> SmlError>,
    ) {
        let diff: MnListDiff = deserialize(include_bytes!(
            "../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"
        ))
        .expect("expected to deserialize");
        let mut coinbase = diff.coinbase_tx.clone();
        let list = MasternodeList::from_diff(diff, 2_227_096, Network::Mainnet).expect("builds");
        let Some(TransactionPayload::CoinbasePayloadType(payload)) =
            &mut coinbase.special_transaction_payload
        else {
            panic!("coinbase payload");
        };
        // The capture's masternodes do not hash to its coinbase's root, its quorums do.
        payload.merkle_root_masternode_list = list.masternode_merkle_root.expect("masternodes");
        tamper(payload);

        let result = list.verify_coinbase_roots(&coinbase);

        assert_eq!(result, error.map_or(Ok(()), |error| Err(error(list.block_hash))));
    }
}
