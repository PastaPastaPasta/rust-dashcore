//! Core's serialization of an LLMQ final commitment (`CFinalCommitment`) and its hash.
//!
//! Core hashes its re-serialization of a decoded commitment (`::SerializeHash(qc)`), which keys
//! the mined-commitment index and is each leaf of the coinbase's quorum merkle root, so the bytes
//! come from here, not from base-sdk's `Commitment` encoder (A1 §1.3).

use dash_primitives::{Commitment, commitment_bls_scheme};
use dash_types::Numeric;

use super::serialize::{CoreWrite, HashWriter, write_dyn_bitset};

/// `LEGACY_BLS_INDEXED_QUORUM_VERSION`.
const LEGACY_BLS_INDEXED_QUORUM_VERSION: u16 = 2;
/// `BASIC_BLS_INDEXED_QUORUM_VERSION`.
const BASIC_BLS_INDEXED_QUORUM_VERSION: u16 = 4;

// Core: llmq/commitment.h:95 CFinalCommitment SERIALIZE_METHODS
/// Writes Core's serialization of `qc`: `nVersion`, `llmqType`, `quorumHash`, `quorumIndex` for
/// the indexed versions (2 and 4) only, the signers and valid-members bitsets as `DYNBITSET`,
/// then `quorumPublicKey`, `quorumVvecHash`, `quorumSig` and `membersSig`, the BLS fields in the
/// legacy scheme for versions 1 and 2 and the basic one otherwise (zeros for an invalid field).
///
/// The version alone decides whether `quorumIndex` is written; an indexed version without one
/// writes Core's default, 0.
pub fn write_commitment(qc: &Commitment, w: &mut impl CoreWrite) {
    let scheme = commitment_bls_scheme(qc.version);
    w.write(&qc.version.to_le_bytes());
    w.write(&[qc.llmq_type.to_base()]);
    w.write(qc.quorum_hash.as_bytes());
    if matches!(qc.version, LEGACY_BLS_INDEXED_QUORUM_VERSION | BASIC_BLS_INDEXED_QUORUM_VERSION) {
        w.write(&qc.quorum_index.unwrap_or(0).to_le_bytes());
    }
    write_dyn_bitset(&qc.signers, w);
    write_dyn_bitset(&qc.valid_members, w);
    w.write(&qc.quorum_public_key.to_bytes(scheme));
    w.write(qc.quorum_vvec_hash.as_bytes());
    w.write(&qc.quorum_sig.to_bytes(scheme));
    w.write(&qc.members_sig.to_bytes(scheme));
}

// Core: llmq/blockprocessor.cpp:452 ::SerializeHash(qc)
/// `::SerializeHash(qc)`: the SHA256d of [`write_commitment`]'s bytes.
pub fn commitment_hash(qc: &Commitment) -> [u8; 32] {
    let mut w = HashWriter::new();
    write_commitment(qc, &mut w);
    w.finish()
}

#[cfg(test)]
mod tests {
    use dash_params::LlmqType;
    use dash_pkc::bls::core::{BlsPkValue, BlsSigValue};
    use dash_pkc::bls::{
        BlsEagerPk, BlsEagerSig, BlsScIetf, BlsSchemeKind, BlsSecretKey, BlsSigId,
    };
    use dash_primitives::{DynBitset, QuorumHash, QuorumVvecHash};

    use super::*;
    use crate::hash::sha256d;

    /// A null commitment of `version`: what Core's miner builds (`CFinalCommitment(params,
    /// quorumHash)`), with `size`-bit bitsets.
    fn null(version: u16, size: u64) -> Commitment {
        let bits = DynBitset {
            num_bits: size,
            data: vec![0; size.div_ceil(8) as usize],
        };
        let scheme = commitment_bls_scheme(version);
        Commitment {
            version,
            llmq_type: LlmqType::from_base(100),
            quorum_hash: QuorumHash::from([0x42; 32]),
            quorum_index: matches!(version, 2 | 4).then_some(0),
            signers: bits.clone(),
            valid_members: bits,
            quorum_public_key: BlsEagerPk::new(BlsPkValue::invalid(), scheme),
            quorum_vvec_hash: QuorumVvecHash::from([0; 32]),
            quorum_sig: BlsEagerSig::new(BlsSigValue::invalid(), scheme),
            members_sig: BlsEagerSig::new(BlsSigValue::invalid(), scheme),
        }
    }

    fn bytes(qc: &Commitment) -> Vec<u8> {
        let mut out = Vec::new();
        write_commitment(qc, &mut out);
        out
    }

