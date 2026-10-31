//! Loaders for `golden/*.json`: what Dash Core computed from captured Core wire payloads.
//!
//! The files are produced by `src/test/o4_vector_dump_tests.cpp` on the `node/o4-core-vectors`
//! Dash Core branch (see `test-vectors/golden/PROVENANCE.md`). Hashes in the JSON are in Core's
//! display form (`uint256::ToString`); these loaders return them in internal byte order, the order
//! Rust hash types store (`to_byte_array`).

use std::fmt;

use crate::hex::uint256_from_hex;
use crate::univalue::UniValue;

#[derive(Debug, Clone, PartialEq)]
pub struct GoldenError(pub String);

impl fmt::Display for GoldenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GoldenError {}

fn err(what: impl fmt::Display) -> GoldenError {
    GoldenError(what.to_string())
}

/// `[proRegTxHash, nVersion, nType, isValid, CSimplifiedMNListEntry::CalcHash()]`
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenEntry {
    pub pro_reg_tx_hash: [u8; 32],
    pub version: u16,
    /// `MnType`: 0 regular, 1 high-performance (Evo).
    pub node_type: u8,
    pub is_valid: bool,
    pub entry_hash: [u8; 32],
}

/// `[llmqType, quorumHash, quorumIndex, nVersion, BuildCommitmentHash()]`
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenCommitment {
    pub llmq_type: u8,
    pub quorum_hash: [u8; 32],
    pub quorum_index: i16,
    pub version: u16,
    pub commitment_hash: [u8; 32],
}

/// What Core computed for one `CSimplifiedMNListDiff`.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenDiff {
    pub version: u16,
    pub base_block_hash: [u8; 32],
    pub block_hash: [u8; 32],
    pub deleted_mns: Vec<[u8; 32]>,
    pub entries: Vec<GoldenEntry>,
    /// `(llmqType, quorumHash)`
    pub deleted_quorums: Vec<(u8, [u8; 32])>,
    pub new_quorums: Vec<GoldenCommitment>,
    pub quorums_cl_sig_groups: usize,
}

/// One captured `mnlistdiff` message.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenMnListDiff {
    pub name: String,
    /// The fixture file this describes, relative to the repository root.
    pub repo_path: Option<String>,
    pub wire_bytes: usize,
    pub wire_sha256: String,
    /// Core's `CSimplifiedMNListDiff` serializes the capture back to the same bytes.
    pub core_round_trip_exact: bool,
    pub cbtx_height: i32,
    pub cbtx_merkle_root_mn_list: [u8; 32],
    /// Present for diffs that continue a chain of captures (the root of the list after applying
    /// them, from Core's `CSimplifiedMNList::CalcMerkleRoot`).
    pub computed_merkle_root_mn_list: Option<[u8; 32]>,
    pub list_size_after_diff: Option<usize>,
    pub diff: GoldenDiff,
}

/// One captured `qrinfo` message. The embedded tip diff is the responding node's live tip at the
/// time of the capture; every other part is a function of the requested block.
#[derive(Debug, Clone, PartialEq)]
pub struct GoldenQrInfo {
    pub name: String,
    pub repo_path: Option<String>,
    pub wire_bytes: usize,
    pub wire_sha256: String,
    pub core_round_trip_exact: bool,
    pub extra_share: bool,
    pub tip: GoldenDiff,
    pub h: GoldenDiff,
    pub h_minus_c: GoldenDiff,
    pub h_minus_2c: GoldenDiff,
    pub h_minus_3c: GoldenDiff,
    pub h_minus_4c: Option<GoldenDiff>,
    pub last_commitment_per_index: Vec<GoldenCommitment>,
}

fn hash(v: &UniValue) -> Result<[u8; 32], GoldenError> {
    let s = v.get_str().map_err(err)?;
    if s.len() != 64 {
        return Err(err(format!("not a 64-digit hash: {s:?}")));
    }
    Ok(uint256_from_hex(s))
}

fn int<T: TryFrom<i64>>(v: &UniValue) -> Result<T, GoldenError> {
    let n = v.get_i64().map_err(err)?;
    T::try_from(n).map_err(|_| err(format!("integer {n} out of range")))
}

fn entry(row: &UniValue) -> Result<GoldenEntry, GoldenError> {
    Ok(GoldenEntry {
        pro_reg_tx_hash: hash(row.at(0))?,
        version: int::<u16>(row.at(1))?,
        node_type: int::<u8>(row.at(2))?,
        is_valid: row.at(3).get_bool().map_err(err)?,
        entry_hash: hash(row.at(4))?,
    })
}

fn commitment(row: &UniValue) -> Result<GoldenCommitment, GoldenError> {
    Ok(GoldenCommitment {
        llmq_type: int::<u8>(row.at(0))?,
        quorum_hash: hash(row.at(1))?,
        quorum_index: int::<i16>(row.at(2))?,
        version: int::<u16>(row.at(3))?,
        commitment_hash: hash(row.at(4))?,
    })
}

fn diff(v: &UniValue) -> Result<GoldenDiff, GoldenError> {
    let rows = |key: &str| v.find_value(key).get_array().map_err(err);
    Ok(GoldenDiff {
        version: int::<u16>(v.find_value("version"))?,
        base_block_hash: hash(v.find_value("baseBlockHash"))?,
        block_hash: hash(v.find_value("blockHash"))?,
        deleted_mns: rows("deletedMNs")?.iter().map(hash).collect::<Result<_, _>>()?,
        entries: rows("mnList")?.iter().map(entry).collect::<Result<_, _>>()?,
        deleted_quorums: rows("deletedQuorums")?
            .iter()
            .map(|r| Ok((int::<u8>(r.at(0))?, hash(r.at(1))?)))
            .collect::<Result<_, GoldenError>>()?,
        new_quorums: rows("newQuorums")?.iter().map(commitment).collect::<Result<_, _>>()?,
        quorums_cl_sig_groups: int::<usize>(v.find_value("quorumsCLSigGroups"))?,
    })
}

