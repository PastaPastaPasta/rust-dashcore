use dash_types::Numeric as _;
use hashes::sha256d;

use crate::Transaction;
use crate::hash_types::{MerkleRootMasternodeList, MerkleRootQuorums};
use crate::sml::error::SmlError;
use crate::sml::masternode_list::MasternodeList;
use crate::transaction::special_transaction::coinbase::CoinbasePayload;

impl MasternodeList {
    /// The quorum root is in the coinbase payload from its version 2. An empty
    /// list hashes to the all-zero root, as in Dash Core. A payload of a
    /// version newer than this crate decodes is read as far as it knows the
    /// layout, see [`CoinbasePayload::from_coinbase_tx`]. A quorum list holding
    /// a commitment twice is refused, as Dash Core refuses a mutated root.
    pub fn verify_coinbase_roots(
        &self,
        coinbase_transaction: &Transaction,
    ) -> Result<(), SmlError> {
        let Some(coinbase_payload) = CoinbasePayload::from_coinbase_tx(coinbase_transaction) else {
            return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash));
        };

        let calculated = self
            .calculate_masternodes_merkle_root(self.known_height)
            .unwrap_or_else(MerkleRootMasternodeList::all_zeros);
        if calculated != coinbase_payload.merkle_root_masternode_list {
            return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash));
        }

        if coinbase_payload.version >= 2 {
            // A non-empty list without a root is a mutated tree: one holding a commitment twice.
            let calculated = match self.calculate_llmq_merkle_root() {
                Some(root) => root,
                None if self.hashes_for_quorum_merkle_root().is_empty() => {
                    MerkleRootQuorums::all_zeros()
                }
                None => return Err(SmlError::CoinbaseCommitmentMismatch(self.block_hash)),
            };
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

    use test_case::test_case;

    use super::*;
    use crate::consensus::{deserialize, serialize};
    use crate::hash_types::{MerkleRootMasternodeList, MerkleRootQuorums};
    use crate::network::message_sml::MnListDiff;
    use crate::sml::error::SmlError;
    use crate::test_utils::UNASSIGNED_SPECIAL_TX_TYPE;
    use crate::transaction::special_transaction::TransactionPayload;
    use crate::transaction::special_transaction::coinbase::CoinbasePayload;
    use crate::{BlockHash, Network, QuorumHash, Transaction};

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
        let (list, mut coinbase) = fixture_list(|_| {});
        let Some(TransactionPayload::CoinbasePayloadType(payload)) =
            &mut coinbase.special_transaction_payload
        else {
            panic!("coinbase payload");
        };
        tamper(payload);

        let result = list.verify_coinbase_roots(&coinbase);

        assert_eq!(result, error.map_or(Ok(()), |error| Err(error(list.block_hash))));
    }

    /// The list of the 2227096 capture, built after `edit`, and the capture's
    /// coinbase committing to its masternodes.
    fn fixture_list(edit: fn(&mut MnListDiff)) -> (MasternodeList, Transaction) {
        let mut diff: MnListDiff = deserialize(include_bytes!(
            "../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"
        ))
        .expect("expected to deserialize");
        edit(&mut diff);
        let mut coinbase = diff.coinbase_tx.clone();
        let list = MasternodeList::from_diff(diff, 2_227_096, Network::Mainnet).expect("builds");
        let Some(TransactionPayload::CoinbasePayloadType(payload)) =
            &mut coinbase.special_transaction_payload
        else {
            panic!("coinbase payload");
        };
        // The capture's masternodes do not hash to its coinbase's root, its quorums do.
        payload.merkle_root_masternode_list = list.masternode_merkle_root.expect("masternodes");
        (list, coinbase)
    }

    /// `coinbase` with its payload written as a version this crate does not
    /// decode, its type and bytes edited, and decoded back from the wire.
    fn newer_coinbase(
        coinbase: &Transaction,
        edit: impl FnOnce(&mut u16, &mut Vec<u8>),
    ) -> Transaction {
        let mut newer = coinbase.with_newer_coinbase_payload();
        let Some(TransactionPayload::RawPayloadType(tx_type, bytes)) =
            &mut newer.special_transaction_payload
        else {
            panic!("a raw payload");
        };
        edit(tx_type, bytes);
        let decoded: Transaction = deserialize(&serialize(&newer)).expect("decodes");
        assert_eq!(decoded, newer, "the decoder keeps the payload raw");
        decoded
    }

    /// A coinbase payload of a version this crate does not decode stays raw.
    /// Dash Core only appends fields in a new version, so the roots are read
    /// from the fields this crate knows, at bytes 6..38 and 38..70, and still
    /// have to match. A raw payload of a version this crate decodes, which
    /// Dash Core never produces, or of another transaction type commits to
    /// nothing.
    #[test_case(|_, _| {}, true; "of a newer version")]
    #[test_case(|_, bytes| bytes[6] ^= 1, false; "of a newer version off the masternode list root")]
    #[test_case(|_, bytes| bytes[38] ^= 1, false; "of a newer version off the quorum list root")]
    #[test_case(
        |_, bytes| bytes[..2].copy_from_slice(&4u16.to_le_bytes()),
        false;
        "of a known version with bytes left over"
    )]
    #[test_case(
        |tx_type, _| *tx_type = UNASSIGNED_SPECIAL_TX_TYPE,
        false;
        "of another transaction type"
    )]
    fn verify_coinbase_roots_of_a_raw_payload(edit: fn(&mut u16, &mut Vec<u8>), matches: bool) {
        let (list, coinbase) = fixture_list(|_| {});
        let coinbase = newer_coinbase(&coinbase, edit);

        let result = list.verify_coinbase_roots(&coinbase);

        let expected = if matches {
            Ok(())
        } else {
            Err(SmlError::CoinbaseCommitmentMismatch(list.block_hash))
        };
        assert_eq!(result, expected);
    }

    /// A newer coinbase payload cut short before its quorum list root commits
    /// to nothing, even for a list without quorums, whose root is all zeros.
    #[test]
    fn a_newer_coinbase_payload_cut_short_commits_to_nothing() {
        let (list, coinbase) = fixture_list(|diff| {
            diff.new_quorums.clear();
            diff.quorums_chainlock_signatures.clear();
        });
        assert!(list.quorums.is_empty());
        let coinbase = newer_coinbase(&coinbase, |_, bytes| bytes.truncate(38));
        assert_eq!(CoinbasePayload::from_coinbase_tx(&coinbase), None);

        let result = list.verify_coinbase_roots(&coinbase);

        assert_eq!(result, Err(SmlError::CoinbaseCommitmentMismatch(list.block_hash)));
    }

    /// Offset of the quorum type byte in a serialized commitment, after its
    /// `u16` version.
    const COMMITMENT_TYPE_OFFSET: usize = 2;

    /// A commitment of a type this crate does not know is hashed into the
    /// coinbase's quorum root as Dash Core serialized it, with its type byte,
    /// two new types at once included. The byte counts: the same commitment
    /// served under another unknown type is not the one committed to.
    #[test_case(201, true; "as committed")]
    #[test_case(202, false; "under another unknown type than committed")]
    fn verify_coinbase_roots_of_quorums_of_unknown_types(served: u8, matches: bool) {
        let mut diff: MnListDiff = deserialize(include_bytes!(
            "../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"
        ))
        .expect("expected to deserialize");
        assert_ne!(diff.new_quorums[0].quorum_hash, diff.new_quorums[1].quorum_hash);
        // Dash Core's bytes of the commitments, two of them of types 200 and
        // 201, which no type of this crate stands for.
        let mut wire: Vec<Vec<u8>> = diff.new_quorums.iter().map(serialize).collect();
        wire[0][COMMITMENT_TYPE_OFFSET] = 200;
        wire[1][COMMITMENT_TYPE_OFFSET] = 201;
        let mut core_hashes: Vec<sha256d::Hash> =
            wire.iter().map(|bytes| sha256d::Hash::hash(bytes)).collect();
        core_hashes.sort();
        let leaves = core_hashes.iter().map(|hash| hash.to_byte_array()).collect();
        let root = dash_evo::sml::merkle_root_quorums_from_hashes(leaves).expect("quorums");
        let committed = MerkleRootQuorums::from_byte_array(root.to_lendian());
        wire[1][COMMITMENT_TYPE_OFFSET] = served;
        diff.new_quorums = wire
            .iter()
            .map(|bytes| deserialize(bytes).expect("a commitment of any type decodes"))
            .collect();
        let mut coinbase = diff.coinbase_tx.clone();
        let list = MasternodeList::from_diff(diff, 2_227_096, Network::Mainnet).expect("builds");
        let Some(TransactionPayload::CoinbasePayloadType(payload)) =
            &mut coinbase.special_transaction_payload
        else {
            panic!("coinbase payload");
        };
        // The capture's masternodes do not hash to its coinbase's root.
        payload.merkle_root_masternode_list = list.masternode_merkle_root.expect("masternodes");
        payload.merkle_root_quorums = committed;

        let result = list.verify_coinbase_roots(&coinbase);

        let expected = if matches {
            Ok(())
        } else {
            Err(SmlError::CoinbaseCommitmentMismatch(list.block_hash))
        };
        assert_eq!(result, expected);
    }

    /// A list maps each commitment by the type and quorum hash it hashes, so
    /// one built from diffs never holds a commitment twice. One edited
    /// through its public maps or restored from damaged storage can. Doubling
    /// the last leaf of an odd level leaves the merkle root as it was, so Dash
    /// Core refuses such a root (`mutated-calc-cbtx-quorummerkleroot`), and so
    /// does this check.
    #[test]
    fn a_quorum_list_holding_a_commitment_twice_is_refused() {
        let diff: MnListDiff = deserialize(include_bytes!(
            "../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"
        ))
        .expect("expected to deserialize");
        let mut coinbase = diff.coinbase_tx.clone();
        let mut list =
            MasternodeList::from_diff(diff, 2_227_096, Network::Mainnet).expect("builds");
        let quorums = Arc::make_mut(&mut list.quorums);
        if quorums.values().map(|quorums| quorums.len()).sum::<usize>() % 2 == 0 {
            quorums.values_mut().next().expect("quorums").pop_first();
        }
        let last = Arc::clone(
            quorums
                .values()
                .flat_map(|quorums| quorums.values())
                .max_by_key(|quorum| quorum.entry_hash)
                .expect("quorums"),
        );
        let committed = list.calculate_llmq_merkle_root().expect("quorums");
        let Some(TransactionPayload::CoinbasePayloadType(payload)) =
            &mut coinbase.special_transaction_payload
        else {
            panic!("coinbase payload");
        };
        payload.merkle_root_masternode_list = list.masternode_merkle_root.expect("masternodes");
        payload.merkle_root_quorums = committed;
        assert_eq!(list.verify_coinbase_roots(&coinbase), Ok(()));

        Arc::make_mut(&mut list.quorums)
            .get_mut(&last.quorum_entry.llmq_type)
            .expect("its type")
            .insert(QuorumHash::from_byte_array([0xEE; 32]), last);
        assert_eq!(list.calculate_llmq_merkle_root(), None, "a mutated tree has no root");

        let result = list.verify_coinbase_roots(&coinbase);

        assert_eq!(result, Err(SmlError::CoinbaseCommitmentMismatch(list.block_hash)));
    }
}
