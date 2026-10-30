//! Payload builders for the session's tests, which may not name BS9a's types (LQ4 §6).

#![allow(clippy::disallowed_methods, reason = "tests build payloads with base-sdk's encoder")]

use dash_p2p_core::DkgRevealedShare;
use dash_pkc::bls::{BlsIesMultiBytes, BlsPkBytes, BlsSigBytes};
use dash_primitives::DynBitset;
use dash_types::codec::BaseCodec;

use super::*;

/// The prefix every DKG message starts with.
pub(crate) struct Prefix {
    pub(crate) t: LlmqType,
    pub(crate) quorum_hash: QuorumHash,
    pub(crate) pro_tx_hash: ProTxHash,
}

impl Prefix {
    fn pro_tx(&self) -> TxHash {
        TxHash::from(*self.pro_tx_hash.as_bytes())
    }
}

fn bitset(bits: &[bool]) -> DynBitset {
    let mut data = vec![0u8; bits.len().div_ceil(8)];
    for (i, _) in bits.iter().enumerate().filter(|(_, b)| **b) {
        data[i / 8] |= 1 << (i % 8);
    }
    DynBitset {
        num_bits: bits.len() as u64,
        data,
    }
}

fn encode(msg: &impl BaseCodec) -> Vec<u8> {
    let mut out = Vec::new();
    msg.encode(&mut out);
    out
}

/// A `qcontrib` payload with its BLS fields in the basic layout.
pub(crate) fn contribution(
    p: &Prefix,
    vvec: &[BlsPkValue],
    blobs: &BlsIesMultiBlobs,
    sig: [u8; 96],
) -> Vec<u8> {
    let basic = BlsSchemeKind::Basic;
    encode(&DkgContribution {
        llmq_type: p.t,
        quorum_hash: p.quorum_hash,
        pro_tx_hash: p.pro_tx(),
        vvec: vvec.iter().map(|pk| BlsPkBytes::from_bytes(pk.to_bytes(basic))).collect(),
        contributions: BlsIesMultiBytes::new(
            BlsPkBytes::from_bytes(blobs.ephemeral.to_bytes(basic)),
            blobs.iv_seed,
            blobs.blobs.clone(),
        ),
        sig: BlsSigBytes::from_bytes(sig),
    })
}

/// A `qcomplaint` payload.
pub(crate) fn complaint(p: &Prefix, bad: &[bool], complain_for: &[bool], sig: [u8; 96]) -> Vec<u8> {
    encode(&DkgComplaint {
        llmq_type: p.t,
        quorum_hash: p.quorum_hash,
        pro_tx_hash: p.pro_tx(),
        bad_members: bitset(bad),
        complain_for_members: bitset(complain_for),
        sig: BlsSigBytes::from_bytes(sig),
    })
}

/// A `qjustify` payload.
pub(crate) fn justification(p: &Prefix, shares: &[(u32, [u8; 32])], sig: [u8; 96]) -> Vec<u8> {
    encode(&DkgJustification {
        llmq_type: p.t,
        quorum_hash: p.quorum_hash,
        pro_tx_hash: p.pro_tx(),
        contributions: shares
            .iter()
            .map(|(index, key)| DkgRevealedShare {
                index: *index,
                key: *key,
            })
            .collect(),
        sig: BlsSigBytes::from_bytes(sig),
    })
}

/// A `qpcommit` payload.
pub(crate) fn premature_commitment(
    p: &Prefix,
    valid: &[bool],
    quorum_sig: [u8; 96],
    sig: [u8; 96],
) -> Vec<u8> {
    encode(&DkgPrematureCommitment {
        llmq_type: p.t,
        quorum_hash: p.quorum_hash,
        pro_tx_hash: p.pro_tx(),
        valid_members: bitset(valid),
        quorum_public_key: BlsPkBytes::from_bytes([0; 48]),
        quorum_vvec_hash: Default::default(),
        quorum_sig: BlsSigBytes::from_bytes(quorum_sig),
        sig: BlsSigBytes::from_bytes(sig),
    })
}