fn opt_str(v: &UniValue, key: &str) -> Option<String> {
    v.find_value(key).get_str().ok().map(str::to_owned)
}

/// Parses `golden/mnlistdiff-core-ground-truth.json`.
pub fn parse_mnlistdiff_ground_truth(text: &str) -> Result<Vec<GoldenMnListDiff>, GoldenError> {
    let root = UniValue::parse(text).map_err(err)?;
    root.get_array()
        .map_err(err)?
        .iter()
        .map(|r| {
            let roots = r.find_value("computedMerkleRootMNList");
            Ok(GoldenMnListDiff {
                name: opt_str(r, "name").ok_or_else(|| err("missing name"))?,
                repo_path: opt_str(r, "repoPath"),
                wire_bytes: int::<usize>(r.find_value("wireBytes"))?,
                wire_sha256: opt_str(r, "wireSha256").ok_or_else(|| err("missing wireSha256"))?,
                core_round_trip_exact: r
                    .find_value("coreRoundTripExact")
                    .get_bool()
                    .map_err(err)?,
                cbtx_height: int::<i32>(r.find_value("cbTxHeight"))?,
                cbtx_merkle_root_mn_list: hash(r.find_value("cbTxMerkleRootMNList"))?,
                computed_merkle_root_mn_list: if roots.is_null() {
                    None
                } else {
                    Some(hash(roots)?)
                },
                list_size_after_diff: {
                    let n = r.find_value("listSizeAfterDiff");
                    if n.is_null() {
                        None
                    } else {
                        Some(int::<usize>(n)?)
                    }
                },
                diff: diff(r.find_value("diff"))?,
            })
        })
        .collect()
}

/// Parses `golden/qrinfo-core-ground-truth.json`.
pub fn parse_qrinfo_ground_truth(text: &str) -> Result<Vec<GoldenQrInfo>, GoldenError> {
    let root = UniValue::parse(text).map_err(err)?;
    root.get_array()
        .map_err(err)?
        .iter()
        .map(|r| {
            let four_c = r.find_value("mnListDiffHMinus4C");
            Ok(GoldenQrInfo {
                name: opt_str(r, "name").ok_or_else(|| err("missing name"))?,
                repo_path: opt_str(r, "repoPath"),
                wire_bytes: int::<usize>(r.find_value("wireBytes"))?,
                wire_sha256: opt_str(r, "wireSha256").ok_or_else(|| err("missing wireSha256"))?,
                core_round_trip_exact: r
                    .find_value("coreRoundTripExact")
                    .get_bool()
                    .map_err(err)?,
                extra_share: r.find_value("extraShare").get_bool().map_err(err)?,
                tip: diff(r.find_value("mnListDiffTip"))?,
                h: diff(r.find_value("mnListDiffH"))?,
                h_minus_c: diff(r.find_value("mnListDiffHMinusC"))?,
                h_minus_2c: diff(r.find_value("mnListDiffHMinus2C"))?,
                h_minus_3c: diff(r.find_value("mnListDiffHMinus3C"))?,
                h_minus_4c: if four_c.is_null() {
                    None
                } else {
                    Some(diff(four_c)?)
                },
                last_commitment_per_index: r
                    .find_value("lastCommitmentPerIndex")
                    .get_array()
                    .map_err(err)?
                    .iter()
                    .map(commitment)
                    .collect::<Result<_, _>>()?,
            })
        })
        .collect()
}

/// `golden/mnlistdiff-core-ground-truth.json`, parsed.
pub fn mnlistdiff_ground_truth() -> Vec<GoldenMnListDiff> {
    let text = crate::read_vector_text("golden/mnlistdiff-core-ground-truth.json");
    parse_mnlistdiff_ground_truth(&text).unwrap_or_else(|e| panic!("mnlistdiff ground truth: {e}"))
}

/// `golden/qrinfo-core-ground-truth.json`, parsed.
pub fn qrinfo_ground_truth() -> Vec<GoldenQrInfo> {
    let text = crate::read_vector_text("golden/qrinfo-core-ground-truth.json");
    parse_qrinfo_ground_truth(&text).unwrap_or_else(|e| panic!("qrinfo ground truth: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_truth_files_parse_and_core_agreed_with_itself() {
        let diffs = mnlistdiff_ground_truth();
        assert_eq!(diffs.len(), 35);
        for d in &diffs {
            assert!(d.core_round_trip_exact, "{}", d.name);
            if let Some(root) = d.computed_merkle_root_mn_list {
                assert_eq!(root, d.cbtx_merkle_root_mn_list, "{}", d.name);
            }
            assert_eq!(d.diff.block_hash.len(), 32);
        }
        // The three chain heads that carry a root check.
        assert_eq!(diffs.iter().filter(|d| d.computed_merkle_root_mn_list.is_some()).count(), 3);

        let qrs = qrinfo_ground_truth();
        assert_eq!(qrs.len(), 2);
        for q in &qrs {
            assert!(q.core_round_trip_exact, "{}", q.name);
            assert!(q.extra_share && q.h_minus_4c.is_some());
            assert_eq!(q.last_commitment_per_index.len(), 32);
        }
    }
}
