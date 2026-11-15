//! Core's `CDataStream` tuple keys for the recovered-signature database.

use dash_consensus::primitives::{CoreWrite, write_compact_size};
use dash_params::LlmqType;
use dash_types::Numeric;

// Core: v24.0.0 src/serialize.h:1038-1044 Serialize(string)
fn prefix(prefix: &[u8], out: &mut Vec<u8>) {
    write_compact_size(prefix.len() as u64, out);
    out.write(prefix);
}

// Core: v24.0.0 src/uint256.h:79-83 base_blob::Serialize
fn hash(bytes: &[u8; 32], out: &mut Vec<u8>) {
    out.write(bytes);
}

// Core: v24.0.0 src/serialize.h:955-964 Serialize(enum)
fn typ(t: LlmqType, out: &mut Vec<u8>) {
    out.write(&[t.to_base()]);
}

// Core: v24.0.0 src/llmq/signing.cpp:163 WriteRecoveredSig
fn time(time: u32, out: &mut Vec<u8>) {
    out.write(&time.to_be_bytes());
}

// Core: v24.0.0 src/llmq/signing.cpp:160 WriteRecoveredSig
pub(crate) fn recovered(t: LlmqType, id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 1 + 32);
    prefix(b"rs_r", &mut out);
    typ(t, &mut out);
    hash(id, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:161-162 WriteRecoveredSig
pub(crate) fn recovered_message(t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) -> Vec<u8> {
    let mut out = recovered(t, id);
    hash(msg, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:168-169 WriteRecoveredSig
pub(crate) fn recovered_hash(hash_value: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 32);
    prefix(b"rs_h", &mut out);
    hash(hash_value, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:172-174 WriteRecoveredSig
pub(crate) fn recovered_session(sign_hash: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 32);
    prefix(b"rs_s", &mut out);
    hash(sign_hash, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:154 WriteRecoveredSig
pub(crate) fn recovered_platform(hash_value: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 32);
    prefix(b"rs_p", &mut out);
    hash(hash_value, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:155-156 WriteRecoveredSig
pub(crate) fn recovered_platform_message(t: LlmqType, id: &[u8; 32], msg: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 1 + 32 + 32);
    prefix(b"rs_m", &mut out);
    typ(t, &mut out);
    hash(id, &mut out);
    hash(msg, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:157 WriteRecoveredSig
pub(crate) fn recovered_platform_time(time_value: u32, hash_value: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 4 + 32);
    prefix(b"rs_u", &mut out);
    time(time_value, &mut out);
    hash(hash_value, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:163-164 WriteRecoveredSig
pub(crate) fn recovered_time(time_value: u32, t: LlmqType, id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 4 + 1 + 32);
    prefix(b"rs_t", &mut out);
    time(time_value, &mut out);
    typ(t, &mut out);
    hash(id, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:332-336 WriteVoteForId
pub(crate) fn vote(t: LlmqType, id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 4 + 1 + 32);
    prefix(b"rs_v", &mut out);
    typ(t, &mut out);
    hash(id, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:333-337 WriteVoteForId
pub(crate) fn vote_time(time_value: u32, t: LlmqType, id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 5 + 4 + 1 + 32);
    prefix(b"rs_vt", &mut out);
    time(time_value, &mut out);
    typ(t, &mut out);
    hash(id, &mut out);
    out
}

// Core: v24.0.0 src/llmq/signing.cpp:119-125 GetRecoveredSigByHash
pub(crate) fn decode_type_id(bytes: &[u8]) -> Option<(LlmqType, [u8; 32])> {
    Some((LlmqType::from_base(*bytes.first()?), bytes.get(1..33)?.try_into().ok()?))
}

// Core: v24.0.0 src/llmq/signing.cpp:239-245 CleanupOldPlatformSigs
pub(crate) fn decode_platform_time(bytes: &[u8]) -> Option<(u32, [u8; 32])> {
    if !bytes.starts_with(b"\x04rs_u") {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes.get(5..9)?.try_into().ok()?),
        bytes.get(9..41)?.try_into().ok()?,
    ))
}

// Core: v24.0.0 src/llmq/signing.cpp:279-289 CleanupOldRecoveredSigs
pub(crate) fn decode_typed_time(bytes: &[u8], tag: &[u8]) -> Option<(u32, LlmqType, [u8; 32])> {
    if bytes.first().copied()? as usize != tag.len() || bytes.get(1..1 + tag.len())? != tag {
        return None;
    }
    let rest = bytes.get(1 + tag.len()..)?;
    let time = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?);
    let (t, id) = decode_type_id(rest.get(4..)?)?;
    Some((time, t, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    // Core: v24.0.0 src/serialize.h:1038-1044 string; :1174-1217 tuple; src/uint256.h:79-89 base_blob
    #[test]
    fn core_tuple_key_bytes() {
        let t = LlmqType::LlmqTest;
        let id = std::array::from_fn(|i| i as u8);
        let msg = std::array::from_fn(|i| (i + 32) as u8);
        // CompactSize(prefix length), ASCII prefix, type byte, raw uint256 bytes. Times are
        // htobe32 followed by little-endian integer serialization, giving big-endian bytes.
        let cases = [
            (
                recovered(t, &id),
                "0472735f7264000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                recovered_message(t, &id, &msg),
                "0472735f7264000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
            ),
            (
                recovered_time(0x01020304, t, &id),
                "0472735f740102030464000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                recovered_hash(&id),
                "0472735f68000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                recovered_session(&id),
                "0472735f73000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                recovered_platform(&id),
                "0472735f70000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                recovered_platform_message(t, &id, &msg),
                "0472735f6d64000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f",
            ),
            (
                recovered_platform_time(0x01020304, &id),
                "0472735f7501020304000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                vote(t, &id),
                "0472735f7664000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
            (
                vote_time(0x01020304, t, &id),
                "0572735f76740102030464000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            ),
        ];
        for (key, golden) in cases {
            assert_eq!(key, unhex(golden), "{golden}");
        }
    }
}
