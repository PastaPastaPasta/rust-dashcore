//! The dashcore shapes Platform's queries return, filled from base-sdk's values (SB0 §5.5).
//!
//! `DashSpvClient::latest_masternode_list` returns a dashcore [`MasternodeList`] and
//! `get_quorum_at_height` a [`QualifiedQuorumEntry`] (Platform's `spv/runtime.rs`). Their values
//! come from base-sdk's list entries ([`SimplifiedMnListEntry`]) and commitments
//! ([`Commitment`]), through Core's serialization of each, plus what the wire does not carry:
//! the list's height and roots, and each quorum's verification status and ChainLock signature.
//! Entry and commitment hashes are computed as dashcore computes them, so a filled shape equals
//! the one dashcore builds from the same message.

use std::collections::BTreeMap;
use std::sync::Arc;

use dash_num::Hash256;
use dash_p2p_core::SimplifiedMnListEntry;
use dash_primitives::{BlockHash, Commitment};
use dash_types::Numeric as _;
use dashcore::consensus::serialize;
use dashcore::hash_types::{MerkleRootMasternodeList, MerkleRootQuorums};
use dashcore::hashes::Hash;
use dashcore::sml::llmq_entry_verification::LLMQEntryVerificationStatus;
use dashcore::sml::masternode_list::MasternodeList;
use dashcore::sml::masternode_list_entry::qualified_masternode_list_entry::QualifiedMasternodeListEntry;
use dashcore::sml::masternode_list_entry::MasternodeListEntry;
use dashcore::sml::quorum_entry::qualified_quorum_entry::{
    QualifiedQuorumEntry, VerifyingChainLockSignaturesType,
};
use dashcore::transaction::special_transaction::quorum_commitment::QuorumEntry;

use super::headers::block_hash_from_base;
use super::{decode_all, decode_dashcore, from_base, to_base, BridgeError};

/// A list entry as dashcore's [`QualifiedMasternodeListEntry`], with its entry hash.
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses the entry base-sdk wrote.
pub fn masternode_list_entry(
    entry: &SimplifiedMnListEntry,
) -> Result<QualifiedMasternodeListEntry, BridgeError> {
    // Entries carry their version on the wire from protocol 70228, the layout dashcore reads.
    let mut bytes = Vec::new();
    entry.encode_as(&mut bytes, true);
    decode_dashcore::<MasternodeListEntry>("smlentry", &bytes).map(Into::into)
}

/// base-sdk's list entry for a dashcore one, with its version (the layout from protocol 70228).
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses the entry dashcore wrote.
pub fn masternode_list_entry_to_base(
    entry: &MasternodeListEntry,
) -> Result<SimplifiedMnListEntry, BridgeError> {
    decode_all("smlentry", &serialize(entry), |cursor| {
        SimplifiedMnListEntry::decode_as(cursor, true)
    })
}

/// base-sdk's commitment for a dashcore quorum entry.
///
/// # Errors
///
/// [`BridgeError::ToBase`] if base-sdk refuses the commitment dashcore wrote.
pub fn quorum_entry_to_base(entry: &QuorumEntry) -> Result<Commitment, BridgeError> {
    to_base("qfcommit", entry)
}

/// A commitment as dashcore's [`QualifiedQuorumEntry`], the shape `get_quorum_at_height`
/// returns, with its commitment and entry hashes and the status and ChainLock signature its
/// caller verified.
///
/// # Errors
///
/// [`BridgeError::FromBase`] if dashcore refuses the commitment base-sdk wrote.
pub fn quorum_entry(
    commitment: &Commitment,
    verified: LLMQEntryVerificationStatus,
    verifying_chain_lock_signature: Option<VerifyingChainLockSignaturesType>,
) -> Result<QualifiedQuorumEntry, BridgeError> {
    let entry: QuorumEntry = from_base("qfcommit", commitment)?;
    Ok(QualifiedQuorumEntry {
        verified,
        verifying_chain_lock_signature,
        ..entry.into()
    })
}

