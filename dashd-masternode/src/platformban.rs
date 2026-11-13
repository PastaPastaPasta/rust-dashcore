//! Core parity: v24.0.0 net_processing.cpp:3906-3913.

use dash_consensus::BlsCtx;
use dash_consensus::hash::HashWriter;
use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_evo::locks::sign_hash;
use dash_evo::types::ProTxHash;
use dash_params::ChainSetup;
use dash_pkc::bls::core::{BlsPkValue, BlsSigValue, BlsVerdict, verify_insecure};
use dash_primitives::QuorumHash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BanSig {
    Valid,
    Invalid,
    Unsupported,
}

impl From<BlsVerdict> for BanSig {
    fn from(verdict: BlsVerdict) -> Self {
        match verdict {
            BlsVerdict::Valid => Self::Valid,
            BlsVerdict::Invalid => Self::Invalid,
            BlsVerdict::Unsupported => Self::Unsupported,
        }
    }
}

pub struct BanQuorum<'a> {
    pub quorum_hash: &'a QuorumHash,
    pub public_key: &'a BlsPkValue,
}

// Core: v24.0.0 net_processing.cpp:3906-3909.
fn ban_hashes(protx: &ProTxHash, height: i32) -> ([u8; 32], [u8; 32]) {
    let mut request = HashWriter::new();
    let prefix = b"PlatformPoSeBan";
    write_compact_size(prefix.len() as u64, &mut request);
    request.write(prefix);
    request.write(protx.as_bytes());
    request.write(&height.to_le_bytes());
    let mut message = HashWriter::new();
    message.write(protx.as_bytes());
    message.write(&height.to_le_bytes());
    (request.finish(), message.finish())
}

// Core: v24.0.0 net_processing.cpp:3898, :3906-3913; bls/bls.cpp:310-325.
pub fn verify_ban_sig(
    params: &ChainSetup,
    protx: &ProTxHash,
    height: i32,
    quorum: BanQuorum<'_>,
    sig: &BlsSigValue,
    bls: BlsCtx,
) -> BanSig {
    let (request_id, msg_hash) = ban_hashes(protx, height);
    let hash =
        sign_hash(params.consensus.llmq_type_platform, quorum.quorum_hash, &request_id, &msg_hash);
    verify_insecure(sig, quorum.public_key, &hash, bls.scheme()).into()
}

#[cfg(test)]
mod tests;