    /// Core's layout field by field: the index only for versions 2 and 4, the bitsets as
    /// `DYNBITSET`, then the BLS fields and the vvec hash. Versions outside 1 to 4 decode (a
    /// `qfcommit` is hashed before its version is checked, `blockprocessor.cpp:115`) and carry no
    /// index.
    #[test]
    fn the_layout_is_cores_for_every_version() {
        for version in [0, 1, 2, 3, 4, 5, 6, 0xffff] {
            let mut qc = null(version, 3);
            qc.signers.data = vec![0b101];
            qc.valid_members.data = vec![0b111];
            if let Some(index) = qc.quorum_index.as_mut() {
                *index = -2;
            }
            let mut want = version.to_le_bytes().to_vec();
            want.push(100);
            want.extend_from_slice(&[0x42; 32]);
            if matches!(version, 2 | 4) {
                want.extend_from_slice(&(-2i16).to_le_bytes());
            }
            want.extend_from_slice(&[3, 0b101, 3, 0b111]);
            want.extend_from_slice(&[0; 48 + 32 + 96 + 96]);
            assert_eq!(bytes(&qc), want, "version {version}");
            assert_eq!(commitment_hash(&qc), sha256d(&want), "version {version}");
        }
    }

    /// Core writes `quorumIndex` by version, not by whether the object holds one: a
    /// non-indexed version drops a stray index, an indexed one without an index writes 0.
    #[test]
    fn the_version_alone_gates_the_quorum_index() {
        let mut stray = null(3, 3);
        stray.quorum_index = Some(7);
        assert_eq!(bytes(&stray), bytes(&null(3, 3)));
        let mut missing = null(4, 3);
        missing.quorum_index = None;
        assert_eq!(bytes(&missing), bytes(&null(4, 3)));
        let mut indexed = null(4, 3);
        indexed.quorum_index = Some(0x0102);
        assert_eq!(bytes(&indexed)[35..37], [0x02, 0x01]);
    }

    /// The BLS fields follow the version's scheme (legacy for versions 1 and 2), each in its place:
    /// a valid key or signature serializes differently in the two schemes.
    #[test]
    fn the_bls_fields_take_the_versions_scheme() {
        let secret = |seed: u8| BlsSecretKey::<BlsScIetf>::from_ikm(&[seed; 32]).expect("a key");
        let key = BlsEagerPk::new(
            BlsPkValue::from_canonical(&secret(7).public_key().to_bytes(), BlsSchemeKind::Basic)
                .expect("canonical"),
            BlsSchemeKind::Basic,
        );
        let sig = |seed: u8| {
            let sig = secret(seed).sign_with(&[seed; 32], BlsSigId::Basic);
            BlsEagerSig::new(
                BlsSigValue::from_canonical(&sig.to_bytes(), BlsSchemeKind::Basic)
                    .expect("canonical"),
                BlsSchemeKind::Basic,
            )
        };
        let (quorum_sig, members_sig) = (sig(8), sig(9));
        for (version, scheme) in [
            (1, BlsSchemeKind::Legacy),
            (2, BlsSchemeKind::Legacy),
            (3, BlsSchemeKind::Basic),
            (4, BlsSchemeKind::Basic),
        ] {
            let mut qc = null(version, 3);
            qc.quorum_public_key = key.clone();
            qc.quorum_vvec_hash = QuorumVvecHash::from([0x11; 32]);
            qc.quorum_sig = quorum_sig.clone();
            qc.members_sig = members_sig.clone();
            let want = [
                &key.to_bytes(scheme)[..],
                &[0x11; 32],
                &quorum_sig.to_bytes(scheme),
                &members_sig.to_bytes(scheme),
            ]
            .concat();
            // The last fields, after the index and bitsets the layout test checks.
            let written = bytes(&qc);
            assert_eq!(written[written.len() - want.len()..], want, "version {version}");
        }
        // Each field differs between the schemes, and the signatures from each other.
        assert_ne!(key.to_bytes(BlsSchemeKind::Legacy), key.to_bytes(BlsSchemeKind::Basic));
        for sig in [&quorum_sig, &members_sig] {
            assert_ne!(sig.to_bytes(BlsSchemeKind::Legacy), sig.to_bytes(BlsSchemeKind::Basic));
        }
        assert_ne!(
            quorum_sig.to_bytes(BlsSchemeKind::Basic),
            members_sig.to_bytes(BlsSchemeKind::Basic)
        );
    }
}