/// The list at `block_hash` as dashcore's [`MasternodeList`], the shape
/// `latest_masternode_list` returns, from its entries and its quorums ([`quorum_entry`]).
///
/// # Errors
///
/// The first entry's error from [`masternode_list_entry`].
pub fn masternode_list<'a>(
    block_hash: &BlockHash,
    known_height: u32,
    masternode_merkle_root: Option<Hash256>,
    llmq_merkle_root: Option<Hash256>,
    entries: impl IntoIterator<Item = &'a SimplifiedMnListEntry>,
    quorums: impl IntoIterator<Item = QualifiedQuorumEntry>,
) -> Result<MasternodeList, BridgeError> {
    let mut masternodes = BTreeMap::new();
    for entry in entries {
        let entry = masternode_list_entry(entry)?;
        // dashcore keys its map by the reversed proRegTxHash.
        masternodes.insert(entry.masternode_list_entry.pro_reg_tx_hash.reverse(), Arc::new(entry));
    }
    let mut quorum_map: BTreeMap<_, BTreeMap<_, _>> = BTreeMap::new();
    for quorum in quorums {
        quorum_map
            .entry(quorum.quorum_entry.llmq_type)
            .or_default()
            .insert(quorum.quorum_entry.quorum_hash, Arc::new(quorum));
    }
    Ok(MasternodeList {
        block_hash: block_hash_from_base(block_hash),
        known_height,
        masternode_merkle_root: masternode_merkle_root
            .map(|root| MerkleRootMasternodeList::from_byte_array(root.to_lendian())),
        llmq_merkle_root: llmq_merkle_root
            .map(|root| MerkleRootQuorums::from_byte_array(root.to_lendian())),
        masternodes: masternodes.into(),
        quorums: quorum_map.into(),
    })
}

#[cfg(test)]
mod tests {
    use dash_types::codec::BaseCodec as _;
    use dashcore::bls_sig_utils::BLSSignature;
    use dashcore::consensus::deserialize;
    use dashcore::network::message_qrinfo::QRInfo;
    use dashcore::network::message_sml::MnListDiff as CoreMnListDiff;
    use dashcore::sml::llmq_entry_verification::LLMQEntryVerificationSkipStatus;
    use dashcore::Network;

    use super::*;
    use crate::bridge::headers::block_hash_to_base;
    use crate::sml_engine::test_support::{fixture_heights, TestEngine};

    /// A whole-list diff decoded by base-sdk fills the list dashcore builds from the same bytes,
    /// the engine's first list and the one `latest_masternode_list` returns until a newer one.
    fn whole_list_matches(bytes: &[u8], height: u32, network: Network) {
        let core: CoreMnListDiff = deserialize(bytes).expect("dashcore decodes the capture");
        let expected = MasternodeList::from_diff(core, height, network).expect("dashcore's list");

        let mut cursor = bytes;
        let diff = dash_p2p_core::MnListDiff::decode(&mut cursor).expect("base-sdk decodes it");
        assert!(cursor.is_empty());
        // Each commitment takes the ChainLock signature whose index set names it.
        let mut signatures = vec![None; diff.new_quorums.len()];
        for cl_sig in &diff.quorum_cl_sigs {
            let mut bytes = Vec::new();
            cl_sig.sig.encode(&mut bytes);
            let signature = BLSSignature::from(<[u8; 96]>::try_from(bytes).expect("96 bytes"));
            for &index in &cl_sig.index_set {
                signatures[usize::from(index)] =
                    Some(VerifyingChainLockSignaturesType::NonRotating(signature));
            }
        }
        let quorums = diff.new_quorums.iter().zip(signatures).map(|(commitment, signature)| {
            quorum_entry(
                commitment,
                LLMQEntryVerificationStatus::Skipped(
                    LLMQEntryVerificationSkipStatus::NotMarkedForVerification,
                ),
                signature,
            )
            .expect("dashcore reads base-sdk's commitment")
        });
        let filled = masternode_list(
            &diff.block_hash,
            height,
            diff.cb_tx_merkle_tree.hashes.first().copied(),
            None,
            &diff.mn_list,
            quorums,
        )
        .expect("dashcore reads base-sdk's entries");
        assert_eq!(filled.masternodes.len(), expected.masternodes.len());
        assert!(filled == expected, "the filled list differs from dashcore's");
    }

