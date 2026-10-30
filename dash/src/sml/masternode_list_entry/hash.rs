use dash_evo::sml::SmlEntry;
use dash_evo::types::{ProTxHash as EvoProTxHash, RawMnType};
use dash_primitives::{
    BlockHash, NetInfo, NetInfoV1, NetInfoV2, PlatformNodeId as EvoPlatformNodeId,
};
use dash_types::Numeric as _;
use dash_types::codec::BaseCodec as _;
use hashes::{Hash, sha256d};

use crate::consensus::Encodable;
use crate::sml::masternode_list_entry::{
    EntryMasternodeType, MasternodeListEntry, MasternodeNetInfo, canonical_legacy_service,
};

impl MasternodeListEntry {
    /// Core's `CSimplifiedMNListEntry::CalcHash`: the hash of the entry as Core serializes the one
    /// it holds, so over the canonical service address.
    ///
    /// Computed by `dash_evo::sml::SmlEntry::hash` (SB7), on the entry converted to it.
    pub fn calculate_entry_hash(&self) -> sha256d::Hash {
        sha256d::Hash::from_byte_array(self.to_sml_entry().hash().to_lendian())
    }

    /// The entry as dash-evo's [`SmlEntry`]: the network info as Core holds it, through its
    /// canonical bytes (an address Core does not keep is the empty one).
    pub fn to_sml_entry(&self) -> SmlEntry {
        let net_info = match &self.service_address {
            MasternodeNetInfo::Legacy(addr) => {
                let mut service = [0u8; 18];
                addr.consensus_encode(&mut &mut service[..]).expect("18 bytes");
                let service = canonical_legacy_service(service);
                NetInfo::Legacy(
                    NetInfoV1::decode(&mut &service[..]).expect("an 18-byte service decodes"),
                )
            }
            MasternodeNetInfo::Extended(info) => {
                let mut bytes = Vec::new();
                info.canonical().consensus_encode_ext(&mut bytes).expect("in memory");
                // Both read Core's `ExtNetInfo` format; what dashcore writes, base-sdk reads.
                NetInfo::Extended(
                    NetInfoV2::decode(&mut &bytes[..])
                        .expect("canonical extended net info decodes"),
                )
            }
        };
        let (mn_type, platform_http_port, platform_node_id) = match &self.mn_type {
            EntryMasternodeType::Regular => (RawMnType::REGULAR, 0, [0; 20]),
            EntryMasternodeType::HighPerformance {
                platform_http_port,
                platform_node_id,
            } => (RawMnType::EVO, *platform_http_port, platform_node_id.to_bytes()),
        };
        SmlEntry {
            version: self.version,
            pro_reg_tx_hash: EvoProTxHash::from(self.pro_reg_tx_hash.to_byte_array()),
            confirmed_hash: BlockHash::from_lendian(
                self.confirmed_hash.map_or([0; 32], |hash| hash.to_byte_array()),
            ),
            net_info,
            operator_key: *self.operator_public_key.as_bytes(),
            voting_key_id: self.key_id_voting.to_byte_array(),
            is_valid: self.is_valid,
            mn_type,
            platform_http_port,
            platform_node_id: EvoPlatformNodeId::from_lendian(platform_node_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use dash_core_vectors::golden::mnlistdiff_ground_truth;
    use hashes::{Hash, sha256};

    use crate::consensus::deserialize;
    use crate::network::message_sml::MnListDiff;
    use crate::sml::masternode_list_entry::EntryMasternodeType;

    // Ground-truth entry hashes computed by Dash Core's `CSimplifiedMNListEntry::CalcHash`
    // (`CHashWriter(SER_GETHASH, ...)`) for every entry of this fixture. `SER_GETHASH` omits the
    // `SER_NETWORK`-gated leading `version`, so the pre-image is the wire body without that field;
    // hashing the full wire (with `version`) yields different values and fails here.
    //
    // The fixture is the payload of a real Core `mnlistdiff` reply, and the expected values come
    // from Core's own deserializer and `CalcHash` run on those exact bytes
    // (`test-vectors/golden/PROVENANCE.md`), not from this crate. The fixture used to be a
    // re-encoding by Rust code whose IPv4 services were written in the IPv4-compatible form, which
    // is not what Core hashes; the constants this test used to hold were hashes of that encoding.
    #[test]
    fn entry_hash_matches_core_calc_hash() {
        let bytes: &[u8] =
            include_bytes!("../../../tests/data/test_DML_diffs/mn_list_diff_0_2227096.bin");
        let golden = mnlistdiff_ground_truth()
            .into_iter()
            .find(|g| g.name == "mainnet-0-2227096")
            .expect("ground truth for the fixture");
        assert_eq!(
            sha256::Hash::hash(bytes).to_string(),
            golden.wire_sha256,
            "the fixture is not the capture Core's ground truth was computed from"
        );

        let diff: MnListDiff = deserialize(bytes).expect("expected to deserialize");
        assert_eq!(diff.new_masternodes.len(), golden.diff.entries.len());

        let (mut legacy, mut evo) = (0, 0);
        for (entry, core) in diff.new_masternodes.iter().zip(&golden.diff.entries) {
            assert_eq!(entry.pro_reg_tx_hash.to_byte_array(), core.pro_reg_tx_hash);
            assert_eq!(
                entry.calculate_entry_hash().to_byte_array(),
                core.entry_hash,
                "entry hash for {} must match Dash Core's CalcHash",
                entry.pro_reg_tx_hash
            );
            match (entry.version, &entry.mn_type) {
                (1, EntryMasternodeType::Regular) => legacy += 1,
                (
                    _,
                    EntryMasternodeType::HighPerformance {
                        ..
                    },
                ) => evo += 1,
                _ => {}
            }
        }
        // Both the legacy (version 1) and the Evo (`nType`/platform fields) paths are covered.
        assert!(legacy > 0 && evo > 0, "fixture covers legacy={legacy} evo={evo}");
    }
}
