//! Whether a final commitment is null, read through its BLS side-car (T6 Q-4a5): a field whose
//! bytes are a valid object only under the other scheme is the invalid object, so it counts as
//! absent.

use dash_consensus::primitives::{P2pCommitment, commitment_is_null, decode_tx_exact};
use dash_core_vectors::hex::parse_hex;
use dash_primitives::SpecialPayload;

const ROWS: &str = include_str!("data/qctx-null-other-scheme.hex");

/// The rows' transaction bytes, with the crafted verification vector hash (32 × 0x06) zeroed so
/// that only the signers, the valid members and the BLS fields decide.
fn rows() -> Vec<(&'static str, Vec<u8>)> {
    let vvec = "06".repeat(32);
    ROWS.lines()
        .filter(|line| !line.starts_with('#'))
        .map(|line| {
            let (name, hex) = line.split_once(' ').expect("a row name and its hex");
            assert_eq!(hex.matches(vvec.as_str()).count(), 1, "{name}: one vvec hash");
            (name, parse_hex(&hex.replace(vvec.as_str(), &"00".repeat(32))))
        })
        .collect()
}

#[test]
fn null_commitment_ignores_fields_of_the_other_scheme() {
    let rows = rows();
    let names: Vec<_> = rows.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["qctx_v1_basic", "qctx_v3_basic", "qctx_v3_legacy"]);
    let mut wrong = Vec::new();
    for (name, bytes) in &rows {
        let decoded = decode_tx_exact(bytes).expect("the row decodes");
        let payload =
            decoded.value().payload().expect("a payload field").expect("the payload decodes");
        let SpecialPayload::QuorumCommitment(qc_tx) = payload.payload() else {
            panic!("{name}: a quorum commitment payload");
        };
        let qc = qc_tx.commitment();
        assert_eq!(qc.signers().count_ones() + qc.valid_members().count_ones(), 0, "{name}");
        let fields = payload.commitment_bls().expect("the side-car of a qcTx");
        let null = *name != "qctx_v3_basic";
        if commitment_is_null(qc, fields) != null
            || P2pCommitment::new(qc.clone()).is_null() != null
        {
            wrong.push(*name);
        }
    }
    assert!(wrong.is_empty(), "rows with the wrong nullness: {wrong:?}");
}
