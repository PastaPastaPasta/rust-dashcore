use crate::dkg::bls::{BlsPkValue, BlsSkValue};
use crate::dkg::wire::QuorumHash;
use dash_consensus::primitives::{CoreWrite, HashWriter, read_compact_size, write_compact_size};
use dash_evo::types::ProTxHash;
use dash_params::LlmqType;
use dash_types::Numeric;

use super::QuorumKeys;
use crate::dkg::bls::BlsSchemeKind;

/// The key used by Core's two quorum contribution rows.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct QuorumDbKey(pub [u8; 32]);

// Core: v24.0.0 src/llmq/quorums.cpp:20 MakeQuorumKey
pub fn quorum_db_key(t: LlmqType, quorum_hash: &QuorumHash, members: &[ProTxHash]) -> QuorumDbKey {
    let mut w = HashWriter::new();
    w.write(&[t.to_base()]);
    w.write(quorum_hash.as_bytes());
    for member in members {
        w.write(member.as_bytes());
    }
    QuorumDbKey(w.finish())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VvecReadError {
    MalformedCount,
    TruncatedRow,
    UndecodableElement {
        index: usize,
    },
}

#[derive(Clone, Debug)]
pub struct StoredContributions {
    pub vvec: Result<Option<Vec<BlsPkValue>>, VvecReadError>,
    pub sk_share: Option<BlsSkValue>,
}

// Core: v24.0.0 src/llmq/quorums.cpp:197 CQuorum::WriteContributions
pub fn encode_vvec(v: &[BlsPkValue]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + 48 * v.len());
    write_compact_size(v.len() as u64, &mut out);
    for pk in v {
        out.extend_from_slice(&pk.to_bytes(BlsSchemeKind::Basic));
    }
    out
}

// Core: v24.0.0 src/bls/bls.h:184-200; src/llmq/quorums.cpp:210-233 ReadContributions.
pub fn decode_vvec(mut bytes: &[u8]) -> Result<Vec<BlsPkValue>, VvecReadError> {
    let count = usize::try_from(
        read_compact_size(&mut bytes, true).map_err(|_| VvecReadError::MalformedCount)?,
    )
    .map_err(|_| VvecReadError::MalformedCount)?;
    let elements = bytes
        .get(..count.checked_mul(48).ok_or(VvecReadError::TruncatedRow)?)
        .ok_or(VvecReadError::TruncatedRow)?;
    let mut out = Vec::with_capacity(count);
    for (index, raw) in elements.as_chunks::<48>().0.iter().enumerate() {
        let key = match BlsPkValue::from_canonical(raw, BlsSchemeKind::Basic) {
            Some(key) => key,
            None if raw == &[0; 48] => BlsPkValue::invalid(),
            None if BlsPkValue::from_canonical(raw, BlsSchemeKind::Legacy).is_some() => {
                BlsPkValue::invalid()
            }
            None => {
                return Err(VvecReadError::UndecodableElement {
                    index,
                });
            }
        };
        out.push(key);
    }
    Ok(out)
}

/// Returns the basic-scheme bytes for storage. The caller must wipe the returned copy.
// Core: v24.0.0 src/llmq/quorums.cpp:205 CQuorum::WriteContributions
pub fn encode_sk(sk: &BlsSkValue) -> [u8; 32] {
    sk.to_bytes(BlsSchemeKind::Basic)
}
// Core: v24.0.0 src/llmq/quorums.cpp:231 CQuorum::ReadContributions
pub fn decode_sk(bytes: &[u8]) -> Option<BlsSkValue> {
    let raw: [u8; 32] = bytes.get(..32)?.try_into().ok()?;
    if raw == [0; 32] {
        Some(BlsSkValue::invalid())
    } else {
        // Core can leave a reduced key in place before rejecting a corrupt row above the
        // group order. Core never writes such a row, so malformed storage is rejected here.
        BlsSkValue::from_canonical(&raw, BlsSchemeKind::Basic)
    }
}

// Core: v24.0.0 src/llmq/quorums.cpp:192 WriteContributions.
pub fn to_store(keys: &QuorumKeys) -> StoredContributions {
    StoredContributions {
        vvec: Ok(keys.vvec.as_ref().map(|v| (**v).clone())),
        sk_share: keys.sk_share.clone().filter(BlsSkValue::is_valid),
    }
}

// Core: v24.0.0 src/bls/bls.h:184-200; src/llmq/quorums.cpp:210-233 ReadContributions.
pub fn from_store(s: StoredContributions) -> Result<Option<QuorumKeys>, VvecReadError> {
    let Some(vvec) = s.vvec? else {
        return Ok(None);
    };
    let mut keys = QuorumKeys::default();
    keys.set_vvec_unchecked(std::sync::Arc::new(vvec));
    keys.sk_share = s.sk_share.filter(BlsSkValue::is_valid);
    Ok(Some(keys))
}

// Core: v24.0.0 src/llmq/quorumsman.cpp:91-100.
pub fn load_or_build_contributions<E, Read, Build, Write>(
    read: Read,
    build: Build,
    write: Write,
) -> Result<Option<QuorumKeys>, E>
where
    Read: FnOnce() -> Result<Option<QuorumKeys>, E>,
    Build: FnOnce() -> Option<QuorumKeys>,
    Write: FnOnce(&QuorumKeys) -> Result<(), E>,
{
    match read()? {
        Some(keys) => Ok(Some(keys)),
        None => {
            let Some(keys) = build() else {
                return Ok(None);
            };
            write(&keys)?;
            Ok(Some(keys))
        }
    }
}