    #[test]
    fn whole_list_diffs_fill_dashcores_list_mainnet() {
        whole_list_matches(
            include_bytes!("../../../dash/tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin"),
            2_227_096,
            Network::Mainnet,
        );
    }

    #[test]
    fn whole_list_diffs_fill_dashcores_list_testnet() {
        whole_list_matches(
            include_bytes!("../../../dash/artifacts/mn_list_diff_testnet_0_1296600.bin"),
            1_296_600,
            Network::Testnet,
        );
    }

    /// After the engine reads Core's `qrinfo` capture (the code behind Platform's two queries),
    /// every list it holds and every quorum a lookup returns comes back equal when filled
    /// through the bridge from base-sdk's values.
    #[tokio::test]
    async fn platform_queries_return_the_same_values_through_the_bridge() {
        let mut engine = TestEngine::knowing(
            Network::Mainnet,
            fixture_heights(include_bytes!(
                "../../../dash/tests/data/test_DML_diffs/block_container_2518986.dat"
            )),
        );
        // The capture the engine's own tests feed (`sml_engine`, `load_qrinfo_2518986_fixture`).
        let (qr_info, _): (QRInfo, _) = bincode::decode_from_slice(
            include_bytes!("../../../dash/tests/data/test_DML_diffs/qrinfo_2518986.dat"),
            bincode::config::standard(),
        )
        .expect("the fixture decodes");
        engine.feed_qr_info(qr_info).await.expect("the engine reads the capture");
        assert!(!engine.masternode_lists.is_empty());

        let (mut lists, mut quorums) = (0, 0);
        for (&height, list) in &engine.masternode_lists {
            let entries: Vec<_> = list
                .masternodes
                .values()
                .map(|entry| {
                    masternode_list_entry_to_base(&entry.masternode_list_entry)
                        .expect("base-sdk reads dashcore's entry")
                })
                .collect();
            let mut filled_quorums = Vec::new();
            for quorum in list.quorums.values().flat_map(BTreeMap::values) {
                let commitment = quorum_entry_to_base(&quorum.quorum_entry)
                    .expect("base-sdk reads dashcore's commitment");
                let filled = quorum_entry(
                    &commitment,
                    quorum.verified.clone(),
                    quorum.verifying_chain_lock_signature,
                )
                .expect("dashcore reads base-sdk's commitment");
                // `get_quorum_at_height`'s lookup at this list's height.
                let (_, looked_up) = engine
                    .quorum_entry_for_hash_at_or_before_height(
                        quorum.quorum_entry.llmq_type,
                        quorum.quorum_entry.quorum_hash,
                        height,
                    )
                    .expect("the engine finds a quorum its list holds");
                assert_eq!(&filled, looked_up);
                filled_quorums.push(filled);
                quorums += 1;
            }
            let filled = masternode_list(
                &block_hash_to_base(&list.block_hash),
                list.known_height,
                list.masternode_merkle_root.map(|root| Hash256::from_lendian(root.to_byte_array())),
                list.llmq_merkle_root.map(|root| Hash256::from_lendian(root.to_byte_array())),
                &entries,
                filled_quorums,
            )
            .expect("dashcore reads base-sdk's entries");
            assert!(&filled == list, "the list at {height} differs through the bridge");
            lists += 1;
        }
        let latest = engine.latest_masternode_list().expect("a latest list");
        assert!(engine.masternode_lists.values().any(|list| list == latest));
        assert!(lists > 0 && quorums > 0, "{lists} lists, {quorums} quorums");
    }
}
