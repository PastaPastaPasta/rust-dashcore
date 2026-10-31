//! `dashcore` against what Dash Core computes from Core-produced `mnlistdiff` captures.
//!
//! The wire fixtures under `tests/data/test_DML_diffs/` are payloads of real Dash Core replies
//! (see `PROVENANCE.md` there). `test-vectors/golden/mnlistdiff-core-ground-truth.json` holds what
//! Core's own `CSimplifiedMNListDiff` / `CalcHash` / `CalcMerkleRoot` computed from the same bytes
//! (`test-vectors/golden/PROVENANCE.md`). Nothing in this file is derived from `dashcore`.

use dash_core_vectors::golden::{
    GoldenDiff, GoldenMnListDiff, mnlistdiff_ground_truth, qrinfo_ground_truth,
};
use dash_core_vectors::hex::uint256_to_hex;
use dashcore::consensus::{deserialize, serialize};
use dashcore::hashes::{Hash, sha256};
use dashcore::network::message_qrinfo::QRInfo;
use dashcore::network::message_sml::MnListDiff;
use dashcore::sml::masternode_list::MasternodeList;
use dashcore::sml::masternode_list_entry::EntryMasternodeType;
use dashcore::{BlockHash, Network};
use std::path::Path;

fn read(repo_path: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(repo_path);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn wire(g: &GoldenMnListDiff) -> Vec<u8> {
    read(g.repo_path.as_deref().unwrap_or_else(|| panic!("{} has no repoPath", g.name)))
}

fn decode(g: &GoldenMnListDiff) -> MnListDiff {
    deserialize(&wire(g)).unwrap_or_else(|e| panic!("{}: cannot decode: {e}", g.name))
}

#[test]
fn fixtures_are_the_bytes_core_computed_its_ground_truth_from() {
    for g in mnlistdiff_ground_truth() {
        let bytes = wire(&g);
        assert_eq!(bytes.len(), g.wire_bytes, "{}", g.name);
        assert_eq!(sha256::Hash::hash(&bytes).to_string(), g.wire_sha256, "{}", g.name);
    }
}

#[test]
fn core_reserializes_what_dashcore_decodes_and_dashcore_reserializes_it_too() {
    for g in mnlistdiff_ground_truth() {
        assert!(g.core_round_trip_exact, "{}", g.name);
        assert_eq!(
            serialize(&decode(&g)),
            wire(&g),
            "{}: dashcore does not re-encode Core's bytes",
            g.name
        );
    }
}

/// Everything Core's ground truth says about one diff: hashes, deleted lists, every entry's
/// `CalcHash` with the fields that feed it, every quorum commitment hash, deleted quorums and the
/// number of ChainLock signature groups.
fn assert_diff_is_core_ground_truth(what: &str, diff: &MnListDiff, core: &GoldenDiff) {
    assert_eq!(diff.base_block_hash.to_byte_array(), core.base_block_hash, "{what}");
    assert_eq!(diff.block_hash.to_byte_array(), core.block_hash, "{what}");
    assert_eq!(diff.version, core.version, "{what}");
    let deleted: Vec<[u8; 32]> =
        diff.deleted_masternodes.iter().map(|h| h.to_byte_array()).collect();
    assert_eq!(deleted, core.deleted_mns, "{what}");
    assert_eq!(diff.new_masternodes.len(), core.entries.len(), "{what}");
    for (entry, c) in diff.new_masternodes.iter().zip(&core.entries) {
        let what = format!("{what}: entry {}", uint256_to_hex(&c.pro_reg_tx_hash));
        assert_eq!(entry.pro_reg_tx_hash.to_byte_array(), c.pro_reg_tx_hash, "{what}");
        assert_eq!(entry.version, c.version, "{what}");
        assert_eq!(entry.is_valid, c.is_valid, "{what}");
        let node_type = match entry.mn_type {
            EntryMasternodeType::Regular => 0,
            EntryMasternodeType::HighPerformance {
                ..
            } => 1,
        };
        assert_eq!(node_type, c.node_type, "{what}");
        assert_eq!(entry.calculate_entry_hash().to_byte_array(), c.entry_hash, "{what}");
    }
    let deleted_quorums: Vec<(u8, [u8; 32])> = diff
        .deleted_quorums
        .iter()
        .map(|q| (u8::from(q.llmq_type), q.quorum_hash.to_byte_array()))
        .collect();
    assert_eq!(deleted_quorums, core.deleted_quorums, "{what}");
    assert_eq!(diff.new_quorums.len(), core.new_quorums.len(), "{what}");
    for (quorum, c) in diff.new_quorums.iter().zip(&core.new_quorums) {
        let what = format!("{what}: quorum {}", uint256_to_hex(&c.quorum_hash));
        assert_eq!(u8::from(quorum.llmq_type), c.llmq_type, "{what}");
        assert_eq!(quorum.quorum_hash.to_byte_array(), c.quorum_hash, "{what}");
        assert_eq!(quorum.quorum_index.unwrap_or(0), c.quorum_index, "{what}");
        assert_eq!(quorum.version, c.version, "{what}");
        assert_eq!(quorum.calculate_commitment_hash().to_byte_array(), c.commitment_hash, "{what}");
    }
    assert_eq!(diff.quorums_chainlock_signatures.len(), core.quorums_cl_sig_groups, "{what}");
}

#[test]
fn every_diff_is_what_core_computed() {
    for g in mnlistdiff_ground_truth() {
        assert_diff_is_core_ground_truth(&g.name, &decode(&g), &g.diff);
    }
}

/// The same for the `qrinfo` captures: the five (or six) diffs and the last commitment per quorum
/// index. The quorum snapshots are only covered by the byte-exact round trip: the ground truth does
/// not record the fields Core read from them (`skipListMode`, member bits, skip list).
#[test]
fn every_qrinfo_is_what_core_computed() {
    for g in qrinfo_ground_truth() {
        let path = g.repo_path.as_deref().expect("repoPath");
        let bytes = read(path);
        assert_eq!(sha256::Hash::hash(&bytes).to_string(), g.wire_sha256, "{}", g.name);
        assert!(g.core_round_trip_exact, "{}", g.name);
        let info: QRInfo = deserialize(&bytes).unwrap_or_else(|e| panic!("{}: {e}", g.name));
        assert_eq!(serialize(&info), bytes, "{}: dashcore does not re-encode Core's bytes", g.name);

        let diffs = [
            ("tip", &info.mn_list_diff_tip, &g.tip),
            ("h", &info.mn_list_diff_h, &g.h),
            ("h-c", &info.mn_list_diff_at_h_minus_c, &g.h_minus_c),
            ("h-2c", &info.mn_list_diff_at_h_minus_2c, &g.h_minus_2c),
            ("h-3c", &info.mn_list_diff_at_h_minus_3c, &g.h_minus_3c),
        ];
        for (label, diff, core) in diffs {
            assert_diff_is_core_ground_truth(&format!("{} {label}", g.name), diff, core);
        }
        assert_eq!(g.extra_share, info.quorum_snapshot_and_mn_list_diff_at_h_minus_4c.is_some());
        if let (Some((_, diff)), Some(core)) =
            (&info.quorum_snapshot_and_mn_list_diff_at_h_minus_4c, &g.h_minus_4c)
        {
            assert_diff_is_core_ground_truth(&format!("{} h-4c", g.name), diff, core);
        }
        assert_eq!(info.last_commitment_per_index.len(), g.last_commitment_per_index.len());
        for (quorum, c) in info.last_commitment_per_index.iter().zip(&g.last_commitment_per_index) {
            let what = format!("{}: last commitment {}", g.name, uint256_to_hex(&c.quorum_hash));
            assert_eq!(quorum.quorum_hash.to_byte_array(), c.quorum_hash, "{what}");
            assert_eq!(quorum.quorum_index.unwrap_or(0), c.quorum_index, "{what}");
            assert_eq!(
                quorum.calculate_commitment_hash().to_byte_array(),
                c.commitment_hash,
                "{what}"
            );
        }
        assert_eq!(info.quorum_snapshot_list.len(), 0, "{}", g.name);
        assert_eq!(info.mn_list_diff_list.len(), 0, "{}", g.name);
    }
}

/// The merkle root of the masternode list a diff leads to must equal the root in the diff's own
/// coinbase, and Core computed the same value from the same entries.
#[test]
fn merkle_root_of_the_resulting_list_equals_the_core_root() {
    let golden = mnlistdiff_ground_truth();
    let by_name =
        |name: &str| golden.iter().find(|g| g.name == name).unwrap_or_else(|| panic!("{name}"));

    let first = by_name("mainnet-0-2227096");
    let list = MasternodeList::from_diff(decode(first), 2_227_096, Network::Mainnet).unwrap();
    let root = list.calculate_masternodes_merkle_root(2_227_096).unwrap();
    assert_eq!(root.to_byte_array(), first.computed_merkle_root_mn_list.unwrap());
    assert_eq!(root.to_byte_array(), first.cbtx_merkle_root_mn_list);
    assert_eq!(list.masternodes.len(), first.list_size_after_diff.unwrap());

    let second = by_name("mainnet-2227096-2241332");
    assert_eq!(second.diff.base_block_hash, first.diff.block_hash);
    let list = list.apply_diff(decode(second), 2_241_332, Network::Mainnet).unwrap();
    let root = list.calculate_masternodes_merkle_root(2_241_332).unwrap();
    assert_eq!(root.to_byte_array(), second.computed_merkle_root_mn_list.unwrap());
    assert_eq!(root.to_byte_array(), second.cbtx_merkle_root_mn_list);
    assert_eq!(list.masternodes.len(), second.list_size_after_diff.unwrap());
    assert_eq!(list.block_hash, BlockHash::from_byte_array(second.diff.block_hash));

    let testnet = by_name("testnet-0-1296600");
    let list = MasternodeList::from_diff(decode(testnet), 1_296_600, Network::Testnet).unwrap();
    let root = list.calculate_masternodes_merkle_root(1_296_600).unwrap();
    assert_eq!(root.to_byte_array(), testnet.computed_merkle_root_mn_list.unwrap());
    assert_eq!(root.to_byte_array(), testnet.cbtx_merkle_root_mn_list);
}
